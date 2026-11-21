# What's on my TV — ingest the video a paired YouTube TV app is playing

**Date:** 2026-11-02 (spike passed 2026-11-03) · **Status:** built 2026-11-03,
live pass pending (§9). Written after a brainstorm with the user, a passing
spike on the user's own TV, and a builder check. Calls made by the author
rather than the user are marked "my call".

## 1. Why

The user watches YouTube on a **Google TV** and wants what is playing to land
in Jodd as an ingested note. Their chosen gesture: **pick up the phone; the TV
keeps playing.**

Today that takes the phone's YouTube → History → Share → Jodd → AI → Extract
(about four taps, and the timestamp is lost). Nothing in Jodd can learn what a
TV is playing, and nothing in Jodd listens on the network (the only listener is
the short-lived OAuth loopback, `auth.rs:525`).

YouTube's TV app supports **"Link with TV code"** — the pairing a phone uses to
act as a remote, through YouTube's unofficial **Lounge API**. Jodd uses only the
read half of it: it joins, asks "what is playing, and where?", and leaves. It
never sends a playback command.

## 2. What the spike measured (2026-11-03, the user's Google TV)

Probe: `src-tauri/examples/lounge_probe.rs`. Raw replies were dumped to the
session scratchpad and are the source of §7's fixtures.

- **Pairing works.** `POST /api/lounge/pairing/get_screen` with
  `pairing_code=<12 digits>` → HTTP 200, `screen.name = "YouTube on TV"`,
  `screenId` (64 hex chars), `loungeToken`, `expiration` (~13 days). A wrong or
  expired code → HTTP **404** with an HTML body.
- **Only the screen ID is needed afterwards.** `POST
  /api/lounge/pairing/get_lounge_token_batch` with `screen_ids=<id>` returns a
  fresh token each time.
- **The TV reports what it plays.** After `bc/bind` + `getNowPlaying` +
  subscribe, `nowPlaying` carried `videoId`, `currentTime` ("72.512"),
  `duration` ("790.201"), `state` ("1" = playing), `listId`, `cpn`.
- **But not always on `nowPlaying`.** On the first run `nowPlaying` carried
  only `listId`; the `videoId` arrived on `onSubtitlesTrackChanged`,
  `onVideoQualityChanged` and `onAudioTrackChanged`, and no `currentTime`
  arrived within 10 s. The parser must take the video ID from **any** event
  and treat the time as optional.
- **TV client:** `clientName = "tvhtml5"`, `app = "lb-v4"`; the set is a
  Hisense HiSmartTV A4 (2020, Android 9), YouTube TVHTML5 `7.20261101.19.00`.
- **The TV keeps every remote it has seen.** The second run's `loungeStatus`
  listed **two** "Jodd probe" devices (one per probe run, each with a new
  device UUID). A fresh UUID per press would pile up remotes on the TV — hence
  §5.2's stored device ID.
- **A minimal bind works, and reading changes nothing** (run 3, same day).
  Bind without `method=setPlaylist` / `magnaKey` → HTTP 200 and the full event
  set. The user confirmed the switch between runs 1 and 2 (chant → Genie4) was
  their own; nothing on the TV changed because of a press.
- **`autoplayUpNext` carries other videos' IDs** (run 3: `wGL0WZpHyO8`, then
  `MQTOFyKFM2o`, while `nowPlaying` named `wnXoCNd57Ks`). An "any event" rule
  would pick the wrong video — §5.1's priority order is required, not
  defensive.
- **`state: "2"` with a frozen `currentTime`** across 8 s (147.487) — paused,
  inferred from the clock not moving. The answer is still the right video and
  second, which §5.1's "state does not gate" rule relies on.
- **The fixtures are saved**, redacted (session IDs, `cpn`, queue ID and device
  UUIDs replaced by placeholders; video IDs and the TV model kept), in
  `src-tauri/tests/fixtures/lounge/run{1,2,3}-{bind,subscribe}.txt` (run 3 is
  the minimal bind, with `autoplayUpNext` and a paused `nowPlaying`). Redaction
  changed string lengths, so the chunk-length lines no longer match — which the
  parser ignores by design (§5.1).
- **Fast enough.** The answer came inside a 5 s listen. Exact latency was not
  measured.
- **No Google sign-in** is involved; pairing is device-to-TV.

Not observed yet: whether the TV shows a "Jodd connected" toast on each press,
and how long a pairing survives (the token is re-fetched, but whether YouTube
expires an unused screen ID is unknown). Both are live-pass items (§9).

## 3. Goals, non-goals, anti-goals

**Goals**
1. One tap on the phone (or desktop) → the Extract dialog opens with the TV's
   video link and the time the user was at, ready to confirm.
2. One-time pairing in Settings with the TV's code; unlink at any time.
3. Works on desktop and Android, on any network (the API is cloud, not LAN).
4. **More than one TV** (the user, 2026-11-03): link several, each with
   **Refresh**, **Re-pair**, **Rename** and **Unpair**, and **one default**.

**Non-goals (this version)**
- Apps other than YouTube on the TV; Shorts and YouTube Kids if they don't
  report (pyytlounge documents Kids as unsupported).
- More than **5** TVs per device (my call; a constant).
- Sharing pairings between the user's devices. A screen ID can control a TV,
  so it never travels through the notes account.
- Ingest that uses the timestamp (e.g. "summarise from 12:34 on"). The time is
  carried as context text only (§5.4).
- Moment notes, a watch log, a Quick Settings tile — future, built on the same
  call (they were discussed; the user chose the one-tap note first).

**Anti-goals — a failure even if it works**
- **Jodd changing anything on the TV.** Only `getNowPlaying` may ever be sent.
  No play, pause, seek, queue or volume command, ever.
- **Ingesting without the user's confirmation.** The Extract dialog stays the
  only place an ingest starts.
- **Background polling of the TV**, or reading the keychain from a polled
  command (gotcha #15).
- **The screen ID leaking**: into logs, a note, `accounts.json`, an error
  message, or the frontend. Whoever holds it can control the TV.

## 4. The user's experience

**Settings → Links → "TVs"** — a list, empty at first.

- **+ Link a TV**: one line of help ("On the TV: YouTube → Settings → Link with
  TV code"), a 12-digit code field, **Link**. Wrong/expired code: "That code
  didn't work — the TV shows a new one every few minutes." At 5 TVs the button
  is disabled with "Unpair one first".
- **Naming.** Every TV calls itself "YouTube on TV" (measured), so the name
  alone cannot tell two apart. The default label is the TV's brand + model
  from `loungeStatus.deviceInfo` ("Hisense HiSmartTV A4"), falling back to the
  screen name, then "TV 2", "TV 3"…; **Rename** lets the user write "Living
  room". Two rows never share a label: a clash gets " (2)".
- **The same TV twice.** Pairing returns the TV's own screen ID; if it equals
  a linked row's (compared in Rust — the frontend never sees screen IDs), that
  row is updated and the user told "Already linked as Living room".
- **Each row:** label, model, a status line, and a menu:
  - **★ Default** — exactly one row while any exist. The first TV linked is
    the default; unpairing the default makes the oldest remaining row the
    default.
  - **Refresh** — checks the TV now (token + a short bind, no
    `getNowPlaying`): "Online · Hisense HiSmartTV A4" / "Couldn't reach it" /
    "Needs re-pairing". It also updates the model text. It never changes the
    label the user chose.
  - **Re-pair** — enter a fresh code for **this** row: label, default and
    position stay. If the code belongs to a different linked row: "That code
    is Bedroom's TV." A code for a TV not in the list is accepted (the TV may
    have been reset, which can change its screen ID).
  - **Rename**.
  - **Unpair** — forgets that TV on this device; the others are untouched.
    That is the revocation Jodd controls. The copy says the TV may still list
    "Jodd" under YouTube → Settings → Linked devices and can be tidied there,
    and **does not claim** that removing it there cuts Jodd off: the token
    batch takes only the TV's screen ID, so whether a TV-side removal revokes
    anything is unknown until live pass item 4 (my call on the wording).
- **A TV that stops answering** (token batch returns no screen) is **not
  deleted**: its row shows "Needs re-pairing" with **Re-pair** and **Unpair**
  (my call — a row vanishing silently is confusing once there are several).

**Use, the AI menu (✦) on desktop and phone → "What's on my TV":**
- No TV linked: the item opens Settings → Links at the TVs list (my call:
  always show the item, for discoverability).
- **Which TV is asked** (my call, reading the user's "one of them will be
  chosen to be default"): the **default** is asked first. If it has a video,
  that is the answer and no other TV is asked. Only if the default has nothing
  (or cannot be reached / needs re-pairing) are the other TVs asked, all at
  once, within the same overall deadline. Then: one with a video → used; more
  than one → a playing TV beats a paused one, and if that still ties, a
  picker ("Living room · วิธีสร้างทีม AI Agent… 2:27" / "Bedroom · …") where
  the user taps one; none → "Nothing is playing on your TVs."
  An answer from a TV other than the default says so on the Source's second
  line ("Watched on Bedroom up to 2:27"), so a surprise is visible.
- One or more linked: the Extract dialog opens **at once**, its Source area
  showing "Asking Living room…" with Cancel. Within a few seconds it fills with:

  ```
  https://youtu.be/uK660HUX-1Y?t=72
  Watched on TV up to 1:12
  ```

  The link is auto-ticked as a source, as any pasted link is. The user picks a
  mode and confirms; from there it is the existing ingest.
- Video but no time yet: the link without `?t=` and without the "Watched…"
  line.
- Nothing playing (no video ID before the deadline): "Nothing is playing on
  YouTube on TV." The dialog stays open and empty, so the user can paste
  instead.
- A TV unlinked on its side (token batch returns no screen): that TV's row is
  marked "Needs re-pairing" and the press continues with the other TVs. If
  that leaves no answer: "Living room needs re-pairing — open Settings."
- Network error: the usual ingest-style error line; nothing is cleared.

**Each device links separately** (my call): pairings are per install, like
the YouTube API key. Pairing the Mac does not pair the phone, and each device
has its own default.

## 5. How it works

### 5.1 Protocol module — `src-tauri/src/ingest/tv_lounge.rs`

- **One block of Lounge constants**, mirroring `ingest/youtube.rs:9-36` ("change
  this block — not a call site"): base URL, `VER=8`, `CVER=1`, `app`,
  `device=REMOTE_CONTROL`, `mdx-version=3`, and the bind parameter set the
  probe used **minus `method=setPlaylist` and `magnaKey`** — pyytlounge's
  remote-control connect sends them; a reader has no reason to, and run 3
  measured the minimal bind working (§2). The live pass still confirms the TV's
  playback and queue are untouched across a press. An endpoints struct with `Default`, like `YoutubeEndpoints`
  (`youtube.rs:186`), so tests inject a local server.
- `pair(code) -> Result<Pairing, PairError>`; `PairError::BadCode` on 404.
- `now_playing(&Pairing, device_id, cancel) -> Result<NowPlaying, TvError>`:
  1. token batch → `TvError::Unlinked` if no screen comes back;
  2. `bc/bind` → read `c` (SID) and `S` (gsessionid), feed all events to the
     accumulator;
  3. if the accumulator has a video ID **and** a time, stop;
  4. else send `getNowPlaying` and read the subscribe stream until it has both;
     once it has a video ID but no time, wait **at most 2 s more**; never past
     a **6 s overall deadline** (my call — the first spike run got no time in
     10 s, so waiting the full window for a time that may never come is the
     slow path to avoid).
     The 2 s window starts when **listening starts** — the subscribe stream's
     response headers — or at the first sighting inside the stream if that is
     later, not when bind ends: in runs 2–3 bind already names the video, and
     the command POST, the subscribe headers and the TV's first `nowPlaying`
     all still have to fit in the window (R23).
- `NowPlaying { video_id, seconds: Option<u64>, duration: Option<u64>, state: Option<String>, tv_name }`.
  No video ID → `TvError::NothingPlaying`.
- **The accumulator** dedupes by event ID and parses the chunked format by
  reading lines until the JSON parses (byte vs character lengths then cannot
  matter — Thai titles make that difference real). Which video wins (my call):
  1. `nowPlaying.videoId`, then `onStateChange.videoId` — the TV's statement
     of the current video;
  2. else the per-video track events — `onVideoQualityChanged`,
     `onSubtitlesTrackChanged`, `onAudioTrackChanged` — which describe the
     video being rendered (measured, run 1);
  3. else `playlistModified.videoId` (measured, run 2 — the queue's current
     item).
  Anything about what plays **next** (`autoplayUpNext`, `onAutoplayModeChanged`,
  `onHasPreviousNextChanged`, any `*UpNext*` event) is never a source. Within a
  tier the latest event wins. `currentTime` / `state` come only from
  `nowPlaying` / `onStateChange`, and only when that event's video ID (if it
  has one) equals the chosen video.
- **State does not gate the answer** (my call): a paused video is the common
  case — the user pauses, then picks up the phone — so any state returns the
  video. "Nothing playing" means no video ID. Live pass item 2 records what the
  home screen and an ended video report; if they report a stale ID, this rule
  is revisited then, with that evidence.
- The video ID is validated with `ingest::urls::valid_id` (`urls.rs:342`, to be
  made `pub(crate)`) before any URL is built.
- HTTP goes through `ingest::net::send_guarded` (`net.rs:182`), building
  `GuardedRequest` literally for POST as `request_player` does
  (`youtube.rs:215-237`), with a form-encoded body and header set by hand. The
  long-poll's deadline is a child `CancellationToken` cancelled by a timer,
  read with `read_capped` (`net.rs:272`) or a chunk loop under that token.
  `send_guarded`'s 20 s request timeout is the outer bound.

### 5.2 Pairing storage — `src-tauri/src/tv_link.rs`

Same shape as `youtube_key.rs` (`:16-80`), the precedent for a user-saved,
non-OAuth secret:

- **Flag file** `<config_base>/jodd/app_tv_links.json` — everything that is
  not secret, so the list renders without a keychain read:

  ```json
  { "version": 1,
    "device_id": "<uuid>",
    "default_tv": "<tv_id>",
    "tvs": [ { "tv_id": "<uuid>", "label": "Living room",
               "model": "Hisense HiSmartTV A4", "screen_name": "YouTube on TV", "screen_hash": "<16 hex>",
               "linked_at": 1762170000000, "needs_repair": false,
               "last_checked_at": null, "last_status": null } ] }
  ```

  `tv_id` is a random local UUID, never derived from the screen ID.
  `device_id` is one UUID per install, minted at the first link and used for
  every TV, so each TV lists Jodd once (measured: a fresh ID per bind piles up
  remotes, §2). `default_tv` is `null` exactly when `tvs` is empty.
- **Keychain**: one entry per TV — service `jodd`, key `tv_screen::<tv_id>`,
  value the screen ID. Unpairing one TV deletes one entry. The flag file is
  written **after** the keychain on link and **before** it on unpair, so a
  crash leaves at worst an orphaned keychain entry, never a row without a
  secret. An orphan is unreachable (no row names its `tv_id`) and is left
  alone — the keychain API cannot enumerate entries, so there is no sweep. If
  a row's secret is missing anyway, the row shows "Needs re-pairing".
- **Same-TV detection without the keychain**: each row also stores
  `screen_hash` — the first 16 hex characters of SHA-256 of the screen ID
  (`sha2` is already a dependency). Link and re-pair compare hashes, so
  "already linked" and "belongs to another row" need no keychain read (on
  macOS, one prompt per row otherwise). The hash cannot be turned back into
  the screen ID.
- **Concurrency**: every write to the file goes through one mutex in
  `tv_link.rs`, as read-modify-write — Settings and a press can race.
- `status()` reads the flag only. `load()` reads the keychain and is called
  only on Link / What's-on-my-TV, inside `spawn_blocking` (as `ready()` is at
  `lib.rs:7790`).
- **The lounge token is never stored** (my call): it is fetched per press,
  which costs one request and keeps one less secret on disk.

### 5.3 Commands — `src-tauri/src/tv_commands.rs`

Registered in the `invoke_handler` list (`lib.rs:9642`); no `#[cfg]`, so
desktop and Android get them. The registration test (`lib.rs:9894`) covers new
`invoke` names.

`TvRow = { tv_id, label, model, is_default, needs_repair, last_status }` is
everything the frontend ever learns about a TV.

- `list_tvs() -> TvRow[]` — flag file only, safe to call from render paths.
- `link_tv(code) -> { row, already_linked: bool }` — `pair`; same screen ID
  as a row → refresh that row; at 5 → `too_many`. Errors `bad_code` /
  `too_many` / `network`.
- `repair_tv(tv_id, code) -> TvRow` — `pair`, then replace that row's secret;
  label/default kept; clears `needs_repair`. Errors as `link_tv`, plus
  `belongs_to_other { label }` when the screen ID is another row's.
- `refresh_tv(tv_id) -> TvRow` — keychain read on the press; token + short
  bind; sets `last_status` (`online` / `unreachable` / `needs_repair`) and the
  model text.
- `rename_tv(tv_id, label)`, `set_default_tv(tv_id)`, `unpair_tv(tv_id)`.
- `tv_now_playing(request_id, tv_id?) -> Outcome` — without `tv_id`, the
  default-first order of §4; with it, only that TV (the picker's second call
  is not needed — the first call returns every candidate). `Outcome` is
  `Found { tv_id, label, url, watched_label?, via_default }`,
  `Choose { candidates: [{ tv_id, label, title_hint?, url, watched_label?, playing }] }`
  or an error `no_tvs` / `nothing_playing` / `needs_repair { label }` /
  `network`. A TV found unlinked is marked `needs_repair` as a side effect.
  Cancellable through the existing `cancel_extraction(request_id)` pattern, or
  a sibling (builder's choice). Asking several TVs runs their `now_playing`
  calls concurrently under one deadline; if `get_lounge_token_batch` accepts
  several `screen_ids` (its name suggests so; unmeasured), one call fetches
  all tokens, else one per TV.
- Device name sent to the TV: "Jodd" (my call; a platform suffix is fine if
  the TV lists remotes by name).

The commands return the URL, never the screen ID or token. Errors carry codes,
not the server's text.

### 5.4 Frontend

- **`ExtractPrefill`** (`src/lib/capture.ts:38`) becomes a tagged union:
  `{ kind: 'capture', captureId, text, title, workflow? } | { kind: 'tv' }`.
  `LessonExtractModal.svelte:97-116` branches on `kind`: `capture` keeps
  today's path (incl. `retireCapture`); `tv` starts the TV request.
- **In the modal, for `kind: 'tv'`:** show "Asking your TV…" + Cancel in the
  Source area; on success set `sourceText` to the URL plus, when there is a
  time, a second line `Watched on TV up to m:ss`; reset `pristine` after
  filling, so closing an untouched filled dialog is not "unsaved". The second
  line reaches the model as context through the existing
  `analyze_ingest_sources` → `context_text` path (`urls.rs:397`); ingest itself
  ignores `t=` (`display_url`, `urls.rs:352`). If the user has typed into
  Source before the answer arrives, the answer does not overwrite it — it is
  appended on a new line (my call).
- **Entry point:** an item "What's on my TV" in `AiMenu.svelte` (beside "Make a
  note with AI…", `:82-87`), which reaches desktop (sidebar ✦) and phone
  (PhoneFolders ✦ → `AiActionsSheet`) at once. It does what
  `AiMenu.makeNote()` does (`:61-63`) plus `extractPrefill.set({ kind: 'tv' })`.
- **Picker:** for `Choose`, the Source area shows the candidates as buttons
  (label, a title hint if the URL analysis has one, the time); a tap fills
  Source as for `Found`. No second network call.
- **Settings:** `TvLinkSettings.svelte` (the list, + Link, each row's menu)
  mounted in `settings/LinksPage.svelte` beside `YoutubeKeySettings.svelte`
  (`:40`), following that component's pattern and tests. The list comes from
  `list_tvs` into a store loaded at startup, which the AI menu also reads, so
  neither awaits IPC to render.
- **Privacy source:** a TV-filled Source is `AiSource::Pasted` (frontend
  `aiSourceFor`, `aiScope.ts:31`), the same as a capture. No new kind.

### 5.5 What goes where on the network

Per press: two HTTPS requests to `www.youtube.com` (token batch, bind) and, if
needed, two more (`getNowPlaying`, subscribe). They carry the screen ID, a
lounge token, the device UUID and the name "Jodd". No AI provider is involved
in the lookup. What happens next is ordinary ingest, under its existing privacy
rules.

## 6. The probe stays, rewired

`examples/lounge_probe.rs` stays as the diagnostic for the day YouTube changes
the protocol, like `ingest_probe` for InnerTube — but rewritten to call the
shipped `tv_lounge` module (`pair`, `now_playing`, plus a `--raw` dump of every
event, each led by the milliseconds since `now` began). It opens neither the
database nor the keychain. It never sees the lounge token and `now` never prints
the screen ID; `pair` prints it once, by design, marked private. It sends the
app's own device ID (`TvLinks::app()?.device_id()`, the flag file only; the env
var `LOUNGE_DEVICE_ID` overrides), so probe runs reuse the app's one "Jodd"
remote on the TV instead of adding one per run. Windows examples need the GUI
manifest `build.rs` adds (gotcha #3) — nothing new to do.

## 7. Tests

- **Parser fixtures** from the spike's dumps, with session IDs, `cpn` and the
  TV's device IDs in `loungeStatus` replaced by placeholders:
  1. bind reply with `playlistModified` carrying `videoId`;
  2. stream where `nowPlaying` has only `listId` and the video ID comes on
     `onVideoQualityChanged` → video, no time;
  3. stream with a full `nowPlaying` → video + 72 s;
  4. duplicate events across chunk reads → each counted once;
  5. a chunk split mid-JSON across two reads.
- **`now_playing` against a local server** (endpoints injected): stops early
  when bind already has both; hits the deadline and returns video without time;
  token batch with no screen → `Unlinked`; pairing 404 → `BadCode`.
- **Anti-goal test:** across every request the client sends in these tests,
  the only `req0__sc` value is `getNowPlaying`, and no bind carries `method`,
  `params` or `videoId`.
- **Priority test:** run 3's fixture → `wnXoCNd57Ks` at 147 s, never its
  `autoplayUpNext` IDs; a synthetic stream with only a track event for A and an
  up-next event for B → A.
- **`tv_link`** (temp config dir; the `youtube_key` tests are the template):
  `list_tvs` reads no keychain; first link becomes default; unpairing the
  default promotes the oldest; the same screen ID twice → one row; a 6th link →
  `too_many`; re-pair keeps label and default; re-pair with another row's
  screen ID → `belongs_to_other`; label clashes get " (2)"; a row whose secret
  is missing reads as `needs_repair`; same-TV checks read no keychain (hash only).
- **Choosing a TV** (pure function over per-TV results): default with a video
  → only it, others not asked; default empty, one other has a video → it, with
  `via_default: false`; two others, one playing and one paused → the playing
  one; two playing → `Choose`; all empty → `nothing_playing`; default
  unlinked → marked `needs_repair` and the others answer.
- **Frontend:** AiMenu item opens Settings when unlinked and the modal with
  `{kind:'tv'}` when linked; modal fills Source and keeps typed text;
  `TvLinkSettings` states.
- The CI gates in CLAUDE.md "Verify", unchanged.

## 8. Docs that change with it

- `docs/ROADMAP.md`: a new item (number assigned at the end of the list —
  numbers are permanent).
- `docs/GOTCHAS.md`, only if the build finds a new constraint; the
  "`nowPlaying` may carry only `listId`" fact lives in this spec and the parser
  comment.
- Jodd-Homepage `privacy.html`: linking a TV lets Jodd ask YouTube what that TV
  is playing, only when you press the button; Jodd keeps the pairing in the
  device's keychain and sends YouTube nothing else; Unpair in Jodd forgets that TV.
- CHANGELOG at release.

## 9. Live pass (after the build)

1. Mac: link with a code; press with a video playing → Extract filled with the
   right link and time (within a few seconds of the TV's clock). Note whether
   the TV shows a toast.
2. Mac: press on the TV's home screen → "Nothing is playing…".
3. S23 FE on **mobile data** (proves the cloud path): link separately, press,
   ingest the note end to end.
4. Unpair the only TV in Jodd → item leads to Settings. Separately, link again, then
   remove "Jodd" on the TV's Linked devices and press: record whether Jodd is
   cut off. If it is not, the Unpair copy and privacy.html already say only
   what is true; nothing else changes.
5. Leave the pairing a week; press again (tests screen-ID longevity).
6. Rename, Refresh, Re-pair (with a fresh code) and Set default on the user's
   TV; link the same TV again → "Already linked".
7. A second TV, if the user has one: default playing → default answers; default
   on its home screen, other playing → other answers, labelled. Without a
   second TV the multi-TV path is covered only by the §7 tests — say so in the
   pass report.
8. During the Mac press and the S23-on-mobile-data press, record from
   `lounge_probe now <id> --raw` the elapsed ms at which the first event
   carrying `currentTime` arrives. Tune `HEAD_START` (`tv_commands`) and the
   after-video window from it.
9. `lounge_probe now 0000000000000000000000000000000000000000000000000000000000000000`
   — a bogus screen ID, no secret — shows how Google answers an unknown
   screen. If it is a non-2xx, decide whether the token batch should map it to
   `Unlinked` (today it is `Network`).

## 10. Left to the builder

Exact module split inside `tv_lounge.rs`; whether cancellation reuses
`cancel_extraction` or gets a sibling; the HTTP mock tooling for §7; copy
wording beyond what §4 quotes; the "Watched on TV" time format for videos over
an hour (`h:mm:ss`).
