//! Microsoft identity platform OAuth — public client, PKCE, loopback redirect.
//!
//! Deliberately separate from `auth.rs`: that module is hardwired to Google
//! through module-level constants. The provider-neutral parts (`PkcePair`,
//! `wait_for_callback_blocking`) are reused from it rather than duplicated.
//! Merge the two only if real duplication shows up — not on prediction.
//!
//! Values verified live on 2026-08-14 against both a personal @live.com
//! account and a Microsoft 365 work account.

const AUTH_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize";
pub const TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

/// `offline_access` is what makes a refresh token come back; without it the
/// user re-authenticates every hour and background sync is impossible.
pub const SCOPES: &str = "Mail.ReadWrite offline_access User.Read";

/// Must exactly match a Redirect URI registered on the Azure app under the
/// "Mobile and desktop applications" platform. 8080 is shared with the Gmail
/// flow — only one sign-in runs at a time.
pub fn redirect_uri() -> &'static str {
    "http://localhost:8080"
}

/// No secret counterpart exists. This is a public client; `Allow public client
/// flows` must be enabled on the registration or the token exchange is refused.
pub fn client_id() -> String {
    std::env::var("MS_CLIENT_ID").unwrap_or_default()
}

pub fn get_auth_url(pkce: &crate::auth::PkcePair) -> String {
    format!(
        "{auth}\
        ?client_id={cid}\
        &response_type=code\
        &redirect_uri={uri}\
        &response_mode=query\
        &scope={scope}\
        &state={state}\
        &code_challenge={chall}\
        &code_challenge_method=S256\
        &prompt=select_account",
        auth = AUTH_URL,
        cid = client_id(),
        uri = urlencoding::encode(redirect_uri()),
        scope = urlencoding::encode(SCOPES),
        state = urlencoding::encode(&pkce.state),
        chall = pkce.challenge,
    )
}

/// Microsoft's v2.0 endpoint documents `scope` as required on both the
/// authorization-code and refresh-token grants, and requires it to be
/// equivalent to (or a subset of) what the original authorization asked for —
/// so both grants send exactly [`SCOPES`].
fn scope_param() -> [(&'static str, &'static str); 1] {
    [("scope", SCOPES)]
}

/// Exchange the authorization code for tokens. Public client: **no secret**.
/// `Allow public client flows` must be enabled on the Azure registration or
/// this is refused.
pub async fn exchange_code(code: &str, verifier: &str) -> Result<crate::auth::TokenData, String> {
    let cid = client_id();
    crate::auth::exchange_code_at(
        TOKEN_URL,
        code,
        verifier,
        cid.as_str(),
        None,
        redirect_uri(),
        &scope_param(),
    )
    .await
}

/// Refresh an access token. Deliberately routed through the shared
/// `auth::refresh_access_token_at` so the failure strings — and therefore
/// `is_unauthorized_error`'s classification and the re-auth path behind it —
/// are byte-identical to Gmail's.
pub async fn refresh_access_token(refresh_token: &str) -> Result<crate::auth::TokenData, String> {
    let cid = client_id();
    crate::auth::refresh_access_token_at(
        TOKEN_URL,
        refresh_token,
        cid.as_str(),
        None,
        &scope_param(),
    )
    .await
}

/// The signed-in identity, used as the Jodd `account_id` (accounts are keyed by
/// email address, immutably).
///
/// `mail` is absent on plenty of real accounts — it is null unless the mailbox
/// has a primary SMTP address surfaced to Graph — so `userPrincipalName` is the
/// fallback, which is what the `User.Read` scope guarantees. Verified live on
/// 2026-08-14: `GET /me` → 200 for both a personal `@live.com` account and a
/// Microsoft 365 work account.
pub fn email_from_me_response(json: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("/me parse: {} — body: {}", e, json))?;
    for key in ["mail", "userPrincipalName"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return Ok(s.to_string());
            }
        }
    }
    Err(format!("/me carried neither mail nor userPrincipalName — body: {json}"))
}

pub async fn get_user_email(access_token: &str) -> Result<String, String> {
    let res = reqwest::Client::new()
        .get(format!(
            "{}/me?$select=mail,userPrincipalName",
            crate::backend::microsoft::wire::GRAPH_BASE
        ))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let body = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        // Same " 401" shape `is_unauthorized_error` looks for.
        return Err(format!("get user profile failed: {} — {}", status, body));
    }
    email_from_me_response(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::PkcePair;

    #[test]
    fn auth_url_carries_pkce_and_the_verified_oauth_parameters() {
        let pkce = PkcePair::generate();
        let url = get_auth_url(&pkce);

        assert!(url.starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize?"),
            "must use the `common` tenant — verified to accept both personal and work accounts");
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge)));
        assert!(url.contains(&urlencoding::encode(&pkce.state).to_string()));
        assert!(url.contains(&urlencoding::encode("http://localhost:8080").to_string()));
        assert!(url.contains(&urlencoding::encode("Mail.ReadWrite offline_access User.Read").to_string()));
        assert!(!url.contains("client_secret"), "public client — no secret may ever appear");
    }

    #[test]
    fn both_grants_carry_the_scope_microsofts_v2_endpoint_requires() {
        assert_eq!(scope_param()[0], ("scope", SCOPES));
    }

    #[test]
    fn the_identity_prefers_mail_and_falls_back_to_the_upn() {
        assert_eq!(
            email_from_me_response(r#"{"mail":"kaiwan.h@live.com","userPrincipalName":"other@x"}"#).unwrap(),
            "kaiwan.h@live.com"
        );
        // `mail` is null on plenty of real mailboxes — the UPN is what
        // `User.Read` actually guarantees.
        assert_eq!(
            email_from_me_response(r#"{"mail":null,"userPrincipalName":"kaiwan@bbmedia.in.th"}"#).unwrap(),
            "kaiwan@bbmedia.in.th"
        );
        assert!(email_from_me_response(r#"{"mail":"","userPrincipalName":""}"#).is_err());
        assert!(email_from_me_response("not json").is_err());
    }
}
