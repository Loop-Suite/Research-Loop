use crate::input::Input;
use crate::llm::Llm;
use crate::promptctx::shared_context;
use crate::spec::Spec;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DESCRIBE_SYSTEM: &str =
    "You are an analyst who summarizes research documents. You do not fabricate \
content that is not in the document. Always respond using only the specified JSON schema.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Describe {
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub key_findings: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub can_be_split: String, // yes|no|unknown
    #[serde(default)]
    pub can_be_split_note: String,
}

pub fn run(llm: &Llm, spec: &Spec, input: &Input) -> Result<Describe> {
    let ctx = shared_context(spec, input);
    let task = "# Task\nWrite a summary of the research document below.\n\n\
         ## Output (JSON only, no code fence)\n\
         {\"title\":\"one line, 50 characters or fewer\",\"summary\":\"2-4 sentences\",\
         \"key_findings\":[\"key finding per section/area, 1 line per item\"],\
         \"labels\":[\"the research-type/area this document covers\"],\
         \"can_be_split\":\"yes|no|unknown\",\"can_be_split_note\":\"rationale (e.g. can it be split by angle given the number of sections)\"}\n";
    llm.json_ctx_typed(Some(&ctx), task, Some(DESCRIBE_SYSTEM))
        .context("describe failed")
}

/// Scans the document for "needs verification"-type markers. Deterministic (no LLM used).
pub fn todo_sections(document: &str) -> Vec<String> {
    let markers = ["[확인필요]", "추후 업데이트", "TODO", "TBD", "재검증 필요"];
    document
        .lines()
        .filter(|l| markers.iter().any(|m| l.contains(m)))
        .map(|l| l.trim().to_string())
        .collect()
}
