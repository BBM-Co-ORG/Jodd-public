//! HTTP provider — any OpenAI-compatible chat-completions endpoint.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::llm::prompt::SYSTEM_PROMPT;
use crate::llm::provider::{ChatRole, ChatTurn, ExtractEnvelope, ExtractError, LlmProvider};
use tokio_util::sync::CancellationToken;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
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

impl HttpProvider {
    async fn send_once(
        &self,
        source: &str,
        cancel: &tokio_util::sync::CancellationToken,
        include_response_format: bool,
    ) -> Result<reqwest::Response, ExtractError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let req_body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatRequestMessage { role: "system", content: SYSTEM_PROMPT },
                ChatRequestMessage { role: "user", content: source },
            ],
            temperature: 0.2,
            response_format: include_response_format
                .then_some(ResponseFormat { fmt_type: "json_object" }),
        };

        let mut req = self.client.post(&url).json(&req_body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        // Race the HTTP send against the cancellation token. Dropping the
        // send future cancels the in-flight reqwest connection cleanly.
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ExtractError::Cancelled),
            r = req.send() => r.map_err(|e| ExtractError::Transport(e.to_string())),
        }
    }
}

#[derive(Serialize)]
struct LinkSuggestionRequestBody<'a> {
    new_text: &'a str,
    candidates: &'a [crate::llm::provider::CandidateSummary],
}

impl HttpProvider {
    async fn send_link_suggestion_request(
        &self,
        source: &str,
        candidates: &[crate::llm::provider::CandidateSummary],
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<reqwest::Response, ExtractError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let user_content = serde_json::to_string(&LinkSuggestionRequestBody {
            new_text: source,
            candidates,
        })
        .map_err(|e| ExtractError::Transport(format!("serialize candidates: {e}")))?;
        let req_body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatRequestMessage {
                    role: "system",
                    content: crate::llm::prompt::LINK_SUGGESTION_SYSTEM_PROMPT,
                },
                ChatRequestMessage { role: "user", content: &user_content },
            ],
            temperature: 0.2,
            response_format: Some(ResponseFormat { fmt_type: "json_object" }),
        };

        let mut req = self.client.post(&url).json(&req_body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ExtractError::Cancelled),
            r = req.send() => r.map_err(|e| ExtractError::Transport(e.to_string())),
        }
    }

    /// Extract the assistant's text from a chat-completions API response.
    fn first_choice_content(raw: &str) -> Result<String, ExtractError> {
        let chat: ChatResponse = serde_json::from_str(raw).map_err(|e| ExtractError::MalformedEnvelope {
            reason: format!("chat envelope: {e}"),
            raw: raw.to_string(),
        })?;

        chat.choices
            .into_iter()
            .next()
            .ok_or_else(|| ExtractError::MalformedEnvelope {
                reason: "no choices".into(),
                raw: raw.to_string(),
            })
            .map(|choice| choice.message.content)
    }
}

#[async_trait::async_trait]
impl LlmProvider for HttpProvider {
    async fn extract(
        &self,
        source: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let mut resp = self.send_once(source, &cancel, true).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();

            // Not every "OpenAI-compatible" gateway supports response_format
            // — some (e.g. kilo.ai) reject the whole request with 400 rather
            // than ignoring the field. The system prompt already enforces
            // raw-JSON-only output, so retry once without it before giving up.
            if status == reqwest::StatusCode::BAD_REQUEST && body.contains("response_format") {
                resp = self.send_once(source, &cancel, false).await?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ExtractError::UpstreamError(format!(
                        "HTTP {status}: {body}"
                    )));
                }
            } else {
                return Err(ExtractError::UpstreamError(format!(
                    "HTTP {status}: {body}"
                )));
            }
        }

        let text = resp
            .text()
            .await
            .map_err(|e| ExtractError::Transport(e.to_string()))?;

        let raw = Self::first_choice_content(&text)?;

        serde_json::from_str::<ExtractEnvelope>(&raw).map_err(|e| {
            ExtractError::MalformedEnvelope {
                reason: format!("inner json: {e}"),
                raw,
            }
        })
    }

    async fn suggest_links(
        &self,
        source: &str,
        candidates: &[crate::llm::provider::CandidateSummary],
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::llm::provider::LinkSuggestionsEnvelope, ExtractError> {
        let resp = self.send_link_suggestion_request(source, candidates, &cancel).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ExtractError::UpstreamError(format!("HTTP {status}: {body}")));
        }

        let text = resp
            .text()
            .await
            .map_err(|e| ExtractError::Transport(e.to_string()))?;

        let raw = Self::first_choice_content(&text)?;

        serde_json::from_str::<crate::llm::provider::LinkSuggestionsEnvelope>(&raw).map_err(|e| {
            ExtractError::MalformedEnvelope {
                reason: format!("inner json: {e}"),
                raw,
            }
        })
    }

    async fn chat(
        &self,
        system: &str,
        turns: &[ChatTurn],
        cancel: CancellationToken,
    ) -> Result<String, ExtractError> {
        let mut messages: Vec<ChatRequestMessage> = Vec::with_capacity(turns.len() + 1);
        messages.push(ChatRequestMessage { role: "system", content: system });
        for t in turns {
            messages.push(ChatRequestMessage {
                role: match t.role {
                    ChatRole::User => "user",
                    ChatRole::Assistant => "assistant",
                },
                content: &t.content,
            });
        }

        let body = ChatRequest {
            model: &self.model,
            messages,
            // Free text: no JSON mode, so no gateway-rejects-response_format
            // retry is needed here (contrast `extract`).
            response_format: None,
            temperature: 0.2,
        };

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut req = self.client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let req = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ExtractError::Cancelled),
            r = req.send() => r,
        }
        .map_err(|e| ExtractError::Transport(e.to_string()))?;

        let status = req.status();
        let text = req
            .text()
            .await
            .map_err(|e| ExtractError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(ExtractError::UpstreamError(format!("{status}: {text}")));
        }
        Self::first_choice_content(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::{Matcher, Server};
    use serde_json::json;

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
    async fn retries_without_response_format_when_gateway_rejects_it() {
        let mut server = Server::new_async().await;

        // First attempt includes response_format — gateway 400s naming the
        // rejected param (this is the exact shape kilo.ai's gateway returns).
        let first = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::PartialJson(json!({
                "response_format": { "type": "json_object" }
            })))
            .with_status(400)
            .with_body(r#"{"error":{"message":"Invalid input","type":"invalid_request_error","param":"response_format","code":"invalid_request_error"}}"#)
            .create_async()
            .await;

        // Retry omits response_format entirely — gateway accepts it.
        let inner = "{\"title\":\"Test\",\"lessons_markdown\":\"## L1\\nbody\",\"tags\":[\"a\"]}";
        let body = format!(
            r#"{{"choices": [{{"message": {{"content": {}}}}}]}}"#,
            serde_json::to_string(inner).unwrap()
        );
        let retry = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::Json(json!({
                "model": "test-model",
                "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": "source"}
                ],
                "temperature": 0.2
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let p = provider_for(&server.url());
        let env = p
            .extract("source", tokio_util::sync::CancellationToken::new())
            .await
            .expect("ok after retry without response_format");
        assert_eq!(env.title.as_deref(), Some("Test"));
        first.assert_async().await;
        retry.assert_async().await;
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

    #[tokio::test]
    async fn suggest_links_success_path_parses_envelope() {
        let mut server = Server::new_async().await;
        let inner = r#"{"suggestions":[{"uuid":"AAAA","related":true,"should_append":true,"addition_text":"See [[new-note-slug]]."}]}"#;
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
        let candidates = vec![crate::llm::provider::CandidateSummary {
            uuid: "AAAA".to_string(),
            title: "Test".to_string(),
            snippet: "snippet".to_string(),
        }];
        let env = p
            .suggest_links("new text", &candidates, tokio_util::sync::CancellationToken::new())
            .await
            .expect("ok");
        assert_eq!(env.suggestions.len(), 1);
        assert!(env.suggestions[0].related);
        assert_eq!(env.suggestions[0].addition_text.as_deref(), Some("See [[new-note-slug]]."));
    }

    #[tokio::test]
    async fn chat_sends_every_turn_and_no_response_format() {
        let mut server = Server::new_async().await;
        let m = server
            .mock("POST", "/chat/completions")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::PartialJson(serde_json::json!({
                    "messages": [
                        { "role": "system",    "content": "SYS" },
                        { "role": "user",      "content": "q1" },
                        { "role": "assistant", "content": "a1" },
                        { "role": "user",      "content": "q2" }
                    ]
                })),
            ]))
            .with_status(200)
            .with_body(r#"{"choices":[{"message":{"content":"the answer"}}]}"#)
            .create_async()
            .await;

        let p = HttpProvider::new(
            server.url(),
            "test-model".into(),
            None,
            std::time::Duration::from_secs(5),
        )
        .unwrap();

        let turns = vec![
            ChatTurn { role: ChatRole::User, content: "q1".into() },
            ChatTurn { role: ChatRole::Assistant, content: "a1".into() },
            ChatTurn { role: ChatRole::User, content: "q2".into() },
        ];
        let got = p.chat("SYS", &turns, CancellationToken::new()).await.unwrap();
        assert_eq!(got, "the answer");
        m.assert_async().await;
    }
}
