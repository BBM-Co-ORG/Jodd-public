//! Curate (M2, docs/superpowers/specs/2026-10-06-curate-m2-design.md):
//! Jodd finds problems deterministically (`candidates`, `secrets`), its own
//! AI decides what to propose (`decide`), and the user approves each fix,
//! which `apply` makes SQLite-first. Nothing is ever hard-deleted: a removed
//! note goes to the backend's trash.

pub mod apply;
pub mod candidates;
pub mod decide;
pub mod secrets;

use serde::{Deserialize, Serialize};

/// At most this many new proposals of one kind per scan — a reviewable
/// batch, not a backlog (spec D6).
pub const MAX_PER_KIND: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Duplicate,
    Misfiled,
    Secret,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Duplicate => "duplicate",
            Kind::Misfiled => "misfiled",
            Kind::Secret => "secret",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        [Kind::Duplicate, Kind::Misfiled, Kind::Secret].into_iter().find(|k| k.as_str() == s)
    }
}

/// A note as the proposal saw it. `local_version` is what makes a stale
/// proposal refuse instead of acting on a note the user has since edited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteRef {
    pub uuid: String,
    pub title: String,
    pub label: String,
    pub local_version: i64,
    pub date: String,
    pub chars: usize,
    /// sha256 of title + folder + body. `local_version` alone misses a
    /// remote edit — `upsert_from_remote` rewrites the body and keeps the
    /// version (the reason `append_reviewed_snapshot` exists); this does not.
    #[serde(default)]
    pub hash: String,
}

pub fn content_hash(n: &crate::db::CachedNote) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for part in [&n.title, &n.label, &n.body_html] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    format!("{:x}", h.finalize())
}

impl NoteRef {
    pub fn of(n: &crate::db::CachedNote) -> NoteRef {
        NoteRef {
            uuid: n.uuid.clone(),
            title: n.title.clone(),
            label: n.label.clone(),
            local_version: n.local_version,
            date: n.date.clone(),
            chars: crate::agent_memory::text::html_to_text(&n.body_html, false).chars().count(),
            hash: content_hash(n),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Action {
    /// Keep `keep`; every other note in the proposal goes to trash.
    Keep { keep: String },
    /// Append every other note to `into`, then trash them.
    Append { into: String },
    /// Move the (single) note to `to`.
    Move { to: String },
    /// Move the (single) note into a folder hidden from agents.
    Hide,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Payload {
    pub notes: Vec<NoteRef>,
    pub action: Action,
    pub reason: String,
    /// Why Jodd flagged it: a shared URL, a similarity, a masked secret kind.
    /// Never a secret value (spec §3.2).
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub id: i64,
    pub kind: Kind,
    pub status: String,
    pub created_at: i64,
    pub error: Option<String>,
    pub payload: Payload,
}

/// `(account, kind, note_key)` identifies a proposal for de-duplication:
/// the sorted uuids it touches.
pub fn note_key(notes: &[NoteRef]) -> String {
    let mut uuids: Vec<&str> = notes.iter().map(|n| n.uuid.as_str()).collect();
    uuids.sort_unstable();
    uuids.join(",")
}

pub fn list(db: &crate::db::Db, account_id: &str, statuses: &[&str]) -> Result<Vec<Proposal>, String> {
    db.curate_list(account_id, statuses)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|(id, kind, payload, status, created_at, error)| {
            Some(Proposal { id, kind: Kind::parse(&kind)?, status, created_at, error, payload: serde_json::from_str(&payload).ok()? })
        })
        .map(Ok)
        .collect()
}

/// `true` when it was stored, `false` when an equal proposal is pending or
/// was dismissed.
pub fn propose(db: &crate::db::Db, account_id: &str, kind: Kind, payload: &Payload, source: &str) -> Result<bool, String> {
    let json = serde_json::to_string(payload).map_err(|e| e.to_string())?;
    Ok(db.curate_insert(account_id, kind.as_str(), &note_key(&payload.notes), &json, source).map_err(|e| e.to_string())?.is_some())
}

/// Pending, stale and failed proposals can all be dismissed — the latter two
/// so the user can clear a card that can no longer be applied.
pub fn dismiss(db: &crate::db::Db, id: i64) -> Result<(), String> {
    for from in ["pending", "stale", "failed"] {
        if db.curate_decide(id, from, "dismissed", None).map_err(|e| e.to_string())? {
            return Ok(());
        }
    }
    Err("This proposal was already decided.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_db;

    fn payload(uuids: &[&str]) -> Payload {
        Payload {
            notes: uuids.iter().map(|u| NoteRef { uuid: u.to_string(), title: u.to_string(), label: "Notes".into(), local_version: 1, date: String::new(), chars: 0, hash: String::new() }).collect(),
            action: Action::Keep { keep: uuids[0].to_string() },
            reason: "r".into(),
            evidence: vec![],
        }
    }

    #[test]
    fn a_scan_never_repeats_a_pending_or_dismissed_proposal() {
        let db = temp_db();
        assert!(propose(&db, "a", Kind::Duplicate, &payload(&["B", "A"]), "jodd-ai").unwrap());
        assert!(!propose(&db, "a", Kind::Duplicate, &payload(&["A", "B"]), "jodd-ai").unwrap(), "same set, other order");
        let id = list(&db, "a", &["pending"]).unwrap()[0].id;
        dismiss(&db, id).unwrap();
        assert!(!propose(&db, "a", Kind::Duplicate, &payload(&["A", "B"]), "jodd-ai").unwrap(), "dismissed stays dismissed");
        assert!(propose(&db, "a", Kind::Misfiled, &payload(&["A", "B"]), "jodd-ai").unwrap(), "another kind is another proposal");
        assert!(propose(&db, "other", Kind::Duplicate, &payload(&["A", "B"]), "jodd-ai").unwrap(), "per account");
    }

    #[test]
    fn deciding_twice_is_refused() {
        let db = temp_db();
        propose(&db, "a", Kind::Duplicate, &payload(&["A", "B"]), "jodd-ai").unwrap();
        let id = list(&db, "a", &["pending"]).unwrap()[0].id;
        assert!(db.curate_decide(id, "pending", "applied", None).unwrap());
        assert!(!db.curate_decide(id, "pending", "dismissed", None).unwrap());
        assert!(dismiss(&db, id).unwrap_err().contains("already decided"));
    }
}
