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
}

/// Beside accounts.json: `<os config dir>/jodd/mcp_write_scope.json`.
pub fn scope_path() -> PathBuf {
    dirs::config_dir()
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
    let root = v.as_object_mut().ok_or("scope file is not a JSON object")?;
    root.insert("agent_workspace".into(), serde_json::json!({ "account_id": account_id }));
    let allowed = string_list(account_entry(v, account_id)?, "allowed_folders")?;
    if !allowed.iter().any(|a| a.as_str() == Some(AGENT_WORKSPACE)) {
        allowed.push(serde_json::json!(AGENT_WORKSPACE));
    }
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
    fn an_unparseable_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "{ not json");
        assert!(edit_at(&p, |v| enable_workspace(v, "gmail:a@x.com")).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json");
        assert!(matches!(load_from(&p), Err(ScopeError::Unparseable(_))));
    }
}
