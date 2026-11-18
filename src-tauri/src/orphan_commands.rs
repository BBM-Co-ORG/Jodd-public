//! Duplicate and orphan cleanup for Gmail accounts: the "N dup" pill, its
//! review list, and the two ways to clear it.
//!
//! Gmail has no REPLACE — a save inserts a new message and trashes the old
//! one — so a failed trash leaves an older copy of the same note behind.
//! `cleanup_orphans` removes those conservatively; `preview_orphans` shows
//! them first and `trash_specific_messages` trashes only what the user chose.
//!
//! Moved out of `lib.rs` 2026-11-16, unchanged.

use std::collections::HashMap;

use tauri::State;

use crate::backend::gmail::wire as gmail;
use crate::{
    cached_label_map, db, ensure_token, log, orphan_cleanup_supported, vertical_for, vertical_from_parts, AppState,
};

// ─── Orphan cleanup (safe replacement for cleanup_stale_uuid_duplicates) ─────
//
// Gmail can accumulate multiple messages with the same X-UUID when save's
// delete-old fails (network blip, race with Apple Notes' IMAP edits, etc.).
// The in-memory dedup in list_notes_in_label hides them from the UI, but
// they waste Gmail storage and slow down subsequent list operations.
//
// This is the SAFE cleanup path. Unlike the original fire-and-forget version
// (which captured keep_id at save time and raced with the next save), this:
//   1. Skips UUIDs whose push is currently in flight (state.pushing set)
//   2. Re-reads the canonical cache.id IMMEDIATELY before each trash call,
//      so a save that lands between scan and trash can't have its live
//      message destroyed
//   3. Bounds work to notes modified in the last 24 hours — older notes
//      rarely accumulate new orphans and the per-uuid header fetch cost
//      is O(messages_in_Notes_labels)
//
// Triggered manually via the cleanup_orphans command. Auto-trigger is held
// back until multi-device test coverage exists (specifically: ensuring a
// fresh Apple-Notes-side edit isn't trashed before it's been polled).

/// How long a tombstoned tag survives before we're confident the underlying
/// note really is gone (not just transiently missing from a Gmail listing)
/// and the tag can be permanently dropped. 7 days is generous relative to
/// any Gmail eventual-consistency hiccup or pagination glitch we've observed,
/// while still bounding how long deleted-account-style cruft lingers.
pub(crate) const TOMBSTONE_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

async fn safe_cleanup_orphans_for_account(
    state: &State<'_, AppState>,
    account_id: &str,
) -> Result<usize, String> {
    // Orphan cleanup only applies to Gmail — see `orphan_cleanup_supported`'s
    // doc comment.
    {
        let list = state.accounts.lock().unwrap();
        if let Some(a) = list.iter().find(|a| a.id == account_id) {
            if !orphan_cleanup_supported(a.backend_kind) {
                log!(
                    "safe_cleanup_orphans_for_account: skipping non-Gmail account {} ({:?})",
                    account_id, a.backend_kind
                );
                return Ok(0);
            }
        }
    }
    let token = ensure_token(state, account_id).await?;
    let label_map = cached_label_map(state, account_id, &token).await?;
    // One bulk uuid->ids scan instead of one full Notes/* re-scan per note
    // (see find_all_duplicate_ids doc comment — this used to be O(candidates
    // * mailbox size) and could hang for many minutes on a normal mailbox).
    let dup_map = gmail::find_all_duplicate_ids(&token, &label_map).await?;

    // The 24h "recent-edit" gate that used to live here was a
    // holdover from the auto-cleanup era (disabled 2026-06-09). Auto-trash
    // had to be cautious because a fresh Apple-Notes-side edit not yet
    // polled could look like an orphan; the gate kept it away from recent
    // notes. This is the user-triggered path now — the in-flight push
    // check + the live-cache-id refusal below are the actual safety net.
    // Keeping the 24h window made the sidebar "N dup" pill diverge from
    // what cleanup could actually fix: stale dups from >24h ago counted
    // toward the pill but were invisible to the modal and untouchable by
    // cleanup. Now any clean note with a non-empty cache id is in scope.
    let candidates: Vec<db::CachedNote> = state.db
        .list_notes(account_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|n| {
            matches!(n.sync_state, db::SyncState::Clean) && !n.id.is_empty()
        })
        .collect();

    log!(
        "cleanup_orphans: scanning {} clean note(s) for account {}",
        candidates.len(), account_id
    );

    let mut trashed_total: usize = 0;
    for note in candidates {
        // Skip if any push is in flight for this uuid. Pushing entries are
        // added by sync_worker_tick lines 1161/1182 before gmail::save_note
        // and removed after — covers the only path that mutates Gmail for
        // this uuid (frontend save_note is local-first only).
        let in_flight = {
            let p = state.pushing.lock().unwrap();
            p.contains(&(account_id.to_string(), note.uuid.clone()))
        };
        if in_flight {
            continue;
        }

        let found = dup_map.get(&note.uuid).cloned().unwrap_or_default();
        if found.len() <= 1 {
            continue; // no duplicates to clean
        }

        // For each candidate, re-verify safety RIGHT BEFORE trashing.
        // This closes the TOCTOU window: between scan completion and trash,
        // a new save could land. If it does, cache.id moves and we bail.
        for gmail_id in found {
            if gmail_id == note.id {
                continue; // this is our live one
            }
            let still_safe = {
                let p = state.pushing.lock().unwrap();
                if p.contains(&(account_id.to_string(), note.uuid.clone())) {
                    false
                } else {
                    match state.db.get(&note.uuid, account_id) {
                        Ok(Some(cur)) => cur.id == note.id,
                        _ => false,
                    }
                }
            };
            if !still_safe {
                log!(
                    "cleanup_orphans: bailing uuid={} — state moved during scan",
                    note.uuid
                );
                break;
            }
            let v_del = vertical_from_parts(state, account_id, token.clone(), label_map.clone())?;
            match v_del.delete(&gmail_id).await.map_err(|e| e.to_string()) {
                Ok(_) => {
                    trashed_total += 1;
                    log!(
                        "cleanup_orphans: trashed orphan id={} for uuid={}",
                        gmail_id, note.uuid
                    );
                }
                Err(e) => {
                    log!(
                        "cleanup_orphans: trash failed id={}: {}",
                        gmail_id, e
                    );
                }
            }
        }
    }
    log!(
        "cleanup_orphans: trashed {} total orphan(s) for {}",
        trashed_total, account_id
    );
    Ok(trashed_total)
}

#[tauri::command]
pub async fn cleanup_orphans(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let trashed = safe_cleanup_orphans_for_account(&state, &account_id).await?;
    // No optimistic decrement of dup_stats here. The previous "subtract
    // trashed from collapsed" looked responsive but caused the pill to
    // flicker: Gmail's index is eventually consistent, so the next
    // list_notes often still saw the just-trashed messages, the pill
    // jumped back up, then dropped again on the poll after. The next
    // list_notes is the single source of truth — frontend should trigger
    // a refresh after cleanup if it wants the pill to update sooner.
    Ok(trashed)
}

#[tauri::command]
pub fn get_dup_stats(
    account_id: String,
    state: State<'_, AppState>,
) -> gmail::DedupSummary {
    state
        .dup_stats
        .lock()
        .unwrap()
        .get(&account_id)
        .cloned()
        .unwrap_or_default()
}

// ─── Orphan review (Tier 2 — shows duplicates before trashing) ──────────────

#[derive(serde::Serialize, Clone, Debug)]
pub struct OrphanVersion {
    pub id: String,
    pub title: String,
    pub date: String,
    /// Plain-text preview of the body, first ~200 chars after stripping HTML.
    pub body_preview: String,
    pub label: String,
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct OrphanGroup {
    pub uuid: String,
    pub keeper: OrphanVersion,
    /// The other Gmail messages with the same X-UUID. These would be trashed
    /// on user confirmation. Order: most recent first.
    pub orphans: Vec<OrphanVersion>,
    /// Ids of other messages with this X-UUID that could not be read even
    /// after retrying. They are still duplicates — the "N dup" pill counts
    /// them — so the modal must say so rather than show fewer than the pill.
    /// Measured 2026-11-17: the header scan spent the Gmail quota, the next 4
    /// fetches came back rate-limited, and the modal showed 2 of 7 notes.
    pub unloaded: Vec<String>,
}

/// One review group from the orphan fetches for `uuid`: the ones that loaded
/// (newest first) and the ids that did not. `None` when there is nothing
/// besides the keeper.
fn orphan_group(
    uuid: &str,
    keeper: OrphanVersion,
    fetched: Vec<(String, Result<OrphanVersion, String>)>,
) -> Option<OrphanGroup> {
    let mut orphans = Vec::new();
    let mut unloaded = Vec::new();
    for (id, r) in fetched {
        match r {
            Ok(v) => orphans.push(v),
            Err(e) => {
                log!("preview_orphans: fetch failed id={}: {}", id, e);
                unloaded.push(id);
            }
        }
    }
    if orphans.is_empty() && unloaded.is_empty() {
        return None;
    }
    // Most-recent first so the user sees the freshest "almost-keeper"
    // candidate at the top of each group.
    let parse = |s: &str| chrono::DateTime::parse_from_rfc2822(s).ok();
    orphans.sort_by(|a, b| parse(&b.date).cmp(&parse(&a.date)));
    Some(OrphanGroup { uuid: uuid.to_string(), keeper, orphans, unloaded })
}

/// Strip HTML tags and decode &nbsp; from body for a clean text preview.
fn body_to_preview(body_html: &str, max_chars: usize) -> String {
    // Crude but adequate: strip <tags>, collapse whitespace.
    let no_tags: String = {
        let mut out = String::with_capacity(body_html.len());
        let mut in_tag = false;
        for c in body_html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    };
    let collapsed: String = no_tags
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.chars().count() <= max_chars {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(max_chars).collect();
        format!("{}…", truncated)
    }
}

#[tauri::command]
pub async fn preview_orphans(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<OrphanGroup>, String> {
    // Orphan preview only applies to Gmail — see `orphan_cleanup_supported`'s
    // doc comment.
    {
        let list = state.accounts.lock().unwrap();
        if let Some(a) = list.iter().find(|a| a.id == account_id) {
            if !orphan_cleanup_supported(a.backend_kind) {
                log!(
                    "preview_orphans: skipping non-Gmail account {} ({:?}) — no dups possible",
                    account_id, a.backend_kind
                );
                return Ok(Vec::new());
            }
        }
    }
    log!("preview_orphans: starting for account {}", account_id);
    let token = ensure_token(&state, &account_id).await?;
    let label_map = cached_label_map(&state, &account_id, &token).await?;
    log!("preview_orphans: token + label_map ready ({} labels)", label_map.len());
    // One bulk uuid->ids scan instead of one full Notes/* re-scan per note
    // (see find_all_duplicate_ids doc comment — this used to be O(candidates
    // * mailbox size) and is what made this modal appear to hang forever).
    let dup_map = gmail::find_all_duplicate_ids(&token, &label_map).await?;
    log!("preview_orphans: dup scan found {} uuid(s) with >1 message", dup_map.iter().filter(|(_, v)| v.len() > 1).count());
    // No 24h recent-edit gate — see safe_cleanup_orphans_for_account for
    // the rationale. The modal must show every dup the sidebar's "N dup"
    // pill is counting; otherwise the user clicks cleanup and watches the
    // pill stay the same.
    let candidates: Vec<db::CachedNote> = state
        .db
        .list_notes(&account_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|n| {
            matches!(n.sync_state, db::SyncState::Clean) && !n.id.is_empty()
        })
        .collect();

    let mut groups: Vec<OrphanGroup> = Vec::new();
    log!("preview_orphans: {} candidates to scan", candidates.len());
    for note in candidates {
        // Skip in-flight pushes — same safety rule as cleanup.
        let in_flight = {
            let p = state.pushing.lock().unwrap();
            p.contains(&(account_id.to_string(), note.uuid.clone()))
        };
        if in_flight {
            continue;
        }
        let ids = dup_map.get(&note.uuid).cloned().unwrap_or_default();
        if ids.len() <= 1 {
            continue;
        }

        let keeper = OrphanVersion {
            id: note.id.clone(),
            title: note.title.clone(),
            date: note.date.clone(),
            body_preview: body_to_preview(&note.body_html, 200),
            label: note.label.clone(),
        };

        let mut fetched = Vec::new();
        for id in ids {
            if id == note.id {
                continue;
            }
            // Fetch each orphan so the user can see what's about to be trashed.
            // Slow on big mailboxes with many duplicates — this is explicit
            // user action, latency is acceptable. Retried like a listing's
            // fetch: the header scan just above spends most of the Gmail
            // quota, so the first fetches after it are the likeliest to be
            // rate-limited (gotcha #42).
            let r = gmail::retry_rate_limited(
                || gmail::fetch_note(&token, &id, &label_map),
                &gmail::RATE_LIMIT_BACKOFF,
            )
            .await
            .map(|n| OrphanVersion {
                id: n.id,
                title: n.title,
                date: n.date,
                body_preview: body_to_preview(&n.body_html, 200),
                label: n.label,
            });
            fetched.push((id, r));
        }
        if let Some(g) = orphan_group(&note.uuid, keeper, fetched) {
            groups.push(g);
        }
    }
    log!("preview_orphans: returning {} group(s)", groups.len());
    Ok(groups)
}

/// Trash specific Gmail message ids. Used by the review modal after the
/// user confirms which orphans to clean up.
///
/// Safety re-checks every id immediately before the API call: it must not
/// be the current cache.id for ANY note (would be trashing the live one),
/// and the cache row whose uuid owns it must not have an in-flight push.
/// Either failure makes us skip that id.
#[tauri::command]
pub async fn trash_specific_messages(
    account_id: String,
    message_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    if message_ids.is_empty() {
        return Ok(0);
    }
    // Build a set of all current cache.ids for this account so we can refuse
    // to trash any of them. The cleanup_orphans path already filters by
    // re-reading per-uuid; for the explicit-id path we need a different
    // shape: which uuid owns each id, and is that uuid clean?
    let cached: HashMap<String, (String, db::SyncState)> = state
        .db
        .list_notes(&account_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|n| (n.id, (n.uuid, n.sync_state)))
        .collect();
    // Fetch the vertical once (token + label_map + meta_label) for all deletes.
    let v = vertical_for(&state, &account_id).await?;

    let mut trashed = 0;
    for id in message_ids {
        if id.is_empty() {
            continue;
        }
        if let Some((uuid, _state)) = cached.get(&id) {
            log!(
                "trash_specific_messages: refusing to trash id={} — it's the live cache id for uuid={}",
                id, uuid
            );
            continue;
        }
        let in_flight = {
            let p = state.pushing.lock().unwrap();
            // We don't know the uuid since this id isn't in the cache (it's
            // an orphan from Gmail). Conservative: if ANY push is in flight
            // for this account, skip. Keeps timing simple; orphans aren't
            // urgent and the next review will surface them again.
            p.iter().any(|(aid, _)| aid == &account_id)
        };
        if in_flight {
            log!(
                "trash_specific_messages: deferring id={} — pushes in flight",
                id
            );
            continue;
        }
        match v.delete(&id).await.map_err(|e| e.to_string()) {
            Ok(_) => {
                trashed += 1;
                log!("trash_specific_messages: trashed id={}", id);
            }
            Err(e) => log!("trash_specific_messages: trash failed id={}: {}", id, e),
        }
    }
    // No optimistic dup_stats decrement — same rationale as cleanup_orphans:
    // Gmail's index is eventually consistent so the next list_notes is the
    // single source of truth. Decrementing here caused the "N dup" pill to
    // flicker after cleanup, which read as a bug to users.
    Ok(trashed)
}

#[cfg(test)]
mod orphan_group_tests {
    use super::*;

    fn v(id: &str, date: &str) -> OrphanVersion {
        OrphanVersion {
            id: id.into(),
            title: "t".into(),
            date: date.into(),
            body_preview: String::new(),
            label: "Notes".into(),
        }
    }

    /// The measured 2026-11-17 case: every orphan of a note came back
    /// rate-limited. The group used to be dropped, so the modal showed fewer
    /// notes than the pill and the copy could not be trashed from it.
    #[test]
    fn a_note_whose_orphans_all_failed_to_load_is_still_a_group() {
        let g = orphan_group(
            "U1",
            v("keep", ""),
            vec![("o1".into(), Err("rate limited".into()))],
        )
        .expect("a duplicate that could not be read is still a duplicate");
        assert!(g.orphans.is_empty());
        assert_eq!(g.unloaded, vec!["o1".to_string()]);
    }

    #[test]
    fn loaded_and_unloaded_orphans_are_kept_apart_newest_first() {
        let g = orphan_group(
            "U1",
            v("keep", ""),
            vec![
                ("old".into(), Ok(v("old", "Thu, 17 Sep 2026 11:44:20 +0700"))),
                ("gone".into(), Err("rate limited".into())),
                ("new".into(), Ok(v("new", "Sat, 26 Sep 2026 14:16:05 +0700"))),
            ],
        )
        .unwrap();
        let ids: Vec<&str> = g.orphans.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["new", "old"]);
        assert_eq!(g.unloaded, vec!["gone".to_string()]);
    }

    #[test]
    fn no_orphans_at_all_is_no_group() {
        assert!(orphan_group("U1", v("keep", ""), Vec::new()).is_none());
    }
}
