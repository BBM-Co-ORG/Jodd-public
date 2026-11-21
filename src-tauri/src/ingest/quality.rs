//! A followed page has to earn its place (spec 2026-10-28 §5.5, D5).
//!
//! Applied only to sources Jodd followed, never to a link the user gave: a
//! page the user picked is read as today. A source that fails here becomes
//! `Failed("not worth reading: …")` — listed in the note's Sources, not
//! summarised — so the note never looks like it covered a wall or a list.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use super::{urls, FetchedSource, SourceKind};

/// How a gated source's status starts; `FetchStatus::label` adds "failed: ".
pub const NOT_WORTH_READING: &str = "not worth reading";

/// Below this many characters a web page is too thin to be a source.
const MIN_CHARS: usize = 400;
/// A wall's own text is short; a long article that mentions cookies is not one.
const WALL_MAX_CHARS: usize = 1_500;
/// Phrases a sign-in, paywall, consent or bot wall puts where the article was
/// (EN + TH). Lowercase.
const WALL_PHRASES: [&str; 20] = [
    "subscribe to continue",
    "subscribe to read",
    "to continue reading",
    "sign in to continue",
    "log in to continue",
    "create a free account",
    "this content is for subscribers",
    "subscribers only",
    "enable javascript",
    "please enable cookies",
    "verify you are human",
    "are you a robot",
    "access denied",
    "just a moment",
    "checking your browser",
    "สมัครสมาชิกเพื่ออ่าน",
    "เข้าสู่ระบบเพื่ออ่าน",
    "กรุณาเข้าสู่ระบบ",
    "เฉพาะสมาชิก",
    "อ่านต่อสำหรับสมาชิก",
];
/// A list page: at least this many lines, nearly all of them short.
const LIST_MIN_LINES: usize = 20;
const LIST_SHORT_LINE_CHARS: usize = 50;
const LIST_SHORT_SHARE: f32 = 0.85;
/// Near-duplicate: 12-character shingles, Jaccard at or above this.
const SHINGLE: usize = 12;
const DUPLICATE_JACCARD: f32 = 0.8;
/// Only the start of a long text is compared — enough to tell two copies apart.
const SHINGLE_TEXT_CHARS: usize = 30_000;

/// Why `source` is not worth reading, or `None`. `earlier`: the sources
/// before it in this ingest.
pub fn not_worth_reading(source: &FetchedSource, earlier: &[&FetchedSource]) -> Option<String> {
    if !source.is_usable() {
        return None;
    }
    let canonical = urls::canonical_url(&source.url);
    if earlier.iter().any(|e| urls::canonical_url(&e.url) == canonical) {
        return Some("the same page as an earlier source".into());
    }
    let mine = shingles(&source.text);
    if earlier.iter().filter(|e| e.is_usable()).any(|e| jaccard(&mine, &shingles(&e.text)) >= DUPLICATE_JACCARD) {
        return Some("nearly the same text as an earlier source".into());
    }
    // A video or post is judged by its platform reader, not by page length.
    if source.kind != SourceKind::Web {
        return None;
    }
    let chars = source.text.chars().count();
    let lower = source.text.to_lowercase();
    if chars < WALL_MAX_CHARS && WALL_PHRASES.iter().any(|p| lower.contains(p)) {
        return Some("the page shows a sign-in, paywall or cookie wall instead of the article".into());
    }
    if chars < MIN_CHARS {
        return Some(format!("too little text ({chars} characters)"));
    }
    let lines: Vec<&str> = source.text.lines().filter(|l| !l.trim().is_empty()).collect();
    let short = lines.iter().filter(|l| l.chars().count() < LIST_SHORT_LINE_CHARS).count();
    if lines.len() >= LIST_MIN_LINES && short as f32 / lines.len() as f32 >= LIST_SHORT_SHARE {
        return Some("a list of links, not an article".into());
    }
    None
}

fn shingles(text: &str) -> HashSet<u64> {
    let norm: Vec<char> = text.split_whitespace().flat_map(|w| w.chars().chain(std::iter::once(' '))).flat_map(char::to_lowercase).take(SHINGLE_TEXT_CHARS).collect();
    let mut out = HashSet::new();
    if norm.len() < SHINGLE {
        return out;
    }
    for w in norm.windows(SHINGLE) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        w.hash(&mut h);
        out.insert(h.finish());
    }
    out
}

fn jaccard(a: &HashSet<u64>, b: &HashSet<u64>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    inter as f32 / (a.len() + b.len() - inter) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::FetchStatus;

    fn web(url: &str, text: &str) -> FetchedSource {
        FetchedSource { url: url.into(), kind: SourceKind::Web, title: None, text: text.into(), status: FetchStatus::Ok }
    }

    fn article(seed: &str) -> String {
        // Distinct per seed, long enough to pass every length rule.
        (0..12).map(|i| format!("{} {i}", seed.repeat(30))).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_real_article_passes() {
        assert_eq!(not_worth_reading(&web("https://a.example/x", &article("ownership")), &[]), None);
    }

    #[test]
    fn thin_walled_and_list_pages_are_named() {
        let thin = not_worth_reading(&web("https://a.example/x", "Just a teaser."), &[]).unwrap();
        assert!(thin.starts_with("too little text"), "{thin}");
        let wall = not_worth_reading(&web("https://a.example/x", "Breaking story\nSubscribe to continue reading this article."), &[]).unwrap();
        assert!(wall.contains("wall"), "{wall}");
        let thai_wall = not_worth_reading(&web("https://a.example/x", "ข่าวเด่น\nกรุณาเข้าสู่ระบบ เพื่ออ่านต่อ"), &[]).unwrap();
        assert!(thai_wall.contains("wall"), "{thai_wall}");
        let list = (0..30).map(|i| format!("Story headline number {i}")).collect::<Vec<_>>().join("\n");
        assert_eq!(not_worth_reading(&web("https://a.example/x", &list), &[]).as_deref(), Some("a list of links, not an article"));
    }

    #[test]
    fn a_long_article_that_mentions_cookies_is_not_a_wall() {
        let text = format!("{}\nWe use cookies; please enable cookies for comments.", article("baking"));
        assert_eq!(not_worth_reading(&web("https://a.example/x", &text), &[]), None);
    }

    #[test]
    fn duplicates_by_url_and_by_text() {
        let first = web("https://a.example/post", &article("ownership"));
        let same_url = web("https://a.example/post#comments", &article("something else entirely"));
        // `canonical_url` leaves web links as they are, so a fragment differs —
        // but the copy below is caught by its text.
        let copy = web("https://mirror.example/copy", &format!("{}\nMirrored.", article("ownership")));
        assert_eq!(not_worth_reading(&web("https://a.example/post", "x".repeat(500).as_str()), &[&first]).as_deref(), Some("the same page as an earlier source"));
        assert_eq!(not_worth_reading(&copy, &[&first]).as_deref(), Some("nearly the same text as an earlier source"));
        assert_eq!(not_worth_reading(&same_url, &[&first]), None);
    }

    #[test]
    fn a_failed_source_or_a_video_is_never_judged_by_length() {
        let failed = FetchedSource::failed("https://a.example/x", SourceKind::Web, "HTTP 404");
        assert_eq!(not_worth_reading(&failed, &[]), None);
        let short_video = FetchedSource { kind: SourceKind::YouTube, ..web("https://youtu.be/abc", "Title\n\nShort description") };
        assert_eq!(not_worth_reading(&short_video, &[]), None);
    }
}
