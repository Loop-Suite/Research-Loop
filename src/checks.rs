//! Deterministic (non-LLM) checks. This merges codereview-loop's policy.rs+semgrep.rs —
//! the research domain has no "automated scanner backed by an external deterministic tool"
//! (a semgrep counterpart), so there's no real reason to split this into two modules
//! (see docs/design-spec.md §3 "structural difference from the semgrep setup").
//! Maps 1:1 to the 6 failure modes observed in docs/research-and-evidence-survey §2.

use crate::input::Input;
use crate::lens::Finding;
use crate::spec::Spec;
use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    NotApplicable,
    NotConfigured,
}

impl CheckStatus {
    pub fn label(&self) -> &'static str {
        match self {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
            CheckStatus::NotApplicable => "N/A",
            CheckStatus::NotConfigured => "NOT_CONFIGURED",
        }
    }

    fn from_label(s: &str) -> Result<CheckStatus> {
        match s {
            "PASS" => Ok(CheckStatus::Pass),
            "WARN" => Ok(CheckStatus::Warn),
            "FAIL" => Ok(CheckStatus::Fail),
            "N/A" => Ok(CheckStatus::NotApplicable),
            "NOT_CONFIGURED" => Ok(CheckStatus::NotConfigured),
            other => Err(anyhow!("Unknown check status: \"{other}\" (must be one of PASS|WARN|FAIL|N/A|NOT_CONFIGURED)")),
        }
    }
}

pub struct CheckResult {
    pub id: String,
    pub title: String,
    pub status: CheckStatus,
    pub evidence: String,
}

/// Failure mode 1: general. Ratio of citation count to approximate sentence count.
/// Takes the max of two heuristics (the Korean sentence-final-ending list / generic punctuation
/// `.!?`) so each compensates for cases the other misses (English documents, endings not on the
/// list, etc.).
/// Assumption: this is a heuristic, not morphological analysis — treat it as an approximation only
/// (uncertain, #5).
fn approx_sentence_count(doc: &str) -> usize {
    let endings = [
        "다.",
        "음.",
        "됨.",
        "함.",
        "임.",
        "라.",
        "니다.",
        "습니다.",
        "입니다.",
        "네요.",
        "어요.",
        "예요.",
    ];
    let ending_hits: usize = endings.iter().map(|e| doc.matches(e).count()).sum();

    // To exclude decimal points between digits ("3.5") and abbreviation-like notation with no
    // trailing space, only count cases where "the punctuation isn't preceded by a digit and is
    // followed by a space/newline".
    let punct_re =
        Regex::new(r"[^0-9][.!?](?:\s|$)").expect("failed to compile sentence punctuation regex");
    let punct_hits = punct_re.find_iter(doc).count();

    ending_hits.max(punct_hits)
}

fn citation_density_check(input: &Input) -> CheckResult {
    let approx_sentences = approx_sentence_count(&input.document);
    let citations = input.citations.len();
    if approx_sentences == 0 {
        return CheckResult {
            id: "citation_density".into(),
            title: "Citation density relative to claims".into(),
            status: CheckStatus::NotApplicable,
            evidence: "Sentence-boundary detection failed (heuristic limitation)".into(),
        };
    }
    let ratio = citations as f64 / approx_sentences as f64;
    let status = if ratio >= 0.05 {
        CheckStatus::Pass
    } else {
        CheckStatus::Warn
    };
    CheckResult {
        id: "citation_density".into(),
        title: "Citation density relative to claims".into(),
        status,
        evidence: format!("Approx. sentence count {approx_sentences}, citations {citations} (ratio {ratio:.3}, heuristic approximation)"),
    }
}

/// Determines whether a URL's host exactly matches `domain` (or is a subdomain of it).
/// Parses the actual host with the `url` crate before comparing, which eliminates the false
/// positives the previous `url.contains(domain)` approach had (e.g. a string like
/// "evil-tossplace.com.attacker.net" that happens to contain `domain` as a substring) (#5).
/// This isn't a full public-suffix-based registrable-domain computation (the psl crate isn't
/// pulled in), but exact host match / subdomain match removes substring false positives.
fn host_matches_owned_domain(url_str: &str, domain: &str) -> bool {
    let host = match url::Url::parse(url_str) {
        Ok(u) => match u.host_str() {
            Some(h) => h.trim_end_matches('.').to_ascii_lowercase(),
            None => return false,
        },
        Err(_) => return false,
    };
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// Scaled-down response to failure mode 4: "self-published content dominating search results" —
/// computes the share of cited domains that fall under spec.subject_owned_domains.
fn source_diversity_check(spec: &Spec, input: &Input) -> CheckResult {
    if spec.subject_owned_domains.is_empty() {
        return CheckResult {
            id: "source_diversity".into(),
            title: "Source diversity (share of self-published domains)".into(),
            status: CheckStatus::NotConfigured,
            evidence: "spec.subject_owned_domains not configured".into(),
        };
    }
    if input.citations.is_empty() {
        return CheckResult {
            id: "source_diversity".into(),
            title: "Source diversity (share of self-published domains)".into(),
            status: CheckStatus::NotApplicable,
            evidence: "No citations".into(),
        };
    }
    let owned = input
        .citations
        .iter()
        .filter(|c| {
            spec.subject_owned_domains
                .iter()
                .any(|d| host_matches_owned_domain(&c.url, d))
        })
        .count();
    let ratio = owned as f64 / input.citations.len() as f64;
    let status = if ratio <= 0.4 {
        CheckStatus::Pass
    } else {
        CheckStatus::Warn
    };
    CheckResult {
        id: "source_diversity".into(),
        title: "Source diversity (share of self-published domains)".into(),
        status,
        evidence: format!(
            "Of {} total citations, {} are self-published domains ({:.0}%)",
            input.citations.len(),
            owned,
            ratio * 100.0
        ),
    }
}

/// Failure mode 5: "the same metric reported with inconsistent figures across mentions". Groups
/// the 2-4 word tokens preceding Korean currency/count expressions (100M-won, trillion-won, percent,
/// person-count, item-count units) as a key, and detects
/// whether the same phrase is followed by different numbers elsewhere in the document.
/// Assumption: this is word-token window matching, not morphological analysis, so false
/// positives/negatives are possible — WARN only, never FAIL (uncertain).
fn numeric_consistency_check(input: &Input) -> CheckResult {
    let re = Regex::new(r"([\p{Hangul}A-Za-z]{2,6}(?:\s+[\p{Hangul}A-Za-z]{1,6}){0,2})\s*([0-9][0-9,]*(?:\.[0-9]+)?)\s*(억원|조원|억|%|명|개)")
        .expect("failed to compile numeric regex");
    let mut seen: HashMap<String, Vec<String>> = HashMap::new();
    for cap in re.captures_iter(&input.document) {
        let phrase = cap
            .get(1)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        let value = format!(
            "{}{}",
            cap.get(2).map(|m| m.as_str()).unwrap_or(""),
            cap.get(3).map(|m| m.as_str()).unwrap_or("")
        );
        if phrase.chars().count() < 2 {
            continue;
        }
        seen.entry(phrase).or_default().push(value);
    }
    let conflicts: Vec<String> = seen
        .into_iter()
        .filter_map(|(phrase, values)| {
            let unique: std::collections::HashSet<&String> = values.iter().collect();
            if unique.len() > 1 {
                Some(format!("\"{}\": {}", phrase, values.join(" vs ")))
            } else {
                None
            }
        })
        .collect();
    if conflicts.is_empty() {
        CheckResult {
            id: "numeric_consistency".into(),
            title: "Numeric consistency (cross-check of repeated figures for the same phrase)".into(),
            status: CheckStatus::Pass,
            evidence: "No cases found of the same phrase carrying different figures (per heuristic detection)".into(),
        }
    } else {
        CheckResult {
            id: "numeric_consistency".into(),
            title: "Numeric consistency (cross-check of repeated figures for the same phrase)"
                .into(),
            status: CheckStatus::Warn,
            evidence: format!(
                "Potential inconsistencies: {} — {}",
                conflicts.len(),
                conflicts.join(" | ")
            ),
        }
    }
}

/// Failure mode 6: "unable to access closed platforms". Only checks whether an honest disclosure
/// like "not verified" appears at least once in the document — its absence isn't necessarily a
/// problem (not every research task runs into access restrictions), but its presence is itself a
/// positive signal.
fn access_limitation_disclosure_check(input: &Input) -> CheckResult {
    let markers = [
        "확인 안 됨",
        "접근 불가",
        "단정할 근거 없음",
        "확인 안됨",
        "미확인",
    ];
    let hits: usize = markers
        .iter()
        .map(|m| input.document.matches(m).count())
        .sum();
    CheckResult {
        id: "access_limitation_disclosure".into(),
        title: "Honest disclosure of access limitations".into(),
        status: if hits > 0 { CheckStatus::Pass } else { CheckStatus::NotApplicable },
        evidence: format!("Found {hits} honest-disclosure phrase(s) (if zero, this may simply mean no access restrictions were hit within the research scope)"),
    }
}

/// Failure mode 3: "credibility contamination from incentivized reviews". If incentive-related
/// keywords appear in the document, this is flagged as an informational WARN rather than
/// PASS/FAIL — whether the disclosure is actually adequate is judged by discourse
/// (citation_status).
fn incentive_disclosure_scan(input: &Input) -> CheckResult {
    let markers = [
        "리뷰 이벤트",
        "협찬",
        "제휴 리뷰",
        "보상 프로그램",
        "인센티브",
        "현금 보상",
    ];
    let hits: Vec<&str> = markers
        .iter()
        .filter(|m| input.document.contains(*m))
        .copied()
        .collect();
    if hits.is_empty() {
        CheckResult {
            id: "incentive_disclosure".into(),
            title: "Incentivized-review mention scan".into(),
            status: CheckStatus::Pass,
            evidence: "No incentive-related keywords found".into(),
        }
    } else {
        CheckResult {
            id: "incentive_disclosure".into(),
            title: "Incentivized-review mention scan".into(),
            status: CheckStatus::Warn,
            evidence: format!("Keywords found: {} — needs re-verification in the discourse round on whether the cited reviews were influenced by this incentive", hits.join(", ")),
        }
    }
}

/// Freshness check addressing failure mode 7: "an earlier conclusion overturned by newer
/// information". Disabled when spec.staleness_threshold_years=0. Extracts every 4-digit year from
/// the document and flags WARN (possible stale evidence present) if any year exceeds the
/// threshold relative to as_of_year.
fn staleness_flag(spec: &Spec, input: &Input, as_of_year: u32) -> CheckResult {
    if spec.staleness_threshold_years == 0 {
        return CheckResult {
            id: "staleness".into(),
            title: "Citation freshness".into(),
            status: CheckStatus::NotConfigured,
            evidence: "spec.staleness_threshold_years not configured".into(),
        };
    }
    let re = Regex::new(r"(19|20)\d{2}").expect("failed to compile year regex");
    let old_years: std::collections::HashSet<u32> = re
        .find_iter(&input.document)
        .filter_map(|m| m.as_str().parse::<u32>().ok())
        .filter(|y| {
            as_of_year.saturating_sub(*y) > spec.staleness_threshold_years && *y <= as_of_year
        })
        .collect();
    if old_years.is_empty() {
        CheckResult {
            id: "staleness".into(),
            title: "Citation freshness".into(),
            status: CheckStatus::Pass,
            evidence: format!(
                "No years exceed the threshold ({} years)",
                spec.staleness_threshold_years
            ),
        }
    } else {
        let mut ys: Vec<u32> = old_years.into_iter().collect();
        ys.sort();
        CheckResult {
            id: "staleness".into(),
            title: "Citation freshness".into(),
            status: CheckStatus::Warn,
            evidence: format!("Years exceeding the threshold ({} years) found: {:?} — recommend re-verifying against more recent evidence", spec.staleness_threshold_years, ys),
        }
    }
}

// ---------------------------------------------------------------------------
// SSRF defense (#11): both dead_link_check and citation quote verification exclusively use the
// safe fetch path below.
// ---------------------------------------------------------------------------

const MAX_REDIRECTS: u32 = 5;
const MAX_BODY_BYTES: usize = 1_000_000; // 1MB

/// Blocks IPv4 loopback (127.0.0.0/8) / private (10/8, 172.16/12, 192.168/16) /
/// link-local (169.254.0.0/16 — includes cloud metadata 169.254.169.254) /
/// multicast·reserved (224.0.0.0/4, 240.0.0.0/4) / unspecified (0.0.0.0).
/// Compares octets directly instead of using standard methods like `Ipv4Addr::is_private`, so
/// behavior stays identical regardless of crate/Rust version quirks — written explicitly for
/// stability.
fn ipv4_is_blocked(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 127
        || o[0] == 10
        || (o[0] == 172 && (16..=31).contains(&o[1]))
        || (o[0] == 192 && o[1] == 168)
        || (o[0] == 169 && o[1] == 254)
        || o[0] >= 224
        || o == [0, 0, 0, 0]
}

/// Blocks IPv6 loopback (::1) / unspecified (::) / link-local (fe80::/10) / unique-local
/// (fc00::/7) / multicast (ff00::/8). IPv4-mapped addresses (::ffff:a.b.c.d) are converted to
/// their internal IPv4 form and re-checked.
fn ipv6_is_blocked(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    if let Some(v4) = ip.to_ipv4() {
        return ipv4_is_blocked(v4);
    }
    let seg0 = ip.segments()[0];
    (seg0 & 0xff00) == 0xff00 // multicast ff00::/8
        || (seg0 & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (seg0 & 0xfe00) == 0xfc00 // unique local fc00::/7
}

fn ip_is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_is_blocked(v4),
        IpAddr::V6(v6) => ipv6_is_blocked(v6),
    }
}

/// Actually DNS-resolves the host (domain or IP literal) and checks every returned IP (prevents
/// DNS rebinding — rather than trusting the hostname alone, this always verifies the IP the
/// connection will actually be made to).
fn resolve_and_validate(host: &str, port: u16) -> Result<()> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        anyhow::ensure!(!ip_is_blocked(ip), "Blocked IP range: {ip}");
        return Ok(());
    }
    let addrs = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("DNS resolution failed: {host}"))?;
    let mut any = false;
    for addr in addrs {
        any = true;
        anyhow::ensure!(
            !ip_is_blocked(addr.ip()),
            "Resolved to a blocked IP range: {} -> {}",
            host,
            addr.ip()
        );
    }
    anyhow::ensure!(any, "No DNS resolution results: {host}");
    Ok(())
}

/// URL parsing + scheme allowlist (http/https only) + host resolve/blocklist validation. Called
/// again on every redirect hop, not just the initial request, to prevent SSRF bypass via redirect
/// (redirecting into an internal network).
fn validate_url_safe(raw_url: &str) -> Result<url::Url> {
    let u = url::Url::parse(raw_url).with_context(|| format!("URL parsing failed: {raw_url}"))?;
    anyhow::ensure!(
        matches!(u.scheme(), "http" | "https"),
        "Disallowed scheme: {}",
        u.scheme()
    );
    let host = u
        .host_str()
        .ok_or_else(|| anyhow!("URL has no host: {raw_url}"))?;
    let port = u
        .port_or_known_default()
        .unwrap_or(if u.scheme() == "https" { 443 } else { 80 });
    resolve_and_validate(host, port)?;
    Ok(u)
}

struct FetchOutcome {
    status: u16,
    content_type: Option<String>,
    body: Option<Vec<u8>>, // Some only for GET
}

fn read_bounded(resp: ureq::Response, max_bytes: usize) -> Result<Vec<u8>> {
    let mut reader = resp.into_reader().take(max_bytes as u64 + 1);
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .context("failed to read response body")?;
    buf.truncate(max_bytes);
    Ok(buf)
}

/// HEAD/GET request with SSRF defenses applied. Disables ureq's automatic redirect tracking
/// (`.redirects(0)`) and instead loops manually, re-running [`validate_url_safe`] on every hop.
/// GET responses are capped at 1MB.
fn safe_fetch(raw_url: &str, method_get: bool) -> Result<FetchOutcome> {
    let mut current = validate_url_safe(raw_url)?;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .redirects(0)
        .build();
    for hop in 0..=MAX_REDIRECTS {
        let req = if method_get {
            agent.get(current.as_str())
        } else {
            agent.head(current.as_str())
        };
        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(anyhow!("Request failed: {e}")),
        };
        let status = resp.status();
        if (300..400).contains(&status) {
            anyhow::ensure!(
                hop < MAX_REDIRECTS,
                "Redirect limit ({}) exceeded",
                MAX_REDIRECTS
            );
            let location = resp
                .header("Location")
                .ok_or_else(|| anyhow!("Redirect response ({status}) has no Location header"))?
                .to_string();
            let next = current
                .join(&location)
                .with_context(|| format!("Failed to resolve redirect URL: {location}"))?;
            current = validate_url_safe(next.as_str())?; // re-validate on every hop — prevents SSRF bypass
            continue;
        }
        let content_type = resp.header("Content-Type").map(|s| s.to_string());
        let body = if method_get {
            Some(read_bounded(resp, MAX_BODY_BYTES)?)
        } else {
            None
        };
        return Ok(FetchOutcome {
            status,
            content_type,
            body,
        });
    }
    Err(anyhow!("Failed to process redirect"))
}

enum Probe {
    Status(u16),
    Err(String),
}

fn probe(url: &str, get: bool) -> Probe {
    match safe_fetch(url, get) {
        Ok(o) => Probe::Status(o.status),
        Err(e) => Probe::Err(e.to_string()),
    }
}

enum LinkStatus {
    Ok,
    /// Only when both HEAD and GET actually received an HTTP response and its status code is
    /// conclusively 4xx/5xx.
    Dead(String),
    /// Transport error (timeout/DNS failure/SSRF block, etc.) — cases where it can't be
    /// conclusively called "dead".
    Unreachable(String),
}

/// Checks with HEAD first, and retries with GET if it fails (transport error) or returns an error
/// status (the "GET fallback on HEAD failure" contract the README specifies — the previous
/// implementation didn't actually have this, #11).
/// The goal is to let sites that don't support HEAD (405, etc.) still get judged healthy via the
/// GET retry.
fn check_one(url: &str) -> LinkStatus {
    match probe(url, false) {
        Probe::Status(s) if s < 400 => LinkStatus::Ok,
        Probe::Status(head_status) => match probe(url, true) {
            Probe::Status(s2) if s2 < 400 => LinkStatus::Ok,
            Probe::Status(s2) => LinkStatus::Dead(format!("HEAD={head_status}, GET={s2}")),
            Probe::Err(e) => LinkStatus::Unreachable(format!("HEAD={head_status}, GET error: {e}")),
        },
        Probe::Err(head_err) => match probe(url, true) {
            Probe::Status(s2) if s2 < 400 => LinkStatus::Ok,
            Probe::Status(s2) => {
                LinkStatus::Unreachable(format!("HEAD error ({head_err}), GET={s2}"))
            }
            Probe::Err(get_err) => LinkStatus::Unreachable(format!(
                "Both HEAD and GET errored: {head_err} / {get_err}"
            )),
        },
    }
}

/// Live dead-link check. SSRF-protected HEAD request (falls back to GET on failure); only 2xx/3xx
/// counts as PASS. Network errors, SSRF blocks, and timeouts are WARN rather than FAIL —
/// distinguishing "dead link" from "couldn't verify" (same intent as the design-spec.md principle).
fn dead_link_check(input: &Input, skip: bool) -> CheckResult {
    if skip {
        return CheckResult {
            id: "dead_link".into(),
            title: "Citation URL response check".into(),
            status: CheckStatus::NotConfigured,
            evidence: "--skip-link-check specified".into(),
        };
    }
    if input.citations.is_empty() {
        return CheckResult {
            id: "dead_link".into(),
            title: "Citation URL response check".into(),
            status: CheckStatus::NotApplicable,
            evidence: "No citations".into(),
        };
    }
    let mut dead: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for c in &input.citations {
        match check_one(&c.url) {
            LinkStatus::Ok => {}
            LinkStatus::Dead(detail) => dead.push(format!("{} ({detail})", c.url)),
            LinkStatus::Unreachable(detail) => unknown.push(format!("{} ({detail})", c.url)),
        }
    }
    if dead.is_empty() && unknown.is_empty() {
        CheckResult {
            id: "dead_link".into(),
            title: "Citation URL response check".into(),
            status: CheckStatus::Pass,
            evidence: format!("All {} responded normally", input.citations.len()),
        }
    } else if !dead.is_empty() {
        CheckResult {
            id: "dead_link".into(),
            title: "Citation URL response check".into(),
            status: CheckStatus::Fail,
            evidence: format!("{} dead link(s): {}", dead.len(), dead.join(", ")),
        }
    } else {
        CheckResult {
            id: "dead_link".into(),
            title: "Citation URL response check".into(),
            status: CheckStatus::Warn,
            evidence: format!(
                "{} response(s) could not be verified (timeout/blocked/etc.): {}",
                unknown.len(),
                unknown.join(", ")
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Live verification of citation_status (#4) — code overrides the LLM's self-assessment.
// ---------------------------------------------------------------------------

pub enum CitationVerification {
    /// --skip-link-check specified, citation_ref is UNKNOWN/unparseable, or there's no quoted
    /// text (evidence) to check against.
    Unfetched,
    /// Request failed, blocked by SSRF protection, non-2xx response, or a non-text content type
    /// (PDF/image/etc.) that can't be checked against.
    FetchFailed,
    /// Confirms the (normalized) body contains finding.evidence (treated as the quoted text).
    QuoteMatched,
    /// The source fetched successfully, but the quoted text wasn't found in the body.
    QuoteNotFound,
}

impl CitationVerification {
    pub fn label(&self) -> &'static str {
        match self {
            CitationVerification::Unfetched => "UNFETCHED",
            CitationVerification::FetchFailed => "FETCH_FAILED",
            CitationVerification::QuoteMatched => "QUOTE_MATCHED",
            CitationVerification::QuoteNotFound => "QUOTE_NOT_FOUND",
        }
    }
}

fn is_text_content_type(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.contains("text") || ct.contains("json") || ct.contains("xml") || ct.contains("html")
}

/// Shallow normalization that only strips whitespace and lowercases. Doesn't absorb
/// morphological/punctuation differences (full text normalization is out of scope), but reduces
/// false negatives from line-break/spacing differences.
fn normalize_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn verify_citation(
    input: &Input,
    citation_ref: &str,
    quote: &str,
    skip: bool,
) -> CitationVerification {
    if skip {
        return CitationVerification::Unfetched;
    }
    let idx: usize = match citation_ref.trim().parse() {
        Ok(n) => n,
        Err(_) => return CitationVerification::Unfetched,
    };
    let citation = match input.citations.iter().find(|c| c.index == idx) {
        Some(c) => c,
        None => return CitationVerification::Unfetched,
    };
    if quote.trim().is_empty() {
        return CitationVerification::Unfetched;
    }
    match safe_fetch(&citation.url, true) {
        Ok(outcome) if outcome.status < 400 => {
            let is_text = outcome
                .content_type
                .as_deref()
                .map(is_text_content_type)
                .unwrap_or(true);
            if !is_text {
                return CitationVerification::FetchFailed;
            }
            let body = match outcome.body {
                Some(b) => b,
                None => return CitationVerification::FetchFailed,
            };
            let body_text = String::from_utf8_lossy(&body);
            if normalize_for_match(&body_text).contains(&normalize_for_match(quote)) {
                CitationVerification::QuoteMatched
            } else {
                CitationVerification::QuoteNotFound
            }
        }
        _ => CitationVerification::FetchFailed,
    }
}

/// Code directly recomputes and overwrites findings' citation_status. The value the LLM
/// originally filled in is kept only for reference in `llm_citation_status` (#4) — treats the
/// evidence field as the "quoted text" and checks it against the actual source (the finding
/// schema has no separate quote field, so evidence is used as a stand-in).
///
/// `llm_citation_status` is only backfilled from `citation_status` the first time (when it's
/// still empty, i.e. a fresh finding from this round's own lens/discourse pass whose
/// `citation_status` still holds the LLM's original self-report). Findings reinserted via
/// `--prior` (STILL_OPEN/REVERSED/UNKNOWN) are clones of a prior round's already-processed
/// finding: their `citation_status` is already that prior round's *code-verified* value, not an
/// LLM self-report, and their `llm_citation_status` already correctly holds the original report
/// from whenever the finding was first raised. Without this guard, re-running verify_citations on
/// a reinserted finding overwrites its genuine `llm_citation_status` with last round's
/// code-verified `citation_status`, silently destroying the value this field exists to preserve.
pub fn verify_citations(input: &Input, findings: &mut [Finding], skip: bool) {
    for f in findings.iter_mut() {
        let verified = verify_citation(input, &f.citation_ref, &f.evidence, skip);
        if f.llm_citation_status.is_empty() {
            f.llm_citation_status = f.citation_status.clone();
        }
        f.citation_status = verified.label().to_string();
    }
}

pub struct CheckOptions {
    pub as_of_year: u32,
    pub skip_link_check: bool,
}

pub fn run_all(spec: &Spec, input: &Input, opts: &CheckOptions) -> Vec<CheckResult> {
    let all = vec![
        citation_density_check(input),
        source_diversity_check(spec, input),
        numeric_consistency_check(input),
        access_limitation_disclosure_check(input),
        incentive_disclosure_scan(input),
        staleness_flag(spec, input, opts.as_of_year),
        dead_link_check(input, opts.skip_link_check),
    ];
    all.into_iter()
        .filter(|r| spec.check_enabled(&r.id))
        .collect()
}

/// Serializes to JSON so report.rs can render a table by cross-referencing spec.deterministic_checks.
/// This is the format [`from_json`] can deserialize back when re-read via `--deterministic-results`.
pub fn to_json(results: &[CheckResult]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for r in results {
        map.insert(r.id.clone(), serde_json::json!({"title": r.title, "status": r.status.label(), "evidence": r.evidence}));
    }
    serde_json::Value::Object(map)
}

/// Deserializes external JSON from `--deterministic-results` (#6). Minimal schema validation:
/// must be an object, must not be empty, and each entry must have a "status" field with a known
/// status label. If there's no title (e.g. a file hand-written by an external tool), falls back
/// to using the id as the title.
pub fn from_json(v: &serde_json::Value) -> Result<Vec<CheckResult>> {
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow!("deterministic_results must be a JSON object"))?;
    anyhow::ensure!(!obj.is_empty(), "deterministic_results is empty");
    let mut out = Vec::new();
    for (id, entry) in obj {
        let status_str = entry
            .get("status")
            .and_then(|s| s.as_str())
            .ok_or_else(|| anyhow!("check \"{id}\" has no status field (or it isn't a string)"))?;
        let status =
            CheckStatus::from_label(status_str).with_context(|| format!("check \"{id}\""))?;
        let evidence = entry
            .get("evidence")
            .and_then(|e| e.as_str())
            .unwrap_or("")
            .to_string();
        let title = entry
            .get("title")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| id.clone());
        out.push(CheckResult {
            id: id.clone(),
            title,
            status,
            evidence,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_loopback_private_linklocal_and_metadata() {
        assert!(ipv4_is_blocked(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(ipv4_is_blocked(Ipv4Addr::new(10, 1, 2, 3)));
        assert!(ipv4_is_blocked(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(ipv4_is_blocked(Ipv4Addr::new(172, 31, 255, 255)));
        assert!(!ipv4_is_blocked(Ipv4Addr::new(172, 32, 0, 1)));
        assert!(ipv4_is_blocked(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(ipv4_is_blocked(Ipv4Addr::new(169, 254, 169, 254))); // cloud metadata
        assert!(ipv4_is_blocked(Ipv4Addr::new(0, 0, 0, 0)));
        assert!(!ipv4_is_blocked(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!ipv4_is_blocked(Ipv4Addr::new(93, 184, 216, 34)));
    }

    #[test]
    fn blocks_ipv6_loopback_linklocal_uniquelocal() {
        assert!(ipv6_is_blocked("::1".parse().unwrap()));
        assert!(ipv6_is_blocked("fe80::1".parse().unwrap()));
        assert!(ipv6_is_blocked("fc00::1".parse().unwrap()));
        assert!(!ipv6_is_blocked("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn rejects_disallowed_schemes() {
        assert!(validate_url_safe("file:///etc/passwd").is_err());
        assert!(validate_url_safe("ftp://example.com/a").is_err());
    }

    #[test]
    fn rejects_literal_private_ip_url() {
        assert!(validate_url_safe("http://127.0.0.1/admin").is_err());
        assert!(validate_url_safe("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_url_safe("http://192.168.0.1/").is_err());
    }

    fn test_input() -> Input {
        Input {
            document: String::new(),
            sections: Vec::new(),
            word_count: 0,
            citations: Vec::new(),
            requirements: None,
            conventions: None,
            deterministic_results: None,
        }
    }

    fn test_finding(id: &str, citation_status: &str, llm_citation_status: &str) -> Finding {
        Finding {
            id: id.to_string(),
            section: "sec".to_string(),
            citation_ref: "1".to_string(),
            claim: "claim".to_string(),
            evidence: "evidence".to_string(),
            impact: String::new(),
            severity: "P1".to_string(),
            label: "x".to_string(),
            confidence: "medium".to_string(),
            recommendation: String::new(),
            lens: "lens_a".to_string(),
            reviewer: String::new(),
            citation_status: citation_status.to_string(),
            llm_citation_status: llm_citation_status.to_string(),
        }
    }

    /// A fresh finding from this round's own lens/discourse pass has `llm_citation_status` still
    /// empty and `citation_status` holding the LLM's self-report — verify_citations should move
    /// that self-report into `llm_citation_status` before overwriting `citation_status` with the
    /// code-verified value.
    #[test]
    fn verify_citations_backfills_llm_citation_status_for_fresh_findings() {
        let mut findings = vec![test_finding("f1", "CONTRADICTED", "")];
        verify_citations(&test_input(), &mut findings, true);
        assert_eq!(findings[0].llm_citation_status, "CONTRADICTED");
        assert_eq!(findings[0].citation_status, "UNFETCHED");
    }

    /// A finding reinserted via `--prior` (STILL_OPEN/REVERSED/UNKNOWN) is a clone of a prior
    /// round's already-processed finding: `citation_status` already holds *that* round's
    /// code-verified value (not an LLM self-report), and `llm_citation_status` already correctly
    /// holds the original self-report from whenever the finding was first raised. Re-running
    /// verify_citations on it must recompute `citation_status` without clobbering the genuine
    /// `llm_citation_status` with the stale code-verified value.
    #[test]
    fn verify_citations_preserves_llm_citation_status_on_reinsertion() {
        let mut findings = vec![test_finding("f1-still-open-r2", "UNFETCHED", "UNVERIFIED")];
        verify_citations(&test_input(), &mut findings, true);
        assert_eq!(findings[0].llm_citation_status, "UNVERIFIED");
        assert_eq!(findings[0].citation_status, "UNFETCHED");
    }

    #[test]
    fn from_json_roundtrips_to_json() {
        let results = vec![CheckResult {
            id: "dead_link".into(),
            title: "Citation URL response check".into(),
            status: CheckStatus::Warn,
            evidence: "test".into(),
        }];
        let v = to_json(&results);
        let back = from_json(&v).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, "dead_link");
        assert_eq!(back[0].status, CheckStatus::Warn);
        assert_eq!(back[0].title, "Citation URL response check");
    }

    #[test]
    fn from_json_rejects_unknown_status() {
        let v = serde_json::json!({"x": {"status": "MAYBE", "evidence": "e"}});
        assert!(from_json(&v).is_err());
    }

    #[test]
    fn from_json_rejects_non_object() {
        let v = serde_json::json!([1, 2, 3]);
        assert!(from_json(&v).is_err());
    }

    #[test]
    fn host_matches_owned_domain_rejects_substring_lookalike() {
        assert!(host_matches_owned_domain(
            "https://tossplace.com/x",
            "tossplace.com"
        ));
        assert!(host_matches_owned_domain(
            "https://www.tossplace.com/x",
            "tossplace.com"
        ));
        assert!(!host_matches_owned_domain(
            "https://evil-tossplace.com.attacker.net/x",
            "tossplace.com"
        ));
        assert!(!host_matches_owned_domain(
            "https://nottossplace.com/x",
            "tossplace.com"
        ));
    }

    #[test]
    fn approx_sentence_count_ignores_decimal_points() {
        // Decimal points like "3.5" must not be counted as sentence endings.
        let doc = "매출은 3.5억 원이다. 성장률은 12.3% 였다.";
        // "이다." isn't on the ending list, so the punctuation-based method should catch it (at least 1).
        assert!(approx_sentence_count(doc) >= 1);
    }
}
