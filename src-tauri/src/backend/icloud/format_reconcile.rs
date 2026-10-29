//! The write-side formatting reconciler — M3's F3, ported function for
//! function from icloud-md's `formatReconcile.ts` (MIT), the same provenance
//! as `crdt.rs` and `format.rs`.
//!
//! Given a note document whose *text* has already been brought to the
//! desired state (`with_text_crdt` / `with_text`), rewrites the attribute
//! runs of every paragraph whose rendered projection differs from the
//! desired model, and applies the corresponding formatting op to the CRDT
//! layer (`crdt::apply_formatting_op` — op-clock bump + anchor restamp).
//!
//! Edits are **clone-overlay**: untouched paragraphs keep their attribute
//! runs verbatim; a changed paragraph's runs are split at paragraph/span
//! boundaries, each piece cloned from its underlying run, and only the
//! fields whose rendered projection actually differs are overlaid.
//! Everything the projection doesn't render — colors, emphasis, fonts on
//! unchanged bold spans, non-list indents, dash-list style values, bare-URL
//! link fields, `attachmentInfo`, paragraph uuids — rides along untouched.
//! This is gotcha #24's opaque-preservation doctrine extended to formatting.
//!
//! One deliberate omission from the reference: icloud-md's
//! `hasExplicitZeroStart` repair SCAN is its own early bug's cleanup and is
//! not ported; the omit-when-default `startingListItemNumber` rule it
//! enforces IS (both in the overlay's repair clause and in the fresh-style
//! shape).

use super::compose::{self, utf16_len};
use super::crdt;
use super::format::{
    decode_note_format, effective_start, is_list_kind, normalize_spans,
    paragraph_projections_equal, projected_kind, style_code, InlineStyle, Paragraph,
    ParagraphKind,
};
use super::gen::topotext;
use prost::Message;
use std::collections::{BTreeSet, HashSet};

/// Why the formatting half of a save was refused. The save then proceeds
/// **text-only** — M2's behavior, spec F6's downgrade rule — never blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatRefusal {
    /// The document's CURRENT formatting can't be decoded, so there is
    /// nothing sound to diff against.
    CurrentFormatUndecodable(String),
    /// The decoded paragraphs don't line up with the desired ones — the
    /// text edit and the format model disagree, and guessing which is right
    /// is how formatting gets written onto the wrong lines.
    ParagraphMismatch,
}

impl std::fmt::Display for FormatRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatRefusal::CurrentFormatUndecodable(reason) => {
                write!(f, "the note's current formatting can't be decoded: {reason}")
            }
            FormatRefusal::ParagraphMismatch => write!(
                f,
                "the note's paragraphs don't line up with the edited text — refusing to guess"
            ),
        }
    }
}

#[derive(Debug, Clone)]
struct Interval {
    start: usize,
    end: usize,
    style: InlineStyle,
}

struct Plan {
    current: Paragraph,
    desired: Paragraph,
    start: usize,
    end: usize,
    current_spans: Vec<Interval>,
    desired_spans: Vec<Interval>,
    todo_uuid: [u8; 16],
    force_fresh_todo_uuid: bool,
    /// The todo uuid the paragraph ALREADY carries (all of a paragraph's
    /// runs share one) — a piece whose own run lacks a `paragraphStyle`
    /// must inherit this rather than mint a second identity for the same
    /// line, which Apple's per-uuid check-state merge would read as two
    /// different checkboxes.
    existing_todo_uuid: Option<Vec<u8>>,
}

/// Reconciles `doc`'s formatting to `desired`. `doc.text()` must already
/// equal the `\n`-join of the desired texts — the text splice runs first,
/// this second. Returns whether anything changed. `mint_uuid` supplies todo
/// identities (injected so tests are deterministic).
pub fn reconcile_note_format(
    doc: &mut compose::NoteDocument,
    desired: &[Paragraph],
    replica_id: [u8; 16],
    mint_uuid: &mut dyn FnMut() -> [u8; 16],
) -> Result<bool, FormatRefusal> {
    let current = decode_note_format(doc.text(), &doc.string.attribute_run)
        .map_err(|e| FormatRefusal::CurrentFormatUndecodable(e.to_string()))?;
    if current.len() != desired.len() {
        return Err(FormatRefusal::ParagraphMismatch);
    }
    for i in 0..desired.len() {
        if current[i].text != desired[i].text {
            return Err(FormatRefusal::ParagraphMismatch);
        }
    }

    // Todo identity dedup: a line inserted next to a checklist item inherits
    // that item's attribute run wholesale (`adjust_attribute_runs`), todo
    // uuid included — and two checklist items must never share an identity
    // (Apple merges check-state per uuid). Every later duplicate re-mints,
    // matching Apple's own client.
    let mut uuid_owners: HashSet<Vec<u8>> = HashSet::new();
    let mut needs_fresh_todo_uuid: HashSet<usize> = HashSet::new();
    for (i, paragraph) in current.iter().enumerate() {
        if paragraph.kind != ParagraphKind::TodoList || desired[i].kind != ParagraphKind::TodoList {
            continue;
        }
        let is_last = i == current.len() - 1;
        let Some(uuid) = todo_uuid_of_paragraph(doc, paragraph, is_last) else { continue };
        if uuid.is_empty() {
            continue;
        }
        if !uuid_owners.insert(uuid) {
            needs_fresh_todo_uuid.insert(i);
        }
    }

    let mut changed_indexes: Vec<usize> = Vec::new();
    for i in 0..desired.len() {
        let prev_c = if i > 0 { Some(&current[i - 1]) } else { None };
        let prev_d = if i > 0 { Some(&desired[i - 1]) } else { None };
        if needs_fresh_todo_uuid.contains(&i)
            || !paragraph_projections_equal(&current[i], &desired[i], prev_c, prev_d)
        {
            changed_indexes.push(i);
        }
    }
    if changed_indexes.is_empty() {
        return Ok(false);
    }

    let plans: Vec<Plan> = changed_indexes
        .iter()
        .map(|&i| {
            build_paragraph_plan(
                &current[i],
                &desired[i],
                i == desired.len() - 1,
                needs_fresh_todo_uuid.contains(&i),
                mint_uuid(),
                todo_uuid_of_paragraph(doc, &current[i], i == desired.len() - 1),
            )
        })
        .collect();

    let new_runs = rewrite_attribute_runs(&doc.string.attribute_run, &plans);
    let ranges: Vec<(usize, usize)> = plans.iter().map(|p| (p.start, p.end)).collect();
    match doc.crdt.take() {
        Some(mut crdt_doc) => {
            // The CRDT document owns the canonical attribute runs — encode
            // rebuilds `doc.string` from it, so the rewrite must land there
            // before the restamp re-encodes.
            crdt_doc.attribute_runs = new_runs;
            crdt::apply_formatting_op(&mut crdt_doc, &ranges, replica_id);
            if let Err(e) = crdt::validate_document_invariants(&crdt_doc) {
                // Restore the identity before refusing: leaving `crdt: None`
                // on a substring-carrying document hands any later dispatch
                // on `doc.crdt` the structurally-invalid `with_text` branch.
                // The mutated runs/anchors are NOT adopted into `doc.string`,
                // so the caller's downgrade still pushes the pre-reconcile
                // document.
                doc.crdt = Some(crdt_doc);
                return Err(FormatRefusal::CurrentFormatUndecodable(format!(
                    "post-edit invariants: {e}"
                )));
            }
            doc.string = crdt::encode_crdt_document(&crdt_doc);
            doc.crdt = Some(crdt_doc);
        }
        None => {
            doc.string.attribute_run = new_runs;
        }
    }
    Ok(true)
}

fn build_paragraph_plan(
    current: &Paragraph,
    desired: &Paragraph,
    is_last_paragraph: bool,
    force_fresh_todo_uuid: bool,
    todo_uuid: [u8; 16],
    existing_todo_uuid: Option<Vec<u8>>,
) -> Plan {
    let start = current.start;
    let end = start + utf16_len(&current.text) + usize::from(!is_last_paragraph);
    Plan {
        current: current.clone(),
        desired: desired.clone(),
        start,
        end,
        // Both sides' intervals are based at the paragraph's position in the
        // DOCUMENT (`current.start`), whatever offsets the desired model was
        // built with — the guard above proved the texts line up.
        current_spans: span_intervals(current, start, end),
        desired_spans: span_intervals(desired, start, end),
        todo_uuid,
        force_fresh_todo_uuid,
        existing_todo_uuid,
    }
}

/// The todo uuid carried by the first run overlapping the paragraph's range
/// (all of a paragraph's runs share one), or `None` when none does.
fn todo_uuid_of_paragraph(
    doc: &compose::NoteDocument,
    paragraph: &Paragraph,
    is_last_paragraph: bool,
) -> Option<Vec<u8>> {
    let start = paragraph.start;
    let end = start + utf16_len(&paragraph.text) + usize::from(!is_last_paragraph);
    let mut offset = 0usize;
    for run in &doc.string.attribute_run {
        let run_start = offset;
        let run_end = offset + run.length as usize;
        offset = run_end;
        if run_start < end && run_end > start {
            if let Some(todo) = run.paragraph_style.as_ref().and_then(|ps| ps.todo.as_ref()) {
                return Some(todo.todo_uuid.clone());
            }
        }
    }
    None
}

/// A paragraph's normalized spans as absolute `[start, end)` intervals based
/// at `base`; the trailing newline (and any uncovered tail) extends the last
/// span, or a plain span if the paragraph is empty — the newline belongs to
/// the paragraph and takes its final inline styling, matching captured runs.
fn span_intervals(paragraph: &Paragraph, base: usize, paragraph_end: usize) -> Vec<Interval> {
    let mut out: Vec<Interval> = Vec::new();
    let mut at = base;
    for span in normalize_spans(paragraph) {
        if span.length == 0 {
            continue;
        }
        out.push(Interval { start: at, end: at + span.length, style: span.style });
        at += span.length;
    }
    match out.last_mut() {
        Some(last) if last.end < paragraph_end => last.end = paragraph_end,
        Some(_) => {}
        None if paragraph_end > at => {
            out.push(Interval { start: at, end: paragraph_end, style: InlineStyle::default() })
        }
        None => {}
    }
    out
}

// ── attribute-run rewrite ───────────────────────────────────────────────

fn rewrite_attribute_runs(runs: &[topotext::AttributeRun], plans: &[Plan]) -> Vec<topotext::AttributeRun> {
    // Split boundaries: each changed paragraph's range edges plus both
    // sides' span boundaries within it. Runs outside every changed range
    // pass through untouched.
    let mut boundaries: BTreeSet<usize> = BTreeSet::new();
    for plan in plans {
        boundaries.insert(plan.start);
        boundaries.insert(plan.end);
        for interval in plan.current_spans.iter().chain(plan.desired_spans.iter()) {
            boundaries.insert(interval.start);
            boundaries.insert(interval.end);
        }
    }

    let mut out: Vec<topotext::AttributeRun> = Vec::new();
    // Only pieces minted here may merge (and be mutated) afterwards —
    // untouched original runs must stay verbatim.
    let mut rewritten: Vec<bool> = Vec::new();
    let mut offset = 0usize;
    for run in runs {
        let run_start = offset;
        let run_end = offset + run.length as usize;
        offset = run_end;
        let overlaps_any = plans.iter().any(|p| run_start < p.end && run_end > p.start);
        if !overlaps_any {
            out.push(run.clone());
            rewritten.push(false);
            continue;
        }
        // Cut the run at every boundary falling inside it, then overlay the
        // pieces that sit inside a changed paragraph.
        let mut cuts: Vec<usize> = vec![run_start];
        cuts.extend(boundaries.range((run_start + 1)..run_end).copied());
        cuts.push(run_end);
        for pair in cuts.windows(2) {
            let (piece_start, piece_end) = (pair[0], pair[1]);
            let mut piece = run.clone();
            piece.length = (piece_end - piece_start) as u32;
            if let Some(plan) = plans.iter().find(|p| piece_start >= p.start && piece_end <= p.end) {
                overlay_piece(&mut piece, plan, piece_start);
            }
            out.push(piece);
            rewritten.push(true);
        }
    }
    merge_encodable_equal_runs(out, &rewritten)
}

fn overlay_piece(piece: &mut topotext::AttributeRun, plan: &Plan, piece_start: usize) {
    overlay_paragraph_style(piece, plan);
    let current_style = style_at(&plan.current_spans, piece_start);
    let desired_style = style_at(&plan.desired_spans, piece_start);
    if current_style != desired_style {
        overlay_inline_style(piece, &current_style, &desired_style);
    }
}

fn style_at(intervals: &[Interval], at: usize) -> InlineStyle {
    intervals
        .iter()
        .find(|iv| at >= iv.start && at < iv.end)
        .map(|iv| iv.style.clone())
        .unwrap_or_default()
}

/// Overlays paragraph-level fields. When the paragraph's projected kind is
/// unchanged, the existing `paragraphStyle` is kept (dash lists stay style
/// 101, iOS uuids stay put) and only the differing managed fields are set;
/// a kind change replaces it with a fresh web-client-shape style.
fn overlay_paragraph_style(piece: &mut topotext::AttributeRun, plan: &Plan) {
    let (current, desired) = (&plan.current, &plan.desired);
    if projected_kind(current.kind) != projected_kind(desired.kind) {
        piece.paragraph_style = Some(fresh_paragraph_style(desired, piece, plan));
        return;
    }
    let need_indent = is_list_kind(desired.kind) && current.indent != desired.indent;
    let need_quote = current.block_quote_level != desired.block_quote_level;
    let need_done = desired.kind == ParagraphKind::TodoList && current.done != desired.done;
    let need_start = desired.kind == ParagraphKind::NumberedList
        && effective_start(current.start_number) != effective_start(desired.start_number);
    let need_identity = desired.kind == ParagraphKind::TodoList && plan.force_fresh_todo_uuid;
    let need_start_repair = piece
        .paragraph_style
        .as_ref()
        .is_some_and(|ps| ps.starting_list_item_number == Some(0));
    if !need_indent && !need_quote && !need_done && !need_start && !need_identity && !need_start_repair {
        return;
    }
    let ps = piece.paragraph_style.get_or_insert_with(|| topotext::ParagraphStyle {
        // A piece whose run had no explicit style gaining e.g. a blockquote
        // level or a done-toggle: write the PARAGRAPH's kind explicitly, the
        // way the web client writes Body for a body line. (icloud-md
        // hardcodes Body here; a ps-less piece inside a list/todo paragraph
        // would then carry style 3 mid-list — write the line's own kind.)
        style: Some(style_code(projected_kind(desired.kind))),
        alignment: Some(4),
        ..Default::default()
    });
    if need_indent {
        ps.indent = Some(desired.indent);
    }
    if need_quote {
        ps.block_quote_level = Some(desired.block_quote_level);
    }
    if need_start {
        set_start_number(ps, desired.start_number);
    }
    // An *explicit* startingListItemNumber of 0 makes Apple render the list
    // from 0 (its own client omits the field for the default of 1 —
    // icloud-md, live-verified), so any rewrite of the paragraph drops it.
    if ps.starting_list_item_number == Some(0) {
        ps.starting_list_item_number = None;
    }
    if need_identity {
        ps.todo = Some(topotext::Todo {
            todo_uuid: plan.todo_uuid.to_vec(),
            done: u32::from(desired.done),
        });
    } else if need_done {
        match ps.todo.as_mut() {
            Some(todo) => todo.done = u32::from(desired.done),
            // A piece whose run had no paragraphStyle at all: inherit the
            // PARAGRAPH's uuid so the line keeps one identity, minting only
            // when the whole paragraph has none.
            None => {
                ps.todo = Some(topotext::Todo {
                    todo_uuid: plan
                        .existing_todo_uuid
                        .clone()
                        .unwrap_or_else(|| plan.todo_uuid.to_vec()),
                    done: u32::from(desired.done),
                })
            }
        }
    }
}

/// A fresh paragraph style in the shape the captured web client writes:
/// fields stamped explicitly (zeros included), alignment 4 (Natural), uuid
/// empty. The exception is `startingListItemNumber`, which Apple's client
/// *omits* rather than zero-stamps — an explicit 0 renders the list starting
/// at 0 — so it's only written for a numbered group genuinely starting past
/// 1. A checklist paragraph keeps the run's existing todo uuid when it has
/// one (a done-toggle isn't an identity change), and otherwise gets the
/// plan's freshly minted one.
fn fresh_paragraph_style(
    desired: &Paragraph,
    piece: &topotext::AttributeRun,
    plan: &Plan,
) -> topotext::ParagraphStyle {
    topotext::ParagraphStyle {
        style: Some(style_code(projected_kind(desired.kind))),
        alignment: Some(4),
        writing_direction: Some(0),
        indent: Some(if is_list_kind(desired.kind) { desired.indent } else { 0 }),
        todo: (desired.kind == ParagraphKind::TodoList).then(|| topotext::Todo {
            // Keep the line's ONE identity: the piece's own run first (it
            // carries the paragraph's uuid when it has one), then the
            // paragraph-level uuid for ps-less pieces, minting only when
            // the whole paragraph has none (or a dedup forced a fresh one).
            todo_uuid: (!plan.force_fresh_todo_uuid)
                .then(|| {
                    piece
                        .paragraph_style
                        .as_ref()
                        .and_then(|ps| ps.todo.as_ref())
                        .map(|t| t.todo_uuid.clone())
                        .or_else(|| plan.existing_todo_uuid.clone())
                })
                .flatten()
                .unwrap_or_else(|| plan.todo_uuid.to_vec()),
            done: u32::from(desired.done),
        }),
        paragraph_hints: Some(0),
        starting_list_item_number: (desired.kind == ParagraphKind::NumberedList
            && effective_start(desired.start_number) != 1)
            .then_some(desired.start_number),
        block_quote_level: Some(desired.block_quote_level),
        uuid: Some(Vec::new()),
    }
}

/// Writes a numbered group's start, clearing the field for the default of 1
/// (matching Apple's omit-when-default shape) and setting it otherwise.
fn set_start_number(ps: &mut topotext::ParagraphStyle, start_number: u32) {
    if effective_start(start_number) == 1 {
        ps.starting_list_item_number = None;
    } else {
        ps.starting_list_item_number = Some(start_number);
    }
}

/// Overlays only the inline fields whose normalized projection differs —
/// bold/italic share `fontHints` (and get the explicit `Font` object the
/// captured web client writes alongside), the rest are plain flag/string
/// fields. Fields that agree keep whatever the underlying run had.
fn overlay_inline_style(piece: &mut topotext::AttributeRun, current: &InlineStyle, desired: &InlineStyle) {
    if current.bold != desired.bold || current.italic != desired.italic {
        piece.font_hints = Some(u32::from(desired.bold) | (u32::from(desired.italic) << 1));
        let font_name = match (desired.bold, desired.italic) {
            (true, true) => Some("SFUIText-BoldItalic"),
            (true, false) => Some("SFUIText-Bold"),
            (false, true) => Some("SFUIText-LightItalic"),
            (false, false) => None,
        };
        piece.font = font_name.map(|name| topotext::Font {
            name: Some(name.to_string()),
            ..Default::default()
        });
    }
    if current.strikethrough != desired.strikethrough {
        piece.strikethrough = Some(u32::from(desired.strikethrough));
    }
    if current.underline != desired.underline {
        piece.underline = Some(u32::from(desired.underline));
    }
    if current.link != desired.link {
        piece.link = Some(desired.link.clone());
    }
}

/// Merges back adjacent *rewritten* runs whose fields (other than length)
/// encode identically — the splitting above can produce runs of equal
/// formatting, and Apple's own saves freely normalize the run table the
/// same way. Untouched original runs never merge; an `attachment_info` run
/// never merges either.
fn merge_encodable_equal_runs(
    runs: Vec<topotext::AttributeRun>,
    rewritten: &[bool],
) -> Vec<topotext::AttributeRun> {
    let mut out: Vec<topotext::AttributeRun> = Vec::with_capacity(runs.len());
    let mut out_rewritten: Vec<bool> = Vec::with_capacity(runs.len());
    for (run, &was_rewritten) in runs.into_iter().zip(rewritten) {
        let can_merge = was_rewritten
            && out_rewritten.last() == Some(&true)
            && out.last().is_some_and(|previous| {
                previous.attachment_info.is_none()
                    && run.attachment_info.is_none()
                    && same_fields_ignoring_length(previous, &run)
            });
        if can_merge {
            out.last_mut().unwrap().length += run.length;
        } else {
            out.push(run);
            out_rewritten.push(was_rewritten);
        }
    }
    out
}

fn same_fields_ignoring_length(a: &topotext::AttributeRun, b: &topotext::AttributeRun) -> bool {
    let mut a = a.clone();
    let mut b = b.clone();
    a.length = 0;
    b.length = 0;
    a.encode_to_vec() == b.encode_to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::format::Span;

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
    fn span(len: usize, f: impl Fn(&mut InlineStyle)) -> Span {
        let mut s = InlineStyle::default();
        f(&mut s);
        Span { style: s, length: len }
    }

    /// Round-trips through the real codec so the fixture is exactly what the
    /// save path holds.
    fn doc_of(text: &str, runs: Vec<topotext::AttributeRun>) -> compose::NoteDocument {
        let d = compose::NoteDocument {
            string: topotext::String {
                string: text.to_string(),
                substring: Vec::new(),
                timestamp: None,
                attribute_run: runs,
            },
            serialization_version: Some(0),
            version_serialization_version: Some(0),
            minimum_supported_version: Some(0),
            crdt: None,
        };
        compose::parse(&compose::encode(&d)).unwrap()
    }

    fn fixed_uuid() -> impl FnMut() -> [u8; 16] {
        let mut n = 0u8;
        move || {
            n += 1;
            [n; 16]
        }
    }

    #[test]
    fn an_unchanged_projection_rewrites_nothing_and_untouched_runs_pass_verbatim() {
        let runs = vec![styled_run(5, 1), run_len(4)];
        let mut doc = doc_of("Head\nbody", runs.clone());
        let desired = decode_note_format("Head\nbody", &doc.string.attribute_run).unwrap();
        let mut mint = fixed_uuid();
        let changed = reconcile_note_format(&mut doc, &desired, [0xBB; 16], &mut mint).unwrap();
        assert!(!changed);
        assert_eq!(doc.string.attribute_run, runs, "nothing may move on a no-op");
    }

    #[test]
    fn bolding_a_word_rewrites_only_that_paragraphs_runs_and_unknown_fields_survive() {
        // One body line carrying a color nothing here reads; bold chars [0,4).
        let mut colored = run_len(9);
        colored.color = Some(topotext::Color { red: 1.0, green: 0.5, blue: 0.0, alpha: 1.0 });
        let mut doc = doc_of("bold rest", vec![colored.clone()]);
        let mut desired = decode_note_format("bold rest", &doc.string.attribute_run).unwrap();
        desired[0].spans = vec![
            span(4, |s| s.bold = true),
            span(5, |_| {}),
        ];
        let mut mint = fixed_uuid();
        reconcile_note_format(&mut doc, &desired, [0xBB; 16], &mut mint).unwrap();
        let out = &doc.string.attribute_run;
        let bold_piece = out.iter().find(|r| r.font_hints == Some(1)).expect("a bold piece");
        assert_eq!(bold_piece.color, colored.color, "clone-overlay must carry the color");
        assert_eq!(bold_piece.font.as_ref().unwrap().name.as_deref(), Some("SFUIText-Bold"));
        let total: u32 = out.iter().map(|r| r.length).sum();
        assert_eq!(total, 9, "runs still cover the text");
    }

    #[test]
    fn a_kind_change_writes_the_web_client_shape_fresh_style() {
        let mut doc = doc_of("line", vec![run_len(4)]);
        let mut desired = decode_note_format("line", &doc.string.attribute_run).unwrap();
        desired[0].kind = ParagraphKind::Heading;
        let mut mint = fixed_uuid();
        reconcile_note_format(&mut doc, &desired, [0xBB; 16], &mut mint).unwrap();
        let ps = doc.string.attribute_run[0].paragraph_style.as_ref().unwrap();
        assert_eq!(ps.style, Some(1));
        assert_eq!(ps.alignment, Some(4));
        assert_eq!(ps.starting_list_item_number, None, "omit-when-default");
        assert_eq!(ps.uuid.as_deref(), Some(&[][..]));
        assert_eq!(ps.paragraph_hints, Some(0));
    }

    #[test]
    fn a_dash_list_left_unchanged_is_never_rewritten_to_bullet() {
        let mut doc = doc_of("item", vec![styled_run(4, 101)]);
        let desired = decode_note_format("item", &doc.string.attribute_run).unwrap();
        let mut mint = fixed_uuid();
        let changed = reconcile_note_format(&mut doc, &desired, [0xBB; 16], &mut mint).unwrap();
        assert!(!changed);
        assert_eq!(doc.string.attribute_run[0].paragraph_style.as_ref().unwrap().style, Some(101));
    }

    #[test]
    fn two_checklist_rows_sharing_a_todo_uuid_get_the_duplicate_reminted() {
        let todo_run = |len| topotext::AttributeRun {
            length: len,
            paragraph_style: Some(topotext::ParagraphStyle {
                style: Some(103),
                todo: Some(topotext::Todo { todo_uuid: vec![7; 16], done: 0 }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut doc = doc_of("a\nb", vec![todo_run(2), todo_run(1)]);
        let desired = decode_note_format("a\nb", &doc.string.attribute_run).unwrap();
        let mut mint = fixed_uuid();
        reconcile_note_format(&mut doc, &desired, [0xBB; 16], &mut mint).unwrap();
        let uuids: Vec<_> = doc
            .string
            .attribute_run
            .iter()
            .filter_map(|r| r.paragraph_style.as_ref()?.todo.as_ref().map(|t| t.todo_uuid.clone()))
            .collect();
        assert_eq!(uuids.len(), 2);
        assert_ne!(uuids[0], uuids[1], "Apple merges check-state per uuid — duplicates are data loss");
    }

    /// The review's finding 6: a piece whose run carries no paragraphStyle
    /// inside a todo paragraph must inherit the PARAGRAPH's uuid on a
    /// done-toggle — a second identity on one line is data Apple's per-uuid
    /// check-state merge misreads. The fabricated style must also carry the
    /// line's own kind, not a hardcoded Body.
    #[test]
    fn a_ps_less_piece_in_a_todo_paragraph_inherits_the_lines_uuid_on_a_done_toggle() {
        let todo_run = topotext::AttributeRun {
            length: 1,
            paragraph_style: Some(topotext::ParagraphStyle {
                style: Some(103),
                todo: Some(topotext::Todo { todo_uuid: vec![7; 16], done: 0 }),
                ..Default::default()
            }),
            ..Default::default()
        };
        // "ab": the anchor (final char) run carries the todo; the FIRST run
        // has no paragraphStyle at all — tolerated on decode.
        let mut doc = doc_of("ab", vec![run_len(1), todo_run]);
        let mut desired = decode_note_format("ab", &doc.string.attribute_run).unwrap();
        assert_eq!(desired[0].kind, ParagraphKind::TodoList, "anchor decides the kind");
        desired[0].done = true;
        let mut mint = fixed_uuid();
        reconcile_note_format(&mut doc, &desired, [0xBB; 16], &mut mint).unwrap();
        let uuids: Vec<Vec<u8>> = doc
            .string
            .attribute_run
            .iter()
            .filter_map(|r| r.paragraph_style.as_ref()?.todo.as_ref().map(|t| t.todo_uuid.clone()))
            .collect();
        assert!(!uuids.is_empty());
        assert!(
            uuids.iter().all(|u| u == &vec![7u8; 16]),
            "the line must keep ONE identity, got {uuids:?}"
        );
        for r in &doc.string.attribute_run {
            if let Some(ps) = &r.paragraph_style {
                assert_ne!(ps.style, Some(3), "no fabricated Body style mid-checklist: {ps:?}");
            }
        }
    }

    #[test]
    fn paragraphs_that_do_not_line_up_refuse_rather_than_guess() {
        let mut doc = doc_of("one\ntwo", vec![run_len(7)]);
        let desired = decode_note_format("one", &[run_len(3)]).unwrap();
        let mut mint = fixed_uuid();
        assert_eq!(
            reconcile_note_format(&mut doc, &desired, [0xBB; 16], &mut mint),
            Err(FormatRefusal::ParagraphMismatch)
        );
    }

    #[test]
    fn an_undecodable_current_format_refuses_with_the_reason() {
        let mut doc = doc_of("x", vec![styled_run(1, 77)]);
        let desired = decode_note_format("x", &[run_len(1)]).unwrap();
        let mut mint = fixed_uuid();
        assert!(matches!(
            reconcile_note_format(&mut doc, &desired, [0xBB; 16], &mut mint),
            Err(FormatRefusal::CurrentFormatUndecodable(_))
        ));
    }

    #[test]
    fn a_crdt_carrying_document_gets_its_rewritten_ranges_restamped() {
        // The crdt fixture shape compose.rs's own tests use: origin run,
        // one content run, sentinel, one-replica clock table.
        let d = compose::NoteDocument {
            string: topotext::String {
                string: "Title\nbody".into(),
                substring: vec![
                    topotext::Substring {
                        char_id: topotext::CharId { replica_id: 0, clock: 0 },
                        length: 0,
                        timestamp: topotext::CharId { replica_id: 0, clock: 0 },
                        tombstone: None,
                        child: vec![1],
                    },
                    topotext::Substring {
                        char_id: topotext::CharId { replica_id: 1, clock: 0 },
                        length: 10,
                        timestamp: topotext::CharId { replica_id: 1, clock: 0 },
                        tombstone: None,
                        child: vec![2],
                    },
                    topotext::Substring {
                        char_id: topotext::CharId { replica_id: 0, clock: 0xFFFF_FFFF },
                        length: 0,
                        timestamp: topotext::CharId { replica_id: 0, clock: 0xFFFF_FFFF },
                        tombstone: None,
                        child: Vec::new(),
                    },
                ],
                timestamp: Some(topotext::VectorTimestamp {
                    clock: vec![topotext::vector_timestamp::Clock {
                        replica_uuid: vec![0xAA; 16],
                        replica_clock: vec![
                            topotext::vector_timestamp::clock::ReplicaClock { clock: 10, subclock: None },
                            topotext::vector_timestamp::clock::ReplicaClock { clock: 1, subclock: None },
                        ],
                    }],
                }),
                attribute_run: vec![run_len(10)],
            },
            serialization_version: Some(0),
            version_serialization_version: Some(0),
            minimum_supported_version: Some(0),
            crdt: None,
        };
        let mut doc = compose::writability(&compose::encode(&d), "Title").unwrap();
        assert!(doc.crdt.is_some(), "fixture must carry CRDT identity");
        let mut desired = decode_note_format(doc.text(), &doc.string.attribute_run).unwrap();
        desired[0].kind = ParagraphKind::Heading;
        let mut mint = fixed_uuid();
        reconcile_note_format(&mut doc, &desired, [0xBB; 16], &mut mint).unwrap();
        let crdt_doc = doc.crdt.as_ref().unwrap();
        crdt::validate_document_invariants(crdt_doc).unwrap();
        // The restamped run's anchor names the editing replica (index 1
        // after the editor-first reorder).
        assert!(
            crdt_doc.runs.iter().any(|r| r.length > 0 && r.anchor.replica == 1),
            "no run was restamped by the editor replica: {:?}",
            crdt_doc.runs
        );
        // And the rewrite survived the re-encode: the heading style is in
        // doc.string, and the runs still cover the text.
        assert!(doc
            .string
            .attribute_run
            .iter()
            .any(|r| r.paragraph_style.as_ref().and_then(|p| p.style) == Some(1)));
        assert!(compose::runs_cover(doc.text(), &doc.string.attribute_run));
    }
}
