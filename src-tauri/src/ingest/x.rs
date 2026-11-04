//! X post link → text, author, duration and a media reference (spec §5.2). One
//! request: the syndication JSON X's own embeds read, `GET …/tweet-result?id=<id>
//! &token=a`, answered 200 for both of the owner's links on 2026-10-18 with no
//! cookie and the default UA.
//!
//! UNOFFICIAL, like the other platform files: when X moves a key the post
//! degrades to "X returned no post data" — the ingest completes either way.
//! There is no X transcript, so `transcript` stays `None`; a post with no
//! `video` media (text, photos, GIFs) keeps its text and has no `media`. A
//! post with a `video` but no usable MP4 (HLS only, or http only) has no
//! `media` either, and says so with `has_video`. Only the post's OWN media is
//! read: the video of a post it quotes (`quoted_tweet`) is not followed.

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::net::{read_capped, send_guarded, GuardedRequest, CANCELLED};
use super::social::{MediaRef, PlatformMeta};
use super::web::MAX_BODY_BYTES;
use super::{FetchPolicy, SourceKind};

// ── X constants ────────────────────────────────────────────────────────────
// Everything that names X's response shape lives HERE and nowhere else. When
// parsing stops finding a post, re-capture the JSON and change this block.
const SYNDICATION: &str = "https://cdn.syndication.twimg.com/tweet-result";
/// Measured 2026-10-18: the constant `a` is accepted. If X starts refusing it,
/// the fallback is to compute the token the way X's embed script does (spec
/// §5.2) — deliberately not built until that happens.
const TOKEN: &str = "a";
const WEB_ORIGIN: &str = "https://x.com";
/// Spec D11: the final media URL's host must be one of these (or a subdomain).
pub const MEDIA_HOSTS: &[&str] = &["twimg.com"];
const NO_DATA: &str = "X returned no post data";
// ───────────────────────────────────────────────────────────────────────────

fn canonical_url(id: &str, screen_name: &str) -> Option<String> {
    let id_ok = !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
    (id_ok && is_handle(screen_name)).then(|| format!("{WEB_ORIGIN}/{screen_name}/status/{id}"))
}

fn is_handle(s: &str) -> bool {
    (1..=15).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn nonblank(s: &str) -> Option<String> {
    Some(s.trim()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// X appends a `https://t.co/…` link for the attached media (and for a link
/// the post quotes) to `text`; it says nothing, so trailing ones are dropped.
/// Links inside the text stay.
fn strip_trailing_links(text: &str) -> String {
    let mut t = text.trim_end();
    // `rsplit` yields the last whitespace-separated token, so a multi-byte
    // space (U+00A0, U+3000) cannot split a character.
    while let Some(last) = t.rsplit(char::is_whitespace).next().filter(|l| l.starts_with("https://t.co/")) {
        t = t[..t.len() - last.len()].trim_end();
    }
    t.to_string()
}

/// A `text` this long may be the cut form of a long post: the one measured
/// long post was cut at 277 characters (`display_text_range`).
const MAYBE_CUT_CHARS: usize = 270;

/// The post's text: the long-post body when X sends one, else `text`. The flag
/// says the text may be a truncation — `text` (before the `t.co` strip) is
/// long and no long-post body was read to replace it.
fn post_text(v: &Value) -> (Option<String>, bool) {
    let long = v["note_tweet"]["note_tweet_results"]["result"]["text"].as_str().and_then(nonblank);
    let Some(short) = v["text"].as_str() else { return (None, false) };
    let maybe_cut = long.is_none() && short.chars().count() >= MAYBE_CUT_CHARS;
    let text = long.unwrap_or_else(|| short.to_string());
    (nonblank(&strip_trailing_links(&text)), maybe_cut)
}

fn author(v: &Value) -> Option<String> {
    let name = v["user"]["name"].as_str().and_then(nonblank);
    let handle = v["user"]["screen_name"].as_str().filter(|h| is_handle(h));
    match (name, handle) {
        (Some(n), Some(h)) => Some(format!("{n} (@{h})")),
        (Some(n), None) => Some(n),
        (None, Some(h)) => Some(format!("@{h}")),
        (None, None) => None,
    }
}

/// `entities.urls[]` as `(t.co link, the link it stands for)` — what X shows
/// in place of each `t.co` in the text. Only https targets; never media.
fn expanded_links(v: &Value) -> Vec<(String, String)> {
    let Some(list) = v["entities"]["urls"].as_array() else { return Vec::new() };
    list.iter()
        .filter_map(|u| Some((u["url"].as_str()?.to_string(), u["expanded_url"].as_str()?.to_string())))
        .filter(|(short, long)| short.starts_with("https://t.co/") && (long.starts_with("https://") || long.starts_with("http://")))
        .collect()
}

/// The lowest-bitrate `video/mp4` variant: the audio is the same in every one
/// and it is the smallest download. A variant with no bitrate ranks last.
fn lowest_mp4(video: &Value) -> Option<String> {
    video["video_info"]["variants"]
        .as_array()?
        .iter()
        .filter(|x| x["content_type"] == "video/mp4")
        .filter_map(|x| Some((x["bitrate"].as_u64().unwrap_or(u64::MAX), x["url"].as_str()?)))
        .filter(|(_, url)| url.starts_with("https://"))
        .min_by_key(|(bitrate, _)| *bitrate)
        .map(|(_, url)| url.to_string())
}

/// Syndication JSON → what the post says, no network. `canonical_url` is the
/// fetcher's to set (it knows the id) — see `fetch_meta_with`.
pub fn parse_tweet(json: &str) -> PlatformMeta {
    parse(json).0
}

/// `parse_tweet`, plus the author's handle when it has the shape X handles
/// have — the fetcher needs it for the canonical URL.
fn parse(json: &str) -> (PlatformMeta, Option<String>) {
    let mut meta = PlatformMeta::new(SourceKind::X);
    let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    // A removed post answers `{}` or a tombstone; a real one always has `text`.
    if !v["text"].is_string() {
        meta.unavailable = Some(NO_DATA.into());
        return (meta, None);
    }
    let (caption, maybe_cut) = post_text(&v);
    meta.caption_maybe_cut = caption.is_some() && maybe_cut;
    meta.title = caption.as_deref().and_then(|c| c.lines().map(str::trim).find(|l| !l.is_empty())).map(str::to_string);
    meta.caption = caption;
    meta.author = author(&v);
    meta.expanded_links = expanded_links(&v);
    // Only a real `video` has audio worth transcribing: `animated_gif` has none
    // and a photo is no media.
    let video = v["mediaDetails"].as_array().and_then(|m| m.iter().find(|m| m["type"] == "video"));
    if let Some(video) = video {
        meta.has_video = true;
        meta.duration_secs = video["video_info"]["duration_millis"].as_f64().map(|ms| ms / 1000.0).filter(|d| *d > 0.0);
        if let Some(url) = lowest_mp4(video) {
            meta.media = Some(MediaRef { url, headers: Vec::new(), cookies: Vec::new(), allowed_host_suffixes: MEDIA_HOSTS });
        }
    }
    let handle = v["user"]["screen_name"].as_str().filter(|h| is_handle(h)).map(str::to_string);
    (meta, handle)
}

/// Never fails the ingest (URL-ingest Decision 2): every outcome is a
/// `PlatformMeta`, and `unavailable` says why there is no post.
///
/// Fetched by id alone (D12): the link as shared never reaches X.
pub async fn fetch_meta(id: &str, policy: FetchPolicy, cancel: &CancellationToken) -> PlatformMeta {
    fetch_meta_with(SYNDICATION, id, policy, cancel).await
}

/// `endpoint` is where the JSON is fetched from: production passes
/// `SYNDICATION`, tests pass their mockito server. The canonical URL is always
/// built on `WEB_ORIGIN`.
async fn fetch_meta_with(endpoint: &str, id: &str, policy: FetchPolicy, cancel: &CancellationToken) -> PlatformMeta {
    let unavailable = |reason: String| PlatformMeta { unavailable: Some(reason), ..PlatformMeta::new(SourceKind::X) };
    // The id is interpolated into the request: digits only (D12).
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return unavailable("not an X post link".into());
    }
    let url = format!("{endpoint}?id={id}&token={TOKEN}");
    let resp = match send_guarded(GuardedRequest::get(&url), policy, cancel).await {
        Ok(r) => r,
        Err(e) if e == CANCELLED => return unavailable(e),
        Err(e) => return unavailable(format!("X could not be reached ({e})")),
    };
    if !resp.status().is_success() {
        return unavailable(format!("X answered HTTP {}", resp.status().as_u16()));
    }
    let body = match read_capped(resp, MAX_BODY_BYTES, cancel).await {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e == CANCELLED => return unavailable(e),
        Err(e) => return unavailable(format!("X could not be read ({e})")),
    };
    let (mut meta, handle) = parse(&body);
    meta.canonical_url = handle.and_then(|h| canonical_url(id, &h));
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIDEO: &str = include_str!("fixtures/x_tweet_video.json");
    const GIFS: &str = include_str!("fixtures/x_tweet_gif_reply.json");
    const ID: &str = "2111113436314648758";

    #[test]
    fn expanded_links_pair_each_tco_with_its_target_and_skip_odd_entries() {
        let m = parse_tweet(r#"{"text":"read https://t.co/aaa now","entities":{"urls":[
            {"url":"https://t.co/aaa","expanded_url":"https://example.com/post"},
            {"url":"https://t.co/bbb","expanded_url":"javascript:alert(1)"},
            {"url":"http://not-tco.example/x","expanded_url":"https://example.com/y"}]}}"#);
        assert_eq!(m.expanded_links, vec![("https://t.co/aaa".to_string(), "https://example.com/post".to_string())]);
        assert!(parse_tweet(r#"{"text":"no entities"}"#).expanded_links.is_empty());
    }

    #[test]
    fn a_video_tweet_yields_text_author_and_the_lowest_bitrate_mp4() {
        let meta = parse_tweet(VIDEO);
        assert_eq!(meta.platform, SourceKind::X);
        assert_eq!(meta.unavailable, None);
        assert_eq!(meta.author.as_deref(), Some("kiosa (@thegreatest_sv)"));
        assert_eq!(meta.title.as_deref(), Some("I truly don't understand why nobody is using this setup"));
        let caption = meta.caption.as_deref().unwrap();
        assert!(caption.starts_with("I truly don't understand"), "{caption}");
        assert!(caption.ends_with("a desk, a memory and a"), "the trailing t.co link for the attached media is dropped: {caption}");
        assert!(!caption.contains("t.co"), "{caption}");
        assert_eq!(meta.duration_secs, Some(30.0));
        assert!(meta.has_video);
        let media = meta.media.expect("a video variant");
        assert_eq!(media.url, "https://video.twimg.com/amplify_video/2111032402583646208/vid/avc1/320x400/z_lwNqdcuK_lasDo.mp4");
        assert_eq!(media.allowed_host_suffixes, MEDIA_HOSTS);
        assert!(media.headers.is_empty() && media.cookies.is_empty());
        assert_eq!(meta.transcript, None);
        assert!(meta.caption_maybe_cut, "the capture's 301-char text is the cut form of a long post");
    }

    #[test]
    fn a_long_text_without_the_long_post_body_may_be_cut() {
        let text = "ก".repeat(270);
        let post = |text: &str, note: &str| parse_tweet(&format!(r#"{{"text":"{text}"{note}}}"#));
        // 270 characters is 810 bytes: the count is characters, and 270 is in.
        assert!(post(&text, "").caption_maybe_cut);
        assert!(!post(&text[..text.len() - 3], "").caption_maybe_cut, "269 characters");
        // The link X appends counts: it is part of what `text` was cut to.
        assert!(post(&format!("{} https://t.co/abc", "a".repeat(260)), "").caption_maybe_cut);
        // A short post and the short reply fixture are complete.
        assert!(!post("short", "").caption_maybe_cut);
        assert!(!parse_tweet(GIFS).caption_maybe_cut);
        // A readable long-post body replaces the cut text: nothing to warn about.
        let note = r#","note_tweet":{"note_tweet_results":{"result":{"text":"the whole post"}}}"#;
        assert!(!post(&text, note).caption_maybe_cut);
        // A note_tweet without text (both captures) does not.
        assert!(post(&text, r#","note_tweet":{"id":"x"}"#).caption_maybe_cut);
        // No caption left, nothing to mark.
        assert!(!post(&format!("https://t.co/{}", "a".repeat(260)), "").caption_maybe_cut);
    }

    #[test]
    fn a_gif_tweet_has_no_media_to_transcribe() {
        let meta = parse_tweet(GIFS);
        assert_eq!(meta.unavailable, None);
        assert_eq!(meta.author.as_deref(), Some("DeepSeek (@deepseek_ai)"));
        assert_eq!(meta.title.as_deref(), Some("🎮 Build complete projects from scratch"));
        assert!(meta.caption.unwrap().ends_with("2/4"));
        assert!(meta.media.is_none(), "animated_gif has no audio");
        assert!(!meta.has_video, "a gif is not a video");
        assert_eq!(meta.duration_secs, None);
        // A photo is no media either; a gif listed BEFORE a video does not hide it.
        let mixed = r#"{"text":"t","mediaDetails":[{"type":"photo"},{"type":"animated_gif","video_info":{"variants":[{"bitrate":0,"content_type":"video/mp4","url":"https://video.twimg.com/g.mp4"}]}},{"type":"video","video_info":{"duration_millis":1500,"variants":[{"bitrate":9,"content_type":"video/mp4","url":"https://video.twimg.com/v.mp4"}]}}]}"#;
        let meta = parse_tweet(mixed);
        assert_eq!(meta.media.unwrap().url, "https://video.twimg.com/v.mp4");
        assert_eq!(meta.duration_secs, Some(1.5));
    }

    #[test]
    fn note_tweet_text_wins_over_the_truncated_text() {
        // UNMEASURED shape: both captures carry `note_tweet` as `{"id": …}` alone,
        // so the path to the full text is the one X's own client uses, not one
        // seen on the wire here.
        let long = r#"{"text":"The start of a long post… https://t.co/abc","user":{"name":"A","screen_name":"a_b"},
            "note_tweet":{"id":"x","note_tweet_results":{"result":{"text":"The start of a long post, and all the rest of it.\n\nSecond paragraph."}}}}"#;
        let meta = parse_tweet(long);
        assert_eq!(meta.caption.as_deref(), Some("The start of a long post, and all the rest of it.\n\nSecond paragraph."));
        assert_eq!(meta.title.as_deref(), Some("The start of a long post, and all the rest of it."));
        // A note_tweet with no text in it (what both captures have) falls back to `text`.
        let meta = parse_tweet(r#"{"text":"short","note_tweet":{"id":"x"}}"#);
        assert_eq!(meta.caption.as_deref(), Some("short"));
        let meta = parse_tweet(r#"{"text":"short","note_tweet":{"note_tweet_results":{"result":{"text":"  "}}}}"#);
        assert_eq!(meta.caption.as_deref(), Some("short"), "a blank note_tweet text is not text");
    }

    #[test]
    fn x_tolerates_a_body_that_is_not_a_tweet() {
        for body in ["{}", "", "not json", "[]", "null", r#"{"text":5}"#, r#"{"tombstone":{"text":"This post is unavailable"}}"#, "<html>login</html>"] {
            let meta = parse_tweet(body);
            assert_eq!(meta.unavailable.as_deref(), Some("X returned no post data"), "{body}");
            assert!(meta.media.is_none() && meta.caption.is_none() && meta.author.is_none() && meta.title.is_none(), "{body}");
        }
    }

    #[test]
    fn only_a_playable_https_mp4_variant_is_a_candidate() {
        let media = |variants: &str| {
            parse_tweet(&format!(r#"{{"text":"t","mediaDetails":[{{"type":"video","video_info":{{"variants":[{variants}]}}}}]}}"#)).media
        };
        // HLS only, a non-https URL, no URL, no variants: no media — but the
        // post still HAS a video, so it is not reported as having no audio.
        for variants in [
            r#"{"content_type":"application/x-mpegURL","url":"https://video.twimg.com/a.m3u8"}"#,
            r#"{"bitrate":1,"content_type":"video/mp4","url":"http://video.twimg.com/a.mp4"}"#,
        ] {
            let meta = parse_tweet(&format!(r#"{{"text":"t","mediaDetails":[{{"type":"video","video_info":{{"variants":[{variants}]}}}}]}}"#));
            assert!(meta.media.is_none() && meta.has_video, "{variants}");
        }
        assert!(media(r#"{"content_type":"application/x-mpegURL","url":"https://video.twimg.com/a.m3u8"}"#).is_none());
        assert!(media(r#"{"bitrate":1,"content_type":"video/mp4","url":"http://video.twimg.com/a.mp4"}"#).is_none());
        assert!(media(r#"{"bitrate":1,"content_type":"video/mp4"}"#).is_none());
        assert!(media("").is_none());
        // A variant without a bitrate ranks after one with it.
        let m = media(r#"{"content_type":"video/mp4","url":"https://video.twimg.com/nobr.mp4"},{"bitrate":5,"content_type":"video/mp4","url":"https://video.twimg.com/br.mp4"}"#);
        assert_eq!(m.unwrap().url, "https://video.twimg.com/br.mp4");
        // A missing or zero duration is absent, not 0.
        for d in ["", r#""duration_millis":0,"#, r#""duration_millis":"x","#] {
            let j = format!(r#"{{"text":"t","mediaDetails":[{{"type":"video","video_info":{{{d}"variants":[]}}}}]}}"#);
            assert_eq!(parse_tweet(&j).duration_secs, None, "{d}");
        }
    }

    #[test]
    fn the_handle_is_interpolated_only_in_the_shape_x_handles_have() {
        let author = |user: &str| parse_tweet(&format!(r#"{{"text":"t","user":{user}}}"#)).author;
        assert_eq!(author(r#"{"name":"N","screen_name":"ok_1"}"#).as_deref(), Some("N (@ok_1)"));
        assert_eq!(author(r#"{"name":"N","screen_name":"a b"}"#).as_deref(), Some("N"));
        assert_eq!(author(r#"{"name":"N","screen_name":"sixteen_chars_xx"}"#).as_deref(), Some("N"));
        assert_eq!(author(r#"{"name":"  ","screen_name":"ok"}"#).as_deref(), Some("@ok"));
        assert_eq!(author(r#"{}"#), None);
        // The caption's trailing links are stripped repeatedly, interior ones stay.
        let c = parse_tweet(r#"{"text":"see https://t.co/aaa and more https://t.co/bbb https://t.co/ccc"}"#).caption;
        assert_eq!(c.as_deref(), Some("see https://t.co/aaa and more"));
        // A multi-byte space before the link (nbsp, ideographic space) is a boundary too.
        let c = parse_tweet("{\"text\":\"สวัสดี\u{a0}https://t.co/aaa\u{3000}https://t.co/bbb\"}").caption;
        assert_eq!(c.as_deref(), Some("สวัสดี"));
        // A post that is only the link has no text at all.
        let m = parse_tweet(r#"{"text":"https://t.co/aaa"}"#);
        assert_eq!((m.unavailable, m.caption, m.title), (None, None, None));
    }

    #[test]
    fn the_canonical_url_needs_a_numeric_id_and_a_real_handle() {
        assert_eq!(canonical_url(ID, "thegreatest_sv").as_deref(), Some("https://x.com/thegreatest_sv/status/2111113436314648758"));
        for (id, h) in [("", "a"), ("12a", "a"), ("1/2", "a"), (ID, ""), (ID, "a/b"), (ID, "a b"), (ID, "sixteen_chars_xx"), (ID, "a?b")] {
            assert_eq!(canonical_url(id, h), None, "{id:?} {h:?}");
        }
    }

    #[tokio::test]
    async fn fetch_meta_asks_for_the_id_and_the_constant_token_and_nothing_else() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("GET", "/tweet-result")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("id".into(), ID.into()),
                mockito::Matcher::UrlEncoded("token".into(), "a".into()),
            ]))
            .with_status(200)
            .with_body(VIDEO)
            .create_async()
            .await;
        let endpoint = format!("{}/tweet-result", server.url());
        let meta = fetch_meta_with(&endpoint, ID, FetchPolicy { allow_loopback: true }, &CancellationToken::new()).await;
        m.assert_async().await;
        assert_eq!(meta.unavailable, None);
        assert_eq!(meta.canonical_url.as_deref(), Some("https://x.com/thegreatest_sv/status/2111113436314648758"));
        assert!(meta.media.is_some() && meta.caption.is_some());
    }

    #[tokio::test]
    async fn a_missing_post_a_failing_endpoint_and_a_bad_id_say_why() {
        let mut server = mockito::Server::new_async().await;
        let _empty = server.mock("GET", "/tweet-result").match_query(mockito::Matcher::UrlEncoded("id".into(), "1".into())).with_status(200).with_body("{}").create_async().await;
        let _gone = server.mock("GET", "/tweet-result").match_query(mockito::Matcher::UrlEncoded("id".into(), "2".into())).with_status(404).create_async().await;
        let endpoint = format!("{}/tweet-result", server.url());
        let policy = FetchPolicy { allow_loopback: true };
        let cancel = CancellationToken::new();

        let meta = fetch_meta_with(&endpoint, "1", policy, &cancel).await;
        assert_eq!(meta.unavailable.as_deref(), Some("X returned no post data"));
        let meta = fetch_meta_with(&endpoint, "2", policy, &cancel).await;
        assert_eq!(meta.unavailable.as_deref(), Some("X answered HTTP 404"));
        // An id that is not all digits is refused before any request is built.
        let meta = fetch_meta_with(&endpoint, "1&token=b", policy, &cancel).await;
        assert_eq!(meta.unavailable.as_deref(), Some("not an X post link"));
        // The default policy refuses the loopback mock: the guard is in the path.
        let meta = fetch_meta_with(&endpoint, "1", FetchPolicy::default(), &cancel).await;
        assert!(meta.unavailable.unwrap().starts_with("X could not be reached"));
        let c = CancellationToken::new();
        c.cancel();
        let meta = fetch_meta_with(&endpoint, "1", policy, &c).await;
        assert_eq!(meta.unavailable.as_deref(), Some("cancelled"));
    }
}
