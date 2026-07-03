//! HTTP provider — any OpenAI-compatible chat-completions endpoint.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::lessons::prompt::SYSTEM_PROMPT;
use crate::lessons::provider::{ExtractEnvelope, ExtractError, LessonProvider};

pub struct HttpProvider {
    base_url: String,
    model: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl HttpProvider {
    pub fn new(
        base_url: String,
        model: String,
        api_key: Option<String>,
        timeout: Duration,
    ) -> Result<Self, ExtractError> {
        // Normalize empty/whitespace-only api_key to None so we don't send
        // `Authorization: Bearer ` (empty credential), which some upstreams
        // 401 on with confusing errors.
        let api_key = api_key.filter(|k| !k.trim().is_empty());
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ExtractError::Transport(e.to_string()))?;
        Ok(Self {
            base_url,
            model,
            api_key,
            client,
        })
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatRequestMessage<'a>>,
    temperature: f32,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct ChatRequestMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    fmt_type: &'static str,
}

#[async_trait::async_trait]
impl LessonProvider for HttpProvider {
    async fn extract(
        &self,
        source: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let req_body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatRequestMessage { role: "system", content: SYSTEM_PROMPT },
                ChatRequestMessage { role: "user", content: source },
            ],
            temperature: 0.2,
            response_format: ResponseFormat { fmt_type: "json_object" },
        };

        let mut req = self.client.post(&url).json(&req_body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        // Race the HTTP send against the cancellation token. Dropping the
        // send future cancels the in-flight reqwest connection cleanly.
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ExtractError::Cancelled),
            r = req.send() => r.map_err(|e| ExtractError::Transport(e.to_string()))?,
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ExtractError::UpstreamError(format!(
                "HTTP {status}: {body}"
            )));
        }

        let chat: ChatResponse = resp
            .json()
            .await
            .map_err(|e| ExtractError::MalformedEnvelope {
                reason: format!("chat envelope: {e}"),
                raw: String::new(),
            })?;

        let raw = chat
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ExtractError::MalformedEnvelope {
                reason: "no choices".into(),
                raw: String::new(),
            })?
            .message
            .content;

        serde_json::from_str::<ExtractEnvelope>(&raw).map_err(|e| {
            ExtractError::MalformedEnvelope {
                reason: format!("inner json: {e}"),
                raw,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn provider_for(url: &str) -> HttpProvider {
        HttpProvider::new(
            url.to_string(),
            "test-model".into(),
            Some("test-key".into()),
            Duration::from_secs(5),
        )
        .expect("build provider")
    }

    #[tokio::test]
    async fn success_path_parses_envelope() {
        let mut server = Server::new_async().await;
        let inner = "{\"title\":\"Test\",\"lessons_markdown\":\"## L1\\nbody\",\"tags\":[\"a\"]}";
        let body = format!(
            r#"{{"choices": [{{"message": {{"content": {}}}}}]}}"#,
            serde_json::to_string(inner).unwrap()
        );
        let _m = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let p = provider_for(&server.url());
        let env = p.extract("source", tokio_util::sync::CancellationToken::new()).await.expect("ok");
        assert_eq!(env.title.as_deref(), Some("Test"));
        assert_eq!(env.tags, vec!["a"]);
    }

    #[tokio::test]
    async fn http_error_becomes_upstream_error() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/chat/completions")
            .with_status(429)
            .with_body(r#"{"error":"rate_limit"}"#)
            .create_async()
            .await;

        let p = provider_for(&server.url());
        let err = p.extract("source", tokio_util::sync::CancellationToken::new()).await.expect_err("expected error");
        match err {
            ExtractError::UpstreamError(msg) => assert!(msg.contains("429")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn pre_cancelled_token_short_circuits_to_cancelled() {
        // If the token is already cancelled before extract starts, the
        // tokio::select! `biased` branch should fire immediately and return
        // Cancelled WITHOUT making the HTTP request. mockito.expect(0)
        // proves no request hit the server.
        let mut server = Server::new_async().await;
        let m = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_body(r#"{"choices": [{"message": {"content": "{}"}}]}"#)
            .expect(0)
            .create_async()
            .await;

        let p = provider_for(&server.url());
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let err = p.extract("source", token).await.expect_err("expected cancel");
        assert!(
            matches!(err, ExtractError::Cancelled),
            "expected Cancelled, got: {err:?}"
        );
        m.assert_async().await;
    }

    #[tokio::test]
    async fn malformed_inner_json_becomes_malformed_envelope() {
        let mut server = Server::new_async().await;
        let body = r#"{"choices": [{"message": {"content": "not json"}}]}"#;
        let _m = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let p = provider_for(&server.url());
        let err = p.extract("source", tokio_util::sync::CancellationToken::new()).await.expect_err("expected error");
        assert!(
            matches!(err, ExtractError::MalformedEnvelope { .. }),
            "got: {err:?}"
        );
    }
}
