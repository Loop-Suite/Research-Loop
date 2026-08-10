use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub const OPENROUTER_DEFAULT_MODEL: &str = "openai/gpt-oss-120b";

/// #18: default per-call timeout for both backends. Generous enough for a slow real call over a
/// large document/context, but finite — without this, a wedged `claude` subprocess or a stalled
/// OpenRouter connection blocks the entire run forever with no way to recover. Overridable via
/// `--timeout-secs` / [`Llm::with_timeout`].
pub const DEFAULT_LLM_TIMEOUT_SECS: u64 = 300;

/// LLM call backend. ClaudeCli = `claude -p` subprocess, OpenRouter = REST API.
#[derive(Clone, Debug)]
pub enum Provider {
    ClaudeCli { bin: String },
    OpenRouter { api_key: String },
}

/// Cumulative token/cost usage. If multiple Llm instances (e.g. a main model +
/// a low-cost model) share the same Arc, they get a combined total across the whole run.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// Only populated when the claude CLI provides it (absent from OpenRouter responses).
    pub cost_usd: f64,
}

impl Usage {
    pub fn summary(&self) -> String {
        let cost = if self.cost_usd > 0.0 {
            format!(", cost ${:.4}", self.cost_usd)
        } else {
            String::new()
        };
        format!(
            "{} LLM calls — input {} / output {} / cache_read {} / cache_write {}{}",
            self.calls,
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_creation_tokens,
            cost
        )
    }
}

#[derive(Debug, Default)]
struct CallUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    cost_usd: f64,
}

#[derive(Debug)]
struct CallResult {
    text: String,
    usage: CallUsage,
}

#[derive(Clone, Debug)]
pub struct Llm {
    pub provider: Provider,
    pub model: Option<String>,
    pub retries: u32,
    pub verbose: bool,
    /// #18: per-call timeout applied to both backends. Defaults to [`DEFAULT_LLM_TIMEOUT_SECS`];
    /// override with [`Llm::with_timeout`].
    pub timeout: Duration,
    usage: Arc<Mutex<Usage>>,
}

impl Llm {
    /// Share this across multiple Llm instances to track combined usage for the whole run.
    pub fn new_usage_tracker() -> Arc<Mutex<Usage>> {
        Arc::new(Mutex::new(Usage::default()))
    }

    pub fn claude_cli(
        bin: String,
        model: Option<String>,
        retries: u32,
        verbose: bool,
        usage: Arc<Mutex<Usage>>,
    ) -> Self {
        Llm {
            provider: Provider::ClaudeCli { bin },
            model,
            retries,
            verbose,
            timeout: Duration::from_secs(DEFAULT_LLM_TIMEOUT_SECS),
            usage,
        }
    }

    /// Requires the `OPENROUTER_API_KEY` env var. Falls back to the 120B open model if no model is given.
    pub fn openrouter(
        model: Option<String>,
        retries: u32,
        verbose: bool,
        usage: Arc<Mutex<Usage>>,
    ) -> Result<Self> {
        let api_key = std::env::var("OPENROUTER_API_KEY").context(
            "OPENROUTER_API_KEY environment variable not set (export OPENROUTER_API_KEY=...)",
        )?;
        Ok(Llm {
            provider: Provider::OpenRouter { api_key },
            model: Some(model.unwrap_or_else(|| OPENROUTER_DEFAULT_MODEL.to_string())),
            retries,
            verbose,
            timeout: Duration::from_secs(DEFAULT_LLM_TIMEOUT_SECS),
            usage,
        })
    }

    /// #18: overrides the default per-call timeout (e.g. from `--timeout-secs`).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Snapshot of usage accumulated so far (from the shared tracker). If another thread
    /// panicked while holding the lock (poisoning it), the totals may be off, but this won't panic too.
    pub fn usage(&self) -> Usage {
        self.usage.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn record_usage(&self, u: &CallUsage) {
        let mut g = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        g.calls += 1;
        g.input_tokens += u.input_tokens;
        g.output_tokens += u.output_tokens;
        g.cache_read_tokens += u.cache_read_tokens;
        g.cache_creation_tokens += u.cache_creation_tokens;
        g.cost_usd += u.cost_usd;
    }

    fn call_once(&self, ctx: Option<&str>, task: &str, system: Option<&str>) -> Result<CallResult> {
        match &self.provider {
            Provider::ClaudeCli { bin } => {
                call_claude(bin, self.model.as_deref(), ctx, task, system, self.timeout)
            }
            Provider::OpenRouter { api_key } => call_openrouter(
                api_key,
                self.model.as_deref(),
                ctx,
                task,
                system,
                self.timeout,
            ),
        }
    }

    /// Takes `ctx` (a stable prefix repeated across calls: project context, conventions,
    /// requirements, diff) separately from `task` (the per-call instruction that varies).
    /// On the OpenRouter backend, ctx gets a cache_control(ephemeral) tag so repeated calls
    /// with the same ctx can hit the cache. The claude-cli backend spawns a fresh subprocess
    /// per call, so caching has no effect there — it just concatenates the two.
    pub fn text_ctx(&self, ctx: Option<&str>, task: &str, system: Option<&str>) -> Result<String> {
        let mut last: Option<anyhow::Error> = None;
        for attempt in 0..=self.retries {
            match self.call_once(ctx, task, system) {
                Ok(r) => {
                    self.record_usage(&r.usage);
                    if !r.text.trim().is_empty() {
                        return Ok(r.text);
                    }
                    last = Some(anyhow!("empty response"));
                }
                Err(e) => last = Some(e),
            }
            if self.verbose {
                match last.as_ref() {
                    Some(error) => eprintln!("[retry {}/{}] {error}", attempt + 1, self.retries),
                    None => eprintln!(
                        "[retry {}/{}] unknown retry error",
                        attempt + 1,
                        self.retries
                    ),
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("unknown failure")))
    }

    /// JSON-forcing variant of [`Llm::text_ctx`]. Retries on parse failure.
    pub fn json_ctx(
        &self,
        ctx: Option<&str>,
        task: &str,
        system: Option<&str>,
    ) -> Result<serde_json::Value> {
        let mut last: Option<anyhow::Error> = None;
        for attempt in 0..=self.retries {
            let raw = match self.call_once(ctx, task, system) {
                Ok(r) => {
                    self.record_usage(&r.usage);
                    r.text
                }
                Err(e) => {
                    last = Some(e);
                    if self.verbose {
                        match last.as_ref() {
                            Some(error) => {
                                eprintln!("[json retry {}/{}] {error}", attempt + 1, self.retries)
                            }
                            None => {
                                eprintln!(
                                    "[json retry {}/{}] unknown json retry error",
                                    attempt + 1,
                                    self.retries
                                );
                            }
                        }
                    }
                    continue;
                }
            };
            match extract_json(&raw) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    last = Some(e);
                    if self.verbose {
                        match last.as_ref() {
                            Some(error) => {
                                eprintln!("[json retry {}/{}] {error}", attempt + 1, self.retries)
                            }
                            None => {
                                eprintln!(
                                    "[json retry {}/{}] unknown json retry error",
                                    attempt + 1,
                                    self.retries
                                );
                            }
                        }
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("JSON response failed")))
    }

    /// Forces a JSON response and deserializes it into `T`. Unlike calling [`Llm::json`] and
    /// then `serde_json::from_value` separately, a response that parses as JSON but doesn't
    /// match `T`'s schema (e.g. a missing required field) is retried up to `self.retries` times
    /// just like a raw parse failure, instead of hard-failing the whole run on the first
    /// response that deviates from the expected shape.
    pub fn json_typed<T: serde::de::DeserializeOwned>(
        &self,
        prompt: &str,
        system: Option<&str>,
    ) -> Result<T> {
        self.json_ctx_typed(None, prompt, system)
    }

    /// Typed, schema-retrying variant of [`Llm::json_ctx`]. See [`Llm::json_typed`].
    pub fn json_ctx_typed<T: serde::de::DeserializeOwned>(
        &self,
        ctx: Option<&str>,
        task: &str,
        system: Option<&str>,
    ) -> Result<T> {
        let mut last: Option<anyhow::Error> = None;
        for attempt in 0..=self.retries {
            let raw = match self.call_once(ctx, task, system) {
                Ok(r) => {
                    self.record_usage(&r.usage);
                    r.text
                }
                Err(e) => {
                    last = Some(e);
                    if self.verbose {
                        eprintln!(
                            "[json retry {}/{}] {}",
                            attempt + 1,
                            self.retries,
                            last.as_ref().unwrap()
                        );
                    }
                    continue;
                }
            };
            let parsed = extract_json(&raw).and_then(|v| {
                serde_json::from_value::<T>(v).map_err(|e| anyhow!("JSON schema mismatch: {e}"))
            });
            match parsed {
                Ok(v) => return Ok(v),
                Err(e) => {
                    last = Some(e);
                    if self.verbose {
                        eprintln!(
                            "[json retry {}/{}] {}",
                            attempt + 1,
                            self.retries,
                            last.as_ref().unwrap()
                        );
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("JSON response failed")))
    }
}

/// The prompt is passed via stdin (to avoid argument length limits). Since this is a
/// subprocess call, caching doesn't apply, so ctx+task are simply concatenated
/// (order only: stable context first, variable instruction last).
///
/// #18: no step here blocks without a bound. Writing stdin, reading stdout/stderr, and waiting
/// for exit each run on their own thread (or a polling loop), so a `claude` process that hangs
/// (stuck auth prompt, stalled network, wedged internally, etc.) is killed and reported as a
/// timeout error instead of blocking the whole run forever.
fn call_claude(
    bin: &str,
    model: Option<&str>,
    ctx: Option<&str>,
    task: &str,
    system: Option<&str>,
    timeout: Duration,
) -> Result<CallResult> {
    let mut cmd = Command::new(bin);
    cmd.arg("-p").arg("--output-format").arg("json");
    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }
    if let Some(s) = system {
        cmd.arg("--append-system-prompt").arg(s);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // #29: put the child in its own process group so a timeout kill can take out any subprocess
    // it spawns too, not just the direct child. Without this, `child.kill()` below only kills
    // `bin` itself — if `bin` (a shell wrapper, or `claude` internally) forks a helper process,
    // that grandchild inherits the stdout/stderr pipe write-ends and keeps them open forever,
    // so the reader threads' `read_to_end()` never sees EOF even after the "timeout" fires.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to run `{bin}` (check it's installed and on PATH)"))?;

    // Write stdin from a dedicated thread rather than inline: if the child writes enough to
    // stdout/stderr before it has finished reading stdin, the OS pipe buffer for those (typically
    // 16-64KB) fills up and the child blocks on its own write — while this thread would still be
    // blocked writing ctx+task (shared_context embeds the *entire* research document, easily
    // hundreds of KB) to stdin, with nothing yet reading stdout/stderr to unblock it. That's a
    // classic std::process deadlock (#4). Spawning the write onto its own thread lets the
    // stdout/stderr reader threads below drain concurrently with the write, so neither side can
    // starve the other.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("failed to open stdin"))?;
    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to open stdout"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to open stderr"))?;
    let ctx_owned = ctx.map(|c| c.to_string());
    let task_owned = task.to_string();
    // #29: these threads report back over a channel (not a bare JoinHandle) so the poll loop
    // below can wait on them with a bounded timeout even after killing the child. A signal-based
    // kill isn't 100% guaranteed to make a pipe's write-end close promptly on every platform/CI
    // runner (observed hanging on GitHub Actions' Linux runner even with process-group kill), so
    // this is defense in depth: if the OS-level kill doesn't unblock the reader fast enough, we
    // still bound how long we wait on it instead of hanging the whole call forever.
    let (writer_tx, writer_rx) = mpsc::channel::<std::io::Result<()>>();
    let (stdout_tx, stdout_rx) = mpsc::channel::<std::io::Result<Vec<u8>>>();
    let (stderr_tx, stderr_rx) = mpsc::channel::<std::io::Result<Vec<u8>>>();
    std::thread::spawn(move || {
        let result = (|| {
            if let Some(c) = &ctx_owned {
                stdin.write_all(c.as_bytes())?;
            }
            stdin.write_all(task_owned.as_bytes())
            // stdin dropped here, closing it and sending EOF to the child.
        })();
        let _ = writer_tx.send(result);
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let result = stdout_pipe.read_to_end(&mut buf).map(|_| buf);
        let _ = stdout_tx.send(result);
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let result = stderr_pipe.read_to_end(&mut buf).map(|_| buf);
        let _ = stderr_tx.send(result);
    });

    // How long to wait for the reader/writer threads to report back once we know the child is
    // no longer running (either it exited on its own, or we just killed it). Generous enough for
    // a cooperative pipe close under normal load, but still finite.
    const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

    // #18: poll for exit with a deadline instead of a blocking wait — `wait_with_output()` (the
    // previous approach) has no way to bound how long it blocks. If the deadline passes first,
    // kill the child; that closes its end of the stdout/stderr pipes, so the reader threads above
    // unblock with whatever partial output exists (which we discard) instead of hanging too.
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            eprintln!("[llm debug] deadline hit, entering kill branch");
            // #29: kill the whole process group (the child is its own group leader, see spawn
            // above), not just the direct child — a grandchild process would otherwise survive
            // and keep the stdout/stderr pipes open, hanging the reader threads below forever.
            #[cfg(unix)]
            {
                let pgid = child.id();
                let kill_status = Command::new("kill")
                    .arg("-KILL")
                    .arg(format!("-{pgid}"))
                    .status();
                eprintln!("[llm debug] pgid kill status: {kill_status:?}");
            }
            let ck = child.kill();
            eprintln!("[llm debug] child.kill() -> {ck:?}");
            let cw = child.wait();
            eprintln!("[llm debug] child.wait() -> {cw:?}");
            let wr = writer_rx.recv_timeout(DRAIN_TIMEOUT);
            eprintln!("[llm debug] writer_rx drained: {}", wr.is_ok());
            let outr = stdout_rx.recv_timeout(DRAIN_TIMEOUT);
            eprintln!("[llm debug] stdout_rx drained: {}", outr.is_ok());
            let errr = stderr_rx.recv_timeout(DRAIN_TIMEOUT);
            eprintln!("[llm debug] stderr_rx drained: {}", errr.is_ok());
            return Err(anyhow!(
                "claude did not respond within {}s and was killed (check that `{bin}` is \
                 installed, authenticated, and reachable — override with --timeout-secs if a \
                 longer-running call is expected)",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    // The child has already exited at this point (the poll loop above only breaks once
    // `try_wait` confirms that), so its pipe write-ends are closed and these should return
    // promptly — but bound the wait anyway rather than trusting that unconditionally (see #29).
    let stdout_buf = stdout_rx
        .recv_timeout(DRAIN_TIMEOUT)
        .map_err(|_| anyhow!("stdout reader thread did not report back in time"))?
        .context("failed to read claude's stdout")?;
    let stderr_buf = stderr_rx
        .recv_timeout(DRAIN_TIMEOUT)
        .map_err(|_| anyhow!("stderr reader thread did not report back in time"))?
        .context("failed to read claude's stderr")?;
    writer_rx
        .recv_timeout(DRAIN_TIMEOUT)
        .map_err(|_| anyhow!("stdin writer thread did not report back in time"))?
        .context("failed to write prompt to claude's stdin")?;
    if !status.success() {
        return Err(anyhow!(
            "claude exited with code {:?}: {}",
            status.code(),
            String::from_utf8_lossy(&stderr_buf).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).with_context(|| {
        format!(
            "failed to parse claude JSON output: {}",
            truncate(&stdout, 400)
        )
    })?;
    if v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false) {
        return Err(anyhow!(
            "claude returned an error response: {}",
            truncate(&stdout, 400)
        ));
    }
    let result = v
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| anyhow!("response missing result field: {}", truncate(&stdout, 400)))?;

    // The usage/cost fields' presence and naming can vary by claude CLI version, so parse them
    // leniently (default to 0 rather than failing — only the result field is treated as a contract).
    let usage_obj = v.get("usage");
    let get_u64 = |key: &str| {
        usage_obj
            .and_then(|u| u.get(key))
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
    };
    let cost_usd = v
        .get("total_cost_usd")
        .or_else(|| v.get("cost_usd"))
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    Ok(CallResult {
        text: result.to_string(),
        usage: CallUsage {
            input_tokens: get_u64("input_tokens"),
            output_tokens: get_u64("output_tokens"),
            cache_read_tokens: get_u64("cache_read_input_tokens"),
            cache_creation_tokens: get_u64("cache_creation_input_tokens"),
            cost_usd,
        },
    })
}

/// cache_control(ephemeral) is an Anthropic Messages API extension, so it only makes sense
/// for Claude-family models — for other models (including OPENROUTER_DEFAULT_MODEL) there's
/// no caching benefit, so no reason to add it. If the model name doesn't contain "claude",
/// we send the plain single-string content as before.
fn supports_prompt_caching(model: &str) -> bool {
    model.to_ascii_lowercase().contains("claude")
}

/// One call to the OpenRouter chat completions API. If ctx is given and the target model is
/// Claude-family, it's split into a separate content block with cache_control(ephemeral) —
/// an optimization to hit the cache on repeated calls with the same ctx (e.g. per-lens review).
/// Otherwise a plain single-string content is sent as before.
fn call_openrouter(
    api_key: &str,
    model: Option<&str>,
    ctx: Option<&str>,
    task: &str,
    system: Option<&str>,
    timeout: Duration,
) -> Result<CallResult> {
    let mut messages: Vec<serde_json::Value> = Vec::new();
    if let Some(s) = system {
        messages.push(serde_json::json!({"role": "system", "content": s}));
    }
    let resolved_model = model.unwrap_or(OPENROUTER_DEFAULT_MODEL);
    let cacheable_ctx = ctx.filter(|c| !c.is_empty() && supports_prompt_caching(resolved_model));
    let user_content = match cacheable_ctx {
        Some(c) => serde_json::json!([
            {"type": "text", "text": c, "cache_control": {"type": "ephemeral"}},
            {"type": "text", "text": task},
        ]),
        None => {
            let combined = match ctx {
                Some(c) if !c.is_empty() => format!("{c}{task}"),
                _ => task.to_string(),
            };
            serde_json::json!(combined)
        }
    };
    messages.push(serde_json::json!({"role": "user", "content": user_content}));

    let body = serde_json::json!({
        "model": resolved_model,
        "messages": messages,
    });

    // ureq 3.x drops the old Error::Status(code, resp) variant (which carried the response
    // body) in favor of Error::StatusCode(u16) alone. To keep the same error message shape
    // (status + response body), we disable automatic http-status-as-error and check the
    // status ourselves, reading the body before it's dropped.
    // #18: timeout_global bounds a stalled connection/response the same way checks.rs's
    // safe_fetch already does for citation/dead-link HTTP calls — previously unset here, so a
    // stalled OpenRouter request could hang the run forever with no way to recover.
    let mut resp = ureq::post(OPENROUTER_URL)
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| anyhow!("openrouter call failed: {e}"))?;

    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        let body_text = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(anyhow!(
            "openrouter responded with status {code}: {}",
            truncate(&body_text, 400)
        ));
    }

    let v: serde_json::Value = resp
        .body_mut()
        .read_json()
        .context("failed to parse openrouter response JSON")?;
    let content = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| {
            anyhow!(
                "openrouter response missing content: {}",
                truncate(&v.to_string(), 400)
            )
        })?;

    // OpenAI-compatible usage schema (prompt_tokens/completion_tokens). Cost isn't in the response, so leave it at 0.
    let usage_obj = v.get("usage");
    let get_u64 = |key: &str| {
        usage_obj
            .and_then(|u| u.get(key))
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
    };
    Ok(CallResult {
        text: content.to_string(),
        usage: CallUsage {
            input_tokens: get_u64("prompt_tokens"),
            output_tokens: get_u64("completion_tokens"),
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: 0.0,
        },
    })
}

/// Extracts just the JSON object (or array) from a response that has code fences/chatter mixed in.
pub fn extract_json(raw: &str) -> Result<serde_json::Value> {
    let t = raw.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
        return Ok(v);
    }
    if let Some(start) = t.find("```") {
        let after = &t[start + 3..];
        let after = after.strip_prefix("json").unwrap_or(after);
        if let Some(end) = after.find("```") {
            let body = after[..end].trim();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                return Ok(v);
            }
        }
    }
    if let (Some(s), Some(e)) = (t.find('{'), t.rfind('}')) {
        if s < e {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t[s..=e]) {
                return Ok(v);
            }
        }
    }
    if let (Some(s), Some(e)) = (t.find('['), t.rfind(']')) {
        if s < e {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t[s..=e]) {
                return Ok(v);
            }
        }
    }
    Err(anyhow!("failed to extract JSON: {}", truncate(t, 400)))
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Regression test for a classic std::process pipe deadlock: `call_claude` used to write
    /// ctx+task to the child's stdin synchronously, in the same thread, before anything started
    /// reading the child's stdout/stderr. If the child writes enough to stdout/stderr before it
    /// has finished reading stdin (progress output, warnings, etc.), the OS pipe buffer for that
    /// (typically 16-64KB) fills up and the child blocks on its own write — while we're still
    /// blocked writing a potentially much larger ctx (shared_context embeds the *entire* research
    /// document, easily hundreds of KB) to its stdin. Neither side can make progress. This is
    /// simulated with a small child script that writes 2MB to stdout before draining stdin.
    #[test]
    fn call_claude_does_not_deadlock_on_large_ctx_with_eager_child_output() {
        let mut script_path = std::env::temp_dir();
        script_path.push(format!(
            "research_loop_fake_claude_{}.sh",
            std::process::id()
        ));
        std::fs::write(
            &script_path,
            "#!/bin/sh\nhead -c 2000000 /dev/zero\ncat >/dev/null\nprintf '{\"result\":\"ok\"}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let big_ctx = "X".repeat(2_000_000); // far exceeds any OS pipe buffer
        let bin = script_path.to_string_lossy().to_string();

        let (tx, rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _ = call_claude(
                &bin,
                None,
                Some(&big_ctx),
                "task",
                None,
                Duration::from_secs(10),
            );
            let _ = tx.send(());
        });

        let finished = rx.recv_timeout(Duration::from_secs(15)).is_ok();
        let _ = std::fs::remove_file(&script_path);
        assert!(
            finished,
            "call_claude did not return within 15s — likely deadlocked writing a large ctx to \
             stdin while the child wrote to stdout before draining stdin"
        );
    }

    /// Regression test for issue #18: previously `call_claude` had no timeout at all —
    /// `wait_with_output()` blocked forever on a child that never exits (stuck auth prompt,
    /// stalled network, wedged process, etc.). Simulated with a fake `claude` that drains stdin
    /// (so this isn't the #4 deadlock) and then sleeps far longer than the configured timeout.
    /// Asserts the call returns an error well within a generous wall-clock bound instead of
    /// hanging for the child's full sleep duration.
    #[test]
    fn call_claude_returns_timeout_error_instead_of_hanging_forever() {
        let mut script_path = std::env::temp_dir();
        script_path.push(format!(
            "research_loop_fake_claude_hang_{}.sh",
            std::process::id()
        ));
        std::fs::write(
            &script_path,
            "#!/bin/sh\ncat >/dev/null\nsleep 300\nprintf '{\"result\":\"too late\"}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let bin = script_path.to_string_lossy().to_string();
        let (tx, rx) = mpsc::channel::<std::result::Result<CallResult, String>>();
        std::thread::spawn(move || {
            let result = call_claude(
                &bin,
                None,
                Some("ctx"),
                "task",
                None,
                Duration::from_millis(300),
            )
            .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });

        // The child sleeps for 300s; a wall-clock bound far below that proves the timeout (not
        // the child eventually exiting) is what ended the call.
        let outcome = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("call_claude did not return within 10s — the 300ms timeout did not fire");
        let _ = std::fs::remove_file(&script_path);

        let err = outcome.expect_err("a child that never exits must produce a timeout error");
        assert!(
            err.contains("did not respond within"),
            "unexpected error message: {err}"
        );
    }

    /// Writes a fake `claude` replacement script that drains stdin then runs `body`, and returns
    /// its path (caller is responsible for removing it).
    fn write_fake_claude_script(tag: &str, body: &str) -> std::path::PathBuf {
        let mut script_path = std::env::temp_dir();
        script_path.push(format!(
            "research_loop_fake_claude_{tag}_{}.sh",
            std::process::id()
        ));
        std::fs::write(&script_path, format!("#!/bin/sh\ncat >/dev/null\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        script_path
    }

    /// Subprocess failure simulation: `claude` exits non-zero (e.g. not authenticated, invalid
    /// flag, crashed). Must surface a clean error including the stderr message, not panic and
    /// not silently treat it as success.
    #[test]
    fn call_claude_returns_clean_error_on_nonzero_exit() {
        let script_path = write_fake_claude_script(
            "nonzero_exit",
            "echo 'not authenticated: run `claude login`' >&2\nexit 1",
        );
        let bin = script_path.to_string_lossy().to_string();

        let result = call_claude(
            &bin,
            None,
            Some("ctx"),
            "task",
            None,
            Duration::from_secs(5),
        );

        let _ = std::fs::remove_file(&script_path);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("exited with code"),
            "unexpected error message: {err}"
        );
        assert!(
            err.contains("not authenticated"),
            "stderr should be surfaced in the error: {err}"
        );
    }

    /// Subprocess failure simulation: `claude` exits 0 but its stdout isn't the expected
    /// `{"result": "..."}` JSON envelope at all (a corrupted/unexpected CLI output shape, not
    /// just a code-fence-wrapped result — extract_json's fence-stripping doesn't apply here,
    /// since this is the outer claude-CLI JSON parse, not the inner LLM response parse). Must
    /// return a clean error, not panic.
    #[test]
    fn call_claude_returns_clean_error_on_non_json_stdout() {
        let script_path = write_fake_claude_script("non_json", "printf 'not json at all'");
        let bin = script_path.to_string_lossy().to_string();

        let result = call_claude(
            &bin,
            None,
            Some("ctx"),
            "task",
            None,
            Duration::from_secs(5),
        );

        let _ = std::fs::remove_file(&script_path);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("failed to parse claude JSON output"),
            "unexpected error message: {err}"
        );
    }
}
