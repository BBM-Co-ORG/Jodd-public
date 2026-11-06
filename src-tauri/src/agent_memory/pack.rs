//! `start_project` (spec 2026-11-05 §16): one call that gathers what a new
//! project needs from the knowledge base — the notes that match the brief,
//! the notes they link to, the project's page and skills, and the user's
//! preferences.
//!
//! Jodd does not guess what the brief means: the calling agent passes
//! keywords in every language the notes may use (D13), and Jodd widens each
//! with the knowledge base's own close tags and folder names (`vocab`, §17).
//! It ranks by how many different keywords a note matches (D14).
//!
//! Read-only, and exactly the MCP read rule (D17): active accounts, no hidden
//! folder, and the agent workspace's pages only as the project page and
//! preferences, never as matches.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;

use super::text::html_to_text;
use super::vocab::{self, TermKind};
use super::{find_page, ABOUT_ME, PROJECTS, ROOT};
use crate::accounts::Account;
use crate::db::{CachedNote, Db};
use crate::mcp_scope::ScopeFile;
use crate::provenance::Tier;

pub const MAX_KEYWORDS: usize = 30;
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 40;
/// Notes one link away, after the matches.
const RELATED_MAX: usize = 10;
/// How many of the best matches are followed for links.
const RELATED_FROM: usize = 10;
/// §17.2: widen a keyword with tags and folder names this close…
const WIDEN_MIN: f32 = 0.5;
/// …at most this many per keyword.
const WIDEN_MAX: usize = 3;

pub struct PackRequest<'a> {
    pub brief: &'a str,
    pub keywords: &'a [String],
    pub project: Option<&'a str>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectPage {
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackNote {
    pub account_id: String,
    pub uuid: String,
    pub title: String,
    pub folder: String,
    pub summary: String,
    /// Which keywords found it; a widened one reads `pos→tag:point-of-sale`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matched: Vec<String>,
    pub trust: Tier,
    /// The note's file in the mirror, when the mirror holds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pack {
    pub project: Option<ProjectPage>,
    pub project_skills: Vec<String>,
    pub preferences: Vec<String>,
    pub notes: Vec<PackNote>,
    pub related: Vec<PackNote>,
    pub next: String,
}

const NEXT: &str = "Read the notes you will rely on in full (read_note, or the file) before relying on them; prefer human_written and human_reviewed over unreviewed. Follow project_skills for the work they cover. Save what you decide with remember.";

/// One search term and the keyword it stands for.
struct Query {
    text: String,
    /// What `matched` shows when this term finds a note.
    label: String,
    /// The original keyword, for counting distinct keywords.
    keyword: String,
}

fn queries(db: &Db, accounts: &[Account], scope: &ScopeFile, keywords: &[String]) -> Result<Vec<Query>, String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    let vocab = vocab::vocabulary(db, accounts, scope)?;
    for k in keywords {
        let k = k.split_whitespace().collect::<Vec<_>>().join(" ");
        let folded = vocab::fold(&k);
        if folded.is_empty() || !seen.insert(folded.clone()) {
            continue;
        }
        out.push(Query { text: k.clone(), label: folded.clone(), keyword: folded.clone() });
        for c in vocab::close_terms(&vocab, &k, WIDEN_MIN, WIDEN_MAX, &[TermKind::Tag, TermKind::Folder]) {
            let t = vocab::fold(&c.term.term);
            if t == folded {
                continue;
            }
            out.push(Query { text: c.term.term.clone(), label: format!("{folded}→{}:{}", c.term.kind.as_str(), c.term.term), keyword: folded.clone() });
        }
    }
    Ok(out)
}

fn summary(note: &CachedNote) -> String {
    let title = vocab::fold(&note.title);
    html_to_text(&note.body_html, false)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && vocab::fold(l) != title)
        .map(|l| {
            let one: String = l.split_whitespace().collect::<Vec<_>>().join(" ");
            if one.chars().count() > 160 { format!("{}…", one.chars().take(159).collect::<String>()) } else { one }
        })
        .unwrap_or_default()
}

fn modified(n: &CachedNote) -> i64 {
    n.last_local_modified_at.max(n.last_remote_modified_at.unwrap_or(0))
}

/// Build the pack. Errors only on a request it cannot serve (no keyword) or a
/// database failure.
pub fn project_pack(db: &Db, accounts: &[Account], scope: &ScopeFile, req: &PackRequest) -> Result<Pack, String> {
    let keywords: Vec<String> = req.keywords.iter().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
    if keywords.is_empty() {
        return Err("Give at least one keyword: the words the notes would use, in every language they may be written in.".into());
    }
    if keywords.len() > MAX_KEYWORDS {
        return Err(format!("At most {MAX_KEYWORDS} keywords."));
    }
    let limit = req.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let active: Vec<&Account> = accounts.iter().filter(|a| a.is_active()).collect();
    let inactive: Vec<String> = accounts.iter().filter(|a| !a.is_active()).map(|a| a.id.clone()).collect();
    let visible = |n: &CachedNote| {
        active.iter().any(|a| a.id == n.account_id)
            && !scope.is_hidden(&n.account_id, &n.label)
            && !crate::folder_scope::matches(&n.label, ROOT, crate::folder_scope::Mode::Subtree)
    };

    // Score every match: distinct keywords first, title hits second.
    struct Hit {
        note: CachedNote,
        keywords: BTreeSet<String>,
        labels: Vec<String>,
        title_hits: usize,
    }
    let mut hits: HashMap<(String, String), Hit> = HashMap::new();
    for q in queries(db, accounts, scope, &keywords)? {
        let rows = db.search_notes(None, None, &q.text, &inactive).map_err(|e| format!("search: {e}"))?;
        let folded = vocab::fold(&q.text);
        for n in rows.into_iter().filter(|n| visible(n)) {
            let title_hit = vocab::fold(&n.title).contains(&folded);
            let h = hits.entry((n.account_id.clone(), n.uuid.clone())).or_insert_with(|| Hit {
                note: n,
                keywords: BTreeSet::new(),
                labels: Vec::new(),
                title_hits: 0,
            });
            h.keywords.insert(q.keyword.clone());
            if !h.labels.contains(&q.label) {
                h.labels.push(q.label.clone());
            }
            if title_hit {
                h.title_hits += 1;
            }
        }
    }
    let mut ranked: Vec<Hit> = hits.into_values().collect();
    ranked.sort_by(|a, b| {
        (b.keywords.len() * 3 + b.title_hits)
            .cmp(&(a.keywords.len() * 3 + a.title_hits))
            .then(modified(&b.note).cmp(&modified(&a.note)))
            .then(a.note.uuid.cmp(&b.note.uuid))
    });
    ranked.truncate(limit);

    // One link away from the best matches.
    let taken: HashSet<(String, String)> = ranked.iter().map(|h| (h.note.account_id.clone(), h.note.uuid.clone())).collect();
    let mut related: Vec<CachedNote> = Vec::new();
    let mut related_seen: HashSet<(String, String)> = HashSet::new();
    'outer: for h in ranked.iter().take(RELATED_FROM) {
        let (acct, uuid) = (&h.note.account_id, &h.note.uuid);
        let mut near = db.outgoing_links(acct, uuid).map_err(|e| e.to_string())?;
        near.extend(db.backlinks(acct, uuid).map_err(|e| e.to_string())?);
        for n in near.into_iter().filter(|n| visible(n)) {
            let key = (n.account_id.clone(), n.uuid.clone());
            if taken.contains(&key) || !related_seen.insert(key) {
                continue;
            }
            related.push(n);
            if related.len() >= RELATED_MAX {
                break 'outer;
            }
        }
    }

    // Trust and mirror files for everything returned.
    let mut trust: HashMap<(String, String), Tier> = HashMap::new();
    let mut by_account: HashMap<String, Vec<String>> = HashMap::new();
    for n in ranked.iter().map(|h| &h.note).chain(related.iter()) {
        by_account.entry(n.account_id.clone()).or_default().push(n.uuid.clone());
    }
    for (acct, uuids) in &by_account {
        for (u, t) in db.trust_for(acct, uuids).map_err(|e| e.to_string())? {
            trust.insert((acct.clone(), u), t.tier);
        }
    }
    let files = if scope.mirror().enabled {
        crate::agent_files::mirror_root(scope.mirror())
            .map(|r| crate::agent_files::mirror::note_files(&r))
            .unwrap_or_default()
    } else {
        HashMap::new()
    };
    let pack_note = |n: &CachedNote, matched: Vec<String>| {
        let key = (n.account_id.clone(), n.uuid.clone());
        PackNote {
            account_id: n.account_id.clone(),
            uuid: n.uuid.clone(),
            title: n.title.clone(),
            folder: n.label.clone(),
            summary: summary(n),
            matched,
            trust: trust.get(&key).copied().unwrap_or(Tier::NotRecorded),
            file: files.get(&key).map(|p| p.display().to_string()),
        }
    };
    let notes: Vec<PackNote> = ranked.iter().map(|h| pack_note(&h.note, h.labels.clone())).collect();
    let related: Vec<PackNote> = related.iter().map(|n| pack_note(n, Vec::new())).collect();

    // The project page, its skills, and the user's preferences, from the
    // agent workspace when one is enabled and visible.
    let (mut project, mut project_skills, mut preferences) = (None, Vec::new(), Vec::new());
    if let Some(ws) = scope.workspace_account().filter(|id| active.iter().any(|a| a.id == *id) && !scope.is_hidden(id, ROOT)) {
        if let Some(name) = req.project.map(str::trim).filter(|p| !p.is_empty()) {
            if let Some(page) = find_page(db, ws, PROJECTS, name)? {
                let text = html_to_text(&page.body_html, false);
                project_skills = super::skill_names(&text);
                project = Some(ProjectPage { title: page.title, text });
            }
        }
        if let Some(about) = find_page(db, ws, ROOT, ABOUT_ME)? {
            preferences = html_to_text(&about.body_html, false)
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("- ["))
                .map(str::to_string)
                .collect();
        }
    }
    let _ = req.brief; // The brief is the agent's; Jodd ranks by its keywords (D13).
    Ok(Pack { project, project_skills, preferences, notes, related, next: NEXT.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_memory::{remember, Kind, RememberRequest};
    use crate::db::RemotePin;

    const ACCT: &str = "gmail:a@x.com";

    fn account() -> Account {
        serde_json::from_str(&format!(r#"{{"id":"{ACCT}","email":"a@x.com","added_at":"2026-01-01T00:00:00Z","backend_kind":"gmail"}}"#)).unwrap()
    }

    fn put(db: &Db, uuid: &str, label: &str, title: &str, body: &str) {
        let n = crate::test_support::note(ACCT, uuid).label(label).title(title).body(body).build();
        db.upsert_from_remote(&n, RemotePin::RemoteWins).unwrap();
    }

    fn scope(json: serde_json::Value) -> ScopeFile {
        serde_json::from_value(json).unwrap()
    }

    fn kw(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn req<'a>(keywords: &'a [String], project: Option<&'a str>) -> PackRequest<'a> {
        PackRequest { brief: "Build a POS and ERP for a shop", keywords, project, limit: None }
    }

    fn kb() -> Db {
        let db = crate::test_support::temp_db();
        put(&db, "costing", "Notes/ERP", "Stock costing", "FIFO and moving average for inventory valuation.");
        put(&db, "vat", "Notes/ERP/Tax", "Thai VAT", "VAT 7% and e-Tax invoice rules for inventory sales. See [[Stock costing]]");
        put(&db, "pos", "Notes/Retail", "Till offline mode", "A POS must keep selling offline.");
        put(&db, "cake", "Notes/Recipes", "Cake", "Flour and eggs.");
        put(&db, "diary", "Notes/Personal", "Diary", "inventory of my feelings");
        db.record_full_listing(ACCT).unwrap();
        db
    }

    #[test]
    fn notes_matching_more_keywords_rank_first_and_unrelated_ones_stay_out() {
        let db = kb();
        let k = kw(&["inventory", "VAT", "e-Tax"]);
        let p = project_pack(&db, &[account()], &scope(serde_json::json!({})), &req(&k, None)).unwrap();
        let titles: Vec<&str> = p.notes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles.first(), Some(&"Thai VAT"), "{titles:?}");
        assert!(titles.contains(&"Stock costing"));
        assert!(!titles.contains(&"Cake"));
        assert!(p.notes[0].matched.contains(&"vat".to_string()), "{:?}", p.notes[0].matched);
        assert!(!p.notes[0].summary.is_empty());
    }

    #[test]
    fn hidden_folders_inactive_accounts_and_workspace_pages_never_appear() {
        let db = kb();
        put(&db, "ws", "Notes/__Agent__/Projects", "Inventory project", "inventory");
        let k = kw(&["inventory"]);
        let s = scope(serde_json::json!({ "accounts": { ACCT: { "hidden_folders": ["Notes/Personal"] } } }));
        let p = project_pack(&db, &[account()], &s, &req(&k, None)).unwrap();
        assert!(!p.notes.iter().any(|n| n.title == "Diary" || n.title == "Inventory project"), "{:?}", p.notes);
        let mut off = account();
        off.status = crate::accounts::AccountStatus::Inactive;
        assert!(project_pack(&db, &[off], &s, &req(&k, None)).unwrap().notes.is_empty());
    }

    #[test]
    fn linked_notes_come_as_related_and_a_keyword_is_widened_by_the_notes_own_folder_names() {
        let db = kb();
        let k = kw(&["e-Tax"]);
        let p = project_pack(&db, &[account()], &scope(serde_json::json!({})), &req(&k, None)).unwrap();
        assert_eq!(p.notes.iter().map(|n| n.title.as_str()).collect::<Vec<_>>(), ["Thai VAT"]);
        assert!(p.related.iter().any(|n| n.title == "Stock costing"), "{:?}", p.related);

        let k = kw(&["retail shop"]);
        let p = project_pack(&db, &[account()], &scope(serde_json::json!({})), &req(&k, None)).unwrap();
        let hit = p.notes.iter().find(|n| n.title == "Till offline mode").expect("found through the folder name");
        assert!(hit.matched.iter().any(|m| m.contains("→folder:Retail")), "{:?}", hit.matched);
    }

    #[test]
    fn the_project_page_its_skills_and_preferences_come_from_the_workspace() {
        let db = kb();
        let a = account();
        let r = |kind, text, project| RememberRequest { kind, text, project, area: None, new_area: false, relates_to: &[], client: "claude-code", actor: &crate::provenance::Actor::Process("test"), date: "2026-11-05" };
        remember(&db, &a, &r(Kind::Skill, "jodd-brand-shop for every visual", Some("Shop POS"))).unwrap();
        remember(&db, &a, &r(Kind::Decision, "Use moving average costing", Some("Shop POS"))).unwrap();
        remember(&db, &a, &r(Kind::Preference, "Thai first, English second", None)).unwrap();
        assert!(remember(&db, &a, &r(Kind::Skill, "brand-shop", Some("Shop POS"))).is_err(), "must name jodd-…");
        assert!(remember(&db, &a, &r(Kind::Skill, "jodd-Brand", Some("Shop POS"))).is_err(), "a valid name");
        assert!(remember(&db, &a, &r(Kind::Skill, "jodd-brand-shop", None)).is_err(), "only on a project page");
        let s = scope(serde_json::json!({ "agent_workspace": { "account_id": ACCT } }));
        let k = kw(&["costing"]);
        let p = project_pack(&db, &[a], &s, &req(&k, Some("shop pos"))).unwrap();
        assert_eq!(p.project.as_ref().map(|x| x.title.as_str()), Some("Shop POS"));
        assert!(p.project.unwrap().text.contains("moving average"));
        assert_eq!(p.project_skills, ["jodd-brand-shop"]);
        assert!(p.preferences.iter().any(|l| l.contains("Thai first")), "{:?}", p.preferences);
        assert!(!p.notes.iter().any(|n| n.folder.starts_with(ROOT)), "workspace pages are not matches");
    }

    #[test]
    fn a_mirrored_note_points_at_its_file() {
        let db = kb();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        let s = scope(serde_json::json!({ "agent_files": { "mirror": { "enabled": true, "path": root, "accounts": [ACCT] } } }));
        crate::agent_files::mirror::sync_mirror(&db, &root, &[account()], &s, &crate::agent_files::SyncOptions::default()).unwrap();
        let k = kw(&["FIFO"]);
        let p = project_pack(&db, &[account()], &s, &req(&k, None)).unwrap();
        let file = p.notes[0].file.as_deref().expect("mirrored");
        assert!(std::fs::read_to_string(file).unwrap().contains("FIFO"));
    }

    #[test]
    fn keywords_are_required_and_bounded() {
        let db = kb();
        let s = scope(serde_json::json!({}));
        assert!(project_pack(&db, &[account()], &s, &req(&kw(&[" "]), None)).is_err());
        let many: Vec<String> = (0..=MAX_KEYWORDS).map(|i| format!("k{i}")).collect();
        assert!(project_pack(&db, &[account()], &s, &req(&many, None)).is_err());
    }
}
