# Auto-link ingest (roadmap #5) — design

> Status: **design / approved** (2026-07-20). Implements
> [docs/LLM-WIKI-GRAPHIFY-ROADMAP.md](../LLM-WIKI-GRAPHIFY-ROADMAP.md) item #5
> (Auto-link/auto-suggest) plus two new ingest entry points: sourcing an
> Extract from an existing note, and retroactively linking an existing note
> into the graph without an LLM rewrite. The "digest" idea (a separate,
> time-delayed reconcile/synthesize pass) raised in the same conversation is
> explicitly **out of scope** here — see Deferred.

## Problem

Karpathy's actual ingest step (confirmed by re-reading his gist directly,
not the roadmap's paraphrase): *"The LLM reads the source, discusses key
takeaways with you, writes a summary page in the wiki, updates the index,
updates relevant entity and concept pages across the wiki, and appends an
entry to the log. **A single source might touch 10-15 wiki pages.**"*

Today's Extract only does the "write a summary page" half. It never touches
other existing notes automatically — cross-linking is entirely manual (the
`[[` picker), and "Append to existing note" (Phase 1) only ever touches the
one note the user explicitly picked. There's also no way to feed an
*already-existing* Jodd note back through the pipeline, either to re-file it
(distill + link) or to retroactively connect it (link only, no rewrite).

## Decisions (locked in brainstorming)

1. **Digest is a separate, later feature.** It's a time-delayed reconcile
   pass over accumulated material, structurally different from ingest-time
   linking (which fires immediately, per-source). Building it now would mean
   designing against a linking foundation (this spec) that doesn't exist
   yet. Deferred.
2. **Two new entry points, not one feature with a mode switch:**
   - **#2a** — Extract gains a **source** toggle (Paste text | Pick existing
     note) alongside its existing destination toggle. Picking an existing
     note feeds its body to the LLM as `source_text`, exactly like pasted
     text; the source note itself is never modified. For cleaning up /
     re-filing content already in Jodd.
   - **#2b** — a new context-menu action, **"🕸 Link into wiki"**, that runs
     the linking core (below) directly against a note's *current* body — no
     LLM distillation, no rewrite of its meaning. For connecting existing
     content without touching what it says.
3. **Linking runs automatically as part of every relevant flow** — not a
   separate manual trigger the user has to remember to press. #2a's linking
   step runs automatically after every Extract (new-note or append,
   including today's plain-paste flow — this generalizes, it doesn't gate
   behind the new source toggle). #2b is inherently a deliberate,
   user-initiated action (right-click → Link into wiki), so no "automatic"
   framing applies there — it always runs when invoked.
4. **Relatedness search: hybrid, FTS narrows, LLM picks** — cheap
   deterministic keyword extraction (Rust, no LLM call) narrows the note
   corpus to a candidate pool via the existing `search_notes` (FTS5), then
   one LLM call judges which candidates are genuinely related and what (if
   anything) to add to each. Avoids a second LLM round-trip just for
   keyword extraction, and avoids sending the whole corpus to the LLM.
5. **New/current note's own outgoing links: automatic, no confirmation.**
   Every candidate the LLM judges related gets a `[[wikilink]]` inserted
   into the note being ingested/linked — low risk, it's the note already
   being written or explicitly targeted.
6. **Touching an EXISTING note: confirm first, append-only.** Every
   candidate the LLM decides also warrants a mention gets proposed as a
   one-line addition to *that* note — shown in a review UI
   (`DupReviewModal`-shaped: list of candidates, checkbox per item,
   Confirm/Skip) before anything is written. Confirmed items are applied as
   pure concatenation (mirrors Phase 1's "Append to existing note" — never
   restructures existing content). Declining review doesn't block or delay
   the primary Extract/link action; it's purely additive and skippable.
7. **No new note "type"/entity-vs-concept distinction.** The research found
   implementations that categorize pages (entity/concept/synthesis) to
   drive the create-vs-update call. Not adopting that structure here — the
   LLM makes the per-candidate judgment directly against plain note
   title+snippet, no formal typing needed. (Note `type` was already
   considered and dropped earlier this session for lack of a consumer;
   still true here.)
8. **No explicit supersession/staleness marking.** LLM Wiki v2's critique
   (silently-overwritten facts should instead be linked+timestamped+marked
   stale) is a real idea but a distinct, larger feature — out of scope.

## Approach

### 1. Shared core: candidate search + judgment

New function, `src-tauri/src/lessons/autolink.rs`:

```rust
pub struct LinkTarget { pub uuid: String, pub title: String }
pub struct ProposedAppend { pub uuid: String, pub title: String, pub addition_text: String }
pub struct LinkSuggestions {
    pub auto_links: Vec<LinkTarget>,
    pub proposed_appends: Vec<ProposedAppend>,
}

pub async fn suggest_links(
    provider: &dyn LessonProvider,
    db: &Db,
    account_id: &str,
    exclude_uuid: Option<&str>,
    text: &str,
    cancel: CancellationToken,
) -> Result<LinkSuggestions, ExtractError>
```

**Step 1 — deterministic candidate search (no LLM call):** extract
candidate search terms from `text` via a hand-rolled heuristic (matching
this crate's no-regex style): split on whitespace/punctuation, drop a short
English+Thai stopword list, keep tokens that are either capitalized
mid-sentence (proper-noun-shaped) or repeated ≥2 times in the text, cap at
8 terms. Run each term through the existing `Db::search_notes(Some(account_id),
None, term)`, merge results by uuid (excluding `exclude_uuid` if given —
e.g. don't suggest linking a note to itself), rank by number of distinct
terms that matched, cap the candidate pool at 20.

**Step 2 — one LLM judgment call:** send the source `text` plus each
candidate's `(uuid, title, first ~200 chars of body_html stripped to text)`
to the provider via a new `LessonProvider` trait method (mirroring `extract()`'s
shape — same `CancellationToken` race, same `ExtractError` variants):

```rust
async fn suggest_links(&self, source: &str, candidates: &[CandidateSummary], cancel: CancellationToken) -> Result<LinkSuggestionsEnvelope, ExtractError>
```

implemented by both `HttpProvider` and `ClaudeCodeProvider`, same as
`extract()` today. The prompt asks the LLM to return, per candidate: `related:
bool`, and if related, `should_append: bool` + `addition_text: Option<String>`
(a single sentence, written to read naturally appended — e.g. "Also
discussed in relation to `[[new-note-slug]]`."). Every `related: true`
candidate becomes a `LinkTarget` (decision 5); every `should_append: true`
candidate additionally becomes a `ProposedAppend` (decision 6).

**Step 3 — apply.** Callers insert `auto_links` as a single trailing line —
`<p>Related: [[Title-uuid8]], [[Title2-uuid82]], …</p>` — appended to the
note being written (slug-link format matching the existing `[[` picker's
output, `db.rs`'s `note_slug`/`uuid_short`). A dedicated trailing line, not
woven into prose, mirrors how the existing tags line and citation
`<details>` block are already both structural/trailing annotations rather
than inline body edits — consistent with this codebase's established
"never restructure existing prose" instinct. Callers present
`proposed_appends` via `LinkSuggestionsModal.svelte` for confirmation.

### 2. #2a — source picker in Extract

`LessonExtractModal.svelte` gains a second toggle, **Source: Paste text |
Pick existing note**, using the same search-and-select UI the destination
toggle already has. Selecting a note sets `source_text` to that note's
`body_html` (stripped to text, matching how the existing `<details>Source
(verbatim)</details>` block already stores raw pasted text — consistent
representation).

After `extract_lessons`/`append_extract_lessons` succeeds (both paths,
regardless of which source mode was used — decision 3), call the new
`suggest_wiki_links` Tauri command with the finished body, passing
`exclude_uuid` = the note's own uuid (the newly-created uuid for "New note"
mode, the `target_uuid` for "Append to existing note" mode — same value
already used by `check_duplicate_citations` today, so this is never a new
lookup) so the note can't suggest linking to itself. Auto-insert `auto_links`
into the just-created/updated note via **one follow-up `apply_local_edit`
call, after the primary save has already completed** — kept as a separate
step rather than folded into `extract_lessons`/`append_extract_lessons`'s
own body-assembly, so this addition can't introduce a regression risk into
those two already-reviewed, delicate functions. Then show
`LinkSuggestionsModal` if `proposed_appends` is non-empty. A provider/LLM
failure on this step is a **soft failure** — logs and skips linking, never
blocks or undoes the primary Extract save (matches the duplicate-citation
warning's non-blocking precedent).

### 3. #2b — "Link into wiki" context-menu action

New entry in `NoteContextMenu.svelte`, mirroring `re_extract_lessons`'s
existing pattern (`NoteContextMenu.svelte:288,663`). Calls
`suggest_wiki_links` directly against the target note's current
`body_html`, with `exclude_uuid` = that same note's own uuid (self-exclusion,
same reasoning as #2a above) — no LLM distillation step at all. Auto-inserts
`auto_links` into the target note's own body via `apply_local_edit`
(append-only, same treatment as decision 5 applied to "the note being
linked" rather than "the note being written"),
then shows the same `LinkSuggestionsModal` for `proposed_appends`.

### 4. New Tauri commands

```rust
#[tauri::command]
async fn suggest_wiki_links(account_id: String, text: String, exclude_uuid: Option<String>, request_id: String, state: State<'_, AppState>) -> Result<LinkSuggestions, String>

#[tauri::command]
fn apply_wiki_link_appends(account_id: String, appends: Vec<ConfirmedAppend>, state: State<'_, AppState>) -> Result<(), String>
```

`suggest_wiki_links` is `async` and registers/deregisters a
`CancellationToken` under `request_id` in `AppState.in_flight_extracts`,
identical bookkeeping to `extract_lessons` — reuses existing infrastructure
rather than inventing new cancellation plumbing. `apply_wiki_link_appends`
loops the confirmed list, calling `apply_local_edit` once per note
(concatenating `addition_text` onto the existing body), inside one
transaction.

## Error handling

- **`suggest_wiki_links` fails or times out** (2a): soft failure, no
  auto-links inserted, no modal shown, the primary Extract result is
  unaffected — the user still gets their distilled/filed note, just without
  suggestions this time.
- **`suggest_wiki_links` fails** (2b): surfaced as an inline error in the
  context-menu action's toast/notice (nothing was written yet — this is a
  pure suggest-then-confirm flow, so failure before confirmation leaves the
  target note untouched).
- **User cancels mid-suggestion**: identical `ExtractError::Cancelled`
  short-circuit as `extract_lessons` — no partial writes, same pattern.
- **`apply_wiki_link_appends` partially fails** (e.g. one target note was
  deleted between suggestion and confirmation): skip that entry, apply the
  rest, report which ones succeeded/failed back to the modal rather than
  aborting the whole batch.

## Edge cases

- **Candidate pool is empty** (no related notes found): `suggest_links`
  returns empty `auto_links`/`proposed_appends` — no modal shown, nothing
  inserted, completely silent success. Matches "no citations found, and
  that's fine" precedent from the citations feature.
- **2b run twice on the same note**: not idempotent-guarded — a second run
  could re-suggest (and, if confirmed again, re-append) the same
  connections. Accepted trade-off for this pass (matches the "no manifest/
  dedup tracking" gap noted in the researched prior art too) — a future
  pass could check for an existing outgoing edge before re-suggesting the
  same target, but isn't required for v1.
- **A candidate note is edited between suggestion and confirmation**: the
  append still applies to whatever the note's current body is at confirm
  time (`apply_local_edit` always operates on current state) — the
  suggested `addition_text` might reference slightly stale context, but
  this mirrors the same acceptable risk window Task 6's duplicate-citation
  check already has.

## Testing

- **Unit** (`lessons/autolink.rs`): keyword-extraction heuristic (stopword
  filtering, capitalization/repetition signal, 8-term cap); candidate
  merge/dedup/exclude-self/rank-by-match-count logic — all pure functions,
  testable without a live LLM or DB.
- **Live verification** (per this codebase's convention for LLM-touching
  and Svelte-UI features):
  - Extract a source related to 2-3 existing notes (shared vocabulary);
    confirm the new note gets auto-inserted `[[wikilinks]]` to them, and the
    review modal proposes reasonable one-line additions to those notes;
    confirm accepting an addition appends it (existing content byte-for-byte
    preserved as a prefix, matching `append_to_note_body`'s existing test
    convention).
  - Right-click an existing, previously-unlinked note → "Link into wiki" →
    confirm it gains outgoing links and proposes appends to related notes,
    with its own body's meaning unchanged (no LLM rewrite).
  - Cancel mid-suggestion (both entry points) → confirm no partial writes.
  - Empty candidate pool (an isolated topic with nothing related) → confirm
    silent no-op, no modal.

## Scope / files

- **New**: `src-tauri/src/lessons/autolink.rs` — keyword extraction,
  candidate search/merge, `suggest_links` orchestration, tests.
- **Modify**: `src-tauri/src/lessons/provider.rs` — new `LessonProvider`
  trait method `suggest_links` + `LinkSuggestionsEnvelope`/`CandidateSummary`
  types.
- **Modify**: `src-tauri/src/lessons/http.rs`, `claude_code.rs` — implement
  the new trait method (new prompt, same call/cancellation shape as
  `extract()`).
- **Modify**: `src-tauri/src/lib.rs` — `suggest_wiki_links`,
  `apply_wiki_link_appends` commands; hook the auto-link step into the
  success path of `extract_lessons`/`append_extract_lessons`.
- **Modify**: `src/lib/components/LessonExtractModal.svelte` — source
  toggle (2a) + post-success suggestion flow.
- **New**: `src/lib/components/LinkSuggestionsModal.svelte` — review UI for
  `proposed_appends`, shared by 2a and 2b.
- **Modify**: `src/lib/components/NoteContextMenu.svelte` — "🕸 Link into
  wiki" action (2b).

## Deferred (not built)

- **Digest** (roadmap follow-on, decision 1) — a separate, time-delayed
  reconcile/synthesize/heal-orphans pass, distinct mechanism from this
  spec's immediate ingest-time linking. Needs its own brainstorm once this
  ships and there's real linked data to reconcile.
- **Note type/entity-vs-concept typing** (decision 7).
- **Explicit supersession/staleness marking on contradiction** (decision 8).
- **Idempotency guard against re-linking the same pair** (edge case above).
- **Auto-run at ingest with zero possibility of override** — this spec
  keeps linking as an automatic-but-skippable step (auto-links always
  applied, appends always confirmable/skippable); a fully silent/forced
  mode was not requested and isn't built.
