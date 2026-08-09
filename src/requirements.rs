use crate::input::Input;
use crate::lens::Finding;
use crate::llm::Llm;
use crate::promptctx::shared_context;
use crate::spec::Spec;
use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const REQ_SYSTEM: &str = "You determine whether the research brief (the list of angles that must be covered) is satisfied, by checking it against the document. \
Do not mark an item as MET without evidence. Respond with exactly one entry per given REQ-ID, \
never inventing REQ-IDs that were not given or skipping any given item. Respond only with the specified JSON schema.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AngleCheck {
    #[serde(default)]
    pub req_id: String,
    pub angle: String,
    pub status: String, // MET|MISSING|AMBIGUOUS|N/A
    pub evidence: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AngleCheckOutput {
    #[serde(default)]
    angles: Vec<AngleCheck>,
}

/// #8: Handing the whole brief to the LLM and trusting the "angles" array as-is meant the code
/// had no way of noticing if the model dropped an item entirely. Now, *before* the brief goes
/// into the prompt, the code first deterministically builds a REQ-001, REQ-002... list, then
/// after the LLM responds it checks that ID set exactly and forces any missing entries to MISSING.
///
/// Parsing rule: blank lines are skipped; the remainder after stripping any leading numbering
/// (`1.` `1)` `(1)`) or bullet marker (`-` `*` `•`) from each line is treated as the item text.
/// Plain lines with no marker are also treated as a single item each (this is not a full
/// Markdown list AST parser, but the deterministic line-by-line breakdown resolves the
/// "LLM judges the whole thing at once" problem).
fn parse_requirements(brief: &str) -> Vec<(String, String)> {
    let marker_re = Regex::new(r"^\s*(?:[-*•]|\(?\d+[.)])\s+")
        .expect("failed to compile requirement marker regex");
    let mut items: Vec<String> = Vec::new();
    for line in brief.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let text = marker_re.replace(trimmed, "").trim().to_string();
        if text.is_empty() {
            continue;
        }
        items.push(text);
    }
    items
        .into_iter()
        .enumerate()
        .map(|(i, text)| (format!("REQ-{:03}", i + 1), text))
        .collect()
}

/// Returns None when requirements (the brief) are not provided (nothing to verify — no N/A listing).
pub fn verify(
    llm: &Llm,
    spec: &Spec,
    input: &Input,
    confirmed: &[&Finding],
) -> Result<Option<Vec<AngleCheck>>> {
    let brief = match &input.requirements {
        None => return Ok(None),
        Some(b) => b,
    };
    let reqs = parse_requirements(brief);
    if reqs.is_empty() {
        return Ok(Some(Vec::new()));
    }

    let req_list = reqs
        .iter()
        .map(|(id, text)| format!("- {id}: {text}"))
        .collect::<Vec<_>>()
        .join("\n");
    let findings_summary = confirmed
        .iter()
        .map(|f| format!("- [{}] {} — {}", f.severity, f.section, f.claim))
        .collect::<Vec<_>>()
        .join("\n");
    let ctx = shared_context(spec, input);
    let task = format!(
        "# Task\nCheck each of the deterministically numbered requirements (REQ-ID) below against the document. \
         You must return exactly one entry for every given REQ-ID (no omissions, no additions).\n\n\
         ## Requirements list (REQ-ID: original brief text)\n{req_list}\n\n\
         ## Confirmed findings (for reference — may support evidence of unmet angles)\n{fs}\n\n\
         ## Output (JSON only, no code fences)\n\
         {{\"angles\":[{{\"req_id\":\"REQ-001\",\"angle\":\"the original text from the requirements list, verbatim\",\
         \"status\":\"MET|MISSING|AMBIGUOUS|N/A\",\"evidence\":\"section evidence, or reason for missing/ambiguous\"}}]}}\n",
        req_list = req_list,
        fs = if findings_summary.is_empty() { "(none)".to_string() } else { findings_summary },
    );
    let v = llm
        .json_ctx(Some(&ctx), &task, Some(REQ_SYSTEM))
        .context("angle coverage verification failed")?;
    let out: AngleCheckOutput =
        serde_json::from_value(v).context("angle coverage JSON schema mismatch")?;

    // Deterministic cross-check: only trust req_ids the LLM returned that are in the list we gave it;
    // any REQ-ID missing from the response is forced to MISSING by the code — this prevents the model
    // from silently dropping an item (#8).
    let mut by_id: HashMap<String, AngleCheck> = out
        .angles
        .into_iter()
        .filter(|a| !a.req_id.trim().is_empty())
        .map(|a| (a.req_id.clone(), a))
        .collect();
    let mut result = Vec::with_capacity(reqs.len());
    for (id, text) in &reqs {
        match by_id.remove(id) {
            Some(mut a) => {
                a.req_id = id.clone();
                if a.angle.trim().is_empty() {
                    a.angle = text.clone();
                }
                result.push(a);
            }
            None => result.push(AngleCheck {
                req_id: id.clone(),
                angle: text.clone(),
                status: "MISSING".to_string(),
                evidence: "This REQ-ID is absent from the LLM output — deterministically marked MISSING by the code (prevents the model from silently dropping a requirement)".to_string(),
            }),
        }
    }
    Ok(Some(result))
}

/// Extracts only MISSING/AMBIGUOUS angles — used as-is in report.rs's coverage_gaps section.
pub fn coverage_gaps(angles: &Option<Vec<AngleCheck>>) -> Vec<String> {
    match angles {
        None => Vec::new(),
        Some(list) => list
            .iter()
            .filter(|a| a.status == "MISSING" || a.status == "AMBIGUOUS")
            .map(|a| format!("{} {} ({})", a.req_id, a.angle, a.status))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numbered_list() {
        let brief = "1. Pricing policy\n2) Competitor comparison\n(3) Market size\n";
        let reqs = parse_requirements(brief);
        assert_eq!(reqs.len(), 3);
        assert_eq!(
            reqs[0],
            ("REQ-001".to_string(), "Pricing policy".to_string())
        );
        assert_eq!(
            reqs[1],
            ("REQ-002".to_string(), "Competitor comparison".to_string())
        );
        assert_eq!(reqs[2], ("REQ-003".to_string(), "Market size".to_string()));
    }

    #[test]
    fn parses_bullets_and_plain_lines() {
        let brief = "- Pricing policy\n* Competitor comparison\n• Market size\nplain line\n\n\nskips blank lines";
        let reqs = parse_requirements(brief);
        assert_eq!(reqs.len(), 5);
        assert_eq!(reqs[3].1, "plain line");
    }

    #[test]
    fn empty_brief_yields_no_requirements() {
        assert!(parse_requirements("   \n\n  ").is_empty());
    }
}
