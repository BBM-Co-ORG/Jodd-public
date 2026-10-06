//! The AI step and the whole scan (spec §3.3). Jodd's own configured
//! provider judges what `candidates` found; secrets need no AI.

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{candidates, propose, secrets, Action, Kind, NoteRef, Payload, MAX_PER_KIND};
use crate::accounts::Account;
use crate::backend::{Capabilities, Write};
use crate::db::{CachedNote, Db};
use crate::llm::provider::{ChatRole, ChatTurn, ExtractError, LlmProvider};
use crate::mcp_scope::ScopeFile;

pub const SOURCE: &str = "jodd-ai";
const NOTE_CHARS: usize = 3_000;

#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct ScanSummary {
    pub duplicates: usize,
    pub misfiled: usize,
    pub secrets: usize,
    /// Candidates the AI could not judge (failure, budget, unreadable answer).
    pub skipped: usize,
    /// Plain-language notes for the summary line, e.g. no AI configured.
    pub notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Verdict {
    verdict: String,
    #[serde(default)]
    keep: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

fn excerpt(n: &CachedNote) -> String {
    crate::agent_memory::text::html_to_text(&n.body_html, false).chars().take(NOTE_CHARS).collect()
}

fn group_message(group: &candidates::Group, cites: &[(String, String)]) -> String {
    let notes: Vec<serde_json::Value> = group
        .notes
        .iter()
        .map(|n| {
            let urls: Vec<String> = cites.iter().filter(|(u, _)| u == &n.uuid).map(|(_, url)| crate::ingest::urls::display_url(url)).collect();
            serde_json::json!({
                "uuid": n.uuid, "title": n.title, "folder": n.label, "date": n.date,
                "characters": crate::agent_memory::text::html_to_text(&n.body_html, false).chars().count(),
                "cites": urls, "text_start": excerpt(n),
            })
        })
        .collect();
    serde_json::json!({ "evidence": group.evidence, "notes": notes }).to_string()
}

/// One group → a duplicate proposal, or `None` for related/distinct.
pub async fn judge_group(provider: &dyn LlmProvider, group: &candidates::Group, cites: &[(String, String)], cancel: CancellationToken) -> Result<Option<Payload>, ExtractError> {
    let reply = provider
        .chat(crate::llm::prompt::CURATE_DUPLICATES_PROMPT, &[ChatTurn { role: ChatRole::User, content: group_message(group, cites) }], cancel)
        .await?;
    let v: Verdict = crate::llm::provider::parse_envelope_lenient(&reply).map_err(|reason| ExtractError::MalformedEnvelope { reason, raw: reply.chars().take(200).collect() })?;
    if v.verdict != "duplicate" {
        return Ok(None);
    }
    let keep = v
        .keep
        .filter(|k| group.notes.iter().any(|n| &n.uuid == k))
        .unwrap_or_else(|| group.notes[0].uuid.clone());
    // An image in a note to be appended would break (apply refuses it), so
    // propose keep for such groups instead of a card that cannot succeed.
    let has_image = group.notes.iter().any(|n| n.uuid != keep && n.body_html.contains("cid:"));
    let action = if v.method.as_deref() == Some("append") && !has_image { Action::Append { into: keep } } else { Action::Keep { keep } };
    Ok(Some(Payload {
        notes: group.notes.iter().map(NoteRef::of).collect(),
        action,
        reason: v.reason.unwrap_or_else(|| "These notes look like duplicates.".into()),
        evidence: group.evidence.clone(),
    }))
}

/// Folders a note may be moved to: not unfiled, not hidden, not the agent workspace.
fn destinations(db: &Db, scope: &ScopeFile, account_id: &str) -> Result<Vec<String>, String> {
    Ok(db
        .list_folders(account_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|f| f.path)
        .filter(|p| !candidates::UNFILED.contains(&p.as_str()))
        .filter(|p| !scope.is_hidden(account_id, p))
        .filter(|p| !crate::folder_scope::matches(p, crate::agent_memory::ROOT, crate::folder_scope::Mode::Subtree))
        .collect())
}

/// Find, judge and store proposals for one account. `provider: None` (no AI
/// configured) still scans for secrets.
pub async fn scan(db: &Db, provider: Option<&dyn LlmProvider>, scope: &ScopeFile, account: &Account, now_ms: i64, cancel: CancellationToken) -> Result<ScanSummary, String> {
    let mut s = ScanSummary::default();
    let all = candidates::eligible(db, scope, &account.id)?;
    let caps = Capabilities::for_backend(account.backend_kind).writes;

    // Secrets first: no AI, and the scan that decides what the AI may see.
    // A note holding a secret is never sent to the AI below — finding the
    // password and then shipping it to a model provider would undo the point.
    let mut with_secrets: std::collections::HashSet<String> = std::collections::HashSet::new();
    let found_by_note: Vec<(&CachedNote, Vec<secrets::Finding>)> = all
        .iter()
        .map(|n| (n, secrets::scan(&format!("{}\n{}", n.title, crate::agent_memory::text::html_to_text(&n.body_html, true)))))
        .filter(|(_, f)| !f.is_empty())
        .collect();
    for (n, _) in &found_by_note {
        with_secrets.insert(n.uuid.clone());
    }
    let notes: Vec<CachedNote> = all.iter().filter(|n| !with_secrets.contains(&n.uuid)).cloned().collect();
    // Review finding 5: offer Hide only where apply can do it — an existing
    // hidden folder, or the right to create `Notes/Private`.
    let can_hide = caps.allows(Write::Relocate)
        && (caps.allows(Write::Folders) || scope.hidden_folders(&account.id).iter().any(|h| db.get_folder(&account.id, h).ok().flatten().is_some()));
    if can_hide {
        for (n, found) in found_by_note {
            if s.secrets >= MAX_PER_KIND {
                break;
            }
            let kinds: Vec<&str> = found.iter().map(|f| f.kind).collect();
            let found = found.clone();
            let payload = Payload {
                notes: vec![NoteRef::of(n)],
                action: Action::Hide,
                reason: format!("This note holds what looks like a {} in plain text, readable by any agent connected to Jodd.", kinds.join(", ")),
                evidence: found.into_iter().map(|f| f.masked).collect(),
            };
            if propose(db, &account.id, Kind::Secret, &payload, SOURCE)? {
                s.secrets += 1;
            }
        }
    }

    let Some(provider) = provider else {
        s.notes.push("No AI provider is set up, so only secrets were checked. Set one in Settings → LLM provider to find duplicates and misfiled notes.".into());
        return Ok(s);
    };

    let can_trash = caps.allows(Write::Relocate) && Capabilities::for_backend(account.backend_kind).has_trash;
    if !can_trash {
        s.notes.push("This account deletes notes permanently, so Organize does not look for duplicates in it.".into());
    }
    if can_trash {
        let cites = db.cited_urls(&account.id).map_err(|e| e.to_string())?;
        for group in candidates::duplicate_groups(&notes, &cites) {
            if s.duplicates >= MAX_PER_KIND || cancel.is_cancelled() {
                break;
            }
            let key = super::note_key(&group.notes.iter().map(NoteRef::of).collect::<Vec<_>>());
            if db.curate_list(&account.id, &["pending", "dismissed"]).map_err(|e| e.to_string())?.iter().any(|r| serde_json::from_str::<Payload>(&r.2).is_ok_and(|p| super::note_key(&p.notes) == key)) {
                continue;
            }
            match judge_group(provider, &group, &cites, cancel.clone()).await {
                Ok(Some(payload)) => {
                    if propose(db, &account.id, Kind::Duplicate, &payload, SOURCE)? {
                        s.duplicates += 1;
                    }
                }
                Ok(None) => {}
                Err(ExtractError::Cancelled) => return Err("cancelled".into()),
                Err(e) => {
                    crate::log!("curate: a duplicate group was not judged: {}", e.to_string().chars().take(200).collect::<String>());
                    s.skipped += 1;
                }
            }
        }

    }
    if caps.allows(Write::Relocate) {
        let folders = destinations(db, scope, &account.id)?;
        if !folders.is_empty() {
            for n in candidates::misfiled(&notes, now_ms) {
                if s.misfiled >= MAX_PER_KIND || cancel.is_cancelled() {
                    break;
                }
                let text = format!("{}\n\n{}", n.title, excerpt(&n));
                match provider.suggest_folder(&text, &folders, cancel.clone()).await {
                    Ok(env) => {
                        let Some(to) = env.folder.filter(|f| folders.contains(f) && f != &n.label) else { continue };
                        let payload = Payload {
                            notes: vec![NoteRef::of(&n)],
                            action: Action::Move { to: to.clone() },
                            reason: env.reason.unwrap_or_else(|| format!("This note fits {to}.")),
                            evidence: vec![format!("left in {}", n.label)],
                        };
                        if propose(db, &account.id, Kind::Misfiled, &payload, SOURCE)? {
                            s.misfiled += 1;
                        }
                    }
                    Err(ExtractError::Cancelled) => return Err("cancelled".into()),
                    Err(e) => {
                        crate::log!("curate: a misfiled note was not judged: {}", e.to_string().chars().take(200).collect::<String>());
                        s.skipped += 1;
                    }
                }
            }
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curate::{candidates::tests::note, list};
    use crate::llm::provider::{CandidateSummary, ExtractEnvelope, FolderSuggestionEnvelope, LinkSuggestionsEnvelope, SourceDigest, WorkflowKind};
    use std::sync::Mutex;

    const A: &str = "gmail:a@x.com";

    struct Fake {
        verdicts: Mutex<Vec<Result<String, ExtractError>>>,
        folder: Option<String>,
        chats: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for Fake {
        async fn extract(&self, _s: &str, _t: &[String], _c: CancellationToken) -> Result<ExtractEnvelope, ExtractError> { unreachable!() }
        async fn run_workflow(&self, _w: WorkflowKind, _s: &str, _t: &[String], _c: CancellationToken) -> Result<ExtractEnvelope, ExtractError> { unreachable!() }
        async fn synthesize(&self, _d: &[SourceDigest], _c: &str, _t: CancellationToken) -> Result<ExtractEnvelope, ExtractError> { unreachable!() }
        async fn suggest_links(&self, _s: &str, _c: &[CandidateSummary], _t: CancellationToken) -> Result<LinkSuggestionsEnvelope, ExtractError> { unreachable!() }
        async fn suggest_folder(&self, _t: &str, _f: &[String], _c: CancellationToken) -> Result<FolderSuggestionEnvelope, ExtractError> {
            Ok(FolderSuggestionEnvelope { folder: self.folder.clone(), reason: Some("It is about work.".into()) })
        }
        async fn chat(&self, _s: &str, turns: &[ChatTurn], _c: CancellationToken) -> Result<String, ExtractError> {
            self.chats.lock().unwrap().push(turns[0].content.clone());
            self.verdicts.lock().unwrap().remove(0)
        }
    }

    fn fake(verdicts: Vec<Result<String, ExtractError>>, folder: Option<&str>) -> Fake {
        Fake { verdicts: Mutex::new(verdicts), folder: folder.map(String::from), chats: Mutex::new(vec![]) }
    }
    fn acct() -> Account {
        serde_json::from_str(&format!(r#"{{"id":"{A}","email":"a@x.com","added_at":"2026-01-01T00:00:00Z"}}"#)).unwrap()
    }
    fn put(db: &Db, uuid: &str, title: &str, label: &str, body: &str) {
        let mut n = note(uuid, title, label, body, 0);
        n.account_id = A.into();
        db.insert_local_new(&n).unwrap();
    }
    fn scope() -> ScopeFile {
        ScopeFile::default()
    }
    const NOW: i64 = 100 * candidates::MISFILED_MIN_AGE_MS;

    #[tokio::test]
    async fn a_duplicate_verdict_becomes_a_proposal_with_the_chosen_note_and_method() {
        let db = crate::test_support::temp_db();
        put(&db, "A", "Weekly plan", "Notes/Work", "<div>a</div>");
        put(&db, "B", "weekly plan", "Notes/Work", "<div>b</div>");
        let p = fake(vec![Ok(r#"{"verdict":"duplicate","keep":"B","method":"append","reason":"Same plan twice."}"#.into())], None);
        let s = scan(&db, Some(&p), &scope(), &acct(), NOW, CancellationToken::new()).await.unwrap();
        assert_eq!(s.duplicates, 1);
        let prop = &list(&db, A, &["pending"]).unwrap()[0];
        assert_eq!(prop.payload.action, Action::Append { into: "B".into() });
        assert_eq!(prop.payload.reason, "Same plan twice.");
        assert!(prop.payload.evidence.contains(&"same title".to_string()));
        assert!(p.chats.lock().unwrap()[0].contains("\"evidence\""));
    }

    #[tokio::test]
    async fn related_distinct_and_failures_make_no_proposal() {
        let db = crate::test_support::temp_db();
        for (u, t) in [("A", "X"), ("B", "x"), ("C", "Y"), ("D", "y"), ("E", "Z"), ("F", "z")] {
            put(&db, u, t, "Notes/Work", "<div>.</div>");
        }
        let p = fake(vec![
            Ok(r#"{"verdict":"related","keep":null,"method":null,"reason":"r"}"#.into()),
            Ok("I think these are distinct.".into()),
            Err(ExtractError::Transport("down".into())),
        ], None);
        let s = scan(&db, Some(&p), &scope(), &acct(), NOW, CancellationToken::new()).await.unwrap();
        assert_eq!((s.duplicates, s.skipped), (0, 2), "{s:?}");
        assert!(list(&db, A, &["pending"]).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_keep_outside_the_group_falls_back_to_its_newest_note() {
        let db = crate::test_support::temp_db();
        put(&db, "A", "Plan", "Notes/Work", "<div>a</div>");
        put(&db, "B", "plan", "Notes/Work", "<div>b</div>");
        let p = fake(vec![Ok(r#"{"verdict":"duplicate","keep":"ZZZ","method":"keep","reason":"r"}"#.into())], None);
        scan(&db, Some(&p), &scope(), &acct(), NOW, CancellationToken::new()).await.unwrap();
        let prop = &list(&db, A, &["pending"]).unwrap()[0];
        assert!(matches!(&prop.payload.action, Action::Keep { keep } if keep == "A" || keep == "B"));
    }

    #[tokio::test]
    async fn misfiled_notes_get_a_move_to_an_existing_folder() {
        let db = crate::test_support::temp_db();
        db.create_folder_local_new(A, "Notes/Work").unwrap();
        put(&db, "N", "Quarterly numbers", "Notes", "<div>revenue</div>");
        let p = fake(vec![], Some("Notes/Work"));
        let s = scan(&db, Some(&p), &scope(), &acct(), NOW, CancellationToken::new()).await.unwrap();
        assert_eq!(s.misfiled, 1);
        assert_eq!(list(&db, A, &["pending"]).unwrap()[0].payload.action, Action::Move { to: "Notes/Work".into() });
        // A second scan proposes nothing new.
        let again = scan(&db, Some(&p), &scope(), &acct(), NOW, CancellationToken::new()).await.unwrap();
        assert_eq!(again.misfiled, 0);
    }

    #[tokio::test]
    async fn a_suggested_folder_that_does_not_exist_is_ignored() {
        let db = crate::test_support::temp_db();
        db.create_folder_local_new(A, "Notes/Work").unwrap();
        put(&db, "N", "Quarterly numbers", "Notes", "<div>revenue</div>");
        let p = fake(vec![], Some("Notes/Invented"));
        let s = scan(&db, Some(&p), &scope(), &acct(), NOW, CancellationToken::new()).await.unwrap();
        assert_eq!(s.misfiled, 0);
    }

    #[tokio::test]
    async fn a_note_with_a_secret_is_never_sent_to_the_ai() {
        let db = crate::test_support::temp_db();
        put(&db, "S1", "Login", "Notes", "<div>pass: Zq7%Lm2pRt9x</div>");
        put(&db, "S2", "login", "Notes", "<div>pin: 482913</div>");
        let p = fake(vec![Ok(r#"{"verdict":"duplicate","keep":"S1","method":"keep","reason":"r"}"#.into())], Some("Notes/Work"));
        db.create_folder_local_new(A, "Notes/Work").unwrap();
        let s = scan(&db, Some(&p), &scope(), &acct(), NOW, CancellationToken::new()).await.unwrap();
        assert_eq!((s.secrets, s.duplicates, s.misfiled), (2, 0, 0), "{s:?}");
        assert!(p.chats.lock().unwrap().is_empty(), "secret notes reached the AI");
    }

    #[tokio::test]
    async fn without_ai_only_secrets_are_checked_and_the_value_is_never_stored() {
        let db = crate::test_support::temp_db();
        put(&db, "S", "Broker", "Notes/Projects", "<div>user: 1234 pass: Zq7%Lm2pRt9x</div>");
        put(&db, "A", "Plan", "Notes/Work", "<div>a</div>");
        put(&db, "B", "plan", "Notes/Work", "<div>b</div>");
        let s = scan(&db, None, &scope(), &acct(), NOW, CancellationToken::new()).await.unwrap();
        assert_eq!((s.secrets, s.duplicates), (1, 0));
        assert!(s.notes[0].contains("No AI provider"));
        let prop = &list(&db, A, &["pending"]).unwrap()[0];
        assert_eq!(prop.payload.action, Action::Hide);
        let stored = serde_json::to_string(&prop.payload).unwrap();
        assert!(!stored.contains("Zq7%Lm2pRt9x"), "{stored}");
    }
}
