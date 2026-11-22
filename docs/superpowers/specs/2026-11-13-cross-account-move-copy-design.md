# Move and copy a note to another account (2026-11-13)

**Status: Approved 2026-11-15, built the same day.** The owner took the
proposal on all four questions in §10: Copy to lists the note's own account
too, a move out of Outlook always confirms, there is no Undo in this version,
and the copy keeps its title. §11 records where the build departs from the
text above, and why. The live pass in §8 is still owed.

**Roadmap:** `CLAUDE.md` roadmap #6, "Cross-account move — UI ready, backend
is a stub". This spec also adds **Copy**, at the owner's request ("ย้ายและ
ก๊อปปี้โน้ตข้ามบัญชี").

**Why now:** `2026-11-13-all-accounts-scope-design.md` made one list show
notes from every account. A user who sees two accounts' notes side by side
will want to put one where the other is. Today the Move to menu lists every
account and disables all but the note's own, with the tooltip "Cross-account
move not supported yet" (`MoveToTargets.svelte`, `accountRow`).

## 1. What already exists, and what is missing

**Most of the work exists, in backup restore.** A restore can already write
a note into a *different* account, across backends, and it states what
did not survive. `backup/restore/` holds the pieces:

| Piece | Where | What it does |
|---|---|---|
| `note_doc` | `backup/bundle.rs` | Turns a live `CachedNote` into a `NoteDoc`: title, body, tags, citations, pin, folder, created date, attachment refs, provenance history, `partial` (render loss) |
| `TargetTraits` | `restore/plan.rs` | What the target backend can hold: `holds_attachments` (Gmail, LocalFs), `inline_tags` (iCloud), `caps.writes` |
| `finish_body` | `restore/plan.rs` | Attachments become `[Attachment not restored: NAME]` where the target cannot hold them. Tags become a `Keywords:` line on iCloud (gotchas #37, #38) |
| The `cid:` refusal (R9) | `restore/plan.rs` | Refuses a body that still names an attachment the target cannot hold; such a push would be refused forever |
| `Loss` | `restore/transform.rs` | `FolderFlattened`, `Attachment`, `Pin`, `TagsAsKeywords`, `Partial` |
| The write | `restore/apply.rs` | `mint_uuid_for(target kind)`, attachment rows first, then one `insert_restored` (a `Dirty` row, history carried, pin in the insert). SQLite only; the worker pushes |

Restore has been tested into Microsoft (folders flattened, attachments
dropped) and Gmail → Gmail (attachments with the same content id, pin kept).

**What restore does not have**, and a move needs:

1. **A source that is a live note**, not a zip. `note_doc` already reads a
   live row; restore just never feeds it straight back in.
2. **The second half of a move:** deleting the original, and only once the
   copy is safely on the target's server.
3. **A UI** that enables other accounts in Move to, and a Copy to.

## 2. Decisions

### 2.1 Two actions, one engine

- **Copy to…** makes a new note in the chosen account and folder. The
  original is untouched.
- **Move to…** in *another* account is a Copy followed by the original's
  deletion (§4). Move to a folder in the *same* account keeps working as it
  does today (`move_notes_batch`, a relabel). Nothing about it changes.

Both go through one Rust module, `transfer.rs`. It takes the source rows
from SQLite, builds each `NoteDoc` with `note_doc`, plans each note with the
restore rules (§3), and writes with the restore write. The shared restore
functions move from `pub(super)` to `pub(crate)`; their behaviour does not
change, so the restore tests keep guarding them.

### 2.2 What the copy carries

| Part | Rule |
|---|---|
| Title | Unchanged (open question 4) |
| Body | `finish_body` for the target, exactly as a restore writes it |
| Folder | The folder the user picked. It already exists, because the picker lists only existing folders (`list_folders`), so nothing is flattened or created |
| Created date | Kept (`x_mail_created_date`), as restore keeps it |
| Pin | Kept where the target can write sidecars, else a `Pin` loss |
| Tags | In the body: carried as text. On iCloud, a `Keywords:` line (`TagsAsKeywords` loss) |
| Attachments | Copied into Gmail and LocalFs targets (`attachment_bytes` from the source, `upsert_attachment` into the target). Placeholders elsewhere (`Attachment` loss) |
| Provenance | The source's history is carried, so an AI-written note stays Unreviewed. Duplicate does the same today (`duplicateOf`). One event by `Actor::Human` records the copy or move; its kind is settled against `provenance::derive`'s rules when built, so it cannot change a note's trust |
| Identity | A **new** uuid from `mint_uuid_for(target kind)`, as restore and Duplicate mint. Keeping the source's uuid could put one Apple UUID on two live notes, in two accounts Apple Notes may both sync. Nothing measured says Apple tolerates that |

### 2.3 What is lost, and when the user is told

One new loss, beside restore's five:

- **`Links`:** a `[[wikilink]]` resolves within one account only
  (`Db::resolve_note_link` filters on `account_id`). So links between the
  moved note and notes left behind stop resolving, in both directions. Raised
  only when the note has such links, from the `edges` table (`mentions`)
  in both directions.

**The confirm sheet shows only when there is a loss** (or for a move out of
Outlook, §4.4). It lists each loss in plain words, per note for a batch, with
**Copy** / **Move** and **Cancel**. A transfer with no loss happens on the
click, like a move within an account.

## 3. Refusals: checked per note before anything is written

| Case | Copy | Move | Why |
|---|---|---|---|
| Target cannot write notes (`caps.writes.notes`) | refused | refused | No target. Today every backend can; the row is disabled with the reason if one ever cannot |
| Source cannot relocate (`caps.writes.relocate`) | — | refused | The original cannot be deleted |
| Source in `conflict` | allowed | refused | Which side moves is the user's decision; resolve first |
| Source not yet saved (`tmp:` uuid, no `id`) | disabled | disabled | Nothing to copy yet, as Duplicate today |
| Source renders partially (`render_loss`, e.g. iCloud `DoesNotRoundTrip`) | allowed, `Partial` loss | **refused** | A copy states what it could not read. Deleting the original would destroy the only copy of that part |
| A `cid:` the target cannot hold survives `finish_body` (R9) | refused | refused | Its push would be refused forever |
| Target folder waiting to be deleted (R15) | refused | refused | The worker would delete the folder with the note inside |

A refusal names the note and the reason. In a batch, the other notes go
ahead.

## 4. The move: copy first, delete the original only when the copy is safe

### 4.1 Phase 1: one SQLite transaction, on the click

1. Write the target row (`Dirty`), its attachments and its history, as in
   §2.
2. Mark the source `deleted_pending`. That one existing state already hides
   the note everywhere it should: list, search, tags, Ask, MCP, the
   badges.
3. Insert a row in a new table, `note_transfers`:
   - `source_account, source_uuid`
   - `target_account, target_uuid`
   - `source_sha`: the body hash that was copied
   - `source_prev_state`: the source's `sync_state` before step 2, to restore
     it if the move is called off
   - `state`: `awaiting_target` → `target_safe` → removed
   - `created_at`

All three steps are in one transaction, on the same database, so a crash
leaves either all of them or none.

### 4.2 Phase 2: the worker deletes only behind a safe copy

- **The deletion waits.** `list_deleted_pending` skips a source with an open
  transfer in `awaiting_target`. The row still counts in
  `has_pending_pushes`: an account with a move in flight is not empty
  (gotcha #2).
- **The copy is safe** once its row is `clean` with a remote id: the
  target's server holds it. The worker sets `target_safe`. Then the source
  deletion goes through the usual `push_one_deletion`: Gmail, LocalFs, SSH
  and iCloud trash it, so it is in that account's Recently Deleted.
- **The base check before deleting** (gotcha #40's rule, for a delete). If
  the source's remote copy changed after phase 1 (an edit from Apple Notes
  arrived), the original is not deleted. The source returns to its live
  state, and both notes stay (keep-both). The user is told:
  "*title* changed in *account* while it was being moved, so both copies
  were kept."
- **Identity can change at create** (gotcha #16). `target_uuid` is read
  through `resolve_note_uuid`, so an iCloud or Microsoft rekey does not lose
  the transfer.
- **A Gmail create is not idempotent** (gotcha #39). The target row is an
  ordinary new dirty row, so `prepare_gmail_create` already covers it.

### 4.3 When the move is called off

The source returns to `source_prev_state` (unhidden), the transfer row goes,
and the user is told. The target copy stays, because deleting a note the user
can see is never automatic (gotcha #14). This happens when:

- **The target push is refused for good** (`push_blocked_reason`). The
  original is back where it was. The copy stays with its reason shown, as any
  refused note.
- **The target account is removed** before the copy is safe.
  `remove_account` calls off its open transfers first.
- **The source account is removed.** Its transfers are dropped with its
  rows. The copy is already a normal note in the target.

Each of these is a change the worker makes on its own. So each one emits
`remote-changed` with both account ids, and the test follows it to the
list (gotcha #6: "trace the path to the pixel").

### 4.4 Outlook has no Recently Deleted

Microsoft's delete is a hard delete (`has_trash = false`). A move *out of*
Outlook always shows the confirm sheet, with one line: "The original will be
deleted from Outlook for good once the copy reaches *target*. Outlook has no
Recently Deleted." Moving *into* Outlook needs no warning.

## 5. Local-first and the UI

- **One IPC per action**: `copy_notes(source: [(account, uuid)], target_account,
  target_folder)` and `move_notes_to_account(...)`. Both are SQLite only, and
  return the target uuids, the refusals and the losses. A **preview**
  command with the same arguments returns the plan without writing; the
  confirm sheet reads it.
- **Optimistic, in one turn** (gotcha #28). On Move, the source rows leave
  the list and the target rows enter it in the same step. If the source was
  open in the editor, the editor switches to the target note, and the
  current account follows it (all-accounts spec §3.4). On an IPC error, both
  roll back.
- **Status while a move is in flight:** the copy shows the waiting ring like
  any unsent note. There is no new state to learn.
- **No Undo** in this version (open question 3). The original is in the
  source's Recently Deleted (except Outlook, §4.4), and Move to back is one
  more move.

### 5.1 Where the actions are

| Surface | Change |
|---|---|
| `MoveToTargets.svelte` (desktop cascade and the phone/⋯ list) | Other accounts are enabled. Each row says why when it is disabled (§3) |
| `NoteContextMenu.svelte`, `NoteMoreMenu.svelte`, the phone note sheet | New **Copy to** row beside Move to, using the same `MoveToTargets` |
| Batch selection | Copy to and Move to take notes from several accounts. The old "Selected notes span multiple accounts — not yet supported" goes |

Drag and drop is not part of this. NoteList still sets an
`application/x-jodd-note` payload, but nothing in the sidebar reads it any
more. Bringing back dropping on a folder is a separate change.

## 6. What does not change

- **Move within an account** (`move_notes_batch`, `moveNoteOptimistic`).
- **Duplicate** (same account, " copy" suffix).
- **Restore.** Only its helpers' visibility changes; its tests stay as they
  are.
- **The save path, sync worker states and merge rules.** The worker only
  learns to hold a deletion behind an open transfer, and to settle the
  transfer.
- **Ask and MCP scope** (gotcha #33).

## 7. Edge cases

- **Same note, two transfers.** A note with an open transfer cannot be
  moved again until it settles. It is hidden in the source, and the copy is a
  different note.
- **Copy then edit.** The copy is an ordinary note from the first moment.
  Edits go to it.
- **Signed-out or blocked target.** Phase 1 still runs; it is local. The
  copy waits like any unsent note. The original stays hidden but safe, and
  sign-in or unblocking lets the move finish.
- **iCloud target.** The body goes through the same create path a restore
  into iCloud uses. A new note has no remote document, so none of
  `compose.rs`'s six per-note refusals can apply to it (they are about what
  the remote already holds, gotcha #24).
- **Microsoft target.** The title-line rules (gotchas #11, #17) belong to
  the push, which already handles a restored note.

## 8. Tests

**Rust (`transfer.rs`, `sync_worker.rs`, `db.rs`)**

- Copy writes one dirty target row. It carries history plus `copy`, the pin
  (Gmail → Gmail), and attachments with the same content id. The source row
  is unchanged.
- Copy into Microsoft and into iCloud: attachments become placeholders,
  tags a `Keywords:` line on iCloud, and the losses say so. Restore's
  `microsoft_target_flattens_folders_and_drops_attachments` is the model.
- `Links` loss raised only when edges cross the boundary, in either
  direction.
- Each refusal in §3, as a table test. A refused note writes nothing,
  including attachment rows.
- **Move phase 1 is atomic:** with a forced failure at each step, either all
  three writes are present or none.
- **The deletion waits:** `list_deleted_pending` skips the source while
  `awaiting_target`, and `has_pending_pushes` still counts it.
- **Safe, then deleted:** once the target is clean, the source deletion
  pushes.
- **The base check:** a remote edit to the source after phase 1 calls the
  deletion off, and both notes stay.
- **Called off:**
  - a permanent refusal of the target push;
  - removing the target account;
  - removing the source account.

  Each restores `source_prev_state` (or drops the transfer) and emits
  `remote-changed`.
- **Rekey:** an iCloud or Microsoft target uuid that changes at create still
  settles its transfer (save → DB → save, gotcha #16).

**Frontend**

- `MoveToTargets`: other accounts enabled, and each disabled reason from §3.
- `noteMove`/`noteCopy` controllers:
  - Move: the optimistic remove and add in one turn, and the editor follows
    the note to its new account;
  - rollback on an IPC error.
- The confirm sheet: shown for a loss and for a move out of Outlook, and
  skipped otherwise.
- `sidebarPhoneSheets.test.ts`: Copy to on the phone sheet.

**Live pass, owed after the build:** Gmail → iCloud, iCloud → Gmail and
Gmail → Outlook. Check each in Notes.app again after 20 minutes (CLAUDE.md:
"a clean check minutes after a write proves nothing").

## 9. Build order

1. `transfer.rs` Copy, reusing the restore helpers, with its preview and
   tests.
2. The UI for Copy to (no Rust state machine needed).
3. `note_transfers` (a migration), Move phase 1, the worker's hold, settle
   and call-off, with their tests.
4. Move in the UI, and enabling other accounts in `MoveToTargets`.

Copy ships alone first if the move takes longer. It is useful on its own and
carries none of the deletion risk.

## 10. Open questions for the owner

1. **Copy to within the same account?**
   - **Proposed:** yes. Copy to lists every writable account, including the
     note's own, so it doubles as "copy to another folder". Same engine, no
     suffix.
   - **Alternative:** other accounts only, and keep Duplicate for the same
     account.
2. **A move out of Outlook deletes the original for good.**
   - **Proposed:** always confirm (§4.4).
   - **Alternative:** refuse Move out of Outlook, and offer Copy only.
3. **Undo.**
   - **Proposed:** none in this version (§5).
   - **Alternative:** an Undo toast that calls the move off while it is
     still `awaiting_target`, and is a move back after that.
4. **The copy's title.**
   - **Proposed:** unchanged. It is in another account, so there is no
     clash to tell apart.
   - **Alternative:** add " copy", as Duplicate does.

## 11. As built: where the build departs from §2–§8

**Engine (§2.1).** `transfer.rs` does not build a `NoteDoc`.

- It calls restore's body functions directly: `strip_attachments`,
  `append_keywords` and the R9 `CID_LEFT` refusal. It reads the source's
  attachments, tags and render loss straight from SQLite.
- `note_doc` needs a whole-account `BackupRead`, which a one-note copy has
  no reason to load.

**Provenance (§2.2).** No new event kind.

- Duplicate's rule (R12) now takes the source's account:
  `NewNoteHistory::DuplicateOf { source_account, .. }` and
  `provenance::copy_events_across_conn`.
- A copy carries the source's history, then one `create` (or `edit`) by
  `human:owner`, exactly as a Duplicate does. Nothing about a note's trust
  can change.

**Tags (§2.2).** The `Keywords:` line is written only when the target reads
tags as hashtag spans (iCloud) and the source does not. An iCloud → iCloud
copy carries its spans as they are.

**The base check (§4.2).** It lives in `reconcile_one_db`: a pull of a
`deleted_pending` original that has an open move. "Changed" means content,
not version:

- A CloudKit tag moves on its own (gotcha #29), so a version over the same
  base (`only_the_version_moved`), or over the very text that was copied,
  calls nothing off.
- A real change calls the move off. That pull is then reconciled against
  the restored original like any other, so both notes stay.

**A copy deleted before it is safe** (not in §4). The user deleted the
moved note, so the original's deletion is let through too, rather than
bringing back a note the user just removed.

**One transaction (§4.1).** `Db::insert_note_copy` writes the copy's
attachments, the copy, the hidden original and the `note_transfers` row
together. A move whose original changed between plan and write (its state
no longer the one planned) writes nothing and is refused with "changed while
it was being moved".

**The UI (§5).**

- The preview (a SQLite read) is awaited before the optimistic step, because
  it decides whether to ask. The originals are hidden in the step that sends
  the write.
- The open note follows its copy once the write returns: the copy's uuid is
  minted in Rust, so it cannot be known earlier.
- A move within one account inside a mixed batch still goes through
  `moveNoteTo`, one note at a time.

**Call-offs (§4.3).**

- A copy the target refused reaches the error bar through
  `note-transfer-called-off`, beside `remote-changed` for both accounts.
- A call-off from the base check has no event of its own: reconcile holds
  no `AppHandle`. The pull's own `remote-changed` brings the original back
  into the list.

**Icons.** Copy to has its own glyph, `copy-to` (a page and an arrow).
Duplicate keeps `copy`: the editor menu's test forbids two rows sharing an
icon.

**Not built.**

- Drag and drop between accounts (§5.1).
- The live pass (§8).
