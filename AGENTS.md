# Agent Guide — Jodd

## For Claude Code / AI Agents working on this project

This file is the short-form orientation. **For anything substantive — current
architecture, doctrine, recent changes, what's done, what's remaining — read
`CLAUDE.md` instead.** It's the living source of truth and stays in sync with
each release.

This file exists to give agents enough surface-level grounding to know what's
here and where to look.

### What Jodd is

A Tauri 2 cross-platform app (Svelte 5 + TypeScript frontend, Rust backend) that
brings Apple Notes to non-Apple devices. Two backends ("verticals") behind a
shared trait surface: a **Gmail vertical** that round-trips Apple Notes by
speaking the same email protocol Apple uses internally, and a **LocalFS
vertical** that stores notes as `.eml` files on disk with no account and no
network at all. Windows, macOS, and Android builds are available; Android is a
Developer Preview.

### Current state (as of v0.23.0)

- Apple Notes ↔ Gmail bidirectional sync via OAuth2 PKCE, with embedded release
  credentials and a BYO override for developers
- **Standalone Local Folder vertical** — `.eml` vault, zero sign-in, works
  offline, same Apple-compatible HTML body format as the Gmail vertical
- Backend-agnostic trait surface (`Transport`/`AtRest`/`Identity`/`Deriver`/
  `MetadataSidecar`/`Vertical`) with `Box<dyn Vertical>` dispatch per account
- Local-first SQLite cache with full sync worker
- Multi-account support (mixed Gmail + local vaults), conflict resolution
  (keep-both), pin sync, cross-instance tag sync
- Full-text search (FTS5, Thai-aware)
- Inline `#hashtag` tags with body as source of truth
- `[[wikilink]]` fact-schema edges + local graph view, rename-safe slug links
- Outline/nested checklists + Tab/Shift-Tab indent
- Attachments (display + round-trip)
- Recently Deleted / Trash
- **Content extraction workflow** — paste any source, LLM distills into
  structured key points filed in `Notes/__Extracts__` (displayed as
  💡 Extracts under a Workflows group). Two providers: HTTP for any
  OpenAI-compatible endpoint, and `claude -p` subprocess.
- Optional diagnostics logging (App Settings → Diagnostics), off by default
- Android phone/tablet UI and APK release pipeline (Developer Preview)

### Architecture decisions (current)

- **SQLite is source of truth** for the UI; sync worker reconciles with the
  account's backend asynchronously on a 5s tick. Never block UI on the
  network (or on local disk I/O for the LocalFS vertical).
- **Backend abstraction** — Gmail-specific code lives behind the `Vertical`
  trait (`src-tauri/src/backend/`), not scattered through `lib.rs`. Adding a
  third backend (e.g. Microsoft Graph) means implementing the trait, not
  touching the sync worker or conflict reconciler.
- **Svelte 5 syntax** — `onclick={}` not `on:click={}`, use runes (`$state`,
  `$props`, `$derived`) in new components.
- **Tauri commands** — async Rust functions exposed via `#[tauri::command]`.
  Frontend invokes via `invoke('command_name', { camelCaseArgs })` — Tauri's
  serde bridge converts to snake_case Rust params.
- **Tokens** — access tokens in memory (AppState), refresh tokens in OS
  keychain under `service=jodd, key=rt::{email}`. Per-account; never on disk.
  Local vaults have no token at all.
- **Local-first doctrine** — see `CLAUDE.md` § "Local-first doctrine" for the
  full rules. Short version: every user action writes SQLite + updates DOM
  synchronously; backend latency lives in the background sync worker.

### Running the app

```bash
# from project root
npm install
npm run tauri dev

# frontend only (no Rust backend)
npm run dev
```

### Releases

Versioning convention is semver-ish. Patch releases (v0.16.1 → v0.16.2)
land polish/cleanup; minor releases (v0.16.x → v0.17.0) ship new features.
The versions in `package.json`, `src-tauri/Cargo.toml`, and
`src-tauri/tauri.conf.json` must stay in sync; update their generated lockfiles
too. See `RELEASE.md`.

### File reference

| Path | Purpose |
|------|---------|
| `src-tauri/src/lib.rs` | Tauri commands + sync worker tick + AppState + `Box<dyn Vertical>` dispatch |
| `src-tauri/src/db.rs` | SQLite cache: migrations, notes/folders/tags tables, FTS5, edges |
| `src-tauri/src/backend/mod.rs` | Backend-agnostic trait surface (`Transport`, `AtRest`, `Identity`, `Deriver`, `MetadataSidecar`, `Vertical`) |
| `src-tauri/src/backend/gmail/` | `GmailVertical` — Gmail REST API (list/fetch/save/delete/labels) |
| `src-tauri/src/backend/localfs/` | `LocalFsVertical` — `.eml` files on disk, no account |
| `src-tauri/src/mime822.rs` | Format-neutral RFC 822/MIME builder shared by both verticals |
| `src-tauri/src/auth.rs` | OAuth PKCE flow + localhost:8080 callback |
| `src-tauri/src/accounts.rs` | Multi-account JSON + keychain helpers |
| `src-tauri/src/lessons/` | Content extraction module (provider trait + impls) |
| `src/App.svelte` | Root: auth check, polling, store wiring |
| `src/lib/components/Sidebar.svelte` | Folder tree + tags + workflows split |
| `src/lib/components/NoteList.svelte` | Per-folder note list + search |
| `src/lib/components/NoteEditor.svelte` | contenteditable editor + autosave + tag chips |
| `src/lib/components/LessonExtractModal.svelte` | Paste-and-extract modal |
| `src/lib/components/NoteContextMenu.svelte` | Right-click on notes (move, delete, re-extract) |
| `src/lib/stores/notes.ts` | All Svelte stores (accounts, notes, folders, tags, etc.) |
| `src/lib/types.ts` | Note, Folder TypeScript interfaces |

### Where to find things

- **What's done / not done** → `CLAUDE.md` § "Done" / "Remaining" lists
- **Architecture & doctrine** → `CLAUDE.md` body
- **Design rationale & history** → `docs/DIRECTION.md`, `docs/superpowers/specs/`
- **Release process** → `RELEASE.md`
- **Open todos** → `TODO.md`
