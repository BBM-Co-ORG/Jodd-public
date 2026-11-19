# iCloud — Vertical #4, Milestone 2 (write)

> Companion to [2026-08-21-icloud-vertical-m1-design.md](2026-08-21-icloud-vertical-m1-design.md),
> which shipped the read path and closed its live pass on 2026-08-23 (772 notes,
> 101 folders, agreeing with Notes.app note for note). This document covers only
> what M2 adds: writing.

## Goal & framing

M1 could not lose a byte, because it could not write one. M2 removes that
guarantee, so every decision here is about **what Jodd refuses to write**, not
about what it can.

The backend is a record store whose note body is Apple's own CRDT document, and
Jodd decodes exactly one field of it — `topotext.String.string`, the visible
text. Attribute runs (bold, links, checklists, paragraph styles, attachments,
tables) are decoded by nobody here. A naive write would therefore reconstruct a
document from text alone and **destroy every formatting decision the user ever
made on that note, server-side, silently.** That is the failure this milestone
is designed around.

M2's answer is to **preserve what it does not understand rather than
interpret it** (Component L), and to **refuse, per note and visibly, whenever
that preservation cannot be proven safe** (Component N).

## What M2 ships

| Area | M2 | Why |
|---|---|---|
| Note create / edit / move / delete | **yes** | `records/modify`, `recordChangeTag` as a real optimistic lock |
| Formatting already on a note | **preserved** | opaque attribute-run splice — never interpreted |
| Formatting newly applied *in Jodd* | **not written** | needs the run semantics M2 deliberately does not guess (Component L3) |
| Folder create / rename / delete | **no** | no evidence either way yet — Component O |
| Sidecars (pin) | **no** | Apple's own pin is a different record type; `RemoteWins` makes a Jodd-side pin meaningless — Component O |
| Attachments | **never** | the iCloud *web* editor cannot attach a file, so there is no client behaviour to mimic |
| Inline `#hashtags` | **still not derived** | Component K stays open; it needs run *semantics*, which L avoids on purpose |
| Recently Deleted / restore | **yes**, restoring to a folder the user picks | the Trash is a real folder that was arriving in every walk; where a note came FROM is not on any field this code reads — Component R |

## Decisions

### 1. The content model is opaque preservation, not interpretation

Three models were available.

1. **Text-only write** — rebuild `topotext.String` from the new text and drop
   everything else. Cheapest, and it silently destroys every attribute run on
   the note. Rejected outright.
2. **Full interpretation** — map Jodd's HTML onto attribute runs and back.
   This is the "full CRDT document model" the M1 spec deferred. It needs the
   *meaning* of Apple's constants (`fontHints` bit values, `ParagraphStyle.style`
   codes, todo UUIDs). Nobody here has measured them, and gotcha #7's rule —
   presets are empirical, not documentation-derived — applies exactly.
3. **Opaque splice** — decode the *whole* `topotext.String` (prost already
   yields every field: `substring`, `timestamp`, `attribute_run`), keep every
   value untouched, and adjust only the run **lengths** so they still cover the
   new text.

**M2 ships 3.** It needs no constant's meaning: a run whose `font_hints` is 1
is carried through as a run whose `font_hints` is 1, whatever that means. The
one thing it must know is arithmetic — how run lengths relate to text length —
and that is measured rather than assumed (Component M).

### 2. The write is refused unless six things hold

`doc::writability` is a single function returning `Writable` or a named reason.
It is the only place that decides, and every reason is a sentence the user can
read on the row (`notes.push_blocked_reason`, gotcha #14).

1. **Record type is `Note`.** A `PasswordProtectedNote` is not a note with an
   empty body — it is a different record type, and the H3 amendment requires
   the guard to key on the type rather than on `wire::LOCKED_BODY_HTML`, which
   would pass the moment the user edits the placeholder.
2. **The document round-trips byte for byte.** Decode → re-encode → compare.
   icloud-md's own rule (PRIOR-ART practice #1): *reproduce the remote's
   current form exactly from your own model, or refuse to edit it.* A document
   that does not round-trip is one this code's model does not cover.
3. **No CRDT identity to mint.** `substring` empty and `timestamp` absent. A
   populated `substring` assigns a `CharID` to character ranges; inserting text
   into one would mean minting IDs for a replica Jodd is not, and there is no
   correct value to invent.
4. **The runs cover the text exactly** — `sum(run.length) == text length in
   UTF-16 code units`. The splice is length arithmetic; if the arithmetic does
   not already hold on the remote's own document, the splice would silently
   mis-attribute the whole note.
5. **No inline objects** — no `U+FFFC` in the text. An attachment, a table or
   an inline hashtag is an object whose run carries its identity; an edit that
   crosses one can orphan it, and Jodd's editor shows the user nothing there to
   tell them so.
6. **The title layer round-trips.** `recompose(text, title_of(text),
   body_of(text)) == text`, evaluated against the remote's own current text.
   This is the guard PRIOR-ART said would have caught gotcha #17, applied at
   the layer that produced gotchas #11, #17 and #21.

Six refusals, each named. A note that fails one is not pushed, keeps its local
edit, and says which. **The distribution of failures on a real account is
itself the measurement that decides what M3 relaxes first.**

### 3. The title's cut position is re-derived, never stored

Gotcha #21 left M2 two obligations, and both are discharged by the same move.

`strip_leading_title` cuts by position, so empty lines *above* the title are cut
with it, and a note that is only a title strips to an empty body (41 of 776 on
the measured account). Re-composing `title + "\n" + body` would therefore
**destroy the leading blank lines on the first push**, and would be unable to
tell a title-only note from one whose body the user emptied.

`doc::recompose` instead edits the remote's own text in place: it finds the same
title line `strip_leading_title` found, and replaces *that line* with the new
title and *everything after it* with the new body. The blank lines above are
prefix, and survive because nothing touched them. A title-only note has no
trailing newline to invent, so it stays title-only.

`recompose(t, title_of(t), body_of(t)) == t` is asserted as a property in
tests **and enforced at runtime** as refusal (2)-(6)'s sixth clause. An empty
body is therefore never read as corruption — it is compared against what the
remote actually holds.

### 4. Delete moves the record to Apple's Trash, and the Trash is readable

`TrashFolder-CloudKit` is a real folder record. A delete sets the note's
`Folder` reference to it, which is recoverable **in Apple Notes** — a strictly
better failure mode than setting `Deleted = 1` and finding out later that
nothing can undo it.

`Capabilities::has_trash` is **true**. The records were arriving in every zone
walk all along; M1 counted them and threw them away. See Component R for the
one thing the view cannot know.

### 5. A write is FOLDED into the account scan, not dropped from it

`AccountCache` holds one zone walk for up to five minutes and every read filters
it. Three things could happen to that cache after a write, and two of them are
wrong.

- **Leave it.** The 2500 ms folder sweep renders the cached walk, so the user
  watches their own edit revert.
- **Drop it.** `save_note_full` takes a scan *before* it writes, so the worker
  draining five dirty notes performs five whole-zone reads back to back against
  Apple's private API — and a second edit in the same burst pays a network round
  trip to learn a `recordChangeTag` the first write already returned.
- **Fold it.** The server said what the record now is; the cache can be told
  rather than asked. `AccountCache::apply` patches the cached `Scan` in place —
  the note, and the `WriteBase` the next edit reads its optimistic lock from.

**The fold does not refresh the cache's timestamp.** A write is not a read, and
letting one extend `MAX_AGE` would let a busy editor hold a walk past the age at
which it is meant to be re-walked.

Folding is each caller's job, not `modify`'s, because only the caller knows what
changed: a content write replaces a note, a delete removes one (it is in the
Trash, which every listing excludes), a move relabels one. The fold also carries
Apple's own pin forward rather than rebuilding the note with `false` — gotcha
#23's third layer, the one that made the pin work in exactly one view.

### 6. An absent record means "deleted elsewhere" only if the zone was read

`push_one_dirty` answers a `NotFound` on an update by dropping the local row and
the unpushed edit it was holding, and `InPlaceUpdateIncludingMove` declares this
backend's `NotFound` trustworthy. It is — of a walk that finished. A walk that
stopped at the page cap, or on a `moreComing` it had no token to resume from,
holds a partial zone in which any note can be missing for no reason at all.
`Scan.complete` tells the two apart; a partial walk reports `Transient`.

### 7. The cached title stops being `TitleEncrypted`

Found by writing the round-trip test, not by reasoning about it, and it is a
read-path change M2 needs before its first push.

`TitleEncrypted` is a **lossy derivation** of the note's first line — truncated
past ~65 characters, inline objects rendered as text, `U+2028` removed — and
diverges on **215 of 776** notes on the live account (gotcha #21). The body is
cut by position, so the line itself is in neither cached field. Pushing
`title + body` back from that cache truncates the note's own first line **on the
server**, for one note in four, silently, on the user's first edit.

`doc::note_title` caches the line instead. `TitleEncrypted` keeps the job gotcha
#21 already gave it: verifying, never deciding. Two consequences worth stating:
Jodd's note list shows the full line where Apple's shows an ellipsis, and
refusal (6) must derive the title the same way — comparing against the record's
field would refuse those same 215 notes for a reason that has nothing to do with
whether they are safe to write.

## Component L — the document write path (`doc.rs`)

### L1. `encode_note_document(&topotext::String) -> Vec<u8>`

`topotext.String` → protobuf → `versioned_document.Document` → protobuf →
**gzip** → the caller base64s it.

**gzip, not zlib**, and the reason is measured (gotcha #20): 774 of 776 notes on
the live account are gzip and the two zlib ones came from the iCloud *web*
client. Writing the container Notes.app itself writes is the unremarkable
choice. The reader accepts both regardless.

The `serialization_version` / `minimum_supported_version` on the wrapper are
**carried over from the document being replaced**, never chosen: they are a
compatibility contract with Apple's own clients, and inventing a number is how
a note becomes unreadable on an older iPhone.

### L2. `splice_runs(old_text, new_text, runs) -> Vec<AttributeRun>`

A prefix/suffix diff in **UTF-16 code units**:

- longest common prefix `p` and suffix `s` (clamped so `p + s` never exceeds
  either length, and never splitting a surrogate pair),
- the runs before the change keep their lengths untouched,
- the run containing offset `p` absorbs the length delta,
- a deletion larger than that run consumes the following runs, which are dropped
  when they reach zero,
- the result always covers the new text exactly, and is never empty while the
  text is non-empty.

No value is read. No value is written. Only `length` changes.

### L3. What this deliberately loses, and why it is stated rather than hidden

Formatting the user applies **in Jodd** does not reach iCloud: the body arrives
as HTML, the write path takes its text, and the splice preserves the *remote's*
runs rather than deriving new ones. Bold typed in Jodd is bold in Jodd's cache
and plain in Apple Notes.

The alternative — refusing the push because the HTML gained a `<b>` — would
block the user's entire text edit over one keystroke of formatting, which is a
worse trade than the one being made. **It is recorded as a known M2 limit**, the
same way M1 recorded having no tags on this backend, and it is what Component K
and full rich-text writing close together in M3, because both need the same
run semantics.

## Component M — the census that decides the gates (`examples/icloud_probe`)

The six refusals above are principled, but the *rate* at which each fires is
unknown, and a gate that refuses 95% of an account is a feature that does not
exist. M1's method applies: **ship the diagnostic, measure, then decide.**

The probe grows a write-readiness census over the same 776 notes. Names and
counts, never values (the rule every M1 diagnostic follows):

| Line | The question it answers |
|---|---|
| `documents re-encode byte-for-byte: n/m` | is refusal (2) viable at all, or does prost simply not reproduce Apple's encoder |
| `substring populated on n note(s)` | refusal (3) — is there CRDT identity on the wire, or is the note document flattened |
| `run coverage: n exact (UTF-16), n exact (chars), n neither` | refusal (4), **and which unit `length` is in** — the one arithmetic fact the splice depends on |
| `attribute runs per note: min/median/max` | how much structure a splice is actually moving |
| `runs carrying <field>: n` (names only) | which run fields occur, so M3 knows what interpreting them costs |
| `paragraphStyle.style values: {v: n}` | the value histogram — a measurement, not a meaning |
| `text contains U+FFFC: n note(s)` | refusal (5)'s real cost |
| `title layer round-trips: n/m` | refusal (6) — and a regression check on `strip_leading_title` |
| `wrapper versions: {(ser, min): n}` | what L1 must carry over rather than choose |

`JODD_PROBE_REDACT` continues to govern; none of these lines can print note
text under any account shape, redacted or not.

**These stay until M3 closes**, on the same terms the M1 walk diagnostics were
kept: they are the instruments pointed at the milestone still open.

## Component N — refusal, per note, on the row

`Capabilities` answers per **backend**; `Account.blocked_reason` answers per
**account**; a locked note and an unspliceable document are per **note**. I2
named that third axis and said M2 is where it connects. It connects through
machinery that already exists and needed no schema change:

- `save_note_full` returns `TransportError::Permanent` with the named reason,
- `push_one_dirty` (lib.rs) stamps `notes.push_blocked_reason` and stops
  retrying (gotcha #14),
- `has_pending_pushes` already excludes blocked rows, so one unwritable note
  cannot wedge a Draining account (gotcha #2's wedge),
- `append_blocked_notes` already merges the row back into the listing, and the
  editor already has a save-status state for it.

**The refusal is not stamped at pull time**, deliberately. `apply_local_edit`
clears `push_blocked_reason` on every edit — that is the re-arm path gotcha #14
specifies — so a pull-time stamp would be erased by the first keystroke and
would have to be re-derived at push anyway. Deciding once, at the point of
writing, is the only place the answer cannot go stale.

## Component O — what stays `false`, and on what evidence

**Folders: `false`, because nothing has been measured either way.** Unlike
Microsoft, where `writes.folders = false` is backed by a mechanism (the
`IPF.StickyNote` container class cannot be set and is immutable afterward),
CloudKit gives no reason to think a `Folder` record cannot be created — it is
an ordinary record with `TitleEncrypted` and `ParentFolder`. That is an
argument for *trying* it, not for shipping it: the goal here is to decide on
evidence, and there is none. M3 measures it with the write probe and flips it
or documents why not.

**Sidecars: `false`, and this one is a design answer rather than a gap.** The
pin on this backend is Apple's own, on a `Note_UserSpecific` record, and
`db::remote_pin_policy(ICloud)` is `RemoteWins` (I1b) — so a Jodd-written
sidecar pin would be overwritten by the next pull, i.e. a control that
visibly does nothing. The right implementation is to write the per-user record
itself, which is a different record type, a tombstone hazard (gotcha #23) and
its own measurement. It is M3's, and it is not a sidecar.

## Component P — `accounts.sync_cursor` and the worker's change detector

The M1 spec assigns both to M2, in three places (decision 7, Component J, and
the deferred list). Both are here.

### P1. The cursor

`Account.sync_cursor: Option<String>` on the account record — opaque bytes the
core never parses, exactly as `SyncCursor` has always promised. Persisted
rather than kept in memory for one reason, and it is worth naming because it is
the only one: **across a restart**. An in-memory token makes the first pull
after every launch a from-scratch read of the whole account; a stored one lets
the first tick after a restart notice what changed while Jodd was closed.

**A cursor is a hint, never a source of truth.** Losing it, or storing a stale
one, costs at most one extra full read — the authoritative pull reads
everything regardless. Nothing may be pruned, deleted or believed on the
strength of what a cursor did or did not report.

### P2. A detector, not a pull

The sync worker gains one step, last in the tick, on its own `PULL_INTERVAL`
(60 s) rather than the worker's 5 s — which is right for draining a local queue
and far too fast for a request against somebody's private API.

It asks `changes_since(cursor)` what changed, stores the new cursor, and if
anything changed **drops the account's cached zone read**. It deliberately does
not apply the changes: a `RemoteChange` carries a `remote_id` and a kind, never
content, so applying one would mean fetching each changed note — and here a
per-note fetch costs a whole zone read (`fetch_note`'s own doc comment prices
that at "an explicit user action", which a background tick is not).

**That is also its answer to gotcha #6.** State the worker changes on its own
needs a route back to the frontend; this needs no new channel because it
changes nothing the frontend reads directly. It invalidates a cache the
existing 2500 ms folder sweep re-reads through, so the change surfaces on the
next sweep. Reconciling into SQLite here instead would change state the UI
displays with nothing telling the UI — the exact shape of the defect gotcha #6
records.

### P3. Priming is not a change

With no stored cursor there is nothing to have changed *since*, and
`changes/zone` with no token hands back the whole account. Reporting that as
"everything just changed" would drop the cached read on every fresh install and
every restart that lost the file. `PullOutcome::Primed` establishes the cursor
and invalidates nothing.

### P4. A quiet run writes nothing

The backend hands back a fresh token on every call, so storing each one would
rewrite `accounts.json` — a file that also holds the user's settings — once a
minute, for the life of the process, to record that nothing happened. The
cursor already in hand is still valid: re-sending it next time asks the same
question and gets the same empty answer. A token only has to advance past
changes that were actually acted on, and icloud-md measured a fifteen-day-old
one still syncing incrementally.

On a run that DID find changes the cursor is stored **before** the invalidate.
If the process dies between the two, the worst case is a cursor that has moved
past a change the cache never dropped — and the authoritative poll reads
everything regardless. Storing after would risk re-reporting the same change
every minute forever if the write kept failing.

An account with a `blocked_reason` is not polled at all. Advanced Data
Protection is the only producer today, and an account it has already found
unreadable cannot become readable by being asked again on a timer;
`index_account` re-stamps the field on every pass, so it clears itself the
moment the account works again.

### P5. Which backends

`incremental_pull_supported` is an exhaustive match — a fifth backend fails
closed rather than inheriting a poll against an endpoint it does not have.
Gmail and Microsoft return an inert cursor from `changes_since`, so a detector
there would report "nothing changed" forever, which is worse than not running.
LocalFs implements it honestly, but its notes are files on the user's own disk
with no rate limit and no session to spend.

## Component R — Recently Deleted, and the one thing it cannot know

### R1. Reading it costs nothing new

A trashed record is in the zone like any other; M1's decode threw it away with
tombstones because neither appears in a listing and the difference bought
nothing. It buys a restore button. `Decoded::Trashed` splits the two, `Scan`
carries them in `trashed` — **not** in `notes`, or every deleted note would
appear in the root — and `fetch_note` searches both so the preview works.

They are decoded in full, bodies included, so the trash view can preview them.
The cost is one gunzip per trashed record per walk, and Apple purges the folder
after thirty days.

### R2. Where a restored note goes is the open question

A trashed record's `Folder` reference has been **replaced** by the Trash's. The
folder it came from is not on any field this code reads.

That is a genuine difference from the other two backends, not a gap in effort:
a trashed Gmail message keeps its `Notes/*` label alongside TRASH, and LocalFs
encodes the original relpath into the trash filename. Both can put a note back
where it was. This one cannot.

**So it asks.** `TrashedNote::original_known` is false here, the menu offers
only "Restore into…", and `restore_note` refuses a restore with no destination
rather than defaulting to the root. Filing every restored note in the root and
calling it a restore is a silent reorganisation of somebody's account — the
same class of thing as the orphan-relocation defect, arriving through a
different door.

`Transport::untrash` stays refused for the same reason, and nothing is lost by
it: a restore here is one write either way, and there is no separate un-delete
step to perform first. `restore_note` routes this backend through
`RestoreKind::MoveOutOfTrash` — a declared dispatch replacing an
`if Gmail { … } else { …LocalFs… }` that would have run LocalFs's
trash-filename decode over a CloudKit `recordName`.

### R3. The diagnostic that could close it

Apple's own client restores a note to the right place, so the information
exists somewhere. `Folders` (**plural**) is the standing candidate: it is in
`DESIRED_KEYS` because the web client asks for it, it comes back on real
records, and nothing reads it.

`examples/icloud_probe` now reports what a trashed record actually carries —
field names, and `Folders` by shape, with record names only. If it names a
folder that is not the Trash, restore can stop asking. If every trashed record
shows `absent`, asking is the right answer and the diagnostic retires with M3.

## Component Q — what M2 hands on, with a reason

These are decisions, not deferrals:

- **Rich text written from Jodd** and **inline hashtags** (Component K) need the
  same thing — the *meaning* of Apple's run constants — which Component L
  deliberately never learns. They close together or not at all.
- **A restore that does not have to ask** needs the folder the note came from,
  and nothing measured says CloudKit keeps it anywhere this code reads.
  Component R ships the restore and the diagnostic; closing the question is
  M3's, and it may close by measurement alone.

## Verification

### Unit-testable with no live account

| Target | Why it matters |
|---|---|
| `encode → decode` round-trip on a document Jodd built | the write path's own floor |
| `decode → encode` byte-identity on a fixture built by an independent encoder | refusal (2) is only meaningful if identity is achievable |
| `splice_runs` — insert, delete, replace, at a run boundary, spanning runs, emptying a run, emptying the text | the arithmetic, exhaustively |
| `recompose(t, title_of(t), body_of(t)) == t` over every `strip_leading_title` fixture | refusal (6), and gotcha #21's obligation |
| leading blank lines survive a title edit | the M1 behaviour that starts costing on write |
| a title-only note stays title-only | 41 of 776 |
| `writability` — one test per refusal, each asserting the *reason* | a gate that fires for the wrong reason is not a gate |
| `records/modify` request shape, and a per-record error inside HTTP 200 | CloudKit reports write failures the way `changes/zone` reports a rejected token |
| `recordChangeTag` mismatch → `TransportError::Conflict` | the optimistic lock is the whole safety story for concurrent edits |
| a locked note refuses by record type, and still refuses after its placeholder body is edited | the H3 amendment, stated as a test |
| push → DB → push again, under the pre-rekey uuid | gotchas #16 and #18: a transport probe cannot see this |
| a successful write invalidates the account scan | decision 5 |
| a trashed record decodes as recoverable, a tombstone does not | R1 — M1 collapsed the two |
| a trashed note is absent from every listing and present in the trash view | R1, and the root label it carries is a placeholder |
| a delete moves the note into the cached trash; a restore moves it out | otherwise it shows in neither place, or in both |
| `has_trash` and `restore_kind` agree, per backend | a view whose button always refuses is what the capability exists to prevent |

### Needs the live account (manual, recorded in the milestone notes)

1. The census (Component M) — run first, before any write lands.
2. `examples/icloud_write_probe` — create → read back → edit → read back →
   move → delete, on a **scratch note it created itself**, refusing to touch any
   pre-existing record.
3. The same cycle through the app: editor → worker → Notes.app on a Mac and an
   iPhone.
4. A locked note edited in Jodd stays refused and says so.
5. A note edited in Jodd and in Notes.app between two ticks lands as a conflict
   copy, not as an overwrite.

### Containment before any live run

The self-test operates only on a `recordName` it minted itself — minted inside
the vertical, never taken from the caller, so no argument can point it at a
real note. A read-only census run must be possible with no write path enabled
at all, which is what makes step 1 safe to run first.

**Both run in the app, against the session the app already holds**, and that is
a correction rather than a preference. `examples/icloud_probe` reads
**icloud-md's** stored cookie jar — scaffolding from the M1 verify-first phase,
before `icloud_auth.rs` existed. Building M2's diagnostics on it meant a
third-party tool's expired session standing between the maintainer and a
measurement of his own account. M1 shipped Jodd's own session; the diagnostics
that came after it should have started there.
