# iCloud — Vertical #4, Milestone 1 (read-only)

> Status: **design / approved** (2026-08-21). Adds a fourth backend vertical
> (CloudKit's private database web service for `com.apple.notes`) behind the
> existing trait surface. M1 is deliberately read-only, and it is the **first
> vertical with no write path at all**.
>
> Builds on:
> - [HANDOFF-2026-08-21-icloud-m1.md](../HANDOFF-2026-08-21-icloud-m1.md) — the brief
> - [docs/PRIOR-ART.md](../../PRIOR-ART.md) — the survey, the probe, the measurements
> - [src-tauri/examples/icloud_probe.rs](../../../src-tauri/examples/icloud_probe.rs) — working Rust for the read path
> - [2026-08-14-microsoft-vertical-design.md](2026-08-14-microsoft-vertical-design.md) — the M1/M2/M3 shape to copy
> - [2026-08-19-account-identity-design.md](2026-08-19-account-identity-design.md) — `{backend}:{email}`, which unblocked this
>
> **Acceptance bar:** an iCloud-native Apple ID can be added from the real UI, its
> notes and folders appear in the sidebar and list with real titles, real folder
> nesting and real body text, every write affordance is absent rather than broken,
> and every existing test stays green. `cargo test --workspace` — not a narrower
> gate.

## Goal & framing

Jodd's three shipped verticals all reach Apple Notes through an **email backend**
— a `Notes` label in Gmail, an Exchange mailbox, or `.eml` files on disk. That
backend only exists for a user who attached a non-iCloud account to Apple Notes.
**A user whose notes live in iCloud is unreachable by construction**, and that is
most Apple Notes users.

Feasibility is not in question. On 2026-08-18 the door opened on all three locks
— auth without Playwright, CloudKit answering, content readable — and
`cargo run --example icloud_probe` printed real titles, folders and timestamps
from Jodd's own Rust with no Node, no Playwright and no protobuf. Everything
below is engineering on top of measurements.

**This is a third protocol family, not the same trick against another host.**
Gmail is REST over RFC822 messages; Microsoft is Graph over Exchange objects;
CloudKit is a record store whose note bodies are Apple's native CRDT document.
Four consequences shape this design:

1. **There is no HTML anywhere.** `mime822.rs` is unused, and so is every
   assumption that a body arrives as markup. The body is base64 → gzip *or*
   zlib → protobuf, and Jodd's entire content model (`AppleHtmlDeriver`, FTS,
   `note_tags`, `edges`, the editor) needs HTML. **The transport is the cheap
   part; the content model is the project.**
2. **Auth is not OAuth.** There is no refresh token, no client secret, no
   loopback listener. There is a browser session that expires in hours and
   rotates its own cookies.
3. **Identity is a lowercase `recordName`**, which is the one shape Jodd's
   shared save path actively corrupts today (Component F).
4. **Advanced Data Protection can make an account permanently unreadable.**
   That is a first-class state, not an error.

M1 exercises no write path whatsoever, which is what keeps the divergences above
from straining the trait surface. Land a usable read-only vertical; let M2 supply
real evidence about whether `Transport` needs reshaping.

## What already exists and does not need building

Checked in the code on 2026-08-21, before designing anything:

- **A fully read-only backend is already expressible end to end.**
  `Capabilities::for_backend` → `Writes` → `refuse_write` (lib.rs:1265) guards
  every write command across 8 call sites, and the frontend already has
  `canWriteAccount` (`notes.ts:141`) with tests that pass
  `writes: { notes: false, folders: false, sidecars: false }`
  (`canWriteAccount.test.ts:8`, `newNoteFn.test.ts:156`). **iCloud M1 is the
  first consumer of a case this layer was built to hold.** No new mechanism.
- **`{backend}:{email}` shipped**, so `BackendKind::ICloud` is purely additive
  and an Apple ID that is also a Gmail address no longer collides.
- **Real folder nesting costs nothing to support.** `folders.path` is already a
  `/`-joined hierarchy and every subtree query
  (`label = ?1 OR label LIKE ?1 || '/%'`) already works on it. gotcha #9's
  hardcoded `"Notes"` in five call sites is an *asset* here: Apple's default
  CloudKit folder is titled exactly `Notes`.

## Decisions locked in brainstorming (2026-08-21)

Eight decisions. The first three were named as open in the handoff; the rest
surfaced from reading the code and are recorded here so they are not rediscovered.

1. **Identity: a declared per-backend canonicalization policy.** Not a deleted
   call site, not a new `remote_id` column. Component F.
2. **Session: Jodd holds no copy of the cookie jar.** Harvest just-in-time from
   the live webview on every CloudKit burst. Component B.
3. **ADP: refuse at sign-in, plus a runtime per-account banner**, and never
   cache an unreadable record. Component H.
4. **Content: `prost` + vendored `.proto` + committed generated code**, with a
   drift check. No `protoc` at build time. Component E.
5. **Folders: build the real tree**, `Notes/A/B`. Component G.
6. **`has_trash: false` in M1.** The Trash folder is readable, but a Recently
   Deleted view whose Restore button always refuses is the "always-empty view
   reads as a sync bug" problem in a new costume. It becomes `true` in the
   milestone that can restore.
7. **The sync worker is not touched**, matching the Microsoft M1 precedent.
   Refresh is the ⟳ button and sign-in indexing. The `syncToken` is used inside
   an index run and kept in memory; persisting it to `accounts.sync_cursor` is
   M2.
8. **Exactly one iCloud account per install in M1**, refused at add time with a
   clear message.

   **Amended 2026-08-21, and the reason changed even though the decision did
   not.** The premise was wrong: macOS `WKWebView`'s data store is app-wide only
   by default, and Tauri 2.11 exposes `data_store_identifier` for a per-webview
   persistent store on macOS 14+ (verify-first #3). So the limit is a **scope
   choice**, not a platform constraint — lifting it means feature-detecting the
   OS version, binding an identifier to each account, and cleaning the store up
   on account removal, none of which a read-only milestone needs.

   The refusal message and the code comment must therefore say "not yet",
   never "not possible". Component A3's one-function shape is what keeps that
   cheap to reverse.

## Component A — account model

### A1. `BackendKind::ICloud`

`accounts.rs` gains a fourth variant. Three places must move with it, and the
existing `all_backends_lists_every_variant` test (accounts.rs:1300) is what
enforces the third:

| Site | Change |
|---|---|
| `backend_prefix` | `ICloud => "icloud"` |
| `ALL_BACKENDS` | length 3 → 4 |
| `vertical_for` (lib.rs:262) | new match arm |

`account_id_for(ICloud, apple_id)` yields `icloud:jodd.demo@gmail.com`.
`Account.email` holds the `appleId` that `/validate` returned — the only string
ever shown to the user as an address.

### A2. `is_ready_local()` — a marker, never the webview

gotcha #15 is binding here. `is_authenticated` is polled every 2 s during
sign-in, and `is_ready_local(&self) -> bool` is synchronous. A webview cookie
read is async and main-thread-bound, so it **cannot** be in this path at any
price.

`Account` gains one field:

```rust
#[serde(default)]
pub icloud_session_established: bool,
```

Set when sign-in completes; cleared when a revival attempt fails (Component B4).
It is a marker, not a credential — no secret goes in `accounts.json`. A stale
`true` is accepted for the same reason `RT_PRESENT`'s stale `true` is: the first
CloudKit call surfaces the loss, which is exactly what a revoked token already
does. Do not "fix" it by asking the webview per call.

### A3. One account, refused at add time

Adding a second iCloud account is refused before anything is persisted, with a
message naming the reason rather than a generic failure.

**The reason is "M1 does not do this yet", not "macOS cannot".** wry *does*
expose `WKWebsiteDataStore(forIdentifier:)` and Tauri surfaces it as
`data_store_identifier` (verify-first #3) — the limit is scope, per decision 8's
amendment. Wording that claims a platform limitation would be false, and would
send whoever lifts it looking for a workaround that is not needed.

Deliberately one function, not a design assumption spread across the vertical,
so deleting it is a one-line change when the milestone that wants multi-account
arrives.

## Component B — `icloud_auth.rs`: sign-in, harvest, revival

Nothing in `auth.rs` or `auth_ms.rs` is reusable. There is no PKCE pair, no
token exchange, no loopback listener — only a browser session.

**The principle, borrowed from icloud-md and stated so it is not eroded: own
none of `idmsa.apple.com`. Depend only on the result.** Apple's own JavaScript
runs whatever this month's password / 2FA / CAPTCHA flow is, inside a real
webview, and Jodd reads the outcome.

### B1. Sign-in flow

1. User picks **iCloud** → a **visible** `WebviewWindow` opens on
   `https://www.icloud.com/`.
2. User signs in normally. Jodd does not touch the page's form fields, ever.
3. Rust polls: harvest cookies → `POST /setup/ws/1/validate` → check the
   completion discriminator (B2).
4. On success, read `dsInfo.dsid`, `dsInfo.appleId`, and
   `webservices.ckdatabasews.url` — the partition host (`p149-…` for the probe
   account) that **cannot be guessed**.
5. Run the ADP readability check (Component H) *before* persisting anything.
6. Create the account, close the window, index.

### B2. The completion discriminator — status code is not enough

A password-accepted-but-2FA-pending session answers **HTTP 200** and even
carries the `ckdatabasews` entry. Closing the window on a 200 shuts it in the
user's face at the 2FA screen. The test is:

```
hsaChallengeRequired !== true   (at the top level AND under dsInfo)
AND webservices.ckdatabasews.url is present
```

`icloud_probe.rs::validate` already implements exactly this; port it, do not
re-derive it.

### B3. Harvest just-in-time — the cookie jar is never copied

**Jodd stores no cookie.** macOS `WKWebView` persists its data store inside the
app container already; that store *is* the session. Before each CloudKit burst,
Rust harvests from the live webview and uses the result for that burst only.

> ### ⚠️ Correction (2026-08-21): harvest with `cookies()`, never `cookies_for_url()`
>
> This paragraph originally named `cookies_for_url("https://www.icloud.com")`.
> **That call returns none of Apple's session cookies.** Read from wry 0.55 and
> `cookie` 0.18, before writing any of Component B:
>
> - `cookies_for_url` filters with `cookie.domain() == url.domain()` — exact
>   string equality (`wry/src/wkwebview/mod.rs:1181`).
> - `cookie::Cookie::domain()` strips a leading dot (`cookie/src/lib.rs:781`),
>   so a `.icloud.com` cookie reports `icloud.com`.
> - `url::Url::domain()` for `https://www.icloud.com` is `www.icloud.com`.
>
> `"icloud.com" != "www.icloud.com"`, so **every domain-scoped cookie is
> dropped** — and Apple's session cookies must be domain-scoped, because the
> same session is used against `setup.icloud.com` and
> `p<N>-ckdatabasews.icloud.com`.
>
> Use `WebviewWindow::cookies()`, which Tauri documents as returning "all
> cookies in the runtime's cookie store for all URLs **including HTTP-only and
> secure cookies**", and do RFC 6265 domain matching in Jodd — host equal to
> the cookie domain, or a subdomain of it. Do **not** "fix" this by asking for
> the bare `https://icloud.com` instead: that happens to match, by accident,
> and still drops host-only cookies set on `www`.
>
> `examples/icloud_webview_probe.rs` calls both side by side so the difference
> is measured on a real jar rather than argued from source.
>
> **Measured 2026-08-22 (macOS 26.6.2), and it is worse than the source-read
> suggested:** `cookies_for_url("https://www.icloud.com")` returned **0** of
> Apple's cookies where `cookies()` returned 17. Apple scopes them
> `Domain=icloud.com`, which RFC 6265 says MUST be sent to `www.icloud.com` —
> so the exact-host filter is confirmed, and it disqualifies the call for
> `p<N>-ckdatabasews.icloud.com` too. See verify-first #3b.
>
> **And do not filter by cookie NAME either.** The same jar carries two
> `X_APPLE_WEB_KB-…` cookies — underscores, not hyphens — alongside fifteen
> `X-APPLE-…` ones, all HttpOnly and Secure. Send everything that
> domain-matches, the way a browser does; a name-shaped rule silently loses
> two 220-byte session cookies.

This is the direct answer to the measurement in PRIOR-ART: a session captured at
15:58 was dead at 19:33 (HTTP 421), *because the live browser rotates cookies and
invalidates the copy*. **A copy is stale by construction, so do not hold one.**
A stored jar in the keychain was rejected for this reason — it looks symmetric
with Gmail's refresh token but it is not the same kind of object.

Giving the icloud.com webview an IPC channel and letting its JS do the fetching
was also rejected: it hands Apple's page Jodd's IPC surface, and the cookie
problem it solves is already solved above.

**Cookies are never logged, never written to disk, and never included in a
bug report.** `icloud_probe.rs` already says "the cookie jar is never printed";
that is a rule, not a courtesy.

**One property is unavailable through Tauri and the loss is chosen, not
overlooked.** WebKit reports `.icloud.com` for a domain cookie and
`www.icloud.com` for a host-only one, but `cookie::Cookie::domain()` strips the
leading dot (cookie 0.18, lib.rs:781) and `domain_raw()` cannot recover it for a
cookie that was *built* rather than parsed — which is how wry constructs them.
So `HarvestedCookie::host_only` is `false` for everything harvested live.
That over-sends within `*.icloud.com` rather than under-sending: every host
involved is Apple's own, while a missing session cookie is a dead request. The
field exists so the rule stays correct and pinned by tests, and so a future
Tauri that exposes the flag needs no new logic.

### B4. Revival

`TransportError::Auth` (from a 421, or a `/validate` that stops being fully
signed in) triggers:

1. Navigate a **hidden** webview to `https://www.icloud.com/`.
2. Let Apple's JS re-authenticate silently against the persisted store.
3. Poll `/validate` with freshly harvested cookies until B2 passes, or timeout.
4. Timeout → clear `icloud_session_established`, surface a **visible** sign-in
   window through the existing auth-loss path.

The hidden webview is created lazily and torn down when idle — an always-resident
`WKWebView` costs real memory for a session that is usually valid anyway.

### B5. Client version strings — read from the live session, via a cookie

The handoff forbids hardcoding `clientBuildNumber` / `clientMasteringNumber` /
`clientId` (the live session carried `2628Build44`; icloud-md hardcodes
`2624Build27`, and the drift its comment warns about has already happened). It
does not say how to read them inside a webview. The mechanism:

An `initialization_script` on the icloud.com webview wraps `fetch` and
`XMLHttpRequest`, captures the query parameters of the first request to
`setup.icloud.com`, and writes them back as a **cookie** on that origin:

```js
document.cookie = "jodd_client_cfg=" + encodeURIComponent(params) + ";path=/";
```

Rust reads it out of the same `cookies()` result that carries the session, so
there is one channel and no new surface.

**Why a cookie and not a custom-protocol `fetch`:** icloud.com ships a strict
CSP, and `connect-src` will block a fetch to an unfamiliar scheme.
`document.cookie` is not subject to CSP, and a WKWebView user script is not
subject to the page's CSP either. This choice is load-bearing; do not "simplify"
it into a fetch.

`ckjsBuildVersion` / `ckjsVersion` stay as named, isolated, loudly-commented
constants (PRIOR-ART practice #5) because they are not present in the setup
request's parameters — a documented maintenance tax rather than a hidden one.

## Component C — `backend/icloud/` (the vertical)

```
src-tauri/src/backend/icloud/
├── mod.rs      # ICloudVertical + trait impls
├── wire.rs     # CloudKit REST + JSON decode (changes/zone, records/lookup)
└── doc.rs      # topotext → text → HTML, and the title split
```

`ICloudVertical` implements `Transport + MetadataSidecar + NoteStore + Identity
+ Deriver`. `Deriver` delegates to the existing `AppleHtmlDeriver` — once the
body is HTML, FTS, tags, edges and citations all work unchanged, and search
spans iCloud alongside Gmail and LocalFs for free.

Every write method returns `TransportError::Permanent` naming M2. Two subtleties:

- **`MetadataSidecar::list_sidecars` returns `Ok(None)`, not `Ok(Some(vec![]))`.**
  The trait's contract is explicit: `None` means "the store is not initialized —
  the caller MUST NOT prune local state". `Some(vec![])` would tell the core to
  prune every local pin to nothing.
- **`NoteStore::list_trashed` returns `Ok(vec![])`** and the Trash folder is
  excluded from the tree, consistent with `has_trash: false`.

### C1. `SIDECARS_UNAVAILABLE_MSG` is wrong for this backend

The constant (lib.rs:1247) reads *"…aren't available on Microsoft accounts yet …
Editing notes and folders works normally."* On an iCloud account both sentences
are false. `write_refusal_for` must select the message by `BackendKind`, not just
by `Write` area. `NOTES_UNAVAILABLE_MSG` and `FOLDERS_UNAVAILABLE_MSG` are
already backend-neutral and need no change.

## Component D — field mapping

| `Note` field | Source | Note |
|---|---|---|
| `id` | `recordName` | |
| `uuid` | `recordName`, **verbatim** | lowercase; Component F |
| `title` | `TitleEncrypted` | base64 → UTF-8. Not encrypted, not compressed |
| `body_html` | `TextDataEncrypted` | base64 → zlib → protobuf → HTML; Component E |
| `version` | `recordChangeTag` | a real optimistic-lock token |
| `date` | `ModificationDate` | ms epoch |
| `x_mail_created_date` | `CreationDate` | ms epoch |
| `label` | derived folder path | Component G |
| `pinned` | `IsPinned` | see D2 |
| `attachments` | always empty | M1 does not fetch them |

`Note::version`'s doc comment enumerates each backend (Gmail: `id`, Microsoft:
`lastModifiedDateTime`, LocalFs: `date`) — add iCloud's row to it rather than
leaving a reader to infer it.

### D1. Dates go through `mime822::format_apple_date`

CloudKit hands back ms-epoch integers. Writing them into `notes.date` as raw
numbers would give this backend a date string shaped unlike every other
backend's, and the cache's dedupe/sort paths compare those strings. Convert to
the same Apple `Date`-header shape the other three produce. `mime822.rs` is
otherwise unused here; this one helper is not.

### D2. `IsPinned` is read, and `pin_dirty` is never set

`IsPinned` is a real field on the Note record, so for the first time Jodd's pin
column can reflect Apple's own pin instead of being purely Jodd-local. Read it.

**`pin_dirty` must never be set on this backend.** `has_pending_pushes` reads it,
the worker only leaves Draining when that returns false, and `remove_account`
refuses a Draining account — one un-pushable pin makes the account permanently
unremovable (gotcha #2's wedge). `writes.sidecars: false` already makes
`refuse_write` block `set_pin`/`set_pin_batch` at the command layer, and
`sidecars_supported` (lib.rs) already gates the worker's drain one layer below.
Both gates read `Capabilities`, so this comes out correct by construction —
stated here so nobody adds a fourth path around them.

Whether a *local* pin should ever write back is M2's question, not M1's.

## Component E — content: the CRDT document → HTML

### E1. Schema handling

Vendor `proto/*.proto` from icloud-md (MIT). They are **data, not code** —
`prost` compiles them directly, and MIT attribution is the only obligation.

**They are not in the npm package.** icloud-md publishes `dist/` only, so the
`.proto` sources PRIOR-ART describes are in the upstream git repo and absent
from the artifact. What `dist/` does carry is `notes/gen/*_pb.js`, and a
Protobuf-ES module embeds the serialized `FileDescriptorProto` of its source —
protoc's own parse, not a copy of the text — so the schema is recoverable
**exactly** rather than reconstructed from note samples. Each descriptor was
re-serialized and compared byte-for-byte before any `.proto` text was emitted.
Full method, and the one trap in it (`protoc-gen-es` strips the `dependency`
field and passes imports as a JS argument, so `crdt.proto`'s
`import "topotext.proto"` has to be recovered separately), in
`src-tauri/proto/PROVENANCE.md`.

**The generated Rust is committed to the repo, and `protoc` does not run at
build time.** `prost-build` 0.13 requires `protoc` on `PATH`, which would add a
new prerequisite to every developer machine, to CI, and — worst — to the Android
NDK cross-build, for a schema that changes approximately never. Instead:

- generated code lives in the tree and is reviewed like any other file;
- a **drift-check test** (PRIOR-ART practice #7) regenerates and compares.

**It never skips, and that is better than this spec first asked for.** The
original plan was "compare when `protoc` is present, skip with a clear message
when it is not" — but a guard that skips on the machine where the edit is made
is not a guard, and `protoc`'s absence is exactly why it is not in the build.
`protox` is a protobuf compiler written in Rust: it parses the `.proto` files
into the same `FileDescriptorSet` protoc emits, and `prost_build::Config::
compile_fds` turns that into Rust. `protox` + `prost-build` are dev-dependencies,
so the pair costs the normal build nothing and the check runs everywhere.

Generating and updating go through one function (`JODD_UPDATE_PROTO_GEN=1`
rewrites instead of comparing) rather than a test plus a separate generator
binary — two places that must agree about how output is produced is the defect
this test exists to catch, one level up.

Noticing when *upstream* changes their schema is a separate question this repo
cannot answer automatically; `proto/PROVENANCE.md` says so rather than implying
the drift check covers it.

`flate2` with the `rust_backend` feature (miniz_oxide) supplies zlib with no C
toolchain — the Android cross-build has enough vendored C already.

### E1b. ⚠️ Correction: the container is gzip *or* zlib, not zlib

**Established 2026-08-21, from icloud-md's own shipped source, while
implementing this component.** docs/PRIOR-ART.md, the M1 handoff and the first
draft of this spec all say the body is "base64 → zlib". That is true of the one
note the probe happened to read, and it is not the rule.

icloud-md's `noteText.ts` records the measurement:

> The compression container isn't determined by which endpoint served the
> record — it's whatever format the data happened to be stored in, which
> depends on whichever client last wrote it (observed both gzip, magic
> `1f 8b`, and zlib, magic `78 9c`, coming back from the exact same
> `changes/zone` endpoint for different records). Both are tried.

**This is not a detail, because of where the wrong version leaked to.** The ADP
detection in Component H originally keyed on "`TextDataEncrypted` does not
begin with the zlib magic `78 9c`". Under that rule every gzip-written note
reads as undecodable — and undecodable is precisely the ADP signature — so Jodd
would have refused a perfectly readable account at sign-in and told the user
their notes were end-to-end encrypted. A wrong constant, surfacing as a false
accusation the user cannot act on.

Two smaller consequences, both implemented:

- **Sniff gzip, and otherwise *attempt* zlib** rather than gating on `78 9c`.
  zlib's second header byte varies with compression level (`78 01`, `78 5e`,
  `78 9c`, `78 da` are all valid), so an equality test on `78 9c` rejects real
  notes written at another level. Let the decoder decide.
- **`flate2` must carry both**, which it does; the `rust_backend` feature keeps
  it pure Rust so the Android cross-build gains no new C.

### E2. Text → HTML

M1 decodes `topotext` for **visible text only**; attribute runs, tables and
embeds are M2. The handoff's rationale stands: notes that all open blank are
indistinguishable from a sync bug, and read-only means lossiness can never
propagate back.

The HTML shape is the one a `contenteditable` produces, so what the editor
renders matches what it would emit: each line escaped and wrapped in `<div>`,
an empty line as `<div><br></div>`. `AppleHtmlDeriver` then runs on it
unchanged.

### E3. ⚠️ `strip_leading_title` — the highest-risk function in M1

**This is the slot that has caused two data-loss bugs (gotchas #11 and #17), and
the handoff does not name it.**

`TitleEncrypted` is a separate field, which makes it tempting to assume the body
holds body-only text. It does not: in Apple's native format the note's first
line **is** the title, and `TitleEncrypted` is derived from it. So the decoded
body repeats the title as line one, and something has to remove it.

**No longer an inference (2026-08-21).** icloud-md's `noteText.ts` states that
what it extracts is *"the plain visible-text string (title + body, no
formatting)"*, and it carries a `noteTitleParagraph` module for the split. That
is a second independent source agreeing with the reasoning above — the same
kind of confirmation PRIOR-ART records three times for gotcha #11. It still
needs checking against a real note, because "the title is line one" does not
say what separates line one from line two, and gotcha #17 is entirely about
that separator.

**Now measured, not inferred (2026-08-22).** `examples/icloud_probe` read
**776 notes** on a live account (`kaiwan@me.com`) and compared every record's
`TitleEncrypted` against the first line of its decoded body. The result
overturned the rule this section originally specified:

| observation | count |
|---|---|
| the first non-empty line **is** the title, character for character | 520 |
| the line differs from the title | 215 |
| the note is a title with no body | 41 |

**27.7% of a real account differs**, and every cause is understood:

- the title is **truncated** past ~65 characters and gains `U+2026`
- an inline object (attachment, table, inline `#hashtag`) is `U+FFFC` in the
  body and rendered text in the title
- the first line of the text can be **empty** — the title is the first
  **non-empty** line
- `U+2028` (soft line break) is present in the body and removed from the title

So `TitleEncrypted` is a **lossy derived display string**, not ground truth.
It cannot be the cut key.

### The rule (implemented, `backend/icloud/doc.rs`)

```rust
pub fn strip_leading_title(text: &str, title: &str) -> StrippedBody
```

1. **Cut through the first non-empty `\n`-delimited line, always.** Apple's
   model is that a note's first line *is* its title; the title field is
   derived from it, not the other way round.
2. **Use `title` only to verify.** The returned `TitleMatch` records *which*
   measured cause explains the difference — `Exact`, `Prefix`, `Truncated`,
   `ContainsObject`, `SoftBreakRemoved` — with `Unexplained` as the signal
   that the four causes above have a gap. The classification never decides
   the cut.
3. **An empty `title` cuts nothing** (`NoTitle`). Apple shows no title for
   that record, so every line is body and removing one is pure loss.

**This reverses what this spec said before**, which was "if it does not match
exactly, leave the body alone" — written believing mismatches were rare. At
27.7% that rule would show a duplicated title on more than one note in four.
The read-only argument that motivated it still holds, in the *other*
direction: a wrong cut here is never written back, so the cost of cutting is a
cosmetic line, while the cost of not cutting is a visible defect on 215 of 776
notes.

Two consequences to carry into M2, both deliberate and both invisible in M1:

- Empty lines *above* the title are cut with it. "The title is the first line
  and the body is the rest" leaves no room for them.
- A note that is nothing but a title strips to an **empty body, correctly** —
  41 of the 776. Damage detection must not read that as corruption; an empty
  body is also gotcha #17's signature, and here it is the right answer.

M2 must revisit both **before its first push**, along with the `Unexplained`
arm, because that is the point where a wrong cut starts costing server-side
content.

## Component F — identity: a declared canonicalization policy

`save_note_db` (lib.rs:2483) runs every incoming identity through
`mime822::canonicalize_uuid`, which **uppercases**. A CloudKit `recordName`
(`f8bf619a-1b84-40eb-932d-6318ee9aeeb4`) parses as a UUID, so it would be
silently uppercased on the user's first edit, stop matching the record it names,
and take the gotcha-#16 rekey path for no reason. The symptom is a lookup that
404s only after the first save.

Microsoft survives this **by accident** — `internetMessageId` (`<…@…>`) does not
parse as a UUID, so `canonicalize_uuid` returns `None`. The fix turns that
accident into a declaration:

```rust
fn canonical_uuid_for(kind: BackendKind, raw: &str) -> String
```

| backend | policy | why |
|---|---|---|
| Gmail | uppercase Apple form | Apple reconciles `X-Universally-Unique-Identifier` by `strcmp`; the hyphen-stripped form read as a different note — "the first major interop bug we fixed" (docs/GMAIL-SYNC.md) |
| LocalFs | uppercase Apple form | same wire format |
| Microsoft | verbatim | `internetMessageId` is not a UUID |
| ICloud | verbatim | `recordName` is lowercase and case-sensitive |

`save_note_db` takes `BackendKind` as one additional parameter; the caller
already knows it. One test pins all four rows, so Microsoft's escape can never
silently become a regression and iCloud's lowercase can never silently become an
uppercase.

Rejected: **deleting the shared call site** as redundant (the Gmail/LocalFs wire
layers do canonicalize at `wire.rs:387/995` and `decode.rs:29`, so it may well
be dead — but removing a net whose catch is unproven risks the project's primary
backend for no gain here); and **a separate `remote_id` column** (a migration
and a rewrite of every uuid read, for a read-only milestone that does not need
it — and the Microsoft precedent already establishes that `uuid` may hold a
backend's native identity).

`Identity::mint()` returns a **lowercase** v4 UUID, matching CloudKit's shape,
even though M1 never creates a record.

## Component G — folders: a real tree

Unlike Microsoft (gotcha #12, where the folder tree is unenumerable and nesting
is unrecoverable), CloudKit returns `Folder` records carrying `ParentFolder`
references. The full hierarchy is available, so build it.

- Walk `ParentFolder` up to `DefaultFolder-CloudKit` and join titles with `/`.
- `DefaultFolder-CloudKit` maps to path `Notes`, so the sidebar and every
  subtree query behave exactly as on Gmail, and gotcha #9's five hardcoded
  `"Notes"` literals are correct here rather than a limitation.
- `TrashFolder-CloudKit` is excluded from the tree; its notes are excluded from
  listings, as are records with `Deleted == 1`.
- The CloudKit folder id is stored in `folders.label_id`, the same column that
  holds a Gmail label id and an Exchange folder id.

Two hazards, both cheap to close and both silent if missed:

- **A `/` inside a folder title** would forge a path segment. Replace it with
  `∕` (U+2215 DIVISION SLASH) on the way in — icloud-md's own visually-near
  look-alike trick, which PRIOR-ART already records for the file-export case.
- **A malformed or cyclic `ParentFolder` chain** must not spin forever. Cap the
  walk depth and fall back to filing the folder directly under `Notes`.

## Component H — Advanced Data Protection

With ADP on, `TextDataEncrypted` is genuinely end-to-end encrypted and there is
no readable content at any price. This is a "this account cannot work" state, not
an error.

### H1. Detection is evidence-shaped, not field-shaped

Nobody has an ADP account to test against, so the design must not depend on a
field nobody has seen. The verdict comes from what the records actually decode
to:

- `TitleEncrypted` does not base64-decode to valid UTF-8, **and**
- `TextDataEncrypted` decompresses in **neither** container.

**Read the second line off `DecodeError`, never off a magic number.**
`icloud/doc.rs` already splits its failures by what a caller should do about
them — `Unreadable` (not a compressed stream at all, which is what ADP content
looks like from out here) versus `Malformed` (it decompressed and then was not
a note document, so a bug or a schema change). The verdict consumes that split
and re-derives nothing. E1b is why: this check's first draft carried its own
copy of "the container is zlib", and that copy was wrong in the direction that
refuses working accounts. A `Malformed` note must never count toward an ADP
verdict.

```
Readable          ≥1 note decoded
NoNotes           0 note records in the zone
Unreadable { n }  n note records, 0 decoded
```

**`NoNotes` is the trap.** A brand-new account with zero notes decodes nothing,
and reporting that as ADP is a false accusation the user cannot act on. The
three-way verdict exists precisely to keep "0 notes" from collapsing into "0
readable".

### H2. Surfacing

- **At sign-in:** run the check before the account is persisted. `Unreadable` →
  an explanatory screen naming ADP and what it means; **no account is created**,
  so the user is not left with a permanently broken entry to clean up.
- **At runtime:** ADP can be switched on after the account exists, so a
  per-account `blocked_reason` (the same shape as `notes.push_blocked_reason`,
  migration #17) drives a banner. It generalizes to any future backend-level
  hard block.

### H3. An unreadable record is never written to the cache

Not as an empty-bodied note, not as a title-only stub. **This is gotcha #17's
exact shape**: a note cached with an empty body is a landmine that M2 detonates
the moment the user edits it and the worker pushes that emptiness back over
content that was fine on the server. M1 has no write path, so the landmine would
sit there quietly until the milestone that arms it.

Skip the record, count it, report the count. Same discipline as gotcha #14's
"do not lie, and do not delete".

#### H3 amended, 2026-08-23 — a LOCKED note is shown, with a placeholder body

The rule above was written for a record nothing could be read from. A
`PasswordProtectedNote` turned out not to be that record, and the amendment is
what the measurement forced.

**Measured live**, two locked notes on `kaiwan@me.com`:

```
fields: ["CreationDate","Deleted","Folder","Folders",
         "MinimumSupportedNotesVersion","ModificationDate",
         "TextDataEncrypted","TitleEncrypted"]
title: readable, 40 char(s)  /  title: readable, 6 char(s)
```

Only `TextDataEncrypted` is beyond reach. Title, folder and both dates come
back intact — so the note can be placed in the right folder, under its own
name, with its own timestamps, and only the body is missing.

Counting alone was the right answer while it was unknown whether *anything*
about the record was legible. It is not enough once it is: a folder Apple
showed with two notes showed one in Jodd, and nothing in the UI said why. The
count reached the log; it never reached the user. Skipping a record that can be
described is the same defect the count was introduced to fix, one layer up.

So `wire::decode_locked_note` builds the note and `wire::LOCKED_BODY_HTML`
stands in for the body — a sentence naming what happened and where to go read
it. `DecodeTally::locked` still counts.

**Why this does not re-arm the landmine, and what M2 owes.** The danger in
gotcha #17 is *silent* emptiness: a body that looks like content the user
simply hasn't typed, which a later write pushes back over text that survived
on the server. A visible sentence is the opposite of silent — but that is an
argument about the *user*, not about the worker, and the worker cannot read.
**M2 must refuse the push by record type, not by inspecting the body.** The
remote record is a `PasswordProtectedNote`, not a note with an empty body;
a guard that keys on `LOCKED_BODY_HTML` would be a string comparison standing
in for a type check, and would pass the moment the user edits the placeholder.
The refusal belongs where the record type is still known.

A locked note in the Trash is skipped like any other deleted record.

## Component I — capabilities

```rust
BackendKind::ICloud => Capabilities {
    folder_model: FolderModel::SingleExclusive,
    fidelity: Fidelity::Full,
    // Inert while `writes` is all-false. Recorded as the truthful future:
    // CloudKit `records/modify` updates in place, so the Gmail
    // insert-new + trash-old sequence must never be copied here.
    save_semantics: SaveSemantics::InPlaceUpdateNeedsExplicitMove,
    // The Trash folder is readable, but restoring is a write. A Recently
    // Deleted view whose only button always refuses is worse than no view.
    has_trash: false,
    // The first fully read-only backend.
    writes: Writes { notes: false, folders: false, sidecars: false },
},
```

No new field. This milestone's whole job on the capability layer is to be the
case it was designed for.

## The walk's diagnostics stay until M3 — a decision, not leftovers

Decided 2026-08-23, after the live pass. Every census below earns its keep by
having already caught something no test could, and M2 (write) and M3 will be
asking the same *kind* of question against the same opaque wire. Removing them
now costs a re-derivation later; leaving them costs a few log lines per walk.
**Remove them in one pass when M3 closes, not opportunistically** — a tidy-up
that deletes one of these while a milestone is still open is deleting the only
instrument pointed at that milestone.

| Line | The question it answers | What it caught |
|---|---|---|
| record type census | what is in the zone besides notes and folders | the pin is on `Note_UserSpecific` |
| root field census | which fields are not universal among root notes | `IsPinned=0` — absent, not merely unread |
| per-user field census | do Apple's field names match what `collect_pins` expects | would have said so; `0 with no note reference` confirmed they do |
| `N pinned … reached the result` | which notes ended up pinned, and where | separated the over-count from the miss |
| `arrived more than once` | is the walk double-counting | the 6 duplicate records (gotcha #22) |
| `list_notes` / `list_cached_notes` pin counts | does the pin survive the write/read boundary | the `from_remote` layer |

Two properties they all share, and any diagnostic added later must too. They
print **names and counts, never values** — a census that could leak note text
by meeting an unanticipated shape is not runnable against a real account, and
these had to be. And each reports a number that can *disagree with Apple's*,
rather than reporting a hypothesis: three hypotheses about the note count were
refused by measurement before the right one, and none of them would have been
refutable by a log line that said what the code believed.

### I1b. The pin is the first per-backend READ policy, 2026-08-23

`Capabilities` describes what a backend can be WRITTEN to. The pin needed the
mirror question — whether a READ is authoritative — and it does not belong in
`Capabilities` either, because it is consumed inside `db.rs` where no
`Capabilities` is in hand and a `Db` method has no business knowing about
backends.

`db::RemotePin` is therefore a required argument to `upsert_from_remote`, with
`db::remote_pin_policy(BackendKind)` stating the answer per backend and **no
wildcard arm**. Same discipline as `canonical_uuid_for` (gotcha #18) and for
the same reason: the failure is silent in both directions. `LocalWins` on
iCloud means the pin never arrives; `RemoteWins` on Gmail means a pull unpins
notes the user pinned by hand, and nothing reports it.

`LocalWins` is the fallback when the backend cannot be determined (an account
removed mid-reconcile): a pin that fails to arrive is a missing feature, a pin
overwritten by a guess is lost user intent.

### I2. Locked notes name a third axis — and it does NOT belong in `Capabilities`

Asked directly (2026-08-23) whether locked notes break the capability design.
They do not, and the reason is worth writing down because the tempting fix is
the wrong one.

`Capabilities::for_backend` answers **per backend, statically** — derived from
`BackendKind` alone, no token, no vertical, no network. That is what makes it
safe to call from the command layer and mirror in the UI. There are two other
axes it deliberately does not cover:

| Axis | Decided by | Where it lives |
|---|---|---|
| per **backend**, static | `BackendKind` | `Capabilities::for_backend` |
| per **account**, discovered | a live probe (ADP) | `Account.blocked_reason` (H2) |
| per **note**, discovered | the record itself (locked) | the note row |

A locked note is the third row: two notes in the same folder of the same
account differ, so nothing keyed on backend or account can express it.
Widening `Capabilities` to carry it would make a static function
account-dependent — the exact property that lets `refuse_write` call it — to
describe something that varies *within* an account anyway.

The per-note axis already has a home: `notes.push_blocked_reason` (gotcha #14)
is a per-row "this cannot be pushed, and here is why". M1 does not use it —
`upsert_from_remote` does not set it, and wiring it in would mean changing a
pull path every backend shares, for a milestone with no writes. **M2 is where
that connects**, and connecting it there is what satisfies the H3 amendment's
requirement above: the refusal is per-note, discovered on read, and recorded
on the row — not a capability, and not a string comparison against the
placeholder body.

## Component J — sync

`Transport::changes_since` is implemented honestly against `changes/zone`,
paging on `moreComing` exactly as `icloud_probe.rs::fetch_zone` does, with the
`syncToken` held in `AppState.account_states` for the run. **The sync worker is
not touched** (decision 7): refresh is the ⟳ button and sign-in indexing, which
is the same slice Microsoft M1 shipped.

One rule belongs in M1 even though incremental sync does not, because it is
correctness rather than optimization: **a `syncToken` the server rejects means a
full refetch, reported, not an abort.** icloud-md established the failure shape
by live probing — a zone-level `BAD_REQUEST` arrives *inside an HTTP 200* — and
a merely old token (15 days) still syncs incrementally. An implementation that
treats a rejected token as an error will fail rarely and confusingly.

Persisting the token to `accounts.sync_cursor` — deferred in CLAUDE.md since the
vertical split, and exactly what this token is — is M2's, alongside the worker
integration that would consume it.

## Component K — ⚠️ hashtags: REOPENED by the 776-note run

The original decision was: *CloudKit exposes `Hashtag` records; ignore them.
Tags are derived from the body by `AppleHtmlDeriver` on every backend, and
"derive, don't migrate" (gotcha #4) is what makes drift impossible.*

**The premise under it is false on this backend.** An inline hashtag is an
**inline object**, so `topotext.String.string` carries `U+FFFC` where the tag
is — the `#name` text is not in the body at all. It was found in E3's data: a
note titled `this note with #this tag` has `#` at index 15 of the title and
`￼` at index 15 of the body. `AppleHtmlDeriver` scans body text for `#word`,
so on iCloud it would find **nothing**, and `note_tags` would be empty for
every note the user actually tagged.

That is not a "derive vs migrate" question any more — deriving from the body
is deriving from a source that does not contain the fact.

Three options, undecided, and **none of them is in M1's scope**:

1. **Reconstruct the tag text from `attribute_run`.** The run that covers the
   `U+FFFC` position carries the attachment/inline-object metadata, so the tag
   name should be recoverable there. Keeps one derivation path, needs the
   attribute-run decode M1 deliberately skipped — i.e. it lands with M2.
2. **Read the `Hashtag` records after all**, as a per-backend source the
   deriver is handed. Cheapest to build, and it makes iCloud the one backend
   where a tag has a second source of truth — exactly what this component
   originally existed to prevent.
3. **Ship M1 with no tags on iCloud, stated.** Honest for a read-only
   milestone; a user who navigates by tag sees an empty result and no reason.

**M1 ships option 3 by default** — not as a choice, but because 1 needs
attribute runs and 2 needs a decision. Record it as a known limit in the
milestone notes rather than letting it read as a bug. Decide 1 vs 2 when
attribute runs land, since that is when option 1 becomes nearly free.

## Verification

### Unit-testable with no live account — write these first

| Target | Why it matters |
|---|---|
| **`strip_leading_title`** | ✅ **done** (12 tests). Highest-risk function in M1. One case per shape MEASURED on the 776-note run — exact, prefix, truncation-with-`U+2026`, `U+FFFC`, `U+2028` removal, a leading empty line, a title-only note, an empty title, and an `Unexplained` line that is still cut |
| `canonical_uuid_for` | All four backends in one matrix. Pins Microsoft's former accident and iCloud's lowercase |
| folder path construction | Nesting, `/` in a title, a cyclic `ParentFolder`, Trash exclusion, `Deleted == 1` exclusion |
| ADP verdict | Especially `NoNotes` vs `Unreadable` — the false-positive case |
| CloudKit JSON decode | Fixtures captured from real `changes/zone` responses |
| topotext decode | A real compressed body, byte fixture, asserting text out |
| proto drift | Regenerate and compare when `protoc` is available; skip loudly otherwise |

### Needs the live account (manual, recorded in the milestone notes)

- ~~Does the decoded body actually repeat the title?~~ ✅ **verified
  2026-08-22, 776 notes** — it does, and the title field turned out to be a
  lossy derivation of it rather than a copy. See E3 for the rule that measurement
  produced.
- ~~Does `ParentFolder` nesting appear as expected?~~ ✅ **verified 2026-08-22** —
  102 folders, 5 levels deep, and at least one folder title contains `/`, so
  G's path builder must escape rather than join naively.
- End-to-end sign-in, including the 2FA screen surviving the poll loop
- Revival after a real expiry — leave a session overnight and confirm the hidden
  webview recovers it without user action

### Containment before any live run

Adopt PRIOR-ART practice #1 now, not after: two independent keys must both turn
before any live test can delete anything, the suite refuses to start if the test
folder holds anything unprefixed, and the test folder is created by hand. M1 is
read-only so the blast radius is small today — but the account is the
maintainer's real Apple ID, and M2 arrives with writes.

### Gate — the commands CI runs, not narrower ones

```bash
cargo test --workspace
node scripts/gen-changelog.mjs
npx vitest run
npx svelte-check --threshold error
npm run build
```

## Verify before writing much code

Five assumptions that would reshape the design if wrong. **All five are now
answered (2026-08-21 from source, 2026-08-22 live).** Progress and method are
recorded here rather than in a separate log, so the next person reads one
document.

### ✅ 1. `cookies()` returns HttpOnly cookies on macOS — YES

Not `cookies_for_url` (see the correction in B3). `cookie_from_wkwebview`
(`wry/src/wkwebview/mod.rs:1070`) reads `isHTTPOnly()` and sets it on the
returned cookie, and `cookies()` applies no filter that could drop one; Tauri's
own doc comment says "including HTTP-only and secure cookies".

**Measured 2026-08-22, macOS 26.6.2, real signed-in jar:** 21 cookies, 17 of
them Apple's, **13 HttpOnly** — including `X-APPLE-WEBAUTH-TOKEN` (220 B),
`X-APPLE-DS-WEB-SESSION-TOKEN` (806 B) and nine `X-APPLE-WEBAUTH-PCS-*`, one
of which is `-Notes`. Harvesting through `cookies()` reaches the session.

**One thing the run added that no source-read would have: the names do not
share a prefix.** Two of Apple's cookies are `X_APPLE_WEB_KB-…` — **underscores**
— and both are HttpOnly, Secure and 220 bytes, so they are not decoration. A
filter written `starts_with("X-APPLE")` drops them silently, which the probe
itself was doing and is now fixed. **Component B must not filter by name at
all**: send whatever domain-matches, the way a browser does.

### ✅ 2. An `initialization_script` beats Apple's own JS — YES

**Measured 2026-08-22.** The injected wrapper observed Apple's own requests and
captured the client parameters off the first one — 119 bytes recovered from
`setup.icloud.com`'s query, read back out of the webview's own cookie jar.
Capturing them *is* the proof; there is no API that reports injection order.

**Consequence: B5 works as designed.** The version strings are read from the
live session, so icloud-md's stale hardcoded `2624Build27` never has to be
copied — which is what the handoff forbids and what the drift measured on the
live session (`2628Build44`) shows is already a live problem, not a
hypothetical one.

### ✅ 3. A per-webview persistent data store on macOS — YES, on macOS 14+

`WebviewWindowBuilder::data_store_identifier([u8; 16])` exists in Tauri 2.11.5
(`src/webview/webview_window.rs:1135`) and wry maps it to
`WKWebsiteDataStore::dataStoreForIdentifier` when
`os_major_version >= 14` (`wry/src/wkwebview/mod.rs:223`).
`App::fetch_data_store_identifiers()` and `remove_data_store_identifier` cover
the lifecycle.

**Measured 2026-08-22 on macOS 26.6.2**: a session was present on the probe's
**first tick, before the user signed in** — the store survived a previous run
under the same identifier. Persistence confirmed.

**So decision 8's limit is a choice, not a constraint** — see the amendment
below. One hazard comes with it: on **macOS 13 and older** wry falls back to the
shared default store **silently, with no error**, so two accounts would share a
jar with nothing to indicate it. Any future multi-account support must
feature-detect the OS version rather than assume — and note that a pass on an
old macOS proves nothing about isolation, which is why the probe prints the OS
version before its verdict.

### ✅ 3b. `cookies_for_url` does exact-host matching — CONFIRMED, and it rules the CloudKit host out

Not one of the original five; it came out of B3's source-read and the live run
settled it. Side by side on the same jar:

| call | cookies | Apple's |
|---|---|---|
| `cookies_for_url("https://www.icloud.com")` | 4 | **0** |
| `cookies_for_url("https://icloud.com")` | 17 | 15 |
| `cookies()` | 21 | 17 |

Apple's cookies carry `Domain=icloud.com`, and **RFC 6265 says a cookie scoped
to `icloud.com` must be sent to `www.icloud.com`.** It returned zero. So wry
matches `cookie.domain() == url.domain()` as exact strings, exactly as the
source read said.

**That is not a `www` quirk — it disqualifies the call for the transport.**
CloudKit is served from `p<N>-ckdatabasews.icloud.com`, another subdomain, so
`cookies_for_url` would hand the burst **zero session cookies**. Harvest with
`cookies()` and do RFC 6265 matching in Jodd. Asking for the bare
`https://icloud.com` is not the fix: it works by accident on today's jar and
still drops any host-only cookie Apple sets on a subdomain later — the four
`www` rows above are host-only cookies proving the shape occurs.

### ✅ 4. Does the decoded body repeat the title, and what ends that line? — YES, and the title field is lossy

**Answered by the 776-note run (2026-08-22, `kaiwan@me.com`).** The body does
open with its title; the separator is `U+000A`, with `U+2028` occurring *inside*
a line as a soft break. What the run also established — and what no small sample
could — is that `TitleEncrypted` is a **lossy derivation** of that line rather
than a copy, which reversed E3's rule. Full result and the rule it produced are
in **E3**; the first-run history is in `docs/PRIOR-ART.md`.

**A note that is only a title is a real shape, and it strips to an empty body —
correctly**, 41 of the 776. Damage detection must not read that as corruption:
an empty body is also gotcha #17's signature.

### ✅ 5. Does `ParentFolder` give real nesting? — YES

**Measured 2026-08-22**: 102 folders, **5 levels** deep. Unlike Microsoft
(gotcha #12), the tree is genuine and every existing subtree query works
untouched.

Two hazards the same run exposed, both for Component G:

- **At least one folder title contains `/`.** `folders.path` is a `/`-joined
  string, so a naive join invents a level that does not exist and two different
  trees can collide on one path. Escape, or the tree is wrong for that user.
- The earlier `jodd.demo@gmail.com` attempt could not answer this at all, and
  **the reason is worth keeping**: `icloud.com/notes` has no folder-creation
  affordance — the `+` sheet offers Email, Calendar Event, Note, Reminder,
  Pages, Numbers and Keynote, and no folder. An Apple ID reachable only through
  a browser has a folder tree frozen at whatever Notes.app already made.

## Implementation order

1. ✅ `BackendKind::ICloud` + `backend_prefix` + `ALL_BACKENDS` + `vertical_for`
   arm + `Capabilities` arm (read-only end to end, nothing to talk to yet)
2. ✅ `canonical_uuid_for` + `save_note_db` param + the four-row test
   (Component F) — independent of everything else and closes the known trap first
3. ✅ Verify-first list above — **all five answered**, both probes run live:
   - `cargo run --example icloud_probe` — #4 and #5, **run 2026-08-22 against
     776 real notes**; #4's answer rewrote E3 and reopened Component K
   - `cargo run --example icloud_webview_probe` — **run 2026-08-22 on macOS
     26.6.2**; #2 confirmed (B5's mechanism works), #1 and #3 confirmed live,
   and the `cookies_for_url` trap measured rather than argued (#3b)
4. 🔶 `icloud_auth.rs` — **landed 2026-08-22** (30 tests): the injected script,
   RFC 6265 cookie matching, the B5 config parse, the B2 discriminator, A3's
   refusal, and the visible-webview sign-in driver. It produces a validated
   `IcloudSession`.

   **It deliberately stops short of creating the account**, because B1's own
   ordering puts the ADP readability check (step 5) before persistence and that
   check needs the transport to fetch a note. Creating the account now would
   mean either persisting one whose notes may be undecodable, or writing the
   gate twice. The account-creation step lands with items 5 and 7.

   Everything decidable without a webview is a pure function over plain data —
   `HarvestedCookie` is this crate's own type, not `tauri::Cookie` — so the
   parts that fail quietly are tested on every machine, and only the driver is
   Mac-only.
5. 🔶 `icloud/wire.rs` — **landed 2026-08-22** (30 tests, fixtures first): the
   `changes/zone` request and envelope, the folder-tree builder (G) and the
   field mapping (D), plus `classify_status` — where **421 is `Auth`, not
   `Transient`**, because the jar rotates on its own and retrying the same
   cookies can only fail.

   `SkipReason` carries `doc::DecodeError`'s Unreadable/Malformed split intact
   so Component H consumes it rather than re-deriving compression magic; a body
   that is not even base64 is `Incomplete`, never `Undecodable`, so a malformed
   response cannot accuse an account of being end-to-end encrypted.

5b. 🔶 **Component C — the vertical** — **landed 2026-08-22** (18 tests, all
   against a mock CloudKit over a real socket). `ICloudVertical` implements
   `Transport + MetadataSidecar + NoteStore + Identity + Deriver`; every write
   returns `Permanent` naming M2, `list_sidecars` returns `Ok(None)` and
   `list_trashed` returns `Ok(vec![])`.

   Three things the implementation added that this spec did not call for, each
   because writing it exposed the question:

   - **`CookieSource`, a trait, not a webview call.** B3 says harvest
     just-in-time and hold no copy; a vertical that reached into `WKWebView`
     itself could only be tested on a Mac with a real Apple ID. The trait has a
     `StaticJar` for tests and a `WebviewJar` for production, and one test pins
     that the walk harvests **once per page** rather than once per walk — which
     is what "hold no copy" actually means when a walk spans minutes.
   - **A failed or empty harvest is `TransportError::Auth`.** Not `Transient`
     (the worker would retry a webview that is not coming back) and not an
     empty `Cookie:` header (CloudKit answers 421, and the cause then reads as
     "the session expired" rather than "the harvest found nothing").
   - **`DecodeTally`** — `decoded` / `unreadable` / `malformed` / `incomplete` /
     `deleted` kept apart on the scan, so **Component H consumes the split and
     re-derives nothing**. Collapsing any two accuses a readable account of
     being end-to-end encrypted (gotcha #20).

   One zone walk per **account**, not per vertical instance — and the
   difference is the whole point. CloudKit has **no per-folder endpoint**, so
   `list_notes_in_folder` cannot narrow the request the way a scoped Graph read
   does; it filters the scan. A vertical is built per operation, so a
   per-instance cache would leave the 2500 ms folder sweep walking the entire
   zone once per folder: **102 whole-zone reads in about four minutes** on the
   measured account, every session, against Apple's private API.

   `AccountCache` (held in `AppState.icloud_scans`, keyed by account) collapses
   that to one. The lock is held across the walk on purpose — two verticals
   racing would otherwise both walk, which is the duplication being removed.
   `list_notes`, the explicit refresh, invalidates first; a five-minute
   `MAX_AGE` is the backstop for paths that forget. And because a zone read
   returns every note in every folder, one walk hydrates the whole account —
   which is also why the sign-in gate's walk is the one that serves the
   indexing pass right after it.

   **The same cache holds the `/validate` bootstrap**, for the same reason one
   layer up: every vertical must establish a session before it can do anything,
   so without it the sweep would `POST /validate` every 2500 ms — about a
   hundred calls in one pass, on top of the zone reads. Nothing cached is a
   secret; the session itself still lives only in the webview's jar.

   **Holding a lock across an await has a cost, and it was paid once.** A 421
   handler that invalidated the *whole* cache from inside the walk took the
   scan lock a second time — a tokio mutex is not reentrant, so the read never
   returned: no error, no panic, no timeout. `invalidate_session` exists for
   that caller, and the 421 test now wraps its call in a timeout so the same
   mistake fails with a sentence instead of hanging the suite.

   **Still not constructible**: `vertical_for` refuses the arm, because no
   iCloud account is ever persisted. That blocker is the ADP gate (item 7), not
   the transport.
6. ✅ `icloud/doc.rs` — vendored protos, committed codegen, drift check,
   text→HTML and `strip_leading_title` all **done**. The strip was blocked on
   #4; #4 ran, and its answer is what E3 now specifies
7. ✅ ADP verdict + refusal + banner (Component H) — **landed 2026-08-23.**
   `AdpVerdict::of` reads `DecodeTally` and re-derives nothing. It has **four**
   variants where H1's table had three, and the fourth is the point: that table
   said "n note records, 0 decoded → Unreadable", which counts a `Malformed`
   document and so contradicts H1's own prose two paragraphs later. Following
   it literally would block an account over a bug in Jodd's decoder or a schema
   change Apple shipped — the exact false accusation this component exists to
   prevent. `Inconclusive` proceeds, loudly; `Unreadable` is the only verdict
   that blocks; `NoNotes` stays the trap it always was.

   H2's runtime half is `Vertical::blocked_reason()` — an optional trait method
   defaulting to `None`, reporting only what a **completed** read learned
   (`OnceCell::get`, never a fetch from a getter). `index_account` stamps it
   onto `Account.blocked_reason`, and the frontend's existing `list_accounts`
   poll carries it to a banner: no new event channel, which is gotcha #6's
   lesson applied ahead of the failure rather than after it. Re-stamped every
   pass, so it clears itself when an account becomes readable again.

   H3 needed no new code — `wire::decode_note` already skips an unreadable
   record rather than caching an empty-bodied note, and `DecodeTally` counts it.
8. ✅ Revival path (B4) — **landed 2026-08-23.** A hidden webview sharing the
   sign-in window's `data_store_identifier`, created lazily and reused, with
   Apple's own JS re-authenticating silently against the persisted store.
   `LiveSession` resolves the window **per harvest** rather than binding one at
   construction: the visible sign-in window can close between two reads, and a
   jar bound to a dead window reports the session gone when it is merely
   somewhere else. A freshly created window gets a bounded warm-up keyed on the
   client-config marker — our script writes it off Apple's own first request,
   so its presence means the page loaded *and* talked to Apple.
8b. ✅ **The way in** — `icloud_sign_in`, a macOS-only button, and the account
   record. Not a numbered item in this spec, which is why it is called out:
   the list went from "the vertical exists" to "manual end-to-end pass" with no
   step in between for *the user being able to start a sign-in at all*.

   **Two entry points, not one** — `AuthScreen` only renders with zero
   accounts, so a button there alone would have made iCloud unreachable for
   every existing user. The Sidebar's account panel carries the same entry, and
   both go through `icloud_sign_in`, which emits `oauth-success` so the
   post-add refresh every other backend already has applies unchanged rather
   than needing a second path to keep in step.

   iCloud is deliberately absent from `backend_kind_for_signin` for the same
   reason LocalFs is — there is no OAuth on this backend, so routing it through
   an OAuth entry point would need a fake shape for a flow that shares none of
   one. `deriveIsMacos` gates the button and **fails closed**, the inverse of
   `deriveIsAndroid`: offering this where the per-webview data store does not
   exist produces a window that can never hold a session.

   **`remove_account` must forget the session, and missing that is a
   wrong-account bug rather than a leak.** The credential lives in `WKWebView`'s
   data store, which survives everything account removal previously did — so
   the next sign-in would find a valid session, `/validate` would answer with
   the OLD Apple ID, and Jodd would silently create an account for the person
   who just signed out. `icloud_auth::forget_session` closes both webviews and
   deletes the store; it is the counterpart of `refuse_second_icloud_account`.
9. 🔶 **Manual end-to-end pass on the live account** — first run 2026-08-23 on
   `kaiwan@me.com`, and it found what only a live run could.

   Sign-in itself worked: `session established for kaiwan@me.com`. **The ADP
   check then failed 98 ms later** with `cookie harvest failed: … failed to
   receive message from webview`. Two mistakes, both invisible to every test
   here because both are about a real `WKWebView`'s lifecycle:

   - `sign_in` closed the visible window on success, leaving the check to
     cold-start the hidden one and harvest from a page that had not reached
     Apple yet. The window now stays open through the check and the caller
     closes it; the warm-up allowance also went from 5 s to 20 s, because five
     was chosen as "generous for a page load" and is not, for a webview that
     must be *created* first.
   - **`get_webview_window` returning `Some` does not mean the webview can be
     talked to.** A window asked to close is still registered for a moment, and
     `cookies()` against it fails. `LiveSession` now falls back to the hidden
     window when the sign-in window stops answering, which turns that race into
     a slower success rather than a sign-in that reports the account unreadable.

   Still to confirm on the next run: notes and the folder tree, the 2FA screen
   surviving the poll, and revival after a session dies.

**Two pieces are unblocked and need no live data**, because both are pure
functions over shapes the wire cannot change: Component G's folder-tree builder
(over `(id, title, parent)` triples — the `/`-in-a-name, cycle, depth-cap and
Trash-exclusion hazards are all decidable now) and Component H1's three-way ADP
verdict over `DecodeError` (whose entire point, keeping `NoNotes` out of
`Unreadable`, needs no account at all).

## Deferred (door open, not built)

- **M2 — write.** `records/modify` with `recordChangeTag` as a real optimistic
  lock. Brings with it: the full CRDT document model (attribute runs, tables,
  embeds), icloud-md's byte-for-byte round-trip guard (*reproduce the remote's
  current form exactly from your own model, or refuse to edit it* — the guard
  that would have caught gotcha #17), `has_trash: true` with restore,
  `accounts.sync_cursor` persistence, and worker integration. `strip_leading_title`'s
  fail-safe direction must be revisited here, before the first push.
- **M3 — Windows and Android sign-in.** Each is its own constraint chain; gotcha
  #8 is the precedent for how expensive assuming otherwise gets.
- **Shared / collaborative notes.** A separate CloudKit database (`shared`), one
  zone per sharer.
- **Attachments.** Impossible by construction on this path: the iCloud *web*
  Notes editor cannot attach a file, so there is no client behavior to mimic.
  Same shape as the Exchange limitation, different cause.

## Open questions (non-blocking)

- What fraction of Apple Notes users have ADP on? Decides whether this vertical
  is a headline feature or a footnote. Nothing in M1 depends on the answer.
- Does an iCloud note's `recordName` ever equal the `X-Universally-Unique-Identifier`
  of the same note on an email backend? Unanswerable as posed — the same note is
  never in both accounts — and it only matters for a future cross-backend
  migration that would need to dedup.
- Should `Vertical` grow an explicit "how do I canonicalize identity" method
  rather than the free function in Component F? M1 has four backends and one
  call site; if a fifth arrives with a fifth policy, the free function is the
  wrong shape and the trait is the right one. Not enough evidence yet.
