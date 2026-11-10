# SSH accounts on Android — an in-process `RusshSession`

**Date:** 2026-09-29
**Status:** approved in brainstorming, awaiting spec review
**Builds on:** [2026-09-26-ssh-storage-backend-design.md](2026-09-26-ssh-storage-backend-design.md)
(the `SshSession` seam, §2 "Three implementations") and
[2026-09-27-ssh-easy-setup-design.md](2026-09-27-ssh-easy-setup-design.md)
(Managed accounts, #103, merged as 22f8ee1). Read
[docs/BACKEND-SSH.md](../../BACKEND-SSH.md) first.

## Why

v0.30.0 shipped the SSH storage backend desktop-only. On Android the account
panel offers only Gmail, Microsoft and iCloud: `Sidebar.svelte` hides both
SSH entries behind `{#if !$isAndroid}`, and `add_ssh_account` /
`ssh_setup_managed` return `Err("SSH accounts are desktop-only for now.")`
under `cfg(target_os = "android")`. The reason is the transport:
`session::ProcessSession` spawns the system `ssh` binary, and #103's setup
pipeline additionally spawns `ssh-keygen`, `ssh-keyscan`, and Jodd itself as
`SSH_ASKPASS`. Android has none of those binaries.

**Success:** on the Galaxy S23 FE (phone layout) and Galaxy Tab S7 (tablet
layout), a user with a VPS host, username and password adds an SSH account
through the same three-screen dialog desktop uses, and it syncs like any
other SSH account — surviving sleep, network changes and process death.

## Decisions taken in brainstorming (2026-09-28)

| # | Question | Decision |
|---|---|---|
| 1 | Credential methods on Android | **Password only.** Key-file import on Android is a later item (content-URI reading, in-process decryption, `.ppk` detection) |
| 2 | Setup UI | **#103's three screens, same wording**, one component with a platform flag; the Key-file radio and the "Use my own ssh setup (advanced)" link are hidden on Android |
| 3 | Transport | **One persistent russh connection per account, one exec channel per request, reusing the existing framing and parser** (the other two options — a long-lived `sh` channel behind a generic `ProcessSession`, and native per-channel stdout/stderr/exit without framing — were rejected; see §2) |
| — | Desktop | Keeps system `ssh` (`ProcessSession`). russh **compiles** on every target so it is tested in ubuntu CI; it is **used** only on Android |
| — | Advanced accounts on Android | Not offered — there is no ssh-agent and no `~/.ssh/config`. Android SSH accounts are Managed accounts |
| — | Key storage | An owner-only file in the app-private data dir, #103's exact layout. Not the Android Keystore: it cannot hand russh an exportable Ed25519 key to sign with, and the app sandbox already isolates the file more strongly than a desktop home directory does |
| — | Crypto backend | `russh` with `default-features = false, features = ["ring", "flate2"]` |

### The crypto backend, measured

russh 0.63.3 (2026-09-09, MSRV 1.89; the toolchain here is 1.96) defaults to
`aws-lc-rs`, a C/assembly build needing cmake — it would be the third vendored
C stack in the NDK cross-build after OpenSSL and SQLCipher. Measured with
`cargo tree` on a scratch crate: with `default-features = false, features =
["ring", "flate2"]` the tree contains **zero** `aws-lc` crates and resolves
`ring v0.17.14` — the exact version already in Jodd's `Cargo.lock` through
reqwest/rustls, which `release.yml`'s `build-android` already cross-compiles
for all four ABIs. Key types come from `ssh-key` with `ed25519-dalek`, pure
Rust. `ssh-key` is pinned by russh at `0.7.0-rc.11`, a release candidate;
Jodd reaches it only through russh's re-export (`russh::keys`) and never
names it as its own dependency, so the two can never disagree on a version.

## 1. Components

Everything below compiles on every target. Runtime selection is by
`cfg(target_os = "android")` at a handful of switch points.

### New

| Unit | Does | Depends on |
|---|---|---|
| `backend/ssh/framing.rs` — extracted from `session.rs` | `frame(script, nonce)` (moved verbatim) and `read_framed(reader, nonce) -> Result<ExecOutput, SshError>` over any `AsyncBufRead + Unpin` (today's `run_framed` minus the write half). `ProcessSession` calls it with no behaviour change | nothing |
| `backend/ssh/russh_session.rs` | `RusshSession: SshSession` (below) and `stderr_for(RusshFailure) -> String` (§3) | `russh`, `framing`, `classify::auth_hint` |
| `backend/ssh/russh_setup.rs` | The in-process setup verbs, writing **the same files** as `setup.rs`: `generate_key`, `fetch_fingerprint`, `install_key_over_password`, `verify_key_login`, `revoke` (§2) | `russh`, `russh::keys`, `scripts` |

**`RusshSession`**, constructed from `(target, SshOptions)` exactly as
`SpawnSpec::ssh` is:

- Holds `tokio::sync::Mutex<Option<client::Handle<PinnedHost>>>` — one
  authenticated connection, created lazily by the first `exec`, never
  persisted.
- `exec(script)`: connect if needed → `channel_open_session` → `exec("sh")`
  → write `framing::frame(script, nonce)` → `eof()` → `read_framed` over the
  channel's stdout (`ChannelStream` / `make_reader`) → close. Per request, a
  fresh uuid nonce, as today.
- The whole request is bounded by `session::EXEC_TIMEOUT` (60 s); connect by
  10 s (`ConnectTimeout=10`'s equivalent); `client::Config::keepalive_interval
  = 15 s` (`ServerAliveInterval=15`'s equivalent), so a socket that died
  during sleep or a Wi-Fi ↔ mobile switch surfaces as an error instead of a
  hang.
- A timeout, a failed channel open, or a desynchronised frame drops the
  handle; the next `exec` reconnects. That is `ProcessSession`'s
  respawn-on-next-request contract.
- **Auth backoff:** an auth-class failure (a refused key, a host-key
  mismatch) is converted by `stderr_for` to OpenSSH's wording, judged by the
  same `classify::auth_hint`, and remembered for `session::AUTH_BACKOFF`
  (120 s) exactly as `ProcessSession::auth_failed` does. While held, `exec`
  returns `SshError::Disconnected { stderr }` without connecting.
- Its `client::Handler` (`PinnedHost`) implements `check_server_key` by
  comparing the offered key against the account's `known_hosts`
  (`SshOptions::known_hosts`), parsed with `russh::keys::known_hosts`, host
  pattern `host` or `[host]:port` for a non-22 port. Any mismatch or missing
  entry refuses the connection. There is no "accept new" path.
- Authentication: public key from `SshOptions::key` (OpenSSH-format file,
  always Ed25519 on Android, so no RSA hash negotiation is needed).

`ProcessSession` keeps its own `auth_failed` logic; the two are not merged
into a shared type (YAGNI — two short copies of a three-line rule, each
tested).

### Changed

| Where | Change |
|---|---|
| `session.rs` | `frame` moves to `framing.rs` (re-exported for existing callers); `run_framed` becomes "write, then `framing::read_framed`" |
| `ssh/mod.rs` `session_for` | Builds `RusshSession` on Android, `ProcessSession` elsewhere, from the same `(target, SshOptions)`. The registry's value type becomes `Arc<dyn SshSession>` |
| `ssh_commands::ssh_fingerprint` | Android arm calls `russh_setup::fetch_fingerprint` |
| `ssh_commands::ssh_setup_managed` | The `desktop-only` early return is removed. The pipeline, its order, its cleanup-on-failure, the `source` marker, the save and the first index are unchanged; each process-bound step (`prepare_key` for `Generate`, `install_key_over_password`, `verify_key_login`) takes its `russh_setup` variant on Android. `SetupCredential::KeyFile` on Android returns `"Key files aren't supported on Android yet — use Password."` — the UI never sends it; the backend refuses it anyway |
| `setup::retire_managed_key` | The policy (`revoke_requested`, keep-key marker, C5 imported-key gate, directory removal) is untouched; only the final "run the revoke script on the server" call gets an Android arm → `russh_setup::revoke` |
| `add_ssh_account`, `list_remote_dirs` (Advanced) | **Keep** the Android refusal; wording becomes `"Advanced SSH needs a desktop — use the simple setup."` |
| App startup (`.setup()` hook, after accounts load) | The orphaned-key-directory sweep (§2 "Process death") |
| `Sidebar.svelte` | "Add SSH Server" (the easy dialog) shows on Android; the Advanced entry stays `{#if !$isAndroid}` |
| `SshEasySetupDialog.svelte` | When `$isAndroid`: no Key-file radio (`method` fixed to `password`), no Advanced link |
| `src-tauri/Cargo.toml` | `russh = { version = "0.63", default-features = false, features = ["ring", "flate2"] }`, target-independent, in `[dependencies]` above the target tables |

**Deliberately untouched:** `scripts.rs` (Android reuses #103's
`install_authorized_key`, `revoke_authorized_key`, `key_body`), `read.rs`,
`write.rs`, `mdconv.rs`, `classify.rs`, the sync worker, and the account
format. An Android Managed account is byte-identical in `accounts.json` to a
desktop one.

## 2. Flows on Android

### Why one channel per request, with the existing framing

- It is what the storage spec named for this seam.
- No state leaks between requests — already true today, since `frame` runs
  every script in a `( … )` subshell, so a per-request channel changes
  nothing a script can rely on.
- **The framing stays load-bearing.** sshd runs an exec request as
  `<login shell> -c sh`, and bash sources `.bashrc` for non-interactive SSH
  sessions, so a `.bashrc` echo arrives on the channel's stdout ahead of the
  first marker — exactly item I-1, which the BEGIN marker (and its leading
  newline) already solves. Dropping the framing for SSH's native
  stdout/stderr/exit-status would reintroduce I-1 and create a second
  protocol with separately-tested edge cases.
- One parser for one wire protocol: a future framing fix lands on both
  platforms.
- Cost: one channel open per request, one round trip — negligible beside a
  `find | sha256sum` listing.

### Setup

**Screen 1 → 2: `ssh_fingerprint(host, port)`**

- A russh connection with `Preferred::key` ordered `ed25519, ecdsa, rsa`, so
  the key captured is the one #103's `pick_keyscan_line` would prefer.
- The handler's `check_server_key` **captures** the offered key and returns
  `false`: the handshake ends before any authentication, so no credential
  can reach a server the user has not yet trusted.
- Returns the existing `setup::Fingerprint { algorithm, fingerprint:
  "SHA256:…", known_hosts_line }`. The line is `<host> <algo> <base64>`, or
  `[<host>]:<port> …` for a non-22 port — tested against the same fixtures
  as `setup::known_hosts_line_for`, so both platforms write identical lines.

**Screen 2 → 3: `ssh_setup_managed(…, Password)`**

1. **Securing the connection** — `setup::write_known_hosts` writes exactly
   the line the user was shown (unchanged).
2. **Setting up Jodd's key** — `russh_setup::generate_key(dir)`:
   an Ed25519 key from the OS RNG, written as `id_ed25519` (OpenSSH private
   key format, mode 0600, directory 0700 — `prepare_key`'s own permissions)
   and `id_ed25519.pub`, commented `jodd-<short-id>@<device>`. On Android
   `<device>` is `android.os.Build.MODEL` (e.g. `SM-S711B`), read through the
   `jni` crate already in the tree; if that read fails, `android`.
   `write_key_source_marker(dir, generated = true)`.
3. **Signing in** — `russh_setup::install_key_over_password`:
   - connect; `check_server_key` now **verifies against the pinned line**
     (the swap window between "Trust" and connecting stays closed, as on
     desktop);
   - `authenticate_password`; if the server's remaining methods offer
     `keyboard-interactive` instead, run it, answering **only** prompts whose
     text mentions "password" (case-insensitive) with the password, and
     aborting on any other prompt — #103's askpass fail-safe rule. Many hosts
     run `PasswordAuthentication no` with `KbdInteractiveAuthentication yes`;
     OpenSSH's client handles both silently, which is why #103 never had to;
   - `exec` `scripts::install_authorized_key(pubkey_line)` over that
     connection; disconnect;
   - `russh_setup::verify_key_login`: a fresh connection with **the key
     alone**, proving it works.
4. **Creating your notes folder** — `session_for` returns a `RusshSession`;
   the existing `check_vault(create = true)` makes `~/Jodd-Notes`.

The save, the first index, and cleanup-on-failure (`remove_dir_all(base)`,
`forget_session`) are unchanged.

### Sync

An ordinary SSH account. The worker's 5 s tick calls `exec`; `RusshSession`
connects on first use. A connection that died while the phone slept is found
by keepalive or by the request timeout, reported `Transient` (A9), dropped,
and re-made on the next tick.

### Process death (gotchas #31/#32 territory)

- **During sync:** nothing to recover. No connection state is persisted; a
  cold start reconnects on the first `exec`.
- **During setup** — the realistic Android case: the user switches to their
  mail app mid-setup and Android kills Jodd. A key directory is left with no
  account row, possibly after its key was installed on the server.
  **Startup sweep:** during `.setup()`, after accounts are loaded, delete
  every `<data_base>/jodd/ssh/<dir>` whose name is not the `ssh_key` of any
  account row. It runs before the UI can start a setup, so it cannot race
  one; an account that is `pending_removal` still has its row, so its
  directory is kept. It is filesystem-only — no IPC, no network — so gotcha
  #32's stranded-reply hazard for work done in `.setup()` does not apply. The
  sweep runs on every platform — the same orphan can
  occur on desktop when the app is quit mid-setup.
  The orphaned `authorized_keys` line on the server **cannot** be removed
  from the phone: the key is gone with its directory. It is harmless (no one
  holds that key), recognisable by its `jodd-…@<device>` comment, and
  documented in `BACKEND-SSH.md`.

### Removal

`retire_managed_key`'s policy decides, unchanged. Android keys are always
`generated`, so revoke is allowed when requested. Its last step on Android is
a **one-shot** russh connection with the key — not the registered session,
which was forgotten when the row was removed — running
`scripts::revoke_authorized_key(key_body(pubkey))`. Best-effort, off the
removal's critical path (local-first).

## 3. Errors

`russh_session::stderr_for(failure) -> String` is a pure function turning
each structured russh failure into the OpenSSH wording `classify.rs` already
understands, so `classify::auth_hint`, `from_session_error`,
`from_setup_stderr` and A9 work unchanged and every case is a fixture:

| russh failure | Synthesised stderr | User sees |
|---|---|---|
| DNS failure | `ssh: Could not resolve hostname <host>: <io error>` | "Can't reach the server…" |
| `ConnectionRefused` / connect timeout | `ssh: connect to host <host> port <p>: Connection refused` / `…: Connection timed out` | same |
| auth rejected, remaining methods include `password` **or** `keyboard-interactive` | `<user>@<host>: Permission denied (publickey,password).` | "Wrong username or password, or this host does not allow password login." |
| auth rejected, only `publickey` remains | `<user>@<host>: Permission denied (publickey).` | **Android override**, below |
| host key ≠ pinned line, or no pinned entry | `WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\nHost key verification failed.` | "This server's identity is different from when you trusted it…" |
| channel/socket died mid-request | the russh error's own text | `Transient`; the next tick reconnects |

`keyboard-interactive` is normalised to `password` in the synthesised line
because `classify::password_auth_was_offered` looks for the substring
`password`, and on Android keyboard-interactive **is** the password method.

**Android override:** `from_setup_stderr`'s "This server only accepts key
files. Choose the Key file option." names a radio button Android does not
show. On Android, `ssh_setup_managed` maps that case to *"This server only
accepts key files. Jodd for Android can't import key files yet."* — at the
command layer, keeping `classify.rs` platform-free.

Session failures stay `Transient` (A9) and never push-block a note
(gotcha #14).

## 4. Testing

All of this runs in `cargo test --workspace` on ubuntu CI.

1. **Pure.** Every row of §3 through `stderr_for` → `classify` (both
   `from_session_error` and `from_setup_stderr`); `known_hosts` lines equal to
   #103's `known_hosts_line_for` fixtures, including `[host]:port`; the
   keyboard-interactive rule answers password prompts and refuses every other
   prompt (a yes/no host-key question included).
2. **Key-format cross-check.** `generate_key`'s output is read back by the
   **real** `ssh-keygen -y -f` (unix CI), and its public line equals
   `id_ed25519.pub`'s key body: the file is one OpenSSH itself accepts, not
   merely one `ssh-key` round-trips with itself. Permissions 0600/0700
   asserted.
3. **Framing extraction.** Every existing `ProcessSession` test passes
   unchanged — the regression proof for the one refactor of working code.
4. **`RusshSession` against a real OpenSSH `sshd`.** `setup.rs`'s
   `start_sshd` moves to a shared test helper and picks its port by binding
   `127.0.0.1:0` (today it derives the port from the pid, so two sshds in one
   test binary would collide). The `ProcessSession` suite, ported:
   stdout/stderr/exit; a marker lookalike in content; `cat` cannot read the
   command stream; reconnect after
   the connection is killed; a hung script times out and the next request
   recovers; the auth hold makes **one** attempt, counted in sshd's log, not
   one per request; a changed host key is refused. Plus a short storage flow
   through `SshVertical` — create → list → save → `Conflict` on an outside
   edit — proving the shipped scripts behave identically over the new
   transport. Skips cleanly where `sshd` is absent, as `start_sshd` does.
5. **The password path, end to end, against a russh test server.** OpenSSH's
   `sshd` needs root to check passwords, which is why #103 could only
   stub-test its askpass half. Password is Android's only method, so a small
   in-process server built on russh's server side accepts `password` and/or
   `keyboard-interactive` (each configurable per test), accepts the installed
   public key afterwards, and runs exec requests through a local `sh` in a
   tempdir used as `$HOME`. Against it, the whole `ssh_setup_managed`
   pipeline runs in CI: fingerprint → trust → generate → password install →
   key-only verify → `~/Jodd-Notes` → the `authorized_keys` line present,
   then `retire_managed_key` removing exactly that line. Wrong password and
   publickey-only servers produce §3's messages. *Limit, stated: this proves
   Jodd against russh's server, not against OpenSSH's password handling —
   the device pass covers that.*
   The same server carries the **startup-banner tests**: its exec handler can
   run `sh -c 'printf banner; exec sh'` (with and without a trailing
   newline), deterministically. The unprivileged OpenSSH `sshd` cannot: it
   runs the real user's login shell from `/etc/passwd`, and a test must not
   write that user's real `~/.bashrc`.
6. **Sweep.** Directories with no account row are removed; a
   `pending_removal` account's directory and a live account's are kept.
7. **Frontend.** `sshEasySetupDialog.test.ts`: with `isAndroid` true, no
   Key-file radio and no Advanced link; `Sidebar.test.ts`: the easy entry
   shows on Android, the Advanced one does not.

### The Android proof

- **Automatic on this PR.** `android-encryption.yml` triggers on
  `src-tauri/Cargo.toml` and `Cargo.lock`, and cross-compiles the whole lib
  test binary for `x86_64-linux-android` — so russh + ring compiling for
  Android is proven on the PR, not at release.
- **Added to that workflow:** one more emulator test,
  `russh_session::tests::android_round_trip_over_loopback`. The runner starts
  a public-key `sshd` on the host, listening on `0.0.0.0` (the emulator reaches
  the host at `10.0.2.2`), and `ssh-keygen`s a client key pair whose public
  half it puts in that sshd's `authorized_keys`. It `adb push`es the private
  key, the port and the `known_hosts` line (written for host `10.0.2.2`) into
  `JODD_TEST_DIR`. The test first runs `generate_key` into a scratch
  directory and parses the result back (generation runs on Android), then
  connects a `RusshSession` with the **pushed** key, pinned to the pushed
  host line, and does a framed `exec` round trip. The key the test generated
  is not the one it logs in with, because the runner cannot install a key
  that does not exist until the emulator runs. It is the only
  pre-device proof that the crypto and socket code actually **run** on an
  Android runtime. Gotcha #13 applies: the test filter goes on the
  `adb shell` line, and pass is asserted two ways (exit code is not reliable
  over `adb shell`). The workflow's `paths` gain `src-tauri/src/backend/ssh/**`.
- **Gap, stated rather than hidden:** only `release.yml`'s `build-android`
  compiles all four ABIs, and it cannot be run before a release without
  creating a draft. `ring`'s aarch64 assembly already ships in every APK via
  reqwest, so the risk is low.
- **Local:** this Mac has no NDK installed and ~19 GB free; no local APK
  build unless the user asks for NDK 27.3 to be installed.

### Verification gate

The CLAUDE.md set — `cargo test --workspace`, `node scripts/gen-changelog.mjs`,
`npx vitest run`, `npx svelte-check --threshold error`, `npm run build` — plus
a green `android-encryption.yml` on the PR.

### Device pass (with the user present; new rows in `BACKEND-SSH.md` §7)

Galaxy S23 FE (phone) and Galaxy Tab S7 (tablet). `adb devices` first; a
screenshot immediately before every adb input; never while the user is using
the device.

| # | Action | Expect |
|---|---|---|
| A1 | Password setup against a real VPS | fingerprint screen, then the account appears and `~/Jodd-Notes` exists |
| A2 | Create and edit a note on the phone | file on the server; note visible on desktop Jodd |
| A3 | Sleep the phone ≥10 min, wake | sync resumes without user action |
| A4 | Switch Wi-Fi ↔ mobile data mid-sync | one `Transient` at most, then sync resumes |
| A5 | Kill Jodd during screen 3, relaunch | orphan key directory swept; no half account |
| A6 | Remove the account with revoke ticked | its `jodd-…@SM-…` line is gone from `authorized_keys`; other keys untouched |
| A7 | Change the server's host key, open Jodd | refused with the "identity is different" message; no note push-blocked |
| A8 | A host with `PasswordAuthentication no`, `KbdInteractiveAuthentication yes` | setup succeeds |

## 5. Documentation

- `BACKEND-SSH.md`: §2 gains the `RusshSession` transport; §6 "Platform
  notes" replaces "Desktop only for v1" with the Android story (Managed only,
  password only, the keyboard-interactive rule, the orphan line after a
  killed setup); §7 gains rows A1–A8.
- `CLAUDE.md`: the Tech Stack "Remote" bullet notes Android SSH uses an
  embedded russh; no new gotcha number until a device pass earns one.
- `docs/ROADMAP.md`: key-file import on Android as a follow-up item.

## Out of scope

- Key-file import on Android (and therefore `.ppk` handling there).
- Advanced (agent / `~/.ssh/config`) accounts on Android.
- Replacing desktop's system `ssh` with russh.
- Holding the key in the Android Keystore.
- Keeping a connection alive while the app is backgrounded (no foreground
  service); sync resumes when the app runs.
- A re-verify screen after a host-key change (remove and re-add, as on
  desktop).
- Cleaning the server-side `authorized_keys` line left by a setup killed
  mid-way.

## Amendments from planning (2026-09-29)

Each one replaces the spec text where the two disagree.

- **B1 — key comment.** The spec says `jodd-<short-id>@<device>`, with the
  device model read over JNI. But #103's shipped desktop code writes
  `jodd-<short-id>` (`setup::pubkey_line_for`), not `@<computer>`. Android
  reuses `pubkey_line_for`, so there is no JNI call and both platforms
  produce identical lines.
- **B2 — features gain `"rsa"`**, so an RSA-only host key can be pinned (the
  spec's preference list is `ed25519, ecdsa, rsa`). `rsa` is pure Rust. This
  was measured: the tree still holds zero `aws-lc`/`cmake`.
- **B3 — Managed-aware sync errors** (`classify::managed_auth_hint`,
  `SshVertical::managed`). The spec left `classify.rs` untouched, but its
  sync-time hints name `ssh-add` and a terminal, which is wrong for any
  Managed account and impossible on a phone. The Review Focus #1 fix.
- **B4 — the sweep only removes directories older than one hour**, for
  Review Focus #2.
- **B5 — `setup_managed_core(…, embedded)`, `session_for_with(…, embedded)`
  and `retire_managed_key_with(…, embedded)`** are extracted, so the Android
  pipeline, sessions and revoke are *executed* in host CI instead of
  existing only behind `cfg(target_os = "android")`.
- **B6 — the emulator test's sshd listens on `127.0.0.1`**, not `0.0.0.0`.
  `10.0.2.2` is the emulator's alias for the host's loopback, so nothing
  needs to listen on a public interface.
- **B7 — the host-key check compares the key only**, ignoring the host
  field of the pinned line. Each account's `known_hosts` holds exactly the
  one line the user trusted, so the host field adds nothing, and ignoring
  it keeps `127.0.0.1`/`10.0.2.2` tests and `[host]:port` forms from
  diverging.
- **B8 — the Managed setup's final vault check** (`ssh_commands::probe_message`
  / `check_vault`, now taking `managed: bool`) also uses
  `classify::managed_auth_hint`, so no Managed path suggests `ssh-add` or a
  terminal. `add_ssh_account` and `list_remote_dirs` pass `false`.
- **B9 — the startup sweep is skipped whenever `accounts::last_load_failed()`
  is true.** `load_accounts()` swallows read/parse errors into an empty
  list, which would otherwise make every key look orphaned and delete it.
  The sweep also decides candidacy with `entry.file_type()` (lstat), so
  symlinks are never swept or followed.
- **B10 — test harness facts that matter to anyone extending the tests:**
  - `test_server::ServerOpts.password_auth` gates the plain password
    method, so keyboard-interactive-only servers can be modelled.
  - `auth_password` rejects with an explicit `proceed_with_methods`, which
    models OpenSSH keeping `password` listed after a wrong guess.
  - `drop_connections` kills sockets through a `Killable` wrapper, lazily,
    on their next I/O.
  - The `open_socket` fix maps `ConnectionReset` to `Refused`.
