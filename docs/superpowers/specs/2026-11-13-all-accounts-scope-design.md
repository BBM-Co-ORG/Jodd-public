# "All accounts" in the For ⌄ picker (2026-11-13)

**Status: Approved 2026-11-13.** The owner took the proposal on all three
open questions (§6): the default stays the current account, tag rename and
delete are not offered under All accounts, and the row is named "All
accounts". Built the same day. §7 records where the build departs from the
text above, and why.

**Builds on** `2026-11-12-sidebar-one-navigation-design.md` §3. That spec put
a "For ⌄" picker above Views and Tags to choose which account they show. It
left "All accounts" out on one stated ground: a list mixing accounts "touches
the save path", which PRODUCT.md principle 1 puts above any visual change.

## 1. The ground for deferring it no longer holds

The deferral cited the 2026-11-10 sidebar spec, decision 1: *"Every list,
Smart Folder query, tag paint and the editor's save path is keyed on one
`currentAccount`."* The code today says otherwise:

- **The editor saves to the note's own account.** `NoteEditor.svelte`
  `saveNote` sends `accountId = note.account_id || currentAccount`. The
  `currentAccount` fallback is reached only by a note with no account, and
  every note gets one at creation: `openNewNote(accountId, path)` writes
  `account_id`. Every other editor action does the same: refetch,
  conflict copy, rearm, tag writes and graph navigation all read
  `note.account_id ?? currentAccount`.
- **Mixed-account lists already ship, twice:**
  - search with **Search in: All accounts** (`searchScope = 'all'`);
  - a tag filter with **All accounts** in the list header (`tagScope = 'all'`
    → `list_cached_notes_with_tags(account_id: null)`).

  Their rows are keyed by `noteKey` (account + uuid), carry an account badge
  (`NoteList.svelte`, the badge under `$tagScope === 'all'`), and are opened
  and saved through the same editor.
- **Tapping a row from another account makes that account current**
  (`NoteList.followNoteContext`), so New note, the folder highlight and
  "Search this account" follow the note the user is in.

So "All accounts" for Views and Tags is a new *source* for the list. It does
not change how a note is saved. The real work is the five places below that
still assume one account.

## 2. What "All accounts" means

- **Which accounts:** every **active** account. That includes signed-out and
  blocked accounts, because their notes are on this device. Draining and
  inactive accounts are excluded, using the rule the cross-account tag read
  already applies (`hidden_account_ids`, `lib.rs`).
- **Views:** each of the four lists (Not linked, Untouched, Made with AI,
  Unreviewed) shows its notes from every such account, in the usual list
  order (pinned first, then newest).
- **Tags:** the cloud shows the union of every such account's tags. The same
  tag name in two accounts is **one** tag here, with the counts summed. That
  is what the existing cross-account tag filter already means by a tag
  (`list_cached_notes_with_tags(null)` matches by name). Picking a tag opens
  the cross-account tag filter (`tagScope = 'all'`), so the list and the
  cloud agree.
- **Not in scope:** folders, and Recently Deleted. They stay under each
  account, where they live.

## 3. Decisions

### 3.1 The picker

- **"All accounts" is the first row** of the For ⌄ menu (sheet on the phone),
  above the accounts, with a separator. With it chosen, the button reads
  **For All accounts**.
- It is offered only with 2+ active accounts, like the picker itself.
- It is stored like any choice (`jodd.sidebar.scopeAccount = '*all'`). It
  falls back to the current account if fewer than two accounts remain
  active.
- **The button shows no mark when All accounts is chosen.** Each account's
  header already carries its own state. A combined mark would repeat that
  and could not say which account it meant.

### 3.2 The scope store says "all" in its type

`stores/sidebarScope.ts` gets a discriminated value. A string sentinel would
let any existing `=== scopeAcct.id` comparison silently compare against
`'*all'`; this makes every consumer handle the new case, enforced by
`svelte-check`:

```ts
type Scope = { kind: 'account'; accountId: string } | { kind: 'all' };
export const scope: Readable<Scope | null>;
```

Likewise `selectedSmartFolder.account` becomes `string | null`, where null
means all accounts. Every reader is in five files today (`App.svelte`,
`viewSelection.ts`, `notes.ts`, `Sidebar.svelte`, `PhoneFolders.svelte`).
`notes.ts`'s "drop a Smart Folder whose account left" must keep a null
selection.

### 3.3 Views read every account in one IPC

The four Smart Folder commands (`list_orphaned_notes`, `list_stale_notes`,
`list_extract_notes`, `list_unreviewed_notes`), plus `list_unreviewed_trust`,
take `account_id: Option<String>`, following `list_cached_notes_with_tags`:

- `None` means every account not in `hidden_account_ids`;
- `Some` behaves exactly as today.

The `db.rs` queries gain the same `Option<&str>` and keep their `ORDER BY`.

- **Why the backend, not a frontend fan-out:** one place owns the rule for
  which accounts count, the one the tag read already uses. One IPC returns
  one ordered list, not N lists merged in JS.
- **Local-first:** all five remain pure SQLite reads.

### 3.4 Opening a note makes its account current, however it opens

A tap already does this (`followNoteContext`). The auto-open after a pick
does not: `selection.openFirstNoteFor` sets `selectedNote` only. In an
all-accounts list the first row can belong to another account. New note,
the folder highlight and "Search this account" would then point at an
account the user is not in.

**Rule:** whenever a note opens, from a tap or from the auto-open, its
account becomes current *in the same synchronous step*. This is gotcha #28:
the identity and the values compared against it are set in one turn. The
same fix also covers today's cross-account tag and search lists.

### 3.5 What stays per account, and says so

- **Tag rename and delete** act on one account's sidecars (`rename_tag`,
  `delete_tag`). With All accounts chosen, the tag menu shows one line
  instead: "Pick an account in For to rename or delete this tag."
  (Open question 2.)
- **New note** from a view or tag list goes where it goes today: the Notes
  folder of the current account, which §3.4 keeps as the account of the
  open note.
- **Unreviewed's review action** (`verify_note`) already takes the note's
  own account.

### 3.6 Naming and badges

- **List title:** the view's own name ("Not linked", `#music`). The
  subtitle, or the phone header's second line, reads **All accounts**.
- **Rows:** each row carries the account badge, already used by the
  cross-account tag and search lists. The badge condition widens to any
  all-accounts source.
- **Empty states:** unchanged (`SMART_FOLDER_EMPTY`). They already speak of
  notes, not of an account.

### 3.7 The phone

- The For ⌄ sheet gains the same first row.
- The Tags pane, opened with All accounts chosen, lists the union. A tap
  opens the cross-account filter. In Select mode, "Show N notes" counts
  across accounts, from `noteTagsByAccount` for every active account.
- `phonePaneAccount` gains the same `{ kind: 'all' }` value instead of a
  sentinel string.

## 4. What does not change

- How notes are saved, synced, pushed or merged. There is no Rust change
  outside the five read commands in §3.3.
- Ask Jodd's scope and MCP scope (gotcha #33). This is navigation, not an AI
  scope.
- The list header's own "All accounts" choices for search and tag filters.
  They keep working, and a tag picked under All accounts arrives with the
  header already showing All accounts.

## 5. Tests

- **Rust** (`db.rs`, `lib.rs`), for each of the five commands:
  - `None` returns every active account's rows, in order;
  - an inactive or draining account's rows are excluded;
  - `Some` is unchanged.
- **`sidebarScope`:** the store yields `{ kind: 'all' }` when chosen. It falls
  back with fewer than two accounts. It survives a reload.
- **`Sidebar.test.ts` and `sidebarPhone*.test.ts`:**
  - the picker shows All accounts first;
  - Views under All load with `accountId: null`;
  - the tag cloud is the union with summed counts;
  - a tag pick sets `tagScope = 'all'`;
  - the tag menu shows the one-line explanation.
- **`selection.test.ts`:** an auto-opened first note from another account
  makes that account current in the same turn.
- **`noteListing.test.ts`:** an all-accounts Smart Folder lists rows from two
  accounts, pinned first then newest, deduplicated by `noteKey`.

## 6. Decided by the owner (2026-11-13), as proposed

1. **The default with 2+ accounts.**
   - **Proposed:** the current account, as today. "All accounts" is a choice
     the user makes once and the app remembers.
   - **Alternative:** All accounts by default. This fits PRODUCT.md principle
     4 ("one workspace, many doors"), but changes what a multi-account user
     sees on upgrade.
2. **Tag rename and delete under All accounts.**
   - **Proposed:** not offered there; the user picks an account first.
   - **Alternative:** apply to every account that can write tags, and say
     which ones it skipped. That is one action writing to several backends.
3. **The row's name.** Proposed: **All accounts**, the words the list
   header's scope menus already use.

## 7. As built: three departures from §3

- **§3.3, how Views read every account.** The four commands take
  `account_id: Option<String>`, as proposed. `None`, though, does not
  reach new SQL: `lib.rs` `read_views` runs the existing per-account query
  once per account in `accounts_in_scope` (every active account) and joins
  the results. Order across accounts does not matter, because the list sorts
  every source itself (`noteListing.ts`). That left `db.rs` untouched, and
  `accounts_in_scope` is unit-tested.
- **`list_unreviewed_trust` is unchanged.** The Unreviewed chips' trust is
  already loaded for every active account (`App.svelte`'s
  `refreshUnreviewed(accountIds)`) and keyed by account and uuid, so an
  all-accounts list finds each row's chip without it.
- **§3.7, the phone Tags pane.** It reads a separate `phonePaneAllAccounts`
  flag beside `phonePaneAccount` rather than a `{ kind: 'all' }` value.
  `phonePaneAccount` keeps its type and its callers.

Two rules the build added:

- **Every sidebar tag pick sets the tag scope.** A pick under All accounts
  sets `tagScope = 'all'`; a pick under one account sets `'account'`
  (`viewSelection.useTagScope`). A change of scope starts a fresh selection,
  as a change of account always did. The list header's own scope menu can
  still widen or narrow afterwards.
- **An all-accounts view is the same view whichever account is current.**
  `viewSnapshot` leaves the account out of it, and the cross-account tag
  read no longer refetches on an account change. Opening one of the view's
  notes switches the account (§3.4), and that must not cancel the view's
  load or blank its list.
