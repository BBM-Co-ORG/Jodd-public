# Vertical #0 extraction — Apple-via-Gmail behind the backend trait surface

> Status: **design / approved** (2026-06-16). Implementation spec for **Active edge
> #1**: extract the email-backend Transport/at-rest abstraction out of `gmail.rs` and
> reframe the current app as "Vertical #0 (Apple-via-Gmail)".
>
> Parent / source of truth: [2026-06-16-architecture-principles-design.md](2026-06-16-architecture-principles-design.md).
> That document locks the *trait surface* and the shared/per-vertical seam; this
> document is the concrete, low-risk migration plan. Where the two differ, the
> parent governs intent and this governs execution.
>
> **Acceptance bar: behavior identical, Apple Notes round-trip intact, existing Rust
> tests still green.** No user-visible change. No schema migration.

## Goal

Introduce the backend-agnostic trait surface so a second backend (JMAP is the
concrete near-term driver) can be added without touching the ~70 `gmail::` call
sites in `lib.rs` again. The load-bearing artifact is the **trait method set**, not
the call-site mechanics.

This pass *draws traits around seams that already exist* and *extracts a reusable
`mime822` module*. It deliberately does **not** untangle the intricate, race-
sensitive dedup/sort/cache-reuse orchestration inside `list_notes` — that work moves
to the core only when JMAP forces it.

## Decisions locked in brainstorming (2026-06-16)

1. **Static dispatch first.** With one vertical today, `AppState` holds a concrete
   `GmailVertical`; call sites use static dispatch through its trait methods. No
   `Box<dyn Vertical>` yet. The trait surface is identical to the eventual dyn
   version, so adding dyn dispatch when JMAP lands is a cheap, localized change.
2. **Add `MetadataSidecar` as a 5th trait.** The shared sync worker's pin/tags
   drain currently calls `gmail::save_meta_sidecar` / `trash_meta_sidecar` directly —
   a core-side caller of a backend-specific mechanism, i.e. a legitimate seam.
   Abstracting it removes `gmail::` hardcoding from shared worker code in this pass.
3. **Pragmatic `list_notes` scope.** Keep the existing list/dedup/sort/cache-reuse
   orchestration as methods on the Gmail vertical (behavior untouched). The clean
   `Transport` primitives (`changes_since`/`fetch`/`save`/`delete`/`list_folders`)
   are *defined and implemented*; decomposing the fat list functions into core-side
   dedup is **deferred** to the JMAP work.

## Roadmap impact assessment (why the surface is right)

| Roadmap item | Touches trait surface? | Rationale |
|---|---|---|
| **JMAP backend** | No — it *validates* the surface | New *wire transport*; at-rest stays MIME/RFC822 → reuses `mime822`. JMAP has a real sync cursor (`/changes` + state string) → exercises `changes_since(cursor)` + `SyncCursor`. Apple-via-JMAP reuses `Identity` (X-UUID). Only a new `Transport` impl. |
| **LLM wiki / workflows** (Karpathy-style) | No — orthogonal | Content-transformation layer producing notes that flow through the normal vertical. Touches only the **Deriver** (LLM emits inline `#tags` / `[[links]]` → derived to index). `LessonProvider` (LLM) is orthogonal to the backend trait (CLAUDE.md edge #4). |
| **Dynamic / smart folders** | No — reinforces it | SQL queries over the neutral index (tags/edges/date), not real backend folders → core/shell concern *above* the vertical seam. Shell must distinguish real folder (`label`, backend-backed) from virtual folder (query, core-backed). |
| **Workflow folders** (`__Extracts__`) | No | Folder `kind` derived from path (derive-don't-migrate); an envelope attribute. |

Conclusion: no roadmap item forces a change to the locked trait surface; they mostly
validate it. The one invariant to preserve: **the Deriver stays content-model-aware,
and `text`/`tags`/`edges` remain the universal neutral model.**

## Module layout

```
src-tauri/src/
├── backend/
│   ├── mod.rs           # traits + neutral value/error types (the locked surface)
│   └── gmail/
│       ├── mod.rs        # GmailVertical: composes parts, declares Capabilities,
│       │                 #   keeps list_notes / list_notes_in_label /
│       │                 #   list_account_index orchestration (dedup/sort untouched)
│       ├── transport.rs  # HTTP → Transport impl + folder/label ops + MetadataSidecar impl
│       └── identity.rs   # mint + rekey_for_conflict_copy
├── mime822.rs            # AtRest module: decode/encode + ALL pure MIME/Apple fns
│                         #   moved out of gmail.rs, WITH the 14 existing unit tests
├── gmail.rs              # shrinks to a thin re-export shim during migration; removed at end
```

- `mime822.rs` is the highest-value, most reusable artifact. Moving the 14 existing
  unit tests *with* the functions gives an instant regression net for the extraction.
- Static dispatch ⇒ `AppState` field changes from ad-hoc Gmail state to a concrete
  `GmailVertical` (or keeps existing fields and gains the vertical as a façade —
  decided in the plan; either way no dyn).

## The trait surface

Lives in `backend/mod.rs`. Shapes below are illustrative Rust (final signatures
settled during implementation); the **method set** is the contract.

### Neutral value & error types

```rust
pub struct SyncCursor(pub Vec<u8>);          // opaque, vertical-owned; core never inspects

pub enum ChangeKind { Upserted, Deleted }
pub struct RemoteChange { pub remote_id: String, pub kind: ChangeKind, pub folder_hint: Option<String> }
pub struct ChangeSet   { pub changes: Vec<RemoteChange>, pub next_cursor: SyncCursor, pub more: bool }

pub struct SaveOutcome { pub remote_id: String /* MAY DIFFER from input — Gmail re-mints */, pub cursor_hint: Option<SyncCursor> }

pub enum TransportError {
    RateLimited { retry_after: Option<std::time::Duration> },
    Transient   { source: anyhow::Error },
    Conflict    { remote_etag: Option<String> }, // core triggers pull + reconcile
    Auth,                                         // soft re-auth prompt — NOT "offline"
    NotFound,                                     // reconcile as remote-delete
    Permanent   { source: anyhow::Error },
}
```

Migration note: existing functions return `Result<_, String>`. Transport methods
return `Result<_, TransportError>`; a single `classify(status, body) -> TransportError`
helper maps Gmail HTTP responses (429/Retry-After → `RateLimited`, 401 → `Auth`,
404 → `NotFound`, 5xx → `Transient`, else `Permanent`). Backoff *policy* stays in the
worker; the transport only *classifies*.

### Transport — wire ops + change detection

```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn changes_since(&self, cursor: Option<&SyncCursor>) -> Result<ChangeSet, TransportError>;
    async fn fetch(&self, id: &str) -> Result<RawNote, TransportError>;
    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError>;
    async fn delete(&self, id: &str) -> Result<(), TransportError>;
    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError>;

    // Folder/label mutation — used by local-first folder ops in lib.rs.
    async fn ensure_folder(&self, path: &str) -> Result<RemoteFolder, TransportError>;
    async fn create_folder(&self, name: &str) -> Result<RemoteFolder, TransportError>;
    async fn rename_folder(&self, id: &str, new_name: &str) -> Result<(), TransportError>;
    async fn delete_folder(&self, id: &str) -> Result<(), TransportError>;
    async fn move_note(&self, id: &str, add: &[String], remove: &[String]) -> Result<(), TransportError>;
}
```

`changes_since` is **defined and implemented now** (Gmail: full-scan via
`list_account_index`, `next_cursor` = inert sentinel, `more = false`) so polling
isn't baked into the surface. The worker is **not** rewired onto the
`changes_since` loop in this pass (see "Worker loop").

### AtRest (= `mime822`) — bytes ⟷ envelope + opaque payload

```rust
pub trait AtRest: Send + Sync {
    fn decode(&self, raw: &RawNote) -> Result<DecodedNote, AtRestError>;
    fn encode(&self, note: &NoteForEncode) -> Result<RawNote, AtRestError>;
}
```

`decode` fills the cheap structural envelope **including `title`** (from the MIME
Subject header) — title is `AtRest`'s job, never the deriver's, and stays off any
async path. `mime822` absorbs the existing pure functions: `get_header`,
`try_recover_mis_decoded_utf8`, `is_ascii`, `format_apple_uuid`, `canonicalize_uuid`,
`format_apple_date`, `rfc2047_encode_subject`, `qp_encode_body`, `strip_html_tags`,
`first_block_or_embed`, `first_line_split`, `inject_title_into_body`,
`strip_leading_title(_once)`, `decode_body`, `find_html_in_parts`, `decode_b64_bytes`,
`header_param`, `referenced_cids`, `base64_mime_wrap`, `data_uri`, plus the
attachment-collection parse and the raw RFC 822 builder currently inside `save_note`.

The Gmail `Transport::fetch` returns the raw message; `AtRest::decode` parses it.
For the Pragmatic pass, `fetch_note`'s current fused fetch+parse may stay as a Gmail
vertical method that internally calls `mime822::decode` — the requirement is that the
*pure parse/build logic physically lives in `mime822`*, not that every fused function
is split this pass.

### Identity — mint + conflict rekey (vertical-owned)

```rust
pub trait Identity: Send + Sync {
    fn mint(&self) -> String;                                   // Apple-format UUID
    fn rekey_for_conflict_copy(&self, original: &NoteRow) -> Result<NoteRow, IdentityError>;
}
```

Core owns the keep-both *policy* in `reconcile_one`; the vertical owns the blob
rewrite (fresh UUID + title suffix + any in-body self-link fixups). This formalizes
the existing conflict-copy logic; behavior unchanged.

### Deriver — content → neutral index (synchronous)

```rust
pub struct Derived { pub text: String, pub tags: Vec<String>, pub edges: Vec<Edge> }
pub trait Deriver: Send + Sync {
    fn derive(&self, kind: ContentKind, blob: &[u8]) -> Result<Derived, DeriveError>;
}
```

Wraps the **existing** FTS-text / `note_tags` / typed-`edges` derivation. Kept
**synchronous** (local CPU/ms; the data doctrine targets *network* latency, not local
compute). **Links/edges and tags are body-resident and re-derived everywhere — they
are never carried in a sidecar.** This pass formalizes the seam (define the trait,
have `GmailVertical` implement it by delegating to the current derivation helpers);
moving the helper code's physical home is a low-priority sub-task, not a behavior
change.

### MetadataSidecar — Jodd-local cross-instance metadata (backend-specific channel)

```rust
pub enum SidecarKind { Pin, Tags }
pub struct SidecarRef { pub id: String, pub uuid: String, pub kind: SidecarKind, pub body: Option<Vec<u8>> }

#[async_trait]
pub trait MetadataSidecar: Send + Sync {
    async fn list_sidecars(&self, kind: SidecarKind) -> Result<Vec<SidecarRef>, TransportError>;
    async fn put_sidecar(&self, uuid: &str, kind: SidecarKind, body: Option<&[u8]>) -> Result<String, TransportError>;
    async fn remove_sidecar(&self, id: &str) -> Result<(), TransportError>;
}
```

Gmail impl = meta-label messages (the existing `___<uuid>` / `tags___<uuid>` subject
conventions, X-UTI `app.jodd.metadata`). This is the metadata-sync pattern for data
that **cannot** live in the body (pin is binary; the tags sidecar is legacy now that
tags are inline `#hashtag`). Future verticals implement their own channel (JMAP: its
own mechanism; LocalFS: a dotfile). Only pin/tags use it — links/text/tags-in-body
do not.

### Vertical — composition + capabilities

```rust
pub struct Capabilities {
    pub folder_model: FolderModel,  // SingleExclusive — shell renders a tree, enforces exclusivity
    pub fidelity:     Fidelity,     // Full — no UI degradation warning
}
pub trait Vertical: Send + Sync {
    fn backend_id(&self) -> &str;
    fn transport(&self) -> &dyn Transport;
    fn at_rest(&self) -> &dyn AtRest;
    fn identity(&self) -> &dyn Identity;
    fn deriver(&self) -> &dyn Deriver;
    fn sidecar(&self) -> &dyn MetadataSidecar;
    fn capabilities(&self) -> &Capabilities;
}
```

`interops_with_apple` is **demoted** — shell/telemetry metadata (e.g. an "Apple"
badge), not a method the core branches on. `folder_model` and `fidelity` are the
only capabilities the core/shell branches on, and both are constant for this vertical.

## Worker loop & `changes_since`

The shared sync worker keeps its current structure and calls the existing
list/save/delete paths **through the new trait methods** (Pragmatic). Concretely:

- Pull paths (`list_notes`, `list_notes_in_label`, `list_account_index`) remain Gmail
  vertical methods with their dedup/sort/cache-reuse logic byte-for-byte unchanged.
- Push paths route `save_note`/`delete_note` through `Transport::save`/`delete`.
- The pin/tags drain routes through `MetadataSidecar` instead of `gmail::` directly.
- `changes_since` is implemented but the worker is **not** rewired onto the
  cursor loop. That rewire (and the `accounts.sync_cursor` storage column) ships with
  JMAP.

No behavior change to sync timing, conflict handling, or dedup.

## Explicitly deferred (door open via trait, NOT built)

- `accounts.sync_cursor` storage column + real cursor logic (Gmail returns an inert
  cursor today).
- Decomposing `list_notes` dedup/sort/cache-reuse into core-side generic logic.
- `note_folders` M:N join table (keep single `label`; `FolderModel::SingleExclusive`).
- `note_remote_ids` side table (keep the `id` column — identity already separated).
- `content_schema_version` + version-gating.
- Envelope columns (`backend_id`, `content_kind`) — **no schema migration this pass**.
- `Box<dyn Vertical>` dynamic dispatch (added with JMAP).
- LocalFS backend, editor/AST rewrite, cross-backend content interop.

## Migration approach (incremental, always-green)

The refactor proceeds so the tree compiles and tests pass after each step:

1. **Extract `mime822.rs`** — move the pure functions + their 14 unit tests out of
   `gmail.rs`; `gmail.rs` re-exports them so existing callers keep working. Run
   `cargo test` — must be green before proceeding.
2. **Define `backend/mod.rs` traits** — types + 5 traits + `Capabilities`. No callers
   yet; pure addition.
3. **Implement the Gmail vertical** — `backend/gmail/{mod,transport,identity}.rs`
   implementing the traits by delegating to existing `gmail::*` functions (thin
   wrappers first; `TransportError` classification helper added here).
4. **Route the 70 call sites** in `lib.rs` through the vertical's trait methods,
   in families (note CRUD → folder ops → sidecar drain), compiling between families.
5. **Remove the `gmail.rs` shim** once no caller references `gmail::*` directly.

Each step is independently reviewable and revertible.

## Acceptance & verification

- `cargo test` — all existing Rust tests green (the 14 `mime822` tests are the
  extraction's safety net; the `lib.rs`/`db.rs` tests guard the wiring).
- `cargo build` succeeds; Tauri bundle layout check passes (exactly one binary in
  `Contents/MacOS/` — see CLAUDE.md active edge #5).
- Apple round-trip spot-check on the `Notes/play5` test subtree: create/edit a note,
  confirm title strip/inject, attachments, tags (`#x`), and a `[[link]]` survive a
  save → pull cycle unchanged.
- All work on a feature branch (not `main`).

## Open questions (non-blocking)

- Exact `AppState` shape for the concrete vertical (new field vs. façade over
  existing per-account state) — settled in the plan; no dyn either way.
- Whether `Transport::fetch` returns Gmail's parsed JSON payload or raw bytes as
  `RawNote` — pick the lower-churn option during implementation; `mime822::decode`
  consumes whichever.
- Precise `RawNote` / `DecodedNote` / `NoteForEncode` struct boundaries vs. the
  existing `Note` / `SavedNote` types — minimize new types in the Pragmatic pass.
</content>
</invoke>
