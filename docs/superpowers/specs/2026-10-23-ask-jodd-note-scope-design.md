# Ask Jodd — note scope and context-menu entry points — design

Date: 2026-10-23 · Status: implemented 2026-10-24 (plan `docs/superpowers/plans/2026-10-24-ask-jodd-note-scope.md`); device live pass owed

## 1. What this is for

Ask Jodd today answers over one of three scopes: all accounts, one account,
or one folder and its subfolders (`ask::AskScope`, `askScope.ts`). The owner
wants to **ask about one specific note**, optionally widened to the notes
around it, and to reach Ask Jodd **from where they already are** — the note
list and the sidebar — instead of opening the popup and choosing a scope.

Success:

- Right-click a note, a folder or an account → "Ask Jodd about this …" →
  the Ask Jodd popup opens, locked to that scope, ready for a question.
- A note-scoped question is answered from that note — and, only when the
  owner ticks it, from the notes linked to it and/or sharing a tag with it,
  up to a depth the owner picks.
- No privacy guarantee of the AI outbound privacy work
  (`2026-10-08-ai-outbound-privacy-design.md`) gets weaker. A hidden folder
  stays hidden, including from the related-notes walk.

## 2. Decisions taken in brainstorming

| Question | Decision | Whose |
|---|---|---|
| Where is note scope reachable? | Only from the note's context menu. The popup's own scope selector does not gain a "this note" option. | owner |
| Which context menus? | Note (note list), folder (sidebar), account (sidebar account header). | owner |
| Does a context-menu open lock the scope? | Yes — shown as "Scope: …", exactly like a loaded conversation today. "Start new conversation" is the way out. | Claude |
| Does it reuse the open conversation? | No. A context-menu open always starts a new conversation; the old one stays in history. | Claude |
| Can note scope include related notes? | Yes, **optional**: "Include linked notes" and "Include notes with the same tag", both **off** by default. | owner |
| Is the walk depth-limited? | Yes: Depth 1, 2 or 3; default 1; 3 is the maximum. | owner (limit), Claude (values) |
| What does "linked" mean? | Both directions: notes this one links to (`Db::outgoing_links`) **and** notes that link to it (`Db::backlinks`). | owner confirmed |
| What does depth count? | Steps from the note. Each step follows every relation that is switched on (links and/or same tag) from every note reached at the previous step. | Claude |
| Cross-account? | No. Links and tags stay inside the note's own account (`edges` and `note_tags` are per account anyway). | Claude |
| Account menu exists? | It does not today. Add one on the account header, holding only this item. | Claude |

## 3. Goals, non-goals, anti-goals

**Goals:** the three menu entries; a `note` scope variant end to end
(frontend type, Rust enum, preflight, retrieval, history); the optional
related-notes walk; a count shown before the first question.

**Non-goals:**

- A "this note" option in the popup's scope dropdown (owner: context menu only).
- Ask Jodd inside the editor or the editor toolbar. (The editor's ⋯ note
  menu, `NoteMoreMenu.svelte`, was a non-goal here until the owner asked for
  it on 2026-10-24 during the live pass; it now carries the same item — §6.2.)
- Cross-account links, `cites` (URL) edges, `child_of` (folder siblings).
- Apple's own note-to-note links (`applenotes:` on iCloud). They are not
  `mentions` edges, so on iCloud "Include linked notes" follows
  `[[wikilinks]]` only. Owner, 2026-10-24: fine for now, a later follow-up.
- Persisting the checkbox/depth choice between openings — every open starts
  from the defaults.
- Any change to the MCP server (`jodd-mcp`). Gotcha #33's "Ask, DB and MCP
  scope must agree" is about folder-subtree matching; this spec adds no
  folder matching, so `folder_scope` and `jodd-mcp/src/scope.rs` stay as
  they are.

**Anti-goals — a failure even if it technically works:**

- A note in a hidden folder reaching the model, **or being walked through**.
  A hidden note's links and tags reveal what it holds, so the walk must
  neither read nor expand it.
- The answer silently drawing on notes the owner did not ask for — e.g.
  related notes when both boxes were off, or another account's notes.
- "I couldn't find anything relevant" for a single-note question about a
  note that exists. One note needs no selection step that can come back
  empty (§5.2).
- A related-notes walk with no bound. A popular tag at depth 3 can reach
  most of a 6,655-note vault.

## 4. The scope type

Rust (`ask/mod.rs`), alongside the existing variants, same serde tagging:

```rust
Note {
    account_id: String,
    uuid: String,
    /// Display only — the title when the scope was chosen. Never used for
    /// retrieval; the preflight re-reads the live title.
    #[serde(default)] title: String,
    #[serde(default)] links: bool,
    #[serde(default)] tags: bool,
    /// 1..=3. Values outside are clamped, not rejected.
    #[serde(default = "one")] depth: u8,
}
```

TypeScript (`askScope.ts`) mirrors it:
`{ kind: 'note'; account_id; uuid; title; links; tags; depth }`.

- `AskScope::account_id()` returns the note's account, so the existing
  `require_account` preflight, the `exclude_accounts` short-circuits in
  `run_ask` / `build_candidate_pool_capped`, and
  `ask_delete_for_account_conn` (history cleanup on account removal) work
  unchanged. `label()` stays `None` for this variant.
- `title` exists because the history list renders scopes with
  `describeScope()` on the frontend and has no way to look a note up. It is
  stale after a rename; that is acceptable for a history label. It goes
  through the same `scope_json` column as today. Old history rows never
  contain this variant, so no migration.
- The uuid can be stale: a note created moments ago may have been rekeyed by
  its first push (gotcha #16). Every read resolves it first with
  `Db::resolve_note_uuid` (as `AiScope::check_source` already does).

## 5. Retrieval

### 5.1 Preflight (`ask_preflight`, `lib.rs`)

For a `Note` scope, after `require_account`:

1. Resolve the uuid; read the note with `note_by_uuid`. Absent or
   `deleted_pending` → refuse: "This note is no longer available."
2. `ai.check_label(account_id, &note.label)` → the existing hidden-note
   refusal if its folder is hidden.
3. Run the walk (§5.3) — read-only SQL — to get the related count.
4. `scope_description`:
   `"{account_id} · {live title}"`, plus `" + {n} related notes"` when
   either box is on, plus `" (closest {cap} kept)"` when the cap bound.

The popup already shows `session.scope` in its consent text, so the count
reaches the owner before anything is sent.

**The walk runs outside the `ai_policy.gate` and accounts locks** (Claude's
call): take what the preflight needs under the locks, release them, walk,
then re-check `ai_stamp` as the preflight already does at its end. A depth-3
walk must not block other AI commands.

**The walk re-runs every turn** (Claude's call), like the folder and account
pools do today, so a note linked mid-conversation or a conversation resumed
from history days later sees the live neighbourhood. The preflight count is
therefore "at the start", and the description says so only implicitly — no
stored note set on `Session`. **Changing a checkbox or the
depth changes the scope value, which already forces a new `begin_ask`** —
the same mechanism as changing the scope dropdown today.

`ask_jodd`'s mid-conversation re-check (currently `Folder` only,
`lib.rs` ~6875) gains the same check for `Note`: a folder can be hidden, or
a note moved into a hidden folder, between turns.

### 5.2 Single note (both boxes off)

`run_ask` skips stages 1–3 (pool, catalog, selection call). It builds the
answer context from the one note and goes straight to the answer call, so a
question costs **one** LLM call. The note's character cap is
`MAX_CONTEXT_CHARS` (120k) instead of `MAX_NOTE_CHARS` (20k); `truncated`
still reports a cut. `notes_in_scope = notes_considered = 1`.

**Consent text for note scope** (Claude's call). The popup's disclosure
line today names "candidate titles/tags/folders, then selected note bodies".
For a note scope it must instead say what is actually sent:

- boxes off: "Sends your questions, eligible conversation history and this
  note's full text (up to 120,000 characters) on every question."
- a box on: the above, plus "titles, tags and folders of the related notes,
  then the bodies of those the AI selects."

### 5.3 The related-notes walk (a box ticked)

A breadth-first walk from the resolved note, in a new function (suggested
home: `ask/pool.rs`, next to `build_candidate_pool`):

- Level 0 is the note. For each step 1..=depth, from every note reached at
  the previous level, collect neighbours:
  - links on → `outgoing_links` ∪ `backlinks`;
  - tags on → every note in the same account that carries any tag of that
    note. For tags use `list_tags_for` / `note_tags` (not body parsing) —
    on iCloud a tag is a hashtag object, not `#word` text (gotcha #38), and
    `note_tags` is already derived correctly per backend.
- A neighbour already seen is skipped (no cycles, no duplicates).
- A neighbour whose folder `ai.allows()` refuses is **dropped and not
  expanded**. Same for `deleted_pending`.
- Order within a level: newest first (`NEWEST_FIRST`, which the link queries
  already return). Levels are kept in order, so nearer notes always win.
- Stop as soon as `CANDIDATE_POOL_MAX` (400) related notes are collected,
  and remember that the cap bound.

**The question still ranks the related set** (Claude's call). Run the
existing FTS term extraction over the question and mark every related note
that `search_notes` hits as `from_fts`, exactly as Source 1 does today — but
only inside the walked set, never adding notes from outside it. FTS hits go
first in the catalog and survive trimming first, as they do now. The walk
itself is still bounded and nearest-first; FTS only orders within it.

Then the existing two-step pipeline runs with these changes:

- The **catalog holds the related notes only** — the focus note is not a
  choice.
- **The selection call is told the focus note's title and tags** (not its
  body; Claude's call), e.g. a line "The question is about the note
  “{title}” (tags: …). Pick related notes that help answer it." The body is
  not sent twice.
- Note scope uses its own selection prompt (or a parameterised one) that
  states "at most 11". The existing
  `select_prompt_states_the_actual_selection_cap` test keeps pinning 12 for
  the other scopes; add the same pin for 11.
- The focus note is **always** in the answer context, first, and is never
  the one a `MAX_CONTEXT_CHARS` trim drops. **Budget** (Claude's call): when
  at least one related note is selected, the focus note is capped at
  60,000 characters (half of `MAX_CONTEXT_CHARS`) and the selected related
  notes share what is left, each still under `MAX_NOTE_CHARS`. When none is
  selected, the focus note gets the full 120,000, as in §5.2. The model may select up to
  `MAX_SELECTED_NOTES - 1` (11) related notes.
- If the model selects none, answer from the focus note alone. Never return
  "I couldn't find anything relevant" for a note scope.
- If the walk finds nothing, this is §5.2 (but the description says
  "+ 0 related notes", so the owner knows the walk ran).

`notes_in_scope` = 1 + related count; `notes_considered` = the same.

### 5.4 Prompt

The answer call must know which note the question is about. Add a short
line to the context for `Note` scope, e.g. "The question is about the note
[[slug]]. Other notes, if any, are related context." **The focus note is never cited** (changed after the live pass,
2026-10-24: a chip naming the note the user was already asking about ended
every line and buried the answer). The answer call is told not to cite it,
and `run_note_ask` strips any `[[focus-slug]]` that comes back anyway and
drops it from `cited`; the note stays in `read`. Related notes are cited as
before (`resolve_citations` unchanged).

## 6. UI

### 6.1 Opening the popup with a scope

Today `askModalOpen` is a boolean store and the popup derives its scope from
the sidebar selection. Add a way to open it **with** a scope — e.g. an
`openAskWith(scope)` helper in `stores/ui.ts` that sets a pending scope and
opens the modal. On open with a pending scope, `AskJoddModal` does what
`startNew(scope)` does today (`scopeOverride = scope`, drop any loaded
conversation, bump `policyRevision`), then clears the pending value. The
plain Ask Jodd button and the AI sheet keep calling `askModalOpen.set(true)`
and behave exactly as now.

### 6.2 The three menu items

| Where | Label | Scope |
|---|---|---|
| `NoteContextMenu.svelte` (note list) | Ask Jodd about this note | `note`, boxes off, depth 1 |
| `NoteMoreMenu.svelte` (editor ⋯; added 2026-10-24) | Ask Jodd about this note | `note`, boxes off, depth 1 |
| Sidebar folder menu (`openFolderMenu`) | Ask Jodd about this folder | `folder` (existing variant) |
| Sidebar account header — **new** menu | Ask Jodd about this account | `account` (existing variant) |

- Not on the Trash row, the "All" row, smart folders or tags. A note in
  Trash: no item.
- **Disabled only for a hidden folder** (Claude's call): the note's folder,
  or the folder itself, is hidden from AI. Reuse `NoteContextMenu`'s
  `aiHiddenFor` / `AI_HIDDEN_HINT`; the folder menu uses the same helper.
  An account item is never disabled for this reason. **Never disabled for
  the account's AI data permission** — the popup's own "Allow AI for …"
  consent step (`needs_consent`) is how that is granted, and disabling the
  item would make it unreachable.
- Multi-select in the note list (batch actions): no item — one note only.
- Phone: each menu already has a sheet form (`noteContextMenuPhoneSheet`,
  the folder sheet). The account menu follows the folder menu's pattern,
  including its sheet and how it is opened on a phone.

### 6.3 The note-scope controls in the popup

Shown only when the active scope is `note`, under the "Scope: …" line:

- ☐ Include linked notes
- ☐ Include notes with the same tag
- Depth: 1 · 2 · 3 — disabled while both boxes are off.

Changing any of them replaces `scopeOverride` with an updated scope, which
starts a new conversation (§5.1). They are hidden — not merely disabled —
for a **loaded** conversation, whose scope is pinned; its "Scope: …" line
shows what it used ("· links, tags, depth 2").

`describeScope()` gains the `note` case: `"{account_id} · {title}"` plus the
related options when on.

## 7. Tests the builder owes

- Rust: serde round trip of `Note` (with and without the defaulted fields).
- Walk: links both directions; tags; depth 1 vs 2 vs 3 on a chain; cycle;
  hidden note neither read **nor walked through** (a chain A → H(hidden) → C
  must not reach C via H); `deleted_pending` dropped; cap binds nearest-first;
  another account's note with the same tag never appears.
- `run_ask` with a fake provider: single-note scope makes exactly **one**
  provider call; related scope keeps the focus note even when the model
  selects nothing; hidden focus note refused; rekeyed uuid resolved.
- Preflight: description text including the count and the cap note.
- Frontend: `describeScope` note case; `askScope`/open-with-scope helper;
  each menu item dispatches the right scope (mounted component, gotcha #28).

## 8. Left to the builder

- Exact names of the walk function and the open-with-scope helper.
- Whether the walk lives in `ask/pool.rs` or a new `ask/related.rs`.
- Wording of the prompt line in §5.4 and of the menu labels' icons.
- Whether tag neighbours come from one SQL query per level or per note, as
  long as the 400 cap bounds the work.
