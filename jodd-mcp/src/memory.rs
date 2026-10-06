//! Agent-memory tools (spec 2026-10-06 §5.4, §5.8): `read_note`, `remember`,
//! `list_topics`, and the server `instructions`. Placement rules live in
//! `jodd_lib::agent_memory`; this file adapts them to MCP.

use crate::scope::WriteScope;
use jodd_lib::accounts::Account;
use jodd_lib::agent_memory::{self, Kind};

/// `read_note` returns at most this many characters per call.
pub const READ_PAGE_CHARS: usize = 20_000;
/// Spec §5.8: instructions ride in every session of every client.
pub const INSTRUCTIONS_MAX_CHARS: usize = 2_000;

/// The one text for a note that is missing OR hidden (spec §5.3): saying
/// "hidden" would confirm the note exists.
pub fn no_such_note(uuid: &str, account_id: &str) -> String {
    format!("No note '{uuid}' in account '{account_id}'.")
}

/// The JSON Schema face of `agent_memory::Kind` — the lib does not depend on
/// schemars, and the enum here is what makes an invented kind a schema error.
#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KindParam {
    /// A choice that was made, and why.
    Decision,
    /// Something learned the hard way that should change future work.
    Lesson,
    /// How the user likes things done.
    Preference,
    /// A stable fact about the project, a person, or the world.
    Fact,
    /// A useful source: a link, a document, a video.
    Source,
}

impl From<KindParam> for Kind {
    fn from(k: KindParam) -> Kind {
        match k {
            KindParam::Decision => Kind::Decision,
            KindParam::Lesson => Kind::Lesson,
            KindParam::Preference => Kind::Preference,
            KindParam::Fact => Kind::Fact,
            KindParam::Source => Kind::Source,
        }
    }
}

#[derive(serde::Serialize)]
pub struct ReadNote {
    pub title: String,
    pub folder: String,
    pub text: String,
    pub total_chars: usize,
    pub next_offset: Option<usize>,
}

pub fn do_read_note(
    db: &jodd_lib::db::Db,
    scope: &WriteScope,
    account_id: &str,
    uuid: &str,
    offset: usize,
    include_source: bool,
) -> Result<ReadNote, String> {
    let note = db
        .note_by_uuid(account_id, uuid)
        .map_err(|e| format!("lookup: {e}"))?
        .filter(|n| !scope.is_hidden(account_id, &n.label))
        .ok_or_else(|| no_such_note(uuid, account_id))?;
    let full = agent_memory::text::html_to_text(&note.body_html, include_source);
    let total_chars = full.chars().count();
    let text: String = full.chars().skip(offset).take(READ_PAGE_CHARS).collect();
    let end = offset + text.chars().count();
    Ok(ReadNote { title: note.title, folder: note.label, text, total_chars, next_offset: (end < total_chars).then_some(end) })
}

/// The workspace account, resolved and checked: enabled, active, and its
/// workspace inside the allowlist (enabling adds it; a hand edit could drop it).
pub fn workspace_account<'a>(accounts: &'a [Account], scope: &WriteScope) -> Result<&'a Account, String> {
    let id = scope
        .workspace_account()
        .ok_or("Agent memory is off — the user can enable it in Jodd Settings → Agent workspace.")?;
    let account = crate::write::resolve_active_account(accounts, id)?;
    if !crate::scope::folder_allowed(scope.allowed_folders(id), agent_memory::ROOT) {
        return Err(format!(
            "The agent workspace {} is not in allowed_folders for '{id}' in mcp_write_scope.json; re-enable it in Jodd Settings.",
            agent_memory::ROOT
        ));
    }
    Ok(account)
}

pub struct RememberArgs<'a> {
    pub kind: Kind,
    pub text: &'a str,
    pub project: Option<&'a str>,
    pub area: Option<&'a str>,
    pub new_area: bool,
    pub relates_to: &'a [String],
    pub client: &'a str,
}

pub fn do_remember(
    db: &jodd_lib::db::Db,
    accounts: &[Account],
    scope: &WriteScope,
    args: &RememberArgs,
) -> Result<agent_memory::Remembered, String> {
    let account = workspace_account(accounts, scope)?;
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    agent_memory::remember(
        db,
        account,
        &agent_memory::RememberRequest {
            kind: args.kind,
            text: args.text,
            project: args.project,
            area: args.area,
            new_area: args.new_area,
            relates_to: args.relates_to,
            client: args.client,
            date: &date,
        },
    )
}

pub fn do_list_topics(db: &jodd_lib::db::Db, accounts: &[Account], scope: &WriteScope) -> Result<Vec<agent_memory::Topic>, String> {
    let account = workspace_account(accounts, scope)?;
    agent_memory::topics(db, &account.id)
}

const INSTRUCTIONS_HEAD: &str = "Jodd is the user's personal knowledge base, shared by all of their agents: lessons, decisions, preferences, sources and project context.

Before non-trivial work, check Jodd: search_notes for the project, topic or person involved, then read_note for the full text. Prefer what Jodd records over your own assumptions, and say so when they conflict.";

const INSTRUCTIONS_MEMORY_ON: &str = "When you learn something worth keeping — a decision and its reason, a lesson, a user preference, a useful source — call remember with one line. For coding work pass project (the repository name); for other work pass an existing area (see list_topics). If unsure, omit both: it goes to Inbox and is sorted later. Never put secrets in a line.";

const INSTRUCTIONS_MEMORY_OFF: &str = "Agent memory is off; the user can enable it in Jodd Settings → Agent workspace.";

const INSTRUCTIONS_TAIL: &str = "Outside the agent workspace, only append. Some folders are hidden from you by the user; do not ask about them.";

pub fn instructions(scope: &WriteScope) -> String {
    let memory = match scope.workspace_account() {
        Some(id) => format!(
            "{INSTRUCTIONS_MEMORY_ON} The agent workspace is {} in account {id}.",
            agent_memory::ROOT
        ),
        None => INSTRUCTIONS_MEMORY_OFF.to_string(),
    };
    format!("{INSTRUCTIONS_HEAD}\n\n{memory}\n\n{INSTRUCTIONS_TAIL}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(json: &str) -> WriteScope {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        std::fs::write(&p, json).unwrap();
        crate::scope::load_write_scope_from(&p).unwrap()
    }
    fn acct() -> Account {
        serde_json::from_str(r#"{"id":"gmail:a@x.com","email":"a@x.com","added_at":"2026-01-01T00:00:00Z"}"#).unwrap()
    }
    fn db() -> (tempfile::TempDir, jodd_lib::db::Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = jodd_lib::db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        (dir, db)
    }
    const WS: &str = r#"{"agent_workspace":{"account_id":"gmail:a@x.com"},"accounts":{"gmail:a@x.com":{"allowed_folders":["Notes/__Agent__"],"hidden_folders":["Notes/Private"]}}}"#;

    fn note(db: &jodd_lib::db::Db, uuid: &str, label: &str, body: &str) {
        db.insert_local_new(&jodd_lib::db::CachedNote {
            uuid: uuid.into(), account_id: "gmail:a@x.com".into(), id: String::new(), title: "T".into(),
            body_html: body.into(), date: "Thu, 4 Jun 2026 01:19:50 +0700".into(), x_mail_created_date: None,
            label: label.into(), local_version: 1, remote_version: None, sync_state: jodd_lib::db::SyncState::Clean,
            last_synced_at: None, last_local_modified_at: 0, last_remote_modified_at: None, pinned: false,
            meta_msg_id: None, pin_dirty: false, push_blocked_reason: None, push_blocked_by_remote: false,
        }).unwrap();
    }

    #[test]
    fn instructions_fit_the_budget_in_both_variants() {
        for s in [scope(WS), scope("{}")] {
            let i = instructions(&s);
            assert!(i.chars().count() <= INSTRUCTIONS_MAX_CHARS, "{}", i.chars().count());
            assert!(i.contains("search_notes") && i.contains("read_note"), "{i}");
        }
        assert!(instructions(&scope(WS)).contains("call remember"));
        assert!(instructions(&scope("{}")).contains("Agent memory is off"));
    }

    #[test]
    fn read_note_pages_and_strips_the_source_block() {
        let (_d, db) = db();
        let body = format!("<div>{}</div><hr>\n<details>\n<summary>Source (verbatim)</summary>\n<pre>SECRET</pre>\n</details>", "x".repeat(25_000));
        note(&db, "U1", "Notes/Work", &body);
        let s = scope(WS);
        let p1 = do_read_note(&db, &s, "gmail:a@x.com", "U1", 0, false).unwrap();
        assert_eq!(p1.text.chars().count(), READ_PAGE_CHARS);
        let next = p1.next_offset.unwrap();
        let p2 = do_read_note(&db, &s, "gmail:a@x.com", "U1", next, false).unwrap();
        assert!(p2.next_offset.is_none() && !p2.text.contains("SECRET") && p2.text.contains("include_source"), "{}", p2.text);
        let with_source = do_read_note(&db, &s, "gmail:a@x.com", "U1", next, true).unwrap();
        assert!(with_source.text.contains("SECRET"), "{}", with_source.text);
    }

    #[test]
    fn a_hidden_note_reads_exactly_like_a_missing_one() {
        let (_d, db) = db();
        note(&db, "U2", "Notes/Private/Diary", "<div>secret</div>");
        let s = scope(WS);
        let hidden = do_read_note(&db, &s, "gmail:a@x.com", "U2", 0, false).err().unwrap();
        let missing = do_read_note(&db, &s, "gmail:a@x.com", "U3", 0, false).err().unwrap();
        assert_eq!(hidden.replace("U2", "?"), missing.replace("U3", "?"));
    }

    #[test]
    fn remember_needs_an_enabled_allowlisted_workspace() {
        let (_d, db) = db();
        let accounts = vec![acct()];
        let args = RememberArgs { kind: Kind::Fact, text: "x", project: None, area: None, new_area: false, relates_to: &[], client: "t" };
        assert!(do_remember(&db, &accounts, &scope("{}"), &args).unwrap_err().contains("Agent memory is off"));
        let not_allowed = r#"{"agent_workspace":{"account_id":"gmail:a@x.com"}}"#;
        assert!(do_remember(&db, &accounts, &scope(not_allowed), &args).unwrap_err().contains("allowed_folders"));
        let r = do_remember(&db, &accounts, &scope(WS), &RememberArgs { project: Some("Jodd"), ..args }).unwrap();
        assert_eq!(r.page_path, "Projects/Jodd");
        let topics = do_list_topics(&db, &accounts, &scope(WS)).unwrap();
        assert!(topics.iter().any(|t| t.title == "Jodd" && t.lines == 1));
    }
}
