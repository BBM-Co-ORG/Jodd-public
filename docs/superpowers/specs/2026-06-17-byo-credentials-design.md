# BYO-Credentials UI Design

**Goal:** Let users supply their own Google OAuth `client_id` + `client_secret` via an in-app settings modal so the pre-built binary supports Gmail sync without requiring a source build.

**Architecture:** Global credential store (one set for all Gmail accounts). User-configured credentials override compile-time embedded ones at runtime. `client_id` stored on disk; `client_secret` stored in OS keychain only.

**Tech Stack:** Rust (Tauri 2), Svelte 5 runes, OS keychain via `keyring` crate (already used for refresh tokens + LLM API keys).

---

## Storage

`google_oauth.json` in Tauri `config_dir` (same directory as `accounts.json`):
```json
{ "client_id": "123456-abc.apps.googleusercontent.com" }
```

`client_secret` stored in OS keychain under service=`jodd`, key=`oauth_client_secret::google`. Never written to disk.

## Credential Resolution (auth.rs)

Three-tier chain, highest priority first:

1. **User-configured** — `google_oauth.json` (client_id) + keychain `oauth_client_secret::google` (secret)
2. **Compile-time embedded** — `option_env!("GOOGLE_CLIENT_ID")` / `option_env!("GOOGLE_CLIENT_SECRET")` baked in by build.rs from CI secrets or local `.env`
3. **Runtime env var** — `std::env::var("GOOGLE_CLIENT_ID")` / `"GOOGLE_CLIENT_SECRET"`

If tier 1 is set, it wins — even if the binary has embedded credentials. This lets the pre-built release binary still work for the developer (embedded credentials) while allowing any user to override with their own.

## Backend: oauth_config.rs (new module)

```rust
pub struct OAuthConfig { pub client_id: String }

pub fn load() -> Option<OAuthConfig>           // reads google_oauth.json
pub fn save(client_id: &str) -> Result<()>     // writes google_oauth.json
pub fn clear() -> Result<()>                   // deletes google_oauth.json

pub fn load_secret() -> Option<String>         // keychain get oauth_client_secret::google
pub fn save_secret(s: &str) -> Result<()>      // keychain set
pub fn clear_secret() -> Result<()>            // keychain delete
```

## Tauri Commands (lib.rs additions)

| Command | Returns | Notes |
|---|---|---|
| `get_oauth_config` | `{ client_id: String, has_secret: bool }` | Never returns secret value |
| `save_oauth_config(client_id, client_secret)` | `Result<()>` | Empty client_id = clear |
| `clear_oauth_config` | `Result<()>` | Removes file + keychain |

## UI Components

### AppSettings.svelte (new)
- Store-driven: reads `$appSettingsOpen` from `ui.ts`
- On mount: `invoke('get_oauth_config')` → populates `clientId` field, `hasSecret` boolean
- Fields: `Client ID` (text), `Client Secret` (password + show/hide toggle)
- `hasSecret=true` shows placeholder `••••••••` with hint "already saved — leave blank to keep"
- Save: `invoke('save_oauth_config', { clientId, clientSecret })` — only sends secret if non-empty
- Clear button: `invoke('clear_oauth_config')` with confirm
- "How to create credentials →" link opens Google Cloud Console docs URL via `openUrl`
- Escape-to-close via `<svelte:window onkeydown>`

### Sidebar.svelte (modify)
- Add `⚙` gear button in `.sidebar-footer`, left of the existing version label
- `onclick={() => appSettingsOpen.set(true)}`

### AuthScreen.svelte (modify)
- On mount: `invoke('get_oauth_config')` → `credentialsConfigured: boolean`
- If `!credentialsConfigured` AND backend is Gmail: disable "Sign in with Google" button
- Show inline note: "Gmail sync requires credentials — Configure first" (clickable → `appSettingsOpen.set(true)`)
- LocalFS path unaffected

### ui.ts (modify)
Append:
```ts
export const appSettingsOpen = writable(false);
```

### App.svelte (modify)
Render `<AppSettings />` alongside existing `<About />` and `<WhatsNew />`.

## Backward Compatibility

- If `google_oauth.json` does not exist → `load()` returns `None` → falls through to tier 2 (embedded)
- Pre-built binary with embedded credentials: works unchanged for the developer
- User who sets their own credentials: tier 1 wins, embedded ignored
- Build-from-source users: their `.env` / CI secrets are tier 2, still work

## Error Handling

- Keychain save failure → surface error string in modal ("Failed to save to keychain: …")
- `google_oauth.json` write failure → surface error
- `get_oauth_config` called when no credentials configured anywhere → returns `{ client_id: "", has_secret: false }`

## Out of Scope

- Per-account credential override (global is sufficient; one Google Cloud project per user)
- Credential validation (test OAuth call) — user finds out at sign-in time
- Outlook/Microsoft credentials (different provider, additive later)
