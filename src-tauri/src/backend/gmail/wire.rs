use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::log;

// Neutral note envelope types now live in the shared core module; re-export
// them here so callers via `wire::Note` etc. continue to resolve unchanged.
pub use crate::backend::{Note, Attachment, SavedNote, MessageIndex, TrashedNote, DedupSummary};

// mime822 extraction (Phase 1): these helpers moved to the shared module.
// Internal use only — no longer re-exported (shim removed, lib.rs calls mime822 directly).
use crate::mime822::{
    canonicalize_uuid, decode_b64_bytes, decode_body,
    format_apple_date, format_apple_uuid,
    inject_title_into_body, referenced_cids,
    strip_leading_title,
    try_recover_mis_decoded_utf8, APPLE_MIME_VERSION,
};

#[derive(Deserialize, Debug)]
struct MessageList {
    messages: Option<Vec<MessageRef>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize, Debug)]
struct MessageRef {
    id: String,
}

#[derive(Deserialize, Debug)]
struct GmailMessage {
    id: String,
    payload: Payload,
    #[serde(rename = "labelIds")]
    label_ids: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
struct Payload {
    headers: Vec<Header>,
    body: Option<Body>,
    parts: Option<Vec<Part>>,
}

#[derive(Deserialize, Debug)]
struct Header {
    name: String,
    value: String,
}

#[derive(Deserialize, Debug)]
struct Body {
    data: Option<String>,
    // Gmail returns larger parts (e.g. Apple Notes images) by reference, not
    // inline — `data` is null and `attachmentId` points at the bytes, fetched
    // via the messages/{id}/attachments/{attachmentId} endpoint.
    #[serde(rename = "attachmentId", default)]
    attachment_id: Option<String>,
}

#[derive(Deserialize, Debug)]
struct Part {
    #[serde(rename = "mimeType")]
    mime_type: String,
    // Gmail omits `body` on some container parts in multipart/related from
    // older Apple Notes versions (4.11 = macOS 10.14). The actual text/html
    // content sits in a deeper child part. Keep this Optional so we tolerate
    // those shapes — find_html_in_parts walks the tree regardless.
    #[serde(default)]
    body: Option<Body>,
    #[serde(default)]
    parts: Option<Vec<Part>>,
    // Per-part headers (Content-Id, Content-Disposition, Content-Type params) —
    // needed to identify and label inline attachments. filename is the
    // convenience field Gmail also exposes for attachment parts.
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    headers: Option<Vec<Header>>,
}

#[derive(Deserialize, Debug)]
struct LabelList {
    labels: Vec<GmailLabel>,
}

#[derive(Deserialize, Debug)]
struct GmailLabel {
    id: String,
    name: String,
}

#[derive(Deserialize, Debug)]
struct InsertResponse {
    id: String,
}

fn get_header(headers: &[Header], name: &str) -> String {
    headers
        .iter()
        .find(|h| h.name.to_lowercase() == name.to_lowercase())
        .map(|h| h.value.clone())
        .unwrap_or_default()
}

// Walk the parts tree looking for the first text/html with a non-empty body.
// Apple Notes' edited messages often nest content under multipart/alternative
// or multipart/mixed — a flat search would miss them.
fn find_html_in_parts(parts: Option<&[Part]>) -> Option<String> {
    let parts = parts?;
    for p in parts {
        // Some container parts (e.g. multipart/related root) have no body —
        // p.body is None there. Skip the body extraction but still recurse.
        if p.mime_type == "text/html" {
            if let Some(data) = p.body.as_ref().and_then(|b| b.data.as_deref()) {
                let decoded = decode_body(data);
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }
        if let Some(nested) = find_html_in_parts(p.parts.as_deref()) {
            return Some(nested);
        }
    }
    None
}

#[derive(Deserialize, Debug)]
struct ProfileResponse {
    #[serde(rename = "emailAddress")]
    email_address: String,
}

// Fetch the authenticated user's email via Gmail's getProfile.
// Called once and cached in AppState by the Tauri command layer.
pub async fn get_user_email(token: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let res = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/profile")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("getProfile failed {}: {}", status, text));
    }
    let p: ProfileResponse = res.json().await.map_err(|e| e.to_string())?;
    Ok(p.email_address)
}

pub async fn get_label_map(token: &str) -> Result<HashMap<String, String>, String> {
    let client = reqwest::Client::new();
    let res = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/labels")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = res.status();
    let body = res.text().await.map_err(|e| e.to_string())?;

    if !status.is_success() {
        log!("labels.list HTTP {} — body: {}", status, body);
        return Err(format!("labels.list failed: {} — {}", status, body));
    }

    let parsed: LabelList = serde_json::from_str(&body).map_err(|e| {
        log!("labels.list parse error: {} — body: {}", e, body);
        e.to_string()
    })?;

    Ok(parsed.labels.into_iter().map(|l| (l.id, l.name)).collect())
}

// Walk every page of `messages.list` for one labelId and return all message
// IDs. Gmail caps a single page at 500; mailboxes with thousands of notes
// would otherwise stop at page 1 and the rest would silently disappear.
//
// This intentionally returns IDs only (no `messages.get`) — callers decide
// what to do with them (count, cross-ref against cache, hydrate later).
pub async fn list_all_message_ids(
    token: &str,
    label_id: &str,
) -> Result<Vec<String>, String> {
    let client = reqwest::Client::new();
    let mut all: Vec<String> = Vec::new();
    let mut page_token: Option<String> = None;
    let mut page = 0usize;
    loop {
        page += 1;
        let mut query: Vec<(&str, String)> = vec![
            ("labelIds", label_id.to_string()),
            ("maxResults", "500".to_string()),
        ];
        if let Some(t) = &page_token {
            query.push(("pageToken", t.clone()));
        }
        let res = client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
            .bearer_auth(token)
            .query(&query)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(format!(
                "messages.list HTTP {} (label={}, page={}): {}",
                status, label_id, page, text
            ));
        }
        let list: MessageList = res.json().await.map_err(|e| e.to_string())?;
        if let Some(msgs) = list.messages {
            all.extend(msgs.into_iter().map(|m| m.id));
        }
        match list.next_page_token {
            Some(t) if !t.is_empty() => page_token = Some(t),
            _ => break,
        }
    }
    Ok(all)
}

/// Cheap account-wide index: every Notes message's `id` paired with the
/// `label` it lives under. Paginated; no `messages.get` calls — typically
/// finishes in seconds even for a 6k+ note mailbox.
///
/// A single message can carry multiple Notes labels (e.g. "Notes" plus
/// "Notes/Work"). Apple Notes stamps the parent "Notes" label on every
/// note in addition to the sub-folder, so a HashMap-order dedup would
/// frequently attribute the message to bare "Notes" and leave the sub-
/// folder count at 0. We walk labels MOST-SPECIFIC FIRST (deepest path,
/// bare "Notes" last) so the dedup attributes each message to its
/// deepest sub-label — same rule `fetch_note` uses for the hydrated note's
/// `label`, keeping the index and hydrated state in agreement.
pub async fn list_account_index(
    token: &str,
    account: &str,
    label_map: &HashMap<String, String>,
) -> Result<Vec<MessageIndex>, String> {
    let mut notes_label_ids: Vec<(String, String)> = label_map
        .iter()
        .filter(|(_, name)| name.as_str() == "Notes" || name.starts_with("Notes/"))
        .map(|(id, name)| (id.clone(), name.clone()))
        .collect();
    notes_label_ids.sort_by(|a, b| {
        let depth = |s: &str| s.matches('/').count();
        depth(&b.1).cmp(&depth(&a.1)).then_with(|| a.1.cmp(&b.1))
    });

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<MessageIndex> = Vec::new();
    for (label_id, label_name) in &notes_label_ids {
        match list_all_message_ids(token, label_id).await {
            Ok(ids) => {
                log!(
                    "index[{}]: label {} ({}) → {} messages",
                    account, label_name, label_id, ids.len()
                );
                for id in ids {
                    if seen.insert(id.clone()) {
                        out.push(MessageIndex {
                            id,
                            label: label_name.clone(),
                        });
                    }
                }
            }
            Err(e) => log!("index[{}]: messages.list failed for {}: {}", account, label_id, e),
        }
    }
    log!("index[{}]: total {} unique messages across {} labels", account, out.len(), notes_label_ids.len());
    Ok(out)
}

// List message ids that carry BOTH `label_id` AND TRASH (a trashed note in that
// folder). messages.list excludes trash by default, so we opt in explicitly.
async fn list_trashed_ids_in_label(token: &str, label_id: &str) -> Result<Vec<String>, String> {
    let client = reqwest::Client::new();
    let mut all: Vec<String> = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut query: Vec<(&str, String)> = vec![
            ("labelIds", label_id.to_string()),
            ("labelIds", "TRASH".to_string()),
            ("includeSpamTrash", "true".to_string()),
            ("maxResults", "500".to_string()),
        ];
        if let Some(t) = &page_token {
            query.push(("pageToken", t.clone()));
        }
        let res = client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
            .bearer_auth(token)
            .query(&query)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(format!("trash list HTTP {} (label={}): {}", status, label_id, text));
        }
        let list: MessageList = res.json().await.map_err(|e| e.to_string())?;
        if let Some(msgs) = list.messages {
            all.extend(msgs.into_iter().map(|m| m.id));
        }
        match list.next_page_token {
            Some(t) if !t.is_empty() => page_token = Some(t),
            _ => break,
        }
    }
    Ok(all)
}

// Fetch a trashed message's metadata; returns None if it isn't an Apple note.
async fn fetch_trashed_meta(
    token: &str,
    id: &str,
    label_map: &HashMap<String, String>,
) -> Result<Option<TrashedNote>, String> {
    let client = reqwest::Client::new();
    let res = client
        .get(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
            id
        ))
        .bearer_auth(token)
        .query(&[
            ("format", "metadata"),
            ("metadataHeaders", "Subject"),
            ("metadataHeaders", "X-Universally-Unique-Identifier"),
            ("metadataHeaders", "X-Uniform-Type-Identifier"),
            ("metadataHeaders", "Date"),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("trash meta HTTP {} (id={})", res.status(), id));
    }
    let msg: GmailMessage = res.json().await.map_err(|e| e.to_string())?;
    let headers = &msg.payload.headers;
    if get_header(headers, "x-uniform-type-identifier") != "com.apple.mail-note" {
        return Ok(None); // not a note (other trashed email)
    }
    let uuid_raw = get_header(headers, "x-universally-unique-identifier");
    let uuid = canonicalize_uuid(&uuid_raw).unwrap_or(uuid_raw);
    let title_raw = get_header(headers, "subject");
    let title = try_recover_mis_decoded_utf8(&title_raw).unwrap_or(title_raw);
    let date = get_header(headers, "date");
    let label = pick_notes_label(msg.label_ids.as_deref().unwrap_or(&[]), label_map);
    Ok(Some(TrashedNote { id: msg.id, uuid, title, date, label }))
}

/// List every Apple note currently in Gmail Trash (across all Notes folders).
/// Caller filters out edit-revisions (uuids still live) — see lib.rs.
pub async fn list_trashed_notes(
    token: &str,
    label_map: &HashMap<String, String>,
) -> Result<Vec<TrashedNote>, String> {
    let note_label_ids: Vec<String> = label_map
        .iter()
        .filter(|(_, name)| name.as_str() == "Notes" || name.starts_with("Notes/"))
        .map(|(id, _)| id.clone())
        .collect();

    // Parallelize the per-label trash queries (was sequential = 20-40 round
    // trips = several seconds of spinner). One spawned task per label.
    let mut label_tasks = Vec::with_capacity(note_label_ids.len());
    for lid in note_label_ids {
        let tok = token.to_string();
        label_tasks.push(tokio::spawn(async move {
            list_trashed_ids_in_label(&tok, &lid).await.unwrap_or_default()
        }));
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ids: Vec<String> = Vec::new();
    for t in label_tasks {
        if let Ok(found) = t.await {
            for id in found {
                if seen.insert(id.clone()) {
                    ids.push(id);
                }
            }
        }
    }

    // Parallelize the metadata fetches too.
    let lm = std::sync::Arc::new(label_map.clone());
    let mut meta_tasks = Vec::with_capacity(ids.len());
    for id in ids {
        let tok = token.to_string();
        let lm = lm.clone();
        meta_tasks.push(tokio::spawn(async move {
            fetch_trashed_meta(&tok, &id, &lm).await.ok().flatten()
        }));
    }
    let mut out: Vec<TrashedNote> = Vec::new();
    for t in meta_tasks {
        if let Ok(Some(tn)) = t.await {
            out.push(tn);
        }
    }
    log!("list_trashed_notes: {} trashed note(s)", out.len());
    Ok(out)
}

/// Restore a trashed note: untrash the Gmail message so it returns to its
/// Notes folder (the original labels are retained through trashing).
pub async fn untrash_note(token: &str, id: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    let res = client
        .post(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}/untrash",
            id
        ))
        .bearer_auth(token)
        // Google rejects body-less POSTs with HTTP 411 unless we send an
        // explicit Content-Length: 0 (reqwest won't emit it for an empty body).
        // Same as the trash call above.
        .header(reqwest::header::CONTENT_LENGTH, "0")
        .body(Vec::<u8>::new())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("untrash HTTP {} (id={}): {}", status, id, text));
    }
    Ok(())
}

/// Pick the single Notes-tree label to expose for a message, given the set of
/// label IDs it carries. Resolves IDs to names, keeps only `Notes` / `Notes/*`,
/// and prefers a sub-label (e.g. `Notes/Work`) over the plain `Notes` root.
///
/// This is the ONE authoritative label-selection rule. `fetch_note` uses it on
/// a freshly-fetched message; the cache fast-paths in `list_notes` /
/// `list_notes_in_label` use it to recompute the label of a reused cached note
/// from the labels it was actually listed under THIS pass — so a remote label
/// move (which leaves the Gmail message id unchanged, and therefore slips past
/// the id-keyed cache) is reconciled instead of preserving the stale label.
fn pick_notes_label(label_ids: &[String], label_map: &HashMap<String, String>) -> String {
    let label_names: Vec<String> = label_ids
        .iter()
        .map(|id| label_map.get(id).cloned().unwrap_or_else(|| id.clone()))
        .filter(|name| name == "Notes" || name.starts_with("Notes/"))
        .collect();
    label_names
        .iter()
        .find(|n| n.starts_with("Notes/"))
        .or_else(|| label_names.first())
        .cloned()
        .unwrap_or_else(|| "Notes".to_string())
}

pub async fn list_notes(
    token: &str,
    label_map: &HashMap<String, String>,
    cache_by_id: &HashMap<String, Note>,
) -> Result<(Vec<Note>, DedupSummary), String> {
    log!("Loaded {} Gmail labels (from cache or fresh)", label_map.len());

    // Find every label that's "Notes" or a sub-label "Notes/...".
    // Querying by labelIds (the API's native field) is more reliable than q=label:Notes.
    let notes_label_ids: Vec<String> = label_map
        .iter()
        .filter(|(_, name)| name.as_str() == "Notes" || name.starts_with("Notes/"))
        .map(|(id, name)| {
            log!("  notes-label: {} = {}", id, name);
            id.clone()
        })
        .collect();

    if notes_label_ids.is_empty() {
        log!("WARNING: no Notes label found. All labels:");
        for (id, name) in label_map {
            log!("  {} = {}", id, name);
        }
        return Ok((vec![], DedupSummary::default()));
    }

    // Collect message IDs across every Notes label (sub-folders included).
    // Per-label paginated walk via list_all_message_ids — Gmail's page cap is
    // 500, so a mailbox with 6k+ notes previously stopped at page 1 and the
    // rest silently disappeared. Dedup across labels (a message can carry
    // multiple Notes labels — e.g. "Notes" + "Notes/Work").
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut all_ids: Vec<String> = Vec::new();
    // Per-message set of Notes label IDs it was listed under this pass. Because
    // we query every Notes label and record which ones returned each message,
    // this reconstructs the message's Notes-tree label membership WITHOUT a
    // messages.get — exactly the input `pick_notes_label` needs to recompute a
    // reused cached note's label (see the cache-reuse loop below).
    let mut id_to_label_ids: HashMap<String, Vec<String>> = HashMap::new();
    for label_id in &notes_label_ids {
        match list_all_message_ids(token, label_id).await {
            Ok(ids) => {
                log!("label {} returned {} messages (all pages)", label_id, ids.len());
                for id in ids {
                    id_to_label_ids
                        .entry(id.clone())
                        .or_default()
                        .push(label_id.clone());
                    if seen_ids.insert(id.clone()) {
                        all_ids.push(id);
                    }
                }
            }
            Err(e) => log!("messages.list failed for label {}: {}", label_id, e),
        }
    }

    // Cache-aware fan-out: anything already hydrated in SQLite (by message
    // id) is reused as-is — saves a `messages.get` call per cached note.
    // On a 6k mailbox the first cold run pays the full cost; every refresh
    // afterward only pays for newly-arrived messages.
    let mut from_cache: Vec<Note> = Vec::new();
    let mut to_fetch: Vec<String> = Vec::new();
    let mut relabeled = 0;
    for id in &all_ids {
        if let Some(cached) = cache_by_id.get(id) {
            let mut note = cached.clone();
            // Reconcile a remote label move. The id-keyed cache reuses the note
            // wholesale, but a message relabeled in Gmail keeps the SAME id, so
            // the cached `label` can be stale (e.g. a deleted folder's label
            // lingering after the message was moved). Recompute from the labels
            // it was actually listed under this pass, using the same rule as
            // fetch_note, and correct the reused copy if they disagree.
            if let Some(label_ids) = id_to_label_ids.get(id) {
                let fresh = pick_notes_label(label_ids, label_map);
                if fresh != note.label {
                    log!(
                        "list_notes: cache relabel id={} '{}' -> '{}'",
                        id, note.label, fresh
                    );
                    note.label = fresh;
                    relabeled += 1;
                }
            }
            from_cache.push(note);
        } else {
            to_fetch.push(id.clone());
        }
    }
    log!(
        "{} ids total — {} reused from cache ({} relabeled), {} to fetch",
        all_ids.len(), from_cache.len(), relabeled, to_fetch.len()
    );

    // Parallelize messages.get with a concurrency cap. Gmail's per-user limit
    // is 250 quota units/sec; messages.get is 5 units, so cap of 8 = ~40
    // units/sec sustained — well under ceiling. Wall-clock for a 50-message
    // mailbox drops from ~12s sequential to ~1.5s.
    const FETCH_CONCURRENCY: usize = 8;
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(FETCH_CONCURRENCY));
    let label_map_arc = std::sync::Arc::new(label_map.clone());
    let token_arc = std::sync::Arc::new(token.to_string());

    let mut handles = Vec::with_capacity(to_fetch.len());
    for id in to_fetch {
        let permit = sem.clone();
        let lm = label_map_arc.clone();
        let tok = token_arc.clone();
        handles.push(tokio::spawn(async move {
            let _p = permit.acquire().await.ok()?;
            match fetch_note(&tok, &id, &lm).await {
                Ok(note) => Some(Ok(note)),
                Err(e) => Some(Err((id, e))),
            }
        }));
    }

    let mut notes = from_cache;
    let mut skipped = 0;
    for h in handles {
        match h.await {
            Ok(Some(Ok(note))) => notes.push(note),
            Ok(Some(Err((id, e)))) => {
                skipped += 1;
                if skipped <= 3 {
                    log!("skipped {}: {}", id, e);
                }
            }
            _ => skipped += 1,
        }
    }
    log!("returning {} notes (skipped {})", notes.len(), skipped);

    // ─── Dedupe by X-UUID: most recent Date wins, longer body as tiebreak ─
    // Apple Notes ↔ Jodd race conditions and our own insert-then-delete
    // pattern can leave multiple Gmail messages with the same X-UUID (the
    // logical note identity).
    //
    // Picking which one to keep is a design choice:
    //   - "longest body wins" catches truncated/broken saves but loses to
    //     legitimate evolution of a note (older verbose → newer terse).
    //   - "most recent Date wins" matches the user's mental model of "show
    //     me the latest version" and aligns with Apple Notes' reconciliation
    //     ("latest revision wins" by Date header).
    // We use most-recent-Date as the primary rule, falling back to length
    // for same-date or unparseable-date cases (a truncated save vs a real
    // save that happen to share a timestamp).
    let parse_date = |s: &str| chrono::DateTime::parse_from_rfc2822(s).ok();
    let mut by_uuid: HashMap<String, Note> = HashMap::new();
    let mut singleton: Vec<Note> = Vec::new();
    let mut duplicates_collapsed = 0;
    let mut conflicting_uuids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for note in notes {
        if note.uuid.is_empty() {
            singleton.push(note);
            continue;
        }
        match by_uuid.entry(note.uuid.clone()) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                duplicates_collapsed += 1;
                conflicting_uuids.insert(note.uuid.clone());
                let existing = entry.get();
                let e_dt = parse_date(&existing.date);
                let n_dt = parse_date(&note.date);
                let new_wins = match (e_dt, n_dt) {
                    (Some(e), Some(n)) if n != e => n > e,
                    // Same time, no time, or only-existing-has-time:
                    // fall back to longer body (catches truncated saves),
                    // then to lexicographic id as a true tiebreak. Without
                    // the id tiebreak a same-date/same-length pair would
                    // resolve by HashMap iteration order, which depends on
                    // Gmail's return order and isn't stable across polls —
                    // so cache.id could spontaneously flip between sweeps.
                    _ => {
                        let n_len = note.body_html.len();
                        let e_len = existing.body_html.len();
                        if n_len != e_len {
                            n_len > e_len
                        } else {
                            note.id > existing.id
                        }
                    }
                };
                if new_wins {
                    log!(
                        "dedupe: uuid={} new wins (date={} body={}b → date={} body={}b), dropping id={}",
                        note.uuid, existing.date, existing.body_html.len(),
                        note.date, note.body_html.len(), existing.id
                    );
                    entry.insert(note);
                } else {
                    log!(
                        "dedupe: uuid={} existing wins (date={} body={}b), dropping id={} (date={} body={}b)",
                        note.uuid, existing.date, existing.body_html.len(),
                        note.id, note.date, note.body_html.len()
                    );
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(note);
            }
        }
    }
    let mut notes: Vec<Note> = by_uuid.into_values().collect();
    notes.extend(singleton);
    if duplicates_collapsed > 0 {
        log!(
            "dedupe: collapsed {} duplicate(s) across {} UUID(s) in list — background cleanup disabled",
            duplicates_collapsed,
            conflicting_uuids.len()
        );
        // Inline background cleanup was DISABLED 2026-06-09 — captured a
        // stale keep_id and raced with subsequent saves, trashing the live
        // message. The safe replacement is cleanup_orphans (lib.rs) which
        // the user triggers manually; the count is surfaced to the UI via
        // the DedupSummary returned below.
    }
    // ─────────────────────────────────────────────────────────────────────

    // Sort by parsed Date header descending. String comparison breaks across
    // months/years and on different timezone offsets — parse to absolute time first.
    notes.sort_by(|a, b| {
        let parse = |s: &str| chrono::DateTime::parse_from_rfc2822(s).ok();
        let a_dt = parse(&a.date);
        let b_dt = parse(&b.date);
        b_dt.cmp(&a_dt)
    });
    let summary = DedupSummary {
        collapsed: duplicates_collapsed,
        uuids_affected: conflicting_uuids.len(),
    };
    Ok((notes, summary))
}

// Scoped fetch: same pipeline as list_notes but for one specific label only.
// Used when the user is focused on a single folder — saves the cost of
// querying every Notes sub-label. Returns notes whose `label` is exactly the
// passed path (no cross-folder dedup needed; one label = one labelIds query).
pub async fn list_notes_in_label(
    token: &str,
    account: &str,
    label_id: &str,
    label_map: &HashMap<String, String>,
    cache_by_id: &HashMap<String, Note>,
) -> Result<Vec<Note>, String> {
    // Paginated list — same reason as list_notes: one label can hold
    // thousands of messages and the API caps a single page at 500.
    let ids = list_all_message_ids(token, label_id).await?;
    let label_name = label_map.get(label_id).map(|s| s.as_str()).unwrap_or("?");
    log!(
        "list_notes_in_label[{}]: {} ({}) returned {} messages (all pages)",
        account, label_name, label_id, ids.len()
    );

    // Name of the folder being queried — used to detect a stale cached label.
    // Every returned message carries `label_id`, so its label must be this
    // folder or a sub-folder of it; anything else is a stale leftover.
    let queried_label = label_map.get(label_id).cloned();

    // Cache-aware split: reuse hydrated notes, fetch only the misses.
    let mut from_cache: Vec<Note> = Vec::new();
    let mut to_fetch: Vec<String> = Vec::new();
    for id in &ids {
        if let Some(cached) = cache_by_id.get(id) {
            let mut note = cached.clone();
            // Scoped reconcile of a remote label move. Unlike list_notes we only
            // queried one label, so we can't reconstruct the full label set —
            // but we know this message IS under `label_id`. If the cached label
            // is neither this folder nor a descendant of it, it's stale (the
            // message was moved here in Gmail while the id-keyed cache kept the
            // old label); correct it. A legitimately-deeper sub-label is kept.
            if let Some(q) = &queried_label {
                let is_self_or_descendant =
                    note.label == *q || note.label.starts_with(&format!("{}/", q));
                if !is_self_or_descendant {
                    log!(
                        "list_notes_in_label[{}]: cache relabel id={} '{}' -> '{}'",
                        account, id, note.label, q
                    );
                    note.label = q.clone();
                }
            }
            from_cache.push(note);
        } else {
            to_fetch.push(id.clone());
        }
    }

    const FETCH_CONCURRENCY: usize = 8;
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(FETCH_CONCURRENCY));
    let label_map_arc = std::sync::Arc::new(label_map.clone());
    let token_arc = std::sync::Arc::new(token.to_string());

    let mut handles = Vec::with_capacity(to_fetch.len());
    for id in to_fetch {
        let permit = sem.clone();
        let lm = label_map_arc.clone();
        let tok = token_arc.clone();
        handles.push(tokio::spawn(async move {
            let _p = permit.acquire().await.ok()?;
            fetch_note(&tok, &id, &lm).await.ok()
        }));
    }
    let mut notes: Vec<Note> = from_cache;
    for h in handles {
        if let Ok(Some(n)) = h.await {
            notes.push(n);
        }
    }

    // Dedup by UUID — most recent Date wins (same rule as list_notes). Within
    // a single folder, duplicates can still exist from race conditions or
    // pre-411-fix orphans.
    let parse_date = |s: &str| chrono::DateTime::parse_from_rfc2822(s).ok();
    let mut by_uuid: HashMap<String, Note> = HashMap::new();
    let mut singleton: Vec<Note> = Vec::new();
    for n in notes {
        if n.uuid.is_empty() {
            singleton.push(n);
            continue;
        }
        let existing = match by_uuid.get(&n.uuid) {
            None => { by_uuid.insert(n.uuid.clone(), n); continue; }
            Some(e) => e,
        };
        let n_dt = parse_date(&n.date);
        let e_dt = parse_date(&existing.date);
        let n_wins = match (n_dt, e_dt) {
            (Some(a), Some(b)) => a > b,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => n.body_html.len() > existing.body_html.len(),
        };
        if n_wins {
            by_uuid.insert(n.uuid.clone(), n);
        }
    }
    let mut out: Vec<Note> = by_uuid.into_values().collect();
    out.append(&mut singleton);
    out.sort_by(|a, b| {
        let parse = |s: &str| chrono::DateTime::parse_from_rfc2822(s).ok();
        parse(&b.date).cmp(&parse(&a.date))
    });
    Ok(out)
}


// Extract a `param=value` / `param="value"` parameter from a header's value.
// e.g. header_param(headers, "content-type", "x-apple-part-url"),
//      header_param(headers, "content-disposition", "filename").
fn header_param(headers: &[Header], header_name: &str, param: &str) -> Option<String> {
    let val = get_header(headers, header_name);
    if val.is_empty() {
        return None;
    }
    let needle = format!("{}=", param);
    // Case-insensitive locate of the param name (ASCII lowercase preserves byte
    // offsets, so the index is valid in the original `val`).
    let idx = val.to_ascii_lowercase().find(&needle)?;
    let after = val[idx + needle.len()..].trim_start();
    let v = if let Some(stripped) = after.strip_prefix('"') {
        stripped.split('"').next().unwrap_or("")
    } else {
        after
            .split(|c: char| c == ';' || c.is_whitespace())
            .next()
            .unwrap_or("")
    };
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

// A part identified as an inline attachment, before its bytes are resolved.
struct PendingAttachment {
    content_id: String,
    mime_type: String,
    filename: Option<String>,
    x_apple_part_url: Option<String>,
    inline_data: Option<String>,
    attachment_id: Option<String>,
}

// Walk the MIME tree, collecting every part that carries a Content-Id — Apple's
// inline attachments, referenced from the body via <object data="cid:…">. These
// are type-agnostic: images, PDFs, zips, .md, .eml, etc. The note body itself is
// the one text/html part WITHOUT a Content-Id, so the cid check below already
// excludes it; we additionally skip text/html defensively in case a future Apple
// version stamps the body with a cid.
fn collect_pending_attachments(parts: Option<&[Part]>, out: &mut Vec<PendingAttachment>) {
    let Some(parts) = parts else { return };
    for p in parts {
        collect_pending_attachments(p.parts.as_deref(), out);
        if p.mime_type == "text/html" {
            continue; // the note body, never an attachment
        }
        let headers = p.headers.as_deref().unwrap_or(&[]);
        let cid_raw = get_header(headers, "content-id");
        if cid_raw.is_empty() {
            continue;
        }
        let content_id = cid_raw
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_string();
        let filename = p
            .filename
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| header_param(headers, "content-disposition", "filename"))
            .or_else(|| header_param(headers, "content-type", "name"));
        let (inline_data, attachment_id) = match p.body.as_ref() {
            Some(b) => (b.data.clone(), b.attachment_id.clone()),
            None => (None, None),
        };
        out.push(PendingAttachment {
            content_id,
            mime_type: p.mime_type.clone(),
            filename,
            x_apple_part_url: header_param(headers, "content-type", "x-apple-part-url"),
            inline_data,
            attachment_id,
        });
    }
}

// Resolve a single attachment's bytes via the dedicated attachments endpoint
// (used when Gmail returned the part by `attachmentId` rather than inline).
async fn fetch_attachment_data(
    token: &str,
    msg_id: &str,
    attachment_id: &str,
) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}/attachments/{}",
            msg_id, attachment_id
        ))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("attachments.get failed {}: {}", status, text));
    }
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let data = v.get("data").and_then(|d| d.as_str()).unwrap_or_default();
    decode_b64_bytes(data).ok_or_else(|| "attachment data not valid base64".to_string())
}

pub async fn fetch_note(
    token: &str,
    id: &str,
    label_map: &HashMap<String, String>,
) -> Result<Note, String> {
    let client = reqwest::Client::new();
    let msg = client
        .get(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
            id
        ))
        .bearer_auth(token)
        // `fields` mask: only request what we actually parse.
        // Drops ~70% of response bytes (no snippet/sizeEstimate/raw/etc).
        // Note: we request 3 levels of nested parts because Apple's edited
        // notes often arrive as multipart/mixed → multipart/alternative →
        // [text/plain, text/html]. Gmail returns the full tree but our fields
        // mask must explicitly ask for it at each depth.
        .query(&[
            ("format", "full"),
            (
                // Same 3-level part tree as before, now also pulling per-part
                // headers/filename and body(attachmentId) so inline attachments
                // (images) can be identified and their bytes resolved.
                "fields",
                "id,labelIds,payload(headers,body/data,parts(mimeType,filename,headers,body(data,attachmentId),parts(mimeType,filename,headers,body(data,attachmentId),parts(mimeType,filename,headers,body(data,attachmentId)))))",
            ),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    // Two-step parse so we can capture the raw body when JSON deserialization
    // fails. Previously we used `.json::<GmailMessage>()` which throws away
    // the response body on error — leaving us with a generic "error decoding
    // response body" that tells us nothing about which message or what shape.
    let raw = msg.text().await.map_err(|e| e.to_string())?;
    let msg = match serde_json::from_str::<GmailMessage>(&raw) {
        Ok(m) => m,
        Err(e) => {
            // Show enough body to diagnose (first 800 chars) plus the error.
            // Most parse errors point at a specific field/type mismatch.
            let preview: String = raw.chars().take(800).collect();
            log!(
                "fetch_note: JSON parse FAILED for id={}: {} — body preview: {}",
                id, e, preview
            );
            return Err(format!("parse error: {} (see terminal for body preview)", e));
        }
    };

    let headers = &msg.payload.headers;

    // must be a mail-note
    let type_id = get_header(headers, "x-uniform-type-identifier");
    if type_id != "com.apple.mail-note" {
        return Err("Not a note".into());
    }

    // Read Subject; if it looks like raw UTF-8 mis-decoded as Latin-1
    // (pre-fix Jodd save), un-mis-decode it.
    let title_raw = get_header(headers, "subject");
    let title = try_recover_mis_decoded_utf8(&title_raw).unwrap_or(title_raw);
    let date = get_header(headers, "date");
    let uuid_raw = get_header(headers, "x-universally-unique-identifier");
    // Canonicalize to hyphenated form so notes saved by old Jodd (hyphen-stripped)
    // still match by UUID in our store.
    let uuid = canonicalize_uuid(&uuid_raw).unwrap_or(uuid_raw);
    let x_mail_created_date = {
        let v = get_header(headers, "x-mail-created-date");
        if v.is_empty() { None } else { Some(v) }
    };

    // Resolve label IDs to human-readable names, then pick the most specific
    // Notes-related label. Prefer a sub-label like "Notes/myNotes" over plain "Notes".
    let label = pick_notes_label(msg.label_ids.as_deref().unwrap_or(&[]), label_map);

    // Decode body — three sources, tried in order. We must FALL THROUGH from
    // a present-but-empty `body.data` to parts, because Gmail returns
    // `payload.body = { size: 0, data: null }` for many multipart-shaped
    // messages, with the real content in payload.parts. Without this fallback
    // those notes render as an empty editor (the bug we just hit).
    let body_html = msg
        .payload
        .body
        .as_ref()
        .and_then(|b| b.data.as_deref())
        .map(decode_body)
        .filter(|s| !s.is_empty())
        .or_else(|| find_html_in_parts(msg.payload.parts.as_deref()))
        .unwrap_or_default();

    if body_html.is_empty() {
        log!(
            "fetch_note: empty body for id={} (has body.data={:?}, parts.count={:?})",
            id,
            msg.payload.body.as_ref().and_then(|b| b.data.as_deref()).map(|d| d.len()),
            msg.payload.parts.as_ref().map(|p| p.len())
        );
    }

    // Apple's convention: first body element duplicates the title. Strip it so
    // the editor doesn't double-show the title. The title we expose comes from
    // the Subject header, which Gmail API has already RFC 2047-decoded for us.
    let body_html = strip_leading_title(&body_html, &title);

    // Collect inline attachments (images). Their bytes arrive either inline
    // (small parts) or by reference (`attachmentId`, a second GET). A failure
    // on one part is logged and skipped — never fails the whole note fetch.
    let mut pending = Vec::new();
    collect_pending_attachments(msg.payload.parts.as_deref(), &mut pending);
    let mut attachments = Vec::with_capacity(pending.len());
    for pa in pending {
        let bytes = if let Some(d) = pa.inline_data.as_deref() {
            decode_b64_bytes(d)
        } else if let Some(aid) = pa.attachment_id.as_deref() {
            match fetch_attachment_data(token, id, aid).await {
                Ok(b) => Some(b),
                Err(e) => {
                    log!("fetch_note: attachment fetch failed id={} cid={}: {}", id, pa.content_id, e);
                    None
                }
            }
        } else {
            None
        };
        if let Some(data) = bytes {
            attachments.push(Attachment {
                content_id: pa.content_id,
                mime_type: pa.mime_type,
                filename: pa.filename,
                x_apple_part_url: pa.x_apple_part_url,
                data,
            });
        }
    }
    if !attachments.is_empty() {
        log!("fetch_note: id={} captured {} attachment(s)", id, attachments.len());
    }

    Ok(Note {
        id: msg.id,
        uuid,
        title,
        body_html,
        date,
        label,
        x_mail_created_date,
        account_id: None, // stamped by the Tauri command layer after fetch
        pinned: false,    // local-only state; merged in by cache lookups, not parsed from wire
        attachments,
    })
}


pub async fn save_note(
    token: &str,
    title: &str,
    body_html: &str,
    existing_gmail_id: Option<&str>,
    existing_uuid: Option<&str>,
    existing_x_mail_created_date: Option<&str>,
    label: &str,
    user_email: &str,
    label_map: &HashMap<String, String>,
    // Attachments the body may reference via <object data="cid:…">. The save
    // path re-emits matching ones as multipart/related parts (with the original
    // Content-Id) instead of stripping them — the image data-loss fix.
    attachments: &[Attachment],
) -> Result<SavedNote, String> {
    // Preserve the X-UUID across saves so the note's identity is stable.
    // Canonicalize old hyphen-stripped UUIDs to Apple's standard hyphenated form.
    let uuid = existing_uuid
        .filter(|s| !s.is_empty())
        .and_then(canonicalize_uuid)
        .unwrap_or_else(|| format_apple_uuid(uuid::Uuid::new_v4()));

    let now_local = chrono::Local::now();
    let date_header = format_apple_date(now_local);
    // For new notes, creation date = now. For edits, preserve the original.
    let created_date = existing_x_mail_created_date
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| date_header.clone());

    // Title-injected view used ONLY to drive CID extraction below — the wire
    // body is (re-)injected inside build_note_mime, so this local is not the
    // bytes we send. inject_title_into_body is idempotent, so both injections
    // see the same input and agree.
    let body_for_cids = inject_title_into_body(body_html, title);

    // Which stored attachments does the body still reference? Only those get
    // re-emitted, so deleting an <object cid:…> from the body drops the image.
    let cids = referenced_cids(&body_for_cids);
    let used: Vec<&Attachment> = attachments
        .iter()
        .filter(|a| cids.iter().any(|c| c == &a.content_id))
        .collect();

    let used_mime: Vec<crate::mime822::MimeAttachment> = used.iter().map(|a| {
        crate::mime822::MimeAttachment {
            content_id: &a.content_id,
            mime_type: &a.mime_type,
            filename: a.filename.as_deref(),
            x_apple_part_url: a.x_apple_part_url.as_deref(),
            data: &a.data,
        }
    }).collect();
    let raw = crate::mime822::build_note_mime(
        title, body_html, &uuid, &date_header, &created_date, user_email, &used_mime,
    );

    let encoded = URL_SAFE.encode(raw.as_bytes());
    let client = reqwest::Client::new();

    // Resolve target label name → label ID. If the user is creating a note in
    // "Notes/myNotes" we need Gmail's Label_NNN, not the human-readable name.
    // Map is supplied by the caller (cached in AppState) — no round-trip here.
    let target_label_id = label_map
        .iter()
        .find(|(_, name)| name.as_str() == label)
        .map(|(id, _)| id.clone())
        .or_else(|| {
            // Fallback to root "Notes" label if the specified one wasn't found
            // (e.g. a brand-new sub-label the user hasn't created in Apple Notes yet)
            label_map
                .iter()
                .find(|(_, name)| name.as_str() == "Notes")
                .map(|(id, _)| id.clone())
        })
        .ok_or_else(|| format!("No matching Gmail label for '{}'", label))?;

    log!(
        "save_note: label='{}' → {}, existing_gmail_id={:?}, uuid={}",
        label, target_label_id, existing_gmail_id, uuid
    );

    let body = serde_json::json!({
        "raw": encoded,
        "labelIds": [target_label_id]
    });

    // internalDateSource=dateHeader: Gmail derives the message's internalDate
    // (what shows in the UI's date column and what dedupe by-Date compares)
    // from our Date: header instead of "now". That places each Jodd save at
    // the moment the user actually saved — the obvious UX expectation.
    //
    // Apple Notes' IMAP APPEND uses INTERNALDATE = X-Mail-Created-Date so
    // every revision of the same note clusters at the original creation
    // time. We deliberately don't replicate that — it surprises users who
    // edited a note today and expect Gmail to reflect "today". Gmail API
    // also doesn't accept an explicit internalDate on insert (the resource
    // field is output-only), so even matching Apple would require lying in
    // the Date: header. Not worth the cost.
    let res = client
        .post("https://gmail.googleapis.com/gmail/v1/users/me/messages")
        .bearer_auth(token)
        .query(&[("internalDateSource", "dateHeader")])
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("Save failed {}: {}", status, text));
    }

    let inserted: InsertResponse = res.json().await.map_err(|e| e.to_string())?;
    log!("save_note[{}]: inserted new id={}", user_email, inserted.id);

    // Replace, don't duplicate: best-effort delete the previous message after
    // the new one is safely in. We delete LAST so a network blip on delete
    // leaves a duplicate (recoverable) instead of data loss (not recoverable).
    if let Some(old_id) = existing_gmail_id.filter(|s| !s.is_empty()) {
        if old_id != inserted.id {
            // delete_note logs the "TRASHED" line (with account); this adds the
            // "which new id replaced it" context so a save's trash is traceable.
            match delete_note(token, user_email, old_id).await {
                Ok(_) => log!(
                    "save_note[{}]: old revision id={} replaced by id={}",
                    user_email, old_id, inserted.id
                ),
                Err(e) => log!(
                    "save_note[{}]: failed to trash old {}: {} (new note saved OK)",
                    user_email, old_id, e
                ),
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // Inline uuid-dedup cleanup is NOT spawned from this path.
    //
    // The previous fire-and-forget tokio::spawn captured keep_id at save
    // time and raced with subsequent saves — cleanup-N could mistake save-
    // N+1's live id for an orphan and trash it. Two notes were destroyed
    // this way in the v0.1.1 forensic session.
    //
    // save_note's OWN delete-old (just above) deletes the specific id it
    // knows is stale with full causal ordering and is sufficient for the
    // normal case. The safe replacement for the orphan-accumulator scenario
    // (delete-old failures, Apple Notes' simultaneous IMAP edits) is the
    // user-triggered `cleanup_orphans` Tauri command which calls
    // `safe_cleanup_orphans_for_account` in lib.rs. That path skips uuids
    // with in-flight pushes and re-reads cache.id immediately before each
    // trash, so a concurrent save cannot have its live message destroyed.
    let _ = (&inserted.id, &label_map);
    // ──────────────────────────────────────────────────────────────────────

    Ok(SavedNote {
        id: inserted.id,
        uuid,
        date: date_header,
        // Editor-view body (pre-inject). See SavedNote.body_html doc.
        body_html: body_html.to_string(),
    })
}

// Bulk equivalent of `find_gmail_ids_for_uuid` below: computes X-UUID ->
// [message ids] for EVERY message across all Notes/* labels in one pass.
//
// `find_gmail_ids_for_uuid` costs O(messages_in_Notes_labels) per call (it
// re-lists every Notes/* label, then re-fetches every message's header) — its
// own doc comment warns "restrict the caller's input to recent UUIDs". When
// the 24h recency gate on the orphan scan callers was removed (2026-06-09,
// see lib.rs safe_cleanup_orphans_for_account / preview_orphans), those
// callers started invoking it once per candidate note instead, turning an
// O(M) cost into O(candidates * M) — thousands of sequential HTTP calls that
// made the "Review duplicates" modal appear to hang forever on any mailbox
// with more than a couple dozen notes. Callers that need duplicate ids for
// MANY notes must call this ONCE and look up per-uuid from the result,
// instead of looping the single-uuid version.
pub async fn find_all_duplicate_ids(
    token: &str,
    label_map: &HashMap<String, String>,
) -> Result<HashMap<String, Vec<String>>, String> {
    let notes_label_ids: Vec<&String> = label_map
        .iter()
        .filter(|(_, name)| name.as_str() == "Notes" || name.starts_with("Notes/"))
        .map(|(id, _)| id)
        .collect();
    if notes_label_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut all_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for label_id in &notes_label_ids {
        if let Ok(ids) = list_all_message_ids(token, label_id).await {
            all_ids.extend(ids);
        }
    }
    if all_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Parallelize the per-message header fetch — same concurrency cap and
    // Gmail quota rationale as list_notes' messages.get fan-out above.
    const FETCH_CONCURRENCY: usize = 8;
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(FETCH_CONCURRENCY));
    let token_arc = std::sync::Arc::new(token.to_string());
    let mut handles = Vec::with_capacity(all_ids.len());
    for id in all_ids {
        let permit = sem.clone();
        let tok = token_arc.clone();
        handles.push(tokio::spawn(async move {
            let _p = permit.acquire().await.ok()?;
            let client = reqwest::Client::new();
            let res = client
                .get(format!(
                    "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
                    id
                ))
                .bearer_auth(tok.as_str())
                .query(&[
                    ("format", "metadata"),
                    ("metadataHeaders", "X-Universally-Unique-Identifier"),
                ])
                .send()
                .await
                .ok()?;
            if !res.status().is_success() {
                return None;
            }
            let msg: GmailMessage = res.json().await.ok()?;
            let raw = get_header(&msg.payload.headers, "x-universally-unique-identifier");
            let uuid = canonicalize_uuid(&raw).unwrap_or(raw);
            Some((id, uuid))
        }));
    }

    let mut by_uuid: HashMap<String, Vec<String>> = HashMap::new();
    for h in handles {
        if let Ok(Some((id, uuid))) = h.await {
            if !uuid.is_empty() {
                by_uuid.entry(uuid).or_default().push(id);
            }
        }
    }
    Ok(by_uuid)
}

// Find all Gmail message ids whose X-UUID header matches `target_uuid`.
// Walks every Notes/* label and fetches the header for each candidate.
//
// Returns the list of matching ids WITHOUT trashing anything — the caller
// decides what to do. This is the safe replacement for the old
// cleanup_stale_uuid_duplicates: that function captured keep_id at spawn
// time and raced with subsequent saves; this one just reports, and the
// caller (safe_cleanup_orphans_for_account in lib.rs) re-reads the cache's
// live id immediately before each trash to close the TOCTOU window.
//
// Cost: O(messages_in_Notes_labels) header fetches per call — fine for a
// single uuid, but see `find_all_duplicate_ids` above if scanning many notes.
pub async fn find_gmail_ids_for_uuid(
    token: &str,
    target_uuid: &str,
    label_map: &HashMap<String, String>,
) -> Result<Vec<String>, String> {
    if target_uuid.is_empty() {
        return Ok(Vec::new());
    }
    let notes_label_ids: Vec<&String> = label_map
        .iter()
        .filter(|(_, name)| name.as_str() == "Notes" || name.starts_with("Notes/"))
        .map(|(id, _)| id)
        .collect();
    if notes_label_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut all_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for label_id in &notes_label_ids {
        // Paginated — duplicates could otherwise hide on page 2+ of a big mailbox.
        if let Ok(ids) = list_all_message_ids(token, label_id).await {
            all_ids.extend(ids);
        }
    }
    if all_ids.is_empty() {
        return Ok(Vec::new());
    }

    let client = reqwest::Client::new();
    let mut matches = Vec::new();
    for id in all_ids {
        let res = match client
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
                id
            ))
            .bearer_auth(token)
            .query(&[("format", "metadata"), ("metadataHeaders", "X-Universally-Unique-Identifier")])
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !res.status().is_success() {
            continue;
        }
        let msg: GmailMessage = match res.json().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let msg_uuid = get_header(&msg.payload.headers, "x-universally-unique-identifier");
        let normalized = canonicalize_uuid(&msg_uuid).unwrap_or(msg_uuid);
        if normalized == target_uuid {
            matches.push(id);
        }
    }
    Ok(matches)
}

// "Delete" here is `messages.trash` — move to TRASH label, not permanent
// erase. Reason: our OAuth scope is gmail.modify, which explicitly does NOT
// grant the permanent `messages.delete` permission. The trash endpoint works
// with gmail.modify and is semantically what we want anyway:
//   - Replaced/orphaned messages stop appearing in our messages.list queries
//     (we filter to the Notes label; TRASH is a different label)
//   - Gmail auto-empties trash after 30 days
//   - The user can manually empty trash if they want immediate purge
// Apple Notes' IMAP path also uses STORE \Deleted + EXPUNGE which Gmail
// implements as "move to TRASH" — so we're matching Apple's effective semantics.
pub async fn delete_note(token: &str, account: &str, id: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    let mut last_err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(200 << attempt)).await;
        }
        let res = client
            .post(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}/trash",
                id
            ))
            .bearer_auth(token)
            // Google's frontend rejects body-less POSTs with HTTP 411 unless we
            // send an explicit Content-Length: 0. `.body("")` alone is not
            // enough — reqwest still doesn't emit the header for an empty body —
            // so set it explicitly.
            .header(reqwest::header::CONTENT_LENGTH, "0")
            .body(Vec::<u8>::new())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = res.status();
        if status.is_success() || status.as_u16() == 404 {
            // 200 OK = trashed. 404 = already gone / already trashed (idempotent OK).
            log!("delete_note[{}]: TRASHED id={} (status={}, attempt {})", account, id, status, attempt + 1);
            return Ok(());
        }
        let body = res.text().await.unwrap_or_default();
        last_err = format!("HTTP {} — {}", status, body);
        log!("delete_note[{}]: attempt {} failed for id={}: {}", account, attempt + 1, id, last_err);
        if status.is_client_error() {
            return Err(last_err);
        }
        // 5xx → retry
    }
    Err(format!("delete_note: id={} failed after 3 attempts: {}", id, last_err))
}

// ===========================================================================
// Folder (= Gmail label) management
//
// Apple Notes folders map 1:1 to Gmail labels under the "Notes/" hierarchy.
// E.g. "Notes/Recipes/Italian" = a label named exactly that. Apple's IMAP
// uses "/" as the hierarchy separator and Gmail's label naming follows the
// same convention, so we don't need any name translation.
// ===========================================================================

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FolderInfo {
    pub id: String,
    pub name: String,
}

// POST /labels — create a new label. Returns its id + name.
// Apple Notes' folders show as hidden-from-IMAP labels too, but for user-
// created folders we use the default (visible in Gmail web), which Apple
// will pick up on its next sync.
// ─── Jodd-managed sidecar messages (cross-instance metadata sync) ───────────
//
// Sidecars carry Jodd-only per-note state (currently: pin) that can't
// round-trip through Apple Notes. They live in a separate Gmail label
// (account.meta_label, default "Notes-Meta") outside the Notes/ hierarchy so
// Apple Notes — which only enumerates Notes/* labels — never sees them.
//
// Identity convention:
//   Subject = "___<note_uuid>"  (triple underscore sentinel + the uuid of
//   the note this sidecar documents). The sentinel guards against the user
//   manually dropping a real note into meta_label: anything NOT starting
//   with "___" is ignored on read.
//
// Existence semantics (current state):
//   sidecar present   = pinned (and any other state in the JSON body)
//   sidecar absent    = unpinned
//
// We TRASH on unpin rather than updating the body to {pinned:false}. This
// keeps the read path to a single messages.list with metadata-only header
// projection — no body fetch, no JSON parse per sidecar. The body field is
// retained as JSON for forward extensibility (tags, color, …) where the
// "exists vs absent" binary won't suffice.

pub const SIDECAR_SUBJECT_PREFIX: &str = "___";

/// Subject prefix for tag sidecars. Intentionally disjoint from
/// `SIDECAR_SUBJECT_PREFIX` (pin's `___`) — neither prefix is a prefix
/// of the other, so each sync's reader can `strip_prefix` its own and
/// safely ignore the other's. Tag sidecars carry a JSON body with the
/// canonical tag set, so the read path fetches FULL_CONTENT (unlike pin's
/// metadata-only listing).
pub const TAG_SIDECAR_SUBJECT_PREFIX: &str = "tags___";

/// One sidecar as returned by list_meta_sidecars — minimal projection
/// (no body fetch). For pin, existence is the signal.
#[derive(Debug, Clone)]
pub struct SidecarRef {
    /// Gmail message id of the sidecar.
    pub id: String,
    /// The note uuid this sidecar documents (parsed from Subject).
    pub note_uuid: String,
}

/// One tag sidecar with its parsed payload — the read path here is heavier
/// than `SidecarRef` because tags are variable-length state that has to
/// come back with the body, not just existence.
#[derive(Debug, Clone)]
pub struct TagSidecarRef {
    /// Gmail message id of the sidecar.
    pub id: String,
    /// The note uuid this sidecar documents (parsed from Subject).
    pub note_uuid: String,
    /// Canonical tag set for the note — sorted, normalized. May be empty,
    /// in which case the sidecar should be trashed on next push (a no-tags
    /// sidecar is a contradiction; we leave it for that one tick rather
    /// than racing the worker).
    pub tags: Vec<String>,
}

/// Resolve `label_path` to a Gmail label id, creating the label if it
/// doesn't exist yet. Used by the sync worker on first sidecar push for
/// an account — by then the user has had a chance to configure the
/// meta_label in Settings (if they want a non-default name) and we
/// materialize it lazily so unused accounts don't end up with an
/// empty "Notes-Meta" label cluttering Gmail.
pub async fn ensure_label(
    token: &str,
    label_path: &str,
    label_map: &HashMap<String, String>,
) -> Result<String, String> {
    if let Some((id, _)) = label_map.iter().find(|(_, n)| n.as_str() == label_path) {
        return Ok(id.clone());
    }
    let info = create_label(token, label_path).await?;
    Ok(info.id)
}

/// List every sidecar message under `meta_label_id`. Uses Gmail's
/// `format=metadata` projection scoped to the Subject header so we never
/// pay for a body fetch — sidecar existence + Subject parse is all the
/// pin-sync path needs. Subjects that don't start with `SIDECAR_SUBJECT_PREFIX`
/// are dropped silently (defensive against the user manually adding a
/// real note to the meta_label).
pub async fn list_meta_sidecars(
    token: &str,
    meta_label_id: &str,
) -> Result<Vec<SidecarRef>, String> {
    let client = reqwest::Client::new();
    let mut out: Vec<SidecarRef> = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut req = client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
            .bearer_auth(token)
            .query(&[
                ("labelIds", meta_label_id),
                ("maxResults", "500"),
            ]);
        if let Some(pt) = page_token.as_deref() {
            req = req.query(&[("pageToken", pt)]);
        }
        let res = req.send().await.map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            let s = res.status();
            let t = res.text().await.unwrap_or_default();
            return Err(format!("list_meta_sidecars list failed {}: {}", s, t));
        }
        let list: MessageList = res.json().await.map_err(|e| e.to_string())?;
        let messages = list.messages.unwrap_or_default();
        // Fetch each sidecar's Subject — metadata-only, no body. We could
        // batch via the gmail batch endpoint for very large meta_labels,
        // but a normal user has at most a few-hundred pinned notes so
        // sequential is fine and keeps the code path simple.
        for m in messages {
            match fetch_subject_only(&client, token, &m.id).await {
                Ok(Some(uuid)) => out.push(SidecarRef { id: m.id, note_uuid: uuid }),
                Ok(None) => {} // not a Jodd sidecar — skip silently
                Err(e) => log!("list_meta_sidecars: fetch_subject_only {}: {}", m.id, e),
            }
        }
        match list.next_page_token {
            Some(t) if !t.is_empty() => page_token = Some(t),
            _ => break,
        }
    }
    Ok(out)
}

/// List every tag sidecar under `meta_label_id`. Unlike `list_meta_sidecars`
/// (pin, metadata-only), this fetches each message with `format=full` so
/// we get the JSON tag list in the body. That makes it heavier than pin
/// sync, but the per-message body is tiny (just `{"tags":[…]}`) and only
/// notes that have ever had tags get a sidecar, so the volume is bounded
/// by "notes the user explicitly tagged" rather than "every pinned note".
///
/// Messages whose subject doesn't start with `TAG_SIDECAR_SUBJECT_PREFIX`
/// are skipped silently (defensive against pin sidecars or any other
/// jodd-managed message ending up here).
pub async fn list_tag_sidecars(
    token: &str,
    meta_label_id: &str,
) -> Result<Vec<TagSidecarRef>, String> {
    let client = reqwest::Client::new();
    let mut out: Vec<TagSidecarRef> = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut req = client
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
            .bearer_auth(token)
            .query(&[
                ("labelIds", meta_label_id),
                ("maxResults", "500"),
            ]);
        if let Some(pt) = page_token.as_deref() {
            req = req.query(&[("pageToken", pt)]);
        }
        let res = req.send().await.map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            let s = res.status();
            let t = res.text().await.unwrap_or_default();
            return Err(format!("list_tag_sidecars list failed {}: {}", s, t));
        }
        let list: MessageList = res.json().await.map_err(|e| e.to_string())?;
        let messages = list.messages.unwrap_or_default();
        for m in messages {
            match fetch_tag_sidecar_full(&client, token, &m.id).await {
                Ok(Some(sidecar)) => out.push(sidecar),
                Ok(None) => {} // not a tag sidecar — could be pin or noise
                Err(e) => log!("list_tag_sidecars: fetch_full {}: {}", m.id, e),
            }
        }
        match list.next_page_token {
            Some(t) if !t.is_empty() => page_token = Some(t),
            _ => break,
        }
    }
    Ok(out)
}

/// Fetch one message with full body; if it's a tag sidecar (prefix matches
/// + body parses as the expected JSON), return the parsed TagSidecarRef.
async fn fetch_tag_sidecar_full(
    client: &reqwest::Client,
    token: &str,
    msg_id: &str,
) -> Result<Option<TagSidecarRef>, String> {
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
        msg_id
    );
    let res = client
        .get(&url)
        .bearer_auth(token)
        .query(&[
            ("format", "full"),
            // Same masking trick as fetch_note — only request the fields
            // we'll actually read. We need headers (Subject) + body data
            // (the JSON tag list). No multipart for sidecars; we always
            // write them as flat text/plain. Still ask for 1 level of
            // parts as defense-in-depth in case Gmail wraps small bodies.
            (
                "fields",
                "id,payload(headers,body/data,parts(mimeType,body/data))",
            ),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status()));
    }
    let msg: GmailMessage = res.json().await.map_err(|e| e.to_string())?;
    let subject = get_header(&msg.payload.headers, "Subject");
    let uuid = match subject.strip_prefix(TAG_SIDECAR_SUBJECT_PREFIX) {
        Some(rest) => {
            let trimmed = rest.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed.to_string()
        }
        None => return Ok(None),
    };
    // Decode the body — try the top-level body first (we always write
    // sidecars flat), then walk one level of parts as a fallback.
    let body_text = msg
        .payload
        .body
        .as_ref()
        .and_then(|b| b.data.as_deref())
        .map(decode_body)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            msg.payload.parts.as_deref().and_then(|parts| {
                parts.iter().find_map(|p| {
                    p.body
                        .as_ref()
                        .and_then(|b| b.data.as_deref())
                        .map(decode_body)
                        .filter(|s| !s.is_empty())
                })
            })
        })
        .unwrap_or_default();
    // Parse the body as JSON `{"tags":["a","b",…]}`. If parsing fails,
    // we still return a sidecar with empty tags so the caller knows the
    // sidecar exists (and apply_remote_tags will clear local tags to
    // match). That's safer than dropping it silently — a malformed
    // sidecar from a future Jodd version shouldn't lose the user's tags
    // without an explicit signal.
    let tags = match serde_json::from_str::<TagsPayload>(&body_text) {
        Ok(p) => p.tags,
        Err(e) => {
            log!(
                "fetch_tag_sidecar_full: body JSON parse failed for {}: {} (treating as empty)",
                msg_id, e
            );
            Vec::new()
        }
    };
    Ok(Some(TagSidecarRef { id: msg.id, note_uuid: uuid, tags }))
}

/// Wire format for the tag sidecar body. Sorted, normalized strings.
#[derive(serde::Serialize, serde::Deserialize)]
struct TagsPayload {
    tags: Vec<String>,
}

/// Fetch the Subject header for a single message and, if it's a Jodd
/// sidecar (prefix matches), return the note_uuid it documents.
async fn fetch_subject_only(
    client: &reqwest::Client,
    token: &str,
    msg_id: &str,
) -> Result<Option<String>, String> {
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
        msg_id
    );
    let res = client
        .get(&url)
        .bearer_auth(token)
        .query(&[("format", "metadata"), ("metadataHeaders", "Subject")])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status()));
    }
    let msg: GmailMessage = res.json().await.map_err(|e| e.to_string())?;
    let subject = get_header(&msg.payload.headers, "Subject");
    if let Some(uuid) = subject.strip_prefix(SIDECAR_SUBJECT_PREFIX) {
        let uuid = uuid.trim();
        if !uuid.is_empty() {
            return Ok(Some(uuid.to_string()));
        }
    }
    Ok(None)
}

/// Insert a new sidecar message for `note_uuid` under `meta_label_id`,
/// then trash `old_sidecar_id` if supplied. Returns the new sidecar's
/// message id. `payload_json` is the state body (currently
/// `{"pinned": true}` for pin; structured so future state can extend
/// without a wire-format change).
pub async fn save_meta_sidecar(
    token: &str,
    note_uuid: &str,
    payload_json: &str,
    meta_label_id: &str,
    old_sidecar_id: Option<&str>,
    user_email: &str,
) -> Result<String, String> {
    save_sidecar_inner(
        token,
        SIDECAR_SUBJECT_PREFIX,
        note_uuid,
        payload_json,
        meta_label_id,
        old_sidecar_id,
        user_email,
    ).await
}

/// Tag sidecar variant — same envelope as `save_meta_sidecar`, different
/// subject prefix (`tags___` vs `___`). Body is `{"tags":["a","b",…]}` as
/// canonical JSON. The two prefixes are intentionally disjoint so each
/// sync's reader rejects the other's sidecars purely by prefix match.
pub async fn save_tag_sidecar(
    token: &str,
    note_uuid: &str,
    payload_json: &str,
    meta_label_id: &str,
    old_sidecar_id: Option<&str>,
    user_email: &str,
) -> Result<String, String> {
    save_sidecar_inner(
        token,
        TAG_SIDECAR_SUBJECT_PREFIX,
        note_uuid,
        payload_json,
        meta_label_id,
        old_sidecar_id,
        user_email,
    ).await
}

async fn save_sidecar_inner(
    token: &str,
    subject_prefix: &str,
    note_uuid: &str,
    payload_json: &str,
    meta_label_id: &str,
    old_sidecar_id: Option<&str>,
    user_email: &str,
) -> Result<String, String> {
    let now_local = chrono::Local::now();
    let date_header = format_apple_date(now_local);
    let domain = user_email.split('@').nth(1).unwrap_or("local.jodd");
    let message_id = format!("<{}@{}>", format_apple_uuid(uuid::Uuid::new_v4()), domain);
    let from = if user_email.is_empty() { "me".to_string() } else { user_email.to_string() };
    let subject = format!("{}{}", subject_prefix, note_uuid);

    // We intentionally do NOT set `X-Uniform-Type-Identifier: com.apple.mail-note`
    // — Apple Notes only acts on messages with that UTI, and we don't want
    // Apple touching our sidecars. We DO set our own UTI so future Jodd
    // code can recognize sidecars by header (in addition to subject prefix).
    let raw = format!(
        "From: {from}\r\n\
        X-Uniform-Type-Identifier: app.jodd.metadata\r\n\
        Content-Type: text/plain; charset=utf-8\r\n\
        Content-Transfer-Encoding: 7bit\r\n\
        Mime-Version: {mime}\r\n\
        Date: {date_header}\r\n\
        Subject: {subject}\r\n\
        Message-Id: {message_id}\r\n\
        \r\n\
        {payload_json}",
        mime = APPLE_MIME_VERSION
    );

    let encoded = URL_SAFE.encode(raw.as_bytes());
    let body = serde_json::json!({
        "raw": encoded,
        "labelIds": [meta_label_id]
    });
    let client = reqwest::Client::new();
    let res = client
        .post("https://gmail.googleapis.com/gmail/v1/users/me/messages")
        .bearer_auth(token)
        .query(&[("internalDateSource", "dateHeader")])
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let s = res.status();
        let t = res.text().await.unwrap_or_default();
        return Err(format!("save_meta_sidecar insert failed {}: {}", s, t));
    }
    let inserted: InsertResponse = res.json().await.map_err(|e| e.to_string())?;
    // Best-effort trash of the previous sidecar — same insert-then-trash
    // pattern as save_note. Failure is logged but doesn't fail the push
    // (worst case: a duplicate sidecar, harmless because pin sync is
    // existence-based and later list_meta_sidecars passes a single id
    // through to the apply step).
    if let Some(old) = old_sidecar_id.filter(|s| !s.is_empty()) {
        if old != inserted.id {
            if let Err(e) = delete_note(token, "meta-sidecar", old).await {
                log!("save_meta_sidecar: delete old {} failed: {}", old, e);
            }
        }
    }
    Ok(inserted.id)
}

/// Trash a sidecar. Same as delete_note (which trashes any Gmail message)
/// but with a more descriptive name at the worker callsite.
pub async fn trash_meta_sidecar(token: &str, sidecar_id: &str) -> Result<(), String> {
    delete_note(token, "meta-sidecar", sidecar_id).await
}

pub async fn create_label(token: &str, name: &str) -> Result<FolderInfo, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "name": name,
        "labelListVisibility": "labelShow",
        "messageListVisibility": "show",
    });
    let res = client
        .post("https://gmail.googleapis.com/gmail/v1/users/me/labels")
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();

    // 409 = label name already exists on Gmail. This happens when the local
    // folders table is fresh (e.g. after sign-out/sign-in) but Gmail still
    // carries the label from a prior session. Treat as success-by-discovery:
    // look up the existing label and return its id. Without this, the sync
    // worker retries create_label every 5s forever, burning API quota.
    if status == reqwest::StatusCode::CONFLICT {
        let map = get_label_map(token).await?;
        if let Some((existing_id, existing_name)) =
            map.into_iter().find(|(_, n)| n.as_str() == name)
        {
            log!(
                "create_label: '{}' already exists on Gmail (id={}) — adopting",
                existing_name,
                existing_id
            );
            return Ok(FolderInfo { id: existing_id, name: existing_name });
        }
        // 409 but the name doesn't appear in labels.list — fall through to
        // the generic error so we don't silently swallow a weirder conflict.
    }

    if !status.is_success() {
        return Err(format!("create_label HTTP {}: {}", status, text));
    }
    let parsed: GmailLabel = serde_json::from_str(&text)
        .map_err(|e| format!("create_label parse error: {} — body: {}", e, text))?;
    log!("create_label: created '{}' id={}", parsed.name, parsed.id);
    Ok(FolderInfo { id: parsed.id, name: parsed.name })
}

// PATCH /labels/{id} — rename a label. Apple Notes IMAP picks up the rename
// on its next sync and updates the folder name in the Notes UI.
pub async fn rename_label(token: &str, label_id: &str, new_name: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({ "name": new_name });
    let res = client
        .patch(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/labels/{}",
            label_id
        ))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(format!("rename_label HTTP {}: {}", status, text));
    }
    log!("rename_label: id={} → '{}'", label_id, new_name);
    Ok(())
}

// DELETE /labels/{id} — remove a label. Gmail's behavior: any messages that
// had ONLY this label are NOT deleted, they just lose the label. We block
// non-empty deletes at the Tauri-command layer per user preference, so by
// the time we get here the label is guaranteed empty (no Notes/sub-label
// messages reference it). Returns 204 No Content on success.
pub async fn delete_label(token: &str, label_id: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    let res = client
        .delete(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/labels/{}",
            label_id
        ))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    if !status.is_success() && status.as_u16() != 404 {
        let text = res.text().await.unwrap_or_default();
        return Err(format!("delete_label HTTP {}: {}", status, text));
    }
    log!("delete_label: id={} removed (status={})", label_id, status);
    Ok(())
}


// POST /messages/{id}/modify — atomically add and remove labels. Used to
// move a note between folders: remove the source label, add the dest label.
// Apple Notes' IMAP sees the label set change and reflects the move on next
// sync — there's no separate "move" verb in either Gmail or IMAP.
pub async fn modify_message_labels(
    token: &str,
    message_id: &str,
    add_label_ids: &[String],
    remove_label_ids: &[String],
) -> Result<(), String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "addLabelIds": add_label_ids,
        "removeLabelIds": remove_label_ids,
    });
    let res = client
        .post(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}/modify",
            message_id
        ))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(format!("modify_message_labels HTTP {}: {}", status, text));
    }
    log!(
        "modify_message_labels: id={} add={:?} remove={:?}",
        message_id, add_label_ids, remove_label_ids
    );
    Ok(())
}


#[cfg(test)]
mod attachment_tests {
    use super::*;

    fn h(name: &str, value: &str) -> Header {
        Header { name: name.to_string(), value: value.to_string() }
    }

    #[test]
    fn header_param_extracts_quoted_and_unquoted() {
        let hs = vec![
            h("Content-Type", "image/png; name=image.png; x-apple-part-url=\"CID-1@mobilenotes.apple.com\""),
            h("Content-Disposition", "inline; filename=image.png"),
        ];
        assert_eq!(header_param(&hs, "content-type", "x-apple-part-url").as_deref(), Some("CID-1@mobilenotes.apple.com"));
        assert_eq!(header_param(&hs, "content-type", "name").as_deref(), Some("image.png"));
        assert_eq!(header_param(&hs, "content-disposition", "filename").as_deref(), Some("image.png"));
        assert_eq!(header_param(&hs, "content-type", "charset"), None);
    }

    // Mirrors the real iPhone specimen's multipart/related tree: text/html body
    // (skipped) + an inline image/png referenced by Content-Id.
    #[test]
    fn collects_inline_image_skips_html() {
        let parts = vec![
            Part {
                mime_type: "text/html".to_string(),
                body: Some(Body { data: Some("PGh0bWw+".to_string()), attachment_id: None }),
                parts: None,
                filename: None,
                headers: Some(vec![h("Content-Type", "text/html; charset=utf-8")]),
            },
            Part {
                mime_type: "image/png".to_string(),
                body: Some(Body { data: None, attachment_id: Some("ANGjdJ_abc".to_string()) }),
                parts: None,
                filename: Some("image.png".to_string()),
                headers: Some(vec![
                    h("Content-Type", "image/png; name=image.png; x-apple-part-url=\"03D58874@mobilenotes.apple.com\""),
                    h("Content-Disposition", "inline; filename=image.png"),
                    h("Content-Id", "<03D58874@mobilenotes.apple.com>"),
                ]),
            },
        ];
        let mut out = Vec::new();
        collect_pending_attachments(Some(&parts), &mut out);
        assert_eq!(out.len(), 1, "html part must be skipped, image captured");
        let a = &out[0];
        assert_eq!(a.content_id, "03D58874@mobilenotes.apple.com"); // <> stripped
        assert_eq!(a.mime_type, "image/png");
        assert_eq!(a.filename.as_deref(), Some("image.png"));
        assert_eq!(a.x_apple_part_url.as_deref(), Some("03D58874@mobilenotes.apple.com"));
        assert_eq!(a.attachment_id.as_deref(), Some("ANGjdJ_abc"));
        assert!(a.inline_data.is_none());
    }


    // Apple attaches arbitrary file types (PDF, zip, .md, .eml). Detection is by
    // Content-Id, NOT mime — so a text/markdown attachment must be captured, and
    // only the text/html body (no Content-Id) excluded.
    #[test]
    fn collects_non_image_attachments() {
        let parts = vec![
            Part { // the body — no Content-Id, must be skipped
                mime_type: "text/html".to_string(),
                body: Some(Body { data: Some("PGI+".to_string()), attachment_id: None }),
                parts: None, filename: None,
                headers: Some(vec![h("Content-Type", "text/html; charset=utf-8")]),
            },
            Part { // PDF attachment
                mime_type: "application/pdf".to_string(),
                body: Some(Body { data: None, attachment_id: Some("pdfA".to_string()) }),
                parts: None, filename: Some("Resume.pdf".to_string()),
                headers: Some(vec![h("Content-Id", "<PDF-1@mobilenotes.apple.com>")]),
            },
            Part { // Markdown attachment — text/* but IS an attachment
                mime_type: "text/markdown".to_string(),
                body: Some(Body { data: Some("IyBSRUFETUU=".to_string()), attachment_id: None }),
                parts: None, filename: Some("README.md".to_string()),
                headers: Some(vec![h("Content-Id", "<MD-1@mobilenotes.apple.com>")]),
            },
        ];
        let mut out = Vec::new();
        collect_pending_attachments(Some(&parts), &mut out);
        let cids: Vec<&str> = out.iter().map(|a| a.content_id.as_str()).collect();
        assert_eq!(cids, vec!["PDF-1@mobilenotes.apple.com", "MD-1@mobilenotes.apple.com"]);
        assert_eq!(out[1].filename.as_deref(), Some("README.md"));
    }
}
