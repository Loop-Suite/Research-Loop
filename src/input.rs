use anyhow::{Context, Result};
use regex::Regex;
use std::io::Read;
use std::path::Path;

/// #19: upper bound on any single input file (document/brief/style/deterministic-results).
/// Reading these with plain `std::fs::read_to_string` had no size limit at all — a huge file
/// (wrong path pointed at a large export/log by mistake) or a symlink to an infinite-but-valid-
/// UTF-8 special file (e.g. `/dev/zero`, all NUL bytes) would be read fully into memory with no
/// bound, risking OOM before `main.rs`'s DOC_WARN_CHARS check even gets a chance to run (that
/// check only warns about cost, and only *after* the full read already succeeded). 64 MiB is
/// far beyond any realistic research document/brief/style guide, but small enough to fail fast
/// and cleanly instead of exhausting memory.
const MAX_INPUT_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Reads `path` with a hard byte cap instead of unconditional `std::fs::read_to_string`. Bounds
/// worst-case memory regardless of the file's reported size or type (a cap alone isn't enough
/// for special files whose metadata lies about length, so this bounds the actual bytes read via
/// `Read::take`, not just a pre-check against `fs::metadata`).
fn read_to_string_capped(path: &Path) -> Result<String> {
    read_to_string_capped_with_limit(path, MAX_INPUT_FILE_BYTES)
}

/// `max_bytes`-parameterized so tests can exercise the cap without writing multi-megabyte fixture
/// files. Production callers always go through [`read_to_string_capped`], which fixes
/// `max_bytes` at [`MAX_INPUT_FILE_BYTES`].
fn read_to_string_capped_with_limit(path: &Path, max_bytes: u64) -> Result<String> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut buf = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut buf)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;
    anyhow::ensure!(
        (buf.len() as u64) <= max_bytes,
        "File exceeds the {max_bytes}-byte size limit: {} — refusing to read further to avoid \
         unbounded memory use (this can also happen if the path is a symlink to a huge or \
         special file)",
        path.display()
    );
    String::from_utf8(buf).with_context(|| format!("File is not valid UTF-8: {}", path.display()))
}

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
        Some(path) => Ok(Some(read_to_string_capped(path)?)),
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
    let document = read_to_string_capped(document_path).with_context(|| {
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
            let s = read_to_string_capped(p).with_context(|| {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "research_loop_input_test_{tag}_{}",
            std::process::id()
        ));
        p
    }

    /// Regression test for issue #19: a file under the limit must still read normally.
    #[test]
    fn read_capped_succeeds_under_the_limit() {
        let path = temp_path("under_limit");
        std::fs::write(&path, "hello world").unwrap();
        let result = read_to_string_capped_with_limit(&path, 100);
        let _ = std::fs::remove_file(&path);
        assert_eq!(result.unwrap(), "hello world");
    }

    /// Regression test for issue #19: previously `std::fs::read_to_string` had no size bound at
    /// all, so an oversized file was read fully into memory before any check ran. A file one
    /// byte over the limit must be rejected with a clear error instead.
    #[test]
    fn read_capped_rejects_a_file_one_byte_over_the_limit() {
        let path = temp_path("over_limit");
        std::fs::write(&path, "x".repeat(11)).unwrap();
        let result = read_to_string_capped_with_limit(&path, 10);
        let _ = std::fs::remove_file(&path);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("size limit"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn read_capped_accepts_a_file_exactly_at_the_limit() {
        let path = temp_path("exact_limit");
        std::fs::write(&path, "x".repeat(10)).unwrap();
        let result = read_to_string_capped_with_limit(&path, 10);
        let _ = std::fs::remove_file(&path);
        assert_eq!(result.unwrap(), "x".repeat(10));
    }

    /// Regression test for issue #19's symlink-to-special-file scenario: `/dev/zero` is an
    /// infinite (never-EOF) source of valid UTF-8 (NUL bytes) whose `fs::metadata().len()`
    /// reports 0 — so a naive pre-check against metadata would not catch it. This proves the cap
    /// is enforced on actual bytes read (via `Read::take`), so a symlink to `/dev/zero` fails
    /// fast with a clear error instead of growing a buffer forever.
    #[cfg(unix)]
    #[test]
    fn read_capped_terminates_on_symlink_to_dev_zero() {
        let path = temp_path("symlink_to_dev_zero");
        let _ = std::fs::remove_file(&path);
        std::os::unix::fs::symlink("/dev/zero", &path).unwrap();
        let result = read_to_string_capped_with_limit(&path, 1024);
        let _ = std::fs::remove_file(&path);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("size limit"),
            "unexpected error message: {err}"
        );
    }

    /// Corrupted/non-UTF8 input must fail cleanly, not panic — e.g. a document saved with the
    /// wrong encoding, or truncated mid-multibyte-sequence.
    #[test]
    fn read_capped_rejects_invalid_utf8() {
        let path = temp_path("invalid_utf8");
        // 0xFF is never valid as a UTF-8 lead byte.
        std::fs::write(&path, [0xFFu8, 0x00, 0x01, 0x02]).unwrap();
        let result = read_to_string_capped_with_limit(&path, 1024);
        let _ = std::fs::remove_file(&path);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not valid UTF-8"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn normalize_rejects_empty_document() {
        let path = temp_path("empty_doc.md");
        std::fs::write(&path, "").unwrap();
        let result = normalize(&path, &None, &None, &None);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err(), "an empty document must be rejected");
    }

    #[test]
    fn normalize_rejects_whitespace_only_document() {
        let path = temp_path("blank_doc.md");
        std::fs::write(&path, "   \n\t\n   \n").unwrap();
        let result = normalize(&path, &None, &None, &None);
        let _ = std::fs::remove_file(&path);
        assert!(
            result.is_err(),
            "a whitespace-only document must be rejected"
        );
    }

    /// Unicode extremes (RTL Arabic/Hebrew, an emoji ZWJ sequence, stacked combining
    /// diacritics) mixed directly into headings/citations/body text must not panic anywhere in
    /// the extract_sections/extract_citations/word_count path (all of which do byte-offset
    /// string work via regex) and must still parse structurally correctly.
    #[test]
    fn normalize_handles_unicode_extremes_without_panicking() {
        let doc = "## \u{645}\u{631}\u{62d}\u{628}\u{627} (Arabic heading)\n\
            Family emoji ZWJ sequence: \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}\n\
            Combining diacritics: e\u{0301}\u{0301}\u{0301} (e + combining acute x3)\n\
            \u{5e2}\u{5d1}\u{5e8}\u{5d9}\u{5ea} (Hebrew) citation [\u{1f4ce}](https://example.com/a)\n";
        let path = temp_path("unicode_extreme_doc.md");
        std::fs::write(&path, doc).unwrap();
        let result = normalize(&path, &None, &None, &None);
        let _ = std::fs::remove_file(&path);
        let inp = result.expect("a unicode-heavy document must parse without panicking");
        assert_eq!(inp.sections.len(), 1);
        assert_eq!(inp.citations.len(), 1);
        assert_eq!(inp.citations[0].url, "https://example.com/a");
    }

    /// A `--deterministic-results` file that isn't even syntactically valid JSON must fail
    /// cleanly through normalize()'s serde_json::from_str, not panic.
    #[test]
    fn normalize_rejects_malformed_deterministic_results_json() {
        let doc_path = temp_path("doc_for_malformed_det.md");
        std::fs::write(&doc_path, "## Section\nSome content.\n").unwrap();
        let det_path = temp_path("malformed_det.json");
        std::fs::write(&det_path, "{ this is not valid json").unwrap();

        let result = normalize(&doc_path, &None, &None, &Some(det_path.clone()));

        let _ = std::fs::remove_file(&doc_path);
        let _ = std::fs::remove_file(&det_path);
        assert!(
            result.is_err(),
            "malformed deterministic-results JSON must be rejected, not panic"
        );
    }
}
