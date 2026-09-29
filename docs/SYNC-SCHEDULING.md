# Package G — sync scheduling evidence (2026-09-20)

Baseline: `main` / `ae96d9a`. Synthetic host evidence only; no real notes,
providers, credentials, account settings or wire protocols exercised/changed.

## Boundaries and preserved contracts

- `note_commands.rs`: save/status IPC adapters, capability and AI eligibility
  checks. `note_mutations.rs`: SQLite save/identity/version service.
- `reconcile.rs`: in-flight exclusion, keep-both and remote reconciliation.
- `sync_worker.rs`: folder → content → deletion → pin → lifecycle → due detector
  sequence, **within each account**. No DB transaction was split. Its push-result
  and failure classifiers, aliases, blocked-queue rules and protocol calls remain.
- `sync_schedule.rs`: admission only. Shared by periodic rounds, `flush_sync`,
  immediate LocalFS saves and account teardown. Queue snapshots are read **after**
  admission. A flush waits, then rereads; it does not replay the previous snapshot.
- Frontend `controllers/refreshQueue.ts` owns latest-intent coalescing behind
  saves/reads, and disposal. `controllers/noteMutations.ts` and `noteStatus.ts`
  own the existing optimistic-delete and identity/version/status behavior; old
  imports remain compatibility exports. Existing mounted C regressions cover them.

The background driver starts a round every five seconds independently of other
accounts' completion. A queued/running account coalesces periodic requests.
Explicit flushes serialize on the same account lane. There are **two remote
account-round slots and one separate LocalFS slot**, including immediate file
saves. A waiter for A never consumes a second capacity slot. The local slot cannot
be held by a remote round, so local file saves cannot wait for network capacity.
Teardown acquires the account lane without a transport slot and rechecks status
before removal; Draining removal requests still queue immediately. Weak lane
entries are reclaimed on the next admission after holders/waiters disappear.

FIFO semaphore admission serves queued C before a later D when capacity frees.
With A held indefinitely, B/C/D progress on the second slot. This is conditional
fairness, **not an unconditional wall-clock latency SLA**: if both remote slots
are occupied by hung operations, a third remote account waits for a completion
or transport timeout. LocalFS still has its own slot. No timeout/abort-and-retry
policy was added: cancelling an ambiguous remote write is a separate protocol
question. The bounded unit is a whole account round, not an individual HTTP call.

`PushingGuard` now covers the initial UUID as well as the pre-existing rekey UUID
marker; failure, dropped future and unwind release in-flight entries. Events
remain at the worker's app boundary: persistence nudge → App listener → local
SQLite status read → account/version controller → mounted editor status. A push
receipt remains distinct from a remote-change nudge; neither claims a newer edit
has synced. Draining/removal still has the existing separate next-round proof.

## Reproducible measurements

Run:

```sh
cargo test --locked -p jodd --lib sync_schedule -- --nocapture
```

`measured_before_after_delayed_backend_workload` uses two delayed fake operations,
A then B, both enqueued at logical time 0. A's operation duration is 100 logical
ms; B's is 1 ms. Completion is controlled by oneshot channels and manual future
polling. No arbitrary sleep, real network, Tauri runtime or provider is involved.
One round per strategy, **two operation samples each (n=4 total)**. The clock is
explicitly advanced by the driver; these are reproducible scheduling observations,
not measured CPU/network latency, averages or performance estimates for users.

| Strategy | Account | Queue wait ms | Operation ms | End-to-end ms | Completion order |
|---|---|---:|---:|---:|---|
| Capacity 1, old global-exclusion characterization | A | 0 | 100 | 100 | 1 |
| Capacity 1, old global-exclusion characterization | B | 100 | 1 | 101 | 2 |
| Capacity 2, new production scheduler | A | 0 | 100 | 100 | 2 |
| Capacity 2, new production scheduler | B | 0 | 1 | 1 | 1 |

The baseline is a faithful model of the old global exclusion identified in
`ae96d9a:src-tauri/src/lib.rs::sync_worker_tick`, **not an invocation of the old
AppHandle-bound worker**. The production default was first set to one slot and
`delayed_account_does_not_block_another_and_capacity_is_bounded` failed with
“B must progress while A is delayed”; default two makes it pass. This red run
characterizes admission; it does not pretend to be a native app benchmark.
The deterministic progress bound is B admitted on its first poll while A's lease
remains held, with C refused capacity until B releases it. FIFO, same-account
waiters, repeated periodic calls, queued cancellation and idle lane cleanup have
separate assertions.

`overlapping_tick_flush_rereads_after_create_rekey_edit_and_delete` combines the
production scheduler with real temporary SQLite and production save/push-completion
services: hold CREATE, locally edit, queue flush, complete stale CREATE/rekey,
verify dirty v2 survives, UPDATE once under the assigned identity, reread an empty
dirty queue, then DELETE the correct remote ID. It is a synthetic scheduler/DB
integration test, not a live transport or whole Tauri worker test. Existing
push/reconcile/refusal/lifecycle suites supply the other transaction regressions.

## Folder scope

Ask and MCP use `folder_scope::matches` with explicit `Mode::Subtree`;
`Mode::Exact` states the navigation/search contract. SQLite's three recursive
read queries use literal `substr` comparison, tested against the same 23-case
fixture at `tests/fixtures/folder-scope.json` as Ask and MCP. Exact navigation and
FTS search are separately asserted and remain exact. Nothing changes the MCP
allowlist, account fallback, normalization or deny-by-default rules.

The old claim that all three implementations agreed was incomplete. The red DB
regression for `Notes/A` returned 5 rows versus 4 expected because SQLite LIKE
also admitted `Notes/a/x`. `%` and `_` in a scope were wildcards, while Rust
matched literally. G removes those SQL discrepancies; the MCP boundary is
unchanged. Cases include root/empty strings, descendants, sibling prefixes,
Unicode, composed/decomposed forms, backslash, spaces, repeated and trailing
slashes. No silent canonicalization is introduced. Folder rename SQL is outside
this read-scope consolidation and was not changed.

## Rendered evidence and limits

With Vite running at 127.0.0.1:1420 and an installed Playwright module:

```sh
JODD_PLAYWRIGHT=/path/to/playwright/index.mjs node tests/browser/package-g.mjs
```

Production App receives synthetic `note-persistence-changed` events and requests
local IPC. The editor changes Synced → pending → Synced; an event for B with the
same UUID cannot mark A synced. Browser requests to non-loopback hosts are blocked.
This verifies the JS event adapter through pixels, **not the packaged Tauri event
bridge**. The 800px desktop capture shows the status; the 360px desktop capture clips the
multi-pane layout and is not a narrow-layout/Android acceptance result. Existing
mounted tests cover rollback, newer typing, stale replies,
rekey and selection. Native Tauri/WebView, Android, live sync, provider usage,
Apple delayed reconciliation and real queue-age/latency distributions are unknown.
No iCloud protocol changes. F fixture replay is not used as model-quality evidence.
