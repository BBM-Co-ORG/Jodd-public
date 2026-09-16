# Account inactive status — design

> **Status:** design approved 2026-07-30, not yet implemented.
> **Scope:** a three-state field on `Account`, four enforcement layers, two new
> commands, one sidebar group. No changes to conflict handling or to any
> backend vertical.
>
> Roadmap item **0d**. Asked for on 2026-07-29: *"account should be able to set
> to inactive status so it is not available in any operation. not one can see
> and not other operation even background task but keep config intact."*

## Why

Two independent needs land on the same feature.

**Directly asked for.** An account you are not using still costs a sync worker
tick every 5 seconds, a slot in every scope selector, and its notes in every
cross-account search result. Removing it is too big a hammer: it wipes the
cache, the keychain entry and the label configuration, and re-adding means a
fresh OAuth round and a full re-index — 6,655 notes on the largest account
here.

**A prerequisite for [0c](../../../CLAUDE.md).** Changing an account's
`notes_label` or `meta_label` is refused outright today, because the sweep
reads a scope change as a mass deletion (`refuse_unsafe_label_change`, commit
da24c2a). Whatever the safe-relabel design turns out to be, it needs the
account to hold still while the labels move: no tick, no sweep, no concurrent
push, and — as this design ends up guaranteeing — nothing left in the outbound
queue.

## The shape of the problem

Jodd is a write-back cache. Edits land in SQLite synchronously and reach Gmail
whenever the worker gets to them; the local-first doctrine states it plainly —
*never wait on Gmail*. Deactivating an account is therefore not a switch, it is
a **quiesce**: stop accepting new work, finish flushing what is already queued,
then go quiet. Every write-back cache has that phase. Modelling it as one
boolean would have skipped it.

That framing decides most of what follows.

## Decisions

Recorded because none of them are recoverable by reading the resulting code.

### Three states, not a boolean

```rust
#[derive(Default)]
enum AccountStatus {
    #[default]
    Active,
    Draining,
    Inactive,
}
```

| state | worker | `vertical_for` | UI |
|---|---|---|---|
| `Active` | full sync | allowed | normal |
| `Draining` | **push only** — no pull, no sweep; flips itself to `Inactive` when the queue empties | allowed | hidden from the tree; listed under Inactive as *"finishing sync — 3 left"* |
| `Inactive` | skipped | **refused** | listed under Inactive, silent |

Pressing Deactivate enters `Draining` immediately. The account disappears from
view at once — the user's action never waits on the network — while queued
edits still travel. The state machine is one-way per user action: only the user
moves `Active → Draining` and `Inactive → Active`; only the worker moves
`Draining → Inactive`.

A boolean was the first draft. It forced a choice between stranding unsent
edits and blocking the user's click on Gmail, and neither is acceptable. Three
states dissolve that: the click is instant *and* nothing is stranded.

`#[serde(default)]` with `#[default] Active` keeps every `accounts.json`
written before this feature parsing as active.

### `Inactive` is a guarantee, not just a label

Because the only route into `Inactive` is the worker draining the queue to
empty, an inactive account has nothing pending — by construction, not by
convention. Three things fall out:

- Removing an inactive account destroys nothing, so it needs no warning.
- 0c gets the precondition it wants for free: a quiet account with an empty
  outbound queue.
- Anything found pending on an `Inactive` account is a bug, and can be asserted
  as one.

### Enforcement goes at `vertical_for()`

51 Tauri commands take an `account_id`. Guarding each is 51 places to get right
now and one more to remember for every command added later.

`vertical_for()` (lib.rs) is already the single door to every backend
operation: it resolves the account, dispatches on `backend_kind`, and returns a
`Box<dyn Vertical>`. Nothing reaches Gmail or a LocalFS vault without it — the
invariant the Vertical #0/#1 work established (CLAUDE.md edge #1).

It refuses `Inactive` only. `Draining` must pass, or the drain cannot happen.
This is the one place the three-state model costs something: the gate is a
match rather than a blanket refusal. It is worth it, and the cost is one
function.

### Cached data stays

Deactivating hides an account; it deletes nothing. Notes, folders, tags, edges
and FTS rows remain, so reactivating is instant rather than a full re-index,
and Jodd-local state with no remote representation — pins and tags whose
sidecars may not exist yet — cannot be lost by toggling a switch. The cost is
disk, the cheap resource here.

## Architecture

Four layers, because "inactive" means something different to each part of the
app that has to honour it: the backend must refuse, the worker must drain then
skip, the cross-account queries must exclude, and the UI must hide.

### 1. Backend operations — `vertical_for()`

```rust
if account.status == AccountStatus::Inactive {
    return Err(format!("account {account_id} is inactive"));
}
```

Covers every command that touches Gmail or a LocalFS vault in one place.

### 2. The worker — drain, then skip

The worker already locks `state.accounts` at the top of each tick. It now
partitions:

- `Active` — unchanged behaviour.
- `Draining` — run the push and delete drains only. Skip the poll, the sweep
  and the folder settle: pulling remote state into an account the user has
  dismissed would be work nobody asked for, and the sweep is the very thing
  that must not run while 0c later moves labels. When `list_dirty`,
  `list_deleted_pending`, `list_pin_dirty` and `list_tags_dirty` are all empty
  for the account, set `Inactive` and log the transition.
- `Inactive` — not touched at all.

### 3. Cross-account reads — an exclusion list

Four queries span accounts and must stop returning rows for an account the user
has dismissed. `Draining` and `Inactive` are both excluded: the account is gone
from the user's view the moment they press the button, and a note that
reappears in search for another thirty seconds would read as a bug.

| `db.rs` | used by |
|---|---|
| `search_notes` | the search box, all-accounts scope, `jodd-mcp` |
| `list_recent_notes` | Ask Jodd's recency prior |
| `count_notes_in_scope` | Ask Jodd's "N in scope" honesty line |
| `list_notes_with_tags` | the cross-account tag filter |

Each takes the ids to exclude. **When the list is empty the SQL is left exactly
as it is today** — no added `NOT IN (…)`, no added binding. `search_notes` runs
on every navigation, and CLAUDE.md edge #8 records that path being deliberately
left alone once already; the common case must stay free.

### 4. Visibility — a derived store

`list_accounts` is unchanged and keeps returning every account with its status.
The frontend derives `activeAccounts` and uses it everywhere except the
sidebar's Inactive group. No existing command changes meaning — the failure
mode `LlmProviderKind::None` demonstrated when it silently redefined itself.

## New commands

```
set_account_status(account_id: String, status: AccountStatus) -> Result<Account>
count_pending_pushes(account_id: String) -> PendingPushes
```

```rust
struct PendingPushes { notes: usize, deletes: usize, pins: usize, tags: usize }
```

`count_pending_pushes` filters the existing worker-drain queries by account. It
has no side effects and drives the "N left" display and the remove
confirmation.

`set_account_status` accepts the transitions the UI offers — `Active →
Draining`, `Inactive → Active`, and `Draining → Inactive` for the give-up
escape below — and rejects anything else, so an unexpected call cannot land an
account in a state the worker will not move it out of.

## Flows

### Deactivating

The control lives in **Account Settings (⚙)**, beside the other per-account
settings, not in a right-click menu — deactivating should not be one mis-click
away.

There is no confirmation dialog and no pending-work warning. Nothing is lost,
so there is nothing to warn about: the account enters `Draining`, leaves the
picker, the folder tree and both scope selectors immediately, and its queued
edits keep flowing. If it was `$currentAccount`, selection moves to the first
active account.

Local-first order applies (doctrine D3): snapshot, flip the store, invoke, roll
back on failure.

### The Inactive group

Collapsed by default at the foot of the account panel: `▸ Inactive (2)`.
Expanded, each account is dimmed and shows:

- `Draining` — *"finishing sync — 3 left"*, plus **Stop waiting**. No Remove;
  see below.
- `Inactive` — the name only, plus **Reactivate** and **Remove**.

No ⚙. An inactive account's settings are not editable; the point of the state
is that nothing about it is in motion.

### Stuck draining

A `Draining` account whose pushes keep failing — expired token, revoked
access, no network for a week — would otherwise wait forever. **Stop waiting**
forces it to `Inactive` and states the cost plainly: *"3 edits will stay on
this device and will be sent if you reactivate."*

This is the one path that leaves work unsent, and it is explicit, user-chosen,
and reversible by reactivating. That is the difference between it and the
first draft, where every deactivation could strand work quietly.

### Reactivating

Sets `Active`, then runs the warm-up cold start performs for a visible account:
`index_account` followed by `sync_pin_state`. An account may have been off for
weeks; its cache is reconciled before the user browses it rather than drifting
into place over later ticks. Anything left unsent by **Stop waiting** is still
`dirty` in SQLite and drains normally from the next tick.

### Removing

**Offered only in the `Inactive` state.** A draining account has no Remove
button; to remove one now, press **Stop waiting** first — instant, and it
already names the cost — and then Remove.

The reason is the order of operations inside `remove_account` (lib.rs:506):
`delete_refresh_token` runs *first*, before the account leaves
`state.accounts` and before the cache is wiped. Removing a draining account
would therefore pull the credential out from under a drain that is still
pushing: the in-flight request fails on auth, and every queued push after it
has no token to use. The unsent edits are destroyed either way, but this
version destroys them while generating auth failures that look like a
different bug.

Restricting Remove to `Inactive` costs the user one extra click and buys an
invariant: **removal is only reachable from a state whose queue is empty by
construction**, so `remove_account` cannot race a live drain. That is stronger
than remembering to be careful.

`remove_account` enforces this itself — it refuses a `Draining` account rather
than trusting the button to be absent. A hidden control is guidance; a refused
command is an invariant, and this one protects a credential deletion that
cannot be undone. `Active` is unaffected: removing a live account stays exactly
as it is today.

The narrow window that remains — Stop waiting pressed while a push sits
mid-await, then Remove immediately — is the case `remove_account`'s own
comments describe at lines 518-523 and already handle by wiping the `pushing`
entries explicitly. Nothing new is needed.

An earlier draft went the other way and required *reactivating* before
removing, reasoning that `remove_account` should not run against an account the
worker is not tending. That was backwards: none of its six steps needs a
running worker, and reactivating restarts the sync only to push edits to Gmail
seconds before deleting all of it anyway.

`Inactive` needs no pending-work confirmation: the queue is empty, so there is
nothing to lose. `count_pending_pushes` is used for the draining account's
"N left" display and by **Stop waiting**, not here.

## Edge cases

**Every account inactive.** `isAuthenticated` must stay bound to
`$accounts.length > 0` — all accounts, not active ones. Binding it to active
accounts sends the user back to the sign-in screen while they still have
accounts configured and, worse, puts the Inactive group out of reach: the only
way back becomes editing `accounts.json` by hand.

**A push already in flight when Deactivate fires.** It holds its own vertical
and runs to completion — and under this design it is not even an exception, it
is the drain doing its job. `vertical_for` still admits `Draining`.

**A conflict raised during the drain.** The drain pushes; it does not poll, so
it cannot discover a remote change. A conflict can therefore only arise from a
push whose remote id moved — handled by the existing `reconcile_one`
keep-both path, which writes a conflict copy locally. That copy is itself
dirty, so it joins the queue and the account keeps draining until it too is
sent.

**`jodd-mcp`.** It calls `jodd_lib::db::Db` directly and knows nothing about
accounts. Left alone, deactivating in the app would still leave those notes
searchable from any Claude Code session — a hole in "not one can see". It will
read `accounts.json` via `jodd_lib::accounts` and exclude non-`Active`
accounts. The `Db::search_notes` signature change breaks its build until this
is done, which is the intended forcing function.

## Testing

Rust:

| test | guards |
|---|---|
| `existing_accounts_json_parses_as_active` | the upgrade path — the one failure that would affect every install |
| `vertical_for_refuses_inactive_but_admits_draining` | the gate distinguishes the two, or the drain cannot run |
| `worker_drains_then_flips_to_inactive` | the state machine terminates |
| `worker_skips_pull_and_sweep_while_draining` | a dismissed account is not pulled back |
| `worker_does_not_touch_an_inactive_account` | quiet means quiet |
| `cross_account_search_excludes_draining_and_inactive` | notes leave the results at the moment of the click |
| `search_is_untouched_when_every_account_is_active` | the common case pays nothing |
| `count_pending_pushes_counts_all_four_kinds` | the "N left" figure is true |
| `set_account_status_rejects_an_unoffered_transition` | no path into a state the worker will not move out of |
| `remove_account_refuses_while_draining` | the credential is never deleted out from under a live drain — enforced, not merely un-clicked |

Frontend, as pure modules beside the components (the `askScope.ts` /
`llmFormDirty.ts` pattern):

- `activeAccounts` filters both non-active states
- `all_accounts_inactive_keeps_the_user_signed_in`

## Out of scope

Deliberately excluded: auto-deactivation when a token expires; scheduled or
temporary deactivation; a reason attached to the deactivation; and a separate
"stop syncing but keep it visible" mode. That last is a different feature with
a different shape — if it is what turns out to be wanted, it deserves its own
design rather than a flag bolted onto this one.
