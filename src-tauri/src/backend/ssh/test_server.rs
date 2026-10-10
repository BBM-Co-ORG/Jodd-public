//! An in-process SSH server on russh's server side. It checks passwords and
//! keyboard-interactive (OpenSSH's sshd needs root to check a password),
//! accepts any key listed in `$HOME/.ssh/authorized_keys` of a tempdir home,
//! and runs every exec request as `sh -c <shell_prefix><command>` in that
//! home. Test-only.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};
use std::time::Duration;

use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{self, Auth, Msg, Response, Session};
use russh::{Channel, ChannelId, ChannelMsg, MethodKind, MethodSet};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// `server::run_stream` spawns its OWN task internally (`session.run`) that
/// owns the socket — aborting the outer per-connection task we spawn below
/// never touches that inner task or its socket, so a naive `AbortHandle`
/// does not actually drop a live connection. Wrapping the accepted socket in
/// this makes every read/write fail once `dead` is set, so the inner task's
/// own I/O errors out and it ends itself — which is what "the connection
/// dropped, as after a phone sleeps" needs to look like from the client side.
struct Killable<S> {
    inner: S,
    dead: Arc<AtomicBool>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Killable<S> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.dead.load(Ordering::SeqCst) {
            return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "test: connection dropped")));
        }
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Killable<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        if self.dead.load(Ordering::SeqCst) {
            return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "test: connection dropped")));
        }
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ServerOpts {
    pub password: Option<&'static str>,
    /// R1: whether the plain `password` auth method is offered at all.
    /// `false` with `keyboard_interactive: true` forces a client through
    /// keyboard-interactive — otherwise a server that would go on to test a
    /// keyboard-interactive-only account never gets there, because plain
    /// password auth (offered whenever `password.is_some()`) succeeds first.
    pub password_auth: bool,
    pub keyboard_interactive: bool,
    /// The one keyboard-interactive prompt the server asks.
    pub kbd_prompt: &'static str,
    /// Prepended to every exec command — `"printf banner; exec "` turns
    /// `sh` into a login shell that prints a banner first (I-1).
    pub shell_prefix: &'static str,
    /// Answer every exec request with SSH_MSG_CHANNEL_FAILURE and leave the
    /// channel open — a server that will not run a shell but never hangs up.
    pub refuse_exec: bool,
}

impl Default for ServerOpts {
    fn default() -> Self {
        ServerOpts {
            password: None,
            password_auth: true,
            keyboard_interactive: false,
            kbd_prompt: "Password: ",
            shell_prefix: "",
            refuse_exec: false,
        }
    }
}

pub(crate) struct TestServer {
    pub port: u16,
    pub host_key_line: String,
    /// One known_hosts line per `start_with_host_keys` extra key, in order.
    pub extra_host_key_lines: Vec<String>,
    pub home: tempfile::TempDir,
    pub connections: Arc<AtomicUsize>,
    live: Arc<StdMutex<Vec<Arc<AtomicBool>>>>,
    accept: tokio::task::JoinHandle<()>,
}

impl TestServer {
    pub fn target(&self) -> String {
        "jodd@127.0.0.1".to_string()
    }

    /// Adds a line to the home's `~/.ssh/authorized_keys`.
    pub fn authorize(&self, pubkey_line: &str) {
        let dir = self.home.path().join(".ssh");
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("authorized_keys")).unwrap();
        use std::io::Write;
        writeln!(f, "{}", pubkey_line.trim()).unwrap();
    }

    /// Kills every open connection — a dead socket, as after a phone sleeps.
    pub fn drop_connections(&self) {
        for d in self.live.lock().unwrap().drain(..) {
            d.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.accept.abort();
        self.drop_connections();
    }
}

pub(crate) async fn start(opts: ServerOpts) -> TestServer {
    start_with_host_keys(opts, Vec::new()).await
}

/// `start`, with host keys offered besides the ed25519 one — a server that
/// holds several, as most real sshd installs do.
pub(crate) async fn start_with_host_keys(opts: ServerOpts, extra_host_keys: Vec<PrivateKey>) -> TestServer {
    let home = tempfile::tempdir().unwrap();
    let host_key = PrivateKey::from(Ed25519Keypair::from_seed(&rand::random::<[u8; 32]>()));
    let mut methods = vec![MethodKind::PublicKey];
    // R1: plain password auth is only offered when both a password is set
    // AND password_auth is true — otherwise a keyboard-interactive-only
    // server must not advertise (or accept) the plain password method.
    if opts.password.is_some() && opts.password_auth {
        methods.push(MethodKind::Password);
    }
    if opts.keyboard_interactive {
        methods.push(MethodKind::KeyboardInteractive);
    }
    let config = Arc::new(server::Config {
        keys: std::iter::once(host_key.clone()).chain(extra_host_keys.iter().cloned()).collect(),
        methods: MethodSet::from(&methods[..]),
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    let live: Arc<StdMutex<Vec<Arc<AtomicBool>>>> = Default::default();
    let home_path = home.path().to_path_buf();
    let accept = {
        let (connections, live) = (connections.clone(), live.clone());
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { return };
                connections.fetch_add(1, Ordering::SeqCst);
                let dead = Arc::new(AtomicBool::new(false));
                live.lock().unwrap().push(dead.clone());
                let killable = Killable { inner: sock, dead };
                let conn = Conn { home: home_path.clone(), opts, channels: HashMap::new() };
                let config = config.clone();
                tokio::spawn(async move {
                    if let Ok(running) = server::run_stream(config, killable, conn).await {
                        let _ = running.await;
                    }
                });
            }
        })
    };
    let host_key_line = super::russh_session::known_hosts_line("127.0.0.1", Some(port), host_key.public_key());
    let extra_host_key_lines = extra_host_keys
        .iter()
        .map(|k| super::russh_session::known_hosts_line("127.0.0.1", Some(port), k.public_key()))
        .collect();
    TestServer { port, host_key_line, extra_host_key_lines, home, connections, live, accept }
}

struct Conn {
    home: PathBuf,
    opts: ServerOpts,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl Conn {
    /// The same method set `start()` builds from `self.opts` — reconstructed
    /// here so a rejected attempt can hand it back as `proceed_with_methods`
    /// and keep the method listed as retriable. Real sshd keeps "password"
    /// in the continuing-methods list across repeated wrong guesses (up to
    /// MaxAuthTries); `Auth::reject()`'s default (`proceed_with_methods:
    /// None`) makes russh's own server drop the just-tried method after a
    /// single failure instead, which would misreport a wrong password as a
    /// key-only server.
    fn offered_methods(&self) -> MethodSet {
        let mut methods = vec![MethodKind::PublicKey];
        if self.opts.password.is_some() && self.opts.password_auth {
            methods.push(MethodKind::Password);
        }
        if self.opts.keyboard_interactive {
            methods.push(MethodKind::KeyboardInteractive);
        }
        MethodSet::from(&methods[..])
    }
}

impl server::Handler for Conn {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        if !self.opts.password_auth {
            return Ok(Auth::reject());
        }
        Ok(if self.opts.password == Some(password) {
            Auth::Accept
        } else {
            Auth::Reject { proceed_with_methods: Some(self.offered_methods()), partial_success: false }
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        if !self.opts.keyboard_interactive {
            return Ok(Auth::reject());
        }
        let Some(mut r) = response else {
            return Ok(Auth::Partial {
                name: Cow::Borrowed(""),
                instructions: Cow::Borrowed(""),
                prompts: Cow::Owned(vec![(Cow::Borrowed(self.opts.kbd_prompt), false)]),
            });
        };
        let answer = r.next();
        Ok(if answer.as_deref() == self.opts.password.map(str::as_bytes) { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let offered = key.to_openssh().unwrap_or_default();
        let listed = std::fs::read_to_string(self.home.join(".ssh/authorized_keys")).unwrap_or_default();
        let ok = listed.lines().any(|l| {
            let l = l.trim();
            !l.is_empty() && super::scripts::key_body(l) == super::scripts::key_body(&offered)
        });
        Ok(if ok { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(&mut self, id: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if self.opts.refuse_exec {
            // The channel stays in `channels`, so it is neither closed nor dropped.
            session.channel_failure(id)?;
            return Ok(());
        }
        let Some(channel) = self.channels.remove(&id) else { return Ok(()) };
        session.channel_success(id)?;
        let cmd = format!("{}{}", self.opts.shell_prefix, String::from_utf8_lossy(data));
        tokio::spawn(run_command(channel, cmd, self.home.clone()));
        Ok(())
    }
}

async fn run_command(channel: Channel<Msg>, cmd: String, home: PathBuf) {
    let (mut rd, wr) = channel.split();
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(&home)
        .env("HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let feeder = tokio::spawn(async move {
        while let Some(msg) = rd.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    if let Some(s) = stdin.as_mut() {
                        let _ = s.write_all(&data).await;
                    }
                }
                ChannelMsg::Eof => {
                    stdin.take();
                }
                _ => {}
            }
        }
    });
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let _ = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
    let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(255);
    let _ = wr.data(&out[..]).await;
    let _ = wr.extended_data(1, &err[..]).await;
    let _ = wr.exit_status(code as u32).await;
    let _ = wr.eof().await;
    let _ = wr.close().await;
    feeder.abort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::client;

    struct AnyHost;
    impl client::Handler for AnyHost {
        type Error = russh::Error;
        async fn check_server_key(&mut self, _: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }

    /// The harness itself: password login, then an exec whose stdin, stdout,
    /// stderr and exit code all cross the channel.
    #[tokio::test]
    async fn runs_sh_over_a_password_login() {
        let srv = start(ServerOpts { password: Some("pw"), ..Default::default() }).await;
        let mut h = client::connect(Default::default(), ("127.0.0.1", srv.port), AnyHost).await.unwrap();
        assert!(h.authenticate_password("jodd", "pw").await.unwrap().success());
        let mut ch = h.channel_open_session().await.unwrap();
        ch.exec(true, "sh").await.unwrap();
        ch.data(&b"echo out; echo err >&2; exit 3\n"[..]).await.unwrap();
        ch.eof().await.unwrap();
        let (mut out, mut err, mut code) = (Vec::new(), Vec::new(), None);
        while let Some(m) = ch.wait().await {
            match m {
                russh::ChannelMsg::Data { data } => out.extend_from_slice(&data),
                russh::ChannelMsg::ExtendedData { data, .. } => err.extend_from_slice(&data),
                russh::ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                russh::ChannelMsg::Close => break,
                _ => {}
            }
        }
        assert_eq!((out.as_slice(), err.as_slice(), code), (&b"out\n"[..], &b"err\n"[..], Some(3)));
        assert_eq!(srv.connections.load(Ordering::SeqCst), 1);
    }

    /// R1: with `password_auth: false` and `keyboard_interactive: true`,
    /// plain password auth must fail so a client is forced through
    /// keyboard-interactive — otherwise a later task's
    /// keyboard-interactive-only server test would never exercise that path,
    /// because plain password auth (offered whenever `password.is_some()`)
    /// would succeed first.
    #[tokio::test]
    async fn password_auth_false_forces_keyboard_interactive() {
        let srv = start(ServerOpts {
            password: Some("pw"),
            password_auth: false,
            keyboard_interactive: true,
            ..Default::default()
        })
        .await;
        let mut h = client::connect(Default::default(), ("127.0.0.1", srv.port), AnyHost).await.unwrap();
        assert!(!h.authenticate_password("jodd", "pw").await.unwrap().success());

        let resp = h.authenticate_keyboard_interactive_start("jodd", None).await.unwrap();
        let prompts = match resp {
            client::KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => prompts,
            other => panic!("expected InfoRequest, got {other:?}"),
        };
        assert_eq!(prompts.len(), 1);

        let resp = h.authenticate_keyboard_interactive_respond(vec!["pw".into()]).await.unwrap();
        assert!(matches!(resp, client::KeyboardInteractiveAuthResponse::Success), "{resp:?}");
    }
}
