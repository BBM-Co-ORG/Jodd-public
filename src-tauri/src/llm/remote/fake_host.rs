//! An in-test AI host: an rmcp server shaped like jodd-ai-host v1.0.0's
//! `jodd-ai-mcp` (`crates/jodd-ai-mcp/src/mcp.rs`), served over any byte
//! stream. Each test sets what the host says — `serverInfo.version`, the
//! `tools/list` entries, each call's reply — and reads back what Jodd sent.
//!
//! Faithful where Jodd's client depends on it:
//! - `infer` and `forget_key` refuse arguments the host does not know with
//!   JSON-RPC invalid params, as the real server does, so an extra field Jodd
//!   starts sending fails here before it fails on a real host;
//! - a tool that ran and failed is `isError: true` with `structuredContent.kind`;
//! - a handled `forget_key` ends the session once its answer is out;
//! - a cancelled call gets no answer.
//!
//! [`Reply::Hangup`] and [`Recorder::vanish`] close the byte stream with no
//! answer: the host went away (the phone lost signal, the server rebooted).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, CancelledNotificationParam, ContentBlock,
    Implementation, JsonObject, ListToolsResult, Meta, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::mcp::META_TOOL_VERSION;

/// The arguments the real `infer` accepts; anything else is invalid params.
const INFER_ARGS: [&str; 5] = ["harness", "prompt", "system", "schema", "timeout_secs"];

/// One `tools/list` entry.
#[derive(Clone, Debug)]
pub(crate) struct FakeTool {
    pub name: String,
    /// `_meta["jodd-ai-host/tool-version"]`; `None` leaves `_meta` out.
    pub tool_version: Option<u64>,
    /// `infer`'s `harness` enum. Ignored for every other tool.
    pub harnesses: Vec<String>,
}

impl FakeTool {
    /// `infer` at tool-version 1 offering `harnesses`.
    pub fn infer(harnesses: &[&str]) -> Self {
        FakeTool {
            name: "infer".into(),
            tool_version: Some(1),
            harnesses: harnesses.iter().map(|h| h.to_string()).collect(),
        }
    }

    /// `forget_key` at tool-version 1.
    pub fn forget_key() -> Self {
        FakeTool::named("forget_key")
    }

    /// Any tool at tool-version 1, with an empty input schema.
    pub fn named(name: &str) -> Self {
        FakeTool { name: name.into(), tool_version: Some(1), harnesses: Vec::new() }
    }

    pub fn version(mut self, v: Option<u64>) -> Self {
        self.tool_version = v;
        self
    }

    fn to_tool(&self) -> Tool {
        let schema = if self.name == "infer" {
            json!({
                "type": "object",
                "properties": {
                    "harness": { "type": "string", "enum": self.harnesses },
                    "prompt": { "type": "string" },
                    "system": { "type": "string" },
                    "schema": { "type": "object" },
                    "timeout_secs": { "type": "integer", "minimum": 1 }
                },
                "required": ["harness", "prompt"],
                "additionalProperties": false
            })
        } else {
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        };
        let Value::Object(schema) = schema else { unreachable!("schemas are objects") };
        let tool = Tool::new(self.name.clone(), "a fake AI host tool", schema);
        match self.tool_version {
            Some(v) => {
                let mut m = Meta::new();
                m.insert(META_TOOL_VERSION.into(), json!(v));
                tool.with_meta(m)
            }
            None => tool,
        }
    }
}

/// What the host answers one call with.
#[derive(Clone, Debug)]
pub(crate) enum Reply {
    /// `infer`'s success: one text item, and `structuredContent`
    /// `{harness, exit_code: 0}` with the harness the call named.
    Text(String),
    /// Success: one text item and `structuredContent`.
    Answer { text: String, structured: Value },
    /// A tool that ran and failed: `isError: true`, one text item,
    /// `structuredContent` (which carries `kind`).
    ToolError { text: String, structured: Value },
    /// A JSON-RPC invalid-params error (-32602).
    InvalidParams(String),
    /// Wait, then answer with the inner reply. A cancel during the wait ends
    /// the call with no answer, as on the real host.
    After(Duration, Box<Reply>),
    /// The host goes away: the byte stream closes and the call gets no answer.
    Hangup,
}

impl Reply {
    /// `infer`'s success: `text`, with `{harness, exit_code: 0}` echoing
    /// the call's harness.
    pub fn text(text: &str) -> Reply {
        Reply::Text(text.into())
    }

    /// A tool error shaped like the real host's: `{kind, ...extra}`.
    pub fn tool_error(kind: &str, text: &str, extra: Value) -> Reply {
        let mut structured = json!({ "kind": kind });
        if let (Value::Object(s), Value::Object(e)) = (&mut structured, extra) {
            s.extend(e);
        }
        Reply::ToolError { text: text.into(), structured }
    }

    /// `forget_key`'s success: `{removed}`.
    pub fn removed(removed: bool) -> Reply {
        Reply::Answer {
            text: if removed {
                "this device's key was removed from the AI host".into()
            } else {
                "the AI host has no key line for this device".into()
            },
            structured: json!({ "removed": removed }),
        }
    }

    pub fn after(delay: Duration, then: Reply) -> Reply {
        Reply::After(delay, Box::new(then))
    }
}

/// One call the host received.
#[derive(Clone, Debug)]
pub(crate) struct Call {
    pub tool: String,
    pub arguments: Option<JsonObject>,
}

/// A `notifications/cancelled` the host received.
#[derive(Clone, Debug)]
pub(crate) struct Cancel {
    pub reason: Option<String>,
}

/// The host to serve. Built per test, then [`FakeHost::serve`]d.
pub(crate) struct FakeHost {
    version: String,
    tools: Vec<FakeTool>,
    replies: HashMap<String, VecDeque<Reply>>,
}

impl FakeHost {
    /// A host at `version` listing `infer` (claude, codex) and `forget_key`.
    /// Without a set reply, `infer` answers `"the answer"` and `forget_key`
    /// `{removed: true}`.
    pub fn new(version: &str) -> Self {
        FakeHost {
            version: version.into(),
            tools: vec![FakeTool::infer(&["claude", "codex"]), FakeTool::forget_key()],
            replies: HashMap::new(),
        }
    }

    /// Replace the `tools/list` entries.
    pub fn tools(mut self, tools: Vec<FakeTool>) -> Self {
        self.tools = tools;
        self
    }

    /// Queue a reply for `tool`. Calls take queued replies in order; the
    /// last one answers every call after it.
    pub fn reply(mut self, tool: &str, reply: Reply) -> Self {
        self.replies.entry(tool.into()).or_default().push_back(reply);
        self
    }

    /// Serve on a fresh in-memory pipe; returns Jodd's ends of it.
    pub fn serve_duplex(self) -> (ReadHalf<DuplexStream>, WriteHalf<DuplexStream>, Recorder) {
        let (jodd, host) = tokio::io::duplex(1 << 16);
        let (host_read, host_write) = tokio::io::split(host);
        let rec = self.serve(host_read, host_write);
        let (read, write) = tokio::io::split(jodd);
        (read, write, rec)
    }

    /// Serve one MCP session on `read`/`write` (what Jodd writes / what Jodd
    /// reads). Runs in the background; the returned [`Recorder`] observes it.
    pub fn serve<R, W>(self, read: R, write: W) -> Recorder
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (cancel_tx, _) = watch::channel(None);
        let rec = Recorder {
            calls: Arc::new(Mutex::new(Vec::new())),
            cancelled: Arc::new(cancel_tx),
            vanish: CancellationToken::new(),
        };
        let quit = CancellationToken::new();
        let server = Server {
            version: self.version,
            tools: self.tools,
            replies: Arc::new(Mutex::new(self.replies)),
            rec: rec.clone(),
            quit: quit.clone(),
        };
        // rmcp talks to one end of an inner pipe; a relay copies between the
        // other end and the caller's stream, so the host can drop that stream
        // on its own (a hangup) whatever rmcp is doing.
        let (inner, outer) = tokio::io::duplex(1 << 16);
        tokio::spawn(relay(read, write, outer, rec.vanish.clone()));
        tokio::spawn(async move {
            let (r, w) = tokio::io::split(inner);
            let transport = AsyncRwTransport::new_server(r, w);
            let Ok(service) = server.serve(transport).await else { return };
            let stop = service.cancellation_token();
            tokio::spawn(async move {
                quit.cancelled().await;
                stop.cancel();
            });
            let _ = service.waiting().await;
        });
        rec
    }
}

/// Copy bytes both ways until either side ends or the host vanishes; then
/// drop the caller's stream, which is what Jodd sees as the channel closing.
async fn relay<R, W>(mut read: R, mut write: W, outer: DuplexStream, vanish: CancellationToken)
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (mut from_host, mut to_host) = tokio::io::split(outer);
    let up = async {
        let _ = tokio::io::copy(&mut read, &mut to_host).await;
        let _ = to_host.shutdown().await;
    };
    let down = async {
        let _ = tokio::io::copy(&mut from_host, &mut write).await;
        let _ = write.shutdown().await;
    };
    tokio::select! {
        // The host's side ended (a session that quit): Jodd's reads end too.
        _ = down => {}
        _ = async { up.await; std::future::pending::<()>().await } => {}
        _ = vanish.cancelled() => {}
    }
}

/// What a test reads back from a served [`FakeHost`].
#[derive(Clone)]
pub(crate) struct Recorder {
    calls: Arc<Mutex<Vec<Call>>>,
    cancelled: Arc<watch::Sender<Option<Cancel>>>,
    vanish: CancellationToken,
}

impl Recorder {
    /// Every call received, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// The arguments of the last call to `tool`.
    pub fn last_args(&self, tool: &str) -> Option<JsonObject> {
        self.calls().into_iter().rev().find(|c| c.tool == tool).and_then(|c| c.arguments)
    }

    /// The first `notifications/cancelled` received, waiting up to `within`.
    pub async fn cancelled_within(&self, within: Duration) -> Option<Cancel> {
        let mut rx = self.cancelled.subscribe();
        let got = tokio::time::timeout(within, rx.wait_for(|c| c.is_some())).await;
        match got {
            Ok(Ok(c)) => c.clone(),
            _ => None,
        }
    }

    /// The host goes away now: the byte stream closes, nothing more is answered.
    pub fn vanish(&self) {
        self.vanish.cancel();
    }
}

#[derive(Clone)]
struct Server {
    version: String,
    tools: Vec<FakeTool>,
    replies: Arc<Mutex<HashMap<String, VecDeque<Reply>>>>,
    rec: Recorder,
    quit: CancellationToken,
}

impl Server {
    fn next_reply(&self, tool: &str) -> Reply {
        let mut replies = self.replies.lock().unwrap();
        match replies.get_mut(tool) {
            Some(q) if q.len() > 1 => q.pop_front().expect("len > 1"),
            Some(q) if q.len() == 1 => q[0].clone(),
            _ if tool == "forget_key" => Reply::removed(true),
            _ => Reply::text("the answer"),
        }
    }

    fn check_args(&self, tool: &str, args: Option<&JsonObject>) -> Result<(), ErrorData> {
        // As the real host: dispatch is by the fixed names, not the listing.
        // With every harness off `infer` is unlisted, yet a call to it is
        // answered (with `not_enabled`), never refused as an unknown tool.
        let unknown = match tool {
            "infer" => args.and_then(|a| a.keys().find(|k| !INFER_ARGS.contains(&k.as_str()))),
            "forget_key" => args.and_then(|a| a.keys().next()),
            _ => return Err(ErrorData::invalid_params("unknown tool", None)),
        };
        match unknown {
            Some(k) => Err(ErrorData::invalid_params(format!("unknown argument `{k}`"), None)),
            None => Ok(()),
        }
    }

    async fn answer(
        &self,
        reply: Reply,
        harness: Option<&str>,
        ct: &CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
        let mut reply = reply;
        loop {
            reply = match reply {
                Reply::Text(text) => Reply::Answer {
                    text,
                    structured: json!({ "harness": harness, "exit_code": 0 }),
                },
                Reply::After(delay, then) => {
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => *then,
                        _ = ct.cancelled() => {
                            return Err(ErrorData::internal_error("the request was cancelled", None))
                        }
                    }
                }
                Reply::Answer { text, structured } => {
                    let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                    r.structured_content = Some(structured);
                    return Ok(r);
                }
                Reply::ToolError { text, structured } => {
                    let mut r = CallToolResult::error(vec![ContentBlock::text(text)]);
                    r.structured_content = Some(structured);
                    return Ok(r);
                }
                Reply::InvalidParams(msg) => return Err(ErrorData::invalid_params(msg, None)),
                Reply::Hangup => {
                    self.rec.vanish();
                    std::future::pending::<()>().await;
                    unreachable!("pending never resolves")
                }
            }
        }
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("jodd-ai-host", self.version.clone()))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.iter().map(FakeTool::to_tool).collect()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let tool = request.name.to_string();
        self.rec.calls.lock().unwrap().push(Call {
            tool: tool.clone(),
            arguments: request.arguments.clone(),
        });
        self.check_args(&tool, request.arguments.as_ref())?;
        let reply = self.next_reply(&tool);
        let harness = request.arguments.as_ref().and_then(|a| a.get("harness")).and_then(Value::as_str);
        let result = self.answer(reply, harness, &context.ct).await;
        // The real host ends the session after a handled `forget_key`.
        if tool == "forget_key" && result.is_ok() {
            self.quit.cancel();
        }
        result
    }

    async fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) {
        self.rec.cancelled.send_if_modified(|c| {
            let first = c.is_none();
            if first {
                *c = Some(Cancel { reason: notification.reason.clone() });
            }
            first
        });
    }
}
