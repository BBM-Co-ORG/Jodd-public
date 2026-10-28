//! Shared test fixtures for `ask::*` modules and `db.rs`'s own tests, plus
//! `refused_port` for any test that needs a connection to be refused.
//!
//! Promoted out of `db.rs`'s `tests_ask_queries` module (Task 4 built it
//! there; this move is a plan-defect repair, not new scope — see Task 5
//! brief Step 0). `test_support` is `#[cfg(test)]`-only (see `lib.rs`), so
//! this file never ships in a release build.

use crate::db::{CachedNote, Db, SyncState};

/// Slice a named function's body out of a source string, by finding its
/// signature and the first `\n}\n` after it. There is no unit-test harness
/// for `State`-bound Tauri command functions, so several tests pin an
/// invariant by reading the function's own source instead of invoking it —
/// this is the one shared helper for that, promoted out of
/// `extract_mint_tests`'s local `body_of` (lib.rs) so a second copy doesn't
/// grow elsewhere.
pub fn extract_fn_body(src: &str, signature: &str) -> String {
    let start = src.find(signature).unwrap_or_else(|| panic!("`{signature}` must exist"));
    let end = start + src[start..].find("\n}\n").expect("function body is closed");
    src[start..end].to_string()
}

pub fn temp_db() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    std::mem::forget(dir);
    Db::open_unencrypted(&path).expect("open temp db")
}

/// Chainable fixture builder for a minimal note. `.modified_ms()` and
/// `.date()` are DELIBERATELY separate setters: `date` is the raw RFC822
/// header string, `modified_ms` drives `last_remote_modified_at` (the
/// epoch value recency ordering must use). Keeping them independent lets
/// one test make the lexical order of `date` contradict the true
/// chronological order, which is the regression guard for spec F4 (a
/// query that orders by `date` instead of the epoch column fails that
/// test).
pub struct NoteBuilder {
    n: CachedNote,
}

pub fn note(account_id: &str, uuid: &str) -> NoteBuilder {
    let default_ms = crate::db::now_ms();
    NoteBuilder {
        n: CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: format!("msg-{uuid}"),
            title: uuid.to_string(),
            body_html: format!("<div>{uuid}</div><div>body of {uuid}</div>"),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 1,
            remote_version: None,
            sync_state: SyncState::Clean,
            last_synced_at: Some(default_ms),
            last_local_modified_at: default_ms,
            last_remote_modified_at: Some(default_ms),
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            push_blocked_reason: None,
            push_blocked_by_remote: false,
        },
    }
}

impl NoteBuilder {
    pub fn label(mut self, label: &str) -> Self {
        self.n.label = label.to_string();
        self
    }

    /// The remote id. LocalFs uses the note's file path here.
    pub fn id(mut self, id: &str) -> Self {
        self.n.id = id.to_string();
        self
    }

    pub fn title(mut self, t: &str) -> Self {
        let body = self
            .n
            .body_html
            .rsplit("<div>")
            .next()
            .unwrap_or("")
            .trim_end_matches("</div>")
            .to_string();
        self.n.title = t.into();
        self.n.body_html = format!("<div>{}</div><div>{}</div>", t, body);
        self
    }

    /// Replace the body while preserving the Apple title-in-body convention
    /// (`<div>{title}</div><div>{body}</div>`), which db::strip_html_to_text
    /// and the title-stripping helpers both assume.
    pub fn body(mut self, b: &str) -> Self {
        self.n.body_html = format!("<div>{}</div><div>{}</div>", self.n.title, b);
        self
    }

    /// Sets last_local_modified_at / last_remote_modified_at / last_synced_at
    /// together — the epoch fields recency ordering reads. Independent of
    /// `.date()` on purpose (see struct doc comment).
    pub fn modified_ms(mut self, ms: i64) -> Self {
        self.n.last_local_modified_at = ms;
        self.n.last_remote_modified_at = Some(ms);
        self.n.last_synced_at = Some(ms);
        self
    }

    pub fn date(mut self, date: &str) -> Self {
        self.n.date = date.to_string();
        self
    }

    /// The finished row, for tests that hand it to `insert_local_new` themselves
    /// (to choose the actor) or that only need the value.
    pub fn build(self) -> CachedNote {
        self.n
    }

    pub fn insert(self, db: &Db) {
        db.insert_local_new(&self.n, &crate::provenance::Actor::test())
            .expect("insert fixture note");
    }
}

/// A loopback port nothing listens on, for as long as this value lives.
pub struct RefusedPort {
    pub port: u16,
    _held: (std::net::TcpStream, std::net::TcpStream),
}

/// A port that refuses connections at once and that no parallel test can
/// claim while the guard is alive.
///
/// The obvious "bind `:0`, read the port, drop the listener" frees the port
/// the moment it is read, and under `cargo test` another test's listener
/// can be handed it before the connect — which then succeeds or hangs
/// instead of being refused (flaked `an_unreachable_port_is_refused_not_hung`
/// on 2026-10-08). The port is held instead as the local end of an
/// established loopback connection: the kernel's `:0` allocator never hands
/// out a port in use, and with no listener on it a SYN is answered with RST.
///
/// Not a bound-but-unlistened socket: measured 2026-10-08, macOS silently
/// drops a SYN to one (3/3 timed out), so the test would hang rather than be
/// refused. The connection-held port refused 300/300 on macOS and on Linux,
/// and `:0` returned it 0 times in 20 000 binds on both.
pub fn refused_port() -> RefusedPort {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
    let holder = std::net::TcpStream::connect(listener.local_addr().unwrap()).expect("connect to it");
    let (peer, _) = listener.accept().expect("accept it");
    RefusedPort { port: holder.local_addr().unwrap().port(), _held: (holder, peer) }
}

#[cfg(test)]
mod tests {
    use super::refused_port;

    /// The two properties every caller of `refused_port` leans on.
    #[test]
    fn a_refused_port_refuses_at_once_and_is_never_handed_out_while_held() {
        let dead = refused_port();
        for _ in 0..50 {
            let err = std::net::TcpStream::connect_timeout(
                &([127, 0, 0, 1], dead.port).into(),
                std::time::Duration::from_secs(5),
            )
            .expect_err("nothing listens on a refused port");
            assert!(
                matches!(err.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset),
                "{err:?}"
            );
        }
        // What a parallel test does to get a port of its own — the race this closes.
        for _ in 0..2000 {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            assert_ne!(l.local_addr().unwrap().port(), dead.port, "a held port was handed to another listener");
        }
    }
}
