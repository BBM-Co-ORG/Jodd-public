//! Body encoding for bundle files. Markdown is the note only when it
//! reproduces the original HTML exactly (`html_to_md`'s own round-trip check
//! is exact in the projection, not byte-exact, so the byte check lives here).
//! Otherwise the `.md` file still gets readable text and the exact HTML is
//! kept beside it (`BodyFormat::HtmlFile`).

use super::BodyFormat;
use crate::backend::ssh::mdconv::{html_to_md, md_to_html, paragraphs_to_md};
use sha2::{Digest, Sha256};

/// The `.md` body and its format. For `HtmlFile` the caller keeps `html`
/// itself in the bundle; the text returned is only for reading.
pub fn encode_body(html: &str) -> (BodyFormat, String) {
    if let Some(md) = html_to_md(html) {
        if md_to_html(&md) == html {
            return (BodyFormat::Markdown, md);
        }
    }
    (BodyFormat::HtmlFile, readable_text(html))
}

/// Best-effort Markdown for a body that has no exact form: the SSH vault's
/// paragraph model when its styles allow, else the note's lines as text.
pub fn readable_text(html: &str) -> String {
    let parsed = crate::backend::icloud::format_html::parse_editor_html_objects_as_text(html).paragraphs;
    paragraphs_to_md(&parsed).unwrap_or_else(|| crate::agent_memory::text::html_to_text(html, true))
}

/// The body HTML from a concept's text. `HtmlFile` never reaches here with
/// its readable text: `OpenBundle::open` swaps in the exact HTML first.
pub fn decode_body(fmt: BodyFormat, text: &str) -> String {
    match fmt {
        BodyFormat::Markdown => md_to_html(text),
        BodyFormat::Html | BodyFormat::HtmlFile => text.to_string(),
    }
}

/// Lowercase hex sha256 of the body HTML.
pub fn content_sha256(html: &str) -> String {
    Sha256::digest(html.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_only_when_byte_exact() {
        let (f, t) = encode_body("<p>Hello <b>world</b></p>");
        if f == BodyFormat::Markdown {
            assert_eq!(decode_body(f, &t), "<p>Hello <b>world</b></p>");
        }
        let (f2, t2) = encode_body("<div><u>under</u></div>");
        assert_eq!(f2, BodyFormat::HtmlFile);
        assert!(t2.contains("under") && !t2.contains('<'), "readable, no tags: {t2:?}");
    }

    #[test]
    fn some_simple_body_is_stored_as_markdown() {
        // Guards against the byte check silently rejecting everything.
        let found = ["<div>Hello</div>", "<p>Hello</p>", "<div>Hello<br></div>"]
            .iter()
            .any(|h| encode_body(h).0 == BodyFormat::Markdown);
        assert!(found, "no simple body encodes as Markdown");
    }

    #[test]
    fn sha_is_of_html_lower_hex() {
        assert_eq!(content_sha256("a").len(), 64);
        assert!(content_sha256("a")
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
