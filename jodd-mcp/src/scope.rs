use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct WriteScope {
    #[serde(default)]
    pub accounts: HashMap<String, AccountScope>,
}

#[derive(Debug, Default, Deserialize)]
pub struct AccountScope {
    #[serde(default)]
    pub allowed_folders: Vec<String>,
}

#[derive(Debug)]
pub enum ScopeError {
    NotConfigured,
    Unparseable(String),
}

/// Beside accounts.json: <os config dir>/jodd/mcp_write_scope.json.
/// Same plain-`dirs` resolution style resolve_db_path already uses —
/// jodd-mcp is desktop-only, no Tauri context needed.
pub fn scope_path() -> PathBuf {
    dirs::config_dir()
        .map(|d| d.join("jodd"))
        .unwrap_or_else(|| std::env::temp_dir().join("jodd"))
        .join("mcp_write_scope.json")
}

pub fn load_write_scope_from(path: &Path) -> Result<WriteScope, ScopeError> {
    let raw = std::fs::read_to_string(path).map_err(|_| ScopeError::NotConfigured)?;
    serde_json::from_str(&raw).map_err(|e| ScopeError::Unparseable(e.to_string()))
}

/// Recursive-subtree match, gotcha #1's shape: exact OR "{allowed}/" prefix.
/// The '/' in the prefix is load-bearing — bare starts_with would leak
/// Notes/Work into Notes/WorkX.
pub fn folder_allowed(allowed: &[String], label: &str) -> bool {
    allowed
        .iter()
        .any(|a| label == a || label.starts_with(&format!("{}/", a)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtree_matches_but_siblings_do_not() {
        let allowed = vec!["Notes/Work".to_string()];
        assert!(folder_allowed(&allowed, "Notes/Work"));
        assert!(folder_allowed(&allowed, "Notes/Work/Projects/ATLAS"));
        // gotcha #1's sibling trap: bare prefix would leak into Notes/WorkX
        assert!(!folder_allowed(&allowed, "Notes/WorkX"));
        assert!(!folder_allowed(&allowed, "Notes"));
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        assert!(!folder_allowed(&[], "Notes/Anything"));
    }

    #[test]
    fn missing_file_is_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        match load_write_scope_from(&dir.path().join("nope.json")) {
            Err(ScopeError::NotConfigured) => {}
            other => panic!("expected NotConfigured, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn bad_json_is_unparseable_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp_write_scope.json");
        std::fs::write(&p, "{ not json").unwrap();
        assert!(matches!(load_write_scope_from(&p), Err(ScopeError::Unparseable(_))));
    }

    #[test]
    fn parses_the_spec_shape() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp_write_scope.json");
        std::fs::write(&p, r#"{"accounts":{"a@x.com":{"allowed_folders":["Notes/__Claude__"]}}}"#).unwrap();
        let s = load_write_scope_from(&p).unwrap();
        assert_eq!(s.accounts["a@x.com"].allowed_folders, vec!["Notes/__Claude__"]);
    }
}
