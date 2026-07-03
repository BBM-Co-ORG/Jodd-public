# Offline cold-start fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the app reachable offline on a cold start when a local cache exists, instead of bouncing the user to the sign-in screen because a network token refresh failed.

**Architecture:** Replace the network-dependent readiness check in `is_authenticated` with a pure, offline-safe decision: an account is usable if we have a local cache to serve OR we hold refreshable credentials (a keychain presence check) — neither touches the network. A present-but-revoked token surfaces a soft re-auth on the first *sync* attempt (existing `handleAuthLoss` path), rather than locking the user out of their own cached notes. Implements design principle 5 ("readiness ≠ network") from the architecture north-star.

**Tech Stack:** Rust (Tauri 2 backend), `rusqlite`, `keyring`. Tests use the existing `#[cfg(test)]` pure-function convention in `lib.rs`.

---

## Background — verified facts

- `is_authenticated` ([src-tauri/src/lib.rs:509-536](src-tauri/src/lib.rs:509)) loops accounts and calls `ensure_token`, whose slow path calls `auth::refresh_access_token` (network). On a cold start, in-memory `account_states` is empty, so the fast path always misses → every account hits the network. Offline → all fail → returns `false` → `{#if !$isAuthenticated}` shows `AuthScreen` ([src/App.svelte:813](src/App.svelte:813)) despite a full cache.
- `load_refresh_token` ([src-tauri/src/accounts.rs:200](src-tauri/src/accounts.rs:200)) is a pure keychain read (`entry.get_password()`), **no network**.
- The cache read paths are already offline-safe: `list_cached_notes`, `list_cached_notes_in_folder`, and `get_note_attachments` ([src-tauri/src/lib.rs:1091](src-tauri/src/lib.rs:1091), reads `db.list_attachments`) never call `ensure_token`.
- The codebase has **no DB/keychain test harness** — existing tests are pure-function only (`lib.rs` `mod validate_folder_segment_tests`). The pure decision function is unit-tested; the thin I/O helpers are verified by `cargo build` + the offline acceptance run (Task 6). Adding a DB test harness is out of scope.

## File structure

- Modify `src-tauri/src/lib.rs` — add pure `account_is_usable`, its test module, and rewrite `is_authenticated`.
- Modify `src-tauri/src/accounts.rs` — add `has_refresh_token` presence helper.
- Modify `src-tauri/src/db.rs` — add `has_cached_notes` existence helper.

No frontend changes: `is_authenticated` keeps its command name and signature, so `App.svelte` is untouched.

---

### Task 1: Pure readiness decision + unit tests

**Files:**
- Modify: `src-tauri/src/lib.rs` (add function near `is_authenticated` at line 509; add a test module beside the existing `mod validate_folder_segment_tests` at line 1535)

- [ ] **Step 1: Write the failing tests**

Add this test module immediately after the closing `}` of `mod validate_folder_segment_tests` (after [src-tauri/src/lib.rs:1581](src-tauri/src/lib.rs:1581)):

```rust
#[cfg(test)]
mod account_readiness_tests {
    use super::*;

    #[test]
    fn usable_with_cache_only() {
        assert!(account_is_usable(true, false));
    }

    #[test]
    fn usable_with_creds_only() {
        assert!(account_is_usable(false, true));
    }

    #[test]
    fn usable_with_both() {
        assert!(account_is_usable(true, true));
    }

    #[test]
    fn not_usable_with_neither() {
        assert!(!account_is_usable(false, false));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml account_readiness_tests`
Expected: compile error — `cannot find function account_is_usable in this scope`.

- [ ] **Step 3: Write the minimal implementation**

Add this function immediately above `async fn is_authenticated` (before [src-tauri/src/lib.rs:509](src-tauri/src/lib.rs:509)):

```rust
/// Pure readiness decision (no I/O). An account is usable when we have a local
/// cache to serve OR credentials we could refresh — neither requires network.
/// This is the heart of design principle 5 ("readiness ≠ network").
fn account_is_usable(has_local_cache: bool, has_refreshable_creds: bool) -> bool {
    has_local_cache || has_refreshable_creds
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml account_readiness_tests`
Expected: `test result: ok. 4 passed`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(auth): pure account_is_usable readiness decision + tests"
```

---

### Task 2: `db.has_cached_notes` existence helper

**Files:**
- Modify: `src-tauri/src/db.rs` (add method inside the `impl Db` block, e.g. next to `count_notes_in_label` at line 2411)

- [ ] **Step 1: Write the implementation**

Add this method inside `impl Db` (immediately after `count_notes_in_label`, after [src-tauri/src/db.rs:2419](src-tauri/src/db.rs:2419)):

```rust
    /// Cheap existence check: does the cache hold ANY note for this account?
    /// Backs the offline-safe readiness gate (`is_authenticated`). Pure SQLite,
    /// sub-ms, no network. Counts rows in any sync_state (a deleted_pending row
    /// still means we have something to show until the worker prunes it).
    pub fn has_cached_notes(&self, account_id: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE account_id = ?1)",
            params![account_id],
            |r| r.get(0),
        )
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: compiles (a `dead_code` warning for the unused method is acceptable until Task 4 wires it).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/db.rs
git commit -m "feat(db): has_cached_notes existence helper for offline readiness"
```

---

### Task 3: `accounts.has_refresh_token` presence helper

**Files:**
- Modify: `src-tauri/src/accounts.rs` (add function next to `load_refresh_token` at line 200)

- [ ] **Step 1: Write the implementation**

Add this function immediately after `load_refresh_token` (after [src-tauri/src/accounts.rs:203](src-tauri/src/accounts.rs:203)):

```rust
/// Presence check only — does the keychain hold a refresh token for this
/// account? Reads the keychain (local) but NEVER refreshes (no network). Used
/// by the offline-safe readiness gate so a cold start can't block on Gmail.
pub fn has_refresh_token(account_id: &str) -> bool {
    load_refresh_token(account_id).is_some()
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: compiles (a `dead_code` warning is acceptable until Task 4 wires it).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/accounts.rs
git commit -m "feat(accounts): has_refresh_token presence check (no network)"
```

---

### Task 4: Rewrite `is_authenticated` to be offline-safe

**Files:**
- Modify: `src-tauri/src/lib.rs:509-536` (replace the body of `is_authenticated`)

- [ ] **Step 1: Replace the function body**

Replace the entire current `is_authenticated` ([src-tauri/src/lib.rs:509-536](src-tauri/src/lib.rs:509)) with:

```rust
#[tauri::command]
async fn is_authenticated(state: State<'_, AppState>) -> Result<bool, String> {
    // "Authenticated" means at least one account is USABLE — readiness ≠ network.
    // Previously this refreshed each account's access token here, which blocked
    // the whole app behind a Gmail round-trip on a cold start while offline
    // (in-memory tokens are empty on launch, so it always hit the network).
    // Now we only do local presence checks: a cached account stays reachable
    // offline, and a present-but-revoked token surfaces a soft re-auth on the
    // first sync attempt (handleAuthLoss), instead of locking the user out.
    let ids: Vec<String> = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .map(|a| a.id.clone())
        .collect();
    if ids.is_empty() {
        log!("is_authenticated: no accounts in store → false");
        return Ok(false);
    }
    for id in &ids {
        let has_creds = accounts::has_refresh_token(id);
        let has_cache = state.db.has_cached_notes(id).unwrap_or(false);
        if account_is_usable(has_cache, has_creds) {
            log!(
                "is_authenticated: {} usable (cache={}, creds={}) → true",
                id, has_cache, has_creds
            );
            return Ok(true);
        }
    }
    log!("is_authenticated: no accounts usable (no cache, no creds) → false");
    Ok(false)
}
```

Note: the function keeps the `async fn ... -> Result<bool, String>` signature (no
`.await` remains, but the Tauri command registration and the frontend `invoke`
stay unchanged). An async fn without an await compiles cleanly.

- [ ] **Step 2: Verify the whole crate compiles and all tests pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: compiles with no `dead_code` warnings for `has_cached_notes` /
`has_refresh_token` / `account_is_usable` (all now used), and
`account_readiness_tests` 4 passed.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "fix(auth): offline-safe is_authenticated — readiness != network

Cold start while offline no longer bounces to the sign-in screen when a
local cache exists. is_authenticated now does local presence checks only
(cached notes OR a keychain refresh-token) instead of a network token
refresh. A revoked token surfaces re-auth on the first sync via the
existing handleAuthLoss path."
```

---

### Task 5: Read-path leak audit (documented)

**Files:**
- Create: `docs/superpowers/notes/2026-06-16-read-path-network-audit.md`

Confirms the cold-start fix is complete and no other normal read/navigation path blocks on Gmail.

- [ ] **Step 1: Enumerate `ensure_token` callers and classify each**

Run: `grep -n "ensure_token(" src-tauri/src/lib.rs`

For each call site, classify as **read/navigation** (must be cache-only — a bug if it calls `ensure_token`) or **sync-worker / explicit-refresh / sign-in** (allowed). Known classification from this plan's investigation:

- `is_authenticated` (was line 524) — **fixed in Task 4** (no longer calls it).
- `list_notes`, `list_notes_in_folder`, `list_trashed_notes`, `refetch_note`,
  `index_account`, folder/pin/tag push helpers, sync-worker push paths — **allowed**
  (explicit refresh, sign-in/index, or background worker — not normal navigation).

- [ ] **Step 2: Confirm the cache read commands are network-free**

Run: `grep -n "fn list_cached_notes\|fn list_cached_notes_in_folder\|fn get_note_attachments" src-tauri/src/lib.rs`

Verify each body uses only `state.db.*` (no `ensure_token`, no `gmail::*`).
`get_note_attachments` ([src-tauri/src/lib.rs:1091](src-tauri/src/lib.rs:1091)) reads
`db.list_attachments` and builds data URIs from stored BLOBs — confirmed cache-only
(the spec's "lazy attachment fetch" concern does not apply: attachments are already
local).

- [ ] **Step 3: Confirm no read-triggered token refresh on the frontend cold-start path**

Read `src/App.svelte` cold-start block ([src/App.svelte:219-277](src/App.svelte:219)).
Confirm the *first paint* uses `loadCachedNotes()` (→ `list_cached_notes`) before any
network call, and that the network calls that follow (`indexAllAccounts`,
`sync_pin_state`, `sync_tag_state`, `loadFolderNotes`) are each wrapped in
`.catch`/`try` so an offline failure degrades silently and never blocks the cache paint.

- [ ] **Step 4: Write the audit note**

Create `docs/superpowers/notes/2026-06-16-read-path-network-audit.md` with: the
`ensure_token` caller table (call site → classification), the confirmation that the
three cache read commands are network-free, and the conclusion that `is_authenticated`
was the only read-path leak (now fixed). If any *new* leak is found, add a follow-up
task here describing the exact file/function and the cache-first fix before closing.

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/notes/2026-06-16-read-path-network-audit.md
git commit -m "docs: read-path network audit — is_authenticated was the only leak"
```

---

### Task 6: Offline acceptance verification

**Files:** none (manual/observed verification of the running app).

- [ ] **Step 1: Build the app**

Run: `npm run tauri build` (or `cargo build --manifest-path src-tauri/Cargo.toml` for a backend-only check).
Expected: builds successfully.

- [ ] **Step 2: Verify the populated-cache offline case (the bug)**

Preconditions: at least one account already signed in with notes in the cache.
1. Disable networking (turn off Wi-Fi / pull the cable / enable Network Link Conditioner "100% Loss").
2. Cold-start the app (fully quit and relaunch).

Expected: the app opens to the **note list painted from cache**, NOT the sign-in
screen. The log shows `is_authenticated: <email> usable (cache=true, creds=true) → true`.
Background sync calls fail and log errors but do not block the UI.

- [ ] **Step 3: Verify the genuinely-signed-out case still gates**

Precondition: no accounts in `accounts.json` (or remove all accounts), cache empty.

Expected: cold start shows `AuthScreen`; log shows
`is_authenticated: no accounts in store → false`.

- [ ] **Step 4: Verify recovery when back online**

Re-enable networking with the populated-cache account. On the next sync tick / focus,
edits flush and pulls resume normally. If the refresh token was revoked while away,
the first sync attempt triggers the existing `handleAuthLoss` re-auth prompt rather
than a silent failure.

- [ ] **Step 5: Commit any doc/log adjustments made during verification** (if none, skip)

```bash
git commit -am "chore: notes from offline cold-start verification"
```

---

## Self-review

**Spec coverage** (against north-star worked example 2 + principle 5):
- `account_usable = has_local_cache || has_refreshable_creds`, presence-only, no network → Tasks 1-4. ✅
- `TransportError::Auth` / revoked token surfaces soft re-auth, doesn't gate cache → preserved via existing `handleAuthLoss`; documented in Task 4 commit + Task 6 step 4. ✅
- "audit other read-path network leaks (attachment fetch, remote image, read-triggered refresh)" → Task 5. ✅ (attachments confirmed cache-only.)

**Placeholder scan:** no TBD/TODO; every code step shows complete code; commands have expected output. ✅

**Type consistency:** `account_is_usable(bool, bool) -> bool`, `has_cached_notes(&str) -> SqlResult<bool>`, `has_refresh_token(&str) -> bool` — names and signatures consistent across Tasks 1-4. ✅

**Scope:** single subsystem (the readiness gate); independently shippable; no frontend change. ✅
