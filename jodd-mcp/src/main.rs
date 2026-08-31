use std::path::PathBuf;
use std::sync::Arc;

mod scope;
mod write;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ServerHandler, ServiceExt,
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
        let result = write::do_note_connections(&self.db, &params.account_id, &params.uuid)
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
}

#[tool_handler]
impl ServerHandler for JoddMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
