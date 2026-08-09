use crate::input::Input;
use crate::lens::Finding;
use crate::llm::Llm;
use crate::promptctx::shared_context;
use crate::spec::Spec;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Adds REVERSED to codereview-loop's 3 states FIXED/STILL_OPEN/UNKNOWN
/// (docs/design-spec.md §0 — a research-domain extension to distinguish cases like the
/// T-order–KT example, where "the prior conclusion was overturned by newer evidence," from
/// STILL_OPEN. A 4th value not present in the original.)
pub const FIXCHECK_SYSTEM: &str = "You determine whether a finding confirmed in a previous round has actually \
been addressed in this document. Do not judge something FIXED without evidence. If the prior conclusion itself \
has been overturned by newer evidence — not merely updated — judge it REVERSED rather than FIXED (e.g. a case \
previously stated as a confirmed fact where the latest evidence points the opposite way). If it cannot be \
verified, judge UNKNOWN. Always respond using only the specified JSON schema.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixStatus {
    pub finding_id: String,
    pub status: String, // FIXED|STILL_OPEN|UNKNOWN|REVERSED
    pub evidence: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FixCheckOutput {
    #[serde(default)]
    results: Vec<FixStatus>,
}

/// If prior_confirmed is empty, returns an empty result (either round 1, or no findings were confirmed previously).
pub fn run(llm: &Llm, spec: &Spec, input: &Input, prior_confirmed: &[Finding]) -> Result<Vec<FixStatus>> {
    if prior_confirmed.is_empty() {
        return Ok(Vec::new());
    }
    let list = prior_confirmed
        .iter()
        .map(|f| format!("- id={} | {} | {}\n  Evidence: {}", f.id, f.section, f.claim, f.evidence))
        .collect::<Vec<_>>()
        .join("\n");
    let ctx = shared_context(spec, input);
    let task = format!(
        "# Task\nDetermine whether the findings confirmed in the previous round below have been addressed or reversed in this document.\n\n\
         ## Findings confirmed in previous round\n{list}\n\n\
         ## Output (JSON only, no code fence)\n\
         {{\"results\":[{{\"finding_id\":\"...\",\"status\":\"FIXED|STILL_OPEN|UNKNOWN|REVERSED\",\"evidence\":\"...\"}}]}}\n",
        list = list
    );
    let v = llm.json_ctx(Some(&ctx), &task, Some(FIXCHECK_SYSTEM)).context("fix check failed")?;
    let out: FixCheckOutput = serde_json::from_value(v).context("fix check JSON schema mismatch")?;
    Ok(out.results)
}
