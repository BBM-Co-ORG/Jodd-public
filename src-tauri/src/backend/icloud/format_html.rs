//! The paragraph projection ↔ editor HTML, both directions — M3's F2.
//!
//! [`render_paragraphs`] emits the HTML shapes `NoteEditor.svelte`'s own
//! execCommand toolbar produces, so a note read from iCloud and a note
//! formatted in Jodd have the same structure: `<h1>/<h2>/<h3>` for
//! Title/Heading/Subheading, `<div>` for Body, one `<pre>` per run of
//! Monospaced lines, `<ul>/<ol>` with nesting by `indent`, the editor's
//! checkbox-`<div>` shape for checklists, `<blockquote>` nesting for
//! blockquote levels, and `<b> <i> <u> <strike> <a>` for the inline spans.
//!
//! [`parse_editor_html`] is its inverse over everything the renderer AND the
//! editor can produce, and its plain-text output is byte-compatible with
//! `compose::html_to_text` for every unformatted shape — the save path uses
//! the parse's text so the text layer and the format model can never
//! disagree.
//!
//! A resolved inline hashtag renders as
//! `<span data-jodd-inline="hashtag" data-ref="{recordName}">#tag</span>`
//! and parses back to `U+FFFC` plus a [`HashtagRef`] — the text layer sees
//! the same object character Apple's document carries (spec F5).

use super::compose::{decode_entities_into, is_inter_block_whitespace, utf16_len};
use super::format::{InlineStyle, Paragraph, ParagraphKind, Span};

/// One resolved inline text attachment, pairing (in document order) with a
/// `U+FFFC` in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashtagRef {
    /// The `InlineAttachment` record's `recordName` — which is what
    /// `attachmentInfo.attachmentIdentifier` carries on the wire.
    pub record_name: String,
    /// The rendered tag text, e.g. `#work` (`AltTextEncrypted`).
    pub text: String,
}

/// The object placeholder — see `compose::OBJECT_REPLACEMENT`'s doc.
const OBJECT_REPLACEMENT: char = '\u{FFFC}';
/// Apple's soft line break inside a paragraph (see `doc::LINE_SEPARATOR`).
const LINE_SEPARATOR: char = '\u{2028}';
/// Pixels per indent level — `NoteEditor.svelte`'s own margin step
/// (`margin / 28` defines the tree in its nested-checklist logic).
const INDENT_PX: i32 = 28;

/// The hashtag span contract, pinned once for both the render and the parse
/// (and greppable from the frontend): renaming either side alone would make
/// the parser stop recognizing the span, turning the tag's rendered text
/// into a literal text change pushed to the server.
pub const INLINE_KIND_ATTR: &str = "data-jodd-inline";
pub const INLINE_REF_ATTR: &str = "data-ref";
pub const INLINE_KIND_HASHTAG: &str = "hashtag";

fn escape_text(s: &str) -> String {
    super::doc::escape_html_text(s)
}
fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

// ── rendering ───────────────────────────────────────────────────────────

/// Renders the inline content of one paragraph: spans wrapped in the fixed
/// order `<b><i><u><strike><a>`, `U+2028` as `<br>`, and each `U+FFFC`
/// consuming the next entry of `objects` (via `object_cursor`) — `Some`
/// renders the marked hashtag span, `None` keeps the raw character.
fn render_inline(
    paragraph: &Paragraph,
    objects: &[Option<HashtagRef>],
    object_cursor: &mut usize,
) -> String {
    // Span END boundaries in UTF-16 units, with a synthetic plain span
    // covering any uncovered tail (spans may under-cover; the tail is plain).
    let total = utf16_len(&paragraph.text);
    let mut spans: Vec<(InlineStyle, usize)> = Vec::with_capacity(paragraph.spans.len() + 1);
    let mut end = 0usize;
    for span in &paragraph.spans {
        end += span.length;
        spans.push((span.style.clone(), end.min(total)));
    }
    if end < total {
        spans.push((InlineStyle::default(), total));
    }
    if spans.is_empty() {
        return String::new();
    }

    let wrap = |style: &InlineStyle, inner: String| -> String {
        let mut wrapped = inner;
        if !style.link.is_empty() {
            wrapped = format!(r#"<a href="{}">{wrapped}</a>"#, escape_attr(&style.link));
        }
        if style.strikethrough {
            wrapped = format!("<strike>{wrapped}</strike>");
        }
        if style.underline {
            wrapped = format!("<u>{wrapped}</u>");
        }
        if style.italic {
            wrapped = format!("<i>{wrapped}</i>");
        }
        if style.bold {
            wrapped = format!("<b>{wrapped}</b>");
        }
        wrapped
    };

    // One pass over the CHARS, assigning each to the span containing its
    // starting unit — a surrogate pair whose halves straddle a span boundary
    // stays whole in the earlier span, where slicing a `Vec<u16>` at the raw
    // boundary would have produced U+FFFD in the cached HTML.
    let mut out = String::new();
    let mut inner = String::new();
    let mut span_index = 0usize;
    let mut unit = 0usize;
    for ch in paragraph.text.chars() {
        while span_index + 1 < spans.len() && unit >= spans[span_index].1 {
            out.push_str(&wrap(&spans[span_index].0, std::mem::take(&mut inner)));
            span_index += 1;
        }
        match ch {
            LINE_SEPARATOR => inner.push_str("<br>"),
            OBJECT_REPLACEMENT => {
                let resolved = objects.get(*object_cursor).and_then(|o| o.as_ref());
                *object_cursor += 1;
                match resolved {
                    Some(tag) => inner.push_str(&format!(
                        r#"<span {INLINE_KIND_ATTR}="{INLINE_KIND_HASHTAG}" {INLINE_REF_ATTR}="{}" contenteditable="false">{}</span>"#,
                        escape_attr(&tag.record_name),
                        escape_text(&tag.text),
                    )),
                    None => inner.push(OBJECT_REPLACEMENT),
                }
            }
            '&' => inner.push_str("&amp;"),
            '<' => inner.push_str("&lt;"),
            '>' => inner.push_str("&gt;"),
            c => inner.push(c),
        }
        unit += ch.len_utf16();
    }
    out.push_str(&wrap(&spans[span_index].0, inner));
    // Trailing zero-length spans (none observed, but cheap to honor).
    for (style, _) in &spans[span_index + 1..] {
        out.push_str(&wrap(style, String::new()));
    }
    out
}

/// The editor-HTML rendering of a projection. `objects[i]` pairs, in
/// document order, with the i-th `U+FFFC` across all paragraphs — `None`
/// (or a missing entry) keeps the raw character, today's behavior.
pub fn render_paragraphs(paragraphs: &[Paragraph], objects: &[Option<HashtagRef>]) -> String {
    /// One open list level. `parent_li_open` records whether this level was
    /// opened inside a then-open parent `<li>` — closing the level closes
    /// that `<li>` too, and ONLY then: a list starting at indent ≥ 1 has no
    /// parent item, and emitting `</li>` for it anyway produced unbalanced
    /// HTML the contenteditable would silently renormalize.
    struct ListLevel {
        ordered: bool,
        parent_li_open: bool,
    }

    let mut out = String::new();
    let mut cursor = 0usize; // U+FFFC index across the document
    let mut i = 0usize;
    // Open list levels, kept across consecutive list paragraphs so items
    // share one <ul>/<ol> and deeper indents nest.
    let mut list_stack: Vec<ListLevel> = Vec::new();
    let mut quote_depth: u32 = 0;
    let mut li_open = false;

    let close_lists_to =
        |out: &mut String, stack: &mut Vec<ListLevel>, li_open: &mut bool, depth: usize| {
            while stack.len() > depth {
                if *li_open {
                    out.push_str("</li>");
                }
                let level = stack.pop().unwrap();
                out.push_str(if level.ordered { "</ol>" } else { "</ul>" });
                // The list we just closed was nested inside the parent's
                // <li> — close that too, exactly when it was open.
                if level.parent_li_open {
                    out.push_str("</li>");
                }
                *li_open = false;
            }
        };

    while i < paragraphs.len() {
        let p = &paragraphs[i];
        let is_list_item =
            matches!(p.kind, ParagraphKind::BulletList | ParagraphKind::DashList | ParagraphKind::NumberedList);

        // Blockquote grouping: enter/leave to match this paragraph's level.
        while quote_depth < p.block_quote_level {
            close_lists_to(&mut out, &mut list_stack, &mut li_open, 0);
            out.push_str("<blockquote>");
            quote_depth += 1;
        }
        while quote_depth > p.block_quote_level {
            close_lists_to(&mut out, &mut list_stack, &mut li_open, 0);
            out.push_str("</blockquote>");
            quote_depth -= 1;
        }

        if !is_list_item {
            close_lists_to(&mut out, &mut list_stack, &mut li_open, 0);
        }

        match p.kind {
            ParagraphKind::Monospaced => {
                // Consecutive monospaced lines share one <pre>.
                let mut lines = Vec::new();
                while i < paragraphs.len()
                    && paragraphs[i].kind == ParagraphKind::Monospaced
                    && paragraphs[i].block_quote_level == p.block_quote_level
                {
                    lines.push(escape_text(&paragraphs[i].text));
                    i += 1;
                }
                out.push_str(&format!("<pre>{}</pre>", lines.join("\n")));
                continue;
            }
            ParagraphKind::TodoList => {
                let margin = if p.indent > 0 {
                    format!(r#" style="margin-left: {}px""#, p.indent * INDENT_PX)
                } else {
                    String::new()
                };
                let checked = if p.done { " checked" } else { "" };
                let inner = render_inline(p, objects, &mut cursor);
                out.push_str(&format!(
                    r#"<div class="jodd-task"{margin}><input type="checkbox"{checked} contenteditable="false">&nbsp;{inner}</div>"#
                ));
            }
            ParagraphKind::BulletList | ParagraphKind::DashList | ParagraphKind::NumberedList => {
                let ordered = p.kind == ParagraphKind::NumberedList;
                let depth = (p.indent.max(0) as usize) + 1;
                // Close levels deeper than this item; close the current level
                // too if its orderedness differs.
                close_lists_to(&mut out, &mut list_stack, &mut li_open, depth);
                if list_stack.len() == depth
                    && list_stack.last().map(|l| l.ordered) != Some(ordered)
                {
                    close_lists_to(&mut out, &mut list_stack, &mut li_open, depth - 1);
                }
                // Open levels up to this item's depth.
                while list_stack.len() < depth {
                    let opening_tag = if ordered {
                        let start = if p.start_number == 0 { 1 } else { p.start_number };
                        if list_stack.len() == depth - 1 && start != 1 {
                            format!(r#"<ol start="{start}">"#)
                        } else {
                            "<ol>".to_string()
                        }
                    } else {
                        "<ul>".to_string()
                    };
                    // A nested list opens inside the previous, still-open
                    // <li> (which closes when the level closes); at the
                    // outermost level — or on an indent jump with no parent
                    // item — there is no <li> to nest in, and the close side
                    // must know that.
                    out.push_str(&opening_tag);
                    list_stack.push(ListLevel { ordered, parent_li_open: li_open });
                    li_open = false;
                }
                if li_open {
                    out.push_str("</li>");
                }
                let inner = render_inline(p, objects, &mut cursor);
                out.push_str(&format!("<li>{inner}"));
                li_open = true;
            }
            ParagraphKind::Title | ParagraphKind::Heading | ParagraphKind::Subheading => {
                let tag = match p.kind {
                    ParagraphKind::Title => "h1",
                    ParagraphKind::Heading => "h2",
                    _ => "h3",
                };
                let inner = render_inline(p, objects, &mut cursor);
                let inner = if inner.is_empty() { "<br>".to_string() } else { inner };
                out.push_str(&format!("<{tag}>{inner}</{tag}>"));
            }
            ParagraphKind::Body => {
                let margin = if p.indent > 0 {
                    format!(r#" style="margin-left: {}px""#, p.indent * INDENT_PX)
                } else {
                    String::new()
                };
                let inner = render_inline(p, objects, &mut cursor);
                let inner = if inner.is_empty() { "<br>".to_string() } else { inner };
                out.push_str(&format!("<div{margin}>{inner}</div>"));
            }
        }
        i += 1;
    }
    close_lists_to(&mut out, &mut list_stack, &mut li_open, 0);
    while quote_depth > 0 {
        out.push_str("</blockquote>");
        quote_depth -= 1;
    }
    out
}

// ── parsing ─────────────────────────────────────────────────────────────

/// The parse of one editor body: the projection, the plain text (the
/// '\n'-join of the paragraphs, `U+FFFC` standing in for each hashtag
/// span), and the hashtag refs in document order, 1:1 with the `U+FFFC`
/// occurrences they produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedBody {
    pub paragraphs: Vec<Paragraph>,
    pub text: String,
    pub hashtags: Vec<HashtagRef>,
}

/// A soft break while a paragraph is still being assembled — the same
/// placeholder trick `compose::html_to_text` uses, rewritten before returning.
const SOFT: char = '\u{1}';

/// One block-tag list, shared with `compose::html_to_text` so the two
/// scanners can never split the same HTML into different paragraph counts.
/// `ul`/`ol` are excluded only because dedicated match arms consume them
/// before this fallback runs.
fn is_block_tag(name: &str) -> bool {
    super::compose::is_block(name) && !matches!(name, "ul" | "ol")
}

/// Extracts a double- or single-quoted attribute value from a raw tag body,
/// **entity-decoded** — the renderer (and any serializer) escapes `&` in
/// attribute values as `&amp;`, so returning the raw slice would hand back
/// `https://x.co/?a=1&amp;b=2` for a link whose run carries `…&b=2`, failing
/// the round trip for every query-string URL.
fn attr_value(raw_tag: &str, name: &str) -> Option<String> {
    let mut rest = raw_tag;
    while let Some(pos) = rest.find(name) {
        let after = &rest[pos + name.len()..];
        let after = after.trim_start();
        if let Some(after) = after.strip_prefix('=') {
            let after = after.trim_start();
            let quote = after.chars().next()?;
            let raw = if quote == '"' || quote == '\'' {
                let inner = &after[1..];
                &inner[..inner.find(quote)?]
            } else {
                // Unquoted value: up to whitespace.
                let end = after.find(|c: char| c.is_whitespace()).unwrap_or(after.len());
                &after[..end]
            };
            let mut decoded = String::with_capacity(raw.len());
            decode_entities_into(&mut decoded, raw);
            return Some(decoded);
        }
        rest = &rest[pos + name.len()..];
    }
    None
}

/// `margin-left: 56px` → indent 2, in the editor's own 28px steps.
fn indent_of_style(raw_tag: &str) -> i32 {
    let Some(style) = attr_value(raw_tag, "style") else { return 0 };
    let Some(pos) = style.find("margin-left") else { return 0 };
    let digits: String =
        style[pos..].chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<i32>().map(|px| px / INDENT_PX).unwrap_or(0)
}

#[derive(Clone)]
struct ListCtx {
    ordered: bool,
    /// `<ol start="N">`, consumed by the level's first `<li>`.
    pending_start: Option<u32>,
}

/// The block context the next flush will stamp on its paragraph.
#[derive(Clone)]
struct BlockCtx {
    kind: ParagraphKind,
    indent: i32,
    done: bool,
    start_number: u32,
}

impl Default for BlockCtx {
    fn default() -> Self {
        BlockCtx { kind: ParagraphKind::Body, indent: 0, done: false, start_number: 0 }
    }
}

/// Parses editor HTML into the projection. Inverse of [`render_paragraphs`]
/// over everything it emits, and tolerant of everything the contenteditable
/// itself produces; unknown markup is transparent (text kept, styling
/// ignored), exactly `compose::html_to_text`'s doctrine.
pub fn parse_editor_html(html: &str) -> ParsedBody {
    parse_editor_html_inner(html, false)
}

/// [`parse_editor_html`] with hashtag spans flattened to their rendered TEXT
/// (`#work`) instead of `U+FFFC` + a ref. The CREATE path uses this: a new
/// record cannot carry another note's `InlineAttachment` (the ref belongs to
/// the source note), and a bare `U+FFFC` with no attachment run is a
/// dangling object Apple renders as nothing — plain tag text is the honest
/// shape while native tag creation stays deferred (spec F5).
pub fn parse_editor_html_objects_as_text(html: &str) -> ParsedBody {
    parse_editor_html_inner(html, true)
}

fn parse_editor_html_inner(html: &str, objects_as_text: bool) -> ParsedBody {
    struct P {
        paragraphs: Vec<Paragraph>,
        hashtags: Vec<HashtagRef>,
        buf: String,
        segs: Vec<(InlineStyle, usize)>,
        open_block: bool,
        ctx: BlockCtx,
        quote_depth: u32,
        list_stack: Vec<ListCtx>,
        pre_depth: usize,
        pre_buf: String,
        bold: i32,
        italic: i32,
        underline: i32,
        strike: i32,
        link_stack: Vec<String>,
        hashtag_depth: usize,
        hashtag_ref: String,
        hashtag_text: String,
        strip_leading_nbsp: bool,
    }
    impl P {
        fn style(&self) -> InlineStyle {
            InlineStyle {
                bold: self.bold > 0,
                italic: self.italic > 0,
                strikethrough: self.strike > 0,
                underline: self.underline > 0,
                link: self.link_stack.last().cloned().unwrap_or_default(),
            }
        }
        fn push_units(&mut self, s: &str) {
            if s.is_empty() {
                return;
            }
            let units = utf16_len(s);
            self.buf.push_str(s);
            let style = self.style();
            match self.segs.last_mut() {
                Some((last, len)) if *last == style => *len += units,
                _ => self.segs.push((style, units)),
            }
        }
        fn push_text(&mut self, raw: &str) {
            // Whitespace between block tags is markup, not a blank line —
            // the same rule `compose::html_to_text` applies, shared so the
            // two scanners cannot disagree about what a paragraph is. It is
            // load-bearing HERE beyond the text: a paragraph whose whole
            // text was "\n" broke `reconcile_formatting`'s one-line-per-
            // paragraph invariant, and every save of such a note silently
            // downgraded to text-only (measured live 2026-08-27).
            if self.pre_depth == 0
                && self.hashtag_depth == 0
                && is_inter_block_whitespace(raw, self.buf.is_empty())
            {
                return;
            }
            let mut decoded = String::new();
            decode_entities_into(&mut decoded, raw);
            if self.hashtag_depth > 0 {
                self.hashtag_text.push_str(&decoded);
                return;
            }
            if self.pre_depth > 0 {
                self.pre_buf.push_str(&decoded);
                return;
            }
            let mut s: &str = &decoded;
            if self.strip_leading_nbsp {
                // The editor's caret anchor after a checkbox — one unit only.
                s = s.strip_prefix('\u{a0}').or_else(|| s.strip_prefix(' ')).unwrap_or(s);
                if !decoded.is_empty() {
                    self.strip_leading_nbsp = false;
                }
            }
            self.push_units(s);
        }
        fn flush(&mut self) {
            let raw = std::mem::take(&mut self.buf);
            let segs = std::mem::take(&mut self.segs);
            let text = if raw == "\u{1}" { String::new() } else { raw.replace(SOFT, "\u{2028}") };
            let spans = if text.is_empty() {
                Vec::new()
            } else {
                segs.into_iter().map(|(style, length)| Span { style, length }).collect()
            };
            let ctx = std::mem::take(&mut self.ctx);
            self.paragraphs.push(Paragraph {
                kind: ctx.kind,
                indent: ctx.indent,
                block_quote_level: self.quote_depth,
                done: ctx.done,
                start_number: ctx.start_number,
                text,
                spans,
                start: 0, // assigned at the end
            });
            self.open_block = false;
            self.strip_leading_nbsp = false;
        }
        fn flush_if_content(&mut self) {
            if !self.buf.is_empty() {
                self.flush();
            }
        }
        fn flush_pre(&mut self) {
            let raw = std::mem::take(&mut self.pre_buf);
            for line in raw.split('\n') {
                let text_len = utf16_len(line);
                self.paragraphs.push(Paragraph {
                    kind: ParagraphKind::Monospaced,
                    indent: 0,
                    block_quote_level: self.quote_depth,
                    done: false,
                    start_number: 0,
                    text: line.to_string(),
                    spans: if text_len == 0 {
                        Vec::new()
                    } else {
                        vec![Span { style: InlineStyle::default(), length: text_len }]
                    },
                    start: 0,
                });
            }
            self.open_block = false;
        }
    }

    let mut p = P {
        paragraphs: Vec::new(),
        hashtags: Vec::new(),
        buf: String::new(),
        segs: Vec::new(),
        open_block: false,
        ctx: BlockCtx::default(),
        quote_depth: 0,
        list_stack: Vec::new(),
        pre_depth: 0,
        pre_buf: String::new(),
        bold: 0,
        italic: 0,
        underline: 0,
        strike: 0,
        link_stack: Vec::new(),
        hashtag_depth: 0,
        hashtag_ref: String::new(),
        hashtag_text: String::new(),
        strip_leading_nbsp: false,
    };

    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        p.push_text(&rest[..lt]);
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            // An unterminated `<` is content, not a tag.
            p.push_text(&rest[lt..]);
            rest = "";
            break;
        };
        let raw = &after[..gt];
        rest = &after[gt + 1..];

        let closing = raw.starts_with('/');
        let name: String = raw
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();

        // The hashtag span swallows everything but its own close.
        if p.hashtag_depth > 0 {
            if name == "span" {
                if closing {
                    p.hashtag_depth -= 1;
                    if p.hashtag_depth == 0 {
                        let tag = HashtagRef {
                            record_name: std::mem::take(&mut p.hashtag_ref),
                            text: std::mem::take(&mut p.hashtag_text),
                        };
                        if objects_as_text {
                            p.push_units(&tag.text);
                        } else {
                            p.hashtags.push(tag);
                            p.push_units(&OBJECT_REPLACEMENT.to_string());
                        }
                    }
                } else {
                    p.hashtag_depth += 1;
                }
            }
            continue;
        }

        match name.as_str() {
            "br" if !closing => {
                if p.pre_depth > 0 {
                    p.pre_buf.push('\n');
                } else {
                    let style = p.style();
                    p.buf.push(SOFT);
                    match p.segs.last_mut() {
                        Some((last, len)) if *last == style => *len += 1,
                        _ => p.segs.push((style, 1)),
                    }
                }
            }
            "input" if !closing => {
                let is_checkbox = attr_value(raw, "type").map(|t| t.eq_ignore_ascii_case("checkbox")).unwrap_or(false);
                if is_checkbox && p.buf.is_empty() {
                    p.ctx.kind = ParagraphKind::TodoList;
                    p.ctx.done = raw.contains("checked");
                    p.strip_leading_nbsp = true;
                }
            }
            "span"
                if !closing
                    && attr_value(raw, INLINE_KIND_ATTR).as_deref() == Some(INLINE_KIND_HASHTAG) =>
            {
                p.hashtag_depth = 1;
                p.hashtag_ref = attr_value(raw, INLINE_REF_ATTR).unwrap_or_default();
                p.hashtag_text = String::new();
            }
            "b" | "strong" => {
                p.bold += if closing { -1 } else { 1 };
                p.bold = p.bold.max(0);
            }
            "i" | "em" => {
                p.italic += if closing { -1 } else { 1 };
                p.italic = p.italic.max(0);
            }
            "u" => {
                p.underline += if closing { -1 } else { 1 };
                p.underline = p.underline.max(0);
            }
            "strike" | "s" | "del" => {
                p.strike += if closing { -1 } else { 1 };
                p.strike = p.strike.max(0);
            }
            "a" => {
                if closing {
                    p.link_stack.pop();
                } else {
                    p.link_stack.push(attr_value(raw, "href").unwrap_or_default());
                }
            }
            "ul" | "ol" => {
                p.flush_if_content();
                if closing {
                    p.list_stack.pop();
                    p.open_block = false;
                } else {
                    p.list_stack.push(ListCtx {
                        ordered: name == "ol",
                        pending_start: attr_value(raw, "start").and_then(|v| v.parse().ok()),
                    });
                }
            }
            "pre" => {
                if closing {
                    if p.pre_depth > 0 {
                        p.pre_depth -= 1;
                        if p.pre_depth == 0 {
                            p.flush_pre();
                        }
                    }
                } else {
                    p.flush_if_content();
                    p.pre_depth += 1;
                }
            }
            "blockquote" => {
                if closing {
                    if p.open_block || !p.buf.is_empty() {
                        p.flush();
                    }
                    p.quote_depth = p.quote_depth.saturating_sub(1);
                } else {
                    p.flush_if_content();
                    p.quote_depth += 1;
                }
            }
            _ if is_block_tag(&name) => {
                if p.pre_depth > 0 {
                    continue; // structure inside <pre> is content-neutral
                }
                if closing {
                    if !p.buf.is_empty() || p.open_block {
                        p.flush();
                    }
                } else {
                    p.flush_if_content();
                    p.open_block = true;
                    p.ctx = BlockCtx::default();
                    match name.as_str() {
                        "h1" => p.ctx.kind = ParagraphKind::Title,
                        "h2" => p.ctx.kind = ParagraphKind::Heading,
                        "h3" => p.ctx.kind = ParagraphKind::Subheading,
                        "li" => {
                            if let Some(top) = p.list_stack.last_mut() {
                                p.ctx.kind = if top.ordered {
                                    ParagraphKind::NumberedList
                                } else {
                                    ParagraphKind::BulletList
                                };
                                p.ctx.start_number = top.pending_start.take().unwrap_or(0);
                                p.ctx.indent = (p.list_stack.len() as i32 - 1).max(0);
                            }
                        }
                        _ => {
                            p.ctx.indent = indent_of_style(raw);
                        }
                    }
                }
            }
            // Everything else — <code>, <kbd>, unknown spans, editor debris —
            // is transparent: text kept, styling ignored.
            _ => {}
        }
    }
    p.push_text(rest);
    if p.pre_depth > 0 {
        p.flush_pre();
    }
    if !p.buf.is_empty() {
        p.flush();
    }

    // Assign start offsets and derive the plain text.
    let mut offset = 0usize;
    for paragraph in &mut p.paragraphs {
        paragraph.start = offset;
        offset += utf16_len(&paragraph.text) + 1;
    }
    let text = p.paragraphs.iter().map(|q| q.text.as_str()).collect::<Vec<_>>().join("\n");
    ParsedBody { paragraphs: p.paragraphs, text, hashtags: p.hashtags }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(kind: ParagraphKind, text: &str, spans: Vec<Span>) -> Paragraph {
        Paragraph {
            kind,
            indent: 0,
            block_quote_level: 0,
            done: false,
            start_number: 0,
            text: text.into(),
            spans,
            start: 0,
        }
    }
    fn span(len: usize, f: impl Fn(&mut InlineStyle)) -> Span {
        let mut s = InlineStyle::default();
        f(&mut s);
        Span { style: s, length: len }
    }

    #[test]
    fn each_kind_renders_its_block_shape() {
        let paras = vec![
            para(ParagraphKind::Heading, "Head", vec![span(4, |_| {})]),
            para(ParagraphKind::Body, "plain", vec![span(5, |_| {})]),
            para(ParagraphKind::BulletList, "li1", vec![span(3, |_| {})]),
            Paragraph { done: true, ..para(ParagraphKind::TodoList, "task", vec![span(4, |_| {})]) },
            para(ParagraphKind::Monospaced, "mono", vec![span(4, |_| {})]),
        ];
        let html = render_paragraphs(&paras, &[]);
        assert!(html.contains("<h2>Head</h2>"), "{html}");
        assert!(html.contains("<div>plain</div>"), "{html}");
        assert!(html.contains("<ul><li>li1</li></ul>"), "{html}");
        assert!(html.contains(r#"class="jodd-task""#) && html.contains("checked"), "{html}");
        assert!(html.contains("<pre>mono</pre>"), "{html}");
    }

    #[test]
    fn inline_styles_nest_in_a_fixed_order_and_links_carry_href() {
        let p = para(
            ParagraphKind::Body,
            "bold link",
            vec![
                span(4, |s| s.bold = true),
                span(1, |_| {}),
                span(4, |s| s.link = "https://j.co".into()),
            ],
        );
        let html = render_paragraphs(&[p], &[]);
        assert!(html.contains("<b>bold</b>"), "{html}");
        assert!(html.contains(r#"<a href="https://j.co">link</a>"#), "{html}");
    }

    #[test]
    fn consecutive_list_items_share_one_list_and_indent_nests() {
        let li = |text: &str, indent| Paragraph {
            indent,
            ..para(ParagraphKind::BulletList, text, vec![span(utf16_len(text), |_| {})])
        };
        let html = render_paragraphs(&[li("a", 0), li("b", 1), li("c", 0)], &[]);
        assert_eq!(html.matches("<ul>").count(), 2, "{html}");
        assert!(
            html.contains("<li>a<ul><li>b</li></ul></li><li>c</li>")
                || html.contains("<li>a</li><ul><li>b</li></ul><li>c</li>"),
            "nesting shape must be a contenteditable-round-trippable form: {html}"
        );
    }

    #[test]
    fn a_resolved_hashtag_renders_as_the_marked_span() {
        let p = para(ParagraphKind::Body, "tag \u{FFFC}!", vec![span(6, |_| {})]);
        let html = render_paragraphs(
            &[p],
            &[Some(HashtagRef { record_name: "abc-123".into(), text: "#work".into() })],
        );
        assert!(
            html.contains(r#"<span data-jodd-inline="hashtag" data-ref="abc-123" contenteditable="false">#work</span>"#),
            "{html}"
        );
        assert!(!html.contains('\u{FFFC}'), "{html}");
    }

    #[test]
    fn an_unresolved_object_keeps_the_raw_character() {
        let p = para(ParagraphKind::Body, "x \u{FFFC}", vec![span(3, |_| {})]);
        assert!(render_paragraphs(&[p.clone()], &[None]).contains('\u{FFFC}'));
        assert!(render_paragraphs(&[p], &[]).contains('\u{FFFC}'));
    }

    #[test]
    fn escaping_and_soft_breaks_match_the_plain_renderer() {
        let p = para(ParagraphKind::Body, "a<b>&\u{2028}c", vec![span(8, |_| {})]);
        let html = render_paragraphs(&[p], &[]);
        assert!(html.contains("a&lt;b&gt;&amp;<br>c"), "{html}");
    }

    #[test]
    fn blockquote_levels_nest_and_an_empty_body_line_keeps_its_height() {
        let quoted = Paragraph {
            block_quote_level: 2,
            ..para(ParagraphKind::Body, "deep", vec![span(4, |_| {})])
        };
        let html = render_paragraphs(&[quoted], &[]);
        assert!(html.contains("<blockquote><blockquote><div>deep</div></blockquote></blockquote>"), "{html}");
        let empty = para(ParagraphKind::Body, "", vec![]);
        assert_eq!(render_paragraphs(&[empty], &[]), "<div><br></div>");
    }

    #[test]
    fn an_ordered_group_starting_past_one_carries_the_start_attribute() {
        let p = Paragraph {
            start_number: 3,
            ..para(ParagraphKind::NumberedList, "third", vec![span(5, |_| {})])
        };
        let html = render_paragraphs(&[p], &[]);
        assert!(html.contains(r#"<ol start="3"><li>third</li></ol>"#), "{html}");
    }

    // ── parsing (Task 4) ────────────────────────────────────────────────

    use super::super::compose;
    use super::super::format::formats_round_trip_equal;

    #[test]
    fn parse_render_are_inverses_over_everything_the_renderer_produces() {
        let corpus: Vec<Vec<Paragraph>> = vec![
            vec![
                para(ParagraphKind::Heading, "Head", vec![span(4, |_| {})]),
                para(ParagraphKind::Body, "plain ไทย", vec![span(9, |_| {})]),
            ],
            vec![Paragraph { done: true, ..para(ParagraphKind::TodoList, "task", vec![span(4, |_| {})]) }],
            vec![Paragraph {
                start_number: 3,
                ..para(ParagraphKind::NumberedList, "third", vec![span(5, |_| {})])
            }],
            vec![para(ParagraphKind::Monospaced, "let x = 1;", vec![span(10, |_| {})])],
            vec![para(
                ParagraphKind::Body,
                "a 😀 b",
                vec![span(2, |_| {}), span(2, |s| s.bold = true), span(2, |_| {})],
            )],
            vec![
                para(ParagraphKind::BulletList, "a", vec![span(1, |_| {})]),
                Paragraph { indent: 1, ..para(ParagraphKind::BulletList, "b", vec![span(1, |_| {})]) },
                para(ParagraphKind::BulletList, "c", vec![span(1, |_| {})]),
            ],
            vec![Paragraph {
                block_quote_level: 2,
                ..para(ParagraphKind::Body, "quoted", vec![span(6, |_| {})])
            }],
            vec![para(
                ParagraphKind::Body,
                "u&s",
                vec![span(1, |s| s.underline = true), span(1, |_| {}), span(1, |s| s.strikethrough = true)],
            )],
        ];
        for paras in corpus {
            let html = render_paragraphs(&paras, &[]);
            let parsed = parse_editor_html(&html);
            assert!(
                formats_round_trip_equal(&parsed.paragraphs, &paras),
                "round trip failed on {html}\nparsed: {:#?}\nwanted: {paras:#?}",
                parsed.paragraphs
            );
        }
    }

    #[test]
    fn parsed_text_matches_html_to_text_for_unformatted_shapes() {
        for html in [
            "<div>a</div><div><br></div><div>b</div>",
            "plain title<div>body</div>",
            "<div>a<br>b</div>",
            "<div>a &amp; b</div>",
            "<div><div>a</div></div>",
            "",
            "one line",
        ] {
            assert_eq!(
                parse_editor_html(html).text,
                compose::html_to_text(html),
                "text drifted from html_to_text on {html}"
            );
        }
    }

    /// **The invariant `reconcile_formatting` depends on, pinned.**
    /// `parsed.text` is the `\n`-join of `parsed.paragraphs`, so a caller
    /// can convert between "how many lines" and "how many paragraphs"
    /// freely. Whitespace a serializer or a paste puts between block tags
    /// used to become a paragraph whose entire text was a newline, which
    /// broke that in both directions at once: the pushed text gained blank
    /// lines, and the paragraph count no longer matched, so the save
    /// downgraded to text-only and the user's formatting vanished
    /// (measured live 2026-08-27).
    #[test]
    fn parsed_text_is_always_the_newline_join_of_the_paragraphs() {
        for html in [
            "<div>a</div>\n<div>b</div>",
            "<div>a</div>\n  <div>b</div>",
            "<div>a</div>\n",
            "\n<div>a</div>",
            "<ul>\n  <li>a</li>\n  <li>b</li>\n</ul>",
            "<div>a</div><div><br></div><div>b</div>",
            "<h2>head</h2>\n<div>body</div>",
            "<div>  spaced  </div>",
        ] {
            let parsed = parse_editor_html(html);
            assert_eq!(
                parsed.text,
                parsed.paragraphs.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join("\n"),
                "text is not the join of the paragraphs on {html}"
            );
            assert!(
                parsed.paragraphs.iter().all(|p| !p.text.contains('\n')),
                "a paragraph carries a newline on {html}: {:?}",
                parsed.paragraphs.iter().map(|p| p.text.as_str()).collect::<Vec<_>>()
            );
            assert_eq!(
                parsed.text,
                compose::html_to_text(html),
                "the two scanners disagree on {html}"
            );
        }
        // Pretty-printed markup must not invent blank lines.
        assert_eq!(parse_editor_html("<div>a</div>\n<div>b</div>").text, "a\nb");
        assert_eq!(parse_editor_html("<ul>\n  <li>a</li>\n</ul>").paragraphs.len(), 1);
        // Content whitespace survives.
        assert_eq!(parse_editor_html("<div>  spaced  </div>").text, "  spaced  ");
    }

    #[test]
    fn a_hashtag_span_parses_back_to_the_object_character() {
        let html = r#"<div>tag <span data-jodd-inline="hashtag" data-ref="abc-123" contenteditable="false">#work</span>!</div>"#;
        let parsed = parse_editor_html(html);
        assert_eq!(parsed.text, "tag \u{FFFC}!");
        assert_eq!(parsed.hashtags.len(), 1);
        assert_eq!(parsed.hashtags[0].record_name, "abc-123");
        assert_eq!(parsed.hashtags[0].text, "#work");
    }

    #[test]
    fn checkbox_divs_are_todo_lines_and_checked_means_done() {
        let html = r#"<div class="jodd-task"><input type="checkbox" checked contenteditable="false">&nbsp;done it</div><div><input type="checkbox">&nbsp;not yet</div>"#;
        let ps = parse_editor_html(html).paragraphs;
        assert_eq!(ps.len(), 2);
        assert!(ps[0].kind == ParagraphKind::TodoList && ps[0].done);
        assert_eq!(ps[0].text, "done it");
        assert!(ps[1].kind == ParagraphKind::TodoList && !ps[1].done);
        assert_eq!(ps[1].text, "not yet");
    }

    #[test]
    fn inline_code_and_unknown_markup_parse_as_plain_text() {
        let parsed = parse_editor_html("<div>a <code>x</code> <kbd>k</kbd></div>");
        assert_eq!(parsed.text, "a x k");
        assert_eq!(
            parsed.paragraphs[0].spans.iter().filter(|s| s.style != InlineStyle::default()).count(),
            0
        );
    }

    #[test]
    fn margin_left_maps_to_indent_in_28px_steps() {
        let parsed = parse_editor_html(r#"<div style="margin-left: 56px">deep</div>"#);
        assert_eq!(parsed.paragraphs[0].indent, 2);
    }

    #[test]
    fn a_multi_line_pre_parses_to_one_monospaced_paragraph_per_line() {
        let parsed = parse_editor_html("<pre>a\nb</pre>");
        assert_eq!(parsed.paragraphs.len(), 2);
        assert!(parsed.paragraphs.iter().all(|q| q.kind == ParagraphKind::Monospaced));
        assert_eq!(parsed.text, "a\nb");
    }

    /// The review's finding 1: an href with a query string escapes `&` as
    /// `&amp;` on render, and the parse must decode it back — otherwise
    /// every note with such a link fails the round trip and flips read-only.
    #[test]
    fn a_query_string_url_survives_the_href_round_trip() {
        let url = "https://x.co/?a=1&b=2";
        let p = para(ParagraphKind::Body, "link", vec![span(4, |s| s.link = url.into())]);
        let html = render_paragraphs(&[p.clone()], &[]);
        assert!(html.contains("&amp;b=2"), "render must escape the ampersand: {html}");
        let parsed = parse_editor_html(&html);
        assert_eq!(parsed.paragraphs[0].spans[0].style.link, url, "parse must decode it back");
        assert!(formats_round_trip_equal(&parsed.paragraphs, &[p]));
    }

    /// The review's finding 4: a list whose first item is indented has no
    /// parent <li> — the close side must not emit one.
    #[test]
    fn a_list_starting_at_indent_one_renders_balanced_html() {
        let p = Paragraph {
            indent: 1,
            ..para(ParagraphKind::BulletList, "deep", vec![span(4, |_| {})])
        };
        let html = render_paragraphs(&[p], &[]);
        assert_eq!(html.matches("<li").count(), html.matches("</li>").count(), "{html}");
        assert_eq!(html.matches("<ul>").count(), html.matches("</ul>").count(), "{html}");
    }

    /// The review's finding 7: a span boundary between a surrogate pair's
    /// halves must not shatter the pair into U+FFFD.
    #[test]
    fn a_span_boundary_inside_a_surrogate_pair_keeps_the_character_whole() {
        // "😀x" is 3 units; spans split at unit 1 — inside the emoji.
        let p = para(
            ParagraphKind::Body,
            "😀x",
            vec![span(1, |s| s.bold = true), span(2, |_| {})],
        );
        let html = render_paragraphs(&[p], &[]);
        assert!(!html.contains('\u{FFFD}'), "{html}");
        assert!(html.contains("😀"), "{html}");
    }

    /// The review's finding 3: the CREATE path flattens hashtag spans to
    /// their text — a new record cannot carry another note's attachment.
    #[test]
    fn objects_as_text_parse_flattens_hashtag_spans() {
        let html = r#"<div>tag <span data-jodd-inline="hashtag" data-ref="abc-123">#work</span>!</div>"#;
        let parsed = parse_editor_html_objects_as_text(html);
        assert_eq!(parsed.text, "tag #work!");
        assert!(parsed.hashtags.is_empty());
        assert!(!parsed.text.contains('\u{FFFC}'));
    }

    #[test]
    fn a_link_parses_its_href_and_bold_state_nests() {
        let parsed = parse_editor_html(r#"<div><b>x<a href="https://j.co">y</a></b></div>"#);
        let spans = &parsed.paragraphs[0].spans;
        assert_eq!(spans.len(), 2);
        assert!(spans[0].style.bold && spans[0].style.link.is_empty());
        assert!(spans[1].style.bold && spans[1].style.link == "https://j.co");
    }
}
