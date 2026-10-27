# Note provenance (M1) — who wrote this note, and has a human looked at it

Status: design approved section by section in conversation 2026-10-07; this
spec awaits review. Parents:
[agent knowledge loop](2026-10-06-agent-knowledge-loop-design.md) (§3 names
"approval of changes to human-owned notes" as the gap no prior art solves) and
[LLM-WIKI-GRAPHIFY-ROADMAP.md](../../LLM-WIKI-GRAPHIFY-ROADMAP.md) (the OKF v0.2
mapping table, "The trust rows are the part worth taking even without any
export"). Lifecycle (`status`, `stale_after`) is **M2, a separate spec**.

## 1. Intent

Agents now write into Jodd (`jodd-mcp` `create_note` / `update_note` /
`remember`), and so do Jodd's own LLM workflows (Extract, URL ingest,
Summarize, Curate). Nothing records which writer produced a note, so:

- an agent reading a note cannot tell the user's own words from another
  agent's unreviewed output, and may repeat the latter as fact;
- the user cannot see what agents wrote or changed, or say "I have checked
  this one";
- an eventual OKF export (roadmap #7, LLM-WIKI #6) has no `generated` or
  `verified` to emit.

**Success:** on the desktop where agents run, every note written from now on
carries who wrote it and when, an agent sees that on every search and read,
the user sees which notes agents wrote that nobody has reviewed, and one click
marks a note reviewed — until its content changes again from anyone but the
user.

## 2. Decisions (from the 2026-10-07 conversation)

| # | Question | Decision |
|---|---|---|
| D1 | Goals | All four: agents weigh trust, the user audits agent writes, lifecycle, OKF-ready. **Split:** M1 = provenance + verification, surfaced in MCP and UI; M2 = lifecycle. OKF export is not built — M1 only names things the OKF way |
| D2 | Where it lives | **SQLite on the device that wrote, nothing synced** (the Curate D5 precedent). Sidecars were rejected: one more Gmail message per agent write is the churn that wedges Apple's IMAP sync, and iCloud has no sidecars at all |
| D3 | Granularity | **An append-only event history per note**, not "latest value" columns. A one-word human fix must not turn an agent's note into a human's, and a review must not survive a later agent edit |
| D4 | How the writer is known | **A required `actor` parameter on the Db write functions**, recorded in the same transaction as the content (approach A of three; B = callers record afterwards, C = SQLite triggers + per-connection actor, both rejected as able to miss a write silently). **Plus a backstop that does not depend on A being complete** (§3.6 rule 2b): raw-SQL writers inside `db.rs` and a stale `jodd-mcp` binary predating this feature both write without an actor, so an unrecorded `local_version` advance reads as `unreviewed` — fail-closed, never a false "reviewed" |
| D5 | A remote content change after a review | **Counts as not human → `unreviewed`.** This device cannot tell the user's iPhone from another Jodd's agent. Cost accepted: an iPhone edit needs a fresh "Mark reviewed" |
| D6 | Who can verify | **Only the user, in the app.** No MCP verify tool — an agent cannot certify itself or another agent. Curate approval is not a verify (it approves a change, not the note) |
| D7 | The agent workspace | **`Notes/__Agent__/` gets no chip and is left out of the Unreviewed folder.** Agents own it outright (agent-loop D2) and every `remember` rewrites three of its pages, so it would fill the folder with pages nobody is meant to review. Agents still get `trust` for those pages over MCP |

## 3. Data model

### 3.1 Table (migration 22)

```sql
CREATE TABLE note_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid       TEXT NOT NULL,
    account_id TEXT NOT NULL,
    kind       TEXT NOT NULL,      -- 'create' | 'edit' | 'remote' | 'move' | 'verify'
                                   -- | 'unrecorded' (rule 2b marker, R11)
    actor      TEXT,               -- §3.2; NULL only when kind = 'remote' or 'unrecorded'
    at         INTEGER NOT NULL,   -- ms since epoch
    local_version INTEGER          -- notes.local_version after this event (§3.6 rule 2b);
                                   -- NULL on rows copied from another note (§3.7),
                                   -- and on the `unrecorded` marker a Duplicate
                                   -- writes (R11, §3.7), which is not copied
);
CREATE INDEX idx_note_events_note ON note_events (account_id, uuid, at);
```

`create`, `edit` and `remote` are **content** kinds; `move` and `verify` are
not. `move` (a label change only) exists so that every `local_version` advance
has a row (rule 2b); `verify` records the version the user saw. Events are ordered by `id` alone, never by `at`
(final review M9): coalescing (§3.4) can make two `at` values equal and a wall clock that
steps back would put a newer agent edit before an older `verify` — failing open.
Coalescing only ever touches the newest row and a copy inserts in order, so id
order is the true order.

`unrecorded` is a marker, not a write anyone performs (R11): when any
non-`verify` event is recorded, the note already has events, and its
`local_version` **before** this write is past the highest `local_version` any
event recorded, a row `kind = 'unrecorded'`, actor NULL, `local_version` = that
pre-write version is inserted first, in the same transaction. Each writer reads
the pre-write version inside its own transaction and passes it to
`record_event_conn`. As an unknown kind the marker counts as non-human content
(rule 2a), so the gap survives every later event until a `verify`. One marker
is written outside this rule: a Duplicate of a source with an unpinned gap
(§3.7) pins it on the copy with `local_version` NULL, because the source's
versions, which the gap is measured by, do not belong to the copy — so `kind =
'unrecorded' AND local_version IS NULL` does **not** mean "copied row", and a
marker copied from a source whose gap was already pinned looks the same: the
row's shape cannot tell a written marker from a copied one.

`note_events` is **not** in `ACCOUNT_ID_TABLES`: that list is "every table whose PRIMARY KEY contains `account_id`" and migration 19 iterates it, so adding a table created by migration 22 would make every fresh install fail at migration 19. It follows `curate_proposals`' precedent (keyed by its own id); account removal deletes its rows explicitly. A future account-id rewrite must include it by hand. Nothing derived is stored — every
answer below is computed from the rows (the `edges` / gotcha #4 doctrine:
derive, don't migrate), so changing a tier rule is a query change.

### 3.2 Actor

A Rust enum whose `Display` is the OKF v0.2 actor string (SPEC §7):

| Writer | String | Source of the value |
|---|---|---|
| The user, in the app | `human:owner` | Tauri commands behind the editor and note actions — **except** a `save_note` that carries an `ai_result_id` (below) |
| An agent via `jodd-mcp` | `<client>/<version>`, e.g. `claude-code/2.1.0` | `peer_info().client_info` `name` + `version` (name is read today at `jodd-mcp/src/main.rs:412`; version is new). Missing → `agent/unknown` |
| A Jodd LLM workflow | `jodd-<workflow>/<model id>`, e.g. `jodd-extract/claude-opus-5-5` | the resolved provider of that run. When the frontend saves AI output through `save_note` (the auto-link "Related: [[…]]" line, `LessonExtractModal.svelte:232` and `NoteContextMenu.svelte:425`) the `ai_result_id` it already passes resolves to the workflow and model — so `ai_policy`'s result registry must record both when a result is issued |
| Jodd's own automation | `process:jodd-<name>`, e.g. `process:jodd-conflict-copy` | fixed at the call site |

`human:owner` and not the account email: Jodd has one user, and an OKF export
must not leak an address. A malformed actor read back is shown as-is, never an
error (OKF: consumers tolerate unknown values).

### 3.3 Which writes record what

| Db function | Event | Notes |
|---|---|---|
| `insert_local_new` | `create` by the caller's actor | |
| `apply_local_edit`, `apply_local_edit_versioned` | `edit` by the caller's actor when title or body changed (§3.5); otherwise `move` | the shared `apply_local_edit_inner` bumps `local_version` even for a label-only change |
| `move_notes_batch` | `move` by the caller's actor, per note | bumps `local_version` |
| `rewrite_tag_in_bodies` (tag rename), `rewrite_links_to_renamed_note_conn` (wikilink rewrite after a rename) | `edit` by the caller's actor, per rewritten note | raw `UPDATE`s inside `db.rs` today; the second also runs when an **agent** renames a note through `update_note`, so it must not assume `human:` |
| `upsert_from_remote` | `remote`, actor NULL | **only when an existing row's content changed** (§3.5). A first insert records nothing — the first index of an account would otherwise flag every note |
| new `Db::verify_note` | `verify` by `human:owner` | refuses a stale view (§5.2) |
| `bump_local_version` | nothing | its two callers (`reconcile.rs:236`, `sync_worker.rs:589`) run after a remote replacement that `upsert_from_remote` already recorded; rule 2b then reads the bump as `unreviewed`, which is what D5 wants anyway |
| `mark_pushed` | nothing | writes the pushed body back but never advances `local_version` |
| `migrate_tags_to_body` | nothing | one-time migration that has already run everywhere; rule 2b covers it if it ever runs again |

Service layers between a command and the Db (`note_mutations`, `agent_memory`,
`jodd-mcp/src/write.rs`, `llm/*`, `ingest/run.rs`, `capture_commands.rs`,
`curate/apply.rs`) gain the same parameter and pass it through; none chooses
an actor on its caller's behalf. `reconcile.rs`'s keep-both conflict copy is
`process:jodd-conflict-copy`, so a conflict copy surfaces as `unreviewed` —
which is what a conflict should do. Tests use `Actor::test()`.

### 3.4 Coalescing

The editor saves often. When the newest event of a note (by `id`) has the same actor and
the same kind, is a `create` or `edit`, and is under
10 minutes old, its `at` **and `local_version`** are updated instead of
inserting a row — updating `at` alone would leave rule 2b reading every
coalesced save as an unrecorded write. `remote` never coalesces: it never
advances `local_version`, so a coalesced remote row would hide a second remote
change from Mark reviewed's stale check (§5.2). `move` and `verify` are never
coalesced either. The table grows by editing sessions, not keystrokes.

### 3.5 "Content changed"

First compare the stored and incoming title and `body_html` exactly — a full
pull re-upserts every note of an account every round, and almost all of them
are byte-identical, so that is the fast path. Only when they differ, compare
**title plus normalized text** — `agent_memory::text::html_to_text` with
whitespace runs collapsed — and record `remote` only if *that* differs **and**
the backend's own change token moved: the stored `remote_version` differs from
the incoming one, or either is NULL (R13). A newer decoder re-reads every note
of an account with different text but the same `remote_version`; that is not a
change anyone made, and without this check every full pull after such an
upgrade would flag the whole account. A backend does not hand back
the bytes Jodd pushed (iCloud rebuilds HTML from its CRDT in `icloud/doc.rs`),
and a byte compare would turn every push's own echo into a `remote` event,
flipping the user's notes to `unreviewed` minutes after they wrote them. Same
principle as gotcha #29: decoded-content equality, never raw bytes. Whether
the projection is stable across each backend's round trip is **proved by the
echo tests in §7, not assumed.**

Accepted blind spot: a remote change to formatting only (bold, a heading
level) is not a `remote` event. Trust is about who wrote the words.

### 3.6 Tiers

Computed per note from its events, first rule that matches:

| # | Tier | Rule |
|---|---|---|
| 1 | `not_recorded` | no events at all |
| 2a | `unreviewed` | a content event that is `remote`, by a non-`human:` actor, or of an unknown kind comes after the latest `verify` (or there is no `verify`) |
| 2b | `unreviewed` | `notes.local_version` is greater than the highest `local_version` any of its events recorded — something wrote the note without an event |
| 3 | `human_reviewed` | a `verify` exists |
| 4 | `human_written` | a `create` is recorded and every content event is by `human:` |
| 5 | `not_recorded` | anything else — in practice a pre-existing note only the user has edited since tracking began: its last edit is known, its origin is not |

The order matters: a pre-existing note the user marks reviewed has no content
events at all and must land on rule 3, not fall through to rule 5.

Plus values for display: **`by` / `at`** = the latest content event
(OKF `generated`; `by` is null for `remote` and for rule 2b), **`created_by`**
= the `create` event's actor, if recorded, and (R14) **`event_id`** = the id of
the latest **non-human** content event after the last `verify` — the thing that
is unreviewed, which Mark reviewed sends back and certifies (§5.2) — with
**`unreviewed_by` / `unreviewed_at`** its actor and time (`unreviewed_by` null
for `remote` and `unrecorded`; both null under rule 2b). All three are null when
nothing is unreviewed. `unrecorded_change` is true under rule 2b or when that
event is an `unrecorded` marker. A user who fixes a typo in an Extract note
therefore still sees the chip name the workflow, not `human:owner`.

**Tiers are computed in one SQL statement per batch** — one `GROUP BY uuid`
over the notes in hand (a page of search results, a smart-folder listing) —
never a query per note: the large test account has 6,655 notes. Existing notes are **not backfilled** —
not even `Notes/__Agent__` pages, whose `remember` lines already carry their
actor line by line.

### 3.7 Lifecycle of the rows

| What happens to the note | `note_events` | Why |
|---|---|---|
| uuid rekeyed at CREATE (gotcha #16, `rekey_note_uuid`) | added to its table loop: delete rows at `new_uuid`, move `old_uuid`'s | or an agent-created note on Microsoft/iCloud loses its history on its first push |
| `Db::delete` (the user deleted it, trash confirmed) | deleted with the note | as `note_tags` |
| **Duplicate** (`NoteContextMenu.svelte:248`) | the copy **inherits the source's events**, with `local_version` NULL, then — only when the source has an unpinned rule-2b gap (`provenance::unrecorded_gap`) — an `unrecorded` marker, also `local_version` NULL (R11; the copied rows lose the versions that gap is measured by), followed by its own `create` by `human:owner` — or its own **`edit`** when the source has no `create` (a pre-existing or rule-5 note), so the copy stays `not_recorded` like its source (R12). `duplicate_of` is resolved through `note_uuid_aliases` first (gotcha #16), and the copy and the insert are one transaction (`Db::insert_duplicate`) | otherwise "Duplicate" launders an unreviewed agent note — or a note of unknown origin — into `human_written`. `save_note` gains an optional `duplicate_of` uuid, because Rust cannot otherwise tell a duplicate from a new note. NULL keeps the copied rows out of rule 2b, whose numbers belong to the source |
| account removed | deleted by `account_id` | |
| **`prune_clean` / `prune_clean_in_label`** | **kept** | a prune is a snapshot a push can land inside (gotcha #35), and a Gmail folder move made on another device shows up as "gone from one label, back in another" with the same uuid. Orphaned rows of a really-gone note cost nothing; deleting them would erase history on every remote move |

Whoever next writes an account-id rewrite (as migration 19 did) must carry `note_events` explicitly.

## 4. Agents (`jodd-mcp`)

- `search_notes`, `note_connections` and `read_note` results gain
  `"trust": { "tier": …, "by": …, "at": "<ISO 8601 with offset>" }`, plus
  `"unreviewed_by"` — present only when `tier` is `unreviewed` and the
  unreviewed event has an actor (R14): `by` is the latest writer of any kind, so
  once the user fixes a typo in an agent note `by` says `human:owner` while the
  words an agent should weigh are still the agent's.
  `read_note` also returns `created_by`. The search response stays under the
  existing size tests (`search_notes_response_stays_under_the_observed_failure_threshold`
  and the `note_connections` twin) — measured by those tests, not estimated.
- Server `instructions` gain one sentence: *"A note's `trust.tier` says who
  wrote it: prefer `human_written` and `human_reviewed` over `unreviewed`, and
  say so when an answer rests on an unreviewed note."*
- Every `jodd-mcp` write passes `<client>/<version>` (§3.2).
- No verify tool (D6). OKF's *machine-confirmed* tier therefore has no source
  in M1; it needs a deterministic checker that does not exist yet.

## 5. The user (app)

1. **Editor breadcrumb chip**, beside the Suggest-folder chip, **only for
   `unreviewed`** and never under `Notes/__Agent__/` (D7):
   `🤖 claude-code · 2 h ago · Mark reviewed` (from `unreviewed_by` /
   `unreviewed_at`, R14); `🔄 changed elsewhere` for a
   `remote` event; `✎ changed without a record` for rule 2b or an `unrecorded`
   marker. No chip for any
   other tier — thousands of `not_recorded` notes must look exactly as they do
   today.
2. **Mark reviewed** sends the `event_id` the chip was showing.
   `Db::verify_note` takes the `event_id` the chip was showing and the `local_version` of the body the editor rendered (§10, PR #139 — the chip's own `note_local_version` is only the fallback) and refuses as **stale** if (a) a **non-human** content event after the last `verify` that the chip did not show exists — one with an id greater than `event_id`, **or the event `event_id` itself if it has since been coalesced forward** (its `local_version` is now greater than the `note_local_version` shown; coalescing keeps the id), or (b) rule 2b holds *and* `notes.local_version` differs from the `note_local_version` shown — an unrecorded change the user did not see. The user's own recorded edits never make a verify stale. On success the `verify` event records the note's current `local_version`, which is what clears rule 2b; refusing whenever 2b held would leave such a note unverifiable forever. On success it writes
   the `verify` event synchronously; the UI is optimistic with rollback
   (local-first doctrine) — not the await-then-mutate shape of CLAUDE.md's
   open defects D5/D6: the chip **and, in the Unreviewed view, the note's row**
   leave before the IPC, and both come back (the row at its old index) on stale
   or failure. On stale, the chip refreshes to the new writer.
3. **Smart folder "Unreviewed"**, beside Orphaned and Stale, **fully virtual**
   like them (no `folders` row — roadmap #2's locked reasoning): this
   account's `unreviewed` notes outside `Notes/__Agent__/`, newest content
   event first.
4. **Android** renders the same chip and folder. They show only what was
   written on that device — Extract or ingest run there with an HTTP provider,
   say — and never what an agent did on the desktop (D2). Not hidden.

Any state the sync worker changes on its own and the UI must reflect (a
`remote` event flipping a tier) travels the existing `remote-changed` route —
**traced to the pixel in a test, per gotcha #6**, not assumed from the event
name. An **in-app** AI write has no such route (R15): once each of these
resolves, the frontend fires `refreshUnreviewed([accountId])` without awaiting
it — Extract (new note and append), URL ingest, `run_llm_workflow` /
`append_llm_workflow_note`, the auto-link "Related" `save_note`
(`LessonExtractModal`, `NoteContextMenu`), `apply_wiki_link_appends`,
`apply_action_items`, Organize's `curate_apply` — and Duplicate, whose copy
inherits an unreviewed history.

## 6. Error handling

- The event is written in the content's transaction: if it fails, the write
  fails. There is no state where content changed and history did not — the
  reason for D4.
- Mark reviewed failing rolls the chip back and says so; refused as stale,
  it shows the newer writer instead and verifies nothing.
- Unknown `kind` or a malformed actor read back: shown as-is, tier computed
  conservatively (an unknown kind counts as non-human content, §3.6 rule 2).

## 7. Testing

**`db.rs` unit tests**
- each write function in §3.3 records the right kind, actor and
  `local_version`; a label-only `apply_local_edit` records `move`, not
  `edit`;
- **every function that advances `local_version` either records an event or
  is listed in §3.3 as covered by rule 2b** — a test that greps `db.rs` for
  `local_version = local_version + 1` and fails on an unlisted function, so
  the next raw writer cannot slip past the way five did before this review;
- rule 2b: a raw `UPDATE … local_version + 1` with no event → `unreviewed`;
- coalescing: same actor + kind within 10 min updates `at`; a different
  actor, kind, or > 10 min inserts; `verify` never coalesces;
- tiers: table-driven over event sequences, including every row of §3.6,
  the pre-existing-note-then-human-edit case (rule 5), the
  pre-existing-note-then-verify case (rule 3), and two events with equal
  `at` ordered by `id`;
- tier batch query: one statement for N notes (assert the statement count),
  same answers as the per-note rules;
- `verify_note`: refused when a non-human content event the chip did not show
  exists (newer than `event_id`, or `event_id` itself coalesced forward), and
  when rule 2b holds with a `local_version` other than the one shown; accepted
  after the user's own recorded edits and once an unrecorded change has been
  seen;
- duplicate: copy of an unreviewed note is `unreviewed`, copy of a reviewed
  one is `human_reviewed`, copied rows carry NULL `local_version`;
- `save_note` with an `ai_result_id` records the workflow actor, without one
  `human:owner`;
- `rekey_note_uuid` carries events, including the poll-vs-push race (rows
  already at `new_uuid`);
- `Db::delete` and account removal drop events; both prunes keep them;
- `upsert_from_remote`: first insert → nothing; byte-identical → nothing
  and no text projection computed; HTML differs but text equal → nothing;
  text differs → one `remote`.

**Echo tests, one per backend** (Gmail MIME, iCloud document, Microsoft,
SSH, LocalFs): a human edit → push → pull through each backend's real
encode/decode on fixtures → **no `remote` event**. Save → DB → save, not
the wire alone (gotcha #16's lesson).

**`jodd-mcp`**: `trust` present on all three tools; size-threshold tests still
pass; `instructions` contains the sentence; a write records
`<client>/<version>`.

**Frontend (vitest)**: chip only for `unreviewed` and never under
`Notes/__Agent__/`; Mark reviewed optimistic + rollback, and the stale
refusal; the Unreviewed smart folder excludes `__Agent__`; Duplicate passes
`duplicate_of`; a `remote-changed` event re-renders the
chip (mounted component, gotcha #28).

**Live pass (desktop)**
1. claude-code creates a note → chip shows its name.
2. Mark reviewed → chip gone; tier `human_reviewed` in `read_note`.
3. claude-code edits it → chip back.
4. Edit it on the iPhone, wait for sync → chip back as "changed elsewhere".
5. **Edit a note in Jodd, wait for push + pull → still no chip.** The echo
   test on the real thing.
6. Open an agent note, have claude-code edit it, then click Mark reviewed
   without reloading → refused, chip shows the new edit.
7. `remember` something → no chip on any `__Agent__` page, nothing new in
   Unreviewed.

## 8. Out of scope

Lifecycle (`status`, `stale_after`) — M2. OKF export. Syncing provenance
across devices. Per-line provenance inside a note. A machine-confirmed tier.
Backfilling existing notes. Letting agents filter search by tier (add when an
agent is seen to need it).

## 9. Open questions

1. Is 10 minutes the right coalescing window? Measure row growth on the
   user's machine for a week after shipping.
2. Does `rmcp` expose a non-empty `client_info.version` for Claude Code,
   Claude Desktop and thClaws? Probe first; `agent/unknown` covers a miss.

## 10. As built

Shipped on `claude/note-provenance-spec` (Tasks 1-8). What differs from, or
was learned beyond, the design above:

**Spec corrections made in Task 1** (the original text would have failed):

1. `note_events` is **not** in `ACCOUNT_ID_TABLES`. Putting it there made
   migration 19's account-id rewrite crash on a table that migration 22 only
   creates later (§3.1).
2. `verify_note` refusing whenever rule 2b held left such a note unverifiable
   forever; §5.2 now refuses only if `local_version` differs from the one the
   chip showed, and a successful `verify` records the current `local_version`,
   which is what clears 2b.
3. The staleness check must also catch the shown event being **coalesced
   forward** (same id, higher `local_version`), not only a newer event id (§5.2).

**Controller rulings that amended the spec afterwards:**

- R4: only the exact string `human:owner` is human; `Actor::segment()` also
  replaces `:` so no agent or workflow string can carry a `human:` prefix.
- R5: the §7 `verify_note` test bullet was rewritten to match corrected §5.2.
- R6: every transaction that reads before it writes (`insert_local_new`,
  `apply_local_edit_inner`, `upsert_from_remote`, `verify_note`) is
  `IMMEDIATE`; a deferred read-then-write cannot wait on `busy_timeout`.
- R7: `remote` events never coalesce (§3.4 now says coalescing applies to
  `create` and `edit` only) - otherwise a second remote change folded into the
  event the chip showed would let `verify_note` certify it unseen.
- R8: the `verify_note` Tauri command is `async`, like `set_pin` / `delete_note`;
  a sync command would hold the main thread for up to 5 s behind a jodd-mcp write.

**Final-review fix wave (rulings R11-R15):**

- R11: a write that left no event is pinned by an `unrecorded` marker row at
  the pre-write `local_version` before the next non-`verify` event (§3.1), so a
  later move or human edit can no longer erase rule 2b; Mark reviewed clears it.
- R14: `event_id` names the latest non-human content event after the last
  `verify`, and `unreviewed_by` / `unreviewed_at` name its writer for the chip
  and for agents (§3.6, §4, §5.1); `by` / `at` stay OKF `generated`. Measured:
  the 50-row `search_notes` fixture went from 39,751 to 41,551 chars (about 36
  per row, all rows unreviewed) and `note_connections` is 41,579, both under the
  unchanged 52,000 gate and the 900-chars-per-row bound.
- R13: `upsert_from_remote` records `remote` only when the text differs AND the
  stored and incoming `remote_version` differ (or either is NULL), so a decoder
  change does not flag every note on the next full pull (§3.5).
- R15: the in-app AI writes listed in §5 refresh the unreviewed set themselves
  (fire-and-forget), so their chip appears at once instead of at the next focus
  or 10-minute poll; the Unreviewed row leaves with the chip before the IPC and
  is restored at its index on stale or failure (§5.2). Tested by mounting `App`
  and firing its real `remote-changed` listener, plus a source guard over the
  call sites.
- R12: Duplicate resolves `duplicate_of` through its alias, copies and inserts
  in one transaction, and records the copy's own event as `edit` when the
  source has no `create`, so an unknown origin stays `not_recorded` (§3.7).
  Follow-up to PR #134: a source with an **unpinned** rule-2b gap gives the
  copy an `unrecorded` marker (actor and `local_version` NULL) before its own
  event, since the copied rows' NULL versions cannot show the gap; the copy
  stays `unreviewed` until verified instead of reading as `human_written`.
- Events are ordered by `id`, not `(at, id)`; coalescing finds the newest row by
  `id` and treats a negative clock difference by its magnitude (§3.1, §3.4).

**Measured:**

- Echo tests (Task 5): all five backends (Gmail, Microsoft, LocalFs, iCloud,
  Ssh) passed on the first run, so `normalized_text` was **not** changed; a
  push's own echo never produces a `remote` event.
- Search size (Task 7): a 50-row `search_notes` response grew from 35,601 to
  39,751 chars (about 83 per row), under the 52,000 gate. `at` was **kept** in
  search and connections (`trust` is `{tier, by, at}`).

**Gate side effect:** `tests/browser/teaching-replay.json` pins sha256 of
`llm/meeting.rs` and `llm/policy.rs` (and a `prompt_version` digest that
includes `meeting.rs`), and this branch edited both. It was regenerated with
the offline procedure in `docs/TEACHING-DEMO.md`; every outcome in the replay
was unchanged. See commit `test(meeting): regenerate the teaching replay ...`.

**Needed for a live run:** copy `.env` into the worktree before
`npm run tauri dev`, or the OAuth client id ships empty.

**Found in the live pass (2026-10-08): a review could certify a body not on
screen.** A stale refusal put the agent's event on the chip at once, but the
editor kept the old body until the next full listing (~48 s: `loadNotes` awaits
the remote `list_notes`), and a second click in that window verified — the
stale check was satisfied by the event the chip now showed. Fixed in two layers:

- *The editor.* Whenever the chip's trust for the open note changes (event id
  or version), NoteEditor re-reads the note from SQLite (`get_cached_note`, a
  local read through the rekey alias) and hands it to `refreshSelection`, the
  update half of `reconcileSelection`, so the render decision and the
  held-back banner apply unchanged. Mark reviewed is disabled until that read
  lands and while a change is held back behind unsaved typing.
- *The verify.* `markReviewed` sends the `local_version` the editor's text
  derives from (`saveBaseVersions`) instead of the chip's, so `verify_note`'s
  existing rule refuses the shown event when it is newer than what was
  rendered, even if the UI races.

**Review of that fix (PR #139): a late listing could still regress the
selection, for local agent writes too.** `list_notes` returned the remote's
body stamped with the row's *current* `local_version`, so an unpushed agent
append (a Gmail listing takes ~48 s; a push-blocked note never pushes) reached
the frontend as {old body, new version}. A listing landing after the re-read
put the old body back with a version that satisfied `verify_note`, and a save
made over it passed the compare-and-swap and overwrote the append. Two more
layers:

- *The listing.* `overlay_cached_row` (lib.rs) — used by `list_notes`,
  `list_notes_in_folder` and `refetch_note` — gives a `dirty` row's title and
  body to the fetched note along with its version. A dirty row's content is
  newer than the remote's by construction (a remote change to it becomes a
  conflict), so a body and the version beside it now always describe the same
  text.
- *The editor.* The re-read key includes the selection's body and version, so
  any body change under a shown chip re-reads SQLite before Mark reviewed comes
  back. This also covers the remaining ordering for `remote` events, which do
  not move `local_version`: a listing that predates a pull and lands after it
  is corrected from the row before the button is enabled.

**Follow-up: the re-read skipped an agent note that was not pushed yet.**
`refreshSelection` returned early on a selection without an `id`, meant for a
brand-new note that lives only in memory. But `id` is the remote's, and an
agent's `create_note`/`remember` is in SQLite with none until the worker pushes
it, so its read was dropped: the old body stayed on screen and Mark reviewed was
refused as stale on every click until the push landed (safe, but confusing). The
gate is now the read itself: `refreshSelection` only ever receives a row
`get_cached_note` returned, and a note that lives only in memory has no row.
Pinned by mounting NoteEditor with an id-less note in
`noteEditorReviewCurrency.test.ts`.

### Live pass - pending (run with the user)

Not run by the implementer: it needs the user, a real account, the real DB
(keychain prompt) and a reinstalled `jodd-mcp` (`cargo build -p jodd-mcp
--release`, copy to `~/.local/bin`, `xattr -c` it; warn about the keychain
prompt before the first run).

| # | Step | Expected | Result |
|---|---|---|---|
| 1 | claude-code creates a note | chip shows `claude-code` | |
| 2 | Mark reviewed | chip gone; `read_note` reports `human_reviewed` | |
| 3 | claude-code edits that note | chip comes back | |
| 4 | Edit it on the iPhone, wait for sync | `changed elsewhere` chip | |
| 5 | Edit a note in Jodd, wait for push + pull | **still no chip** | |
| 6 | Open an agent note, have claude-code edit it, click Mark reviewed without reloading | refused; chip shows the new edit | |
| 7 | `remember` something | no chip on any `__Agent__` page; nothing new in Unreviewed | |
