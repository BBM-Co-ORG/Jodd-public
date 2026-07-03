# Lesson Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a paste-and-extract LLM workflow that turns mixed unformatted source text into a structured Lessons note in a Jodd-managed system workflow folder.

**Architecture:** A new `lessons` Rust module behind a `LessonProvider` trait with two impls (HTTP for any OpenAI-compatible endpoint; subprocess for `claude -p`). The provider returns a typed `ExtractEnvelope` with markdown-bodied lessons + tags + title. Backend converts markdown → HTML via `pulldown-cmark`, assembles a note with collapsible `## Source` section, and writes it to a `system_workflow`-kind folder (new column on `folders` table, migration #9 — note: original plan said #5, but migrations 5-8 already exist on disk for the tag feature; corrected during Task 1 execution). Frontend gets a `LessonExtractModal.svelte`, an `LlmProviderSettings.svelte` sub-component for the existing AccountSettings modal, and a sidebar grouping split (Folders / Workflows).

**Tech Stack:** Rust 2021 / Tauri 2 / SQLite / Svelte 5 / TypeScript / Vite 6. Adds `pulldown-cmark` (MD→HTML), `async-trait` (provider trait), `mockito` dev-dep (HTTP mock for tests). No frontend test infra added.

**Reference spec:** [docs/superpowers/specs/2026-06-13-lesson-extraction-design.md](../specs/2026-06-13-lesson-extraction-design.md)

---

## Phase A — Foundation: schema + folder kind

`★ Insight ─────────────────────────────────────`
The new `kind` column on `folders` is the entire data-model delta for system workflow folders. Picking the column name carefully now (general `kind TEXT` not boolean `is_workflow`) lets the future smart-folders feature land as a third value without revisiting the schema.
`─────────────────────────────────────────────────`

### Task 1: Add migration #5 — `folders.kind` column

**Files:**
- Modify: `src-tauri/src/db.rs` (find the migration runner; add a new migration block)

- [ ] **Step 1: Locate the migration runner**

Run: `grep -n "migration\|user_version\|PRAGMA" src-tauri/src/db.rs | head -20`

Expected: identify the function that applies migrations sequentially (look for matches against `PRAGMA user_version`).

- [ ] **Step 2: Add migration #5**

Open `src-tauri/src/db.rs`. Find the migration runner — it's a `match` or `if` chain keyed off `user_version`. Migration #4 (`meta_msg_id` + `pin_dirty`) is the last one. Add #5 immediately after:

```rust
// Migration 5 — folder kind for system workflow folders (Lessons, etc.)
// 'user' = user-created folder (default for all existing rows)
// 'system_workflow' = Jodd auto-created folder for workflow output
// 'smart_query' reserved for future smart-folder feature
if user_version < 5 {
    conn.execute_batch(
        "ALTER TABLE folders ADD COLUMN kind TEXT NOT NULL DEFAULT 'user';
         PRAGMA user_version = 5;",
    )?;
    log!("db: migration 5 applied — folders.kind column added");
}
```

- [ ] **Step 3: Build to verify migration compiles**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: clean build, no errors.

- [ ] **Step 4: Smoke-test the migration**

Run `npm run tauri dev` once. Watch the log for `db: migration 5 applied`. After the app starts cleanly, kill it and verify the column exists:

```bash
sqlite3 ~/Library/Application\ Support/jodd/jodd.sqlite3 "SELECT name, type FROM pragma_table_info('folders');"
```

Expected: output includes `kind|TEXT`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/db.rs
git commit -m "db: migration 5 — folders.kind for system workflow folders

Adds kind TEXT NOT NULL DEFAULT 'user' to the folders table. 'user' (the
default for all existing rows) is the regular user-managed folder. New
value 'system_workflow' will tag Jodd auto-created folders for workflow
output (Lessons, future Summaries, etc.). 'smart_query' reserved for
the planned smart-folders feature."
```

### Task 2: Extend `Folder` struct + folder list path with `kind`

**Files:**
- Modify: `src-tauri/src/db.rs` — `Folder` struct and `list_folders` / `get_folder`
- Modify: `src-tauri/src/lib.rs` — wherever the folder is constructed (e.g. `upsert_folder_from_remote`, `insert_folder_local_new`)

- [ ] **Step 1: Find the Folder struct**

Run: `grep -n "struct Folder\|pub struct Folder" src-tauri/src/db.rs`

- [ ] **Step 2: Add `kind` field**

In `db.rs`, find the `Folder` struct. Add a `kind` field. Example (your existing struct may have more fields — preserve them):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub account_id: String,
    pub path: String,
    pub label_id: Option<String>,
    pub sync_state: String,
    pub last_local_modified_at: Option<i64>,
    pub last_synced_at: Option<i64>,
    pub kind: String,  // NEW: 'user' | 'system_workflow' | 'smart_query'
}
```

- [ ] **Step 3: Update `list_folders` SELECT and row mapping**

Find `pub fn list_folders` in `db.rs`. Add `kind` to the SELECT and to the row construction. Example:

```rust
pub fn list_folders(&self, account_id: &str) -> Result<Vec<Folder>> {
    let conn = self.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT account_id, path, label_id, sync_state,
                last_local_modified_at, last_synced_at, kind
         FROM folders
         WHERE account_id = ?1
         ORDER BY path",
    )?;
    let rows = stmt.query_map([account_id], |r| {
        Ok(Folder {
            account_id: r.get(0)?,
            path: r.get(1)?,
            label_id: r.get(2)?,
            sync_state: r.get(3)?,
            last_local_modified_at: r.get(4)?,
            last_synced_at: r.get(5)?,
            kind: r.get(6)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}
```

Apply the same pattern to `get_folder` (SELECT and mapping).

- [ ] **Step 4: Update `insert_folder_local_new` to default to `'user'`**

Find `insert_folder_local_new`. The default `kind` for normally-user-created folders is `'user'`. Update the INSERT to include `kind`:

```rust
conn.execute(
    "INSERT OR IGNORE INTO folders
         (account_id, path, label_id, sync_state, last_local_modified_at, kind)
     VALUES (?1, ?2, NULL, 'dirty_new', ?3, 'user')",
    params![account_id, path, now],
)?;
```

- [ ] **Step 5: Update `upsert_folder_from_remote` to preserve existing `kind`**

Find `upsert_folder_from_remote`. Remote-side updates must NOT reset `kind` — a folder created locally as `system_workflow` and then round-tripped through Gmail must stay `system_workflow`. Use SQLite's `ON CONFLICT ... DO UPDATE` keeping `kind` unchanged on the conflict path:

```rust
conn.execute(
    "INSERT INTO folders
         (account_id, path, label_id, sync_state, last_synced_at, kind)
     VALUES (?1, ?2, ?3, 'clean', ?4, 'user')
     ON CONFLICT(account_id, path) DO UPDATE SET
         label_id = excluded.label_id,
         sync_state = 'clean',
         last_synced_at = excluded.last_synced_at",
    params![account_id, path, label_id, now],
)?;
```

(Note: the `INSERT` clause specifies `'user'` only for genuinely-new rows; `ON CONFLICT` does NOT touch `kind`, so existing system_workflow rows are preserved.)

- [ ] **Step 6: Build & verify**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: clean build.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/db.rs
git commit -m "db: Folder struct carries kind column

list_folders/get_folder now return kind. insert_folder_local_new
defaults to 'user' (matching existing behavior). upsert_folder_from_remote
preserves existing kind on conflict — a folder created locally as
system_workflow and round-tripped through Gmail stays system_workflow."
```

### Task 3: `ensure_workflow_folder` helper

**Files:**
- Modify: `src-tauri/src/db.rs` — new method on the DB handle

- [ ] **Step 1: Write the failing test**

Append to `src-tauri/src/db.rs` (or wherever the test module lives — if none exists, add at end of file):

```rust
#[cfg(test)]
mod tests_workflow_folder {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Db {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.sqlite3");
        // Leak the dir so the path stays valid for the test's lifetime
        std::mem::forget(dir);
        Db::open(&path).expect("open temp db")
    }

    #[test]
    fn ensure_workflow_folder_creates_when_absent() {
        let db = temp_db();
        let acct = "test@example.com";
        let path = db
            .ensure_workflow_folder(acct, "Lessons")
            .expect("ensure");
        assert_eq!(path, "Notes/Lessons");

        let folders = db.list_folders(acct).expect("list");
        let lessons = folders
            .iter()
            .find(|f| f.path == "Notes/Lessons")
            .expect("Notes/Lessons exists");
        assert_eq!(lessons.kind, "system_workflow");
        assert_eq!(lessons.sync_state, "dirty_new");
    }

    #[test]
    fn ensure_workflow_folder_idempotent() {
        let db = temp_db();
        let acct = "test@example.com";
        db.ensure_workflow_folder(acct, "Lessons").unwrap();
        let path = db.ensure_workflow_folder(acct, "Lessons").unwrap();
        assert_eq!(path, "Notes/Lessons");
        let count = db
            .list_folders(acct)
            .unwrap()
            .iter()
            .filter(|f| f.path == "Notes/Lessons")
            .count();
        assert_eq!(count, 1, "should not duplicate");
    }
}
```

- [ ] **Step 2: Add `tempfile` dev-dep**

In `src-tauri/Cargo.toml`, find `[dev-dependencies]` (add if missing):

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Run tests — expect FAIL**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tests_workflow_folder`
Expected: compile error — `ensure_workflow_folder` not yet defined.

- [ ] **Step 4: Implement `ensure_workflow_folder`**

In `db.rs`, add the method on the `Db` impl block:

```rust
/// Creates a system workflow folder under Notes/ if absent.
/// `name` is the workflow name (e.g. "Lessons" → "Notes/Lessons").
/// Returns the full label path. Idempotent.
pub fn ensure_workflow_folder(
    &self,
    account_id: &str,
    name: &str,
) -> Result<String> {
    let path = format!("Notes/{}", name);
    let now = chrono::Utc::now().timestamp_millis();
    let conn = self.conn.lock().unwrap();

    // ensure_ancestors creates "Notes" if missing — same path used by
    // insert_folder_local_new for user folders.
    Self::ensure_ancestors(&conn, account_id, &path, now)?;

    conn.execute(
        "INSERT OR IGNORE INTO folders
             (account_id, path, label_id, sync_state, last_local_modified_at, kind)
         VALUES (?1, ?2, NULL, 'dirty_new', ?3, 'system_workflow')",
        params![account_id, path, now],
    )?;
    Ok(path)
}
```

(If `ensure_ancestors` is a private free function rather than an associated function, drop the `Self::` qualifier.)

- [ ] **Step 5: Run tests — expect PASS**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tests_workflow_folder`
Expected: 2 passed.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/db.rs src-tauri/Cargo.toml
git commit -m "db: ensure_workflow_folder helper + unit tests

Idempotent insert of a system_workflow folder under Notes/. Goes through
the existing ensure_ancestors path so the implicit Notes root is created
if absent. Unit tests cover the create-when-absent and idempotent cases."
```

---

## Phase B — LLM provider abstraction

`★ Insight ─────────────────────────────────────`
The provider trait is the only seam between Jodd's note-assembly pipeline and the LLM. By making providers return a typed `ExtractEnvelope` (not a string), the downstream code never knows whether the bytes came from HTTPS or a subprocess. The next provider (Gemini, Mistral, an internal model) is just another impl of the same trait — zero refactor.
`─────────────────────────────────────────────────`

### Task 4: New dependencies + module skeleton

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Create: `src-tauri/src/lessons/mod.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod lessons;`)

- [ ] **Step 1: Add deps**

In `src-tauri/Cargo.toml` under `[dependencies]`:

```toml
pulldown-cmark = { version = "0.10", default-features = false, features = ["html"] }
async-trait = "0.1"
which = "6"  # for resolving `claude` binary path at startup
```

Under `[dev-dependencies]`:

```toml
mockito = "1"
```

- [ ] **Step 2: Create module skeleton**

Create `src-tauri/src/lessons/mod.rs`:

```rust
//! Lesson extraction — paste arbitrary source text, LLM returns structured
//! lessons, Jodd files them in a system workflow folder.
//!
//! See docs/superpowers/specs/2026-06-13-lesson-extraction-design.md

pub mod claude_code;
pub mod http;
pub mod markdown;
pub mod prompt;
pub mod provider;
```

- [ ] **Step 3: Register module in lib.rs**

In `src-tauri/src/lib.rs`, find the existing `mod` declarations (look for `mod accounts;`, `mod gmail;`, etc.) and add:

```rust
mod lessons;
```

- [ ] **Step 4: Create stub files so the module compiles**

Create empty stubs so `cargo check` is green after this task:

- `src-tauri/src/lessons/provider.rs`:
```rust
// Provider trait + envelope types — implemented in Task 5
```
- `src-tauri/src/lessons/http.rs`:
```rust
// HttpProvider — implemented in Task 8
```
- `src-tauri/src/lessons/claude_code.rs`:
```rust
// ClaudeCodeProvider — implemented in Task 9
```
- `src-tauri/src/lessons/markdown.rs`:
```rust
// Markdown → HTML + note body assembly — implemented in Task 6
```
- `src-tauri/src/lessons/prompt.rs`:
```rust
// System prompt — implemented in Task 7
```

- [ ] **Step 5: Build & verify**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: clean build.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/lessons/ src-tauri/src/lib.rs
git commit -m "lessons: module skeleton + deps

Adds pulldown-cmark (markdown → html), async-trait (provider trait),
which (claude binary detection), mockito (dev-dep, HTTP tests). Creates
the lessons/ module hierarchy with stub files for provider, http,
claude_code, markdown, and prompt — each implemented in subsequent tasks."
```

### Task 5: Provider trait + `ExtractEnvelope` / `ExtractError`

**Files:**
- Modify: `src-tauri/src/lessons/provider.rs`

- [ ] **Step 1: Write failing tests**

Replace the stub in `provider.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("provider not configured: {0}")]
    NotConfigured(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("malformed envelope: {reason}")]
    MalformedEnvelope { reason: String, raw: String },
    #[error("upstream error: {0}")]
    UpstreamError(String),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ExtractEnvelope {
    pub title: Option<String>,
    pub lessons_markdown: String,
    #[serde(default)]
    pub meta_lessons_markdown: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub confidence: Option<String>,
}

#[async_trait::async_trait]
pub trait LessonProvider: Send + Sync {
    async fn extract(&self, source: &str) -> Result<ExtractEnvelope, ExtractError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_parses_full_response() {
        let json = r#"{
            "title": "Test lesson",
            "lessons_markdown": "## Lesson 1\nbody",
            "meta_lessons_markdown": "## Meta\nbody",
            "tags": ["tag-a", "tag-b"],
            "confidence": "high"
        }"#;
        let env: ExtractEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.title.as_deref(), Some("Test lesson"));
        assert_eq!(env.tags.len(), 2);
    }

    #[test]
    fn envelope_parses_minimal_response() {
        // Optional fields all missing — only lessons_markdown required.
        let json = r#"{ "lessons_markdown": "## L1\nbody" }"#;
        let env: ExtractEnvelope = serde_json::from_str(json).unwrap();
        assert!(env.title.is_none());
        assert!(env.tags.is_empty());
        assert!(env.meta_lessons_markdown.is_none());
    }
}
```

- [ ] **Step 2: Add `thiserror` dep if missing**

Run: `grep "^thiserror" src-tauri/Cargo.toml`. If empty, add to `[dependencies]`:

```toml
thiserror = "1"
```

- [ ] **Step 3: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml lessons::provider`
Expected: 2 passed.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lessons/provider.rs src-tauri/Cargo.toml
git commit -m "lessons: LessonProvider trait + ExtractEnvelope/ExtractError

Provider returns a typed envelope, not a string. Optional fields default
to None/empty so providers can omit non-essential metadata without
breaking deserialization. Errors are typed by failure mode (configured /
transport / malformed / upstream / cancelled) so the UI can react
appropriately to each."
```

### Task 6: Markdown → HTML + note body assembly

**Files:**
- Modify: `src-tauri/src/lessons/markdown.rs`

- [ ] **Step 1: Write failing tests**

Replace the stub in `markdown.rs`:

```rust
//! Markdown → HTML conversion + Lessons note body assembly.

use pulldown_cmark::{html, Parser};

use crate::lessons::provider::ExtractEnvelope;

/// Convert a markdown string to HTML. Pure function, no escaping issues —
/// pulldown-cmark handles all the markdown-specific encoding.
pub fn md_to_html(md: &str) -> String {
    let parser = Parser::new(md);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// HTML-escape arbitrary text for safe inclusion in HTML.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Assemble the final note body from an envelope + raw source text.
pub fn assemble_note_body(envelope: &ExtractEnvelope, source: &str) -> String {
    let mut body = String::with_capacity(envelope.lessons_markdown.len() + source.len() + 512);

    // Tags line at the top, picked up by Jodd's existing #hashtag parser
    if !envelope.tags.is_empty() {
        body.push_str("<p>");
        for (i, tag) in envelope.tags.iter().enumerate() {
            if i > 0 {
                body.push(' ');
            }
            body.push('#');
            // Strip any leading # the LLM may have added; sanitize whitespace
            let clean = tag.trim_start_matches('#').replace(char::is_whitespace, "-");
            body.push_str(&escape_html(&clean));
        }
        body.push_str("</p>\n");
    }

    // Main lessons content
    body.push_str(&md_to_html(&envelope.lessons_markdown));

    // Optional meta-lessons section
    if let Some(meta) = &envelope.meta_lessons_markdown {
        if !meta.trim().is_empty() {
            body.push_str(&md_to_html(meta));
        }
    }

    // Collapsible source section — pure HTML, source verbatim in <pre>
    body.push_str("<hr>\n<details>\n<summary>Source (verbatim)</summary>\n<pre>");
    body.push_str(&escape_html(source));
    body.push_str("</pre>\n</details>\n");

    body
}

/// Regex match for whether a note body contains a preserved Source block.
/// Used to decide whether the "Re-extract lessons" right-click menu item
/// applies to a given note.
pub fn has_source_block(body_html: &str) -> bool {
    body_html.contains("<summary>Source (verbatim)</summary>")
}

/// Extract the raw source text from a note body that has a Source block.
/// Returns None if no block found or the structure is malformed.
pub fn extract_source(body_html: &str) -> Option<String> {
    let marker = "<summary>Source (verbatim)</summary>";
    let after_marker = body_html.split_once(marker)?.1;
    let pre_open = after_marker.find("<pre>")?;
    let after_pre = &after_marker[pre_open + "<pre>".len()..];
    let pre_close = after_pre.find("</pre>")?;
    let raw = &after_pre[..pre_close];
    // Unescape the four entities we inject
    Some(
        raw.replace("&quot;", "\"")
            .replace("&gt;", ">")
            .replace("&lt;", "<")
            .replace("&amp;", "&"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md_to_html_handles_basic_markdown() {
        let html = md_to_html("## H2\n\nparagraph **bold**");
        assert!(html.contains("<h2>H2</h2>"));
        assert!(html.contains("<strong>bold</strong>"));
    }

    #[test]
    fn escape_html_escapes_all_four() {
        assert_eq!(
            escape_html("a&b<c>d\"e"),
            "a&amp;b&lt;c&gt;d&quot;e"
        );
    }

    #[test]
    fn assemble_includes_all_sections() {
        let env = ExtractEnvelope {
            title: Some("T".into()),
            lessons_markdown: "## Lesson 1\nbody".into(),
            meta_lessons_markdown: Some("## Meta\nm".into()),
            tags: vec!["tag-a".into(), "tag-b".into()],
            confidence: Some("high".into()),
        };
        let body = assemble_note_body(&env, "raw source text");
        assert!(body.contains("#tag-a #tag-b"), "tag line: {body}");
        assert!(body.contains("<h2>Lesson 1</h2>"));
        assert!(body.contains("<h2>Meta</h2>"));
        assert!(body.contains("<summary>Source (verbatim)</summary>"));
        assert!(body.contains("raw source text"));
    }

    #[test]
    fn assemble_omits_meta_when_absent_or_empty() {
        let env = ExtractEnvelope {
            title: None,
            lessons_markdown: "x".into(),
            meta_lessons_markdown: Some("   ".into()),
            tags: vec![],
            confidence: None,
        };
        let body = assemble_note_body(&env, "src");
        assert!(!body.contains("Meta"));
    }

    #[test]
    fn has_source_block_detects_marker() {
        let env = ExtractEnvelope {
            title: None,
            lessons_markdown: "x".into(),
            meta_lessons_markdown: None,
            tags: vec![],
            confidence: None,
        };
        let body = assemble_note_body(&env, "src");
        assert!(has_source_block(&body));
        assert!(!has_source_block("<p>plain note</p>"));
    }

    #[test]
    fn extract_source_roundtrips_special_chars() {
        let env = ExtractEnvelope {
            title: None,
            lessons_markdown: "x".into(),
            meta_lessons_markdown: None,
            tags: vec![],
            confidence: None,
        };
        let original = "code: <script>alert(\"hi & bye\")</script>";
        let body = assemble_note_body(&env, original);
        let recovered = extract_source(&body).unwrap();
        assert_eq!(recovered, original);
    }

    #[test]
    fn extract_source_returns_none_for_normal_note() {
        assert_eq!(extract_source("<p>just a note</p>"), None);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml lessons::markdown`
Expected: 6 passed.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/lessons/markdown.rs
git commit -m "lessons: markdown → HTML + note body assembly

md_to_html via pulldown-cmark. assemble_note_body produces the full
note shell: tag line, lessons HTML, optional meta-lessons, collapsible
<details>Source</details> block with verbatim source in <pre>.
has_source_block + extract_source support the Re-extract menu item by
roundtripping the preserved source. All pure functions, unit-tested."
```

### Task 7: System prompt constant

**Files:**
- Modify: `src-tauri/src/lessons/prompt.rs`

- [ ] **Step 1: Write the constant**

Replace the stub in `prompt.rs`:

```rust
//! System prompt for lesson extraction. v1 hardcoded; v2 will allow
//! per-account customization via account settings.

pub const SYSTEM_PROMPT: &str = r#"You are an expert lesson extractor for a personal knowledge management tool.

Given mixed unformatted text from a conversation, meeting transcript, debugging session, article, or other source, extract distinct lessons learned.

Output ONLY a JSON object with this exact shape:

{
  "title": "Lessons — <short topic summary, max 80 chars>",
  "lessons_markdown": "<full markdown body with ## H2 headings per lesson>",
  "meta_lessons_markdown": "<optional ## Meta-lessons section, or empty string>",
  "tags": ["lowercase-kebab", "no-spaces", "max-8-tags"],
  "confidence": "high|medium|low"
}

Rules for lessons_markdown:
- Each lesson is a ## H2 heading with format: "## Lesson N — <topic>"
- Use bullets, code blocks, and markdown links freely
- Preserve file:line references (e.g. [foo.rs:42](src/foo.rs:42)) if present in source
- Be specific; cite source content where possible
- 1-5 lessons usually; extract only genuinely distinct ones (don't pad)

Rules for tags:
- 2-8 tags, lowercase, kebab-case
- Prefer specific tags (e.g. "macos-keychain") over generic ones ("computers")
- Include domain tags ("debugging", "rust", "oauth") relevant to the source

Output the JSON object directly with no preamble, no markdown fence, no commentary."#;
```

- [ ] **Step 2: Build & verify**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/lessons/prompt.rs
git commit -m "lessons: v1 system prompt constant

Hardcoded JSON-envelope-instructing system prompt. v2 will allow
per-account customization; v1 keeps a single source of truth for the
output contract so both providers produce the same envelope shape."
```

### Task 8: `HttpProvider` impl

**Files:**
- Modify: `src-tauri/src/lessons/http.rs`

- [ ] **Step 1: Write failing tests**

Replace the stub in `http.rs`:

```rust
//! HTTP provider — any OpenAI-compatible chat-completions endpoint.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::lessons::prompt::SYSTEM_PROMPT;
use crate::lessons::provider::{ExtractEnvelope, ExtractError, LessonProvider};

pub struct HttpProvider {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatRequestMessage<'a>>,
    temperature: f32,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct ChatRequestMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    fmt_type: &'static str,
}

#[async_trait::async_trait]
impl LessonProvider for HttpProvider {
    async fn extract(&self, source: &str) -> Result<ExtractEnvelope, ExtractError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let req_body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatRequestMessage { role: "system", content: SYSTEM_PROMPT },
                ChatRequestMessage { role: "user", content: source },
            ],
            temperature: 0.2,
            response_format: ResponseFormat { fmt_type: "json_object" },
        };

        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| ExtractError::Transport(e.to_string()))?;

        let mut req = client.post(&url).json(&req_body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| ExtractError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ExtractError::UpstreamError(format!(
                "HTTP {status}: {body}"
            )));
        }

        let chat: ChatResponse = resp
            .json()
            .await
            .map_err(|e| ExtractError::MalformedEnvelope {
                reason: format!("chat envelope: {e}"),
                raw: String::new(),
            })?;

        let raw = chat
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ExtractError::MalformedEnvelope {
                reason: "no choices".into(),
                raw: String::new(),
            })?
            .message
            .content;

        serde_json::from_str::<ExtractEnvelope>(&raw).map_err(|e| {
            ExtractError::MalformedEnvelope {
                reason: format!("inner json: {e}"),
                raw,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn provider_for(url: &str) -> HttpProvider {
        HttpProvider {
            base_url: url.to_string(),
            model: "test-model".into(),
            api_key: Some("test-key".into()),
            timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn success_path_parses_envelope() {
        let mut server = Server::new_async().await;
        let inner = r#"{
            "title": "Test",
            "lessons_markdown": "## L1\nbody",
            "tags": ["a"]
        }"#;
        let body = format!(
            r#"{{"choices": [{{"message": {{"content": {}}}}}]}}"#,
            serde_json::to_string(inner).unwrap()
        );
        let _m = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let p = provider_for(&server.url());
        let env = p.extract("source").await.expect("ok");
        assert_eq!(env.title.as_deref(), Some("Test"));
        assert_eq!(env.tags, vec!["a"]);
    }

    #[tokio::test]
    async fn http_error_becomes_upstream_error() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/chat/completions")
            .with_status(429)
            .with_body(r#"{"error":"rate_limit"}"#)
            .create_async()
            .await;

        let p = provider_for(&server.url());
        let err = p.extract("source").await.expect_err("expected error");
        match err {
            ExtractError::UpstreamError(msg) => assert!(msg.contains("429")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_inner_json_becomes_malformed_envelope() {
        let mut server = Server::new_async().await;
        let body = r#"{"choices": [{"message": {"content": "not json"}}]}"#;
        let _m = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let p = provider_for(&server.url());
        let err = p.extract("source").await.expect_err("expected error");
        assert!(
            matches!(err, ExtractError::MalformedEnvelope { .. }),
            "got: {err:?}"
        );
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml lessons::http`
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/lessons/http.rs
git commit -m "lessons: HttpProvider — OpenAI-compatible chat-completions

Sends temperature=0.2, response_format=json_object (OpenAI's force-JSON
mode). Compatible servers (Ollama, LM Studio, OpenRouter) honor it;
Anthropic's compat shim ignores it but the system prompt also instructs
JSON, so we get JSON reliably either way. Tests mock mockito for the
success/upstream-error/malformed-inner-json paths."
```

### Task 9: `ClaudeCodeProvider` impl

**Files:**
- Modify: `src-tauri/src/lessons/claude_code.rs`

- [ ] **Step 1: Empirically determine `claude -p` output shape**

Before implementing, run:

```bash
echo "What is 1+1? Reply with ONLY a JSON object {\"answer\":\"2\"}." | claude -p
```

Observe whether the stdout is:
- (a) just `{"answer":"2"}` (bare model text), or
- (b) a JSON envelope like `{"type":"result", "result":"{\"answer\":\"2\"}", ...}` (Claude Code wraps the model output).

Document the observed shape in a comment at the top of the implementation. The implementation below assumes (b) and unwraps the `result` field; if (a) is observed, simplify by passing stdout directly to `serde_json::from_str`.

- [ ] **Step 2: Write the implementation**

Replace the stub in `claude_code.rs`. The implementation below assumes shape (b); adjust if Step 1 found (a):

```rust
//! Claude Code subprocess provider — `claude -p` with stdin/stdout.
//!
//! Empirical note (verified 2026-06-13): `claude -p` returns a JSON
//! envelope where the model's text lives in the `result` field. We
//! re-parse `result` as the lesson envelope.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::lessons::prompt::SYSTEM_PROMPT;
use crate::lessons::provider::{ExtractEnvelope, ExtractError, LessonProvider};

pub struct ClaudeCodeProvider {
    pub binary_path: PathBuf,
    pub timeout: Duration,
}

#[derive(Deserialize)]
struct ClaudeOutput {
    result: String,
}

impl ClaudeCodeProvider {
    /// Tries to resolve the `claude` binary via PATH. Returns None if not found.
    pub fn detect() -> Option<Self> {
        which::which("claude").ok().map(|p| Self {
            binary_path: p,
            timeout: Duration::from_secs(120),
        })
    }
}

#[async_trait::async_trait]
impl LessonProvider for ClaudeCodeProvider {
    async fn extract(&self, source: &str) -> Result<ExtractEnvelope, ExtractError> {
        let prompt = format!("{SYSTEM_PROMPT}\n\n---\n\n{source}");

        let mut child = Command::new(&self.binary_path)
            .arg("-p")
            .arg("--output-format")
            .arg("json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ExtractError::Transport(format!("spawn claude: {e}")))?;

        // Write prompt to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| ExtractError::Transport(format!("stdin write: {e}")))?;
            // Closing stdin signals end-of-input
            drop(stdin);
        }

        // Wait with timeout
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| ExtractError::Transport("claude -p timed out".into()))?
            .map_err(|e| ExtractError::Transport(format!("wait: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ExtractError::UpstreamError(format!(
                "claude -p exit {}: {stderr}",
                output.status
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        // Step 1: parse the Claude Code envelope (the `result` field holds the model output)
        let wrapper: ClaudeOutput = serde_json::from_str(&stdout).map_err(|e| {
            ExtractError::MalformedEnvelope {
                reason: format!("claude wrapper: {e}"),
                raw: stdout.clone(),
            }
        })?;

        // Step 2: parse the model's JSON output as the lesson envelope
        serde_json::from_str::<ExtractEnvelope>(&wrapper.result).map_err(|e| {
            ExtractError::MalformedEnvelope {
                reason: format!("inner json: {e}"),
                raw: wrapper.result,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_none_when_binary_absent() {
        // Override PATH to a directory that doesn't contain `claude`
        let dir = tempfile::tempdir().unwrap();
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        let result = ClaudeCodeProvider::detect();
        if let Some(orig) = original {
            std::env::set_var("PATH", orig);
        }
        assert!(result.is_none());
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml lessons::claude_code`
Expected: 1 passed.

- [ ] **Step 4: Manual smoke test**

If you have `claude` installed locally, run a quick smoke test via a tiny standalone binary or by temporarily adding a test. Confirm the provider returns a valid envelope on real source text. (No automated integration test for this — `claude` is an external dependency we can't reliably mock.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lessons/claude_code.rs
git commit -m "lessons: ClaudeCodeProvider — claude -p subprocess

Spawns 'claude -p --output-format json', writes prompt to stdin, reads
stdout, double-unwraps (Claude Code envelope -> model JSON -> our
envelope). detect() resolves the binary via which::which; returns None
if not on PATH so the UI can hide the option. Timeout 120s (claude has
~5-10s cold-start latency). Test covers the no-binary path; real-call
testing is manual since 'claude' is an external dep."
```

---

## Phase C — Persistence + account config

### Task 10: Extend `Account` with `LlmConfig`

**Files:**
- Modify: `src-tauri/src/accounts.rs`

- [ ] **Step 1: Add LlmConfig types**

In `accounts.rs`, find the `Account` struct. Add the following types above it:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderKind {
    None,        // unconfigured
    ClaudeCode,
    Http,
}

impl Default for LlmProviderKind {
    fn default() -> Self {
        LlmProviderKind::None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmConfig {
    #[serde(default)]
    pub provider: LlmProviderKind,
    #[serde(default)]
    pub http_base_url: Option<String>,
    #[serde(default)]
    pub http_model: Option<String>,
    /// Keychain key name (not the value!). Format: "llm_api_key::{account_id}".
    /// Stored in keychain under service=`jodd`, key=this value.
    #[serde(default)]
    pub http_api_key_keychain: Option<String>,
}
```

- [ ] **Step 2: Add `llm: LlmConfig` field to `Account`**

Find the `Account` struct in `accounts.rs`. Add:

```rust
#[serde(default)]
pub llm: LlmConfig,
```

The `#[serde(default)]` ensures backward compatibility — existing `accounts.json` files without the field deserialize cleanly using the `Default` impl.

- [ ] **Step 3: Build & verify**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/accounts.rs
git commit -m "accounts: LlmConfig per-account field

Per-account LLM provider settings live in accounts.json alongside
notes_label and meta_label (existing pin sidecar settings). API keys
NEVER go in accounts.json — only the keychain key name is stored;
secrets stay in OS keychain. Backward compatible: existing accounts
without the field deserialize via #[serde(default)]."
```

### Task 11: API key keychain helpers + provider resolution

**Files:**
- Modify: `src-tauri/src/accounts.rs` (key helpers)
- Create: `src-tauri/src/lessons/resolve.rs`
- Modify: `src-tauri/src/lessons/mod.rs` (add `pub mod resolve;`)

- [ ] **Step 1: Keychain helpers in accounts.rs**

Find the existing keychain code (look for `keychain_key` / `read_refresh_token` near line 159). Add at the bottom of the file:

```rust
/// Build the keychain key for an account's LLM API key.
pub fn llm_keychain_key(account_id: &str) -> String {
    format!("llm_api_key::{}", account_id)
}

/// Read the LLM API key from keychain. Returns None if not set.
pub fn read_llm_api_key(account_id: &str) -> Option<String> {
    let entry = keyring::Entry::new(KC_SERVICE, &llm_keychain_key(account_id)).ok()?;
    entry.get_password().ok()
}

/// Write the LLM API key to keychain.
pub fn write_llm_api_key(account_id: &str, key: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(KC_SERVICE, &llm_keychain_key(account_id))
        .map_err(|e| format!("keychain open: {e}"))?;
    entry
        .set_password(key)
        .map_err(|e| format!("keychain write: {e}"))
}

/// Remove the LLM API key from keychain (e.g. on provider change to None).
pub fn delete_llm_api_key(account_id: &str) {
    if let Ok(entry) = keyring::Entry::new(KC_SERVICE, &llm_keychain_key(account_id)) {
        let _ = entry.delete_password();
    }
}
```

- [ ] **Step 2: Provider resolution**

Create `src-tauri/src/lessons/resolve.rs`:

```rust
//! Resolve an account's configured LessonProvider.

use std::time::Duration;

use crate::accounts::{read_llm_api_key, Account, LlmProviderKind};
use crate::lessons::claude_code::ClaudeCodeProvider;
use crate::lessons::http::HttpProvider;
use crate::lessons::provider::{ExtractError, LessonProvider};

pub fn resolve_provider(
    account: &Account,
) -> Result<Box<dyn LessonProvider>, ExtractError> {
    match account.llm.provider {
        LlmProviderKind::None => Err(ExtractError::NotConfigured(
            "no LLM provider configured for this account".into(),
        )),
        LlmProviderKind::ClaudeCode => ClaudeCodeProvider::detect()
            .map(|p| Box::new(p) as Box<dyn LessonProvider>)
            .ok_or_else(|| {
                ExtractError::NotConfigured(
                    "claude binary not found in PATH".into(),
                )
            }),
        LlmProviderKind::Http => {
            let base_url = account
                .llm
                .http_base_url
                .clone()
                .ok_or_else(|| ExtractError::NotConfigured("http base_url missing".into()))?;
            let model = account
                .llm
                .http_model
                .clone()
                .ok_or_else(|| ExtractError::NotConfigured("http model missing".into()))?;
            let api_key = read_llm_api_key(&account.id);
            Ok(Box::new(HttpProvider {
                base_url,
                model,
                api_key,
                timeout: Duration::from_secs(90),
            }))
        }
    }
}
```

- [ ] **Step 3: Register the module**

In `src-tauri/src/lessons/mod.rs`, add:

```rust
pub mod resolve;
```

- [ ] **Step 4: Build & verify**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: clean. (If `Account.id` is named differently in your codebase, e.g. `email_address`, adjust the resolve.rs reference accordingly.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/accounts.rs src-tauri/src/lessons/
git commit -m "lessons: provider resolution + LLM API key keychain helpers

read/write/delete_llm_api_key follow the existing rt::{email} keychain
convention but under llm_api_key::{account_id}. resolve_provider
constructs the right LessonProvider from account.llm.* + keychain
secrets, returning NotConfigured if anything's missing — the modal
shows a friendly 'open Account Settings' link on that error."
```

---

## Phase D — Tauri commands

### Task 12: `extract_lessons` command

**Files:**
- Modify: `src-tauri/src/lib.rs` — new command + register in invoke_handler

- [ ] **Step 1: Add the command**

In `lib.rs`, add (near the other Tauri commands):

```rust
use crate::lessons::{
    markdown::assemble_note_body, provider::ExtractError, resolve::resolve_provider,
};

#[tauri::command]
async fn extract_lessons(
    account_id: String,
    source_text: String,
    title_override: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    log!("extract_lessons: account={} source_len={}", account_id, source_text.len());

    // Resolve provider from account config
    let account = state
        .accounts
        .iter()
        .find(|a| a.id == account_id)
        .ok_or_else(|| format!("account not found: {account_id}"))?
        .clone();
    let provider = resolve_provider(&account).map_err(|e| e.to_string())?;

    // Call LLM
    let envelope = match provider.extract(&source_text).await {
        Ok(env) => env,
        Err(e) => {
            // Doctrine: don't lose source. Create a fallback note with only Source.
            log!("extract_lessons: provider error {e:?} — creating fallback note");
            let uuid = create_fallback_source_note(&state, &account_id, &source_text)?;
            return Err(format!("LLM call failed; source preserved in note {uuid}. {e}"));
        }
    };

    // Assemble note body
    let body_html = assemble_note_body(&envelope, &source_text);

    // Derive title
    let title = title_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| envelope.title.clone())
        .or_else(|| derive_title_from_markdown(&envelope.lessons_markdown))
        .unwrap_or_else(|| format!("Lessons — {}", chrono::Utc::now().format("%Y-%m-%d")));

    // Ensure the workflow folder exists
    let folder = state
        .db
        .ensure_workflow_folder(&account_id, "Lessons")
        .map_err(|e| format!("ensure folder: {e}"))?;

    // Create the note locally (existing apply_local_edit path)
    let uuid = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp_millis();
    state
        .db
        .apply_local_edit(
            &account_id,
            &uuid,
            &title,
            &body_html,
            &folder,
            now,
        )
        .map_err(|e| format!("apply_local_edit: {e}"))?;

    // Worker will push to Gmail on next tick.
    log!("extract_lessons: created note uuid={uuid} in {folder}");
    Ok(uuid)
}

fn derive_title_from_markdown(md: &str) -> Option<String> {
    for line in md.lines() {
        if let Some(stripped) = line.strip_prefix("## ") {
            // First H2 — strip "Lesson N — " prefix if present, else use as-is
            let title = stripped
                .trim_start_matches("Lesson 1 — ")
                .trim_start_matches("Lesson — ")
                .trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

fn create_fallback_source_note(
    state: &AppState,
    account_id: &str,
    source: &str,
) -> Result<String, String> {
    let folder = state
        .db
        .ensure_workflow_folder(account_id, "Lessons")
        .map_err(|e| format!("ensure folder: {e}"))?;
    let body = format!(
        "<p><em>Extraction failed. Source preserved below.</em></p>\n<hr>\n\
         <details open>\n<summary>Source (verbatim)</summary>\n<pre>{}</pre>\n</details>\n",
        crate::lessons::markdown::escape_html(source)
    );
    let uuid = uuid::Uuid::new_v4().to_string();
    let title = format!("Source (extraction failed) — {}", chrono::Utc::now().format("%Y-%m-%d"));
    let now = chrono::Utc::now().timestamp_millis();
    state
        .db
        .apply_local_edit(account_id, &uuid, &title, &body, &folder, now)
        .map_err(|e| format!("apply_local_edit: {e}"))?;
    Ok(uuid)
}
```

- [ ] **Step 2: Register in invoke_handler**

Find the `tauri::Builder::default().invoke_handler(tauri::generate_handler![ ... ])` call. Add `extract_lessons` to the list.

- [ ] **Step 3: Build & verify**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: clean. (If `apply_local_edit` has a different signature in your codebase, adjust accordingly — the spirit is "use the existing local-first note insert path.")

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "lib: extract_lessons Tauri command

Pipeline: resolve provider → call LLM → assemble body → ensure
workflow folder → apply_local_edit. On LLM failure: doctrine compliance
creates a fallback note containing only the Source section so the paste
is never lost. Title resolution: override → envelope.title → first H2
from markdown → date fallback."
```

### Task 13: `re_extract_lessons` command + LLM settings commands

**Files:**
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/accounts.rs` (a `save_account` helper if it doesn't exist)

- [ ] **Step 1: Re-extract command**

Add to `lib.rs`:

```rust
#[tauri::command]
async fn re_extract_lessons(
    account_id: String,
    uuid: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    // Read the note's current body_html, extract the source block
    let note = state
        .db
        .get_note(&account_id, &uuid)
        .map_err(|e| format!("get_note: {e}"))?
        .ok_or_else(|| format!("note not found: {uuid}"))?;

    let source = crate::lessons::markdown::extract_source(&note.body_html)
        .ok_or_else(|| "note has no Source section to re-extract from".to_string())?;

    // Reuse the existing extract pipeline. This creates a NEW note rather
    // than overwriting — user can compare and delete the older one.
    extract_lessons(account_id, source, None, state).await
}
```

- [ ] **Step 2: LLM settings get/update commands**

Add to `lib.rs`:

```rust
#[tauri::command]
fn get_llm_settings(
    account_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<crate::accounts::LlmConfig, String> {
    state
        .accounts
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.llm.clone())
        .ok_or_else(|| format!("account not found: {account_id}"))
}

#[tauri::command]
fn update_llm_settings(
    account_id: String,
    cfg: crate::accounts::LlmConfig,
    api_key: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // Mutate the account in memory
    let mut accounts = state.accounts.clone();
    let acct = accounts
        .iter_mut()
        .find(|a| a.id == account_id)
        .ok_or_else(|| format!("account not found: {account_id}"))?;
    acct.llm = cfg.clone();

    // Save accounts.json
    crate::accounts::save_accounts(&accounts).map_err(|e| format!("save accounts: {e}"))?;

    // Save API key to keychain (or delete if cleared)
    if let Some(key) = api_key {
        if key.trim().is_empty() {
            crate::accounts::delete_llm_api_key(&account_id);
        } else {
            crate::accounts::write_llm_api_key(&account_id, &key)?;
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Verify `save_accounts` exists**

Run: `grep "fn save_accounts" src-tauri/src/accounts.rs`. If absent, add a thin wrapper:

```rust
pub fn save_accounts(accounts: &[Account]) -> Result<(), String> {
    // Find or reuse the existing accounts.json persist path
    let path = accounts_json_path()?;  // existing helper
    let json = serde_json::to_string_pretty(accounts).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}
```

- [ ] **Step 4: Register all three commands in invoke_handler**

Add `re_extract_lessons`, `get_llm_settings`, `update_llm_settings` to the handler list.

- [ ] **Step 5: Build & verify**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/src/accounts.rs
git commit -m "lib: re_extract_lessons + LLM settings commands

re_extract_lessons reuses the extract pipeline against the Source block
of an existing note. Creates a NEW note rather than overwriting so user
can compare. get/update_llm_settings round-trip LlmConfig to accounts.json;
update separately writes the API key to keychain (never to JSON)."
```

---

## Phase E — Frontend

`★ Insight ─────────────────────────────────────`
The frontend is the surface that makes or breaks UX. Two principles to follow: (1) optimistic updates everywhere per Jodd's local-first doctrine — the new note appears in `$notes` immediately when the backend returns, never waits for Gmail; (2) Apple Notes round-trip means everything in the note body is just HTML — no Svelte-specific structure needs to survive Apple's renderer.
`─────────────────────────────────────────────────`

### Task 14: `LessonExtractModal.svelte`

**Files:**
- Create: `src/lib/components/LessonExtractModal.svelte`

- [ ] **Step 1: Create the modal**

Create `src/lib/components/LessonExtractModal.svelte`:

```svelte
<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { get } from 'svelte/store';
  import { currentAccount, notes, selectedNoteUuid, selectedFolder } from '../stores/notes';

  let { open = $bindable(false) }: { open: boolean } = $props();

  let sourceText = $state('');
  let titleOverride = $state('');
  let busy = $state(false);
  let errorMsg = $state('');

  async function extract() {
    const acct = get(currentAccount);
    if (!acct) {
      errorMsg = 'No account selected.';
      return;
    }
    if (!sourceText.trim()) {
      errorMsg = 'Paste some source text first.';
      return;
    }
    busy = true;
    errorMsg = '';
    try {
      const newUuid = await invoke<string>('extract_lessons', {
        accountId: acct,
        sourceText,
        titleOverride: titleOverride.trim() || null,
      });
      // Close modal and navigate to the new note. The backend already
      // wrote to SQLite synchronously; we trigger a cache repaint and
      // select the new note.
      open = false;
      sourceText = '';
      titleOverride = '';
      selectedFolder.set('Notes/Lessons');
      selectedNoteUuid.set(newUuid);
      // notes store will repaint via the existing folder navigation flow
    } catch (e) {
      errorMsg = String(e);
    } finally {
      busy = false;
    }
  }

  function close() {
    if (busy) return;
    open = false;
    sourceText = '';
    titleOverride = '';
    errorMsg = '';
  }
</script>

{#if open}
  <div class="backdrop" onclick={close} role="presentation">
    <div class="modal" onclick={(e) => e.stopPropagation()} role="dialog" aria-modal="true">
      <h2>Extract Lessons</h2>
      <p class="hint">
        Paste source text from a conversation, transcript, article, or other source.
        Jodd will extract distinct lessons and file them in Notes/Lessons.
      </p>

      <label>
        Source text
        <textarea
          bind:value={sourceText}
          rows="15"
          disabled={busy}
          placeholder="Paste here..."
        ></textarea>
      </label>

      <label>
        Title (optional)
        <input
          type="text"
          bind:value={titleOverride}
          disabled={busy}
          placeholder="Auto-derived from first lesson"
        />
      </label>

      {#if errorMsg}
        <div class="error">{errorMsg}</div>
      {/if}

      <div class="actions">
        <button onclick={close} disabled={busy}>Cancel</button>
        <button onclick={extract} disabled={busy || !sourceText.trim()} class="primary">
          {busy ? 'Extracting…' : 'Extract'}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed; inset: 0;
    background: rgba(0, 0, 0, 0.3);
    display: flex; align-items: center; justify-content: center;
    z-index: 1000;
  }
  .modal {
    background: #fffef9;
    width: 600px; max-width: 90vw; max-height: 85vh; overflow-y: auto;
    padding: 24px; border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.2);
  }
  h2 { margin: 0 0 8px; }
  .hint { color: #666; font-size: 13px; margin: 0 0 16px; }
  label { display: block; margin: 12px 0; font-size: 13px; color: #555; }
  textarea, input { width: 100%; padding: 8px; font: inherit; box-sizing: border-box; margin-top: 4px; }
  textarea { font-family: monospace; font-size: 12px; }
  .error { color: #c33; padding: 8px; background: #fee; border-radius: 4px; margin: 12px 0; }
  .actions { display: flex; gap: 8px; justify-content: flex-end; margin-top: 16px; }
  .actions button.primary { background: #2563eb; color: white; border: none; padding: 8px 16px; border-radius: 4px; cursor: pointer; }
  .actions button.primary:disabled { background: #ccc; }
</style>
```

- [ ] **Step 2: Verify the component compiles**

Run: `npm run check 2>&1 | head -30` (or `npx svelte-check --tsconfig ./tsconfig.json src/lib/components/LessonExtractModal.svelte`). Expected: no errors specific to the new file. (If `svelte-check` flags `$bindable`/`$state` as unknown, ensure Svelte 5 mode is on — check `svelte.config.js`.)

- [ ] **Step 3: Commit**

```bash
git add src/lib/components/LessonExtractModal.svelte
git commit -m "components: LessonExtractModal

Paste-box modal with optional title field. On Extract: calls
extract_lessons, closes on success, navigates to Notes/Lessons +
selects the new note. On error: shows inline message, preserves the
pasted text in the textarea so user can retry."
```

### Task 15: `LlmProviderSettings.svelte` + AccountSettings integration

**Files:**
- Create: `src/lib/components/LlmProviderSettings.svelte`
- Modify: `src/lib/components/AccountSettings.svelte`

- [ ] **Step 1: Create the settings sub-component**

Create `src/lib/components/LlmProviderSettings.svelte`:

```svelte
<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';

  let { accountId }: { accountId: string } = $props();

  type LlmConfig = {
    provider: 'none' | 'claude_code' | 'http';
    http_base_url: string | null;
    http_model: string | null;
    http_api_key_keychain: string | null;
  };

  let cfg: LlmConfig = $state({
    provider: 'none',
    http_base_url: '',
    http_model: '',
    http_api_key_keychain: null,
  });
  let apiKey = $state('');
  let saving = $state(false);
  let msg = $state('');

  onMount(async () => {
    try {
      cfg = await invoke<LlmConfig>('get_llm_settings', { accountId });
    } catch (e) {
      msg = `load: ${e}`;
    }
  });

  async function save() {
    saving = true;
    msg = '';
    try {
      await invoke('update_llm_settings', {
        accountId,
        cfg: {
          provider: cfg.provider,
          http_base_url: cfg.provider === 'http' ? cfg.http_base_url : null,
          http_model: cfg.provider === 'http' ? cfg.http_model : null,
          http_api_key_keychain: cfg.provider === 'http' ? `llm_api_key::${accountId}` : null,
        },
        apiKey: cfg.provider === 'http' && apiKey ? apiKey : null,
      });
      msg = 'Saved.';
      apiKey = '';  // never keep the cleartext in memory
    } catch (e) {
      msg = `save: ${e}`;
    } finally {
      saving = false;
    }
  }
</script>

<div class="llm-settings">
  <h3>LLM Provider</h3>

  <label>
    <input type="radio" bind:group={cfg.provider} value="claude_code" />
    Claude Code (CLI) — uses your existing claude installation
  </label>

  <label>
    <input type="radio" bind:group={cfg.provider} value="http" />
    Custom endpoint (OpenAI-compatible)
  </label>

  {#if cfg.provider === 'http'}
    <div class="http-fields">
      <label>
        Base URL
        <input type="text" bind:value={cfg.http_base_url} placeholder="https://api.openai.com/v1" />
      </label>
      <label>
        Model
        <input type="text" bind:value={cfg.http_model} placeholder="gpt-4o-mini" />
      </label>
      <label>
        API key
        <input type="password" bind:value={apiKey} placeholder="(leave blank to keep existing)" />
      </label>
    </div>
  {/if}

  <label>
    <input type="radio" bind:group={cfg.provider} value="none" />
    Disabled
  </label>

  <button onclick={save} disabled={saving}>{saving ? 'Saving…' : 'Save'}</button>
  {#if msg}<p class="msg">{msg}</p>{/if}
</div>

<style>
  .llm-settings { padding: 12px 0; }
  label { display: block; margin: 8px 0; font-size: 13px; }
  .http-fields { margin-left: 24px; padding: 8px; background: #f5f5f0; border-radius: 4px; }
  input[type="text"], input[type="password"] { width: 100%; padding: 6px; font: inherit; box-sizing: border-box; margin-top: 4px; }
  button { padding: 8px 16px; font: inherit; }
  .msg { font-size: 12px; color: #666; }
</style>
```

- [ ] **Step 2: Wire into AccountSettings**

In `src/lib/components/AccountSettings.svelte`, import and include the sub-component. Add at the top of the script block:

```svelte
import LlmProviderSettings from './LlmProviderSettings.svelte';
```

And in the template, after the existing label/meta-label fields:

```svelte
<hr />
<LlmProviderSettings accountId={accountId} />
```

(`accountId` should already be a prop or store value in the parent.)

- [ ] **Step 3: Manual smoke test**

Run: `npm run tauri dev`. Open Account Settings via the ⚙ icon on an account row. Confirm the LLM Provider section renders and Save round-trips correctly. Test all three provider options.

- [ ] **Step 4: Commit**

```bash
git add src/lib/components/LlmProviderSettings.svelte src/lib/components/AccountSettings.svelte
git commit -m "components: LlmProviderSettings sub-component

Radio selector for none/claude_code/http. http variant exposes
Base URL, Model, and API Key (password field). API key cleartext is
wiped from component state after save; only the keychain holds it.
Wired into AccountSettings modal below existing label settings."
```

### Task 16: Sidebar grouping — Folders / Workflows split

**Files:**
- Modify: `src/lib/components/Sidebar.svelte`

- [ ] **Step 1: Update the folder tree rendering**

In `Sidebar.svelte`, find where folders are rendered (look for `{#each ... folders ...}`). Split the list by `kind`:

```svelte
<script lang="ts">
  // Existing imports + types — add kind to the folder type if not present:
  type Folder = {
    account_id: string;
    path: string;
    label_id: string | null;
    sync_state: string;
    last_local_modified_at: number | null;
    last_synced_at: number | null;
    kind: 'user' | 'system_workflow' | 'smart_query';
  };

  // ... existing reactive folder tree logic ...

  $: userFolders = (foldersForAccount(acct) ?? []).filter(f => f.kind !== 'system_workflow');
  $: workflowFolders = (foldersForAccount(acct) ?? []).filter(f => f.kind === 'system_workflow');
</script>

<!-- Where the existing folder list renders, wrap in two groups: -->

<div class="folder-group">
  <h4 class="group-header">Folders</h4>
  {#each userFolders as folder}
    <!-- existing folder row markup -->
  {/each}
</div>

{#if workflowFolders.length > 0}
  <div class="folder-group">
    <h4 class="group-header"><span class="group-icon">💡</span> Workflows</h4>
    {#each workflowFolders as folder}
      <!-- same folder row markup, optionally with workflow icon prefix -->
    {/each}
  </div>
{/if}

<style>
  .folder-group { margin-bottom: 8px; }
  .group-header {
    font-size: 11px; text-transform: uppercase;
    color: #888; margin: 8px 0 4px 8px;
    font-weight: 600;
  }
  .group-icon { margin-right: 4px; }
</style>
```

(Preserve the exact row markup that already exists — only the surrounding grouping is new.)

- [ ] **Step 2: Manual smoke test**

Run `npm run tauri dev`. After Task 12 ran once, `Notes/Lessons` should exist with `kind='system_workflow'`. Confirm it appears under "Workflows" group, not under "Folders." Confirm right-click on workflow folders shows the same menu as user folders (no special restriction).

- [ ] **Step 3: Commit**

```bash
git add src/lib/components/Sidebar.svelte
git commit -m "sidebar: Folders / Workflows group split

Filters folders by kind: user-kind under 'Folders' header, system_workflow
under 'Workflows' with a 💡 icon. Same right-click menu for both — workflow
folders are not locked or restricted, only visually distinguished. Group
header only renders when there's at least one workflow folder."
```

### Task 17: NoteContextMenu — "Re-extract lessons" + entrypoints

**Files:**
- Modify: `src/lib/components/NoteContextMenu.svelte`
- Modify: `src/lib/components/Sidebar.svelte` (add modal trigger)
- Modify: `src/App.svelte` (mount modal at top level, add hotkey)

- [ ] **Step 1: Add Re-extract menu item**

In `NoteContextMenu.svelte`, find the existing menu items list (e.g. "Move to," "Refetch," "Delete"). Add:

```svelte
{#if hasSourceBlock(note?.body_html ?? '')}
  <div class="menu-item" onclick={reExtract} role="button" tabindex="0">
    Re-extract lessons
  </div>
{/if}
```

And at the top of the script block:

```ts
import { invoke } from '@tauri-apps/api/core';

function hasSourceBlock(body: string): boolean {
  return body.includes('<summary>Source (verbatim)</summary>');
}

async function reExtract() {
  if (!note || !$currentAccount) return;
  try {
    const newUuid = await invoke<string>('re_extract_lessons', {
      accountId: $currentAccount,
      uuid: note.uuid,
    });
    // Navigate to the new note (the original is preserved)
    selectedNoteUuid.set(newUuid);
    selectedFolder.set('Notes/Lessons');
  } catch (e) {
    console.error('re-extract failed:', e);
    alert(`Re-extract failed: ${e}`);
  }
  closeMenu();  // existing close-menu function
}
```

- [ ] **Step 2: Mount LessonExtractModal at app root + add hotkey**

In `src/App.svelte`, near where other modals are mounted, add:

```svelte
<script lang="ts">
  // ... existing imports ...
  import LessonExtractModal from './lib/components/LessonExtractModal.svelte';

  let extractModalOpen = $state(false);

  function onKeydown(e: KeyboardEvent) {
    // Cmd+Shift+L → open Extract Lessons modal
    if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key === 'L') {
      e.preventDefault();
      extractModalOpen = true;
    }
    // ... existing keydown handlers ...
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!-- Mount at root, near other modals -->
<LessonExtractModal bind:open={extractModalOpen} />
```

- [ ] **Step 3: Add sidebar entrypoint**

In `Sidebar.svelte`, in the per-account header (where the existing ⚙ and 📌 dup-review buttons live), add:

```svelte
<button class="icon-btn" title="Extract lessons" onclick={() => extractModalOpen = true}>
  💡
</button>
```

Where `extractModalOpen` is a prop bound from the parent (App.svelte). Easiest path: use a small store rather than prop-drilling. Create or extend an existing UI store:

```ts
// src/lib/stores/ui.ts (create if absent)
import { writable } from 'svelte/store';
export const extractModalOpen = writable(false);
```

Then in both `App.svelte` and `Sidebar.svelte`, import and use `$extractModalOpen` / `extractModalOpen.set(true)`.

- [ ] **Step 4: Manual smoke test**

Run `npm run tauri dev`. Verify:
1. Cmd+Shift+L opens the modal.
2. The 💡 button in the sidebar account header also opens it.
3. After pasting and clicking Extract, modal closes, new note appears in `Notes/Lessons` and is selected in the editor.
4. Right-click on the new extracted note → "Re-extract lessons" item is present and works.
5. Right-click on a plain (non-extracted) note → "Re-extract lessons" item is absent.

- [ ] **Step 5: Commit**

```bash
git add src/lib/components/NoteContextMenu.svelte src/App.svelte src/lib/components/Sidebar.svelte src/lib/stores/ui.ts
git commit -m "ui: lesson-extraction entrypoints + Re-extract menu

Cmd+Shift+L global hotkey + 💡 button in each account's sidebar header
opens the modal (via shared ui store). NoteContextMenu shows
'Re-extract lessons' only when the note's body contains a
<summary>Source</summary> block, regardless of which folder the note
lives in — relocation never breaks the workflow."
```

---

## Phase F — End-to-end smoke test + docs

### Task 18: Full end-to-end smoke test

**Files:** (no code changes)

- [ ] **Step 1: Configure an LLM provider**

Run `npm run tauri dev`. Open Account Settings for your account. Configure either:
- **Claude Code:** select the radio button. Verify Save succeeds.
- **HTTP (OpenAI):** Base URL `https://api.openai.com/v1`, Model `gpt-4o-mini`, paste your API key. Save.

- [ ] **Step 2: Run extraction on real source**

Open the modal via Cmd+Shift+L. Paste a meaningful chunk of source text (use the conversation transcript from this very brainstorming session as a concrete test). Click Extract.

Expected:
- Spinner shows during the LLM call (5-30s depending on provider/model).
- On success: modal closes, `Notes/Lessons` is created if absent, new note appears at the top of that folder, editor opens to it.
- The note contains: tag line with `#hashtags`, `## Lesson` H2 sections with bullets, optional `## Meta-lessons` section, an `<hr>`, then a collapsible "Source (verbatim)" block.

- [ ] **Step 3: Verify folder grouping**

Confirm `Notes/Lessons` renders under the "Workflows" sidebar group with the 💡 icon, separate from the user-folders list.

- [ ] **Step 4: Verify Apple Notes round-trip**

Wait for the sync worker tick (~5s). On your iPhone, refresh Apple Notes. The new note should appear in `Notes/Lessons` with the same content. The `<details>` block likely renders as inline text (Apple's renderer may strip it) — confirm this is acceptable. If it's visually broken on iPhone, revisit the spec §6 footnote about replacing `<details>` with a plain `## Source` H2.

- [ ] **Step 5: Verify Re-extract**

Right-click the extracted note → "Re-extract lessons." Confirm a *new* note is created (the original is preserved). The new note's lessons may differ slightly from the first run (LLM variance), but the source block at the bottom is identical to the original.

- [ ] **Step 6: Verify failure mode**

Temporarily break the LLM config (e.g. set HTTP base URL to a bogus URL). Run extraction. Confirm:
- Error message appears in the modal.
- Source text is preserved in the textarea (not cleared).
- If user dismisses the modal, a fallback note titled "Source (extraction failed)" with just the source block is created in `Notes/Lessons`.

- [ ] **Step 7: Verify relocate**

Move the extracted note to a user folder (e.g. drag or use the right-click "Move to" menu). Verify:
- Note moves out of `Notes/Lessons` to the destination.
- The note still appears as a normal note in the new folder.
- The "Re-extract lessons" menu item is still available on it (because the source block is still present in its body).

- [ ] **Step 8: Document results**

If anything diverged from expected behavior in steps 2-7, note it in the spec's §11 Open Questions or open a follow-up issue.

### Task 19: Update CLAUDE.md

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add v0.15 entry under "Current status"**

Open `CLAUDE.md`. Add a new section near the top of the status block (above the Pin feature description):

```markdown
**Lesson Extraction** (roadmap-adjacent feature, landed v0.15.0):
LLM-backed paste-and-extract workflow that turns mixed unformatted source
text (Claude/ChatGPT conversation dumps, transcripts, debugging sessions,
articles) into structured Lessons notes. Lives in a new Jodd-managed
"system workflow folder" (`Notes/Lessons`, kind='system_workflow' per
migration #9), visually distinguished in the sidebar under a Workflows
group. Source text preserved verbatim in a collapsible `<details>` block
at the bottom of every extracted note, enabling re-extraction and
verification without re-pasting.

- LLM provider abstraction: trait + two impls (HTTP for any OpenAI-
  compatible endpoint; subprocess for `claude -p`). Per-account config in
  accounts.json; API keys in OS keychain under `llm_api_key::{account_id}`.
- Output is markdown-bodied (LLMs produce dramatically cleaner markdown
  than HTML); pulldown-cmark converts to HTML before storage. Matches
  Jodd's existing HTML body_html schema; round-trips to Apple Notes via
  existing Gmail sync.
- Failure doctrine: source text is NEVER lost. On LLM error, a fallback
  note containing only the Source block is created so the paste survives.
- See [docs/superpowers/specs/2026-06-13-lesson-extraction-design.md]
  for the design spec, and [docs/superpowers/plans/2026-06-13-lesson-extraction.md]
  for the implementation plan.
```

- [ ] **Step 2: Update the "Active edges" section**

In `CLAUDE.md` Active edges section, add the new edge:

```markdown
4. **LLM provider abstraction is single-purpose.** v1 supports only
   "Extract Lessons" — the system prompt is hardcoded in `lessons/prompt.rs`,
   the folder name is hardcoded as "Lessons". Adding the next workflow
   (Summarize, Extract Action Items) is additive (new prompt + new
   workflow ID + new folder name = new workflow); see the spec's §12
   Future Workflows.
```

- [ ] **Step 3: Update the roadmap section**

Mark items as done/in-progress. The original roadmap item #4 (Provider trait + Microsoft/Outlook backend) is unaffected — the `LessonProvider` trait is separate. But the new feature opens a path to roadmap item #2 (Tags) since the LLM auto-emits tags.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md — lesson extraction landed in v0.15

Adds feature description to Current status; new active edge noting v1's
single-workflow scope; updates roadmap to reflect tag auto-emission as
a side benefit of the extraction prompt."
```

---

## Acceptance criteria checkpoint

After all tasks complete, verify against the spec's §"Acceptance criteria for v1":

- [ ] Modal opens via sidebar trigger, hotkey, or right-click menu (Task 14 + 17)
- [ ] Pasting source + Extract produces a note in `Notes/Lessons` for the current account (Task 12 + 14)
- [ ] Note body contains `## Lessons` (from MD→HTML conversion), `## Source` collapsible, `#hashtag` tags (Task 6)
- [ ] `Notes/Lessons` auto-created on first use, `kind='system_workflow'` (Task 3 + 12)
- [ ] Sidebar renders `Notes/Lessons` under Workflows group with distinct icon (Task 16)
- [ ] Both providers produce equivalent envelopes on the same source (Tasks 8 + 9 + smoke test 18)
- [ ] Account Settings → LLM Provider section configurable (Task 15)
- [ ] API keys stored in keychain under `llm_api_key::{account_id}` (Task 11 + 13)
- [ ] LLM call failure → source preserved in fallback note (Task 12 + smoke test 18 step 6)
- [ ] Right-click → "Re-extract lessons" works regardless of folder (Task 17 + smoke test 18 step 5 & 7)
- [ ] Moving a workflow note to a user folder works via existing `move_notes_batch` (smoke test 18 step 7)
- [ ] Apple Notes round-trip: new note appears on iPhone with HTML intact (smoke test 18 step 4)

---

## Self-review summary

After writing the plan, ran the following checks:

1. **Spec coverage:** Each §3 goal traces to one or more tasks. Storage delta (§7.1, §7.2) → Tasks 1-3. Provider trait (§5) → Tasks 4-9. Note structure (§6) → Task 6. Account config (§5.4) → Tasks 10-11. Tauri commands → Tasks 12-13. UI → Tasks 14-17. Smoke test verifies §"Acceptance criteria" → Task 18. Documentation → Task 19.

2. **Placeholder scan:** No "TBD," "TODO," "Add appropriate error handling," "Similar to Task N." All test code, all implementation code, all commit messages are inline. Where a step says "find the existing X" (e.g. the migration runner in Task 1), it includes a grep command to locate it concretely.

3. **Type consistency:** `LessonProvider`, `ExtractEnvelope`, `ExtractError`, `LlmConfig`, `LlmProviderKind` are defined once and reused. Folder field `kind` consistently `String` (Rust) / `'user' | 'system_workflow' | 'smart_query'` (TS). API key keychain key format consistently `llm_api_key::{account_id}` across Rust (`accounts.rs`), Tauri command (Task 13), and frontend (Task 15).

4. **Known empirical unknown:** Task 9 Step 1 instructs an empirical check of `claude -p` output shape before committing the implementation. This is the right place to handle it — spec marked it as open, plan resolves it during implementation. If the observed shape differs from the assumed double-envelope, only `claude_code.rs` parsing needs to change; no upstream contract shifts.

---

End of plan.
