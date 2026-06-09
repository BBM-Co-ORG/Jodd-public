# Architecture

This document describes the moving parts of Jodd at the level a new
contributor needs in order to find their way around the codebase.

## The big picture

```
UI (Svelte 5) ─── invoke ──▶ Tauri commands (lib.rs)
                                   │
                                   ▼
                          SQLite cache (db.rs)  ◀── source of truth for the UI
                                   │
                                   ▼
                          Sync worker (lib.rs)  ◀── 5-second tick, drains dirty rows
                                   │
                                   ▼
                          Gmail REST (gmail.rs) ◀── remote replica
```

The defining property: **SQLite is the truth of the moment.** The UI
reads from the cache. Writes go to the cache first, marked `dirty`.
A background worker drains dirty rows to Gmail on a 5-second tick.
Polls pull remote state back via the same cache. The user never
waits on a Gmail round trip during normal editing or navigation.

## Tech stack

- **Frontend**: Svelte 5 + TypeScript + Vite 6
- **Backend**: Tauri 2 + Rust
- **Local store**: SQLite (`jodd.sqlite3`) via `rusqlite`
- **Remote**: Gmail REST API over HTTPS, OAuth 2.0 with PKCE
- **Targets**: Windows, macOS, Linux

## How Apple Notes ↔ Gmail works

Each note is a single Gmail message with these headers:

- `X-Uniform-Type-Identifier: com.apple.mail-note`
- `X-Universally-Unique-Identifier: <UUID>` — Apple's identity for the
  note; preserved across edits, the only stable cross-device anchor
- `X-Mail-Created-Date` — creation timestamp Apple uses for sort
- `Subject:` — note title
- Body: HTML

Folders map onto Gmail labels under a configurable root
(`Notes` by default), e.g. `Notes/Work/Projects`. Apple wraps the
title inside the body as `<div>{title}</div>` or
`<span style="…">{title}</span>`; Jodd strips/injects that wrapper at
the boundary so the UI title and body editor stay separate.
(See `inject_title_into_body` and `strip_leading_title` in
`src-tauri/src/gmail.rs`.)

We use the **REST API** rather than IMAP-XOAUTH2 for reasons
documented in `docs/REST-vs-IMAP-XOAUTH2.md` — chiefly: simpler error
handling, lower per-request latency on slow networks, and no IMAP
session lifecycle.

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
`Notes/A` and `Notes/A/B` exist in the same transaction.

## In-flight push tracking

A `pushing: HashSet<(account_id, uuid)>` set in `AppState` prevents
the next poll tick from treating an in-progress push as a remote-side
change. Without this guard, our own pushes can race the next poll
and look like a remote edit, generating a false conflict.

## Multi-account model

- **Account identity** is the email address. Immutable.
- **Account metadata** lives in `accounts.json` in the OS user data
  directory.
- **Refresh tokens** live in the OS keychain
  (`security`/`Credential Manager`/`Secret Service`), keyed by the
  account email under service name `jodd`.
- **Access tokens** and the **label-name → label-id cache** live in
  process memory only.
- Every Tauri command takes an explicit `account_id`. There is no
  "current account" on the Rust side; the frontend's `currentAccount`
  store is a UI convenience for the active sidebar selection.

## The SQLite schema

`notes` — primary key `(uuid, account_id)`. Columns include the Gmail
message id, title, body, dates, label, sync_state, version counters,
and (for pin support) `pinned`, `meta_msg_id`, `pin_dirty`. Indexes
on `(account_id, label)` and `sync_state`; partial indexes on
`pinned=1` and `pin_dirty=1`.

`folders` — primary key `(account_id, path)`. Columns include the
Gmail label id, sync_state, and last-modified timestamps. Index on
`sync_state`.

Core operations are exposed by `src-tauri/src/db.rs` and are all
single-transaction.

## Layering rules

1. **Every user action writes synchronously to SQLite** in a single
   transaction.
2. **Frontend state updates happen synchronously** with the write
   (optimistic mutate; rollback on backend failure).
3. **Gmail is touched only by background paths**: the worker tick,
   explicit refresh buttons, sign-in / index pass, and
   reconciliation flows.

Any normal navigation or editing command that blocks on Gmail is a
bug. Any frontend state mutation that happens after an awaited IPC
is a bug.

## Key files

- `src-tauri/src/lib.rs` — Tauri commands, sync worker tick,
  conflict reconciler.
- `src-tauri/src/gmail.rs` — Gmail REST + MIME + Apple header
  handling. The first place to look when adding a second provider.
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
  `Notes/`, inline `#hashtags`, switching to a Microsoft backend
  itself.
- **Backend-specific** — works on one provider, not the other.
  Example: Microsoft Graph tasks/reminders have no Gmail equivalent
  that Apple Notes uses.
- **Jodd-local only** — won't appear on iPhone. Example: pin (Apple
  stores pin in iCloud metadata, not in the mail-note format).

The rule of thumb: anything stored *inside* the message body is safe;
anything stored as a custom IMAP flag, sidecar message in an unknown
folder, or non-Apple header will be silently dropped by Apple on next
sync. Keep custom metadata in the SQLite cache, not in the message —
or use a Jodd-managed sidecar message in a Jodd-managed label (which
is exactly how pin works).

## Local-first doctrine, restated

Every user action must:

1. Write synchronously to SQLite, transactional and atomic.
2. Update in-memory state and DOM synchronously, optimistically if
   needed, with rollback on failure.
3. Never wait on Gmail. Background sync pushes asynchronously; the
   user never sees Gmail latency in normal navigation or editing.

This is the single most important invariant in the codebase. Any
change that erodes it is almost certainly wrong.
