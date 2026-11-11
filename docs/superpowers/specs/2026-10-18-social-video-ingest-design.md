# Social video ingest — TikTok, Instagram, Facebook, X, and speech-to-text

> Status: **design agreed in brainstorming (2026-10-16 → 2026-10-18), awaiting
> the owner's read of this document.** Builds on
> [2026-09-15-url-ingest-design.md](2026-09-15-url-ingest-design.md) (the
> fetch → map → reduce pipeline, `ingest/` module, SSRF guard) and
> [2026-09-27-ingest-workflows-and-transcript-design.md](2026-09-27-ingest-workflows-and-transcript-design.md)
> (workflow modes, Transcript raw / Clean). Every number in "Measurements" is
> one run, from one Mac, on one residential connection in Thailand — a
> signal, not a guarantee (see "Risk").

## 1. Intent

URL ingest turns a YouTube link into a note because YouTube publishes caption
tracks that Jodd can read. Every other video platform the owner uses is
treated as an ordinary web page: `urls::classify` returns `UrlKind::Web`, the
page reader drops `<script>` (where these sites keep their data), and the LLM
receives almost nothing. Separately, sharing an Instagram Reel into Jodd from
the phone produces a note titled **"instagram.com"**: the share sheet never
looks at the link, so `capture::default_title` falls back to the host
(`capture.rs:168-170`), and when the user hands the share to AI that default
is forwarded as a title override (`CaptureSheet.svelte:262` →
`LessonExtractModal.svelte:97` → `ingest/run.rs:587`) that beats the AI's own
title. Fetching platform content does not fix this by itself; §5.6 does.

The owner watches **knowledge clips** — how-tos, AI tooling, work tips — in
**Thai and English, often both in one clip**, on an **Android phone, an
Android tablet and a Mac**, and wants the same result Extract gives a YouTube
link today ("like the Extract we have, maybe more").

The knowledge in such a clip is mostly **spoken**. A caption under the video
is a teaser at best. So this is two pieces of work that only make sense
together:

1. **Layer 1 — know the platform.** Recognise TikTok, Instagram, Facebook and
   X links; fetch the post's caption, author and (where the platform offers
   one) its own auto-transcript; title shared links properly.
2. **Layer 2 — speech-to-text.** When no platform transcript exists (or the
   user asks), download the clip, extract its **audio track only**, and send
   it to a speech-to-text endpoint the user configured — a new
   `ingest/audio.rs`.

### Success

- Pasting or sharing any of the measured links (Appendix A) gives a note whose
  `## Sources` entry names the author and caption, and whose body is built
  from **what was said in the clip** whenever the clip could be reached.
- A shared video link is never titled by its host: a plain Save uses the
  caption/author, an AI-made note the AI's own title (D9, D13).
- The user always knows **where audio went**: before it is sent (the modal
  says so), and afterwards (the note says so).
- Nothing about the platforms requires the user to sign in to them, in Jodd or
  anywhere else.

## 2. Goals, non-goals, anti-goals

**Goals**

- TikTok, Instagram, Facebook (Reels and video posts) and X recognised as
  their own source kinds, desktop and Android alike.
- Platform auto-transcripts used when they exist (TikTok, measured).
- A user-configured, OpenAI-compatible speech-to-text endpoint, used for the
  clips that have no transcript.
- The Share to Jodd sheet titles a shared video link by its caption/author.

**Non-goals (this spec)**

- **What is shown on screen** (slides, code, text overlays). The owner asked
  for it "if there is a simple tool, otherwise later". There is no simple
  tool: it needs frames cut out of the video, which means H.264 decoding — no
  pure-Rust decoder, and Android cannot spawn `ffmpeg`. It is the follow-up in
  §11.
- **On-device Whisper.** `docs/LOCAL-AI-RESEARCH.md` picks whisper.cpp for a
  future local pipeline; this spec does not build it. A local server that
  speaks the same API (§5) is supported on day one through "Custom".
- Playlists, profiles, hashtag pages, Stories, live streams, carousels of
  images, and Facebook posts whose video needs a login (§4.3).
- Following "watch the full video in the comments" — comments need a login.
- Changing YouTube's path. It keeps its own captions route; speech-to-text is
  **not** added as a YouTube fallback here (YouTube blocks audio download —
  the URL-ingest spec's PO-token finding).

**Anti-goals — a failure even if it technically works**

- **Any use of the user's own platform session** — a signed-in webview,
  imported browser cookies, a stored platform credential. The owner's Meta,
  TikTok and X accounts must never be put at risk by Jodd.
- **Audio leaving the device without the user being told where.** The owner
  said they don't need everything on-device, but it must be communicated
  clearly.
- **A note that looks complete when it is not** — a caption summarised as if
  it were the talk, or a caption Facebook cut with "..." presented as whole.
- **Sending the whole video** to the speech-to-text endpoint. Only the audio
  track leaves.

## 3. Measurements (2026-10-16 → 2026-10-18)

Plain HTTP, no account, no cookies except those the platform set on the page
response itself. Throwaway probes in the session scratchpad; the owner's own
links (Appendix A).

| Platform | Caption + author | Platform transcript | Video file | Audio codec |
|---|---|---|---|---|
| **TikTok** 2 of 3 | ✅ `__UNIVERSAL_DATA_FOR_REHYDRATION__` → `webapp.video-detail.itemInfo.itemStruct` (`desc`, `author`, `video.duration`) | ✅ `video.subtitleInfos[]` — `tha-TH`, `webvtt`, `Source: ASR`; the VTT URL needs **no** cookies | ✅ `video.playAddr` — **403 without** the page's cookies (`ttwid`, `tt_chain_token`, `msToken`, `tt_csrf_token`), 200/206 with them | **HE-AACv2** |
| TikTok link 3 | ❌ `statusCode 10240`, `"item is dark post"` | — | — | — |
| **Instagram** 5 of 5 | ✅ only with a **mobile Safari UA**: `og:title` / `og:description` on `/reel/<id>/` (desktop UA → redirect to `/accounts/login/`) | ❌ none found | ✅ `/reel/<id>/embed/captioned/` (mobile UA) carries a `video_url` (escaped JSON) → one redirect → MP4 | **HE-AAC** |
| **Facebook Reel** (`/reel/<id>`, `/share/r/<code>`) | ✅ mobile UA: `og:title` ("… \| Page \| Facebook"), `og:description` | ❌ | ✅ **`og:video`** on the mobile page is the MP4 URL itself; `/share/r/` needs no redirect — the share page carries the Reel's tags | (not probed) |
| **Facebook post** (`permalink.php?story_fbid=pfbid…`) | ✅ mobile UA: page name + post text, **cut with "..."** when long | ❌ | ❌ six routes tried (mobile page, `og:url`, `plugins/video.php`, `plugins/post.php`, both UAs) | — |
| **X** 2 of 2 | ✅ `cdn.syndication.twimg.com/tweet-result?id=<id>&token=<t>` → `text` (long posts: `note_tweet`), `user` | ❌ | ✅ `mediaDetails[].video_info.variants[]` (`video/mp4`) on `video.twimg.com`; GIFs come as `animated_gif` | AAC-LC |

Other facts that shape the design:

- **Sizes.** A 143 s Instagram Reel was **27.7 MB** — over the 25 MB upload
  limit of OpenAI-style transcription APIs. Its audio track alone is ~1 MB
  (56 kb/s). One Facebook Reel was a whole film: **408 MB**.
- **TikTok's own Thai ASR** heard "Claude Code" as "คลอสโค้ด". Usable, not
  exact. Whisper on the same clip was **not measured** (§10).
- The owner's Facebook examples all said "ดูวิดีโอฉบับเต็มจากลิงค์ในคอมเม้นท์"
  — the post is a teaser and the real clip is in the comments.
- **The `?srtk=` / `igsh` query** on Instagram app links is a share-tracking
  value. Fetching by Reel id alone works; the query is never needed.

## 4. Decisions

| # | Decision |
|---|---|
| D1 | **Four new source kinds**, each in its own file under `ingest/`, each with **one block of platform constants** (UA, endpoints, JSON paths' entry keys) — the `youtube::IOS` pattern, so a breakage is fixed in one place. |
| D2 | **Transcript order: platform transcript → speech-to-text → caption only.** A platform transcript is free and sends nothing anywhere; it wins by default. The Extract modal offers **"Transcribe the audio myself"** to override it for one run (TikTok's ASR mishears names). |
| D3 | **Speech-to-text is its own app-level setting**, separate from the LLM provider: no current provider (agent CLIs, the chat-completions HTTP provider) accepts audio. One shape — OpenAI's `POST {base}/audio/transcriptions`, multipart — because it covers cloud services *and* a server the user runs (the AI host), on every platform, with no native dependency. |
| D4 | **Only the audio track leaves the device**, remuxed into an audio-only `.m4a` **without decoding** (§5.3). HE-AAC is what TikTok and Instagram use, and no pure-Rust decoder handles it; remuxing sidesteps decoding entirely and makes files ~10–20× smaller. |
| D5 | **Communicating where audio goes** has three places: (1) saving the setting shows the host and the user confirms it ("Audio from video links will be sent to `api.groq.com`"); the confirmation is **bound to that host**, and a changed base URL asks again; (2) the Extract modal states it before the run, per clip; (3) the note's `## Sources` entry records it. |
| D6 | **No speech-to-text configured is a normal state**, not an error: the clip is ingested from its platform transcript or caption, the source is `Partial`, and the reason says that setting up speech-to-text would add the spoken content. |
| D7 | **Caps.** Skip download when the platform-stated duration exceeds **30 min**, or the response's size exceeds **300 MB**; skip upload when the remuxed audio exceeds **25 MB** (the endpoint limit; configurable per endpoint). Skipped → `Partial("too long to transcribe (N min)")` with caption/author kept. *(Numbers are the designer's call — see §12.)* |
| D8 | **Facebook posts are honest `Partial`s.** Page name + post text; a text ending in "..." or "…" is labelled as possibly cut off in the status, and the map/synthesis prompts already treat the status as part of the source. The status for a post says Facebook does not show the video without signing in. |
| D9 | **Share to Jodd fills the title in the background.** While the sheet is open, a no-LLM `peek_link_metadata` fetches caption/author for a recognised video link and fills the title field **only if the user has not edited it**. Saving never waits for it. YouTube is included (desktop shares of a YouTube link are titled "youtube.com" today, `capture.rs` test `default_title_order`). |
| D10 | **Platform cookies live for one fetch, in memory.** TikTok's download needs the cookies its page response set; they are carried to the media request and dropped. Never persisted, never shared between sources, never the user's. |
| D11 | **Media URLs are host-allowlisted** on top of the SSRF guard: a media URL lifted from page JSON is fetched only if its host ends with that platform's CDN suffix (§5.2). |
| D12 | **The note keeps the canonical URL** for the four new kinds — tracking queries stripped at entry, share codes replaced once the fetch reveals the real post (§5.1). |
| D13 | **A title the user did not type is not an override.** The share sheet forwards its title to Extract only when the user edited the field; a default (host or D9-filled) title is not forwarded, so an AI-made note gets the AI's title, as Extract does for YouTube. A plain Save uses the field as shown. |
| D14 | **Provenance lives inside the source text, so the stored block's format does not change.** Each fetched source's text opens with labelled lines — `Platform:`, `Author:`, `Caption:` (with "(may be cut off)" under D8), `Transcript source:` (`TikTok auto-captions` / `api.groq.com, whisper-large-v3` / `none — <reason>`) — then a blank line and the transcript. `## Sources` lines and Re-extract read them back from there; older Jodd builds parse the block unchanged and just see more text. |
| D15 | **Speech-to-text calls are receipted, not budgeted.** One call per source, never retried in a loop, so `llm::budget` does not apply; `llm::receipts` records stage `transcribing`, the host, audio seconds and bytes — never content. |

## 5. Approach

```
Extract modal / Share sheet hand-off
   │ analyze_ingest_sources(...)          — classify now knows 4 more kinds; still no network
   ▼
ingest_sources(... , transcribe_override)
   ├─ 1. Fetch       ingest::{tiktok,instagram,facebook,x}::fetch_meta     caption, author, duration,
   │                                                                         platform transcript?, media URL?
   ├─ 1b. Transcript platform transcript                      ── if present and not overridden → done
   │                 else ingest::audio:: download → remux → transcribe     ── if STT configured & under caps
   │                 else caption only → Partial(reason)
   ├─ 2. Map / 3. Reduce / 4. Write — unchanged (the source text is the transcript, prefixed by the caption)
```

The source text handed to the map step is D14's labelled header (platform,
author, caption, transcript source) followed by the transcript. A
caption-only source carries the header alone.

### 5.1 `ingest/urls.rs` — classification

`UrlKind` gains `TikTok { id }`, `Instagram { shortcode }`,
`Facebook { url_kind }` and `X { id }`. Recognised shapes, from the owner's
links plus the obvious siblings:

| Kind | Shapes |
|---|---|
| TikTok | `tiktok.com/@user/video/<id>`, `vt.tiktok.com/<code>` and `tiktok.com/t/<code>` (short links: resolve via the guarded redirect follow), `m.tiktok.com/v/<id>` |
| Instagram | `/reel/<code>/`, `/reels/<code>/`, `/p/<code>/`, `/tv/<code>/` — query dropped |
| Facebook | `/reel/<id>`, `/share/r/<code>/`, `/share/v/<code>/`, `/<page>/videos/<id>`, `/watch/?v=<id>`, `fb.watch/<code>` → video; `permalink.php?story_fbid=…`, `/<page>/posts/<id>`, `/share/p/<code>/` → post |
| X | `x.com/<user>/status/<id>`, `twitter.com/…`, `mobile.` variants |
| Unsupported, with a reason | profiles, hashtag/explore pages, Stories, live, `instagram.com/<user>/` |

**Which URL a note keeps (D12).** Today `display_url` only governs what the
LLM sees; the note keeps the URL as given, in the `## Sources` link
(`llm/markdown.rs:292-294`) and the stored block's `URL:` line
(`ingest/stored.rs:60`) — a YouTube `si=` already reaches notes. For the four
new kinds, the URL is **canonicalised where it enters the pipeline**, so the
note, the `cites` edge, duplicate detection and the LLM all see the same
string:

- Tracking queries are removed with no network: `srtk`, `igsh`, `igshid`,
  `is_from_webapp`, `sender_device`, `mibextid`, `rdid`, `share_url`, `s`
  and `t` on X. `/reels/<code>/` becomes `/reel/<code>/`.
- **Identity in the query is kept**: Facebook `permalink.php` keeps
  `story_fbid` and `id`; `watch/?v=` keeps `v`.
- **Share codes in the path** (`/share/r/<code>/`, `/share/v/`, `/share/p/`,
  `vt.tiktok.com/<code>`, `tiktok.com/t/<code>`, `fb.watch/<code>`) cannot
  be canonicalised offline. `classify` accepts them as-is; the fetch step
  replaces the source's URL with the canonical one when the platform reveals
  it (`og:url`, the final redirect, TikTok's `itemStruct.id` + author), and
  keeps the share URL when it does not.

YouTube is out of scope for D12 and keeps today's behaviour. `log_form`
follows the same canonical form.

### 5.2 One file per platform

Each exposes `fetch_meta(url, kind, policy, cancel) -> PlatformMeta` and a
`#[cfg(test)]` parser over trimmed captured fixtures.

```rust
pub struct PlatformMeta {
    pub title: Option<String>,        // what the Sources list and the share sheet show
    pub author: Option<String>,
    pub caption: Option<String>,
    pub caption_maybe_cut: bool,      // D8
    pub duration_secs: Option<f64>,
    pub transcript: Option<PlatformTranscript>, // { text, language, source: "TikTok auto-captions" }
    pub media: Option<MediaRef>,      // { url, headers, cookies } — D10
    pub unavailable: Option<String>,  // "dark post", login wall, removed…
}
```

| File | Route (measured, §3) | Media host suffixes (D11) |
|---|---|---|
| `ingest/tiktok.rs` | Page with Jodd's desktop UA → rehydration JSON. `subtitleInfos`: prefer the user's-language track if several, else the first; VTT → text with `youtube::caption_text`-style cleanup. `playAddr` + the page response's `Set-Cookie`s. A non-zero `statusCode` → `unavailable(statusMsg)`. | `tiktok.com`, `tiktokcdn.com`, `tiktokcdn-us.com`, `tiktokv.com` |
| `ingest/instagram.rs` | Mobile-Safari UA. `/reel/<code>/` for `og:` tags (title holds author + caption); `/reel/<code>/embed/captioned/` for `video_url` and `video_duration`. | `cdninstagram.com`, `fbcdn.net` |
| `ingest/facebook.rs` | Mobile-Safari UA on the URL as given (share links included). `og:type = video.other` + `og:video` → video; no `og:video` → post — except a `video.*` page whose `og:url` is not a post link, which is a clip Facebook did not show (final review: measured posts are `video.other` too). | `fbcdn.net` |
| `ingest/x.rs` | Syndication JSON. `token` is computed the way X's embed script computes it if a constant stops working (`token=a` worked on 2026-10-18). Video: the **lowest-bitrate** `video/mp4` variant — audio is the same in every variant. `animated_gif` has no audio → no transcription. | `twimg.com` |

`GuardedRequest` gains a cookie list; `send_guarded` gains a way to return
the response's `Set-Cookie`s. No cookie store is introduced (the guard's "no
cookie store" property stays true across fetches).

### 5.3 `ingest/audio.rs` — download, remux, transcribe

**Download.** `send_guarded` with a **media policy**: per-read idle timeout
instead of the 20 s whole-request timeout (a 28 MB clip on a phone connection
needs longer), `MAX` from D7, streamed to a temp file under a cache dir, deleted when the
source finishes — success, failure or cancel. `paths.rs` has only
`config_base` and `data_base` today, so this adds `cache_base()` (desktop: the
OS cache dir + `jodd`; Android: the app's cache dir, which the OS may clear),
and a startup sweep that deletes leftovers from a crash. Cancellation races every chunk.

**Remux.** Read the MP4's audio track (`moov` → `trak` with `soun` handler)
and write an audio-only MP4 (`.m4a`) with the same `esds`/sample table — **no
decode, no re-encode**. Pure Rust (a crate such as `mp4`, or a small purpose
writer — builder's call), must cross-compile for the Android NDK targets the
release workflow builds. Handles `moov`-at-end files. A file with no audio
track → `Partial("the clip has no sound")`.

**Transcribe.** `POST {base_url}/audio/transcriptions`, multipart
`file=<audio.m4a>`, `model=<model>`, `response_format=json` (or `verbose_json`
where segment times are wanted later), **no `language`** by default — Thai and
English are mixed within a clip, and forcing one damages the other. A
per-endpoint optional language setting exists for users whose clips are
single-language. Bearer key from the keychain. Same cancel token. This
request does **not** go through the ingest SSRF guard — like the HTTP LLM
provider, its address is the user's own choice and may legitimately be on
their LAN (the AI host).

**Errors map to statuses**, never to a failed ingest (URL-ingest Decision 2):
HTTP 401/403 → "speech-to-text rejected the key"; 413 → "too long to
transcribe"; network → "speech-to-text could not be reached"; an empty
transcript → "no speech found". Each falls back to the platform transcript or
caption.

### 5.4 Speech-to-text settings

`app_stt.json` beside `app_llm.json`, mirroring `app_llm_config.rs`:
`{ preset, base_url, model, language, max_upload_mb, confirmed_host }`, the
key in the keychain as `stt_api_key::__app__`. App-level only: audio is not
account data, and the same clip is the same clip whatever account the note
lands in. (The account's existing `data_allowed` still gates the LLM steps,
unchanged.)

Settings UI: a **Speech-to-text** section in app settings with three presets
plus Custom, and a **Test** button that transcribes a bundled 3-second clip
(Thai + English) so the user sees it work before relying on it:

| Preset | base_url | model | Status |
|---|---|---|---|
| OpenAI | `https://api.openai.com/v1` | `whisper-1` | **not exercised** — gotcha #7: mark the row so until a live run |
| Groq | `https://api.groq.com/openai/v1` | `whisper-large-v3` | **not exercised** |
| My server (Custom) | e.g. the AI host | user-entered | the owner's AI host has no STT service yet — §11 |

`confirmed_host` (D5): Save shows "Audio from video links will be sent to
**`<host>`** to turn speech into text." and stores the host only when the
user confirms. A `base_url` whose host differs from `confirmed_host` is
treated as not configured until confirmed again.

### 5.5 Pipeline and UI changes

- `IngestStage` gains `Downloading` and `Transcribing` (`index`, `total`,
  `url_host` as today — never content).
- `analyze_ingest_sources` returns, per source, the kind and whether
  speech-to-text **would** be used if needed; the modal shows one line under
  the source list when any selected source may be transcribed: "Audio from
  these clips will be sent to `api.groq.com`." — or, unconfigured: "Spoken
  content needs speech-to-text — set it up in Settings. Captions will be used
  instead." with a link to the section.
- **"Transcribe the audio myself"** (D2): a checkbox shown only when a
  selected source is a platform with its own transcripts and STT is
  configured.
- Source-kind labels in `LessonExtractModal.svelte` (today `'Video' | 'Page'
  | 'Skipped'`) gain the platform name, so the list says "TikTok", not
  "Page".
- `## Sources` lines carry where the text came from: `TikTok auto-captions`,
  `transcribed by api.groq.com (whisper-large-v3)`, or `caption only — <reason>`,
  read from D14's `Transcript source:` line — at first write and on
  Re-extract alike.
- The stored Source block is unchanged in format (D14); Re-extract still
  needs no refetch and no audio.
- Transcript mode (raw / Clean) applies to the result exactly as it does to a
  YouTube transcript.

### 5.6 Share sheet title (D9)

`peek_link_metadata(url) -> Option<{ title, author }>`: classify; for the four
new kinds, run that platform's `fetch_meta` **without any media or transcript
request**; for YouTube, a new metadata-only call — `youtube::request_player`
for the title and author, with **no caption request** (`fetch_youtube` always
fetches captions, so it is not reused). Return a title of the form
`<caption first line> — <author>` (capped like `DEFAULT_TITLE_CHARS`).
`CaptureSheet.svelte` calls it once, after showing `default_title`, sequence-
guarded; it replaces the field only if the field still equals the default it
showed. A failure is silent (the host title stays).

**The hand-off to AI (D13).** `CaptureSheet` tracks whether the user edited
the title field. `extractPrefill.title` is set only when they did; otherwise
it is empty, `titleOverride` stays unset, and `ingest/run.rs` uses the AI's
title. This is what fixes "instagram.com" on the AI path even when the peek
has not returned or failed.

## 6. Security and privacy

- **SSRF:** every platform request and media download goes through
  `send_guarded` (pinned resolution, re-validated redirects); media URLs are
  additionally host-allowlisted (D11), since they come from JSON a page
  served.
- **No user identity on the platforms** (anti-goal 1). Requests carry only
  cookies the platform set during the same fetch (D10).
- **Tracking values** in share links are stripped before the URL reaches the
  note, the LLM or a log (D12, §5.1). Path share codes go to the platform once,
  when the fetch resolves them, and are replaced when it reveals the
  canonical URL.
- **The audio file** is a temp file, deleted on every exit path; never stored
  in a note, never synced.
- **Fetched captions and transcripts are third-party text** — they flow
  through the same sanitising write path as all ingest output (URL-ingest
  Decision 10). Masking (`CheckedProvider`) applies as it does to any source
  text.
- **The user-agent strings** are browser-like and, for Instagram and
  Facebook, an iPhone Safari string. This is the same posture as YouTube's
  IOS InnerTube client: unofficial, owner-accepted, isolated in one block per
  platform (§12).

## 7. Platforms

Everything is HTTP + pure Rust, so the same code runs on macOS, Windows and
Android; no child process, no webview. Android specifics the builder must
exercise: the temp-file path, the Channel progress across the longer
`Downloading`/`Transcribing` stages, and the remux crate under the NDK
cross-build in `release.yml`.

## 8. Testing

- **Parsers:** trimmed fixtures captured from the owner's links (TikTok page
  JSON incl. `subtitleInfos` and a dark-post page; Instagram mobile page and
  embed; Facebook Reel and post mobile pages; X syndication JSON incl.
  `animated_gif` and `note_tweet`). Fixtures must have every personal or
  session value (cookies, tokens, signed CDN query strings) replaced.
- **Classifier:** every shape in §5.1 and the owner's exact links
  (Appendix A), including tracking queries.
- **Audio:** remux an HE-AAC fixture (a few seconds, generated, not
  downloaded) and assert the output has one `soun` track with identical
  samples; an `moov`-at-end fixture; a video with no audio track.
- **Transcription client:** mockito for 200, 401, 413, timeout, empty text,
  cancel.
- **Pipeline:** fakes for "platform transcript wins", "override transcribes",
  "unconfigured → Partial with the set-up reason", "over cap → Partial",
  "temp file deleted on cancel".
- **Probe:** extend `examples/ingest_probe.rs` with `--social <url>` that
  prints, per stage, what came back (no content beyond lengths and the first
  line) — the tool the next breakage is diagnosed with.
- **Live pass** (Mac, S23 FE, Tab S7): one link per platform from
  Appendix A through Extract and through Share to Jodd, with STT configured
  and not configured.

## 9. Relationship to other work

- **URL ingest (2026-09-15):** this reverses nothing. Its rejection of Whisper
  was about YouTube's PO-token wall and still holds for YouTube; these four
  platforms do not have that wall (§3).
- **AI outbound privacy (2026-10-08):** speech-to-text is a new outbound
  channel *outside* `CheckedProvider` — it sends audio, not text, and masking
  does not apply. D5 is its transparency mechanism; it needs a line in that
  spec's "where data leaves" table and in the public privacy policy
  (`Jodd-Homepage/privacy.html`). The policy change is **its own PR in the
  Jodd-Homepage repo**, prepared with this work and merged by the owner
  **before the release that ships speech-to-text** — a push to that repo's
  `main` is live at once, so it is never pushed from here without the
  owner's go-ahead.
- **Roadmap:** add as a sub-item of URL ingest in `docs/ROADMAP.md`; the
  on-screen-content follow-up and an AI-host STT service go in as new items.

## 10. Not measured — the builder measures before relying on it

- Whisper (any endpoint) on these Thai/English clips, and whether it beats
  TikTok's own ASR — decides whether D2's default order should flip.
- Whether the endpoints accept **HE-AAC in an `.m4a`** (they decode with
  ffmpeg-family libraries, which should — confirm in the live pass).
- Instagram's own auto-captions: none were found in the embed; not searched
  further.
- Anything from an Android device's own network, and repeated fetches (rate
  limits — TikTok's oEmbed endpoint returned `ratelimit triggered` on its
  first call, which is why it is not used).

## 11. Follow-ups (not this spec)

1. **What is on screen** — keyframes → a vision-capable model. Needs a
   decoder; candidates are platform media APIs (AVFoundation, Android
   MediaCodec/`MediaMetadataRetriever`) behind a seam, or doing it on the AI
   host.
2. **Speech-to-text on the AI host** — add an OpenAI-compatible Whisper
   service (e.g. faster-whisper/speaches, or whisper.cpp's server) to the
   `jodd-ai-host` image, so "My server" works without a cloud key.
3. **On-device whisper.cpp** on desktop (`LOCAL-AI-RESEARCH.md`).
4. **Shared video files** — Share to Jodd with a saved video file feeds the
   same remux → transcribe path, and covers what needs a login.

## 12. Calls the designer made (the owner may overrule)

- D7's numbers: 30 min, 300 MB, 25 MB.
- D2's default (platform transcript first).
- App-level, not per-account, speech-to-text setting.
- Presets offered: OpenAI and Groq.
- An iPhone Safari UA for Instagram and Facebook (§6).
- `peek_link_metadata` includes YouTube.
- D12: the note keeps the canonical URL, not the link as shared.
- D13: an AI-made note from a share uses the AI's title unless the user typed
  one.
- D14: provenance is carried inside the source text, so old builds keep
  parsing new notes.
- D15: speech-to-text is receipted but not budgeted.
- The privacy-policy PR lands before the release (§9).

## Appendix A — the owner's links (measurement set and live-pass set)

```
https://www.tiktok.com/@tonhormshop/video/7691929848184376584?is_from_webapp=1&sender_device=pc
https://www.tiktok.com/@tonsr_/video/7696048386981645569?is_from_webapp=1&sender_device=pc
https://www.tiktok.com/@fastmoss_thailand/video/7690727976043826462?is_from_webapp=1&sender_device=pc   (dark post)
https://www.instagram.com/reels/DWy2pKFjXKC/
https://www.instagram.com/reels/DelI8vGCVjC/
https://www.instagram.com/reels/DenT2W_vU0d/
https://www.instagram.com/reels/DeeZ8r9gII0/
https://www.instagram.com/reel/DeeyrDqyckk/?srtk=…                                                       (from the app)
https://www.facebook.com/reel/2601500666993021                                                           (a whole film, 408 MB)
https://www.facebook.com/reel/1120404190712037
https://www.facebook.com/share/r/1FVsvkbtSt/                                                             (from the app)
https://www.facebook.com/permalink.php?story_fbid=pfbid02bt6e15oMG2UvHAMSE9bjrmrsATnabwwrff88S34G7EDaVSEybWmxNKKrYrNAQdSRl&id=61574743782103   (post)
https://www.facebook.com/permalink.php?story_fbid=pfbid02EeD1xzPZFXU6yJS7J3ib7FhbMacax5mPVBcNr7GdkM8z38K2UTeJEcRg3J3z9Fjkl&id=61574743782103   (post)
https://x.com/thegreatest_sv/status/2111113436314648758
https://x.com/deepseek_ai/status/2111375646223933650
```
