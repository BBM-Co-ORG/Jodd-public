# iCloud on Android — porting Vertical #4 to the one platform it was refused on

> Status: **design / approved** (2026-09-10). Brings the CloudKit backend to the
> Android Developer Preview. No new backend, no new milestone of the vertical —
> the same `ICloudVertical` that ships on macOS and Windows, reached through a
> platform seam that Android needs three arms in.
>
> Builds on:
> - the wry Android cookie-crash fix, vendored 2026-09-10 (`[patch.crates-io]`
>   at the workspace root, gotcha #31) — without it this is not buildable
> - the 2026-09-10 device spike (four probes on a Galaxy S23 FE), recorded in
>   `CLAUDE.md`'s roadmap 4b
> - [2026-08-21-icloud-vertical-m1-design.md](2026-08-21-icloud-vertical-m1-design.md)
>   — Components B3 (harvest), B4 (revival), B5 (version strings), A3 (one account)
> - [icloud_webview_probe.rs](../../../src-tauri/examples/icloud_webview_probe.rs)
>   — the eight questions the desktop platforms had to answer
>
> **Acceptance bar:** on a real Galaxy S23 FE, an iCloud-native Apple ID can be
> added from Jodd's own UI, its notes and folders appear with real titles and
> real nesting, an edit made in Jodd reaches Apple Notes and is **still correct
> 20 minutes later**, removing the account genuinely empties the cookie jar, and
> `cargo test --workspace` plus `cargo check --target aarch64-linux-android` are
> green. The platform gate flips in its own commit, last, after that pass.

## Goal & framing

Three of Jodd's four backends already run on Android. iCloud is refused, with a
message that names a mechanism:

```
iCloud accounts need a desktop webview to hold the session
```

That sentence was true when it was written and is now two-thirds false. The
device spike measured what actually stands in the way, and the only hard blocker
— an uncaught `NullPointerException` in wry's `getCookies` that killed the whole
app — is fixed and vendored. What remains is a seam, not a wall.

**The transport is untouched.** `wire.rs`, `doc.rs`, `compose.rs`, `crdt.rs`,
`format*.rs` and `transport.rs` are `reqwest` and protobuf; none of them has ever
known a webview exists. Reads, writes, the CRDT replica engine, relocation and
the six per-note refusals all cross to Android unchanged. The whole of this
design lives in `icloud_auth.rs`, plus two `cfg` arms in `lib.rs` and one line
in `platform.ts`.

**What Android changes is the answer to "where does the cookie jar live".** On
desktop it lives inside a specific webview, which is why Component B4 keeps a
hidden one alive: close the last webview and the session is gone (measured on
Windows, 2026-09-09 — `WebviewWrapper::drop` tears the web context down). On
Android `CookieManager.getInstance()` is **process-wide**: the jar belongs to the
process, not to any window. That single fact removes B4 rather than porting it,
and it is also the design's one load-bearing assumption — Component C says how it
is checked and what happens if it is false.

### Three APIs that compile and do nothing

The spike's most useful result is not a feature; it is a list of calls that
return `Ok` on Android while performing no part of their name. Every one of them
is on the path this port would otherwise reuse verbatim:

| Call | Android reality | Where it bites |
|---|---|---|
| `WebviewWindow::cookies()` | `Ok(Vec::new())`, a hardcoded stub in wry | the harvest reads an empty jar and the session looks dead |
| `WebviewWindow::set_visible(false)` | `// Unsupported` | B4's hidden webview is a full-screen overlay |
| `WebviewWindow::clear_all_browsing_data()` | deletes caches, history and form data — **never touches `CookieManager`** | `forget_session` reports success and forgets nothing |

This is the family `CLAUDE.md` already names for the desktop pair: *"Each
platform ignores the other's lever silently."* Android adds a third member, and
none of the three fails loudly. **Every acceptance step in Component G therefore
measures an outcome, never a return code.**

## Decisions locked in brainstorming (2026-09-10)

1. **No hidden session webview on Android.** The jar is the process's. When the
   session dies the user presses **Reconnect**, which is the existing
   `icloud_reauthenticate` command and the existing button in
   `AccountSettings.svelte` — no new UI, no new command.
2. **`cfg(target_os = "android")`, never `cfg(mobile)`.** iOS is unmeasured and
   stays refused. This is the same fail-closed discipline as
   `ICLOUD_PLATFORMS`, `canonical_uuid_for` and `remote_pin_policy`: adding a
   platform is a deliberate act with a device run behind it.
3. **Capabilities are not narrowed for Android.** `Capabilities::for_backend` is
   derived from `BackendKind` alone, and making it platform-dependent would be a
   new mechanism to serve a milestone gate. Android gets the writable backend
   macOS and Windows have, and the live pass must therefore include a write
   verified past the delayed-merge window.
4. **The one-account refusal stays.** Its wording already says the limit is
   Jodd's rather than the platform's, and on Android it is *more* true than
   elsewhere: `CookieManager` is process-wide, so per-account jars are not
   expressible there at all today.
5. **The platform gate flips last, in its own commit.** One line, revertable
   without touching anything else.

## Component A — `AndroidJar`, the cookie source

### A1. What Android hands back, and the trap in it

`cookies_for_url` on Android bottoms out in `CookieManager.getCookie(url)`, which
returns a **`Cookie:` request header** — `name=value; name=value`. wry splits it
and parses each pair (`main_pipe.rs`, `WebViewMessage::GetCookies`), so every
cookie arrives with a name, a value, and **`domain()`, `path()` and `secure()`
all `None`**.

`harvest_one` maps a missing domain to `""`. `cookie_header_for` then filters
with `domain_matches(host, "")`, which is false for every host, so the header
comes out **empty** — and `establish` reads an empty header as a session that is
gone. Reusing the desktop harvest unchanged produces a sign-in that appears to
work, a `/validate` that says anonymous, and a user told their session is
unavailable seconds after Apple accepted their password.

### A2. The stamp

Android has already done the domain matching — that is what asking for a URL
*means*. So the Android arm puts back what the header format dropped:

```rust
/// Pure, so the rule is tested on the host and JNI is not in the way.
fn stamp_android_cookies(asked: &str, pairs: &[(String, String)]) -> Vec<HarvestedCookie>
```

Every cookie harvested from an `*.icloud.com` URL is stamped:

| field | value | why |
|---|---|---|
| `domain` | `icloud.com` | the registrable parent, which is what Apple's own cookies carry (`Domain=.icloud.com`) — desktop's jar has exactly this shape after `harvest_one` strips the leading dot |
| `host_only` | `false` | same reason, and the same lossiness desktop already documents |
| `path` | `/` | the header format carries no path; `path_matches` treats `/` as matching everything, which is the permissive direction and matches what the browser just decided |
| `secure` | `true` | every URL asked is `https`, and the flag is only ever used to *withhold* |

Stamping the registrable parent rather than the hostname asked is what makes the
**CloudKit partition host work without being known in advance**. `harvest()`
takes no host parameter — deliberately, since a jar is harvested once per burst
and filtered per request — and `p<N>-ckdatabasews.icloud.com` is only learned
from `/validate`, i.e. *after* the first harvest. A cookie stamped `icloud.com`
domain-matches it for free. Stamping `www.icloud.com` instead would hand every
CloudKit request an empty jar, which is precisely the failure gotcha #19 warns
about on the desktop side for the mirror-image reason.

### A3. Which URLs, and which webview

```
for url in ["https://www.icloud.com", "https://setup.icloud.com"]:
    stamp_android_cookies(url, cookies_for_url(url))
dedup by name, first wins
```

Two URLs, not one: a host-only cookie Apple sets on `setup.icloud.com` is
invisible to a request for `www.icloud.com`, and `/validate` is the first call of
every session. `idmsa.apple.com` is deliberately **not** in the list — nothing
Jodd sends ever goes to it, and `cookie_header_for` would refuse an `apple.com`
cookie for an `icloud.com` host anyway.

The harvest reads through the **main app webview** (label `"main"`, Tauri's
default since `tauri.conf.json` names none). Not an iCloud webview: on Android
there is not necessarily one alive, and there does not need to be — see
Component C.

### A4. What stays shared

`AndroidJar` implements the existing `CookieSource` trait and nothing else
changes. `cookie_header_for`, `ClientConfig::from_jar`, `establish`,
`classify_validate`, `MARKER_PREFIX` filtering and every test around them are
untouched. The trait keeps its purpose: the vertical is exercisable against a
mock HTTP server with a `StaticJar`, on any machine.

## Component B — B5 without injection: a fallback, not a replacement

`initialization_script` does not reach external pages on Android (measured: a
`document.title` marker never took effect; wry injects through a custom protocol
handler that covers only app-served pages). So `INIT_SCRIPT` never runs on
icloud.com, the `jodd_icloud_cfg` marker cookie is never written,
`ClientConfig::from_jar` returns `None`, and `establish` stops with *"no client
config captured yet"* before CloudKit is ever reached.

The three parameters split by nature:

| parameter | Android source |
|---|---|
| `clientBuildNumber` | hardcoded constant |
| `clientMasteringNumber` | hardcoded constant |
| `clientId` | `Uuid::new_v4()`, minted once per session |

`clientId` is not Apple's secret — Apple's own web client mints a UUID per
session, which is why `ClientConfig::query()` already treats it as opaque. Only
the first two can drift.

**The shape is a fallback, so desktop behaviour does not change and Android
upgrades itself if wry ever gains external-page injection:**

```rust
ClientConfig::from_jar(&jar).unwrap_or_else(ClientConfig::android_fallback)
```

Two obligations come with the constants:

- **They are captured, not invented.** Task 1 of implementation is a desktop
  sign-in whose captured `ClientConfig` is read out of the log (adding the log
  line if it is not already there), and the pair that comes back is what lands in
  the constants — with the capture date in the doc comment beside them. The
  values in the existing unit tests (`2628Build44` / `2628B36`) are fixtures from
  2026-08-22 and must not be reused as if they were current: `CLAUDE.md` records
  the build number moving 2624 → 2628 → 2630 in three weeks.
- **Every fallback logs.** One line per `establish` on Android naming the value
  and its capture date, so a future CloudKit refusal has a first suspect instead
  of a mystery. This is the degradation the spike predicted; it is accepted, not
  hidden.

## Component C — no B4, and the assumption underneath it

`LiveSession` (`ensure_session_webview` → `open_session_window` → warm-up loop)
exists because a desktop jar dies with its webview. On Android the jar is the
process's, so the Android `CookieSource` is `AndroidJar` over the main webview
and there is no hidden window, no warm-up, and no cold-start allowance.

**The assumption is that a webview which never visited icloud.com can read
icloud.com's cookies.** The spike saw this happen but called it *not conclusive*,
because the webview it asked had loaded icloud.com itself. It is checked as step
2 of the live pass, immediately after the step that proves the cookies exist at
all — before any other work depends on it.

**If it is false**, the fallback is bounded and does not change anything above:
`AndroidJar` gains a label field and harvests from the iCloud sign-in webview
while one is alive, and the session then lasts only as long as that webview does.
Reconnect becomes a more frequent interruption; nothing else in this design
moves.

**What no longer happens on Android, stated so it is not read as a defect:**
Apple's own JS never re-authenticates a resident session, because no webview sits
on icloud.com between sign-ins. Sessions therefore age out on Apple's schedule
and end with a Reconnect. That is the decision taken in brainstorming, and the
existing sign-in hint already tells the user that ticking **Keep me signed in**
is what decides how often it happens.

## Component D — `forget_session`: the one that silently forgets nothing

`clear_all_browsing_data()` maps to `RustWebView.clearAllBrowsingData()`:

```kotlin
deleteDatabase("webviewCache.db"); deleteDatabase("webview.db")
clearCache(true); clearHistory(); clearFormData()
```

No `CookieManager`. So the Windows arm, reused on Android, would leave the
session cookies exactly where they are and report success. The consequence is not
a leak — it is the **wrong-account bug** this function exists to prevent: remove
the account, sign in with a different Apple ID, and `/validate` answers with the
previous one, so Jodd silently creates an account for the wrong person.

The Android arm calls `CookieManager` itself, through `PlatformWebview::
jni_handle()` (present on Android in tauri 2.11.5):

```
removeAllCookies(null)   then   flush()
```

`flush()` is not optional: `removeAllCookies` is asynchronous and the app may be
killed before the removal reaches disk, which would resurrect the jar on next
launch.

**Order:** clear first, then close the webviews — the Windows order, for the
Windows reason (the lever is reached *through* a live webview). It is stated
explicitly rather than inherited, because `CLAUDE.md` records this exact ordering
inverting between platforms with neither mistake reporting an error.

Nothing else in the process depends on those cookies: Jodd's own frontend is
served from a custom protocol and stores its state in `localStorage`, which
`removeAllCookies` does not touch, and Gmail/Microsoft sign-in happens in the
system browser rather than in-process.

## Component E — sign-in lifecycle

`sign_in` is reused whole; the `#[cfg(desktop)]` gate becomes
`#[cfg(any(desktop, target_os = "android"))]`. `.inner_size()` and `.title()`
compile on Android and have no effect; `.initialization_script(INIT_SCRIPT)` is
kept even though it is inert, so the code upgrades itself if that changes.
`POLL_INTERVAL` / `POLL_TICKS` stay at ten minutes — a phone sign-in with a 2FA
code typed on another device is slower than a desktop one, not faster.

`close_signin_window` needs a real Android arm: it must **not** open a successor
window, because there is no B4 to hand over to. Forgetting this leaves a
full-screen Apple webview permanently on top of the app, which is the most
visible failure available in this design and the cheapest to avoid.

### E1. The back button — a predicted defect, deliberately not pre-solved

`main_pipe.rs` calls `WryActivity.setWebView` on **every** webview creation, so
`mWebView` — the field the activity's `OnBackPressedCallback` navigates — points
at whichever webview was created last. During sign-in that is Apple's, and Back
walking back through Apple's flow is correct behaviour.

After `close_signin_window`, `mWebView` still references a destroyed webview.
The predicted consequence is that the first Back press in the main app after an
iCloud sign-in closes the app instead of navigating.

This is listed as live-pass step 7 rather than fixed in advance, because the
alternative is guessing at a fix for a behaviour nobody has observed. If it
reproduces, the remedy is an extension of the wry patch this project already
carries — the fork exists, and this is the same class of one-line Android defect.

## Component F — the platform gate

`deriveSupportsIcloud` gains `'android'` in `ICLOUD_PLATFORMS`, and
`vertical_for`'s `#[cfg(not(desktop))]` refusal narrows to iOS. The refusal
message it leaves behind must stop claiming a desktop webview is required, since
that will no longer be why.

This is the **last** commit of the milestone. Until it lands, every Android arm
above is dead code on a shipped build — which is the point: the branch stays
mergeable and the preview stays unaffected while the device work proceeds.

## Component G — verification

### G1. On the host, no device

- `stamp_android_cookies` is pure and fully tested off-device:
  - a stamped jar yields a non-empty header for `setup.icloud.com`
    **and** for `p130-ckdatabasews.icloud.com` — the partition host is the point
    of the registrable-parent stamp, so it is asserted rather than assumed;
  - a jar built the desktop way from Android's name/value pairs (`domain: ""`)
    yields an **empty** header. The trap is pinned as a test, not merely
    avoided — this is the failure that would otherwise present as "the session
    is dead".
  - Jodd's own `jodd_icloud_*` markers are still excluded.
- `ClientConfig::android_fallback` returns all three fields, and `query()`
  contains the captured build and mastering numbers.
- `deriveSupportsIcloud('android')` — flipped with Component F, not before.
- `cargo check --target aarch64-linux-android` must pass. Every `cfg` arm in
  this design compiles only on the target it is written for, and nothing else in
  the workspace type-checks them.
- The full CI gate, not a narrower one: `cargo test --workspace`,
  `node scripts/gen-changelog.mjs`, `npx vitest run`,
  `npx svelte-check --threshold error`, `npm run build`.

### G2. On the Galaxy S23 FE, riskiest assumption first

Ordered so that a design-ending answer arrives in step 1, not step 6.

1. **Sign in.** Does `/validate` reach `Complete`? This is the first time anyone
   has signed in to Apple in a webview on this phone, and it proves the thing
   nothing so far has: that the HttpOnly `X-APPLE-WEBAUTH-*` and `X_APPLE_WEB_KB-*`
   cookies come through `cookies_for_url`. If they do not, the design ends here.
2. **Harvest from `"main"`.** Component C's assumption. If false, take C's
   bounded fallback and continue.
3. **Read.** Note and folder counts agree with Notes.app on the same account.
   Counts are stated as *notes* or *records*, never ambiguously — gotcha #22.
4. **Write.** Edit a note in Jodd; confirm in Apple Notes, then **re-check past
   20 minutes**. A clean check minutes after a write proves nothing on this
   backend.
5. **Restart.** Kill the app and reopen: does the session survive
   (`CookieManager` persistence + `flush`)?
6. **Remove the account.** `/validate` must then report not-signed-in. This is
   the only proof Component D's JNI call did anything.
7. **Back button**, during and after sign-in (Component E1).

Steps 1–6 gate the platform flip. Step 7 gates nothing on its own but must be
recorded either way.

### G3. What is out of scope, with reasons

- **iOS.** No target, no device, no measurement.
- **Multiple Apple IDs on Android.** `CookieManager` is process-wide; unlike the
  desktop levers there is no parameter to derive per account. The existing
  refusal covers it.
- **Reading `clientBuildNumber` live on Android.** Blocked by wry, tracked as a
  logged degradation rather than worked around by scraping Apple's bundle.
- **Attachments, pins, new hashtags.** Unchanged from the desktop vertical and
  unrelated to the platform.

## Documentation this milestone owes

- `CLAUDE.md`: roadmap 4b's Android paragraph rewritten from "measured, not
  merely unbuilt" to what shipped; the three-silent-APIs table added to gotcha
  #31, which already owns the Android/wry story.
- A handoff at `docs/superpowers/HANDOFF-2026-09-10-icloud-android.md` recording
  the live-pass numbers — including step 2's answer, which is the one a future
  reader will want and cannot re-derive.
