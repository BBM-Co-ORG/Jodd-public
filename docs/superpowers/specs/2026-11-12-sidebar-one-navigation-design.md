# Sidebar: one navigation, two sizes (2026-11-12)

Source: the owner's ask of 2026-11-12 — "Make the phone look more like the
Mac. Views on Android look odd. Re-check what they should be called and how
they should be grouped, and whether the layout suits having several
accounts" — and the Impeccable critique of the sidebar on every platform the
same day (`.impeccable/critique/2026-11-12T15-41-45Z__src-lib-components-sidebar-svelte.md`,
23/40). The owner approved the proposal and chose each open option below.

**Supersedes:** decision 1 of `2026-11-10-sidebar-workspace-design.md` (Views
and Tags act on "the account of the last thing picked", and Recently Deleted
sits in Views) and decision 6 (the phone order), and §5.2 of
`2026-10-20-mobile-navigation-dialogs-editor-header-design.md` (the phone's
"Smart folders" drill-in, the account label, the Agents group in the AI ✦
sheet). It also replaces the phone half of PR #342 (Views as a fold on the
phone, merged the same day): the same stored key, `jodd.sidebar.viewsOpen`,
now lives in `src/lib/sidebarFolds.ts` with the other folds, and #342's
`src/lib/viewsFold.ts` is removed.

## The problem

- **One concept, three names.** "Views" on desktop held the four computed
  lists and Recently Deleted; "Views" on the phone held Tags, a "Smart views"
  drill-in and Recently Deleted; the code and the spec said "Smart folders".
- **Recently Deleted is not a view.** It is each backend's own trash, which
  is why it vanished from Views whenever Outlook (no trash) was current.
- **The scope was a memory task.** Views and Tags acted on the account of the
  last thing picked. Opening an iCloud folder silently swapped the tag cloud
  to iCloud's; the Tags header never named its account.
- **528 tags had no way in.** Opening Tags on desktop pushed every account
  out of the column; the phone Tags pane was an A–Z list about 25,000px long.
- **The phone was a different design.** Its account label had no fold, no
  backend tag and only one of desktop's three state marks; the four views
  were one drill-in deeper than on the Mac.

## Decisions

### 1. Names and order — the same on every layout

From the top of the column:

1. **For ⌄** — the account picker (§3), only with two or more active
   accounts.
2. **Views** — the four computed lists only: Not linked, Untouched, Made with
   AI, Unreviewed (`SMART_FOLDER_NAMES`). A fold, closed by default, one
   stored choice per device for both layouts. "Smart views" is retired.
   "Smart Folders" stays free for the user-defined smart folders the roadmap
   reserves (`folders.kind = 'smart_query'`).
3. **Tags** — its own section (§4).
4. **Each account** — its header (fold chevron, short name, backend tag,
   state marks, count; the header opens every note in the account), its
   folders, its **Agents** group, and its own **Recently Deleted** (§2).

Not linked takes the `link` icon (the magnifier read as search).

### 2. Recently Deleted belongs to its account

The last row of each account's tree, as in Apple Notes, and only where
`backend_capabilities.has_trash` is true (`shouldShowTrash`). Outlook simply
has no row, rather than a shared row that disappears. Its icon is ink
(`<Icon name="trash" inherit />`): the row goes to a place; it deletes nothing.

### 3. The "For ⌄" picker names the account Views and Tags show

*Owner's choice, over per-account copies and over keeping today's rule.*

- One component, `sidebar/ScopePicker.svelte`: an anchored menu on desktop and
  tablet, a bottom sheet on the phone (Popover decides). Rows: each active
  account with a check on the one shown and its state mark (alert or
  waiting, as on its header).
- **Choosing changes what Views and Tags list, and nothing else.** The open
  list, the open note and the current account stay. Picking a view or a tag
  then opens it, and that pick makes its account current, as every pick does.
- **Until the user chooses, the scope is the current account** (the 2026-11-10
  rule), and the picker says which. Once chosen it stays, across launches
  (`jodd.sidebar.scopeAccount`), until the user chooses again or that account
  stops being active (`stores/sidebarScope.ts`).
- A tag pill reads as picked only when its account is the current one: a
  same-named tag in another account is a different tag.
- **Not built: All accounts.** Every list, view query and the editor's save
  path is keyed on one `currentAccount`; a mixed list touches the save path,
  which PRODUCT.md principle 1 puts above any visual change. The picker is
  where an "All accounts" row would go when that change has its own spec.
  *Update 2026-11-13:* proposed in `2026-11-13-all-accounts-scope-design.md`,
  which finds the save path is already note-keyed (§1 there).

### 4. Tags

*Owner's choice: filter and capped cloud.*

- **Desktop and tablet:** the pill cloud, as before, with AND/OR and Clear.
  From 20 tags a **Filter N tags** field sits above it, and the cloud is
  capped at about six lines of pills and scrolls inside itself, so opening
  Tags never pushes the accounts out of the column.
- **Phone:** the Tags header opens the Tags pane (with Select and AND/OR, as
  before), which gains the same filter from 20 tags.
- The filter is a case-insensitive substring, so a Thai tag matches on any
  part, as search does. It clears when the scoped account changes.
- **Most-used first** (added 2026-11-13 at the owner's request): both Tags
  lists (the desktop cloud and the phone Tags pane) show the tags on the most
  notes first, ties A–Z (`src/lib/tagOrder.ts`). The store, `tagsByAccount`,
  stays alphabetical: the editor's # picker reads its order.
- **Tags is one fold for the column** (`jodd.sidebar.tagsOpen`), no longer one
  per account, so it does not flip open or shut when its account changes. An
  upgrade reads the old per-account key once: open if any account had it open.

### 5. The phone is the desktop column at touch size

What now matches the Mac exactly: the order and names (§1), Views opening in
place (the Smart views pane and its `smart` phoneNav pane are gone, so a view's
list sits right above Folders), the account header (chevron, short name,
backend tag, marks, lock, count), the per-account fold
(`jodd.sidebar.accountsCollapsed`, shared), per-account Recently Deleted, the
Agents group in the tree (the AI ✦ sheet now lists only the AI menu), and the
Lamp Mark rail on the open row. Rows are 48px; the account name, the fold
labels and the top-level folders' icons share one column.

What differs, and why:

| Phone | Why |
|---|---|
| The Tags header opens the Tags pane, not a pill cloud | Hundreds of tags at a 48px touch size cannot sit inline, and picking several needs a Select mode |
| Each view shows its meaning as a second line (`SMART_FOLDER_HINTS`) | Touch has no hover, so a tooltip never shows |
| The bottom bar (New folder · AI ✦ · New note) and the pane header's gear replace the footer; accounts are managed in Settings | Thumb reach, and the Android pattern (spec 2026-10-20 §5.1) |

**State on the phone.** The Sidebar now polls `count_pending_pushes` on the
phone too, every 5 s, SQLite only (gotcha #15), so the waiting ring shows
there. The phone's Folders pane is the Sidebar mounted only while that pane
shows, so the poll runs exactly while the headers are on screen.

### 6. Small fixes made with it

- Desktop hit areas: the fold toggles and the header's `+` and `‹` are 28px
  (DESIGN.md's desktop minimum).
- A crowded account header gives way from the backend tag first (it shrinks
  ten times faster than the name), so the name keeps its letters.
- The phone's "VIEWS· a" label (missing space) is gone with the label.
- A Local Folder account gets its account colour in Field Instrument and
  Midnight Line: the look rule matched `data-backend='localfs'`, but the
  account carries `local_fs` (Rust's snake_case), so it matched nothing.
  `tokens.test.ts` now checks each selector against the kinds `backendLabel`
  names; the old check passed with the bug in place.
- In Midnight Line, an account's Recently Deleted row is its own folder group,
  so its station sits on the account's line.

### 7. The tablet's ☰ says when an account needs the user

*Added 2026-11-13 at the owner's request (a follow-up of this spec); refused
changes added the same day, also at the owner's request.*

While the folders are out of sight (the portrait drawer closed, the wide
column collapsed), an account's header and notice are hidden too. ☰ then
carries the Settings gear's danger dot (`.pane-dot`'s shape) whenever an
active account's header would carry its alert mark — the same rule,
`accountHeaderMark(accountDotState(…)) === 'alert'`, so the two never disagree:
signed out, blocked, or changes the backend refused for good. Changes still
waiting do not light it. Its accessible name and title say which and why:
"Folders — kai · Outlook is signed out", "… is not syncing" (another block),
"… has 2 changes it can't send" (refused), "Folders — 2 accounts need you".
With the folders shown the dot goes: the header says it there.

Refused changes are not on the account, so ☰ reads `count_pending_pushes`
(SQLite only, gotcha #15) every 5 s — the worker refuses on its own and emits
nothing (gotcha #6). The Sidebar polls the same count while it is mounted, so
☰ polls only while the folders are hidden, and forgets its counts when they
show; a failed read shows no dot. Tests: `tabletChrome.test.ts`.

## Keyboard and screen reader

- The column keeps one Tab stop (navRows). The picker is a button with
  `aria-haspopup="menu"`; its rows are `menuitem`s (↑/↓ move, Esc closes and
  returns focus), the account shown carries `aria-current`.
- The phone account header carries `aria-expanded`; its chevron is its own
  button ("Collapse a · Gmail").
- Each view row on the phone keeps its name as the row's text; the hint is
  the second line, read after it.

## Out of scope / follow-ups

- All accounts in Views and Tags (§3): proposed in
  `2026-11-13-all-accounts-scope-design.md`.

## Tests

`Sidebar.test.ts` (Views holds the four views only; per-account Recently
Deleted, none on Microsoft; the picker changes Tags and not the list, and is
remembered; a same-named tag; one account, no picker; Tags is one fold; the
filter, Thai included, and the cap; 28px hit areas), `sidebarPhone.test.ts`
(the phone order, header, lock and backend tag; Views in place with hints; an
account folds; the waiting ring from the SQLite count; Tags follows the
picker; Agents in the tree and not in the AI sheet; the Tags pane filter),
`sidebarPhoneSheets.test.ts` (the picker as a sheet on the phone and an
anchored menu on desktop), `sidebarFolds.test.ts` (stored folds, the legacy
Tags key, the scope's fallbacks), `phoneNav.test.ts` (no `smart` pane).
