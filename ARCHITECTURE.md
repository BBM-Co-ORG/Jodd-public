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
                     │            │            │
            GmailVertical   LocalFsVertical  MicrosoftVertical
            (Gmail REST)    (.eml files      (Graph API over an
                             on disk)          Exchange mailbox)
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
- **Remote (Microsoft vertical)**: Microsoft Graph API over HTTPS,
  OAuth 2.0 with PKCE — verified against a live Exchange mailbox; see
  [Status](README.md#status) for what release builds do and don't
  carry for it
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

- **`Transport`** — folder CRUD (`list_folders` / `ensure_folder` /
  `create_folder` / `rename_folder` / `delete_folder`) plus a note
  `delete` and `move_note`; those seven methods are what the sync
  worker and Tauri commands actually call. The trait also declares
  `save` and `changes_since`, but neither carries real weight today:
  every vertical's `save` is a thin wrapper that just forwards to
  `NoteStore::save_note_full` (the method callers use directly), and
  the only call site for `changes_since` in the repository today is a
  test assertion. The split from `NoteStore` below is real, but it
  isn't a clean one.
- **`NoteStore`** — list / fetch / save / trash-and-restore operations
  for notes themselves, with each backend free to own its own
  read/write strategy (Gmail dedups transient duplicates; LocalFS
  doesn't need to, since it keeps one file per uuid).
- **`Identity`** — one method, `mint() -> String`, that generates a
  fresh Apple-format note UUID
  (`X-Universally-Unique-Identifier`). Despite the name it has nothing
  to do with account identification or authentication — that lives in
  `accounts.rs`. It is also barely exercised: the only call site in
  the repository today is a test assertion; every real note-creation
  path mints its UUID inline instead of going through this trait.
- **`Deriver`** — derives search index, tags, and link/backlink edges
  from a note body. Shared across backends so a note looks the same
  to search and the graph view regardless of where it's stored.
- **`MetadataSidecar`** — Jodd-only metadata that piggybacks on the
  backend as a sidecar message/file rather than living in the note
  body. Today that's `Pin` alone — `SidecarKind` has a single variant.
  Tags used to work the same way, with a second sidecar syncing tags
  across Jodd instances, but that mechanism was replaced by deriving
  tags from the note body's inline `#hashtags` on every write/pull;
  the columns it left behind (`tags_meta_msg_id`, `tags_dirty`) are
  still in the SQLite schema but are dead — no Rust code reads or
  writes them anymore, kept only because this codebase has no `DROP
  COLUMN` precedent.
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
so the verticals reuse it directly:

- **`src-tauri/src/mime822.rs`** — the RFC 822 / MIME builder and the
  Apple-specific title-in-body wrapping logic. Zero dependency on the
  rest of the app, so a future IMAP/JMAP vertical can reuse it too. The
  Microsoft Graph vertical does **not** build messages with it: an
  Exchange note carries none of Apple's `X-` headers and isn't a MIME
  message at all, so its wire format needed its own module. It does
  still borrow one format-neutral helper from the file,
  `format_apple_uuid`, in its `Identity::mint` — so the accurate
  statement is that `build_note_mime` is unused there, not the module.
- **`src-tauri/src/backend/deriver_applehtml.rs`** — the shared
  search/tags/edges deriver, implemented by all three verticals, so
  full-text search and the graph view behave identically whichever
  backend a note came from.

## How Apple Notes ↔ email works

This section describes the **Gmail** backend specifically. A
Microsoft account is fundamentally different, not just a
second email provider: Apple treats it as an **Exchange** account, so
each note is a structured Exchange item with no `X-` headers, no MIME
body, and identity keyed on `internetMessageId` instead of Apple's
`X-Universally-Unique-Identifier`. See [Status](README.md#status) for
what that backend already does.

Each Gmail note is a single message with these headers:

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
and no IMAP session lifecycle. Microsoft Graph is REST-shaped for the
same reasons, which is part of why the trait surface above is
REST-based rather than IMAP-based.

## Sync state machine

`SyncState` (`db.rs`) declares five note states, but only three are
ever actually reached:

- `clean` — local copy matches remote.
- `dirty` — local edit pending push.
- `deleted_pending` — local delete pending push.

The other two are declared and converted but never written by
anything that runs:

- `pull_needed` — meant to mark "remote change detected, not yet
  applied," but no code path ever sets it. It exists only as an enum
  variant, a `from_str`/`as_str` round-trip, and the source condition
  in three defensive `CASE sync_state WHEN 'pull_needed' THEN
  'conflict' …` clauses — two in the shared body behind
  `apply_local_edit`, one in `move_notes_batch` — that only fire on a
  row that is already `pull_needed`, which never happens.
- `conflict` — `Db::flag_conflict` is the only unconditional writer
  of this state, and it has no callers anywhere in the workspace.

Keep-both conflict resolution is real and shipped, but it does not
work by parking a note in the `conflict` state — it resolves inline,
in one step, inside `reconcile_one` (`lib.rs`). When a poll notices a
`dirty` note whose remote version has also moved, `reconcile_one`
mints a fresh-UUID copy holding the local content (title suffixed
`(conflict from {device} {date})`), then applies the remote content
to the primary row through the same path an ordinary pull uses —
which also resets the primary's `sync_state` straight to `clean`, not
to `conflict`. The copy leaves as an ordinary `dirty` row for the
worker to push on the next tick. Neither row ever passes through
`pull_needed` or `conflict`; those two states are dead code the enum
still carries.

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
via a sidecar message), and `tags_meta_msg_id` / `tags_dirty` — dead
columns from the tag sidecar that body-derived `#hashtags` replaced,
still present because nothing outside migrations #7 and #8 reads or
writes them and this codebase has no `DROP COLUMN` precedent
(see [Verticals](#backend-abstraction-verticals) above). Indexes on
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
  `NoteStore`, `Identity`, `Deriver`, `MetadataSidecar`, `Vertical`,
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
  Notes), the Microsoft backend's note create/edit/move/delete
  itself (measured against a live account). **Not** in this
  tier on Microsoft: folder create/rename/delete, and attachments —
  both are permanent limitations of that backend, not gaps to fill in
  later; see [Status](README.md#status).
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
is exactly how pin works — and how cross-device tags used to work,
before they moved to being derived from the body's inline
`#hashtags`).

## At-rest encryption and trust boundaries

The SQLite cache is encrypted at rest via SQLCipher, whose compiled-in
default cipher is AES-256-CBC — Jodd's code never overrides it. The key
lifecycle lives in `src-tauri/src/db_crypto.rs`: the cipher key is a
256-bit random value held as a 64-character hex string and stored in the
OS credential store — macOS Keychain, Windows Credential Manager, Android
Keystore — the same place OAuth refresh tokens already live.

The key is deliberately **not** derived from a passphrase. SQLCipher's
raw-key syntax takes the key directly because it is machine-generated and
never human-typed: there is no guessable input, so there is no brute-force
surface for a KDF to slow down.

Four properties of the implementation are worth stating, because each
exists to prevent a specific way of losing user data:

- **A failed key read is never treated as "no key yet."** `load_key_hex`
  returns `Ok(None)` only when the entry genuinely does not exist. A
  locked secret service, a lost macOS ACL, or an unavailable backend
  returns `Err`. Collapsing the two would let a fresh key be minted over a
  good one, leaving the already-encrypted database permanently
  undecryptable.
- **The key is persisted and confirmed before it encrypts anything.** The
  order is generate → persist → confirm → encrypt, never encrypt-then-save.
- **Plaintext detection needs no key.** `is_plaintext_sqlite` reads the
  first 16 bytes and looks for SQLite's magic header. A correctly
  encrypted file's header is itself encrypted, so the check alone
  distinguishes the two cases and drives the one-time migration.
- **A wrong key fails at open, not later.** `PRAGMA key` never errors on a
  wrong key — only the first real read does. `open_encrypted` therefore
  forces a canary query before returning, so the failure surfaces at open
  time rather than at an arbitrary later query.

### What crosses which boundary

| Boundary | What is on the far side |
|---|---|
| **Device** | Encrypted SQLite cache; cipher key and OAuth refresh token in the OS credential store; access tokens in process memory only; `accounts.json` metadata (address, backend kind) in plaintext. |
| **Network** | TLS. OAuth 2.0 with PKCE — an intercepted authorization code is useless without the per-flow verifier. |
| **Provider** | The user's own Gmail or Microsoft account. |
| **Apple devices** | iPhone / Mac reading the same store. |

Two facts about this picture matter more than the rest:

**There is no Jodd server.** No note, and no note metadata, is transmitted
to BBMedia. The absence of a fifth column in that table is the design.

**A Local Folder vault is plaintext, on purpose.** At-rest encryption
covers the cache of a remote source of truth. It does not cover a Local
Folder vault, because a directly-readable folder of `.eml` files on a disk
the user chose is the entire point of that backend. Encrypting it would
remove the property it exists to provide.

## The capability model

Backends are not equally capable, and the difference is expressed in code
rather than in a comment or a wiki page. `Capabilities`
(`src-tauri/src/backend/mod.rs`) is derived from the account's backend kind
alone — no token, no network call — so the UI can adapt without a round
trip.

- **`Writes { notes, folders, sidecars }`** replaced a single `can_write`
  flag, which could not express a backend where notes are writable and
  folders are not.
- **`has_trash`** tells the UI whether a "Recently Deleted" view exists at
  all. A backend that reports `false` must hide the view rather than show
  an always-empty one, which reads to a user as a sync bug.
- **`SaveSemantics`** answers two questions at once — whether a content
  push relocates a note by itself, and whether a `NotFound` on that push
  is trustworthy evidence the note is gone. The two travel together
  because they share one root cause: whether the backend's save is
  REPLACE-shaped or PATCH-shaped. Letting a future backend declare them
  separately would recreate the chance of forgetting one.

The model's real purpose is to record limits the code must honour. A
capability set to `false` permanently — with the evidence for it in the
doc comment beside it — is how a proven limitation stops being
rediscovered, and stops being shipped as a feature that fails quietly on
someone's phone.

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
