# Jodd — Distribution & OAuth Credentials Reference

This document captures how Jodd embeds Google OAuth credentials, why that's
acceptable for our client type, and what gates stand between today's setup and
a real consumer distribution. It exists so the credential and verification
constraints aren't rediscovered the hard way at launch time.

## 1. How credentials are embedded

Jodd is a **Desktop application** OAuth client using **PKCE (RFC 7636)**.
Credentials are baked into the binary at **compile time**:

- `src-tauri/build.rs` reads `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET`
  from either process env (CI path — GitHub Actions secrets) or a local
  `../.env` (dev path), and emits them as `cargo:rustc-env`.
- `src-tauri/src/auth.rs` reads them back via `option_env!` (compile-time),
  falling back to runtime `std::env::var` for `tauri dev` convenience.
- Release binaries therefore need **no `.env` at the user's install location**.

**Failure mode to remember:** if neither the env var nor `../.env` is present
*when the build runs*, the values bake in empty. The installed app then sends
`client_id=` (empty) in the auth URL, and Google rejects it with
**`Error 400: invalid_request`, `flowName=GeneralOAuthFlow`**. The fix is
always "populate `.env`, rebuild" — never a runtime config change, because the
value is compile-time.

## 2. Is embedding credentials normal? — yes, with nuance

| Credential | Secret? | Embedding it |
|---|---|---|
| **Client ID** | No — visible in every redirect URL | Always embedded. Normal for all native apps. |
| **Client secret** | Not truly secret *for Desktop clients* | Google explicitly documents it as embeddable in distributed binaries. |

Google's [native-app OAuth docs](https://developers.google.com/identity/protocols/oauth2/native-app)
state the Desktop client secret is **not actually secret** — it's expected to
ship inside distributed binaries. Real security comes from **PKCE**: an
intercepted authorization code cannot be exchanged without the per-flow
`code_verifier`, even if the embedded secret is extracted with `strings`.

Precedent: Thunderbird ships an embedded Google client ID + secret in its
open-source binary and operates at consumer scale. Our approach is the same
industry-normal pattern for this app category.

**What the embedded secret does NOT do:** provide any security on its own. It
is extractable. Treat it as public. The only real risks from a leaked
ID+secret are (a) someone burning **our Cloud project's Gmail API quota**, and
(b) abuse getting **our project flagged**. Low risk for a personal/small tool;
worth mitigating before a wide consumer launch (see §4).

## 3. The real consumer-distribution gate: OAuth verification

The embedded secret is *not* the blocker for shipping to the public —
**Google's OAuth app verification** is.

Jodd requests `https://www.googleapis.com/auth/gmail.modify`, a **sensitive
scope**. Until the app passes verification:

- Capped at ~100 test users (added manually on the OAuth consent screen).
- Every user sees the **"Google hasn't verified this app"** interstitial.

Verification requires: a published privacy policy, a homepage on a verified
domain, the consent screen fully configured, and — for sensitive/restricted
scopes — a security assessment. `gmail.modify` is *sensitive* (free
verification) rather than *restricted* (the `https://mail.google.com/` scope,
which needs the $15k+ CASA assessment); choosing `gmail.modify` was a
deliberate cost decision recorded in `auth.rs`.

## 4. Future hardening (deferred — current PKCE path is intentional)

We are deliberately on the embedded-credential + PKCE path and staying there
for now. Options for a wider launch, in order of effort:

1. **Stay as-is.** Acceptable per Google's Desktop-client guidance. Best for
   personal use and a small test-user group. *(Current state.)*
2. **Thin token-exchange backend.** Move the secret server-side; the desktop
   app talks to our backend instead of Google's token endpoint directly. Lets
   us rotate the secret without shipping a new binary, throttle abuse, and keep
   API quota under our control. Adds a service to operate.
3. **OAuth verification** (orthogonal to 1 vs 2 — required for either before
   public release). This is the gate that actually lands first.

**Bottom line:** the embedded secret is fine and intentional; verification is
the thing to schedule before a consumer launch.
