//! Vertical #5 — SSH. Notes are Markdown (or HTML) files with YAML
//! frontmatter under a directory on a server the user owns, read and written
//! through one persistent remote `sh`. Design:
//! docs/superpowers/specs/2026-09-26-ssh-storage-backend-design.md (read its
//! "Amendments from planning" — they override the body).

pub mod askpass;
pub mod classify;
pub mod file;
pub mod framing;
pub mod frontmatter;
pub mod mdconv;
pub mod russh_session;
pub mod russh_setup;
pub mod scripts;
pub mod session;
pub mod setup;
mod read;
#[cfg(all(test, unix))]
pub(crate) mod test_server;
#[cfg(all(test, unix))]
pub(crate) mod test_sshd;
#[cfg(all(test, unix))]
mod tests;
mod transport;
mod write;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use super::{Capabilities, ContentKind, Derived, Deriver, Identity, TransportError};
use scripts::Flavor;
use session::{ProcessSession, SpawnSpec, SshSession};

pub const ATTACHMENTS_REFUSAL: &str = "attachments are not supported on SSH accounts yet";

pub struct SshVertical {
    pub(crate) root: String,
    pub(crate) target: String,
    #[allow(dead_code)]
    pub(crate) account_id: String,
    session: Arc<dyn SshSession>,
    flavor: tokio::sync::OnceCell<Flavor>,
    capabilities: Capabilities,
    managed: bool,
}

impl SshVertical {
    pub fn new(session: Arc<dyn SshSession>, target: String, root: String, account_id: String) -> Self {
        SshVertical {
            root,
            target,
            account_id,
            session,
            flavor: tokio::sync::OnceCell::new(),
            capabilities: Capabilities::for_backend(crate::accounts::BackendKind::Ssh),
            managed: false,
        }
    }

    /// A Managed account (Jodd holds the key): errors name Jodd's remedy,
    /// never `ssh-add`. See `classify::managed_auth_hint`.
    pub fn managed(mut self, yes: bool) -> Self {
        self.managed = yes;
        self
    }

    /// Run one script; a non-zero exit becomes the classified error.
    pub(crate) async fn exec(&self, script: &str) -> Result<String, TransportError> {
        let out = self
            .session
            .exec(script)
            .await
            .map_err(|e| classify::from_session_error(&e, &self.target, self.managed))?;
        classify::from_exit(&out)?;
        Ok(out.stdout)
    }

    /// Probed once per vertical instance — one short round trip on the
    /// already-open session.
    pub(crate) async fn flavor(&self) -> Result<Flavor, TransportError> {
        self.flavor
            .get_or_try_init(|| async {
                let out = self.exec(scripts::PROBE).await?;
                scripts::parse_probe(&out).map_err(|m| TransportError::Transient { source: anyhow::anyhow!(m) })
            })
            .await
            .copied()
    }
}

impl Identity for SshVertical {
    fn mint(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

impl Deriver for SshVertical {
    fn derive(&self, kind: ContentKind, blob: &[u8]) -> Derived {
        crate::backend::deriver_applehtml::AppleHtmlDeriver.derive(kind, blob)
    }
}

// ── Session registry ─────────────────────────────────────────────────────
//
// `vertical_for` builds a fresh vertical on every call; the ssh process must
// outlive it, or every 5-second tick would pay a handshake. Keyed by account,
// and replaced when the target changes.

/// True where the embedded russh client is the transport: Android, which has
/// no `ssh` binary. Desktop keeps system ssh (`ProcessSession`).
pub const EMBEDDED: bool = cfg!(target_os = "android");

type Registry = StdMutex<HashMap<String, (String, session::SshOptions, bool, Arc<dyn SshSession>)>>;
static SESSIONS: OnceLock<Registry> = OnceLock::new();

pub fn session_for(account_id: &str, target: &str, opts: &session::SshOptions) -> Arc<dyn SshSession> {
    session_for_with(account_id, target, opts, EMBEDDED)
}

/// `embedded` is `EMBEDDED` in production; tests pass `true` to run the
/// Android transport on a desktop CI host.
pub fn session_for_with(account_id: &str, target: &str, opts: &session::SshOptions, embedded: bool) -> Arc<dyn SshSession> {
    let mut map = SESSIONS.get_or_init(Default::default).lock().unwrap();
    if let Some((t, o, e, s)) = map.get(account_id) {
        if t == target && o == opts && *e == embedded {
            return s.clone();
        }
    }
    let s: Arc<dyn SshSession> = if embedded {
        Arc::new(russh_session::RusshSession::new(target.to_string(), opts.clone(), session::EXEC_TIMEOUT))
    } else {
        Arc::new(ProcessSession::new(SpawnSpec::ssh(target, opts), session::EXEC_TIMEOUT))
    };
    map.insert(account_id.to_string(), (target.to_string(), opts.clone(), embedded, s.clone()));
    s
}

/// Advanced accounts and the "browse a server before adding it" flow build
/// `SshOptions::default()`; a Managed account's options come from
/// `managed_options`.
pub fn managed_options(account: &crate::accounts::Account) -> session::SshOptions {
    let Some(key_dir) = account.ssh_key.as_deref() else {
        return session::SshOptions::default();
    };
    let base = crate::paths::data_base().map(|d| d.join("jodd").join("ssh").join(key_dir));
    session::SshOptions {
        port: account.ssh_port,
        key: base.as_ref().map(|b| b.join("id_ed25519")),
        known_hosts: base.as_ref().map(|b| b.join("known_hosts")),
    }
}

/// Whether Jodd holds this account's key (Managed) rather than the user's
/// own ssh-agent (Advanced). Decides which remedy an auth error names —
/// see `SshVertical::managed`.
pub fn is_managed(account: &crate::accounts::Account) -> bool {
    account.ssh_key.is_some()
}

/// Dropping the last `Arc` kills the ssh child (`kill_on_drop`) or closes the
/// russh connection.
pub fn forget_session(account_id: &str) {
    if let Some(m) = SESSIONS.get() {
        m.lock().unwrap().remove(account_id);
    }
}

// ── Pure helpers ─────────────────────────────────────────────────────────

/// `Notes/Work/a.md` → `Notes/Work`. Labels are root-relative directory
/// paths, the LocalFs convention.
pub(crate) fn label_of(path: &str) -> String {
    parent_dir(path)
}

pub(crate) fn parent_dir(path: &str) -> String {
    path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_else(|| "Notes".into())
}

pub(crate) fn dir_of_label(label: &str) -> String {
    let l = label.trim_matches('/');
    if l == "Notes" || l.starts_with("Notes/") { l.to_string() } else { format!("Notes/{l}") }
}

/// The most a slug may take, in BYTES. What a 60-character Thai slug (3
/// bytes each) already took, so no name gets longer than one this backend
/// could always write — the ceiling that matters, because a trashed note's
/// name embeds its whole path (`trash_name`) under the 255-byte limit.
/// Counting characters instead gave English titles a third of Thai's room.
const SLUG_MAX_BYTES: usize = 180;

/// File name for a new note: letters and digits of any script kept, runs
/// of anything else become one `-`, at most `SLUG_MAX_BYTES`. A name that
/// must be cut is cut at a word boundary, never inside a word or a number
/// ("…2026-09-2" read as a typo); one with no boundary to cut at (a long
/// Thai run) is cut at a character boundary.
pub(crate) fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in title.chars() {
        if c.is_alphanumeric() || matches!(c, '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}') {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.extend(c.to_lowercase());
        } else {
            dash = true;
        }
    }
    if out.len() > SLUG_MAX_BYTES {
        let mut cut = SLUG_MAX_BYTES;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        // Mid-word unless the next character starts a new word.
        if out.as_bytes().get(cut) != Some(&b'-') {
            if let Some(dash) = out[..cut].rfind('-') {
                cut = dash;
            }
        }
        out.truncate(cut);
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() { "untitled".into() } else { s }
}

/// A trashed file or folder is named `<32-hex nonce>-<trash_encode(path)>`:
/// unique by construction, so trashing a second note that once lived at the
/// same path can never `mv` over the first.
pub(crate) fn trash_name(path: &str) -> String {
    format!("{}-{}", uuid::Uuid::new_v4().simple(), crate::backend::localfs::transport::trash_encode(path))
}

/// The original path of a `trash_name`. A name without the nonce prefix
/// (nothing Jodd writes, but a person can `mv` a file in by hand) decodes
/// whole.
pub(crate) fn trash_original(name: &str) -> String {
    let b = name.as_bytes();
    let prefixed = b.len() > 33 && b[32] == b'-' && b[..32].iter().all(u8::is_ascii_hexdigit);
    crate::backend::localfs::transport::trash_decode(if prefixed { &name[33..] } else { name })
}

pub(crate) fn canon_uuid(raw: &str) -> Option<String> {
    uuid::Uuid::parse_str(raw.trim()).ok().map(|u| u.hyphenated().to_string())
}

pub(crate) fn apple_date_from_unix(secs: i64) -> String {
    use chrono::TimeZone;
    let dt = chrono::Local.timestamp_opt(secs, 0).single().unwrap_or_else(chrono::Local::now);
    crate::mime822::format_apple_date(dt)
}

pub(crate) fn apple_now() -> String {
    crate::mime822::format_apple_date(chrono::Local::now())
}

pub(crate) fn iso_from_unix(secs: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(secs, 0).single().unwrap_or_else(chrono::Local::now).to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

pub(crate) fn iso_now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Frontmatter `created` (ISO 8601) → the `x_mail_created_date` shape.
pub(crate) fn iso_to_apple(iso: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(iso.trim()).ok().map(|d| {
        d.format("%a, %-d %b %Y %H:%M:%S %z").to_string()
    })
}

pub(crate) fn apple_to_iso(apple: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc2822(apple.trim())
        .ok()
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

#[cfg(test)]
mod managed_tests {
    fn account(ssh_key: Option<&str>) -> crate::accounts::Account {
        crate::accounts::Account {
            id: "ssh:me@box".into(),
            email: "me@box".into(),
            added_at: "2026-01-01T00:00:00Z".into(),
            notes_label: None,
            meta_label: None,
            llm: Default::default(),
            backend_kind: crate::accounts::BackendKind::Ssh,
            root_dir: Some("/srv/notes".into()),
            ssh_target: Some("me@box".into()),
            ssh_port: None,
            ssh_key: ssh_key.map(str::to_string),
            icloud_session_established: false,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status: Default::default(),
            pending_removal: false,
        }
    }

    /// A Managed account is one Jodd holds a key for; an Advanced one uses
    /// the user's own ssh-agent and must keep the `ssh-add` advice.
    #[test]
    fn managed_means_jodd_holds_the_key() {
        assert!(super::is_managed(&account(Some("abcdef12-3456-7890-abcd-ef1234567890"))));
        assert!(!super::is_managed(&account(None)));
    }
}
