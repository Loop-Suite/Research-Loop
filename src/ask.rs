use crate::input::Input;
use crate::llm::Llm;
use crate::promptctx::shared_context;
use crate::spec::Spec;
use anyhow::{Context, Result};

pub const ASK_SYSTEM: &str = "You are an analyst who answers questions about a research document. \
You answer only based on the document, brief, and tone guide. If there is no basis for an answer, say you don't know.";

pub fn run(llm: &Llm, spec: &Spec, input: &Input, question: &str) -> Result<String> {
    let ctx = shared_context(spec, input);
    let task = format!("# Question\n{question}\n");
    llm.text_ctx(Some(&ctx), &task, Some(ASK_SYSTEM)).context("ask failed")
}
