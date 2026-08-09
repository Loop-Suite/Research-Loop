use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A research lens (one of 7 personas, selected to fit the research-type's nature).
/// Has the same fields as codereview-loop's Lens — only the prompt (guide/persona_voice) distinguishes the domain.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Lens {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub guide: String,
    /// If true, always force-included at the lens selection stage.
    #[serde(default)]
    pub always: bool,
    /// The signal that causes this lens to be chosen (inserted verbatim into the selection prompt).
    #[serde(default)]
    pub signal: String,
    /// Characterized persona name (empty means no persona). Intended to suppress sycophancy.
    #[serde(default)]
    pub persona_name: String,
    /// One-line statement of the persona's perspective/principle.
    #[serde(default)]
    pub persona_voice: String,
    /// Display-only string (e.g. 1/2). Not used in selection logic — see docs/design-spec.md §1 assumptions.
    #[serde(default)]
    pub tier: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Spec {
    pub name: String,
    /// The research target/context (e.g. "domestic cafe POS competitors"). Inserted verbatim into the prompt.
    #[serde(default)]
    pub context: String,
    pub lenses: Vec<Lens>,
    /// The list of labels allowed on findings.
    pub labels: Vec<String>,
    /// List of domains directly published by the research subject company (used to identify self-published content, source_diversity_check).
    /// E.g. ["tossplace.com", "payhere.in"]. If empty, that check is NOT_CONFIGURED.
    #[serde(default)]
    pub subject_owned_domains: Vec<String>,
    /// Threshold (in years) for judging cited evidence as "stale". 0 means unset (staleness_flag disabled).
    #[serde(default)]
    pub staleness_threshold_years: u32,
    /// List of checks.rs item ids that always run regardless of whether deterministic checks are enabled.
    /// If empty, all items checks.rs is able to compute are run.
    #[serde(default)]
    pub enabled_checks: Vec<String>,
}

impl Spec {
    pub fn load(path: &Path) -> Result<Spec> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read spec file: {}", path.display()))?;
        let spec: Spec = toml::from_str(&s)
            .with_context(|| format!("Failed to parse spec TOML: {}", path.display()))?;
        anyhow::ensure!(!spec.lenses.is_empty(), "lenses is empty");
        anyhow::ensure!(!spec.labels.is_empty(), "labels is empty");
        Ok(spec)
    }

    pub fn lens_by_id(&self, id: &str) -> Option<&Lens> {
        self.lenses.iter().find(|l| l.id == id)
    }

    pub fn always_lenses(&self) -> Vec<&Lens> {
        self.lenses.iter().filter(|l| l.always).collect()
    }

    pub fn optional_lenses(&self) -> Vec<&Lens> {
        self.lenses.iter().filter(|l| !l.always).collect()
    }

    pub fn labels_prompt(&self) -> String {
        self.labels
            .iter()
            .map(|l| format!("\"{l}\""))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn check_enabled(&self, id: &str) -> bool {
        self.enabled_checks.is_empty() || self.enabled_checks.iter().any(|c| c == id)
    }
}
