# Vertical #0 Extraction (Apple-via-Gmail) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the email-backend abstraction out of `gmail.rs`, reframing the app as "Vertical #0 (Apple-via-Gmail)" behind a backend-agnostic trait surface, with **no user-visible behavior change**.

**Architecture:** Pull the genuinely format-neutral MIME/Apple helpers into a reusable `mime822` module. Define five traits (`Transport`, `AtRest`, `Identity`, `Deriver`, `MetadataSidecar`) + a `Vertical` composition trait in a new `backend` module. Implement them for a concrete `GmailVertical` that wraps the existing `gmail::*` free functions. Route the ~70 `gmail::*` call sites in `lib.rs` through the vertical via **static dispatch** (no `Box<dyn>`). Keep the intricate `list_notes` dedup/sort/cache-reuse orchestration untouched (Pragmatic scope).

**Tech Stack:** Rust, Tauri 2, `async_trait`, `reqwest`, `serde`, `anyhow` (new dep for `TransportError::source`), existing `cargo test` suite as the regression net.

**Source spec:** [docs/superpowers/specs/2026-06-16-vertical-0-gmail-extraction-design.md](../specs/2026-06-16-vertical-0-gmail-extraction-design.md)

**Acceptance bar:** `cargo test` green (esp. the 14 MIME tests), `cargo build` succeeds, Tauri bundle has one binary in `Contents/MacOS/`, Apple round-trip intact on the `Notes/play5` test subtree. Feature branch only.

---

## File Structure

| File | Responsibility |
|---|---|
| `src-tauri/src/mime822.rs` (create) | Format-neutral MIME/Apple **string & byte** helpers + the RFC822 message **builder** + their unit tests. Reusable by future IMAP/JMAP/Graph verticals. |
| `src-tauri/src/backend/mod.rs` (create) | The five traits + `Vertical` + `Capabilities` + neutral value/error types (`SyncCursor`, `ChangeSet`, `SaveOp`, `SaveOutcome`, `TransportError`, …). The locked surface. |
| `src-tauri/src/backend/gmail/mod.rs` (create) | `GmailVertical`: composes the parts, declares `Capabilities`, owns the `Note`/`Attachment`/`DedupSummary`/`SidecarRef`/`TagSidecarRef`/`FolderInfo` types and the `list_notes`/`list_notes_in_label`/`list_account_index`/`list_trashed_notes` orchestration (dedup/sort untouched). |
| `src-tauri/src/backend/gmail/transport.rs` (create) | Gmail HTTP calls → `Transport` impl + folder/label ops + `MetadataSidecar` impl + `TransportError` classifier. The Gmail-JSON-coupled decode (`Part`/`Body` walkers) lives here. |
| `src-tauri/src/backend/gmail/identity.rs` (create) | `Identity` impl: `mint` + `rekey_for_conflict_copy`. |
| `src-tauri/src/gmail.rs` (modify → shrink) | Thin re-export shim during migration; removed (or reduced to nothing) in Phase 5. |
| `src-tauri/src/lib.rs` (modify) | `mod` declarations; call sites routed through `GmailVertical`. |
| `src-tauri/Cargo.toml` (modify) | Add `anyhow` and `async-trait` deps (if not already present). |

**Phasing:** Each phase leaves the tree compiling and `cargo test` green, and is independently committable/revertible.

- **Phase 0** — branch + dependency prep.
- **Phase 1** — extract `mime822` (re-export shim keeps all callers working). *Highest value, lowest risk — could ship alone.*
- **Phase 2** — define the trait surface (`backend/mod.rs`). Pure addition, no callers.
- **Phase 3** — implement `GmailVertical` wrapping existing functions.
- **Phase 4** — route the ~70 call sites through the vertical, family by family.
- **Phase 5** — remove the `gmail.rs` shim (optional cleanup).

---

## Phase 0 — Branch & dependency prep

### Task 0.1: Confirm branch and baseline-green tests

**Files:** none (verification only)

- [ ] **Step 1: Confirm on the feature branch**

Run: `git branch --show-current`
Expected: `refactor/vertical-0-gmail-extraction` (created during brainstorming). If not, run `git checkout -b refactor/vertical-0-gmail-extraction`.

- [ ] **Step 2: Establish the green baseline**

Run: `cd src-tauri && cargo test 2>&1 | tail -30`
Expected: all tests pass (the 14 MIME tests in `gmail.rs` + `db.rs`/`lib.rs` tests). Record the pass count — every later phase must match or exceed it.

- [ ] **Step 3: Confirm the call-site inventory matches the plan**

Run: `cd src-tauri && grep -c "gmail::" src/lib.rs`
Expected: `70` (function + type references). If the number differs, the codebase moved since planning — re-scan before proceeding.

### Task 0.2: Add dependencies

**Files:**
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Check what's already present**

Run: `cd src-tauri && grep -E "anyhow|async-trait|async_trait" Cargo.toml`
Expected: note which are missing.

- [ ] **Step 2: Add the missing deps**

In `src-tauri/Cargo.toml` under `[dependencies]`, add any not already present:

```toml
anyhow = "1"
async-trait = "0.1"
```

(`async-trait` is needed for `async fn` in the `Transport`/`MetadataSidecar` traits; `anyhow::Error` is the `source` payload in `TransportError::{Transient,Permanent}`.)

- [ ] **Step 3: Verify it resolves**

Run: `cd src-tauri && cargo build 2>&1 | tail -15`
Expected: builds (deps fetched).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "build: add anyhow + async-trait for backend trait surface"
```

---

## Phase 1 — Extract `mime822`

The neutral helpers move to `mime822.rs`; `gmail.rs` re-exports them so **no call site changes** in this phase. The 14 existing tests split: the format-neutral ones move with the functions; the Gmail-`Part`-coupled ones stay in `gmail.rs`.

### Task 1.1: Create `mime822.rs` with the neutral helpers

**Files:**
- Create: `src-tauri/src/mime822.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod mime822;`)
- Modify: `src-tauri/src/gmail.rs` (delete moved fns, add `use crate::mime822::*;` re-export)

- [ ] **Step 1: Declare the module**

In `src-tauri/src/lib.rs`, add near the other `mod` declarations (e.g. after `mod gmail;`):

```rust
pub mod mime822;
```

- [ ] **Step 2: Move the format-neutral functions into `mime822.rs`**

Create `src-tauri/src/mime822.rs`. **Move** (cut from `gmail.rs`, paste here verbatim) these items — they have **no** dependency on the Gmail `Header`/`Part`/`Body` structs:

- Constants: `APPLE_MIME_VERSION`, `INLINE_TITLE_TAGS`
- `try_recover_mis_decoded_utf8`
- `is_ascii`
- `format_apple_uuid` (already `pub`)
- `canonicalize_uuid` (already `pub`)
- `format_apple_date`
- `rfc2047_encode_subject`
- `qp_encode_body`
- `strip_html_tags`
- `first_block_or_embed`
- `first_line_split`
- `inject_title_into_body`
- `strip_leading_title`
- `strip_leading_title_once`
- `decode_body`
- `decode_b64_bytes`
- `referenced_cids`
- `base64_mime_wrap`
- `data_uri` (already `pub`)

Add the file header:

```rust
//! Format-neutral MIME / Apple-Notes helpers shared by every email-family
//! backend (Gmail today; IMAP / JMAP / Graph later). These operate on strings
//! and bytes only — nothing here knows about Gmail's REST JSON shape. The
//! Gmail-JSON-coupled MIME-tree walkers (collect_pending_attachments,
//! find_html_in_parts) stay in the Gmail vertical because Gmail pre-parses
//! MIME into JSON; a raw-RFC822 backend would parse bytes instead but reuse
//! every helper below.

use base64::{Engine as _, engine::general_purpose::URL_SAFE};
```

Make every moved fn `pub` (they need to be callable from `gmail.rs` and the future vertical). Keep their bodies byte-for-byte identical.

- [ ] **Step 3: Re-export from `gmail.rs` so existing callers keep working**

At the top of `src-tauri/src/gmail.rs`, after the existing `use` lines, add:

```rust
// mime822 extraction (Phase 1): these helpers moved to the shared module.
// Re-exported here so existing intra-module callers and lib.rs keep compiling
// unchanged. Removed when call sites migrate to backend::mime822 directly.
pub use crate::mime822::{
    base64_mime_wrap, canonicalize_uuid, data_uri, decode_b64_bytes, decode_body,
    first_block_or_embed, first_line_split, format_apple_date, format_apple_uuid,
    inject_title_into_body, is_ascii, qp_encode_body, referenced_cids,
    rfc2047_encode_subject, strip_html_tags, strip_leading_title,
    strip_leading_title_once, try_recover_mis_decoded_utf8, APPLE_MIME_VERSION,
    INLINE_TITLE_TAGS,
};
```

Remove the now-duplicate `const APPLE_MIME_VERSION` / `const INLINE_TITLE_TAGS` and the moved fn bodies from `gmail.rs`. Leave `get_header`, `header_param`, `find_html_in_parts`, `collect_pending_attachments` in `gmail.rs` (they depend on `Header`/`Part`).

- [ ] **Step 4: Verify it compiles**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: builds. Fix any missed `use base64::Engine` imports in `mime822.rs` (some moved fns call `.encode()` / `.decode()` which need `Engine as _` in scope — already in the header).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/mime822.rs src-tauri/src/gmail.rs src-tauri/src/lib.rs
git commit -m "refactor(mime822): extract format-neutral MIME helpers from gmail.rs"
```

### Task 1.2: Move the neutral tests to `mime822.rs`

**Files:**
- Modify: `src-tauri/src/mime822.rs` (add tests)
- Modify: `src-tauri/src/gmail.rs` (remove moved tests)

- [ ] **Step 1: Move the title tests**

Move the entire `mod title_tests` block (5 tests: `strips_partly_styled_title`, `strips_bare_text_title`, `strips_div_wrapped_title`, `strips_span_wrapped_title`, `does_not_strip_non_title_first_line`, `inject_is_idempotent_on_styled_title`, `inject_adds_title_when_absent`, `strip_then_inject_roundtrips_styled_title`, `strips_title_keeps_trailing_image_object`) from `gmail.rs` into `mime822.rs`. Update the `use super::...` line to import from `mime822`'s `super` (the functions are now siblings):

```rust
#[cfg(test)]
mod title_tests {
    use super::{inject_title_into_body, strip_leading_title};
    // ... rest verbatim
}
```

- [ ] **Step 2: Move the two neutral attachment tests**

From `mod attachment_tests` in `gmail.rs`, move ONLY these two tests (they depend only on neutral helpers) into a `mod tests` in `mime822.rs`:
- `referenced_cids_extracts_and_dedupes`
- `base64_wrap_at_76_cols`

```rust
#[cfg(test)]
mod mime_byte_tests {
    use super::{base64_mime_wrap, referenced_cids};

    #[test]
    fn referenced_cids_extracts_and_dedupes() {
        // ... verbatim from gmail.rs
    }

    #[test]
    fn base64_wrap_at_76_cols() {
        // ... verbatim from gmail.rs
    }
}
```

Leave `header_param_extracts_quoted_and_unquoted`, `collects_inline_image_skips_html`, `collects_non_image_attachments` in `gmail.rs`'s `attachment_tests` (they use `Header`/`Part`/`collect_pending_attachments`/`header_param`).

- [ ] **Step 3: Run the full suite**

Run: `cd src-tauri && cargo test 2>&1 | tail -30`
Expected: same pass count as the Task 0.1 baseline. The 7 moved tests now run under `mime822`; the 3 remaining run under `gmail`.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/mime822.rs src-tauri/src/gmail.rs
git commit -m "refactor(mime822): relocate neutral MIME tests with their functions"
```

### Task 1.3: Extract the RFC822 message builder (AtRest encode side)

The `save_note` function builds the raw RFC822 string inline. Extract that string-building into a pure, testable `mime822::build_note_mime` so the future `AtRest::encode` and any IMAP/JMAP backend can reuse it. The HTTP POST + label resolution stay in `gmail.rs`.

**Files:**
- Modify: `src-tauri/src/mime822.rs` (add `build_note_mime` + a test)
- Modify: `src-tauri/src/gmail.rs` (`save_note` calls it)

- [ ] **Step 1: Add `MimeAttachment` + `build_note_mime` to `mime822.rs`**

`build_note_mime` must NOT depend on the Gmail `Attachment` struct (that lives in the vertical). Define a minimal neutral input struct and have `save_note` adapt:

```rust
/// Neutral attachment input for the RFC822 builder. The Gmail vertical's
/// `Attachment` is adapted into this at the call boundary.
pub struct MimeAttachment<'a> {
    pub content_id: &'a str,
    pub mime_type: &'a str,
    pub filename: Option<&'a str>,
    pub x_apple_part_url: Option<&'a str>,
    pub data: &'a [u8],
}

/// Build the raw RFC822 message bytes for an Apple note, matching Apple Notes'
/// on-wire shape (content-adaptive us-ascii/7bit vs utf-8/QP; single text/html
/// or multipart/related when attachments are referenced). Returns the raw
/// string; the caller base64url-encodes and POSTs it.
///
/// `body_html` here is the EDITOR-VIEW body (title NOT yet injected) — this fn
/// injects the title as the first element (idempotent). `used` is the set of
/// attachments the body actually references (caller pre-filters via
/// referenced_cids).
pub fn build_note_mime(
    title: &str,
    body_html: &str,
    uuid: &str,
    date_header: &str,
    created_date: &str,
    user_email: &str,
    used: &[MimeAttachment<'_>],
) -> String {
    // MOVE here, verbatim, the body of save_note from the line
    //   `let body_with_title = inject_title_into_body(body_html, title);`
    // down to the end of the `let raw = if used.is_empty() { ... } else { ... };`
    // block, returning `raw`. (The uuid/date/created_date/message_id/from
    // computations that depend only on these args also move; the Message-Id's
    // fresh uuid is generated here.)
}
```

Note for the implementer: in `save_note`, the values `uuid`, `date_header`, `created_date` are computed *before* the raw-building block — pass them in as args (they're also needed afterwards for the `SavedNote` return + the cache). The `message_id` and `from` are local to building — compute them inside `build_note_mime`.

- [ ] **Step 2: Rewire `save_note` to call it**

In `gmail.rs` `save_note`, replace the inline raw-building block with:

```rust
let used_mime: Vec<crate::mime822::MimeAttachment> = used.iter().map(|a| {
    crate::mime822::MimeAttachment {
        content_id: &a.content_id,
        mime_type: &a.mime_type,
        filename: a.filename.as_deref(),
        x_apple_part_url: a.x_apple_part_url.as_deref(),
        data: &a.data,
    }
}).collect();
let raw = crate::mime822::build_note_mime(
    title, body_html, &uuid, &date_header, &created_date, user_email, &used_mime,
);
```

Keep everything else in `save_note` (cid filtering, label resolution, POST, delete-old, `SavedNote` return) unchanged.

- [ ] **Step 3: Add a builder smoke test**

In `mime822.rs` `mod mime_byte_tests`, add:

```rust
#[test]
fn build_note_mime_ascii_single_part_has_apple_headers() {
    let raw = super::build_note_mime(
        "Hello", "<html><body><div>Hello</div><div>world</div></body></html>",
        "AAAA-BBBB", "Thu, 4 Jun 2026 01:19:50 +0700",
        "Thu, 4 Jun 2026 01:19:50 +0700", "u@example.com", &[],
    );
    assert!(raw.contains("X-Uniform-Type-Identifier: com.apple.mail-note"));
    assert!(raw.contains("X-Universally-Unique-Identifier: AAAA-BBBB"));
    assert!(raw.contains("charset=us-ascii"));
    assert!(raw.contains("Subject: Hello"));
}

#[test]
fn build_note_mime_multipart_when_attachment_referenced() {
    let body = "<html><body><div>t</div>\
        <object data=\"cid:C1@x\"></object></body></html>";
    let att = super::MimeAttachment {
        content_id: "C1@x", mime_type: "image/png",
        filename: Some("i.png"), x_apple_part_url: None, data: &[1u8, 2, 3],
    };
    let raw = super::build_note_mime(
        "t", body, "U", "D", "C", "u@x.com", &[att],
    );
    assert!(raw.contains("multipart/related"));
    assert!(raw.contains("Content-Id: <C1@x>"));
}
```

- [ ] **Step 4: Run tests**

Run: `cd src-tauri && cargo test 2>&1 | tail -30`
Expected: baseline pass count + 2 new tests green.

- [ ] **Step 5: Apple round-trip safety check (manual, important)**

This task touches the write path. Build the app and verify a save still round-trips:
Run: `cd .. && npm run tauri dev` (or use an existing dev build), create/edit a note in `Notes/play5`, confirm title strips/injects correctly and (if testing attachments) an inline image survives a save→pull. Document the result in the commit message.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/mime822.rs src-tauri/src/gmail.rs
git commit -m "refactor(mime822): extract build_note_mime (AtRest encode side); round-trip verified"
```

---

## Phase 2 — Define the trait surface

Pure addition: `backend/mod.rs` with types + traits. No callers yet, so the only risk is "does it compile."

### Task 2.1: Create `backend/mod.rs` with value & error types

**Files:**
- Create: `src-tauri/src/backend/mod.rs`
- Modify: `src-tauri/src/lib.rs` (add `pub mod backend;`)

- [ ] **Step 1: Declare the module**

In `src-tauri/src/lib.rs`, add: `pub mod backend;`

- [ ] **Step 2: Write the neutral types**

Create `src-tauri/src/backend/mod.rs`:

```rust
//! Backend-agnostic trait surface ("Vertical #0" seam). The shared core
//! (sync worker, conflict policy, cache) talks to a backend only through
//! these traits. Gmail is the first and only implementor today (static
//! dispatch); JMAP/Graph plug in later by implementing the same set.
//!
//! See docs/superpowers/specs/2026-06-16-architecture-principles-design.md
//! for the locked surface and rationale.

use std::time::Duration;

pub mod gmail;

/// Opaque, vertical-owned sync position (Gmail historyId / JMAP state /
/// IMAP UIDVALIDITY+MODSEQ / mtime). The core persists and loops over it,
/// never inspects it. Gmail returns an inert cursor today (full-scan path).
#[derive(Clone, Debug, Default)]
pub struct SyncCursor(pub Vec<u8>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind { Upserted, Deleted }

#[derive(Clone, Debug)]
pub struct RemoteChange {
    pub remote_id: String,
    pub kind: ChangeKind,
    pub folder_hint: Option<String>,
}

#[derive(Debug, Default)]
pub struct ChangeSet {
    pub changes: Vec<RemoteChange>,
    pub next_cursor: SyncCursor,
    pub more: bool,
}

/// Result of a save. `remote_id` MAY DIFFER from any input id — Gmail re-mints
/// the message id on every content edit. The core re-points the cache `id`.
#[derive(Debug)]
pub struct SaveOutcome {
    pub remote_id: String,
    pub cursor_hint: Option<SyncCursor>,
}

/// Classified transport failure. The transport CLASSIFIES (reads HTTP status /
/// Retry-After); the shared worker owns the retry POLICY + backoff/jitter.
#[derive(Debug)]
pub enum TransportError {
    RateLimited { retry_after: Option<Duration> },
    Transient { source: anyhow::Error },
    Conflict { remote_etag: Option<String> },
    Auth,
    NotFound,
    Permanent { source: anyhow::Error },
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::RateLimited { retry_after } =>
                write!(f, "rate limited (retry_after={:?})", retry_after),
            TransportError::Transient { source } => write!(f, "transient: {}", source),
            TransportError::Conflict { remote_etag } =>
                write!(f, "conflict (etag={:?})", remote_etag),
            TransportError::Auth => write!(f, "auth"),
            TransportError::NotFound => write!(f, "not found"),
            TransportError::Permanent { source } => write!(f, "permanent: {}", source),
        }
    }
}
impl std::error::Error for TransportError {}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd src-tauri && cargo build 2>&1 | tail -15`
Expected: builds (with dead-code warnings — fine, no callers yet).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/backend/mod.rs src-tauri/src/lib.rs
git commit -m "feat(backend): add neutral value + error types for trait surface"
```

### Task 2.2: Add the trait definitions

**Files:**
- Modify: `src-tauri/src/backend/mod.rs`

- [ ] **Step 1: Add input/output structs the traits reference**

Append to `backend/mod.rs`:

```rust
/// Params for a save. Owned by the core (built from a cache row); the vertical
/// interprets them. `body_html` is editor-view (title not yet injected).
pub struct SaveOp<'a> {
    pub title: &'a str,
    pub body_html: &'a str,
    pub existing_remote_id: Option<&'a str>,
    pub existing_uuid: Option<&'a str>,
    pub existing_created_date: Option<&'a str>,
    pub label: &'a str,
}

/// A folder as the backend reports it (Gmail label, JMAP mailbox, …).
#[derive(Clone, Debug)]
pub struct RemoteFolder {
    pub id: String,
    pub path: String,
}

/// Which Jodd-local metadata a sidecar carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidecarKind { Pin, Tags }

/// A discovered sidecar. `body` is present only for kinds that need it (Tags);
/// Pin is existence-only and leaves `body` None.
#[derive(Clone, Debug)]
pub struct SidecarRecord {
    pub id: String,
    pub note_uuid: String,
    pub kind: SidecarKind,
    pub body: Option<Vec<u8>>,
}
```

- [ ] **Step 2: Add the five traits + `Vertical` + `Capabilities`**

```rust
use async_trait::async_trait;

#[async_trait]
pub trait Transport: Send + Sync {
    async fn changes_since(&self, cursor: Option<&SyncCursor>) -> Result<ChangeSet, TransportError>;
    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError>;
    async fn delete(&self, remote_id: &str) -> Result<(), TransportError>;
    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError>;
    async fn ensure_folder(&self, path: &str) -> Result<RemoteFolder, TransportError>;
    async fn create_folder(&self, name: &str) -> Result<RemoteFolder, TransportError>;
    async fn rename_folder(&self, id: &str, new_name: &str) -> Result<(), TransportError>;
    async fn delete_folder(&self, id: &str) -> Result<(), TransportError>;
    async fn move_note(&self, remote_id: &str, add: &[String], remove: &[String]) -> Result<(), TransportError>;
}

#[async_trait]
pub trait MetadataSidecar: Send + Sync {
    async fn list_sidecars(&self, kind: SidecarKind) -> Result<Vec<SidecarRecord>, TransportError>;
    /// `body` None = existence-only (Pin); Some = JSON payload (Tags). Trashes
    /// `replace` if supplied (insert-then-trash). Returns the new sidecar id.
    async fn put_sidecar(&self, note_uuid: &str, kind: SidecarKind, body: Option<&[u8]>, replace: Option<&str>) -> Result<String, TransportError>;
    async fn remove_sidecar(&self, id: &str) -> Result<(), TransportError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderModel { SingleExclusive }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fidelity { Full }

#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub folder_model: FolderModel,
    pub fidelity: Fidelity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentKind { AppleHtml }

#[derive(Clone, Debug)]
pub struct Edge { pub rel: String, pub target: String }

#[derive(Default, Debug)]
pub struct Derived {
    pub text: String,
    pub tags: Vec<String>,
    pub edges: Vec<Edge>,
}

/// Synchronous: local CPU/ms work (the data doctrine targets network latency,
/// not local compute). Links/edges/tags are body-resident and re-derived
/// everywhere — they are never carried in a sidecar.
pub trait Deriver: Send + Sync {
    fn derive(&self, kind: ContentKind, blob: &[u8]) -> Derived;
}

pub trait Identity: Send + Sync {
    fn mint(&self) -> String;
}

pub trait Vertical: Send + Sync {
    fn backend_id(&self) -> &str;
    fn capabilities(&self) -> &Capabilities;
}
```

Note: `AtRest` is realized by the `mime822` module (encode) + the Gmail vertical's JSON decode; it is not a separately-dispatched trait object in this Pragmatic pass (a single concrete decode path exists). `Identity::rekey_for_conflict_copy` is deferred to Task 3.4 where the existing conflict logic is wrapped — `mint` is defined now.

- [ ] **Step 3: Verify it compiles**

Run: `cd src-tauri && cargo build 2>&1 | tail -15`
Expected: builds (dead-code warnings expected).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/backend/mod.rs
git commit -m "feat(backend): define Transport/MetadataSidecar/Deriver/Identity/Vertical traits"
```

---

## Phase 3 — Implement `GmailVertical`

Wrap the existing `gmail::*` free functions behind the traits. Move the Gmail-specific types and orchestration into `backend/gmail/`. The existing `gmail.rs` keeps its free functions for now (the vertical delegates to them via `crate::gmail::*`); they move physically in Phase 5.

### Task 3.1: Create the `GmailVertical` struct + error classifier

**Files:**
- Create: `src-tauri/src/backend/gmail/mod.rs`
- Create: `src-tauri/src/backend/gmail/transport.rs`

- [ ] **Step 1: Write the vertical struct**

Create `src-tauri/src/backend/gmail/mod.rs`:

```rust
//! Vertical #0 — Apple-via-Gmail. Composes the Gmail transport + identity +
//! deriver behind the backend trait surface. Constructed per-operation from
//! the already-fetched (token, label_map, user_email); the shared core's
//! ensure_token / cached_label_map run first and unchanged.

use super::{Capabilities, Fidelity, FolderModel, Vertical};
use std::collections::HashMap;

pub mod transport;
pub mod identity;

pub struct GmailVertical {
    pub(crate) token: String,
    pub(crate) label_map: HashMap<String, String>,
    pub(crate) user_email: String,
    capabilities: Capabilities,
}

impl GmailVertical {
    pub fn new(token: String, label_map: HashMap<String, String>, user_email: String) -> Self {
        Self {
            token, label_map, user_email,
            capabilities: Capabilities {
                folder_model: FolderModel::SingleExclusive,
                fidelity: Fidelity::Full,
            },
        }
    }
}

impl Vertical for GmailVertical {
    fn backend_id(&self) -> &str { "apple-via-gmail" }
    fn capabilities(&self) -> &Capabilities { &self.capabilities }
}
```

- [ ] **Step 2: Write the error classifier**

Create `src-tauri/src/backend/gmail/transport.rs`:

```rust
//! Gmail Transport + MetadataSidecar impls — thin wrappers over the existing
//! crate::gmail free functions, plus HTTP-status classification into
//! TransportError. The existing functions return Result<_, String>; this layer
//! maps the string/status to the classified enum so the worker can branch.

use crate::backend::{TransportError};
use std::time::Duration;

/// Map an HTTP status (and optional Retry-After seconds) to a TransportError.
/// Used by the trait wrappers that talk to Gmail directly. For wrappers that
/// delegate to existing String-returning fns, `classify_str` inspects the
/// error text (the existing fns embed "HTTP {status}" in their messages).
pub(crate) fn classify_status(status: u16, retry_after: Option<u64>, body: &str) -> TransportError {
    match status {
        429 => TransportError::RateLimited { retry_after: retry_after.map(Duration::from_secs) },
        401 | 403 => TransportError::Auth,
        404 => TransportError::NotFound,
        409 => TransportError::Conflict { remote_etag: None },
        500..=599 => TransportError::Transient { source: anyhow::anyhow!("HTTP {}: {}", status, body) },
        _ => TransportError::Permanent { source: anyhow::anyhow!("HTTP {}: {}", status, body) },
    }
}

/// Classify an error string produced by the existing crate::gmail functions,
/// which embed " HTTP {status}" / " {status}" markers. Falls back to the
/// is_unauthorized heuristic already used in lib.rs.
pub(crate) fn classify_str(err: &str) -> TransportError {
    let has = |code: &str| err.contains(code);
    if has(" 401") || has("UNAUTHENTICATED") || has("Invalid Credentials") || has(" 403") {
        TransportError::Auth
    } else if has(" 429") {
        TransportError::RateLimited { retry_after: None }
    } else if has(" 404") {
        TransportError::NotFound
    } else if has(" 409") {
        TransportError::Conflict { remote_etag: None }
    } else if has(" 500") || has(" 502") || has(" 503") || has(" 504") {
        TransportError::Transient { source: anyhow::anyhow!("{}", err) }
    } else {
        TransportError::Permanent { source: anyhow::anyhow!("{}", err) }
    }
}
```

- [ ] **Step 3: Wire the submodules**

`backend/gmail/mod.rs` already declares `pub mod transport; pub mod identity;`. Create a placeholder `backend/gmail/identity.rs`:

```rust
use crate::backend::Identity;
use super::GmailVertical;

impl Identity for GmailVertical {
    fn mint(&self) -> String {
        crate::mime822::format_apple_uuid(uuid::Uuid::new_v4())
    }
}
```

- [ ] **Step 4: Add a classifier unit test**

In `backend/gmail/transport.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TransportError;

    #[test]
    fn classify_str_maps_known_codes() {
        assert!(matches!(classify_str("Save failed 401: x"), TransportError::Auth));
        assert!(matches!(classify_str("HTTP 404 (id=z)"), TransportError::NotFound));
        assert!(matches!(classify_str("foo 503 bar"), TransportError::Transient { .. }));
        assert!(matches!(classify_str("weird 418"), TransportError::Permanent { .. }));
    }
}
```

- [ ] **Step 5: Build + test**

Run: `cd src-tauri && cargo test 2>&1 | tail -20`
Expected: baseline + 1 new test green.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/backend/gmail/
git commit -m "feat(backend/gmail): GmailVertical struct + identity + error classifier"
```

### Task 3.2: Implement `Transport` for `GmailVertical`

**Files:**
- Modify: `src-tauri/src/backend/gmail/transport.rs`

- [ ] **Step 1: Implement the trait by delegating to existing functions**

Append to `transport.rs`:

```rust
use crate::backend::{
    ChangeKind, ChangeSet, RemoteChange, RemoteFolder, SaveOp, SaveOutcome,
    SyncCursor, Transport,
};
use super::GmailVertical;
use async_trait::async_trait;

#[async_trait]
impl Transport for GmailVertical {
    async fn changes_since(&self, _cursor: Option<&SyncCursor>) -> Result<ChangeSet, TransportError> {
        // Gmail full-scans today (no cursor wired). Return every Notes message
        // id as an Upsert with an inert cursor. The worker is NOT yet driven by
        // this method (Pragmatic scope) — it exists so the surface is real and
        // JMAP can implement a real cursor without changing the trait.
        let idx = crate::gmail::list_account_index(&self.token, &self.user_email, &self.label_map)
            .await
            .map_err(|e| classify_str(&e))?;
        let changes = idx.into_iter().map(|m| RemoteChange {
            remote_id: m.id,
            kind: ChangeKind::Upserted,
            folder_hint: Some(m.label),
        }).collect();
        Ok(ChangeSet { changes, next_cursor: SyncCursor::default(), more: false })
    }

    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError> {
        // NOTE: the existing save_note also loads attachments + updates cache at
        // the call site. This wrapper covers the network save only; callers that
        // need attachment re-emit continue to use the richer path until Phase 4
        // migrates them. See save_with_attachments below.
        unimplemented!("use save_with_attachments — see Task 3.2 step 2")
    }

    async fn delete(&self, remote_id: &str) -> Result<(), TransportError> {
        crate::gmail::delete_note(&self.token, &self.user_email, remote_id)
            .await.map_err(|e| classify_str(&e))
    }

    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError> {
        let map = crate::gmail::get_label_map(&self.token).await.map_err(|e| classify_str(&e))?;
        Ok(map.into_iter()
            .filter(|(_, n)| n == "Notes" || n.starts_with("Notes/"))
            .map(|(id, path)| RemoteFolder { id, path })
            .collect())
    }

    async fn ensure_folder(&self, path: &str) -> Result<RemoteFolder, TransportError> {
        let id = crate::gmail::ensure_label(&self.token, path, &self.label_map)
            .await.map_err(|e| classify_str(&e))?;
        Ok(RemoteFolder { id, path: path.to_string() })
    }

    async fn create_folder(&self, name: &str) -> Result<RemoteFolder, TransportError> {
        let info = crate::gmail::create_label(&self.token, name).await.map_err(|e| classify_str(&e))?;
        Ok(RemoteFolder { id: info.id, path: info.name })
    }

    async fn rename_folder(&self, id: &str, new_name: &str) -> Result<(), TransportError> {
        crate::gmail::rename_label(&self.token, id, new_name).await.map_err(|e| classify_str(&e))
    }

    async fn delete_folder(&self, id: &str) -> Result<(), TransportError> {
        crate::gmail::delete_label(&self.token, id).await.map_err(|e| classify_str(&e))
    }

    async fn move_note(&self, remote_id: &str, add: &[String], remove: &[String]) -> Result<(), TransportError> {
        crate::gmail::modify_message_labels(&self.token, remote_id, add, remove)
            .await.map_err(|e| classify_str(&e))
    }
}
```

- [ ] **Step 2: Add the attachment-aware save as an inherent method**

`save_note` needs attachments + label_map + returns a richer `SavedNote` (uuid, date, body for the cache). Rather than force that through the lean `SaveOp`, expose it as an inherent method on `GmailVertical` that the worker calls (the lean `Transport::save` stays for the generic loop / future backends):

```rust
impl GmailVertical {
    /// Attachment-aware save used by the sync worker's push path. Wraps the
    /// existing save_note (insert-then-trash, multipart/related re-emit) and
    /// returns the full SavedNote the cache needs.
    pub async fn save_note_full(
        &self,
        op: &SaveOp<'_>,
        attachments: &[crate::gmail::Attachment],
    ) -> Result<crate::gmail::SavedNote, TransportError> {
        crate::gmail::save_note(
            &self.token, op.title, op.body_html,
            op.existing_remote_id, op.existing_uuid, op.existing_created_date,
            op.label, &self.user_email, &self.label_map, attachments,
        ).await.map_err(|e| classify_str(&e))
    }
}
```

Then make `Transport::save` delegate with no attachments (so the trait is fully implemented, not `unimplemented!`):

```rust
    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError> {
        let saved = self.save_note_full(&op, &[]).await?;
        Ok(SaveOutcome { remote_id: saved.id, cursor_hint: None })
    }
```

- [ ] **Step 3: Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -10`
Expected: builds, baseline tests green. (Dead-code warnings for not-yet-called methods are fine.)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/backend/gmail/transport.rs
git commit -m "feat(backend/gmail): implement Transport (delegating to gmail fns)"
```

### Task 3.3: Implement `MetadataSidecar` for `GmailVertical`

**Files:**
- Modify: `src-tauri/src/backend/gmail/transport.rs`

- [ ] **Step 1: Implement the trait**

The existing pin path uses `list_meta_sidecars` (metadata-only) and the tag path `list_tag_sidecars` (full body). Map both onto `list_sidecars(kind)`:

```rust
use crate::backend::{MetadataSidecar, SidecarKind, SidecarRecord};

#[async_trait]
impl MetadataSidecar for GmailVertical {
    async fn list_sidecars(&self, kind: SidecarKind) -> Result<Vec<SidecarRecord>, TransportError> {
        // meta_label_id is resolved by the caller today; the worker passes the
        // resolved id via the inherent helpers below. The trait method resolves
        // the configured meta label from label_map by convention ("Notes-Meta"
        // default is handled at the lib.rs layer); here we require the caller to
        // have ensured it. For Pragmatic scope, list by walking label_map for a
        // "Notes-Meta"-like label is avoided — the worker uses list_sidecars_in.
        unimplemented!("worker uses list_sidecars_in(meta_label_id, kind)")
    }
    async fn put_sidecar(&self, note_uuid: &str, kind: SidecarKind, body: Option<&[u8]>, replace: Option<&str>) -> Result<String, TransportError> {
        unimplemented!("worker uses put_sidecar_in(meta_label_id, ...)")
    }
    async fn remove_sidecar(&self, id: &str) -> Result<(), TransportError> {
        crate::gmail::trash_meta_sidecar(&self.token, id).await.map_err(|e| classify_str(&e))
    }
}
```

NOTE to implementer: the pin/tag push paths already resolve `meta_label_id` themselves (via `ensure_label`) before calling the sidecar fns, and that resolution mutates the label cache (`invalidate_label_cache`). To keep that behavior identical, expose **inherent** methods that take the resolved `meta_label_id`, and have the trait methods that don't carry it stay `unimplemented!` (they have no core caller in this pass — only `remove_sidecar` does). This mirrors the `save_note_full` pattern.

- [ ] **Step 2: Add the inherent sidecar helpers the worker will call**

```rust
impl GmailVertical {
    pub async fn list_pin_sidecars_in(&self, meta_label_id: &str) -> Result<Vec<crate::gmail::SidecarRef>, TransportError> {
        crate::gmail::list_meta_sidecars(&self.token, meta_label_id).await.map_err(|e| classify_str(&e))
    }
    pub async fn list_tag_sidecars_in(&self, meta_label_id: &str) -> Result<Vec<crate::gmail::TagSidecarRef>, TransportError> {
        crate::gmail::list_tag_sidecars(&self.token, meta_label_id).await.map_err(|e| classify_str(&e))
    }
    pub async fn put_pin_sidecar_in(&self, note_uuid: &str, payload_json: &str, meta_label_id: &str, replace: Option<&str>) -> Result<String, TransportError> {
        crate::gmail::save_meta_sidecar(&self.token, note_uuid, payload_json, meta_label_id, replace, &self.user_email)
            .await.map_err(|e| classify_str(&e))
    }
    pub async fn put_tag_sidecar_in(&self, note_uuid: &str, payload_json: &str, meta_label_id: &str, replace: Option<&str>) -> Result<String, TransportError> {
        crate::gmail::save_tag_sidecar(&self.token, note_uuid, payload_json, meta_label_id, replace, &self.user_email)
            .await.map_err(|e| classify_str(&e))
    }
}
```

- [ ] **Step 3: Build + test**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -10`
Expected: builds, tests green.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/backend/gmail/transport.rs
git commit -m "feat(backend/gmail): implement MetadataSidecar + inherent sidecar helpers"
```

### Task 3.4: Implement `Deriver` (formalize existing derivation)

The derivation (FTS text / `note_tags` / `edges`) currently runs inside `db.rs` on every write. Phase scope: **define the seam without moving the logic**. The `Deriver` impl exposes the body→`Derived` mapping for callers that want the neutral view; the existing in-`db.rs` derivation stays the authoritative write-time path (behavior identical). This formalizes the bridge and gives JMAP a place to plug a different content parser.

**Files:**
- Create: `src-tauri/src/backend/gmail/deriver.rs`
- Modify: `src-tauri/src/backend/gmail/mod.rs` (add `pub mod deriver;`)

- [ ] **Step 1: Locate the existing body-parsing helpers**

Run: `cd src-tauri && grep -n "fn .*tag\|fn .*hashtag\|fn .*edge\|fn .*wikilink\|fn .*derive\|fn strip" src/db.rs | head -30`
Expected: find the inline `#hashtag` parser, the `[[slug]]` parser, and the HTML-strip-for-FTS helper. Note their names/signatures.

- [ ] **Step 2: Implement `Deriver` delegating to those helpers**

Create `backend/gmail/deriver.rs`. Call the existing `db.rs` helpers (make them `pub(crate)` if needed) — do **not** reimplement parsing:

```rust
use crate::backend::{ContentKind, Derived, Deriver, Edge};
use super::GmailVertical;

impl Deriver for GmailVertical {
    fn derive(&self, _kind: ContentKind, blob: &[u8]) -> Derived {
        let body = std::str::from_utf8(blob).unwrap_or("");
        // Delegate to the SAME helpers db.rs uses on the write path so the
        // neutral view matches the indexed view exactly. Replace the helper
        // names below with the actual ones found in Step 1.
        let text = crate::db::strip_html_for_fts(body);
        let tags = crate::db::parse_body_hashtags(body);
        let edges = crate::db::parse_body_wikilinks(body)
            .into_iter()
            .map(|target| Edge { rel: "mentions".into(), target })
            .collect();
        Derived { text, tags, edges }
    }
}
```

If the existing helpers are private and entangled with SQL writes, leave a `// TODO(JMAP): extract pure parser` note and implement `derive` by calling the smallest pure sub-parsers available — but **do not change** the write-path derivation. The goal is the seam, not a rewrite.

- [ ] **Step 3: Declare module + build**

Add `pub mod deriver;` to `backend/gmail/mod.rs`.
Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: builds. Resolve helper-name/visibility errors using the real names from Step 1.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/backend/gmail/deriver.rs src-tauri/src/backend/gmail/mod.rs src-tauri/src/db.rs
git commit -m "feat(backend/gmail): formalize Deriver seam over existing derivation"
```

---

## Phase 4 — Route the call sites through the vertical

Replace `gmail::*` *function* calls in `lib.rs` with `GmailVertical` trait/inherent-method calls, family by family. The vertical is constructed from the already-fetched `(token, label_map, user_email)`. `gmail::*` *type* references (`gmail::Note`, etc.) stay until Phase 5.

Add a small constructor helper near the top of `lib.rs` to avoid repetition:

```rust
fn gmail_vertical(token: &str, label_map: &std::collections::HashMap<String, String>, account_id: &str) -> backend::gmail::GmailVertical {
    // user_email == account_id in this codebase (account identity IS the email).
    backend::gmail::GmailVertical::new(token.to_string(), label_map.clone(), account_id.to_string())
}
```

> **Per-task discipline:** after each family, run `cargo build` then `cargo test`, expect green, then commit. If a family's behavior is even slightly ambiguous, prefer leaving the `gmail::` call in place and noting it — partial migration is safe because the shim still exists.

### Task 4.1: Push family — `save_note` / `delete_note`

**Files:**
- Modify: `src-tauri/src/lib.rs` (`push_one_dirty`, `push_one_deletion`)

- [ ] **Step 1: Migrate `push_one_dirty`**

Replace the `gmail::save_note(...)` call with:

```rust
let v = gmail_vertical(&token, &label_map, &n.account_id);
let op = backend::SaveOp {
    title: &n.title, body_html: &n.body_html,
    existing_remote_id: existing_gmail_id, existing_uuid,
    existing_created_date: existing_x_mail, label: &n.label,
};
let saved = v.save_note_full(&op, &attachments).await.map_err(|e| e.to_string())?;
```

Keep the surrounding attachment-load and `mark_pushed` lines unchanged.

- [ ] **Step 2: Migrate `push_one_deletion`**

Replace `gmail::delete_note(&token, &n.account_id, &n.id).await?;` with:

```rust
let v = gmail_vertical(&token, &label_map_for_delete, &n.account_id);
v.transport().delete(&n.id).await.map_err(|e| e.to_string())?;
```

`push_one_deletion` doesn't currently fetch `label_map` — `delete`/`remove_sidecar` don't need it, so construct the vertical with an empty map: `gmail_vertical(&token, &std::collections::HashMap::new(), &n.account_id)`. For the two sidecar trashes, replace `gmail::trash_meta_sidecar(&token, x)` with `v.remove_sidecar(x).await` (uses `MetadataSidecar::remove_sidecar`, which ignores label_map). Import the trait: `use backend::MetadataSidecar;`.

- [ ] **Step 3: Build + test + commit**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -10`
Expected: green.
```bash
git add src-tauri/src/lib.rs
git commit -m "refactor(lib): route push/delete through GmailVertical"
```

### Task 4.2: Sidecar push family — `push_one_pin` / `push_one_tag_set`

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Migrate `push_one_pin`**

Keep `meta_label` resolution, `ensure_label`, and `invalidate_label_cache` exactly as-is (behavior-sensitive). Replace the two `gmail::` sidecar calls:

```rust
let v = gmail_vertical(&token, &label_map, &n.account_id);
// pinned branch:
let id = v.put_pin_sidecar_in(&n.uuid, &payload, &meta_label_id, n.meta_msg_id.as_deref())
    .await.map_err(|e| e.to_string())?;
// unpinned branch:
if let Err(e) = v.remove_sidecar(old).await { log!("push_one_pin: trash sidecar {} failed: {}", old, e); }
```

Replace the `gmail::ensure_label(...)` call with `v.transport().ensure_folder(&meta_label).await.map_err(|e| e.to_string())?.id` (or keep `gmail::ensure_label` — both acceptable; prefer the trait for consistency). Import `use backend::{Transport, MetadataSidecar};`.

- [ ] **Step 2: Migrate `push_one_tag_set`** identically, using `v.put_tag_sidecar_in(...)`.

- [ ] **Step 3: Build + test + commit**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -10`
```bash
git add src-tauri/src/lib.rs
git commit -m "refactor(lib): route pin/tag sidecar push through GmailVertical"
```

### Task 4.3: Folder family — `push_one_folder` + folder Tauri commands

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Migrate folder mutations**

Find each `gmail::{create_label, rename_label, delete_label, modify_message_labels}` call (in `push_one_folder` and the folder Tauri commands). Replace with the corresponding `Transport` method via a constructed vertical:
- `gmail::create_label(&token, name)` → `v.transport().create_folder(name).await` (returns `RemoteFolder`; adapt `.id`/`.path` where the old `FolderInfo.id`/`.name` were used)
- `gmail::rename_label(&token, id, new)` → `v.transport().rename_folder(id, new).await`
- `gmail::delete_label(&token, id)` → `v.transport().delete_folder(id).await`
- `gmail::modify_message_labels(&token, id, add, rem)` → `v.transport().move_note(id, add, rem).await`

Map `TransportError` → `String` with `.map_err(|e| e.to_string())` at each site.

- [ ] **Step 2: Build + test + commit**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -10`
```bash
git add src-tauri/src/lib.rs
git commit -m "refactor(lib): route folder mutations through GmailVertical transport"
```

### Task 4.4: Read/orchestration family — `list_notes` / `list_notes_in_label` / `list_account_index` / `list_trashed_notes` / sidecar reads / `find_gmail_ids_for_uuid`

These keep their dedup/sort orchestration. Expose them as inherent methods on `GmailVertical` that delegate to the existing `gmail::*` functions, then route the `lib.rs` callers.

**Files:**
- Modify: `src-tauri/src/backend/gmail/mod.rs` (inherent orchestration methods)
- Modify: `src-tauri/src/lib.rs` (callers)

- [ ] **Step 1: Add orchestration methods to `GmailVertical`**

In `backend/gmail/mod.rs`:

```rust
use std::collections::HashMap;
use crate::backend::TransportError;
use crate::backend::gmail::transport::classify_str;

impl GmailVertical {
    pub async fn list_notes(&self, cache_by_id: &HashMap<String, crate::gmail::Note>)
        -> Result<(Vec<crate::gmail::Note>, crate::gmail::DedupSummary), TransportError> {
        crate::gmail::list_notes(&self.token, &self.label_map, cache_by_id)
            .await.map_err(|e| classify_str(&e))
    }
    pub async fn list_notes_in_label(&self, label_id: &str, cache_by_id: &HashMap<String, crate::gmail::Note>)
        -> Result<Vec<crate::gmail::Note>, TransportError> {
        crate::gmail::list_notes_in_label(&self.token, &self.user_email, label_id, &self.label_map, cache_by_id)
            .await.map_err(|e| classify_str(&e))
    }
    pub async fn list_account_index(&self)
        -> Result<Vec<crate::gmail::MessageIndex>, TransportError> {
        crate::gmail::list_account_index(&self.token, &self.user_email, &self.label_map)
            .await.map_err(|e| classify_str(&e))
    }
    pub async fn list_trashed_notes(&self)
        -> Result<Vec<crate::gmail::TrashedNote>, TransportError> {
        crate::gmail::list_trashed_notes(&self.token, &self.label_map)
            .await.map_err(|e| classify_str(&e))
    }
    pub async fn untrash_note(&self, id: &str) -> Result<(), TransportError> {
        crate::gmail::untrash_note(&self.token, id).await.map_err(|e| classify_str(&e))
    }
    pub async fn fetch_note(&self, id: &str) -> Result<crate::gmail::Note, TransportError> {
        crate::gmail::fetch_note(&self.token, id, &self.label_map).await.map_err(|e| classify_str(&e))
    }
    pub async fn find_gmail_ids_for_uuid(&self, target_uuid: &str) -> Result<Vec<String>, TransportError> {
        crate::gmail::find_gmail_ids_for_uuid(&self.token, target_uuid, &self.label_map)
            .await.map_err(|e| classify_str(&e))
    }
}
```

(`MessageIndex`, `TrashedNote`, `Note`, `DedupSummary` are re-exported in Phase 5; until then reference them as `crate::gmail::*`.)

- [ ] **Step 2: Route the `lib.rs` callers**

Replace each remaining `gmail::{list_notes,list_notes_in_label,list_account_index,list_trashed_notes,untrash_note,fetch_note,find_gmail_ids_for_uuid}` call with a constructed `gmail_vertical(...)` + the matching inherent method, mapping `TransportError` → `String`. Example for the main `list_notes` command:

```rust
let v = gmail_vertical(&token, &label_map, &account_id);
let (mut result, mut dedup) = v.list_notes(&cache_map).await.map_err(|e| e.to_string())?;
```

For `sync_pin_state` / `sync_tag_state` reads, route `gmail::list_meta_sidecars` / `gmail::list_tag_sidecars` through `v.list_pin_sidecars_in(meta_id)` / `v.list_tag_sidecars_in(meta_id)` (Task 3.3 helpers).

- [ ] **Step 3: Build + test + commit**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -10`
```bash
git add src-tauri/src/lib.rs src-tauri/src/backend/gmail/mod.rs
git commit -m "refactor(lib): route note/sidecar reads through GmailVertical orchestration"
```

### Task 4.5: Sweep for remaining `gmail::` function calls

**Files:**
- Modify: `src-tauri/src/lib.rs` (any stragglers)

- [ ] **Step 1: Find remaining function calls**

Run: `cd src-tauri && grep -n "gmail::[a-z_]*(" src/lib.rs`
Expected: ideally empty. Anything left is a function call not yet migrated (e.g. `gmail::get_label_map`, `gmail::canonicalize_uuid`, `gmail::data_uri`, `gmail::get_user_email`).

- [ ] **Step 2: Migrate the stragglers**

- `gmail::canonicalize_uuid` / `gmail::format_apple_uuid` / `gmail::data_uri` → `crate::mime822::*` (already re-exported; switch the path).
- `gmail::get_label_map` inside `cached_label_map` → keep delegating to `crate::gmail::get_label_map` for now (it runs *before* a vertical exists — it's what produces the label_map). Acceptable to leave; note it in the commit. Alternatively expose `GmailVertical::get_label_map` taking only the token. Prefer leaving it: it's the bootstrap that builds the vertical's input.
- `gmail::get_user_email` (sign-in/profile) → leave as `crate::gmail::get_user_email` (bootstrap, pre-vertical) or expose a static helper. Leaving is fine.

- [ ] **Step 3: Build + test + commit**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -10`
```bash
git add src-tauri/src/lib.rs
git commit -m "refactor(lib): migrate straggler gmail:: calls to mime822/vertical"
```

### Task 4.6: Full verification gate

**Files:** none (verification)

- [ ] **Step 1: Confirm only types/bootstrap remain on `gmail::`**

Run: `cd src-tauri && grep -n "gmail::" src/lib.rs`
Expected: only `gmail::Note`, `gmail::Attachment`, `gmail::DedupSummary`, `gmail::SidecarRef`, `gmail::TrashedNote`, `gmail::MessageIndex`, `gmail::FolderInfo` (types) and the documented bootstrap calls (`get_label_map`, `get_user_email`). No other function calls.

- [ ] **Step 2: Full test run**

Run: `cd src-tauri && cargo test 2>&1 | tail -30`
Expected: matches or exceeds the Task 0.1 baseline pass count.

- [ ] **Step 3: Release build + bundle layout check**

Run: `cd .. && npm run tauri build 2>&1 | tail -40`
Expected: builds; verify exactly one binary in `Contents/MacOS/` (CLAUDE.md edge #5). If macOS, run the same check the CI `Verify macOS bundle layout` step runs.

- [ ] **Step 4: Apple round-trip end-to-end check**

Build/run the app against the `Notes/play5` test subtree:
- Create a note with a `#tag` and a `[[link]]`; confirm it saves, pulls back, title strips/injects correctly.
- Edit it; confirm no duplicate, no title doubling.
- Pin it; confirm the sidecar appears in `Notes-Meta` and survives a re-pull.
- Delete it; confirm it goes to Trash and the sidecars are trashed.
Document results in the commit.

- [ ] **Step 5: Commit the verification note**

```bash
git commit --allow-empty -m "test: verify Vertical #0 routing — tests green, Apple round-trip intact on play5"
```

---

## Phase 5 — Remove the `gmail.rs` shim (optional cleanup)

The trait-surface goal is met after Phase 4. This phase is cosmetic: physically relocate the Gmail types + free functions into `backend/gmail/` and delete `gmail.rs`. Skip or defer if churn risk outweighs the tidiness benefit — the re-export shim is harmless.

### Task 5.1: Relocate Gmail types + free functions

**Files:**
- Modify: `src-tauri/src/backend/gmail/mod.rs` (host the types + remaining free fns)
- Modify: `src-tauri/src/lib.rs` (`use backend::gmail::*` for types; drop `mod gmail;`)
- Delete: `src-tauri/src/gmail.rs`

- [ ] **Step 1: Move the type definitions**

Move `Note`, `Attachment`, `SavedNote`, `MessageIndex`, `TrashedNote`, `DedupSummary`, `SidecarRef`, `TagSidecarRef`, `FolderInfo` and the still-needed free functions (`get_label_map`, `get_user_email`, `fetch_note`, `save_note`, `delete_note`, `list_*`, `ensure_label`, `create/rename/delete_label`, `modify_message_labels`, sidecar fns, `find_gmail_ids_for_uuid`, and the Gmail-JSON structs `GmailMessage`/`Payload`/`Part`/`Body`/`Header` + `get_header`/`header_param`/`find_html_in_parts`/`collect_pending_attachments`) from `gmail.rs` into `backend/gmail/` (split across `mod.rs`/`transport.rs` by responsibility). Keep the 3 remaining attachment tests with `collect_pending_attachments`.

- [ ] **Step 2: Update references**

In `lib.rs`: remove `mod gmail;`, change type references from `gmail::X` to `backend::gmail::X` (add a `use backend::gmail;` alias to minimize churn). Update the inherent methods that referenced `crate::gmail::*` to local paths.

- [ ] **Step 3: Delete the file**

Run: `git rm src-tauri/src/gmail.rs`

- [ ] **Step 4: Build + full test**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -30`
Expected: green, baseline pass count preserved.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(backend): relocate Gmail vertical out of gmail.rs; remove shim"
```

### Task 5.2: Update docs

**Files:**
- Modify: `CLAUDE.md` (active edge #1 → done; add the backend module map)

- [ ] **Step 1: Update CLAUDE.md**

Mark Active edge #1 as resolved (cite this plan + the resulting module layout: `mime822.rs`, `backend/mod.rs`, `backend/gmail/`). Note the Pragmatic deferrals (dedup decomposition, dyn dispatch, cursor storage) so the next engineer knows what's intentionally left for JMAP. Add the "Key files" entries for the new modules.

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: mark edge #1 (provider abstraction) resolved; document backend module map"
```

---

## Self-Review

**Spec coverage:**
- mime822 extraction → Phase 1 ✓
- 5 traits (Transport/AtRest/Identity/Deriver/MetadataSidecar) → Phase 2 + 3 ✓ (AtRest realized via mime822 encode + Gmail JSON decode, per Pragmatic note)
- Vertical + Capabilities (folder_model + fidelity; interops_with_apple demoted) → Task 3.1 ✓
- Static dispatch, no dyn → constructor helper + concrete `GmailVertical` ✓
- MetadataSidecar pulls gmail:: out of worker → Task 3.3 + 4.2 ✓
- Pragmatic list_notes (keep dedup/sort) → Task 4.4 inherent methods ✓
- changes_since defined + implemented (full-scan, inert cursor), worker not rewired → Task 3.2 ✓
- TransportError classifier → Task 3.1 ✓
- No schema migration; deferrals listed → respected throughout ✓
- Behavior-identical verification (tests + Apple round-trip) → Task 1.3, 4.6 ✓

**Placeholder scan:** The two `unimplemented!` calls (`Transport::save` pre-step-2, `MetadataSidecar::list_sidecars`/`put_sidecar`) are intentional and documented — `Transport::save` is made concrete in Task 3.2 step 2; the two sidecar trait methods have no core caller in this pass (the worker uses the inherent `*_in` helpers), which is stated explicitly. Acceptable as designed; not stray TODOs.

**Type consistency:** `SaveOp` fields (`existing_remote_id`/`existing_uuid`/`existing_created_date`) are used consistently in Tasks 3.2 and 4.1. `RemoteFolder { id, path }` consistent across Transport methods and Task 4.3 adaptation. `SidecarRecord`/`SidecarKind` consistent between Task 2.1 and 3.3. `classify_str` defined in Task 3.1 and reused in 3.2/3.3/4.4. `gmail_vertical(...)` helper signature consistent across Phase 4.

**Known soft spots flagged for the implementer:** Task 3.4 (Deriver helper names) and Task 5.1 (struct relocation split) depend on real symbol names discovered at implementation time — both have explicit "grep first / replace names" steps rather than guessed code.
