//! The agent workspace (spec 2026-10-06 §5): topic pages holding `[kind]`
//! lines (Basic Memory's shape), inside Karpathy's LLM-Wiki layers —
//! `Conventions`, a Jodd-generated `Index`, an append-only `Log`.
//!
//! The rules live here, not in `jodd-mcp`, so the app (enable, skeleton) and
//! the MCP server (`remember`, `list_topics`, `brief`) share one placement
//! policy. Agents never compose a path: `remember` decides where a line goes.

pub mod brief;
pub mod project;
pub mod text;

use crate::accounts::Account;
use crate::db::{CachedNote, Db, SyncState};
use crate::llm::markdown::{md_to_html, sanitize_note_html};

pub use crate::mcp_scope::AGENT_WORKSPACE as ROOT;
pub const PROJECTS: &str = "Notes/__Agent__/Projects";
pub const AREAS: &str = "Notes/__Agent__/Areas";
pub const CONVENTIONS: &str = "Conventions";
pub const INDEX: &str = "Index";
pub const LOG: &str = "Log";
pub const ABOUT_ME: &str = "About me";
pub const INBOX: &str = "Inbox";
/// One observation is one line; a paragraph belongs in a note of its own.
pub const MAX_LINE_CHARS: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Decision,
    Lesson,
    Preference,
    Fact,
    Source,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Decision => "decision",
            Kind::Lesson => "lesson",
            Kind::Preference => "preference",
            Kind::Fact => "fact",
            Kind::Source => "source",
        }
    }
    pub const ALL: [Kind; 5] = [Kind::Decision, Kind::Lesson, Kind::Preference, Kind::Fact, Kind::Source];
}

pub struct RememberRequest<'a> {
    pub kind: Kind,
    pub text: &'a str,
    pub project: Option<&'a str>,
    pub area: Option<&'a str>,
    pub new_area: bool,
    pub relates_to: &'a [String],
    /// The MCP client's self-reported name — a label, not an identity.
    pub client: &'a str,
    /// `YYYY-MM-DD`, injected so tests are deterministic.
    pub date: &'a str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Remembered {
    pub page_title: String,
    /// Relative to the workspace, e.g. `Projects/Jodd`.
    pub page_path: String,
    pub uuid: String,
    pub line: String,
    pub created_page: bool,
    /// Follow-up writes (Index, Log) that failed after the line was saved.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Topic {
    pub title: String,
    pub folder: String,
    pub summary: String,
    pub lines: usize,
}

const CONVENTIONS_MD: &str = "How the agent workspace is organized. Agents read this; you may edit it.

- One page per topic. Projects (one per code repository) live in Projects, other subjects in Areas.
- A page starts with a one-line summary, then one line per fact: - [kind] text (date · agent).
- Kinds: decision, lesson, preference, fact, source.
- Relations between pages: relates_to [[Page title]].
- Preferences that are not about one topic go to About me; anything unsorted goes to Inbox.
- Index and Log are written by Jodd. Do not edit them; Index is rewritten on every write.
- Outside this workspace agents only append, or propose changes for you to approve.
- Older agent notes may still be in Notes/__Claude__.";

pub fn normalize(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Backslash-escape what Markdown or HTML would interpret, and fold the
/// text onto one line: agent text is data on a list line, never markup.
fn escape_inline(s: &str) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(one_line.len());
    for c in one_line.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '~' | '!') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn format_line(kind: Kind, text: &str, date: &str, client: &str) -> String {
    format!("- [{}] {} ({} · {})", kind.as_str(), escape_inline(text), date, escape_inline(client))
}

/// Markdown to write. Titles are escaped like agent text: a relates_to of
/// `x](https://evil.example)` must stay text, not become a link (review
/// finding 3).
fn relation_line(title: &str) -> String {
    format!("- relates_to [[{}]]", escape_inline(title))
}

/// The same line as it reads back through `html_to_text` — what "already
/// on the page" is compared against.
fn relation_text(title: &str) -> String {
    format!("- relates_to [[{title}]]")
}

pub fn log_line(date: &str, page_path: &str, kind: Kind, client: &str) -> String {
    format!("## [{date}] remember | {page_path} | {} | {}", kind.as_str(), escape_inline(client))
}

fn to_html(md: &str) -> String {
    sanitize_note_html(&md_to_html(md))
}

/// Line 1 of a page: its one-line summary.
pub fn page_summary(body_html: &str) -> String {
    text::html_to_text(body_html, false)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("- "))
        .unwrap_or("")
        .to_string()
}

fn observation_count(body_html: &str) -> usize {
    text::html_to_text(body_html, false).lines().filter(|l| l.trim_start().starts_with("- [")).count()
}

/// Up to three existing names nearest to `name`: containment first, then
/// trigram Jaccard ≥ 0.3.
pub fn suggest(name: &str, existing: &[String]) -> Vec<String> {
    fn grams(s: &str) -> std::collections::HashSet<String> {
        let padded: Vec<char> = format!("  {} ", normalize(s)).chars().collect();
        padded.windows(3).map(|w| w.iter().collect()).collect()
    }
    let want = grams(name);
    let n = normalize(name);
    let mut scored: Vec<(f64, &String)> = existing
        .iter()
        .map(|e| {
            let g = grams(e);
            let inter = want.intersection(&g).count() as f64;
            let union = want.union(&g).count().max(1) as f64;
            let ne = normalize(e);
            let contains = !n.is_empty() && (ne.contains(&n) || n.contains(&ne));
            (if contains { 1.0 } else { inter / union }, e)
        })
        .filter(|(s, _)| *s >= 0.3)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then_with(|| a.1.cmp(b.1)));
    scored.into_iter().take(3).map(|(_, e)| e.clone()).collect()
}

fn require_capable(account: &Account) -> Result<(), String> {
    let w = crate::backend::Capabilities::for_backend(account.backend_kind).writes;
    if w.notes && w.folders {
        Ok(())
    } else {
        Err(format!(
            "Account '{}' cannot hold the agent workspace: Jodd cannot create folders there. Choose another account in Settings → Agent workspace.",
            account.id
        ))
    }
}

fn find_page(db: &Db, account_id: &str, folder: &str, title: &str) -> Result<Option<CachedNote>, String> {
    let want = normalize(title);
    // Deterministic when duplicates exist (two first writes raced before the
    // lock, or another device made one): every writer picks the same page,
    // so lines stop scattering, and Curate merges the rest.
    Ok(db
        .list_notes_by_label(account_id, folder)
        .map_err(|e| format!("list {folder}: {e}"))?
        .into_iter()
        .filter(|n| normalize(&n.title) == want)
        .min_by(|a, b| a.uuid.cmp(&b.uuid)))
}

fn insert_page(db: &Db, account: &Account, folder: &str, title: &str, body_html: String) -> Result<String, String> {
    crate::folder_label::validate_note_title(title)?;
    let uuid = crate::backend::mint_uuid_for(account.backend_kind);
    let now = crate::db::now_ms();
    db.insert_local_new(&CachedNote {
        uuid: uuid.clone(),
        account_id: account.id.clone(),
        id: String::new(),
        title: title.to_string(),
        body_html,
        date: chrono::Local::now().to_rfc2822(),
        x_mail_created_date: None,
        label: folder.to_string(),
        local_version: 1,
        remote_version: None,
        sync_state: SyncState::Dirty,
        last_synced_at: None,
        last_local_modified_at: now,
        last_remote_modified_at: None,
        pinned: false,
        meta_msg_id: None,
        pin_dirty: false,
        push_blocked_reason: None,
        push_blocked_by_remote: false,
    })
    .map_err(|e| format!("insert {title}: {e}"))?;
    Ok(uuid)
}

/// Compare-and-swap edit, re-reading on every attempt so a concurrent
/// writer's change is built upon, never overwritten (the same shape as
/// `jodd-mcp`'s `write_with_retry`).
pub(crate) fn edit_page(db: &Db, account_id: &str, uuid: &str, f: impl Fn(&CachedNote) -> String) -> Result<(), String> {
    for _ in 0..5 {
        let n = db
            .note_by_uuid(account_id, uuid)
            .map_err(|e| format!("lookup: {e}"))?
            .ok_or_else(|| format!("page {uuid} disappeared"))?;
        let body = f(&n);
        if db
            .apply_local_edit_versioned(uuid, account_id, &n.title, &body, &n.label, n.local_version)
            .map_err(|e| format!("edit: {e}"))?
        {
            return Ok(());
        }
    }
    Err("the page is being edited by another process; try again".into())
}

fn ensure_folder(db: &Db, account_id: &str, path: &str) -> Result<(), String> {
    if db.get_folder(account_id, path).map_err(|e| e.to_string())?.is_none() {
        db.create_folder_local_new(account_id, path).map_err(|e| format!("create {path}: {e}"))?;
    }
    Ok(())
}

fn ensure_page(db: &Db, account: &Account, folder: &str, title: &str, md: &str) -> Result<CachedNote, String> {
    if let Some(n) = find_page(db, &account.id, folder, title)? {
        return Ok(n);
    }
    let uuid = insert_page(db, account, folder, title, to_html(md))?;
    db.note_by_uuid(&account.id, &uuid).map_err(|e| e.to_string())?.ok_or_else(|| "page vanished".into())
}

/// Every agent session runs its own `jodd-mcp`, and the app enables the
/// workspace from a third process: find-then-insert of a page must not
/// interleave across them, or two first `remember`s both create
/// `Projects/X` (review finding 2). A lock file beside the user's cache —
/// `create_new` is atomic — held for one remember; a lock older than
/// `STALE_LOCK` belongs to a crashed process and is taken over.
struct WorkspaceLock(std::path::PathBuf);
const STALE_LOCK: std::time::Duration = std::time::Duration::from_secs(30);

impl WorkspaceLock {
    fn acquire() -> Result<WorkspaceLock, String> {
        let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("jodd");
        std::fs::create_dir_all(&dir).map_err(|e| format!("lock dir: {e}"))?;
        let path = dir.join("agent-workspace.lock");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(WorkspaceLock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > STALE_LOCK);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if std::time::Instant::now() > deadline {
                        return Err("another agent is writing to the workspace; try again".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(format!("lock: {e}")),
            }
        }
    }
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Folders and fixed pages of the workspace. Idempotent: an existing page is
/// never rewritten, so a user's edit of `Conventions` survives.
pub fn ensure_skeleton(db: &Db, account: &Account) -> Result<(), String> {
    let _lock = WorkspaceLock::acquire()?;
    ensure_skeleton_locked(db, account)
}

fn ensure_skeleton_locked(db: &Db, account: &Account) -> Result<(), String> {
    require_capable(account)?;
    for f in [ROOT, PROJECTS, AREAS] {
        ensure_folder(db, &account.id, f)?;
    }
    ensure_page(db, account, ROOT, CONVENTIONS, CONVENTIONS_MD)?;
    ensure_page(db, account, ROOT, ABOUT_ME, "Your preferences and how you like to work, as agents have learned them.")?;
    ensure_page(db, account, ROOT, INBOX, "Observations not yet filed under a project or area; Curate sorts them.")?;
    ensure_page(db, account, ROOT, LOG, "Append-only record of agent writes, newest last. Written by Jodd.")?;
    if find_page(db, &account.id, ROOT, INDEX)?.is_none() {
        insert_page(db, account, ROOT, INDEX, String::new())?;
        regenerate_index(db, account)?;
    }
    Ok(())
}

/// Every topic page with its summary, for `list_topics` and `Index`.
pub fn topics(db: &Db, account_id: &str) -> Result<Vec<Topic>, String> {
    let mut out = Vec::new();
    for folder in [PROJECTS, AREAS, ROOT] {
        let mut pages: Vec<CachedNote> = db.list_notes_by_label(account_id, folder).map_err(|e| e.to_string())?;
        pages.retain(|n| folder != ROOT || matches!(n.title.as_str(), ABOUT_ME | INBOX));
        pages.sort_by_key(|n| normalize(&n.title));
        out.extend(pages.into_iter().map(|n| Topic {
            summary: page_summary(&n.body_html),
            lines: observation_count(&n.body_html),
            title: n.title,
            folder: folder.to_string(),
        }));
    }
    Ok(out)
}

pub fn render_index(topics: &[Topic]) -> String {
    let mut md = String::from("Generated by Jodd from the pages in this workspace. Do not edit: it is rewritten on every write.\n");
    for (heading, folder) in [("Projects", PROJECTS), ("Areas", AREAS), ("Workspace", ROOT)] {
        let section: Vec<&Topic> = topics.iter().filter(|t| t.folder == folder).collect();
        if section.is_empty() {
            continue;
        }
        md.push_str(&format!("\n## {heading}\n\n"));
        for t in section {
            let lines = if t.lines == 1 { "1 line".to_string() } else { format!("{} lines", t.lines) };
            md.push_str(&format!("- [[{}]] — {} ({lines})\n", escape_inline(&t.title), escape_inline(&t.summary)));
        }
    }
    md
}

fn regenerate_index(db: &Db, account: &Account) -> Result<(), String> {
    let html = to_html(&render_index(&topics(db, &account.id)?));
    let index = match find_page(db, &account.id, ROOT, INDEX)? {
        Some(n) => n,
        None => return insert_page(db, account, ROOT, INDEX, html).map(|_| ()),
    };
    edit_page(db, &account.id, &index.uuid, |_| html.clone())
}

fn append_log(db: &Db, account: &Account, line: &str) -> Result<(), String> {
    let log = ensure_page(db, account, ROOT, LOG, "Append-only record of agent writes, newest last. Written by Jodd.")?;
    let fragment = to_html(line);
    edit_page(db, &account.id, &log.uuid, |n| format!("{}{}", n.body_html, fragment))
}

enum Target {
    Area { title: String },
    Project { title: String },
    AboutMe,
    Inbox,
}

/// Placement rules, spec §5.4 — the only place they exist.
pub fn remember(db: &Db, account: &Account, req: &RememberRequest) -> Result<Remembered, String> {
    require_capable(account)?;
    let text = req.text.trim();
    if text.is_empty() {
        return Err("Nothing to remember: text is empty.".into());
    }
    if text.chars().count() > MAX_LINE_CHARS {
        return Err(format!("One observation is one line of at most {MAX_LINE_CHARS} characters; save longer material as a note (create_note) and remember a line pointing to it."));
    }
    let clean = |s: Option<&str>| s.map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|s| !s.is_empty());
    let target = match (clean(req.area), clean(req.project)) {
        (Some(area), _) => Target::Area { title: area },
        (None, Some(project)) => Target::Project { title: project },
        (None, None) if req.kind == Kind::Preference => Target::AboutMe,
        _ => Target::Inbox,
    };
    for r in req.relates_to {
        crate::folder_label::validate_note_title(r)?;
        if r.contains("]]") || r.contains("[[") {
            return Err(format!("relates_to '{r}' must be a page title, without brackets."));
        }
    }

    let _lock = WorkspaceLock::acquire()?;
    ensure_skeleton_locked(db, account)?;
    let (folder, title, new_summary) = match &target {
        Target::Area { title } => {
            if find_page(db, &account.id, AREAS, title)?.is_none() && !req.new_area {
                let existing: Vec<String> = db
                    .list_notes_by_label(&account.id, AREAS)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|n| n.title)
                    .collect();
                let near = suggest(title, &existing);
                let hint = if near.is_empty() { String::new() } else { format!(" Did you mean {}?", near.iter().map(|n| format!("'{n}'")).collect::<Vec<_>>().join(", ")) };
                return Err(format!("No area '{title}'.{hint} Pass new_area: true to create it, or omit area to file the line in Inbox."));
            }
            (AREAS, title.clone(), format!("Area: {}.", escape_inline(title)))
        }
        Target::Project { title } => (PROJECTS, title.clone(), format!("Project {}: what agents have learned working on it.", escape_inline(title))),
        Target::AboutMe => (ROOT, ABOUT_ME.to_string(), String::new()),
        Target::Inbox => (ROOT, INBOX.to_string(), String::new()),
    };

    let line = format_line(req.kind, text, req.date, req.client);
    let mut md_lines = vec![line.clone()];
    let existing = find_page(db, &account.id, folder, &title)?;
    // An existing page keeps its own spelling: "youtube SERVICES" files into
    // "YouTube services", it does not rename it.
    let title = existing.as_ref().map(|p| p.title.clone()).unwrap_or(title);
    let (uuid, created_page) = match existing {
        Some(page) => {
            let have = text::html_to_text(&page.body_html, false);
            md_lines.extend(req.relates_to.iter().filter(|r| !have.contains(&relation_text(r))).map(|r| relation_line(r)));
            let fragment = to_html(&md_lines.join("\n"));
            edit_page(db, &account.id, &page.uuid, |n| format!("{}{}", n.body_html, fragment))?;
            (page.uuid, false)
        }
        None => {
            md_lines.extend(req.relates_to.iter().map(|r| relation_line(r)));
            let body = to_html(&format!("{new_summary}\n\n{}", md_lines.join("\n")));
            (insert_page(db, account, folder, &title, body)?, true)
        }
    };

    let page_path = folder.strip_prefix(ROOT).unwrap_or(folder).trim_start_matches('/');
    let page_path = if page_path.is_empty() { title.clone() } else { format!("{page_path}/{title}") };
    let mut warnings = Vec::new();
    if let Err(e) = regenerate_index(db, account) {
        warnings.push(format!("Index not updated ({e}); the next remember rewrites it."));
    }
    if let Err(e) = append_log(db, account, &log_line(req.date, &page_path, req.kind, req.client)) {
        warnings.push(format!("Log not updated ({e})."));
    }
    Ok(Remembered { page_title: title, page_path, uuid, line, created_page, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_db;

    fn account(id: &str, backend: &str) -> Account {
        serde_json::from_str(&format!(r#"{{"id":"{id}","email":"a@x.com","added_at":"2026-01-01T00:00:00Z","backend_kind":"{backend}"}}"#)).unwrap()
    }
    fn gmail() -> Account {
        account("gmail:a@x.com", "gmail")
    }
    fn req<'a>(kind: Kind, text: &'a str) -> RememberRequest<'a> {
        RememberRequest { kind, text, project: None, area: None, new_area: false, relates_to: &[], client: "claude-code", date: "2026-10-06" }
    }
    fn page(db: &Db, folder: &str, title: &str) -> CachedNote {
        find_page(db, "gmail:a@x.com", folder, title).unwrap().unwrap_or_else(|| panic!("no page {folder}/{title}"))
    }
    fn text_of(db: &Db, folder: &str, title: &str) -> String {
        text::html_to_text(&page(db, folder, title).body_html, false)
    }

    #[test]
    fn skeleton_creates_folders_and_pages_once() {
        let (db, a) = (temp_db(), gmail());
        ensure_skeleton(&db, &a).unwrap();
        ensure_skeleton(&db, &a).unwrap();
        for f in [ROOT, PROJECTS, AREAS] {
            assert!(db.get_folder(&a.id, f).unwrap().is_some(), "{f}");
        }
        let root = db.list_notes_by_label(&a.id, ROOT).unwrap();
        let mut titles: Vec<&str> = root.iter().map(|n| n.title.as_str()).collect();
        titles.sort();
        assert_eq!(titles, [ABOUT_ME, CONVENTIONS, INBOX, INDEX, LOG]);
    }

    #[test]
    fn a_users_edit_of_conventions_survives_the_skeleton() {
        let (db, a) = (temp_db(), gmail());
        ensure_skeleton(&db, &a).unwrap();
        let c = page(&db, ROOT, CONVENTIONS);
        db.apply_local_edit_versioned(&c.uuid, &a.id, &c.title, "<div>mine</div>", &c.label, c.local_version).unwrap();
        ensure_skeleton(&db, &a).unwrap();
        assert_eq!(page(&db, ROOT, CONVENTIONS).body_html, "<div>mine</div>");
    }

    #[test]
    fn a_project_page_is_created_on_first_use_then_appended_to() {
        let (db, a) = (temp_db(), gmail());
        let r1 = remember(&db, &a, &RememberRequest { project: Some("Jodd"), ..req(Kind::Decision, "Chunk Key points") }).unwrap();
        let r2 = remember(&db, &a, &RememberRequest { project: Some("jodd "), ..req(Kind::Lesson, "Run the control") }).unwrap();
        assert!(r1.created_page && !r2.created_page);
        assert_eq!((r1.page_path.as_str(), r1.uuid.as_str()), ("Projects/Jodd", r2.uuid.as_str()));
        let t = text_of(&db, PROJECTS, "Jodd");
        assert!(t.starts_with("Project Jodd: what agents"), "{t}");
        assert!(t.contains("- [decision] Chunk Key points (2026-10-06 · claude-code)"), "{t}");
        assert!(t.contains("- [lesson] Run the control (2026-10-06 · claude-code)"), "{t}");
        assert!(r1.warnings.is_empty(), "{:?}", r1.warnings);
    }

    #[test]
    fn an_unknown_area_is_refused_with_suggestions_and_nothing_is_written() {
        let (db, a) = (temp_db(), gmail());
        remember(&db, &a, &RememberRequest { area: Some("YouTube services"), new_area: true, ..req(Kind::Fact, "x") }).unwrap();
        let before = db.list_notes_by_label(&a.id, AREAS).unwrap().len();
        let log_before = text_of(&db, ROOT, LOG);
        let e = remember(&db, &a, &RememberRequest { area: Some("Youtube"), ..req(Kind::Fact, "y") }).unwrap_err();
        assert!(e.contains("No area 'Youtube'") && e.contains("'YouTube services'") && e.contains("new_area"), "{e}");
        assert_eq!(db.list_notes_by_label(&a.id, AREAS).unwrap().len(), before);
        assert_eq!(text_of(&db, ROOT, LOG), log_before);
    }

    #[test]
    fn area_names_match_ignoring_case_and_spacing() {
        let (db, a) = (temp_db(), gmail());
        remember(&db, &a, &RememberRequest { area: Some("YouTube services"), new_area: true, ..req(Kind::Fact, "a") }).unwrap();
        let r = remember(&db, &a, &RememberRequest { area: Some("  youtube   SERVICES "), ..req(Kind::Fact, "b") }).unwrap();
        assert_eq!(r.page_title, "YouTube services");
        assert_eq!(db.list_notes_by_label(&a.id, AREAS).unwrap().len(), 1);
        assert!(!r.created_page);
    }

    #[test]
    fn preferences_without_a_topic_go_to_about_me_and_the_rest_to_inbox() {
        let (db, a) = (temp_db(), gmail());
        assert_eq!(remember(&db, &a, &req(Kind::Preference, "Ask before force-pushing")).unwrap().page_path, ABOUT_ME);
        assert_eq!(remember(&db, &a, &req(Kind::Fact, "Something")).unwrap().page_path, INBOX);
        assert!(text_of(&db, ROOT, ABOUT_ME).contains("- [preference] Ask before force-pushing"));
    }

    #[test]
    fn agent_text_is_escaped_single_line_and_sanitized() {
        let (db, a) = (temp_db(), gmail());
        let r = remember(&db, &a, &req(Kind::Fact, "a <script>x()</script>\n**b** [l](http://e) ")).unwrap();
        let html = page(&db, ROOT, INBOX).body_html;
        assert!(!html.contains("<script") && !html.contains("<strong>") && !html.contains("<a "), "{html}");
        let t = text_of(&db, ROOT, INBOX);
        assert!(t.contains("- [fact] a <script>x()</script> **b** [l](http://e) (2026-10-06"), "{t}");
        assert!(!r.line.contains('\n'));
    }

    #[test]
    fn relates_to_adds_one_wikilink_line_per_page() {
        let (db, a) = (temp_db(), gmail());
        let rel = vec!["YouTube services".to_string()];
        remember(&db, &a, &RememberRequest { project: Some("Jodd"), relates_to: &rel, ..req(Kind::Fact, "a") }).unwrap();
        remember(&db, &a, &RememberRequest { project: Some("Jodd"), relates_to: &rel, ..req(Kind::Fact, "b") }).unwrap();
        let t = text_of(&db, PROJECTS, "Jodd");
        assert_eq!(t.matches("relates_to [[YouTube services]]").count(), 1, "{t}");
        assert!(remember(&db, &a, &RememberRequest { relates_to: &["[[x]]".to_string()], ..req(Kind::Fact, "c") }).is_err());
    }

    #[test]
    fn titles_are_text_never_links_or_markup() {
        let (db, a) = (temp_db(), gmail());
        let evil = vec!["x](https://evil.example/p)".to_string()];
        remember(&db, &a, &RememberRequest { project: Some("*Bold* <b>p</b>"), relates_to: &evil, ..req(Kind::Fact, "a") }).unwrap();
        remember(&db, &a, &RememberRequest { project: Some("*Bold* <b>p</b>"), relates_to: &evil, ..req(Kind::Fact, "b") }).unwrap();
        let p = page(&db, PROJECTS, "*Bold* <b>p</b>");
        assert!(!p.body_html.contains("<a ") && !p.body_html.contains("<em>") && !p.body_html.contains("<b>"), "{}", p.body_html);
        let t = text::html_to_text(&p.body_html, false);
        assert_eq!(t.matches("relates_to [[x](https://evil.example/p)]]").count(), 1, "{t}");
        let index = page(&db, ROOT, INDEX).body_html;
        assert!(!index.contains("<em>") && !index.contains("<b>"), "{index}");
    }

    #[test]
    fn concurrent_first_writes_make_one_page() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let db = std::sync::Arc::new(Db::open_unencrypted(&path).unwrap());
        let handles: Vec<_> = (0..6)
            .map(|i| {
                let db = db.clone();
                std::thread::spawn(move || {
                    let text = format!("line {i}");
                    remember(&db, &gmail(), &RememberRequest { project: Some("Race"), ..req(Kind::Fact, &text) }).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(db.list_notes_by_label("gmail:a@x.com", PROJECTS).unwrap().len(), 1);
        for f in [CONVENTIONS, INDEX, LOG, ABOUT_ME, INBOX] {
            let n = db.list_notes_by_label("gmail:a@x.com", ROOT).unwrap().into_iter().filter(|n| n.title == f).count();
            assert_eq!(n, 1, "{f}");
        }
        assert_eq!(text_of(&db, PROJECTS, "Race").matches("- [fact] line").count(), 6);
    }

    #[test]
    fn index_lists_every_page_once_and_log_gets_one_line_per_write() {
        let (db, a) = (temp_db(), gmail());
        remember(&db, &a, &RememberRequest { project: Some("Jodd"), ..req(Kind::Decision, "a") }).unwrap();
        remember(&db, &a, &RememberRequest { project: Some("Jodd"), ..req(Kind::Lesson, "b") }).unwrap();
        remember(&db, &a, &RememberRequest { area: Some("Marketing"), new_area: true, ..req(Kind::Fact, "c") }).unwrap();
        let index = text_of(&db, ROOT, INDEX);
        assert_eq!(index.matches("[[Jodd]]").count(), 1, "{index}");
        assert!(index.contains("[[Jodd]] — Project Jodd: what agents have learned working on it. (2 lines)"), "{index}");
        assert!(index.contains("[[Marketing]]") && index.contains("[[About me]]") && index.contains("[[Inbox]]"), "{index}");
        let log = text_of(&db, ROOT, LOG);
        assert_eq!(log.matches("## [2026-10-06] remember | Projects/Jodd |").count(), 2, "{log}");
        assert!(log.contains("## [2026-10-06] remember | Areas/Marketing | fact | claude-code"), "{log}");
    }

    #[test]
    fn render_index_is_deterministic() {
        let t = vec![
            Topic { title: "B".into(), folder: PROJECTS.into(), summary: "b".into(), lines: 1 },
            Topic { title: "Inbox".into(), folder: ROOT.into(), summary: "i".into(), lines: 0 },
        ];
        assert_eq!(render_index(&t), render_index(&t));
        assert!(render_index(&t).find("## Projects").unwrap() < render_index(&t).find("## Workspace").unwrap());
    }

    #[test]
    fn suggest_ranks_the_nearest_three() {
        let existing: Vec<String> = ["YouTube services", "Marketing", "Thai market", "Personal finance"].map(String::from).to_vec();
        assert_eq!(suggest("Youtube", &existing)[0], "YouTube services");
        assert_eq!(suggest("marketng", &existing)[0], "Marketing");
        assert!(suggest("quantum physics", &existing).is_empty());
        assert!(suggest("a", &existing).len() <= 3);
    }

    #[test]
    fn an_account_without_folder_writes_cannot_hold_the_workspace() {
        let (db, a) = (temp_db(), account("microsoft:a@x.com", "microsoft"));
        let e = remember(&db, &a, &req(Kind::Fact, "x")).unwrap_err();
        assert!(e.contains("cannot hold the agent workspace"), "{e}");
        assert!(db.list_notes_by_label(&a.id, ROOT).unwrap().is_empty());
    }

    #[test]
    fn empty_and_oversized_text_is_refused() {
        let (db, a) = (temp_db(), gmail());
        assert!(remember(&db, &a, &req(Kind::Fact, "   ")).is_err());
        let long = "x".repeat(MAX_LINE_CHARS + 1);
        assert!(remember(&db, &a, &req(Kind::Fact, &long)).unwrap_err().contains("create_note"));
    }
}
