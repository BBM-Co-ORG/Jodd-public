# Extract filing — design

> Status: **design / approved in brainstorming** (2026-09-15). Decouples an
> Extract note's *provenance* (where it came from) from its *location* (where
> it lives): new extracts land in a real `Notes/Inbox` folder, an LLM proposes
> an existing folder to file each one into, and `__Extracts__` becomes a
> virtual smart folder instead of a place.
>
> This is **Spec A** of two. **Spec B — URL ingest** (fetch a pasted URL
> deterministically, then extract with a tool-less LLM, NotebookLM-style)
> builds on the destination model decided here and is not designed yet. See
> "Relationship to other work".

## Problem

Every Extract writes into one hardcoded, protected folder,
`Notes/__Extracts__` (`WORKFLOW_FOLDER_EXTRACTS`). The notes themselves are
ordinary `notes` rows that sync to Apple Notes like any other; only the folder
is special. Four things are wrong with that, measured or read from code:

1. **The folder records how a note arrived, not what it is about.** A Settrade
   debugging note, a note on metabolic hormones and a Thai side-income plan
   sit side by side. A wiki organises by topic; this organises by ingestion
   mechanism.
2. **It is a pile nobody triages.** Measured 2026-09-12 through `jodd-mcp` on
   `gmail:kaiwan@bbmedia.co.th`: 11 notes in `Notes/__Extracts__`, **5 of them
   records of an LLM failing to read a bare YouTube URL.** (The junk itself is
   an input problem — roadmap #0b and Spec B — but the folder is where it
   silently accumulates.)
3. **Outlook accounts cannot use Extract at all** (read from code; not yet
   exercised live). `extract_note` and `append_extract_note` both call
   `refuse_write(Write::Folders)`, and
   `Capabilities::for_backend(Microsoft).writes.folders` is permanently
   `false` (gotcha #12). `append_extract_note` refuses even though appending
   to an existing note creates no folder.
4. **The folder can never go away.** `isProtectedWorkflowFolder`
   (`Sidebar.svelte`) hides rename/move/delete for every `__name__` folder, so
   once Jodd stops writing there the user still cannot remove it.

Two adjacent facts shape the design:

- **Provenance is already body-derived in one place.** The Re-extract menu
  item is gated on `hasSourceBlock(body)` — the `<summary>Source
  (verbatim)</summary>` marker — not on the folder.
- **Re-extract does not replace.** `re_extract_note` calls `extract_note`
  again, producing a second note; the post-re-extract repaint hardcodes
  `Notes/__Extracts__`.

## Decisions (locked in brainstorming)

1. **The machine proposes at capture time; the human confirms or defers.**
   Capture is the cheapest moment for an LLM to classify (it has just read
   the content) and the most expensive for a person. A proposal is one click
   to accept; ignoring it leaves the note in the Inbox. Rejected: organise
   entirely later (an Inbox that depends on discipline is the pile we have),
   and a plain "current folder" default with no suggestion (predictable, but
   does not solve "extracts live in the wrong place").
2. **The Inbox is an ordinary folder, `Notes/Inbox`.** It shows as "Inbox" on
   an iPhone, is created on first use, and its backlog is visible through the
   sidebar's existing per-folder count. Rejected: the Notes root (already
   crowded — 584 notes at the root of the iCloud account — so a backlog is
   invisible and uncountable), and a system `__Inbox__` (renders literally on
   iPhone and inherits `__Extracts__`'s Outlook problem).
3. **Suggestions choose only from folders that already exist.** Rust verifies
   the returned path is one it offered. The taxonomy stays the user's; an LLM
   that could mint folders would reproduce the tag sprawl roadmap #0 already
   measured (27 distinct tags from 4 extracts).
4. **Existing `__Extracts__` is unprotected, not migrated.** No note is moved
   automatically — a bulk remote relocation across every device and account
   is exactly what "derive, don't migrate" exists to avoid. The folder becomes
   renamable/deletable, and a "Suggest folder" action works on any note, so
   the 11 existing extracts are filed by the same mechanism as new ones.
5. **Re-extract still creates a new note, now beside its source.** It lands in
   the source note's folder (not `__Extracts__`), so the two can be compared
   and one deleted. Replacing in place is out of scope: it would overwrite
   user edits made after extraction and hits iCloud's per-note refusals
   (gotcha #24).
6. **Folder suggestion is its own LLM call, `suggest_folder`.** One code path
   serves both "after Extract" and the "Suggest folder" menu action. Rejected:
   a field on `ExtractEnvelope` (cannot serve existing notes, so a second
   path would be needed anyway); piggybacking on the auto-link call
   (`autolink::suggest_links` returns early with no LLM call when
   `extract_keywords` finds nothing, which is the common case for Thai text,
   and router Decision 6 plans to gate that call per account); a no-LLM
   "neighbours vote" over FTS results (weak on Thai for the same keyword
   reason, and blind to folder names that hold no notes yet).
7. **Post-Extract calls run sequentially, not in parallel.** Nothing records
   whether two agent-CLI subprocesses may run concurrently, and presets are
   empirical (gotcha #7). The note is already written, so nobody waits on
   either call; order is `suggest_note_folder`, then `suggest_wiki_links`.
8. **The post-Extract suggestion counts as part of the Extract the user
   triggered**, exactly as auto-link does today. When roadmap #0b's
   per-account gate on untriggered LLM calls is built, it must cover
   `suggest_folder` as well as auto-link.
9. **Workflow-folder protection stays, with a retired list.** `Notes/__Claude__`
   is a `system_workflow` folder that `jodd-mcp` uses as its write allowlist
   (`mcp_write_scope.json`); renaming it would silently break MCP writes.
   Protection therefore keeps applying to every `__name__` folder except
   those in `RETIRED_WORKFLOW_FOLDERS = ['__Extracts__']`. The reservation of
   the `__name__` syntax in `validate_folder_segment` is unchanged.

## Approach

### Data flow

```
Extract ─▶ write note to resolve_destination() synchronously (SQLite)
             ─▶ close modal, navigate to the returned label
             ─▶ suggest_note_folder(account, uuid)       (then)
             ─▶ suggest_wiki_links (unchanged)
                     │
                     ▼
   editor breadcrumb:   Inbox → Trading?   [Move] [Keep here]
                     │ Move
                     ▼
   move_notes_batch (Write::Relocate), optimistic with rollback
```

"Suggest folder" in the note context menu calls the same command and feeds
the same chip. Accepting a proposal is an ordinary relocation: a `label`
change plus the dirty flag the worker already pushes (Gmail relabel, Outlook
`/move`, iCloud `Folders`). Every backend that can write notes can relocate
them (`writes.relocate` is `true` on Gmail, LocalFs, Microsoft and iCloud).

### Destination resolution

`llm::filing::resolve_destination(db, account_id, can_create_folders)`, used
by every path that creates an extract note:

1. `Notes/Inbox` exists in `folders` and is not `deleted_pending` → use it.
2. Otherwise, if the account can write folders → create it with
   `Db::create_folder_local_new` (kind derives to `user`) and use it.
3. Otherwise → `"Notes"`, the root, which every backend can write (on
   Microsoft by well-known name, gotcha #12).

A `deleted_pending` Inbox row falls through to step 3, not step 2:
`create_folder_local_new`'s `ON CONFLICT DO NOTHING` would leave the row
pending deletion and file the note into a folder about to disappear.

Outlook presents folders rooted under `Notes/` (`present_under_notes_root`),
so a user-made Inbox that Jodd has scanned is found as `Notes/Inbox` there
too. A duplicated leaf name becomes `Inbox~abc123`, which step 1 does not
match, so such an account files into the root.

**This adds a hardcoded `"Notes"` root to the five folder-side sites gotcha #9
lists** (`Notes/Inbox`, and the root fallback). It follows the same convention
as `ensure_workflow_folder`, but it is one more place roadmap #0c must route
through `effective_notes_label()` when per-account labels are finished — that
item's checklist should name `resolve_destination`.

## Components

New logic lives in a new module, `src-tauri/src/llm/filing.rs`, which answers
one question — where does a note belong — in the same way `autolink.rs`
answers "what does it link to". `lib.rs` gains thin commands only.

### Rust

**`llm/filing.rs` (new)**

```rust
pub fn resolve_destination(db: &Db, account_id: &str, can_create_folders: bool)
    -> SqlResult<String>;

pub struct FolderSuggestion { pub path: String, pub reason: Option<String> }

pub async fn suggest_folder(
    provider: &dyn LlmProvider,
    db: &Db,
    account_id: &str,
    uuid: &str,
    cancel: CancellationToken,
) -> Result<Option<FolderSuggestion>, ExtractError>;
```

- **Candidates:** `Db::list_folders(account_id)` minus rows that are
  `deleted_pending`, `kind = 'system_workflow'`, `Notes/Inbox`, or the root
  `Notes`. An empty candidate list returns `Ok(None)` **without calling the
  provider.**
- **Note text:** title plus `db::strip_html_to_text(body_html)`, truncated to
  6 000 characters.
- **Validation:** the returned `folder` must equal a candidate path exactly
  and must differ from the note's current `label`; otherwise `Ok(None)`,
  logging the rejected value.

**`llm/provider.rs`**

- `FolderSuggestionEnvelope { folder: Option<String>, reason: Option<String> }`
  with a hand-written strict `JSON_SCHEMA`: both properties in `required`,
  nullable via `["string", "null"]`, `"additionalProperties": false` — the
  shape `codex exec --output-schema` demands (see `ExtractEnvelope`'s doc
  comment). Pinned by `folder_schema_agrees_with_the_envelope`.
- New trait method, **with no default implementation**, so every provider —
  and both test fakes (`ask/run.rs`, `llm/autolink.rs`) — decides explicitly:

  ```rust
  async fn suggest_folder(
      &self,
      note_text: &str,
      folders: &[String],
      cancel: CancellationToken,
  ) -> Result<FolderSuggestionEnvelope, ExtractError>;
  ```

**`llm/prompt.rs`** — `FOLDER_SUGGESTION_SYSTEM_PROMPT`: choose one path from
the provided list, copied verbatim, or `null` when none fits; `reason` is one
short sentence.

**`llm/http.rs`** — targeted refactor: generalise `send_link_suggestion_request`
into `send_json_request(system, user_content, cancel)`, used by both
`suggest_links` and `suggest_folder`, so there are two chat-completions
request builders rather than three. Responses parse through
`parse_envelope_lenient` (gotcha #7b).

**`llm/agent_cli.rs`** — `suggest_folder` calls `run_json` with
`FolderSuggestionEnvelope::JSON_SCHEMA`, like `suggest_links`.

**`llm/markdown.rs`** — `SOURCE_MARKER` becomes `pub` so the extract smart
folder and the body builder share one definition.

**`lib.rs`**

| Command | Change |
|---|---|
| `extract_note` | Refuses `Write::Notes` only. Destination from `resolve_destination`. Returns `{ uuid, label }`. Body refactored into an internal function that takes an explicit destination, so `re_extract_note` can supply its own. |
| `append_extract_note` | Refuses `Write::Notes` only. |
| `re_extract_note` | Destination = the source note's `label` if that folder still exists (not `deleted_pending`), else `resolve_destination`. Returns `{ uuid, label }`. |
| `create_fallback_source_note` | Uses `resolve_destination`. |
| `suggest_note_folder(account_id, uuid, request_id)` | **New.** Read-only, so no `refuse_write`. Registers a `CancellationToken` in `in_flight_extracts` and removes it on every exit path. Resolves the provider through `resolve_provider_for_account`. Returns `Option<FolderSuggestion>`. |
| `list_extract_notes(account_id)` | **New.** Backs the Extracts smart folder. |

`WORKFLOW_FOLDER_EXTRACTS` loses its production callers. `ensure_workflow_folder`
keeps its tests (it is the subject of the Microsoft reconciliation regression
test) and stays available for any future workflow folder.

**`db.rs`** — `Db::list_extract_notes(account_id)`: `notes` rows for the
account, not `deleted_pending`, whose `body_html` contains `SOURCE_MARKER`,
newest first — the same fully-virtual shape as `list_orphaned_notes`.

### Frontend

| File | Change |
|---|---|
| `stores/notes.ts` | `folderSuggestions`: a store keyed `"<account_id>:<uuid>"` → `{ path, reason }`, never persisted. `selectedSmartFolder`'s kind union gains `'extracts'`. |
| `LessonExtractModal.svelte` | Navigate to the returned `label` instead of `Notes/__Extracts__`. After a new-note extract, call `suggest_note_folder` and then `runAutoLinkSuggestions`, neither awaited by the UI. A failed suggestion is logged, never shown. |
| `NoteEditor.svelte` | A chip in the breadcrumb, `→ <folder>` with `[Move]` `[Keep here]`, `reason` as its tooltip. Visibility is derived from `$selectedNote.uuid` and `folderSuggestions` **in the same reactive expression** (gotcha #28). `[Keep here]` deletes the entry. |
| shared helper `moveNotesOptimistic` | Lifted out of `NoteContextMenu.svelte`'s `moveTo` so the chip and the menu share one snapshot → optimistic → rollback implementation. |
| `NoteContextMenu.svelte` | New "Suggest folder" item, gated on `Write::Relocate`, snapshotting the note's fields before `onClose()` like `linkIntoWiki`. Re-extract drops its `canWriteFolders` gate and repaints the returned `label`. |
| `Sidebar.svelte` | "💡 Extracts" smart-folder row beside Orphaned/Stale. `isProtectedWorkflowFolder` returns `false` for leaves in `RETIRED_WORKFLOW_FOLDERS`. |
| `App.svelte` | `loadSmartFolderNotes` dispatches `'extracts'` → `list_extract_notes`. |

## Error handling / edge cases

### Destination

| Situation | Behaviour |
|---|---|
| User renames or deletes `Notes/Inbox` | The next extract recreates it. Intended: the Inbox always exists. |
| `Notes/Inbox` is `deleted_pending` | Root, for this extract (see "Destination resolution"). |
| Two devices create the Inbox on **Gmail** before syncing | Safe: `create_label` answers 409 by adopting the existing label (`gmail/wire.rs`). |
| Two devices create the Inbox on **iCloud** before syncing | **Unverified.** No name-based dedupe was found in `backend/icloud/`; two folders titled "Inbox" may result. The same exposure already exists for `__Extracts__`. Covered by the live pass. |
| **Outlook** with a duplicated `Inbox` leaf (`Inbox~abc123`) | No exact `Notes/Inbox`, so root. |
| Folder or note insert fails | Error returned to the modal; `sourceText` is preserved; no half-written note. |

### Suggestion

The same command serves two callers with different UX needs, so the command
returns distinct outcomes (`Ok(Some)`, `Ok(None)`, `Err`) and the **caller**
decides whether to be quiet.

| Situation | After Extract (automatic) | From the context menu (explicit) |
|---|---|---|
| Provider `Disabled` / not configured (`NotConfigured`) | Skip silently | "No LLM provider is configured for this account" |
| No candidate folders | No LLM call; nothing shown | "No folders to choose from yet" |
| `null`, an unknown path, or the current folder | No chip; rejected value logged | "No better folder found" |
| Malformed envelope (after lenient parse), timeout, transport error | No chip; logged | Error shown |
| Cancellation | Token removed from `in_flight_extracts` on every exit | Same |

### Chip and move

| Situation | Behaviour |
|---|---|
| Result arrives after the user opened another note | Stored by `account:uuid`; the chip appears when that note is reopened. |
| Note moved or deleted before the result arrives | The chip renders only while the note exists and its `label` differs from the proposal. |
| Proposed folder disappeared meanwhile | The account's folder list is **snapshotted when the chip appears**, not re-read on click — re-reading would await IPC before the optimistic write (local-first doctrine). A proposal naming a folder absent from that snapshot is removed on click with a message. A folder deleted **after** the chip appeared is not seen: the move proceeds and `move_notes_batch`, which does not validate its target, is the authority. |
| Move fails | Rolled back by `moveNotesOptimistic`; the chip stays. |
| App quits | Proposals are lost by design; "Suggest folder" asks again. |

### Doctrine checks

- **Local-first.** The note is written before any LLM call; accepting a
  proposal is an optimistic local write. Nothing on the editing path waits
  on the remote.
- **Gotcha #6.** Every step starts from a user action and returns over the
  IPC call awaiting it — with one exception, on Microsoft: the sync worker's
  create push **rekeys the note's uuid** on its own (gotcha #16), usually
  while the LLM call is in flight, and nothing moves the open editor to the
  new uuid. The suggestion flow copes rather than routing that change to the
  frontend: the outcome carries the note's current uuid; the proposal is
  recorded under both the uuid asked about and the current one (clearing
  either removes both); and `move_notes_batch` resolves aliases, so a move
  issued under the old uuid still reaches the live row.

## Testing

### Rust

| Unit | Cases |
|---|---|
| `filing::resolve_destination` (`temp_db`) | Existing Inbox is used · absent + can create → a `dirty_new` row with kind **`user`** · absent + cannot create → `"Notes"` and **no row inserted** · `deleted_pending` Inbox → `"Notes"` |
| candidate filtering | Excludes `deleted_pending`, `system_workflow`, `Notes/Inbox`, `Notes` |
| `filing::suggest_folder` (a fake provider that counts calls) | Valid path → `Some` · unknown path → `None` · current label → `None` · `null` → `None` · empty candidates → **provider called 0 times** |
| `Db::list_extract_notes` | Marker present → included · absent → excluded · `deleted_pending` → excluded |
| `FolderSuggestionEnvelope` | `folder_schema_agrees_with_the_envelope` · HTTP (mockito): a fenced envelope still parses |
| `send_json_request` refactor | The existing `suggest_links` HTTP tests pass **unmodified** |
| re-extract destination | Source label kept · source folder gone → `resolve_destination` |

Tauri commands taking `State` have no command-level harness in this repo, so
"Outlook is no longer refused" is covered in tests only at the level of
`resolve_destination(can_create_folders = false)` and the removed refusal;
the proof is the live pass.

### Frontend (vitest; mount components, per gotcha #28)

| File | Cases |
|---|---|
| `reExtract.test.ts` (updated) | Repaints the returned `label`, not `Notes/__Extracts__` |
| `noteEditorFolderSuggestion.test.ts` (new) | Proposal for A, switch to B → chip gone **in the same reactive pass** · back to A → chip shown · proposal equals label → no chip · failed move → rollback, chip remains |
| `extractModalFiling.test.ts` (new) | Navigates to the returned label · `suggest_note_folder` invoked **before** `suggest_wiki_links` · a failing suggestion surfaces no error |
| `noteContextMenuSuggestFolder.test.ts` (new) | Explicit call answering `None` or `NotConfigured` shows a message |
| `sidebarWorkflowProtection.test.ts` (new) | `__Extracts__` offers rename/delete · **`__Claude__` stays protected** · the Extracts row invokes `list_extract_notes` |

### Gates — the commands CI runs

```bash
cargo test --workspace
node scripts/gen-changelog.mjs
npx vitest run
npx svelte-check --threshold error
npm run build
```

### Live pass

Before starting: confirm the binary under test by mtime and `ps`, and that
only one Jodd is running — `tauri dev` shares SQLite with the installed app,
so two instances are two sync workers.

| Backend | Verify by observation |
|---|---|
| **Gmail** | First Extract creates `Notes/Inbox`; the iPhone shows an "Inbox" folder (**unverified:** any interaction with Gmail's system `INBOX` label or Apple's IMAP folder handling) · accepting the chip moves the note on the iPhone |
| **Outlook** (`kaiwan.h@live.com`) | **Extract succeeds** · lands in the root, or in a hand-made Inbox that already holds a note · a move into a hand-made folder reaches Notes.app · the folder chip appears on a fresh Outlook extract, and Move reaches Notes.app |
| **iCloud** (`kaiwan@me.com`) | The new note is clean **when re-checked more than 20 minutes after writing** · the duplicate-Inbox race: two instances creating the Inbox before syncing — record the outcome |
| **LocalFs** | An `Inbox` directory appears |
| **All** | `__Extracts__` can be renamed and deleted · `jodd-mcp` `create_note` into `__Claude__` still works · extract three Thai sources and record whether each proposal is sensible (qualitative; no numeric threshold) |

## Scope / files

- **New:** `src-tauri/src/llm/filing.rs`; the four frontend test files above.
- **Changed:** `src-tauri/src/llm/{mod,provider,prompt,http,agent_cli,markdown}.rs`,
  `src-tauri/src/ask/run.rs` (test fake), `src-tauri/src/lib.rs`,
  `src-tauri/src/db.rs`, `src/lib/stores/notes.ts`,
  `src/lib/components/{LessonExtractModal,NoteEditor,NoteContextMenu,Sidebar}.svelte`,
  `src/App.svelte`, `src/lib/components/reExtract.test.ts`.
- **Docs after landing:** `docs/LLM-WIKI-GRAPHIFY-ROADMAP.md` (record the
  shipped change), gotcha #10's text (it names `WORKFLOW_FOLDER_EXTRACTS` as
  Extract's output folder — correct it in place, never renumber).
- **Unchanged:** `jodd-mcp` (its `__Claude__` folder stays protected);
  `src-tauri/examples/` (nothing there references Extract).

## Relationship to other work

- **Roadmap #0b (Extract input router).** Independent and complementary: the
  router stops junk before it is written; this decides where legitimate
  extracts go. Its per-account gate must cover `suggest_folder` (Decision 8).
- **Spec B — URL ingest (next).** Will reuse `resolve_destination` and the
  suggestion flow unchanged. Its open design question is recorded here so it
  is not lost: fetch deterministically in Rust and hand the text to a
  tool-less LLM, rather than grant a web-fetch tool to an agent CLI — a
  proposed reversal of the router spec's Decisions 1 and 4, to be argued
  there with the security review Decision 4 asks for.
- **Roadmap #0 (tag vocabulary).** Unaffected; folders and tags stay
  orthogonal (a folder answers "where it is kept", a tag "what it is about").

## Found in passing — not addressed here

- **Auto-link inserts links without confirmation.** `runAutoLinkSuggestions`
  appends a `Related: [[…]]` line to the note body immediately; only
  `proposed_appends` wait for the user. In the measured vault a Settrade note
  links to two unrelated failed-YouTube extracts; this mechanism explains it
  (the specific occurrence was not traced).

## Found after landing — fixed

- **Extract put the LLM's raw HTML into the note body (fixed 2026-09-15).**
  `build_ingest_fragment` pushed `md_to_html(lessons_markdown)` and
  `md_to_html(meta)` straight into the body, and `md_to_html` passes raw HTML
  in the markdown through untouched. The editor renders a body with
  `innerHTML`. The Tauri CSP (`script-src 'self'`) blocks `<script>`, `on*=`
  handlers and `javascript:` URLs, so this never reached `invoke` — but a
  remote `<img>` beacon (`img-src https:`), an inline-styled overlay
  (`style-src 'unsafe-inline'`) and junk markup that syncs out to Apple Notes
  all got through. Read from code, raised by the Spec B brainstorm: URL ingest
  will feed the LLM page text a third party controls. Both fields now go
  through `render_llm_markdown` = `md_to_html` → `taskify_checklists` →
  `sanitize_note_html`, the pipeline `jodd-mcp`'s write tools already used, so
  it covers new-note Extract, append and re-extract at once. **Behaviour
  change:** GFM tasklists in Extract output become tickable Jodd task rows
  instead of `disabled` checkboxes. **Residual, not addressed:** the shared
  allowlist keeps `style` on `th`/`td` unfiltered (markdown table alignment),
  so a table cell can still carry arbitrary inline CSS; narrowing it touches
  `jodd-mcp` and the `is_replace_safe` strict list, so it belongs with Spec B's
  security review.

## Deferred (not built)

- An LLM proposing **new** folders (Decision 3).
- Replacing a note's content on Re-extract (Decision 5).
- A triage screen for the Inbox beyond the chip and the context-menu action.
- Configurable Inbox name or per-account destination settings.
- Persisting proposals across restarts.
