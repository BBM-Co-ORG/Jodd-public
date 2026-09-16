# At-rest encryption — design

> Status: **approved** (2026-08-13, revised same day). Decision: SQLCipher,
> **OpenSSL** crypto backend (via `bundled-sqlcipher-vendored-openssl`) —
> reversed from an initial LibTomCrypt choice after a PoC found it isn't
> reachable as a Cargo feature, and follow-up research found a confirmed
> crash bug in every SQLCipher version the Rust ecosystem currently vendors
> for that backend. See "PoC finding" and "Follow-up research" under the
> crypto-backend section for the full evidence trail. Not yet scoped into an
> implementation plan.

Captures what's actually encrypted at rest in Jodd today, what isn't, the
options considered for closing the gap, and why SQLCipher-with-OpenSSL is
the chosen direction — including the reversed LibTomCrypt decision and the
concrete evidence behind reversing it, kept in the doc rather than erased,
since "we tried X and found Y" is worth more than a clean-looking decision
with the false start deleted.

## Problem

The user wants to be able to claim "data is encrypted at rest" for Jodd. That
claim is currently **false** for the thing that matters most: `jodd.sqlite3` —
note titles, bodies, tags, the whole graph — is a **plain, unencrypted SQLite
file** on disk (`paths.rs` / `rusqlite`, `bundled` feature, no cipher). Anyone
with filesystem access — another local user, malware, a stolen unlocked
laptop, a phone without device encryption — can read every note directly.

`docs/PRIVACY-POLICY.md` doesn't currently assert at-rest encryption for the
DB, so nothing is contradicted today — but nothing is covered either.

## What's already protected, and how

Credential storage already goes through the OS keychain, split across four
distinct entries, all under `keyring-core` (`secrets.rs`,
[secrets.rs:17-42](../../../src-tauri/src/secrets.rs)) with service name
`jodd`:

| Secret | Keychain key | Where |
|---|---|---|
| Gmail refresh token, per account | `rt::{account_id}` | [accounts.rs:290-291](../../../src-tauri/src/accounts.rs) |
| Per-account LLM API key | `llm_api_key::{account_id}` | [accounts.rs:340](../../../src-tauri/src/accounts.rs) |
| App-level (global) LLM API key | `llm_api_key::__app__` | [app_llm_config.rs:19](../../../src-tauri/src/app_llm_config.rs) |
| Google OAuth **client secret** (BYO credentials) | `oauth_client_secret::google` | [oauth_config.rs:5-6](../../../src-tauri/src/oauth_config.rs) |

The Google OAuth **client ID** is *not* a secret (Google treats Desktop client
IDs as public) and is stored in plaintext at
`<config dir>/jodd/google_oauth.json` ([oauth_config.rs:13-18](../../../src-tauri/src/oauth_config.rs)),
separate from its paired secret above. The `.env`-baked `GOOGLE_CLIENT_ID` is
only the build-time *default*; `auth::client_id()` prefers the runtime
`oauth_config.json` value when present ([auth.rs:91-94](../../../src-tauri/src/auth.rs)).

Each keychain entry is independently gate-able by the OS — on macOS this means
one Keychain "Allow / Always Allow" prompt per entry, re-triggered whenever the
app's code signature changes (a side effect of ad-hoc/dev signing not being
stable across builds, not something Jodd controls directly). N Gmail accounts
+ 2 LLM keys + 1 OAuth client secret = that many potential prompts on a fresh
install or a re-signed upgrade. "Always Allow" trades away the one-time human
gate against future silent access by anything matching that signature — modest
but real risk reduction lost for convenience.

**Gap:** none of this covers the SQLite file itself, which is where the actual
note content lives.

## Options considered

1. **SQLCipher** (page-level AES-256 encryption, transparent below the SQL
   layer) — recommended, see below.
2. **Rely on OS full-disk encryption** (FileVault / BitLocker / Android FBE)
   and describe *that* in the privacy policy. Zero engineering cost, but it's
   not an app-level claim — it protects a powered-off/stolen device, not
   another local user or process on an unlocked machine, and Jodd can't take
   credit for a platform feature as its own.
3. **Column-level encryption** of `body_html`/`title` only, keyed from the
   keychain, everything else left plaintext. Rejected: breaks FTS5 (can't
   index encrypted text), and search/FTS is central enough to this app
   ([db.rs](../../../src-tauri/src/db.rs)) that this would touch far more
   surface than the crypto itself justifies.

SQLCipher is the only option that lets Jodd make a real, meaningful "encrypted
at rest" claim without gutting search.

## How SQLCipher works

A fork of SQLite that encrypts every page (default 4KB) with AES-256-CBC + HMAC
before it hits disk, and decrypts on read — transparently, below the B-tree
layer. `PRAGMA key = '...'` (or `sqlite3_key()`) is set once per connection
right after open; every existing query, index, and FTS5 table keeps working
unmodified, because encryption happens beneath the SQL layer, not inside it.
The file header is encrypted too, so an attacker with just the `.sqlite3` file
can't even read table/column names.

Integration shape for Jodd: link `rusqlite` with a `bundled-sqlcipher-*`
feature instead of plain `bundled`; generate a random 256-bit key on first run;
store it via the existing `secrets.rs` keychain path (a fifth entry, e.g.
`db_key::{profile}`); call `PRAGMA key` immediately after `Connection::open()`
wherever that currently happens in `paths.rs`/`db.rs`. A wrong key doesn't
error — it just fails to parse valid pages — so a canary query is needed to
distinguish "wrong key" from "corrupt DB".

Existing users have a plaintext `jodd.sqlite3` today, so shipping this needs a
**one-time migration**: open the old plaintext DB, `ATTACH` a fresh encrypted
DB, `sqlcipher_export()`, swap files. That migration — not the encryption
itself — is the actual engineering work.

## Crypto backend: OpenSSL vs. LibTomCrypt

SQLCipher's crypto is pluggable via a fixed C-ABI interface, with four
supported backends: **OpenSSL** (default), **LibTomCrypt**, **CommonCrypto**
(Apple-only), **Windows CNG** (Windows-only).

**OpenSSL is the wrong default for this codebase, and Jodd has already paid
for finding this out once — for a different dependency.** `reqwest`'s
`default-tls` pulls in `native-tls`, which on Linux links `openssl-sys`; that
build fails outright cross-compiling for Android, because there's no OpenSSL
to link against there. The existing fix
([Cargo.toml:82-100](../../../src-tauri/Cargo.toml)) splits `reqwest`'s TLS
backend by platform: desktop keeps `native-tls`, Android gets `rustls-tls`
instead. SQLCipher's default OpenSSL backend would reproduce the *identical*
Android cross-compile failure, now for a second, unrelated C library.

Two more points sharpen this:

- **rustls doesn't transfer as a fix here.** It worked for `reqwest` because
  both are pure-Rust libraries and rustls is a drop-in `Cargo.toml` feature
  swap within reqwest's own backend system. SQLCipher is a C library expecting
  a C-ABI crypto plugin; rustls/`ring` expose no such interface. Using rustls
  here would mean writing a brand-new C-compatible SQLCipher crypto provider
  from scratch — unproven work, no existing crate does this.
- **OpenSSL isn't even reused today on desktop.** `native-tls` only links
  `openssl-sys` on Linux; on macOS it uses Secure Transport, on Windows it
  uses SChannel. So SQLCipher+OpenSSL wouldn't extend an existing dependency —
  it would introduce a brand-new Perl/`Configure`-based OpenSSL build to
  **macOS and Windows for the first time**, on top of the known-bad Android
  case.

**Original decision (now reopened): LibTomCrypt, the same backend on all
three platforms.** Plain C, no external build system (no Perl, no
`Configure`), compiles the same trivial way anywhere a C compiler exists —
including the Android NDK — sidestepping the `openssl-sys` failure entirely.
AES throughput is software-only (no AES-NI), but at note-body scale (a few
KB per page) this cost is unmeasurable. The alternative of using each
platform's native backend (CommonCrypto / CNG) was rejected for reintroducing
per-platform code paths — two or three DB-export routines to validate instead
of one, for no workload-driven reason. This reasoning is all still sound —
what turned out wrong is the assumption that LibTomCrypt was *reachable*
through the crate Jodd would actually use.

### PoC finding (2026-08-13): LibTomCrypt is not a `libsqlite3-sys` Cargo feature

Ran a scoped PoC specifically to de-risk this decision before committing
engineering time to it: added `rusqlite = { version = "0.31", features =
["bundled-sqlcipher"] }` (the exact version Jodd's `Cargo.toml` pins) to a
scratch crate, fetched it, and read the resulting `libsqlite3-sys-0.28.0`
build script and vendored SQLCipher amalgamation directly rather than
inferring from documentation.

**Finding:** `build.rs` supports exactly three crypto-backend paths —
vendored OpenSSL (`bundled-sqlcipher-vendored-openssl`), system OpenSSL
(dynamic-linked, if found via `OPENSSL_LIB_DIR`/`OPENSSL_INCLUDE_DIR` or
auto-discovery), and CommonCrypto (`-DSQLCIPHER_CRYPTO_CC`, gated on
`host.contains("apple") && target.contains("apple")` — Apple platforms
only). **There is no code path that ever defines
`-DSQLCIPHER_CRYPTO_LIBTOMCRYPT` or links a `tomcrypt` library.** On Android
specifically, none of the three conditions match, so the build script falls
through to `println!("cargo:rustc-link-lib=dylib=crypto")` — it still
expects an OpenSSL-shaped `libcrypto` to link against and would fail exactly
the way the LibTomCrypt choice was meant to avoid.

The LibTomCrypt C glue code *is* physically present in the vendored
amalgamation (`crypto_libtomcrypt.c`, compiled in behind `#ifdef
SQLCIPHER_CRYPTO_LIBTOMCRYPT` — confirmed present in both `libsqlite3-sys`
0.28.0, Jodd's pinned version, and the current 0.38.2), including a real
`#include <tomcrypt.h>`. So SQLCipher-the-C-project genuinely supports this
backend — the gap is entirely in `libsqlite3-sys`'s build script never
selecting it, and LibTomCrypt itself (a separate upstream C library) never
being vendored or fetched by this crate at all.

**What using LibTomCrypt actually requires, now that this is verified rather
than assumed:** forking or patching `libsqlite3-sys`'s `build.rs` to define
the right flag instead of the OpenSSL/CommonCrypto ones, *and* separately
vendoring and cross-compiling LibTomCrypt itself for every target, then
linking it in. That is materially more ongoing engineering than "add a
Cargo feature" — closer to "maintain a fork of a third-party build script
plus a second vendored C dependency" — and this spec's original comparison
never weighed that cost against OpenSSL's.

### Follow-up research (2026-08-13): no existing crate solves this, and LibTomCrypt has a worse problem than build cost

Checked option (c) from the PoC finding — whether any existing Rust crate
or wrapper already solved this — plus what actually happens if the
`LIBSQLITE3_FLAGS` env var is used to inject `-DSQLCIPHER_CRYPTO_LIBTOMCRYPT`
without forking `build.rs`.

- **No existing crate solves this.** `rusqlcipher`, the one alternate
  SQLCipher wrapper on crates.io, explicitly only supports OpenSSL 1.1.0+,
  with no Android or LibTomCrypt story.
- **`LIBSQLITE3_FLAGS` is necessary but not sufficient.** It only injects
  preprocessor `-D`/`-U` flags — it doesn't stop the unconditional
  `cargo:rustc-link-lib=dylib=crypto` linker line Android falls through to
  regardless (see PoC finding above), and does nothing to vendor,
  cross-compile, or point the build at LibTomCrypt itself.
- **The decisive finding: SQLCipher's LibTomCrypt backend had a confirmed,
  general (not Android-specific) segfault bug — `sqlcipher_ltc_activate`
  doesn't reinitialize the Fortuna PRNG after `sqlcipher_ltc_deactivate`
  clears it, crashing the next random-number call — only fixed upstream in
  **SQLCipher 4.17.0**. The version actually vendored by the Rust ecosystem
  is older than that fix in both directions checked: `libsqlite3-sys 0.28.0`
  (what Jodd's pinned `rusqlite = "0.31"` pulls today) vendors **SQLCipher
  4.5.3**; the *latest* `libsqlite3-sys 0.38.2` vendors **SQLCipher 4.14.0**
  — still below the fix. Upgrading the Rust dependency doesn't dodge this.
  A deactivate/reactivate cycle is not a contrived edge case for a mobile
  app with backgrounding and connection churn — this is a real crash risk,
  not just extra build effort.
- **One data point in LibTomCrypt's favor, for completeness, not a
  solution:** the official `sqlcipher/sqlcipher-android` project defaults to
  LibTomCrypt and is actively maintained by SQLCipher's own team, proving
  it's achievable in principle. But it's a Java/Gradle-oriented library
  bundling prebuilt `.aar`/`.so` artifacts for JNI consumption, not a
  reusable native SDK Jodd's Rust/Tauri stack could link against — a harder
  reference to study, not a dependency to add.

**Decision, reopened and now closed: OpenSSL, via
`bundled-sqlcipher-vendored-openssl`.** This is no longer "which backend is
cheaper" — it's "accept OpenSSL's known, bounded Android cross-compile cost
(already scoped earlier in this doc), since LibTomCrypt is the only
alternative and it carries a real, previously-crash-causing bug in every
SQLCipher version actually reachable through the Rust ecosystem today."
OpenSSL's audit/CVE history is larger than LibTomCrypt's, but "more
scrutinized, no open crash bug" beats "less scrutinized, confirmed crash
bug in the shipped version" without much debate. The Android build-cost
tradeoffs from the original OpenSSL section (CI time, ~2-5MB/ABI APK
weight, ongoing CVE-watching) stand as the accepted cost of this decision.

## Platform impact summary

> Superseded by the crypto-backend reversal below (OpenSSL, not LibTomCrypt)
> — kept as originally written since the LocalFs/prompt/migration content is
> still accurate; only the "Build risk"/"Binary size" row assumed the
> LibTomCrypt backend that was later reversed. See "Follow-up research" for
> the corrected build-cost picture: vendored OpenSSL via
> `bundled-sqlcipher-vendored-openssl`, the exact Android cross-compile cost
> this table's Android column already described as "the one that matters."

| | macOS | Windows | Android |
|---|---|---|---|
| Build risk | Low with vendored OpenSSL — same maturity as other Rust/Android projects already using this feature | Low — same | The known cost this decision accepts: `openssl-sys`-equivalent NDK cross-compile via `bundled-sqlcipher-vendored-openssl` |
| Binary size | Small-to-moderate increase (vendored OpenSSL + SQLCipher object code) | Same | Same, ~2-5MB/ABI — APK weight is more sensitive generally |
| User-visible prompt | One more possible Keychain "Always Allow" dialog, same mechanism as existing `rt::`/`llm_api_key::` entries | One more Credential Manager entry — Windows doesn't re-prompt per rebuild the way macOS does, so likely *less* prompt fatigue than macOS | Silent — `android-native-keyring-store` backs onto Android Keystore via encrypted SharedPreferences, no per-access dialog by default |
| Migration cost | One-time plaintext → encrypted export on first launch of the new build, same shape on all three platforms |

## Open questions — resolved

`Db::open(app_data_dir)` ([db.rs:173-176](../../../src-tauri/src/db.rs))
opens exactly **one** `jodd.sqlite3` per install, covering all accounts
(PK is `(uuid, account_id)`, not one DB per account). It's called once,
synchronously, inside Tauri's `.setup()` ([lib.rs:5083](../../../src-tauri/src/lib.rs)).
That single fact resolves most of what was open:

1. **Keychain key naming/scoping — singleton, not per-account.**
   One DB, one key: `db_cipher_key::v1` (service `jodd`, same `KC_SERVICE` as
   the other four entries). The `v1` isn't decoration — it lets a future
   re-key (compromised key, cipher change) mint a fresh entry without
   colliding with the old one mid-transition. Test/temp-dir DBs
   ([db.rs:3235](../../../src-tauri/src/db.rs), `:5098`, `:5761`) don't need
   real key management — skip encryption behind `cfg(test)` or use a fixed
   non-secret test key, since they never hold real user data.

2. **Migration timing — synchronous, inside the existing `Db::open()` /
   `.setup()` path.** The schema-migration table already runs synchronously
   at every startup ([db.rs:302-319](../../../src-tauri/src/db.rs)) and
   nobody treats that as a local-first doctrine violation — that doctrine's
   "never block on remote" rule targets *network* calls, not local disk I/O.
   The one-time plaintext→encrypted export is the same category of cost:
   proportional to note count, done once, ever. Reuse the existing
   migration-table mechanism rather than inventing a background/async path.

3. **Privacy-policy wording** (add to `docs/PRIVACY-POLICY.md` **after**
   shipping, not before) — **the claim is scoped to Gmail-synced accounts
   only.** The DB encryption itself applies uniformly to every account; only
   Gmail's guarantee is meaningful (LocalFs has an independent unencrypted
   copy that defeats it — see "Scope limitation" below), so the *claim*
   must be scoped even though the *mechanism* isn't:
   > "For Gmail-synced accounts, notes stored in Jodd's local database are
   > encrypted at rest (AES-256, SQLCipher). The encryption key is generated
   > on your device and stored in your operating system's secure credential
   > store (macOS Keychain / Windows Credential Manager / Android Keystore)
   > — Jodd never transmits this key anywhere. This does not apply to Local
   > Folder vaults: those store notes as plain files in the folder you
   > choose, and protecting that folder (e.g. via full-disk encryption) is
   > your responsibility."

4. **CI proof for Android — still genuinely open.** `release.yml` needs a
   build step that cross-compiles OpenSSL+SQLCipher (via
   `bundled-sqlcipher-vendored-openssl`) for at least `aarch64-linux-android`
   via the NDK, *and* a test that round-trips
   `PRAGMA key` + a real query on an emulator or device — "it compiles"
   alone wouldn't catch a key-derivation mismatch. Must be proven green in
   CI before merge, same skepticism CLAUDE.md gotcha #8 already applies to
   Android OAuth ("a port verified on one Android device is not verified").
   This is the one remaining blocker before `superpowers:writing-plans` can
   produce a real build sequence.

## Decision

**SQLCipher is decided.** Page-level AES-256 encryption of `jodd.sqlite3`,
one identical build/key path across macOS, Windows, and Android — this part
was never in question and doesn't depend on which crypto backend was picked.

**Crypto backend: OpenSSL, via `bundled-sqlcipher-vendored-openssl`.**
Reopened, researched, and closed on 2026-08-13, reversing the original
LibTomCrypt call. The original reasoning — avoid the Android `openssl-sys`
cross-compile failure this codebase already hit once for `reqwest` — was
sound, but rested on an unverified assumption that LibTomCrypt was reachable
as a `libsqlite3-sys` Cargo feature the same way vendored OpenSSL is. It
isn't (PoC finding above), and worse: SQLCipher's LibTomCrypt backend
carries a confirmed segfault bug (Fortuna PRNG deactivate/reactivate, fixed
upstream only in SQLCipher 4.17.0) present in every SQLCipher version the
Rust ecosystem currently vendors — 4.5.3 in Jodd's pinned dependency, 4.14.0
in the latest available (follow-up research above). This isn't a
build-cost tradeoff anymore: LibTomCrypt is the only alternative to
OpenSSL, and it ships a real crash risk with no available fix through
existing tooling. OpenSSL's Android cross-compile cost (CI time, ~2-5MB/ABI
APK weight, ongoing CVE-watching — see the original comparison above)
stands as accepted.

Not yet turned into an implementation plan — the open questions below (key
naming/scoping, migration timing, CI proof on Android) need answers first
before `superpowers:writing-plans` can produce a real build sequence.

## Key lifecycle: generation, association, loss, rotation

**Generation.** A random 256-bit key from `rand::OsRng` (already a
dependency, [Cargo.toml:44](../../../src-tauri/Cargo.toml)) — not a
passphrase. Applied via SQLCipher's raw-key syntax
(`PRAGMA key = "x'<64 hex chars>'"`), which skips PBKDF2 key derivation
entirely; there's no brute-force surface to slow down against for a
machine-generated key that's never human-typed, and skipping the KDF avoids
paying its cost on every single app launch.

**Ordering, crash-safety.** Generate → write to keychain → confirm the write
succeeded → *only then* use the key to encrypt anything. Reversing this order
risks a crash between "data encrypted" and "key saved," which would strand
the user permanently locked out of their own data by their own app.

**Association is bookkeeping, not cryptography.** A correctly encrypted
SQLCipher file is indistinguishable from random bytes without the key — there
is no header field identifying which key opens it. The link is only:
"`app_data_dir/jodd.sqlite3` on this machine ↔ keychain entry
`db_cipher_key::v1` on this machine." No separate key-fingerprint sidecar for
v1 (one file, one key, one machine — the complexity isn't earned yet). What
*is* required: `Db::open()`'s error handling must distinguish three distinct
failure states — no key in keychain (expected on fresh install), key present
but doesn't decrypt this file (drift), and file isn't valid
SQLite/SQLCipher at all (corruption) — since each needs different recovery
messaging, not one generic "can't open database."

**Backup/restore — mostly already covered by existing architecture, for
Gmail accounts.** `jodd.sqlite3` is a *derived local cache* relative to a
Gmail account: the durable copy of a note is the Gmail message; the DB
mirrors it. `note_tags` derives from inline `#hashtags` in the body; pin
state and tags sync via a Jodd-authored sidecar message in Gmail
(`meta_msg_id`/`tags_meta_msg_id`). So for Gmail accounts, a full local DB
loss — with or without the key — is recoverable by re-indexing from Gmail,
the same mechanism a fresh install already uses. Only currently-`dirty`
(unpushed) edits are actually lost, a pre-existing risk unrelated to
encryption. **Requirement this adds:** when `Db::open()` hits the "key
doesn't decrypt this file" case, the failure path must offer "re-index this
account from Gmail" as a first-class recovery action, not a dead end.

**This "cache, not source of truth" framing does NOT hold for LocalFs
vaults, and that matters for more than backup — see the scope-limitation
section below.** For LocalFs accounts the vault directory *is* the durable
source of truth (not the DB), so DB-loss recovery still works fine there too
(re-scan the vault, same as a fresh install) — but that's a different claim
from "the DB encryption protects this account's data," which is false for
LocalFs. A bespoke encrypted export/backup feature is out of scope for this
spec either way — worth a future roadmap item, not required to ship this
safely.

## Scope limitation: LocalFs vaults are NOT covered by this design

**SQLCipher encrypts `jodd.sqlite3` uniformly — it applies to every account
in the DB identically, Gmail or LocalFs or any future backend, with no
per-account branching in the mechanism itself.** What differs by backend is
not whether the DB row is protected, but whether that protection is
*meaningful* — i.e. whether the encrypted DB copy is the only local
plaintext-risk copy of the data, or whether an independent unencrypted copy
sits right next to it and makes the DB's protection moot.

For a LocalFs (Local Folder) account, `LocalFsVertical`'s `MetadataSidecar`
impl writes the actual note content and metadata as **plain, unencrypted
files directly in the user-chosen vault directory** — not just a cache
mirror:

- Note bodies as `.eml` files (`decode.rs`)
- Pin state as `{uuid}.pin` — [transport.rs:479](../../../src-tauri/src/backend/localfs/transport.rs)
- Tags as `{uuid}.tags.json` — [transport.rs:482](../../../src-tauri/src/backend/localfs/transport.rs)

All written via plain `std::fs::write`
([transport.rs:534-548](../../../src-tauri/src/backend/localfs/transport.rs)),
no encryption anywhere in that path, sitting on the same local disk as
`jodd.sqlite3`. For a LocalFs account, the vault directory *is* the durable
source of truth, not a remote server — so even though its DB row is
encrypted exactly like every other account's, that encryption protects
**nothing** for that account's actual content: anyone with filesystem access
reads the `.eml` files directly and never needs the DB. The gap is a
redundant unencrypted copy elsewhere, not a hole in what SQLCipher covers.

**Consequence for the "encrypted at rest" claim:** it is true for
Gmail-synced accounts and **false for Local Folder accounts** — not because
the DB encryption skips LocalFs rows, but because LocalFs has a second,
unprotected copy of the same content that defeats the point. The
privacy-policy wording below must say so explicitly rather than implying
blanket coverage. This also means any future backend sharing this DB (e.g.
the Microsoft/Graph vertical on the roadmap) inherits the *good* case
automatically, with no extra work — same "cache of a remote source" shape
as Gmail, so its at-rest protection is meaningful for free.

Two ways to close this gap were considered and rejected **for the existing
LocalFs backend**:
- **Retrofit encryption onto today's vault.** Rejected — the Local Folder
  backend's entire value proposition is a plain, human/tool-inspectable
  folder of `.eml` files a user can `rsync`, back up, or open outside Jodd.
  Encrypting it by default works directly against that design goal, for
  every existing vault user, without them choosing it.
- **Silently ignore the gap.** Rejected — shipping DB encryption while
  implying full "at rest" coverage would make the privacy-policy claim
  false for a real, shipped backend.

**Decision: scope the claim narrowly for the current LocalFs backend, and
push explicit, honest disclosure to the moment of account creation rather
than relying on a privacy-policy footnote nobody reads.** At-rest encryption
covers the local cache of Gmail-synced notes only. Local Folder vaults are
out of scope for this spec — not silently, but stated plainly where the
user makes the choice:

- **Requirement:** the "Add Local Folder" flow (`Sidebar.svelte`'s
  "+ Add Local Folder" action → `add_local_account()`,
  [lib.rs:946](../../../src-tauri/src/lib.rs)) must show an explicit warning
  before or at folder selection — plain language, not buried in settings —
  stating that notes in this vault are stored as **unencrypted plain files**
  in the folder chosen, and that protecting them (disk encryption, access
  controls, backup handling) is the user's responsibility. This is a
  disclosure requirement, not a feature to build later — it should ship
  alongside (or even ahead of) the SQLCipher work, since it's true of the
  LocalFs backend *today*, independent of whether Gmail-account encryption
  ships at all.
- Revised privacy-policy draft above already reflects the narrowed claim.

**Future, separate feature — an encrypted vault variant, not a toggle on
today's backend.** Track as its own future spec (working name: "Secured
LocalFs backend" — pick a name that reads as a distinct choice at
account-creation time, not a checkbox that silently changes existing vault
behavior). Still architecturally distinct from both today's LocalFs backend
and the SQLCipher DB work in this spec, but the leading candidate design is
cheaper than first sketched, because it reuses rather than reinvents:

- **Make plaintext vault emission optional per account**, not encryption of
  the vault files themselves. `push_one_dirty()`'s call into
  `save_note_full()` (the step that writes `.eml`/`.pin`/`.tags.json` to
  disk) is skipped or gated behind a flag for a "Secured LocalFs" account.
  Since SQLite already stores full-fidelity `title`/`body_html` for every
  account unconditionally (confirmed above — no backend-specific schema
  branching), no data is lost by not also materializing it as plaintext
  files. This reuses the exact SQLCipher + OS-keychain machinery already
  designed for Gmail in this spec — no new whole-file crypto engine, no
  passphrase UX, no new key-derivation code path.
- **This does not reduce the underlying risk, only the engineering cost of
  reaching it.** The moment vault emission is off, that account's SQLite row
  is no longer a mirror of anything — it's the only copy. Losing the key
  means permanent, total loss of that account's notes, with nothing to
  re-index from, identical in severity to the passphrase-vault sketch this
  replaces — just reached via a cheaper implementation path.
- **Sharper consequence specific to this codebase: the shared-key blast
  radius stops being uniform.** `jodd.sqlite3` has one key
  (`db_cipher_key::v1`) shared across every account. Today, losing it is
  uniformly low-stakes — every account is Gmail, every account recovers via
  re-index. The instant one account opts into "no plaintext vault," that
  same key becomes catastrophic-to-lose for that one account while staying a
  non-event for every other account behind it — with no way to tell, from
  the key alone, which accounts it protects are recoverable and which
  aren't. Two ways to resolve this, to decide when this becomes a real spec:
  - **(a)** Accept the mixed-severity key as-is and document plainly that
    some accounts behind it are unrecoverable on loss and some aren't —
    cheapest, but conflates two different guarantees under one artifact.
  - **(b)** Give "no plaintext vault" accounts a *separate* keychain entry
    (e.g. `db_cipher_key::localfs_secured::{account_id}`), decoupled from
    the shared cache key. Costs little extra — a second `PRAGMA
    key`-scoped attach or keychain lookup — but lets heavier-duty UX (an
    explicit "save this recovery key somewhere safe" flow, print-a-phrase)
    apply *only* where the stakes warrant it, instead of either burdening
    every Gmail user with key-backup ceremony that doesn't apply to them, or
    under-warning the high-stakes accounts because they share a low-stakes
    key's UX treatment.
- **Not a strict security win — a confidentiality-vs-availability trade.**
  Turning off plaintext vault emission swaps "readable by anyone with disk
  access" for "readable by no one, ever, if the key disappears — including
  the user." Some users would rationally prefer the plaintext-but-always-
  recoverable version. Whichever mode a user picks, the disclosure
  requirement above (explicit warning at the point of choice, not a
  buried setting) must extend to this: the user needs to understand which
  failure mode they're accepting, not just that "Secured" sounds better.

**Key loss is permanent, by design.** No backdoor, no server-side escrow
(Jodd has no server to escrow into, and an escrowed key would defeat the
claim anyway). In practice this means "this device's local cache is gone,"
recovered the same way as scenario above. State this plainly wherever the
feature is documented.

**Changing computers is already a fresh install, unaffected by this.**
Jodd's existing multi-device story is: new machine → new install → new
`jodd.sqlite3` → OAuth re-auth → full re-index from Gmail. Encryption doesn't
change this; the new machine just generates its own new key the same way a
day-one install does. Manually copying `jodd.sqlite3` to another machine is
not a supported path today and stays unsupported — no export/import
machinery is being built to enable it.

**Key rotation — not built in v1, but not architecturally blocked either.**
Rotation earns its cost against *in-transit* key leakage (network, logs,
third parties); this key never leaves the device, so that threat model
doesn't apply. The realistic trigger — "this machine itself may be
compromised" — isn't fixed by rotating the DB key anyway, since an attacker
with local access already had the plaintext before rotation. Skip building a
rotation feature/UI now. The one thing worth doing preemptively, because it's
free today and expensive to retrofit: name the keychain entry
`db_cipher_key::v1`, not `db_cipher_key` — a future rotation is then just a
new versioned entry plus SQLCipher's `PRAGMA rekey` (re-encrypts an
already-open connection in place, cheaper than the full export/import the
initial plaintext→encrypted migration needs), not a naming-scheme migration
bolted onto a rotation feature.

## jodd-mcp coordination

`jodd-mcp` is not a client of the running app — it's a **separate OS
process** that links `jodd_lib` directly (`jodd-mcp` depends on it as
`jodd_lib = { path = "../src-tauri", package = "jodd" }`, per
[Cargo.toml gotcha #5](../../../CLAUDE.md)) and calls `db::Db::open()` on
`jodd.sqlite3` itself, independent of whether `Jodd.app` is even running.
Once that file is SQLCipher-encrypted, `jodd-mcp` needs the same
`db_cipher_key::v1` to open it — and because it's compiled as a **distinct
binary** from `Jodd.app`, this doesn't fall out of the design already
covered above for free. Three concrete problems, in order of how much they
change the implementation:

1. **macOS keychain ACLs are scoped per code-signature, so `jodd-mcp` needs
   its own grant to the same key — and the keychain-access-group fix is
   VERIFIED NOT AVAILABLE, not just unconfirmed.** The default behavior
   (used everywhere else in `secrets.rs` today) ties a keychain item's
   access list to the *requesting binary's* signature — that's why "Always
   Allow" only authorizes the one binary that asked.

   **PoC finding (2026-08-13):** read `apple-native-keyring-store`'s actual
   source (both `1.0.1`, the version Jodd's `Cargo.lock` pins, and current
   `1.0.2`). The crate ships two independent stores: `keychain` (the legacy
   Keychain Services store — what `secrets.rs` actually uses today via
   `features = ["keychain"]`, [secrets.rs:20-22](../../../src-tauri/src/secrets.rs))
   and `protected` (the newer Data Protection Keychain store). Access-group
   support (`access_group: Option<String>`, `set_access_group()`, a
   `test_shared_access_groups()` test) exists **only** in `protected` — zero
   references anywhere in `keychain.rs`. But `protected`'s own module doc
   says, verbatim: *"To use all the features of this module, your client
   application must be code-signed with a provisioning profile. Since
   command-line tools cannot be code-signed, there's not much point in their
   using this module."* `jodd-mcp` is exactly that — a command-line MCP
   server invoked via stdio by agent CLIs. **Both stores are dead ends, for
   different reasons: the one Jodd uses has no access-group feature; the one
   with access groups explicitly rules out CLI tools.** This is a confirmed
   answer, not an open question — do not re-scope this as "extend the
   dependency," that path is closed by the crate's own design.

   **Decision: (a), accept the per-binary grant as the primary design — not
   a degraded fallback, and not (b) or (c).** `jodd-mcp` gets its own
   one-time macOS keychain prompt, same category as the `rt::`/
   `llm_api_key::`/`oauth_client_secret::google` prompts Jodd already ships
   — one more instance of a pattern already accepted, not a new kind of
   problem. **The only new risk is a headless agent invoking `jodd-mcp` for
   the first time with nobody present to click "Allow" — solved by making
   that moment deliberate instead of accidental:** add a one-time
   interactive setup step to `jodd-mcp`'s own setup instructions — run
   `jodd-mcp` once by hand from a terminal (during the same pass where the
   user wires it into their agent's MCP config), surfacing the keychain
   dialog while a human is actually there. `secrets.rs` already has a
   `self_test()` pattern for exactly this shape of check
   ([secrets.rs:199](../../../src-tauri/src/secrets.rs)) — extend that idea
   to a `jodd-mcp --self-test` (or equivalent) that touches
   `db_cipher_key::v1` once, interactively, during setup. The fail-fast
   requirement in problem 2 below is then the safety net, not the primary
   plan: if `jodd-mcp` ever does hit an ungranted key (setup step skipped,
   re-signed build), it errors clearly — "run `jodd-mcp --self-test` once,
   or open Jodd.app" — instead of hanging on a dialog nobody can see.

   **(b) and (c) are real options, deliberately deferred, not rejected.**
   (b) — routing `jodd-mcp` through the running `Jodd.app` instead of
   opening the DB itself, so only one process ever holds the key — is
   architecturally the cleanest fix, but contradicts how `jodd-mcp` is
   built today (no IPC/core-crate layer; a repo-structure review already
   flagged this independently: "no core crate — jodd-mcp links the whole
   Tauri app"). A real architecture change, disproportionate to bundle into
   an encryption feature. (c) — hand-rolling raw `security-framework` calls
   using the legacy Keychain's `SecTrustedApplication` mechanism (list
   multiple trusted application signatures on one item, no App-Sandbox
   provisioning needed) — could remove the prompt entirely, but is
   unverified, its own PoC, and steps outside the `keyring-core`
   abstraction for this one credential. Revisit either only if the
   one-time-prompt friction from (a) becomes a real, reported complaint —
   don't build ahead of that signal.
2. **A keychain prompt can silently break `jodd-mcp`, not just annoy
   someone.** `jodd-mcp` is invoked headlessly by an external agent (Claude
   Code, Codex CLI) — there is often no interactive session and no human
   present to click "Allow." If macOS ever needs to (re-)prompt for
   `jodd-mcp`'s access (first run, a re-signed build), the process can hang
   waiting on a dialog nobody can see, or fail in a way an agent can't
   self-diagnose. Whatever the resolution to problem 1 is, `jodd-mcp` must
   fail fast with a clear, actionable error in this case — never hang.
3. **This is largely a macOS-specific problem — the spec should not
   over-engineer a cross-platform fix.** Windows Credential Manager entries
   are scoped per-*user*, not per-signed-binary — `jodd-mcp.exe` can likely
   read whatever `Jodd.exe` wrote with no extra prompting at all via
   `windows-native-keyring-store`. Linux's Secret Service API
   (`zbus-secret-service-keyring-store`) sits in between — access is scoped
   by requesting-application identity with at most a first-use consent
   prompt, generally far less strict than macOS's per-signature ACL. Scope
   the fix effort to macOS; assume Windows and Linux work with no special
   handling until proven otherwise.

**Migration ownership — undecided, and a real race if left undecided.** The
plaintext→encrypted migration was designed to run synchronously inside
`Db::open()`/`.setup()` — but `jodd-mcp` calls `Db::open()` too,
independently, and could be invoked by an agent *before* the user has ever
launched the GUI app on a machine carrying an existing plaintext DB. Two
processes racing to `ATTACH`/export/rename the same file is a materially
worse race than the already-tracked concurrent-dirty-write race (that one's
mitigated by `mark_pushed`'s version guard; a migration mid-flight has no
equivalent guard designed). **Decision needed before implementation:** only
`Jodd.app` performs the migration; `jodd-mcp` must detect a still-plaintext
DB and refuse to open it with a clear "run the Jodd app first" error rather
than attempting or racing the migration itself.

**The confidentiality boundary this doesn't change, worth stating
explicitly given what `jodd-mcp` is.** SQLCipher protects the *disk* —
someone who steals the machine or reads the file without running as this OS
user. It provides no protection against anything that can already execute
as the user, and `jodd-mcp` is precisely that by design: a sanctioned second
door that operates entirely *after* decryption, built so an external LLM
agent can act as this user's write proxy. This isn't a gap MCP introduces —
the GUI app has the same property — but `jodd-mcp` is a more novel,
higher-exposure surface (a possibly-cloud-hosted agent driving local
writes) than the GUI app, so the privacy-policy wording above should not
imply at-rest encryption protects against a compromised or over-trusted MCP
client. It doesn't, and can't.

## Shared-machine impact: two different scenarios with opposite conclusions

"Multiple people share a Windows/Mac" collapses two very different setups
that this design treats completely differently — worth being explicit so
the feature isn't oversold in one case and undersold in the other.

**Separate OS accounts on one machine (family iMac with per-person logins, a
shared work laptop, a lab machine) — already well-isolated today, and this
is arguably the strongest real-world justification for the whole feature.**
`jodd.sqlite3` lives under `dirs::data_dir()`
([paths.rs:32-42](../../../src-tauri/src/paths.rs)) — the standard
per-OS-user directory (`~/Library/Application Support` on macOS, `%APPDATA%`
on Windows) — so two separate logins on the same machine already get fully
separate DB files *and* separate keychains, automatically, with no
multi-user code in Jodd at all. A standard second user can't read another
user's `Application Support`/`AppData` contents under default permissions,
encrypted or not. Where SQLCipher earns its keep here is against an
**Administrator account on the same machine, or root/sudo** — both routinely
bypass normal file permissions (`sudo cat` on macOS, "Take Ownership" on
Windows NTFS), so without encryption an admin reads another user's notes
trivially. With SQLCipher, they get ciphertext, and the key is sealed in
*that specific user's own* login keychain / DPAPI store, generally
unreachable without that user's password.

**This is the sharpest justification for the feature found in this whole
design: FileVault/BitLocker — Option 2, rejected earlier in this spec —
provides zero protection in this exact scenario.** Full-disk encryption only
protects a powered-off, unmounted disk; the moment any user is logged in and
the volume is mounted, it draws no line between OS accounts on that running
machine at all. **SQLCipher is the only layer in this entire design that
protects against a fellow logged-in account — even an admin — snooping on a
shared, currently-running machine.** That's a stronger case than "stolen
laptop," which FileVault already covers on its own. One caveat: on a
**domain-joined Windows machine**, DPAPI's per-user key can have an
escrow/recovery path via Active Directory that IT/domain admins can
sometimes exercise — a corporate-managed shared Windows machine has a weaker
guarantee than a personal one.

**Multiple people sharing ONE OS login (the common "family shares one
Windows account" setup) — SQLCipher provides zero protection here, and this
must be stated plainly rather than left implied.** If two people share one
OS session, they're indistinguishable to both the OS and to Jodd — the key
unlocks the DB for whoever is running as that OS user, with no concept of
which human is physically at the keyboard. Anyone who opens Jodd on that
shared, already-unlocked session sees everything, encrypted-at-rest or not,
because decryption already happened transparently the moment the app opened
the DB. This connects to something already true of Jodd independent of this
spec: there is no app-level lock (PIN/passcode/biometric) gating the UI on
top of the OS session. SQLCipher and an app-level lock are complementary,
not substitutes — SQLCipher defends the *disk*, an app lock would defend the
*walk-up-to-an-unlocked-session* case — and this spec's work covers only the
former. An app-level lock is a distinct, future feature, not a byproduct of
this one.
