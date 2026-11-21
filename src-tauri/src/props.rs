//! Property-block and due-date grammar (spec 2026-11-19-properties-tasks-views §5.1, §6.2).
//! The TypeScript mirror reads the same fixtures in `tests/fixtures/`.

use chrono::NaiveDate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prop {
    /// 0-based index of the line within the property block.
    pub pos: usize,
    /// Lower-cased key.
    pub key: String,
    pub key_display: String,
    pub value: String,
    pub value_date: Option<NaiveDate>,
}

const MAX_KEY_CHARS: usize = 32;

fn norm(s: &str) -> String {
    s.replace('\u{00A0}', " ")
}

/// Thai vowel/tone marks are not all `alphanumeric`; a key may carry them.
fn is_key_mark(c: char) -> bool {
    matches!(c as u32, 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E)
}

/// Strict `YYYY-MM-DD`.
pub fn parse_iso_date(s: &str) -> Option<NaiveDate> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if !b
        .iter()
        .enumerate()
        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// `Key: value` -> (display key, trimmed value).
pub fn parse_property_line(line: &str) -> Option<(String, String)> {
    let line = norm(line);
    let colon = line.find(':')?;
    let key = &line[..colon];
    let rest = &line[colon + 1..];
    let mut chars = key.chars();
    if !chars.next()?.is_alphabetic() {
        return None;
    }
    if key.chars().count() > MAX_KEY_CHARS || key.ends_with(' ') {
        return None;
    }
    if !key
        .chars()
        .all(|c| c.is_alphanumeric() || is_key_mark(c) || matches!(c, ' ' | '-' | '_'))
    {
        return None;
    }
    if !rest.starts_with(' ') {
        return None;
    }
    let value = rest.trim();
    if value.is_empty() {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

/// The run of property lines at the head of `lines_after_title`.
pub fn parse_property_block(lines_after_title: &[&str]) -> Vec<Prop> {
    let mut out = Vec::new();
    for (pos, line) in lines_after_title.iter().enumerate() {
        let Some((key_display, value)) = parse_property_line(line) else {
            break;
        };
        out.push(Prop {
            pos,
            key: key_display.to_lowercase(),
            value_date: parse_iso_date(&value),
            key_display,
            value,
        });
    }
    out
}

/// The due date in a task's text: `📅 YYYY-MM-DD` or `due: YYYY-MM-DD`; the last wins.
pub fn parse_due(task_text: &str) -> Option<NaiveDate> {
    let text = norm(task_text);
    // ASCII-only lowercasing keeps byte offsets identical to `text`.
    let lower = text.to_ascii_lowercase();
    let mut found: Option<(usize, NaiveDate)> = None;
    let mut consider = |at: usize, after: usize, text: &str| {
        let tail = text[after..].trim_start_matches([' ', '\t']);
        let digit_follows = tail.as_bytes().get(10).is_some_and(u8::is_ascii_digit);
        if let Some(d) = tail.get(..10).and_then(parse_iso_date).filter(|_| !digit_follows) {
            if found.map_or(true, |(p, _)| at > p) {
                found = Some((at, d));
            }
        }
    };
    for (at, _) in text.match_indices('\u{1F4C5}') {
        let mut after = at + '\u{1F4C5}'.len_utf8();
        if text[after..].starts_with('\u{FE0F}') {
            after += '\u{FE0F}'.len_utf8();
        }
        consider(at, after, &text);
    }
    for (at, _) in lower.match_indices("due:") {
        let boundary = text[..at]
            .chars()
            .next_back()
            .map_or(true, |c| !c.is_alphanumeric());
        if boundary {
            consider(at, at + 4, &text);
        }
    }
    found.map(|(_, d)| d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(v: &serde_json::Value) -> Option<NaiveDate> {
        v.as_str().map(|s| parse_iso_date(s).expect(s))
    }

    #[test]
    fn property_block_conformance() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/property-block.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let lines: Vec<&str> = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap())
                .collect();
            let got = parse_property_block(&lines);
            let want = case["props"].as_array().unwrap();
            assert_eq!(got.len(), want.len(), "{case}");
            for (i, (g, w)) in got.iter().zip(want).enumerate() {
                assert_eq!(g.pos, i, "{case}");
                assert_eq!(g.key, w["key"].as_str().unwrap(), "{case}");
                assert_eq!(g.key_display, w["key_display"].as_str().unwrap(), "{case}");
                assert_eq!(g.value, w["value"].as_str().unwrap(), "{case}");
                assert_eq!(g.value_date, date(&w["value_date"]), "{case}");
            }
        }
    }

    #[test]
    fn due_date_conformance() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/due-dates.json")).unwrap();
        for case in cases.as_array().unwrap() {
            assert_eq!(
                parse_due(case["text"].as_str().unwrap()),
                date(&case["due"]),
                "{case}"
            );
        }
    }
}
