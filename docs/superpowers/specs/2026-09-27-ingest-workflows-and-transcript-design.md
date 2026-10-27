# URL ingest honors the selected workflow + a Transcript workflow

> Status: design approved in chat 2026-09-27; this spec awaits review.
> Builds on [2026-09-15-url-ingest-design.md](2026-09-15-url-ingest-design.md)
> and roadmap #2 (Summarize / Action items / Expand bullets).

## Problem

In the Extract modal, selecting a link under **Sources from links** disables
the four workflow buttons and runs `ingest_sources`, whose map step is
hard-wired to `provider.extract` (`ingest/run.rs:232`). Whatever the user
picked, a link always produces **Key points**. The only on-screen hint says
the selection "is ignored", not what runs instead, so it reads as a bug.

There is also no workflow that turns a spoken source into readable text while
keeping all of it. A user who wants a YouTube talk "transcribed and tidied"
(ถอดความแล้วเรียบเรียง) has no option: every existing workflow condenses.

## Decisions (from the brainstorm)

| # | Decision |
|---|---|
| D1 | One ingest pipeline with a mode parameter — not a second `ingest_transcript` command. Fetch, cancel, progress, duplicate detection, the Sources block and the write stage stay shared. |
| D2 | **Clean = rewrite for readability, keep everything.** Remove fillers and false starts, add punctuation, break into paragraphs/headings, keep the source language. No summarizing, no dropping content. |
| D3 | **Action items stays paste-only.** With a link selected its button is disabled with an explanation; its preview/apply flow is not routed through ingest. |
| D4 | Transcript without Clean is **LLM-free**: the fetched text, paragraphed. |
| D5 | Transcript on pasted text is Clean-only — raw would copy the box into a note. |

## Modes

`ingest::run::IngestMode`, passed into `run_ingest` and set by the command:

```rust
pub enum IngestMode {
    KeyPoints,                          // today's behavior, unchanged
    Workflow(WorkflowKind),             // Summarize | ExpandBullets only
    Transcript { clean: bool },
}
```

`IngestMode::Workflow(ActionItems)` and `Workflow(CleanTranscript)` are
refused at the command boundary (`ingest_sources` returns an error before any
fetch), mirroring `run_llm_workflow`'s existing ActionItems refusal. The
multi-source Re-extract path (`IngestInput::Stored`) always passes
`KeyPoints` — out of scope here.

| Mode | Map (per usable source) | Reduce (≥ 2 sources) |
|---|---|---|
| KeyPoints | `provider.extract` (unchanged) | `provider.synthesize` (unchanged) |
| Workflow(k) | `provider.run_workflow(k, map_input(..), &[])` | **Concatenate** per-source results under each source's heading (`combine_digests`), no notice. `SYNTHESIS_SYSTEM_PROMPT` is written for key points; reusing it would turn a summary back into key points. |
| Transcript { clean: false } | No LLM. `paragraphize(src.text)` | Concatenate |
| Transcript { clean: true } | `transcript::clean(provider, src.text)` — chunked, see below | Concatenate |

`combine_digests` gains a flag (or a sibling) so the concatenation used as a
*design choice* carries no "synthesis failed" notice, while the existing
failure fallback keeps it.

The Transcript modes do **not** apply `map_input`'s 60 000-char cap: the
point is the whole source. Clean works through chunks; raw needs no LLM.

## Clean transcript — `llm/transcript.rs` (new)

One unit, used by both ingest and the pasted-text path, so chunking and the
length guard exist in exactly one place.

```rust
pub struct CleanOutcome { pub markdown: String, pub tags: Vec<String>, pub raw_chunks: usize }
pub async fn clean(provider: &dyn LlmProvider, text: &str, cancel: CancellationToken)
    -> Result<CleanOutcome, ExtractError>;
pub fn chunk(text: &str, max_chars: usize) -> Vec<&str>;
pub fn paragraphize(text: &str) -> String;
```

- **New `WorkflowKind::CleanTranscript`** with `CLEAN_TRANSCRIPT_SYSTEM_PROMPT`
  in `llm/prompt.rs`: the text between markers is a spoken-word transcript
  (possibly one section of a longer one); rewrite it for reading; remove
  fillers, repetitions and false starts; add punctuation and paragraphs; add
  `##` headings only at clear topic changes; **do not summarize, omit,
  reorder or add content**; keep the source's language; same JSON envelope.
  Tag reuse is appended as for Summarize. Every exhaustive `match` on
  `WorkflowKind` (prompt lookup, provider impls, frontend `WorkflowKind`
  type) gets the new arm.
- **Chunk size derives from the output cap, not a constant.** The default
  `output_tokens` is 4 096 (`budget::Settings`), and Thai tokenizes far worse
  than English, so an 8 000-char Thai chunk would be truncated.
  `max_chars = clamp(output_tokens × 1.0, 2 000, 12 000)` — ≈ 4 000 chars at
  the default. The factor is a named constant, and the plan must measure it on
  one Thai and one English transcript before fixing it (gotcha #7: empirical,
  not derived).
- **Chunk boundaries:** YouTube text arrives as one whitespace-joined line
  (`youtube.rs:134`), so there are no newlines to split on. Cut at the last
  sentence terminator (`.?!。` or a Thai space run) before `max_chars`; fall
  back to the last whitespace; never mid-word.
- **Attempt budget is checked before the first call.** `max_attempts`
  defaults to 16 per request; a 67 000-char transcript is ~17 chunks. If
  `chunks + 1 > max_attempts`, refuse with *"This source needs N AI calls;
  your AI limits allow M. Raise 'Max attempts' in AI limits, or use
  Transcript without Clean."* — never start and stop half-way.
- **Length guard, per chunk:** if the cleaned chunk's character count is
  below `MIN_CLEAN_RATIO` (0.6) of the input chunk, treat it as a summary the
  model was told not to write. Keep the **raw** chunk in its place and count
  it in `raw_chunks`. The note then carries one notice: *"N of M sections
  could not be cleaned without losing content and are shown as-is."* A chunk
  that errors (not cancel) takes the same fallback. Nothing is silently lost —
  the same principle as `REDUCE_FAILED`.
- Title comes from the fetched source title (or `title_override`), not from
  the chunks' envelopes. Tags are the de-duplicated union across chunks.

## Raw transcript — `paragraphize`

No LLM. Groups the text into paragraphs of ~600 chars at the same boundaries
`chunk` uses, drops caption noise markers (`[Music]`, `[Applause]`,
`[เพลง]`, `♪`) and writes plain `<p>` blocks. For a web page the fetched text
already has structure; `paragraphize` keeps existing blank-line breaks and only
re-flows paragraphs longer than the target.

## Pasted text

`run_workflow_note_into` special-cases `WorkflowKind::CleanTranscript`: it
calls `transcript::clean` instead of a single `run_workflow`, so a long pasted
transcript is chunked the same way. Append mode (`append_llm_workflow_note`)
does the same. Transcript-without-Clean is not offered for pasted text (D5).

## IPC

`ingest_sources` gains `mode: IngestModeDto`:

```ts
type IngestModeDto =
  | { kind: 'key_points' }
  | { kind: 'workflow'; workflow: 'summarize' | 'expand_bullets' }
  | { kind: 'transcript'; clean: boolean };
```

Missing `mode` deserializes to `key_points`, so an older frontend build keeps
today's behavior.

## UI — `LessonExtractModal.svelte`

- A fifth workflow button, **Transcript**. When selected, a checkbox
  **"Clean (rewrite with AI, keep everything)"**, default on. With no link
  selected it is forced on and disabled, with the D5 reason as its title.
- With a link selected: all buttons stay enabled except **Action items**,
  which is disabled with *"Action items work on pasted text only."* If Action
  items was already selected when a link gets selected, the selection falls
  back to Key points in the same reactive turn (gotcha #28).
- The note "URL ingest always produces a digest — the workflow selection above
  is ignored" is removed.
- Hint text and title follow the selected workflow as today; the submit button
  reads *Ingest N sources* as today.
- Progress lines for Clean show the chunk: *"Cleaning youtube.com — section
  3 of 12"* (a new `IngestStage::Cleaning` with a `chunk` / `chunks` pair).

## Error handling

| Case | Result |
|---|---|
| Chunk count exceeds attempt budget | Refused before any LLM call, message names both numbers and the raw alternative |
| One chunk shrinks below the ratio or errors | Raw chunk kept, one notice in the note |
| Every chunk falls back | Note is written (all raw) with the notice — same as raw mode, never an empty note |
| Cancel mid-clean | Nothing written (existing cancel contract) |
| Source fetch fails | Unchanged (Sources list records it) |
| `Workflow(ActionItems)` via IPC | Refused at the command, before fetch |

## Testing

Rust (`cargo test --workspace`):
- `run_ingest` per mode with the existing `FakeProvider`: KeyPoints call
  counts unchanged; Workflow calls `run_workflow` and never `synthesize`;
  raw Transcript makes **zero** provider calls; Clean makes one call per chunk.
- `chunk`: never splits a word; respects `max_chars`; Thai text without
  terminators splits at spaces; one-line YouTube shape.
- `clean`: a fake that returns a short summary triggers the raw fallback and
  the notice; attempt-budget refusal happens with zero provider calls.
- `paragraphize`: strips noise markers, keeps all words (word multiset equal
  after removing markers).
- `ingest_sources` refuses `ActionItems` before fetch; missing `mode`
  deserializes to KeyPoints.
- `workflow_system_prompt` covers `CleanTranscript` with and without tags.

Frontend (`npx vitest run`, mounted component per gotcha #28):
- Selecting a link keeps Summarize enabled and disables Action items; an
  active Action items falls back to Key points in the same turn.
- The `ingest_sources` invoke carries the selected mode.
- Transcript with no link: Clean checkbox checked and disabled.

Live pass (after merge gates): the reporter's link
`https://www.youtube.com/watch?v=CiIK5SrSuQQ` in Transcript raw, Transcript
Clean and Summarize; record chunk count, fallback count and wall time.

## Out of scope

- Account tag vocabulary for the Key points map step (the `&[]` at
  `ingest/run.rs:232`) — separate small fix.
- Action items from links (D3).
- Translation.
- Multi-source Re-extract honoring a mode.
