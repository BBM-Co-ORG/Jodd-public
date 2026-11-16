//! Editor HTML ↔ Markdown for SSH note files (spec amendment A1).
//!
//! Both directions pivot through the iCloud vertical's paragraph projection
//! (`icloud::format` / `icloud::format_html`), which is already the exact
//! inverse of the editor's own HTML. The dialect is **line-oriented**: one
//! paragraph per line, an empty paragraph is an empty line, and
//! `pulldown-cmark` only ever sees one line's inline text. CommonMark block
//! parsing would merge lines and collapse blank runs, failing the round trip
//! for nearly every real note.

use crate::backend::icloud::compose::utf16_len;
use crate::backend::icloud::format::{formats_round_trip_equal, InlineStyle, Paragraph, ParagraphKind, Span};
use crate::backend::icloud::format_html::{parse_editor_html_objects_as_text, render_paragraphs};

/// `None` when Markdown cannot hold the paragraphs: underline, a line that
/// opens with whitespace, or a code line that is itself a fence.
pub fn paragraphs_to_md(ps: &[Paragraph]) -> Option<String> {
    if ps.is_empty() {
        return Some(String::new());
    }
    let mut lines: Vec<String> = Vec::new();
    let mut prev_number: Option<(i32, u32)> = None;
    let mut i = 0;
    while i < ps.len() {
        let p = &ps[i];
        let quote = "> ".repeat(p.block_quote_level as usize);
        if p.kind == ParagraphKind::Monospaced {
            lines.push(format!("{quote}```"));
            while i < ps.len()
                && ps[i].kind == ParagraphKind::Monospaced
                && ps[i].block_quote_level == p.block_quote_level
            {
                if ps[i].text.trim_start().starts_with("```") {
                    return None;
                }
                lines.push(format!("{quote}{}", ps[i].text));
                i += 1;
            }
            lines.push(format!("{quote}```"));
            prev_number = None;
            continue;
        }
        if p.text.starts_with([' ', '\t']) {
            return None;
        }
        // Only list kinds can express indent in Markdown; a non-list indent
        // (rendered as `margin-left` in editor HTML) would be silently
        // dropped rather than round-tripped.
        let is_list = matches!(
            p.kind,
            ParagraphKind::BulletList | ParagraphKind::DashList | ParagraphKind::NumberedList | ParagraphKind::TodoList
        );
        if !is_list && p.indent != 0 {
            return None;
        }
        let inline = inline_md(p)?;
        let indent = "  ".repeat(p.indent.max(0) as usize);
        let line = match p.kind {
            ParagraphKind::Title => format!("# {inline}"),
            ParagraphKind::Heading => format!("## {inline}"),
            ParagraphKind::Subheading => format!("### {inline}"),
            ParagraphKind::Body => escape_line_start(&inline),
            ParagraphKind::BulletList => format!("{indent}* {inline}"),
            ParagraphKind::DashList => format!("{indent}- {inline}"),
            ParagraphKind::TodoList => format!("{indent}- [{}] {inline}", if p.done { 'x' } else { ' ' }),
            ParagraphKind::NumberedList => {
                let n = match prev_number {
                    Some((ind, n)) if ind == p.indent => n + 1,
                    _ => if p.start_number == 0 { 1 } else { p.start_number },
                };
                prev_number = Some((p.indent, n));
                format!("{indent}{n}. {inline}")
            }
            ParagraphKind::Monospaced => unreachable!("handled above"),
        };
        if p.kind != ParagraphKind::NumberedList {
            prev_number = None;
        }
        lines.push(format!("{quote}{line}"));
        i += 1;
    }
    Some(lines.join("\n") + "\n")
}

/// The paragraph's text cut into maximal same-style runs. Span lengths are
/// UTF-16 units; text past the last span is unstyled.
fn segments(p: &Paragraph) -> Vec<(InlineStyle, String)> {
    fn push(out: &mut Vec<(InlineStyle, String)>, style: &InlineStyle, s: String) {
        if s.is_empty() {
            return;
        }
        match out.last_mut() {
            Some((st, t)) if st == style => t.push_str(&s),
            _ => out.push((style.clone(), s)),
        }
    }
    let mut out = Vec::new();
    let mut chars = p.text.chars();
    for span in &p.spans {
        let mut taken = 0;
        let mut s = String::new();
        while taken < span.length {
            match chars.next() {
                Some(c) => {
                    taken += c.len_utf16();
                    s.push(c);
                }
                None => break,
            }
        }
        push(&mut out, &span.style, s);
    }
    push(&mut out, &InlineStyle::default(), chars.collect());
    out
}

fn inline_md(p: &Paragraph) -> Option<String> {
    let mut out = String::new();
    for (style, text) in segments(p) {
        if style.underline {
            return None;
        }
        let core = text.trim_matches(' ');
        if core.is_empty() {
            out.push_str(&text);
            continue;
        }
        let lead = &text[..text.len() - text.trim_start_matches(' ').len()];
        let trail = &text[text.trim_end_matches(' ').len()..];
        let mut s = escape_inline(core);
        if style.strikethrough {
            s = format!("~~{s}~~");
        }
        if style.italic {
            s = format!("*{s}*");
        }
        if style.bold {
            s = format!("**{s}**");
        }
        if !style.link.is_empty() {
            s = format!("[{s}]({})", link_dest(&style.link));
        }
        out.push_str(lead);
        out.push_str(&s);
        out.push_str(trail);
    }
    Some(out)
}

fn escape_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '_' | '[' | ']' | '`' | '~' | '<' | '&') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn link_dest(url: &str) -> String {
    if url.contains([' ', '(', ')', '<', '>']) {
        format!("<{}>", url.replace('<', "%3C").replace('>', "%3E"))
    } else {
        url.to_string()
    }
}

/// Escape only what would change the line's block kind. `#tag` stays as-is:
/// hashtags are how tags travel (gotcha-free, spec §1).
fn escape_line_start(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let hashes = chars.iter().take_while(|&&c| c == '#').count();
    let heading_like = (1..=6).contains(&hashes) && matches!(chars.get(hashes), None | Some(' '));
    let first = chars.first().copied();
    let rule_like = {
        let t: Vec<char> = chars.iter().copied().filter(|c| !c.is_whitespace()).collect();
        t.len() >= 3 && (t.iter().all(|&c| c == '-') || t.iter().all(|&c| c == '+'))
    };
    let bullet_like = matches!(first, Some('-' | '+')) && (matches!(chars.get(1), None | Some(' ')) || rule_like);
    let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    let ordered_like = (1..=9).contains(&digits)
        && matches!(chars.get(digits), Some('.' | ')'))
        && matches!(chars.get(digits + 1), None | Some(' '));
    if ordered_like {
        let (d, rest) = s.split_at(digits);
        return format!("{d}\\{rest}");
    }
    if heading_like || bullet_like || first == Some('>') {
        return format!("\\{s}");
    }
    s.to_string()
}

/// Every element and attribute is one `render_paragraphs` itself emits. Any
/// other markup would be dropped silently by the projection, so a note that
/// has some cannot take the Markdown path.
pub(crate) fn vocabulary_ok(html: &str) -> bool {
    use markup5ever_rcdom::{Handle, NodeData};

    fn ok(node: &Handle) -> bool {
        match &node.data {
            NodeData::Element { name, attrs, .. } => {
                let attrs = attrs.borrow();
                let names: Vec<String> = attrs.iter().map(|a| a.name.local.to_string()).collect();
                let get = |k: &str| attrs.iter().find(|a| &*a.name.local == k).map(|a| a.value.to_string());
                let only = |allowed: &[&str]| names.iter().all(|n| allowed.contains(&n.as_str()));
                let allowed = match &*name.local {
                    "br" | "h1" | "h2" | "h3" | "pre" | "ul" | "li" | "blockquote" | "b" | "strong"
                    | "i" | "em" | "strike" | "s" => names.is_empty(),
                    "ol" => only(&["start"]),
                    "a" => only(&["href"]),
                    "input" => only(&["type", "checked", "contenteditable", "disabled"])
                        && get("type").is_some_and(|t| t.eq_ignore_ascii_case("checkbox")),
                    "div" => names.is_empty()
                        || (get("class").as_deref() == Some("jodd-task")
                            && only(&["class", "style"])
                            && get("style").map_or(true, |s| crate::llm::markdown::is_margin_left_only(&s))),
                    "span" => get("data-jodd-inline").is_some()
                        && only(&["data-jodd-inline", "data-ref", "contenteditable"]),
                    _ => false,
                };
                allowed && node.children.borrow().iter().all(ok)
            }
            NodeData::Comment { .. } => false,
            _ => node.children.borrow().iter().all(ok),
        }
    }

    crate::llm::markdown::with_fragment_root(html, |root| root.children.borrow().iter().all(ok))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::icloud::format::{InlineStyle, Paragraph, ParagraphKind::*, Span};

    fn plain(kind: crate::backend::icloud::format::ParagraphKind, text: &str) -> Paragraph {
        let len = utf16_len(text);
        Paragraph {
            kind, indent: 0, block_quote_level: 0, done: false, start_number: 0,
            text: text.into(),
            spans: if len == 0 { vec![] } else { vec![Span { style: InlineStyle::default(), length: len }] },
            start: 0,
        }
    }

    fn styled(text: &str, runs: &[(InlineStyle, usize)]) -> Paragraph {
        let mut p = plain(Body, text);
        p.spans = runs.iter().map(|(s, l)| Span { style: s.clone(), length: *l }).collect();
        p
    }

    fn bold() -> InlineStyle { InlineStyle { bold: true, ..Default::default() } }

    #[test]
    fn one_paragraph_is_one_line_and_empty_is_empty() {
        let md = paragraphs_to_md(&[plain(Body, "a"), plain(Body, ""), plain(Body, "b")]).unwrap();
        assert_eq!(md, "a\n\nb\n");
        assert_eq!(paragraphs_to_md(&[]).unwrap(), "");
    }

    #[test]
    fn block_kinds_get_their_markers() {
        let mut todo = plain(TodoList, "ship it");
        todo.indent = 1;
        todo.done = true;
        let mut n1 = plain(NumberedList, "three");
        n1.start_number = 3;
        let n2 = plain(NumberedList, "four");
        let mut quoted = plain(Body, "said");
        quoted.block_quote_level = 1;
        let md = paragraphs_to_md(&[
            plain(Title, "T"), plain(Heading, "H"), plain(Subheading, "S"),
            plain(BulletList, "b"), plain(DashList, "d"), todo, n1, n2, quoted,
        ]).unwrap();
        assert_eq!(md, "# T\n## H\n### S\n* b\n- d\n  - [x] ship it\n3. three\n4. four\n> said\n");
    }

    #[test]
    fn a_run_of_monospaced_lines_is_one_fence() {
        let md = paragraphs_to_md(&[plain(Monospaced, "let x = 1;"), plain(Monospaced, "  x"), plain(Body, "after")]).unwrap();
        assert_eq!(md, "```\nlet x = 1;\n  x\n```\nafter\n");
    }

    #[test]
    fn inline_styles_become_markers_that_retreat_off_spaces() {
        let p = styled("hello world", &[(bold(), 6), (InlineStyle::default(), 5)]);
        assert_eq!(paragraphs_to_md(&[p]).unwrap(), "**hello** world\n");
        let link = InlineStyle { link: "https://j.co/a b".into(), ..Default::default() };
        let p = styled("go", &[(link, 2)]);
        assert_eq!(paragraphs_to_md(&[p]).unwrap(), "[go](<https://j.co/a b>)\n");
    }

    #[test]
    fn text_that_looks_like_markup_is_escaped() {
        let md = paragraphs_to_md(&[
            plain(Body, "# not a heading"), plain(Body, "1. not a list"), plain(Body, "- nor this"),
            plain(Body, "a*b_c [x] `y` ~z~ <t> & more"), plain(Body, "#tag stays a hashtag"), plain(Body, "---"),
        ]).unwrap();
        assert_eq!(
            md,
            "\\# not a heading\n1\\. not a list\n\\- nor this\n\
             a\\*b\\_c \\[x\\] \\`y\\` \\~z\\~ \\<t> \\& more\n#tag stays a hashtag\n\\---\n"
        );
    }

    /// Markdown has no underline; the note must go to `.html` instead.
    #[test]
    fn underline_cannot_be_written() {
        let u = InlineStyle { underline: true, ..Default::default() };
        assert_eq!(paragraphs_to_md(&[styled("x", &[(u, 1)])]), None);
        assert_eq!(paragraphs_to_md(&[plain(Body, "  leading spaces")]), None);
        assert_eq!(paragraphs_to_md(&[plain(Monospaced, "```")]), None);
    }

    #[test]
    fn non_list_indent_cannot_be_written() {
        let mut body = plain(Body, "x");
        body.indent = 1;
        assert_eq!(paragraphs_to_md(&[body]), None);
        let mut title = plain(Title, "T");
        title.indent = 1;
        assert_eq!(paragraphs_to_md(&[title]), None);
    }

    #[test]
    fn the_vocabulary_is_what_the_renderer_emits() {
        for ok in [
            "<div>a <b>b</b> <i>i</i> <strike>s</strike> <a href=\"https://j.co\">l</a></div><div><br></div>",
            "<h1>t</h1><h2>h</h2><h3>s</h3><pre>code</pre><blockquote><div>q</div></blockquote>",
            "<ul><li>a<ul><li>b</li></ul></li></ul><ol start=\"3\"><li>c</li></ol>",
            "<div class=\"jodd-task\" style=\"margin-left: 28px\"><input type=\"checkbox\" checked contenteditable=\"false\">&nbsp;t</div>",
            "<div><input type=\"checkbox\">&nbsp;t</div>",
        ] {
            assert!(vocabulary_ok(ok), "{ok}");
        }
        for bad in [
            "<div><span style=\"color:red\">x</span></div>",
            "<div><u>x</u></div>",
            "<div style=\"margin-left: 28px\">indented body</div>",
            "<div><img src=\"cid:a\"></div>",
            "<table><tr><td>x</td></tr></table>",
            "<div><font size=\"5\">x</font></div>",
            "<p>paragraph</p>",
            "<div><!-- c --></div>",
        ] {
            assert!(!vocabulary_ok(bad), "{bad}");
        }
    }
}

/// Editor HTML → file Markdown, or `None` to write the note as `.html`.
/// `Some` only when the round trip is exact in the projection (spec A1).
pub fn html_to_md(html: &str) -> Option<String> {
    if !vocabulary_ok(html) {
        return None;
    }
    let parsed = parse_editor_html_objects_as_text(html).paragraphs;
    let md = paragraphs_to_md(&parsed)?;
    formats_round_trip_equal(&parsed, &md_to_paragraphs(&md)).then_some(md)
}

/// File Markdown → editor HTML. Total: any text reads as something.
pub fn md_to_html(md: &str) -> String {
    render_paragraphs(&md_to_paragraphs(md), &[])
}

pub fn md_to_paragraphs(md: &str) -> Vec<Paragraph> {
    if md.is_empty() {
        return Vec::new();
    }
    let body = md.strip_suffix('\n').unwrap_or(md);
    let mut out: Vec<Paragraph> = Vec::new();
    let mut fence: Option<u32> = None;
    for raw in body.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let (level, line) = strip_quotes(raw);
        if let Some(fence_level) = fence {
            if level == fence_level && line.trim() == "```" {
                fence = None;
            } else {
                out.push(para(ParagraphKind::Monospaced, 0, fence_level, false, 0, line.to_string(), None));
            }
            continue;
        }
        if line.starts_with("```") {
            fence = Some(level);
            continue;
        }
        out.push(block_line(level, line));
    }
    let mut start = 0;
    for p in &mut out {
        p.start = start;
        start += utf16_len(&p.text) + 1;
    }
    out
}

fn strip_quotes(mut s: &str) -> (u32, &str) {
    let mut n = 0;
    while let Some(rest) = s.strip_prefix('>') {
        n += 1;
        s = rest.strip_prefix(' ').unwrap_or(rest);
    }
    (n, s)
}

fn is_list(kind: ParagraphKind) -> bool {
    matches!(
        kind,
        ParagraphKind::BulletList | ParagraphKind::DashList | ParagraphKind::NumberedList | ParagraphKind::TodoList
    )
}

/// `spans: None` = parse `text` as inline Markdown; `Some` = use verbatim.
fn para(
    kind: ParagraphKind,
    indent: i32,
    level: u32,
    done: bool,
    start_number: u32,
    text: String,
    spans: Option<Vec<Span>>,
) -> Paragraph {
    let (text, spans) = match spans {
        Some(s) => (text, s),
        None if kind == ParagraphKind::Monospaced => {
            let len = utf16_len(&text);
            let s = if len == 0 { vec![] } else { vec![Span { style: InlineStyle::default(), length: len }] };
            (text, s)
        }
        None => parse_inline(&text),
    };
    Paragraph {
        kind,
        indent: if is_list(kind) { indent } else { 0 },
        block_quote_level: level,
        done,
        start_number,
        text,
        spans,
        start: 0,
    }
}

fn block_line(level: u32, line: &str) -> Paragraph {
    use ParagraphKind::*;
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && (line.len() == hashes || line[hashes..].starts_with(' ')) {
        let kind = match hashes { 1 => Title, 2 => Heading, _ => Subheading };
        let text = line[hashes..].strip_prefix(' ').unwrap_or(&line[hashes..]);
        return para(kind, 0, level, false, 0, text.to_string(), None);
    }
    let spaces = line.chars().take_while(|&c| c == ' ').count();
    let indent = (spaces / 2) as i32;
    let rest = &line[spaces..];
    for bullet in ["- [", "* [", "+ ["] {
        if let Some(after) = rest.strip_prefix(bullet) {
            let mut cs = after.chars();
            if let (Some(mark @ (' ' | 'x' | 'X')), Some(']')) = (cs.next(), cs.next()) {
                let tail = &after[2..];
                if tail.is_empty() || tail.starts_with(' ') {
                    let text = tail.strip_prefix(' ').unwrap_or(tail);
                    return para(TodoList, indent, level, mark != ' ', 0, text.to_string(), None);
                }
            }
        }
    }
    if let Some(text) = rest.strip_prefix("- ").or((rest == "-").then_some("")) {
        return para(DashList, indent, level, false, 0, text.to_string(), None);
    }
    if let Some(text) = rest.strip_prefix("* ").or_else(|| rest.strip_prefix("+ ")) {
        return para(BulletList, indent, level, false, 0, text.to_string(), None);
    }
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits) {
        let after = &rest[digits..];
        if let Some(text) = after.strip_prefix(". ").or_else(|| after.strip_prefix(") ")) {
            let n = rest[..digits].parse().unwrap_or(1);
            return para(NumberedList, indent, level, false, n, text.to_string(), None);
        }
    }
    para(Body, 0, level, false, 0, line.to_string(), None)
}

fn parse_inline(text: &str) -> (String, Vec<Span>) {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    fn push_text(plain: &mut String, spans: &mut Vec<Span>, style: &InlineStyle, t: &str) {
        if t.is_empty() {
            return;
        }
        plain.push_str(t);
        let len = utf16_len(t);
        match spans.last_mut() {
            Some(s) if s.style == *style => s.length += len,
            _ => spans.push(Span { style: style.clone(), length: len }),
        }
    }

    let mut plain = String::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut style = InlineStyle::default();
    let mut outer_links: Vec<String> = Vec::new();
    for event in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(Tag::Strong) => style.bold = true,
            Event::End(TagEnd::Strong) => style.bold = false,
            Event::Start(Tag::Emphasis) => style.italic = true,
            Event::End(TagEnd::Emphasis) => style.italic = false,
            Event::Start(Tag::Strikethrough) => style.strikethrough = true,
            Event::End(TagEnd::Strikethrough) => style.strikethrough = false,
            Event::Start(Tag::Link { dest_url, .. }) => {
                outer_links.push(std::mem::replace(&mut style.link, dest_url.to_string()));
            }
            Event::End(TagEnd::Link) => style.link = outer_links.pop().unwrap_or_default(),
            Event::Text(t) | Event::Code(t) | Event::Html(t) | Event::InlineHtml(t) => {
                push_text(&mut plain, &mut spans, &style, &t)
            }
            Event::SoftBreak | Event::HardBreak => push_text(&mut plain, &mut spans, &style, " "),
            _ => {}
        }
    }
    (plain, spans)
}

#[cfg(test)]
mod reader_tests {
    use super::*;
    use crate::backend::icloud::format::ParagraphKind::*;
    use crate::backend::icloud::format_html::parse_editor_html_objects_as_text;

    fn kinds(md: &str) -> Vec<(crate::backend::icloud::format::ParagraphKind, i32, bool, String)> {
        md_to_paragraphs(md).into_iter().map(|p| (p.kind, p.indent, p.done, p.text)).collect()
    }

    #[test]
    fn lines_map_to_paragraphs_one_to_one() {
        assert_eq!(
            kinds("a\n\nb\n"),
            vec![(Body, 0, false, "a".into()), (Body, 0, false, "".into()), (Body, 0, false, "b".into())]
        );
        assert!(md_to_paragraphs("").is_empty());
        assert_eq!(kinds("\n"), vec![(Body, 0, false, "".into())]);
    }

    #[test]
    fn nested_checklists_read_back_with_indent_and_state() {
        assert_eq!(
            kinds("- [ ] a\n  - [x] b\n* [X] c\n"),
            vec![(TodoList, 0, false, "a".into()), (TodoList, 1, true, "b".into()), (TodoList, 0, true, "c".into())]
        );
    }

    /// What an agent or a person writes by hand, not just what Jodd wrote.
    #[test]
    fn hand_written_markdown_is_read_sensibly() {
        let ps = md_to_paragraphs("Some **bold** text\n#### deep\n1. one\n2) two\n> quoted\n```rust\nfn x() {}\n```\n");
        let got: Vec<_> = ps.iter().map(|p| (p.kind, p.text.as_str(), p.block_quote_level)).collect();
        assert_eq!(got, vec![
            (Body, "Some bold text", 0), (Subheading, "deep", 0), (NumberedList, "one", 0),
            (NumberedList, "two", 0), (Body, "quoted", 1), (Monospaced, "fn x() {}", 0),
        ]);
        assert!(ps[0].spans.iter().any(|s| s.style.bold && s.length == 4));
        assert_eq!(ps[2].start_number, 1);
    }

    #[test]
    fn escapes_read_back_as_literal_text() {
        let ps = md_to_paragraphs("\\# not a heading\n1\\. not a list\na\\*b \\[x\\] \\& c\n#tag\n");
        let texts: Vec<_> = ps.iter().map(|p| (p.kind, p.text.as_str())).collect();
        assert_eq!(texts, vec![(Body, "# not a heading"), (Body, "1. not a list"), (Body, "a*b [x] & c"), (Body, "#tag")]);
    }

    /// Bodies shaped the way NoteEditor and `render_paragraphs` produce them.
    const EDITOR_FIXTURES: &[&str] = &[
        "<div>Hello <b>world</b></div><div><br></div><div>second line with #tag</div>",
        "<div class=\"jodd-task\"><input type=\"checkbox\" contenteditable=\"false\">&nbsp;todo</div>\
         <div class=\"jodd-task\" style=\"margin-left: 28px\"><input type=\"checkbox\" checked contenteditable=\"false\">&nbsp;<b>done</b> it</div>",
        "<h1>Title</h1><h2>Heading</h2><div>body with <i>italic</i> and <strike>gone</strike></div>",
        "<ul><li>one<ul><li>nested</li></ul></li><li>two</li></ul><ol start=\"3\"><li>three</li><li>four</li></ol>",
        "<blockquote><div>quoted</div></blockquote><pre>let x = 1;</pre><div><a href=\"https://j.co/x\">link</a></div>",
        "<div>ไทย ภาษา and emoji 🎉 <b>หนา</b></div>",
        "<div># literal hash</div><div>1. literal number</div><div>a*b_c</div>",
    ];

    #[test]
    fn every_editor_fixture_round_trips_through_markdown() {
        for html in EDITOR_FIXTURES {
            let md = html_to_md(html).unwrap_or_else(|| panic!("should fit Markdown: {html}"));
            let original = parse_editor_html_objects_as_text(html).paragraphs;
            let back = parse_editor_html_objects_as_text(&md_to_html(&md)).paragraphs;
            assert!(formats_round_trip_equal(&original, &back), "{html}\n--- md ---\n{md}");
        }
    }

    #[test]
    fn a_checklist_becomes_gfm_tasks_in_the_file() {
        let md = html_to_md(EDITOR_FIXTURES[1]).unwrap();
        assert_eq!(md, "- [ ] todo\n  - [x] **done** it\n");
    }

    #[test]
    fn what_markdown_cannot_hold_is_refused() {
        for html in [
            "<div><u>under</u></div>",
            "<div><span style=\"color:red\">red</span></div>",
            "<div><img src=\"cid:abc\"></div>",
            "<div style=\"margin-left: 28px\">indented body</div>",
        ] {
            assert_eq!(html_to_md(html), None, "{html}");
        }
    }

    /// Spec §5, the Q4-C scenario at the codec level: a program ticks a box.
    #[test]
    fn a_sed_tick_on_the_server_checks_the_box() {
        let md = html_to_md(EDITOR_FIXTURES[1]).unwrap().replace("- [ ] todo", "- [x] todo");
        let html = md_to_html(&md);
        let ps = parse_editor_html_objects_as_text(&html).paragraphs;
        assert!(ps.iter().all(|p| p.kind == TodoList && p.done), "{html}");
    }
}
