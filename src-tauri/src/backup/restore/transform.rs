//! Pure functions a restored note's body and folder pass through. No I/O, no
//! database: each takes values and returns values, so the whole table of
//! cases is unit-tested without an account.

use crate::backup::AttachmentRef;
use crate::db::{extract_wikilinks, link_text};
use crate::llm::markdown::{escape_html, keywords_line};
use crate::mime822::append_html;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Something a restore could not carry over faithfully; the restore report
/// lists these per note.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Loss {
    /// The target cannot create folders, so the note landed in the root.
    FolderFlattened { from: String },
    /// An attachment was replaced by a placeholder.
    Attachment { filename: String },
    /// The pin was not restored.
    Pin,
    /// Tags were written as a `Keywords:` line (iCloud has no body hashtags).
    TagsAsKeywords,
    /// The backup recorded the note as partial.
    Partial { reason: String },
}

/// Where restored notes go in the target account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Placement {
    /// Same folder path, under the target's root.
    Original,
    /// Everything under one new folder (`<target_root>/<name>/<rest>`).
    UnderNewFolder(String),
}

/// The attribute value of `name` inside one tag's text, quotes removed.
fn attr_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(name) {
        let at = from + pos;
        from = at + name.len();
        let before_ok = at > 0 && lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = tag[at + name.len()..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value = rest[1..].trim_start();
        return Some(match value.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let inner = &value[1..];
                &inner[..inner.find(q).unwrap_or(inner.len())]
            }
            _ => &value[..value.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(value.len())],
        });
    }
    None
}

/// Byte length of the tag starting at `html[0] == '<'`, through its `>`;
/// quotes are respected so a `>` inside an attribute does not end it.
fn tag_len(html: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in html.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(i + 1),
            _ => {}
        }
    }
    None
}

/// The Content-ID an `<object data="cid:X">` or `<img src="cid:X">` tag names.
fn cid_of(tag: &str, is_object: bool) -> Option<&str> {
    let value = attr_value(tag, if is_object { "data" } else { "src" })?.trim();
    let cid = value.get(..4).filter(|p| p.eq_ignore_ascii_case("cid:")).map(|_| &value[4..])?;
    Some(cid.trim_matches(|c| c == '<' || c == '>'))
}

/// Replace every inline attachment element (`<object data="cid:X">…</object>`
/// and `<img src="cid:X">`) with `<span>[Attachment not restored: NAME]</span>`.
/// NAME is the ref's filename, or the content id when there is none (or no
/// ref names that id). A restore into a backend that cannot hold the bytes
/// (gotcha: attachments are Gmail-only) would otherwise leave dead `cid:` links.
pub fn strip_attachments(html: &str, refs: &[AttachmentRef]) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        let Some(rel) = html[i..].find('<') else { break };
        let at = i + rel;
        let is_object = lower[at..].starts_with("<object");
        let is_img = lower[at..].starts_with("<img");
        let name_len = if is_object { 7 } else { 4 };
        let named = (is_object || is_img)
            && html[at + name_len..].chars().next().is_some_and(|c| c.is_whitespace() || c == '>' || c == '/');
        let tag = if named { tag_len(&html[at..]).map(|n| &html[at..at + n]) } else { None };
        let Some(cid) = tag.and_then(|t| cid_of(t, is_object)) else {
            out.push_str(&html[i..at + 1]);
            i = at + 1;
            continue;
        };
        let tag_end = at + tag.map_or(0, str::len);
        let end = if is_object && !tag.is_some_and(|t| t.ends_with("/>")) {
            lower[tag_end..].find("</object>").map_or(tag_end, |p| tag_end + p + "</object>".len())
        } else {
            tag_end
        };
        let name = refs
            .iter()
            .find(|r| r.content_id.trim_matches(|c| c == '<' || c == '>') == cid)
            .and_then(|r| r.filename.as_deref())
            .filter(|f| !f.is_empty())
            .unwrap_or(cid);
        out.push_str(&html[i..at]);
        out.push_str(&format!("<span>[Attachment not restored: {}]</span>", escape_html(name)));
        i = end;
    }
    out.push_str(&html[i.min(html.len())..]);
    out
}

/// Add the tags as a `Keywords:` line INSIDE the document (gotcha #37). With
/// no tags (or none left after cleaning), the body comes back unchanged.
pub fn append_keywords(html: &str, tags: &[String]) -> String {
    let line = keywords_line(tags);
    if line.is_empty() {
        return html.to_string();
    }
    append_html(html, &line)
}

/// The first 8 lowercase hex digits of a uuid, dashes removed.
/// The 8-hex id a `[[title-<id>]]` link carries for `uuid` — the one rule the
/// editor and SQL use (`db::uuid_short`), so a Microsoft `internetMessageId`
/// maps through its hash rather than not at all.
fn id_prefix(uuid: &str) -> Option<String> {
    Some(crate::db::uuid_short(uuid))
}

/// Point `[[Title-xxxxxxxx]]` links at the notes' new uuids. `old_to_new` maps
/// full uuids; a wikilink whose trailing 8-hex suffix is the prefix of a mapped
/// old uuid gets the new uuid's. A link with no mapping, and text that merely
/// looks like a suffix, stay as they are.
///
/// "Wikilink" is `db::extract_wikilinks` over `db::link_text`, the one
/// definition the editor, the edge table and Ask share, and the rewrite hits
/// the literal `[[inner]]` in the HTML. A link split by inline tags (or
/// written with entities) therefore does not match and is left unchanged:
/// it keeps the old suffix and resolves by title, or not at all.
pub fn rewrite_id_suffixes(html: &str, old_to_new: &HashMap<String, String>) -> String {
    let prefixes: HashMap<String, String> =
        old_to_new.iter().filter_map(|(old, new)| Some((id_prefix(old)?, id_prefix(new)?))).collect();
    if prefixes.is_empty() {
        return html.to_string();
    }
    let mut rewrites: HashMap<String, String> = HashMap::new();
    for inner in extract_wikilinks(&link_text(html)) {
        let Some(pos) = inner.rfind('-') else { continue };
        let suffix = &inner[pos + 1..];
        if suffix.len() != 8 || !suffix.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if let Some(new) = prefixes.get(&suffix.to_ascii_lowercase()) {
            rewrites.insert(inner.clone(), format!("{}{}", &inner[..pos + 1], new));
        }
    }
    if rewrites.is_empty() {
        return html.to_string();
    }
    // One pass, so a rewritten link is never rewritten again.
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while let Some(rel) = html[i..].find("[[") {
        let start = i + rel + 2;
        out.push_str(&html[i..start]);
        i = start;
        let Some(len) = html[start..].find("]]") else { break };
        let inner = &html[start..start + len];
        if inner.contains("[[") {
            continue;
        }
        match rewrites.get(inner) {
            Some(new) => out.push_str(new),
            None => out.push_str(inner),
        }
        out.push_str("]]");
        i = start + len + 2;
    }
    out.push_str(&html[i..]);
    out
}

/// Where a note's folder goes in the target account, and what that cost.
///
/// `source_label` is under `source_root` (or is it); `target_root` replaces the
/// root segment. With `folders_writable` false the target cannot create the
/// folder, so the note goes in the root and the move is reported, unless it
/// was in the root already.
pub fn map_folder(
    source_label: &str,
    source_root: &str,
    target_root: &str,
    placement: &Placement,
    folders_writable: bool,
) -> (String, Option<Loss>) {
    if !folders_writable {
        let loss = (source_label != source_root).then(|| Loss::FolderFlattened { from: source_label.to_string() });
        return (target_root.to_string(), loss);
    }
    let rest = if source_label == source_root {
        ""
    } else {
        source_label.strip_prefix(&format!("{source_root}/")).unwrap_or(source_label)
    };
    let mut parts = vec![target_root];
    if let Placement::UnderNewFolder(name) = placement {
        parts.push(name);
    }
    if !rest.is_empty() {
        parts.push(rest);
    }
    (parts.join("/"), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(cid: &str, filename: Option<&str>) -> AttachmentRef {
        AttachmentRef {
            content_id: cid.to_string(),
            filename: filename.map(str::to_string),
            mime: "image/png".to_string(),
            path: format!("attachments/{cid}"),
            x_apple_part_url: None,
        }
    }

    #[test]
    fn attachments_become_placeholders() {
        let refs = [att("a@x", Some("photo.png")), att("b@x", Some("a<b>.png")), att("c@x", None)];
        let html = "<div>one<object type=\"application/x-apple-msg-attachment\" data=\"cid:a@x\"></object>\
            two<img src=\"cid:b@x\">three<OBJECT data='cid:c@x'><i>fallback</i></OBJECT>end</div>";
        assert_eq!(
            strip_attachments(html, &refs),
            "<div>one<span>[Attachment not restored: photo.png]</span>\
             two<span>[Attachment not restored: a&lt;b&gt;.png]</span>\
             three<span>[Attachment not restored: c@x]</span>end</div>"
        );
        // An unrelated image and a non-cid object survive.
        let keep = "<img src=\"https://x/y.png\"><object data=\"file.pdf\"></object>";
        assert_eq!(strip_attachments(keep, &refs), keep);
        // A cid no ref names falls back to the content id.
        assert_eq!(
            strip_attachments("<img src=\"cid:zz\">", &[]),
            "<span>[Attachment not restored: zz]</span>"
        );
        // Multibyte text around the tags is preserved.
        assert_eq!(
            strip_attachments("ก<img src=\"cid:q\">ข", &[att("q", Some("ค.png"))]),
            "ก<span>[Attachment not restored: ค.png]</span>ข"
        );
    }

    #[test]
    fn keywords_go_inside_the_document() {
        let html = "<html><body><div>x</div></body></html>";
        let out = append_keywords(html, &["alpha".to_string(), "b c".to_string()]);
        assert_eq!(out, "<html><body><div>x</div><p>Keywords: alpha, b c</p>\n</body></html>");
        assert!(out.find("Keywords").unwrap() < out.find("</body>").unwrap());
        assert_eq!(append_keywords(html, &[]), html);
        assert_eq!(append_keywords("<div>x</div>", &["#".to_string()]), "<div>x</div>");
    }

    #[test]
    fn id_suffix_rewritten_only_for_mapped_notes() {
        let map = HashMap::from([(
            "3F2A9C1B-0000-4000-8000-000000000001".to_string(),
            "AABBCCDD-1111-4111-8111-000000000002".to_string(),
        )]);
        let html = "<div>[[Plan-3f2a9c1b]] and [[Other-deadbeef]] and 3f2a9c1b and [[Plan-3F2A9C1B]]</div>";
        assert_eq!(
            rewrite_id_suffixes(html, &map),
            "<div>[[Plan-aabbccdd]] and [[Other-deadbeef]] and 3f2a9c1b and [[Plan-aabbccdd]]</div>"
        );
        // A link split by inline tags is not a wikilink to the scanner (R1) and is left as is.
        let split = "<div>[[Plan-3f2a<b>9c1b</b>]]</div>";
        assert_eq!(rewrite_id_suffixes(split, &map), split);
        // A rewritten link is not rewritten twice when its new suffix is another note's old one.
        let chain = HashMap::from([
            ("11111111-0000-4000-8000-000000000000".to_string(), "22222222-0000-4000-8000-000000000000".to_string()),
            ("22222222-0000-4000-8000-000000000000".to_string(), "33333333-0000-4000-8000-000000000000".to_string()),
        ]);
        assert_eq!(rewrite_id_suffixes("[[A-11111111]] [[A-22222222]]", &chain), "[[A-22222222]] [[A-33333333]]");
        assert_eq!(rewrite_id_suffixes("[[Plan-3f2a9c1b]]", &HashMap::new()), "[[Plan-3f2a9c1b]]");
    }

    #[test]
    fn microsoft_ids_map_through_the_shared_short_id_rule() {
        // A Microsoft source id is an internetMessageId: its link suffix is
        // db::uuid_short's hash, not hex digits read off the server name.
        let ms = "<JH0PR04MB7174E99@JH0PR04MB7174.apcprd04.prod.outlook.com>".to_string();
        let new = "AABBCCDD-1111-4111-8111-000000000002".to_string();
        let old_short = crate::db::uuid_short(&ms);
        let html = format!("<div>[[Plan-{old_short}]]</div>");
        let map = HashMap::from([(ms, new)]);
        assert_eq!(rewrite_id_suffixes(&html, &map), "<div>[[Plan-aabbccdd]]</div>");
    }

    #[test]
    fn folder_mapping_table() {
        let orig = Placement::Original;
        let under = Placement::UnderNewFolder("Restored a@b.c 2026-10-24".to_string());
        assert_eq!(map_folder("Notes/Work", "Notes", "Notes", &orig, true), ("Notes/Work".to_string(), None));
        assert_eq!(map_folder("Notes/Work/Sub", "Notes", "Jodd", &orig, true), ("Jodd/Work/Sub".to_string(), None));
        assert_eq!(map_folder("Notes", "Notes", "Jodd", &orig, true), ("Jodd".to_string(), None));
        assert_eq!(
            map_folder("Notes/Work", "Notes", "Notes", &under, true),
            ("Notes/Restored a@b.c 2026-10-24/Work".to_string(), None)
        );
        assert_eq!(
            map_folder("Notes", "Notes", "Notes", &under, true),
            ("Notes/Restored a@b.c 2026-10-24".to_string(), None)
        );
        assert_eq!(
            map_folder("Notes/Work", "Notes", "Notes", &orig, false),
            ("Notes".to_string(), Some(Loss::FolderFlattened { from: "Notes/Work".to_string() }))
        );
        assert_eq!(map_folder("Notes", "Notes", "Notes", &under, false), ("Notes".to_string(), None));
    }

    #[test]
    fn serde_shapes() {
        assert_eq!(serde_json::to_string(&Loss::Pin).unwrap(), r#"{"kind":"pin"}"#);
        assert_eq!(
            serde_json::to_string(&Loss::FolderFlattened { from: "a".into() }).unwrap(),
            r#"{"kind":"folderFlattened","from":"a"}"#
        );
        assert_eq!(serde_json::to_string(&Placement::Original).unwrap(), r#""original""#);
        assert_eq!(
            serde_json::to_string(&Placement::UnderNewFolder("x".into())).unwrap(),
            r#"{"underNewFolder":"x"}"#
        );
    }
}
