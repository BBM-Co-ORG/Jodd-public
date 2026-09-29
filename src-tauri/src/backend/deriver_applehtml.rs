use crate::backend::{ContentKind, Derived, Deriver, Edge};

/// Deriver for the Apple-HTML content model, shared by every vertical that
/// stores Apple-HTML bodies (Gmail, LocalFS). Delegates to the same pure db.rs
/// body parsers the write path uses, so the neutral view matches the index.
pub struct AppleHtmlDeriver;

impl Deriver for AppleHtmlDeriver {
    fn derive(&self, _kind: ContentKind, blob: &[u8]) -> Derived {
        let body = std::str::from_utf8(blob).unwrap_or("");
        let text = crate::db::strip_html_to_text(body);
        let tags = crate::db::tags_from_body(body);
        let edges = crate::db::extract_wikilinks(body)
            .into_iter().map(|target| Edge { rel: "mentions".to_string(), target }).collect();
        Derived { text, tags, edges }
    }
}
