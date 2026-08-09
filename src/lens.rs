use crate::input::Input;
use crate::llm::Llm;
use crate::promptctx::shared_context;
use crate::spec::{Lens, Spec};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const LENS_SYSTEM: &str = "You are an analyst verifying a market/competitor research document. \
Unsubstantiated suspicions are not findings — file them as unverified instead. \
Only flag claims actually stated in the document; do not speculate about content that isn't there. \
Respond strictly in the specified JSON schema.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    #[serde(default)]
    pub id: String,
    /// The document section where the evidence is located (the `## ` heading text).
    pub section: String,
    /// The citation number backing the finding (an index into the citations list), or "UNKNOWN".
    #[serde(default = "unknown")]
    pub citation_ref: String,
    pub claim: String,
    pub evidence: String,
    #[serde(default)]
    pub impact: String,
    pub severity: String, // P0-P3
    pub label: String,
    #[serde(default = "unknown")]
    pub confidence: String,
    #[serde(default)]
    pub recommendation: String,
    #[serde(default)]
    pub lens: String,
    /// This lens's persona name (empty string if not set in the spec).
    #[serde(default)]
    pub reviewer: String,
    /// Citation reliability verdict. Initially populated by the LLM, but checks::verify_citations
    /// overwrites it with one of UNFETCHED|FETCH_FAILED|QUOTE_MATCHED|QUOTE_NOT_FOUND via an actual
    /// HTTP re-fetch + citation-text comparison (#4). The LLM's original value (in the
    /// VERIFIED|UNVERIFIED|STALE|CONTRADICTED schema) is kept in `llm_citation_status` for reference
    /// only — the source of truth is this field, as recomputed by code, not the LLM's original guess.
    #[serde(default = "unverified")]
    pub citation_status: String,
    /// The LLM's original citation_status verdict (reference only, surfaced as advisory in the report). Populated by code.
    #[serde(default)]
    pub llm_citation_status: String,
}

fn unknown() -> String {
    "UNKNOWN".to_string()
}

fn unverified() -> String {
    "UNVERIFIED".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LensOutput {
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub unverified: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoodThing {
    pub section: String,
    pub practice: String,
    pub why: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoodThingsOutput {
    #[serde(default)]
    pub good_things: Vec<GoodThing>,
}

/// For lenses with a persona assigned, prepend the character identity to the system prompt (to suppress sycophancy).
fn persona_system(lens: &Lens) -> String {
    if lens.persona_name.is_empty() {
        LENS_SYSTEM.to_string()
    } else {
        format!(
            "You are \"{}\". {}\nDo not agree just to agree — if your judgment differs from this identity's perspective, say so clearly.\n\n{}",
            lens.persona_name, lens.persona_voice, LENS_SYSTEM
        )
    }
}

/// Uses the LLM to select, from the candidate lenses (excluding "always"), those that fit this research type/document.
pub fn select_lenses(llm: &Llm, spec: &Spec, input: &Input) -> Result<Vec<String>> {
    let optional = spec.optional_lenses();
    if optional.is_empty() {
        return Ok(Vec::new());
    }
    let catalog = optional
        .iter()
        .map(|l| {
            let who = if l.persona_name.is_empty() { l.title.clone() } else { format!("{} ({})", l.title, l.persona_name) };
            format!("- id=\"{}\" | {} — selection signal: {}", l.id, who, l.signal)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let ctx = shared_context(spec, input);
    let task = format!(
        "# Task\nPick 3-5 verification lenses that fit the research document below (no changes after selection).\n\n\
         ## Candidate lenses\n{catalog}\n\n\
         ## Output (JSON only)\n{{\"selected\":[\"id\", ...]}}\n",
        catalog = catalog
    );
    let v = llm
        .json_ctx(Some(&ctx), &task, Some("You are a research director whose only job is lens selection. Respond strictly in the JSON schema."))
        .context("Lens selection failed")?;
    let selected: Vec<String> = v
        .get("selected")
        .and_then(|s| s.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let valid: Vec<String> = selected
        .into_iter()
        .filter(|id| spec.lens_by_id(id).is_some())
        .collect();
    anyhow::ensure!(!valid.is_empty(), "Lens selection result is empty, or contains only ids not present in the spec");
    Ok(valid)
}

fn build_review_task(spec: &Spec, lens_title: &str, lens_guide: &str) -> String {
    format!(
        "# Task\nIndependently verify the research document below from the \"{lens_title}\" perspective (do not reference other reviewers' results).\n\n\
         ## This lens's focus\n{lens_guide}\n\n\
         ## Verification principles\n\
         - For each finding, state the evidencing section (section) and citation number (citation_ref — one of the document's [n] citations, or UNKNOWN).\n\
         - severity is one of P0 (critical: factual error/corrupted figures) through P3 (minor) — follow the definitions in docs/design-spec.md §6.\n\
         - citation_status is one of VERIFIED (confirmed against the source)|UNVERIFIED (not checked)|STALE (outdated evidence)|CONTRADICTED (conflicts with other evidence).\n\
         - label must be exactly one of: {labels}\n\
         - Unsubstantiated suspicions go under unverified.\n\n\
         ## Output (JSON only, no code fences)\n\
         {{\"findings\":[{{\"section\":\"...\",\"citation_ref\":\"...\",\"claim\":\"...\",\"evidence\":\"...\",\
         \"impact\":\"...\",\"severity\":\"P0|P1|P2|P3\",\"label\":<one of the allowed values>,\
         \"confidence\":\"high|medium|low\",\"recommendation\":\"...\",\"citation_status\":\"VERIFIED|UNVERIFIED|STALE|CONTRADICTED\"}}],\"unverified\":[\"...\"]}}\n",
        lens_title = lens_title,
        lens_guide = lens_guide,
        labels = spec.labels_prompt(),
    )
}

pub fn review_lens(llm: &Llm, spec: &Spec, input: &Input, lens_id: &str) -> Result<LensOutput> {
    let lens = spec
        .lens_by_id(lens_id)
        .ok_or_else(|| anyhow::anyhow!("Lens not found in spec: {lens_id}"))?;
    let ctx = shared_context(spec, input);
    let task = build_review_task(spec, &lens.title, &lens.guide);
    let system = persona_system(lens);
    let v = llm
        .json_ctx(Some(&ctx), &task, Some(&system))
        .with_context(|| format!("Lens review failed: {lens_id}"))?;
    let mut out: LensOutput =
        serde_json::from_value(v).with_context(|| format!("Lens review JSON schema mismatch: {lens_id}"))?;
    let reviewer = if lens.persona_name.is_empty() { lens.title.clone() } else { lens.persona_name.clone() };
    for (i, f) in out.findings.iter_mut().enumerate() {
        f.id = format!("{}-{}", lens_id, i + 1);
        f.lens = lens_id.to_string();
        f.reviewer = reviewer.clone();
        if f.citation_ref.trim().is_empty() {
            f.citation_ref = unknown();
        }
    }
    Ok(out)
}

const GOOD_THINGS_GUIDE: &str = "Find concrete research practices worth preserving (e.g. citing real measured data, honestly disclosing access limitations). Do not manufacture unsubstantiated praise.";

pub fn review_good_things(llm: &Llm, spec: &Spec, input: &Input) -> Result<GoodThingsOutput> {
    let ctx = shared_context(spec, input);
    let task = format!(
        "# Task\nFind good research practices worth preserving in the research document below.\n\n\
         ## This lens's focus\n{guide}\n\n\
         ## Output (JSON only, no code fences)\n\
         {{\"good_things\":[{{\"section\":\"...\",\"practice\":\"...\",\"why\":\"...\"}}]}}\n\
         If there is no concrete example to cite as evidence, return good_things as an empty array.\n",
        guide = GOOD_THINGS_GUIDE,
    );
    let v = llm.json_ctx(Some(&ctx), &task, Some(LENS_SYSTEM)).context("Good Things lens failed")?;
    let out: GoodThingsOutput =
        serde_json::from_value(v).context("Good Things JSON schema mismatch")?;
    Ok(out)
}
