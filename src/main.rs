mod ask;
mod checks;
mod dedup;
mod describe;
mod discourse;
mod fixcheck;
mod improve;
mod input;
mod lens;
mod llm;
mod promptctx;
mod quantify;
mod report;
mod requirements;
mod spec;
mod state;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use lens::Finding;
use llm::Llm;
use spec::Spec;
use std::path::PathBuf;

/// #9: Version string bumped manually only when the prompt JSON schema/instructions change
/// structurally. Recorded in state.json, used to tell whether "the prompt itself changed"
/// when comparing against past rounds.
const PROMPT_VERSION: &str = "1";

#[derive(clap::ValueEnum, Clone, Debug, PartialEq)]
enum Backend {
    /// claude -p subprocess
    Claude,
    /// OpenRouter REST API (requires OPENROUTER_API_KEY)
    Openrouter,
}

#[derive(Parser, Debug)]
#[command(
    name = "research",
    version,
    about = "Multi-angle (multi-persona) verification of market/competitor research documents — Code-Review-Loop ported to the research domain"
)]
struct Cli {
    #[arg(long, default_value = "claude", global = true)]
    claude_bin: String,
    #[arg(long, value_enum, default_value = "claude", global = true)]
    backend: Backend,
    #[arg(long, global = true)]
    model: Option<String>,
    /// Low-cost model used for simple judgment steps like lens selection, good things, coverage verification, fix check, etc.
    #[arg(long, global = true)]
    cheap_model: Option<String>,
    #[arg(long, default_value_t = 2, global = true)]
    retries: u32,
    /// #18: per-call timeout (seconds) for both backends (claude CLI subprocess / OpenRouter
    /// HTTP). A hung backend is killed/aborted and reported as an error instead of blocking the
    /// run forever.
    #[arg(long, default_value_t = llm::DEFAULT_LLM_TIMEOUT_SECS, global = true)]
    timeout_secs: u64,
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Independent per-lens review + discourse cross-verification (default pipeline)
    Review {
        #[arg(long)]
        spec: PathBuf,
        /// Research document to verify (markdown)
        #[arg(long)]
        document: PathBuf,
        /// Research brief (list of angles that must be covered)
        #[arg(long)]
        brief: Option<PathBuf>,
        /// Tone/format guide
        #[arg(long)]
        style: Option<PathBuf>,
        #[arg(long)]
        deterministic_results: Option<PathBuf>,
        /// Manually specify lenses (comma-separated). If unspecified, the LLM selects based on document characteristics.
        #[arg(long)]
        lenses: Option<String>,
        #[arg(long, default_value = "runs")]
        out: PathBuf,
        #[arg(long, default_value_t = 1)]
        concurrency: usize,
        /// Maximum number of discourse rounds
        #[arg(long, default_value_t = 2)]
        max_rounds: usize,
        /// Previous round's --out directory (state.json). If given, adds FIXED/STILL_OPEN/REVERSED verdicts for previously confirmed findings.
        #[arg(long)]
        prior: Option<PathBuf>,
        /// Reference year (YYYY) for staleness_flag calculation. If unspecified, approximated by extracting the max 4-digit integer from the document.
        #[arg(long)]
        as_of_year: Option<u32>,
        /// Skip dead_link_check (actual HTTP requests) — for use in network-less environments/CI.
        #[arg(long)]
        skip_link_check: bool,
    },
    /// Summary/key findings/labels/whether the document is separable + scan for markers needing verification
    Describe {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        document: PathBuf,
        #[arg(long)]
        brief: Option<PathBuf>,
        #[arg(long)]
        style: Option<PathBuf>,
        #[arg(long, default_value = "runs")]
        out: PathBuf,
    },
    /// Concrete revision suggestions (incorporating further research/corrections)
    Improve {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        document: PathBuf,
        #[arg(long)]
        brief: Option<PathBuf>,
        #[arg(long)]
        style: Option<PathBuf>,
        #[arg(long, default_value = "runs")]
        out: PathBuf,
    },
    /// Free-form question about the document (accumulated in ask.md)
    Ask {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        document: PathBuf,
        #[arg(long)]
        brief: Option<PathBuf>,
        #[arg(long)]
        style: Option<PathBuf>,
        #[arg(long, default_value = "runs")]
        out: PathBuf,
        question: String,
    },
}

fn main() {
    let cli = Cli::parse();
    let (llm, cheap_llm) = match build_llm(&cli) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
    };
    match dispatch(&cli, &llm, &cheap_llm) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            // #23: a failure partway through a run (e.g. discourse exhausting its retry budget
            // on a real model that produced 3 different malformed response shapes in a row —
            // reproduced for real in this session's execution-verification round) must not
            // silently discard the cost of every LLM call already made earlier in the same run.
            // Each run_*() function already prints llm.usage().summary() as its last line on
            // success; this is the equivalent for every other exit path, so a run that spent
            // real money before failing always reports what it spent, not nothing.
            let usage = llm.usage();
            if usage.calls > 0 {
                eprintln!("{}", usage.summary());
            }
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
    }
}

fn build_llm(cli: &Cli) -> Result<(Llm, Llm)> {
    let usage = Llm::new_usage_tracker();
    let cheap_model = cli.cheap_model.clone().or_else(|| cli.model.clone());
    let timeout = std::time::Duration::from_secs(cli.timeout_secs);
    let (main_llm, cheap_llm) = match cli.backend {
        Backend::Claude => (
            Llm::claude_cli(
                cli.claude_bin.clone(),
                cli.model.clone(),
                cli.retries,
                cli.verbose,
                usage.clone(),
            )
            .with_timeout(timeout),
            Llm::claude_cli(
                cli.claude_bin.clone(),
                cheap_model,
                cli.retries,
                cli.verbose,
                usage.clone(),
            )
            .with_timeout(timeout),
        ),
        Backend::Openrouter => (
            Llm::openrouter(cli.model.clone(), cli.retries, cli.verbose, usage.clone())?
                .with_timeout(timeout),
            Llm::openrouter(cheap_model, cli.retries, cli.verbose, usage.clone())?
                .with_timeout(timeout),
        ),
    };
    Ok((main_llm, cheap_llm))
}

/// PASS=0, REVISE=1 (#12) — only the review subcommand has a verdict-based exit code. The other
/// subcommands always return 0 on normal completion (errors are handled by main()'s Err branch via exit(1), not this function).
fn dispatch(cli: &Cli, llm: &Llm, cheap_llm: &Llm) -> Result<i32> {
    match &cli.cmd {
        Cmd::Review {
            spec,
            document,
            brief,
            style,
            deterministic_results,
            lenses,
            out,
            concurrency,
            max_rounds,
            prior,
            as_of_year,
            skip_link_check,
        } => run_review(
            llm,
            cheap_llm,
            spec,
            document,
            brief,
            style,
            deterministic_results,
            lenses,
            out,
            *concurrency,
            *max_rounds,
            prior,
            *as_of_year,
            *skip_link_check,
        ),
        Cmd::Describe {
            spec,
            document,
            brief,
            style,
            out,
        } => {
            run_describe(llm, spec, document, brief, style, out)?;
            Ok(0)
        }
        Cmd::Improve {
            spec,
            document,
            brief,
            style,
            out,
        } => {
            run_improve(llm, spec, document, brief, style, out)?;
            Ok(0)
        }
        Cmd::Ask {
            spec,
            document,
            brief,
            style,
            out,
            question,
        } => {
            run_ask(llm, spec, document, brief, style, out, question)?;
            Ok(0)
        }
    }
}

fn default_as_of_year(document: &str) -> u32 {
    let re = regex::Regex::new(r"(19|20)\d{2}").expect("failed to compile year regex");
    re.find_iter(document)
        .filter_map(|m| m.as_str().parse::<u32>().ok())
        .max()
        .unwrap_or(2026)
}

#[allow(clippy::too_many_arguments)]
fn run_review(
    llm: &Llm,
    cheap_llm: &Llm,
    spec_path: &PathBuf,
    document_path: &PathBuf,
    brief_path: &Option<PathBuf>,
    style_path: &Option<PathBuf>,
    deterministic_results_path: &Option<PathBuf>,
    lenses_arg: &Option<String>,
    out: &PathBuf,
    concurrency: usize,
    max_rounds: usize,
    prior: &Option<PathBuf>,
    as_of_year: Option<u32>,
    skip_link_check: bool,
) -> Result<i32> {
    let started_at = state::unix_ts();
    let sp = Spec::load(spec_path)?;
    let inp = input::normalize(
        document_path,
        brief_path,
        style_path,
        deterministic_results_path,
    )?;
    const DOC_WARN_CHARS: usize = 300_000;
    if inp.document.len() > DOC_WARN_CHARS {
        eprintln!(
            "Warning: document is {} characters, which is large — it gets resent in full on every per-lens review/discourse/coverage call, driving up token cost",
            inp.document.len()
        );
    }

    let as_of = as_of_year.unwrap_or_else(|| default_as_of_year(&inp.document));

    let out_dir = prepare_out(out)?;

    let prior_state = match prior {
        None => None,
        Some(p) => Some(state::load(p)?),
    };
    let round = prior_state.as_ref().map(|s| s.round + 1).unwrap_or(1);

    println!(
        "Starting research verification (round {}) — {} ({} sections, {} words, {} citations)",
        round,
        sp.name,
        inp.sections.len(),
        inp.word_count,
        inp.citations.len()
    );

    let optional_selected: Vec<String> = match lenses_arg {
        Some(s) => {
            let ids = parse_lenses_arg(s)?;
            for id in &ids {
                anyhow::ensure!(
                    sp.lens_by_id(id).is_some(),
                    "lens id not found in spec: {id}"
                );
            }
            ids
        }
        None => lens::select_lenses(cheap_llm, &sp, &inp)?,
    };
    let mut selected_ids: Vec<String> = optional_selected;
    for l in sp.always_lenses() {
        if l.id != "good_things" && !selected_ids.contains(&l.id) {
            selected_ids.push(l.id.clone());
        }
    }
    println!("Selected lenses: {}", selected_ids.join(", "));

    let lens_outputs: Vec<(String, lens::LensOutput)> =
        par_map(concurrency, selected_ids.clone(), |id| {
            let out = lens::review_lens(llm, &sp, &inp, &id)?;
            println!(
                "  Lens complete: {} — {} findings, {} unverified",
                id,
                out.findings.len(),
                out.unverified.len()
            );
            Ok((id, out))
        })?;

    let mut findings: Vec<Finding> = Vec::new();
    let mut unverified: Vec<(String, String)> = Vec::new();
    for (id, out) in lens_outputs {
        findings.extend(out.findings);
        for u in out.unverified {
            unverified.push((id.clone(), u));
        }
    }

    let good_things = if sp.lens_by_id("good_things").is_some() {
        lens::review_good_things(cheap_llm, &sp, &inp)?.good_things
    } else {
        Vec::new()
    };

    let (audit, mut resolved) = if findings.is_empty() {
        println!("No findings — skipping discourse");
        (Vec::new(), std::collections::HashMap::new())
    } else {
        println!("Starting discourse (max {} rounds)", max_rounds);
        discourse::run(llm, &sp, &mut findings, max_rounds, concurrency)?
    };

    // #10: captured here, before the --prior reinsertion block below mutates `findings`/`resolved`
    // — the semantic dedup pass needs this round's own fresh CONFIRMED findings (from this round's
    // independent lens pass + discourse), untouched by anything reinserted from a previous round.
    let fresh_confirmed: Vec<Finding> = findings
        .iter()
        .filter(|f| resolved.get(&f.id).map(|r| r.status.as_str()) == Some("CONFIRMED"))
        .cloned()
        .collect();

    // #7: --prior recheck results are explicitly branched into 4 outcomes: FIXED (close)/
    // STILL_OPEN (keep)/REVERSED (new high-risk finding)/UNKNOWN (keep + flag for human review).
    // Previously only STILL_OPEN/REVERSED were handled and the rest (especially UNKNOWN) silently
    // disappeared from findings/score — treating "cannot verify" as "resolved" is a safety issue,
    // so UNKNOWN is now always kept and requires human confirmation.
    let mut fix_results: Vec<fixcheck::FixStatus> = Vec::new();
    // #10: STILL_OPEN reinsertions collected here so they can be checked against fresh_confirmed
    // for semantic duplicates right after this block (see the dedup::run call below). Only
    // STILL_OPEN participates — REVERSED is deliberately promoted into a new high-risk finding
    // (a signal worth keeping distinct even if the underlying fact overlaps) and UNKNOWN is
    // already flagged for human review, so neither is the "unaddressed issue counted twice" shape
    // issue #10 reports.
    let mut still_open_reinsertions: Vec<Finding> = Vec::new();
    if let Some(ps) = &prior_state {
        let prior_confirmed: Vec<Finding> = ps
            .findings
            .iter()
            .filter(|f| {
                ps.resolved
                    .get(&f.id)
                    .map(|r| r.status == "CONFIRMED")
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        fix_results = fixcheck::run(cheap_llm, &sp, &inp, &prior_confirmed)?;
        fix_results = reconcile_fix_results(&prior_confirmed, fix_results);
        for fr in &fix_results {
            let Some(orig) = prior_confirmed.iter().find(|f| f.id == fr.finding_id) else {
                continue;
            };
            match fr.status.as_str() {
                "FIXED" => {
                    // Closed — not re-added to findings/resolved. Naturally excluded from the report/score.
                }
                "STILL_OPEN" => {
                    // Fix #2: lens-generated ids are round-less ("<lens_id>-<position>"), so they
                    // repeat identically every round. Reusing orig.id here could collide with an
                    // unrelated finding this round's own lens pass generated at the same
                    // lens/position, silently clobbering that finding's `resolved` entry. Mint a
                    // round-scoped id instead, the same way the REVERSED branch below already does.
                    let mut still_open = orig.clone();
                    still_open.id = format!("{}-still-open-r{}", orig.id, round);
                    findings.push(still_open.clone());
                    resolved.insert(
                        still_open.id.clone(),
                        discourse::Resolution {
                            finding_id: still_open.id.clone(),
                            status: "CONFIRMED".to_string(),
                            merged_into: String::new(),
                            reason: format!("STILL_OPEN vs. previous round: {}", fr.evidence),
                            needs_human_review: false,
                        },
                    );
                    still_open_reinsertions.push(still_open);
                }
                "REVERSED" => {
                    // The prior conclusion itself has been overturned — instead of reusing the
                    // existing finding as-is, promote it to a new high-risk (P0) finding with a
                    // separate id.
                    let mut reversed = orig.clone();
                    reversed.id = format!("{}-reversed-r{}", orig.id, round);
                    reversed.severity = "P0".to_string();
                    reversed.evidence = format!(
                        "[REVERSED] Prior conclusion overturned by newer evidence: {}",
                        fr.evidence
                    );
                    findings.push(reversed.clone());
                    resolved.insert(
                        reversed.id.clone(),
                        discourse::Resolution {
                            finding_id: reversed.id.clone(),
                            status: "CONFIRMED".to_string(),
                            merged_into: String::new(),
                            reason: format!("REVERSED vs. previous round (promoted to a new high-risk finding): {}", fr.evidence),
                            needs_human_review: true,
                        },
                    );
                }
                "UNKNOWN" => {
                    // Cannot verify — kept rather than silently dropped like FIXED, but flagged as
                    // needing human review. Fix #2: mint a round-scoped id (see STILL_OPEN above)
                    // instead of reusing orig.id, so this doesn't collide with a same-position
                    // finding freshly generated by this round's own lens pass.
                    let mut unresolved = orig.clone();
                    unresolved.id = format!("{}-unknown-r{}", orig.id, round);
                    findings.push(unresolved.clone());
                    resolved.insert(
                        unresolved.id.clone(),
                        discourse::Resolution {
                            finding_id: unresolved.id.clone(),
                            status: "CONFIRMED".to_string(),
                            merged_into: String::new(),
                            reason: format!("Could not verify vs. previous round (UNKNOWN) — not auto-resolved, human review required: {}", fr.evidence),
                            needs_human_review: true,
                        },
                    );
                }
                other => {
                    eprintln!(
                        "Warning: fix check returned an unknown status \"{other}\" (finding {})",
                        fr.finding_id
                    );
                }
            }
        }
    }

    // #10: this round's own fresh lens pass can independently rediscover the exact same
    // real-world issue a STILL_OPEN reinsertion above is already flagging as unaddressed — under a
    // different id, and often a different label/citation_ref too, since those are self-reported
    // per LLM call rather than stable across rounds (see dedup.rs doc comment / issue #10 for the
    // reproduction). One semantic-dedup LLM call resolves overlaps the same way discourse.rs
    // already resolves same-round cross-lens duplicates: MERGED. The fresh (this-round) finding is
    // the one downgraded to MERGED — the reinserted finding is kept, since it's the one carrying
    // the cross-round "STILL_OPEN vs previous round" continuity evidence the next round's fixcheck
    // needs. quantify::summarize only scores CONFIRMED findings, so this is what actually stops
    // the same unaddressed issue from being deducted twice.
    for pair in dedup::run(cheap_llm, &still_open_reinsertions, &fresh_confirmed)? {
        if resolved.get(&pair.fresh_id).map(|r| r.status.as_str()) == Some("CONFIRMED") {
            println!(
                "  Dedup: {} merged into {} (same issue as a --prior STILL_OPEN reinsertion — {})",
                pair.fresh_id, pair.reinserted_id, pair.reason
            );
            resolved.insert(
                pair.fresh_id.clone(),
                discourse::Resolution {
                    finding_id: pair.fresh_id.clone(),
                    status: "MERGED".to_string(),
                    merged_into: pair.reinserted_id.clone(),
                    reason: format!(
                        "Semantic duplicate of --prior STILL_OPEN reinsertion {}: {}",
                        pair.reinserted_id, pair.reason
                    ),
                    needs_human_review: false,
                },
            );
        }
    }

    // #4/#3: citation_status is not trusted from the LLM's self-assessment as-is; the code actually
    // re-requests over HTTP and cross-checks the cited quote, overwriting it with
    // UNFETCHED/FETCH_FAILED/QUOTE_MATCHED/QUOTE_NOT_FOUND. The LLM's original value is kept
    // only for reference in finding.llm_citation_status.
    // Fix #3: this call must run after the STILL_OPEN/REVERSED/UNKNOWN reinsertion block above,
    // not before it — otherwise findings re-inserted from --prior keep last round's stale
    // citation_status instead of being (re-)checked against this round's document/sources.
    checks::verify_citations(&inp, &mut findings, skip_link_check);

    // #6: If external results were supplied via --deterministic-results (inp.deterministic_results
    // is already parsed and populated at the input::normalize stage), use them as-is after schema
    // validation, without re-running the internal checks::run_all() and overwriting them — previously
    // external results were completely ignored and only the internal re-run results ever made it
    // into the report/verdict.
    let checks_results: Vec<checks::CheckResult> = match &inp.deterministic_results {
        Some(external) => checks::from_json(external)
            .context("--deterministic-results schema validation failed")?,
        None => checks::run_all(
            &sp,
            &inp,
            &checks::CheckOptions {
                as_of_year: as_of,
                skip_link_check,
            },
        ),
    };
    // Snapshot the checks_results actually used in this round — so a later run can reuse it via
    // `--deterministic-results runs/deterministic-results.json` (in place of an external scanner),
    // or the results can be audited as-is (#6). This is a format from_json can read back directly.
    let det_results_path = out_dir.join("deterministic-results.json");
    std::fs::write(
        &det_results_path,
        serde_json::to_string_pretty(&checks::to_json(&checks_results))?,
    )
    .with_context(|| format!("failed to write {}", det_results_path.display()))?;

    let confirmed_refs: Vec<&Finding> = findings
        .iter()
        .filter(|f| resolved.get(&f.id).map(|r| r.status.as_str()) == Some("CONFIRMED"))
        .collect();
    let angles = requirements::verify(cheap_llm, &sp, &inp, &confirmed_refs)?;
    let coverage_gaps = requirements::coverage_gaps(&angles);

    let quant = quantify::summarize(
        &inp,
        &findings,
        &resolved,
        &checks_results,
        coverage_gaps.len(),
    );

    let path = report::write(report::ReportCtx {
        out_dir: &out_dir,
        spec: &sp,
        input: &inp,
        selected_lenses: &selected_ids,
        round,
        findings: &findings,
        resolved: &resolved,
        unverified: &unverified,
        good_things: &good_things,
        checks: &checks_results,
        angles: &angles,
        coverage_gaps: &coverage_gaps,
        audit: &audit,
        quant: &quant,
        fix_results: &fix_results,
    })?;

    // #9: RunManifest fields (for reproducibility/auditing) — input/spec fingerprint, model/provider, timing, cost, prompt version.
    let provider_label = match &llm.provider {
        crate::llm::Provider::ClaudeCli { .. } => "claude-cli",
        crate::llm::Provider::OpenRouter { .. } => "openrouter",
    };
    let usage = llm.usage();
    state::write(
        &out_dir,
        &state::State {
            round,
            findings: findings.clone(),
            resolved: resolved.clone(),
            input_hash: state::fingerprint_str(&inp.document),
            spec_hash: state::fingerprint_str(&serde_json::to_string(&sp).unwrap_or_default()),
            model_id: llm.model.clone().unwrap_or_default(),
            provider: provider_label.to_string(),
            started_at,
            completed_at: state::unix_ts(),
            cost_usd: usage.cost_usd,
            prompt_version: PROMPT_VERSION.to_string(),
        },
    )?;

    println!(
        "\nDone — verdict={} score={}/100 coverage_gaps={}",
        quant.verdict, quant.score, quant.coverage_gap_count
    );
    println!("Report: {}", path.display());
    println!("Next round: --prior {}", out_dir.display());
    println!("{}", llm.usage().summary());

    // #12: REVISE used to also exit 0, making it unusable as a CI gate — only PASS is 0, everything else (REVISE) is 1.
    Ok(if quant.verdict == "PASS" { 0 } else { 1 })
}

fn run_describe(
    llm: &Llm,
    spec_path: &PathBuf,
    document_path: &PathBuf,
    brief_path: &Option<PathBuf>,
    style_path: &Option<PathBuf>,
    out: &PathBuf,
) -> Result<()> {
    let sp = Spec::load(spec_path)?;
    let inp = input::normalize(document_path, brief_path, style_path, &None)?;
    let out_dir = prepare_out(out)?;
    let d = describe::run(llm, &sp, &inp)?;
    let todos = describe::todo_sections(&inp.document);
    let path = report::write_describe(&out_dir, &d, &todos)?;
    println!("describe complete: {}", path.display());
    println!("{}", llm.usage().summary());
    Ok(())
}

fn run_improve(
    llm: &Llm,
    spec_path: &PathBuf,
    document_path: &PathBuf,
    brief_path: &Option<PathBuf>,
    style_path: &Option<PathBuf>,
    out: &PathBuf,
) -> Result<()> {
    let sp = Spec::load(spec_path)?;
    let inp = input::normalize(document_path, brief_path, style_path, &None)?;
    let out_dir = prepare_out(out)?;
    let suggestions = improve::run(llm, &sp, &inp)?;
    let path = report::write_improve(&out_dir, &suggestions)?;
    println!(
        "improve complete: {} suggestions — {}",
        suggestions.len(),
        path.display()
    );
    println!("{}", llm.usage().summary());
    Ok(())
}

fn run_ask(
    llm: &Llm,
    spec_path: &PathBuf,
    document_path: &PathBuf,
    brief_path: &Option<PathBuf>,
    style_path: &Option<PathBuf>,
    out: &PathBuf,
    question: &str,
) -> Result<()> {
    let sp = Spec::load(spec_path)?;
    let inp = input::normalize(document_path, brief_path, style_path, &None)?;
    let out_dir = prepare_out(out)?;
    let answer = ask::run(llm, &sp, &inp, question)?;
    let path = out_dir.join("ask.md");
    let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
    existing.push_str(&format!("\n## Q: {question}\n\n{answer}\n"));
    std::fs::write(&path, existing)
        .with_context(|| format!("failed to write {}", path.display()))?;
    println!("{}", answer);
    println!("\n(accumulated in: {})", path.display());
    println!("{}", llm.usage().summary());
    Ok(())
}

fn prepare_out(p: &PathBuf) -> Result<PathBuf> {
    std::fs::create_dir_all(p)
        .with_context(|| format!("failed to create output directory: {}", p.display()))?;
    Ok(p.clone())
}

/// Parses the `--lenses` comma-separated override into trimmed, non-empty ids. Unlike the
/// LLM-driven auto-selection path (`lens::select_lenses`), which already requires selecting at
/// least one valid lens, this manual-override path had no equivalent floor: `--lenses ""` (or a
/// value that is only commas/whitespace, e.g. `--lenses " , ,"`) used to parse to an empty id
/// list with no error, silently producing a review with zero participating lenses (no findings,
/// no discourse) that could still report `verdict=PASS score=100/100` — a false-confidence result
/// for a document nothing actually reviewed.
fn parse_lenses_arg(s: &str) -> Result<Vec<String>> {
    let ids: Vec<String> = s
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect();
    anyhow::ensure!(
        !ids.is_empty(),
        "--lenses produced no valid lens ids (got only empty/blank entries)"
    );
    Ok(ids)
}

/// Safety net for `--prior` reconciliation: if the fixcheck LLM response silently drops a
/// finding_id that was in `prior_confirmed` (a known LLM failure mode — the same class of defect
/// the REQ-ID cross-check in requirements.rs guards against for the coverage-verification call),
/// the run_review loop below only iterates over `fix_results`, so a dropped id previously just
/// vanished from findings/report/score with no trace and no human-review flag — worse than
/// UNKNOWN, which is at least explicit. This synthesizes a deterministic UNKNOWN entry (so it
/// re-enters findings and forces needs_human_review, same as an explicit UNKNOWN from the LLM)
/// for every prior_confirmed id absent from fix_results.
fn reconcile_fix_results(
    prior_confirmed: &[Finding],
    mut fix_results: Vec<fixcheck::FixStatus>,
) -> Vec<fixcheck::FixStatus> {
    let seen: std::collections::HashSet<String> =
        fix_results.iter().map(|fr| fr.finding_id.clone()).collect();
    for orig in prior_confirmed {
        if !seen.contains(&orig.id) {
            fix_results.push(fixcheck::FixStatus {
                finding_id: orig.id.clone(),
                status: "UNKNOWN".to_string(),
                evidence: "This finding_id is absent from the fix-check LLM response — \
                    deterministically forced to UNKNOWN by the code (prevents a previously \
                    confirmed finding from silently vanishing when the model drops it from its \
                    output)"
                    .to_string(),
            });
        }
    }
    fix_results
}

/// Runs threads in batches of `concurrency`, sequentially (barrier per chunk).
/// discourse.rs's independent per-lens critic calls (#1) also reuse this helper.
pub(crate) fn par_map<T, R, F>(concurrency: usize, items: Vec<T>, f: F) -> Result<Vec<R>>
where
    T: Send,
    R: Send,
    F: Fn(T) -> Result<R> + Sync,
{
    let c = concurrency.max(1);
    let mut out: Vec<R> = Vec::new();
    let mut rest = items;
    while !rest.is_empty() {
        let take = c.min(rest.len());
        let chunk: Vec<T> = rest.drain(..take).collect();
        let results: Vec<Result<R>> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk.into_iter().map(|item| s.spawn(|| f(item))).collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .map_err(|_| anyhow!("worker thread panicked"))
                        .and_then(|r| r)
                })
                .collect()
        });
        for r in results {
            out.push(r?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str) -> Finding {
        Finding {
            id: id.to_string(),
            section: "sec".to_string(),
            citation_ref: "1".to_string(),
            claim: format!("claim-{id}"),
            evidence: format!("evidence-{id}"),
            impact: String::new(),
            severity: "P0".to_string(),
            label: "x".to_string(),
            confidence: "medium".to_string(),
            recommendation: String::new(),
            lens: "lens_a".to_string(),
            reviewer: String::new(),
            citation_status: "UNVERIFIED".to_string(),
            llm_citation_status: String::new(),
        }
    }

    /// Reproduces a known LLM failure mode (JSON truncation / dropping a list item): the
    /// fixcheck LLM response omits one of the previously-confirmed findings entirely. Before this
    /// reconciliation existed, that finding simply vanished — not FIXED, not STILL_OPEN, not
    /// UNKNOWN, no needs_human_review flag, nothing. It must instead re-appear, forced to UNKNOWN.
    #[test]
    fn reconcile_fix_results_forces_unknown_for_ids_missing_from_llm_response() {
        let prior_confirmed = vec![finding("f1"), finding("f2")];
        let fix_results = vec![fixcheck::FixStatus {
            finding_id: "f1".to_string(),
            status: "STILL_OPEN".to_string(),
            evidence: "still present".to_string(),
        }];
        // Sanity check: f2 really is absent from the raw LLM output before reconciliation.
        assert!(!fix_results.iter().any(|fr| fr.finding_id == "f2"));

        let reconciled = reconcile_fix_results(&prior_confirmed, fix_results);
        assert_eq!(reconciled.len(), 2, "f2 must not silently vanish");
        let f2 = reconciled
            .iter()
            .find(|fr| fr.finding_id == "f2")
            .expect("f2 must be present after reconciliation");
        assert_eq!(f2.status, "UNKNOWN");
    }

    #[test]
    fn parse_lenses_arg_rejects_empty_string() {
        assert!(parse_lenses_arg("").is_err());
    }

    #[test]
    fn parse_lenses_arg_rejects_blank_only_entries() {
        assert!(parse_lenses_arg(" , , ").is_err());
    }

    #[test]
    fn parse_lenses_arg_parses_and_trims_valid_ids() {
        let ids =
            parse_lenses_arg("market_dynamics, financial_forensics ,incentive_integrity").unwrap();
        assert_eq!(
            ids,
            vec![
                "market_dynamics",
                "financial_forensics",
                "incentive_integrity"
            ]
        );
    }

    #[test]
    fn reconcile_fix_results_is_a_no_op_when_llm_covers_every_id() {
        let prior_confirmed = vec![finding("f1")];
        let fix_results = vec![fixcheck::FixStatus {
            finding_id: "f1".to_string(),
            status: "FIXED".to_string(),
            evidence: "addressed".to_string(),
        }];
        let reconciled = reconcile_fix_results(&prior_confirmed, fix_results);
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].status, "FIXED");
    }
}
