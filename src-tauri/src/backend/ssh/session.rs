//! The one seam between the SSH vertical and a remote POSIX shell.
//!
//! Every storage operation is a `sh` script run through [`SshSession::exec`].
//! [`ProcessSession`] keeps ONE shell process alive per account and frames
//! each request with nonce markers, so a 5-second sync tick costs no SSH
//! handshake (Win32-OpenSSH has no ControlMaster). Production runs
//! `ssh … sh`; tests run a plain local `sh` through the same framing, so the
//! scripts under test are the scripts that ship.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

/// Per-request ceiling. Generous because a listing hashes the whole vault.
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a refused key or untrusted host key is remembered before the
/// session tries ssh again. The worker ticks every 5 seconds; respawning on
/// each one is ~12 failed logins a minute, which is exactly what fail2ban-
/// style sshd guards ban an address for.
pub const AUTH_BACKOFF: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit: i32,
}

#[derive(Debug)]
pub enum SshError {
    /// The `ssh` (or `sh`) binary could not be started at all.
    Spawn(std::io::Error),
    /// The shell process ended mid-request. `stderr` is what the process
    /// itself printed — where OpenSSH reports auth and host-key failures.
    Disconnected { stderr: String },
    Timeout,
    /// Output that breaks the framing. The session is dropped and respawned.
    Protocol(String),
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SshError::Spawn(e) => write!(f, "could not start ssh: {e}"),
            SshError::Disconnected { stderr } => write!(f, "ssh disconnected: {}", stderr.trim()),
            SshError::Timeout => write!(f, "ssh request timed out"),
            SshError::Protocol(m) => write!(f, "ssh framing error: {m}"),
        }
    }
}

impl std::error::Error for SshError {}

#[async_trait]
pub trait SshSession: Send + Sync {
    async fn exec(&self, script: &str) -> Result<ExecOutput, SshError>;
}

/// Single-quote `s` for POSIX sh. File names come from programs on the
/// server, so every path in every script goes through here.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A target is handed to ssh after `--`, but a leading `-` is refused anyway:
/// it is never a real host, and refusing it keeps the rule local.
pub fn validate_target(target: &str) -> Result<(), String> {
    if target.is_empty() {
        return Err("the SSH target is empty".into());
    }
    if target.starts_with('-') {
        return Err("an SSH target cannot start with '-'".into());
    }
    if target.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("an SSH target cannot contain spaces or control characters".into());
    }
    Ok(())
}

pub fn ssh_binary() -> PathBuf {
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        root.join("System32").join("OpenSSH").join("ssh.exe")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/usr/bin/ssh")
    }
}

#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

/// Extra `ssh` argv a Managed account contributes on top of the fixed
/// Advanced-account flags. `SshOptions::default()` — no port, no key, no
/// known_hosts — produces argv byte-identical to the pre-Managed shape, so
/// Advanced accounts and the browse-before-adding flow are unaffected.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SshOptions {
    pub port: Option<u16>,
    pub key: Option<PathBuf>,
    pub known_hosts: Option<PathBuf>,
}

impl SpawnSpec {
    pub fn ssh(target: &str, opts: &SshOptions) -> Self {
        let mut args: Vec<String> = [
            "-o", "BatchMode=yes",
            "-o", "ServerAliveInterval=15",
            "-o", "ConnectTimeout=10",
        ].iter().map(|s| s.to_string()).collect();
        if let Some(port) = opts.port {
            args.push("-p".into());
            args.push(port.to_string());
        }
        if let Some(key) = &opts.key {
            args.push("-i".into());
            args.push(key.display().to_string());
            args.push("-o".into());
            args.push("IdentitiesOnly=yes".into());
        }
        if let Some(kh) = &opts.known_hosts {
            args.push("-o".into());
            // `UserKnownHostsFile` accepts a whitespace-separated list of
            // files, so ssh splits an unquoted path at any space — and
            // Jodd's own desktop data dir has one by construction on macOS
            // (`~/Library/Application Support/...`). Quoting keeps the path
            // as one token the way ssh's own config-file parser expects.
            args.push(format!("UserKnownHostsFile=\"{}\"", kh.display()));
            args.push("-o".into());
            args.push("StrictHostKeyChecking=yes".into());
        }
        args.push("-T".into());
        args.push("--".into());
        args.push(target.to_string());
        args.push("sh".into());
        SpawnSpec { program: ssh_binary(), args, cwd: None }
    }

    pub fn local_sh(cwd: PathBuf) -> Self {
        SpawnSpec { program: PathBuf::from("sh"), args: Vec::new(), cwd: Some(cwd) }
    }
}

struct Live {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<StdMutex<String>>,
    stderr_task: JoinHandle<()>,
}

pub struct ProcessSession {
    spec: SpawnSpec,
    timeout: Duration,
    live: tokio::sync::Mutex<Option<Live>>,
    /// The last auth-class failure and ssh's stderr for it; see `AUTH_BACKOFF`.
    auth_failed: StdMutex<Option<(Instant, String)>>,
}

impl ProcessSession {
    pub fn new(spec: SpawnSpec, timeout: Duration) -> Self {
        ProcessSession { spec, timeout, live: tokio::sync::Mutex::new(None), auth_failed: StdMutex::new(None) }
    }

    fn spawn(&self) -> Result<Live, SshError> {
        let mut cmd = Command::new(&self.spec.program);
        cmd.args(&self.spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = &self.spec.cwd {
            cmd.current_dir(dir);
        }
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().map_err(SshError::Spawn)?;
        let missing = |p: &str| SshError::Protocol(format!("no {p} pipe"));
        let stdin = child.stdin.take().ok_or_else(|| missing("stdin"))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| missing("stdout"))?);
        let err_pipe = child.stderr.take().ok_or_else(|| missing("stderr"))?;
        let stderr = Arc::new(StdMutex::new(String::new()));
        let sink = stderr.clone();
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(err_pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut s = sink.lock().unwrap();
                if s.len() > 64 * 1024 {
                    s.clear();
                }
                s.push_str(&line);
                s.push('\n');
            }
        });
        Ok(Live { child, stdin, stdout, stderr, stderr_task })
    }
}

fn disconnected() -> SshError {
    SshError::Disconnected { stderr: String::new() }
}

async fn run_framed(live: &mut Live, script: &str, nonce: &str) -> Result<ExecOutput, SshError> {
    live.stdin.write_all(super::framing::frame(script, nonce).as_bytes()).await.map_err(|_| disconnected())?;
    live.stdin.flush().await.map_err(|_| disconnected())?;
    super::framing::read_framed(&mut live.stdout, nonce).await
}

#[async_trait]
impl SshSession for ProcessSession {
    async fn exec(&self, script: &str) -> Result<ExecOutput, SshError> {
        let mut guard = self.live.lock().await;
        if let Some((at, stderr)) = self.auth_failed.lock().unwrap().as_ref() {
            if at.elapsed() < AUTH_BACKOFF {
                return Err(SshError::Disconnected { stderr: stderr.clone() });
            }
        }
        if guard.is_none() {
            *guard = Some(self.spawn()?);
        } else if let Some(live) = guard.as_ref() {
            // The auth hold judges the stderr of the request that died, not
            // whatever the process printed during earlier ones. Never cleared
            // on a fresh spawn: ssh's own connect-time refusal is already
            // arriving in it.
            live.stderr.lock().unwrap().clear();
        }
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let live = guard.as_mut().expect("spawned above");
        match tokio::time::timeout(self.timeout, run_framed(live, script, &nonce)).await {
            Ok(Ok(out)) => Ok(out),
            Ok(Err(SshError::Disconnected { .. })) => {
                let mut dead = guard.take().expect("live session");
                let _ = tokio::time::timeout(Duration::from_secs(2), dead.child.wait()).await;
                let _ = tokio::time::timeout(Duration::from_secs(2), &mut dead.stderr_task).await;
                let stderr = dead.stderr.lock().unwrap().clone();
                if super::classify::auth_hint(&stderr, "").is_some() {
                    *self.auth_failed.lock().unwrap() = Some((Instant::now(), stderr.clone()));
                }
                Err(SshError::Disconnected { stderr })
            }
            // Framing is desynchronised: the only safe state is a new process.
            Ok(Err(e)) => {
                guard.take();
                Err(e)
            }
            Err(_) => {
                guard.take();
                Err(SshError::Timeout)
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn local(timeout: Duration) -> (tempfile::TempDir, ProcessSession) {
        let dir = tempfile::tempdir().unwrap();
        let s = ProcessSession::new(SpawnSpec::local_sh(dir.path().to_path_buf()), timeout);
        (dir, s)
    }

    #[tokio::test]
    async fn stdout_and_exit_come_back() {
        let (_d, s) = local(Duration::from_secs(10));
        let o = s.exec("echo hi").await.unwrap();
        assert_eq!(o, ExecOutput { stdout: "hi\n".into(), stderr: String::new(), exit: 0 });
    }

    #[tokio::test]
    async fn stderr_and_a_nonzero_exit_are_kept_apart() {
        let (_d, s) = local(Duration::from_secs(10));
        let o = s.exec("echo oops >&2; exit 7").await.unwrap();
        assert_eq!((o.stdout.as_str(), o.stderr.as_str(), o.exit), ("", "oops\n", 7));
    }

    #[tokio::test]
    async fn output_without_a_trailing_newline_is_kept_exactly() {
        let (_d, s) = local(Duration::from_secs(10));
        assert_eq!(s.exec("printf abc").await.unwrap().stdout, "abc");
        assert_eq!(s.exec("printf 'abc\\n\\n'").await.unwrap().stdout, "abc\n\n");
    }

    #[tokio::test]
    async fn a_marker_lookalike_in_content_does_not_end_the_frame() {
        let (_d, s) = local(Duration::from_secs(10));
        let o = s
            .exec("printf '%s\\n' '@@JODD_ERR_0000' '@@JODD_END_0000 0'; echo after")
            .await
            .unwrap();
        assert!(o.stdout.ends_with("after\n"), "{:?}", o.stdout);
    }

    /// `$$` inside the per-request subshell is the long-lived shell's pid.
    #[tokio::test]
    async fn one_process_serves_every_request() {
        let (_d, s) = local(Duration::from_secs(10));
        let a = s.exec("echo $$").await.unwrap().stdout;
        let b = s.exec("echo $$").await.unwrap().stdout;
        assert_eq!(a, b);
    }

    /// A script reading stdin must get EOF, not the command stream.
    #[tokio::test]
    async fn a_script_cannot_read_the_command_stream() {
        let (_d, s) = local(Duration::from_secs(5));
        assert_eq!(s.exec("cat").await.unwrap().stdout, "");
        assert_eq!(s.exec("echo still").await.unwrap().stdout, "still\n");
    }

    #[tokio::test]
    async fn a_dead_shell_is_reported_then_respawned() {
        let (_d, s) = local(Duration::from_secs(10));
        let err = s.exec("kill -9 $$").await.unwrap_err();
        assert!(matches!(err, SshError::Disconnected { .. }), "{err:?}");
        assert_eq!(s.exec("echo back").await.unwrap().stdout, "back\n");
    }

    #[tokio::test]
    async fn a_hung_script_times_out_and_the_next_request_recovers() {
        let (_d, s) = local(Duration::from_millis(300));
        assert!(matches!(s.exec("sleep 5").await.unwrap_err(), SshError::Timeout));
        assert_eq!(s.exec("echo ok").await.unwrap().stdout, "ok\n");
    }

    fn sh_c(dir: &std::path::Path, script: &str) -> SpawnSpec {
        SpawnSpec { program: PathBuf::from("sh"), args: vec!["-c".into(), script.into()], cwd: Some(dir.to_path_buf()) }
    }

    /// I-1: `ssh host sh` runs the login user's shell startup first; a MOTD
    /// or a `.bashrc` echo must not land in the first response.
    #[tokio::test]
    async fn shell_startup_output_is_not_part_of_any_response() {
        let d = tempfile::tempdir().unwrap();
        let s = ProcessSession::new(sh_c(d.path(), "echo Welcome to the box; exec sh"), Duration::from_secs(10));
        assert_eq!(s.exec("echo hi").await.unwrap().stdout, "hi\n");
        let s = ProcessSession::new(sh_c(d.path(), "echo Welcome to the box; exec sh"), Duration::from_secs(10));
        let root = d.path().join("vault");
        let o = s.exec(&crate::backend::ssh::scripts::resolve_root(root.to_str().unwrap(), true)).await.unwrap();
        assert_eq!(o.stdout.trim(), root.canonicalize().unwrap().to_str().unwrap());
        assert!(!o.stdout.contains("Welcome"), "{:?}", o.stdout);
    }

    /// A banner with no trailing newline would put the BEGIN marker mid-line,
    /// where an exact-line match never sees it.
    #[tokio::test]
    async fn startup_output_without_a_newline_does_not_hide_the_marker() {
        let d = tempfile::tempdir().unwrap();
        let s = ProcessSession::new(sh_c(d.path(), "printf 'no newline banner'; exec sh"), Duration::from_secs(5));
        assert_eq!(s.exec("echo hi").await.unwrap().stdout, "hi\n");
        assert_eq!(s.exec("echo again").await.unwrap().stdout, "again\n");
    }

    /// The auth hold must judge only the request that died: a "Permission
    /// denied" the long-lived process printed during an EARLIER request (fd 3
    /// here is the process's own stderr) is not why this one failed.
    #[tokio::test]
    async fn an_earlier_requests_stderr_does_not_trigger_the_auth_hold() {
        let d = tempfile::tempdir().unwrap();
        let s = ProcessSession::new(sh_c(d.path(), "echo spawn >> spawns; exec 3>&2; exec sh"), Duration::from_secs(10));
        s.exec("echo 'sudo: Permission denied' >&3").await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await; // let the stderr reader drain it
        assert!(matches!(s.exec("kill -9 $$").await.unwrap_err(), SshError::Disconnected { .. }));
        assert_eq!(s.exec("echo back").await.unwrap().stdout, "back\n", "not held back");
        assert_eq!(std::fs::read_to_string(d.path().join("spawns")).unwrap(), "spawn\nspawn\n");
    }

    /// I-8: a refused key must not be retried every sync tick — sshd's
    /// fail2ban would ban the user's address.
    #[tokio::test]
    async fn an_auth_failure_is_remembered_instead_of_respawning() {
        let d = tempfile::tempdir().unwrap();
        let s = ProcessSession::new(
            sh_c(d.path(), "echo spawn >> spawns; echo 'me@box: Permission denied (publickey).' >&2; exit 255"),
            Duration::from_secs(10),
        );
        for _ in 0..2 {
            match s.exec("echo hi").await.unwrap_err() {
                SshError::Disconnected { stderr } => assert!(stderr.contains("Permission denied"), "{stderr:?}"),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(std::fs::read_to_string(d.path().join("spawns")).unwrap(), "spawn\n", "one spawn, not one per request");
    }

    /// Only auth is held back: a network drop respawns on the next request.
    #[tokio::test]
    async fn a_non_auth_disconnect_is_not_remembered() {
        let d = tempfile::tempdir().unwrap();
        let s = ProcessSession::new(
            sh_c(d.path(), "echo spawn >> spawns; echo 'ssh: connect to host box port 22: Connection refused' >&2; exit 255"),
            Duration::from_secs(10),
        );
        for _ in 0..2 {
            assert!(matches!(s.exec("echo hi").await.unwrap_err(), SshError::Disconnected { .. }));
        }
        assert_eq!(std::fs::read_to_string(d.path().join("spawns")).unwrap(), "spawn\nspawn\n");
    }

    #[tokio::test]
    async fn sh_quote_survives_hostile_names() {
        let (d, s) = local(Duration::from_secs(10));
        for name in ["it's", "$(touch pwned)", "`touch pwned`", "a b", "ไทย", "-rf", "x;touch pwned"] {
            let o = s.exec(&format!("printf %s {}", sh_quote(name))).await.unwrap();
            assert_eq!(o.stdout, name);
        }
        assert!(!d.path().join("pwned").exists(), "a quoted name was executed");
    }

    #[test]
    fn validate_target_refuses_option_injection_and_whitespace() {
        assert!(validate_target("me@box").is_ok());
        assert!(validate_target("box").is_ok());
        assert!(validate_target("").is_err());
        assert!(validate_target("-oProxyCommand=touch x").is_err());
        assert!(validate_target("a b").is_err());
        assert!(validate_target("a\nb").is_err());
    }

    /// Gotcha-#26-class trap: a Finder-launched app has no Homebrew PATH.
    #[test]
    fn the_ssh_binary_is_an_absolute_path() {
        assert!(ssh_binary().is_absolute(), "{:?}", ssh_binary());
        let spec = SpawnSpec::ssh("me@box", &SshOptions::default());
        let dd = spec.args.iter().position(|a| a == "--").expect("-- before the target");
        assert_eq!(spec.args[dd + 1], "me@box");
        assert!(spec.args.iter().any(|a| a == "BatchMode=yes"));
        assert!(!spec.args.iter().any(|a| a.contains("StrictHostKeyChecking")));
    }

    #[test]
    fn managed_options_add_port_key_and_known_hosts() {
        let opts = SshOptions {
            port: Some(2222),
            key: Some(PathBuf::from("/data/ssh/acct/id_ed25519")),
            known_hosts: Some(PathBuf::from("/data/ssh/acct/known_hosts")),
        };
        let spec = SpawnSpec::ssh("me@box", &opts);
        assert!(spec.args.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(spec.args.windows(2).any(|w| w == ["-i", "/data/ssh/acct/id_ed25519"]));
        assert!(spec.args.iter().any(|a| a == "IdentitiesOnly=yes"));
        assert!(spec.args.windows(2).any(|w| w[0] == "-o" && w[1] == "UserKnownHostsFile=\"/data/ssh/acct/known_hosts\""));
        assert!(spec.args.iter().any(|a| a == "StrictHostKeyChecking=yes"));
    }

    /// C1: `UserKnownHostsFile` accepts a whitespace-separated list of files,
    /// so an unquoted path with a space (e.g. macOS's own
    /// `~/Library/Application Support/jodd/ssh/<uuid>/`) gets split by ssh
    /// and host-key verification breaks. Every Managed account's known_hosts
    /// path must arrive as one quoted token.
    #[test]
    fn known_hosts_path_with_a_space_is_quoted() {
        let opts = SshOptions {
            port: None,
            key: None,
            known_hosts: Some(PathBuf::from("/tmp/a b/known_hosts")),
        };
        let spec = SpawnSpec::ssh("me@box", &opts);
        assert!(
            spec.args.iter().any(|a| a == "UserKnownHostsFile=\"/tmp/a b/known_hosts\""),
            "{:?}",
            spec.args
        );
    }

    #[test]
    fn advanced_options_leave_argv_unchanged() {
        let spec = SpawnSpec::ssh("me@box", &SshOptions::default());
        assert!(!spec.args.iter().any(|a| a == "-i" || a == "-p"));
        assert!(!spec.args.iter().any(|a| a.contains("StrictHostKeyChecking")));
    }
}
