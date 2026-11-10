# iCloud vertical M3 — formatting round-trip + inline hashtags (Component K)

**Date:** 2026-08-26 · **Status:** approved for implementation (pipeline
pre-approved in the milestone brief; every decision below is overridable)
**Prereqs:** M2.5 + the 2026-08-26 table-order fix — `writes.notes` is `true`
and text-only editing is live-verified end to end. This spec builds on that
write path; nothing here reopens it.

## The goal, in one sentence each

1. Formatting applied in Jodd's editor — bold/italic/underline/strike,
   headings, bullet/numbered lists, checklists, blockquotes, monospaced
   blocks, links — reaches Apple Notes on iCloud accounts, and formatting
   already on a note renders in Jodd instead of flattening to plain text.
2. Inline `#hashtags` — which on this backend are inline OBJECTS
   (`U+FFFC` + `attributeRun.attachmentInfo` + a separate `InlineAttachment`
   record), invisible to `AppleHtmlDeriver` since M1 — display, index into
   `note_tags`, and survive edits (Component K, option 1 from the M1 spec,
   now that attribute runs are decoded).

## Provenance rule (non-negotiable, from the 2026-08-26 investigation)

**icloud-md@0.6.2 is the wire-shape reference.** Its `dist/notes/noteFormat.js`
decodes attribute-run semantics with every wire value confirmed on a captured
formatting-evolution session (its own header: style 0=Title 1=Heading
2=Subheading 3=Body 4=Monospaced 100=bulletList 101=dashList 102=numberedList
103=todoList; `fontHints` bit 1=bold bit 2=italic; `underline`/`strikethrough`
flag fields; `link` covers exactly the linked range; absent `paragraphStyle`
means Body; `indent` is list nesting; todo carries `{uuid, done}`).
`dist/notes/formatReconcile.js` is the write side, live-verified against
Apple's own rendering. We port their discipline function for function and do
not guess where they measured. Where we deliberately diverge (F4), the
divergence is stated with its reason and carries a live-phase check.

## Approaches considered

- **A (chosen): port icloud-md's clone-overlay reconciler.** Decode the
  current formatting from attribute runs into a paragraph/span projection,
  diff against the projection of what Jodd's editor HTML expresses, rewrite
  ONLY the runs of paragraphs whose projection changed — each rewritten piece
  cloned from the run under it so every field this code does not render
  (colors, emphasis, fonts on unchanged spans, paragraph uuids, unknown
  protobuf fields) rides along untouched. This is gotcha #24's
  opaque-preservation doctrine extended to formatting, and it is the only
  approach with a proven-against-Apple precedent.
- **B (rejected): derive fresh runs from the editor HTML every save.**
  Simplest to write and destroys every value the editor does not express —
  exactly the failure `compose.rs`'s header exists to prevent.
- **C (rejected): read-only formatting.** Render but keep dropping on write.
  Halves the milestone; the write side is the point.

## The projection (what "formatting" means here)

A **paragraph** = one `\n`-terminated line of the document text, carrying:
`kind` (title/heading/subheading/body/monospaced/bulletList/dashList/
numberedList/todoList), `indent`, `blockQuoteLevel`, `done` (todoList only),
`startNumber` (numberedList only), and inline **spans** of
`{bold, italic, strikethrough, underline, link}`. Paragraph attributes come
from the run covering the line's trailing newline (Apple anchors paragraph
state there — confirmed on icloud-md's captures), falling back to the last
character for the final unterminated line.

Comparisons happen on the **normalized** projection, ported verbatim from
`noteFormat.js`: dash lists ≡ bullet lists; a link equal to its own covered
text ≡ no link (Apple auto-links bare URLs; we must never "remove" one);
monospaced paragraphs drop inline styling; bold/italic/strike retreat off
whitespace at styled-interval edges; trailing horizontal whitespace is not
part of the projection on non-monospaced paragraphs. Dimensions the
projection does not carry (colors, alignment, fonts, non-list indents…) never
participate in equality — so they are never "changed" and never rewritten.

## Components

### F1 — `backend/icloud/format.rs`: decode + projection

Port of `noteFormat.js`: `decode_note_format(text, &[AttributeRun]) ->
Result<Vec<Paragraph>, FormatUnsupported>` plus `normalize_spans`,
`paragraph_projections_equal`, `formats_round_trip_equal`. Refusals ported
too: a run table that overshoots the text, and a `paragraphStyle.style`
outside the known map, are `FormatUnsupported` — reported, never guessed at.
An under-covering run table is tolerated (uncovered text is plain Body), same
policy as icloud-md. All offsets are **UTF-16 code units** — the unit
`AttributeRun.length` is measured to be in (776/776), and `text.split('\n')`
indices must be computed in that unit, not `char`s (a Thai note is the test
that catches this).

### F2 — `backend/icloud/format_html.rs`: projection ↔ editor HTML

Two functions, exact inverses over everything either side can produce:
`render_paragraphs(&[Paragraph], &HashtagTexts) -> String` and
`parse_editor_html(&str) -> Vec<Paragraph>` (the parse also yields the plain
text, replacing `html_to_text` on this backend's save path so text and model
can never disagree). The mapping, matching what `NoteEditor.svelte`'s
execCommand toolbar actually produces:

| Editor HTML | Apple |
|---|---|
| `<h1>` / `<h2>` / `<h3>` line | `style` 0 / 1 / 2 (Title / Heading / Subheading) |
| plain `<div>` line | Body (style 3 or absent) |
| `<pre>` block | style 4 (Monospaced), one paragraph per inner line |
| `<ul><li>` | style 100 (style 101 dash also renders as `<ul>` — projection folds them) |
| `<ol><li>` | style 102 + `startingListItemNumber` when a group starts past 1 |
| `<div><input type="checkbox">…</div>` | style 103 + `todo{uuid, done}` (`checked` ↔ `done`) |
| list nesting depth / `margin-left: N*28px` on non-list lines | `indent` |
| `<blockquote>` | `blockQuoteLevel` (editor expresses level 1; deeper levels are preserved, not editable) |
| `<b>`/`<strong>`, `<i>`/`<em>` | `fontHints` bits 1/2 (+ the explicit `Font` name the captured web client writes: `SFUIText-Bold` / `-LightItalic` / `-BoldItalic`) |
| `<u>` | `underline = 1` |
| `<strike>`/`<s>`/`<del>` | `strikethrough = 1` |
| `<a href>` | `link` (bare-URL links render as plain text per the projection) |
| `<span data-jodd-inline="hashtag" data-ref="{recordName}">#tag</span>` | `U+FFFC` + `attachmentInfo` (F5) |

Inline `<code>` has no Apple equivalent (Monospaced is a paragraph style) and
is dropped on this backend, stated as a known limit. Unknown markup parses as
plain text, exactly as `html_to_text` does today.

Read-path integration: `doc::note_body_html` becomes format-aware — decode
the format, cut the title paragraph by the existing `title_span` position
(gotcha #21's rule is untouched; runs covering the removed span are sliced by
offset), render the rest. When `decode_note_format` refuses, fall back to
today's plain rendering — a note never renders worse than M2.

### F3 — `backend/icloud/format_reconcile.rs`: the write side

Port of `formatReconcile.js`: `reconcile_note_format(&mut NoteDocument,
&[Paragraph], replica_id) -> Result<bool, FormatRefusal>`. Runs AFTER the
text edit (`with_text_crdt`) has brought `doc.text` to the desired state;
refuses (never guesses) if the decoded paragraphs don't line up with the
desired ones text-for-text. Clone-overlay exactly as the reference: split
boundaries at changed-paragraph edges plus both sides' span boundaries,
pieces cloned from the underlying run, only projection-differing fields
overlaid; untouched runs pass through **by identity**; adjacent rewritten
pieces whose fields encode identically merge back (never an
`attachment_info` run, never an untouched original). The todo-uuid dedup
scan is ported (two checklist rows must never share an identity — Apple
merges check-state per uuid; a row inheriting its neighbor's run via
`adjust_attribute_runs` inherits the uuid too, and every later duplicate
re-mints). icloud-md's explicit-zero `startingListItemNumber` repair pass is
its own bug's cleanup and is NOT ported; the omit-when-default rule it
enforces IS (write the field only for a group genuinely starting past 1).

**Failure is downgrade, not blockage.** When reconcile refuses
(undecodable current format, paragraph mismatch), the save proceeds
**text-only** — exactly M2's L3 trade, now the exception instead of the
rule: blocking a whole text edit over a formatting keystroke is the worse
failure. The downgrade is logged with the refusal reason.

**The stale-cache guard (added during implementation, forced by M2's own
transport test).** An editor body that is ENTIRELY plain over a body that
currently carries formatting skips the reconcile and preserves the remote's
runs — M2's exact behavior. A fully-plain body over a formatted note is far
more likely a pre-M3 cached rendering echoing back (the cache's `body_html`
regenerates on the next zone walk, but a save can race the first
post-upgrade walk) than a deliberate strip-everything, and reconciling it
would delete the server's formatting silently — gotcha #17's landmine
class. The benign cost: a user who truly removes ALL formatting in Jodd
sees it return on the next pull. Partial formatting reconciles fully,
removals included.

### F4 — `crdt.rs`: `apply_formatting_op` — the style-clock restamp

Port of `noteDocument.js`'s `applyFormattingOp`/`restampVisibleRange`: every
rewritten paragraph range gets its substring runs' anchor restamped to
`(our replica, max(old anchor clock + 1, style_clock_seed))`, and the
replica's op counter advances past the max assigned — this is what makes
Jodd's formatting op win merge-time LWW against older restyles, the same
machinery `tombstone_visible_range`'s +8 deletion bias already uses.

**One deliberate divergence: the restamp WIDENS to run boundaries instead of
splitting runs.** icloud-md splits at the range edges; this engine's
never-split-a-run rule (Addendum 3) was kept after the table-order root cause
landed, and both confirming live passes ran on it — a split write from Jodd
has no post-table-fix live pass. Widening restamps whole overlapped runs:
the *rendered* formatting is identical (it lives in `attribute_run`, which
F3 rewrites precisely); only the merge-priority granularity is coarser — a
concurrent restyle of an adjacent paragraph sharing a run could lose LWW it
would otherwise win. That is a rarer, milder failure than re-testing splits
live, and the live phase checks it (concurrent-format test, below). If the
live phase ever proves Jodd splits merge clean, this narrows to the
reference's exact behavior.

### F5 — hashtags (Component K closes, read + preserve)

- **The walk collects `InlineAttachment` records** — already requested
  (`DESIRED_RECORD_TYPES` and `AltTextEncrypted`/`UTIEncrypted`/
  `TokenContentIdentifierEncrypted` have been in `DESIRED_KEYS` since the
  envelope conformance) and until now discarded. `Scan` grows a map
  `recordName -> InlineRef { type_uti, alt_text }`; `attachmentInfo.
  attachmentIdentifier` IS the record name (icloud-md `push.js` looks
  records up by it directly).
- **Render:** a `U+FFFC` whose run's `attachmentInfo` resolves to an
  inline-text-attachment record renders as
  `<span data-jodd-inline="hashtag" data-ref="{recordName}">{alt_text}</span>`,
  contenteditable-atomic. `AppleHtmlDeriver` sees the `#word` text and
  `note_tags` fills for the first time on this backend. `parse_editor_html`
  maps the span back to `U+FFFC` + the same `attachmentInfo` (by `data-ref`),
  so the text layer round-trips exactly and an edit elsewhere in the note
  never disturbs the object. Deleting the span in the editor deletes the
  character; the `InlineAttachment` record is left orphaned (Apple tolerates
  orphans; cascading deletes are out of scope).
- **~~The `InlineObjects` refusal narrows, on measurement.~~ REVERSED
  during implementation, on a measurement of its own.** The plan was: after
  F5, a note whose every inline object is a resolvable inline-text
  attachment becomes writable (48 notes), while media and tables stay
  refused. That rests on "an edit never disturbs the object", which the
  bullet above asserts — and a probe run against the real splice showed it
  is false for one edit in particular. **Moving a `U+FFFC` within the text
  leaves its `attachmentInfo` run behind**: the character arrives at its new
  position covered by a plain run, resolving to nothing, and Apple renders
  the user's tag as empty. (`compose.rs`'s
  `an_edit_that_moves_an_inline_object_loses_its_attachment_info` pins it,
  and is written to FAIL the day the write path learns to rebuild the run —
  which is what re-narrowing needs, plus a live pass of its own.)

  So M3 ships the half it has evidence for: **hashtags are READ** (they
  render, and `note_tags` fills on this backend for the first time) and the
  notes carrying them stay **read-only**, exactly as M2 left them. Measured
  cost: 48 of 782 notes, 6%. The UTI allow-list still matters for the read
  side and is still measured rather than assumed — F7's census reports the
  distribution.
- **Creating a NEW native hashtag from Jodd is deferred**, stated: it needs
  a fresh `InlineAttachment` record whose field shape no reference
  implementation writes (icloud-md never creates one) — that is a live
  capture away, not a guess away. Until then a `#word` typed in Jodd stays
  plain text: non-destructive, indexed locally by the deriver, rendered as
  text by Apple.

### F6 — gate changes (`compose::writability` + save path)

- `layers_round_trip` (refusal 6) upgrades from plain-text equality to the
  full new pipeline: decode format → render HTML → parse back → text equal
  AND `formats_round_trip_equal` on the projection. A note that fails stays
  refused with the same reason as today. Some of the current 14
  `LayersDoNotRoundTrip` notes may become writable; F7 measures.
- The save path (`ICloudVertical` update branch) becomes:
  `parse_editor_html` → recompose text through the title layer (unchanged)
  → `with_text_crdt` (unchanged) → `reconcile_note_format` + 
  `apply_formatting_op` (new, downgrade-on-refusal) → encode.
- `NoteDocument::new` (create) keeps its bare one-run shape for the text,
  then runs the same reconcile against the desired paragraphs — a note
  created with formatting gets real runs from day one. The create path
  parses hashtag spans **as their text** rather than as objects
  (`parse_editor_html_objects_as_text`): a new record cannot reference
  another note's `InlineAttachment`, so emitting a bare `U+FFFC` would
  create the dangling object the F5 bullet above describes. A `#word` typed
  into a new note is therefore plain text on Apple's side — see F5's
  deferred-creation note.

### F7 — the formatting census (measure before trusting)

`icloud_census` grows a formatting section, read-only, run against the live
account before the write side is trusted: how many notes'
`decode_note_format` succeeds; the distribution of refusal reasons; the
paragraph-kind and `style`-value distribution (any value outside the map is
a named line, not a guess); the `InlineAttachment` UTI distribution (feeds
F5's allow-list); how many of the 48 `InlineObjects` notes become writable;
how many of the 14 `LayersDoNotRoundTrip` notes pass the new gate; and a
whole-account `formats_round_trip_equal(decode(x), parse(render(decode(x))))`
count — the read side proven against every real note before any write.

## What M3 deliberately does not do

- **`DoesNotRoundTrip` (95 notes)** — the prost re-encoding gap in
  `topotext.String` is untouched; those notes stay refused. Separate
  investigation, unchanged since M2.
- **Media attachments and tables** stay refused (`InlineObjects`, narrowed
  but not removed).
- **Colors, alignment, fonts beyond bold/italic, superscript, emphasis** —
  never rendered, never rewritten, byte-preserved by clone-overlay.
- **Inline `<code>`** is dropped on this backend (no Apple equivalent).
- **Native hashtag creation** — deferred pending a live capture (F5).
- **Title-line formatting** — the title is edited in a plain input; its
  paragraph keeps whatever style it has (desired[0] ≡ current[0] by
  construction, so the reconciler never touches it).

## Verification

**Unit tests (no live account), every fixture a measured shape:** the F1
decode against documents built from the wire values icloud-md's captures
confirmed (style codes, fontHints bits, newline-anchored paragraph state,
absent-style-means-Body, under- vs over-covering run tables); F2 render/parse
inverses over every shape the editor produces (including Thai text, emoji —
UTF-16 offsets — checkbox rows, nested lists, `margin-left` indents, the
hashtag span); F3 clone-overlay properties ported from the reference's
documented behavior (untouched runs pass by identity; unknown fields survive
a neighboring rewrite byte-for-byte; todo-uuid dedup; bare-URL links never
removed; dash lists never rewritten to 100 when unchanged); F4 restamp
widening (runs never split; clocks never regress; op counter advances); F6
gate matrix. Round-trip properties as `proptest`-style loops over the same
corpus shapes where cheap.

**Gates (the commands CI runs, not narrower ones):** `cargo test
--workspace` · `node scripts/gen-changelog.mjs` · `npx vitest run` ·
`npx svelte-check --threshold error` · `npm run build`.

**Live phase (blocked on the user being present — keychain prompts, second
device):** run F7's census first and read it before any write. Then, on
FRESH Apple-typed scratch notes in `Notes/Jodd.M2.5.test`, Notes.app quit
during every write, every pass re-checked past 20 minutes (Addendum 3's
premature-verification trap is the reason):

1. Apple-typed note with bold + heading + bullet list + checklist + a real
   `#tag` → verify Jodd renders all of it; toggle a checkbox and bold a word
   in Jodd → verify in Notes.app (20+ min, then iPhone).
2. Jodd-created note with formatting → verify Apple renders it.
3. Text edit in Jodd on a formatted note → formatting elsewhere untouched.
4. The concurrent-format check for F4's divergence: format paragraph A in
   Jodd while paragraph B (sharing a run) is restyled in Apple; verify
   neither client's change is lost after the merge settles.
5. A hashtag note edited in Jodd → tag still live in Apple (renders as a
   token, filters in Apple's tag browser).

## Open questions the live phase answers

- The hashtag/mention UTI strings (F7 census).
- Whether Apple re-parses a plain-text `#word` synced from Jodd into a real
  tag on its side (observed behavior decides how loudly the "deferred
  creation" limit needs stating in the UI).
- Whether widened restamps ever lose a concurrent adjacent restyle (test 4);
  if splits prove necessary, they get their own fresh-note live pass first.
