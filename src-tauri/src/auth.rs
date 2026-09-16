use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::distributions::Distribution;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// Google OAuth 2.0 endpoints.
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

// Android's redirect, delivered by Android App Links. Must match, character
// for character, both the `android:host`/`android:path` pair in
// AndroidManifest.xml and an authorized redirect URI on the Web OAuth client.
// Three places, one string — see docs/android/APP-LINKS-SETUP.md.
#[cfg(target_os = "android")]
pub const ANDROID_REDIRECT_URI: &str = "https://jodd.bbmedia.co.th/oauth2redirect";

/// Where Google sends the auth code back to.
///
/// Two platforms, two mechanisms, and the history of both is worth keeping
/// because each looks like the obvious choice right up until it fails.
///
/// **Custom scheme (`co.bbmedia.jodd:/oauth2redirect`) — dead.** This was the
/// original Android design, following Google's own Android documentation.
/// Google has since removed it: the authorization request comes back
/// `Error 400: invalid_request / Custom URI scheme is not enabled for your
/// Android client`.
///
/// **Loopback — correct on desktop, unreliable on Android.** Google accepts it
/// from any device (it validates the redirect against the client TYPE, and
/// localhost is exempt from the HTTPS-only rule), and it genuinely worked on an
/// Infinix X6821 running Android 13. It then failed on a Galaxy S23 FE running
/// Android 16, because the OS killed Jodd while the user was on the consent
/// screen, and a listener in a dead process receives nothing. Desktop has no
/// such problem: the app stays alive while the browser is in front.
///
/// **App Links — what Android uses now.** The redirect starts the app via an
/// Intent instead of requiring it to have survived, which is exactly the
/// failure above. The cost is that it only works if a real web server vouches
/// for the app, so sign-in now depends on `assetlinks.json` being reachable.
#[cfg(not(target_os = "android"))]
pub fn redirect_uri() -> &'static str {
    "http://localhost:8080/callback"
}

#[cfg(target_os = "android")]
pub fn redirect_uri() -> &'static str {
    ANDROID_REDIRECT_URI
}

// Both client types Jodd uses — Desktop and Web — carry a secret, so this is
// uniform across platforms. Kept as a function rather than folded into
// `client_secret()` because the empty-string-means-absent distinction is
// load-bearing: Google rejects `client_secret=""`, so the key must be omitted
// rather than sent blank.
pub fn client_secret_opt() -> Option<String> {
    let s = client_secret();
    if s.is_empty() { None } else { Some(s) }
}

// `gmail.modify` (sensitive scope, free verification) instead of the older
// `https://mail.google.com/` (restricted scope, requires $15k+ CASA assessment).
// Modify gives us read + insert + delete + label-modify, which covers everything
// Jodd needs.
const SCOPES: &str = "https://www.googleapis.com/auth/gmail.modify";

/// First non-blank of `configured`, `embedded`, then `runtime` — **the
/// parameters are listed in precedence order**, which is the only reliable way
/// to read this given that `auth_ms` has a different order.
///
/// **Google: configured → embedded → runtime. Microsoft:
/// configured → runtime → embedded.** `auth_ms::pick_client_id` takes its
/// arguments in *its* precedence order too, so the two signatures are not
/// interchangeable — copying a call from one module to the other silently
/// swaps two tiers. The divergence is deliberate and documented on
/// `auth_ms::client_id`: `MS_CLIENT_ID` was the only BYO mechanism through
/// 0.24.1, so demoting it below an embedded id would quietly move those users
/// onto Jodd's own registration.
///
/// Compile-time embedded outranking the runtime var is correct *here* because
/// the configured tier sits above both and is where a BYO client belongs; the
/// env var is a developer convenience, not a supported override.
///
/// Split out from [`client_id`]/[`client_secret`] so the precedence is testable
/// without mutating process environment (which races across parallel tests) or
/// writing a real config file — the same reasoning as
/// `auth_ms::pick_client_id`, which this mirrors.
///
/// Blank is absent, and blank means *after trimming*: a config file hand-edited
/// to `"  "`, or an exported-but-empty var in a shell or CI step, must not blank
/// out a working credential further down. Values are returned trimmed, so a
/// client id pasted into Settings with a trailing newline still works —
/// `save_oauth_config` already trims on the way in, so this only catches values
/// that arrived by another route.
fn pick_credential(configured: Option<&str>, embedded: Option<&str>, runtime: Option<&str>) -> String {
    [configured, embedded, runtime]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string()
}

// Three-tier resolution: user-configured (wins over embedded — any user can
// supply their own Google Cloud project without recompiling) → compile-time
// embedded (CI bakes in developer's credentials) → runtime env var (dev fallback).
//
// **The embedded pair differs by platform, because the client TYPE does.** A
// Desktop-type client only accepts `http://localhost` redirects; an https
// redirect requires a **Web application** client. Android's redirect is https
// by necessity (see `redirect_uri`), so Android cannot share the Desktop
// client no matter how convenient that would be — this is a Google-side
// constraint on the client, not a property of the device.
//
// BYO credentials still work everywhere, including Android: the override is
// consulted before the platform split. An Android user supplying their own
// client must register `ANDROID_REDIRECT_URI` on it — the domain is Jodd's,
// but a redirect URI is just a string their own client can authorize.
pub fn client_id() -> String {
    let configured = crate::oauth_config::load().map(|c| c.client_id);
    client_id_from(configured.as_deref())
}

/// [`client_id`] with the configured tier already in hand.
///
/// Exists so `get_oauth_config`, which needs both the stored id (to show in
/// Settings) and the resolved id (to decide whether sign-in is possible), can
/// read `google_oauth.json` once instead of twice.
///
/// Passing the stored value straight through as the answer would be wrong: a
/// user running on embedded credentials has nothing configured, and would have
/// had the sign-in button disabled.
pub fn client_id_from(configured: Option<&str>) -> String {
    #[cfg(target_os = "android")]
    let (embedded, var) = (
        option_env!("GOOGLE_CLIENT_ID_ANDROID"),
        "GOOGLE_CLIENT_ID_ANDROID",
    );
    #[cfg(not(target_os = "android"))]
    let (embedded, var) = (option_env!("GOOGLE_CLIENT_ID"), "GOOGLE_CLIENT_ID");
    let runtime = std::env::var(var).ok();
    pick_credential(configured, embedded, runtime.as_deref())
}

// Google's flow requires both client_secret AND the PKCE verifier for token
// exchange. Google documents the Desktop client's secret as not actually
// secret — it is embeddable in distributed binaries by design — and a Web
// client's secret is no better protected once it ships inside an APK. PKCE is
// what actually protects the exchange, via the per-flow verifier.
//
// Paired with `client_id` above: same platform split, same reason.
pub fn client_secret() -> String {
    let configured = crate::oauth_config::load_secret();
    client_secret_from(configured.as_deref())
}

/// [`client_secret`] with the configured tier already in hand.
///
/// Exists so `get_oauth_config`, which needs both "is a secret stored?" and "is
/// a secret available at all?", can read the `oauth_client_secret::google`
/// keychain entry once instead of twice per call — and `AuthScreen` invokes that
/// command on mount.
///
/// Note the asymmetry with [`client_id_from`]: the configured secret lives in
/// the **keychain** while the configured id lives in a **file**, because a
/// secret belongs in the credential store and an id does not.
/// `oauth_config::load_secret` filters empties on the way out, so the blank
/// handling in `pick_credential` is redundant for this caller and load-bearing
/// for the other.
pub fn client_secret_from(configured: Option<&str>) -> String {
    #[cfg(target_os = "android")]
    let (embedded, var) = (
        option_env!("GOOGLE_CLIENT_SECRET_ANDROID"),
        "GOOGLE_CLIENT_SECRET_ANDROID",
    );
    #[cfg(not(target_os = "android"))]
    let (embedded, var) = (option_env!("GOOGLE_CLIENT_SECRET"), "GOOGLE_CLIENT_SECRET");
    let runtime = std::env::var(var).ok();
    pick_credential(configured, embedded, runtime.as_deref())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TokenData {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

// ─── PKCE ────────────────────────────────────────────────────────────────────
// RFC 7636. The verifier is high-entropy randomness held privately by the
// client across one auth flow. The challenge (sha256(verifier), base64url-no-pad)
// goes out in the auth URL. On token exchange, sending the verifier proves we
// are the same client that started the flow — without ever transmitting a
// long-lived shared secret. This is the recommended OAuth pattern for desktop
// and mobile apps where `client_secret` cannot truly be kept secret.

const VERIFIER_CHARSET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

// Serialize/Deserialize so `secrets::save_pending_signin` can persist this
// through the keychain — see that function for why an in-memory-only
// `pending_pkce` slot is dead by construction on Android cold launch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
    // OAuth `state` parameter (RFC 6749 §10.12) — CSRF protection. Bound to
    // this flow's PKCE pair so the two live and die together: the callback's
    // `state` query param must equal this value before we'll exchange the code.
    pub state: String,
}

impl PkcePair {
    pub fn generate() -> Self {
        // 64 chars from the RFC 7636 unreserved-URL set; well within the
        // 43–128 range the spec allows. ~380 bits of entropy.
        let mut rng = rand::thread_rng();
        let dist = rand::distributions::Uniform::from(0..VERIFIER_CHARSET.len());
        let verifier: String = (0..64)
            .map(|_| VERIFIER_CHARSET[dist.sample(&mut rng)] as char)
            .collect();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        // 32 chars from the same URL-safe charset — ~190 bits, well past any
        // brute-force window for a single in-flight auth request.
        let state: String = (0..32)
            .map(|_| VERIFIER_CHARSET[dist.sample(&mut rng)] as char)
            .collect();
        PkcePair { verifier, challenge, state }
    }
}

// ─── Auth URL ────────────────────────────────────────────────────────────────

pub fn get_auth_url(pkce: &PkcePair) -> String {
    format!(
        "{auth}\
        ?client_id={cid}\
        &redirect_uri={uri}\
        &response_type=code\
        &scope={scope}\
        &access_type=offline\
        &prompt=consent\
        &state={state}\
        &code_challenge={chall}\
        &code_challenge_method=S256",
        auth = AUTH_URL,
        cid = client_id(),
        uri = urlencoding::encode(redirect_uri()),
        scope = urlencoding::encode(SCOPES),
        state = urlencoding::encode(&pkce.state),
        chall = pkce.challenge,
    )
}

// ─── Token exchange (initial sign-in) ────────────────────────────────────────

/// Form parameters for the authorization-code exchange. `secret` is `None` on
/// Android — the key must be ABSENT, not empty, or Google rejects the request.
pub fn exchange_params<'a>(
    code: &'a str,
    verifier: &'a str,
    client_id: &'a str,
    secret: Option<&'a str>,
    redirect: &'a str,
) -> Vec<(&'a str, &'a str)> {
    let mut p = vec![
        ("code", code),
        ("client_id", client_id),
        ("code_verifier", verifier),
        ("redirect_uri", redirect),
        ("grant_type", "authorization_code"),
    ];
    if let Some(s) = secret {
        p.push(("client_secret", s));
    }
    p
}

/// Form parameters for a refresh-token grant. Same `None` rule as above.
pub fn refresh_params<'a>(
    refresh_token: &'a str,
    client_id: &'a str,
    secret: Option<&'a str>,
) -> Vec<(&'a str, &'a str)> {
    let mut p = vec![
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
    ];
    if let Some(s) = secret {
        p.push(("client_secret", s));
    }
    p
}

pub async fn exchange_code(code: &str, verifier: &str) -> Result<TokenData, String> {
    let cid = client_id();
    let csec = client_secret_opt();
    let redirect = redirect_uri();
    exchange_code_at(TOKEN_URL, code, verifier, cid.as_str(), csec.as_deref(), redirect, &[]).await
}

/// Provider-neutral authorization-code exchange. Same rationale as
/// [`refresh_access_token_at`]: one POST, one error format, both providers.
#[allow(clippy::too_many_arguments)]
pub async fn exchange_code_at(
    token_url: &str,
    code: &str,
    verifier: &str,
    client_id: &str,
    secret: Option<&str>,
    redirect: &str,
    extra: &[(&str, &str)],
) -> Result<TokenData, String> {
    let client = reqwest::Client::new();
    let mut params = exchange_params(code, verifier, client_id, secret, redirect);
    params.extend_from_slice(extra);
    let res = client
        .post(token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let body = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("token exchange failed: {} — {}", status, body));
    }
    serde_json::from_str(&body).map_err(|e| format!("token parse: {} — body: {}", e, body))
}

// ─── Refresh ─────────────────────────────────────────────────────────────────
// Google's docs state Desktop clients send client_id only on refresh when
// using PKCE for initial auth. The refresh token is the long-lived credential
// here — PKCE protects only the initial code→token exchange.

pub async fn refresh_access_token(refresh_token: &str) -> Result<TokenData, String> {
    let cid = client_id();
    let csec = client_secret_opt();
    refresh_access_token_at(TOKEN_URL, refresh_token, cid.as_str(), csec.as_deref(), &[]).await
}

/// Provider-neutral refresh-token grant. Google is the only caller with a
/// secret; Microsoft is a public client and passes `None` (see `auth_ms`).
///
/// **The error strings are the point of sharing this.** `is_unauthorized_error`
/// in lib.rs classifies refresh/API failures by substring, and the re-auth path
/// hangs off that classification. A second hand-rolled POST for Microsoft would
/// have drifted in wording the first time either side was edited, and the only
/// symptom would be a Microsoft account that silently stops recovering from a
/// revoked token. One implementation, one format, both providers.
///
/// `extra` carries provider-specific form fields — Microsoft's v2.0 endpoint
/// documents `scope` as required on this grant.
pub async fn refresh_access_token_at(
    token_url: &str,
    refresh_token: &str,
    client_id: &str,
    secret: Option<&str>,
    extra: &[(&str, &str)],
) -> Result<TokenData, String> {
    let client = reqwest::Client::new();
    let mut params = refresh_params(refresh_token, client_id, secret);
    params.extend_from_slice(extra);
    let res = client
        .post(token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let body = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("refresh failed: {} — {}", status, body));
    }
    serde_json::from_str(&body).map_err(|e| format!("refresh parse: {} — body: {}", e, body))
}

// ─── OAuth callback server (desktop only) ────────────────────────────────────
//
// Not compiled on Android, where the code arrives as an App Links Intent
// (see `redirect_uri`). That is a security boundary, not just dead-code
// hygiene: this listener binds `0.0.0.0:8080`, so on a phone it would be an
// unauthenticated HTTP server reachable from every other app on the device and
// from anything sharing the Wi-Fi. It is tolerable on desktop only because the
// flow that needs it cannot work any other way.

#[cfg(not(target_os = "android"))]
#[derive(Debug)]
pub struct CallbackResult {
    pub code: String,
    pub state: String,
}

/// An authorization server that came back refusing, rather than not coming
/// back at all. Every field is reported verbatim; **this module deliberately
/// does not interpret them**, because the interpretation depends on which
/// provider was asked, and only the caller knows that (see
/// `lib.rs::signin_denial`).
///
/// `description` and `state` are `Option` because a real refusal arrives
/// without them. Measured 2026-08-17 against a non-admin user in an outside
/// Microsoft 365 tenant, whose whole redirect was:
///
/// ```text
/// ?error=access_denied&error_subcode=cancel&state=QljKk8On4aTpcb0O
/// ```
///
/// No `error_description`, no AADSTS code — **byte-identical in shape to the
/// same user simply pressing Cancel.** Do not add a classifier keyed on an
/// AADSTS code: Microsoft's docs show one in most examples and it is not
/// there in practice, so such a code would branch on a field that never
/// arrives.
#[cfg(not(target_os = "android"))]
#[derive(Debug, PartialEq, Eq)]
pub struct CallbackDenial {
    pub error: String,
    pub subcode: Option<String>,
    pub description: Option<String>,
    pub state: Option<String>,
}

/// What the browser handed back. `Err` from the functions returning this is
/// reserved for "we never heard anything usable" — a bind failure, a timeout,
/// a superseding sign-in. A refusal is an *outcome*, not an error: the flow
/// completed, the answer was no.
#[cfg(not(target_os = "android"))]
#[derive(Debug, PartialEq, Eq)]
pub enum CallbackOutcome {
    Code(CallbackResult),
    Denied(CallbackDenial),
}

// `CallbackResult` needs the same derives as the enum that now holds it, and
// gets them here rather than on the struct so the success path's definition
// stays about the success path.
#[cfg(not(target_os = "android"))]
impl PartialEq for CallbackResult {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code && self.state == other.state
    }
}
#[cfg(not(target_os = "android"))]
impl Eq for CallbackResult {}

/// Split an OAuth redirect's query string into "got a code" / "got a refusal".
///
/// Pure, so the whole matrix is testable without binding a socket — the
/// listener around it is not.
#[cfg(not(target_os = "android"))]
fn parse_callback_query(query: &str) -> Result<CallbackOutcome, String> {
    let raw = |key: &str| -> Option<&str> {
        query
            .split('&')
            .find_map(|p| p.strip_prefix(format!("{key}=").as_str()))
    };
    // Opaque values (`code`, `state`): percent-decode only. A `+` here is a
    // literal `+` — Google's authorization codes really do contain them,
    // arriving as `%2B`, so translating `+` to a space would corrupt the code
    // and the exchange would fail with nothing to point at.
    let opaque = |key: &str| -> Option<String> {
        raw(key).map(|v| {
            urlencoding::decode(v)
                .map(|s| s.into_owned())
                .unwrap_or_else(|_| v.to_string())
        })
    };
    // Prose shown to a human (`error_description`): `+` IS a space, per the
    // form-encoding the OAuth error response uses. Substituted before
    // decoding, so an escaped `%2B` still ends up a literal `+`.
    let prose = |key: &str| -> Option<String> {
        raw(key).map(|v| {
            let spaced = v.replace('+', " ");
            urlencoding::decode(&spaced)
                .map(|s| s.into_owned())
                .unwrap_or(spaced)
        })
    };

    // Checked before `code` deliberately: an authorization server that is
    // refusing has no business also handing out a code, so if a response
    // somehow carries both, refuse rather than exchange it.
    if let Some(error) = opaque("error") {
        return Ok(CallbackOutcome::Denied(CallbackDenial {
            error,
            subcode: opaque("error_subcode"),
            description: prose("error_description"),
            state: opaque("state"),
        }));
    }
    let code = opaque("code").ok_or("No code in callback URL")?;
    // Without state there is no CSRF check to run, so this stays an error
    // rather than becoming a denial the UI would explain away as a policy
    // decision by the user's organisation.
    let state = opaque("state").ok_or("No state in callback URL")?;
    Ok(CallbackOutcome::Code(CallbackResult { code, state }))
}

/// How long an abandoned sign-in keeps port 8080. Long enough for a user who
/// tabs away mid-consent, short enough that a forgotten flow does not block
/// the next attempt for the life of the process.
#[cfg(not(target_os = "android"))]
const CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Poll interval. `Server::recv()` blocks forever, which is what stranded the
/// port; `recv_timeout` lets the loop notice cancellation and the deadline.
#[cfg(not(target_os = "android"))]
const CALLBACK_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// Block on the loopback listener until the browser redirects back, the caller
/// cancels, or [`CALLBACK_TIMEOUT`] passes. **Blocking — call from
/// `spawn_blocking`, not from an async task.**
///
/// The listener used to be `Server::http(..)` followed by a bare `recv()`,
/// which never returns if the user abandons the flow. The server then stayed
/// bound for the life of the process, so every later sign-in died on
/// "Address already in use (os error 98)" — the app could only ever
/// authenticate once per launch. Desktop hid this because one attempt usually
/// succeeds; on Android, where the first attempt picked the wrong Google
/// account and needed retrying, it made the app unusable.
///
/// Returning from this function drops `server`, which releases the port. Every
/// exit path below therefore has to be a return, not a `continue` that could
/// spin forever.
#[cfg(not(target_os = "android"))]
pub fn wait_for_callback_blocking(
    port: u16,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<CallbackOutcome, String> {
    let server = tiny_http::Server::http(format!("0.0.0.0:{port}")).map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + CALLBACK_TIMEOUT;

    let request = loop {
        if cancel.is_cancelled() {
            return Err("sign-in superseded by a newer attempt".to_string());
        }
        if std::time::Instant::now() >= deadline {
            return Err("timed out waiting for the browser to redirect back".to_string());
        }
        match server.recv_timeout(CALLBACK_POLL) {
            Ok(Some(req)) => break req,
            Ok(None) => continue,
            Err(e) => return Err(e.to_string()),
        }
    };
    let url = request.url().to_string();
    let outcome = parse_callback_query(url.split('?').nth(1).unwrap_or(""));

    // Answer the browser on EVERY path, not just the happy one. The old code
    // returned before responding whenever a parameter was missing, which is
    // exactly what a refusal looks like — so a user who was told "Need admin
    // approval" and clicked back to the app was left staring at a hung, blank
    // tab. The page deliberately does not try to explain the refusal: the app
    // knows which provider was asked and what to suggest, and this page does
    // not.
    let body = match &outcome {
        Ok(CallbackOutcome::Code(_)) => {
            "<h2>✅ Jodd Connected!</h2>\
             <p>You can close this tab and return to the app.</p>"
        }
        _ => {
            "<h2>Sign-in was not completed</h2>\
             <p>You can close this tab and return to Jodd, which explains what to do next.</p>"
        }
    };
    let response = tiny_http::Response::from_string(format!(
        "<html><head><meta charset='utf-8'></head>\
        <body style='font-family:sans-serif;text-align:center;padding:60px'>\
        {body}</body></html>"
    ))
    .with_header(
        // Declare charset so the browser decodes the ✅ glyph as UTF-8, not Latin-1.
        "Content-Type: text/html; charset=utf-8"
            .parse::<tiny_http::Header>()
            .unwrap(),
    );
    let _ = request.respond(response);

    outcome
}

#[cfg(test)]
mod oauth_param_tests {
    use super::*;

    fn keys(params: &[(&str, &str)]) -> Vec<String> {
        params.iter().map(|(k, _)| k.to_string()).collect()
    }

    // ── Callback query parsing ──────────────────────────────────────────────

    /// The real thing, captured 2026-08-17 from `jodd@renny.co.th` — a
    /// non-admin user in an outside Microsoft 365 tenant — after Microsoft
    /// showed "Need admin approval" and the user took the only route it
    /// offers back to the app, "Return to the application without granting
    /// consent". This is the single most load-bearing string in this file:
    /// every simplification of the denial path has to keep working on it.
    #[cfg(not(target_os = "android"))]
    const MEASURED_ADMIN_CONSENT_DENIAL: &str =
        "error=access_denied&error_subcode=cancel&state=QljKk8On4aTpcb0O";

    #[cfg(not(target_os = "android"))]
    fn denial(query: &str) -> CallbackDenial {
        match parse_callback_query(query) {
            Ok(CallbackOutcome::Denied(d)) => d,
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn the_measured_admin_consent_refusal_parses_as_a_denial() {
        assert_eq!(
            denial(MEASURED_ADMIN_CONSENT_DENIAL),
            CallbackDenial {
                error: "access_denied".to_string(),
                subcode: Some("cancel".to_string()),
                // Both absent in the real capture. If a future change starts
                // asserting `description.is_some()`, it is testing a fiction.
                description: None,
                state: Some("QljKk8On4aTpcb0O".to_string()),
            }
        );
    }

    /// The point of the whole feature, pinned as an assertion: a user who
    /// pressed Cancel and a user whose tenant blocked consent are the SAME
    /// bytes. Nothing downstream may claim to tell them apart.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn a_plain_cancel_is_indistinguishable_from_the_admin_consent_refusal() {
        let plain_cancel = "error=access_denied&error_subcode=cancel&state=QljKk8On4aTpcb0O";
        assert_eq!(denial(plain_cancel), denial(MEASURED_ADMIN_CONSENT_DENIAL));
    }

    /// Not every provider is as terse — Google and some Microsoft flows do
    /// send a description. Captured when present because it is the only
    /// provider-side detail a support conversation can use.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn a_description_is_captured_when_the_provider_sends_one() {
        let d = denial("error=access_denied&error_description=AADSTS65004%3a+User+declined+to+consent.&state=S1");
        // `+` is a space in a query string, and this field is prose shown to a
        // human — decoding it is display correctness, not pedantry.
        assert_eq!(
            d.description.as_deref(),
            Some("AADSTS65004: User declined to consent.")
        );
    }

    /// `+` must NOT be turned into a space in `code`: an authorization code is
    /// opaque and a Google one really can contain a literal `+` (arriving as
    /// `%2B`). The two fields decode differently on purpose.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn a_code_keeps_its_plus_signs_while_a_description_loses_them() {
        let out = parse_callback_query("code=A%2FB%2BC&state=S%3DT").unwrap();
        assert_eq!(
            out,
            CallbackOutcome::Code(CallbackResult {
                code: "A/B+C".to_string(),
                state: "S=T".to_string(),
            })
        );
    }

    /// `error` wins over `code` if both somehow appear. An authorization
    /// server that is refusing has no business handing out a code, so the
    /// conservative read is to refuse rather than exchange it.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn a_refusal_beats_a_code_when_a_response_carries_both() {
        assert_eq!(denial("code=ABC&error=access_denied&state=S1").error, "access_denied");
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn a_code_without_state_is_still_refused() {
        // No state, no CSRF check — this must not become a denial the UI
        // explains away as "your organisation blocked it".
        assert!(parse_callback_query("code=ABC123").is_err());
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn a_query_with_neither_code_nor_error_is_an_error() {
        assert!(parse_callback_query("").is_err());
        assert!(parse_callback_query("scope=openid").is_err());
    }

    #[test]
    fn exchange_params_include_the_secret_when_one_is_supplied() {
        let p = exchange_params("CODE", "VERIFIER", "CID", Some("SECRET"), "REDIR");
        assert!(keys(&p).contains(&"client_secret".to_string()));
        assert!(p.contains(&("client_secret", "SECRET")));
    }

    #[test]
    fn exchange_params_omit_the_secret_entirely_when_none() {
        // No client type Jodd currently uses is secret-less — Desktop and Web
        // both have one, and the Android type that did was retired (see
        // `redirect_uri`). The distinction is still worth pinning: a
        // user-supplied client via oauth_config may legitimately have no
        // secret, and sending an empty string is NOT equivalent to omitting
        // the key — Google rejects `client_secret=""`. `client_secret_opt()`
        // maps empty to `None` precisely so this branch is reachable.
        let p = exchange_params("CODE", "VERIFIER", "CID", None, "REDIR");
        assert!(!keys(&p).contains(&"client_secret".to_string()));
    }

    #[test]
    fn exchange_params_always_carry_the_pkce_verifier() {
        let p = exchange_params("CODE", "VERIFIER", "CID", None, "REDIR");
        assert!(p.contains(&("code_verifier", "VERIFIER")));
        assert!(p.contains(&("grant_type", "authorization_code")));
        assert!(p.contains(&("redirect_uri", "REDIR")));
    }

    #[test]
    fn refresh_params_omit_the_secret_when_none() {
        let p = refresh_params("RT", "CID", None);
        assert!(!keys(&p).contains(&"client_secret".to_string()));
        assert!(p.contains(&("grant_type", "refresh_token")));
        assert!(p.contains(&("refresh_token", "RT")));
    }

    #[test]
    fn refresh_params_include_the_secret_when_supplied() {
        let p = refresh_params("RT", "CID", Some("SECRET"));
        assert!(p.contains(&("client_secret", "SECRET")));
    }

    // Both values are pinned because neither is a free choice, and because
    // getting one wrong fails on a device rather than in CI — possibly days
    // later. Loopback is exempt from Google's HTTPS-only rule only while it
    // stays literally localhost; the Android URL has to match the manifest's
    // intent-filter and the Web client's authorized redirect exactly, so an
    // edit here that is not mirrored in both other places silently breaks
    // sign-in with no local symptom.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_redirect_uri_is_the_loopback_callback() {
        assert_eq!(redirect_uri(), "http://localhost:8080/callback");
    }

    #[cfg(target_os = "android")]
    #[test]
    fn android_redirect_uri_is_the_app_links_url() {
        assert_eq!(redirect_uri(), "https://jodd.bbmedia.co.th/oauth2redirect");
        // Google requires https for every non-loopback redirect, and App Links
        // will not verify a plain-http one either.
        assert!(redirect_uri().starts_with("https://"));
    }

    #[test]
    fn auth_url_embeds_the_platform_redirect_uri() {
        let pkce = PkcePair::generate();
        let url = get_auth_url(&pkce);
        assert!(url.contains(&urlencoding::encode(redirect_uri()).into_owned()));
        assert!(url.contains("code_challenge_method=S256"));
    }

    // A cancelled token alone cannot prove `port` is honoured: cancellation
    // is checked only *after* the bind, so this only pins that a cancelled
    // caller gets the cancellation message back, on whichever port happens
    // to be free. Pair it with the test below, which is the one that
    // actually fails if `port` is accepted but silently ignored.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn callback_listener_reports_cancellation_on_a_free_port() {
        use tokio_util::sync::CancellationToken;
        let cancel = CancellationToken::new();
        cancel.cancel(); // return immediately; we only care that binding succeeded
        let err = wait_for_callback_blocking(18080, &cancel).unwrap_err();
        assert!(err.contains("superseded"), "cancelled flow should report cancellation, got: {err}");
    }

    // Proves the `port` argument is the one actually bound. We occupy an
    // OS-assigned free port ourselves with a plain `TcpListener`, then hand
    // that exact port to `wait_for_callback_blocking` with a token that is
    // deliberately left un-cancelled. A correct implementation tries to bind
    // that same port and fails immediately (EADDRINUSE), so the call returns
    // right away. If `port` were accepted but silently ignored — the bind
    // still hardcoded to 8080 — the call would instead succeed binding 8080
    // (assuming it's free) and then block in the poll loop for up to
    // `CALLBACK_TIMEOUT` (5 minutes), since nothing here ever cancels it or
    // sends it a request. We therefore bound how long we wait for a result,
    // not just check the error text — a "timed out waiting…" error after 5
    // minutes would also fail to contain "superseded" and could otherwise
    // slip past a text-only assertion.
    /// The browser must be ANSWERED on the refusal path, not just parsed.
    ///
    /// Before 2026-08-17 this path returned before `request.respond(..)`, so a
    /// user who clicked "Return to the application without granting consent"
    /// watched a tab hang and then fail — while Jodd, having read the request
    /// perfectly well, showed nothing. Two separate silences from one missing
    /// line.
    ///
    /// Drives a real listener over a real socket because that missing line is
    /// invisible to `parse_callback_query`'s tests: the parse was never the
    /// broken half.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn the_browser_is_answered_on_the_refusal_path() {
        use std::io::{Read, Write};
        use tokio_util::sync::CancellationToken;

        // Take a free port, then release it so the listener can claim it.
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
            probe.local_addr().unwrap().port()
        };

        let listener = std::thread::spawn(move || {
            let cancel = CancellationToken::new(); // never cancelled
            wait_for_callback_blocking(port, &cancel)
        });

        // The listener binds on its own thread, so connect with retries rather
        // than a sleep — a fixed sleep is either flaky or slow, and this test
        // exists to be trusted.
        let mut stream = None;
        for _ in 0..100 {
            match std::net::TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
        let mut stream = stream.expect("listener never came up");
        // Exactly what Microsoft's browser redirect sends — the string measured
        // on 2026-08-17, verbatim.
        stream
            .write_all(concat!(
                "GET /?error=access_denied&error_subcode=cancel&state=QljKk8On4aTpcb0O HTTP/1.1\r\n",
                "Host: localhost\r\nConnection: close\r\n\r\n"
            ).as_bytes())
            .expect("send the redirect");

        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read the reply");

        assert!(
            response.contains("Sign-in was not completed"),
            "the tab must be told the flow is over, got:\n{response}"
        );
        assert!(
            !response.contains("Jodd Connected"),
            "a refusal must never render the success page:\n{response}"
        );

        let outcome = listener.join().expect("listener thread panicked");
        assert!(
            matches!(outcome, Ok(CallbackOutcome::Denied(_))),
            "got {outcome:?}"
        );
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn callback_listener_binds_the_port_it_is_given() {
        use std::sync::mpsc;
        use tokio_util::sync::CancellationToken;

        let occupied =
            std::net::TcpListener::bind("0.0.0.0:0").expect("bind an OS-assigned free port");
        let taken_port = occupied.local_addr().unwrap().port();

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let cancel = CancellationToken::new(); // deliberately left un-cancelled
            let result = wait_for_callback_blocking(taken_port, &cancel);
            drop(occupied); // keep the port held for the whole call attempt
            let _ = tx.send(result);
        });

        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect(
                "bind on an already-occupied port should fail immediately, not block toward \
                 the 5-minute callback timeout — this hanging is itself evidence `port` was \
                 ignored",
            );
        let err = result.expect_err("binding an already-occupied port must fail");
        assert!(
            !err.contains("superseded"),
            "should be a bind failure on the port we occupied, not a cancellation message: {err}"
        );
    }
}

#[cfg(test)]
mod credential_tier_tests {
    use super::*;

    // Mirrors `auth_ms::client_id_tiers_resolve_configured_then_runtime_then_embedded`.
    // Testing `pick_credential` rather than `client_id`/`client_secret` is what
    // makes all three tiers assertable at all: the real functions read a config
    // file, the keychain, and process environment, none of which a unit test can
    // vary without racing every other test in the process.

    #[test]
    fn credential_tiers_resolve_configured_then_embedded_then_runtime() {
        // Configured outranks everything — a user with their own Google Cloud
        // project must not be silently switched onto Jodd's registration.
        assert_eq!(pick_credential(Some("cfg"), Some("baked"), Some("env")), "cfg");
        // Embedded outranks the runtime var. THIS IS THE OPPOSITE of
        // `auth_ms::pick_client_id`, where runtime wins — see `pick_credential`
        // for why the two modules diverge. Correct here because the configured
        // tier above is where a BYO client belongs, leaving the env var as a
        // developer convenience rather than a supported override.
        assert_eq!(pick_credential(None, Some("baked"), Some("env")), "baked");
        // Runtime env is the last resort — the dev path when nothing was baked
        // in at build time (no .env present during `tauri build`).
        assert_eq!(pick_credential(None, None, Some("env")), "env");
        // Blank is absent, not an override: a config file hand-edited to empty,
        // or an exported-but-empty var in a shell or CI step, must not blank out
        // a working credential further down.
        assert_eq!(pick_credential(Some(""), Some("   "), Some("env")), "env");
        assert_eq!(pick_credential(Some(" \t\n"), Some("baked"), None), "baked");
        // Values come back trimmed, so a credential pasted with a trailing
        // newline still authenticates instead of failing at Google with a
        // useless error.
        assert_eq!(pick_credential(Some(" cfg\n"), None, None), "cfg");
        // Nothing anywhere: empty, so the missing-client-id refusal names the
        // variable rather than letting Google answer with an OAuth error page.
        assert_eq!(pick_credential(None, None, None), "");
        assert_eq!(pick_credential(Some(""), Some(""), Some("")), "");
    }

    #[test]
    fn the_from_variants_accept_a_configured_value_without_touching_storage() {
        // What `get_oauth_config` relies on: pass the configured tier in and it
        // wins, with no config-file or keychain access of its own.
        assert_eq!(client_id_from(Some("my-client-id")), "my-client-id");
        assert_eq!(client_secret_from(Some("my-secret")), "my-secret");
    }
}
