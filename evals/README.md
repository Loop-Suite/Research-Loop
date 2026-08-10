# Empirical review findings and real-run costs

This documents an actual review pass done on this repo's own code, in two phases: a static
code review (no LLM calls, zero cost) followed by real execution of `research`'s own
`--prior`-chained `review` pipeline via `claude -p --model haiku` (real API cost). Modeled on
[Code-Review-Loop's `evals/README.md`](https://github.com/Loop-Suite/Code-Review-Loop/blob/main/evals/README.md)
— same intent: report what was actually measured, not what was expected, and say plainly
where a number is estimated rather than measured.

All issue numbers below (#2-#10) are on this repo; all are real GitHub issues, filed and
fixed (except #10) as part of this same session.

## TL;DR

| Phase | Method | Real LLM cost | Findings |
|---|---|---|---|
| Round 1: static review | reading `src/`, no LLM calls | $0 | 2 fixed (#2, #3) |
| Round 2: deeper static review | reading `src/`, no LLM calls | $0 | 4 fixed (#4, #5, #6, #7) |
| Execution review, pre-fix attempt | `review` × 2 lenses, crashed before usage printed | ~$0.15–$0.25 (unmeasured, see caveat) | 1 fixed (#8) |
| Execution review, post-fix, round 1 | `review --lenses market_dynamics,financial_forensics --model haiku` | $0.4136 (state.json `cost_usd`) | — |
| Execution review, post-fix, round 2 | `review --prior <round-1-dir>` (same lenses/model) | $0.3707 (state.json `cost_usd`) | 2 found: 1 fixed (#9), 1 open (#10) |
| **Total** | | **≈ $0.93–$1.03** | **8 fixed, 1 open** |

## What this bought

- **Static review alone (zero cost) caught 6 real bugs before any model ran**, including two
  correctness bugs in `--prior` reinsertion logic (#2, #3), a real prompt-injection defense
  bypass (#7 — security-relevant, reproduced directly, not theoretical), and a real deadlock
  reproduced under a 15-second test timeout (#4). None of this needed an LLM call to find.
- **The first real execution immediately found something code reading didn't: a full-run
  crash on the very first attempt.** #8 — a haiku discourse-critic response was valid JSON but
  missing a required field, and the old code path had no retry for that case, so it aborted
  the entire run via `?` with no `report.md`/`state.json` written, discarding every dollar of
  LLM spend already made earlier in that same run. This class of bug is close to undetectable
  by code reading or hand-written unit tests — it only shows up when a real model actually
  returns a malformed-shape (but valid-JSON) response.
- **The #8 fix was re-verified against the same real failure, not just re-run in isolation.**
  After the fix, rerunning hit the *same* schema mismatch again on the *same* real haiku
  response pattern — but the new `Llm::json_typed` retry loop recovered automatically on the
  3rd attempt and produced a normal `report.md`. Same failure condition, different outcome,
  confirmed on real model output.
- **Fixing #3 introduced a new bug of its own, only visible by inspecting real state.** #9 was
  found by reading round 2's actual `state.json` byte-for-byte after a real `--prior`-chained
  run, not by re-reading the #3 diff — the #3 fix's own re-ordering of `verify_citations`
  clobbered `llm_citation_status` on reinserted findings, a side effect a unit test written
  against the #3 scenario alone would not have caught.
- **#10 is real, measured, and deliberately left open.** Round 2's actual `report.md` scored
  54/100 with 5 separate deductions for what was, on inspection, 2 distinct unaddressed
  issues — because a `--prior`-reinserted finding and this round's independently-rediscovered
  version of the same real-world fact get different LLM-authored `label`/`citation_ref` values
  each round, so no deterministic key reliably matches them. A deterministic dedup key
  (lens+section+label, and separately citation_ref) was tried against this exact reproduction
  and confirmed insufficient. Root fix needs semantic (LLM-judged) matching, not a heuristic —
  documented and left open rather than patched around.
- **Total real spend across everything that hit the API: roughly $0.93–$1.03.** $0.7843 of
  that is exact (`state.json`'s `cost_usd` field, two successful runs). The rest is a real but
  unmeasured cost from the pre-fix crash path — flagged as an estimate, not folded into the
  precise figure as if it were measured.

## Round 1: static review (no LLM calls)

Read `src/main.rs`'s `--prior` reinsertion path end to end.

- **#2 — Finding id collision:** `STILL_OPEN`/`UNKNOWN` reinsertion used round-less finding ids
  (`"<lens_id>-<position>"`). Any fresh finding landing at the same lens/position this round
  overwrote the reinserted entry in `resolved`, silently destroying an unrelated finding's
  record. Fixed by minting a round-scoped id (`"{id}-still-open-r{round}"` /
  `"{id}-unknown-r{round}"`), matching the pattern the `REVERSED` branch already used.
- **#3 — Stale citation_status:** `checks::verify_citations` ran *before* the `--prior`
  reinsertion block, so `STILL_OPEN`/`REVERSED`/`UNKNOWN` findings carried forward whatever
  `citation_status` they had from the *prior* round instead of being re-checked against this
  round's sources. Fixed by moving the `verify_citations` call to after reinsertion.

Both fixed in a single commit (`6056c50`), since the fix for #3 (reordering the call) touches
the same block as #2's id-collision fix.

## Round 2: deeper static review (no LLM calls)

A second, more adversarial pass over the same `--prior`/lens-selection/prompt-construction
code, specifically looking for concurrency and input-validation edge cases the first pass
didn't target.

- **#4 — `call_claude` deadlock:** stdin (`ctx`+`task`) was written to the child process
  synchronously, in-thread, *before* anything started draining stdout/stderr.
  `shared_context` embeds the entire research document on every call, so a large document plus
  a child that writes enough to stdout before finishing reading stdin (a real
  `claude` CLI behavior — progress/status output) fills the OS pipe buffer and deadlocks both
  sides with no timeout. **Reproduced for real**: a test harness spawns a fake child script
  that writes 2MB to stdout before draining stdin, against a ~2MB `ctx` — this hung under a
  15-second test timeout before the fix. Fixed by writing stdin from a dedicated thread so
  `wait_with_output()` can drain stdout/stderr concurrently; the same test passes in ~0.15s
  after the fix.
- **#5 — `--prior` fixcheck silent loss:** the re-check loop only processed finding ids that
  fixcheck's LLM response actually returned. If the model dropped an id from its JSON output —
  a known LLM failure mode — that finding vanished entirely: not `FIXED`, not `STILL_OPEN`, no
  `needs_human_review` flag, no trace anywhere in the report or score. Fixed with
  `reconcile_fix_results`: any `prior_confirmed` id missing from the response is
  deterministically forced to `UNKNOWN` (re-enters findings, flags `needs_human_review`),
  matching how an explicit `UNKNOWN` was already handled.
- **#6 — `--lenses ""` selects zero lenses silently:** the manual `--lenses` override path had
  no non-empty check, unlike the automatic LLM-selection path. `--lenses ""` (or a
  blank/comma-only value) produced zero selected lenses, and with the default spec that meant
  zero findings, discourse skipped entirely, and the run could still report
  `verdict=PASS score=100/100` for a document nothing actually reviewed. Fixed by extracting a
  `parse_lenses_arg()` that requires at least one resulting id, mirroring the invariant the
  auto-selection path already enforced.
- **#7 — `escape_fence` prompt-injection bypass (security).** `escape_fence` used
  `doc.replace("```", ...)` to stop a document's own content from prematurely closing the
  ` ```untrusted_document ` fence that isolates it as untrusted data. `str::replace` on a fixed
  3-character pattern only fully breaks up backtick runs whose length is an exact multiple of
  3 — a run of 4, 5, 7, or 8 backticks (e.g. a document nesting a ` ``` ` example inside an
  outer ` ```` ` fence, an ordinary Markdown technique) leaves a genuine ` ``` ` substring
  behind. **Confirmed by actually constructing runs of N=4 through 8 backticks and observing
  the bypass** — the escaped output still contained a raw 3-backtick sequence able to close the
  fence early, which is the exact condition this function exists to prevent. Fixed by
  replacing the fixed-pattern `.replace` with a regex matching any run of 2+ backticks and
  inserting a zero-width space between every adjacent pair in the run, so no run length can
  survive with 3 contiguous raw backticks.

Commits: `71b3a6a` (#4), `92c12a0` (#5), `1d4b2a3` (#6), `a6266c1` (#7).

## Execution review: real `claude -p --model haiku` calls

Setup: `research review --lenses market_dynamics,financial_forensics --model haiku` against a
small test document, then a second round via `research review --prior <round-1-dir>` (same
lenses/model) against a revised version of the same document — a real 2-round `--prior` chain,
the actual code path #2/#3/#9/#10 all concern.

### Pre-fix attempt: immediate crash (#8)

The first real invocation crashed before completing. A haiku discourse-critic call returned
valid JSON that didn't match the expected schema (a required `target` field missing) —
`Llm::json_ctx` retried on JSON-parse failure, but every call site then ran
`serde_json::from_value::<T>(v)` as a separate, unretried step outside that loop, so a
schema-shape mismatch skipped the retry path entirely and hard-aborted the whole run through
`?`. No `report.md`, no `state.json`, and every dollar of LLM spend already made earlier in
that run was discarded with no usage summary printed.

**Fix:** added `Llm::json_typed`/`json_ctx_typed`, folding JSON parsing and schema
deserialization into the same retry loop, and switched all 9 call sites to it (`6f67d87`,
fixes #8).

**Re-run after the fix reproduced the identical underlying condition** — haiku returned the
same malformed shape again — but this time `json_typed`'s retry loop caught it and succeeded
on the 3rd attempt, producing a normal `report.md`. Same real failure condition, verified fix.

### Post-fix round 1: success, $0.4136

Cost is `state.json`'s `cost_usd` field from this actual run, not estimated.

### Post-fix round 2 (`--prior`-chained): success, $0.3707, 2 findings

Cost again read directly from `state.json`. Inspecting this run's state surfaced two further
issues:

- **#9 — `verify_citations` clobbers `llm_citation_status` on reinsertion.** Direct side
  effect of the #3 fix: `verify_citations` unconditionally copied `citation_status` into
  `llm_citation_status` before recomputing `citation_status`, correct for fresh findings
  (`llm_citation_status` defaults to empty) but wrong for `--prior`-reinserted findings, whose
  `citation_status` already holds a prior round's code-verified value and whose
  `llm_citation_status` already correctly holds the true original self-report. Re-running
  `verify_citations` on a reinserted finding overwrote the genuine self-report with the stale
  code-verified value. **Reproduced for real** over this exact 2-round chain: a reinserted
  finding's `llm_citation_status` flipped from `UNVERIFIED` (its true round-1 self-report) to
  `UNFETCHED` (a duplicate of `citation_status`) in round 2's actual report. Fixed by only
  backfilling `llm_citation_status` when it's still empty (`ed8e19a`, fixes #9).
- **#10 — OPEN, not fixed.** Round 2's real `report.md` scored 54/100 with 5 separate
  `CONFIRMED`-finding deductions for what was, on inspection of the actual findings, only 2
  distinct real-world unaddressed issues — because each `--prior`-reinserted finding
  (`STILL_OPEN`) sits alongside a freshly-discovered finding about the same fact from this
  round's own independent lens pass, under a different LLM-authored id, and often a different
  `label`/`citation_ref` format too. `quantify::summarize` sums `penalty(severity)` over every
  `CONFIRMED` finding with no dedup between them, so the same real issue gets double- (or
  worse) counted. A deterministic dedup key (matching `lens`+`section`+`label`, and separately
  matching `citation_ref`) was tried against this exact reproduction and **confirmed
  insufficient** — the fresh and reinserted copies of the same fact had different `label`
  values (`market_dynamics` vs. `incentive_integrity` in the reproduction) and different
  `citation_ref` formats between rounds, since both are LLM-authored per call, not stable
  identifiers. **This needs semantic (LLM-judged) matching to fix correctly, not a heuristic
  key — a heuristic patch was deliberately not applied, and the issue is left open with this
  root cause documented** rather than closed with a fix that only handles the reproduction
  case and silently fails on the next label/format variation.

## Cost detail

| Run | Outcome | Cost | Source |
|---|---|---|---|
| Pre-fix attempt (2 lens reviews before crash) | crashed, no usage printed | ~$0.15–$0.25 | estimated — 불확실, not measured |
| Post-fix round 1 | success | $0.4136 | `state.json` `cost_usd`, exact |
| Post-fix round 2 (`--prior`) | success | $0.3707 | `state.json` `cost_usd`, exact |
| **Total** | | **≈ $0.93–$1.03** | partly exact, partly estimated |

## Limitations and caveats

- **This is one review session's log, not a benchmark.** One document, one model (`haiku`),
  one 2-lens combination (`market_dynamics`, `financial_forensics`), one 2-round `--prior`
  chain. No comparison matrix, no cross-repo run, no repeat-run non-determinism check — unlike
  Code-Review-Loop's own `evals/` harness, there was no golden-set/promptfoo infrastructure run
  here, just direct execution and inspection of the actual output files.
  Code-Review-Loop's `evals/README.md` is the style reference, not a claim that this repo has
  equivalent eval coverage.
- **The pre-fix crash's cost is a real but unmeasured cost, not a placeholder.** It is not
  folded into the $0.7843 exact figure; it is reported separately as a range and marked
  uncertain, per the same convention.
- **#10 is a real, open, unresolved bug**, not a documentation gap — the score-inflation effect
  is measured on real output above, and the fix requires design work (semantic dedup) not yet
  done. It is not closed by this document.
- **Static-review findings (#2–#7) were not independently re-verified by a second real
  execution run beyond the one 2-round chain reported here** — #2, #3's reordering, #5, #6, and
  #7 are covered by unit tests added in their respective commits (see each commit's diff), not
  by a further live model run specifically targeting each one.
