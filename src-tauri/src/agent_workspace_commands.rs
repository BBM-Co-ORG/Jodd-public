//! Settings → Agent workspace (spec 2026-10-06 §5.1, §5.3): enable the
//! workspace in one account, hide folders from agents. The rules are in
//! `agent_memory` and `mcp_scope`; this file is the Tauri face of them.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use tauri::{Emitter, State};

use crate::accounts::Account;
use crate::db::Db;
use crate::AppState;

#[derive(Debug, Serialize, PartialEq)]
pub struct EligibleAccount {
    pub account_id: String,
    pub email: String,
}

#[derive(Debug, Serialize)]
pub struct AgentWorkspaceStatus {
    /// The workspace account, when enabled.
    pub account_id: Option<String>,
    pub folder: &'static str,
    /// Active accounts that can hold the workspace (notes AND folders writable).
    pub eligible: Vec<EligibleAccount>,
    /// Hidden folders per account id, as the scope file says.
    pub hidden: HashMap<String, Vec<String>>,
    pub scope_path: String,
    /// The scope file exists but cannot be parsed: nothing can be changed
    /// from here until the user fixes it by hand.
    pub error: Option<String>,
}

fn eligible(accounts: &[Account]) -> Vec<EligibleAccount> {
    accounts
        .iter()
        .filter(|a| a.is_active())
        .filter(|a| {
            let w = crate::backend::Capabilities::for_backend(a.backend_kind).writes;
            w.notes && w.folders
        })
        .map(|a| EligibleAccount { account_id: a.id.clone(), email: a.email.clone() })
        .collect()
}

pub fn status_core(accounts: &[Account], path: &Path) -> AgentWorkspaceStatus {
    let (scope, error) = match crate::mcp_scope::load_from(path) {
        Ok(s) => (s, None),
        Err(crate::mcp_scope::ScopeError::NotConfigured) => (Default::default(), None),
        Err(crate::mcp_scope::ScopeError::Unparseable(e)) => (Default::default(), Some(e)),
    };
    let hidden = accounts
        .iter()
        .filter(|a| a.is_active())
        .map(|a| (a.id.clone(), scope.hidden_folders(&a.id).to_vec()))
        .filter(|(_, h)| !h.is_empty())
        .collect();
    AgentWorkspaceStatus {
        account_id: scope.workspace_account().map(str::to_string),
        folder: crate::agent_memory::ROOT,
        eligible: eligible(accounts),
        hidden,
        scope_path: path.display().to_string(),
        error,
    }
}

/// SQLite first (local-first: the skeleton is the user-visible part), then
/// the scope file. If the file write fails the skeleton stays — empty pages
/// in a folder are harmless — and the error is shown.
pub fn enable_core(db: &Db, accounts: &[Account], account_id: &str, path: &Path) -> Result<(), String> {
    let account = accounts
        .iter()
        .find(|a| a.id == account_id && a.is_active())
        .ok_or_else(|| format!("No active account '{account_id}'."))?;
    crate::agent_memory::ensure_skeleton(db, account)?;
    crate::mcp_scope::edit_at(path, |v| crate::mcp_scope::enable_workspace(v, account_id))
}

pub fn set_hidden_core(accounts: &[Account], account_id: &str, folder: &str, hidden: bool, path: &Path) -> Result<(), String> {
    if !accounts.iter().any(|a| a.id == account_id) {
        return Err(format!("No account '{account_id}'."));
    }
    crate::folder_label::validate_label_path(folder)?;
    crate::mcp_scope::edit_at(path, |v| crate::mcp_scope::set_hidden(v, account_id, folder, hidden))
}

#[tauri::command]
pub fn agent_workspace_status(state: State<'_, AppState>) -> AgentWorkspaceStatus {
    let accounts = state.accounts.lock().unwrap().clone();
    status_core(&accounts, &crate::mcp_scope::scope_path())
}

#[tauri::command]
pub fn enable_agent_workspace(account_id: String, app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let accounts = state.accounts.lock().unwrap().clone();
    enable_core(&state.db, &accounts, &account_id, &crate::mcp_scope::scope_path())?;
    crate::log!("agent workspace: enabled in {account_id}");
    // Gotcha #6: the skeleton's folders and pages were written by Rust; the
    // sidebar learns of them only through this nudge.
    let _ = app.emit("remote-changed", account_id);
    Ok(())
}

#[tauri::command]
pub fn set_folder_hidden_from_agents(account_id: String, folder: String, hidden: bool, state: State<'_, AppState>) -> Result<(), String> {
    let accounts = state.accounts.lock().unwrap().clone();
    set_hidden_core(&accounts, &account_id, &folder, hidden, &crate::mcp_scope::scope_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_db;

    fn acct(id: &str, backend: &str) -> Account {
        serde_json::from_str(&format!(r#"{{"id":"{id}","email":"a@x.com","added_at":"2026-01-01T00:00:00Z","backend_kind":"{backend}"}}"#)).unwrap()
    }

    #[test]
    fn only_folder_capable_active_accounts_are_eligible() {
        let accounts = vec![acct("gmail:a@x.com", "gmail"), acct("microsoft:a@x.com", "microsoft")];
        assert_eq!(eligible(&accounts).iter().map(|e| e.account_id.as_str()).collect::<Vec<_>>(), ["gmail:a@x.com"]);
    }

    #[test]
    fn enabling_creates_the_skeleton_and_records_the_workspace() {
        let (db, dir) = (temp_db(), tempfile::tempdir().unwrap());
        let path = dir.path().join("mcp_write_scope.json");
        std::fs::write(&path, r#"{"accounts":{"a@x.com":{"allowed_folders":["Notes/__Claude__"]}}}"#).unwrap();
        let accounts = vec![acct("gmail:a@x.com", "gmail")];
        enable_core(&db, &accounts, "gmail:a@x.com", &path).unwrap();
        assert!(db.get_folder("gmail:a@x.com", crate::agent_memory::PROJECTS).unwrap().is_some());
        let s = status_core(&accounts, &path);
        assert_eq!(s.account_id.as_deref(), Some("gmail:a@x.com"));
        let scope = crate::mcp_scope::load_from(&path).unwrap();
        assert_eq!(scope.allowed_folders("gmail:a@x.com"), ["Notes/__Claude__".to_string(), crate::agent_memory::ROOT.to_string()]);
    }

    #[test]
    fn an_ineligible_account_is_refused_before_anything_is_written() {
        let (db, dir) = (temp_db(), tempfile::tempdir().unwrap());
        let path = dir.path().join("mcp_write_scope.json");
        let accounts = vec![acct("microsoft:a@x.com", "microsoft")];
        assert!(enable_core(&db, &accounts, "microsoft:a@x.com", &path).is_err());
        assert!(!path.exists());
        assert!(db.get_folder("microsoft:a@x.com", crate::agent_memory::ROOT).unwrap().is_none());
    }

    #[test]
    fn hiding_round_trips_through_status_and_an_unparseable_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp_write_scope.json");
        let accounts = vec![acct("gmail:a@x.com", "gmail")];
        set_hidden_core(&accounts, "gmail:a@x.com", "Notes/Personal", true, &path).unwrap();
        assert_eq!(status_core(&accounts, &path).hidden["gmail:a@x.com"], ["Notes/Personal"]);
        assert!(set_hidden_core(&accounts, "gmail:a@x.com", "Notes/../x", true, &path).is_err());
        std::fs::write(&path, "{ broken").unwrap();
        assert!(status_core(&accounts, &path).error.is_some());
        assert!(set_hidden_core(&accounts, "gmail:a@x.com", "Notes/Other", true, &path).is_err());
    }
}
