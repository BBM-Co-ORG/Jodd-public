//! Deriver for the Gmail (Apple-HTML) vertical. Formalizes the content → neutral
//! index bridge by delegating to the SAME pure body parsers `db.rs` uses on the
//! write path, so this neutral view matches the indexed view exactly. The
//! write-path derivation in `db.rs` remains authoritative and untouched; this
//! impl is the seam a future backend (JMAP) plugs a different content parser
//! into. Synchronous: local CPU/ms work, off the network.
//!
//! Note: tags and edges are body-resident and re-derived everywhere — they are
//! never carried in a sidecar. `edges` here surfaces the content-derived
//! `mentions` relation (`[[wikilinks]]`); the `child_of` (note→folder) and
//! `tagged` (note→tag) edges are derived from the envelope/label and the `tags`
//! list respectively, not from free body content, so they are not duplicated
//! here.

use super::GmailVertical;
use crate::backend::{ContentKind, Derived, Deriver};

impl Deriver for GmailVertical {
    fn derive(&self, kind: ContentKind, blob: &[u8]) -> Derived {
        crate::backend::deriver_applehtml::AppleHtmlDeriver.derive(kind, blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vertical() -> GmailVertical {
        GmailVertical::new(String::new(), HashMap::new(), String::new(), String::new())
    }

    #[test]
    fn derive_extracts_text_tags_and_mention_edges() {
        let body = "<div>hello #work world</div><div>see [[Other Note-abcd1234]]</div>";
        let d = vertical().derive(ContentKind::AppleHtml, body.as_bytes());
        assert!(d.text.contains("hello"));
        assert!(d.text.contains("world"));
        assert!(!d.text.contains('<'), "HTML tags must be stripped from text");
        assert!(d.tags.contains(&"work".to_string()));
        assert_eq!(d.edges.len(), 1);
        assert_eq!(d.edges[0].rel, "mentions");
        assert_eq!(d.edges[0].target, "Other Note-abcd1234");
    }
}
