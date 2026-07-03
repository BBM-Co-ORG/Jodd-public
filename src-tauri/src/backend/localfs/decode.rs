//! raw-RFC822 → neutral envelope. Implemented in task B2.

use mail_parser::{MessageParser, MimeHeaders};
use crate::backend::{Attachment, Note};

/// Parse raw .eml (RFC822) bytes into a neutral Note. `label` is the folder this
/// file lives in (the caller derives it from the file's path). Reuses mime822
/// helpers for the HTML body. The `id` (remote path) is filled by the caller.
pub fn decode_eml(bytes: &[u8], label: &str) -> Result<Note, String> {
    let msg = MessageParser::default()
        .parse(bytes)
        .ok_or_else(|| "not a valid RFC822 message".to_string())?;

    // Subject → title (RFC2047 already decoded by mail-parser; recover mis-encoded as a fallback).
    let title_raw = msg.subject().unwrap_or("").to_string();
    let title = crate::mime822::try_recover_mis_decoded_utf8(&title_raw)
        .unwrap_or(title_raw);

    // Helper to read a raw header value as text.
    let hdr = |name: &str| -> String {
        msg.header(name)
            .and_then(|h| h.as_text())
            .map(|s| s.to_string())
            .unwrap_or_default()
    };

    // X-UUID, Date, X-Mail-Created-Date headers.
    let uuid_raw = hdr("X-Universally-Unique-Identifier");
    let uuid = crate::mime822::canonicalize_uuid(&uuid_raw).unwrap_or(uuid_raw);
    // "Date" is a STANDARD header — mail-parser parses it into a structured
    // DateTime, so `hdr("Date")` (.as_text()) returns empty and the UI shows
    // "Invalid Date". Read the parsed date and render it back to RFC 2822 so it
    // matches what the rest of the app stores/parses (chrono parse_from_rfc2822,
    // JS new Date()). Fall back to the raw text header if parsing didn't happen.
    let date = msg
        .date()
        .map(|d| d.to_rfc822())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| hdr("Date"));
    let x_mail = {
        let v = hdr("X-Mail-Created-Date");
        (!v.is_empty()).then_some(v)
    };

    // HTML body (first text/html part).
    let html = msg.body_html(0).map(|c| c.into_owned()).unwrap_or_default();
    let body_html = crate::mime822::strip_leading_title(&html, &title);

    // Attachments: iterate over all parts that have a Content-Id.
    let mut attachments = Vec::new();
    for part in msg.attachments() {
        let cid = match part.content_id() {
            Some(c) if !c.is_empty() => c,
            _ => continue,
        };
        // Strip surrounding angle brackets that RFC 2822 wraps Content-Id in.
        let cid_clean = cid.trim_matches(|c: char| c == '<' || c == '>').to_string();

        let mime_type = part
            .content_type()
            .map(|ct| match ct.subtype() {
                Some(st) => format!("{}/{}", ct.ctype(), st),
                None => ct.ctype().to_string(),
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());

        attachments.push(Attachment {
            content_id: cid_clean,
            mime_type,
            filename: part.attachment_name().map(|s| s.to_string()),
            x_apple_part_url: None,
            data: part.contents().to_vec(),
        });
    }

    Ok(Note {
        id: String::new(),
        uuid,
        title,
        body_html,
        date,
        label: label.to_string(),
        x_mail_created_date: x_mail,
        account_id: None,
        pinned: false,
        attachments,
    })
}

#[cfg(test)]
mod tests {
    use super::decode_eml;

    #[test]
    fn build_then_decode_roundtrips_envelope() {
        let raw = crate::mime822::build_note_mime(
            "My Title",
            "<html><body><div>My Title</div><div>hello #t [[L-abcd1234]]</div></body></html>",
            "AAAA-BBBB",
            "Thu, 4 Jun 2026 01:19:50 +0700",
            "Mon, 1 Jan 2024 09:00:00 +0700",
            "u@x.com",
            &[],
        );
        let d = decode_eml(raw.as_bytes(), "Notes").expect("decode");
        assert_eq!(d.uuid, "AAAA-BBBB");
        assert_eq!(d.title, "My Title");
        assert_eq!(d.label, "Notes");
        assert!(
            d.body_html.contains("hello #t [[L-abcd1234]]"),
            "body retained, got: {}",
            d.body_html
        );
        assert!(
            !d.body_html.contains("<div>My Title</div>"),
            "title row must be stripped, got: {}",
            d.body_html
        );
        assert_eq!(
            d.x_mail_created_date.as_deref(),
            Some("Mon, 1 Jan 2024 09:00:00 +0700")
        );
        // `date` must be a real, RFC2822-parseable value — NOT empty (which the
        // UI renders as "Invalid Date"). Regression guard for the standard-header
        // decode bug (mail-parser stores Date structured, not as text).
        assert!(!d.date.is_empty(), "date must not be empty (would show 'Invalid Date')");
        let want = chrono::DateTime::parse_from_rfc2822("Thu, 4 Jun 2026 01:19:50 +0700").unwrap();
        let got = chrono::DateTime::parse_from_rfc2822(&d.date)
            .expect("decoded date must be RFC2822-parseable, got broken value");
        assert_eq!(got, want, "Date header must round-trip to the same instant");
    }

    #[test]
    fn decode_multipart_with_attachment() {
        let att = crate::mime822::MimeAttachment {
            content_id: "C1@x",
            mime_type: "image/png",
            filename: Some("i.png"),
            x_apple_part_url: None,
            data: &[1u8, 2, 3, 4],
        };
        let raw = crate::mime822::build_note_mime(
            "T",
            "<html><body><div>T</div><object data=\"cid:C1@x\"></object></body></html>",
            "U-1",
            "Thu, 4 Jun 2026 01:19:50 +0700",
            "Thu, 4 Jun 2026 01:19:50 +0700",
            "u@x.com",
            &[att],
        );
        let d = decode_eml(raw.as_bytes(), "Notes").expect("decode");
        assert_eq!(d.attachments.len(), 1, "one attachment expected");
        assert_eq!(d.attachments[0].content_id, "C1@x");
        assert_eq!(d.attachments[0].data, vec![1u8, 2, 3, 4]);
    }

    /// Thai title and Thai body must survive build_note_mime → decode_eml intact.
    /// build_note_mime encodes non-ASCII subjects as RFC2047 encoded-words and the
    /// body as quoted-printable (charset=utf-8). mail-parser 0.9 is expected to
    /// decode both transparently. This is CRITICAL for Jodd's primary Thai audience.
    #[test]
    fn thai_title_and_body_roundtrip() {
        let thai_title = "ทดสอบบันทึก";
        let thai_body_text = "เนื้อหาภาษาไทย";
        let body_html = format!(
            "<html><body><div>{}</div><div>{}</div></body></html>",
            thai_title, thai_body_text
        );
        let raw = crate::mime822::build_note_mime(
            thai_title,
            &body_html,
            "THAI-UUID-1234",
            "Thu, 4 Jun 2026 01:19:50 +0700",
            "Thu, 4 Jun 2026 01:19:50 +0700",
            "u@x.com",
            &[],
        );

        // The raw EML must contain an RFC2047 encoded Subject (not raw Thai bytes).
        assert!(
            raw.contains("=?utf-8?"),
            "Subject should be RFC2047 encoded for non-ASCII, got headers:\n{}",
            &raw[..raw.find("\r\n\r\n").unwrap_or(raw.len())]
        );
        // And quoted-printable transfer encoding for the body.
        assert!(
            raw.contains("Content-Transfer-Encoding: quoted-printable"),
            "body should be QP-encoded"
        );

        let d = decode_eml(raw.as_bytes(), "Notes/Thai").expect("decode Thai note");

        assert_eq!(
            d.title, thai_title,
            "Thai title must survive RFC2047 encode→decode round-trip"
        );
        assert_eq!(d.label, "Notes/Thai");
        assert!(
            d.body_html.contains(thai_body_text),
            "Thai body text must survive QP encode→decode round-trip, got: {}",
            d.body_html
        );
        assert!(
            !d.body_html.contains(&format!("<div>{}</div>", thai_title)),
            "title row must be stripped from body, got: {}",
            d.body_html
        );
    }
}
