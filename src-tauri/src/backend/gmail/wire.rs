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

// List every message id currently in Gmail Trash, account-wide. Deliberately
// NOT scoped to any Notes label: scoping by label used to mean a trashed note
// became unfindable the moment its folder's label was deleted (deleting a
// Gmail label strips it from every message that carried it, including ones
// already in Trash). Scanning all of Trash and letting fetch_trashed_meta's
// X-UTI check filter down to Apple notes keeps orphaned notes visible.
// messages.list excludes trash by default, so we opt in explicitly.
async fn list_trash_ids(token: &str) -> Result<Vec<String>, String> {
    let client = reqwest::Client::new();
    let mut all: Vec<String> = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut query: Vec<(&str, String)> = vec![
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
            return Err(format!("trash list HTTP {}: {}", status, text));
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

/// Fetch a message's current label ids. Used by restore_note to check, as
/// ground truth, whether a just-untrashed message actually carries a
/// Notes-tree label — the `original_label` a caller passes may be a
/// synthetic "Notes" fallback (see pick_notes_label) rather than the
/// message's real prior label, so it can't be trusted on its own to detect
/// "this note's folder was deleted". Uses format=metadata with no headers
/// requested (format=minimal omits `payload` entirely, which GmailMessage
/// requires) — still a cheap call, just labelIds we actually need.
pub async fn get_message_label_ids(token: &str, id: &str) -> Result<Vec<String>, String> {
    let client = reqwest::Client::new();
    let res = client
        .get(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
            id
        ))
        .bearer_auth(token)
        .query(&[("format", "metadata"), ("metadataHeaders", "Subject")])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("get labels HTTP {} (id={})", res.status(), id));
    }
    let msg: GmailMessage = res.json().await.map_err(|e| e.to_string())?;
    Ok(msg.label_ids.unwrap_or_default())
}

/// A listing with one message per uuid.
pub struct Deduped {
    pub kept: Vec<Note>,
    /// Extra messages dropped because another carried the same uuid.
    pub collapsed: usize,
    pub uuids_affected: usize,
    /// The dropped messages that hold a line of text their kept copy lacks.
    /// Dropping those loses work — see `holds_lines_missing_from`.
    pub divergent: Vec<Note>,
}

/// Collapse messages that carry the same X-UUID to one per note.
///
/// Apple Notes ↔ Jodd races and the insert-then-trash pattern can leave
/// several messages with one uuid. Which one is kept is a design choice:
///   - "longest body wins" catches truncated saves but loses to legitimate
///     evolution of a note (older verbose → newer terse);
///   - "most recent Date wins" matches "show me the latest version" and
///     Apple Notes' own reconciliation.
/// Most-recent-Date is the rule, falling back to the longer body for a
/// same-date or unparseable pair, then to the id so the pick is stable
/// across polls (HashMap order is not, and `cache.id` would flip).
///
/// What a pick cannot do is merge, so a dropped message whose text the kept
/// one lacks is returned in `divergent` instead of disappearing — measured
/// 2026-10-21: two devices each wrote note 80D6C8E1 over the same base, and
/// the 18:05 revision's link vanished from Jodd behind a log line.
pub fn dedupe_by_uuid(notes: Vec<Note>) -> Deduped {
    let parse_date = |s: &str| chrono::DateTime::parse_from_rfc2822(s).ok();
    let mut by_uuid: HashMap<String, Note> = HashMap::new();
    let mut singleton: Vec<Note> = Vec::new();
    let mut dropped: Vec<Note> = Vec::new();
    let mut affected: std::collections::HashSet<String> = std::collections::HashSet::new();
    for note in notes {
        if note.uuid.is_empty() {
            singleton.push(note);
            continue;
        }
        let Some(existing) = by_uuid.get(&note.uuid) else {
            by_uuid.insert(note.uuid.clone(), note);
            continue;
        };
        affected.insert(note.uuid.clone());
        let new_wins = match (parse_date(&existing.date), parse_date(&note.date)) {
            (Some(e), Some(n)) if n != e => n > e,
            _ => {
                let (n_len, e_len) = (note.body_html.len(), existing.body_html.len());
                if n_len != e_len { n_len > e_len } else { note.id > existing.id }
            }
        };
        let loser = if new_wins { by_uuid.insert(note.uuid.clone(), note).expect("present") } else { note };
        let winner = &by_uuid[&loser.uuid];
        log!(
            "dedupe: uuid={} keeping id={} (date={} body={}b), dropping id={} (date={} body={}b)",
            loser.uuid, winner.id, winner.date, winner.body_html.len(),
            loser.id, loser.date, loser.body_html.len()
        );
        dropped.push(loser);
    }
    let collapsed = dropped.len();
    let divergent = dropped
        .into_iter()
        .filter(|d| holds_lines_missing_from(&d.body_html, &by_uuid[&d.uuid].body_html))
        .collect();
    let mut kept: Vec<Note> = by_uuid.into_values().collect();
    kept.extend(singleton);
    Deduped { kept, collapsed, uuids_affected: affected.len(), divergent }
}

/// Whether `dropped` has a line of text `kept` does not. An older revision
/// whose every line survives is history (Jodd's own failed trash-old leaves
/// those) and is safe to drop; one with a line of its own is somebody's work.
/// Lines, not bytes: Gmail and Apple re-encode the same text differently.
pub fn holds_lines_missing_from(dropped: &str, kept: &str) -> bool {
    let lines = |html: &str| -> std::collections::HashSet<String> {
        crate::agent_memory::text::html_to_text(html, true)
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|l| !l.is_empty())
            .collect()
    };
    let have = lines(kept);
    lines(dropped).iter().any(|l| !have.contains(l))
}

/// The version a Gmail message is, if it is still the live one. A Gmail note's
/// version is its message id (every save inserts a new message and trashes
/// the old), so a message is the current revision exactly when it exists and
/// is not in the trash. `labels` is `None` for a message Gmail no longer has.
pub fn live_version(id: &str, labels: Option<Vec<String>>) -> Option<String> {
    labels.filter(|l| !l.iter().any(|x| x == "TRASH")).map(|_| id.to_string())
}

/// `live_version` over the wire: one `format=minimal` read of `id`'s labels.
/// A 404 is an answer (gone), any other failure is an error — the caller
/// must not write on a check it could not make.
pub async fn current_version(token: &str, id: &str) -> Result<Option<String>, String> {
    let res = reqwest::Client::new()
        .get(format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{}", id))
        .bearer_auth(token)
        .query(&[("format", "minimal")])
        .send()
        .await
        .map_err(|e| format!("current_version {}: {}", id, e))?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("current_version {}: HTTP {} {}", id, status, text));
    }
    // Not `GmailMessage`: `format=minimal` carries no `payload`, which that
    // struct requires.
    #[derive(Deserialize)]
    struct Labels {
        #[serde(rename = "labelIds")]
        label_ids: Option<Vec<String>>,
    }
    let msg: Labels = res.json().await.map_err(|e| format!("current_version {}: decode: {}", id, e))?;
    Ok(live_version(id, Some(msg.label_ids.unwrap_or_default())))
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
    // A trashed Gmail message keeps its `Notes/*` label alongside TRASH, so the
    // folder above is the one the note really came from.
    Ok(Some(TrashedNote { id: msg.id, uuid, title, date, label, original_known: true }))
}

/// List every Apple note currently in Gmail Trash, account-wide — including
/// notes whose folder label has since been deleted (see list_trash_ids).
/// Caller filters out edit-revisions (uuids still live) — see lib.rs.
pub async fn list_trashed_notes(
    token: &str,
    label_map: &HashMap<String, String>,
) -> Result<Vec<TrashedNote>, String> {
    let ids = list_trash_ids(token).await?;

    // Parallelize the metadata fetches (one Gmail round trip per candidate).
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

    // ─── Dedupe by X-UUID — see `dedupe_by_uuid` ───────────────────────────
    let Deduped { kept, collapsed, uuids_affected, divergent } = dedupe_by_uuid(notes);
    let mut notes = kept;
    if collapsed > 0 {
        log!(
            "dedupe: collapsed {} duplicate(s) across {} UUID(s) in list — background cleanup disabled",
            collapsed, uuids_affected
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
    let summary = DedupSummary { collapsed, uuids_affected, divergent };
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
) -> Result<(Vec<Note>, Vec<Note>), String> {
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

    // Dedup by UUID — the same rule as list_notes. Within a single folder,
    // duplicates can still exist from race conditions or pre-411-fix orphans.
    let Deduped { kept: mut out, divergent, .. } = dedupe_by_uuid(notes);
    out.sort_by(|a, b| {
        let parse = |s: &str| chrono::DateTime::parse_from_rfc2822(s).ok();
        parse(&b.date).cmp(&parse(&a.date))
    });
    Ok((out, divergent))
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
        id: msg.id.clone(),
        uuid,
        title,
        body_html,
        date,
        version: msg.id,   // Gmail re-mints the id on every edit
        label,
        local_version: 0,
        x_mail_created_date,
        account_id: None, // stamped by the Tauri command layer after fetch
        pinned: false,    // local-only state; merged in by cache lookups, not parsed from wire
        attachments,
        push_blocked_reason: None,
        push_blocked_by_remote: false,
    })
}


/// Resolve the uuid to write on this save: preserve `existing_uuid`
/// (canonicalized to Apple's hyphenated form) when it parses, or mint a
/// fresh one when there is none or it doesn't parse as a uuid at all — e.g.
/// corrupted or pre-migration data. Extracted out of `save_note` so this
/// decision is pinned by a unit test: it's what makes the returned
/// `SavedNote.uuid` differ from the caller's cached uuid on Gmail, not only
/// on Microsoft's create path, which is what makes `push_one_dirty`'s
/// uuid-convergence rekey (lib.rs) fire here too. See
/// `resolve_uuid_for_save_tests` below.
fn resolve_uuid_for_save(existing_uuid: Option<&str>) -> String {
    existing_uuid
        .filter(|s| !s.is_empty())
        .and_then(canonicalize_uuid)
        .unwrap_or_else(|| format_apple_uuid(uuid::Uuid::new_v4()))
}

#[cfg(test)]
mod resolve_uuid_for_save_tests {
    use super::*;

    #[test]
    fn a_valid_cached_uuid_is_preserved_verbatim() {
        let existing = format_apple_uuid(uuid::Uuid::new_v4());
        assert_eq!(resolve_uuid_for_save(Some(&existing)), existing);
    }

    /// The scenario a fix-round-1 review on Task 6 flagged: when this mints
    /// fresh, `save_note`'s returned `SavedNote.uuid` differs from the
    /// caller's cached uuid — which makes `push_one_dirty`'s rekey (lib.rs)
    /// fire on Gmail too, not only on Microsoft's create path. That rekey
    /// mechanism is intentional and backend-agnostic (see its comment in
    /// lib.rs); this test just pins the Gmail-side trigger for it.
    #[test]
    fn an_unparseable_cached_uuid_gets_a_fresh_one_not_preserved_verbatim() {
        let fresh = resolve_uuid_for_save(Some("not-a-real-uuid"));
        assert_ne!(fresh, "not-a-real-uuid");
        assert!(uuid::Uuid::parse_str(&fresh).is_ok(), "must be a real uuid: {fresh}");
    }

    #[test]
    fn a_missing_or_empty_cached_uuid_also_mints_fresh() {
        assert!(uuid::Uuid::parse_str(&resolve_uuid_for_save(None)).is_ok());
        assert!(uuid::Uuid::parse_str(&resolve_uuid_for_save(Some(""))).is_ok());
    }
}

/// A note ready to insert: the RFC822 bytes plus the values `save_note`
/// reports back in `SavedNote`. Produced by `compose_note`, consumed by
/// `save_note` — the split is what lets a test see the exact bytes a push
/// would send without a network.
pub struct ComposedNote {
    pub uuid: String,
    pub date_header: String,
    /// The `X-Mail-Created-Date` in `raw` — see `SavedNote::created_date`.
    pub created_date: String,
    pub raw: String,
}

impl ComposedNote {
    /// What `save_note` reports once Gmail has inserted these bytes as
    /// message `id`. `body_html` is the editor-view input, not `raw`'s body —
    /// see `SavedNote::body_html`.
    pub fn into_saved(self, id: String, body_html: &str) -> SavedNote {
        SavedNote {
            version: id.clone(), // Gmail: id IS the version — see SavedNote::version
            id,
            uuid: self.uuid,
            date: self.date_header,
            created_date: Some(self.created_date),
            body_html: body_html.to_string(),
            local_version: 0,
        }
    }
}

/// Everything `save_note` sends, minus the HTTP.
///
/// `user_email` must be the **bare** address (`Account::email`), never the
/// `{backend}:{email}` account id — it becomes the `From:` header verbatim.
/// Passing the id wrote `From: gmail:user@…` onto every note Jodd pushed until
/// 2026-09-30; the Message-Id domain survived only because it splits on `@`.
pub fn compose_note(
    title: &str,
    body_html: &str,
    existing_uuid: Option<&str>,
    existing_x_mail_created_date: Option<&str>,
    user_email: &str,
    // Attachments the body may reference via <object data="cid:…">. The save
    // path re-emits matching ones as multipart/related parts (with the original
    // Content-Id) instead of stripping them — the image data-loss fix.
    attachments: &[Attachment],
) -> ComposedNote {
    // Preserve the X-UUID across saves so the note's identity is stable.
    // Canonicalize old hyphen-stripped UUIDs to Apple's standard hyphenated
    // form; mint fresh if there's nothing usable — see resolve_uuid_for_save.
    let uuid = resolve_uuid_for_save(existing_uuid);

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
    ComposedNote { uuid, date_header, created_date, raw }
}

pub async fn save_note(
    token: &str,
    note: ComposedNote,
    // Editor-view body, echoed back in `SavedNote` — the wire bytes are
    // already in `note.raw`.
    body_html: &str,
    existing_gmail_id: Option<&str>,
    label: &str,
    // Log prefix only; the From header was settled in `compose_note`.
    account: &str,
    label_map: &HashMap<String, String>,
) -> Result<SavedNote, String> {
    let encoded = URL_SAFE.encode(note.raw.as_bytes());
    let client = reqwest::Client::new();

    // Resolve target label name → label ID. If the user is creating a note in
    // "Notes/myNotes" we need Gmail's Label_NNN, not the human-readable name.
    // Map is supplied by the caller (cached in AppState) — no round-trip
    // unless the label is missing from it, in which case it is created (or,
    // on a 409, adopted) rather than the note being filed in the root. The
    // caller's cached map is then stale; `push_one_dirty` invalidates it.
    let target_label_id = match resolve_target(label, label_map) {
        LabelTarget::Existing(id) => id,
        LabelTarget::Create => {
            log!(
                "save_note: label '{}' is not in the cached label map — creating it before the insert",
                label
            );
            create_label(token, label).await?.id
        }
    };

    log!(
        "save_note: label='{}' → {}, existing_gmail_id={:?}, uuid={}",
        label, target_label_id, existing_gmail_id, note.uuid
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
    log!("save_note[{}]: inserted new id={}", account, inserted.id);

    // Replace, don't duplicate: best-effort delete the previous message after
    // the new one is safely in. We delete LAST so a network blip on delete
    // leaves a duplicate (recoverable) instead of data loss (not recoverable).
    if let Some(old_id) = existing_gmail_id.filter(|s| !s.is_empty()) {
        if old_id != inserted.id {
            // delete_note logs the "TRASHED" line (with account); this adds the
            // "which new id replaced it" context so a save's trash is traceable.
            match delete_note(token, account, old_id).await {
                Ok(_) => log!(
                    "save_note[{}]: old revision id={} replaced by id={}",
                    account, old_id, inserted.id
                ),
                Err(e) => log!(
                    "save_note[{}]: failed to trash old {}: {} (new note saved OK)",
                    account, old_id, e
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

    Ok(note.into_saved(inserted.id, body_html))
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

// Read one message's raw X-Universally-Unique-Identifier header.
//
// Unlike `find_gmail_ids_for_uuid` / `find_all_duplicate_ids`, which
// `continue` past a failed fetch because they only REPORT, this returns Err:
// its caller (sync_worker's deletion sweep) decides whether to trash, and a
// fetch it could not complete must stop the deletion from being called done
// rather than be read as "not a copy". 404 is the one failure that is an
// answer — the message no longer exists, so there is nothing to trash — and
// it comes back as an empty header.
//
// Returns the header as Gmail stored it; canonicalising is the caller's job,
// so that comparison lives in one tested place.
pub async fn fetch_uuid_header(token: &str, id: &str) -> Result<String, String> {
    fetch_metadata_header(token, id, "X-Universally-Unique-Identifier").await
}

// One header of one message, with `fetch_uuid_header`'s error rules: a fetch
// that fails is Err, and a message that is gone (404) reads as empty.
pub async fn fetch_metadata_header(token: &str, id: &str, header: &str) -> Result<String, String> {
    let res = reqwest::Client::new()
        .get(format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{}", id))
        .bearer_auth(token)
        .query(&[("format", "metadata"), ("metadataHeaders", header)])
        .send()
        .await
        .map_err(|e| format!("fetch_metadata_header {} {}: {}", id, header, e))?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(String::new());
    }
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("fetch_metadata_header {} {}: HTTP {} {}", id, header, status, text));
    }
    let msg: GmailMessage = res
        .json()
        .await
        .map_err(|e| format!("fetch_metadata_header {} {}: decode: {}", id, header, e))?;
    Ok(get_header(&msg.payload.headers, header))
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

/// One sidecar as returned by list_meta_sidecars — minimal projection
/// (no body fetch). For pin, existence is the signal.
#[derive(Debug, Clone)]
pub struct SidecarRef {
    /// Gmail message id of the sidecar.
    pub id: String,
    /// The note uuid this sidecar documents (parsed from Subject).
    pub note_uuid: String,
}

/// Where a push bound for a label goes: the label of exactly that name, or a
/// create. There is no third answer — see `resolve_target`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LabelTarget {
    Existing(String),
    Create,
}

/// `Existing` with the id of the label named exactly `label`, or `Create`
/// when this map has no such label. Never another folder's id: a missing
/// label is either a folder whose create has not reached Gmail yet or one
/// removed remotely, and in both cases filing the note anywhere else is a
/// silent move the user did not ask for. `save_note` and `ensure_label`
/// take their label id from this match and nowhere else.
pub(crate) fn resolve_target(label: &str, label_map: &HashMap<String, String>) -> LabelTarget {
    match label_map.iter().find(|(_, name)| name.as_str() == label) {
        Some((id, _)) => LabelTarget::Existing(id.clone()),
        None => LabelTarget::Create,
    }
}

/// Every refusal `adopt_on_conflict` returns contains this, and
/// `classify_str` checks for it before its ` 409` arm — the refusal names the
/// status, and must not be read as a `Conflict`.
pub(crate) const LABEL_CREATE_REFUSED: &str = "Gmail refused to create label";

/// What a 409 from `POST /labels` means, given the fresh `labels.list` map:
/// the label of exactly `name` already exists (a fresh local folders table
/// against a label Gmail kept from a prior session) and is adopted, or
/// Gmail refused a name it does not list.
///
/// The second case is a permanent refusal, not a conflict: the identical
/// create can never succeed, and Gmail's push never refetches on `Conflict`
/// (`refetch_on_conflict`), so as a `Conflict` the note retried every tick
/// and held a Draining account forever (gotchas #2, #14). Gmail's 409 body
/// was not captured, and whether a name differing only in case is a
/// conflict is UNMEASURED — so such a label is named in the refusal, for the
/// user to act on, but never adopted: it is another folder to Jodd, and
/// filing into it is the silent move `resolve_target` exists to prevent.
///
/// ASSUMED, also unmeasured: a `labels.list` made right after the 409 already
/// shows a label another client created a moment earlier. If it lagged, a race
/// that used to clear itself on the next tick would now push-block until the
/// user acts.
pub(crate) fn adopt_on_conflict(
    name: &str,
    map: HashMap<String, String>,
    body: &str,
) -> Result<FolderInfo, String> {
    if let Some((id, existing)) = map.iter().find(|(_, n)| n.as_str() == name) {
        return Ok(FolderInfo { id: id.clone(), name: existing.clone() });
    }
    let lower = name.to_lowercase();
    match map.values().find(|n| n.to_lowercase() == lower) {
        Some(other) => Err(format!(
            "{LABEL_CREATE_REFUSED} '{name}' (HTTP 409) and has no label of exactly that name. \
             Its label '{other}' differs only in case — move the note there, or rename \
             one of the two folders."
        )),
        None => Err(format!(
            "{LABEL_CREATE_REFUSED} '{name}' (HTTP 409) and has no label of exactly that \
             name to adopt: {body}"
        )),
    }
}

/// `LABEL_CREATE_REFUSED`'s counterpart for `PATCH /labels/{id}`; checked by
/// `classify_str` before its ` 409` arm for the same reason.
pub(crate) const LABEL_RENAME_REFUSED: &str = "Gmail refused to rename label";

/// What a 409 from `PATCH /labels/{id}` means, given the fresh `labels.list`
/// map: the label already carries `new_name` (the same rename, made on another
/// device first) and there is nothing left to do, or the rename is refused.
///
/// The refusal is permanent, not a conflict. As a `Conflict` it was returned
/// from `push_one_folder` every tick, and the `dirty_renamed` row held a
/// Draining account forever (gotchas #2, #14). Nothing the worker does makes
/// the identical PATCH succeed: when another label holds the name, only the
/// user can resolve which folder is which. Same caveats as
/// `adopt_on_conflict` — the 409 body was never captured, case-only clashes
/// are UNMEASURED, and `labels.list` is ASSUMED to show a label made a moment
/// earlier.
pub(crate) fn adopt_rename_on_conflict(
    label_id: &str,
    new_name: &str,
    map: HashMap<String, String>,
    body: &str,
) -> Result<(), String> {
    if map.get(label_id).map(String::as_str) == Some(new_name) {
        return Ok(());
    }
    let others = || map.iter().filter(|(id, _)| id.as_str() != label_id).map(|(_, n)| n);
    let lower = new_name.to_lowercase();
    if others().any(|n| n == new_name) {
        Err(format!(
            "{LABEL_RENAME_REFUSED} to '{new_name}' (HTTP 409): another label already has \
             that name."
        ))
    } else if let Some(other) = others().find(|n| n.to_lowercase() == lower) {
        Err(format!(
            "{LABEL_RENAME_REFUSED} to '{new_name}' (HTTP 409). Its label '{other}' differs \
             only in case — pick another name."
        ))
    } else {
        Err(format!(
            "{LABEL_RENAME_REFUSED} to '{new_name}' (HTTP 409) and has no other label of that \
             name: {body}"
        ))
    }
}

#[cfg(test)]
mod resolve_target_tests {
    use super::*;
    use crate::backend::gmail::transport::classify_str;
    use crate::backend::TransportError;

    fn map() -> HashMap<String, String> {
        HashMap::from([
            ("Label_1".to_string(), "Notes".to_string()),
            ("Label_7".to_string(), "Notes/Work".to_string()),
        ])
    }

    #[test]
    fn a_known_label_resolves_to_its_own_id() {
        assert_eq!(resolve_target("Notes/Work", &map()), LabelTarget::Existing("Label_7".into()));
        assert_eq!(resolve_target("Notes", &map()), LabelTarget::Existing("Label_1".into()));
    }

    /// Measured live 2026-10-08: a note moved into `Notes/__Claude__`, whose
    /// Gmail label had been removed, was pushed as `→ Label_1` — the root —
    /// and the next pull relabeled the cache to `Notes` to match. A label the
    /// map does not know is not the root; the push must create it instead.
    /// `save_note` takes its label id from this match and nowhere else, so a
    /// root fallback re-added here fails this test.
    #[test]
    fn a_label_missing_from_the_map_never_resolves_to_the_root() {
        assert_eq!(resolve_target("Notes/__Claude__", &map()), LabelTarget::Create);
    }

    /// A different-case name is a different folder to Jodd: adopting it would
    /// file the note somewhere the user did not pick, and the next pull would
    /// rename the folder in the cache to Gmail's spelling.
    #[test]
    fn a_label_differing_only_in_case_is_not_the_same_target() {
        assert_eq!(resolve_target("notes/work", &map()), LabelTarget::Create);
    }

    #[test]
    fn a_409_adopts_the_label_of_exactly_that_name() {
        let info = adopt_on_conflict("Notes/Work", map(), "{}").expect("adopt");
        assert_eq!((info.id.as_str(), info.name.as_str()), ("Label_7", "Notes/Work"));
    }

    /// The 409 whose name `labels.list` does not show was classified
    /// `Conflict` (the error carried " 409"), which Gmail's push never
    /// refetches on, so the note retried every tick and held a Draining
    /// account forever (gotchas #2, #14). Retrying the identical create
    /// cannot succeed; it must reach the worker as `Permanent`.
    #[test]
    fn a_409_with_no_exact_match_is_a_permanent_refusal() {
        let err = adopt_on_conflict(
            "Notes/__Claude__",
            map(),
            r#"{"error":{"code":409,"message":"Label name exists or conflicts"}}"#,
        )
        .expect_err("no label of that name to adopt");
        assert!(
            matches!(classify_str(&err), TransportError::Permanent { .. }),
            "got {:?} for {err:?}",
            classify_str(&err),
        );
    }

    /// The refusal is what the push-block banner shows, so when a label
    /// differing only in case is the likely culprit it names that label.
    #[test]
    fn a_409_refusal_names_a_label_differing_only_in_case() {
        let err = adopt_on_conflict("notes/work", map(), "{}").expect_err("no exact match");
        assert!(err.contains("'Notes/Work'"), "{err}");
        assert!(matches!(classify_str(&err), TransportError::Permanent { .. }));
    }

    /// The rename already happened (the same rename made on another device):
    /// the label being renamed carries the target name, so there is nothing
    /// left to do.
    #[test]
    fn a_rename_409_adopts_when_the_label_already_has_the_target_name() {
        assert_eq!(adopt_rename_on_conflict("Label_7", "Notes/Work", map(), "{}"), Ok(()));
    }

    /// A rename 409 read as " 409" → `Conflict`, which `push_one_folder`
    /// returned as an error every tick while the row held a Draining account.
    /// When another label holds the name, the identical PATCH cannot succeed.
    #[test]
    fn a_rename_409_onto_another_labels_name_is_a_permanent_refusal() {
        let err = adopt_rename_on_conflict("Label_1", "Notes/Work", map(), "{}")
            .expect_err("Label_7 holds that name");
        assert!(err.contains("'Notes/Work'"), "{err}");
        assert!(
            matches!(classify_str(&err), TransportError::Permanent { .. }),
            "got {:?} for {err:?}",
            classify_str(&err),
        );
    }

    #[test]
    fn a_rename_409_refusal_names_a_label_differing_only_in_case() {
        let err = adopt_rename_on_conflict("Label_1", "notes/work", map(), "{}").expect_err("no exact match");
        assert!(err.contains("'Notes/Work'"), "{err}");
        assert!(matches!(classify_str(&err), TransportError::Permanent { .. }));
    }
}

/// Resolve `label_path` to a Gmail label id, creating the label if it
/// doesn't exist yet. Used by the sync worker on first sidecar push for
/// an account — by then the user has had a chance to configure the
/// meta_label in Settings (if they want a non-default name) and we
/// materialize it lazily so unused accounts don't end up with an
/// empty "Notes-Meta" label cluttering Gmail. `save_note` uses it too, so a
/// note pushed to a missing folder recreates that folder's label.
pub async fn ensure_label(
    token: &str,
    label_path: &str,
    label_map: &HashMap<String, String>,
) -> Result<String, String> {
    match resolve_target(label_path, label_map) {
        LabelTarget::Existing(id) => Ok(id),
        LabelTarget::Create => Ok(create_label(token, label_path).await?.id),
    }
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
    // worker retries create_label every 5s forever, burning API quota. A 409
    // for a name `labels.list` does not show is a permanent refusal — see
    // `adopt_on_conflict`.
    if status == reqwest::StatusCode::CONFLICT {
        let info = adopt_on_conflict(name, get_label_map(token).await?, &text)?;
        log!("create_label: '{}' already exists on Gmail (id={}) — adopting", info.name, info.id);
        return Ok(info);
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
    if status == reqwest::StatusCode::CONFLICT {
        let text = res.text().await.unwrap_or_default();
        adopt_rename_on_conflict(label_id, new_name, get_label_map(token).await?, &text)?;
        log!("rename_label: id={} already named '{}' — adopting", label_id, new_name);
        return Ok(());
    }
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


/// `dedupe_by_uuid`: which copy a listing keeps is unchanged; what is new is
/// that a dropped copy holding text the kept one lacks is reported, not lost.
/// Fixtures are the 2026-10-21 pair on note 80D6C8E1: the 18:05 revision
/// (one link appended) and the 00:40 one written over an older base.
#[cfg(test)]
mod dedupe_tests {
    use super::*;

    const UUID: &str = "80D6C8E1-C49F-42F6-9859-540845D6359C";
    const LIST: &str = "<div>https://www.youtube.com/watch?v=jXZvDtXg5co</div><div>https://www.facebook.com/watch/?v=965106449453157</div>";

    fn msg(id: &str, date: &str, body: &str) -> Note {
        serde_json::from_value(serde_json::json!({
            "id": id, "uuid": UUID, "title": "จีนสั้น character references", "body_html": body,
            "date": date, "version": id, "label": "Notes/Personal/Creator/Charactor ref",
        }))
        .unwrap()
    }

    fn the_1805_revision() -> Note {
        msg("1a158fccdb4a2561", "Tue, 20 Oct 2026 18:05:25 +0700",
            &format!("{LIST}<div><br></div><div>— 2026-10-20 18:05 —</div><div>https://youtube.com/watch?v=a0uhMvKEgYI</div>"))
    }

    fn the_0040_revision() -> Note {
        msg("1a15a6640194ada5", "Wed, 21 Oct 2026 00:40:13 +0700",
            &format!("{LIST}<div><br></div><div>— 2026-10-21 00:40 —</div><div>https://youtube.com/watch?v=b3pgye7HewM</div>"))
    }

    #[test]
    fn a_dropped_copy_with_a_line_the_kept_one_lacks_is_reported() {
        let d = dedupe_by_uuid(vec![the_1805_revision(), the_0040_revision()]);

        assert_eq!(d.kept.len(), 1);
        assert_eq!(d.kept[0].id, "1a15a6640194ada5", "the newer Date still wins");
        assert_eq!((d.collapsed, d.uuids_affected), (1, 1));
        assert_eq!(d.divergent.len(), 1);
        assert_eq!(d.divergent[0].id, "1a158fccdb4a2561");
    }

    /// An older revision whose every line survives in the kept one is history,
    /// not lost work — Jodd's own failed trash-old leaves these. Dropped as
    /// before, or every listing would mint copies of old states.
    #[test]
    fn a_dropped_copy_whose_lines_all_survive_is_dropped_quietly() {
        let older = msg("old", "Mon, 19 Oct 2026 20:51:13 +0700", LIST);
        let d = dedupe_by_uuid(vec![older, the_0040_revision()]);

        assert_eq!(d.kept.len(), 1);
        assert_eq!(d.collapsed, 1);
        assert!(d.divergent.is_empty());
    }

    #[test]
    fn an_identical_copy_is_dropped_quietly_whatever_the_order() {
        let a = the_0040_revision();
        let mut b = the_0040_revision();
        b.id = "1a15a6640194ada6".into();
        for pair in [vec![a.clone(), b.clone()], vec![b, a]] {
            let d = dedupe_by_uuid(pair);
            assert_eq!(d.kept.len(), 1);
            assert!(d.divergent.is_empty());
        }
    }

    #[test]
    fn notes_without_a_uuid_are_all_kept() {
        let mut a = the_0040_revision();
        a.uuid.clear();
        let mut b = the_1805_revision();
        b.uuid.clear();
        let d = dedupe_by_uuid(vec![a, b]);
        assert_eq!(d.kept.len(), 2);
        assert_eq!(d.collapsed, 0);
    }
}

/// Whether `live_version` reads Gmail's answer right (see its doc comment).
#[cfg(test)]
mod live_version_tests {
    use super::live_version;

    fn labels(l: &[&str]) -> Option<Vec<String>> {
        Some(l.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_message_in_its_label_is_the_live_version() {
        assert_eq!(live_version("1a15bdf6b1d35164", labels(&["Label_131", "SENT"])).as_deref(), Some("1a15bdf6b1d35164"));
    }

    /// What every Jodd save and Apple's IMAP delete leave behind: the old
    /// revision, trashed — measured on 1a15a6640194ada5, 2026-10-21.
    #[test]
    fn a_trashed_message_is_not() {
        assert_eq!(live_version("1a15a6640194ada5", labels(&["Label_131", "TRASH", "SENT"])), None);
    }

    #[test]
    fn a_message_that_is_gone_is_not() {
        assert_eq!(live_version("gone", None), None);
    }
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
