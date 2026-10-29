//! A CloudKit note body → HTML.
//!
//! This is the half of the iCloud vertical that needed no live account to
//! build and none to verify, so it exists before the transport does.
//!
//! The path, end to end:
//!
//! ```text
//! Note.TextDataEncrypted
//!   → base64                       (the caller does this — it is a JSON string)
//!   → gzip *or* zlib               decompress_note_document
//!   → versioned_document.Document  → .version[0].data
//!   → topotext.String              → .string   = the visible text
//!   → HTML                         text_to_html
//! ```
//!
//! **`*Encrypted` is a misnomer on an account without Advanced Data
//! Protection.** It describes Apple's server-side at-rest encryption, not
//! anything a client must undo. `TitleEncrypted` and `SnippetEncrypted` are
//! base64 of plain text and never reach this module at all; only the body is
//! a compressed protobuf document, which is what makes a read-only M1 far
//! smaller than "port the content model" (docs/PRIOR-ART.md).
//!
//! **M1 decodes visible text only.** `topotext.String` also carries
//! `attribute_run` — bold, links, checklists, attachments, paragraph styles —
//! and M2 needs all of it. Text alone is a deliberate stopping point, not an
//! oversight: a read-only milestone cannot propagate lossiness back, and a
//! note that opens blank is indistinguishable from a sync bug.

use std::io::Read;

use super::gen::{topotext, versioned_document};
use prost::Message;

/// Why a note body could not be turned into text.
///
/// The variants are split by **what a caller should do about it**, not by
/// where in the pipeline the failure happened. `Unreadable` is the one that
/// matters: it is the shape an Advanced Data Protection note takes, where the
/// bytes are genuinely end-to-end encrypted and no amount of retrying or
/// re-fetching will help. The ADP verdict is built on top of this rather than
/// re-deriving compression magic numbers of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The bytes are not a compressed stream in either container Apple uses.
    /// On an account with ADP on, this is what genuinely-encrypted content
    /// looks like from out here.
    Unreadable(String),
    /// It decompressed, but what came out is not the document shape this
    /// backend understands. A real parse failure — a bug or a schema change,
    /// not an encrypted note.
    Malformed(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Unreadable(m) => write!(f, "unreadable note body: {m}"),
            DecodeError::Malformed(m) => write!(f, "malformed note document: {m}"),
        }
    }
}
impl std::error::Error for DecodeError {}

/// gzip's magic. **Both containers occur, from the same endpoint.**
///
/// This is the single most important fact in this file and it is measured,
/// not assumed: icloud-md's own `noteText.ts` records observing *both* gzip
/// (`1f 8b`) and zlib (`78 9c`) come back from the exact same `changes/zone`
/// response for different records, because the container is whatever the
/// client that last wrote the note used — it is not a property of the
/// endpoint, the account, or the record type.
///
/// docs/PRIOR-ART.md, the M1 handoff and this project's own design spec all
/// say "base64 → zlib", full stop. That is true of the one note the probe
/// happened to read. Treating it as the rule would mean every gzip-written
/// note fails to decompress — and, worse, fails in exactly the way
/// [`DecodeError::Unreadable`] describes, so the ADP check would refuse a
/// perfectly readable account at sign-in and tell the user their notes are
/// end-to-end encrypted.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// zlib's magic, for the same reason. Not used as a gate — anything that is
/// not gzip is attempted as zlib — but named so the pair reads together.
#[allow(dead_code)]
const ZLIB_MAGIC: [u8; 2] = [0x78, 0x9c];

/// Decompresses a note body, accepting either container.
///
/// Sniffs gzip and otherwise attempts zlib, rather than sniffing both and
/// rejecting anything else: zlib's header is a two-byte checksum-constrained
/// pair whose first byte varies with the compression level (`78 01`, `78 5e`,
/// `78 9c`, `78 da` are all valid), so a `78 9c` equality test would reject
/// real notes written at a different level. Let the decoder decide.
pub fn decompress_note_document(buf: &[u8]) -> Result<Vec<u8>, DecodeError> {
    if buf.len() < 2 {
        return Err(DecodeError::Unreadable(format!("{} byte(s) — too short to be a document", buf.len())));
    }
    let mut out = Vec::new();
    let res = if buf[..2] == GZIP_MAGIC {
        flate2::read::GzDecoder::new(buf).read_to_end(&mut out)
    } else {
        flate2::read::ZlibDecoder::new(buf).read_to_end(&mut out)
    };
    match res {
        Ok(_) => Ok(out),
        Err(e) => Err(DecodeError::Unreadable(format!(
            "neither gzip nor zlib ({e}) — first bytes {:02x?}",
            &buf[..buf.len().min(4)]
        ))),
    }
}

/// Unwraps `versioned_document.Document` down to the payload it carries.
///
/// Requires **exactly one** version. Apple's own captures always have one, and
/// a document with several is one this code does not understand — refusing is
/// the honest answer, where picking the first or the last would be a guess
/// that silently returns the wrong content. (icloud-md refuses here too, for
/// the same reason.)
fn unwrap_versioned_document(raw: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let doc = versioned_document::Document::decode(raw)
        .map_err(|e| DecodeError::Malformed(format!("versioned_document.Document: {e}")))?;
    if doc.version.len() != 1 {
        return Err(DecodeError::Malformed(format!(
            "document carries {} versions — only exactly one is understood",
            doc.version.len()
        )));
    }
    doc.version[0]
        .data
        .clone()
        .ok_or_else(|| DecodeError::Malformed("version carries no data payload".into()))
}

/// The visible text of a note — **title line included**.
///
/// Apple keeps no separate body: the note's first line *is* its title, and
/// `TitleEncrypted` on the record is derived from it. So what comes back here
/// starts with the title, and something downstream has to decide whether to
/// remove it (that is `strip_leading_title`, and it is the highest-risk
/// function in this milestone — see the design spec). This function
/// deliberately does not: its job is to report what the document says.
pub fn decode_note_text(compressed: &[u8]) -> Result<String, DecodeError> {
    let raw = decompress_note_document(compressed)?;
    let payload = unwrap_versioned_document(&raw)?;
    let s = topotext::String::decode(&payload[..])
        .map_err(|e| DecodeError::Malformed(format!("topotext.String: {e}")))?;
    Ok(s.string)
}

pub(super) fn escape_html_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Apple's **soft** line break, inside a paragraph — what Shift+Enter makes.
///
/// **Measured, not assumed** (2026-08-22, live account): a note displaying as
/// `two line 1` came back as `two line ` + `U+2028` + `1`, and its
/// `TitleEncrypted` was `two line 1` — the same characters with the separator
/// removed. So this codepoint really does occur inside body text, and Apple's
/// own title derivation normalizes it away.
///
/// It was found by `examples/icloud_probe`'s mismatch diagnosis, against code
/// that had already shipped with tests. Every one of those tests used `\n`,
/// which is gotcha #11's warning almost word for word: a tidy rule that three
/// samples confirm, and the falsifying case is the one nobody had made yet.
const LINE_SEPARATOR: char = '\u{2028}';

/// Unicode's PARAGRAPH SEPARATOR, `U+2029`.
///
/// **Not measured** — no note in the corpus carried one. Handled anyway
/// because the alternative is worse in both directions: it is a line break to
/// every Unicode-aware renderer, so leaving it in a `<div>` collapses it to
/// nothing and silently welds two lines together, exactly the failure
/// `LINE_SEPARATOR` just produced. Treated as a paragraph break, matching what
/// Unicode says it is.
const PARAGRAPH_SEPARATOR: char = '\u{2029}';

/// Plain text → the HTML shape a `contenteditable` produces.
///
/// One `<div>` per paragraph, an empty paragraph as `<div><br></div>`, and a
/// soft break inside a paragraph as `<br>`. That is what the editor itself
/// emits, so a note read from iCloud and a note typed in Jodd have the same
/// structure rather than one rendering subtly differently from the other.
/// `AppleHtmlDeriver` then runs over it unchanged, which is what gets FTS,
/// `#hashtags`, `[[wikilinks]]` and citations on this backend for free.
///
/// **Three separators, not one.** `U+000A` and `U+2029` end a paragraph;
/// `U+2028` breaks a line *within* one. Keeping that distinction is not
/// pedantry — collapsing them all to `<div>` would make M2 unable to tell a
/// soft break from a paragraph break when it writes back, and dropping
/// `U+2028` entirely (what this function did before 2026-08-22) welds two
/// lines into one on screen.
///
/// Escaping is not optional and not cosmetic: a note whose text contains
/// `<b>` or `&` — entirely ordinary in notes about code — would otherwise be
/// injected as live markup into the editor and into everything derived from
/// it.
///
/// `\r\n` is stripped rather than trusted: a stray `\r` would render as a
/// visible character and, once written back in M2, would differ from what came
/// down.
pub fn text_to_html(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    text.split(|c| c == '\n' || c == PARAGRAPH_SEPARATOR)
        .map(|para| {
            let para = para.strip_suffix('\r').unwrap_or(para);
            if para.is_empty() {
                return "<div><br></div>".to_string();
            }
            let inner = para
                .split(LINE_SEPARATOR)
                .map(escape_html_text)
                .collect::<Vec<_>>()
                .join("<br>");
            format!("<div>{inner}</div>")
        })
        .collect()
}

/// Unicode's OBJECT REPLACEMENT CHARACTER, `U+FFFC`.
///
/// **Measured 2026-08-22 on 776 real notes.** Apple puts one of these in
/// `topotext.String.string` wherever the note carries an inline object — an
/// attachment, a table, and (the case that bit) an inline **hashtag**. The
/// record's `TitleEncrypted` renders the same position as ordinary text, which
/// is how it was found: a note titled `this note with #this tag` has `#` at
/// index 15 of the title and `￼` at index 15 of the body.
const OBJECT_REPLACEMENT: char = '\u{FFFC}';

/// The ellipsis Apple appends when it truncates a title, `U+2026`.
///
/// **Measured**: an 86-character first line yields a 66-character title that
/// diverges at index 65 with this character; a 161-character line yields 67,
/// diverging at 66. So the cut lands somewhere around 65–66 characters and the
/// exact boundary is not worth pinning — what matters is recognising the shape.
const ELLIPSIS: char = '\u{2026}';

/// Why the body's first line was accepted as the title — or wasn't.
///
/// Every variant except [`TitleMatch::Unexplained`] is a **measured** cause of
/// `TitleEncrypted` differing from the line it was derived from. Keeping them
/// apart is what lets a future run count how often our model of Apple's
/// derivation has a gap, rather than reducing 776 notes to a yes/no.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleMatch {
    /// The line is the title, character for character. 520 of 776.
    Exact,
    /// The line starts with the title and continues. 41 of 776.
    Prefix,
    /// The title is a truncated prefix of the line plus [`ELLIPSIS`].
    Truncated,
    /// The line carries [`OBJECT_REPLACEMENT`], so a character comparison is
    /// meaningless — the title renders inline objects as text and the body
    /// does not.
    ContainsObject,
    /// The line matches the title once [`LINE_SEPARATOR`] is removed, which is
    /// what Apple's own derivation does.
    SoftBreakRemoved,
    /// None of the above. **The line is still cut** — Apple's model is that
    /// the first line IS the title — but this is the signal that the four
    /// causes above do not cover everything.
    Unexplained,
    /// The record carries no title, so there is nothing to remove and the
    /// whole text is body.
    NoTitle,
}

/// The body of a note, with its title line removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrippedBody {
    pub body: String,
    pub matched: TitleMatch,
}

/// Removes the title line from a decoded note body.
///
/// **This is the highest-risk function in M1** — the slot that produced
/// gotchas #11 and #17 on the email backends. What makes it tractable here is
/// that the rule was measured on 776 real notes rather than inferred from a
/// handful (docs/PRIOR-ART.md).
///
/// # The rule
///
/// **Cut through the first non-empty `\n`-delimited line.** Apple's model is
/// that a note's first line *is* its title; `TitleEncrypted` is derived from
/// that line, not the other way round. The title field is used only to
/// **verify**, never as the cut key.
///
/// That inversion is the whole design, and it is a deliberate reversal of what
/// this spec originally said ("if it does not match exactly, leave the body
/// alone"). That was written believing mismatches were rare. They are **27.7%
/// of a real account**, so leaving them would show a duplicated title on one
/// note in four — and every one of the four causes is now measured and
/// understood:
///
/// - the title is **truncated** with an ellipsis past ~65 characters
/// - inline objects are `U+FFFC` in the body and text in the title
/// - the first line can be **empty** (the title is the first NON-empty line)
/// - `U+2028` is removed from the title
///
/// # What it deliberately does NOT do
///
/// It does not preserve empty lines that appear *above* the title. They are
/// cut with it, because "the title is the first line and the body is the rest"
/// has no room for them. In M1 that is invisible — nothing is ever written
/// back. **M2 must revisit this before its first push**, since it is a
/// round-trip difference, and so must the `Unexplained` arm.
///
/// A note that is nothing but a title strips to an **empty body, correctly**.
/// Damage detection must not read that as corruption — an empty body is also
/// gotcha #17's signature, and here it is the right answer.
pub fn strip_leading_title(text: &str, title: &str) -> StrippedBody {
    if title.is_empty() {
        // Apple shows no title for this note, so every line is body. Cutting
        // one would be pure loss.
        return StrippedBody { body: text.to_string(), matched: TitleMatch::NoTitle };
    }

    let Some((start, end)) = title_span(text) else {
        // Nothing but empty lines. There is no title line to cut whatever the
        // record's title field claims, and `Unexplained` is the honest report:
        // our model of Apple's derivation does not cover this note.
        return StrippedBody { body: text.to_string(), matched: TitleMatch::Unexplained };
    };
    let line = &text[start..end];
    // `end + 1` steps past the '\n' that ended the title line. It runs off the
    // end when the title line ends the text rather than being followed by one,
    // which is exactly the title-only note.
    let body = text.get(end + 1..).unwrap_or("").to_string();

    StrippedBody { body, matched: classify(line, title) }
}

/// Byte offsets of the note's title line — the first NON-empty `\n`-delimited
/// line — as `(start, end)`, excluding the newline that ends it.
///
/// Hoisted out of [`strip_leading_title`] so the write path can cut at exactly
/// the same place rather than re-deriving it: `compose::recompose` replaces
/// this span and leaves everything around it alone, which is what keeps the
/// empty lines *above* a title (and a title-only note's missing trailing
/// newline) from being destroyed on the first push. A second implementation of
/// "where does the title end" is how the round trip stops being one.
pub fn title_span(text: &str) -> Option<(usize, usize)> {
    let mut cursor = 0usize;
    for segment in text.split('\n') {
        if !segment.is_empty() {
            return Some((cursor, cursor + segment.len()));
        }
        cursor += segment.len() + 1; // + the '\n' that split consumed
    }
    None
}

/// Which measured cause explains this line differing from its title.
fn classify(line: &str, title: &str) -> TitleMatch {
    if line == title {
        return TitleMatch::Exact;
    }
    // Before the truncation check: a title the user actually ended with an
    // ellipsis is a prefix, not a truncation.
    if line.starts_with(title) {
        return TitleMatch::Prefix;
    }
    if let Some(head) = title.strip_suffix(ELLIPSIS) {
        if !head.is_empty() && line.starts_with(head) {
            return TitleMatch::Truncated;
        }
    }
    if line.replace(LINE_SEPARATOR, "") == title {
        return TitleMatch::SoftBreakRemoved;
    }
    // Last, because it is the weakest claim: the presence of an inline object
    // only explains WHY a comparison cannot work, it does not confirm a match.
    if line.contains(OBJECT_REPLACEMENT) {
        return TitleMatch::ContainsObject;
    }
    TitleMatch::Unexplained
}

/// The note's title as Jodd caches it: **the note's own first line**, not the
/// record's `TitleEncrypted`.
///
/// M1 cached `TitleEncrypted` directly, which was right for a milestone that
/// only displayed it — it is exactly what Apple's own list shows. M2 cannot,
/// and the reason is the whole of gotcha #21 read in the write direction:
/// `TitleEncrypted` is a **lossy derivation** of the first line (truncated past
/// ~65 characters, inline objects rendered as text, `U+2028` removed), so on
/// **215 of 776** notes on the live account it is not the line it came from.
///
/// The body is cut by position, so the line itself is in neither cached field.
/// Push `title + "\n" + body` back with a truncated title and the note's first
/// line is truncated **on the server** — one note in four, silently, on the
/// user's first edit. Caching the line instead makes the round trip exact by
/// construction, and leaves `TitleEncrypted` doing the job gotcha #21 already
/// assigned it: verifying, never deciding. [`TitleMatch`] still reports which
/// measured cause explains a difference.
///
/// An empty `TitleEncrypted` still means "Apple shows no title for this
/// record", so every line is body and there is no title to take.
pub fn note_title(text: &str, title_field: &str) -> String {
    if title_field.is_empty() {
        return String::new();
    }
    match title_span(text) {
        Some((start, end)) => text[start..end].to_string(),
        // Nothing but empty lines: there is no first line to take, so the
        // record's own field is all there is.
        None => title_field.to_string(),
    }
}

/// A note body, straight from the record's `TextDataEncrypted` bytes to the
/// HTML the cache stores. The whole M1 content path in one call.
pub fn note_body_html(compressed: &[u8]) -> Result<String, DecodeError> {
    Ok(text_to_html(&decode_note_text(compressed)?))
}

/// Per-occurrence resolution of the text's `U+FFFC` inline objects: `out[i]`
/// answers for the i-th object in document order — `Some` when the covering
/// run's `attachmentInfo` names a live inline-TEXT attachment record
/// (a hashtag, most importantly), `None` for everything else (media,
/// tables, unresolvable ids), which keeps the raw character, today's shape.
///
/// The run must be exactly one unit long and sit exactly on the `U+FFFC` —
/// icloud-md's own embed-model check; anything else does not resolve.
pub(super) fn resolve_inline_objects(
    text: &str,
    runs: &[topotext::AttributeRun],
    inline_refs: &std::collections::HashMap<String, super::wire::InlineRef>,
) -> Vec<Option<super::format_html::HashtagRef>> {
    use super::format_html::HashtagRef;
    let mut by_offset: std::collections::HashMap<usize, HashtagRef> = std::collections::HashMap::new();
    let mut offset = 0usize;
    for run in runs {
        if run.length == 1 {
            if let Some(id) = run.attachment_info.as_ref().and_then(|i| i.attachment_identifier.as_ref()) {
                if let Some(r) = inline_refs.get(id) {
                    if r.type_uti.contains("inlinetextattachment") && !r.alt_text.is_empty() {
                        by_offset
                            .insert(offset, HashtagRef { record_name: id.clone(), text: r.alt_text.clone() });
                    }
                }
            }
        }
        offset += run.length as usize;
    }
    // ~94% of notes carry no attachment run at all (measured: 48 of 782 have
    // inline objects) — skip the full-text scan for them. Callers treat a
    // missing entry as None, so an empty vec is "nothing resolved".
    if by_offset.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (unit_offset, unit) in text.encode_utf16().enumerate() {
        if unit == OBJECT_REPLACEMENT as u16 {
            out.push(by_offset.remove(&unit_offset));
        }
    }
    out
}

/// The text with every RESOLVED inline object rendered as its display text
/// (`#work`), unresolved objects kept as the raw character. This is what
/// `TitleEncrypted`/`SnippetEncrypted` must be derived from — Apple's own
/// title derivation renders inline objects as text (gotcha #21), and a
/// title derived from the raw text would put a literal `U+FFFC` in Apple's
/// list view.
pub fn text_with_objects_rendered(
    text: &str,
    runs: &[topotext::AttributeRun],
    inline_refs: &std::collections::HashMap<String, super::wire::InlineRef>,
) -> String {
    let objects = resolve_inline_objects(text, runs, inline_refs);
    if objects.iter().all(|o| o.is_none()) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    for ch in text.chars() {
        if ch == OBJECT_REPLACEMENT {
            match objects.get(i).and_then(|o| o.as_ref()) {
                Some(tag) => out.push_str(&tag.text),
                None => out.push(ch),
            }
            i += 1;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Body HTML for the editor: formatting-aware when the runs decode, exactly
/// today's plain rendering when they don't — a note never renders worse than
/// M2 (spec F2/F5).
///
/// The title cut stays POSITIONAL (gotcha #21): [`strip_leading_title`]
/// decides where the body starts, never the format model — the format only
/// says how the remaining lines look.
pub fn note_body_html_formatted(
    text: &str,
    title_field: &str,
    runs: &[topotext::AttributeRun],
    inline_refs: &std::collections::HashMap<String, super::wire::InlineRef>,
) -> String {
    let stripped = strip_leading_title(text, title_field);
    let plain = || text_to_html(&stripped.body);
    if stripped.body.is_empty() {
        return String::new();
    }
    // Separators the format model does not carry — the plain renderer
    // handles them; rendering them through the paragraph model would weld
    // lines, the exact failure `LINE_SEPARATOR`'s doc records. One shared
    // predicate with the layer gate and the save path.
    if !super::format::model_applies(text) {
        return plain();
    }
    let paragraphs = match super::format::decode_note_format(text, runs) {
        Ok(p) => p,
        Err(_) => return plain(),
    };
    // The cut removes whole leading lines (empty lines above the title plus
    // the title line itself), so the body's paragraphs are a suffix of the
    // full text's.
    let body_lines = stripped.body.split('\n').count();
    let cut = paragraphs.len().saturating_sub(body_lines);
    // Objects are resolved across the FULL text; the ones in the removed
    // prefix are skipped so pairing stays 1:1 with the body's `U+FFFC`s.
    let objects = resolve_inline_objects(text, runs, inline_refs);
    let removed: usize = paragraphs[..cut]
        .iter()
        .map(|p| p.text.chars().filter(|c| *c == OBJECT_REPLACEMENT).count())
        .sum();
    let body_objects = objects.get(removed..).unwrap_or(&[]);
    super::format_html::render_paragraphs(&paragraphs[cut..], body_objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::{GzEncoder, ZlibEncoder};
    use std::io::Write;

    // ── format-aware body HTML (M3 Task 6) ──────────────────────────────

    mod formatted_body {
        use super::super::*;
        use std::collections::HashMap;

        fn run_len(len: u32) -> topotext::AttributeRun {
            topotext::AttributeRun { length: len, ..Default::default() }
        }
        fn styled_run(len: u32, style: u32) -> topotext::AttributeRun {
            topotext::AttributeRun {
                length: len,
                paragraph_style: Some(topotext::ParagraphStyle {
                    style: Some(style),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }

        #[test]
        fn a_formatted_note_renders_headings_and_lists_in_the_body_html() {
            // "T\nHead\nitem" — title "T" (cut), "Head" Heading, "item" bullet.
            let runs = vec![styled_run(2, 0), styled_run(5, 1), styled_run(4, 100)];
            let html = note_body_html_formatted("T\nHead\nitem", "T", &runs, &HashMap::new());
            assert!(html.contains("<h2>Head</h2>"), "{html}");
            assert!(html.contains("<ul><li>item</li></ul>"), "{html}");
            assert!(!html.contains(">T<"), "the title line must be cut: {html}");
        }

        #[test]
        fn an_undecodable_format_falls_back_to_the_plain_rendering() {
            let runs = vec![styled_run(9, 77)]; // unknown style ⇒ unsupported
            let html = note_body_html_formatted("T\nbody", "T", &runs, &HashMap::new());
            assert_eq!(html, "<div>body</div>");
        }

        #[test]
        fn a_hashtag_object_resolves_through_the_inline_refs_map() {
            let runs = vec![
                run_len(2), // "T\n"
                topotext::AttributeRun {
                    length: 1,
                    attachment_info: Some(topotext::AttachmentInfo {
                        attachment_identifier: Some("tag-1".into()),
                        type_uti: Some("com.apple.notes.inlinetextattachment.hashtag".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                run_len(5), // " here"
            ];
            let mut refs = HashMap::new();
            refs.insert(
                "tag-1".to_string(),
                super::super::super::wire::InlineRef {
                    type_uti: "com.apple.notes.inlinetextattachment.hashtag".into(),
                    alt_text: "#work".into(),
                },
            );
            let html = note_body_html_formatted("T\n\u{FFFC} here", "T", &runs, &refs);
            assert!(html.contains(r#"data-ref="tag-1""#), "{html}");
            assert!(html.contains("#work"), "{html}");
            assert!(!html.contains('\u{FFFC}'), "{html}");
        }

        #[test]
        fn an_unresolvable_object_keeps_the_raw_character_and_plain_notes_render_unchanged() {
            let runs = vec![run_len(4)];
            let html = note_body_html_formatted("T\n\u{FFFC}x", "T", &runs, &HashMap::new());
            assert!(html.contains('\u{FFFC}'), "{html}");
            // A plain note must render byte-identically to the M2 path.
            let plain = vec![run_len(6)];
            assert_eq!(
                note_body_html_formatted("T\nbody", "T", &plain, &HashMap::new()),
                "<div>body</div>"
            );
            // A title-only note stays an empty body.
            assert_eq!(note_body_html_formatted("Just a title", "Just a title", &[], &HashMap::new()), "");
        }

        #[test]
        fn carriage_returns_and_u2029_take_the_plain_path() {
            let html = note_body_html_formatted("T\nbody\r\nmore", "T", &[run_len(12)], &HashMap::new());
            assert_eq!(html, text_to_html("body\r\nmore"));
        }
    }

    /// Builds the real byte shape a CloudKit note body has, so these tests
    /// exercise the actual nesting rather than a convenient stand-in.
    fn note_document(text: &str) -> Vec<u8> {
        let inner = topotext::String { string: text.to_string(), ..Default::default() };
        let doc = versioned_document::Document {
            serialization_version: Some(1),
            version: vec![versioned_document::Version {
                serialization_version: Some(1),
                minimum_supported_version: Some(1),
                data: Some(inner.encode_to_vec()),
            }],
        };
        doc.encode_to_vec()
    }

    fn zlib(raw: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(raw).unwrap();
        e.finish().unwrap()
    }

    fn gzip(raw: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(raw).unwrap();
        e.finish().unwrap()
    }

    /// The headline case. Both containers come back from the SAME endpoint —
    /// it depends on which client last wrote the note — so a decoder that
    /// only knows zlib fails on real notes, and fails as "unreadable", which
    /// is the signature the ADP check keys on.
    #[test]
    fn both_gzip_and_zlib_bodies_decode() {
        let raw = note_document("Meeting notes\nfirst point");
        assert_eq!(decode_note_text(&zlib(&raw)).unwrap(), "Meeting notes\nfirst point");
        assert_eq!(decode_note_text(&gzip(&raw)).unwrap(), "Meeting notes\nfirst point");
    }

    #[test]
    fn zlib_at_any_compression_level_decodes() {
        // A `78 9c` equality test would reject these: the second header byte
        // varies with level, and only `9c` is the default.
        let raw = note_document("level check");
        for level in [0u32, 1, 6, 9] {
            let mut e = ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
            e.write_all(&raw).unwrap();
            let body = e.finish().unwrap();
            assert_eq!(
                decode_note_text(&body).unwrap(),
                "level check",
                "zlib level {level} must decode"
            );
        }
    }

    /// What an ADP note looks like from out here: bytes that are not a
    /// compressed stream at all. Must be `Unreadable`, because that is what
    /// the sign-in check turns into "this account cannot work" rather than
    /// into a retry.
    #[test]
    fn genuinely_encrypted_bytes_report_unreadable_not_malformed() {
        let ciphertext = [0x42u8, 0x9e, 0x00, 0xff, 0x13, 0x37, 0xaa, 0x01];
        match decode_note_text(&ciphertext) {
            Err(DecodeError::Unreadable(_)) => {}
            other => panic!("ADP-shaped bytes must read as Unreadable, got {other:?}"),
        }
        // Short inputs too — an empty or truncated field is not a document.
        assert!(matches!(decode_note_text(&[]), Err(DecodeError::Unreadable(_))));
        assert!(matches!(decode_note_text(&[0x78]), Err(DecodeError::Unreadable(_))));
    }

    /// The contrast that makes the split useful: something that decompresses
    /// fine but is not a note document is a bug or a schema change, NOT an
    /// encrypted account, and must never be reported as one.
    #[test]
    fn decompressible_garbage_is_malformed_not_unreadable() {
        // Valid zlib, and the inner bytes are a well-formed `Document` with no
        // version at all — the shape this code refuses rather than guesses at.
        let doc = versioned_document::Document {
            serialization_version: Some(1),
            version: vec![],
        };
        match decode_note_text(&zlib(&doc.encode_to_vec())) {
            Err(DecodeError::Malformed(m)) => assert!(m.contains("0 versions"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    /// Picking one would be a guess that silently returns the wrong content.
    #[test]
    fn a_multi_version_document_is_refused_rather_than_guessed_at() {
        let v = |t: &str| versioned_document::Version {
            serialization_version: Some(1),
            minimum_supported_version: Some(1),
            data: Some(topotext::String { string: t.into(), ..Default::default() }.encode_to_vec()),
        };
        let doc = versioned_document::Document {
            serialization_version: Some(1),
            version: vec![v("first"), v("second")],
        };
        assert!(matches!(
            decode_note_text(&zlib(&doc.encode_to_vec())),
            Err(DecodeError::Malformed(_))
        ));
    }

    /// The title is line one of the text, and this function must NOT remove
    /// it. Stripping belongs downstream, where the record's own `title` field
    /// is available as ground truth to cut on.
    #[test]
    fn the_title_line_is_returned_not_stripped() {
        let raw = note_document("Groceries\nmilk\neggs");
        assert_eq!(decode_note_text(&zlib(&raw)).unwrap(), "Groceries\nmilk\neggs");
    }

    /// The case that falsified the first implementation. Measured on the live
    /// account: the separator is inside real note text, and dropping it welds
    /// two visible lines into one.
    #[test]
    fn a_soft_line_break_becomes_br_inside_the_same_paragraph() {
        assert_eq!(
            text_to_html("two line \u{2028}1"),
            "<div>two line <br>1</div>",
            "U+2028 is a break WITHIN a paragraph — dropping it silently joins the lines"
        );
        // And it stays distinguishable from a paragraph break, which is what
        // lets M2 write back the shape it read.
        assert_eq!(text_to_html("a\u{2028}b\nc"), "<div>a<br>b</div><div>c</div>");
    }

    /// Unhandled, U+2029 would collapse to nothing inside a <div> and produce
    /// the same weld. Reasoned rather than measured — no corpus note had one.
    #[test]
    fn a_unicode_paragraph_separator_ends_the_paragraph() {
        assert_eq!(text_to_html("a\u{2029}b"), "<div>a</div><div>b</div>");
    }

    #[test]
    fn text_becomes_one_div_per_line() {
        assert_eq!(text_to_html("a\nb"), "<div>a</div><div>b</div>");
        assert_eq!(text_to_html("one line"), "<div>one line</div>");
        assert_eq!(text_to_html(""), "");
    }

    #[test]
    fn a_blank_line_keeps_its_height() {
        assert_eq!(
            text_to_html("a\n\nb"),
            "<div>a</div><div><br></div><div>b</div>",
            "an empty <div> collapses in a contenteditable — the note would lose the gap"
        );
    }

    /// Notes about code are ordinary, and this is the difference between
    /// rendering `<b>` and executing it.
    #[test]
    fn markup_in_the_text_is_escaped_not_injected() {
        assert_eq!(
            text_to_html("use <b> & <i>"),
            "<div>use &lt;b&gt; &amp; &lt;i&gt;</div>"
        );
        assert_eq!(
            text_to_html("<script>alert(1)</script>"),
            "<div>&lt;script&gt;alert(1)&lt;/script&gt;</div>"
        );
    }

    /// Thai has no word spaces, so a byte-oriented split would cut mid
    /// codepoint. Nothing here slices, but the project's first language is
    /// Thai and this is where that would surface.
    #[test]
    fn thai_text_survives_the_whole_path() {
        let text = "บันทึกการประชุม\nข้อ ๑ ทดสอบ";
        let html = note_body_html(&zlib(&note_document(text))).unwrap();
        assert_eq!(html, "<div>บันทึกการประชุม</div><div>ข้อ ๑ ทดสอบ</div>");
    }

    #[test]
    fn note_body_html_is_the_whole_path_in_one_call() {
        let html = note_body_html(&gzip(&note_document("Title\n\nbody & more"))).unwrap();
        assert_eq!(
            html,
            "<div>Title</div><div><br></div><div>body &amp; more</div>"
        );
    }

    // ---- strip_leading_title -------------------------------------------
    //
    // Every case below except `an_unexplained_line_is_still_cut` and the
    // all-empty guard is a shape MEASURED on 776 notes of a real account
    // (2026-08-22, `examples/icloud_probe`). The counts in the doc comment
    // are from that same run.

    #[test]
    fn the_ordinary_case_is_an_exact_match() {
        let got = strip_leading_title("Meeting notes\nfirst point\nsecond", "Meeting notes");
        assert_eq!(got.matched, TitleMatch::Exact);
        assert_eq!(got.body, "first point\nsecond");
    }

    #[test]
    fn a_truncated_title_still_cuts_the_whole_line() {
        // Apple truncates around 65 characters and appends U+2026. The line is
        // longer than its own title, which is exactly the case a
        // cut-only-on-exact-match rule would refuse — 27.7% of the account.
        let line = "A very long first line that Apple decided to shorten when it built the title field";
        let text = format!("{line}\nand the body below");
        let title = format!("{}{ELLIPSIS}", &line[..65]);

        let got = strip_leading_title(&text, &title);
        assert_eq!(got.matched, TitleMatch::Truncated);
        assert_eq!(got.body, "and the body below");
    }

    #[test]
    fn a_title_the_user_ended_with_an_ellipsis_is_a_prefix_not_a_truncation() {
        // Ordering inside `classify` is load-bearing: `starts_with` is tested
        // before the ellipsis strip, so a real ellipsis in the user's own text
        // is not misread as Apple's truncation marker.
        let got = strip_leading_title("Wait\u{2026} for it\nbody", "Wait\u{2026}");
        assert_eq!(got.matched, TitleMatch::Prefix);
        assert_eq!(got.body, "body");
    }

    #[test]
    fn an_inline_object_in_the_line_is_recognised_rather_than_compared() {
        // An inline hashtag is U+FFFC in the body and ordinary text in the
        // title, so no character comparison can ever succeed here.
        let got = strip_leading_title("this note with \u{FFFC}\nbody", "this note with #this");
        assert_eq!(got.matched, TitleMatch::ContainsObject);
        assert_eq!(got.body, "body");
    }

    #[test]
    fn a_soft_break_removed_from_the_title_is_recognised() {
        // Measured: a note displaying `two line 1` came back as
        // `two line ` + U+2028 + `1`, with U+2028 absent from the title.
        let got = strip_leading_title("two line \u{2028}1\nbody", "two line 1");
        assert_eq!(got.matched, TitleMatch::SoftBreakRemoved);
        assert_eq!(got.body, "body");
    }

    #[test]
    fn a_leading_empty_line_is_cut_with_the_title() {
        // The title is the first NON-empty line, so the empty lines above it
        // are part of the title line's territory, not of the body. M2 must
        // revisit this before it writes anything back.
        let got = strip_leading_title("\n\nThe title\nbody", "The title");
        assert_eq!(got.matched, TitleMatch::Exact);
        assert_eq!(got.body, "body");
    }

    #[test]
    fn a_note_that_is_only_a_title_strips_to_an_empty_body() {
        // Correct, not corruption — and worth pinning, because an empty body
        // is also gotcha #17's damage signature on the email backends.
        let got = strip_leading_title("Just a title", "Just a title");
        assert_eq!(got.matched, TitleMatch::Exact);
        assert_eq!(got.body, "");
    }

    #[test]
    fn a_record_with_no_title_keeps_every_line() {
        let got = strip_leading_title("first\nsecond", "");
        assert_eq!(got.matched, TitleMatch::NoTitle);
        assert_eq!(got.body, "first\nsecond");
    }

    #[test]
    fn an_unexplained_line_is_still_cut_but_says_so() {
        // The whole point of option (ก): Apple's model wins over our ability
        // to explain the difference. The variant is the signal that the four
        // measured causes have a gap, not a licence to keep the line.
        let got = strip_leading_title("something else entirely\nbody", "the title");
        assert_eq!(got.matched, TitleMatch::Unexplained);
        assert_eq!(got.body, "body");
    }

    #[test]
    fn text_with_no_non_empty_line_is_left_alone() {
        let got = strip_leading_title("\n\n", "a title");
        assert_eq!(got.matched, TitleMatch::Unexplained);
        assert_eq!(got.body, "\n\n");
    }

    #[test]
    fn the_body_never_keeps_the_title_line_it_cut() {
        // One assertion over every measured shape at once: whatever the
        // classification, the first line is gone. A regression that made
        // `classify` decide the cut would fail here rather than in a UI that
        // shows the title twice.
        let cases: &[(&str, &str)] = &[
            ("Meeting notes\nbody", "Meeting notes"),
            ("Wait\u{2026} for it\nbody", "Wait\u{2026}"),
            ("this note with \u{FFFC}\nbody", "this note with #this"),
            ("two line \u{2028}1\nbody", "two line 1"),
            ("\n\nThe title\nbody", "The title"),
            ("something else entirely\nbody", "the title"),
        ];
        for (text, title) in cases {
            assert_eq!(
                strip_leading_title(text, title).body,
                "body",
                "the title line survived for {title:?}"
            );
        }
    }

    /// The read-side half of M2's round trip. `TitleEncrypted` is a lossy
    /// derivation of the first line on 215 of 776 real notes; caching it and
    /// then pushing `title + body` back would truncate the note's own first
    /// line, on the server, for one note in four.
    #[test]
    fn the_cached_title_is_the_notes_own_first_line_not_apples_derivation() {
        // Truncated past ~65 characters — the commonest measured divergence.
        assert_eq!(
            note_title("A first line Apple shortened for its list\nbody", "A first line Apple…"),
            "A first line Apple shortened for its list"
        );
        // A soft break Apple's derivation removes.
        assert_eq!(note_title("two line\u{2028}1\nbody", "two line 1"), "two line\u{2028}1");
        // The ordinary case is unchanged.
        assert_eq!(note_title("Title\nbody", "Title"), "Title");
        // The title is the first NON-empty line.
        assert_eq!(note_title("\n\nTitle\nbody", "Title"), "Title");
    }

    /// An empty title field means Apple shows no title for this record, so
    /// every line is body — taking one would be pure loss, exactly as on the
    /// read side.
    #[test]
    fn a_record_with_no_title_field_gets_no_title_from_its_body() {
        assert_eq!(note_title("just some text\nmore", ""), "");
    }

    /// Nothing but empty lines: there is no first line to take, so the
    /// record's own field is all there is.
    #[test]
    fn a_text_with_no_line_at_all_falls_back_to_the_record_field() {
        assert_eq!(note_title("\n\n", "Whatever Apple says"), "Whatever Apple says");
    }

    #[test]
    fn stripping_a_body_that_already_lost_its_title_is_not_a_second_cut() {
        // There is no re-entry in M1 (nothing writes back), but a caller that
        // stripped twice would otherwise silently eat a real line. `NoTitle`
        // is the guard: the second pass has no title to remove.
        let once = strip_leading_title("Title\nfirst\nsecond", "Title");
        let twice = strip_leading_title(&once.body, "");
        assert_eq!(twice.body, once.body);
    }
}
