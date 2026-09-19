# jodd-mcp

An [MCP](https://modelcontextprotocol.io) server exposing Jodd's note vault
(its SQLite cache) to any MCP client — e.g. a Claude Code session in another
project. Reads work out of the box. **Writes are off until you explicitly
grant them**, per account, per folder — see [Granting write
access](#granting-write-access).

Design rationale:
[`docs/superpowers/specs/2026-07-14-wiki-graphify-bundle-1-2-4-design.md`](../docs/superpowers/specs/2026-07-14-wiki-graphify-bundle-1-2-4-design.md)
(read tools) and
[`docs/superpowers/specs/2026-08-11-mcp-write-access-design.md`](../docs/superpowers/specs/2026-08-11-mcp-write-access-design.md)
(write tools).

## Tools

### Read — always available

- `search_notes(account_id?, label?, query)` — full-text search (FTS5,
  Thai-aware trigram) over title + body. Omit `account_id`/`label` to search
  every account/folder. A multi-word query that matches nothing as a literal
  phrase is retried as an OR of its terms, so combining several terms in one
  call returns hits rather than `[]`.
  **Each match's `body_html` is a bounded, tag-stripped text preview (240
  chars), not the note's full content, and at most 50 matches come back,
  best-ranked first.** Both caps exist because an unbounded response is both
  a size problem (a live response of ~52,000 characters broke the client)
  and an exposure problem — whatever secret a note holds should not flow
  into an LLM's context just because it matched. **There is no tool that
  returns a note's complete content by uuid**; narrow the scope
  (`account_id`, `label`, a more specific query) rather than assuming 50
  results were all of them.
- `note_connections(account_id, uuid)` — a note's `[[wikilink]]` graph:
  `outgoing` (notes it links to) and `backlinks` (notes that link to it).
  Connected notes carry the same bounded preview `search_notes` returns, and
  `outgoing` + `backlinks` **combined** hold at most 50 notes (`outgoing`
  fills first) — a hub note's response stays under the same size bound as
  `search_notes`.
- `list_accounts()` — the accounts this server can see: `account_id`,
  `backend_kind`, and `allowed_folders` (which folders the write tools may
  touch). **An empty `allowed_folders` means no write access.** Call this
  first; the write tools' error messages all point back at it.
- `list_tasks(account_id, label?, include_done?)` — checklist rows across the
  account's notes, with their completed state and a 0-based `index` per note.
  `label` scopes to that folder **and its subfolders** — unlike
  `search_notes`, which is exact-match. `include_done` defaults to false, and
  filters *without renumbering*: `index` stays the row's absolute position in
  the note, which is what `set_task_state` expects.

### Write — allowlist-gated

Every one of these refuses unless `mcp_write_scope.json` grants the target
folder for that account. Writes land in Jodd's local SQLite cache as `dirty`
rows; the desktop app's sync worker pushes them to the backend (Gmail →
Apple Notes, or a local `.eml` vault) the next time it runs.

- `create_note(account_id, folder, title, body_markdown)` — body is
  **Markdown, never HTML**; Jodd converts and sanitizes it. `#hashtags` in the
  body become the note's tags. `- [ ]` lines become real, tickable checklist
  rows. The folder is created if missing.
- `update_note(account_id, uuid, body_markdown, mode?, title?, force?)` —
  `mode` is `"append"` (default; existing bytes are never touched) or
  `"replace"`. Replace is **refused** when the note holds content outside
  Jodd's safe subset — Apple checklist state, attachments — that a full
  rewrite would silently destroy, unless `force=true`.
- `set_task_state(account_id, uuid, index, checked, expect_text)` — complete
  or reopen one checklist row. Byte-surgical: only that checkbox changes.
  This, not `update_note` with `mode="replace"`, is how to act on a task.
  `expect_text` is **required** — the task text you believe is at `index`,
  as `list_tasks` returned it — and must be non-empty. An index alone is not
  a stable identity (index 3 exists in every note with four rows), so a
  mismatch refuses the call instead of ticking the wrong box in the wrong
  note. Get both `index` and `expect_text` from `list_tasks` immediately
  before calling.
- `create_folder(account_id, path)` — a full label under `Notes/`. Missing
  ancestors are created.
- `set_pin(account_id, uuid, pinned)` — pin or unpin a note; pinned notes sort
  to the top in Jodd's own UI. Like `update_note`/`set_task_state`, checked
  against the note's *current* folder, not wherever it was when you found the
  uuid. Refused on a backend that can't yet push sidecar changes (pin state
  travels as a Jodd-managed sidecar message, separate from the note content
  push) — `list_accounts`' `backend_kind` won't tell you this directly; a
  refusal names the reason.

Read tools are pure reads and are safe to run alongside a live Jodd instance
(SQLite WAL mode permits concurrent readers). So are the write tools —
`Db::open` sets a busy timeout and the writes are version-guarded — but they
do mutate the same database the desktop app has open.

## Granting write access

Write access is **deny-by-default**. There is no UI for it and no flag: the
server reads a single file, and if that file does not exist every write tool
refuses with an error naming the path.

Create `mcp_write_scope.json` next to `accounts.json` in Jodd's config dir:

| OS      | Path                                                       |
| ------- | ---------------------------------------------------------- |
| macOS   | `~/Library/Application Support/jodd/mcp_write_scope.json`   |
| Windows | `%APPDATA%\jodd\mcp_write_scope.json`                       |
| Linux   | `~/.config/jodd/mcp_write_scope.json`                       |

```json
{
  "accounts": {
    "you@example.com": {
      "allowed_folders": ["Notes/__Claude__"]
    },
    "localfs:AAFD814E-0D6B-4021-BE37-0B222769C871": {
      "allowed_folders": ["Notes/__Claude__"]
    }
  }
}
```

- Keys are Jodd **account ids** — the email address for a Gmail account, an
  arbitrary user-chosen identifier for a LocalFs vault (there is no OAuth
  identity to anchor it to, hence the `localfs:` prefix in the example
  above). Get them from `list_accounts`.
- `allowed_folders` entries are full folder labels. Each grants that folder
  **and everything beneath it** — `Notes/Work` covers
  `Notes/Work/Projects/ATLAS`, but not the unrelated sibling `Notes/WorkX`.
- An account absent from the file, or present with an empty
  `allowed_folders`, has no write access at all.
- Only accounts in the **Active** state are writable. An account you have set
  to Draining or Inactive in the app is refused, and does not appear in
  `list_accounts`.
- The file is re-read on every call, so edits take effect without restarting
  the server or the app.
- If the file exists but does not parse, the **write** tools refuse loudly
  with the parse error. The read tools are unaffected.

### Adding an account

1. Call `list_accounts` to get the exact `account_id` — do not guess it from
   the email you signed in with; a LocalFs vault's id in particular is
   arbitrary and won't match anything visible in the UI.
2. Add (or edit) that key in `mcp_write_scope.json`, same shape as the
   example above.
3. Call `list_accounts` again — no restart needed, the file is re-read every
   call — and confirm the new `allowed_folders` shows up for that account.
4. A `create_note` into the granted folder is the fastest end-to-end check:
   it should appear in Jodd's sidebar under **Workflows** if the folder is a
   `__name__`-style leaf (e.g. `Notes/__Claude__`), and sync to the backend
   (Gmail/Apple Notes, or the local vault) on the app's next worker tick.

### What the allowlist does and does not protect

It confines an agent to a folder subtree per account. That is the whole
security control for this feature, so choose the grant deliberately:

- **Start with a dedicated folder** — `Notes/__Claude__` is the convention.
  A `__name__` leaf is classified as a system-workflow folder and grouped
  separately in Jodd's sidebar, so an agent's output is visually distinct
  from your own notes.
- **Granting `Notes` grants the entire vault.** There is no read-only or
  append-only mode, and no undo beyond what the backend keeps.
- Notes written here **reach your other devices**. On a Gmail account they
  sync to Apple Notes on your iPhone and Mac.
- Folder labels are validated before use: no `.`/`..` segments, no
  backslashes, no drive prefixes, no control characters, 200 bytes per
  segment. A label is a filesystem path to a LocalFs account, so this is
  what keeps a crafted path from escaping the vault root.

Nothing outside `allowed_folders` is writable by any tool here: `update_note`,
`set_task_state` and `set_pin` all re-check the note's *current* folder on
every call, so a uuid captured before you moved a note out of the sandbox
does not keep working.

## Build

```bash
cargo build --release -p jodd-mcp
```

The binary is at `target/release/jodd-mcp` (`.exe` on Windows) — repo-root
`target/`, not `src-tauri/target/`.

## DB path

Defaults to the same location the Jodd desktop app itself uses:
`dirs::data_dir()/jodd/jodd.sqlite3` (e.g.
`~/Library/Application Support/jodd/jodd.sqlite3` on macOS,
`%APPDATA%\jodd\jodd.sqlite3` on Windows). Override with `--db-path <path>`
or the `JODD_DB_PATH` environment variable.

Note: `jodd-mcp` opens the database the same way the desktop app does
(`jodd_lib::db::Db::open`), which applies schema migrations and idempotent
backfill passes (FTS, tag, and edge backfills) on open. In steady state —
a DB already on the schema version this binary expects — these are no-ops.
But if `jodd-mcp` is built from a different Jodd version than the one that
last touched the DB, opening it can trigger a one-time migration/backfill
write. Keep `jodd-mcp` and the desktop app on matching versions to avoid
this.

## Register with Claude Code

```bash
claude mcp add jodd -- /absolute/path/to/target/release/jodd-mcp
```

Or, to point at a non-default DB location:

```bash
claude mcp add jodd -- /absolute/path/to/target/release/jodd-mcp --db-path /absolute/path/to/jodd.sqlite3
```

This is a manual, one-time setup step — `jodd-mcp` is not auto-registered by
the Jodd desktop app, and is not part of the Tauri app bundle or its release
pipeline (see design spec decision 5 for why it lives in its own workspace
member instead of `src-tauri/src/bin` or `src-tauri/examples/`).
