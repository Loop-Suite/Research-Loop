use crate::input::Input;
use crate::llm::Llm;
use crate::promptctx::shared_context;
use crate::spec::Spec;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const IMPROVE_SYSTEM: &str = "You are an analyst who proposes concrete revisions to a research document. \
You only propose changes to content that is actually stated in the document. You do not fabricate new facts \
without evidence — a revision suggestion is an instruction to 'add this / re-investigate this,' not a way to \
arbitrarily fill in unverified figures. Always respond using only the specified JSON schema.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub relevant_section: String,
    pub existing_text: String,
    pub suggestion_content: String,
    pub revised_text: String,
    pub one_sentence_summary: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ImproveOutput {
    #[serde(default)]
    suggestions: Vec<Suggestion>,
}

pub fn run(llm: &Llm, spec: &Spec, input: &Input) -> Result<Vec<Suggestion>> {
    let ctx = shared_context(spec, input);
    let task = format!(
        "# Task\nPropose concrete revisions (incorporating further research / corrections) for this research document.\n\n\
         ## Rules\n\
         - existing_text/revised_text must quote/edit sentences that actually exist in the document.\n\
         - Do not fabricate unverified new figures — use an instruction like 're-investigate this part' instead.\n\
         - one_sentence_summary must be 6 words or fewer.\n\
         - label must be exactly one of: {labels}\n\n\
         ## Output (JSON only, no code fence)\n\
         {{\"suggestions\":[{{\"relevant_section\":\"...\",\"existing_text\":\"...\",\
         \"suggestion_content\":\"...\",\"revised_text\":\"...\",\"one_sentence_summary\":\"...\",\
         \"label\":<one of the allowed values>}}]}}\n",
        labels = spec.labels_prompt(),
    );
    let out: ImproveOutput = llm
        .json_ctx_typed(Some(&ctx), &task, Some(IMPROVE_SYSTEM))
        .context("improve failed")?;
    Ok(out.suggestions)
}
