//! Stage 1 — the SQL pre-filter (spec §5.1).
//!
//! This, not the catalog, is what makes the feature tractable: a full catalog
//! of the live 6,655-note account would be ~150k tokens. The pool is the union
//! of three cheap SQL sources, deduped and capped, and it is the recall
//! ceiling of the whole design — which is why the UI reports how many notes
//! were in scope versus considered.

use std::collections::HashMap;

use crate::ask::terms::extract_query_terms;
use crate::ask::{AskScope, Candidate, CANDIDATE_POOL_MAX, RECENCY_K};
use crate::db::{CachedNote, Db};

/// Max terms pulled out of a question. Each becomes one FTS query, so this
/// bounds the query count per turn.
const MAX_QUERY_TERMS: usize = 8;

fn to_candidate(n: &CachedNote, tags: Vec<String>, from_fts: bool) -> Candidate {
    Candidate {
        uuid8: crate::db::uuid_short(&n.uuid),
        uuid: n.uuid.clone(),
        account_id: n.account_id.clone(),
        title: n.title.clone(),
        label: n.label.clone(),
        tags,
        date_ms: n.last_remote_modified_at.unwrap_or(n.last_local_modified_at),
        from_fts,
    }
}

/// Tags for a note, derived from its body — the same derivation the rest of
/// Jodd uses, so no extra query and no risk of disagreeing with note_tags.
fn tags_of(n: &CachedNote) -> Vec<String> {
    crate::db::tags_from_body(&n.body_html)
}

pub fn build_candidate_pool(
    db: &Db,
    scope: &AskScope,
    question: &str,
    exclude_accounts: &[String],
) -> rusqlite::Result<Vec<Candidate>> {
    build_candidate_pool_capped(db, scope, question, CANDIDATE_POOL_MAX, exclude_accounts)
}

/// Cap is a parameter so tests can exercise the fill order without inserting
/// 400 fixtures.
pub fn build_candidate_pool_capped(
    db: &Db,
    scope: &AskScope,
    question: &str,
    cap: usize,
    exclude_accounts: &[String],
) -> rusqlite::Result<Vec<Candidate>> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen: HashMap<(String, String), usize> = HashMap::new();

    let push = |out: &mut Vec<Candidate>,
                    seen: &mut HashMap<(String, String), usize>,
                    n: &CachedNote,
                    from_fts: bool| {
        let key = (n.account_id.clone(), n.uuid.clone());
        if let Some(&idx) = seen.get(&key) {
            // Already pooled from another source. An FTS hit is strictly more
            // informative than a recency hit, so let the flag win.
            //
            // Not load-bearing today: Source 1 (FTS) always runs first, so a
            // duplicate can only re-set an already-true flag right now. This
            // guards against a future reordering of the sources (or a fourth
            // source landing before FTS) silently losing the flag — keep it.
            if from_fts {
                out[idx].from_fts = true;
            }
            return;
        }
        seen.insert(key, out.len());
        out.push(to_candidate(n, tags_of(n), from_fts));
    };

    // ── Source 1: FTS over the question's content words ──────────────────
    // Scope is applied to the RESULTS, not inside search_notes: passing its
    // exact-match `label` filter down would make this source blind to
    // descendants the other two sources can see (spec §5.5).
    for term in extract_query_terms(question, MAX_QUERY_TERMS) {
        for n in db.search_notes(scope.account_id(), None, &term, exclude_accounts)? {
            if !in_scope(&n, scope) {
                continue;
            }
            push(&mut out, &mut seen, &n, true);
        }
    }

    // ── Source 2: structural (folder scope only) ─────────────────────────
    // Skipped for account/all-accounts scope, where it would be the whole
    // vault and the cap would be meaningless.
    if let AskScope::Folder { account_id, label } = scope {
        for n in db.list_notes_in_subtree(account_id, label)? {
            push(&mut out, &mut seen, &n, false);
        }
    }

    // ── Source 3: recency prior ──────────────────────────────────────────
    for n in db.list_recent_notes(scope.account_id(), RECENCY_K, exclude_accounts)? {
        if !in_scope(&n, scope) {
            continue;
        }
        push(&mut out, &mut seen, &n, false);
    }

    // Fill order when the cap binds: FTS first (evidence of relevance to this
    // question), then everything else in insertion order.
    //
    // In practice this sort is a no-op today, not merely stable: Source 1
    // (FTS) is the only pusher of from_fts=true and always runs to
    // completion before Source 2/3 push anything, and the dedup branch
    // above only ever flips a flag toward true, never toward false. So `out`
    // arrives here already laid out as [FTS…, non-FTS…] — already sorted by
    // this key, not just grouped. Determinism therefore comes from source
    // ordering, not from sort stability. sort_by_key (not sort_unstable_by_key)
    // is kept anyway as a stable safety net: if a future source ever gets
    // spliced in ahead of FTS, this becomes load-bearing again, and an
    // unstable sort would be free to reorder equal-key elements at that
    // point.
    out.sort_by_key(|c| !c.from_fts);
    out.truncate(cap);
    Ok(out)
}

/// Whether a note satisfies a scope. Folder scope is recursive; the '/' before
/// the wildcard is what stops 'Notes/A' from matching 'Notes/AB'.
fn in_scope(n: &CachedNote, scope: &AskScope) -> bool {
    match scope {
        AskScope::AllAccounts => true,
        AskScope::Account { account_id } => &n.account_id == account_id,
        AskScope::Folder { account_id, label } => {
            &n.account_id == account_id
                && (&n.label == label || n.label.starts_with(&format!("{label}/")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{note, temp_db};

    #[test]
    fn fts_hits_are_included_and_flagged() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "u1").title("Sync conflicts").body("keep-both reconciliation").insert(&db);
        note(a, "u2").title("Unrelated").body("grocery list").insert(&db);

        let pool = build_candidate_pool(
            &db,
            &crate::ask::AskScope::Account { account_id: a.into() },
            "what did I decide about sync conflicts?",
            &[],
        )
        .unwrap();

        let hit = pool.iter().find(|c| c.uuid == "u1").expect("u1 should be in the pool");
        assert!(hit.from_fts, "u1 matched the term 'conflicts' and must be flagged");
    }

    #[test]
    fn recency_fills_the_pool_when_nothing_matches() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "u1").title("Alpha").body("aaa").modified_ms(1_000).insert(&db);
        note(a, "u2").title("Beta").body("bbb").modified_ms(2_000).insert(&db);

        let pool = build_candidate_pool(
            &db,
            &crate::ask::AskScope::Account { account_id: a.into() },
            "zzzzz qqqqq wwwww",
            &[],
        )
        .unwrap();

        assert_eq!(pool.len(), 2, "recency backfills when FTS finds nothing");
        assert!(pool.iter().all(|c| !c.from_fts));
        assert_eq!(pool[0].uuid, "u2", "newest first");
    }

    #[test]
    fn pool_is_deduplicated_across_sources() {
        let db = temp_db();
        let a = "acct@x";
        // Matches FTS AND is the most recent, so it comes from two sources.
        note(a, "u1").title("Sync conflicts").body("text").modified_ms(9_999).insert(&db);

        let pool = build_candidate_pool(
            &db,
            &crate::ask::AskScope::Account { account_id: a.into() },
            "sync conflicts",
            &[],
        )
        .unwrap();

        assert_eq!(pool.iter().filter(|c| c.uuid == "u1").count(), 1);
        // u1 is inserted by the FTS source (Source 1 runs first), so
        // from_fts is true from that initial push — this does NOT exercise
        // the flag-merge branch at the top of `push` (that branch only
        // matters if a non-FTS source could ever run before FTS; see its
        // comment). What this assertion actually guards: dedup keeps the
        // single surviving entry's from_fts flag as true rather than
        // clobbering it back to false.
        assert!(pool[0].from_fts, "the surviving deduped entry must keep from_fts = true");
    }

    #[test]
    fn candidate_date_ms_reflects_last_remote_modified_at() {
        // Regression guard for spec F4: date_ms must come from the epoch
        // column (last_remote_modified_at via .modified_ms()), never from
        // the RFC822 `date` string. Mutating pool.rs's to_candidate to read
        // a different field, or to hardcode a value, fails this.
        let db = temp_db();
        let a = "acct@x";
        note(a, "u1").modified_ms(1_234_567_890_123).insert(&db);

        let pool = build_candidate_pool(
            &db,
            &crate::ask::AskScope::Account { account_id: a.into() },
            "anything",
            &[],
        )
        .unwrap();

        let c = pool.iter().find(|c| c.uuid == "u1").expect("u1 in pool");
        assert_eq!(c.date_ms, 1_234_567_890_123);
    }

    #[test]
    fn fts_hits_come_before_recency_when_the_cap_binds() {
        let db = temp_db();
        let a = "acct@x";
        // One old note that matches, and many newer ones that do not.
        note(a, "match").title("Tahoe bundle").body("signing").modified_ms(1).insert(&db);
        for i in 0..50 {
            note(a, &format!("n{i}")).title("filler").body("nothing").modified_ms(1_000 + i).insert(&db);
        }

        let pool = build_candidate_pool_capped(
            &db,
            &crate::ask::AskScope::Account { account_id: a.into() },
            "tahoe bundle signing",
            3,
            &[],
        )
        .unwrap();

        assert_eq!(pool.len(), 3);
        assert_eq!(pool[0].uuid, "match", "FTS hits fill first even when oldest");
    }

    #[test]
    fn fts_hit_term_order_is_preserved_through_the_pool() {
        // Two FTS-matching notes, each hit by a DIFFERENT term in the
        // question (so each term's search_notes call returns exactly one
        // row — no FTS-rank ambiguity), inserted so "match1" is pushed
        // before "match2". This guards term-order determinism through the
        // FTS loop (extract_query_terms → per-term search_notes → push):
        // if the terms were iterated out of order, or a note landed under
        // the wrong term, pool[1] would stop being "match2".
        //
        // NOTE ON SORT STABILITY (spec: fill order, pool.rs sort_by_key
        // comment): this test does NOT distinguish sort_by_key from
        // sort_unstable_by_key. Verified empirically — swapping in
        // sort_unstable_by_key still passes this test, because Source 1
        // (FTS) always finishes entirely before Source 2/3 run, so `out` is
        // already grouped [from_fts=true, from_fts=true, ..., false, ...]
        // BEFORE the sort call; the sort has nothing left to reorder. That
        // architectural fact — not the sort's stability — is what today's
        // fill order actually rests on. sort_by_key is kept anyway as
        // defensive future-proofing (see its comment) in case a future
        // source is ever spliced in ahead of FTS; there is currently no way
        // to construct an input to build_candidate_pool that exercises the
        // stability guarantee, short of unit-testing the sort in isolation
        // on a synthetic (non-pre-grouped) Vec<Candidate>.
        let db = temp_db();
        let a = "acct@x";
        note(a, "match1").title("Quuxaaaa widget").body("x").modified_ms(1).insert(&db);
        note(a, "match2").title("Quuxbbbb widget").body("x").modified_ms(2).insert(&db);
        for i in 0..50 {
            note(a, &format!("n{i}")).title("filler").body("nothing").modified_ms(1_000 + i).insert(&db);
        }

        let pool = build_candidate_pool_capped(
            &db,
            &crate::ask::AskScope::Account { account_id: a.into() },
            "quuxaaaa quuxbbbb",
            3,
            &[],
        )
        .unwrap();

        assert_eq!(pool.len(), 3);
        assert_eq!(pool[0].uuid, "match1", "first term in the question is searched first");
        assert_eq!(
            pool[1].uuid, "match2",
            "second term's match must land right after the first term's, in term order"
        );
    }

    #[test]
    fn folder_scope_is_recursive_and_excludes_prefix_siblings() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "in1").label("Notes/A").insert(&db);
        note(a, "in2").label("Notes/A/B").insert(&db);
        note(a, "out").label("Notes/AB").insert(&db);

        let pool = build_candidate_pool(
            &db,
            &crate::ask::AskScope::Folder { account_id: a.into(), label: "Notes/A".into() },
            "anything",
            &[],
        )
        .unwrap();

        let mut uuids: Vec<&str> = pool.iter().map(|c| c.uuid.as_str()).collect();
        uuids.sort();
        assert_eq!(uuids, vec!["in1", "in2"]);
    }

    #[test]
    fn all_accounts_scope_spans_accounts() {
        let db = temp_db();
        note("a1", "u1").insert(&db);
        note("a2", "u2").insert(&db);

        let pool = build_candidate_pool(&db, &crate::ask::AskScope::AllAccounts, "anything", &[]).unwrap();
        assert_eq!(pool.len(), 2);
    }
}
