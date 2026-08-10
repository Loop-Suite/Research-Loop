mod ask;
mod checks;
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
    match real_main() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
    }
}

fn build_llm(cli: &Cli) -> Result<(Llm, Llm)> {
    let usage = Llm::new_usage_tracker();
    let cheap_model = cli.cheap_model.clone().or_else(|| cli.model.clone());
    let (main_llm, cheap_llm) = match cli.backend {
        Backend::Claude => (
            Llm::claude_cli(
                cli.claude_bin.clone(),
                cli.model.clone(),
                cli.retries,
                cli.verbose,
                usage.clone(),
            ),
            Llm::claude_cli(
                cli.claude_bin.clone(),
                cheap_model,
                cli.retries,
                cli.verbose,
                usage.clone(),
            ),
        ),
        Backend::Openrouter => (
            Llm::openrouter(cli.model.clone(), cli.retries, cli.verbose, usage.clone())?,
            Llm::openrouter(cheap_model, cli.retries, cli.verbose, usage.clone())?,
        ),
    };
    Ok((main_llm, cheap_llm))
}

/// PASS=0, REVISE=1 (#12) — only the review subcommand has a verdict-based exit code. The other
/// subcommands always return 0 on normal completion (errors are handled by main()'s Err branch via exit(1), not this function).
fn real_main() -> Result<i32> {
    let cli = Cli::parse();
    let (llm, cheap_llm) = build_llm(&cli)?;

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
            &llm,
            &cheap_llm,
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
            run_describe(&llm, spec, document, brief, style, out)?;
            Ok(0)
        }
        Cmd::Improve {
            spec,
            document,
            brief,
            style,
            out,
        } => {
            run_improve(&llm, spec, document, brief, style, out)?;
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
            run_ask(&llm, spec, document, brief, style, out, question)?;
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
            let ids: Vec<String> = s
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect();
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

    // #7: --prior recheck results are explicitly branched into 4 outcomes: FIXED (close)/
    // STILL_OPEN (keep)/REVERSED (new high-risk finding)/UNKNOWN (keep + flag for human review).
    // Previously only STILL_OPEN/REVERSED were handled and the rest (especially UNKNOWN) silently
    // disappeared from findings/score — treating "cannot verify" as "resolved" is a safety issue,
    // so UNKNOWN is now always kept and requires human confirmation.
    let mut fix_results: Vec<fixcheck::FixStatus> = Vec::new();
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
