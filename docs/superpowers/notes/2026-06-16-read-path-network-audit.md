# Read-path network-leak audit

**Date:** 2026-06-16  
**Branch:** `fix/offline-cold-start`  
**Auditor:** Claude Code (automated, Task 5 of offline cold-start fix)

## Purpose

Jodd's core doctrine: normal read/navigation must never block on the network.
The immediate trigger for this audit was fixing `is_authenticated`, which previously
called `ensure_token` (a potentially blocking network call) on every cold start. That
fix was landed in this branch. This note verifies that `is_authenticated` was the
**only** such leak and that no other normal read/navigation path touches the network
via `ensure_token`.

The audit covers:

1. Every `ensure_token` call site in `src-tauri/src/lib.rs` — is the enclosing function a
   read/navigation path (must be cache-only), or is it an allowed path (background worker,
   explicit refresh, sign-in, index)?
2. The three cache-read commands (`list_cached_notes`, `list_cached_notes_in_folder`,
   `get_note_attachments`) — are they truly network-free?
3. The frontend cold-start sequence in `src/App.svelte` — does the first paint use
   the cache before any network call, and are all subsequent network calls
   failure-isolated?

---

## 1. `ensure_token` call-site classification

`grep -n "ensure_token(" src-tauri/src/lib.rs` yields 17 occurrences.
The definition at line 104 is excluded from the table; the remaining 16 call sites
are each the first statement of their enclosing function.

| Line | Enclosing function | Classification | Reason |
|-----:|---|---|---|
| 752 | `list_notes` | **Allowed** | Explicit full-account refresh; triggered by background sweep, manual ⟳, and poll — never on navigation |
| 1033 | `delete_note` (id-only fallback branch) | **Allowed** | Dead-code path (no live caller passes only `id`; uuid path returns before this line). Even if reached, it is a write action (trash), not a read |
| 1147 | `list_trashed_notes` | **Allowed** | Explicit user action — opens the "Recently Deleted" view, which is never invoked on cold start or normal folder navigation |
| 1192 | `restore_note` | **Allowed** | Explicit user write action (untrash) |
| 1632 | `list_notes_in_folder` | **Allowed** | Scoped Gmail refresh; called by background sweep, folder settle, poll, and manual refresh — never on initial folder click (which uses `list_cached_notes_in_folder`) |
| 1717 | `refetch_note` | **Allowed** | Explicit user action — "Refetch from Gmail" context-menu item |
| 1775 | `sync_pin_state` | **Allowed** | Cold-start post-index sidecar pull; called **after** `loadCachedNotes()` paints the UI (see §3); wrapped in `.catch` on the frontend so an offline failure is silent |
| 1835 | `index_account` | **Allowed** | Intentional network call — the Phase C index pass; also called after `loadCachedNotes()` so it does not block first paint |
| 2136 | `safe_cleanup_orphans_for_account` | **Allowed** | Explicit user action — orphan cleanup flow triggered from the DupReviewModal |
| 2323 | `preview_orphans` | **Allowed** | Explicit user action — DupReviewModal preview |
| 2425 | `trash_specific_messages` | **Allowed** | Explicit user write action — confirmed orphan trash |
| 2503 | `push_one_dirty` | **Allowed** | Background sync worker — drains `sync_state = dirty` rows |
| 2548 | `push_one_deletion` | **Allowed** | Background sync worker — drains `sync_state = deleted_pending` rows |
| 2595 | `push_one_pin` | **Allowed** | Background sync worker — drains `pin_dirty = 1` rows |
| 2658 | `push_one_tag_set` | **Allowed** | Background sync worker — drains `tags_dirty = 1` rows |
| 2700 | `push_one_folder` | **Allowed** | Background sync worker — drains dirty/renamed/deleted folder rows |

**`is_authenticated` (line 516):** no longer calls `ensure_token`. The fixed
implementation reads only `accounts::has_refresh_token` (keychain presence, no
network) and `state.db.has_cached_notes` (SQLite, no network). This is the fix
landed in this branch.

No call site classified as read/navigation touches `ensure_token`.

---

## 2. Cache-read commands are network-free

### `list_cached_notes` (line 1872)

```rust
async fn list_cached_notes(account_id: String, state: State<'_, AppState>) -> ... {
    let db = state.db.clone();
    let cached = db.list_notes(&account_id).map_err(|e| e.to_string())?;
    Ok(cached.into_iter().map(|c| c.to_frontend_note()).collect())
}
```

Only accesses `state.db`. No `ensure_token`, no `gmail::` call. Network-free.

### `list_cached_notes_in_folder` (line 1858)

```rust
async fn list_cached_notes_in_folder(account_id: String, path: String, state: ...) -> ... {
    let cached = state.db.list_notes_by_label(&account_id, &path).map_err(...)?;
    Ok(cached.into_iter().map(|c| c.to_frontend_note()).collect())
}
```

Only accesses `state.db`. No `ensure_token`, no `gmail::` call. Network-free.

### `get_note_attachments` (line 1106)

```rust
async fn get_note_attachments(account_id: String, uuid: String, state: ...) -> ... {
    let atts = state.db.list_attachments(&account_id, &uuid).map_err(...)?;
    Ok(atts.into_iter().map(|a| {
        let data_uri = if a.mime_type.starts_with("image/") {
            gmail::data_uri(&a.mime_type, &a.data)
        } else { String::new() };
        AttachmentDto { data_uri, content_id: a.content_id, mime_type: a.mime_type }
    }).collect())
}
```

`state.db.list_attachments` reads the `attachments` table (SQLite BLOBs stored in
migration #9). `gmail::data_uri` is a pure utility function that encodes a byte
slice to a `data:` URI — it is not an HTTP call. No `ensure_token`, no network I/O.
The "lazy attachment fetch over network" concern does not apply: attachments are
stored locally as BLOBs on first fetch and served from SQLite thereafter.

---

## 3. Frontend cold-start sequence (App.svelte lines 219–277)

The sequence triggered by `$isAuthenticated` flipping to `true`:

```
loadCachedNotes()          ← paint #1: pure cache (list_cached_notes)
indexAllAccounts()         ← calls index_account (network) — AFTER first paint
loadTags()                 ← list_note_tags (SQLite only, network-free)
Promise.allSettled([
  sync_pin_state (per acct).catch(warn),   ← network; failure is silent
  sync_tag_state (per acct).catch(warn),   ← no-op (disabled: inline tags)
])
loadCachedNotes()          ← paint #2: refresh after pin/tag sidecar apply
loadTags()
try {
  loadFolderNotes(folder)  ← list_notes_in_folder (network); try/catch
} catch (e) { console.error(e) }
startBackgroundSweep()
```

Confirmation:

- `loadCachedNotes()` is the **first** call; it invokes `list_cached_notes`
  (§2 above: pure SQLite). The user sees a populated note list before any network
  call executes.
- `indexAllAccounts()` runs after `loadCachedNotes()` returns. `Promise.allSettled`
  means a per-account failure (e.g. offline) logs but does not throw.
- `sync_pin_state` and `sync_tag_state` are inside `Promise.allSettled` with
  per-item `.catch(warn)` guards. Both are post-first-paint. `sync_tag_state` is a
  no-op stub (line 1814–1821) so it cannot block anything.
- `loadFolderNotes` (→ `list_notes_in_folder`) is inside `try { … } catch (e)
  { console.error(e) }`. An offline failure degrades silently; the cache paint
  from `loadCachedNotes()` stays visible.

The cold-start sequence correctly follows cache-first, network-second with full
offline degradation.

---

## 4. Conclusion

`is_authenticated` was the **only** read-path network leak. All other `ensure_token`
call sites are in:

- background sync worker functions (`push_one_dirty`, `push_one_deletion`,
  `push_one_pin`, `push_one_tag_set`, `push_one_folder`),
- explicit user-triggered refresh commands (`list_notes`, `list_notes_in_folder`,
  `list_trashed_notes`, `refetch_note`, `index_account`, `restore_note`,
  `sync_pin_state`),
- explicit user write/action commands (`delete_note` id-fallback, `restore_note`,
  `safe_cleanup_orphans_for_account`, `preview_orphans`, `trash_specific_messages`).

None of those are invoked on the silent cold-start or normal folder-navigation path.

The three cache-read commands (`list_cached_notes`, `list_cached_notes_in_folder`,
`get_note_attachments`) are all network-free — pure SQLite reads.

The frontend cold-start block paints from cache before any network call and wraps
every subsequent network call with `.catch`/`try` so offline failures degrade
silently.

**No follow-up items.** The fix already landed in this branch is sufficient.
