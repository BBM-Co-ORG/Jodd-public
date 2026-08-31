//! CRDT text-editing engine for iCloud note documents.
//!
//! Ported from icloud-md's `noteDocument.ts` (MIT,
//! github.com/coddingtonbear/icloud-md), the same provenance as the vendored
//! `proto/*.proto` files (`proto/PROVENANCE.md`). `compose.rs`'s
//! `CarriesCrdtIdentity` refusal names the problem this module solves:
//! `topotext.String.substring` carries Apple's per-character CRDT identity —
//! a DAG of `Substring` records linked by `child` edges, each anchored to a
//! `(replica, clock)` coordinate — and editing it means minting new
//! identities as a replica, in an order that merges correctly against every
//! other replica that has ever touched the document.
//!
//! Operates on the prost-generated `topotext::{String, Substring, CharId,
//! VectorTimestamp}` types at its edges; internally uses a domain model
//! (`TextRun`/`ReplicaEntry`) matching icloud-md's own split, so the edit
//! logic doesn't have to think in wire-format terms. Two refusals this
//! module inherits directly from icloud-md, because the model does not
//! carry them through an edit and a document using them must be refused
//! rather than silently mis-encoded: a `subclock` on any replica clock
//! entry (never observed live), and a missing replica clock table
//! (`topotext.String.timestamp`) on a document that otherwise carries
//! `substring` data.

use super::compose::{is_high_surrogate, is_low_surrogate, utf16_len};
use super::gen::topotext;

/// The clock value marking the end-of-document sentinel run — never a real
/// edit position.
pub const SENTINEL_CLOCK: u32 = 0xFFFF_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunCoord {
    pub replica: u32,
    pub clock: u32,
}

/// One CRDT run: `TextRun` = wire `Substring`, `anchor` = `Substring.timestamp`
/// (the run's *style* clock, not a position), `sequence` = `Substring.child`
/// — outgoing DAG edges, 0-based indexes into the document's runs array,
/// always pointing at a later index (Apple serializes in topological order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRun {
    pub coord: RunCoord,
    pub length: u32,
    pub anchor: RunCoord,
    pub tombstone: bool,
    pub sequence: Vec<usize>,
}

/// One replica's row in the clock table. `counters[0]` = text clock (total
/// UTF-16 units ever inserted), `counters[1]` = style clock; any further
/// entries have unknown meaning and are preserved verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaEntry {
    pub id: [u8; 16],
    pub counters: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CrdtDocument {
    pub text: String,
    pub runs: Vec<TextRun>,
    pub replicas: Vec<ReplicaEntry>,
    pub attribute_runs: Vec<topotext::AttributeRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrdtError {
    /// `topotext.String.timestamp` (the replica clock table) is absent.
    MissingTimestampTable,
    /// A replica clock entry carries a `subclock` — never observed live;
    /// this model doesn't carry it through an edit, so refuse rather than
    /// silently drop it.
    SubclockPresent,
    /// A `Substring.tombstone` field is set to something other than `1`.
    InvalidTombstoneValue(u32),
    /// A replica clock table entry's UUID isn't 16 bytes.
    ReplicaUuidWrongLength(usize),
    /// Visible (non-tombstoned) run lengths don't sum to the text length.
    VisibleLengthMismatch { runs: u64, text: usize },
    /// Attribute run lengths don't sum to the text length.
    AttributeLengthMismatch { runs: u64, text: usize },
    /// A run's coord names a replica outside the replica table.
    RunReplicaOutOfRange { replica: u32, table_len: usize },
    /// A run's clocks exceed its replica's own counter.
    RunClockExceedsCounter { replica: u32, clock: u32, length: u32, counter: u32 },
    /// A non-sentinel run has no outgoing child edge at all.
    MissingChildEdge { index: usize },
    /// A child edge doesn't point strictly forward within range.
    ChildEdgeOutOfRange { index: usize, child: usize },
}

impl std::fmt::Display for CrdtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CrdtError::MissingTimestampTable => {
                write!(f, "note document is missing its replica clock table")
            }
            CrdtError::SubclockPresent => {
                write!(f, "replica clock entry carries a subclock this model doesn't understand")
            }
            CrdtError::InvalidTombstoneValue(v) => {
                write!(f, "substring tombstone flag has unexpected value {v}")
            }
            CrdtError::ReplicaUuidWrongLength(len) => {
                write!(f, "replica clock entry UUID is {len} bytes, expected 16")
            }
            CrdtError::VisibleLengthMismatch { runs, text } => {
                write!(f, "visible run lengths ({runs}) do not match text length ({text})")
            }
            CrdtError::AttributeLengthMismatch { runs, text } => {
                write!(f, "attribute run lengths ({runs}) do not match text length ({text})")
            }
            CrdtError::RunReplicaOutOfRange { replica, table_len } => {
                write!(f, "run references replica {replica} outside the {table_len}-entry replica table")
            }
            CrdtError::RunClockExceedsCounter { replica, clock, length, counter } => {
                write!(f, "run clocks exceed replica {replica}'s counter ({clock}+{length} > {counter})")
            }
            CrdtError::MissingChildEdge { index } => {
                write!(f, "run {index} has no child edge")
            }
            CrdtError::ChildEdgeOutOfRange { index, child } => {
                write!(f, "run {index} has a child edge to {child}, outside the forward range")
            }
        }
    }
}

pub fn is_sentinel(run: &TextRun) -> bool {
    run.coord.clock == SENTINEL_CLOCK
}

// ── parsing ─────────────────────────────────────────────────────────────

pub fn parse_crdt_document(s: &topotext::String) -> Result<CrdtDocument, CrdtError> {
    let vt = s.timestamp.as_ref().ok_or(CrdtError::MissingTimestampTable)?;
    let replicas = vt.clock.iter().map(parse_replica_entry).collect::<Result<Vec<_>, _>>()?;
    let runs = s.substring.iter().map(parse_text_run).collect::<Result<Vec<_>, _>>()?;
    Ok(CrdtDocument {
        text: s.string.clone(),
        runs,
        replicas,
        attribute_runs: s.attribute_run.clone(),
    })
}

fn parse_text_run(run: &topotext::Substring) -> Result<TextRun, CrdtError> {
    let tombstone = match run.tombstone {
        None => false,
        Some(1) => true,
        Some(v) => return Err(CrdtError::InvalidTombstoneValue(v)),
    };
    Ok(TextRun {
        coord: RunCoord { replica: run.char_id.replica_id, clock: run.char_id.clock },
        length: run.length,
        anchor: RunCoord { replica: run.timestamp.replica_id, clock: run.timestamp.clock },
        tombstone,
        sequence: run.child.iter().map(|&c| c as usize).collect(),
    })
}

fn parse_replica_entry(entry: &topotext::vector_timestamp::Clock) -> Result<ReplicaEntry, CrdtError> {
    if entry.replica_uuid.len() != 16 {
        return Err(CrdtError::ReplicaUuidWrongLength(entry.replica_uuid.len()));
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(&entry.replica_uuid);
    let counters = entry
        .replica_clock
        .iter()
        .map(|c| if c.subclock.is_some() { Err(CrdtError::SubclockPresent) } else { Ok(c.clock) })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ReplicaEntry { id, counters })
}

// ── encoding ────────────────────────────────────────────────────────────

pub fn encode_crdt_document(doc: &CrdtDocument) -> topotext::String {
    topotext::String {
        string: doc.text.clone(),
        substring: doc.runs.iter().map(encode_text_run).collect(),
        timestamp: Some(topotext::VectorTimestamp {
            clock: doc.replicas.iter().map(encode_replica_entry).collect(),
        }),
        attribute_run: doc.attribute_runs.clone(),
    }
}

fn encode_text_run(run: &TextRun) -> topotext::Substring {
    topotext::Substring {
        char_id: topotext::CharId { replica_id: run.coord.replica, clock: run.coord.clock },
        length: run.length,
        timestamp: topotext::CharId { replica_id: run.anchor.replica, clock: run.anchor.clock },
        tombstone: if run.tombstone { Some(1) } else { None },
        child: run.sequence.iter().map(|&c| c as u32).collect(),
    }
}

fn encode_replica_entry(entry: &ReplicaEntry) -> topotext::vector_timestamp::Clock {
    topotext::vector_timestamp::Clock {
        replica_uuid: entry.id.to_vec(),
        replica_clock: entry
            .counters
            .iter()
            .map(|&clock| topotext::vector_timestamp::clock::ReplicaClock { clock, subclock: None })
            .collect(),
    }
}

// ── invariants ──────────────────────────────────────────────────────────

/// Pre/post-condition check every CRDT edit must satisfy: run and
/// attribute-run lengths agree with the text, every run's clocks stay
/// within its replica's own counter, and the child-edge graph is sane
/// (delegated to [`validate_child_edges`]).
pub fn validate_document_invariants(doc: &CrdtDocument) -> Result<(), CrdtError> {
    let visible_length: u64 =
        doc.runs.iter().filter(|r| !r.tombstone).map(|r| r.length as u64).sum();
    let text_len = utf16_len(&doc.text);
    if visible_length != text_len as u64 {
        return Err(CrdtError::VisibleLengthMismatch { runs: visible_length, text: text_len });
    }

    let attr_length: u64 = doc.attribute_runs.iter().map(|r| r.length as u64).sum();
    if attr_length != text_len as u64 {
        return Err(CrdtError::AttributeLengthMismatch { runs: attr_length, text: text_len });
    }

    for run in &doc.runs {
        if is_sentinel(run) {
            continue;
        }
        if run.coord.replica as usize > doc.replicas.len() {
            return Err(CrdtError::RunReplicaOutOfRange {
                replica: run.coord.replica,
                table_len: doc.replicas.len(),
            });
        }
        if run.coord.replica != 0 {
            let counter = doc.replicas[run.coord.replica as usize - 1]
                .counters
                .first()
                .copied()
                .unwrap_or(0);
            if run.coord.clock as u64 + run.length as u64 > counter as u64 {
                return Err(CrdtError::RunClockExceedsCounter {
                    replica: run.coord.replica,
                    clock: run.coord.clock,
                    length: run.length,
                    counter,
                });
            }
        }
    }

    validate_child_edges(&doc.runs)
}

/// Sanity for the child-edge graph: every non-sentinel run has at least one
/// outgoing edge, and every edge points strictly forward and in range —
/// Apple's save order is a topological traversal, so any document a real
/// client wrote satisfies this, and forward-only edges can't form a cycle.
pub fn validate_child_edges(runs: &[TextRun]) -> Result<(), CrdtError> {
    for (index, run) in runs.iter().enumerate() {
        if is_sentinel(run) {
            continue;
        }
        if run.sequence.is_empty() {
            return Err(CrdtError::MissingChildEdge { index });
        }
        for &child in &run.sequence {
            if child <= index || child >= runs.len() {
                return Err(CrdtError::ChildEdgeOutOfRange { index, child });
            }
        }
    }
    Ok(())
}

// ── child-edge surgery ─────────────────────────────────────────────────

/// Adds `delta` to every child edge across `runs` pointing at an array
/// index `>= threshold` — the bookkeeping every runs-array splice owes the
/// graph, since edges are stored as array indexes.
fn shift_child_edges(runs: &mut [TextRun], threshold: usize, delta: isize) {
    for run in runs.iter_mut() {
        for child in run.sequence.iter_mut() {
            if *child >= threshold {
                *child = (*child as isize + delta) as usize;
            }
        }
    }
}

/// Splits `runs[index]` at `offset` (`0 < offset < length`) into head and
/// tail in place — Apple's `splitTopoSubstring_atIndex`: the tail inherits
/// the head's outgoing child edges, the head's only child becomes the tail,
/// and every edge that pointed at the split run keeps pointing at the head
/// (which keeps the run's coord, so edges by index stay correct). The tail
/// lands at `index + 1`; returns its index.
pub fn split_run_at(runs: &mut Vec<TextRun>, index: usize, offset: u32) -> usize {
    let run = &runs[index];
    assert!(
        offset > 0 && offset < run.length,
        "cannot split run {index} at offset {offset} (length {}) — CRDT model out of sync",
        run.length
    );
    shift_child_edges(runs, index + 1, 1);
    let run = &runs[index];
    let tail = TextRun {
        coord: RunCoord { replica: run.coord.replica, clock: run.coord.clock + offset },
        length: run.length - offset,
        anchor: run.anchor,
        tombstone: run.tombstone,
        sequence: run.sequence.clone(),
    };
    runs[index].length = offset;
    runs[index].sequence = vec![index + 1];
    runs.insert(index + 1, tail);
    index + 1
}

/// Splices a freshly created `run` into the array at `index` and into the
/// child graph between its new array neighbours — Apple's
/// `insertAttributedString_after_before`: when the predecessor has an edge
/// to the run being displaced, the new run takes that edge's place
/// (predecessor -> new -> successor); when it doesn't (a branched graph
/// where the array neighbours aren't graph-linked), the new run takes over
/// ALL of the predecessor's children and becomes its only child.
pub fn insert_run_at(runs: &mut Vec<TextRun>, index: usize, mut run: TextRun) {
    shift_child_edges(runs, index, 1);
    if index > 0 {
        // The displaced successor sat at `index`; after the shift above,
        // edges to it read `index + 1`.
        let successor_edge = runs[index - 1].sequence.iter().position(|&c| c == index + 1);
        if let Some(pos) = successor_edge {
            runs[index - 1].sequence[pos] = index;
            run.sequence = vec![index + 1];
        } else {
            run.sequence = std::mem::take(&mut runs[index - 1].sequence);
            runs[index - 1].sequence = vec![index];
        }
    } else {
        // No predecessor: a document without the usual zero-length origin
        // lead run (never observed in a capture). The new run becomes a
        // start node pointing at the run it displaced.
        run.sequence = vec![index + 1];
    }
    runs.insert(index, run);
}

// ── multi-hunk text diff ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    /// UTF-16 code-unit offset into the OLD text.
    pub start: usize,
    pub delete_length: usize,
    pub insert_text: String,
}

/// Minimal single-region diff over UTF-16 code units, never splitting a
/// surrogate pair.
pub fn compute_splice(old: &[u16], new: &[u16]) -> Splice {
    let max_prefix = old.len().min(new.len());
    let mut prefix = 0;
    while prefix < max_prefix && old[prefix] == new[prefix] {
        prefix += 1;
    }
    if prefix > 0 && is_high_surrogate(old[prefix - 1]) {
        prefix -= 1;
    }

    let max_suffix = old.len().min(new.len()) - prefix;
    let mut suffix = 0;
    while suffix < max_suffix && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix] {
        suffix += 1;
    }
    if suffix > 0 && is_low_surrogate(old[old.len() - suffix]) {
        suffix -= 1;
    }

    let insert_units = &new[prefix..new.len() - suffix];
    Splice {
        start: prefix,
        delete_length: old.len() - prefix - suffix,
        insert_text: String::from_utf16_lossy(insert_units),
    }
}

/// Splits into lines with their "\n" terminators kept attached, so line
/// indexes convert to UTF-16 offsets by plain accumulation.
fn split_lines_inclusive(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.split('\n').map(|l| format!("{l}\n")).collect();
    if lines.last().map(String::as_str) == Some("\n") {
        lines.pop();
    } else if let Some(last) = lines.last_mut() {
        last.pop();
    }
    lines
}

/// `result[i]` = UTF-16 offset where line `i` starts; one trailing entry
/// (the text's total length) so a hunk starting past the last line — a pure
/// append — still resolves.
fn line_start_offsets(lines: &[String]) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for line in lines {
        let last = *offsets.last().unwrap();
        offsets.push(last + utf16_len(line));
    }
    offsets
}

struct LineHunk {
    old_range: std::ops::Range<usize>,
    new_range: std::ops::Range<usize>,
}

/// Line-level LCS via the classic O(N·M) DP-table backtrack — note bodies
/// are small enough that this is not a performance concern, and it avoids a
/// new dependency for what icloud-md gets from `node-diff3`'s `diffIndices`.
/// Returns only the CHANGED regions (consecutive non-matching lines
/// coalesced into one hunk each), never the matching stretches between them
/// — this is what keeps untouched text between two edits out of any hunk.
fn line_diff_hunks(old_lines: &[String], new_lines: &[String]) -> Vec<LineHunk> {
    let n = old_lines.len();
    let m = new_lines.len();
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = if old_lines[i - 1] == new_lines[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    enum Op {
        Equal,
        Delete,
        Insert,
    }
    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old_lines[i - 1] == new_lines[j - 1] {
            ops.push(Op::Equal);
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            ops.push(Op::Insert);
            j -= 1;
        } else {
            ops.push(Op::Delete);
            i -= 1;
        }
    }
    ops.reverse();

    let mut hunks = Vec::new();
    let (mut oi, mut ni) = (0usize, 0usize);
    let mut current: Option<(usize, usize)> = None;
    for op in &ops {
        match op {
            Op::Equal => {
                if let Some((ho, hn)) = current.take() {
                    hunks.push(LineHunk { old_range: ho..oi, new_range: hn..ni });
                }
                oi += 1;
                ni += 1;
            }
            Op::Delete => {
                current.get_or_insert((oi, ni));
                oi += 1;
            }
            Op::Insert => {
                current.get_or_insert((oi, ni));
                ni += 1;
            }
        }
    }
    if let Some((ho, hn)) = current {
        hunks.push(LineHunk { old_range: ho..oi, new_range: hn..ni });
    }
    hunks
}

/// Multi-hunk diff over UTF-16 code units: line-level LCS locates each
/// changed region, then [`compute_splice`] tightens every hunk to character
/// precision. Returned splices carry non-overlapping, ascending `old_text`
/// offsets. This is what keeps a CRDT edit minimal when the text differs
/// from the server text in more than one place — only genuinely changed
/// characters are tombstoned/inserted, and everything between hunks keeps
/// its original runs and authorship (see [`apply_text_edit`]'s doc comment
/// for the fusion bug this prevents).
pub fn compute_splices(old_text: &str, new_text: &str) -> Vec<Splice> {
    if old_text == new_text {
        return Vec::new();
    }
    let old_lines = split_lines_inclusive(old_text);
    let new_lines = split_lines_inclusive(new_text);
    let old_offsets = line_start_offsets(&old_lines);

    let mut splices = Vec::new();
    for hunk in line_diff_hunks(&old_lines, &new_lines) {
        let old_start = old_offsets[hunk.old_range.start];
        let old_hunk: String = old_lines[hunk.old_range.clone()].concat();
        let new_hunk: String = new_lines[hunk.new_range.clone()].concat();
        let old_units: Vec<u16> = old_hunk.encode_utf16().collect();
        let new_units: Vec<u16> = new_hunk.encode_utf16().collect();
        let inner = compute_splice(&old_units, &new_units);
        if inner.delete_length == 0 && inner.insert_text.is_empty() {
            continue;
        }
        splices.push(Splice {
            start: old_start + inner.start,
            delete_length: inner.delete_length,
            insert_text: inner.insert_text,
        });
    }
    splices
}

// ── replica table management ───────────────────────────────────────────

/// Puts `replica_id` FIRST in the replica table — index 1 — renumbering
/// every run's replica references to the reordered table, and returns 1.
///
/// First, not appended, and that is a capture, not a style choice: Apple's
/// own web client, performing a mid-text edit on a Mac-typed note
/// (ground-truth capture, 2026-08-26, note `0BF5E08B`), serialized the
/// document with ITS replica at index 1 and the Mac's — previously index
/// 1 — renumbered to 2 on every run. Each client writes the table with
/// itself first, exactly what a serializer of TTMergeableString's
/// in-memory state (where "self" is replica 0) would produce. Jodd's
/// first engine APPENDED itself instead, which made its documents the
/// only ones in existence claiming another device as the table's head —
/// and after the same capture eliminated the document runs, the write
/// envelope, the write shape, and the replica-registration maps as
/// differences, table order is the one structural divergence left
/// standing between writes Apple's clients merge cleanly and writes they
/// merge by duplicating the note.
///
/// A replica joining an existing document initializes both clocks to the
/// maxima observed across the table — Apple's own fresh web replica picks
/// up the previous session's text clock (measured: the web client's first
/// insert started at the Mac's counter, 23) and continues the global
/// formatting-op numbering, so its ops win LWW against every older op, as
/// intended. On a brand-new document (empty table) both maxima are zero.
pub fn ensure_replica(doc: &mut CrdtDocument, replica_id: [u8; 16]) -> usize {
    let old_pos = doc.replicas.iter().position(|r| r.id == replica_id);
    if old_pos == Some(0) {
        return 1;
    }

    // Reorder the table: us first, everyone else in their existing order.
    let entry = match old_pos {
        Some(pos) => doc.replicas.remove(pos),
        None => {
            let max_text_clock =
                doc.replicas.iter().map(|r| r.counters.first().copied().unwrap_or(0)).max().unwrap_or(0);
            let max_op_clock =
                doc.replicas.iter().map(|r| r.counters.get(1).copied().unwrap_or(0)).max().unwrap_or(0);
            ReplicaEntry { id: replica_id, counters: vec![max_text_clock, max_op_clock] }
        }
    };
    doc.replicas.insert(0, entry);

    // Renumber every run's references: old 1-based index -> new 1-based
    // index. Index 0 is the origin/sentinel pseudo-replica and never moves.
    let remap = |index: u32| -> u32 {
        if index == 0 {
            return 0;
        }
        let old = (index - 1) as usize;
        let new = match old_pos {
            // Table before: [others...]; a run's old index shifts down by
            // one past the removal point, then everything moves up one for
            // the insertion at the front.
            Some(pos) if old == pos => 0,
            Some(pos) if old > pos => old, // -1 for removal, +1 for insert
            _ => old + 1,                  // no removal before it; +1 for insert
        };
        (new + 1) as u32
    };
    for run in &mut doc.runs {
        if is_sentinel(run) {
            continue;
        }
        run.coord.replica = remap(run.coord.replica);
        run.anchor.replica = remap(run.anchor.replica);
    }
    1
}

/// The style-clock floor for one editing pass (mirrors Apple's
/// `updateClock`): the highest anchor clock any run in the document
/// carries — tie-broken by replica UUID byte comparison (`[u8; 16]`'s
/// native lexicographic `Ord`, the same ordering the CRDT merge's
/// last-write-wins uses) — plus one when that stamp's holder would beat us
/// in the tie-break, so our new stamps never lose to anything already in
/// the document. Floored at our own replica's own style counter so it
/// never regresses. Replica 0 is the origin/sentinel pseudo-replica and
/// carries no real stamps.
pub fn style_clock_seed(doc: &CrdtDocument, replica_index: usize) -> u32 {
    let our_id = doc.replicas[replica_index - 1].id;
    let mut max_clock: i64 = -1;
    let mut max_holder: Option<[u8; 16]> = None;
    for run in &doc.runs {
        if run.anchor.replica == 0 {
            continue;
        }
        let holder = doc.replicas.get(run.anchor.replica as usize - 1).map(|r| r.id).unwrap_or([0u8; 16]);
        let clock = run.anchor.clock as i64;
        let beats_current = clock > max_clock || (clock == max_clock && max_holder.is_some_and(|h| holder > h));
        if beats_current {
            max_clock = clock;
            max_holder = Some(holder);
        }
    }
    let mut seed = 0i64;
    if let Some(holder) = max_holder {
        seed = max_clock + if holder >= our_id { 1 } else { 0 };
    }
    let own_counter = doc.replicas[replica_index - 1].counters.get(1).copied().unwrap_or(0);
    seed.max(own_counter as i64) as u32
}

// ── tombstone / insert / attribute-run adjustment ──────────────────────

/// Marks the visible range `[start, start+length)` as tombstoned, splitting
/// runs at the boundaries ([`split_run_at`]), and restamps each newly
/// tombstoned piece's anchor with `(replica_index, max(old anchor clock +
/// 8, style_clock_floor))` — Apple's deletion-bias rule, which lets the
/// deletion win the merge-time last-write-wins against up to 8 clock steps
/// of concurrent restyling. Returns the highest clock stamped, or `None`
/// when the range covered nothing.
pub fn tombstone_visible_range(
    doc: &mut CrdtDocument,
    start: usize,
    length: usize,
    replica_index: usize,
    style_clock_floor: u32,
) -> Option<u32> {
    let end = start + length;
    let mut visible = 0usize;
    let mut max_assigned: Option<u32> = None;
    let mut i = 0usize;
    while i < doc.runs.len() && visible < end {
        let run = &doc.runs[i];
        if run.tombstone || run.length == 0 || is_sentinel(run) {
            i += 1;
            continue;
        }
        let run_start = visible;
        let run_end = run_start + run.length as usize;
        if run_end <= start {
            visible = run_end;
            i += 1;
            continue;
        }

        let mut target_index = i;
        let mut target_start = run_start;
        if start > run_start {
            target_index = split_run_at(&mut doc.runs, i, (start - run_start) as u32);
            target_start = start;
        }
        let target_len = doc.runs[target_index].length as usize;
        if end < target_start + target_len {
            split_run_at(&mut doc.runs, target_index, (end - target_start) as u32);
        }
        let assigned = doc.runs[target_index].anchor.clock.saturating_add(8).max(style_clock_floor);
        doc.runs[target_index].tombstone = true;
        doc.runs[target_index].anchor = RunCoord { replica: replica_index as u32, clock: assigned };
        max_assigned = Some(max_assigned.map_or(assigned, |m| m.max(assigned)));
        visible = target_start + doc.runs[target_index].length as usize;
        i = target_index;
    }
    max_assigned
}

/// Writes a replica's text-clock slot (`counters[0]`), growing the vector
/// rather than panicking if a malformed replica's `counters` is empty.
/// `validate_document_invariants` already tolerates that shape on read via
/// `.first().copied().unwrap_or(0)` (treating an absent counter as 0), so a
/// document that clears validation can still reach here with an empty
/// `counters` — an index-assignment on `[0]` would panic where the read side
/// does not.
fn set_text_clock(replica: &mut ReplicaEntry, value: u32) {
    match replica.counters.first_mut() {
        Some(slot) => *slot = value,
        None => replica.counters.push(value),
    }
}

/// Inserts `text` (UTF-16 units) at visible position `start`, extending the
/// replica's own trailing run when contiguous (byte-for-byte what a real
/// client's own append does), otherwise adding a new run anchored at
/// `(replica_index, 0)` — "never restyled", what a plain typed run gets.
/// Returns whether a structural change (a new run) was made, as opposed to
/// a pure extension.
pub fn insert_visible_text(doc: &mut CrdtDocument, start: usize, text: &[u16], replica_index: usize) -> bool {
    let clock = doc.replicas[replica_index - 1].counters.first().copied().unwrap_or(0);

    let mut visible = 0usize;
    let mut insert_index = doc.runs.len();
    let mut i = 0usize;
    while i < doc.runs.len() {
        if is_sentinel(&doc.runs[i]) {
            insert_index = i;
            break;
        }
        if doc.runs[i].tombstone || doc.runs[i].length == 0 {
            insert_index = i + 1;
            i += 1;
            continue;
        }
        let run_end = visible + doc.runs[i].length as usize;
        if start < run_end {
            let offset = start - visible;
            insert_index = if offset == 0 { i } else { split_run_at(&mut doc.runs, i, offset as u32) };
            break;
        }
        visible = run_end;
        insert_index = i + 1;
        i += 1;
    }

    let extends_previous = insert_index > 0 && {
        let previous = &doc.runs[insert_index - 1];
        !previous.tombstone
            && !is_sentinel(previous)
            && previous.coord.replica == replica_index as u32
            && previous.coord.clock + previous.length == clock
    };

    if extends_previous {
        doc.runs[insert_index - 1].length += text.len() as u32;
        set_text_clock(&mut doc.replicas[replica_index - 1], clock + text.len() as u32);
        return false;
    }

    insert_run_at(
        &mut doc.runs,
        insert_index,
        TextRun {
            coord: RunCoord { replica: replica_index as u32, clock },
            length: text.len() as u32,
            anchor: RunCoord { replica: replica_index as u32, clock: 0 },
            tombstone: false,
            sequence: Vec::new(),
        },
    );
    set_text_clock(&mut doc.replicas[replica_index - 1], clock + text.len() as u32);
    true
}

/// Re-lengths `doc.attribute_runs` after one hunk's delete/insert. Ported
/// SEPARATELY from `compose::splice_runs` because this runs ONCE PER HUNK
/// inside [`apply_text_edit`]'s loop, not once for the whole document —
/// reusing the whole-document `splice_runs` here would reintroduce the same
/// multi-edit fusion risk the CRDT text diff itself exists to avoid.
/// `start`/`delete_length`/`insert_length` are UTF-16 code units, matching
/// the hunk's own units.
pub fn adjust_attribute_runs(doc: &mut CrdtDocument, start: usize, delete_length: usize, insert_length: usize) {
    let end = start + delete_length;
    let mut out: Vec<topotext::AttributeRun> = Vec::new();
    let mut visible = 0usize;
    for run in &doc.attribute_runs {
        let run_start = visible;
        let run_end = visible + run.length as usize;
        visible = run_end;
        let overlap = end.min(run_end).saturating_sub(start.max(run_start));
        let remaining = (run.length as usize).saturating_sub(overlap);
        if remaining > 0 {
            let mut piece = run.clone();
            piece.length = remaining as u32;
            out.push(piece);
        }
    }

    if insert_length > 0 {
        let mut grown = false;
        let mut run_end = 0usize;
        for i in 0..out.len() {
            run_end += out[i].length as usize;
            if start <= run_end {
                if out[i].attachment_info.is_none() {
                    out[i].length += insert_length as u32;
                } else {
                    let mut piece = out[i].clone();
                    piece.attachment_info = None;
                    piece.length = insert_length as u32;
                    let at = if start == run_end { i + 1 } else { i };
                    out.insert(at, piece);
                }
                grown = true;
                break;
            }
        }
        if !grown {
            match out.last_mut() {
                Some(last) if last.attachment_info.is_none() => {
                    last.length += insert_length as u32;
                }
                Some(last) => {
                    let mut piece = last.clone();
                    piece.attachment_info = None;
                    piece.length = insert_length as u32;
                    out.push(piece);
                }
                None => {
                    out.push(topotext::AttributeRun { length: insert_length as u32, ..Default::default() });
                }
            }
        }
    }
    doc.attribute_runs = out;
}

/// Expands the visible range `[start, start + delete_length)` to the
/// boundaries of every visible run it overlaps, so the caller can tombstone
/// whole runs instead of splitting any (see [`apply_text_edit`]'s
/// runs-are-never-split section for the live measurement behind this).
///
/// A zero-length range (a pure insert) sitting exactly ON a run boundary
/// overlaps nothing and comes back unchanged — that is the append/between-
/// runs case, which merges clean without any widening. A zero-length range
/// strictly INSIDE a run widens to that whole run: the insert has to land
/// mid-run, and landing mid-run without a split means rewriting the run.
fn widen_to_run_boundaries(doc: &CrdtDocument, start: usize, delete_length: usize) -> (usize, usize) {
    let end = start + delete_length;
    let mut visible = 0usize;
    let mut w_start = start;
    let mut w_end = end;
    for run in &doc.runs {
        if run.tombstone || run.length == 0 || is_sentinel(run) {
            continue;
        }
        let run_start = visible;
        let run_end = run_start + run.length as usize;
        visible = run_end;
        // Overlap test: a non-empty range overlaps a run it intersects; an
        // empty range (pure insert) only "overlaps" the run it sits strictly
        // inside of.
        let overlaps = if delete_length > 0 {
            start < run_end && end > run_start
        } else {
            start > run_start && start < run_end
        };
        if overlaps {
            w_start = w_start.min(run_start);
            w_end = w_end.max(run_end);
        }
        if run_start >= end && delete_length > 0 {
            break;
        }
        if delete_length == 0 && run_start > start {
            break;
        }
    }
    (w_start, w_end - w_start)
}

// ── formatting ops ─────────────────────────────────────────────────────

/// The CRDT half of a formatting change (M3 F4): restamps the anchor of
/// every visible run overlapping any of `ranges` (UTF-16 `[start, end)`
/// pairs) to `(our replica, max(old anchor clock + 1, floor))`, advancing
/// the replica's op counter past the max assigned — Apple's
/// `applyFormattingOp`, which is what makes this replica's restyle win
/// merge-time last-write-wins against older restyles.
///
/// **Deliberately diverges from icloud-md in one way: ranges are WIDENED to
/// run boundaries instead of splitting runs.** The reference splits at the
/// range edges (`restampVisibleRange`); this engine's never-split-a-run rule
/// (see [`apply_text_edit`]'s doc) was kept after the table-order root cause
/// landed, and both confirming live passes ran on it — a split write from
/// Jodd has no post-table-fix live pass. The rendered formatting is
/// identical either way (it lives in `attribute_run`, rewritten precisely
/// by the reconciler); only the merge-priority granularity is coarser, and
/// the live phase checks that trade (spec F4).
pub fn apply_formatting_op(doc: &mut CrdtDocument, ranges: &[(usize, usize)], replica_id: [u8; 16]) {
    if ranges.is_empty() {
        return;
    }
    let replica_index = ensure_replica(doc, replica_id);
    let style_clock_floor = style_clock_seed(doc, replica_index);
    let mut max_assigned: i64 = -1;
    for &(start, end) in ranges {
        let mut visible = 0usize;
        for run in doc.runs.iter_mut() {
            if run.tombstone || run.length == 0 || is_sentinel(run) {
                continue;
            }
            let run_start = visible;
            let run_end = run_start + run.length as usize;
            visible = run_end;
            if run_end <= start {
                continue;
            }
            if run_start >= end {
                break;
            }
            // Overlaps the range: restamp the WHOLE run, never split.
            let assigned = (run.anchor.clock.saturating_add(1)).max(style_clock_floor);
            run.anchor = RunCoord { replica: replica_index as u32, clock: assigned };
            max_assigned = max_assigned.max(assigned as i64);
        }
    }
    if max_assigned >= 0 {
        let replica = &mut doc.replicas[replica_index - 1];
        if replica.counters.len() < 2 {
            replica.counters.resize(2, 0);
        }
        replica.counters[1] = ((max_assigned + 1) as u32).max(style_clock_floor);
    }
}

// ── the entry point ─────────────────────────────────────────────────────

/// Applies a plain-text edit to the document in place. Diffs old/new text
/// into per-hunk splices ([`compute_splices`]) so that untouched text
/// BETWEEN two edits keeps its original runs and authorship — this is the
/// fusion bug icloud-md's own history documents: a single whole-document
/// splice tombstoned another replica's live runs merely because a second,
/// distant difference (e.g. a trailing newline) destroyed the common
/// suffix, and a device with unmerged history then revived the
/// concurrently-edited runs alongside the re-authored copy, fusing the
/// text. Returns whether anything changed, or a [`CrdtError`] if the
/// document (before or after the edit) fails its own invariants.
///
/// # Runs are never split — every splice is WIDENED to run boundaries
///
/// This is where this engine now deliberately diverges from icloud-md's,
/// and the divergence is measured, not preferred. icloud-md splits the run
/// a splice boundary lands inside (`splitTopoSubstring_atIndex`, recovered
/// from Apple's own captured web bundle) and this engine's first version
/// did the same, byte-for-byte — and every live push that split an
/// Apple-authored run made Apple's own clients duplicate the note on merge
/// (three fresh notes, 2026-08-26, reproducible with Notes.app fully quit
/// during the write). Two shapes merged clean in the same live rig: edits
/// that only APPEND (no existing run touched), and edits that tombstone
/// the overlapped runs WHOLE and re-insert the retained text as part of
/// the editor's own new run — which is exactly what Apple's own client
/// does for a body change (ground-truth capture, same day: it tombstoned
/// its entire 28-unit run and inserted the new text as one fresh run,
/// splitting nothing). So every splice here is widened to cover the full
/// visible runs it overlaps: the widened range is tombstoned run-whole —
/// [`tombstone_visible_range`] never needs to split — and the retained
/// prefix/suffix text rides along inside the inserted run.
///
/// The ATTRIBUTE runs are still adjusted with the NARROW splice: the final
/// visible text is identical either way, and the narrow adjustment is what
/// keeps the formatting of retained-but-reauthored text where it was
/// instead of flattening it into the insertion run's style.
///
/// # Mutation semantics
///
/// This function mutates `doc` in place and is **not transactional** — if it
/// returns `Err` (either the pre-edit or post-edit invariant check failing),
/// `doc` may have already been partially or fully mutated and must not be
/// trusted as-is. A caller that needs to preserve the original document on
/// failure must operate on a clone and only adopt the mutated result after a
/// successful `Ok`.
pub fn apply_text_edit(doc: &mut CrdtDocument, new_text: &str, replica_id: [u8; 16]) -> Result<bool, CrdtError> {
    let old_text = doc.text.clone();
    if old_text == new_text {
        return Ok(false);
    }
    validate_document_invariants(doc)?;

    let splices = compute_splices(&old_text, new_text);
    let replica_index = ensure_replica(doc, replica_id);
    let style_clock_floor = style_clock_seed(doc, replica_index);
    let mut max_assigned_style_clock: Option<u32> = None;

    // The document's CURRENT visible text as the loop mutates it — the
    // widened insert needs the retained prefix/suffix characters, and
    // `doc.text` still holds the pre-edit text until the loop is done.
    let mut current_text: Vec<u16> = old_text.encode_utf16().collect();

    let mut structural_change = false;
    let mut inserted_new_run = false;
    let mut delta: i64 = 0;
    for splice in &splices {
        let start = (splice.start as i64 + delta) as usize;
        let insert_units: Vec<u16> = splice.insert_text.encode_utf16().collect();
        let insert_len_units = insert_units.len();

        // Widen to the boundaries of the visible runs the narrow splice
        // touches. A pure insert exactly ON a run boundary widens to
        // nothing — that is the append case, and it stays an extension of
        // our own trailing run.
        let (w_start, w_delete) = widen_to_run_boundaries(doc, start, splice.delete_length);
        let mut w_insert: Vec<u16> = Vec::with_capacity(w_delete + insert_len_units);
        w_insert.extend_from_slice(&current_text[w_start..start]);
        w_insert.extend_from_slice(&insert_units);
        w_insert.extend_from_slice(&current_text[start + splice.delete_length..w_start + w_delete]);

        if w_delete > 0 {
            if let Some(assigned) =
                tombstone_visible_range(doc, w_start, w_delete, replica_index, style_clock_floor)
            {
                max_assigned_style_clock = Some(max_assigned_style_clock.map_or(assigned, |m| m.max(assigned)));
            }
            structural_change = true;
        }
        if !w_insert.is_empty() {
            let new_run = insert_visible_text(doc, w_start, &w_insert, replica_index);
            inserted_new_run = inserted_new_run || new_run;
            structural_change = structural_change || new_run;
        }
        // Narrow, not widened: retained text keeps its attribute runs.
        adjust_attribute_runs(doc, start, splice.delete_length, insert_len_units);

        current_text.splice(start..start + splice.delete_length, insert_units);
        delta += insert_len_units as i64 - splice.delete_length as i64;
    }

    if structural_change {
        let floor_bump = if inserted_new_run { 1 } else { 0 };
        let new_style_clock = max_assigned_style_clock
            .map(|m| m + 1)
            .unwrap_or(0)
            .max(style_clock_floor)
            .max(floor_bump);
        if let Some(replica) = doc.replicas.get_mut(replica_index - 1) {
            if replica.counters.len() < 2 {
                replica.counters.resize(2, 0);
            }
            replica.counters[1] = new_style_clock;
        }
    }

    doc.text = new_text.to_string();
    validate_document_invariants(doc)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLICA_A: [u8; 16] = [0xAA; 16];
    const REPLICA_B: [u8; 16] = [0xBB; 16];

    fn sentinel() -> TextRun {
        TextRun {
            coord: RunCoord { replica: 0, clock: SENTINEL_CLOCK },
            length: 0,
            anchor: RunCoord { replica: 0, clock: SENTINEL_CLOCK },
            tombstone: false,
            sequence: Vec::new(),
        }
    }

    fn origin() -> TextRun {
        TextRun {
            coord: RunCoord { replica: 0, clock: 0 },
            length: 0,
            anchor: RunCoord { replica: 0, clock: 0 },
            tombstone: false,
            sequence: vec![1],
        }
    }

    /// A synthetic document in the shape observed in captured web-client
    /// saves: origin run, content runs, end sentinel, replica table, one
    /// plain attribute run — matches icloud-md's own `makeDocument` test
    /// helper.
    fn make_document(text: &str, mut runs: Vec<TextRun>, replica_clocks: &[u32]) -> CrdtDocument {
        let mut all_runs = vec![origin()];
        all_runs.append(&mut runs);
        all_runs.push(sentinel());
        let mut replicas = vec![ReplicaEntry {
            id: REPLICA_A,
            counters: vec![*replica_clocks.first().unwrap_or(&0), 1],
        }];
        for &clock in &replica_clocks[1.min(replica_clocks.len())..] {
            replicas.push(ReplicaEntry { id: REPLICA_B, counters: vec![clock, 1] });
        }
        CrdtDocument {
            text: text.to_string(),
            runs: all_runs,
            replicas,
            attribute_runs: vec![topotext::AttributeRun { length: utf16_len(text) as u32, ..Default::default() }],
        }
    }

    fn simple_document(text: &str) -> CrdtDocument {
        make_document(
            text,
            vec![TextRun {
                coord: RunCoord { replica: 1, clock: 0 },
                length: utf16_len(text) as u32,
                anchor: RunCoord { replica: 1, clock: 0 },
                tombstone: false,
                sequence: vec![2],
            }],
            &[utf16_len(text) as u32],
        )
    }

    #[test]
    fn parse_encode_round_trips_a_synthetic_document() {
        let doc = simple_document("Grocery list\nEggs\n");
        let wire = encode_crdt_document(&doc);
        let reparsed = parse_crdt_document(&wire).unwrap();
        assert_eq!(reparsed, doc);
        assert_eq!(reparsed.runs.len(), 3);
        assert_eq!(reparsed.replicas.len(), 1);
        assert_eq!(reparsed.attribute_runs.len(), 1);
        assert_eq!(reparsed.attribute_runs[0].length, 18);
    }

    #[test]
    fn a_document_missing_its_replica_clock_table_is_refused() {
        let s = topotext::String {
            string: "hi".into(),
            substring: Vec::new(),
            timestamp: None,
            attribute_run: Vec::new(),
        };
        assert_eq!(parse_crdt_document(&s), Err(CrdtError::MissingTimestampTable));
    }

    #[test]
    fn a_subclock_is_refused() {
        let mut doc = simple_document("hi");
        let wire = encode_crdt_document(&doc);
        let mut s = wire;
        s.timestamp.as_mut().unwrap().clock[0].replica_clock.push(
            topotext::vector_timestamp::clock::ReplicaClock { clock: 0, subclock: Some(1) },
        );
        assert_eq!(parse_crdt_document(&s), Err(CrdtError::SubclockPresent));
        doc.replicas[0].counters.push(0); // keep clippy quiet about unused mut
    }

    #[test]
    fn invariant_validation_rejects_run_lengths_that_disagree_with_the_text() {
        let mut doc = simple_document("hello");
        doc.text = "hello!".into();
        assert!(matches!(
            validate_document_invariants(&doc),
            Err(CrdtError::VisibleLengthMismatch { .. })
        ));
    }

    #[test]
    fn invariant_validation_rejects_clocks_past_the_replica_counter() {
        let mut doc = simple_document("hello");
        doc.replicas[0].counters[0] = 3;
        assert!(matches!(
            validate_document_invariants(&doc),
            Err(CrdtError::RunClockExceedsCounter { .. })
        ));
    }

    #[test]
    fn validate_child_edges_rejects_a_backward_edge() {
        let mut doc = simple_document("hello");
        doc.runs[1].sequence = vec![0];
        assert_eq!(
            validate_document_invariants(&doc),
            Err(CrdtError::ChildEdgeOutOfRange { index: 1, child: 0 })
        );
    }

    #[test]
    fn validate_child_edges_rejects_an_out_of_range_edge() {
        let mut doc = simple_document("hello");
        doc.runs[1].sequence = vec![9];
        assert_eq!(
            validate_document_invariants(&doc),
            Err(CrdtError::ChildEdgeOutOfRange { index: 1, child: 9 })
        );
    }

    #[test]
    fn validate_child_edges_rejects_a_dangling_non_sentinel_run() {
        let mut doc = simple_document("hello");
        doc.runs[1].sequence = Vec::new();
        assert_eq!(validate_document_invariants(&doc), Err(CrdtError::MissingChildEdge { index: 1 }));
    }

    #[test]
    fn a_replica_id_that_is_not_16_bytes_is_refused() {
        let doc = simple_document("hi");
        let mut s = encode_crdt_document(&doc);
        s.timestamp.as_mut().unwrap().clock[0].replica_uuid.pop();
        assert_eq!(parse_crdt_document(&s), Err(CrdtError::ReplicaUuidWrongLength(15)));
    }

    #[test]
    fn compute_splice_finds_minimal_edits() {
        let case = |old: &str, new: &str| {
            compute_splice(&old.encode_utf16().collect::<Vec<_>>(), &new.encode_utf16().collect::<Vec<_>>())
        };
        assert_eq!(case("abc", "abXc"), Splice { start: 2, delete_length: 0, insert_text: "X".into() });
        assert_eq!(case("abc", "ac"), Splice { start: 1, delete_length: 1, insert_text: "".into() });
        assert_eq!(case("abc", "aXc"), Splice { start: 1, delete_length: 1, insert_text: "X".into() });
        assert_eq!(case("abc", "abc def"), Splice { start: 3, delete_length: 0, insert_text: " def".into() });
        assert_eq!(case("", "new"), Splice { start: 0, delete_length: 0, insert_text: "new".into() });
    }

    #[test]
    fn compute_splice_never_splits_a_surrogate_pair() {
        let old: Vec<u16> = "😀".encode_utf16().collect();
        let new: Vec<u16> = "😁".encode_utf16().collect();
        let s = compute_splice(&old, &new);
        assert_eq!(s, Splice { start: 0, delete_length: 2, insert_text: "😁".into() });
    }

    #[test]
    fn compute_splices_is_empty_for_unchanged_text() {
        assert_eq!(compute_splices("same", "same"), Vec::new());
    }

    #[test]
    fn compute_splices_degenerates_to_one_hunk_for_one_changed_region() {
        assert_eq!(
            compute_splices("abc def", "abc XX def"),
            vec![Splice { start: 4, delete_length: 0, insert_text: "XX ".into() }],
        );
    }

    #[test]
    fn compute_splices_keeps_separated_edits_as_separate_hunks() {
        assert_eq!(
            compute_splices("one\ntwo\nthree\n", "one EDIT\ntwo\nthree, EDITED\n"),
            vec![
                Splice { start: 3, delete_length: 0, insert_text: " EDIT".into() },
                Splice { start: 13, delete_length: 0, insert_text: ", EDITED".into() },
            ],
        );
    }

    /// The 2026-07-29 fusion regression, straight from icloud-md's own test
    /// suite: a mid-document insert plus a trailing-newline difference must
    /// stay two small hunks, not one spanning splice that loses the common
    /// suffix and rewrites everything from the insert to the end.
    #[test]
    fn compute_splices_keeps_a_mid_document_insert_and_a_trailing_newline_diff_separate() {
        let remote = "p2 bravo dev-E1\n\np3 charlie\ntyped-on-device tail";
        let local = "p2 bravo EDIT-E1 dev-E1\n\np3 charlie\ntyped-on-device tail\n";
        assert_eq!(
            compute_splices(remote, local),
            vec![
                Splice { start: 9, delete_length: 0, insert_text: "EDIT-E1 ".into() },
                Splice { start: utf16_len(remote), delete_length: 0, insert_text: "\n".into() },
            ],
        );
    }

    fn run_at(replica: u32, clock: u32, length: u32, sequence: Vec<usize>) -> TextRun {
        TextRun {
            coord: RunCoord { replica, clock },
            length,
            anchor: RunCoord { replica, clock: 0 },
            tombstone: false,
            sequence,
        }
    }

    #[test]
    fn split_run_at_divides_a_run_and_shifts_downstream_edges() {
        // origin[1], A(len 10)[2], sentinel[]
        let mut runs = vec![origin(), run_at(1, 0, 10, vec![2]), sentinel()];
        let tail_index = split_run_at(&mut runs, 1, 4);
        assert_eq!(tail_index, 2);
        assert_eq!(runs.len(), 4);
        assert_eq!(runs[1], run_at(1, 0, 4, vec![2]));
        assert_eq!(runs[2].coord, RunCoord { replica: 1, clock: 4 });
        assert_eq!(runs[2].length, 6);
        assert_eq!(runs[2].sequence, vec![3]); // shifted from the original run's [2] -> [3]
        assert!(is_sentinel(&runs[3]));
    }

    #[test]
    fn insert_run_at_redirects_the_predecessors_edge() {
        // origin[1], A(len 5)[2], sentinel[] ; insert a new run at index 1
        let mut runs = vec![origin(), run_at(1, 0, 5, vec![2]), sentinel()];
        insert_run_at(&mut runs, 1, run_at(2, 0, 3, Vec::new()));
        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0].sequence, vec![1]); // origin -> new run, unchanged index
        assert_eq!(runs[1].sequence, vec![2]); // new run -> old run's new position
        assert_eq!(runs[2].sequence, vec![3]); // old run's edge, shifted to sentinel's new position
    }

    #[test]
    fn insert_run_at_takes_over_all_children_when_predecessor_isnt_linked_to_the_displaced_run() {
        // A branch node: origin[1], A[2,3], B[4], C[4], sentinel[] (index 4).
        // Insert between B (index 2) and C (index 3): B's only edge points at
        // 4 (sentinel), not at 3, so the new run takes over ALL of B's
        // children and becomes B's only child.
        let mut runs = vec![
            origin(),
            run_at(1, 0, 3, vec![2, 3]),
            run_at(2, 0, 3, vec![4]),
            run_at(1, 3, 3, vec![4]),
            sentinel(),
        ];
        insert_run_at(&mut runs, 3, run_at(1, 6, 2, Vec::new()));
        assert_eq!(runs.len(), 6);
        assert_eq!(runs[1].sequence, vec![2, 4]); // A still branches to B and (shifted) C
        assert_eq!(runs[2].sequence, vec![3]); // B -> new run
        assert_eq!(runs[3].sequence, vec![5]); // new run took over B's old edge to sentinel
        assert_eq!(runs[4].sequence, vec![5]); // C (shifted) -> sentinel, untouched
    }

    #[test]
    fn ensure_replica_returns_the_existing_index_for_a_known_replica() {
        let doc = simple_document("hi");
        let mut doc = doc;
        assert_eq!(ensure_replica(&mut doc, REPLICA_A), 1);
        assert_eq!(doc.replicas.len(), 1); // no new entry
    }

    #[test]
    fn ensure_replica_seeds_a_joining_replica_from_the_observed_maxima() {
        let mut doc = simple_document("Hello"); // replica A: counters [5, 1]
        let index = ensure_replica(&mut doc, REPLICA_B);
        // FIRST in the table (the web-client capture's order), seeded from
        // the maxima, with A renumbered to index 2 on its runs.
        assert_eq!(index, 1);
        assert_eq!(doc.replicas.len(), 2);
        assert_eq!(doc.replicas[0].id, REPLICA_B);
        assert_eq!(doc.replicas[0].counters, vec![5, 1]);
        assert_eq!(doc.replicas[1].id, REPLICA_A);
        assert_eq!(doc.runs[1].coord.replica, 2, "A's run renumbered");
    }

    #[test]
    fn ensure_replica_seeds_a_fresh_document_at_zero() {
        let mut doc = CrdtDocument {
            text: String::new(),
            runs: vec![origin(), sentinel()],
            replicas: Vec::new(),
            attribute_runs: Vec::new(),
        };
        let index = ensure_replica(&mut doc, REPLICA_A);
        assert_eq!(index, 1);
        assert_eq!(doc.replicas[0].counters, vec![0, 0]);
    }

    #[test]
    fn style_clock_seed_is_the_replicas_own_counter_when_it_already_holds_every_stamp() {
        let doc = simple_document("hi"); // one run, anchor (replica 1, clock 0), own counter[1] = 1
        assert_eq!(style_clock_seed(&doc, 1), 1);
    }

    /// The scenario the floor exists to prevent: replica B restyled its text
    /// up to style clock 80 while our own style counter sits at 5. The floor
    /// must clear B's stamp so a deletion from our stale counter doesn't
    /// lose the merge-time last-write-wins against every device that saw
    /// B's stamp.
    #[test]
    fn style_clock_seed_clears_another_replicas_higher_stamp_with_the_uuid_tie_break() {
        let mut doc = make_document(
            "Hellobrave ",
            vec![
                run_at(1, 0, 5, vec![2]),
                TextRun {
                    coord: RunCoord { replica: 2, clock: 0 },
                    length: 6,
                    anchor: RunCoord { replica: 2, clock: 80 },
                    tombstone: false,
                    sequence: vec![3],
                },
            ],
            &[5, 6],
        );
        doc.replicas[0].counters[1] = 5;
        doc.replicas[1].counters[1] = 81;
        // Our (A's) UUID is 0xAA..., B's is 0xBB... — B beats A in the
        // byte-lexicographic tie-break, so the floor includes the +1 bump.
        assert_eq!(style_clock_seed(&doc, 1), 81);
    }

    #[test]
    fn tombstone_visible_range_splits_and_stamps_the_deletion_bias() {
        // simple_document("Hello brave world"): one run, replica 1, len 18.
        let mut doc = simple_document("Hello brave world");
        // "brave " is UTF-16 offset 6..12.
        let assigned = tombstone_visible_range(&mut doc, 6, 6, 2, 1);
        assert_eq!(assigned, Some(8)); // max(old anchor clock 0 + 8, floor 1)
        let tombstones: Vec<_> = doc.runs.iter().filter(|r| r.tombstone).collect();
        assert_eq!(tombstones.len(), 1);
        assert_eq!(tombstones[0].length, 6);
        assert_eq!(tombstones[0].coord.clock, 6);
        assert_eq!(tombstones[0].anchor, RunCoord { replica: 2, clock: 8 });
    }

    #[test]
    fn insert_visible_text_extends_the_replicas_own_trailing_run() {
        let mut doc = simple_document("Hello"); // replica 1, coord clock 0, length 5
        let units: Vec<u16> = " there".encode_utf16().collect();
        let structural = insert_visible_text(&mut doc, 5, &units, 1);
        assert!(!structural);
        assert_eq!(doc.runs[1].length, 11);
        assert_eq!(doc.replicas[0].counters[0], 11);
    }

    #[test]
    fn insert_visible_text_adds_a_new_run_for_a_different_replica() {
        let mut doc = simple_document("Hello");
        let units: Vec<u16> = "!".encode_utf16().collect();
        let index = ensure_replica(&mut doc, REPLICA_B);
        assert_eq!(index, 1, "the editor is first in the table");
        let structural = insert_visible_text(&mut doc, 5, &units, index);
        assert!(structural);
        let inserted = &doc.runs[doc.runs.len() - 2];
        assert_eq!(inserted.coord, RunCoord { replica: 1, clock: 5 });
        assert_eq!(inserted.length, 1);
    }

    #[test]
    fn adjust_attribute_runs_shrinks_deleted_overlap_and_drops_emptied_runs() {
        let mut doc = simple_document("aaabbbccc");
        doc.attribute_runs = vec![
            topotext::AttributeRun { length: 3, ..Default::default() },
            topotext::AttributeRun { length: 3, ..Default::default() },
            topotext::AttributeRun { length: 3, ..Default::default() },
        ];
        adjust_attribute_runs(&mut doc, 1, 7, 0); // delete "aabbbcc" (offsets 1..8)
        let lengths: Vec<u32> = doc.attribute_runs.iter().map(|r| r.length).collect();
        assert_eq!(lengths, vec![1, 1]); // "a" kept from run 1, "c" kept from run 3; run 2 fully consumed
    }

    #[test]
    fn adjust_attribute_runs_grows_the_run_before_the_insertion_point() {
        let mut doc = simple_document("ab");
        doc.attribute_runs = vec![
            topotext::AttributeRun { length: 1, ..Default::default() },
            topotext::AttributeRun { length: 1, ..Default::default() },
        ];
        adjust_attribute_runs(&mut doc, 1, 0, 2); // insert 2 units at offset 1
        let lengths: Vec<u32> = doc.attribute_runs.iter().map(|r| r.length).collect();
        assert_eq!(lengths, vec![3, 1]);
    }

    #[test]
    fn adjust_attribute_runs_never_grows_an_attachment_info_run() {
        let mut doc = simple_document("a\u{FFFC}b");
        doc.attribute_runs = vec![
            topotext::AttributeRun { length: 1, ..Default::default() },
            topotext::AttributeRun {
                length: 1,
                attachment_info: Some(topotext::AttachmentInfo {
                    attachment_identifier: Some("A-1".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            topotext::AttributeRun { length: 1, ..Default::default() },
        ];
        // Insert right after the embed (offset 2).
        adjust_attribute_runs(&mut doc, 2, 0, 1);
        assert_eq!(doc.attribute_runs.len(), 4);
        assert_eq!(doc.attribute_runs[1].length, 1);
        assert!(doc.attribute_runs[1].attachment_info.is_some());
        assert_eq!(doc.attribute_runs[2].length, 1);
        assert!(doc.attribute_runs[2].attachment_info.is_none());
    }

    // ── apply_text_edit entry-point tests ───────────────────────────────

    fn visible_text(doc: &CrdtDocument) -> String {
        let units: Vec<u16> = doc.text.encode_utf16().collect();
        let mut position = 0usize;
        let mut out: Vec<u16> = Vec::new();
        for run in &doc.runs {
            if run.tombstone || run.coord.clock == SENTINEL_CLOCK {
                continue;
            }
            let end = (position + run.length as usize).min(units.len());
            out.extend_from_slice(&units[position..end]);
            position += run.length as usize;
        }
        String::from_utf16_lossy(&out)
    }

    fn sequences_of(doc: &CrdtDocument) -> Vec<Vec<usize>> {
        doc.runs.iter().map(|r| r.sequence.clone()).collect()
    }

    #[test]
    fn appending_with_our_own_replica_extends_the_trailing_run() {
        let mut doc = simple_document("Hello");
        let run_count_before = doc.runs.len();
        assert!(apply_text_edit(&mut doc, "Hello there", REPLICA_A).unwrap());
        assert_eq!(doc.text, "Hello there");
        assert_eq!(doc.runs.len(), run_count_before);
        assert_eq!(doc.replicas.len(), 1);
        assert_eq!(doc.replicas[0].counters[0], 11);
        assert_eq!(doc.replicas[0].counters[1], 1); // pure extension: no new edit event
        assert_eq!(doc.attribute_runs[0].length, 11);
        assert_eq!(visible_text(&doc), "Hello there");
    }

    #[test]
    fn appending_as_a_new_replica_adds_a_replica_entry_and_a_run() {
        let mut doc = simple_document("Hello");
        assert!(apply_text_edit(&mut doc, "Hello!", REPLICA_B).unwrap());
        assert_eq!(doc.replicas.len(), 2);
        assert_eq!(doc.replicas[0].id, REPLICA_B, "the editor is first in the table");
        assert_eq!(doc.replicas[0].counters, vec![6, 1]);
        assert_eq!(visible_text(&doc), "Hello!");
        validate_document_invariants(&doc).unwrap();
    }

    /// The strategy pin for the 2026-08-26 live measurement: a mid-run
    /// insert must NOT split the run it lands in — Apple's clients
    /// duplicate the whole note on merging a foreign split. The overlapped
    /// run is tombstoned WHOLE and the retained text rides along inside
    /// the editor's own inserted run, exactly the shape Apple's own client
    /// writes for a body change (ground-truth capture, same day) and the
    /// shape the same live rig merged clean.
    #[test]
    fn mid_text_insertion_rewrites_the_whole_run_instead_of_splitting() {
        let mut doc = simple_document("Hello world");
        assert!(apply_text_edit(&mut doc, "Hello brave world", REPLICA_B).unwrap());
        assert_eq!(doc.text, "Hello brave world");
        assert_eq!(visible_text(&doc), "Hello brave world");
        let live: Vec<_> = doc.runs.iter().filter(|r| r.length > 0 && !r.tombstone).collect();
        assert_eq!(live.len(), 1, "one whole replacement run, no split fragments");
        assert_eq!((live[0].coord, live[0].length), (RunCoord { replica: 1, clock: 11 }, 17));
        let tombstones: Vec<_> = doc.runs.iter().filter(|r| r.tombstone).collect();
        assert_eq!(tombstones.len(), 1, "the overlapped run is tombstoned whole");
        assert_eq!((tombstones[0].coord, tombstones[0].length), (RunCoord { replica: 2, clock: 0 }, 11));
        validate_document_invariants(&doc).unwrap();
    }

    #[test]
    fn deletion_tombstones_the_whole_run_and_reinserts_the_rest() {
        let mut doc = simple_document("Hello brave world");
        assert!(apply_text_edit(&mut doc, "Hello world", REPLICA_B).unwrap());
        assert_eq!(visible_text(&doc), "Hello world");
        let tombstones: Vec<_> = doc.runs.iter().filter(|r| r.tombstone).collect();
        assert_eq!(tombstones.len(), 1);
        assert_eq!(tombstones[0].length, 17, "the whole overlapped run, not a mid-run fragment");
        assert_eq!(tombstones[0].coord.clock, 0);
        assert_eq!(tombstones[0].anchor.replica, 1, "restamped by the deleting replica, table-first");
        let live: Vec<_> = doc.runs.iter().filter(|r| r.length > 0 && !r.tombstone).collect();
        assert_eq!(live.len(), 1);
        assert_eq!((live[0].coord, live[0].length), (RunCoord { replica: 1, clock: 17 }, 11));
        assert_eq!(doc.replicas.len(), 2);
        assert_eq!(doc.replicas[0].counters[0], 28, "17 seeded + 11 reinserted");
        validate_document_invariants(&doc).unwrap();
    }

    #[test]
    fn deletion_spanning_multiple_runs_tombstones_each_whole_run() {
        let mut doc = make_document(
            "aaabbbccc",
            vec![run_at(1, 0, 3, vec![2]), run_at(2, 0, 3, vec![3]), run_at(1, 3, 3, vec![4])],
            &[6, 3],
        );
        assert!(apply_text_edit(&mut doc, "aacc", REPLICA_A).unwrap());
        assert_eq!(visible_text(&doc), "aacc");
        let tombstoned: Vec<_> = doc.runs.iter().filter(|r| r.tombstone).collect();
        assert_eq!(tombstoned.len(), 3, "every overlapped run, tombstoned whole");
        assert_eq!(tombstoned.iter().map(|r| r.length).sum::<u32>(), 9, "all nine characters");
        let live: Vec<_> = doc.runs.iter().filter(|r| r.length > 0 && !r.tombstone).collect();
        assert_eq!(live.len(), 1, "the retained text is one run of the editor's own");
        assert_eq!(live[0].length, 4);
        assert_eq!(live[0].coord.replica, 1);
        validate_document_invariants(&doc).unwrap();
    }

    #[test]
    fn edits_never_split_a_surrogate_pair() {
        let mut doc = simple_document("ab\u{1f600}cd");
        assert!(apply_text_edit(&mut doc, "ab\u{1f601}cd", REPLICA_B).unwrap());
        assert_eq!(doc.text, "ab\u{1f601}cd");
        assert_eq!(visible_text(&doc), "ab\u{1f601}cd");
        validate_document_invariants(&doc).unwrap();
    }

    #[test]
    fn unchanged_text_is_a_no_op() {
        let mut doc = simple_document("same");
        let before = encode_crdt_document(&doc);
        assert!(!apply_text_edit(&mut doc, "same", REPLICA_B).unwrap());
        assert_eq!(encode_crdt_document(&doc), before);
    }

    /// Mirrors the 2026-08-26 icloud.com ground-truth capture (`web probe`,
    /// `0BF5E08B`): a foreign replica's mid-text edit on a single-run
    /// Mac-typed note. The web client's output put ITSELF first in the
    /// replica table, renumbered the Mac's runs to index 2, and seeded its
    /// text clock from the Mac's counter — this pins all three on our
    /// engine's output for the same edit.
    #[test]
    fn a_foreign_mid_text_edit_puts_the_editor_first_like_the_web_client_capture() {
        let mut doc = simple_document("web probe\nweb body line"); // REPLICA_A, len 23
        assert!(apply_text_edit(&mut doc, "web probe\nweb body liXXne", REPLICA_B).unwrap());
        assert_eq!(doc.replicas.len(), 2);
        assert_eq!(doc.replicas[0].id, REPLICA_B, "the editor serializes itself first");
        assert_eq!(doc.replicas[1].id, REPLICA_A);
        // Every run authored by A now references index 2.
        for run in doc.runs.iter().filter(|r| !is_sentinel(r) && r.length > 0) {
            if r_is(run, &doc, REPLICA_A) {
                assert_eq!(run.coord.replica, 2);
            }
        }
        // Our first insert seeds from the observed maximum, like the web
        // client's did (its first clock was the Mac's counter, 23).
        let ours: Vec<_> =
            doc.runs.iter().filter(|r| !r.tombstone && r.coord.replica == 1).collect();
        assert!(!ours.is_empty());
        assert_eq!(ours[0].coord.clock, 23);
        assert_eq!(visible_text(&doc), "web probe\nweb body liXXne");
        validate_document_invariants(&doc).unwrap();
    }

    fn r_is(run: &TextRun, doc: &CrdtDocument, id: [u8; 16]) -> bool {
        let idx = run.coord.replica as usize;
        idx >= 1 && doc.replicas.get(idx - 1).map(|r| r.id) == Some(id)
    }

    #[test]
    fn consecutive_pushes_from_the_same_replica_keep_extending_the_same_run() {
        let mut doc = simple_document("v1");
        apply_text_edit(&mut doc, "v1 v2", REPLICA_B).unwrap();
        let runs_after_first = doc.runs.len();
        apply_text_edit(&mut doc, "v1 v2 v3", REPLICA_B).unwrap();
        assert_eq!(doc.runs.len(), runs_after_first);
        assert_eq!(visible_text(&doc), "v1 v2 v3");
        validate_document_invariants(&doc).unwrap();
    }

    #[test]
    fn editing_a_linear_document_keeps_child_edges_a_1_to_n_chain() {
        let mut doc = simple_document("Hello world");
        apply_text_edit(&mut doc, "Hello brave world", REPLICA_B).unwrap();
        let seqs: Vec<_> = doc
            .runs
            .iter()
            .filter(|r| r.coord.clock != SENTINEL_CLOCK)
            .map(|r| r.sequence.clone())
            .collect();
        let expected: Vec<_> = (0..seqs.len()).map(|i| vec![i + 1]).collect();
        assert_eq!(seqs, expected);
    }

    /// A branched document like a real capture's two-child run: two
    /// replicas inserted concurrently after run A, so A carries two child
    /// edges and the array neighbours B/C aren't graph-linked (both point
    /// at the sentinel). Layout: 0 origin[1], 1 A"aaa"[2,3], 2 B"bbb"[4],
    /// 3 C"ccc"[4], 4 sentinel[].
    fn branched_document() -> CrdtDocument {
        make_document(
            "aaabbbccc",
            vec![run_at(1, 0, 3, vec![2, 3]), run_at(2, 0, 3, vec![4]), run_at(1, 3, 3, vec![4])],
            &[6, 3],
        )
    }

    #[test]
    fn a_deletion_inside_one_branch_preserves_the_other_branchs_edges() {
        let mut doc = branched_document();
        assert!(apply_text_edit(&mut doc, "aaabbccc", REPLICA_A).unwrap());
        assert_eq!(visible_text(&doc), "aaabbccc");
        validate_document_invariants(&doc).unwrap();
        // The edited branch's run is tombstoned WHOLE (no split) and the
        // retained "bb" is re-inserted as the editor's own run; the OTHER
        // branch (run1 -> run3) keeps its edge, index-shifted only.
        assert_eq!(sequences_of(&doc), vec![vec![1], vec![2, 4], vec![3], vec![5], vec![5], vec![]]);
        let tombstones: Vec<_> = doc.runs.iter().map(|r| r.tombstone).collect();
        assert_eq!(tombstones, vec![false, false, true, false, false, false]);
        assert_eq!(doc.runs[2].anchor, RunCoord { replica: 1, clock: 8 });
        assert_eq!((doc.runs[3].coord, doc.runs[3].length), (RunCoord { replica: 1, clock: 6 }, 2));
    }

    #[test]
    fn an_insert_between_branches_follows_the_edge_splice_rule() {
        let mut doc = branched_document();
        assert!(apply_text_edit(&mut doc, "aaabbbXXccc", REPLICA_A).unwrap());
        assert_eq!(visible_text(&doc), "aaabbbXXccc");
        validate_document_invariants(&doc).unwrap();
        assert_eq!(sequences_of(&doc), vec![vec![1], vec![2, 4], vec![3], vec![5], vec![5], vec![]]);
        let inserted = &doc.runs[3];
        assert_eq!(inserted.coord, RunCoord { replica: 1, clock: 6 });
        assert_eq!(inserted.length, 2);
    }

    #[test]
    fn structural_edits_advance_the_style_clock_and_the_sentinel_never_gets_a_sequence() {
        let mut doc = simple_document("Hello brave world");
        assert_eq!(doc.replicas[0].counters[1], 1);
        apply_text_edit(&mut doc, "Hello world", REPLICA_A).unwrap();
        assert_eq!(doc.replicas[0].counters[1], 9);
        assert_eq!(doc.runs.last().unwrap().sequence, Vec::<usize>::new());
    }

    /// 2026-07-29 fusion regression: a multi-hunk edit must never re-author
    /// another replica's text between the hunks.
    #[test]
    fn a_multi_hunk_edit_never_reauthors_another_replicas_text_between_hunks() {
        let mut doc = make_document(
            "alpha\n\nmid\nbravo-device",
            vec![run_at(1, 0, 11, vec![2]), run_at(2, 0, 12, vec![3])],
            &[11, 12],
        );
        assert!(apply_text_edit(&mut doc, "alpha EDIT\n\nmid\nbravo-device\n", REPLICA_A).unwrap());
        // The other replica's run sits between the two hunks and is neither
        // widened over nor tombstoned nor re-authored — hunk 1 widens only
        // to ITS run's boundaries, and hunk 2 is a boundary append.
        let foreign: Vec<_> = doc.runs.iter().filter(|r| r.coord.replica == 2).collect();
        assert_eq!(foreign.len(), 1);
        assert_eq!(foreign[0].coord, RunCoord { replica: 2, clock: 0 });
        assert_eq!(foreign[0].length, 12);
        assert!(!foreign[0].tombstone);
        // Hunk 1 lands inside replica 1's own run, which is rewritten whole
        // (tombstone + reinsert with the edit embedded) — the ONLY tombstone.
        let tombstones: Vec<_> = doc.runs.iter().filter(|r| r.tombstone).collect();
        assert_eq!(tombstones.len(), 1);
        assert_eq!((tombstones[0].coord, tombstones[0].length), (RunCoord { replica: 1, clock: 0 }, 11));
        assert_eq!(visible_text(&doc), "alpha EDIT\n\nmid\nbravo-device\n");
        validate_document_invariants(&doc).unwrap();
    }

    #[test]
    fn multi_hunk_deletions_share_one_formatting_op_stamp() {
        let mut doc = simple_document("aa bb\nmid\ncc dd\n");
        assert!(apply_text_edit(&mut doc, "aa\nmid\ncc\n", REPLICA_A).unwrap());
        let tombstones: Vec<_> = doc.runs.iter().filter(|r| r.tombstone).collect();
        assert_eq!(tombstones.len(), 2);
        for run in &tombstones {
            assert_eq!(run.anchor, RunCoord { replica: 1, clock: 8 });
        }
        assert_eq!(doc.replicas[0].counters[1], 9);
        assert_eq!(visible_text(&doc), "aa\nmid\ncc\n");
    }

    #[test]
    fn deleting_text_another_replica_restyled_stamps_past_its_higher_clock() {
        let mut doc = make_document(
            "Hellobrave ",
            vec![
                run_at(1, 0, 5, vec![2]),
                TextRun { coord: RunCoord { replica: 2, clock: 0 }, length: 6, anchor: RunCoord { replica: 2, clock: 80 }, tombstone: false, sequence: vec![3] },
            ],
            &[5, 6],
        );
        doc.replicas[0].counters[1] = 5;
        doc.replicas[1].counters[1] = 81;
        assert!(apply_text_edit(&mut doc, "Hello", REPLICA_A).unwrap());
        let tombstones: Vec<_> = doc.runs.iter().filter(|r| r.tombstone).collect();
        assert_eq!(tombstones.len(), 1);
        assert_eq!(tombstones[0].anchor, RunCoord { replica: 1, clock: 88 }); // max(80+8, 81)
        assert_eq!(doc.replicas[0].counters[1], 89);
        assert_eq!(visible_text(&doc), "Hello");
        validate_document_invariants(&doc).unwrap();
    }

    // ── attachment-info insertion guard ─────────────────────────────────

    fn document_with_embed() -> CrdtDocument {
        let mut doc = simple_document("a\u{FFFC}b");
        doc.attribute_runs = vec![
            topotext::AttributeRun { length: 1, ..Default::default() },
            topotext::AttributeRun {
                length: 1,
                paragraph_style: Some(topotext::ParagraphStyle { style: Some(3), ..Default::default() }),
                attachment_info: Some(topotext::AttachmentInfo {
                    attachment_identifier: Some("A-1".into()),
                    type_uti: Some("public.jpeg".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            topotext::AttributeRun { length: 1, ..Default::default() },
        ];
        doc
    }

    #[test]
    fn inserting_right_after_an_embed_never_grows_its_attachment_info_run() {
        let mut doc = document_with_embed();
        assert!(apply_text_edit(&mut doc, "a\u{FFFC}Xb", REPLICA_A).unwrap());
        validate_document_invariants(&doc).unwrap();
        assert_eq!(doc.attribute_runs.len(), 4);
        assert_eq!(doc.attribute_runs[1].length, 1);
        assert!(doc.attribute_runs[1].attachment_info.is_some());
        assert_eq!(doc.attribute_runs[2].length, 1);
        assert!(doc.attribute_runs[2].attachment_info.is_none());
        assert_eq!(doc.attribute_runs[2].paragraph_style.as_ref().unwrap().style, Some(3));
    }

    // ── apply_formatting_op (M3 F4) ─────────────────────────────────────

    #[test]
    fn a_formatting_op_restamps_overlapped_runs_whole_and_never_splits() {
        let mut doc = simple_document("0123456789");
        let runs_before = doc.runs.len();
        apply_formatting_op(&mut doc, &[(2, 5)], REPLICA_B);
        assert_eq!(doc.runs.len(), runs_before, "restamp must not split");
        let content = doc.runs.iter().find(|r| !is_sentinel(r) && r.length > 0).unwrap();
        assert_eq!(content.anchor.replica, 1, "restamped by the editor replica (index 1 after reorder)");
        validate_document_invariants(&doc).unwrap();
    }

    #[test]
    fn restamp_clocks_never_regress_and_the_op_counter_advances() {
        let mut doc = simple_document("abcdef");
        let old_anchor_clock = doc.runs[1].anchor.clock;
        apply_formatting_op(&mut doc, &[(0, 6)], REPLICA_B);
        let new_anchor = doc.runs.iter().find(|r| !is_sentinel(r) && r.length > 0).unwrap().anchor;
        assert!(new_anchor.clock > old_anchor_clock);
        // counters[1] (the op/style clock) moved past the assigned stamp —
        // the editor sits FIRST in the table after ensure_replica.
        let editor = &doc.replicas[0];
        assert_eq!(editor.id, REPLICA_B);
        assert!(editor.counters[1] > new_anchor.clock - 1);
    }

    #[test]
    fn tombstoned_runs_and_untouched_runs_keep_their_anchors() {
        // The editing replica is the document's own, so no table reorder
        // muddies the comparison; restamp only the second run's range.
        let mut doc = make_document(
            "aaabbb",
            vec![run_at(1, 0, 3, vec![2]), run_at(1, 3, 3, vec![3])],
            &[6],
        );
        let untouched = doc.runs[1].anchor;
        apply_formatting_op(&mut doc, &[(3, 6)], REPLICA_A);
        assert_eq!(doc.runs[1].anchor, untouched, "run outside every range keeps its anchor");
        assert_ne!(doc.runs[2].anchor, untouched);
        assert_eq!(doc.runs[2].anchor.replica, 1);
        validate_document_invariants(&doc).unwrap();
    }

    #[test]
    fn an_edit_then_a_formatting_op_yields_a_document_that_still_validates() {
        let mut doc = simple_document("Title\nbody line");
        apply_text_edit(&mut doc, "Title\nbody line!", REPLICA_B).unwrap();
        apply_formatting_op(&mut doc, &[(6, 16)], REPLICA_B);
        validate_document_invariants(&doc).unwrap();
        assert_eq!(doc.text, "Title\nbody line!");
    }
}
