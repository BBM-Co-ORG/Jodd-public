use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::distributions::Distribution;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// Google OAuth 2.0 endpoints.
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub const REDIRECT_URI: &str = "http://localhost:8080/callback";

// `gmail.modify` (sensitive scope, free verification) instead of the older
// `https://mail.google.com/` (restricted scope, requires $15k+ CASA assessment).
// Modify gives us read + insert + delete + label-modify, which covers everything
// Jodd needs.
const SCOPES: &str = "https://www.googleapis.com/auth/gmail.modify";

// Resolution order: compile-time env (baked in by build.rs from CI secrets or
// a local .env at build time) → runtime env (dev convenience with dotenv
// loaded in lib.rs). Release binaries get the compile-time value, so no .env
// is needed at the user's install location.
fn embedded_or_runtime(name: &str, compile_time: Option<&'static str>) -> String {
    compile_time
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| std::env::var(name).unwrap_or_default())
}

pub fn client_id() -> String {
    embedded_or_runtime("GOOGLE_CLIENT_ID", option_env!("GOOGLE_CLIENT_ID"))
}

// Google's Desktop OAuth flow requires both client_secret AND the PKCE verifier
// for token exchange — see https://developers.google.com/identity/protocols/oauth2/native-app
// The secret is embedded in the binary and explicitly documented as not actually
// secret for Desktop clients; PKCE provides the additional per-flow protection.
pub fn client_secret() -> String {
    embedded_or_runtime("GOOGLE_CLIENT_SECRET", option_env!("GOOGLE_CLIENT_SECRET"))
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

#[derive(Clone, Debug)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
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
        PkcePair { verifier, challenge }
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
        &code_challenge={chall}\
        &code_challenge_method=S256",
        auth = AUTH_URL,
        cid = client_id(),
        uri = urlencoding::encode(REDIRECT_URI),
        scope = urlencoding::encode(SCOPES),
        chall = pkce.challenge,
    )
}

// ─── Token exchange (initial sign-in) ────────────────────────────────────────

pub async fn exchange_code(code: &str, verifier: &str) -> Result<TokenData, String> {
    let client = reqwest::Client::new();
    let cid = client_id();
    let csec = client_secret();
    let params = [
        ("code", code),
        ("client_id", cid.as_str()),
        ("client_secret", csec.as_str()), // Google requires this even with PKCE
        ("code_verifier", verifier),       // PKCE provides per-flow protection
        ("redirect_uri", REDIRECT_URI),
        ("grant_type", "authorization_code"),
    ];
    let res = client
        .post(TOKEN_URL)
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
    let client = reqwest::Client::new();
    let cid = client_id();
    let csec = client_secret();
    let params = [
        ("refresh_token", refresh_token),
        ("client_id", cid.as_str()),
        ("client_secret", csec.as_str()), // Google requires this for Desktop clients
        ("grant_type", "refresh_token"),
    ];
    let res = client
        .post(TOKEN_URL)
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

// ─── OAuth callback server ───────────────────────────────────────────────────

pub async fn wait_for_callback() -> Result<String, String> {
    let server = tiny_http::Server::http("0.0.0.0:8080").map_err(|e| e.to_string())?;

    let request = server.recv().map_err(|e| e.to_string())?;
    let url = request.url().to_string();

    let code = url
        .split('?')
        .nth(1)
        .and_then(|q| q.split('&').find(|p| p.starts_with("code=")))
        .and_then(|p| p.strip_prefix("code="))
        .map(|c| c.to_string())
        .ok_or("No code in callback URL")?;

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

    Ok(code)
}
