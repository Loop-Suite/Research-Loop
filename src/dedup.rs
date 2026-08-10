use crate::lens::Finding;
use crate::llm::Llm;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Issue #10: a `--prior` STILL_OPEN reinsertion (fixcheck.rs judged the document hasn't actually
/// fixed a previously-confirmed issue) and this round's own fresh lens pass can independently
/// rediscover the exact same real-world issue under two different finding ids. Round-relative ids,
/// and the `label`/`citation_ref` fields the LLM self-reports per call, are not stable identifiers
/// of "the same issue" across independent calls — confirmed by direct reproduction in issue #10
/// (the same cash-reward-review flaw came back as `label=market_dynamics` / `citation_ref="1"` on
/// one call and `label=incentive_integrity` / `citation_ref="[1]"` on the other) — so no
/// deterministic key reliably dedupes them. This module asks the LLM directly, in a single call
/// over a pre-filtered candidate-pair list, and the caller (main.rs) marks a confirmed duplicate
/// MERGED — reusing discourse.rs's existing MERGED resolution status/score-exclusion, the same
/// mechanism that already dedupes same-round cross-lens duplicates.
pub const DEDUP_SYSTEM: &str = "You determine whether two findings describe the same real-world \
issue in a document, even when their wording, severity, or label/citation format differ. One \
finding was reinserted from a previous round because the document still hasn't addressed it; the \
other was freshly reported this round by an independent review pass. Judge same_issue=true only \
when both point at the same underlying flaw at the same location in the document — not merely a \
similar topic or the same section in general. Respond strictly in the specified JSON schema only.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DedupVerdict {
    pub reinserted_id: String,
    pub fresh_id: String,
    #[serde(default)]
    pub same_issue: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DedupOutput {
    #[serde(default)]
    pairs: Vec<DedupVerdict>,
}

/// Loose, deterministic pre-filter for "plausibly the same part of the document" — not the dedup
/// judgment itself (that's the LLM's job), just a way to keep the candidate-pair list small enough
/// for a single call.
fn sections_similar(a: &str, b: &str) -> bool {
    let a = a.trim().to_ascii_lowercase();
    let b = b.trim().to_ascii_lowercase();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.contains(&b) || b.contains(&a)
}

/// Candidate pairs worth asking the LLM about: same lens, or a plausibly-similar section. This is
/// a union (OR), not an intersection — issue #10's own reproduction shows the fresh rediscovery
/// can land on a *different* lens than the one that originally raised it (the LLM's independent
/// per-round lens selection/attribution isn't stable), so requiring both would silently miss the
/// exact duplicates this module exists to catch.
fn candidate_pairs<'a>(
    reinserted: &'a [Finding],
    fresh_confirmed: &'a [Finding],
) -> Vec<(&'a Finding, &'a Finding)> {
    let mut pairs = Vec::new();
    for r in reinserted {
        for f in fresh_confirmed {
            if r.lens == f.lens || sections_similar(&r.section, &f.section) {
                pairs.push((r, f));
            }
        }
    }
    pairs
}

/// Detects `--prior` STILL_OPEN reinsertions that are semantic duplicates of a finding this
/// round's own lens pass freshly confirmed, via one LLM call over a pre-filtered candidate list
/// (issue #10). Returns only the pairs judged `same_issue`; the caller marks the fresh finding's
/// resolution MERGED into the reinserted one (kept, since it carries the "STILL_OPEN vs previous
/// round" continuity evidence a future round's fixcheck also needs). Returns empty without calling
/// the LLM if either list is empty, or if no candidate pair passes the pre-filter.
pub fn run(
    llm: &Llm,
    reinserted: &[Finding],
    fresh_confirmed: &[Finding],
) -> Result<Vec<DedupVerdict>> {
    if reinserted.is_empty() || fresh_confirmed.is_empty() {
        return Ok(Vec::new());
    }
    let pairs = candidate_pairs(reinserted, fresh_confirmed);
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let catalog = pairs
        .iter()
        .map(|(r, f)| {
            format!(
                "- reinserted_id={} | section={} | severity={}\n  claim: {}\n  evidence: {}\n  fresh_id={} | section={} | severity={}\n  claim: {}\n  evidence: {}",
                r.id, r.section, r.severity, r.claim, r.evidence,
                f.id, f.section, f.severity, f.claim, f.evidence,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let task = format!(
        "# Task\nFor each candidate pair below, judge whether the reinserted finding (carried over from a \
         previous round as still unaddressed) and the fresh finding (newly reported this round by an \
         independent review pass) describe the same real-world issue in the document.\n\n\
         ## Candidate pairs\n{catalog}\n\n\
         ## Output (JSON only, no code fence)\n\
         {{\"pairs\":[{{\"reinserted_id\":\"...\",\"fresh_id\":\"...\",\"same_issue\":true|false,\"reason\":\"...\"}}]}}\n",
        catalog = catalog
    );
    let out: DedupOutput = llm
        .json_typed(&task, Some(DEDUP_SYSTEM))
        .context("semantic dedup check failed")?;
    Ok(out.pairs.into_iter().filter(|p| p.same_issue).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str, lens: &str, section: &str) -> Finding {
        Finding {
            id: id.to_string(),
            section: section.to_string(),
            citation_ref: "1".to_string(),
            claim: format!("claim-{id}"),
            evidence: format!("evidence-{id}"),
            impact: String::new(),
            severity: "P1".to_string(),
            label: "x".to_string(),
            confidence: "medium".to_string(),
            recommendation: String::new(),
            lens: lens.to_string(),
            reviewer: String::new(),
            citation_status: "UNVERIFIED".to_string(),
            llm_citation_status: String::new(),
        }
    }

    #[test]
    fn run_skips_llm_call_when_reinserted_is_empty() {
        // No fake claude binary configured — if this actually shelled out, the test would fail
        // with a spawn error, not silently pass. Reaching Ok(vec![]) proves the short-circuit.
        let llm = Llm::claude_cli(
            "/nonexistent/definitely-not-a-real-binary".to_string(),
            None,
            0,
            false,
            Llm::new_usage_tracker(),
        );
        let fresh = vec![finding("f1", "market_dynamics", "Market Dynamics")];
        let result = run(&llm, &[], &fresh).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn run_skips_llm_call_when_fresh_confirmed_is_empty() {
        let llm = Llm::claude_cli(
            "/nonexistent/definitely-not-a-real-binary".to_string(),
            None,
            0,
            false,
            Llm::new_usage_tracker(),
        );
        let reinserted = vec![finding(
            "market_dynamics-3-still-open-r2",
            "market_dynamics",
            "Market Dynamics",
        )];
        let result = run(&llm, &reinserted, &[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn candidate_pairs_crosses_lens_boundary_via_similar_section() {
        // Reproduces issue #10's actual cross-lens case: the reinserted finding is still tagged
        // with the lens that raised it last round (market_dynamics), while this round's
        // independent rediscovery landed on a different lens (financial_forensics) — only the
        // section ties them together.
        let reinserted = vec![finding(
            "market_dynamics-4-still-open-r2",
            "market_dynamics",
            "App Store Ratings",
        )];
        let fresh = vec![finding(
            "financial_forensics-2",
            "financial_forensics",
            "App Store Ratings",
        )];
        let pairs = candidate_pairs(&reinserted, &fresh);
        assert_eq!(
            pairs.len(),
            1,
            "different lens + matching section must still form a candidate pair"
        );
    }

    #[test]
    fn candidate_pairs_excludes_unrelated_lens_and_section() {
        let reinserted = vec![finding("a-1-still-open-r2", "lens_a", "Section A")];
        let fresh = vec![finding("b-1", "lens_b", "Section B")];
        assert!(candidate_pairs(&reinserted, &fresh).is_empty());
    }

    #[test]
    fn sections_similar_handles_substring_and_case() {
        assert!(sections_similar("Market Dynamics", "market dynamics"));
        assert!(sections_similar(
            "Market Dynamics — Competitive Landscape",
            "Market Dynamics"
        ));
        assert!(!sections_similar("Market Dynamics", "Financial Forensics"));
        assert!(!sections_similar("", "Market Dynamics"));
    }

    /// End-to-end reproduction of issue #10's real 2-round scenario, through the actual subprocess
    /// boundary (a fake `claude` binary), not a hand-mocked Llm — verifies the full
    /// spawn/stdin/stdout/JSON-parse round trip a real run would go through. The reinserted and
    /// fresh findings use the exact mismatched label/citation_ref/claim wording from the issue's
    /// reproduction (same underlying fact, described differently by two independent LLM calls);
    /// the fake claude script returns the JSON verdict a correctly-prompted real model would give.
    #[test]
    fn run_dedupes_issue_10_reproduction_case_via_fake_claude_subprocess() {
        let mut reinserted = finding(
            "market_dynamics-3-still-open-r2",
            "market_dynamics",
            "Market Dynamics",
        );
        reinserted.citation_ref = "[1]".to_string();
        reinserted.claim =
            "Cash-reward review program (5,000 won per review) inflates app-store rating"
                .to_string();

        let mut fresh = finding(
            "market_dynamics-3",
            "incentive_integrity",
            "Market Dynamics",
        );
        fresh.citation_ref = "1".to_string();
        fresh.claim = "TestPay still runs its cash-reward review event paying 5000 won per review"
            .to_string();

        let expected = DedupOutput {
            pairs: vec![DedupVerdict {
                reinserted_id: reinserted.id.clone(),
                fresh_id: fresh.id.clone(),
                same_issue: true,
                reason: "both describe the same unresolved cash-reward review program".to_string(),
            }],
        };
        let (script_path, json_path) = write_fake_claude(&expected, "issue10");
        let llm = Llm::claude_cli(
            script_path.to_string_lossy().to_string(),
            None,
            0,
            false,
            Llm::new_usage_tracker(),
        );

        let result = run(
            &llm,
            std::slice::from_ref(&reinserted),
            std::slice::from_ref(&fresh),
        );

        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&json_path);

        let pairs = result.expect("dedup run against fake claude must succeed");
        assert_eq!(
            pairs.len(),
            1,
            "the reproduction pair must be judged same_issue"
        );
        assert_eq!(pairs[0].reinserted_id, reinserted.id);
        assert_eq!(pairs[0].fresh_id, fresh.id);
    }

    #[test]
    fn run_drops_pairs_the_llm_judges_different_issues() {
        let reinserted = finding("a-1-still-open-r2", "lens_a", "Section A");
        let fresh = finding("a-2", "lens_a", "Section A");

        let expected = DedupOutput {
            pairs: vec![DedupVerdict {
                reinserted_id: reinserted.id.clone(),
                fresh_id: fresh.id.clone(),
                same_issue: false,
                reason: "unrelated claims".to_string(),
            }],
        };
        let (script_path, json_path) = write_fake_claude(&expected, "different");
        let llm = Llm::claude_cli(
            script_path.to_string_lossy().to_string(),
            None,
            0,
            false,
            Llm::new_usage_tracker(),
        );

        let result = run(
            &llm,
            std::slice::from_ref(&reinserted),
            std::slice::from_ref(&fresh),
        );

        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&json_path);

        let pairs = result.expect("dedup run against fake claude must succeed");
        assert!(
            pairs.is_empty(),
            "same_issue=false pairs must be filtered out of the result"
        );
    }

    /// Writes a fake `claude` CLI replacement (`-p --output-format json` compatible: drains stdin,
    /// prints `{"result": "<json>"}`) that always returns `expected` as the dedup call's result,
    /// regardless of the prompt it's given — sufficient to exercise `run`'s subprocess/JSON-parse
    /// path deterministically. The JSON payload is written to a companion file rather than
    /// inlined into the shell script, to avoid any shell-quoting of embedded quotes/newlines.
    fn write_fake_claude(
        expected: &DedupOutput,
        tag: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let pid = std::process::id();
        let mut json_path = std::env::temp_dir();
        json_path.push(format!("research_loop_dedup_fake_result_{tag}_{pid}.json"));
        let outer = serde_json::json!({ "result": serde_json::to_string(expected).unwrap() });
        std::fs::write(&json_path, serde_json::to_string(&outer).unwrap()).unwrap();

        let mut script_path = std::env::temp_dir();
        script_path.push(format!("research_loop_dedup_fake_claude_{tag}_{pid}.sh"));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\ncat >/dev/null\ncat \"{}\"\n",
                json_path.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (script_path, json_path)
    }
}
