# Per-account Ingest trigger — design

> Status: **design / approved** (2026-07-20). Moves the "💡 Ingest source"
> trigger from a single global sidebar row to a per-account button, so the
> target account is unambiguous by construction. Follows on from
> [docs/superpowers/specs/2026-07-10-extract-ingest-entrypoint-design.md](2026-07-10-extract-ingest-entrypoint-design.md)
> (Phase 1), which placed the original global trigger.

## Problem

The current "💡 Ingest source" row (`Sidebar.svelte:1083-1091`) is a single
button above the account list, gated only on `$currentAccount` being
non-null — it submits against whichever account happens to be selected,
with no visual confirmation of which one that is. During live testing of
this session's citations feature, this caused a real mistake: an Extract
was submitted against a local test folder when the intent was a Gmail
account, because nothing in the button itself showed which
account was current. With two-plus accounts in regular use, this is a
standing correctness risk, not a one-off.

## Decisions (locked in brainstorming)

1. **Placement: per-account, in the folder-tree account header** — a new
   icon button on each account's `account-header` (`Sidebar.svelte:1096-1111`,
   next to the account email / dup-pill), not in the compact bottom
   account-switcher row next to ⚙/✕. Phase 1's own spec rejected that
   bottom-row placement for "too little visual weight for the action's
   importance" and "implies a relationship to account settings that doesn't
   exist" — that objection still holds today, so the button goes where the
   user is already looking when browsing that account's folders, not into
   the settings-feeling cluster.
2. **Old global row removed entirely**, not kept as a secondary path —
   matches Phase 1's own precedent when it moved the trigger out of the
   account-CRUD cluster in the first place. One way to ingest, always
   explicit about the target account.
3. **Icon-only**, with a `title` tooltip naming the account — mirrors the
   existing icon-only ⚙/✕ treatment elsewhere in the sidebar. No visible
   label text needed once the button is already inside that account's own
   section.
4. **Account-switch side effect kept minimal** — clicking a different
   account's button sets `$currentAccount` if it differs, nothing else.
   Matches the existing lightweight account-pick handler
   (`Sidebar.svelte:1298`, `currentAccount.set(a.id)`), which likewise
   doesn't clear tags/smart-folder selection on a plain account switch —
   no new clearing behavior invented for this change.

## Approach

### 1. Remove the global trigger

Delete the `.ingest-row` button (`Sidebar.svelte:1083-1091`) and its CSS
rules (`.ingest-row`, `.ingest-row:hover:not(:disabled)`,
`.ingest-row:disabled`, `Sidebar.svelte:1535-1556` region).

### 2. Add the per-account trigger

In the `account-header` block (`Sidebar.svelte:1096-1111`), add a new
button after the account email, before the dup-pill:

```svelte
<div class="account-header" title={accountDisplay(acct)}>
  {#if acct.backend_kind === 'local_fs'}
    <span class="account-kind-icon" title="Local folder account">📁</span>
  {/if}
  <span class="account-email">{accountDisplay(acct)}</span>
  <button
    type="button"
    class="account-ingest-btn"
    onclick={() => {
      if ($currentAccount !== acct.id) currentAccount.set(acct.id);
      extractModalOpen.set(true);
    }}
    title="Ingest a source into {accountDisplay(acct)}"
  >💡</button>
  {#if cleanupResult[acct.id]}
    ...
```

`extractModalOpen` is already imported (`Sidebar.svelte:8`); no new store
needed. `LessonExtractModal.svelte` already reads `$currentAccount` at
submit time (unchanged) — since this button guarantees `$currentAccount`
is the clicked account *before* the modal opens, the modal targets the
right account with no changes to it or to any backend command.

### 3. New CSS

Small icon button matching the visual weight of `.dup-pill`/existing
account-header children — exact values (padding, hover state, color) left
to implementation to match the file's established icon-button pattern
(`.account-row-settings`/`.account-row-remove`, `Sidebar.svelte:1984-2006`)
rather than inventing a new visual language.

## Error handling / edge cases

- **Account not ready** (e.g. a `local_fs` account whose directory is
  temporarily unavailable): no new gating added. The button behaves like
  any other per-account action in this file (e.g. selecting a folder) —
  if the account isn't usable, that surfaces the same way it already does
  elsewhere, not a new failure mode this change needs to handle.
- **Rapid clicks across two different accounts' buttons**: `currentAccount`
  is a plain `writable`, last write wins; `extractModalOpen.set(true)` is
  idempotent (already-open stays open). No race beyond what already exists
  for any two rapid store writes elsewhere in this file.

## Testing

No vitest coverage for Svelte component changes in this codebase (matches
Phase 1 and this session's convention) — manual/live verification:
- Confirm the global "Ingest source" row is gone from the top of the
  sidebar.
- Each account section shows a 💡 button in its header.
- Clicking Account A's button while Account B is current switches to A
  and opens the modal; submitting targets A (visible in the created note's
  account).
- Clicking the current account's own button opens the modal without
  triggering an unnecessary account switch (re-render/flicker check).

## Scope / files

- `src/lib/components/Sidebar.svelte` — remove `.ingest-row` + its CSS,
  add the per-account button + its CSS.
- No changes to `LessonExtractModal.svelte`, backend commands, or the
  destination-picker logic — trigger-placement only, same scope boundary
  Phase 1 used.

## Deferred (not built)

- **Ingest from an existing note** (a new source-input mode picking a note
  instead of pasting text) and **real multi-page distribution on ingest**
  (Karpathy's actual "touches 10-15 wiki pages" ingest behavior, vs. today's
  one-page-per-source) — both explicitly out of scope for this change, to
  be brainstormed together as a follow-on (see
  [docs/LLM-WIKI-GRAPHIFY-ROADMAP.md](../../LLM-WIKI-GRAPHIFY-ROADMAP.md)
  item #5). A related "cleanup/digest" idea (a periodic pass that lets
  accumulated raw material "digest," reorganize, and surface answers over
  time — going beyond the already-shipped Orphaned/Stale lint into active
  synthesis) was also raised and is noted for that same follow-on session,
  not designed here.
