# Android App Links — what has to exist outside this repo

Jodd's Android OAuth redirect is an `https://` URL delivered to the app by
Android App Links. This file records why, and the two things that live on a
web server rather than in the codebase.

## Why not the simpler options

**Custom URI scheme** — Google removed it. The authorization request is
rejected outright:

```
Error 400: invalid_request
Custom URI scheme is not enabled for your Android client.
```

and the docs say "Custom URI schemes are no longer supported on Android and
Chrome apps". This is not a configuration problem; the flow no longer exists.

**Loopback (`http://localhost:8080/callback`)** — accepted by Google, and it
genuinely worked on an Infinix X6821 running Android 13: token exchange
succeeded, the refresh token reached the Keystore, and the sync worker talked
to Gmail. It then **failed on a Galaxy S23 FE running Android 16**, because the
OS killed Jodd while the user was on Google's consent screen. The app log shows
a 2m40s silence between the listener starting and a cold start with no account,
and `complete_oauth` never ran. The browser showed "This site can't be reached".

That is the difference App Links exists to remove: the redirect **starts** the
app rather than requiring it to have stayed alive. Two devices, two outcomes,
and the design has to survive the worse one.

## What has to be hosted

**`https://jodd.bbmedia.co.th/.well-known/assetlinks.json`**

Serve [assetlinks.json](assetlinks.json) at exactly that path. Requirements
Android enforces:

- **HTTPS with a valid certificate.** No redirects — Android does not follow
  them when verifying. Checked on 2026-08-03: `https://jodd.bbmedia.co.th/`
  answers `200` with no redirect, served by nginx. This is the single most
  common reason verification fails, and it is why the host is not the company
  apex: `bbmedia.co.th` almost certainly `301`s to `www.`, which loses.
- **`Content-Type: application/json`**. Serving it as `text/plain` fails.
  nginx needs to be told, since the default type map has no rule for a file
  under `.well-known/`:

  ```nginx
  location = /.well-known/assetlinks.json {
      default_type application/json;
      alias /path/to/assetlinks.json;
  }
  ```
- Reachable without authentication, and not behind a bot challenge.

**The host already serves Jodd's info site, and that constrains the manifest.**
`assetlinks.json` grants `delegate_permission/common.handle_all_urls` at
**host** granularity — the grant itself has no path scoping. On a host with
real pages, a broad intent-filter would mean every tap on a Jodd marketing link
opens the app instead of the browser. The scoping therefore has to happen on
the Android side, with an **exact** path rather than a prefix:

```xml
<data android:scheme="https"
      android:host="jodd.bbmedia.co.th"
      android:path="/oauth2redirect" />
```

`android:path` matches the whole path exactly; `pathPrefix` would swallow
`/oauth2redirect-anything` too, and omitting the path entirely claims the whole
site. Nothing on the web side can narrow this — only the manifest can.

**The fingerprint list is the whole security model.** Android only hands the
URL to an app whose signing certificate appears there — this is what stops a
different app from claiming Jodd's redirect and receiving the auth code.

The file lists two fingerprints, and the array taking more than one is what
lets debug and release builds both verify without swapping the file:

- `48:05:…:BD:D6` — the **debug** key, i.e. `~/.android/debug.keystore`, which
  is what `tauri android build --debug` signs with.
- `E4:06:…:24:C2` — the **release** key, `jodd-android.keystore` alias `jodd`.
  Once a signed release has shipped, replacing this key prevents existing
  installs from accepting updates and breaks verified App Links for the old
  install. Preserve the release keystore and its password in durable,
  access-controlled storage.

**It is SHA-256, not SHA-1.** `keytool -list -v` prints both, and SHA-1 is the
one Google's *Android-type* OAuth client asked for — a client type this project
no longer uses (see above). `assetlinks.json` reads only
`sha256_cert_fingerprints`. To re-derive either:

```bash
keytool -list -v -keystore jodd-android.keystore -alias jodd | grep SHA256
keytool -list -v -keystore ~/.android/debug.keystore -alias androiddebugkey \
        -storepass android | grep SHA256
```

Regenerating the release keystore changes its fingerprint, so this file has to
be updated and redeployed at the same time.

## What has to exist in Google Cloud Console

An OAuth client of type **Web application** (not Android, not Desktop) with
this authorized redirect URI:

```
https://jodd.bbmedia.co.th/oauth2redirect
```

Web clients have a client secret; Android sends it exactly as desktop does.

## Required, not optional: the page at the redirect URL

Deploy [oauth2redirect.html](oauth2redirect.html) at
`https://jodd.bbmedia.co.th/oauth2redirect`. It is a load-bearing part of the
flow, not a courtesy page, and this was the last thing to be understood.

**Verified App Links are not enough, because the callback is a redirect.**
Android hands a URL to an app on navigations the *user* starts — tapping a
link. Browsers deliberately do not hand off a server-side redirect that occurs
mid-navigation, and an OAuth callback is exactly that: Google 302s the consent
page to the redirect URI. Measured on a Galaxy S23 FE with the domain
`verified` and link handling `Enabled`: the browser rendered the page and no
VIEW intent for the domain was ever created. Sign-in could not complete, three
times in a row, with every other layer provably correct.

The page closes that gap. It reads `code` and `state` from its own address bar
and re-issues them as an `intent:` URL naming the package:

```
intent://jodd.bbmedia.co.th/oauth2redirect?<query>#Intent;scheme=https;package=co.bbmedia.jodd;end
```

`window.location.replace` to that URL was enough on Samsung Internet — the app
came to the front on a cold start and logged the code, with no tap. A button
carries the same URL for browsers that require a user gesture.

Two properties worth keeping if this is ever rewritten:

- **`package=` pins the receiver**, so no other installed app can claim the
  code, and — verified by setting the domain back to `Disabled` and re-running
  — the handoff still works when the user's per-domain link preference is off.
  That matters: that preference is one wrong tap away for any real user.
- **The callback page does not forward the code to another service.** The HTTPS
  callback request itself reaches the website before Android or the fallback
  page hands it to Jodd. The page has no external subresources and uses
  `Referrer-Policy: no-referrer`; the app nginx location disables access logs.
  The outer reverse proxy/CDN must also be configured not to log callback query
  strings, and this must be verified after deployment.

## Verifying it works

After deploying, check Android's own verifier rather than guessing:

```bash
adb shell pm get-app-links co.bbmedia.jodd
```

**Pass `--user cur`, or the answer is half the story.** Without it the output
stops after `Domain verification state`, which is only the web side — whether
`assetlinks.json` vouches for this APK. The per-user block underneath is a
separate gate, and it is the one that actually decides whether a link opens in
the app:

```
    User 0:
      Verification link handling allowed: true
      Selection state:
        Disabled:                      <-- verified, and still going to the browser
          jodd.bbmedia.co.th
```

That is a real state, observed on the test device, and it is self-reinforcing:
every callback that falls through to the browser teaches Android that the
browser handles this domain. Reading only `verified` and declaring the setup
sound wasted an hour here. Turn it on with:

```bash
adb shell pm set-app-links-user-selection --user cur --package co.bbmedia.jodd true jodd.bbmedia.co.th
```

or, on the phone, Settings → Apps → Jodd → Open by default → Open supported
links. The `intent:` handoff above works either way, which is why it is the
mechanism rather than the fallback.

`verified` is what you want. `1024` means the domain could not be reached or
the JSON was rejected. To force a re-check without reinstalling:

```bash
adb shell pm verify-app-links --re-verify co.bbmedia.jodd
```

Google also publishes a checker that reports the same thing from their side:
<https://developers.google.com/digital-asset-links/tools/generator>
