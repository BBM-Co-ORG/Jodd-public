//! The related-notes walk behind a note scope (spec 2026-10-23 §5.3): a
//! bounded breadth-first walk from one note over its wikilinks (both
//! directions) and/or the notes sharing one of its tags.
//!
//! Three rules the tests below pin:
//! - a note in a hidden folder is neither read nor expanded — its links and
//!   tags would otherwise reveal what it holds;
//! - nearer levels always win the cap, newest first within a level;
//! - nothing crosses into another account.

use std::collections::HashSet;

use crate::db::{CachedNote, Db};
use crate::privacy::ai_scope::AiScope;

/// The notes a note scope widens to. Never contains the focus note.
#[derive(Debug, Default)]
pub struct RelatedSet {
    pub notes: Vec<CachedNote>,
    /// True when the cap stopped the walk before it ran out of notes.
    pub capped: bool,
}

pub fn walk_related(
    db: &Db,
    focus: &CachedNote,
    links: bool,
    tags: bool,
    depth: u8,
    cap: usize,
    ai: &AiScope,
) -> rusqlite::Result<RelatedSet> {
    let mut out = RelatedSet::default();
    if !links && !tags {
        return Ok(out);
    }
    let account = focus.account_id.as_str();
    let mut seen: HashSet<String> = HashSet::from([focus.uuid.clone()]);
    let mut frontier: Vec<CachedNote> = vec![focus.clone()];
    // A tag's whole group arrives the first time it is expanded; querying it
    // again from every member is group-size² work for no new note.
    let mut expanded_tags: HashSet<String> = HashSet::new();

    for _ in 0..depth.clamp(1, crate::ask::NOTE_DEPTH_MAX) {
        let mut next: Vec<CachedNote> = Vec::new();
        for from in &frontier {
            let mut neighbours: Vec<CachedNote> = Vec::new();
            if links {
                neighbours.extend(db.outgoing_links(account, &from.uuid)?);
                neighbours.extend(db.backlinks(account, &from.uuid)?);
            }
            if tags {
                let new_tags: Vec<String> = db
                    .list_tags_for(account, &from.uuid)?
                    .into_iter()
                    .filter(|t| expanded_tags.insert(t.clone()))
                    .collect();
                neighbours.extend(db.notes_sharing_tags(account, &new_tags, &from.uuid)?);
            }
            for n in neighbours {
                // The link queries already exclude deleted_pending; the
                // account check is defense in depth — edges are per account.
                if n.account_id != account || !seen.insert(n.uuid.clone()) {
                    continue;
                }
                // Hidden: not read, and not enqueued, so never walked through.
                if !ai.allows(&n.account_id, &n.label) {
                    continue;
                }
                if out.notes.len() == cap {
                    out.capped = true;
                    return Ok(out);
                }
                out.notes.push(n.clone());
                next.push(n);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(out)
}

/// The focus note of a note scope, as it is NOW: the uuid followed through
/// a rekey alias (gotcha #16), refused when gone or in a hidden folder. The
/// preflight and every turn's re-check go through this — a note can be
/// deleted, or moved into a hidden folder, mid-conversation.
pub fn check_note_scope(db: &Db, ai: &AiScope, account_id: &str, uuid: &str) -> Result<CachedNote, String> {
    let uuid = db.resolve_note_uuid(uuid, account_id).map_err(|e| format!("resolve note: {e}"))?;
    let note = db
        .note_by_uuid(account_id, &uuid)
        .map_err(|e| format!("read note: {e}"))?
        .ok_or_else(|| crate::ask::NOTE_GONE.to_string())?;
    ai.check_label(account_id, &note.label)?;
    Ok(note)
}

/// What the popup shows as the scope before anything is sent (spec §5.1).
pub fn note_scope_description(account_id: &str, live_title: &str, any_box: bool, related: usize, capped: bool) -> String {
    let mut s = format!("{account_id} · {live_title}");
    if any_box {
        s.push_str(&format!(" + {related} related notes"));
    }
    if capped {
        s.push_str(&format!(" (closest {} kept)", crate::ask::CANDIDATE_POOL_MAX));
    }
    s
}

/// The note-scope half of `ask_preflight`: check the note, run the walk for
/// its count, describe it. Read-only SQL; the caller must not hold the AI
/// policy gate or the accounts lock while this runs.
pub fn preflight_note(db: &Db, ai: &AiScope, scope: &crate::ask::AskScope) -> Result<String, String> {
    let crate::ask::AskScope::Note { account_id, uuid, links, tags, .. } = scope else {
        return Err("not a note scope".into());
    };
    let focus = check_note_scope(db, ai, account_id, uuid)?;
    let related = walk_related(db, &focus, *links, *tags, scope.note_depth(), crate::ask::CANDIDATE_POOL_MAX, ai)
        .map_err(|e| format!("walk related notes: {e}"))?;
    Ok(note_scope_description(account_id, &focus.title, *links || *tags, related.notes.len(), related.capped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{note, temp_db};

    const A: &str = "acct@x";

    fn walk(db: &Db, focus: &str, links: bool, tags: bool, depth: u8, cap: usize, ai: &AiScope) -> RelatedSet {
        let f = db.note_by_uuid(A, focus).unwrap().expect("focus note");
        walk_related(db, &f, links, tags, depth, cap, ai).unwrap()
    }

    fn uuids(s: &RelatedSet) -> Vec<String> {
        let mut v: Vec<String> = s.notes.iter().map(|n| n.uuid.clone()).collect();
        v.sort();
        v
    }

    fn chain(db: &Db) {
        // a → b → c → d, each by title wikilink.
        note(A, "a").title("A").body("see [[B]]").insert(db);
        note(A, "b").title("B").body("see [[C]]").insert(db);
        note(A, "c").title("C").body("see [[D]]").insert(db);
        note(A, "d").title("D").body("end").insert(db);
    }

    #[test]
    fn links_follow_both_directions() {
        let db = temp_db();
        note(A, "a").title("A").body("see [[B]]").insert(&db);
        note(A, "b").title("B").body("plain").insert(&db);
        note(A, "c").title("C").body("about [[A]]").insert(&db);
        let s = walk(&db, "a", true, false, 1, 400, &AiScope::default());
        assert_eq!(uuids(&s), vec!["b", "c"]);
        assert!(!s.capped);
    }

    #[test]
    fn nothing_ticked_walks_nothing() {
        let db = temp_db();
        chain(&db);
        assert!(walk(&db, "a", false, false, 3, 400, &AiScope::default()).notes.is_empty());
    }

    #[test]
    fn depth_limits_the_chain() {
        let db = temp_db();
        chain(&db);
        let ai = AiScope::default();
        // Backlinks count too, so from "a" only forward neighbours exist.
        assert_eq!(uuids(&walk(&db, "a", true, false, 1, 400, &ai)), vec!["b"]);
        assert_eq!(uuids(&walk(&db, "a", true, false, 2, 400, &ai)), vec!["b", "c"]);
        assert_eq!(uuids(&walk(&db, "a", true, false, 3, 400, &ai)), vec!["b", "c", "d"]);
        assert_eq!(uuids(&walk(&db, "a", true, false, 9, 400, &ai)), vec!["b", "c", "d"]);
    }

    #[test]
    fn cycles_are_visited_once() {
        let db = temp_db();
        note(A, "a").title("A").body("see [[B]]").insert(&db);
        note(A, "b").title("B").body("back to [[A]]").insert(&db);
        assert_eq!(uuids(&walk(&db, "a", true, false, 3, 400, &AiScope::default())), vec!["b"]);
    }

    #[test]
    fn same_tag_notes_are_included() {
        let db = temp_db();
        note(A, "a").title("A").body("one #x").insert(&db);
        note(A, "b").title("B").body("two #x").insert(&db);
        note(A, "c").title("C").body("three #y").insert(&db);
        assert_eq!(uuids(&walk(&db, "a", false, true, 1, 400, &AiScope::default())), vec!["b"]);
    }

    #[test]
    fn tags_never_cross_accounts() {
        let db = temp_db();
        note(A, "a").title("A").body("one #x").insert(&db);
        note("other@x", "b").title("B").body("two #x").insert(&db);
        assert!(walk(&db, "a", false, true, 3, 400, &AiScope::default()).notes.is_empty());
    }

    #[test]
    fn a_hidden_note_is_not_read_or_walked_through() {
        let db = temp_db();
        note(A, "a").title("A").body("see [[H]]").insert(&db);
        note(A, "h").title("H").body("see [[C]] #secret").label("Notes/Private").insert(&db);
        note(A, "c").title("C").body("end #secret").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/Private"]);
        let s = walk(&db, "a", true, true, 3, 400, &ai);
        assert!(s.notes.is_empty(), "{:?}", uuids(&s));
    }

    #[test]
    fn deleted_notes_are_dropped() {
        let db = temp_db();
        note(A, "a").title("A").body("see [[B]]").insert(&db);
        note(A, "b").title("B").body("gone").insert(&db);
        db.mark_deleted("b", A).unwrap();
        assert!(walk(&db, "a", true, false, 1, 400, &AiScope::default()).notes.is_empty());
    }

    #[test]
    fn cap_keeps_nearest_first() {
        let db = temp_db();
        note(A, "a").title("A").body("[[B1]] [[B2]] [[B3]]").insert(&db);
        note(A, "b1").title("B1").body("[[C1]] [[C2]] [[C3]]").insert(&db);
        note(A, "b2").title("B2").body("x").insert(&db);
        note(A, "b3").title("B3").body("x").insert(&db);
        for c in ["c1", "c2", "c3"] {
            note(A, c).title(&c.to_uppercase()).body("x").insert(&db);
        }
        let s = walk(&db, "a", true, false, 2, 4, &AiScope::default());
        let got = uuids(&s);
        assert_eq!(got.len(), 4, "{got:?}");
        for b in ["b1", "b2", "b3"] {
            assert!(got.contains(&b.to_string()), "{got:?}");
        }
        assert!(s.capped);
    }

    #[test]
    fn note_description_plain() {
        assert_eq!(note_scope_description("a", "Plan", false, 0, false), "a · Plan");
    }

    #[test]
    fn note_description_with_zero_related() {
        assert_eq!(note_scope_description("a", "Plan", true, 0, false), "a · Plan + 0 related notes");
    }

    #[test]
    fn note_description_capped() {
        assert_eq!(
            note_scope_description("a", "Plan", true, 400, true),
            "a · Plan + 400 related notes (closest 400 kept)"
        );
    }

    fn scope(uuid: &str, links: bool) -> crate::ask::AskScope {
        crate::ask::AskScope::Note { account_id: A.into(), uuid: uuid.into(), title: "stale".into(), links, tags: false, depth: 1 }
    }

    #[test]
    fn preflight_reads_the_live_title_and_counts_related() {
        let db = temp_db();
        note(A, "a").title("Live title").body("see [[B]]").insert(&db);
        note(A, "b").title("B").body("x").insert(&db);
        assert_eq!(preflight_note(&db, &AiScope::default(), &scope("a", false)).unwrap(), "acct@x · Live title");
        assert_eq!(
            preflight_note(&db, &AiScope::default(), &scope("a", true)).unwrap(),
            "acct@x · Live title + 1 related notes"
        );
    }

    #[test]
    fn check_refuses_a_missing_or_hidden_note() {
        let db = temp_db();
        assert_eq!(check_note_scope(&db, &AiScope::default(), A, "nope").unwrap_err(), crate::ask::NOTE_GONE);
        note(A, "h").title("H").body("x").label("Notes/Private").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/Private"]);
        assert_eq!(check_note_scope(&db, &ai, A, "h").unwrap_err(), crate::privacy::ai_scope::HIDDEN_NOTE);
        db.mark_deleted("h", A).unwrap();
        assert_eq!(check_note_scope(&db, &AiScope::default(), A, "h").unwrap_err(), crate::ask::NOTE_GONE);
    }

    #[test]
    fn check_follows_a_rekey_alias() {
        let db = temp_db();
        note(A, "old").title("T").body("x").insert(&db);
        db.rekey_note_uuid("old", "new", A).unwrap();
        assert_eq!(check_note_scope(&db, &AiScope::default(), A, "old").unwrap().uuid, "new");
    }

    /// Review finding (2026-10-24): the 400 cap bounded the result, not the
    /// work — every frontier note re-queried the same tag group, so a dense
    /// tag at depth 3 read group-size² rows (measured ~0.5 s release for 300
    /// notes). Each tag is now expanded once per walk, so depth 3 over one tag
    /// group costs about what depth 1 does. Compared against depth 1 rather
    /// than a wall-clock bound, so a slow CI runner cannot flake it.
    #[test]
    fn a_dense_tag_group_is_expanded_once() {
        let db = temp_db();
        let body = format!("#x {}", "w".repeat(5_000));
        for i in 0..300 {
            note(A, &format!("n{i:03}")).title(&format!("N{i}")).body(&body).insert(&db);
        }
        let timed = |depth: u8| {
            let start = std::time::Instant::now();
            let s = walk(&db, "n000", false, true, depth, 400, &AiScope::default());
            assert_eq!(s.notes.len(), 299);
            start.elapsed()
        };
        timed(1); // warm the page cache
        let one = timed(1);
        let three = timed(3);
        assert!(three < one * 4 + std::time::Duration::from_millis(30), "depth 1 {one:?}, depth 3 {three:?}");
    }
}
