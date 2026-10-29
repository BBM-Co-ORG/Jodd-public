//! OKF v0.2 concept documents: one note = one `.md` file with a YAML
//! frontmatter block. Everything Jodd-specific sits under a single `jodd:` key
//! so the top level stays plain OKF and other tools see a tidy concept.

use super::{JoddMeta, NoteDoc};
use serde::Serialize;
use serde_yaml_ng::Value;

/// Wire form of an `{by, at}` attestation (OKF §5.2).
#[derive(Serialize)]
struct Attestation {
    by: String,
    at: String,
}

#[derive(Serialize)]
struct Source {
    id: String,
    resource: String,
}

/// The frontmatter as written. Field order here IS the key order in the file:
/// `type` first, then the OKF keys, then `jodd`. Anything else in a foreign
/// file is ignored on read (serde skips unknown keys).
#[derive(Serialize)]
struct FrontmatterOut<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "str::is_empty")]
    title: &'a str,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    tags: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    generated: Option<Attestation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    verified: Vec<Attestation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sources: Vec<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    jodd: Option<&'a JoddMeta>,
}

fn attestation((by, at): &(String, String)) -> Attestation {
    Attestation { by: by.clone(), at: at.clone() }
}

/// A note as `---` frontmatter followed by its body, byte for byte.
pub fn render_concept(doc: &NoteDoc) -> Result<String, String> {
    let out = FrontmatterOut {
        kind: "Note",
        title: &doc.title,
        tags: &doc.tags,
        generated: doc.generated.as_ref().map(attestation),
        verified: doc.verified.iter().map(attestation).collect(),
        sources: doc
            .sources
            .iter()
            .enumerate()
            .map(|(i, resource)| Source { id: format!("s{}", i + 1), resource: resource.clone() })
            .collect(),
        jodd: doc.jodd.as_ref(),
    };
    let yaml = serde_yaml_ng::to_string(&out).map_err(|e| format!("frontmatter: {e}"))?;
    Ok(format!("---\n{yaml}---\n{}", doc.body))
}

/// Split `---\n<yaml>---\n<body>` into its two parts. The closing marker is
/// the first line that is exactly `---`.
fn split_frontmatter(text: &str) -> Result<(&str, &str), String> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or("no frontmatter: the file does not start with ---")?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Ok((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err("no frontmatter: the closing --- is missing".into())
}

/// A scalar read as text; anything structured (or null) is not text.
fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `tags`: a list of scalars, or a single scalar read as a one-tag list.
/// Null, a map, or anything else yields no tags; bad items are skipped.
fn lenient_tags(v: &Value) -> Vec<String> {
    match v {
        Value::Sequence(items) => items.iter().filter_map(scalar_text).collect(),
        other => scalar_text(other).into_iter().collect(),
    }
}

/// An `{by, at}` mapping. A mapping with neither key is not an attestation.
fn lenient_attestation(v: &Value) -> Option<(String, String)> {
    let map = v.as_mapping()?;
    let get = |k: &str| map.get(k).and_then(scalar_text).unwrap_or_default();
    let (by, at) = (get("by"), get("at"));
    if by.is_empty() && at.is_empty() {
        None
    } else {
        Some((by, at))
    }
}

/// `verified`: a list of attestations, or (OKF §11) a bare mapping as a list of one.
fn lenient_verified(v: &Value) -> Vec<(String, String)> {
    match v {
        Value::Sequence(items) => items.iter().filter_map(lenient_attestation).collect(),
        other => lenient_attestation(other).into_iter().collect(),
    }
}

/// `sources`: entries with a `resource`; a bare string entry is taken as the resource.
fn lenient_sources(v: &Value) -> Vec<String> {
    let one = |e: &Value| match e {
        Value::Mapping(m) => m.get("resource").and_then(scalar_text),
        other => scalar_text(other),
    };
    let found: Vec<String> = match v {
        Value::Sequence(items) => items.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    };
    found.into_iter().filter(|r| !r.is_empty()).collect()
}

/// Read a concept document. OKF §11 forbids rejecting a concept over its
/// optional fields, so every OKF-owned key is read on its own and falls back
/// to empty when it is null or the wrong shape (a scalar `tags: foo` becomes
/// `["foo"]`). Unknown keys are ignored, a bare `verified` map is a list of
/// one, a missing `title` is `""` (the caller falls back to the file stem),
/// and no `jodd:` gives `None`. `jodd:` itself is Jodd's own schema and stays
/// strict. A file with no frontmatter block, or whose frontmatter is not a
/// YAML mapping, is an error.
pub fn parse_concept(text: &str) -> Result<NoteDoc, String> {
    let (yaml, body) = split_frontmatter(text)?;
    let root: Value = if yaml.trim().is_empty() {
        Value::Mapping(Default::default())
    } else {
        serde_yaml_ng::from_str(yaml).map_err(|e| format!("frontmatter: {e}"))?
    };
    let map = match &root {
        Value::Mapping(m) => m,
        Value::Null => return parse_with(&Default::default(), body),
        _ => return Err("frontmatter is not a YAML mapping".into()),
    };
    parse_with(map, body)
}

fn parse_with(map: &serde_yaml_ng::Mapping, body: &str) -> Result<NoteDoc, String> {
    let jodd = match map.get("jodd") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            serde_yaml_ng::from_value::<JoddMeta>(v.clone()).map_err(|e| format!("jodd: {e}"))?,
        ),
    };
    Ok(NoteDoc {
        title: map.get("title").and_then(scalar_text).unwrap_or_default(),
        tags: map.get("tags").map(lenient_tags).unwrap_or_default(),
        sources: map.get("sources").map(lenient_sources).unwrap_or_default(),
        generated: map.get("generated").and_then(lenient_attestation),
        verified: map.get("verified").map(lenient_verified).unwrap_or_default(),
        jodd,
        body: body.to_string(),
    })
}

/// Collapse whitespace runs (newlines included) so text stays on one line.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A Markdown link target that survives spaces and parentheses.
fn link_target(path: &str) -> String {
    path.replace(' ', "%20").replace('(', "%28").replace(')', "%29")
}

/// An `index.md` (OKF §8). Only the bundle root carries frontmatter, and
/// only to declare `okf_version`. Entries are `(link path, title)`.
pub fn render_index(title: &str, root: bool, entries: &[(String, String)]) -> String {
    let mut out = String::new();
    if root {
        out.push_str("---\nokf_version: \"0.2\"\n---\n");
    }
    out.push_str(&format!("# {}\n", one_line(title)));
    if !entries.is_empty() {
        out.push('\n');
    }
    for (path, entry_title) in entries {
        let text = one_line(entry_title).replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]");
        out.push_str(&format!("* [{text}]({})\n", link_target(path)));
    }
    out
}

/// A `log.md` (OKF §9) with one entry. The heading is the date part of
/// `date_iso`.
pub fn render_log(date_iso: &str, line: &str) -> String {
    let date: String = date_iso.chars().take(10).collect();
    format!("# Directory Update Log\n\n## {date}\n* **Update**: {}\n", one_line(line))
}

#[cfg(test)]
mod tests {
    use super::super::{AttachmentRef, BodyFormat, HistoryEvent, JoddMeta, NoteDoc};
    use super::*;

    fn full_doc() -> NoteDoc {
        NoteDoc {
            title: "Meeting with Ann".into(),
            tags: vec!["project-x".into(), "followup".into()],
            sources: vec!["https://example.com/article".into(), "https://b.example/x".into()],
            generated: Some(("human:owner".into(), "2026-10-22T14:03:11+07:00".into())),
            verified: vec![
                ("human:owner".into(), "2026-10-22T15:00:00+07:00".into()),
                ("agent:claude".into(), "2026-10-23T09:00:00Z".into()),
            ],
            jodd: Some(JoddMeta {
                uuid: "3F2A0000-0000-4000-8000-000000000001".into(),
                account: "gmail:kaiwan@bbmedia.co.th".into(),
                backend: "gmail".into(),
                folder: "Notes/Work".into(),
                created: Some("2026-10-01T08:00:00+07:00".into()),
                modified: Some("2026-10-22T14:03:11+07:00".into()),
                pinned: true,
                body: BodyFormat::Markdown,
                html: None,
                content_sha256: "ab12".repeat(16),
                sync_state: "dirty".into(),
                attachments: vec![
                    AttachmentRef {
                        content_id: "abc@x".into(),
                        filename: Some("photo.jpg".into()),
                        mime: "image/jpeg".into(),
                        path: "/references/attachments/3F2A/abc@x-photo.jpg".into(),
                        x_apple_part_url: Some("cid:abc@x".into()),
                    },
                    AttachmentRef {
                        content_id: "def@x".into(),
                        filename: None,
                        mime: "application/pdf".into(),
                        path: "/references/attachments/3F2A/def@x".into(),
                        x_apple_part_url: None,
                    },
                ],
                history: vec![
                    HistoryEvent { kind: "create".into(), by: Some("human:owner".into()), at: "2026-10-01T08:00:00+07:00".into() },
                    HistoryEvent { kind: "edit".into(), by: None, at: "2026-10-22T14:03:11+07:00".into() },
                ],
                partial: Some("renders with loss".into()),
            }),
            body: "# Heading\n\nSome **bold** text.\n\n- a\n- b\n".into(),
        }
    }

    #[test]
    fn concept_round_trips_every_field() {
        let doc = full_doc();
        let text = render_concept(&doc).unwrap();
        assert_eq!(parse_concept(&text).unwrap(), doc);
    }

    #[test]
    fn minimal_concept_round_trips() {
        let doc = NoteDoc {
            title: String::new(),
            tags: vec![],
            sources: vec![],
            generated: None,
            verified: vec![],
            jodd: None,
            body: String::new(),
        };
        let text = render_concept(&doc).unwrap();
        assert_eq!(text, "---\ntype: Note\n---\n");
        assert_eq!(parse_concept(&text).unwrap(), doc);
    }

    #[test]
    fn body_is_carried_byte_for_byte() {
        for body in ["", "x", "\n\n  lead\n", "a\r\nb\r\n", "---\nnot frontmatter\n---\n", "trailing  \n\n\n"] {
            let mut doc = full_doc();
            doc.body = body.into();
            let back = parse_concept(&render_concept(&doc).unwrap()).unwrap();
            assert_eq!(back.body, body, "body {body:?}");
        }
    }

    #[test]
    fn yaml_lookalike_titles_survive() {
        for title in [
            "yes", "null", "- x", "a: b", "\"quoted\"", "#hash", "บันทึกการประชุม", "a\nb",
            "a\n---\nb", " padded ", "tab\there", "~", "123", "1e5", "'single'", "a: b\n- c", "",
        ] {
            let mut doc = full_doc();
            doc.title = title.into();
            let text = render_concept(&doc).unwrap();
            let back = parse_concept(&text).unwrap();
            assert_eq!(back.title, title, "title {title:?} rendered as:\n{text}");
            assert_eq!(back, doc);
        }
    }

    #[test]
    fn lookalike_scalars_in_jodd_fields_survive() {
        let mut doc = full_doc();
        doc.tags = vec!["yes".into(), "1e5".into(), "null".into(), "a: b".into()];
        doc.sources = vec!["https://x.example/?a=b#c".into()];
        let j = doc.jodd.as_mut().unwrap();
        j.uuid = "1e5".into();
        j.folder = "null".into();
        j.content_sha256 = "00123".into();
        j.account = "ssh:me@host:22".into();
        let back = parse_concept(&render_concept(&doc).unwrap()).unwrap();
        assert_eq!(back, doc);
    }

    #[test]
    fn rendered_concept_has_type_note_first_and_okf_only_keys_at_top() {
        let text = render_concept(&full_doc()).unwrap();
        assert!(text.starts_with("---\ntype: Note\n"), "{text}");
        let fm = text.strip_prefix("---\n").unwrap().split("\n---\n").next().unwrap();
        let allowed = ["type", "title", "tags", "generated", "verified", "sources", "jodd"];
        let mut keys = Vec::new();
        for line in fm.lines() {
            if line.starts_with(' ') || line.starts_with('-') {
                continue;
            }
            keys.push(line.split(':').next().unwrap().to_string());
        }
        assert!(!keys.is_empty());
        for k in &keys {
            assert!(allowed.contains(&k.as_str()), "unexpected top-level key {k}");
        }
        assert_eq!(keys, allowed, "key order is fixed");
        assert!(text.contains("body: markdown"));
        assert!(!text.contains("okf_version"));
    }

    #[test]
    fn sources_are_rendered_as_id_resource_maps() {
        let text = render_concept(&full_doc()).unwrap();
        assert!(text.contains("id: s1"), "{text}");
        assert!(text.contains("id: s2"), "{text}");
        assert!(text.contains("resource: https://example.com/article"), "{text}");
    }

    #[test]
    fn foreign_concept_without_jodd_parses() {
        let text = "---\ntype: Metric\ntitle: Revenue\ntags: [a]\nverified: {by: \"human:x\", at: \"2026-01-01T00:00:00Z\"}\nowner: team\n---\n# Definition\n...";
        let doc = parse_concept(text).unwrap();
        assert!(doc.jodd.is_none());
        assert_eq!(doc.title, "Revenue");
        assert_eq!(doc.tags, vec!["a".to_string()]);
        assert_eq!(doc.verified, vec![("human:x".to_string(), "2026-01-01T00:00:00Z".to_string())]);
        assert!(doc.body.starts_with("# Definition"));
    }

    #[test]
    fn malformed_optional_okf_fields_do_not_reject_the_document() {
        let wrap = |fm: &str| format!("---\ntype: Metric\n{fm}\n---\n# Body\n\ntext\n");
        let body = "# Body\n\ntext\n";

        let d = parse_concept(&wrap("tags: ~")).unwrap();
        assert!(d.tags.is_empty());
        assert_eq!(d.body, body);
        let d = parse_concept(&wrap("tags: null")).unwrap();
        assert!(d.tags.is_empty());
        let d = parse_concept(&wrap("tags: foo")).unwrap();
        assert_eq!(d.tags, vec!["foo".to_string()]);
        let d = parse_concept(&wrap("tags: {a: b}")).unwrap();
        assert!(d.tags.is_empty());
        let d = parse_concept(&wrap("tags: [a, [b], 3, c]")).unwrap();
        assert_eq!(d.tags, vec!["a".to_string(), "3".to_string(), "c".to_string()]);

        let d = parse_concept(&wrap("title: [a]")).unwrap();
        assert_eq!(d.title, "");
        assert_eq!(d.body, body);
        let d = parse_concept(&wrap("title: ~")).unwrap();
        assert_eq!(d.title, "");

        let d = parse_concept(&wrap("sources: ~")).unwrap();
        assert!(d.sources.is_empty());
        let d = parse_concept(&wrap("sources: https://x.example")).unwrap();
        assert_eq!(d.sources, vec!["https://x.example".to_string()]);
        let d = parse_concept(&wrap("sources: [{id: s1}, {id: s2, resource: \"u\"}]")).unwrap();
        assert_eq!(d.sources, vec!["u".to_string()]);

        let d = parse_concept(&wrap("generated: ~")).unwrap();
        assert!(d.generated.is_none());
        let d = parse_concept(&wrap("generated: nope")).unwrap();
        assert!(d.generated.is_none());
        let d = parse_concept(&wrap("verified: yes")).unwrap();
        assert!(d.verified.is_empty());
        let d = parse_concept(&wrap("jodd: ~")).unwrap();
        assert!(d.jodd.is_none());
        assert_eq!(d.body, body);
    }

    #[test]
    fn jodd_key_stays_strict() {
        assert!(parse_concept("---\ntype: Note\njodd: nope\n---\nx").is_err());
    }

    #[test]
    fn non_mapping_frontmatter_is_an_error() {
        assert!(parse_concept("---\n- a\n- b\n---\nx").is_err());
    }

    #[test]
    fn missing_title_is_empty_and_crlf_frontmatter_parses() {
        let doc = parse_concept("---\r\ntype: Note\r\n---\r\nbody\r\n").unwrap();
        assert_eq!(doc.title, "");
        assert_eq!(doc.body, "body\r\n");
    }

    #[test]
    fn text_without_frontmatter_is_an_error() {
        assert!(parse_concept("# Just markdown\n").is_err());
        assert!(parse_concept("---\ntype: Note\nno closing marker\n").is_err());
    }

    #[test]
    fn root_index_carries_okf_version() {
        let root = render_index("x", true, &[]);
        assert!(root.starts_with("---\nokf_version: \"0.2\"\n---\n"), "{root}");
        let sub = render_index("x", false, &[]);
        assert!(!sub.contains("---"));
    }

    #[test]
    fn index_lists_entries_and_escapes_link_text() {
        let idx = render_index(
            "Work",
            false,
            &[("Notes/Work/Plan (v2).md".into(), "Plan [draft]\nline2".into())],
        );
        assert!(idx.starts_with("# Work\n"), "{idx}");
        assert!(idx.contains("* [Plan \\[draft\\] line2](Notes/Work/Plan%20%28v2%29.md)\n"), "{idx}");
    }

    #[test]
    fn log_entry_uses_iso_date_heading() {
        let log = render_log("2026-10-24T09:15:00Z", "Backed up 12 notes\nfrom gmail");
        assert_eq!(
            log,
            "# Directory Update Log\n\n## 2026-10-24\n* **Update**: Backed up 12 notes from gmail\n"
        );
    }
}
