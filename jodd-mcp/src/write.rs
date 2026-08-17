use crate::scope::WriteScope;
use jodd_lib::accounts::Account;

#[derive(serde::Serialize)]
pub struct AccountEntry {
    pub account_id: String,
    pub backend_kind: String,
    pub allowed_folders: Vec<String>,
}

pub fn do_list_accounts(accounts: &[Account], scope: &WriteScope) -> Vec<AccountEntry> {
    accounts
        .iter()
        .filter(|a| a.is_active())
        .map(|a| AccountEntry {
            account_id: a.id.clone(),
            backend_kind: format!("{:?}", a.backend_kind),
            allowed_folders: scope
                .accounts
                .get(&a.id)
                .map(|s| s.allowed_folders.clone())
                .unwrap_or_default(),
        })
        .collect()
}

pub fn resolve_active_account<'a>(
    accounts: &'a [Account],
    account_id: &str,
) -> Result<&'a Account, String> {
    accounts
        .iter()
        .find(|a| a.id == account_id && a.is_active())
        .ok_or_else(|| format!(
            "Unknown or inactive account '{account_id}'. Call list_accounts to see valid account_ids and their allowed folders."
        ))
}

/// `resolve_active_account`, plus a refusal for a write this backend cannot
/// perform in this area (notes / folders / sidecars).
///
/// The Tauri command layer has its own copy of this check (`refuse_write` in
/// lib.rs), but this server writes to the SAME SQLite file without going
/// through it. A dirty row created here is just as unpushable, and just as
/// permanent: the worker's push fails forever, `has_pending_pushes` never
/// empties, so the account can never leave Draining and can never be removed.
pub fn resolve_writable_account<'a>(
    accounts: &'a [Account],
    account_id: &str,
    w: jodd_lib::backend::Write,
) -> Result<&'a Account, String> {
    let account = resolve_active_account(accounts, account_id)?;
    if !jodd_lib::backend::Capabilities::for_backend(account.backend_kind).writes.allows(w) {
        let area = match w {
            jodd_lib::backend::Write::Notes => "note edits",
            jodd_lib::backend::Write::Folders => "folder changes",
            jodd_lib::backend::Write::Sidecars => "pin and tag changes",
        };
        return Err(format!(
            "Account '{account_id}' can't accept {area} in Jodd yet. \
             Reads (search_notes, list_tasks, note_connections) still work."
        ));
    }
    Ok(account)
}

pub fn do_create_note(
    db: &jodd_lib::db::Db,
    accounts: &[Account],
    scope: &WriteScope,
    account_id: &str,
    folder: &str,
    title: &str,
    body_markdown: &str,
) -> Result<String, String> {
    let account = resolve_writable_account(accounts, account_id, jodd_lib::backend::Write::Notes)?;
    // Syntax BEFORE the allowlist, and non-negotiably so: the allowlist is a
    // string-prefix test, so `Notes/__Claude__/../../pwned` is "inside" an
    // allowlist of `["Notes/__Claude__"]` while resolving outside the vault
    // root on a LocalFs account. Validating first is what makes the prefix
    // test mean what it looks like it means.
    jodd_lib::folder_label::validate_label_path(folder)?;
    jodd_lib::folder_label::validate_note_title(title)?;
    let allowed = scope
        .accounts
        .get(&account.id)
        .map(|s| s.allowed_folders.as_slice())
        .unwrap_or(&[]);
    if !crate::scope::folder_allowed(allowed, folder) {
        return Err(format!(
            "Folder '{folder}' is not writable for account '{account_id}'. Allowed: {allowed:?}. \
             Edit mcp_write_scope.json (in Jodd's config dir) to grant it, or pick an allowed folder."
        ));
    }

    // Does this note write imply a *folder* write? `create_folder_local_new`
    // inserts a `dirty_new` folder row (plus every missing ancestor, via
    // `ensure_ancestors`) when `folder` is new, and `has_pending_pushes`
    // counts `folders WHERE sync_state != 'clean'` — so on a backend that
    // cannot push folders, such a row is unpushable forever, the account
    // never leaves Draining, and it can never be removed (gotcha #2). That
    // is the risk, and it is confined entirely to the new-folder case: when
    // the row already exists, `create_folder_local_new` is an
    // `ON CONFLICT DO NOTHING` no-op over an ancestor chain that already
    // exists, so it writes nothing at all.
    //
    // The check therefore has to be conditional. Microsoft is the first
    // backend with the notes-on/folders-off split (2026-08-15 M2 closeout —
    // Graph-created folders never reach Apple Notes, see
    // `Capabilities::for_backend`), and gating unconditionally turned every
    // `create_note` on a Microsoft account into a refusal, including notes
    // aimed at folders Apple itself created. `get_folder` is the same
    // existence question `do_create_folder` already asks.
    let folder_exists =
        db.get_folder(account_id, folder).map_err(|e| format!("get_folder: {e}"))?.is_some();
    if !folder_exists {
        resolve_writable_account(accounts, account_id, jodd_lib::backend::Write::Folders)?;
    }

    // md_to_html → taskify_checklists → sanitize_note_html. The middle step is
    // what makes an agent's `- [ ]` a checkbox somebody can actually tick
    // rather than a GFM decoration; sanitize must come last, over the rows.
    let body_html = jodd_lib::llm::markdown::sanitize_note_html(
        &jodd_lib::llm::markdown::taskify_checklists(&jodd_lib::llm::markdown::md_to_html(
            body_markdown,
        )),
    );

    if !folder_exists {
        db.create_folder_local_new(account_id, folder).map_err(|e| format!("ensure folder: {e}"))?;
    }

    // Mirrors extract_note's insert (lib.rs:4055-4083): fresh Apple-format
    // UUID, Dirty, empty remote id until the worker pushes.
    let uuid = jodd_lib::mime822::format_apple_uuid(uuid::Uuid::new_v4());
    let now = jodd_lib::db::now_ms();
    let note = jodd_lib::db::CachedNote {
        uuid: uuid.clone(),
        account_id: account_id.to_string(),
        id: String::new(),
        title: title.to_string(),
        body_html,
        date: chrono::Local::now().to_rfc2822(),
        x_mail_created_date: None,
        label: folder.to_string(),
        local_version: 1,
        remote_version: None,
        sync_state: jodd_lib::db::SyncState::Dirty,
        last_synced_at: None,
        last_local_modified_at: now,
        last_remote_modified_at: None,
        pinned: false,
        meta_msg_id: None,
        pin_dirty: false,
    };
    db.insert_local_new(&note).map_err(|e| format!("insert_local_new: {e}"))?;
    Ok(uuid)
}

/// Re-reads the note and calls `compute` fresh on every attempt, so a retry
/// after a lost race recomputes against the LATEST state instead of blindly
/// resubmitting a stale value — the property that stops a concurrent
/// writer's edit from being silently discarded (see
/// `Db::apply_local_edit_versioned`). `compute` returning `Err` aborts
/// immediately and is NOT retried — reserved for validation failures
/// (allowlist, replace-guard, stale `expect_text`) that re-reading can't
/// fix. Exhausting `max_attempts` on version conflicts alone is the only
/// path to the final error below.
///
/// Looks the note up via `Db::note_by_uuid`, not `Db::get`, and that choice
/// is deliberate: `note_by_uuid` excludes rows with `sync_state =
/// 'deleted_pending'`, so an MCP write against a uuid the user just deleted
/// locally fails with "No note … Find uuids via search_notes" instead of
/// silently reviving it. `Db::get` has no such filter — `lib.rs`'s sibling
/// `apply_local_edit_with_retry` uses `Db::get` because none of its callers
/// (Extract re-ingest, auto-link appends) can be aimed at a
/// deleted-pending row in the first place. That's why this crate keeps its
/// own retry helper instead of sharing one with `lib.rs` (see the plan's
/// Global Constraints, docs/superpowers/plans/
/// 2026-08-12-concurrent-local-writer-race.md) — unifying them would blur
/// this intentional lookup difference.
fn write_with_retry(
    db: &jodd_lib::db::Db,
    account_id: &str,
    uuid: &str,
    max_attempts: u32,
    mut compute: impl FnMut(&jodd_lib::db::CachedNote) -> Result<(String, String), String>,
) -> Result<(), String> {
    for _ in 0..max_attempts {
        let existing = db
            .note_by_uuid(account_id, uuid)
            .map_err(|e| format!("lookup: {e}"))?
            .ok_or_else(|| format!("No note '{uuid}' in account '{account_id}'. Find uuids via search_notes."))?;
        let (new_title, new_body) = compute(&existing)?;
        let applied = db
            .apply_local_edit_versioned(uuid, account_id, &new_title, &new_body, &existing.label, existing.local_version)
            .map_err(|e| format!("apply_local_edit: {e}"))?;
        if applied {
            return Ok(());
        }
    }
    Err(format!(
        "Note '{uuid}' is being edited by another process right now; try again."
    ))
}

#[derive(Clone, Copy)]
pub enum UpdateMode {
    Append,
    Replace,
}

pub fn do_update_note(
    db: &jodd_lib::db::Db,
    accounts: &[Account],
    scope: &WriteScope,
    account_id: &str,
    uuid: &str,
    body_markdown: &str,
    mode: UpdateMode,
    title: Option<&str>,
    force: bool,
) -> Result<(), String> {
    let account = resolve_writable_account(accounts, account_id, jodd_lib::backend::Write::Notes)?;
    if let Some(t) = title {
        jodd_lib::folder_label::validate_note_title(t)?;
    }
    let allowed = scope
        .accounts
        .get(&account.id)
        .map(|s| s.allowed_folders.as_slice())
        .unwrap_or(&[]);

    use jodd_lib::llm::markdown::{is_replace_safe, md_to_html, sanitize_note_html, taskify_checklists};
    let fragment = sanitize_note_html(&taskify_checklists(&md_to_html(body_markdown)));

    write_with_retry(db, account_id, uuid, 5, |existing| {
        // Check the note's CURRENT label — a stale uuid from an earlier search
        // must not reach outside the sandbox after a manual move (spec §4.3).
        // Re-checked on every retry attempt, not just the first, since a retry
        // means the row genuinely changed underneath us.
        if !crate::scope::folder_allowed(allowed, &existing.label) {
            return Err(format!(
                "Note '{uuid}' lives in '{}', which is not writable for '{account_id}'. Allowed: {allowed:?}. \
                 Edit mcp_write_scope.json to grant it.",
                existing.label
            ));
        }

        let new_body = match mode {
            // Existing bytes pass through untouched — the whole fidelity story
            // (spec §4.4). Two well-formed fragments concatenated stay well-formed.
            UpdateMode::Append => format!("{}{}", existing.body_html, fragment),
            UpdateMode::Replace => {
                if !force && !is_replace_safe(&existing.body_html) {
                    return Err(format!(
                        "Replace refused: note '{uuid}' contains markup outside Jodd's safe subset — \
                         an <input> element (Apple/Jodd checklist state), an attachment reference, or \
                         other foreign markup that a full-body replace would silently destroy. \
                         Use mode=\"append\" to add content safely, or pass force=true only if \
                         destroying that content is intended."
                    ));
                }
                fragment.clone()
            }
        };

        let new_title = title.unwrap_or(&existing.title).to_string();
        Ok((new_title, new_body))
    })
}

/// One checklist row inside a note, in document order.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskItem {
    /// 0-based position of this checkbox within the note, counting every
    /// checkbox regardless of state. Absolute — `set_task_state` (Task 8d)
    /// takes this index, so filtering by `include_done` must never renumber.
    pub index: usize,
    pub text: String,
    pub checked: bool,
    /// Nesting depth, `margin-left / 28` (Jodd's own convention — see
    /// `NoteEditor.svelte:443-460` and `taskify_checklists`'s
    /// `TASK_INDENT_PX`). 0 for a top-level row.
    pub level: usize,
}

/// One note's checklist, for `list_tasks`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteTasks {
    pub uuid: String,
    pub title: String,
    pub label: String,
    pub tasks: Vec<TaskItem>,
}

/// The write side of the indent convention, imported rather than
/// re-declared: `taskify_checklists` stamps `margin-left: {28 × depth}px`
/// and this crate divides it back out. Two `const … = 28` declarations in
/// two crates, each with a comment pointing at the other, is the arrangement
/// that survives review and then drifts (finding I3).
use jodd_lib::llm::markdown::{TASK_INDENT_MAX_PX, TASK_INDENT_PX};

/// Deepest level the editor can round-trip, derived from the same pair of
/// constants the write side clamps with. A body carrying a deeper
/// `margin-left` (hand-edited, or authored elsewhere) reports the deepest
/// real level instead of one the editor could never produce.
const TASK_MAX_LEVEL: usize = TASK_INDENT_MAX_PX / TASK_INDENT_PX;

fn is_checkbox_input(node: &markup5ever_rcdom::Handle) -> bool {
    match &node.data {
        markup5ever_rcdom::NodeData::Element { name, attrs, .. } if &*name.local == "input" => {
            attrs
                .borrow()
                .iter()
                .any(|a| &*a.name.local == "type" && a.value.eq_ignore_ascii_case("checkbox"))
        }
        _ => false,
    }
}

fn element_attr(node: &markup5ever_rcdom::Handle, key: &str) -> Option<String> {
    match &node.data {
        markup5ever_rcdom::NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|a| &*a.name.local == key)
            .map(|a| a.value.to_string()),
        _ => None,
    }
}

/// `margin-left: <n>px` out of a `style` attribute value, Jodd's only use of
/// inline style on a task row (see `is_margin_left_only` in
/// `llm/markdown.rs`).
fn margin_left_px(style: &str) -> Option<usize> {
    for decl in style.split(';') {
        let (prop, val) = decl.split_once(':')?;
        if prop.trim().eq_ignore_ascii_case("margin-left") {
            return val.trim().strip_suffix("px")?.parse().ok();
        }
    }
    None
}

/// Concatenate every text descendant of `row`, skipping the checkbox
/// `<input>` itself (it carries no text), and trim the result — the row's
/// "text content" the brief specifies.
fn row_text(row: &markup5ever_rcdom::Handle) -> String {
    fn walk(node: &markup5ever_rcdom::Handle, out: &mut String) {
        match &node.data {
            markup5ever_rcdom::NodeData::Text { contents } => out.push_str(&contents.borrow()),
            markup5ever_rcdom::NodeData::Element { .. } => {
                for child in node.children.borrow().iter() {
                    walk(child, out);
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    for child in row.children.borrow().iter() {
        if is_checkbox_input(child) {
            continue;
        }
        walk(child, &mut out);
    }
    out.trim().to_string()
}

/// Document-order scan for checkbox rows. `node`'s children are walked in
/// order; when a child is a checkbox `<input>`, `node` is the row (Jodd's
/// task rows are `<div><input type="checkbox">...text</div>`, so the
/// checkbox's immediate parent is always the row) and a `TaskItem` is
/// recorded immediately — *before* recursing further — so the resulting
/// order is a true preorder walk, matching the byte order the HTML source
/// was written in.
///
/// This must equal `set_checkbox_state`'s byte-order tag scan (Task 8d):
/// that function counts `<input>` tags as it meets them scanning forward
/// through the raw bytes, which is exactly source/document order. A
/// two-pass version of this walk — record every direct-child checkbox of
/// `node` first, THEN recurse into `node`'s children — produces level order
/// instead: a checkbox nested one level deeper than a later sibling
/// checkbox would be emitted *after* it, even though it appears earlier in
/// the byte stream. `expect_text`/index-based callers (`set_task_state`)
/// rely on this function and the byte scanner agreeing on what "index N"
/// means, so the single interleaved pass below — recurse immediately after
/// recording, before moving to the next sibling — is load-bearing, not a
/// style choice. See `parse_tasks_and_set_checkbox_state_agree_on_nested_checkboxes`.
fn collect_tasks(node: &markup5ever_rcdom::Handle, out: &mut Vec<TaskItem>) {
    let children = node.children.borrow();
    for child in children.iter() {
        if is_checkbox_input(child) {
            let level = element_attr(node, "style")
                .and_then(|s| margin_left_px(&s))
                .map(|px| (px / TASK_INDENT_PX).min(TASK_MAX_LEVEL))
                .unwrap_or(0);
            out.push(TaskItem {
                index: out.len(),
                text: row_text(node),
                checked: element_attr(child, "checked").is_some(),
                level,
            });
        }
        collect_tasks(child, out);
    }
}

/// Parse every checkbox row out of a note body, in document order. `index`
/// is the 0-based position of each `input[type=checkbox]` regardless of
/// state — callers that filter by `checked` must not renumber afterwards
/// (see `TaskItem::index`).
pub fn parse_tasks(body_html: &str) -> Vec<TaskItem> {
    use html5ever::tendril::TendrilSink;
    use markup5ever_rcdom::RcDom;

    let dom = html5ever::parse_fragment(
        RcDom::default(),
        Default::default(),
        html5ever::QualName::new(None, html5ever::ns!(html), html5ever::local_name!("body")),
        vec![],
        false,
    )
    .one(body_html);

    let mut tasks = Vec::new();
    if let Some(root) = dom.document.children.borrow().first().cloned() {
        collect_tasks(&root, &mut tasks);
    }
    tasks
}

/// Cap on a note-body preview, in `char`s.
///
/// This bounds ONE field of ONE row; the response bound is the product of
/// this, [`PREVIEW_TITLE_MAX_CHARS`] and [`SEARCH_MAX_ROWS`] — see the
/// arithmetic on `SEARCH_MAX_ROWS`. A per-field cap alone does not bound a
/// response: 200 rows × 240 chars is 48,000 characters of preview text
/// before a single other field, which is already the same order as the
/// ~52,000-character live failure this exists to stay under.
pub const SEARCH_PREVIEW_MAX_CHARS: usize = 240;

/// Cap on a note `title` in a tool response, in `char`s. Titles are normally
/// far shorter; this is defensive — a title is agent-visible, unbounded, and
/// was the one field left uncapped on an otherwise bounded path.
pub const PREVIEW_TITLE_MAX_CHARS: usize = 120;

/// Row cap on the `search_notes` and `note_connections` **tool** paths.
///
/// `Db::search_notes` returns up to 200 rows (its own `LIMIT`), and the app's
/// UI still gets all of them. An MCP response is a different budget: it lands
/// verbatim in an LLM's context.
///
/// For `note_connections`, this bounds `outgoing.len() + backlinks.len()`
/// combined, not each list independently — `do_note_connections` fills
/// `outgoing` first and gives `backlinks` whatever budget remains. Capping
/// each list at `SEARCH_MAX_ROWS` separately would let a hub note's response
/// reach ~2× the arithmetic below.
///
/// Worst case per row, as JSON (`backend::Note`'s serialized fields):
///
/// ```text
///   body_html  240   (SEARCH_PREVIEW_MAX_CHARS)
///   title      120   (PREVIEW_TITLE_MAX_CHARS)
///   uuid        36    id          ~20
///   date        31    x_mail_created_date  31
///   label      ~80    account_id  ~40    pinned  5
///   field names + JSON punctuation      ~130
///   ─────────────────────────────────────────
///   ≈ 735 chars/row
/// ```
///
/// × 50 rows ≈ 37,000 characters — comfortably under the ~52,000-character
/// smallest observed live client failure, with ~30% headroom. At the old
/// 200-row limit the same arithmetic gives ≈ 147,000, roughly 3× the
/// threshold: the preview cap alone did NOT bound the response.
///
/// `label` is the one field with no hard cap (a folder path is user-authored
/// and validated to 200 bytes per segment, not in total); the ~80 above is a
/// realistic depth, not a guarantee.
/// `search_notes_response_stays_under_the_observed_failure_threshold`
/// measures the real number on a worst-case fixture: **31,201 characters /
/// 36,601 bytes** (624 chars/row), vs ~125,000 characters for the same
/// fixture at 200 rows.
///
/// The bound is stated in **characters**, the unit the live failure was
/// observed in. Bytes are up to 3× that for all-Thai content — a
/// pathological all-Thai vault could reach ~90KB. That is accepted: token
/// budgets, not byte counts, are what an MCP client actually strains
/// against, and no fixed row count bounds both units tightly.
///
/// Rows are already rank-ordered (`search_notes`) or newest-first
/// (`note_connections`), so this keeps the best ones. It is stated in both
/// tool descriptions and in `jodd-mcp/README.md` so a caller that needs more
/// narrows the scope rather than silently believing it saw everything.
pub const SEARCH_MAX_ROWS: usize = 50;

/// Bound every agent-visible free-text field of one row. Applied on the MCP
/// **tool** paths only.
///
/// The body is HTML and goes through `html_to_preview`. The title is NOT: it
/// is stored as plain text, so parsing it as HTML would silently eat a real
/// title — `ทดสอบ Title <h3>` (an actual title from live testing) strips
/// down to `ทดสอบ Title`, and a literal `&amp;` would decode to `&`. It is
/// only truncated, on a char boundary.
fn previewize(note: &mut jodd_lib::backend::Note) {
    note.body_html = html_to_preview(&note.body_html, SEARCH_PREVIEW_MAX_CHARS);
    note.title = note.title.chars().take(PREVIEW_TITLE_MAX_CHARS).collect();
}

/// Strip HTML tags out of `body_html`, collapse whitespace runs to a single
/// space, and truncate to at most `max_chars` characters on a
/// char-boundary-safe cut. Used only by the `search_notes` and
/// `note_connections` MCP **tools** (via `previewize`) to bound response
/// size and avoid flowing full note bodies — including any secrets they
/// contain — unbounded into an LLM's context. `Db::search_notes`,
/// `Db::outgoing_links` and `Db::backlinks` are untouched; the app's own UI
/// still gets full `body_html` for its note preview rendering.
///
/// Parses via the same html5ever/rcdom stack as `parse_tasks` above (the
/// one parser/serializer pinned for this workspace) rather than a regex or
/// hand-rolled `<...>` stripper, which would mangle entities (`&amp;`,
/// `&nbsp;`) and can leave stray angle-bracket fragments from malformed
/// input.
pub fn html_to_preview(body_html: &str, max_chars: usize) -> String {
    use html5ever::tendril::TendrilSink;
    use markup5ever_rcdom::{NodeData, RcDom};

    fn walk(node: &markup5ever_rcdom::Handle, out: &mut String) {
        match &node.data {
            NodeData::Text { contents } => out.push_str(&contents.borrow()),
            // `<script>`/`<style>` text is not visible content — a browser
            // never renders it. Surfacing it in a preview would put code (and
            // whatever a crafted note hid inside it) into an LLM's context
            // while looking, to the user, like an empty note. Jodd's
            // sanitizer should already have removed these on write; this is
            // insurance on the read path, which also serves bodies that
            // arrived from Apple/Gmail rather than through Jodd.
            NodeData::Element { name, .. }
                if matches!(name.local.as_ref(), "script" | "style") =>
            {
                out.push(' ');
            }
            NodeData::Element { .. } => {
                for child in node.children.borrow().iter() {
                    walk(child, out);
                }
                // A space between block-level fragments (e.g. adjacent
                // `<div>`s) keeps words from two different rows from
                // running together, matching how a browser would render
                // them on separate lines.
                out.push(' ');
            }
            _ => {}
        }
    }

    let dom = html5ever::parse_fragment(
        RcDom::default(),
        Default::default(),
        html5ever::QualName::new(None, html5ever::ns!(html), html5ever::local_name!("body")),
        vec![],
        false,
    )
    .one(body_html);

    let mut raw = String::new();
    if let Some(root) = dom.document.children.borrow().first().cloned() {
        walk(&root, &mut raw);
    }

    // Collapse any run of whitespace (spaces, tabs, newlines — including the
    // separators `walk` injected between elements) to a single space.
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");

    // char-boundary-safe truncation: take the first `max_chars` chars, not
    // bytes, so multi-byte text (Thai, emoji, ...) is never sliced mid-
    // codepoint.
    collapsed.chars().take(max_chars).collect()
}

/// The `search_notes` **tool**'s path: calls `Db::search_notes` (unchanged),
/// bounds each row's free-text fields (`previewize`) and caps the row count
/// at [`SEARCH_MAX_ROWS`] before the MCP layer serializes them.
/// `Db::search_notes` itself, and every other caller of it (the app's own
/// UI), still gets full body content and all 200 rows — both bounds are
/// scoped to this one tool's response.
pub fn do_search_notes(
    db: &jodd_lib::db::Db,
    account_id: Option<&str>,
    label: Option<&str>,
    query: &str,
    exclude_accounts: &[String],
) -> Result<Vec<jodd_lib::backend::Note>, String> {
    let rows = db
        .search_notes(account_id, label, query, exclude_accounts)
        .map_err(|e| format!("search_notes: {e}"))?;
    Ok(rows
        .iter()
        .take(SEARCH_MAX_ROWS)
        .map(|n| {
            let mut note = n.to_frontend_note();
            previewize(&mut note);
            note
        })
        .collect())
}

/// A note's `[[wikilink]]` neighbourhood, as the `note_connections` tool
/// returns it.
#[derive(serde::Serialize)]
pub struct NoteConnectionsResult {
    pub outgoing: Vec<jodd_lib::backend::Note>,
    pub backlinks: Vec<jodd_lib::backend::Note>,
}

/// The `note_connections` **tool**'s path, mirroring `do_search_notes`.
///
/// Both link lists carry whole notes, so before this existed the tool
/// returned full, untruncated `body_html` for every neighbour — the same
/// secret-exposure and response-size class `search_notes` had already been
/// fixed for, left open in a second tool, and a way to read any wikilinked
/// note's complete content by uuid. `Db::outgoing_links`/`Db::backlinks`
/// are untouched: the app's own graph view still gets full bodies.
///
/// The [`SEARCH_MAX_ROWS`] budget is shared across **both** lists combined,
/// not applied to each independently — a hub note with 50+ outgoing links
/// and 50+ backlinks would otherwise serialize ~100 rows, breaking the same
/// response-size bound this cap exists to hold. `outgoing` fills first (it
/// answers "what does this note reference", the more common `note_connections`
/// use), then `backlinks` gets whatever budget remains.
pub fn do_note_connections(
    db: &jodd_lib::db::Db,
    account_id: &str,
    uuid: &str,
) -> Result<NoteConnectionsResult, String> {
    let preview_up_to = |rows: Vec<jodd_lib::db::CachedNote>, limit: usize| {
        rows.iter()
            .take(limit)
            .map(|n| {
                let mut note = n.to_frontend_note();
                previewize(&mut note);
                note
            })
            .collect::<Vec<_>>()
    };
    let outgoing = db
        .outgoing_links(account_id, uuid)
        .map_err(|e| format!("note_connections: outgoing_links: {e}"))?;
    let backlinks = db
        .backlinks(account_id, uuid)
        .map_err(|e| format!("note_connections: backlinks: {e}"))?;
    let outgoing = preview_up_to(outgoing, SEARCH_MAX_ROWS);
    let remaining = SEARCH_MAX_ROWS - outgoing.len();
    let backlinks = preview_up_to(backlinks, remaining);
    Ok(NoteConnectionsResult {
        outgoing,
        backlinks,
    })
}

/// List checklist tasks across an account's notes (spec §4.6(B)). Read-only:
/// resolves the account for a consistent "unknown account" error but is NOT
/// allowlist-gated (it reads no more than `search_notes` already does).
/// `include_done=false` filters the returned tasks but never renumbers —
/// `index` stays the absolute position `set_task_state` (Task 8d) expects.
/// A note whose tasks are ALL filtered out is dropped from the result
/// entirely: a `{"uuid":…,"tasks":[]}` entry answers no question the caller
/// asked and, on a vault with many finished checklists, is most of the
/// response. The SQL pre-filter is a `LIKE`, so a note can also parse to
/// zero tasks with `include_done=true`; that is dropped for the same reason.
pub fn do_list_tasks(
    db: &jodd_lib::db::Db,
    accounts: &[Account],
    account_id: &str,
    label: Option<&str>,
    include_done: bool,
) -> Result<Vec<NoteTasks>, String> {
    resolve_active_account(accounts, account_id)?;
    let notes = db
        .list_notes_with_checkboxes(account_id, label)
        .map_err(|e| format!("list_notes_with_checkboxes: {e}"))?;
    Ok(notes
        .into_iter()
        .filter_map(|n| {
            let mut tasks = parse_tasks(&n.body_html);
            if !include_done {
                tasks.retain(|t| !t.checked);
            }
            if tasks.is_empty() {
                return None;
            }
            Some(NoteTasks { uuid: n.uuid, title: n.title, label: n.label, tasks })
        })
        .collect())
}

/// Byte span of a single `<input ...>` (or `<input .../>`) tag starting at
/// `start` (the index of `<`), found by scanning forward for the matching
/// `>`. Returns the end index (exclusive) of the tag, i.e. one past `>`.
/// Jodd/Apple never put a literal `>` inside an attribute value on a task
/// row's `<input>`, so a naive scan for the next `>` is sufficient — this
/// mirrors the file's existing "no regex crate, hand-rolled scanner"
/// convention (see `extract_urls` in the deriver).
fn find_tag_end(body: &str, start: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        if bytes[i] == b'>' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// Does the byte span (a `<input ...>` tag) declare `type="checkbox"`,
/// `type='checkbox'`, or the unquoted `type=checkbox`? Case-insensitive on
/// the value, matching `is_checkbox_input`'s DOM-based check.
fn span_is_checkbox(span: &str) -> bool {
    let lower = span.to_ascii_lowercase();
    lower.contains("type=\"checkbox\"")
        || lower.contains("type='checkbox'")
        || lower.contains("type=checkbox")
}

/// Mutate the `index`-th checkbox `<input>` tag in `body_html` in place —
/// byte-surgical, not a DOM round-trip. Every byte outside that one tag's
/// span (including its own non-`checked` attributes, and any whitespace or
/// entity elsewhere in the document) is copied verbatim, so this never
/// disturbs Apple- or Jodd-authored markup Jodd didn't write itself.
pub fn set_checkbox_state(body_html: &str, index: usize, checked: bool) -> Result<String, String> {
    let bytes = body_html.as_bytes();
    let mut checkbox_count = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        // Look for the next "<input" tag start (case-insensitive, no
        // allocation — checking every '<' via a full lowercased copy of the
        // remaining tail would be quadratic on documents with many tags).
        if bytes[i] == b'<' && bytes[i..].len() >= 6 && bytes[i + 1..i + 6].eq_ignore_ascii_case(b"input") {
            let Some(end) = find_tag_end(body_html, i) else { break };
            let span = &body_html[i..end];
            if span_is_checkbox(span) {
                if checkbox_count == index {
                    let new_span = mutate_checkbox_span(span, checked);
                    let mut out = String::with_capacity(body_html.len() + new_span.len());
                    out.push_str(&body_html[..i]);
                    out.push_str(&new_span);
                    out.push_str(&body_html[end..]);
                    return Ok(out);
                }
                checkbox_count += 1;
            }
            i = end;
            continue;
        }
        i += 1;
    }
    Err(format!(
        "Task index {index} is out of range: this note has {checkbox_count} task(s), valid indices are 0-{}. Call list_tasks for current indices.",
        checkbox_count.saturating_sub(1)
    ))
}

/// Rewrite one `<input ...>` span to carry (or not carry) `checked`.
/// Handles `checked`, `checked=""`, and `checked='...'`/`checked="..."`
/// forms on the way out, and both `>` and `/>` closers on the way in.
fn mutate_checkbox_span(span: &str, checked: bool) -> String {
    let has_checked = {
        let lower = span.to_ascii_lowercase();
        // Match "checked" as a whole attribute name: preceded by whitespace,
        // followed by whitespace, '=', '/', or the closing '>'.
        let mut found = false;
        let bytes = lower.as_bytes();
        let mut idx = 0;
        while let Some(pos) = lower[idx..].find("checked") {
            let start = idx + pos;
            let end = start + "checked".len();
            let preceded_ok = start == 0 || bytes[start - 1].is_ascii_whitespace();
            let followed_ok = end >= bytes.len()
                || bytes[end].is_ascii_whitespace()
                || bytes[end] == b'='
                || bytes[end] == b'/'
                || bytes[end] == b'>';
            if preceded_ok && followed_ok {
                found = true;
                break;
            }
            idx = start + 1;
        }
        found
    };

    if checked {
        if has_checked {
            return span.to_string();
        }
        // Insert ` checked=""` just before the closing `>` (or `/>`).
        let trimmed = span.strip_suffix('>').expect("span always ends with '>'");
        if let Some(before_slash) = trimmed.strip_suffix('/') {
            format!("{before_slash} checked=\"\"/>")
        } else {
            format!("{trimmed} checked=\"\">")
        }
    } else {
        if !has_checked {
            return span.to_string();
        }
        // Remove the `checked` attribute (with an optional `="..."` /
        // `='...'` value) plus one adjacent space, preserving everything
        // else byte-for-byte.
        let lower = span.to_ascii_lowercase();
        let bytes = lower.as_bytes();
        let mut start = 0usize;
        loop {
            let pos = lower[start..].find("checked").expect("checked is present");
            let attr_start = start + pos;
            let attr_end = attr_start + "checked".len();
            let preceded_ok =
                attr_start == 0 || bytes[attr_start - 1].is_ascii_whitespace();
            let followed_ok = attr_end >= bytes.len()
                || bytes[attr_end].is_ascii_whitespace()
                || bytes[attr_end] == b'='
                || bytes[attr_end] == b'/'
                || bytes[attr_end] == b'>';
            if preceded_ok && followed_ok {
                // Extend over an optional `="value"` / `='value'`.
                let mut value_end = attr_end;
                let rest = &span[attr_end..];
                if let Some(after_eq) = rest.strip_prefix('=') {
                    if let Some(q) = after_eq.chars().next() {
                        if q == '"' || q == '\'' {
                            if let Some(close) = after_eq[1..].find(q) {
                                value_end = attr_end + 1 + 1 + close + 1;
                            }
                        }
                    }
                }
                // Drop one adjacent space: prefer the leading space (so
                // "<input checked type=..." -> "<input type=..."), else a
                // trailing one.
                let mut remove_start = attr_start;
                let mut remove_end = value_end;
                if remove_start > 0 && span.as_bytes()[remove_start - 1] == b' ' {
                    remove_start -= 1;
                } else if remove_end < span.len() && span.as_bytes()[remove_end] == b' ' {
                    remove_end += 1;
                }
                let mut out = String::with_capacity(span.len());
                out.push_str(&span[..remove_start]);
                out.push_str(&span[remove_end..]);
                return out;
            }
            start = attr_start + 1;
        }
    }
}

/// Toggle one task's checkbox in a note, without touching any other byte of
/// the body (spec §4.6(C)) — the surgical alternative `update_note`'s
/// replace-mode refusal points callers to. Resolves the account, checks the
/// note's CURRENT label against the allowlist (a stale uuid must not reach
/// outside the sandbox after a manual move), guards against a stale/wrong
/// index via `expect_text` (required — `index` alone is not a stable
/// identity, so a caller that skips this check has zero protection against
/// ticking the wrong note's checkbox), then writes via `apply_local_edit`.
/// The validate-and-compute half of `do_set_task_state` — pulled out so both
/// the real retry loop and the test-only race-injection wrapper share it.
/// Re-invoked fresh on every retry attempt: the allowlist check and the
/// `expect_text` check must both be re-evaluated against the CURRENT row,
/// not the snapshot from a stale attempt — a racer could have renamed or
/// moved the note in between.
fn set_task_state_compute(
    existing: &jodd_lib::db::CachedNote,
    allowed: &[String],
    account_id: &str,
    uuid: &str,
    index: usize,
    checked: bool,
    expect_text: Option<&str>,
) -> Result<(String, String), String> {
    if !crate::scope::folder_allowed(allowed, &existing.label) {
        return Err(format!(
            "Note '{uuid}' lives in '{}', which is not writable for '{account_id}'. Allowed: {allowed:?}. \
             Edit mcp_write_scope.json to grant it.",
            existing.label
        ));
    }

    if let Some(expected) = expect_text {
        let tasks = parse_tasks(&existing.body_html);
        let actual = tasks.get(index).map(|t| t.text.as_str());
        if actual != Some(expected) {
            return Err(match actual {
                Some(actual_text) => format!(
                    "expect_text mismatch: task {index} is actually \"{actual_text}\", not \"{expected}\". \
                     Call list_tasks for current indices before retrying."
                ),
                None => format!(
                    "expect_text mismatch: task index {index} does not exist (this note has {} task(s)). \
                     Call list_tasks for current indices before retrying.",
                    tasks.len()
                ),
            });
        }
    }

    let new_body = set_checkbox_state(&existing.body_html, index, checked)?;
    Ok((existing.title.clone(), new_body))
}

/// Ticks or unticks one checklist box. `index` is 0-based document order of
/// `input[type=checkbox]` within the note — the same order `list_tasks`
/// returns. Retries against fresh state on a lost write race (see
/// `write_with_retry`); `expect_text` — required, since `index` alone is not
/// a stable identity across notes — is re-validated on every attempt so a
/// stale guard can't slip through after a retry.
pub fn do_set_task_state(
    db: &jodd_lib::db::Db,
    accounts: &[Account],
    scope: &WriteScope,
    account_id: &str,
    uuid: &str,
    index: usize,
    checked: bool,
    expect_text: &str,
) -> Result<(), String> {
    let account = resolve_writable_account(accounts, account_id, jodd_lib::backend::Write::Notes)?;
    let allowed = scope
        .accounts
        .get(&account.id)
        .map(|s| s.allowed_folders.as_slice())
        .unwrap_or(&[]);

    if expect_text.trim().is_empty() {
        return Err(
            "expect_text must be the real task text, not empty or whitespace-only. \
             Call list_tasks to read the current text at this index before retrying.".to_string(),
        );
    }

    write_with_retry(db, account_id, uuid, 5, |existing| {
        set_task_state_compute(existing, allowed, account_id, uuid, index, checked, Some(expect_text))
    })
}

#[cfg(test)]
fn do_set_task_state_with_retry_hook(
    db: &jodd_lib::db::Db,
    accounts: &[Account],
    scope: &WriteScope,
    account_id: &str,
    uuid: &str,
    index: usize,
    checked: bool,
    expect_text: Option<&str>,
    on_first_attempt: &mut dyn FnMut(),
) -> Result<(), String> {
    let account = resolve_writable_account(accounts, account_id, jodd_lib::backend::Write::Notes)?;
    let allowed = scope
        .accounts
        .get(&account.id)
        .map(|s| s.allowed_folders.as_slice())
        .unwrap_or(&[]);
    let mut first = true;
    write_with_retry(db, account_id, uuid, 5, |existing| {
        if first {
            first = false;
            on_first_attempt();
        }
        set_task_state_compute(existing, allowed, account_id, uuid, index, checked, expect_text)
    })
}

/// Pin or unpin a note — mirrors the Tauri `set_pin` command
/// ([lib.rs:2589](../../src-tauri/src/lib.rs)), which is itself a thin
/// wrapper: `refuse_write(&state, &account_id, backend::Write::Sidecars)`
/// then `Db::set_pin`. This server has no `AppState`/`refuse_write` — this
/// crate's own `resolve_writable_account` above is the equivalent gate, same
/// `Capabilities` check, for the same reason spelled out on that function:
/// a `pin_dirty` row on a backend that can't push sidecars is exactly as
/// permanently unpushable as a `dirty` note or a `dirty_new` folder row, and
/// `has_pending_pushes` counts it the same way — stuck in Draining forever.
///
/// Looked up via `note_by_uuid`, not `Db::get` — the same choice
/// `write_with_retry` makes and for the same reason (see its doc comment):
/// a uuid the user just deleted locally (`deleted_pending`) must read as
/// "no such note" through this tool, not be silently actionable.
///
/// Unlike `write_with_retry`'s callers, there is no optimistic-version race
/// to guard here, so no retry loop: `Db::set_pin` writes `pinned`/
/// `pin_dirty` directly and unversioned (see its own doc comment — "Does
/// NOT touch `sync_state`: pin push is orthogonal to content push"), on a
/// column no content edit ever touches. A second local writer flipping the
/// same pin moments later is a legitimate last-write-wins, not a lost
/// update, so one lookup, one allowlist check, one write is the whole
/// function.
///
/// The allowlist check below mirrors `do_update_note`'s, not
/// `do_create_note`'s folder-existence check — pin doesn't move a note or
/// touch a folder, so at first glance it looks like the allowlist has
/// nothing to gate. But `do_update_note`'s comment makes the real shape of
/// the check clear: it isn't about folder writes specifically, it's the
/// sandbox boundary the whole write surface enforces on *the note itself*,
/// evaluated against the note's CURRENT label so a uuid captured from an
/// earlier `search_notes` call (before a manual move out of the sandbox)
/// can't reach outside the allowlist through this tool either. `scope.rs`'s
/// `folder_allowed` is a plain label match with no folder-specific
/// semantics — it is exactly as applicable to "may this tool act on this
/// note" as it is to "may this tool create this folder".
pub fn do_set_pin(
    db: &jodd_lib::db::Db,
    accounts: &[Account],
    scope: &WriteScope,
    account_id: &str,
    uuid: &str,
    pinned: bool,
) -> Result<(), String> {
    let account =
        resolve_writable_account(accounts, account_id, jodd_lib::backend::Write::Sidecars)?;
    let allowed = scope
        .accounts
        .get(&account.id)
        .map(|s| s.allowed_folders.as_slice())
        .unwrap_or(&[]);

    let existing = db
        .note_by_uuid(account_id, uuid)
        .map_err(|e| format!("lookup: {e}"))?
        .ok_or_else(|| {
            format!("No note '{uuid}' in account '{account_id}'. Find uuids via search_notes.")
        })?;

    if !crate::scope::folder_allowed(allowed, &existing.label) {
        return Err(format!(
            "Note '{uuid}' lives in '{}', which is not writable for '{account_id}'. Allowed: {allowed:?}. \
             Edit mcp_write_scope.json to grant it.",
            existing.label
        ));
    }

    db.set_pin(uuid, account_id, pinned).map_err(|e| format!("set_pin: {e}"))
}

pub fn do_create_folder(
    db: &jodd_lib::db::Db,
    accounts: &[Account],
    scope: &WriteScope,
    account_id: &str,
    path: &str,
) -> Result<(), String> {
    let account = resolve_writable_account(accounts, account_id, jodd_lib::backend::Write::Folders)?;
    // Same validator, same order, as `do_create_note` — the two used to
    // disagree (this one checked empty segments, that one checked nothing),
    // which is how the traversal escape got in. One function, both callers.
    jodd_lib::folder_label::validate_label_path(path)?;
    if path == jodd_lib::folder_label::NOTES_ROOT {
        return Err(format!(
            "'{path}' is the notes root and always exists — pass a path beneath it, \
             e.g. '{path}/Research'."
        ));
    }
    let allowed = scope
        .accounts
        .get(&account.id)
        .map(|s| s.allowed_folders.as_slice())
        .unwrap_or(&[]);
    if !crate::scope::folder_allowed(allowed, path) {
        return Err(format!(
            "Folder path '{path}' is not inside the allowed folders for '{account_id}': {allowed:?}. \
             Edit mcp_write_scope.json to grant it."
        ));
    }
    if db.get_folder(account_id, path).map_err(|e| e.to_string())?.is_some() {
        return Err(format!("Folder '{path}' already exists."));
    }
    db.create_folder_local_new(account_id, path).map_err(|e| format!("create folder: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(id: &str) -> jodd_lib::accounts::Account {
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","email":"{id}","added_at":"2026-01-01T00:00:00Z"}}"#
        )).unwrap()
    }

    /// `acct()` omits `status`, which serde defaults to Active — so every
    /// test that used only `acct()` exercised exactly one of the three
    /// lifecycle states, and the account half of the security boundary went
    /// untested (finding I4). This builds the other two.
    fn acct_with_status(id: &str, status: &str) -> jodd_lib::accounts::Account {
        let a: jodd_lib::accounts::Account = serde_json::from_str(&format!(
            r#"{{"id":"{id}","email":"{id}","added_at":"2026-01-01T00:00:00Z","status":"{status}"}}"#
        )).unwrap();
        // Guard the fixture itself: a typo in `status` would silently deserialize
        // to the serde default (Active) and make every assertion below vacuous.
        assert_ne!(
            a.status,
            jodd_lib::accounts::AccountStatus::Active,
            "fixture asked for status={status:?} but got the serde default"
        );
        a
    }

    fn scope_with(id: &str, folders: &[&str]) -> crate::scope::WriteScope {
        serde_json::from_str(&format!(
            r#"{{"accounts":{{"{id}":{{"allowed_folders":{}}}}}}}"#,
            serde_json::to_string(folders).unwrap()
        )).unwrap()
    }

    #[test]
    fn list_accounts_reports_kind_and_folders() {
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let entries = do_list_accounts(&accounts, &scope);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].account_id, "a@x.com");
        assert_eq!(entries[0].backend_kind, "Gmail"); // serde default backend_kind
        assert_eq!(entries[0].allowed_folders, vec!["Notes/__Claude__"]);
    }

    #[test]
    fn list_accounts_empty_folders_when_account_not_in_scope() {
        let entries = do_list_accounts(&[acct("b@x.com")], &crate::scope::WriteScope::default());
        assert!(entries[0].allowed_folders.is_empty());
    }

    #[test]
    fn resolve_unknown_account_teaches_list_accounts() {
        let err = resolve_active_account(&[acct("a@x.com")], "nope@x.com").unwrap_err();
        assert!(err.contains("list_accounts"), "error must teach the next call: {err}");
    }

    fn acct_with_backend(id: &str, backend: &str) -> jodd_lib::accounts::Account {
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","email":"{id}","added_at":"2026-01-01T00:00:00Z","backend_kind":"{backend}"}}"#
        )).unwrap()
    }

    /// This server writes straight to SQLite, bypassing the Tauri commands, so
    /// it needs its own copy of the per-area write refusal. A dirty row on a
    /// Microsoft account can never be pushed, which pins the account in
    /// Draining forever and makes it unremovable. Live verification on
    /// 2026-08-15 confirmed notes (real code, proven against
    /// kaiwan.h@live.com) but found folders are a measured negative —
    /// Graph-created folders never reach Apple Notes, see
    /// `Capabilities::for_backend`'s Microsoft arm in `src-tauri` for the
    /// mechanism. So only `Write::Notes` is allowed here; folders join
    /// sidecars (pin, tags, refused until M4) in being refused.
    #[test]
    fn a_backend_that_cannot_write_this_area_is_refused_before_any_write() {
        use jodd_lib::backend::Write;

        let ms = [acct_with_backend("m@live.com", "microsoft")];
        assert!(
            resolve_writable_account(&ms, "m@live.com", Write::Notes).is_ok(),
            "Notes must be allowed on Microsoft — confirmed live 2026-08-15"
        );
        assert!(
            resolve_writable_account(&ms, "m@live.com", Write::Sidecars).is_ok(),
            "Sidecars (pin) must be allowed on Microsoft as of M4 — a named property on the note itself"
        );
        let err = resolve_writable_account(&ms, "m@live.com", Write::Folders).unwrap_err();
        assert!(!err.to_lowercase().contains("read-only"), "got: {err}");

        // The writable backends must be untouched by the new gate.
        for w in [Write::Notes, Write::Folders, Write::Sidecars] {
            assert!(resolve_writable_account(&[acct("a@x.com")], "a@x.com", w).is_ok());
            let local = [acct_with_backend("v@local", "local_fs")];
            assert!(resolve_writable_account(&local, "v@local", w).is_ok());
        }
    }

    /// `do_create_note` calls `create_folder_local_new`, which inserts a
    /// `dirty_new` folder row (plus ancestors) whenever `folder` is new — the
    /// same shape lib.rs's `extract_note`/`append_extract_note` have, which
    /// is why those two check both `Write::Notes` and `Write::Folders`. A
    /// backend refused for folders but allowed for notes could otherwise
    /// still queue a folder row through this path that `has_pending_pushes`
    /// (`folders WHERE sync_state != 'clean'`) can never clear.
    ///
    /// Microsoft now has exactly this split (notes on, folders off, per the
    /// 2026-08-15 live-verification closeout), which is what made the
    /// *placement* of the folder check matter as much as its presence: while
    /// it ran unconditionally, `do_create_note` refused every Microsoft call,
    /// including ones aimed at a folder that already exists and would insert
    /// no folder row at all. It is now guarded by `folder_exists`, so it
    /// fires exactly when `create_folder_local_new` would really write.
    ///
    /// Scanning the function body is still the right check *for presence*
    /// (same idea as lib.rs's `write_kinds_declared_in_source`): it fails the
    /// moment either kind is removed, rather than trusting that the two will
    /// always happen to move in lockstep. But a substring scan cannot see
    /// *when* a check runs, which is the axis the regression above lived on —
    /// so the behavioural tests that follow are the other half of this pair,
    /// not a duplicate of it.
    #[test]
    fn create_note_checks_both_notes_and_folders_before_any_write() {
        let src = include_str!("write.rs");
        let start = src.find("pub fn do_create_note(").expect("do_create_note must exist");
        let body_start = start + src[start..].find('{').unwrap();
        let end = src[body_start..]
            .find("\npub fn ")
            .map(|i| body_start + i)
            .unwrap_or(src.len());
        let body = &src[body_start..end];
        assert!(
            body.contains("jodd_lib::backend::Write::Notes"),
            "do_create_note must check Write::Notes before inserting the note"
        );
        assert!(
            body.contains("jodd_lib::backend::Write::Folders"),
            "do_create_note must check Write::Folders before create_folder_local_new"
        );
    }

    /// The behavioural half of the pair above, and the one that catches what
    /// a source scan structurally cannot: *when* the `Write::Folders` check
    /// runs. Microsoft is the first backend with the notes-on/folders-off
    /// split, and an unconditional folder gate turns it into a blanket
    /// refusal of `create_note` — including for a folder that already exists,
    /// where `create_folder_local_new` would insert nothing at all. That is
    /// not conservatism, it is a functional regression: the whole MCP write
    /// surface for Microsoft accounts disappears the moment the capability
    /// flips, with an error message that blames folders for a note write.
    #[test]
    fn create_note_into_an_existing_folder_works_on_a_notes_on_folders_off_backend() {
        let (_dir, db) = test_db();
        let accounts = vec![acct_with_backend("m@live.com", "microsoft")];
        let scope = scope_with("m@live.com", &["Notes/__Claude__"]);

        // Arrange the case that matters: the folder is already here, pulled
        // from the remote, so it is `clean` and needs no folder write.
        db.upsert_folder_from_remote("m@live.com", "Notes/__Claude__", "AAMkFolderId")
            .unwrap();

        let uuid = do_create_note(
            &db, &accounts, &scope, "m@live.com", "Notes/__Claude__", "t", "body",
        )
        .expect("a note write into an existing folder must not be refused for folder reasons");

        let note = db.note_by_uuid("m@live.com", &uuid).unwrap().unwrap();
        assert_eq!(note.label, "Notes/__Claude__");

        // The reason the gate exists at all: nothing unpushable may be left
        // behind. `has_pending_pushes` counts `folders WHERE sync_state !=
        // 'clean'`, so the folder row must still be exactly as we found it.
        let folders = db.list_folders("m@live.com").unwrap();
        assert!(
            folders
                .iter()
                .all(|f| f.sync_state == jodd_lib::db::FolderSyncState::Clean),
            "no folder row may be left non-clean on a folders-off backend: {:?}",
            folders.iter().map(|f| (&f.path, f.sync_state)).collect::<Vec<_>>()
        );
    }

    /// The other side of the same boundary: a folder that does NOT exist is
    /// a real folder write, so it must still be refused — and must leave no
    /// row of either kind behind, since the refusal has to land before the
    /// first insert rather than halfway through.
    #[test]
    fn create_note_into_a_new_folder_is_still_refused_on_a_folders_off_backend() {
        let (_dir, db) = test_db();
        let accounts = vec![acct_with_backend("m@live.com", "microsoft")];
        let scope = scope_with("m@live.com", &["Notes/__Claude__"]);

        let err = do_create_note(
            &db, &accounts, &scope, "m@live.com", "Notes/__Claude__", "t", "body",
        )
        .unwrap_err();
        assert!(err.contains("folder changes"), "must name the refused area: {err}");

        assert!(
            db.list_folders("m@live.com").unwrap().is_empty(),
            "a refused create_note must not have inserted the folder"
        );
        assert!(
            db.list_notes("m@live.com").unwrap().is_empty(),
            "a refused create_note must not have inserted the note either"
        );
    }

    /// A folders-off backend must not be able to sneak an *ancestor* row in
    /// either. `create_folder_local_new` calls `ensure_ancestors`, which
    /// inserts every strict ancestor below `Notes` as `dirty_new` — so
    /// `Notes/A/B` is two unpushable rows, not one, and the existence check
    /// has to be reached before any of it.
    #[test]
    fn create_note_refusal_on_a_folders_off_backend_leaves_no_ancestor_rows() {
        let (_dir, db) = test_db();
        let accounts = vec![acct_with_backend("m@live.com", "microsoft")];
        let scope = scope_with("m@live.com", &["Notes/A"]);

        do_create_note(&db, &accounts, &scope, "m@live.com", "Notes/A/B", "t", "body")
            .unwrap_err();
        assert!(
            db.list_folders("m@live.com").unwrap().is_empty(),
            "neither 'Notes/A/B' nor its ancestor 'Notes/A' may be inserted"
        );
    }

    /// A fully-writable backend keeps the old behaviour end to end: a new
    /// folder is created on demand by `create_note`, as it always was.
    #[test]
    fn create_note_still_creates_a_missing_folder_on_a_fully_writable_backend() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);

        do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__", "t", "body")
            .unwrap();
        assert!(
            db.get_folder("a@x.com", "Notes/__Claude__").unwrap().is_some(),
            "Gmail must still get the folder created for it"
        );
    }

    fn test_db() -> (tempfile::TempDir, jodd_lib::db::Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = jodd_lib::db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        (dir, db)
    }

    #[test]
    fn write_with_retry_recomputes_against_fresh_state_after_a_lost_race() {
        let (_dir, db) = test_db();
        let uuid = "AAAAAAAA-1111-2222-3333-444444444444";
        db.insert_local_new(&jodd_lib::db::CachedNote {
            uuid: uuid.to_string(),
            account_id: "a@x.com".to_string(),
            id: String::new(),
            title: "T".to_string(),
            body_html: "<p>base</p>".to_string(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 1,
            remote_version: None,
            sync_state: jodd_lib::db::SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: 0,
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
        })
        .unwrap();

        let mut first_attempt = true;
        write_with_retry(&db, "a@x.com", uuid, 5, |existing| {
            if first_attempt {
                first_attempt = false;
                // Simulate a second local writer (the App's editor, or another
                // MCP call) landing its own edit between our read and our write —
                // exactly the window `apply_local_edit_versioned` guards.
                db.apply_local_edit(uuid, "a@x.com", &existing.title, "<p>base</p><p>racer</p>", &existing.label)
                    .unwrap();
            }
            Ok((existing.title.clone(), format!("{}<p>mine</p>", existing.body_html)))
        })
        .unwrap();

        let row = db.get(uuid, "a@x.com").unwrap().unwrap();
        assert!(row.body_html.contains("racer"), "the racer's edit must survive: {}", row.body_html);
        assert!(row.body_html.contains("mine"), "our edit must also land, computed against the racer's body: {}", row.body_html);
    }

    #[test]
    fn set_task_state_retries_and_revalidates_expect_text_against_fresh_body() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let uuid = do_create_note(
            &db, &accounts, &scope, "a@x.com", "Notes/__Claude__", "t",
            "- [ ] first task\n",
        )
        .unwrap();

        let mut first_attempt = true;
        do_set_task_state_with_retry_hook(&db, &accounts, &scope, "a@x.com", &uuid, 0, true, Some("first task"), &mut || {
            if first_attempt {
                first_attempt = false;
                // A racer renames the task's text before our write lands —
                // our stale expect_text must be re-validated against THIS
                // body on retry, not silently accepted.
                let existing = db.note_by_uuid("a@x.com", &uuid).unwrap().unwrap();
                db.apply_local_edit(&uuid, "a@x.com", &existing.title,
                    "<div><input type=\"checkbox\" contenteditable=\"false\">&nbsp;renamed task</div>",
                    &existing.label).unwrap();
            }
        })
        .expect_err("expect_text must be re-checked against the racer's renamed text, not the stale snapshot");
    }

    #[test]
    fn create_note_writes_dirty_row_with_derived_tags() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);

        let uuid = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "Repo notes", "#rust #jodd\n\n## Findings\n\n**bold** text").unwrap();

        let n = db.get(&uuid, "a@x.com").unwrap().unwrap();
        assert_eq!(n.sync_state, jodd_lib::db::SyncState::Dirty);
        assert_eq!(n.label, "Notes/__Claude__");
        assert_eq!(n.title, "Repo notes");
        assert!(n.body_html.contains("<h2>Findings</h2>"));
        assert!(n.id.is_empty(), "no remote id until the worker pushes");
        // Tags derive from body hashtags — the spec's no-tag-tools doctrine.
        let tags = db.list_all_tags("a@x.com").unwrap();
        assert!(tags.iter().any(|(t, _)| t == "rust"), "hashtag must land in note_tags: {tags:?}");
        // Target folder was ensured.
        assert!(db.get_folder("a@x.com", "Notes/__Claude__").unwrap().is_some());
    }

    #[test]
    fn create_note_refuses_folder_outside_allowlist() {
        let (_dir, db) = test_db();
        let err = do_create_note(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", "Notes/Private", "t", "b").unwrap_err();
        assert!(err.contains("Notes/Private") && err.contains("mcp_write_scope.json"),
            "error must name the folder and the config file: {err}");
        // The refused write left nothing behind — not just an absent guess UUID
        // (which would be trivially true of any UUID never created).
        assert!(db.list_notes("a@x.com").unwrap().is_empty(),
            "a refused write must not create any note in the account");
    }

    #[test]
    fn create_note_sanitizes_malformed_body() {
        let (_dir, db) = test_db();
        let uuid = do_create_note(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", "Notes/__Claude__", "t", "hello <div><b>unclosed").unwrap();
        let n = db.get(&uuid, "a@x.com").unwrap().unwrap();
        assert_eq!(n.body_html.matches("<div").count(), n.body_html.matches("</div").count());
    }

    /// The canonical Jodd task row. Both write paths must produce it verbatim —
    /// `NoteEditor.taskBlock()` only recognizes a checkbox that is a direct
    /// child of a top-level block.
    const TASK_ROW: &str = r#"<div><input type="checkbox" contenteditable="false">&nbsp;ship it</div>"#;

    #[test]
    fn create_note_turns_a_markdown_tasklist_into_a_tickable_row() {
        let (_dir, db) = test_db();
        let uuid = do_create_note(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", "Notes/__Claude__", "t", "- [ ] ship it\n").unwrap();
        let body = db.get(&uuid, "a@x.com").unwrap().unwrap().body_html;
        assert!(body.contains(TASK_ROW), "agent tasklist must land as a Jodd task row: {body}");
        assert!(!body.contains("disabled"), "a disabled box cannot be ticked: {body}");
    }

    fn seeded_note(db: &jodd_lib::db::Db, body_html: &str) -> String {
        // Seed through do_create_note where possible; for Apple-authored
        // fixtures (which the sanitizer would strip) insert directly.
        let uuid = do_create_note(db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", "Notes/__Claude__", "seed", "seed body").unwrap();
        // Overwrite the body verbatim, simulating content that arrived from sync.
        db.apply_local_edit(&uuid, "a@x.com", "seed", body_html, "Notes/__Claude__").unwrap();
        uuid
    }

    #[test]
    fn append_never_mutates_existing_bytes() {
        let (_dir, db) = test_db();
        let apple_body = "<ul><li><input type=\"checkbox\" checked=\"\">done</li></ul>";
        let uuid = seeded_note(&db, apple_body);
        do_update_note(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", &uuid, "new **finding**", UpdateMode::Append, None, false).unwrap();
        let n = db.get(&uuid, "a@x.com").unwrap().unwrap();
        assert!(n.body_html.starts_with(apple_body), "existing bytes must be untouched:\n{}", n.body_html);
        assert!(n.body_html.contains("<strong>finding</strong>"));
        assert_eq!(n.sync_state, jodd_lib::db::SyncState::Dirty);
    }

    #[test]
    fn replace_succeeds_on_safe_body_even_with_cosmetic_differences() {
        let (_dir, db) = test_db();
        // Safe subset, but not ammonia-serialized (uppercase tag).
        let uuid = seeded_note(&db, "<P>old</P>");
        do_update_note(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", &uuid, "rewritten", UpdateMode::Replace, None, false).unwrap();
        let n = db.get(&uuid, "a@x.com").unwrap().unwrap();
        assert!(n.body_html.contains("rewritten") && !n.body_html.contains("old"));
    }

    #[test]
    fn replace_refuses_apple_authored_content_without_force() {
        let (_dir, db) = test_db();
        let uuid = seeded_note(&db, "<ul><li><input type=\"checkbox\" checked=\"\">done</li></ul>");
        let err = do_update_note(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", &uuid, "rewritten", UpdateMode::Replace, None, false).unwrap_err();
        assert!(err.contains("append") && err.contains("force"), "error must teach both ways forward: {err}");
        // Body unchanged.
        assert!(db.get(&uuid, "a@x.com").unwrap().unwrap().body_html.contains("checked"));
        // force=true is the explicit override.
        do_update_note(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", &uuid, "rewritten", UpdateMode::Replace, None, true).unwrap();
        assert!(!db.get(&uuid, "a@x.com").unwrap().unwrap().body_html.contains("checked"));
    }

    #[test]
    fn update_note_turns_a_markdown_tasklist_into_a_tickable_row() {
        // Second call site: a fix applied to only one of the two would pass
        // create_note's test and still ship an inert checkbox here.
        let (_dir, db) = test_db();
        let uuid = seeded_note(&db, "<p>x</p>");
        do_update_note(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", &uuid, "- [ ] ship it\n", UpdateMode::Append, None, false).unwrap();
        let body = db.get(&uuid, "a@x.com").unwrap().unwrap().body_html;
        assert!(body.contains(TASK_ROW), "agent tasklist must land as a Jodd task row: {body}");
    }

    #[test]
    fn update_refuses_note_outside_allowlist() {
        let (_dir, db) = test_db();
        // Note lives in Notes/__Claude__; allowlist grants only Notes/Other.
        let uuid = seeded_note(&db, "<p>x</p>");
        let err = do_update_note(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/Other"]),
            "a@x.com", &uuid, "y", UpdateMode::Append, None, false).unwrap_err();
        assert!(err.contains("Notes/__Claude__"), "must name the note's actual folder: {err}");
    }

    #[test]
    fn create_folder_inside_allowlist() {
        let (_dir, db) = test_db();
        do_create_folder(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", "Notes/__Claude__/Research").unwrap();
        assert!(db.get_folder("a@x.com", "Notes/__Claude__/Research").unwrap().is_some());
    }

    #[test]
    fn create_folder_refuses_outside_allowlist_and_bad_paths() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        assert!(do_create_folder(&db, &accounts, &scope, "a@x.com", "Notes/Elsewhere").is_err());
        assert!(do_create_folder(&db, &accounts, &scope, "a@x.com", "Elsewhere").is_err());
        assert!(do_create_folder(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__/").is_err());
        assert!(do_create_folder(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__//x").is_err());
    }

    #[test]
    fn create_folder_duplicate_is_a_clear_error() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        do_create_folder(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__/R").unwrap();
        let err = do_create_folder(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__/R").unwrap_err();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn parse_tasks_reads_state_order_and_depth() {
        let body = concat!(
            r#"<div><input type="checkbox" contenteditable="false">&nbsp;first</div>"#,
            r#"<div style="margin-left: 28px"><input type="checkbox" checked="" contenteditable="false">&nbsp;nested done</div>"#,
        );
        let tasks = parse_tasks(body);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].index, 0);
        assert_eq!(tasks[0].text, "first");
        assert!(!tasks[0].checked);
        assert_eq!(tasks[0].level, 0);
        assert_eq!(tasks[1].index, 1);
        assert_eq!(tasks[1].text, "nested done");
        assert!(tasks[1].checked);
        assert_eq!(tasks[1].level, 1);
    }

    #[test]
    fn list_tasks_can_hide_done_items() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        // "done" first, "open" second: the survivor's absolute index is 1, not
        // 0. Reversed deliberately from the brief's original "open, done"
        // order — that order left the survivor at index 0 both before AND
        // after filtering, so a renumbering bug would produce the identical
        // assertion and pass. This order fails immediately under renumbering.
        let uuid = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "t", "- [x] done\n- [ ] open\n").unwrap();

        let all = do_list_tasks(&db, &accounts, "a@x.com", None, true).unwrap();
        let note = all.iter().find(|n| n.uuid == uuid).unwrap();
        assert_eq!(note.tasks.len(), 2);

        let open_only = do_list_tasks(&db, &accounts, "a@x.com", None, false).unwrap();
        let note = open_only.iter().find(|n| n.uuid == uuid).unwrap();
        assert_eq!(note.tasks.len(), 1);
        assert_eq!(note.tasks[0].text, "open");
        assert_eq!(note.tasks[0].index, 1,
            "index stays absolute (its position among ALL tasks, done included), not re-numbered after filtering");
    }

    #[test]
    fn parse_tasks_preserves_inline_markup_in_text() {
        // The exact fixture gap that let a Critical data-loss bug through in
        // Task 8b next door: every fixture there was plain text, so a naive
        // first-text-node implementation of row text extraction would have
        // passed unnoticed. row_text recurses into every Element child
        // collecting Text nodes, so bold text and a link's anchor text must
        // both survive into the flattened `text` field.
        let body = concat!(
            r#"<div><input type="checkbox" contenteditable="false">"#,
            r#"&nbsp;Ship <strong>v2</strong> per the <a href="https://example.com/spec">spec</a></div>"#,
        );
        let tasks = parse_tasks(body);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "Ship v2 per the spec", "got: {:?}", tasks[0].text);
    }

    #[test]
    fn set_checkbox_state_is_byte_surgical() {
        // Deliberately non-canonical markup around the target: it must come
        // back byte-identical. A DOM round-trip would normalize it.
        let body = concat!(
            "<P class='keep-me'>Odd   markup &amp; entities</P>",
            r#"<div><input type="checkbox" contenteditable="false">&nbsp;one</div>"#,
            r#"<div><input type="checkbox" contenteditable="false">&nbsp;two</div>"#,
            "<object type=\"application/x-apple-msg-attachment\" data=\"cid:x\"></object>",
        );
        let out = set_checkbox_state(body, 1, true).unwrap();
        assert!(out.starts_with("<P class='keep-me'>Odd   markup &amp; entities</P>"),
            "untouched bytes must survive verbatim: {out}");
        assert!(out.contains("cid:x"), "attachment ref must survive: {out}");
        let tasks = parse_tasks(&out);
        assert!(!tasks[0].checked, "task 0 untouched");
        assert!(tasks[1].checked, "task 1 ticked");
        // Unticking returns to the original bytes exactly.
        assert_eq!(set_checkbox_state(&out, 1, false).unwrap(), body);
    }

    #[test]
    fn set_checkbox_state_is_idempotent_and_range_checked() {
        let body = r#"<div><input type="checkbox" checked="" contenteditable="false">&nbsp;a</div>"#;
        assert_eq!(set_checkbox_state(body, 0, true).unwrap(), body, "already checked = no-op");
        let err = set_checkbox_state(body, 5, true).unwrap_err();
        assert!(err.contains("this note has 1 task(s), valid indices are 0-0"),
            "message should state an unambiguous inclusive range: {err}");
    }

    #[test]
    fn expect_text_mismatch_refuses_and_names_the_actual_task() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let uuid = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "t", "- [ ] ship it\n").unwrap();
        let err = do_set_task_state(&db, &accounts, &scope, "a@x.com", &uuid, 0, true,
            "something else").unwrap_err();
        assert!(err.contains("ship it"), "must name what is actually at that index: {err}");
        assert!(err.contains("list_tasks"), "must teach the recovery call: {err}");
        assert!(!parse_tasks(&db.get(&uuid, "a@x.com").unwrap().unwrap().body_html)[0].checked,
            "refused call must not have written");
    }

    #[test]
    fn set_task_state_requires_expect_text_to_match_before_writing() {
        // Renamed/strengthened version of the near-miss scenario: calling
        // with the WRONG account's task text against a note that happens
        // to have a validly-indexed task must refuse, not silently tick
        // the wrong box.
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let uuid = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "t", "- [ ] buy milk\n- [ ] walk dog\n").unwrap();
        // "buy cheese" is a plausible task text from an unrelated note/account
        // that happens to share index 0 — exactly the live near-miss shape.
        let err = do_set_task_state(&db, &accounts, &scope, "a@x.com", &uuid, 0, true,
            "buy cheese").unwrap_err();
        assert!(err.contains("buy milk"), "must name what's actually at index 0: {err}");
        let tasks = parse_tasks(&db.get(&uuid, "a@x.com").unwrap().unwrap().body_html);
        assert!(!tasks[0].checked, "refused call must not have written");
    }

    #[test]
    fn set_task_state_refuses_an_empty_or_whitespace_expect_text() {
        // `expect_text: String` (not `Option`) stops a caller from omitting
        // the field, but a lazy/adversarial caller could still satisfy the
        // schema with "" without ever having consulted list_tasks — a vacuous
        // guess proves nothing, so it must be refused just like a wrong one.
        //
        // A non-empty task text ("buy milk") would NOT discriminate this
        // fix from no fix at all: even without the blank-check, `"" !=
        // Some("buy milk")` already falls into the ordinary mismatch branch
        // and produces an error mentioning "list_tasks" too. The only
        // scenario that actually proves the guard is a note whose task text
        // is ITSELF blank — under the old code `"" == Some("")` would
        // vacuously MATCH and silently write. Construct that via
        // `seeded_note`, which writes raw HTML directly, bypassing the
        // markdown-tasklist conversion that would never itself produce an
        // empty row.
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let body = r#"<div><input type="checkbox" contenteditable="false"></div>"#;
        let uuid = seeded_note(&db, body);
        assert_eq!(parse_tasks(body)[0].text, "", "fixture must have a genuinely empty task text");

        for blank in ["", "   ", "\t\n"] {
            let err = do_set_task_state(&db, &accounts, &scope, "a@x.com", &uuid, 0, true,
                blank).unwrap_err();
            assert!(err.contains("list_tasks"), "must teach the recovery call: {err}");
        }

        let tasks = parse_tasks(&db.get(&uuid, "a@x.com").unwrap().unwrap().body_html);
        assert!(!tasks[0].checked, "a blank guess must never write, even against a blank task");
    }

    #[test]
    fn set_task_state_respects_the_allowlist() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let uuid = do_create_note(&db, &accounts, &scope_with("a@x.com", &["Notes/__Claude__"]),
            "a@x.com", "Notes/__Claude__", "t", "- [ ] x\n").unwrap();
        let err = do_set_task_state(&db, &accounts, &scope_with("a@x.com", &["Notes/Other"]),
            "a@x.com", &uuid, 0, true, "x").unwrap_err();
        assert!(err.contains("Notes/__Claude__"), "must name the note's folder: {err}");
    }

    #[test]
    fn set_task_state_persists_the_correct_box_and_leaves_the_rest_untouched() {
        // End-to-end through the DB, not just the pure `set_checkbox_state`
        // core: this is the only test that actually re-reads a persisted row
        // after `do_set_task_state`, so it is the one that would catch
        // `db.apply_local_edit(uuid, account_id, &existing.title, &new_body,
        // &existing.label)` (write.rs) having `title` and `new_body`
        // transposed — that transposition compiles cleanly (both are &str)
        // and would silently write the whole HTML body into the title column
        // on every tick, but every other test here only inspects the return
        // value of the pure function or asserts on a refusal.
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let uuid = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "My tasks", "- [ ] first\n- [ ] second\n").unwrap();

        do_set_task_state(&db, &accounts, &scope, "a@x.com", &uuid, 1, true, "second").unwrap();

        let n = db.get(&uuid, "a@x.com").unwrap().unwrap();
        let tasks = parse_tasks(&n.body_html);
        assert_eq!(tasks.len(), 2, "body must still parse as two tasks: {}", n.body_html);
        assert_eq!(tasks[0].text, "first");
        assert!(!tasks[0].checked, "task 0 must remain untouched");
        assert_eq!(tasks[1].text, "second");
        assert!(tasks[1].checked, "task 1 must be the one ticked");
        assert_eq!(n.title, "My tasks", "title must survive the write, not be overwritten by the body");
        assert_eq!(n.label, "Notes/__Claude__", "label must survive the write");
        assert_eq!(n.sync_state, jodd_lib::db::SyncState::Dirty);
    }

    // ── C1: folder-label path traversal ──────────────────────────────────
    //
    // The allowlist is the entire security control for this feature, and it
    // is a string-prefix test. `Notes/__Claude__/../../../pwned` starts with
    // `Notes/__Claude__/`, so `folder_allowed` says yes; on a LocalFs account
    // the label IS a filesystem path, `Path::join`ed onto the vault root with
    // no normalization, so the write landed outside the vault. Both write
    // tools now run `validate_label_path` BEFORE the allowlist.

    /// Every traversal shape, through `do_create_note`. Asserting only "it
    /// returned Err" would pass against an implementation that rejects after
    /// having already created the folder row (the order the pre-fix code
    /// used: allowlist, then `create_folder_local_new`, then the note), so
    /// this asserts the *whole account* is still empty of both notes and
    /// folders after each attempt.
    #[test]
    fn create_note_refuses_traversal_and_leaves_no_row_behind() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);

        for bad in [
            // Mid-path: the reviewer's reproduction. Prefix-allowed, escapes.
            "Notes/__Claude__/../../../pwned",
            // A bare `..` segment right under the allowed root.
            "Notes/__Claude__/..",
            // `.` is the same class and normalizes to a different folder.
            "Notes/__Claude__/./x",
            // Windows separator: contains no '/', so a POSIX-shaped check
            // (split on '/', reject "..") sees one innocuous segment.
            r"Notes/__Claude__/..\..\pwned",
            // Windows drive prefix: Path::join REPLACES the path, no `..`
            // involved at all.
            "Notes/__Claude__/D:pwned",
            // Trailing space — Win32 strips it, so this reaches the FS as `..`.
            "Notes/__Claude__/.. /x",
        ] {
            let err = do_create_note(&db, &accounts, &scope, "a@x.com", bad, "t", "b")
                .unwrap_err();
            assert!(
                !err.contains("not writable"),
                "{bad:?} must be refused as a malformed path, not merely fall out of the \
                 allowlist — it is INSIDE the allowlist by prefix: {err}"
            );
            assert!(
                db.list_notes("a@x.com").unwrap().is_empty(),
                "{bad:?} left a note row behind"
            );
            assert!(
                db.list_folders("a@x.com").unwrap().is_empty(),
                "{bad:?} left a folder row behind: {:?}",
                db.list_folders("a@x.com").unwrap().iter().map(|f| f.path.clone()).collect::<Vec<_>>()
            );
        }

        // Control: the same call with a clean path under the same allowlist
        // succeeds, so the loop above is not passing because everything fails.
        do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__", "t", "b").unwrap();
    }

    /// The other writer. Before this wave the two disagreed — `do_create_folder`
    /// validated empty segments and `do_create_note` validated nothing — which
    /// is how the gap arose, so both are pinned to the same behavior here.
    #[test]
    fn create_folder_refuses_traversal_and_leaves_no_row_behind() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);

        for bad in [
            "Notes/__Claude__/../../../pwned",
            "Notes/__Claude__/..",
            "Notes/__Claude__/./x",
            r"Notes/__Claude__/..\..\pwned",
            "Notes/__Claude__/D:pwned",
            "Notes/__Claude__/.. /x",
        ] {
            let err = do_create_folder(&db, &accounts, &scope, "a@x.com", bad).unwrap_err();
            assert!(!err.contains("not inside the allowed folders"),
                "{bad:?} is inside the allowlist by prefix; it must be refused as malformed: {err}");
            // `create_folder_local_new` calls `ensure_ancestors`, so a late
            // rejection would still have materialised the ancestor chain.
            assert!(db.list_folders("a@x.com").unwrap().is_empty(),
                "{bad:?} left folder rows behind: {:?}",
                db.list_folders("a@x.com").unwrap().iter().map(|f| f.path.clone()).collect::<Vec<_>>());
        }

        do_create_folder(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__/ok").unwrap();
    }

    /// The deliberate non-rejection. A blanket `!path.contains("..")` would
    /// close C1 and also refuse a folder a user legitimately owns — traversal
    /// needs a segment to BE a dot-run, not to contain dots. Both of these
    /// must keep working, and `__name__` leaves must stay creatable because
    /// `Notes/__Claude__` is the feature's own default sandbox (gotcha #4's
    /// convention is app-layer policy, not part of the safety core).
    #[test]
    fn legitimate_dotted_and_reserved_names_still_work() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        for good in ["Notes/__Claude__/my..notes", "Notes/__Claude__/v1.2", "Notes/__Claude__/..hidden"] {
            do_create_folder(&db, &accounts, &scope, "a@x.com", good)
                .unwrap_or_else(|e| panic!("{good:?} is a legitimate folder name: {e}"));
            assert!(db.get_folder("a@x.com", good).unwrap().is_some());
        }
        // And a __name__ leaf, which the app-side validator reserves.
        do_create_folder(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__/__Extracts__").unwrap();
    }

    /// M6, on the two string fields the tools accept from an agent.
    #[test]
    fn oversized_folder_segments_and_titles_are_refused() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let huge = "x".repeat(jodd_lib::folder_label::MAX_SEGMENT_BYTES + 1);

        let err = do_create_folder(&db, &accounts, &scope, "a@x.com",
            &format!("Notes/__Claude__/{huge}")).unwrap_err();
        assert!(err.contains("too long"), "{err}");

        let err = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            &"t".repeat(jodd_lib::folder_label::MAX_TITLE_BYTES + 1), "b").unwrap_err();
        assert!(err.contains("too long"), "{err}");
        assert!(db.list_notes("a@x.com").unwrap().is_empty(), "the refused note must not exist");

        // A title with a newline would be header injection into `Subject:`.
        assert!(do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "line\nSubject: forged", "b").is_err());
    }

    // ── I2: deleted_pending notes are not writable ───────────────────────

    /// `db.get` returns rows in any sync state, so a write to a note the user
    /// already deleted landed in the row, left `sync_state = deleted_pending`
    /// (apply_local_edit's state machine has an `ELSE sync_state` arm), and
    /// was trashed by the worker on the next tick — while the tool answered
    /// `{"updated":true}`. `list_tasks` already treated the note as gone, so
    /// the read and write tools disagreed about whether it existed.
    #[test]
    fn writes_to_a_deleted_note_are_refused_not_silently_discarded() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let uuid = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "doomed", "- [ ] ship it\n").unwrap();
        let before = db.get(&uuid, "a@x.com").unwrap().unwrap().body_html;
        db.mark_deleted(&uuid, "a@x.com").unwrap();

        for err in [
            do_update_note(&db, &accounts, &scope, "a@x.com", &uuid, "more",
                UpdateMode::Append, None, false).unwrap_err(),
            do_set_task_state(&db, &accounts, &scope, "a@x.com", &uuid, 0, true, "ship it").unwrap_err(),
        ] {
            // Same shape as a uuid that never existed: a deleted note IS gone.
            assert!(err.contains(&format!("No note '{uuid}'")) && err.contains("search_notes"),
                "must match the no-such-note error shape: {err}");
        }

        // The row is untouched: still deleted_pending, still the old body. An
        // implementation that checked the state only AFTER apply_local_edit
        // would return the right error and still have written.
        let row = db.get(&uuid, "a@x.com").unwrap().unwrap();
        assert_eq!(row.sync_state, jodd_lib::db::SyncState::DeletedPending);
        assert_eq!(row.body_html, before, "a refused write must not have touched the body");

        // And list_tasks agrees the note is gone — the disagreement is closed
        // from both sides.
        let listed = do_list_tasks(&db, &accounts, "a@x.com", None, true).unwrap();
        assert!(listed.iter().all(|n| n.uuid != uuid), "deleted note must not be listed");
    }

    // ── I4: the account half of the security boundary ────────────────────

    /// `resolve_active_account` gates on `is_active()`, so Draining is
    /// refused too — stricter than `vertical_for`, and deliberate: gotcha #2's
    /// "Stop waiting" means Draining can hold unsent edits, and an agent
    /// adding more to a quiescing account is exactly what we don't want.
    /// Until now every fixture was Active by serde default.
    #[test]
    fn draining_and_inactive_accounts_are_refused_by_every_write_tool() {
        let (_dir, db) = test_db();
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        // Seed a real note while the account is Active, so the update/task
        // tools get past the lookup and fail on the account gate specifically.
        let uuid = do_create_note(&db, &[acct("a@x.com")], &scope, "a@x.com",
            "Notes/__Claude__", "t", "- [ ] x\n").unwrap();

        for status in ["draining", "inactive"] {
            let accounts = vec![acct_with_status("a@x.com", status)];
            for err in [
                do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__", "t", "b").unwrap_err(),
                do_create_folder(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__/New").unwrap_err(),
                do_update_note(&db, &accounts, &scope, "a@x.com", &uuid, "more",
                    UpdateMode::Append, None, false).unwrap_err(),
                do_set_task_state(&db, &accounts, &scope, "a@x.com", &uuid, 0, true, "x").unwrap_err(),
                do_list_tasks(&db, &accounts, "a@x.com", None, true).unwrap_err(),
            ] {
                assert!(err.contains("inactive account") && err.contains("list_accounts"),
                    "status={status}: {err}");
            }
            // Nothing was written, and the account is not offered either.
            assert!(db.get_folder("a@x.com", "Notes/__Claude__/New").unwrap().is_none());
            assert!(do_list_accounts(&accounts, &scope).is_empty(),
                "status={status} must not be advertised by list_accounts");
        }

        // Control: the same calls succeed for the Active account, so the
        // assertions above are about `status` and nothing else.
        do_create_folder(&db, &[acct("a@x.com")], &scope, "a@x.com", "Notes/__Claude__/New").unwrap();
    }

    // ── M3 / I3 ──────────────────────────────────────────────────────────

    /// A note whose every task is done is noise in the agent's context when
    /// it asked for what is outstanding.
    #[test]
    fn list_tasks_drops_notes_with_nothing_left_to_show() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let all_done = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "finished", "- [x] a\n- [x] b\n").unwrap();
        let has_open = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "live", "- [x] a\n- [ ] b\n").unwrap();

        let open = do_list_tasks(&db, &accounts, "a@x.com", None, false).unwrap();
        assert_eq!(open.len(), 1, "only the note with an open task survives: {open:?}");
        assert_eq!(open[0].uuid, has_open);
        assert_eq!(open[0].tasks[0].index, 1, "surviving index is still absolute");

        // include_done=true still shows both — the filter is about emptiness,
        // not about hiding completed work.
        let every = do_list_tasks(&db, &accounts, "a@x.com", None, true).unwrap();
        assert_eq!(every.len(), 2);
        assert!(every.iter().any(|n| n.uuid == all_done));
    }

    /// I3's read side: `level` is derived from the write side's own constants
    /// now, including the ceiling. A body indented past what the editor can
    /// produce reports the deepest real level, not an invented one.
    #[test]
    fn task_level_shares_the_write_sides_scale_and_ceiling() {
        use jodd_lib::llm::markdown::{TASK_INDENT_MAX_PX, TASK_INDENT_PX};
        let body = format!(
            concat!(
                r#"<div style="margin-left: {}px"><input type="checkbox">&nbsp;three</div>"#,
                r#"<div style="margin-left: {}px"><input type="checkbox">&nbsp;absurd</div>"#,
            ),
            TASK_INDENT_PX * 3,
            TASK_INDENT_MAX_PX * 4,
        );
        let tasks = parse_tasks(&body);
        assert_eq!(tasks[0].level, 3, "must use the same 28px scale the writer stamps");
        assert_eq!(tasks[1].level, TASK_INDENT_MAX_PX / TASK_INDENT_PX,
            "deeper than the editor can round-trip clamps to the deepest real level");
    }

    #[test]
    fn parse_tasks_and_set_checkbox_state_agree_on_nested_checkboxes() {
        // Regression for a divergence between the two orderings a checkbox
        // "index" can mean: `parse_tasks`'s DOM walk vs. `set_checkbox_state`'s
        // forward byte scan. Byte order is always source order. An earlier
        // version of `collect_tasks` recorded every DIRECT-child checkbox of a
        // node before recursing into any child's subtree — level order, not
        // document order — so a checkbox nested one level deeper than a later
        // sibling checkbox was reported AFTER it, even though it appears
        // EARLIER in the raw HTML.
        //
        // This fixture — an inner checkbox nested inside the first <div>,
        // followed by a second checkbox that is a direct child of the outer
        // <div> — reproduces exactly that shape. Under the old level-order
        // walk, `tasks[0]` would be the OUTER row and `tasks[1]` the inner
        // one (backwards from source order); under the fixed walk `tasks[0]`
        // is the byte-earlier (inner) row. `row_text` legitimately pulls in
        // a nested row's text too (it walks every descendant text node,
        // independent of `collect_tasks`'s emission order), so the second
        // row's *content* is unavoidably "contaminated" with the first row's
        // text either way — that's a `row_text` property, not what this test
        // is checking. What's checked is ORDER (`tasks[0].text` must be the
        // unambiguous, non-contaminated "inner" — the old bug would instead
        // put the contaminated text at index 0) and that the mutation lands
        // on the byte-later checkbox, not the nested one a divergent
        // ordering would target.
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let body = concat!(
            "<div>",
            r#"<div><input type="checkbox" contenteditable="false">&nbsp;inner</div>"#,
            r#"<input type="checkbox" contenteditable="false">&nbsp;outer"#,
            "</div>",
        );
        let uuid = seeded_note(&db, body);

        let before = db.get(&uuid, "a@x.com").unwrap().unwrap();
        let tasks = parse_tasks(&before.body_html);
        assert_eq!(tasks.len(), 2);
        assert!(!tasks[0].checked && !tasks[1].checked);
        assert_eq!(tasks[0].text, "inner",
            "byte-earlier (nested) checkbox must be index 0, unambiguously: {tasks:?}");

        // A real caller learns the guard text from list_tasks (== parse_tasks
        // here); use exactly what it reports rather than a hand-guessed
        // string, so this test doesn't depend on row_text's contamination
        // behavior — only on set_checkbox_state landing on the same tag
        // parse_tasks[1] describes.
        let expect = tasks[1].text.clone();
        do_set_task_state(&db, &accounts, &scope, "a@x.com", &uuid, 1, true, &expect).unwrap();

        let after = parse_tasks(&db.get(&uuid, "a@x.com").unwrap().unwrap().body_html);
        assert!(!after[0].checked, "inner (index 0) must remain unchecked");
        assert!(after[1].checked,
            "the byte-later (outer) checkbox must be the one ticked, not the nested one a divergent ordering would have targeted");
    }

    #[test]
    fn html_to_preview_strips_tags_and_truncates() {
        let html = "<div>Hello <b>world</b></div><div>this is a much longer second line that goes on</div>";
        let preview = html_to_preview(html, 20);
        assert!(!preview.contains('<'), "tags must be stripped: {preview:?}");
        assert!(preview.chars().count() <= 20, "must respect max_chars: {preview:?}");
    }

    #[test]
    fn html_to_preview_never_splits_a_multibyte_char() {
        // Thai text — regression guard for the char-boundary requirement.
        let html = "<p>ทดสอบภาษาไทยมีสระวรรยุกต์ได้ถูกต้อง สวัสดีครับ</p>";
        let preview = html_to_preview(html, 10);
        // Must not panic (a byte-slice cut mid-codepoint panics or produces
        // invalid UTF-8), and must be valid UTF-8 by construction (String).
        assert!(preview.chars().count() <= 10);
    }

    #[test]
    fn html_to_preview_does_not_leak_a_secret_beyond_the_cap() {
        // Not a redaction feature — just proves the cap actually bounds
        // output size regardless of how much sensitive text a note holds.
        let long_secret_note = format!("<p>{}</p>", "sk-ant-a".repeat(10_000));
        let preview = html_to_preview(&long_secret_note, SEARCH_PREVIEW_MAX_CHARS);
        assert!(preview.chars().count() <= SEARCH_PREVIEW_MAX_CHARS);
    }

    #[test]
    fn search_notes_tool_path_truncates_but_db_search_notes_returns_full_body() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);

        let long_body = format!(
            "#findable\n\n{}",
            "this note has a very long body full of matched text ".repeat(50)
        );
        let uuid = do_create_note(
            &db, &accounts, &scope, "a@x.com", "Notes/__Claude__", "Long note", &long_body,
        ).unwrap();

        // Direct Db::search_notes call (existing behavior, untouched):
        // full, untruncated body_html.
        let direct = db.search_notes(Some("a@x.com"), None, "findable", &[]).unwrap();
        assert_eq!(direct.len(), 1);
        assert!(
            direct[0].body_html.chars().count() > SEARCH_PREVIEW_MAX_CHARS,
            "sanity: the seeded body must exceed the preview cap for this test to prove anything"
        );

        // Tool-path wrapper: same rows, but body_html is a bounded, tag-free
        // preview.
        let previewed = do_search_notes(&db, Some("a@x.com"), None, "findable", &[]).unwrap();
        assert_eq!(previewed.len(), 1);
        assert_eq!(previewed[0].uuid, uuid);
        assert!(
            previewed[0].body_html.chars().count() <= SEARCH_PREVIEW_MAX_CHARS,
            "tool path must bound body_html: {} chars",
            previewed[0].body_html.chars().count()
        );
        assert!(!previewed[0].body_html.contains('<'), "tool path must strip tags: {:?}", previewed[0].body_html);
    }

    #[test]
    fn html_to_preview_skips_script_and_style_text() {
        let html = "<div>visible<script>var secret = 'sk-ant-leak';</script>\
                    <style>.x{content:'also-hidden'}</style>tail</div>";
        let preview = html_to_preview(html, SEARCH_PREVIEW_MAX_CHARS);
        assert!(preview.contains("visible") && preview.contains("tail"), "{preview:?}");
        assert!(!preview.contains("sk-ant-leak"), "script text leaked into preview: {preview:?}");
        assert!(!preview.contains("also-hidden"), "style text leaked into preview: {preview:?}");
    }

    #[test]
    fn search_notes_tool_path_caps_the_title_too() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let long_title = "T".repeat(PREVIEW_TITLE_MAX_CHARS * 3);
        do_create_note(
            &db, &accounts, &scope, "a@x.com", "Notes/__Claude__", &long_title, "#findable body",
        ).unwrap();

        let direct = db.search_notes(Some("a@x.com"), None, "findable", &[]).unwrap();
        assert!(
            direct[0].title.chars().count() > PREVIEW_TITLE_MAX_CHARS,
            "sanity: stored title must exceed the cap for this test to prove anything"
        );
        let previewed = do_search_notes(&db, Some("a@x.com"), None, "findable", &[]).unwrap();
        assert!(
            previewed[0].title.chars().count() <= PREVIEW_TITLE_MAX_CHARS,
            "tool path must bound title: {} chars",
            previewed[0].title.chars().count()
        );
    }

    #[test]
    fn title_capping_truncates_but_does_not_html_strip() {
        // A title is plain text, not HTML. Running it through
        // html_to_preview would eat a real title: `ทดสอบ Title <h3>` (from
        // live testing) strips to `ทดสอบ Title`. Short enough to be
        // untouched by the cap, so any change here is the strip, not the
        // truncation.
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let title = "ทดสอบ Title <h3>";
        do_create_note(
            &db, &accounts, &scope, "a@x.com", "Notes/__Claude__", title, "#findable body",
        ).unwrap();
        let previewed = do_search_notes(&db, Some("a@x.com"), None, "findable", &[]).unwrap();
        assert_eq!(previewed[0].title, title, "title must survive the tool path verbatim");
    }

    /// Finding 3: a per-field cap is not a response bound. This measures the
    /// ACTUAL serialized response for a worst-case vault — far more matches
    /// than the row cap, every capped field at its maximum, a deep folder
    /// label — and pins it under the smallest live client failure observed
    /// (~52,000 characters). Without the row cap the same fixture serializes
    /// to roughly 3× that.
    #[test]
    fn search_notes_response_stays_under_the_observed_failure_threshold() {
        const OBSERVED_LIVE_FAILURE_CHARS: usize = 52_000;
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        // A deliberately deep label: `label` is the one response field with
        // no cap, so the fixture must stress it rather than assume "Notes/X".
        let deep_label = "Notes/__Claude__/Research/Backends/Gmail/Wire/Decoding/Edge-cases";
        let scope = scope_with("a@x.com", &[deep_label]);

        // 200 = Db::search_notes' own LIMIT, i.e. the most rows the tool path
        // can ever be handed.
        // Titles are capped at 500 BYTES on the write path, so the fixture
        // stays under that while still exceeding PREVIEW_TITLE_MAX_CHARS.
        let long_title = "หัวข้อยาว Long Title ".repeat(8);
        let long_body = format!("#findable {}", "a long body full of matched text ".repeat(200));
        for i in 0..200 {
            do_create_note(
                &db, &accounts, &scope, "a@x.com", deep_label,
                &format!("{long_title}{i}"), &long_body,
            ).unwrap();
        }

        let rows = do_search_notes(&db, Some("a@x.com"), None, "findable", &[]).unwrap();
        assert_eq!(rows.len(), SEARCH_MAX_ROWS, "tool path must cap the row count");

        let json = serde_json::to_string(&rows).unwrap();
        assert!(
            json.chars().count() < OBSERVED_LIVE_FAILURE_CHARS,
            "worst-case search_notes response is {} chars — must stay under the {} \
             observed live failure threshold",
            json.chars().count(),
            OBSERVED_LIVE_FAILURE_CHARS
        );
        // Tighter than the failure threshold: the documented bound on
        // SEARCH_MAX_ROWS is ~735 chars/row. If a future field pushes past
        // this, the doc comment's arithmetic is stale and must be redone.
        assert!(
            json.chars().count() <= SEARCH_MAX_ROWS * 900,
            "per-row size {} exceeds the documented ~735 chars/row budget ({} bytes total)",
            json.chars().count() / SEARCH_MAX_ROWS,
            json.len()
        );
    }

    #[test]
    fn note_connections_tool_path_truncates_but_db_returns_full_body() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);

        let long_body = "this target note has a very long body indeed ".repeat(50);
        let target = do_create_note(
            &db, &accounts, &scope, "a@x.com", "Notes/__Claude__", "Target", &long_body,
        ).unwrap();
        let source = do_create_note(
            &db, &accounts, &scope, "a@x.com", "Notes/__Claude__", "Source",
            "see [[Target]] for detail",
        ).unwrap();

        // Db level (what the app's own graph view gets): full body.
        let direct = db.outgoing_links("a@x.com", &source).unwrap();
        assert_eq!(direct.len(), 1, "fixture must produce exactly one outgoing link");
        assert_eq!(direct[0].uuid, target);
        assert!(
            direct[0].body_html.chars().count() > SEARCH_PREVIEW_MAX_CHARS,
            "sanity: the target body must exceed the preview cap"
        );

        // Tool path: same row, bounded body — outgoing AND backlinks, since
        // returning a full body on either one is the whole finding.
        let out = do_note_connections(&db, "a@x.com", &source).unwrap();
        assert_eq!(out.outgoing.len(), 1);
        assert_eq!(out.outgoing[0].uuid, target);
        assert!(
            out.outgoing[0].body_html.chars().count() <= SEARCH_PREVIEW_MAX_CHARS,
            "outgoing body_html unbounded: {} chars",
            out.outgoing[0].body_html.chars().count()
        );
        assert!(!out.outgoing[0].body_html.contains('<'), "outgoing must be tag-stripped");

        let back = do_note_connections(&db, "a@x.com", &target).unwrap();
        assert_eq!(back.backlinks.len(), 1);
        assert_eq!(back.backlinks[0].uuid, source);
        assert!(
            back.backlinks[0].body_html.chars().count() <= SEARCH_PREVIEW_MAX_CHARS,
            "backlinks body_html unbounded"
        );
        assert!(!back.backlinks[0].body_html.contains('<'), "backlinks must be tag-stripped");
    }

    /// The tool wrapper, not just the helper, must be what truncates: this
    /// asserts on the JSON the MCP layer actually serializes (the same
    /// `NoteConnectionsResult` `main.rs` hands to `serde_json`), so wiring
    /// `do_note_connections` up but serializing the raw `CachedNote`s
    /// elsewhere would fail here.
    #[test]
    fn note_connections_serialized_response_carries_no_full_body() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let secret = "sk-ant-donotleak";
        let long_body = format!("{} {}", "filler text ".repeat(60), secret);
        do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__", "Target", &long_body).unwrap();
        let source = do_create_note(
            &db, &accounts, &scope, "a@x.com", "Notes/__Claude__", "Source", "see [[Target]]",
        ).unwrap();

        let json = serde_json::to_string(&do_note_connections(&db, "a@x.com", &source).unwrap()).unwrap();
        assert!(
            !json.contains(secret),
            "content past the preview cap reached the serialized response: {json}"
        );
    }

    /// Re-review finding: capping `outgoing` and `backlinks` independently
    /// at SEARCH_MAX_ROWS still lets a hub note's *combined* response exceed
    /// the same failure threshold `search_notes_response_stays_under_the_
    /// observed_failure_threshold` measures. This saturates both lists past
    /// the cap and checks the combined row count and serialized size.
    #[test]
    fn note_connections_response_stays_under_the_observed_failure_threshold() {
        const OBSERVED_LIVE_FAILURE_CHARS: usize = 52_000;
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let deep_label = "Notes/__Claude__/Research/Backends/Gmail/Wire/Decoding/Edge-cases";
        let scope = scope_with("a@x.com", &[deep_label]);

        let long_title = "หัวข้อยาว Long Title ".repeat(8);
        let long_body = |extra: &str| {
            format!("{} {}", "a long body full of matched text ".repeat(200), extra)
        };

        // The hub note: linked FROM 60 notes (its backlinks) and linking TO
        // 60 notes (its outgoing), so both lists exceed SEARCH_MAX_ROWS on
        // their own and the combined budget is what actually gets tested.
        let hub = do_create_note(
            &db, &accounts, &scope, "a@x.com", deep_label, "Hub", "the hub note",
        ).unwrap();

        for i in 0..60 {
            // Target of the hub's outgoing links.
            do_create_note(
                &db, &accounts, &scope, "a@x.com", deep_label,
                &format!("{long_title}out{i}"), &long_body("target"),
            ).unwrap();
            // Source of the hub's backlinks.
            do_create_note(
                &db, &accounts, &scope, "a@x.com", deep_label,
                &format!("{long_title}in{i}"), &long_body(&format!("[[Hub]] {i}")),
            ).unwrap();
        }
        let outgoing_links: String = (0..60)
            .map(|i| format!("[[{long_title}out{i}]] "))
            .collect();
        do_update_note(
            &db, &accounts, &scope, "a@x.com", &hub,
            &outgoing_links, UpdateMode::Append, None, false,
        ).unwrap();

        let out = do_note_connections(&db, "a@x.com", &hub).unwrap();
        assert!(
            out.outgoing.len() + out.backlinks.len() <= SEARCH_MAX_ROWS,
            "combined note_connections response must stay within SEARCH_MAX_ROWS: \
             {} outgoing + {} backlinks",
            out.outgoing.len(),
            out.backlinks.len()
        );

        let json = serde_json::to_string(&out).unwrap();
        assert!(
            json.chars().count() < OBSERVED_LIVE_FAILURE_CHARS,
            "worst-case note_connections response is {} chars — must stay under the {} \
             observed live failure threshold",
            json.chars().count(),
            OBSERVED_LIVE_FAILURE_CHARS
        );
    }

    #[test]
    fn set_pin_pins_and_unpins_a_note() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let uuid = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "t", "body").unwrap();

        // create_note's own insert leaves pinned=false, pin_dirty=false —
        // confirm the starting point before asserting the flip actually did
        // something, rather than trusting a false already sitting there.
        let before = db.note_by_uuid("a@x.com", &uuid).unwrap().unwrap();
        assert!(!before.pinned && !before.pin_dirty);

        do_set_pin(&db, &accounts, &scope, "a@x.com", &uuid, true).unwrap();
        let pinned = db.note_by_uuid("a@x.com", &uuid).unwrap().unwrap();
        assert!(pinned.pinned, "pinned must be set");
        assert!(pinned.pin_dirty, "pin_dirty must be set so the worker pushes the sidecar");

        do_set_pin(&db, &accounts, &scope, "a@x.com", &uuid, false).unwrap();
        let unpinned = db.note_by_uuid("a@x.com", &uuid).unwrap().unwrap();
        assert!(!unpinned.pinned, "unpin must clear pinned");
        assert!(unpinned.pin_dirty, "pin_dirty stays set — Db::set_pin's own idempotent-retry doc comment");
    }

    /// The behavioural half of the Sidecars gate — proves `do_set_pin`
    /// actually reaches SQLite through `resolve_writable_account`, not just
    /// that the gate function returns the right answer in isolation.
    ///
    /// Before M4 (2026-08-16) this test proved the OPPOSITE: Microsoft was
    /// the one backend that refused `Write::Sidecars`, so this asserted
    /// `do_set_pin` was refused there and wrote nothing. M4 turned Sidecars
    /// on for Microsoft — a named MAPI property on the note itself, not a
    /// second sidecar message — and there is now no backend anywhere in
    /// `Capabilities::for_backend` that allows Notes but refuses Sidecars,
    /// so a refusal scenario can no longer be constructed against real
    /// capabilities. This now proves the positive case instead: `do_set_pin`
    /// actually writes on Microsoft, the backend this exact gate used to
    /// block.
    #[test]
    fn set_pin_succeeds_on_microsoft_now_that_m4_turned_sidecars_on() {
        let (_dir, db) = test_db();
        let ms_accounts = vec![acct_with_backend("m@live.com", "microsoft")];
        let scope = scope_with("m@live.com", &["Notes/__Claude__"]);
        // Folder must pre-exist (same seed create_note_into_an_existing_
        // folder_works_on_a_notes_on_folders_off_backend uses) — Microsoft's
        // Folders write is still off, so do_create_note into a not-yet-known
        // folder would itself be refused before ever reaching the note this
        // test needs to seed.
        db.upsert_folder_from_remote("m@live.com", "Notes/__Claude__", "AAMkFolderId")
            .unwrap();
        let uuid = do_create_note(
            &db, &ms_accounts, &scope, "m@live.com", "Notes/__Claude__", "t", "body",
        ).unwrap();

        do_set_pin(&db, &ms_accounts, &scope, "m@live.com", &uuid, true).unwrap();

        let note = db.note_by_uuid("m@live.com", &uuid).unwrap().unwrap();
        assert!(note.pinned, "M4: pin must actually be written on Microsoft now");
        assert!(note.pin_dirty, "pin_dirty must be set so the worker pushes the sidecar");
    }

    #[test]
    fn set_pin_refuses_note_outside_allowlist() {
        let (_dir, db) = test_db();
        // Note lives in Notes/__Claude__; allowlist grants only Notes/Other —
        // same shape as update_refuses_note_outside_allowlist.
        let uuid = seeded_note(&db, "<p>x</p>");
        let err = do_set_pin(&db, &[acct("a@x.com")], &scope_with("a@x.com", &["Notes/Other"]),
            "a@x.com", &uuid, true).unwrap_err();
        assert!(err.contains("Notes/__Claude__"), "must name the note's actual folder: {err}");
        assert!(!db.note_by_uuid("a@x.com", &uuid).unwrap().unwrap().pinned,
            "a refused pin call must not have written");
    }

    #[test]
    fn set_pin_refuses_unknown_uuid() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let err = do_set_pin(&db, &accounts, &scope, "a@x.com", "no-such-uuid", true).unwrap_err();
        // Same convention write_with_retry's lookup uses (do_update_note,
        // do_set_task_state): name the uuid, point at search_notes — not a
        // silent no-op the way the SQLite UPDATE underneath would allow.
        assert!(err.contains("no-such-uuid") && err.contains("search_notes"),
            "must teach the recovery call: {err}");
    }

    #[test]
    fn set_pin_refuses_a_deleted_pending_note() {
        let (_dir, db) = test_db();
        let accounts = vec![acct("a@x.com")];
        let scope = scope_with("a@x.com", &["Notes/__Claude__"]);
        let uuid = do_create_note(&db, &accounts, &scope, "a@x.com", "Notes/__Claude__",
            "t", "body").unwrap();
        // mark_deleted is how a local delete reaches sync_state =
        // 'deleted_pending' (see mark_deleted's own tests in db.rs); the note
        // still exists as a row (pending push of the delete) but must not be
        // silently actionable through this tool — same reasoning as
        // write_with_retry's choice of note_by_uuid over Db::get.
        db.mark_deleted(&uuid, "a@x.com").unwrap();
        let err = do_set_pin(&db, &accounts, &scope, "a@x.com", &uuid, true).unwrap_err();
        assert!(err.contains(&uuid), "must name the uuid: {err}");
    }
}
