# Ask Jodd conversation history — design

Date: 2026-10-14 · Status: approved in conversation, awaiting spec review

## 1. What this is for

Ask Jodd conversations disappear when the modal closes. The owner wants to
**look back** at earlier conversations, **pick one up and keep asking**, and
**archive** the ones they are done with, without losing them.

Success:

- Every answered question is still there after closing the modal or
  restarting Jodd.
- Opening an old conversation and asking a follow-up gives the model the
  earlier turns as context, exactly as if the conversation had never closed.
- Archiving hides a conversation from the main list; it can be unarchived.
  Delete is separate and permanent.
- No privacy guarantee shipped by privacy PR1–PR3 gets weaker (§4).

## 2. What this reverses, and why that is allowed

The Ask Jodd spec (`2026-07-29-ask-jodd-design.md` §3) lists **"No
persistence of conversations … no new table"** as a non-goal, and §5 says
"closing the modal discards the conversation". That was a scoping decision for
v1, not a safety property. This spec replaces those two lines. The
`ask/mod.rs` module doc ("read-only, ephemeral") is updated in the same change:
Ask still never writes notes, folders, edges or sidecars; it now writes its
**own** two tables.

What does **not** change: `Session` in `llm/policy.rs` remains the only thing
that sends history to a provider, and a stamp change still resets it. Saved
history is a second copy, on disk, that a new `Session` can be **seeded from**
after it passes the checks in §4.

## 3. Decisions taken in brainstorming

| Question | Decision |
|---|---|
| What is "archive"? | Hide from the main list, recoverable. Separate Delete. Local SQLite only, never synced. |
| What is saved? | Every answered turn, automatically. Title = first question, renameable. A setting turns it off. |
| Continue after AI settings changed? | Re-check eligibility; ask before sending history to a different destination; otherwise read-only (§4). |
| Where is the list? | Left rail inside a wider modal on desktop/tablet; a full-screen list behind a History button on phone. |

## 4. Resuming a conversation — the privacy rules

Continuing a conversation means sending its earlier questions and answers to a
provider again. Each rule below closes one way that resend could leak
something today's ephemeral design never had to consider.

**R1 — Scope must still be eligible.** The same checks `begin_ask` runs today:
`require_account` for an account-anchored scope, `AiScope::check_folder` for a
folder scope, at least one AI-allowed account.

**R2 — Every note the conversation READ must still be eligible.** Not just the
cited ones: an uncited note's text still shaped the answer. For each
`(uuid, account_id)` in any turn's `read_json`:

1. resolve through `Db::resolve_note_uuid` (gotcha #16 — an iCloud rekey must
   not look like a deletion);
2. the account must pass `llm::policy::account_allowed`;
3. the note must exist and not be `deleted_pending`;
4. `AiScope::check_label(account_id, note.label)` must pass.

Any failure makes the conversation **read-only** with a one-line reason
("A note this conversation used is now in a hidden folder.") and a **Start new
conversation with this scope** button. Fail-closed: a missing note fails R2,
because §R4 cannot rebuild its masked values without it.

**R3 — Ask before a new destination.** The conversation stores the destination
string shown at its last turn (the `AskSessionInfo.destination` text, kept on
`Session.destination` when the session opens — never re-read at save time). If the
current destination differs and the caller did not pass
`accept_destination == current`, `resume_ask` returns
`NeedsConfirm { previous, current }`. The UI shows *"Earlier questions and
answers will be sent to {current}. Continue?"* and, on Continue, calls
`resume_ask` again with `accept_destination = current`. Comparing the accepted
string against the destination computed on that second call means a change
between the two calls asks again instead of slipping through.

**R4 — Rebuild masking context without persisting it.** `privacy::vault::Known`
is "memory only: never persisted". It holds values detected by their context
in a note body (`wifi pass: X`), so a bare `X` repeated later is masked too.
After a restart that set is empty, and an old answer containing `X` would be
resent with nothing to detect it. On resume, `resume_ask` re-reads the current
bodies of every `read_json` note and runs `Vault::reserving` over them,
then `remember`s into the new session's `Known`. The context is re-derived
from the same source it came from; nothing secret is written to disk beyond
the answer text, which is already there.

R4 is only as good as the notes it re-reads: if a read note was later edited
to remove the context that identified a value (say, the `pass:` before it),
R4 cannot rebuild that mask, and the bare value in an old answer is resent.
R3's confirmation and the owner's own view of the history are the backstop;
masking is defense in depth, not the guarantee.

R2 and R4 are implemented as one function,
`ask::history::resume_known(db, accounts, ai, turns) -> Result<Known, String>`.
It checks every note and rebuilds the masking context in a single fail-closed
pass, so a note cannot vanish between the check and the rebuild.

After R1–R4 pass, `Session.turns` is seeded from SQLite and the session
behaves exactly like a fresh one. A stamp change **during** the resumed
conversation resets the in-memory session as today; the saved turns remain on
disk and the conversation goes back through R1–R4 next time.

## 5. Storage — migration 28

```sql
CREATE TABLE ask_conversations (
  id           TEXT PRIMARY KEY,          -- uuid v4
  title        TEXT NOT NULL,
  scope_json   TEXT NOT NULL,             -- serde AskScope, same wire shape
  destination  TEXT NOT NULL,             -- destination text at the last turn
  created_at   INTEGER NOT NULL,          -- epoch ms
  updated_at   INTEGER NOT NULL,
  archived_at  INTEGER                    -- NULL = active
);
CREATE INDEX ask_conversations_list ON ask_conversations(archived_at, updated_at);

CREATE TABLE ask_turns (
  conversation_id TEXT NOT NULL,          -- ask_conversations.id
  seq             INTEGER NOT NULL,
  question        TEXT NOT NULL,
  answer_markdown TEXT NOT NULL,
  cited_json      TEXT NOT NULL,          -- Vec<CitedNote>
  read_json       TEXT NOT NULL,          -- Vec<{uuid, account_id}>
  stats_json      TEXT NOT NULL,          -- notes_in_scope/considered/used, trimmed, dropped_citations
  created_at      INTEGER NOT NULL,
  PRIMARY KEY (conversation_id, seq)
);
```

- Lives in `jodd.sqlite3`, so it is under the same SQLCipher encryption as the
  notes whose excerpts the answers contain.
- **Not** added to `ACCOUNT_ID_TABLES`: neither table has an `account_id`
  column, and migration 19's rekey does not apply. Account ids inside
  `scope_json`/`read_json`/`cited_json` are the post-migration-19
  `{backend}:{email}` form already, since nothing older than this migration
  can exist.
- No foreign key: `db.rs` never enables `PRAGMA foreign_keys`, so a
  `REFERENCES … ON DELETE CASCADE` would be silently inert. Every delete path
  (conversation delete, `clear_ask_history`, account removal) deletes the
  turns explicitly in the same transaction as their conversation rows.
- Only **answered** turns are written, matching what `Session::finish_turn`
  keeps. Cancelled and failed turns stay UI-only, as today.
- `title` = the first question, whitespace-collapsed, cut to 80 chars on a
  char boundary (Thai-safe).

**Account removal.** `Db::delete_account` also deletes conversations whose
`scope_json` names that account (`Account` or `Folder`). `AllAccounts`
conversations stay; their chips to the removed account's notes stop resolving,
and R2 makes them read-only, since a read note's account no longer exists.

**Never exposed to jodd-mcp.** No MCP tool reads these tables (MCP stays
deny-by-default, gotcha #33).

## 6. Backend surface

`ask/history.rs` (new) owns the SQL and the R2/R4 helpers so `lib.rs` only
wires commands.

| Command | Does |
|---|---|
| `begin_ask(scope)` | Unchanged. |
| `resume_ask(conversation_id, accept_destination?)` | Load, R1–R4, seed. Returns `AskResume` (tagged by `kind`, snake_case): `Ready { session }`, `NeedsConfirm { previous, current }` or `ReadOnly { reason }`. None of them carries turns; the modal gets them from `load_ask_conversation`. |
| `ask_jodd(...)` | Unchanged inputs. After `finish_turn` succeeds and history is on: insert the conversation row on the first answer, append the turn, bump `updated_at` and `destination`. `AskAnswer` gains `conversation_id: Option<String>` (None = history off or the save failed; the save failure is logged and never fails the answer). |
| `list_ask_conversations(archived: bool)` | id, title, scope label, updated_at, turn count. |
| `load_ask_conversation(id)` | Turns for read-only display, with no eligibility checks (it is local text the owner already saw). |
| `rename_ask_conversation(id, title)` | |
| `set_ask_conversation_archived(id, archived)` | |
| `delete_ask_conversation(id)` | Also `end_ask`s a live session bound to it. |
| `get/set_ask_history_enabled(bool)`, `clear_ask_history()` | |

R1 refusals are returned as errors with `begin_ask`'s messages, so the modal's consent/provider UI handles them; only R2 and "archived" return `ReadOnly`.

`Session` gains `conversation_id: Option<String>`, set when seeded or after
the first save, so later turns append to the same row.

**The setting lives in its own file** (`ask_history.json` under
`paths::config_base()`, default `{ "enabled": true }`), **not** in
`AppLlmConfig`. `ai_stamp` hashes `app_llm_config::load()`, so a field there
would reset every live Ask session whenever the owner toggled history.

`ask_wire_shapes_are_stable` is extended for `AskAnswer.conversation_id` and
the `AskResume` variants, with the TS types updated in the same commit.

## 7. UI

`AskJoddModal.svelte` hosts a new `AskHistory.svelte` (list, grouping, row
menu) so the modal does not grow another 300 lines.

- **Desktop/tablet (≥ 768px):** modal widens; left rail with **+ New**, groups
  *Today / Last 7 days / Older* by `updated_at`, then a collapsible
  **Archived (N)**. Row ⋯ menu: Rename · Archive/Unarchive · Delete (with confirmation).
- **Phone (< 768px):** a **History** button in the header shows the list
  full-screen, with ← to go back to the chat.
- Selecting a conversation renders its turns (`renderAnswer(markdown, cited)`,
  stats line rebuilt from `stats_json`), sets the scope selector to its scope
  and locks it. Changing scope still means a new conversation.
- `NeedsConfirm` → an inline bar above the input with **Continue** /
  **Not now** ("Cancel" already labels the in-flight cancel button). `ReadOnly` → a banner with the reason and the button from R2
  instead of the input. Archived conversations open read-only, with an
  **Unarchive** button.
- **Local-first:** rename, archive, unarchive and delete update the list in
  memory first, then `invoke`, and roll back with an error toast on failure.
  This avoids the await-before-mutate shape of open defects D5/D6.
- The empty state's "conversations are not kept" sentence is replaced with one
  saying where history is kept and how to turn it off.
- App Settings: a **Keep Ask Jodd history** switch. Turning it off asks
  *"Also delete existing history?"* (Keep / Delete).

## 8. Testing

Rust (`cargo test --workspace`):

- Migration 28 on a fresh DB and on a DB at version 27.
- Save → list → load round trip; first answer creates the row; title cut on a
  Thai string; archive/unarchive filtering; delete removes turns.
- History off → nothing written, `conversation_id` None.
- R1 and R2: hidden folder, account AI off, deleted note → `ReadOnly`; a
  rekeyed note (alias) still passes.
- R3: changed destination → `NeedsConfirm`; matching `accept_destination` →
  `Ready`; stale `accept_destination` → `NeedsConfirm` again.
- **R4:** a note body with `wifi pass: hunter22x`, an answer repeating bare
  `hunter22x`; resume with an empty `Known` and assert the outgoing history
  carries a mask token, not the value. Run the control: the same resume with
  R4 disabled must leak, or the test proves nothing.
- `delete_account` removes account- and folder-scoped conversations, keeps
  all-accounts ones.
- Wire-shape test updated.

Frontend (`npx vitest run`): grouping by date, the archived toggle, and
optimistic rename/archive with rollback.

Live pass: macOS (ask → close → restart → continue; change provider →
confirm bar; hide a cited folder → read-only) and the S23 FE phone layout.

## 9. Out of scope

Search within history · export / save conversation as a note · syncing history
across devices · MCP access to history · retention limits or auto-expiry.
