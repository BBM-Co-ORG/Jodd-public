# Jodd — project history (archive)

> Moved out of `CLAUDE.md` on 2026-08-10. This file is the **record of what
> was built and why**; `CLAUDE.md` keeps only what still changes how you should
> work on the code today. Nothing here was edited — sections are verbatim.
> Read this when you need the reasoning behind a design that already shipped.

---

## Current status (2026-07-30 — Ask Jodd)

**Ask Jodd** — an in-app, multi-turn, ephemeral chat over the local SQLite
cache, roadmap item #7 from
[docs/LLM-WIKI-GRAPHIFY-ROADMAP.md](LLM-WIKI-GRAPHIFY-ROADMAP.md) and
Feature 1 of
[HANDOFF-2026-07-29-tier1-copilot.md](superpowers/HANDOFF-2026-07-29-tier1-copilot.md),
via [spec](superpowers/specs/2026-07-29-ask-jodd-design.md) +
[plan](superpowers/plans/2026-07-30-ask-jodd.md).

A per-turn, four-stage pipeline (`src-tauri/src/ask/`): (1) a pure-SQL
pre-filter (`pool.rs`) unions FTS hits over `ask::terms::extract_query_terms`
(a new question-shaped keyword extractor — the existing
`autolink::extract_keywords` only keeps capitalized/repeated tokens and is
useless on a one-sentence question, Thai worst of all), the folder subtree
when folder-scoped, and a recency prior by `last_remote_modified_at`, deduped
and capped at `CANDIDATE_POOL_MAX = 400`; (2) a compact per-candidate catalog
line (`catalog.rs`); (3) LLM call 1 picks `uuid8`s from the catalog via a
lenient hex-token scan, not JSON (`catalog.rs` + `prompt.rs`); (4) LLM call 2
answers from the selected bodies, HTML-stripped and per-note-truncated before
the total budget is applied (`context.rs`), citing sources as
`[[<slug>-<uuid8>]]`. `run.rs` orchestrates all four stages; `RECENCY_K = 150`,
`MAX_SELECTED_NOTES = 12`, `MAX_NOTE_CHARS = 20_000`,
`MAX_CONTEXT_CHARS = 120_000` are defined once in `ask/mod.rs`. Retrieval
re-runs every turn against the accumulated conversation. Nothing is
persisted — no new table, no sidecar, no write to `notes` — closing the modal
discards the conversation.

Tauri commands `ask_jodd` / `cancel_ask`, with `AppState.in_flight_asks:
Mutex<HashMap<String, CancellationToken>>` mirroring `in_flight_extracts`.
`AskAnswer` carries `notes_in_scope` / `notes_considered` / `notes_used` so
the UI can show the honesty line **"N in scope → N considered → N read"** —
a heavily thinned pool is visible, not inferred from a weak answer.

**App-level LLM provider, with a per-account cascade.** Ask Jodd is
cross-account, so no single account's provider is the right owner. New
`src-tauri/src/app_llm_config.rs` (JSON config in the Tauri config dir, API
key in the keychain under `llm_api_key::__app__`) mirrors `oauth_config.rs`
exactly. `accounts::LlmProviderKind` gains a `Disabled` variant, and
**`LlmProviderKind::None` changes meaning from "unconfigured" to
"inherit"** — every `LlmConfig` field is already `#[serde(default)]`, so
every existing `accounts.json` parses unchanged and every current account
becomes an inheritor, which is the intended upgrade behavior.
`llm::resolve::resolve_app_provider()` — always the app-level provider,
independent of the `apply_to_accounts` toggle — is what Ask Jodd uses.
`llm::resolve::resolve_provider_for_account()` implements the cascade
(inherit / explicit override / `Disabled` refuses) for Extract and auto-link,
replacing the old `resolve_provider`. Account Settings relabels the empty
provider choice from "None" to "Use app default" and adds an explicit
"Disabled" option — required, not cosmetic, since the old label now states
the opposite of what the value does.

**Two known limitations, measured against the live vault, not assumed.**
The §5.1 SQL pre-filter is the recall ceiling of this design: on the
6,655-note flat test account, a conceptual question whose
wording matches no note and whose target isn't recent can be missed — the
"N in scope → N considered → N read" line exists specifically to make that
visible. Embeddings are the named successor, not dismissed, but blocked
structurally rather than by effort: agent-CLI providers (`claude -p`,
`codex`, …) expose no embedding endpoint, so an embedding index would work
only for HTTP providers and split the feature's behavior by provider type.

## Earlier status (2026-07-16 — LLM Wiki / Graphify session)

Three independent tracks from [docs/LLM-WIKI-GRAPHIFY-ROADMAP.md](LLM-WIKI-GRAPHIFY-ROADMAP.md)
(roadmap items #1, #2, #4), implemented via
[spec](superpowers/specs/2026-07-14-wiki-graphify-bundle-1-2-4-design.md) +
[plan](superpowers/plans/2026-07-14-wiki-graphify-bundle-1-2-4.md), on branch
`claude/jodd-llm-wiki-graphify-35997f`.

**Structured citations** — a new `rel='cites'` row type in the existing `edges`
table (no schema migration needed for the type itself; see the backfill note
below), derived from every note body on every write via a hand-rolled URL
scanner (`db::extract_urls` — no `regex` crate). Surfaced as a "📎 Sources"
group in the editor's Connections panel (`note_citations` command), plus a
soft duplicate-source warning in the Extract modal (`check_duplicate_citations`
pre-flight command, never a hard block — "Continue anyway" always available).
Jodd-local only, like tags/edges — never round-trips to Apple Notes.
Migration #15 (`DELETE FROM edges;`) forces a one-time repopulate on upgrade
so pre-existing notes' citations backfill immediately, not just on next edit
(same pattern as migration #13's precedent — `edges` is fully derived, so
clearing it is always safe).

**Smart Folders** — two fixed, per-account views: "🔍 Orphaned" (zero incoming
`[[wikilink]]` backlinks) and "🕰 Stale" (untouched 30+ days, hardcoded
threshold). **Fully virtual** — deliberately NOT a `folders` table row (despite
`folders.kind` already reserving `'smart_query'` for exactly this) to stay out
of Gmail-label sync semantics entirely; a separate `selectedSmartFolder` /
`smartFolderNotes` store pair in `notes.ts`, mutually exclusive with regular
folder/tag selection. `Db::list_orphaned_notes` / `list_stale_notes` (db.rs).
Read-only view — no rename/move/delete/context-menu.

**`jodd-mcp`** — a new read-only MCP server exposing `search_notes` and
`note_connections` (Jodd's graph, from any Claude Code session) via a brand
new Cargo **workspace** (see "Project structure" above) — `src-tauri` is no
longer a standalone crate. Calls `jodd_lib::db::Db` methods directly, no Tauri
runtime. Uses `rmcp` 2.2 (the plan's original `0.16` pin didn't resolve on
crates.io by the time this landed — SDKs move fast; check current version
before assuming). Manual one-time setup via `claude mcp add`, see
[jodd-mcp/README.md](../jodd-mcp/README.md). **Not part of the Tauri bundle** —
converting `src-tauri` into a workspace member silently broke two hardcoded
`src-tauri/target/...` paths in `.github/workflows/release.yml` (rust-cache
config + macOS bundle-verification step); both fixed in the same branch. If
you touch the workspace layout again, grep `.github/workflows/` for
`src-tauri/target` first.

### Earlier — 2026-06-12 — productivity-features session, v0.14.x

All shipped to `main` (pushed). Highlights:

**Tier 0 — correctness**
- **Attachments**: capture / preserve / render / stale-body safety-net, stored
  as SQLite BLOBs (gmail.rs `multipart/related`; db.rs migration #9; NoteEditor
  `hydrateAttachments`).

**Tier 1**
- **Search-as-index**: SQLite **FTS5**, `tokenize='trigram'` so Thai is
  substring-searchable, over title + HTML-stripped body. Derived on every write
  + backfilled (migration #10). `search_notes` with a **scope selector** (folder
  / account / **all accounts**). Inline #hashtags are searchable too.
- **Recently Deleted / Trash**: `list_trashed_notes` + `restore_note` (untrash +
  relabel), revision-vs-genuine-deletion filter, note-list-style UI + context
  menu. Scope kept to `gmail.modify` over `mail.google.com` (no permanent
  delete, narrower access) — both are classified **Restricted** by Google,
  so the choice narrows the verification story but doesn't avoid CASA by
  itself; see `docs/DISTRIBUTION.md`.
- **Richer text toolbar**: underline / strike / heading (`<h2>`) / ordered list;
  format buttons `onmousedown preventDefault` to keep editor focus.
- **Outline / nesting**: Tab / Shift-Tab indent — list items nest via
  execCommand (nested `<ul>`, round-trips); other lines (incl. checklists)
  indent by margin on the nearest LINE block. Enter on a checklist row
  continues the list (new task, same indent) / exits when empty; nested
  checklists **roll up** (a parent auto-ticks once all subtasks are done, and
  vice-versa). Editor-focus fixes: arrow keys no longer (a) get stolen by the
  note-list nav handler, (b) get captured by the `[[` picker inside an existing
  link.

**Tier 2 #6 — Tags: cutover from sidecar → inline #hashtag**
- Tags now live as inline `#hashtag` in the note **body** (single source of
  truth, round-trips to Apple). `note_tags` is a FULL-REPLACE derivation of body
  hashtags on every write (`reconcile_tags_from_body`); one-time
  `migrate_tags_to_body` injects legacy sidecar tags into bodies;
  `sync_tag_state` disabled. Chip add/remove edit the body; sidebar Tags render
  as compact collapsible **pills**; **cross-account** tag filter (scope
  selector); rename/delete tag **rewrite every carrying body** (HTML-aware) so
  they stick + round-trip.

**Tier 2 #5 — Fact-schema edges + backlinks**
- General **`edges`** table (migrations #11–13): `mentions` (`[[wikilinks]]`),
  `child_of` (note→folder), `tagged` (note→#tag) — derived on every write +
  backfilled.
- **Slug links**: `[[<title-slug>-<uuid8>]]` — unique (uuid id) + durable
  (re-derived from the round-tripping UUID) + readable + rename-safe; resolved by
  id, with a `[[` **autocomplete picker** (plain `[[Title]]` still works).
- **Connections panel** (→ links to / ← linked from) + a 🕸 local **graph view**
  modal (radial, colour-coded, clickable). Editor **context bar** (account ·
  folder · copyable slug).

**Cross-cutting fixes**: Apple-Notes IMAP sync-confusion recovery (toggle Notes
off/on — see memory), account-tagged + greppable `TRASHED`/`UNTRASHED` logs,
editor caret revert (the real culprit was the Windows mouse pointer, not the
caret), spurious-save guard (`userEdited`).

> **Doctrine note:** tags + edges follow the same model — derived from the body
> (which round-trips), indexed in SQLite, never stored as a sidecar/flag Apple
> would drop. See "Compatibility tiers".

### Earlier — 2026-06-09 architectural pass + Pin (v0.1.2)

**Content Extraction** (landed v0.16.1, hardened in v0.16.2, internal module now
`llm`):
LLM-backed paste-and-extract workflow that turns mixed unformatted source
text (Claude/ChatGPT conversation dumps, transcripts, debugging sessions,
articles, meeting notes, anything) into structured extract notes. Lives in
a Jodd-managed "system workflow folder" stored as `Notes/__Extracts__`
(kind='system_workflow' per migration #14), displayed as just "Extracts"
(with a 💡 icon) under a Workflows group in the sidebar after marker-strip.
Source text preserved verbatim in a collapsible `<details>` block at the
bottom of every extracted note, enabling re-extraction and verification
without re-pasting.

Internal naming was migrated to match the user-facing vocabulary on
2026-07-27: module `lessons` → `llm`, trait `LessonProvider` → `LlmProvider`,
commands `extract_lessons`/`re_extract_lessons`/`append_extract_lessons` →
`extract_note`/`re_extract_note`/`append_extract_note`. The original
churn-minimization decision was made when the module hosted a single
workflow; it now hosts two (Extract, auto-link). A multi-preset agent-CLI
provider layer is designed but not yet built — see
[the design spec](superpowers/specs/2026-07-27-agent-cli-llm-providers-design.md).
**`lessons_markdown` and `meta_lessons_markdown` were
deliberately NOT renamed — they are JSON keys in the LLM wire contract
(`prompt.rs`), not internal names.** User-facing labels ("Extract",
"Extracts", `__Extracts__`) are unchanged.

The reserved `__name__` syntax (any folder matching `__*__`) is documented
as Jodd-managed; legacy `Notes/Lessons` or `Notes/Extracts` folders from
before the standardization are treated as regular user folders.

- LLM provider abstraction: trait + two impls (HTTP for any OpenAI-
  compatible endpoint; subprocess for `claude -p`). Per-account config in
  accounts.json; API keys in OS keychain under `llm_api_key::{account_id}`.
- Cancellation propagated end-to-end (v0.16.2): the modal's Cancel button
  invokes `cancel_extraction(request_id)` which fires a `CancellationToken`
  stored in AppState.in_flight_extracts; the provider's tokio::select!
  branch unwinds — HttpProvider drops the in-flight reqwest future,
  the agent-CLI provider calls child.start_kill so the subprocess stops
  consuming Claude Code subscription quota. A cancelled extract does NOT
  create the fallback source note (user actively chose to abort).
- Output is markdown-bodied (LLMs produce dramatically cleaner markdown
  than HTML); pulldown-cmark with GFM extensions (tables, strikethrough,
  tasklists, footnotes) converts to HTML before storage. Matches Jodd's
  existing HTML body_html schema; round-trips to Apple Notes via existing
  Gmail sync.
- Folder protection: only folders matching BOTH kind='system_workflow' AND
  the `__name__` pattern get rename/move/delete hidden. `isProtectedWorkflowFolder`
  in Sidebar.svelte; `validate_folder_segment` in lib.rs rejects user
  attempts to create folders matching the pattern.
- Failure doctrine: source text is NEVER lost. On LLM error, a fallback
  note containing only the Source block is created so the paste survives.
- Tags emitted by the LLM (e.g. `#database-migrations`) populate `note_tags`
  via the existing inline-tag body parser; hyphen is part of the tag
  word-class (db.rs is_tag_word_char + NoteEditor.svelte client mirror).
- See [docs/superpowers/specs/2026-06-13-lesson-extraction-design.md] for
  the design spec and [docs/superpowers/plans/2026-06-13-lesson-extraction.md]
  for the implementation plan.

**Tags** (roadmap item #2) landed v0.14.3–v0.14.5 across three waves:

WAVE 1 — local-only tags (migration #5):
- `note_tags` join table — PK `(account_id, uuid, tag)`, index on
  `(account_id, tag)` covering both the sidebar "count per tag" query
  and the "notes carrying tag X" filter. Tags are pre-normalized by
  the write path: trimmed, leading `#` stripped, lowercased, charset
  `[a-z0-9_-]`.
- Jodd-local only despite the roadmap note about `#hashtag` in body:
  tags are NOT stored in the note body and do NOT round-trip to Apple
  Notes (Apple has no tagging system). The HTML body remains the
  Apple-compatible payload; tags are sidecar metadata in SQLite.

WAVE 2 — tombstone-based prune_clean race recovery (migration #6):
- `tag_tombstones` table (PK matches note_tags + `deleted_at`) acts as
  a recovery buffer for Gmail's eventually-consistent `q=label:Notes`.
  Before: a transient list-omission caused `prune_clean` to delete the
  cache row AND its `note_tags` entries — tags silently destroyed.
  Now: orphan-tag step moves rows to `tag_tombstones` instead of
  deleting; `upsert_from_remote` restores tombstoned tags when the
  note reappears. Old tombstones swept after TOMBSTONE_TTL_MS.

WAVE 3 — cross-Jodd-instance tag sync (migration #7):
- Mirrors Pin's sidecar pattern with one twist: pin is binary
  (sidecar exists = pinned), tags are a variable-length set, so the
  sidecar carries a JSON body `{"tags":["a","b",…]}` and
  `list_tag_sidecars` fetches WITH body (not metadata-only like
  pin's `list_meta_sidecars`).
- Subject convention `tags___<UUID>` — leading `tags` keeps the
  prefix disjoint from pin's `___<UUID>` so each sync's reader
  rejects the other's sidecars by prefix match alone.
- Columns: `tags_meta_msg_id TEXT` (current sidecar's Gmail message
  id, NULL = none yet) + `tags_dirty INTEGER` (orthogonal to
  `sync_state` AND `pin_dirty` — a row can be content-dirty, pin-
  dirty, AND tags-dirty simultaneously). Partial index on
  `tags_dirty = 1` covers the worker drain.
- Local-wins on inbound: `apply_remote_tags` skips when
  `tags_dirty=1`. Worker drains via `list_tags_dirty` → save or
  trash sidecar in `meta_label`, then `mark_tags_pushed`.

ONE-SHOT BACKFILL (migration #8):
- Runs ONCE per install (recorded in the migrations table). Bulk-
  flips `tags_dirty=1` for every uuid that already has a `note_tags`
  row, so notes tagged before v0.14.4 get a sidecar created on the
  first post-upgrade tick. No-op on fresh installs. If both Mac and
  Windows run #8 on differently-tagged sets, sidecar last-write-wins
  on the second push; local-wins blocks the inbound clobber on the
  loser, and divergent edits converge through normal sync afterwards.

**Pin** (roadmap item #1) landed 2026-06-09 in two waves:

WAVE 1 — local-only pin (commits 301435a, e392796):
- `pinned INTEGER NOT NULL DEFAULT 0` column on `notes` (migration #3),
  partial index `(account_id, pinned) WHERE pinned = 1`.
- `set_pin` + `set_pin_batch` Tauri commands — pure local-first SQLite
  writes.
- NoteList sort `pinned DESC, date DESC`, 📌 prefix on the title row.
- Context menu: single-note "Pin"/"Unpin" toggle at top; multi-select
  shows "Pin all"/"Unpin all"/both. snapshot/optimistic-update/rollback.

WAVE 2 — cross-Jodd-instance sync (commits b5b5deb–cc28c62):
The doctrine update: pin lives in SQLite AND in a Jodd-managed sidecar
message in a configurable Gmail meta-label. Multiple Jodd instances
signed into the same Gmail account share pin state through the sidecar
without involving Apple Notes (which ignores anything outside Notes/*).

- Per-account label config in Account: `notes_label` (default "Notes"),
  `meta_label` (default "Notes-Meta"). `get_account_settings` /
  `update_account_settings` Tauri commands; AccountSettings.svelte modal
  reachable from the ⚙ icon on each account row in the bottom panel.
- Migration #4: `meta_msg_id TEXT` (current sidecar's Gmail message id)
  and `pin_dirty INTEGER` (orthogonal to sync_state — a row can be
  content-dirty AND pin-dirty simultaneously). Partial index on
  `pin_dirty = 1` covers the worker drain query.
- `set_pin`/`set_pin_batch` mark pin_dirty=1. NEW: `apply_remote_pin`
  (skip if pin_dirty=1, local wins until pushed), `clear_pins_not_in`
  (drop pins whose sidecar disappeared remotely), `list_pin_dirty`,
  `mark_pin_pushed`.
- Gmail layer: `SidecarRef`, `ensure_label`, `list_meta_sidecars`
  (uses `format=metadata, metadataHeaders=Subject` so the read path
  never fetches sidecar bodies), `save_meta_sidecar`, `trash_meta_sidecar`.
  Subject convention: `___<note_uuid>` triple-underscore sentinel.
  X-UTI `app.jodd.metadata` so Apple Notes ignores them.
  Sidecar EXISTS = pinned. Unpin = trash sidecar (no falsy body needed).
- Worker: `push_one_pin` drains pin_dirty rows. Resolves meta_label
  for the account, ensures it exists, then save_meta_sidecar (pinned)
  or trash_meta_sidecar (unpinned). Runs AFTER content + deletes
  (lowest priority — pin is UX-only).
- Pull: list_notes already does inline sidecar reconciliation. NEW
  `sync_pin_state` Tauri command does the same as a dedicated cold-
  start trigger (list_notes is NOT called on cold start). App.svelte
  calls it in parallel across accounts after `indexAllAccounts()`
  completes, then loadCachedNotes() to re-paint.
- Verified end-to-end: pinned a note in Notes/pinsync, observed
  meta_msg_id populated by the worker, wiped local pin column to
  simulate a fresh second Jodd install, cold-started → sync_pin_state
  pulled the sidecar from Notes-Meta and re-applied pinned=1.

Local-first doctrine compliance landed for **D1-D4 and D8-D10** this
session. D5-D7 documented as deferred (out-of-scope minor variants of
D1-D4 patterns). Every closed defect verified end-to-end in the release
build via computer-use, scoped to a test `Notes/play5` subtree that did
not touch real user data.

Closed this session:
- **D1** (9669999) — `db::ensure_ancestors` auto-inserts missing folder
  ancestors as `dirty_new` in the same transaction as the leaf insert.
- **D2** (91e6984) — `list_cached_notes_in_folder` + `paintFolderFromCache`
  make navigation pure SQLite; Gmail-touching `list_notes_in_folder`
  reserved for explicit refresh (sweep, settle, poll, manual).
- **D3** (4568ad2) — `Sidebar.{createFolderUnder, renameFolder, deleteFolder}`
  rewritten to optimistic-first + rollback (mirroring `moveFolderTo`).
- **D4** (f8e1fff) — `move_notes_batch` + `delete_notes_batch` Tauri
  commands, one SQLite tx each; `NoteContextMenu.{moveBatchTo, deleteBatch}`
  collapsed from N invokes to 1.
- **D8** (3cb165f) — `db::list_deleted_pending_uuids` filter in
  `list_notes` / `list_notes_in_folder` / `refetch_note` so Gmail's
  eventual consistency can't resurrect just-deleted notes as UI ghosts.
- **D9** (988d304) — `.folder-menu` viewport-fit (`menuAdjustedX/Y`) +
  `max-height: calc(100vh - 16px); overflow-y: auto;`.
- **D10** (86e61f7) — `moveTargetState` replaces the binary
  `isValidDropTarget` filter for menu rendering: parent renders disabled
  with italic "(current)" tag instead of vanishing.

Deferred:
- **D5** — `Sidebar.removeAccount` is D3-shaped.
- **D6** — `NoteContextMenu.deleteNote` single-note path is mildly D3-shaped.
- **D7** — `delete_note` legacy id-fallback branch is D2-shaped; no live caller.

See "Known defects" below for full rationale and exact file/function references.

## Prior status
- [x] Google OAuth2 (PKCE) + refresh token rotation
- [x] Gmail REST API: list/fetch/save/delete/labels
- [x] SQLite local-first cache + 5s sync worker
- [x] Conflict detection with keep-both reconciliation
- [x] In-flight push tracking (no self-induced false conflicts)
- [x] Local-first folder ops (create/rename/delete/move)
- [x] Multi-account UI + per-account keychain storage
- [x] Forensic-test correctness pass (aa9a041 + docs/SYNC-BUGS-2026-06-07.md)
- [x] Cross-platform release CI (Windows, macOS ad-hoc signed)
- [x] **Multi-account hardening** (commit 15448c5): pushing-set cleanup on
      remove_account; per-account recentlySavedUuids; sync_worker_tick
      live-accounts check; label_map_cache async refresh-lock; move_note
      removed as dead code; safe orphan cleanup (preview_orphans +
      trash_specific_messages) with re-check immediately before each trash
- [x] **Duplicate review UI** (commit 15448c5): amber `N dup` pill in sidebar
      account header; DupReviewModal with keeper + orphan version preview,
      per-orphan checkbox, "Move N to Trash" confirm
- [x] **Checklist editor** (commit 15448c5): formatTask toolbar button;
      microtask-deferred attribute sync (no preventDefault); editor.contains()
      re-render guard. EML round-trip proven — Jodd writes `checked=""`, Apple
      preserves it on display but never writes it back. Tasks are Jodd-
      authoritative state.
- [x] **Multi-select notes** (commit 97f2671): selectedUuids store, cmd/shift-
      click + Cmd+A, batch move + batch delete in NoteContextMenu with
      optimistic per-item updates and per-item rollback. Amber multi-selected
      visual.
- [x] **Folder UX polish** (commit 97f2671): move-to submenu max-height 60vh
      (was 100vh — caused unreachable bottom items); auto-expand ancestors on
      any $selectedFolder change AND on folder create; folder command entry
      logging for future diagnostics

## Backend vertical abstraction — former edges #1 / #1b (both DONE, 2026-06-16)

1. **Backend trait abstraction — DONE (Vertical #0 extraction, 2026-06-16).**
   The email-backend abstraction was extracted out of `gmail.rs`; the app is now
   reframed as "Vertical #0 (Apple-via-Gmail)" behind a backend-agnostic trait
   surface. All ~70 `gmail::*` call sites in `lib.rs` route through a concrete
   `GmailVertical` (static dispatch); only 5 bootstrap calls remain
   (`get_label_map` ×3, `get_user_email` ×2 — they run before a token-bound
   vertical exists). No behavior change; Apple round-trip preserved (the
   RFC822 builder move is byte-identical + golden-tested). New module map:
   - `src-tauri/src/mime822.rs` — format-neutral MIME/Apple helpers + the RFC822
     builder (`build_note_mime`). Reusable by IMAP/JMAP/Graph. Zero `crate::` deps.
   - `src-tauri/src/backend/mod.rs` — the trait surface: `Transport`, `AtRest`
     (realized via `mime822` encode + Gmail JSON decode), `Identity`, `Deriver`,
     `MetadataSidecar`, `Vertical` + `Capabilities` (`folder_model` + `fidelity`).
   - `src-tauri/src/backend/gmail/{mod,transport,identity,deriver}.rs` —
     `GmailVertical`, wrapping the existing `gmail::*` fns. Fat `list_*`
     orchestration (dedup/sort) kept intact as inherent methods (Pragmatic scope).
   Spec: [docs/superpowers/specs/2026-06-16-vertical-0-gmail-extraction-design.md];
   plan: [docs/superpowers/plans/2026-06-16-vertical-0-gmail-extraction.md];
   north-star: [docs/superpowers/specs/2026-06-16-architecture-principles-design.md].
   **Deliberately deferred (door open via trait, not built):** `Box<dyn Vertical>`
   dynamic dispatch (added with backend #2); decomposing `list_notes` dedup/sort
   into core-side generic logic; `accounts.sync_cursor` storage + real cursor
   (`changes_since` defined + implemented as full-scan w/ inert cursor; worker not
   yet rewired onto it); `note_folders` M:N; `note_remote_ids`;
   `content_schema_version`; removal of the `gmail.rs` `pub use` re-export shim +
   relocation of the Gmail types/JSON structs into `backend/gmail/` (Phase 5 — the
   `gmail::` TYPE references in lib.rs and the shim remain until then). Adding
   Microsoft/Graph or JMAP now means: implement the `Transport` trait for the new
   wire (reusing `mime822` + `Identity`), add `Box<dyn>` dispatch, done. See
   [docs/REST-vs-IMAP-XOAUTH2.md](REST-vs-IMAP-XOAUTH2.md) for why the trait
   shape stays REST-based (Microsoft Graph is REST-shaped; IMAP deprecated upstream).
   **UPDATE (Vertical #1, 2026-06-16):** the items above marked "deferred until
   backend #2" are now BUILT — `Box<dyn Vertical>` dynamic dispatch exists
   (`vertical_for` in `lib.rs` dispatches on `Account.backend_kind`), and the fat
   `list_*` orchestration was promoted to a `NoteStore` trait (Gmail's dedup stays
   a Gmail-internal quirk; LocalFS implements its own one-file-per-uuid scan). The
   `gmail.rs` shim is also gone (Phase 5 done: Gmail wire lives in
   `backend/gmail/wire.rs`). Still deferred: `accounts.sync_cursor` real cursor,
   `note_folders` M:N, `note_remote_ids`, `content_schema_version`, JMAP/Graph.

1b. **Vertical #1 — LocalFS — DONE (2026-06-16).** A second backend vertical:
   notes stored as `.eml` files (RFC822 wrapping the SAME Apple-HTML body as Gmail,
   so it reuses `mime822` + the editor + `Identity`; `content_kind` stays
   `AppleHtml` — NOT markdown). Proves the federation: a genuinely divergent
   backend (filesystem transport, no OAuth/keychain, raw-RFC822 decode via
   `mail-parser`, stable remote-id = file path) plugs into the shared core
   (`Box<dyn Vertical>`, neutral index, conflict, sync_state) without bloating it.
   - `src-tauri/src/backend/localfs/{mod,transport,decode}.rs` — `LocalFsVertical`.
     Storage under the vault's `root_dir`: `Notes/<...folders...>/<uuid>.eml`;
     `.trash/<percent-encoded-relpath>` (delete = move to `.trash`, restore decodes
     back to the ORIGINAL subfolder); `.meta/<uuid>.{pin,tags.json}` sidecars.
     `decode.rs` parses raw `.eml` → neutral envelope (read the standard `Date`
     header via `msg.date()`, NOT `.as_text()` — it's structured).
   - Shared `AppleHtmlDeriver` (`backend/deriver_applehtml.rs`) — both verticals
     derive FTS/tags/edges identically; cross-vertical search/graph span both.
   - Account model: `accounts::BackendKind { Gmail, LocalFs }` + `root_dir`
     (serde-default = Gmail, back-compat). LocalFs account id = `localfs:<uuid>`;
     display name (vault name) in `email`, shown as `localfs:<vaultname>` everywhere
     (must be unique among local vaults). Readiness (`is_ready_local`) = dir exists,
     no network/keychain. Add via "Add Local Folder" (dialog plugin) → name prompt;
     rename via account settings (`rename_local_account`).
   - Deps added: `mail-parser`, `walkdir`, `tauri-plugin-dialog`. Verified by
     `examples/roundtrip_localfs.rs` (tempdir, no network) + live GUI test.
   Spec: [docs/superpowers/specs/2026-06-16-localfs-vertical-design.md]; plan:
   [docs/superpowers/plans/2026-06-16-localfs-vertical.md]. Follow-up DONE
   (slug rewrite-on-rename, branch `feat/slug-rewrite-on-rename`): `[[*-uuid8]]`
   wikilink DISPLAY text used to go stale after a note rename (resolution by
   uuid8 was always fine; only the frozen title-slug was wrong). Fixed by
   `db::rewrite_links_to_renamed_note_conn` (free fn mirroring
   `rewrite_tag_in_bodies`) hooked into `apply_local_edit`: it captures the
   previous title, and when `slugify(prev) != slugify(new)` rewrites every
   carrier's body (carriers found via the `edges` index — rel='mentions',
   dst_id=uuid8 — no full scan), flipping each clean→dirty so the worker
   re-syncs. Plain `[[Title]]` (no uuid8) left untouched by design. Backend-
   agnostic (operates on the SQLite cache; both Gmail + LocalFS verticals push
   the dirtied carriers). Spec:
   [docs/superpowers/specs/2026-06-16-slug-rewrite-on-rename-design.md]; plan:
   [docs/superpowers/plans/2026-06-16-slug-rewrite-on-rename.md].


## Closed local-first defects (D1–D11, 2026-06-09 architectural pass)

### Closed (session 2026-06-09 architectural pass)

- **D1. Orphan child folder rows.** ✅ CLOSED.
  `db::ensure_ancestors` inserts every missing strict ancestor below the
  implicit "Notes" root as `dirty_new` inside the same transaction as the
  leaf insert. Wired into both `insert_folder_local_new` and
  `upsert_folder_from_remote`. Root cause was the pull path:
  `upsert_folder_from_remote` accepted whatever labels Gmail returned, and
  Gmail allows `A/B` to exist without `A` (the slash is just a character),
  so a deletion of `Notes/play4` upstream while `Notes/play4/play4sub`
  survived created the orphan. The user's SQLite had already self-healed
  by the time the fix landed; no one-shot repair query was needed.

- **D2. `list_notes_in_folder` blocked on Gmail.** ✅ CLOSED.
  Added `list_cached_notes_in_folder` (pure SQLite read scoped to one
  label). Folder-click navigation in `App.svelte` now paints from cache
  immediately via the new `paintFolderFromCache`. The existing
  `list_notes_in_folder` stays as the Gmail-touching command and is still
  called by the 10s folder settle, the 2.5s sweep tick, the 10min poll,
  and the manual refresh button — all explicit reconciliation paths.

- **D3. Frontend state updates after awaited invoke.** ✅ CLOSED.
  `Sidebar.createFolderUnder`, `renameFolder`, and `deleteFolder`
  rewritten to mirror `moveFolderTo`: snapshot → optimistic mutate →
  invoke → rollback. Also dropped the post-success `$refreshNotes()`
  they used to fire (full Gmail re-fetches for state changes that
  didn't touch any note content).

- **D4. N sequential save_note / delete_note in batch ops.** ✅ CLOSED.
  Added `move_notes_batch` + `delete_notes_batch` Tauri commands. Each
  runs a single SQLite transaction over the supplied uuids.
  `NoteContextMenu.moveBatchTo` and `deleteBatch` now fire one invoke,
  with full snapshot/rollback semantics.

- **D8. Ghost notes after delete: list paths returned Gmail messages
  whose local row was `deleted_pending`.** ✅ CLOSED (2026-06-09,
  post-D10). Surfaced during Phase 6: after a batch delete, the 10s
  folder settle fired `list_notes_in_folder` → Gmail's index hadn't
  yet caught up to the worker's trash calls → returned the
  just-deleted messages → frontend merge re-added them to `$notes`.
  SQLite was correct throughout; only the UI showed ghosts.
  Fix: new `db::list_deleted_pending_uuids(account_id)` helper, called
  by all three Gmail-touching read paths (`list_notes`,
  `list_notes_in_folder`, `refetch_note`) to filter the result before
  returning. `refetch_note` returns an explicit error instead of
  filtering since the caller is asking for one specific message
  ("uuid is marked deleted locally — refusing to resurrect").

- **D9. Folder context menu clips off-screen on tall trees.** ✅ CLOSED
  (Phase 7, 2026-06-09). Sidebar's `.folder-menu` inlines the move-to
  folder list directly (no nested submenu like `NoteContextMenu`) and
  had no viewport-fit clamp or max-height. When the right-click landed
  low in the sidebar AND the account had many labels, Delete ran below
  the viewport bottom and was unreachable. Fix: mirror
  `NoteContextMenu`'s `adjustedX/Y` snap via a `menuEl` bind +
  reactive `getBoundingClientRect()`, plus
  `max-height: calc(100vh - 16px); overflow-y: auto;` on the menu so
  even a too-tall menu scrolls instead of clipping. Verified
  end-to-end by reaching the previously-unreachable Delete on a
  deeply-nested `play5/play5a/play5ab` triplet.

- **D10. Move-to filtered the parent entirely — looked like the folder
  vanished.** ✅ CLOSED (2026-06-09, post-Phase-7). Right-clicking a
  subfolder showed a move-to list with the parent removed (since moving
  a child to its parent is a no-op). On the play5/play5a/play5ab tree,
  right-clicking play5a hid play5 entirely — user couldn't see the
  folder structure they were working inside. Fix: replace
  `isValidDropTarget` filter in the move-to render with a three-way
  `moveTargetState` (`valid | parent | hide`). Self and descendants
  still hide (truly impossible targets). Parent renders disabled with
  a "(current)" italic tag so the structure is visible without being
  selectable.

- **D11. Empty folders invisible on cold start.** ✅ CLOSED (2026-06-09,
  post-Windows-OAuth-fix). Folders with no notes (e.g. `Notes/play2`,
  `Notes/play3`, `Notes/play4` and their subs) did not appear in the
  sidebar until the user navigated. Root cause: the folders cache was
  reconciled from the Gmail label set **only inside `list_notes`**, which
  is NOT called on cold start (cold start runs `index_account` +
  `sync_pin_state` only — see Pin sync wave 2 above). Folders that
  *contained* a note still appeared because the sidebar infers a folder
  path from note labels (`folderCountsByAccount`); empty labels have no
  note to infer from, so they stayed hidden. Verified against the user's
  real mailbox: SQLite `folders` table held all of play2/3/4 as `clean`,
  but the sidebar omitted them. Fix: extracted the list_notes folder
  reconciliation into `reconcile_folders_from_labels(db, account_id,
  label_map, prune)` and called it upsert-only (`prune=false`) from
  `index_account`, so the cold-start index pass populates empty folders.
  Pruning stays list_notes-only (`prune=true`) — cold start must not
  delete on a possibly-partial view. Frontend: `Sidebar` reactive folder
  refresh now also depends on `$noteIndex`, so it re-reads `list_folders`
  once the cold-start index (which now carries the reconciled folders)
  lands. NOTE: counts shown beside a folder are direct-label only (notes
  in that exact label, not descendants) — by design, matching Apple Notes;
  the "All" account total is the full mailbox count, so a small per-folder
  number next to a large total is correct, not a bug.


## Roadmap — shipped

### Done
- [x] **Pin** (2026-06-09) — `pinned` column + `set_pin` + sort.
- [x] **Attachments** (Tier 0) — SQLite-BLOB store, full round-trip.
- [x] **Search-as-index** — FTS5 (trigram, Thai) + cross-account scope.
- [x] **Recently Deleted / Trash** — list + restore (untrash + relabel).
- [x] **Richer text toolbar** — underline / strike / heading / ordered list.
- [x] **Tags inline `#hashtag`** — body = source of truth, round-trips; sidebar
      pills, cross-account filter, rename/delete (was "Tags via #hashtag").
- [x] **Fact-schema edges + backlinks** — `edges` (mentions/child_of/tagged) +
      `[[slug]]` links + `[[` picker + local graph view.
- [x] **Outline / nesting** — Tab/Shift-Tab indent (nested `<ul>` + margin),
      checklist Enter-continues + nested-checklist roll-up; editor-focus fixes.
- [x] **Content Extraction** (v0.16.1, hardened v0.16.2) — LLM-backed
      paste-and-extract workflow. Generic enough to handle debug sessions,
      meetings, articles, conversations — prompt adapts structure to source.
      `llm` Rust module behind a `LlmProvider` trait with HTTP
      (OpenAI-compatible) + `claude -p` subprocess impls.
      Output goes into a `system_workflow`-kind folder stored as
      `Notes/__Extracts__` (migration #14), displayed as "Extracts" in the
      sidebar Workflows group after marker-strip. The `__name__` syntax is
      RESERVED for Jodd-managed folders — `validate_folder_segment` rejects
      any user-create or rename matching `__*__`. Source preserved verbatim
      in a collapsible `<details>` block for re-extraction. Cancellation
      (v0.16.2) propagates through `CancellationToken` to both providers —
      drops the reqwest future (HTTP) or kills the child subprocess
      (Claude CLI). Tags emitted by the LLM (hyphens included) populate
      `note_tags` via the existing inline-tag parser.
- [x] **Agent-CLI LLM providers** (2026-07-28) — any headless agent CLI
      (`claude`, `codex`, `qwen`, `gemini`, `opencode`, `aider`, or a Custom
      spec) can back Extract and auto-link. One runner, a preset table, a
      Test-connection probe, and a single retry for CLIs with no JSON mode.
      Verified live: claude and codex. See
      [the design spec](superpowers/specs/2026-07-27-agent-cli-llm-providers-design.md).
- [x] **Structured citations** (2026-07-16) — `rel='cites'` edges, hand-rolled
      URL scanner, Sources panel, soft duplicate-source warning. Jodd-local
      only. See Current status.
- [x] **Smart Folders — Orphaned + Stale** (2026-07-16) — fully virtual,
      per-account, fixed set. See Current status and edge #3.
- [x] **`jodd-mcp` — read-only MCP graph server** (2026-07-16) —
      `search_notes` + `note_connections`, new Cargo workspace member. See
      Current status and edge #7.
- [x] **Account inactive status** (2026-07-30) — `AccountStatus`
      (Active/Draining/Inactive) on `Account`. Deactivating is a *quiesce*:
      the account leaves every view immediately and the worker keeps draining
      its outbound queues. The worker flips Draining → Inactive once
      `db::has_pending_pushes` returns false; that path means **`Inactive`
      is a guarantee that nothing is pending**. The exception: "Stop waiting"
      forces the flip immediately, and any unsent edits stay in SQLite as
      `dirty` rows — not lost, but left on the device. They drain normally
      if the account is reactivated. Anything relying on the guarantee (0c
      included) must check the queue rather than assume the state. `vertical_for`
      refuses `Inactive` only; `remove_account` refuses `Draining`, because it
      deletes the refresh token first. See edge #9.
- [x] **Android bring-up — Sub-project 1: headless core** (2026-08-03, branch
      `feat/android-core`, not yet merged). Sideload → Google sign-in →
      185 notes / 70 labels into SQLite → edit on the phone → the edit appears
      in Apple Notes on iPhone, verified on a Galaxy S23 FE (Android 16) and
      an Infinix X6821 (Android 13). New seams: `secrets.rs` (credential
      store, `keyring-core` + per-target providers) and `paths.rs`
      (config/data dirs). OAuth was the whole of the work — see edge #11 and
      [docs/android/APP-LINKS-SETUP.md](android/APP-LINKS-SETUP.md).
      **No UI work: the desktop three-pane layout is unusable at phone width,
      by design.** That is Sub-project 2; Sub-project 3 is the APK release
      pipeline. See roadmap item 8.
