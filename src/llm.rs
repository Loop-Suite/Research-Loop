use anyhow::{anyhow, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub const OPENROUTER_DEFAULT_MODEL: &str = "openai/gpt-oss-120b";

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
            usage,
        })
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
                call_claude(bin, self.model.as_deref(), ctx, task, system)
            }
            Provider::OpenRouter { api_key } => {
                call_openrouter(api_key, self.model.as_deref(), ctx, task, system)
            }
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

    /// Forces a JSON response. Retries on parse failure.
    pub fn json(&self, prompt: &str, system: Option<&str>) -> Result<serde_json::Value> {
        self.json_ctx(None, prompt, system)
    }

    /// JSON-forcing variant of [`Llm::text_ctx`].
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
}

/// The prompt is passed via stdin (to avoid argument length limits). Since this is a
/// subprocess call, caching doesn't apply, so ctx+task are simply concatenated
/// (order only: stable context first, variable instruction last).
fn call_claude(
    bin: &str,
    model: Option<&str>,
    ctx: Option<&str>,
    task: &str,
    system: Option<&str>,
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

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to run `{bin}` (check it's installed and on PATH)"))?;

    // Write stdin from a dedicated thread rather than inline: if the child writes enough to
    // stdout/stderr before it has finished reading stdin, the OS pipe buffer for those (typically
    // 16-64KB) fills up and the child blocks on its own write — while this thread would still be
    // blocked writing ctx+task (shared_context embeds the *entire* research document, easily
    // hundreds of KB) to stdin, with nothing yet reading stdout/stderr to unblock it. That's a
    // classic std::process deadlock. Spawning the write onto its own thread lets
    // wait_with_output() below start draining stdout/stderr concurrently with the write, so
    // neither side can starve the other.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("failed to open stdin"))?;
    let ctx_owned = ctx.map(|c| c.to_string());
    let task_owned = task.to_string();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        if let Some(c) = &ctx_owned {
            stdin.write_all(c.as_bytes())?;
        }
        stdin.write_all(task_owned.as_bytes())
        // stdin dropped here at the end of the closure, closing it and sending EOF to the child.
    });

    let out = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| anyhow!("stdin writer thread panicked"))?
        .context("failed to write prompt to claude's stdin")?;
    if !out.status.success() {
        return Err(anyhow!(
            "claude exited with code {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
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

    let result = ureq::post(OPENROUTER_URL)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .send_json(body);

    let resp = match result {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            return Err(anyhow!(
                "openrouter responded with status {code}: {}",
                truncate(&body, 400)
            ));
        }
        Err(e) => return Err(anyhow!("openrouter call failed: {e}")),
    };

    let v: serde_json::Value = resp
        .into_json()
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
    use std::time::Duration;

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
            let _ = call_claude(&bin, None, Some(&big_ctx), "task", None);
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
}
