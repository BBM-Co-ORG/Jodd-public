# Properties, dated tasks and views (P1–P3) — design

Date: 2026-11-19 · Status: draft for approval · Owner: Kaiwan

## 1. Intent

The owner wants Jodd to be a PIM, an "AI-OS" and a second brain at once. What it
lacks for that is not another hard-coded feature but **three primitives** that
later features (Today, Daily note, Weekly review, Person CRM, Project dashboard,
Meeting prep) can be *defined* from instead of built one by one:

- **P1 Properties** — `Key: value` lines that make a note carry data.
- **P2 Dated tasks** — checklist rows that carry a due date.
- **P3 Views** — saved, programmable filters ("which notes / which task rows
  match these conditions"), written as notes.

One query engine serves both the owner in the app and every agent over
`jodd-mcp`, so a given view gives the same answer to both.

Everything is **plain text in the note body**, typed by hand on any device —
including Apple Notes on the iPhone — and **derived into SQLite** by Jodd. Jodd
adds authoring help and rendering on top, never hidden metadata underneath.

## 2. Goals, non-goals, anti-goals

**Goals**
1. A property block typed on an iPhone in Apple Notes is recognized by Jodd on
   the next sync, with no Jodd-only step.
2. Jodd's editor helps write the block and due dates in the canonical form, and
   renders recognized lines so the owner can see the system understood them.
3. A view is a note; creating, editing, syncing and deleting a view is creating,
   editing, syncing and deleting a note.
4. The app and agents read views through one Rust engine with the same results
   (within the agent's hidden-folder scope).
5. `list_tasks` / `set_task_state` keep working with unchanged indices.

**Non-goals (this spec)**
- Reminders / notifications when something falls due (needs P7 Trigger).
- Daily notes, templates, recipes, schedules (P5–P7).
- Entities and aliases (P4), cross-account views, numeric/typed values beyond
  dates, times of day on due dates, recurring tasks.
- A task-row list pane in the app (v1 lists *notes*; see §6.5).
- Mapping properties into the SSH backend's YAML frontmatter.

**Anti-goals — a failure even if it "works"**
- **Jodd rewrites what the owner typed without a click.** No normalizing,
  reformatting or "repairing" a property line or a date on open, on save or on
  sync. The 2026-10-11 `migrate_tags_to_body` lesson: a repair that writes into
  the body turns a derivation into edits pushed to the owner's Apple Notes.
- **A view writes anything** into any note.
- **Views become `folders` rows.** Rejected on 2026-07-13 (LLM-WIKI roadmap,
  Smart Folders): a row no Gmail label backs gets caught by
  `reconcile_folders_from_labels`' prune, the dirty-folder push loop and the
  folder context menu — the D1/D8/D11 class of bug. Views stay virtual, like
  Orphaned/Stale/Extracts/Unreviewed. `folders.kind = 'smart_query'` stays
  unused.
- **`set_task_state` indices shift** because of this work.
- **An agent sees a hidden folder** through a view.

## 3. Constraints (each found by a real failure)

- *Round trip:* anything stored inside the body survives Apple; anything outside
  it is Jodd-local (ARCHITECTURE "Compatibility tiers"). A `Key: value` line is
  plain text and round-trips on every backend; iCloud's hashtag stage 1 already
  shipped a `Keywords:` line the same way.
- *Derive, don't migrate* (gotcha #4): derived tables are re-derived on every
  write/sync from the body; a migration only creates the tables and backfills.
- *Title* (gotchas #11, #21, #27): the title is the first **line** of the note's
  text, not a node shape.
- *Scope* (gotcha #33): Ask, DB and MCP folder scope agree through
  `folder_scope::matches`; MCP is deny-by-default.
- *Worker → UI* (gotcha #6): a view that is open must refresh when the worker
  changes data on its own (`remote-changed`) — trace it to the pixel.
- *Painting* (gotcha #34, Mac lag 2026-11-17/18): decoration uses the CSS Custom
  Highlight API, never the selection; ranges leaving every highlight pass
  through a transparent "gone" highlight; paint **on-screen ranges only** (the
  Find fix of 2026-11-18), or WebKit's `updateHighlightPositions` freezes long
  notes.
- *Prior decision:* note "type" was dropped twice (2026-07-15, 2026-07-20) "for
  lack of a consumer"; the recorded reopening condition is "a type-filtered
  Smart Folder". P1 therefore ships **only together with P3**.

## 4. Approaches considered

| | How | Trade-off |
|---|---|---|
| **1 — chosen** | Derive on every write into `note_props` and `task_items`; a view's **own property block is its query** | One migration and more derivation to keep in step; one syntax for everything, P3 is literally made of P1, `jodd-mcp` reads the tables directly (same SQLite file) |
| 2 | No tables; parse bodies when a view opens | No migration, no drift; every view open and MCP call re-parses every note's HTML — slow on Android, duplicated in two processes, worst for "sort tasks by due" |
| 3 | Derive tables, but a view is one query line (`due<=today folder:Projects/* -tag:someday`) | More expressive (OR/NOT) but a second syntax and parser; a typo reads as noise on the iPhone |

Owner's decisions (2026-11-19): **approach 1**; **one engine for app and agents
(option C), the app listing notes first**; **properties only in a block directly
under the title**; **a note's own `Due:` stays out of task views** (§8.2).

## 5. P1 — Properties

### 5.1 Syntax

The **property block** is the run of lines that starts on the line directly
after the title line and ends at the first line that is blank or is not a
property line. A note with no such line has no properties. Lines further down
the note never count, however they look (`Q: ราคาเท่าไร` in meeting minutes is
text).

A **property line** is `Key: value`:
- `Key`: starts with a letter (any script, Thai included); then letters, digits,
  spaces, `-`, `_`; at most 32 characters; no leading/trailing space.
- `:` then at least one space, then a non-empty `value` (trimmed).
- Keys compare **case-insensitively** (`status` = `Status`); the first spelling
  in the note is the display form.
- A value is one string; commas carry no meaning inside a note
  (`Client: Backyard&Co, Ltd` is one value). A key may repeat; every occurrence
  is kept, in order.
- A value that is an ISO date `YYYY-MM-DD` is also stored as a date for
  comparison. Nothing else is typed.

Where the block starts depends on whether a backend's *cached* body still carries
the title line (gotcha #21 says iCloud's does; `inject_title_into_body` suggests
Gmail's does too). The builder pins this **per backend with a test** against a
real cached body and starts the block on the correct line for each.

### 5.2 Storage

```sql
note_props(account_id, uuid, pos INTEGER, key TEXT,   -- key lower-cased
           key_display TEXT, value TEXT, value_date TEXT NULL,
           PRIMARY KEY (account_id, uuid, pos))
-- indexes: (account_id, key, value), (account_id, key, value_date)
```

### 5.3 Where it is derived

`db.rs` already has one choke point that re-derives a note's tags and edges from
its body: `reconcile_tags_and_edges_from_body_conn` (rename it, e.g.
`reconcile_derived_from_body_conn`). P1 and P2 hook in there, parsing the body
once. Every site that deletes a note's `note_tags`/`edges` today (`delete`,
`rekey_note_uuid`, `delete_account`, `delete_notes_batch`, `mark_deleted`,
`prune_clean`, and any others found) must do the same for `note_props` and
`task_items`. A **guard test** scans `db.rs` and `db/` for every
`DELETE FROM note_tags` and fails if the matching `note_props`/`task_items`
statement is missing (precedent: `every_local_version_bump_is_accounted_for`).

### 5.4 One parser, two languages

The editor must recognize exactly what the deriver recognizes. The property-line
and due-date grammars are implemented in Rust and in TypeScript, and both run the
**same fixture file** (precedent: Package G's shared scope fixtures) — Thai keys,
repeated keys, a blank line ending the block, a non-property line ending the
block, a title that itself looks like `Key: value`.

## 6. P2 — Dated tasks

### 6.1 What a task row is

Every backend already converges on one shape:
`<div class="jodd-task"><input type="checkbox" [checked]>…text</div>` — Gmail
and LocalFs through `taskify_checklists`, SSH from `- [ ]`, iCloud from Apple's
own checklists (`icloud/format_html.rs`). The row parser that `jodd-mcp`
uses today (`write.rs`, `TaskItem`, absolute `index`) **moves into `jodd_lib`**
so derivation and MCP share one implementation; `list_tasks` output and
`set_task_state` indices stay byte-identical (a test pins this on fixtures).

### 6.2 Due-date syntax

Inside the task's text: `📅 YYYY-MM-DD` or `due: YYYY-MM-DD` (case-insensitive,
anywhere in the row). If several appear, the last wins. Date only — no time.
The **canonical form Jodd writes** is `📅 2026-11-25` (the convention Obsidian
Tasks uses, so notes moved between tools keep working); `due:` is accepted for
fast typing and for keyboards without emoji.

### 6.3 Storage

```sql
task_items(account_id, uuid, idx INTEGER,            -- = list_tasks index
           text TEXT, done INTEGER, due TEXT NULL, depth INTEGER,
           PRIMARY KEY (account_id, uuid, idx))
-- index: (account_id, done, due)
```

"Today" is the device's local date at query time; it is never stored.

### 6.4 Round trip

Due dates are text, so they survive everywhere. Whether an iPhone shows a
Jodd-written checkbox in a **Gmail** note as a checkbox is **unmeasured** — live
check L1 (§10) before any user-facing text claims it.

### 6.5 What the app shows in v1

The app lists *notes*. A view with `Show: tasks` lists each note that holds at
least one matching row, with the count of matching rows and the earliest due
date. Ticking a task happens in the note, as today. A task-row pane is a later
step.

## 7. Authoring and rendering in the editor

**Rule: decoration only.** The body changes only when the owner clicks: picks a
suggestion, accepts a fix, or uses a toolbar action.

**Rendering (recognized = visible)**
- Property block: the key painted muted, the value painted as data (accent
  color), via CSS highlights; plus a marker in the existing line-number gutter
  (#335) on each recognized property line.
- Due dates: the date text painted by state — overdue (red), today (amber),
  future (neutral) — with the relative day ("พรุ่งนี้", "เลย 2 วัน") on hover.
- A line in the block position that *almost* parses (e.g. `Status:active`,
  `Due: 25/11`) gets a wavy underline.
- On-screen ranges only; re-paint on scroll, like Find.

**Authoring help**
- Typing at the start of a line inside the block offers **keys** already used in
  this account (from `note_props`, most used first); after `Key: `, offers
  **values** used with that key. Same component pattern as `HashtagPicker` /
  `LinkPicker`.
- After `Due:` / `📅` / `due:`, a **date picker** writes `YYYY-MM-DD`.
- **Fix-it** on a wavy underline: `25/11` → this year's `2026-11-25`;
  `25/11/2569` (Buddhist year) → `2026-11-25`; `Status:active` → `Status: active`.
- Toolbar **Add property**: inserts `Key: ` under the title, creating the block
  if there is none (and keeping it contiguous). Toolbar **Due date** on a task
  row appends ` 📅 <picked date>`.

## 8. P3 — Views

### 8.1 What a view is

A **view** is any note in a folder whose leaf is `__Views__` (in practice
`Notes/__Views__`, auto-classified `system_workflow` by `derive_workflow_kind`).
Its title is the view's name; its **property block is its query**. It syncs to
every device like any note; on the iPhone it is a readable text note.

```
งานค้างวันนี้
Show: tasks
Due: <= today
Done: no
Folder: Projects/*
Sort: due
```

### 8.2 Query keys

Lines AND together. Within one line, commas mean OR. A value may start with
`not ` to negate it.

| Key | Values | Meaning |
|---|---|---|
| `Show` | `notes` (default), `tasks` | what the view returns |
| `Folder` | `Projects/*` (subtree), `Inbox` (exact) | via `folder_scope::matches` |
| `Tag` | `meeting, call` | note tags (`db::tags_for_account` semantics) |
| `Done` | `yes`, `no` | task rows (`Show: tasks`) |
| `Due` | `today`, `< today`, `<= today`, `this week`, `next 7 days`, `>= 2026-12-01`, `none` | task rows |
| `Modified` | `last 7 days`, `today` | note's last modification |
| `Pinned` | `yes`, `no` | |
| `Text` | words | FTS over the note |
| `Sort` | `due`, `modified`, `title` | default: `modified` (notes), `due` (tasks) |
| `Limit` | number | default chosen by the builder |
| *any other key* | `active, waiting` / `not done` / `< today` | matches `note_props` (see below) |

**Structural keys** are `Show`, `Folder`, `Tag`, `Modified`, `Pinned`, `Text`,
`Sort`, `Limit`. In a view they always mean the row above; a note may still carry
a property with one of these names, but a view cannot filter on it (documented,
not an error). **`Due` and `Done` depend on `Show`:** under `Show: tasks` they
filter task rows; under `Show: notes` they are ordinary property filters on the
note's own `Due:` / `Done:` lines. *(Builder-check call, 2026-11-19.)*
**A note's own `Due:` never appears in a `Show: tasks` view** — not as a row,
not as a pseudo-row. Task views list checklist rows only; a project's deadline
is found by a `Show: notes` view. Combining both into one "due today" list is
the Today composite's job, later. *(Owner's decision, 2026-11-19.)*

**Property filters.** Key case-insensitive. A value with no operator matches
exactly, case-insensitively. A value that starts with a comparison (`<`, `<=`,
`>`, `>=`) or is a date word (`today`, `this week`, `next 7 days`, `none`)
compares against the note's `value_date`, with the same grammar as `Due`. A note
whose value for that key is not a date does not match, and the result reports
"N notes have `Key:` values that are not dates". *(Builder-check call.)*

Notes in `__Views__` themselves, trashed and `deleted_pending` notes never
appear in results. The keys of view notes are left out of the key picker and of
the "no note has the property" check.

### 8.3 Errors are shown, never swallowed

A clause the engine cannot read (`Due: soon`) is reported with its line, and the
view does **not** run with the clause silently dropped (a filter that quietly
widens is worse than none). A key no note in the account uses still runs and
returns zero rows, and the result says "no note has the property `X`".

### 8.4 The engine

A Rust module (e.g. `src-tauri/src/views/`) with one entry point:
`run_view(db, account_id, query, scope, today) -> ViewResult { rows, errors }`.
Parsing the view note → a typed query → SQL with **bound parameters only**
(never interpolated values). `scope` is `None` for the app and the agent's
hidden-folder scope for MCP. The same function backs the Tauri commands and the
MCP tools; neither re-implements filtering.

### 8.5 In the app

- The sidebar's **Views** fold lists each view note under the four fixed rows,
  per account. Virtual: nothing is written to `folders`.
- Clicking one shows matching notes in the middle pane (§6.5 for tasks).
- Editing a view note shows a live strip under its block: "ตรง 12 โน้ต", or its
  errors.
- **New view** creates a note in `__Views__` (ensuring the folder the way
  workflow folders are ensured today) with a starter block.
- An open view re-runs on local edits and on `remote-changed` (gotcha #6).

### 8.6 For agents (`jodd-mcp`)

- `list_views(account_id)` — name, uuid, its query lines, any errors.
- `query_notes(account_id, view | lines)` — runs a saved view by name/uuid, or an
  inline block of query lines. Task rows carry `uuid`, `idx` and `text`, so the
  result feeds `set_task_state` (which still demands `expect_text`).
- Creating a view is `create_note` into `__Views__`, gated by the existing write
  allowlist. A view note inside a hidden folder is not listed.
- The installed `jodd-mcp` binary must be rebuilt and reinstalled to expose
  these (stale-binary memory).

### 8.7 Accounts that cannot hold `__Views__`

Views are per account and need a `__Views__` folder in that account. Where
`Capabilities::for_backend(..).writes.folders` is false (Microsoft, permanently —
gotcha #12), **New view** is not offered; its place in the Views fold says views
need an account that can create folders. If a `__Views__` folder already holds a
note there (made in Notes.app), its views are read normally. `list_views` returns
an empty list for such accounts, not an error. Cross-account views stay a
non-goal. *(Builder-check call, 2026-11-19.)*

## 9. Migration and backfill

One migration (next free number on `main` — 39 as of 2026-11-19) creates both
tables and backfills them by re-deriving every cached note through the same
helper (precedent: `migrate.rs` already re-derives edges in a migration).

## 10. Verification

**Automated:** the shared fixtures (§5.4); per-backend title-line test (§5.1);
derivation on every write path and the delete-site guard (§5.3); `list_tasks`
index parity (§6.1); engine tests per key, OR/NOT, errors, sorting, scope
exclusion of hidden folders; a test that rendering/authoring code never calls a
save without a user action.

**Live checks (owed before the feature is described to users):**
- **L1** Gmail: a Jodd checklist with `📅` dates, viewed and edited in Apple
  Notes on the iPhone — does the checkbox survive, and the date text?
- **L2** A property block typed in Apple Notes on the iPhone (Gmail and iCloud)
  appears as properties in Jodd; on iCloud re-check past 20 minutes (the delayed
  merge, BACKEND-ICLOUD).
- **L3** Painting on a long ingested note on the Mac and on Android: no freeze.
- **L4** An agent's `query_notes` and the app's view return the same notes.

## 11. Left to the builder

Table/column names, the module layout, the starter block for New view, the
`Limit` default, how keys are ranked in the picker, the exact colors (from
DESIGN.md tokens), the shape of the Tauri commands, and how the TS and Rust
fixture runners share one file.

## 12. Open

- L1's answer may force the Gmail-specific wording of task features.
