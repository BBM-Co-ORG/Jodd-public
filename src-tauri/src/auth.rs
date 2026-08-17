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

// Tier 2+3: compile-time env (baked in by build.rs from CI secrets or a local
// .env at build time) → runtime env (dev convenience). Release binaries get the
// compile-time value so no .env is needed at the user's install location.
fn embedded_or_runtime(name: &str, compile_time: Option<&'static str>) -> String {
    compile_time
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| std::env::var(name).unwrap_or_default())
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
    if let Some(cfg) = crate::oauth_config::load() {
        if !cfg.client_id.is_empty() {
            return cfg.client_id;
        }
    }
    #[cfg(target_os = "android")]
    {
        embedded_or_runtime(
            "GOOGLE_CLIENT_ID_ANDROID",
            option_env!("GOOGLE_CLIENT_ID_ANDROID"),
        )
    }
    #[cfg(not(target_os = "android"))]
    {
        embedded_or_runtime("GOOGLE_CLIENT_ID", option_env!("GOOGLE_CLIENT_ID"))
    }
}

// Google's flow requires both client_secret AND the PKCE verifier for token
// exchange. Google documents the Desktop client's secret as not actually
// secret — it is embeddable in distributed binaries by design — and a Web
// client's secret is no better protected once it ships inside an APK. PKCE is
// what actually protects the exchange, via the per-flow verifier.
//
// Paired with `client_id` above: same platform split, same reason.
pub fn client_secret() -> String {
    if let Some(s) = crate::oauth_config::load_secret() {
        return s;
    }
    #[cfg(target_os = "android")]
    {
        embedded_or_runtime(
            "GOOGLE_CLIENT_SECRET_ANDROID",
            option_env!("GOOGLE_CLIENT_SECRET_ANDROID"),
        )
    }
    #[cfg(not(target_os = "android"))]
    {
        embedded_or_runtime("GOOGLE_CLIENT_SECRET", option_env!("GOOGLE_CLIENT_SECRET"))
    }
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

// Serialize/Deserialize so `secrets::save_pending_pkce` can persist this
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
) -> Result<CallbackResult, String> {
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

    let query = url.split('?').nth(1).unwrap_or("");
    let param = |key: &str| -> Option<String> {
        query
            .split('&')
            .find(|p| p.starts_with(&format!("{}=", key)))
            .and_then(|p| p.split_once('='))
            .map(|(_, v)| urlencoding::decode(v).map(|s| s.into_owned()).unwrap_or_else(|_| v.to_string()))
    };
    let code = param("code").ok_or("No code in callback URL")?;
    let state = param("state").ok_or("No state in callback URL")?;

    let response = tiny_http::Response::from_string(
        "<html><head><meta charset='utf-8'></head>\
        <body style='font-family:sans-serif;text-align:center;padding:60px'>\
        <h2>✅ Jodd Connected!</h2>\
        <p>You can close this tab and return to the app.</p>\
        </body></html>",
    )
    .with_header(
        // Declare charset so the browser decodes the ✅ glyph as UTF-8, not Latin-1.
        "Content-Type: text/html; charset=utf-8"
            .parse::<tiny_http::Header>()
            .unwrap(),
    );
    let _ = request.respond(response);

    Ok(CallbackResult { code, state })
}

#[cfg(test)]
mod oauth_param_tests {
    use super::*;

    fn keys(params: &[(&str, &str)]) -> Vec<String> {
        params.iter().map(|(k, _)| k.to_string()).collect()
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
