//! Jodd's MCP client for an AI host: one MCP session over a byte stream
//! (the SSH channel's stdout and stdin), speaking the contract in
//! jodd-ai-host v1.0.0's `docs/PROTOCOL.md`. This file knows nothing about
//! SSH; a caller opens the channel and hands over its two ends.
//!
//! What it promises its callers:
//! - `system` is always sent, even empty, so the host's join is the legacy
//!   wrapper's `{system}\n\n---\n\n{prompt}` byte for byte;
//! - a schema that is not a JSON object is a Jodd bug: refused here, never sent;
//! - a tool that ran and failed is [`McpError::Tool`] with a kind this Jodd
//!   knows ([`normalise_kind`]: an unknown kind is `cli_failed`);
//! - a cancel sends `notifications/cancelled` for that request and returns
//!   [`McpError::Cancelled`] at once, without waiting for the host;
//! - a stream that closes (the host went away) fails every pending and later
//!   call with [`McpError::Protocol`], never a hang.
//!
//! There is no deadline here: a host that keeps the stream open and never
//! answers is the caller's to bound (with `cancel`, or a timeout around it).

use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResult, ClientRequest, JsonObject,
    ServerResult, Tool,
};
use rmcp::service::{PeerRequestOptions, RunningService, ServiceError};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{RoleClient, ServiceExt};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

/// The SSH request that starts an MCP session (`SSH_ORIGINAL_COMMAND`).
pub const MCP_REQUEST: &str = "jodd-ai-mcp";
/// Where the host kit installs the server; the key line's forced command.
pub const MCP_BIN: &str = "/usr/local/bin/jodd-ai-mcp";
/// The `_meta` key carrying each tool's contract version.
pub const META_TOOL_VERSION: &str = "jodd-ai-host/tool-version";
/// (tool, highest tool-version this Jodd understands, host version that introduced it)
pub const KNOWN_TOOLS: &[(&str, u64, &str)] = &[("infer", 1, "1.0.0"), ("forget_key", 1, "1.0.0")];

/// Every error `kind` this Jodd knows (PROTOCOL.md, `infer` and `forget_key`).
const KNOWN_KINDS: &[&str] = &[
    "not_enabled",
    "too_large",
    "busy",
    "cli_missing",
    "timeout",
    "cli_failed",
    "no_key",
    "forget_failed",
];

/// What a cancel tells the host.
const CANCEL_REASON: &str = "cancelled by Jodd";
/// How long a cancel waits for `notifications/cancelled` to be written.
const CANCEL_SEND_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// One `tools/list` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInfo {
    pub name: String,
    /// `_meta["jodd-ai-host/tool-version"]`; 1 when absent or not a whole number.
    pub tool_version: u64,
    /// `infer`'s `harness` enum (the enabled harnesses); empty for other tools.
    pub harnesses: Vec<String>,
}

/// What the host says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    /// `serverInfo.version`, as the host sent it (it may not parse).
    pub version: String,
    pub tools: Vec<ToolInfo>,
}

/// One `infer` call.
/// `Debug` shows lengths, never the prompt, system text or schema (ruling P4).
#[derive(Clone, Copy)]
pub struct InferArgs<'a> {
    pub harness: &'a str,
    pub prompt: &'a str,
    /// Always sent, even empty.
    pub system: &'a str,
    /// JSON Schema text; must be a JSON object.
    pub schema: Option<&'a str>,
    /// Can only lower the host's limit.
    pub timeout_secs: Option<u64>,
}

/// `Debug` shows the CLI's stdout and stderr as lengths only (ruling P4).
pub enum McpError {
    /// The tool ran and failed (`isError: true`). `kind` has been through
    /// [`normalise_kind`]; `text` is the host's own sentence.
    Tool { kind: String, text: String, exit_code: Option<i32>, stdout: Option<String>, stderr: Option<String> },
    /// Handshake failed, malformed reply, a refused request, transport closed.
    Protocol(String),
    /// The caller's cancel token fired; the host was told.
    Cancelled,
}

impl std::fmt::Debug for InferArgs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferArgs")
            .field("harness", &self.harness)
            .field("prompt_len", &self.prompt.len())
            .field("system_len", &self.system.len())
            .field("schema_len", &self.schema.map(str::len))
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl std::fmt::Debug for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::Tool { kind, text, exit_code, stdout, stderr } => f
                .debug_struct("Tool")
                .field("kind", kind)
                .field("text", text)
                .field("exit_code", exit_code)
                .field("stdout_len", &stdout.as_ref().map(String::len))
                .field("stderr_len", &stderr.as_ref().map(String::len))
                .finish(),
            McpError::Protocol(m) => f.debug_tuple("Protocol").field(m).finish(),
            McpError::Cancelled => f.write_str("Cancelled"),
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::Tool { kind, text, .. } => write!(f, "{text} ({kind})"),
            McpError::Protocol(m) => write!(f, "{m}"),
            McpError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for McpError {}

/// A known error kind passes through; anything else is `cli_failed`, as
/// PROTOCOL.md requires of every client (new kinds may come in any release).
pub fn normalise_kind(kind: &str) -> &str {
    KNOWN_KINDS.iter().copied().find(|k| *k == kind).unwrap_or("cli_failed")
}

/// One MCP session with one AI host.
pub struct McpClient {
    service: RunningService<RoleClient, ()>,
}

impl McpClient {
    /// The MCP handshake over `read` (the host's stdout) and `write` (its
    /// stdin). Returns the client and `serverInfo.version`.
    pub async fn connect<R, W>(read: R, write: W) -> Result<(McpClient, String), McpError>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let transport = AsyncRwTransport::new_client(read, write);
        let service = ()
            .serve(transport)
            .await
            .map_err(|e| McpError::Protocol(format!("the AI host did not complete the MCP handshake: {e}")))?;
        let Some(info) = service.peer_info() else {
            let _ = service.cancel().await;
            return Err(McpError::Protocol("the AI host sent no server info".into()));
        };
        let version = info.server_info.version.clone();
        Ok((McpClient { service }, version))
    }

    /// `tools/list`, with the version [`McpClient::connect`] returned.
    pub async fn host_info(&self, version: String) -> Result<HostInfo, McpError> {
        let tools = self.service.peer().list_all_tools().await.map_err(service_error)?;
        Ok(HostInfo { version, tools: tools.iter().map(tool_info).collect() })
    }

    /// Run one prompt; the answer text. `cancel` stops it: the host gets
    /// `notifications/cancelled` and this returns [`McpError::Cancelled`].
    pub async fn infer(&self, args: InferArgs<'_>, cancel: CancellationToken) -> Result<String, McpError> {
        let arguments = infer_arguments(&args)?;
        if cancel.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(
            CallToolRequestParams::new("infer").with_arguments(arguments),
        ));
        let mut handle = self
            .service
            .peer()
            .send_cancellable_request(request, PeerRequestOptions::no_options())
            .await
            .map_err(service_error)?;
        let reply = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            r = &mut handle.rx => Some(r),
        };
        let Some(reply) = reply else {
            // Best effort, and bounded: a closed stream already stopped the
            // host's run, and a stalled one (a dead network, a full window)
            // must not hold the caller here.
            let _ = tokio::time::timeout(CANCEL_SEND_WAIT, handle.cancel(Some(CANCEL_REASON.into()))).await;
            return Err(McpError::Cancelled);
        };
        let result = call_result(reply.map_err(|_| closed())?.map_err(service_error)?)?;
        if result.is_error == Some(true) {
            return Err(tool_error(&result));
        }
        text_of(&result).ok_or_else(|| McpError::Protocol("the AI host's answer had no text".into()))
    }

    /// Remove this device's key from the host; whether a line was removed.
    /// The host ends the session after answering.
    pub async fn forget_key(&self) -> Result<bool, McpError> {
        let result = self
            .service
            .peer()
            .call_tool(CallToolRequestParams::new("forget_key"))
            .await
            .map_err(service_error)?;
        if result.is_error == Some(true) {
            return Err(tool_error(&result));
        }
        result
            .structured_content
            .as_ref()
            .and_then(|s| s.get("removed"))
            .and_then(Value::as_bool)
            .ok_or_else(|| McpError::Protocol("the AI host's forget_key answer had no `removed`".into()))
    }

    /// End the session: the stream closes, which stops any run still going.
    pub async fn close(self) {
        let _ = self.service.cancel().await;
    }
}

/// `infer`'s arguments. `system` always; `schema` and `timeout_secs` only when set.
fn infer_arguments(args: &InferArgs<'_>) -> Result<JsonObject, McpError> {
    let mut a = JsonObject::new();
    a.insert("harness".into(), args.harness.into());
    a.insert("prompt".into(), args.prompt.into());
    a.insert("system".into(), args.system.into());
    if let Some(text) = args.schema {
        match serde_json::from_str::<Value>(text) {
            Ok(v @ Value::Object(_)) => {
                a.insert("schema".into(), v);
            }
            _ => return Err(McpError::Protocol("schema is not a JSON object".into())),
        }
    }
    if let Some(t) = args.timeout_secs {
        a.insert("timeout_secs".into(), t.into());
    }
    Ok(a)
}

fn tool_info(t: &Tool) -> ToolInfo {
    let tool_version = t
        .meta
        .as_ref()
        .and_then(|m| m.get(META_TOOL_VERSION))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let harnesses = if t.name == "infer" {
        t.input_schema
            .get("properties")
            .and_then(|p| p.get("harness"))
            .and_then(|h| h.get("enum"))
            .and_then(Value::as_array)
            .map(|e| e.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    ToolInfo { name: t.name.to_string(), tool_version, harnesses }
}

fn call_result(r: ServerResult) -> Result<CallToolResult, McpError> {
    match r {
        ServerResult::CallToolResult(r) => Ok(r),
        _ => Err(McpError::Protocol("the AI host answered a tool call with something else".into())),
    }
}

/// Every text item, joined by newlines (the host sends one).
fn text_of(r: &CallToolResult) -> Option<String> {
    let texts: Vec<&str> = r.content.iter().filter_map(|c| c.as_text()).map(|t| t.text.as_str()).collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

fn tool_error(r: &CallToolResult) -> McpError {
    let s = r.structured_content.as_ref();
    let field = |k: &str| s.and_then(|s| s.get(k));
    let string = |k: &str| field(k).and_then(Value::as_str).map(str::to_string);
    McpError::Tool {
        kind: normalise_kind(field("kind").and_then(Value::as_str).unwrap_or("")).to_string(),
        text: text_of(r).unwrap_or_default(),
        exit_code: field("exit_code").and_then(Value::as_i64).and_then(|c| i32::try_from(c).ok()),
        stdout: string("stdout"),
        stderr: string("stderr"),
    }
}

fn closed() -> McpError {
    McpError::Protocol("the connection to the AI host closed".into())
}

fn service_error(e: ServiceError) -> McpError {
    match e {
        ServiceError::TransportClosed => closed(),
        ServiceError::McpError(e) => McpError::Protocol(format!("the AI host refused the request: {}", e.message)),
        other => McpError::Protocol(format!("MCP: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;
    use crate::llm::remote::fake_host::{FakeHost, FakeTool, Reply};

    async fn connected(host: FakeHost) -> (McpClient, String, crate::llm::remote::fake_host::Recorder) {
        let (read, write, rec) = host.serve_duplex();
        let (client, version) = McpClient::connect(read, write).await.expect("handshake");
        (client, version, rec)
    }

    fn args<'a>(harness: &'a str, prompt: &'a str) -> InferArgs<'a> {
        InferArgs { harness, prompt, system: "", schema: None, timeout_secs: None }
    }

    #[tokio::test]
    async fn connect_reads_server_version() {
        let (client, version, _) = connected(FakeHost::new("1.2.3")).await;
        assert_eq!(version, "1.2.3");
        client.close().await;
    }

    #[tokio::test]
    async fn host_info_reads_tools_meta_and_harness_enum() {
        let host = FakeHost::new("1.0.0").tools(vec![
            FakeTool::infer(&["claude", "codex"]),
            FakeTool::forget_key().version(None),
            FakeTool::named("infer_v2").version(Some(3)),
        ]);
        let (client, version, _) = connected(host).await;
        let info = client.host_info(version).await.expect("tools/list");
        assert_eq!(info.version, "1.0.0");
        assert_eq!(
            info.tools,
            vec![
                ToolInfo {
                    name: "infer".into(),
                    tool_version: 1,
                    harnesses: vec!["claude".into(), "codex".into()],
                },
                // No `_meta`: tool-version 1.
                ToolInfo { name: "forget_key".into(), tool_version: 1, harnesses: vec![] },
                ToolInfo { name: "infer_v2".into(), tool_version: 3, harnesses: vec![] },
            ]
        );
        client.close().await;
    }

    #[tokio::test]
    async fn infer_sends_exactly_the_args() {
        let (client, _, rec) = connected(FakeHost::new("1.0.0")).await;
        let schema = r#"{"type":"object","properties":{"a":{"type":"string"}}}"#;
        client
            .infer(
                InferArgs {
                    harness: "codex",
                    prompt: "P",
                    system: "S",
                    schema: Some(schema),
                    timeout_secs: Some(180),
                },
                CancellationToken::new(),
            )
            .await
            .expect("infer");
        let sent = serde_json::Value::Object(rec.last_args("infer").expect("infer was called"));
        assert_eq!(
            sent,
            json!({
                "harness": "codex",
                "prompt": "P",
                "system": "S",
                "schema": {"type": "object", "properties": {"a": {"type": "string"}}},
                "timeout_secs": 180
            })
        );

        // An empty system is still sent; absent options are left out.
        client.infer(args("claude", "Q"), CancellationToken::new()).await.expect("infer");
        let sent = serde_json::Value::Object(rec.last_args("infer").unwrap());
        assert_eq!(sent, json!({"harness": "claude", "prompt": "Q", "system": ""}));
        client.close().await;
    }

    #[tokio::test]
    async fn a_schema_that_is_not_an_object_is_never_sent() {
        let (client, _, rec) = connected(FakeHost::new("1.0.0")).await;
        for bad in ["[1]", "\"s\"", "not json"] {
            let a = InferArgs { schema: Some(bad), ..args("codex", "P") };
            match client.infer(a, CancellationToken::new()).await {
                Err(McpError::Protocol(m)) => assert_eq!(m, "schema is not a JSON object", "{bad}"),
                other => panic!("{bad}: {other:?}"),
            }
        }
        assert!(rec.calls().is_empty());
        client.close().await;
    }

    #[tokio::test]
    async fn infer_returns_the_text_content() {
        let host = FakeHost::new("1.0.0").reply("infer", Reply::text("สวัสดี — the answer"));
        let (client, _, _) = connected(host).await;
        let got = client.infer(args("claude", "P"), CancellationToken::new()).await.expect("infer");
        assert_eq!(got, "สวัสดี — the answer");
        client.close().await;
    }

    #[tokio::test]
    async fn a_tool_error_carries_kind_and_raw_output() {
        let host = FakeHost::new("1.0.0").reply(
            "infer",
            Reply::tool_error(
                "cli_failed",
                "codex exited with code 2",
                json!({"exit_code": 2, "stdout": "o", "stderr": "e"}),
            ),
        );
        let (client, _, _) = connected(host).await;
        match client.infer(args("codex", "P"), CancellationToken::new()).await {
            Err(McpError::Tool { kind, text, exit_code, stdout, stderr }) => {
                assert_eq!(kind, "cli_failed");
                assert_eq!(text, "codex exited with code 2");
                assert_eq!(exit_code, Some(2));
                assert_eq!(stdout.as_deref(), Some("o"));
                assert_eq!(stderr.as_deref(), Some("e"));
            }
            other => panic!("{other:?}"),
        }
        client.close().await;
    }

    #[tokio::test]
    async fn an_unknown_kind_is_cli_failed() {
        assert_eq!(normalise_kind("not_signed_in"), "cli_failed");
        assert_eq!(normalise_kind(""), "cli_failed");
        for kind in [
            "not_enabled",
            "too_large",
            "busy",
            "cli_missing",
            "timeout",
            "cli_failed",
            "no_key",
            "forget_failed",
        ] {
            assert_eq!(normalise_kind(kind), kind);
        }

        // On the wire too: a kind this Jodd does not know arrives as cli_failed,
        // with the host's text kept.
        let host = FakeHost::new("1.9.0").reply(
            "infer",
            Reply::tool_error("not_signed_in", "claude is not signed in", json!({})),
        );
        let (client, _, _) = connected(host).await;
        match client.infer(args("claude", "P"), CancellationToken::new()).await {
            Err(McpError::Tool { kind, text, exit_code, .. }) => {
                assert_eq!(kind, "cli_failed");
                assert_eq!(text, "claude is not signed in");
                assert_eq!(exit_code, None);
            }
            other => panic!("{other:?}"),
        }
        client.close().await;
    }

    #[tokio::test]
    async fn cancel_sends_cancelled_and_returns_promptly() {
        let host = FakeHost::new("1.0.0")
            .reply("infer", Reply::after(Duration::from_secs(10), Reply::text("too late")));
        let (client, _, rec) = connected(host).await;
        let cancel = CancellationToken::new();
        {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                cancel.cancel();
            });
        }
        let started = Instant::now();
        let got = client.infer(args("claude", "P"), cancel).await;
        let took = started.elapsed();
        assert!(matches!(got, Err(McpError::Cancelled)), "{got:?}");
        assert!(took < Duration::from_secs(1), "took {took:?}");
        // The host heard about it while the session was still open.
        let seen = rec.cancelled_within(Duration::from_secs(1)).await.expect("no cancel arrived");
        assert_eq!(seen.reason.as_deref(), Some("cancelled by Jodd"));
        client.close().await;
    }

    #[tokio::test]
    async fn an_already_cancelled_token_sends_nothing() {
        let (client, _, rec) = connected(FakeHost::new("1.0.0")).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let got = client.infer(args("claude", "P"), cancel).await;
        assert!(matches!(got, Err(McpError::Cancelled)), "{got:?}");
        assert!(rec.calls().is_empty());
        client.close().await;
    }

    #[tokio::test]
    async fn a_closed_stream_is_protocol_error() {
        // Nothing answers the handshake.
        let got = McpClient::connect(tokio::io::empty(), tokio::io::sink()).await;
        assert!(matches!(got, Err(McpError::Protocol(_))), "{:?}", got.err());

        // The host goes away mid-call.
        let host = FakeHost::new("1.0.0").reply("infer", Reply::Hangup);
        let (client, _, _) = connected(host).await;
        let got = tokio::time::timeout(
            Duration::from_secs(2),
            client.infer(args("claude", "P"), CancellationToken::new()),
        )
        .await
        .expect("a closed stream must not hang");
        assert!(matches!(got, Err(McpError::Protocol(_))), "{got:?}");

        // And every call after it.
        let got = client.forget_key().await;
        assert!(matches!(got, Err(McpError::Protocol(_))), "{got:?}");
        client.close().await;
    }

    #[tokio::test]
    async fn an_unlisted_infer_still_gets_its_tool_error() {
        // Every harness off: `infer` is not listed, but a call to it is
        // answered with `not_enabled`, as the real host does.
        let host = FakeHost::new("1.0.0").tools(vec![FakeTool::forget_key()]).reply(
            "infer",
            Reply::tool_error(
                "not_enabled",
                "claude is not enabled on this AI host. Ask its administrator to run `sudo jodd-ai-host enable claude`",
                json!({}),
            ),
        );
        let (client, version, _) = connected(host).await;
        let info = client.host_info(version).await.expect("tools/list");
        assert!(info.tools.iter().all(|t| t.name != "infer"));
        match client.infer(args("claude", "P"), CancellationToken::new()).await {
            Err(McpError::Tool { kind, text, .. }) => {
                assert_eq!(kind, "not_enabled");
                assert!(text.contains("sudo jodd-ai-host enable claude"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        client.close().await;
    }

    #[tokio::test]
    async fn the_fake_echoes_the_harness_it_was_called_with() {
        // Guards the fake itself: Tasks 2-4 read `structuredContent.harness`.
        let (read, write, _) = FakeHost::new("1.0.0").serve_duplex();
        let transport = rmcp::transport::async_rw::AsyncRwTransport::new_client(read, write);
        let service = rmcp::ServiceExt::serve((), transport).await.expect("handshake");
        let mut a = JsonObject::new();
        a.insert("harness".into(), "codex".into());
        a.insert("prompt".into(), "P".into());
        let r = service
            .peer()
            .call_tool(CallToolRequestParams::new("infer").with_arguments(a))
            .await
            .expect("infer");
        assert_eq!(r.structured_content, Some(json!({"harness": "codex", "exit_code": 0})));
        let _ = service.cancel().await;
    }

    #[test]
    fn debug_never_shows_prompt_system_schema_or_cli_output() {
        let a = InferArgs {
            harness: "codex",
            prompt: "SECRET-PROMPT",
            system: "SECRET-SYSTEM",
            schema: Some(r#"{"SECRET":"SCHEMA"}"#),
            timeout_secs: Some(30),
        };
        let shown = format!("{a:?}");
        for secret in ["SECRET-PROMPT", "SECRET-SYSTEM", "SECRET"] {
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(shown.contains("codex") && shown.contains("prompt_len: 13"), "{shown}");

        let e = McpError::Tool {
            kind: "cli_failed".into(),
            text: "codex exited with code 2".into(),
            exit_code: Some(2),
            stdout: Some("SECRET-OUT".into()),
            stderr: Some("SECRET-ERR".into()),
        };
        let shown = format!("{e:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
        assert!(shown.contains("cli_failed") && shown.contains("stdout_len: Some(10)"), "{shown}");
    }

    #[tokio::test]
    async fn forget_key_reads_removed() {
        for removed in [true, false] {
            let host = FakeHost::new("1.0.0").reply("forget_key", Reply::removed(removed));
            let (client, _, rec) = connected(host).await;
            assert_eq!(client.forget_key().await.expect("forget_key"), removed);
            // No arguments: the host refuses any.
            assert_eq!(rec.last_args("forget_key").unwrap_or_default().len(), 0);
            client.close().await;
        }

        let host = FakeHost::new("1.0.0")
            .reply("forget_key", Reply::tool_error("forget_failed", "could not edit", json!({})));
        let (client, _, _) = connected(host).await;
        match client.forget_key().await {
            Err(McpError::Tool { kind, .. }) => assert_eq!(kind, "forget_failed"),
            other => panic!("{other:?}"),
        }
        client.close().await;
    }

    #[tokio::test]
    async fn a_refused_request_is_protocol_error() {
        let host = FakeHost::new("1.0.0").reply("infer", Reply::InvalidParams("`prompt` is required".into()));
        let (client, _, _) = connected(host).await;
        match client.infer(args("claude", "P"), CancellationToken::new()).await {
            Err(McpError::Protocol(m)) => assert!(m.contains("`prompt` is required"), "{m}"),
            other => panic!("{other:?}"),
        }
        client.close().await;
    }
}
