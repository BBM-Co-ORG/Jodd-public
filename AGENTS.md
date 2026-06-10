# Agent Guide — Jodd

## For Claude Code / AI Agents working on this project

### Immediate Priority — Fix OAuth env loading
The app fails with `Missing required parameter: client_id` because
`GOOGLE_CLIENT_ID` env var is not available at runtime.

**Root cause**: `dotenv::dotenv()` in `lib.rs run()` looks for `.env` relative
to the process working directory, which during `tauri dev` is `src-tauri/`.
The `.env` file is in the project root (one level up).

**Fix options** (pick one):
1. Change `dotenv::dotenv()` to `dotenv::from_path("../.env")` — simplest
2. Copy `.env` into `src-tauri/` as well
3. Use `tauri::api::path` to find project root dynamically

After fixing, `auth::client_id()` in `auth.rs` should return the real client ID.

### Second Priority — Fix Label ID → Name mapping
`gmail.rs list_notes()` returns `label: "Label_XXXXXXX"` (raw ID) instead of `"Notes"` or `"myNotes"`.

**Fix**: Before listing notes, call `labels.list`, build a `HashMap<String, String>` (id → name),
then use it when constructing `Note.label`.

```rust
// In gmail.rs
pub async fn get_label_map(token: &str) -> HashMap<String, String> {
    // GET /gmail/v1/users/me/labels
    // return map of id -> name
}
```

### Third Priority — Fix note save (replace, not append)
`save_note()` always creates a new Gmail message. Should replace existing.

**Fix**: After successful insert, delete the old message:
```rust
// After POST /messages succeeds and returns new_id:
if let Some(old_id) = existing_gmail_id {
    delete_note(token, &old_id).await.ok();
}
```
Note: `existing_id` in current code is the UUID, not the Gmail message ID.
Need to pass the Gmail message ID separately.

### Architecture Decisions
- **No local DB in v0.1** — all state lives in Gmail. Add SQLite cache in v0.2.
- **Svelte 5 syntax** — use `onclick={}` not `on:click={}`, use `mount()` not `new App()`
- **Rust async** — all Gmail API calls are async, use `tokio::spawn` for background work
- **Token storage** — access token in memory (AppState), refresh token in OS keyring

### Running the App
```bash
# from project root
npm run tauri dev

# frontend only (no Rust)
npm run dev
```

### Debugging OAuth
Add this to `lib.rs` in `open_auth_url` to verify env vars are loaded:
```rust
eprintln!("CLIENT_ID = {}", auth::client_id());
eprintln!("AUTH_URL = {}", auth::get_auth_url());
```

### Testing Note Sync
1. Add Google account to iPhone: Settings → Mail → Accounts → Google → enable Notes
2. Create a note in Apple Notes on iPhone
3. Check Gmail web: search `label:notes` — should see the note as an email
4. Run Jodd → sign in → should see the same note

### File Reference
| File | Purpose |
|------|---------|
| `src-tauri/src/auth.rs` | OAuth URL, token exchange, localhost:8080 callback server |
| `src-tauri/src/gmail.rs` | All Gmail API calls (list/fetch/save/delete/labels) |
| `src-tauri/src/lib.rs` | Tauri command handlers, AppState, run() entrypoint |
| `src/App.svelte` | Root: auth check, event listener, polling fallback, loadNotes |
| `src/lib/components/AuthScreen.svelte` | Sign in UI |
| `src/lib/components/Sidebar.svelte` | Folder list from note labels |
| `src/lib/components/NoteList.svelte` | Note list filtered by selected folder |
| `src/lib/components/NoteEditor.svelte` | contenteditable editor, autosave |
| `src/lib/stores/notes.ts` | Svelte stores: isAuthenticated, notes, folders, selectedNote, etc. |
| `src/lib/types.ts` | Note, Folder TypeScript interfaces |
