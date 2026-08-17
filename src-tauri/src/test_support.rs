//! Shared test fixtures for `ask::*` modules and `db.rs`'s own tests.
//!
//! Promoted out of `db.rs`'s `tests_ask_queries` module (Task 4 built it
//! there; this move is a plan-defect repair, not new scope — see Task 5
//! brief Step 0). `test_support` is `#[cfg(test)]`-only (see `lib.rs`), so
//! this file never ships in a release build.

use crate::db::{CachedNote, Db, SyncState};

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
        },
    }
}

impl NoteBuilder {
    pub fn label(mut self, label: &str) -> Self {
        self.n.label = label.to_string();
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

    pub fn insert(self, db: &Db) {
        db.insert_local_new(&self.n).expect("insert fixture note");
    }
}
