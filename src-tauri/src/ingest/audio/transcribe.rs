//! Upload a clip's audio to the user's speech-to-text endpoint (spec §5.3).
//!
//! `POST {base_url}/audio/transcriptions`, multipart — OpenAI's shape, which a
//! cloud service and a server the user runs both speak. Every failure is a
//! short status string, never a failed ingest (URL-ingest Decision 2): the
//! caller falls back to the platform transcript or the caption.
//!
//! This request does **not** go through the ingest SSRF guard: like the HTTP
//! LLM provider, its address is the user's own choice and may be on their LAN
//! (the AI host). What stops audio going somewhere unintended is the confirmed
//! host in `SttReady` (D5), not an address filter.

use std::path::Path;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::ingest::net::{CANCELLED, REQUEST_TIMEOUT};
use crate::stt_config::SttReady;

/// A long clip's upload plus the model's run; connect still uses
/// `REQUEST_TIMEOUT` so a wrong address fails fast.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(300);

pub const REJECTED_KEY: &str = "speech-to-text rejected the key";
pub const TOO_LONG: &str = "too long to transcribe";
pub const UNREACHABLE: &str = "speech-to-text could not be reached";
pub const NO_SPEECH: &str = "no speech found";
const UNREADABLE_ANSWER: &str = "speech-to-text answered something unreadable";

/// The spec's status string for an HTTP status that was not a success.
fn status_error(status: u16) -> String {
    match status {
        401 | 403 => REJECTED_KEY.to_string(),
        413 => TOO_LONG.to_string(),
        500..=599 => UNREACHABLE.to_string(),
        n => format!("speech-to-text answered HTTP {n}"),
    }
}

/// Transcribe `audio` (an `.m4a`) with the confirmed endpoint. `Err` is a
/// status string for the source, not a fault to propagate.
pub async fn transcribe(ready: &SttReady, audio: &Path, cancel: &CancellationToken) -> Result<String, String> {
    if cancel.is_cancelled() {
        return Err(CANCELLED.to_string());
    }
    tokio::select! {
        _ = cancel.cancelled() => Err(CANCELLED.to_string()),
        r = upload(ready, audio) => r,
    }
}

async fn upload(ready: &SttReady, audio: &Path) -> Result<String, String> {
    let len = tokio::fs::metadata(audio).await.map_err(|e| format!("audio file: {e}"))?.len();
    if len > ready.max_upload_bytes {
        return Err(TOO_LONG.to_string());
    }
    let bytes = tokio::fs::read(audio).await.map_err(|e| format!("audio file: {e}"))?;

    let file = reqwest::multipart::Part::bytes(bytes)
        .file_name("audio.m4a")
        .mime_str("audio/mp4")
        .map_err(|e| e.to_string())?;
    let mut form = reqwest::multipart::Form::new()
        .part("file", file)
        .text("model", ready.model.clone())
        .text("response_format", "json");
    if let Some(language) = ready.language.as_deref().filter(|l| !l.is_empty()) {
        form = form.text("language", language.to_string());
    }

    let client = reqwest::Client::builder()
        .connect_timeout(REQUEST_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        // The audio goes to the confirmed address only: a redirect would carry
        // it, and the key, somewhere the user never confirmed.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.post(format!("{}/audio/transcriptions", ready.base_url.trim_end_matches('/'))).multipart(form);
    if let Some(key) = ready.api_key.as_deref() {
        req = req.bearer_auth(key);
    }
    // Neither the URL nor the response body goes into an error: the first can
    // carry a key in a proxy's path, the second is whatever the server said.
    let resp = req.send().await.map_err(|_| UNREACHABLE.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(status_error(status.as_u16()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|_| UNREADABLE_ANSWER.to_string())?;
    let text = body.get("text").and_then(|t| t.as_str()).ok_or_else(|| UNREADABLE_ANSWER.to_string())?.trim();
    if text.is_empty() {
        return Err(NO_SPEECH.to_string());
    }
    Ok(text.to_string())
}

/// The Settings **Test** button: transcribe the bundled clip. It goes through a
/// real file under `dir` (prefix `ingest-`, so the startup sweep cleans one
/// a crash left) because `transcribe` takes a path, as the pipeline's does; the
/// file is deleted on every exit.
pub async fn test_clip(ready: &SttReady, dir: &Path) -> Result<String, String> {
    tokio::fs::create_dir_all(dir).await.map_err(|e| format!("cache dir: {e}"))?;
    let path = dir.join(format!("{}stt-test-{}.m4a", crate::paths::INGEST_TEMP_PREFIX, uuid::Uuid::new_v4()));
    tokio::fs::write(&path, crate::stt_config::TEST_CLIP).await.map_err(|e| format!("cache dir: {e}"))?;
    let result = transcribe(ready, &path, &CancellationToken::new()).await;
    let _ = tokio::fs::remove_file(&path).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    fn ready(base_url: &str) -> SttReady {
        SttReady {
            preset: crate::stt_config::SttPreset::Custom,
            base_url: base_url.to_string(),
            model: "whisper-1".into(),
            language: None,
            max_upload_bytes: 25 * 1024 * 1024,
            api_key: None,
            host: "127.0.0.1".into(),
        }
    }

    fn clip() -> tempfile::NamedTempFile {
        let f = tempfile::Builder::new().suffix(".m4a").tempfile().unwrap();
        std::fs::write(f.path(), crate::stt_config::TEST_CLIP).unwrap();
        f
    }

    #[tokio::test]
    async fn transcribe_posts_multipart_without_a_language_by_default() {
        let mut server = mockito::Server::new_async().await;
        let with_language = server
            .mock("POST", "/audio/transcriptions")
            .match_body(Matcher::Regex(r#"name="language""#.into()))
            .with_status(200)
            .with_body(r#"{"text":"wrong"}"#)
            .expect(0)
            .create_async()
            .await;
        let ok = server
            .mock("POST", "/audio/transcriptions")
            .match_header("content-type", Matcher::Regex("^multipart/form-data; boundary=".into()))
            .match_body(Matcher::Regex(r#"name="file"; filename="audio.m4a""#.into()))
            .match_body(Matcher::Regex(r#"(?i)content-type: audio/mp4"#.into()))
            .match_body(Matcher::Regex(r#"name="model"\r\n\r\nwhisper-1"#.into()))
            .match_body(Matcher::Regex(r#"name="response_format"\r\n\r\njson"#.into()))
            .with_status(200)
            .with_body(r#"{"text":" สวัสดีครับ this is a test "}"#)
            .expect(1)
            .create_async()
            .await;
        let f = clip();
        let text = transcribe(&ready(&server.url()), f.path(), &CancellationToken::new()).await.unwrap();
        assert_eq!(text, "สวัสดีครับ this is a test");
        ok.assert_async().await;
        with_language.assert_async().await;
    }

    #[tokio::test]
    async fn transcribe_sends_the_language_and_the_key_when_set() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", "/audio/transcriptions")
            .match_header("authorization", "Bearer sekret")
            .match_body(Matcher::Regex(r#"name="language"\r\n\r\nth"#.into()))
            .with_status(200)
            .with_body(r#"{"text":"ok"}"#)
            .create_async()
            .await;
        let mut r = ready(&server.url());
        r.language = Some("th".into());
        r.api_key = Some("sekret".into());
        let f = clip();
        assert_eq!(transcribe(&r, f.path(), &CancellationToken::new()).await.unwrap(), "ok");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn transcribe_maps_errors_to_the_spec_strings() {
        let f = clip();
        let cancel = CancellationToken::new();
        let mut server = mockito::Server::new_async().await;
        for (status, want) in [
            (401, "speech-to-text rejected the key"),
            (403, "speech-to-text rejected the key"),
            (413, "too long to transcribe"),
            (500, "speech-to-text could not be reached"),
            (503, "speech-to-text could not be reached"),
            (404, "speech-to-text answered HTTP 404"),
            (302, "speech-to-text answered HTTP 302"),
            (429, "speech-to-text answered HTTP 429"),
        ] {
            let m = server.mock("POST", "/audio/transcriptions").with_status(status).with_body("denied: sekret").create_async().await;
            assert_eq!(transcribe(&ready(&server.url()), f.path(), &cancel).await.unwrap_err(), want, "status {status}");
            m.remove_async().await;
        }

        // Nothing spoken, or nothing but whitespace.
        for body in [r#"{"text":""}"#, r#"{"text":"  \n"}"#] {
            let m = server.mock("POST", "/audio/transcriptions").with_status(200).with_body(body).create_async().await;
            assert_eq!(transcribe(&ready(&server.url()), f.path(), &cancel).await.unwrap_err(), "no speech found");
            m.remove_async().await;
        }

        // A 200 that is not the endpoint's JSON (a captive portal, a proxy page).
        let m = server.mock("POST", "/audio/transcriptions").with_status(200).with_body("<html>sign in</html>").create_async().await;
        assert_eq!(
            transcribe(&ready(&server.url()), f.path(), &cancel).await.unwrap_err(),
            "speech-to-text answered something unreadable"
        );
        m.remove_async().await;

        // Connection refused: bind a port, then free it.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert_eq!(
            transcribe(&ready(&format!("http://127.0.0.1:{port}")), f.path(), &cancel).await.unwrap_err(),
            "speech-to-text could not be reached"
        );
    }

    #[tokio::test]
    async fn transcribe_refuses_a_file_over_the_endpoint_limit_without_sending() {
        let mut server = mockito::Server::new_async().await;
        let m = server.mock("POST", "/audio/transcriptions").expect(0).create_async().await;
        let mut r = ready(&server.url());
        r.max_upload_bytes = 100;
        let f = clip();
        assert_eq!(transcribe(&r, f.path(), &CancellationToken::new()).await.unwrap_err(), "too long to transcribe");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn transcribe_races_cancel() {
        // A server that accepts and never answers.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = listener.accept().await {
                held.push(s);
            }
        });
        let f = clip();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c2.cancel();
        });
        let started = std::time::Instant::now();
        let err = transcribe(&ready(&format!("http://127.0.0.1:{port}")), f.path(), &cancel).await.unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_cancel_before_the_call_sends_nothing() {
        let mut server = mockito::Server::new_async().await;
        let m = server.mock("POST", "/audio/transcriptions").expect(0).create_async().await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let f = clip();
        assert_eq!(transcribe(&ready(&server.url()), f.path(), &cancel).await.unwrap_err(), "cancelled");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn test_clip_uploads_the_bundled_clip_and_leaves_no_file() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", "/audio/transcriptions")
            .match_body(Matcher::Regex(r#"name="file"; filename="audio.m4a""#.into()))
            .with_status(200)
            .with_body(r#"{"text":"hello"}"#)
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("jodd");
        assert_eq!(test_clip(&ready(&server.url()), &scratch).await.unwrap(), "hello");
        m.assert_async().await;
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);

        // A failure cleans up too.
        let m = server.mock("POST", "/audio/transcriptions").with_status(401).create_async().await;
        assert_eq!(test_clip(&ready(&server.url()), &scratch).await.unwrap_err(), REJECTED_KEY);
        m.remove_async().await;
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    }
}
