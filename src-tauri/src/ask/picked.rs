//! The `Notes` scope: notes the owner picked together in the note list.
//!
//! Only the picked notes are ever read — no pool, no related walk. A note
//! that was deleted since, or sits in a folder hidden from AI, is left out
//! and counted, so the scope line can say what was dropped instead of the
//! answer silently drawing on fewer notes than the owner asked about.

use std::collections::HashSet;

use crate::db::{CachedNote, Db};
use crate::privacy::ai_scope::AiScope;

/// Refusal when none of the picked notes exists any more.
pub const NOTES_GONE: &str = "None of these notes is available any more.";

/// The picked notes as they are NOW, in the order they were picked.
#[derive(Debug, Default)]
pub struct Picked {
    pub notes: Vec<CachedNote>,
    /// Deleted, or never existed.
    pub gone: usize,
    /// In a folder hidden from AI.
    pub hidden: usize,
    /// More than `PICKED_NOTES_MAX` were picked; the first ones were kept.
    pub capped: bool,
}

/// Resolve every picked uuid (following rekey aliases, gotcha #16), drop
/// duplicates, deleted notes and hidden ones. Refused only when nothing is
/// left — a scope with some notes left out still answers, and says so.
///
/// The preflight and every turn go through this: a note can be deleted, or
/// moved into a hidden folder, mid-conversation.
pub fn resolve_picked(db: &Db, ai: &AiScope, account_id: &str, uuids: &[String]) -> Result<Picked, String> {
    let mut out = Picked { capped: uuids.len() > crate::ask::PICKED_NOTES_MAX, ..Default::default() };
    let mut seen: HashSet<String> = HashSet::new();
    for raw in uuids.iter().take(crate::ask::PICKED_NOTES_MAX) {
        let uuid = db.resolve_note_uuid(raw, account_id).map_err(|e| format!("resolve note: {e}"))?;
        if !seen.insert(uuid.clone()) {
            continue;
        }
        match db.note_by_uuid(account_id, &uuid).map_err(|e| format!("read note: {e}"))? {
            None => out.gone += 1,
            Some(n) if !ai.allows(account_id, &n.label) => out.hidden += 1,
            Some(n) => out.notes.push(n),
        }
    }
    if out.notes.is_empty() {
        // All hidden says why; anything else reads as gone.
        return Err(if out.hidden > 0 && out.gone == 0 {
            crate::privacy::ai_scope::HIDDEN_NOTE.to_string()
        } else {
            NOTES_GONE.to_string()
        });
    }
    Ok(out)
}

/// What the popup shows as the scope before anything is sent: up to three
/// live titles, then a count of the rest and of what was left out.
pub fn picked_scope_description(account_id: &str, p: &Picked) -> String {
    const SHOWN: usize = 3;
    let titles: Vec<String> = p
        .notes
        .iter()
        .take(SHOWN)
        .map(|n| format!("\u{201c}{}\u{201d}", if n.title.trim().is_empty() { "Untitled" } else { n.title.trim() }))
        .collect();
    let mut s = format!("{account_id} · {}", titles.join(", "));
    if p.notes.len() > SHOWN {
        s.push_str(&format!(" and {} more", p.notes.len() - SHOWN));
    }
    let mut left_out: Vec<String> = Vec::new();
    if p.gone > 0 {
        left_out.push(format!("{} no longer available", p.gone));
    }
    if p.hidden > 0 {
        left_out.push(format!("{} hidden from AI", p.hidden));
    }
    if !left_out.is_empty() {
        s.push_str(&format!(" ({} — left out)", left_out.join(", ")));
    }
    if p.capped {
        s.push_str(&format!(" (first {} kept)", crate::ask::PICKED_NOTES_MAX));
    }
    s
}

/// The `Notes` half of `ask_preflight`. Read-only SQL.
pub fn preflight_picked(db: &Db, ai: &AiScope, scope: &crate::ask::AskScope) -> Result<String, String> {
    let crate::ask::AskScope::Notes { account_id, uuids, .. } = scope else {
        return Err("not a picked-notes scope".into());
    };
    let p = resolve_picked(db, ai, account_id, uuids)?;
    Ok(picked_scope_description(account_id, &p))
}

/// Per-note character caps that share `total` fairly: a note shorter than
/// its share keeps all of it and hands the rest to the longer ones (water
/// filling). The caps never sum past `total`, so `apply_total_cap` drops
/// nothing — every picked note the answer reads gets a fair slice, instead
/// of the first long one taking the turn.
pub fn fair_share_caps(lens: &[usize], total: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..lens.len()).collect();
    order.sort_by_key(|&i| lens[i]);
    let mut caps = vec![0; lens.len()];
    let mut remaining = total;
    for (k, &i) in order.iter().enumerate() {
        let share = remaining / (lens.len() - k);
        caps[i] = lens[i].min(share);
        remaining -= caps[i];
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{note, temp_db};

    const A: &str = "acct@x";

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn keeps_pick_order_and_drops_duplicates() {
        let db = temp_db();
        note(A, "b").title("B").insert(&db);
        note(A, "a").title("A").insert(&db);
        let p = resolve_picked(&db, &AiScope::default(), A, &ids(&["b", "a", "b"])).unwrap();
        let got: Vec<&str> = p.notes.iter().map(|n| n.uuid.as_str()).collect();
        assert_eq!(got, ["b", "a"]);
        assert_eq!((p.gone, p.hidden, p.capped), (0, 0, false));
    }

    #[test]
    fn leaves_out_gone_and_hidden_notes_and_says_so() {
        let db = temp_db();
        note(A, "ok").title("Plan").insert(&db);
        note(A, "secret").title("Bank").label("Notes/Private").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/Private"]);
        let p = resolve_picked(&db, &ai, A, &ids(&["ok", "secret", "deleted"])).unwrap();
        assert_eq!(p.notes.len(), 1);
        assert_eq!((p.gone, p.hidden), (1, 1));
        assert_eq!(
            picked_scope_description(A, &p),
            "acct@x · \u{201c}Plan\u{201d} (1 no longer available, 1 hidden from AI — left out)"
        );
    }

    #[test]
    fn a_hidden_note_is_never_read_even_by_another_account_id() {
        // The note exists only in A; asking as B must not find it.
        let db = temp_db();
        note(A, "x").title("X").insert(&db);
        assert_eq!(resolve_picked(&db, &AiScope::default(), "other@x", &ids(&["x"])).unwrap_err(), NOTES_GONE);
    }

    #[test]
    fn refuses_only_when_nothing_is_left() {
        let db = temp_db();
        note(A, "secret").title("Bank").label("Notes/Private").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/Private"]);
        assert_eq!(
            resolve_picked(&db, &ai, A, &ids(&["secret"])).unwrap_err(),
            crate::privacy::ai_scope::HIDDEN_NOTE
        );
        assert_eq!(resolve_picked(&db, &ai, A, &ids(&["gone"])).unwrap_err(), NOTES_GONE);
        assert_eq!(resolve_picked(&db, &ai, A, &ids(&[])).unwrap_err(), NOTES_GONE);
    }

    #[test]
    fn description_names_three_then_counts() {
        let db = temp_db();
        for (u, t) in [("1", "One"), ("2", ""), ("3", "Three"), ("4", "Four"), ("5", "Five")] {
            note(A, u).title(t).insert(&db);
        }
        let p = resolve_picked(&db, &AiScope::default(), A, &ids(&["1", "2", "3", "4", "5"])).unwrap();
        assert_eq!(
            picked_scope_description(A, &p),
            "acct@x · \u{201c}One\u{201d}, \u{201c}Untitled\u{201d}, \u{201c}Three\u{201d} and 2 more"
        );
    }

    #[test]
    fn a_select_all_is_bounded() {
        let db = temp_db();
        let many: Vec<String> = (0..crate::ask::PICKED_NOTES_MAX + 5).map(|i| format!("n{i}")).collect();
        for u in &many {
            note(A, u).title(u).insert(&db);
        }
        let p = resolve_picked(&db, &AiScope::default(), A, &many).unwrap();
        assert_eq!(p.notes.len(), crate::ask::PICKED_NOTES_MAX);
        assert!(p.capped);
        assert!(picked_scope_description(A, &p).ends_with(&format!("(first {} kept)", crate::ask::PICKED_NOTES_MAX)));
    }

    #[test]
    fn fair_share_gives_short_notes_all_and_splits_the_rest() {
        assert_eq!(fair_share_caps(&[100, 1_000, 1_000], 1_000), vec![100, 450, 450]);
        assert_eq!(fair_share_caps(&[10, 20], 1_000), vec![10, 20]);
        assert_eq!(fair_share_caps(&[5_000, 5_000, 5_000], 900), vec![300, 300, 300]);
        assert_eq!(fair_share_caps(&[], 900), Vec::<usize>::new());
        let caps = fair_share_caps(&[1, 50_000, 999_999, 7, 120_000], 120_000);
        assert!(caps.iter().sum::<usize>() <= 120_000);
        assert_eq!((caps[0], caps[3]), (1, 7));
    }
}
