# jodd-mcp write access — let a local AI agent create/maintain notes in the vault

> **Status:** design approved 2026-08-11, ready for implementation planning
> **Origin:** brainstorm 2026-08-11, prompted by "should Jodd expose write access to
> AI agents, given we're multi-platform native"
> **Scope:** write tools for `jodd-mcp` only. No Claude Skill wrapper, no Settings
> UI for the allowlist, no remote/cloud MCP transport — all explicitly deferred
> (§6).

## 1. Problem

`jodd-mcp` (locked 2026-07-13, see
[LLM-WIKI-GRAPHIFY-ROADMAP.md](../../LLM-WIKI-GRAPHIFY-ROADMAP.md) item 4) is
deliberately **read-only**: `search_notes` + `note_connections`. That serves
"what does the user already know about X" queries from any local Claude Code
session, but it means an agent can never act on what it learns — it cannot
file a note, extend one, or organize the vault. The ask is to let a local AI
agent (Claude Code/Desktop, on the same machine as Jodd) create and maintain
notes in the vault, without turning `jodd-mcp` into unrestricted write access
to everything the user has ever written — and to do so for **every account
kind Jodd supports**, not just Gmail. Jodd already has two backend verticals
(`Account.backend_kind`): **Gmail** (REST, round-trips to Apple Notes) and
**LocalFs** (`.eml` files under a `root_dir` on disk, a pure local vault with
no Apple/Gmail round-trip). Both share the same `notes`/`folders` SQLite
schema and the same `AppleHtmlDeriver` (FTS/tags/edges) — nothing about the
write feature should be Gmail-specific.

## 2. Approaches considered

| # | Approach | Verdict |
|---|---|---|
| A | **Direct SQLite write.** `jodd-mcp` calls the same `jodd_lib::db::Db` functions the Tauri app's own commands already call (`insert_local_new`, `apply_local_edit`), against the same WAL-mode `jodd.sqlite3`. The background sync worker drains the resulting dirty rows next time the user opens Jodd. | **Chosen.** Matches the local-first doctrine exactly — SQLite is truth-of-the-moment regardless of which process wrote it. WAL mode is already on (`db.rs:177-183`), so two processes touching the file concurrently is existing, proven behavior, not new risk. **Also the only approach that's backend-agnostic for free**: `Db` has no notion of Gmail vs. LocalFs — that split lives entirely in `vertical_for(account)`, one layer below where this writes. A `create_note` call needs zero backend-specific code either way. |
| B | **IPC into a running Tauri app instance**, forwarding writes to its own command handlers. | Rejected. Only pays off if the app must be open at write time; the user confirmed it's fine for a note to sit `dirty` until Jodd is next opened. Adds a new local RPC surface for no needed benefit. |
| C | **Direct-to-Gmail write** via the `Vertical`/`Transport` trait, bypassing the local cache entirely. | Rejected for v1. Device-independent, but duplicates OAuth/refresh logic into a second process and reintroduces the self-induced-conflict class `a693d11` already fixed for the app itself (no `AppState.pushing`-equivalent tracking in a standalone binary). **Also Gmail-specific by construction** — a LocalFs account has no OAuth/Gmail REST endpoint to write to, so this approach would need a second, separate code path for LocalFs vaults anyway, on top of everything else it costs. Revisit only if a remote/cloud MCP caller is ever needed (§6). |

Caller shape locked by the brainstorm: **local only** — Claude Code/Desktop on
the same machine as Jodd, same as today's read-only `jodd-mcp`. This rules out
needing any network-reachable transport for v1.

## 3. Architecture

`jodd-mcp` gains a second tool tier — writes — gated by a **per-account folder
allowlist**. Same binary, same Cargo workspace member, same DB. No new
process, no new transport. The running Jodd app is never required to be open;
its 5s sync-worker tick drains whatever the MCP process left `dirty` the next
time it runs, identically to any edit made while the app was closed.

```
Claude Code (stdio MCP client)
        │
        ▼
   jodd-mcp process
        │  1. load mcp_write_scope.json
        │  2. resolve target folder/note's label
        │  3. allowlist check (recursive subtree)
        ▼
  jodd_lib::db::Db   (same jodd.sqlite3, WAL mode)
        │  insert_local_new / apply_local_edit
        ▼
  row marked dirty / dirty_new
        │
        ▼  (only once the user next opens Jodd)
  existing 5s sync worker → vertical_for(account)
        │
        ├─ BackendKind::Gmail   → Gmail REST → Apple Notes
        └─ BackendKind::LocalFs → .eml file under account's root_dir
```

`jodd-mcp` never branches on `backend_kind` itself — it writes to `Db`
exactly once per operation, the same call regardless of which kind of
account it's targeting. The branch already exists one layer down, in the
worker's `vertical_for(account)` dispatch, and this feature simply sits
above it, same as `search_notes`/`note_connections` already do today.

## 4. Components

### 4.1 `mcp_write_scope.json` (new)

Lives beside `accounts.json` in the Tauri config dir. Shape:

```json
{
  "accounts": {
    "<account_id>": {
      "allowed_folders": ["Notes/__Claude__", "Notes/Work/Projects"]
    }
  }
}
```

- **Deny-by-default.** No file, no account entry, or an empty
  `allowed_folders` list all mean "no write access for that account" — write
  tools refuse outright rather than guessing a folder.
- **Matching is recursive-subtree**, reusing gotcha #1's pattern exactly:
  `label = ?1 OR label LIKE ?1 || '/%'`. An entry of `Notes/Work/Projects`
  covers `Notes/Work/Projects/ATLAS` but not the sibling
  `Notes/Work/ProjectsX` — the same `|| '/%'` guard `db.rs`'s test fixture
  already asserts against for `search_notes`/`ask::pool`.
- Hand-edited JSON for v1. A Jodd Settings UI to manage it is explicitly
  deferred (§6) — the file format is the contract either way, so a UI can be
  added later without changing the tool surface.
- **Backend-agnostic by construction.** The `folders`/`notes.label` schema
  (§3) is identical for Gmail and LocalFs accounts, so one `account_id` key
  and one `allowed_folders` list work unchanged for either kind — there's no
  separate shape for a LocalFs vault's allowlist.
- **⚠️ The allowlist protects a *namespace*, and one backend reads that
  namespace as filesystem paths.** Added 2026-08-12 after the whole-branch
  review found and reproduced an escape. §3's claim that the feature is
  backend-agnostic "by construction" is true of `Db` — which genuinely has no
  notion of Gmail vs LocalFs — and false one layer down: `LocalFsVertical`
  maps a label to `root_dir/Notes/<rest>` with a plain `Path::join`, which
  does not normalize. A label containing `..` therefore passed the
  string-prefix allowlist check and escaped the vault root, letting an agent
  write files anywhere the user could. That confidence in the abstraction is
  precisely what stopped anyone asking what a label *means* to each vertical.
  **A label is not an opaque string; validate it as a path.** Every write
  entry point routes through one shared `validate_label_path` that rejects
  `.`/`..`/empty segments and caps length — one validator, so a future third
  writer inherits it instead of re-deriving it.

### 4.2 `list_accounts` (new, read-only, no allowlist gating)

Solves a real gap: `account_id` is the `Account.id` field (`accounts.rs:92`
— `pub id: AccountId`, a real email for Gmail accounts, an arbitrary
user-chosen identifier for LocalFs vaults since there's no OAuth identity to
anchor it to), and nothing previously told a calling agent what values are
valid. Returns, for every active account (`hidden_account_ids()`-filtered,
same as `search_notes`), covering **both `BackendKind::Gmail` and
`BackendKind::LocalFs` accounts** with no distinction in how they're listed:

```json
{
  "account_id": "personal@example.com",
  "backend_kind": "Gmail",
  "allowed_folders": ["Notes/__Claude__"]
}
```

`backend_kind` is included explicitly so a caller (or the user reading a
transcript) knows what to expect from a write — a `Gmail` account's notes
eventually reach Apple Notes; a `LocalFs` account's notes stay in that
vault's `root_dir` and never leave the machine. This is the natural first
call for any write-capable session — it answers "which accounts exist" and
"what am I allowed to touch in each" in one round trip, instead of an agent
having to infer `account_id` incidentally from a prior `search_notes` result
(works, but only once something already exists to search for) or require the
user to state it. Unlike the write tools, no `account_id` param — it lists
everything the caller could act on.

### 4.3 Write tools

Four write tools: `create_note`, `update_note`, `create_folder`,
`set_task_state` (§4.6). All require
`account_id` explicitly — unlike `search_notes`, where it's optional. "Write
to every account" has no sane default; guessing one would be a silent,
surprising choice. Callers are expected to resolve `account_id` via
`list_accounts` (§4.2) first, not guess it.

**Tags: hashtags-in-body, no dedicated tag tools.** An earlier draft had
`add_tag`/`remove_tag` tools calling `Db::add_tag`/`Db::remove_tag` directly.
Review killed them: since v0.15.x the body is the single source of truth for
tags — `reconcile_tags_from_body_conn` re-derives `note_tags` from body
hashtags on every write, and the comment at
[lib.rs:4085-4093](../../../src-tauri/src/lib.rs#L4085) records the exact
failure mode (Extract once called `add_tag` explicitly; "the body-derived
reconciliation overwrote those rows a moment later"). A tag added via a
dedicated tool but absent from the body would silently self-erase on the next
body write. So the agent asserts tags the same way Extract does: **write
`#hashtag` into `body_markdown`**. This is also the fidelity manifest's
SHARED tier — inline hashtags round-trip to Apple Notes natively, whereas
bare `note_tags` rows are Jodd-local at best. Tag *removal* requires a body
edit and therefore rides `update_note` replace; a dedicated removal
affordance is deferred (§8).

**Content contract: Markdown in, HTML never.** No write tool accepts
`body_html`. The body parameter on `create_note`/`update_note` is
**`body_markdown`**, and Jodd converts it internally (§4.4). Rationale: the
note body's wire format is an Apple-specific HTML dialect
([FIDELITY-Gmail-Apple.md](../../FIDELITY-Gmail-Apple.md)) that a calling
agent cannot be expected to know — checklist `checked=` state, attachment
`<object cid>` references, title injection. Markdown naturally produces only
the SHARED-tier safe subset (headings, bold/italic, lists, links), so an
agent that never touches HTML can never corrupt what it can't see. The
agent-facing format and the storage format are deliberately different
layers.

- **`create_note(account_id, folder, title, body_markdown)`** — `folder` must
  resolve inside the allowlist (checked before anything is written). Converts
  the body via §4.4, then mirrors the `extract_note` pattern at
  [lib.rs:4050-4083](../../../src-tauri/src/lib.rs#L4050): resolve/ensure the
  folder, generate a fresh UUID, build a `db::CachedNote` with
  `sync_state: Dirty`, call `Db::insert_local_new`. Tags are never passed
  explicitly — they derive from `#hashtag` text in the body via the same
  deriver `insert_local_new` already runs, identically to Extract.
- **`update_note(account_id, uuid, body_markdown, mode = "append", force =
  false)`** — looks up the note's *current* label and allowlist-checks it
  (protects against a stale UUID captured from an earlier `search_notes`
  call reaching outside the sandbox, e.g. after the note was manually
  moved), then applies one of two modes; both end in `Db::apply_local_edit`
  unchanged:
  - **`mode: "append"` (default, always safe).** Fetch `existing.body_html`,
    convert + sanitize the new Markdown fragment (§4.4), concatenate after
    the existing body. **Existing bytes are never touched** — whatever
    Apple-authored markup lives there (checklist state, attachment refs,
    foreign HTML) survives regardless of what the agent knows. Same shape as
    `llm::markdown::append_to_note_body`
    ([markdown.rs:98](../../../src-tauri/src/llm/markdown.rs#L98)), already
    proven by Extract's re-ingest path. Two well-formed fragments
    concatenated stay well-formed, so append needs no guard.
  - **`mode: "replace"` (guarded).** Full-body rewrite. Before writing, run
    `is_replace_safe(existing.body_html)` — the mechanical check defined in
    §4.4, evaluated **on canonicalized forms, not raw bytes**:
    `canon(body) == strict_sanitize(body)`, where `canon()` parses and
    re-serializes through the same html5ever pipeline with **no filtering**.
    Comparing raw `sanitize(x) == x` would false-positive on nearly every
    non-MCP-authored note — ammonia's serializer normalizes attribute
    quoting, entities, and whitespace, so even a semantically-safe body
    typed in Jodd's own editor differs cosmetically from ammonia's output.
    Running both sides through the same serializer cancels the cosmetics; a
    difference then means content was **actually stripped**. Equal ⇒ the
    note holds nothing worth protecting — replace freely (covers the common
    case, especially notes the agent itself created). Different ⇒ the note
    carries content Jodd cannot re-author (Apple checklist state,
    attachment parts, unknown markup): **refuse**, with an error naming what
    would be lost and suggesting append — or `force: true` if the caller
    truly intends the destruction.
  - Title is updated only when a `title` param is passed; omitted = keep.
  - **Folder is not an editable field in v1** — keeps the guardrail a single
    label check instead of two (old-folder AND new-folder).
  - **The tool description carries this contract** — MCP descriptions are
    read by the calling LLM, so "append is default and always safe; replace
    refuses when it would destroy content you can't see" written there means
    a well-behaved agent self-selects append without ever hitting the guard.
    The guard is a backstop, not the primary UX — same
    teach-the-caller principle as the `list_accounts` error (§5).
- **`create_folder(account_id, path)`** — the new path itself must resolve
  inside the allowlist. Wraps the same path the `create_folder` Tauri command
  ([lib.rs:2809](../../../src-tauri/src/lib.rs#L2809)) uses, same
  `dirty_new` sync state. Note: per gotcha #4, a `__name__` leaf (including
  the example `Notes/__Claude__`) is classified `kind='system_workflow'` by
  `derive_workflow_kind` and grouped with `__Extracts__` in the sidebar's
  workflow section. **This is intentional** — agent output grouped like
  workflow output is the right default — but it's a convention consequence,
  not an accident, so it's stated here.

Deliberately **not** in v1: `delete_note`, moving a note between folders, and
dedicated tag tools (see above). All are higher-blast-radius or
doctrine-conflicting relative to the "create/maintain" goal; see §8.

### 4.4 Content pipeline: `md_to_html` → `sanitize_note_html`

Every body an agent supplies passes through two stages before touching
`notes.body_html`:

1. **Convert** — the existing `llm::markdown::md_to_html()`
   ([markdown.rs:28](../../../src-tauri/src/llm/markdown.rs#L28)),
   pulldown-cmark with the same GFM options Extract already uses in
   production. Zero new conversion code in `jodd-mcp`.
2. **Sanitize** — a new `sanitize_note_html()` in the `llm::markdown` (or a
   sibling) module, backed by `ammonia`. This step exists because
   pulldown-cmark's well-formedness guarantee covers only pure Markdown
   syntax: CommonMark permits **raw HTML passthrough**, which pulldown-cmark
   echoes verbatim without validating. An LLM caller's Markdown containing a
   stray or unclosed tag would otherwise flow into `body_html` untouched.
   `ammonia` re-parses with a real HTML5 parsing algorithm (`html5ever`,
   already transitively in `Cargo.lock`) and re-serializes from the parsed
   tree — output is balanced and valid **by construction**, not by trust —
   with the allowed tag/attribute set pinned to the fidelity manifest's
   SHARED-tier subset (headings, b/i/strong/em, lists, links, p/div/span,
   blockquote, code/pre). Anything else is stripped, never passed through.

**Two allowlists, because they answer two different questions.** An earlier
draft used one filter for both hygiene and protection. Implementation
(2026-08-12) proved that impossible: GFM tasklists compile to
`<input type="checkbox" disabled>`, so the write path *must* allow
`input[type,checked]` — but Jodd's own editor represents checklist rows as
exactly `<input type="checkbox" checked="">`
([NoteEditor.svelte:433-441](../../../src/lib/components/NoteEditor.svelte#L433)),
so allowing it made the guard pass notes carrying checklist state — the one
fact the fidelity manifest calls "the canonical PRESERVED tier." One list
cannot both permit a construct and protect it. The module therefore exposes:

- **`sanitize_note_html(html) -> String`** — the **permissive** list
  (headings, text emphasis, lists, links, tables, `details`/`summary`,
  `input[type,checked,disabled]`, `class`/`id`). Applied to agent-supplied
  fragments on the way in. "What may an agent write?"
- **`is_replace_safe(html) -> bool`** — `canonicalize_note_html(html) ==
  strict_sanitize(html)`, where the **strict** list is the permissive one
  **minus `input` and its attributes**. Any checkbox at all — Apple's,
  Jodd's, or an agent's own inert one — makes a note replace-unsafe. "What
  must never be silently destroyed?"
- **`canonicalize_note_html(html) -> String`** — parse + re-serialize, no
  filtering. Shared by the comparison above.

Two rules that make the pipeline safe rather than destructive:

- **Only new fragments are sanitized — never `existing.body_html`.**
  Sanitizing existing content would itself be the data-loss bug: it would
  strip the very Apple-authored markup (checklist `checked=`, `<object cid>`
  attachment refs) the whole design protects. Existing bytes pass through
  append untouched; the sanitizer only gates what the agent adds.
- **The strict list is derived from the permissive one**, not written out
  twice, so a tag added for the write path cannot silently widen the guard
  without someone deciding it should.

GFM tasklists get a dedicated conversion step so agent-written tasks are
real, tickable Jodd/Apple rows rather than inert decorations — see §4.6,
which also covers reading and completing tasks. One consequence of the rule
above is worth stating: a note holding *any* checkbox, including an
agent-written one, is thereafter replace-unsafe. That is conservative in the
right direction — once a box exists, someone may have ticked it.

Pre-existing issue this surfaces (out of scope, tracked in §8): Extract's own
`assemble_note_body` output reaches `body_html` unsanitized today
([lib.rs:4040](../../../src-tauri/src/lib.rs#L4040)) — the same
`sanitize_note_html()` should eventually back that path too.

### 4.5 Cross-process safety (required, not optional hardening)

Two defects surface the moment a second process writes the DB. Both are
pre-existing hazards this feature widens from theoretical to real, and both
fixes are one-liners in `db.rs` that benefit the app too:

- **`mark_pushed` must become version-guarded.**
  [db.rs:1326-1348](../../../src-tauri/src/db.rs#L1326) today
  unconditionally sets `sync_state='clean'` **and overwrites `body_html`
  with the pushed body**. The race: worker reads a dirty note and starts
  pushing → `jodd-mcp` appends to the same note (new body, still `dirty`) →
  worker's `mark_pushed` lands, reverting the body to the pre-append version
  and marking it clean. **The MCP edit is silently and permanently lost.**
  Fix: the worker passes the `local_version` it read at push start, and
  `mark_pushed` adds `AND local_version = ?` to its `WHERE`. A mid-push edit
  then makes the UPDATE a no-op — the row stays `dirty` and simply
  re-pushes on the next tick. Backend-agnostic, and closes the same
  (narrower) in-process window for the app's own UI edits during a push.
- **`Db::open` must set `PRAGMA busy_timeout`** (e.g. 5000 ms) alongside the
  existing WAL/synchronous/temp_store PRAGMAs
  ([db.rs:181-183](../../../src-tauri/src/db.rs#L181)). SQLite's default
  busy timeout is 0: WAL permits concurrent readers but still one writer at
  a time, so without it, whichever of app/`jodd-mcp` loses a write collision
  gets an immediate `SQLITE_BUSY` error instead of waiting a few
  milliseconds. One line in `Db::open` covers both processes, since
  `jodd-mcp` opens the DB through the same function.

### 4.6 Task semantics — checkboxes are state, not formatting

Added 2026-08-12 after implementation exposed the gap. A checklist item is
the one construct in a note body that carries **user state** rather than
presentation: `checked` is a fact about the world, authored by whoever
ticked the box, on any device. An agent maintaining a vault should be able
to write real tasks, see which are outstanding, and complete them — and
must never destroy a tick it cannot see.

**The canonical Jodd/Apple task row** (from
[NoteEditor.svelte:1178](../../../src/lib/components/NoteEditor.svelte#L1178)
and `propagateChecklist` at :443-460):

```html
<div><input type="checkbox" contenteditable="false">&nbsp;task text</div>
<div checked-form><input type="checkbox" checked="" contenteditable="false">&nbsp;done task</div>
<div style="margin-left: 28px">…</div>   <!-- nesting: 28px per level, max 168 -->
```

GFM Markdown compiles `- [ ]` to something structurally different —
`<ul><li><input disabled="" type="checkbox">text</li></ul>` — which renders
as a checkbox but is **not** a Jodd task row: `disabled` blocks ticking
outright, and `taskBlock()` (NoteEditor.svelte:433) requires the input to be
a direct child of a top-level block, so Enter-key and indent behavior do not
apply. An agent writing `- [ ]` today produces a decoration, not a task.

**(A) Conversion — `taskify_checklists(html)`.** A new step between
`md_to_html` and `sanitize_note_html` (§4.4) rewrites GFM tasklists into the
canonical row shape: `<li>` → `<div>`, `disabled` dropped,
`contenteditable="false"` added, `&nbsp;` separator, nesting depth → `style="margin-left:{28×level}px"`.
Parser-based (the html5ever stack §4.4 already pins), never regex.
**Scope rule: a list converts only if *every* item is a task item.** Mixed
lists are left untouched — rare, and the fallback is today's behavior, not a
regression. The permissive allowlist gains `contenteditable` on `input` and
`style` on `div` (value-restricted to `margin-left`), or those attributes
would be stripped a step later. The strict list (§4.4) still excludes
`input` entirely, so this does not weaken the replace-guard.

**(B) Read — `list_tasks(account_id, label?, include_done?)`.** Returns
`[{uuid, title, label, tasks: [{index, text, checked, level}]}]` so an agent
can answer "what's outstanding" without parsing HTML itself — the same
reason bodies go in as Markdown. Read-only, not allowlist-gated (it reads no
more than `search_notes` already does). **A SQL pre-filter is mandatory**
before parsing: `body_html LIKE '%type="checkbox"%'`. The measured vault has
6,655 notes / 18 MB of HTML in one account (see the Ask Jodd spec's F1);
parsing every body per call is not viable, and the pre-filter is the same
narrow-then-work shape `ask::pool` uses.

**(C) Write — `set_task_state(account_id, uuid, index, checked, expect_text?)`.**
Ticks or unticks one box. `index` is 0-based document order of
`input[type=checkbox]` within the note — the same order `list_tasks`
returns.

- **Byte-surgical, not a DOM round-trip.** Locate the *index*-th `<input …>`
  tag whose text contains `type="checkbox"`, then insert or remove
  ` checked=""` **inside that one tag**, leaving every other byte of the
  body untouched. Re-serializing the whole document through html5ever would
  normalize markup Jodd did not author — the same data-loss the
  never-sanitize-existing-bodies rule (§4.4) exists to prevent. Tag document
  order equals byte order, so a small forward scanner suffices; the codebase
  already prefers hand-rolled scanners here (`extract_urls`, "no regex
  crate").
- **`expect_text` guards against stale indices.** Optional; when given and
  the task at `index` does not match, refuse and name the actual text. An
  index read minutes ago may have shifted if the note changed — cheap
  insurance, and it teaches the caller what it actually hit.
- Allowlist-gated like every write (§4.1), and it is the *safe* way to
  change a task: an agent steered here by the replace-guard's error message
  completes a task without risking the rest of the body.

**Deliberately not in v1:** creating or deleting a task row in place (write
a new one via `update_note` append), and reordering. Nested-list conversion
beyond the margin mapping above.

## 5. Error handling

- **Folder not in allowlist** → the tool call fails with an explicit error
  naming the folder and pointing at `mcp_write_scope.json`. Never a silent
  no-op — a caller (human or agent) driving Claude Code needs to see *why*
  the write didn't happen.
- **`mcp_write_scope.json` missing or unparseable** → all write tools refuse
  with a "no write scope configured" error. The existing read-only tools
  (`search_notes`, `note_connections`) are untouched by this — a broken write
  config must never regress a feature that already works.
- **`account_id` omitted on a write call** → rejected at the schema level
  (required parameter), not resolved to a default.
- **`account_id` unknown / not in `hidden_account_ids()`-filtered active set**
  → error message explicitly suggests calling `list_accounts` (§4.2) rather
  than just "invalid account" — the failure should teach the caller the
  correct next call, not just reject it.
- **`mode: "replace"` refused by the fidelity guard (§4.3)** → same
  teach-the-caller principle: the error names *what* the note contains that
  a replace would destroy (checklist state / attachment references /
  unrecognized markup) and states the ways forward: `mode: "append"`,
  `set_task_state` when the intent was to complete a task (§4.6), or
  `force: true` if the destruction is genuinely intended. Never a bare
  "replace not allowed".
- **`set_task_state` index out of range, or `expect_text` mismatch** → name
  how many tasks the note actually has, or what the task at that index
  really says, and point at `list_tasks` for fresh indices.
- **DB lock contention with a concurrently-running Jodd app** — WAL (already
  on) handles concurrent reads; write-vs-write collisions and the in-flight
  push race are handled by §4.5's two `db.rs` changes (`busy_timeout`,
  version-guarded `mark_pushed`). `jodd-mcp` opens the DB through the same
  `Db::open`, so both processes always run identical PRAGMAs.

## 6. Testing

- **Allowlist matcher unit tests** — mirror the sibling-folder fixture that
  already guards gotcha #1 (`Notes/Work` allowed must not leak into
  `Notes/WorkX`).
- **Sanitizer unit tests (§4.4)** — malformed raw-HTML passthrough in
  Markdown (unclosed `<div>`, stray tags) comes out balanced; disallowed
  tags are stripped; and the canonical-comparison property: for a body that
  is semantically inside the safe subset but *not* ammonia-serialized (e.g.
  editor-style `<b>x</b><div><br></div>` markup),
  `canon(x) == sanitize(x)` holds — the cosmetic-normalization
  false-positive the raw `sanitize(x) == x` check would have produced.
- **Append/replace semantics tests** — append never mutates existing bytes
  (byte-compare prefix); replace succeeds on a clean body *and* on a
  cosmetically-non-canonical safe body; replace refuses on a body containing
  `checked=` / `<object cid>` fixtures and succeeds on the same fixtures
  with `force: true`.
- **`mark_pushed` race test (§4.5)** — read a dirty note, apply a second
  `apply_local_edit` (simulating the MCP process), then call `mark_pushed`
  with the *original* `local_version`: the row must stay `dirty` with the
  second edit's body intact.
- **Tag derivation test** — `create_note` with `#hashtag` in
  `body_markdown` yields the tag in `note_tags` via the body-derivation
  path (no explicit tag call anywhere in the write path).
- **Integration tests** against a temp `jodd_lib::db::Db::new` instance,
  writing through the new tool functions and asserting `sync_state` transitions
  (`dirty`/`dirty_new`) and that FTS/tags/edges derive correctly — same shape
  as existing `db.rs` tests, invoked through the new wrappers instead of
  directly.
- **Manual smoke test**: register the rebuilt `jodd-mcp` binary as a local MCP
  server in Claude Code (`claude mcp add jodd -- <binary>`, server name
  `jodd` — consistent with §7), create/edit a note in an allowed folder,
  then open Jodd and confirm the note appears and pushes to Gmail on the
  next worker tick.
- **Repeat the smoke test against a LocalFs account** (a Jodd account with
  `backend_kind = LocalFs`, pointed at a temp `root_dir`) — confirm the same
  `create_note`/`update_note` calls work unchanged and the note lands as a
  `.eml` file on disk after the next worker tick, with no Gmail/network
  activity involved. This is the concrete proof that §2's "generalizes for
  free" claim actually holds, not just an architectural assertion.

## 7. How it's used

No Claude Skill and no connector needed for v1 — this is wired exactly like
the existing read-only `jodd-mcp`: a local stdio MCP server registered with
the calling agent's own MCP config. Nothing about distribution changes; only
the tool surface grows. Two concrete callers:

**Claude Code:**
```bash
claude mcp add jodd -- /path/to/target/release/jodd-mcp
```
(or the project-scoped `.mcp.json` equivalent). Then, in a session:
> "List my Jodd accounts, then file a note into `Notes/__Claude__` in
> personal@example.com summarizing this repo's README."

Claude calls `list_accounts` → picks `personal@example.com` and confirms
`Notes/__Claude__` is in its `allowed_folders` → calls `create_note`. Every
step is visible in the transcript since MCP tool calls are logged like any
other tool use.

**Codex CLI:** registered the same way, in `~/.codex/config.toml`:
```toml
[mcp_servers.jodd]
command = "/path/to/target/release/jodd-mcp"
args = []
```
**Verified against Codex CLI 0.147.0** (2026-08-12) — `codex mcp list` shows
`jodd` as `enabled`, and a forced tool call round-tripped through
`mcp: jodd/list_accounts (completed)` with output matching Claude Code's
exactly.

⚠️ **`codex exec` (headless) needs `--approve-for-me`, not just
`approval = "never"` in config.** The first call to any newly-registered MCP
tool goes through a separate approval gate that the shell-command approval
policy does not cover. With no TTY to answer it, `codex exec` silently
**cancels** the call — `mcp: jodd/list_accounts (failed) — user cancelled MCP
tool call` — even though nothing was actually declined by a human. This
reproduced identically inside and outside an unrelated third-party hook
wrapper (Orca), ruling that out as the cause; it is Codex's own MCP-approval
flow. Confirmed fix:
```bash
codex exec --approve-for-me "list Jodd accounts, then create a note in Notes/__Claude__ \
  titled 'Repo notes' summarizing what this project does"
```
Without `--approve-for-me`, a plausible-looking failure mode is that Codex
silently **falls back to reading `accounts.json` off disk with raw shell**
(`sed`/`jq`) instead of calling `list_accounts` — observed once in testing.
The output can look superficially correct (same accounts) while never having
called `jodd-mcp` at all, and while completely bypassing the write allowlist
for any operation that follows. If validating this integration, confirm
`mcp: jodd/<tool>` actually appears in the transcript — don't infer a
working MCP connection from plausible output alone.

Same `list_accounts` → `create_note` sequence, driven by Codex's tool-calling
loop instead of Claude's.

Both cases lean on `list_accounts` (§4.2) as the required first call — this
is precisely the gap that motivated adding it: without it, an agent's first
write attempt would either guess an `account_id` or require the user to
supply one by hand.

Same sequence for a **LocalFs vault** — nothing in the prompt or the tool
calls changes, only which `account_id` gets picked:
> "List my Jodd accounts, then add a note to `Notes/Journal` in my
> `research-vault` account about today's findings."

`list_accounts` returns `research-vault` with `"backend_kind": "LocalFs"`;
the agent proceeds identically, and the note ends up as an `.eml` file under
that account's `root_dir` instead of eventually reaching Gmail.

## 8. Deferred / future work

### 8.0 Known gaps at merge (from the whole-branch review, 2026-08-12)

Recorded here because they outlive the scratch workspace. Ranked.

1. **Add a containment check inside `LocalFsVertical::folder_path`** — the
   actual filesystem sink. The traversal escape (§4.1) is currently closed by
   validating at every *entry point*; the sink itself still trusts its input.
   Every one of its eight callers is now either validated or disk-derived, so
   nothing live can reach it with a bad label — but that is validation at N
   places rather than one, and N grows. A ninth caller added later inherits
   nothing. Deferred deliberately: `canonicalize` fails on a not-yet-created
   directory (the `ensure_folder` case) and a symlinked vault root makes
   "under root" ambiguous, so it needs its own tests rather than riding along
   with a jodd-mcp fix. **Braces now, belt next.**
2. **`update_note` / `set_task_state` never re-validate `existing.label`.** No
   live source can mint a row with a traversal label (LocalFs labels are
   derived from real on-disk paths; Gmail labels are not filesystem paths;
   `rename_folder` composes from a validated leaf), so this is residual risk
   only for a DB left over from a pre-fix build. Close it alongside (1).
3. **Four independent notions of "is a checkbox"** — `is_checkbox_input`
   (DOM), `span_is_checkbox` (byte span, accepts three quoting forms),
   `task_checkbox`, and the SQL pre-filter `body_html LIKE '%type="checkbox"%'`.
   The SQL one is the narrowest: a note with `type='checkbox'` would be
   invisible to `list_tasks` while `set_task_state` would happily tick it. Not
   reachable today (every producer double-quotes) and nothing tests the
   agreement.
4. **`insert_local_new` and `apply_local_edit` are not transactional** — row
   write, FTS reindex, tag/edge/citation reconciliation and slug-link rewrite
   run as separate autocommit statements. Pre-existing and self-healing on the
   next write, but a second process now makes a half-applied edit *observable*,
   widening the window from "one process, one mutex" to two.
5. **No length cap on `body_markdown`** — the one unbounded agent-controlled
   string in the write surface. `folder`, `path` and `title` are capped.
6. **The success-payload JSON escaping has no test** — the builders sit behind
   `rmcp`'s tool macros, so a title containing `"` is never exercised.
7. **App-side folder validation silently got stricter** (backslashes, control
   characters, `X:` drive prefixes, extended dot-runs) as a side effect of
   sharing the validator. Safe direction, user-initiated paths only, but it
   went out without a CHANGELOG line.
8. **Orphan Gmail message on a mid-push edit.** `mark_pushed` is
   version-guarded (§4.5), but the vertical has already pushed by the time the
   guard fires, so the first push's message is never trashed. No data loss —
   the row re-pushes next tick — and "Cleanup orphans" tooling exists.



- **`delete_note` / move-between-folders** — real use cases, but higher blast
  radius; revisit once the create/update/folder surface has real usage to
  learn from.
- **Dedicated tag-removal affordance** — dropped from v1 along with
  `add_tag`/`remove_tag` (§4.3: body-derived tags make dedicated tag tools
  self-erasing). Removing a tag means removing the `#hashtag` from the body,
  which today requires a guarded replace; a targeted "remove this hashtag"
  helper could make that safer later if usage shows the need.
- **Settings UI for `mcp_write_scope.json`** — today it's hand-edited JSON;
  a UI affordance in Account Settings is a natural fast-follow, not a
  blocker.
- **Claude Skill wrapper** (e.g. "file this into my Jodd vault") — a thin
  convenience layer over the MCP tools, not a different architecture. Worth
  doing once the raw tools are proven.
- **Remote/cloud MCP transport (Approach C)** — only relevant if a caller
  other than "Claude Code on the same machine as Jodd" shows up. Not needed
  today; revisit if that changes.
- **Sanitize Extract's own output** — `assemble_note_body` reaches
  `body_html` unsanitized today ([lib.rs:4040](../../../src-tauri/src/lib.rs#L4040));
  route it through the same `sanitize_note_html()` once it exists. Separate
  change with its own regression risk (existing Extract notes' bodies must
  not churn), so deliberately not bundled into v1 here.
- **Task row creation/deletion in place, and reordering** — §4.6 covers
  writing tasks (via `update_note`), reading them (`list_tasks`), and
  completing them (`set_task_state`); surgically inserting or removing a row
  mid-body is the remaining gap.
