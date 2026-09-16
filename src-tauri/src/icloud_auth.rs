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

/// The URLs an Android harvest asks `CookieManager` about.
///
/// **Two, not one.** `CookieManager.getCookie(url)` answers with what a browser
/// would SEND to that url, so a cookie Apple scoped host-only to
/// `setup.icloud.com` is invisible to a request for `www.icloud.com` — and
/// `/validate` against `setup.icloud.com` is the first call of every session.
///
/// `idmsa.apple.com` is deliberately absent. Nothing Jodd sends ever goes
/// there; it is where Apple's own sign-in pages live, and
/// [`cookie_header_for`] would refuse an `apple.com` cookie for an
/// `icloud.com` host in any case.
///
/// The `p<N>-ckdatabasews` partition host is absent for a different reason: it
/// is not known until `/validate` answers, and it does not need to be — see
/// [`stamp_android_cookies`].
pub const ANDROID_HARVEST_URLS: [&str; 2] =
    ["https://www.icloud.com", "https://setup.icloud.com"];

/// Puts back the cookie attributes Android's API drops.
///
/// **wry's Android `cookies_for_url` parses a `Cookie:` REQUEST header**
/// (`main_pipe.rs`, `WebViewMessage::GetCookies`), which carries names and
/// values and nothing else. Every cookie therefore arrives with `domain()`,
/// `path()` and `secure()` all `None`, and [`harvest_one`]'s mapping of that to
/// `domain: ""` matches no host at all — so reusing the desktop harvest here
/// produces an empty `Cookie:` header, a `/validate` that says anonymous, and a
/// user told their session is unavailable seconds after Apple accepted their
/// password. `the_shape_android_hands_back_unstamped_matches_nothing` pins it.
///
/// Nothing is being invented: **Android already did the domain matching** — it
/// is what asking for a URL means. This restores what the header format could
/// not express.
///
/// An `*.icloud.com` cookie is stamped with the **registrable parent**,
/// `icloud.com`, rather than the host asked. That is the shape Apple's own
/// cookies carry (`Domain=.icloud.com`) and the shape the desktop jar has after
/// `harvest_one` strips the leading dot — and it is what lets one harvest serve
/// the CloudKit partition host without knowing its name. Stamping the hostname
/// instead would hand every CloudKit burst an empty jar, which is gotcha #19's
/// failure with the platforms swapped.
///
/// Anything outside `*.icloud.com` is stamped **host-only**, so the
/// over-sending accepted inside Apple's notes domain cannot escape it.
///
/// `path` is `/` because the header format carries no path and
/// [`path_matches`] treats `/` as matching everything: the permissive
/// direction, and the one the browser has already decided by answering at all.
/// `secure` is `true` because every URL asked is `https`, and the flag is only
/// ever used to withhold.
pub fn stamp_android_cookies(asked_url: &str, pairs: &[(String, String)]) -> Vec<HarvestedCookie> {
    let host = host_of(asked_url).unwrap_or_default();
    let (domain, host_only) = if host == "icloud.com" || host.ends_with(".icloud.com") {
        ("icloud.com".to_string(), false)
    } else {
        (host, true)
    };
    pairs
        .iter()
        .map(|(name, value)| HarvestedCookie {
            name: name.trim().to_string(),
            value: value.to_string(),
            domain: domain.clone(),
            path: "/".to_string(),
            host_only,
            secure: true,
        })
        .collect()
}

/// The hostname of an `https://host/…` URL, lowercased, without userinfo or
/// port. Hand-rolled rather than pulled from `url::Url` so this stays a pure
/// function over `&str` that the whole test suite can reach.
fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split('/').next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
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

/// The client version strings Android cannot read live, and the date they were
/// read on a platform that can.
///
/// **Whether `initialization_script` reaches external pages on Android is an
/// open question, not a settled fact — read this before trusting either
/// direction.** The 2026-09-10 spike concluded it does not: it used a
/// `document.title` marker that Apple's own page overwrites on load, so the
/// instrument could not tell "did not inject" from "injected, then got
/// overwritten" and the spike read the ambiguity as failure, consistent at
/// the time with wry's source (Android injects through a custom protocol
/// handler, which covers only app-served pages). The 2026-09-10 live pass
/// then produced evidence pointing the other way: `resolve_client_config`
/// logs only on its fallback branch, and the device produced zero such lines
/// while a client config still resolved. That is one inference from an
/// *absent* log line, not a positive confirming line on the success branch —
/// it is not yet confirmed either way. **Do not swing this comment to
/// asserting injection DOES work; that would repeat the same error
/// inverted.** See CLAUDE.md's iCloud-on-Android section and
/// `docs/superpowers/HANDOFF-2026-09-10-icloud-android.md` for the full
/// evidence.
///
/// These are a **fallback, not a replacement**, kept regardless of which way
/// the open question resolves: [`resolve_client_config`] prefers a real
/// capture wherever one exists, so desktop behaviour is unchanged and
/// Android upgrades itself for free if injection turns out to work there.
///
/// **Captured, never invented.** `CLAUDE.md` records this number moving
/// 2624 → 2628 → 2630 inside three weeks, and icloud-md's own hardcoded value
/// was already stale when it was read. Re-capture with:
///
/// ```text
/// curl -s https://www.icloud.com/ | grep -o 'data-cw-private-[a-z-]*="[^"]*"'
/// ```
///
/// and cross-check against the `icloud: client config build=… mastering=…`
/// line logged by `establish` on a live desktop sign-in.
pub const ANDROID_CLIENT_BUILD_NUMBER: &str = "2630Build56";
/// See [`ANDROID_CLIENT_BUILD_NUMBER`].
pub const ANDROID_CLIENT_MASTERING_NUMBER: &str = "2630Build56";
/// See [`ANDROID_CLIENT_BUILD_NUMBER`]. Logged beside the values so a future
/// CloudKit refusal has a first suspect instead of a mystery.
pub const ANDROID_CLIENT_CONFIG_CAPTURED: &str = "2026-09-10";

/// One `clientId` per app run.
///
/// Apple's web client mints a UUID per session and keeps it. A fresh one per
/// `establish` would present the account with a new client several times a
/// minute — behaviour nothing here has measured, on a private API.
static ANDROID_CLIENT_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

impl ClientConfig {
    /// See [`ANDROID_CLIENT_BUILD_NUMBER`]. Not `cfg`-gated, so the whole test
    /// suite can reach it; only its *call site* is.
    pub fn android_fallback() -> Self {
        ClientConfig {
            client_build_number: ANDROID_CLIENT_BUILD_NUMBER.to_string(),
            client_mastering_number: ANDROID_CLIENT_MASTERING_NUMBER.to_string(),
            client_id: ANDROID_CLIENT_ID
                .get_or_init(|| uuid::Uuid::new_v4().to_string().to_uppercase())
                .clone(),
        }
    }
}

/// Where the client config comes from on this platform.
///
/// Off Android this is exactly `ClientConfig::from_jar`, and `None` keeps its
/// meaning: the webview has not reached Apple yet, which is B4's readiness
/// signal and not a sign-out.
#[cfg(not(target_os = "android"))]
fn resolve_client_config(jar: &[HarvestedCookie]) -> Option<ClientConfig> {
    ClientConfig::from_jar(jar)
}

/// See the non-Android definition above. Here `None` is never returned:
/// whether the marker cookie can exist on this platform is the open question
/// [`ANDROID_CLIENT_BUILD_NUMBER`] documents, so this always tries a real
/// capture first and only falls back to the captured constants when one is
/// not there — belt-and-braces either way the open question resolves. A bare
/// `None` here would mean refusing every session on the platform rather than
/// reporting one that is not ready, which is the failure mode this avoids
/// regardless of which way injection turns out to behave.
#[cfg(target_os = "android")]
fn resolve_client_config(jar: &[HarvestedCookie]) -> Option<ClientConfig> {
    if let Some(captured) = ClientConfig::from_jar(jar) {
        return Some(captured);
    }
    crate::log!(
        "icloud: no injected client config on Android — using the values captured {} \
         (build={}). If CloudKit starts refusing requests, re-capture on a desktop first.",
        ANDROID_CLIENT_CONFIG_CAPTURED,
        ANDROID_CLIENT_BUILD_NUMBER
    );
    Some(ClientConfig::android_fallback())
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
#[cfg(all(icloud_webview, not(target_os = "android")))]
pub struct WebviewJar {
    pub app: tauri::AppHandle,
    pub label: String,
}

#[cfg(all(icloud_webview, not(target_os = "android")))]
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

/// Harvests from the **process's** cookie jar, which is where Android keeps it.
///
/// `CookieManager.getInstance()` is process-wide, so this reads through the
/// app's own main webview — one that has never visited icloud.com — and there
/// is no hidden session webview to keep alive (Component B4 is absent on this
/// platform, not ported to it).
///
/// **`cookies()` is unusable here**: wry's Android implementation is a
/// hardcoded `Ok(Vec::new())`, so the desktop harvest would read an empty jar
/// and report the session dead. `cookies_for_url` is the only door — and until
/// the fix vendored at the workspace root (gotcha #31) it killed the process
/// outright for any URL with no cookies.
///
/// Reading through `"main"` rather than an iCloud webview also sidesteps the
/// race `LiveSession` has to work around on desktop: the main webview cannot be
/// mid-close while a harvest runs.
#[cfg(target_os = "android")]
pub struct AndroidJar {
    pub app: tauri::AppHandle,
}

/// The one Android cookie read, called through whichever webview the caller
/// already has in hand.
///
/// **`CookieManager.getInstance()` is process-wide** (see [`AndroidJar`]'s doc
/// comment above), so it makes no difference whether `win` is the app's own
/// `"main"` window — which has never itself navigated to icloud.com — or the
/// visible sign-in window Apple's pages are loaded into: both read the same
/// jar. `AndroidJar::harvest` has no window but `"main"` to reach for outside
/// a sign-in flow, so it uses that; [`sign_in`]'s poll loop already holds the
/// sign-in window and passes it directly instead of looking `"main"` up a
/// second time.
///
/// Extracted so there is exactly one Android call to `cookies_for_url` in this
/// file — before this, `sign_in`'s poll read its jar through `win.cookies()`
/// (the desktop call), which is a hardcoded `Ok(Vec::new())` stub on Android
/// and never reads anything: a user could complete sign-in successfully and
/// still watch the poll run its full ten minutes to "iCloud sign-in timed
/// out." with nothing in any log naming why.
#[cfg(target_os = "android")]
fn harvest_android_from(win: &tauri::WebviewWindow) -> Result<Vec<HarvestedCookie>, String> {
    let mut out: Vec<HarvestedCookie> = Vec::new();
    for raw in ANDROID_HARVEST_URLS {
        let url = tauri::Url::parse(raw).map_err(|e| e.to_string())?;
        let cs = win
            .cookies_for_url(url)
            .map_err(|e| format!("could not read the iCloud session for {raw}: {e}"))?;
        let pairs: Vec<(String, String)> = cs
            .iter()
            .map(|c| (c.name().to_string(), c.value().to_string()))
            .collect();
        // First host wins. The two URLs overlap heavily and a duplicate
        // name in a `Cookie:` header is ambiguous to the server.
        for c in stamp_android_cookies(raw, &pairs) {
            if !out.iter().any(|k| k.name == c.name) {
                out.push(c);
            }
        }
    }
    Ok(out)
}

#[cfg(target_os = "android")]
#[async_trait::async_trait]
impl CookieSource for AndroidJar {
    async fn harvest(&self) -> Result<Vec<HarvestedCookie>, String> {
        use tauri::Manager;
        let win = self
            .app
            .get_webview_window("main")
            .ok_or_else(|| "no main webview to harvest the iCloud session from".to_string())?;
        harvest_android_from(&win)
    }
}

/// The [`CookieSource`] this platform uses.
///
/// **One factory, because three call sites drift** — the same reason
/// [`isolated`] exists for the data store. `icloud_vertical_for`,
/// `icloud_sign_in`'s ADP gate and `icloud_reauthenticate` all construct one,
/// and each was naming `LiveSession` directly.
#[cfg(all(icloud_webview, not(target_os = "android")))]
pub fn cookie_source(app: &tauri::AppHandle) -> std::sync::Arc<dyn CookieSource> {
    std::sync::Arc::new(LiveSession { app: app.clone() })
}

/// See the non-Android definition above.
#[cfg(target_os = "android")]
pub fn cookie_source(app: &tauri::AppHandle) -> std::sync::Arc<dyn CookieSource> {
    std::sync::Arc::new(AndroidJar { app: app.clone() })
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

    let http = reqwest::Client::new();
    let mut anonymous_ticks = 0u32;
    loop {
    let jar = cookies.harvest().await.map_err(|e| {
        crate::log!("icloud: cookie harvest failed: {e}");
        TransportError::Auth
    })?;
    let Some(client) = resolve_client_config(&jar) else {
        // The marker cookie is written by our own injected script off Apple's
        // first request. Missing means the webview has not loaded icloud.com
        // yet — not that the user is signed out.
        crate::log!("icloud: no client config captured yet — the webview has not reached Apple");
        return Err(TransportError::Auth);
    };
    crate::log!(
        "icloud: client config build={} mastering={}",
        client.client_build_number,
        client.client_mastering_number
    );
    let header = cookie_header_for(SETUP_HOSTNAME, VALIDATE_PATH, &jar);
    if header.is_empty() {
        crate::log!("icloud: harvested jar carries no session for {SETUP_HOSTNAME}");
        return Err(TransportError::Auth);
    }

    match validate_at(&http, SETUP_HOST, &header, &client).await {
        ValidateOutcome::Complete { apple_id, dsid, ck_host } => {
            return Ok(IcloudSession { apple_id, dsid, ck_host, client });
        }
        ValidateOutcome::NoCloudKitService => {
            return Err(TransportError::Permanent {
                source: anyhow::anyhow!(
                    "Apple reports no iCloud Notes service for this Apple ID. Turn Notes on for iCloud on an Apple device."
                ),
            });
        }
        // Both are "the webview has work to do", which is what Auth means to
        // the retry policy and what the revival path exists to answer.
        //
        // **They must SAY so.** Every other arm of this function logs its
        // cause, and these two — the overwhelmingly common ones — logged
        // nothing, so a session failing here was indistinguishable in the log
        // from a jar that was never harvested. Chasing one such failure cost a
        // round of reading code that turned out to be working: the harvest
        // succeeded, the config was captured, the header was built, and Apple
        // simply answered "anonymous". The count is included because an
        // otherwise healthy jar and an empty one produce the same verdict here.
        ValidateOutcome::TwoFactorPending => {
            crate::log!(
                "icloud: Apple accepted the password but two-factor is still pending ({} cookies)",
                jar.len()
            );
            return Err(TransportError::Auth);
        }
        // **Not a verdict on the first tick — see REAUTH_TICKS.** A webview
        // created moments ago has loaded icloud.com anonymously and Apple's own
        // JS has not finished re-authenticating against the stored session yet.
        ValidateOutcome::NotSignedIn if anonymous_ticks < REAUTH_TICKS => {
            if anonymous_ticks == 0 {
                crate::log!(
                    "icloud: the webview is anonymous so far ({} cookies) - giving Apple's own JS time to re-authenticate",
                    jar.len()
                );
            }
            anonymous_ticks += 1;
            tokio::time::sleep(REAUTH_INTERVAL).await;
            continue;
        }
        ValidateOutcome::NotSignedIn => {
            crate::log!(
                "icloud: /validate still says anonymous after {:?} ({} cookies harvested, {} of them Apple's)",
                REAUTH_INTERVAL * REAUTH_TICKS,
                jar.len(),
                jar.iter()
                    .filter(|c| {
                        let n = c.name.to_ascii_uppercase();
                        n.starts_with("X-APPLE") || n.starts_with("X_APPLE")
                    })
                    .count()
            );
            return Err(TransportError::Auth);
        }
        ValidateOutcome::Failed(m) => {
            // A dropped connection is not a verdict about the session.
            return Err(TransportError::Transient { source: anyhow::anyhow!(m) });
        }
    }
    }
}

/// How long [`establish`] keeps re-asking `/validate` while the webview still
/// looks anonymous.
///
/// **This exists because the readiness test one layer down is not a session
/// test, and cannot be.** `LiveSession::harvest` waits for the client-config
/// marker, which our injected script writes off Apple's FIRST request — and an
/// anonymous page load issues that request too. So the marker proves "the
/// webview reached Apple", never "the webview is signed in", and harvest
/// returns as soon as it appears.
///
/// Measured on Windows 2026-09-09: after `sign_in` closed its window, a fresh
/// hidden webview answered in **1.1 s with 3 cookies**, `/validate` said
/// anonymous, and `index_account` failed — so a sign-in that had just read 773
/// notes successfully indexed none of them. Apple's own JS needs seconds to
/// re-authenticate a cold webview against the stored session; nothing was
/// giving it any.
///
/// `/validate` is the only real test, and it lives here, so the waiting lives
/// here too. The cost falls on a genuinely signed-out account — one that will
/// keep answering anonymous — and is bounded so the sync worker is never held
/// for long. That is the right side to pay on: a slow correct answer beats a
/// fast wrong one that empties the user's note list.
// NOT `#[cfg(desktop)]`: these are consumed by `establish`, which is itself
// ungated (it only takes a `CookieSource` and calls `/validate`, with no
// desktop-only types), so it — and therefore these two constants — is
// compiled for the Android target too. Gating them desktop-only left
// `establish`'s re-auth loop referencing names that do not exist on Android
// (`cannot find value REAUTH_TICKS`), a break invisible to CI (ubuntu) and a
// Windows/macOS host because on every DESKTOP target `cfg(desktop)` is true —
// it surfaced only in the Android cross-compile, which the release's
// SQLCipher gate is the one thing that runs. A `u32` and a `Duration` carry
// nothing platform-specific, so the honest fix is to let them exist wherever
// their one caller does.
const REAUTH_TICKS: u32 = 12;
const REAUTH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1250);

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
/// The hidden window shares the sign-in window's store — see [`isolated`] for
/// the per-platform lever — which is what makes the persisted store the *same*
/// store: Apple's own JS then re-authenticates silently against it on load,
/// with no user interaction. **Measured on both engines, 2026-09-09**
/// (probe Q6), so this is not a macOS-only mechanism.
///
/// On Apple it works from macOS 14 up — below it wry falls back to the shared default
/// store **with no error** (which is also why this install holds one Apple ID;
/// see [`refuse_second_icloud_account`]).
///
/// Created **lazily and reused**, never per call: an always-resident
/// `WKWebView` costs real memory for a session that is usually valid, and a
/// fresh one per read would reload icloud.com every few seconds.
///
/// Prefers the visible sign-in window while it exists, so a read during sign-in
/// does not race a second webview against the one the user is looking at.
#[cfg(all(icloud_webview, not(target_os = "android")))]
pub fn ensure_session_webview(app: &tauri::AppHandle) -> Result<String, String> {
    use tauri::Manager;

    if app.get_webview_window(SIGNIN_WINDOW).is_some() {
        return Ok(SIGNIN_WINDOW.to_string());
    }
    open_session_window(app)?;
    Ok(SESSION_WINDOW.to_string())
}

/// Creates the hidden session webview if it is not already there.
///
/// **Split out of [`ensure_session_webview`] because that function prefers the
/// SIGN-IN window and returns early when one exists — which made it useless to
/// the one caller that needs a hidden window created while the sign-in window
/// is still open.** [`close_signin_window`] called it for exactly that and got
/// a silent no-op: the fix looked right, changed the log order, and left the
/// defect in place. Measured 2026-09-09.
///
/// Idempotent, so every caller can just ask.
#[cfg(all(icloud_webview, not(target_os = "android")))]
fn open_session_window(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

    if app.get_webview_window(SESSION_WINDOW).is_some() {
        return Ok(());
    }
    let url = tauri::Url::parse(ICLOUD_URL).map_err(|e| e.to_string())?;
    // `isolated` gives it the SAME store the sign-in window wrote — otherwise
    // Apple's JS has nothing to re-authenticate against and this window is a
    // stranger. That it is the same call on every platform is the point.
    isolated(WebviewWindowBuilder::new(app, SESSION_WINDOW, WebviewUrl::External(url)))
        .title("Jodd — iCloud session")
        .visible(false)
        // The injected script must run here too: `establish` reads the client
        // version strings out of the jar, and a window that never captured
        // them yields no config however good its session is.
        .initialization_script(INIT_SCRIPT)
        .build()
        .map_err(|e| format!("could not open the iCloud session webview: {e}"))?;

    crate::log!("icloud: opened the hidden session webview");
    Ok(())
}

/// Closes the visible sign-in window, leaving the session in the data store.
///
/// The counterpart of [`sign_in`] returning with it open. **Not
/// [`forget_session`]**, which also deletes the store — that is for removing
/// the account, and calling it here would throw away the session that was just
/// established.
#[cfg(all(icloud_webview, not(target_os = "android")))]
pub fn close_signin_window(app: &tauri::AppHandle) {
    use tauri::Manager;

    // **Open the successor BEFORE closing this one, or the session dies with
    // the window.** Tauri keys its `WebContextStore` on the isolation
    // parameter, and `WebviewWrapper::drop` REMOVES the entry once the last
    // webview referencing it goes away (everywhere but Linux). So closing the
    // only iCloud webview tears the web context down, and the next one built
    // for the same store is a fresh browser session that can see nothing but
    // what was persisted to disk — which, unless the user ticked Apple's
    // "Keep me signed in", is almost nothing.
    //
    // Measured on Windows 2026-09-09, twice: sign-in read 773 notes through
    // this window, this function closed it, and the hidden webview created
    // milliseconds later harvested FOUR cookies and validated as anonymous,
    // so `index_account` failed and the user was told "iCloud session
    // unavailable: auth" about a sign-in that had just succeeded.
    //
    // The comment below this one already learned half the lesson — keep the
    // window open through the ADP check — and the same class of failure simply
    // moved one step later.
    //
    // `ensure_session_webview` is what holds the reference across the
    // handover. Its failure is deliberately not fatal: this function's job is
    // to close a window, and a caller that could not open the hidden one is no
    // worse off for having closed this one — the next harvest rebuilds it.
    if let Err(e) = open_session_window(app) {
        crate::log!("icloud: could not open the session webview before closing sign-in: {e}");
    }

    if let Some(w) = app.get_webview_window(SIGNIN_WINDOW) {
        let _ = w.close();
    }
}

/// The Android counterpart: close the window, and nothing else.
///
/// **There is no successor to open.** The jar is the process's
/// (`CookieManager.getInstance()`), so nothing is handed over and nothing dies
/// with this window — which is the whole reason [`AndroidJar`] can read through
/// the main webview afterwards.
///
/// Opening one anyway would be the most visible failure available in this
/// design: `set_visible(false)` is unsupported on Android, so a "hidden"
/// session webview is a full-screen Apple page permanently on top of Jodd.
#[cfg(target_os = "android")]
pub fn close_signin_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window(SIGNIN_WINDOW) {
        let _ = w.close();
        crate::log!("icloud: closed the sign-in webview");
    }
    restore_main_content_view(app);
}

/// Jodd's own webview, remembered before Apple's page takes the screen.
///
/// Android has no second window to put that page in: wry attaches a webview
/// with **`activity.setContentView(webview)`**, which is a replacement, not an
/// overlay — Jodd's own view is detached from the hierarchy, not merely
/// covered. Nothing hands it back, because `close()` only forgets the webview
/// on tauri's Rust side (gotcha #31).
#[cfg(target_os = "android")]
static MAIN_WEBVIEW: std::sync::OnceLock<std::sync::Mutex<Option<jni::objects::GlobalRef>>> =
    std::sync::OnceLock::new();

/// Take a global reference to Jodd's own webview while it is still the one
/// wry has registered for this activity.
///
/// **Order is the whole trick.** `JniHandle::exec` hands the closure whatever
/// webview `ACTIVITY_PROXY` currently holds — a single slot per activity, which
/// the sign-in webview *overwrites* when it is created. Called after that, this
/// would remember Apple's page and "restoring" it would put the user back where
/// they started. Called before, the slot can only hold the main webview, so no
/// identity check is needed — and the call site is the line above the builder
/// for exactly that reason.
#[cfg(target_os = "android")]
fn remember_main_webview(app: &tauri::AppHandle) {
    use tauri::Manager;

    let Some(win) = app.get_webview_window("main") else {
        crate::log!("icloud: no main webview to remember — Apple's page will stay on screen");
        return;
    };
    let res = win.with_webview(|pw| {
        pw.jni_handle().exec(|env, _activity, webview| {
            if webview.is_null() {
                crate::log!("icloud: the activity had no webview registered to remember");
                return;
            }
            match env.new_global_ref(webview) {
                Ok(r) => {
                    let slot = MAIN_WEBVIEW.get_or_init(|| std::sync::Mutex::new(None));
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(r);
                    }
                    crate::log!("icloud: remembered Jodd's own webview before opening Apple's page");
                }
                Err(e) => {
                    let _ = env.exception_clear();
                    crate::log!("icloud: could not remember Jodd's own webview ({e})");
                }
            }
        });
    });
    if let Err(e) = res {
        crate::log!("icloud: could not reach the main webview to remember it ({e})");
    }
}

/// Put Jodd's own webview back on screen after the sign-in page is done.
///
/// This is what `close()` cannot do here, and the user-visible half of the
/// sign-in: without it Apple's finished page sits on top of Jodd until the
/// process is killed, however successful the sign-in was.
///
/// Every step is checked rather than unwrapped — this runs on the Android main
/// thread, so a panic takes the app down right after a sign-in that worked. A
/// failure leaves exactly the old behaviour (Apple's page stays), which is why
/// it logs and returns instead of retrying.
#[cfg(target_os = "android")]
fn restore_main_content_view(app: &tauri::AppHandle) {
    use tauri::Manager;

    let remembered = MAIN_WEBVIEW
        .get()
        .and_then(|m| m.lock().ok().and_then(|g| g.clone()));
    let Some(main_ref) = remembered else {
        crate::log!(
            "icloud: no remembered webview to restore — Apple's page stays up; close Jodd from              the app switcher and reopen it, the account is already signed in"
        );
        return;
    };
    let Some(win) = app.get_webview_window("main") else {
        crate::log!("icloud: no main window to run the restore through");
        return;
    };

    let res = win.with_webview(move |pw| {
        pw.jni_handle().exec(move |env, activity, _webview| {
            let view = main_ref.as_obj();
            let mut call = || -> Result<(), jni::errors::Error> {
                // `setContentView` throws if the view still has a parent, and
                // whether it does depends on how Android tore the old content
                // down — so detach first rather than assume either way.
                let parent = env
                    .call_method(view, "getParent", "()Landroid/view/ViewParent;", &[])?
                    .l()?;
                if !parent.is_null() {
                    env.call_method(
                        &parent,
                        "removeView",
                        "(Landroid/view/View;)V",
                        &[(&view).into()],
                    )?;
                }
                env.call_method(
                    activity,
                    "setContentView",
                    "(Landroid/view/View;)V",
                    &[(&view).into()],
                )?;
                Ok(())
            };
            match call() {
                Ok(()) => crate::log!("icloud: put Jodd's own webview back on screen"),
                Err(e) => {
                    // A pending Java exception left unhandled turns the next
                    // JNI call in this process into undefined behaviour.
                    let _ = env.exception_clear();
                    crate::log!(
                        "icloud: could not put Jodd's webview back ({e}) — Apple's page stays                          up; close Jodd from the app switcher and reopen it"
                    );
                }
            }
        });
    });
    if let Err(e) = res {
        crate::log!("icloud: could not reach the main webview to restore it ({e})");
    }
}

/// A [`CookieSource`] over whichever webview currently holds the session,
/// creating the hidden one if needed.
///
/// Resolves the window **per harvest**, not once at construction: the visible
/// sign-in window can close between two reads, and a jar bound to a dead window
/// would report the session gone when it is merely somewhere else.
#[cfg(all(icloud_webview, not(target_os = "android")))]
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
#[cfg(all(icloud_webview, not(target_os = "android")))]
const WARMUP_TICKS: u32 = 40;
#[cfg(all(icloud_webview, not(target_os = "android")))]
const WARMUP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(all(icloud_webview, not(target_os = "android")))]
impl LiveSession {
    /// Opens the hidden window, bypassing the sign-in window entirely.
    ///
    /// Used when the preferred window turns out to be unusable — see
    /// [`CookieSource::harvest`] below for why "present" and "usable" are not
    /// the same thing here.
    fn hidden_only(&self) -> Result<WebviewJar, String> {
        open_session_window(&self.app)?;
        Ok(WebviewJar { app: self.app.clone(), label: SESSION_WINDOW.to_string() })
    }
}

#[cfg(all(icloud_webview, not(target_os = "android")))]
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
#[cfg(icloud_webview)]
pub async fn forget_session(app: &tauri::AppHandle) {
    // **THE ORDER INVERTS BETWEEN PLATFORMS, and each half is a silent no-op
    // in the other's order.** WebKit refuses to delete a store that is still
    // in use, so the Apple arm must close the windows FIRST. WebView2 reaches
    // the profile *through* a live webview's `ICoreWebView2Profile2`, so the
    // other arm has nothing left to call once the windows are gone and must
    // clear FIRST. Neither mistake reports an error: the account is removed,
    // the jar survives, and the next sign-in silently adopts the previous
    // Apple ID — the wrong-account bug this function exists to prevent, not a
    // leak. Measured on both engines, 2026-09-09
    // (`examples/icloud_webview_probe -- --forget`).
    #[cfg(all(not(target_vendor = "apple"), not(target_os = "android")))]
    clear_browsing_data_first(app).await;

    // Clear FIRST, close after — the Windows order, for the Windows reason: the
    // lever is reached *through* a live webview. Stated rather than inherited,
    // because this order inverts on Apple and neither mistake reports an error.
    #[cfg(target_os = "android")]
    {
        remove_all_cookies(app);
        // `JniHandle::exec` queues the closure on the webview thread and
        // returns; this is the only thing that gives it a chance to land.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    close_icloud_windows(app);

    #[cfg(target_vendor = "apple")]
    {
        use tauri::Manager;
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
}

/// Closes both iCloud webviews, whichever of them exist.
#[cfg(icloud_webview)]
fn close_icloud_windows(app: &tauri::AppHandle) {
    use tauri::Manager;
    for label in [SIGNIN_WINDOW, SESSION_WINDOW] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.close();
            crate::log!("icloud: closed webview '{label}' while forgetting the session");
        }
    }
}

/// Empties Android's process-wide cookie jar.
///
/// **The lever the other platforms use is not one here.** wry's
/// `clear_all_browsing_data` maps to `RustWebView.clearAllBrowsingData()`,
/// which deletes caches, history and form data and never touches
/// `CookieManager` — so it would return `Ok(())` having preserved the one thing
/// this function exists to destroy. That is the third member of the family
/// `CLAUDE.md` names for the desktop pair: each platform ignores the other's
/// lever silently.
///
/// `flush()` is not tidiness: `removeAllCookies` is asynchronous and Android
/// may kill the process before the removal reaches disk, which would resurrect
/// the jar on the next launch — the wrong-account bug, delayed rather than
/// avoided.
///
/// Runs on the webview's own thread through wry's `JniHandle`, so no thread has
/// to be attached to the JVM by hand. `exec` is fire-and-forget (its closure
/// returns `()`), hence the short wait in the caller.
///
/// The main webview is used deliberately: the jar is the process's, so this
/// works with no iCloud webview alive — including when a sign-in was abandoned
/// before one existed.
///
/// **This is the opposite scoping from the Windows arm next to it
/// (`clear_browsing_data_first`), and that asymmetry is deliberate, not an
/// oversight.** `CookieManager` on Android is process-wide, so this clears
/// cookies for every webview in the app, not just the iCloud ones — where
/// WebView2's `ClearBrowsingDataAll` is scoped to Jodd's own isolated iCloud
/// profile and leaves the rest of the app alone. It is acceptable here
/// because nothing else in the app relies on cookies surviving this call:
/// Gmail and Microsoft OAuth run in the system browser, not in an in-process
/// webview (gotcha #8), so there is no other cookie-bearing surface for this
/// to collaterally wipe.
#[cfg(target_os = "android")]
fn remove_all_cookies(app: &tauri::AppHandle) {
    use tauri::Manager;

    let Some(win) = app.get_webview_window("main") else {
        crate::log!(
            "icloud: no main webview to reach CookieManager through. The next sign-in may find \
             the previous Apple ID still signed in; /validate reports whose session it is."
        );
        return;
    };

    let res = win.with_webview(|pw| {
        pw.jni_handle().exec(|env, _activity, _webview| {
            use jni::objects::JObject;

            // Every step is checked rather than unwrapped: this runs on the
            // Android main thread, and a panic here takes the app down during
            // an account removal the user has already confirmed.
            let mut call = || -> Result<(), jni::errors::Error> {
                let class = env.find_class("android/webkit/CookieManager")?;
                let cm = env
                    .call_static_method(
                        &class,
                        "getInstance",
                        "()Landroid/webkit/CookieManager;",
                        &[],
                    )?
                    .l()?;
                let null = JObject::null();
                env.call_method(
                    &cm,
                    "removeAllCookies",
                    "(Landroid/webkit/ValueCallback;)V",
                    &[(&null).into()],
                )?;
                env.call_method(&cm, "flush", "()V", &[])?;
                Ok(())
            };
            match call() {
                Ok(()) => crate::log!("icloud: emptied Android's cookie jar"),
                Err(e) => {
                    // A pending Java exception left unhandled turns the next
                    // JNI call in this process into undefined behaviour.
                    let _ = env.exception_clear();
                    crate::log!(
                        "icloud: could not empty Android's cookie jar ({e}). The next sign-in \
                         may find the previous Apple ID still signed in; /validate reports \
                         whose session it is."
                    );
                }
            }
        });
    });
    if let Err(e) = res {
        crate::log!(
            "icloud: could not reach the Android webview to clear cookies: {e}. The next \
             sign-in may find the previous Apple ID still signed in; /validate reports whose \
             session it is."
        );
    }
}

/// The non-Apple half of [`forget_session`], which must run **before** the
/// windows close — see that function's comment for why.
///
/// `clear_all_browsing_data` maps to `ICoreWebView2Profile2::ClearBrowsingDataAll`,
/// and wry hands it a completion handler that discards the result, so this is
/// fire-and-forget: the short wait afterwards is the only thing that gives it a
/// chance to land before the window it ran on is torn down.
///
/// **Clearing the whole profile is safe only because the profile is Jodd's
/// own** — see [`isolated`]. Without a `data_directory` these webviews would
/// share the app's WebView2 profile and this call would wipe the frontend's
/// storage with it. The probe's Q5 is what checks that, and it must stay green.
#[cfg(all(desktop, not(target_vendor = "apple")))]
async fn clear_browsing_data_first(app: &tauri::AppHandle) {
    use tauri::Manager;
    // Either window reaches the same profile, so the first one that answers is
    // enough. A sign-in that was abandoned leaves neither, and there is then
    // nothing to clear.
    for label in [SESSION_WINDOW, SIGNIN_WINDOW] {
        let Some(w) = app.get_webview_window(label) else { continue };
        match w.clear_all_browsing_data() {
            Ok(()) => {
                crate::log!("icloud: cleared the webview profile via '{label}'");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                return;
            }
            Err(e) => crate::log!("icloud: could not clear the profile via '{label}': {e}"),
        }
    }
    crate::log!(
        "icloud: no live webview to clear the profile through. A later sign-in may find the \
         previous Apple ID still signed in; /validate reports whose session it is."
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Component A3 — one Apple ID per install
// ────────────────────────────────────────────────────────────────────────────

/// Refuses a second iCloud account **before anything is persisted**.
///
/// Jodd names **one** store for the iCloud webviews ([`isolated`]), so every
/// account shares one cookie jar and a second Apple ID would silently inherit
/// the first one's session.
///
/// **The reason is "Jodd does not do this yet", not "the platform cannot", and
/// the message says so.** Both levers take a parameter — an identifier on
/// Apple (`WKWebsiteDataStore(forIdentifier:)`, macOS 14+), a path on Windows
/// and Linux — so per-account isolation is a matter of deriving that
/// parameter from the `account_id` rather than hardcoding it. Wording that
/// claimed a platform limitation would send whoever lifts it hunting for a
/// workaround that is not needed.
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
             (A limit of the current milestone, not of this platform — the sign-in \
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
#[cfg(all(desktop, target_vendor = "apple"))]
pub const DATA_STORE_ID: [u8; 16] = *b"jodd-icloud-0001";

/// The non-Apple counterpart of [`DATA_STORE_ID`]: an identifier there, a path
/// here, and Tauri keys the web context on the path.
///
/// Under Jodd's own data base (`paths.rs`, so Android's sandbox rule is
/// honoured if this ever runs there) and beside nothing else, because
/// [`forget_session`] empties this profile wholesale.
#[cfg(all(desktop, not(target_vendor = "apple")))]
fn icloud_webview_dir() -> std::path::PathBuf {
    crate::paths::data_base()
        .unwrap_or_else(std::env::temp_dir)
        .join("jodd")
        .join("icloud-webview")
}

/// Gives an iCloud webview its own persistent cookie jar, by whatever
/// mechanism this platform has.
///
/// **The whole platform seam, in one place because three call sites drift.**
/// `sign_in`, `ensure_session_webview` and `LiveSession::hidden_only` all have
/// to name the SAME store or B4 is a stranger to the session it is meant to
/// resume — and each was configuring itself independently.
///
/// WebKit has no per-webview data *directory*; `data_store_identifier` exists
/// precisely as its replacement, mapped to `WKWebsiteDataStore(forIdentifier:)`
/// on macOS 14+. WebView2 has no data *store identifier* — Tauri's method
/// compiles there and does nothing at all — but it does take a user data
/// folder, which Tauri keys its `WebContextStore` on and wry hands to
/// `CreateCoreWebView2EnvironmentWithOptions`.
///
/// **Each platform ignores the other's lever silently**, which is why calling
/// the wrong one looks like it worked: the webviews still open, still sign in,
/// and quietly share the app's own profile. Measured 2026-09-09 on both
/// engines with `examples/icloud_webview_probe`.
#[cfg(all(desktop, target_vendor = "apple"))]
fn isolated<'a, R: tauri::Runtime, M: tauri::Manager<R>>(
    b: tauri::WebviewWindowBuilder<'a, R, M>,
) -> tauri::WebviewWindowBuilder<'a, R, M> {
    b.data_store_identifier(DATA_STORE_ID)
}

/// See the Apple definition above for what this is and why there are two.
#[cfg(all(desktop, not(target_vendor = "apple")))]
fn isolated<'a, R: tauri::Runtime, M: tauri::Manager<R>>(
    b: tauri::WebviewWindowBuilder<'a, R, M>,
) -> tauri::WebviewWindowBuilder<'a, R, M> {
    b.data_directory(icloud_webview_dir())
}

/// Android's arm: the identity function, deliberately.
///
/// **The lever that matters here is not per-webview at all.** Android keeps cookies in
/// `CookieManager.getInstance()`, which is process-wide, so no builder option can give
/// an iCloud webview its own jar. A `data_directory` would isolate everything EXCEPT the
/// cookies — the one thing this function exists to isolate — and would read, at every
/// call site, as isolation that is not happening. Naming it as identity keeps the
/// asymmetry visible instead of hiding it behind a call that looks like the other two.
///
/// This is also why [`refuse_second_icloud_account`] is *more* true on Android than
/// elsewhere: on the desktop platforms the limit is Jodd's, and here there is no
/// parameter to derive per account even in principle.
#[cfg(target_os = "android")]
fn isolated<'a, R: tauri::Runtime, M: tauri::Manager<R>>(
    b: tauri::WebviewWindowBuilder<'a, R, M>,
) -> tauri::WebviewWindowBuilder<'a, R, M> {
    b
}

/// How often the poll asks `/validate`, and for how long in total.
///
/// Ten minutes is not generosity: a first sign-in can involve a password, a
/// 2FA code typed off another device, and occasionally a CAPTCHA. A tighter cap
/// would time out a user who is doing everything right.
#[cfg(icloud_webview)]
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
#[cfg(icloud_webview)]
const POLL_TICKS: u32 = 200;

/// Converts one webview cookie into the plain form the rules above work on.
///
/// See [`HarvestedCookie::host_only`] for why that flag is `false` here rather
/// than read from the cookie: the leading dot that carries the distinction is
/// stripped before Tauri hands it over, and cannot be recovered.
///
/// Back to `not(target_os = "android")`, per gating file R4. This used to be
/// widened to plain `icloud_webview` because `sign_in`'s poll loop called it
/// directly, bypassing `WebviewJar` entirely — but that call was itself the
/// bug: `sign_in` read `win.cookies()` on Android, which is wry's hardcoded
/// `Ok(Vec::new())` stub, so the poll harvested nothing, forever, no matter
/// how a real sign-in completed. Now that `harvest_from_signin_window` (below)
/// splits that read per platform, this function's only callers —
/// [`WebviewJar::harvest`] and this file's desktop arm of
/// `harvest_from_signin_window` — are both already `not(target_os =
/// "android")`, so widening it bought nothing and hid the real defect behind
/// a function that compiled but was never reachable correctly.
#[cfg(all(icloud_webview, not(target_os = "android")))]
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

/// The sign-in poll's cookie read, split per platform so `sign_in` itself
/// stays a single, platform-neutral function.
///
/// **Desktop: byte-for-byte what `sign_in` used to do inline** — `win.cookies()`
/// mapped through [`harvest_one`], with the same "window is tearing down, not
/// a bug" error on failure. Nothing here changes desktop behaviour.
///
/// **Android: `win.cookies()` is a hardcoded `Ok(Vec::new())` stub in wry**, so
/// the desktop code above would silently harvest an empty jar on every tick —
/// which is exactly what shipped before this split: a user could complete
/// sign-in, including 2FA, and still watch the poll run its full ten minutes
/// to "iCloud sign-in timed out.", because `ClientConfig::from_jar` (fed by
/// that empty jar) never saw a client config to validate against, and nothing
/// logged why. `harvest_android_from` is the one place on this platform that
/// calls the real door, `cookies_for_url`, and it is passed `win` — the
/// visible sign-in window itself — rather than looking up `"main"` again,
/// since `CookieManager.getInstance()` is process-wide and both windows read
/// the same jar (see [`AndroidJar`]).
#[cfg(all(icloud_webview, not(target_os = "android")))]
fn harvest_from_signin_window(win: &tauri::WebviewWindow) -> Result<Vec<HarvestedCookie>, String> {
    match win.cookies() {
        Ok(cs) => Ok(cs.iter().map(harvest_one).collect()),
        // The window is tearing down. Treat it as the cancel it almost
        // always is rather than retrying a webview that no longer exists.
        Err(e) => Err(format!("iCloud sign-in window closed ({e}).")),
    }
}

/// See the non-Android definition above.
#[cfg(all(icloud_webview, target_os = "android"))]
fn harvest_from_signin_window(win: &tauri::WebviewWindow) -> Result<Vec<HarvestedCookie>, String> {
    harvest_android_from(win)
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
#[cfg(icloud_webview)]
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
    // Must run BEFORE the builder: creating this webview overwrites the one
    // `JniHandle::exec` reports for the activity. See `remember_main_webview`.
    #[cfg(target_os = "android")]
    remember_main_webview(app);
    let win = isolated(WebviewWindowBuilder::new(app, SIGNIN_WINDOW, WebviewUrl::External(url)))
        .title("Sign in to iCloud")
        .inner_size(1100.0, 820.0)
        .initialization_script(INIT_SCRIPT)
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

        let jar: Vec<HarvestedCookie> = harvest_from_signin_window(&win)?;

        // No config yet means Apple's own first request has not gone out, so
        // there is nothing to validate against. This is the normal state for
        // the first tick or two, not an error.
        //
        // `resolve_client_config`, not `ClientConfig::from_jar` directly:
        // whether [`INIT_SCRIPT`] reaches icloud.com on Android and writes
        // the marker cookie this reads is an open question (see
        // `ANDROID_CLIENT_BUILD_NUMBER`'s doc comment), not a settled "never"
        // — so a bare `from_jar` risks refusing every Android session at this
        // line, forever, if the answer turns out to be no. The Android arm
        // tries a real capture first and only falls back to a captured
        // version string when one is not there — see `resolve_client_config`.
        let Some(client) = resolve_client_config(&jar) else { continue };

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

    // ── Android: the attributes CookieManager's answer does not carry ────
    mod android_harvest {
        use super::*;

        fn pairs(names: &[&str]) -> Vec<(String, String)> {
            names.iter().map(|n| ((*n).to_string(), format!("{n}-value"))).collect()
        }

        /// The whole reason the stamp exists: the partition host is not known
        /// until `/validate` answers, so the harvest cannot ask for it — a
        /// cookie stamped with the registrable parent reaches it anyway.
        #[test]
        fn a_stamped_jar_reaches_the_cloudkit_partition_host() {
            let jar = stamp_android_cookies(
                "https://www.icloud.com",
                &pairs(&["X-APPLE-WEBAUTH-TOKEN"]),
            );
            let header = cookie_header_for("p130-ckdatabasews.icloud.com", "/database/1", &jar);
            assert!(header.contains("X-APPLE-WEBAUTH-TOKEN=X-APPLE-WEBAUTH-TOKEN-value"));
        }

        #[test]
        fn a_stamped_jar_reaches_the_setup_host_that_bootstraps_every_session() {
            let jar =
                stamp_android_cookies("https://setup.icloud.com", &pairs(&["X_APPLE_WEB_KB-ABC"]));
            let header = cookie_header_for(SETUP_HOSTNAME, VALIDATE_PATH, &jar);
            assert!(header.contains("X_APPLE_WEB_KB-ABC="));
        }

        /// **The trap, pinned as a test rather than merely avoided.** Android's
        /// `cookies_for_url` parses a `Cookie:` REQUEST header, so every cookie
        /// arrives with `domain()`, `path()` and `secure()` all `None`;
        /// `harvest_one` turns that into `domain: ""`, which domain-matches
        /// nothing. The symptom is not a compile error or an empty result at
        /// the call site — it is `establish` reporting a session that is gone,
        /// seconds after Apple accepted the user's password.
        #[test]
        fn the_shape_android_hands_back_unstamped_matches_nothing() {
            let unstamped = vec![HarvestedCookie {
                name: "X-APPLE-WEBAUTH-TOKEN".into(),
                value: "t".into(),
                domain: String::new(),
                path: "/".into(),
                host_only: false,
                secure: true,
            }];
            assert_eq!(cookie_header_for(SETUP_HOSTNAME, VALIDATE_PATH, &unstamped), "");
        }

        /// Stamping must not smuggle Jodd's own bookkeeping to Apple — the job
        /// `MARKER_PREFIX` does, which the stamp must not undo.
        #[test]
        fn jodd_markers_are_still_excluded_after_stamping() {
            let jar = stamp_android_cookies(
                "https://www.icloud.com",
                &pairs(&["jodd_icloud_cfg", "X-APPLE-DS-WEB-SESSION-TOKEN"]),
            );
            let header = cookie_header_for(SETUP_HOSTNAME, VALIDATE_PATH, &jar);
            assert!(!header.contains("jodd_icloud_cfg"));
            assert!(header.contains("X-APPLE-DS-WEB-SESSION-TOKEN="));
        }

        /// A host outside `*.icloud.com` is stamped host-only, so the
        /// over-sending the registrable-parent stamp accepts inside Apple's
        /// notes domain cannot escape it.
        #[test]
        fn a_host_outside_icloud_is_stamped_host_only() {
            let jar = stamp_android_cookies("https://idmsa.apple.com", &pairs(&["aasp"]));
            assert_eq!(jar[0].domain, "idmsa.apple.com");
            assert!(jar[0].host_only);
            assert_eq!(cookie_header_for(SETUP_HOSTNAME, VALIDATE_PATH, &jar), "");
        }

        #[test]
        fn the_harvest_urls_are_both_icloud_hosts() {
            for u in ANDROID_HARVEST_URLS {
                let jar = stamp_android_cookies(u, &pairs(&["s"]));
                assert_eq!(jar[0].domain, "icloud.com", "{u}");
                assert!(!jar[0].host_only, "{u}");
            }
        }
    }

    // ── Android: B5 without an injected script ──────────────────────────
    mod android_client_config {
        use super::*;

        /// The fallback is only ever right if it is complete: a partial config
        /// is sent to Apple as a malformed request whose failure says nothing
        /// about the missing capture — the same reasoning `ClientConfig::parse`
        /// already encodes by refusing partials.
        #[test]
        fn the_fallback_carries_all_three_parameters() {
            let c = ClientConfig::android_fallback();
            assert_eq!(c.client_build_number, ANDROID_CLIENT_BUILD_NUMBER);
            assert_eq!(c.client_mastering_number, ANDROID_CLIENT_MASTERING_NUMBER);
            assert!(!c.client_id.is_empty());
            let q = c.query();
            assert!(q.contains(&format!("clientBuildNumber={ANDROID_CLIENT_BUILD_NUMBER}")));
            assert!(q.contains(&format!(
                "clientMasteringNumber={ANDROID_CLIENT_MASTERING_NUMBER}"
            )));
        }

        /// **One client instance per app run.** Apple's own web client mints a
        /// `clientId` per session and keeps it; a fresh UUID on every
        /// `establish` would present the account with a new client several
        /// times a minute, which no measurement here covers.
        #[test]
        fn the_client_id_is_stable_within_a_process() {
            assert_eq!(
                ClientConfig::android_fallback().client_id,
                ClientConfig::android_fallback().client_id
            );
        }

        /// The constants are captured, not invented, and must not silently
        /// decay into the 2026-08-22 test fixtures.
        #[test]
        fn the_captured_constants_are_not_the_old_fixtures() {
            assert_ne!(ANDROID_CLIENT_BUILD_NUMBER, "2628Build44");
            assert!(ANDROID_CLIENT_BUILD_NUMBER.contains("Build"));
            assert!(!ANDROID_CLIENT_MASTERING_NUMBER.is_empty());
        }

        /// On every platform that CAN inject, a real capture still wins.
        #[test]
        fn a_captured_config_is_preferred_to_the_fallback() {
            let jar = vec![HarvestedCookie {
                name: CLIENT_CFG_COOKIE.into(),
                value: "clientBuildNumber%3D9999Build1%26clientMasteringNumber%3D9999B1%26clientId%3DABC"
                    .into(),
                domain: "icloud.com".into(),
                path: "/".into(),
                host_only: false,
                secure: true,
            }];
            let c = resolve_client_config(&jar).expect("a captured config resolves");
            assert_eq!(c.client_build_number, "9999Build1");
        }

        /// Desktop keeps its readiness signal: a jar with no marker means the
        /// webview has not reached Apple yet, and answering with a fallback
        /// would hide B4's warm-up failing.
        #[cfg(not(target_os = "android"))]
        #[test]
        fn a_jar_with_no_marker_resolves_to_nothing_off_android() {
            assert!(resolve_client_config(&[]).is_none());
        }
    }
}
