# Android bring-up — Sub-project 1 (headless core)

> **Status:** design approved 2026-07-27, not yet implemented.
> **Scope:** make the Rust core run on Android. **No UI work.**
> Mobile UI shell is Sub-project 2; APK release pipeline is Sub-project 3.

## Goal & framing

Jodd exists to put Apple Notes on non-Apple devices. Android is therefore the
product-relevant mobile target (on iOS, Apple Notes is already native, so a
Jodd iOS app would only add multi-account / wiki-graph / extract — not access).
`TODO.md` has carried "Android version — needs OAuth redirect rework (no
localhost callback on mobile)" since before this spec; this is that work.

This sub-project deliberately ships an app that **looks wrong** — the desktop
three-pane layout rendered at phone width — but **works**. The split is possible
because of the project's own local-first doctrine: the UI reads SQLite through
`notes.ts` stores and never touches Gmail or the platform directly. Once the
Rust core runs on Android, the existing frontend runs unmodified. What breaks on
Android is exclusively the code that bypasses an abstraction and calls the host
OS directly.

**Success criterion (single, binary):** sideload the APK on a physical Android
device → sign in with Google → notes populate SQLite → edit a note on the phone
→ **the edit appears in Apple Notes on iPhone**. UI ugliness is accepted.

## Decisions locked in brainstorming (2026-07-27)

1. **Android first**, iOS not in scope at all.
2. **Scope = read + write + sync** (core note-taking parity), not read-only,
   not full desktop parity.
3. **Distribution = sideload APK**, own keystore. No Play Store, therefore no
   Google OAuth app verification and no CASA assessment.
4. **OAuth = deep link + Android OAuth client type** (§3). BYO credentials
   stays desktop-only in v1.
5. **Secrets = `keyring` 4 + `android-native-keyring-store`**, behind a
   mandatory on-device verification gate; **fallback = encrypted file in the
   app-private dir** if the gate fails (§2). **Superseded — see the note at
   the top of §2. What shipped is `keyring-core` 1 directly, not `keyring` 4.**

## Findings that drive the design (verified, not inferred)

These were each confirmed against the actual source before designing:

| # | Finding | Evidence |
|---|---|---|
| F1 | `keyring` 2.3.3 has **no Android backend** and silently falls back to an in-memory `mock` | `keyring-2.3.3/src/lib.rs:185-193` — `#[cfg(not(any(linux, freebsd, openbsd, macos, ios, windows)))] use mock as default;`. Crate has no `android.rs`. |
| F2 | `keyring` **4.1.5 does** have an Android backend | `keyring-4.1.5/Cargo.toml` — `[target.'cfg(target_os = "android")'.dependencies.android-native-keyring-store] version = "1.0.0"` |
| F3 | That backend encrypts `SharedPreferences` with Android Keystore keys and expects `ndk-context` to already be initialized — which **Tauri Mobile does** | `android-native-keyring-store-1.0.0/README.md`; crate contains `keystore.rs`, `cipher.rs`, `shared_preferences.rs` |
| F4 | The OAuth callback listener is a **blocking** `recv()` on a spawned task | `auth.rs:186-188` (`tiny_http::Server::http("0.0.0.0:8080")`, `server.recv()`), spawned at `lib.rs:393` |
| F5 | Token exchange **requires `client_secret`** today (Desktop client type) | `auth.rs:130`, `auth.rs:161` |
| F6 | `accounts::load_accounts()` and `Db::open()` run **before** `tauri::Builder::default()` | `lib.rs:4196`, `lib.rs:4204`, builder starts `lib.rs:4229` |
| F7 | Five call sites reach the OS filesystem directly | `accounts.rs:188`, `oauth_config.rs:14`, `applog.rs:44`, `applog.rs:57`, `lib.rs:4204` |
| F8 | `tauri-plugin-deep-link` 2.4.9 supports Android custom schemes with `appLink: false`, exposes `on_open_url` / `get_current` | Tauri v2 docs, `plugin/deep-linking.mdx` |
| F9 | `RunEvent::Opened { urls }` is documented as available on Android; `Resumed`/`Paused` mapping to Android activity lifecycle is **not** documented | Tauri v2 docs, `learn/mobile-file-associations.mdx` |

**F1 is the most dangerous finding in this spec.** On Android,
`keyring::Entry::set_password()` returns `Ok(())` and writes to a mock that
dies with the process. No compile error, no runtime error, no log line. The
observable symptom is "must sign in again every launch" — a symptom that points
away from its cause. The design must make this state unreachable, not merely
avoided (§2).

## §0 Prerequisites — toolchain

Machine state at design time: JDK 17 ✅, Android SDK at `~/Library/Android/sdk` ✅,
**NDK ❌ absent**, Rust Android targets ❌ none installed, `ANDROID_HOME` /
`NDK_HOME` / `JAVA_HOME` ❌ unset, `tauri-cli` 2.11.2 ✅, `src-tauri/gen/android/`
❌ not scaffolded.

Steps:

1. Install the NDK via `sdkmanager`.
2. Export `ANDROID_HOME`, `NDK_HOME`, `JAVA_HOME`.
3. `rustup target add aarch64-linux-android armv7-linux-androideabi
   i686-linux-android x86_64-linux-android`.
4. `npx tauri android init` → generates `src-tauri/gen/android/`.

**`gen/android/` is committed to git** (build outputs excluded). `.gitignore`
currently ignores `src-tauri/gen/schemas/` only, so a new rule is needed for
the Android build dirs but not the project files. Rationale: CI must build
reproducibly, and `AndroidManifest.xml` needs a hand edit
(`android:allowBackup="false"`, §2).

**Keystore:** one keystore, `jodd-android.keystore`, used by both local dev
builds and CI. Reason: a Google Android OAuth client binds to exactly one
package name + one SHA-1 fingerprint, so two keystores would mean two client
IDs and build-time selection between them. One keystore keeps a single OAuth
client. Trade-off accepted: the keystore must exist locally and in CI secrets.
Never committed.

## §1 Platform paths seam — new `paths.rs`

**Problem.** Per F7, five sites call `dirs::config_dir()` / `dirs::data_dir()`
directly. On Android these do not resolve to the app-private sandbox.

**Design.** A new `src-tauri/src/paths.rs` holding two `OnceLock<PathBuf>`
(config base, data base), initialized exactly once inside `.setup()` from
`app.path().app_config_dir()` and `app.path().app_data_dir()`. All five sites
call `paths::config_base()` / `paths::data_base()` instead of `dirs::*`.

This is not a new pattern for the project — `applog.rs` already splits
`settings_path_under(&base)` / `log_file_path_under(&base)` away from the
`dirs::`-calling wrapper so tests can inject a tempdir. §1 extends that same
shape to `accounts.rs` and `oauth_config.rs`, which become unit-testable as a
side effect.

**Consequent refactor (the non-obvious part).** Per F6, `load_accounts()` and
`Db::open()` currently run before the Tauri builder exists, but `app.path()` is
only available after it. Therefore:

- Move `accounts::load_accounts()` and `db::Db::open()` into `.setup()`.
- `AppState` construction moves into `.setup()`; `app.manage(app_state)` is
  called there rather than via `.manage()` on the builder.
- `paths::init(app)` runs first inside `.setup()`, before anything reads a path.

Desktop behavior must not change. `app.path().app_config_dir()` on macOS
resolves under `~/Library/Application Support/<identifier>`, whereas the current
`dirs::config_dir().join("jodd")` resolves to `~/Library/Application
Support/jodd`. **These differ** (`co.bbmedia.jodd` vs `jodd`). To avoid
orphaning existing users' `accounts.json`, desktop keeps its current literal
path: `paths::config_base()` returns the legacy `dirs::config_dir().join("jodd")`
on desktop and the Tauri-resolved app dir on Android. The seam exists to make
the platform difference explicit and testable, not to relocate desktop data.

## §2 Secrets — upgrade `keyring` 2 → 4

> **SUPERSEDED (recorded once the implementation proved this approach cannot
> work — do not build against the design below).** `keyring` 4's `v1`
> compatibility shim — the API surface this section is written against — never
> registers a credential store on Android at all: every arm of the shim's
> `set_credential_store()` excludes `target_os = "android"`. Calling
> `keyring::Entry::new` on Android under this design would still resolve to
> the same silent in-memory `mock` that F1 warns about with keyring 2 — the
> `android-native-keyring-store` dependency below would sit in `Cargo.toml`
> unused, because nothing in the `v1` shim ever calls into it. This was not
> caught by inspection; it surfaced when Sub-project 1 was actually built.
>
> What shipped instead: `keyring-core` 1 directly (the API layer `keyring` 4
> itself is built on), with an explicit per-platform `CredentialStore` chosen
> in `src-tauri/src/secrets.rs::platform_store()` — `apple-native-keyring-store`
> on macOS, `windows-native-keyring-store` on Windows,
> `zbus-secret-service-keyring-store` on Linux, and `android-native-keyring-store`
> (the same crate F3 identified) on Android, registered via
> `keyring_core::set_default_store()` behind a `std::sync::Once` guard in
> `secrets::init()`. Bypassing the `v1` shim also sidesteps a second defect
> the shim has: it initializes lazily behind an `AtomicBool` that lets a late
> thread skip initialization rather than wait, so concurrent first callers can
> race into `NoDefaultStore` even on desktop
> (`keyring-4.1.5/src/v1.rs:47` — see `secrets.rs`'s
> `concurrent_first_calls_all_succeed` test, which guards exactly this).
> Call-site count: 13 across three files (7 in `accounts.rs`, 3 in
> `oauth_config.rs`, 3 in `app_llm_config.rs` — the last added after this spec
> was written, for the app-level LLM provider's API key), not the ten this
> section originally counted.
>
> The rest of this section is left as originally written, for the historical
> record of what was planned and why it looked sufficient at the time.

**Design.**

```toml
keyring = { version = "4", features = ["v1"] }

[target.'cfg(target_os = "android")'.dependencies]
android-native-keyring-store = "1"
```

The `v1` feature preserves the `keyring::Entry::new(service, key)` API used at
all ten existing call sites (seven in `accounts.rs`, three in
`oauth_config.rs`), so the desktop code is untouched. On Android, the store is registered explicitly at
startup (keyring 4 uses a "set the default store" model rather than
compile-time platform selection).

Note that the `v1` feature alone pulls only the Apple / Windows / secret-service
stores — it does **not** pull the Android store. The Android registration is
therefore a required, explicit step, not something the feature flag does for us.

**Making F1 unreachable.** Add a compile-time guard on Android asserting that
the Android store crate is present, and a startup assertion that the default
store was successfully registered before any credential read/write. A silent
fall-through to `mock` must fail loudly rather than degrade.

**Verification gate (blocking).** `android-native-keyring-store` is version
1.0.0, and its claim that Tauri Mobile pre-initializes `ndk-context` is
secondhand. Before any other work builds on it:

> Write a refresh token → force-stop the app from Recents → relaunch → read the
> token back. Must return the identical value.

**If the gate fails, the fallback is an encrypted file in the app-private dir**
(`/data/data/co.bbmedia.jodd/`), plus `android:allowBackup="false"` in the
manifest. The app-private dir is already sandboxed per-app by Android and is
unreadable by other apps absent root, so this is the same protection level most
apps rely on. Weaker than Keystore-wrapped, and this is a knowingly accepted
trade-off for v1 sideload, not an oversight. `allowBackup="false"` is set in
**both** branches so the token never leaves the device via Android Backup.

## §3 OAuth — deep link replaces the loopback listener

**Why the existing flow cannot be carried over.** The usual objection is that
Google documents the loopback flow for desktop OSes only. The stronger,
concrete objection is process lifecycle: launching the system browser sends
Jodd to the background, and per F4 the callback listener is a *blocking*
`server.recv()` on a spawned task. Android freezes backgrounded processes, so
the listener may never respond and the sign-in hangs with no error. A deep link
arrives as an Intent that **brings the app back to the foreground**, which is
precisely why the platform mandates it.

**Design.**

- Add `tauri-plugin-deep-link` 2.4.9. Configure a custom scheme in
  `tauri.conf.json` under `plugins.deep-link.mobile` with
  `scheme: ["co.bbmedia.jodd"]`, `appLink: false` (no domain verification, no
  hosting).
- `redirect_uri` becomes `co.bbmedia.jodd:/oauth2redirect` on Android;
  `http://localhost:8080/callback` remains on desktop. `auth.rs` gains a
  platform-conditional `redirect_uri()` in place of the
  `pub const REDIRECT_URI`.
- Register an **Android**-type OAuth client in Google Cloud Console against
  package `co.bbmedia.jodd` and the SHA-1 of `jodd-android.keystore`.
- Android OAuth clients have **no client secret**. Per F5, `exchange_code` and
  `refresh_access_token` currently always send `client_secret`; both must omit
  it when the target is Android.
- The code-receiving path changes; **the security machinery does not**. PKCE
  (`PkcePair`, verifier/challenge, `code_challenge_method=S256`) and the
  constant-time `state` CSRF check at `lib.rs:407` are reused verbatim. The
  deep-link handler parses `code` and `state` from the incoming URL and feeds
  the same `pending_pkce` comparison.
- Handle both delivery cases: app already running (`on_open_url`) and app
  launched cold by the redirect (`get_current` at startup).
- `tiny_http` and `wait_for_callback()` are retained for desktop only, behind
  `#[cfg(not(target_os = "android"))]`.

**Accepted consequences.**

1. **BYO credentials becomes desktop-only in v1.** An Android OAuth client is
   bound to package name + SHA-1, so a BYO user would have to register *our*
   package and fingerprint in *their* Cloud project. Android uses the embedded
   client ID. The BYO section of App Settings is hidden on Android (§4).
2. One keystore for dev and CI (§0), so one client ID.

## §4 Feature gating

Gate in Rust with `#[cfg(target_os = "android")]`, and expose a `platform`
value to the frontend so menu entries can be hidden rather than failing when
tapped.

| Disabled on Android | Reason |
|---|---|
| LocalFS vertical (`add_local_account`, `rename_local_account`) | No arbitrary filesystem access; Android uses SAF, a different model |
| `ClaudeCodeProvider` (`which` crate + subprocess) | Cannot spawn child processes |
| `tauri-plugin-dialog` folder picker | Does not map to Android |
| BYO OAuth UI in `AppSettings.svelte` | Per §3 |

The HTTP LLM provider is **not** disabled — it is `reqwest` over HTTPS and works
unchanged. (Its UI surface, the Extract modal, is Sub-project 2's concern.)

## §5 Sync lifecycle — surviving Doze

**Problem.** `spawn_sync_worker` (`lib.rs:3449`) is `loop { sleep(SYNC_INTERVAL);
tick() }`. When Android freezes the process, the loop stops and rows left in
`sync_state = dirty` stay unpushed with no signal to the user. On desktop this
case does not exist.

**Design (v1 — deliberately no WorkManager).**

- A new `flush_sync` Tauri command runs one `sync_worker_tick` immediately.
- The frontend drives it: `visibilitychange` and `pagehide` on the WebView →
  `invoke('flush_sync')` on the way out, and again on the way back in
  (catch-up-on-resume, rather than waiting out the remaining sleep).
- **Why the WebView and not Rust `RunEvent`:** per F9, `RunEvent::Opened` is
  documented on Android but `Resumed`/`Paused` mapping to Android activity
  lifecycle is not. `document.visibilitychange` is a WebView-guaranteed signal
  and is therefore the more reliable primary mechanism. If a Rust-side lifecycle
  event turns out to be available on Android during implementation, it may be
  added as a belt-and-braces secondary trigger — not as a replacement.
- The existing 5s loop is unchanged and remains the foreground mechanism.

**Stated limitation, not a bug:** with the app closed there is no background
sync. Deferred to v2 (WorkManager). This must be written down in user-facing
release notes so "my phone edit didn't sync until I opened the app" reads as
known behavior.

## Verification

Ordered; each gate blocks the next.

1. **Toolchain** — `npx tauri android build` produces an APK.
2. **Secrets gate (§2)** — write token → force-stop → relaunch → read back
   identical. Blocking; determines whether the keyring path or the encrypted-file
   fallback ships.
3. **Paths** — `accounts.json`, `google_oauth.json`, `jodd.sqlite3`, and
   `logs/jodd.log` all land under the app-private dir on device; desktop paths
   are byte-identical to pre-change (regression check on macOS).
4. **OAuth** — sign-in completes from a cold app launch and from a warm one;
   `state` mismatch is still rejected.
5. **Round-trip (the success criterion)** — edit on Android → appears in Apple
   Notes on iPhone; edit in Apple Notes → appears on Android.
6. **Lifecycle** — make an edit, immediately background the app, confirm the
   push completed (dirty row cleared) rather than stranding.
7. **Desktop non-regression** — full existing `cargo test` + `npm test` pass;
   macOS bundle still has exactly one binary in `Contents/MacOS/`
   (the standing constraint from CLAUDE.md edge #5).

## Implementation order (phases)

1. §0 toolchain + `tauri android init` + keystore. Ends with an installable APK
   of the unmodified app (expected to fail at sign-in — that is the point).
2. §2 secrets gate. Isolated spike; decides keyring vs. encrypted file.
3. §1 `paths.rs` + the `.setup()` lifecycle move. Desktop-testable, no Android
   needed to validate.
4. §3 OAuth deep link.
5. §4 feature gating.
6. §5 lifecycle flush.
7. Verification ladder end to end.

Phases 2 and 3 are independent and may be done in either order; 4 depends on 1
and 3; 5 and 6 depend on 4.

## Out of scope (explicitly not in this sub-project)

Responsive layout, stack navigation, long-press / touch affordances, mobile
editor and soft-keyboard handling, bottom sheets (all → Sub-project 2). CI APK
build, signing in CI, Play Store (→ Sub-project 3). Background sync while the
app is closed (→ v2, WorkManager). iOS (not planned). Attachment authoring,
cross-account move, and every other item already on the roadmap remain
unaffected.

## Open questions (non-blocking)

- Exact keyring 4 store-registration API — resolved during phase 2's spike; the
  design does not depend on which form it takes.
- Whether a Rust-side Android lifecycle event exists to supplement §5 — a
  bonus if so, not required.
- Whether `gen/android/` needs manual `AndroidManifest.xml` edits beyond
  `allowBackup="false"`; the deep-link plugin is expected to generate its own
  intent-filter from `tauri.conf.json`.
