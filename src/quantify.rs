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
