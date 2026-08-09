# research-loop Design Spec

research-loop ports Code-Review-Loop's (a Rust-based persona-review CLI) 12-stage pipeline and persona-discourse structure into the "web-research-based market/competitor research documentation" domain, with only the minimal changes that the domain makes unavoidable. As with marketing-loop, the original codereview implementation is treated as the primary reference, and every extension not present in the original is called out explicitly.

---

## 0. research-loop mapping against Code-Review-Loop's 12-stage pipeline

| Stage | Module | codereview original | research-loop substitution |
|---|---|---|---|
| Input normalization / convention injection | input.rs | diff + coding convention | Injects the research subject (`--topic`, e.g. "domestic café POS competitors") + existing document (if any) + tone/format guide. Normalizes the document into section_id units (e.g. `market-share`, `financials`, `org-culture`) — the counterpart to file:line, required for discourse evidence citation |
| Lens selection (3-5) | lens.rs::select_lenses | based on diff characteristics | based on `--research-type` |
| Deterministic vs semantic split | report.rs::deterministic_table | — | same structure retained |
| Policy checks / binary verdicts | policy.rs | coding policy | binary gates such as missing source attribution, undisclosed incentives, dead links |
| Per-lens independent review | lens.rs::review_lens | — | independent review per persona, same structure |
| Discourse debate | discourse.rs | AGREE/CHALLENGE/CONNECT/SURFACE | most rules ported as-is, CHALLENGE condition redefined (§4) |
| Requirement verification | requirements.rs | PR requirements | verifies coverage of the research brief (list of angles that must be covered) → produces `coverage_gaps` |
| Quantitative summarization | quantify.rs | P0=25/P1=12/P2=5/P3=1 | weight numbers kept as-is, only the severity definitions are redefined for the domain (§7) |
| Prior-run fix check (--prior) | fixcheck.rs + state.rs | FIXED/STILL_OPEN/UNKNOWN | logic retained. However, in the research domain, the case where "a prior conclusion is overturned by newer evidence" (e.g. the T-order/KT case) is extended into a separate new `REVERSED` state — a value absent from the original three states, see §6 |
| Human-voice rewrite | humanvoice.rs | — | not applicable (research documents are not about tone normalization) — **not applied, original stage skipped** (an explicit difference from marketing-loop) |
| Final report assembly | report.rs | — | same structure retained + `citation_status`/`source_diversity` fields added |

---

## 1. Research-domain personas (7)

Follows the marketing-loop convention as-is (real individuals; persona voice is built on that field's principles/writings).

| Lens | Persona (real) | Basis | Persona tone | Tier |
|---|---|---|---|---|
| market_dynamics | Michael Porter | Harvard Business School, *Competitive Strategy*, creator of the Five Forces framework | Structural, industry-analysis tone; repeatedly asks "is this a genuine structural advantage or a transient one?" | 1 |
| financial_forensics | Aswath Damodaran | NYU Stern, "Dean of Valuation," the "narrative and numbers must agree" principle | Skeptical, quantitative-first; flags immediately when narrative and numbers diverge; obsessive about checking primary sources (audited financials) | 1 |
| payments_regulatory_economics | Patrick McKenzie (patio11) | Author of the "Bits about Money" newsletter, writer specializing in payments-industry structure and regulatory economics | Practitioner tone dissecting fee structures; repeatedly asks "does this business model survive regulatory change?" | 1 |
| engineering_diligence | Gergely Orosz | "The Pragmatic Engineer" newsletter, an org-diagnosis methodology based on job postings and engineering blogs | Centers on measuring job postings/tech stack directly; skeptical of unsupported "we have the best engineering" claims | 1 |
| incentive_integrity | Cory Doctorow | Coined the "enshittification" concept, writes critiques of platform incentive distortion | Cynical, structure-critical tone; repeatedly asks "whose interest does this review serve?" | 1 |
| org_culture_signal | Adam Grant | Wharton organizational psychology, *Give and Take*, *Originals* | Data-driven interpretation of organizational behavior; repeatedly warns against over-trusting a single platform's ratings | 2 |
| closed_platform_ethnography | danah boyd | Microsoft Research/Data & Society, research methodology for closed and algorithmic platforms | Emphasizes methodological humility; always distinguishes "couldn't access" from "doesn't exist" | 2 |

> **Assumption:** As with marketing-loop, tier=1 (5 required lenses) / tier=2 (2 supplementary lenses) used here is a field newly defined for research-loop with a different meaning from the original spec.rs's `tier: String` (display-only, not involved in selection logic). To strictly follow the original, `tier` should remain a display label while whether-required-or-not should be managed via a separate `always: bool` (inherited from the marketing-loop DESIGN note).
>
> **Assumption:** The payments_regulatory_economics, incentive_integrity, and closed_platform_ethnography personas are not actual regulators or lawyers — they are an approximate mapping of each lens onto the philosophy of "dissecting payments economics," "critiquing platform incentives," and "research ethics for closed communities" respectively (the same kind of assumption as marketing-loop's claims_compliance not being an actual lawyer). Actual legal/regulatory determinations are handled by policy.rs's deterministic gates, not by personas.

### Lens selection by research-type (4-6 of the 7-lens pool)

| --research-type | Selected lenses |
|---|---|
| competitor_landscape | market_dynamics, financial_forensics, engineering_diligence, incentive_integrity |
| financial_diligence | financial_forensics, market_dynamics, payments_regulatory_economics |
| market_sizing | market_dynamics, payments_regulatory_economics, financial_forensics |
| org_and_culture | org_culture_signal, engineering_diligence, incentive_integrity |
| community_sentiment | incentive_integrity, closed_platform_ethnography, org_culture_signal |
| full_deep_dive | market_dynamics, financial_forensics, payments_regulatory_economics, engineering_diligence, incentive_integrity, org_culture_signal, closed_platform_ethnography (all 7) |

---

## 2. spec.toml example

```toml
[[lenses]]
persona_name = "Michael Porter"
persona_voice = "Structural, industry-analysis tone. Repeatedly asks 'is this a sustainable structural advantage or a transient promotional one?' Examines entry barriers, substitutes, and buyer bargaining power from a Five Forces perspective."
lens = "market_dynamics"
tier = 1

[[lenses]]
persona_name = "Aswath Damodaran"
persona_voice = "Skeptical, quantitative-first. Immediately flags where the narrative (marketing copy) diverges from the numbers (audited financials). Assigns low confidence to financial claims that lack primary-source verification."
lens = "financial_forensics"
tier = 1

[[lenses]]
persona_name = "Patrick McKenzie"
persona_voice = "Practitioner tone dissecting payments-industry structure and fee mechanics. Repeatedly asks 'does this business model survive regulatory change and fee cuts?'"
lens = "payments_regulatory_economics"
tier = 1

[[lenses]]
persona_name = "Gergely Orosz"
persona_voice = "Grounded in direct measurement of job postings and engineering blogs. Skeptical of unsupported 'best-in-class engineering' marketing claims; re-verifies org size and tech stack against primary sources."
lens = "engineering_diligence"
tier = 1

[[lenses]]
persona_name = "Cory Doctorow"
persona_voice = "Cynical, structure-critical. Repeatedly asks 'whose interest does this review serve?' Always checks first whether an incentive/review program exists."
lens = "incentive_integrity"
tier = 1

[[lenses]]
persona_name = "Adam Grant"
persona_voice = "Interprets organizational-psychology data. Warns against over-trusting a single review platform's rating; repeatedly flags sample size and respondent bias."
lens = "org_culture_signal"
tier = 2

[[lenses]]
persona_name = "danah boyd"
persona_voice = "Methodological humility. Always distinguishes 'couldn't access' from 'doesn't exist.' Pushes back on statements that assert closed-platform sentiment as fact."
lens = "closed_platform_ethnography"
tier = 2
```

> The meaning and limits of the tier values above are the same as the assumption note in §1.

---

## 3. deterministic_checks list

Follows marketing-loop's "separate deterministic checks from the LLM" principle as-is (same rationale as bizplan-loop DESIGN.md item 11). Each of the 6 failure modes observed in §2 (research survey) has been converted into an automatable check.

| check_id | Description | Failure mode addressed (survey §2) | Local tool/implementation |
|---|---|---|---|
| citation_density_check | ratio of source links to claim sentences | general | custom implementation (sentence splitting + link counting) |
| dead_link_check | checks the response code of cited URLs | general | linkinator (npm) — existing tool (reused as-is from marketing-loop) |
| source_diversity_check | distribution of source domains, especially the share belonging to the research subject's own domain | "subject-published content dominating search results" | custom implementation (domain extraction + tallying) |
| numeric_consistency_check | checks whether the figure for the same entity+metric pair (e.g. "T-order revenue") is consistent throughout the document | "the same metric's figures conflicting across passes" | custom implementation (regex-based entity-metric-value extraction, then comparison) |
| staleness_flag | computes the diff between the document's narration date and the cited article's publish date, warns if it exceeds a threshold | "a prior conclusion overturned by newer information" | custom implementation (date parsing + diff) |
| incentive_disclosure_scan | checks whether citations near "review event / affiliate / sponsorship / reward" keywords disclose the incentive | "credibility contamination from incentivized reviews" | custom implementation (keyword + nearby-context scan) |
| access_limitation_disclosure_check | checks whether "could not verify" and "does not exist" are actually described as distinct — cross-checks whether an access-attempt record (e.g. a WebFetch block log) exists | "inability to access a closed platform" | custom implementation (regex: presence of phrasing such as "could not verify" / "access denied" / "no basis to conclude") |
| readability_score | readability metrics such as Flesch-Kincaid | general | textstat (Python) — existing tool (reused as-is from marketing-loop) |
| duplicate_content_check | degree of identical-paragraph repetition across rounds (whether it was copy-pasted without re-researching) | general | simhash — existing tool (reused as-is from marketing-loop) |

**citation_status (citation-hallucination handling) structural difference:** Unlike marketing-loop's semgrep-equivalent scanner, whether "this URL actually supports this claim" cannot be fully automated deterministically (CITETRACER-style frameworks also end with an LLM verification stage — research survey §5). `citation_status` (VERIFIED/UNVERIFIED/STALE/CONTRADICTED) is therefore split out as a semantic concern that **a persona judges during the discourse round after checking the source text**, rather than as a deterministic_check.

---

## 4. discourse.rs porting decisions

| Original rule | Validity in the research domain | Decision |
|---|---|---|
| Strip reviewer identity, keep only id/file:line/claim/evidence | valid | Ported as-is. file:line → section_id:citation_index |
| AGREE is valid only when it cites new evidence not already in the finding | valid | Ported as-is. AGREE holds only when "the same figure/claim is reconfirmed from a different independent source" (e.g. both Seoul Economic Daily and Maeil Ilbo citing T-order's ₩41.9B revenue) |
| At least one CHALLENGE is mandatory per round; falling short triggers one automatic retry | conditionally valid — needs modification | **Quantitative-vs-qualitative metric disagreement** (survey §2 case: app-store rating vs. qualitative reviews, Jobplanet vs. Blind) is the core CHALLENGE trigger in the research domain. However, an unsupported rebuttal like "this feels outdated" does not qualify as CHALLENGE (downgraded to SURFACE) — **a valid CHALLENGE is restricted to cases that re-measure the same metric via a different methodology/source and raise a numeric discrepancy**. The same kind of calibration as marketing-loop's "distinguish taste-based rebuttal from evidence-based rebuttal" principle |
| CONNECT (relates to another lens's finding) | valid | Ported as-is. E.g. links the financial_forensics lens's loss finding ↔ the incentive_integrity lens's "free-distribution strategy" finding |
| SURFACE (raises a new issue) | valid | Ported as-is |

> **Assumption:** Narrowing the CHALLENGE condition to "only discrepancies re-measured via a different methodology/source count" is a design decision (no basis for it in the original README), and is the same kind of minimal calibration as marketing-loop's addition of a "taste vs. evidence" distinction. It is not an unvalidated extension — it is scoped strictly as a fix for a defect that a straight port would otherwise cause (unsupported nitpicking rebuttals forcing a mandatory retry every round).

---

## 5. CLI subcommand mapping

| Subcommand | codereview original | research-loop counterpart | 1:1? |
|---|---|---|---|
| review | diff/spec/requirements/conventions/deterministic-results → report.md+state.json | research document/spec/research brief (list of angles to cover, counterpart to requirements)/tone guide (counterpart to conventions)/deterministic-results → report.md+state.json | 1:1, only input names substituted |
| describe | PR summary: title/summary/walkthrough/labels/can_be_split/TODO scan | document summary: key findings/coverage gaps/staleness list/labels (research-type, research subject)/can_be_split (whether the section can be split)/TODO scan ([needs verification], "update later" markers) | 1:1 |
| improve | before/after patch proposal | proposes a revised section reflecting further research (before/after) | 1:1, only "patch" → "revised section" substituted |
| ask | free-form query, appended to ask.md | free-form query (e.g. "Did this company obtain PCI-DSS certification?"), appended to ask.md | 1:1, unchanged |

All 4 subcommands can be ported by substituting only the input domain, with no structural change. No new subcommand is added (the same minimalism principle as marketing-loop).

---

## 6. Output schema (report.md / state.json)

### report.md fields

- **verdict** (PASS/REVISE — same assumption as marketing-loop: the original's exact verdict formula isn't in the README, so it's inferred as a policy-fail override; not certain)
- **policy checks** (binary pass/fail list: dead links, undisclosed incentives, etc.)
- **findings** (persona/severity/section location/claim/evidence)
- **good things**
- **deterministic checks** (status/evidence per check_id)
- **discourse audit** (per-round AGREE/CHALLENGE/CONNECT/SURFACE log)
- **requirements verification** (whether the angles required by the research brief are covered → `coverage_gaps` list)
- **citation_status summary** (counts of VERIFIED/UNVERIFIED/STALE/CONTRADICTED — new to research-loop, a field absent from marketing-loop)
- **source_diversity summary** (ratio of independent sources vs. sources published by the research subject itself — new to research-loop)
- **compared to the prior round** (only when `--prior` is given: list of FIXED/STILL_OPEN/UNKNOWN/**REVERSED** (new, see §0))

> The human-voice review section is **not applied** (see §0) — since research documents are not about tone rewriting, that section from marketing-loop is dropped outright.

### state.json schema

> As with marketing-loop, this does not reuse the original state.rs's 3-field `State { round, findings, resolved }` structure as-is — it expands the fields needed to reconstruct the report. It is explicitly noted as a new design that only references the minimal-snapshot concept, not an "identical structure" to the original.

```json
{
  "run_id": "string",
  "research_type": "competitor_landscape|financial_diligence|market_sizing|org_and_culture|community_sentiment|full_deep_dive",
  "topic": "string",
  "timestamp": "ISO8601",
  "verdict": "PASS|REVISE",
  "score": 0,
  "policy_checks": [{"check_id": "string", "status": "PASS|FAIL", "evidence": "string"}],
  "deterministic_checks": {"check_id": {"status": "PASS|FAIL|WARN", "evidence": "string"}},
  "lens_selected": ["market_dynamics", "financial_forensics"],
  "findings": [{"id": "string", "lens": "string", "persona": "string", "severity": "P0|P1|P2|P3", "section_ref": "section_id:citation_index", "claim": "string", "evidence": "string", "citation_status": "VERIFIED|UNVERIFIED|STALE|CONTRADICTED", "status": "FIXED|STILL_OPEN|UNKNOWN|REVERSED"}],
  "discourse_log": [{"round": 0, "tag": "AGREE|CHALLENGE|CONNECT|SURFACE", "persona": "string", "target_finding_id": "string", "evidence": "string"}],
  "coverage_gaps": ["string"],
  "source_diversity": {"independent_sources": 0, "subject_owned_sources": 0, "ratio": 0.0},
  "good_things": ["string"],
  "prior_ref": "path|null"
}
```

### severity weights

quantify.rs's hardcoded values P0=25/P1=12/P2=5/P3=1 are kept as-is (same as marketing-loop, no extension). Only the severity definitions are reinterpreted per domain:

| Severity | Research-domain definition |
|---|---|
| P0 | factual error / numeric contamination (e.g. misreading an accounting-method change as business contraction, citing an incorrect financial figure without re-verification) — risk of the document's credibility collapsing |
| P1 | undisclosed source bias (citing self-published content as if it were neutral information, undisclosed incentivized reviews) |
| P2 | coverage gap (a required angle is missing), staleness not flagged |
| P3 | minor wording/formatting issues |

> **Assumption:** These severity definitions are a design decision; as with marketing-loop, the original code-domain's exact P0-P3 definitions aren't in the README and can't be confirmed — only the numeric weights are kept identical to the original.

## 7. Not yet done

- **Automated citation_status determination pipeline**: currently relies entirely on persona judgment during the discourse round. Adding a CITETRACER-style cascading verification (cache lookup → URL fetch → connector → web search) as a deterministic pre-filter could reduce the burden on the discourse round (see research survey §5, §Next steps) — not implemented.
- **calibration set** (a limitation of the same nature as the bizplan-loop DESIGN.md item): without a process to calibrate the rubric against samples of actual high- and low-quality research reports, the severity thresholds remain empirically unvalidated.
- **`--watch` mode**: continuous-tracking features like Klue/Crayon's real-time monitoring (Compete Agent) are out of scope for this design (static document generation).
