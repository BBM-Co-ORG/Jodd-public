#!/usr/bin/env python3
"""Dev helper: obtain a Microsoft Graph access token for Jodd probes.

Public-client PKCE auth-code flow with a loopback listener — the same shape
`auth.rs` uses for Gmail on desktop, and the shape `auth_ms.rs` will use.
Originally the 2026-08-14 door test that proved a third-party app registered
in one tenant can get delegated Mail.ReadWrite consent from a personal
Microsoft account and reach the Notes mailbox.

Kept in the repo because the M1 plan's Task 8 needs a live token to probe
whether Graph accepts a $filter over an extended property. See
docs/superpowers/plans/2026-08-14-microsoft-vertical-m1.md.

Usage:
    python3 scripts/ms_get_token.py                 # run the flow, print a summary
    python3 scripts/ms_get_token.py --print-token   # also print the raw access token

--print-token writes a live credential to stdout. It expires in ~1 hour, but
do not paste it anywhere that persists. It exists so Task 8's probe can be run
as: cargo run --example ms_probe -- "$(...)".

No client secret anywhere: public clients cannot hold one.
"""
import base64
import hashlib
import http.server
import json
import os
import secrets
import sys
import threading
import urllib.parse
import urllib.request

CLIENT_ID = "f95a0627-0c33-4fa7-b905-52a57fa1e523"
REDIRECT_URI = "http://localhost:8765"
PORT = 8765
AUTHORITY = "https://login.microsoftonline.com/common"
SCOPE = "Mail.ReadWrite offline_access User.Read"

verifier = base64.urlsafe_b64encode(os.urandom(64)).decode().rstrip("=")
challenge = base64.urlsafe_b64encode(
    hashlib.sha256(verifier.encode()).digest()
).decode().rstrip("=")
state = secrets.token_urlsafe(16)

auth_url = AUTHORITY + "/oauth2/v2.0/authorize?" + urllib.parse.urlencode({
    "client_id": CLIENT_ID,
    "response_type": "code",
    "redirect_uri": REDIRECT_URI,
    "response_mode": "query",
    "scope": SCOPE,
    "state": state,
    "code_challenge": challenge,
    "code_challenge_method": "S256",
    "prompt": "select_account",
})

result = {}
done = threading.Event()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        params = {k: v[0] for k, v in q.items()}

        # Only an actual OAuth callback ends the wait. Browsers hit this origin
        # for reasons that have nothing to do with the redirect — Chrome sends a
        # speculative preconnect the moment the URL is typed into the omnibox,
        # and every page load is followed by /favicon.ico. Both arrive with no
        # query string, and treating them as the callback woke the script with
        # an empty `result`, which then failed the state check as
        # "STATE MISMATCH" *before the user had even consented*. That is a
        # confusing way to report "nothing happened yet", so ignore anything
        # that carries neither `code` nor `error`.
        is_callback = "code" in params or "error" in params

        if is_callback:
            result.update(params)
            body = (b"<html><body style='font:16px sans-serif;padding:40px'>"
                    b"<h2>Jodd door test</h2>"
                    b"<p>You can close this tab and return to the terminal.</p>"
                    b"</body></html>")
            status = 200
        else:
            body = b"waiting for the OAuth callback"
            status = 204 if self.path.startswith("/favicon") else 200

        self.send_response(status)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

        if is_callback:
            done.set()

    def log_message(self, *a):
        pass


def graph(token, path):
    req = urllib.request.Request(
        "https://graph.microsoft.com/v1.0/" + path,
        headers={"Authorization": "Bearer " + token},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "{}")


srv = http.server.HTTPServer(("127.0.0.1", PORT), Handler)
threading.Thread(target=srv.serve_forever, daemon=True).start()

print("AUTH_URL_BEGIN")
print(auth_url)
print("AUTH_URL_END")

# Open it here rather than leaving the user to copy it. The URL is ~400
# characters and the terminal wraps it, so copying by hand is error-prone —
# and the failure it produces is misleading: browsing to REDIRECT_URI itself
# (the short, memorable half) just parks on the listener's holding page while
# the terminal sits at "waiting", with nothing to say the consent screen was
# never opened.
try:
    import webbrowser
    opened = webbrowser.open(auth_url)
except Exception:  # noqa: BLE001 - a headless box has no browser; not fatal
    opened = False

if opened:
    print("\nopened the consent page in your browser.")
else:
    print("\ncould not open a browser — copy the URL between the markers above.")
print("Sign in as the account whose Apple Notes you want to read.")
print("NOTE: %s is where consent RETURNS to. Do not browse there directly."
      % REDIRECT_URI)
print("\nwaiting for the consent redirect on %s ..." % REDIRECT_URI, flush=True)

if not done.wait(timeout=420):
    print("RESULT: TIMEOUT — no redirect received within 7 minutes")
    raise SystemExit(1)
srv.shutdown()

if "error" in result:
    print("RESULT: CONSENT REFUSED AT THE DOOR")
    print("  error             =", result.get("error"))
    print("  error_description =", urllib.parse.unquote_plus(result.get("error_description", "")))
    raise SystemExit(2)

if result.get("state") != state:
    # Print both sides. A bare "mismatch" is unactionable, and the two real
    # causes look completely different: a stale browser tab replaying an older
    # authorize request returns a *different* well-formed state, while a
    # non-callback request that slipped through returns none at all.
    print("RESULT: STATE MISMATCH — aborting")
    print("  expected =", state)
    print("  received =", result.get("state", "<no state parameter at all>"))
    print("  full callback params =", sorted(result.keys()) or "<empty>")
    if not result.get("state"):
        print("\n  No state came back, so this was not an OAuth callback.")
        print("  Close any other tab pointing at localhost:8765 and retry.")
    else:
        print("\n  A different state came back — an older attempt's tab answered")
        print("  this listener. Close every localhost:8765 tab and retry.")
    raise SystemExit(3)

print("\n--- got authorization code, exchanging for token (no client secret) ---", flush=True)
data = urllib.parse.urlencode({
    "client_id": CLIENT_ID,
    "grant_type": "authorization_code",
    "code": result["code"],
    "redirect_uri": REDIRECT_URI,
    "code_verifier": verifier,
}).encode()
req = urllib.request.Request(
    AUTHORITY + "/oauth2/v2.0/token",
    data=data,
    headers={"Content-Type": "application/x-www-form-urlencoded"},
)
try:
    with urllib.request.urlopen(req, timeout=30) as r:
        tok = json.loads(r.read().decode())
except urllib.error.HTTPError as e:
    err = json.loads(e.read().decode() or "{}")
    print("RESULT: TOKEN EXCHANGE FAILED —", e.code)
    print("  error             =", err.get("error"))
    print("  error_description =", (err.get("error_description") or "")[:400])
    raise SystemExit(4)

access = tok["access_token"]
print("token OK")
print("  scopes granted   =", tok.get("scope"))
print("  expires_in       =", tok.get("expires_in"))
print("  refresh_token    =", "present" if tok.get("refresh_token") else "MISSING")

if "--print-token" in sys.argv:
    print("\nACCESS_TOKEN_BEGIN")
    print(access)
    print("ACCESS_TOKEN_END")

print("\n--- calling Graph with OUR OWN app token ---", flush=True)
st, me = graph(access, "me?$select=userPrincipalName")
print("GET /me ->", st, me.get("userPrincipalName", me.get("error", {}).get("message", "")))

q = ("me/messages?$top=25&$select=subject"
     "&$expand=singleValueExtendedProperties($filter=id eq 'String 0x0E05')")
st, msgs = graph(access, urllib.parse.quote(q, safe="/?&=$'()"))
print("GET /me/messages ->", st)
if st == 200:
    notes = []
    for m in msgs.get("value", []):
        sv = m.get("singleValueExtendedProperties") or []
        folder = sv[0]["value"] if sv else "?"
        notes.append((folder, m.get("subject")))
    print("  returned %d items; folder -> subject:" % len(notes))
    for folder, subj in notes:
        print("    [%s] %s" % (folder, (subj or "")[:48]))
    print("\nRESULT: DOOR IS OPEN — third-party public client reached the mailbox.")
else:
    print("  error:", json.dumps(msgs)[:400])
    print("\nRESULT: TOKEN OK BUT GRAPH CALL FAILED")
