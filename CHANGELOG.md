# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-08-10

Initial tagged release. This version covers all work from the initial commit through
the end of an adversarial security/robustness re-audit and edge-case test expansion
(issues #18, #19 below); see [`evals/README.md`](evals/README.md) for the real
`claude -p --model haiku` execution-review log and its measured/estimated costs.

### Added

- Initial `research` CLI (Rust, `clap`) with four subcommands:
  - `review` — independent per-lens persona review, discourse cross-examination
    (per-lens critic calls + a single adjudicator call), deterministic checks, and a
    quantitative PASS/REVISE verdict with a numeric score.
  - `describe` — document summary, key findings, labels, and a deterministic scan for
    "needs verification" markers.
  - `improve` — concrete before/after revision suggestions.
  - `ask` — free-form Q&A over a document, accumulated to `ask.md`.
- `--prior`-chained review rounds: re-checks a previous round's confirmed findings
  against the current document and reconciles them into
  FIXED / STILL_OPEN / REVERSED / UNKNOWN.
- Deterministic (non-LLM) checks (`src/checks.rs`): citation density, source diversity
  (self-published-domain share), numeric consistency (same phrase, conflicting
  figures), access-limitation disclosure, incentivized-review-mention scan, staleness
  flag (citation freshness vs. `as_of_year`), and a live dead-link check.
- SSRF-hardened HTTP fetch path shared by dead-link checking and citation-quote
  verification: blocks loopback/private/link-local (incl. the `169.254.169.254` cloud
  metadata endpoint)/multicast/reserved/unspecified IP ranges on every redirect hop
  (not just the initial request), restricts to `http`/`https` schemes, and caps
  response bodies at 1MB.
- Semantic dedup pass (`src/dedup.rs`): an LLM-judged check (not a heuristic key) that
  stops a `--prior` STILL_OPEN reinsertion and this round's independent rediscovery of
  the same real-world issue — under a different id/label/citation format — from being
  scored as two separate deductions.
- Two LLM backends: `claude -p` subprocess (default, no separate API key) and
  OpenRouter REST API (`--backend openrouter`, requires `OPENROUTER_API_KEY`).
- `evals/README.md`: empirical review-findings log and real-run cost accounting from
  executing the tool's own pipeline against real `claude -p --model haiku` calls.
- Dependabot configuration for automated dependency updates.
- Edge-case regression test suite: empty/whitespace-only documents, non-UTF8 file
  content, malformed `--deterministic-results` JSON, unicode extremes (RTL Arabic/
  Hebrew text, an emoji ZWJ sequence, stacked combining diacritics) through the
  document-parsing and `escape_fence` paths, and subprocess-failure simulation
  (non-zero exit, non-JSON stdout) via fake `claude` scripts.

### Changed

- `ureq` 2.12.1 → 3.3.0 (breaking API changes adapted: `Error::Status` removal,
  `http_status_as_error` handling).
- `toml` 0.8.23 → 1.1.4+spec-1.1.0.
- `clap` 4.6.4 → 4.6.6.
- `actions/checkout` (CI) v4 → v7.

### Fixed

- CI: fixed a `cargo fmt --check` failure blocking the pipeline. (#1)
- `--prior` finding-id collision: STILL_OPEN/UNKNOWN reinsertion reused a round-less
  finding id (`"<lens_id>-<position>"`), so a fresh finding landing at the same
  lens/position in a later round silently overwrote the reinserted entry's resolution.
  Fixed by minting a round-scoped id. (#2)
- Stale `citation_status` on `--prior`-reinserted findings: `verify_citations` ran
  *before* reinsertion, so STILL_OPEN/REVERSED/UNKNOWN findings kept whatever
  `citation_status` they had from the *prior* round instead of being re-checked
  against the current round's sources. (#3)
- `--prior` fixcheck silent finding loss: if the fixcheck LLM response dropped a
  `prior_confirmed` finding's id, that finding vanished entirely — not FIXED, not
  STILL_OPEN, no `needs_human_review` flag, no trace in the report or score. Fixed by
  deterministically forcing any missing id to UNKNOWN. (#5)
- `--lenses ""` (or a blank/comma-only value) silently selected zero lenses — with the
  default spec, that meant zero findings, discourse skipped, and the run could still
  report `verdict=PASS score=100/100` for a document nothing actually reviewed. (#6)
- `review` crash on schema-mismatch LLM responses: a response that was valid JSON but
  didn't match the expected schema (e.g. a missing required field) skipped the retry
  path entirely and hard-aborted the whole run via `?`, discarding all LLM spend
  already made in that run with no `report.md`/`state.json` written. Fixed by folding
  schema deserialization into the same retry loop as JSON parsing. (#8)
- `verify_citations` clobbered `llm_citation_status` when re-run on a
  `--prior`-reinserted finding, overwriting the genuine original LLM self-report with
  a stale code-verified value carried over from the prior round. (#9)
- `--prior` STILL_OPEN reinsertion double-counting: the same real-world unaddressed
  issue, independently rediscovered by this round's own lens pass under a different
  id/label/citation format, was deducted from the score twice. A deterministic
  (label/section/citation_ref) key was tried and confirmed insufficient; fixed with a
  semantic (LLM-judged) dedup pass instead. (#10)

### Security

- **`escape_fence` prompt-injection defense bypass.** The function that neutralizes a
  document's own backticks (so document content can't prematurely close the
  ` ```untrusted_document ` fence isolating it as untrusted data) used a fixed-pattern
  `.replace("```", ...)`, which only fully breaks up backtick runs whose length is an
  exact multiple of 3. A run of 4, 5, 7, or 8 backticks (e.g. a document nesting a
  ` ``` ` example inside an outer ```` fence — an ordinary Markdown technique) left a
  genuine ` ``` ` substring behind, able to close the fence early. Reproduced directly
  by constructing backtick runs of length 4 through 12. Fixed by matching any run of
  2+ backticks and inserting a zero-width space between every adjacent pair, so no run
  length can survive with 3 contiguous raw backticks. (#7)
- **`call_claude` stdin/stdout pipe deadlock.** stdin (`ctx`+`task`, which embeds the
  entire research document — easily hundreds of KB) was written synchronously before
  anything started draining the child's stdout/stderr. A `claude` process that writes
  enough to stdout before finishing reading stdin (real, observed CLI behavior —
  progress/status output) filled the OS pipe buffer and deadlocked both sides
  indefinitely, with no timeout. Reproduced with a 2MB ctx against a fake child that
  writes 2MB to stdout before draining stdin. Fixed by writing stdin from a dedicated
  thread so stdout/stderr can drain concurrently. (#4)
- **No timeout on either LLM backend.** Beyond the deadlock above, neither backend
  bounded a simply-hung call: `call_claude` blocked forever on `wait_with_output()`
  against a `claude` process stuck on an auth prompt, a stalled network connection, or
  an internal retry loop, and `call_openrouter`'s `ureq::Agent` had no `timeout_global`
  set at all — unlike `checks.rs`'s already-hardened `safe_fetch`. A single wedged
  call anywhere in the pipeline hung the entire run with no recovery and no partial
  output. Found in an adversarial re-audit of resource-exhaustion/hang vectors,
  independent of the prior review rounds above. `call_claude` now polls for exit
  against a deadline and kills a wedged child; `call_openrouter` gets the same
  `timeout_global` `checks.rs` already uses. Both honor a new `--timeout-secs` flag
  (default 300s). (#18)
- **Unbounded memory use reading input files.** `--document`/`--brief`/`--style`/
  `--deterministic-results` were read with plain `std::fs::read_to_string`, with no
  size bound at all. A huge file (wrong path pointed at a large export/log by
  mistake), or a symlink to an infinite-but-valid-UTF-8 special file (e.g.
  `/dev/zero`, all NUL bytes, whose `fs::metadata().len()` reports 0 — so a
  metadata-only pre-check would not have caught it), would be read fully into memory
  with no bound, risking OOM before `main.rs`'s existing `DOC_WARN_CHARS` check even
  gets a chance to run (that check only warns about cost, and only *after* the full
  read already succeeded). Found in the same adversarial re-audit as #18. Reads are
  now capped at 64MB via `Read::take` on the actual bytes read, not just a
  pre-check against file metadata, so both failure modes fail fast and cleanly
  instead of growing memory without bound. (#19)
- **SSRF-hardened HTTP fetch path**, present since the initial commit (see *Added*
  above): private/link-local/metadata/multicast IP-range blocking re-validated on
  every redirect hop, scheme allowlisting, and response-size capping for both the
  dead-link check and citation-quote verification.
