//! The knowledge base's own words (spec 2026-11-05 §17): the tags, folder
//! names and note titles of the notes an agent may see. An agent asked about
//! "ระบบขายหน้าร้าน" can write "POS" itself — what it cannot know is that
//! this user's notes say "point-of-sale" and file it under "Retail". This is
//! how it finds out, without sending a note anywhere to be embedded.
//!
//! Closeness is character-trigram similarity after case and Unicode folding,
//! plus containment: no word segmentation, so Thai works as well as English.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

use crate::accounts::Account;
use crate::db::Db;
use crate::mcp_scope::ScopeFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TermKind {
    Tag,
    Folder,
    Title,
}

impl TermKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TermKind::Tag => "tag",
            TermKind::Folder => "folder",
            TermKind::Title => "title",
        }
    }
}

/// One term of the vocabulary.
#[derive(Debug, Clone, Serialize)]
pub struct Term {
    pub term: String,
    pub kind: TermKind,
    /// For a folder: its full path in the first account that has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Visible notes carrying it.
    pub notes: usize,
}

/// A term close to a word, with how close.
#[derive(Debug, Clone, Serialize)]
pub struct CloseTerm {
    #[serde(flatten)]
    pub term: Term,
    pub similarity: f32,
}

pub(crate) fn fold(s: &str) -> String {
    s.nfkc().collect::<String>().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn grams(folded: &str) -> HashSet<String> {
    let padded: Vec<char> = format!("  {folded} ").chars().collect();
    padded.windows(3).map(|w| w.iter().collect()).collect()
}

/// 0..=1. Containment (either way, the shorter at least 2 characters) scores
/// from 0.5 up by how much of the longer it covers; otherwise trigram Jaccard.
pub fn similarity(a: &str, b: &str) -> f32 {
    let (a, b) = (fold(a), fold(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    // Hyphens, underscores and spaces are the same word boundary in tags.
    let squash = |s: &str| s.chars().filter(|c| !matches!(c, '-' | '_' | ' ' | '/')).collect::<String>();
    let (sa, sb) = (squash(&a), squash(&b));
    if !sa.is_empty() && sa == sb {
        return 0.95;
    }
    let (short, long) = if sa.chars().count() <= sb.chars().count() { (&sa, &sb) } else { (&sb, &sa) };
    let jaccard = {
        let (ga, gb) = (grams(&a), grams(&b));
        let inter = ga.intersection(&gb).count() as f32;
        let union = ga.union(&gb).count() as f32;
        if union == 0.0 { 0.0 } else { inter / union }
    };
    if short.chars().count() >= 2 && long.contains(short.as_str()) {
        let cover = short.chars().count() as f32 / long.chars().count() as f32;
        return jaccard.max(0.5 + 0.5 * cover);
    }
    jaccard
}

/// Everything an agent may see in these accounts: active accounts only,
/// nothing from a hidden folder (the MCP read rule, gotcha #33), and not the
/// agent workspace's own pages.
pub fn vocabulary(db: &Db, accounts: &[Account], scope: &ScopeFile) -> Result<Vec<Term>, String> {
    // (kind, folded) → (display, path, notes)
    let mut acc: HashMap<(TermKind, String), (String, Option<String>, HashSet<String>)> = HashMap::new();
    let mut add = |kind: TermKind, display: &str, path: Option<&str>, note: String| {
        let key = (kind, fold(display));
        if key.1.is_empty() {
            return;
        }
        let e = acc.entry(key).or_insert_with(|| (display.to_string(), path.map(str::to_string), HashSet::new()));
        e.2.insert(note);
    };
    for account in accounts.iter().filter(|a| a.is_active()) {
        let visible = |label: &str| {
            !scope.is_hidden(&account.id, label)
                && !crate::folder_scope::matches(label, super::ROOT, crate::folder_scope::Mode::Subtree)
        };
        let notes = db.note_titles(&account.id).map_err(|e| e.to_string())?;
        let mut visible_uuids = HashSet::new();
        for (uuid, title, label) in &notes {
            if !visible(label) {
                continue;
            }
            visible_uuids.insert(uuid.clone());
            let key = format!("{}\u{0}{uuid}", account.id);
            add(TermKind::Title, title, None, key.clone());
            // Every segment below the root, so "Retail" is found inside
            // "Notes/Clients/Retail".
            let segs: Vec<&str> = label.split('/').collect();
            for i in 1..segs.len() {
                add(TermKind::Folder, segs[i], Some(&segs[..=i].join("/")), key.clone());
            }
        }
        for (uuid, tag) in db.note_tag_pairs(&account.id).map_err(|e| e.to_string())? {
            if visible_uuids.contains(&uuid) {
                add(TermKind::Tag, &tag, None, format!("{}\u{0}{uuid}", account.id));
            }
        }
    }
    let mut out: Vec<Term> = acc
        .into_iter()
        .map(|((kind, _), (term, path, notes))| Term { term, kind, path, notes: notes.len() })
        .collect();
    out.sort_by(|a, b| (a.kind.as_str(), &a.term).cmp(&(b.kind.as_str(), &b.term)));
    Ok(out)
}

/// Up to `max` terms closest to `word`, at least `min` similar, best first;
/// ties go to the term more notes carry.
pub fn close_terms(vocab: &[Term], word: &str, min: f32, max: usize, kinds: &[TermKind]) -> Vec<CloseTerm> {
    let mut hits: Vec<CloseTerm> = vocab
        .iter()
        .filter(|t| kinds.contains(&t.kind))
        .map(|t| CloseTerm { term: t.clone(), similarity: similarity(word, &t.term) })
        .filter(|c| c.similarity >= min)
        .collect();
    hits.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.term.notes.cmp(&a.term.notes))
            .then(a.term.term.cmp(&b.term.term))
    });
    hits.truncate(max);
    hits
}

/// Every kind, for `find_terms`.
pub const ALL_KINDS: [TermKind; 3] = [TermKind::Tag, TermKind::Folder, TermKind::Title];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::RemotePin;

    const ACCT: &str = "gmail:a@x.com";

    fn account() -> Account {
        serde_json::from_str(&format!(r#"{{"id":"{ACCT}","email":"a@x.com","added_at":"2026-01-01T00:00:00Z","backend_kind":"gmail"}}"#)).unwrap()
    }

    fn put(db: &Db, uuid: &str, label: &str, title: &str, body: &str) {
        let n = crate::test_support::note(ACCT, uuid).label(label).title(title).body(body).build();
        db.upsert_from_remote(&n, RemotePin::RemoteWins).unwrap();
    }

    #[test]
    fn similarity_handles_case_separators_containment_and_thai() {
        assert_eq!(similarity("POS", "pos"), 1.0);
        assert!(similarity("point of sale", "point-of-sale") >= 0.9);
        assert!(similarity("ขายหน้าร้าน", "ระบบขายหน้าร้าน") >= 0.5, "containment");
        assert!(similarity("inventory", "inventry") > 0.3, "a typo is still close");
        assert!(similarity("inventory", "accounting") < 0.2);
        assert_eq!(similarity("", "x"), 0.0);
    }

    #[test]
    fn the_vocabulary_holds_titles_folder_segments_and_tags_of_visible_notes_only() {
        let db = crate::test_support::temp_db();
        put(&db, "a", "Notes/Clients/Retail", "Stock costing", "FIFO #point-of-sale");
        put(&db, "b", "Notes/Personal", "Diary", "secret #private-tag");
        put(&db, "c", "Notes/__Agent__/Projects", "Jodd", "x");
        let mut scope = ScopeFile::default();
        scope.accounts.insert(ACCT.into(), serde_json::from_value(serde_json::json!({ "hidden_folders": ["Notes/Personal"] })).unwrap());
        let v = vocabulary(&db, &[account()], &scope).unwrap();
        let has = |kind: TermKind, term: &str| v.iter().any(|t| t.kind == kind && t.term == term);
        assert!(has(TermKind::Title, "Stock costing"));
        assert!(has(TermKind::Folder, "Retail") && has(TermKind::Folder, "Clients"));
        assert!(v.iter().any(|t| t.kind == TermKind::Tag && t.term.contains("point-of-sale")), "{v:?}");
        assert!(!v.iter().any(|t| t.term.contains("Diary") || t.term.contains("Personal") || t.term.contains("private")), "hidden stays hidden: {v:?}");
        assert!(!v.iter().any(|t| t.term == "Jodd" || t.term == "__Agent__"), "workspace pages are not vocabulary");
        let close = close_terms(&v, "Point of Sale", 0.5, 3, &ALL_KINDS);
        assert!(close[0].term.term.contains("point-of-sale"), "{close:?}");
    }
}
