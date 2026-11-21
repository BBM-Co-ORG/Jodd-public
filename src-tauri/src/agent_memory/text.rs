//! Note HTML → readable text with its line structure kept, for `read_note`
//! and the briefing. `ssh::mdconv::html_to_md` is exact-or-nothing (it
//! returns `None` unless the round trip is lossless) and `jodd-mcp`'s
//! `html_to_preview` collapses everything onto one line; an agent reading a
//! page needs neither — it needs the lines.

use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// The summary the ingest source block opens with (`llm::markdown`).
const SOURCE_SUMMARY: &str = "Source (verbatim)";

pub fn html_to_text(html: &str, include_source: bool) -> String {
    // A source note (visible-text recognition, so it also covers the plain
    // `<div>` form a sync back leaves). `recognise` itself calls this with
    // `include_source: true`, which is what stops the recursion.
    if !include_source && html.contains(crate::ingest::source_note::MARKER_PREFIX) && crate::ingest::source_note::recognise(html).is_some() {
        let full = render_text(html, true);
        let mut kept: Vec<&str> = full
            .split('\n')
            .take_while(|l| !l.starts_with(crate::ingest::stored::HEADER_PREFIX))
            .collect();
        while kept.last().is_some_and(|l| l.trim().is_empty()) {
            kept.pop();
        }
        kept.push("[Source text omitted — read_note with include_source: true]");
        return kept.join("\n");
    }
    render_text(html, include_source)
}

fn render_text(html: &str, include_source: bool) -> String {
    let dom = html5ever::parse_fragment(
        RcDom::default(),
        Default::default(),
        html5ever::QualName::new(None, html5ever::ns!(html), html5ever::local_name!("body")),
        vec![],
        false,
    )
    .one(html);
    let mut out = String::new();
    if let Some(root) = dom.document.children.borrow().first().cloned() {
        walk(&root, include_source, false, &mut out);
    }
    // One blank line at most between blocks, no trailing spaces — a tidy-up
    // of the block structure `walk` emitted, which a `<pre>`'s own lines are
    // not: they travel as `PRE_NEWLINE` through it and become `\n` after.
    let mut lines: Vec<String> = Vec::new();
    for line in out.lines().map(|l| l.trim_end().to_string()) {
        if line.trim().is_empty() && lines.last().is_none_or(|l| l.trim().is_empty()) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n").replace(PRE_NEWLINE, "\n")
}

/// A `<pre>`'s newline while `walk`'s output is tidied (a Unicode
/// noncharacter: never meant to appear in interchanged text, and stripped
/// from note text on the way in so it cannot be forged).
const PRE_NEWLINE: char = '\u{FDD0}';

fn text_of(node: &Handle) -> String {
    let mut s = String::new();
    fn collect(n: &Handle, s: &mut String) {
        if let NodeData::Text { contents } = &n.data {
            s.push_str(&contents.borrow());
        }
        for c in n.children.borrow().iter() {
            collect(c, s);
        }
    }
    collect(node, &mut s);
    s
}

fn walk(node: &Handle, include_source: bool, in_pre: bool, out: &mut String) {
    match &node.data {
        NodeData::Text { contents } if in_pre => {
            // Inside a `<pre>` the newlines and spacing ARE the structure —
            // the ingest Source block's per-source header lines among them.
            out.extend(contents.borrow().chars().filter(|&c| c != PRE_NEWLINE).map(|c| if c == '\n' { PRE_NEWLINE } else { c }));
        }
        NodeData::Text { contents } => {
            let t = contents.borrow();
            // Collapse the source's own whitespace runs; block structure
            // comes from the elements, not from newlines inside text nodes.
            let collapsed = t.split_whitespace().collect::<Vec<_>>().join(" ").replace(PRE_NEWLINE, "");
            if collapsed.is_empty() {
                // A whitespace-only node between inline elements is still a
                // word break (`<span>#a</span> <span>#b</span>`); between
                // blocks the line break already separates them.
                if !t.is_empty() && !out.is_empty() && !out.ends_with([' ', '\n']) {
                    out.push(' ');
                }
            } else {
                if t.starts_with(char::is_whitespace) && !out.ends_with([' ', '\n']) && !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&collapsed);
                if t.ends_with(char::is_whitespace) {
                    out.push(' ');
                }
            }
        }
        NodeData::Element { name, .. } => {
            let tag = name.local.as_ref();
            match tag {
                "script" | "style" => return,
                "details" if !include_source => {
                    let is_source = node.children.borrow().iter().any(|c| {
                        matches!(&c.data, NodeData::Element { name, .. } if name.local.as_ref() == "summary")
                            && text_of(c).trim() == SOURCE_SUMMARY
                    });
                    if is_source {
                        out.push_str("\n[Source text omitted — read_note with include_source: true]\n");
                        return;
                    }
                }
                "br" => {
                    out.push('\n');
                    return;
                }
                _ => {}
            }
            let block = matches!(
                tag,
                "div" | "p" | "li" | "ul" | "ol" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre" | "blockquote" | "tr" | "hr" | "details" | "summary"
            );
            if block && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            match tag {
                "li" => out.push_str("- "),
                "h1" => out.push_str("# "),
                "h2" => out.push_str("## "),
                "h3" => out.push_str("### "),
                _ => {}
            }
            if tag == "input" {
                let checked = matches!(&node.data, NodeData::Element { attrs, .. } if attrs.borrow().iter().any(|a| a.name.local.as_ref() == "checked"));
                out.push_str(if checked { "[x] " } else { "[ ] " });
            }
            for c in node.children.borrow().iter() {
                walk(c, include_source, in_pre || tag == "pre", out);
            }
            if block && !out.ends_with('\n') {
                out.push('\n');
            }
        }
        _ => {
            for c in node.children.borrow().iter() {
                walk(c, include_source, in_pre, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_become_lines_and_lists_keep_their_dashes() {
        let t = html_to_text("<div>Summary</div><ul><li>[decision] a</li><li>b</li></ul><h2>Head</h2><p>x<br>y</p>", false);
        assert_eq!(t, "Summary\n- [decision] a\n- b\n## Head\nx\ny");
    }

    #[test]
    fn the_source_block_is_omitted_unless_asked_for() {
        let html = "<p>Key</p><hr>\n<details>\n<summary>Source (verbatim)</summary>\n<pre>SECRET TRANSCRIPT</pre>\n</details>";
        let without = html_to_text(html, false);
        assert!(without.contains("Key") && !without.contains("SECRET") && without.contains("include_source"), "{without}");
        assert!(html_to_text(html, true).contains("SECRET TRANSCRIPT"));
    }

    /// A `<pre>`'s newlines ARE its structure. The ingest Source block is one
    /// `<pre>` whose `=== Jodd source K of N ===` / URL / Title / Status
    /// lines are what tell one source from the next; collapsing them put a
    /// whole multi-source block on one line for every agent that read it.
    #[test]
    fn pre_keeps_its_line_breaks_and_spacing() {
        let block = "=== Jodd source 1 of 2 ===\nURL: https://a.example/\nTitle: A\nStatus: ok\n\nline one\n  indented\n\n=== Jodd source 2 of 2 ===\nURL: https://b.example/\nTitle: B\nStatus: ok\n\nsecond";
        let html = crate::llm::markdown::assemble_note_body(
            &crate::llm::provider::ExtractEnvelope { title: None, lessons_markdown: "## Point\n- x".into(), meta_lessons_markdown: None, tags: vec![], confidence: None },
            block,
            crate::backend::TagModel::BodyText,
        );
        let t = html_to_text(&html, true);
        assert!(t.contains(block), "{t}");
        // Outside a <pre>, newlines inside text are still not structure.
        assert_eq!(html_to_text("<p>a\n   b</p><pre>c\n   d</pre><p>e</p>", false), "a b\nc\n   d\ne");
        // The blank-line tidy-up is for block structure, not a <pre>'s lines.
        assert_eq!(html_to_text("<pre>a\n\n\nb  \nc</pre>", false), "a\n\n\nb  \nc");
        // Note text cannot forge a line break through the placeholder.
        assert_eq!(html_to_text("<p>x\u{FDD0}y</p><pre>p\u{FDD0}q</pre>", false), "xy\npq");
    }

    #[test]
    fn scripts_are_never_text_and_entities_decode() {
        assert_eq!(html_to_text("<div>a &amp; b<script>evil()</script></div>", false), "a & b");
    }

    #[test]
    fn whitespace_between_inline_elements_is_a_space() {
        // iCloud renders each inline hashtag as its own element; gluing them
        // turned `#a #b` into `#a#b`, which reads as one tag.
        assert_eq!(html_to_text("<div><span>#a</span> <span>#b</span></div>", false), "#a #b");
        assert_eq!(html_to_text("<b>foo</b> <i>bar</i>", false), "foo bar");
        assert_eq!(html_to_text("<div>a</div>\n<div>b</div>", false), "a\nb");
    }

    #[test]
    fn checkboxes_show_their_state() {
        let t = html_to_text(r#"<div><input type="checkbox">&nbsp;todo</div><div><input type="checkbox" checked="">&nbsp;done</div>"#, false);
        assert!(t.contains("[ ]") && t.contains("todo") && t.contains("[x]") && t.contains("done"), "{t}");
    }

    fn a_source_note() -> String {
        use crate::ingest::{source_note::*, FetchStatus, FetchedSource, SourceKind};
        let h = SourceHeader { fetched_at: "2026-11-19 14:05".into(), sha256: content_hash("t"), with: vec![], validators: Validators::default() };
        let src = FetchedSource { url: "https://example.com/a".into(), kind: SourceKind::Web, title: Some("T".into()), text: "SECRET fetched text".into(), status: FetchStatus::Ok };
        render_source_note(&src, &h).1
    }

    #[test]
    fn a_source_note_keeps_its_marker_and_omits_the_fetched_text() {
        let html = a_source_note();
        for body in [html.clone(), crate::ingest::source_note::plain_form(&html)] {
            let t = html_to_text(&body, false);
            assert!(t.starts_with(crate::ingest::source_note::MARKER_PREFIX), "{t}");
            assert!(t.contains("example.com/a"), "{t}");
            assert!(!t.contains("SECRET"), "{t}");
            assert!(t.ends_with("[Source text omitted — read_note with include_source: true]"), "{t}");
            assert!(html_to_text(&body, true).contains("SECRET fetched text"));
        }
    }
}
