# Package H — synthetic teaching replay

Baseline: `main / af8a07f`, clean checkout on 2026-09-20. This is a development
browser fixture, not a shipped application mode. No credentials, private notes,
provider configuration or real note store is read. No model or paid service is
called. Production search, editor, review and receipt components are mounted;
mock IPC fails closed on unsupported commands/inputs, and apply always refuses.
The fixture refuses to install in a native Tauri window. Normal app builds do not
import it. No default provider/model or retrieval strategy changed.

## Reproduce

From the repo root, run `npm run dev -- --host 127.0.0.1` (or use the existing
server on port 1420), then open:

- `http://127.0.0.1:1420/tests/browser/package-h.html`: desktop platform mode.
- `http://127.0.0.1:1420/tests/browser/package-h.html?platform=android`: simulated
  mobile platform responses in Chrome, **not** an Android/WebView device test.

The instructor layout stacks production panes at narrow widths. This does not
fix or claim to validate the full desktop App's three-pane layout at 360px; G's
clipped full-App screenshot remains a separate known limitation.

The browser runner requires an installed Playwright module and Chrome:

```sh
JODD_PLAYWRIGHT=/path/to/playwright/index.mjs node tests/browser/package-h.mjs
```

It allows only the fixture server origin, checks production controls, native
Tab/Enter/Space, exact folder search IPC without AI, empty/loading/error and retry,
pending-edit close/reopen, evidence passage focus, five replay lessons and refused
apply. It captures 360/800 light/dark for desktop and simulated mobile platforms.
CSP also prevents outbound fetches from a manually opened fixture. Synthetic
`example.test` source links are examples, not fetched source material.

## Lesson sequence (approximately 10 minutes; not a time-saving measurement)

1. **Find without AI.** Choose Try exact search, select This folder, and inspect
   the Thai result. The production list sends `search_notes` with exact label
   scope; browser replies are deterministic fixtures, **not execution of SQLite
   FTS**. Workspace DB tests cover the real local FTS/exact-folder contract.
   Try no matches, held loading, error and retry. No AI is required for this task.
2. **Grounded synthesis.** Choose the first lesson, copy the displayed source,
   open Action Items, select Action items and paste it. Preview the decision and
   action, then follow Evidence 1 by keyboard to its full passage.
3. **Missing information.** Repeat with Missing owner and date: neither is invented.
   Incomplete source withholds commitments and surfaces unresolved information.
   Quote matching only proves passage location. It cannot establish intent,
   ownership, or whether a proposal became a commitment. These are synthetic
   responses, not measured model quality.
4. **Boundaries.** Policy denial and Budget denial replay errors produced by the
   real Rust boundary functions. Expand receipts. They contain recorded local
   validation/admission timing and **zero provider attempts**, not a model trace,
   chain-of-thought, provider latency or token/billing measurement. Unknown cost
   is not zero. Recordings do not run the whole native command bridge.
5. **Regression.** In the editor, type, browse the empty folder, read the context
   notice, then Close note and reopen. The note keeps its own account/folder and
   save chain. Sources default to three readable host/path labels; expand and Tab
   through every source. See `packageH.test.ts` for the failure-before/fix-after
   tests, including an older save completing/rekeying before a queued newer save fails. The newer closed draft survives reopening. Prior C tests also cover serialization, aliases and late save replies.

Saving the replay preview intentionally reports read-only: a recorded receipt is
not a backend-held eligible review draft. The real F workflow still requires its
backend proposal, snapshot/version checks and explicit apply. This fixture does
not emulate a second authorization implementation or allow replayed results to
write real notes. The editable demonstration note exists only in browser memory.

## Recorded fixture provenance

`src-tauri/examples/teaching_replay.rs` uses the existing 36-case synthetic corpus,
production `meeting::{request,parse,envelope}`, Markdown renderer,
`policy::build_account_provider`, in-memory `budget::{Ledger,run_in,Attempt}` and
`receipts::{Store,run_in}`. The denied provider constructor and budget dispatch
continuation panic if reached, so generation fails if either boundary is bypassed.
No provider is constructed; no global ledger, config paths or note DB is opened.
The three response cases are `th-01`, `th-02`, `th-10`; inspect the full corpus for
its development/held-out split. This teaching replay must not be used to tune later
held-out comparisons. Labels are agent-authored and awaiting independent review.

Regenerate only from these synthetic sources:

```sh
cargo run --locked -q -p jodd --example teaching_replay > /tmp/teaching-replay.json
# After successful exit and inspection:
cp /tmp/teaching-replay.json tests/browser/teaching-replay.json
```

The JSON records corpus/meeting/policy/budget SHA-256 values and receipts' prompt
version. An artifact integrity test fails if those contracts change without a
regenerated recording. Run IDs, passage IDs and local timings change per run;
reproducibility means the same outcomes, not byte-identical timing/UUIDs. Raw text
in this teaching fixture is deliberately synthetic; production D receipt storage
continues to contain metadata only.

## Comparison and measurement

`node scripts/compare-meeting-routes.mjs` emits `pending approved routes and
outputs`; the checked-in `tests/evals/meeting-actions-v1/comparison.json` is that
report. It freezes corpus and prompt-module digests and all 12 held-out IDs. No
live results, independently reviewed labels or approved route outputs were supplied.
No quality/cost default is selected. FTS stays unchanged; there is no measured
retrieval-miss evidence to justify embeddings.

With **already collected and authorized** output files, pass a manifest path.
The harness validates provenance and all 36 output IDs, invokes only the existing
local Rust evaluator, then compares the fixed held-out rows across routes. It
retains failure IDs, language groups, reference-match precision/recall denominators
and abstention. Development failures also remain visible. Missing responses cannot
fall back to synthetic output. Scoring time is excluded from provider latency.
See the corpus README for the manifest shape. A manifest is supplied provenance,
not independently verified authorization or permission to collect anything.

Before claiming savings or choosing a route: independently review labels; authorize
routes and whole-workflow budgets; freeze corpus/prompt and thresholds; use repeated
runs over the same cases; randomize/counterbalance manual/non-AI and AI-assisted
work; record task time **including** human review/correction, acceptance criteria,
wrong actions and failures. Record all retries/enrichment and usage provenance,
priced as-of dates if available, and total cost per human-accepted result. Report
sample sizes and failure denominators separately by split/language. Human acceptance
n=0, correction time, provider latency, live usage/cost and time saved remain unknown.
There is no compliance, calibrated-confidence or quality-of-life claim.

## Verification boundary

Session 8 in the delivery plan records exact gates and local artifact paths.
Host tests + Chrome/mock IPC are distinct from packaged Tauri, actual Android,
live AI, remote sync and delayed Apple reconciliation. None of those live layers
were exercised. Scheduler, backend protocols, B permissions, D retention/privacy,
E reservations and F eligible-draft SQL guards were not modified.
