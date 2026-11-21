//! YouTube Lounge: parsing what a paired TV says, and deciding which video it
//! is playing from that.
//!
//! The parsing half is pure — no network. The TV answers a bind and a long-poll with
//! "chunked" text: a length line, then a JSON array of `[id, [name, args...]]`
//! events. Which event names the video, and how far to trust each, was
//! measured on a Hisense Google TV (spec §2, fixtures in
//! `tests/fixtures/lounge/`); the rules are spec §5.1.
//!
//! The client half (bottom of the file) is READ-ONLY by construction: the only
//! command it can send is [`COMMAND`], a constant, and no function takes a
//! command, a method, params or a video ID. Jodd never changes the TV.
//!
//! See docs/superpowers/specs/2026-11-02-tv-now-playing-ingest-design.md.

use std::time::Duration;

use reqwest::{Method, Url};
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::net::{read_capped, send_guarded, GuardedRequest, CANCELLED};
use super::urls::valid_id;
use super::FetchPolicy;

/// One Lounge event: `[id, [name, args...]]` on the wire.
#[derive(Clone, Debug, PartialEq)]
pub struct LoungeEvent {
    pub id: i64,
    pub name: String,
    pub args: Vec<Value>,
}

/// A reply that never closes its array is hostile or broken; stop holding it.
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;

/// Turns the TV's chunked byte stream into events.
///
/// The length lines are never trusted: the redacted fixtures (and, in
/// practice, any proxy) make them lie. A chunk is complete when its JSON
/// parses as an array. Bytes are kept undecoded until a whole line is in,
/// because a network read can end inside a 3-byte Thai character.
#[derive(Default)]
pub struct ChunkReader {
    /// Bytes after the last `\n` seen — an incomplete line, still undecoded.
    raw: Vec<u8>,
    /// Decoded lines of a JSON array not yet complete.
    pending: String,
}

impl ChunkReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<LoungeEvent> {
        self.raw.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(nl) = self.raw.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.raw.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line_bytes[..nl]);
            self.line(&line, &mut out);
        }
        if self.raw.len() > MAX_PENDING_BYTES {
            self.raw.clear();
        }
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<LoungeEvent>) {
        let trimmed = line.trim();
        if self.pending.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
            return; // a length line (or a blank one)
        }
        self.pending.push_str(line);
        self.pending.push('\n');
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&self.pending) {
            out.extend(items.iter().filter_map(event_from_item));
            self.pending.clear();
        } else if self.pending.len() > MAX_PENDING_BYTES {
            self.pending.clear();
        }
    }
}

fn event_from_item(item: &Value) -> Option<LoungeEvent> {
    let id = item.get(0)?.as_i64()?;
    let arr = item.get(1)?.as_array()?;
    let name = arr.first()?.as_str()?.to_owned();
    Some(LoungeEvent { id, name, args: arr[1..].to_vec() })
}

/// Every event in a complete reply body.
pub fn parse_all(body: &str) -> Vec<LoungeEvent> {
    let mut reader = ChunkReader::new();
    let mut events = reader.push(body.as_bytes());
    if !body.ends_with('\n') {
        events.extend(reader.push(b"\n"));
    }
    events
}

/// What the TV is playing, as far as its events say.
#[derive(Clone, Debug, PartialEq)]
pub struct Sighting {
    pub video_id: String,
    pub seconds: Option<u64>,
    pub duration: Option<u64>,
    pub state: Option<String>,
}

/// A value that came from an event naming `video` (or no video at all).
type Tagged<T> = Option<(Option<String>, Option<T>)>;

/// Folds events into "which video, how far in".
///
/// `nowPlaying` is the TV's own statement and carries the time, but on a
/// Google TV it can arrive with only a `listId`; the video then arrives on
/// the track events. `autoplayUpNext` and the rest name videos that are NOT
/// playing (run 3 measured two), so only the three tiers below ever supply a
/// video ID. The highest tier seen wins, and the latest event within it.
#[derive(Default)]
pub struct Accumulator {
    last_id: Option<i64>,
    sid: Option<String>,
    gsession: Option<String>,
    model: Option<String>,
    /// (tier, id) — tier 1 is best.
    video: Option<(u8, String)>,
    seconds: Tagged<u64>,
    duration: Tagged<u64>,
    state: Tagged<String>,
}

fn tier(name: &str) -> Option<u8> {
    match name {
        "nowPlaying" | "onStateChange" => Some(1),
        "onVideoQualityChanged" | "onSubtitlesTrackChanged" | "onAudioTrackChanged" => Some(2),
        "playlistModified" => Some(3),
        _ => None,
    }
}

/// `"32.854"` or `32.854` → whole seconds; under a second is "no time yet".
fn whole_seconds(v: &Value) -> Option<u64> {
    let f = match v {
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (f.is_finite() && f >= 1.0).then(|| f.floor() as u64)
}

impl Accumulator {
    /// Events at or below the last id seen are replays and are ignored.
    pub fn feed(&mut self, ev: &LoungeEvent) {
        if self.last_id.is_some_and(|last| ev.id <= last) {
            return;
        }
        self.last_id = Some(ev.id);
        let first = ev.args.first();
        let str_arg = |i: usize| ev.args.get(i).and_then(Value::as_str).map(str::to_owned);
        match ev.name.as_str() {
            "c" => self.sid = str_arg(0),
            "S" => self.gsession = str_arg(0),
            "loungeStatus" => {
                if let Some(m) = first.and_then(model_from_status) {
                    self.model = Some(m);
                }
            }
            name => {
                let Some(t) = tier(name) else { return };
                let obj = first.unwrap_or(&Value::Null);
                let id = obj.get("videoId").and_then(Value::as_str).filter(|s| !s.is_empty());
                let id = match id {
                    Some(raw) => match valid_id(raw) {
                        Some(ok) => Some(ok),
                        None => return, // a malformed ID poisons the whole event
                    },
                    None => None,
                };
                if let Some(id) = &id {
                    if self.video.as_ref().is_none_or(|(cur, _)| t <= *cur) {
                        self.video = Some((t, id.clone()));
                    }
                }
                if t == 1 {
                    self.take_timing(obj, id);
                }
            }
        }
    }

    /// A tier-1 event's own time, duration and state, remembered with the
    /// video it named so a different video's time is never borrowed.
    fn take_timing(&mut self, obj: &Value, id: Option<String>) {
        if let Some(v) = obj.get("currentTime") {
            self.seconds = Some((id.clone(), whole_seconds(v)));
        }
        if let Some(v) = obj.get("duration") {
            self.duration = Some((id.clone(), whole_seconds(v)));
        }
        if let Some(v) = obj.get("state") {
            let s = match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            };
            self.state = Some((id, s));
        }
    }

    /// (SID from `c`, gsessionid from `S`).
    pub fn session(&self) -> Option<(String, String)> {
        Some((self.sid.clone()?, self.gsession.clone()?))
    }

    pub fn last_event_id(&self) -> i64 {
        self.last_id.unwrap_or(0)
    }

    pub fn sighting(&self) -> Option<Sighting> {
        let (_, video_id) = self.video.as_ref()?;
        fn pick<T: Clone>(field: &Tagged<T>, video: &str) -> Option<T> {
            let (tag, value) = field.as_ref()?;
            tag.as_deref().is_none_or(|t| t == video).then(|| value.clone()).flatten()
        }
        Some(Sighting {
            video_id: video_id.clone(),
            seconds: pick(&self.seconds, video_id),
            duration: pick(&self.duration, video_id),
            state: pick(&self.state, video_id),
        })
    }

    /// "Hisense HiSmartTV A4", from the `LOUNGE_SCREEN` entry of `loungeStatus`.
    pub fn tv_model(&self) -> Option<String> {
        self.model.clone()
    }
}

/// `devices` is a JSON string holding an array whose screen entry holds
/// `deviceInfo`, a second JSON string. Both layers are strings on the wire.
fn model_from_status(status: &Value) -> Option<String> {
    let devices: Value = serde_json::from_str(status.get("devices")?.as_str()?).ok()?;
    let screen = devices.as_array()?.iter().find(|d| d["type"] == "LOUNGE_SCREEN")?;
    let info: Value = serde_json::from_str(screen.get("deviceInfo")?.as_str()?).ok()?;
    let brand = info.get("brand").and_then(Value::as_str).unwrap_or("").trim();
    let model = info.get("model").and_then(Value::as_str).unwrap_or("").trim();
    let joined = format!("{brand} {model}").trim().to_owned();
    (!joined.is_empty()).then_some(joined)
}

// ── Lounge client constants ───────────────────────────────────────────────
// Every value YouTube sees about "which remote is asking" lives HERE and
// nowhere else — change this block, not a call site. Measured on a Hisense
// Google TV (spec §2, run 3). Deliberately absent: `method` and `magnaKey`
// (pyytlounge's remote-control connect sends them; a reader has no reason to).
pub const DEVICE_NAME: &str = "Jodd";
const APP: &str = "web";
const DEVICE: &str = "REMOTE_CONTROL";
const MDX_VERSION: &str = "3";
const CAPABILITIES: &str = "que,dsdtr,atp";
const VER: &str = "8";
const CVER: &str = "1";
/// The ONE command Jodd ever sends (`req0__sc`). It asks; it changes nothing.
const COMMAND: &str = "getNowPlaying";
const DEFAULT_BASE: &str = "https://www.youtube.com/api/lounge";
/// A token batch, bind or pairing reply is small; anything bigger is not Lounge.
const MAX_REPLY_BYTES: usize = 1024 * 1024;
/// The long-poll is bounded by time; this bounds it by size too.
const MAX_STREAM_BYTES: usize = 4 * 1024 * 1024;
// ───────────────────────────────────────────────────────────────────────────

/// Injectable so tests can point every request at mockito.
#[derive(Debug, Clone)]
pub struct LoungeEndpoints {
    pub base: String,
}

impl Default for LoungeEndpoints {
    fn default() -> Self {
        LoungeEndpoints { base: DEFAULT_BASE.into() }
    }
}

impl LoungeEndpoints {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base.trim_end_matches('/'))
    }
}

/// How long one press may take. Spec §5.1: 6 s overall; once a video ID is
/// known but no time, at most 2 s more, counted from when listening starts (the
/// subscribe's headers) or from the first sighting if that is later — never
/// past the overall deadline.
#[derive(Debug, Clone, Copy)]
pub struct Deadlines {
    pub overall: Duration,
    pub after_video: Duration,
}

impl Default for Deadlines {
    fn default() -> Self {
        Deadlines { overall: Duration::from_secs(6), after_video: Duration::from_secs(2) }
    }
}

#[derive(Clone, PartialEq)]
pub struct Pairing {
    pub screen_id: String,
    pub screen_name: String,
}

// The screen ID is what the lounge token is minted from: it never reaches a log.
impl std::fmt::Debug for Pairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pairing").field("screen_id", &"<redacted>").field("screen_name", &self.screen_name).finish()
    }
}

#[derive(Debug, PartialEq)]
pub enum PairError {
    BadCode,
    Network(String),
}

#[derive(Debug, PartialEq)]
pub enum TvError {
    /// The token batch returned no screen: the TV unlinked Jodd (or was reset).
    Unlinked,
    NothingPlaying,
    /// A short reason. Never a URL: every URL here carries the lounge token.
    Network(String),
    Cancelled,
}

/// "857 907 240 183" / "857-907-240-183" → "857907240183".
pub fn normalize_code(input: &str) -> Result<String, PairError> {
    let digits: String = input.chars().filter(char::is_ascii_digit).collect();
    if digits.len() == 12 { Ok(digits) } else { Err(PairError::BadCode) }
}

/// `application/x-www-form-urlencoded` of `pairs`, for a body or a query.
fn form(pairs: &[(&str, &str)]) -> String {
    let mut u = Url::parse("http://form.invalid/").expect("static URL");
    u.query_pairs_mut().extend_pairs(pairs);
    u.query().unwrap_or("").to_owned()
}

fn post_form(url: String, body: String) -> GuardedRequest {
    GuardedRequest {
        method: Method::POST,
        url,
        headers: vec![("content-type", "application/x-www-form-urlencoded".into())],
        body: Some(body.into_bytes()),
        cookies: Vec::new(),
    }
}

/// A cancellation that a timer fires at `at`, so a long request can be cut
/// short without being the user's cancel. `send_guarded` has a fixed 20 s
/// request timeout; this is how the 6 s / 2 s deadlines are enforced.
struct Deadline {
    token: CancellationToken,
    at: Instant,
    timer: tokio::task::JoinHandle<()>,
}

impl Deadline {
    fn new(parent: &CancellationToken, after: Duration) -> Self {
        let token = parent.child_token();
        let at = Instant::now() + after;
        let fire = token.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep_until(at).await;
            fire.cancel();
        });
        Deadline { token, at, timer }
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.timer.abort();
    }
}

/// What `send`/`body` failed with: the user's cancel, the deadline, or the network.
enum Stop {
    Cancelled,
    Deadline,
    Network(String),
}

impl Stop {
    /// `user` is the caller's token; `s` is a `send_guarded` / `read_capped` error.
    fn from(s: String, user: &CancellationToken) -> Stop {
        match (s == CANCELLED, user.is_cancelled()) {
            (true, true) => Stop::Cancelled,
            (true, false) => Stop::Deadline,
            _ => Stop::Network(s),
        }
    }

    fn into_tv(self) -> TvError {
        match self {
            Stop::Cancelled => TvError::Cancelled,
            Stop::Deadline => TvError::Network("timed out".into()),
            Stop::Network(s) => TvError::Network(s),
        }
    }
}

struct Ctx<'a> {
    ep: &'a LoungeEndpoints,
    policy: FetchPolicy,
    /// The caller's token — only a real cancel.
    user: &'a CancellationToken,
    /// The user's token plus the deadline timer.
    work: &'a CancellationToken,
}

impl Ctx<'_> {
    async fn send(&self, req: GuardedRequest) -> Result<reqwest::Response, Stop> {
        send_guarded(req, self.policy, self.work).await.map_err(|s| Stop::from(s, self.user))
    }

    /// A whole small reply, status checked.
    async fn reply(&self, req: GuardedRequest) -> Result<Vec<u8>, Stop> {
        let resp = self.send(req).await?;
        if !resp.status().is_success() {
            return Err(Stop::Network(format!("HTTP {}", resp.status().as_u16())));
        }
        read_capped(resp, MAX_REPLY_BYTES, self.work).await.map_err(|s| Stop::from(s, self.user))
    }

    /// `screens[0].loungeToken`; no screen is `Unlinked`.
    async fn lounge_token(&self, screen_id: &str) -> Result<String, TvError> {
        let req = post_form(self.ep.url("/pairing/get_lounge_token_batch"), form(&[("screen_ids", screen_id)]));
        let bytes = self.reply(req).await.map_err(Stop::into_tv)?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| TvError::Network("unexpected reply".into()))?;
        match v["screens"].get(0).and_then(|s| s["loungeToken"].as_str()).filter(|t| !t.is_empty()) {
            Some(t) => Ok(t.to_owned()),
            None => Err(TvError::Unlinked),
        }
    }
}

/// Feed `bytes` through `reader` into `acc`, telling `trace` of each event.
fn absorb(reader: &mut ChunkReader, bytes: &[u8], acc: &mut Accumulator, trace: &mut (dyn FnMut(&LoungeEvent) + Send)) {
    for ev in reader.push(bytes) {
        trace(&ev);
        acc.feed(&ev);
    }
}

/// Session ids from a bind reply: (SID, gsessionid).
type Session = (String, String);

/// `bc/bind`: opens a remote-control session. The reply carries the session
/// ids and usually the TV's state. Sends no command, `method`, `params` or
/// video ID (anti-goal: Jodd never changes the TV).
async fn bind(
    ctx: &Ctx<'_>,
    token: &str,
    device_id: &str,
    acc: &mut Accumulator,
    trace: &mut (dyn FnMut(&LoungeEvent) + Send),
) -> Result<Session, TvError> {
    let url = format!("{}?{}", ctx.ep.url("/bc/bind"), form(&[("RID", "1"), ("VER", VER), ("CVER", CVER), ("auth_failure_option", "send_error")]));
    let body = form(&[
        ("app", APP),
        ("mdx-version", MDX_VERSION),
        ("name", DEVICE_NAME),
        ("id", device_id),
        ("device", DEVICE),
        ("capabilities", CAPABILITIES),
        ("ui", "false"),
        ("theme", "cl"),
        ("loungeIdToken", token),
    ]);
    let bytes = ctx.reply(post_form(url, body)).await.map_err(Stop::into_tv)?;
    let mut reader = ChunkReader::new();
    absorb(&mut reader, &bytes, acc, trace);
    absorb(&mut reader, b"\n", acc, trace); // flush a last line with no newline
    acc.session().ok_or_else(|| TvError::Network("the TV sent no session".into()))
}

/// The query both the command and the subscribe carry.
fn session_query<'a>(
    session: &'a Session,
    rid: &'a str,
    aid: &'a str,
    token: &'a str,
    device_id: &'a str,
    stream: bool,
) -> Vec<(&'a str, &'a str)> {
    let mut q = vec![("SID", session.0.as_str()), ("gsessionid", session.1.as_str()), ("RID", rid)];
    if stream {
        q.extend([("TYPE", "xmlhttp"), ("CI", "0")]);
    }
    q.extend([("VER", VER), ("CVER", CVER)]);
    if stream {
        q.push(("v", "2"));
    }
    q.extend([("AID", aid), ("loungeIdToken", token), ("device", DEVICE), ("app", APP), ("name", DEVICE_NAME), ("id", device_id)]);
    q
}

/// Is the TV linked and answering? Token + bind only — never asks what plays.
/// `Ok(model)` is "Hisense HiSmartTV A4" when the TV reports one.
pub async fn check(
    ep: &LoungeEndpoints,
    policy: FetchPolicy,
    screen_id: &str,
    device_id: &str,
    cancel: &CancellationToken,
) -> Result<Option<String>, TvError> {
    let deadline = Deadline::new(cancel, Deadlines::default().overall);
    let ctx = Ctx { ep, policy, user: cancel, work: &deadline.token };
    let token = ctx.lounge_token(screen_id).await?;
    let mut acc = Accumulator::default();
    bind(&ctx, &token, device_id, &mut acc, &mut |_| {}).await?;
    Ok(acc.tv_model())
}

/// Pair with the code the TV shows under Settings → Link with TV code.
pub async fn pair(ep: &LoungeEndpoints, policy: FetchPolicy, code: &str, cancel: &CancellationToken) -> Result<Pairing, PairError> {
    let code = normalize_code(code)?;
    let ctx = Ctx { ep, policy, user: cancel, work: cancel };
    let req = post_form(ep.url("/pairing/get_screen"), form(&[("pairing_code", code.as_str())]));
    let resp = ctx.send(req).await.map_err(|s| match s {
        Stop::Network(s) => PairError::Network(s),
        Stop::Cancelled | Stop::Deadline => PairError::Network(CANCELLED.into()),
    })?;
    if resp.status().as_u16() == 404 {
        return Err(PairError::BadCode);
    }
    if !resp.status().is_success() {
        return Err(PairError::Network(format!("HTTP {}", resp.status().as_u16())));
    }
    let bytes = read_capped(resp, MAX_REPLY_BYTES, cancel).await.map_err(PairError::Network)?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| PairError::Network("unexpected reply".into()))?;
    let screen = &v["screen"];
    let screen_id = screen["screenId"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| PairError::Network("unexpected reply".into()))?;
    let name = screen["name"].as_str().filter(|s| !s.is_empty()).unwrap_or("YouTube on TV");
    Ok(Pairing { screen_id: screen_id.to_owned(), screen_name: name.to_owned() })
}

/// Which video the TV is on, and how far in (spec §5.1).
pub async fn now_playing(
    ep: &LoungeEndpoints,
    policy: FetchPolicy,
    screen_id: &str,
    device_id: &str,
    deadlines: Deadlines,
    cancel: &CancellationToken,
) -> Result<(Sighting, Option<String>), TvError> {
    now_playing_traced(ep, policy, screen_id, device_id, deadlines, cancel, &mut |_| {}).await
}

/// [`now_playing`], telling `trace` of every event parsed (bind and subscribe).
/// The diagnostic probe uses it; nothing else should need to.
pub async fn now_playing_traced(
    ep: &LoungeEndpoints,
    policy: FetchPolicy,
    screen_id: &str,
    device_id: &str,
    deadlines: Deadlines,
    cancel: &CancellationToken,
    trace: &mut (dyn FnMut(&LoungeEvent) + Send),
) -> Result<(Sighting, Option<String>), TvError> {
    let deadline = Deadline::new(cancel, deadlines.overall);
    let ctx = Ctx { ep, policy, user: cancel, work: &deadline.token };
    let token = ctx.lounge_token(screen_id).await?;
    let mut acc = Accumulator::default();
    let session = bind(&ctx, &token, device_id, &mut acc, trace).await?;

    // When the after-video window opens: set by `listen` once it is listening
    // (R23), never here — bind may already name the video, and the command
    // POST and the subscribe's headers would otherwise eat the window.
    let mut video_since: Option<Instant> = None;
    // Did the TV answer at all? Only an answer makes "nothing playing" true.
    let mut failure: Option<TvError> = None;
    if !has_time(&acc) {
        match listen(&ctx, &deadline, deadlines, &token, device_id, &session, &mut acc, &mut video_since, trace).await {
            Ok(Heard::Clean) => {}
            Ok(Heard::Broke) => failure = Some(TvError::Network("the connection to the TV broke".into())),
            Err(Stop::Deadline) => failure = Some(TvError::Network("timed out".into())),
            Err(other) => return Err(other.into_tv()),
        }
    }
    if cancel.is_cancelled() {
        return Err(TvError::Cancelled);
    }
    // A sighting already made always wins; a failure with none says nothing
    // about the TV, so it is reported as a failure.
    match (acc.sighting(), failure) {
        (Some(s), _) => Ok((s, acc.tv_model())),
        (None, Some(failure)) => Err(failure),
        (None, None) => Err(TvError::NothingPlaying),
    }
}

fn has_time(acc: &Accumulator) -> bool {
    acc.sighting().is_some_and(|s| s.seconds.is_some())
}

/// How the stream ended, once the subscribe's headers had arrived.
enum Heard {
    /// The stream ended, or ran to a deadline: the TV was heard.
    Clean,
    /// A read failed mid-stream.
    Broke,
}

/// Ask `getNowPlaying`, then read the event stream until there is a video and
/// a time, the stream ends, or a deadline passes. `Err(Deadline)` is not a
/// failure by itself — the caller decides, from what the accumulator holds,
/// whether it is a timeout or a sighting.
#[allow(clippy::too_many_arguments)]
async fn listen(
    ctx: &Ctx<'_>,
    deadline: &Deadline,
    deadlines: Deadlines,
    token: &str,
    device_id: &str,
    session: &Session,
    acc: &mut Accumulator,
    video_since: &mut Option<Instant>,
    trace: &mut (dyn FnMut(&LoungeEvent) + Send),
) -> Result<Heard, Stop> {
    let aid = acc.last_event_id().to_string();
    // The command. Its reply is read from the stream below, not from here.
    let url = format!("{}?{}", ctx.ep.url("/bc/bind"), form(&session_query(session, "2", &aid, token, device_id, false)));
    let ask = ctx.send(post_form(url, form(&[("count", "1"), ("ofs", "0"), ("req0__sc", COMMAND)]))).await?;
    if !ask.status().is_success() {
        return Err(Stop::Network(format!("HTTP {}", ask.status().as_u16())));
    }
    drop(ask);

    let url = format!("{}?{}", ctx.ep.url("/bc/bind"), form(&session_query(session, "rpc", &aid, token, device_id, true)));
    let mut resp = ctx.send(GuardedRequest::get(&url)).await?;
    if !resp.status().is_success() {
        return Err(Stop::Network(format!("HTTP {}", resp.status().as_u16())));
    }
    // Listening starts now. A video bind already named gets its after-video
    // window from here, not from when bind ended: the command POST, the
    // subscribe headers and the TV's first `nowPlaying` all come after (R23).
    // A video first seen in the stream opens its window at that sighting.
    if video_since.is_none() && acc.sighting().is_some() {
        *video_since = Some(Instant::now());
    }
    let mut reader = ChunkReader::new();
    let mut total = 0usize;
    let mut heard = Heard::Clean;
    while !has_time(acc) {
        let stop_at = match *video_since {
            Some(t) => deadline.at.min(t + deadlines.after_video),
            None => deadline.at,
        };
        tokio::select! {
            biased;
            _ = ctx.work.cancelled() => break,
            _ = tokio::time::sleep_until(stop_at) => break,
            chunk = resp.chunk() => match chunk {
                Ok(Some(bytes)) => {
                    total += bytes.len();
                    absorb(&mut reader, &bytes, acc, trace);
                    if video_since.is_none() && acc.sighting().is_some() {
                        *video_since = Some(Instant::now());
                    }
                    if total > MAX_STREAM_BYTES { break; }
                }
                Ok(None) => {
                    absorb(&mut reader, b"\n", acc, trace);
                    break;
                }
                // broke mid-stream: what the TV already said still counts
                Err(_) => {
                    absorb(&mut reader, b"\n", acc, trace);
                    heard = Heard::Broke;
                    break;
                }
            },
        }
    }
    if ctx.user.is_cancelled() { Err(Stop::Cancelled) } else { Ok(heard) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RUN1_BIND: &str = include_str!("../../tests/fixtures/lounge/run1-bind.txt");
    const RUN1_SUB: &str = include_str!("../../tests/fixtures/lounge/run1-subscribe.txt");
    const RUN2_BIND: &str = include_str!("../../tests/fixtures/lounge/run2-bind.txt");
    const RUN2_SUB: &str = include_str!("../../tests/fixtures/lounge/run2-subscribe.txt");
    const RUN3_BIND: &str = include_str!("../../tests/fixtures/lounge/run3-bind.txt");
    const RUN3_SUB: &str = include_str!("../../tests/fixtures/lounge/run3-subscribe.txt");

    fn run(bind: &str, sub: &str) -> Vec<LoungeEvent> {
        let mut v = parse_all(bind);
        v.extend(parse_all(sub));
        v
    }

    fn fed(events: &[LoungeEvent]) -> Accumulator {
        let mut acc = Accumulator::default();
        for e in events {
            acc.feed(e);
        }
        acc
    }

    fn ev(id: i64, name: &str, arg: Value) -> LoungeEvent {
        LoungeEvent { id, name: name.into(), args: vec![arg] }
    }

    #[test]
    fn run2_full_now_playing() {
        let acc = fed(&run(RUN2_BIND, RUN2_SUB));
        assert_eq!(
            acc.sighting(),
            Some(Sighting {
                video_id: "uK660HUX-1Y".into(),
                seconds: Some(32),
                duration: Some(790),
                state: Some("1".into()),
            })
        );
    }

    #[test]
    fn run1_video_from_track_events_without_time() {
        let s = fed(&run(RUN1_BIND, RUN1_SUB)).sighting().unwrap();
        assert_eq!(s.video_id, "8A4qEKgaMTk");
        assert_eq!(s.seconds, None);
    }

    #[test]
    fn run3_never_takes_autoplay_up_next() {
        let mut acc = Accumulator::default();
        for e in run(RUN3_BIND, RUN3_SUB) {
            acc.feed(&e);
            if let Some(s) = acc.sighting() {
                assert_ne!(s.video_id, "wGL0WZpHyO8");
                assert_ne!(s.video_id, "MQTOFyKFM2o");
            }
        }
        let s = acc.sighting().unwrap();
        assert_eq!(s.video_id, "wnXoCNd57Ks");
        assert_eq!(s.seconds, Some(147));
        assert_eq!(s.state, Some("2".into()));
    }

    #[test]
    fn track_event_beats_up_next_synthetic() {
        let acc = fed(&[
            ev(1, "onVideoQualityChanged", json!({"videoId": "AAAAAAAAAAA"})),
            ev(2, "autoplayUpNext", json!({"videoId": "BBBBBBBBBBB"})),
        ]);
        assert_eq!(acc.sighting().unwrap().video_id, "AAAAAAAAAAA");
    }

    #[test]
    fn playlist_modified_is_last_resort() {
        let mut acc = Accumulator::default();
        acc.feed(&ev(1, "playlistModified", json!({"videoId": "AAAAAAAAAAA"})));
        assert_eq!(acc.sighting().unwrap().video_id, "AAAAAAAAAAA");
        acc.feed(&ev(2, "onAudioTrackChanged", json!({"videoId": "CCCCCCCCCCC"})));
        assert_eq!(acc.sighting().unwrap().video_id, "CCCCCCCCCCC");
        // a later, lower-tier event does not take it back
        acc.feed(&ev(3, "playlistModified", json!({"videoId": "AAAAAAAAAAA"})));
        assert_eq!(acc.sighting().unwrap().video_id, "CCCCCCCCCCC");
    }

    #[test]
    fn time_only_from_matching_video() {
        let acc = fed(&[
            ev(1, "nowPlaying", json!({"videoId": "AAAAAAAAAAA", "currentTime": "50"})),
            ev(2, "onStateChange", json!({"videoId": "BBBBBBBBBBB", "currentTime": "9"})),
        ]);
        let s = acc.sighting().unwrap();
        assert_eq!(s.video_id, "BBBBBBBBBBB");
        assert_eq!(s.seconds, Some(9));
        // and A's time is never lent to B when B's event has none
        let acc = fed(&[
            ev(1, "nowPlaying", json!({"videoId": "AAAAAAAAAAA", "currentTime": "50"})),
            ev(2, "onStateChange", json!({"videoId": "BBBBBBBBBBB", "state": "1"})),
        ]);
        let s = acc.sighting().unwrap();
        assert_eq!((s.video_id.as_str(), s.seconds), ("BBBBBBBBBBB", None));
    }

    #[test]
    fn duplicate_ids_counted_once() {
        let events = run(RUN2_BIND, RUN2_SUB);
        let once = fed(&events);
        let mut twice = fed(&events);
        for e in &events {
            twice.feed(e);
        }
        assert_eq!(once.sighting(), twice.sighting());
        assert_eq!(once.last_event_id(), twice.last_event_id());
        assert_eq!(twice.last_event_id(), 13);
    }

    #[test]
    fn session_ids() {
        let acc = fed(&parse_all(RUN3_BIND));
        assert_eq!(acc.session(), Some(("SID_REDACTED".into(), "GSESSION_REDACTED".into())));
    }

    #[test]
    fn tv_model_from_lounge_status() {
        let acc = fed(&parse_all(RUN2_BIND));
        assert_eq!(acc.tv_model(), Some("Hisense HiSmartTV A4".into()));
    }

    #[test]
    fn chunk_reader_split_mid_json() {
        let bytes = RUN2_SUB.as_bytes();
        // inside the first JSON array, past its first event
        let cut = RUN2_SUB.find("onVolumeChanged").unwrap();
        let mut reader = ChunkReader::new();
        let mut got = reader.push(&bytes[..cut]);
        got.extend(reader.push(&bytes[cut..]));
        assert_eq!(got, parse_all(RUN2_SUB));
        assert!(!got.is_empty());
    }

    #[test]
    fn chunk_reader_keeps_a_character_split_across_pushes() {
        let thai = "เสียงต้นฉบับภาษาไทย";
        let body = format!("40\n[[1,[\"onAudioTrackListChanged\",{{\"t\":\"{thai}\"}}]]\n]\n");
        let bytes = body.as_bytes();
        let at = body.find('เ').unwrap() + 1; // one byte into a 3-byte character
        assert!(!body.is_char_boundary(at));
        let mut reader = ChunkReader::new();
        let mut got = reader.push(&bytes[..at]);
        got.extend(reader.push(&bytes[at..]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].args[0]["t"], thai);
    }

    #[test]
    fn invalid_video_id_rejected() {
        let acc = fed(&[ev(1, "nowPlaying", json!({"videoId": "../../x", "currentTime": "50"}))]);
        assert_eq!(acc.sighting(), None);
    }

    // ── Client half ───────────────────────────────────────────────────────
    // Every mock below is `tracked`: it records the request it matched and
    // REFUSES (so the test fails with a 501) any request whose query or body
    // carries `method`, `params` or `videoId`, or a `req0__sc` other than
    // `getNowPlaying` — the anti-goal, enforced per request. The aggregate
    // test then asserts over everything recorded.

    use std::sync::{Arc, Mutex};

    use mockito::{Matcher, Mock, Server, ServerGuard};

    const POLICY: FetchPolicy = FetchPolicy { allow_loopback: true };
    const SCREEN: &str = "SCREEN_ID_ABC";
    const LOUNGE_TOKEN: &str = "LOUNGE_TOKEN_XYZ";
    const DEVICE_ID: &str = "00000000-0000-4000-8000-0000000000aa";

    #[derive(Debug, Clone)]
    struct Sent {
        target: String,
        body: String,
    }
    type Log = Arc<Mutex<Vec<Sent>>>;

    fn pairs_of(raw: &str) -> Vec<(String, String)> {
        Url::parse(&format!("http://x.invalid/?{raw}")).unwrap().query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
    }

    /// What is wrong with a request, if anything — the anti-goal.
    fn violations(target: &str, body: &str) -> Vec<String> {
        let query = target.split_once('?').map_or("", |(_, q)| q);
        let mut out = Vec::new();
        for (what, raw) in [("query", query), ("body", body)] {
            for needle in ["method=", "params=", "videoId="] {
                if raw.contains(needle) {
                    out.push(format!("{what} contains {needle}"));
                }
            }
            for (k, v) in pairs_of(raw) {
                if matches!(k.as_str(), "method" | "params" | "videoId") {
                    out.push(format!("{what} carries key {k}"));
                }
                if k == "req0__sc" && v != "getNowPlaying" {
                    out.push(format!("{what} sends command {v}"));
                }
            }
        }
        out
    }

    fn tracked(mock: Mock, log: &Log) -> Mock {
        let log = log.clone();
        mock.match_request(move |req| {
            let body = req.body().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
            let target = req.path_and_query().to_owned();
            let clean = violations(&target, &body).is_empty();
            log.lock().unwrap().push(Sent { target, body });
            clean
        })
    }

    struct Rig {
        server: ServerGuard,
        log: Log,
        mocks: Vec<Mock>,
    }

    impl Rig {
        async fn new() -> Rig {
            Rig::sharing(Arc::default()).await
        }
        async fn sharing(log: Log) -> Rig {
            Rig { server: Server::new_async().await, log, mocks: Vec::new() }
        }
        fn ep(&self) -> LoungeEndpoints {
            LoungeEndpoints { base: self.server.url() }
        }
        async fn keep(&mut self, mock: Mock) {
            let m = tracked(mock, &self.log).create_async().await;
            self.mocks.push(m);
        }
        async fn token(&mut self, reply: &str) {
            let m = self
                .server
                .mock("POST", "/pairing/get_lounge_token_batch")
                .match_body(Matcher::Exact(format!("screen_ids={SCREEN}")))
                .with_body(reply)
                .expect(1);
            self.keep(m).await;
        }
        async fn good_token(&mut self) {
            self.token(&format!(r#"{{"screens":[{{"screenId":"{SCREEN}","loungeToken":"{LOUNGE_TOKEN}"}}]}}"#)).await;
        }
        async fn bind(&mut self, body: &str) {
            let m = self.server.mock("POST", "/bc/bind").match_query(Matcher::UrlEncoded("RID".into(), "1".into())).with_body(body).expect(1);
            self.keep(m).await;
        }
        /// The `getNowPlaying` POST: its body must be EXACTLY this.
        async fn command(&mut self, hits: usize) {
            let m = self
                .server
                .mock("POST", "/bc/bind")
                .match_query(Matcher::UrlEncoded("RID".into(), "2".into()))
                .match_body(Matcher::Exact("count=1&ofs=0&req0__sc=getNowPlaying".into()))
                .with_body("")
                .expect(hits);
            self.keep(m).await;
        }
        async fn subscribe(&mut self, body: &str, hits: usize) {
            let m = self.server.mock("GET", "/bc/bind").match_query(Matcher::UrlEncoded("RID".into(), "rpc".into())).with_body(body).expect(hits);
            self.keep(m).await;
        }
        /// Every mock was hit as many times as it expected.
        fn verify(&self) {
            for m in &self.mocks {
                m.assert();
            }
        }
    }

    fn chunk(events: &[&str]) -> String {
        format!("999\n[{}\n]\n", events.join("\n,"))
    }

    fn bind_with_playing() -> String {
        chunk(&[
            r#"[0,["c","SID1","",8]]"#,
            r#"[1,["S","GS1"]]"#,
            r#"[2,["nowPlaying",{"videoId":"AAAAAAAAAAA","currentTime":"72.5"}]]"#,
        ])
    }

    fn bind_without_video() -> String {
        chunk(&[r#"[0,["c","SID1","",8]]"#, r#"[1,["S","GS1"]]"#, r#"[2,["noop"]]"#])
    }

    fn quick() -> Deadlines {
        Deadlines { overall: Duration::from_secs(5), after_video: Duration::from_millis(300) }
    }

    async fn run_now(rig: &Rig, deadlines: Deadlines) -> Result<(Sighting, Option<String>), TvError> {
        now_playing(&rig.ep(), POLICY, SCREEN, DEVICE_ID, deadlines, &CancellationToken::new()).await
    }

    #[test]
    fn normalize_code_accepts_spaces_and_dashes() {
        assert_eq!(normalize_code("857 907 240 183"), Ok("857907240183".into()));
        assert_eq!(normalize_code("857-907-240-183"), Ok("857907240183".into()));
        assert_eq!(normalize_code("12345"), Err(PairError::BadCode));
        assert_eq!(normalize_code("8579072401831"), Err(PairError::BadCode));
    }

    #[tokio::test]
    async fn pair_bad_code_is_404() {
        let mut rig = Rig::new().await;
        let m = rig.server.mock("POST", "/pairing/get_screen").with_status(404).with_body("<html><body>Not Found</body></html>");
        rig.keep(m).await;
        let got = pair(&rig.ep(), POLICY, "857 907 240 183", &CancellationToken::new()).await;
        assert_eq!(got, Err(PairError::BadCode));
    }

    #[tokio::test]
    async fn pair_reads_screen() {
        let mut rig = Rig::new().await;
        let m = rig
            .server
            .mock("POST", "/pairing/get_screen")
            .match_header("content-type", "application/x-www-form-urlencoded")
            .match_body(Matcher::Exact("pairing_code=857907240183".into()))
            .with_body(r#"{"screen":{"screenId":"S1","name":"YouTube on TV","loungeToken":"T"}}"#)
            .expect(1);
        rig.keep(m).await;
        let got = pair(&rig.ep(), POLICY, "857-907-240-183", &CancellationToken::new()).await;
        assert_eq!(got, Ok(Pairing { screen_id: "S1".into(), screen_name: "YouTube on TV".into() }));
        rig.verify();
    }

    #[test]
    fn pairing_debug_redacts() {
        let p = Pairing { screen_id: "S1_SECRET".into(), screen_name: "YouTube on TV".into() };
        let shown = format!("{p:?}");
        assert!(!shown.contains("S1_SECRET"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
        assert!(shown.contains("YouTube on TV"), "{shown}");
    }

    #[tokio::test]
    async fn stops_after_bind_when_bind_has_video_and_time() {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        rig.bind(&bind_with_playing()).await;
        rig.command(0).await;
        rig.subscribe("", 0).await;
        let (s, model) = run_now(&rig, Deadlines::default()).await.unwrap();
        assert_eq!((s.video_id.as_str(), s.seconds), ("AAAAAAAAAAA", Some(72)));
        assert_eq!(model, None);
        rig.verify();
    }

    #[tokio::test]
    async fn asks_and_subscribes_otherwise() {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        rig.bind(RUN3_BIND).await;
        rig.command(1).await; // exact body: count=1&ofs=0&req0__sc=getNowPlaying
        rig.subscribe(RUN3_SUB, 1).await;
        let (s, _) = run_now(&rig, Deadlines::default()).await.unwrap();
        assert_eq!(s.video_id, "wnXoCNd57Ks");
        assert_eq!(s.seconds, Some(147));
        rig.verify();
    }

    #[tokio::test]
    async fn bind_carries_the_measured_parameters_and_the_session() {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        let m = rig
            .server
            .mock("POST", "/bc/bind")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("RID".into(), "1".into()),
                Matcher::UrlEncoded("VER".into(), "8".into()),
                Matcher::UrlEncoded("CVER".into(), "1".into()),
                Matcher::UrlEncoded("auth_failure_option".into(), "send_error".into()),
            ]))
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded("app".into(), "web".into()),
                Matcher::UrlEncoded("mdx-version".into(), "3".into()),
                Matcher::UrlEncoded("name".into(), "Jodd".into()),
                Matcher::UrlEncoded("id".into(), DEVICE_ID.into()),
                Matcher::UrlEncoded("device".into(), "REMOTE_CONTROL".into()),
                Matcher::UrlEncoded("capabilities".into(), "que,dsdtr,atp".into()),
                Matcher::UrlEncoded("ui".into(), "false".into()),
                Matcher::UrlEncoded("theme".into(), "cl".into()),
                Matcher::UrlEncoded("loungeIdToken".into(), LOUNGE_TOKEN.into()),
            ]))
            .with_body(bind_without_video())
            .expect(1);
        rig.keep(m).await;
        // the command and the stream carry the session the bind returned
        let m = rig
            .server
            .mock("POST", "/bc/bind")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("SID".into(), "SID1".into()),
                Matcher::UrlEncoded("gsessionid".into(), "GS1".into()),
                Matcher::UrlEncoded("RID".into(), "2".into()),
                Matcher::UrlEncoded("AID".into(), "2".into()),
                Matcher::UrlEncoded("loungeIdToken".into(), LOUNGE_TOKEN.into()),
            ]))
            .with_body("")
            .expect(1);
        rig.keep(m).await;
        let m = rig
            .server
            .mock("GET", "/bc/bind")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("SID".into(), "SID1".into()),
                Matcher::UrlEncoded("gsessionid".into(), "GS1".into()),
                Matcher::UrlEncoded("RID".into(), "rpc".into()),
                Matcher::UrlEncoded("TYPE".into(), "xmlhttp".into()),
                Matcher::UrlEncoded("CI".into(), "0".into()),
                Matcher::UrlEncoded("v".into(), "2".into()),
                Matcher::UrlEncoded("AID".into(), "2".into()),
            ]))
            .with_body(chunk(&[r#"[3,["noop"]]"#]))
            .expect(1);
        rig.keep(m).await;
        assert_eq!(run_now(&rig, quick()).await, Err(TvError::NothingPlaying));
        rig.verify();
    }

    #[tokio::test]
    async fn video_without_time_returns_when_stream_ends() {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        rig.bind(RUN1_BIND).await;
        rig.command(1).await;
        rig.subscribe(RUN1_SUB, 1).await;
        let started = std::time::Instant::now();
        let deadlines = Deadlines { overall: Duration::from_secs(2), after_video: Duration::from_millis(200) };
        let (s, _) = run_now(&rig, deadlines).await.unwrap();
        assert_eq!(s.video_id, "8A4qEKgaMTk");
        assert_eq!(s.seconds, None);
        assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
        rig.verify();
    }

    #[tokio::test]
    async fn video_without_time_stops_after_the_after_video_wait_when_the_stream_hangs() {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        rig.bind(&bind_without_video()).await;
        rig.command(1).await;
        let first = chunk(&[r#"[3,["onVideoQualityChanged",{"videoId":"AAAAAAAAAAA"}]]"#]);
        let m = rig
            .server
            .mock("GET", "/bc/bind")
            .match_query(Matcher::UrlEncoded("RID".into(), "rpc".into()))
            .with_chunked_body(move |w| {
                w.write_all(first.as_bytes())?;
                w.flush()?;
                std::thread::sleep(Duration::from_millis(2500)); // the TV goes quiet
                Ok(())
            })
            .expect(1);
        rig.keep(m).await;
        let started = std::time::Instant::now();
        let (s, _) = run_now(&rig, quick()).await.unwrap();
        assert_eq!((s.video_id.as_str(), s.seconds), ("AAAAAAAAAAA", None));
        // 300 ms after the video, not the 2.5 s the stream stayed open (nor the 5 s overall)
        assert!(started.elapsed() < Duration::from_millis(2000), "took {:?}", started.elapsed());
    }

    /// A rig whose bind answered with a session and no video, and whose
    /// subscribe is `body_fn`.
    async fn rig_with_stream(
        body_fn: impl Fn(&mut dyn std::io::Write) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Rig {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        rig.bind(&bind_without_video()).await;
        rig.command(1).await;
        let m = rig
            .server
            .mock("GET", "/bc/bind")
            .match_query(Matcher::UrlEncoded("RID".into(), "rpc".into()))
            .with_chunked_body(move |w| body_fn(w))
            .expect(1);
        rig.keep(m).await;
        rig
    }

    fn short_overall() -> Deadlines {
        Deadlines { overall: Duration::from_millis(500), after_video: Duration::from_millis(200) }
    }

    #[tokio::test]
    async fn a_stream_that_breaks_before_any_video_is_a_network_error_not_nothing_playing() {
        let rig = rig_with_stream(|w| {
            w.write_all(chunk(&[r#"[3,["noop"]]"#]).as_bytes())?;
            w.flush()?;
            std::thread::sleep(Duration::from_millis(100));
            Err(std::io::Error::other("connection reset"))
        })
        .await;
        let err = run_now(&rig, quick()).await.unwrap_err();
        assert!(matches!(err, TvError::Network(_)), "{err:?}");
        let shown = format!("{err:?}");
        for secret in [LOUNGE_TOKEN, SCREEN, "127.0.0.1", "http"] {
            assert!(!shown.contains(secret), "{secret} leaked into {shown}");
        }
    }

    #[tokio::test]
    async fn a_stream_that_breaks_after_a_video_keeps_the_sighting() {
        let rig = rig_with_stream(|w| {
            w.write_all(chunk(&[r#"[3,["onVideoQualityChanged",{"videoId":"AAAAAAAAAAA"}]]"#]).as_bytes())?;
            w.flush()?;
            std::thread::sleep(Duration::from_millis(100));
            Err(std::io::Error::other("connection reset"))
        })
        .await;
        let (s, _) = run_now(&rig, quick()).await.unwrap();
        assert_eq!((s.video_id.as_str(), s.seconds), ("AAAAAAAAAAA", None));
    }

    /// One HTTP request read off a raw socket: (method, target, body).
    async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, String, String)> {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let (head_end, len) = loop {
            let mut tmp = [0u8; 4096];
            let n = sock.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                break (i + 4, len);
            }
        };
        while buf.len() < head_end + len {
            let mut tmp = [0u8; 4096];
            let n = sock.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let request_line = head.lines().next().unwrap_or_default().to_owned();
        let mut parts = request_line.split(' ');
        let (method, target) = (parts.next().unwrap_or_default().to_owned(), parts.next().unwrap_or_default().to_owned());
        let body = String::from_utf8_lossy(&buf[head_end..head_end + len]).into_owned();
        Some((method, target, body))
    }

    /// mockito sends response headers before it runs a body closure, so it
    /// cannot play a server that never answers. This one can: token, bind and
    /// command are answered; the subscribe GET is read and then left hanging
    /// with no status line. Requests are recorded into `log` for the anti-goal.
    async fn server_that_never_answers_the_subscribe(log: Log) -> (LoungeEndpoints, tokio::task::JoinHandle<()>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let log = log.clone();
                tokio::spawn(async move {
                    let Some((method, target, body)) = read_request(&mut sock).await else { return };
                    log.lock().unwrap().push(Sent { target: target.clone(), body });
                    let reply = if target.starts_with("/pairing/get_lounge_token_batch") {
                        format!(r#"{{"screens":[{{"screenId":"{SCREEN}","loungeToken":"{LOUNGE_TOKEN}"}}]}}"#)
                    } else if method == "POST" && target.contains("RID=1") {
                        bind_without_video()
                    } else if method == "POST" {
                        String::new()
                    } else {
                        tokio::time::sleep(Duration::from_secs(3)).await; // never a byte
                        return;
                    };
                    let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len());
                    let _ = sock.write_all(head.as_bytes()).await;
                });
            }
        });
        (LoungeEndpoints { base }, task)
    }

    #[tokio::test]
    async fn a_subscribe_that_never_answers_is_a_network_error_not_nothing_playing() {
        let log: Log = Arc::default();
        let (ep, server) = server_that_never_answers_the_subscribe(log.clone()).await;
        let started = std::time::Instant::now();
        let got = now_playing(&ep, POLICY, SCREEN, DEVICE_ID, short_overall(), &CancellationToken::new()).await;
        server.abort();
        assert_eq!(got, Err(TvError::Network("timed out".into())));
        assert!(started.elapsed() < Duration::from_millis(1500), "took {:?}", started.elapsed());
        // it really got as far as the subscribe, and still sent nothing but the one command
        let sent = log.lock().unwrap().clone();
        assert!(sent.iter().any(|s| s.target.contains("RID=rpc")), "{sent:?}");
        for s in &sent {
            assert_eq!(violations(&s.target, &s.body), Vec::<String>::new());
        }
    }

    /// A server on a slow link, for R23. Bind names a video and no time. The
    /// `getNowPlaying` POST and the subscribe's response headers each take
    /// `lag` to come back, and the TV's `nowPlaying` (with a time) follows the
    /// headers by `then`. mockito cannot delay headers, hence the raw socket.
    async fn slow_link_server(log: Log, lag: Duration, then: Duration) -> (LoungeEndpoints, tokio::task::JoinHandle<()>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let bind_names_a_video = chunk(&[
            r#"[0,["c","SID1","",8]]"#,
            r#"[1,["S","GS1"]]"#,
            r#"[2,["onVideoQualityChanged",{"videoId":"AAAAAAAAAAA"}]]"#,
        ]);
        let now_playing = chunk(&[r#"[3,["nowPlaying",{"videoId":"AAAAAAAAAAA","currentTime":"12.5"}]]"#]);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let (log, bind_names_a_video, now_playing) = (log.clone(), bind_names_a_video.clone(), now_playing.clone());
                tokio::spawn(async move {
                    let Some((method, target, body)) = read_request(&mut sock).await else { return };
                    log.lock().unwrap().push(Sent { target: target.clone(), body });
                    let (wait, reply) = if target.starts_with("/pairing/get_lounge_token_batch") {
                        (Duration::ZERO, format!(r#"{{"screens":[{{"screenId":"{SCREEN}","loungeToken":"{LOUNGE_TOKEN}"}}]}}"#))
                    } else if method == "POST" && target.contains("RID=1") {
                        (Duration::ZERO, bind_names_a_video)
                    } else if method == "POST" {
                        (lag, String::new())
                    } else {
                        (lag, now_playing)
                    };
                    tokio::time::sleep(wait).await; // nothing, not even a status line, until now
                    let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", reply.len());
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.flush().await;
                    if method == "GET" {
                        tokio::time::sleep(then).await; // headers are out; the TV takes its time to say what it is playing
                    }
                    let _ = sock.write_all(reply.as_bytes()).await;
                });
            }
        });
        (LoungeEndpoints { base }, task)
    }

    /// R23: the after-video window opens when listening starts, not when bind
    /// ends. Here bind already names the video; the command POST and the
    /// subscribe headers take 1.5 s each, so the TV's time arrives 3.5 s in —
    /// long after a window that began with bind (2 s) would have closed.
    #[tokio::test]
    async fn the_after_video_window_opens_when_listening_starts() {
        let log: Log = Arc::default();
        let (ep, server) = slow_link_server(log.clone(), Duration::from_millis(1500), Duration::from_millis(500)).await;
        let deadlines = Deadlines { overall: Duration::from_secs(6), after_video: Duration::from_secs(2) };
        let started = std::time::Instant::now();
        let got = now_playing(&ep, POLICY, SCREEN, DEVICE_ID, deadlines, &CancellationToken::new()).await;
        server.abort();
        let (s, _) = got.unwrap();
        assert_eq!((s.video_id.as_str(), s.seconds), ("AAAAAAAAAAA", Some(12)));
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
        for sent in log.lock().unwrap().iter() {
            assert_eq!(violations(&sent.target, &sent.body), Vec::<String>::new());
        }
    }

    #[tokio::test]
    async fn an_open_stream_with_no_video_until_the_deadline_is_nothing_playing() {
        let rig = rig_with_stream(|w| {
            w.write_all(chunk(&[r#"[3,["noop"]]"#]).as_bytes())?;
            w.flush()?;
            std::thread::sleep(Duration::from_millis(2000)); // the TV listens, says nothing
            Ok(())
        })
        .await;
        let started = std::time::Instant::now();
        let got = run_now(&rig, short_overall()).await;
        assert_eq!(got, Err(TvError::NothingPlaying));
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(450) && took < Duration::from_millis(1500), "took {took:?}");
    }

    #[tokio::test]
    async fn token_batch_without_screen_is_unlinked() {
        let mut rig = Rig::new().await;
        rig.token(r#"{"screens":[]}"#).await;
        rig.bind("").await;
        let got = run_now(&rig, quick()).await;
        assert_eq!(got, Err(TvError::Unlinked));
        // and `bind` was never reached
        assert!(!rig.mocks[1].matched());
    }

    #[tokio::test]
    async fn no_video_is_nothing_playing() {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        rig.bind(&bind_without_video()).await;
        rig.command(1).await;
        rig.subscribe(&chunk(&[r#"[3,["noop"]]"#]), 1).await;
        assert_eq!(run_now(&rig, quick()).await, Err(TvError::NothingPlaying));
        rig.verify();
    }

    #[tokio::test]
    async fn check_does_not_ask_now_playing() {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        rig.bind(RUN2_BIND).await;
        rig.command(0).await;
        rig.subscribe("", 0).await;
        let got = check(&rig.ep(), POLICY, SCREEN, DEVICE_ID, &CancellationToken::new()).await;
        assert_eq!(got, Ok(Some("Hisense HiSmartTV A4".into())));
        rig.verify();
    }

    #[tokio::test]
    async fn check_of_an_unlinked_tv_is_unlinked() {
        let mut rig = Rig::new().await;
        rig.token(r#"{"screens":[]}"#).await;
        let got = check(&rig.ep(), POLICY, SCREEN, DEVICE_ID, &CancellationToken::new()).await;
        assert_eq!(got, Err(TvError::Unlinked));
    }

    #[tokio::test]
    async fn cancel_returns_cancelled() {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let got = now_playing(&rig.ep(), POLICY, SCREEN, DEVICE_ID, quick(), &cancel).await;
        assert_eq!(got, Err(TvError::Cancelled));
        let got = check(&rig.ep(), POLICY, SCREEN, DEVICE_ID, &cancel).await;
        assert_eq!(got, Err(TvError::Cancelled));
    }

    #[tokio::test]
    async fn errors_never_carry_the_token_the_screen_or_a_url() {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        let m = rig.server.mock("POST", "/bc/bind").match_query(Matcher::UrlEncoded("RID".into(), "1".into())).with_status(500).with_body("boom").expect(1);
        rig.keep(m).await;
        let err = run_now(&rig, quick()).await.unwrap_err();
        let shown = format!("{err:?} {err:?}");
        assert!(matches!(err, TvError::Network(_)), "{shown}");
        for secret in [LOUNGE_TOKEN, SCREEN, DEVICE_ID, "127.0.0.1", "http"] {
            assert!(!shown.contains(secret), "{secret} leaked into {shown}");
        }
        // a dead server too: the transport error must not echo the URL
        let dead = LoungeEndpoints { base: "http://127.0.0.1:1".into() };
        let err = now_playing(&dead, POLICY, SCREEN, DEVICE_ID, quick(), &CancellationToken::new()).await.unwrap_err();
        let shown = format!("{err:?}");
        assert!(matches!(err, TvError::Network(_)), "{shown}");
        for secret in [SCREEN, "127.0.0.1", "http"] {
            assert!(!shown.contains(secret), "{secret} leaked into {shown}");
        }
    }

    #[tokio::test]
    async fn traced_sees_every_event_from_bind_and_subscribe() {
        let mut rig = Rig::new().await;
        rig.good_token().await;
        rig.bind(RUN3_BIND).await;
        rig.command(1).await;
        rig.subscribe(RUN3_SUB, 1).await;
        let mut names: Vec<String> = Vec::new();
        let mut trace = |e: &LoungeEvent| names.push(e.name.clone());
        let (s, _) = now_playing_traced(&rig.ep(), POLICY, SCREEN, DEVICE_ID, Deadlines::default(), &CancellationToken::new(), &mut trace)
            .await
            .unwrap();
        assert_eq!(s.video_id, "wnXoCNd57Ks");
        let expected: Vec<String> = run(RUN3_BIND, RUN3_SUB).into_iter().map(|e| e.name).collect();
        assert_eq!(names, expected);
        assert!(names.iter().any(|n| n == "autoplayUpNext")); // traced even though never a source
    }

    #[test]
    fn the_anti_goal_check_itself_catches_what_it_guards() {
        assert!(violations("/bc/bind?RID=1", "app=web&name=Jodd").is_empty());
        assert!(violations("/bc/bind?RID=2", "count=1&ofs=0&req0__sc=getNowPlaying").is_empty());
        assert!(!violations("/bc/bind?RID=1", "app=web&method=setPlaylist").is_empty());
        assert!(!violations("/bc/bind?RID=1", "app=web&params=x").is_empty());
        assert!(!violations("/bc/bind?RID=1&videoId=AAAAAAAAAAA", "").is_empty());
        assert!(!violations("/bc/bind?RID=2", "req0__sc=setPlaylist").is_empty());
    }

    /// The anti-goal (spec §7; ruling R4). Every flow the client has runs
    /// against mocks that all feed ONE log; across everything it sent, no
    /// request carries `method`, `params` or `videoId`, and the only
    /// `req0__sc` is `getNowPlaying`.
    #[tokio::test]
    async fn bind_never_sends_control_params() {
        let log: Log = Arc::default();

        // 1. a bind that already answers
        let mut rig = Rig::sharing(log.clone()).await;
        rig.good_token().await;
        rig.bind(&bind_with_playing()).await;
        run_now(&rig, quick()).await.unwrap();

        // 2. a bind that does not: command + subscribe follow
        let mut rig = Rig::sharing(log.clone()).await;
        rig.good_token().await;
        rig.bind(RUN3_BIND).await;
        rig.command(1).await;
        rig.subscribe(RUN3_SUB, 1).await;
        run_now(&rig, quick()).await.unwrap();
        rig.verify();

        // 3. the health check
        let mut rig = Rig::sharing(log.clone()).await;
        rig.good_token().await;
        rig.bind(RUN2_BIND).await;
        check(&rig.ep(), POLICY, SCREEN, DEVICE_ID, &CancellationToken::new()).await.unwrap();

        // 4. pairing
        let mut rig = Rig::sharing(log.clone()).await;
        let m = rig.server.mock("POST", "/pairing/get_screen").with_body(r#"{"screen":{"screenId":"S1","name":"TV"}}"#);
        rig.keep(m).await;
        pair(&rig.ep(), POLICY, "857907240183", &CancellationToken::new()).await.unwrap();

        let sent = log.lock().unwrap().clone();
        assert!(!sent.is_empty());
        let mut commands = Vec::new();
        for s in &sent {
            assert_eq!(violations(&s.target, &s.body), Vec::<String>::new(), "{} {}", s.target, s.body);
            commands.extend(pairs_of(&s.body).into_iter().chain(pairs_of(s.target.split_once('?').map_or("", |(_, q)| q))).filter(|(k, _)| k == "req0__sc").map(|(_, v)| v));
        }
        assert!(!commands.is_empty(), "the test must see the command, or it proves nothing");
        assert!(commands.iter().all(|c| c == "getNowPlaying"), "{commands:?}");
        // binds were really sent (RID=1) and really clean
        assert!(sent.iter().filter(|s| s.target.contains("RID=1")).count() >= 3);
    }
}
