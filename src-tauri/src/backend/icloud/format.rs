//! The semantic formatting model between Apple's attribute runs and Jodd's
//! editor HTML — M3's Component F1, ported from icloud-md's `noteFormat.ts`
//! (MIT, github.com/coddingtonbear/icloud-md), the same provenance as
//! `crdt.rs` and the vendored `proto/*.proto`.
//!
//! [`decode_note_format`] projects a note's `text + attribute_run` table onto
//! the dimensions Jodd renders: paragraph kind (title/heading/subheading/
//! body/monospaced/lists/todo), list nesting depth, blockquote level, todo
//! state, and the inline bold/italic/strikethrough/underline/link spans.
//! Everything else Apple can express — color, emphasis, superscript,
//! alignment, fonts, per-paragraph uuids — is deliberately NOT part of the
//! model: those fields render as plain text, stay byte-preserved on
//! untouched runs, and are carried through rewritten runs by the reconciler's
//! clone-overlay (`format_reconcile.rs`).
//!
//! Wire values (confirmed on icloud-md's captured formatting-evolution
//! session, its own header comment): style 0=Title 1=Heading 2=Subheading
//! 3=Body (also written explicitly; absent `paragraphStyle` means Body too)
//! 4=Monospaced 100=bullet list 101=dash list 102=numbered list 103=checklist
//! (with `todo{uuid, done}`); `indent` is list nesting; `fontHints` bit 1 =
//! bold, bit 2 = italic; underline/strikethrough are plain flag fields;
//! `link` covers exactly the linked range.
//!
//! **All offsets and lengths are UTF-16 code units** (`compose::utf16_len`) —
//! the unit `AttributeRun.length` is measured to be in (776/776 notes).

use super::compose::utf16_len;
use super::gen::topotext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParagraphKind {
    Title,
    Heading,
    Subheading,
    Body,
    Monospaced,
    BulletList,
    DashList,
    NumberedList,
    TodoList,
}

/// `ParagraphKind` → the wire `ParagraphStyle.style` value.
pub fn style_code(kind: ParagraphKind) -> u32 {
    match kind {
        ParagraphKind::Title => 0,
        ParagraphKind::Heading => 1,
        ParagraphKind::Subheading => 2,
        ParagraphKind::Body => 3,
        ParagraphKind::Monospaced => 4,
        ParagraphKind::BulletList => 100,
        ParagraphKind::DashList => 101,
        ParagraphKind::NumberedList => 102,
        ParagraphKind::TodoList => 103,
    }
}

/// The wire `style` value → kind, `None` for a code this model has never
/// seen — the caller refuses rather than guesses ([`FormatUnsupported`]).
pub fn kind_of_style(style: u32) -> Option<ParagraphKind> {
    Some(match style {
        0 => ParagraphKind::Title,
        1 => ParagraphKind::Heading,
        2 => ParagraphKind::Subheading,
        3 => ParagraphKind::Body,
        4 => ParagraphKind::Monospaced,
        100 => ParagraphKind::BulletList,
        101 => ParagraphKind::DashList,
        102 => ParagraphKind::NumberedList,
        103 => ParagraphKind::TodoList,
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub underline: bool,
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub style: InlineStyle,
    /// UTF-16 code units.
    pub length: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Paragraph {
    pub kind: ParagraphKind,
    pub indent: i32,
    pub block_quote_level: u32,
    /// Meaningful only for [`ParagraphKind::TodoList`].
    pub done: bool,
    /// Meaningful only for [`ParagraphKind::NumberedList`]; 0 = default (1).
    pub start_number: u32,
    /// The line's text, no trailing `\n` (a `U+2028` soft break stays inside).
    pub text: String,
    pub spans: Vec<Span>,
    /// UTF-16 offset of the line start in the full document text.
    pub start: usize,
}

/// Why a note's formatting cannot be decoded. Reported, never guessed at —
/// the read path falls back to the plain rendering and the write path
/// downgrades to text-only (spec F6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatUnsupported {
    /// The run table covers more units than the text has: runs and text
    /// disagree structurally.
    RunsOvershootText,
    /// A `ParagraphStyle.style` value outside the measured map.
    UnknownStyle(u32),
}

impl std::fmt::Display for FormatUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatUnsupported::RunsOvershootText => {
                write!(f, "the note's formatting runs overshoot its text")
            }
            FormatUnsupported::UnknownStyle(code) => {
                write!(f, "the note uses a paragraph style ({code}) Jodd doesn't understand")
            }
        }
    }
}

/// Does the paragraph model apply to this text at all? The model splits on
/// `\n` only; `\r` and `U+2029` are separators it cannot carry, and every
/// consumer — the read render, the layer gate, the save-path reconcile —
/// must take the plain path for them TOGETHER, or the gate certifies a
/// segmentation the other layers don't use. One predicate, three callers.
pub fn model_applies(text: &str) -> bool {
    !text.contains('\r') && !text.contains('\u{2029}')
}

/// The inline dimensions of one run, exactly icloud-md's `inlineStyleOfRun`.
pub fn inline_style_of_run(run: &topotext::AttributeRun) -> InlineStyle {
    let hints = run.font_hints.unwrap_or(0);
    InlineStyle {
        bold: hints & 1 != 0,
        italic: hints & 2 != 0,
        strikethrough: run.strikethrough == Some(1),
        underline: run.underline == Some(1),
        link: run.link.clone().unwrap_or_default(),
    }
}

/// The kind a `paragraph_style` expresses. Absent style — and a style
/// carrying only e.g. an indent, with no `style` field — both mean Body
/// (icloud-md, confirmed on the wire). `Err` for an unknown code.
fn paragraph_kind_of(ps: Option<&topotext::ParagraphStyle>) -> Result<ParagraphKind, FormatUnsupported> {
    let Some(ps) = ps else { return Ok(ParagraphKind::Body) };
    let Some(code) = ps.style else { return Ok(ParagraphKind::Body) };
    kind_of_style(code).ok_or(FormatUnsupported::UnknownStyle(code))
}

/// One run's covering interval, in UTF-16 units.
struct RunInterval<'a> {
    run: &'a topotext::AttributeRun,
    start: usize,
    end: usize,
}

/// Splits a note's text into per-line paragraphs and derives each one's
/// paragraph attributes and inline spans from the attribute runs covering it.
///
/// Paragraph attributes come from the run covering the line's trailing
/// newline (a paragraph's style flows through its newline — confirmed on
/// icloud-md's captures), falling back to the line's last character for the
/// final, unterminated line. Runs may span multiple lines (Apple merges
/// adjacent equal runs freely).
///
/// The run table may UNDER-cover the text (tolerated: uncovered text is
/// plain Body — same policy as icloud-md's `decodeNoteEmbedSlots`) but must
/// not overshoot it — that means the runs and text disagree structurally,
/// and the note is refused rather than guessed at.
pub fn decode_note_format(
    text: &str,
    runs: &[topotext::AttributeRun],
) -> Result<Vec<Paragraph>, FormatUnsupported> {
    let text_len = utf16_len(text);
    let covered: u64 = runs.iter().map(|r| r.length as u64).sum();
    if covered > text_len as u64 {
        return Err(FormatUnsupported::RunsOvershootText);
    }
    for run in runs {
        // Surface an unknown style up front, whichever line it lands on.
        paragraph_kind_of(run.paragraph_style.as_ref())?;
    }

    let mut intervals: Vec<RunInterval> = Vec::with_capacity(runs.len());
    {
        let mut offset = 0usize;
        for run in runs {
            if run.length > 0 {
                intervals.push(RunInterval { run, start: offset, end: offset + run.length as usize });
            }
            offset += run.length as usize;
        }
    }
    // Intervals are sorted and disjoint by construction, so binary search —
    // a linear scan restarted per query made this O(runs²) per note, on the
    // zone walk, against a measured worst case of 1426 runs on one note.
    let run_at = |unit: usize| -> Option<&RunInterval> {
        let i = intervals.partition_point(|iv| iv.end <= unit);
        intervals.get(i).filter(|iv| unit >= iv.start)
    };

    let lines: Vec<&str> = text.split('\n').collect();
    let mut paragraphs = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let line_start = offset;
        let line_len = utf16_len(line);
        let line_end = line_start + line_len;
        let has_newline = index < lines.len() - 1;
        offset = line_end + usize::from(has_newline);

        let anchor_index = if has_newline { Some(line_end) } else { line_end.checked_sub(1) };
        let anchor_run = anchor_index
            .filter(|&i| i >= line_start || has_newline)
            .and_then(run_at)
            .map(|iv| iv.run);
        let ps = anchor_run.and_then(|r| r.paragraph_style.as_ref());
        // Unknown codes were refused above, so this cannot fail here.
        let kind = paragraph_kind_of(ps).unwrap_or(ParagraphKind::Body);

        let mut spans: Vec<Span> = Vec::new();
        let mut at = line_start;
        while at < line_end {
            let (style, span_end) = match run_at(at) {
                Some(iv) => (inline_style_of_run(iv.run), iv.end.min(line_end)),
                None => (InlineStyle::default(), line_end),
            };
            let length = span_end - at;
            match spans.last_mut() {
                Some(previous) if previous.style == style => previous.length += length,
                _ => spans.push(Span { style, length }),
            }
            at = span_end;
        }

        paragraphs.push(Paragraph {
            kind,
            indent: ps.and_then(|p| p.indent).unwrap_or(0),
            block_quote_level: ps.and_then(|p| p.block_quote_level).unwrap_or(0),
            done: kind == ParagraphKind::TodoList
                && ps.and_then(|p| p.todo.as_ref()).map(|t| t.done == 1).unwrap_or(false),
            start_number: ps.and_then(|p| p.starting_list_item_number).unwrap_or(0),
            text: (*line).to_string(),
            spans,
            start: line_start,
        });
    }
    Ok(paragraphs)
}

// ── round-trip projection ───────────────────────────────────────────────

/// Dash lists collapse into bullet lists in the projection — Apple's own
/// clients keep style 101 on unchanged dash paragraphs, and the reconciler
/// must never "fix" one to 100 just because the projection folds them.
pub fn projected_kind(kind: ParagraphKind) -> ParagraphKind {
    if kind == ParagraphKind::DashList { ParagraphKind::BulletList } else { kind }
}

pub fn is_list_kind(kind: ParagraphKind) -> bool {
    matches!(
        kind,
        ParagraphKind::BulletList | ParagraphKind::DashList | ParagraphKind::NumberedList | ParagraphKind::TodoList
    )
}

/// The effective first number a rendered numbered list carries — only
/// compared at the start of a run of numbered items.
pub(super) fn effective_start(start_number: u32) -> u32 {
    if start_number == 0 { 1 } else { start_number }
}

fn utf16_slice(s: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().skip(start).take(end.saturating_sub(start)).collect();
    String::from_utf16_lossy(&units)
}

fn unit_is_whitespace(u: u16) -> bool {
    char::from_u32(u as u32).map(|c| c.is_whitespace()).unwrap_or(false)
}

fn merge_adjacent_equal_spans(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        match out.last_mut() {
            Some(previous) if previous.style == span.style => previous.length += span.length,
            _ => out.push(span),
        }
    }
    out
}

/// Per-unit sweep turning bold/italic/strikethrough off on whitespace at the
/// edges of each styled interval. Underline (`<u>`, no flanking rules) and
/// links keep their exact extents — icloud-md's
/// `trimDelimiterStylesOffWhitespace`, unit for unit.
fn trim_delimiter_styles_off_whitespace(text: &str, spans: Vec<Span>) -> Vec<Span> {
    let any_delimited = spans.iter().any(|s| s.style.bold || s.style.italic || s.style.strikethrough);
    if spans.is_empty() || !any_delimited {
        return spans;
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    // Expand to one style per unit.
    let mut styles: Vec<InlineStyle> = Vec::with_capacity(units.len());
    for span in &spans {
        for _ in 0..span.length {
            styles.push(span.style.clone());
        }
    }
    let dims: [fn(&InlineStyle) -> bool; 3] =
        [|s| s.bold, |s| s.italic, |s| s.strikethrough];
    let sets: [fn(&mut InlineStyle, bool); 3] = [
        |s, v| s.bold = v,
        |s, v| s.italic = v,
        |s, v| s.strikethrough = v,
    ];
    for (get, set) in dims.iter().zip(sets.iter()) {
        let mut i = 0usize;
        while i < styles.len() {
            if !get(&styles[i]) {
                i += 1;
                continue;
            }
            let mut end = i;
            while end < styles.len() && get(&styles[end]) {
                end += 1;
            }
            let mut k = i;
            while k < end && units.get(k).copied().map(unit_is_whitespace).unwrap_or(false) {
                set(&mut styles[k], false);
                k += 1;
            }
            let mut k = end;
            while k > i && units.get(k - 1).copied().map(unit_is_whitespace).unwrap_or(false) {
                set(&mut styles[k - 1], false);
                k -= 1;
            }
            i = end;
        }
    }
    merge_adjacent_equal_spans(styles.into_iter().map(|style| Span { style, length: 1 }).collect())
}

/// A paragraph's spans in canonical projection form (icloud-md's
/// `normalizeSpans`): dash≡bullet is handled by [`projected_kind`], a link
/// whose target is exactly its own covered text collapses to "not a link"
/// (Apple auto-links bare URLs — "removing" one on write would be a phantom
/// change), monospaced paragraphs drop inline styling entirely, and the
/// delimiter-notated styles retreat off whitespace at their edges.
pub fn normalize_spans(paragraph: &Paragraph) -> Vec<Span> {
    let text_len = utf16_len(&paragraph.text);
    if paragraph.kind == ParagraphKind::Monospaced {
        return if text_len == 0 {
            Vec::new()
        } else {
            vec![Span { style: InlineStyle::default(), length: text_len }]
        };
    }
    let mut out: Vec<Span> = Vec::new();
    let mut at = 0usize;
    for span in &paragraph.spans {
        let covered = utf16_slice(&paragraph.text, at, at + span.length);
        at += span.length;
        let mut style = span.style.clone();
        if style.link == covered {
            style.link = String::new();
        }
        match out.last_mut() {
            Some(previous) if previous.style == style => previous.length += span.length,
            _ => out.push(Span { style, length: span.length }),
        }
    }
    // Second effective-link pass: adjacent runs each carrying the full URL as
    // their link merge above only if their raw attrs matched; re-check whether
    // the merged span now covers exactly its link text.
    let mut start = 0usize;
    for span in &mut out {
        if !span.style.link.is_empty()
            && utf16_slice(&paragraph.text, start, start + span.length) == span.style.link
        {
            span.style.link = String::new();
        }
        start += span.length;
    }
    merge_adjacent_equal_spans(trim_delimiter_styles_off_whitespace(&paragraph.text, out))
}

/// Trailing horizontal whitespace is invisible in Apple Notes, so it is not
/// part of the projection: non-monospaced paragraphs compare with trailing
/// spaces/tabs removed and spans shrunk to cover exactly the trimmed text.
/// Monospaced paragraphs keep theirs.
pub fn trim_trailing_whitespace(paragraph: &Paragraph) -> Paragraph {
    if paragraph.kind == ParagraphKind::Monospaced {
        return paragraph.clone();
    }
    let text = paragraph.text.trim_end_matches([' ', '\t']);
    if text.len() == paragraph.text.len() {
        return paragraph.clone();
    }
    let text_len = utf16_len(text);
    let mut spans = Vec::new();
    let mut remaining = text_len;
    for span in &paragraph.spans {
        if remaining == 0 {
            break;
        }
        let length = span.length.min(remaining);
        spans.push(Span { style: span.style.clone(), length });
        remaining -= length;
    }
    Paragraph { text: text.to_string(), spans, ..paragraph.clone() }
}

/// Whether two paragraphs at the same position agree on every dimension this
/// projection renders. The previous paragraph on each side feeds the
/// numbered-list group-start rule: only a group's first item compares its
/// effective start number, since later items re-derive theirs by counting.
pub fn paragraph_projections_equal(
    raw_a: &Paragraph,
    raw_b: &Paragraph,
    previous_a: Option<&Paragraph>,
    previous_b: Option<&Paragraph>,
) -> bool {
    let a = trim_trailing_whitespace(raw_a);
    let b = trim_trailing_whitespace(raw_b);
    if projected_kind(a.kind) != projected_kind(b.kind) || a.text != b.text {
        return false;
    }
    if a.block_quote_level != b.block_quote_level {
        return false;
    }
    if is_list_kind(a.kind) && a.indent != b.indent {
        return false;
    }
    if a.kind == ParagraphKind::TodoList && a.done != b.done {
        return false;
    }
    if a.kind == ParagraphKind::NumberedList {
        let starts_group = |p: Option<&Paragraph>, me: &Paragraph| {
            p.map(|prev| prev.kind != ParagraphKind::NumberedList || prev.indent != me.indent)
                .unwrap_or(true)
        };
        let sa = starts_group(previous_a, &a);
        let sb = starts_group(previous_b, &b);
        if sa != sb {
            return false;
        }
        if sa && effective_start(a.start_number) != effective_start(b.start_number) {
            return false;
        }
    }
    let spans_a = normalize_spans(&a);
    let spans_b = normalize_spans(&b);
    spans_a == spans_b
}

/// Whole-document projection equality — the round-trip gate: dimensions the
/// projection deliberately does not render never participate.
pub fn formats_round_trip_equal(a: &[Paragraph], b: &[Paragraph]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for i in 0..a.len() {
        let prev_a = if i > 0 { Some(&a[i - 1]) } else { None };
        let prev_b = if i > 0 { Some(&b[i - 1]) } else { None };
        if !paragraph_projections_equal(&a[i], &b[i], prev_a, prev_b) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_len(len: u32) -> topotext::AttributeRun {
        topotext::AttributeRun { length: len, ..Default::default() }
    }
    fn styled_run(len: u32, style: u32) -> topotext::AttributeRun {
        topotext::AttributeRun {
            length: len,
            paragraph_style: Some(topotext::ParagraphStyle { style: Some(style), ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn absent_paragraph_style_means_body_and_absent_runs_mean_body() {
        let ps = decode_note_format("Title\nbody", &[run_len(10)]).unwrap();
        assert_eq!(ps.len(), 2);
        assert!(ps.iter().all(|p| p.kind == ParagraphKind::Body));
        assert_eq!(ps[1].start, 6);
        // An UNDER-covering table is tolerated: uncovered text is plain Body.
        let ps = decode_note_format("Title\nbody", &[run_len(3)]).unwrap();
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[1].kind, ParagraphKind::Body);
    }

    #[test]
    fn paragraph_state_anchors_on_the_trailing_newline_run() {
        // Run 1 covers "Hea" (heading), run 2 covers "\nb": the newline is in
        // the Body run, so the LINE is Body — a paragraph's style flows
        // through its newline.
        let ps = decode_note_format("Hea\nb", &[styled_run(3, 1), run_len(2)]).unwrap();
        assert_eq!(ps[0].kind, ParagraphKind::Body);
        // Run 1 covers "Hea\n" — the newline carries Heading.
        let ps = decode_note_format("Hea\nb", &[styled_run(4, 1), run_len(1)]).unwrap();
        assert_eq!(ps[0].kind, ParagraphKind::Heading);
        // The final unterminated line anchors on its LAST character.
        assert_eq!(ps[1].kind, ParagraphKind::Body);
    }

    #[test]
    fn every_measured_style_code_maps_and_unknown_codes_refuse() {
        for (code, kind) in [
            (0, ParagraphKind::Title),
            (1, ParagraphKind::Heading),
            (2, ParagraphKind::Subheading),
            (3, ParagraphKind::Body),
            (4, ParagraphKind::Monospaced),
            (100, ParagraphKind::BulletList),
            (101, ParagraphKind::DashList),
            (102, ParagraphKind::NumberedList),
            (103, ParagraphKind::TodoList),
        ] {
            let ps = decode_note_format("x", &[styled_run(1, code)]).unwrap();
            assert_eq!(ps[0].kind, kind, "style {code}");
            assert_eq!(style_code(kind), code);
        }
        assert_eq!(
            decode_note_format("x", &[styled_run(1, 7)]),
            Err(FormatUnsupported::UnknownStyle(7))
        );
    }

    #[test]
    fn overshooting_runs_refuse_rather_than_guess() {
        assert_eq!(decode_note_format("ab", &[run_len(5)]), Err(FormatUnsupported::RunsOvershootText));
    }

    #[test]
    fn inline_spans_split_on_style_change_and_merge_when_equal() {
        let bold = topotext::AttributeRun { length: 4, font_hints: Some(1), ..Default::default() };
        let ps = decode_note_format("bold plain", &[bold, run_len(6)]).unwrap();
        assert_eq!(ps[0].spans.len(), 2);
        assert!(ps[0].spans[0].style.bold);
        assert_eq!(ps[0].spans[0].length, 4);
        assert!(!ps[0].spans[1].style.bold);

        let all = topotext::AttributeRun {
            length: 1,
            font_hints: Some(3),
            underline: Some(1),
            strikethrough: Some(1),
            link: Some("https://x".into()),
            ..Default::default()
        };
        let s = inline_style_of_run(&all);
        assert!(s.bold && s.italic && s.underline && s.strikethrough);
        assert_eq!(s.link, "https://x");
    }

    #[test]
    fn offsets_are_utf16_units_thai_is_one_emoji_is_two() {
        let ps = decode_note_format("ไทย😀\nb", &[run_len(7)]).unwrap();
        assert_eq!(ps[0].text, "ไทย😀");
        assert_eq!(ps[1].start, 6);
    }

    // ── normalization + projection equality (Task 2) ────────────────────

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
    fn a_bare_url_link_normalizes_to_plain_text() {
        let p = para(
            ParagraphKind::Body,
            "https://x.co",
            vec![span(12, |s| s.link = "https://x.co".into())],
        );
        let n = normalize_spans(&p);
        assert_eq!(n.len(), 1);
        assert!(n[0].style.link.is_empty());
    }

    #[test]
    fn adjacent_bare_url_pieces_collapse_via_the_second_pass() {
        // Two spans, each carrying the full URL as its link but covering only
        // half of it: neither half equals the link alone; merged they do.
        let p = para(
            ParagraphKind::Body,
            "https://x.co",
            vec![span(6, |s| s.link = "https://x.co".into()), span(6, |s| s.link = "https://x.co".into())],
        );
        let n = normalize_spans(&p);
        assert_eq!(n.len(), 1);
        assert!(n[0].style.link.is_empty());
    }

    #[test]
    fn delimiter_styles_retreat_off_edge_whitespace_but_underline_keeps_its_extent() {
        let p = para(ParagraphKind::Body, " b ", vec![span(3, |s| {
            s.bold = true;
            s.underline = true;
        })]);
        let n = normalize_spans(&p);
        assert_eq!(n.iter().map(|s| s.length).collect::<Vec<_>>(), vec![1, 1, 1]);
        assert!(!n[0].style.bold && n[0].style.underline);
        assert!(n[1].style.bold);
        assert!(!n[2].style.bold && n[2].style.underline);
    }

    #[test]
    fn monospaced_paragraphs_drop_inline_styling_entirely() {
        let p = para(ParagraphKind::Monospaced, "code", vec![span(4, |s| s.bold = true)]);
        let n = normalize_spans(&p);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].style, InlineStyle::default());
        assert!(normalize_spans(&para(ParagraphKind::Monospaced, "", vec![])).is_empty());
    }

    #[test]
    fn trailing_whitespace_is_not_part_of_the_projection_except_monospaced() {
        let p = para(ParagraphKind::Body, "hi  ", vec![span(4, |_| {})]);
        let t = trim_trailing_whitespace(&p);
        assert_eq!(t.text, "hi");
        assert_eq!(t.spans.iter().map(|s| s.length).sum::<usize>(), 2);
        let m = para(ParagraphKind::Monospaced, "hi  ", vec![span(4, |_| {})]);
        assert_eq!(trim_trailing_whitespace(&m).text, "hi  ");
    }

    #[test]
    fn dash_and_bullet_lists_project_equal_but_todo_done_does_not() {
        let dash = para(ParagraphKind::DashList, "item", vec![span(4, |_| {})]);
        let bullet = Paragraph { kind: ParagraphKind::BulletList, ..dash.clone() };
        assert!(paragraph_projections_equal(&dash, &bullet, None, None));
        let todo = Paragraph { kind: ParagraphKind::TodoList, ..dash.clone() };
        let done = Paragraph { done: true, ..todo.clone() };
        assert!(!paragraph_projections_equal(&todo, &done, None, None));
    }

    #[test]
    fn only_a_numbered_groups_first_item_compares_start_numbers() {
        let one = Paragraph {
            kind: ParagraphKind::NumberedList,
            start_number: 5,
            ..para(ParagraphKind::NumberedList, "i", vec![span(1, |_| {})])
        };
        let other = Paragraph { start_number: 0, ..one.clone() };
        assert!(!paragraph_projections_equal(&one, &other, None, None));
        assert!(paragraph_projections_equal(&one, &other, Some(&one), Some(&one)));
    }

    #[test]
    fn formats_round_trip_equal_compares_whole_documents() {
        let a = vec![para(ParagraphKind::Heading, "T", vec![span(1, |_| {})])];
        let b = vec![para(ParagraphKind::Heading, "T", vec![span(1, |_| {})])];
        assert!(formats_round_trip_equal(&a, &b));
        assert!(!formats_round_trip_equal(&a, &[]));
    }

    #[test]
    fn todo_indent_quote_and_start_number_are_read_off_the_anchor_run() {
        let run = topotext::AttributeRun {
            length: 1,
            paragraph_style: Some(topotext::ParagraphStyle {
                style: Some(103),
                indent: Some(2),
                block_quote_level: Some(1),
                todo: Some(topotext::Todo { todo_uuid: vec![9; 16], done: 1 }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let ps = decode_note_format("x", &[run]).unwrap();
        assert_eq!(ps[0].kind, ParagraphKind::TodoList);
        assert!(ps[0].done);
        assert_eq!(ps[0].indent, 2);
        assert_eq!(ps[0].block_quote_level, 1);
    }
}
