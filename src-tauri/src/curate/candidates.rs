//! Deterministic candidates (spec §3.1): no AI, no network — evidence the
//! AI then judges.

use std::collections::{HashMap, HashSet};

use crate::db::{CachedNote, Db};
use crate::mcp_scope::ScopeFile;

/// Text compared for similarity: the first this-many characters.
const SIMILARITY_CHARS: usize = 2_000;
/// Trigram Jaccard at or above this joins two notes.
pub const SIMILARITY: f64 = 0.6;
/// Shorter notes are too generic to call similar by text alone.
const MIN_SIMILAR_CHARS: usize = 200;
pub const MAX_GROUP: usize = 4;
/// Misfiled skips notes touched this recently: probably still being written.
pub const MISFILED_MIN_AGE_MS: i64 = 24 * 60 * 60 * 1000;
pub const UNFILED: [&str; 2] = ["Notes", "Notes/Inbox"];

/// Notes Curate may look at: live, outside hidden folders and outside the
/// agent workspace (agents own that one).
pub fn eligible(db: &Db, scope: &ScopeFile, account_id: &str) -> Result<Vec<CachedNote>, String> {
    Ok(db
        .list_notes(account_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|n| !scope.is_hidden(account_id, &n.label))
        .filter(|n| !crate::folder_scope::matches(&n.label, crate::agent_memory::ROOT, crate::folder_scope::Mode::Subtree))
        .collect())
}

fn text(n: &CachedNote) -> String {
    crate::agent_memory::text::html_to_text(&n.body_html, false)
}

fn trigrams(s: &str) -> HashSet<u64> {
    use std::hash::{Hash, Hasher};
    let norm: Vec<char> = s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().chars().take(SIMILARITY_CHARS).collect();
    norm.windows(3)
        .map(|w| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            w.hash(&mut h);
            h.finish()
        })
        .collect()
}

pub fn jaccard(a: &HashSet<u64>, b: &HashSet<u64>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    inter / (a.len() as f64 + b.len() as f64 - inter)
}

/// One duplicate group and why its members were joined.
#[derive(Debug, Clone)]
pub struct Group {
    pub notes: Vec<CachedNote>,
    pub evidence: Vec<String>,
}

fn find(p: &mut Vec<usize>, i: usize) -> usize {
    let mut r = i;
    while p[r] != r {
        r = p[r];
    }
    let mut c = i;
    while p[c] != r {
        let n = p[c];
        p[c] = r;
        c = n;
    }
    r
}

/// Connected components of "same cited URL", "same normalized title" or
/// "similar text", capped at `MAX_GROUP` (newest kept), largest first.
pub fn duplicate_groups(notes: &[CachedNote], cites: &[(String, String)]) -> Vec<Group> {
    let index: HashMap<&str, usize> = notes.iter().enumerate().map(|(i, n)| (n.uuid.as_str(), i)).collect();
    let mut parent: Vec<usize> = (0..notes.len()).collect();
    let mut why: Vec<Vec<String>> = vec![Vec::new(); notes.len()];
    let mut join = |a: usize, b: usize, reason: String, parent: &mut Vec<usize>, why: &mut Vec<Vec<String>>| {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent[rb] = ra;
        }
        for i in [a, b] {
            if !why[i].contains(&reason) {
                why[i].push(reason.clone());
            }
        }
    };

    let mut by_url: HashMap<&str, Vec<usize>> = HashMap::new();
    for (uuid, url) in cites {
        if let Some(&i) = index.get(uuid.as_str()) {
            by_url.entry(url.as_str()).or_default().push(i);
        }
    }
    for (url, members) in &by_url {
        for w in members.windows(2) {
            join(w[0], w[1], format!("both cite {}", crate::ingest::urls::display_url(url)), &mut parent, &mut why);
        }
    }

    let mut by_title: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, n) in notes.iter().enumerate() {
        let t = crate::agent_memory::normalize(&n.title);
        if !t.is_empty() {
            by_title.entry(t).or_default().push(i);
        }
    }
    for members in by_title.values() {
        for w in members.windows(2) {
            join(w[0], w[1], "same title".into(), &mut parent, &mut why);
        }
    }

    let grams: Vec<Option<HashSet<u64>>> = notes
        .iter()
        .map(|n| {
            let t = text(n);
            (t.chars().count() >= MIN_SIMILAR_CHARS).then(|| trigrams(&t))
        })
        .collect();
    for i in 0..notes.len() {
        let Some(gi) = &grams[i] else { continue };
        for j in (i + 1)..notes.len() {
            let Some(gj) = &grams[j] else { continue };
            // Jaccard ≤ min/max of the set sizes: skip pairs that cannot reach
            // the threshold without touching their sets (review finding 7).
            let (lo, hi) = (gi.len().min(gj.len()) as f64, gi.len().max(gj.len()) as f64);
            if lo / hi < SIMILARITY {
                continue;
            }
            let s = jaccard(gi, gj);
            if s >= SIMILARITY {
                join(i, j, format!("{:.0}% similar text", s * 100.0), &mut parent, &mut why);
            }
        }
    }

    let mut comps: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..notes.len() {
        let r = find(&mut parent, i);
        comps.entry(r).or_default().push(i);
    }
    let mut groups: Vec<Group> = comps
        .into_values()
        .filter(|m| m.len() > 1)
        .map(|mut m| {
            m.sort_by_key(|&i| std::cmp::Reverse(notes[i].last_local_modified_at.max(notes[i].last_remote_modified_at.unwrap_or(0))));
            m.truncate(MAX_GROUP);
            let mut evidence: Vec<String> = Vec::new();
            for &i in &m {
                for e in &why[i] {
                    if !evidence.contains(e) {
                        evidence.push(e.clone());
                    }
                }
            }
            Group { notes: m.iter().map(|&i| notes[i].clone()).collect(), evidence }
        })
        .collect();
    groups.sort_by(|a, b| b.notes.len().cmp(&a.notes.len()).then_with(|| a.notes[0].uuid.cmp(&b.notes[0].uuid)));
    groups
}

/// Notes left in `Notes` or `Notes/Inbox`, not touched in the last day,
/// newest first.
pub fn misfiled(notes: &[CachedNote], now_ms: i64) -> Vec<CachedNote> {
    let mut out: Vec<CachedNote> = notes
        .iter()
        .filter(|n| UNFILED.contains(&n.label.as_str()))
        .filter(|n| now_ms - n.last_local_modified_at.max(n.last_remote_modified_at.unwrap_or(0)) >= MISFILED_MIN_AGE_MS)
        .cloned()
        .collect();
    out.sort_by_key(|n| std::cmp::Reverse(n.last_local_modified_at.max(n.last_remote_modified_at.unwrap_or(0))));
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::SyncState;

    pub fn note(uuid: &str, title: &str, label: &str, body: &str, modified: i64) -> CachedNote {
        CachedNote {
            uuid: uuid.into(), account_id: "a".into(), id: String::new(), title: title.into(), body_html: body.into(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".into(), x_mail_created_date: None, label: label.into(), local_version: 1,
            remote_version: None, sync_state: SyncState::Clean, last_synced_at: None, last_local_modified_at: modified,
            last_remote_modified_at: None, pinned: false, meta_msg_id: None, pin_dirty: false, push_blocked_reason: None,
            push_blocked_by_remote: false,
        }
    }

    fn long(seed: &str) -> String {
        format!("<div>{}</div>", (0..60).map(|i| format!("{seed} sentence number {i} about the topic.")).collect::<Vec<_>>().join(" "))
    }

    #[test]
    fn a_shared_cited_url_groups_notes() {
        let notes = vec![note("A", "One", "Notes", "<div>a</div>", 2), note("B", "Two", "Notes", "<div>b</div>", 1), note("C", "Three", "Notes", "<div>c</div>", 3)];
        let cites = vec![("A".into(), "https://www.youtube.com/watch?v=QnHWXRVxKlQ".into()), ("B".into(), "https://www.youtube.com/watch?v=QnHWXRVxKlQ".into())];
        let g = duplicate_groups(&notes, &cites);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].notes.iter().map(|n| n.uuid.as_str()).collect::<Vec<_>>(), ["A", "B"]);
        assert!(g[0].evidence[0].starts_with("both cite "), "{:?}", g[0].evidence);
    }

    #[test]
    fn equal_titles_and_similar_text_group_but_different_text_does_not() {
        let notes = vec![
            note("A", "Weekly plan", "Notes", "<div>x</div>", 1),
            note("B", "  weekly   PLAN ", "Notes/Work", "<div>y</div>", 2),
            note("C", "Draft", "Notes", &long("alpha"), 3),
            note("D", "Draft copy", "Notes", &long("alpha"), 4),
            note("E", "Other", "Notes", &long("completely different words zebra"), 5),
        ];
        let g = duplicate_groups(&notes, &[]);
        let sets: Vec<Vec<&str>> = g.iter().map(|g| { let mut v: Vec<&str> = g.notes.iter().map(|n| n.uuid.as_str()).collect(); v.sort(); v }).collect();
        assert!(sets.contains(&vec!["A", "B"]), "{sets:?}");
        assert!(sets.contains(&vec!["C", "D"]), "{sets:?}");
        assert!(!sets.iter().any(|s| s.contains(&"E")), "{sets:?}");
    }

    #[test]
    fn a_group_is_capped_at_its_newest_members() {
        let notes: Vec<CachedNote> = (0..6).map(|i| note(&format!("N{i}"), "Same", "Notes", "<div>x</div>", i)).collect();
        let g = duplicate_groups(&notes, &[]);
        assert_eq!(g[0].notes.len(), MAX_GROUP);
        assert_eq!(g[0].notes[0].uuid, "N5");
    }

    #[test]
    fn short_notes_are_never_similar_by_text_alone() {
        let notes = vec![note("A", "a", "Notes", "<div>ok</div>", 1), note("B", "b", "Notes", "<div>ok</div>", 2)];
        assert!(duplicate_groups(&notes, &[]).is_empty());
    }

    #[test]
    fn jaccard_is_the_plain_ratio() {
        let a: HashSet<u64> = [1, 2, 3, 4].into();
        let b: HashSet<u64> = [3, 4, 5, 6].into();
        assert!((jaccard(&a, &b) - 2.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn misfiled_is_only_unfiled_and_not_brand_new() {
        let day = MISFILED_MIN_AGE_MS;
        let now = 10 * day;
        let notes = vec![
            note("A", "a", "Notes", "", now - 2 * day),
            note("B", "b", "Notes/Inbox", "", now - 3 * day),
            note("C", "c", "Notes/Inbox", "", now - day / 2),
            note("D", "d", "Notes/Work", "", 0),
        ];
        assert_eq!(misfiled(&notes, now).iter().map(|n| n.uuid.as_str()).collect::<Vec<_>>(), ["A", "B"]);
    }

    #[test]
    fn eligible_drops_hidden_workspace_and_deleted() {
        let db = crate::test_support::temp_db();
        for (u, l) in [("A", "Notes"), ("H", "Notes/Private/x"), ("W", "Notes/__Agent__/Projects")] {
            let mut n = note(u, u, l, "", 1);
            n.account_id = "gmail:a@x.com".into();
            db.insert_local_new(&n, &crate::provenance::Actor::test()).unwrap();
        }
        let mut d = note("D", "D", "Notes", "", 1);
        d.account_id = "gmail:a@x.com".into();
        db.insert_local_new(&d, &crate::provenance::Actor::test()).unwrap();
        db.mark_deleted("D", "gmail:a@x.com").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        std::fs::write(&p, r#"{"accounts":{"gmail:a@x.com":{"hidden_folders":["Notes/Private"]}}}"#).unwrap();
        let scope = crate::mcp_scope::load_from(&p).unwrap();
        let e = eligible(&db, &scope, "gmail:a@x.com").unwrap();
        assert_eq!(e.iter().map(|n| n.uuid.as_str()).collect::<Vec<_>>(), ["A"]);
    }
}
