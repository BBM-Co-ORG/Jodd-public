//! A TikTok / Instagram / Facebook / X link → one `FetchedSource` (spec §5).
//!
//! The platform file reads the post (`fetch_meta`); this file decides where
//! the source's text comes from — D2's order, platform transcript, then
//! speech-to-text, then the caption alone — and says which one in D14's header
//! and, when it is not the whole source, in the status.

use tokio_util::sync::CancellationToken;

use super::audio::{self, MAX_DURATION_SECS};
use super::net::CANCELLED;
use super::run::IngestStage;
use super::social::{platform_label, render_source_text, PlatformMeta, Provenance};
use super::{facebook, instagram, tiktok, urls, x, FetchPolicy, FetchStatus, FetchedSource, HttpFetcher, SourceKind};
use crate::stt_config::SttReady;

/// D6: no speech-to-text is a normal state, and the status says what would help.
pub const UNCONFIGURED: &str = "caption only — set up Speech-to-text in Settings to include what is said";
/// The header's `Transcript source:` for the same state.
const UNCONFIGURED_SOURCE: &str = "speech-to-text is not set up";
/// D8.
pub const FACEBOOK_POST: &str = "Facebook does not show this post's video without signing in";
/// An X post with no video (text, photo, GIF) has nothing to transcribe and is
/// whole. A post that has one X gave no usable MP4 for is not (`has_video`).
const NO_SPOKEN_AUDIO: &str = "no spoken audio";
const CAPTION_CUT: &str = "the caption may be cut off";

/// Fetch the post behind `url` and turn it into a source, with the
/// fetcher's speech-to-text and "Transcribe the audio myself" (D2).
///
/// `kind` is the caller's classification (`run.rs` classifies every link).
/// TikTok and Facebook are fetched at the link as given; Instagram and X by
/// the shortcode / id read from it, so a link without one fails here.
pub async fn fetch(
    fetcher: &HttpFetcher,
    url: &str,
    kind: SourceKind,
    cancel: &CancellationToken,
    on_stage: &(dyn Fn(IngestStage) + Send + Sync),
) -> FetchedSource {
    let policy = fetcher.policy;
    let not_a_link = || FetchedSource::failed(url, kind, format!("not a {} link", platform_label(kind)));
    #[allow(unused_mut)]
    let mut meta = match (kind, urls::classify(url)) {
        #[cfg(test)]
        (SourceKind::TikTok, _) if fetcher.test_media_hosts.is_some() => {
            tiktok::fetch_meta_with(url, fetcher.test_media_hosts.unwrap(), policy, cancel).await
        }
        (SourceKind::TikTok, _) => tiktok::fetch_meta(url, policy, cancel).await,
        (SourceKind::Facebook, _) => facebook::fetch_meta(url, policy, cancel).await,
        (SourceKind::Instagram, urls::UrlKind::Instagram { shortcode }) => instagram::fetch_meta(&shortcode, policy, cancel).await,
        (SourceKind::X, urls::UrlKind::X { id }) => x::fetch_meta(&id, policy, cancel).await,
        _ => return not_a_link(),
    };
    // A cancelled sub-request (TikTok's captions) can look like "no transcript".
    if cancel.is_cancelled() {
        return FetchedSource::failed(url, kind, CANCELLED);
    }
    #[cfg(test)]
    if let (Some(hosts), Some(media)) = (fetcher.test_media_hosts, meta.media.as_mut()) {
        media.allowed_host_suffixes = hosts;
    }
    source_from_meta(url, meta, fetcher.stt.as_ref(), fetcher.prefer_own_transcription, policy, cancel, on_stage).await
}

/// D2/D6/D7/D8 as one table: where the text comes from, and the status that
/// says so when it is not the whole source. The header (D14) is always kept.
pub(crate) async fn source_from_meta(
    url: &str,
    meta: PlatformMeta,
    stt: Option<&SttReady>,
    prefer_own: bool,
    policy: FetchPolicy,
    cancel: &CancellationToken,
    on_stage: &(dyn Fn(IngestStage) + Send + Sync),
) -> FetchedSource {
    let kind = meta.platform;
    if let Some(reason) = meta.unavailable.clone() {
        return FetchedSource::failed(url, kind, reason);
    }
    let platform = platform_label(kind);
    // D2: the platform's transcript wins unless the user asked for their own
    // — which only means something when there is speech-to-text to ask.
    let own = meta.transcript.is_none() || (prefer_own && stt.is_some());

    // `(transcript, transcript source, Partial reason)`.
    let (transcript, source, reason) = if meta.is_post {
        (String::new(), none(FACEBOOK_POST), Some(FACEBOOK_POST.to_string()))
    } else if let Some(t) = meta.transcript.clone().filter(|_| !own) {
        (t.text, t.source, None)
    } else if kind == SourceKind::X && meta.media.is_none() && !meta.has_video {
        (String::new(), none(NO_SPOKEN_AUDIO), None)
    } else {
        // Checked before speech-to-text is: setting it up would not help these.
        let attempt = if meta.media.is_none() {
            Err(format!("{platform} did not show this clip's video"))
        } else if meta.duration_secs.is_some_and(|s| s > MAX_DURATION_SECS) {
            Err(audio::too_long(meta.duration_secs))
        } else if let Some(stt) = stt {
            audio::transcribe_clip(&meta, stt, policy, cancel, on_stage).await
        } else {
            Err(UNCONFIGURED.to_string())
        };
        if cancel.is_cancelled() {
            return FetchedSource::failed(url, kind, CANCELLED);
        }
        match (attempt, meta.transcript.clone()) {
            (Ok((text, source)), _) => (text, source, None),
            // Only a forced run gets here with a platform transcript: it stays
            // the text, and the status says the user's own run did not happen.
            (Err(e), Some(t)) => (t.text, t.source, Some(e)),
            (Err(e), None) if e == UNCONFIGURED => (String::new(), none(UNCONFIGURED_SOURCE), Some(e)),
            (Err(e), None) => (String::new(), none(&e), Some(e)),
        }
    };

    let reason = match (reason, meta.caption_maybe_cut) {
        (Some(r), true) => Some(format!("{r}; {CAPTION_CUT}")),
        (None, true) if transcript.is_empty() => Some(CAPTION_CUT.to_string()),
        (r, _) => r,
    };
    let provenance = Provenance {
        platform: platform.to_string(),
        author: meta.author,
        caption: meta.caption,
        caption_maybe_cut: meta.caption_maybe_cut,
        transcript_source: source,
    };
    FetchedSource {
        // D12: a share link becomes the post's own URL once the platform names it.
        url: meta.canonical_url.unwrap_or_else(|| url.to_string()),
        kind,
        title: meta.title,
        text: render_source_text(&provenance, &transcript),
        status: reason.map_or(FetchStatus::Ok, FetchStatus::Partial),
    }
}

/// D14's `Transcript source:` when there is no transcript.
fn none(reason: &str) -> String {
    format!("none — {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    use crate::ingest::social::{parse_provenance, vtt_to_text, MediaRef, PlatformTranscript};

    const PAGE: &str = include_str!("fixtures/tiktok_video.html");
    const VTT: &str = include_str!("fixtures/tiktok_subtitle.vtt");
    const CLIP: &[u8] = include_bytes!("fixtures/audio_he_aac.mp4");
    const ID: &str = "7696048386981645569";
    const CANONICAL: &str = "https://www.tiktok.com/@tonsr_/video/7696048386981645569";
    const SHARED: &str = "https://vt.tiktok.com/ZSabc123/";
    const LOOPBACK: FetchPolicy = FetchPolicy { allow_loopback: true };

    struct Mocks {
        server: mockito::ServerGuard,
        media: mockito::Mock,
        stt: mockito::Mock,
    }

    /// A TikTok page and its captions (or a 404 for them) on mockito, read by
    /// the real TikTok fetcher; the media URL then points at mockito too.
    async fn tiktok_meta(captions: bool, media_hits: usize, stt_hits: usize, stt_status: usize) -> (PlatformMeta, Mocks) {
        let mut server = mockito::Server::new_async().await;
        let page = PAGE.replace("https://v16-webapp.tiktok.com", &server.url());
        let _p = server.mock("GET", format!("/@tonsr_/video/{ID}").as_str()).with_status(200).with_body(page).create_async().await;
        let _v = server
            .mock("GET", Matcher::Regex("/redacted/.*".into()))
            .with_status(if captions { 200 } else { 404 })
            .with_body(VTT)
            .create_async()
            .await;
        let media = server.mock("GET", "/clip.mp4").expect(media_hits).with_status(200).with_body(CLIP).create_async().await;
        let stt = server
            .mock("POST", "/audio/transcriptions")
            .expect(stt_hits)
            .with_status(stt_status)
            .with_body(r#"{"text":"what the speaker said"}"#)
            .create_async()
            .await;
        let url = format!("{}/@tonsr_/video/{ID}", server.url());
        let mut meta = tiktok::fetch_meta_with(&url, &["127.0.0.1"], LOOPBACK, &CancellationToken::new()).await;
        assert_eq!(meta.transcript.is_some(), captions, "the fixture's captions");
        let media_ref = meta.media.as_mut().expect("the fixture's playAddr");
        media_ref.url = format!("{}/clip.mp4", server.url());
        media_ref.allowed_host_suffixes = &["127.0.0.1"];
        (meta, Mocks { server, media, stt })
    }

    fn ready(server: &mockito::ServerGuard) -> SttReady {
        SttReady {
            preset: crate::stt_config::SttPreset::Groq,
            base_url: server.url(),
            model: "whisper-large-v3".into(),
            language: None,
            max_upload_bytes: 25 * 1024 * 1024,
            api_key: None,
            host: server.host_with_port(),
        }
    }

    async fn decide(meta: PlatformMeta, stt: Option<&SttReady>, prefer_own: bool) -> FetchedSource {
        source_from_meta(SHARED, meta, stt, prefer_own, LOOPBACK, &CancellationToken::new(), &|_| {}).await
    }

    fn transcript_source(s: &FetchedSource) -> String {
        parse_provenance(&s.text).expect("a provenance header").transcript_source
    }

    /// The transcript below the header's blank line.
    fn body(s: &FetchedSource) -> &str {
        s.text.split_once("\n\n").map(|(_, b)| b).unwrap_or("")
    }

    #[tokio::test]
    async fn platform_transcript_wins() {
        let (meta, m) = tiktok_meta(true, 0, 0, 200).await;
        let stt = ready(&m.server);
        let s = decide(meta, Some(&stt), false).await;
        m.media.assert_async().await;
        m.stt.assert_async().await;
        assert_eq!(s.status, FetchStatus::Ok);
        assert_eq!((s.kind, s.url.as_str()), (SourceKind::TikTok, CANONICAL), "D12: the short link becomes the post's URL");
        assert!(s.text.starts_with("Platform: TikTok\nAuthor: @tonsr_ (Lazy_Productive)\nCaption: 5 skill ใน claude code"), "{}", s.text);
        assert_eq!(transcript_source(&s), "TikTok auto-captions");
        assert_eq!(body(&s), vtt_to_text(VTT));
        assert!(s.title.as_deref().unwrap().starts_with("5 skill"));
    }

    #[tokio::test]
    async fn override_transcribes_even_with_a_platform_transcript() {
        let (meta, m) = tiktok_meta(true, 1, 1, 200).await;
        let stt = ready(&m.server);
        let s = decide(meta, Some(&stt), true).await;
        m.media.assert_async().await;
        m.stt.assert_async().await;
        assert_eq!(s.status, FetchStatus::Ok);
        assert_eq!(transcript_source(&s), format!("speech-to-text: {}, whisper-large-v3", m.server.host_with_port()));
        assert_eq!(
            crate::ingest::social::sources_status(&parse_provenance(&s.text).unwrap()),
            format!("transcribed by {} (whisper-large-v3)", m.server.host_with_port())
        );
        assert_eq!(body(&s), "what the speaker said");
    }

    #[tokio::test]
    async fn unconfigured_is_partial_with_the_set_up_reason() {
        let (meta, m) = tiktok_meta(false, 0, 0, 200).await;
        let s = decide(meta, None, false).await;
        m.media.assert_async().await;
        assert_eq!(s.status, FetchStatus::Partial(UNCONFIGURED.into()));
        assert_eq!(transcript_source(&s), "none — speech-to-text is not set up");
        assert_eq!(body(&s), "", "a caption-only source is its header alone");
        assert!(s.is_usable(), "the header still reaches the map step");

        // With no speech-to-text, "Transcribe the audio myself" changes nothing.
        let (meta, _m) = tiktok_meta(true, 0, 0, 200).await;
        assert_eq!(decide(meta, None, true).await.status, FetchStatus::Ok);
    }

    #[tokio::test]
    async fn a_failed_transcription_keeps_the_platform_transcript() {
        // Forced, the platform's transcript stays the text and the failure is the status.
        let (meta, m) = tiktok_meta(true, 1, 1, 503).await;
        let stt = ready(&m.server);
        let s = decide(meta, Some(&stt), true).await;
        m.stt.assert_async().await;
        assert_eq!(s.status, FetchStatus::Partial("speech-to-text could not be reached".into()));
        assert_eq!(transcript_source(&s), "TikTok auto-captions");
        assert_eq!(body(&s), vtt_to_text(VTT));

        // With nothing to fall back on, the header says why there is no transcript.
        let (meta, m) = tiktok_meta(false, 1, 1, 401).await;
        let stt = ready(&m.server);
        let s = decide(meta, Some(&stt), false).await;
        m.stt.assert_async().await;
        assert_eq!(s.status, FetchStatus::Partial("speech-to-text rejected the key".into()));
        assert_eq!(transcript_source(&s), "none — speech-to-text rejected the key");
        assert_eq!(body(&s), "");
    }

    #[tokio::test]
    async fn no_transcript_and_speech_to_text_ready_transcribes() {
        let (meta, m) = tiktok_meta(false, 1, 1, 200).await;
        let stt = ready(&m.server);
        let stages = std::sync::Mutex::new(Vec::new());
        let s = source_from_meta(SHARED, meta, Some(&stt), false, LOOPBACK, &CancellationToken::new(), &|st| stages.lock().unwrap().push(st)).await;
        assert_eq!(s.status, FetchStatus::Ok);
        assert_eq!(body(&s), "what the speaker said");
        assert_eq!(stages.into_inner().unwrap(), vec![IngestStage::Downloading, IngestStage::Transcribing]);
    }

    fn post(platform: SourceKind) -> PlatformMeta {
        PlatformMeta {
            author: Some("The Page".into()),
            caption: Some("Some words".into()),
            title: Some("Some words".into()),
            ..PlatformMeta::new(platform)
        }
    }

    /// Rows that need no network: every one is decided before a download.
    #[tokio::test]
    async fn the_rows_that_never_download() {
        let nowhere = SttReady {
            preset: crate::stt_config::SttPreset::Custom,
            base_url: "http://127.0.0.1:9".into(),
            model: "m".into(),
            language: None,
            max_upload_bytes: 1,
            api_key: None,
            host: "127.0.0.1:9".into(),
        };
        let stt = Some(&nowhere);

        let gone = PlatformMeta { unavailable: Some("item is dark post".into()), ..post(SourceKind::TikTok) };
        let s = decide(gone, stt, false).await;
        assert_eq!((s.status, s.text.as_str(), s.url.as_str()), (FetchStatus::Failed("item is dark post".into()), "", SHARED));

        let fb = PlatformMeta { is_post: true, ..post(SourceKind::Facebook) };
        let s = decide(fb.clone(), stt, false).await;
        assert_eq!(s.status, FetchStatus::Partial("Facebook does not show this post's video without signing in".into()));
        assert_eq!(transcript_source(&s), "none — Facebook does not show this post's video without signing in");
        assert!(s.text.contains("Caption: Some words"));
        // D8: a post text that may be cut says so too.
        let s = decide(PlatformMeta { caption_maybe_cut: true, ..fb }, stt, false).await;
        assert_eq!(s.status, FetchStatus::Partial("Facebook does not show this post's video without signing in; the caption may be cut off".into()));

        // An X post with no video is whole: a text post has no spoken audio.
        let s = decide(post(SourceKind::X), stt, false).await;
        assert_eq!((s.status.clone(), transcript_source(&s)), (FetchStatus::Ok, "none — no spoken audio".to_string()));
        // ... unless its text may be cut, and then nothing else is wrong with it.
        let s = decide(PlatformMeta { caption_maybe_cut: true, ..post(SourceKind::X) }, stt, false).await;
        assert_eq!(s.status, FetchStatus::Partial("the caption may be cut off".into()));
        // A post WITH a video X gave no usable MP4 for (HLS only, http only) is not whole.
        let s = decide(PlatformMeta { has_video: true, ..post(SourceKind::X) }, stt, false).await;
        assert_eq!(s.status, FetchStatus::Partial("X did not show this clip's video".into()));
        assert_eq!(transcript_source(&s), "none — X did not show this clip's video");

        // A video platform that showed no video is not complete.
        for (kind, name) in [(SourceKind::TikTok, "TikTok"), (SourceKind::Instagram, "Instagram"), (SourceKind::Facebook, "Facebook")] {
            let s = decide(post(kind), stt, false).await;
            assert_eq!(s.status, FetchStatus::Partial(format!("{name} did not show this clip's video")), "{name}");
            let s = decide(post(kind), None, false).await;
            assert_eq!(s.status, FetchStatus::Partial(format!("{name} did not show this clip's video")), "setting up speech-to-text would not help: {name}");
        }

        // Too long: nothing is downloaded, the caption is kept, and so is a
        // platform transcript when the user asked for their own.
        let media = MediaRef { url: "http://127.0.0.1:9/never.mp4".into(), headers: vec![], cookies: vec![], allowed_host_suffixes: &["127.0.0.1"] };
        let long = PlatformMeta { duration_secs: Some(3600.0), media: Some(media), ..post(SourceKind::Instagram) };
        let s = decide(long.clone(), stt, false).await;
        assert_eq!(s.status, FetchStatus::Partial("too long to transcribe (60 min)".into()));
        assert_eq!(transcript_source(&s), "none — too long to transcribe (60 min)");
        let s = decide(long.clone(), None, false).await;
        assert_eq!(s.status, FetchStatus::Partial("too long to transcribe (60 min)".into()), "setting it up would not help either");
        let captions = PlatformTranscript { text: "the platform's words".into(), language: None, source: "TikTok captions".into() };
        let with = PlatformMeta { platform: SourceKind::TikTok, transcript: Some(captions), caption_maybe_cut: true, ..long };
        let s = decide(with.clone(), stt, true).await;
        assert_eq!(s.status, FetchStatus::Partial("too long to transcribe (60 min); the caption may be cut off".into()));
        assert_eq!((transcript_source(&s), body(&s)), ("TikTok captions".to_string(), "the platform's words"));
        // Not forced: the platform's transcript is the whole source; the header marks the caption.
        let s = decide(with, stt, false).await;
        assert_eq!(s.status, FetchStatus::Ok);
        assert!(s.text.contains("(may be cut off)"));
    }

    #[tokio::test]
    async fn a_link_that_is_not_the_platform_fails_without_a_request() {
        let fetcher = HttpFetcher { policy: LOOPBACK, ..HttpFetcher::default() };
        for kind in [SourceKind::Instagram, SourceKind::X] {
            let s = fetch(&fetcher, "https://example.com/a", kind, &CancellationToken::new(), &|_| {}).await;
            assert!(matches!(s.status, FetchStatus::Failed(_)), "{kind:?}: {:?}", s.status);
        }
    }

    /// `HttpFetcher`'s social arm end to end: the real TikTok page and captions,
    /// the clip and the endpoint all on mockito. The fetcher's own speech-to-text
    /// and "Transcribe the audio myself" are what decide, and the stages arrive.
    #[tokio::test]
    async fn the_http_fetcher_forwards_speech_to_text_and_the_override() {
        use crate::ingest::SourceFetcher;
        for prefer_own in [true, false] {
            let mut server = mockito::Server::new_async().await;
            let page = PAGE.replace("https://v16-webapp.tiktok.com", &server.url()).replace("https://v16-webapp-prime.tiktok.com", &server.url());
            let _p = server.mock("GET", format!("/@tonsr_/video/{ID}").as_str()).with_status(200).with_body(page).create_async().await;
            let _v = server.mock("GET", Matcher::Regex("^/redacted/".into())).with_status(200).with_body(VTT).create_async().await;
            let hits = usize::from(prefer_own);
            let media = server.mock("GET", Matcher::Regex("^/video/tos/".into())).expect(hits).with_status(200).with_body(CLIP).create_async().await;
            let stt = server
                .mock("POST", "/audio/transcriptions")
                .expect(hits)
                .with_status(200)
                .with_body(r#"{"text":"what the speaker said"}"#)
                .create_async()
                .await;
            let fetcher = HttpFetcher {
                policy: LOOPBACK,
                stt: Some(ready(&server)),
                prefer_own_transcription: prefer_own,
                test_media_hosts: Some(&["127.0.0.1"]),
                ..HttpFetcher::default()
            };
            let stages = std::sync::Mutex::new(Vec::new());
            let url = format!("{}/@tonsr_/video/{ID}", server.url());
            let s = fetcher.fetch_reporting(&url, SourceKind::TikTok, CancellationToken::new(), &|st| stages.lock().unwrap().push(st)).await;
            media.assert_async().await;
            stt.assert_async().await;
            assert_eq!((s.status.clone(), s.kind, s.url.as_str()), (FetchStatus::Ok, SourceKind::TikTok, CANONICAL));
            let stages = stages.into_inner().unwrap();
            if prefer_own {
                assert_eq!(body(&s), "what the speaker said");
                assert_eq!(stages, vec![IngestStage::Downloading, IngestStage::Transcribing]);
            } else {
                assert_eq!(transcript_source(&s), "TikTok auto-captions");
                assert!(stages.is_empty(), "{stages:?}");
            }
        }
    }
}
