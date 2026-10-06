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
        walk(&root, include_source, &mut out);
    }
    // One blank line at most between blocks, no trailing spaces.
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
    lines.join("\n")
}

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

fn walk(node: &Handle, include_source: bool, out: &mut String) {
    match &node.data {
        NodeData::Text { contents } => {
            let t = contents.borrow();
            // Collapse the source's own whitespace runs; block structure
            // comes from the elements, not from newlines inside text nodes.
            let collapsed = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if !collapsed.is_empty() {
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
                walk(c, include_source, out);
            }
            if block && !out.ends_with('\n') {
                out.push('\n');
            }
        }
        _ => {
            for c in node.children.borrow().iter() {
                walk(c, include_source, out);
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

    #[test]
    fn scripts_are_never_text_and_entities_decode() {
        assert_eq!(html_to_text("<div>a &amp; b<script>evil()</script></div>", false), "a & b");
    }

    #[test]
    fn checkboxes_show_their_state() {
        let t = html_to_text(r#"<div><input type="checkbox">&nbsp;todo</div><div><input type="checkbox" checked="">&nbsp;done</div>"#, false);
        assert!(t.contains("[ ]") && t.contains("todo") && t.contains("[x]") && t.contains("done"), "{t}");
    }
}
