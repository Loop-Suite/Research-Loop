use anyhow::{Context, Result};
use regex::Regex;
use std::path::Path;

/// A single citation (Markdown link) within the document.
#[derive(Debug, Clone)]
pub struct Citation {
    pub index: usize,
    pub text: String,
    pub url: String,
}

/// Normalized input. Missing information is left as None, and UNKNOWN handling is displayed by the caller (report).
pub struct Input {
    pub document: String,
    /// List of `## ` headings (counterpart to codereview-loop's changed_files — used for section-level evidence references).
    pub sections: Vec<String>,
    pub word_count: usize,
    pub citations: Vec<Citation>,
    /// Research brief: the list of angles that must be covered (counterpart to codereview-loop's requirements).
    pub requirements: Option<String>,
    /// Tone/format guide (counterpart to codereview-loop's conventions).
    pub conventions: Option<String>,
    /// Result from checks.rs. If None, computed independently while the review subcommand runs.
    pub deterministic_results: Option<serde_json::Value>,
}

fn read_opt(p: &Option<std::path::PathBuf>) -> Result<Option<String>> {
    match p {
        None => Ok(None),
        Some(path) => {
            let s = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read file: {}", path.display()))?;
            Ok(Some(s))
        }
    }
}

fn extract_sections(doc: &str) -> Vec<String> {
    doc.lines()
        .filter_map(|l| l.strip_prefix("## ").map(|s| s.trim().to_string()))
        .collect()
}

/// Extracts all Markdown links `[text](url)`. Only the http(s) scheme is treated as a citation (internal anchors `#` excluded).
fn extract_citations(doc: &str) -> Vec<Citation> {
    let re = Regex::new(r"\[([^\]]*)\]\((https?://[^)\s]+)\)")
        .expect("failed to compile citation regex");
    re.captures_iter(doc)
        .enumerate()
        .map(|(i, c)| Citation {
            index: i + 1,
            text: c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default(),
            url: c.get(2).map(|m| m.as_str().to_string()).unwrap_or_default(),
        })
        .collect()
}

pub fn normalize(
    document_path: &Path,
    requirements_path: &Option<std::path::PathBuf>,
    conventions_path: &Option<std::path::PathBuf>,
    deterministic_results_path: &Option<std::path::PathBuf>,
) -> Result<Input> {
    let document = std::fs::read_to_string(document_path).with_context(|| {
        format!(
            "Failed to read research document: {}",
            document_path.display()
        )
    })?;
    anyhow::ensure!(!document.trim().is_empty(), "Research document is empty");

    let sections = extract_sections(&document);
    let citations = extract_citations(&document);
    let word_count = document.split_whitespace().count();

    let requirements = read_opt(requirements_path)?;
    let conventions = read_opt(conventions_path)?;
    let deterministic_results = match deterministic_results_path {
        None => None,
        Some(p) => {
            let s = std::fs::read_to_string(p).with_context(|| {
                format!("Failed to read deterministic results file: {}", p.display())
            })?;
            Some(serde_json::from_str(&s).with_context(|| {
                format!(
                    "Failed to parse deterministic results JSON: {}",
                    p.display()
                )
            })?)
        }
    };

    Ok(Input {
        document,
        sections,
        word_count,
        citations,
        requirements,
        conventions,
        deterministic_results,
    })
}
