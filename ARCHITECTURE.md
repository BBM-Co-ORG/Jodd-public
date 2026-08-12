# Architecture

This document describes the moving parts of Jodd at the level a new
contributor needs in order to find their way around the codebase.

## The big picture

```
UI (Svelte 5) ─── invoke ──▶ Tauri commands (lib.rs)
                                   │
                                   ▼
                          SQLite cache (db.rs)   ◀── source of truth for the UI
                                   │
                                   ▼
                          Sync worker (lib.rs)   ◀── 5-second tick, drains dirty rows
                                   │
                                   ▼
                          Box<dyn Vertical>       ◀── backend-agnostic trait surface
                             │            │
                    GmailVertical   LocalFsVertical
                    (Gmail REST)    (.eml files on disk)
```

The defining property: **SQLite is the truth of the moment.** The UI
reads from the cache. Writes go to the cache first, marked `dirty`.
A background worker drains dirty rows to whichever backend the
account uses, on a 5-second tick. Polls pull remote state back via
the same cache. The user never waits on a network (or disk) round
trip during normal editing or navigation.

## Tech stack

- **Frontend**: Svelte 5 + TypeScript + Vite 6
- **Backend**: Tauri 2 + Rust
- **Local store**: SQLite (`jodd.sqlite3`) via `rusqlite`, with FTS5
  (trigram tokenizer, so substring search works for non-space-delimited
  scripts like Thai) over note titles and bodies
- **Remote (Gmail vertical)**: Gmail REST API over HTTPS, OAuth 2.0
  with PKCE
- **Remote (LocalFS vertical)**: the filesystem — no network, no
  account
- **Release targets**: Windows, Apple Silicon macOS, and Android (Developer
  Preview). Intel macOS is temporarily omitted from CI; Linux is not a
  published release target.

## Agent access through MCP

`jodd-mcp/` is an optional local MCP server over the same SQLite cache. Read
tools expose bounded search and connection results. Write tools are
deny-by-default and require an explicit per-account folder allowlist in
`mcp_write_scope.json`; they accept Markdown and use the same database,
sanitization, derivation, and background-sync paths as in-app edits. Task
updates also require the caller's expected checklist text so a stale agent
cannot silently change a different row.

## Backend abstraction: Verticals

Jodd talks to more than one backend, so the parts that are specific
to "how notes are transported and encoded" sit behind a trait
surface (`src-tauri/src/backend/mod.rs`) instead of being hard-coded
against Gmail:

- **`Transport`** — list / fetch / save / delete operations against
  wherever the notes actually live.
- **`AtRest`** — encode/decode a neutral note envelope to/from the
  backend's wire format.
- **`Identity`** — how an account is identified and authenticated (or,
  for local vaults, simply "a directory exists").
- **`Deriver`** — derives search index, tags, and link/backlink edges
  from a note body. Shared across backends so a note looks the same
  to search and the graph view regardless of where it's stored.
- **`MetadataSidecar`** — Jodd-only metadata (pin, tags) that piggybacks
  on the backend as a sidecar message/file rather than living in the
  note body.
- **`Vertical`** — the composition of the above, plus a
  `Capabilities` descriptor (folder model, fidelity tier) that the UI
  can use to adapt.

`lib.rs` dispatches to a concrete `Box<dyn Vertical>` per account
based on the account's configured backend kind, so the sync worker,
conflict reconciler, and every Tauri command are written once against
the trait, not once per backend.

Two verticals exist today:

- **`GmailVertical`** (`src-tauri/src/backend/gmail/`) — talks to the
  Gmail REST API. This is how Jodd achieves genuine Apple Notes
  round-trip: Apple already syncs Notes to non-iCloud accounts as
  email, so Jodd reads and writes the same messages Apple does.
- **`LocalFsVertical`** (`src-tauri/src/backend/localfs/`) — stores
  each note as a `.eml` file on disk (`Notes/<...folders...>/<uuid>.eml`),
  using the *same* RFC 822 encoding and the *same* Apple-compatible
  HTML body as the Gmail vertical. No sign-in, no network, works
  fully offline. It proves the abstraction is real: a filesystem
  transport with no OAuth, no keychain, and a different stable-id
  scheme (file path instead of a Gmail message id) plugs into the same
  SQLite cache, conflict model, and sync-state machine as Gmail.

Shared, backend-neutral code lives outside the trait implementations
so both verticals reuse it directly:

- **`src-tauri/src/mime822.rs`** — the RFC 822 / MIME builder and the
  Apple-specific title-in-body wrapping logic. Zero dependency on the
  rest of the app, so a future IMAP/JMAP/Microsoft Graph vertical can
  reuse it too.
- **`src-tauri/src/backend/deriver_applehtml.rs`** — the shared
  search/tags/edges deriver, so full-text search and the graph view
  span both backends identically.

## How Apple Notes ↔ email works

Each note is a single message with these headers:

- `X-Uniform-Type-Identifier: com.apple.mail-note`
- `X-Universally-Unique-Identifier: <UUID>` — Apple's identity for the
  note; preserved across edits, the only stable cross-device anchor
- `X-Mail-Created-Date` — creation timestamp Apple uses for sort
- `Subject:` — note title
- Body: HTML

Folders map onto Gmail labels under a configurable root (`Notes` by
default), e.g. `Notes/Work/Projects`. Apple wraps the title inside the
body as `<div>{title}</div>` or `<span style="…">{title}</span>`;
Jodd strips/injects that wrapper at the boundary so the UI title and
body editor stay separate.

The Gmail vertical uses Gmail's **REST API** rather than IMAP-XOAUTH2
for reasons documented in `docs/REST-vs-IMAP-XOAUTH2.md` — chiefly:
simpler error handling, lower per-request latency on slow networks,
and no IMAP session lifecycle. Microsoft Graph (planned) is REST-shaped
for the same reasons, which is part of why the trait surface above is
REST-based rather than IMAP-based.

## Sync state machine

Notes carry one of these sync states:

- `clean` — local copy matches remote.
- `dirty` — local edit pending push.
- `pull_needed` — remote change detected, fetch + apply.
- `conflict` — local edit AND remote change detected for the same
  note. Resolved by **keep-both**: a fresh-UUID copy is created with
  a `(conflict from …)` suffix in the title, preserving the local
  content; the primary row converges to remote. The user edits either
  copy to resolve.
- `deleted_pending` — local delete pending push.

Folders carry:

- `clean | dirty_new | dirty_renamed | deleted_pending`.

Folder hierarchy is auto-completed: inserting `Notes/A/B/C` ensures
`Notes/A` and `Notes/A/B` exist in the same transaction, on both the
locally-initiated path and the path that reconciles folders pulled
from the backend — so a backend that allows a leaf label to exist
without its parent (Gmail does) never produces an orphaned row.

## In-flight push tracking

A `pushing: HashSet<(account_id, uuid)>` set in `AppState` prevents
the next poll tick from treating an in-progress push as a remote-side
change. Without this guard, our own pushes can race the next poll
and look like a remote edit, generating a false conflict.

## Multi-account model

- **Account identity** is the email address for Gmail accounts, or a
  user-chosen vault name (`localfs:<vaultname>`) for local folders.
  Immutable once created.
- **Account metadata** lives in `accounts.json` in the OS user data
  directory, including which backend (`Gmail` / `LocalFs`) and, for
  local vaults, the root directory on disk.
- **Refresh tokens** (Gmail only) live in the OS keychain
  (`security`/`Credential Manager`/`Secret Service`), keyed by the
  account email under service name `jodd`. Local vaults have no
  token — readiness is just "the directory exists."
- **Access tokens** and the **label-name → label-id cache** live in
  process memory only.
- Every Tauri command takes an explicit `account_id`. There is no
  "current account" on the Rust side; the frontend's `currentAccount`
  store is a UI convenience for the active sidebar selection.

## The SQLite schema

`notes` — primary key `(uuid, account_id)`. Columns include the
backend's message/file id, title, body, dates, label, sync_state,
version counters, and columns for Jodd-local features layered on top
of the base sync model: `pinned` / `meta_msg_id` / `pin_dirty` (pin,
via a sidecar message), and `tags_meta_msg_id` / `tags_dirty`
(cross-device tag sync, also via a sidecar). Indexes on
`(account_id, label)` and `sync_state`; partial indexes on the dirty
flags so the sync worker's drain queries stay cheap as the table
grows.

`folders` — primary key `(account_id, path)`. Columns include the
label/folder id, sync_state, a derived `kind` (e.g. `system_workflow`
for Jodd-managed folders like the AI-extraction output folder), and
last-modified timestamps.

`note_tags` — join table for inline `#hashtag` tags, derived from the
note body on every write (never stored as a separate source of
truth — see "Compatibility tiers" below).

`edges` — a general fact table for note↔note and note↔folder
relationships: `mentions` (`[[wikilinks]]`), `child_of` (note→folder),
`tagged` (note→tag). Derived on every write, backing both the
backlinks panel and the local graph view.

FTS5 (trigram tokenizer) mirrors title + body text for search, so
substring search works across scripts that don't tokenize on
whitespace.

Every derived table above follows the same rule: **derive, don't
migrate.** If the source of truth is the note body (tags, links) or
a stable naming convention (a folder's `kind`), the derivation runs
on every relevant write/sync rather than being computed once and
trusted forever. That makes a second device, or a fresh install,
converge to the same derived state without a one-shot repair step.

Core operations are exposed by `src-tauri/src/db.rs` and are all
single-transaction.

## Layering rules

1. **Every user action writes synchronously to SQLite** in a single
   transaction.
2. **Frontend state updates happen synchronously** with the write
   (optimistic mutate; rollback on backend failure).
3. **The backend is touched only by background paths**: the worker
   tick, explicit refresh buttons, sign-in / index pass, and
   reconciliation flows.

Any normal navigation or editing command that blocks on the backend
(Gmail network round trip, or even local disk I/O) is a bug. Any
frontend state mutation that happens after an awaited IPC is a bug.

## Key files

- `src-tauri/src/lib.rs` — Tauri commands, sync worker tick, conflict
  reconciler, `Box<dyn Vertical>` dispatch per account.
- `src-tauri/src/backend/mod.rs` — the trait surface (`Transport`,
  `AtRest`, `Identity`, `Deriver`, `MetadataSidecar`, `Vertical`,
  `Capabilities`) and the neutral envelope types.
- `src-tauri/src/backend/gmail/` — `GmailVertical`: Gmail REST +
  Gmail-JSON wire format.
- `src-tauri/src/backend/localfs/` — `LocalFsVertical`: `.eml`
  files-on-disk, raw RFC 822 decode.
- `src-tauri/src/backend/deriver_applehtml.rs` — shared search/tags/
  edges derivation, used by both verticals.
- `src-tauri/src/mime822.rs` — format-neutral RFC 822/MIME builder +
  Apple title-wrapping helpers, shared by both verticals.
- `src-tauri/src/db.rs` — SQLite cache, migrations, sync-state
  transitions.
- `src-tauri/src/accounts.rs` — Multi-account JSON + keychain.
- `src-tauri/src/auth.rs` — PKCE OAuth + localhost callback.
- `src/lib/stores/notes.ts` — Global frontend state
  (accounts, notes, folders, selection, indices).
- `src/lib/components/NoteEditor.svelte` — Autosave on change,
  push-state tracking.
- `src/lib/components/Sidebar.svelte` — Account list, folder tree,
  folder context menu, drag-free move-to.

## Compatibility tiers for new features

When designing a new feature, classify it:

- **Round-trips to Apple Notes** — the feature works seamlessly on
  iPhone/Mac too. Examples: title/body edits, folder hierarchy under
  `Notes/`, inline `#hashtags` (Jodd's **tags** feature — parsed
  client-side from the body, so the same hashtags show up in Apple
  Notes), a Microsoft backend itself once it lands.
- **Backend-specific** — works on one backend, not the other.
  Example: Microsoft Graph tasks/reminders have no Gmail or local-file
  equivalent.
- **Jodd-local only** — won't appear on iPhone. Examples: pin (Apple
  stores pin state in iCloud metadata, not in the mail-note format),
  `[[wikilinks]]` and the graph view (stored as plain text in the
  body — safe, but Apple renders it as text, not a link), AI-assisted
  note extraction output.

The rule of thumb: anything stored *inside* the message body is safe;
anything stored as a custom IMAP flag, sidecar message in an unknown
folder, or non-Apple header will be silently dropped by Apple on next
sync. Keep custom metadata in the SQLite cache, not in the message —
or use a Jodd-managed sidecar message in a Jodd-managed label (which
is exactly how pin and cross-device tag sync work).

## Local-first doctrine, restated

Every user action must:

1. Write synchronously to SQLite, transactional and atomic.
2. Update in-memory state and DOM synchronously, optimistically if
   needed, with rollback on failure.
3. Never wait on the backend. Background sync pushes asynchronously;
   the user never sees backend latency in normal navigation or
   editing.

This is the single most important invariant in the codebase. Any
change that erodes it is almost certainly wrong.
