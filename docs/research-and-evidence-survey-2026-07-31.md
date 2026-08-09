# research-loop Research Survey

## 1. Overview

Code-Review-Loop's core structure is **① independent review per persona → ② anonymized cross-examination via discourse (mandatory CHALLENGE) → ③ a deterministic verdict**. To assess whether this structure can be ported to the "web-research-based market/competitor research documentation" domain (research-loop), this survey investigated (A) the GitHub ecosystem of market-research/competitive-intelligence (CI) automation skills, (B) the architecture of commercial CI platforms, and (C) adjacent research covering failure modes specific to research documents (citation hallucination, numeric inconsistency). marketing-loop's survey of discourse-adjacent domains (fact-checking/legal review/peer review) is domain-agnostic content and is inherited as-is; this document adds investigation of only the items specific to the research domain.

## 2. Motivation: failure modes observed in actual research work

This design is directly grounded in the following phenomena, repeatedly observed across the MangroveCafeOrder project's POS-competitor research (real work spanning many rounds).

| Phenomenon | Real-world case |
|---|---|
| Quantitative-vs-qualitative metric disagreement | PayHere's app-store rating (4.0 / 245 reviews) came out higher than its qualitative reviews (Clien, negative). A 0.8-point gap between Jobplanet (2.7) and Blind (3.5) |
| Subject-published content dominating search results | Searching "café POS recommendation" in Korean repeatedly surfaced Toss Place's own blog ("Owner Stories") at the top — without distinguishing it from independent sources, marketing copy gets mistaken for neutral information |
| Credibility contamination from incentivized reviews | Confirmed Toss Place was running a cash-reward program ("₩5,000 for writing a review + ₩50,000 per referred install") — a significant share of the positive reviews cited may have been written under financial incentive |
| The same metric's figures conflicting across passes | The count of POS terminals T-order integrates with ranged from "25" to "50+" depending on the source. T-order's revenue looked like it had shrunk (₩58.7B in 2023 vs. ₩41.9B in 2025), but this was actually caused by an accounting-method change, not the business itself shrinking — misread without re-verification |
| A prior conclusion overturned by newer information | The T-order/KT relationship was recorded as "acquisition talks rumored, unconfirmed," but re-investigation three months later found it had escalated into a public IP-theft dispute and restructuring — a research document must state the evidence date explicitly rather than assume "the latest pass is always best" |
| Inability to access a closed platform | Korea's largest self-employed-business community (a Naver café) is login-gated and not indexed by search engines — "could not verify" and "does not exist" must be recorded as distinct |

Each row of this table is reflected directly in §5 (porting discourse) and in the design spec's deterministic_checks and severity definitions.

## 3. GitHub ecosystem of market-research/competitive-intelligence (CI) automation skills

| Repo | Characteristics | discourse stage |
|---|---|---|
| ferdinandobons/startup-skill | startup validation, competitive intelligence, and planning AI agent skills | not confirmable |
| phuryn/pm-skills | PM Skills Marketplace, 100+ skills (market research: includes persona/segmentation/journey mapping/market sizing/competitive analysis) | not confirmable |
| Imbad0202/academic-research-skills | a Deep Research 13-agent research team, Socratic guided mode, PRISMA systematic review, Semantic Scholar API verification | partial — a cross-model double-verification (DA) option is mentioned, but it is not a mandatory CHALLENGE |

Conclusion: SKILL.md-format market-research skills exist, but a skill equipped with Code-Review-Loop-style mandatory discourse cross-verification is **not confirmable** (the same conclusion as the marketing-loop survey).

## 4. Commercial competitive-intelligence (CI) platform architecture

| Tool | Structure | Price range |
|---|---|---|
| Klue | web monitoring + AI curation + "Compete Agent" (real-time competitor-mention detection on sales calls) | ~$20K-40K/yr |
| Crayon | enterprise monitoring + battlecards + field-intelligence integration | ~$20K-40K/yr |
| Kompyte | centered on web/digital-tracking automation (since 2014, now part of Semrush) | ~$300/yr~ |

**Architecture observation**: all three vendors share a two-stage structure — "① data collection (web monitoring) → ② AI curation/summarization (a single layer)." There is no evidence anywhere supporting independent per-persona review or a cross-examination (discourse) structure — this matches exactly the conclusion the marketing-loop survey reached: "most commercial tools are a two-stage generate/curate pipeline, with no precedent for a three-stage discourse structure."

**Reference value as a research technique**: the evidence-gathering techniques mentioned in CI-industry practice (tracking innovation patterns via patent analysis, **inferring a competitor's target industries, growth priorities, and product direction via job-posting analysis**) match exactly the method actually used in this POS research (estimating tech stack and org size from Wanted/Rallit job postings) — confirmed as an industry-standard technique.

## 5. A failure mode specific to research documents: citation-hallucination detection

An item absent from marketing-loop's survey scope; investigated separately since it is a core risk for research-document automation.

- **"Source or It Didn't Happen" (CITETRACER, arXiv:2605.08583)** — redefines citation-hallucination detection as a 12-code taxonomy (REAL/POTENTIAL/HALLUCINATED) and proposes a multi-agent detector that extracts structured citations from PDF/BibTeX and then verifies evidence in a cascading order: cache lookup → URL fetch → scholar connector → web search. **Reuse point**: the approach of separating "unverified" from "confirmed false" into distinct grades — reflected directly in this design's `citation_status` field (VERIFIED/UNVERIFIED/STALE/CONTRADICTED).
- **"Detecting and Correcting Reference Hallucinations in Commercial LLMs and Deep Research Agents" (arXiv:2604.03173)** — empirically measures that reference hallucination occurs even in commercial deep-research agents. Evidence that research-document automation demands higher citation accuracy than general text generation.
- **Academic Paper Reviewer, a 7-agent framework** (multi-perspective peer review: EIC + 3 dynamic reviewers + Devil's Advocate, a concession-threshold protocol) — the combination of persona diversity plus a deliberate rebuttal role (Devil's Advocate) serves the same purpose as discourse.rs's mandatory CHALLENGE. However, code-level update rules (thresholds, etc.) are not confirmable from the source.
- **MAD-Fact (arXiv:2510.22967)** — a multi-agent debate framework for evaluating long-form factuality. Specialized for assessing the factuality of long texts that mix many individual claims, like a research document — a precedent that maps more directly onto scoring research reports than marketing-loop does.

**Synthesis**: the discourse cross-verification structure itself is inherited as-is from the adjacent-domain cases (fact-checking/legal review/peer review) marketing-loop already confirmed, but the research-domain-specific sub-problem of "citation-hallucination detection" is additionally incorporated by mapping CITETRACER's grading scheme (REAL/POTENTIAL/HALLUCINATED) onto the `citation_status` field, and by using MAD-Fact's long-form factuality-evaluation structure as a reference for the discourse-round design.

## 6. discourse (independent judgment → cross-debate → consensus) adjacent-domain cases — inherited from the marketing-loop survey

marketing-loop's §4 (fact-checking/journalism, legal review, academic peer review, HAJailBench termination conditions, the legal-MAD 3-ply structure) is a domain-agnostic survey and is inherited as-is. Summary:

- No adjacent domain has rules codified to the level of Code-Review-Loop (mandatory CHALLENGE, anonymization, file:line new-evidence requirement).
- Closest matches: HAJailBench (quantified termination conditions — similarity thresholds, risk-band convergence), legal MAD (arXiv:2606.30906, a 3-ply structure specifying even the number of model calls).
- MAD-Fact (long-form factuality) and CITETRACER (citation-hallucination grading), newly confirmed in this survey, are added to §5 as research-domain-specific supporting evidence.

## 7. Overall conclusion

- The three-stage structure "independent persona review → discourse cross-verification → deterministic verdict" has **no confirmable precedent** in the market-research/CI automation ecosystem (skills or commercial tools alike) — the same conclusion as the marketing-loop survey is reconfirmed in the CI domain.
- The differentiators specific to the research domain are **citation-hallucination detection** (CITETRACER) and **long-form factuality evaluation** (MAD-Fact); both are reflected in the `citation_status` field — absent from marketing-loop — and in the discourse-round design.
- The CI industry's practical evidence-gathering techniques (job-posting and patent analysis) match the methods actually used in this POS research — reflected in the lens design (see §the formal design spec) as the "engineering_diligence" persona.
- Each of the 6 failure modes observed in actual research work (§2) was mapped 1:1 onto either a deterministic_check or a discourse CHALLENGE condition (see design spec §3, §4).

### Suggested next steps

- Consider CITETRACER's cascading verification order (cache lookup → URL fetch → connector → web search) as the primary reference template for a `citation_status` determination pipeline.
- If MAD-Fact's detailed long-form factuality-evaluation algorithm (its claim-decomposition method) is publicly available, it would be worth incorporating further into the discourse-round design — this survey confirmed only an overview; a detailed follow-up is needed.
- Commercial CI tools' (Klue/Crayon) "Compete Agent" real-time monitoring feature is out of scope for this design (static document generation), but could be referenced for a future `--watch` mode extension.

## 8. Follow-up survey: OSS deep-research / company-research agent architectures (2026-07-31, additional survey after implementation)

After finishing the Rust CLI implementation, we re-verified — this time against the open-source deep-research ecosystem — whether the "independent persona → discourse → deterministic verdict" structure is actually a differentiator. Rather than the citation-hallucination-adjacent field (§5), this round targeted OSS projects solving the exact same problem as research-loop: "competitor/company research automation."

### Surveyed projects and their architecture

The initial survey was based on READMEs/landing pages. **The table below is the result of re-verification by directly reading the actual source files, and in the process one point where the README's description diverges from the actual code was found and corrected** — this correction is itself an instance of practicing, on itself, the very point of research-loop's engineering_diligence lens: "always re-verify against primary sources."

| Project | Scale/owner | Architecture (code-backed) | Cross-verification/discourse present? |
|---|---|---|---|
| **GPT Researcher** (assafelovic/gpt-researcher) | ~28,000★, 240 contributors (as of mid-2026) — the most widely adopted OSS deep-research agent | **[correction]** the README states "adopts the most-frequent information across 20+ sources," but reading the actual `skills/researcher.py`/`skills/curator.py` code shows otherwise — `_get_context_by_web_search()` collects sub-queries in parallel, then `get_similar_content_by_query()` filters based on **vector-embedding semantic similarity**, and `curate_sources()` makes a single LLM call that scores relevance/reliability/accuracy on 3 criteria and keeps only the top N. Not "frequency" but **semantic similarity + a single LLM ranking pass** | **None (confirmed from code)** — `curator.py` only ranks sources and returns the top N; it has no logic to detect and resolve mutually contradictory sources ("on error, returns the raw data as-is"). There is no way to filter out multiple sources that copied the same error |
| **company-research-agent** (guy-hartstein) | LangGraph-based, Gemini 2.5 Flash + GPT-5.1 | actual files under `backend/nodes/`: `grounding.py` (Tavily crawl of the target company's website, up to 50 pages — despite being called "grounding," this is just raw-material collection, not fact verification), `collector.py`, `curator.py` (relevance ranking), `enricher.py` (fills in raw content per URL, parallel-batch processed), `briefing.py` (category summarization via Gemini), `editor.py` (final-copy compilation via GPT-5.1) | **Explicitly none (confirmed by directly reading 3 source files)** — none of grounding.py, enricher.py, or curator.py has any contradiction-detection or cross-verification logic. **Directly confirmed that even a project fully identical in domain to research-loop and on a current (2026) stack has zero discourse structure at the code level** |
| **MetaGPT** | ~50,000★, "Code = SOP(Team)" | directly read `_save_competitive_analysis()` in `actions/write_prd.py` — it merely converts the "COMPETITIVE_QUADRANT_CHART" data produced alongside PRD generation into Mermaid and renders it as SVG | **None (confirmed from code)** — the competitive analysis is a **byproduct of a single LLM call** during the PRD-generation stage; there is no separate verification/cross-check stage at all. In reality a far simpler single pass than the marketing copy ("an SOP-ified team") suggests |
| **FacTool** (GAIR-NLP, newly added) | academic OSS | **tool-augmented** verification per domain across 4 domains — knowledge-based QA/code/math/scientific literature: QA uses multi-source Serper, code uses actual execution results, math uses Python re-execution, papers use source-text cross-checking. Dual scoring at the claim level and the response level | no discourse, but the direction of "verify via actual execution/source cross-checking rather than LLM judgment" suggests room to reinforce research-loop's `citation_status` (currently dependent on persona judgment during discourse) with code-execution-based checks — e.g. adding a deterministic secondary check that actually fetches a cited URL and does a string comparison for numeric claims |
| **DeerFlow** (ByteDance) / **open_deep_research** (LangChain, ranked #6 on Deep Research Bench) | large-company/community OSS | a plan-execute-loop research graph | could not re-verify at the source-code level (overview only, time constraints) — this round explicitly notes its verification depth differs from the other rows |
| **Loki** (an open-source fact-verification tool) | academic tool | 5 stages: claim identification → check-worthiness judgment → evidence-query generation → evidence retrieval (Serper API) → verification | not discourse, a single linear pipeline. The "check-worthiness judgment" stage has reference value (below) |

### Overall conclusion (relation to the existing §7, updated after source-code re-verification)

- **§7's conclusion is reconfirmed at the code level**: regardless of what their READMEs say, all three projects — GPT Researcher (curator.py), company-research-agent (3 node files), and MetaGPT (write_prd.py) — have no contradiction-detection or cross-verification logic in their actual source. The three-stage "independent persona review → discourse → deterministic verdict" structure remains unconfirmed.
- **Methodological self-correction**: the initial description of GPT Researcher as "frequency-based" was a direct carryover of the README's wording, while the actual code was "semantic-similarity vector filtering + a single LLM ranking pass." This difference is not trivial — the former is closer to "majority vote," the latter to "a single LLM judgment," and their failure points differ. **This survey itself demonstrated that architecture must not be asserted from a README alone.**
- **company-research-agent is the most important counter-case**: even while solving the exact same problem as research-loop (company-research automation) on a current 2026 stack, it was confirmed at the source level that none of its 6 stages — grounding→collect→curate→enrich→brief→edit — has any rebuttal or re-measurement step.
- **FacTool's tool-augmented verification** is a new reinforcement idea — separately from discourse (qualitative judgment), there is room to add a **deterministic secondary verification** to checks.rs that actually fetches a cited URL and does a string/number comparison for numeric claims (not implemented, backlog).
- **Loki's check-worthiness pre-filter** is worth adding to the backlog as an idea for reinforcing checks.rs's `citation_density_check` (which only measures density and doesn't look at a claim's verification-worthiness) — not implemented.

### Additional confirmation from a real-world smoke test

Right after this survey, we actually built the research-loop CLI and ran `review` against MangroveCafeOrder's POS-competitor research document (510 lines, 97 citations). `numeric_consistency_check` actually caught 8 different figures (₩15.5B/₩18.6B/₩49.01B/₩74.59B/₩12.8B, etc. — each referring to a different company/round, yet grouped together and detected because they were attached to the same phrase) attached to the phrase "operating loss," and during the discourse round an unverified estimate — "Toss Place has 300,000 locations (the company's own announcement says 200,000; 300,000 is an estimate)" — was raised as SURFACE. Whichever of the three re-verified projects above (GPT Researcher's similarity+single-ranking, company-research-agent's sequential pipeline, MetaGPT's single-LLM byproduct) had been used, it would have adopted the "300,000 locations repeatedly cited by multiple secondary outlets" outright, with no such rebuttal/re-measurement step — **this reconfirms the real-world effectiveness of the discourse structure through an actual output**.
