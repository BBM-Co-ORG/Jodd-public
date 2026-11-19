# Meeting actions v1 — synthetic, offline fixtures

Protocol fixed before any provider comparison (2026-09-20). This corpus is
agent-authored synthetic material with explicit reference labels, not private
notes, human acceptance evidence, or recorded provider output. Development and
held-out IDs are fixed in cases.json. Held-out cases must not tune prompts during
later authorized provider comparisons; this initial implementation can inspect
all fixtures for harness correctness. Do not call an LLM judge or provider here.

Predeclared gates: 100% schema/evidence-address validity on accepted fixture
responses; zero invented owners, dates or commitments in critical reference
cases; 100% rejection of invalid evidence/empty/oversized input; >=95% supported
reference action/decision recall and >=95% reference abstention on future model
outputs. Report every failure and denominator separately by split/language.
Exact quotation checks are necessary but do not prove semantic entailment: a
model may misclassify a proposal or negation. Human review remains required.

Run: `cargo run --locked -p jodd --example meeting_actions_eval`.
The harness calls the production preparation/parser/grounding/renderer locally.
No provider is instantiated, settings loaded, network requested or note written.
Recorded responses are synthetic fixtures, so a green replay is validator coverage,
NOT measured model quality. Regressions also run in the Rust workspace tests.

Metrics: reference passage retrieval recall, reference field precision/recall,
abstention, validation failures and local harness latency (median/p95). This
workflow reads one explicitly supplied source in full; retrieval recall is input
passage coverage, not FTS recall. Oversized inputs are refused in full to avoid
hiding a later contradiction. Explicitly truncated input must be marked incomplete
in the source; unknown omissions cannot be detected. Report human acceptance,
edit effort and cost per accepted result as unknown with n=0 until an authorized
human/provider evaluation; no zero-token, free-subscription or time-saving claim.

A future comparison must freeze corpus/prompt digest and thresholds, record
provider/config/date/repeats, reserve E's whole-workflow budget, retain D's usage
provenance and include retries/enrichment plus human review/correction time.
Human labels should be independently reviewed before such a comparison. No live
comparison or route/default selection is authorized by this file.

To score previously authorized responses without invoking any provider, append a
local JSON path to the example command. Shape: `{ "en-01": {"items": [...],
"incomplete": false}, ... }`, one response per corpus ID (raw response strings also
accepted). Missing IDs fail, failures exit nonzero, and semantic mismatches against
the reference are reported even when quotations are real. All cases must be
present, including expected refusals. This does not authorize collecting them.

Split: 24 development / 12 held out; each contains English, Thai and mixed text.
Held out includes complete decisions/tasks, relative dates, cancellations and
explicitly truncated sources (four cases per language), so its positive recall
and abstention denominators are both nonzero. Development includes missing
owner/date, contradiction, proposals, injection, invalid fields/evidence, empty
and oversized sources. A literal `[INCOMPLETE]` or `[TRUNCATED]` marker must be
acknowledged; the UI can also mark a source incomplete. Unknown omissions remain
undetectable. Existing HTML sources retain raw HTML in the Source block and use
paragraph-preserving decoded text for provider evidence; pasted text stays literal.

## Package H offline route comparison

`node scripts/compare-meeting-routes.mjs` reports pending approved routes/outputs.
`comparison.json` is the checked-in no-route report, not a benchmark. To score
already-authorized local outputs, supply a JSON manifest:

```json
{
  "corpus_sha256": "copy the current value from comparison.json",
  "prompt_sha256": "copy the current value from comparison.json",
  "routes": [{
    "id": "route-a-repeat-1",
    "approval_reference": "reference to the actual collection authorization",
    "provider": "record the approved provider",
    "model": "record the exact selected model",
    "configuration": "record relevant configuration and whole-workflow budget",
    "collected_at": "record collection date",
    "repeat": 1,
    "outputs": "responses.json"
  }]
}
```

`outputs` is relative to the manifest and must contain all 36 response IDs in F's
format. Use one route entry per collected repeat. The prompt digest hashes the
whole `meeting.rs` contract (prompt/schema/validation), not just prose. Corpus or
contract mismatch refuses scoring. Approval fields are supplied provenance, not
independently verified authorization; this tool never collects outputs. It scores
through the production Rust evaluator and compares the same 12 held-out IDs,
retaining every failure and per-language denominator. Missing output IDs are an
error, never a fallback. Human acceptance, live latency/usage/cost and time saved
remain unknown; no automatic route selection. See `docs/TEACHING-DEMO.md` for the
manual/non-AI baseline and human review/correction measurement protocol.
