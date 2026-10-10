//! Video → transcript (spec "ingest/youtube.rs"), through InnerTube's
//! `youtubei/v1/player` as the IOS client — the route the 2026-09-15 spike
//! measured working in plain HTTP, with correctly spaced Thai. UNOFFICIAL:
//! the owner accepted that risk in the spec; `examples/ingest_probe.rs` is how
//! a breakage is diagnosed.

use serde_json::Value;

// ── InnerTube client constants ────────────────────────────────────────────
// Every value YouTube sees about "which app is asking" lives HERE and nowhere
// else. Measured 2026-09-15 (spec "Spike"). YouTube retires mobile
// `clientVersion`s over time: when transcripts stop arriving, re-run the
// probe, then change this block — not a call site.
pub struct InnerTubeClient {
    pub name: &'static str,
    /// `X-Youtube-Client-Name`.
    pub name_id: &'static str,
    pub version: &'static str,
    pub device_make: &'static str,
    pub device_model: &'static str,
    pub os_name: &'static str,
    pub os_version: &'static str,
    pub user_agent: &'static str,
}

pub const IOS: InnerTubeClient = InnerTubeClient {
    name: "IOS",
    name_id: "5",
    version: "20.10.4",
    device_make: "Apple",
    device_model: "iPhone16,2",
    os_name: "iPhone",
    os_version: "18.3.2.22D82",
    user_agent: "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)",
};
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Playability {
    Ok,
    /// Spec error table: `Partial`, with a loud log naming the client version.
    LoginRequired(String),
    /// Private, removed, region-blocked…: `Failed(YouTube's reason)`.
    Unplayable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionTrack {
    pub base_url: String,
    pub language_code: String,
    pub is_asr: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerInfo {
    pub playability: Playability,
    pub title: Option<String>,
    pub description: Option<String>,
    pub tracks: Vec<CaptionTrack>,
}

fn nonempty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn player_info(v: &Value) -> PlayerInfo {
    let status = v["playabilityStatus"]["status"].as_str().unwrap_or("");
    let reason = nonempty(v["playabilityStatus"]["reason"].as_str())
        .unwrap_or_else(|| if status.is_empty() { "YouTube did not say why".to_string() } else { status.to_string() });
    let playability = match status {
        "OK" => Playability::Ok,
        "LOGIN_REQUIRED" => Playability::LoginRequired(reason),
        _ => Playability::Unplayable(reason),
    };
    let tracks = v["captions"]["playerCaptionsTracklistRenderer"]["captionTracks"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    Some(CaptionTrack {
                        base_url: t["baseUrl"].as_str()?.to_string(),
                        language_code: t["languageCode"].as_str().unwrap_or("").to_string(),
                        is_asr: t["kind"].as_str() == Some("asr"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    PlayerInfo {
        playability,
        title: nonempty(v["videoDetails"]["title"].as_str()),
        description: nonempty(v["videoDetails"]["shortDescription"].as_str()),
        tracks,
    }
}

pub fn parse_player(json: &str) -> Result<PlayerInfo, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("unreadable player response: {e}"))?;
    Ok(player_info(&v))
}

/// Title and description only — the WEB caption URLs need a PO token and
/// answer 0 bytes (spike). A streaming deserializer reads the first JSON
/// value and ignores the script text after it.
pub fn parse_watch_page(html: &str) -> Option<PlayerInfo> {
    let at = html.find("ytInitialPlayerResponse")?;
    let rest = html[at + "ytInitialPlayerResponse".len()..].trim_start().strip_prefix('=')?.trim_start();
    let v: Value = serde_json::Deserializer::from_str(rest).into_iter::<Value>().next()?.ok()?;
    Some(player_info(&v))
}

pub fn choose_track(tracks: &[CaptionTrack]) -> Option<&CaptionTrack> {
    tracks.iter().find(|t| !t.is_asr).or_else(|| tracks.first())
}

/// Tags → spaces, then entities decoded TWICE (the IOS payload carries
/// `&amp;#39;`), then whitespace collapsed.
pub fn caption_text(body: &str) -> String {
    let mut stripped = String::with_capacity(body.len());
    let mut in_tag = false;
    for c in body.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                stripped.push(' ');
            }
            _ if !in_tag => stripped.push(c),
            _ => {}
        }
    }
    let once = crate::db::decode_html_entities(&stripped).into_owned();
    let twice = crate::db::decode_html_entities(&once).into_owned();
    twice.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Spaces between two Thai characters, per Thai character. Caption-segment
/// joins give a few percent; one space per word (the ANDROID client's output)
/// gives ~20%. Printed by the probe, asserted by the Thai fixture test.
pub fn thai_word_gap_ratio(text: &str) -> f64 {
    let is_thai = |c: char| ('\u{0E00}'..='\u{0E7F}').contains(&c);
    let chars: Vec<char> = text.chars().collect();
    let thai = chars.iter().filter(|c| is_thai(**c)).count();
    if thai == 0 {
        return 0.0;
    }
    let gaps = chars.windows(3).filter(|w| is_thai(w[0]) && w[1] == ' ' && is_thai(w[2])).count();
    gaps as f64 / thai as f64
}

use reqwest::Method;
use tokio_util::sync::CancellationToken;

use crate::ingest::net::{read_capped, send_guarded, GuardedRequest, CANCELLED};
use crate::ingest::web::{decode, MAX_BODY_BYTES};
use crate::ingest::{FetchPolicy, FetchStatus, FetchedSource, SourceKind};

/// Injectable so tests can point every request at mockito.
#[derive(Debug, Clone)]
pub struct YoutubeEndpoints {
    pub player_url: String,
    pub watch_url_prefix: String,
}

impl Default for YoutubeEndpoints {
    fn default() -> Self {
        YoutubeEndpoints {
            player_url: "https://www.youtube.com/youtubei/v1/player?prettyPrint=false".into(),
            watch_url_prefix: "https://www.youtube.com/watch?v=".into(),
        }
    }
}

async fn get_body(req: GuardedRequest, policy: FetchPolicy, cancel: &CancellationToken) -> Result<String, String> {
    let resp = send_guarded(req, policy, cancel).await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    // Finding F5: `decode(_, None)` sniffs a `charset=` string out of the
    // first 4 KB of the BODY when no content-type is given — a fine fallback
    // for a real HTML page, but every response `get_body` reads here (the IOS
    // player's JSON, the watch-page HTML, the caption XML) is YouTube's own
    // UTF-8 output, and a video description or caption can itself contain the
    // literal text "charset=" and mis-select an encoding. State it explicitly
    // instead of sniffing.
    Ok(decode(&read_capped(resp, MAX_BODY_BYTES, cancel).await?, Some("application/json; charset=utf-8")))
}

pub async fn request_player(id: &str, endpoints: &YoutubeEndpoints, policy: FetchPolicy, cancel: &CancellationToken) -> Result<PlayerInfo, String> {
    let body = serde_json::json!({
        "context": {"client": {
            "clientName": IOS.name, "clientVersion": IOS.version, "deviceMake": IOS.device_make,
            "deviceModel": IOS.device_model, "osName": IOS.os_name, "osVersion": IOS.os_version,
            "hl": "en", "gl": "US"
        }},
        "videoId": id, "contentCheckOk": true, "racyCheckOk": true
    });
    let req = GuardedRequest {
        method: Method::POST,
        url: endpoints.player_url.clone(),
        headers: vec![
            ("content-type", "application/json".into()),
            ("user-agent", IOS.user_agent.into()),
            ("x-youtube-client-name", IOS.name_id.into()),
            ("x-youtube-client-version", IOS.version.into()),
        ],
        body: Some(body.to_string().into_bytes()),
    };
    parse_player(&get_body(req, policy, cancel).await?)
}

fn warn_possibly_stale(what: &str) {
    crate::log!(
        "ingest: YouTube returned {what} to the IOS client (clientVersion {}) — the client constants may be stale; run `cargo run --example ingest_probe`",
        IOS.version
    );
}

fn partial(url: &str, info: &PlayerInfo, reason: &str) -> FetchedSource {
    let text = [info.title.as_deref(), info.description.as_deref()].into_iter().flatten().collect::<Vec<_>>().join("\n\n");
    if text.is_empty() {
        return FetchedSource { title: info.title.clone(), ..FetchedSource::failed(url, SourceKind::YouTube, reason) };
    }
    FetchedSource { url: url.to_string(), kind: SourceKind::YouTube, title: info.title.clone(), text, status: FetchStatus::Partial(reason.to_string()) }
}

async fn watch_fallback(url: &str, id: &str, endpoints: &YoutubeEndpoints, policy: FetchPolicy, cancel: &CancellationToken, reason: &str) -> FetchedSource {
    let req = GuardedRequest {
        headers: vec![("accept-language", "en-US,en;q=0.9".into())],
        ..GuardedRequest::get(&format!("{}{id}", endpoints.watch_url_prefix))
    };
    match get_body(req, policy, cancel).await.and_then(|html| parse_watch_page(&html).ok_or_else(|| "no player data on the watch page".into())) {
        Ok(info) => partial(url, &info, reason),
        Err(e) => FetchedSource::failed(url, SourceKind::YouTube, format!("{reason}; the watch page also failed ({e})")),
    }
}

/// Spec Decision 2 and the error table: never fails the ingest; degrades to
/// `Partial` (title + description) or `Failed(reason)`.
pub async fn fetch_youtube(url: &str, id: &str, endpoints: &YoutubeEndpoints, policy: FetchPolicy, cancel: &CancellationToken) -> FetchedSource {
    let player = match request_player(id, endpoints, policy, cancel).await {
        Ok(p) => p,
        Err(e) if e == CANCELLED => return FetchedSource::failed(url, SourceKind::YouTube, CANCELLED),
        Err(e) => return watch_fallback(url, id, endpoints, policy, cancel, &format!("the player request failed ({e})")).await,
    };
    match &player.playability {
        Playability::Unplayable(reason) => {
            return FetchedSource { title: player.title.clone(), ..FetchedSource::failed(url, SourceKind::YouTube, reason.clone()) }
        }
        Playability::LoginRequired(reason) => {
            warn_possibly_stale(&format!("LOGIN_REQUIRED ({reason})"));
            return watch_fallback(url, id, endpoints, policy, cancel, &format!("YouTube asked to sign in ({reason})")).await;
        }
        Playability::Ok => {}
    }
    let Some(track) = choose_track(&player.tracks) else {
        return partial(url, &player, "no captions");
    };
    let caption_req = GuardedRequest { headers: vec![("user-agent", IOS.user_agent.into())], ..GuardedRequest::get(&track.base_url) };
    let body = match get_body(caption_req, policy, cancel).await {
        Ok(b) => b,
        Err(e) => return partial(url, &player, &format!("the transcript request failed ({e})")),
    };
    let text = caption_text(&body);
    if text.is_empty() {
        warn_possibly_stale("an empty caption body");
        return partial(url, &player, "YouTube returned an empty transcript");
    }
    FetchedSource { url: url.to_string(), kind: SourceKind::YouTube, title: player.title.clone(), text, status: FetchStatus::Ok }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EN: &str = include_str!("fixtures/yt_player_ios_en.json");
    const TH: &str = include_str!("fixtures/yt_player_ios_th.json");
    const GONE: &str = include_str!("fixtures/yt_player_ios_unavailable.json");
    const CAPTION_EN: &str = include_str!("fixtures/yt_caption_en.xml");
    const CAPTION_TH: &str = include_str!("fixtures/yt_caption_th.xml");
    const WATCH_EN: &str = include_str!("fixtures/yt_watch_en.html");

    #[test]
    fn the_english_capture_is_playable_with_captions() {
        let info = parse_player(EN).unwrap();
        assert_eq!(info.playability, Playability::Ok);
        assert!(info.title.as_deref().is_some_and(|t| !t.is_empty()));
        assert!(info.tracks.iter().any(|t| t.language_code.starts_with("en")), "{:?}", info.tracks);
    }

    #[test]
    fn the_thai_capture_has_only_auto_captions() {
        let info = parse_player(TH).unwrap();
        assert!(choose_track(&info.tracks).expect("a track").is_asr);
    }

    #[test]
    fn a_non_ok_playability_carries_youtubes_reason() {
        match parse_player(GONE).unwrap().playability {
            Playability::Unplayable(reason) => assert!(!reason.is_empty()),
            other => panic!("expected Unplayable, got {other:?}"),
        }
    }

    /// Derived from the capture's own track shape: YouTube did not hand us a
    /// video with both kinds.
    #[test]
    fn a_manual_track_is_preferred_over_asr() {
        let asr = CaptionTrack { base_url: "a".into(), language_code: "en".into(), is_asr: true };
        let manual = CaptionTrack { base_url: "m".into(), language_code: "en".into(), is_asr: false };
        assert_eq!(choose_track(&[asr.clone(), manual.clone()]), Some(&manual));
        assert_eq!(choose_track(&[asr.clone()]), Some(&asr));
        assert_eq!(choose_track(&[]), None);
    }

    #[test]
    fn caption_text_strips_tags_and_decodes_entities_twice() {
        assert_eq!(caption_text(r#"<timedtext><body><p t="0">it&amp;#39;s</p><p t="1">A &amp;amp; B</p></body></timedtext>"#), "it's A & B");
        let en = caption_text(CAPTION_EN);
        assert!(!en.is_empty());
        assert!(!en.contains('<') && !en.contains("&#39;") && !en.contains("&amp;"), "{en}");
    }

    #[test]
    fn caption_text_keeps_a_literal_greater_than_in_the_text() {
        assert_eq!(caption_text(r#"<transcript><text start="0">5 > 3</text></transcript>"#), "5 > 3");
    }

    /// The spike's reason for choosing IOS: ANDROID spaced every Thai word.
    /// The control proves the metric can see that failure.
    #[test]
    fn the_thai_transcript_reads_as_normal_thai() {
        let th = caption_text(CAPTION_TH);
        assert!(th.chars().any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c)), "no Thai in the capture: {th}");
        assert!(thai_word_gap_ratio(&th) < 0.08, "ratio {} — per-word spacing: {th}", thai_word_gap_ratio(&th));
        assert!(thai_word_gap_ratio("ยก ตัว อย่าง เช่น การ ทำ งาน") > 0.08, "control: the metric must see per-word spacing");
    }

    #[test]
    fn the_watch_page_yields_the_same_title() {
        let page = parse_watch_page(WATCH_EN).expect("player response on the page");
        assert_eq!(page.title, parse_player(EN).unwrap().title);
        assert!(parse_watch_page("<html>consent wall</html>").is_none());
    }

    #[test]
    fn the_client_block_matches_the_spike() {
        assert_eq!((IOS.name, IOS.name_id, IOS.version), ("IOS", "5", "20.10.4"));
        assert!(IOS.user_agent.starts_with("com.google.ios.youtube/20.10.4 "));
    }

    use crate::ingest::FetchStatus;
    use mockito::Matcher;
    use tokio_util::sync::CancellationToken;

    const VIDEO: &str = "https://www.youtube.com/watch?v=jXtnhyro-QE";

    fn endpoints(server: &mockito::ServerGuard) -> YoutubeEndpoints {
        YoutubeEndpoints {
            player_url: format!("{}/youtubei/v1/player?prettyPrint=false", server.url()),
            watch_url_prefix: format!("{}/watch?v=", server.url()),
        }
    }

    /// The fixtures' redacted `baseUrl` points at youtube.com; aim it at mockito.
    fn local(json: &str, server: &mockito::ServerGuard) -> String {
        json.replace("https://www.youtube.com/api/timedtext", &format!("{}/api/timedtext", server.url()))
    }

    async fn run(server: &mockito::ServerGuard) -> crate::ingest::FetchedSource {
        fetch_youtube(VIDEO, "jXtnhyro-QE", &endpoints(server), FetchPolicy { allow_loopback: true }, &CancellationToken::new()).await
    }

    #[tokio::test]
    async fn a_playable_video_with_captions_is_ok_and_asks_as_the_ios_client() {
        let mut server = mockito::Server::new_async().await;
        let player = server
            .mock("POST", "/youtubei/v1/player")
            .match_query(Matcher::Any)
            .match_header("x-youtube-client-name", "5")
            .match_header("x-youtube-client-version", "20.10.4")
            .match_header("user-agent", IOS.user_agent)
            .match_body(Matcher::PartialJson(serde_json::json!({
                "context": {"client": {"clientName": "IOS", "clientVersion": "20.10.4", "deviceModel": "iPhone16,2", "osVersion": "18.3.2.22D82"}},
                "videoId": "jXtnhyro-QE"
            })))
            .with_status(200)
            .with_body(local(EN, &server))
            .create_async()
            .await;
        let _c = server.mock("GET", "/api/timedtext").match_query(Matcher::Any).with_status(200).with_body(CAPTION_EN).create_async().await;
        let s = run(&server).await;
        player.assert_async().await;
        assert_eq!(s.status, FetchStatus::Ok);
        assert_eq!(s.title, parse_player(EN).unwrap().title);
        assert_eq!(s.text, caption_text(CAPTION_EN));
    }

    /// Derived from the English capture with its captions removed.
    #[tokio::test]
    async fn no_caption_tracks_is_partial_with_title_and_description() {
        let mut v: serde_json::Value = serde_json::from_str(EN).unwrap();
        v["captions"] = serde_json::Value::Null;
        let mut server = mockito::Server::new_async().await;
        let _p = server.mock("POST", "/youtubei/v1/player").match_query(Matcher::Any).with_status(200).with_body(v.to_string()).create_async().await;
        let s = run(&server).await;
        assert_eq!(s.status, FetchStatus::Partial("no captions".into()));
        assert!(s.text.contains(parse_player(EN).unwrap().title.as_deref().unwrap()));
    }

    #[tokio::test]
    async fn an_empty_caption_body_is_partial() {
        let mut server = mockito::Server::new_async().await;
        let _p = server.mock("POST", "/youtubei/v1/player").match_query(Matcher::Any).with_status(200).with_body(local(EN, &server)).create_async().await;
        let _c = server.mock("GET", "/api/timedtext").match_query(Matcher::Any).with_status(200).with_body("").create_async().await;
        assert_eq!(run(&server).await.status, FetchStatus::Partial("YouTube returned an empty transcript".into()));
    }

    /// Finding F5: `get_body`'s response is always YouTube's own UTF-8
    /// output, and `decode(_, None)`'s fallback sniffs a `charset=` string
    /// out of the first 4 KB of the BODY ITSELF when no content-type is
    /// given — a video description can legitimately contain that literal
    /// text (someone writing about character encodings, or just this test).
    /// Multi-byte UTF-8 (café, 日本語) decoded as a single-byte encoding like
    /// windows-1252 comes out as mojibake, which is the regression this
    /// guards: `get_body` must state UTF-8 explicitly rather than sniff.
    #[tokio::test]
    async fn a_charset_looking_string_in_the_description_does_not_change_the_decoding() {
        let mut v: serde_json::Value = serde_json::from_str(EN).unwrap();
        v["videoDetails"]["shortDescription"] = serde_json::Value::String("see charset=windows-1252 for an example".into());
        v["videoDetails"]["title"] = serde_json::Value::String("café — 日本語".into());
        v["captions"] = serde_json::Value::Null; // no caption fetch needed for this assertion
        let mut server = mockito::Server::new_async().await;
        let _p = server.mock("POST", "/youtubei/v1/player").match_query(Matcher::Any).with_status(200).with_body(v.to_string()).create_async().await;
        let s = run(&server).await;
        assert_eq!(s.title.as_deref(), Some("café — 日本語"), "{:?}", s.title);
    }

    #[tokio::test]
    async fn a_non_ok_playability_fails_with_youtubes_reason() {
        let mut server = mockito::Server::new_async().await;
        let _p = server.mock("POST", "/youtubei/v1/player").match_query(Matcher::Any).with_status(200).with_body(GONE).create_async().await;
        let Playability::Unplayable(reason) = parse_player(GONE).unwrap().playability else { unreachable!() };
        assert_eq!(run(&server).await.status, FetchStatus::Failed(reason));
    }

    /// Derived: the English capture with its status replaced by the shape the
    /// spike's research names for a bot check.
    #[tokio::test]
    async fn login_required_falls_back_to_the_watch_page_as_partial() {
        let mut v: serde_json::Value = serde_json::from_str(EN).unwrap();
        v["playabilityStatus"] = serde_json::json!({"status": "LOGIN_REQUIRED", "reason": "Sign in to confirm you're not a bot"});
        v["videoDetails"] = serde_json::Value::Null;
        let mut server = mockito::Server::new_async().await;
        let _p = server.mock("POST", "/youtubei/v1/player").match_query(Matcher::Any).with_status(200).with_body(v.to_string()).create_async().await;
        let _w = server.mock("GET", "/watch").match_query(Matcher::Any).with_status(200).with_header("content-type", "text/html").with_body(WATCH_EN).create_async().await;
        let s = run(&server).await;
        assert!(matches!(&s.status, FetchStatus::Partial(r) if r.contains("sign in")), "{:?}", s.status);
        assert_eq!(s.title, parse_player(EN).unwrap().title);
    }

    #[tokio::test]
    async fn a_failed_player_request_falls_back_to_the_watch_page() {
        let mut server = mockito::Server::new_async().await;
        let _p = server.mock("POST", "/youtubei/v1/player").match_query(Matcher::Any).with_status(500).create_async().await;
        let _w = server.mock("GET", "/watch").match_query(Matcher::Any).with_status(200).with_header("content-type", "text/html").with_body(WATCH_EN).create_async().await;
        assert!(matches!(run(&server).await.status, FetchStatus::Partial(_)));
    }

    /// A closed port stands in for a connection-level failure on the caption
    /// GET; `net::send_guarded` must not let YouTube's signed `baseUrl`
    /// (query, including `signature=`) leak into the degraded reason.
    #[tokio::test]
    async fn a_failed_transcript_request_leaks_no_signed_url() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut v: serde_json::Value = serde_json::from_str(EN).unwrap();
        v["captions"]["playerCaptionsTracklistRenderer"]["captionTracks"][0]["baseUrl"] =
            serde_json::json!(format!("http://127.0.0.1:{port}/api/timedtext?signature=SECRET"));
        let mut server = mockito::Server::new_async().await;
        let _p = server.mock("POST", "/youtubei/v1/player").match_query(Matcher::Any).with_status(200).with_body(v.to_string()).create_async().await;
        let s = run(&server).await;
        assert!(matches!(s.status, FetchStatus::Partial(_)), "{:?}", s.status);
        let FetchStatus::Partial(reason) = &s.status else { unreachable!() };
        assert!(!reason.contains("SECRET"), "{reason}");
        assert!(!reason.to_lowercase().contains("signature"), "{reason}");
    }
}
