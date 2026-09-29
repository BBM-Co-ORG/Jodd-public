//! The write half of the content layer: HTML back into a CloudKit note document.
//!
//! [`super::doc`] turns Apple's document into HTML and stops there, because a
//! read-only milestone cannot propagate lossiness. This module is what M2 adds,
//! and its whole design is the answer to one question: **Jodd decodes exactly
//! one field of a note document — the visible text — so what happens to the
//! rest of it when the user edits a note?**
//!
//! Rebuilding the document from text alone answers "it is destroyed", server
//! side, silently, for every bold word, checklist, link and table on the note.
//! So nothing here interprets a formatting value. The document is decoded
//! whole (prost yields `substring`, `timestamp` and `attribute_run` whether or
//! not anything reads them), every value is carried through untouched, and the
//! only thing this module changes is the `length` of the runs — the arithmetic
//! that keeps them covering the text after an edit.
//!
//! What that buys, and what it costs, are both stated in the M2 design spec:
//! formatting already on a note survives an edit made in Jodd; formatting
//! *applied* in Jodd does not reach iCloud, because deriving new runs needs the
//! meaning of Apple's constants and this module deliberately never learns it.
//!
//! Everything the write path refuses is in [`writability`], and every refusal
//! is a sentence that ends up on `notes.push_blocked_reason` (gotcha #14).

use std::io::Write as _;

use prost::Message;

use super::crdt;
use super::doc::{self, DecodeError};
use super::gen::{topotext, versioned_document};

/// A note document, decoded whole.
///
/// The wrapper's version numbers are carried, not chosen: they are a
/// compatibility contract with Apple's own clients, and inventing one is how a
/// note becomes unreadable on an older iPhone. A create has no document to
/// carry them from, which is what [`NoteDocument::new`] is for.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteDocument {
    /// The whole `topotext.String`, every field, values untouched.
    pub string: topotext::String,
    /// `versioned_document.Document.serializationVersion`.
    pub serialization_version: Option<u32>,
    /// `versioned_document.Version.serializationVersion`.
    pub version_serialization_version: Option<u32>,
    /// `versioned_document.Version.minimumSupportedVersion`.
    pub minimum_supported_version: Option<u32>,
    /// `Some` only when this document's CRDT identity was successfully
    /// parsed and validated by `crdt::parse_crdt_document` — the signal
    /// that [`NoteDocument::with_text_crdt`] can be used instead of the
    /// opaque [`NoteDocument::with_text`] splice. `None` on every document
    /// built by [`NoteDocument::new`] or that failed CRDT validation (the
    /// two cases `writability`'s `CarriesCrdtIdentity` refusal still
    /// covers).
    pub crdt: Option<crdt::CrdtDocument>,
}

impl NoteDocument {
    /// The visible text.
    pub fn text(&self) -> &str {
        &self.string.string
    }

    /// A brand-new document for a note that has no remote form yet.
    ///
    /// One attribute run covering the whole text, carrying nothing: no font,
    /// no paragraph style, no hints. That is the minimum a document needs to
    /// be internally consistent, and it is the honest shape for content Jodd
    /// composed — claiming a paragraph style would mean guessing which of
    /// Apple's codes means "title", which is the guess this module exists to
    /// avoid. Apple's own clients restyle a note's first line as its title on
    /// display anyway; the record's `TitleEncrypted` is what the note is
    /// called.
    /// The wrapper version triple Apple's own documents carry.
    ///
    /// **Measured, not chosen** (2026-08-24, 776 notes on a live account): every
    /// single one is `(Some(0), Some(0), Some(0))`. `None` is not the same
    /// thing — in proto2 an absent optional field is not emitted at all, while
    /// `Some(0)` emits it with the value zero — and `NoteDocument::new` used
    /// `None`, so a note created by Jodd omitted three fields that are present
    /// on every note Apple writes. That is the leading explanation for the
    /// live-run failure where Apple Notes showed a Jodd-created note as
    /// "New Note / No additional text".
    const APPLE_WRAPPER_VERSIONS: (Option<u32>, Option<u32>, Option<u32>) =
        (Some(0), Some(0), Some(0));

    pub fn new(text: &str) -> NoteDocument {
        NoteDocument {
            string: topotext::String {
                string: text.to_string(),
                substring: Vec::new(),
                timestamp: None,
                attribute_run: vec![topotext::AttributeRun {
                    length: utf16_len(text) as u32,
                    ..Default::default()
                }],
            },
            serialization_version: Self::APPLE_WRAPPER_VERSIONS.0,
            version_serialization_version: Self::APPLE_WRAPPER_VERSIONS.1,
            minimum_supported_version: Self::APPLE_WRAPPER_VERSIONS.2,
            crdt: None,
        }
    }

    /// A brand-new document **carrying CRDT identity**, which is what a note
    /// Apple keeps looks like — [`new`]'s replacement on every path that
    /// actually creates a note.
    ///
    /// **[`new`] alone produces a note Apple's own client throws away.**
    /// Measured live 2026-08-27: three notes created through Jodd's real UI
    /// were accepted by CloudKit, appeared in Apple Notes, and were
    /// tombstoned by Apple within four minutes — purged, not moved to
    /// Recently Deleted, with no delete ever asked for by Jodd. The document
    /// [`new`] builds has `substring` empty and `timestamp` absent: no
    /// per-character identity, no replica clock table. Apple's clients merge
    /// notes *through* that structure, so a document without it is not a
    /// note they can hold. It also explains M2's older observation that a
    /// Jodd-created note "came back empty" — there were no runs to
    /// reconstruct the text from.
    ///
    /// This mirrors icloud-md's `buildInitialNoteDocument` exactly: seed
    /// Apple's own two-node graph — a zero-length origin run at replica 0
    /// whose single child edge points at the end sentinel, which is what
    /// `initWithReplicaID` produces — then run the ordinary text edit over
    /// it, so the first insert splices itself into that edge the same way
    /// every later edit does. One code path mints identity, not two.
    ///
    /// Empty text is refused (`None`), as the reference refuses it: there is
    /// no edit to apply and no run to mint, and a note with neither is the
    /// shape this function exists to stop producing. Callers fall back to
    /// [`new`] — a note with no title and no body is nothing to lose, and
    /// the user's next keystroke goes through the normal edit path.
    pub fn new_with_replica(text: &str, replica_id: [u8; 16]) -> NoteDocument {
        Self::try_new_with_replica(text, replica_id).unwrap_or_else(|| Self::new(text))
    }

    /// [`new_with_replica`]'s fallible core: `None` when the text is empty or
    /// the engine refuses its own seed (neither observed; the fallback keeps
    /// a create from failing outright either way).
    pub fn try_new_with_replica(text: &str, replica_id: [u8; 16]) -> Option<NoteDocument> {
        if text.is_empty() {
            return None;
        }
        // Apple's `initWithReplicaID` seed: origin run → end sentinel.
        let mut seed = crdt::CrdtDocument {
            text: String::new(),
            runs: vec![
                crdt::TextRun {
                    coord: crdt::RunCoord { replica: 0, clock: 0 },
                    length: 0,
                    anchor: crdt::RunCoord { replica: 0, clock: 0 },
                    tombstone: false,
                    sequence: vec![1],
                },
                crdt::TextRun {
                    coord: crdt::RunCoord { replica: 0, clock: crdt::SENTINEL_CLOCK },
                    length: 0,
                    anchor: crdt::RunCoord { replica: 0, clock: crdt::SENTINEL_CLOCK },
                    tombstone: false,
                    sequence: Vec::new(),
                },
            ],
            replicas: Vec::new(),
            attribute_runs: Vec::new(),
        };
        crdt::apply_text_edit(&mut seed, text, replica_id).ok()?;
        Some(NoteDocument {
            string: crdt::encode_crdt_document(&seed),
            serialization_version: Self::APPLE_WRAPPER_VERSIONS.0,
            version_serialization_version: Self::APPLE_WRAPPER_VERSIONS.1,
            minimum_supported_version: Self::APPLE_WRAPPER_VERSIONS.2,
            crdt: Some(seed),
        })
    }

    /// The same document carrying different text, with every run preserved and
    /// only its lengths spliced. See [`splice_runs`].
    ///
    /// Only valid on a document whose `crdt` field is `None` — this splice
    /// updates `string.string` and re-lengths `attribute_run` but leaves
    /// `string.substring`/`string.timestamp` untouched, so calling it on a
    /// CRDT-carrying document produces a structurally invalid one (visible
    /// run lengths still reflecting the OLD text). Use [`with_text_crdt`]
    /// instead when `crdt` is `Some`.
    pub fn with_text(&self, new_text: &str) -> NoteDocument {
        debug_assert!(
            self.crdt.is_none(),
            "with_text called on a CRDT-carrying document — use with_text_crdt instead"
        );
        let runs = splice_runs(self.text(), new_text, &self.string.attribute_run);
        let mut out = self.clone();
        out.string.string = new_text.to_string();
        out.string.attribute_run = runs;
        out
    }

    /// The CRDT-aware sibling of [`with_text`]: performs a real edit via
    /// [`crdt::apply_text_edit`] instead of the opaque preserve-everything
    /// splice. Only valid to call on a document whose `crdt` field is
    /// `Some` — i.e. one `writability()` returned successfully because the
    /// CRDT engine could parse and validate it. Errors if `self.crdt` is
    /// `None` (this document was never CRDT-writable to begin with) or if
    /// the edit itself fails the engine's own invariant checks.
    pub fn with_text_crdt(&self, new_text: &str, replica_id: [u8; 16]) -> Result<NoteDocument, crdt::CrdtError> {
        let mut crdt_doc = self.crdt.clone().ok_or(crdt::CrdtError::MissingTimestampTable)?;
        crdt::apply_text_edit(&mut crdt_doc, new_text, replica_id)?;
        let mut out = self.clone();
        out.string = crdt::encode_crdt_document(&crdt_doc);
        out.crdt = Some(crdt_doc);
        Ok(out)
    }
}

/// Decodes a note body into a document that can be written back.
///
/// The read path ([`doc::decode_note_text`]) stops at the text on purpose;
/// this one keeps everything, because everything is what has to survive a
/// write. It shares the same `DecodeError` split — an `Unreadable` body is
/// what Advanced Data Protection looks like from out here, and must never be
/// confused with a document this code merely does not understand.
pub fn parse(compressed: &[u8]) -> Result<NoteDocument, DecodeError> {
    let raw = doc::decompress_note_document(compressed)?;
    let wrapper = versioned_document::Document::decode(&raw[..])
        .map_err(|e| DecodeError::Malformed(format!("versioned_document.Document: {e}")))?;
    if wrapper.version.len() != 1 {
        return Err(DecodeError::Malformed(format!(
            "document carries {} versions — only exactly one is understood",
            wrapper.version.len()
        )));
    }
    let version = &wrapper.version[0];
    let payload = version
        .data
        .as_ref()
        .ok_or_else(|| DecodeError::Malformed("version carries no data payload".into()))?;
    let string = topotext::String::decode(&payload[..])
        .map_err(|e| DecodeError::Malformed(format!("topotext.String: {e}")))?;
    Ok(NoteDocument {
        string,
        serialization_version: wrapper.serialization_version,
        version_serialization_version: version.serialization_version,
        minimum_supported_version: version.minimum_supported_version,
        crdt: None,
    })
}

/// The uncompressed protobuf bytes of a document, ready to be gzipped.
fn to_raw(d: &NoteDocument) -> Vec<u8> {
    let payload = d.string.encode_to_vec();
    versioned_document::Document {
        serialization_version: d.serialization_version,
        version: vec![versioned_document::Version {
            serialization_version: d.version_serialization_version,
            minimum_supported_version: d.minimum_supported_version,
            data: Some(payload),
        }],
    }
    .encode_to_vec()
}

/// A document, ready for `TextDataEncrypted` — the caller base64s it.
///
/// **zlib, not gzip — reversed 2026-08-26.** The first version of this
/// function chose gzip on the measurement that 774 of 776 notes on the live
/// account are gzip (gotcha #20) — but those are Notes.app's own writes
/// arriving through Apple's internal sync stack. What a CLIENT of this web
/// API sends is a different question, and icloud-md's `noteText.ts` answers
/// it from captures: *"client traffic is zlib (magic `78 9c`), never gzip,
/// so we match that."* The two zlib notes in the census are exactly the two
/// the web client wrote. Matching the only proven client of this endpoint is
/// the same conformance rule as the field set in `wire::modify_note_body`.
/// The reader accepts both either way, so this decides nothing about reading.
pub fn encode(d: &NoteDocument) -> Vec<u8> {
    let raw = to_raw(d);
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing to a Vec cannot fail; the Result is the io::Write signature.
    let _ = enc.write_all(&raw);
    enc.finish().unwrap_or_default()
}

/// The decompressed bytes of a note body — the layer `round_trips` compares.
///
/// Exposed so a diagnostic can ask *where* a failed round trip differs without
/// re-implementing the decompression rule (gzip or zlib, gotcha #20).
pub fn decompress(compressed: &[u8]) -> Result<Vec<u8>, DecodeError> {
    doc::decompress_note_document(compressed)
}

/// Does the INNER `topotext.String` reproduce byte for byte, whatever the
/// wrapper does?
///
/// Splits a failed round trip into the two things it can mean. A wrapper-only
/// mismatch is a field this code omits or emits differently — recoverable by
/// carrying it. A string mismatch is the content model itself being
/// incomplete, which is a different and much larger problem. Counting them
/// together says only "12% failed" and points nowhere.
pub fn inner_round_trips(parsed: &NoteDocument, raw: &[u8]) -> bool {
    let Ok(wrapper) = versioned_document::Document::decode(raw) else { return false };
    let Some(payload) = wrapper.version.first().and_then(|v| v.data.as_ref()) else {
        return false;
    };
    parsed.string.encode_to_vec() == *payload
}

/// Does this remote document reproduce **byte for byte** from Jodd's model?
///
/// icloud-md's first write guard, and the one PRIOR-ART singles out as the
/// discipline that would have caught gotcha #17: *reproduce the remote's
/// current form exactly from your own model, or refuse to edit it.* A document
/// that decodes but does not re-encode identically is one this model does not
/// fully cover — an unknown field, a different wire ordering, a shape prost
/// normalizes — and editing it would rewrite whatever the difference is.
///
/// The comparison is on the **decompressed** bytes: gzip output depends on the
/// compressor's settings, so comparing compressed bytes would refuse every
/// note on earth for a reason that has nothing to do with the document.
pub fn round_trips(compressed: &[u8]) -> bool {
    let Ok(raw) = doc::decompress_note_document(compressed) else { return false };
    let Ok(parsed) = parse(compressed) else { return false };
    // Both layers, not just the outer one: an inner `topotext.String` that
    // re-encodes differently is invisible in the wrapper's own bytes only if
    // the wrapper is rebuilt from the re-encoded payload — which is exactly
    // what `to_raw` does.
    to_raw(&parsed) == raw
}

// ────────────────────────────────────────────────────────────────────────────
// Attribute runs
// ────────────────────────────────────────────────────────────────────────────

/// Text length in UTF-16 code units — the unit `AttributeRun.length` is in.
///
/// Apple's document is an `NSAttributedString` on the other side of the wire,
/// and `NSString` counts UTF-16. That is asserted rather than assumed:
/// [`runs_cover`] is a write gate, so a note whose runs do not already add up
/// in this unit is refused rather than spliced with the wrong arithmetic, and
/// `examples/icloud_probe` reports the distribution across a real account in
/// all three candidate units.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Do the runs cover the text exactly?
pub fn runs_cover(text: &str, runs: &[topotext::AttributeRun]) -> bool {
    let total: u64 = runs.iter().map(|r| r.length as u64).sum();
    total == utf16_len(text) as u64
}

/// Re-lengths the runs so they cover `new_text`, preserving every value.
///
/// A prefix/suffix diff, which is what a text editor does to an attributed
/// string: everything before the change keeps its attributes, everything after
/// keeps its attributes, and the run that contains the edit absorbs the
/// difference. A deletion larger than that run consumes the runs after it,
/// which are dropped as they empty.
///
/// **Nothing here reads a value.** A run whose `font_hints` is 1 comes out a
/// run whose `font_hints` is 1, whatever that means to Apple — that is the
/// whole point of the design (M2 spec, Component L).
///
/// Offsets are UTF-16 code units throughout, and the prefix/suffix are pulled
/// back off a surrogate pair rather than splitting one: half a surrogate is not
/// a position any text has.
pub fn splice_runs(
    old_text: &str,
    new_text: &str,
    runs: &[topotext::AttributeRun],
) -> Vec<topotext::AttributeRun> {
    let old: Vec<u16> = old_text.encode_utf16().collect();
    let new: Vec<u16> = new_text.encode_utf16().collect();

    // A document with no runs at all gets one covering the new text: the
    // alternative is returning nothing, which leaves the document internally
    // inconsistent the moment anything reads it.
    if runs.is_empty() {
        return vec![topotext::AttributeRun { length: new.len() as u32, ..Default::default() }];
    }
    if old == new {
        return runs.to_vec();
    }

    let max = old.len().min(new.len());
    let mut p = 0usize;
    while p < max && old[p] == new[p] {
        p += 1;
    }
    // Never end the common prefix between a surrogate pair's halves.
    if p > 0 && is_high_surrogate(old[p - 1]) {
        p -= 1;
    }

    let mut s = 0usize;
    while s < max - p && old[old.len() - 1 - s] == new[new.len() - 1 - s] {
        s += 1;
    }
    if s > 0 && is_low_surrogate(old[old.len() - s]) {
        s -= 1;
    }

    let del_start = p;
    let del_end = old.len() - s;
    let ins_len = new.len() - p - s;

    let mut out: Vec<topotext::AttributeRun> = Vec::with_capacity(runs.len());
    let mut cursor = 0usize;
    let mut inserted = false;
    for (i, run) in runs.iter().enumerate() {
        let start = cursor;
        let end = start + run.length as usize;
        cursor = end;

        // How much of this run the deletion took.
        let overlap = end.min(del_end).saturating_sub(start.max(del_start));
        let mut len = (run.length as usize).saturating_sub(overlap);

        // The insertion inherits the run of the character BEFORE it, which is
        // what typing at the end of a bold word does in any editor — and, on
        // this backend specifically, is what keeps a lengthened title inside
        // the title's own run instead of handing it to the body's. An
        // insertion at offset 0 has no preceding character and takes the first
        // run.
        let takes_insertion =
            (del_start > start && del_start <= end) || (del_start == 0 && i == 0);
        if !inserted && ins_len > 0 && takes_insertion {
            len += ins_len;
            inserted = true;
        }
        if len > 0 {
            out.push(topotext::AttributeRun { length: len as u32, ..run.clone() });
        }
    }

    if out.is_empty() && !new.is_empty() {
        // Everything was deleted and then something was typed: keep the first
        // run's attributes rather than inventing a bare one, since that is what
        // the user was writing in.
        out.push(topotext::AttributeRun { length: new.len() as u32, ..runs[0].clone() });
    }
    out
}

pub(super) fn is_high_surrogate(u: u16) -> bool {
    (0xD800..0xDC00).contains(&u)
}
pub(super) fn is_low_surrogate(u: u16) -> bool {
    (0xDC00..0xE000).contains(&u)
}

// ────────────────────────────────────────────────────────────────────────────
// HTML → text
// ────────────────────────────────────────────────────────────────────────────

/// A soft break, while paragraphs are still being assembled. Never escapes
/// this module: [`html_to_text`] rewrites it to `U+2028` before returning.
const SOFT: char = '\u{1}';

/// The inverse of [`doc::text_to_html`].
///
/// Deliberately a scanner rather than a parse: the input is the shape
/// `text_to_html` emits and the shape a `contenteditable` emits, both of which
/// are `<div>` per paragraph with `<br>` inside one, and neither of which needs
/// a DOM to read. Everything else — `<b>`, `<span style=…>`, an editor's stray
/// wrapper — is dropped, which is the same statement as "formatting applied in
/// Jodd does not reach iCloud" (M2 spec, L3).
///
/// **A `<br>` alone in a block is an empty paragraph, not a soft break.** That
/// is what an empty line is in both generators, and reading it as a soft break
/// welds two paragraphs together — the same failure `U+2028` produced on the
/// read side before it was measured.
pub fn html_to_text(html: &str) -> String {
    let mut paras: Vec<String> = Vec::new();
    let mut buf = String::new();
    // Set by an opening block tag: it is what lets `<div></div>` be an empty
    // paragraph while a closing tag with nothing before it and no opening of
    // its own (the outer half of a nested pair) adds nothing.
    let mut open_block = false;
    let mut rest = html;

    while let Some(lt) = rest.find('<') {
        let raw = &rest[..lt];
        if !is_inter_block_whitespace(raw, buf.is_empty()) {
            decode_entities_into(&mut buf, raw);
        }
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            // An unterminated `<` is content, not a tag.
            decode_entities_into(&mut buf, &rest[lt..]);
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

        if name == "br" {
            buf.push(SOFT);
        } else if is_block(&name) {
            if closing {
                if !buf.is_empty() || open_block {
                    paras.push(std::mem::take(&mut buf));
                    open_block = false;
                }
            } else {
                if !buf.is_empty() {
                    paras.push(std::mem::take(&mut buf));
                }
                open_block = true;
            }
        }
    }
    if !is_inter_block_whitespace(rest, buf.is_empty()) {
        decode_entities_into(&mut buf, rest);
    }
    if !buf.is_empty() {
        paras.push(buf);
    }

    paras
        .iter()
        .map(|p| if p.as_str() == SOFT.to_string() { String::new() } else { p.replace(SOFT, "\u{2028}") })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn is_block(name: &str) -> bool {
    matches!(
        name,
        "div" | "p" | "li" | "ul" | "ol" | "blockquote" | "pre"
            | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            | "table" | "tr" | "section" | "article"
    )
}

/// Is this raw text node whitespace that a serializer inserted BETWEEN block
/// elements, rather than content?
///
/// `nothing_accumulated` means the current paragraph has no text yet, so
/// this run sits against a tag on both sides: between `</div>` and `<div>`,
/// or directly inside a container like `<ul>`. Whitespace there is markup —
/// a pretty-printer's newline, or (measured live 2026-08-27) the
/// indentation a paste carries. Reading it as content invents a blank line
/// in the note's text and, for the paragraph model, a paragraph whose whole
/// text is a newline.
///
/// **The cost, stated:** a block containing ONLY whitespace (`<div> </div>`)
/// becomes an empty paragraph rather than a one-space one. That loses a
/// space on a line that had nothing else, and it is the safe direction —
/// `layers_round_trip` then refuses such a note instead of rewriting it,
/// where the old behaviour silently added blank lines to every note whose
/// HTML was ever pretty-printed. Whitespace that shares a paragraph with
/// real text (`<div>  spaced  </div>`) is untouched.
///
/// Shared with `format_html::parse_editor_html` for the same reason
/// [`decode_entities_into`] is: two scanners that disagree about what a
/// paragraph is are two different notes.
pub(super) fn is_inter_block_whitespace(raw: &str, nothing_accumulated: bool) -> bool {
    nothing_accumulated && !raw.is_empty() && raw.chars().all(char::is_whitespace)
}

/// Unescapes the entities [`doc::escape_html_text`] can produce, plus the few a
/// `contenteditable` adds on its own.
/// Shared with `format_html::parse_editor_html` so the two scanners can
/// never disagree on entity decoding.
pub(super) fn decode_entities_into(buf: &mut String, s: &str) {
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        buf.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let done = [
            ("&amp;", '&'), ("&lt;", '<'), ("&gt;", '>'), ("&quot;", '"'),
            ("&#39;", '\''), ("&apos;", '\''), ("&nbsp;", '\u{a0}'),
        ]
        .iter()
        .find_map(|(pat, ch)| {
            tail.strip_prefix(pat).map(|r| {
                buf.push(*ch);
                r
            })
        });
        match done {
            Some(r) => rest = r,
            None => {
                buf.push('&');
                rest = &tail[1..];
            }
        }
    }
    buf.push_str(rest);
}

// ────────────────────────────────────────────────────────────────────────────
// The title layer
// ────────────────────────────────────────────────────────────────────────────

/// Puts a title and a body back into one text, **editing the remote's own text
/// in place** rather than composing a fresh one.
///
/// This is gotcha #21's M2 obligation, and the two consequences it named are
/// both discharged by not composing:
///
/// - `strip_leading_title` cuts empty lines that sit *above* the title along
///   with it. They are prefix here, and nothing touches them, so they survive
///   a title edit instead of being destroyed on the first push.
/// - A note that is nothing but a title (41 of 776 on the live account) strips
///   to an empty body. There is no trailing newline to invent, so it stays
///   title-only rather than growing a blank line per edit — the shape gotcha
///   #17's separator bug produced on Exchange.
///
/// `recompose(t, title, title, body_of(t)) == t` is both a test property and a
/// runtime gate ([`writability`]).
pub fn recompose(old_text: &str, old_title: &str, new_title: &str, new_body: &str) -> String {
    // Apple shows no title for a record whose title field is empty, so every
    // line of it was body and there is no line to replace.
    if old_title.is_empty() {
        return match (new_title.is_empty(), new_body.is_empty()) {
            (true, _) => new_body.to_string(),
            (false, true) => new_title.to_string(),
            (false, false) => format!("{new_title}\n{new_body}"),
        };
    }
    let Some((start, end)) = doc::title_span(old_text) else {
        return compose_new(new_title, new_body);
    };
    let had_separator = old_text[end..].starts_with('\n');
    let mut out = String::with_capacity(old_text.len() + new_title.len() + new_body.len());
    out.push_str(&old_text[..start]);
    out.push_str(new_title);
    if had_separator || !new_body.is_empty() {
        out.push('\n');
    }
    out.push_str(new_body);
    out
}

/// The text of a note that has no remote form yet.
pub fn compose_new(title: &str, body: &str) -> String {
    match (title.is_empty(), body.is_empty()) {
        (true, _) => body.to_string(),
        (false, true) => title.to_string(),
        (false, false) => format!("{title}\n{body}"),
    }
}

// ────────────────────────────────────────────────────────────────────────────
// The gate
// ────────────────────────────────────────────────────────────────────────────

/// Unicode's OBJECT REPLACEMENT CHARACTER — an inline object's position in the
/// text (an attachment, a table, an inline hashtag). See gotcha #21.
const OBJECT_REPLACEMENT: char = '\u{FFFC}';

/// Why this note cannot be written.
///
/// Each variant is a refusal the M2 spec argues for individually, and each ends
/// up on `notes.push_blocked_reason` as a sentence the user reads (gotcha #14).
/// **Their relative frequency on a real account is the measurement that decides
/// what M3 relaxes first**, which is why they are kept apart rather than
/// collapsed into one "unsupported note".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unwritable {
    /// A `PasswordProtectedNote`, which is a different record type — never a
    /// note with an empty body. Component H3's amendment requires the guard to
    /// key on the type, because a guard keyed on `wire::LOCKED_BODY_HTML`
    /// passes the moment the user edits the placeholder.
    Locked,
    /// The remote document does not reproduce byte for byte from this model.
    DoesNotRoundTrip,
    /// `topotext.String.substring` / `timestamp` carry per-character CRDT
    /// identity. Inserting text would mean minting `CharID`s for a replica
    /// Jodd is not, and there is no correct value to invent.
    CarriesCrdtIdentity { substrings: usize },
    /// The runs do not already cover the text, so the splice's arithmetic does
    /// not hold on this note and would mis-attribute all of it.
    RunsDoNotCoverText { runs: usize, run_total: u64, text_len: usize },
    /// The text carries inline objects. An edit that crosses one can orphan
    /// it, and the editor shows the user nothing there to warn them.
    InlineObjects { count: usize },
    /// The title/HTML layers do not round-trip: what Jodd would send back
    /// differs from what it was given, before the user has changed anything.
    LayersDoNotRoundTrip,
}

impl std::fmt::Display for Unwritable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unwritable::Locked => write!(
                f,
                "this note is password-protected in Apple Notes — Jodd can show its title \
                 but cannot write to it"
            ),
            Unwritable::DoesNotRoundTrip => write!(
                f,
                "Jodd cannot reproduce this note's stored format exactly, so it will not \
                 overwrite it — open it in Apple Notes to edit"
            ),
            Unwritable::CarriesCrdtIdentity { substrings } => write!(
                f,
                "this note carries per-character sync data Jodd cannot extend \
                 ({substrings} range(s)) — open it in Apple Notes to edit"
            ),
            Unwritable::RunsDoNotCoverText { runs, run_total, text_len } => write!(
                f,
                "this note's formatting ({runs} run(s) covering {run_total} of {text_len} \
                 characters) is not a shape Jodd can preserve — open it in Apple Notes to edit"
            ),
            Unwritable::InlineObjects { count } => write!(
                f,
                "this note contains {count} attachment(s), table(s) or inline tag(s) Jodd \
                 cannot yet write — open it in Apple Notes to edit"
            ),
            Unwritable::LayersDoNotRoundTrip => write!(
                f,
                "Jodd's own reading of this note does not reproduce it exactly, so it will \
                 not overwrite it — open it in Apple Notes to edit"
            ),
        }
    }
}

/// May this document be written back?
///
/// Six refusals, checked against the **remote's own current document** rather
/// than against the edit — an edit cannot be judged safe by looking at it, only
/// by knowing whether the thing it will overwrite is reproducible.
///
/// `compressed` is the record's `TextDataEncrypted` bytes, `title` its
/// `TitleEncrypted`.
///
/// With no `InlineAttachment` map, nothing resolves and every inline object
/// refuses — exactly M2's behavior. Callers holding a zone scan use
/// [`writability_with_refs`] so resolvable hashtags stop refusing (M3 F5).
pub fn writability(compressed: &[u8], title: &str) -> Result<NoteDocument, Unwritable> {
    writability_with_refs(compressed, title, &std::collections::HashMap::new())
}

/// [`writability`] with the walk's `InlineAttachment` map (M3 F5/F6).
pub fn writability_with_refs(
    compressed: &[u8],
    title: &str,
    inline_refs: &std::collections::HashMap<String, super::wire::InlineRef>,
) -> Result<NoteDocument, Unwritable> {
    if !round_trips(compressed) {
        return Err(Unwritable::DoesNotRoundTrip);
    }
    let d = parse(compressed).map_err(|_| Unwritable::DoesNotRoundTrip)?;
    let crdt_doc = if !d.string.substring.is_empty() || d.string.timestamp.is_some() {
        let parsed = crdt::parse_crdt_document(&d.string)
            .map_err(|_| Unwritable::CarriesCrdtIdentity { substrings: d.string.substring.len() })?;
        match crdt::validate_document_invariants(&parsed) {
            Ok(()) => Some(parsed),
            // Same condition the plain `runs_cover` check below refuses more
            // specifically as `RunsDoNotCoverText` — surface it that way here
            // too, rather than letting the CRDT branch's catch-all bury a
            // run-coverage problem behind a CRDT-identity reason that has
            // nothing to do with it. Every other `CrdtError` variant (parse
            // failure, `SubclockPresent`, `RunReplicaOutOfRange`, etc.) still
            // means "this engine cannot mint identity here" and stays
            // `CarriesCrdtIdentity`.
            Err(crdt::CrdtError::AttributeLengthMismatch { .. }) => {
                return Err(Unwritable::RunsDoNotCoverText {
                    runs: d.string.attribute_run.len(),
                    run_total: d.string.attribute_run.iter().map(|r| r.length as u64).sum(),
                    text_len: utf16_len(d.text()),
                });
            }
            Err(_) => {
                return Err(Unwritable::CarriesCrdtIdentity { substrings: d.string.substring.len() });
            }
        }
    } else {
        None
    };
    let text = d.text();
    if !runs_cover(text, &d.string.attribute_run) {
        return Err(Unwritable::RunsDoNotCoverText {
            runs: d.string.attribute_run.len(),
            run_total: d.string.attribute_run.iter().map(|r| r.length as u64).sum(),
            text_len: utf16_len(text),
        });
    }
    // Refusal (5) — **M3 narrowed this and then put it back, deliberately.**
    //
    // The narrowing said: an object that resolves to a live inline-TEXT
    // attachment (a hashtag) renders and parses back to the same character,
    // so only UNRESOLVED objects (media, tables, dead refs) need refuse.
    // That is true of the character. It is NOT true of the object: an
    // attachment lives in a 1-unit run carrying `attachmentInfo`, and the
    // text splice does not move that run with the character. A probe
    // (`an_edit_that_moves_an_inline_object_loses_its_attachment_info`,
    // below) measured it: moving `U+FFFC` within the text leaves a bare
    // object character pointing at nothing, which Apple renders as empty —
    // the user's tag, silently gone.
    //
    // Re-narrowing is real work, not a flag flip: the write path would have
    // to rebuild the `attachmentInfo` run at the character's new position
    // from `format_html::ParsedBody::hashtags` (which already pairs 1:1
    // with each `U+FFFC` in document order), and that rebuild needs a live
    // pass of its own. Until then this backend does what it has evidence
    // for: it READS hashtags (new in M3 — they render, and `note_tags`
    // fills for the first time here) and refuses to WRITE the notes
    // carrying them. Measured cost: 48 of 782 notes (6%) are read-only.
    let objects = text.chars().filter(|c| *c == OBJECT_REPLACEMENT).count();
    if objects > 0 {
        return Err(Unwritable::InlineObjects { count: objects });
    }
    if !layers_round_trip(text, title, &d.string.attribute_run, inline_refs) {
        return Err(Unwritable::LayersDoNotRoundTrip);
    }
    Ok(NoteDocument { crdt: crdt_doc, ..d })
}

/// Does the note survive Jodd's own representation of it, unchanged?
///
/// Text → (title, body) → HTML → back → text, compared against the original.
/// One expression covering both places this project has lost content before:
/// the title cut (gotchas #11, #17, #21) and the HTML the editor round-trips
/// through — which is also where a separator Jodd cannot reproduce (`\r`,
/// `U+2029`) would otherwise be silently rewritten on the user's next save.
///
/// **`title_field` is the RECORD's `TitleEncrypted`, and the title this
/// actually uses is derived from it** by [`doc::note_title`] — the same
/// derivation the cache does. Comparing against the record's field directly
/// would refuse every note whose field is a lossy derivation of its first
/// line: truncated past ~65 characters, inline objects rendered as text,
/// `U+2028` removed — **215 of 776 notes on the live account**. The guard
/// would then be measuring the gap gotcha #21 documents rather than anything
/// about this note's writability, and would make one note in four read-only
/// for a reason that has nothing to do with the risk it exists to catch.
/// **M3 upgraded this from plain-text equality to the full formatting
/// pipeline** (spec F6): when the format decodes, the note goes through the
/// format-aware render (`doc::note_body_html_formatted`) and back
/// (`format_html::parse_editor_html`), and both the TEXT and the PROJECTION
/// must survive. When the format does not decode — or the text carries a
/// separator the paragraph model doesn't (`\r`, `U+2029`) — the plain-HTML
/// check stands, so those notes stay exactly as writable as M2 left them.
pub fn layers_round_trip(
    text: &str,
    title_field: &str,
    runs: &[topotext::AttributeRun],
    inline_refs: &std::collections::HashMap<String, super::wire::InlineRef>,
) -> bool {
    let title = doc::note_title(text, title_field);
    let body = doc::strip_leading_title(text, &title).body;
    let plain_ok = || {
        let through_html = html_to_text(&doc::text_to_html(&body));
        recompose(text, &title, &title, &through_html) == text
    };
    if body.is_empty() || !super::format::model_applies(text) {
        return plain_ok();
    }
    let Ok(full) = super::format::decode_note_format(text, runs) else {
        return plain_ok();
    };
    let body_lines = body.split('\n').count();
    let cut = full.len().saturating_sub(body_lines);
    let html = doc::note_body_html_formatted(text, title_field, runs, inline_refs);
    let parsed = super::format_html::parse_editor_html(&html);
    recompose(text, &title, &title, &parsed.text) == text
        && super::format::formats_round_trip_equal(&parsed.paragraphs, &full[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run carrying a value nothing here understands, which is the point:
    /// every assertion below is that it comes back unchanged.
    fn run(len: u32, hint: u32) -> topotext::AttributeRun {
        topotext::AttributeRun { length: len, font_hints: Some(hint), ..Default::default() }
    }

    fn doc_with(text: &str, runs: Vec<topotext::AttributeRun>) -> NoteDocument {
        NoteDocument {
            string: topotext::String {
                string: text.to_string(),
                substring: Vec::new(),
                timestamp: None,
                attribute_run: runs,
            },
            serialization_version: Some(1),
            version_serialization_version: Some(1),
            minimum_supported_version: Some(1),
            crdt: None,
        }
    }

    /// One whole-text run, which is the commonest real shape and the one the
    /// gate accepts.
    fn plain(text: &str) -> NoteDocument {
        doc_with(text, vec![run(utf16_len(text) as u32, 0)])
    }

    fn body_of(d: &NoteDocument) -> Vec<u8> {
        encode(d)
    }

    // ── the document itself ─────────────────────────────────────────────

    #[test]
    fn a_document_jodd_wrote_reads_back_as_the_same_document() {
        let d = plain("Title\nbody line");
        let bytes = body_of(&d);
        assert_eq!(doc::decode_note_text(&bytes).unwrap(), "Title\nbody line");
        assert_eq!(parse(&bytes).unwrap(), d);
    }

    /// The write path's own floor: whatever this module encodes, the M1 read
    /// path — which is what the user's other devices' data goes through —
    /// must decode. zlib is what captured web-client traffic sends — never
    /// gzip, which is Notes.app's own container (see `encode`'s comment).
    #[test]
    fn what_the_writer_emits_is_zlib_and_the_reader_takes_it() {
        let bytes = body_of(&plain("hello"));
        assert_eq!(bytes[0], 0x78, "the writer must emit zlib");
        assert_eq!(doc::note_body_html(&bytes).unwrap(), "<div>hello</div>");
    }

    /// Refusal (2). A document built by an independent encoder must reproduce
    /// byte for byte, or the guard is refusing everything for a reason that
    /// has nothing to do with the note.
    #[test]
    fn a_document_from_an_independent_encoder_round_trips() {
        let inner = topotext::String {
            string: "Groceries\nmilk".into(),
            substring: Vec::new(),
            timestamp: None,
            attribute_run: vec![run(9, 1), run(5, 0)],
        };
        let raw = versioned_document::Document {
            serialization_version: Some(1),
            version: vec![versioned_document::Version {
                serialization_version: Some(1),
                minimum_supported_version: Some(2),
                data: Some(inner.encode_to_vec()),
            }],
        }
        .encode_to_vec();
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&raw).unwrap();
        let compressed = e.finish().unwrap();

        assert!(round_trips(&compressed));
        let parsed = parse(&compressed).unwrap();
        assert_eq!(parsed.minimum_supported_version, Some(2));
        assert_eq!(parsed.string.attribute_run, vec![run(9, 1), run(5, 0)]);
    }

    /// **A note Jodd CREATES must carry CRDT identity, or Apple throws it
    /// away.** Measured live 2026-08-27: three notes created through the real
    /// UI reached CloudKit (HTTP 200, visible in Apple Notes within a
    /// minute), and every one was tombstoned by Apple's own client within
    /// four minutes — not moved to Recently Deleted, purged. Jodd never
    /// asked to delete any of them.
    ///
    /// The cause is this constructor: it produced a `topotext.String` with
    /// `substring` EMPTY and `timestamp` ABSENT — a document with no
    /// per-character identity and no replica clock table at all. Apple's
    /// clients merge notes through that structure; handed one that has none,
    /// they discard it. The same shape explains the M2 observation that a
    /// Jodd-created note "came back empty" (CLAUDE.md): there were no runs to
    /// reconstruct text from.
    ///
    /// icloud-md's `buildInitialNoteDocument` is the reference and it does
    /// the opposite: it seeds Apple's own two-node graph (origin run + end
    /// sentinel, empty replica table) and then runs the ordinary text edit
    /// over it, so a brand-new note is born with the same structure every
    /// other note has.
    #[test]
    fn a_note_jodd_creates_carries_crdt_identity_like_every_note_apple_writes() {
        let d = NoteDocument::new_with_replica("Title\nbody", [0xAB; 16]);
        assert!(
            !d.string.substring.is_empty(),
            "a created note with no substring runs is a note Apple discards"
        );
        assert!(
            d.string.timestamp.is_some(),
            "a created note with no replica clock table is a note Apple discards"
        );
        assert_eq!(d.text(), "Title\nbody");

        // The replica table names US, and the runs cover the text.
        let parsed = crdt::parse_crdt_document(&d.string).expect("the engine must parse its own output");
        crdt::validate_document_invariants(&parsed).expect("and it must satisfy its own invariants");
        assert_eq!(parsed.replicas[0].id, [0xAB; 16], "the editor sits FIRST in the table");

        // And the whole thing survives the real codec and the write gate —
        // an edit must not find the note it just created unwritable.
        let back = parse(&encode(&d)).unwrap();
        assert_eq!(back.string, d.string);
        assert!(writability(&encode(&d), "Title").is_ok());
    }

    /// A note Jodd CREATES carries the wrapper versions Apple's own notes
    /// carry, and `None` is not one of them.
    ///
    /// In proto2 an absent optional field is not emitted at all; `Some(0)`
    /// emits it with the value zero. Every one of 776 real notes has the
    /// triple `(0, 0, 0)`, so writing `None` produced a document missing three
    /// fields that are universal on this account.
    #[test]
    fn a_note_jodd_creates_carries_the_versions_apples_own_notes_carry() {
        let d = NoteDocument::new("Title\nbody");
        assert_eq!(
            (d.serialization_version, d.version_serialization_version, d.minimum_supported_version),
            (Some(0), Some(0), Some(0)),
            "measured on 776 live notes — every one carries the triple, none omits it"
        );
        // And it survives the encode, which is the thing Apple actually reads.
        let back = parse(&encode(&d)).unwrap();
        assert_eq!(back.serialization_version, Some(0));
        assert_eq!(back.version_serialization_version, Some(0));
        assert_eq!(back.minimum_supported_version, Some(0));
    }

    /// The wrapper's version numbers are a compatibility contract with Apple's
    /// own clients — carried from the document being replaced, never chosen.
    /// Inventing one is how a note becomes unreadable on an older iPhone.
    #[test]
    fn an_edit_carries_the_wrapper_versions_it_was_given() {
        let mut d = plain("one");
        d.minimum_supported_version = Some(9);
        d.version_serialization_version = Some(4);
        let after = parse(&encode(&d.with_text("two"))).unwrap();
        assert_eq!(after.minimum_supported_version, Some(9));
        assert_eq!(after.version_serialization_version, Some(4));
    }

    #[test]
    fn a_body_that_is_not_a_document_at_all_does_not_round_trip() {
        assert!(!round_trips(b"not compressed, not a document"));
    }

    // ── the splice ──────────────────────────────────────────────────────

    #[test]
    fn an_unchanged_text_leaves_every_run_exactly_as_it_was() {
        let runs = vec![run(5, 1), run(7, 2)];
        assert_eq!(splice_runs("abcde12345 6", "abcde12345 6", &runs), runs);
    }

    #[test]
    fn an_insertion_grows_the_run_it_lands_in_and_leaves_the_others() {
        // "Title" (5, bold) + "\nbody" (5).  Type " more" at the end.
        let runs = vec![run(5, 1), run(5, 0)];
        let out = splice_runs("Title\nbody", "Title\nbody more", &runs);
        assert_eq!(out, vec![run(5, 1), run(10, 0)]);
    }

    #[test]
    fn editing_the_title_grows_the_title_run_not_the_body_run() {
        let runs = vec![run(5, 1), run(5, 0)];
        let out = splice_runs("Title\nbody", "Title one\nbody", &runs);
        assert_eq!(out, vec![run(9, 1), run(5, 0)], "the title's own run must absorb the edit");
    }

    #[test]
    fn a_deletion_inside_one_run_shrinks_only_that_run() {
        let runs = vec![run(5, 1), run(5, 0)];
        let out = splice_runs("Title\nbody", "Ti\nbody", &runs);
        assert_eq!(out, vec![run(2, 1), run(5, 0)]);
    }

    /// A deletion wider than one run consumes the runs after it, and the ones
    /// it empties are dropped rather than left as zero-length rubble.
    #[test]
    fn a_deletion_spanning_runs_drops_the_ones_it_empties() {
        let runs = vec![run(3, 1), run(3, 2), run(3, 3)];
        let out = splice_runs("aaabbbccc", "aaccc", &runs);
        assert_eq!(out, vec![run(2, 1), run(3, 3)]);
    }

    #[test]
    fn emptying_the_note_leaves_no_runs_at_all() {
        let out = splice_runs("abc", "", &[run(3, 1)]);
        assert!(out.is_empty());
    }

    /// Delete everything, then type: the user is still writing in what the
    /// first run described, so that is what the new text carries. An invented
    /// bare run would be this module choosing a format, which it never does.
    #[test]
    fn replacing_the_whole_text_keeps_the_first_runs_attributes() {
        let out = splice_runs("abc", "xyzw", &[run(2, 7), run(1, 9)]);
        assert_eq!(out, vec![run(4, 7)]);
    }

    #[test]
    fn a_document_with_no_runs_gets_one_covering_the_new_text() {
        let out = splice_runs("abc", "abcd", &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].length, 4);
    }

    /// Lengths are UTF-16 code units, so an emoji is two and Thai is one each.
    /// Getting this wrong mis-attributes every note written in either.
    #[test]
    fn lengths_are_utf16_code_units_not_bytes_or_chars() {
        assert_eq!(utf16_len("ก"), 1);
        assert_eq!(utf16_len("😀"), 2);
        let runs = vec![run(2, 1), run(3, 0)];
        // "😀" + "ABC", with "X" typed straight after the emoji: it inherits
        // the emoji's run, and the emoji counts two.
        let out = splice_runs("😀ABC", "😀XABC", &runs);
        assert_eq!(out, vec![run(3, 1), run(3, 0)]);
    }

    /// The prefix must not end between a surrogate pair's halves: half a
    /// surrogate is not a position any text has, and a run boundary there
    /// would be a length no client can honour.
    #[test]
    fn a_common_prefix_never_splits_a_surrogate_pair() {
        // Two different emoji share their leading surrogate.
        let out = splice_runs("😀", "😁", &[run(2, 5)]);
        assert_eq!(out, vec![run(2, 5)]);
        assert!(runs_cover("😁", &out));
    }

    /// The property the whole splice exists for: after any edit, the runs
    /// still cover the text exactly.
    #[test]
    fn the_runs_always_still_cover_the_text() {
        let runs = vec![run(6, 1), run(5, 2), run(6, 3)];
        let old = "Title\n2345\n789012";
        assert!(runs_cover(old, &runs), "the fixture itself must cover the text");
        for new in [
            "Title\n2345\n789012", "", "T", "Title\n2345\n789012 and more",
            "Xitle\n2345\n789012", "Title\n789012", "totally different text",
            "Title\n2345\n78901", "😀Title\n2345\n789012",
        ] {
            let out = splice_runs(old, new, &runs);
            assert!(runs_cover(new, &out), "runs stopped covering {new:?}: {out:?}");
        }
    }

    // ── HTML → text ─────────────────────────────────────────────────────

    #[test]
    fn html_and_text_are_inverses_for_everything_the_reader_produces() {
        for text in [
            "", "one line", "one\ntwo", "a\n\nb", "soft\u{2028}break",
            "markup <b>&amp;</b> stays text", "ไทย\nสองบรรทัด", "trailing\n",
            "\n\nleading blanks\nbody",
        ] {
            assert_eq!(html_to_text(&doc::text_to_html(text)), text, "round trip failed on {text:?}");
        }
    }

    /// A `<br>` alone in a block is an empty line, not a soft break. Reading it
    /// as a soft break welds two paragraphs together — the same failure `U+2028`
    /// produced on the read side before it was measured.
    #[test]
    fn a_lone_br_is_an_empty_paragraph_and_an_inline_br_is_a_soft_break() {
        assert_eq!(html_to_text("<div>a</div><div><br></div><div>b</div>"), "a\n\nb");
        assert_eq!(html_to_text("<div>a<br>b</div>"), "a\u{2028}b");
    }

    /// **Whitespace BETWEEN block elements is markup, not content.** A
    /// serializer that pretty-prints — or, in practice, a paste — puts a
    /// newline between `</div>` and `<div>`, and reading it as content
    /// invents a blank line in the note's text. Measured live 2026-08-27:
    /// a note created in Jodd from pasted content pushed extra blank lines
    /// to iCloud, and the paragraph model built from the same scan gained a
    /// paragraph whose entire text was "\n" — which broke
    /// `reconcile_formatting`'s "one line per paragraph" invariant and
    /// silently downgraded every save of that note to text-only.
    ///
    /// Only whitespace with nothing accumulated and no block open is
    /// dropped: `<div>  spaced  </div>` keeps its spaces, and `<div> </div>`
    /// is still an empty paragraph.
    #[test]
    fn whitespace_between_blocks_is_markup_not_a_blank_line() {
        assert_eq!(html_to_text("<div>a</div>\n<div>b</div>"), "a\nb");
        assert_eq!(html_to_text("<div>a</div>\n  <div>b</div>"), "a\nb");
        assert_eq!(html_to_text("<div>a</div>\n"), "a");
        assert_eq!(html_to_text("\n<div>a</div>"), "a");
        assert_eq!(html_to_text("<ul>\n<li>a</li>\n<li>b</li>\n</ul>"), "a\nb");
        // Content whitespace is untouched.
        assert_eq!(html_to_text("<div>  spaced  </div>"), "  spaced  ");
        // A block holding only whitespace loses it and stays a LINE — the
        // stated cost above.
        assert_eq!(html_to_text("<div> </div><div>b</div>"), "\nb");
        assert_eq!(html_to_text("<div>a</div><div><br></div><div>b</div>"), "a\n\nb");
    }

    /// Gotcha #11's shape: a bare text node before the first block element.
    #[test]
    fn a_leading_text_node_is_its_own_paragraph() {
        assert_eq!(html_to_text("plain title<div>body</div>"), "plain title\nbody");
    }

    #[test]
    fn nesting_does_not_invent_a_trailing_empty_paragraph() {
        assert_eq!(html_to_text("<div><div>a</div></div>"), "a");
    }

    /// L3, stated as a test: formatting applied in Jodd does not reach iCloud,
    /// because deriving new runs needs the meaning of Apple's constants and
    /// this module deliberately never learns it. The user's TEXT still lands.
    #[test]
    fn inline_formatting_is_dropped_and_the_text_survives() {
        assert_eq!(html_to_text("<div>a <b>bold</b> word</div>"), "a bold word");
    }

    #[test]
    fn entities_come_back_as_the_characters_they_escaped() {
        assert_eq!(html_to_text("<div>a &amp; b &lt;c&gt; &quot;d&quot;</div>"), "a & b <c> \"d\"");
        assert_eq!(html_to_text("<div>&unknown; stays</div>"), "&unknown; stays");
    }

    // ── the title layer ─────────────────────────────────────────────────

    /// The round-trip property, over the same shapes `strip_leading_title`'s
    /// own tests use — including the two gotcha #21 called out as M2's problem.
    #[test]
    fn recompose_puts_back_exactly_what_strip_took_out() {
        for (text, title) in [
            ("Title\nbody", "Title"),
            ("Title", "Title"),
            ("Title\n", "Title"),
            ("\n\nTitle\nbody", "Title"),
            ("Title\nbody\nmore", "Title"),
            ("no title field here\nbody", ""),
            ("", ""),
        ] {
            let body = doc::strip_leading_title(text, title).body;
            assert_eq!(recompose(text, title, title, &body), text, "on {text:?}");
        }
    }

    /// Gotcha #21's first M2 obligation. The blank lines above the title are
    /// cut by the reader; composing `title + "\n" + body` would destroy them on
    /// the first push. Editing in place leaves them alone.
    #[test]
    fn empty_lines_above_the_title_survive_a_title_edit() {
        assert_eq!(recompose("\n\nOld\nbody", "Old", "New", "body"), "\n\nNew\nbody");
    }

    /// Gotcha #21's second — 41 of 776 notes on the live account. A title-only
    /// note has no separator to invent, so it must not grow a blank line per
    /// edit (which is gotcha #17's signature on Exchange).
    #[test]
    fn a_title_only_note_stays_title_only_however_often_it_is_edited() {
        let mut text = "Just a title".to_string();
        for i in 0..5 {
            let title = format!("Just a title {i}");
            let body = doc::strip_leading_title(&text, "Just a title").body;
            text = recompose(&text, "Just a title", &title, &body);
            assert_eq!(text, title);
        }
    }

    #[test]
    fn a_body_added_to_a_title_only_note_gets_the_separator_it_needs() {
        assert_eq!(recompose("Title", "Title", "Title", "new body"), "Title\nnew body");
    }

    #[test]
    fn a_note_created_in_jodd_composes_title_then_body() {
        assert_eq!(compose_new("T", "b"), "T\nb");
        assert_eq!(compose_new("T", ""), "T");
        assert_eq!(compose_new("", "b"), "b");
    }

    // ── the gate ────────────────────────────────────────────────────────

    #[test]
    fn an_ordinary_note_is_writable() {
        let d = plain("Title\nbody");
        assert!(writability(&encode(&d), "Title").is_ok());
    }

    #[test]
    fn a_body_that_does_not_decode_is_refused_before_anything_else() {
        assert_eq!(writability(b"\x00\x01\x02", "T"), Err(Unwritable::DoesNotRoundTrip));
    }

    /// Refusal (3). Per-character CRDT identity would have to be minted for a
    /// replica Jodd is not, and there is no correct value to invent.
    #[test]
    fn a_document_carrying_crdt_identity_is_refused_and_says_how_much() {
        let mut d = plain("abc");
        d.string.substring = vec![topotext::Substring {
            char_id: topotext::CharId { replica_id: 1, clock: 1 },
            length: 3,
            timestamp: topotext::CharId { replica_id: 1, clock: 1 },
            tombstone: None,
            child: Vec::new(),
        }];
        assert_eq!(
            writability(&encode(&d), ""),
            Err(Unwritable::CarriesCrdtIdentity { substrings: 1 })
        );
    }

    /// Refusal (4). If the arithmetic does not already hold on Apple's own
    /// document, splicing it would mis-attribute the whole note.
    #[test]
    fn runs_that_do_not_cover_the_text_are_refused_with_both_numbers() {
        let d = doc_with("abcdef", vec![run(2, 0)]);
        assert_eq!(
            writability(&encode(&d), ""),
            Err(Unwritable::RunsDoNotCoverText { runs: 1, run_total: 2, text_len: 6 })
        );
    }

    /// Refusal (5). `U+FFFC` is an attachment, a table or an inline hashtag —
    /// an object whose run carries its identity, and which the editor shows the
    /// user nothing of. With no `InlineAttachment` map, nothing resolves.
    #[test]
    fn a_note_with_inline_objects_is_refused_and_counts_them() {
        let d = plain("tag \u{FFFC} here \u{FFFC}");
        assert_eq!(writability(&encode(&d), ""), Err(Unwritable::InlineObjects { count: 2 }));
    }

    // ── the M3 narrowing of refusal (5) + the projection-aware layer gate ──

    fn hashtag_run(id: &str) -> topotext::AttributeRun {
        topotext::AttributeRun {
            length: 1,
            attachment_info: Some(topotext::AttachmentInfo {
                attachment_identifier: Some(id.to_string()),
                type_uti: Some("com.apple.notes.inlinetextattachment.hashtag".into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    fn one_ref(
        id: &str,
        uti: &str,
        alt: &str,
    ) -> std::collections::HashMap<String, super::super::wire::InlineRef> {
        let mut refs = std::collections::HashMap::new();
        refs.insert(
            id.to_string(),
            super::super::wire::InlineRef { type_uti: uti.into(), alt_text: alt.into() },
        );
        refs
    }

    #[test]
    /// **A resolvable hashtag does NOT make the note writable** — M3
    /// narrowed this refusal and then put it back on measurement (see the
    /// gate's own comment, and the probe two tests below). Reading the tag
    /// is the half that ships; writing the note that carries it is not.
    fn a_note_carrying_a_resolvable_hashtag_is_still_refused_for_writing() {
        let mut d = plain("Title\ntag \u{FFFC} here");
        // "Title\ntag " (10) + object (1) + " here" (5).
        d.string.attribute_run = vec![run(10, 0), hashtag_run("tag-1"), run(5, 0)];
        let refs = one_ref("tag-1", "com.apple.notes.inlinetextattachment.hashtag", "#x");
        assert_eq!(
            writability_with_refs(&encode(&d), "Title", &refs),
            Err(Unwritable::InlineObjects { count: 1 }),
            "resolving the tag is what makes it RENDER, never what makes the note writable"
        );
    }

    /// The measurement the refusal above rests on, kept as a test so the
    /// next person to re-narrow the gate has to face it: the text splice
    /// moves the object CHARACTER and leaves its `attachmentInfo` run
    /// behind, so the tag ends up pointing at nothing.
    ///
    /// When this test starts failing — i.e. when the write path learns to
    /// rebuild the run at the character's new position — the gate above can
    /// be narrowed again, and that change needs its own live pass.
    #[test]
    fn an_edit_that_moves_an_inline_object_loses_its_attachment_info() {
        let mut d = plain("T\nA \u{FFFC} B");
        d.string.attribute_run = vec![run(4, 0), hashtag_run("tag-1"), run(2, 0)];
        let base = parse(&encode(&d)).unwrap();
        // The user drags the tag to the end of the line.
        let moved = base.with_text("T\nA B \u{FFFC}");
        let refs = one_ref("tag-1", "com.apple.notes.inlinetextattachment.hashtag", "#x");
        assert!(
            doc::resolve_inline_objects(moved.text(), &moved.string.attribute_run, &refs)
                .iter()
                .all(|o| o.is_none()),
            "the object still resolves — the splice now carries attachmentInfo, so the \
             InlineObjects gate can be narrowed again (with its own live pass): {:?}",
            moved.string.attribute_run
        );
    }

    #[test]
    fn a_media_attachment_still_refuses_and_an_unresolvable_object_still_refuses() {
        let mut d = plain("Title\npic \u{FFFC}");
        d.string.attribute_run = vec![run(10, 0), hashtag_run("img-1")];
        // The record resolves but is NOT an inline-text attachment.
        let refs = one_ref("img-1", "public.jpeg", "");
        assert_eq!(
            writability_with_refs(&encode(&d), "Title", &refs),
            Err(Unwritable::InlineObjects { count: 1 })
        );
        // No record at all: refused, as today.
        assert!(matches!(
            writability(&encode(&d), "Title"),
            Err(Unwritable::InlineObjects { .. })
        ));
    }

    #[test]
    fn a_formatted_note_that_projects_cleanly_passes_the_upgraded_layer_gate() {
        let d = doc_with(
            "Title\nHead\nitem",
            vec![
                run(6, 0),
                topotext::AttributeRun {
                    length: 5,
                    paragraph_style: Some(topotext::ParagraphStyle { style: Some(1), ..Default::default() }),
                    ..Default::default()
                },
                topotext::AttributeRun {
                    length: 4,
                    paragraph_style: Some(topotext::ParagraphStyle { style: Some(100), ..Default::default() }),
                    ..Default::default()
                },
            ],
        );
        let result = writability(&encode(&d), "Title");
        assert!(result.is_ok(), "expected writable, got {result:?}");
    }

    /// A note whose format decodes but whose projection does NOT survive the
    /// render/parse trip refuses as `LayersDoNotRoundTrip` — the gate the
    /// live phase measures against the 14-note baseline. `U+FFFC` inside a
    /// paragraph resolved as a hashtag whose ALT TEXT differs in shape is
    /// exercised elsewhere; here an unknown style (undecodable) falls back
    /// to the PLAIN gate and stays writable exactly as M2 left it.
    #[test]
    fn an_undecodable_format_keeps_m2s_plain_gate_verdict() {
        let d = doc_with("Title\nbody", vec![
            topotext::AttributeRun {
                length: 10,
                paragraph_style: Some(topotext::ParagraphStyle { style: Some(77), ..Default::default() }),
                ..Default::default()
            },
        ]);
        assert!(writability(&encode(&d), "Title").is_ok());
    }

    /// The refusal that nearly ate a quarter of the account. `TitleEncrypted`
    /// is a lossy derivation of the first line on 215 of 776 real notes, so a
    /// guard that compared against it would have made one note in four
    /// read-only — measuring gotcha #21's known gap rather than anything about
    /// this note's writability.
    #[test]
    fn a_note_whose_title_field_is_truncated_is_still_writable() {
        let line = "A first line long enough that Apple shortened it for its own list";
        let d = plain(&format!("{line}\nbody"));
        assert!(
            writability(&encode(&d), "A first line long enough that Apple…").is_ok(),
            "the record's title field only verifies — it must never decide the cut"
        );
    }

    /// Refusal (6). A separator Jodd's own HTML cannot carry would be silently
    /// rewritten on the user's next save; refusing says so instead.
    #[test]
    fn text_jodds_own_html_cannot_reproduce_is_refused() {
        // A `\r` inside the BODY: `text_to_html` strips it rather than trusting
        // it, so what Jodd would send back differs from what it was given —
        // before the user has changed anything.
        let d = plain("Title\nbody\r\nmore");
        assert_eq!(writability(&encode(&d), "Title"), Err(Unwritable::LayersDoNotRoundTrip));

        // And `U+2029`, which no note in the 776-note corpus carried but which
        // `text_to_html` treats as a paragraph break: writing it back as `\n`
        // would be a silent change to the remote on the user's next save.
        let d = plain("Title\nbody\u{2029}more");
        assert_eq!(writability(&encode(&d), "Title"), Err(Unwritable::LayersDoNotRoundTrip));
    }

    /// Every refusal has to be readable by the person it stops, and has to name
    /// where the note *can* be edited — Jodd is not that place for these.
    #[test]
    fn every_refusal_names_apple_notes_as_the_way_out() {
        for u in [
            Unwritable::Locked,
            Unwritable::DoesNotRoundTrip,
            Unwritable::CarriesCrdtIdentity { substrings: 1 },
            Unwritable::RunsDoNotCoverText { runs: 1, run_total: 1, text_len: 2 },
            Unwritable::InlineObjects { count: 1 },
            Unwritable::LayersDoNotRoundTrip,
        ] {
            assert!(u.to_string().contains("Apple Notes"), "{u:?} gives the user nowhere to go");
        }
    }

    /// A document carrying real CRDT identity that this engine CAN parse
    /// and validate must now be writable, not refused — the whole point of
    /// M2.5. Uses the exact shape crdt::tests::simple_document builds, by
    /// hand here since compose.rs's tests can't import crdt's private test
    /// helpers.
    #[test]
    fn a_document_the_crdt_engine_can_parse_and_validate_is_now_writable() {
        let d = NoteDocument {
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
                attribute_run: vec![run(10, 0)],
            },
            serialization_version: Some(0),
            version_serialization_version: Some(0),
            minimum_supported_version: Some(0),
            crdt: None,
        };
        let result = writability(&encode(&d), "Title");
        assert!(result.is_ok(), "expected the CRDT engine to accept this document, got {result:?}");
        let parsed = result.unwrap();
        assert!(parsed.crdt.is_some());
    }

    /// A document carrying a shape the CRDT engine refuses (a `subclock`)
    /// stays refused with the same `CarriesCrdtIdentity` reason as before —
    /// nothing regresses for documents this port doesn't understand.
    #[test]
    fn a_crdt_document_this_engine_cannot_parse_stays_refused() {
        let mut d = plain("abc");
        d.string.substring = vec![topotext::Substring {
            char_id: topotext::CharId { replica_id: 1, clock: 1 },
            length: 3,
            timestamp: topotext::CharId { replica_id: 1, clock: 1 },
            tombstone: None,
            child: Vec::new(),
        }];
        d.string.timestamp = Some(topotext::VectorTimestamp {
            clock: vec![topotext::vector_timestamp::Clock {
                replica_uuid: vec![0xAA; 16],
                replica_clock: vec![topotext::vector_timestamp::clock::ReplicaClock { clock: 3, subclock: Some(1) }],
            }],
        });
        assert!(matches!(
            writability(&encode(&d), ""),
            Err(Unwritable::CarriesCrdtIdentity { .. })
        ));
    }

    /// Fix for the misreport this milestone's final review flagged: the CRDT
    /// branch used to map EVERY `validate_document_invariants` failure to
    /// `CarriesCrdtIdentity`, including the one condition
    /// (`AttributeLengthMismatch`) that the plain, non-CRDT path already
    /// refuses more specifically as `RunsDoNotCoverText`. A CRDT-carrying
    /// note whose runs simply don't cover its text should get the specific
    /// reason, not the CRDT-identity one — they are different problems and
    /// `notes.push_blocked_reason` should say which.
    #[test]
    fn a_crdt_document_with_uncovered_runs_is_refused_as_runs_do_not_cover_text() {
        let d = NoteDocument {
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
                // Visible run lengths above sum to 10, matching the text — but
                // this sums to 5, so only AttributeLengthMismatch fires.
                attribute_run: vec![run(5, 0)],
            },
            serialization_version: Some(0),
            version_serialization_version: Some(0),
            minimum_supported_version: Some(0),
            crdt: None,
        };
        assert_eq!(
            writability(&encode(&d), "Title"),
            Err(Unwritable::RunsDoNotCoverText { runs: 1, run_total: 5, text_len: 10 })
        );
    }

    /// Fix for the same-shaped defect on the `with_text` side: it must never
    /// be called on a CRDT-carrying document (it only re-lengths
    /// `attribute_run` and leaves `substring`/`timestamp` describing the OLD
    /// text) — `with_text_crdt` is the only correct entry point once
    /// `writability()` has returned `crdt: Some(...)`.
    #[test]
    #[should_panic(expected = "with_text_crdt")]
    fn with_text_panics_on_a_crdt_carrying_document() {
        let d = NoteDocument {
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
                attribute_run: vec![run(10, 0)],
            },
            serialization_version: Some(0),
            version_serialization_version: Some(0),
            minimum_supported_version: Some(0),
            crdt: None,
        };
        let parsed = writability(&encode(&d), "Title").unwrap();
        assert!(parsed.crdt.is_some(), "fixture must actually be CRDT-carrying for this test to mean anything");
        let _ = parsed.with_text("new text entirely");
    }

    #[test]
    fn with_text_crdt_performs_a_real_edit_and_stays_writable() {
        let d = NoteDocument {
            string: topotext::String {
                string: "Hello".into(),
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
                        length: 5,
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
                            topotext::vector_timestamp::clock::ReplicaClock { clock: 5, subclock: None },
                            topotext::vector_timestamp::clock::ReplicaClock { clock: 1, subclock: None },
                        ],
                    }],
                }),
                attribute_run: vec![run(5, 0)],
            },
            serialization_version: Some(0),
            version_serialization_version: Some(0),
            minimum_supported_version: Some(0),
            crdt: None,
        };
        let parsed = writability(&encode(&d), "Hello").unwrap();
        let edited = parsed.with_text_crdt("Hello there", [0xBB; 16]).unwrap();
        assert_eq!(edited.text(), "Hello there");
        // The result is itself writable — an edit must not make a note
        // read-only for the next one.
        assert!(writability(&encode(&edited), "Hello there").is_ok());
    }

    /// The whole point of the design, as one assertion: a note with formatting
    /// nothing here understands is edited, and comes back with that formatting
    /// intact and re-lengthed.
    #[test]
    fn an_edit_preserves_formatting_this_module_cannot_read() {
        let d = doc_with("Title\nbody", vec![run(5, 1), run(5, 2)]);
        let compressed = encode(&d);
        let parsed = writability(&compressed, "Title").unwrap();

        let body = doc::strip_leading_title(parsed.text(), "Title").body;
        let edited = recompose(parsed.text(), "Title", "Title", &format!("{body} more"));
        let out = parse(&encode(&parsed.with_text(&edited))).unwrap();

        assert_eq!(out.text(), "Title\nbody more");
        assert_eq!(out.string.attribute_run, vec![run(5, 1), run(10, 2)]);
        // And the result is itself writable — an edit must not make a note
        // read-only for the next one.
        assert!(writability(&encode(&out), "Title").is_ok());
    }
}
