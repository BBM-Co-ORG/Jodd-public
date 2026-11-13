//! One SSH note file ↔ one note. The file contract is spec §1 with
//! amendments A2/A3: `.md` carries its title as the first line, `.html`
//! carries it in the frontmatter, and an `.html` body is stored verbatim.

use super::frontmatter::{self, Frontmatter};
use super::mdconv;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ext {
    Md,
    Html,
}

impl Ext {
    pub fn as_str(self) -> &'static str {
        match self {
            Ext::Md => "md",
            Ext::Html => "html",
        }
    }

    pub fn of_path(path: &str) -> Option<Ext> {
        if path.ends_with(".md") {
            Some(Ext::Md)
        } else if path.ends_with(".html") {
            Some(Ext::Html)
        } else {
            None
        }
    }
}

pub fn file_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name).to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub fm: Option<Frontmatter>,
    pub title: String,
    pub body_html: String,
    pub ext: Ext,
}

pub fn decode(path: &str, bytes: &[u8]) -> Option<Decoded> {
    let ext = Ext::of_path(path)?;
    let text = String::from_utf8_lossy(bytes);
    let (fm, rest) = frontmatter::split(&text);
    let stem = file_stem(path);
    let (title, body_html) = match ext {
        Ext::Md => {
            let (title, body_md) = split_title(rest);
            (if title.is_empty() { stem } else { title }, mdconv::md_to_html(body_md))
        }
        Ext::Html => {
            let title = fm.as_ref().and_then(|f| f.title.clone()).filter(|t| !t.is_empty()).unwrap_or(stem);
            (title, rest.to_string())
        }
    };
    Some(Decoded { fm, title, body_html, ext })
}

/// The first line is the title; exactly one blank line after it is the
/// separator `encode` writes, and is dropped.
fn split_title(md: &str) -> (String, &str) {
    let (first, rest) = md.split_once('\n').unwrap_or((md, ""));
    let first = first.trim_end_matches('\r');
    let hashes = first.chars().take_while(|&c| c == '#').count();
    let title = if (1..=6).contains(&hashes) && (first.len() == hashes || first[hashes..].starts_with(' ')) {
        first[hashes..].trim_start_matches(' ')
    } else {
        first
    };
    let rest = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n')).unwrap_or(rest);
    (title.trim().to_string(), rest)
}

pub fn encode(fm: &Frontmatter, title: &str, body_html: &str) -> (Ext, String) {
    let head = frontmatter::render(fm);
    match mdconv::html_to_md(body_html) {
        Some(md) => {
            let title_line = title.replace(['\n', '\r'], " ");
            (Ext::Md, format!("{head}# {title_line}\n\n{md}"))
        }
        None => (Ext::Html, format!("{head}{body_html}")),
    }
}

pub fn with_frontmatter(bytes: &[u8], edit: impl FnOnce(&mut Frontmatter)) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let (fm, rest) = frontmatter::split(&text);
    let mut fm = fm.unwrap_or_default();
    edit(&mut fm);
    let mut out = frontmatter::render(&fm).into_bytes();
    // `rest` came from the lossy text, so a body with invalid UTF-8 would be
    // mangled by U+FFFD replacement; take the body from the original bytes
    // by offset instead, unless the frontmatter block itself is what held
    // the invalid bytes (then the lossy `rest` is the best we can do).
    let head_len = text.len() - rest.len();
    match bytes.get(..head_len) {
        Some(prefix) if prefix == &text.as_bytes()[..head_len] => out.extend_from_slice(&bytes[head_len..]),
        _ => out.extend_from_slice(rest.as_bytes()),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::icloud::format::formats_round_trip_equal;
    use crate::backend::icloud::format_html::parse_editor_html_objects_as_text;

    fn fm(uuid: &str) -> Frontmatter {
        Frontmatter { uuid: Some(uuid.into()), title: Some("T".into()), created: None, pinned: false, extra: vec![] }
    }

    fn same_body(a: &str, b: &str) -> bool {
        formats_round_trip_equal(
            &parse_editor_html_objects_as_text(a).paragraphs,
            &parse_editor_html_objects_as_text(b).paragraphs,
        )
    }

    #[test]
    fn a_checklist_note_is_written_as_markdown() {
        let body = "<div class=\"jodd-task\"><input type=\"checkbox\" contenteditable=\"false\">&nbsp;a</div>";
        let (ext, text) = encode(&fm("u"), "T", body);
        assert_eq!(ext, Ext::Md);
        assert_eq!(text, "---\nuuid: u\ntitle: \"T\"\npinned: false\n---\n# T\n\n- [ ] a\n");
    }

    #[test]
    fn underline_sends_the_note_to_html_verbatim() {
        let body = "<div><u>x</u></div>";
        let (ext, text) = encode(&fm("u"), "T", body);
        assert_eq!(ext, Ext::Html);
        assert!(text.ends_with("---\n<div><u>x</u></div>"), "{text}");
        let d = decode("Notes/t.html", text.as_bytes()).unwrap();
        assert_eq!((d.title.as_str(), d.body_html.as_str()), ("T", body));
    }

    #[test]
    fn encode_then_decode_is_the_same_note() {
        for body in [
            "<div>Hello <b>world</b></div><div><br></div><div>x</div>",
            "<div><br></div><div>starts with an empty line</div>",
            "",
        ] {
            let head = Frontmatter { title: Some("My # title".into()), ..fm("u") };
            let (ext, text) = encode(&head, "My # title", body);
            let d = decode(&format!("Notes/n.{}", ext.as_str()), text.as_bytes()).unwrap();
            assert_eq!(d.title, "My # title");
            assert_eq!(d.fm, Some(Frontmatter { title: Some("My # title".into()), ..fm("u") }));
            assert!(same_body(body, &d.body_html), "{body:?}\n{text}\n{:?}", d.body_html);
        }
    }

    #[test]
    fn a_file_a_program_wrote_decodes_without_frontmatter() {
        let d = decode("Notes/Inbox/summary.md", b"# Summary\n\n- [ ] do it\n").unwrap();
        assert_eq!(d.fm, None);
        assert_eq!(d.title, "Summary");
        assert!(d.body_html.contains("type=\"checkbox\""), "{}", d.body_html);
        let d = decode("Notes/Inbox/plain.md", b"first line is the title\nbody\n").unwrap();
        assert_eq!(d.title, "first line is the title");
        assert_eq!(decode("Notes/empty.md", b"").unwrap().title, "empty");
        assert!(decode("Notes/x.txt", b"x").is_none());
    }

    #[test]
    fn with_frontmatter_keeps_the_body_bytes_exactly() {
        let original = b"# Summary\n\nweird  spacing\t\n* kept *as written*\n";
        let out = with_frontmatter(original, |f| { f.uuid = Some("u".into()); f.pinned = true; });
        let text = String::from_utf8(out).unwrap();
        assert!(text.ends_with(std::str::from_utf8(original).unwrap()), "{text}");
        let (fm, _) = crate::backend::ssh::frontmatter::split(&text);
        assert_eq!(fm.map(|f| (f.uuid, f.pinned)), Some((Some("u".into()), true)));

        let with_extra = b"---\nuuid: u\nsource: bot\n---\nbody";
        let out = String::from_utf8(with_frontmatter(with_extra, |f| f.pinned = true)).unwrap();
        assert_eq!(out, "---\nuuid: u\npinned: true\nsource: bot\n---\nbody");
    }

    #[test]
    fn with_frontmatter_preserves_non_utf8_body_bytes() {
        let original = b"---\nuuid: u\n---\nbody \xff\xfe bytes";
        let out = with_frontmatter(original, |f| f.pinned = true);
        assert!(out.ends_with(b"body \xff\xfe bytes"), "{out:?}");

        let no_frontmatter = b"# T\n\xff";
        let out = with_frontmatter(no_frontmatter, |f| f.pinned = true);
        assert!(out.ends_with(b"# T\n\xff"), "{out:?}");
    }

    #[test]
    fn file_stem_and_ext() {
        assert_eq!(file_stem("Notes/A/it's.md"), "it's");
        assert_eq!(Ext::of_path("Notes/a.html"), Some(Ext::Html));
        assert_eq!(Ext::of_path("Notes/a.md"), Some(Ext::Md));
        assert_eq!(Ext::of_path("Notes/a.MD"), None);
    }
}
