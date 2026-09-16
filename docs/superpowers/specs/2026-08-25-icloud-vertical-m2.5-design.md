# iCloud vertical M2.5 — CRDT text writes

## Problem

M2's write-readiness census (2026-08-24, 776 notes, `kaiwan@me.com`) measured
`WRITABLE: 0/776`. Two independent refusals account for all of them:

```
refused — CarriesCrdtIdentity: 681 (87.8%)
refused — DoesNotRoundTrip:     95 (12.2%)
```

This spec covers **only `CarriesCrdtIdentity`** — the real blocker, and what
M2.5 is. `DoesNotRoundTrip` is a separate, smaller, unrelated problem (all 95
mismatches are in the inner `topotext.String`'s encoding, not the CRDT
question) and stays out of scope here, per the decomposition below.

`topotext.String.substring` carries Apple's per-character CRDT identity —
`Substring` records forming a DAG (via `child` edges), each anchored to a
`(replica, clock)` coordinate. Inserting or deleting text means minting new
identities **as a replica**, in an order that merges correctly against every
other replica that has ever touched the document. Jodd is not a replica: it
has no replica id, and an invented one is not ordered against what's already
on the document. `backend/icloud/compose.rs`'s `writability()` refuses this
correctly today — the gate is right, the capability just doesn't exist yet.
Building it is this spec.

## Decomposition

M2.5 as scoped by the handoff bundles three things. Two are tightly coupled;
one is independent:

1. **Replica identity** — mint + durably persist Jodd's own CRDT replica
   UUID. Meaningless without (2).
2. **The edit engine** — port the topological edge-surgery Apple's own
   client does (insert, delete/tombstone, multi-hunk diff) to Rust. Needs
   (1) to have an identity to write under.
3. **`DoesNotRoundTrip` root cause** — unrelated prost/protobuf-encoding
   investigation, unblocks a different 12.2% of notes, needs no replica
   identity at all.

**This spec covers (1) + (2) only.** (3) is real work and stays named as a
follow-up, not designed here — bundling it would dilute both.

## Prior art

A working, MIT-licensed, live-verified reference for exactly this problem
exists: `icloud-md` (github.com/coddingtonbear/icloud-md,
`src/notes/noteDocument.ts`), the same provenance as this repo's own vendored
`proto/*.proto` files. Read in full for this spec (not just its doc comments
via the handoff) — two facts from the source that the summary understated:

- **The diff is multi-hunk**, not a single prefix/suffix splice. icloud-md's
  own header comments describe a real bug they shipped and fixed: a
  single-splice diff tombstoned another replica's *untouched* runs between
  two separate edits, because a distant second difference (e.g. a trailing
  newline) killed the common suffix, and a device with unmerged history then
  revived the concurrently-edited runs alongside the re-authored copy,
  fusing the text. Jodd's own `compose::splice_runs` (used for attribute
  runs) is single-hunk — it must **not** be reused for the CRDT text diff.
  The multi-hunk diff (line-level LCS, then per-hunk character-tightening)
  has to be ported too.
- **A pure text edit still touches the "style clock"** — `Substring.timestamp`
  (this spec calls it the *anchor*, following icloud-md's naming) is the same
  field Apple's client restamps for formatting ops, and a text deletion
  restamps it too, with a `+8` clock bias so the deletion wins later merges
  against up to 8 steps of concurrent restyling. This is not formatting
  scope creeping in — it's part of what makes an insert/delete a valid CRDT
  operation at all. What stays out of scope (per the scope decision below) is
  the separate `applyFormattingOp` entry point Apple's client also has —
  restamping ranges in response to a user picking bold/italic. Nothing here
  adds that entry point or derives new attribute runs from HTML.

## Scope decisions (confirmed)

- **This spec covers CRDT replica writes only.** `DoesNotRoundTrip` is not
  designed here.
- **Replica UUID storage: `accounts.json`.** A new
  `Account.icloud_replica_id: Option<String>` field, given the same
  treatment as the existing `sync_cursor` field — durable, per-account,
  `#[serde(default)]`, and not a secret (analogous to a device id, not a
  credential — nothing about a replica UUID is sensitive, unlike the OAuth
  refresh tokens gotcha #19 already keeps out of this file).
- **Text edits only, no formatting-op writes.** The anchor/style-clock
  bookkeeping inside insert/delete ships (it's required — see above), but
  there is no `applyFormattingOp` port and no attempt to derive new
  attribute runs from HTML. Formatting applied in Jodd still does not reach
  iCloud — unchanged from M2 (gotcha #24).
- **Live verification: a read-only dry run, driven from this session.**
  `kaiwan@me.com`'s iCloud session is already established on this machine
  (`icloud_session_established: true` in the real `accounts.json`). Once the
  port and its unit tests are solid, run a census-style dry pass — parse
  every real note's CRDT model, re-encode, diff against the captured bytes —
  through the running dev app. No writes. This is the same discipline M2's
  own census used, one step earlier: prove the *model* round-trips against
  real documents before any write is attempted against them.

## Architecture

### Module: `backend/icloud/crdt.rs`

New file, separate from `compose.rs`. Mirrors the existing read/write split
(`doc.rs` = read, `compose.rs` = write): `compose.rs` stays the
orchestration/gate layer (`writability`, the six refusals, the title/HTML
round-trip checks); `crdt.rs` owns the CRDT-specific model and edit
mechanics, analogous to how `doc.rs` owns decode and `compose.rs` owns the
opaque-preservation splice today.

### Data model

A thin domain wrapper, mirroring icloud-md's own `TextRun`/`ReplicaEntry`
split from the wire types (`parseTextRun`/`encodeTextRun` there), rather than
editing `topotext::Substring`/`VectorTimestamp` directly:

```rust
struct RunCoord { replica: u32, clock: u32 }

struct TextRun {
    coord: RunCoord,       // Substring.charID
    length: u32,
    anchor: RunCoord,      // Substring.timestamp — the style clock, not a position
    tombstone: bool,
    sequence: Vec<usize>,  // Substring.child — outgoing DAG edges, 0-based indexes
}

struct ReplicaEntry {
    id: [u8; 16],          // VectorTimestamp.Clock.replicaUUID
    counters: Vec<u32>,    // [0] = text clock, [1] = style clock; rest preserved verbatim
}
```

Conversion to/from the prost-generated `topotext::Substring` /
`topotext::VectorTimestamp` happens at the model's edges only — matching how
`compose::NoteDocument` already wraps `topotext::String` for the opaque path.

Two refusals `parse_crdt_document` inherits directly from icloud-md, because
the model does not carry them through an edit and a document using them must
be refused rather than silently mis-encoded:

- **A `subclock` on any replica clock entry.** Never observed live; icloud-md
  refuses it outright (`parseReplicaEntry`) rather than guess.
- **A missing `timestamp` (replica clock table) on the `topotext.String`.**
  `writability()`'s existing `CarriesCrdtIdentity` check already treats
  `timestamp.is_some()` as evidence of CRDT identity; a document with
  `substring` populated but no `timestamp` is a shape neither this port nor
  icloud-md's has ever seen and must stay refused.

### Core operations (ported from `noteDocument.ts`)

- `parse_crdt_document(topotext::String) -> Result<CrdtDocument, CrdtError>`
  / `encode_crdt_document(&CrdtDocument) -> topotext::String` — model
  round-trip, mirroring `parseNoteDocument`/`encodeNoteDocument`.
- `compute_splices(old_text, new_text) -> Vec<Splice>` — multi-hunk diff:
  line-level LCS locates changed regions, then a per-hunk prefix/suffix pass
  (reusing `compose::splice_runs`'s surrogate-pair-safe character diff logic
  as the per-hunk primitive, **not** as the whole-document diff) tightens
  each hunk to character precision.
- `split_run_at(runs, index, offset) -> usize` / `insert_run_at(runs, index,
  run)` — the edge surgery: `split_run_at` divides one run into head/tail,
  shifting every downstream `sequence` index and handing the tail the head's
  outgoing edges; `insert_run_at` splices a new run into both the array and
  the graph, taking over the predecessor's edge to its old successor.
- `tombstone_visible_range` / `insert_visible_text` — the two halves of
  `apply_text_edit`, each splitting runs at range boundaries via
  `split_run_at`, each restamping/assigning the anchor coordinate (deletion:
  `max(old_anchor.clock + 8, style_clock_floor)`; insertion: extend the
  replica's own trailing run when contiguous, else a fresh run anchored at
  `(replica, 0)`).
- `ensure_replica(doc, replica_id) -> usize` — 1-based table index, seeding a
  newly-joining replica's counters from the maxima already in the table (so
  its ops win later-clock LWW against everything already present, and a
  fresh document seeds at zero).
- `style_clock_seed(doc, replica_index) -> u32` — the floor for one editing
  pass: the highest anchor clock in the document, tie-broken by
  byte-lexicographic replica UUID comparison (`TTIDComparator`'s ordering),
  floored at the replica's own counter so it never regresses.
- `apply_text_edit(doc, new_text, replica_id) -> bool` — the entry point:
  diffs, applies each hunk (tombstone then insert, tracking `delta` to shift
  subsequent hunk offsets), returns whether anything changed.
- `adjust_attribute_runs(doc, start, delete_length, insert_length)` — ported
  from icloud-md's `adjustAttributeRuns`, called **once per hunk** inside
  `apply_text_edit`'s loop, exactly where the hunk's own tombstone/insert
  happen. **This is not `compose::splice_runs`, and cannot reuse it**:
  `splice_runs` is a single whole-document prefix/suffix diff, while a CRDT
  edit may carry several independent hunks (the same multi-hunk diff
  `compute_splices` exists for), and running a whole-document splice per
  hunk would re-diff and mis-attribute text between hunks that
  `compute_splices` already correctly left alone. Shrinks runs the deletion
  overlaps (dropping any it fully consumes), then grows the run just before
  the insertion point to inherit its formatting — except a run carrying
  `attachment_info`, which must keep covering exactly its one `U+FFFC`
  placeholder and never grows; the inserted text gets its own plain run
  instead.
- `validate_document_invariants` / `validate_child_edges` — pre/post
  condition checks: visible-run lengths match text length, every run's
  clocks stay within its replica's counter, every non-sentinel run has at
  least one forward-pointing child edge in range.

Offsets throughout are UTF-16 code units (`compose::utf16_len`'s existing
unit, confirmed by M2's census as the real wire unit), and no boundary may
split a surrogate pair — the same rule `compose::splice_runs` already
enforces, reused rather than re-derived.

### Replica identity

`backend::icloud::mint_replica_id_for(account) -> [u8; 16]`, following
`mint_uuid_for`/`canonical_uuid_for`'s pattern (gotcha #18): a function
stating the policy explicitly, not a wildcard default. Minted **lazily**, on
the first attempt to push a CRDT-writable edit for that account — not at
sign-in, since M1/M2 accounts never needed one and minting eagerly for every
existing read-only account would be work with no consumer. Once minted,
persisted to `Account.icloud_replica_id` immediately, before the edit that
needed it is attempted, and never regenerated: a second UUID for the same
account would make every prior edit look like an unordered, unrelated
replica to Apple's merge.

### `compose.rs` integration

`writability()`'s `CarriesCrdtIdentity` branch today:

```rust
if !d.string.substring.is_empty() || d.string.timestamp.is_some() {
    return Err(Unwritable::CarriesCrdtIdentity { substrings: d.string.substring.len() });
}
```

New behavior: attempt `crdt::parse_crdt_document(&d.string)` first.

- **Parse/validate fails** (a `subclock`, a missing `timestamp` despite a
  populated `substring`, or any invariant `validate_document_invariants`
  catches) → refuse, keeping `Unwritable::CarriesCrdtIdentity` as the
  reason. The note stays exactly as unwritable as it is today; nothing
  regresses.
- **Parse/validate succeeds** → the note is CRDT-writable. `with_text` (or a
  new sibling that only `writability` reaches once this path exists) calls
  `crdt::apply_text_edit` instead of the opaque `splice_runs` path, using
  the account's replica id (minting it first if absent).

The other five refusals in `writability()` — `Locked`, `DoesNotRoundTrip`,
`RunsDoNotCoverText`, `InlineObjects`, `LayersDoNotRoundTrip` — are
untouched. `RunsDoNotCoverText` in particular stays checked **before** any
CRDT work: the attribute-run splice (`compose::splice_runs`) still owns
formatting preservation exactly as it does today, unchanged by this spec.

### Round-trip gates

Two, layered, matching the discipline `writability()` already uses for the
opaque path:

1. **`round_trips()` / `inner_round_trips()`, unchanged.** These already
   compare the whole `topotext.String` byte-for-byte and gate first — a
   document that fails this is `DoesNotRoundTrip`, never reaching the CRDT
   question at all.
2. **`validate_document_invariants` / `validate_child_edges`, new.** Once a
   document passes (1), the CRDT model's own parse must additionally satisfy
   its invariants before being trusted for an edit — mirroring icloud-md's
   own two-gate discipline (`noteDocumentRoundTrips` then
   `validateDocumentInvariants`).

No new field is added to `Unwritable` for a CRDT-model validation failure —
it collapses into the existing `CarriesCrdtIdentity` variant, since from the
user's side both mean the same thing: "this note carries per-character sync
data Jodd cannot extend yet."

## Testing strategy

- **Unit tests, ported from `noteDocument.test.ts`** where the fixtures
  translate cleanly: single insert, single delete, insert-then-delete in one
  pass, multi-hunk edits (the fusion-bug regression case specifically —
  two edited paragraphs with untouched text between them must not lose the
  untouched runs' authorship), edge-splitting at both a run's own boundary
  and mid-run, the anchor `+8` deletion bias, `ensure_replica` joining an
  existing document vs. seeding a fresh one, the UUID tie-break in
  `style_clock_seed`, and both invariant validators independently.
- **A live, read-only dry run** (this session, once the above is solid):
  parse + re-encode the CRDT model of every real note on `kaiwan@me.com`
  (776+), diff against the captured bytes, same shape as `icloud_census`'s
  existing `DecodeTally`. This is the FIRST live signal, cheaper and safer
  than any write — it answers "does the ported model actually match Apple's
  real documents" before a single byte is sent.
- **Explicitly out of scope for this spec**: any live *write* against the
  real account. That is the next milestone's gate, once the dry run above
  has passed.

## What this does not change

- `Unwritable::DoesNotRoundTrip`, `RunsDoNotCoverText`, `InlineObjects`,
  `LayersDoNotRoundTrip`, `Locked` — all five other refusals, untouched.
- `compose::splice_runs` — unchanged, and still the only attribute-run
  adjustment the **opaque** (non-CRDT) path uses. The CRDT path gets its own
  per-hunk `crdt::adjust_attribute_runs` instead (see above) — a new
  function, not a reuse of `splice_runs` under a different name.
- `Capabilities::for_backend(ICloud).writes.notes` — stays `false` until
  this spec's implementation has cleared both its unit tests and the live
  dry run. Turning it on is a separate, later decision, not implied by this
  spec landing.
- Hashtags / inline objects (`U+FFFC`, Component K) — still refused via
  `InlineObjects`, unrelated to this spec.
