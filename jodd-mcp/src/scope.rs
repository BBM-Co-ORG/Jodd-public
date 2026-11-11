//! `mcp_write_scope.json` is modelled once, in `jodd_lib::mcp_scope`, because
//! the app now writes it too (agent workspace, hidden folders). This module
//! keeps the names the tools already use.

use std::path::Path;

pub use jodd_lib::mcp_scope::{scope_path, ScopeError, ScopeFile as WriteScope};

pub fn load_write_scope_from(path: &Path) -> Result<WriteScope, ScopeError> {
    jodd_lib::mcp_scope::load_from(path)
}

/// Recursive-subtree match, gotcha #1's shape: exact OR "{allowed}/" prefix.
/// The '/' in the prefix is load-bearing — bare starts_with would leak
/// Notes/Work into Notes/WorkX.
pub fn folder_allowed(allowed: &[String], label: &str) -> bool {
    allowed
        .iter()
        .any(|a| jodd_lib::folder_scope::matches(label, a, jodd_lib::folder_scope::Mode::Subtree))
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

    /// After migration #19 every `Account.id` is `{backend}:{email}` while
    /// every key in an existing, hand-written `mcp_write_scope.json` is still
    /// a bare email — and nothing migrates that file. Without the fallback the
    /// lookup misses, the allowlist reads as empty, and `folder_allowed`
    /// denies everything: every MCP write tool refuses, silently, against a
    /// file that still says exactly what the user meant.
    #[test]
    fn a_bare_email_key_still_grants_a_qualified_account() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp_write_scope.json");
        std::fs::write(&p, r#"{"accounts":{"a@x.com":{"allowed_folders":["Notes/__Claude__"]}}}"#).unwrap();
        let s = load_write_scope_from(&p).unwrap();

        assert_eq!(s.allowed_folders("gmail:a@x.com"), ["Notes/__Claude__".to_string()]);
        assert!(folder_allowed(s.allowed_folders("gmail:a@x.com"), "Notes/__Claude__/Sub"));
        // Not a blanket "any account matches any key": the bare form has to be
        // this account's own.
        assert!(s.allowed_folders("gmail:someone-else@x.com").is_empty());
        // …and an unknown prefix is not stripped at all.
        assert!(s.allowed_folders("imap:a@x.com").is_empty());
    }

    /// A file that HAS been re-keyed must not be second-guessed by a stale
    /// bare entry sitting beside it.
    #[test]
    fn the_qualified_key_wins_when_both_are_present() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp_write_scope.json");
        std::fs::write(
            &p,
            r#"{"accounts":{"a@x.com":{"allowed_folders":["Notes/Old"]},
                            "gmail:a@x.com":{"allowed_folders":["Notes/New"]}}}"#,
        )
        .unwrap();
        let s = load_write_scope_from(&p).unwrap();
        assert_eq!(s.allowed_folders("gmail:a@x.com"), ["Notes/New".to_string()]);
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

#[cfg(test)]
mod conformance {
    #[test]
    fn shared_literal_scope_fixture_and_deny_by_default() {
        let cases: serde_json::Value = serde_json::from_str(include_str!("../../tests/fixtures/folder-scope.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let label = case["label"].as_str().unwrap();
            let allowed = vec![case["scope"].as_str().unwrap().to_owned()];
            assert_eq!(super::folder_allowed(&allowed,label),case["subtree"].as_bool().unwrap(),"{case}");
            assert!(!super::folder_allowed(&[],label));
        }
    }
}
