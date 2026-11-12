//! The YAML frontmatter block at the top of every SSH note file.
//!
//! Deliberately not a YAML parser: Jodd owns four flat keys, and every other
//! line is kept verbatim in `extra` so a program on the server that stores
//! its own metadata here keeps it across a Jodd save.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frontmatter {
    pub uuid: Option<String>,
    pub title: Option<String>,
    pub created: Option<String>,
    pub pinned: bool,
    pub extra: Vec<String>,
}

/// `None` when the file does not open with a `---` line or never closes the
/// block. Such a file is adopted (spec §2), not rejected.
pub fn split(text: &str) -> (Option<Frontmatter>, &str) {
    let Some(first_end) = text.find('\n') else { return (None, text) };
    if text[..first_end].trim_end_matches('\r') != "---" {
        return (None, text);
    }
    let start = first_end + 1;
    let mut consumed = start;
    let mut fm = Frontmatter::default();
    for raw in text[start..].split_inclusive('\n') {
        consumed += raw.len();
        let line = raw.trim_end_matches('\n').trim_end_matches('\r');
        if line == "---" {
            return (Some(fm), &text[consumed..]);
        }
        apply_line(&mut fm, line);
    }
    (None, text)
}

fn apply_line(fm: &mut Frontmatter, line: &str) {
    let Some((key, value)) = line.split_once(':') else {
        fm.extra.push(line.to_string());
        return;
    };
    let value = unquote(value);
    match key.trim() {
        "uuid" => fm.uuid = Some(value).filter(|v| !v.is_empty()),
        "title" => fm.title = Some(value),
        "created" => fm.created = Some(value).filter(|v| !v.is_empty()),
        "pinned" => fm.pinned = value.eq_ignore_ascii_case("true"),
        _ => fm.extra.push(line.to_string()),
    }
}

fn unquote(raw: &str) -> String {
    let v = raw.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let mut out = String::new();
        let mut chars = v[1..v.len() - 1].chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    v.to_string()
}

fn quote(s: &str) -> String {
    let flat = s.replace(['\n', '\r'], " ");
    format!("\"{}\"", flat.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn render(fm: &Frontmatter) -> String {
    let mut out = String::from("---\n");
    if let Some(u) = &fm.uuid {
        out.push_str(&format!("uuid: {u}\n"));
    }
    if let Some(t) = &fm.title {
        out.push_str(&format!("title: {}\n", quote(t)));
    }
    if let Some(c) = &fm.created {
        out.push_str(&format!("created: {c}\n"));
    }
    out.push_str(&format!("pinned: {}\n", fm.pinned));
    for line in &fm.extra {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("---\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jodd_file_splits_into_fields_and_body() {
        let text = "---\nuuid: 5f3a\ntitle: \"Meeting: \\\"Q3\\\"\"\ncreated: 2026-09-26T10:00:00+07:00\npinned: true\n---\n# Meeting\nbody\n";
        let (fm, rest) = split(text);
        let fm = fm.unwrap();
        assert_eq!(fm.uuid.as_deref(), Some("5f3a"));
        assert_eq!(fm.title.as_deref(), Some("Meeting: \"Q3\""));
        assert_eq!(fm.created.as_deref(), Some("2026-09-26T10:00:00+07:00"));
        assert!(fm.pinned);
        assert_eq!(rest, "# Meeting\nbody\n");
    }

    #[test]
    fn no_opening_line_or_no_closing_line_means_no_frontmatter() {
        assert_eq!(split("# Just a note\n"), (None, "# Just a note\n"));
        assert_eq!(split("---\nuuid: x\nnever closed\n"), (None, "---\nuuid: x\nnever closed\n"));
        assert_eq!(split(""), (None, ""));
    }

    #[test]
    fn crlf_files_split_too() {
        let (fm, rest) = split("---\r\nuuid: a\r\n---\r\nbody");
        assert_eq!(fm.unwrap().uuid.as_deref(), Some("a"));
        assert_eq!(rest, "body");
    }

    /// A program on the server may keep its own keys here; a Jodd save must
    /// not delete them.
    #[test]
    fn unknown_keys_survive_a_render() {
        let (fm, _) = split("---\nuuid: a\nsource: cron-job\ntags: [x, y]\n---\n");
        let mut fm = fm.unwrap();
        fm.title = Some("T".into());
        let again = split(&render(&fm)).0.unwrap();
        assert_eq!(again.extra, vec!["source: cron-job", "tags: [x, y]"]);
        assert_eq!(again, fm);
    }

    #[test]
    fn render_quotes_titles_and_flattens_newlines() {
        let fm = Frontmatter { uuid: Some("u".into()), title: Some("a \\ \"b\"\nc".into()), created: None, pinned: false, extra: vec![] };
        let text = render(&fm);
        assert_eq!(text, "---\nuuid: u\ntitle: \"a \\\\ \\\"b\\\" c\"\npinned: false\n---\n");
        assert_eq!(split(&text).0.unwrap().title.as_deref(), Some("a \\ \"b\" c"));
    }

    #[test]
    fn single_quoted_values_are_yaml_single_quoted() {
        let (fm, _) = split("---\ntitle: 'it''s'\npinned: 'TRUE'\n---\n");
        let fm = fm.unwrap();
        assert_eq!(fm.title.as_deref(), Some("it's"));
        assert!(fm.pinned);
    }
}
