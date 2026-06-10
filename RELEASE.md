# Release guide

## What's already wired up

- **OAuth credentials are embedded at build time** ([build.rs](src-tauri/build.rs) → [auth.rs](src-tauri/src/auth.rs)). Process env wins over `.env`; in CI, GitHub secrets are injected as env at the `cargo tauri build` step.
- **Tight CSP** in [tauri.conf.json](src-tauri/tauri.conf.json) — blocks inline scripts, allows inline styles (Apple Notes uses heavy inline CSS) and `data:`/`https:` images for note content.
- **Optimized release profile** in [Cargo.toml](src-tauri/Cargo.toml) (`strip`, `lto`, `panic="abort"`, `opt-level="s"`).
- **GitHub Actions workflow** at [.github/workflows/release.yml](.github/workflows/release.yml). Builds for Apple Silicon, Intel Mac, Windows. Triggers on `v*` tag push, or manual dispatch.

## What you still need to do

### 1. Required: add repo secrets

Repo → Settings → Secrets and variables → Actions → New repository secret

| Secret name | Value |
|---|---|
| `GOOGLE_CLIENT_ID` | from Google Cloud Console → OAuth Desktop client |
| `GOOGLE_CLIENT_SECRET` | same |

Without these, the workflow runs but the binary will fail OAuth at runtime (empty creds).

### 2. Bump version

Three places — keep in sync:

- `version` in [package.json](package.json)
- `version` in [src-tauri/tauri.conf.json](src-tauri/tauri.conf.json)
- `version` in [src-tauri/Cargo.toml](src-tauri/Cargo.toml)

### 3. Decide on Google OAuth app status

Google Cloud Console → OAuth consent screen.

| Status | When to use | Caveats |
|---|---|---|
| **Testing** | Internal/limited use | Cap 100 test users; refresh tokens expire in 7 days |
| **In Production** | Public release | Requires Google verification (free for `gmail.modify` scope — submit form, ~1-2 weeks) |

### 4. Trigger a release

Tag push (real release):
```bash
git tag v0.2.0
git push origin v0.2.0
```

Manual dispatch (draft, for testing the pipeline):
- Actions tab → Release workflow → Run workflow

The workflow creates a **draft release** with the built artifacts attached. Review and publish from the GitHub Releases UI.

## Optional but recommended

### Code signing

Without signing, users see:
- **macOS**: "Jodd can't be opened because Apple cannot check it for malicious software" on first launch (right-click → Open works around it)
- **Windows**: SmartScreen blue banner — user clicks "More info → Run anyway"

To sign, add these secrets and they'll be picked up automatically by the workflow:

**macOS** (needs Apple Developer membership, ~$99/yr):
| Secret | Notes |
|---|---|
| `APPLE_CERTIFICATE` | base64-encoded `.p12` of Developer ID Application cert |
| `APPLE_CERTIFICATE_PASSWORD` | password of the `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | your Apple ID |
| `APPLE_PASSWORD` | app-specific password (appleid.apple.com → Sign-In and Security → App-Specific Passwords) |
| `APPLE_TEAM_ID` | the Team ID from the Apple Developer portal |

For Apple to actually notarize (required for Gatekeeper to accept silently): the workflow already passes these to `tauri-action`, which runs notarization when all are present.

**Windows** (needs a code-signing cert from a CA — DigiCert, Sectigo, SSL.com etc; ~$100-500/yr):
| Secret | Notes |
|---|---|
| `WINDOWS_CERTIFICATE` | base64-encoded `.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | password of the `.pfx` |

Modern Windows code signing needs **EV certs** for SmartScreen to trust immediately; standard certs build reputation over time (many user installs).

### Auto-updater

Tauri has `tauri-plugin-updater` — point it at a JSON manifest hosted somewhere (e.g. GitHub Releases or your own server), sign updates with a key, app checks at startup and offers update. Significant setup; skip until you have actual users.

## Pre-release smoke test (run before tagging)

1. **Clean macOS**: use a fresh user account or a VM. No `~/Library/Application Support/jodd`, no Keychain entry.
2. Install the built `.dmg`, open. Should land at AuthScreen.
3. Sign in with a real Google account. Should hit Gmail and start indexing.
4. Quit (Cmd-Q), reopen. Cache paint should show notes immediately; index should refresh in seconds.
5. Remove the account from the in-app panel. Re-add. Should work without restart.
6. Edit a note in Apple Notes (iPhone), wait ~30s, refresh in Jodd. Should show the edit.
7. Edit a note in Jodd, refresh Apple Notes on the iPhone. Should show the edit.
8. **Windows**: same as above on a fresh VM. Watch for SmartScreen, OAuth callback port collisions, font rendering, sidebar layout at 1366×768.

## Known limitations

- **OAuth callback port `localhost:8080` is hard-coded** in [auth.rs:10](src-tauri/src/auth.rs). If port is in use, sign-in hangs with no useful error. Workaround: kill whatever's on 8080 and retry. Fix planned: bind to port 0 and register multiple redirect URIs.
- **SQLite cache stores note bodies unencrypted** at `~/Library/Application Support/jodd/jodd.sqlite3` (macOS) / `%APPDATA%\jodd\` (Windows). Disclose this in your install docs if relevant.
- **Sync worker race on remove account** — if the worker is mid-push when an account is removed, it can re-insert a row into the just-wiped cache. Edge case; never observed in practice. Fix: check account existence per-tick.
