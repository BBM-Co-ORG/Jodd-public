//! iCloud sign-in: a browser session, not an OAuth flow.
//!
//! **Nothing in `auth.rs` or `auth_ms.rs` is reusable here.** There is no PKCE
//! pair, no client secret, no token exchange and no loopback listener — only a
//! real browser session against Apple's own pages, in a Tauri webview.
//!
//! The principle, borrowed from icloud-md and stated so it is not eroded:
//! **own none of `idmsa.apple.com`; depend only on the result.** Apple's own
//! JavaScript runs whatever this month's password / 2FA / CAPTCHA flow is, and
//! Jodd reads the outcome. Jodd never touches the page's form fields.
//!
//! # Shape of this module
//!
//! Everything that can be decided without a webview is a **pure function over
//! plain data**, and everything that needs `WKWebView` is a thin driver at the
//! bottom. That split is not tidiness: the webview half can only be exercised
//! on a Mac with a real Apple ID, so anything left inside it is untested on
//! every machine that runs `cargo test`. The cookie matching, the client-config
//! parse and the completion discriminator are the parts that can be wrong in a
//! quiet, expensive way, and all three live above the seam.
//!
//! # What is deliberately NOT here yet
//!
//! **No account is created.** Component B1's own ordering puts the Advanced
//! Data Protection readability check (Component H) *before* persisting
//! anything, and that check needs the transport (`icloud/wire.rs`) to fetch a
//! note. So the last step of sign-in cannot land before those do; creating the
//! account now would mean either persisting an account whose notes may be
//! undecodable, or duplicating the gate later. What this module produces is a
//! validated [`IcloudSession`] — the object both of those steps consume.
//!
//! [`refuse_second_icloud_account`] is here already, because the reason it
//! exists (one cookie jar per install) is a property of this module's webview,
//! not of the vertical.

use serde::{Deserialize, Serialize};

/// Where sign-in happens. Apple's own pages, start to finish.
pub const ICLOUD_URL: &str = "https://www.icloud.com/";

/// The bootstrap service. `/validate` against this host is the **first call of
/// any session**: it confirms the session AND reports which
/// `p<N>-ckdatabasews` partition serves the account — a host that cannot be
/// guessed and is not the same for two Apple IDs.
pub const SETUP_HOST: &str = "https://setup.icloud.com";

/// The hostname `SETUP_HOST` resolves to, kept separately because cookie
/// scoping is decided by host, not by URL. Tests point the HTTP call at a local
/// mock server; the cookie header they assert on must still be the one the real
/// host would receive.
pub const SETUP_HOSTNAME: &str = "setup.icloud.com";

/// Path of the validate endpoint, needed for RFC 6265 path matching.
pub const VALIDATE_PATH: &str = "/setup/ws/1/validate";

/// Every cookie this module's own script writes starts with this.
///
/// Two jobs. It namespaces our markers away from Apple's, and it is what
/// [`cookie_header_for`] filters on so **Jodd's own bookkeeping is never sent
/// to Apple**. A stray cookie in a request to `setup.icloud.com` would be
/// harmless today and is still the wrong default: the harvest exists to
/// reproduce what a browser would send, and a browser would not have invented
/// these.
pub const MARKER_PREFIX: &str = "jodd_icloud_";

/// Carries the client version strings captured off Apple's own first request.
pub const CLIENT_CFG_COOKIE: &str = "jodd_icloud_cfg";

/// Injected into the icloud.com webview at document-start (Component B5).
///
/// It wraps `fetch` and `XMLHttpRequest.open`, reads the client version
/// parameters off Apple's **own** first request to `setup.icloud.com`, and
/// writes them back through the one channel Rust can already read: a cookie on
/// the page's origin.
///
/// **Verified live (2026-08-22, macOS 26.6.2)** — the wrapper does win the race
/// against Apple's own JS, which is verify-first item #2 and the reason this
/// mechanism is allowed to exist at all. Capturing the parameters *is* the
/// proof; there is no API that reports injection order.
///
/// **Why a cookie and not a `fetch` to a custom scheme.** icloud.com ships a
/// strict CSP and `connect-src` will block a request to an unfamiliar scheme.
/// `document.cookie` is not subject to CSP, and a `WKWebView` user script is
/// not subject to the page's CSP either. Load-bearing; do not "simplify" it
/// into a fetch.
///
/// **Why not Tauri IPC.** That hands Apple's page Jodd's IPC surface. This
/// channel is one-directional and carries nothing but our own markers.
///
/// Every statement is inside a `try`. A throw here would run inside Apple's
/// page before Apple's own code, and breaking the sign-in form is a far worse
/// failure than losing the version strings.
pub const INIT_SCRIPT: &str = r#"
(function () {
  var mark = function (name, value) {
    try {
      document.cookie = name + "=" + encodeURIComponent(value) + ";path=/;SameSite=Lax";
    } catch (e) {}
  };

  var captured = false;
  var note = function (raw) {
    if (captured) return;
    try {
      var u = new URL(raw, location.href);
      if (u.hostname.indexOf("setup.icloud.com") === -1) return;
      var out = [];
      ["clientBuildNumber", "clientMasteringNumber", "clientId"].forEach(function (k) {
        var v = u.searchParams.get(k);
        if (v) out.push(k + "=" + v);
      });
      if (out.length !== 3) return;
      captured = true;
      mark("jodd_icloud_cfg", out.join("&"));
    } catch (e) {}
  };

  try {
    var origFetch = window.fetch;
    window.fetch = function (input) {
      try { note(typeof input === "string" ? input : (input && input.url) || ""); } catch (e) {}
      return origFetch.apply(this, arguments);
    };
  } catch (e) {}

  try {
    var origOpen = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (method, url) {
      try { note(url); } catch (e) {}
      return origOpen.apply(this, arguments);
    };
  } catch (e) {}
})();
"#;

// ────────────────────────────────────────────────────────────────────────────
// The harvested jar
// ────────────────────────────────────────────────────────────────────────────

/// One cookie, reduced to what CloudKit requests need — and, crucially, to
/// **plain data owned by this crate**.
///
/// Not `tauri::Cookie`, on purpose. Every rule below is then testable on any
/// machine, including the Linux CI runner that has no `WKWebView` at all. The
/// conversion from Tauri's type is a handful of lines at the bottom of this
/// module and is the only part a non-Mac cannot exercise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestedCookie {
    pub name: String,
    pub value: String,
    /// The cookie's domain **without** a leading dot.
    pub domain: String,
    pub path: String,
    /// True when the cookie was set with no `Domain` attribute, so it must go
    /// to that exact host and nowhere else.
    ///
    /// **From a live webview this is always `false`, and that is a known,
    /// deliberate loss.** WebKit reports `.icloud.com` for a domain cookie and
    /// `www.icloud.com` for a host-only one, but `cookie::Cookie::domain()`
    /// strips the leading dot (cookie 0.18, lib.rs:781) and `domain_raw()`
    /// cannot recover it for a cookie that was built rather than parsed — which
    /// is how wry constructs them. The distinction is genuinely unavailable
    /// through Tauri's API.
    ///
    /// Defaulting to `false` over-sends within `*.icloud.com` rather than
    /// under-sending. That direction is chosen, not accidental: every host
    /// involved is Apple's own, while a missing session cookie is a dead
    /// request. The field exists so the rule itself stays correct and pinned by
    /// tests, and so a future Tauri that exposes the flag needs no new logic.
    pub host_only: bool,
    pub secure: bool,
}

/// RFC 6265 §5.1.3 domain-match.
///
/// `host` equals `domain`, or `host` ends with `.domain`. The dot boundary is
/// the whole point: without it `noticloud.com` matches `icloud.com`.
fn domain_matches(host: &str, domain: &str) -> bool {
    if host.eq_ignore_ascii_case(domain) {
        return true;
    }
    let Some(prefix) = host.len().checked_sub(domain.len()) else {
        return false;
    };
    prefix > 0
        && host.as_bytes()[prefix - 1] == b'.'
        && host[prefix..].eq_ignore_ascii_case(domain)
}

/// RFC 6265 §5.1.4 path-match.
fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if cookie_path.is_empty() || cookie_path == "/" {
        return true;
    }
    if request_path == cookie_path {
        return true;
    }
    request_path.starts_with(cookie_path)
        && (cookie_path.ends_with('/') || request_path[cookie_path.len()..].starts_with('/'))
}

/// Builds the `Cookie:` header for one request, the way a browser would.
///
/// **This function is why the harvest uses `cookies()` and not
/// `cookies_for_url()`.** wry's `cookies_for_url` filters with
/// `cookie.domain() == url.domain()` — exact string equality — so a cookie
/// scoped `Domain=icloud.com` is dropped for `www.icloud.com`, which RFC 6265
/// says it MUST match. Measured 2026-08-22 on a real signed-in jar: **0** of
/// Apple's cookies came back for `https://www.icloud.com` where `cookies()`
/// returned 17.
///
/// That is not a `www` curiosity. CloudKit is served from
/// `p<N>-ckdatabasews.icloud.com`, another subdomain, so `cookies_for_url`
/// would hand every CloudKit burst an **empty jar**. Asking for the bare
/// `https://icloud.com` instead is not the fix either: it works by accident on
/// today's jar and silently drops any host-only cookie Apple sets on a
/// subdomain later.
///
/// **Nothing is filtered by cookie NAME.** The same live jar carries fifteen
/// `X-APPLE-…` cookies and two `X_APPLE_WEB_KB-…` ones — **underscores** — all
/// HttpOnly, Secure and 220 bytes. A rule shaped like `starts_with("X-APPLE")`
/// loses two session cookies without a word. Send what domain-matches; the
/// only exclusion is Jodd's own markers.
///
/// Ordering follows RFC 6265 §5.4: longer paths first. Servers rarely care,
/// but a deterministic order is what makes this testable.
pub fn cookie_header_for(host: &str, path: &str, jar: &[HarvestedCookie]) -> String {
    let mut matched: Vec<&HarvestedCookie> = jar
        .iter()
        .filter(|c| !c.name.starts_with(MARKER_PREFIX))
        .filter(|c| {
            if c.host_only {
                host.eq_ignore_ascii_case(&c.domain)
            } else {
                domain_matches(host, &c.domain)
            }
        })
        .filter(|c| path_matches(path, &c.path))
        .collect();

    matched.sort_by(|a, b| b.path.len().cmp(&a.path.len()));
    matched
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

// ────────────────────────────────────────────────────────────────────────────
// Component B5 — client version strings, read from the live session
// ────────────────────────────────────────────────────────────────────────────

/// The three parameters every CloudKit and setup request must carry.
///
/// **Never hardcoded.** The live session carried `clientBuildNumber =
/// 2628Build44`; icloud-md hardcodes `2624Build27`, and the drift its own
/// comment warns about has already happened. These come off Apple's own first
/// request, via [`INIT_SCRIPT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    pub client_build_number: String,
    pub client_mastering_number: String,
    pub client_id: String,
}

impl ClientConfig {
    /// Parses the marker cookie's value: `encodeURIComponent("a=1&b=2&c=3")`.
    ///
    /// Returns `None` unless all three parameters are present and non-empty. A
    /// partial config is worse than none — it would be sent to Apple as a
    /// malformed request whose failure says nothing about the missing capture.
    pub fn parse(raw: &str) -> Option<Self> {
        // `encodeURIComponent` escapes `&` and `=` themselves, so the whole
        // blob is one encoded string and must be decoded before it is split.
        // Splitting first would find no separators at all.
        let decoded = urlencoding::decode(raw).ok()?;
        let mut build = None;
        let mut mastering = None;
        let mut id = None;
        for pair in decoded.split('&') {
            let Some((k, v)) = pair.split_once('=') else { continue };
            if v.is_empty() {
                continue;
            }
            match k {
                "clientBuildNumber" => build = Some(v.to_string()),
                "clientMasteringNumber" => mastering = Some(v.to_string()),
                "clientId" => id = Some(v.to_string()),
                _ => {}
            }
        }
        Some(ClientConfig {
            client_build_number: build?,
            client_mastering_number: mastering?,
            client_id: id?,
        })
    }

    /// Finds the marker cookie in a harvested jar and parses it.
    pub fn from_jar(jar: &[HarvestedCookie]) -> Option<Self> {
        jar.iter()
            .find(|c| c.name == CLIENT_CFG_COOKIE)
            .and_then(|c| Self::parse(&c.value))
    }

    /// The query string every setup/CloudKit URL carries.
    ///
    /// `requestId` is fresh per call, matching what the web client does.
    pub fn query(&self) -> String {
        format!(
            "clientBuildNumber={}&clientMasteringNumber={}&clientId={}&requestId={}",
            self.client_build_number,
            self.client_mastering_number,
            self.client_id,
            uuid::Uuid::new_v4()
        )
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Component B2 — the completion discriminator
// ────────────────────────────────────────────────────────────────────────────

/// A validated session: what `/validate` bootstraps and the transport consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcloudSession {
    /// The Apple ID as Apple reports it — the only string shown to the user as
    /// an address, and what `account_id_for(ICloud, …)` is minted from.
    pub apple_id: String,
    pub dsid: String,
    /// The `p<N>-ckdatabasews` partition serving this account. **Not
    /// guessable** and not the same for two Apple IDs.
    pub ck_host: String,
    pub client: ClientConfig,
}

/// What one `/validate` call means for the sign-in poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidateOutcome {
    /// Fully signed in. Close the window.
    Complete { apple_id: String, dsid: String, ck_host: String },
    /// Password accepted, 2FA still pending. **Keep the window open.**
    TwoFactorPending,
    /// No usable session yet — not signed in, or the session died. Keep polling.
    NotSignedIn,
    /// Signed in, but Apple reports no CloudKit database service for this
    /// account. Polling will never fix that, so it is not `NotSignedIn`.
    NoCloudKitService,
    /// Something this code does not understand. Surfaced rather than folded
    /// into "keep waiting", which would hang the sign-in window forever.
    Failed(String),
}

/// Decides what a `/validate` response means. **The whole of Component B2.**
///
/// **HTTP 200 is not the test, and getting that wrong shuts the window in the
/// user's face at the 2FA screen.** A password-accepted-but-2FA-pending
/// session answers 200 *and* carries a `ckdatabasews` entry. The discriminator
/// is:
///
/// ```text
/// hsaChallengeRequired !== true   (at the top level AND under dsInfo)
/// AND webservices.ckdatabasews.url is present
/// ```
///
/// Apple puts the flag in both places depending on the stage, so both are
/// checked — `icloud_probe.rs::validate` does the same and this is a port of
/// it, not a re-derivation.
pub fn classify_validate(status: u16, body: &serde_json::Value) -> ValidateOutcome {
    match status {
        // 421 MISDIRECTED REQUEST is Apple's "this session is over". It is the
        // status an unsigned-in poll returns too, which is why it means "keep
        // waiting" here rather than "fail".
        421 | 401 | 403 => return ValidateOutcome::NotSignedIn,
        s if !(200..300).contains(&s) => {
            return ValidateOutcome::Failed(format!("/validate answered HTTP {s}"))
        }
        _ => {}
    }

    let challenged = |v: &serde_json::Value| v == &serde_json::Value::Bool(true);
    if challenged(&body["hsaChallengeRequired"]) || challenged(&body["dsInfo"]["hsaChallengeRequired"]) {
        return ValidateOutcome::TwoFactorPending;
    }

    let dsid = body["dsInfo"]["dsid"].as_str();
    let apple_id = body["dsInfo"]["appleId"].as_str();
    let (Some(dsid), Some(apple_id)) = (dsid, apple_id) else {
        // 200 with no dsInfo at all is what an anonymous session answers, so
        // this is "not signed in yet", not a malformed response.
        return ValidateOutcome::NotSignedIn;
    };

    match body["webservices"]["ckdatabasews"]["url"].as_str() {
        Some(host) if !host.is_empty() => ValidateOutcome::Complete {
            apple_id: apple_id.to_string(),
            dsid: dsid.to_string(),
            ck_host: host.trim_end_matches('/').to_string(),
        },
        _ => ValidateOutcome::NoCloudKitService,
    }
}

/// `POST /setup/ws/1/validate`, against `base` so tests can point it at a mock.
///
/// The cookie header is built by the caller ([`cookie_header_for`]) rather than
/// here, so the matching rules stay testable without an HTTP server and this
/// function stays what it says it is: one request and one classification.
pub async fn validate_at(
    http: &reqwest::Client,
    base: &str,
    cookie_header: &str,
    client: &ClientConfig,
) -> ValidateOutcome {
    let url = format!("{base}{VALIDATE_PATH}?{}", client.query());
    let resp = http
        .post(&url)
        .header("Cookie", cookie_header)
        .header("Origin", "https://www.icloud.com")
        .header("Referer", ICLOUD_URL)
        .header("Accept", "application/json")
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        // A transport error is not a verdict about the session — the user may
        // simply be offline mid-sign-in. Keep polling.
        Err(e) => return ValidateOutcome::Failed(format!("/validate could not be reached: {e}")),
    };
    let status = resp.status().as_u16();
    // A non-2xx body is frequently HTML or empty; classify on the status alone
    // rather than letting a JSON parse failure mask a clear 421.
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    classify_validate(status, &body)
}

// ────────────────────────────────────────────────────────────────────────────
// B3 — where a CloudKit burst gets its cookies
// ────────────────────────────────────────────────────────────────────────────

/// Supplies the live jar, once per burst.
///
/// **The seam exists because Jodd must never hold a copy of the session**
/// (gotcha #19): a captured jar measured dead inside 3.5 hours, not because it
/// expired but because the live browser rotated its cookies out from under the
/// copy. So the vertical asks for cookies immediately before it needs them and
/// keeps nothing.
///
/// It is a trait rather than a direct webview call for the same reason
/// [`HarvestedCookie`] is this crate's own type: the transport is then
/// exercisable against a mock HTTP server on any machine, with a jar supplied
/// by [`StaticJar`]. A vertical that reached into `WKWebView` itself could only
/// be tested on a Mac with a real Apple ID.
#[async_trait::async_trait]
pub trait CookieSource: Send + Sync {
    /// **Never cached by the implementation.** Returning a stored jar would
    /// reintroduce exactly the staleness this trait exists to avoid.
    async fn harvest(&self) -> Result<Vec<HarvestedCookie>, String>;
}

/// A fixed jar, for tests and probes.
pub struct StaticJar(pub Vec<HarvestedCookie>);

#[async_trait::async_trait]
impl CookieSource for StaticJar {
    async fn harvest(&self) -> Result<Vec<HarvestedCookie>, String> {
        Ok(self.0.clone())
    }
}

/// Harvests from a live webview, by window label.
///
/// Which window that is belongs to whoever owns the webview's lifecycle: the
/// visible sign-in window while [`sign_in`] is running, and the hidden revival
/// webview afterwards (Component B4, not built yet). This type only reads.
///
/// A missing window is an error, not an empty jar. An empty jar would send an
/// unauthenticated CloudKit request and read the 421 as a dead session, which
/// is the same symptom with the cause erased.
#[cfg(desktop)]
pub struct WebviewJar {
    pub app: tauri::AppHandle,
    pub label: String,
}

#[cfg(desktop)]
#[async_trait::async_trait]
impl CookieSource for WebviewJar {
    async fn harvest(&self) -> Result<Vec<HarvestedCookie>, String> {
        use tauri::Manager;
        let win = self
            .app
            .get_webview_window(&self.label)
            .ok_or_else(|| format!("no iCloud webview '{}' to harvest a session from", self.label))?;
        // `cookies()`, never `cookies_for_url` — see `cookie_header_for`.
        let cs = win.cookies().map_err(|e| format!("could not read the iCloud session: {e}"))?;
        Ok(cs.iter().map(harvest_one).collect())
    }
}

/// Bootstraps a session from whatever jar `cookies` yields.
///
/// **`/validate` is the first call of any session, always.** It is not a
/// liveness ping bolted on: it is the only source of the account's `dsid` and
/// of which `p<N>-ckdatabasews` partition serves it, and that host **cannot be
/// guessed** — it differs per Apple ID. So every path that wants a transport
/// goes through here, and nothing about the session is ever persisted; the
/// marker on the account record says only that one existed once (gotcha #19).
///
/// Errors are [`crate::backend::TransportError`] because every caller is a
/// transport path. A session that is gone, pending 2FA, or unreachable is
/// `Auth`: all three mean "the webview must do something before this can work",
/// which is exactly what the revival path answers.
pub async fn establish(
    cookies: &dyn CookieSource,
) -> Result<IcloudSession, crate::backend::TransportError> {
    use crate::backend::TransportError;

    let jar = cookies.harvest().await.map_err(|e| {
        crate::log!("icloud: cookie harvest failed: {e}");
        TransportError::Auth
    })?;
    let Some(client) = ClientConfig::from_jar(&jar) else {
        // The marker cookie is written by our own injected script off Apple's
        // first request. Missing means the webview has not loaded icloud.com
        // yet — not that the user is signed out.
        crate::log!("icloud: no client config captured yet — the webview has not reached Apple");
        return Err(TransportError::Auth);
    };
    let header = cookie_header_for(SETUP_HOSTNAME, VALIDATE_PATH, &jar);
    if header.is_empty() {
        crate::log!("icloud: harvested jar carries no session for {SETUP_HOSTNAME}");
        return Err(TransportError::Auth);
    }

    match validate_at(&reqwest::Client::new(), SETUP_HOST, &header, &client).await {
        ValidateOutcome::Complete { apple_id, dsid, ck_host } => {
            Ok(IcloudSession { apple_id, dsid, ck_host, client })
        }
        ValidateOutcome::NoCloudKitService => Err(TransportError::Permanent {
            source: anyhow::anyhow!(
                "Apple reports no iCloud Notes service for this Apple ID. Turn Notes on for \
                 iCloud on an Apple device."
            ),
        }),
        // Both are "the webview has work to do", which is what Auth means to
        // the retry policy and what the revival path exists to answer.
        ValidateOutcome::TwoFactorPending | ValidateOutcome::NotSignedIn => Err(TransportError::Auth),
        ValidateOutcome::Failed(m) => {
            // A dropped connection is not a verdict about the session.
            Err(TransportError::Transient { source: anyhow::anyhow!(m) })
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Component B4 — revival: the hidden webview
// ────────────────────────────────────────────────────────────────────────────

/// The visible window `sign_in` opens.
pub const SIGNIN_WINDOW: &str = "jodd-icloud-signin";

/// The hidden window every later read harvests from.
pub const SESSION_WINDOW: &str = "jodd-icloud-session";

/// Makes sure *some* webview holds a live iCloud session, and returns which.
///
/// **This is the whole of B4, and it is not an optimization.** Jodd stores no
/// cookie: the jar lives in `WKWebView`'s own persistent store and the live
/// browser rotates it (gotcha #19). So after the sign-in window closes there is
/// nothing left to harvest from unless a webview stays alive — and a session
/// dies in hours, so "sign in again" would be a daily interruption rather than
/// a rare one.
///
/// The hidden window shares the sign-in window's `data_store_identifier`, which
/// is what makes the persisted store the *same* store: Apple's own JS then
/// re-authenticates silently against it on load, with no user interaction. That
/// only works from macOS 14 up — below it wry falls back to the shared default
/// store **with no error** (which is also why this install holds one Apple ID;
/// see [`refuse_second_icloud_account`]).
///
/// Created **lazily and reused**, never per call: an always-resident
/// `WKWebView` costs real memory for a session that is usually valid, and a
/// fresh one per read would reload icloud.com every few seconds.
///
/// Prefers the visible sign-in window while it exists, so a read during sign-in
/// does not race a second webview against the one the user is looking at.
#[cfg(desktop)]
pub fn ensure_session_webview(app: &tauri::AppHandle) -> Result<String, String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

    if app.get_webview_window(SIGNIN_WINDOW).is_some() {
        return Ok(SIGNIN_WINDOW.to_string());
    }
    if app.get_webview_window(SESSION_WINDOW).is_some() {
        return Ok(SESSION_WINDOW.to_string());
    }

    let url = tauri::Url::parse(ICLOUD_URL).map_err(|e| e.to_string())?;
    WebviewWindowBuilder::new(app, SESSION_WINDOW, WebviewUrl::External(url))
        .title("Jodd — iCloud session")
        .visible(false)
        // The SAME store the sign-in window wrote, or Apple's JS has nothing
        // to re-authenticate against and this window is a stranger.
        .data_store_identifier(DATA_STORE_ID)
        // The injected script must run here too: `establish` reads the client
        // version strings out of the jar, and a window that never captured
        // them yields no config however good its session is.
        .initialization_script(INIT_SCRIPT)
        .build()
        .map_err(|e| format!("could not open the iCloud session webview: {e}"))?;

    crate::log!("icloud: opened the hidden session webview");
    Ok(SESSION_WINDOW.to_string())
}

/// Closes the visible sign-in window, leaving the session in the data store.
///
/// The counterpart of [`sign_in`] returning with it open. **Not
/// [`forget_session`]**, which also deletes the store — that is for removing
/// the account, and calling it here would throw away the session that was just
/// established.
#[cfg(desktop)]
pub fn close_signin_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window(SIGNIN_WINDOW) {
        let _ = w.close();
    }
}

/// A [`CookieSource`] over whichever webview currently holds the session,
/// creating the hidden one if needed.
///
/// Resolves the window **per harvest**, not once at construction: the visible
/// sign-in window can close between two reads, and a jar bound to a dead window
/// would report the session gone when it is merely somewhere else.
#[cfg(desktop)]
pub struct LiveSession {
    pub app: tauri::AppHandle,
}

/// How long a freshly-created hidden webview is given to load icloud.com and
/// let Apple's JS re-authenticate before a harvest gives up.
///
/// It is a **cold-start allowance, not a retry policy**: an already-warm window
/// answers on the first tick, so this only costs anything the first time a read
/// follows a restart.
///
/// Twenty seconds, not five. Five was chosen as "generous for a page load" and
/// was not: a hidden `WKWebView` has to be created, load icloud.com, run
/// Apple's own JS, and let it issue its first `setup.icloud.com` request before
/// the marker cookie exists. The first real sign-in blew straight through five
/// and reported the session dead. A genuinely dead session still surfaces as
/// `Auth`, just twenty seconds later, and only on a path that had to build a
/// webview anyway.
#[cfg(desktop)]
const WARMUP_TICKS: u32 = 40;
#[cfg(desktop)]
const WARMUP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(desktop)]
impl LiveSession {
    /// Opens the hidden window, bypassing the sign-in window entirely.
    ///
    /// Used when the preferred window turns out to be unusable — see
    /// [`CookieSource::harvest`] below for why "present" and "usable" are not
    /// the same thing here.
    fn hidden_only(&self) -> Result<WebviewJar, String> {
        use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
        if self.app.get_webview_window(SESSION_WINDOW).is_none() {
            let url = tauri::Url::parse(ICLOUD_URL).map_err(|e| e.to_string())?;
            WebviewWindowBuilder::new(&self.app, SESSION_WINDOW, WebviewUrl::External(url))
                .title("Jodd — iCloud session")
                .visible(false)
                .data_store_identifier(DATA_STORE_ID)
                .initialization_script(INIT_SCRIPT)
                .build()
                .map_err(|e| format!("could not open the iCloud session webview: {e}"))?;
            crate::log!("icloud: opened the hidden session webview");
        }
        Ok(WebviewJar { app: self.app.clone(), label: SESSION_WINDOW.to_string() })
    }
}

#[cfg(desktop)]
#[async_trait::async_trait]
impl CookieSource for LiveSession {
    async fn harvest(&self) -> Result<Vec<HarvestedCookie>, String> {
        let label = ensure_session_webview(&self.app)?;
        let mut jar = WebviewJar { app: self.app.clone(), label: label.clone() };

        // **`get_webview_window` returning `Some` does not mean the webview can
        // be talked to.** A window that has been asked to close is still
        // registered for a moment, and `cookies()` against it fails with
        // "failed to receive message from webview" — measured on the first real
        // sign-in, 98 ms after the session was established, because the
        // sign-in window was closing while this ran. Falling back to the hidden
        // window turns that race into a slower success instead of a sign-in
        // that reports the account unreadable.
        if label == SIGNIN_WINDOW {
            if let Err(e) = jar.harvest().await {
                crate::log!("icloud: '{SIGNIN_WINDOW}' is not answering ({e}) — using the hidden window");
                jar = self.hidden_only()?;
            }
        }

        // A window created microseconds ago has an empty jar, and returning it
        // would report the session dead when the page has simply not loaded.
        // The client-config marker is the signal: our injected script writes it
        // off Apple's OWN first request, so its presence means the page loaded
        // and talked to Apple — a far better readiness test than counting
        // cookies, which a partial load also produces.
        for tick in 0..WARMUP_TICKS {
            let harvested = jar.harvest().await?;
            if ClientConfig::from_jar(&harvested).is_some() {
                return Ok(harvested);
            }
            if tick == 0 {
                crate::log!("icloud: waiting for the session webview '{label}' to reach Apple");
            }
            tokio::time::sleep(WARMUP_INTERVAL).await;
        }
        // Return what is there rather than an error: the caller
        // (`establish`, the vertical's `cookie_header`) decides what an
        // insufficient jar means, and both already say `Auth`. Failing here
        // would put a second, differently-worded verdict on the same fact.
        jar.harvest().await
    }
}

/// Closes the iCloud webviews and deletes the data store behind them.
///
/// **Removing the account is not enough on its own, and the gap is a
/// wrong-account bug rather than a leak.** The session does not live in
/// `accounts.json` or the keychain — it lives in `WKWebView`'s persistent data
/// store, which survives everything `remove_account` does. Leave it and the
/// next sign-in finds a valid session already there: `/validate` answers with
/// the OLD Apple ID, and Jodd silently creates an account for the person who
/// just signed out. No prompt, no error, nothing on screen that looks wrong.
///
/// So this is the counterpart of [`refuse_second_icloud_account`]: one jar per
/// install means removing the account must actually empty the jar.
///
/// The windows are closed first because WebKit will not delete a store that is
/// still in use, and the deletion is best-effort — a failure here must not
/// block the removal the user asked for, so it is logged and swallowed. The
/// worst case is a stale jar, and the sign-in flow's own `/validate` reports
/// whichever Apple ID it belongs to.
#[cfg(desktop)]
pub async fn forget_session(app: &tauri::AppHandle) {
    use tauri::Manager;

    for label in [SIGNIN_WINDOW, SESSION_WINDOW] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.close();
            crate::log!("icloud: closed webview '{label}' before forgetting the session");
        }
    }

    #[cfg(target_vendor = "apple")]
    {
        // A window closed microseconds ago may still hold the store open, and
        // WebKit's refusal in that case is indistinguishable from any other
        // failure. One short wait costs nothing on the path that already
        // succeeds and turns the common race into a success.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        match app.remove_data_store(DATA_STORE_ID).await {
            Ok(()) => crate::log!("icloud: data store removed — the next sign-in starts clean"),
            Err(e) => crate::log!(
                "icloud: could not remove the data store ({e}). A later sign-in may find the \
                 previous Apple ID still signed in; /validate reports whose session it is."
            ),
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        // `remove_data_store` is Apple-only in Tauri 2.11. Nothing else can
        // hold an iCloud session today (the button is macOS-only), so there is
        // nothing to forget here — but say so rather than leaving a silent
        // no-op for whoever brings up Windows in M3.
        crate::log!("icloud: no data store to remove on this platform");
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Component A3 — one Apple ID per install
// ────────────────────────────────────────────────────────────────────────────

/// Refuses a second iCloud account **before anything is persisted**.
///
/// macOS `WKWebView`'s default data store is app-wide: one cookie jar however
/// many accounts exist, so a second Apple ID would silently share the first
/// one's session.
///
/// **The reason is "M1 does not do this yet", not "macOS cannot", and the
/// message says so.** `data_store_identifier` exists in Tauri 2.11 and wry maps
/// it to `WKWebsiteDataStore(forIdentifier:)` on macOS 14+ — verified live. The
/// limit is scope. Wording that claimed a platform limitation would send
/// whoever lifts it hunting for a workaround that is not needed.
///
/// (The hazard that comes with lifting it: on macOS 13 and older wry falls back
/// to the shared store **with no error**, so multi-account support must
/// feature-detect the OS version rather than assume.)
///
/// Deliberately one function rather than an assumption spread through the
/// vertical, so removing the limit is a one-line change.
pub fn refuse_second_icloud_account(existing: &[crate::accounts::Account]) -> Result<(), String> {
    let already = existing
        .iter()
        .find(|a| a.backend_kind == crate::accounts::BackendKind::ICloud);
    match already {
        None => Ok(()),
        Some(a) => Err(format!(
            "Jodd can hold one iCloud account at a time, and {} is already signed in. \
             Remove it first to add a different Apple ID. \
             (This is a limit of the current milestone, not of macOS — the sign-in \
             window shares one cookie store today.)",
            a.email
        )),
    }
}

// ────────────────────────────────────────────────────────────────────────────
// The webview driver — the only part a non-Mac cannot exercise
// ────────────────────────────────────────────────────────────────────────────

/// Fixed, so the store a sign-in creates is the store the next launch finds.
/// A random id per run would start every session from an empty jar.
#[cfg(desktop)]
pub const DATA_STORE_ID: [u8; 16] = *b"jodd-icloud-0001";

/// How often the poll asks `/validate`, and for how long in total.
///
/// Ten minutes is not generosity: a first sign-in can involve a password, a
/// 2FA code typed off another device, and occasionally a CAPTCHA. A tighter cap
/// would time out a user who is doing everything right.
#[cfg(desktop)]
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
#[cfg(desktop)]
const POLL_TICKS: u32 = 200;

/// Converts one webview cookie into the plain form the rules above work on.
///
/// See [`HarvestedCookie::host_only`] for why that flag is `false` here rather
/// than read from the cookie: the leading dot that carries the distinction is
/// stripped before Tauri hands it over, and cannot be recovered.
#[cfg(desktop)]
fn harvest_one(c: &tauri::webview::Cookie<'static>) -> HarvestedCookie {
    HarvestedCookie {
        name: c.name().to_string(),
        value: c.value().to_string(),
        domain: c.domain().unwrap_or_default().trim_start_matches('.').to_string(),
        path: c.path().unwrap_or("/").to_string(),
        host_only: false,
        secure: c.secure().unwrap_or(false),
    }
}

/// Opens a **visible** sign-in window and polls until Apple says the session is
/// complete.
///
/// Returns the validated session **with the window still open** — the caller
/// closes it with [`close_signin_window`] once it has finished reading. That
/// is not tidiness: the caller's next step is the ADP check, and this window
/// holds the only warm session there is.
///
/// It does **not** create an account: Component B1 puts the ADP readability
/// check before persistence, and that check needs the transport. See this
/// module's header.
///
/// **The cookie read must not happen on the main thread.** `cookies()` bottoms
/// out in `WKHTTPCookieStore.getAllCookies`, which wry dispatches *to* the main
/// thread and then blocks waiting on a channel — calling it from there waits
/// for itself. Tauri runs `async` commands off the main thread, and
/// `examples/icloud_webview_probe` proves the shape from a spawned thread.
#[cfg(desktop)]
pub async fn sign_in(app: &tauri::AppHandle) -> Result<IcloudSession, String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

    // A window left over from an abandoned attempt would be polled instead of
    // the new one, and would already be sitting on a finished page.
    if let Some(old) = app.get_webview_window(SIGNIN_WINDOW) {
        let _ = old.close();
    }

    let url = tauri::Url::parse(ICLOUD_URL).map_err(|e| e.to_string())?;
    let win = WebviewWindowBuilder::new(app, SIGNIN_WINDOW, WebviewUrl::External(url))
        .title("Sign in to iCloud")
        .inner_size(1100.0, 820.0)
        .initialization_script(INIT_SCRIPT)
        .data_store_identifier(DATA_STORE_ID)
        .build()
        .map_err(|e| format!("could not open the iCloud sign-in window: {e}"))?;

    // Closing the window is how a user cancels. Without this the poll would run
    // its full ten minutes against a window that is gone.
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = cancelled.clone();
        win.on_window_event(move |e| {
            if matches!(e, tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed) {
                cancelled.store(true, Ordering::SeqCst);
            }
        });
    }

    let http = reqwest::Client::new();
    let mut last_seen = ValidateOutcome::NotSignedIn;

    for _ in 0..POLL_TICKS {
        tokio::time::sleep(POLL_INTERVAL).await;
        if cancelled.load(Ordering::SeqCst) {
            return Err("iCloud sign-in was cancelled.".to_string());
        }

        let jar: Vec<HarvestedCookie> = match win.cookies() {
            Ok(cs) => cs.iter().map(harvest_one).collect(),
            // The window is tearing down. Treat it as the cancel it almost
            // always is rather than retrying a webview that no longer exists.
            Err(e) => return Err(format!("iCloud sign-in window closed ({e}).")),
        };

        // No config yet means Apple's own first request has not gone out, so
        // there is nothing to validate against. This is the normal state for
        // the first tick or two, not an error.
        let Some(client) = ClientConfig::from_jar(&jar) else { continue };

        let header = cookie_header_for(SETUP_HOSTNAME, VALIDATE_PATH, &jar);
        if header.is_empty() {
            continue;
        }

        last_seen = validate_at(&http, SETUP_HOST, &header, &client).await;
        match last_seen {
            ValidateOutcome::Complete { ref apple_id, ref dsid, ref ck_host } => {
                let session = IcloudSession {
                    apple_id: apple_id.clone(),
                    dsid: dsid.clone(),
                    ck_host: ck_host.clone(),
                    client,
                };
                // **The window stays open.** The caller's next step is the ADP
                // readability check, which needs a live jar — and this window
                // already has one, loaded and warm. Closing here forced that
                // check to cold-start the hidden webview instead and harvest
                // from a page that had not reached Apple yet, which surfaced
                // as "could not read this iCloud account: auth" on the very
                // first real sign-in. `close_signin_window` is the caller's.
                return Ok(session);
            }
            // Both mean "the user is still working"; keep the window open.
            ValidateOutcome::TwoFactorPending | ValidateOutcome::NotSignedIn => continue,
            ValidateOutcome::NoCloudKitService => {
                let _ = win.close();
                return Err(
                    "This Apple ID is signed in, but Apple reports no iCloud Notes service for it. \
                     Turn Notes on for iCloud on an Apple device, then try again."
                        .to_string(),
                );
            }
            ValidateOutcome::Failed(ref m) => {
                // Not fatal on its own — a dropped connection mid-sign-in lands
                // here. Keep polling; the timeout below reports the last cause.
                crate::log!("icloud sign-in: {m}");
                continue;
            }
        }
    }

    let _ = win.close();
    Err(match last_seen {
        ValidateOutcome::Failed(m) => format!("iCloud sign-in timed out: {m}"),
        ValidateOutcome::TwoFactorPending => {
            "iCloud sign-in timed out while two-factor authentication was still pending.".to_string()
        }
        _ => "iCloud sign-in timed out.".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cookie(name: &str, domain: &str) -> HarvestedCookie {
        HarvestedCookie {
            name: name.to_string(),
            value: format!("{name}-value"),
            domain: domain.to_string(),
            path: "/".to_string(),
            host_only: false,
            secure: true,
        }
    }

    // ── RFC 6265 domain matching ────────────────────────────────────────
    //
    // Every case below is measured on a real signed-in jar (2026-08-22,
    // macOS 26.6.2) or is the boundary a naive implementation gets wrong.

    #[test]
    fn a_domain_cookie_reaches_the_www_host_that_cookies_for_url_dropped() {
        // The exact failure that disqualified `cookies_for_url`: it returned 0
        // of Apple's cookies for https://www.icloud.com because it compares
        // domains as equal strings. RFC 6265 says this MUST match.
        let jar = vec![cookie("X-APPLE-WEBAUTH-TOKEN", "icloud.com")];
        assert_eq!(
            cookie_header_for("www.icloud.com", "/", &jar),
            "X-APPLE-WEBAUTH-TOKEN=X-APPLE-WEBAUTH-TOKEN-value"
        );
    }

    #[test]
    fn a_domain_cookie_reaches_the_cloudkit_partition_host() {
        // The case that actually matters. CloudKit is served from
        // p<N>-ckdatabasews.icloud.com, so a matcher that only handled `www`
        // would still hand every burst an empty jar.
        let jar = vec![cookie("X-APPLE-DS-WEB-SESSION-TOKEN", "icloud.com")];
        let header = cookie_header_for("p149-ckdatabasews.icloud.com", "/database/1", &jar);
        assert!(header.contains("X-APPLE-DS-WEB-SESSION-TOKEN"), "got {header:?}");
    }

    #[test]
    fn the_dot_boundary_is_load_bearing() {
        // Without the leading-dot check, `noticloud.com` ends with
        // `icloud.com` and would be handed the session.
        let jar = vec![cookie("X-APPLE-WEBAUTH-TOKEN", "icloud.com")];
        assert_eq!(cookie_header_for("noticloud.com", "/", &jar), "");
        assert_eq!(cookie_header_for("evil-icloud.com", "/", &jar), "");
    }

    #[test]
    fn a_host_only_cookie_does_not_leak_to_a_sibling_subdomain() {
        let jar = vec![HarvestedCookie { host_only: true, ..cookie("session", "www.icloud.com") }];
        assert!(cookie_header_for("www.icloud.com", "/", &jar).contains("session"));
        assert_eq!(cookie_header_for("p149-ckdatabasews.icloud.com", "/", &jar), "");
    }

    #[test]
    fn underscored_apple_cookies_are_not_filtered_out() {
        // Measured: two of Apple's session cookies are named X_APPLE_WEB_KB-…
        // with UNDERSCORES, alongside fifteen X-APPLE-… ones. A name-shaped
        // filter loses them silently, so there is no name filter at all.
        let jar = vec![
            cookie("X-APPLE-WEBAUTH-TOKEN", "icloud.com"),
            cookie("X_APPLE_WEB_KB-N4R3FR6", "icloud.com"),
            cookie("x-apple-group", "icloud.com"),
        ];
        let header = cookie_header_for("setup.icloud.com", VALIDATE_PATH, &jar);
        assert!(header.contains("X_APPLE_WEB_KB-N4R3FR6"), "got {header:?}");
        assert!(header.contains("x-apple-group"), "got {header:?}");
    }

    #[test]
    fn jodds_own_markers_are_never_sent_to_apple() {
        let jar = vec![
            cookie("X-APPLE-WEBAUTH-TOKEN", "icloud.com"),
            cookie(CLIENT_CFG_COOKIE, "www.icloud.com"),
        ];
        let header = cookie_header_for("setup.icloud.com", VALIDATE_PATH, &jar);
        assert!(!header.contains(CLIENT_CFG_COOKIE), "got {header:?}");
        assert!(header.contains("X-APPLE-WEBAUTH-TOKEN"));
    }

    #[test]
    fn a_path_scoped_cookie_stays_in_its_path() {
        let scoped = HarvestedCookie { path: "/setup".to_string(), ..cookie("s", "icloud.com") };
        let jar = vec![scoped];
        assert!(cookie_header_for("setup.icloud.com", "/setup/ws/1/validate", &jar).contains("s="));
        assert_eq!(cookie_header_for("setup.icloud.com", "/database/1", &jar), "");
    }

    #[test]
    fn a_path_prefix_must_end_on_a_segment_boundary() {
        let scoped = HarvestedCookie { path: "/set".to_string(), ..cookie("s", "icloud.com") };
        // "/setup" starts with "/set" as a string but is a different path.
        assert_eq!(cookie_header_for("setup.icloud.com", "/setup", &[scoped]), "");
    }

    #[test]
    fn longer_paths_come_first() {
        // RFC 6265 §5.4. Servers rarely care; a deterministic order is what
        // makes the header assertable at all.
        let jar = vec![
            HarvestedCookie { path: "/".to_string(), ..cookie("root", "icloud.com") },
            HarvestedCookie { path: "/setup/ws".to_string(), ..cookie("deep", "icloud.com") },
        ];
        let header = cookie_header_for("setup.icloud.com", VALIDATE_PATH, &jar);
        assert!(header.starts_with("deep="), "got {header:?}");
    }

    #[test]
    fn an_empty_jar_produces_an_empty_header() {
        assert_eq!(cookie_header_for("setup.icloud.com", "/", &[]), "");
    }

    // ── Component B5 — the client config cookie ─────────────────────────

    #[test]
    fn the_config_cookie_is_decoded_before_it_is_split() {
        // `encodeURIComponent` escapes `&` and `=` themselves, so the raw value
        // has no separators to split on. Splitting first finds one field named
        // the whole blob — which parses to None and would read as "the capture
        // failed" on a capture that worked perfectly.
        let raw = "clientBuildNumber%3D2628Build44%26clientMasteringNumber%3D2628B36%26clientId%3DABC-123";
        let cfg = ClientConfig::parse(raw).expect("a well-formed capture must parse");
        assert_eq!(cfg.client_build_number, "2628Build44");
        assert_eq!(cfg.client_mastering_number, "2628B36");
        assert_eq!(cfg.client_id, "ABC-123");
    }

    #[test]
    fn an_unencoded_value_parses_too() {
        // urlencoding::decode leaves a string with nothing to decode alone, so
        // the same parser covers both shapes rather than needing a mode flag.
        let cfg = ClientConfig::parse("clientBuildNumber=a&clientMasteringNumber=b&clientId=c")
            .expect("parse");
        assert_eq!(cfg.client_id, "c");
    }

    #[test]
    fn a_partial_config_is_none_rather_than_a_malformed_request() {
        assert!(ClientConfig::parse("clientBuildNumber%3D2628Build44").is_none());
        assert!(
            ClientConfig::parse("clientBuildNumber%3Da%26clientMasteringNumber%3Db").is_none(),
            "two of three is still not a usable config"
        );
    }

    #[test]
    fn an_empty_parameter_value_does_not_count_as_present() {
        // Apple's own request would never carry these empty, but a wrapper that
        // fired mid-navigation could. An empty clientId sent to Apple fails in
        // a way that says nothing about the real cause.
        assert!(
            ClientConfig::parse("clientBuildNumber%3Da%26clientMasteringNumber%3Db%26clientId%3D")
                .is_none()
        );
    }

    #[test]
    fn the_config_is_found_in_a_jar_by_name() {
        let jar = vec![
            cookie("X-APPLE-WEBAUTH-TOKEN", "icloud.com"),
            HarvestedCookie {
                value: "clientBuildNumber%3Dx%26clientMasteringNumber%3Dy%26clientId%3Dz".into(),
                ..cookie(CLIENT_CFG_COOKIE, "www.icloud.com")
            },
        ];
        assert_eq!(ClientConfig::from_jar(&jar).unwrap().client_build_number, "x");
        assert!(ClientConfig::from_jar(&jar[..1]).is_none());
    }

    #[test]
    fn the_query_carries_all_three_parameters_and_a_fresh_request_id() {
        let cfg = ClientConfig {
            client_build_number: "2628Build44".into(),
            client_mastering_number: "2628B36".into(),
            client_id: "ABC".into(),
        };
        let a = cfg.query();
        assert!(a.contains("clientBuildNumber=2628Build44"));
        assert!(a.contains("clientMasteringNumber=2628B36"));
        assert!(a.contains("clientId=ABC"));
        assert!(a.contains("requestId="));
        assert_ne!(a, cfg.query(), "requestId must be fresh per call");
    }

    // ── Component B2 — the completion discriminator ─────────────────────

    fn complete_body() -> serde_json::Value {
        json!({
            "dsInfo": { "dsid": "12345", "appleId": "someone@me.com" },
            "webservices": { "ckdatabasews": { "url": "https://p149-ckdatabasews.icloud.com:443" } }
        })
    }

    #[test]
    fn a_fully_signed_in_session_is_complete() {
        match classify_validate(200, &complete_body()) {
            ValidateOutcome::Complete { apple_id, dsid, ck_host } => {
                assert_eq!(apple_id, "someone@me.com");
                assert_eq!(dsid, "12345");
                assert_eq!(ck_host, "https://p149-ckdatabasews.icloud.com:443");
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn a_trailing_slash_is_trimmed_off_the_cloudkit_host() {
        // Every caller formats `{ck_host}/database/1/...`, so a trailing slash
        // becomes a double slash in the path — which CloudKit rejects.
        let mut body = complete_body();
        body["webservices"]["ckdatabasews"]["url"] = json!("https://p149-ckdatabasews.icloud.com:443/");
        match classify_validate(200, &body) {
            ValidateOutcome::Complete { ck_host, .. } => {
                assert_eq!(ck_host, "https://p149-ckdatabasews.icloud.com:443")
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn a_two_factor_pending_session_answers_200_with_a_cloudkit_host_and_is_still_not_complete() {
        // THE trap this discriminator exists for. Reading the status code —
        // or even "is there a ckdatabasews entry?" — closes the sign-in window
        // in the user's face while the 2FA prompt is on screen.
        let mut body = complete_body();
        body["hsaChallengeRequired"] = json!(true);
        assert_eq!(classify_validate(200, &body), ValidateOutcome::TwoFactorPending);
    }

    #[test]
    fn the_two_factor_flag_is_honored_under_ds_info_too() {
        // Apple puts it in either place depending on the stage, so both are
        // checked. Checking only the top level passes the 2FA screen through.
        let mut body = complete_body();
        body["dsInfo"]["hsaChallengeRequired"] = json!(true);
        assert_eq!(classify_validate(200, &body), ValidateOutcome::TwoFactorPending);
    }

    #[test]
    fn an_expired_or_absent_session_is_not_signed_in_rather_than_a_failure() {
        // 421 is what an un-signed-in poll answers, so it must mean "keep
        // waiting". Reporting it as an error would abort every sign-in on its
        // very first tick.
        for status in [421u16, 401, 403] {
            assert_eq!(
                classify_validate(status, &serde_json::Value::Null),
                ValidateOutcome::NotSignedIn,
                "HTTP {status}"
            );
        }
    }

    #[test]
    fn a_200_with_no_ds_info_is_not_signed_in() {
        assert_eq!(classify_validate(200, &json!({})), ValidateOutcome::NotSignedIn);
    }

    #[test]
    fn a_signed_in_account_with_no_cloudkit_service_is_its_own_verdict() {
        // Polling will never turn this into a session, so folding it into
        // NotSignedIn would hang the window for the full ten minutes and then
        // report a timeout instead of the real cause.
        let mut body = complete_body();
        body["webservices"]["ckdatabasews"] = json!({});
        assert_eq!(classify_validate(200, &body), ValidateOutcome::NoCloudKitService);
    }

    #[test]
    fn an_unexpected_status_is_surfaced_not_swallowed() {
        match classify_validate(500, &serde_json::Value::Null) {
            ValidateOutcome::Failed(m) => assert!(m.contains("500"), "got {m:?}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // ── Component A3 — one Apple ID per install ─────────────────────────

    fn account(kind: crate::accounts::BackendKind, email: &str) -> crate::accounts::Account {
        crate::accounts::Account {
            id: crate::accounts::account_id_for(kind, email),
            email: email.to_string(),
            added_at: "2026-08-22T00:00:00Z".to_string(),
            notes_label: None,
            meta_label: None,
            llm: Default::default(),
            backend_kind: kind,
            root_dir: None,
            icloud_session_established: false,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status: crate::accounts::AccountStatus::Active,
        }
    }

    #[test]
    fn the_first_icloud_account_is_allowed() {
        use crate::accounts::BackendKind::*;
        let existing = vec![account(Gmail, "a@b.com"), account(Microsoft, "a@b.com")];
        assert!(refuse_second_icloud_account(&existing).is_ok());
        assert!(refuse_second_icloud_account(&[]).is_ok());
    }

    #[test]
    fn a_second_icloud_account_is_refused_and_names_the_first() {
        use crate::accounts::BackendKind::*;
        let existing = vec![account(Gmail, "a@b.com"), account(ICloud, "kaiwan@me.com")];
        let err = refuse_second_icloud_account(&existing).unwrap_err();
        assert!(err.contains("kaiwan@me.com"), "the user must know which one to remove: {err}");
    }

    #[test]
    fn the_refusal_does_not_claim_a_platform_limitation() {
        // `data_store_identifier` exists and works on macOS 14+ — verified
        // live. Wording that blamed macOS would send whoever lifts this limit
        // hunting for a workaround that is not needed.
        use crate::accounts::BackendKind::*;
        let err = refuse_second_icloud_account(&[account(ICloud, "x@me.com")]).unwrap_err();
        let lowered = err.to_lowercase();
        assert!(
            !lowered.contains("cannot") && !lowered.contains("not supported"),
            "the reason is scope, not capability: {err}"
        );
        assert!(lowered.contains("milestone"), "say why it is a limit today: {err}");
    }

    // ── validate_at over a real HTTP round trip ─────────────────────────

    #[tokio::test]
    async fn validate_at_sends_the_session_and_reads_the_verdict() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", mockito::Matcher::Regex(format!("^{VALIDATE_PATH}.*")))
            .match_header("Cookie", mockito::Matcher::Regex("X-APPLE-WEBAUTH-TOKEN".into()))
            .match_header("Origin", "https://www.icloud.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(complete_body().to_string())
            .create_async()
            .await;

        let jar = vec![cookie("X-APPLE-WEBAUTH-TOKEN", "icloud.com")];
        let cfg = ClientConfig {
            client_build_number: "2628Build44".into(),
            client_mastering_number: "2628B36".into(),
            client_id: "ABC".into(),
        };
        let header = cookie_header_for(SETUP_HOSTNAME, VALIDATE_PATH, &jar);
        let out = validate_at(&reqwest::Client::new(), &server.url(), &header, &cfg).await;

        m.assert_async().await;
        assert!(matches!(out, ValidateOutcome::Complete { .. }), "got {out:?}");
    }

    #[tokio::test]
    async fn an_expired_session_reads_as_not_signed_in_even_with_a_non_json_body() {
        // Apple answers 421 with an HTML error page. Letting the JSON parse
        // failure decide would turn the clearest signal in the protocol into
        // an unexplained error.
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(421)
            .with_body("<html>gone</html>")
            .create_async()
            .await;

        let cfg = ClientConfig {
            client_build_number: "x".into(),
            client_mastering_number: "y".into(),
            client_id: "z".into(),
        };
        let out = validate_at(&reqwest::Client::new(), &server.url(), "a=b", &cfg).await;
        assert_eq!(out, ValidateOutcome::NotSignedIn);
    }

    // ── the injected script ─────────────────────────────────────────────

    #[test]
    fn the_init_script_writes_the_cookie_this_module_reads() {
        // The JS constant and the Rust constant are two halves of one channel
        // with nothing but this test connecting them. A rename on either side
        // silently ends the capture, and the symptom is a sign-in that never
        // completes for a reason no log explains.
        assert!(
            INIT_SCRIPT.contains(CLIENT_CFG_COOKIE),
            "the script must write the cookie name Rust looks for"
        );
        assert!(
            CLIENT_CFG_COOKIE.starts_with(MARKER_PREFIX),
            "the config cookie must be filtered out of requests to Apple"
        );
    }
}
