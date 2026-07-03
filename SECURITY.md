# Security policy

## Reporting a vulnerability

If you believe you have found a security vulnerability in Jodd —
something that could let an attacker read, modify, or destroy data
belonging to a user other than themselves, escape the OAuth token
scope, escalate local privileges, or otherwise compromise the
integrity of the sync — **please do not file a public issue or
pull request**.

Instead, email:

> **security@bbmedia.co.th**

with:

- A description of the issue.
- Steps to reproduce, or a proof-of-concept if you have one.
- Your assessment of the impact (what an attacker could do).
- Your name / handle for credit, if you want public acknowledgment.

We will reply within **5 business days** to acknowledge receipt, and
we aim to have a fix or mitigation ready within **30 days** for issues
we can reproduce. We will coordinate public disclosure with you.

## Scope

In scope:

- The Jodd desktop binary and its source code in this repository.
- The OAuth 2.0 flow (PKCE handling, redirect URI handling, token
  storage).
- The local SQLite cache (data isolation between accounts, injection
  in queries, schema integrity).
- The Gmail REST request construction (anything that could cause Jodd
  to act outside the user's intended account or scope).
- Any IPC surface exposed by Tauri commands.

Out of scope:

- Vulnerabilities in upstream dependencies that we cannot fix in Jodd
  (please report those to the relevant project). We will, however,
  update affected versions promptly once a patch is released upstream.
- Issues that require physical access to an unlocked, signed-in
  device. The local SQLite cache is intentionally not encrypted at
  rest; this is documented in [DISCLAIMER.md](DISCLAIMER.md).
- Phishing / social engineering targeted at the user's Google account.
- The unverified-app warning from Google when you build Jodd against
  your own OAuth client. This is a Google policy, not a Jodd issue.

## Bounty

We **do not currently run a paid bug bounty program**. We will credit
you publicly (with your permission) in the release notes and in the
acknowledgments section of the about screen.

## Disclosure history

(none yet)
