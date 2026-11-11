# In-app updates on every platform — design

Date: 2026-11-11. Status: **draft, awaiting approval.** Replaces the
Android-only updater from PR #315 (ships in 0.46.0).

## 1. Why

Only Android can update itself today. The user asked why, and wants **one**
update feature that works the way other Mac and Windows apps do: Jodd notices a
new version, downloads it and installs it. They explicitly do not want "an
afterthought fix for Android" with desktop bolted on beside it.

What blocks a plain "install everywhere":

- **macOS builds are ad-hoc signed** (`"signingIdentity": "-"`). Every ad-hoc
  build is a different app to the Keychain, so a self-installed update opens
  the Keychain prompt for the Gmail refresh token. A missed or denied prompt
  cost the user their Gmail account on 2026-10-26 (gotcha #46). The user does
  **not** plan to get an Apple Developer ID now (asked 2026-11-11).
- **Windows builds are unsigned.** Windows Credential Manager is not tied to
  the binary, so updating does not prompt. Tauri's official updater plugin
  (NSIS / MSI) is the route.
- **The release pipeline is split across two repositories.** Jodd-public builds
  and holds only OAuth client ids that ship inside the binaries anyway. The
  private repository holds every signing key and is the only thing that
  publishes (`release-sign.yml`).

## 2. Goals, non-goals, anti-goals

**Goals**

1. One frontend feature — one state machine, one banner, one Settings
   section — for Android, Windows and macOS.
2. **Windows: full install.** Check → download → verify → install → Jodd
   reopens on the new version.
3. **macOS: check and notify only**, with a button that opens the release
   page. Self-install sits behind **one switch** in the code
   (`MACOS_SELF_INSTALL = false`). Turning it on later is that switch plus
   desktop signing secrets, not a redesign (§9).
4. **Android: same behaviour as today**, behind the same contract.
5. The release pipeline produces a signed `latest.json` and checks it before
   publishing. A dry run can never be offered as an update.
6. **Auto-check on by default on every platform** (the user's decision,
   2026-11-11): ~10 s after launch, at most once a day, can be turned off.
   `privacy.html` says so.

**Non-goals**

- Code-signing the desktop apps (Developer ID, Authenticode). That is separate
  work, and §9 says what changes once it exists.
- Intel Macs, Linux, iOS: no builds exist.
- Beta or pre-release channels.
- Downloading in the background before the user asks.
- Delta updates.

**Anti-goals — a failure even if it "works"**

- A self-update on macOS while the build is ad-hoc signed (gotcha #46).
- The updater's private key anywhere on Jodd-public, or in any job that runs
  Jodd-public's code. Whoever holds it can push code to every Windows install.
  This is the same rule as the Android keystore.
- Losing an edit because Jodd closed to install.
- A dry-run draft, or a build the private repository did not sign, being
  offered to users.
- A downgrade: a tampered `latest.json` pairing a new version number with an
  older, validly signed installer.

## 3. What the user sees

Same places as today: the bottom banner (`UpdateBanner.svelte`) and
Settings › General › **Updates**. The `{#if $isAndroid}` wrapper goes away.
What changes by platform is only the **action**:

| Platform | Banner when an update exists | The button does |
|---|---|---|
| Android | "Jodd 0.47.0 is available (29 MB)." [Install] [Later] | today's flow, unchanged |
| Windows | "Jodd 0.47.0 is available (12 MB)." [Install and restart] [Later] | downloads with %, then "Installing — Jodd will close and reopen." and Jodd quits; the installer shows a progress bar and reopens Jodd |
| macOS | "Jodd 0.47.0 is available." [Download] [Later] | opens the release page in the browser |

- "Later" hides the banner for that version only. A newer version shows again.
  This is today's `jodd:updateDismissed`.
- Settings shows the same states, plus "Check now" and **Check for updates
  once a day** (on by default), with one short paragraph per platform about
  what happens:
  - Windows: "Jodd downloads the installer from its GitHub release, checks it
    was signed by Jodd's release key, then closes and reinstalls itself."
  - macOS: "Jodd tells you when a new version is out. Installing it yourself
    from the release page avoids a Keychain prompt this build can't skip."
  - Android: today's text.
- **Desktop apps stay open for days.** The daily check runs ~10 s after launch
  and again whenever it falls due while Jodd is running (re-evaluated about
  every hour). Android keeps "after launch" only, as today.
- A failed **automatic** check stays silent (today's `quiet`). A failed manual
  check or install shows its reason.
- If Windows has no installer entry matching how Jodd was installed (§5.2),
  the banner falls back to the macOS behaviour: [Download] opens the release
  page. That is never an error.

## 4. The contract (frontend ⇄ Rust)

Command names stay the same, so nothing else in the app moves:

- `check_app_update() → { current, available: AvailableUpdate | null }`
- `install_app_update(version) → InstallOutcome`
- `request_install_permission() → bool` (Android only; refuses elsewhere, as
  today)
- Progress event `app-update-progress { received, total }`, now emitted on
  Windows too.

```ts
interface AvailableUpdate {
  version: string;
  size: number | null;        // bytes, when the release says
  releaseUrl: string;         // the GitHub release page
  method: 'androidInstaller' | 'desktopInstaller' | 'releasePage';
}
type InstallOutcome =
  | { kind: 'handedOff' }        // Android: the system installer is up
  | { kind: 'needsPermission' }  // Android: Install unknown apps is off
  | { kind: 'restarting' };      // Windows: the installer is running; Jodd exits
```

- `method` is decided **in Rust** by one pure function, `install_method()`,
  from the OS, the switch, and whether the manifest has an entry for this
  install. The frontend never guesses the platform for this.
- Android's `apkUrl` and `sha256` leave the frontend type. They were never sent
  back — `install_app_update` re-reads the release itself — so only the UI
  stops seeing them.
- `releasePage` never calls `install_app_update`. The frontend opens
  `releaseUrl` with the opener plugin. Rust only lets `releaseUrl` be a
  `https://github.com/BBM-Co-ORG/Jodd-public/releases/tag/v…` URL.
- The state machine (`appUpdate.ts`) keeps its phases. It adds `restarting`,
  and `upToDate` / `available` / `downloading` / `error` work for every
  method. `needsPermission` and `handedOff` stay Android-only.

## 5. Rust

### 5.1 Layout

`src-tauri/src/android_update.rs` becomes a module, so the history moves with
the file:

```
src-tauri/src/app_update/
  mod.rs      commands, AvailableUpdate, InstallOutcome, install_method(),
              MACOS_SELF_INSTALL, shared version parsing
  android.rs  today's code, moved (`git mv`) — behaviour unchanged
  desktop.rs  the tauri-plugin-updater path (Windows + macOS)
```

`mod.rs` sends each command to `android` or `desktop` by `cfg`.
`desktop_refusal` goes; `request_install_permission` refuses on desktop with
its own message.

### 5.2 Desktop: tauri-plugin-updater 2.14.x

- **Dependency only on desktop:** `[target.'cfg(not(target_os =
  "android"))'.dependencies] tauri-plugin-updater = "2.14"`, registered only
  there. 2.14.1 is the current stable line; 3.0 is alpha.
- **Called from Rust only.** No `updater:*` permission in
  `capabilities/default.json`, so the webview cannot start an update itself.
- **Config** in `tauri.conf.json`:
  ```json
  "plugins": { "updater": {
    "pubkey": "<base64 minisign public key>",
    "endpoints": ["https://github.com/BBM-Co-ORG/Jodd-public/releases/latest/download/latest.json"],
    "requireSignedVersion": true,
    "windows": { "installMode": "passive" }
  } }
  ```
  - `requireSignedVersion` rejects an installer whose signed version differs
    from what `latest.json` announces. This is the anti-downgrade goal. It is
    safe to turn on from day one because no release was ever signed without
    the version.
  - `passive` shows only the installer's progress bar. The plugin then passes
    NSIS `/P /UPDATE /R` (restart after) or msiexec `/passive
    AUTOLAUNCHAPP=True`.
  - Jodd's NSIS install mode is the default `currentUser`, so updating an NSIS
    install needs no UAC. An MSI install is per-machine, so UAC asks.
- **User-Agent:** `Jodd/<version>`, the same as Android, set as a request
  header. It replaces the plugin's default `tauri-plugin-updater/2.14.1`, so
  privacy.html's wording holds.
- **Jodd reads `latest.json` itself; the plugin installs.**
  - `check_app_update` fetches the endpoint with `reqwest` (already a
    dependency) and parses it in a pure function, `pick_desktop_update(json,
    current, target_keys)`. That function returns the version, the size, the
    release page, and whether there is an entry for this install.
  - `install_app_update` fetches it again, confirms the version, and passes
    that same JSON to the plugin's `Updater::restore_update(raw_json)`. That
    call builds the plugin's `Update` from it and keeps every plugin check:
    signature, `requireSignedVersion`, HTTPS.
  - Why not the plugin's own `check()`: for a newer version with no entry
    for this install type, it returns `Err(TargetsNotFound)` with no version
    attached (plugin `updater.rs:722,758-778`). The banner could then not say
    "Jodd X is available". Parsing the manifest ourselves also makes the
    decision testable with fixtures.
- **Which installer.** The plugin reads how this copy was installed (the
  bundler stamps the bundle type into the binary) and looks up
  `windows-x86_64-nsis` or `windows-x86_64-msi`, then a bare `windows-x86_64`.
  `pick_desktop_update` uses the same order, with the bundle type from
  `tauri::utils::platform::bundle_type()`. **`latest.json` deliberately has no
  bare key.** A copy whose install type is unknown then gets `releasePage`
  (§3) rather than running the wrong installer kind over its install.
- **MSI installs update themselves too** (my call). The plugin starts
  `msiexec` and exits at once, and UAC then asks inside msiexec. If the user
  declines, Jodd is already closed and nothing reopens it. They open the old
  version again and the banner returns: nothing is lost, only a restart. The
  banner adds "Windows will ask for permission" on an MSI install, and the
  live check covers a declined UAC.
- **Defence in depth**, mirroring Android's `pick_update`, in a pure function
  `vet_desktop_update(version, download_url)`:
  - The URL must be `https://github.com/BBM-Co-ORG/Jodd-public/releases/download/v{version}/`
    followed by one of the expected file names.
  - The version must parse as a plain `X.Y.Z`.

  Otherwise the command refuses. The signature check is the real guarantee;
  this one stops a manifest that points anywhere else.
- **`install_app_update(version)`** re-checks, as Android does. It refuses if
  the latest version is no longer `version`, then downloads with progress
  events, verifies (the plugin does this) and installs. One install at a time
  (same `AtomicBool` guard as Android).
  - On Windows `install()` launches the installer and calls
    `std::process::exit(0)`. The command therefore emits `restarting` as an
    event **before** calling it, because its return value never arrives.
  - On macOS with the switch off, the command refuses ("macOS updates install
    from the release page") even if called.
- **Size:** read by `pick_desktop_update` from `latest.json`'s per-platform
  `size` field. The plugin ignores unknown fields (`ReleaseManifestPlatform`
  has no `deny_unknown_fields`). Missing → `null`.

### 5.3 The switch

```rust
/// Off until macOS builds are signed with a Developer ID: an ad-hoc build
/// that replaces itself is a new app to the Keychain, and the refresh-token
/// prompt it triggers cost the Gmail account on 2026-10-26 (gotcha #46).
pub(crate) const MACOS_SELF_INSTALL: bool = false;
```

A unit test pins it to `false` and names §9, so turning it on is a deliberate,
reviewed diff. `install_method(Os::MacOs, …)` returns `releasePage` while it
is off.

### 5.4 Android

Moved, not changed: the same GitHub API check, APK asset rule, size and SHA-256
checks, Package installer hand-off, permission flow and pruning.

- `pick_update` keeps returning an internal Android struct, still carrying
  `apk_url` and `sha256`. `mod.rs` maps it to the public `AvailableUpdate`.
  So its tests move unchanged.
- The one exception is `the_commands_refuse_on_desktop`. It tests
  `desktop_refusal`, which goes, so it is replaced by a test of
  `request_install_permission`'s desktop refusal.

## 6. No lost edits, no duplicates, when Windows closes Jodd

The Windows installer path ends in `exit(0)`. SQLite commits are durable, so a
dirty row that was **not yet sent** is just pushed after the restart. Two
things are not safe, and both must be handled before `install()`:

1. **An edit still inside the editor's 1.5 s autosave debounce.**
   - Before calling `install_app_update` for a `desktopInstaller`, the
     frontend asks the open editor to write any pending edit to SQLite and
     **awaits** that write. If the write fails, the install does not start,
     and the error says why.
   - How the editor exposes the flush is the builder's choice; the existing
     `flushPendingEdit` path is the starting point.
   - A vitest proves install waits for the flush and stops on a failed one.
2. **A push already on the wire.** Only Gmail CREATE has a lost-answer guard
   (gotcha #39). On Microsoft, Exchange assigns the identity at CREATE
   (gotcha #16). An `exit(0)` after the POST but before the rekey makes the
   next run create the note again — a duplicate.
   - So `install_app_update`, after the download verifies and before
     `install()`, takes `sync_schedule.exclusive(account_id)` for **every**
     account, with a 30 s overall deadline, and holds the guards until the
     process exits.
   - That waits out any round in flight and admits no new one.
   - At the deadline it releases them and refuses: "Jodd is still sending
     changes — try again in a minute". The download stays cached, so the
     retry is instant.
   - A Rust test drives this with a held lease.
   - Pushing everything to the remote first is **not** required.
3. **Long user-started jobs.**
   - **Backup and restore:** install refuses while one runs.
   - **Ask, Studio and Extract runs** are cut off exactly as a normal quit
     would cut them; the banner's "Jodd will close" says so.

   How "a backup is running" is detected is the builder's choice; there is
   one source of truth for it, or the builder adds one.

## 7. Release pipeline

### 7.1 Keys

- A new minisign key pair just for updates, created with
  `tauri signer generate`, password-protected.
- **The user generates it** (my call: they said they had no preference). The
  password is typed into their own terminal, so it never passes through an
  agent. They back up the private key and password, the same way as the
  Android keystore, because losing them means installed copies can never
  update again.
- Secrets `TAURI_SIGNING_PRIVATE_KEY` and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` go on the private repo as
  **environment secrets of `android-signing`**, the environment
  `release-sign.yml`'s job already runs in.
  - Not repository secrets: a workflow on any branch can read those.
  - The user also limits that environment's deployment branches to `main`.
    That is one settings change, included in the commands the plan gives
    them, and done before the first signed dry run.
  - The environment keeps its name: renaming it would move the Android
    secrets too.
- The public key goes in `tauri.conf.json`, **and** its key id is pinned in
  `release-sign.yml`, as `RELEASE_CERT_SHA256` is for Android. A change to the
  config file alone cannot change what the workflow accepts.
- **The PR cannot merge until the public key is in it.** The plan includes the
  exact commands for the user.

### 7.2 Jodd-public build (`release.yml`)

- **No signing there, and no `createUpdaterArtifacts`.**
  - On Windows, Tauri 2 updates with the very same `-setup.exe` / `.msi` it
    ships, so the private repository can sign those finished files afterwards.
  - Turning on `createUpdaterArtifacts` would make the public build demand the
    private key.
- New: right after `tauri-action`, each desktop leg writes the SHA-256 of its
  bundle files (`.exe`, `.msi`, `.app.tar.gz`) and uploads them as a workflow
  artifact `desktop-digests-<os>`. A workflow artifact can only come from that
  run, so release-sign can later prove a draft asset is the one this build
  produced, not one swapped in afterwards by someone with write access on
  Jodd-public.

### 7.3 Private repo signs and publishes (`release-sign.yml`)

New steps, after the APK is attached and **before** Publish. Like everything
already there, they run for dry runs too, minus publishing:

1. **Download** `Jodd_{V}_x64-setup.exe`, `Jodd_{V}_x64_en-US.msi` and
   `Jodd_aarch64.app.tar.gz` from the draft. Check each file's SHA-256
   against the `desktop-digests-*` artifacts of the same run, and against the
   asset's GitHub `digest`.
2. **Sign** each with the Tauri CLI at the version in the repo's
   `package-lock.json`, then delete the key material straight after, as for
   the keystore.
   - Install with **`npm ci --ignore-scripts`**, so no dependency's install
     script runs in a job that holds signing keys. The lockfile's integrity
     hashes pin the CLI and its native binary.
   - Sign with `node_modules/.bin/tauri signer sign --app-version {V}`.
   - The macOS tarball is signed too, though no build installs it yet, so §9
     needs no pipeline change.
3. **Build `latest.json`** with `scripts/updater-manifest.mjs` (new, tested):
   ```json
   {
     "version": "0.47.0",
     "pub_date": "<UTC RFC 3339>",
     "notes": "<CHANGELOG section for 0.47.0, without ### Verification scope>",
     "platforms": {
       "windows-x86_64-nsis": { "url": ".../download/v0.47.0/Jodd_0.47.0_x64-setup.exe", "signature": "...", "size": 0 },
       "windows-x86_64-msi":  { "url": ".../download/v0.47.0/Jodd_0.47.0_x64_en-US.msi", "signature": "...", "size": 0 },
       "darwin-aarch64-app":  { "url": ".../download/v0.47.0/Jodd_aarch64.app.tar.gz",  "signature": "...", "size": 0 }
     }
   }
   ```
   - URLs use the **release tag**, so a dry run's manifest names its own
     draft.
   - `notes` reuses the parser What's New uses:
     - `scripts/changelog-parse.mjs` gets the section text.
     - `DEVELOPER_ONLY_SECTIONS` moves from `src/lib/whatsNew.ts` into
       `changelog-parse.mjs`, and `whatsNew.ts` imports it from there.
     - So there is one list, not two.
4. **Verify before upload**, failing the job on any miss:
   - `minisign -V` (Ubuntu's `minisign` package) for each file against the
     pinned public key.
   - The signed trusted comment carries `version:{V}`.
   - The manifest's own `--check`: version = `{V}`; each URL is the
     Jodd-public download path for this tag and names an asset that exists in
     the draft; each `signature` equals the `.sig` just made; each `size`
     equals the asset's size.
5. **Upload `latest.json`** to the draft.
6. `scripts/release-expected-assets.sh` learns `latest.json`. The signed
   draft holds **8** assets, not 7. Update `release-dispatch.sh`'s message,
   RELEASE.md and the `stage` check, which keeps checking six.

Then Publish runs as today. `latest.json` is in the draft before it goes
public, so the manifest and the release appear together.

**Why a dry run is never offered:** the endpoint is
`/releases/latest/download/latest.json`, and GitHub's "latest" never resolves
to a draft. Dry runs are drafts that are never published (`release-sign.yml`
already refuses `publish` for a `-dryrun.N` tag). A test pins the endpoint
string.

### 7.4 What the first updater release does not fix

Copies older than the first release with this feature have no desktop
updater, so users install that one release by hand. The CHANGELOG entry says
so.

## 8. Testing

- **Rust**
  - `install_method` for every (OS, switch, manifest entry) case.
  - `MACOS_SELF_INSTALL == false`.
  - `vet_desktop_update` accepts the right URL and refuses another host,
    another tag folder or a malformed version.
  - `pick_desktop_update` covers:
    - newer with an entry → `desktopInstaller`;
    - newer with no entry for this install → `releasePage` with the version;
    - same or older → none;
    - a malformed manifest → error;
    - size present or absent.
  - The install quiesce: a held account lease delays install, and the
    deadline refuses it.
  - A config test: `tauri.conf.json`'s updater `pubkey` decodes as a minisign
    public key whose id equals the one pinned in `release-sign.yml`; the
    endpoint is exactly the `releases/latest/download/latest.json` URL;
    `requireSignedVersion` is `true`.
  - Every existing Android test, moved.
- **Vitest** (`appUpdate.test.ts`)
  - Each method's path through the state machine.
  - Auto-check now runs on every platform; desktop re-checks when due.
  - `releasePage` opens the URL and never invokes install.
  - Install waits for the editor flush, and does not start after a failed one.
  - "Later" still holds per version.
- **Node** (`scripts/updater-manifest.test.mjs`): builds the manifest from
  fixtures, and `--check` refuses each defect in §7.3 step 4. The fixture is a
  throwaway key and signature made for the test, labelled as such, never the
  release key.
- **The CI gate:** gen-changelog, `cargo test --workspace --locked`, vitest,
  svelte-check, `npm run build`.

## 9. Turning on macOS self-install later

All of it, so it stays a configuration change:

1. Developer ID: add the `APPLE_*` secrets to Jodd-public's `release`
   environment, and uncomment the env block in `release.yml` (RELEASE.md, Code
   signing).
2. Flip `MACOS_SELF_INSTALL` to `true` and update its test.
3. Expect **one** Keychain prompt on the first Developer-ID build. Items
   created by ad-hoc builds belong to no team. After that, signed builds share
   the identity, and updates stop prompting.
4. Before shipping, prove the macOS path once:
   - The plugin accepts tauri-action's `Jodd_aarch64.app.tar.gz` layout.
   - The update replaces `/Applications/Jodd.app` in place.

   Neither has been exercised (the switch is off).

No pipeline change: the tarball is already signed and listed in `latest.json`
(§7.3).

## 10. Privacy and docs

- **Jodd-Homepage PR** (the user merges; main is live). `privacy.html` §2
  `#updates` currently says "Android only". It changes to: every platform
  checks GitHub; the daily check is on by default; Windows downloads the
  installer from GitHub; macOS only opens the release page; the request names
  only `Jodd/<version>`.
- **In this repo:** RELEASE.md (secrets table, key backup, 8 assets, replace
  the "Auto-updater: skip" paragraph); ARCHITECTURE.md key-files line; the
  CHANGELOG entry.

## 11. Live-check plan (owed after merge)

The install path can only be proven across two published versions. The order:

1. **Key and secrets:** the user generates the key, commits the public key,
   and sets the two secrets.
2. **Signed dry run** (`release-dispatch.sh vX.Y.Z --dry-run`):
   - The draft lists 8 assets.
   - Download `latest.json` and `-setup.exe`, and verify the signature
     locally with `minisign -V`.
   - On a Windows machine, run the draft's `setup.exe` by hand, and note
     SmartScreen's behaviour for a browser-downloaded unsigned installer.
3. **Release A**, the first with the updater:
   - Install it by hand on Windows (NSIS, and an MSI copy if available) and on
     macOS.
   - Android updates to it through the existing path, which proves Android is
     unchanged.
4. **Release B:**
   - Windows with A installed, signed in to Gmail:
     - The banner shows within ~10 s.
     - Type into a note and click Install within 1.5 s. Jodd closes, the
       progress bar shows, and Jodd reopens on B.
     - The edit is there. No credential prompt. Still signed in.
     - Note whether SmartScreen appears for the in-app download. It is
       expected not to, since the file carries no Mark-of-the-Web, but that
       is unverified.
   - The MSI copy: UAC asks, then it installs and reopens. Repeat with UAC
     declined: Jodd stays closed, reopening it shows A and the banner again,
     and nothing is lost.
   - Click Install while a Microsoft note is being created: no duplicate
     after the restart (§6).
   - macOS with A installed: the banner shows; Download opens B's release
     page; no Keychain prompt.
   - Turning the daily check off stops the automatic request. Confirm it in
     the network log or with a proxy.

**Not verified on a real Windows machine before merge:** the whole runtime
path. That covers NSIS passive install and relaunch, MSI, the bundle-type
lookup, SmartScreen, the `restarting` event arriving before exit, and the
editor flush racing the exit. CI proves only the pure logic, the config
contract and the manifest.

## 12. Calls I made (check these)

- Desktop uses the official plugin; Android keeps its own GitHub-API path.
  Not one transport for all three: the plugin does not support Android, and
  Android's path is proven on devices.
- The private repo signs after the build, rather than `createUpdaterArtifacts`
  at build time, so the key never meets Jodd-public.
- `requireSignedVersion` on from the first release.
- `latest.json` has no bare `windows-x86_64` key, so an unknown install type
  falls back to the release page.
- The macOS tarball is signed and listed now, though no build uses it yet.
- Public build digests act as a provenance check on draft assets.
- One click on Windows ("Install and restart"), no separate download-then-
  restart step.
- Desktop re-evaluates the daily check about every hour while open.
- You generate the key; I commit only the public half.
- Jodd parses `latest.json` itself and installs through the plugin's
  `restore_update`, rather than the plugin's `check()` (which cannot report
  the version for the release-page fallback).
- The install waits up to 30 s for in-flight pushes on every account, then
  refuses. It is refused during backup and restore. Ask and Studio runs are
  cut off as on a normal quit.
- MSI copies self-update. A declined UAC leaves Jodd closed until you open it
  again, with nothing lost.
- The updater secrets go on the `android-signing` environment, limited to
  `main`. The signing job installs the CLI with `--ignore-scripts`.

## 13. Changed during implementation (2026-11-11, after review)

- **The desktop install is two commands, not one.** The branch review found
  that flushing the editors once, before the download, let text typed during
  the download (seconds to minutes) miss SQLite when `exit(0)` came. Now
  `download_app_update(version)` downloads and verifies with the editors open
  and keeps the file in memory; the frontend then makes every editor
  read-only (`editorsLocked`), saves them, and calls `install_app_update`,
  which quiesces and installs. A refused install keeps the file, so Try again
  does not download again (what §6 already promised). Android is unchanged:
  `download_app_update` refuses there.
- **The backup/restore check runs again once the lanes are held:** a restore
  takes no sync lane, so one started during the 30 s wait was not refused.
- **Known gap, not fixed:** `release.yml` records GitHub's digest of each
  draft asset, not a hash computed from the bundle on the runner. It proves
  the asset did not change between that step and signing, not that it is the
  byte-for-byte file tauri-action built.
