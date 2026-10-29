# AI actions and agent folders — UX cleanup design

The owner asked to "clean up workflow UX/UI" and chose both halves: how a
person reaches Jodd's AI actions, and the sidebar's **Workflows** folder group.
"Design first": nothing here is built. Every call below is the spec author's
unless marked as the owner's, and §11 lists the calls for checking.

Builds on the 2026-10-20 mobile navigation spec
([2026-10-20-mobile-navigation-dialogs-editor-header-design.md](2026-10-20-mobile-navigation-dialogs-editor-header-design.md)),
which introduced the phone AI ✦ sheet, the `Modal`/`Popover` primitives, the
one-line copy rule (§4.4) and "note actions are one module" (§3.3). This
design applies those rules to desktop and tablet, which that spec left alone.

## 1. What is wrong today

Measured on `main` at v0.39.1 (2026-10-28).

### 1.1 AI actions are scattered

About 14 entry points across 9 surfaces, 5 icons, and no single place to
start on desktop:

| Surface | Entry | Icon | Opens |
|---|---|---|---|
| Sidebar account header | (no text) tooltip "Ingest a source into …" | `bulb` | Extract dialog, sets that account current |
| Sidebar footer | (no text) tooltip "Ask Jodd about your notes" | `chat` | Ask, no preset scope |
| Sidebar Views | **Organize** row (desktop only) | `check` | CurateReview |
| Account menu | Ask Jodd about this account | `chat` | Ask, account scope |
| Folder menu | Ask Jodd about this folder | `chat` | Ask, folder scope |
| Folder menu | Generate from this folder… | `bulb` | StudioModal |
| Note context menu | Ask Jodd about this note · Re-extract · Link into wiki · Suggest folder | `chat` `bulb` `graph` `folder` | Ask / re-extract / LinkSuggestionsModal / filing |
| Editor ⋯ menu | Ask Jodd about this note **only** | `chat` | Ask, note scope |
| Share to Jodd sheet | "Or make a note with AI" chips | — | Extract dialog, prefilled |
| Hotkey | Cmd/Ctrl+Shift+L (not in the shortcuts help) | — | Extract dialog |
| Phone Folders bar | raw "✦" glyph, aria "AI actions" | — | AI sheet: Ask, Extract, Workflows folders |

Gating is uneven:
- The account-header Extract button has **no** write-capability check.
- "Generate from this folder…" has **no** hidden-from-AI check. Studio's
  backend refuses a hidden folder (`studio/plan.rs:105`), so the user opens a
  dialog only to be refused. Ask, directly above it, is disabled with
  `AI_HIDDEN_HINT`.
- The editor ⋯ menu claims to mirror the context menu
  (`NoteMoreMenu.svelte` header comment) but has one of its four AI items.

### 1.2 One dialog, six names

`LessonExtractModal.svelte` is called:
- "Ingest a source into …" (sidebar tooltip)
- "Extract" (phone sheet, CHANGELOG)
- "Key points" (its own default option, `ingestSources.ts:14`)
- "lesson extractor" (Re-extract tooltip)
- "Or make a note with AI" (Share to Jodd)

Its title changes with the selected option, and its stop button reads
"Cancel extraction" even while it runs Summarize.

Studio is never named in the UI ("Generate from this folder"), although the
note list shows a "Studio" chip and the CHANGELOG says Studio. User-visible
strings mix **LLM** and **AI** ("LLM provider", "Ask the LLM which existing
folder…", "AI receipts"). The hidden-folder hint says "hidden from AI", but
the Settings heading it points to says "Hidden from agents".

### 1.3 "Workflows" holds no workflows

The sidebar's **Workflows** group is every folder whose leaf matches
`__name__` (`derive_workflow_kind`, `db.rs:6309`), plus their descendants
(`isWorkflowPath`, `Sidebar.svelte:309`). In production today that is:

- `Notes/__Agent__` with `Projects/` and `Areas/`: the agent workspace,
  created by `agent_memory::ensure_skeleton_locked` and written by MCP
  `remember`.
- `Notes/__Claude__` (by convention): whatever folder the jodd-mcp allowlist
  names, created on an agent's first `create_note`.
- `Notes/__Extracts__`: **retired** since 2026-09-15. Nothing writes there,
  and it is already renamable (`RETIRED_WORKFLOW_FOLDERS`).
- A legacy plain-named `system_workflow` row such as `Notes/Lessons`. The kind
  is never demoted, so it stays in the group, unprotected.

**None of Jodd's own AI workflows file there.** Extract, Summarize, Action
items, Expand bullets, Transcript, URL ingest and Share to Jodd all file into
the ordinary `Notes/Inbox` (`llm/filing.rs`). Studio files into the research
folder itself. `ensure_workflow_folder` has no production caller. A person
who runs Summarize and looks under "Workflows" finds nothing; what is there
is the agents' folders.

The protected-row copy is also stale. "Workflow folder — managed by Jodd"
rests on two comment claims (`Sidebar.svelte` ~1901): that the folder
"auto-recreates on next workflow run", and that renaming would orphan
`ensure_workflow_folder`. Neither is true. The real reason is that renaming
would cut the folder off from the agents that write there (the
`mcp_write_scope.json` allowlist).

There is also an unguarded path: **Move to** offers agent folders as targets
for a folder (`rowsByAccount` filtered only by `moveTargetState`). The
allowlist matches a subtree (`folder_scope::matches`, `Mode::Subtree`), so
moving a user folder under `Notes/__Agent__` silently makes it writable by
agents.

### 1.4 The Extract dialog is crowded

Top to bottom (`LessonExtractModal.svelte` ~920–1150):
1. a two-sentence hint
2. three stacked segmented controls (workflow · paste or pick a note · new or
   append)
3. option checkboxes, the textarea, the links section and the title

Several controls show and then disable themselves with a paragraph of
explanation. For example, with links pasted, Append shows a warning
paragraph and Action items is greyed out.

## 2. Goals, non-goals, anti-goals

**Goals**

- One AI menu, the same on every layout: an anchored popover on desktop and
  tablet, the existing sheet on phone.
- One name per AI action, used in every entry point, title, button and
  tooltip.
- Every AI entry point applies the same three gates, the same way (§4).
- The folder group says what it holds: agent folders. Retired and legacy rows
  rejoin the ordinary folder list.
- The note's AI actions are the same in the context menu and the editor ⋯
  menu.
- The make-a-note dialog asks one question at a time, in the order a person
  thinks: what to make, from what, and where it goes.

**Non-goals**

- New AI features, new workflows, or changes to what any workflow sends or
  saves.
- Renaming code: files (`LessonExtractModal.svelte`), stores
  (`extractModalOpen`), Rust identifiers, the `system_workflow` kind value, or
  any IPC command. This is a UI pass. Code renames are a separate, mechanical
  PR if wanted.
- Any change to `derive_workflow_kind`, a migration, or the protection rule
  itself (gotcha #4: derive, don't migrate).
- A new hotkey for Ask Jodd (§12).
- Duplicate review (`DupReviewModal`). It is not AI, and its naming overlap
  with Organize is a follow-up (§12).
- The CHANGELOG's past entries. History stays as written.

**Anti-goals — it fails even if it "works" when:**

- An action becomes reachable for an account, folder or note that a gate used
  to refuse: a read-only account, a hidden folder, or an unconsented account.
- Any open path awaits IPC before the menu or dialog appears (local-first
  doctrine).
- A frontend string match on a backend error breaks. `AskJoddModal` detects
  "needs an LLM provider" by substring (`AskJoddModal.svelte:142`); the
  displayed text may change, the matched text may not, unless both change
  together.
- Phone Back stops working through the menu → dialog → result chain (gotcha
  #31; 2026-10-20 §4.1).

## 3. Names

One table, applied everywhere. "…" marks an entry that opens a dialog.

| Thing | Name in the UI | Replaces |
|---|---|---|
| The menu | **AI** (button aria-label "AI actions") | ✦ glyph, footer 💬 |
| Ask | **Ask Jodd…** / Ask Jodd about this account · folder · note | unchanged |
| The Extract dialog, as an entry | **Make a note with AI…** | "Ingest a source into …", "Extract" |
| Its title | **Make a note with AI** (fixed, not per option) | per-option title |
| Its options | Key points · Summarize · Action items · Expand bullets · Transcript | unchanged |
| Its primary button | **Make note** · **Append** · **Preview action items** · **Make note from N links** | "Extract", "Ingest N sources", "Preview meeting actions" |
| While running | **Working…**, with a **Stop** button | "Extracting…", "Ingesting…", "Cancel extraction" |
| Studio, as an entry | **Studio…** (AI menu) · **Generate with Studio…** (folder menu) | "Generate from this folder…" |
| Studio title | **Studio** with the folder as subtitle | "Generate from this folder" |
| Organize | **Organize…** | unchanged (code keeps "Curate") |
| Re-extract | **Redo key points** — "Run Key points again on this note's Source block" | "Re-extract", "lesson extractor" |
| Link into wiki | **Find related notes** | "Link into wiki" |
| Suggest folder | **Suggest a folder** — "Ask AI which folder this note belongs in" | "Ask the LLM which existing folder…" |
| The provider | **AI provider** (Settings › AI heading, Ask's empty state) | "LLM provider" |
| The hidden-folder setting | **Hidden from AI** (Settings › Privacy heading); help line "Jodd's AI features and agents never see notes in these folders." | "Hidden from agents" — the same list also gates Jodd's own AI (`aiHiddenFor`) |
| Consent button | **Allow AI for {account}** everywhere | three variants |
| The Views row listing notes with a Source block | **Made with AI** (added in phase 1: it otherwise collides with the `__Extracts__` folder, which phase 3 shows as "Extracts" in the folder list) | "Extracts" |
| The folder group | **Agents** | "Workflows" |
| A protected agent folder's menu note | **Agent folder: agents write here, so it can't be renamed or moved.** | "Workflow folder — managed by Jodd" |

Rules for the builder:
- **"AI", never "LLM", in user-visible text.** Code, logs, docs and backend
  error strings matched by the frontend stay as they are.
- **No "ingest" or "extract" in user-visible text.** "Sources from links"
  stays. Past CHANGELOG entries are history and stay as written.
- Every changed string is listed in the PR description, as 2026-10-20 §4.4
  required, so the owner reviews wording in one place.

## 4. The three gates

Every AI entry point decides its state from the same three questions, in this
order. The rule is one function in `src/lib/controllers/aiActions.ts` (new)
that each surface calls. No surface writes its own condition.

1. **Can it be written?** Actions that create or change a note (Make a note,
   Studio, Redo key points, Find related notes, Suggest a folder) need
   `canWrite(caps, 'notes')`, or `'relocate'` for Suggest a folder. The
   answer is **hidden** when no, as menus do today for write actions.
2. **Is the scope hidden from AI?** If the folder or note is hidden
   (`aiHiddenFor`), the item is **disabled**, with `AI_HIDDEN_HINT` as a
   visible second line on the item, not only a tooltip. Touch has no hover
   (2026-10-20 §4.4).
3. **Is there a scope at all?** Studio from the AI menu needs a real folder
   selected (not All, a Smart Folder, a tag, Search or Recently Deleted). With
   none, it is **disabled** with the second line "Select a folder first".

Consent and a missing provider stay where they are today: inside the dialog,
after it opens. Checking them in the menu would need IPC before the menu
opens.

This closes the two gaps in §1.1: the account-level Make a note gains gate 1,
and Studio in the folder menu gains gate 2.

## 5. One AI menu

### 5.1 Content

One component, `AiMenu.svelte` (new), renders the list. `AiActionsSheet`
becomes a thin phone wrapper around it, and the desktop popover is a second
thin wrapper. The list, in order:

1. **Ask Jodd…** — scope as today with no preset: the current folder,
   otherwise the current account.
2. **Make a note with AI…** — into the current account. Shows the shortcut
   (⇧⌘L / Ctrl+Shift+L) on desktop.
3. **Studio…** — on the selected folder (gate 3).
4. **Organize…** — the current account. Desktop only, as today (curate spec
   D5).
5. *(phone only)* separator, then the **Agents** group: one row per agent
   folder, as the sheet lists Workflows today. Desktop and tablet show these
   in the sidebar instead.

Each row is icon, label, and an optional second line (a gate reason or the
shortcut).

### 5.2 Where it opens

| Layout | Button | Presentation |
|---|---|---|
| Desktop | Sidebar footer, replacing the 💬 Ask button: icon `sparkle`, tooltip "AI" | `Popover` anchored to the button, placement top-start |
| Tablet (column or drawer) | The same footer button (tablet mounts the desktop Sidebar) | `Popover`. The drawer closes first when the item opens a dialog |
| Phone | The existing centre button in the Folders bar, now `<Icon name="sparkle">` instead of the raw ✦ | `Modal phone="sheet"`, as today |

Choosing an item closes the menu, then opens the dialog. On phone this keeps
today's order: the sheet's history entry pops, then the dialog's entry is
pushed (`runAfterClose` / `closeInto`). This needs no new mechanism.

### 5.3 What is removed

- **The account-header `bulb` button.** Its one job, opening Make a note for
  an account other than the current one, moves to the account menu: **Make a
  note with AI in this account…**, directly under "Ask Jodd about this
  account", behind gate 1. It sets `currentAccount` and opens the dialog in
  the same synchronous step, as the header button does today.
- **The sidebar-footer 💬 button.** Ask Jodd is the AI menu's first item.
- **The Views › Organize row.** Organize is in the AI menu. The Views group
  keeps only views.

### 5.4 Where the dialog saves, said up front

The account switch above loses a cue: the header button showed which account
you were in. To replace it, the make-a-note dialog's first line names the
destination (§7), e.g. "Saves to Inbox in kaiwan@gmail.com." Nothing else
changes about which account a run uses.

## 6. Agent folders

All frontend. No change to `kind`, `derive_workflow_kind`, or any Rust code.

1. **Rename the group** "Workflows" → **Agents**, on desktop, tablet and the
   phone sheet. The header has no icon. Rows use `folder` like every other
   folder, so `bulb` no longer means "folder".
2. **Group membership follows the name pattern**, the same contract as gotcha
   #4. A row is in Agents iff it, or an ancestor, is `system_workflow`
   **and** has an `__x__` leaf, **and** is not in `RETIRED_WORKFLOW_FOLDERS`.
   Effects:
   - `Notes/__Extracts__` rejoins the ordinary list, shown as "Extracts". It
     is already renamable and deletable.
   - A legacy plain-named `system_workflow` row (`Notes/Lessons`) rejoins the
     ordinary list. It is already unprotected.
   - `__Agent__/Projects` stays under `__Agent__`, by the ancestor rule.

   Grouping and protection then use one predicate,
   `isAgentFolder(kinds, path)` in `sidebar/folderTree.ts`, instead of today's
   two (`isWorkflowPath` for grouping, `isProtectedWorkflowFolder` for
   protection).
3. **Protection is unchanged in effect.** Rename, Move to and Delete stay
   hidden on a protected agent folder. The menu note gets the honest reason
   (§3), and the stale code comment is corrected to match: the allowlist,
   not auto-recreation.
4. **Folders cannot be moved into an agent folder.** The folder **Move to**
   list leaves out agent folders and their subtrees, so a user folder can
   never be made agent-writable by a drag or a menu pick. **Notes** can still
   be moved into one: giving one note to an agent is an explicit,
   note-by-note act, and the destination says "Agents" in the picker.
5. **Display names come from one helper.** `viewName.ts` drops its private
   copy of `stripWorkflowMarkers` and imports `sidebar/folderTree.ts`'s.

## 7. The make-a-note dialog

Same component, same commands, same behaviour. Only order, copy and which
controls show change.

```
Make a note with AI                                              [×]
Saves to Inbox in kaiwan@gmail.com.                       ← one line
What to make  [Key points][Summarize][Action items][Expand bullets][Transcript]
Jodd will distill it into structured key points.         ← the option's one line
┌──────────────────────────────────────────────────────────────┐
│ Paste text or links…                                         │
└──────────────────────────────────────────────────────────────┘
Use an existing note instead                              ← link button
[Sources from links — only when links are found]
[option: Clean transcript / Source is incomplete — only for that option]
Title (optional)                                          ← new note only
Add to   (•) New note   ( ) Existing note…                ← hidden while links are ticked
                                              [Stop]  [Make note]
```

- **The first line is the destination.** It says Inbox, or Notes when the
  account cannot create folders and has no `Notes/Inbox`, the same decision
  `resolve_destination` makes. With Append it reads "Adds to *{note title}*."
  It replaces today's two-sentence hint.
- **What to make comes first**, with the selected option's one-line
  description under it (today's `WORKFLOW_HINTS`). Disabled options say why
  on that line: with links ticked, Action items reads "Action items work on
  pasted text only."
- **The source is the textarea.** "Pick existing note" becomes a link button,
  "Use an existing note instead", which swaps the textarea for the note
  search. Its counterpart is "Paste text instead". The segmented control goes.
- **"Add to" shows only when it is a real choice.** While links are ticked a
  run always makes a new note, so the control is hidden rather than shown
  with a warning paragraph.
- **Title placeholder**: "Made from the result" (today "Auto-derived from
  first lesson").
- **Primary and stop labels** per §3. The modal title stays fixed while the
  option changes.
- Share to Jodd's prefill, consent, receipts, the links section, the action
  items review and the duplicate-source warning are untouched.

## 8. Note AI actions, one list

Per 2026-10-20 §3.3, the four note AI actions become one list exported from
`controllers/noteActions.ts`, rendered by both `NoteContextMenu` and
`NoteMoreMenu`, so the two menus cannot drift. Each item applies the gates in
§4.

- Ask Jodd about this note — `chat`
- Find related notes — `graph`
- Suggest a folder — `folder`
- Redo key points — `sparkle`, only when the note has a Source block

They sit together under a separator in both menus. On phone the editor ⋯
sheet gains the three it lacks. The owner asked for Ask there on 2026-10-24;
this extends that choice (§11).

## 9. Icons

- **Add `sparkle`** to `Icon.svelte`. It is the only new icon. It marks
  things that run AI and make something: the AI menu button, Make a note,
  Studio, Redo key points.
- **`chat`** stays Ask. **`check`** stays Organize. **`graph`** and
  **`folder`** stay on their note actions.
- **`bulb` leaves every AI and folder surface.** If nothing else uses it, the
  builder removes it.

## 10. Constraints that bite

Read the full entries in docs/GOTCHAS.md.

| Rule | Where it applies |
|---|---|
| Gotcha #4 | Agent grouping is derived from the leaf pattern on read. No migration, no new kind value, no demotion |
| Gotcha #28 | "Make a note with AI in this account…" sets `currentAccount` and opens the dialog in one synchronous step; the dialog's existing account-change reset must not fire on that open |
| Gotcha #31 / 2026-10-20 §4.1 | Phone: the sheet's history entry pops before the dialog's is pushed; Back from the dialog returns to Folders, not to a dead sheet |
| Local-first doctrine | The menu and every gate read loaded stores only. No IPC before a menu or dialog opens |
| 2026-10-20 §4.1 guard | No component except `Modal`/`Popover` sets `role="dialog"`; the desktop AI menu is a `Popover` |
| 2026-10-20 §4.4 | One line of help per control; gate reasons are visible lines, not tooltips |
| Privacy (spec 2026-10-08) | Gate 2 only disables. Backend refusals (Studio `plan.rs`, Ask, Extract's `sourceHidden`) stay as the real guard |

## 11. Calls for the owner to check

1. **One AI button replaces the footer Ask button.** Ask Jodd from the
   sidebar becomes two clicks on desktop. The note, folder and account menus
   still open it in one.
2. **The names**: "Make a note with AI", "Studio", "Agents", "Find related
   notes", "Redo key points", "AI provider", "Hidden from AI".
3. **The account-header button goes**, and per-account Make a note moves into
   the account menu.
4. **Folders cannot be moved into agent folders; notes can.**
5. **The editor ⋯ menu gains** Find related notes, Suggest a folder and Redo
   key points.
6. **`__Extracts__` and legacy `Notes/Lessons` move** out of the group into
   the ordinary folder list.

## 12. Testing

**Unit (vitest).** Each test file that pins an old label or group is updated
in the same PR as the change. Known ones:
- `llmWorkflowModal.test.ts`, `extractModal*.test.ts`
- `sidebarWorkflowProtection.test.ts`, `sidebarPhone.test.ts`
  (`['Ask Jodd','Extract','Claude']`, the `['Workflows','Views']` headers)
- `StudioModal.test.ts`, `askContextMenus.test.ts`, `CaptureSheet.test.ts`
- `LlmProviderSettings.test.ts`, `settings.test.ts`

`grep` for each old string before calling a phase done.

New tests:
- `aiActions.ts` gates: read-only account hides, hidden folder disables with
  the hint, no folder disables Studio. One table-driven test feeds every
  surface's inputs.
- `AiMenu`: item order; Organize absent on Android; the Agents group on phone
  only.
- Desktop footer: the button opens a `Popover`; choosing Ask opens Ask and
  closes the popover.
- Account menu: "Make a note with AI in this account…" sets the account and
  opens the dialog in one turn; hidden for a read-only account.
- Agents grouping: `__Agent__` with children in; `__Extracts__` and a
  plain-named `system_workflow` row out; `isAgentFolder` drives both grouping
  and protection.
- Folder Move to: agent folders and their subtrees are absent; note Move to
  still lists them.
- Menu parity: the AI items rendered by `NoteContextMenu` and `NoteMoreMenu`
  for the same note are equal.
- Make-a-note dialog: the destination line for Inbox, root and Append; "Add
  to" hidden while links are ticked; the swap between paste and existing
  note.
- Shortcuts help lists ⇧⌘L / Ctrl+Shift+L.

**Live pass**:
- **macOS**: every AI menu item by keyboard alone (Tab, Enter, Esc returns
  focus to the button).
- **Galaxy S23 FE (phone)**: Folders → AI sheet → each item → Back at each
  step.
- **Tab S7, portrait drawer**: the AI popover from the drawer footer.

## 13. Phasing

Each phase is its own PR and shippable alone, in this order:

1. **Words and gates.**
   - the §3 strings and the `sparkle` icon
   - `aiActions.ts`, with Studio's hidden-folder gate and Make a note's
     write gate
   - the shortcut in the help list

   Smallest, no layout change.

   **As built (2026-10-28):** gate reasons stay in the tooltip in phase 1;
   the visible second line (§4 item 2) moves to phase 2. The existing menus
   draw disabled rows at 40% opacity in a 180–220px column, where a
   65-character line would be faint and wrap three times; the AI menu is
   built with room for it, and the three note and folder menus adopt the
   same row then. Phase 1 also gave the sidebar folder menu the disabled
   style it lacked (a disabled Ask looked clickable), and renamed two
   backend messages that name UI labels (`curate::decide::NO_PROVIDER_NOTE`,
   `llm::provider::STUDIO_ONLY`). Nothing in the frontend matches on either.
2. **One AI menu.**
   - `AiMenu`, with the desktop and tablet popover
   - the phone sheet on top of it
   - the account-menu entry
   - removing the header button, the footer 💬 and the Views › Organize row
   **As built (2026-10-28):**
   - `AiMenu.svelte` holds the list; the Sidebar footer's `Popover` and
     `AiActionsSheet` wrap it, each passing `run` (close, then act).
     Studio shows its folder as a hint, Make a note its shortcut (desktop).
   - Opening the popover moves focus to its first item: it sits after the
     rest of the sidebar in the DOM, so Tab would otherwise reach Settings
     and About first. Choosing an item gives focus back to the ✦ button
     before the dialog opens, so the dialog returns focus there.
   - The visible reason line is one global class, `.menu-reason`
     (`src/styles/sheet-menu.css`), placed after the disabled button so
     its dimming does not fade the reason. The note menu, the editor ⋯
     menu and the folder menu show it **once per menu**, not per item:
     hidden from AI is the only reason their AI items are disabled.
   - Organize now acts on the current account, as the rest of the menu
     does, where the Views row named one account each.
   - The account menu's new item, like its Ask item, does not close a
     tablet's portrait drawer; only the AI menu's items do (`pick`).
3. **Agents.**
   - the rename and `isAgentFolder`
   - retired and legacy rows rejoin the folder list
   - the protected-row copy and the corrected comment
   - the folder Move to rule
   **As built (2026-10-28):**
   - One base rule, `agentFolderRoot(kinds, path)` in
     `sidebar/folderTree.ts`, with two thin uses: `isAgentFolder` (the
     group: the folder or an ancestor is an agent folder) and
     `isProtectedAgentFolder` (the root itself). A single predicate for
     both, as §6.2 first said, would have protected `__Agent__/Projects`,
     against §6.3's "unchanged in effect".
   - The group heading carries the `person` icon after all: without one it
     sat out of line with "Views" below it.
   - The protected folder's note is a `.menu-reason` line, not the
     uppercase header row it replaced, which shouted and stretched the menu.
   - The folder Move-to lists show names without `__` markers; the note
     Move-to picker does too, and tags agent folders "Agents".
   - The rule is frontend-only: `move_folder` itself does not refuse an
     agent folder as a parent. Agents still create folders inside their own
     (`create_folder` over MCP), which this does not touch.
   - `bulb` had no use left and is removed from `Icon.svelte`.
4. **The make-a-note dialog** (§7).
   **As built (2026-10-28):**
   - The destination line asks the backend, not the frontend:
     `list_capture_folders` returns `filing::default_destination` from
     SQLite, the answer Share to Jodd already names. Until it answers, the
     account's `folders` capability decides, as `default_destination` does
     when there is no Inbox row yet.
   - Choosing "Use an existing note instead" now hides the textarea. Before,
     the picked note's raw HTML filled it.
   - Clean transcript and "Source is incomplete" moved below the source and
     the links, as §7 drew them; the KEYWORDS hint stays under the choice
     it is about.
   - The two choices are labelled by their visible headings
     (`aria-labelledby`), not by `aria-label`s nobody could see.
   - With "Existing note…" already chosen and links pasted, "Add to" stays
     (`ingesting` is false), so the links section keeps its one line,
     "Links always make a new note. Switch to “New note” to use them."
5. **Note AI actions, one list** (§8).

   **As built (2026-10-28):**
   - `noteAiActions` (and `hasSourceBlock`, `noteAiReason`) in
     `controllers/noteActions.ts` is the list; each menu maps an id to its
     own handler, because each closes itself its own way (the ⋯ menu waits
     for its close to render before opening Ask; the note actions start,
     then close, as the context menu runs them).
   - "Find related notes" hands its proposals to an app-level review
     (`stores/ui.ts` `linkSuggestions`, `LinkSuggestionsModal` mounted in
     App.svelte). NoteList owned it before, and on a phone NoteList is not
     on screen while the editor is, so the ⋯ menu's run would have found
     nothing to show its results in.
   - The context menu's order follows §8: Ask, Find related notes, Suggest
     a folder, Redo key points (Redo was second).
   - The ⋯ menu adds no separators around the group: there they divide
     groups of writes, and a read-only account keeps Ask with none.

## 14. Left to the builder

- The `sparkle` glyph's drawing, matched to the stroke style of the existing
  icons.
- Popover placement details if top-start clips in a short window.
- Whether the option descriptions in §7 keep today's `WORKFLOW_HINTS` wording
  or are rewritten to one line each (listed in the PR either way).
- Whether `AiActionsSheet.svelte` survives as the phone wrapper or folds into
  `AiMenu` with a `phone` prop. Either works, provided there is one list.

## 15. Follow-ups found during the audit (not part of this design)

- **Duplicate naming.** The non-AI "{n} dup" pill and "Review duplicates"
  (Gmail duplicate messages) share a word with Organize's AI "duplicate
  notes".
- **Ask history shows a raw `account_id`** in its scope line
  (`askHistory.ts` `describeScope`).
- **A hotkey for Ask Jodd.** There is none today; pick one that no editor
  shortcut uses.
- **Code renames** (`LessonExtractModal` → `MakeNoteModal`,
  `extractModalOpen`, `WorkflowEntry`) as one mechanical PR after phase 4.
- **`ensure_workflow_folder` and the Rust `strip_workflow_markers`** have no
  production caller. Delete them, or keep them for a future Jodd-managed
  folder and say so in their doc comments.
- **CHANGELOG line ~636** still describes "a 💡 button on each account". It is
  history, so leave it; the release notes for phase 2 should say where the
  button went.
