//! Parse a view note's property block into a typed [`ViewQuery`] (spec §8.2–8.3).
//!
//! Date words stay symbolic here; the engine resolves them against `today`.
//! Every unreadable line is reported — a clause is never dropped silently.

use crate::folder_scope::Mode;
use crate::props::{parse_iso_date, Prop};
use chrono::NaiveDate;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    Notes,
    Tasks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Due,
    Modified,
    Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
}

/// What a comparison is made against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Today,
    Date(NaiveDate),
}

/// A date condition; `Today`-relative parts are resolved at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateSpec {
    Today,
    /// ISO week, Monday through Sunday.
    ThisWeek,
    /// Today through today + N days.
    NextDays(u32),
    /// Today - N days through today.
    LastDays(u32),
    Cmp(Cmp, Anchor),
    /// An absolute date, matched exactly (`= 2026-12-01` or the bare date).
    On(NaiveDate),
    /// Has no date.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueTest {
    Exact(String),
    Date(DateSpec),
}

/// One property line: alternatives (comma = OR), each possibly negated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    pub key: String,
    pub any_of: Vec<(bool, ValueTest)>,
}

#[derive(Debug, Clone)]
pub struct ViewQuery {
    pub show: Show,
    /// (negated, full label path, mode)
    /// One group per `Folder:` line; groups AND, alternatives within a group OR.
    pub folders: Vec<Vec<(bool, String, Mode)>>,
    pub tags: Vec<Vec<(bool, String)>>,
    /// One entry per line; all must hold.
    pub done: Vec<bool>,
    /// One group per `Due:` line; groups AND, alternatives within a group OR.
    pub due: Vec<Vec<DateSpec>>,
    pub modified: Vec<DateSpec>,
    pub pinned: Vec<bool>,
    pub text: Vec<String>,
    pub props: Vec<Clause>,
    pub sort: Sort,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ViewError {
    /// `Prop.pos`: 0-based line within the property block.
    pub line: usize,
    pub key: String,
    pub message: String,
}

pub const DEFAULT_LIMIT: u32 = 200;

fn word(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Comma-separated alternatives; a leading `not ` negates one.
fn alternatives(value: &str) -> Vec<(bool, String)> {
    value
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(|a| match a.get(..4) {
            Some(p) if p.eq_ignore_ascii_case("not ") => (true, a[4..].trim().to_string()),
            _ => (false, a.to_string()),
        })
        .collect()
}

fn parse_anchor(s: &str) -> Option<Anchor> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("today") {
        Some(Anchor::Today)
    } else {
        parse_iso_date(s).map(Anchor::Date)
    }
}

/// The shared date grammar of `Due`, `Modified` and date-valued properties.
fn parse_date_spec(raw: &str) -> Option<DateSpec> {
    let v = raw.trim();
    for (op, cmp) in [
        ("<=", Cmp::Le),
        (">=", Cmp::Ge),
        ("<", Cmp::Lt),
        (">", Cmp::Gt),
    ] {
        if let Some(rest) = v.strip_prefix(op) {
            return parse_anchor(rest).map(|a| DateSpec::Cmp(cmp, a));
        }
    }
    if let Some(rest) = v.strip_prefix('=') {
        return parse_iso_date(rest.trim()).map(DateSpec::On);
    }
    if let Some(d) = parse_iso_date(v) {
        return Some(DateSpec::On(d));
    }
    let w = word(v);
    match w.as_str() {
        "today" => return Some(DateSpec::Today),
        "this week" => return Some(DateSpec::ThisWeek),
        "none" => return Some(DateSpec::None),
        _ => {}
    }
    let parts: Vec<&str> = w.split(' ').collect();
    if let [dir, n, unit] = parts[..] {
        if unit == "days" || unit == "day" {
            let n: u32 = n.parse().ok()?;
            return match dir {
                "next" => Some(DateSpec::NextDays(n)),
                "last" => Some(DateSpec::LastDays(n)),
                _ => None,
            };
        }
    }
    None
}

/// Does the value look like it was meant as a date test (so a failure to read
/// it is an error rather than an exact-text match)?
fn looks_like_date_test(v: &str) -> bool {
    let v = v.trim();
    v.starts_with(['<', '>', '=']) || parse_date_spec(v).is_some()
}

fn parse_yes_no(v: &str) -> Option<bool> {
    match word(v).as_str() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

fn full_folder(value: &str) -> Option<(String, Mode)> {
    let (path, mode) = match value.strip_suffix("/*") {
        Some(p) => (p, Mode::Subtree),
        None => (value, Mode::Exact),
    };
    let path = path.trim().trim_end_matches('/');
    if path.is_empty() {
        return None;
    }
    let full = if path == "Notes" || path.starts_with("Notes/") {
        path.to_string()
    } else {
        format!("Notes/{path}")
    };
    Some((full, mode))
}

pub fn parse_view(props: &[Prop]) -> Result<ViewQuery, Vec<ViewError>> {
    let mut errors: Vec<ViewError> = Vec::new();
    let err = |errors: &mut Vec<ViewError>, p: &Prop, m: String| {
        errors.push(ViewError {
            line: p.pos,
            key: p.key.clone(),
            message: m,
        })
    };

    // `Show` first: `Due` and `Done` mean different things under each.
    let mut show = Show::Notes;
    let mut seen_show = false;
    for p in props.iter().filter(|p| p.key == "show") {
        if seen_show {
            err(&mut errors, p, "`Show:` is given twice".into());
            continue;
        }
        seen_show = true;
        match word(&p.value).as_str() {
            "notes" => show = Show::Notes,
            "tasks" => show = Show::Tasks,
            _ => err(
                &mut errors,
                p,
                format!("`Show:` must be notes or tasks, not \"{}\"", p.value),
            ),
        }
    }

    let mut q = ViewQuery {
        show,
        folders: vec![],
        tags: vec![],
        done: vec![],
        due: vec![],
        modified: vec![],
        pinned: vec![],
        text: vec![],
        props: vec![],
        sort: if show == Show::Tasks {
            Sort::Due
        } else {
            Sort::Modified
        },
        limit: DEFAULT_LIMIT,
    };
    let mut conditions = 0usize;
    let mut seen: Vec<&str> = vec![];

    for p in props {
        let key = p.key.as_str();
        if matches!(key, "sort" | "limit") {
            if seen.contains(&key) {
                err(&mut errors, p, format!("`{}:` is given twice", p.key_display));
                continue;
            }
            seen.push(key);
        }
        match key {
            "show" => {}
            "sort" => match word(&p.value).as_str() {
                "due" => q.sort = Sort::Due,
                "modified" => q.sort = Sort::Modified,
                "title" => q.sort = Sort::Title,
                _ => err(
                    &mut errors,
                    p,
                    format!("`Sort:` must be due, modified or title, not \"{}\"", p.value),
                ),
            },
            "limit" => match p.value.trim().parse::<u32>() {
                Ok(n) if n > 0 => q.limit = n,
                _ => err(
                    &mut errors,
                    p,
                    format!("`Limit:` must be a number above 0, not \"{}\"", p.value),
                ),
            },
            "folder" => {
                conditions += 1;
                let alts = alternatives(&p.value);
                if alts.is_empty() {
                    err(&mut errors, p, "`Folder:` needs a folder".into());
                }
                let mut group = Vec::new();
                for (neg, v) in alts {
                    match full_folder(&v) {
                        Some((path, mode)) => group.push((neg, path, mode)),
                        None => err(&mut errors, p, format!("`Folder:` cannot read \"{v}\"")),
                    }
                }
                if !group.is_empty() {
                    q.folders.push(group);
                }
            }
            "tag" => {
                conditions += 1;
                let alts = alternatives(&p.value);
                if alts.is_empty() {
                    err(&mut errors, p, "`Tag:` needs a tag".into());
                }
                if !alts.is_empty() {
                    q.tags.push(alts);
                }
            }
            "pinned" => {
                conditions += 1;
                match parse_yes_no(&p.value) {
                    Some(b) => q.pinned.push(b),
                    None => err(
                        &mut errors,
                        p,
                        format!("`Pinned:` must be yes or no, not \"{}\"", p.value),
                    ),
                }
            }
            "text" => {
                conditions += 1;
                if p.value.trim().is_empty() {
                    err(&mut errors, p, "`Text:` needs some words".into());
                } else {
                    q.text.push(p.value.trim().to_string());
                }
            }
            "modified" => {
                conditions += 1;
                match parse_date_spec(&p.value) {
                    Some(DateSpec::None) | None => err(
                        &mut errors,
                        p,
                        format!(
                            "`Modified:` cannot read \"{}\" (try today, last 7 days, >= 2026-12-01)",
                            p.value
                        ),
                    ),
                    Some(s) => q.modified.push(s),
                }
            }
            "done" if show == Show::Tasks => {
                conditions += 1;
                match parse_yes_no(&p.value) {
                    Some(b) => q.done.push(b),
                    None => err(
                        &mut errors,
                        p,
                        format!("`Done:` must be yes or no, not \"{}\"", p.value),
                    ),
                }
            }
            "due" if show == Show::Tasks => {
                conditions += 1;
                let specs: Option<Vec<DateSpec>> = p
                    .value
                    .split(',')
                    .map(|a| parse_date_spec(a))
                    .collect();
                match specs {
                    Some(g) if !g.is_empty() => q.due.push(g),
                    _ => err(
                        &mut errors,
                        p,
                        format!(
                            "`Due:` cannot read \"{}\" (try today, <= today, this week, next 7 days, >= 2026-12-01, none)",
                            p.value
                        ),
                    ),
                }
            }
            _ => {
                conditions += 1;
                let alts = alternatives(&p.value);
                if alts.is_empty() {
                    err(
                        &mut errors,
                        p,
                        format!("`{}:` has no value to match", p.key_display),
                    );
                    continue;
                }
                let mut any_of = Vec::new();
                for (neg, v) in alts {
                    if looks_like_date_test(&v) {
                        match parse_date_spec(&v) {
                            Some(s) => any_of.push((neg, ValueTest::Date(s))),
                            None => err(
                                &mut errors,
                                p,
                                format!("`{}:` cannot read the date test \"{v}\"", p.key_display),
                            ),
                        }
                    } else {
                        any_of.push((neg, ValueTest::Exact(v)));
                    }
                }
                q.props.push(Clause {
                    key: p.key.clone(),
                    any_of,
                });
            }
        }
    }

    if conditions == 0 && errors.is_empty() {
        errors.push(ViewError {
            line: 0,
            key: String::new(),
            message: "this view has no conditions".into(),
        });
    }
    if errors.is_empty() {
        Ok(q)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::props::parse_property_block;

    fn parse(lines: &[&str]) -> Result<ViewQuery, Vec<ViewError>> {
        parse_view(&parse_property_block(lines))
    }

    #[test]
    fn show_defaults_to_notes() {
        let q = parse(&["Pinned: yes"]).unwrap();
        assert_eq!(q.show, Show::Notes);
        assert_eq!(q.sort, Sort::Modified);
    }

    #[test]
    fn show_tasks() {
        let q = parse(&["Show: tasks", "Done: no"]).unwrap();
        assert_eq!(q.show, Show::Tasks);
        assert_eq!(q.sort, Sort::Due);
        assert_eq!(q.done, vec![false]);
    }

    #[test]
    fn folder_star_is_subtree() {
        let q = parse(&["Folder: Projects/*, Inbox, not Notes/Old, Notes/*"]).unwrap();
        assert_eq!(q.folders.len(), 1);
        let f: Vec<_> = q.folders[0]
            .iter()
            .map(|(n, p, m)| (*n, p.as_str(), matches!(m, Mode::Subtree)))
            .collect();
        assert_eq!(
            f,
            vec![
                (false, "Notes/Projects", true),
                (false, "Notes/Inbox", false),
                (true, "Notes/Old", false),
                (false, "Notes", true),
            ]
        );
        let q = parse(&["Folder: Notes"]).unwrap();
        assert_eq!(q.folders[0][0].1, "Notes");
        assert!(matches!(q.folders[0][0].2, Mode::Exact));
    }

    #[test]
    fn comma_is_or_and_not_negates() {
        let q = parse(&["Tag: meeting, not someday"]).unwrap();
        assert_eq!(
            q.tags,
            vec![vec![(false, "meeting".to_string()), (true, "someday".to_string())]]
        );
    }

    #[test]
    fn due_is_task_filter_under_tasks() {
        let q = parse(&["Show: tasks", "Due: <= today"]).unwrap();
        assert_eq!(q.due, vec![vec![DateSpec::Cmp(Cmp::Le, Anchor::Today)]]);
        assert!(q.props.is_empty());
    }

    #[test]
    fn due_is_property_under_notes() {
        let q = parse(&["Show: notes", "Due: <= today"]).unwrap();
        assert!(q.due.is_empty());
        assert_eq!(q.props.len(), 1);
        assert_eq!(q.props[0].key, "due");
        assert_eq!(
            q.props[0].any_of,
            vec![(false, ValueTest::Date(DateSpec::Cmp(Cmp::Le, Anchor::Today)))]
        );
    }

    #[test]
    fn custom_key_date_ops() {
        let q = parse(&["Next contact: <= today", "Status: active, not done"]).unwrap();
        assert_eq!(q.props[0].key, "next contact");
        assert!(matches!(q.props[0].any_of[0].1, ValueTest::Date(_)));
        assert_eq!(
            q.props[1].any_of,
            vec![
                (false, ValueTest::Exact("active".into())),
                (true, ValueTest::Exact("done".into()))
            ]
        );
    }

    #[test]
    fn date_words() {
        let d = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        let cases = [
            ("today", DateSpec::Today),
            ("this week", DateSpec::ThisWeek),
            ("next 7 days", DateSpec::NextDays(7)),
            ("last 7 days", DateSpec::LastDays(7)),
            ("none", DateSpec::None),
            ("< today", DateSpec::Cmp(Cmp::Lt, Anchor::Today)),
            (">= 2026-12-01", DateSpec::Cmp(Cmp::Ge, Anchor::Date(d))),
        ];
        for (v, want) in cases {
            let line = format!("Due: {v}");
            let q = parse(&["Show: tasks", line.as_str()]).unwrap();
            assert_eq!(q.due, vec![vec![want]], "{v}");
        }
    }

    #[test]
    fn bare_date_under_due_means_that_day() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        let q = parse(&["Show: tasks", "Due: 2026-11-25"]).unwrap();
        assert_eq!(q.due, vec![vec![DateSpec::On(d)]]);
        let q = parse(&["Show: tasks", "Due: 2026-11-25, today"]).unwrap();
        assert_eq!(q.due, vec![vec![DateSpec::On(d), DateSpec::Today]]);
    }

    /// Ruling R10: `Modified:` and date-valued properties share `Due:`'s date
    /// grammar, so a bare ISO date there also means that day — a date test,
    /// not exact text.
    #[test]
    fn bare_date_under_modified_and_date_properties_means_that_day() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        let q = parse(&["Modified: 2026-11-25"]).unwrap();
        assert_eq!(q.modified, vec![DateSpec::On(d)]);
        let q = parse(&["Next contact: 2026-11-25"]).unwrap();
        assert_eq!(q.props.len(), 1);
        assert_eq!(q.props[0].key, "next contact");
        assert_eq!(q.props[0].any_of, vec![(false, ValueTest::Date(DateSpec::On(d)))]);
    }

    #[test]
    fn unreadable_clause_is_error() {
        let e = parse(&["Show: tasks", "Due: soon"]).unwrap_err();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].key, "due");
        assert_eq!(e[0].line, 1);
        let e = parse(&["Next contact: <= someday"]).unwrap_err();
        assert_eq!(e[0].key, "next contact");
    }

    #[test]
    fn no_conditions_is_error() {
        let e = parse(&["Show: tasks"]).unwrap_err();
        assert_eq!(e[0].message, "this view has no conditions");
        assert!(parse(&["Sort: title", "Limit: 5"]).is_err());
    }

    #[test]
    fn bad_show_is_error() {
        let e = parse(&["Show: everything", "Pinned: yes"]).unwrap_err();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].key, "show");
        assert_eq!(e[0].line, 0);
    }

    #[test]
    fn bad_structural_values_collect_all_errors() {
        let e = parse(&["Pinned: maybe", "Show: tasks", "Done: perhaps", "Sort: size"]).unwrap_err();
        let keys: Vec<_> = e.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(keys, vec!["pinned", "done", "sort"]);
    }

    #[test]
    fn repeated_setting_is_error() {
        assert!(parse(&["Pinned: yes", "Sort: due", "Sort: title"]).is_err());
        assert!(parse(&["Pinned: yes", "Limit: 1", "Limit: 2"]).is_err());
        assert!(parse(&["Pinned: yes", "Show: tasks", "Show: notes"]).is_err());
    }

    #[test]
    fn repeated_lines_and_together() {
        let q = parse(&["Folder: Projects/*", "Folder: not Projects/Archive/*"]).unwrap();
        assert_eq!(q.folders.len(), 2);
        assert_eq!(q.folders[1][0].1, "Notes/Projects/Archive");
        assert!(q.folders[1][0].0);
        let d1 = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 11, 30).unwrap();
        let q = parse(&["Show: tasks", "Due: >= 2026-11-01", "Due: <= 2026-11-30"]).unwrap();
        assert_eq!(
            q.due,
            vec![
                vec![DateSpec::Cmp(Cmp::Ge, Anchor::Date(d1))],
                vec![DateSpec::Cmp(Cmp::Le, Anchor::Date(d2))]
            ]
        );
        let q = parse(&["Tag: meeting", "Tag: not someday"]).unwrap();
        assert_eq!(
            q.tags,
            vec![vec![(false, "meeting".to_string())], vec![(true, "someday".to_string())]]
        );
    }

    #[test]
    fn limit_default_200() {
        assert_eq!(parse(&["Pinned: yes"]).unwrap().limit, 200);
    }

    #[test]
    fn limit_parses() {
        assert_eq!(parse(&["Pinned: yes", "Limit: 25"]).unwrap().limit, 25);
        assert!(parse(&["Pinned: yes", "Limit: lots"]).is_err());
        assert!(parse(&["Pinned: yes", "Limit: 0"]).is_err());
    }
}
