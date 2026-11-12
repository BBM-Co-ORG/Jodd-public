# Architecture & Design Principles — "Local cache, pluggable backends"

> Status: **design / north-star** (2026-06-16). Direction-setting, not an
> implementation mandate. Defines the vocabulary, the two doctrines, the target
> information architecture (IA), the **trait surface**, and the shared/per-vertical
> seam that every future feature and refactor should conform to. The current app
> is reframed as the first "vertical"; migration is incremental.
>
> Incorporates the seam-proposals review (2026-06-16). Verdicts grounded in the
> current code are recorded inline; where the review and the code disagreed, the
> code won.

## Headline

**Local cache, pluggable backends.**

> Plain language: *"Your notes always live on your device. Gmail, a markdown
> folder, whatever — those are backends you plug in, and each one is its own
> world."*

Jodd is a **local cache fronting a federation of self-contained backends**. The
cache is the user's reality for reading and editing. Each backend is an
independent vertical — its own transport, storage format, identity rules, editor,
and view — that plugs into a shared, backend-agnostic core. Backends do **not**
need to understand each other.

## Why "local-first" is retired as the headline

The classic local-first definition (Ink & Switch) means *the local copy is the
ultimate authority and sync is an optional enhancement*. That over-claims what
Jodd owns:

- For Apple-interop backends, **durability and identity (the Apple UUID) actually
  live in the backend** (Gmail/Apple), not locally. The cache is authoritative for
  the *moment* and the *UX*, not for ultimate truth.
- The defining idea here is **pluggability and federation** — which "local-first"
  says nothing about.

"Local-first" survives as a *property of the hot path* (reads/writes hit the
cache, never block on the network), not as the name of the system.

## The two doctrines

Everything derives from two sentences.

### 1. Data doctrine — the cache is the truth-of-the-moment

Every user action writes synchronously to SQLite and updates in-memory/DOM state
in the same logical step (optimistic, with rollback). Reads come from the cache.
The background sync worker pushes asynchronously. **No normal navigation or edit
ever blocks on the network.**

Scope note: "never blocks" targets **network latency**. Local-only work in the
write path — SQLite writes, cheap structural decode, even index derivation — is
CPU/millisecond work and does **not** violate the doctrine. Do not move local work
off the write path on doctrine grounds alone; move it only when measured to hurt.

### 2. Structural doctrine — each backend is a self-contained world; the core is the common ground

A backend is a complete vertical:
`(wire transport × at-rest form) + identity + content model + editor + view +
deriver + declared capabilities`.

Backends do **not** interop at the content level — you cannot losslessly open a
markdown note in the HTML editor, and you are not required to. What they share is
the core: identity envelope, sync engine loop, conflict policy, app shell, and a
format-neutral derived index.

## Vocabulary

| Term | Meaning |
|---|---|
| **Vertical / backend** | A self-contained plugin: transport + at-rest form + identity + content model + editor + view + deriver + capabilities. |
| **Wire transport** | How bytes move: Gmail REST, IMAP, MS Graph, WebDAV, local FS, NFS/SMB. |
| **At-rest form** | How a note is serialized on the backend: MIME/RFC 822, markdown file, JSON. |
| **Shared core** | Backend-agnostic infrastructure: cache envelope, sync worker **loop**, `sync_state` machine, conflict **policy**, accounts/keychain, app shell, neutral index. |
| **Envelope** | The format-neutral cache row the core understands (identity, folder, title, dates, sync state, opaque content payload). |
| **Content payload** | The vertical's serialized note content. **Opaque to the core**; only the owning vertical interprets it. |
| **Remote id** | The backend-native id (Gmail msg id, IMAP UID, Graph id). **Moves** (Gmail mints a new id on every content edit) and is **not** the identity. Stored beside the stable `uuid`. |
| **Identity (`uuid`)** | The stable, vertical-minted note identity. The cache PK. Never the remote id. |
| **Sync cursor** | A backend's "position" (Gmail historyId, Graph deltaLink, IMAP UIDVALIDITY+MODSEQ, mtime). **Opaque, vertical-owned**; the core persists it and loops, never opens it. |
| **Deriver** | The one bridge from a vertical to the core: parses content into neutral `{ text, tags[], edges[] }` for the shared index. |
| **Capability** | A property a vertical declares **and the core branches on**. If the core never branches on it, it is documentation/telemetry, not a capability. |

## The trait surface (lock this before edge #1)

The load-bearing artifact is the trait **method set**, not the ~70 `gmail::*` call
sites. If the boundaries below aren't in the surface from the start, edge #1 routes
70 sites through a trait missing four boundaries and gets reopened. The call-site
mechanics are trivial once the surface is right.

A vertical is split into small traits so modules (`mime822`, `localfs`) are
reusable rather than a forced matrix. Illustrative Rust (shape, not final):

```rust
// Opaque, vertical-interpreted sync position — treated exactly like content_blob.
pub struct SyncCursor(pub Vec<u8>);

pub enum ChangeKind { Upserted, Deleted }
pub struct RemoteChange { pub remote_id: RemoteId, pub kind: ChangeKind, pub folder_hint: Option<FolderRef> }
pub struct ChangeSet   { pub changes: Vec<RemoteChange>, pub next_cursor: SyncCursor, pub more: bool }

// TRANSPORT — wire ops + change detection. Core owns the LOOP; transport owns
// "what changed" + cursor semantics. `changes_since(None)` = full bootstrap;
// this is where historyId / deltaLink / UIDVALIDITY+MODSEQ / mtime hide.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn changes_since(&self, cursor: Option<&SyncCursor>) -> Result<ChangeSet, TransportError>;
    async fn fetch(&self, id: &RemoteId) -> Result<RawNote, TransportError>;
    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError>; // Gmail insert+trash / Graph PATCH / IMAP APPEND+EXPUNGE — one method
    async fn delete(&self, id: &RemoteId) -> Result<(), TransportError>;
    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError>;
}
pub struct SaveOutcome { pub remote_id: RemoteId /* may DIFFER from input — Gmail */, pub cursor_hint: Option<SyncCursor> }

// Backoff split: transport CLASSIFIES (reads 429/Retry-After/Graph header);
// shared worker owns the retry policy + jitter. Neither side owns both.
pub enum TransportError {
    RateLimited { retry_after: Option<Duration> },
    Transient   { source: anyhow::Error },
    Conflict    { remote_etag: Option<String> }, // core triggers pull + reconcile
    Auth,                                         // soft re-auth prompt — NOT "offline"
    NotFound,                                     // reconcile as remote-delete
    Permanent   { source: anyhow::Error },
}

// AT-REST — bytes <-> envelope + opaque payload. `mime822` implements this,
// shared by gmail/imap/graph. decode() fills the CHEAP structural envelope
// fields INCLUDING `title` (MIME Subject / markdown first heading). Title is
// NOT the deriver's job, and stays off any async path.
pub trait AtRest: Send + Sync {
    fn decode(&self, raw: &RawNote) -> Result<DecodedNote, AtRestError>;
    fn encode(&self, note: &NoteRow) -> Result<RawNote, AtRestError>;
}

// IDENTITY — mint + conflict rekey, VERTICAL-OWNED. Core owns the keep-both
// POLICY; the vertical owns the blob rewrite (fresh Message-ID, attachment CID,
// in-body self-links) because only it understands the payload.
pub trait Identity: Send + Sync {
    fn mint(&self) -> NoteId;
    fn rekey_for_conflict_copy(&self, original: &NoteRow) -> Result<NoteRow, IdentityError>;
}

// DERIVER — content -> neutral index. SYNCHRONOUS today (local CPU/ms; see
// data-doctrine scope note). Edges are TYPED.
pub struct Derived { pub text: String, pub tags: Vec<String>, pub edges: Vec<Edge> }
pub trait Deriver: Send + Sync {
    fn derive(&self, kind: ContentKind, blob: &[u8]) -> Result<Derived, DeriveError>;
}

// VERTICAL — composition + declared capabilities (only the ones the core branches on).
pub trait Vertical: Send + Sync {
    fn backend_id(&self)   -> BackendId;
    fn transport(&self)    -> &dyn Transport;
    fn at_rest(&self)      -> &dyn AtRest;
    fn identity(&self)     -> &dyn Identity;
    fn deriver(&self)      -> &dyn Deriver;
    fn capabilities(&self) -> &Capabilities;
}
```

The shared worker loop (cursor meaning = vertical; loop + persistence = core):

```rust
let mut cursor = store.load_cursor(account);          // None on first run / no-cursor backend
loop {
    let set = vertical.transport().changes_since(cursor.as_ref()).await?;
    for ch in set.changes { core.reconcile(account, ch).await?; }   // generic
    store.save_cursor(account, &set.next_cursor);                   // never inspected
    cursor = Some(set.next_cursor);
    if !set.more { break; }
}
```

> **What ships in the trait now vs. later.** `changes_since(cursor)` is in the
> surface from day one so full-scan polling isn't baked in — but the
> `accounts.sync_cursor` storage column and any real cursor logic are **deferred**
> until a cursor-based vertical exists (today's Gmail path full-scans; a no-cursor
> vertical returns an inert cursor). Define the boundary, defer the storage.

## The shared / per-vertical seam (the contract)

Keeping this line clean is the whole point of "flexible without being haphazard."

### Shared core owns (vertical never touches)

- Account model + keychain.
- Sync worker **loop** + the **`sync_state` machine**
  (`clean | dirty | pull_needed | conflict | deleted_pending` + folder equivalents).
- Conflict reconciliation **policy** (keep-both). The *mechanics* of minting a
  conflict copy delegate to `Identity::rekey_for_conflict_copy` — the core never
  opens the blob.
- App shell / IA (sidebar, folder tree, note list, account picker).
- Neutral derived index (FTS5 text, `note_tags`, typed `edges`) — format-agnostic
  by construction (text is text, a tag is a string, an edge is a typed UUID/tag
  reference). This is the federation's one **universal derived read-only model**:
  search and graph span verticals even though content does not interop.

### Each vertical owns

Wire transport · at-rest serializer · identity (mint + conflict rekey) · content
model · editor · viewer · deriver. The deriver is the single small bridge to the
core; everything else stays inside the vertical.

### Cache shape — shared envelope + opaque content payload

```sql
CREATE TABLE notes (
    account_id    TEXT    NOT NULL,
    uuid          TEXT    NOT NULL,   -- minted identity, globally unique by construction
    backend_id    TEXT    NOT NULL,   -- functional dependency of uuid
    id            TEXT    NOT NULL,   -- remote id (MOVES: Gmail re-mints on edit)
    title         TEXT    NOT NULL,   -- from AtRest::decode (cheap structural), not the deriver
    label         TEXT    NOT NULL,   -- folder; single value (Apple folders are exclusive — see below)
    -- created / modified / synced timestamps ...
    sync_state    TEXT    NOT NULL,
    content_kind  TEXT    NOT NULL,   -- which editor/viewer the UI loads
    body_html     BLOB    NOT NULL,   -- the content payload — OPAQUE to the core
    PRIMARY KEY (account_id, uuid)
);
```

Notes on the shape (and what the review proposed vs. what the code already does):

- **Identity is already separated.** Today's schema already keeps a stable `uuid`
  PK and a moving `id` (remote msg id) in the same row, and re-points `id` on every
  push (`mark_pushed`). This already neutralizes Gmail's id churn. A `note_remote_ids`
  side table would be normalization, not added correctness — **deferred** (keep the
  column) until a real query need appears.
- **Title is structural, not derived.** It comes from `AtRest::decode` (MIME Subject
  today), so the note list renders instantly with no derive pass.
- **`content_kind`** lets the UI auto-select the right editor/viewer — a note can
  never open in the wrong editor.
- **`content_schema_version`** (multi-device skew guard) is a planned envelope
  column, **deferred** until the content model grows; policy when it lands: opening
  a blob newer than this app can render → read-only banner, never downgrade-rewrite.

### Folder model — single value now, `FolderModel` capability reserved

Gmail labels are technically many-to-many, but **Apple Notes folders are
exclusive** (a note lives in exactly one `Notes/*` folder). The shipping code
already collapses any multi-label case to one via `pick_notes_label`
(deepest-first) — an intentional simplification matching Apple semantics, not a
bug. So:

- Keep the **single `label`** field for the Apple vertical (`FolderModel::SingleExclusive`).
- Declare `FolderModel` as a **capability the core branches on** (shell rendering:
  tree vs. label-chips; exclusivity enforcement). This is a legitimate
  "axis-B-the-core-must-know" case.
- **Defer** the M:N `note_folders` join table — needed only by a future vertical
  that reads arbitrary (non-Apple) labels, which is not on the roadmap.

### Capabilities — only what the core branches on

A capability is legitimate only if the **shared core (or shell)** changes behavior
on it. Rule of thumb: *if you can't name the line that reads it, it's decoration —
push it into the vertical.*

```rust
pub struct Capabilities {
    pub folder_model: FolderModel,  // REAL: shell rendering + exclusivity enforcement
    pub fidelity:     Fidelity,     // REAL: UI degradation warning ("some styling will degrade")
    // schema_current / schema_min_readable — added with content_schema_version (deferred)
}
```

`interops_with_apple` is **demoted**: the core never branches on it (conflict
rekey → `Identity`; "Apple drops sidecar metadata" → the vertical's `encode`). It
lives as shell/telemetry metadata (e.g. an "Apple" badge), not a method the core
calls. This maps onto the existing "compatibility tiers" in CLAUDE.md, promoted
from prose to structure — but only where structure earns its place.

### Deriver — synchronous, off the network, typed edges

- **Synchronous** in the write step (local CPU, milliseconds). Keep it sync until
  measured to hurt; async would add eventual-consistency complexity to solve a
  latency problem that does not exist (the doctrine targets *network*, not local
  compute). Title is excluded (it's `AtRest`'s job), so list rendering is unaffected.
- **Edges are already typed.** The `edges` table carries a `rel` kind
  (mentions / child_of / tagged) since migrations #11–13; the earlier "edge = UUID
  pair" phrasing in this doc was imprecise. A block-level `source_anchor` slot is
  **reserved** (nullable, unused today) — door open, YAGNI respected.

### Backends compose from reusable modules

`transport × at-rest form` are reusable **modules**, not a forced matrix:
`mime822` (at-rest) shared by Gmail/IMAP/Graph; a `localfs` transport reusable by
multiple verticals. A vertical *wires* a transport and an at-rest module together;
it does not re-implement them. This is where "reuse what already exists first"
lands.

## Key decisions and rationale

1. **Federation over a universal content hub.** Reject a single canonical content
   model that every backend projects from. Lossless cross-format conversion is the
   most expensive, riskiest problem in the space and is unnecessary — Apple Notes
   and markdown-native notes are genuinely different worlds. Federation dissolves
   the problem instead of solving it.
2. **MIME / RFC 822 is not the canonical format.** It is the at-rest form of the
   *email* transport family, behind a shared `mime822` module. MIME's warts
   (quoted-printable/base64, fragile multipart, header folding) must never leak
   into the core's sync, conflict, or index logic.
3. **Apple semantics is a capability, not a mandatory layer** — and a demoted one
   at that (the core doesn't branch on it). Only some backends interop with Apple.
4. **Fat vertical, thin core.** Verticals hide quirks behind a uniform interface;
   the core sees one `save`, one `changes_since`. Capability methods exist only
   where the core must branch (`folder_model`, `fidelity`).
5. **Readiness ≠ network.** "Is an account usable?" is answerable from local state.
   An offline backend is not an unusable account (see worked example 2).
6. **Derive, don't migrate.** Derived state whose truth lives elsewhere
   (`content_kind` ← backend, `title` ← at-rest decode, neutral index ← content,
   folder `kind` ← path, `content_schema_version` ← decode) is re-derived on every
   write/sync, never fixed up by a one-shot migration.

## Worked example 1 — Vertical #0: Apple-via-Gmail (reuse what exists)

The current app *is* the first vertical; behavior is unchanged, code is reorganized
to expose the seam:

- **transport** = existing Gmail REST calls (~70 `gmail::*` sites today).
- **at-rest** = `mime822` module extracted from `gmail.rs`, shared-ready for IMAP/Graph.
- **identity** = `uuid` already minted/preserved from Apple's X-UUID; `id` is the
  moving Gmail id.
- **content model** = the HTML subset the current editor produces.
- **editor / view** = existing `NoteEditor.svelte` + renderer.
- **deriver** = existing FTS / `note_tags` / typed `edges` derivation, formalized
  as the vertical's bridge, kept synchronous.
- **capabilities** = `folder_model = SingleExclusive`, full `fidelity`.
  (`interops_with_apple` = true, but as shell/telemetry metadata, not a core call.)

First concrete refactor = **Active edge #1**: extract the `mime822` at-rest module
and the `Transport`/`Identity`/`Deriver` traits, route the ~70 sites through them.
No user-visible change.

## Worked example 2 — Offline cold-start (a doctrine bug this design fixes)

Today, on a cold start while offline, `is_authenticated` calls `ensure_token`,
which refreshes the access token over the network (access tokens are not
persisted; only refresh tokens live in the keychain). Offline → refresh fails →
`is_authenticated` returns false → the UI shows the sign-in screen **even though a
full local cache exists**. A read path blocked on the network — a data-doctrine
violation.

Fix, in trait terms (principle 5):

```rust
fn account_usable(&self, acc: &Account) -> bool {
    self.has_local_cache(acc) || self.has_refreshable_creds(acc)
    // has_refreshable_creds = keychain HAS a refresh token (presence check),
    // NOT "refresh succeeded". Never touches the network.
}
```

A `TransportError::Auth` from a *sync* attempt surfaces a soft re-auth prompt; it
must not gate reaching the cache. This fix ships **first**, independent of the
Provider work. Alongside it, **audit other read-path network leaks** (lazy
attachment fetch, remote image load, any token refresh triggered by a read) — the
cold-start bug suggests the doctrine was aspirational, not enforced; one fix is not
an audit.

## Sequencing

**Lock before touching the 70 call sites** (these define the trait; deciding them
after the refactor means redoing it):

1. The **`Transport` method set** incl. `changes_since(cursor)`,
   `SaveOutcome { remote_id }`, and the `TransportError` classification enum.
2. **`Identity::{ mint, rekey_for_conflict_copy }`** — without it, Gmail id churn
   and conflict rekey have no home.
3. **`Capabilities`** with `FolderModel` + `fidelity` (the two the core branches on).

**Decide alongside, small / independently shippable:**

4. Offline cold-start `account_usable` fix + read-path leak audit — **ships first**.
5. Keep the deriver **synchronous**; formalize it as the vertical bridge.
6. Doc/wording fixes (typed edges + reserved block-anchor; title from `AtRest`).

**Deferred storage / features (door open, not built):**

- `accounts.sync_cursor` column and real cursor logic (define `changes_since` now).
- `note_folders` M:N join table (keep single `label`; declare `FolderModel`).
- `note_remote_ids` side table (keep the `id` column).
- `content_schema_version` + version-gating (add when the content model grows).
- LocalFS backend, editor/AST rewrite, cross-backend content interop.

## Open questions

- Exact envelope column evolution vs. today's schema: migrate lazily; current
  columns are a valid Apple-vertical specialization until a second vertical forces
  generalization.
- `transport × at-rest` as a true runtime matrix vs. fixed per-vertical bundles —
  revisit only when a real second combination exists.
- Conflict-copy proliferation UX over a flaky-sync lifetime — keep-both is
  mechanically correct; a GC/merge affordance is a product question, not
  architectural.
