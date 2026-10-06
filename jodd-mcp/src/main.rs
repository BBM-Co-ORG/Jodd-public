use std::path::PathBuf;
use std::sync::Arc;

mod memory;
mod scope;
mod write;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    Peer, RoleServer, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::Deserialize;

/// Account ids the app has dismissed. Read fresh on each call rather than
/// cached at startup: the MCP server is long-lived and the user may deactivate
/// an account while a Claude Code session is open.
fn hidden_account_ids() -> Vec<String> {
    jodd_lib::accounts::load_accounts()
        .into_iter()
        .filter(|a| !a.is_active())
        .map(|a| a.id)
        .collect()
}

/// Resolve the SQLite cache path: `--db-path <path>` or `JODD_DB_PATH` env
/// var override, else the same default the Tauri app itself uses
/// (`dirs::data_dir().join("jodd")`, see `src-tauri/src/lib.rs:3949`),
/// joined with `jodd.sqlite3`.
fn resolve_db_path() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    if let Some(idx) = args.iter().position(|a| a == "--db-path") {
        if let Some(path) = args.get(idx + 1) {
            return PathBuf::from(path);
        }
    }
    if let Ok(path) = std::env::var("JODD_DB_PATH") {
        return PathBuf::from(path);
    }
    let data_dir = dirs::data_dir()
        .map(|d| d.join("jodd"))
        .unwrap_or_else(|| std::env::temp_dir().join("jodd"));
    data_dir.join("jodd.sqlite3")
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchNotesParams {
    /// Restrict to one account (Jodd account id). Omit to search every
    /// account.
    account_id: Option<String>,
    /// Restrict to one folder label (e.g. "Notes/Work"). Omit to search
    /// every folder.
    label: Option<String>,
    /// The search query. FTS5, trigram-tokenized (Thai-aware).
    query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NoteConnectionsParams {
    /// The account this note belongs to.
    account_id: String,
    /// The note's UUID.
    uuid: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CreateNoteParams {
    /// Target account. Get valid ids from list_accounts.
    account_id: String,
    /// Target folder label, e.g. "Notes/__Claude__". Must be inside the
    /// account's allowed_folders (see list_accounts). Created if missing.
    folder: String,
    /// Note title.
    title: String,
    /// Note body as Markdown (GFM). Jodd converts and sanitizes it —
    /// never send HTML. Include #hashtags in the text to tag the note.
    body_markdown: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UpdateNoteParams {
    /// Target account (see list_accounts).
    account_id: String,
    /// The note's UUID (from search_notes or create_note).
    uuid: String,
    /// Content to add, as Markdown (never HTML). #hashtags become tags.
    body_markdown: String,
    /// "append" (default, always safe — existing content is never touched)
    /// or "replace" (full rewrite; refused if the note contains content you
    /// cannot see and would destroy, e.g. Apple checklist state — unless
    /// force=true).
    mode: Option<String>,
    /// New title; omit to keep the current one.
    title: Option<String>,
    /// Only meaningful with mode="replace": override the destroy-guard.
    force: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListTasksParams {
    /// Target account (see list_accounts).
    account_id: String,
    /// Restrict to one folder and its subfolders, e.g. "Notes/Work". Omit for the whole account.
    label: Option<String>,
    /// Include already-completed tasks. Defaults to false (outstanding only).
    include_done: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SetTaskStateParams {
    /// Target account (see list_accounts).
    account_id: String,
    /// The note's UUID.
    uuid: String,
    /// 0-based task index within the note, as returned by list_tasks.
    index: usize,
    /// true to complete the task, false to reopen it.
    checked: bool,
    /// Required: the task text you believe is at this index, from list_tasks.
    /// `index` alone is not a stable identity — the same index can validly
    /// exist in an unrelated note — so if the text does not match, the call
    /// is refused instead of ticking the wrong box.
    expect_text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SetPinParams {
    /// Target account (see list_accounts).
    account_id: String,
    /// The note's UUID.
    uuid: String,
    /// true to pin the note, false to unpin it.
    pinned: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CreateFolderParams {
    /// Target account (see list_accounts).
    account_id: String,
    /// Full folder path under Notes/, e.g. "Notes/__Claude__/Research".
    /// Must be inside the account's allowed_folders. A __name__ leaf is
    /// classified as a system-workflow folder (grouped with __Extracts__).
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReadNoteParams {
    /// The account this note belongs to.
    account_id: String,
    /// The note's UUID (from search_notes, note_connections or list_topics).
    uuid: String,
    /// Character offset to continue from — the `next_offset` of the previous call.
    offset: Option<usize>,
    /// Include the verbatim source text an ingest saved at the end of the
    /// note. Defaults to false: it is often most of the note.
    include_source: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RememberParams {
    /// What kind of knowledge this line is.
    kind: memory::KindParam,
    /// One line, in your own words. Never a secret.
    text: String,
    /// For coding work: the repository name, e.g. "Jodd". Omit to let Jodd
    /// take it from your workspace folder, when your client shares it.
    project: Option<String>,
    /// For other work: an existing area from list_topics. An unknown area is
    /// refused with suggestions unless new_area is true.
    area: Option<String>,
    /// Create `area` as a new page. Only when list_topics has nothing that fits.
    new_area: Option<bool>,
    /// Titles of related pages, recorded as relates_to [[Title]] once per page.
    relates_to: Option<Vec<String>>,
}

/// Read tools never fail on a MISSING scope file — it means nothing is
/// hidden, as before hidden folders existed — but an UNPARSEABLE one now
/// refuses (spec 2026-10-06 §5.3): it may be the file that hides the
/// user's private folders, and reading as if it hid nothing would expose them.
fn load_scope_for_read() -> Result<scope::WriteScope, rmcp::ErrorData> {
    match scope::load_write_scope_from(&scope::scope_path()) {
        Ok(s) => Ok(s),
        Err(scope::ScopeError::NotConfigured) => Ok(scope::WriteScope::default()),
        Err(scope::ScopeError::Unparseable(e)) => Err(rmcp::ErrorData::invalid_params(
            format!("mcp_write_scope.json is unreadable ({e}) — fix it at {}. Jodd refuses to read or write until it parses, since it may be what hides private folders.", scope::scope_path().display()),
            None,
        )),
    }
}

/// The project for `remember` when the agent passed none: the client's first
/// `file://` root, if it advertises roots (spec §5.5). Logs what it saw — the
/// evidence §5.10's live probe records.
async fn project_from_roots(peer: &Peer<RoleServer>, client: &str) -> Option<String> {
    let advertises = peer.peer_info().is_some_and(|i| i.capabilities.roots.is_some());
    if !advertises {
        eprintln!("jodd-mcp: client {client}: no roots capability");
        return None;
    }
    let roots = match tokio::time::timeout(std::time::Duration::from_secs(3), peer.list_roots()).await {
        Ok(Ok(r)) => r.roots,
        other => {
            eprintln!("jodd-mcp: client {client}: list_roots failed: {:?}", other.map(|r| r.map(|_| ())));
            return None;
        }
    };
    eprintln!("jodd-mcp: client {client}: {} root(s)", roots.len());
    roots.iter().find_map(|r| {
        let path = root_dir(&r.uri)?;
        jodd_lib::agent_memory::project::project_from_dir(&path)
    })
}

/// A root's directory: a properly decoded `file://` URI (`%20`, Thai,
/// `localhost`), and only one that exists — walking up from a path that is
/// not there could land in an unrelated repo such as a dotfiles `~` (review
/// finding 4).
fn root_dir(uri: &str) -> Option<PathBuf> {
    let path = url::Url::parse(uri).ok().filter(|u| u.scheme() == "file")?.to_file_path().ok()?;
    path.is_dir().then_some(path)
}

/// Shared by every write tool (spec §5's missing/unparseable behavior): an
/// unconfigured or unparseable mcp_write_scope.json must fail loudly here,
/// not be silently downgraded to an empty allowlist the way read tools do.
fn load_scope_for_write() -> Result<scope::WriteScope, rmcp::ErrorData> {
    match scope::load_write_scope_from(&scope::scope_path()) {
        Ok(s) => Ok(s),
        Err(scope::ScopeError::NotConfigured) => Err(rmcp::ErrorData::invalid_params(
            format!("No write scope configured. Create {} with {{\"accounts\":{{\"<account_id>\":{{\"allowed_folders\":[\"Notes/__Claude__\"]}}}}}} to grant write access.", scope::scope_path().display()),
            None,
        )),
        Err(scope::ScopeError::Unparseable(e)) => Err(rmcp::ErrorData::invalid_params(
            format!("mcp_write_scope.json is unreadable ({e}) — fix it at {}. Write tools refuse until it parses; read tools are unaffected.", scope::scope_path().display()),
            None,
        )),
    }
}

/// Serialize a write tool's success payload. Hand-built via `format!` until
/// finding M1: every field is agent-controlled (a folder path, a uuid, a
/// title), so a single `"` in one of them produced malformed JSON the client
/// then failed to parse — a write that succeeded reported as a protocol
/// error. `serde_json` escapes; `format!` does not.
fn ok_json(value: serde_json::Value) -> Result<String, rmcp::ErrorData> {
    serde_json::to_string(&value)
        .map_err(|e| rmcp::ErrorData::internal_error(format!("serializing result: {e}"), None))
}

#[derive(Clone)]
struct JoddMcp {
    db: Arc<jodd_lib::db::Db>,
    // Read by the `#[tool_handler]`-generated `ServerHandler` impl, not by
    // hand-written code, so rustc's dead-code pass can't see the use.
    #[allow(dead_code)]
    tool_router: ToolRouter<JoddMcp>,
}

#[tool_router]
impl JoddMcp {
    #[tool(
        description = "Full-text search over Jodd's notes (FTS5, Thai-aware trigram). Pass account_id and/or label to narrow scope; omit both to search every account and folder. Each match's body is a bounded, tag-stripped text preview (not the full body_html) — there is currently no tool to look up a note's complete content by uuid. At most 50 matches are returned, best-ranked first; if that may not be all of them, narrow with account_id/label or a more specific query rather than assuming you have seen everything."
    )]
    fn search_notes(
        &self,
        Parameters(params): Parameters<SearchNotesParams>,
    ) -> Result<String, rmcp::ErrorData> {
        let notes = write::do_search_notes(
            &self.db,
            params.account_id.as_deref(),
            params.label.as_deref(),
            &params.query,
            &hidden_account_ids(),
            &load_scope_for_read()?,
        )
        .map_err(|e| rmcp::ErrorData::internal_error(format!("search_notes: {e}"), None))?;
        serde_json::to_string(&notes).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("search_notes: serializing results: {e}"), None)
        })
    }

    #[tool(
        description = "A note's [[wikilink]] connections: notes it links to (outgoing) and notes that link to it (backlinks). Given an account_id and a note uuid. Each connected note's body is the same bounded, tag-stripped text preview search_notes returns — not its full content. outgoing and backlinks share a combined budget of 50 notes total (outgoing fills first), not 50 each — a hub note's response can otherwise blow past the same size bound search_notes is held to."
    )]
    fn note_connections(
        &self,
        Parameters(params): Parameters<NoteConnectionsParams>,
    ) -> Result<String, rmcp::ErrorData> {
        let result = write::do_note_connections(&self.db, &params.account_id, &params.uuid, &load_scope_for_read()?)
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        serde_json::to_string(&result).map_err(|e| {
            rmcp::ErrorData::internal_error(
                format!("note_connections: serializing results: {e}"),
                None,
            )
        })
    }

    #[tool(
        description = "List Jodd accounts available to this server: account_id, backend_kind (Gmail notes eventually reach Apple Notes; LocalFs notes stay in an on-disk vault), and which folders write tools may touch (allowed_folders, from mcp_write_scope.json; empty = no write access). Call this FIRST before any write tool."
    )]
    fn list_accounts(&self) -> Result<String, rmcp::ErrorData> {
        // Unparseable scope must not break a read-style tool (§5): degrade to
        // empty allowlists; the write tools themselves will refuse loudly.
        let scope = scope::load_write_scope_from(&scope::scope_path()).unwrap_or_default();
        let entries = write::do_list_accounts(&jodd_lib::accounts::load_accounts(), &scope);
        serde_json::to_string(&entries)
            .map_err(|e| rmcp::ErrorData::internal_error(format!("list_accounts: {e}"), None))
    }

    #[tool(
        description = "Create a new note in a Jodd account. Body is Markdown (never HTML); #hashtags in the body become the note's tags. The folder must be inside the account's allowed_folders (call list_accounts first). The note syncs to the backend (Gmail/Apple Notes or a local vault) next time the Jodd app runs."
    )]
    fn create_note(&self, Parameters(p): Parameters<CreateNoteParams>) -> Result<String, rmcp::ErrorData> {
        let scope = load_scope_for_write()?;
        let accounts = jodd_lib::accounts::load_accounts();
        let uuid = write::do_create_note(&self.db, &accounts, &scope, &p.account_id, &p.folder, &p.title, &p.body_markdown)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        ok_json(serde_json::json!({ "uuid": uuid, "folder": p.folder }))
    }

    #[tool(
        description = "Update an existing Jodd note. Default mode 'append' adds your Markdown after the current content and is always safe. Mode 'replace' rewrites the whole body and is REFUSED when the note holds content outside the safe subset (Apple checklist state, attachments) unless force=true. Prefer append."
    )]
    fn update_note(&self, Parameters(p): Parameters<UpdateNoteParams>) -> Result<String, rmcp::ErrorData> {
        let scope = load_scope_for_write()?;
        let accounts = jodd_lib::accounts::load_accounts();
        let mode = match p.mode.as_deref() {
            None | Some("append") => write::UpdateMode::Append,
            Some("replace") => write::UpdateMode::Replace,
            Some(other) => return Err(rmcp::ErrorData::invalid_params(
                format!("Unknown mode '{other}': use \"append\" or \"replace\"."), None)),
        };
        write::do_update_note(&self.db, &accounts, &scope, &p.account_id, &p.uuid,
            &p.body_markdown, mode, p.title.as_deref(), p.force.unwrap_or(false))
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        ok_json(serde_json::json!({ "uuid": p.uuid, "updated": true }))
    }

    #[tool(
        description = "List checklist tasks across a Jodd account's notes, with their completed state. Use this to answer what is outstanding. Indices returned here are what set_task_state expects."
    )]
    fn list_tasks(&self, Parameters(p): Parameters<ListTasksParams>) -> Result<String, rmcp::ErrorData> {
        let accounts = jodd_lib::accounts::load_accounts();
        let notes = write::do_list_tasks(
            &self.db,
            &accounts,
            &p.account_id,
            p.label.as_deref(),
            p.include_done.unwrap_or(false),
            &load_scope_for_read()?,
        )
        .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        serde_json::to_string(&notes).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("list_tasks: serializing results: {e}"), None)
        })
    }

    #[tool(
        description = "Complete or reopen one checklist task in a note. Surgical — only that checkbox changes, every other byte of the note is preserved, so this is the safe way to act on a task (never use update_note replace for it). Get indices AND expect_text from list_tasks first; expect_text is required and the call is refused if it doesn't match, since index alone is not a stable identity across notes."
    )]
    fn set_task_state(&self, Parameters(p): Parameters<SetTaskStateParams>) -> Result<String, rmcp::ErrorData> {
        let scope = load_scope_for_write()?;
        let accounts = jodd_lib::accounts::load_accounts();
        write::do_set_task_state(&self.db, &accounts, &scope, &p.account_id, &p.uuid,
            p.index, p.checked, &p.expect_text)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        ok_json(serde_json::json!({ "uuid": p.uuid, "index": p.index, "checked": p.checked }))
    }

    #[tool(
        description = "Pin or unpin a note. Pinned notes sort to the top in Jodd's own UI. The note must be inside the account's allowed_folders (call list_accounts first) — checked against its current folder, not wherever it was when you found the uuid."
    )]
    fn set_pin(&self, Parameters(p): Parameters<SetPinParams>) -> Result<String, rmcp::ErrorData> {
        let scope = load_scope_for_write()?;
        let accounts = jodd_lib::accounts::load_accounts();
        write::do_set_pin(&self.db, &accounts, &scope, &p.account_id, &p.uuid, p.pinned)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        ok_json(serde_json::json!({ "uuid": p.uuid, "pinned": p.pinned }))
    }

    #[tool(
        description = "Create a folder in a Jodd account. Path must be a full label under Notes/ and inside the account's allowed_folders (call list_accounts first). Missing ancestor folders are created automatically."
    )]
    fn create_folder(&self, Parameters(p): Parameters<CreateFolderParams>) -> Result<String, rmcp::ErrorData> {
        let scope = load_scope_for_write()?;
        let accounts = jodd_lib::accounts::load_accounts();
        write::do_create_folder(&self.db, &accounts, &scope, &p.account_id, &p.path)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        ok_json(serde_json::json!({ "path": p.path, "created": true }))
    }

    #[tool(
        description = "Read one note in full, as text with its lines kept. Long notes come in pages of 20000 characters: pass the returned next_offset to continue. The verbatim source text an ingest saved is left out unless include_source is true."
    )]
    fn read_note(&self, Parameters(p): Parameters<ReadNoteParams>) -> Result<String, rmcp::ErrorData> {
        let scope = load_scope_for_read()?;
        let note = memory::do_read_note(&self.db, &scope, &p.account_id, &p.uuid, p.offset.unwrap_or(0), p.include_source.unwrap_or(false))
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        ok_json(serde_json::to_value(note).map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?)
    }

    #[tool(
        description = "Save one line of knowledge to the user's agent workspace in Jodd, where every agent and the user can find it later. Jodd decides the page: project (a repository) → Projects/<project>; area (a subject) → Areas/<area>; a preference with neither → About me; otherwise Inbox. Use this for decisions and their reasons, lessons, user preferences, stable facts and useful sources — not for secrets or long text (write a note with create_note and remember a line pointing to it)."
    )]
    async fn remember(&self, peer: Peer<RoleServer>, Parameters(p): Parameters<RememberParams>) -> Result<String, rmcp::ErrorData> {
        let scope = load_scope_for_write()?;
        let client = peer.peer_info().map(|i| i.client_info.name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "agent".into());
        let project = match (&p.project, &p.area) {
            (None, None) => project_from_roots(&peer, &client).await,
            _ => p.project.clone(),
        };
        let relates_to = p.relates_to.unwrap_or_default();
        let accounts = jodd_lib::accounts::load_accounts();
        let saved = memory::do_remember(&self.db, &accounts, &scope, &memory::RememberArgs {
            kind: p.kind.into(),
            text: &p.text,
            project: project.as_deref(),
            area: p.area.as_deref(),
            new_area: p.new_area.unwrap_or(false),
            relates_to: &relates_to,
            client: &client,
        })
        .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        ok_json(serde_json::to_value(saved).map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?)
    }

    #[tool(
        description = "List the pages of the user's agent workspace — projects, areas, About me, Inbox — with each page's one-line summary and how many lines it holds. Call it before remember with an area, to reuse an existing area instead of inventing a near-duplicate."
    )]
    fn list_topics(&self) -> Result<String, rmcp::ErrorData> {
        let scope = load_scope_for_read()?;
        let topics = memory::do_list_topics(&self.db, &jodd_lib::accounts::load_accounts(), &scope)
            .map_err(|e| rmcp::ErrorData::invalid_params(e, None))?;
        ok_json(serde_json::to_value(topics).map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?)
    }
}

#[tool_handler]
impl ServerHandler for JoddMcp {
    fn get_info(&self) -> ServerInfo {
        // Re-read per handshake: the user may enable the workspace while the
        // client is not running; an unreadable file gets the "off" text.
        let scope = scope::load_write_scope_from(&scope::scope_path()).unwrap_or_default();
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(memory::instructions(&scope))
    }
}

/// `jodd-mcp brief [--cwd DIR] [--project NAME] [--max-chars N]`.
#[derive(Debug, PartialEq)]
struct BriefArgs {
    cwd: Option<PathBuf>,
    project: Option<String>,
    max_chars: usize,
}

fn parse_brief_args(args: &[String]) -> BriefArgs {
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    BriefArgs {
        cwd: value("--cwd").filter(|c| !c.is_empty()).map(PathBuf::from),
        project: value("--project").filter(|p| !p.trim().is_empty()),
        max_chars: value("--max-chars").and_then(|n| n.parse().ok()).filter(|n| *n > 0).unwrap_or(8_000),
    }
}

const BRIEF_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Spec §5.6: a session-start hook must never block a session. Every failure
/// prints one line to stderr and nothing to stdout, and the process exits 0.
fn run_brief(args: &[String]) {
    let a = parse_brief_args(args);
    let fail = |why: &str| eprintln!("jodd-mcp brief: {why} — no briefing");
    if let Err(e) = jodd_lib::secrets::init() {
        return fail(&e.to_string());
    }
    let db_path = resolve_db_path();
    if !db_path.exists() || jodd_lib::db_crypto::is_plaintext_sqlite(&db_path) {
        return fail("no encrypted Jodd database");
    }
    let db = match jodd_lib::db::Db::open(&db_path.parent().unwrap_or(&db_path).to_path_buf()) {
        Ok(d) => d,
        Err(e) => return fail(&format!("{e:?}")),
    };
    let scope = match scope::load_write_scope_from(&scope::scope_path()) {
        Ok(s) => s,
        Err(_) => return fail("no readable mcp_write_scope.json"),
    };
    let project = a.project.clone().or_else(|| {
        let dir = a.cwd.clone().or_else(|| std::env::current_dir().ok())?;
        jodd_lib::agent_memory::project::project_from_dir(&dir)
    });
    match jodd_lib::agent_memory::brief::briefing(&db, &scope, project.as_deref(), a.max_chars) {
        Ok(text) if !text.is_empty() => {
            // write_all, not print!: a hook runner that closed our stdout
            // must not turn into a panic.
            use std::io::Write;
            let _ = std::io::stdout().write_all(text.as_bytes()).and_then(|_| std::io::stdout().flush());
        }
        Ok(_) => {}
        Err(e) => fail(&e),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("brief") {
        // Spec §5.6, review finding 5: whatever happens — a keychain prompt
        // nobody answers, a locked DB, a panic — a session-start hook gets
        // an exit 0 within BRIEF_DEADLINE.
        //
        // The likeliest real-world stall is the macOS keychain prompt a
        // freshly built or updated binary raises for the DB key: in a hook
        // nobody answers it. The deadline turns that into "no briefing this
        // session"; `jodd-mcp --self-test` once, with "Always Allow", grants
        // the binary for good. The work stays on the main thread so the
        // Security framework sees an ordinary caller.
        std::thread::spawn(|| {
            std::thread::sleep(BRIEF_DEADLINE);
            eprintln!("jodd-mcp brief: gave up after {}s — no briefing", BRIEF_DEADLINE.as_secs());
            std::process::exit(0);
        });
        let rest = argv[2..].to_vec();
        let _ = std::panic::catch_unwind(|| run_brief(&rest));
        std::process::exit(0);
    }

    // Required unconditionally, not just for --self-test: Db::open() now
    // reads/writes the OS keychain for the DB cipher key on every call, so
    // every path through main() needs the credential store registered
    // first — the same registration Jodd.app performs at startup.
    jodd_lib::secrets::init().map_err(|e| anyhow::anyhow!(e))?;

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--self-test") {
        return match jodd_lib::db_crypto::self_test() {
            Ok(()) => {
                println!("jodd-mcp --self-test: OK — keychain access to the notes database is granted.");
                Ok(())
            }
            Err(e) => {
                eprintln!("jodd-mcp --self-test: FAILED — {e}");
                std::process::exit(1);
            }
        };
    }

    let db_path = resolve_db_path();
    // db_path here is the FILE, but Db::open takes the DIRECTORY and appends
    // "jodd.sqlite3" itself (src-tauri/src/db.rs:142-144) — pass the parent.
    let db_dir = db_path.parent().unwrap_or(&db_path).to_path_buf();

    if db_path.exists() && jodd_lib::db_crypto::is_plaintext_sqlite(&db_path) {
        eprintln!(
            "jodd-mcp: {} is not yet encrypted. Open Jodd.app once to run the \
             one-time migration to at-rest encryption, then retry.",
            db_path.display()
        );
        std::process::exit(1);
    }

    let db = match jodd_lib::db::Db::open(&db_dir) {
        Ok(d) => Arc::new(d),
        Err(jodd_lib::db_crypto::DbOpenError::KeyMismatchOrCorrupt) => {
            eprintln!(
                "jodd-mcp: the stored key does not decrypt {}. Run \
                 `jodd-mcp --self-test` once, or open Jodd.app, then retry.",
                db_path.display()
            );
            std::process::exit(1);
        }
        Err(e) => return Err(anyhow::anyhow!(e)),
    };
    eprintln!("jodd-mcp: opened DB at {}", db_path.display());

    let server = JoddMcp {
        db,
        tool_router: JoddMcp::tool_router(),
    };
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn root_dirs_are_decoded_and_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("My Project ทดสอบ");
        std::fs::create_dir_all(&spaced).unwrap();
        let uri = url::Url::from_file_path(&spaced).unwrap().to_string();
        assert!(uri.contains("%20"), "{uri}");
        assert_eq!(root_dir(&uri), Some(spaced.clone()));
        assert_eq!(root_dir(&format!("{uri}-missing")), None);
        assert_eq!(root_dir("https://example.com/x"), None);
        assert_eq!(root_dir("not a uri"), None);
    }

    #[test]
    fn brief_args_parse_with_safe_defaults() {
        assert_eq!(parse_brief_args(&args(&[])), BriefArgs { cwd: None, project: None, max_chars: 8_000 });
        assert_eq!(
            parse_brief_args(&args(&["--cwd", "/r/Jodd", "--project", "Jodd", "--max-chars", "300"])),
            BriefArgs { cwd: Some(PathBuf::from("/r/Jodd")), project: Some("Jodd".into()), max_chars: 300 }
        );
        // An unexpanded hook variable or a bad number never breaks the hook.
        assert_eq!(parse_brief_args(&args(&["--cwd", "", "--max-chars", "lots"])).max_chars, 8_000);
        assert_eq!(parse_brief_args(&args(&["--cwd", ""])).cwd, None);
    }

    /// Review Focus 1: a hidden note must not leak through a side door. Every
    /// tool is either a write tool or a read tool that takes the read scope —
    /// a new tool fails this test until someone decides which it is.
    #[test]
    fn every_tool_is_classified_and_every_read_tool_filters_hidden_folders() {
        const READS: &[&str] = &["search_notes", "note_connections", "list_accounts", "list_tasks", "read_note", "list_topics"];
        const WRITES: &[&str] = &["create_note", "update_note", "set_task_state", "set_pin", "create_folder", "remember"];
        let full = include_str!("main.rs");
        // Only the code above this test module — the test itself spells `#[tool(`.
        let src = &full[..full.find("#[cfg(test)]\nmod tests").unwrap()];
        let mut found = Vec::new();
        for chunk in src.split("#[tool(").skip(1) {
            let after = chunk.split(")]").skip(1).collect::<Vec<_>>().join(")]");
            let name = after.split("fn ").nth(1).and_then(|r| r.split('(').next()).unwrap().trim().to_string();
            let body = after.split("\n    #[tool(").next().unwrap_or("");
            assert!(READS.contains(&name.as_str()) || WRITES.contains(&name.as_str()), "unclassified tool {name}");
            if READS.contains(&name.as_str()) && name != "list_accounts" {
                assert!(body.contains("load_scope_for_read()"), "read tool {name} does not load the read scope");
            }
            found.push(name);
        }
        assert_eq!(found.len(), READS.len() + WRITES.len(), "{found:?}");
    }
}
