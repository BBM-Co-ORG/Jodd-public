//! iCloud diagnostics: the commands that measure a live CloudKit account.
//!
//! Two are reachable from the app — Settings › Account calls `icloud_census`
//! and `icloud_relocation_selftest`. The other four have no screen: they are
//! run from the devtools console during development and refuse in a release
//! build (`DIAGNOSTIC_IS_DEV_ONLY`), because three of them write to the
//! account.
//!
//! Every command here checks that the account is an iCloud one, builds the
//! vertical from the session the app already holds (`icloud_vertical_for`),
//! and logs the report it returns.

use tauri::State;

use crate::{account_backend_kind, accounts, log, AppState};
#[cfg(icloud_webview)]
use crate::icloud_vertical_for;

/// The M2 write census, run against the account's own live session.
///
/// **A diagnostic with no UI, on purpose.** `examples/icloud_probe` reads
/// icloud-md's stored cookie jar — scaffolding from before this project had an
/// iCloud session of its own — so measuring an account Jodd is already signed
/// into meant refreshing a third-party tool's expired session through a HAR
/// capture. This runs the same `census::report` over a zone read made with the
/// session the app is holding.
///
/// Invoke it from the app's devtools console:
///
/// ```js
/// await __TAURI__.core.invoke('icloud_census', { accountId: 'icloud:you@me.com' })
/// ```
///
/// Read-only: it walks the zone and sends nothing. Costs one whole-zone read,
/// which is the price every explicit refresh on this backend already pays.
#[tauri::command]
pub async fn icloud_census(account_id: String, state: State<'_, AppState>) -> Result<String, String> {
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    // Built directly rather than through `vertical_for`'s `Box<dyn Vertical>`:
    // the census is not part of any trait, because it is a diagnostic on this
    // one backend and widening a shared trait to carry it would put a stub in
    // front of three backends that have nothing to report.
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .write_census()
        .await
        .map_err(|e| e.to_string())?;
    // Logged as well as returned: the console shows it to whoever ran it, the
    // log is what gets pasted into a milestone note.
    log!("icloud_census[{account_id}]:\n{report}");
    Ok(report)
}

/// The four console-only iCloud diagnostics below (`icloud_write_selftest`,
/// `icloud_content_write_selftest`, `icloud_debug_note_history`,
/// `icloud_debug_record_lookup`) refuse in a release build. No screen calls
/// them — they are run from the devtools console, which a release build does
/// not have — so in release they were write access to a real account that only
/// injected script could reach. `icloud_census` and
/// `icloud_relocation_selftest` stay: Settings › Account calls them.
const DIAGNOSTIC_IS_DEV_ONLY: &str = "this diagnostic is only available in a development build";

/// The live write self-test, against the account's own session.
///
/// The counterpart of `icloud_census`, and the only thing that can confirm the
/// one part of M2 no test in this repo can: whether Apple accepts the
/// `records/modify` request shape, and whether the server really re-checks a
/// `recordChangeTag`.
///
/// **It writes.** `folder` must name a folder that already exists — make an
/// empty one in Apple Notes first — and the scratch note's `recordName` is
/// minted inside the vertical, never taken from here, so nothing a caller
/// passes can point this at a real note.
///
/// ```js
/// await __TAURI__.core.invoke('icloud_write_selftest',
///   { accountId: 'icloud:you@me.com', folder: 'Notes/FolderforJoddTesting' })
/// ```
#[tauri::command]
pub async fn icloud_write_selftest(
    account_id: String,
    folder: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if !cfg!(debug_assertions) {
        return Err(DIAGNOSTIC_IS_DEV_ONLY.to_string());
    }
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    if folder.trim().is_empty() {
        return Err("a destination folder is required — this writes to a real account, and \
                    a run with no destination would land a note in the user's root"
            .to_string());
    }
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .write_selftest(&folder)
        .await
        .map_err(|e| e.to_string())?;
    log!("icloud_write_selftest[{account_id}]:\n{report}");
    Ok(report)
}

/// The live **relocation** self-test — the write questions the census did not
/// close.
///
/// `icloud_census` measured the note DOCUMENT and answered no: `WRITABLE:
/// 0/776`, because Apple's per-character CRDT identity is on every note and
/// `compose` cannot mint the `CharID`s an insertion needs. Move, delete and
/// folder writes are a different question — none of them sends a document at
/// all — and nothing has measured them.
///
/// **It writes, and one thing it tests could destroy content**: whether
/// CloudKit's `update` really changes only the fields it is given, or replaces
/// the record. The vertical captures the note's exact document bytes first and
/// puts them back if a move disturbs them, but the containment that matters is
/// the argument: `folder` must name a folder holding **exactly one note**, and
/// that note is the subject. Prepare a throwaway folder with a throwaway note.
///
/// ```js
/// await __TAURI__.core.invoke('icloud_relocation_selftest',
///   { accountId: 'icloud:you@me.com', folder: 'Notes/Jodd M2 Test Folder' })
/// ```
#[tauri::command]
pub async fn icloud_relocation_selftest(
    account_id: String,
    folder: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    if folder.trim().is_empty() {
        return Err("a folder is required — it names the one note this test relocates, and \
                    there is no default that would be safe"
            .to_string());
    }
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .relocation_selftest(folder.trim())
        .await
        .map_err(|e| e.to_string())?;
    log!("icloud_relocation_selftest[{account_id}]:\n{report}");
    Ok(report)
}

/// **The content self-test (M2.5)** — proves the CRDT text-edit engine
/// against a REAL note that already carries CRDT identity, not a scratch
/// one Jodd minted (which never carries `substring`/`timestamp` — see
/// `ICloudVertical::content_write_selftest`'s own doc comment for why no
/// scratch equivalent exists). Appends a marker, verifies it landed
/// byte-exact, reverts to the original text, verifies that too. Net effect
/// on the note: none — but two real writes happen.
///
/// Mints/persists this account's CRDT replica id on first use
/// (`ensure_icloud_replica_id`) — the same durable identity every future
/// CRDT write on this account will reuse.
///
/// **Point this at a note you chose deliberately** — `recordName` is not
/// validated beyond "exists and is CRDT-writable"; there is no scratch-folder
/// containment the way `relocation_selftest` has, because there is no way to
/// manufacture a CRDT-carrying scratch note to contain it to.
///
/// ```js
/// await __TAURI__.core.invoke('icloud_content_write_selftest',
///   { accountId: 'icloud:you@me.com', recordName: 'test-m2-5-note-a51636f7' })
/// ```
#[tauri::command]
pub async fn icloud_content_write_selftest(
    account_id: String,
    record_name: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if !cfg!(debug_assertions) {
        return Err(DIAGNOSTIC_IS_DEV_ONLY.to_string());
    }
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    if record_name.trim().is_empty() {
        return Err("a recordName is required — this writes to a real, existing note, and \
                    there is no default that would be safe"
            .to_string());
    }
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .content_write_selftest(record_name.trim())
        .await
        .map_err(|e| e.to_string())?;
    log!("icloud_content_write_selftest[{account_id}]:\n{report}");
    Ok(report)
}

/// **Forensics for one disagreement** — three independent surfaces (Mac
/// Notes.app, icloud.com, iPhone) agree with each other and disagree with
/// Jodd's own zone walk about where a note lives. That rules out staleness on
/// Apple's side; what is left to look at is the raw record this backend
/// itself read, not another summary of it.
///
/// `titleContains` filters by a substring because the caller has no
/// `recordName` to hand — this reads its own account, walking the whole zone
/// like the census does, and prints identifiers and counts only.
///
/// ```js
/// await __TAURI__.core.invoke('icloud_debug_note_history',
///   { accountId: 'icloud:you@me.com', titleContains: 'M2 Note' })
/// ```
#[tauri::command]
pub async fn icloud_debug_note_history(
    account_id: String,
    title_contains: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if !cfg!(debug_assertions) {
        return Err(DIAGNOSTIC_IS_DEV_ONLY.to_string());
    }
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    if title_contains.trim().is_empty() {
        return Err("a title substring is required".to_string());
    }
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .debug_note_history(title_contains.trim())
        .await
        .map_err(|e| e.to_string())?;
    log!("icloud_debug_note_history[{account_id}]:\n{report}");
    Ok(report)
}

/// **A direct point read, not another walk of the change feed.** Every other
/// diagnostic on this backend — including `icloud_debug_note_history` above —
/// reads through `changes/zone`, a change feed whose own staleness is
/// gotcha #22's subject. This calls `records/lookup` instead: the per-record
/// endpoint icloud-md uses and this codebase never had. It exists to answer,
/// rather than assume, whether a feed/client disagreement is the whole
/// endpoint behind or just the feed — pass the `recordName` a prior
/// `icloud_debug_note_history` run already printed.
///
/// ```js
/// await __TAURI__.core.invoke('icloud_debug_record_lookup',
///   { accountId: 'icloud:you@me.com', recordName: 'E4513CCE-...' })
/// ```
#[tauri::command]
pub async fn icloud_debug_record_lookup(
    account_id: String,
    record_name: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if !cfg!(debug_assertions) {
        return Err(DIAGNOSTIC_IS_DEV_ONLY.to_string());
    }
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    if record_name.trim().is_empty() {
        return Err("a recordName is required".to_string());
    }
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .debug_record_lookup(record_name.trim())
        .await
        .map_err(|e| e.to_string())?;
    log!("icloud_debug_record_lookup[{account_id}]:\n{report}");
    Ok(report)
}
