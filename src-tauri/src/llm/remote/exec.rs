//! One AI call = one fresh russh connection with the restricted key: never
//! the notes `session_for` registry, whose mutex would hold sync for the
//! length of an LLM call (spec §4.5).

use std::path::PathBuf;
use std::time::Duration;

use russh::ChannelMsg;
use tokio_util::sync::CancellationToken;

use crate::accounts::AgentHost;
use crate::backend::ssh::russh_session::{self, Credential, RusshFailure};
use crate::llm::provider::ExtractError;

/// What one remote request produced. `exit` is `None` when the remote never
/// reported an exit status (death by signal, or a dropped connection); callers
/// must treat that as a failure, not as success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit: Option<u32>,
}

#[async_trait::async_trait]
pub trait RemoteExec: Send + Sync + std::fmt::Debug {
    /// Runs `command` with `stdin` as its input. The channel's stdin is never
    /// closed (wrapper protocol v2): the remote must know where its input ends,
    /// and it reads EOF as "Jodd went away". Cancel and the deadline hang up,
    /// which is what stops a run on the server.
    async fn exec(&self, command: &str, stdin: &[u8], deadline: Duration, cancel: CancellationToken)
        -> Result<RemoteOutput, ExtractError>;
}

#[derive(Debug, Clone)]
pub struct RusshExec {
    pub target: String,
    pub port: Option<u16>,
    pub key: PathBuf,
    pub known_hosts: PathBuf,
    /// For messages: `vps.example` / `vps.example:2222`.
    pub label: String,
}

impl RusshExec {
    pub fn for_host(h: &AgentHost) -> Result<Self, ExtractError> {
        let dir = crate::llm::remote::host_dir(h)
            .ok_or_else(|| ExtractError::NotConfigured("this AI host has no usable key directory".into()))?;
        Ok(Self { target: h.target.clone(), port: h.port, key: dir.join("id_ed25519"),
                  known_hosts: dir.join("known_hosts"), label: crate::llm::remote::host_label(h) })
    }
}

pub(crate) fn map_failure(f: &RusshFailure, label: &str) -> ExtractError {
    match f {
        RusshFailure::Resolve { .. } | RusshFailure::Refused { .. } | RusshFailure::TimedOut { .. } =>
            ExtractError::Transport(format!("Couldn't reach AI host {label}: {}", russh_session::stderr_for(f).trim())),
        RusshFailure::HostKeyMismatch =>
            ExtractError::Refused(format!("The AI host {label}'s identity changed. Set it up again in AI settings.")),
        RusshFailure::AuthRejected { .. } =>
            ExtractError::NotConfigured(format!("The AI host {label} no longer accepts Jodd's key. Set it up again in AI settings.")),
        RusshFailure::Other(m) => ExtractError::Transport(format!("AI host {label}: {m}")),
    }
}

#[async_trait::async_trait]
impl RemoteExec for RusshExec {
    async fn exec(&self, command: &str, stdin: &[u8], deadline: Duration, cancel: CancellationToken)
        -> Result<RemoteOutput, ExtractError> {
        let label = self.label.clone();
        let work = async {
            // `connect` pins against the file's CONTENTS.
            let pinned = std::fs::read_to_string(&self.known_hosts).map_err(|_| {
                ExtractError::NotConfigured(format!("The AI host {label} is not set up. Set it up in AI settings."))
            })?;
            let dead = |e: russh::Error| ExtractError::Transport(format!("AI host {label}: {e}"));
            // Connect, open and exec can each block (a filtered host), so cancel
            // must win in every one of them, not only in the read loop below.
            let setup = async {
                let h = russh_session::connect(&self.target, self.port, pinned, Credential::Key(&self.key)).await
                    .map_err(|f| map_failure(&f, &label))?;
                let ch = h.channel_open_session().await.map_err(dead)?;
                ch.exec(true, command).await.map_err(dead)?;
                Ok::<_, ExtractError>((h, ch))
            };
            let (_h, ch) = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ExtractError::Cancelled),
                r = setup => r?,
            };
            // stdin is fed from the same loop that reads the answer. Writing it
            // first and reading after stalls forever when the wrapper refuses the
            // request (exit 64/65) without reading a prompt larger than the SSH
            // window: the write waits for window the server will never grant.
            // No EOF after it (protocol v2): sshd closes the forced command's
            // stdin when this connection goes away, and that EOF is the
            // wrapper's cancel. Sending one here would cancel every run.
            let (mut rx, tx) = ch.split();
            let mut feed = Box::pin(tx.data(stdin));
            let (mut fed, mut feed_err) = (false, None);
            let (mut out, mut err, mut exit) = (Vec::new(), Vec::new(), None);
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        // Hanging up is what stops the run: sshd closes the
                        // wrapper's stdin and its watcher kills the CLI (or, mid-
                        // upload, the wrapper refuses the short prompt). The
                        // TERM is best-effort only: sshd refuses signal requests
                        // to forced-command sessions (measured 2026-10-13,
                        // OpenSSH 10.3p1; docs/BACKEND-SSH.md section 10).
                        let _ = tx.signal(russh::Sig::TERM).await;
                        let _ = tx.close().await;
                        return Err(ExtractError::Cancelled);
                    }
                    msg = rx.wait() => match msg {
                        Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data),
                        Some(ChannelMsg::ExtendedData { data, ext: 1 }) => err.extend_from_slice(&data),
                        Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                        Some(ChannelMsg::Failure) => return Err(ExtractError::Transport(format!("AI host {label} refused the request"))),
                        Some(ChannelMsg::Close) | None => break,
                        Some(_) => {}
                    },
                    r = &mut feed, if !fed => {
                        fed = true;
                        if let Err(e) = r { feed_err = Some(e); }
                    }
                }
            }
            // A failed write only matters when the server never answered: a
            // wrapper that exited early closes the channel under the writer.
            if let (None, Some(e)) = (exit, feed_err) { return Err(dead(e)); }
            Ok(RemoteOutput { stdout: String::from_utf8_lossy(&out).into_owned(),
                              stderr: String::from_utf8_lossy(&err).into_owned(), exit })
        };
        match tokio::time::timeout(deadline, work).await {
            Ok(r) => r,
            Err(_) => Err(ExtractError::Transport(format!("AI host {label} did not answer within {}s", deadline.as_secs()))),
        }
    }
}

/// These run a real ssh server on loopback and drive it with `sh`/`chmod`: Unix only.
#[cfg(all(test, unix))]
mod live_tests {
    use super::*;
    use crate::backend::ssh::test_server::{start, ServerOpts, TestServer};
    use crate::llm::remote::wrapper::{generate, CliPaths, wrapper_file_name, WRAPPER_VERSION};

    /// Echoes the prompt into the answer file; a prompt of exactly `sleep`
    /// hangs (the cancel/deadline tests), with no process-global env needed,
    /// and records the pid of the `sleep` it waits on in `$HOME/fake.pid`. It
    /// copies stdin to the file byte for byte so a lost trailing byte shows.
    const FAKE_CODEX: &str = "#!/bin/sh\nout=; prev=\nfor a in \"$@\"; do [ \"$prev\" = --output-last-message ] && out=$a; prev=$a; done\ncat > \"$out\"\nif [ \"$(cat \"$out\")\" = sleep ]; then sleep 20 & echo $! > \"$HOME/fake.pid\"; wait $!; fi\necho err-line >&2\n";

    /// A server with the wrapper installed and a restricted key authorised,
    /// exactly as `ai_host_setup` leaves it.
    async fn host() -> (TestServer, tempfile::TempDir, RusshExec) {
        let srv = start(ServerOpts::default()).await;
        let home = srv.home.path().to_path_buf();
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin/codex"), FAKE_CODEX).unwrap();
        std::process::Command::new("chmod").arg("755").arg(home.join("bin/codex")).status().unwrap();
        let text = generate(&CliPaths { path_env: "/usr/bin:/bin".into(), claude: None,
            codex: Some(home.join("bin/codex").to_string_lossy().into_owned()) }).unwrap();
        std::fs::create_dir_all(home.join(".jodd")).unwrap();
        let w = home.join(".jodd").join(wrapper_file_name());
        std::fs::write(&w, text).unwrap();
        std::process::Command::new("chmod").arg("700").arg(&w).status().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let made = crate::backend::ssh::russh_setup::generate_key(dir.path()).unwrap();
        let body = crate::backend::ssh::scripts::key_body(&made.pubkey_line);
        srv.authorize(&format!("restrict,command=\"{} --key abcd1234\" {body} jodd-ai-abcd1234", w.display()));
        std::fs::write(dir.path().join("known_hosts"), format!("{}\n", srv.host_key_line)).unwrap();
        let exec = RusshExec { target: srv.target(), port: Some(srv.port), key: dir.path().join("id_ed25519"),
            known_hosts: dir.path().join("known_hosts"), label: "test-host".into() };
        (srv, dir, exec)
    }

    fn req() -> String { format!("jodd-ai-run {WRAPPER_VERSION} run codex") }

    /// What `run_remote` sends: protocol v2 framing, no schema.
    fn v2(prompt: &str) -> Vec<u8> { crate::llm::agent_cli::remote_stdin(None, prompt) }

    /// Waits for the fake CLI's hanging `sleep` to record its pid.
    async fn fake_pid(srv: &TestServer) -> String {
        let p = srv.home.path().join("fake.pid");
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&p) { if !s.trim().is_empty() { return s.trim().to_string(); } }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the fake CLI never started");
    }

    /// True once `pid` is gone, polling for up to `within`.
    async fn gone_within(pid: &str, within: Duration) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < within {
            let alive = std::process::Command::new("kill").args(["-0", pid]).stderr(std::process::Stdio::null())
                .status().unwrap().success();
            if !alive { return true; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test]
    async fn stdout_stderr_and_exit_arrive_separately() {
        let (_s, _d, exec) = host().await;
        let out = exec.exec(&req(), &v2("hello"), Duration::from_secs(30), CancellationToken::new()).await.unwrap();
        assert_eq!(out.stdout, "hello");
        assert_eq!(out.stderr, "err-line\n");
        assert_eq!(out.exit, Some(0));
    }

    #[tokio::test]
    async fn large_prompt_round_trips() {
        let (_s, _d, exec) = host().await;
        let prompt: String = "ข้อความ line\n".repeat(80_000); // ~1.5 MB of UTF-8
        let out = exec.exec(&req(), &v2(&prompt), Duration::from_secs(60), CancellationToken::new()).await.unwrap();
        assert_eq!(out.stdout.len(), prompt.len());
        assert_eq!(out.stdout, prompt);
    }

    #[tokio::test]
    async fn a_version_mismatch_with_a_huge_prompt_returns_65_promptly() {
        // The wrapper exits 65 without reading stdin, so a prompt larger than the
        // SSH window must not leave the client stalled in `ch.data`.
        let (_s, _d, exec) = host().await;
        let big = vec![b'x'; 3 * 1024 * 1024];
        let stdin = [format!("-\n{}\n", big.len()).as_bytes(), &big].concat();
        let started = std::time::Instant::now();
        let out = exec.exec(&format!("jodd-ai-run {} run codex", WRAPPER_VERSION + 1), &stdin,
            Duration::from_secs(60), CancellationToken::new()).await.unwrap();
        assert_eq!(out.exit, Some(65), "{out:?}");
        assert!(out.stderr.contains("version mismatch"), "{out:?}");
        assert!(started.elapsed() < Duration::from_secs(10), "stalled for {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn the_restricted_key_cannot_run_anything_else() {
        let (_s, _d, exec) = host().await;
        let custom = format!("jodd-ai-run {WRAPPER_VERSION} run custom");
        for cmd in ["id", "sh", &custom] {
            let out = exec.exec(cmd, b"", Duration::from_secs(10), CancellationToken::new()).await.unwrap();
            assert_eq!(out.exit, Some(64), "{cmd}: {out:?}");
        }
    }

    #[tokio::test]
    async fn cancel_returns_promptly_and_stops_the_cli_on_the_server() {
        // Stop closes the channel and drops the connection; the server closes
        // the wrapper's stdin, and v2's watcher turns that EOF into a kill.
        let (srv, _d, exec) = host().await;
        let cancel = CancellationToken::new();
        let run = tokio::spawn({
            let (exec, cancel) = (exec.clone(), cancel.clone());
            async move { exec.exec(&req(), &v2("sleep"), Duration::from_secs(60), cancel).await }
        });
        let pid = fake_pid(&srv).await;
        let started = std::time::Instant::now();
        cancel.cancel();
        let r = run.await.unwrap();
        assert!(matches!(r, Err(ExtractError::Cancelled)), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        assert!(gone_within(&pid, Duration::from_secs(2)).await, "the CLI outlived Stop");
    }

    #[tokio::test]
    async fn a_run_that_needs_no_eof_answers_while_stdin_stays_open() {
        // v2: Jodd never closes stdin, so a wrapper that waited for EOF would
        // hang until the deadline. A short deadline proves it does not.
        let (_s, _d, exec) = host().await;
        let out = exec.exec(&req(), &v2("hi"), Duration::from_secs(5), CancellationToken::new()).await.unwrap();
        assert_eq!((out.exit, out.stdout.as_str()), (Some(0), "hi"), "{out:?}");
    }

    /// A server that accepts the TCP connection and never speaks SSH: the
    /// handshake waits, and Stop must still be heard.
    #[tokio::test]
    async fn cancel_during_a_black_holed_connect_returns_cancelled() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hold = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await { held.push(sock); }
        });
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("known_hosts"), "").unwrap();
        let exec = RusshExec { target: "me@127.0.0.1".into(), port: Some(port), key: dir.path().join("none"),
            known_hosts: dir.path().join("known_hosts"), label: "blackhole".into() };
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(300)).await; c2.cancel(); });
        let started = std::time::Instant::now();
        let r = exec.exec(&req(), &v2("hi"), Duration::from_secs(60), cancel).await;
        assert!(matches!(r, Err(ExtractError::Cancelled)), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        hold.abort();
    }

    #[tokio::test]
    async fn the_deadline_is_a_transport_error_naming_the_host_and_stops_the_cli() {
        let (srv, _d, exec) = host().await;
        // Long enough for connect + auth + exec + the fake's pidfile on a loaded CI runner.
        let r = exec.exec(&req(), &v2("sleep"), Duration::from_secs(3), CancellationToken::new()).await;
        match r { Err(ExtractError::Transport(m)) => assert!(m.contains("test-host"), "{m}"), other => panic!("{other:?}") }
        // The deadline drops the connection too, which is what stops the run.
        let pid = fake_pid(&srv).await;
        assert!(gone_within(&pid, Duration::from_secs(2)).await, "the CLI outlived the deadline");
    }

    #[tokio::test]
    async fn an_unreachable_host_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("known_hosts"), "").unwrap();
        let exec = RusshExec { target: "me@127.0.0.1".into(), port: Some(1), key: dir.path().join("none"),
            known_hosts: dir.path().join("known_hosts"), label: "127.0.0.1:1".into() };
        let r = exec.exec(&req(), b"", Duration::from_secs(5), CancellationToken::new()).await;
        match r { Err(ExtractError::Transport(m)) => assert!(m.contains("Couldn't reach AI host 127.0.0.1:1"), "{m}"), other => panic!("{other:?}") }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_host_refuses_a_key_dir_that_is_not_a_uuid() {
        let h = AgentHost { target: "me@vps".into(), port: None, key_dir: "../..".into() };
        assert!(matches!(RusshExec::for_host(&h), Err(ExtractError::NotConfigured(_))));
    }
}
