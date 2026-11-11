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
/// ssh session; `KeyFile` imports a key the user already has (and the server
/// already trusts) and only verifies it. The password/passphrase never
/// touches disk or argv — see `askpass` and `setup::install_key_over_password`.
#[derive(serde::Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum SetupCredential {
    Password { password: String },
    KeyFile { path: String, passphrase: Option<String> },
}

pub(crate) const KEY_FILES_NOT_ON_ANDROID: &str = "Key files aren't supported on Android yet — use Password.";

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

    let (source, password_for_install): (setup::PublicKeySource, Option<String>) = match credential {
        SetupCredential::Password { password } => (setup::PublicKeySource::Generate, Some(password)),
        SetupCredential::KeyFile { .. } if embedded => return Err(KEY_FILES_NOT_ON_ANDROID.to_string()),
        SetupCredential::KeyFile { path, passphrase } => {
            (setup::PublicKeySource::ImportFile { path: path.into(), passphrase }, None)
        }
    };
    // C5: the ONLY point that knows which source was chosen.
    let key_was_generated = matches!(source, setup::PublicKeySource::Generate);

    let prepared = if embedded {
        russh_setup::generate_key(base)?
    } else {
        let b = base.to_path_buf();
        tokio::task::spawn_blocking(move || setup::prepare_key(&b, source))
            .await
            .map_err(|e| format!("setup task failed: {e}"))??
    };
    setup::write_key_source_marker(base, key_was_generated).map_err(fail)?;
    setup::write_known_hosts(base, &known_hosts_line).map_err(fail)?;

    let key_just_installed = password_for_install.is_some();
    if let Some(password) = password_for_install {
        let known_hosts_path = base.join("known_hosts");
        let installed = if embedded {
            russh_setup::install_key_over_password(&target, port, &known_hosts_path, &password, &prepared.pubkey_line).await
        } else {
            let (t, line) = (target.clone(), prepared.pubkey_line.clone());
            tokio::task::spawn_blocking(move || setup::install_key_over_password(&t, port, &known_hosts_path, &password, &line))
                .await
                .map_err(|e| format!("setup task failed: {e}"))?
        };
        installed.map_err(fail)?;
    }

    let opts = session::SshOptions { port, key: Some(base.join("id_ed25519")), known_hosts: Some(base.join("known_hosts")) };
    let verified = if embedded {
        russh_setup::verify_key_login(&target, &opts).await
    } else {
        let (t, o) = (target.clone(), opts.clone());
        tokio::task::spawn_blocking(move || setup::verify_key_login(&t, &o, key_just_installed))
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

    let display_name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
        .unwrap_or_else(|| default_name(&target, &abs));
    let id = accounts::account_id_for(accounts::BackendKind::Ssh, &uuid);
    let account = accounts::Account {
        id: id.clone(), email: display_name, added_at: chrono::Utc::now().to_rfc3339(),
        notes_label: None, meta_label: None, llm: accounts::LlmConfig::default(),
        backend_kind: accounts::BackendKind::Ssh, root_dir: Some(abs.clone()),
        ssh_target: Some(target.clone()), ssh_port: port, ssh_key: Some(uuid.clone()),
        icloud_session_established: false, blocked_reason: None, sync_cursor: None,
        icloud_replica_id: None, status: accounts::AccountStatus::Active, pending_removal: false,
    };
    // Checked under the same lock as the push, as `add_ssh_account` does, so
    // two setups racing to the same vault cannot both land.
    let duplicate = {
        let mut list = state.accounts.lock().unwrap();
        if let Some(e) = duplicate_vault(&list, &target, &abs) {
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
                let _ = std::fs::remove_dir_all(&base);
                crate::backend::ssh::forget_session(&format!("ssh:{uuid}"));
                return Err(e);
            }
            None
        }
    };
    if let Some(e) = duplicate {
        abandon_managed_setup(&uuid, &target, &opts, &base, crate::backend::ssh::EMBEDDED).await;
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
        assert_eq!(r.unwrap_err(), KEY_FILES_NOT_ON_ANDROID);
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
}
