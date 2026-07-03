# Slug Rewrite-on-Rename Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** When a note is renamed, rewrite slug-form `[[*-uuid8]]` wikilinks in every carrying note's body so the displayed link text stays fresh (resolution by `uuid8` was already correct).

**Architecture:** A pure body-rewrite fn (`rewrite_wikilink_slug_in_body`) + a connection-scoped DB helper (`rewrite_links_to_renamed_note_conn`) that finds carriers via the `edges` index and rewrites their bodies (bump version, clean→dirty, re-derive). Hooked into `db::apply_local_edit`: when the saved note's slug changes, rewrite inbound links. Mirrors the existing `rewrite_tag_in_bodies` / `rewrite_hashtag_in_body` pattern. Backend-agnostic (operates on the cache; the sync worker pushes dirtied carriers via their vertical).

**Tech Stack:** Rust, rusqlite, existing `cargo test` suite (87 tests).

**Source spec:** [docs/superpowers/specs/2026-06-16-slug-rewrite-on-rename-design.md](../specs/2026-06-16-slug-rewrite-on-rename-design.md)

**Acceptance bar:** existing 87 tests green + new tests; behavior verified for slug-form links only (plain `[[Title]]` untouched); single file (`src-tauri/src/db.rs`).

---

## File Structure

| File | Responsibility |
|---|---|
| `src-tauri/src/db.rs` (modify) | `rewrite_wikilink_slug_in_body` (pure fn, near `rewrite_hashtag_in_body`); `rewrite_links_to_renamed_note_conn` (connection-scoped helper, near `rewrite_tag_in_bodies`); the `apply_local_edit` hook; unit + integration tests in the existing `#[cfg(test)] mod` blocks. |

No other files. No schema change (`edges.dst_id` already exists, migration #12). No frontend/vertical changes.

---

### Task 1: `rewrite_wikilink_slug_in_body` (pure fn) + unit tests

**Files:**
- Modify: `src-tauri/src/db.rs` (add fn near `rewrite_hashtag_in_body` ~line 2892; add tests in the `#[cfg(test)] mod tests` block that holds `slug_and_note_slug`/`wikilinks_basic_dedup_trim`)

- [x] **Step 1: Write the failing unit tests**

In the existing `#[cfg(test)] mod tests` block in `db.rs` (the one with `wikilinks_basic_dedup_trim`), add:

```rust
#[test]
fn rewrite_wikilink_slug_updates_matching_uuid8() {
    let b = "see [[old-title-abc12345]] and [[other-def67890]] and [[Plain Title]] end";
    let out = super::rewrite_wikilink_slug_in_body(b, "abc12345", "new-title-abc12345");
    assert_eq!(
        out,
        "see [[new-title-abc12345]] and [[other-def67890]] and [[Plain Title]] end",
        "only the matching uuid8 link is rewritten; other uuid8 + plain title untouched"
    );
}

#[test]
fn rewrite_wikilink_slug_handles_hyphenated_titles_and_titleless() {
    // hyphenated title-slug: only the trailing 8-hex token matters
    let b = "[[weekly-review-notes-abc12345]] [[abc12345]]";
    let out = super::rewrite_wikilink_slug_in_body(b, "abc12345", "renamed-abc12345");
    assert_eq!(out, "[[renamed-abc12345]] [[renamed-abc12345]]");
}

#[test]
fn rewrite_wikilink_slug_uuid8_case_insensitive_and_noop_when_absent() {
    let b = "[[t-ABC12345]] [[nope-99999999]]";
    let out = super::rewrite_wikilink_slug_in_body(b, "abc12345", "x-abc12345");
    assert_eq!(out, "[[x-abc12345]] [[nope-99999999]]");
    // no matching uuid8 → unchanged
    assert_eq!(super::rewrite_wikilink_slug_in_body("[[a-11111111]]", "abc12345", "x-abc12345"), "[[a-11111111]]");
}
```

- [x] **Step 2: Run — fails (fn undefined)**

Run: `cd src-tauri && cargo test --lib rewrite_wikilink_slug 2>&1 | tail -8`
Expected: FAIL (`cannot find function rewrite_wikilink_slug_in_body`).

- [x] **Step 3: Implement the fn**

Add near `rewrite_hashtag_in_body` in `db.rs`:

```rust
/// Rewrite every slug-form wikilink that targets `uuid8` to use `new_slug`.
/// A slug link is `[[<title-slug>-<uuid8>]]` (or titleless `[[<uuid8>]]`), where
/// uuid8 is the 8 hex chars immediately before `]]` (after the final `-`).
/// Only links whose trailing 8-hex token equals `uuid8` (case-insensitive) are
/// rewritten — links to other notes and plain `[[Title]]` (no `-uuid8` suffix)
/// are left untouched. Returns the body (== input if nothing matched).
fn rewrite_wikilink_slug_in_body(body_html: &str, uuid8: &str, new_slug: &str) -> String {
    let want = uuid8.to_lowercase();
    let bytes = body_html.as_bytes();
    let mut out = String::with_capacity(body_html.len());
    let mut i = 0;
    while i < body_html.len() {
        // Look for the start of a wikilink `[[`.
        if bytes[i] == b'[' && i + 1 < body_html.len() && bytes[i + 1] == b'[' {
            // Find the closing `]]`.
            if let Some(rel_end) = body_html[i + 2..].find("]]") {
                let inner = &body_html[i + 2..i + 2 + rel_end];
                // Extract the trailing token after the last '-' (or the whole
                // inner if there is no '-'): that's the uuid8 for a slug link.
                let tail = inner.rsplit('-').next().unwrap_or(inner);
                if tail.len() == 8
                    && tail.bytes().all(|b| b.is_ascii_hexdigit())
                    && tail.eq_ignore_ascii_case(&want)
                {
                    out.push_str("[[");
                    out.push_str(new_slug);
                    out.push_str("]]");
                    i = i + 2 + rel_end + 2; // past the closing ]]
                    continue;
                }
            }
        }
        // Default: copy this char (advance by full UTF-8 width).
        let ch_len = body_html[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        out.push_str(&body_html[i..i + ch_len]);
        i += ch_len;
    }
    out
}
```

- [x] **Step 4: Run — passes**

Run: `cd src-tauri && cargo test --lib rewrite_wikilink_slug 2>&1 | tail -8`
Expected: 3 tests PASS.

- [x] **Step 5: Commit**

```bash
git add src-tauri/src/db.rs
git commit -m "feat(slug): rewrite_wikilink_slug_in_body — rewrite [[*-uuid8]] links by uuid8"
```

### Task 2: `rewrite_links_to_renamed_note_conn` + `apply_local_edit` hook + integration test

**Files:**
- Modify: `src-tauri/src/db.rs` (add helper near `rewrite_tag_in_bodies` ~line 1865; edit `apply_local_edit` ~line 933; add integration test)

- [x] **Step 1: Write the failing integration test**

In the `db.rs` test module that constructs an in-memory/temp `Db` (find an existing test that calls `Db::open`/`apply_local_edit`/`insert_local_new` to copy its setup style), add:

```rust
#[test]
fn rename_rewrites_inbound_slug_links() {
    let db = test_db(); // use the existing helper that opens a temp Db; if none, open at a tempfile
    let acc = "acct";
    // Target note B (its uuid8 is the first 8 hex).
    let b_uuid = "ABCDEF12-0000-0000-0000-000000000000"; // uuid8 = "abcdef12"
    db.insert_local_new(&make_note(acc, b_uuid, "Old B Title", "<div>Old B Title</div><div>body</div>", "Notes")).unwrap();
    // Carrier note A links to B by slug-form.
    let a_uuid = "11111111-0000-0000-0000-000000000000";
    db.insert_local_new(&make_note(acc, a_uuid, "A", "<div>A</div><div>see [[old-b-title-abcdef12]] here</div>", "Notes")).unwrap();
    // Sanity: A must be a known carrier (edges derived on insert).
    // Now RENAME B via the save path (new title → new slug).
    db.apply_local_edit(b_uuid, acc, "New B Title", "<div>New B Title</div><div>body</div>", "Notes").unwrap();
    // A's body link text should now reflect B's new slug; A should be dirty.
    let a = db.get(a_uuid, acc).unwrap().unwrap();
    assert!(a.body_html.contains("[[new-b-title-abcdef12]]"), "A body rewritten, got: {}", a.body_html);
    assert!(!a.body_html.contains("old-b-title"), "old slug text gone");
    // (B itself is just edited; not asserting B here.)
}
```

Helper notes for the implementer: reuse the test module's existing `Db` setup + note-construction helpers if present (grep the test module for `insert_local_new(` to copy the `CachedNote`/`Note` builder). If no `test_db()`/`make_note` helper exists, write minimal local ones in the test (open a `Db` at a `tempfile::NamedTempFile` path; build the `CachedNote`/`Note` with the fields `insert_local_new` needs). `uuid8` of `ABCDEF12-...` is `abcdef12` (lowercased first 8 hex).

- [x] **Step 2: Run — fails**

Run: `cd src-tauri && cargo test --lib rename_rewrites_inbound 2>&1 | tail -15`
Expected: FAIL (A's body still shows `old-b-title` — no rewrite wired yet).

- [x] **Step 3: Implement the connection-scoped helper**

Add to `db.rs` (free fn or `impl Db` — make it a free `_conn` fn so `apply_local_edit` can call it with its already-locked `conn` without re-locking the Mutex; mirror the `reconcile_*_conn` helpers' shape):

```rust
/// Rewrite slug-form links to `target_uuid` across every carrying note's body
/// so the displayed text matches `new_slug`. Carriers found via the edges index
/// (rel='mentions', dst_id=uuid8) — no full scan. Each rewritten carrier: body
/// updated, local_version bumped, clean→dirty (worker re-syncs it), tags/edges/
/// fts re-derived. Uses the supplied connection (no re-lock). Returns # rewritten.
fn rewrite_links_to_renamed_note_conn(
    conn: &rusqlite::Connection,
    account_id: &str,
    target_uuid: &str,
    new_slug: &str,
) -> SqlResult<usize> {
    let uuid8 = uuid_short(target_uuid);
    let carriers: Vec<(String, String, String, String)> = {
        let mut s = conn.prepare(
            "SELECT DISTINCT n.uuid, n.title, n.label, n.body_html
             FROM notes n
             JOIN edges e ON e.account_id = n.account_id AND e.src_uuid = n.uuid
             WHERE n.account_id = ?1 AND e.rel = 'mentions' AND e.dst_id = ?2
               AND n.sync_state != 'deleted_pending'",
        )?;
        let it = s.query_map(params![account_id, uuid8], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        it.filter_map(|x| x.ok()).collect()
    };
    let mut count = 0usize;
    for (uuid, title, label, body_html) in &carriers {
        let new_body = rewrite_wikilink_slug_in_body(body_html, &uuid8, new_slug);
        if &new_body == body_html {
            continue;
        }
        conn.execute(
            "UPDATE notes SET body_html = ?1,
                 local_version = local_version + 1,
                 sync_state = CASE sync_state WHEN 'clean' THEN 'dirty' ELSE sync_state END,
                 last_local_modified_at = ?2
             WHERE uuid = ?3 AND account_id = ?4",
            params![new_body, now_ms(), uuid, account_id],
        )?;
        reconcile_tags_from_body_conn(conn, account_id, uuid, &new_body)?;
        reconcile_edges_from_body_conn(conn, account_id, uuid, label, &new_body)?;
        fts_index_conn(conn, uuid, account_id, title, &new_body)?;
        count += 1;
    }
    Ok(count)
}
```

(Confirm the `reconcile_*_conn` / `fts_index_conn` signatures accept `&Connection` — they're already called with `&conn` from `apply_local_edit`, so they do.)

- [x] **Step 4: Wire the hook into `apply_local_edit`**

Edit `apply_local_edit` (db.rs ~933). BEFORE the existing `UPDATE`, read the previous title; AFTER the existing re-derivation, if the slug changed, call the helper. Final shape:

```rust
pub fn apply_local_edit(
    &self,
    uuid: &str,
    account_id: &str,
    title: &str,
    body_html: &str,
    label: &str,
) -> SqlResult<()> {
    let conn = self.conn.lock().unwrap();
    // Capture the previous title so we can detect a rename (slug change).
    let prev_title: Option<String> = conn
        .query_row(
            "SELECT title FROM notes WHERE uuid = ?1 AND account_id = ?2",
            params![uuid, account_id],
            |r| r.get(0),
        )
        .optional()?;
    conn.execute(
        "UPDATE notes
         SET title = ?1, body_html = ?2, label = ?3,
             local_version = local_version + 1,
             sync_state = CASE sync_state
                 WHEN 'clean' THEN 'dirty'
                 WHEN 'pull_needed' THEN 'conflict'
                 WHEN 'conflict' THEN 'dirty'
                 ELSE sync_state
             END,
             last_local_modified_at = ?4
         WHERE uuid = ?5 AND account_id = ?6",
        params![title, body_html, label, now_ms(), uuid, account_id],
    )?;
    fts_index_conn(&conn, uuid, account_id, title, body_html)?;
    reconcile_tags_from_body_conn(&conn, account_id, uuid, body_html)?;
    reconcile_edges_from_body_conn(&conn, account_id, uuid, label, body_html)?;
    // Rename → rewrite inbound slug links so their displayed text stays fresh.
    // Only when the title-slug actually changed (titleless slug = uuid8 only).
    if let Some(pt) = prev_title {
        if slugify(&pt) != slugify(title) {
            let new_slug = note_slug(title, uuid);
            rewrite_links_to_renamed_note_conn(&conn, account_id, uuid, &new_slug)?;
        }
    }
    Ok(())
}
```

Requires `use rusqlite::OptionalExtension;` in scope for `.optional()` (check the top of db.rs; add if missing — it's commonly already imported).

- [x] **Step 5: Run the integration test + full suite**

Run: `cd src-tauri && cargo test --lib rename_rewrites_inbound 2>&1 | tail -10`
Expected: PASS.
Run: `cd src-tauri && cargo test 2>&1 | tail -5`
Expected: all pass (was 87; now 87 + 4 new = 91).

- [x] **Step 6: Commit**

```bash
git add src-tauri/src/db.rs
git commit -m "feat(slug): rewrite inbound [[*-uuid8]] links on rename (apply_local_edit hook)"
```

### Task 3: verification gate

**Files:** none

- [x] **Step 1:** `cd src-tauri && cargo test 2>&1 | tail -5` → all green (91). `cargo build 2>&1 | grep -ic warning` → 0.
- [x] **Step 2:** Manual reasoning check against the spec edge cases: plain `[[Title]]` untouched (covered by Task 1 test), hyphenated slug (covered), titleless slug (covered), other-uuid8 untouched (covered), self-link (the renamed note links to itself: it's selected as a carrier via edges src==target — its own body's self-link is rewritten by the same pass; confirm by noting `apply_local_edit` updated the renamed note's own body already with the new title, and the helper additionally fixes any `[[*-self-uuid8]]` in OTHER notes; a self-link inside the just-saved body is in `body_html` which the caller passed — if the editor inserted a self-link it would carry the NEW slug from the picker, so no stale self-link in the saved body). No code needed.
- [x] **Step 3:** Commit (empty if nothing changed):
```bash
git commit --allow-empty -m "test(slug): verify rewrite-on-rename — 91 tests green, edge cases covered"
```

---

## Self-Review

**Spec coverage:** scope=slug-form only (Task 1 fn matches trailing uuid8, leaves plain `[[Title]]` — tested) ✓; trigger=on save when slug changes (Task 2 `apply_local_edit` hook with `slugify(prev)!=slugify(new)`) ✓; carriers via edges index (Task 2 query on `dst_id`/`idx_edges_dst_id`) ✓; mirror `rewrite_tag_in_bodies` (UPDATE+bump+clean→dirty+re-derive) ✓; backend-agnostic (cache only, worker pushes) ✓; tests (unit + integration) ✓; edge cases (hyphen, titleless, other-uuid8, plain-title, self-link) ✓.

**Placeholder scan:** code blocks are complete; the integration-test setup says "reuse existing helpers / else write minimal" with concrete guidance (open temp Db, build the note) — the implementer must adapt to the real `CachedNote`/`insert_local_new` shape, which is a verify-against-real-symbols step, not a vague TODO.

**Type consistency:** `rewrite_wikilink_slug_in_body(body, uuid8, new_slug)` used identically in Task 1 + Task 2 helper. `rewrite_links_to_renamed_note_conn(conn, account, target_uuid, new_slug)` used in Task 2 helper + the `apply_local_edit` hook. `note_slug`/`slugify`/`uuid_short` are existing fns.

**Known soft spot:** the integration test's Db/Note construction must match the real `db.rs` test conventions (Task 2 Step 1 instructs grepping the test module for `insert_local_new(` to copy the builder); the `.optional()` import (Task 2 Step 4) must be confirmed.
