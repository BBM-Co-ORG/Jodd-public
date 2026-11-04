//! #103's setup verbs, in-process, for Android (no `ssh-keygen`, no
//! `ssh-keyscan`, no askpass). They write the SAME files as `setup.rs`, so
//! everything that reads those files — `managed_options`,
//! `retire_managed_key`'s policy, the markers — is shared.

use std::path::Path;

use rand::RngCore;
use russh::keys::ssh_key::{private::Ed25519Keypair, LineEnding};
use russh::keys::{HashAlg, PrivateKey};

use super::russh_session::{self, Credential, RusshFailure};
use super::session::{ExecOutput, SshOptions, EXEC_TIMEOUT};
use super::{classify, scripts, setup};

pub(crate) const KEY_ONLY_ON_ANDROID: &str =
    "This server only accepts key files. Jodd for Android can't import key files yet.";

/// The setup-screen message for an embedded-client failure. A publickey-only
/// server gets the Android wording: #103's "Choose the Key file option" names
/// a radio button Android does not show.
fn setup_message(f: &RusshFailure) -> String {
    match f {
        RusshFailure::AuthRejected { password_offered: false, .. } => KEY_ONLY_ON_ANDROID.to_string(),
        RusshFailure::Other(m) => format!("Could not connect: {m}"),
        other => classify::from_setup_stderr(&russh_session::stderr_for(other)),
    }
}

pub(crate) const SERVER_STOPPED_ANSWERING: &str =
    "The server stopped answering. Check your connection and try again.";

/// Every network verb below runs under this deadline (`session::EXEC_TIMEOUT`
/// in production): a server that accepts a request and then never answers
/// must not leave the setup spinner, or a revoke's blocking thread, waiting
/// forever.
async fn within<T>(limit: std::time::Duration, verb: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    tokio::time::timeout(limit, verb).await.unwrap_or_else(|_| Err(SERVER_STOPPED_ANSWERING.to_string()))
}

pub(crate) fn generate_key(base_dir: &Path) -> Result<setup::ManagedSetup, String> {
    let result = (|| {
        std::fs::create_dir_all(base_dir).map_err(|e| format!("could not create {}: {e}", base_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(base_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("could not secure {}: {e}", base_dir.display()))?;
        }
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        let mut key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
        key.set_comment("jodd");
        let key_path = base_dir.join("id_ed25519");
        let pem = key.to_openssh(LineEnding::LF).map_err(|e| format!("could not encode the key: {e}"))?;
        std::fs::write(&key_path, pem.as_bytes()).map_err(|e| format!("could not write the key: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("could not secure key: {e}"))?;
        }
        let public = key.public_key().to_openssh().map_err(|e| format!("could not encode the public key: {e}"))?;
        let pubkey_line = setup::pubkey_line_for(&public, base_dir);
        std::fs::write(base_dir.join("id_ed25519.pub"), format!("{pubkey_line}\n"))
            .map_err(|e| format!("could not write the public key: {e}"))?;
        Ok(setup::ManagedSetup { dir: base_dir.to_path_buf(), pubkey_line })
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(base_dir);
    }
    result
}

pub(crate) async fn fetch_fingerprint(host: &str, port: Option<u16>) -> Result<setup::Fingerprint, String> {
    within(EXEC_TIMEOUT, fetch_fingerprint_now(host, port)).await
}

async fn fetch_fingerprint_now(host: &str, port: Option<u16>) -> Result<setup::Fingerprint, String> {
    let key = russh_session::capture_host_key(host, port).await.map_err(|f| setup_message(&f))?;
    Ok(setup::Fingerprint {
        algorithm: russh_session::algorithm_label(&key),
        fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
        known_hosts_line: russh_session::known_hosts_line(host, port, &key),
    })
}

pub(crate) async fn install_key_over_password(
    target: &str,
    port: Option<u16>,
    known_hosts_path: &Path,
    password: &str,
    pubkey_line: &str,
) -> Result<(), String> {
    within(EXEC_TIMEOUT, install_key_over_password_now(target, port, known_hosts_path, password, pubkey_line)).await
}

async fn install_key_over_password_now(
    target: &str,
    port: Option<u16>,
    known_hosts_path: &Path,
    password: &str,
    pubkey_line: &str,
) -> Result<(), String> {
    let pinned = std::fs::read_to_string(known_hosts_path).map_err(|e| format!("could not read known_hosts: {e}"))?;
    let h = russh_session::connect(target, port, pinned, Credential::Password(password))
        .await
        .map_err(|f| setup_message(&f))?;
    let out = russh_session::run_on(&h, &scripts::install_authorized_key(pubkey_line))
        .await
        .map_err(|e| format!("Could not connect: {e}"))?;
    let _ = h.disconnect(russh::Disconnect::ByApplication, "", "").await;
    if out.exit != 0 {
        return Err(format!("Could not install Jodd's key on the server: {}", out.stderr.trim()));
    }
    Ok(())
}

/// Bounded by `run_script_once`'s own deadline.
pub(crate) async fn verify_key_login(target: &str, opts: &SshOptions) -> Result<(), String> {
    run_script_once(target, opts, "exit 0\n").await.map(|_| ()).map_err(|e| {
        if e.contains("Permission denied") {
            setup::verify_failure_message(&e, true)
        } else {
            e
        }
    })
}

/// A one-shot connection with the account's key: key-only verification,
/// and revoke-on-removal (the registered session is already forgotten then).
pub(crate) async fn run_script_once(target: &str, opts: &SshOptions, script: &str) -> Result<ExecOutput, String> {
    within(EXEC_TIMEOUT, run_script_once_now(target, opts, script)).await
}

async fn run_script_once_now(target: &str, opts: &SshOptions, script: &str) -> Result<ExecOutput, String> {
    let key = opts.key.as_deref().ok_or("this account has no key")?;
    let kh = opts.known_hosts.as_deref().ok_or("this account has no known_hosts")?;
    let pinned = std::fs::read_to_string(kh).map_err(|e| format!("could not read known_hosts: {e}"))?;
    let h = russh_session::connect(target, opts.port, pinned, Credential::Key(key))
        .await
        .map_err(|f| match f {
            RusshFailure::AuthRejected { .. } => russh_session::stderr_for(&f),
            other => setup_message(&other),
        })?;
    let out = russh_session::run_on(&h, script).await.map_err(|e| format!("Could not connect: {e}"))?;
    let _ = h.disconnect(russh::Disconnect::ByApplication, "", "").await;
    Ok(out)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::backend::ssh::session::SshOptions;
    use crate::backend::ssh::test_server::{self, ServerOpts};

    fn opts_for(dir: &Path, port: u16) -> SshOptions {
        SshOptions { port: Some(port), key: Some(dir.join("id_ed25519")), known_hosts: Some(dir.join("known_hosts")) }
    }

    #[test]
    fn a_generated_key_is_one_openssh_itself_reads() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("abcdef12-3456-7890-abcd-ef1234567890");
        let setup = generate_key(&base).unwrap();
        assert!(setup.pubkey_line.starts_with("ssh-ed25519 ") && setup.pubkey_line.ends_with(" jodd-abcdef12"), "{}", setup.pubkey_line);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(base.join("id_ed25519")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&base).unwrap().permissions().mode() & 0o777, 0o700);
        // The real OpenSSH reader, not ssh-key reading its own output.
        if let Ok(out) = std::process::Command::new("ssh-keygen").args(["-y", "-P", "", "-f"]).arg(base.join("id_ed25519")).output() {
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let derived = String::from_utf8_lossy(&out.stdout);
            assert_eq!(scripts::key_body(&derived), scripts::key_body(&setup.pubkey_line));
        }
    }

    /// Review item 4: every setup verb has a deadline, so a server that stops
    /// answering cannot leave the setup spinner (or a revoke's blocking
    /// thread) waiting forever.
    #[tokio::test]
    async fn a_verb_that_never_finishes_is_cut_off_with_a_plain_message() {
        let started = std::time::Instant::now();
        let r: Result<(), String> = within(std::time::Duration::from_millis(50), std::future::pending()).await;
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(r.unwrap_err(), SERVER_STOPPED_ANSWERING);
        let ok: Result<u8, String> = within(std::time::Duration::from_secs(5), async { Ok(7) }).await;
        assert_eq!(ok.unwrap(), 7);
    }

    /// Key verification against a server that refuses `exec` reports it
    /// promptly instead of hanging on the refused channel.
    #[tokio::test]
    async fn verifying_against_a_server_that_refuses_exec_fails_promptly() {
        let srv = test_server::start(ServerOpts { refuse_exec: true, ..Default::default() }).await;
        let d = tempfile::tempdir().unwrap();
        let setup = generate_key(d.path()).unwrap();
        srv.authorize(&setup.pubkey_line);
        std::fs::write(d.path().join("known_hosts"), format!("{}\n", srv.host_key_line)).unwrap();
        let started = std::time::Instant::now();
        let e = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            verify_key_login(&srv.target(), &opts_for(d.path(), srv.port)),
        )
        .await
        .expect("verification must not hang")
        .unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(e.contains("refused to run a shell"), "{e}");
    }

    #[tokio::test]
    async fn the_fingerprint_screen_gets_desktops_shape() {
        let srv = test_server::start(ServerOpts { password: Some("pw"), ..Default::default() }).await;
        let fp = fetch_fingerprint("127.0.0.1", Some(srv.port)).await.unwrap();
        assert_eq!(fp.algorithm, "ED25519");
        assert!(fp.fingerprint.starts_with("SHA256:"), "{}", fp.fingerprint);
        assert_eq!(fp.known_hosts_line, srv.host_key_line);
    }

    async fn install_with(opts: ServerOpts, password: &str) -> (test_server::TestServer, tempfile::TempDir, Result<(), String>) {
        let srv = test_server::start(opts).await;
        let d = tempfile::tempdir().unwrap();
        let setup = generate_key(d.path()).unwrap();
        std::fs::write(d.path().join("known_hosts"), format!("{}\n", srv.host_key_line)).unwrap();
        let r = install_key_over_password(&srv.target(), Some(srv.port), &d.path().join("known_hosts"), password, &setup.pubkey_line).await;
        (srv, d, r)
    }

    #[tokio::test]
    async fn a_password_installs_the_key_and_the_key_alone_then_signs_in() {
        let (srv, d, r) = install_with(ServerOpts { password: Some("hunter2"), ..Default::default() }, "hunter2").await;
        r.unwrap();
        let listed = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        assert!(listed.contains(" jodd-"), "{listed}");
        verify_key_login(&srv.target(), &opts_for(d.path(), srv.port)).await.unwrap();
    }

    #[tokio::test]
    async fn a_keyboard_interactive_only_server_takes_the_password() {
        let (srv, _d, r) = install_with(
            ServerOpts { password: Some("hunter2"), password_auth: false, keyboard_interactive: true, ..Default::default() },
            "hunter2",
        ).await;
        r.unwrap();
        let listed = std::fs::read_to_string(srv.home.path().join(".ssh/authorized_keys")).unwrap();
        assert!(listed.contains(" jodd-"), "{listed}");
    }

    /// Review Focus #3: a 2FA prompt must never receive the password.
    #[tokio::test]
    async fn a_non_password_prompt_aborts_without_sending_the_password() {
        let (srv, _d, r) = install_with(
            ServerOpts {
                password: Some("hunter2"),
                password_auth: false,
                keyboard_interactive: true,
                kbd_prompt: "Verification code: ",
                ..Default::default()
            },
            "hunter2",
        ).await;
        let e = r.unwrap_err();
        assert!(!e.contains("hunter2"), "the password leaked into an error: {e}");
        assert!(!srv.home.path().join(".ssh/authorized_keys").exists(), "nothing was installed");
    }

    #[tokio::test]
    async fn a_wrong_password_says_so() {
        let (_srv, _d, r) = install_with(ServerOpts { password: Some("right"), ..Default::default() }, "wrong").await;
        assert!(r.unwrap_err().contains("Wrong username or password"));
    }

    #[tokio::test]
    async fn a_key_only_server_gets_the_android_message() {
        let (_srv, _d, r) = install_with(ServerOpts::default(), "anything").await;
        assert_eq!(r.unwrap_err(), KEY_ONLY_ON_ANDROID);
    }

    #[tokio::test]
    async fn a_swapped_host_key_is_refused_before_the_password_is_sent() {
        let srv = test_server::start(ServerOpts { password: Some("hunter2"), ..Default::default() }).await;
        let d = tempfile::tempdir().unwrap();
        let setup = generate_key(d.path()).unwrap();
        let other = test_server::start(ServerOpts::default()).await;
        std::fs::write(d.path().join("known_hosts"), format!("{}\n", other.host_key_line)).unwrap();
        let e = install_key_over_password(&srv.target(), Some(srv.port), &d.path().join("known_hosts"), "hunter2", &setup.pubkey_line).await.unwrap_err();
        assert!(e.contains("identity is different"), "{e}");
    }
}
