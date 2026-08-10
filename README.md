# research-loop

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Release: v0.1.0](https://img.shields.io/badge/release-v0.1.0-green.svg)](https://github.com/Loop-Suite/Research-Loop/releases/tag/v0.1.0)

A Rust CLI that validates market/competitor research documents through **independent multi-persona review → per-lens independent discourse cross-examination → deterministic verdict**, instead of trusting a single LLM's pass over the text.

This is a working, buildable binary (`cargo build --release` produces `target/release/research`, 53 tests passing), not just a design document — it has been run end-to-end against a real research document (see [Validated on a real document](#validated-on-a-real-document) below). It is also still under active hardening: of the 13 issues opened against its own design, all 13 are closed — 12 as of the tagged [`v0.1.0`](https://github.com/Loop-Suite/Research-Loop/releases/tag/v0.1.0) release, plus one more (#23) closed afterward on `main` but not yet part of a tagged release (see [Known limitations](#known-limitations) and [Real-world validation](#real-world-validation)).

The default LLM backend is a `claude -p` subprocess (Claude Code CLI) — no separate API key required. An OpenRouter backend is also available (`--backend openrouter`, requires `OPENROUTER_API_KEY`, defaults to `openai/gpt-oss-120b` if `--model` is unset).

## Why this exists

This tool generalizes something that actually happened: across many rounds inside the MangroveCafeOrder project, a Korean café-POS competitor research document was researched, drafted, re-researched, and corrected over and over. That repeated cycle surfaced concrete failure modes that a single-pass research pipeline does not catch on its own:

- **Quantitative vs. qualitative signals disagreeing** — PayHere's app-store rating looked fine (4.0 / 245 reviews) while long-form reviews on Clien were scathing; Jobplanet (2.7) and Blind (3.5) disagreed by 0.8 points on the same employer.
- **A subject's own marketing content dominating search results** — searching in Korean for "café POS recommendation" surfaced Toss Place's own blog series repeatedly, which could be miscited as independent coverage if not distinguished from marketing copy.
- **Paid/incentivized reviews contaminating credibility** — Toss Place was found to be running a "₩5,000 per review + ₩50,000 per referred install" cash program, meaning positive-sounding posts about it could not be taken at face value.
- **The same metric reappearing with different numbers across sources** — T-order's count of integrated POS terminals was cited as anywhere from "25" to "50+" depending on the source, and its reported revenue (₩58.7B in 2023 vs. ₩41.9B in 2025) looked like decline but was actually explained by an accounting-method change, not a shrinking business — indistinguishable without re-verification.
- **A prior conclusion getting reversed by newer evidence** — a T-order/KT relationship recorded as "acquisition talks rumored, unconfirmed" turned out, three months later, to be a public IP-dispute and restructuring story — the kind of reversal a document has to explicitly flag as *overturned*, not just updated.
- **Closed platforms getting silently skipped** — Korea's largest self-employed-business community runs on a login-gated Naver café that search engines don't index, so "could not verify" and "does not exist" need to stay distinguishable.

None of these are caught by asking one model to "review this document" once. research-loop encodes the fix as a repeatable pipeline instead of manual vigilance.

> Design rationale, stage-by-stage: **[docs/design-spec.md](docs/design-spec.md)**
> Evidence survey (competitive-intelligence tooling landscape, citation-hallucination-detection research, and a source-code-level re-verification of several open-source "deep research" agents): **[docs/research-and-evidence-survey-2026-07-31.md](docs/research-and-evidence-survey-2026-07-31.md)**

## Architecture at a glance

```mermaid
flowchart LR
    subgraph Inputs
        DOC["Research document (Markdown)<br/>--document"]
        BRIEF["Brief: angles that must be covered<br/>--brief (optional)"]
        STYLE["Tone/format guide<br/>--style (optional)"]
        DETRES["External deterministic results<br/>--deterministic-results (optional)"]
        PRIOR["Prior round's state.json<br/>--prior (optional)"]
    end

    DOC --> NORM["input::normalize<br/>sections, citations, word count"]
    BRIEF --> NORM
    STYLE --> NORM
    DETRES --> NORM

    NORM --> CHECKS["checks::run_all<br/>7 deterministic checks<br/>(skipped if --deterministic-results given)"]
    NORM --> SELECT["lens::select_lenses<br/>LLM picks 3-5 of 7 optional lenses<br/>(or --lenses override)"]

    SELECT --> REV1["lens::review_lens<br/>Persona A, independent"]
    SELECT --> REV2["lens::review_lens<br/>Persona B, independent"]
    SELECT --> REVN["lens::review_lens<br/>Persona N, independent"]
    SELECT -.always included.-> GOOD["lens::review_good_things<br/>good research practices worth keeping"]

    REV1 --> POOL["Findings pool<br/>(lens id kept, used to route discourse)"]
    REV2 --> POOL
    REVN --> POOL

    POOL --> DISCOURSE["discourse::run<br/>one independent critic call per lens<br/>+ one adjudicator call"]
    PRIOR -.-> FIXCHECK["fixcheck::run<br/>FIXED / STILL_OPEN / UNKNOWN / REVERSED"]
    FIXCHECK -.-> DISCOURSE

    DISCOURSE --> VERIFY["checks::verify_citations<br/>real HTTP re-fetch + quote match"]
    VERIFY --> COVERAGE["requirements::verify<br/>brief angle coverage → coverage_gaps"]
    CHECKS --> QUANT["quantify::summarize<br/>severity-weighted score + PASS/REVISE verdict"]
    COVERAGE --> QUANT
    GOOD --> REPORT

    QUANT --> REPORT["report::write<br/>report.md + deterministic-results.json + state.json"]
```

## Module map (`src/`)

```mermaid
flowchart TB
    MAIN["main.rs<br/>clap CLI, subcommand dispatch, par_map concurrency helper"]

    MAIN --> SPEC["spec.rs<br/>loads TOML lens pool, labels, thresholds"]
    MAIN --> INPUT["input.rs<br/>parses document/brief/style, extracts sections + citations"]
    MAIN --> LENS["lens.rs<br/>select_lenses, review_lens, review_good_things"]
    MAIN --> CHECKS["checks.rs<br/>7 deterministic checks, SSRF-guarded HTTP fetch, citation verification"]
    MAIN --> DISCOURSE["discourse.rs<br/>per-lens critic calls + adjudicator, resolution states"]
    MAIN --> FIXCHECK["fixcheck.rs<br/>--prior recheck of previously CONFIRMED findings"]
    MAIN --> REQ["requirements.rs<br/>brief angle coverage verification"]
    MAIN --> QUANTIFY["quantify.rs<br/>severity scoring + PASS/REVISE verdict"]
    MAIN --> REPORT["report.rs<br/>writes report.md / describe.md / improve.md / ask.md"]
    MAIN --> STATE["state.rs<br/>RunManifest: state.json for --prior chaining + audit"]
    MAIN --> DESCRIBE["describe.rs<br/>Describe subcommand"]
    MAIN --> IMPROVE["improve.rs<br/>Improve subcommand"]
    MAIN --> ASK["ask.rs<br/>Ask subcommand"]

    LENS --> PROMPTCTX["promptctx.rs<br/>shared prompt context + prompt-injection defenses"]
    DISCOURSE --> PROMPTCTX
    CHECKS --> PROMPTCTX
    REQ --> PROMPTCTX

    MAIN --> LLM["llm.rs<br/>Llm: claude-cli subprocess or OpenRouter REST, usage/cost tracking"]
    LENS -.calls.-> LLM
    DISCOURSE -.calls.-> LLM
    FIXCHECK -.calls.-> LLM
    REQ -.calls.-> LLM
```

## Requirements

- Rust 1.85+ (enforced via `rust-version` in `Cargo.toml` and CI's MSRV matrix job)
- `claude` CLI installed and logged in (pass `--claude-bin` if it's not on `PATH`), or `OPENROUTER_API_KEY` if using `--backend openrouter`

## Build

```bash
cargo build --release   # binary at target/release/research
```

## Subcommands

```bash
# 1) Independent per-lens review + discourse cross-examination (the core pipeline)
research --model sonnet --cheap-model haiku review \
  --spec specs/default.toml --document my-research.md \
  --brief brief.md --out runs/pos --concurrency 4 --max-rounds 2

# 2) Summarize the document (key findings, labels, splittability) + scan for "needs verification" markers
research describe --spec specs/default.toml --document my-research.md --out runs/pos

# 3) Propose concrete revisions (things to re-research or correct)
research improve --spec specs/default.toml --document my-research.md --out runs/pos

# 4) Free-form Q&A grounded in the document, appended to ask.md
research ask --spec specs/default.toml --document my-research.md --out runs/pos \
  "Did this company obtain PCI-DSS certification?"
```

Selected `review` flags (see `src/main.rs` for the full `clap` definition): `--lenses` (comma-separated manual override, skips LLM lens selection), `--deterministic-results` (feed in externally computed check results instead of re-running `checks::run_all`), `--prior <run-dir>` (chain against a previous round's `state.json`), `--as-of-year` (staleness baseline; defaults to the highest 4-digit year found in the document), `--skip-link-check` (skip live HTTP checks for CI/offline use).

`review`'s exit code reflects the verdict, so it can be used as a CI gate: `PASS` → exit `0`, `REVISE` → exit `1` (Rust-level errors, e.g. a malformed spec, also exit `1`). `describe`/`improve`/`ask` always exit `0` on success — they don't produce a verdict.

## Execution flow of a `review` run

```mermaid
sequenceDiagram
    participant CLI as research review
    participant Spec as spec::load
    participant Input as input::normalize
    participant Lens as lens::select_lenses /<br/>review_lens (parallel, par_map)
    participant Discourse as discourse::run
    participant Fix as fixcheck::run
    participant Checks as checks::run_all /<br/>verify_citations
    participant Cov as requirements::verify
    participant Report as report::write / state::write

    CLI->>Spec: load TOML persona pool
    CLI->>Input: read document, extract sections & citations
    CLI->>Lens: pick 3-5 optional lenses (or --lenses override)
    par independent, sealed reviews
        Lens->>Lens: Persona A reviews (no visibility into B, C...)
        Lens->>Lens: Persona B reviews
        Lens->>Lens: Persona N reviews
        Lens->>Lens: good_things review (always included)
    end
    Lens-->>CLI: findings pool (tagged by originating lens)
    opt --prior <run-dir> supplied
        CLI->>Fix: recheck previously CONFIRMED findings
        Fix-->>CLI: FIXED / STILL_OPEN / UNKNOWN / REVERSED
    end
    CLI->>Discourse: cross-examination rounds (per-lens critic calls, see below)
    Discourse-->>CLI: CONFIRMED / REJECTED / MERGED / UNCERTAIN
    CLI->>Checks: 7 deterministic checks + real HTTP citation re-fetch
    opt --brief supplied
        CLI->>Cov: verify brief angles against confirmed findings
        Cov-->>CLI: coverage_gaps
    end
    CLI->>Report: assemble report.md, deterministic-results.json, state.json
    Report-->>CLI: verdict = PASS or REVISE (exit code 0 / 1)
```

## Lens selection and the persona pool

Each lens is voiced by a real analyst/author to suppress sycophancy — the model is told *who* it is arguing as, not just given a topic. There is no `--research-type` flag: `lens::select_lenses` shows the LLM a catalog of each lens's `signal` (the kind of document it applies to) and asks it to freely pick 3-5 of the 7 optional lenses; an 8th lens, `good_things` (looks for research practices worth keeping), is always included and is not part of that selection.

```mermaid
graph TB
    classDef tier1 fill:#2b6cb0,color:#fff,stroke:#1a4971
    classDef tier2 fill:#718096,color:#fff,stroke:#4a5568
    classDef always fill:#38a169,color:#fff,stroke:#276749

    CATALOG["lens catalog (id + signal text)<br/>fed to one LLM call"] --> PICK["select 3-5 optional lenses"]

    PICK --> MD["market_dynamics<br/>Michael Porter<br/>structural vs. transient advantage"]:::tier1
    PICK --> FF["financial_forensics<br/>Aswath Damodaran<br/>narrative must match the numbers"]:::tier1
    PICK --> PR["payments_regulatory_economics<br/>Patrick McKenzie<br/>does the model survive fee regulation?"]:::tier1
    PICK --> ED["engineering_diligence<br/>Gergely Orosz<br/>verify via job postings & eng blogs"]:::tier1
    PICK --> II["incentive_integrity<br/>Cory Doctorow<br/>whose interest does this review serve?"]:::tier1
    PICK --> OC["org_culture_signal<br/>Adam Grant<br/>don't over-trust a single review platform"]:::tier2
    PICK --> CP["closed_platform_ethnography<br/>danah boyd<br/>'couldn't access' ≠ 'doesn't exist'"]:::tier2

    GT["good_things<br/>always included, not selected<br/>surfaces practices worth keeping"]:::always
```

## Discourse: independent critic per lens, then a single adjudicator

Discourse used to run as one central LLM call simulating every lens's rebuttals at once. It now runs as one independent critic call **per participating lens**, each shown only the *other* lenses' findings (never its own — a lens cannot review itself) and run in parallel via the same `par_map` concurrency helper used for the initial lens reviews. Their moves are then pooled and resolved by a single adjudicator call.

```mermaid
flowchart TB
    FINDINGS["Findings pool from all lenses"] --> GROUP["discourse::participating_lenses<br/>group unresolved findings by originating lens"]
    GROUP --> CHECK{"2+ lenses<br/>participating?"}
    CHECK -- "no (0-1 lens)" --> SKIP["critic stage skipped for this round<br/>(no comparison target)"]
    CHECK -- "yes" --> CA["run_lens_critic_call: Lens A<br/>sees only Lens B, C, ...'s findings"]
    CHECK -- "yes" --> CB["run_lens_critic_call: Lens B<br/>sees only Lens A, C, ...'s findings"]
    CHECK -- "yes" --> CN["run_lens_critic_call: Lens N<br/>sees only the other lenses' findings"]
    CA & CB & CN -->|"parallel, par_map"| MOVES["pooled AGREE / CHALLENGE / CONNECT / SURFACE moves"]
    SKIP --> MOVES
    MOVES --> ADJ["single adjudicator call<br/>DISCOURSE_ADJUDICATE_SYSTEM"]
    ADJ --> RES["resolutions: CONFIRMED / REJECTED / MERGED / UNCERTAIN"]
```

Every `AGREE`/`CHALLENGE` counts as weight 1.0 — the model's self-reported `confidence` (high/medium/low) is still recorded for the audit trail but no longer converts into a differentiated vote weight, since those weights were never calibrated against any ground truth (see [Known limitations](#known-limitations)). A valid `CHALLENGE` must re-measure the same metric via a different method or an independent source and find a discrepancy — "this feels outdated" with no counter-evidence is downgraded to `SURFACE` instead.

### Resolution lifecycle, including `--prior` re-checks

```mermaid
stateDiagram-v2
    direction LR
    [*] --> UNRESOLVED: finding raised by a lens
    UNRESOLVED --> CONFIRMED: net vote (AGREE-CHALLENGE, equal weight) >= 0.6
    UNRESOLVED --> REJECTED: net vote (AGREE-CHALLENGE, equal weight) <= -0.6
    UNRESOLVED --> UNCERTAIN: vote in between, rounds exhausted
    UNCERTAIN --> CONFIRMED: later round tips the vote
    UNCERTAIN --> REJECTED: later round tips the vote
    CONFIRMED --> [*]: appears in report.md findings table

    state "Next round only, via --prior" as NextRound {
        CONFIRMED --> FIXED: document addressed it (dropped, not re-reported)
        CONFIRMED --> STILL_OPEN: unaddressed, re-enters this round's findings
        CONFIRMED --> REVERSED: newer evidence overturns the prior conclusion — promoted to a new P0 finding
        CONFIRMED --> UNKNOWN: cannot tell either way — kept, flagged needs_human_review
    }
```

`REVERSED` is a research-loop addition with no equivalent in Code-Review-Loop's fixcheck — it exists specifically for the "prior conclusion overturned by newer evidence" failure mode (the T-order/KT example above). `STILL_OPEN`, `REVERSED`, and `UNKNOWN` all re-enter the current round's findings (`UNKNOWN` was previously dropped silently — fixed, see [Known limitations](#known-limitations)); only `FIXED` drops out.

## Deterministic checks (`checks.rs`)

Code-Review-Loop splits this into `policy.rs` + `semgrep.rs`. Research documents have no equivalent of an "auto-fill everything" scanner like semgrep, so there was no reason to keep two modules — they're merged into one.

```mermaid
flowchart LR
    DOC["Normalized document<br/>+ citations list"] --> C1["citation_density_check<br/>approx. claim-sentence count vs. citation count"]
    DOC --> C2["source_diversity_check<br/>% citations on subject-owned domains<br/>(exact host/subdomain match via url crate)"]
    DOC --> C3["numeric_consistency_check<br/>same phrase, conflicting figures (word-window regex)"]
    DOC --> C4["staleness_flag<br/>citation year vs. threshold"]
    DOC --> C5["incentive_disclosure_scan<br/>'review event' / 'sponsorship' keywords"]
    DOC --> C6["access_limitation_disclosure_check<br/>honest 'could not verify' phrasing present"]
    DOC --> C7["dead_link_check<br/>real HTTP HEAD via ureq, SSRF-guarded"]

    C1 & C2 & C3 & C4 & C5 & C6 & C7 --> RESULT["PASS / WARN / FAIL / N-A / NOT_CONFIGURED<br/>+ evidence string"]
    RESULT --> REPORTOUT["report.md → Deterministic checks table<br/>+ deterministic-results.json (reusable via --deterministic-results)"]
```

| check | what it does |
|---|---|
| `citation_density_check` | approximate claim-sentence count (max of Korean sentence-ending particles vs. general punctuation heuristics) vs. citation count |
| `source_diversity_check` | share of citations whose URL host exactly matches (or is a subdomain of) a domain the research *subject* itself owns — parsed via the `url` crate, not substring matching |
| `numeric_consistency_check` | flags the same short phrase (2-4 words before a Korean numeric unit — eok/jo/won/%/myeong/gae, i.e. hundred-million, trillion, won-currency, percent, person-counter, item-counter) appearing with different numbers elsewhere in the document (heuristic, `WARN` only, never `FAIL`) |
| `staleness_flag` | flags citation years older than a configurable threshold relative to `--as-of-year` |
| `incentive_disclosure_scan` | flags "review event / sponsorship / cash reward" language for downstream discourse review |
| `access_limitation_disclosure_check` | checks the document actually says "could not verify" somewhere, rather than staying silent about access limits |
| `dead_link_check` | makes a real HTTP HEAD request (via `ureq`, through an SSRF-guarded fetch path that blocks loopback/private/link-local/metadata addresses) per citation URL; timeouts are `WARN`, not `FAIL`. Skippable with `--skip-link-check` |

Separately, `checks::verify_citations` re-fetches each cited URL (same SSRF-guarded path) after discourse and overwrites each finding's `citation_status` with `UNFETCHED` / `FETCH_FAILED` / `QUOTE_MATCHED` / `QUOTE_NOT_FOUND` based on whether the finding's `evidence` text is actually found (whitespace/case-normalized substring match) in the fetched page. The LLM's original self-reported status is preserved separately as `llm_citation_status` but no longer drives the report.

## Deterministic vs. LLM-judgment boundary

The verdict is designed so an LLM's opinion can never override "hard evidence" from the deterministic checks — `quantify::verdict` short-circuits to `REVISE` before it even looks at findings or confidence if any check has `FAIL`ed:

```mermaid
flowchart TD
    START(["quantify::verdict"]) --> C1{"any deterministic<br/>check == FAIL?"}
    C1 -- yes --> REVISE["REVISE<br/>(independent of findings/confidence)"]
    C1 -- no --> C2{"any resolution has<br/>needs_human_review = true?<br/>(--prior UNKNOWN/REVERSED)"}
    C2 -- yes --> REVISE
    C2 -- no --> C3{"any CONFIRMED finding<br/>severity P0 or P1?"}
    C3 -- yes --> REVISE
    C3 -- no --> C4{"coverage_gap_count > 0?<br/>(--brief angle missing)"}
    C4 -- yes --> REVISE
    C4 -- no --> PASS["PASS"]

    SCORE["score = 100 - Σ penalty(severity)<br/>over CONFIRMED findings only<br/>P0=25 P1=12 P2=5 P3=1, floor 0"]
```

`score` (0-100) is reported alongside `verdict` but never itself decides `PASS`/`REVISE` — it exists to rank *how bad* a `REVISE`, or how clean a `PASS`, is.

## Validated on a real document

This was run against the research document that motivated it (MangroveCafeOrder's POS-competitor research):

- `describe` extracted key findings, sections, and "needs verification" markers from the document.
- `review` produced `verdict=PASS score=95/100`, and along the way `numeric_consistency_check` caught 8 conflicting figures behind the phrase "operating loss" (each figure was correct for a *different* company/round, but the check correctly flagged that they needed disambiguating) — and the discourse round surfaced an unverified secondary-source estimate that diverged from the company's own official figure, something a naive "most-repeated-number-wins" approach would have missed.

## It was checked against real open-source competitors, at the source-code level

The evidence survey didn't stop at README pages. Reading actual source files in `assafelovic/gpt-researcher`, `guy-hartstein/company-research-agent`, and `geekan/MetaGPT` corrected one of the survey's own earlier claims: GPT Researcher's README implies "most-frequent-info-wins" voting, but its actual `curator.py` does vector-similarity filtering plus a single LLM ranking pass — no contradiction detection either way. `company-research-agent` (LangGraph + Gemini 2.5 + GPT-5.1, solving the *same problem* as research-loop) turned out to be a strictly sequential 6-node pipeline with zero cross-validation anywhere in its source. Full writeup, including the self-correction: [docs/research-and-evidence-survey-2026-07-31.md §8](docs/research-and-evidence-survey-2026-07-31.md).

## Real-world validation

`research`'s own `--prior`-chained review pipeline has been run against itself twice, in two
separate real-money sessions: an initial 2-round `--prior` chain, and a later, deeper 3-round
chain run as part of a production-hardening pass. Full methodology, every raw number, and
caveats for both (one document per session, one model, no benchmark matrix):
[evals/README.md](evals/README.md).

**Session 1 — 2-round chain:**

- **9 real bugs found** — 6 from zero-cost static review alone (including a real
  prompt-injection fence-escaping bypass and a reproduced `std::process` pipe deadlock), 3 more
  surfaced only by actual execution, incl. **#8**: a valid-but-wrong-shape haiku response crashed
  the entire run pre-fix, discarding all LLM spend already made — a class of bug code reading
  alone doesn't catch.
- **#10** (`--prior` STILL_OPEN reinsertion double-counting an unaddressed issue's severity)
  needed semantic (LLM-judged) dedup, not a deterministic key — a `lens`+`section`+`label` key
  was tried against the real reproduction and confirmed insufficient. Logged as open in the eval
  write-up; fixed separately afterward (`src/dedup.rs`).
- Session 1 real spend: ≈ $0.93–$1.03 (two successful rounds at $0.4136 + $0.3707, exact from
  `state.json`, plus one pre-fix crashed attempt estimated at $0.15–$0.25).

**Session 2 — production hardening pass, 3-round chain (4 lenses):**

- **2 more real bugs found by a fresh adversarial re-audit**, zero LLM cost — **#18** (neither
  LLM backend had a timeout; a hung `claude` subprocess or stalled OpenRouter connection
  blocked the run forever) and **#19** (no size cap reading `--document`/`--brief`/`--style`/
  `--deterministic-results`, risking unbounded memory use).
- **1 more real bug found by going a round deeper than Session 1** (3 rounds instead of 2):
  **#23** — round 3 crashed (haiku returned 3 different malformed-JSON shapes in a row to the
  discourse critic call) only *after* 4 real, already-costed lens-review calls had completed;
  that spend appeared nowhere in the output. Fixed by printing the accumulated usage/cost
  summary on any exit, not only success.
- Round 1 ($1.0567, 15 calls) and round 2 ($1.3207, 20 calls) succeeded — exact costs from
  `state.json`; round 3's crash cost is unmeasured, estimated ≈$0.3–$0.6 (round 3 was
  deliberately not re-run — see `evals/README.md`).
- Session 2 real spend: $2.3774 exact (rounds 1-2) + ≈$0.3–$0.6 estimated (round 3's crash).

**Total real spend across both sessions to date: ≈ $3.6–$4.0.**

Test suite: **53 tests passing** on `main` (52 as of the tagged
[`v0.1.0`](https://github.com/Loop-Suite/Research-Loop/releases/tag/v0.1.0) release; +1
subprocess-level integration test added afterward for the #23 fix, not yet in a tagged
release).

## Known limitations

Tracked as GitHub issues against this repo: 13 filed, all 13 closed (12 as of the tagged
[`v0.1.0`](https://github.com/Loop-Suite/Research-Loop/releases/tag/v0.1.0) release; #23
closed afterward, its fix on `main` but not yet in a tagged release — see
[Real-world validation](#real-world-validation) above). One further limitation below
(confidence calibration) is a known design gap, not filed as its own GitHub issue.

- **Design gap, not GitHub-tracked — confidence calibration:** `confidence` (high/medium/low) is an uncalibrated self-report, not a measured accuracy rate. `discourse.rs` no longer converts it into a differentiated vote weight (it used to: high=1.0, medium=0.6, low=0.3) — those numbers were never validated against ground truth, so every `AGREE`/`CHALLENGE` now counts as weight 1.0 (plain majority vote) until a labeled benchmark exists to calibrate against.
- **Citation verification is a substring match, not entailment:** `checks::verify_citations` re-fetches each cited page and checks whether the finding's `evidence` text appears in it (closed #4) — but `QUOTE_MATCHED` means the exact wording was found on the page, not that the page's *meaning* supports the claim. Fully tool-augmented verification (re-fetch, extract the actual number, diff it against the document's claim) is not implemented.
- **`numeric_consistency_check` is a word-window regex,** not morphological/entity parsing — expect false positives/negatives. It stays `WARN`-only for exactly this reason.
- **`citation_density_check` is still a heuristic** (Korean sentence-ending particles vs. general punctuation, whichever counts more) after its false-positive fix (#5) — `source_diversity_check`, by contrast, now does exact host/subdomain matching via the `url` crate rather than substring matching.
- **Same-model bias risk:** if the generation model and the judge model are the same, the judge tends to rate its own writing style more favorably. `--cheap-model` silently falls back to `--model` when unset, with no warning printed — worth adding.
- **No human-voice rewrite stage:** research documents aren't being rewritten for tone, so that Code-Review-Loop stage was dropped entirely rather than adapted (see `docs/design-spec.md` §0).

## Lineage

Architecture origin: Code-Review-Loop (`Loop-Suite/codereview-loop`) — independent persona review, anonymized discourse, deterministic verdict, ported to the market/competitor-research documentation domain.
