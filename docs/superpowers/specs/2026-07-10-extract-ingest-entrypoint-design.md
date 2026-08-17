# Extract ingest entry point + append mode — design

> Status: **design / approved** (2026-07-10). Relocates the "Extract lessons from
> source" trigger out of the account-management dropdown into a persistent,
> top-level entry point, and adds a destination picker so a source can be ingested
> into an **existing** note instead of always creating a new one. UI + data-flow
> only — no change to the LLM prompt, provider abstraction, or auto-linking.
> Follows on from a conversation reframing Extract as the "ingest" step of the
> Karpathy LLM-Wiki pattern (raw source → LLM → wiki page, append-only log).

## Problem

The 💡 "Extract lessons from a source" trigger currently lives inside the
account-panel dropdown (`Sidebar.svelte:1247-1252`), in the same 3-icon cluster as
⚙ Account settings and ✕ Remove account. Extraction is not an account-management
action — it is a content-creation workflow (conceptually "add this to my wiki"),
so burying it in account CRUD makes it hard to discover and implies a relationship
to account settings that doesn't exist.

Separately, `extract_lessons` ([lib.rs:3413](../../../src-tauri/src/lib.rs))
always creates a brand-new note in `Notes/__Extracts__`. There is no way to feed a
new source into an **existing** note — every ingest is a fresh, disconnected page,
which is a weaker version of the LLM-Wiki pattern's "ingest updates 10-15 existing
pages" step.

## Decisions (locked in brainstorming)

1. **Scope: UI placement + manual destination picker only.** Automatic
   cross-referencing / suggesting related notes (the full "auto-link" idea) is a
   separate, larger spec — deferred, see below. This spec only lets the user
   **manually** pick an existing note as the ingest target; no search-and-suggest
   logic, no prompt changes.
2. **New entry point: a persistent full-width row in the Sidebar**, below
   `sidebar-header` and above the account list — visible regardless of which
   account/folder is selected. Rejected: a 3rd icon crammed into the existing
   `sidebar-header` icon row (too little visual weight for the action's
   importance) and a new app-wide toolbar above the 3-pane layout (correct
   long-term home for a "search-level" global action, but new UI chrome that
   Jodd doesn't have today — out of scope for a placement-only pass).
3. **Old trigger removed entirely**, not kept as a secondary path. The
   account-dropdown loses the 💡 button; only ⚙/✕ remain.
4. **Destination picker allows any note in the current account**, not just notes
   already inside `Notes/__Extracts__`. The "wiki" a source gets ingested into is
   the whole account, not one folder.
5. **Append is pure concatenation — never restructures existing content.**
   New material (tags line, lessons markdown, meta, a fresh `<details>` source
   block) is appended after the existing body, verbatim. This mirrors the
   LLM-Wiki `log.md` "append-only, chronological" convention and is the only
   append strategy with zero risk of corrupting or reordering what's already
   there. A note ingested into multiple times ends up with multiple
   `<details>Source</details>` blocks, one per ingest, in order.
6. **`re_extract_lessons`** (the existing "re-extract from this note's own saved
   source" context-menu action, `NoteContextMenu.svelte:660-669`) is untouched —
   different flow (regenerates from a note's own preserved source), out of scope.

## Approach

### 1. UI: persistent Ingest entry point

`Sidebar.svelte`, new markup between `sidebar-header` (ends ~line 1062) and the
`<nav class="folder-list">` account loop:

```svelte
<button
  class="ingest-row"
  onclick={() => { if ($currentAccount) extractModalOpen.set(true); }}
  disabled={!$currentAccount}
  title="Ingest a source into your notes"
>💡 Ingest source</button>
```

Full-width row styled distinctly from folder rows (own background/padding, not a
`folder-item`). Uses the existing `$currentAccount` store — same gating pattern
already used by the "+ New folder" button (`Sidebar.svelte:1050-1053`): disabled
when no account is selected, otherwise targets whichever account is current. No
new state.

Remove the old trigger block entirely
(`Sidebar.svelte:1247-1252`, the 💡 button inside `.account-row-settings`).

### 2. Modal: destination picker

`LessonExtractModal.svelte` gains a two-option toggle at the top of the form:

- **New note** (default) — today's behavior, unchanged.
- **Append to existing note** — reveals a text input that calls the existing
  `search_notes(account_id, label: null, query)` command (`db.rs:657`, already
  FTS5-backed) as the user types, lists matches (title + folder), and lets them
  click one to select it as `target_uuid`. Selecting clears on toggling back to
  "New note".

Submit branches on the toggle:
- New note → calls `extract_lessons(account_id, source_text, title_override,
  request_id)` exactly as today.
- Append → calls the new `append_extract_lessons(account_id, target_uuid,
  source_text, request_id)` command (below). `title_override` does not apply in
  append mode — the target note's title is never changed.

### 3. Backend: `append_extract_lessons` command

New Tauri command in `lib.rs`, sibling to `extract_lessons`, reusing the same
cancellation-token bookkeeping and provider resolution:

```rust
#[tauri::command]
async fn append_extract_lessons(
    account_id: String,
    target_uuid: String,
    source_text: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<String, String>
```

Flow:
1. Register `CancellationToken` under `request_id` (identical to `extract_lessons`).
2. Resolve `account` + `provider` (identical).
3. **Before calling the LLM**, check the target note exists for
   `(account_id, target_uuid)`. If missing → remove the cancel token, return an
   error the modal surfaces inline ("target note not found"), and do **not**
   call the LLM — no wasted call, no fallback note needed since nothing was
   attempted yet. This is an existence check only — do not hold onto the fetched
   row for step 5 (see race note below).
4. Call `provider.extract(&source_text, cancel).await` (identical to
   `extract_lessons`, including the `Cancelled` short-circuit and the
   provider-error fallback path — see Error handling below). This can take
   seconds to tens of seconds.
5. On success, **re-fetch the target note fresh** (a second, separate lookup —
   not the step-3 snapshot) immediately before writing:
   `new_body = lessons::markdown::append_to_note_body(&existing.body_html,
   &envelope, &source_text)`, then `state.db.apply_local_edit(&target_uuid,
   &account_id, &existing.title, &new_body, &existing.label)`. Title and label
   are passed through **unchanged** — this call never renames or moves the
   target note. `apply_local_edit` already handles FTS reindex, tag/edge
   re-derivation, and `clean → dirty` (db.rs:933) — no extra bookkeeping needed
   here.
   - **Why re-fetch instead of reusing the step-3 snapshot**: the LLM call in
     step 4 can run long enough for the target note to change in the meantime
     (e.g. the user has it open in the editor and autosave fires). Building the
     appended body from a stale pre-LLM snapshot and then overwriting
     `body_html` with it would silently discard that concurrent edit. Re-fetching
     right before the write shrinks the race to the (negligible, single-writer,
     mutex-guarded) window between the re-fetch and the `apply_local_edit` call
     itself, instead of the whole LLM round-trip.
   - If the re-fetch finds the note gone (deleted while the LLM call was in
     flight): treat like provider failure — create the fallback source note,
     return an error. Do not silently create a new note under the old title as
     a substitute target; the user asked to append to a specific note that no
     longer exists, so surface that plainly.
6. Remove the cancel token, return `target_uuid`.

### 4. `markdown.rs`: `append_to_note_body`

New pure function, sibling to `assemble_note_body`:

```rust
/// Append a new ingest's content to an existing note body. Pure concatenation —
/// never edits or reorders what's already there. The tags line, lessons
/// markdown, optional meta section, and a fresh `<details>` source block are
/// built exactly like `assemble_note_body` and appended after `existing_body`.
pub fn append_to_note_body(existing_body: &str, envelope: &ExtractEnvelope, source: &str) -> String
```

Implementation: build the same tags-line + lessons + meta + source-block
fragment `assemble_note_body` builds (factor the shared fragment-building out of
`assemble_note_body` into a private helper both functions call), and return
`format!("{existing_body}{fragment}")`. No fragment is inserted mid-body; no
existing `<details>` block is touched.

## Error handling

- **Target note missing/wrong account**: validated before the LLM call (step 3
  above) — fails fast, no wasted LLM call, existing note (if any, in a different
  account) is never touched.
- **LLM call fails after target validated**: identical to today's
  `extract_lessons` failure path — `create_fallback_source_note` creates a new
  note in `Notes/__Extracts__` preserving the raw paste, and the command returns
  an error. The append **target note is never modified** (nothing is written to
  it until the LLM succeeds), so a failed append cannot corrupt existing content.
  This fallback fires regardless of which mode (new/append) the user had
  selected — the source must never be lost.
- **User cancels**: identical `ExtractError::Cancelled` short-circuit as
  `extract_lessons` — no fallback note, textarea keeps the source for retry.

## Edge cases

- **Appending into a note that has no prior `<details>` source block** (a regular
  hand-written note, not a previous extract): works unchanged — concatenation
  doesn't require one to already exist.
- **Appending into a note the user has open in the editor at the same time**:
  `apply_local_edit` already flips `sync_state` and bumps `local_version`; the
  editor's existing reactive note store picks up the change like any other
  local-first write (no new mechanism needed).
- **Very long notes accumulating many source blocks over time**: accepted
  trade-off of the append-only design (decision 5); no size cap in this spec.

## Testing

- **Unit** (`markdown.rs`): `append_to_note_body` — existing content byte-for-byte
  preserved as a prefix; new tags/lessons/meta appended after it; a note appended
  twice ends up with two `<details>Source</details>` blocks in order; appending
  with empty existing body behaves like a fresh `assemble_note_body` call.
- **Live verification** (per CLAUDE.md convention, scoped to a test folder):
  - Sidebar shows the new "💡 Ingest source" row regardless of selected
    account/folder; disabled with no account selected.
  - Account dropdown no longer shows a 💡 button (only ⚙/✕ remain).
  - New-note mode: unchanged behavior, spot-check against current build.
  - Append mode: search finds an existing note, append succeeds, prior content
    intact, new tags/edges re-derive (visible in sidebar tag pills / connections
    panel), note round-trips to Gmail/LocalFS normally on the next sync tick.
  - Append with an invalid/deleted target: inline error, no LLM call made (check
    logs), no fallback note created (nothing was attempted).
  - Provider failure during append: fallback note created in `__Extracts__`,
    target note unchanged.

## Scope / files

- `src/lib/components/Sidebar.svelte` — new ingest row; remove old 💡 button.
- `src/lib/components/LessonExtractModal.svelte` — destination toggle + search
  picker + branch on submit.
- `src-tauri/src/lib.rs` — new `append_extract_lessons` command.
- `src-tauri/src/lessons/markdown.rs` — new `append_to_note_body` (+ shared
  fragment-building helper extracted from `assemble_note_body`) + tests.
- No schema changes. No changes to `lessons/prompt.rs`, `provider.rs`,
  `resolve.rs`, `http.rs`, `claude_code.rs`, or `extract_lessons` itself.

## Deferred (not built)

- **Auto-link / auto-suggest**: the LLM (or a search pass) automatically finding
  related existing notes and either suggesting them as an append target or
  inserting `[[wikilinks]]` into the new content. This spec only covers
  *manual* target selection. Separate spec.
- **`index.md`-equivalent**: a summary rollup page listing all Extracts with
  one-line descriptions. Not built here.
- **Schema doc for create-vs-edit rules**: no prompt-level guidance is added for
  the LLM to decide when to prefer updating vs. creating — that decision stays
  100% manual (the user's toggle) in this spec.
- **Orphan/staleness lint pass**: unrelated to this spec; tracked separately
  against the existing backlinks/edges data.
