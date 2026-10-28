# Release guide

## What's already wired up

- **OAuth credentials are embedded at build time** ([build.rs](src-tauri/build.rs) → [auth.rs](src-tauri/src/auth.rs)). Process env wins over `.env`; in CI, GitHub secrets are injected as env at the `cargo tauri build` step.
- **Tight CSP** in [tauri.conf.json](src-tauri/tauri.conf.json) — blocks inline scripts, allows inline styles (Apple Notes uses heavy inline CSS) and `data:`/`https:` images for note content.
- **Optimized release profile** in [Cargo.toml](src-tauri/Cargo.toml) (`strip`, `lto`, `panic="abort"`, `opt-level="s"`).
- **A two-repository release pipeline** (since 2026-10-26):
  - [.github/workflows/release.yml](.github/workflows/release.yml) **builds on `BBM-Co-ORG/Jodd-public`**, whose GitHub-hosted runners are free because it is public: Apple Silicon macOS and Windows bundles, the two jodd-mcp archives, an **unsigned** arm64 Android APK (the file keeps the `android-universal` name), and the Android at-rest-encryption proof. It leaves a **draft** release there. Intel macOS is still not built.
  - [.github/workflows/release-sign.yml](.github/workflows/release-sign.yml) **runs in this private repository only**. It is the one place the Android release keystore is used: it checks where the APK came from, signs it, checks the signer against [docs/android/assetlinks.json](docs/android/assetlinks.json), attaches it to the draft, and publishes.
  - Both files exist in both repositories (the sync copies `.github/`); each one's jobs are skipped in the other repository, so a stray run there bills nothing.
  - [scripts/release-dispatch.sh](scripts/release-dispatch.sh) runs the two halves in order from your machine.
- **No Rust cache in release builds.** They compile from scratch. A cache is not source, so the tree check cannot see it, and anything that runs on Jodd-public's `main` could write cache entries a release would link into the binaries and into the APK the release key signs. Minutes are free there; the cost is wall time — Windows cargo ~25 min cold vs ~7 warm, so expect a release to take roughly 30–40 minutes end to end. The old Mon/Thu warm-up schedule and `cache_only` dispatch are gone with it. setup-node's npm cache stays: `npm ci` checks every package against `package-lock.json`'s integrity hashes.

### Why the build moved, and what it saves

The private repository ran past its Actions spending limit in October 2026, and release builds were about 60% of that bill. Every run of the old `release.yml`, Oct 1–25 2026 (18 releases plus cache warm-ups):

| Runner | Minutes | Rate | Cost |
|---|---|---|---|
| macOS | 228 | $0.062/min | $14.14 |
| Windows | 880 | $0.010/min | $8.80 |
| Linux (Android ~11, proof ~13, uploads ~2 per release) | ~470 | $0.006/min (assumed) | ~$2.80 |
| **Total** | **~1,580** | | **~$25.75 per 25 days** |

That is ~88 billed minutes and ~$1.43 per release, ~$31 a month at that pace. What stays billed is `release-sign.yml`: ~3 Linux minutes (~$0.02) per release, ~65 minutes (~$0.40) a month — well inside the private repository's included minutes. **Saved: ~85 minutes and ~$1.40 per release; ~1,800 minutes and ~$30 a month.** The private repository's own CI is a separate budget line (PR #243).

## What you still need to do

### 1. Secrets — which repository holds what

**Never put the Android keystore, or any token for this repository, on Jodd-public.** Anyone holding the keystore can sign an APK that Android hands Jodd's App Links OAuth callback to (CLAUDE.md gotcha #8), and the key cannot be rotated without breaking updates for every existing install.

**Jodd-public** → Settings → Environments → `release` → Environment secrets. The `release` environment's deployment branches are limited to `main`, so only the snapshot's own workflow can read them:

| Secret name | Value |
|---|---|
| `GOOGLE_CLIENT_ID` | **Production** Google OAuth Desktop client ID (macOS/Windows) |
| `GOOGLE_CLIENT_SECRET` | Its client secret |
| `MS_CLIENT_ID` | Microsoft public client ID (no secret pairs with it) |
| `GOOGLE_CLIENT_ID_ANDROID` | Google OAuth **Web**-application client ID used by the Android App Link callback |
| `GOOGLE_CLIENT_SECRET_ANDROID` | Its client secret |

All five end up inside the shipped binaries, where anyone can extract them; that is why they may live in a public repository and the keystore may not. Each build job's first step fails if one is empty, rather than shipping a build that cannot sign in.

Also on Jodd-public, tighten `main`'s ruleset (Settings → Rules → `main_ruleset`). Today it blocks deletion and force-push and requires a pull request, but with **0 required approvals**, so anyone with write access can merge their own PR into `main` — and `main` is what releases build. Set the bypass list to the release maintainer only (the account that runs `sync-to-public.sh`, which needs to force-push), drop the blanket "Organization admin" and "Repository admin" bypasses, and require at least one approval. What this does **not** stop: a fine-grained token acts as its owner, so `PUBLIC_REPO_TOKEN` carries whatever bypass its owner has. The token stays a secret to guard, not a solved problem.

**This repository (BBM-Co-ORG/Jodd)** → Settings → Secrets and variables → Actions:

| Secret name | Value |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | Base64-encoded Android release keystore |
| `ANDROID_KEYSTORE_PASSWORD` | Android keystore password |
| `ANDROID_KEY_ALIAS` | Android release key alias |
| `ANDROID_KEY_PASSWORD` | Android release key password |
| `PUBLIC_REPO_TOKEN` | Fine-grained token, resource owner `BBM-Co-ORG`, repository access **Jodd-public only**: Contents read & write, Actions read |

Optionally move the four `ANDROID_*` secrets into an environment named `android-signing` with deployment branches limited to `main` — `release-sign.yml` already runs in it, so the keystore then cannot be read by a workflow on any other branch. `release-sign.yml` fails if any signing secret is absent, and refuses an APK whose signer is not the release key (`E4:06:…:24:C2` — the other fingerprint in assetlinks.json is the debug key). The Google/Microsoft secrets that used to live here are no longer read by any workflow in this repository; leave them until the first release under the new flow has shipped, then delete them.

### 2. Bump version

Five files — keep the three manifests and their lockfiles in sync:

- `version` in [package.json](package.json)
- root package version in [package-lock.json](package-lock.json)
- `version` in [src-tauri/tauri.conf.json](src-tauri/tauri.conf.json)
- `version` in [src-tauri/Cargo.toml](src-tauri/Cargo.toml)
- Jodd package version in [Cargo.lock](Cargo.lock)

The public build's `preflight` refuses to start unless `tauri.conf.json` and `package.json` match the tag and `CHANGELOG.md` has a section for it.

### 3. Decide on Google OAuth app status

Google Cloud Console → OAuth consent screen.

| Status | When to use | Caveats |
|---|---|---|
| **Testing** | Internal/limited use | Cap 100 test users; refresh tokens expire in 7 days |
| **In Production** | Public release | Requires Google verification (free for `gmail.modify` scope — submit form, ~1-2 weeks) |

### 4. Cut a release

```bash
git tag v0.39.0
git push origin v0.39.0                                  # fires nothing on its own
public-mirror-prep/scripts/sync-to-public.sh v0.39.0     # force-push the snapshot; read the summary, then confirm
scripts/release-dispatch.sh v0.39.0                      # build on Jodd-public, then sign + publish here
```

The order matters. `release.yml` builds whatever Jodd-public's `main` holds, so the sync must come first; the script pushes no tags, and its force-push confirmation is deliberate and must not be automated away. `release-dispatch.sh` refuses to dispatch unless that snapshot is of the tagged commit, then waits on your machine (`gh run watch` — no runner is billed for waiting), then dispatches `release-sign.yml`. **Running it without `--dry-run` is the publication approval**, as pushing the tag was before.

What happens, and what stops it:

1. **`preflight`** (Jodd-public, ~20 s): main must be the snapshot of `upstream_sha`, the version must match the tag, CHANGELOG must have the section, and the tag must not already be published.
2. **Builds**, in parallel: macOS + Windows (with jodd-mcp) into the draft; the unsigned APK as the `android-apk-unsigned` workflow artifact (kept 7 days); the encryption proof. **`stage`** then sets the draft's notes from `CHANGELOG.md` and checks it holds the six desktop + jodd-mcp assets at this commit.
3. **`release-sign.yml`** (private): checks the run is Jodd-public's own `release.yml`, dispatched on `main`, all green, **with the encryption proof job present by name and passed** (a skipped job would still leave the run green); checks the snapshot's tree is hash-identical to the tagged commit's except the paths the sync replaces or strips; signs; checks the signer; attaches the APK; checks all seven assets; publishes. Publishing creates the tag on Jodd-public at the snapshot, and the job confirms it did.

**The Android at-rest-encryption proof gates publication** (gotcha #13). It boots an emulator and runs the SQLCipher round-trip on-device ([android-encryption.yml](.github/workflows/android-encryption.yml), pulled in via `workflow_call` so there is one definition, not a copy). If it fails, the binaries still build and the draft still exists, but `stage` does not run and `release-sign.yml` refuses to publish, so **nothing reaches users**. This was not true for v0.24.1, which published while that job was red because the publishing job did not depend on it.

A re-run after a failure: dispatch again with the same tag. tauri-action reuses the existing draft — but keeps the commit it was first created at, so if you re-synced in between, `stage` and `release-sign.yml` both refuse; delete the stale draft (`gh release delete vX.Y.Z -R BBM-Co-ORG/Jodd-public --yes`) and dispatch again. If only signing failed (a billing refusal, a missing secret), re-run `release-sign.yml` with the same inputs within 7 days, before the APK artifact expires.

The app's What's New shows the same section **without `### Verification
scope`** (`DEVELOPER_ONLY_SECTIONS` in `src/lib/whatsNew.ts`): that one is for
the people shipping Jodd — what was and was not tested — and stays in
CHANGELOG.md and the release notes. Every other section is read in the app, so
write it for users.

The proof also runs on its own — on a pull request that changes `db_crypto.rs`, `backend/ssh` or the Cargo manifests, and on demand:

```bash
gh workflow run android-encryption.yml                          # this repository's branch (billed)
gh workflow run android-encryption.yml -R BBM-Co-ORG/Jodd-public  # the current snapshot (free)
```

**Never sync a ref older than the 2026-10-26 pipeline change while Jodd-public's Release workflow is enabled.** Older snapshots carry the old `release.yml`, which triggers on tag pushes — and publishing a release creates a tag — so it would rebuild into an already-published release (the v0.17.7 asset-pollution trap).

### 5. A dry run (the first release under this flow, and after any pipeline change)

```bash
git checkout main && git pull
public-mirror-prep/scripts/sync-to-public.sh HEAD    # a commit on main
scripts/release-dispatch.sh v0.38.0 --dry-run        # the version in tauri.conf.json at that commit
```

A dry run does everything except publish, into a draft named `vX.Y.Z-dryrun.N` (N = the run number). It needs no private tag, and the version may be one that is already published. Because the APK is still signed with the release key, `release-sign.yml` only signs a dry run of a commit that is already on this repository's `main`. Then:

1. On https://github.com/BBM-Co-ORG/Jodd-public/releases, the draft lists **7** assets; the APK is `Jodd_vX.Y.Z-dryrun.N_android-universal.apk`.
2. Install that APK **over the Jodd already installed** on a test phone (`adb install -r`): Android only accepts an update signed with the same key, so a successful install proves the key; then sign in to Google once to prove the App Links OAuth callback still lands in the app.
3. Read both Actions tabs: Jodd-public's "Release" run and this repository's "Release sign" run.
4. Delete the draft: `gh release delete vX.Y.Z-dryrun.N -R BBM-Co-ORG/Jodd-public --yes` (its tag was never created).

The Actions tab's Run workflow button on Jodd-public defaults to a dry run too.

## Optional but recommended

### Code signing

Without signing, users see:
- **macOS**: "Jodd can't be opened because Apple cannot check it for malicious software" on first launch (right-click → Open works around it)
- **Windows**: SmartScreen blue banner — user clicks "More info → Run anyway"

To sign, add these secrets to Jodd-public's `release` environment (that is where the desktop bundles are built now) and uncomment the matching env block in `release.yml`'s Build & bundle step:

**macOS** (needs Apple Developer membership, ~$99/yr):
| Secret | Notes |
|---|---|
| `APPLE_CERTIFICATE` | base64-encoded `.p12` of Developer ID Application cert |
| `APPLE_CERTIFICATE_PASSWORD` | password of the `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | your Apple ID |
| `APPLE_PASSWORD` | app-specific password (appleid.apple.com → Sign-In and Security → App-Specific Passwords) |
| `APPLE_TEAM_ID` | the Team ID from the Apple Developer portal |

For Apple to actually notarize (required for Gatekeeper to accept silently): with that env block uncommented, `tauri-action` runs notarization when all are present.

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

> **TODO (long-term fix):** add Apple Developer ID + notarization to the release CI. The `APPLE_*` secrets table under [Code signing](#code-signing) above is ready for `tauri-action`; once the secrets land in Jodd-public's `release` environment and `release.yml`'s commented env block is enabled, the workflow notarizes automatically and end users can double-click without terminal commands. Tauri 2 bundle-signing config lives in [tauri.conf.json](src-tauri/tauri.conf.json).

## Pre-release smoke test (run before tagging)

1. **Clean macOS**: use a fresh user account or a VM. No `~/Library/Application Support/jodd`, no Keychain entry.
2. Install the built `.dmg`, open. Should land at AuthScreen.
3. Sign in with a real Google account. Should hit Gmail and start indexing.
4. Quit (Cmd-Q), reopen. Cache paint should show notes immediately; index should refresh in seconds.
5. Remove the account from the in-app panel. Re-add. Should work without restart.
6. Edit a note in Apple Notes (iPhone), wait ~30s, refresh in Jodd. Should show the edit.
7. Edit a note in Jodd, refresh Apple Notes on the iPhone. Should show the edit.
8. **Windows**: same as above on a fresh VM. Watch for SmartScreen, OAuth callback port collisions, font rendering, and sidebar layout at 1366×768.
9. **Android**: install the signed APK (a dry run's draft has one — §5) on a clean arm64 device, complete OAuth through the verified `https://jodd.bbmedia.co.th/oauth2redirect` App Link, then verify folder → notes → editor navigation, system Back, one edit in each direction, and an attachment display.
10. **MCP**: with a test-only folder allowlisted, run `list_accounts`, a bounded search, create/append a note, and tick one checklist row using the exact expected task text. Confirm a write outside the allowlist is refused.

## Known limitations

- **Desktop OAuth callback port `localhost:8080` is hard-coded** in [auth.rs](src-tauri/src/auth.rs). If the port is in use, desktop sign-in hangs with no useful error. Android does not open this listener; it returns through a verified HTTPS App Link.
- **SQLite cache is encrypted at rest (SQLCipher)** as of v0.24.0, at `~/Library/Application Support/jodd/jodd.sqlite3` (macOS) / `%APPDATA%\jodd\` (Windows).
- **Release signing is incomplete on desktop.** macOS is ad-hoc signed and not notarized; Windows is unsigned, so both platforms show first-run trust warnings. Android releases are signed.
