use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::log;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Note {
    pub id: String,
    pub uuid: String,
    pub title: String,
    pub body_html: String,
    pub date: String,
    pub label: String,
    // Apple tracks original creation time separately from Date (last modified).
    // Preserve across edits so we don't reset the creation time on every save.
    #[serde(default)]
    pub x_mail_created_date: Option<String>,
    // Multi-account: which Gmail account this note belongs to.
    // Stamped by the Tauri command layer after fetch (gmail.rs is account-blind).
    #[serde(default)]
    pub account_id: Option<String>,
    // Jodd-local pin state. Never travels over the wire (Apple Notes stores
    // pin in iCloud metadata, which the email backend doesn't carry) — it's
    // populated from the SQLite cache by `CachedNote::to_frontend_note`.
    // For freshly-parsed wire-format notes (`parse_message`), default to false.
    #[serde(default)]
    pub pinned: bool,
}

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

/// Lightweight stub returned by `list_account_index` — just enough to drive
/// folder counts and a "loading X of Y" indicator without paying for a full
/// `messages.get` per row. Hydrated to a real `Note` later via the normal
/// list path (cache-aware) when the user focuses a folder.
#[derive(Serialize, Clone, Debug)]
pub struct MessageIndex {
    pub id: String,
    pub label: String,
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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SavedNote {
    pub id: String,   // new Gmail message ID
    pub uuid: String, // X-Universally-Unique-Identifier (preserved or freshly generated)
    // Date header we put in the raw email (RFC 2822). The local cache must
    // mirror this — otherwise the next pull's dedupe-by-Date compares the
    // fresh remote against a stale cached date and gets the order wrong.
    pub date: String,
    // Body in EDITOR-VIEW form — what the user sees in the contenteditable.
    // This is the input we received (pre-inject_title), NOT the wire-format
    // bytes we sent to Gmail. Reason for the asymmetry: the pull path stores
    // post-strip_leading_title bodies. If push stored post-inject bodies the
    // cache would flip between "with title row" and "without title row"
    // depending on which side most recently touched it. Keeping the cache as
    // "editor-view" mirrors what fetch_note hands back, so list/dedupe/render
    // see one consistent shape regardless of origin.
    pub body_html: String,
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

// Recover a Subject that was saved by pre-fix Jodd: raw UTF-8 bytes written
// to a header that's spec'd as 7-bit ASCII. Gmail returns those bytes mis-decoded
// as Latin-1 / Windows-1252, producing strings like "à¸—à¸"à¸ªà¸­à¸š" for "ทดสอบ".
//
// Heuristic: cast each char back to its Latin-1 byte value (with a small map for
// the Windows-1252 supplements 0x80–0x9F), then try to decode the byte sequence
// as UTF-8. If the result is valid UTF-8 with non-Latin-1 codepoints (i.e. real
// multi-byte UTF-8 chars), it's almost certainly a real recovery. Legitimate
// Latin-1/CP1252 input would fail UTF-8 validation due to lone high bytes.
fn try_recover_mis_decoded_utf8(s: &str) -> Option<String> {
    // Only attempt if string contains chars that suggest mis-decoded UTF-8
    // (a non-ASCII char that's < 0x100 — i.e. a Latin-1 high byte).
    if !s.chars().any(|c| (c as u32) >= 0x80 && (c as u32) < 0x100) {
        return None;
    }
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        let byte = if cp < 0x100 {
            cp as u8
        } else {
            // Windows-1252 supplement mapping (0x80–0x9F range)
            match cp {
                0x20AC => 0x80, 0x201A => 0x82, 0x0192 => 0x83, 0x201E => 0x84,
                0x2026 => 0x85, 0x2020 => 0x86, 0x2021 => 0x87, 0x02C6 => 0x88,
                0x2030 => 0x89, 0x0160 => 0x8A, 0x2039 => 0x8B, 0x0152 => 0x8C,
                0x017D => 0x8E, 0x2018 => 0x91, 0x2019 => 0x92, 0x201C => 0x93,
                0x201D => 0x94, 0x2022 => 0x95, 0x2013 => 0x96, 0x2014 => 0x97,
                0x02DC => 0x98, 0x2122 => 0x99, 0x0161 => 0x9A, 0x203A => 0x9B,
                0x0153 => 0x9C, 0x017E => 0x9E, 0x0178 => 0x9F,
                _ => return None, // out-of-band char — abort recovery
            }
        };
        bytes.push(byte);
    }
    let recovered = String::from_utf8(bytes).ok()?;
    // Only accept the recovery if it contains chars outside Latin-1 range,
    // i.e. real multi-byte UTF-8 content (Thai, CJK, etc.). Otherwise the
    // original was likely just legitimate Latin-1 and we'd corrupt it.
    if recovered.chars().any(|c| (c as u32) >= 0x100) {
        Some(recovered)
    } else {
        None
    }
}

// Apple's exact Mime-Version masquerade — recognized by Apple Notes as a
// native-client message. Without this, Apple may treat our edits as foreign.
const APPLE_MIME_VERSION: &str = "1.0 (Mac OS X Notes 4.13 \\(3146.121.7\\))";

// Returns true if the entire string is pure ASCII (no bytes ≥ 0x80).
// Controls content-adaptive encoding choice (us-ascii+7bit vs utf-8+QP).
fn is_ascii(s: &str) -> bool {
    s.bytes().all(|b| b < 0x80)
}

// Format a uuid::Uuid as Apple's "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX" uppercase
// hyphenated form. Apple's reconciliation does exact-string match on X-UUID.
pub fn format_apple_uuid(u: uuid::Uuid) -> String {
    u.hyphenated().to_string().to_uppercase()
}

// Normalize whatever UUID we have (Apple-style with hyphens, or our old
// hyphen-stripped form from before this fix) to Apple's canonical format.
pub fn canonicalize_uuid(s: &str) -> Option<String> {
    // Try parsing both forms — uuid::Uuid::parse_str accepts both
    uuid::Uuid::parse_str(s).ok().map(format_apple_uuid)
}

// Apple's Date header format: `Thu, 4 Jun 2026 01:19:50 +0700`
// No leading zero on day; local timezone offset.
fn format_apple_date(dt: chrono::DateTime<chrono::Local>) -> String {
    dt.format("%a, %-d %b %Y %H:%M:%S %z").to_string()
}

// RFC 2047 encoded-word for a Subject header. Picks B (base64) vs Q
// (quoted-printable-like) by whichever produces shorter output, matching
// Apple's strategy. For pure-ASCII inputs the caller should skip encoding
// entirely (we still handle that case safely by returning the original).
fn rfc2047_encode_subject(text: &str) -> String {
    if is_ascii(text) {
        return text.to_string();
    }
    // B form: base64 of UTF-8 bytes, fixed ~33% overhead
    let b = format!(
        "=?utf-8?B?{}?=",
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
    );
    // Q form: like quoted-printable but with space → underscore and a
    // restricted set of literal-safe characters per RFC 2047 §4.2.
    let q_inner: String = text
        .bytes()
        .map(|b| match b {
            b' ' => "_".to_string(),
            // Letters, digits, and a small safe punctuation set pass through.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'!' | b'*' | b'+' | b'-' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("={:02X}", b),
        })
        .collect();
    let q = format!("=?utf-8?Q?{}?=", q_inner);
    if q.len() <= b.len() { q } else { b }
}

// Quoted-printable body encoding. Apple uses this for non-ASCII bodies and
// declares Content-Transfer-Encoding: quoted-printable.
fn qp_encode_body(s: &str) -> String {
    String::from_utf8_lossy(&quoted_printable::encode(s.as_bytes())).into_owned()
}

// Inject the title as the first <div> inside <body>. Idempotent: if the body
// already starts with the title, we don't double-inject. Matches Apple's
// convention that the body's first element is the displayed title.
fn inject_title_into_body(body_html: &str, title: &str) -> String {
    if title.is_empty() {
        return body_html.to_string();
    }
    // Already has the title at the front? Don't re-inject.
    if let Some(start) = body_html.find("<body") {
        let after_open = match body_html[start..].find('>') {
            Some(o) => start + o + 1,
            None => return body_html.to_string(),
        };
        let inner = &body_html[after_open..];
        let trimmed = inner.trim_start();

        // Already starts with <div>{title}</div>?
        if let Some(div_open) = trimmed.find("<div") {
            if div_open == 0 {
                if let Some(content_start) = trimmed.find('>').map(|i| i + 1) {
                    if let Some(close_rel) = trimmed[content_start..].find("</div>") {
                        let content = &trimmed[content_start..content_start + close_rel];
                        if content.trim() == title {
                            return body_html.to_string();
                        }
                    }
                }
            }
        }
        // Already starts with <span...>{title}</span>? Apple wraps single-line
        // titles this way (carrying caret-color/font-family/etc. from the
        // editing context). Without this case we'd inject a <div>title</div>
        // ABOVE Apple's span — Apple Notes then renders the title on two lines.
        if let Some(span_open) = trimmed.find("<span") {
            if span_open == 0 {
                if let Some(content_start) = trimmed.find('>').map(|i| i + 1) {
                    if let Some(close_rel) = trimmed[content_start..].find("</span>") {
                        let content = &trimmed[content_start..content_start + close_rel];
                        if content.trim() == title {
                            return body_html.to_string();
                        }
                    }
                }
            }
        }
        // Already starts with bare {title}?
        if trimmed.starts_with(title) {
            return body_html.to_string();
        }

        // Inject.
        let title_div = format!("<div>{}</div>", title);
        return format!("{}{}{}", &body_html[..after_open], title_div, inner);
    }
    body_html.to_string()
}

// Apple Notes uses the first body element as the displayed title.
// On read, strip the leading `<div>{title}</div>`, `<span...>{title}</span>`,
// or bare-text title if the body opens with the subject — otherwise the editor
// would double-show it.
//
// Iterates: when a save was made by old Jodd on top of an Apple note, the body
// can hold both `<div>{title}</div>` (our injection) AND `<span>{title}</span>`
// (Apple's original) back-to-back. Strip one and the next pass catches the
// other — so we loop until a pass returns the input unchanged.
fn strip_leading_title(body_html: &str, title: &str) -> String {
    let mut current = body_html.to_string();
    loop {
        let next = strip_leading_title_once(&current, title);
        if next == current {
            return current;
        }
        current = next;
    }
}

fn strip_leading_title_once(body_html: &str, title: &str) -> String {
    if title.is_empty() {
        return body_html.to_string();
    }
    // Find the body open tag; we only operate inside <body...>...</body>
    let (head, inner_and_tail) = match body_html.find("<body") {
        Some(start) => {
            let after_open = match body_html[start..].find('>') {
                Some(o) => start + o + 1,
                None => return body_html.to_string(),
            };
            (&body_html[..after_open], &body_html[after_open..])
        }
        None => return body_html.to_string(),
    };

    // Case 1: <div>{title}</div> as first child (possibly with attributes/styles).
    // We do a lenient match: look for the first `<div...>title</div>` whose
    // text content equals our title.
    if let Some(div_open) = inner_and_tail.find("<div") {
        if inner_and_tail[..div_open].trim().is_empty() {
            let after_div_open = inner_and_tail[div_open..]
                .find('>')
                .map(|o| div_open + o + 1);
            if let Some(content_start) = after_div_open {
                if let Some(close_rel) = inner_and_tail[content_start..].find("</div>") {
                    let content = &inner_and_tail[content_start..content_start + close_rel];
                    if content.trim() == title {
                        let after_close = content_start + close_rel + "</div>".len();
                        return format!("{}{}", head, &inner_and_tail[after_close..]);
                    }
                }
            }
        }
    }

    // Case 3: <span...>{title}</span> as first child (Apple's single-line note
    // format — span carries inline styles like caret-color/font-family).
    // Without this case the editor would render the title twice: once in the
    // top bar (from Subject) and once as the first body line.
    if let Some(span_open) = inner_and_tail.find("<span") {
        if inner_and_tail[..span_open].trim().is_empty() {
            let after_span_open = inner_and_tail[span_open..]
                .find('>')
                .map(|o| span_open + o + 1);
            if let Some(content_start) = after_span_open {
                if let Some(close_rel) = inner_and_tail[content_start..].find("</span>") {
                    let content = &inner_and_tail[content_start..content_start + close_rel];
                    if content.trim() == title {
                        let after_close = content_start + close_rel + "</span>".len();
                        return format!("{}{}", head, &inner_and_tail[after_close..]);
                    }
                }
            }
        }
    }

    // Case 2: bare text title (e.g. Apple's `<body>english<div>...`)
    // If the body's first text content equals the title, strip it.
    let trimmed_start = inner_and_tail.trim_start();
    if let Some(rest) = trimmed_start.strip_prefix(title) {
        let leading_ws = &inner_and_tail[..inner_and_tail.len() - trimmed_start.len()];
        return format!("{}{}{}", head, leading_ws, rest);
    }

    body_html.to_string()
}

fn decode_body(data: &str) -> String {
    // Gmail uses base64url. Some payloads come through as standard base64 —
    // try both rather than silently returning empty.
    URL_SAFE
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(data))
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
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
                    "index: label {} ({}) → {} messages",
                    label_name, label_id, ids.len()
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
            Err(e) => log!("index: messages.list failed for {}: {}", label_id, e),
        }
    }
    log!("index: total {} unique messages across {} labels", out.len(), notes_label_ids.len());
    Ok(out)
}

/// Observation summary from a single list_notes pass. Used by the frontend
/// to display an unobtrusive "N duplicates" indicator so the user has a
/// signal that cleanup_orphans is worth running.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct DedupSummary {
    /// Extra Gmail messages collapsed into their primary by uuid.
    pub collapsed: usize,
    /// How many distinct uuids had at least one duplicate.
    pub uuids_affected: usize,
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
    label_id: &str,
    label_map: &HashMap<String, String>,
    cache_by_id: &HashMap<String, Note>,
) -> Result<Vec<Note>, String> {
    // Paginated list — same reason as list_notes: one label can hold
    // thousands of messages and the API caps a single page at 500.
    let ids = list_all_message_ids(token, label_id).await?;
    log!("list_notes_in_label: {} returned {} messages (all pages)", label_id, ids.len());

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
                        "list_notes_in_label: cache relabel id={} '{}' -> '{}'",
                        id, note.label, q
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
                "fields",
                "id,labelIds,payload(headers,body/data,parts(mimeType,body/data,parts(mimeType,body/data,parts(mimeType,body/data))))",
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

    // Inject the title as the first body element so Apple Notes displays it.
    // Skip injection if the body already starts with the title (idempotent saves).
    let body_with_title = inject_title_into_body(body_html, title);

    // Content-adaptive encoding (mirrors Apple):
    //   pure ASCII  → charset=us-ascii, 7bit, plain Subject, raw body
    //   non-ASCII   → charset=utf-8,   QP, RFC 2047 Subject, QP body
    let body_is_ascii = is_ascii(&body_with_title);
    let subject_is_ascii = is_ascii(title);
    let all_ascii = body_is_ascii && subject_is_ascii;

    let (charset, cte, subject_line, encoded_body) = if all_ascii {
        ("us-ascii", "7bit", title.to_string(), body_with_title)
    } else {
        (
            "utf-8",
            "quoted-printable",
            rfc2047_encode_subject(title),
            qp_encode_body(&body_with_title),
        )
    };

    // Message-ID format mirroring Apple: <UUID@user-domain>
    let domain = user_email.split('@').nth(1).unwrap_or("local.jodd");
    let message_id = format!("<{}@{}>", format_apple_uuid(uuid::Uuid::new_v4()), domain);

    // From header: real email if we have it, fall back to Gmail's "me" shortcut.
    let from = if user_email.is_empty() { "me".to_string() } else { user_email.to_string() };

    let raw = format!(
        "From: {from}\r\n\
        X-Uniform-Type-Identifier: com.apple.mail-note\r\n\
        Content-Type: text/html;\r\n\tcharset={charset}\r\n\
        Content-Transfer-Encoding: {cte}\r\n\
        Mime-Version: {mime}\r\n\
        Date: {date_header}\r\n\
        X-Mail-Created-Date: {created_date}\r\n\
        Subject: {subject_line}\r\n\
        X-Universally-Unique-Identifier: {uuid}\r\n\
        Message-Id: {message_id}\r\n\
        \r\n\
        {encoded_body}",
        mime = APPLE_MIME_VERSION
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
    log!("save_note: inserted new message id={}", inserted.id);

    // Replace, don't duplicate: best-effort delete the previous message after
    // the new one is safely in. We delete LAST so a network blip on delete
    // leaves a duplicate (recoverable) instead of data loss (not recoverable).
    if let Some(old_id) = existing_gmail_id.filter(|s| !s.is_empty()) {
        if old_id != inserted.id {
            match delete_note(token, old_id).await {
                Ok(_) => log!("save_note: deleted old message {}", old_id),
                Err(e) => log!(
                    "save_note: failed to delete old {}: {} (new note saved OK)",
                    old_id, e
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
// Cost: O(messages_in_Notes_labels) header fetches. For a 6k-note mailbox
// that's significant — restrict the caller's input to recent UUIDs.
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
pub async fn delete_note(token: &str, id: &str) -> Result<(), String> {
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
            log!("delete_note: id={} trashed (status={}, attempt {})", id, status, attempt + 1);
            return Ok(());
        }
        let body = res.text().await.unwrap_or_default();
        last_err = format!("HTTP {} — {}", status, body);
        log!("delete_note: attempt {} failed for id={}: {}", attempt + 1, id, last_err);
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
            if let Err(e) = delete_note(token, old).await {
                log!("save_meta_sidecar: delete old {} failed: {}", old, e);
            }
        }
    }
    Ok(inserted.id)
}

/// Trash a sidecar. Same as delete_note (which trashes any Gmail message)
/// but with a more descriptive name at the worker callsite.
pub async fn trash_meta_sidecar(token: &str, sidecar_id: &str) -> Result<(), String> {
    delete_note(token, sidecar_id).await
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
