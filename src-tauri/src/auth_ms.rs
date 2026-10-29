//! Microsoft identity platform OAuth — public client, PKCE, loopback redirect
//! on desktop and the App Links redirect on Android.
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
/// "Mobile and desktop applications" platform — BOTH values below are, under
/// that one platform. That is what lets one `MS_CLIENT_ID` serve every
/// platform: Azure keys redirect URIs per platform inside a registration,
/// where Google keys them per client TYPE and forces Android onto a second
/// client (`GOOGLE_CLIENT_ID_ANDROID`). Registering the https URI under a
/// "Web" platform instead would turn the code exchange confidential and
/// have it demand a secret this public client has none of (`AADSTS7000218`).
///
/// Desktop: 8080 is shared with the Gmail flow — only one sign-in runs at a
/// time. Android: the same App Links URL Gmail uses, for the same reason —
/// the OS may kill Jodd while the user is on the consent page, and a
/// loopback listener in a dead process hears nothing (gotcha #8). The app
/// tells the two providers apart by `pending_backend`, not by the URL.
#[cfg(not(target_os = "android"))]
pub fn redirect_uri() -> &'static str {
    "http://localhost:8080"
}

#[cfg(target_os = "android")]
pub fn redirect_uri() -> &'static str {
    crate::auth::ANDROID_REDIRECT_URI
}

/// No secret counterpart exists. This is a public client; `Allow public client
/// flows` must be enabled on the registration or the token exchange is refused.
///
/// Three tiers, matching Gmail's shape: user-configured in App Settings →
/// runtime `MS_CLIENT_ID` → the value `build.rs` embedded at compile time.
/// Before 2026-08-17 only the middle one existed, so release builds shipped
/// with no Microsoft client id at all and refused every Microsoft sign-in at
/// [`crate::refuse_missing_client_id`].
///
/// One client id serves **every** Microsoft account in the install, exactly as
/// one Google client serves every Gmail account. That is a real limit, not just
/// a simplification: an organisation whose IT registers Jodd inside its own
/// tenant produces a *single-tenant* client id, and setting that here would
/// break a personal `@outlook.com` account in the same install. Per-account
/// credentials are the fix and are deliberately not built yet.
///
/// **The env var sits ABOVE the embedded value, which is the one place this
/// does not mirror `auth::embedded_or_runtime`.** Gmail can afford env-as-
/// fallback because it has always had a Settings tier above both. `MS_CLIENT_ID`
/// was the *only* tier through 0.24.1 and the public README instructs users to
/// bring their own registration that way, so demoting it below an embedded id
/// would not error — it would silently run those users on Jodd's registration
/// instead of their own, losing the consent grants and tenant approvals they
/// arranged against their app. Once the Settings field has shipped for a
/// release or two, this can collapse into Gmail's exact order.
pub fn client_id() -> String {
    pick_client_id(
        crate::oauth_config::load_ms_client_id().as_deref(),
        std::env::var("MS_CLIENT_ID").ok().as_deref(),
        option_env!("MS_CLIENT_ID"),
    )
}

/// First non-blank of `configured`, `runtime`, then `embedded`. Split out from
/// [`client_id`] so the precedence is testable without mutating process
/// environment (which races across parallel tests) or writing a real config
/// file.
fn pick_client_id(
    configured: Option<&str>,
    runtime: Option<&str>,
    embedded: Option<&str>,
) -> String {
    [configured, runtime, embedded]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// Where a tenant administrator grants Jodd consent on behalf of their whole
/// organisation — the one thing a blocked user can actually forward to IT.
///
/// `organizations` rather than a tenant id: the refusal arrives *before* any
/// token, so Jodd never learns which tenant the user belongs to and has
/// nothing to substitute. `organizations` makes Microsoft resolve it against
/// whichever work/school account the admin signs in with. Per
/// learn.microsoft.com/entra/identity/enterprise-apps/grant-admin-consent,
/// "Construct the URL for granting tenant-wide admin consent".
///
/// **Measured end to end on 2026-08-18 — this link is the actual remedy, not
/// a plausible one.** An admin of an outside tenant (`admin@renny.co.th`)
/// opened it, granted consent for the organisation, and Microsoft answered
/// `?admin_consent=True&tenant=…`. The non-admin who had been refused
/// (`jodd@renny.co.th`) then signed in to Jodd normally: token exchange OK,
/// refresh token issued, account added.
///
/// Two things that run measured too, and neither is guessable:
///
/// - **Omitting `redirect_uri` does not produce a Microsoft confirmation
///   page.** Microsoft redirects to one of the app's *registered* redirect
///   URIs, and it chose `http://localhost:8765` — the M1 probe's, not the
///   `:8080` this vertical uses. Nothing listens there, so the admin sees a
///   browser error **after the grant is already recorded**. Hence the warning
///   in `adminRequestText` (src/lib/signInBlocked.ts): without it an admin
///   reports a success as a failure.
/// - The `unverified` badge and "This application is not published by
///   Microsoft or your organization" both appear on the admin's consent
///   screen. Neither blocks the grant.
pub fn admin_consent_url() -> String {
    // Empty `client_id` is unreachable here: `refuse_missing_client_id` in
    // lib.rs blocks the flow before the browser ever opens, so there is no
    // path to a refusal without one.
    admin_consent_url_for(&client_id())
}

/// Split out from [`admin_consent_url`] purely so the URL's shape is testable
/// without mutating process environment from a test thread.
fn admin_consent_url_for(client_id: &str) -> String {
    format!("https://login.microsoftonline.com/organizations/adminconsent?client_id={client_id}")
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
    fn admin_consent_url_targets_the_signing_in_users_own_tenant() {
        let url = admin_consent_url_for("CID-123");
        // `organizations`, never `common` and never a hardcoded tenant: this
        // link is handed to an admin in an organisation Jodd cannot name,
        // because the refusal that produces it arrives before any token.
        assert_eq!(
            url,
            "https://login.microsoftonline.com/organizations/adminconsent?client_id=CID-123"
        );
    }

    #[test]
    fn admin_consent_url_is_not_the_sign_in_url() {
        // Guards a plausible mix-up: `adminconsent` is a different endpoint
        // from `oauth2/v2.0/authorize`, and sending IT the latter just makes
        // the admin repeat the user's failed sign-in.
        let url = admin_consent_url_for("CID-123");
        assert!(!url.contains("authorize"), "wrong endpoint: {url}");
        assert!(!url.contains("/common/"), "must resolve the admin's own tenant: {url}");
    }

    #[test]
    fn client_id_tiers_resolve_configured_then_runtime_then_embedded() {
        // App Settings outranks everything — the bring-your-own-registration
        // path a downloaded build has to offer.
        assert_eq!(pick_client_id(Some("cfg"), Some("env"), Some("baked")), "cfg");
        // The env var outranks the embedded id. This is the deliberate
        // divergence from Gmail's order: it keeps the pre-Settings BYO
        // workflow (public README, 0.24.1) working instead of silently
        // switching those users onto Jodd's own registration.
        assert_eq!(pick_client_id(None, Some("env"), Some("baked")), "env");
        // Embedded is the last resort, and it is what makes a plain download
        // able to sign in at all.
        assert_eq!(pick_client_id(None, None, Some("baked")), "baked");
        // Blank is absent, not an override — a config file saved empty, or an
        // exported-but-empty var in a shell or CI step, must not blank out a
        // working id further down.
        assert_eq!(pick_client_id(Some(""), Some("   "), Some("baked")), "baked");
        assert_eq!(pick_client_id(Some(" \t\n"), None, Some("baked")), "baked");
        // Nothing anywhere: empty, so `refuse_missing_client_id` names the
        // variable instead of Microsoft answering AADSTS900144 in a browser.
        assert_eq!(pick_client_id(None, None, None), "");
        assert_eq!(pick_client_id(Some(""), Some(""), Some("")), "");
        // Surrounding whitespace is trimmed off a value that is otherwise real.
        assert_eq!(pick_client_id(Some(" cfg \n"), None, None), "cfg");
    }

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
        assert!(url.contains(&urlencoding::encode(redirect_uri()).to_string()));
        assert!(url.contains(&urlencoding::encode("Mail.ReadWrite offline_access User.Read").to_string()));
        assert!(!url.contains("client_secret"), "public client — no secret may ever appear");
    }

    /// Desktop keeps the loopback listener; the URI must match the one
    /// registered under the Azure app's "Mobile and desktop applications".
    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_redirects_to_the_loopback_listener() {
        assert_eq!(redirect_uri(), "http://localhost:8080");
    }

    /// Android has no listener to redirect to — the OS may kill Jodd while
    /// the user is on Microsoft's consent page — so it takes the same App
    /// Links URL Gmail uses, and the same page hands the code back.
    #[cfg(target_os = "android")]
    #[test]
    fn android_redirects_through_the_same_app_links_url_as_gmail() {
        assert_eq!(redirect_uri(), crate::auth::ANDROID_REDIRECT_URI);
        assert_eq!(redirect_uri(), "https://jodd.bbmedia.co.th/oauth2redirect");
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
