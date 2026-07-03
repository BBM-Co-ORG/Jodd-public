# How Jodd treats Gmail for note sync

This document describes the protocol, message format, and data-loss defenses
that Jodd uses to keep Apple Notes ↔ Gmail bidirectional sync working
correctly. It's intended as a reference for understanding why specific code
exists, especially the parts that look like over-engineering until you see
the failure mode they prevent.

**Audience:** maintainers, contributors, anyone trying to understand a bug
report involving sync or Apple Notes interop.

---

## 1. Architectural model

```
┌─────────────────┐         IMAP         ┌─────────────────┐
│  Apple Notes    │  ←─────────────────→ │     Gmail       │
│  (iOS / Mac)    │  (APPEND + EXPUNGE)  │   (storage)     │
└─────────────────┘                      └────────┬────────┘
                                                  │
                                                  │ Gmail REST API
                                                  │ (HTTPS + OAuth 2.0 + PKCE)
                                                  │
                                            ┌─────▼─────┐
                                            │  Jodd    │
                                            │ (this app)│
                                            └───────────┘
```

**Gmail is the single source of truth.** Jodd stores no notes locally
(beyond an in-memory cache for the current session). Apple Notes also
treats Gmail as backing storage when its Google account has Notes enabled.

Jodd and Apple Notes are **peers** through Gmail — neither is master.
Both read and write the same underlying messages. Bidirectional changes
propagate as soon as the other side polls Gmail.

### Why peers, not master/replica

We considered (and rejected) the master/replica model where Jodd would
treat its own writes as authoritative. That breaks the moment the user
edits on iPhone — and the iPhone is where most edits actually happen.
Peer-to-peer through Gmail is the only model that handles multi-device
editing without requiring our own backend service.

---

## 2. Authentication

### OAuth 2.0 flow with PKCE

Jodd uses Google's **Desktop application** OAuth client type with PKCE
(RFC 7636) for per-flow protection. Both `client_id` and `client_secret`
are embedded in the binary; PKCE is an additional defense layer:

```
1. User clicks Sign in →
2. Jodd generates code_verifier (64 random chars, ~380 bits entropy)
3. Jodd computes code_challenge = base64url(sha256(code_verifier))
4. Browser opens to accounts.google.com/o/oauth2/v2/auth?
       client_id=…&
       redirect_uri=http://localhost:8080/callback&
       scope=https://www.googleapis.com/auth/gmail.modify&
       access_type=offline&
       prompt=consent&
       code_challenge=…&code_challenge_method=S256
5. User authorizes
6. Browser redirects to localhost:8080/callback?code=…
7. Jodd exchanges the code at oauth2.googleapis.com/token with:
       client_id, client_secret, code_verifier, code, redirect_uri
8. Google returns access_token + refresh_token
9. Refresh token persists in OS keychain (per account)
10. Access token cached in memory (ephemeral)
```

### Why PKCE despite Google requiring `client_secret`

Google's Desktop OAuth still requires `client_secret` even with PKCE
(spec-non-compliant but documented Google behavior). The `client_secret`
for Desktop clients is **documented as embeddable in distributed binaries** —
Google treats it as not actually secret. PKCE adds the defense that an
intercepted auth `code` cannot be exchanged for tokens without also having
the per-flow `code_verifier`, which lives only briefly in memory.

So: `client_secret` extractable from binary + PKCE verifier per-flow =
strictly more secure than `client_secret` alone.

### Scope: `gmail.modify`

Jodd uses [`https://www.googleapis.com/auth/gmail.modify`](https://developers.google.com/identity/protocols/oauth2/scopes#gmail)
which is a "sensitive" (not "restricted") scope. It permits:

- Read messages and labels
- Insert and delete messages
- Modify labels

Crucially, this scope's verification path is **free** (privacy policy +
brand review). The broader `https://mail.google.com/` scope is "restricted"
and requires a $15K+ CASA security assessment for production verification —
we deliberately chose the narrower scope.

### Refresh token lifecycle

- **First sign-in:** Google returns refresh token. Jodd writes it to OS
  keychain under per-account key (`jodd` / `rt::<email>`).
- **Subsequent app starts:** Jodd reads from keychain → calls token refresh →
  receives new access token (refresh tokens themselves are long-lived).
- **Sign-out (per account):** delete keychain entry + clear in-memory state.
- **Token rotation:** if Google rotates the refresh token, we save the new one.

### Multi-account model

```
~/Library/Application Support/jodd/accounts.json
  ├── kaiwan@bbmedia.co.th  (added 2026-06-04T...)
  └── kaiwan.h@gmail.com    (added 2026-06-04T...)

OS Keychain:
  ├── jodd / rt::kaiwan@bbmedia.co.th  → <refresh token>
  └── jodd / rt::kaiwan.h@gmail.com    → <refresh token>

In-memory (AppState):
  account_states: HashMap<AccountId, AccountState> {
    "kaiwan@bbmedia.co.th": {
      access_token: Some("ya29..."),
      label_map_cache: Some((HashMap, Instant)),
    },
    ...
  }
```

Each account fetches/writes/caches independently. The Tauri command layer
threads `account_id` through every operation; the Gmail API layer
(`gmail.rs`) is account-blind and just takes a `token`.

---

## 3. Gmail REST API endpoints we use

| Endpoint | Method | When | Cost (units) |
|---|---|---|---|
| `users/me/profile` | GET | First account resolution | 1 |
| `users/me/labels` | GET | Build label map (cached 5min) | 1 |
| `users/me/messages?labelIds=…` | GET | List notes in a label | 5 |
| `users/me/messages/{id}?format=full&fields=…` | GET | Fetch one note's content | 5 |
| `users/me/messages?internalDateSource=dateHeader` | POST | Save (insert) a note | 25 |
| `users/me/messages/{id}` | DELETE | Remove old version, cleanup duplicates | 10 |
| `oauth2.googleapis.com/token` | POST | Initial exchange + refresh | (not counted) |

### Fields mask

`messages.get` is called with `?fields=id,labelIds,payload(headers,body/data,parts(mimeType,body/data,parts(...)))` to drop ~70% of response bytes
(no snippet, no sizeEstimate, no raw, no historyId). Three levels of nested
`parts` are requested because Apple Notes' edited messages sometimes arrive
as nested multipart structures.

### `internalDateSource=dateHeader`

Set on every `messages.insert`. Tells Gmail to use our explicit `Date:`
header as the sort key rather than the moment of insertion. This matters
because Apple Notes' reconciliation picks the "latest" revision by date —
without this parameter, a save-then-reconcile race could pick the wrong
version.

### Parallelism and concurrency cap

`list_notes` fans out `messages.get` calls in parallel via `tokio::spawn`
with a semaphore of **8 concurrent in-flight requests**. The cap exists
because Gmail's per-user rate limit is 250 units/sec; at 5 units per
`messages.get`, 8 in flight × ~30/sec sustained = 1200 units/sec headroom
well under ceiling.

```rust
// gmail.rs (excerpt)
const FETCH_CONCURRENCY: usize = 8;
let sem = Arc::new(Semaphore::new(FETCH_CONCURRENCY));
let handles: Vec<_> = message_refs.into_iter().map(|m| {
    let permit = sem.clone();
    let token = token.clone();
    tokio::spawn(async move {
        let _p = permit.acquire().await.ok()?;
        fetch_note(&token, &m.id, &label_map).await.ok()
    })
}).collect();
```

### Quota math (typical small mailbox of 10 notes)

| Operation | Frequency | Units/event | Daily total at 60s poll |
|---|---|---|---|
| labels.list (cached) | every 5 min | 1 | 288 |
| messages.list × 3 labels | every 60s | 15 | 21,600 |
| messages.get × 10 (parallel) | every 60s | 50 | 72,000 |
| Save (with cleanup) | typical user, 5/day | ~70 | 350 |
| **Total per day** | | | **~94,000** |

Gmail's per-project daily cap is **1,000,000,000** units. We use <0.01% on
a personal-use account. Quota is not a practical constraint at any
reasonable scale; latency from round-trips is.

### HTTP client pattern

Every Gmail API call follows the same shape (gmail.rs):

```rust
let client = reqwest::Client::new();
let res = client
    .<method>(<url>)
    .bearer_auth(token)             // Authorization: Bearer ya29...
    .query(&[("key", "value"), …])  // for GET parameters / mask
    .form(&params)                  //   OR  application/x-www-form-urlencoded
    .json(&body)                    //   OR  application/json
    .send()
    .await
    .map_err(|e| e.to_string())?;

let status = res.status();
let body = res.text().await.map_err(|e| e.to_string())?;
if !status.is_success() {
    return Err(format!("op failed: {} — {}", status, body));
}
let parsed: ResponseType = serde_json::from_str(&body)
    .map_err(|e| format!("parse: {} — body: {}", e, body))?;
```

The reason we always read `body` via `.text()` even on success — and parse
it manually with `serde_json::from_str` — is that errors capture the
response body in the error message. `.json::<T>()` returns a parse error
without the original body, which makes debugging API issues painful.

### Status check pattern (not just `.error_for_status()`)

reqwest's `.send()` returns `Ok` for any HTTP response, including 4xx/5xx.
The naive shape

```rust
client.send().await?       // succeeds for 5xx
    .json::<T>().await?    // parse error doesn't include body
```

silently swallows server errors. We fixed this in two waves:

1. `delete_note` was the original silent-success bug — see §7 (Write pipeline) below.
2. All other paths now explicitly read status and propagate the body in
   error messages.

### Idempotent vs non-idempotent operations

| Operation | Idempotent? | Retry safe? |
|---|---|---|
| `users/me/profile` | ✓ | yes |
| `users/me/labels` | ✓ | yes |
| `users/me/messages?labelIds=…` | ✓ | yes |
| `users/me/messages/{id}?format=full` | ✓ | yes |
| `messages.insert` | **no** (creates new id) | ❌ — retry creates duplicates |
| `messages.delete` | ✓ (404 = already deleted) | yes |
| OAuth token refresh | ✓ | yes |

Currently only `delete_note` has explicit retry logic (3 attempts with
exponential backoff for 5xx). The others rely on the caller — if a
`messages.get` fails during `list_notes`, that one note is dropped from
the result and the next poll picks it up. Acceptable for v0.1 because
the user-visible failure is just "one note missing temporarily."

### Concurrency cap

Parallel `messages.get` calls are bounded by `tokio::sync::Semaphore`
with N=8:

```rust
const FETCH_CONCURRENCY: usize = 8;
let sem = Arc::new(Semaphore::new(FETCH_CONCURRENCY));
// each task acquires a permit before calling fetch_note
```

The number was chosen against Gmail's **per-user** rate limit of 250
units/sec. Each `messages.get` is 5 units; 8 concurrent × ~30 calls/sec
sustained = ~1200 units/sec of capacity, comfortably under the ceiling
even when other operations (list, save) run concurrently. Bumping to 12
or 16 would still be safe; we kept 8 as a conservative default.

### What we don't do (API layer)

- **No client reuse.** Each call creates a fresh `reqwest::Client`. With
  reqwest's internal connection pooling, this is slightly wasteful but
  not measurably so for our request volume. A long-lived shared client
  would save a few μs per call.
- **No request timeouts.** We rely on reqwest's defaults (~30s connect,
  no overall timeout). A request that hangs would block the caller
  indefinitely. Should be addressed for production hardening.
- **No automatic 401 → refresh-token retry.** If an access token expires
  mid-session (after ~1 hour), the next call returns 401 and surfaces as
  an error. Refreshing on 401 with retry would be cleaner; for v0.1 the
  user works around it by closing/reopening the app (triggers token
  refresh from keychain via `ensure_token`).
- **No request tracing / metrics.** Each `log!` line is the only
  observability. For production, OpenTelemetry or structured logging
  would help track per-endpoint latency, error rates, quota burn.
- **No backoff for `messages.insert`.** Because insert isn't idempotent,
  blind retry would duplicate the message. The right pattern is "search
  for our X-UUID after a perceived failure and decide based on what's
  there" — not implemented yet.

---

## 4. Message format on the wire

Apple Notes ↔ Gmail messages have a specific structure that Jodd must
mirror byte-for-byte for bidirectional compatibility. The format is
reverse-engineered (Apple has never published a spec) and verified
empirically via export-and-diff testing.

### Headers Jodd writes

```
From: <user_email>                                            ← from users.getProfile
X-Uniform-Type-Identifier: com.apple.mail-note               ← Apple's note marker
Content-Type: text/html; charset=<utf-8 or us-ascii>         ← content-adaptive
Content-Transfer-Encoding: <7bit or quoted-printable>        ← content-adaptive
Mime-Version: 1.0 (Mac OS X Notes 4.13 \(3146.121.7\))      ← Apple's masquerade
Date: <RFC 2822, local timezone, %-d format>                ← latest modified time
X-Mail-Created-Date: <preserved across saves>                ← original creation time
Subject: <plain or RFC 2047 encoded-word>                    ← content-adaptive
X-Universally-Unique-Identifier: <8-4-4-4-12 hex, hyphens>  ← the logical note identity
Message-Id: <<new-UUID>@<email-domain>>                      ← fresh per save
```

### Why each field matters

- **`X-Uniform-Type-Identifier: com.apple.mail-note`** — Apple Notes filters
  messages by this exact value. Without it, the message exists in Gmail but
  Apple Notes won't display it.

- **`X-Universally-Unique-Identifier`** — the **logical note identity**.
  Apple Notes' reconciliation matches messages by this UUID, picking the
  most recent by Date. Two messages with the same UUID = revisions of the
  same note. Two messages with different UUIDs = different notes.

  Format is critical: **`XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`** (uppercase,
  hyphenated). Stripped-hyphen form (`XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX`)
  is treated as a different UUID by Apple's `strcmp` reconciliation. Jodd
  used to strip hyphens — this caused duplicates on round-trip and was the
  first major interop bug we fixed.

- **`X-Mail-Created-Date`** — Apple tracks original creation time separately
  from `Date:` (last modified). Jodd preserves this across edits so Apple
  Notes doesn't reset the creation timestamp on every Jodd save.

- **`Mime-Version: 1.0 (Mac OS X Notes 4.13 \(3146.121.7\))`** — Apple
  uses this exact literal string. Different MIME versions cause Apple Notes
  to treat the message as a "foreign" client's writing, with subtle
  consequences (sometimes refuses to edit, sometimes creates parallel
  revisions). The escaped parens (`\(` `\)`) are part of the literal — not
  escape sequences for any parser; they're how Apple Mail constructs the
  value internally. We copy byte-for-byte.

- **`Date:`** in Apple's no-leading-zero format (`Thu, 4 Jun 2026 …`)
  rather than the more common zero-padded form. Apple's reconciliation
  doesn't care, but the format matches what their own writes produce.

- **`Message-Id:`** — UUID format with the user's email domain
  (`<UUID@bbmedia.co.th>`). Apple regenerates this on every save (each save
  is a new Gmail message); Jodd does the same. The X-UUID is the durable
  identity, Message-Id is per-message.

### Body structure

```html
<html><head></head><body style="overflow-wrap: break-word;
   -webkit-nbsp-mode: space; line-break: after-white-space;">
  <div>{title}</div>             ← Apple's "first body element = displayed title" convention
  <div>{line 1}</div>             ← actual note content
  <div>{line 2}</div>
  ...
</body></html>
```

**The title MUST be the first body element.** Apple Notes uses the body's
first text node (whether wrapped in `<div>` or bare text) as the displayed
title chip in its UI. The `Subject:` header is metadata; Apple Notes
ignores it for display. Jodd's `inject_title_into_body()` ensures this on
save; `strip_leading_title()` removes it on read so the editor doesn't
double-show the title.

This was the second major interop fix: notes that Jodd wrote without title
injection appeared *titleless* in Apple Notes (the title chip would show
"line 1" instead).

### Content-adaptive encoding

Jodd mirrors Apple's encoding strategy based on content:

| Content | charset | CTE | Subject |
|---|---|---|---|
| Pure ASCII | `us-ascii` | `7bit` | plain text |
| Any non-ASCII | `utf-8` | `quoted-printable` | RFC 2047 encoded-word |

For non-ASCII subjects, RFC 2047 encoded-word format picks **B vs Q**
encoding by whichever produces a shorter result (matches Apple's
optimization):

- `B` (base64): constant ~33% overhead. Wins for non-ASCII-dominant text.
- `Q` (quoted-printable-like): 1 char/byte for ASCII, 3 chars/byte for
  non-ASCII. Wins for ASCII-dominant text.

### What we deliberately diverge from

- **`Received:` header**: Gmail's API auto-inserts
  `Received: from <project-id> ... by gmailapi.google.com with HTTPREST`
  on every `messages.insert`. We can't suppress this without switching to
  IMAP APPEND. Apple Notes appears not to parse this header, so the
  divergence is cosmetic only.

- **Long Subject chunking**: Apple splits long encoded-word subjects into
  ≤75-char chunks separated by CRLF + space (per RFC 2047 §2). Jodd emits
  one long encoded-word that exceeds 75 chars. Gmail accepts it, Apple
  parses it. Not strictly compliant but not blocking.

---

## 5. Encoding decisions and edge cases

Encoding in this codebase is a **stack of four independent layers**, each
with its own decision logic. Bugs almost always live in one specific layer,
so understanding the boundaries makes debugging faster.

```
Layer 4: HTTP transport    base64url of the raw RFC 822 message
              ↑              (Gmail REST API's wire format for messages.insert)
Layer 3: Header encoding   RFC 2047 encoded-word for non-ASCII Subject
              ↑
Layer 2: Charset           us-ascii  vs  utf-8
              ↑
Layer 1: Body transfer     7bit  vs  quoted-printable  vs  8bit  vs  base64
                            (MIME Content-Transfer-Encoding)
```

We don't currently use `base64` for body transfer (Apple Notes doesn't, and
text content rarely benefits over QP). We don't use `8bit` because it's
ambiguously supported across mail infrastructure — `quoted-printable` is the
strict-7bit-safe alternative.

### Layer 1 — Content-Transfer-Encoding (CTE)

```
                          ╔═══════════════════════════════╗
content all ASCII (≤0x7F)?║ YES → 7bit  + body sent raw  ║
                          ║ NO  → quoted-printable + QP   ║
                          ╚═══════════════════════════════╝
```

**`7bit`** — Body bytes are sent unchanged. Lines must be ≤998 bytes
(RFC 5322), no bytes ≥ 0x80. Valid only when the entire body is pure ASCII.
Apple uses `7bit` for English-only notes.

**`quoted-printable`** (QP) — Encodes any byte not in the printable ASCII
range as `=XX` (hex of the byte). Rules:

- Printable ASCII (0x21–0x7E except `=`): pass through
- `=` itself: encoded as `=3D`
- Non-ASCII bytes: encoded as `=XX`
- Spaces at end of line: encoded as `=20` (otherwise stripped by some readers)
- Tabs: encoded as `=09`
- Lines limited to 76 chars; longer lines use **soft line breaks** —
  trailing `=` at end of line means "join with next line, no actual newline"

Example: the UTF-8 bytes for `ไทย` (`E0 B9 84 E0 B8 97 E0 B8 A2`) are
encoded as `=E0=B9=84=E0=B8=97=E0=B8=A2`.

Jodd uses the [`quoted_printable`](https://docs.rs/quoted_printable) crate
for both encode and decode — implementing QP by hand is a common source of
edge-case bugs around line breaks and trailing whitespace.

### Layer 2 — Charset declaration

The `Content-Type` header carries the charset:

```
Content-Type: text/html;
    charset=us-ascii    ← for 7bit
    charset=utf-8       ← for QP
```

**Why us-ascii instead of utf-8 for pure-ASCII content?** Strictly speaking,
UTF-8 is a superset of ASCII, so `charset=utf-8 + 7bit` would be valid for
ASCII text. Apple uses `us-ascii` specifically, and we mirror that for
byte-identical output. Mail clients that pre-date UTF-8 support also
handle `us-ascii` more reliably.

**Why utf-8 instead of one of the legacy 8-bit single-byte charsets**
(ISO-8859-1, Windows-1252) for non-ASCII content? Because UTF-8 is the only
universal encoding that handles every script (Thai, Chinese, emoji, etc.)
without needing per-script charset switching. Apple uses utf-8 unconditionally
for non-ASCII; we do too.

### Layer 3 — Subject header encoding (RFC 2047)

Subject is special: per RFC 5322, all header values must be **7-bit ASCII
only**. There's no in-band way to declare a charset for a header line.
RFC 2047 adds an escape syntax called **encoded-word**:

```
Subject: =?<charset>?<encoding>?<encoded-text>?=
```

`<encoding>` is one of:

- **`B`** — base64 of the UTF-8 bytes (constant ~33% overhead)
- **`Q`** — quoted-printable-like, but with these RFC 2047 §4.2 differences:
  - Space → underscore (`_`), not `=20`
  - Limited safe-char set (letters, digits, and a handful of punctuation)
  - All other bytes → `=XX`

**Which one wins?** Length, mostly:

| Input character mix | B length | Q length | We pick |
|---|---|---|---|
| All ASCII | `1.33N + 12` | `N + 11` | Q (and we'd usually skip encoding entirely) |
| All non-ASCII (Thai) | `1.33N + 12` | `3N + 11` | B |
| Mixed half-and-half | `1.33N + 12` | `2N + 11` | B for large N, Q for small |

Jodd computes both and picks the shorter — this matches Apple's behavior
and we've verified empirically.

**Examples:**

| Subject text | Encoded as |
|---|---|
| `Hello` | (no encoding needed) `Subject: Hello` |
| `ทดสอบ` | `Subject: =?utf-8?B?4LiX4LiU4Liq4Lit4Lia?=` (B is shorter) |
| `mix ไทย english` | `Subject: =?utf-8?Q?mix_=E0=B9=84=E0=B8=97=E0=B8=A2_english?=` (Q is shorter) |

**Long Subject chunking** (RFC 2047 §2): no encoded-word should exceed
75 chars total. For longer subjects, the spec says to emit multiple
encoded-words separated by CRLF + space. Apple does this. Jodd currently
emits one possibly-long encoded-word — Gmail and Apple parse it correctly,
but strict RFC parsers would reject it. **Known limitation.**

### Layer 4 — HTTP transport (base64url)

The Gmail REST API's `messages.insert` accepts a raw RFC 822 message in a
JSON field:

```json
POST /gmail/v1/users/me/messages?internalDateSource=dateHeader
{
  "raw": "<base64url-encoded raw RFC 822 message>",
  "labelIds": ["Label_xxx"]
}
```

`raw` uses **base64url** (RFC 4648 §5) — the URL-safe variant of base64
where `+/=` are replaced with `-_` and trailing `=` padding is preserved.
We construct the raw message as a single Rust `String` containing all the
headers and the (possibly-QP-encoded) body, then encode it once with
`base64::engine::general_purpose::URL_SAFE`.

**`messages.get` response body uses the same encoding** — `payload.body.data`
is base64url-encoded. We decode it with our `decode_body` helper which
tries URL_SAFE first and falls back to STANDARD base64 in case any path
ever returns standard base64 instead.

### Decoding flow on read

Decoding goes through the layers in **reverse**:

```
1. HTTP/JSON:        Gmail's messages.get returns body.data as base64url
                     ↓ URL_SAFE.decode()
2. Transfer (CTE):   Decoded bytes are the body content, ALREADY
                     transfer-decoded by Gmail. (Verified empirically —
                     QP-encoded notes from Apple Notes come back as
                     plain UTF-8 text, not "=E0=B9=84..." literals.)
                     ↓
3. Charset:          Bytes are interpreted as UTF-8 directly. If invalid
                     UTF-8, the result is None and we silently produce
                     an empty body (caller's fallback to parts kicks in).
                     ↓
4. RFC 2047:         Subject headers come back from Gmail ALREADY
                     RFC 2047-decoded. Verified by inspecting Apple-saved
                     notes — we get "ไทย", not "=?utf-8?B?4LmE4LiX4Lii?=".
```

So most decoding work is done by Gmail's API server. The only decoding we
do is base64url → bytes → UTF-8 string. We don't have to implement QP
decoder or RFC 2047 decoder on the read side.

### Mojibake patterns (the bugs we've seen)

When encoding goes wrong, the corruption pattern is recognizable. We've
defended against three:

**Pattern A: UTF-8 mis-decoded as Latin-1 / Windows-1252.** Pre-fix Jodd
wrote raw UTF-8 in the Subject header (spec violation). Gmail stored those
bytes literally. On read, Gmail tried to interpret them as text — falling
back to Windows-1252 for the high-byte range — producing strings like
`à¸—à¸"à¸ªà¸_à¸š` for what was originally `ทดสอบ`.

```
ทดสอบ (UTF-8 bytes):    E0 B9 84  E0 B8 97  E0 B8 A2  E0 B8 AA  E0 B8 9A
Interpreted as cp1252: à   ¸   "    à   ¸    —   à   ¸   ª   à   ¸   _    à   ¸   š
                                          ^^^                                   
                            (0x97 in cp1252 = em-dash "—", not the
                             control char that Latin-1 would say)
```

**Defense:** `try_recover_mis_decoded_utf8()` in gmail.rs. Detects
characters in the Latin-1 high range (0x80–0xFF), casts them back to bytes
(with a Windows-1252 supplement table for codepoints like `0x2014` →
`0x97` for em-dash), and tries to UTF-8-decode the resulting byte
sequence. Accepts the recovery only if the result contains real non-Latin-1
codepoints — so legitimate French/German Latin-1 text isn't falsely
"recovered" into garbage.

**Pattern B: Charset declared but missing in HTTP response.** The OAuth
callback success page (`<h2>✅ Jodd Connected!</h2>`) was originally
served without a `charset=utf-8` declaration in the `Content-Type` header.
Some browsers defaulted to Latin-1 and showed `âœ…` instead of `✅`.

**Defense:** explicit `Content-Type: text/html; charset=utf-8` header in
the callback response. Cheap fix; always declare charset on any HTTP
response with non-ASCII content.

**Pattern C: HTML special chars in body content.** Apple's body sometimes
contains `&nbsp;` entities (non-breaking spaces). On read, we pass these
through unchanged to the contenteditable, which renders them as visible
NBSP characters — looks like extra spacing in the rendered note. On write,
we don't entity-encode anything we don't need to (the contenteditable's
`.innerHTML` output is already valid HTML).

**Not currently defended:** if the body contains a bare `<` that's not part
of a tag, the contenteditable parser might interpret it as a malformed
tag. This would survive a round-trip but display oddly. Not observed in
practice because contenteditable's serialization always emits well-formed
HTML.

### Edge cases by content type

| Content | charset | CTE | Subject | Notes |
|---|---|---|---|---|
| `hello` / `hello world` | us-ascii | 7bit | plain | Simplest case |
| Title with `&` | us-ascii | 7bit | `Subject: foo & bar` | `&` is ASCII, not entity-encoded |
| `café` (Latin-1 char) | utf-8 | QP | RFC 2047 Q | French accent: `=C3=A9` for `é` |
| `🎉 launched!` (emoji) | utf-8 | QP | RFC 2047 B | Emoji is 4-byte UTF-8 |
| `ทดสอบ` (Thai) | utf-8 | QP | RFC 2047 B | All bytes ≥ 0x80, B wins |
| `mix 한국어 english` (CJK + ASCII) | utf-8 | QP | RFC 2047 Q | ASCII-dominant, Q wins |
| Body with `<span style="color:red">` | (matches body lang) | (matches body) | (matches title lang) | Inline styles round-trip as-is |
| 100-char Thai subject | utf-8 | QP | RFC 2047 B (one long word) | RFC says split at 75; we don't |

### Specific characters that have caused bugs

- **NBSP (U+00A0)**: Apple writes literal `&nbsp;` entity into body HTML.
  Survives round-trip but may render with extra spacing. Not a bug per se.
- **`—` em-dash (U+2014)**: Lives in the Windows-1252 supplement range
  (0x97). The mojibake recovery table maps it explicitly.
- **`"` left/right curly quotes (U+201C / U+201D)**: Same situation as
  em-dash — Windows-1252 supplement (0x93 / 0x94). In our recovery table.
- **`เ`, `แ`, `ใ`, `โ`, `ไ` Thai leading vowels**: These have a property
  that's relevant for character-counting code: they're visually-leading
  but byte-sequential, so character offsets don't match visual cursor
  position. Doesn't affect Jodd's logic but worth knowing for any future
  text-position work.
- **CRLF in raw message** vs **LF in body HTML**: The raw RFC 822 message
  uses CRLF line terminators between headers and as the header/body
  separator. The body HTML itself can use LF only — both renderers
  tolerate it.

---

## 6. Read pipeline

```
┌───────────────────────────────────────────────────────────────┐
│  list_notes(account_id)                                       │
│                                                               │
│  1. ensure_token(account_id)                                  │
│     ├── in-memory cached? return                              │
│     └── refresh from keychain → call refresh endpoint         │
│                                                               │
│  2. cached_label_map(account_id, token)                       │
│     ├── cache fresh (< 5min)? return                          │
│     └── GET /labels → store with timestamp                    │
│                                                               │
│  3. Filter label_map for "Notes" + "Notes/*" entries          │
│                                                               │
│  4. For each Notes label:                                     │
│       GET /messages?labelIds=<label_id>                       │
│                                                               │
│  5. Dedupe message IDs across labels                          │
│                                                               │
│  6. Parallel fetch (cap 8): GET /messages/<id>?fields=…       │
│                                                               │
│  7. For each fetched message → fetch_note(msg, label_map):    │
│       ├── verify X-Uniform-Type-Identifier matches            │
│       ├── decode body (body.data → parts fallback → recurse)  │
│       ├── recover mojibake'd Subject (Latin-1 → UTF-8)        │
│       ├── canonicalize X-UUID to hyphenated form              │
│       ├── strip leading title from body                       │
│       └── return Note                                         │
│                                                               │
│  8. Self-heal: if 0 results, retry with fresh label map       │
│     (catches the "Apple recreated the Notes label" case)      │
│                                                               │
│  9. Dedupe by X-UUID (longest body wins)                      │
│                                                               │
│  10. Sort by parsed Date desc                                 │
│                                                               │
│  11. Stamp account_id on every note                           │
│                                                               │
│  12. Return Vec<Note>                                         │
└───────────────────────────────────────────────────────────────┘
```

### Body extraction with multipart fallback

Gmail returns message payloads in one of two shapes:

- **Single-part**: `payload.body.data` contains the encoded body
- **Multipart**: `payload.body.data` is null/empty; `payload.parts` contains
  the actual content as nested structures

Apple Notes' **edited** messages frequently arrive as multipart
(`multipart/mixed` → `multipart/alternative` → `[text/plain, text/html]`)
even though the **original** message was single-part. This was a real bug
that caused empty editor display for any note Apple had edited recently.

Our fallback walks the parts tree recursively:

```rust
// gmail.rs (excerpt)
let body_html = msg.payload.body.as_ref()
    .and_then(|b| b.data.as_deref())
    .map(decode_body)
    .filter(|s| !s.is_empty())
    .or_else(|| find_html_in_parts(msg.payload.parts.as_deref()))
    .unwrap_or_default();
```

### Subject mojibake recovery

Jodd versions before the RFC 2047 fix wrote raw UTF-8 in the Subject
header (spec violation — Subject must be 7-bit ASCII or encoded-word).
Gmail stored those bytes as-is; on read, Gmail returned them as the JSON
string mis-decoded as Latin-1, producing strings like `"à¸—à¸\"à¸ªà¸­à¸š"`
for what was originally `"ทดสอบ"`.

`try_recover_mis_decoded_utf8()` detects this pattern (a recognizable
fingerprint where every byte ≥ 0x80) and recovers the original UTF-8 by:

1. Casting each char back to its Latin-1 byte value (with Windows-1252
   supplement table for codepoints in 0x80–0x9F range)
2. Attempting to re-decode the byte sequence as UTF-8
3. Accepting the recovery only if it produces real non-Latin-1 codepoints
   (so legitimate French/German text doesn't get false-positive recovered)

This is display-only recovery — the note in Gmail still has the broken
Subject bytes stored. Re-saving the note (any edit triggers autosave)
writes a properly RFC 2047-encoded Subject and the broken version gets
swept by `cleanup_stale_uuid_duplicates`.

### Title stripping

Apple's title-as-first-body-element convention means a verbatim editor
display would show the title twice (once as the Title field, once as the
first body line). `strip_leading_title()` removes the leading element if
it matches the Subject:

```rust
// Case 1: <div>{title}</div> as first child of body → strip it
// Case 2: bare text {title} immediately after <body...> → strip it
// Otherwise: leave body untouched
```

Edge case: a note whose actual content legitimately starts with text
matching the subject would lose that first line. Rare and acceptable for
v0.1 — Apple Notes has the same behavior.

### UUID canonicalization

```rust
// gmail.rs
fn canonicalize_uuid(s: &str) -> Option<String> {
    uuid::Uuid::parse_str(s).ok().map(format_apple_uuid)
}
```

`uuid::Uuid::parse_str` accepts both hyphenated and stripped forms. The
result is always emitted in Apple's canonical
`XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX` format. This lets old Jodd-saved
notes (with stripped UUIDs) interop with new Jodd saves and Apple Notes.

### Dedupe by X-UUID

When two Gmail messages share the same X-UUID, exactly one should win.
Jodd picks the one with the longest `body_html` — this handles the case
where one is a truncated/broken save and the other is the full content:

```rust
// In list_notes after fetch
let mut by_uuid: HashMap<String, Note> = HashMap::new();
for note in fetched {
    match by_uuid.entry(note.uuid.clone()) {
        Entry::Occupied(mut e) if note.body_html.len() > e.get().body_html.len() => {
            e.insert(note);
        }
        Entry::Vacant(e) => { e.insert(note); }
        _ => {}
    }
}
```

This is defense in depth — even when the write-side cleanup pass fails to
delete an orphan, the user sees the correct content.

---

## 7. Write pipeline

```
┌───────────────────────────────────────────────────────────────┐
│  save_note(account_id, title, body, …)                        │
│                                                               │
│  1. ensure_token(account_id)                                  │
│                                                               │
│  2. cached_label_map(account_id, token)                       │
│                                                               │
│  3. Resolve target label name → label ID                      │
│                                                               │
│  4. Generate UUID (preserve existing if editing)              │
│                                                               │
│  5. Generate Message-Id with user's email domain              │
│                                                               │
│  6. inject_title_into_body(body, title)                       │
│       └── adds <div>{title}</div> as first child of <body>    │
│           if not already present                              │
│                                                               │
│  7. Decide encoding (content-adaptive):                       │
│       ASCII-only → us-ascii + 7bit + plain Subject            │
│       Non-ASCII → utf-8 + QP body + RFC 2047 Subject (B|Q)    │
│                                                               │
│  8. Format raw RFC 822 message with all headers               │
│                                                               │
│  9. base64url encode → POST /messages?internalDateSource=…    │
│                                                               │
│  10. If existing_gmail_id provided AND ≠ new id:              │
│        delete_note(existing_gmail_id)                         │
│                                                               │
│  11. cleanup_stale_uuid_duplicates:                           │
│        ├── messages.list per Notes label                      │
│        ├── For each ≠ keep_id: GET headers, check X-UUID      │
│        └── Delete any others matching same UUID               │
│                                                               │
│  12. Return SavedNote { id, uuid }                            │
└───────────────────────────────────────────────────────────────┘
```

### Insert-then-delete (not delete-then-insert)

Always insert the new version first, then delete the old. The reverse
ordering — delete first, then insert — would leave the user with **no
copy** of their note if the insert failed. Insert-then-delete leaves
**two copies** in the failure case: annoying but recoverable.

This is also why the cleanup pass exists: when delete-old fails (Apple's
ID was stale because Apple did its own write in the meantime), the
orphan accumulates. The cleanup pass on the next save sweeps it.

### Status-checked delete with retry

Previously `delete_note` silently treated all HTTP responses as success
(reqwest's `send()` returns `Ok` for any response, including 4xx/5xx).
This caused real-world duplicates because Google could respond 404
("already deleted") and we'd log "deleted" without verifying.

Now:

```rust
// gmail.rs
pub async fn delete_note(token: &str, id: &str) -> Result<(), String> {
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(200 << attempt)).await;
        }
        let res = client.delete(...).send().await?;
        let status = res.status();
        if status.is_success() || status.as_u16() == 404 {
            // 204 = deleted, 404 = idempotent (already gone)
            return Ok(());
        }
        if status.is_client_error() {
            return Err(...); // non-retryable
        }
        // 5xx → retry
    }
    Err(...)
}
```

The retry handles Gmail's transient propagation lag — a freshly-inserted
message can briefly 404 from delete while Gmail's index updates.

### Cleanup-on-save: defending against multi-writer races

Every successful save kicks off `cleanup_stale_uuid_duplicates`:

```rust
// Pseudo-code
async fn cleanup_stale_uuid_duplicates(token, target_uuid, keep_id, label_map) {
    let all_ids = collect message IDs across all Notes labels;
    for id in all_ids if id != keep_id {
        let msg = GET /messages/{id}?format=metadata
                       &metadataHeaders=X-Universally-Unique-Identifier;
        if msg.x_uuid == target_uuid {
            delete_note(id);  // orphan with our UUID
        }
    }
}
```

Why this is necessary: Jodd's `existing_gmail_id` in its in-memory store
goes stale the moment Apple edits the same note (Apple's IMAP APPEND
creates a new id; our delete-old then targets a dead id). Without cleanup,
every Apple-edit-followed-by-Jodd-edit cycle would leave a duplicate.

Cost: ~3 extra HTTP round-trips per save (list + N metadata gets + M
deletes). For small mailboxes this is sub-second. Worth it.

### Defense layering

```
Layer 1: body extraction with multipart fallback
         (defends against empty editor display)
            ↓ if that fails
Layer 2: data-loss guard in autosave
         (refuses save if new content < 25% of stored)
            ↓ if a save still slips through
Layer 3: dedupe-on-read (longest body wins)
         (user sees correct content regardless of stored duplicates)
            ↓ in parallel
Layer 4: cleanup-on-save (sweeps orphans)
         (storage layer converges to one-message-per-UUID over time)
```

No single layer is perfect; combined they make data loss essentially
impossible under any single bug.

---

## 8. Caching and freshness

### Label map cache (5-minute TTL)

`labels.list` is called once per account every 5 minutes. The result is
cached in `AccountState.label_map_cache`. This eliminates one HTTP
round-trip on every poll and every save.

**Self-healing on stale cache:**

```rust
// list_notes
let mut result = gmail::list_notes(&token, &cached_label_map).await?;
if result.is_empty() {
    let fresh = gmail::get_label_map(&token).await?;
    if fresh != cached_label_map {
        update cache;
        result = gmail::list_notes(&token, &fresh).await?;
    }
}
```

This catches the "Apple Notes deleted+recreated the Notes label, so the
cached Label_xxx is dead" scenario. Without self-healing, the user would
see zero notes for up to 5 minutes after such an event.

### Access token cache (in-memory only)

Access tokens are cached in `AccountState.access_token`. They're not
persisted to disk — on app restart they're refreshed from the keychain-
stored refresh token. Lifetimes are ~1 hour but Jodd doesn't track
expiry; we just rely on the next API call returning 401 to trigger refresh
(not currently implemented — long-running sessions may hit token expiry
once a day and require a one-time error).

### Background polling

```
60-second interval polling   (when window is focused)
    ↓ AND ↓
focus-event refresh         (Tauri onFocusChanged + visibilitychange)
    ↓ AND ↓
activity-event refresh      (mouseenter + keydown, 10s throttle)
    ↓ AND ↓
manual refresh button       (always available)
```

All paths go through the same `loadNotes` function. The 10-second
throttle on activity-events prevents mousemove storms from spamming
refreshes. Polling pauses entirely when the window is unfocused (saves
battery and quota).

### "Don't disturb the user while typing"

When external content arrives via polling, Jodd re-renders the editor's
DOM **only if the user isn't currently typing**:

```ts
const userIsTyping = document.activeElement === editorEl;
const shouldRender = uuidChanged || (bodyChanged && !isSaving && !userIsTyping);
```

If the user is mid-edit when Apple's version arrives, Jodd's local edit
wins (last-writer-wins per UUID). True collaborative editing would require
OT/CRDT — out of scope for v0.1.

---

## 9. Data-loss defenses (full list)

| # | Defense | Layer | What it prevents |
|---|---|---|---|
| 1 | Body extraction multipart fallback | Read | Empty editor for Apple-edited notes |
| 2 | Mojibake recovery for legacy bad Subjects | Read | Garbled titles from pre-RFC-2047 Jodd saves |
| 3 | UUID canonicalization | Read | Mismatched UUIDs from pre-fix hyphen-stripping |
| 4 | Self-healing label cache | Read | Zero-result lockout after label recreation |
| 5 | Dedupe by X-UUID, longest wins | Read | Wrong content shown when duplicates exist |
| 6 | Title-injection idempotence | Write | Repeated saves don't accumulate `<div>title</div><div>title</div>…` |
| 7 | Title strip + re-inject round-trip | Read+Write | Apple Notes' title display continues to work |
| 8 | Content-adaptive encoding (us-ascii vs utf-8) | Write | Spec-compliant; subject doesn't corrupt for non-ASCII |
| 9 | RFC 2047 Subject for non-ASCII | Write | Apple Notes / Gmail web don't mis-decode |
| 10 | `internalDateSource=dateHeader` | Write | Apple's "latest revision wins" reconciliation correct |
| 11 | Insert-then-delete ordering | Write | Network failure leaves 2 copies, not 0 |
| 12 | Status-checked delete with retry | Write | Transient propagation lag doesn't leak orphans |
| 13 | Cleanup-on-save for stale duplicates | Write | Eventual consistency: 1 message per UUID |
| 14 | Data-loss guard in autosave (>75% shrink) | Frontend | Empty editor + accidental keystroke can't wipe stored content |
| 15 | `clearTimeout(saveTimer)` on note switch | Frontend | Stale timer from note A can't fire on note B |
| 16 | Don't fire autosave from `onTitleKeydown` navigation keys | Frontend | Arrow keys / Tab don't trigger saves |
| 17 | External-update detection in editor | Frontend | Polling refreshes show Apple's edits without disturbing active typing |
| 18 | Capability permissions for focus events | Tauri | Without these, Tauri silently drops focus listeners |
| 19 | Apple's `X-Mail-Created-Date` preservation across saves | Write | Original creation time stays put |

---

## 10. Known limitations

### Race window: edit-vs-poll

If you're typing in Jodd when Apple Notes' edit arrives via polling, the
external version is silently ignored to preserve your active edit. When
you save, your version overwrites Apple's edit (last-writer-wins per
X-UUID). True merge is not implemented.

**Mitigation:** in practice, users rarely edit the same note simultaneously
on two devices. The cleanup pass + dedupe-on-read ensure no permanent data
loss, just temporary divergence resolved on next save.

### Label map staleness window (≤ 5 min)

If you create a new sub-folder in Apple Notes, Jodd won't see it until
the cache expires (5 minutes). The self-healing zero-result retry catches
the case where labels are deleted/recreated.

### No background sync when window closed

Jodd only polls when the window is open and focused (or recently was).
Closing the window stops all sync activity. Notes edited on iPhone won't
appear until Jodd is next opened. This is intentional (no daemon) but
limits "always up to date" semantics.

### Token expiry not proactively handled

Access tokens expire after ~1 hour. Jodd relies on the next API call
failing with 401 to trigger refresh — but the current code doesn't catch
401 specifically. In practice this means a session left open overnight
may show errors on first morning interaction; closing and reopening
refreshes cleanly.

### Apple's `Received:` header divergence

Apple Notes' IMAP APPEND doesn't add `Received: from … HTTPREST` headers;
Gmail's REST API auto-adds them on `messages.insert`. Apple Notes appears
to not parse this, so it's cosmetic, but a strict reader would distinguish
Jodd writes from Apple writes by this header.

---

## 11. What we deliberately don't do (and why)

### No SQLite cache (yet)

Every read goes to Gmail. Every cold start re-fetches. This is a v0.1
choice — keeps the code simple and Gmail as the unambiguous source of
truth. v0.2 will add a SQLite cache with `history.list` deltas for:
- Instant cold-launch
- Offline read access
- Full-text search via FTS
- Near-zero polling cost

### No IMAP backend

Apple Notes itself syncs via IMAP IDLE. Jodd uses Gmail REST instead
because:
- OAuth 2.0 is simpler than XOAUTH2 over IMAP
- REST avoids the long-running connection lifecycle
- Gmail's quota system is easier to reason about than IMAP rate limits

The tradeoff: we add a `Received: ... HTTPREST` header Apple doesn't,
and we may write subtly non-byte-perfect formatting. So far this has not
caused observable interop bugs, but it's a known divergence.

### No `users.history.list`

The Gmail History API would let us fetch only deltas since the last sync
(2 quota units regardless of mailbox size). We don't use it yet because
without a local cache there's nothing to delta-from. Both will land
together in v0.2.

### No multi-provider yet

Microsoft 365 / Exchange has its own equivalent of the Notes IMAP
convention. Jodd's gmail.rs is currently Gmail-specific; a v0.2 refactor
will introduce a `MailProvider` trait so MS365 and Gmail can coexist.

---

## 12. Quick lookup: which file does what

| File | Responsibility |
|---|---|
| `src-tauri/src/lib.rs` | Tauri commands, AppState, account routing |
| `src-tauri/src/auth.rs` | OAuth + PKCE + callback server |
| `src-tauri/src/accounts.rs` | Account list persistence + per-account keychain |
| `src-tauri/src/gmail.rs` | Gmail REST calls, message format, dedup, cleanup |
| `src-tauri/src/build.rs` | `.env` → compile-time env injection |
| `src-tauri/capabilities/default.json` | Tauri 2 permission grants (event listeners, etc.) |
| `src/App.svelte` | Top-level orchestration + polling |
| `src/lib/components/NoteEditor.svelte` | Contenteditable + autosave + data-loss guard |
| `src/lib/components/NoteList.svelte` | List + cross-folder search + sort |
| `src/lib/components/Sidebar.svelte` | Folder tree + account sign-out |
| `src/lib/stores/notes.ts` | Svelte stores: notes, accounts, currentAccount, etc. |
| `src/lib/types.ts` | TypeScript interfaces (Note, Account, Folder) |

---

## 13. Adding new defenses / changing the format

If you find a new failure mode that needs defending:

1. **First, write a test case** — capture the exact `.eml` from Gmail
   showing the failure, the screenshot showing the user-visible symptom,
   and the terminal log showing what Jodd did.
2. **Decide which layer to defend at** (read vs write vs frontend) per
   the principle: defenses belong wherever you have the most context.
3. **Document the defense in this file** — add a row to the table in §9,
   describe what it prevents.
4. **Don't remove existing defenses just because they overlap** — that's
   defense-in-depth working as designed. They have non-zero failure rates
   individually; combined they're robust.

If you need to change the on-the-wire message format:

1. **Capture an Apple-native .eml first** (export from Gmail web). Diff
   against what Jodd currently produces. Decide which differences matter.
2. **Test bidirectional round-trip** — write from Jodd, edit in Apple
   Notes, verify X-UUID is preserved (single message in Gmail, not two).
3. **Update §4 (Message Format) in this file** with the new headers.

---

## References

- [Google OAuth 2.0 for Desktop apps](https://developers.google.com/identity/protocols/oauth2/native-app)
- [Gmail REST API reference](https://developers.google.com/gmail/api/reference/rest)
- [RFC 7636: Proof Key for Code Exchange (PKCE)](https://www.rfc-editor.org/rfc/rfc7636)
- [RFC 2047: MIME Part Three: Message Header Extensions for Non-ASCII Text](https://www.rfc-editor.org/rfc/rfc2047)
- [RFC 2045: Quoted-Printable Content-Transfer-Encoding](https://www.rfc-editor.org/rfc/rfc2045)
- [atotto/apple-notes-imap](https://github.com/atotto/apple-notes-imap) —
  reference implementation of Apple Notes' IMAP message format (Go)
