//! SSH failures → `TransportError`. Pure functions over what ssh and the
//! scripts printed, so every OpenSSH wording is a fixture, not a live server.

use crate::backend::TransportError;

use super::session::{ExecOutput, SshError};

/// The file changed since the version the caller expected.
pub const EXIT_CONFLICT: i32 = 3;
pub const EXIT_NOT_FOUND: i32 = 4;
/// `cd <root>` failed: the vault directory is gone or unmounted.
pub const EXIT_NO_ROOT: i32 = 97;

/// What the user has to do about an authentication failure, or `None` if
/// `stderr` does not describe one. Jodd never accepts a host key itself:
/// trusting a new server is the user's decision.
pub fn auth_hint(stderr: &str, target: &str) -> Option<String> {
    if stderr.contains("Host key verification failed")
        || stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
    {
        return Some(format!(
            "{target}'s host key is not trusted yet. Run `ssh {target}` once in a terminal and accept it, then retry."
        ));
    }
    if stderr.contains("Permission denied") {
        return Some(format!(
            "{target} refused the key. Load it with `ssh-add` — Jodd runs ssh non-interactively and cannot ask for a passphrase."
        ));
    }
    None
}

/// A Managed account's key and trust live in Jodd's own directory, so
/// `auth_hint`'s remedies (`ssh-add`, a terminal) do not apply — and on a
/// phone cannot. The honest remedy is re-running setup.
pub fn managed_auth_hint(stderr: &str) -> Option<String> {
    if stderr.contains("Host key verification failed") || stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        return Some(
            "This server's identity is different from when you trusted it. That happens after a server \
             reinstall, and also if someone is intercepting the connection. Remove this account and add \
             it again once you are sure it is your server."
                .to_string(),
        );
    }
    if stderr.contains("Permission denied") {
        return Some(
            "The server no longer accepts Jodd's key for this account. Remove the account and add it \
             again to set up a new key."
                .to_string(),
        );
    }
    None
}

/// Always `Transient` (spec amendment A9): a session failure is about the
/// account, and `Permanent` would block individual notes (gotcha #14).
/// `managed` selects `managed_auth_hint` over `auth_hint` — a Managed
/// account's key and trust live in Jodd's own directory, so `ssh-add` and
/// "run ssh in a terminal" are wrong (and impossible on a phone).
pub fn from_session_error(e: &SshError, target: &str, managed: bool) -> TransportError {
    let message = match e {
        SshError::Disconnected { stderr } => {
            let hint = if managed { managed_auth_hint(stderr) } else { auth_hint(stderr, target) };
            hint.unwrap_or_else(|| {
                let last = stderr
                    .lines()
                    .rev()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("connection closed");
                format!("ssh: {last}")
            })
        }
        other => other.to_string(),
    };
    TransportError::Transient { source: anyhow::anyhow!(message) }
}

/// True if ssh's "Permission denied (...)" line lists `password` among the
/// methods it offered — the two `Permission denied` cases in the setup
/// error table are told apart by this alone.
pub fn password_auth_was_offered(stderr: &str) -> bool {
    stderr
        .lines()
        .find(|l| l.contains("Permission denied ("))
        .and_then(|l| l.split('(').nth(1))
        .map(|methods| methods.to_lowercase().contains("password"))
        .unwrap_or(false)
}

/// Plain-language errors for Managed SSH setup (spec §4). Pure function over
/// ssh's stderr — every case here is a fixture, never a live server.
pub fn from_setup_stderr(stderr: &str) -> String {
    if stderr.contains("Host key verification failed")
        || stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
    {
        return "This server's identity is different from when you trusted it. \
            That happens after a server reinstall, and also if someone is \
            intercepting the connection.".to_string();
    }
    if stderr.contains("Permission denied") {
        return if password_auth_was_offered(stderr) {
            "Wrong username or password, or this host does not allow password login.".to_string()
        } else {
            "This server only accepts key files. Choose the Key file option.".to_string()
        };
    }
    if stderr.contains("Could not resolve")
        || stderr.contains("Connection refused")
        || stderr.contains("Connection timed out")
        || stderr.contains("Operation timed out")
    {
        return "Can't reach the server. Check the address, and that the server is running.".to_string();
    }
    let last = stderr.lines().rev().map(str::trim).find(|l| !l.is_empty());
    match last {
        Some(l) => format!("Could not connect: {l}"),
        None => "Could not connect to the server.".to_string(),
    }
}

pub fn from_exit(out: &ExecOutput) -> Result<(), TransportError> {
    match out.exit {
        0 => Ok(()),
        EXIT_CONFLICT => Err(TransportError::Conflict { remote_etag: None }),
        EXIT_NOT_FOUND => Err(TransportError::NotFound),
        EXIT_NO_ROOT => Err(TransportError::Transient {
            source: anyhow::anyhow!("the vault directory is missing on the server"),
        }),
        n => Err(TransportError::Permanent {
            source: anyhow::anyhow!("remote script exited {n}: {}", out.stderr.trim()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ssh::session::{ExecOutput, SshError};

    fn out(exit: i32, stderr: &str) -> ExecOutput {
        ExecOutput { stdout: String::new(), stderr: stderr.into(), exit }
    }

    /// Real OpenSSH wording: macOS/Linux, then Win32-OpenSSH (CRLF).
    #[test]
    fn a_refused_key_names_ssh_add() {
        for s in [
            "me@box: Permission denied (publickey).\n",
            "me@box: Permission denied (publickey,password,keyboard-interactive).\r\n",
        ] {
            assert!(auth_hint(s, "me@box").unwrap().contains("ssh-add"), "{s}");
        }
    }

    #[test]
    fn an_untrusted_host_key_sends_the_user_to_a_terminal() {
        for s in [
            "Host key verification failed.\r\n",
            "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n",
        ] {
            assert!(auth_hint(s, "me@box").unwrap().contains("ssh me@box"), "{s}");
        }
    }

    #[test]
    fn network_failures_are_not_auth() {
        for s in [
            "ssh: Could not resolve hostname nosuch: nodename nor servname provided, or not known\r\n",
            "ssh: connect to host 10.0.0.1 port 22: Connection refused\n",
            "ssh: connect to host box port 22: Operation timed out\n",
        ] {
            assert_eq!(auth_hint(s, "me@box"), None, "{s}");
        }
    }

    /// Spec amendment A9: every session failure is Transient and carries its
    /// explanation in the message, never the bare word "auth".
    #[test]
    fn session_failures_are_transient_and_keep_the_hint() {
        let e = SshError::Disconnected { stderr: "me@box: Permission denied (publickey).\n".into() };
        let t = from_session_error(&e, "me@box", false);
        assert!(matches!(t, TransportError::Transient { .. }));
        assert!(t.to_string().contains("ssh-add"), "{t}");
        let t = from_session_error(&SshError::Disconnected { stderr: "ssh: connect to host box port 22: Connection refused\n".into() }, "me@box", false);
        assert!(t.to_string().contains("Connection refused"), "{t}");
        assert!(matches!(from_session_error(&SshError::Timeout, "b", false), TransportError::Transient { .. }));
    }

    #[test]
    fn a_managed_account_never_hears_about_ssh_add_or_a_terminal() {
        for stderr in [
            "jodd@box: Permission denied (publickey).\n",
            "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\nHost key verification failed.\n",
        ] {
            let t = from_session_error(&SshError::Disconnected { stderr: stderr.into() }, "jodd@box", true);
            let msg = t.to_string();
            assert!(!msg.contains("ssh-add") && !msg.contains("terminal"), "{msg}");
            assert!(msg.contains("Remove"), "names the remedy: {msg}");
            assert!(matches!(t, TransportError::Transient { .. }), "A9");
        }
    }

    #[test]
    fn an_advanced_account_keeps_its_hints() {
        let t = from_session_error(&SshError::Disconnected { stderr: "Permission denied (publickey).".into() }, "me@box", false);
        assert!(t.to_string().contains("ssh-add"), "{t}");
    }

    #[test]
    fn script_exit_codes_map_to_transport_errors() {
        assert!(from_exit(&out(0, "")).is_ok());
        assert!(matches!(from_exit(&out(3, "")), Err(TransportError::Conflict { .. })));
        assert!(matches!(from_exit(&out(4, "")), Err(TransportError::NotFound)));
        assert!(matches!(from_exit(&out(97, "")), Err(TransportError::Transient { .. })));
        let e = from_exit(&out(1, "mv: disk full\n")).unwrap_err();
        assert!(matches!(e, TransportError::Permanent { .. }));
        assert!(e.to_string().contains("disk full"), "{e}");
    }

    #[test]
    fn unreachable_host_is_explained_plainly() {
        for stderr in [
            "ssh: Could not resolve hostname totallybogus.example: Name or service not known\n",
            "ssh: connect to host 10.0.0.5 port 22: Connection refused\n",
            "ssh: connect to host 10.0.0.5 port 22: Connection timed out\n",
        ] {
            let msg = from_setup_stderr(stderr);
            assert!(msg.contains("Can't reach"), "{msg}");
        }
    }

    #[test]
    fn publickey_only_names_the_key_file_option() {
        let msg = from_setup_stderr("box: Permission denied (publickey).\n");
        assert!(msg.contains("only accepts key files"), "{msg}");
    }

    #[test]
    fn password_offered_but_refused_names_both_causes() {
        let msg = from_setup_stderr("box: Permission denied (publickey,password).\n");
        assert!(msg.contains("Wrong username or password") && msg.contains("does not allow password login"), "{msg}");
    }

    #[test]
    fn a_changed_host_key_gets_the_reinstall_explanation() {
        let msg = from_setup_stderr("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @\n");
        assert!(msg.contains("different from when you trusted it"), "{msg}");
    }

    #[test]
    fn password_auth_offered_is_detected_case_and_order_independent() {
        assert!(password_auth_was_offered("Permission denied (publickey,password,keyboard-interactive).\n"));
        assert!(password_auth_was_offered("Permission denied (password).\n"));
        assert!(!password_auth_was_offered("Permission denied (publickey).\n"));
    }
}
