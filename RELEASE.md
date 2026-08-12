# Release guide

## What's already wired up

- **OAuth credentials are embedded at build time** ([build.rs](src-tauri/build.rs) → [auth.rs](src-tauri/src/auth.rs)). Process env wins over `.env`; in CI, GitHub secrets are injected as env at the `cargo tauri build` step.
- **Tight CSP** in [tauri.conf.json](src-tauri/tauri.conf.json) — blocks inline scripts, allows inline styles (Apple Notes uses heavy inline CSS) and `data:`/`https:` images for note content.
- **Optimized release profile** in [Cargo.toml](src-tauri/Cargo.toml) (`strip`, `lto`, `panic="abort"`, `opt-level="s"`).
- **GitHub Actions workflow** at [.github/workflows/release.yml](.github/workflows/release.yml). Builds Apple Silicon macOS, Windows, and a signed universal Android APK. Intel macOS is temporarily disabled to conserve Actions minutes. The workflow triggers on a `v*` tag push or manual dispatch.

## What you still need to do

### 1. Required: add repo secrets

Repo → Settings → Secrets and variables → Actions → New repository secret

| Secret name | Value |
|---|---|
| `GOOGLE_CLIENT_ID` | Google OAuth Desktop client ID (macOS/Windows) |
| `GOOGLE_CLIENT_SECRET` | Google OAuth Desktop client secret |
| `GOOGLE_CLIENT_ID_ANDROID` | Google OAuth Web client ID used by the Android App Link callback |
| `GOOGLE_CLIENT_SECRET_ANDROID` | Google OAuth Web client secret |
| `ANDROID_KEYSTORE_BASE64` | Base64-encoded Android release keystore |
| `ANDROID_KEYSTORE_PASSWORD` | Android keystore password |
| `ANDROID_KEY_ALIAS` | Android release key alias |
| `ANDROID_KEY_PASSWORD` | Android release key password |
| `PUBLIC_REPO_TOKEN` | Token allowed to publish releases to `BBM-Co-ORG/Jodd-public` |

The Android job intentionally fails if its signing material is absent. A build
signed with another key would break the verified App Link used for OAuth on
existing installs.

### 2. Bump version

Five files — keep the three manifests and their lockfiles in sync:

- `version` in [package.json](package.json)
- root package version in [package-lock.json](package-lock.json)
- `version` in [src-tauri/tauri.conf.json](src-tauri/tauri.conf.json)
- `version` in [src-tauri/Cargo.toml](src-tauri/Cargo.toml)
- Jodd package version in [Cargo.lock](Cargo.lock)

### 3. Decide on Google OAuth app status

Google Cloud Console → OAuth consent screen.

| Status | When to use | Caveats |
|---|---|---|
| **Testing** | Internal/limited use | Cap 100 test users; refresh tokens expire in 7 days |
| **In Production** | Public release | Requires Google verification (free for `gmail.modify` scope — submit form, ~1-2 weeks) |

### 4. Trigger a release

Tag push (real public release):
```bash
git tag v0.23.0
git push origin v0.23.0
```

Manual dispatch (draft, for testing the pipeline):
- Actions tab → Release workflow → Run workflow

Manual dispatch builds into a **private draft release** for pipeline testing and
does not publish to the public repository.

A real tag builds all platforms, fills release notes from the matching
`CHANGELOG.md` section, and then automatically creates or updates a **published**
release in `BBM-Co-ORG/Jodd-public`. The private upstream release remains a draft.
Treat pushing the tag as the publication approval — there is no separate public
review gate after it.

### 5. Publish the sanitized source snapshot

The release workflow publishes binaries and notes only. After the tag succeeds,
publish the matching one-commit source snapshot separately:

```bash
public-mirror-prep/scripts/sync-to-public.sh v0.23.0
```

Read the generated summary and secret-scan result before confirming. The script
replaces the public repository's `main` history with a sanitized snapshot, so
the force-push confirmation is deliberate and must not be automated away.

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

## Installing on macOS (ad-hoc signed builds)

Current CI ships macOS bundles with **ad-hoc signing only** — no Developer ID, no notarization (see [release.yml](.github/workflows/release.yml)). When the `.dmg` / `.app` is downloaded via Chrome or Safari, the bundle picks up the `com.apple.quarantine` xattr. The combination of *quarantine* + *adhoc signature* (`TeamIdentifier=not set`, `spctl` rejects) makes Gatekeeper **hang `dyld` indefinitely** on first launch — the Dock icon shows "Application Not Responding" and `/usr/bin/sample` on the process shows the main thread stuck at `_dyld_start + 0` with ~96K physical footprint (binary never finishes loading).

Diagnosed 2026-06-14 on the v0.16.2 build. Signature state of a fresh download:
- `codesign -dv Jodd.app` → `Signature=adhoc`, `TeamIdentifier=not set`
- `spctl --assess -vv Jodd.app` → `rejected`
- `xattr -l Jodd.app` → `com.apple.quarantine: 0381;...;Chrome;...`

**Fix** (run after dragging Jodd.app into `/Applications`):

```bash
sudo xattr -dr com.apple.quarantine /Applications/Jodd.app
sudo codesign --force --deep --sign - /Applications/Jodd.app
open /Applications/Jodd.app
```

`spctl --assess` will keep saying `rejected` after this — expected for adhoc, not what blocked launch. The quarantine xattr was the blocker.

> **TODO (long-term fix):** add Apple Developer ID + notarization to the release CI. The `APPLE_*` secrets table under [Code signing](#code-signing) above is already plumbed through `tauri-action`; once the secrets land, the workflow notarizes automatically and end users can double-click without terminal commands. Tauri 2 bundle-signing config lives in [tauri.conf.json](src-tauri/tauri.conf.json).

## Pre-release smoke test (run before tagging)

1. **Clean macOS**: use a fresh user account or a VM. No `~/Library/Application Support/jodd`, no Keychain entry.
2. Install the built `.dmg`, open. Should land at AuthScreen.
3. Sign in with a real Google account. Should hit Gmail and start indexing.
4. Quit (Cmd-Q), reopen. Cache paint should show notes immediately; index should refresh in seconds.
5. Remove the account from the in-app panel. Re-add. Should work without restart.
6. Edit a note in Apple Notes (iPhone), wait ~30s, refresh in Jodd. Should show the edit.
7. Edit a note in Jodd, refresh Apple Notes on the iPhone. Should show the edit.
8. **Windows**: same as above on a fresh VM. Watch for SmartScreen, OAuth callback port collisions, font rendering, and sidebar layout at 1366×768.
9. **Android**: install the signed universal APK on a clean device, complete OAuth through the verified `https://jodd.bbmedia.co.th/oauth2redirect` App Link, then verify folder → notes → editor navigation, system Back, one edit in each direction, and an attachment display.
10. **MCP**: with a test-only folder allowlisted, run `list_accounts`, a bounded search, create/append a note, and tick one checklist row using the exact expected task text. Confirm a write outside the allowlist is refused.

## Known limitations

- **Desktop OAuth callback port `localhost:8080` is hard-coded** in [auth.rs](src-tauri/src/auth.rs). If the port is in use, desktop sign-in hangs with no useful error. Android does not open this listener; it returns through a verified HTTPS App Link.
- **SQLite cache stores note bodies unencrypted** at `~/Library/Application Support/jodd/jodd.sqlite3` (macOS) / `%APPDATA%\jodd\` (Windows). Disclose this in your install docs if relevant.
- **Release signing is incomplete on desktop.** macOS is ad-hoc signed and not notarized; Windows is unsigned, so both platforms show first-run trust warnings. Android releases are signed.
