//! AI host setup (spec 2026-10-13-remote-agent-cli §4.6). The password is
//! used once per setup/update and never stored, logged or echoed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::accounts::AgentHost;
use crate::backend::ssh::{
    classify,
    russh_session::{self, Credential, RusshFailure},
    russh_setup, scripts, session, setup,
};
use crate::llm::remote::{
    self,
    exec::{RemoteExec, RusshExec},
    install, wrapper,
};

/// Probe, wrapper upload and authorized_keys edit are a handful of round trips
/// on one connection; a server that accepts the connection and then goes quiet
/// must not leave the setup spinner running forever.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AiHostPreset {
    pub id: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AiHostStatus {
    pub version: u32,
    pub current: bool,
    pub presets: Vec<AiHostPreset>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiHostSetupResult {
    pub host: AgentHost,
    pub status: AiHostStatus,
}

pub(crate) fn parse_status(stdout: &str) -> AiHostStatus {
    let version = stdout
        .lines()
        .find_map(|l| l.strip_prefix("version="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let presets = stdout
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("preset=")?;
            let (id, path) = rest.split_once(" path=")?;
            Some(AiHostPreset {
                id: id.to_string(),
                path: Some(path.trim().to_string()).filter(|p| p.starts_with('/')),
            })
        })
        .collect();
    AiHostStatus { version, current: version == wrapper::WRAPPER_VERSION, presets }
}

/// Over the restricted key: the wrapper's own `status` verb.
pub(crate) async fn status_core(exec: &dyn RemoteExec) -> Result<AiHostStatus, String> {
    let out = exec
        .exec(
            &format!("jodd-ai-run {} status", wrapper::WRAPPER_VERSION),
            b"",
            Duration::from_secs(30),
            CancellationToken::new(),
        )
        .await
        .map_err(|e| e.to_string())?;
    match out.exit {
        Some(0) => Ok(parse_status(&out.stdout)),
        other => Err(format!(
            "The AI host did not answer the status check (exit {other:?}). {}",
            out.stderr.trim()
        )),
    }
}

/// User-facing text for a connect failure. Never includes the password.
fn setup_message(f: &RusshFailure) -> String {
    match f {
        RusshFailure::HostKeyMismatch => {
            "The server's identity does not match the fingerprint you confirmed. Nothing was changed.".into()
        }
        RusshFailure::AuthRejected { password_offered: true, .. } => {
            "The server rejected that username or password.".into()
        }
        RusshFailure::AuthRejected { password_offered: false, .. } => {
            "This server does not accept password sign-in, which setting up an AI host needs once.".into()
        }
        RusshFailure::Other(m) => format!("Could not connect: {m}"),
        other => classify::from_setup_stderr(&russh_session::stderr_for(other)),
    }
}

/// Why an install over the password session failed, and whether the server may
/// have been changed before it did: `touched` is set the moment the install
/// script is sent, so a timeout or a dropped connection after that point still
/// counts.
struct InstallFailure {
    msg: String,
    touched: bool,
}

impl InstallFailure {
    fn untouched(msg: String) -> Self {
        Self { msg, touched: false }
    }
}

/// Over the PASSWORD session: probe flavor + CLI paths, install wrapper + line.
async fn install_over_password(
    target: &str,
    port: Option<u16>,
    known_hosts: &Path,
    password: &str,
    key_id: &str,
    pubkey_line: &str,
) -> Result<(), InstallFailure> {
    // `connect` pins against the file's CONTENTS, not its path.
    let pinned = std::fs::read_to_string(known_hosts)
        .map_err(|e| InstallFailure::untouched(format!("could not read known_hosts: {e}")))?;
    let touched = AtomicBool::new(false);
    let work = async {
        let h = russh_session::connect(target, port, pinned, Credential::Password(password))
            .await
            .map_err(|f| setup_message(&f))?;
        let run = |script: String| {
            let h = &h;
            async move { russh_session::run_on(h, &script).await.map_err(|e| format!("Could not talk to the server: {e}")) }
        };
        let result = async {
            let probe = run(scripts::PROBE.to_string()).await?;
            let flavor = scripts::parse_probe(&probe.stdout)?;
            let paths = install::parse_probe_paths(&run(install::PROBE_PATHS.to_string()).await?.stdout);
            let text = wrapper::generate(&paths)?;
            touched.store(true, Ordering::SeqCst);
            let out = run(install::install_script(flavor, &text, key_id, pubkey_line)).await?;
            if out.exit != 0 || out.stdout.trim() != "installed" {
                return Err(format!("Installing on the server failed: {}", out.stderr.trim()));
            }
            Ok(())
        }
        .await;
        let _ = h.disconnect(russh::Disconnect::ByApplication, "", "").await;
        result
    };
    let result = tokio::time::timeout(INSTALL_TIMEOUT, work)
        .await
        .unwrap_or_else(|_| Err(russh_setup::SERVER_STOPPED_ANSWERING.to_string()));
    result.map_err(|msg| InstallFailure { msg, touched: touched.load(Ordering::SeqCst) })
}

/// What a failed setup tells the user it may have left on the server. The
/// local key is already gone, so the line cannot be used — but it is there.
fn left_behind_note(key_id: &str, label: &str) -> String {
    format!(
        "Jodd may have left an entry ending in jodd-ai-{key_id} in ~/.ssh/authorized_keys on {label} \
         (and a file ~/.jodd/ai-run-v{}); its key no longer exists on this computer, so you can remove that line.",
        wrapper::WRAPPER_VERSION
    )
}

pub(crate) async fn setup_core(
    host: &str,
    port: Option<u16>,
    user: &str,
    password: &str,
    known_hosts_line: &str,
    base: &Path,
    key_dir: &str,
) -> Result<AiHostSetupResult, String> {
    setup_with_verifier(host, port, user, password, known_hosts_line, base, key_dir, |e| Box::new(e)).await
}

/// `setup_core` with the final key-session check replaceable, so the
/// "installed, but the key does not work" path can be driven in a test.
#[allow(clippy::too_many_arguments)]
async fn setup_with_verifier(
    host: &str,
    port: Option<u16>,
    user: &str,
    password: &str,
    known_hosts_line: &str,
    base: &Path,
    key_dir: &str,
    verifier: impl FnOnce(RusshExec) -> Box<dyn RemoteExec>,
) -> Result<AiHostSetupResult, String> {
    session::validate_target(host)?;
    session::validate_target(user)?;
    let target = format!("{user}@{host}");
    let key_id = remote::key_id_for(key_dir);
    if key_id.len() != 8 {
        return Err("internal: bad key id".into());
    }
    let agent_host = AgentHost { target: target.clone(), port, key_dir: key_dir.to_string() };
    let label = remote::host_label(&agent_host);
    let fail = |e: String| {
        let _ = std::fs::remove_dir_all(base);
        e
    };
    let made = russh_setup::generate_key(base)?;
    setup::write_known_hosts(base, known_hosts_line).map_err(fail)?;
    // Our own comment, so `forget` and re-install can find exactly this line.
    let pubkey_line = format!("{} jodd-ai-{key_id}", scripts::key_body(&made.pubkey_line));
    install_over_password(&target, port, &base.join("known_hosts"), password, &key_id, &pubkey_line)
        .await
        .map_err(|f| if f.touched { fail(format!("{} {}", f.msg, left_behind_note(&key_id, &label))) } else { fail(f.msg) })?;
    let exec = RusshExec {
        target,
        port,
        key: base.join("id_ed25519"),
        known_hosts: base.join("known_hosts"),
        label: label.clone(),
    };
    // Reconnect WITH THE KEY: proves the restricted line is live.
    let status = status_core(verifier(exec).as_ref())
        .await
        .map_err(|e| fail(format!("{e} {}", left_behind_note(&key_id, &label))))?;
    Ok(AiHostSetupResult { host: agent_host, status })
}

/// Reinstall the wrapper and this key's line with the password. The working
/// key stays on disk whether or not this succeeds.
pub(crate) async fn update_core(h: &AgentHost, base: &Path, password: &str) -> Result<AiHostStatus, String> {
    let key_id = remote::key_id_for(&h.key_dir);
    let pub_text = std::fs::read_to_string(base.join("id_ed25519.pub"))
        .map_err(|e| format!("Jodd's key is missing: {e}"))?;
    let pubkey_line = format!("{} jodd-ai-{key_id}", scripts::key_body(&pub_text));
    install_over_password(&h.target, h.port, &base.join("known_hosts"), password, &key_id, &pubkey_line)
        .await
        .map_err(|f| f.msg)?;
    let exec = RusshExec {
        target: h.target.clone(),
        port: h.port,
        key: base.join("id_ed25519"),
        known_hosts: base.join("known_hosts"),
        label: remote::host_label(h),
    };
    status_core(&exec).await
}

/// Best effort on the server (it may be gone), certain locally.
pub(crate) async fn forget_core(h: &AgentHost, base: &Path, exec: &dyn RemoteExec) {
    let r = exec
        .exec(
            &format!("jodd-ai-run {} forget", wrapper::WRAPPER_VERSION),
            b"",
            Duration::from_secs(15),
            CancellationToken::new(),
        )
        .await;
    crate::log!("ai_host_forget {}: remote {:?}", remote::host_label(h), r.map(|o| o.exit));
    let _ = std::fs::remove_dir_all(base);
}

/// The only way a command turns an IPC `AgentHost` into a directory: through
/// `host_dir`, which refuses a `key_dir` that is not a uuid.
fn base_for(h: &AgentHost) -> Result<PathBuf, String> {
    remote::host_dir(h).ok_or_else(|| "this AI host has no usable key directory".to_string())
}

#[tauri::command]
pub(crate) async fn ai_host_setup(
    host: String,
    port: Option<u16>,
    user: String,
    password: String,
    known_hosts_line: String,
) -> Result<AiHostSetupResult, String> {
    let key_dir = uuid::Uuid::new_v4().to_string();
    let probe = AgentHost { target: format!("{user}@{host}"), port, key_dir: key_dir.clone() };
    let base = base_for(&probe).map_err(|_| "no app data directory available".to_string())?;
    setup_core(&host, port, &user, &password, &known_hosts_line, &base, &key_dir).await
}

#[tauri::command]
pub(crate) async fn ai_host_status(host: AgentHost) -> Result<AiHostStatus, String> {
    let exec = RusshExec::for_host(&host).map_err(|e| e.to_string())?;
    status_core(&exec).await
}

#[tauri::command]
pub(crate) async fn ai_host_update(host: AgentHost, password: String) -> Result<AiHostStatus, String> {
    update_core(&host, &base_for(&host)?, &password).await
}

#[tauri::command]
pub(crate) async fn ai_host_forget(host: AgentHost) -> Result<(), String> {
    let base = base_for(&host)?;
    let exec = RusshExec::for_host(&host).map_err(|e| e.to_string())?;
    forget_core(&host, &base, &exec).await;
    Ok(())
}

/// These run a real ssh server on loopback (`test_server`): Unix only.
#[cfg(all(test, unix))]
mod live_tests {
    use super::*;
    use crate::backend::ssh::test_server::{start, ServerOpts};

    const KEY_DIR: &str = "0123abcd-0000-4000-8000-000000000000";

    async fn server() -> crate::backend::ssh::test_server::TestServer {
        start(ServerOpts { password: Some("hunter2"), ..Default::default() }).await
    }

    fn exec_for(r: &AiHostSetupResult, dir: &std::path::Path) -> crate::llm::remote::exec::RusshExec {
        crate::llm::remote::exec::RusshExec { target: r.host.target.clone(), port: r.host.port,
            key: dir.join("id_ed25519"), known_hosts: dir.join("known_hosts"), label: "t".into() }
    }

    #[tokio::test]
    async fn setup_installs_a_restricted_key_and_reports_status() {
        let srv = server().await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let r = setup_core("127.0.0.1", Some(srv.port), "jodd", "hunter2", &srv.host_key_line, &dir, KEY_DIR).await.unwrap();
        assert_eq!(r.host.target, "jodd@127.0.0.1");
        assert_eq!(r.status.version, crate::llm::remote::wrapper::WRAPPER_VERSION);
        assert!(r.status.current);
        assert_eq!(r.status.presets.len(), 2);
        let keys = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        assert!(keys.contains("restrict,command=") && keys.contains("jodd-ai-0123abcd"), "{keys}");
        assert!(!keys.contains("hunter2"));
        assert!(dir.join("id_ed25519").exists() && dir.join("known_hosts").exists());
    }

    #[tokio::test]
    async fn a_wrong_password_leaves_nothing_behind() {
        let srv = server().await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let e = setup_core("127.0.0.1", Some(srv.port), "jodd", "wrong", &srv.host_key_line, &dir, KEY_DIR).await.unwrap_err();
        assert!(!e.contains("wrong"), "never echo the password: {e}");
        assert!(!dir.exists(), "the key dir is removed on failure");
    }

    #[tokio::test]
    async fn setup_refuses_a_mismatched_host_key() {
        let srv = server().await;
        let other = server().await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        // The user confirmed `other`'s fingerprint, but we are talking to `srv`.
        let e = setup_core("127.0.0.1", Some(srv.port), "jodd", "hunter2", &other.host_key_line, &dir, KEY_DIR).await.unwrap_err();
        assert!(e.to_lowercase().contains("identity") || e.to_lowercase().contains("host key"), "{e}");
        assert!(!dir.exists());
        let keys = srv.home.path().join(".ssh/authorized_keys");
        assert!(!keys.exists() || !std::fs::read_to_string(keys).unwrap().contains("jodd-ai-"), "nothing installed");
    }

    #[tokio::test]
    async fn forget_removes_the_line_and_the_local_key() {
        let srv = server().await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let r = setup_core("127.0.0.1", Some(srv.port), "jodd", "hunter2", &srv.host_key_line, &dir, KEY_DIR).await.unwrap();
        let exec = exec_for(&r, &dir);
        forget_core(&r.host, &dir, &exec).await;
        let keys = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        assert!(!keys.contains("jodd-ai-0123abcd"), "{keys}");
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn forget_still_removes_the_local_key_when_the_server_is_gone() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("k");
        std::fs::create_dir_all(&dir).unwrap();
        let h = AgentHost { target: "me@127.0.0.1".into(), port: Some(1), key_dir: "k".into() };
        let exec = crate::llm::remote::exec::RusshExec { target: h.target.clone(), port: h.port,
            key: dir.join("id_ed25519"), known_hosts: dir.join("known_hosts"), label: "t".into() };
        forget_core(&h, &dir, &exec).await;
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn update_reinstalls_with_the_password_and_keeps_the_same_key() {
        let srv = server().await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let r = setup_core("127.0.0.1", Some(srv.port), "jodd", "hunter2", &srv.host_key_line, &dir, KEY_DIR).await.unwrap();
        let before = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        let s = update_core(&r.host, &dir, "hunter2").await.unwrap();
        assert!(s.current);
        let after = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        assert_eq!(after.matches("jodd-ai-0123abcd").count(), 1, "{after}");
        assert_eq!(before.lines().count(), after.lines().count());
        let e = update_core(&r.host, &dir, "wrong").await.unwrap_err();
        assert!(!e.contains("wrong"), "{e}");
        assert!(dir.join("id_ed25519").exists(), "a failed update must not delete the working key");
    }

    #[tokio::test]
    async fn status_reports_a_server_that_does_not_answer() {
        let base = tempfile::tempdir().unwrap();
        let exec = crate::llm::remote::exec::RusshExec { target: "me@127.0.0.1".into(), port: Some(1),
            key: base.path().join("id_ed25519"), known_hosts: base.path().join("known_hosts"), label: "t".into() };
        assert!(status_core(&exec).await.is_err());
    }

    #[derive(Debug)]
    struct BrokenKey;

    #[async_trait::async_trait]
    impl RemoteExec for BrokenKey {
        async fn exec(&self, _: &str, _: &[u8], _: Duration, _: CancellationToken)
            -> Result<crate::llm::remote::exec::RemoteOutput, crate::llm::provider::ExtractError> {
            Err(crate::llm::provider::ExtractError::Transport("the key was refused".into()))
        }
    }

    #[tokio::test]
    async fn installed_but_unverified_removes_the_key_and_names_what_remains() {
        let srv = server().await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let e = setup_with_verifier("127.0.0.1", Some(srv.port), "jodd", "hunter2", &srv.host_key_line, &dir, KEY_DIR,
            |_| Box::new(BrokenKey)).await.unwrap_err();
        assert!(e.contains("jodd-ai-0123abcd") && e.contains("authorized_keys"), "{e}");
        assert!(!e.contains("hunter2"), "{e}");
        assert!(!dir.exists(), "the local key is removed");
        let keys = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        assert!(keys.contains("jodd-ai-0123abcd"), "the note is honest: the line really is still there: {keys}");
    }

    #[tokio::test]
    async fn a_failure_before_anything_was_installed_does_not_claim_a_leftover() {
        let srv = server().await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let e = setup_core("127.0.0.1", Some(srv.port), "jodd", "wrong", &srv.host_key_line, &dir, KEY_DIR).await.unwrap_err();
        assert!(!e.contains("jodd-ai-"), "{e}");
    }

    #[tokio::test]
    async fn forget_and_update_refuse_a_key_dir_that_is_not_a_uuid() {
        // Refusal happens in `base_for`, before `forget_core` could remove anything
        // or any connection is attempted.
        for bad in ["../..", "..", "/etc", "a/b", ""] {
            let h = AgentHost { target: "me@127.0.0.1".into(), port: Some(1), key_dir: bad.into() };
            assert!(ai_host_forget(h.clone()).await.is_err(), "forget {bad:?}");
            assert!(ai_host_update(h, "pw".into()).await.is_err(), "update {bad:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parsing() {
        let s = parse_status("version=1\npreset=claude path=-\npreset=codex path=/usr/bin/codex\n");
        assert_eq!(s.version, 1);
        assert_eq!(s.presets, vec![
            AiHostPreset { id: "claude".into(), path: None },
            AiHostPreset { id: "codex".into(), path: Some("/usr/bin/codex".into()) },
        ]);
    }
}
