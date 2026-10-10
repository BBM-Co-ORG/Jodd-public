# Local Folder vaults on Android — shared with Syncthing

**Date:** 2026-09-29
**Status:** approved in brainstorming, awaiting spec review
**Related:** the LocalFs vertical (`src-tauri/src/backend/localfs/`), the
Android platform seams (`paths.rs`, `secrets.rs`, docs/ARCHITECTURE.md), and
gotchas #8, #14, #31, #32.

## Why

On Android the account panel hides **Add Local Folder**, and
`add_local_account` refuses with *"Local vaults need arbitrary filesystem
access, which Android does not provide"* (`lib.rs:2605`). The user wants a
Local Folder vault on the tablet that **Syncthing on the same device can also
see**, so one vault is shared between desktop Jodd (Mac) and Android Jodd
through Syncthing, with no account and no server.

**Success:** on the Galaxy Tab S7, the user picks a folder Syncthing shares,
and notes written on either device appear on the other; an edit made on both
while they were apart shows up as a visible conflict copy on both, with neither
edit lost.

## Decisions taken in brainstorming (2026-09-29)

| # | Question | Decision |
|---|---|---|
| 1 | How Android may reach a shared folder | **All files access** (`MANAGE_EXTERNAL_STORAGE`). Jodd is sideloaded, so Google Play's restriction on this permission does not apply. App-private storage (invisible to Syncthing) and SAF `content://` access (a second I/O path) were rejected |
| 2 | How the user chooses the folder | **Android's own folder picker** (`ACTION_OPEN_DOCUMENT_TREE`), with the returned tree URI translated to a filesystem path. Internal storage (`primary:`) only |
| 3 | Syncthing conflict copies (`*.sync-conflict-*.eml`) | **Adopt each as a visible keep-both note** with a deterministic id, so every device converges on the same result |
| 4 | Plumbing | **A small Kotlin Tauri mobile plugin inside Jodd's Android project**, registered from Rust. JNI from Rust (cannot receive activity results) and patching `tauri-plugin-dialog` (a third vendored fork beside wry/tao) were rejected |

### What was measured while designing

- **`tauri-plugin-dialog` 2.7.1 has no folder picker on Android.** Its
  `DialogPlugin.kt` fires only `ACTION_GET_CONTENT` for files; `pick_folder`
  exists only in `desktop.rs`. So `openDialog({ directory: true })` cannot be
  reused on Android, even once the permission is granted.
- **LocalFs is plain `std::fs`.** It walks `Notes/**/*.eml`, keeps `.trash/`
  and `.meta/`, and names each note `<uuid>.eml`. Once Jodd holds All files
  access, the vertical works on an Android path unchanged.
- **LocalFs does not deduplicate.** `list_all_notes` returns every `*.eml`.
  A Syncthing conflict copy (`<uuid>.sync-conflict-<date>-<dev>.eml`) keeps the
  `.eml` extension and the same `X-Universally-Unique-Identifier`, so today both
  files are read and whichever the walk reaches last silently wins in the cache,
  and the pair fight on every scan.
- **LocalFs note saves are not atomic.** `transport.rs:369` and `:541` call
  `std::fs::write` in place. With one writer that was fine; with Syncthing's
  watcher rescanning within seconds, a half-written note can be propagated.

## 1. Components

### New

| Unit | Does | Tested by |
|---|---|---|
| `src-tauri/gen/android/app/src/main/java/co/bbmedia/jodd/StoragePlugin.kt` | A Tauri mobile plugin (`@TauriPlugin`) with three commands. **`hasAllFilesAccess`** → `Environment.isExternalStorageManager()`. **`requestAllFilesAccess`** → starts `Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION` with `package:co.bbmedia.jodd` for a result, and on return resolves with the re-checked value. **`pickFolder`** → starts `Intent.ACTION_OPEN_DOCUMENT_TREE` for a result and resolves with the tree URI string, or `null` when cancelled. Activity results use the plugin API's `startActivityForResult(invoke, intent, callbackName)`, the pattern `DialogPlugin.kt` uses | Device pass (no Kotlin CI harness) |
| `src-tauri/src/android_storage.rs` | `pub fn tree_uri_to_path(uri: &str) -> Result<PathBuf, String>` — **pure, compiled on every target**: accepts only authority `com.android.externalstorage.documents`, percent-decodes the tree document id, requires the `primary:` volume, returns `/storage/emulated/0/<rest>`; anything else → `"Choose a folder on the tablet's internal storage."`. `pub fn validate_android_vault_path(path: &Path, access_granted: bool) -> Result<(), String>` — pure. On Android only: the plugin registration (`tauri::plugin::Builder::new("jodd-storage")` with `register_android_plugin("co.bbmedia.jodd", "StoragePlugin")`) and three Tauri commands: `local_folder_access() -> bool`, `request_local_folder_access() -> bool`, `pick_local_folder() -> Option<String>` (picker → `tree_uri_to_path`) | Host unit tests |
| `src-tauri/src/backend/localfs/conflicts.rs` | `pub(crate) async fn adopt(vertical: &LocalFsVertical) -> usize`: finds files under `Notes/` whose **name** matches `^(.+)\.sync-conflict-(\d{8})-(\d{6})-([A-Z0-9]{7})\.eml$`, and turns each into an ordinary note (§2). Runs at the start of `list_all_notes` **on every platform** — a desktop vault in a Syncthing folder benefits too | Host tests over tempdir vaults |

### Changed

| Where | Change |
|---|---|
| `AndroidManifest.xml` | `<uses-permission android:name="android.permission.MANAGE_EXTERNAL_STORAGE" tools:ignore="ScopedStorage" />` (and the `xmlns:tools` namespace), with a comment: sideloaded distribution; needed so Syncthing and Jodd share one folder; Google Play restricts this permission |
| `add_local_account` (`lib.rs`) | The `cfg(android)` refusal is removed. On Android it calls `validate_android_vault_path(path, <plugin: hasAllFilesAccess>)` — refuses without access, refuses a path outside `/storage/emulated/0/` — then does a probe write (create and remove `.jodd-probe` in the folder). Desktop behaviour unchanged |
| `LocalFsVertical` saves (`transport.rs:369`, `:541`) | Write to `.<file name>.tmp` in the **same directory**, then `std::fs::rename` over the target: Syncthing (and a reader) only ever sees a complete file. The temp name does not end in `.eml`, so `all_eml` never lists it |
| LocalFs I/O error mapping (`perm` in `transport.rs`) | A `PermissionDenied` I/O error on Android becomes a `TransportError::Transient` whose message names the remedy: *"Jodd no longer has All files access — turn it on in Settings → Apps → Jodd."* |
| `Sidebar.svelte` | "Add Local Folder" is shown on Android too. `addLocalFolder` branches: Android → the §2 flow through the three commands; desktop → `openDialog` as today. The existing "Notes in this folder are not encrypted" confirmation appears on both |
| `src-tauri/Cargo.toml` | `uuid` gains the `v5` feature (`features = ["v4", "v5"]`) for the deterministic conflict id; no new crate |
| `lib.rs` `.setup()` / builder | Registers the plugin on Android only. The plugin performs no IPC during `.setup()`, so gotcha #32 does not apply |

**Unchanged:** the vault format (`Notes/**/<uuid>.eml`, `.trash/`, `.meta/`),
the RFC822 decoder, the Apple-HTML model, and desktop's picker. A vault shared
through Syncthing is byte-for-byte what desktop Jodd already reads and writes.

## 2. Flows

### First use — Add Local Folder on Android

1. The account panel shows **Add Local Folder**; the frontend calls
   `local_folder_access()`.
2. **Not granted yet:** Jodd shows its own explanation first — *"To share a
   folder with Syncthing, Jodd needs All files access. Android will open a
   settings page: turn Jodd on, then come back."* [Continue] [Cancel].
   Continue → `request_local_folder_access()` → Android's page for Jodd → on
   return, the re-checked value. Still off → *"Without All files access, Jodd
   can't use a folder that other apps can see."* Nothing is saved.
3. **Pick:** `pick_local_folder()` → Android's folder picker. Cancel → `null`,
   nothing happens.
4. **Translate:** `tree_uri_to_path` —
   `content://com.android.externalstorage.documents/tree/primary%3ASync%2FJodd`
   → document id `primary:Sync/Jodd` → `/storage/emulated/0/Sync/Jodd`. Any
   other volume or authority → *"Choose a folder on the tablet's internal
   storage."*
5. **Confirm and add:** the existing "not encrypted" confirmation, the name
   prompt, then `add_local_account(path)`. A folder that already contains
   `Notes/` (a vault Syncthing brought from the Mac) is used as-is; an empty
   folder gets `Notes/` as desktop does today.

### Process death while in Settings or the picker

Nothing is persisted before step 5, so there is nothing to recover: after
relaunch the user taps Add Local Folder again, and step 2 is skipped if access
was already granted. No pending-flow state is needed — unlike Android sign-in
(gotcha #8).

### Sync

Identical to desktop LocalFs: the UI's existing polling drives a full scan of
`Notes/`. Syncthing delivers files whenever it does; the next scan sees them.
Jodd's own saves are temp-then-rename, so Syncthing never propagates a partial
note.

### Access turned off later

Every LocalFs read and write fails with a permission error, mapped to
**Transient** with the remedy text (§1). No note is push-blocked (gotcha #14);
sync recovers as soon as access is back.

### A Syncthing conflict appears

At the start of each `list_all_notes`, `conflicts::adopt` handles every file
matching the conflict-name pattern:

1. Decode it with the existing `decode_eml`. **If it cannot be decoded, leave
   it in place, log it, and skip it** — a conflict file is only ever removed
   after its content is saved as a note.
2. **New id = UUIDv5** (a fixed Jodd namespace UUID declared in `conflicts.rs`)
   over the conflict file's vault-relative path, formatted as an Apple uuid.
3. Save through the vertical's normal save path (`save_note_full`) with that
   id, the same folder label, the conflict file's own created date, and title
   *"<original title> (conflict from Syncthing, <YYYY-MM-DD HH:MM>)"* where the
   date and time come from the conflict file **name**.
4. Delete the conflict file.

Every value written is derived from the conflict file itself, so the step is
**idempotent**: a kill between step 3 and 4 leaves the next scan rewriting the
same `<id>.eml` with the same bytes; and when the Mac and the tablet adopt the
same conflict at the same moment they write byte-identical files, which
Syncthing treats as already in sync rather than as a new conflict.

## 3. Errors

| Where | Situation | User sees |
|---|---|---|
| Add | Access off | *"Without All files access, Jodd can't use a folder that other apps can see."* |
| Add | SD card, Downloads, Drive, or any non-`primary:` pick | *"Choose a folder on the tablet's internal storage."* |
| Add | Path outside `/storage/emulated/0/` (UI bypassed) | refused by `validate_android_vault_path` |
| Add | Probe write fails | *"Jodd can't write to this folder."* |
| Add | Same folder or same vault name | the existing duplicate checks, unchanged |
| Sync | Permission error at runtime | Transient, *"Jodd no longer has All files access — turn it on in Settings → Apps → Jodd."*; nothing push-blocked |
| Scan | Undecodable conflict file | left in place, logged; never deleted |

## 4. Testing

All automated tests run in `cargo test --workspace` and `npx vitest run`.

1. **`tree_uri_to_path`:** primary root; nested; percent-encoded Thai folder
   names; trailing slash; SD-card volume id; Downloads provider; Drive provider;
   malformed URI; a `document/` (not `tree/`) URI.
2. **`validate_android_vault_path`:** granted/not granted × inside/outside the
   prefix; a `..` segment that would escape the prefix is refused.
3. **`conflicts.rs`** over tempdir vaults: one conflict → a new note with the
   right title, folder and created date, the conflict file gone, the original
   `<uuid>.eml` untouched; **idempotence** — a second scan changes nothing;
   **simulated kill** — new file present and conflict file still present →
   exactly one copy after the next scan; **two devices** — two vault copies
   adopting the same conflict produce byte-identical files; undecodable conflict
   left in place; nested folder; a note whose *title* contains "sync-conflict"
   is not matched.
4. **Atomic saves:** after create and update, no `.tmp` file remains and the
   file decodes completely.
5. **Frontend:** `Sidebar.android.test.ts` now asserts "Add Local Folder"
   **is** shown on Android (it currently asserts the opposite); `addLocalFolder`
   calls `pick_local_folder` on Android and `openDialog` on desktop; the
   encryption confirmation appears on both.

**Android build proof:** `android-encryption.yml` compiles the Rust side for
Android on any PR touching the Cargo manifests; the Kotlin plugin compiles only
inside an APK build, so a local debug APK build (which now works on the dev Mac)
is the Kotlin proof before merge. The four-ABI release build remains the known
gap.

### Device pass (Tab S7 + Mac, Syncthing on both)

The user installs Syncthing on both devices and shares one folder; Claude does
not install apps or pair devices. Claude can then drive most rows over adb
(screenshot before every input). Results are recorded as a new subsection,
"Local Folder on Android (All files access + Syncthing)", under
docs/ARCHITECTURE.md's existing "Platform seams (Android bring-up)" section.

| # | Action | Expect |
|---|---|---|
| L1 | Add Local Folder on an install without access | explanation → Settings → back → picker → account appears |
| L2 | Create a note on the tablet | appears in Mac Jodd after Syncthing syncs |
| L3 | Create a note in Mac Jodd | appears on the tablet |
| L4 | Pause Syncthing, edit the same note on both, resume | one "(conflict from Syncthing, …)" note on **both** devices; neither edit lost; no second conflict |
| L5 | Turn off All files access, open Jodd, turn it back on | Transient error with the remedy, then recovery; nothing push-blocked |
| L6 | Pick an SD card or Downloads folder | *"Choose a folder on the tablet's internal storage."* |
| L7 | Kill Jodd while on the Settings page, relaunch | no half account; Add again skips the permission step |
| L8 | Pick the folder that already holds the Mac's vault | existing notes appear as-is |

## Out of scope

- Google Play distribution (would require SAF instead of All files access).
- SD cards and non-`primary:` volumes.
- Encrypting LocalFs vaults (the existing disclosure stays).
- Multi-vault SSH paths (a separate design item raised the same day).
- File-change notifications; the existing polling full scan drives sync.
- Syncthing configuration, pairing, or bundling.

## Amendments (2026-09-29)

- **C1 — split into two parts.** Part 1 (this spec minus conflicts): All files
  access, the StoragePlugin, tree-URI translation, the `add_local_account`
  gate, atomic writes, and the Transient mapping for permission loss.
  Part 2: Syncthing conflict adoption (`conflicts.rs`, the `uuid` `v5` feature,
  device-pass row L4) — its own plan, written when the user uses Syncthing.
  Part 1 contains no Syncthing-specific code.
- **C2 — Android 10 and older are refused** with "Local Folder on Android needs
  Android 11 or newer." (`Environment.isExternalStorageManager()` is API 30+;
  minSdk is 24). The plugin reports `{ granted, supported }`.
- **C3 — the storage root and `Android/` are refused**: "Choose a folder inside
  internal storage, not its top level." / "Choose a folder outside Android/ —
  other apps can't see inside it."
- **C4 — the permission-loss mapping is scoped**: only `PermissionDenied` on
  Android becomes Transient (`localfs::transport::map_io`); every other I/O
  error, and desktop, keep the Permanent mapping.
- **C5 — `tree_uri_to_path` is stricter than C3 says.** It refuses `Android/`
  case-insensitively (shared storage is case-insensitive), refuses NUL bytes,
  and returns a normalised path rebuilt from its components. A trailing
  `/document/…` part of the URI is ignored in favour of the tree id.
- **C6 — the plugin calls are non-blocking** (`run_mobile_plugin_async`).
  `has_all_files_access` returns `Result<bool, String>`, so a plugin failure is
  shown as itself, never as "no access". Tauri 2.11.5 never resolves a pending
  plugin call if Android recreates the activity while Settings or the picker is
  open, so the frontend keeps a single-flow guard (one activity-result slot)
  and an orphan timer: on `visible` it waits 2 s; if the SAME plugin call is
  still pending it frees the button and retires that flow. `hidden` cancels the
  timer.
- **C7 — an unreadable vault fails its listing loudly.** On Android,
  `list_notes` and `list_notes_in_folder` first ask `has_all_files_access` for
  a Local Folder account (`localfs_listing_gate`), before any read or prune:
  without access they return `ACCESS_LOST`, and a failed check returns its own
  error. EACCES is not relied on, because MediaProvider can let `read_dir`
  succeed but filter it to Jodd's own files, which would make the listing
  silently partial and prune clean notes. Behind the gate, a directory that
  exists but cannot be read still fails through `ensure_readable` instead of
  listing empty; a missing `Notes/` (a new vault) still lists empty. Either
  failure reaches the ErrorBar from both `loadFolderNotes` and `loadNotes` (the
  All view, via `mergeAccountListings`), and the account's notes stay visible.
  Desktop has no access gate.
- **C8 — `probe_writable` never clobbers an existing `.jodd-probe`.** It
  creates its file with `create_new`, and falls back to a uuid-named probe if
  that name exists; it removes only the file it created.
