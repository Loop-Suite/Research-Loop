use crate::checks::{CheckResult, CheckStatus};
use crate::discourse::Resolution;
use crate::input::Input;
use crate::lens::Finding;
use std::collections::HashMap;

pub struct QuantSummary {
    pub verdict: String, // PASS|REVISE — docs/design-spec.md §6 (simplified from codereview's 4-state verdict)
    pub score: i64,      // 0-100
    pub score_deductions: Vec<String>,
    pub coverage_gap_count: usize,
}

fn severity_penalty(severity: &str) -> i64 {
    match severity {
        "P0" => 25,
        "P1" => 12,
        "P2" => 5,
        "P3" => 1,
        _ => 0,
    }
}

/// Deducts from 100 points using only CONFIRMED findings.
/// Assumption: deduction amounts stay identical to codereview-loop's numbers (do not extend, docs/design-spec.md §6).
fn score(findings: &[Finding], resolved: &HashMap<String, Resolution>) -> (i64, Vec<String>) {
    let mut total = 100i64;
    let mut deductions = Vec::new();
    for f in findings {
        if resolved.get(&f.id).map(|r| r.status.as_str()) == Some("CONFIRMED") {
            let p = severity_penalty(&f.severity);
            total -= p;
            deductions.push(format!(
                "[{}] {} -{} pts — {}",
                f.severity, f.section, p, f.claim
            ));
        }
    }
    (total.max(0), deductions)
}

/// Two states: PASS/REVISE.
///
/// #3: A FAIL from the deterministic checks (checks.rs) is "hard evidence" — no matter how much
/// self-reported confidence (discourse.rs confidence_weight) piles up AGREE votes that push some
/// finding to REJECTED, this function always returns REVISE if checks itself is FAIL, regardless
/// of that finding — it's an independent condition that never references findings/resolved state,
/// so there's no way to route around it with confidence weighting (pinned by
/// quantify_tests::hard_evidence_check_fail_forces_revise_regardless_of_findings).
///
/// #7: If even one resolution has needs_human_review set (a finding that came back UNKNOWN/REVERSED
/// from a --prior re-check), it forces REVISE regardless of that finding's severity — "unable to
/// verify" is never auto-passed.
fn verdict(
    findings: &[Finding],
    resolved: &HashMap<String, Resolution>,
    checks: &[CheckResult],
    coverage_gap_count: usize,
) -> String {
    // Priority 1: deterministic check FAIL — always takes precedence, regardless of findings/confidence.
    if checks.iter().any(|c| c.status == CheckStatus::Fail) {
        return "REVISE".to_string();
    }
    // Priority 2: a resolution explicitly flagged as needing human review (#7 UNKNOWN/REVERSED).
    if resolved.values().any(|r| r.needs_human_review) {
        return "REVISE".to_string();
    }

    let confirmed: Vec<&Finding> = findings
        .iter()
        .filter(|f| resolved.get(&f.id).map(|r| r.status.as_str()) == Some("CONFIRMED"))
        .collect();

    if confirmed
        .iter()
        .any(|f| f.severity == "P0" || f.severity == "P1")
    {
        return "REVISE".to_string();
    }
    if coverage_gap_count > 0 {
        return "REVISE".to_string();
    }
    "PASS".to_string()
}

pub fn summarize(
    _input: &Input,
    findings: &[Finding],
    resolved: &HashMap<String, Resolution>,
    checks: &[CheckResult],
    coverage_gap_count: usize,
) -> QuantSummary {
    let (sc, deductions) = score(findings, resolved);
    let v = verdict(findings, resolved, checks, coverage_gap_count);
    QuantSummary {
        verdict: v,
        score: sc,
        score_deductions: deductions,
        coverage_gap_count,
    }
}

#[cfg(test)]
mod quantify_tests {
    use super::*;

    fn finding(id: &str, severity: &str) -> Finding {
        Finding {
            id: id.to_string(),
            section: "sec".to_string(),
            citation_ref: "1".to_string(),
            claim: format!("claim-{id}"),
            evidence: format!("evidence-{id}"),
            impact: String::new(),
            severity: severity.to_string(),
            label: "x".to_string(),
            confidence: "medium".to_string(),
            recommendation: String::new(),
            lens: "market_dynamics".to_string(),
            reviewer: String::new(),
            citation_status: "UNVERIFIED".to_string(),
            llm_citation_status: String::new(),
        }
    }

    /// Reproduces issue #10's core symptom and its fix: a --prior STILL_OPEN reinsertion and this
    /// round's own fresh rediscovery of the same real-world issue must be deducted once, not
    /// twice. dedup.rs judges the fresh finding a semantic duplicate and main.rs downgrades its
    /// resolution to MERGED (reusing the same status/score-exclusion discourse.rs already applies
    /// to same-round cross-lens duplicates) — score() only sums CONFIRMED findings, so a MERGED
    /// finding contributes nothing.
    #[test]
    fn merged_resolution_excludes_semantic_duplicate_from_score() {
        let findings = vec![
            finding("market_dynamics-3-still-open-r2", "P1"), // reinserted, kept CONFIRMED
            finding("market_dynamics-3", "P1"), // fresh rediscovery, downgraded to MERGED by dedup
        ];
        let mut resolved: HashMap<String, Resolution> = HashMap::new();
        resolved.insert(
            "market_dynamics-3-still-open-r2".to_string(),
            Resolution {
                finding_id: "market_dynamics-3-still-open-r2".to_string(),
                status: "CONFIRMED".to_string(),
                merged_into: String::new(),
                reason: "STILL_OPEN vs. previous round".to_string(),
                needs_human_review: false,
            },
        );
        resolved.insert(
            "market_dynamics-3".to_string(),
            Resolution {
                finding_id: "market_dynamics-3".to_string(),
                status: "MERGED".to_string(),
                merged_into: "market_dynamics-3-still-open-r2".to_string(),
                reason: "Semantic duplicate of --prior STILL_OPEN reinsertion".to_string(),
                needs_human_review: false,
            },
        );
        let (total, deductions) = score(&findings, &resolved);
        assert_eq!(
            total, 88,
            "one P1 (-12) deduction, not two (-24) — the MERGED duplicate must not be scored"
        );
        assert_eq!(deductions.len(), 1);
    }

    /// Negative control for the test above: proves it actually exercises the fix, by showing what
    /// the pre-#10-fix behavior (both copies left CONFIRMED) produces — a double deduction for one
    /// real issue, exactly the bug issue #10 reports.
    #[test]
    fn without_dedup_merge_the_same_issue_is_double_counted() {
        let findings = vec![
            finding("market_dynamics-3-still-open-r2", "P1"),
            finding("market_dynamics-3", "P1"),
        ];
        let mut resolved: HashMap<String, Resolution> = HashMap::new();
        for f in &findings {
            resolved.insert(
                f.id.clone(),
                Resolution {
                    finding_id: f.id.clone(),
                    status: "CONFIRMED".to_string(),
                    merged_into: String::new(),
                    reason: String::new(),
                    needs_human_review: false,
                },
            );
        }
        let (total, deductions) = score(&findings, &resolved);
        assert_eq!(
            total, 76,
            "both left CONFIRMED means -24 total for one real issue — the pre-fix bug shape"
        );
        assert_eq!(deductions.len(), 2);
    }

    #[test]
    fn hard_evidence_check_fail_forces_revise_regardless_of_findings() {
        let findings: Vec<Finding> = Vec::new();
        let resolved: HashMap<String, Resolution> = HashMap::new();
        let checks = vec![CheckResult {
            id: "dead_link".into(),
            title: "Citation URL response check".into(),
            status: CheckStatus::Fail,
            evidence: "test".into(),
        }];
        let v = verdict(&findings, &resolved, &checks, 0);
        assert_eq!(
            v, "REVISE",
            "A deterministic check FAIL must always force REVISE even with zero confirmed findings (i.e. unaffected by confidence weighting) (#3)"
        );
    }

    #[test]
    fn needs_human_review_forces_revise_even_for_low_severity() {
        let findings: Vec<Finding> = Vec::new();
        let mut resolved: HashMap<String, Resolution> = HashMap::new();
        resolved.insert(
            "f1".to_string(),
            Resolution {
                finding_id: "f1".to_string(),
                status: "CONFIRMED".to_string(),
                merged_into: String::new(),
                reason: "unknown".to_string(),
                needs_human_review: true,
            },
        );
        let checks: Vec<CheckResult> = Vec::new();
        let v = verdict(&findings, &resolved, &checks, 0);
        assert_eq!(v, "REVISE", "REVISE must always be forced when any resolution has the needs_human_review flag set (#7)");
    }

    #[test]
    fn clean_run_is_pass() {
        let findings: Vec<Finding> = Vec::new();
        let resolved: HashMap<String, Resolution> = HashMap::new();
        let checks: Vec<CheckResult> = Vec::new();
        assert_eq!(verdict(&findings, &resolved, &checks, 0), "PASS");
    }
}
