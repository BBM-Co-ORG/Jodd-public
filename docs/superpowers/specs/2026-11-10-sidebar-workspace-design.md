# Sidebar: one workspace, many doors (2026-11-10)

Source: the Impeccable critique of `Sidebar.svelte` on 2026-11-10
(`.impeccable/critique/2026-11-10T03-55-10Z__src-lib-components-sidebar-svelte.md`,
24/40). The owner chose the full structural change and asked for it to be
finished without further questions, so the decisions below were made in this
document rather than asked.

## The problem

Every account carried its own copy of the same chrome: a header, an "All
<address>" row, the Notes tree, a Views group (four Smart Folders and Recently
Deleted) and a Tags section. Five accounts made about 45 rows of chrome, 20 of
them identical Smart Folder rows, before any folder the user made. The state
that matters most per account (signed out, blocked, unsent changes, read-only)
lived in one footer chip that only ever described the current account.

## Decisions

1. **Views and Tags belong to the workspace, shown once.** One Views group
   (the Smart Folders and Recently Deleted) and one Tags section sit at the
   top of the column. They act on the *current account*: the account of the
   last thing picked. With more than one active account the Views header
   names it (`Views · work`), so the scope is never a memory task.
   - **Not built: an all-accounts aggregate.** Every list, Smart Folder query,
     tag paint and the editor's save path is keyed on one `currentAccount`
     (`controllers/viewSelection.ts`, the `list_*_notes` commands). A list that
     mixes accounts touches the save path, which PRODUCT.md principle 1 puts
     above any visual change. That is a separate change with its own spec.
2. **The account header is the account's row.** Clicking it opens every note
   in the account (the old "All <address>" row, now gone, as the phone already
   did). Its chevron collapses the account; ←/→ do the same from the keyboard.
   The header shows the short name (`accountShortName`: the address's local
   part, or the folder name for a Local Folder; the full address is its
   title), the backend, the note count, and the account's own state.
3. **State lives on the account it describes.** Following principle 2 (sync is
   invisible until it needs the user):
   - signed out or blocked: a danger dot on the header, and a row under it
     with the reason, plus **Sign in again** for Gmail and Microsoft;
   - changes waiting: a hollow muted ring, named "Syncing — N changes
     waiting";
   - synced, or not yet known: nothing;
   - folders read-only for this backend (Microsoft): a lock with its reason.
   Desktop and tablet poll `count_pending_pushes` (SQLite only, gotcha #15)
   for every active account on the 5s cadence the chip used. The phone polls
   nothing; signed-out and blocked come from the account itself.
4. **The current account is marked.** Its header is in full ink; the others
   are secondary. The footer chip still names it.
5. **Duplicates move under the header.** The "N duplicates" pill was a button
   inside the header; with the header now a button itself, it becomes its own
   notice row under it.
6. **The phone follows the same order.** Folders pane: Tags, Smart folders and
   Recently Deleted once at the top for the current account, then the accounts
   with their state, then the trees.

## Keyboard and screen reader

- The column keeps one Tab stop (navRows). Account headers join the rows at
  depth −1, so ← from a top-level folder lands on its account.
- Each account's rows sit in a `role="group"` named for the account.
- Menus take focus when they open, move with ↑/↓, and hand focus back to the
  row when Esc closes them. Move to opens on click, Enter or →.
- Per-account buttons name their account ("Remove work · Gmail").

## Out of scope

The all-accounts aggregate (decision 1), nameable accounts (the critique's
third question), and a folder filter.
