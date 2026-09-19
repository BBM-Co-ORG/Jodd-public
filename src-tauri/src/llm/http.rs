//! HTTP provider — any OpenAI-compatible chat-completions endpoint.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::llm::prompt::{extract_system_prompt, SYSTEM_PROMPT};
use crate::llm::provider::{
    parse_envelope_lenient, ChatRole, ChatTurn, ExtractEnvelope, ExtractError, LlmProvider,
};
use tokio_util::sync::CancellationToken;

/// What to put in `ExtractError::Transport` for a reqwest failure.
///
/// `reqwest::Error`'s own `Display` names the request, never the fault:
/// "error sending request for url (…)" is byte-identical for a refused
/// connection, a DNS miss, a timeout and a TLS fault. The half a user can act
/// on lives in the `source()` chain, which `to_string()` discards — measured
/// 2026-09-04 against a closed port, where the chain read `client error
/// (Connect)` → `tcp connect error` → `Connection refused (os error 61)`.
///
/// Only the ROOT is appended. The middle links name reqwest's internal layers
/// and would push the useful sentence off the end of a settings-dialog error
/// box for no gain.
fn transport_message(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();

    let mut cursor = std::error::Error::source(e);
    let mut root = None;
    while let Some(src) = cursor {
        root = Some(src);
        cursor = src.source();
    }
    if let Some(cause) = root {
        msg.push_str(&format!(": {cause}"));
    }

    // The one class of transport failure the user can fix from the dialog
    // they are looking at. Everything else (DNS, TLS, timeout) is left to the
    // root cause alone rather than guessed at.
    if e.is_connect() {
        msg.push_str(" — nothing is listening there; check the Base URL and that the server is running.");
    }

    msg
}

pub struct HttpProvider {
    base_url: String,
    model: String,
    api_key: Option<String>,
    disable_thinking: bool,
    client: reqwest::Client,
}

impl HttpProvider {
    pub fn new(
        base_url: String,
        model: String,
        api_key: Option<String>,
        disable_thinking: bool,
        timeout: Duration,
    ) -> Result<Self, ExtractError> {
        // Normalize empty/whitespace-only api_key to None so we don't send
        // `Authorization: Bearer ` (empty credential), which some upstreams
        // 401 on with confusing errors.
        let api_key = api_key.filter(|k| !k.trim().is_empty());
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ExtractError::Transport(transport_message(&e)))?;
        Ok(Self {
            base_url,
            model,
            api_key,
            disable_thinking,
            client,
        })
    }

    /// The llama.cpp knob that suppresses Qwen3-style reasoning, or `None` when
    /// the account did not ask for it. `None` makes `skip_serializing_if` drop
    /// the key from the body entirely, which is what keeps hosted providers'
    /// requests byte-for-byte unchanged — some 400 on unknown body params.
    ///
    /// All three request builders go through this: `send_once` (extract),
    /// `send_json_request` (auto-link, folder suggestion) and `chat` (Ask Jodd).
    /// `chat` matters most of the three: it is the longest generation, so it is
    /// where the thinking overhead is most likely to hit the timeout.
    fn thinking_kwargs(&self) -> Option<serde_json::Value> {
        self.disable_thinking
            .then(|| serde_json::json!({ "enable_thinking": false }))
    }
}

// Buffer within the attempt so duration includes response-body receipt and
// cancellation covers both headers and body. Existing parsers stay unchanged.
struct BufferedResponse { status: reqwest::StatusCode, body: super::receipts::AiResult<String> }
impl BufferedResponse {
    fn status(&self) -> reqwest::StatusCode { self.status }
    async fn text(self) -> Result<String, reqwest::Error> { Ok(self.body.value) }
}
impl HttpProvider {
    async fn dispatch(&self, req: reqwest::RequestBuilder, cancel: &CancellationToken, ignore_error_body_failure: bool) -> Result<BufferedResponse, ExtractError> {
        if cancel.is_cancelled() { return Err(ExtractError::Cancelled); }
        // Account the complete serialized payload, including history and catalogs.
        let mut request = req.build().map_err(|e| ExtractError::Transport(transport_message(&e)))?;
        if let Some((parameter, limit)) = super::budget::output_limit() {
            let name = match parameter {
                super::budget::OutputParameter::MaxTokens => Some("max_tokens"),
                super::budget::OutputParameter::MaxCompletionTokens => Some("max_completion_tokens"),
                super::budget::OutputParameter::Unsupported => None,
            };
            if let Some(name) = name {
                let bytes = request.body().and_then(|b| b.as_bytes()).ok_or_else(|| ExtractError::Transport("AI request cannot be budgeted".into()))?;
                let mut json: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ExtractError::Transport("AI request cannot be budgeted".into()))?;
                json[name] = limit.into();
                *request.body_mut() = Some(serde_json::to_vec(&json).map_err(|_| ExtractError::Transport("AI request cannot be budgeted".into()))?.into());
            }
        }
        let input_bytes = request.body().and_then(|b| b.as_bytes()).map(|b| b.len()).ok_or_else(|| ExtractError::Transport("AI request cannot be budgeted".into()))?;
        let mut budget = super::budget::Attempt::start(input_bytes)?;
        let mut attempt = super::receipts::Attempt::start("http", Some(&self.model));
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ExtractError::Cancelled),
            result = async {
                let response = self.client.execute(request).await.map_err(|e| ExtractError::Transport(transport_message(&e)))?;
                let status = response.status();
                let body = match response.text().await {
                    Ok(body) => body,
                    Err(_) if ignore_error_body_failure && !status.is_success() => String::new(),
                    Err(e) => return Err(ExtractError::Transport(transport_message(&e))),
                };
                let usage = super::receipts::Usage::from_response(&body);
                Ok(BufferedResponse { status, body: super::receipts::AiResult { value: body, usage } })
            } => result,
        };
        attempt.finish(&result);
        if let Ok(response) = &result {
            budget.reconcile(&response.body.usage);
            attempt.record_usage(response.body.usage.clone());
            if !response.status.is_success() {
                attempt.finish::<()>(&Err(ExtractError::UpstreamError(String::new())));
            }
        }
        result
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
    /// llama.cpp-specific. See `HttpProvider::thinking_kwargs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<serde_json::Value>,
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
        system: &str,
        source: &str,
        cancel: &tokio_util::sync::CancellationToken,
        include_response_format: bool,
    ) -> Result<BufferedResponse, ExtractError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let req_body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatRequestMessage { role: "system", content: system },
                ChatRequestMessage { role: "user", content: source },
            ],
            temperature: 0.2,
            response_format: include_response_format
                .then_some(ResponseFormat { fmt_type: "json_object" }),
            chat_template_kwargs: self.thinking_kwargs(),
        };

        let mut req = self.client.post(&url).json(&req_body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        // Race the HTTP send against the cancellation token. Dropping the
        // send future cancels the in-flight reqwest connection cleanly.
        self.dispatch(req, cancel, true).await
    }
}

#[derive(Serialize)]
struct LinkSuggestionRequestBody<'a> {
    new_text: &'a str,
    candidates: &'a [crate::llm::provider::CandidateSummary],
}

impl HttpProvider {
    /// One chat-completions request whose answer is a JSON envelope — the
    /// shape `suggest_links` and `suggest_folder` share. `extract` keeps its
    /// own `send_once` because it alone retries without `response_format`
    /// when a gateway rejects the field.
    async fn send_json_request(
        &self,
        system: &str,
        user_content: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<BufferedResponse, ExtractError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let req_body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatRequestMessage { role: "system", content: system },
                ChatRequestMessage { role: "user", content: user_content },
            ],
            temperature: 0.2,
            response_format: Some(ResponseFormat { fmt_type: "json_object" }),
            chat_template_kwargs: self.thinking_kwargs(),
        };

        let mut req = self.client.post(&url).json(&req_body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        self.dispatch(req, cancel, true).await
    }

    /// `send_json_request`, then the status check and the LENIENT parse
    /// (gotcha #7b) — so every envelope workflow on this provider fails and
    /// recovers the same way.
    async fn post_json_envelope<T: serde::de::DeserializeOwned + std::fmt::Debug>(
        &self,
        system: &str,
        user_content: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<T, ExtractError> {
        let resp = self.send_json_request(system, user_content, cancel).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ExtractError::UpstreamError(format!("HTTP {status}: {body}")));
        }

        let text = resp
            .text()
            .await
            .map_err(|e| ExtractError::Transport(transport_message(&e)))?;
        let raw = Self::first_choice_content(&text)?;

        // Lenient for the same reason `extract` is — see the note there.
        parse_envelope_lenient::<T>(&raw).map_err(|reason| ExtractError::MalformedEnvelope { reason, raw })
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
        existing_tags: &[String],
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let system = extract_system_prompt(existing_tags);
        let mut resp = self.send_once(&system, source, &cancel, true).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();

            // Not every "OpenAI-compatible" gateway supports response_format
            // — some (e.g. kilo.ai) reject the whole request with 400 rather
            // than ignoring the field. The system prompt already enforces
            // raw-JSON-only output, so retry once without it before giving up.
            if status == reqwest::StatusCode::BAD_REQUEST && body.contains("response_format") {
                super::receipts::check("retry_without_response_format");
                resp = self.send_once(&system, source, &cancel, false).await?;
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
            .map_err(|e| ExtractError::Transport(transport_message(&e)))?;

        let raw = Self::first_choice_content(&text)?;

        // Lenient, not strict, and the same parser the agent-CLI provider
        // uses. An endpoint that merely *accepts* `response_format` without
        // enforcing it answers with a markdown-fenced object; strict parsing
        // threw away answers that were correct in every way but the wrapper.
        // See the fenced-envelope test for the live measurement.
        parse_envelope_lenient::<ExtractEnvelope>(&raw).map_err(|reason| {
            ExtractError::MalformedEnvelope { reason, raw }
        })
    }

    async fn run_workflow(
        &self,
        workflow: crate::llm::provider::WorkflowKind,
        source: &str,
        existing_tags: &[String],
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let meeting_request = if workflow == crate::llm::provider::WorkflowKind::ActionItems {
            Some(super::meeting::request(source)?)
        } else { None };
        let payload = meeting_request.as_deref().unwrap_or(source);
        let system = crate::llm::prompt::workflow_system_prompt(workflow, existing_tags);
        let mut resp = self.send_once(&system, payload, &cancel, true).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();

            // Same gateway-compat retry as `extract` — see its own comment.
            if status == reqwest::StatusCode::BAD_REQUEST && body.contains("response_format") {
                super::receipts::check("retry_without_response_format");
                resp = self.send_once(&system, payload, &cancel, false).await?;
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
            .map_err(|e| ExtractError::Transport(transport_message(&e)))?;

        let raw = Self::first_choice_content(&text)?;

        if workflow == crate::llm::provider::WorkflowKind::ActionItems {
            let result = super::meeting::parse(&raw, source)?;
            super::receipts::check("meeting_quotes_validated_not_entailment");
            return super::meeting::envelope(&result, source);
        }
        // Lenient for the same reason `extract` is (gotcha #7b).
        parse_envelope_lenient::<ExtractEnvelope>(&raw).map_err(|reason| {
            ExtractError::MalformedEnvelope { reason, raw }
        })
    }

    async fn suggest_links(
        &self,
        source: &str,
        candidates: &[crate::llm::provider::CandidateSummary],
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::llm::provider::LinkSuggestionsEnvelope, ExtractError> {
        let user_content = serde_json::to_string(&LinkSuggestionRequestBody {
            new_text: source,
            candidates,
        })
        .map_err(|e| ExtractError::Transport(format!("serialize candidates: {e}")))?;
        self.post_json_envelope(
            crate::llm::prompt::LINK_SUGGESTION_SYSTEM_PROMPT,
            &user_content,
            &cancel,
        )
        .await
    }

    async fn suggest_folder(
        &self,
        note_text: &str,
        folders: &[String],
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::llm::provider::FolderSuggestionEnvelope, ExtractError> {
        let user_content = crate::llm::provider::folder_suggestion_request_json(note_text, folders);
        self.post_json_envelope(
            crate::llm::prompt::FOLDER_SUGGESTION_SYSTEM_PROMPT,
            &user_content,
            &cancel,
        )
        .await
    }

    async fn synthesize(
        &self,
        digests: &[crate::llm::provider::SourceDigest],
        context: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        // `post_json_envelope` = `send_json_request` + status check + the
        // lenient parse (gotcha #7b), as `suggest_links`/`suggest_folder` do.
        let user_content = crate::llm::provider::synthesis_request_json(digests, context);
        self.post_json_envelope(crate::llm::prompt::SYNTHESIS_SYSTEM_PROMPT, &user_content, &cancel)
            .await
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
            chat_template_kwargs: self.thinking_kwargs(),
        };

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut req = self.client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let req = self.dispatch(req, &cancel, false).await?;

        let status = req.status();
        let text = req
            .text()
            .await
            .map_err(|e| ExtractError::Transport(transport_message(&e)))?;
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
        provider_with_thinking(url, false)
    }

    fn provider_with_thinking(url: &str, disable_thinking: bool) -> HttpProvider {
        HttpProvider::new(
            url.to_string(),
            "test-model".into(),
            Some("test-key".into()),
            disable_thinking,
            Duration::from_secs(5),
        )
        .expect("build provider")
    }

    /// An "OpenAI-compatible" endpoint that accepts `response_format`
    /// without ENFORCING it hands back a markdown-fenced envelope. Measured
    /// live against `thclaws --serve` 0.118.0 on 2026-09-04: 4 runs out of 4,
    /// across two model ids, wrapped the object in ```json despite Jodd
    /// sending `response_format: {"type":"json_object"}` AND despite
    /// SYSTEM_PROMPT spelling out "Do not wrap your response in a markdown
    /// code fence". This is not thClaws-specific — Ollama, LM Studio and
    /// llama.cpp all take the field and ignore it.
    ///
    /// The agent-CLI sibling has stripped fences since v0.16.1
    /// (`parse_envelope_lenient`); this provider parsed strictly and turned
    /// a perfectly good answer into MalformedEnvelope.
    #[tokio::test]
    async fn a_fenced_envelope_from_an_endpoint_that_ignores_response_format_still_parses() {
        let mut server = Server::new_async().await;
        let inner = "```json\n{\"title\":\"Fenced\",\"lessons_markdown\":\"## L1\\nbody\",\"tags\":[\"a\"]}\n```";
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
        let env = p
            .extract("source", &[], tokio_util::sync::CancellationToken::new())
            .await
            .expect("a fenced envelope must still parse");
        assert_eq!(env.title.as_deref(), Some("Fenced"));
        assert_eq!(env.lessons_markdown, "## L1\nbody");
    }

    /// reqwest's `Display` is deliberately generic — "error sending request
    /// for url (…)" is what a DNS failure, a timeout, a TLS fault and a
    /// refused connection all say, identically. The half a user can act on
    /// sits three levels down a `source()` chain that `to_string()` drops.
    ///
    /// Measured 2026-09-04 against a closed port: Display said only "error
    /// sending request for url (…)", while the chain read `client error
    /// (Connect)` → `tcp connect error` → **`Connection refused (os error
    /// 61)`**. A user who had typed the wrong port saw the same sentence as
    /// a user whose server was down and as a user with a bad URL, and
    /// reported all three as one bug — correctly, given what they were shown.
    #[tokio::test]
    async fn a_refused_connection_says_so_instead_of_just_error_sending_request() {
        // Bind to get a port the OS says is free, then drop the listener so
        // nothing is listening on it. Hardcoding a port makes the test a
        // hostage to whatever else the machine happens to be running.
        let dead_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr").port()
        };

        let p = provider_for(&format!("http://127.0.0.1:{dead_port}/v1"));
        let err = p
            .extract("source", &[], tokio_util::sync::CancellationToken::new())
            .await
            .expect_err("a closed port must fail");

        let msg = err.to_string();
        assert!(
            msg.contains("Connection refused"),
            "the root cause must survive into the message the user reads; got: {msg}"
        );
        assert!(
            msg.contains("nothing is listening"),
            "a refused connection is user-fixable, so say what to do; got: {msg}"
        );
    }

    /// One request per `WorkflowKind`, asserting the request body carries
    /// that workflow's own prompt (not a shared generic one) and that the
    /// envelope in the response still parses. Marker text is a distinctive
    /// substring of each workflow's system prompt in `prompt.rs`.
    #[tokio::test]
    async fn run_workflow_sends_the_matching_prompt_and_parses_the_envelope() {
        use crate::llm::provider::WorkflowKind;

        let cases = [
            (WorkflowKind::Summarize, "short, faithful overview"),
            (WorkflowKind::ActionItems, "concrete, actionable"),
            (WorkflowKind::ExpandBullets, "expand a short, terse"),
        ];

        for (kind, marker) in cases {
            let mut server = Server::new_async().await;
            let inner = if kind == WorkflowKind::ActionItems { r#"{"items":[],"incomplete":false}"# } else { "{\"title\":\"Test\",\"lessons_markdown\":\"## L1\\nbody\",\"tags\":[\"a\"]}" };
            let body = format!(
                r#"{{"choices": [{{"message": {{"content": {}}}}}]}}"#,
                serde_json::to_string(inner).unwrap()
            );
            let m = server
                .mock("POST", "/chat/completions")
                .match_body(Matcher::Regex(marker.to_string()))
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(body)
                .create_async()
                .await;

            let p = provider_for(&server.url());
            let env = p
                .run_workflow(kind, "source", &[], tokio_util::sync::CancellationToken::new())
                .await
                .unwrap_or_else(|e| panic!("{kind:?} should succeed: {e}"));
            assert_eq!(env.title.as_deref(), Some(if kind == WorkflowKind::ActionItems { "Meeting actions — draft" } else { "Test" }));
            m.assert_async().await;
        }
    }

    #[tokio::test]
    async fn action_items_numbers_complete_source_and_omits_account_vocabulary() {
        let mut server = Server::new_async().await;
        let inner = r#"{"items":[{"kind":"action","text":"Kai will send QA tomorrow","owner":"Kai","due":"tomorrow","passage":2}],"incomplete":false}"#;
        let m = server.mock("POST", "/chat/completions")
            .match_body(Matcher::PartialJson(serde_json::json!({"messages":[
                {"role":"system","content":crate::llm::meeting::PROMPT},
                {"role":"user","content":crate::llm::meeting::request("Meeting\nKai will send QA tomorrow").unwrap()}
            ]})))
            .with_status(200).with_body(serde_json::json!({"choices":[{"message":{"content":format!("```json\n{inner}\n```")}}]}).to_string())
            .create_async().await;
        let env=provider_for(&server.url()).run_workflow(crate::llm::provider::WorkflowKind::ActionItems,
            "Meeting\nKai will send QA tomorrow", &["UNRELATED_PRIVATE_TAG".into()],tokio_util::sync::CancellationToken::new()).await.unwrap();
        assert!(env.lessons_markdown.contains("Owner: Kai; Due: tomorrow"));
        assert!(env.lessons_markdown.contains("Evidence 2"));
        m.assert_async().await;
    }
    #[tokio::test]
    async fn action_items_empty_and_oversized_sources_never_dispatch() {
        let mut server=Server::new_async().await;
        let m=server.mock("POST","/chat/completions").expect(0).create_async().await;
        for source in [String::new(),"ก".repeat(crate::llm::meeting::MAX_SOURCE_CHARS+1)] {
            assert!(provider_for(&server.url()).run_workflow(crate::llm::provider::WorkflowKind::ActionItems,
                &source,&[],tokio_util::sync::CancellationToken::new()).await.is_err());
        }
        m.assert_async().await;
    }

    #[tokio::test]
    async fn action_items_refuses_unattributed_commitments() {
        let mut server = Server::new_async().await;
        let inner = r#"{"title":"Tasks","lessons_markdown":"- [ ] Kai will ship on Friday"}"#;
        let m = server.mock("POST", "/chat/completions").with_status(200)
            .with_body(serde_json::json!({"choices":[{"message":{"content":inner}}]}).to_string())
            .create_async().await;
        let result = provider_for(&server.url()).run_workflow(
            crate::llm::provider::WorkflowKind::ActionItems,
            "No owner or delivery date was agreed.", &[], tokio_util::sync::CancellationToken::new()).await;
        assert!(result.is_err(), "unattributed commitments must not become a saved checklist");
        m.assert_async().await;
    }

    /// Same measured behavior as `a_fenced_envelope_from_an_endpoint_that_
    /// ignores_response_format_still_parses`, for `run_workflow`: the lenient
    /// parse plumbing is shared with `extract`, so a fenced answer must
    /// still parse here too.
    #[tokio::test]
    async fn run_workflow_survives_a_fenced_envelope() {
        let mut server = Server::new_async().await;
        let inner = "```json\n{\"title\":\"Fenced\",\"lessons_markdown\":\"## L1\\nbody\",\"tags\":[\"a\"]}\n```";
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
        let env = p
            .run_workflow(
                crate::llm::provider::WorkflowKind::Summarize,
                "source",
                &[],
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("a fenced envelope must still parse");
        assert_eq!(env.title.as_deref(), Some("Fenced"));
        assert_eq!(env.lessons_markdown, "## L1\nbody");
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
        let env = p.extract("source", &[], tokio_util::sync::CancellationToken::new()).await.expect("ok");
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
        let err = p.extract("source", &[], tokio_util::sync::CancellationToken::new()).await.expect_err("expected error");
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
        let err = p.extract("source", &[], token).await.expect_err("expected cancel");
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
            .extract("source", &[], tokio_util::sync::CancellationToken::new())
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
        let err = p.extract("source", &[], tokio_util::sync::CancellationToken::new()).await.expect_err("expected error");
        assert!(
            matches!(err, ExtractError::MalformedEnvelope { .. }),
            "got: {err:?}"
        );
    }

    /// auto-link runs against the same endpoints as Extract and had the same
    /// strict parse, so a fence cost a whole suggestion round there too.
    #[tokio::test]
    async fn suggest_links_also_survives_a_fenced_envelope() {
        let mut server = Server::new_async().await;
        let inner = "```json\n{\"suggestions\":[{\"uuid\":\"AAAA\",\"related\":true,\"should_append\":true,\"addition_text\":\"See [[x]].\"}]}\n```";
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
            .expect("a fenced envelope must still parse");
        assert_eq!(env.suggestions.len(), 1);
        assert!(env.suggestions[0].related);
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

    /// The folder proposal runs against the same OpenAI-compatible endpoints
    /// as Extract, so the same fence (gotcha #7b) must not cost the answer.
    #[tokio::test]
    async fn suggest_folder_survives_a_fenced_envelope() {
        let mut server = Server::new_async().await;
        let inner = "```json\n{\"folder\":\"Notes/Trading\",\"reason\":\"About trading.\"}\n```";
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

        let env = provider_for(&server.url())
            .suggest_folder(
                "note",
                &["Notes/Trading".to_string()],
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("a fenced envelope must still parse");
        assert_eq!(env.folder.as_deref(), Some("Notes/Trading"));
    }

    /// The request must carry the folder prompt and the offered folders —
    /// matched on the wire, not on a struct.
    #[tokio::test]
    async fn suggest_folder_sends_its_prompt_and_the_folder_list() {
        let mut server = Server::new_async().await;
        let m = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex("choose one folder".into()),
                Matcher::Regex("Notes/Trading".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"choices":[{"message":{"content":"{\"folder\":null,\"reason\":null}"}}]}"#)
            .create_async()
            .await;

        let env = provider_for(&server.url())
            .suggest_folder(
                "note",
                &["Notes/Trading".to_string()],
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("ok");
        m.assert_async().await;
        assert_eq!(env.folder, None);
    }

    /// Same wire discipline as `suggest_folder_sends_its_prompt_and_the_folder_list`,
    /// and a fenced answer still parses (gotcha #7b).
    #[tokio::test]
    async fn synthesize_sends_its_prompt_and_payload_and_survives_a_fence() {
        let mut server = Server::new_async().await;
        let inner = "```json\n{\"title\":\"Combined\",\"lessons_markdown\":\"## Across\\nx\",\"meta_lessons_markdown\":null,\"tags\":[],\"confidence\":null}\n```";
        let m = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::AllOf(vec![
                Matcher::Regex("cross-source key points".into()),
                Matcher::Regex("user_context".into()),
                Matcher::Regex("youtu.be/jXtnhyro-QE".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({"choices": [{"message": {"content": inner}}]}).to_string())
            .create_async()
            .await;
        let digests = vec![crate::llm::provider::SourceDigest {
            title: None,
            display_url: "https://youtu.be/jXtnhyro-QE".into(),
            status: "ok".into(),
            lessons_markdown: "## A".into(),
        }];
        let env = provider_for(&server.url())
            .synthesize(&digests, "ctx", tokio_util::sync::CancellationToken::new())
            .await
            .expect("fenced envelope parses");
        m.assert_async().await;
        assert_eq!(env.title.as_deref(), Some("Combined"));
    }

    #[tokio::test]
    async fn disable_thinking_sends_chat_template_kwargs_on_extract() {
        // With the account opted in, the body must carry
        // chat_template_kwargs.enable_thinking=false so llama.cpp skips the
        // Qwen3 reasoning step. match_body proves it reached the wire rather
        // than merely being set on the struct.
        let mut server = Server::new_async().await;
        let m = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::PartialJson(json!({
                "chat_template_kwargs": { "enable_thinking": false }
            })))
            .with_status(200)
            .with_body(r#"{"choices": [{"message": {"content": "{\"lessons_markdown\":\"x\"}"}}]}"#)
            .create_async()
            .await;

        provider_with_thinking(&server.url(), true)
            .extract("source", &[], tokio_util::sync::CancellationToken::new())
            .await
            .expect("ok");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn disable_thinking_reaches_the_chat_path_too() {
        // `chat` is Ask Jodd, the longest generation of the three request
        // builders and so the one where thinking overhead is most likely to
        // hit the timeout. The original fix only covered `extract`; main has
        // three builders and all three go through `thinking_kwargs`.
        let mut server = Server::new_async().await;
        let m = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::PartialJson(json!({
                "chat_template_kwargs": { "enable_thinking": false }
            })))
            .with_status(200)
            .with_body(r#"{"choices":[{"message":{"content":"the answer"}}]}"#)
            .create_async()
            .await;

        let turns = vec![ChatTurn { role: ChatRole::User, content: "q".into() }];
        provider_with_thinking(&server.url(), true)
            .chat("SYS", &turns, CancellationToken::new())
            .await
            .expect("ok");
        m.assert_async().await;
    }

    #[test]
    fn chat_request_serialization_gates_on_kwargs() {
        // Default: chat_template_kwargs is None, so skip_serializing_if omits
        // the key entirely and hosted providers that 400 on unknown body
        // params are unaffected. Enabled: the key is present and false.
        let base = ChatRequest {
            model: "m",
            messages: vec![ChatRequestMessage { role: "user", content: "hi" }],
            temperature: 0.2,
            response_format: None,
            chat_template_kwargs: None,
        };
        let off = serde_json::to_string(&base).unwrap();
        assert!(!off.contains("chat_template_kwargs"), "got: {off}");

        let on = ChatRequest {
            chat_template_kwargs: Some(json!({ "enable_thinking": false })),
            ..base
        };
        let on = serde_json::to_string(&on).unwrap();
        assert!(
            on.contains(r#""chat_template_kwargs":{"enable_thinking":false}"#),
            "got: {on}"
        );
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
            false,
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
