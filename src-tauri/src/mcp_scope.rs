//! The one model of `mcp_write_scope.json` (spec 2026-10-06 §5.1, §5.3).
//!
//! `jodd-mcp` reads it to decide what an agent may write and read; since the
//! agent-workspace milestone the app also WRITES it (enable the workspace,
//! hide a folder). One reader of the format here, used by both, so the two
//! cannot disagree about what the file says.
//!
//! The file was hand-edited only until now, so every write goes through
//! `edit_at`: it edits the parsed JSON in place — unknown keys, other
//! accounts and legacy bare-email keys survive — refuses to touch a file it
//! cannot parse, and replaces the file atomically.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The agent workspace root (spec §5.1). It can never be hidden.
pub const AGENT_WORKSPACE: &str = "Notes/__Agent__";

#[derive(Debug, Default, Deserialize)]
pub struct ScopeFile {
    #[serde(default)]
    pub agent_workspace: Option<AgentWorkspace>,
    #[serde(default)]
    pub accounts: HashMap<String, AccountScope>,
    /// Files Jodd writes for Claude Code (spec 2026-11-05 §4.1).
    #[serde(default)]
    pub agent_files: AgentFilesScope,
}

#[derive(Debug, Default, Deserialize)]
pub struct AgentFilesScope {
    #[serde(default)]
    pub mirror: MirrorScope,
    /// Folders published as Claude Code skills (spec 2026-11-05 §6).
    #[serde(default)]
    pub skills: Vec<SkillScope>,
}

/// One folder published as `~/.claude/skills/jodd-<name>/`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct SkillScope {
    /// Without the `jodd-` prefix; see [`check_skill_name`].
    pub name: String,
    pub account_id: String,
    pub folder: String,
    /// A folder on disk whose files are copied into the skill's `assets/`
    /// (fonts, templates — things a note cannot carry).
    #[serde(default)]
    pub assets_dir: Option<String>,
    /// Publish only reviewed notes: an iPhone edit waits too (§6.5).
    #[serde(default)]
    pub only_reviewed: bool,
}

/// Longest name without the prefix, so `jodd-<name>` fits in 64 characters.
pub const SKILL_NAME_MAX: usize = 58;

/// Lowercase ASCII letters, digits and single hyphens — stricter than Claude
/// Code needs, so a skill also uploads to claude.ai unchanged.
pub fn check_skill_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= SKILL_NAME_MAX
        && name.split('-').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "'{name}' is not a usable skill name: use lowercase letters, digits and single hyphens, at most {SKILL_NAME_MAX} characters."
        ))
    }
}

/// One folder inside the other, either way (`/`-boundary, gotcha #33).
pub fn folders_overlap(a: &str, b: &str) -> bool {
    use crate::folder_scope::{matches, Mode};
    matches(a, b, Mode::Subtree) || matches(b, a, Mode::Subtree)
}

/// The read-only Markdown mirror (spec 2026-11-05 §5). Off unless enabled.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
pub struct MirrorScope {
    #[serde(default)]
    pub enabled: bool,
    /// Absolute path; `None` means `agent_files::default_mirror_path()`.
    #[serde(default)]
    pub path: Option<String>,
    /// Account ids whose notes are mirrored. Opt-in: empty mirrors nothing.
    #[serde(default)]
    pub accounts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentWorkspace {
    pub account_id: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct AccountScope {
    #[serde(default)]
    pub allowed_folders: Vec<String>,
    #[serde(default)]
    pub hidden_folders: Vec<String>,
}

#[derive(Debug)]
pub enum ScopeError {
    NotConfigured,
    Unparseable(String),
}

impl ScopeFile {
    /// This account's entry — keyed by the account id, falling back to the
    /// **pre-migration bare email**. Nothing migrates this hand-edited file,
    /// so after migration #19 its keys are bare emails while every
    /// `Account.id` is `{backend}:{email}`; looking up only the qualified id
    /// would match nothing. The qualified key wins when both are present.
    fn entry(&self, account_id: &str) -> Option<&AccountScope> {
        self.accounts.get(account_id).or_else(|| {
            crate::accounts::legacy_bare_id(account_id).and_then(|bare| self.accounts.get(&bare))
        })
    }

    pub fn allowed_folders(&self, account_id: &str) -> &[String] {
        self.entry(account_id).map(|e| e.allowed_folders.as_slice()).unwrap_or(&[])
    }

    pub fn hidden_folders(&self, account_id: &str) -> &[String] {
        self.entry(account_id).map(|e| e.hidden_folders.as_slice()).unwrap_or(&[])
    }

    /// Subtree match through the shared matcher (gotcha #33): `Notes/Work`
    /// hides `Notes/Work/X`, never `Notes/WorkX`.
    pub fn is_hidden(&self, account_id: &str, label: &str) -> bool {
        self.hidden_folders(account_id)
            .iter()
            .any(|h| crate::folder_scope::matches(label, h, crate::folder_scope::Mode::Subtree))
    }

    /// The workspace account, if one is enabled.
    pub fn workspace_account(&self) -> Option<&str> {
        self.agent_workspace.as_ref().map(|w| w.account_id.as_str())
    }

    pub fn mirror(&self) -> &MirrorScope {
        &self.agent_files.mirror
    }

    pub fn skills(&self) -> &[SkillScope] {
        &self.agent_files.skills
    }

    /// Whether an agent may write in `folder` or anywhere inside it — the
    /// agent workspace included, enabled or not. A skill is instructions, so
    /// a folder an agent could write cannot be one (spec §6.1).
    pub fn agent_writable(&self, account_id: &str, folder: &str) -> bool {
        folders_overlap(folder, AGENT_WORKSPACE) || self.allowed_folders(account_id).iter().any(|a| folders_overlap(folder, a))
    }
}

/// Beside accounts.json: `<config base>/jodd/mcp_write_scope.json`.
///
/// Resolved through the platform seam (`paths::config_base()`), never
/// `dirs::` directly: on Android `dirs::config_dir()` has no answer, so the
/// scope file (which carries the hidden-from-agents folders) used to land in
/// the temp dir. On desktop `config_base()` is `dirs::config_dir()` (Tauri's
/// unsuffixed `config_dir()` is the same call), so the path is unchanged and
/// `jodd-mcp` — which never runs `paths::init` — still agrees with the app.
pub fn scope_path() -> PathBuf {
    scope_path_from(crate::paths::config_base())
}

/// Pure half of [`scope_path`], testable without the `paths` OnceLocks. The
/// temp-dir fallback is for a platform with no config base at all.
fn scope_path_from(config_base: Option<PathBuf>) -> PathBuf {
    config_base
        .map(|d| d.join("jodd"))
        .unwrap_or_else(|| std::env::temp_dir().join("jodd"))
        .join("mcp_write_scope.json")
}

pub fn load_from(path: &Path) -> Result<ScopeFile, ScopeError> {
    let raw = std::fs::read_to_string(path).map_err(|_| ScopeError::NotConfigured)?;
    serde_json::from_str(&raw).map_err(|e| ScopeError::Unparseable(e.to_string()))
}

/// Edit the file's JSON in place and replace it atomically. A missing file
/// starts as `{}`; an unparseable one is an error and is left untouched —
/// overwriting what the user wrote because we could not read it would be
/// the worst outcome of letting the app write this file at all.
pub fn edit_at(path: &Path, f: impl FnOnce(&mut serde_json::Value) -> Result<(), String>) -> Result<(), String> {
    let mut value: serde_json::Value = match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map_err(|e| format!("{} is not valid JSON ({e}); fix it by hand — Jodd will not overwrite it", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    if !value.is_object() {
        return Err(format!("{} is not a JSON object; fix it by hand", path.display()));
    }
    f(&mut value)?;
    let parent = path.parent().ok_or("scope file has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| format!("temp file: {e}"))?;
    use std::io::Write;
    tmp.write_all(serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?.as_bytes())
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|e| format!("write scope: {e}"))?;
    tmp.persist(path).map_err(|e| format!("replace {}: {}", path.display(), e.error))?;
    Ok(())
}

/// The key to edit for `account_id`: the qualified id if present, else an
/// existing legacy bare-email entry (edited in place, never duplicated),
/// else the qualified id (a new entry).
fn entry_key(accounts: &serde_json::Map<String, serde_json::Value>, account_id: &str) -> String {
    if accounts.contains_key(account_id) {
        return account_id.to_string();
    }
    match crate::accounts::legacy_bare_id(account_id) {
        Some(bare) if accounts.contains_key(&bare) => bare,
        _ => account_id.to_string(),
    }
}

fn account_entry<'a>(v: &'a mut serde_json::Value, account_id: &str) -> Result<&'a mut serde_json::Map<String, serde_json::Value>, String> {
    let root = v.as_object_mut().ok_or("scope file is not a JSON object")?;
    let accounts = root.entry("accounts").or_insert_with(|| serde_json::json!({}));
    let accounts = accounts.as_object_mut().ok_or("\"accounts\" is not a JSON object")?;
    let key = entry_key(accounts, account_id);
    accounts
        .entry(key)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("the entry for {account_id} is not a JSON object"))
}

fn string_list<'a>(entry: &'a mut serde_json::Map<String, serde_json::Value>, key: &str) -> Result<&'a mut Vec<serde_json::Value>, String> {
    entry
        .entry(key)
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| format!("\"{key}\" is not a list"))
}

/// Record `account_id` as the workspace account and allow writes to
/// `AGENT_WORKSPACE` there. Idempotent; never removes anything.
pub fn enable_workspace(v: &mut serde_json::Value, account_id: &str) -> Result<(), String> {
    // The other half of §6.1: agents must never gain write access to a skill.
    if let Ok(current) = serde_json::from_value::<ScopeFile>(v.clone()) {
        if let Some(s) = current.skills().iter().find(|s| s.account_id == account_id && folders_overlap(&s.folder, AGENT_WORKSPACE)) {
            return Err(format!("Stop publishing the skill '{}' first: agents would be able to write it.", s.name));
        }
    }
    let root = v.as_object_mut().ok_or("scope file is not a JSON object")?;
    root.insert("agent_workspace".into(), serde_json::json!({ "account_id": account_id }));
    let allowed = string_list(account_entry(v, account_id)?, "allowed_folders")?;
    if !allowed.iter().any(|a| a.as_str() == Some(AGENT_WORKSPACE)) {
        allowed.push(serde_json::json!(AGENT_WORKSPACE));
    }
    Ok(())
}

/// Replace the mirror settings (`agent_files.mirror`). Other keys under
/// `agent_files`, such as a later phase's `skills`, are kept.
pub fn set_mirror(v: &mut serde_json::Value, mirror: &MirrorScope) -> Result<(), String> {
    let root = v.as_object_mut().ok_or("scope file is not a JSON object")?;
    let files = root.entry("agent_files").or_insert_with(|| serde_json::json!({}));
    let files = files.as_object_mut().ok_or("\"agent_files\" is not an object")?;
    files.insert(
        "mirror".into(),
        serde_json::json!({ "enabled": mirror.enabled, "path": mirror.path, "accounts": mirror.accounts }),
    );
    Ok(())
}

/// Add a skill, or update the one with the same name and folder. Refused for
/// a bad name, a name another folder uses, a folder another skill already
/// publishes, a hidden folder, and any folder an agent may write (§6.1).
pub fn set_skill(v: &mut serde_json::Value, skill: &SkillScope) -> Result<(), String> {
    check_skill_name(&skill.name)?;
    let current: ScopeFile = serde_json::from_value(v.clone()).map_err(|e| format!("scope file: {e}"))?;
    if current.agent_writable(&skill.account_id, &skill.folder) {
        return Err(format!(
            "Agents can write in '{}', so it cannot be a skill: a skill is instructions Claude Code follows, and an agent could write its own.",
            skill.folder
        ));
    }
    if current.is_hidden(&skill.account_id, &skill.folder) {
        return Err(format!("'{}' is hidden from AI, so it cannot be a skill.", skill.folder));
    }
    let same_place = |s: &SkillScope| s.account_id == skill.account_id && s.folder == skill.folder;
    if let Some(other) = current.skills().iter().find(|s| s.name == skill.name && !same_place(s)) {
        return Err(format!("The name '{}' is already used for {}.", skill.name, other.folder));
    }
    if let Some(other) = current.skills().iter().find(|s| same_place(s) && s.name != skill.name) {
        return Err(format!("'{}' is already published as '{}'. Stop publishing it first.", skill.folder, other.name));
    }
    let root = v.as_object_mut().ok_or("scope file is not a JSON object")?;
    let files = root.entry("agent_files").or_insert_with(|| serde_json::json!({}));
    let files = files.as_object_mut().ok_or("\"agent_files\" is not an object")?;
    let list = files.entry("skills").or_insert_with(|| serde_json::json!([]));
    let list = list.as_array_mut().ok_or("\"skills\" is not a list")?;
    list.retain(|e| e.get("name").and_then(|n| n.as_str()) != Some(skill.name.as_str()));
    list.push(serde_json::to_value(skill).map_err(|e| e.to_string())?);
    Ok(())
}

/// Stop publishing `name`. Removing a name that is not there is not an error.
pub fn remove_skill(v: &mut serde_json::Value, name: &str) -> Result<(), String> {
    let Some(list) = v.get_mut("agent_files").and_then(|f| f.get_mut("skills")).and_then(|l| l.as_array_mut()) else {
        return Ok(());
    };
    list.retain(|e| e.get("name").and_then(|n| n.as_str()) != Some(name));
    Ok(())
}

/// Hide or un-hide one folder from agents. The workspace cannot be hidden:
/// an agent that cannot read its own memory has no memory.
pub fn set_hidden(v: &mut serde_json::Value, account_id: &str, folder: &str, hidden: bool) -> Result<(), String> {
    if hidden && (crate::folder_scope::matches(folder, AGENT_WORKSPACE, crate::folder_scope::Mode::Subtree)
        || crate::folder_scope::matches(AGENT_WORKSPACE, folder, crate::folder_scope::Mode::Subtree))
    {
        return Err(format!("'{folder}' holds the agent workspace, which agents must be able to read."));
    }
    let list = string_list(account_entry(v, account_id)?, "hidden_folders")?;
    let present = list.iter().position(|h| h.as_str() == Some(folder));
    match (hidden, present) {
        (true, None) => list.push(serde_json::json!(folder)),
        (false, Some(i)) => {
            list.remove(i);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn scope_path_is_under_app_data_on_android() {
        // Android: paths::init hands the app-private dir as the config base.
        let base = PathBuf::from("/data/user/0/co.bbmedia.jodd/files");
        assert_eq!(
            scope_path_from(Some(base.clone())),
            base.join("jodd").join("mcp_write_scope.json")
        );
    }

    #[test]
    fn scope_path_falls_back_to_temp_only_without_a_config_base() {
        assert_eq!(
            scope_path_from(None),
            std::env::temp_dir().join("jodd").join("mcp_write_scope.json")
        );
    }

    /// Desktop identity: with no `paths::init` (jodd-mcp, tests) the seam falls
    /// back to `dirs::config_dir()`, so the path is what it always was.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn scope_path_on_desktop_is_unchanged() {
        let legacy = dirs::config_dir()
            .map(|d| d.join("jodd"))
            .unwrap_or_else(|| std::env::temp_dir().join("jodd"))
            .join("mcp_write_scope.json");
        assert_eq!(scope_path(), legacy);
    }

    use super::*;

    fn write(dir: &tempfile::TempDir, json: &str) -> PathBuf {
        let p = dir.path().join("mcp_write_scope.json");
        std::fs::write(&p, json).unwrap();
        p
    }

    #[test]
    fn legacy_bare_key_still_resolves_for_both_lists() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, r#"{"accounts":{"a@x.com":{"allowed_folders":["Notes/__Claude__"],"hidden_folders":["Notes/Personal"]}}}"#);
        let s = load_from(&p).unwrap();
        assert_eq!(s.allowed_folders("gmail:a@x.com"), ["Notes/__Claude__".to_string()]);
        assert!(s.is_hidden("gmail:a@x.com", "Notes/Personal/Diary"));
        assert!(!s.is_hidden("gmail:b@x.com", "Notes/Personal"));
    }

    #[test]
    fn hidden_folders_default_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, r#"{"accounts":{"gmail:a@x.com":{"allowed_folders":["Notes/X"]}}}"#);
        let s = load_from(&p).unwrap();
        assert!(s.hidden_folders("gmail:a@x.com").is_empty());
        assert!(!s.is_hidden("gmail:a@x.com", "Notes/X"));
        assert!(s.workspace_account().is_none());
    }

    #[test]
    fn is_hidden_is_a_subtree_not_a_string_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, r#"{"accounts":{"gmail:a@x.com":{"hidden_folders":["Notes/Work"]}}}"#);
        let s = load_from(&p).unwrap();
        assert!(s.is_hidden("gmail:a@x.com", "Notes/Work"));
        assert!(s.is_hidden("gmail:a@x.com", "Notes/Work/Clients"));
        assert!(!s.is_hidden("gmail:a@x.com", "Notes/WorkX"));
        assert!(!s.is_hidden("gmail:a@x.com", "Notes"));
    }

    #[test]
    fn edits_preserve_unknown_keys_other_accounts_and_legacy_entries() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, r#"{"comment":"mine","accounts":{
            "a@x.com":{"allowed_folders":["Notes/__Claude__"],"note":"keep"},
            "localfs:Z":{"allowed_folders":["Notes/__Claude__"]}}}"#);
        edit_at(&p, |v| enable_workspace(v, "gmail:a@x.com")).unwrap();
        edit_at(&p, |v| set_hidden(v, "gmail:a@x.com", "Notes/Personal", true)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["comment"], "mine");
        assert_eq!(v["accounts"]["localfs:Z"]["allowed_folders"], serde_json::json!(["Notes/__Claude__"]));
        // The legacy bare entry was edited in place, not duplicated.
        assert!(v["accounts"].get("gmail:a@x.com").is_none(), "{v}");
        assert_eq!(v["accounts"]["a@x.com"]["note"], "keep");
        assert_eq!(v["accounts"]["a@x.com"]["allowed_folders"], serde_json::json!(["Notes/__Claude__", AGENT_WORKSPACE]));
        assert_eq!(v["accounts"]["a@x.com"]["hidden_folders"], serde_json::json!(["Notes/Personal"]));
        assert_eq!(load_from(&p).unwrap().workspace_account(), Some("gmail:a@x.com"));
    }

    #[test]
    fn enable_workspace_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp_write_scope.json");
        edit_at(&p, |v| enable_workspace(v, "gmail:a@x.com")).unwrap(); // also: a missing file is created
        edit_at(&p, |v| enable_workspace(v, "gmail:a@x.com")).unwrap();
        let s = load_from(&p).unwrap();
        assert_eq!(s.allowed_folders("gmail:a@x.com"), [AGENT_WORKSPACE.to_string()]);
    }

    #[test]
    fn the_workspace_and_its_ancestors_cannot_be_hidden() {
        let mut v = serde_json::json!({});
        for f in [AGENT_WORKSPACE, "Notes/__Agent__/Projects", "Notes"] {
            assert!(set_hidden(&mut v, "gmail:a@x.com", f, true).is_err(), "{f}");
        }
        assert!(set_hidden(&mut v, "gmail:a@x.com", "Notes/__AgentX", true).is_ok());
    }

    #[test]
    fn unhiding_removes_only_that_folder() {
        let mut v = serde_json::json!({});
        set_hidden(&mut v, "gmail:a@x.com", "Notes/A", true).unwrap();
        set_hidden(&mut v, "gmail:a@x.com", "Notes/B", true).unwrap();
        set_hidden(&mut v, "gmail:a@x.com", "Notes/A", true).unwrap();
        set_hidden(&mut v, "gmail:a@x.com", "Notes/A", false).unwrap();
        assert_eq!(v["accounts"]["gmail:a@x.com"]["hidden_folders"], serde_json::json!(["Notes/B"]));
    }

    #[test]
    fn mirror_settings_round_trip_and_keep_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, r#"{"accounts":{"a@x.com":{"allowed_folders":["Notes/__Claude__"]}},"agent_files":{"skills":[{"name":"x","account_id":"gmail:a@x.com","folder":"Notes/Brand"}]},"other":1}"#);
        assert_eq!(load_from(&p).unwrap().mirror(), &MirrorScope::default(), "off unless enabled");
        let m = MirrorScope { enabled: true, path: Some("/m".into()), accounts: vec!["gmail:a@x.com".into()] };
        edit_at(&p, |v| set_mirror(v, &m)).unwrap();
        let scope = load_from(&p).unwrap();
        assert_eq!(scope.mirror(), &m);
        assert_eq!(scope.allowed_folders("gmail:a@x.com"), ["Notes/__Claude__".to_string()]);
        let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(raw["agent_files"]["skills"][0]["name"], "x");
        assert_eq!(raw["other"], 1);
    }

    fn skill(name: &str, folder: &str) -> SkillScope {
        SkillScope { name: name.into(), account_id: "gmail:a@x.com".into(), folder: folder.into(), assets_dir: None, only_reviewed: false }
    }

    #[test]
    fn skill_names_are_strict() {
        for ok in ["brand", "brand-bbmedia", "a1-b2"] {
            assert!(check_skill_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Brand", "brand_x", "-brand", "brand-", "a--b", "แบรนด์", &"a".repeat(SKILL_NAME_MAX + 1)] {
            assert!(check_skill_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_skill_is_refused_where_agents_can_write_and_agents_never_gain_a_skill() {
        let mut v = serde_json::json!({ "accounts": { "gmail:a@x.com": { "allowed_folders": ["Notes/__Claude__", "Notes/Shared/Inbox"] } } });
        for f in ["Notes/__Claude__", "Notes/__Claude__/X", "Notes/Shared", "Notes", "Notes/__Agent__/Skills"] {
            assert!(set_skill(&mut v, &skill("x", f)).is_err(), "{f}");
        }
        set_skill(&mut v, &skill("brand", "Notes/Brand")).unwrap();
        set_skill(&mut v, &skill("brand", "Notes/Brand")).unwrap();
        assert_eq!(v["agent_files"]["skills"].as_array().unwrap().len(), 1, "updating keeps one entry");
        assert!(set_skill(&mut v, &skill("brand", "Notes/Other")).is_err(), "name taken");
        assert!(set_skill(&mut v, &skill("brand2", "Notes/Brand")).is_err(), "folder already published");
        set_hidden(&mut v, "gmail:a@x.com", "Notes/Secret", true).unwrap();
        assert!(set_skill(&mut v, &skill("s", "Notes/Secret/Deep")).is_err(), "hidden");

        let mut w = serde_json::json!({});
        set_skill(&mut w, &SkillScope { folder: "Notes/Brand".into(), ..skill("b", "x") }).unwrap();
        w["agent_files"]["skills"].as_array_mut().unwrap().push(serde_json::to_value(skill("agent", "Notes/__Agent__/Playbook")).unwrap());
        assert!(enable_workspace(&mut w, "gmail:a@x.com").is_err(), "enabling would make a skill agent-writable");
        remove_skill(&mut w, "agent").unwrap();
        enable_workspace(&mut w, "gmail:a@x.com").unwrap();
        assert_eq!(serde_json::from_value::<ScopeFile>(w).unwrap().skills().len(), 1);
    }

    #[test]
    fn an_unparseable_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "{ not json");
        assert!(edit_at(&p, |v| enable_workspace(v, "gmail:a@x.com")).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json");
        assert!(matches!(load_from(&p), Err(ScopeError::Unparseable(_))));
    }
}
