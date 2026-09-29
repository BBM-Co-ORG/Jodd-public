//! The embedded SSH client: Android has no `ssh` binary. One russh
//! connection per account, one exec channel per request, every request
//! framed by `framing` exactly as `ProcessSession` frames it. Design:
//! docs/superpowers/specs/2026-09-29-ssh-android-russh-design.md.

use russh::keys::{Algorithm, PublicKey};

use super::scripts;

/// Everything that can stop a russh connection, as data. `stderr_for`
/// turns it into OpenSSH's own wording so `classify` — the one place that
/// decides what a failure means to the user and to the auth backoff —
/// serves both transports from one table.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RusshFailure {
    Resolve { host: String, detail: String },
    Refused { host: String, port: u16 },
    TimedOut { host: String, port: u16 },
    /// `password_offered` is true when the server's remaining methods include
    /// `password` OR `keyboard-interactive`: on Android both ARE the password
    /// method, and `classify::password_auth_was_offered` looks for the word
    /// "password".
    AuthRejected { user: String, host: String, password_offered: bool },
    HostKeyMismatch,
    Other(String),
}

pub(crate) fn stderr_for(f: &RusshFailure) -> String {
    match f {
        RusshFailure::Resolve { host, detail } => format!("ssh: Could not resolve hostname {host}: {detail}\n"),
        RusshFailure::Refused { host, port } => format!("ssh: connect to host {host} port {port}: Connection refused\n"),
        RusshFailure::TimedOut { host, port } => format!("ssh: connect to host {host} port {port}: Connection timed out\n"),
        RusshFailure::AuthRejected { user, host, password_offered } => format!(
            "{user}@{host}: Permission denied ({}).\n",
            if *password_offered { "publickey,password" } else { "publickey" }
        ),
        RusshFailure::HostKeyMismatch => {
            "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\nHost key verification failed.\n".to_string()
        }
        RusshFailure::Other(m) => format!("ssh: {m}\n"),
    }
}

pub(crate) fn split_target(target: &str) -> Result<(String, String), String> {
    match target.rsplit_once('@') {
        Some((user, host)) if !user.is_empty() && !host.is_empty() => Ok((user.to_string(), host.to_string())),
        _ => Err(format!("an SSH target needs a username: user@host, not {target:?}")),
    }
}

/// The line #103's `ssh-keyscan` path would write: `host algo base64`,
/// `[host]:port …` for a non-22 port, no comment.
pub(crate) fn known_hosts_line(host: &str, port: Option<u16>, key: &PublicKey) -> String {
    let openssh = key.to_openssh().unwrap_or_default();
    let body = scripts::key_body(&openssh);
    match port {
        Some(p) if p != 22 => format!("[{host}]:{p} {body}"),
        _ => format!("{host} {body}"),
    }
}

/// B7: an account's known_hosts holds exactly the line the user trusted, so
/// the key alone decides. Any non-comment line whose `algo base64` equals the
/// offered key's is a match; an empty or comment-only file matches nothing.
pub(crate) fn pinned_key_matches(pinned_file: &str, offered: &PublicKey) -> bool {
    let Ok(openssh) = offered.to_openssh() else { return false };
    let want = scripts::key_body(&openssh);
    pinned_file
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(char::is_whitespace).map(|(_, rest)| scripts::key_body(rest.trim())))
        .any(|body| body == want)
}

pub(crate) fn algorithm_label(key: &PublicKey) -> String {
    match key.algorithm() {
        Algorithm::Ed25519 => "ED25519".into(),
        Algorithm::Ecdsa { .. } => "ECDSA".into(),
        Algorithm::Rsa { .. } => "RSA".into(),
        other => other.as_str().to_uppercase(),
    }
}

/// #103's askpass fail-safe, for keyboard-interactive: only a prompt that
/// asks for a password gets the password.
pub(crate) fn wants_password(prompt: &str) -> bool {
    prompt.to_lowercase().contains("password")
}

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, MethodKind};

use super::framing;
use super::session::{ExecOutput, SshError, SshOptions, SshSession, AUTH_BACKOFF};

/// ConnectTimeout=10's equivalent.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) enum HostCheck {
    /// Screen 1 → 2: record the offered key and refuse, so the handshake ends
    /// before any credential is sent to a server nobody has trusted yet.
    Capture(Arc<StdMutex<Option<PublicKey>>>),
    /// Every other connection: the account's known_hosts file contents.
    Pinned(String),
}

pub(crate) struct PinnedHost {
    check: HostCheck,
    refused: Arc<AtomicBool>,
}

impl client::Handler for PinnedHost {
    type Error = russh::Error;

    async fn check_server_key(&mut self, offered: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = offered else {
            self.refused.store(true, Ordering::SeqCst);
            return Ok(false);
        };
        match &self.check {
            HostCheck::Capture(slot) => {
                *slot.lock().unwrap() = Some(key.clone());
                Ok(false)
            }
            HostCheck::Pinned(file) => {
                let ok = pinned_key_matches(file, key);
                if !ok {
                    self.refused.store(true, Ordering::SeqCst);
                }
                Ok(ok)
            }
        }
    }
}

/// The host-key algorithms the client offers, in order: the pinned key's
/// algorithm first, then the rest. Negotiation picks the client's first
/// algorithm the server also has, so a fixed ed25519-first list would make a
/// server pinned by its ECDSA or RSA key — one that later gains an ed25519
/// key — present the ed25519 key, fail the pin, and look like an attack.
/// OpenSSH avoids this the same way: it prefers what known_hosts holds.
/// Capture (`None`) and an unparseable pin keep the default order.
pub(crate) fn preferred_host_key_algorithms(pinned_file: Option<&str>) -> Vec<Algorithm> {
    use russh::keys::{EcdsaCurve, HashAlg};
    let default = vec![
        Algorithm::Ed25519,
        Algorithm::Ecdsa { curve: EcdsaCurve::NistP256 },
        Algorithm::Ecdsa { curve: EcdsaCurve::NistP384 },
        Algorithm::Ecdsa { curve: EcdsaCurve::NistP521 },
        Algorithm::Rsa { hash: Some(HashAlg::Sha512) },
        Algorithm::Rsa { hash: Some(HashAlg::Sha256) },
    ];
    // Every pinned key's algorithm, in file order. An RSA key has one key
    // type but two signature algorithms here; both move up together.
    let pinned: Vec<Algorithm> = pinned_file
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(char::is_whitespace))
        .filter_map(|(_, rest)| PublicKey::from_openssh(scripts::key_body(rest.trim())).ok())
        .map(|k| k.algorithm())
        .collect();
    let same_family = |a: &Algorithm, b: &Algorithm| match (a, b) {
        (Algorithm::Rsa { .. }, Algorithm::Rsa { .. }) => true,
        _ => a == b,
    };
    let mut ordered: Vec<Algorithm> = Vec::with_capacity(default.len());
    for p in &pinned {
        for a in default.iter().filter(|a| same_family(a, p)) {
            if !ordered.contains(a) {
                ordered.push(a.clone());
            }
        }
    }
    ordered.extend(default.into_iter().filter(|a| !pinned.iter().any(|p| same_family(a, p))));
    ordered
}

fn client_config(pinned_file: Option<&str>) -> Arc<client::Config> {
    let mut c = client::Config::default();
    // ServerAliveInterval=15's equivalent: a socket that died while the phone
    // slept surfaces as an error instead of a hang.
    c.keepalive_interval = Some(Duration::from_secs(15));
    c.keepalive_max = 3;
    c.preferred.key = std::borrow::Cow::Owned(preferred_host_key_algorithms(pinned_file));
    Arc::new(c)
}

async fn open_socket(host: &str, port: u16) -> Result<tokio::net::TcpStream, RusshFailure> {
    let addrs: Vec<_> = match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::lookup_host((host, port))).await {
        Ok(Ok(a)) => a.collect(),
        Ok(Err(e)) => return Err(RusshFailure::Resolve { host: host.into(), detail: e.to_string() }),
        Err(_) => return Err(RusshFailure::TimedOut { host: host.into(), port }),
    };
    let Some(addr) = addrs.first() else {
        return Err(RusshFailure::Resolve { host: host.into(), detail: "no address".into() });
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(s)) => {
            let _ = s.set_nodelay(true);
            Ok(s)
        }
        // On macOS loopback, connecting to a port nothing is listening on
        // intermittently comes back as ECONNRESET instead of ECONNREFUSED —
        // both mean the same thing here (nobody accepted), so both classify
        // as Refused.
        Ok(Err(e)) if matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset) => {
            Err(RusshFailure::Refused { host: host.into(), port })
        }
        Ok(Err(e)) => Err(RusshFailure::Other(format!("connect to host {host} port {port}: {e}"))),
        Err(_) => Err(RusshFailure::TimedOut { host: host.into(), port }),
    }
}

async fn handshake(host: &str, port: u16, check: HostCheck) -> Result<client::Handle<PinnedHost>, RusshFailure> {
    let sock = open_socket(host, port).await?;
    let refused = Arc::new(AtomicBool::new(false));
    let config = client_config(match &check {
        HostCheck::Capture(_) => None,
        HostCheck::Pinned(file) => Some(file.as_str()),
    });
    let handler = PinnedHost { check, refused: refused.clone() };
    match tokio::time::timeout(CONNECT_TIMEOUT, client::connect_stream(config, sock, handler)).await {
        Ok(Ok(h)) => Ok(h),
        Ok(Err(_)) if refused.load(Ordering::SeqCst) => Err(RusshFailure::HostKeyMismatch),
        Ok(Err(e)) => Err(RusshFailure::Other(e.to_string())),
        Err(_) => Err(RusshFailure::TimedOut { host: host.into(), port }),
    }
}

pub(crate) async fn capture_host_key(host: &str, port: Option<u16>) -> Result<PublicKey, RusshFailure> {
    let slot = Arc::new(StdMutex::new(None));
    let port = port.unwrap_or(22);
    let result = handshake(host, port, HostCheck::Capture(slot.clone())).await;
    if let Some(k) = slot.lock().unwrap().take() {
        return Ok(k);
    }
    match result {
        Err(f) => Err(f),
        Ok(_) => Err(RusshFailure::Other("the server offered no host key".into())),
    }
}

pub(crate) enum Credential<'a> {
    Key(&'a Path),
    Password(&'a str),
}

fn offers_password(methods: &russh::MethodSet) -> bool {
    methods.contains(&MethodKind::Password) || methods.contains(&MethodKind::KeyboardInteractive)
}

pub(crate) async fn connect(
    target: &str,
    port: Option<u16>,
    pinned_file: String,
    cred: Credential<'_>,
) -> Result<client::Handle<PinnedHost>, RusshFailure> {
    let (user, host) = split_target(target).map_err(RusshFailure::Other)?;
    let mut h = handshake(&host, port.unwrap_or(22), HostCheck::Pinned(pinned_file)).await?;
    let rejected = |password_offered| RusshFailure::AuthRejected { user: user.clone(), host: host.clone(), password_offered };
    let other = |e: russh::Error| RusshFailure::Other(e.to_string());
    match cred {
        Credential::Key(path) => {
            let key = russh::keys::load_secret_key(path, None)
                .map_err(|e| RusshFailure::Other(format!("could not read Jodd's key: {e}")))?;
            match h.authenticate_publickey(&user, PrivateKeyWithHashAlg::new(Arc::new(key), None)).await.map_err(other)? {
                client::AuthResult::Success => Ok(h),
                client::AuthResult::Failure { remaining_methods, .. } => Err(rejected(offers_password(&remaining_methods))),
            }
        }
        Credential::Password(pw) => {
            let remaining = match h.authenticate_password(&user, pw).await.map_err(other)? {
                client::AuthResult::Success => return Ok(h),
                client::AuthResult::Failure { remaining_methods, .. } => remaining_methods,
            };
            if !remaining.contains(&MethodKind::KeyboardInteractive) {
                return Err(rejected(offers_password(&remaining)));
            }
            // Many hosts run `PasswordAuthentication no` with
            // `KbdInteractiveAuthentication yes`. Answer only prompts that ask
            // for a password (#103's askpass rule); anything else — a 2FA
            // code — aborts rather than leaking the password into it.
            let mut reply = h.authenticate_keyboard_interactive_start(&user, None).await.map_err(other)?;
            for _ in 0..5 {
                match reply {
                    client::KeyboardInteractiveAuthResponse::Success => return Ok(h),
                    client::KeyboardInteractiveAuthResponse::Failure { .. } => return Err(rejected(true)),
                    client::KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                        if !prompts.iter().all(|p| wants_password(&p.prompt)) {
                            return Err(RusshFailure::Other(
                                "the server asked for something other than a password (e.g. a verification code), which Jodd cannot answer".into(),
                            ));
                        }
                        let answers = prompts.iter().map(|_| pw.to_string()).collect();
                        reply = h.authenticate_keyboard_interactive_respond(answers).await.map_err(other)?;
                    }
                }
            }
            Err(rejected(true))
        }
    }
}

/// One framed request on its own channel. The whole stdout is collected
/// first — a reply spans many `Data` messages — then parsed by the same
/// `read_framed` `ProcessSession` uses.
pub(crate) async fn run_on(h: &client::Handle<PinnedHost>, script: &str) -> Result<ExecOutput, SshError> {
    let dead = |e: russh::Error| SshError::Disconnected { stderr: format!("ssh: {e}\n") };
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let mut ch = h.channel_open_session().await.map_err(dead)?;
    ch.exec(true, "sh").await.map_err(dead)?;
    ch.data(framing::frame(script, &nonce).as_bytes()).await.map_err(dead)?;
    ch.eof().await.map_err(dead)?;
    let mut out = Vec::new();
    while let Some(msg) = ch.wait().await {
        match msg {
            ChannelMsg::Data { data } => out.extend_from_slice(&data),
            ChannelMsg::Close => break,
            // The reply to `exec(true, …)`: the server will not run `sh`. It
            // may never close the channel, so waiting on would hang.
            ChannelMsg::Failure => {
                return Err(SshError::Disconnected { stderr: "ssh: the server refused to run a shell\n".into() });
            }
            _ => {}
        }
    }
    framing::read_framed(&mut &out[..], &nonce).await
}

/// The Android `SshSession`: one authenticated connection, made lazily,
/// never persisted — a process that Android kills just reconnects on the
/// next sync tick.
pub struct RusshSession {
    target: String,
    opts: SshOptions,
    timeout: Duration,
    conn: tokio::sync::Mutex<Option<client::Handle<PinnedHost>>>,
    /// The last auth-class failure; see `session::AUTH_BACKOFF`.
    auth_failed: StdMutex<Option<(Instant, String)>>,
}

impl RusshSession {
    pub fn new(target: String, opts: SshOptions, timeout: Duration) -> Self {
        RusshSession { target, opts, timeout, conn: tokio::sync::Mutex::new(None), auth_failed: StdMutex::new(None) }
    }

    async fn exec_on(&self, conn: &mut Option<client::Handle<PinnedHost>>, script: &str) -> Result<ExecOutput, SshError> {
        if conn.as_ref().map_or(true, |h| h.is_closed()) {
            let key = self.opts.key.as_deref().ok_or_else(|| SshError::Protocol("this account has no key".into()))?;
            let kh = self.opts.known_hosts.as_deref().ok_or_else(|| SshError::Protocol("this account has no known_hosts".into()))?;
            let pinned = std::fs::read_to_string(kh).map_err(|e| SshError::Protocol(format!("could not read known_hosts: {e}")))?;
            let h = connect(&self.target, self.opts.port, pinned, Credential::Key(key))
                .await
                .map_err(|f| SshError::Disconnected { stderr: stderr_for(&f) })?;
            *conn = Some(h);
        }
        run_on(conn.as_ref().expect("connected above"), script).await
    }
}

#[async_trait]
impl SshSession for RusshSession {
    async fn exec(&self, script: &str) -> Result<ExecOutput, SshError> {
        let mut conn = self.conn.lock().await;
        if let Some((at, stderr)) = self.auth_failed.lock().unwrap().as_ref() {
            if at.elapsed() < AUTH_BACKOFF {
                return Err(SshError::Disconnected { stderr: stderr.clone() });
            }
        }
        match tokio::time::timeout(self.timeout, self.exec_on(&mut conn, script)).await {
            Ok(Ok(out)) => Ok(out),
            Ok(Err(e)) => {
                conn.take();
                if let SshError::Disconnected { stderr } = &e {
                    if super::classify::auth_hint(stderr, "").is_some() {
                        *self.auth_failed.lock().unwrap() = Some((Instant::now(), stderr.clone()));
                    }
                }
                Err(e)
            }
            Err(_) => {
                conn.take();
                Err(SshError::Timeout)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ssh::classify;
    use russh::keys::ssh_key::private::Ed25519Keypair;
    use russh::keys::PrivateKey;

    fn key(seed: u8) -> russh::keys::PublicKey {
        PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32])).public_key().clone()
    }

    #[test]
    fn every_failure_reaches_the_setup_message_desktop_already_shows() {
        let host = "box".to_string();
        let cases = [
            (RusshFailure::Resolve { host: host.clone(), detail: "no such host".into() }, "Can't reach the server"),
            (RusshFailure::Refused { host: host.clone(), port: 22 }, "Can't reach the server"),
            (RusshFailure::TimedOut { host: host.clone(), port: 22 }, "Can't reach the server"),
            (RusshFailure::AuthRejected { user: "me".into(), host: host.clone(), password_offered: true }, "Wrong username or password"),
            (RusshFailure::AuthRejected { user: "me".into(), host: host.clone(), password_offered: false }, "only accepts key files"),
            (RusshFailure::HostKeyMismatch, "identity is different"),
        ];
        for (f, want) in cases {
            let msg = classify::from_setup_stderr(&stderr_for(&f));
            assert!(msg.contains(want), "{f:?} → {msg}");
        }
    }

    #[test]
    fn auth_and_host_key_failures_are_auth_class_for_the_backoff() {
        let rejected = RusshFailure::AuthRejected { user: "me".into(), host: "box".into(), password_offered: false };
        assert!(classify::auth_hint(&stderr_for(&rejected), "").is_some());
        assert!(classify::auth_hint(&stderr_for(&RusshFailure::HostKeyMismatch), "").is_some());
        assert!(classify::auth_hint(&stderr_for(&RusshFailure::Refused { host: "box".into(), port: 22 }), "").is_none());
    }

    #[test]
    fn the_known_hosts_line_matches_what_desktop_writes() {
        let k = key(1);
        let body = scripts::key_body(&k.to_openssh().unwrap()).to_string();
        for port in [None, Some(22), Some(2222)] {
            assert_eq!(
                known_hosts_line("box.example.com", port, &k),
                crate::backend::ssh::setup::known_hosts_line_for("box.example.com", port, &format!("box.example.com {body}")),
                "port {port:?}"
            );
        }
        assert!(known_hosts_line("box", Some(2222), &k).starts_with("[box]:2222 ssh-ed25519 "));
    }

    #[test]
    fn a_pinned_line_matches_only_its_own_key() {
        let line = known_hosts_line("box", None, &key(1));
        assert!(pinned_key_matches(&line, &key(1)));
        assert!(!pinned_key_matches(&line, &key(2)));
        assert!(!pinned_key_matches("", &key(1)), "no pin means no trust");
        assert!(!pinned_key_matches("# only a comment\n", &key(1)));
        // B7: the host field is not compared.
        assert!(pinned_key_matches(&known_hosts_line("10.0.2.2", Some(2222), &key(1)), &key(1)));
    }

    const ECDSA_P256: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBMcOBuD8UBqy1rhjUJqDRoxemvq+wv/5jtkCIoTNQqmYIqyHhFSBafG3og5oDvDs49yQBdByCT15Qhz4BsWV5p8=";
    const ECDSA_P384: &str = "ecdsa-sha2-nistp384 AAAAE2VjZHNhLXNoYTItbmlzdHAzODQAAAAIbmlzdHAzODQAAABhBNg5TJ7N6zbycmO+Ra4d89d6hg5odqaAjfktvFMWgkDPDSxO1Kr9MT8cJPQi2HYsO23uCbz6Wy9fVwqRsexFZiy+PlEZIXrCc5HpfcTL+Wc07HWRvYTJRskvkZePUWcVug==";
    const RSA: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQDmyUhZBOJieNK5NoMFHPW6JacoXLopSdNKtWRn+hm2IqIHwSbBjt292EZUeflSNlYxjUqtrnn2Ro11CHc6eRalp3hf3QAi2N4/KslRu9smKXA9nz9zoBiJCnFVO599s4KemFBHCQAHRivMCwyC2G3N9uERLZQ21rKcF4kHaIwPHfMLriyFGtHre4nCzRgKa9R+GQ9pIwmAVsUQIan0QowFkIJlULAAsL2qGCRaWiaLdpWdlGVRn9M7axRdF9+2Ow3D1R5G2vU2ZFSmaLnj4qkHuuMzXo2uJ2lm2LmdiXxCWwLvMJEVJw6g2uQLErnn9V6KADzF4YFmTg8qn0361oOV";

    fn same_set(a: &[Algorithm], b: &[Algorithm]) -> bool {
        a.len() == b.len() && a.iter().all(|x| b.contains(x))
    }

    #[test]
    fn capture_and_an_unusable_pin_keep_the_default_order() {
        use russh::keys::EcdsaCurve;
        let default = preferred_host_key_algorithms(None);
        assert_eq!(default[0], Algorithm::Ed25519);
        assert_eq!(default[1], Algorithm::Ecdsa { curve: EcdsaCurve::NistP256 });
        for junk in ["", "# only a comment\n", "box ssh-ed25519 not-base64!!", "box"] {
            assert_eq!(preferred_host_key_algorithms(Some(junk)), default, "{junk:?}");
        }
    }

    #[test]
    fn an_ed25519_pin_leads_with_ed25519() {
        let line = known_hosts_line("box", None, &key(1));
        assert_eq!(preferred_host_key_algorithms(Some(&line)), preferred_host_key_algorithms(None));
    }

    #[test]
    fn an_ecdsa_pin_leads_with_its_own_curve() {
        use russh::keys::EcdsaCurve;
        let default = preferred_host_key_algorithms(None);
        for (body, curve) in [(ECDSA_P256, EcdsaCurve::NistP256), (ECDSA_P384, EcdsaCurve::NistP384)] {
            let got = preferred_host_key_algorithms(Some(&format!("[box]:2222 {body}\n")));
            assert_eq!(got[0], Algorithm::Ecdsa { curve }, "{got:?}");
            assert!(same_set(&got, &default), "same algorithms, reordered, none duplicated: {got:?}");
        }
    }

    #[test]
    fn an_rsa_pin_leads_with_the_rsa_signatures() {
        use russh::keys::HashAlg;
        let got = preferred_host_key_algorithms(Some(&format!("box {RSA}\n")));
        assert_eq!(got[0], Algorithm::Rsa { hash: Some(HashAlg::Sha512) }, "{got:?}");
        assert_eq!(got[1], Algorithm::Rsa { hash: Some(HashAlg::Sha256) }, "{got:?}");
        assert!(same_set(&got, &preferred_host_key_algorithms(None)), "{got:?}");
    }

    #[test]
    fn targets_need_a_user() {
        assert_eq!(split_target("me@box").unwrap(), ("me".to_string(), "box".to_string()));
        assert!(split_target("box").is_err());
        assert!(split_target("@box").is_err());
    }

    #[test]
    fn only_password_prompts_get_the_password() {
        assert!(wants_password("Password: "));
        assert!(wants_password("me@box's password:"));
        assert!(!wants_password("Verification code: "));
        assert!(!wants_password("Are you sure you want to continue connecting (yes/no)?"));
    }

    #[test]
    fn ed25519_is_labelled_like_ssh_keygen() {
        assert_eq!(algorithm_label(&key(1)), "ED25519");
    }

    /// The body of the emulator test, also run on the host against the
    /// in-process server. Generation runs on the device; login uses a key the
    /// CI runner installed, since the runner cannot install a key that does
    /// not exist until the emulator runs.
    pub(crate) async fn loopback_round_trip(dir: &std::path::Path, host: &str) {
        let scratch = dir.join("generated");
        let _ = std::fs::remove_dir_all(&scratch);
        let made = crate::backend::ssh::russh_setup::generate_key(&scratch).unwrap();
        russh::keys::load_secret_key(scratch.join("id_ed25519"), None).unwrap();
        assert!(made.pubkey_line.starts_with("ssh-ed25519 "), "{}", made.pubkey_line);

        let user = std::fs::read_to_string(dir.join("user")).unwrap().trim().to_string();
        let port: u16 = std::fs::read_to_string(dir.join("port")).unwrap().trim().parse().unwrap();
        let s = RusshSession::new(
            format!("{user}@{host}"),
            crate::backend::ssh::session::SshOptions {
                port: Some(port),
                key: Some(dir.join("client_key")),
                known_hosts: Some(dir.join("known_hosts")),
            },
            std::time::Duration::from_secs(30),
        );
        let o = crate::backend::ssh::session::SshSession::exec(&s, "echo framed-over-russh").await.unwrap();
        assert_eq!((o.stdout.as_str(), o.exit), ("framed-over-russh\n", 0));
    }

    #[cfg(target_os = "android")]
    #[tokio::test]
    async fn android_round_trip_over_loopback() {
        let dir = std::path::PathBuf::from(std::env::var("JODD_TEST_DIR").expect("JODD_TEST_DIR must name the pushed fixture dir"));
        // 10.0.2.2 is the emulator's alias for the host's loopback.
        loopback_round_trip(&dir, "10.0.2.2").await;
    }

    #[cfg(unix)]
    mod live {
        use super::*;
        use crate::backend::ssh::session::{ExecOutput, SshError, SshOptions, SshSession};
        use crate::backend::ssh::test_server::{self, ServerOpts};
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        /// A server that trusts one freshly generated key, and a session holding it.
        async fn keyed(opts: ServerOpts, timeout: Duration) -> (test_server::TestServer, tempfile::TempDir, RusshSession) {
            keyed_on(test_server::start(opts).await, timeout)
        }

        fn keyed_on(srv: test_server::TestServer, timeout: Duration) -> (test_server::TestServer, tempfile::TempDir, RusshSession) {
            let dir = tempfile::tempdir().unwrap();
            let key = PrivateKey::from(Ed25519Keypair::from_seed(&rand::random::<[u8; 32]>()));
            std::fs::write(dir.path().join("id_ed25519"), key.to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap().as_bytes()).unwrap();
            std::fs::write(dir.path().join("known_hosts"), format!("{}\n", srv.host_key_line)).unwrap();
            srv.authorize(&key.public_key().to_openssh().unwrap());
            let s = RusshSession::new(srv.target(), SshOptions {
                port: Some(srv.port),
                key: Some(dir.path().join("id_ed25519")),
                known_hosts: Some(dir.path().join("known_hosts")),
            }, timeout);
            (srv, dir, s)
        }

        #[tokio::test]
        async fn stdout_stderr_and_exit_come_back() {
            let (_srv, _d, s) = keyed(ServerOpts::default(), Duration::from_secs(20)).await;
            assert_eq!(s.exec("echo hi").await.unwrap(), ExecOutput { stdout: "hi\n".into(), stderr: String::new(), exit: 0 });
            let o = s.exec("echo oops >&2; exit 7").await.unwrap();
            assert_eq!((o.stdout.as_str(), o.stderr.as_str(), o.exit), ("", "oops\n", 7));
        }

        #[tokio::test]
        async fn one_connection_serves_every_request() {
            let (srv, _d, s) = keyed(ServerOpts::default(), Duration::from_secs(20)).await;
            for _ in 0..3 {
                s.exec("true").await.unwrap();
            }
            assert_eq!(srv.connections.load(Ordering::SeqCst), 1);
        }

        #[tokio::test]
        async fn a_marker_lookalike_in_content_does_not_end_the_frame() {
            let (_srv, _d, s) = keyed(ServerOpts::default(), Duration::from_secs(20)).await;
            let o = s.exec("printf '%s\\n' '@@JODD_ERR_0000' '@@JODD_END_0000 0'; echo after").await.unwrap();
            assert!(o.stdout.ends_with("after\n"), "{:?}", o.stdout);
        }

        /// I-1 over the new transport: a login shell that prints before `sh` runs.
        #[tokio::test]
        async fn a_login_banner_is_not_part_of_any_response() {
            for prefix in ["echo Welcome to the box; exec ", "printf 'no newline banner'; exec "] {
                let (_srv, _d, s) = keyed(ServerOpts { shell_prefix: prefix, ..Default::default() }, Duration::from_secs(20)).await;
                assert_eq!(s.exec("echo hi").await.unwrap().stdout, "hi\n", "prefix {prefix:?}");
                assert_eq!(s.exec("echo again").await.unwrap().stdout, "again\n");
            }
        }

        #[tokio::test]
        async fn a_script_cannot_read_the_command_stream() {
            let (_srv, _d, s) = keyed(ServerOpts::default(), Duration::from_secs(20)).await;
            assert_eq!(s.exec("cat").await.unwrap().stdout, "");
            assert_eq!(s.exec("echo still").await.unwrap().stdout, "still\n");
        }

        /// Review Focus #4: a reply far larger than one SSH window.
        #[tokio::test]
        async fn a_multi_megabyte_reply_arrives_whole() {
            let (_srv, _d, s) = keyed(ServerOpts::default(), Duration::from_secs(60)).await;
            let o = s.exec("head -c 3000000 /dev/zero | tr '\\0' 'a'").await.unwrap();
            assert_eq!(o.stdout.len(), 3_000_000);
            assert!(o.stdout.bytes().all(|b| b == b'a'));
        }

        /// Review Focus #5: the worker and the UI on one session at once.
        #[tokio::test]
        async fn concurrent_requests_serialise_and_both_answer_correctly() {
            let (_srv, _d, s) = keyed(ServerOpts::default(), Duration::from_secs(20)).await;
            let (a, b) = tokio::join!(s.exec("sleep 0.2; echo a"), s.exec("echo b"));
            assert_eq!((a.unwrap().stdout, b.unwrap().stdout), ("a\n".to_string(), "b\n".to_string()));
        }

        #[tokio::test]
        async fn a_dropped_connection_is_reported_then_remade() {
            let (srv, _d, s) = keyed(ServerOpts::default(), Duration::from_secs(20)).await;
            s.exec("true").await.unwrap();
            srv.drop_connections();
            tokio::time::sleep(Duration::from_millis(100)).await;
            // The first request after the drop may fail or may already reconnect;
            // either way the next one must work, on a NEW connection.
            let _ = s.exec("true").await;
            assert_eq!(s.exec("echo back").await.unwrap().stdout, "back\n");
            assert_eq!(srv.connections.load(Ordering::SeqCst), 2);
        }

        #[tokio::test]
        async fn a_hung_script_times_out_and_the_next_request_recovers() {
            let (_srv, _d, s) = keyed(ServerOpts::default(), Duration::from_millis(500)).await;
            assert!(matches!(s.exec("sleep 5").await.unwrap_err(), SshError::Timeout));
            assert_eq!(s.exec("echo ok").await.unwrap().stdout, "ok\n");
        }

        /// Review item 4: a server that answers `exec` with a channel failure
        /// but never closes the channel must not hold the request until the
        /// timeout — it is an error the moment the refusal arrives.
        #[tokio::test]
        async fn a_refused_exec_fails_at_once_instead_of_waiting_for_the_timeout() {
            let (_srv, _d, s) = keyed(ServerOpts { refuse_exec: true, ..Default::default() }, Duration::from_secs(10)).await;
            let started = std::time::Instant::now();
            let err = s.exec("echo never").await.unwrap_err();
            assert!(started.elapsed() < Duration::from_secs(5), "took {:?}: {err:?}", started.elapsed());
            match err {
                SshError::Disconnected { stderr } => assert!(stderr.contains("refused to run a shell"), "{stderr}"),
                other => panic!("{other:?}"),
            }
        }

        /// I-8 over the new transport: a refused key is not retried per tick.
        #[tokio::test]
        async fn a_refused_key_is_held_instead_of_reconnecting() {
            let (srv, _d, s) = keyed(ServerOpts::default(), Duration::from_secs(20)).await;
            // The key file stays; the server just no longer lists it.
            std::fs::remove_file(srv.home.path().join(".ssh/authorized_keys")).unwrap();
            for _ in 0..2 {
                match s.exec("echo hi").await.unwrap_err() {
                    SshError::Disconnected { stderr } => assert!(stderr.contains("Permission denied"), "{stderr}"),
                    other => panic!("{other:?}"),
                }
            }
            assert_eq!(srv.connections.load(Ordering::SeqCst), 1, "held, not one login per request");
        }

        #[tokio::test]
        async fn a_changed_host_key_is_refused_and_held() {
            let (srv, d, s) = keyed(ServerOpts::default(), Duration::from_secs(20)).await;
            std::fs::write(d.path().join("known_hosts"), known_hosts_line("127.0.0.1", Some(srv.port), &key(9))).unwrap();
            for _ in 0..2 {
                match s.exec("true").await.unwrap_err() {
                    SshError::Disconnected { stderr } => assert!(stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED"), "{stderr}"),
                    other => panic!("{other:?}"),
                }
            }
            assert_eq!(srv.connections.load(Ordering::SeqCst), 1);
        }

        /// An ECDSA host key made by the real `ssh-keygen`, or `None` where
        /// it is not installed (then the caller skips, as `start_sshd` does).
        fn ssh_keygen_ecdsa(dir: &std::path::Path) -> Option<PrivateKey> {
            let path = dir.join("ecdsa_host_key");
            let out = std::process::Command::new("ssh-keygen")
                .args(["-q", "-t", "ecdsa", "-b", "256", "-N", "", "-f"])
                .arg(&path)
                .output()
                .ok()?;
            out.status.success().then(|| russh::keys::load_secret_key(&path, None).unwrap())
        }

        /// Review item 1: a server pinned by its ECDSA key that also holds an
        /// ed25519 key must present the pinned one, not the client's
        /// default favourite — or the pin fails and reads as an attack.
        #[tokio::test]
        async fn a_host_pinned_by_ecdsa_still_connects_once_it_also_has_ed25519() {
            let keys = tempfile::tempdir().unwrap();
            let Some(ecdsa) = ssh_keygen_ecdsa(keys.path()) else {
                eprintln!("ssh-keygen not available in this environment — skipping");
                return;
            };
            let srv = test_server::start_with_host_keys(ServerOpts::default(), vec![ecdsa]).await;
            let pinned = srv.extra_host_key_lines[0].clone();
            assert!(pinned.contains(" ecdsa-sha2-nistp256 "), "{pinned}");
            let (_srv, d, s) = keyed_on(srv, Duration::from_secs(20));
            std::fs::write(d.path().join("known_hosts"), format!("{pinned}\n")).unwrap();
            assert_eq!(s.exec("echo pinned-by-ecdsa").await.unwrap().stdout, "pinned-by-ecdsa\n");
        }

        #[tokio::test]
        async fn capture_reads_the_host_key_without_authenticating() {
            let srv = test_server::start(ServerOpts { password: Some("pw"), ..Default::default() }).await;
            let k = capture_host_key("127.0.0.1", Some(srv.port)).await.unwrap();
            assert_eq!(known_hosts_line("127.0.0.1", Some(srv.port), &k), srv.host_key_line);
        }

        #[tokio::test]
        async fn an_unreachable_port_is_refused_not_hung() {
            let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let err = capture_host_key("127.0.0.1", Some(port)).await.unwrap_err();
            assert!(matches!(err, RusshFailure::Refused { .. }), "{err:?}");
        }

        /// The shipped storage scripts, unchanged, over the new transport.
        #[tokio::test]
        async fn the_storage_scripts_behave_identically_over_russh() {
            use crate::backend::{SaveOp, TransportError, NoteStore};
            let (srv, _d, s) = keyed(ServerOpts::default(), Duration::from_secs(30)).await;
            let root = srv.home.path().canonicalize().unwrap().to_str().unwrap().to_string();
            let v = crate::backend::ssh::SshVertical::new(std::sync::Arc::new(s), srv.target(), root.clone(), "ssh:test".into());
            let op = SaveOp { title: "Plan", body_html: "<div>original</div>", existing_remote_id: None, existing_uuid: None, existing_created_date: None, label: "Notes", base_version: None };
            let saved = v.save_full(&op).await.unwrap();
            let (notes, _) = v.list_all_notes(&Default::default()).await.unwrap();
            assert!(notes.iter().any(|n| n.uuid == saved.uuid));
            let path = srv.home.path().join(&saved.id);
            let edited = std::fs::read_to_string(&path).unwrap().replace("original", "edited on the server");
            std::fs::write(&path, &edited).unwrap();
            let again = SaveOp { body_html: "<div>mine</div>", existing_remote_id: Some(&saved.id), existing_uuid: Some(&saved.uuid), base_version: Some(&saved.version), ..op };
            assert!(matches!(v.save_full(&again).await.unwrap_err(), TransportError::Conflict { .. }));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
        }

        /// Against real OpenSSH, where it is installed (CI installs openssh-server).
        #[tokio::test]
        async fn exec_works_against_openssh_sshd() {
            let d = tempfile::tempdir().unwrap();
            let Some(sshd) = crate::backend::ssh::test_sshd::start_sshd(d.path()) else {
                eprintln!("sshd not available in this environment — skipping");
                return;
            };
            let key = PrivateKey::from(Ed25519Keypair::from_seed(&rand::random::<[u8; 32]>()));
            std::fs::write(d.path().join("id"), key.to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap().as_bytes()).unwrap();
            std::fs::write(&sshd.authorized_keys, format!("{}\n", key.public_key().to_openssh().unwrap())).unwrap();
            std::fs::write(d.path().join("kh"), format!("{}\n", sshd.host_key_line)).unwrap();
            let user = std::env::var("USER").unwrap_or_else(|_| "test".into());
            let s = RusshSession::new(format!("{user}@127.0.0.1"), SshOptions {
                port: Some(sshd.port), key: Some(d.path().join("id")), known_hosts: Some(d.path().join("kh")),
            }, Duration::from_secs(20));
            assert_eq!(s.exec("echo over-openssh").await.unwrap().stdout, "over-openssh\n");
        }

        #[tokio::test]
        async fn the_emulator_test_body_passes_against_the_in_process_server() {
            let srv = test_server::start(ServerOpts::default()).await;
            let d = tempfile::tempdir().unwrap();
            let client = crate::backend::ssh::russh_setup::generate_key(&d.path().join("client")).unwrap();
            std::fs::copy(d.path().join("client/id_ed25519"), d.path().join("client_key")).unwrap();
            srv.authorize(&client.pubkey_line);
            std::fs::write(d.path().join("known_hosts"), format!("{}\n", srv.host_key_line)).unwrap();
            std::fs::write(d.path().join("user"), "jodd\n").unwrap();
            std::fs::write(d.path().join("port"), format!("{}\n", srv.port)).unwrap();
            super::loopback_round_trip(d.path(), "127.0.0.1").await;
        }
    }
}
