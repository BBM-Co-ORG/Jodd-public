# Lesson Extraction — Design Spec

**Status:** Draft for review
**Author:** Brainstorming session 2026-06-13 (Kaiwan + Claude)
**Target version:** v0.15.0 (post-Pin, after Provider trait or in parallel)
**Estimated scope:** ~600 LoC Rust, ~400 LoC Svelte, one migration, one new crate dep

---

## 1. Problem

Users frequently produce mixed unformatted text from external sources — Claude/ChatGPT conversation dumps, Slack threads, meeting transcripts, browser-clipped articles — that contains valuable distinct lessons buried in noise. Today there is no path to ingest that text into Jodd and have the lessons surfaced as structured, searchable note content.

The first ingest path is **paste-from-anywhere**. Future ingest paths (browser plugin, share-sheet, file drop) are pure additions to the same downstream pipeline and out of scope for this spec.

## 2. Non-goals

- **Multi-workflow menu in v1.** v1 ships exactly one workflow: "Extract Lessons." The architecture must accommodate future workflows (Summarize, Extract Action Items, Expand Bullets) without refactoring, but those are deferred.
- **Browser plugin, share-sheet, file ingestion.** Future work.
- **In-place transformation of existing notes** (right-click → "Extract lessons from this note"). v1 is paste-modal only; right-click-on-existing-note is a v2 affordance.
- **Re-running extraction with a different prompt or provider.** v1 supports re-extraction (right-click → "Re-extract") against a note's preserved `## Source` section, but always uses the current default prompt and the current provider config.
- **Caching, batching, or cost dashboards.** Each extraction is a single LLM call; cost surfacing is per-call only.
- **Streaming progress in the UI.** The modal blocks until the LLM call completes (with a spinner and cancel). Streaming is a polish item; not v1.

## 3. Goals

1. Single paste-and-extract action from any account context produces a structured "Lessons" note in that account.
2. The output note preserves the raw source verbatim in a collapsible `## Source` section, so future-self can verify findings and re-run extraction.
3. The note lands in a Jodd-managed **system workflow folder** (`Notes/Lessons`) that is visually distinct from user folders in the sidebar.
4. Notes inside the workflow folder are normal Jodd notes — editable, movable, deletable, taggable, pinnable. Relocating one to a user folder works through existing `move_notes_batch`; it sheds workflow identity but keeps its `## Source` block and remains re-extractable.
5. LLM provider is user-configurable per-account, supporting both BYO OpenAI-compatible endpoints (covers OpenAI, Anthropic, OpenRouter, Ollama, LM Studio, anything) and Claude Code shell-out (`claude -p`) for users who want to inherit Claude Code's existing auth.
6. Tags suggested by the LLM are auto-inserted inline as `#hashtag` and picked up by Jodd's existing tag parser.
7. If extraction fails (LLM error, malformed JSON, subprocess crash), the source text is **never lost**: Jodd creates a note containing only the `## Source` section so the paste is preserved.

## 4. Architecture overview

```
┌──────────────────────────────────────────────────────────────┐
│  LessonExtractModal.svelte                                   │
│    • paste box                                               │
│    • optional title field (auto-derived from first H2)       │
│    • account context (defaults to currentAccount)            │
│    • [Extract] button + spinner + [Cancel]                   │
└──────────────────────┬───────────────────────────────────────┘
                       │ invoke('extract_lessons', { account_id, source_text, title? })
                       ▼
┌──────────────────────────────────────────────────────────────┐
│  lib.rs::extract_lessons (Tauri command)                     │
│    1. resolve LessonProvider from account settings           │
│    2. provider.extract(source_text) → ExtractEnvelope        │
│    3. md_to_html(envelope.lessons_markdown)                  │
│    4. assemble final body_html (Lessons + Source + meta)     │
│    5. ensure_workflow_folder(account_id, "Lessons")          │
│    6. db.apply_local_edit (new uuid, label=Notes/Lessons,    │
│       sync_state=dirty)                                      │
│    7. return new note UUID                                   │
└────────────────────┬─────────────────────────────────────────┘
                     │
                     ▼
┌──────────────────────────────────────────────────────────────┐
│  trait LessonProvider                                        │
│    async fn extract(&self, source: &str)                     │
│      -> Result<ExtractEnvelope, ExtractError>                │
│                                                              │
│  impl HttpProvider { endpoint, model, api_key_key }          │
│    POST /chat/completions, JSON-in-prompt, parse response    │
│                                                              │
│  impl ClaudeCodeProvider { path: Option<PathBuf> }           │
│    spawn `claude -p`, write to stdin, read stdout            │
└──────────────────────────────────────────────────────────────┘
```

Background sync worker is unchanged. The new note is `dirty`, the existing worker pushes it to Gmail on the next tick exactly like any other locally-created note. No new sync paths.

## 5. Provider abstraction

### 5.1 Trait

```rust
// src-tauri/src/lessons/provider.rs
#[derive(Debug)]
pub enum ExtractError {
    /// Provider can't be initialized — missing config, missing binary, etc.
    NotConfigured(String),
    /// Network or subprocess failure.
    Transport(String),
    /// LLM returned non-JSON or schema-incompatible JSON. Includes raw response.
    MalformedEnvelope { reason: String, raw: String },
    /// LLM returned an error response (rate limit, content filter, etc.).
    UpstreamError(String),
    /// User cancelled via the modal.
    Cancelled,
}

#[derive(Deserialize, Debug)]
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
```

### 5.2 HttpProvider (covers OpenAI, Anthropic-compat, OpenRouter, Ollama, LM Studio)

```rust
pub struct HttpProvider {
    pub base_url: String,      // e.g. "https://api.openai.com/v1"
    pub model: String,         // e.g. "gpt-4o-mini"
    pub api_key: Option<String>, // None → no Authorization header (local Ollama)
    pub timeout: Duration,     // default 90s
}
```

Calls `POST {base_url}/chat/completions` with an OpenAI-shaped body:
```json
{
  "model": "...",
  "messages": [
    { "role": "system", "content": <SYSTEM_PROMPT> },
    { "role": "user", "content": <source_text> }
  ],
  "temperature": 0.2,
  "response_format": { "type": "json_object" }
}
```

`response_format: json_object` is OpenAI's "force JSON mode" parameter. Most OpenAI-compatible servers (LM Studio, Ollama, OpenRouter) honor it. Anthropic's compat shim ignores it but the system prompt also instructs JSON, so we get JSON reliably either way. Defensive: parse `choices[0].message.content` as JSON; on parse failure, return `MalformedEnvelope`.

API key storage: existing keychain pattern. Service `jodd`, key format `llm_api_key::{account_id}::{provider_id}`. Account settings reference the key by name only; the raw key never leaves the keychain → keyring crate → backend.

### 5.3 ClaudeCodeProvider

```rust
pub struct ClaudeCodeProvider {
    pub binary_path: PathBuf,  // resolved via `which claude` at startup, cached
    pub timeout: Duration,     // default 120s — claude -p has more startup latency
}
```

Spawns subprocess: `claude -p --output-format json` (Claude Code's JSON output mode if available; otherwise plain `-p` with prompt-instructed JSON parsing). System prompt and source text concatenated and passed via stdin. Read stdout, parse as JSON, return `ExtractEnvelope`.

Detection at app startup: try `which claude` (or Windows `where claude`). If found, `ClaudeCodeProvider` is available as an option in Settings; if not, the option is shown disabled with a tooltip explaining "Install Claude Code to use this option."

**Caveat documented in spec:** `claude -p` returns its response as a JSON envelope where the model's text lives in a `result` field, not as the bare JSON the model produces. We need to extract that field and re-parse, OR we instruct the model to embed our envelope inside its response and unwrap two layers. Behavior to verify before implementation: run `claude -p` with a JSON-output prompt and confirm the response shape. **Open question, see §11.**

### 5.4 Provider resolution per account

Account settings (extending the existing `accounts.json` per-account config from the Pin feature) add:

```rust
pub struct LlmConfig {
    pub provider: LlmProviderKind,  // Http | ClaudeCode
    pub http_base_url: Option<String>,
    pub http_model: Option<String>,
    pub http_api_key_keychain: Option<String>, // keychain key, NOT the value
}

pub enum LlmProviderKind {
    Http,
    ClaudeCode,
}
```

If no config is present, the modal surfaces "LLM provider not configured — open Account Settings."

## 6. Output note structure

The new note is HTML-bodied (matching every other Jodd note). Structure:

```html
<!-- Tags line — picked up by existing inline #hashtag parser -->
<p>#lessons #debugging #macos</p>

<!-- Main extracted content (converted from envelope.lessons_markdown via pulldown-cmark) -->
<h2>Lesson 1 — Why .env matters in dev but not in the installed .app</h2>
<p>Jodd resolves GOOGLE_CLIENT_ID via two arms...</p>
<!-- ... etc ... -->

<!-- Meta-lessons (converted from envelope.meta_lessons_markdown) -->
<h2>Meta-lessons (general)</h2>
<ul>...</ul>

<!-- Collapsible source — pure HTML, no markdown conversion -->
<hr>
<details>
  <summary>Source (verbatim)</summary>
  <pre>[raw pasted text, HTML-escaped]</pre>
</details>
```

Apple Notes' HTML renderer handles `<details>` gracefully (either as a collapsible or as inline text; either is acceptable). The `<pre>` block preserves whitespace and prevents Apple's renderer from rewrapping the source.

**Title resolution:**
1. If `envelope.title` is present, use it verbatim.
2. Otherwise, first H2 from `lessons_markdown` (strip the `## ` and any `— ` suffix).
3. Otherwise fallback: `"Lessons — {YYYY-MM-DD}"`.

## 7. System workflow folder

### 7.1 Schema delta

Migration #5 adds a `kind` column to the `folders` table:

```sql
ALTER TABLE folders ADD COLUMN kind TEXT NOT NULL DEFAULT 'user';
-- Allowed values: 'user' | 'system_workflow' | 'smart_query' (future).
```

No backfill — every existing row defaults to `'user'`. Column is indexed only as part of the existing `(account_id, path)` PK; no separate index needed (workflow folders are few per account).

### 7.2 Auto-creation

`ensure_workflow_folder(account_id, "Lessons")`:
1. Check `folders` table for `(account_id, "Notes/Lessons")`.
2. If present, return existing path.
3. If absent: insert with `kind='system_workflow'`, `sync_state='dirty_new'`. Worker pushes to Gmail like any other folder.

This goes through the existing `insert_folder_local_new` path, which already invokes `ensure_ancestors` to create `Notes/` if it doesn't exist (per the D1 fix from 2026-06-09).

### 7.3 Sidebar treatment

`Sidebar.svelte` renders folders in groups:

```
┌─────────────────────────────────┐
│ [📁] Folders                    │  (collapsible header)
│   ▸ 📁 BBMedia            (2)   │
│   ▸ 📁 Personal           (2)   │
│   ▸ 📁 Projects           (2)   │
├─────────────────────────────────┤
│ [💡] Workflows                  │  (collapsible header)
│     💡 Lessons            (3)   │
└─────────────────────────────────┘
```

The grouping is driven by `folders.kind` (read once when building the folder tree). User folders render in the "Folders" group; `kind='system_workflow'` folders render in "Workflows" with a distinct icon. Both groups support the same right-click menu — workflow folders are NOT locked; the user can rename, move, delete, or move notes between them like any other folder.

### 7.4 Relocate behavior

When a user moves a note out of `Notes/Lessons` into a user folder via existing `move_notes_batch`, the note's `label` column updates and that's it. The note is no longer rendered under "Workflows" (because that grouping is by current folder, not by origin). No metadata about "was extracted" travels with the note.

**Re-extraction capability** lives at the **content layer, not the folder layer**: any note whose body contains a `<details><summary>Source` block (regex match in the body) gets a right-click menu item "Re-extract lessons." This works regardless of which folder the note currently lives in — relocation never breaks the workflow.

## 8. UI changes

### 8.1 New: `LessonExtractModal.svelte`

Triggered by:
- A "Workflows" entry in the sidebar account picker (`+ Extract Lessons`)
- Cmd+Shift+L global hotkey
- A new "Extract Lessons" item in the account's right-click menu

Modal content:
- **Account selector** (defaults to `$currentAccount`; usually hidden for single-account users)
- **Source paste box** (large textarea, ~20 lines visible, no character limit but soft warning if token-count estimate > model context)
- **Optional title field** (placeholder: "Auto-derive from first lesson")
- **Provider indicator** (small text: "Using: Claude Code" or "Using: OpenAI (gpt-4o-mini)"; click → opens Account Settings)
- **[Extract]** button (primary; disabled while empty or extraction in progress)
- **Spinner + [Cancel]** during the LLM call
- On success: modal closes, new note opens in the editor, sidebar refreshes to show the new note in `Notes/Lessons`
- On failure: error message in modal, source preserved, retry button. If user dismisses, source is still saved as a fallback note in `Notes/Lessons` with only `## Source` section

### 8.2 Extended: `AccountSettings.svelte`

Existing settings modal (per-account, behind the ⚙ icon) gains a new "LLM Provider" section:

```
┌────────────────────────────────────────────────────────┐
│  LLM Provider                                          │
│                                                        │
│  ◉ Claude Code (CLI)                                   │
│     Path:  /usr/local/bin/claude   [Detect]            │
│     Status: ✓ Detected, version 0.2.51                 │
│                                                        │
│  ○ Custom endpoint                                     │
│     Base URL:     [https://api.openai.com/v1     ]     │
│     Model:        [gpt-4o-mini                   ]     │
│     API key:      [••••••••••••••••••] [Test] [Save]   │
│                                                        │
└────────────────────────────────────────────────────────┘
```

API key Save writes to keychain; UI shows masked value or "not set." Test button fires a minimal `extract("test")` call and reports success/failure.

### 8.3 Sidebar grouping

Per §7.3 — split into "Folders" and "Workflows" groups. Sort within "Workflows" alphabetically (only one entry for now).

## 9. Failure modes & doctrine compliance

| Failure | Behavior |
|---|---|
| LLM provider not configured | Modal shows "Configure LLM in Account Settings" link; Extract button disabled. |
| Network error / timeout | Error message in modal; raw source preserved in textarea. Retry available. If user dismisses: fallback note created with only `## Source`. |
| Malformed JSON response | Same as network error. Raw LLM response stored in app log (not in note) for debugging. |
| User cancels mid-extraction | Subprocess killed (E) or HTTP request aborted (C). Source remains in modal. No note created. |
| User closes modal without extracting | No note created. Source discarded (it was never sent anywhere). |
| Claude Code binary not in PATH | "Claude Code (CLI)" option disabled with tooltip; only Custom endpoint usable. |
| Workflow folder already exists with wrong kind | If `Notes/Lessons` exists with `kind='user'`, leave it as-is, write extraction note into it anyway. Don't auto-upgrade `kind` — too easy to corrupt user state. |

**Local-first doctrine compliance:** The new `extract_lessons` Tauri command writes to SQLite synchronously *before* returning. The frontend optimistically updates `$notes` and switches focus to the new note. Background worker handles the Gmail push asynchronously. Matches the doctrine codified in CLAUDE.md.

**Pushing-set tracking:** The new note is added to `AppState.pushing` between insert and worker pickup, same as other local-first creates, to prevent self-induced false conflicts (the a693d11 pattern).

## 10. Migration & rollout

1. **Migration #5** — add `folders.kind` column. Default `'user'`. Backward-compatible; existing app versions ignore the column.
2. **New crate dependency** — `pulldown-cmark = "0.10"`. ~150 KB binary impact.
3. **No breaking changes** to existing Tauri commands, frontend stores, or sync worker.
4. **Feature flag:** none. v1 lands in v0.15.0 as a first-class feature. Account Settings shows the LLM Provider section unconditionally; if neither Claude Code nor a custom endpoint is configured, the modal trigger is hidden in the sidebar (no broken UX).

## 11. Open questions (deferred to implementation)

1. **`claude -p` output shape.** Need to empirically confirm whether `claude -p` returns:
   - The model's raw text (we parse that as JSON), or
   - A JSON envelope `{result: "...", ...}` (we extract `result` and parse it as JSON).
   This affects the `ClaudeCodeProvider::extract` parsing logic. Quick test via the CLI before writing the provider.
2. **Token-count estimation in modal.** OpenAI provides `tiktoken` for offline counting; Anthropic's count is approximate via API. For v1, skip the live token count — show a hard warning only if the pasted text is over 50 KB (rough proxy for "approaching most models' context windows"). Add live counting in v1.1.
3. **Streaming.** v1 is non-streaming. If the LLM is slow (10–30s for Claude Code on a long source), the modal spinner is acceptable but not delightful. Streaming partial output into a preview pane is a polish item — deferred.
4. **Cost surfacing.** v1 doesn't show estimated cost. Some users will care (especially heavy users of paid APIs). Add as v1.1 if requested.
5. **Re-extraction prompt.** Same prompt as fresh extraction. If output is bad, user re-runs; if still bad, user edits the system prompt in Account Settings (no UI for this in v1 — prompt is hardcoded). v2 candidate: per-account prompt customization.

## 12. Future workflows (out of scope, framing-only)

The architecture supports adding workflows by:
1. Defining a new workflow ID and default folder name (`"Summaries"`, `"Action Items"`).
2. Adding a corresponding system prompt.
3. Adding a modal trigger and provider call that returns the same `ExtractEnvelope` shape.

Each workflow gets its own `Notes/{WorkflowName}` system folder, auto-created on first use, rendered under "Workflows" in the sidebar. No new schema, no new abstractions — the work is mostly UI and prompt design.

Three to four workflows likely make sense over time:
- **Extract Lessons** (v1)
- **Summarize** — condense a long source into a one-paragraph summary
- **Extract Action Items** — pull out concrete TODOs from meeting notes / transcripts
- **Expand Bullets** — flesh out a sparse bullet list into a full prose note

Each is additive. None requires revisiting the design above.

---

## Acceptance criteria for v1

- [ ] Modal opens via sidebar trigger, hotkey, or right-click menu
- [ ] Pasting source text + clicking Extract produces a new note in `Notes/Lessons` for the current account
- [ ] New note body contains `## Lessons` (from LLM output, MD→HTML converted), `## Source` collapsible section (raw text), and `#hashtag` tags at the top
- [ ] Workflow folder `Notes/Lessons` is auto-created on first use, with `kind='system_workflow'`
- [ ] Sidebar renders `Notes/Lessons` under a "Workflows" group with distinct icon
- [ ] Both `HttpProvider` and `ClaudeCodeProvider` produce equivalent `ExtractEnvelope` outputs on the same source
- [ ] Account Settings → LLM Provider section allows configuring Claude Code path OR (Base URL, Model, API Key)
- [ ] API keys stored in keychain under `llm_api_key::{account_id}::{provider_id}`
- [ ] If LLM call fails after the user submits, source is preserved in a fallback note containing only `## Source`
- [ ] Right-click → "Re-extract lessons" works on any note whose body contains `<details><summary>Source` regardless of folder
- [ ] Moving a workflow note to a user folder via existing `move_notes_batch` works unchanged
- [ ] All round-trips: new extraction note appears on iPhone in `Notes/Lessons` after Gmail sync, with HTML body intact

## Appendix A — System prompt (v1 draft)

```
You are an expert lesson extractor for a personal knowledge management tool.

Given mixed unformatted text from a conversation, meeting transcript, debugging
session, article, or other source, extract distinct lessons learned.

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

Output the JSON object directly with no preamble, no markdown fence, no commentary.
```

This prompt is hardcoded in v1. Customization is a v2 concern.

## Appendix B — File touchpoints

New:
- `src-tauri/src/lessons/mod.rs` — module root
- `src-tauri/src/lessons/provider.rs` — trait + types
- `src-tauri/src/lessons/http.rs` — `HttpProvider` impl
- `src-tauri/src/lessons/claude_code.rs` — `ClaudeCodeProvider` impl
- `src-tauri/src/lessons/markdown.rs` — `pulldown-cmark` wrapper + Source-section assembler
- `src-tauri/src/lessons/prompt.rs` — system prompt constant
- `src/lib/components/LessonExtractModal.svelte`
- `src/lib/components/LlmProviderSettings.svelte` (sub-component of AccountSettings)

Modified:
- `src-tauri/src/lib.rs` — register `extract_lessons` + `re_extract_lessons` Tauri commands
- `src-tauri/src/db.rs` — migration #9 (`folders.kind` column; original spec said #5 but migrations 5-8 already exist for the tag feature), update `Folder` struct, update insert/list paths
- `src-tauri/src/accounts.rs` — extend per-account JSON config with `LlmConfig`
- `src/lib/components/Sidebar.svelte` — render workflow folders under "Workflows" group
- `src/lib/components/NoteContextMenu.svelte` — add "Re-extract lessons" conditional menu item
- `src/lib/components/AccountSettings.svelte` — include `LlmProviderSettings.svelte`
- `Cargo.toml` — add `pulldown-cmark`, `async-trait`
- `CLAUDE.md` — update roadmap section, add lesson-extraction to recent features

---

End of spec.
