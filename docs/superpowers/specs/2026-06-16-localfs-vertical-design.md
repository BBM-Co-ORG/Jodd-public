# LocalFS — Vertical #1 (stress-test the federation)

> Status: **design / approved** (2026-06-16). Adds a second backend vertical
> (LocalFS) to validate the backend trait surface end-to-end. The explicit goal
> is to **stress-test the abstraction** — prove a genuinely divergent backend
> plugs into the shared core without bloating it.
>
> Builds on:
> - [2026-06-16-architecture-principles-design.md](2026-06-16-architecture-principles-design.md) (north-star)
> - [2026-06-16-vertical-0-gmail-extraction-design.md](2026-06-16-vertical-0-gmail-extraction-design.md) (Vertical #0, shipped)
>
> **Acceptance bar:** existing 76 Rust tests stay green (Gmail path
> behavior-identical after trait promotion); a LocalFS vertical is usable from
> the real UI; an integration example round-trips a note through LocalFS on a
> temp dir; Gmail + LocalFS accounts coexist with search/graph spanning both.

## Goal & framing

The Vertical #0 work introduced the trait surface but kept Gmail as the only
implementor (static dispatch, fat orchestration as inherent methods). LocalFS is
the **hardest** second vertical precisely because it is the most divergent — it
exercises every seam the Pragmatic pass deferred: dynamic dispatch, an account
with no OAuth/keychain, a filesystem transport with a real cursor, raw-RFC822
decode (no pre-parsed JSON), and two verticals coexisting in one shared core.

**Scope decision (locked in brainstorming):** LocalFS is a *transport for the
same Apple-HTML/MIME world*, NOT a markdown backend. Notes are stored as `.eml`
files (RFC822 wrapping the existing Apple-HTML body). `content_kind` stays
`AppleHtml`, so the existing editor/viewer and `mime822` encode path are reused
unchanged. Markdown / a second content model is explicitly out of scope.

## Decisions locked in brainstorming (2026-06-16)

1. **Goal = stress-test the abstraction** (not a polished product).
2. **At-rest stays RFC822/HTML** (`.eml` files); reuse `mime822` + editor; no markdown.
3. **Orchestration = trait-per-vertical**, on principle (not compromise): `list_*`
   orchestration becomes trait methods each vertical implements its own way.
   Gmail's dedup-by-UUID is a **Gmail quirk** (transient duplicates from
   insert-then-trash + IMAP races) and stays inside the Gmail vertical — it is
   NOT promoted to core, because LocalFS (one file per uuid) has no such quirk.
   Core keeps only the genuinely generic parts (sort, cache upsert, conflict,
   sync_state, neutral index, prune, worker loop).
4. **Account = generalized** with `backend_kind` + `root_dir`; LocalFS account id
   is a minted uuid; **Full UI add** (an "Add Local Folder" button + native
   directory picker). Readiness = root dir exists/readable (no network/keychain).
5. **Change detection = full scan + core prune** (filesystem strong-consistency
   makes immediate prune safe — no tombstones needed, unlike Gmail's
   eventually-consistent list).

## Why this is the right stress test (the dedup insight)

The orchestration decision is the crux. `list_notes`'s dedup-by-UUID exists only
because Gmail has no REPLACE (insert-then-trash) and is eventually consistent, so
multiple messages can transiently share one UUID. LocalFS writes exactly one
`<uuid>.eml` per note and overwrites in place — **structurally one note per uuid**,
no dedup possible or needed.

Therefore "decompose dedup into core" would be a trap: it would push a
Gmail-specific quirk into the supposedly-neutral core, forcing LocalFS (and future
JMAP/Graph) to carry logic they don't need — violating *fat vertical, thin core*.
The honest classification a stress test should reveal: **dedup is a vertical
quirk; sort/cache/conflict/index/prune are general.** Hence trait-per-vertical for
the read strategy, with generic post-processing staying in core. This also keeps
the working, race-tested Gmail path **wrapped, not rewritten** (lowest risk).

## Component A — Core changes (trait promotion + dyn dispatch)

### A1. Promote inherent methods to trait methods

Today `lib.rs` calls inherent `GmailVertical` methods (`save_note_full`,
`list_notes`, `list_notes_in_label`, `list_account_index`, `fetch_note`,
`find_gmail_ids_for_uuid`, `list_pin_sidecars_in`, …) — not callable through
`&dyn Vertical`. Promote them onto traits so both verticals implement them and the
core dispatches dynamically. Proposed trait shape (refined during implementation):

```rust
// The note read/write/orchestration surface the core drives, per-vertical.
#[async_trait]
pub trait NoteStore: Send + Sync {
    async fn list_all_notes(&self, cache_by_id: &HashMap<String, Note>) -> Result<Vec<Note>, TransportError>;
    async fn list_notes_in_folder(&self, folder: &str, cache_by_id: &HashMap<String, Note>) -> Result<Vec<Note>, TransportError>;
    async fn list_index(&self) -> Result<Vec<MessageIndex>, TransportError>;
    async fn fetch_note(&self, remote_id: &str) -> Result<Note, TransportError>;
    async fn save_note_full(&self, op: &SaveOp<'_>, attachments: &[Attachment]) -> Result<SavedNote, TransportError>;
    async fn find_ids_for_uuid(&self, uuid: &str) -> Result<Vec<String>, TransportError>;
    async fn list_trashed(&self) -> Result<Vec<TrashedNote>, TransportError>;
    async fn untrash(&self, remote_id: &str) -> Result<(), TransportError>;
}
```

`Transport` (save/delete/folders/changes_since) and `MetadataSidecar` already
exist as traits. `Vertical` composes them all + `Identity` + `Deriver` +
`Capabilities`, and exposes accessors so the core can reach each facet through one
`&dyn Vertical`. Gmail's dedup stays inside `GmailVertical::list_all_notes`.

### A2. Dynamic dispatch

`AppState` gains a way to resolve a `Box<dyn Vertical>` (or `Arc<dyn Vertical>`)
per account. The current `gmail_vertical(token, label_map, account_id)` helper is
replaced by:

```rust
async fn vertical_for(state: &AppState, account_id: &str) -> Result<Box<dyn Vertical>, String>
```

which branches on `account.backend_kind`:
- **Gmail:** runs the existing bootstrap (`ensure_token` → `cached_label_map`) and
  builds `GmailVertical` (unchanged behavior).
- **LocalFs:** reads `root_dir` from the account and builds `LocalFsVertical` — no
  token, no label map, no network.

The sync worker and Tauri commands call `vertical_for(...)` then drive everything
through `&dyn Vertical`. The Gmail-only bootstrap (`ensure_token`/`cached_label_map`)
moves inside the Gmail branch of `vertical_for`, so LocalFS never touches it.

### A3. Move neutral envelope types to the core

Move `Note`, `Attachment`, `SavedNote` (and `MessageIndex`, `TrashedNote`,
`DedupSummary` as needed) from `backend/gmail/wire.rs` to `backend/mod.rs` (or
`backend/types.rs`) — they are already format-neutral. The Gmail-JSON structs
(`GmailMessage`, `Payload`, `Part`, `Body`, `Header`) and `SidecarRef`/
`TagSidecarRef` stay in `wire.rs` (Gmail-specific). Both verticals then share the
neutral types; `SaveOp`/`SaveOutcome` already live in core.

## Component B — LocalFsVertical (`backend/localfs/`)

```
src-tauri/src/backend/localfs/
├── mod.rs          # LocalFsVertical { root_dir, account_id } + Vertical + Capabilities
├── transport.rs    # FS Transport + NoteStore + MetadataSidecar impls
└── decode.rs       # raw-RFC822 → neutral envelope (Component C)
```

Identity + Deriver are identical to Gmail (same Apple-HTML content model) — factor
a shared `AppleHtmlDeriver` and reuse the same `mint` (both call
`mime822::format_apple_uuid(Uuid::new_v4())`).

### Storage layout (under the account's `root_dir`)

```
<root>/
├── Notes/                       # the Notes tree = folder hierarchy
│   ├── <uuid>.eml               # a note in folder "Notes"
│   └── play5/<uuid>.eml         # a note in folder "Notes/play5"
├── .trash/<uuid>.eml            # deleted notes (Recently Deleted parity, restorable)
└── .meta/                       # Jodd-local sidecars
    ├── <uuid>.pin               # pin sidecar (existence = pinned)
    └── <uuid>.tags.json         # tags sidecar  {"tags":[...]}
```

- **folder ↔ subdirectory**: `Notes/play5` ↔ `<root>/Notes/play5/`. `folder_model
  = SingleExclusive` (a file lives in exactly one directory). `ensure_folder` =
  `mkdir -p`; `rename_folder` = rename dir; `delete_folder` = remove (empty) dir;
  `move_note` = move the file between dirs.
- **save_note_full**: `mime822::build_note_mime(...)` → write `<root>/<folder>/<uuid>.eml`
  (overwrite in place). `remote_id` = the file's path relative to root. Edit
  overwrites the same file → `remote_id` is **stable** across edits (unlike
  Gmail's churning id) — this validates the core handles a stable-id transport.
  If the folder changed, move the file to the new dir.
- **list_all_notes / list_notes_in_folder**: walk `*.eml` (whole tree / one
  subdir) → `decode` each → `Note`. No dedup (one file per uuid). `label` derived
  from the file's directory path.
- **fetch_note**: read one file → decode.
- **delete**: move the file to `<root>/.trash/<uuid>.eml` (restorable). `list_trashed`
  reads `.trash/`; `untrash` moves it back to its folder.
- **changes_since**: implemented as a full scan returning the current note set,
  with `next_cursor` = the scan timestamp (a real, non-inert `SyncCursor`,
  validating the cursor plumbing). The worker driver remains
  `list_all_notes` + the core's existing prune (FS strong-consistency → prune
  immediately, no tombstone needed).
- **MetadataSidecar**: pin = presence of `.meta/<uuid>.pin`; tags = `.meta/<uuid>.tags.json`.
  `put_sidecar`/`list_sidecars`/`remove_sidecar` map to file create/scan/delete.
  This exercises the sidecar seam on a non-Gmail vertical and is portable if the
  folder is synced (Dropbox/git).

## Component C — raw-RFC822 decode (the one new at-rest piece)

Gmail returns pre-parsed JSON; a `.eml` file is raw bytes, so LocalFS must parse
the MIME envelope itself. `decode(bytes) -> DecodedNote`:
1. Parse headers: `Subject` (RFC2047-decoded → title), `X-Universally-Unique-Identifier`,
   `Date`, `X-Mail-Created-Date`.
2. Walk the MIME structure: find the `text/html` part (handle `multipart/related`
   boundaries), decode its transfer-encoding (`quoted-printable` / `base64` / `7bit`).
3. Collect attachment parts (Content-Id, mime, bytes) the same way the Gmail path does.
4. Hand the extracted HTML to the **existing `mime822` helpers** (`strip_leading_title`,
   etc.) to produce the editor-view body — identical post-processing to Gmail.

**Dependency decision:** use the `mail-parser` crate (pure-Rust, robust) to parse
the envelope, then map its output to the neutral envelope. Rationale: MIME has many
edge cases (header folding, encodings, nested multipart); hand-rolling risks
subtle Apple-incompatibility, and a crate keeps LocalFS decoupled from Gmail's
`Part`/`Body`/`Header` structs. (Alternative considered: hand-roll a minimal parser
for our own written format — rejected as fragile and a poorer reuse story.)

**Symmetry test:** a unit test feeds `build_note_mime(...)` output back through
`decode(...)` and asserts the envelope (uuid, title, date, body, attachments)
round-trips. This proves the write↔read pair is consistent.

## Component D — Account model + frontend

### D1. Account generalization

```rust
pub enum BackendKind { Gmail, LocalFs }   // serde default = Gmail (back-compat)

pub struct Account {
    pub id: String,                  // Gmail: email; LocalFs: minted uuid
    pub email: String,               // LocalFs: display name (folder basename)
    #[serde(default)] pub backend_kind: BackendKind,
    #[serde(default)] pub root_dir: Option<String>,   // LocalFs only
    // existing Gmail fields (notes_label, meta_label, …) stay, unused by LocalFs
}
```

`serde(default)` keeps existing `accounts.json` valid (old accounts → `Gmail`,
`root_dir = None`).

### D2. Readiness (doctrine: readiness ≠ network)

`account_usable` / the readiness check branches on `backend_kind`:
- **Gmail:** has a refreshable refresh token (existing logic).
- **LocalFs:** `root_dir` exists and is a readable directory. No keychain, no network.

### D3. Add / remove

- New Tauri command `add_local_account(path: String)`: validate the dir, mint an
  account id, derive `email` (display name) from the basename, persist to
  `accounts.json`, index it (cold-start path that builds the LocalFs vertical and
  reconciles folders/notes from the tree).
- Frontend: an **"Add Local Folder"** entry (in `AuthScreen` / the account area)
  → `tauri-plugin-dialog` open-directory picker → `invoke('add_local_account', …)`.
  (Confirm `tauri-plugin-dialog` is enabled; add it if missing.)
- `remove_account` works for LocalFs (drops the cache rows + accounts.json entry;
  never deletes the user's files).

### D4. Rest of the UI is unchanged

Because `content_kind = AppleHtml`, the LocalFs account renders in the sidebar like
a Gmail account and reuses the existing folder tree, note list, and editor. A small
visual marker (📁) distinguishes a local account. No editor/viewer work.

## Component E — Sync worker

The worker tick already iterates accounts. For each, it calls
`vertical_for(state, account)` and drives push/pull through `&dyn Vertical`. The
Gmail-specific `ensure_token`/`cached_label_map` bootstrap moves inside the Gmail
branch of `vertical_for`, so a LocalFs account simply skips it. Pin/tags drain,
content push, deletes, and pull all route through trait methods — identical control
flow for both verticals; only the concrete impl differs.

## Verification

- **Unit:** raw-RFC822 round-trip (`build_note_mime` → `decode` → assert envelope);
  LocalFS folder/path ↔ label mapping; sidecar file create/scan/delete.
- **Existing suite:** all 76 tests green — the Gmail path must be behavior-identical
  after trait promotion + type relocation.
- **Integration example** `examples/roundtrip_localfs.rs` on a **tempdir** (no
  network, no real account, CI-friendly): add a LocalFs account → save → list →
  edit (stable id, no dup) → pin (sidecar file appears) → move between folders →
  delete (→ `.trash`) → untrash → assert each step. Mirrors `roundtrip_refactor.rs`.
- **Cross-vertical:** with a Gmail and a LocalFs account both present, confirm the
  neutral index (FTS search, tags, graph) returns results spanning both backends —
  the one universal read-model over the federation.
- **Live (optional, manual):** add a real local folder in the running app, create a
  note, confirm the `.eml` file appears on disk and re-opens correctly.

## Implementation order (phases)

1. **Phase A — core**: relocate neutral types; define `NoteStore` (+ compose into
   `Vertical`); promote Gmail's inherent methods to trait impls; introduce
   `vertical_for` + `Box<dyn Vertical>`; route `lib.rs` + worker through it. Gmail
   behavior-identical; 76 tests green. *(Highest risk — touches the working path.)*
2. **Phase B — LocalFS vertical**: `backend/localfs/` (Transport + NoteStore +
   MetadataSidecar + decode via `mail-parser`); unit tests + `roundtrip_localfs.rs`.
3. **Phase C — account + UI**: `Account` generalization, readiness branch,
   `add_local_account` + dialog button, sidebar marker; cross-vertical check.

## Deferred (door open, not built)

- Markdown / a second `content_kind` and editor (the whole point of *this* scope is
  to NOT need it yet).
- Real incremental mtime cursor (full-scan + prune is enough; cursor type is
  exercised, optimization deferred).
- OS file-watching (notify/FSEvents) for real-time external edits.
- Importing arbitrary external `.eml` (we read what we write; `mail-parser` makes
  import feasible later).
- `accounts.sync_cursor` storage column (still deferred from Vertical #0).

## Open questions (non-blocking)

- Exact `Vertical` accessor shape for `NoteStore`/`Transport`/`MetadataSidecar`
  facets vs. one flat trait — settle during Phase A for minimal churn.
- Whether `list_index` (cheap account-wide index) is needed for LocalFS or whether
  `list_all_notes` suffices (a local scan is already cheap) — likely fold together.
- Conflict semantics when the same `.eml` is edited externally while dirty locally
  — reuse the existing keep-both `reconcile_one` (works on `Note`s regardless of
  vertical); verify on a LocalFs row during Phase B.
