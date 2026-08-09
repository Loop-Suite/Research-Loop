use crate::lens::Finding;
use crate::llm::Llm;
use crate::par_map;
use crate::spec::Spec;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::sync::OnceLock;

/// docs/design-spec.md §4: CHALLENGE is only recognized as valid when it "re-measures the same
/// metric with a different methodology/independent source and surfaces a numeric or claim
/// discrepancy" (narrower than the original codereview-loop rule of "rebuttal via evidence,
/// counter-example, or scope"). The prompt explicitly demotes unsupported taste-based rebuttals
/// (e.g. "this seems outdated") to SURFACE.
///
/// #2: Previously, if a round produced zero CHALLENGE moves, the code would automatically
/// re-invoke the same prompt — removed because it risked the model fabricating a rebuttal just
/// to match the expected shape, even in situations where having no counter-example is normal.
/// Zero CHALLENGE moves is now treated as a normal outcome (the prompt below also drops the
/// "at least one" requirement).
///
/// #1: Moves and resolutions are split into separate LLM calls — now an independent critic call
/// per lens — fully resolving #1. The moves stage itself is split into "one independent call per
/// participating lens": each lens never sees the findings it authored itself and only reviews
/// findings from other lenses (preventing a lens from judging its own finding). Per-lens calls
/// run in parallel without knowledge of each other's results (par_map), and a single resolutions
/// call makes the final adjudication over the collected moves. See [`run_round_call`].
pub const DISCOURSE_MOVES_SYSTEM: &str = "You are the critic reviewing findings submitted by multiple analysts. \
This call does not make the final verdict (CONFIRMED/REJECTED/MERGED/UNCERTAIN) — that is handled by a separate adjudicator call. \
Do not produce empty agreement or rebuttal with no substance. Use AGREE only when there is new citation/evidence. \
Recognize CHALLENGE only when it 're-measures the same metric with a different methodology or an independent source and surfaces a numeric or claim discrepancy' — \
unsupported taste-based rebuttals like 'this seems outdated' or 'the tone is off' should be raised as SURFACE, not CHALLENGE. \
It is normal for a round to have zero CHALLENGE moves — if there is no basis for a rebuttal, do not fabricate one just to match the expected shape. \
AGREE/CHALLENGE must always specify a confidence (high|medium|low) reflecting the strength of the claim. \
Respond strictly in the specified JSON schema only.";

/// System prompt for the adjudication-only call (#1). This call does not create new moves; it
/// determines CONFIRMED/REJECTED/MERGED/UNCERTAIN using only the evidence the moves call has
/// already produced — this at least minimally separates the structure of "judging your own
/// rebuttal."
pub const DISCOURSE_ADJUDICATE_SYSTEM: &str = "You are the adjudicator who makes only the final verdict for each finding, \
based on the moves (AGREE/CHALLENGE/CONNECT/SURFACE) other analysts have already raised. Do not create new moves, and do not \
fabricate evidence that is not in the given moves list. If the moves do not support a verdict, leave it as UNCERTAIN. \
Respond strictly in the specified JSON schema only.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Move {
    #[serde(rename = "move")]
    pub kind: String, // AGREE|CHALLENGE|CONNECT|SURFACE
    pub lens: String,
    pub target: String,
    pub detail: String,
    #[serde(default)]
    pub new_evidence: String,
    #[serde(default)]
    pub confidence: String, // high|medium|low (meaningful only for AGREE/CHALLENGE)
}

fn equal_confidence_bucket() -> ConfidenceBucket {
    ConfidenceBucket {
        high: Some(1.0),
        medium: Some(1.0),
        low: Some(1.0),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfidenceBucket {
    #[serde(default)]
    high: Option<f64>,
    #[serde(default)]
    medium: Option<f64>,
    #[serde(default)]
    low: Option<f64>,
}

impl ConfidenceBucket {
    fn value(&self, confidence: &str) -> Option<f64> {
        match confidence {
            "high" => self.high,
            "medium" => self.medium,
            "low" => self.low,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfidenceCalibration {
    #[serde(default = "equal_confidence_bucket")]
    default: ConfidenceBucket,
    #[serde(default)]
    by_lens: HashMap<String, ConfidenceBucket>,
    #[serde(default)]
    by_model: HashMap<String, ConfidenceBucket>,
    #[serde(default)]
    by_error_type: HashMap<String, ConfidenceBucket>,
    #[serde(default)]
    by_lens_model: HashMap<String, ConfidenceBucket>,
    #[serde(default)]
    by_lens_error_type: HashMap<String, ConfidenceBucket>,
    #[serde(default)]
    by_model_error_type: HashMap<String, ConfidenceBucket>,
    #[serde(default)]
    by_lens_model_error_type: HashMap<String, ConfidenceBucket>,
}

impl Default for ConfidenceCalibration {
    fn default() -> Self {
        Self {
            default: equal_confidence_bucket(),
            by_lens: HashMap::new(),
            by_model: HashMap::new(),
            by_error_type: HashMap::new(),
            by_lens_model: HashMap::new(),
            by_lens_error_type: HashMap::new(),
            by_model_error_type: HashMap::new(),
            by_lens_model_error_type: HashMap::new(),
        }
    }
}

impl ConfidenceCalibration {
    fn weighted(&self, confidence: &str, lens: &str, model: &str, error_type: &str) -> f64 {
        let confidence = confidence.to_ascii_lowercase();
        let lens = lens.trim();
        let model = model.trim();
        let error_type = error_type.trim();
        let key_lens_model_error = format!("{lens}|{model}|{error_type}");
        let key_lens_model = format!("{lens}|{model}");
        let key_model_error = format!("{model}|{error_type}");
        let key_lens_error = format!("{lens}|{error_type}");

        let bucket = self
            .by_lens_model_error_type
            .get(&key_lens_model_error)
            .or_else(|| self.by_lens_model.get(&key_lens_model))
            .or_else(|| self.by_model_error_type.get(&key_model_error))
            .or_else(|| self.by_lens_error_type.get(&key_lens_error))
            .or_else(|| self.by_error_type.get(error_type))
            .or_else(|| self.by_lens.get(lens))
            .or_else(|| self.by_model.get(model))
            .unwrap_or(&self.default);

        bucket.value(&confidence).unwrap_or(1.0)
    }
}

fn confidence_calibration() -> &'static ConfidenceCalibration {
    static CALIBRATION: OnceLock<ConfidenceCalibration> = OnceLock::new();
    CALIBRATION.get_or_init(load_confidence_calibration)
}

fn load_confidence_calibration() -> ConfidenceCalibration {
    let path = match env::var("RESEARCH_DISCOURSE_CONFIDENCE_CALIBRATION_PATH") {
        Ok(path) => path,
        Err(_) => return ConfidenceCalibration::default(),
    };
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return ConfidenceCalibration::default(),
    };
    serde_json::from_str(&content).unwrap_or_default()
}

/// ReConcile-style confidence bucket → weight. Instead of discarding residual UNCERTAIN
/// findings without a verdict once rounds are exhausted, make the final verdict from
/// accumulated AGREE/CHALLENGE weights.
///
/// Default weights are all 1.0 (equal weighting); specifying a JSON calibration file via the
/// `RESEARCH_DISCOURSE_CONFIDENCE_CALIBRATION_PATH` environment variable applies weights based
/// on lens/model/error-type combinations.
fn confidence_weight(confidence: &str, lens: &str, model: &str, error_type: &str) -> f64 {
    confidence_calibration().weighted(confidence, lens, model, error_type)
}

const VOTE_THRESHOLD: f64 = 0.6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    pub finding_id: String,
    pub status: String, // CONFIRMED|REJECTED|MERGED|UNCERTAIN
    #[serde(default)]
    pub merged_into: String,
    #[serde(default)]
    pub reason: String,
    /// #7: Set on findings where the fix check in a --prior re-examination judged the result
    /// UNKNOWN (unable to verify) or REVERSED (flipped back). This flag lets report.rs/quantify.rs
    /// explicitly surface that a human must verify manually, instead of silently clearing UNKNOWN
    /// as if it were FIXED.
    #[serde(default)]
    pub needs_human_review: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DiscourseRound {
    #[serde(default)]
    moves: Vec<Move>,
    #[serde(default)]
    resolutions: Vec<Resolution>,
    #[serde(default)]
    surfaced: Vec<Finding>,
}

pub struct DiscourseAudit {
    pub round: usize,
    pub moves: Vec<Move>,
}

/// lens/reviewer is intentionally not exposed — knowing which persona raised a finding could
/// tilt discourse toward "authority" rather than evidence (grounded in collusion/bias research,
/// inherited from codereview-loop).
fn findings_catalog(findings: &[Finding], resolved: &HashMap<String, Resolution>) -> String {
    findings
        .iter()
        .map(|f| {
            let status = resolved
                .get(&f.id)
                .map(|r| r.status.as_str())
                .unwrap_or("UNRESOLVED");
            format!(
                "- id={} | section={} | citation={} | severity={} | label={} | citation_status={} | status={}\n  claim: {}\n  evidence: {}",
                f.id, f.section, f.citation_ref, f.severity, f.label, f.citation_status, status, f.claim, f.evidence
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Collects, in order of first appearance and without duplicates, the list of lenses that own
/// this round's review targets (unresolved or UNCERTAIN from a previous round) (#1) — the
/// participant roster used to create a separate critic call per lens.
fn participating_lenses(
    findings: &[Finding],
    resolved: &HashMap<String, Resolution>,
) -> Vec<String> {
    let mut lenses: Vec<String> = Vec::new();
    for f in findings {
        let reviewable = resolved
            .get(&f.id)
            .map(|r| r.status == "UNCERTAIN")
            .unwrap_or(true);
        if reviewable && !f.lens.is_empty() && !lenses.contains(&f.lens) {
            lenses.push(f.lens.clone());
        }
    }
    lenses
}

/// Builds the stage-1 (moves) prompt dedicated to a single lens (`acting_lens`) (fully resolves #1).
/// `other_findings` is the list the caller (`run_lens_critic_call`) has already filtered to
/// exclude findings owned by `acting_lens` — this function only catalogs that result as-is and
/// never mixes the lens's own findings back in.
fn build_moves_prompt_for_lens(
    spec: &Spec,
    other_findings: &[Finding],
    resolved: &HashMap<String, Resolution>,
    round: usize,
    acting_lens: &str,
) -> String {
    let persona = spec
        .lens_by_id(acting_lens)
        .map(|l| format!("{}({})", l.title, l.persona_voice))
        .unwrap_or_else(|| acting_lens.to_string());
    format!(
        "# Task\nPerform stage 1 (moves) of round {round} discourse as an independent critic call per lens. \
         You are the critic for lens '{acting_lens}' — the {persona} perspective. The list below contains only \
         findings from 'other lenses', already excluding anything your own lens authored — excluded from the \
         start so that judging your own finding is structurally impossible. This stage does not make a verdict \
         (CONFIRMED/REJECTED, etc.) — the verdict is made by a separate adjudicator call after all lenses' moves \
         are collected.\n\n\
         ## Findings under review (other lenses; unresolved status only are new move targets)\n{catalog}\n\n\
         ## Rules\n\
         - Each move is one of AGREE/CHALLENGE/CONNECT/SURFACE; specify a finding id in target. Fill the lens field with '{acting_lens}'.\n\
         - AGREE: only when there is new evidence (new_evidence) not present in the target finding, reconfirming the same figure/claim from an independent source. confidence required.\n\
         - CHALLENGE: recognize only a discrepancy re-measured via a different methodology/different source (no taste-based rebuttals). confidence required. If none exists, leave it out — not mandatory.\n\
         - CONNECT: specify two or more finding ids in detail, connecting findings across different lenses (e.g. a financial finding ↔ an incentive finding).\n\
         - SURFACE: add a new finding to the surfaced array along with evidence. Unsupported rebuttals also go here.\n\
         - confidence applies only to AGREE/CHALLENGE: high if the claim's evidentiary strength is strong, medium if moderate, low if weak.\n\
         - Do not produce empty agreement/rebuttal with no substance.\n\n\
         ## Output (JSON only, no code fences)\n\
         {{\"moves\":[{{\"move\":\"AGREE|CHALLENGE|CONNECT|SURFACE\",\"lens\":\"{acting_lens}\",\"target\":\"finding id\",\
         \"detail\":\"...\",\"new_evidence\":\"...\",\"confidence\":\"high|medium|low\"}}],\
         \"surfaced\":[{{\"section\":\"...\",\"citation_ref\":\"...\",\"claim\":\"...\",\"evidence\":\"...\",\"impact\":\"...\",\
         \"severity\":\"P0|P1|P2|P3\",\"label\":<one of the allowed values>,\"confidence\":\"high|medium|low\",\"recommendation\":\"...\",\
         \"citation_status\":\"VERIFIED|UNVERIFIED|STALE|CONTRADICTED\"}}]}}\n",
        round = round,
        acting_lens = acting_lens,
        persona = persona,
        catalog = findings_catalog(other_findings, resolved),
    )
}

fn moves_catalog(moves: &[Move]) -> String {
    if moves.is_empty() {
        return "(no moves this round — zero CHALLENGE is normal)".to_string();
    }
    moves
        .iter()
        .map(|m| {
            format!(
                "- [{}] lens={} target={} confidence={} — {} (new_evidence: {})",
                m.kind, m.lens, m.target, m.confidence, m.detail, m.new_evidence
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Stage-2 (adjudication) prompt — takes as input only the moves already finalized in stage 1.
/// This call has no authority to create new moves; it only determines
/// CONFIRMED/REJECTED/MERGED/UNCERTAIN based on the given moves (#1).
fn build_resolutions_prompt(
    findings: &[Finding],
    resolved: &HashMap<String, Resolution>,
    round: usize,
    moves: &[Move],
) -> String {
    format!(
        "# Task\nPerform stage 2 (adjudication) of round {round} discourse. Below are the moves other analysts \
         have already raised this round — you do not create new moves; you make the final verdict for each \
         finding using only these moves as evidence.\n\n\
         ## All findings (unresolved status only are new verdict targets)\n{catalog}\n\n\
         ## This round's moves (verdict evidence, already finalized — not editable)\n{moves}\n\n\
         ## Rules\n\
         - resolutions only judges findings that were UNRESOLVED or UNCERTAIN in a previous round: CONFIRMED|REJECTED|MERGED|UNCERTAIN.\n\
         - Do not make a verdict the moves do not support — leave it as UNCERTAIN if evidence is insufficient.\n\n\
         ## Output (JSON only, no code fences)\n\
         {{\"resolutions\":[{{\"finding_id\":\"...\",\"status\":\"CONFIRMED|REJECTED|MERGED|UNCERTAIN\",\
         \"merged_into\":\"\",\"reason\":\"...\"}}]}}\n",
        round = round,
        catalog = findings_catalog(findings, resolved),
        moves = moves_catalog(moves),
    )
}

/// Iterates discourse rounds. Terminates once no unresolved/UNCERTAIN findings remain or
/// max_rounds is reached.
///
/// #2: Previously, a round with zero CHALLENGE moves was automatically re-requested by the code
/// — removed because it risked the model fabricating a rebuttal just to match the expected
/// shape, even though having no counter-example is normal. Zero CHALLENGE is now accepted as-is
/// (no re-call).
///
/// #1: [`run_round_call`] splits moves into independent critic calls per lens, and resolutions
/// are made in a single separate adjudicator call after collecting all those results — this
/// function itself just receives and aggregates that result, so the logic is unchanged from
/// before. `concurrency` is used to run per-lens critic calls in parallel (main.rs `par_map`,
/// reusing the same infrastructure as the lens review stage).
pub fn run(
    llm: &Llm,
    spec: &Spec,
    findings: &mut Vec<Finding>,
    max_rounds: usize,
    concurrency: usize,
) -> Result<(Vec<DiscourseAudit>, HashMap<String, Resolution>)> {
    let max_rounds = max_rounds.max(1);
    let mut resolved: HashMap<String, Resolution> = HashMap::new();
    let mut audit: Vec<DiscourseAudit> = Vec::new();

    for round in 1..=max_rounds {
        let unresolved = findings.iter().any(|f| {
            resolved
                .get(&f.id)
                .map(|r| r.status == "UNCERTAIN")
                .unwrap_or(true)
        });
        if !unresolved {
            break;
        }

        let mut dr = run_round_call(llm, spec, findings, &resolved, round, concurrency)?;

        for (i, sf) in dr.surfaced.iter_mut().enumerate() {
            sf.id = format!("surface-r{}-{}", round, i + 1);
            if sf.lens.is_empty() {
                sf.lens = "discourse".to_string();
            }
            if sf.citation_ref.trim().is_empty() {
                sf.citation_ref = "UNKNOWN".to_string();
            }
        }
        findings.extend(dr.surfaced.clone());

        for r in dr.resolutions.clone() {
            resolved.insert(r.finding_id.clone(), r);
        }

        audit.push(DiscourseAudit {
            round,
            moves: dr.moves,
        });

        if round == max_rounds {
            break;
        }
    }

    let model = llm.model.as_deref().unwrap_or("unknown");
    let finding_by_id: HashMap<String, &Finding> =
        findings.iter().map(|f| (f.id.clone(), f)).collect();

    // Remaining UNCERTAIN/unjudged findings after rounds are exhausted: make the final verdict via confidence-weighted vote.
    for f in findings.iter() {
        let still_uncertain = resolved
            .get(&f.id)
            .map(|r| r.status == "UNCERTAIN")
            .unwrap_or(true);
        if !still_uncertain {
            continue;
        }

        let net: f64 = audit
            .iter()
            .flat_map(|a| a.moves.iter())
            .filter(|m| m.target == f.id)
            .filter_map(|m| {
                let target = finding_by_id.get(&m.target)?;
                let weight = confidence_weight(&m.confidence, &target.lens, model, &target.label);
                match m.kind.as_str() {
                    "AGREE" => Some(weight),
                    "CHALLENGE" => Some(-weight),
                    _ => Some(0.0),
                }
            })
            .sum();

        let (status, reason) = if net >= VOTE_THRESHOLD {
            ("CONFIRMED".to_string(), format!("discourse rounds exhausted, confirmed via confidence-weighted vote (net={net:.2})"))
        } else if net <= -VOTE_THRESHOLD {
            ("REJECTED".to_string(), format!("discourse rounds exhausted, rejected via confidence-weighted vote (net={net:.2})"))
        } else {
            (
                "UNCERTAIN".to_string(),
                format!("discourse rounds exhausted, no verdict reached (net={net:.2})"),
            )
        };

        resolved.insert(
            f.id.clone(),
            Resolution {
                finding_id: f.id.clone(),
                status,
                merged_into: String::new(),
                reason,
                needs_human_review: false,
            },
        );
    }

    Ok((audit, resolved))
}

#[derive(Debug, Clone, Default, Deserialize)]
struct MovesRound {
    #[serde(default)]
    moves: Vec<Move>,
    #[serde(default)]
    surfaced: Vec<Finding>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ResolutionsRound {
    #[serde(default)]
    resolutions: Vec<Resolution>,
}

/// Independent critic call per lens (#1) — `acting_lens` sees only findings from "other lenses",
/// excluding what it authored itself, and generates moves/surfaced from that. The resulting
/// move's lens field is always pinned to `acting_lens` (so attribution stays stable even if the
/// model fills the lens field incorrectly or leaves it blank).
fn run_lens_critic_call(
    llm: &Llm,
    spec: &Spec,
    findings: &[Finding],
    resolved: &HashMap<String, Resolution>,
    round: usize,
    acting_lens: &str,
) -> Result<MovesRound> {
    let others: Vec<Finding> = findings
        .iter()
        .filter(|f| f.lens != acting_lens)
        .cloned()
        .collect();
    let prompt = build_moves_prompt_for_lens(spec, &others, resolved, round, acting_lens);
    let mv = llm
        .json(&prompt, Some(DISCOURSE_MOVES_SYSTEM))
        .with_context(|| {
            format!("discourse round {round} lens '{acting_lens}' critic call failed")
        })?;
    let mut mr: MovesRound = serde_json::from_value(mv).with_context(|| {
        format!("discourse round {round} lens '{acting_lens}' moves JSON schema mismatch")
    })?;
    for m in mr.moves.iter_mut() {
        m.lens = acting_lens.to_string();
    }
    Ok(mr)
}

/// Executes a single round — now independent critic calls per lens: fully resolves #1.
/// 1) Groups this round's review-target findings by owning lens ([`participating_lenses`]) to
///    get the participating lenses.
/// 2) If fewer than 2 lenses participate (no comparison target), skips the critic stage entirely
///    (proceeds with no moves).
/// 3) Calls [`run_lens_critic_call`] independently for each participating lens, with no knowledge
///    of each other's results — since the number of calls scales with the number of lenses, runs
///    them in parallel up to `concurrency` (`par_map`, same infrastructure as the lens review stage).
/// 4) Collects moves from all lenses and makes the final verdict with a single
///    DISCOURSE_ADJUDICATE_SYSTEM adjudication call (existing logic unchanged, only the input
///    changes to the combined per-lens moves).
fn run_round_call(
    llm: &Llm,
    spec: &Spec,
    findings: &[Finding],
    resolved: &HashMap<String, Resolution>,
    round: usize,
    concurrency: usize,
) -> Result<DiscourseRound> {
    let lenses = participating_lenses(findings, resolved);

    let (all_moves, all_surfaced): (Vec<Move>, Vec<Finding>) = if lenses.len() < 2 {
        // If there are 0-1 lenses, there is no comparison target — skip the critic call entirely (that lens is skipped).
        (Vec::new(), Vec::new())
    } else {
        let results: Vec<MovesRound> = par_map(concurrency, lenses, |acting_lens| {
            run_lens_critic_call(llm, spec, findings, resolved, round, &acting_lens)
        })?;
        let mut moves = Vec::new();
        let mut surfaced = Vec::new();
        for mr in results {
            moves.extend(mr.moves);
            surfaced.extend(mr.surfaced);
        }
        (moves, surfaced)
    };

    let res_prompt = build_resolutions_prompt(findings, resolved, round, &all_moves);
    let rv = llm
        .json(&res_prompt, Some(DISCOURSE_ADJUDICATE_SYSTEM))
        .with_context(|| format!("discourse round {round} resolutions stage failed"))?;
    let rr: ResolutionsRound = serde_json::from_value(rv)
        .with_context(|| format!("discourse round {round} resolutions JSON schema mismatch"))?;

    Ok(DiscourseRound {
        moves: all_moves,
        resolutions: rr.resolutions,
        surfaced: all_surfaced,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Lens;

    fn finding(id: &str, lens: &str) -> Finding {
        Finding {
            id: id.to_string(),
            section: "sec".to_string(),
            citation_ref: "1".to_string(),
            claim: format!("claim-{id}"),
            evidence: format!("evidence-{id}"),
            impact: String::new(),
            severity: "P2".to_string(),
            label: "x".to_string(),
            confidence: "medium".to_string(),
            recommendation: String::new(),
            lens: lens.to_string(),
            reviewer: String::new(),
            citation_status: "UNVERIFIED".to_string(),
            llm_citation_status: String::new(),
        }
    }

    fn test_spec() -> Spec {
        Spec {
            name: "test".to_string(),
            context: String::new(),
            lenses: vec![
                Lens {
                    id: "lens_a".to_string(),
                    title: "Lens A".to_string(),
                    guide: String::new(),
                    always: false,
                    signal: String::new(),
                    persona_name: String::new(),
                    persona_voice: "Perspective A".to_string(),
                    tier: String::new(),
                },
                Lens {
                    id: "lens_b".to_string(),
                    title: "Lens B".to_string(),
                    guide: String::new(),
                    always: false,
                    signal: String::new(),
                    persona_name: String::new(),
                    persona_voice: "Perspective B".to_string(),
                    tier: String::new(),
                },
            ],
            labels: vec!["x".to_string()],
            subject_owned_domains: Vec::new(),
            staleness_threshold_years: 0,
            enabled_checks: Vec::new(),
        }
    }

    #[test]
    fn participating_lenses_dedupes_and_skips_resolved() {
        let findings = vec![
            finding("f1", "lens_a"),
            finding("f2", "lens_b"),
            finding("f3", "lens_a"),
        ];
        let mut resolved = HashMap::new();
        resolved.insert(
            "f3".to_string(),
            Resolution {
                finding_id: "f3".to_string(),
                status: "CONFIRMED".to_string(),
                merged_into: String::new(),
                reason: String::new(),
                needs_human_review: false,
            },
        );
        let lenses = participating_lenses(&findings, &resolved);
        assert_eq!(lenses, vec!["lens_a".to_string(), "lens_b".to_string()]);
    }

    #[test]
    fn participating_lenses_empty_when_all_resolved() {
        let findings = vec![finding("f1", "lens_a")];
        let mut resolved = HashMap::new();
        resolved.insert(
            "f1".to_string(),
            Resolution {
                finding_id: "f1".to_string(),
                status: "CONFIRMED".to_string(),
                merged_into: String::new(),
                reason: String::new(),
                needs_human_review: false,
            },
        );
        assert!(participating_lenses(&findings, &resolved).is_empty());
    }

    /// Core verification for #1: the prompt for lens A must contain only lens B's findings, and
    /// lens A's own findings (neither id nor claim) must never be exposed — the requirement that
    /// prevents a lens from judging its own finding.
    #[test]
    fn lens_prompt_excludes_own_findings_includes_others() {
        let findings = vec![finding("f-a1", "lens_a"), finding("f-b1", "lens_b")];
        let resolved = HashMap::new();
        let spec = test_spec();

        let others_for_a: Vec<Finding> = findings
            .iter()
            .filter(|f| f.lens != "lens_a")
            .cloned()
            .collect();
        let prompt_a = build_moves_prompt_for_lens(&spec, &others_for_a, &resolved, 1, "lens_a");
        assert!(
            prompt_a.contains("f-b1"),
            "lens A prompt should contain lens B's finding"
        );
        assert!(prompt_a.contains("claim-f-b1"));
        assert!(
            !prompt_a.contains("f-a1"),
            "lens A prompt should not contain its own (lens_a) finding id"
        );
        assert!(
            !prompt_a.contains("claim-f-a1"),
            "lens A prompt should not contain its own claim"
        );

        let others_for_b: Vec<Finding> = findings
            .iter()
            .filter(|f| f.lens != "lens_b")
            .cloned()
            .collect();
        let prompt_b = build_moves_prompt_for_lens(&spec, &others_for_b, &resolved, 1, "lens_b");
        assert!(
            prompt_b.contains("f-a1"),
            "lens B prompt should contain lens A's finding"
        );
        assert!(
            !prompt_b.contains("f-b1"),
            "lens B prompt should not contain its own (lens_b) finding id"
        );
    }

    #[test]
    fn single_lens_has_no_comparison_target() {
        // If there is only 1 lens (no comparison target), run_round_call never creates a critic call at all.
        let findings = vec![finding("f1", "lens_a"), finding("f2", "lens_a")];
        let resolved = HashMap::new();
        assert_eq!(
            participating_lenses(&findings, &resolved),
            vec!["lens_a".to_string()]
        );
        // Reproduces the lenses.len() < 2 branch exactly: must proceed with moves empty, without any critic call.
        let lenses = participating_lenses(&findings, &resolved);
        assert!(
            lenses.len() < 2,
            "a single lens has no comparison target, so the critic stage should be skipped"
        );
    }
}
