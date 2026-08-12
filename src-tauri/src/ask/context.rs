//! Stage 4 — turning selected note ids into bounded answer context
//! (spec §5.4).

use std::collections::HashSet;

use crate::ask::{AnswerContext, SelectedNote, MAX_CONTEXT_CHARS, MAX_NOTE_CHARS, MAX_SELECTED_NOTES};
use crate::db::Db;

/// Fetch the selected notes and apply the three budget caps.
///
/// Cap ORDER is load-bearing: per-note first, then count, then total. The
/// live vault's largest note is 1,037,880 chars, 8x the whole-turn budget —
/// applying only a total cap would let that one note consume the turn and
/// silently crowd out every other selection.
///
/// `fts_uuids` are the pool members that matched the question's own words;
/// they survive the count trim first, since they are the only ones with
/// evidence of relevance to *this* question.
pub fn build_answer_context(
    db: &Db,
    selected: &[(String, String)],
    fts_uuids: &HashSet<String>,
) -> rusqlite::Result<AnswerContext> {
    let mut fetched: Vec<(bool, SelectedNote)> = Vec::new();

    for (account_id, uuid) in selected {
        // A note can disappear between selection and fetch (a sync tick may
        // have deleted it). Skipping is correct: the answer simply cites one
        // fewer source, which is better than failing the whole turn.
        let Some(n) = db.note_by_uuid(account_id, uuid)? else {
            continue;
        };
        let full = crate::db::strip_html_to_text(&n.body_html);
        let truncated = full.chars().count() > MAX_NOTE_CHARS;
        let text: String = if truncated {
            // Reserve room for the suffix so the whole string, suffix
            // included, still respects MAX_NOTE_CHARS.
            const SUFFIX: &str = "\n… [truncated]";
            let suffix_len = SUFFIX.chars().count();
            let keep = MAX_NOTE_CHARS.saturating_sub(suffix_len);
            full.chars().take(keep).collect::<String>() + SUFFIX
        } else {
            full
        };
        fetched.push((
            fts_uuids.contains(uuid),
            SelectedNote {
                uuid8: crate::db::uuid_short(&n.uuid),
                slug: crate::db::note_slug(&n.title, &n.uuid),
                uuid: n.uuid.clone(),
                account_id: n.account_id.clone(),
                title: n.title.clone(),
                text,
                truncated,
            },
        ));
    }

    let before = fetched.len();

    // FTS hits first, stable within each group so the model's stated order is
    // otherwise preserved.
    fetched.sort_by_key(|(is_fts, _)| !*is_fts);
    fetched.truncate(MAX_SELECTED_NOTES);

    let notes = apply_total_cap(fetched.into_iter().map(|(_, n)| n).collect());

    let trimmed = notes.len() < before;
    Ok(AnswerContext { notes, trimmed })
}

/// The total-characters cap: admit notes (in their given order) until the
/// running total would exceed `MAX_CONTEXT_CHARS`, then stop.
///
/// The first note is always admitted regardless of its own length — the
/// `!notes.is_empty()` guard exists so that one oversized note produces "an
/// answer from a big source" instead of "an answer from nothing". In
/// practice, given MAX_NOTE_CHARS (20,000) < MAX_CONTEXT_CHARS (120,000),
/// no note reaching this function can be long enough on its own to trip the
/// guard — every note was already truncated to at most MAX_NOTE_CHARS chars
/// upstream. The guard is exercised directly (bypassing that upstream
/// truncation) in the unit tests below, since it is dead code on the public
/// `build_answer_context` path under the current constants and would only
/// activate if a future change made MAX_NOTE_CHARS >= MAX_CONTEXT_CHARS.
fn apply_total_cap(fetched: Vec<SelectedNote>) -> Vec<SelectedNote> {
    let mut notes: Vec<SelectedNote> = Vec::new();
    let mut total = 0usize;
    for n in fetched {
        let len = n.text.chars().count();
        if total + len > MAX_CONTEXT_CHARS && !notes.is_empty() {
            break;
        }
        total += len;
        notes.push(n);
    }
    notes
}

/// The context block sent to the answer call. Each note is labelled with the
/// exact slug the model must cite, so citing correctly requires no invention.
pub fn render_context(ctx: &AnswerContext) -> String {
    let mut out = String::new();
    for n in &ctx.notes {
        out.push_str(&format!("--- NOTE [[{}]] — {}\n{}\n\n", n.slug, n.title, n.text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{note, temp_db};
    use std::collections::HashSet;

    #[test]
    fn a_huge_note_is_truncated_and_does_not_evict_the_others() {
        // The live vault's largest note is 1,037,880 chars — 8x the whole-turn
        // budget. Capping per note BEFORE the total is what keeps it from
        // consuming the entire turn (spec F1b).
        let db = temp_db();
        let a = "acct@x";
        note(a, "big").title("Big").body(&"x".repeat(1_000_000)).insert(&db);
        note(a, "small1").title("S1").body("short body one").insert(&db);
        note(a, "small2").title("S2").body("short body two").insert(&db);

        let selected = vec![
            (a.to_string(), "big".to_string()),
            (a.to_string(), "small1".to_string()),
            (a.to_string(), "small2".to_string()),
        ];
        let ctx = build_answer_context(&db, &selected, &HashSet::new()).unwrap();

        assert_eq!(ctx.notes.len(), 3, "the big note must not evict the small ones");
        let big = ctx.notes.iter().find(|n| n.uuid == "big").unwrap();
        assert!(big.truncated);
        assert!(big.text.chars().count() <= crate::ask::MAX_NOTE_CHARS);
        // Pin content, not just the bound: the cut must fill the budget (not
        // discard everything down to 0 or some arbitrary smaller amount) and
        // must actually carry the truncation marker the answer prompt tells
        // the model to look for.
        assert!(
            big.text.chars().count() > crate::ask::MAX_NOTE_CHARS - 100,
            "truncated text should nearly fill the budget, got {} chars",
            big.text.chars().count()
        );
        assert!(big.text.ends_with("[truncated]"), "got tail {:?}", &big.text[big.text.len().saturating_sub(40)..]);
    }

    #[test]
    fn the_total_cap_binds_and_admits_roughly_six_of_twelve_equal_notes() {
        // Each fixture body strips to 20,004 chars, so the per-note cap DOES
        // fire on all 12, clamping each to exactly MAX_NOTE_CHARS (20,000).
        // The arithmetic is still what makes this the only test that can
        // catch a broken or missing total-chars cap: 12 * 20,000 = 240,000 is
        // double MAX_CONTEXT_CHARS (120,000), so ~6 of the 12 clamped notes
        // survive — MAX_SELECTED_NOTES (12) never binds either.
        let db = temp_db();
        let a = "acct@x";
        let body = "y".repeat(20_000);
        let mut selected = Vec::new();
        for i in 0..12 {
            let uuid = format!("eq{i}");
            note(a, &uuid).title(&format!("Eq{i}")).body(&body).insert(&db);
            selected.push((a.to_string(), uuid));
        }
        assert!(12 * 20_000 > crate::ask::MAX_CONTEXT_CHARS);
        assert!(12 <= crate::ask::MAX_SELECTED_NOTES);

        let ctx = build_answer_context(&db, &selected, &HashSet::new()).unwrap();

        assert!(ctx.trimmed, "the total cap must have dropped at least one note");
        // 120,000 / 20,000 == 6 exactly.
        assert_eq!(ctx.notes.len(), 6, "expected exactly six ~20k-char notes to fit in the 120k budget");
        let total: usize = ctx.notes.iter().map(|n| n.text.chars().count()).sum();
        assert!(total <= crate::ask::MAX_CONTEXT_CHARS, "total {total} must not exceed the budget");
    }

    #[test]
    fn a_single_oversized_note_is_still_admitted_not_dropped_to_empty() {
        // Practical-level check: given the real constants, a huge body is
        // capped by MAX_NOTE_CHARS long before the total cap ever sees it,
        // so a lone note is trivially under budget. This pins that observed
        // behaviour; the guard itself is exercised directly below since it
        // cannot be triggered through this public path (see apply_total_cap's
        // doc comment).
        let db = temp_db();
        let a = "acct@x";
        note(a, "solo").title("Solo").body(&"z".repeat(500_000)).insert(&db);
        let selected = vec![(a.to_string(), "solo".to_string())];

        let ctx = build_answer_context(&db, &selected, &HashSet::new()).unwrap();

        assert_eq!(ctx.notes.len(), 1, "a lone oversized note must still be admitted");
        assert!(!ctx.trimmed, "nothing was dropped — one note was selected, one note survived");
    }

    fn synthetic_note(uuid: &str, text_len: usize) -> SelectedNote {
        SelectedNote {
            uuid: uuid.to_string(),
            account_id: "acct@x".to_string(),
            uuid8: uuid.to_string(),
            title: uuid.to_string(),
            slug: uuid.to_string(),
            text: "q".repeat(text_len),
            truncated: false,
        }
    }

    #[test]
    fn apply_total_cap_admits_a_lone_note_even_when_it_alone_exceeds_the_budget() {
        // Direct test of the `!notes.is_empty()` first-note guard
        // (context.rs), bypassing the upstream per-note truncation that
        // makes this unreachable through build_answer_context under the
        // current constants. A synthetic note deliberately longer than
        // MAX_CONTEXT_CHARS must still come back — refusing it would leave
        // the answer call with zero context instead of one big source.
        let solo = synthetic_note("solo", crate::ask::MAX_CONTEXT_CHARS + 50_000);
        let notes = apply_total_cap(vec![solo]);
        assert_eq!(notes.len(), 1, "a lone over-budget note must still be admitted");

        // And once one over-budget note is in, a second note must NOT be
        // admitted on top of it — the guard is "always admit the first",
        // not "ignore the budget forever".
        let over = synthetic_note("solo", crate::ask::MAX_CONTEXT_CHARS + 50_000);
        let extra = synthetic_note("extra", 10);
        let notes = apply_total_cap(vec![over, extra]);
        assert_eq!(notes.len(), 1, "a second note must not be admitted once the first already exceeds budget");
        assert_eq!(notes[0].uuid, "solo");
    }

    #[test]
    fn selection_is_capped_at_max_selected_notes() {
        let db = temp_db();
        let a = "acct@x";
        let mut selected = Vec::new();
        for i in 0..(crate::ask::MAX_SELECTED_NOTES + 5) {
            let uuid = format!("u{i}");
            note(a, &uuid).insert(&db);
            selected.push((a.to_string(), uuid));
        }
        let ctx = build_answer_context(&db, &selected, &HashSet::new()).unwrap();
        assert_eq!(ctx.notes.len(), crate::ask::MAX_SELECTED_NOTES);
        assert!(ctx.trimmed);
    }

    #[test]
    fn fts_hits_survive_the_trim_ahead_of_others() {
        let db = temp_db();
        let a = "acct@x";
        let mut selected = Vec::new();
        for i in 0..(crate::ask::MAX_SELECTED_NOTES + 3) {
            let uuid = format!("u{i}");
            note(a, &uuid).insert(&db);
            selected.push((a.to_string(), uuid));
        }
        // The LAST one is an FTS hit — without prioritization it would be cut.
        let last = selected.last().unwrap().1.clone();
        let mut fts = HashSet::new();
        fts.insert(last.clone());

        let ctx = build_answer_context(&db, &selected, &fts).unwrap();
        assert!(ctx.notes.iter().any(|n| n.uuid == last), "FTS hit must survive the trim");
    }

    #[test]
    fn a_missing_note_is_skipped_not_an_error() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "here").insert(&db);
        let selected = vec![
            (a.to_string(), "here".to_string()),
            (a.to_string(), "vanished".to_string()),
        ];
        let ctx = build_answer_context(&db, &selected, &HashSet::new()).unwrap();
        assert_eq!(ctx.notes.len(), 1);
    }

    #[test]
    fn rendered_context_carries_the_slug_for_citation() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "aabbccdd-0000-0000-0000-000000000000").title("Sync conflicts").body("body").insert(&db);
        let selected = vec![(a.to_string(), "aabbccdd-0000-0000-0000-000000000000".to_string())];
        let ctx = build_answer_context(&db, &selected, &HashSet::new()).unwrap();
        let rendered = render_context(&ctx);
        assert!(rendered.contains("sync-conflicts-aabbccdd"), "got {rendered}");
        assert!(rendered.contains("body"));
    }

    #[test]
    fn empty_selection_yields_empty_context() {
        let db = temp_db();
        let ctx = build_answer_context(&db, &[], &HashSet::new()).unwrap();
        assert!(ctx.notes.is_empty());
        assert!(!ctx.trimmed);
    }
}
