//! Settings → AI › Notes on disk / Skills for Claude Code (spec 2026-11-05
//! §5.7, §6–§7): the Tauri face of `agent_files`. Desktop only — Claude Code
//! and `jodd-mcp` do not run on Android.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::accounts::Account;
use crate::agent_files::{self, skills, SyncOptions, SyncOutcome};
use crate::db::Db;
use crate::mcp_scope::{self, MirrorScope, SkillScope};
use crate::AppState;

#[derive(Debug, Serialize, PartialEq)]
pub struct MirrorAccount {
    pub account_id: String,
    pub email: String,
}

#[derive(Debug, Serialize)]
pub struct AgentFilesView {
    pub enabled: bool,
    /// The folder the mirror uses: the configured one, else the default.
    pub path: String,
    pub default_path: String,
    pub accounts: Vec<String>,
    /// Active accounts the user may choose from.
    pub choices: Vec<MirrorAccount>,
    pub last: Option<agent_files::status::Status>,
    /// The scope file cannot be parsed: nothing can change from here.
    pub error: Option<String>,
    /// Something to tell the user about the last change, e.g. old files that
    /// could not be removed.
    pub notice: Option<String>,
}

fn display(p: Option<PathBuf>) -> String {
    p.map(|p| p.display().to_string()).unwrap_or_default()
}

pub fn view_core(accounts: &[Account], scope_path: &Path) -> AgentFilesView {
    let (scope, error) = match mcp_scope::load_from(scope_path) {
        Ok(s) => (s, None),
        Err(mcp_scope::ScopeError::NotConfigured) => (Default::default(), None),
        Err(mcp_scope::ScopeError::Unparseable(e)) => (Default::default(), Some(e)),
    };
    let m = scope.mirror();
    AgentFilesView {
        enabled: m.enabled,
        path: display(agent_files::mirror_root(m)),
        default_path: display(agent_files::default_mirror_path()),
        accounts: m.accounts.clone(),
        choices: accounts
            .iter()
            .filter(|a| a.is_active())
            .map(|a| MirrorAccount { account_id: a.id.clone(), email: a.email.clone() })
            .collect(),
        last: if m.enabled { agent_files::last_status(&scope) } else { None },
        error,
        notice: None,
    }
}

/// `~/x` → the home folder; blank → `None` (the default).
fn normalize_path(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(p) = raw.map(str::trim).filter(|p| !p.is_empty()) else { return Ok(None) };
    let p = match p.strip_prefix("~/").or_else(|| (p == "~").then_some("")) {
        Some(rest) => dirs::home_dir().ok_or("no home folder")?.join(rest).display().to_string(),
        None => p.to_string(),
    };
    if !Path::new(&p).is_absolute() {
        return Err(format!("Choose a full folder path, not '{p}'."));
    }
    Ok(Some(p))
}

/// Save the settings, then remove the old copy when the mirror was turned
/// off or moved: it is unencrypted, so it should not outlive the setting.
pub fn set_mirror_core(
    accounts: &[Account],
    enabled: bool,
    path: Option<&str>,
    chosen: &[String],
    scope_path: &Path,
) -> Result<AgentFilesView, String> {
    let old = match mcp_scope::load_from(scope_path) {
        Ok(s) => s.mirror().clone(),
        Err(mcp_scope::ScopeError::NotConfigured) => MirrorScope::default(),
        Err(mcp_scope::ScopeError::Unparseable(e)) => {
            return Err(format!("{} cannot be read ({e}). Fix it by hand; Jodd will not overwrite it.", scope_path.display()))
        }
    };
    let path = normalize_path(path)?;
    if let Some(unknown) = chosen.iter().find(|id| !accounts.iter().any(|a| &a.id == *id)) {
        return Err(format!("No account '{unknown}'."));
    }
    let new = MirrorScope { enabled, path, accounts: chosen.to_vec() };
    let (old_root, new_root) = (agent_files::mirror_root(&old), agent_files::mirror_root(&new));
    if enabled {
        let root = new_root.as_deref().ok_or("No home folder to put the mirror in; choose a folder.")?;
        crate::agent_files::mirror::check_root(root)?;
    }
    mcp_scope::edit_at(scope_path, |v| mcp_scope::set_mirror(v, &new))?;

    let mut notice = None;
    if old.enabled && (!enabled || old_root != new_root) {
        if let Some(root) = old_root.as_deref() {
            match agent_files::remove_mirror(root) {
                Ok(n) => crate::log!("agent files: removed {n} mirrored files from {}", root.display()),
                Err(e) => notice = Some(format!("The old copy in {} could not be removed: {e}", root.display())),
            }
        }
    }
    let mut view = view_core(accounts, scope_path);
    view.notice = notice;
    Ok(view)
}

#[tauri::command]
pub fn agent_files_status(state: State<'_, AppState>) -> AgentFilesView {
    let accounts = state.accounts.lock().unwrap().clone();
    view_core(&accounts, &mcp_scope::scope_path())
}

#[tauri::command]
pub fn set_agent_mirror(
    enabled: bool,
    path: Option<String>,
    accounts: Vec<String>,
    state: State<'_, AppState>,
) -> Result<AgentFilesView, String> {
    let all = state.accounts.lock().unwrap().clone();
    set_mirror_core(&all, enabled, path.as_deref(), &accounts, &mcp_scope::scope_path())
}

/// "Sync now": the same engine the hook runs, in-process with the app's own
/// database (no keychain read). `allow_large_delete` is the user's "Apply
/// anyway" after a run held deletions back.
#[tauri::command]
pub async fn sync_agent_files_now(allow_large_delete: bool, state: State<'_, AppState>) -> Result<SyncOutcome, String> {
    let db = state.db.clone();
    let accounts = state.accounts.lock().unwrap().clone();
    tokio::task::spawn_blocking(move || {
        let scope = match mcp_scope::load_from(&mcp_scope::scope_path()) {
            Ok(s) => s,
            Err(mcp_scope::ScopeError::NotConfigured) => return Ok(SyncOutcome::default()),
            Err(mcp_scope::ScopeError::Unparseable(e)) => return Err(format!("mcp_write_scope.json cannot be read ({e})")),
        };
        Ok(agent_files::sync(&db, &accounts, &scope, &SyncOptions { allow_large_delete, ..Default::default() }))
    })
    .await
    .map_err(|e| format!("sync task failed: {e}"))?
}

// ─── Skills (spec §6) ────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SkillRow {
    #[serde(flatten)]
    pub entry: SkillScope,
    pub email: String,
    /// What the last run did with it; `None` before the first run.
    pub status: Option<skills::SkillStatus>,
}

#[derive(Debug, Serialize)]
pub struct SkillsView {
    pub skills_root: String,
    pub skills: Vec<SkillRow>,
    pub last_finished_at: Option<String>,
    pub problems: Vec<String>,
    pub error: Option<String>,
}

pub fn skills_view_core(accounts: &[Account], scope_path: &Path, opts: &SyncOptions) -> SkillsView {
    let (scope, error) = match mcp_scope::load_from(scope_path) {
        Ok(s) => (s, None),
        Err(mcp_scope::ScopeError::NotConfigured) => (Default::default(), None),
        Err(mcp_scope::ScopeError::Unparseable(e)) => (Default::default(), Some(e)),
    };
    let last = agent_files::last_skills_status(opts);
    let skills = scope
        .skills()
        .iter()
        .map(|e| SkillRow {
            entry: e.clone(),
            email: accounts.iter().find(|a| a.id == e.account_id).map(|a| a.email.clone()).unwrap_or_default(),
            status: last.as_ref().and_then(|l| l.skills.iter().find(|s| s.name == e.name && s.folder == e.folder).cloned()),
        })
        .collect();
    SkillsView {
        skills_root: display(opts.skills_root.clone().or_else(skills::default_skills_root)),
        skills,
        last_finished_at: last.as_ref().map(|l| l.finished_at.clone()),
        problems: last.map(|l| l.problems).unwrap_or_default(),
        error,
    }
}

/// Record a folder as a skill (§6.1). The rules that refuse one live in
/// `mcp_scope::set_skill`, so the app and a later reader agree; this adds
/// what only the app can check — the account and the folder exist.
pub fn set_skill_core(db: &Db, accounts: &[Account], entry: SkillScope, scope_path: &Path) -> Result<(), String> {
    let account = accounts
        .iter()
        .find(|a| a.id == entry.account_id && a.is_active())
        .ok_or_else(|| format!("No active account '{}'.", entry.account_id))?;
    crate::folder_label::validate_label_path(&entry.folder)?;
    let exists = db.get_folder(&account.id, &entry.folder).map_err(|e| e.to_string())?.is_some()
        || !db.list_notes_by_label(&account.id, &entry.folder).map_err(|e| e.to_string())?.is_empty();
    if !exists {
        return Err(format!("No folder '{}' in {}.", entry.folder, account.email));
    }
    let assets_dir = normalize_path(entry.assets_dir.as_deref())?;
    if let Some(dir) = assets_dir.as_deref() {
        if !Path::new(dir).is_dir() {
            return Err(format!("The assets folder {dir} does not exist."));
        }
    }
    let entry = SkillScope { assets_dir, ..entry };
    mcp_scope::edit_at(scope_path, |v| mcp_scope::set_skill(v, &entry))
}

/// Stop publishing: the setting goes, and so does the skill folder, at once —
/// Claude Code should not keep following instructions the user withdrew.
pub fn remove_skill_core(name: &str, scope_path: &Path, skills_root: Option<&Path>) -> Result<(), String> {
    mcp_scope::edit_at(scope_path, |v| mcp_scope::remove_skill(v, name))?;
    if let Some(root) = skills_root {
        skills::unpublish(&root.join(format!("{}{name}", skills::PREFIX)))?;
    }
    Ok(())
}

#[tauri::command]
pub fn agent_skills_view(state: State<'_, AppState>) -> SkillsView {
    let accounts = state.accounts.lock().unwrap().clone();
    skills_view_core(&accounts, &mcp_scope::scope_path(), &SyncOptions::default())
}

#[tauri::command]
pub fn set_agent_skill(
    name: String,
    account_id: String,
    folder: String,
    assets_dir: Option<String>,
    only_reviewed: bool,
    state: State<'_, AppState>,
) -> Result<SkillsView, String> {
    let accounts = state.accounts.lock().unwrap().clone();
    let entry = SkillScope { name, account_id, folder, assets_dir, only_reviewed };
    set_skill_core(&state.db, &accounts, entry, &mcp_scope::scope_path())?;
    Ok(skills_view_core(&accounts, &mcp_scope::scope_path(), &SyncOptions::default()))
}

#[tauri::command]
pub fn remove_agent_skill(name: String, state: State<'_, AppState>) -> Result<SkillsView, String> {
    let accounts = state.accounts.lock().unwrap().clone();
    remove_skill_core(&name, &mcp_scope::scope_path(), skills::default_skills_root().as_deref())?;
    Ok(skills_view_core(&accounts, &mcp_scope::scope_path(), &SyncOptions::default()))
}

/// Phase 3 (spec §10): keep the mirror and skills current while the app runs.
/// Started by `.setup()` only after the cache-lock check, so a locked cache
/// never reaches it (gotcha #44); it re-checks the lock each tick anyway.
/// Desktop only: Claude Code does not run on Android.
///
/// Route to the screen (gotcha #6): each run ends with `live::EVENT`, which
/// `AgentFilesSettings` and `AgentSkillsSettings` listen for.
pub fn spawn_live_sync(app: tauri::AppHandle) {
    use crate::agent_files::live::{self, Debouncer, Fingerprint};
    use tauri::{Emitter, Manager};
    tauri::async_runtime::spawn(async move {
        let mut debouncer = Debouncer::default();
        let mut tick = tokio::time::interval(live::TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if crate::cache_lock::engaged().is_some() {
                continue;
            }
            let scope_path = mcp_scope::scope_path();
            // An unreadable file means "write nothing, delete nothing", as in
            // every other caller; a missing one means nothing is configured.
            let Ok(scope) = mcp_scope::load_from(&scope_path) else { continue };
            if !live::wanted(&scope, skills::default_skills_root().as_deref()) {
                continue;
            }
            let state = app.state::<AppState>();
            let db = state.db.clone();
            let accounts = state.accounts.lock().unwrap().clone();
            let fp = Fingerprint::read(&db, &accounts, &scope_path);
            if !debouncer.observe(&fp, std::time::Instant::now()) {
                continue;
            }
            let run = tokio::task::spawn_blocking(move || agent_files::sync(&db, &accounts, &scope, &SyncOptions::default())).await;
            match run {
                Ok(out) if live::was_busy(&out) => continue,
                Ok(out) => {
                    debouncer.ran(&fp);
                    for e in out.mirror_error.iter().chain(out.skills_error.iter()) {
                        crate::log!("agent files: live sync: {e}");
                    }
                    let _ = app.emit(live::EVENT, ());
                }
                Err(e) => {
                    debouncer.ran(&fp);
                    crate::log!("agent files: live sync task failed: {e}");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(id: &str) -> Account {
        serde_json::from_str(&format!(r#"{{"id":"{id}","email":"a@x.com","added_at":"2026-01-01T00:00:00Z","backend_kind":"gmail"}}"#)).unwrap()
    }

    #[test]
    fn enabling_saves_and_turning_off_removes_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let scope = dir.path().join("mcp_write_scope.json");
        let root = dir.path().join("mirror");
        let accounts = vec![acct("gmail:a@x.com")];
        let v = set_mirror_core(&accounts, true, Some(root.to_str().unwrap()), &["gmail:a@x.com".into()], &scope).unwrap();
        assert!(v.enabled);
        assert_eq!(v.path, root.display().to_string());
        assert_eq!(v.accounts, ["gmail:a@x.com"]);

        // A mirror on disk, as a sync would leave it.
        let db = crate::test_support::temp_db();
        let s = mcp_scope::load_from(&scope).unwrap();
        let skills_root = dir.path().join("skills");
        let opts = SyncOptions { skills_root: Some(skills_root), state_dir: Some(dir.path().to_path_buf()), ..Default::default() };
        assert!(agent_files::sync(&db, &accounts, &s, &opts).mirror.is_some());
        assert!(root.join("README.md").exists());

        let off = set_mirror_core(&accounts, false, Some(root.to_str().unwrap()), &[], &scope).unwrap();
        assert!(!off.enabled && off.notice.is_none());
        assert!(!root.exists(), "the unencrypted copy goes with the setting");
    }

    #[test]
    fn a_folder_with_other_files_and_unknown_accounts_are_refused_before_saving() {
        let dir = tempfile::tempdir().unwrap();
        let scope = dir.path().join("mcp_write_scope.json");
        let busy = dir.path().join("busy");
        std::fs::create_dir_all(&busy).unwrap();
        std::fs::write(busy.join("x.txt"), "x").unwrap();
        let accounts = vec![acct("gmail:a@x.com")];
        assert!(set_mirror_core(&accounts, true, Some(busy.to_str().unwrap()), &[], &scope).is_err());
        assert!(set_mirror_core(&accounts, true, Some("relative/path"), &[], &scope).is_err());
        assert!(set_mirror_core(&accounts, true, None, &["gmail:nobody@x.com".into()], &scope).is_err());
        assert!(!scope.exists(), "nothing was written");
    }

    #[test]
    fn a_skill_is_saved_published_and_withdrawn_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let scope = dir.path().join("mcp_write_scope.json");
        let skills_root = dir.path().join("skills");
        let db = crate::test_support::temp_db();
        let accounts = vec![acct("gmail:a@x.com")];
        let n = crate::test_support::note("gmail:a@x.com", "c").label("Notes/Brand").title("Brand").body("Use when making slides.").build();
        db.upsert_from_remote(&n, crate::db::RemotePin::RemoteWins).unwrap();
        db.record_full_listing("gmail:a@x.com").unwrap();
        let entry = |folder: &str| SkillScope { name: "brand".into(), account_id: "gmail:a@x.com".into(), folder: folder.into(), assets_dir: None, only_reviewed: false };
        assert!(set_skill_core(&db, &accounts, entry("Notes/Nope"), &scope).is_err(), "folder must exist");
        assert!(set_skill_core(&db, &accounts, SkillScope { assets_dir: Some("/no/such/dir".into()), ..entry("Notes/Brand") }, &scope).is_err());
        set_skill_core(&db, &accounts, entry("Notes/Brand"), &scope).unwrap();

        let opts = SyncOptions { skills_root: Some(skills_root.clone()), state_dir: Some(dir.path().to_path_buf()), ..Default::default() };
        let out = agent_files::sync(&db, &accounts, &mcp_scope::load_from(&scope).unwrap(), &opts);
        assert!(out.mirror.is_none(), "the mirror is off");
        assert_eq!(out.skills.unwrap().skills[0].state, skills::PUBLISHED);
        let view = skills_view_core(&accounts, &scope, &opts);
        assert_eq!(view.skills[0].status.as_ref().unwrap().state, skills::PUBLISHED);
        assert!(skills_root.join("jodd-brand/SKILL.md").exists());

        remove_skill_core("brand", &scope, Some(&skills_root)).unwrap();
        assert!(!skills_root.join("jodd-brand").exists());
        assert!(skills_view_core(&accounts, &scope, &opts).skills.is_empty());
    }

    /// Gotcha #6: a background run must tell the screen, after it has run.
    #[test]
    fn the_live_loop_emits_after_each_run_and_skips_a_locked_cache() {
        let body = crate::test_support::extract_fn_body(include_str!("agent_files_commands.rs"), "pub fn spawn_live_sync(");
        let run = body.find("agent_files::sync(").expect("runs the engine");
        let emit = body.find("app.emit(live::EVENT").expect("tells the frontend");
        assert!(run < emit);
        assert!(body.find("cache_lock::engaged()").expect("checks the lock") < run);
    }

    #[test]
    fn an_unparseable_scope_file_is_reported_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let scope = dir.path().join("mcp_write_scope.json");
        std::fs::write(&scope, "{ broken").unwrap();
        assert!(view_core(&[], &scope).error.is_some());
        assert!(set_mirror_core(&[], false, None, &[], &scope).is_err());
        assert_eq!(std::fs::read_to_string(&scope).unwrap(), "{ broken");
    }
}
