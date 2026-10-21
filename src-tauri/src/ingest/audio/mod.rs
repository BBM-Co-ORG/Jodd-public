//! A clip's sound, ready for speech-to-text (spec §5.3). Only the audio
//! track ever leaves the device, remuxed and never decoded (D4).

pub mod remux;
pub mod transcribe;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::ingest::net::{self, download_to_file, host_matches_suffixes, GuardedRequest, MediaPolicy, CANCELLED};
use crate::ingest::run::IngestStage;
use crate::ingest::social::{platform_label, stt_transcript_source, PlatformMeta};
use crate::ingest::FetchPolicy;
use crate::stt_config::{SttPreset, SttReady};

/// D7: a clip the platform says is longer than this is never downloaded.
pub const MAX_DURATION_SECS: f64 = 1800.0;
/// D7: the most a media download may be.
pub const MAX_MEDIA_BYTES: u64 = 300 * 1024 * 1024;
/// §5.3: a download has no whole-request timeout, only this wait for the
/// headers or the next chunk — a 28 MB clip on a phone needs longer than 20 s.
pub const MEDIA_IDLE_TIMEOUT: Duration = net::REQUEST_TIMEOUT;
/// A download that timed out is tried once more. Measured 2026-10-19: a Reel's
/// CDN sent no headers for 20 s, and the same URL answered in 2–3 s, three
/// times, minutes later. Only a timeout is retried — every other failure is
/// an answer.
const DOWNLOAD_ATTEMPTS: usize = 2;

/// D7's duration cap and the download's idle wait, named together so a test
/// can shorten either.
#[derive(Debug, Clone, Copy)]
struct ClipLimits {
    max_secs: f64,
    idle_timeout: Duration,
}

const CLIP_LIMITS: ClipLimits = ClipLimits { max_secs: MAX_DURATION_SECS, idle_timeout: MEDIA_IDLE_TIMEOUT };

/// The status when every attempt timed out: says who did not answer and what to do.
fn download_timed_out(platform: &str) -> String {
    format!("{platform} didn't send the video in time (tried twice) — try again later")
}

/// D7's status, `"too long to transcribe (N min)"`; without a duration there
/// is no N to name.
pub fn too_long(secs: Option<f64>) -> String {
    match secs {
        Some(s) => format!("{} ({} min)", transcribe::TOO_LONG, (s / 60.0).ceil() as u64),
        None => transcribe::TOO_LONG.to_string(),
    }
}

/// D15's per-call count, keyed by the KIND of endpoint: receipts take only
/// code-owned labels, so the host itself is recorded in the note's Sources line.
fn calls_metric(preset: SttPreset) -> &'static str {
    match preset {
        SttPreset::OpenAi => "stt_calls_openai",
        SttPreset::Groq => "stt_calls_groq",
        SttPreset::Custom => "stt_calls_custom",
    }
}

/// One clip's two scratch files under `cache_base()/jodd/`, deleted when this
/// drops — on success, on every error, on cancel, and when the future holding
/// it is itself dropped. A remux still running on the blocking pool when that
/// happens cannot be stopped, so it checks `abandoned` once it is done and
/// deletes what it wrote (`remux_abandonable`). Only a crash leaves files,
/// for `paths::sweep_ingest_temp` at the next start.
struct TempClip {
    video: PathBuf,
    audio: PathBuf,
    abandoned: Arc<AtomicBool>,
}

impl TempClip {
    fn new(dir: &Path) -> Self {
        let stem = format!("{}{}", crate::paths::INGEST_TEMP_PREFIX, uuid::Uuid::new_v4());
        TempClip {
            video: dir.join(format!("{stem}.mp4")),
            audio: dir.join(format!("{stem}.m4a")),
            abandoned: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Drop for TempClip {
    fn drop(&mut self) {
        // Set before deleting: a remux that misses the flag finished before
        // it was set, so its file exists and is deleted below.
        self.abandoned.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.video);
        let _ = std::fs::remove_file(&self.audio);
    }
}

/// The remux, run so that an owner which has gone away leaves nothing behind.
fn remux_abandonable(video: &Path, audio: &Path, abandoned: &AtomicBool) -> Result<remux::RemuxedAudio, remux::RemuxError> {
    let result = remux::remux_audio_only(video, audio);
    if abandoned.load(Ordering::SeqCst) {
        let _ = std::fs::remove_file(video);
        let _ = std::fs::remove_file(audio);
    }
    result
}

/// Download `meta`'s media, remux its audio track, transcribe it with `stt`.
/// `Ok((transcript, transcript_source))`, the source being D14's
/// `speech-to-text: <host>, <model>`; `Err` is the source's status reason, already worded for
/// the user. Nothing is requested for a clip over `MAX_DURATION_SECS` or a
/// media URL whose host is off the platform's allowlist (D7, D11), and a clip
/// whose remuxed audio is over it is never uploaded.
pub async fn transcribe_clip(
    meta: &PlatformMeta,
    stt: &SttReady,
    policy: FetchPolicy,
    cancel: &CancellationToken,
    on_stage: &(dyn Fn(IngestStage) + Send + Sync),
) -> Result<(String, String), String> {
    let dir = crate::paths::cache_base().ok_or("no cache folder on this device")?.join("jodd");
    transcribe_clip_in(&dir, CLIP_LIMITS, meta, stt, policy, cancel, on_stage).await
}

/// `transcribe_clip` with its scratch folder and limits named — a test's own
/// folder, so it can list exactly what was left behind, a cap a short fixture
/// can exceed, and an idle wait a test can sit through.
async fn transcribe_clip_in(
    dir: &Path,
    limits: ClipLimits,
    meta: &PlatformMeta,
    stt: &SttReady,
    policy: FetchPolicy,
    cancel: &CancellationToken,
    on_stage: &(dyn Fn(IngestStage) + Send + Sync),
) -> Result<(String, String), String> {
    let platform = platform_label(meta.platform);
    let max_secs = limits.max_secs;
    if let Some(secs) = meta.duration_secs.filter(|s| *s > max_secs) {
        return Err(too_long(Some(secs)));
    }
    let Some(media) = meta.media.as_ref() else {
        return Err(format!("{platform} did not show this clip's video"));
    };
    // The URL came from the platform's page: its host is vetted before any
    // request; `download_to_file` vets the host the bytes finally come from.
    let host_ok = reqwest::Url::parse(&media.url)
        .ok()
        .and_then(|u| u.host_str().map(|h| host_matches_suffixes(h, media.allowed_host_suffixes)))
        .unwrap_or(false);
    if !host_ok {
        return Err(format!("{platform} offered a video from an unexpected address"));
    }
    let clip = TempClip::new(dir);

    on_stage(IngestStage::Downloading);
    let media_limits = MediaPolicy { allowed_host_suffixes: media.allowed_host_suffixes, max_bytes: MAX_MEDIA_BYTES, idle_timeout: limits.idle_timeout };
    for attempt in 1..=DOWNLOAD_ATTEMPTS {
        let req = GuardedRequest { headers: media.headers.clone(), cookies: media.cookies.clone(), ..GuardedRequest::get(&media.url) };
        match download_to_file(req, &media_limits, policy, cancel, &clip.video).await {
            Ok(_) => break,
            Err(e) if e.starts_with(net::TOO_LARGE) => return Err(too_long(meta.duration_secs)),
            Err(e) if e == net::TIMED_OUT && attempt < DOWNLOAD_ATTEMPTS && !cancel.is_cancelled() => {
                crate::log!("ingest: {platform} video download timed out; trying once more");
            }
            Err(e) if e == net::TIMED_OUT => return Err(download_timed_out(platform)),
            Err(e) => return Err(e),
        }
    }
    if cancel.is_cancelled() {
        return Err(CANCELLED.into());
    }
    let (video, audio_path, abandoned) = (clip.video.clone(), clip.audio.clone(), clip.abandoned.clone());
    let audio = tokio::task::spawn_blocking(move || remux_abandonable(&video, &audio_path, &abandoned))
        .await
        .map_err(|e| format!("audio remux: {e}"))?
        .map_err(|e| e.to_string())?;
    // Only the `.m4a` is needed from here on.
    let _ = std::fs::remove_file(&clip.video);
    if cancel.is_cancelled() {
        return Err(CANCELLED.into());
    }
    // D7 again, now that the length is known: Facebook gives none up front.
    if audio.duration_secs > max_secs {
        return Err(too_long(Some(audio.duration_secs)));
    }

    on_stage(IngestStage::Transcribing);
    // D15: receipted, never budgeted — the endpoint's kind, sizes and
    // seconds only, never content or the host.
    crate::llm::receipts::stage("transcribing");
    crate::llm::receipts::metric_add(calls_metric(stt.preset), 1);
    crate::llm::receipts::metric_add("stt_audio_bytes", audio.bytes as usize);
    crate::llm::receipts::metric_add("stt_audio_secs", audio.duration_secs.round() as usize);
    let result = transcribe::transcribe(stt, &clip.audio, cancel).await;
    // Whatever runs next for the ingest is not transcription.
    crate::llm::receipts::stage("fetching_sources");
    match result {
        Ok(text) => Ok((text, stt_transcript_source(&stt.host, &stt.model))),
        Err(e) if e == transcribe::TOO_LONG => Err(too_long(Some(audio.duration_secs))),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use mockito::Matcher;

    use crate::ingest::social::MediaRef;
    use crate::ingest::SourceKind;

    pub(crate) const CLIP: &[u8] = include_bytes!("../fixtures/audio_he_aac.mp4");
    const LOOPBACK: FetchPolicy = FetchPolicy { allow_loopback: true };

    fn stt(server: &mockito::ServerGuard) -> SttReady {
        SttReady {
            preset: SttPreset::Groq,
            base_url: server.url(),
            model: "whisper-1".into(),
            language: None,
            max_upload_bytes: 25 * 1024 * 1024,
            api_key: Some("sk-test".into()),
            host: server.host_with_port(),
        }
    }

    fn meta(media_url: &str, duration: Option<f64>) -> PlatformMeta {
        PlatformMeta {
            duration_secs: duration,
            media: Some(MediaRef {
                url: media_url.to_string(),
                headers: vec![("referer", "https://www.tiktok.com/".into())],
                cookies: vec![("ttwid".into(), "TW".into())],
                allowed_host_suffixes: &["127.0.0.1"],
            }),
            ..PlatformMeta::new(SourceKind::TikTok)
        }
    }

    fn temp_dir() -> std::path::PathBuf {
        crate::paths::cache_base().expect("a cache dir").join("jodd")
    }

    /// `transcribe_clip_in` a fresh temporary folder with the real cap: a test
    /// never writes to the app's cache folder (only the one that pins that
    /// location calls `transcribe_clip`).
    async fn transcribe_here(
        m: &PlatformMeta,
        ready: &SttReady,
        cancel: &CancellationToken,
        on_stage: &(dyn Fn(IngestStage) + Send + Sync),
    ) -> Result<(String, String), String> {
        let dir = tempfile::tempdir().unwrap();
        transcribe_clip_in(dir.path(), CLIP_LIMITS, m, ready, LOOPBACK, cancel, on_stage).await
    }

    #[tokio::test]
    async fn a_clip_is_downloaded_with_its_headers_remuxed_and_transcribed() {
        let mut server = mockito::Server::new_async().await;
        let media = server
            .mock("GET", "/v.mp4")
            .match_header("referer", "https://www.tiktok.com/")
            .match_header("cookie", "ttwid=TW")
            .with_status(200)
            .with_header("content-type", "video/mp4")
            .with_body(CLIP)
            .create_async()
            .await;
        let upload = server
            .mock("POST", "/audio/transcriptions")
            .match_header("authorization", "Bearer sk-test")
            // The remuxed `.m4a`, not the video file: its `ftyp` brand is M4A.
            .match_body(Matcher::Regex("M4A ".into()))
            .with_status(200)
            .with_body(r#"{"text":"  hello from the clip  "}"#)
            .create_async()
            .await;
        let stages = Mutex::new(Vec::new());
        let got = transcribe_here(&meta(&format!("{}/v.mp4", server.url()), Some(5.0)), &stt(&server), &CancellationToken::new(), &|s| {
            stages.lock().unwrap().push(s)
        })
        .await;
        media.assert_async().await;
        upload.assert_async().await;
        assert_eq!(got, Ok(("hello from the clip".to_string(), format!("speech-to-text: {}, whisper-1", server.host_with_port()))));
        assert_eq!(stages.into_inner().unwrap(), vec![IngestStage::Downloading, IngestStage::Transcribing]);
    }

    /// D15: one call is one count, keyed by the endpoint's kind, and the stage
    /// goes back to fetching once the call is over.
    #[tokio::test]
    async fn a_transcription_is_receipted_by_endpoint_kind_without_the_host() {
        use crate::llm::receipts::{run_in, Store};
        let mut server = mockito::Server::new_async().await;
        let _media = server.mock("GET", "/v.mp4").with_status(200).with_body(CLIP).create_async().await;
        let _upload = server.mock("POST", "/audio/transcriptions").with_status(200).with_body(r#"{"text":"words"}"#).create_async().await;
        let store = std::sync::Arc::new(Mutex::new(Store::open(None).unwrap()));
        let url = format!("{}/v.mp4", server.url());
        let ready = stt(&server);
        run_in(store.clone(), "r", None, "ingest", async {
            crate::llm::receipts::stage("fetching_sources");
            transcribe_here(&meta(&url, Some(5.0)), &ready, &CancellationToken::new(), &|_| {}).await.map(|_| ())
        })
        .await
        .unwrap();
        let row = store.lock().unwrap().list(None).remove(0);
        let step = &row.steps[0];
        let calls: Vec<_> = step.metrics.iter().filter(|(k, _)| k.starts_with("stt_calls_")).collect();
        assert_eq!(calls, vec![(&"stt_calls_groq".to_string(), &1)]);
        assert!(step.metrics.get("stt_audio_bytes").is_some_and(|b| *b > 0));
        assert_eq!(step.stage, "fetching_sources");
        let json = serde_json::to_string(&row).unwrap();
        assert!(!json.contains(&server.host_with_port()) && !json.contains("words"), "{json}");

        assert_eq!(calls_metric(SttPreset::OpenAi), "stt_calls_openai");
        assert_eq!(calls_metric(SttPreset::Custom), "stt_calls_custom");
    }

    /// The remux cannot be stopped mid-run; one whose owner has gone away
    /// deletes what it wrote.
    #[test]
    fn an_abandoned_remux_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (video, audio) = (dir.path().join("ingest-x.mp4"), dir.path().join("ingest-x.m4a"));
        std::fs::write(&video, CLIP).unwrap();
        assert!(remux_abandonable(&video, &audio, &AtomicBool::new(true)).is_ok());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        std::fs::write(&video, CLIP).unwrap();
        assert!(remux_abandonable(&video, &audio, &AtomicBool::new(false)).is_ok());
        assert!(audio.exists(), "an owner still waiting gets its file");
    }

    #[tokio::test]
    async fn over_duration_cap_downloads_nothing() {
        let mut server = mockito::Server::new_async().await;
        let media = server.mock("GET", "/v.mp4").expect(0).with_body(CLIP).create_async().await;
        let upload = server.mock("POST", "/audio/transcriptions").expect(0).create_async().await;
        let stages = Mutex::new(Vec::new());
        let got = transcribe_here(&meta(&format!("{}/v.mp4", server.url()), Some(1801.0)), &stt(&server), &CancellationToken::new(), &|s| {
            stages.lock().unwrap().push(s)
        })
        .await;
        assert_eq!(got, Err("too long to transcribe (31 min)".to_string()));
        media.assert_async().await;
        upload.assert_async().await;
        assert!(stages.into_inner().unwrap().is_empty(), "nothing started, so nothing was reported");
    }

    #[tokio::test]
    async fn a_download_or_upload_over_its_cap_is_too_long() {
        let mut server = mockito::Server::new_async().await;
        let _big = server
            .mock("GET", "/big.mp4")
            .with_status(206)
            .with_header("content-range", &format!("bytes 0-9/{}", MAX_MEDIA_BYTES + 1))
            .with_body("0123456789")
            .create_async()
            .await;
        let _clip = server.mock("GET", "/v.mp4").with_status(200).with_body(CLIP).create_async().await;
        let upload = server.mock("POST", "/audio/transcriptions").expect(0).create_async().await;
        let (ready, cancel) = (stt(&server), CancellationToken::new());

        let got = transcribe_here(&meta(&format!("{}/big.mp4", server.url()), Some(600.0)), &ready, &cancel, &|_| {}).await;
        assert_eq!(got, Err("too long to transcribe (10 min)".to_string()));
        let got = transcribe_here(&meta(&format!("{}/big.mp4", server.url()), None), &ready, &cancel, &|_| {}).await;
        assert_eq!(got, Err("too long to transcribe".to_string()), "no duration, no number");

        // The remuxed audio is over the endpoint's limit: the remux's own duration names it.
        let tiny = SttReady { max_upload_bytes: 100, ..ready.clone() };
        let got = transcribe_here(&meta(&format!("{}/v.mp4", server.url()), None), &tiny, &cancel, &|_| {}).await;
        let reason = got.unwrap_err();
        assert!(reason.starts_with("too long to transcribe (") && reason.ends_with(" min)"), "{reason}");
        upload.assert_async().await;
    }

    /// Facebook gives no duration up front, so only the remux can tell a clip
    /// is over the cap: it is refused there, before anything is uploaded. A
    /// cap below the fixture's length stands in for a 30-minute clip.
    #[tokio::test]
    async fn a_clip_over_the_cap_by_its_remuxed_length_is_not_uploaded() {
        let mut server = mockito::Server::new_async().await;
        let media = server.mock("GET", "/v.mp4").with_status(200).with_body(CLIP).create_async().await;
        let upload = server.mock("POST", "/audio/transcriptions").expect(0).create_async().await;
        let dir = tempfile::tempdir().unwrap();
        let stages = Mutex::new(Vec::new());
        let got = transcribe_clip_in(dir.path(), ClipLimits { max_secs: 0.5, ..CLIP_LIMITS }, &meta(&format!("{}/v.mp4", server.url()), None), &stt(&server), LOOPBACK, &CancellationToken::new(), &|s| {
            stages.lock().unwrap().push(s)
        })
        .await;
        media.assert_async().await;
        upload.assert_async().await;
        assert_eq!(got, Err("too long to transcribe (1 min)".to_string()), "named by the remuxed audio's own length");
        assert_eq!(stages.into_inner().unwrap(), vec![IngestStage::Downloading], "transcription never started");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing is left behind");
    }

    #[tokio::test]
    async fn a_media_url_off_the_allowlist_is_never_requested() {
        let mut server = mockito::Server::new_async().await;
        let media = server.mock("GET", "/v.mp4").expect(0).with_body(CLIP).create_async().await;
        let mut m = meta(&format!("{}/v.mp4", server.url()), Some(5.0));
        m.media.as_mut().unwrap().allowed_host_suffixes = &["tiktokcdn.com"];
        let got = transcribe_here(&m, &stt(&server), &CancellationToken::new(), &|_| {}).await;
        assert_eq!(got, Err("TikTok offered a video from an unexpected address".to_string()));
        media.assert_async().await;

        m.media = None;
        let got = transcribe_here(&m, &stt(&server), &CancellationToken::new(), &|_| {}).await;
        assert_eq!(got, Err("TikTok did not show this clip's video".to_string()));
    }

    #[tokio::test]
    async fn an_html_answer_is_not_a_video_and_is_not_uploaded() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/v.mp4").with_status(200).with_body("<html>log in</html>").create_async().await;
        let upload = server.mock("POST", "/audio/transcriptions").expect(0).create_async().await;
        let got = transcribe_here(&meta(&format!("{}/v.mp4", server.url()), Some(5.0)), &stt(&server), &CancellationToken::new(), &|_| {}).await;
        assert_eq!(got, Err("not a video file".to_string()));
        upload.assert_async().await;
    }

    /// A bare media server: its first `stalls` connections get no answer at
    /// all (the 2026-10-19 live case — the CDN never sent headers), the rest
    /// get the clip. Returns the clip's URL and how many connections it saw.
    async fn media_server_that_stalls(stalls: usize) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>, tokio::task::JoinHandle<()>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v.mp4", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(AtomicUsize::new(0));
        let count = seen.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let n = count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    if n < stalls {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return;
                    }
                    let head = format!("HTTP/1.1 200 OK\r\ncontent-type: video/mp4\r\ncontent-length: {}\r\n\r\n", CLIP.len());
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(CLIP).await;
                    let _ = sock.flush().await;
                });
            }
        });
        (url, seen, server)
    }

    fn quick_limits() -> ClipLimits {
        ClipLimits { idle_timeout: Duration::from_secs(1), ..CLIP_LIMITS }
    }

    #[tokio::test]
    async fn a_download_that_timed_out_is_tried_once_more() {
        let (url, seen, server) = media_server_that_stalls(1).await;
        let mut stt_server = mockito::Server::new_async().await;
        let upload = stt_server.mock("POST", "/audio/transcriptions").with_status(200).with_body(r#"{"text":"said the second time"}"#).create_async().await;
        let dir = tempfile::tempdir().unwrap();
        let got = transcribe_clip_in(dir.path(), quick_limits(), &meta(&url, Some(5.0)), &stt(&stt_server), LOOPBACK, &CancellationToken::new(), &|_| {}).await;
        server.abort();
        assert_eq!(got.map(|(text, _)| text), Ok("said the second time".to_string()));
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2, "one stalled try, one retry");
        upload.assert_async().await;
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing is left behind");
    }

    #[tokio::test]
    async fn a_download_that_times_out_twice_says_so_and_uploads_nothing() {
        let (url, seen, server) = media_server_that_stalls(2).await;
        let mut stt_server = mockito::Server::new_async().await;
        let upload = stt_server.mock("POST", "/audio/transcriptions").expect(0).create_async().await;
        let dir = tempfile::tempdir().unwrap();
        let got = transcribe_clip_in(dir.path(), quick_limits(), &meta(&url, Some(5.0)), &stt(&stt_server), LOOPBACK, &CancellationToken::new(), &|_| {}).await;
        server.abort();
        assert_eq!(got, Err("TikTok didn't send the video in time (tried twice) — try again later".to_string()));
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2, "exactly one retry");
        upload.assert_async().await;
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing is left behind");
    }

    /// Cancel while the audio is at the endpoint: the `.m4a` (and the video,
    /// already gone) must not outlive the call. `download_to_file` cleans up
    /// its own failures; this phase is `TempClip`'s alone.
    #[tokio::test]
    async fn cancel_mid_upload_deletes_the_audio_file() {
        use tokio::io::AsyncReadExt;
        let mut server = mockito::Server::new_async().await;
        let _media = server.mock("GET", "/v.mp4").with_status(200).with_body(CLIP).create_async().await;
        // An endpoint that takes the upload and never answers.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (got_upload, upload_seen) = tokio::sync::oneshot::channel();
        let endpoint = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let _ = got_upload.send(());
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let ready = SttReady { base_url: format!("http://{addr}"), host: addr.to_string(), ..stt(&server) };
        let dir = tempfile::tempdir().unwrap();
        let (m, cancel) = (meta(&format!("{}/v.mp4", server.url()), Some(5.0)), CancellationToken::new());
        let run = tokio::spawn({
            let (cancel, dir) = (cancel.clone(), dir.path().to_path_buf());
            async move { transcribe_clip_in(&dir, CLIP_LIMITS, &m, &ready, LOOPBACK, &cancel, &|_| {}).await }
        });
        tokio::time::timeout(Duration::from_secs(5), upload_seen).await.expect("the upload started").unwrap();
        let during: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(during.len(), 1, "only the .m4a is kept while uploading: {during:?}");
        cancel.cancel();
        assert_eq!(run.await.unwrap(), Err(net::CANCELLED.to_string()));
        endpoint.abort();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing is left behind");
    }

    /// The scratch file is looked for where `transcribe_clip` really puts it
    /// (`cache_base()/jodd/`), and recognised by a marker only this test's
    /// server sends — other tests may have their own files there. The stalled
    /// server is a bare listener, not mockito: a mockito body that blocks keeps
    /// its pooled server busy after the guard drops, and stalls the next test
    /// that is handed it.
    #[tokio::test]
    async fn cancel_mid_download_deletes_the_temp_file() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let marker = format!("JODD-CANCEL-TEST-{}", uuid::Uuid::new_v4());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sent = marker.clone();
        let stalled = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let head = format!("HTTP/1.1 200 OK\r\ncontent-type: video/mp4\r\ncontent-length: 100000\r\n\r\n{sent}");
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
            // The rest of the clip never comes.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let mut stt_server = mockito::Server::new_async().await;
        let upload = stt_server.mock("POST", "/audio/transcriptions").expect(0).create_async().await;
        let (m, ready, cancel) = (meta(&format!("http://{addr}/slow.mp4"), Some(5.0)), stt(&stt_server), CancellationToken::new());
        let run = tokio::spawn({
            let cancel = cancel.clone();
            async move { transcribe_clip(&m, &ready, LOOPBACK, &cancel, &|_| {}).await }
        });

        let ours = async {
            loop {
                for entry in std::fs::read_dir(temp_dir()).into_iter().flatten().flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(crate::paths::INGEST_TEMP_PREFIX)
                        && name.ends_with(".mp4")
                        && std::fs::read(entry.path()).is_ok_and(|b| b.windows(marker.len()).any(|w| w == marker.as_bytes()))
                    {
                        return entry.path();
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let video = tokio::time::timeout(Duration::from_secs(5), ours).await.expect("the download reached the temp dir");
        cancel.cancel();
        let got = run.await.unwrap();
        stalled.abort();
        assert_eq!(got, Err(net::CANCELLED.to_string()));
        assert!(!video.exists(), "{} is left behind", video.display());
        assert!(!video.with_extension("m4a").exists());
        upload.assert_async().await;
    }
}
