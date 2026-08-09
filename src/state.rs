use crate::discourse::Resolution;
use crate::lens::Finding;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Fingerprint for reproducibility/auditing. Not a cryptographic hash — it's a raw 64-bit value from
/// `std::hash::DefaultHasher` (SipHash13 with a fixed key `(0,0)`, so it's deterministic regardless of
/// process/machine). The purpose is to quickly compare "did the input change on the next round/rerun,"
/// not to verify security integrity (#9 — introduces the RunManifest concept; a full SHA-256-grade hash is out of scope).
fn fingerprint(s: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn fingerprint_str(s: &str) -> String {
    fingerprint(s)
}

/// UTC unix timestamp (seconds) as a string. To avoid pulling in a new chrono dependency, this
/// records only epoch seconds with no calendar formatting — always UTC-based, with no timezone ambiguity.
pub fn unix_ts() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Snapshot of findings and verdicts at the end of a round. Carried forward by the next round (--prior).
///
/// #9: added reproducibility/audit fields on top of what used to be just the 3 fields
/// round/findings/resolved (the RunManifest concept). All are `#[serde(default)]`, so old
/// state.json files (--prior) that lack these fields still load fine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub round: usize,
    pub findings: Vec<Finding>,
    pub resolved: HashMap<String, Resolution>,
    /// Fingerprint of the raw input document text (non-cryptographic, see [`fingerprint`]).
    #[serde(default)]
    pub input_hash: String,
    /// Fingerprint of the full serialized spec (TOML).
    #[serde(default)]
    pub spec_hash: String,
    /// The model id used for the run (unspecified means the backend default — stays an empty string).
    #[serde(default)]
    pub model_id: String,
    /// "claude-cli" | "openrouter".
    #[serde(default)]
    pub provider: String,
    /// Round start time (UTC unix epoch seconds, as a string).
    #[serde(default)]
    pub started_at: String,
    /// Round completion time (UTC unix epoch seconds, as a string).
    #[serde(default)]
    pub completed_at: String,
    /// Accumulated llm.usage().cost_usd (0.0 if the claude CLI doesn't provide a value — OpenRouter responses have no cost field).
    #[serde(default)]
    pub cost_usd: f64,
    /// Prompt schema version. Bumped manually whenever the prompt structure (JSON schema, etc.) changes —
    /// used to distinguish "did the prompt itself change" when comparing results against past rounds.
    #[serde(default)]
    pub prompt_version: String,
}

pub fn write(out_dir: &Path, state: &State) -> Result<PathBuf> {
    let path = out_dir.join("state.json");
    std::fs::write(&path, serde_json::to_string_pretty(state)?)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}

pub fn load(dir: &Path) -> Result<State> {
    let path = if dir.is_dir() { dir.join("state.json") } else { dir.to_path_buf() };
    let s = std::fs::read_to_string(&path).with_context(|| format!("Failed to read {} (--prior should be a previous --out directory)", path.display()))?;
    serde_json::from_str(&s).with_context(|| format!("Failed to parse {}", path.display()))
}
