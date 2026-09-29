# Microsoft/Outlook — Vertical #2, Milestone 2 (the write path)

> Status: **design / approved** (2026-08-14). Turns on note creation, editing,
> deletion and folder operations for the Microsoft/Exchange vertical. M1 shipped
> read-only and is merged (`aa5ca63`).
>
> Builds on:
> - [2026-08-14-microsoft-vertical-design.md](2026-08-14-microsoft-vertical-design.md) (M1 — read path)
> - `CLAUDE.md` → "How Apple Notes ↔ Outlook.com works", gotchas #4, #9, #11, #12
> - [HANDOFF-2026-08-14-microsoft-m2.md](../HANDOFF-2026-08-14-microsoft-m2.md)
>
> **Acceptance bar:** on a live `@live.com` account, a note created, edited,
> retitled, moved and deleted in Jodd appears correctly in Notes.app and on the
> iPhone, with no duplicates and no conflict copies; folders can be created,
> renamed and deleted; pin and tags remain refused. `cargo test --workspace` —
> not a narrower gate.

## What was measured before this design existed

Every M2 unknown was run against the live account on 2026-08-14 via
`scripts/ms_write_probe.py` and `scripts/ms_folder_probe.py`, with the
Apple-side half confirmed by eye in Notes.app. **Nothing below is inferred from
documentation.**

| Behaviour | Result |
| --- | --- |
| `POST /me/mailFolders/{id}/childFolders` | **201** with *and* without `PR_CONTAINER_CLASS`; **both appear in Notes.app** |
| `PATCH /me/mailFolders/{id}` `displayName` | **200**, confirmed through `PR_PARENT_DISPLAY` on a witness note |
| `GET /me/mailFolders/{id}/messages` after `DELETE` | **404** (200 before the delete) |
| `POST /me/messages/{id}/move` with `Prefer: IdType="ImmutableId"` | **201**, Graph `id` **and** `internetMessageId` preserved |
| the same move **without** the `Prefer` header | id **not** preserved |
| `PATCH` of `subject` + `body` | **200**, id unchanged, one correct note in Notes.app with no duplicate title line |
| `POST` a note with **no** `PR_MESSAGE_CLASS` | **201**, but the item is `IPM.Note` — an **email**, silently filed in the Notes folder |

Four consequences shape everything below.

1. **Folders and notes are asymmetric on their class property.** A note without
   `PR_MESSAGE_CLASS` is silently wrong; a folder without `PR_CONTAINER_CLASS`
   is completely fine. Reasoning by analogy from one to the other would have
   produced a confident, unfounded claim — which is why probe 1b existed as a
   separate script rather than as an assumption.
2. **`Prefer: IdType="ImmutableId"` is load-bearing, now causally.** The
   contrast run is the evidence: the same move without the header changed the
   id. Never drop it.
3. **A deleted folder id 404s.** This gives a *positive existence check* for a
   folder id already held, which is what makes Component F possible.
4. **`PATCH` leaves the id unchanged** — the headline advantage of this backend,
   and the reason Component D exists.

## Decisions locked in brainstorming (2026-08-14)

- **Probe before design.** Six behaviours branched the architecture; all were
  run first and the results are the table above. The M1 lesson generalises: a
  fixture written by whoever wrote the matcher can only prove the matcher agrees
  with itself, and *two readers of the same API* prove only that the API agrees
  with itself. Apple is the independent observer.
- **`can_write` splits into `Writes { notes, folders, sidecars }`.** M2 ships
  note and folder writes but not sidecars, and one boolean cannot say that.
- **New folders are created under the `Notes` root.** Rejected: the selected
  folder (Jodd renders the result flat, so the user sees something other than
  what they asked for) and a parent picker (exposes a hierarchy Jodd cannot
  display back).
- **Delete confirms when `has_trash` is false.** Capability-driven, not a
  `backend_kind` conditional. Rejected: confirming everywhere (friction on
  genuinely recoverable Gmail deletes) and a Jodd-local soft delete (a milestone
  of its own).
- **A created note's uuid is rekeyed after its first push,** and the resulting
  wikilink window is documented rather than engineered around.
- **Conflict detection moves off the remote id and onto a per-vertical
  `version`.** This is the "should `Transport` be reshaped?" evidence the M1
  spec deferred to M3, arriving early and from an unwatched direction.

## Component A — capabilities split

`Capabilities` gains a `Writes` struct in place of `can_write`:

```rust
pub struct Capabilities {
    pub folder_model: FolderModel,
    pub fidelity: Fidelity,
    pub has_trash: bool,
    pub writes: Writes,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Writes {
    pub notes: bool,     // create / edit / delete / move a note
    pub folders: bool,   // create / rename / delete a folder
    pub sidecars: bool,  // pin, tags
}

#[derive(Clone, Copy, Debug)]
pub enum Write { Notes, Folders, Sidecars }
```

| vertical | `notes` | `folders` | `sidecars` |
| --- | --- | --- | --- |
| Gmail | true | true | true |
| LocalFs | true | true | true |
| Microsoft | **true** | **true** | **false** |

### A1. Why this is not cosmetic

`refuse_if_read_only` ([lib.rs:1117](../../../src-tauri/src/lib.rs)) exists to
stop a write reaching SQLite on a backend that cannot push it. Its doc comment
states the cost of a leak: a `dirty` / `pin_dirty` / `tags_dirty` row makes
`db::has_pending_pushes` true forever, the account can never leave Draining, and
`remove_account` refuses Draining (gotcha #2) — **the account becomes
permanently unremovable.**

Flipping a single `can_write` to true would reopen that trap one milestone
early, because the same boolean guards `set_pin`, `set_pin_batch`, `add_tag`,
`remove_tag`, `rename_tag` and `delete_tag`, all of which route to
`put_sidecar` — which stays unimplemented until M4.

Note the asymmetry that makes sidecars specifically dangerous: a failed *note*
push retries harmlessly forever, but sidecar dirt is what `has_pending_pushes`
reads, and that is wired to account lifecycle. Same retry loop, very different
blast radius.

Exchange sidecars are also genuinely unexplored, not a port. On Gmail a sidecar
is a message under a `Notes-Meta` *label*, invisible to Apple. Exchange has no
labels — a sidecar must be an item in some folder, and any item in the Notes
tree renders in the user's Notes.app as a stray note. That is a design problem,
which is why M1 assigned it to M4.

### A2. Call-site mapping

`refuse_if_read_only(state, id)` becomes `refuse_write(state, id, Write::…)`.
The eighteen guarded commands map as:

| kind | commands |
| --- | --- |
| `Notes` | `save_note`, `delete_note`, `move_notes_batch`, `delete_notes_batch`, `restore_note`, `apply_wiki_link_appends` |
| `Folders` | `create_folder`, `rename_folder`, `delete_folder`, `move_folder` |
| `Sidecars` | `set_pin`, `set_pin_batch`, `add_tag`, `remove_tag`, `rename_tag`, `delete_tag` |
| **both** `Notes` **and** `Folders` | `extract_note`, `append_extract_note` — each writes a note *and* calls `ensure_workflow_folder` |

A miscategorised call site reopens the wedge, and nothing in the type system
catches it. Mitigation: a table-driven test listing every command with its kind,
so the mapping is asserted rather than assumed. This test is the spec, executable.

Refusal messages must be per-kind. "This account is read-only" is now false —
notes and folders are writable — so the sidecar message must say what is
actually unavailable and name M4.

### A3. Frontend and jodd-mcp

`canWriteAccount(caps)` ([notes.ts:92](../../../src/lib/stores/notes.ts)) becomes
`canWrite(caps, kind)`, keeping the optimistic-`true`-while-loading default for
the existing no-flicker reason — safe precisely because the backend refuses
independently. A capabilities object cached before `writes` existed must read as
all-true, mirroring the `can_write`-absent case already tested.

`jodd-mcp/src/write.rs:52` opens the same SQLite directly and is therefore a
second write path. Its five tools take the same per-kind check.

## Component B — the note write path

### B1. `save_note_full` / `save`

```
existing_remote_id == None   →  POST   /me/mailFolders/{folder_id}/messages
existing_remote_id == Some   →  PATCH  /me/messages/{id}
label changed                →  POST   /me/messages/{id}/move
```

**`POST` must always set `PR_MESSAGE_CLASS = IPM.StickyNote`.** Measured: omit
it and Graph answers `201` while depositing an `IPM.Note` — an email — in the
Notes folder. `isDraft: true` came back either way, so `isDraft` is **not** the
discriminator; message class is the only thing separating a note from mail.

**The Gmail sequence is never copied.** Gmail has no REPLACE, so `save_note`
there is insert-new + trash-old with an id repair through `mark_pushed`.
`PATCH` here is a true in-place update: one request, id preserved, no duplicate,
no repair. Copying the Gmail dance would create a second live message per edit.

**Attachments are impossible on this backend** — Apple itself refuses them on
Exchange accounts. `save_note_full` receives `attachments: &[Attachment]`; if it
is ever non-empty, return `TransportError::Permanent` naming the limitation.
Silently dropping them would discard user data with a success result. M1 always
yields empty attachment lists, so this fires only on a future cross-account move.

### B2. Folder resolution on create

A `POST` needs a folder id, and `SaveOp` carries only `label`. Resolve through
the vertical's `folder_ids` map (filled by `vertical_for` from the `folders`
table's `label_id`). An unknown label calls `ensure_folder`, which creates it
under the `Notes` root — see Component F.

## Component C — identity, and the uuid rekey

**Jodd cannot choose a note's identity on this backend, but local-first requires
it to write the row before the identity is known.**

| | Gmail | Microsoft |
| --- | --- | --- |
| uuid source | `X-Universally-Unique-Identifier`, a header **Jodd writes** | `internetMessageId`, which **Exchange assigns** |
| known at | creation, locally | only after the `POST` returns |

So a note created in Jodd carries a locally-minted uuid (`Identity::mint`, which
M1 left in place noting "the write path has it ready" — it is not) that Exchange
will never echo. Left alone, the next pull returns the note keyed by
`internetMessageId`, `upsert_from_remote` finds no such row and inserts a second
one, and `prune_clean` then deletes the original. The user creates a note and
seconds later it is silently replaced by a different row, taking its pins, tags
and editor attachment with it.

**The fix: rekey on first push.** `save_note_full` returns the real
`internetMessageId` in `SavedNote.uuid` — a field that already exists on the
struct and is discarded by every caller today, because on Gmail it is always the
uuid the caller already had. `mark_pushed` gains a rekey path that runs inside
one transaction when the returned uuid differs from the local one.

**The rekey is small, because of "derive, don't migrate" (gotcha #4).** Of the
six tables keyed by note uuid:

| table | treatment |
| --- | --- |
| `notes` | a real `UPDATE` of the primary key |
| `edges`, `note_tags`, FTS5 | **derived from the body** — delete under the old uuid, re-derive under the new |
| `tag_tombstones` | re-keyed with the row |
| `attachments` | impossible on this backend; nothing to move |

**Accepted casualty.** Wikilinks embed `<uuid8>` in the body
(`[[title-slug-abcd1234]]`), so a link created in the seconds between making a
note and its first push will dangle permanently. The window is one settle
interval (`PUSH_SETTLE_MS`, 5 s). Rejected: hiding un-pushed notes from wikilink
autocomplete (a per-backend condition in shared UI) and rewriting other notes'
bodies during a push (which would sync those notes to Apple as a side effect of
an unrelated save — a small problem given a much larger blast radius).

## Component D — conflict detection

**`reconcile_one` currently treats "the remote id changed" as synonymous with
"the remote changed."** That was never a property of remotes in general; it is a
Gmail implementation detail that leaked into backend-agnostic code and read as
universal because it happened to hold for both existing verticals.

```rust
// db.rs:2701
remote_version: Some(n.id.clone()),
// lib.rs:1570
let remote_changed = existing.remote_version.as_deref() != Some(&fetched.id);
```

Gmail re-mints a message id on every content edit, so a changed id **is** the
signal. Exchange `PATCH`es in place and the id is stable — measured, probe 5:
`id unchanged by PATCH: True`. The same property that makes this backend better
to write to disables the conflict detector:

```
Apple edits note X   →  Exchange PATCHes in place  →  id unchanged
Jodd holds X dirty   →  remote_changed = false     →  no conflict detected
worker pushes        →  PATCH overwrites Apple's edit — silently
```

M1 never exposed this: read-only means no row is ever `Dirty`, so the branch is
unreachable. **M2 is what makes it live**, and shipping writes without the fix
means shipping a known silent-overwrite path — strictly less safe than Gmail.

**The fix.** `backend::Note` gains `version: String`, filled per vertical:

| vertical | `version` | note |
| --- | --- | --- |
| Gmail | `id` | byte-for-byte today's behaviour; zero risk |
| Microsoft | `lastModifiedDateTime` | already fetched by the scan |
| LocalFs | file mtime | already stat-ed |

`CachedNote::from_remote` stores it in `remote_version`; `reconcile_one`
compares it. One field, three fills, one comparison site, no backend
conditionals.

**Audit obligation:** `remote_version` has readers beyond line 1570. Every one
must be checked for an assumption that it holds a message id before the meaning
changes underneath it.

## Component E — the title/body split, write direction

M1 shipped `strip_leading_title_ms` (the read half). M2 adds
`inject_title_into_body_ms`.

Apple duplicates the title into the body, so `subject` and the leading body line
must move together or the note shows two different titles. The shape to emit is
the one measured in probe 5 and confirmed in Notes.app — the title as a leading
bare text node, then a `<div>` per line:

```html
<html><body>probe-title-after<div>the body line under the title</div></body></html>
```

**Contract: a round-trip property against the existing stripper.**

```
strip_leading_title_ms(inject_title_into_body_ms(title, body)) == body
```

Table-driven over the four real note shapes from gotcha #11 (plain text node,
bare text, `<b><i>` formatted, leading `<div>`), plus the injected shape itself.
Two traps recorded from the 2026-08-14 investigation and still live: an
element-only walk **skips text nodes**, and three samples yield the
tidy-but-wrong "title = first text node" rule — the formatted-title case is what
falsifies it.

Failure mode if wrong: the first `<div>` is stripped instead — **the second line
of real content, silently, with no error and no log**, on every note.

## Component F — folder operations

### F1. The four operations

| operation | request | evidence (design-time expectation) |
| --- | --- | --- |
| create | `POST /me/mailFolders/{notes_root}/childFolders` | 201; ~~appears in Notes.app~~ — see below |
| rename | `PATCH /me/mailFolders/{id}` `{displayName}` | 200; confirmed via `PR_PARENT_DISPLAY` |
| delete | `DELETE /me/mailFolders/{id}` | 204 |
| move a note | `POST /me/messages/{id}/move` `{destinationId}` | 201; id preserved under `Prefer` |

`PR_CONTAINER_CLASS` is **not** required to get a 201 — measured. Send it
anyway on the `PR_MESSAGE_CLASS` precedent: it costs nothing... except that the
201 was the whole story at design time, and it turned out not to be. Live
testing 2026-08-15 found the created folder never reaches Apple Notes at all
— Graph accepts the request and silently drops the extended property that
would have made it a real Notes container. **The "appears in Notes.app" cell
above was the design-time expectation, not a live measurement — it is now
disproven.** See "Deferred to M3" (folder writes) for the full mechanism
including the `500 ErrorObjectTypeChanged` that rules out fixing it
after creation.

### F2. Where a created folder goes

Jodd's folder tree is flat because the parent/child shape is unrecoverable
(gotcha #12, closed out — every avenue tried and dead). Apple's real tree is
nested. A created folder therefore goes under the **`Notes` root**, making it a
sibling of the folders already visible.

The root's id is reachable only through `PR_PARENT_DISPLAY == "Notes"` on a
message — i.e. **only if a note sits directly in the top-level Notes folder.**
If no such note exists, `create_folder` returns a `Permanent` error saying so
rather than guessing a parent. Rare (Apple creates the folder and users
generally have notes in it) but real, and silent-wrong is the worse failure.

**Stated limitation:** a folder the user thinks of as "inside L1" cannot be
expressed. Jodd cannot see the nesting, so it cannot offer it.

### F3. The vanishing folder, and its fix

Today `list_folders` derives folders *from the notes inside them*, so "absent
from the listing" conflates **emptied** with **deleted**, and
`prune_clean_folders` ([db.rs:3090](../../../src-tauri/src/db.rs)) cannot tell
them apart. That is recorded in CLAUDE.md as deliberate, and on a read-only
backend it is defensible. M2 makes it a bug, because writes create empty folders
on purpose:

| step | folder row | in sidebar? |
| --- | --- | --- |
| user creates "Ideas" in Jodd | `dirty_new` | yes — prune skips non-clean rows |
| worker pushes, `create_folder` succeeds → `mark_folder_created` | **`clean`** | yes |
| next pull: "Ideas" holds no note, so the scan does not report it | not in keep-list | **pruned — gone** |

The folder disappears *because the push succeeded*. A failed push would have
left it `dirty_new` and safe.

**Fix, enabled by the measured 404:**

```
list_folders() = folders derived from the scan
               ∪ known folder ids that still answer 200 on /messages
```

A known id that 404s is genuinely deleted and is pruned correctly; one that
returns 200-empty exists and survives. Cost: one request per
known-but-currently-empty folder per pull — usually zero.

This also repairs the read-side behaviour: **an emptied folder no longer
disappears**, which CLAUDE.md currently documents as accepted. That doc entry
must be updated, not left contradicting the code.

An empty folder created **on the iPhone** is still undiscoverable — Jodd has no
id for a folder it has never seen. That remains a stated limitation.

## Component G — deletion

`DELETE /me/messages/{id}` → 204, and the note leaves Notes.app within ~60 s.
**It is a hard delete with no undo** — measured 2026-08-14: after an Apple-side
delete, Deleted Items held zero items and the note was absent from every scan.
`Capabilities::has_trash` is false on evidence, not on the absence of a visible
trash.

So the UI must confirm rather than lean on a restore path that does not exist. A
confirmation dialog naming the deletion as permanent, shown when
`has_trash === false`. Gmail and LocalFs keep today's one-click delete; both
have a real trash. Capability-driven — no `backend_kind` in the frontend.

## What M2 does **not** implement

- **Folder writes** (create/rename) — code shipped and unit-tested, but
  gated off (`writes.folders: false`) because live testing 2026-08-15 found
  Graph-created folders never reach Apple Notes. See "Deferred to M3" below
  for the mechanism and the one avenue not yet tried.
- **Sidecars** (`put_sidecar`, `remove_sidecar`) — M4. `list_sidecars` keeps
  returning `Ok(None)`, the trait's "do not prune" signal, which protects
  locally-pinned rows.
- **`changes_since`** — keeps returning `Permanent`. Verified this session: it
  has **no callers anywhere in the workspace**. The worker is not driven by it,
  and an honest error beats a fabricated change list a future caller would trust.
- **`find_ids_for_uuid`** — also has **no callers anywhere**, only the trait
  declaration. Left erroring. Worth deleting from the trait, but not on this
  branch.
- **Folder nesting** — unreachable, not unimplemented. Do not spend a cycle.
- **Android** — its own OAuth constraint chain, as Google's was (gotcha #8).

## Verification

Unit-testable with no live account, and worth writing first:

| target | why |
| --- | --- |
| `inject_title_into_body_ms` round-trip | highest-risk function in M2; failure silently eats the second line of every note |
| the `Write` call-site mapping table | a miscategorised site reopens the unremovable-account wedge and nothing else catches it |
| uuid rekey | must move the row and re-derive the derived tables in one transaction |
| `version`-based conflict detection | assert a same-id-different-`lastModifiedDateTime` pull is seen as a remote change |
| `list_folders` existence check | emptied folder survives; deleted folder is pruned |
| Graph JSON decode of write responses | fixtures from the probe runs, **not** hand-written |

**The fixture caution, restated because this branch has already paid for it.**
A request for `String 0x001A` returns `String 0x1a`, and every lookup compared
against the sent form — so a 13-note mailbox read as zero notes while all 82
module tests stayed green, because every fixture was hand-written with the sent
form. Use `wire::same_property_id`. Capture fixtures from real responses.

Needs a live account (manual, recorded in the milestone notes):

- [x] create / edit / retitle / move / delete a note, each confirmed in
  Notes.app — **done, 2026-08-15.** `cargo run --example ms_write_probe`
  drove the shipped code through all five against `kaiwan.h@live.com`; every
  step returned 2xx, and Notes.app confirmed the retitled note in the Notes
  root plus the empty-title note rendering correctly.
- [x] a `cargo run --example ms_write_probe` sibling to `ms_read_probe`,
  running the **shipped** code against the real API — **done, 2026-08-15**,
  see above. The Python probes answered what the API does; this answered
  whether Jodd's code does it. Both `?`s on that path are now closed for
  notes.
- [x] create / rename / delete a folder, confirmed in Notes.app — **run,
  2026-08-15, and it FAILED.** Folders created through Graph never reached
  Apple Notes: two folders created the day before were still absent 21 hours
  later and after a forced resync, while two folders created by hand in
  Notes.app were visible the whole time and received Graph-written notes
  fine. `writes.folders` has been flipped back to `false` as a result — see
  "Folder writes: deferred to M3" below for the full mechanism and what
  remains untried.
- [ ] **the conflict path**: edit the same note on Apple and in Jodd, confirm a
  conflict copy appears rather than a silent overwrite. Still open — not run
  in the 2026-08-15 session, which focused on the note CRUD surface and the
  folder finding above. This is the reason Component D is in this milestone
  and remains the one live-untested piece of the note write path.

Gate — the commands CI runs, never anything narrower:

```bash
cargo test --workspace
cargo check --workspace --all-targets
npx vitest run
npx svelte-check --threshold error
npm run build
```

`cargo test -p jodd --lib` does not compile `src-tauri/examples/`. A gate that
differs from the merge gate only proves it agrees with itself.

## Implementation order (phases)

1. **`Note.version` + `reconcile_one`** — backend-agnostic, no behaviour change
   for Gmail or LocalFs. Lands first so the write path is never live without it.
2. **`Writes` split + `refuse_write` + the mapping test** — still refuses every
   Microsoft write; pure refactor, provable green.
3. **`inject_title_into_body_ms`** with the round-trip property, tests first.
4. **`save_note_full` / `save`** — `POST` (with the class) and `PATCH`, plus the
   uuid rekey and its transaction.
5. **`delete`** + the `has_trash`-driven confirmation dialog.
6. **Folder ops** — create / rename / delete / `move_note`, and the Notes-root
   resolution with its explicit refusal.
7. **`list_folders` existence check**, and update CLAUDE.md's now-stale
   "emptied folder disappears" entry.
8. **Flip `Writes { notes: true, folders: true }`** and remove the frontend
   gates in step with the methods that back them — not before.
9. **Live pass** on `kaiwan.h@live.com`, including the conflict scenario.

Ordering rationale: every phase before 8 is safe to ship half-done, because the
capability still refuses. Phase 8 is the only irreversible step, and it comes
after everything it depends on is proven.

**Actual outcome of phases 8-9 (2026-08-15):** phase 9's live pass found phase
8 half wrong. Flipping `Writes { notes: true, folders: true }` was the plan,
but the live pass showed folder writes silently fail to reach Apple — see the
checklist above and "Folder writes: deferred to M3" below. The shipped flip is
`Writes { notes: true, folders: false }`; the ordering rationale held exactly
as designed — phase 8 being gated on live proof is what caught this before it
reached a release rather than after.

## Deferred to M3 — closed, 2026-08-15, permanent negative

### Folder writes (create / rename) — CLOSED

Implemented and unit-tested, but gated off at `Capabilities::for_backend` —
`writes.folders` is `false`, and this is now permanent, not deferred.
Full detail lives in CLAUDE.md's "How Apple Notes ↔ Outlook.com works"
section; summarized here for the record:

1. `POST /me/mailFolders/{id}/childFolders` (the call needed to nest a folder
   under Notes) returns **201** but silently drops the extended property
   that would mark the folder as a Notes container. Not in Microsoft's
   documented list of folder-write endpoints (`POST /me/mailFolders`,
   `PATCH /me/mailFolders/{id}`) — this may be exactly why.
2. `PATCH /me/mailFolders/{id}` with `PR_CONTAINER_CLASS`, tried as a
   fix-after-creation, returns **`500 ErrorObjectTypeChanged`** — *"Operation
   would change object type, which is not permitted."* The container class
   is the object type, and it is immutable post-creation. This is the
   decisive measurement: it rules out any two-step create-then-classify
   approach on a `childFolders`-created object.
3. `POST /me/mailFolders` (the documented creation path) returns **201** but
   creates at mailbox root, not nested under Notes.

**M3 (2026-08-15, live, `scripts/ms_folder_move_probe.py`) ran the one avenue
this spec left untried:** create at root via `POST /me/mailFolders`, passing
`PR_CONTAINER_CLASS` at creation time (not after, per finding 2 above), then
`POST /me/mailFolders/{id}/move` under the Notes folder id. Two folders were
created this way — one with the class set, one without — both moved
successfully (2xx throughout) and both had a witness note filed into them.

**Result: neither folder ever appeared in Notes.app, confirmed on both Mac
and iPhone independently, with `outlook.live.com`'s own Notes UI as a third
observer showing both the whole time as legitimate children of `Notes`.** A
forced account resync did not surface them either — the same confirmatory
bar (absence + resync) this spec's Deferred-to-M3 section originally used
for the `childFolders` finding.

**Reading the folders back — which finding 2's `GET` 404 did not allow for
`childFolders`-created folders, but does work here (see CLAUDE.md gotcha #12's
M3 addendum) — found the mechanism directly:** `PR_CONTAINER_CLASS` came back
as **`IPF.Note`** on *both* folders, including the one whose creation payload
explicitly requested `IPF.StickyNote`. So `POST /me/mailFolders` silently
drops the class exactly like `childFolders` does. Combined with finding 2,
this closes the question completely: **no sequence of Graph API calls can
create a folder classed `IPF.StickyNote`**, which Apple's Notes sync almost
certainly requires to recognize a folder as a Notes container (the same way
`PR_MESSAGE_CLASS` gates messages). `isHidden` was checked and ruled out as
an alternative explanation — both folders read back `isHidden: false`, never
set by Jodd, and Outlook's own UI showed them unhidden throughout.

There is no further avenue to try. Folder create/rename/delete on this
backend should be treated as a closed, permanent limitation going forward,
not a milestone backlog item.

### M4 sidecars: a named property on the note, not a sidecar message

The M4 plan (as scoped elsewhere) was a sidecar *message* — a second Graph
item per note carrying Jodd-private metadata (pin, tags), mirroring the Gmail
Notes-Meta approach. Live testing 2026-08-15 makes a better option visible:
extended properties on **messages** are proven to work — that is the
exact mechanism M2 uses to set `PR_MESSAGE_CLASS` to `IPM.StickyNote` at
note-creation time. A **MAPI named property under a Jodd-owned GUID**, set on
the note's own message object, would be invisible to Apple (Apple only reads
the fields it knows about) while avoiding the sidecar message's known problem:
M1's spec already flagged a sidecar message as appearing in the user's
Notes.app as a stray note, unsolved there. A named property has no such
leak — it never becomes a note Apple can see.

Cost: one extra `$expand` per read to fetch the property, and gotcha #12
already measured that two `$expand`s over *different* property ids return
`200` with nothing at all (not an error — silently empty), so this needs its
own pagination/request rather than piggybacking on the sticky-note-class
`$expand` M2 already sends. Packing all Jodd metadata (pin, tags, whatever
M4/M5 need) into a **single JSON-valued property** keeps this to one extra
`$expand` regardless of how many sidecar fields exist, rather than one per
field.

### M3 conflict detection: prefer `changeKey`/`@odata.etag` over `lastModifiedDateTime`

Component D's conflict detection (this milestone) compares
`lastModifiedDateTime`, which has a one-second resolution — already flagged by
a reviewer as a real gap: two writes inside the same second are
indistinguishable. Exchange returns `changeKey` (and Graph mirrors it as
`@odata.etag`) on every message response — it is the concurrency token
Exchange was designed around for exactly this problem, with no
resolution-window limitation. M3 should switch conflict detection to compare
that instead of (or in addition to) the timestamp.

## Open questions (non-blocking)

- **Release blocker, not an M2 task:** `auth_ms::client_id()` has no config or
  embedded tier the way `auth::client_id()` does. A dev build reads `.env`; a
  released binary would carry no Microsoft client id and be inert.
- Can a **non-admin** user in a **different** M365 tenant consent? The door test
  covered a personal account and a tenant admin in the app's own tenant.
- Two full mailbox paginations per pull remain (`list_all_notes` then
  `list_folders` each run their own scan). Bounded and correct; worth folding.
- Should `find_ids_for_uuid` and `changes_since` be removed from `Transport`
  entirely, given neither has a caller in the workspace?
