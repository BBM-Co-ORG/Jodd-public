//! Format-neutral MIME / Apple-Notes helpers shared by every email-family
//! backend (Gmail today; IMAP / JMAP / Graph later). These operate on strings
//! and bytes only — nothing here knows about Gmail's REST JSON shape. The
//! Gmail-JSON-coupled MIME-tree walkers (collect_pending_attachments,
//! find_html_in_parts) stay in the Gmail vertical because Gmail pre-parses
//! MIME into JSON; a raw-RFC822 backend would parse bytes instead but reuse
//! every helper below.

use base64::{Engine as _, engine::general_purpose::URL_SAFE};

// Apple's exact Mime-Version masquerade — recognized by Apple Notes as a
// native-client message. Without this, Apple may treat our edits as foreign.
pub const APPLE_MIME_VERSION: &str = "1.0 (Mac OS X Notes 4.13 \\(3146.121.7\\))";

// Inline tags that are PART OF the title (the user styled part of it). Anything
// else — <div>, <br>, <object>, <img>, … — ends the title line.
pub const INLINE_TITLE_TAGS: &[&str] = &[
    "b", "i", "u", "s", "strike", "em", "strong", "span", "font", "a", "sub",
    "sup", "mark", "code", "small", "big",
];

// Recover a Subject that was saved by pre-fix Jodd: raw UTF-8 bytes written
// to a header that's spec'd as 7-bit ASCII. Gmail returns those bytes mis-decoded
// as Latin-1 / Windows-1252, producing strings like "à¸—à¸"à¸ªà¸­à¸š" for "ทดสอบ".
//
// Heuristic: cast each char back to its Latin-1 byte value (with a small map for
// the Windows-1252 supplements 0x80–0x9F), then try to decode the byte sequence
// as UTF-8. If the result is valid UTF-8 with non-Latin-1 codepoints (i.e. real
// multi-byte UTF-8 chars), it's almost certainly a real recovery. Legitimate
// Latin-1/CP1252 input would fail UTF-8 validation due to lone high bytes.
pub fn try_recover_mis_decoded_utf8(s: &str) -> Option<String> {
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

// Returns true if the entire string is pure ASCII (no bytes ≥ 0x80).
// Controls content-adaptive encoding choice (us-ascii+7bit vs utf-8+QP).
pub fn is_ascii(s: &str) -> bool {
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
pub fn format_apple_date(dt: chrono::DateTime<chrono::Local>) -> String {
    dt.format("%a, %-d %b %Y %H:%M:%S %z").to_string()
}

// RFC 2047 encoded-word for a Subject header. Picks B (base64) vs Q
// (quoted-printable-like) by whichever produces shorter output, matching
// Apple's strategy. For pure-ASCII inputs the caller should skip encoding
// entirely (we still handle that case safely by returning the original).
pub fn rfc2047_encode_subject(text: &str) -> String {
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
pub fn qp_encode_body(s: &str) -> String {
    String::from_utf8_lossy(&quoted_printable::encode(s.as_bytes())).into_owned()
}

// Remove HTML tags, returning only text content. Used to compare the body's
// title row against the (plain-text) Subject even when the user styled part of
// the title — e.g. `new from iphone <b><i>เพิ่มภาษาไทย</i></b>`.
pub fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

// Byte offset of the first tag in `s` that is NOT an inline-formatting tag —
// i.e. the first block or embedded element (<div>, <br>, <object>, <img>, …).
// This is what bounds the title line: a title may carry inline styling but ends
// the moment real content (a block, or an embedded attachment) begins. None if
// there is no such tag.
pub fn first_block_or_embed(s: &str) -> Option<usize> {
    let mut i = 0;
    while i < s.len() {
        if s.as_bytes()[i] == b'<' {
            let after = &s[i + 1..];
            let name_src = after.strip_prefix('/').unwrap_or(after);
            let name: String = name_src
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if !name.is_empty() && !INLINE_TITLE_TAGS.contains(&name.as_str()) {
                return Some(i);
            }
        }
        i += s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    None
}

// Split the body's inner-HTML (content after `<body...>`) into the title row
// and the rest. Returns (title_text, remainder_start): title_text is the
// tag-stripped, trimmed text of the first "line" (a leading `<div>…</div>`, or
// everything up to the first block/embed tag — which preserves inline-styled
// titles like `foo <b>bar</b>`), and remainder_start is that row's byte offset
// within `inner`. title_text equals the plain Subject for every title shape
// Apple emits (bare / `<div>`-wrapped / `<span>`-wrapped / partly styled).
pub fn first_line_split(inner: &str) -> (String, usize) {
    let trimmed = inner.trim_start();
    let ws = inner.len() - trimmed.len();
    let line_end = if trimmed.starts_with("<div") {
        // Title wrapped in a <div> — the row is that whole div.
        match trimmed.find("</div>") {
            Some(c) => c + "</div>".len(),
            None => trimmed.len(),
        }
    } else {
        // Leading text + inline styling, up to the first block/embed tag. This
        // is what preserves a trailing <object>/<img> (e.g. a title+image-only
        // note `img<object cid:…>`) — only "img" is stripped, the image stays.
        first_block_or_embed(trimmed).unwrap_or(trimmed.len())
    };
    let text = strip_html_tags(&trimmed[..line_end]).trim().to_string();
    (text, ws + line_end)
}

// Split a stored body into (head, inner) — the prefix to preserve verbatim and
// the content whose FIRST LINE is the title Apple Notes displays.
//
// Two framings, one meaning of "inner":
//   * full document (what Apple authors): head = `<html>…<body …>`, inner =
//     everything after the body open tag.
//   * bare fragment (what JODD authors — Extract's `assemble_note_body`,
//     jodd-mcp's `md_to_html`/`sanitize_note_html`): head = "", inner = the
//     whole string. Before this existed both title functions required a
//     literal `<body` and silently no-op'd on every Jodd-authored note, so the
//     title never reached the body at all and Apple showed the first sentence
//     as the note's pseudo-title.
//
// None only for a malformed `<body` with no closing `>`, which both functions
// have always passed through untouched.
fn split_head_inner(body_html: &str) -> Option<(&str, &str)> {
    match body_html.find("<body") {
        Some(start) => {
            let after_open = start + body_html[start..].find('>')? + 1;
            Some((&body_html[..after_open], &body_html[after_open..]))
        }
        None => Some(("", body_html)),
    }
}

// Byte offset in `inner` just past a leading, literal `<div>{title}</div>` —
// the exact inverse of what `inject_title_into_body` prepends. The tag-stripped
// comparison below cannot recognize its own output when the title itself
// contains angle brackets (a real title: `ทดสอบ Title <h3>` strips down to
// `ทดสอบ Title`), which would leave the row in the body and show the title
// twice. Matching our own emission literally makes the round-trip exact for
// ANY title.
fn leading_literal_title_div(inner: &str, title: &str) -> Option<usize> {
    let title_div = format!("<div>{}</div>", title);
    let trimmed = inner.trim_start();
    let ws = inner.len() - trimmed.len();
    trimmed
        .starts_with(&title_div)
        .then(|| ws + title_div.len())
}

// Inject the title as the first <div> of the body content. Idempotent: if the
// body's first line already IS the title (any styling), we don't double-inject.
// Matches Apple's convention that the body's first element is the displayed
// title.
pub fn inject_title_into_body(body_html: &str, title: &str) -> String {
    if title.is_empty() {
        return body_html.to_string();
    }
    let Some((head, inner)) = split_head_inner(body_html) else {
        return body_html.to_string();
    };
    // Already carries the title — literally ours, or bare/wrapped/styled.
    if leading_literal_title_div(inner, title).is_some() {
        return body_html.to_string();
    }
    let (first_text, _) = first_line_split(inner);
    if first_text == title.trim() {
        return body_html.to_string();
    }
    format!("{}<div>{}</div>{}", head, title, inner)
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
pub fn strip_leading_title(body_html: &str, title: &str) -> String {
    let mut current = body_html.to_string();
    loop {
        let next = strip_leading_title_once(&current, title);
        if next == current {
            return current;
        }
        current = next;
    }
}

pub fn strip_leading_title_once(body_html: &str, title: &str) -> String {
    if title.is_empty() {
        return body_html.to_string();
    }
    // `<body...>`-relative for a full document, whole-string for a fragment.
    let Some((head, inner)) = split_head_inner(body_html) else {
        return body_html.to_string();
    };

    // Exact inverse of our own injection first — see `leading_literal_title_div`.
    if let Some(remainder_start) = leading_literal_title_div(inner, title) {
        return format!("{}{}", head, &inner[remainder_start..]);
    }

    // The displayed title is the body's first line. Compare its tag-stripped
    // text to the Subject — this handles bare, <div>-wrapped, <span>-wrapped,
    // AND partly-styled titles (e.g. `foo <b><i>bar</i></b>`) with one path.
    // The previous per-shape exact-HTML matching silently failed on styled
    // titles, leaving the title row in the body → editor showed it twice and a
    // re-save prepended a plain duplicate.
    let (first_text, remainder_start) = first_line_split(inner);
    if first_text == title.trim() {
        return format!("{}{}", head, &inner[remainder_start..]);
    }

    body_html.to_string()
}

pub fn decode_body(data: &str) -> String {
    // Gmail uses base64url. Some payloads come through as standard base64 —
    // try both rather than silently returning empty.
    URL_SAFE
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(data))
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

// Decode base64url (Gmail's default) → bytes, falling back to standard base64.
pub fn decode_b64_bytes(data: &str) -> Option<Vec<u8>> {
    URL_SAFE
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(data))
        .ok()
}

// Extract every `cid:<id>` referenced in the body (Apple's inline-attachment
// links live in `<object … data="cid:X">`). Used to decide which stored
// attachments to re-emit on save — only those the body still references, so
// removing an image from the body removes it from the message too.
pub fn referenced_cids(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(pos) = rest.find("cid:") {
        let after = &rest[pos + 4..];
        let end = after
            .find(|c: char| c == '"' || c == '\'' || c == '>' || c == '<' || c.is_whitespace())
            .unwrap_or(after.len());
        let cid = &after[..end];
        if !cid.is_empty() && !out.iter().any(|c| c == cid) {
            out.push(cid.to_string());
        }
        rest = &after[end..];
    }
    out
}

// Base64-encode bytes and hard-wrap at 76 columns with CRLF (RFC 2045), the
// shape Apple emits for attachment parts.
pub fn base64_mime_wrap(data: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    let mut out = String::with_capacity(b64.len() + b64.len() / 76 * 2 + 2);
    let mut i = 0;
    let bytes = b64.as_bytes();
    while i < bytes.len() {
        let end = (i + 76).min(bytes.len());
        out.push_str(&b64[i..end]);
        out.push_str("\r\n");
        i = end;
    }
    out
}

// Build a `data:` URI for an attachment so the frontend can render it inline
// (e.g. <img src="data:image/png;base64,…">).
pub fn data_uri(mime_type: &str, data: &[u8]) -> String {
    format!(
        "data:{};base64,{}",
        mime_type,
        base64::engine::general_purpose::STANDARD.encode(data)
    )
}

/// Neutral attachment input for the RFC822 builder. The Gmail vertical's
/// `Attachment` is adapted into this at the call boundary.
#[derive(Debug)]
pub struct MimeAttachment<'a> {
    pub content_id: &'a str,
    pub mime_type: &'a str,
    pub filename: Option<&'a str>,
    pub x_apple_part_url: Option<&'a str>,
    pub data: &'a [u8],
}

/// Build the raw RFC822 message bytes for an Apple note, matching Apple Notes'
/// on-wire shape (content-adaptive us-ascii/7bit vs utf-8/QP; single text/html
/// or multipart/related when attachments are referenced). Returns the raw
/// string; the caller base64url-encodes and POSTs it.
///
/// `body_html` is EDITOR-VIEW (title not yet injected) — this fn injects the
/// title as the first element (idempotent). `used` is the set of attachments
/// the body references (caller pre-filters via referenced_cids).
pub fn build_note_mime(
    title: &str,
    body_html: &str,
    uuid: &str,
    date_header: &str,
    created_date: &str,
    user_email: &str,
    used: &[MimeAttachment<'_>],
) -> String {
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

    let raw = if used.is_empty() {
        // Single-part text/html — the common case (no attachments).
        format!(
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
        )
    } else {
        // multipart/related mirroring Apple: part 0 = the text/html body (which
        // still carries the <object data="cid:…"> refs), parts 1..N = each
        // referenced attachment with its ORIGINAL Content-Id so the refs stay
        // valid. This is what stops the image data-loss bug on re-save.
        let boundary = format!("Apple-Mail-{}", format_apple_uuid(uuid::Uuid::new_v4()));
        let mut body_parts = format!(
            "--{boundary}\r\n\
            Content-Type: text/html;\r\n\tcharset={charset}\r\n\
            Content-Transfer-Encoding: {cte}\r\n\
            \r\n\
            {encoded_body}\r\n"
        );
        for a in used {
            let filename = a.filename.map(|s| s.to_string()).unwrap_or_else(|| "attachment".to_string());
            let part_url = a
                .x_apple_part_url
                .unwrap_or(a.content_id);
            body_parts.push_str(&format!(
                "--{boundary}\r\n\
                Content-Type: {mime_type};\r\n\tname={filename};\r\n\tx-apple-part-url=\"{part_url}\"\r\n\
                Content-Disposition: inline;\r\n\tfilename={filename}\r\n\
                Content-Transfer-Encoding: base64\r\n\
                Content-Id: <{cid}>\r\n\
                \r\n\
                {b64}",
                mime_type = a.mime_type,
                cid = a.content_id,
                b64 = base64_mime_wrap(a.data),
            ));
        }
        body_parts.push_str(&format!("--{boundary}--"));
        format!(
            "From: {from}\r\n\
            X-Uniform-Type-Identifier: com.apple.mail-note\r\n\
            Content-Type: multipart/related;\r\n\ttype=\"text/html\";\r\n\tboundary={boundary}\r\n\
            Content-Transfer-Encoding: 7bit\r\n\
            Mime-Version: {mime}\r\n\
            Date: {date_header}\r\n\
            X-Mail-Created-Date: {created_date}\r\n\
            Subject: {subject_line}\r\n\
            X-Universally-Unique-Identifier: {uuid}\r\n\
            Message-Id: {message_id}\r\n\
            \r\n\
            {body_parts}",
            mime = APPLE_MIME_VERSION
        )
    };
    raw
}

#[cfg(test)]
mod title_tests {
    use super::{inject_title_into_body, strip_leading_title};

    const WRAP: &str = "<html><head></head><body style=\"overflow-wrap: break-word;\">";

    fn body(inner: &str) -> String {
        format!("{}{}</body></html>", WRAP, inner)
    }

    // Real iPhone specimen: title is bare text + an inline-styled span. The old
    // exact-HTML matching failed here, duplicating the title.
    #[test]
    fn strips_partly_styled_title() {
        let title = "new from iphone เพิ่มภาษาไทย(bold italic)";
        let b = body("new from iphone <b><i>เพิ่มภาษาไทย(bold italic)</i></b><div><b>body bold</b></div>");
        let stripped = strip_leading_title(&b, title);
        assert_eq!(stripped, body("<div><b>body bold</b></div>"));
    }

    #[test]
    fn strips_bare_text_title() {
        let title = "new from iphone";
        let b = body("new from iphone<div>line two</div>");
        assert_eq!(strip_leading_title(&b, title), body("<div>line two</div>"));
    }

    #[test]
    fn strips_div_wrapped_title() {
        let title = "My Note";
        let b = body("<div>My Note</div><div>line two</div>");
        assert_eq!(strip_leading_title(&b, title), body("<div>line two</div>"));
    }

    #[test]
    fn strips_span_wrapped_title() {
        let title = "My Note";
        let b = body("<span style=\"caret-color: rgb(0,0,0)\">My Note</span><div>x</div>");
        assert_eq!(strip_leading_title(&b, title), body("<div>x</div>"));
    }

    #[test]
    fn does_not_strip_non_title_first_line() {
        let title = "Subject Here";
        let b = body("totally different<div>x</div>");
        assert_eq!(strip_leading_title(&b, title), b);
    }

    #[test]
    fn inject_is_idempotent_on_styled_title() {
        // The crux of the round-trip bug: re-saving must NOT prepend a plain
        // duplicate when the first line already carries the (styled) title.
        let title = "new from iphone เพิ่มภาษาไทย(bold italic)";
        let b = body("new from iphone <b><i>เพิ่มภาษาไทย(bold italic)</i></b><div>x</div>");
        assert_eq!(inject_title_into_body(&b, title), b);
    }

    #[test]
    fn inject_adds_title_when_absent() {
        let title = "Fresh";
        let b = body("<div>just body</div>");
        assert_eq!(
            inject_title_into_body(&b, title),
            body("<div>Fresh</div><div>just body</div>")
        );
    }

    // strip then inject (the read→edit→save path) must converge, not duplicate.
    #[test]
    fn strip_then_inject_roundtrips_styled_title() {
        let title = "new from iphone เพิ่มภาษาไทย(bold italic)";
        let wire = body("new from iphone <b><i>เพิ่มภาษาไทย(bold italic)</i></b><div>x</div>");
        let editor_view = strip_leading_title(&wire, title); // pull stores this
        let reinjected = inject_title_into_body(&editor_view, title); // push re-adds
        // Re-injects a plain title row, but exactly ONE — and a second strip
        // returns to the editor view (no accumulation).
        assert_eq!(strip_leading_title(&reinjected, title), editor_view);
    }

    // Real img.eml specimen: title + image only, no <div>. The title must strip
    // but the trailing <object> (the image) MUST survive — regression guard for
    // the bug where the whole line (image included) was stripped.
    #[test]
    fn strips_title_keeps_trailing_image_object() {
        let title = "img";
        let obj = "<object type=\"application/x-apple-msg-attachment\" \
            data=\"cid:C227C1F3@mobilenotes.apple.com\"></object>";
        let b = body(&format!("img{}", obj));
        assert_eq!(strip_leading_title(&b, title), body(obj));
    }

    // --- Fragment bodies (no <html>/<body> wrapper) -------------------------
    // Every note Jodd itself authors is a bare fragment: Extract's
    // `assemble_note_body`, and jodd-mcp's `create_note`/`update_note`
    // (`md_to_html`/`sanitize_note_html`). Before this fix both functions
    // required a literal `<body` and silently no-op'd on these, so the title
    // Apple Notes displays (the body's first line) was simply absent.

    #[test]
    fn inject_adds_title_to_a_bare_fragment_no_body_tag() {
        // The exact shape md_to_html/sanitize_note_html/assemble_note_body
        // produce — confirmed live via jodd-mcp create_note.
        let title = "Fresh";
        let frag = "<p>just a paragraph</p>";
        let result = inject_title_into_body(frag, title);
        assert_eq!(result, "<div>Fresh</div><p>just a paragraph</p>");
    }

    #[test]
    fn inject_is_idempotent_on_a_fragment_whose_first_line_already_matches() {
        let title = "Fresh";
        let already = "<div>Fresh</div><p>just a paragraph</p>";
        assert_eq!(inject_title_into_body(already, title), already);
    }

    #[test]
    fn strip_removes_the_injected_title_from_a_fragment() {
        let title = "Fresh";
        let injected = "<div>Fresh</div><p>just a paragraph</p>";
        assert_eq!(
            strip_leading_title(injected, title),
            "<p>just a paragraph</p>"
        );
    }

    // Scope note: this proves JODD's OWN inject→strip cycle is exact for a
    // fragment — what Jodd writes, Jodd reads back byte-identically. It does
    // NOT prove the note survives a round trip through *Apple*. The title
    // here is injected unescaped, so `<h3>` reaches the wire as markup: a
    // real Apple Notes client renders that as an unclosed heading, and if
    // Apple re-normalizes and saves the HTML the next read sees a shape
    // neither branch of `strip_leading_title` recognizes. Escaping on inject
    // is a deliberate follow-up (it also has to change
    // `leading_literal_title_div` and the `first_line_split` comparison);
    // until it lands, do not read this test as an Apple-fidelity guarantee.
    #[test]
    fn fragment_round_trips_inject_then_strip_back_to_original() {
        let title = "ทดสอบ Title <h3>"; // real Thai + HTML-shaped title from live testing
        let frag = "<p>A quick test note created to verify Jodd's write path works.</p>\n";
        let injected = inject_title_into_body(frag, title);
        assert_ne!(
            injected, frag,
            "injection must actually add something for a fragment"
        );
        assert_eq!(strip_leading_title(&injected, title), frag);
    }

    #[test]
    fn full_document_behavior_is_unchanged() {
        // Regression guard: the existing <body>-wrapped path must be
        // byte-identical to its pre-fix behavior.
        let title = "Fresh";
        let b = body("<div>just body</div>"); // existing `body()` test helper
        assert_eq!(
            inject_title_into_body(&b, title),
            body("<div>Fresh</div><div>just body</div>")
        );
    }
}

#[cfg(test)]
mod mime_byte_tests {
    use super::{base64_mime_wrap, referenced_cids};

    #[test]
    fn referenced_cids_extracts_and_dedupes() {
        let body = "<div>x</div><div><object type=\"application/x-apple-msg-attachment\" \
            data=\"cid:03D58874@mobilenotes.apple.com\"></object></div>\
            <div><object data=\"cid:03D58874@mobilenotes.apple.com\"></object></div>";
        assert_eq!(referenced_cids(body), vec!["03D58874@mobilenotes.apple.com"]);
        assert!(referenced_cids("<div>no images here</div>").is_empty());
    }

    #[test]
    fn base64_wrap_at_76_cols() {
        let data = vec![0u8; 100]; // 100 bytes → 136 base64 chars → 2 lines
        let wrapped = base64_mime_wrap(&data);
        for line in wrapped.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(line.len() <= 76, "line exceeds 76 cols: {}", line.len());
        }
        // Round-trips back to the original bytes once newlines are stripped.
        use base64::Engine;
        let joined: String = wrapped.split("\r\n").collect();
        let decoded = base64::engine::general_purpose::STANDARD.decode(joined).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn build_note_mime_ascii_single_part_golden() {
        let raw = super::build_note_mime(
            "Hello", "<html><body><div>Hello</div><div>world</div></body></html>",
            "AAAA-BBBB", "Thu, 4 Jun 2026 01:19:50 +0700",
            "Thu, 4 Jun 2026 01:19:50 +0700", "u@example.com", &[],
        );
        assert!(raw.contains("X-Uniform-Type-Identifier: com.apple.mail-note"));
        assert!(raw.contains("X-Universally-Unique-Identifier: AAAA-BBBB"));
        assert!(raw.contains("charset=us-ascii"));
        assert!(raw.contains("Content-Transfer-Encoding: 7bit"));
        assert!(raw.contains("Subject: Hello"));
        // body present, title injected idempotently (no double "Hello" row)
        assert!(raw.contains("<div>Hello</div><div>world</div>"));
    }

    #[test]
    fn build_note_mime_multipart_when_attachment_referenced() {
        let body = "<html><body><div>t</div><object data=\"cid:C1@x\"></object></body></html>";
        let att = super::MimeAttachment {
            content_id: "C1@x", mime_type: "image/png",
            filename: Some("i.png"), x_apple_part_url: None, data: &[1u8, 2, 3],
        };
        let raw = super::build_note_mime("t", body, "U", "D", "C", "u@x.com", &[att]);
        assert!(raw.contains("multipart/related"));
        assert!(raw.contains("Content-Id: <C1@x>"));
        assert!(raw.contains("Content-Type: image/png"));
        // x_apple_part_url None → defaults to content_id
        assert!(raw.contains("x-apple-part-url=\"C1@x\""));
    }

    // Distinct Date vs X-Mail-Created-Date so a caller transposing the two
    // args (they're both &str and adjacent) is caught — each must land in its
    // own header.
    #[test]
    fn build_note_mime_date_and_created_date_not_transposed() {
        let raw = super::build_note_mime(
            "T", "<html><body><div>T</div></body></html>", "U",
            "Thu, 4 Jun 2026 01:19:50 +0700", // date_header
            "Mon, 1 Jan 2024 09:00:00 +0700", // created_date
            "u@x.com", &[],
        );
        assert!(raw.contains("Date: Thu, 4 Jun 2026 01:19:50 +0700\r\n"));
        assert!(raw.contains("X-Mail-Created-Date: Mon, 1 Jan 2024 09:00:00 +0700\r\n"));
    }
}
