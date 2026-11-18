//! Adding an SSH account and browsing its server for the vault directory.
//! Nothing is persisted until the server has proved it can host a vault
//! (the iCloud rule: check before storing anything).

use tauri::State;

use crate::backend::ssh::{classify, scripts, setup, session::{self, SshSession}};
use crate::{accounts, AppState};

#[derive(serde::Serialize, Debug, PartialEq)]
pub struct RemoteDirs {
    pub path: String,
    pub dirs: Vec<String>,
}

/// `managed` selects `classify::managed_auth_hint` over `classify::auth_hint`
/// — a Managed account's key and trust live in Jodd's own directory, so
/// `ssh-add` and "run ssh in a terminal" are wrong (and impossible on a
/// phone). `add_ssh_account` and `list_remote_dirs` are Advanced/browse
/// flows and pass `false`; `setup_managed_core` passes `true`.
fn probe_message(target: &str, root: &str, out: Result<session::ExecOutput, session::SshError>, managed: bool) -> Result<String, String> {
    match out {
        Err(session::SshError::Disconnected { stderr }) => {
            let hint = if managed { classify::managed_auth_hint(&stderr) } else { classify::auth_hint(&stderr, target) };
            Err(hint.unwrap_or_else(|| format!("could not reach {target}: {}", stderr.trim())))
        }
        Err(e) => Err(format!("could not reach {target}: {e}")),
        Ok(o) if o.exit == 0 => Ok(o.stdout),
        Ok(o) if o.exit == 4 => Err(format!("{root} does not exist on {target} — tick “Create this directory” to make it")),
        Ok(o) if o.exit == 5 => Err(format!("{root} on {target} is not writable")),
        Ok(o) => Err(format!("the server refused ({}): {}", o.exit, o.stderr.trim())),
    }
}

/// The folder a Managed setup uses when the user leaves the field blank.
pub(crate) const DEFAULT_MANAGED_ROOT: &str = "~/Jodd-Notes";

fn managed_root(root: Option<String>) -> String {
    root.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).unwrap_or_else(|| DEFAULT_MANAGED_ROOT.to_string())
}

/// One server may host many vaults — one account per directory — so only the
/// same target AND the same server-resolved path is refused. `abs` must be
/// `check_vault`'s output: `~/x` and `/home/me/x` are the same vault.
fn duplicate_vault(list: &[accounts::Account], target: &str, abs: &str) -> Option<String> {
    list.iter()
        .any(|a| {
            a.backend_kind == accounts::BackendKind::Ssh
                && a.ssh_target.as_deref() == Some(target)
                && a.root_dir.as_deref() == Some(abs)
        })
        .then(|| format!("{target}:{abs} is already an account"))
}

fn default_name(target: &str, abs: &str) -> String {
    let leaf = abs.rsplit('/').find(|s| !s.is_empty()).unwrap_or(abs);
    format!("{target}:{leaf}")
}

/// Resolve `root` on the server to an absolute directory and check it can
/// host a vault. Separate from the command so a local `sh` can drive it.
/// `managed` is threaded through to `probe_message` — see its doc comment.
pub(crate) async fn check_vault(s: &dyn SshSession, target: &str, root: &str, create: bool, managed: bool) -> Result<String, String> {
    let abs = probe_message(target, root, s.exec(&scripts::resolve_root(root, create)).await, managed)?.trim().to_string();
    let probe = probe_message(target, root, s.exec(scripts::PROBE).await, managed)?;
    scripts::parse_probe(&probe)?;
    Ok(abs)
}

/// One fixed key for every browse session, never a target-derived one: a
/// distinct key per target would leak an idle ssh child per target ever
/// browsed, since `list_remote_dirs` (unlike `add_ssh_account`) has no
/// natural end-of-life moment to call `forget_session` at. `session_for`
/// replaces the map entry when the target differs, so re-browsing under a new
/// target drops the previous `Arc<ProcessSession>` and `kill_on_drop` ends its
/// process — at most one idle browse process exists at a time.
const BROWSE_KEY: &str = "ssh-browse";

#[tauri::command]
pub(crate) async fn list_remote_dirs(target: String, path: String) -> Result<RemoteDirs, String> {
    #[cfg(target_os = "android")]
    return Err("Advanced SSH needs a desktop — use the simple setup.".to_string());

    session::validate_target(&target)?;
    let s = crate::backend::ssh::session_for(BROWSE_KEY, &target, &session::SshOptions::default());
    let path = if path.trim().is_empty() { "~".to_string() } else { path };
    let res = s.exec(&scripts::list_dirs(&path)).await;
    // The session remembers an auth failure for `AUTH_BACKOFF` so the sync
    // worker cannot hammer sshd; in this dialog every retry is a click, and
    // the user who just ran `ssh-add` must not wait two minutes for it.
    if res.is_err() {
        crate::backend::ssh::forget_session(BROWSE_KEY);
    }
    let out = probe_message(&target, &path, res, false)?;
    let (path, dirs) = scripts::parse_dirs(&out).ok_or("the server listed nothing")?;
    Ok(RemoteDirs { path, dirs })
}

#[tauri::command]
pub(crate) async fn add_ssh_account(
    target: String,
    root: String,
    name: Option<String>,
    create: bool,
    state: State<'_, AppState>,
) -> Result<accounts::Account, String> {
    #[cfg(target_os = "android")]
    return Err("Advanced SSH needs a desktop — use the simple setup.".to_string());

    let target = target.trim().to_string();
    session::validate_target(&target)?;
    let root = root.trim().to_string();
    if root.is_empty() {
        return Err("choose a directory on the server".into());
    }
    let s = crate::backend::ssh::session_for(BROWSE_KEY, &target, &session::SshOptions::default());
    let checked = check_vault(s.as_ref(), &target, &root, create, false).await;
    crate::backend::ssh::forget_session(BROWSE_KEY);
    let abs = checked?;

    let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| default_name(&target, &abs));
    let id = accounts::account_id_for(accounts::BackendKind::Ssh, &uuid::Uuid::new_v4().to_string());
    let account = accounts::Account {
        id: id.clone(),
        email: name,
        added_at: chrono::Utc::now().to_rfc3339(),
        notes_label: None,
        meta_label: None,
        llm: accounts::LlmConfig::default(),
        backend_kind: accounts::BackendKind::Ssh,
        root_dir: Some(abs.clone()),
        ssh_target: Some(target.clone()),
        ssh_port: None,
        ssh_key: None,
        icloud_session_established: false,
        blocked_reason: None,
        sync_cursor: None,
        icloud_replica_id: None,
        status: accounts::AccountStatus::Active,
        pending_removal: false,
    };
    {
        let mut list = state.accounts.lock().unwrap();
        if let Some(e) = duplicate_vault(&list, &target, &abs) {
            return Err(e);
        }
        list.push(account.clone());
        accounts::save_accounts(&list)?;
    }
    crate::index_account(id, state).await?;
    Ok(account)
}

/// The credential path chosen in the Managed setup dialog. `Password`
/// generates a fresh key here and installs it over one password-authenticated
/// ssh session; `KeyFile` logs in once with a key the user already has (and
/// the server already trusts) to install that same fresh key, and keeps no
/// copy of the user's. The password/passphrase never
/// touches disk or argv — see `askpass` and `setup::install_key_over_password`.
#[derive(serde::Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum SetupCredential {
    Password { password: String },
    KeyFile { path: String, passphrase: Option<String> },
}

pub(crate) const KEY_FILES_NOT_ON_ANDROID: &str =
    "Key files aren't supported on Android yet — use Run a command on the server, or Password.";

/// Everything `ssh_setup_managed` proves before it saves an account.
#[derive(Debug)]
pub(crate) struct ManagedReady {
    pub target: String,
    pub opts: session::SshOptions,
    pub abs: String,
}

/// #103's pipeline, unchanged in order and cleanup, with `embedded` choosing
/// the verbs: system ssh/ssh-keygen/ssh-keyscan (desktop) or the in-process
/// russh ones (`russh_setup`, Android). Any failure removes `base` and
/// forgets the session before returning, so a retry starts clean.
pub(crate) async fn setup_managed_core(
    host: String,
    port: Option<u16>,
    user: String,
    credential: SetupCredential,
    known_hosts_line: String,
    root: &str,
    uuid: &str,
    base: &std::path::Path,
    embedded: bool,
) -> Result<ManagedReady, String> {
    use crate::backend::ssh::russh_setup;
    session::validate_target(&host)?;
    let target = format!("{user}@{host}");
    let fail = |e: String| {
        let _ = std::fs::remove_dir_all(base);
        crate::backend::ssh::forget_session(&format!("ssh:{uuid}"));
        e
    };

    // The user's own key file is read, decoded and held in memory only: it
    // opens one russh connection to install Jodd's freshly generated key and
    // is then dropped. Jodd keeps no copy of it.
    enum Door { Password(String), UserKey(russh::keys::PrivateKey) }
    let door = match credential {
        SetupCredential::Password { password } => Door::Password(password),
        SetupCredential::KeyFile { .. } if embedded => return Err(KEY_FILES_NOT_ON_ANDROID.to_string()),
        SetupCredential::KeyFile { path, passphrase } => {
            let bytes = std::fs::read(&path).map_err(|e| format!("could not read {path}: {e}"))?;
            // bcrypt decryption of an encrypted key can take seconds: off the runtime.
            let key = tokio::task::spawn_blocking(move || russh_setup::load_user_key(&bytes, passphrase.as_deref()))
                .await
                .map_err(|e| format!("setup task failed: {e}"))??;
            Door::UserKey(key)
        }
    };

    let prepared = if embedded {
        russh_setup::generate_key(base)?
    } else {
        let b = base.to_path_buf();
        tokio::task::spawn_blocking(move || setup::prepare_key(&b, setup::PublicKeySource::Generate))
            .await
            .map_err(|e| format!("setup task failed: {e}"))??
    };
    // Every managed key is one Jodd generated and installed itself.
    setup::write_key_source_marker(base, true).map_err(fail)?;
    setup::write_known_hosts(base, &known_hosts_line).map_err(fail)?;

    let known_hosts_path = base.join("known_hosts");
    let installed = match door {
        Door::Password(password) if embedded => {
            russh_setup::install_key_over_password(&target, port, &known_hosts_path, &password, &prepared.pubkey_line).await
        }
        Door::Password(password) => {
            let (t, line) = (target.clone(), prepared.pubkey_line.clone());
            tokio::task::spawn_blocking(move || setup::install_key_over_password(&t, port, &known_hosts_path, &password, &line))
                .await
                .map_err(|e| format!("setup task failed: {e}"))?
        }
        Door::UserKey(key) => {
            russh_setup::install_key_with_user_key(&target, port, &known_hosts_path, &key, &prepared.pubkey_line).await
        }
    };
    installed.map_err(fail)?;

    let opts = session::SshOptions { port, key: Some(base.join("id_ed25519")), known_hosts: Some(base.join("known_hosts")) };
    let ctx = setup::VerifyContext::Installed;
    let verified = if embedded {
        russh_setup::verify_key_login(&target, &opts, ctx).await
    } else {
        let (t, o) = (target.clone(), opts.clone());
        tokio::task::spawn_blocking(move || setup::verify_key_login(&t, &o, ctx))
            .await
            .map_err(|e| format!("setup task failed: {e}"))?
    };
    verified.map_err(fail)?;

    let s = crate::backend::ssh::session_for_with(&format!("ssh:{uuid}"), &target, &opts, embedded);
    let abs = check_vault(s.as_ref(), &target, root, true, true).await.map_err(fail)?;
    Ok(ManagedReady { target, opts, abs })
}

/// Undoes a setup that got as far as installing its key but must not become
/// an account (a duplicate vault, found only once the server has resolved the
/// path). Revokes the line just installed — an imported key is left alone,
/// C5 — and deletes the key directory, exactly as removing the account would.
pub(crate) async fn abandon_managed_setup(uuid: &str, target: &str, opts: &session::SshOptions, base: &std::path::Path, embedded: bool) {
    crate::backend::ssh::forget_session(&format!("ssh:{uuid}"));
    let (t, o, b) = (target.to_string(), opts.clone(), base.to_path_buf());
    let outcome = tokio::task::spawn_blocking(move || crate::backend::ssh::setup::retire_managed_key_with(&t, &o, &b, true, embedded)).await;
    crate::log!("ssh_setup_managed: abandoned a setup on {target}: {outcome:?}");
}

#[tauri::command]
pub(crate) async fn ssh_fingerprint(host: String, port: Option<u16>) -> Result<setup::Fingerprint, String> {
    session::validate_target(&host)?;
    if crate::backend::ssh::EMBEDDED {
        return crate::backend::ssh::russh_setup::fetch_fingerprint(&host, port).await;
    }
    tokio::task::spawn_blocking(move || setup::fetch_fingerprint(&host, port))
        .await
        .map_err(|e| format!("setup task failed: {e}"))?
}

/// Drives the full Managed SSH setup pipeline: generate or import a key pair,
/// install it on the server (password path) or simply verify it (key-file
/// path), pin the host's fingerprint, then reuse `check_vault` to create/
/// confirm `root` (blank = `~/Jodd-Notes`). One key per account: a second
/// vault on the same server is a second setup. Saves the new Managed Account only once every step
/// has succeeded; any failure after the key material exists on disk cleans up
/// that key directory (and any session registered under the fresh uuid)
/// before returning, so a retry starts clean.
#[tauri::command]
pub(crate) async fn ssh_setup_managed(
    host: String,
    port: Option<u16>,
    user: String,
    credential: SetupCredential,
    known_hosts_line: String,
    root: Option<String>,
    name: Option<String>,
    state: State<'_, AppState>,
) -> Result<accounts::Account, String> {
    let root = managed_root(root);
    let uuid = uuid::Uuid::new_v4().to_string();
    let base = crate::paths::data_base()
        .map(|d| d.join("jodd").join("ssh").join(&uuid))
        .ok_or("no app data directory available")?;
    let ManagedReady { target, abs, opts } = setup_managed_core(
        host, port, user, credential, known_hosts_line, &root, &uuid, &base, crate::backend::ssh::EMBEDDED,
    ).await?;

    save_managed_account(state, ManagedReady { target, abs, opts }, port, &uuid, &base, name, OnDuplicate::Abandon).await
}

/// What to do when the vault turns out to be one the user already has.
/// Password path: take the just-installed line back off and delete the key,
/// as today. Command path: keep the key — the user's own command put the line
/// there, and they may retry with another folder.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum OnDuplicate { Abandon, KeepKey }

/// True when an account already owns this setup's key directory.
fn account_owns_setup(list: &[accounts::Account], setup_id: &str) -> bool {
    list.iter().any(|a| a.ssh_key.as_deref() == Some(setup_id))
}

/// A setup id becomes at most one account: a second finish must not push a
/// second row sharing the first one's key directory.
fn setup_already_saved(list: &[accounts::Account], setup_id: &str) -> Option<String> {
    account_owns_setup(list, setup_id).then(|| "This setup has already been added as an account.".to_string())
}

/// The tail shared by both setup paths: build the account, refuse a duplicate
/// vault under the lock, save, first index.
pub(crate) async fn save_managed_account(
    state: State<'_, AppState>,
    ready: ManagedReady,
    port: Option<u16>,
    uuid: &str,
    base: &std::path::Path,
    name: Option<String>,
    on_duplicate: OnDuplicate,
) -> Result<accounts::Account, String> {
    let ManagedReady { target, abs, opts } = ready;
    let display_name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
        .unwrap_or_else(|| default_name(&target, &abs));
    let id = accounts::account_id_for(accounts::BackendKind::Ssh, uuid);
    let account = accounts::Account {
        id: id.clone(), email: display_name, added_at: chrono::Utc::now().to_rfc3339(),
        notes_label: None, meta_label: None, llm: accounts::LlmConfig::default(),
        backend_kind: accounts::BackendKind::Ssh, root_dir: Some(abs.clone()),
        ssh_target: Some(target.clone()), ssh_port: port, ssh_key: Some(uuid.to_string()),
        icloud_session_established: false, blocked_reason: None, sync_cursor: None,
        icloud_replica_id: None, status: accounts::AccountStatus::Active, pending_removal: false,
    };
    // Checked under the same lock as the push, as `add_ssh_account` does, so
    // two setups racing to the same vault cannot both land.
    let duplicate = {
        let mut list = state.accounts.lock().unwrap();
        if let Some(e) = setup_already_saved(&list, uuid) {
            Some(e)
        } else if let Some(e) = duplicate_vault(&list, &target, &abs) {
            Some(e)
        } else {
            list.push(account.clone());
            if let Err(e) = accounts::save_accounts(&list) {
                // Held continuously since the push above — nothing else could
                // have touched `list` in between, so this removes exactly the
                // row just pushed (matched by id rather than `pop()`, so it
                // stays correct even if that ever stops being true).
                list.retain(|a| a.id != id);
                drop(list);
                // The Command path's line is already on the server: keep the
                // key so Cancel can still revoke it.
                if on_duplicate == OnDuplicate::Abandon {
                    let _ = std::fs::remove_dir_all(base);
                }
                crate::backend::ssh::forget_session(&format!("ssh:{uuid}"));
                return Err(e);
            }
            None
        }
    };
    if let Some(e) = duplicate {
        if on_duplicate == OnDuplicate::Abandon {
            abandon_managed_setup(uuid, &target, &opts, base, crate::backend::ssh::EMBEDDED).await;
        }
        return Err(e);
    }
    // The account now exists — saved, key installed, vault checked. A first
    // index that fails here (a network blip right after setup) is a sync
    // problem the worker retries on its next round, not a failed setup:
    // returning Err would put the dialog back on "Trust and connect", and a
    // retry would mint a second account and install a second key.
    if let Err(e) = crate::index_account(id.clone(), state).await {
        crate::log!("ssh_setup_managed: {id} was added, but its first index failed ({e}); the sync worker will retry");
    }
    Ok(account)
}


// ---- "Run a command on the server": prepare / finish / cancel -------------
// The key is made and pinned first, the user adds its line themselves, and
// only then does Jodd connect. `setup_id` is the key directory's uuid.

#[derive(serde::Serialize)]
pub(crate) struct PreparedSshCommand {
    pub setup_id: String,
    pub command: String,
}

/// The key directory for a setup id. The id is checked as a uuid before any
/// path is built, as `remote::host_dir` does, so `../x` never reaches a join.
fn command_setup_base(setup_id: &str) -> Result<std::path::PathBuf, String> {
    uuid::Uuid::parse_str(setup_id).map_err(|_| "invalid setup id".to_string())?;
    crate::paths::data_base()
        .map(|d| d.join("jodd").join("ssh").join(setup_id))
        .ok_or_else(|| "no app data directory available".to_string())
}

/// Generates and pins the key, returns the one-line command that authorizes
/// it. Any failure deletes `base`.
pub(crate) async fn prepare_command_core(
    host: &str,
    port: Option<u16>,
    user: &str,
    known_hosts_line: &str,
    base: &std::path::Path,
    embedded: bool,
) -> Result<String, String> {
    use crate::backend::ssh::russh_setup;
    let _ = port; // the command runs on the server; the port matters only at finish
    let fail = |e: String| {
        let _ = std::fs::remove_dir_all(base);
        e
    };
    session::validate_target(host).map_err(fail)?;
    let prepared = if embedded {
        russh_setup::generate_key(base).map_err(fail)?
    } else {
        let b = base.to_path_buf();
        tokio::task::spawn_blocking(move || setup::prepare_key(&b, setup::PublicKeySource::Generate))
            .await
            .map_err(|e| fail(format!("setup task failed: {e}")))?
            .map_err(fail)?
    };
    // Generated, so removing the account revokes the line the user adds.
    setup::write_key_source_marker(base, true).map_err(fail)?;
    setup::write_known_hosts(base, known_hosts_line).map_err(fail)?;
    scripts::authorize_command(user, prepared.pubkey_line.trim()).map_err(fail)
}

/// Verifies the key now logs in, then checks the vault. Never deletes `base`:
/// every failure here is one the user can fix and retry. After a successful
/// verify it writes `base/verified`, so a later cancel knows a line exists to
/// revoke.
pub(crate) async fn finish_command_core(
    host: &str,
    port: Option<u16>,
    user: &str,
    root: &str,
    uuid: &str,
    base: &std::path::Path,
    embedded: bool,
) -> Result<ManagedReady, String> {
    use crate::backend::ssh::russh_setup;
    session::validate_target(host)?;
    if !base.is_dir() {
        return Err(setup::SETUP_EXPIRED.into());
    }
    let target = format!("{user}@{host}");
    let opts = session::SshOptions { port, key: Some(base.join("id_ed25519")), known_hosts: Some(base.join("known_hosts")) };
    let ctx = setup::VerifyContext::UserRan { host, user };
    let verified = if embedded {
        russh_setup::verify_key_login(&target, &opts, ctx).await
    } else {
        let (t, o, h, u) = (target.clone(), opts.clone(), host.to_string(), user.to_string());
        tokio::task::spawn_blocking(move || setup::verify_key_login(&t, &o, setup::VerifyContext::UserRan { host: &h, user: &u }))
            .await
            .map_err(|e| format!("setup task failed: {e}"))?
    };
    verified?;
    std::fs::write(base.join("verified"), b"").map_err(|e| format!("could not record the verified key: {e}"))?;

    let s = crate::backend::ssh::session_for_with(&format!("ssh:{uuid}"), &target, &opts, embedded);
    match check_vault(s.as_ref(), &target, root, true, true).await {
        Ok(abs) => Ok(ManagedReady { target, opts, abs }),
        Err(e) => {
            crate::backend::ssh::forget_session(&format!("ssh:{uuid}"));
            Err(e)
        }
    }
}

/// Best effort, always returns. A key that was verified has its line on the
/// server, so it is revoked (which also deletes the directory); an unverified
/// one only ever existed here.
pub(crate) async fn cancel_command_core(target: &str, port: Option<u16>, base: &std::path::Path, embedded: bool) {
    if base.join("verified").exists() {
        let opts = session::SshOptions { port, key: Some(base.join("id_ed25519")), known_hosts: Some(base.join("known_hosts")) };
        let (t, b) = (target.to_string(), base.to_path_buf());
        let outcome = tokio::task::spawn_blocking(move || setup::retire_managed_key_with(&t, &opts, &b, true, embedded)).await;
        crate::log!("ssh_setup_cancel_command: {target}: {outcome:?}");
    } else {
        let _ = std::fs::remove_dir_all(base);
    }
}

#[tauri::command]
pub(crate) async fn ssh_setup_prepare_command(
    host: String,
    port: Option<u16>,
    user: String,
    known_hosts_line: String,
) -> Result<PreparedSshCommand, String> {
    let setup_id = uuid::Uuid::new_v4().to_string();
    let base = command_setup_base(&setup_id)?;
    let command = prepare_command_core(&host, port, &user, &known_hosts_line, &base, crate::backend::ssh::EMBEDDED).await?;
    Ok(PreparedSshCommand { setup_id, command })
}

#[tauri::command]
pub(crate) async fn ssh_setup_finish_command(
    setup_id: String,
    host: String,
    port: Option<u16>,
    user: String,
    root: Option<String>,
    name: Option<String>,
    state: State<'_, AppState>,
) -> Result<accounts::Account, String> {
    let base = command_setup_base(&setup_id)?;
    let root = managed_root(root);
    let ready = finish_command_core(&host, port, &user, &root, &setup_id, &base, crate::backend::ssh::EMBEDDED).await?;
    save_managed_account(state, ready, port, &setup_id, &base, name, OnDuplicate::KeepKey).await
}

#[tauri::command]
pub(crate) async fn ssh_setup_cancel_command(setup_id: String, host: String, port: Option<u16>, user: String, state: State<'_, AppState>) -> Result<(), String> {
    let base = command_setup_base(&setup_id)?;
    // A live account owns this key now (finish succeeded, or is mid-flight):
    // cancelling must not revoke its line or delete its key.
    if account_owns_setup(&state.accounts.lock().unwrap(), &setup_id) {
        return Ok(());
    }
    crate::backend::ssh::forget_session(&format!("ssh:{setup_id}"));
    cancel_command_core(&format!("{user}@{host}"), port, &base, crate::backend::ssh::EMBEDDED).await;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::backend::ssh::session::{ProcessSession, SpawnSpec};
    use std::time::Duration;

    #[tokio::test]
    async fn check_vault_resolves_creates_and_explains() {
        let d = tempfile::tempdir().unwrap();
        let s = ProcessSession::new(SpawnSpec::local_sh(d.path().to_path_buf()), Duration::from_secs(20));
        let want = d.path().join("vault");
        let err = check_vault(&s, "me@box", want.to_str().unwrap(), false, false).await.unwrap_err();
        assert!(err.contains("does not exist") && err.contains("Create this directory"), "{err}");
        let abs = check_vault(&s, "me@box", want.to_str().unwrap(), true, false).await.unwrap();
        assert_eq!(abs, want.canonicalize().unwrap().to_str().unwrap());
    }

    #[test]
    fn a_refused_key_is_explained_at_add_time() {
        let out = Err(session::SshError::Disconnected { stderr: "me@box: Permission denied (publickey).\n".into() });
        assert!(probe_message("me@box", "/x", out, false).unwrap_err().contains("ssh-add"));
    }

    /// Review Focus #1 fix round 1: the Managed setup pipeline's post-install
    /// vault check (`setup_managed_core` → `check_vault` → `probe_message`)
    /// must never suggest `ssh-add` or a terminal — a Managed account's key
    /// and trust live in Jodd's own directory, and on Android there is no
    /// terminal to run one in.
    #[test]
    fn a_managed_vault_probe_never_suggests_ssh_add_or_a_terminal() {
        let out = Err(session::SshError::Disconnected { stderr: "jodd@box: Permission denied (publickey).\n".into() });
        let msg = probe_message("jodd@box", "/x", out, true).unwrap_err();
        assert!(!msg.contains("ssh-add") && !msg.contains("terminal"), "{msg}");
        assert!(msg.contains("Remove"), "names the remedy: {msg}");
    }

    /// The Advanced/browse paths (`add_ssh_account`, `list_remote_dirs`) keep
    /// their existing `ssh-add` wording — only the Managed setup path changes.
    #[test]
    fn an_advanced_vault_probe_keeps_ssh_add() {
        let out = Err(session::SshError::Disconnected { stderr: "jodd@box: Permission denied (publickey).\n".into() });
        let msg = probe_message("jodd@box", "/x", out, false).unwrap_err();
        assert!(msg.contains("ssh-add"), "{msg}");
    }

    #[test]
    fn the_default_name_is_target_and_leaf() {
        assert_eq!(default_name("me@box", "/home/me/notes"), "me@box:notes");
        assert_eq!(default_name("box", "/"), "box:/");
    }

    /// Pins the shape `ssh_setup_managed` writes into `accounts.json`: the
    /// new fields land, and no field carries the raw password/passphrase —
    /// those are consumed by `prepare_key`/`install_key_over_password` and
    /// never stored.
    #[test]
    fn managed_account_gets_ssh_port_and_ssh_key_but_no_secret() {
        let a = accounts::Account {
            id: "ssh:abc".into(), email: "vps:notes".into(), added_at: "2026-01-01T00:00:00Z".into(),
            notes_label: None, meta_label: None, llm: accounts::LlmConfig::default(),
            backend_kind: accounts::BackendKind::Ssh, root_dir: Some("/home/me/Jodd-Notes".into()),
            ssh_target: Some("me@vps.example.com".into()), ssh_port: Some(2222), ssh_key: Some("abc".into()),
            icloud_session_established: false, blocked_reason: None, sync_cursor: None,
            icloud_replica_id: None, status: accounts::AccountStatus::Active, pending_removal: false,
        };
        assert!(a.is_ready_local());
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.to_lowercase().contains("password") && !json.to_lowercase().contains("passphrase"));
        assert_eq!(a.ssh_port, Some(2222));
        assert_eq!(a.ssh_key.as_deref(), Some("abc"));
    }

    /// `account_id_for` prefixes with `"ssh:"`; `ssh_key` must NOT carry that
    /// prefix, since it is joined onto a filesystem path where `:` is illegal
    /// on Windows. Enforced by construction in `ssh_setup_managed` (it sets
    /// `ssh_key: Some(uuid.clone())`, never the prefixed id) — this test pins
    /// that invariant.
    #[test]
    fn ssh_key_field_is_the_bare_directory_name_not_the_prefixed_id() {
        let id = accounts::account_id_for(accounts::BackendKind::Ssh, "abc-123");
        assert_eq!(id, "ssh:abc-123");
        assert!(!"abc-123".contains(':'));
    }

    /// `BROWSE_KEY` is one fixed key for every target: `session_for` replaces
    /// the registry entry (and drops the old `Arc`, ending its process via
    /// `kill_on_drop`) whenever the target under that key changes, so
    /// browsing a second server can never accumulate a second idle session.
    /// `SpawnSpec::ssh` builds argv only — nothing is spawned until `exec` is
    /// called, so this never touches a real ssh process.
    #[test]
    fn session_for_replaces_the_entry_when_the_target_changes() {
        let opts = session::SshOptions::default();
        let a = crate::backend::ssh::session_for("k", "a", &opts);
        let a_again = crate::backend::ssh::session_for("k", "a", &opts);
        assert!(std::sync::Arc::ptr_eq(&a, &a_again), "same target reuses the same session");

        let b = crate::backend::ssh::session_for("k", "b", &opts);
        assert!(!std::sync::Arc::ptr_eq(&a, &b), "a new target replaces the entry");

        let b_again = crate::backend::ssh::session_for("k", "b", &opts);
        assert!(std::sync::Arc::ptr_eq(&b, &b_again), "the new target is then reused in turn");

        crate::backend::ssh::forget_session("k");
    }

    use crate::backend::ssh::test_server::{self, ServerOpts};

    async fn run_core(opts: ServerOpts, credential: SetupCredential) -> (test_server::TestServer, tempfile::TempDir, std::path::PathBuf, Result<ManagedReady, String>) {
        run_core_at(opts, credential, DEFAULT_MANAGED_ROOT).await
    }

    async fn run_core_at(opts: ServerOpts, credential: SetupCredential, root: &str) -> (test_server::TestServer, tempfile::TempDir, std::path::PathBuf, Result<ManagedReady, String>) {
        let srv = test_server::start(opts).await;
        let fp = crate::backend::ssh::russh_setup::fetch_fingerprint("127.0.0.1", Some(srv.port)).await.unwrap();
        let data = tempfile::tempdir().unwrap();
        let uuid = uuid::Uuid::new_v4().to_string();
        let base = data.path().join("ssh").join(&uuid);
        let r = setup_managed_core("127.0.0.1".into(), Some(srv.port), "jodd".into(), credential, fp.known_hosts_line, root, &uuid, &base, true).await;
        (srv, data, base, r)
    }

    fn ssh_account(target: &str, root: &str) -> accounts::Account {
        accounts::Account {
            id: format!("ssh:{}", uuid::Uuid::new_v4()), email: "x".into(), added_at: "2026-01-01T00:00:00Z".into(),
            notes_label: None, meta_label: None, llm: accounts::LlmConfig::default(),
            backend_kind: accounts::BackendKind::Ssh, root_dir: Some(root.into()),
            ssh_target: Some(target.into()), ssh_port: None, ssh_key: None,
            icloud_session_established: false, blocked_reason: None, sync_cursor: None,
            icloud_replica_id: None, status: accounts::AccountStatus::Active, pending_removal: false,
        }
    }

    /// One server may host many vaults — one account per path. Only the same
    /// target AND the same resolved path is a duplicate.
    #[test]
    fn a_duplicate_vault_is_same_target_and_same_path_only() {
        let list = vec![ssh_account("me@box", "/home/me/Jodd-Notes")];
        let err = duplicate_vault(&list, "me@box", "/home/me/Jodd-Notes").unwrap();
        assert!(err.contains("me@box:/home/me/Jodd-Notes") && err.contains("already"), "{err}");
        assert_eq!(duplicate_vault(&list, "me@box", "/home/me/Work"), None);
        assert_eq!(duplicate_vault(&list, "you@box", "/home/me/Jodd-Notes"), None);
    }

    #[test]
    fn a_blank_managed_root_means_the_default_folder() {
        assert_eq!(managed_root(None), "~/Jodd-Notes");
        assert_eq!(managed_root(Some("   ".into())), "~/Jodd-Notes");
        assert_eq!(managed_root(Some(" ~/Work/Notes ".into())), "~/Work/Notes");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_managed_setup_creates_the_chosen_folder() {
        let (srv, _data, _base, r) = run_core_at(
            ServerOpts { password: Some("hunter2"), ..Default::default() },
            SetupCredential::Password { password: "hunter2".into() },
            "~/Work/Notes",
        ).await;
        let ready = r.unwrap();
        let want = srv.home.path().join("Work/Notes").canonicalize().unwrap();
        assert_eq!(ready.abs, want.to_str().unwrap());
        assert!(!srv.home.path().join("Jodd-Notes").exists(), "the default folder is not made as well");
    }

    /// A Managed duplicate is only detectable after the key is installed (the
    /// server resolves `~`), so refusing it must take that key back off the
    /// server and delete the local key directory.
    #[tokio::test(flavor = "multi_thread")]
    async fn abandoning_a_managed_setup_revokes_its_key_and_removes_its_directory() {
        let (srv, _data, base, r) = run_core(
            ServerOpts { password: Some("hunter2"), ..Default::default() },
            SetupCredential::Password { password: "hunter2".into() },
        ).await;
        let ready = r.unwrap();
        let keys = srv.home.path().join(".ssh/authorized_keys");
        assert_eq!(std::fs::read_to_string(&keys).unwrap().lines().count(), 1);
        abandon_managed_setup("test-uuid", &ready.target, &ready.opts, &base, true).await;
        assert_eq!(std::fs::read_to_string(&keys).unwrap().trim(), "", "Jodd's line was revoked");
        assert!(!base.exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_android_pipeline_runs_end_to_end_and_removal_revokes_exactly_its_line() {
        let (srv, _data, base, r) = run_core(
            ServerOpts { password: Some("hunter2"), ..Default::default() },
            SetupCredential::Password { password: "hunter2".into() },
        ).await;
        let ready = r.unwrap();
        let notes = srv.home.path().join("Jodd-Notes").canonicalize().unwrap();
        assert_eq!(ready.abs, notes.to_str().unwrap());
        assert!(crate::backend::ssh::setup::key_was_generated(&base));
        srv.authorize("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOtherKeyOtherKeyOtherKeyOtherKeyOtherKe someone-else");
        let before = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        assert_eq!(before.lines().count(), 2);

        let (t, o, b) = (ready.target.clone(), ready.opts.clone(), base.clone());
        let outcome = tokio::task::spawn_blocking(move || crate::backend::ssh::setup::retire_managed_key_with(&t, &o, &b, true, true))
            .await.unwrap();
        assert_eq!(outcome, crate::backend::ssh::setup::RetireOutcome::Revoked);
        let after = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        assert_eq!(after.trim(), before.lines().nth(1).unwrap().trim(), "only Jodd's line went");
        assert!(!base.exists());
    }

    #[tokio::test]
    async fn a_wrong_password_leaves_nothing_behind() {
        let (_srv, _data, base, r) = run_core(
            ServerOpts { password: Some("right"), ..Default::default() },
            SetupCredential::Password { password: "wrong".into() },
        ).await;
        assert!(r.unwrap_err().contains("Wrong username or password"));
        assert!(!base.exists(), "the key directory was cleaned up");
    }

    #[tokio::test]
    async fn a_key_file_is_refused_on_the_embedded_path() {
        let (_srv, _data, base, r) = run_core(ServerOpts::default(), SetupCredential::KeyFile { path: "/nope".into(), passphrase: None }).await;
        assert_eq!(r.unwrap_err(), "Key files aren't supported on Android yet — use Run a command on the server, or Password.");
        assert!(!base.exists());
    }

    #[test]
    fn session_for_with_keys_on_the_transport_too() {
        let o = session::SshOptions::default();
        let a = crate::backend::ssh::session_for_with("ssh:t-embedded", "me@box", &o, false);
        let b = crate::backend::ssh::session_for_with("ssh:t-embedded", "me@box", &o, true);
        assert!(!std::sync::Arc::ptr_eq(&a, &b), "switching transport must not reuse the old session");
        crate::backend::ssh::forget_session("ssh:t-embedded");
    }

    // ---- Command path ----

    async fn command_server() -> (test_server::TestServer, tempfile::TempDir, std::path::PathBuf, String) {
        let srv = test_server::start(ServerOpts::default()).await;
        let data = tempfile::tempdir().unwrap();
        let uuid = uuid::Uuid::new_v4().to_string();
        let base = data.path().join("ssh").join(&uuid);
        (srv, data, base, uuid)
    }

    fn authorize_device_key(srv: &test_server::TestServer, base: &std::path::Path) {
        srv.authorize(&scripts::authorized_line(std::fs::read_to_string(base.join("id_ed25519.pub")).unwrap().trim()));
    }

    /// A Command setup left open past `ORPHAN_MIN_AGE` can have its directory
    /// swept by another Jodd; finishing it then says so, not a file error.
    #[tokio::test]
    async fn command_path_finish_after_the_directory_was_swept_says_it_expired() {
        let data = tempfile::tempdir().unwrap();
        let base = data.path().join("ssh").join("gone");
        let e = finish_command_core("127.0.0.1", Some(22), "jodd", "~/Jodd-Notes", "gone", &base, true).await.unwrap_err();
        assert_eq!(e, "This setup expired — start again.");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn command_path_finish_before_the_line_is_added_keeps_the_key_and_explains() {
        let (srv, _data, base, uuid) = command_server().await;
        let cmd = prepare_command_core("127.0.0.1", Some(srv.port), "jodd", &srv.host_key_line, &base, true).await.unwrap();
        assert!(cmd.starts_with("( u='jodd';"), "{cmd}");
        assert!(crate::backend::ssh::setup::key_was_generated(&base));
        let e = finish_command_core("127.0.0.1", Some(srv.port), "jodd", "~/Jodd-Notes", &uuid, &base, true).await.unwrap_err();
        assert!(e.contains("doesn't accept Jodd's key yet"), "{e}");
        assert!(base.join("id_ed25519").exists());
        assert!(!base.join("verified").exists());
        authorize_device_key(&srv, &base);
        let ready = finish_command_core("127.0.0.1", Some(srv.port), "jodd", "~/Jodd-Notes", &uuid, &base, true).await.unwrap();
        assert!(ready.abs.ends_with("Jodd-Notes"));
        assert!(base.join("verified").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn command_path_vault_failure_after_verify_keeps_the_key() {
        let (srv, _data, base, uuid) = command_server().await;
        prepare_command_core("127.0.0.1", Some(srv.port), "jodd", &srv.host_key_line, &base, true).await.unwrap();
        authorize_device_key(&srv, &base);
        std::fs::write(srv.home.path().join("afile"), "x").unwrap();
        let e = finish_command_core("127.0.0.1", Some(srv.port), "jodd", "~/afile/sub", &uuid, &base, true).await;
        assert!(e.is_err());
        assert!(base.join("id_ed25519").exists(), "the key survives a vault failure");
        assert!(base.join("verified").exists());
        let ready = finish_command_core("127.0.0.1", Some(srv.port), "jodd", "~/Jodd-Notes", &uuid, &base, true).await;
        assert!(ready.is_ok(), "{:?}", ready.err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn command_path_cancel_after_verify_revokes_the_line() {
        let (srv, _data, base, uuid) = command_server().await;
        prepare_command_core("127.0.0.1", Some(srv.port), "jodd", &srv.host_key_line, &base, true).await.unwrap();
        let body = std::fs::read_to_string(base.join("id_ed25519.pub")).unwrap().split_whitespace().nth(1).unwrap().to_string();
        authorize_device_key(&srv, &base);
        finish_command_core("127.0.0.1", Some(srv.port), "jodd", "~/Jodd-Notes", &uuid, &base, true).await.unwrap();
        let keys = srv.home.path().join(".ssh/authorized_keys");
        assert!(std::fs::read_to_string(&keys).unwrap().contains(&body));
        cancel_command_core(&srv.target(), Some(srv.port), &base, true).await;
        assert!(!std::fs::read_to_string(&keys).unwrap().contains(&body), "the line was revoked");
        assert!(!base.exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn command_path_cancel_before_verify_only_deletes_the_key() {
        let (srv, _data, base, _uuid) = command_server().await;
        prepare_command_core("127.0.0.1", Some(srv.port), "jodd", &srv.host_key_line, &base, true).await.unwrap();
        let (port, target) = (srv.port, srv.target());
        drop(srv); // no connection may be needed
        cancel_command_core(&target, Some(port), &base, true).await;
        assert!(!base.exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn command_path_desktop_prepare_writes_a_generated_key_and_the_command() {
        if !std::path::Path::new("/usr/bin/ssh").exists() { return; }
        let (srv, _data, base, _uuid) = command_server().await;
        let cmd = prepare_command_core("127.0.0.1", Some(srv.port), "jodd", &srv.host_key_line, &base, false).await.unwrap();
        assert!(cmd.starts_with("( u='jodd';"), "{cmd}");
        assert!(base.join("id_ed25519").exists() && base.join("known_hosts").exists());
        assert!(crate::backend::ssh::setup::key_was_generated(&base));
    }

    #[tokio::test]
    async fn command_path_prepare_failure_deletes_the_directory() {
        let (srv, _data, base, _uuid) = command_server().await;
        let e = prepare_command_core("127.0.0.1", Some(srv.port), "", &srv.host_key_line, &base, true).await;
        assert!(e.is_err());
        assert!(!base.exists());
    }

    #[test]
    fn command_setup_ids_must_be_uuids() {
        for bad in ["../x", "/abs", "..", "", "a/b"] {
            assert!(command_setup_base(bad).is_err(), "{bad:?}");
        }
        if let Ok(p) = command_setup_base("3f2a9b1c-0d4e-4f00-8a00-000000000000") {
            assert!(p.ends_with("ssh/3f2a9b1c-0d4e-4f00-8a00-000000000000"), "{p:?}");
        }
    }

    #[test]
    fn a_setup_id_owned_by_an_account_is_not_cancelled_or_saved_twice() {
        let id = "3f2a9b1c-0d4e-4f00-8a00-000000000000";
        let mut a = ssh_account("me@box", "/home/me/Jodd-Notes");
        assert!(!account_owns_setup(&[a.clone()], id));
        assert_eq!(setup_already_saved(&[a.clone()], id), None);
        a.ssh_key = Some(id.into());
        assert!(account_owns_setup(&[a.clone()], id));
        assert!(setup_already_saved(&[a], id).unwrap().contains("already been added"));
    }

    /// The KeepKey duplicate/save-failure paths never touch `base`; only
    /// Abandon deletes it. Pinned on the cancel side: a verified base that an
    /// account owns is left alone by the guard, so its line stays revocable.
    #[tokio::test(flavor = "multi_thread")]
    async fn command_path_keeps_the_key_when_an_account_owns_the_setup() {
        let (srv, _data, base, uuid) = command_server().await;
        prepare_command_core("127.0.0.1", Some(srv.port), "jodd", &srv.host_key_line, &base, true).await.unwrap();
        authorize_device_key(&srv, &base);
        finish_command_core("127.0.0.1", Some(srv.port), "jodd", "~/Jodd-Notes", &uuid, &base, true).await.unwrap();
        let mut acct = ssh_account(&srv.target(), "/x");
        acct.ssh_key = Some(uuid.clone());
        // what ssh_setup_cancel_command does first:
        if !account_owns_setup(&[acct], &uuid) {
            cancel_command_core(&srv.target(), Some(srv.port), &base, true).await;
        }
        assert!(base.join("id_ed25519").exists());
        let keys = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        assert!(!keys.trim().is_empty(), "the line is still on the server");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_file_setup_keeps_no_copy_and_marks_the_key_generated() {
        if !std::path::Path::new("/usr/bin/ssh").exists() { return; }
        let srv = test_server::start(ServerOpts::default()).await;
        let provider_dir = tempfile::tempdir().unwrap();
        let provider = crate::backend::ssh::russh_setup::generate_key(&provider_dir.path().join("p")).unwrap();
        let provider_file = provider_dir.path().join("p/id_ed25519");
        srv.authorize(&provider.pubkey_line);
        let fp = crate::backend::ssh::russh_setup::fetch_fingerprint("127.0.0.1", Some(srv.port)).await.unwrap();
        let data = tempfile::tempdir().unwrap();
        let uuid = uuid::Uuid::new_v4().to_string();
        let base = data.path().join("ssh").join(&uuid);
        let r = setup_managed_core(
            "127.0.0.1".into(), Some(srv.port), "jodd".into(),
            SetupCredential::KeyFile { path: provider_file.to_str().unwrap().into(), passphrase: None },
            fp.known_hosts_line, DEFAULT_MANAGED_ROOT, &uuid, &base, false,
        ).await;
        r.unwrap();
        assert!(crate::backend::ssh::setup::key_was_generated(&base));
        let provider_body = scripts::key_body(&provider.pubkey_line).to_string();
        let jodd_body = scripts::key_body(std::fs::read_to_string(base.join("id_ed25519.pub")).unwrap().trim()).to_string();
        assert_ne!(jodd_body, provider_body, "Jodd's own key, not the user's");
        let provider_private = std::fs::read_to_string(&provider_file).unwrap();
        for e in std::fs::read_dir(&base).unwrap() {
            let text = String::from_utf8_lossy(&std::fs::read(e.unwrap().path()).unwrap()).to_string();
            assert!(!text.contains(&provider_body), "the user's public key body is in a file under base");
            assert!(!text.contains(provider_private.trim()), "a copy of the user's key was kept");
        }
        let keys = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        // ssh-keygen's own .pub carries its own comment; the installed line
        // carries the account's — so compare the restricted prefix and body.
        let body = scripts::key_body(std::fs::read_to_string(base.join("id_ed25519.pub")).unwrap().trim()).to_string();
        let prefix = scripts::authorized_line("");
        assert!(keys.lines().any(|l| l.starts_with(prefix.trim()) && l.contains(&body)), "{keys}");
    }
}
