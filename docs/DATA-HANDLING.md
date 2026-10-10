# Jodd — Data Handling Design Reference

This document describes how Jodd stores, reads, writes, and synchronizes
note and folder data. It's the canonical reference for the **data model and
sync rules** — the "source of truth" of the system per our architecture.

## 1. Architectural roles

There is **no single master**. Each component plays a distinct role:

| Component | Role | Identity |
|---|---|---|
| **SQLite** (in app-data dir) | Local working replica. Reads/writes happen here first. | Per-install. |
| **Gmail** | Shared sync hub. All replicas exchange state through it. | Per-account. |
| **Apple Notes / iOS Notes / Gmail web** | Other replicas. Each has its own local store + sync logic. | Per-device. |

**The truth is the data model + sync rules**, not any single storage layer.
If a write reaches Gmail, that's the version other replicas will eventually
converge on. A local-only write that hasn't synced is *also* valid state for
this replica — we just track that it's `dirty` until pushed.

## 2. Per-note state machine

Every cached note carries a `sync_state` value:

```
        ┌──────────┐
        │  clean   │ ── local matches the last server state we know
        └────┬─────┘
             │
         local edit
             │
             ▼
        ┌──────────┐
        │  dirty   │ ── local was edited; worker will push to Gmail
        └────┬─────┘
             │
       push success  ────────► back to clean (id + remote_version updated)
             │
       push failure → stays dirty, retried next worker tick

        ┌──────────────┐
        │ pull_needed  │ ── remote changed; not yet applied locally
        └──────┬───────┘    (placeholder for Phase 4 use)
               │
       remote applied → clean

        ┌──────────┐
        │ conflict │ ── BOTH local and remote changed since last sync
        └──────────┘    (Phase 4: keep both copies, user picks one)

        ┌─────────────────┐
        │ deleted_pending │ ── user deleted locally; worker will trash on Gmail
        └────────┬────────┘
                 │
          trash success
                 │
                 ▼
            ROW REMOVED
```

## 3. Schema

Single table `notes` with primary key `(uuid, account_id)`:

```
uuid                      TEXT     Apple X-Universally-Unique-Identifier (hyphenated)
account_id                TEXT     User's email — distinguishes the same uuid across accounts
id                        TEXT     Current Gmail message id (empty for unsynced new notes)
title                     TEXT
body_html                 TEXT
date                      TEXT     RFC 2822 Date header (last-modified time)
x_mail_created_date       TEXT?    RFC 2822 (preserved across edits — note's "birthday")
label                     TEXT     "Notes" or "Notes/<sub-path>"
local_version             INTEGER  Monotonic counter; bumps on every local edit
remote_version            TEXT?    Gmail message id at last known sync point
sync_state                TEXT     One of: clean | dirty | pull_needed | conflict | deleted_pending
last_synced_at            INTEGER? ms since epoch
last_local_modified_at    INTEGER  ms since epoch
last_remote_modified_at   INTEGER? ms since epoch (parsed from Date header)
```

Indexes:
- `(account_id, label)` — used by sidebar count rollups.
- Partial index on `sync_state` WHERE `!= 'clean'` — used by sync worker to
  find rows with pending work without scanning the whole table.

## 4. Read pipeline

### Cold start (instant)

1. `list_cached_notes(account_id)` reads from SQLite → returns immediately
2. Frontend paints UI from this snapshot
3. `list_notes(account_id)` fetches from Gmail in background
4. On completion: upsert into cache + prune (see §6) → frontend re-renders

User-perceived latency for first frame: **sub-millisecond** (local DB read).

### Hot path (focus / folder settle / poll)

The settle handlers in `App.svelte` call `list_notes_in_folder` (scoped to
the active folder) or `list_notes` (full sweep). Each upserts results to
the cache and runs the appropriate prune (per-folder or per-account).

The 10-minute background poll always uses the full sweep — it's the only
authoritative reconciliation against the remote.

## 5. Write pipeline (local-first)

```
User edit                                          ┌── Background worker ──┐
    │                                              │                       │
    ▼                                              │  Every 5s:            │
┌─────────────┐                                    │   - list dirty rows   │
│ apply_local │   sync_state: clean → dirty        │   - for each:         │
│ _edit       │   local_version: bump              │     ─ insert + trash  │
└─────────────┘   (returns immediately)            │       via gmail.rs    │
    │                                              │     ─ mark_pushed     │
    ▼                                              │   - list deleted_p... │
   UI                                              │     ─ trash on Gmail  │
                                                   │     ─ db.delete row   │
                                                   └───────────────────────┘
```

**Key property:** the `save_note` Tauri command **does not call Gmail**.
It writes SQLite and returns. The user sees "Saved" within microseconds.
The Gmail push happens in the background, retried as needed.

For a brand-new note (no UUID yet), Rust generates an Apple-format UUID
and inserts a row with `id = ""`, `remote_version = None`, `sync_state =
dirty`. The worker fills in `id` and flips to `clean` on first push.

## 6. Pruning (remote-deletion propagation)

The cache must learn when notes are **gone** from the remote, not just when
new ones appear. Two prune paths:

| Trigger | Scope | What gets dropped |
|---|---|---|
| Full `list_notes` sweep | Whole account | Any `clean` row whose uuid wasn't in the fetched set |
| Scoped `list_notes_in_folder` | One folder | Any `clean` row IN that folder whose uuid wasn't in the fetched set |

**Pruning never touches** `dirty` / `deleted_pending` / `conflict` rows —
those have pending local intent and the sync layer owns their lifecycle.

This was a design oversight in Phase 2 (cache-first reads) — without
pruning, the cache was append-only from remote and accumulated stale
rows forever. The flash on cold start was the symptom.

## 7. Soft vs hard delete

### Notes

| Stage | DB state | UI shows |
|---|---|---|
| User deletes locally | `sync_state = 'deleted_pending'`, row still in DB | Note hidden (the DB read query filters out deleted_pending) |
| Worker trashes on Gmail | Row removed via `db.delete(uuid, account_id)` | Same — still hidden |

So locally, the soft-deleted window is **bounded** — it lasts only until
the worker confirms Gmail trash. After that, hard delete.

Remote-initiated deletes (note removed via Apple Notes or Gmail web)
land directly as **hard deletes** via the prune path. No soft phase
because there's no local intent to preserve.

### Labels / folders

**Hard delete only.** No soft state. `delete_folder` Tauri command calls
`gmail::delete_label` immediately. If offline, the call fails and the
folder is NOT removed locally.

Known asymmetry with notes — folder deletes are online-only in v1.

### No "trash" within Jodd

Once a row is hard-deleted from SQLite, it's gone from this replica.
Recovery is via **Gmail's Trash** (~30-day retention) — accessed
through Gmail web, not Jodd.

## 8. Conflict handling (Phase 4)

**Implemented.** When a pull fetches a note that has BOTH a different
`remote_version` than our cache AND a local `dirty` state, we treat it as
a concurrent-edit conflict and apply the **keep-both** rule.

### Detection

In `reconcile_one` (lib.rs), for every fetched note:

```
existing.sync_state == Dirty
    AND existing.remote_version != fetched.id
    → CONFLICT
```

### Resolution (v1: keep both, primary converges to remote)

The design principle: **every replica should agree on every uuid's
content**. So the primary note (uuid=X) converges to the REMOTE state.
The LOCAL content (the one at risk of being overwritten) is preserved
as a new conflict-copy note with a fresh uuid.

1. **Original row** (uuid=X) — `upsert_from_remote` applied. Content
   becomes the remote version, `sync_state = 'clean'`. The worker won't
   push it. All replicas now agree on what's at uuid=X.

2. **New duplicate row** (new uuid) — `sync_state = 'dirty'`.
   Title = `<original title> (conflict from <Device> <YYYY-MM-DD HH:MM>)`.
   Content = the LOCAL version (the at-risk content we just saved).
   Worker pushes it as a brand-new note on Gmail so other replicas see it too.

After everything settles, every replica shows the same picture:

| Replica | Original (uuid=X) | Conflict-copy (new uuid) |
|---|---|---|
| Jodd | remote content | local content (with conflict suffix) |
| Apple Notes | remote content | local content (with conflict suffix, synced in) |
| Gmail | remote content | local content (with conflict suffix) |

The user sees two notes — the primary (with whatever remote said) and
a clearly-labeled "(conflict from X)" copy of the local edits. Resolution
options:

- **Keep remote (primary)** → delete the conflict-copy. Done.
- **Keep local** → delete the primary, optionally rename the conflict-copy
  to remove the suffix.
- **Merge** → manually copy fragments between the two, then delete the
  one you didn't merge into.

### Why this swap matters

An earlier version of this implementation kept LOCAL on the primary and
put REMOTE in the conflict-copy. That was structurally asymmetric: same
uuid showed different content on different replicas. Apple Notes would
see remote on the primary (its own edit, unchanged) AND remote on the
conflict-copy (our pushed copy of remote) — visually identical duplicates
distinguished only by title. Confusing.

The current direction (REMOTE on primary, LOCAL preserved as the
conflict-copy) makes every replica agree on every uuid's content, with
the divergent version clearly labeled as "the version that was on
<Device>".

### What's intentionally NOT done

- **Auto-merge.** We never try to merge the two contents. Same-line
  diffs, structural HTML differences, etc. are not safe to auto-resolve.
- **UI conflict badges.** Future work — the conflict-flagged row still
  renders like a normal note. The "(conflict from...)" suffix on the
  duplicate is the user's signal.
- **Repeated conflict prevention.** If the user takes no action AND the
  remote changes again, `reconcile_one` sees the row is already in
  Conflict state and skips creating another duplicate. The conflict is
  "sticky" until the user does something.

### Known race window

If a user edits a conflict-flagged note (transitioning to Dirty), and a
new remote change arrives in the brief window between that edit and the
worker's push, we'd detect another conflict and create another duplicate.
This is rare (worker tick = 5s) and not lossy — just produces an extra
duplicate the user can delete. Acceptable for v1.

### Self-induced conflict suppression (in-flight push tracking)

A subtler race lives between the worker's call to `gmail::save_note`
(which produces a new Gmail message id) and `db.mark_pushed` (which
records the new id in the cache). During that ~1-2 second window:

- Gmail has the new message (id = A2)
- Our cache still says `remote_version = A1`, `sync_state = dirty`

Any concurrent `list_notes` that lands in this window would see
"fetched.id = A2" vs "cached.remote_version = A1" — and falsely flag
this as a remote-change conflict, creating a duplicate of our OWN push.

The fix: `AppState.pushing` (a `HashSet<(account_id, uuid)>`) is
updated by `sync_worker_tick` to mark uuids that are currently being
pushed. `reconcile_one` consults this set FIRST and skips any fetched
note whose uuid is in flight from our worker. Once `mark_pushed`
completes and the entry is removed from `pushing`, future reconciles
proceed normally.

This is purely in-memory state; it doesn't persist across restarts.
On restart, in-flight rows are still in their pre-push state (`dirty`)
and the worker will retry them — so we never lose the push intent.

## 9. What's authoritative when

| Operation | Authoritative source |
|---|---|
| What notes exist for an account | Gmail (after a full `list_notes` sweep) |
| What labels/folders exist | Gmail (`list_folders`) |
| Note content (clean rows) | Gmail |
| Note content (dirty rows) | Local cache (pending push) |
| Sync state machine | Local cache only |
| Pending deletions | Local cache (until worker confirms trash) |

## 10. Background sync worker

Runs continuously, 5-second interval, single-instance:

1. `db.list_dirty()` → for each row, `gmail::save_note` (insert + trash), then `db.mark_pushed`
2. `db.list_deleted_pending()` → for each row, `gmail::delete_note` (trash), then `db.delete`

Failures (network down, 5xx) leave the row in its pending state — retried
next tick. Permanent failures (4xx) are logged but the row also stays
pending; we'd rather block sync than silently lose data.

## 11. Invariants

1. **No silent overwrites of local intent.** A row with `sync_state` in
   {`dirty`, `deleted_pending`, `conflict`} is never overwritten by a
   remote upsert. Phase 4 turns the conflict case into "keep both".
2. **Every save is at-least-once durable.** SQLite write completes before
   the Tauri command returns. App crash before Gmail push → row stays
   dirty in cache, worker retries on next start.
3. **No "saved" without local persistence.** The UI's "Saved" indicator
   reflects the cache state, never the Gmail state. (Worker's status
   could surface separately in the future.)
4. **Apple format compatibility.** UUID is canonicalized to 8-4-4-4-12
   hyphenated form; date headers are RFC 2822; `X-Mail-Created-Date`
   is preserved across edits.

## 12. Known limitations (v1)

- ~~**Folder operations are online-only.**~~ **Resolved.** Folders now
  follow the same local-first pattern as notes — see §15.
- **Conflict detection is not implemented yet** (Phase 4).
- **Single-replica view.** The cache reflects one device's state. If you
  run Jodd on two machines pointed at the same Gmail account, they each
  have their own SQLite — they sync via Gmail but don't communicate
  directly.
- **No edit history.** Each save replaces the previous Gmail message
  (insert + trash); the old version is in Gmail's Trash for ~30 days
  but isn't browseable from Jodd.
- **maxResults=200 per label.** Accounts with >200 notes in a single
  label will have those notes truncated on each fetch. Doesn't affect
  cached notes once they're in SQLite.

## 15. Folder operations (local-first)

Folders mirror the note model: a `folders` table in SQLite tracks per-account
folder paths with sync_state. Mutations write SQLite immediately; the
background worker pushes to Gmail asynchronously.

### Schema

```
account_id              TEXT
path                    TEXT     "Notes" or "Notes/<sub-path>"
label_id                TEXT?    Gmail's label id (NULL until first push)
sync_state              TEXT     clean | dirty_new | dirty_renamed | deleted_pending
last_local_modified_at  INTEGER  ms since epoch
last_synced_at          INTEGER? ms since epoch
PRIMARY KEY (account_id, path)
```

### State machine

```
        local user action
              │
              ▼
         dirty_new        ← user created locally; label_id still NULL
              │
       worker pushes
              │
              ▼
           clean          ← Gmail has it now; label_id stored
              │
       local rename / move
              │
              ▼
       dirty_renamed      ← user changed path; label_id still points to the
              │             same Gmail label, which gets renamed on push
       worker pushes
              │
              ▼
           clean

         local delete
              │
              ▼
       deleted_pending    ← user wants gone; worker will trash on Gmail
              │
       worker trashes
              │
              ▼
         ROW REMOVED
```

A `dirty_new` folder that gets deleted before being pushed is dropped
immediately (no Gmail call needed). This is handled by `mark_folder_deleted`.

### Cascade rules

A rename or move of `Notes/A` also affects:
- All descendant folders (`Notes/A/B`, `Notes/A/B/C`, ...) — they get their
  path prefix rewritten in one SQLite transaction.
- All notes whose `label` field starts with that path — their label is
  rewritten too. Notes are NOT marked dirty (Gmail keeps label_id stable
  across renames, so the server-side note already points at the renamed
  label; the local `label` field is just our mirror of the name).

Each touched folder transitions to `dirty_renamed`, so the worker pushes
each rename to Gmail individually (Gmail's REST API doesn't have a
"rename whole subtree" operation).

### Worker ordering

Inside `sync_worker_tick`, folders are pushed BEFORE notes:

1. `dirty_new` folders first (sorted shallowest-first by path length, so
   parents are created before children try to reference them).
2. `dirty_renamed` folders.
3. `deleted_pending` folders (sorted deepest-first so children are
   removed before parents).
4. Then the existing note phase (dirty + deleted_pending).

This ordering matters because notes' `label` field is the path. If a
note targets `Notes/Recipes/Italian` and that folder doesn't yet exist
in Gmail's label_map, `save_note`'s label resolution falls back to the
"Notes" root and the note ends up in the wrong folder.

After each successful folder create/rename, the label_map cache is
invalidated so the next note save sees the updated state.

### Pull reconciliation

`list_notes` (full sweep) is authoritative for the remote folder list
because its `label_map` is the complete set of labels Gmail reports.
After reconciling notes, `list_notes` also:

1. Upserts each remote `Notes/*` label into the folders table as `clean`
   (skipping rows already in pending states — the worker owns those).
2. Prunes clean cache rows whose paths aren't in the remote list
   (folder deleted externally).

`list_notes_in_folder` (scoped fetch) is NOT used for folder reconciliation
— it's authoritative only for one label, not the whole tree.

### list_folders Tauri command

Reads SQLite only — no network. Returns paths in any non-deleted state
(so dirty_new folders appear immediately after creation). The full sweep
inside `list_notes` keeps the cache current.

## 13. File locations

| Path | Purpose |
|---|---|
| `~/Library/Application Support/jodd/jodd.sqlite3` (macOS) | The cache |
| `%APPDATA%/jodd/jodd.sqlite3` (Windows) | The cache |
| `~/Library/Application Support/jodd/accounts.json` | Account metadata (non-sensitive) |
| Keychain entry `jodd/rt::<email>` | OAuth refresh token (sensitive) |
| `~/Library/Application Support/jodd/jodd.sqlite3-wal` and `-shm` | SQLite WAL journal |

## 14. Diagnostic queries

If you need to inspect the cache directly (for debugging):

```bash
sqlite3 ~/Library/Application\ Support/jodd/jodd.sqlite3

-- counts by state
SELECT sync_state, COUNT(*) FROM notes GROUP BY sync_state;

-- pending writes
SELECT uuid, account_id, label, sync_state, last_local_modified_at
FROM notes WHERE sync_state != 'clean';

-- everything in one account
SELECT uuid, title, label, sync_state FROM notes WHERE account_id = '<email>';
```

Read-only inspection is safe while the app is running. Writes via the
sqlite3 CLI while Jodd is active risk corruption — quit the app first.
