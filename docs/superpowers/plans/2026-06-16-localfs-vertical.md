# LocalFS Vertical #1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a second backend vertical (LocalFS — `.eml` files on disk) behind the existing trait surface, proving the federation design holds: dynamic dispatch, an account with no OAuth/keychain, a filesystem transport, raw-RFC822 decode, and two verticals coexisting in one shared core — with the Gmail path behavior-identical.

**Architecture:** Promote Gmail's inherent orchestration methods to a `NoteStore` trait and make `Vertical` a super-trait of all facets, so the core drives any backend through `Box<dyn Vertical>`. Gmail's dedup-by-UUID stays a Gmail-internal quirk (LocalFS has none). LocalFS reuses `mime822` (encode) + the Apple-HTML content model + editor; its one genuinely new piece is a raw-RFC822 decoder (via `mail-parser`). Notes are `.eml` files; folders are subdirectories; pin/tags are `.meta/` files; deletes move to `.trash/`.

**Tech Stack:** Rust, Tauri 2, `async-trait`, `mail-parser` (new), `tauri-plugin-dialog` (new), `walkdir` (new), `dirs`, existing `cargo test` suite as the regression net + a tempdir-based integration example.

**Source spec:** [docs/superpowers/specs/2026-06-16-localfs-vertical-design.md](../specs/2026-06-16-localfs-vertical-design.md)

**Acceptance bar:** all existing 76 Rust tests stay green (Gmail behavior-identical); `examples/roundtrip_localfs.rs` round-trips a note on a tempdir; a LocalFS account is usable in the running app; Gmail + LocalFS coexist with the neutral index spanning both. Feature branch `feat/localfs-vertical`.

---

## File Structure

| File | Responsibility |
|---|---|
| `src-tauri/src/backend/mod.rs` (modify) | Add `NoteStore` trait; make `Vertical` a super-trait (`Transport + MetadataSidecar + NoteStore + Identity + Deriver`); host relocated neutral types (`Note`, `Attachment`, `SavedNote`, `MessageIndex`, `TrashedNote`, `DedupSummary`). |
| `src-tauri/src/backend/gmail/mod.rs` (modify) | Implement `NoteStore` for `GmailVertical` (move the inherent methods into it); `new()` gains `meta_label`. |
| `src-tauri/src/backend/gmail/transport.rs` (modify) | Make `MetadataSidecar` real for Gmail (resolve meta-label internally); map Gmail sidecar types → neutral `SidecarRecord`. |
| `src-tauri/src/backend/gmail/wire.rs` (modify) | Drop the neutral type *definitions* (now in core); keep Gmail-JSON structs + functions; re-import the neutral types. |
| `src-tauri/src/backend/localfs/mod.rs` (create) | `LocalFsVertical { root_dir, account_id }` + `Vertical`/`Capabilities` + `Identity`. |
| `src-tauri/src/backend/localfs/transport.rs` (create) | FS `Transport` + `NoteStore` + `MetadataSidecar` impls. |
| `src-tauri/src/backend/localfs/decode.rs` (create) | raw-RFC822 bytes → neutral envelope (via `mail-parser`) + symmetry test. |
| `src-tauri/src/backend/deriver_applehtml.rs` (create) | Shared `AppleHtmlDeriver` (extract from gmail/deriver.rs) used by both verticals. |
| `src-tauri/src/accounts.rs` (modify) | `BackendKind` enum; `Account.backend_kind` + `root_dir`; readiness branch. |
| `src-tauri/src/lib.rs` (modify) | `vertical_for(state, account_id) -> Box<dyn Vertical>`; route worker + commands through `&dyn Vertical`; `add_local_account` command; simplify sidecar push to trait calls. |
| `src-tauri/src/main.rs` / `tauri.conf.json` (modify) | Register `tauri-plugin-dialog`. |
| `src/lib/components/*` (modify) | "Add Local Folder" button + dir picker; 📁 marker on local accounts. |
| `src-tauri/Cargo.toml` (modify) | Add `mail-parser`, `walkdir`, `tauri-plugin-dialog`. |
| `src-tauri/examples/roundtrip_localfs.rs` (create) | Tempdir integration round-trip (no network). |

**Phasing:** A (core enabler) → B (LocalFS vertical) → C (account + UI). Each phase leaves the tree compiling and `cargo test` green.

---

## Phase A — Core: trait promotion + dynamic dispatch

### Task A0: Branch + baseline

**Files:** none

- [ ] **Step 1:** `git branch --show-current` → expect `feat/localfs-vertical` (created in brainstorming). If not: `git checkout -b feat/localfs-vertical`.
- [ ] **Step 2:** `cd src-tauri && cargo test 2>&1 | tail -5` → record baseline **76 passed**. Every Phase-A task must preserve this.

### Task A1: Relocate neutral types to core

**Files:**
- Modify: `src-tauri/src/backend/mod.rs`, `src-tauri/src/backend/gmail/wire.rs`

- [ ] **Step 1:** Move the *definitions* of `Note`, `Attachment`, `SavedNote`, `MessageIndex`, `TrashedNote`, `DedupSummary` from `wire.rs` to `backend/mod.rs` (they are format-neutral structs). Keep their derives (`Serialize`/`Deserialize`/`Clone`/`Debug`) and `#[serde(...)]` attrs verbatim. Leave the Gmail-JSON structs (`GmailMessage`, `Payload`, `Part`, `Body`, `Header`, `FolderInfo`, `SidecarRef`, `TagSidecarRef`) in `wire.rs`.
- [ ] **Step 2:** In `wire.rs`, add `use crate::backend::{Note, Attachment, SavedNote, MessageIndex, TrashedNote, DedupSummary};` so its functions still resolve those names.
- [ ] **Step 3:** Fix references: anything that said `wire::Note` etc. (e.g. in `backend/gmail/mod.rs`, `transport.rs`) now also resolves via `crate::backend::Note`. Prefer `crate::backend::Note` at use sites. `lib.rs` references via the `use backend::gmail::wire as gmail;` alias still work if `wire` re-imports them (Step 2) — verify with build.
- [ ] **Step 4:** `cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -5` → 76 passed.
- [ ] **Step 5:** Commit:
```bash
git add src/backend/ && git commit -m "refactor(backend): relocate neutral note types to core module"
```

### Task A2: Define `NoteStore` + make `Vertical` a super-trait

**Files:**
- Modify: `src-tauri/src/backend/mod.rs`

- [ ] **Step 1:** Add the `NoteStore` trait to `backend/mod.rs`:
```rust
use std::collections::HashMap;

/// Per-vertical note read/write orchestration. Each backend implements its own
/// strategy (Gmail dedups transient duplicates; LocalFS has one file per uuid so
/// it does not). The generic post-processing (sort, cache upsert, conflict,
/// index, prune) stays in the core, not here.
#[async_trait]
pub trait NoteStore: Send + Sync {
    async fn list_all_notes(&self, cache_by_id: &HashMap<String, Note>) -> Result<(Vec<Note>, DedupSummary), TransportError>;
    async fn list_notes_in_folder(&self, folder: &str, cache_by_id: &HashMap<String, Note>) -> Result<Vec<Note>, TransportError>;
    async fn list_index(&self) -> Result<Vec<MessageIndex>, TransportError>;
    async fn fetch_note(&self, remote_id: &str) -> Result<Note, TransportError>;
    async fn save_note_full(&self, op: &SaveOp<'_>, attachments: &[Attachment]) -> Result<SavedNote, TransportError>;
    async fn find_ids_for_uuid(&self, uuid: &str) -> Result<Vec<String>, TransportError>;
    async fn list_trashed(&self) -> Result<Vec<TrashedNote>, TransportError>;
    async fn untrash(&self, remote_id: &str) -> Result<(), TransportError>;
}
```
- [ ] **Step 2:** Change the `Vertical` trait to a super-trait:
```rust
pub trait Vertical: Transport + MetadataSidecar + NoteStore + Identity + Deriver + Send + Sync {
    fn backend_id(&self) -> &str;
    fn capabilities(&self) -> &Capabilities;
}
```
- [ ] **Step 3:** `cargo build 2>&1 | tail -20` → expect an error that `GmailVertical: Vertical` is no longer satisfied (missing `NoteStore`). That's expected; Task A3 fixes it. (Do NOT commit a non-compiling tree — proceed straight to A3, then build+commit together.)

### Task A3: Implement `NoteStore` + real `MetadataSidecar` for Gmail

**Files:**
- Modify: `src-tauri/src/backend/gmail/mod.rs`, `src-tauri/src/backend/gmail/transport.rs`

- [ ] **Step 1:** In `backend/gmail/mod.rs`, convert the inherent methods block (`list_notes`, `list_notes_in_label`, `list_account_index`, `list_trashed_notes`, `untrash_note`, `fetch_note`, `find_gmail_ids_for_uuid`) into an `#[async_trait] impl NoteStore for GmailVertical` block, renaming to the trait names: `list_notes`→`list_all_notes`, `list_notes_in_label`→`list_notes_in_folder` (param `folder`: resolve the folder name → label id via `self.label_map` internally, then delegate to the existing `wire::list_notes_in_label`), `list_account_index`→`list_index`, `list_trashed_notes`→`list_trashed`, `untrash_note`→`untrash`, `fetch_note` (same name), `find_gmail_ids_for_uuid`→`find_ids_for_uuid`. Keep `save_note_full` (already matches `NoteStore::save_note_full` signature — move it into the impl). Bodies stay byte-identical (still delegate to `wire::*`); Gmail's dedup remains inside `wire::list_notes`.
  - `list_notes_in_folder` note: the existing `wire::list_notes_in_label` takes a `label_id`. Resolve `folder` (e.g. `"Notes/play5"`) → label id via `self.label_map.iter().find(|(_,n)| **n == folder)`. If not found, return `Ok(vec![])` (folder not on the backend yet).
- [ ] **Step 2:** `GmailVertical::new` gains a `meta_label: String` param: change signature to `new(token, label_map, user_email, meta_label)` and store `pub(crate) meta_label: String`. (Callers updated in A4.)
- [ ] **Step 3:** In `transport.rs`, replace the two `unimplemented!` `MetadataSidecar` methods with real impls that resolve the meta-label internally (no `meta_label_id` param leaks to the trait):
```rust
#[async_trait]
impl MetadataSidecar for GmailVertical {
    async fn list_sidecars(&self, kind: SidecarKind) -> Result<Vec<SidecarRecord>, TransportError> {
        // Resolve meta_label id; if the label doesn't exist yet, there are no sidecars.
        let meta_id = match self.label_map.iter().find(|(_, n)| **n == self.meta_label) {
            Some((id, _)) => id.clone(),
            None => return Ok(vec![]),
        };
        match kind {
            SidecarKind::Pin => {
                let refs = crate::backend::gmail::wire::list_meta_sidecars(&self.token, &meta_id)
                    .await.map_err(|e| classify_str(&e))?;
                Ok(refs.into_iter().map(|r| SidecarRecord { id: r.id, note_uuid: r.note_uuid, kind: SidecarKind::Pin, body: None }).collect())
            }
            SidecarKind::Tags => {
                let refs = crate::backend::gmail::wire::list_tag_sidecars(&self.token, &meta_id)
                    .await.map_err(|e| classify_str(&e))?;
                Ok(refs.into_iter().map(|r| {
                    let body = serde_json::to_vec(&serde_json::json!({"tags": r.tags})).ok();
                    SidecarRecord { id: r.id, note_uuid: r.note_uuid, kind: SidecarKind::Tags, body }
                }).collect())
            }
        }
    }
    async fn put_sidecar(&self, note_uuid: &str, kind: SidecarKind, body: Option<&[u8]>, replace: Option<&str>) -> Result<String, TransportError> {
        // Ensure the meta-label exists (create on first use; 409 self-adopts).
        let meta_id = crate::backend::gmail::wire::ensure_label(&self.token, &self.meta_label, &self.label_map)
            .await.map_err(|e| classify_str(&e))?;
        let payload = body.map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_else(|| "{}".to_string());
        match kind {
            SidecarKind::Pin => crate::backend::gmail::wire::save_meta_sidecar(&self.token, note_uuid, &payload, &meta_id, replace, &self.user_email).await,
            SidecarKind::Tags => crate::backend::gmail::wire::save_tag_sidecar(&self.token, note_uuid, &payload, &meta_id, replace, &self.user_email).await,
        }.map_err(|e| classify_str(&e))
    }
    async fn remove_sidecar(&self, id: &str) -> Result<(), TransportError> {
        crate::backend::gmail::wire::trash_meta_sidecar(&self.token, id).await.map_err(|e| classify_str(&e))
    }
}
```
  Delete the old inherent `list_pin_sidecars_in`/`list_tag_sidecars_in`/`put_pin_sidecar_in`/`put_tag_sidecar_in` helpers (their callers move to the trait in A4).
- [ ] **Step 4:** `cargo build 2>&1 | tail -30`. Expect errors only in `lib.rs` (it still calls the removed inherent helpers + old `gmail_vertical()` + `GmailVertical::new` old signature). Those are fixed in A4. If `backend/` itself doesn't compile, fix here. Do not commit yet (tree not green until A4).

### Task A4: `vertical_for` + dynamic dispatch + route lib.rs/worker

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1:** Replace the `gmail_vertical(...)` helper with:
```rust
use crate::backend::Vertical;

/// Build the backend vertical for an account (dynamic dispatch). Gmail does the
/// token+label_map bootstrap here; future non-Gmail backends branch on kind.
async fn vertical_for(state: &State<'_, AppState>, account_id: &str) -> Result<Box<dyn Vertical>, String> {
    // Phase A: only Gmail exists; Phase C adds the LocalFs branch.
    let token = ensure_token(state, account_id).await?;
    let label_map = cached_label_map(state, account_id, &token).await?;
    let meta_label = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == account_id)
            .map(|a| a.effective_meta_label().to_string())
            .ok_or_else(|| format!("account {} not found", account_id))?
    };
    Ok(Box::new(backend::gmail::GmailVertical::new(token, label_map, account_id.to_string(), meta_label)))
}
```
- [ ] **Step 2:** Update every call site. The previous code did `let v = gmail_vertical(&token, &label_map, &account_id);` after manually fetching token+label_map. Now: `let v = vertical_for(&state, &account_id).await?;` and remove the now-redundant local `token`/`label_map` fetches **only where they were used solely to build the vertical**. Where `token`/`label_map` are still needed for the 5 bootstrap calls (`get_label_map`, `get_user_email`) or for `meta_id` lookups that moved into the vertical, keep them. Method-name changes at call sites:
  - `v.list_notes(&cache)` → `v.list_all_notes(&cache)`
  - `v.list_notes_in_label(&label_id, &cache)` → `v.list_notes_in_folder(&folder_name, &cache)` (pass the folder *name*, not label id — the vertical resolves it; find the folder name from the existing label-id→name lookup at that site)
  - `v.list_account_index()` → `v.list_index()`
  - `v.list_trashed_notes()` → `v.list_trashed()`; `v.untrash_note(id)` → `v.untrash(id)`
  - `v.find_gmail_ids_for_uuid(u)` → `v.find_ids_for_uuid(u)`
  - `v.fetch_note(id)`, `v.save_note_full(&op, &att)`, `v.delete(id)` — unchanged names
  - Pin push: replace `v.put_pin_sidecar_in(uuid, payload, meta_id, replace)` + the surrounding `ensure_label`/`invalidate_label_cache` with `v.put_sidecar(&n.uuid, SidecarKind::Pin, Some(payload.as_bytes()), n.meta_msg_id.as_deref()).await` for the pinned branch, and `v.remove_sidecar(old)` for unpin. Drop the now-unneeded `ensure_label`+`invalidate_label_cache` lines (the vertical ensures internally). `mark_pin_pushed` consumes the returned id as before.
  - Tag push: same shape with `SidecarKind::Tags` and the `{"tags":[...]}` payload.
  - Pin/tag PULL (`sync_pin_state`/`sync_tag_state`): replace `v.list_pin_sidecars_in(meta_id)` with `v.list_sidecars(SidecarKind::Pin)` (returns `Vec<SidecarRecord>`; adapt the apply loop to read `rec.note_uuid` / `rec.id`; for tags parse `rec.body` JSON `{"tags":[...]}`). Remove the now-unused manual `meta_id` resolution at these sites.
  - `import crate::backend::SidecarKind;` where needed.
- [ ] **Step 3:** Worker tick (`push_one_dirty`/`push_one_deletion`/`push_one_pin`/`push_one_tag_set`/`push_one_folder` + the read commands): each builds `vertical_for(&state, &account_id).await?` instead of the manual token/label_map + `gmail_vertical`. For `push_one_deletion` (which used an empty map before), `vertical_for` now does the Gmail bootstrap — that's fine (it needs the token to delete anyway). Keep all control flow, logging, `mark_*` calls identical.
- [ ] **Step 4:** `cargo build 2>&1 | tail -30` until clean, then `cargo test 2>&1 | tail -8` → **76 passed**.
- [ ] **Step 5:** Commit (A2+A3+A4 together — first green point):
```bash
git add src/ && git commit -m "refactor(backend): NoteStore trait + Box<dyn Vertical> dynamic dispatch

Promote Gmail's inherent orchestration to NoteStore; make Vertical a super-trait
of all facets; unify sidecars under MetadataSidecar (meta-label resolved inside
the vertical, self-heals via 409-adopt); route lib.rs + worker via vertical_for.
Gmail behavior-identical; 76 tests green."
```

### Task A5: Phase-A verification gate

**Files:** none

- [ ] **Step 1:** `cargo test 2>&1 | tail -8` → 76 passed. `cargo build --release 2>&1 | grep -ic warning` → expect 0 (annotate any new dead code instead of leaving warnings).
- [ ] **Step 2:** Live regression: `cargo run --example roundtrip_refactor 2>&1 | grep -E "PASS|FAIL|✅|❌"` → all PASS (the Gmail vertical still round-trips through the new dyn path against the real account).
- [ ] **Step 3:** Commit any annotation fixes; otherwise proceed to Phase B.

---

## Phase B — LocalFS vertical

### Task B1: Dependencies + module scaffold + shared deriver

**Files:**
- Modify: `src-tauri/Cargo.toml`, `src-tauri/src/backend/mod.rs`
- Create: `src-tauri/src/backend/localfs/mod.rs`, `src-tauri/src/backend/deriver_applehtml.rs`
- Modify: `src-tauri/src/backend/gmail/deriver.rs`

- [ ] **Step 1:** Add to `Cargo.toml` `[dependencies]`:
```toml
mail-parser = "0.9"
walkdir = "2"
```
Run `cargo build 2>&1 | tail -5` to fetch.
- [ ] **Step 2:** Extract the shared Apple-HTML deriver. Create `backend/deriver_applehtml.rs`:
```rust
use crate::backend::{ContentKind, Derived, Deriver, Edge};

/// Deriver for the Apple-HTML content model, shared by every vertical that
/// stores Apple-HTML bodies (Gmail, LocalFS). Delegates to the same pure db.rs
/// body parsers the write path uses.
pub struct AppleHtmlDeriver;

impl Deriver for AppleHtmlDeriver {
    fn derive(&self, _kind: ContentKind, blob: &[u8]) -> Derived {
        let body = std::str::from_utf8(blob).unwrap_or("");
        let text = crate::db::strip_html_to_text(body);
        let tags = crate::db::tags_from_body(body);
        let edges = crate::db::extract_wikilinks(body)
            .into_iter().map(|target| Edge { rel: "mentions".to_string(), target }).collect();
        Derived { text, tags, edges }
    }
}
```
Add `pub mod deriver_applehtml;` to `backend/mod.rs`. In `backend/gmail/deriver.rs`, replace the body of `impl Deriver for GmailVertical::derive` to delegate: `crate::backend::deriver_applehtml::AppleHtmlDeriver.derive(kind, blob)`. Keep the existing deriver test (it now exercises the shared path).
- [ ] **Step 3:** Create `backend/localfs/mod.rs`:
```rust
//! Vertical #1 — LocalFS. Stores notes as .eml files (RFC822 wrapping the same
//! Apple-HTML body as Gmail) under a root directory. Reuses mime822 (encode),
//! the Apple-HTML content model + editor, and Identity (X-UUID in the file). The
//! one new piece is raw-RFC822 decode (decode.rs).

use super::{Capabilities, ContentKind, Derived, Deriver, Fidelity, FolderModel, Identity, Vertical};

pub mod transport;
pub mod decode;

pub struct LocalFsVertical {
    pub(crate) root: std::path::PathBuf,
    pub(crate) account_id: String,
    capabilities: Capabilities,
}

impl LocalFsVertical {
    pub fn new(root: std::path::PathBuf, account_id: String) -> Self {
        Self { root, account_id, capabilities: Capabilities { folder_model: FolderModel::SingleExclusive, fidelity: Fidelity::Full } }
    }
    fn notes_dir(&self) -> std::path::PathBuf { self.root.join("Notes") }
    fn trash_dir(&self) -> std::path::PathBuf { self.root.join(".trash") }
    fn meta_dir(&self) -> std::path::PathBuf { self.root.join(".meta") }
}

impl Identity for LocalFsVertical {
    fn mint(&self) -> String { crate::mime822::format_apple_uuid(uuid::Uuid::new_v4()) }
}
impl Deriver for LocalFsVertical {
    fn derive(&self, kind: ContentKind, blob: &[u8]) -> Derived {
        crate::backend::deriver_applehtml::AppleHtmlDeriver.derive(kind, blob)
    }
}
impl Vertical for LocalFsVertical {
    fn backend_id(&self) -> &str { "localfs" }
    fn capabilities(&self) -> &Capabilities { &self.capabilities }
}
```
Add `pub mod localfs;` to `backend/mod.rs`.
- [ ] **Step 4:** `cargo build 2>&1 | tail -20` → expect errors only that `LocalFsVertical` doesn't yet impl `Transport`/`NoteStore`/`MetadataSidecar` (Tasks B3/B4). The deriver extraction + scaffold should compile on their own except the missing impls referenced by `Vertical`. To keep the tree green, temporarily DO NOT add `impl Vertical for LocalFsVertical` until B4 — comment it out with a `// wired in B4` note, build, then commit B1 (deps + deriver extraction + scaffold structs) green.
- [ ] **Step 5:** `cargo test 2>&1 | tail -5` → 76 passed. Commit:
```bash
git add Cargo.toml Cargo.lock src/backend/ && git commit -m "feat(localfs): add deps, shared AppleHtmlDeriver, LocalFsVertical scaffold"
```

### Task B2: raw-RFC822 decode + symmetry test

**Files:**
- Create: `src-tauri/src/backend/localfs/decode.rs`

- [ ] **Step 1: Write the symmetry test first (TDD).** In `decode.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::decode_eml;

    #[test]
    fn build_then_decode_roundtrips_envelope() {
        let raw = crate::mime822::build_note_mime(
            "My Title", "<html><body><div>My Title</div><div>hello #t [[L-abcd1234]]</div></body></html>",
            "AAAA-BBBB", "Thu, 4 Jun 2026 01:19:50 +0700",
            "Mon, 1 Jan 2024 09:00:00 +0700", "u@x.com", &[],
        );
        let d = decode_eml(raw.as_bytes(), "Notes").expect("decode");
        assert_eq!(d.uuid, "AAAA-BBBB");
        assert_eq!(d.title, "My Title");
        assert_eq!(d.label, "Notes");
        // editor-view body: title row stripped, content + tag + link retained
        assert!(d.body_html.contains("hello #t [[L-abcd1234]]"));
        assert!(!d.body_html.contains("<div>My Title</div>"), "title row must be stripped");
        assert_eq!(d.x_mail_created_date.as_deref(), Some("Mon, 1 Jan 2024 09:00:00 +0700"));
    }
}
```
- [ ] **Step 2: Run it (fails — `decode_eml` undefined).** `cargo test --lib decode 2>&1 | tail -5` → FAIL.
- [ ] **Step 3: Implement `decode_eml`.** Returns a `Note` (the neutral type). Uses `mail-parser` for the envelope, then reuses `mime822::strip_leading_title` for the editor-view body:
```rust
use mail_parser::MessageParser;
use crate::backend::{Attachment, Note};

/// Parse a raw .eml byte buffer (RFC822) into a neutral Note. `label` is the
/// folder this file lives in (derived from its path by the caller). Mirrors the
/// Gmail decode but over raw bytes; reuses mime822 helpers for the HTML body.
pub fn decode_eml(bytes: &[u8], label: &str) -> Result<Note, String> {
    let msg = MessageParser::default().parse(bytes).ok_or("not a valid RFC822 message")?;
    let header = |name: &str| msg.header(name).and_then(|h| h.as_text()).map(|s| s.to_string()).unwrap_or_default();
    let title_raw = msg.subject().map(|s| s.to_string()).unwrap_or_default();
    let title = crate::mime822::try_recover_mis_decoded_utf8(&title_raw).unwrap_or(title_raw);
    let uuid_raw = header("X-Universally-Unique-Identifier");
    let uuid = crate::mime822::canonicalize_uuid(&uuid_raw).unwrap_or(uuid_raw);
    let date = header("Date");
    let x_mail = { let v = header("X-Mail-Created-Date"); (!v.is_empty()).then_some(v) };
    // HTML body: prefer the text/html part.
    let html = msg.html_body_count().gt(&0)
        .then(|| msg.body_html(0).map(|c| c.into_owned()))
        .flatten()
        .unwrap_or_default();
    let body_html = crate::mime822::strip_leading_title(&html, &title);
    // Attachments: parts with a Content-Id.
    let mut attachments = Vec::new();
    for part in msg.attachments() {
        if let Some(cid) = part.content_id() {
            attachments.push(Attachment {
                content_id: cid.trim_matches(|c| c == '<' || c == '>').to_string(),
                mime_type: part.content_type().and_then(|c| c.c_type.as_ref().map(|t| {
                    match &c.c_subtype { Some(s) => format!("{}/{}", t, s), None => t.to_string() }
                })).unwrap_or_else(|| "application/octet-stream".to_string()),
                filename: part.attachment_name().map(|s| s.to_string()),
                x_apple_part_url: None,
                data: part.contents().to_vec(),
            });
        }
    }
    Ok(Note {
        id: String::new(), // remote_id (path) set by the transport
        uuid, title, body_html, date, label: label.to_string(),
        x_mail_created_date: x_mail, account_id: None, pinned: false, attachments,
    })
}
```
  (Adjust field names to the actual `Note`/`Attachment` definitions from A1 — verify against `backend/mod.rs`. The `mail-parser` 0.9 API names above may need minor tweaks; consult its docs and make the test pass.)
- [ ] **Step 4: Run the test — pass.** `cargo test --lib decode 2>&1 | tail -5` → PASS (77 total).
- [ ] **Step 5: Commit.**
```bash
git add src/backend/localfs/decode.rs && git commit -m "feat(localfs): raw-RFC822 decode via mail-parser + symmetry test"
```

### Task B3: LocalFS `Transport` + `NoteStore`

**Files:**
- Create: `src-tauri/src/backend/localfs/transport.rs`

- [ ] **Step 1:** Implement `Transport` for `LocalFsVertical`. Key operations (use `std::fs` + `walkdir`):
```rust
use crate::backend::{ChangeSet, RemoteFolder, SaveOp, SaveOutcome, SyncCursor, Transport, TransportError};
use super::LocalFsVertical;
use async_trait::async_trait;

fn perm(e: std::io::Error) -> TransportError { TransportError::Permanent { source: e.into() } }

impl LocalFsVertical {
    // Map a folder label ("Notes" or "Notes/play5") to an on-disk dir under root.
    fn folder_path(&self, folder: &str) -> std::path::PathBuf {
        // folder always starts with "Notes"; strip the leading "Notes" and join.
        let rel = folder.strip_prefix("Notes").unwrap_or(folder).trim_start_matches('/');
        if rel.is_empty() { self.notes_dir() } else { self.notes_dir().join(rel) }
    }
    fn note_path(&self, folder: &str, uuid: &str) -> std::path::PathBuf {
        self.folder_path(folder).join(format!("{}.eml", uuid))
    }
}

#[async_trait]
impl Transport for LocalFsVertical {
    async fn changes_since(&self, _cursor: Option<&SyncCursor>) -> Result<ChangeSet, TransportError> {
        // Full scan: the NoteStore::list_all_notes path is the driver; this exists
        // to honor the trait with a real (timestamp) cursor. Return empty changes
        // + a timestamp cursor (the worker uses list_all_notes + core prune).
        Ok(ChangeSet { changes: vec![], next_cursor: SyncCursor(Vec::new()), more: false })
    }
    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError> {
        // Delegate to NoteStore::save_note_full with no attachments for the lean path.
        let saved = crate::backend::NoteStore::save_note_full(self, &op, &[]).await?;
        Ok(SaveOutcome { remote_id: saved.id, cursor_hint: None })
    }
    async fn delete(&self, remote_id: &str) -> Result<(), TransportError> {
        // remote_id is the file path relative to root. Move to .trash/<uuid>.eml.
        let src = self.root.join(remote_id);
        if !src.exists() { return Ok(()); } // idempotent
        std::fs::create_dir_all(self.trash_dir()).map_err(perm)?;
        let fname = src.file_name().ok_or(TransportError::NotFound)?;
        std::fs::rename(&src, self.trash_dir().join(fname)).map_err(perm)
    }
    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError> {
        let mut out = vec![RemoteFolder { id: "Notes".into(), path: "Notes".into() }];
        for entry in walkdir::WalkDir::new(self.notes_dir()).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_dir() && entry.path() != self.notes_dir() {
                if let Ok(rel) = entry.path().strip_prefix(&self.root) {
                    let path = rel.to_string_lossy().replace('\\', "/");
                    out.push(RemoteFolder { id: path.clone(), path });
                }
            }
        }
        Ok(out)
    }
    async fn ensure_folder(&self, path: &str) -> Result<RemoteFolder, TransportError> {
        let dir = self.folder_path(path);
        std::fs::create_dir_all(&dir).map_err(perm)?;
        Ok(RemoteFolder { id: path.to_string(), path: path.to_string() })
    }
    async fn create_folder(&self, name: &str) -> Result<RemoteFolder, TransportError> { self.ensure_folder(name).await }
    async fn rename_folder(&self, id: &str, new_name: &str) -> Result<(), TransportError> {
        std::fs::rename(self.folder_path(id), self.folder_path(new_name)).map_err(perm)
    }
    async fn delete_folder(&self, id: &str) -> Result<(), TransportError> {
        let dir = self.folder_path(id);
        if dir.exists() { std::fs::remove_dir_all(dir).map_err(perm)?; }
        Ok(())
    }
    async fn move_note(&self, remote_id: &str, add: &[String], remove: &[String]) -> Result<(), TransportError> {
        // add[0] = destination folder label; move the file's dir, keep filename.
        let Some(dest_folder) = add.first() else { return Ok(()) };
        let src = self.root.join(remote_id);
        let fname = src.file_name().ok_or(TransportError::NotFound)?.to_owned();
        let dest_dir = self.folder_path(dest_folder);
        std::fs::create_dir_all(&dest_dir).map_err(perm)?;
        std::fs::rename(&src, dest_dir.join(fname)).map_err(perm)?;
        let _ = remove; // FS move implies removal from the old dir
        Ok(())
    }
}
```
- [ ] **Step 2:** Implement `NoteStore` for `LocalFsVertical`:
```rust
use crate::backend::{Attachment, DedupSummary, MessageIndex, Note, NoteStore, SavedNote, TrashedNote};
use std::collections::HashMap;

impl LocalFsVertical {
    fn read_note_at(&self, path: &std::path::Path) -> Option<Note> {
        let bytes = std::fs::read(path).ok()?;
        let rel_dir = path.parent()?.strip_prefix(&self.root).ok()?;
        let label = { let s = rel_dir.to_string_lossy().replace('\\', "/"); if s.is_empty() { "Notes".to_string() } else { s } };
        let mut note = super::decode::decode_eml(&bytes, &label).ok()?;
        // remote_id = path relative to root (stable across edits).
        note.id = path.strip_prefix(&self.root).ok()?.to_string_lossy().replace('\\', "/");
        Some(note)
    }
    fn all_eml(&self) -> Vec<std::path::PathBuf> {
        walkdir::WalkDir::new(self.notes_dir()).into_iter().filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && e.path().extension().map(|x| x == "eml").unwrap_or(false))
            .map(|e| e.path().to_path_buf()).collect()
    }
}

#[async_trait]
impl NoteStore for LocalFsVertical {
    async fn list_all_notes(&self, _cache: &HashMap<String, Note>) -> Result<(Vec<Note>, DedupSummary), TransportError> {
        // One file per uuid → no dedup. (The cache arg is unused: parsing a local
        // file is cheap, unlike a Gmail messages.get; revisit only if measured.)
        let notes = self.all_eml().iter().filter_map(|p| self.read_note_at(p)).collect();
        Ok((notes, DedupSummary::default()))
    }
    async fn list_notes_in_folder(&self, folder: &str, _cache: &HashMap<String, Note>) -> Result<Vec<Note>, TransportError> {
        let dir = self.folder_path(folder);
        let notes = walkdir::WalkDir::new(&dir).max_depth(1).into_iter().filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && e.path().extension().map(|x| x == "eml").unwrap_or(false))
            .filter_map(|e| self.read_note_at(e.path())).collect();
        Ok(notes)
    }
    async fn list_index(&self) -> Result<Vec<MessageIndex>, TransportError> {
        Ok(self.all_eml().iter().filter_map(|p| {
            let n = self.read_note_at(p)?;
            Some(MessageIndex { id: n.id, label: n.label })
        }).collect())
    }
    async fn fetch_note(&self, remote_id: &str) -> Result<Note, TransportError> {
        self.read_note_at(&self.root.join(remote_id)).ok_or(TransportError::NotFound)
    }
    async fn save_note_full(&self, op: &SaveOp<'_>, attachments: &[Attachment]) -> Result<SavedNote, TransportError> {
        let uuid = op.existing_uuid.filter(|s| !s.is_empty())
            .and_then(crate::mime822::canonicalize_uuid)
            .unwrap_or_else(|| crate::mime822::format_apple_uuid(uuid::Uuid::new_v4()));
        let now = crate::mime822::format_apple_date(chrono::Local::now());
        let created = op.existing_created_date.filter(|s| !s.is_empty()).map(|s| s.to_string()).unwrap_or_else(|| now.clone());
        let used: Vec<crate::mime822::MimeAttachment> = {
            let body_with_title = crate::mime822::inject_title_into_body(op.body_html, op.title);
            let cids = crate::mime822::referenced_cids(&body_with_title);
            attachments.iter().filter(|a| cids.iter().any(|c| *c == a.content_id))
                .map(|a| crate::mime822::MimeAttachment { content_id: &a.content_id, mime_type: &a.mime_type, filename: a.filename.as_deref(), x_apple_part_url: a.x_apple_part_url.as_deref(), data: &a.data })
                .collect()
        };
        let raw = crate::mime822::build_note_mime(op.title, op.body_html, &uuid, &now, &created, "local@jodd", &used);
        let dir = self.folder_path(op.label);
        std::fs::create_dir_all(&dir).map_err(perm)?;
        // If the file already exists in a DIFFERENT folder (folder change), remove the old.
        if let Some(old_id) = op.existing_remote_id.filter(|s| !s.is_empty()) {
            let old = self.root.join(old_id);
            let new = dir.join(format!("{}.eml", uuid));
            if old.exists() && old != new { let _ = std::fs::remove_file(&old); }
        }
        let path = dir.join(format!("{}.eml", uuid));
        std::fs::write(&path, raw.as_bytes()).map_err(perm)?;
        let rel = path.strip_prefix(&self.root).map_err(|e| TransportError::Permanent { source: e.into() })?.to_string_lossy().replace('\\', "/");
        Ok(SavedNote { id: rel, uuid, date: now, body_html: op.body_html.to_string() })
    }
    async fn find_ids_for_uuid(&self, uuid: &str) -> Result<Vec<String>, TransportError> {
        Ok(self.all_eml().iter().filter_map(|p| {
            let n = self.read_note_at(p)?; (n.uuid == uuid).then_some(n.id)
        }).collect())
    }
    async fn list_trashed(&self) -> Result<Vec<TrashedNote>, TransportError> {
        let dir = self.trash_dir();
        if !dir.exists() { return Ok(vec![]); }
        Ok(walkdir::WalkDir::new(&dir).max_depth(1).into_iter().filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && e.path().extension().map(|x| x == "eml").unwrap_or(false))
            .filter_map(|e| { let bytes = std::fs::read(e.path()).ok()?; let n = super::decode::decode_eml(&bytes, "Notes").ok()?;
                Some(TrashedNote { id: e.path().strip_prefix(&self.root).ok()?.to_string_lossy().replace('\\',"/"), uuid: n.uuid, title: n.title, date: n.date, label: "Notes".into() }) })
            .collect())
    }
    async fn untrash(&self, remote_id: &str) -> Result<(), TransportError> {
        let src = self.root.join(remote_id);
        let fname = src.file_name().ok_or(TransportError::NotFound)?.to_owned();
        std::fs::create_dir_all(self.notes_dir()).map_err(perm)?;
        std::fs::rename(&src, self.notes_dir().join(fname)).map_err(perm)
    }
}
```
  (Verify `Note`/`Attachment`/`SavedNote`/`TrashedNote`/`MessageIndex` field names against `backend/mod.rs` from A1 and adjust.)
- [ ] **Step 3:** `cargo build 2>&1 | tail -30` → fix until clean. `cargo test 2>&1 | tail -5` → 77 passed.
- [ ] **Step 4:** Commit:
```bash
git add src/backend/localfs/transport.rs && git commit -m "feat(localfs): filesystem Transport + NoteStore impls"
```

### Task B4: LocalFS `MetadataSidecar` + wire up `Vertical`

**Files:**
- Modify: `src-tauri/src/backend/localfs/transport.rs`, `src-tauri/src/backend/localfs/mod.rs`

- [ ] **Step 1:** Implement `MetadataSidecar` for `LocalFsVertical` (files under `.meta/`):
```rust
use crate::backend::{MetadataSidecar, SidecarKind, SidecarRecord};

impl LocalFsVertical {
    fn pin_path(&self, uuid: &str) -> std::path::PathBuf { self.meta_dir().join(format!("{}.pin", uuid)) }
    fn tags_path(&self, uuid: &str) -> std::path::PathBuf { self.meta_dir().join(format!("{}.tags.json", uuid)) }
}

#[async_trait]
impl MetadataSidecar for LocalFsVertical {
    async fn list_sidecars(&self, kind: SidecarKind) -> Result<Vec<SidecarRecord>, TransportError> {
        let dir = self.meta_dir();
        if !dir.exists() { return Ok(vec![]); }
        let ext = match kind { SidecarKind::Pin => ".pin", SidecarKind::Tags => ".tags.json" };
        let mut out = vec![];
        for e in walkdir::WalkDir::new(&dir).max_depth(1).into_iter().filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(uuid) = name.strip_suffix(ext) {
                let body = match kind { SidecarKind::Tags => std::fs::read(e.path()).ok(), SidecarKind::Pin => None };
                out.push(SidecarRecord { id: e.path().strip_prefix(&self.root).map(|p| p.to_string_lossy().replace('\\',"/")).unwrap_or_default(), note_uuid: uuid.to_string(), kind, body });
            }
        }
        Ok(out)
    }
    async fn put_sidecar(&self, note_uuid: &str, kind: SidecarKind, body: Option<&[u8]>, _replace: Option<&str>) -> Result<String, TransportError> {
        std::fs::create_dir_all(self.meta_dir()).map_err(perm)?;
        let path = match kind { SidecarKind::Pin => self.pin_path(note_uuid), SidecarKind::Tags => self.tags_path(note_uuid) };
        std::fs::write(&path, body.unwrap_or(b"")).map_err(perm)?;
        Ok(path.strip_prefix(&self.root).map(|p| p.to_string_lossy().replace('\\',"/")).unwrap_or_default())
    }
    async fn remove_sidecar(&self, id: &str) -> Result<(), TransportError> {
        let p = self.root.join(id);
        if p.exists() { std::fs::remove_file(p).map_err(perm)?; }
        Ok(())
    }
}
```
- [ ] **Step 2:** In `localfs/mod.rs`, ensure `impl Vertical for LocalFsVertical` is present/uncommented (from B1). Now all super-trait bounds are satisfied.
- [ ] **Step 3:** `cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -5` → 77 passed.
- [ ] **Step 4:** Commit:
```bash
git add src/backend/localfs/ && git commit -m "feat(localfs): MetadataSidecar (.meta files); complete Vertical impl"
```

### Task B5: tempdir integration example

**Files:**
- Create: `src-tauri/examples/roundtrip_localfs.rs`

- [ ] **Step 1:** Write the example driving `LocalFsVertical` end-to-end on a tempdir (no account, no network). Mirror `roundtrip_refactor.rs`'s check structure:
```rust
// Live round-trip for the LocalFS vertical on a throwaway temp directory.
//   cargo run --example roundtrip_localfs
use std::collections::HashMap;
use jodd_lib::backend::localfs::LocalFsVertical;
use jodd_lib::backend::{NoteStore, MetadataSidecar, Transport, SaveOp, SidecarKind};

#[tokio::main]
async fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("jodd_localfs_rt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let v = LocalFsVertical::new(dir.clone(), "local-test".into());
    let mut fails = vec![];
    let chk = |c: bool, m: &str, f: &mut Vec<String>| { if c { eprintln!("[rt] PASS: {}", m) } else { eprintln!("[rt] FAIL: {}", m); f.push(m.into()) } };

    v.ensure_folder("Notes/play5").await.map_err(|e| e.to_string())?;
    let op = SaveOp { title: "LFS title", body_html: "<div>body #t [[L-abcd1234]]</div>",
        existing_remote_id: None, existing_uuid: None, existing_created_date: None, label: "Notes/play5" };
    let saved = v.save_note_full(&op, &[]).await.map_err(|e| e.to_string())?;
    chk(dir.join(&saved.id).exists(), "eml file written to disk", &mut fails);

    let (notes, _) = v.list_all_notes(&HashMap::new()).await.map_err(|e| e.to_string())?;
    let mine = notes.iter().find(|n| n.uuid == saved.uuid);
    chk(mine.is_some(), "note found via list_all_notes", &mut fails);
    if let Some(n) = mine {
        chk(n.title == "LFS title", "title round-trips", &mut fails);
        chk(n.label == "Notes/play5", "label = folder path", &mut fails);
        chk(n.body_html.contains("#t") && n.body_html.contains("[[L-abcd1234]]"), "tag+link retained", &mut fails);
        chk(!n.body_html.contains("<div>LFS title</div>"), "title row stripped", &mut fails);
    }

    let op2 = SaveOp { title: "LFS title", body_html: "<div>EDITED #t</div>",
        existing_remote_id: Some(&saved.id), existing_uuid: Some(&saved.uuid), existing_created_date: None, label: "Notes/play5" };
    let saved2 = v.save_note_full(&op2, &[]).await.map_err(|e| e.to_string())?;
    chk(saved2.id == saved.id, "edit keeps stable remote_id (overwrite in place)", &mut fails);
    let (notes2, _) = v.list_all_notes(&HashMap::new()).await.map_err(|e| e.to_string())?;
    chk(notes2.iter().filter(|n| n.uuid == saved.uuid).count() == 1, "exactly one copy after edit", &mut fails);

    v.put_sidecar(&saved.uuid, SidecarKind::Pin, None, None).await.map_err(|e| e.to_string())?;
    chk(dir.join(".meta").join(format!("{}.pin", saved.uuid)).exists(), "pin sidecar file created", &mut fails);
    chk(v.list_sidecars(SidecarKind::Pin).await.map_err(|e| e.to_string())?.iter().any(|s| s.note_uuid == saved.uuid), "pin sidecar listed", &mut fails);

    v.move_note(&saved2.id, &["Notes".to_string()], &["Notes/play5".to_string()]).await.map_err(|e| e.to_string())?;
    let (notes3, _) = v.list_all_notes(&HashMap::new()).await.map_err(|e| e.to_string())?;
    chk(notes3.iter().find(|n| n.uuid == saved.uuid).map(|n| n.label == "Notes").unwrap_or(false), "move_note relabels to Notes", &mut fails);

    let moved_id = notes3.iter().find(|n| n.uuid == saved.uuid).unwrap().id.clone();
    v.delete(&moved_id).await.map_err(|e| e.to_string())?;
    chk(v.list_trashed().await.map_err(|e| e.to_string())?.iter().any(|t| t.uuid == saved.uuid), "delete moves to .trash", &mut fails);

    let _ = std::fs::remove_dir_all(&dir);
    if fails.is_empty() { eprintln!("[rt] ✅ ALL PASSED"); Ok(()) }
    else { eprintln!("[rt] ❌ {} failed", fails.len()); Err(format!("{} checks failed", fails.len())) }
}
```
  Ensure `LocalFsVertical`, `localfs` module, and the traits are `pub` enough to import as `jodd_lib::backend::localfs::LocalFsVertical`. Add `pub use` if needed.
- [ ] **Step 2:** Run: `cargo run --example roundtrip_localfs 2>&1 | grep -E "PASS|FAIL|✅|❌"` → all PASS.
- [ ] **Step 3:** Commit:
```bash
git add src/backend/ examples/roundtrip_localfs.rs && git commit -m "test(localfs): tempdir round-trip example (no network)"
```

---

## Phase C — Account model + frontend

### Task C1: Generalize `Account` + readiness

**Files:**
- Modify: `src-tauri/src/accounts.rs`

- [ ] **Step 1:** Add `BackendKind` and fields:
```rust
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind { #[default] Gmail, LocalFs }
```
In `struct Account`, add:
```rust
    #[serde(default)] pub backend_kind: BackendKind,
    #[serde(default)] pub root_dir: Option<String>,
```
`#[serde(default)]` keeps existing `accounts.json` valid (old → `Gmail`, `root_dir=None`).
- [ ] **Step 2:** Add a readiness helper:
```rust
impl Account {
    /// Local readiness — never touches the network/keychain.
    pub fn is_ready_local(&self) -> bool {
        match self.backend_kind {
            BackendKind::Gmail => crate::accounts::load_refresh_token(&self.id).is_some(),
            BackendKind::LocalFs => self.root_dir.as_ref().map(|d| std::path::Path::new(d).is_dir()).unwrap_or(false),
        }
    }
}
```
  Wire this into wherever `is_authenticated`/account-usable is computed (search for the existing readiness check from the offline-cold-start fix and add the LocalFs branch). Gmail behavior unchanged.
- [ ] **Step 3:** `cargo test 2>&1 | tail -5` → 77 passed. Commit:
```bash
git add src/accounts.rs src/lib.rs && git commit -m "feat(accounts): BackendKind + root_dir + local readiness branch"
```

### Task C2: `vertical_for` LocalFs branch + `add_local_account`

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1:** Extend `vertical_for` (from A4) with the LocalFs branch:
```rust
async fn vertical_for(state: &State<'_, AppState>, account_id: &str) -> Result<Box<dyn Vertical>, String> {
    let kind = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == account_id).map(|a| (a.backend_kind, a.root_dir.clone(), a.effective_meta_label().to_string()))
            .ok_or_else(|| format!("account {} not found", account_id))?
    };
    match kind.0 {
        accounts::BackendKind::LocalFs => {
            let root = kind.1.ok_or("local account missing root_dir")?;
            Ok(Box::new(backend::localfs::LocalFsVertical::new(std::path::PathBuf::from(root), account_id.to_string())))
        }
        accounts::BackendKind::Gmail => {
            let token = ensure_token(state, account_id).await?;
            let label_map = cached_label_map(state, account_id, &token).await?;
            Ok(Box::new(backend::gmail::GmailVertical::new(token, label_map, account_id.to_string(), kind.2)))
        }
    }
}
```
- [ ] **Step 2:** Add the Tauri command:
```rust
#[tauri::command]
async fn add_local_account(state: State<'_, AppState>, path: String) -> Result<accounts::Account, String> {
    let p = std::path::Path::new(&path);
    if !p.is_dir() { return Err(format!("not a directory: {}", path)); }
    let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
    let id = format!("localfs:{}", crate::mime822::format_apple_uuid(uuid::Uuid::new_v4()));
    let account = accounts::Account {
        id: id.clone(), email: name, backend_kind: accounts::BackendKind::LocalFs,
        root_dir: Some(path.clone()), notes_label: None, meta_label: None,
        // ...fill remaining Account fields with their defaults (check the struct)...
    };
    { let mut list = state.accounts.lock().unwrap(); list.push(account.clone()); accounts::save_accounts(&list)?; }
    // Cold-start index this account so its folders/notes populate the cache.
    index_account(&state, &id).await?;   // reuse the existing per-account index entrypoint
    Ok(account)
}
```
  (Match `Account`'s exact fields. `index_account` is the existing cold-start indexer — confirm its signature and that it now works through `vertical_for` so it indexes a LocalFs account.)
- [ ] **Step 3:** Register `add_local_account` in the `tauri::generate_handler![...]` list.
- [ ] **Step 4:** `cargo test 2>&1 | tail -5` → 77 passed; `cargo build 2>&1 | tail -10` clean. Commit:
```bash
git add src/lib.rs && git commit -m "feat(localfs): vertical_for LocalFs branch + add_local_account command"
```

### Task C3: `tauri-plugin-dialog` + frontend "Add Local Folder"

**Files:**
- Modify: `src-tauri/Cargo.toml`, `src-tauri/src/lib.rs` (or `main.rs`), `src-tauri/capabilities/*.json`, `src/lib/components/AuthScreen.svelte` (or `Sidebar.svelte`), `src/lib/stores/notes.ts`

- [ ] **Step 1:** Add the dialog plugin. `Cargo.toml`: `tauri-plugin-dialog = "2"`. Register in the Tauri builder (where other plugins like `tauri-plugin-shell` are registered): `.plugin(tauri_plugin_dialog::init())`. Add the dialog permission to the capabilities JSON (e.g. `"dialog:allow-open"`). Install the JS side: `npm i @tauri-apps/plugin-dialog`.
- [ ] **Step 2:** Add an "Add Local Folder" button near the existing "Add Account" entry (in `AuthScreen.svelte` and/or the account area of `Sidebar.svelte`). On click:
```ts
import { open } from '@tauri-apps/plugin-dialog';
async function addLocalFolder() {
  const dir = await open({ directory: true, multiple: false, title: 'Choose a notes folder' });
  if (typeof dir !== 'string') return;
  const account = await invoke('add_local_account', { path: dir });
  // refresh accounts/sidebar the same way add-account does (push to accounts store, select it)
}
```
  Wire the returned account into the existing `accounts` store + trigger the same post-add UI refresh the OAuth add-account flow uses.
- [ ] **Step 3:** Show a 📁 marker for `backend_kind === 'local_fs'` accounts in the sidebar account row (the `Account` type in `types.ts` gains `backend_kind?: string`).
- [ ] **Step 4:** Build the frontend + app: `npm run tauri build 2>&1 | tail -20` (or `tauri dev` for a quick check) — confirms the plugin + UI compile and the bundle layout check (one binary in `Contents/MacOS/`) still passes.
- [ ] **Step 5:** Commit:
```bash
git add -A && git commit -m "feat(localfs): Add Local Folder UI (dialog plugin) + sidebar marker"
```

### Task C4: Cross-vertical verification gate

**Files:** none (manual + example)

- [ ] **Step 1:** `cargo test 2>&1 | tail -5` → 77 passed. `cargo run --example roundtrip_localfs` → all PASS. `cargo run --example roundtrip_refactor` → all PASS (Gmail still works through dyn dispatch).
- [ ] **Step 2:** Live: run the app, "Add Local Folder" → pick an empty test dir, create a note, confirm a `<uuid>.eml` appears on disk and the note re-opens with correct title/body. Pin it → `.meta/<uuid>.pin` appears. Delete → file moves to `.trash/`.
- [ ] **Step 3:** Cross-vertical: with the existing Gmail account + the new local account both present, run a search (FTS) for a term in both → confirm results span both accounts (the neutral index is backend-agnostic).
- [ ] **Step 4:** Commit a verification note:
```bash
git commit --allow-empty -m "test: verify LocalFS vertical — round-trips, coexists with Gmail, index spans both"
```

---

## Self-Review

**Spec coverage:**
- Trait-per-vertical orchestration (dedup stays Gmail-internal) → A2/A3 (`NoteStore`, Gmail dedup in `wire::list_notes`) ✓
- `Box<dyn Vertical>` dynamic dispatch → A2 (super-trait) + A4/C2 (`vertical_for`) ✓
- Neutral types in core → A1 ✓
- LocalFS `.eml`/folder=dir/`.trash`/`.meta` storage → B3/B4 ✓
- raw-RFC822 decode via `mail-parser` + symmetry test → B2 ✓
- Reuse mime822 encode + AppleHtmlDeriver + Identity → B1/B3 ✓
- changes_since full-scan + core prune → B3 (inert changes_since; list_all_notes driver) ✓
- Account generalization + readiness (no network) → C1 ✓
- Full UI add (dialog) + minted id → C2/C3 ✓
- Verification: existing tests green, tempdir example, cross-vertical → A5/B5/C4 ✓
- Deferred items (markdown, mtime cursor, file-watch, external .eml import, sync_cursor column) → not built ✓

**Placeholder scan:** Code steps include real code. Two spots intentionally say "match the actual field names from A1 / the `Account` struct" (B2/B3/C2) and "the mail-parser 0.9 API may need minor tweaks" (B2) — these are *verify-against-real-symbols* instructions with a concrete test to confirm correctness (the symmetry test, `cargo build`), not vague TODOs. The frontend wiring (C3) references existing add-account refresh flow rather than reproducing it — acceptable since it's "mirror the existing pattern in this file."

**Type consistency:** `NoteStore` method names are used identically in A2 (definition), A3 (Gmail impl), A4 (call sites), B3 (LocalFS impl), B5 (example). `SidecarRecord { id, note_uuid, kind, body }` consistent across A3/B4/C-pull. `SaveOp`/`SavedNote`/`Note`/`Attachment` names consistent (relocated in A1, used everywhere after). `vertical_for` signature consistent A4 → C2 (extended, same shape). `BackendKind { Gmail, LocalFs }` consistent C1/C2.

**Known soft spots flagged for the implementer:** B2 (`mail-parser` API specifics — the symmetry test is the arbiter), A3/A4 (sidecar unification touches the working pin/tag path — the `roundtrip_refactor` live check in A5 guards it), C2/C3 (match exact `Account` fields + existing add-account UI flow).
