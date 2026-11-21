//! The MCP session's byte stream: one fresh russh connection, `exec` of
//! [`MCP_REQUEST`](super::mcp::MCP_REQUEST), and that channel pumped into two
//! in-memory pipes that [`McpClient`] reads and writes. Also the one place
//! that tells a legacy host (a `jodd-ai-run` wrapper, no MCP server) from a
//! broken one.
//!
//! Pump rules:
//! - channel data goes to `read`; stderr (extended data 1) to a buffer capped
//!   at [`STDERR_CAP`]; the exit status is recorded;
//! - what the client writes to `write` goes out as channel data;
//! - when the client drops `write`, the pump sends EOF and closes;
//! - when the channel closes (or the connection drops), `end` fires;
//! - `cancel` closes the channel and drops the connection, after giving the
//!   client [`CANCEL_FLUSH`] to get its `notifications/cancelled` out.

use std::sync::Mutex;
use std::time::Duration;

use russh::{Channel, ChannelMsg};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::mcp::{McpClient, McpError};
use crate::llm::provider::ExtractError;

/// How long the MCP handshake may take once the channel is open. A host that
/// accepts the stream and never answers fails here instead of hanging.
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(20);
/// After a failed handshake, how long to wait for the remote's exit status
/// before deciding it was not a legacy wrapper.
pub const LEGACY_EXIT_WAIT: Duration = Duration::from_secs(5);
/// What the legacy wrapper (`wrapper.rs` TEMPLATE) says to a request it does
/// not know, and how it exits.
const LEGACY_EXIT: u32 = 64;
const LEGACY_STDERR: &str = "unknown request";
/// The most stderr kept from one channel.
pub const STDERR_CAP: usize = 64 * 1024;
/// How long a cancel waits for the client's last bytes (its
/// `notifications/cancelled`) before closing the channel under it.
pub const CANCEL_FLUSH: Duration = Duration::from_millis(250);
/// After the client is done, how long to wait for the server's close (and the
/// exit status) before dropping the connection anyway.
const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// How long the pump waits for an EOF or a close to go out: a dead network
/// must not keep the pump (and `end`) from finishing.
const TEARDOWN_WAIT: Duration = Duration::from_secs(2);
/// How much stderr a handshake error quotes.
const QUOTED_STDERR: usize = 300;
const PIPE_BUF: usize = 1 << 16;

/// How the remote process ended, as far as the channel said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelEnd {
    /// `None`: no exit status arrived (a signal, a dropped connection, a cancel).
    pub exit: Option<u32>,
    pub stderr: String,
}

/// One open MCP channel: the client reads `read` and writes `write`.
pub struct McpChannel {
    pub read: DuplexStream,
    pub write: DuplexStream,
    pub end: oneshot::Receiver<ChannelEnd>,
}

#[async_trait::async_trait]
pub trait RemoteMcp: Send + Sync + std::fmt::Debug {
    /// One fresh connection, exec(MCP_REQUEST), pumped into duplex pipes.
    async fn open_mcp(&self, cancel: CancellationToken) -> Result<McpChannel, ExtractError>;
}

/// What answered on the channel.
pub enum Opened {
    /// An MCP host, and its `serverInfo.version`.
    Mcp(McpClient, String),
    /// A host with only the legacy `jodd-ai-run` wrapper.
    Legacy,
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Opened::Mcp(_, version) => f.debug_tuple("Mcp").field(version).finish(),
            Opened::Legacy => f.write_str("Legacy"),
        }
    }
}

/// Connect the MCP client on the channel, within [`HANDSHAKE_DEADLINE`]. If
/// the handshake fails and the remote process ended with exit 64 and
/// "unknown request" on stderr (within [`LEGACY_EXIT_WAIT`]), it is Legacy.
pub async fn connect(ch: McpChannel) -> Result<Opened, McpError> {
    connect_within(ch, HANDSHAKE_DEADLINE).await
}

async fn connect_within(ch: McpChannel, deadline: Duration) -> Result<Opened, McpError> {
    let McpChannel { read, write, end } = ch;
    let failed = match tokio::time::timeout(deadline, McpClient::connect(read, write)).await {
        Ok(Ok((client, version))) => return Ok(Opened::Mcp(client, version)),
        Ok(Err(e)) => e,
        // Dropping the half-done client drops both pipes: the pump closes the channel.
        Err(_) => {
            return Err(McpError::Protocol(format!(
                "the AI host did not complete the MCP handshake within {}s",
                deadline.as_secs_f32()
            )))
        }
    };
    match tokio::time::timeout(LEGACY_EXIT_WAIT, end).await {
        Ok(Ok(end)) if is_legacy(&end) => Ok(Opened::Legacy),
        Ok(Ok(end)) => Err(with_exit(failed, &end)),
        _ => Err(failed),
    }
}

/// The legacy wrapper's answer to a request it does not know.
fn is_legacy(end: &ChannelEnd) -> bool {
    end.exit == Some(LEGACY_EXIT) && end.stderr.contains(LEGACY_STDERR)
}

/// A failed handshake, with how the remote ended: `exit 127: jodd-ai-mcp: not
/// found` says what to fix where "the connection closed" does not.
fn with_exit(e: McpError, end: &ChannelEnd) -> McpError {
    let McpError::Protocol(m) = e else { return e };
    let stderr = end.stderr.trim();
    let quoted: String = stderr.chars().take(QUOTED_STDERR).collect();
    let more = if quoted.len() < stderr.len() { "…" } else { "" };
    McpError::Protocol(match (end.exit, quoted.is_empty()) {
        (Some(code), true) => format!("{m} (exit {code})"),
        (Some(code), false) => format!("{m} (exit {code}: {quoted}{more})"),
        (None, false) => format!("{m} ({quoted}{more})"),
        (None, true) => m,
    })
}

/// Pump `channel` (on the connection `conn`, held until the pump ends and
/// then dropped, which closes the connection) into a fresh [`McpChannel`].
pub(crate) fn pump<H: Send + 'static>(
    conn: H,
    channel: Channel<russh::client::Msg>,
    cancel: CancellationToken,
) -> McpChannel {
    let (read, to_client) = tokio::io::duplex(PIPE_BUF);
    let (write, from_client) = tokio::io::duplex(PIPE_BUF);
    let (end_tx, end) = oneshot::channel();
    tokio::spawn(async move {
        let ended = run_pump(channel, to_client, from_client, cancel).await;
        // The channel is over: hang up before anyone acts on how it ended.
        drop(conn);
        let _ = end_tx.send(ended);
    });
    McpChannel { read, write, end }
}

async fn run_pump(
    channel: Channel<russh::client::Msg>,
    mut to_client: DuplexStream,
    mut from_client: DuplexStream,
    cancel: CancellationToken,
) -> ChannelEnd {
    let (mut rx, tx) = channel.split();
    let seen: Mutex<(Option<u32>, Vec<u8>)> = Mutex::new((None, Vec::new()));
    {
        // Client → channel. A client that is done (dropped `write`) gets EOF
        // then close, which also ends the host's session.
        let up = async {
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                match from_client.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.data(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = tokio::time::timeout(TEARDOWN_WAIT, tx.eof()).await;
            let _ = tokio::time::timeout(TEARDOWN_WAIT, tx.close()).await;
        };
        // Channel → client, until the channel closes or the connection drops.
        let down = async {
            let mut client_reads = true;
            while let Some(msg) = rx.wait().await {
                match msg {
                    ChannelMsg::Data { data } => {
                        // A client that dropped `read` must not stop the pump.
                        if client_reads && to_client.write_all(&data).await.is_err() {
                            client_reads = false;
                        }
                    }
                    ChannelMsg::ExtendedData { data, ext: 1 } => {
                        let mut s = seen.lock().unwrap();
                        let room = STDERR_CAP.saturating_sub(s.1.len());
                        s.1.extend_from_slice(&data[..room.min(data.len())]);
                    }
                    ChannelMsg::ExitStatus { exit_status } => seen.lock().unwrap().0 = Some(exit_status),
                    ChannelMsg::Eof => {
                        let _ = to_client.shutdown().await;
                    }
                    // A refused exec leaves the channel open with nothing behind
                    // it; treat it as closed rather than wait on it.
                    ChannelMsg::Failure | ChannelMsg::Close => break,
                    _ => {}
                }
            }
        };
        tokio::pin!(up, down);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Let `notifications/cancelled` out first: it is queued behind
                // nothing, so a short wait is enough, and a dead network must
                // not hold the cancel.
                let _ = tokio::time::timeout(CANCEL_FLUSH, &mut up).await;
                let _ = tokio::time::timeout(TEARDOWN_WAIT, tx.close()).await;
            }
            _ = &mut up => {
                // Close sent; wait (briefly) for the host's close and exit status.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {}
                    _ = tokio::time::timeout(CLOSE_WAIT, &mut down) => {}
                }
            }
            _ = &mut down => {}
        }
    }
    let (exit, stderr) = seen.into_inner().unwrap_or_else(|p| p.into_inner());
    ChannelEnd { exit, stderr: String::from_utf8_lossy(&stderr).into_owned() }
}

/// These run russh's server side in-process on loopback (`test_server`).
#[cfg(all(test, unix))]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Instant;

    use super::*;
    use crate::backend::ssh::test_server::{start, ExecCtx, ExecHandler, ServerOpts, TestServer};
    use crate::llm::remote::exec::RusshExec;
    use crate::llm::remote::fake_host::{FakeHost, Recorder, Reply};
    use crate::llm::remote::mcp::{InferArgs, MCP_REQUEST};

    /// A server whose exec requests go to `handler`, and a [`RusshExec`] with
    /// a key it accepts. The key line forces no command, so the handler sees
    /// exactly what Jodd asked to run.
    async fn served(handler: ExecHandler) -> (TestServer, tempfile::TempDir, RusshExec) {
        let srv = start(ServerOpts { exec_handler: Some(handler), ..Default::default() }).await;
        let dir = tempfile::tempdir().unwrap();
        let made = crate::backend::ssh::russh_setup::generate_key(dir.path()).unwrap();
        let body = crate::backend::ssh::scripts::key_body(&made.pubkey_line);
        srv.authorize(&format!("{body} jodd-ai-abcd1234"));
        std::fs::write(dir.path().join("known_hosts"), format!("{}\n", srv.host_key_line)).unwrap();
        let exec = RusshExec {
            target: srv.target(),
            port: Some(srv.port),
            key: dir.path().join("id_ed25519"),
            known_hosts: dir.path().join("known_hosts"),
            label: "test-host".into(),
        };
        (srv, dir, exec)
    }

    /// What a [`serving`] handler saw.
    #[derive(Clone, Default)]
    struct Seen {
        commands: Arc<StdMutex<Vec<String>>>,
        hosts: Arc<StdMutex<Vec<Recorder>>>,
    }

    impl Seen {
        fn commands(&self) -> Vec<String> {
            self.commands.lock().unwrap().clone()
        }
        fn host(&self) -> Recorder {
            self.hosts.lock().unwrap().last().cloned().expect("no exec reached the handler")
        }
    }

    /// Serves a fresh fake AI host on each exec's streams. It exits 0 once
    /// the session ends (the server sends the status when stdout closes).
    fn serving(make: impl Fn() -> FakeHost + Send + Sync + 'static, seen: Seen) -> ExecHandler {
        Arc::new(move |ctx: ExecCtx| {
            seen.commands.lock().unwrap().push(ctx.command.clone());
            let rec = make().serve(ctx.stdin, ctx.stdout);
            seen.hosts.lock().unwrap().push(rec);
            drop(ctx.stderr);
            Box::pin(async { 0 })
        })
    }

    /// Writes `stderr` and exits `code` without reading anything.
    fn exiting(code: u32, stderr: &'static str) -> ExecHandler {
        Arc::new(move |mut ctx: ExecCtx| {
            Box::pin(async move {
                let _ = ctx.stderr.write_all(stderr.as_bytes()).await;
                code
            })
        })
    }

    fn args<'a>(prompt: &'a str) -> InferArgs<'a> {
        InferArgs { harness: "claude", prompt, system: "", schema: None, timeout_secs: None }
    }

    async fn open(exec: &RusshExec, cancel: CancellationToken) -> McpChannel {
        exec.open_mcp(cancel).await.expect("open_mcp")
    }

    async fn mcp(exec: &RusshExec, cancel: CancellationToken) -> (McpClient, String) {
        match connect(open(exec, cancel).await).await {
            Ok(Opened::Mcp(client, version)) => (client, version),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn mcp_over_russh_loopback() {
        let seen = Seen::default();
        let make = || FakeHost::new("1.4.2").reply("infer", Reply::text("สวัสดี over ssh"));
        let (srv, _d, exec) = served(serving(make, seen.clone())).await;
        let (client, version) = mcp(&exec, CancellationToken::new()).await;
        assert_eq!(version, "1.4.2");
        assert_eq!(seen.commands(), vec![MCP_REQUEST.to_string()]);
        let got = client.infer(args("P"), CancellationToken::new()).await.expect("infer");
        assert_eq!(got, "สวัสดี over ssh");
        assert_eq!(seen.host().last_args("infer").unwrap()["prompt"], "P");
        client.close().await;
        assert_eq!(srv.connections.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_large_prompt_and_answer_cross_the_channel() {
        // Far past the SSH window and the pipes' buffers, both ways.
        let answer = "ตอบ answer\n".repeat(60_000);
        let seen = Seen::default();
        let reply = answer.clone();
        let make = move || FakeHost::new("1.0.0").reply("infer", Reply::text(&reply));
        let (_srv, _d, exec) = served(serving(make, seen.clone())).await;
        let (client, _) = mcp(&exec, CancellationToken::new()).await;
        let prompt = "ข้อความ line\n".repeat(80_000);
        let got = client.infer(args(&prompt), CancellationToken::new()).await.expect("infer");
        assert_eq!(got.len(), answer.len());
        assert_eq!(seen.host().last_args("infer").unwrap()["prompt"].as_str().map(str::len), Some(prompt.len()));
        client.close().await;
    }

    #[tokio::test]
    async fn a_legacy_wrapper_is_detected() {
        // The real generated wrapper v2, forced by the key line, as on a host
        // set up before the kit existed.
        let (_srv, _d, exec) = crate::llm::remote::exec::live_tests::host().await;
        let started = Instant::now();
        let got = connect(open(&exec, CancellationToken::new()).await).await;
        assert!(matches!(got, Ok(Opened::Legacy)), "{got:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn the_legacy_answer_is_detected_in_process_too() {
        // The same detection with no `sh`: the Android emulator runs this one.
        let (_srv, _d, exec) = served(exiting(64, "unknown request\n")).await;
        let got = connect(open(&exec, CancellationToken::new()).await).await;
        assert!(matches!(got, Ok(Opened::Legacy)), "{got:?}");
    }

    #[tokio::test]
    async fn an_unrelated_exit_is_not_legacy() {
        for (code, stderr) in [(1, "boom"), (64, "no key id"), (1, "unknown request"), (127, "")] {
            let (_srv, _d, exec) = served(exiting(code, stderr)).await;
            match connect(open(&exec, CancellationToken::new()).await).await {
                Err(McpError::Protocol(m)) => {
                    assert!(m.contains(&format!("exit {code}")), "{code} {stderr:?}: {m}");
                    assert!(m.contains(stderr), "{code} {stderr:?}: {m}");
                }
                other => panic!("{code} {stderr:?}: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_silent_host_fails_the_handshake_at_the_deadline() {
        // Accepts the stream, reads it, never answers.
        let handler: ExecHandler = Arc::new(|mut ctx: ExecCtx| {
            Box::pin(async move {
                let mut sink = Vec::new();
                let _ = ctx.stdin.read_to_end(&mut sink).await;
                drop(ctx);
                0
            })
        });
        let (_srv, _d, exec) = served(handler).await;
        let started = Instant::now();
        let got = connect_within(open(&exec, CancellationToken::new()).await, Duration::from_millis(500)).await;
        assert!(matches!(got, Err(McpError::Protocol(ref m)) if m.contains("handshake")), "{got:?}");
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_dropped_channel_mid_call_is_transport_not_a_hang() {
        // The host stalls in the call, then goes away: once with the channel
        // closing in order, once with the whole connection dying under it.
        for kill_connection in [false, true] {
            let seen = Seen::default();
            let make = || FakeHost::new("1.0.0").reply("infer", Reply::after(Duration::from_millis(400), Reply::Hangup));
            let (srv, _d, exec) = served(serving(make, seen.clone())).await;
            let (client, _) = mcp(&exec, CancellationToken::new()).await;
            if kill_connection {
                // Every socket write fails from now on; the host's hangup is
                // the next write.
                srv.drop_connections();
            }
            let started = Instant::now();
            let got = tokio::time::timeout(Duration::from_secs(2), client.infer(args("P"), CancellationToken::new()))
                .await
                .unwrap_or_else(|_| panic!("kill_connection={kill_connection}: the call hung"));
            assert!(matches!(got, Err(McpError::Protocol(_))), "kill_connection={kill_connection}: {got:?}");
            assert!(started.elapsed() < Duration::from_secs(2));
            client.close().await;
        }
    }

    #[tokio::test]
    async fn cancel_closes_the_channel() {
        let (eof_tx, eof_rx) = oneshot::channel::<Instant>();
        let eof_tx = Arc::new(StdMutex::new(Some(eof_tx)));
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let started_tx = Arc::new(StdMutex::new(Some(started_tx)));
        let handler: ExecHandler = Arc::new(move |mut ctx: ExecCtx| {
            let (eof_tx, started_tx) = (eof_tx.clone(), started_tx.clone());
            Box::pin(async move {
                if let Some(t) = started_tx.lock().unwrap().take() {
                    let _ = t.send(());
                }
                let mut sink = Vec::new();
                let _ = ctx.stdin.read_to_end(&mut sink).await;
                if let Some(t) = eof_tx.lock().unwrap().take() {
                    let _ = t.send(Instant::now());
                }
                0
            })
        });
        let (_srv, _d, exec) = served(handler).await;
        let cancel = CancellationToken::new();
        // The client holds both pipes open: only the cancel can close it.
        let McpChannel { read: _read, write: _write, end } = open(&exec, cancel.clone()).await;
        started_rx.await.expect("the exec never reached the handler");
        let cancelled_at = Instant::now();
        cancel.cancel();
        let eof_at = tokio::time::timeout(Duration::from_secs(1), eof_rx)
            .await
            .expect("the handler's stdin stayed open after cancel")
            .unwrap();
        assert!(eof_at.duration_since(cancelled_at) < Duration::from_secs(1));
        let end = tokio::time::timeout(Duration::from_secs(1), end).await.expect("`end` never fired");
        assert!(end.is_ok());
    }

    #[tokio::test]
    async fn a_cancelled_call_tells_the_host_before_the_channel_closes() {
        // One token for the channel and the call, as the runner passes it.
        let seen = Seen::default();
        let make = || FakeHost::new("1.0.0").reply("infer", Reply::after(Duration::from_secs(10), Reply::text("late")));
        let (_srv, _d, exec) = served(serving(make, seen.clone())).await;
        let cancel = CancellationToken::new();
        let (client, _) = mcp(&exec, cancel.clone()).await;
        {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                cancel.cancel();
            });
        }
        let got = client.infer(args("P"), cancel).await;
        assert!(matches!(got, Err(McpError::Cancelled)), "{got:?}");
        let heard = seen.host().cancelled_within(Duration::from_secs(1)).await;
        assert_eq!(heard.expect("no notifications/cancelled reached the host").reason.as_deref(), Some("cancelled by Jodd"));
        client.close().await;
    }

    #[test]
    fn only_exit_64_with_unknown_request_is_legacy() {
        let end = |exit, stderr: &str| ChannelEnd { exit, stderr: stderr.into() };
        assert!(is_legacy(&end(Some(64), "unknown request\n")));
        assert!(!is_legacy(&end(Some(64), "no key id\n")));
        assert!(!is_legacy(&end(Some(1), "unknown request\n")));
        assert!(!is_legacy(&end(None, "unknown request\n")));
    }
}
