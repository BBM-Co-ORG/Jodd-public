# Debounce note push (settle window + max-defer cap) — design

Date: 2026-07-21
Status: proposed
Scope: `src-tauri/src/lib.rs` sync-worker dirty-drain only

## Context / problem

The sync worker drains content-dirty notes on every 5 s tick
(`SYNC_INTERVAL`, `lib.rs:3014`) with **no debounce**: the `list_dirty()`
loop (`lib.rs:3311`) pushes *every* dirty note immediately via
`push_one_dirty` → `gmail::save_note`, which is **insert-new + trash-old**
(Gmail has no REPLACE; `wire.rs:1203`).

Because the editor autosaves on change (`apply_local_edit`, which stamps
`last_local_modified_at = now` every call — `db.rs:1024/1061`), a note under
continuous editing flips back to `dirty` between ticks and is **re-pushed
once per 5 s tick**. One minute of continuous editing ⇒ ~12 insert+trash
cycles ⇒ ~12 Gmail messages created (11 trashed) for a single logical edit.

This churn has two observed consequences:

1. **Apple Notes IMAP sync wedges** on high-churn mailboxes (per-mailbox,
   per-device). Confirmed 2026-07-21: `Notes/__Extracts__` and
   `Notes/Personal/มนต์` (high APPEND/EXPUNGE volume) lagged/wedged while the
   low-churn `Notes` root synced fine. Recovery required a manual Notes
   off/on resync. Root cause of the wedge is churn *volume*, not message
   content (proven: the exact "stuck" notes synced fine after resync, and a
   brand-new note synced within minutes once the wedge cleared). See memory
   `apple-notes-sync-confusion`.
2. **Orphan duplicate accumulation.** Rapid successive pushes can leave an
   intermediate insert un-trashed (a push's `existing_gmail_id` goes stale
   relative to the previous push's result), stranding a copy. Evidence: 6
   uuid clusters from a single 3 Jul editing burst, 3–5 copies each, created
   within ~1 minute, still lingering weeks later (cleaned up manually
   2026-07-21). Note: `trash-old` itself never *failed* (0 failures in the
   log) — the orphan is born from trashing an already-stale id, not from a
   failed trash.

Both problems share one root cause: **redundant push cycles per edit.**

## Goal

Cut the number of insert+trash push cycles for an actively-edited note from
"one per 5 s tick" to "one per edit-burst" (plus a periodic cap), thereby
reducing Gmail mailbox churn (→ fewer Apple-Notes wedges) and shrinking the
window in which the stale-`existing_gmail_id` orphan race can fire.

### Non-goals

- Not changing `save_note`'s insert-new/trash-old mechanic (inherent to
  Gmail's no-REPLACE API).
- Not adding a durable trash-retry queue — `trash-old` does not fail in
  practice (0 failures observed), so retry would prevent ~0 orphans.
- Not re-enabling automatic orphan cleanup (`safe_cleanup_orphans` stays
  manual). Debounce reduces orphan *formation*; the existing manual
  duplicate-review UI handles any residue.
- No settings UI / no user-configurable thresholds (hardcoded constants,
  matching the existing hardcoded staleness threshold etc.).
- Not touching folder / deletion / pin / tag drains — deletions especially
  must still propagate promptly; they are not a churn source.

## Design

Add a **settle-window filter** in front of the existing content-dirty drain
loop. Leave everything else (in-flight `pushing` set, ordering, `push_one_dirty`,
`mark_pushed`) untouched.

A dirty note is pushed on a given tick iff **either**:

- **Settled** — it has been quiet for at least `PUSH_SETTLE_MS`:
  `now - last_local_modified_at >= PUSH_SETTLE_MS`. (The user has paused
  editing, so the note is stable and worth one push.)
- **Overdue** — it has already been synced at least once and it has been too
  long since that last successful sync:
  `last_synced_at` is `Some(s)` and `now - s >= MAX_DEFER_MS`. (Safety valve
  so a note edited continuously for minutes still reaches Gmail/Apple
  periodically instead of never.)

Otherwise the note is **skipped this tick** and re-evaluated on the next one.

### The predicate (extracted, pure, unit-testable)

```rust
/// Should this content-dirty note be pushed on the current tick?
/// `last_synced_at == None` ⇒ never synced (brand-new note): it can only
/// become due via the settle branch, never the overdue branch — a
/// never-synced note that is *continuously* edited defers until the user
/// pauses. That is safe: SQLite is the source of truth and survives restart,
/// so a deferred never-synced note is never lost, only not-yet-mirrored.
fn note_push_due(
    now_ms: i64,
    last_local_modified_at: i64,
    last_synced_at: Option<i64>,
    settle_ms: i64,
    max_defer_ms: i64,
) -> bool {
    let settled = now_ms - last_local_modified_at >= settle_ms;
    let overdue = matches!(last_synced_at, Some(s) if now_ms - s >= max_defer_ms);
    settled || overdue
}
```

Wiring in the drain loop (`lib.rs:3311`):

```rust
for n in dirty {
    if !live_accts.contains(&n.account_id) { /* unchanged */ continue; }
    if !note_push_due(now_ms(), n.last_local_modified_at, n.last_synced_at,
                      PUSH_SETTLE_MS, MAX_DEFER_MS) {
        continue; // still settling — re-evaluate next tick
    }
    // ...unchanged: mark pushing, push_one_dirty, unmark, log
}
```

`CachedNote` already carries both `last_local_modified_at` and
`last_synced_at` (populated by `list_dirty()`), so no schema or query change
is needed.

### Constants (near `SYNC_INTERVAL`, `lib.rs`)

```rust
const PUSH_SETTLE_MS: i64 = 5_000;   // ~one tick of quiet before pushing
const MAX_DEFER_MS:  i64 = 60_000;   // force a previously-synced note through ≥ once/min
```

Rationale: `PUSH_SETTLE_MS = 5 000` (one `SYNC_INTERVAL`) makes an
actively-typed note wait for a ~5–10 s pause before its single push;
`MAX_DEFER_MS = 60 000` caps worst-case churn for a note edited nonstop at
one push/minute (a ~12× reduction from one/5 s) while keeping other devices
reasonably fresh.

## Why this preserves the local-first doctrine

The debounce is on the **Gmail push** (already a background, invisible step),
**not** on the SQLite write. `apply_local_edit` still commits synchronously to
SQLite and the DOM updates optimistically — the user sees "Saved" immediately
and never waits on Gmail. Only the *mirror* to Gmail waits for a quiet moment.

## Edge cases

- **Never-synced note, continuous edit:** defers until a pause (settle
  branch only). Safe — SQLite holds it; on restart `last_local_modified_at`
  is old ⇒ it pushes. Documented in the predicate.
- **App closes while a note is settling:** no push happens; SQLite retains
  the edit; next launch `list_dirty()` returns it with an old
  `last_local_modified_at` ⇒ pushed on the first eligible tick. No data loss.
- **A note dirtied once and left:** first tick likely skips (within settle),
  next tick (≥5 s later) pushes. ~5–10 s added latency, invisible per
  doctrine.
- **Conflict / deletion / pin / tag states:** untouched — separate drains,
  not filtered.
- **Clock skew / `now - t` negative:** a negative delta simply reads as
  "not yet settled / not overdue" ⇒ skip; self-corrects next tick. No panic
  (plain `i64` subtraction).

## Testing

**Unit (pure predicate) — the core coverage:**

| case | last_local_modified | last_synced | expect |
|---|---|---|---|
| quiet, synced recently | now-6s | now-6s | **push** (settled) |
| actively editing, synced recently | now-1s | now-2s | skip |
| editing nonstop, overdue | now-1s | now-70s | **push** (overdue) |
| never synced, actively editing | now-1s | None | skip |
| never synced, quiet | now-6s | None | **push** (settled) |
| boundary: exactly settle_ms | now-5s | now-5s | **push** (>=) |

**Manual / integration (verify churn drop):** edit a note continuously for
~1 min with `jodd.log` open. Before: ~12 `save_note` lines for that uuid.
After: ~1 (on pause) or ≤2 (one at the 60 s cap + one on pause). Confirm the
note still ends up correct in Gmail and Apple Notes.

## Rollout / risk

Single-file, ~10-line change plus one pure function and its tests. No
migration, no schema change, no frontend change, backward compatible. Risk is
low: the worst failure mode (predicate too conservative) merely delays a Gmail
push slightly; SQLite durability guarantees no data loss.

## Out of scope (possible follow-ups)

- Flush-all-dirty on graceful shutdown (currently deferred to next launch —
  acceptable, SQLite-durable).
- Making `MAX_DEFER`/`SETTLE` configurable per account.
- Re-enabling automatic `safe_cleanup_orphans` (separate decision; debounce
  is expected to make it largely unnecessary).
