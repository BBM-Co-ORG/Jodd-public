//! Literal, case-sensitive folder paths. No trimming, Unicode normalization,
//! dot-segment resolution or trailing-slash removal at the authorization boundary.
#[derive(Clone, Copy)]
pub enum Mode {
    Exact,
    Subtree,
}
pub fn matches(label: &str, scope: &str, mode: Mode) -> bool {
    label == scope
        || matches!(mode, Mode::Subtree)
            && label
                .strip_prefix(scope)
                .is_some_and(|s| s.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_scope_conformance() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/folder-scope.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let (scope, label) = (
                case["scope"].as_str().unwrap(),
                case["label"].as_str().unwrap(),
            );
            assert_eq!(
                matches(label, scope, Mode::Exact),
                case["exact"].as_bool().unwrap(),
                "{case}"
            );
            assert_eq!(
                matches(label, scope, Mode::Subtree),
                case["subtree"].as_bool().unwrap(),
                "{case}"
            );
            let dir = tempfile::tempdir().unwrap();
            let db = crate::db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
            crate::test_support::note("a", "n")
                .label(label)
                .body("needle <input type=\"checkbox\">")
                .insert(&db);
            let expected = usize::from(case["subtree"].as_bool().unwrap());
            assert_eq!(
                db.list_notes_in_subtree("a", scope).unwrap().len(),
                expected,
                "{case}"
            );
            assert_eq!(
                db.list_notes_with_checkboxes("a", Some(scope))
                    .unwrap()
                    .len(),
                expected,
                "{case}"
            );
            assert_eq!(
                db.count_notes_in_scope(Some("a"), Some(scope), &[])
                    .unwrap(),
                expected,
                "{case}"
            );
            assert_eq!(
                db.search_notes(Some("a"), Some(scope), "needle", &[])
                    .unwrap()
                    .len(),
                usize::from(case["exact"].as_bool().unwrap()),
                "exact search: {case}"
            );
            assert_eq!(
                db.list_notes_by_label("a", scope).unwrap().len(),
                usize::from(case["exact"].as_bool().unwrap()),
                "{case}"
            );
        }
    }
    #[test]
    fn database_subtree_conforms_to_literal_matching() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let labels = [
            "Notes",
            "Notes/A",
            "Notes/A/x",
            "Notes/AB",
            "Notes/a/x",
            "Notes/%/x",
            "Notes/_/x",
            "Notes/ไทย/子",
            "Notes/ไทย",
            "Notes/A/",
            "Notes/A//x",
            "",
            "/x",
            "Notes/é",
            "Notes/e\u{301}",
            "Notes/\\/x",
        ];
        for (i, label) in labels.iter().enumerate() {
            crate::test_support::note("a", &i.to_string())
                .label(label)
                .body("<input type=\"checkbox\">")
                .insert(&db);
        }
        for scope in labels {
            let expected = labels
                .iter()
                .filter(|l| matches(l, scope, Mode::Subtree))
                .count();
            assert_eq!(
                db.list_notes_in_subtree("a", scope).unwrap().len(),
                expected,
                "list {scope}"
            );
            assert_eq!(
                db.count_notes_in_scope(Some("a"), Some(scope), &[])
                    .unwrap(),
                expected,
                "count {scope}"
            );
            assert_eq!(
                db.list_notes_with_checkboxes("a", Some(scope))
                    .unwrap()
                    .len(),
                expected,
                "tasks {scope}"
            );
        }
    }
}
