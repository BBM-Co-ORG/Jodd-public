//! Citation keys and slugs.
//!
//! The AI cites sources by short keys such as `[S3]`. These pure functions turn
//! the keys we issued into `[[slug]]` wikilinks (which Jodd's edge derivation
//! turns into backlinks) and strip every key we did not issue, so a made-up
//! citation can never reach the saved note.

use std::collections::HashMap;

use crate::db;

/// The `[[…]]` target for a source note. Always ends in `-<8 hex>` so that
/// `parse_link_key` resolves it by id: a title that slugifies to nothing
/// (empty, emoji, punctuation) gets `untitled-<8 hex>` rather than a bare id.
pub fn citation_slug(title: &str, uuid: &str) -> String {
    let slug = db::note_slug(title, uuid);
    if slug.contains('-') {
        slug
    } else {
        format!("untitled-{}", db::uuid_short(uuid))
    }
}

/// The result of [`resolve_keys`].
pub struct Resolved {
    pub markdown: String,
    /// Keys we did not issue, dropped: the note's "were removed" notice counts these.
    pub removed_keys: usize,
    /// `[[links]]` the AI wrote itself, unwrapped to plain text (no notice).
    pub unwrapped_links: usize,
}

/// Replace known source keys with `[[slug]]` and strip the rest.
///
/// 1. A `[[inner]]` the AI wrote itself becomes `inner` (counted in
///    `unwrapped_links`): only links we build may reach the note. When `inner`
///    is itself a key group (`[[S1]]`), it becomes `[S1]` instead and step 2
///    resolves it as a key.
/// 2. A `[…]` group made only of `S<digits>` tokens (case-insensitive, separated
///    by commas, semicolons or spaces) becomes the `[[slug]]` of each known key,
///    joined by one space. Unknown keys are dropped (counted in `removed_keys`).
///    A group with no known key is removed, along with one adjacent space.
/// 3. Any other bracket text is left alone.
///
/// The steps run in that order, so step 1 never touches step 2's output.
pub fn resolve_keys(md: &str, keys: &HashMap<String, String>) -> Resolved {
    let (unwrapped, unwrapped_links) = unwrap_wikilinks(md);
    let mut removed = 0;
    let chars: Vec<char> = unwrapped.chars().collect();
    let mut out = String::with_capacity(unwrapped.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(close) = chars[i + 1..].iter().position(|&c| c == ']' || c == '[') {
                let end = i + 1 + close;
                if chars[end] == ']' {
                    let inner: String = chars[i + 1..end].iter().collect();
                    if let Some(tokens) = key_tokens(&inner) {
                        let mut links: Vec<String> = Vec::new();
                        for token in tokens {
                            match keys.get(&token) {
                                Some(slug) => {
                                    let link = format!("[[{slug}]]");
                                    if !links.contains(&link) {
                                        links.push(link);
                                    }
                                }
                                None => removed += 1,
                            }
                        }
                        i = end + 1;
                        if links.is_empty() {
                            // Take one adjacent space with the group: the one before
                            // it, else the one after it when nothing precedes it on
                            // the line.
                            if out.ends_with(' ') {
                                out.pop();
                            } else if (out.is_empty() || out.ends_with('\n')) && chars.get(i) == Some(&' ') {
                                i += 1;
                            }
                        } else {
                            out.push_str(&links.join(" "));
                        }
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    Resolved { markdown: out, removed_keys: removed, unwrapped_links }
}

/// Unwrap every `[[inner]]` (single line) to `inner`, or to `[inner]` when
/// `inner` is a key group, so it is then resolved as a key; returns the text
/// and how many ordinary links were unwrapped.
fn unwrap_wikilinks(md: &str) -> (String, usize) {
    let chars: Vec<char> = md.chars().collect();
    let mut out = String::with_capacity(md.len());
    let mut count = 0;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' && chars.get(i + 1) == Some(&'[') {
            let mut j = i + 2;
            while j + 1 < chars.len() && chars[j] != '\n' && !(chars[j] == ']' && chars[j + 1] == ']') {
                j += 1;
            }
            if j + 1 < chars.len() && chars[j] == ']' && chars[j + 1] == ']' {
                let inner: String = chars[i + 2..j].iter().collect();
                if key_tokens(&inner).is_some() {
                    out.push('[');
                    out.push_str(&inner);
                    out.push(']');
                } else {
                    out.push_str(&inner);
                    count += 1;
                }
                i = j + 2;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, count)
}

/// `Some(["S1", "S2"])` when `inner` is one or more `S<digits>` tokens separated
/// by commas, semicolons or whitespace, normalised to upper case; `None` for
/// anything else.
fn key_tokens(inner: &str) -> Option<Vec<String>> {
    let tokens: Vec<&str> = inner.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return None;
    }
    let mut keys = Vec::with_capacity(tokens.len());
    for t in tokens {
        let mut chars = t.chars();
        let first = chars.next()?;
        let digits = chars.as_str();
        if !first.eq_ignore_ascii_case(&'s') || digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        keys.push(format!("S{digits}"));
    }
    Some(keys)
}

/// The closing "Sources" section: one `[[slug]]` bullet per source.
pub fn sources_markdown(slugs: &[String]) -> String {
    let mut s = String::from("## Sources\n\n");
    for slug in slugs {
        s.push_str(&format!("- [[{slug}]]\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::Actor;
    use crate::test_support::{note, temp_db};

    const PLAIN: &str = "1a2b3c4d-0000-4000-8000-000000000001";

    fn keys(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_titled_note_gets_its_note_slug() {
        assert_eq!(citation_slug("EV chargers", PLAIN), crate::db::note_slug("EV chargers", PLAIN));
        assert_eq!(citation_slug("EV chargers", PLAIN), "ev-chargers-1a2b3c4d");
    }

    #[test]
    fn a_title_that_slugifies_to_nothing_gets_untitled() {
        for title in ["", "🔋🔋", "!!!"] {
            assert_eq!(citation_slug(title, PLAIN), "untitled-1a2b3c4d", "title {title:?}");
        }
        // And the link it makes is a real edge to that note, by id.
        let db = temp_db();
        let acct = "gmail:t@example.com";
        let target = note(acct, PLAIN).title("🔋🔋").build();
        db.insert_local_new(&target, &Actor::test()).unwrap();
        let slug = citation_slug(&target.title, PLAIN);
        let src_uuid = "9f9f9f9f-0000-4000-8000-000000000002";
        note(acct, src_uuid).body(&format!("Prices fell [[{slug}]].")).insert(&db);
        let back = db.backlinks(acct, PLAIN).unwrap();
        assert_eq!(back.len(), 1, "the citation must become a backlink");
        assert_eq!(back[0].uuid, src_uuid);
    }

    #[test]
    fn a_known_key_becomes_a_wikilink() {
        let r = resolve_keys("Prices fell [S1].", &keys(&[("S1", "ev-1a2b3c4d")]));
        assert_eq!(r.markdown, "Prices fell [[ev-1a2b3c4d]].");
        assert_eq!(r.removed_keys, 0);
    }

    #[test]
    fn combined_and_unknown_keys() {
        let k = keys(&[("S1", "a"), ("S2", "b")]);
        let r = resolve_keys("[S1, S2]", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys), ("[[a]] [[b]]", 0));
        let r = resolve_keys("[S1,S2]", &k);
        assert_eq!(r.markdown, "[[a]] [[b]]");
        let r = resolve_keys("[S1][S2]", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys), ("[[a]][[b]]", 0));
        let r = resolve_keys("[S99]", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys), ("", 1));
        let r = resolve_keys("[s1]", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys), ("[[a]]", 0));
        let r = resolve_keys("x [S1, S99] y", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys), ("x [[a]] y", 1));
        // A fully unknown group takes one adjacent space with it, never both.
        let r = resolve_keys("Prices fell [S99] today.", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys), ("Prices fell today.", 1));
        let r = resolve_keys("Prices fell.[S99] Next.", &k);
        assert_eq!(r.markdown, "Prices fell. Next.");
        let r = resolve_keys("[S99] Starts here.", &k);
        assert_eq!(r.markdown, "Starts here.");
        // A repeated key is linked once.
        assert_eq!(resolve_keys("[S1, s1]", &k).markdown, "[[a]]");
    }

    #[test]
    fn other_bracket_text_is_left_alone() {
        let k = keys(&[("S1", "a")]);
        for text in ["see [the docs] and [1]", "array[S] and [S1x]", "[ ]", "[]", "open [S1 and more", "a [b [S1]"] {
            let r = resolve_keys(text, &k);
            if text == "a [b [S1]" {
                assert_eq!(r.markdown, "a [b [[a]]");
            } else {
                assert_eq!(r.markdown, text, "text {text:?}");
                assert_eq!(r.removed_keys, 0);
            }
        }
    }

    #[test]
    fn wikilinks_the_ai_wrote_are_unwrapped_and_counted_apart_from_keys() {
        let k = keys(&[("S1", "a")]);
        let r = resolve_keys("see [[Some Note]]", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys, r.unwrapped_links), ("see Some Note", 0, 1));
        // Our own replacements are not affected.
        let r = resolve_keys("see [[Some Note]] and [S1] [S9]", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys, r.unwrapped_links), ("see Some Note and [[a]]", 1, 1));
    }

    #[test]
    fn a_key_the_ai_wrapped_as_a_wikilink_is_resolved_as_a_key() {
        let k = keys(&[("S1", "a"), ("S2", "b")]);
        let r = resolve_keys("Fact [[S1]].", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys, r.unwrapped_links), ("Fact [[a]].", 0, 0));
        let r = resolve_keys("Fact [[s2]] and [[S1, S2]].", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys, r.unwrapped_links), ("Fact [[b]] and [[a]] [[b]].", 0, 0));
        let r = resolve_keys("Made up [[S99]] here.", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys, r.unwrapped_links), ("Made up here.", 1, 0));
        // Not a key: an ordinary AI-written link, unwrapped.
        let r = resolve_keys("[[S1 notes]]", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys, r.unwrapped_links), ("S1 notes", 0, 1));
    }

    #[test]
    fn a_semicolon_separates_keys() {
        let k = keys(&[("S1", "a"), ("S2", "b")]);
        let r = resolve_keys("[S1; S2]", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys), ("[[a]] [[b]]", 0));
        let r = resolve_keys("[S1;S2; S9]", &k);
        assert_eq!((r.markdown.as_str(), r.removed_keys), ("[[a]] [[b]]", 1));
        // A semicolon alone is still not a key group.
        assert_eq!(resolve_keys("[;]", &k).markdown, "[;]");
    }

    #[test]
    fn thai_titles_slug_and_replace() {
        let slug = citation_slug("ราคาเครื่องชาร์จ", PLAIN);
        assert!(slug.starts_with("ราคาเครื่องชาร์จ-") && slug.ends_with("1a2b3c4d"), "{slug}");
        let r = resolve_keys("ราคาลดลง[S1]", &keys(&[("S1", &slug)]));
        assert_eq!(r.markdown, format!("ราคาลดลง[[{slug}]]"));
        // Emoji and Thai around a stripped key do not split a character.
        let r = resolve_keys("🔋ราคา [S9] ลดลง🔋", &HashMap::new());
        assert_eq!((r.markdown.as_str(), r.removed_keys), ("🔋ราคา ลดลง🔋", 1));
    }

    #[test]
    fn sources_lists_every_slug() {
        assert_eq!(sources_markdown(&["a".into(), "b".into()]), "## Sources\n\n- [[a]]\n- [[b]]\n");
    }
}
