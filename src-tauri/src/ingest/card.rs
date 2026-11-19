//! A social post Jodd could read only the caption of becomes a CARD, not an AI
//! summary (2026-11-19).
//!
//! Measured on two Instagram reels with no transcript: the caption was one line,
//! and Key points' "1–7 points" prompt turned it into four sections — the fetch
//! status said three times, guesses at what the reel "likely" covers, a
//! Meta-points section, and tags that describe the fetch (`incomplete-source`,
//! `instagram-reel`) rather than the subject. There is nothing in one line for
//! a model to distill, so this writes what Jodd already knows instead: the
//! caption as it was posted, who posted it, and the post's own hashtags as tags.
//! The Sources list still says why there is no transcript.

use super::social::{parse_provenance, platform_label, transcript_after_header};
use super::{FetchedSource, SourceKind};
use crate::llm::provider::ExtractEnvelope;

/// A caption longer than this is a post with something to distill, and goes
/// to the AI as before. The same bar `quality::MIN_CHARS` sets for a web page
/// being "too thin to be a source".
pub const CARD_MAX_CHARS: usize = 400;
/// The Extract prompt's title limit.
const TITLE_MAX_CHARS: usize = 80;
/// The Extract prompt's tag limit.
const MAX_TAGS: usize = 8;

/// The card for `s`, or `None` when `s` is not a caption-only social post: it
/// must carry D14's header, no transcript, no comments or pasted text, and a
/// caption of at most `CARD_MAX_CHARS`.
pub fn caption_card(s: &FetchedSource) -> Option<ExtractEnvelope> {
    if !s.kind.is_social() || crate::ingest::discuss::without_discussion(&s.text).len() != s.text.len() {
        return None;
    }
    let p = parse_provenance(&s.text)?;
    if !transcript_after_header(&s.text)?.is_empty() {
        return None;
    }
    let caption = p.caption.as_deref().map(str::trim).filter(|c| !c.is_empty())?;
    if caption.chars().count() > CARD_MAX_CHARS {
        return None;
    }
    let platform = platform_label(s.kind);
    let title = Some(card_title(caption)).filter(|t| !t.is_empty()).unwrap_or_else(|| match p.author.as_deref() {
        Some(a) => format!("{} on {platform}", display_name(a).unwrap_or_else(|| a.to_string())),
        None => format!("{platform} post"),
    });
    let mut md = String::new();
    md.push_str(&author_line(s.kind, p.author.as_deref()));
    md.push_str("\n\n> ");
    md.push_str(&escape_markdown(caption));
    if p.caption_maybe_cut {
        md.push_str(" …");
    }
    md.push('\n');
    Some(ExtractEnvelope { title: Some(title), lessons_markdown: md, meta_lessons_markdown: None, tags: hashtags(caption), confidence: None })
}

/// Whether `c` continues a hashtag. Thai is listed whole because its tone
/// marks (`้`) are not alphabetic, and `#เคสนี้ดี` is one tag.
fn tag_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || ('\u{0E00}'..='\u{0E7F}').contains(&c) || ('\u{0300}'..='\u{036F}').contains(&c)
}

/// `(start, end)` byte ranges of each `#word` in `text`, `#` included. A `#`
/// glued to the word before it counts too: Instagram captions put tags right
/// after an emoji (`🏃‍♀️#เคสนี้ดี`).
fn hashtag_spans(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c != '#' {
            continue;
        }
        let mut end = i + 1;
        while let Some(&(j, d)) = it.peek() {
            if !tag_char(d) {
                break;
            }
            end = j + d.len_utf8();
            it.next();
        }
        if end > i + 1 {
            out.push((i, end));
        }
    }
    out
}

/// The post's own hashtags, lowercased, first spelling kept, at most `MAX_TAGS`.
fn hashtags(caption: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for (a, b) in hashtag_spans(caption) {
        let tag = caption[a + 1..b].to_lowercase();
        if tags.len() < MAX_TAGS && !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

/// The caption without its hashtags, one line, cut to the title limit.
fn card_title(caption: &str) -> String {
    let mut rest = String::new();
    let mut at = 0;
    for (a, b) in hashtag_spans(caption) {
        rest.push_str(&caption[at..a]);
        rest.push(' ');
        at = b;
    }
    rest.push_str(&caption[at..]);
    let line = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TITLE_MAX_CHARS {
        return line;
    }
    let cut: String = line.chars().take(TITLE_MAX_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The `@handle` inside an author string, in the shape every platform's
/// handles have — the author comes from stored text a Re-extract reads back,
/// so only that shape is ever put into a URL.
fn handle(author: &str) -> Option<&str> {
    let at = author.find('@')?;
    let rest = &author[at + 1..];
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_')).unwrap_or(rest.len());
    let h = &rest[..end];
    (!h.is_empty() && h.len() <= 30).then_some(h)
}

/// The author without its handle: `Oil oill (@oill_oil)` → `Oil oill`,
/// `@tonsr_ (Lazy_Productive)` → `Lazy_Productive`.
fn display_name(author: &str) -> Option<String> {
    let without = match handle(author) {
        Some(h) => author.replacen(&format!("@{h}"), "", 1),
        None => author.to_string(),
    };
    let name = without.replace(['(', ')'], " ").split_whitespace().collect::<Vec<_>>().join(" ");
    (!name.is_empty()).then_some(name)
}

fn profile_url(kind: SourceKind, handle: &str) -> Option<String> {
    match kind {
        SourceKind::Instagram => Some(format!("https://www.instagram.com/{handle}/")),
        SourceKind::TikTok => Some(format!("https://www.tiktok.com/@{handle}")),
        SourceKind::X => Some(format!("https://x.com/{handle}")),
        _ => None,
    }
}

/// `Instagram · **Oil oill** · [@oill_oil](https://www.instagram.com/oill_oil/)`.
fn author_line(kind: SourceKind, author: Option<&str>) -> String {
    let mut parts = vec![platform_label(kind).to_string()];
    if let Some(author) = author {
        if let Some(name) = display_name(author) {
            parts.push(format!("**{}**", escape_markdown(&name)));
        }
        if let Some(h) = handle(author) {
            parts.push(match profile_url(kind, h) {
                Some(url) => format!("[@{}]({url})", escape_markdown(h)),
                None => format!("@{}", escape_markdown(h)),
            });
        }
    }
    parts.join(" · ")
}

/// Platform text is untrusted and is not Markdown: every character Markdown
/// gives a meaning to is escaped, so a caption reads as it was posted and
/// cannot open a heading, a link or raw HTML.
fn escape_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '<' | '>' | '#' | '!' | '|' | '~' | '-' | '+' | '.' | '{' | '}') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::social::{render_source_text, Provenance};
    use crate::ingest::FetchStatus;

    const REEL: &str = "https://www.instagram.com/reel/DfhPZMDBoSM/";
    const CAPTION: &str = "ขายของแบบบเร็ววววว 🏃‍♀️#เคสนี้ดี #creativemarketing #การตลาด #strava";

    fn reel(caption: &str, transcript: &str) -> FetchedSource {
        let p = Provenance {
            platform: "Instagram".into(),
            author: Some("Oil oill (@oill_oil)".into()),
            caption: Some(caption.into()),
            caption_maybe_cut: false,
            transcript_source: "none — speech-to-text is not set up".into(),
        };
        FetchedSource {
            url: REEL.into(),
            kind: SourceKind::Instagram,
            title: Some("ขายของแบบบเร็ววววว 🏃‍♀️#เคสนี้ดี".into()),
            text: render_source_text(&p, transcript),
            status: FetchStatus::Partial("caption only — set up Speech-to-text in Settings to include what is said".into()),
        }
    }

    #[test]
    fn a_caption_only_reel_becomes_a_card() {
        let card = caption_card(&reel(CAPTION, "")).expect("a card");
        assert_eq!(card.title.as_deref(), Some("ขายของแบบบเร็ววววว 🏃‍♀️"));
        assert_eq!(card.tags, vec!["เคสนี้ดี", "creativemarketing", "การตลาด", "strava"]);
        let md = &card.lessons_markdown;
        assert!(md.starts_with("Instagram · **Oil oill** · [@oill\\_oil](https://www.instagram.com/oill_oil/)"), "{md}");
        assert!(md.contains("> ขายของแบบบเร็ววววว 🏃‍♀️\\#เคสนี้ดี \\#creativemarketing"), "{md}");
        // Nothing about the fetch: the Sources list says that, once.
        assert!(!md.to_lowercase().contains("speech-to-text") && !md.contains("caption only"), "{md}");
        assert!(card.meta_lessons_markdown.is_none());
    }

    #[test]
    fn the_card_renders_as_the_caption_was_posted() {
        let card = caption_card(&reel("Comment and get rich with me #sidehustle", "")).unwrap();
        assert_eq!(card.title.as_deref(), Some("Comment and get rich with me"));
        assert_eq!(card.tags, vec!["sidehustle"]);
        let html = crate::llm::markdown::render_llm_markdown(&card.lessons_markdown);
        assert!(html.contains("<blockquote>") && html.contains("Comment and get rich with me #sidehustle"), "{html}");
        assert!(html.contains("href=\"https://www.instagram.com/oill_oil/\">@oill_oil</a>"), "{html}");
    }

    #[test]
    fn a_hostile_caption_stays_text() {
        let card = caption_card(&reel("# Heading [x](https://evil.example) <img src=x onerror=alert(1)> **b**", "")).unwrap();
        let html = crate::llm::markdown::render_llm_markdown(&card.lessons_markdown);
        assert!(!html.contains("<h1") && !html.contains("evil.example\"") && !html.contains("<img") && !html.contains("<strong>b"), "{html}");
        assert!(html.contains("[x](https://evil.example)"), "the caption's text survives: {html}");
    }

    #[test]
    fn a_transcript_a_long_caption_or_comments_go_to_the_ai() {
        assert!(caption_card(&reel(CAPTION, "what was said in the reel")).is_none());
        assert!(caption_card(&reel(&"word ".repeat(100), "")).is_none(), "over CARD_MAX_CHARS");
        let mut pasted = reel(CAPTION, "");
        pasted.text = crate::ingest::discuss::append_pasted(&pasted.text, "the steps are A, B and C");
        assert!(caption_card(&pasted).is_none());
        // A web page that merely opens with the header's shape is not a post.
        let web = FetchedSource { kind: SourceKind::Web, ..reel(CAPTION, "") };
        assert!(caption_card(&web).is_none());
    }

    /// Redo key points reads the reel back from its source note (#405): what
    /// comes back must still be a caption-only post, card for card.
    #[test]
    fn a_reel_read_back_from_its_source_note_is_still_the_same_card() {
        use crate::ingest::source_note::{content_hash, parse_source_note, render_source_note, SourceHeader, Validators};
        let src = reel(CAPTION, "");
        let header = SourceHeader { fetched_at: "2026-11-19 14:05".into(), sha256: content_hash(&src.text), with: vec![], validators: Validators::default() };
        let (_, body) = render_source_note(&src, &header);
        let (_, back) = parse_source_note(&body).expect("the source note parses");
        let (a, b) = (caption_card(&src).unwrap(), caption_card(&back).expect("still a card"));
        assert_eq!((a.title, a.lessons_markdown, a.tags), (b.title, b.lessons_markdown, b.tags));
    }

    #[test]
    fn a_caption_of_only_hashtags_is_titled_by_its_author() {
        let card = caption_card(&reel("#strava #running", "")).unwrap();
        assert_eq!(card.title.as_deref(), Some("Oil oill on Instagram"));
        assert_eq!(card.tags, vec!["strava", "running"]);
    }

    #[test]
    fn authors_from_every_platform() {
        assert_eq!(author_line(SourceKind::TikTok, Some("@tonsr_ (Lazy_Productive)")), "TikTok · **Lazy\\_Productive** · [@tonsr\\_](https://www.tiktok.com/@tonsr_)");
        assert_eq!(author_line(SourceKind::Facebook, Some("ติด Ai ไปกับลุงจืด")), "Facebook · **ติด Ai ไปกับลุงจืด**");
        assert_eq!(author_line(SourceKind::X, None), "X");
        // Only a handle's own shape reaches a URL.
        assert_eq!(handle("Evil (@a/../b)"), Some("a"));
    }

    #[test]
    fn a_long_caption_title_is_cut() {
        let caption = format!("{} #tag", "ก".repeat(120));
        let title = card_title(&caption);
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
        assert!(title.ends_with('…'));
    }
}
