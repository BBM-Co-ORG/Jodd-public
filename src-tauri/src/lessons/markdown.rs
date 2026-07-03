//! Markdown → HTML conversion + Lessons note body assembly.

use pulldown_cmark::{html, Options, Parser};

use crate::lessons::provider::ExtractEnvelope;

/// Marker for the collapsible Source block. Single source of truth for
/// `assemble_note_body` (writer), `has_source_block` (detector), and
/// `extract_source` (parser) — change here and all three sites stay in sync.
const SOURCE_MARKER: &str = "<summary>Source (verbatim)</summary>";

/// GFM extensions LLMs commonly emit. Tables turn `| col1 | col2 |\n|---|---|`
/// into a real <table>; strikethrough handles `~~text~~`; tasklists render
/// `- [x] done` as `<input type="checkbox">`; footnotes pair `[^1]` with
/// their definitions. Without these, pulldown-cmark's defaults leave the
/// raw markdown syntax visible in the rendered HTML.
fn md_options() -> Options {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts
}

/// Convert a markdown string to HTML. Pure function, no escaping issues —
/// pulldown-cmark handles all the markdown-specific encoding.
pub fn md_to_html(md: &str) -> String {
    let parser = Parser::new_ext(md, md_options());
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// HTML-escape arbitrary text for safe inclusion in HTML.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Assemble the final note body from an envelope + raw source text.
pub fn assemble_note_body(envelope: &ExtractEnvelope, source: &str) -> String {
    let mut body = String::with_capacity(envelope.lessons_markdown.len() + source.len() + 512);

    // Tags line at the top, picked up by Jodd's existing #hashtag parser
    if !envelope.tags.is_empty() {
        body.push_str("<p>");
        for (i, tag) in envelope.tags.iter().enumerate() {
            if i > 0 {
                body.push(' ');
            }
            body.push('#');
            // LLMs sometimes emit `#tag` or multi-word tags; normalize so
            // Jodd's #hashtag parser picks them up (strip leading #,
            // collapse whitespace to '-' so the tag is a single token).
            let clean = tag.trim_start_matches('#').replace(char::is_whitespace, "-");
            body.push_str(&escape_html(&clean));
        }
        body.push_str("</p>\n");
    }

    // Main lessons content
    body.push_str(&md_to_html(&envelope.lessons_markdown));

    // Optional meta-lessons section
    if let Some(meta) = &envelope.meta_lessons_markdown {
        if !meta.trim().is_empty() {
            body.push_str(&md_to_html(meta));
        }
    }

    // Collapsible source section — pure HTML, source verbatim in <pre>
    body.push_str("<hr>\n<details>\n");
    body.push_str(SOURCE_MARKER);
    body.push_str("\n<pre>");
    body.push_str(&escape_html(source));
    body.push_str("</pre>\n</details>\n");

    body
}

/// Regex match for whether a note body contains a preserved Source block.
/// Extract the raw source text from a note body that has a Source block.
/// Returns None if no block found or the structure is malformed.
pub fn extract_source(body_html: &str) -> Option<String> {
    let after_marker = body_html.split_once(SOURCE_MARKER)?.1;
    let pre_open = after_marker.find("<pre>")?;
    let after_pre = &after_marker[pre_open + "<pre>".len()..];
    let pre_close = after_pre.find("</pre>")?;
    let raw = &after_pre[..pre_close];
    // Unescape the four entities we inject
    Some(
        raw.replace("&quot;", "\"")
            .replace("&gt;", ">")
            .replace("&lt;", "<")
            .replace("&amp;", "&"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md_to_html_handles_basic_markdown() {
        let html = md_to_html("## H2\n\nparagraph **bold**");
        assert!(html.contains("<h2>H2</h2>"));
        assert!(html.contains("<strong>bold</strong>"));
    }

    #[test]
    fn md_to_html_renders_gfm_tables() {
        let md = "| Col1 | Col2 |\n|---|---|\n| a | b |";
        let html = md_to_html(md);
        assert!(html.contains("<table>"), "missing <table>: {html}");
        assert!(html.contains("<th>Col1</th>"));
        assert!(html.contains("<td>a</td>"));
    }

    #[test]
    fn md_to_html_renders_strikethrough_and_tasklists() {
        assert!(md_to_html("~~struck~~").contains("<del>struck</del>"));
        let tasklist = md_to_html("- [x] done\n- [ ] todo");
        assert!(tasklist.contains("<input"), "missing tasklist input: {tasklist}");
        assert!(tasklist.contains("disabled"), "tasklist should be disabled: {tasklist}");
    }

    #[test]
    fn escape_html_escapes_all_four() {
        assert_eq!(
            escape_html("a&b<c>d\"e"),
            "a&amp;b&lt;c&gt;d&quot;e"
        );
    }

    #[test]
    fn assemble_includes_all_sections() {
        let env = ExtractEnvelope {
            title: Some("T".into()),
            lessons_markdown: "## Lesson 1\nbody".into(),
            meta_lessons_markdown: Some("## Meta\nm".into()),
            tags: vec!["tag-a".into(), "tag-b".into()],
            confidence: Some("high".into()),
        };
        let body = assemble_note_body(&env, "raw source text");
        assert!(body.contains("#tag-a #tag-b"), "tag line: {body}");
        assert!(body.contains("<h2>Lesson 1</h2>"));
        assert!(body.contains("<h2>Meta</h2>"));
        assert!(body.contains("<summary>Source (verbatim)</summary>"));
        assert!(body.contains("raw source text"));
    }

    #[test]
    fn assemble_omits_meta_when_absent_or_empty() {
        let env = ExtractEnvelope {
            title: None,
            lessons_markdown: "x".into(),
            meta_lessons_markdown: Some("   ".into()),
            tags: vec![],
            confidence: None,
        };
        let body = assemble_note_body(&env, "src");
        assert!(!body.contains("Meta"));
    }

    #[test]
    fn extract_source_roundtrips_special_chars() {
        let env = ExtractEnvelope {
            title: None,
            lessons_markdown: "x".into(),
            meta_lessons_markdown: None,
            tags: vec![],
            confidence: None,
        };
        let original = "code: <script>alert(\"hi & bye\")</script>";
        let body = assemble_note_body(&env, original);
        let recovered = extract_source(&body).unwrap();
        assert_eq!(recovered, original);
    }

    #[test]
    fn extract_source_returns_none_for_normal_note() {
        assert_eq!(extract_source("<p>just a note</p>"), None);
    }
}
