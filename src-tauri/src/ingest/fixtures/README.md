# URL ingest fixtures

Captured on the date of this file's first commit, by the script in
`docs/superpowers/plans/2026-09-15-url-ingest.md` Task 5 Step 1, from a
residential connection, against public videos `jXtnhyro-QE` (English) and
`ve4f7oz-UPs` (Thai, auto-captions only), plus `aaaaaaaaaaa` (nonexistent).

What was changed from the wire, and nothing else:

- `yt_player_ios_*.json` — kept only `playabilityStatus.{status,reason}`,
  `videoDetails.{videoId,title,shortDescription[0:300]}` and each caption
  track's `languageCode`, `kind`, `name`. **`baseUrl` is redacted** to
  `https://www.youtube.com/api/timedtext?v=<id>&lang=<code>&redacted=1`: the
  real one carries signatures and the capturing IP.
- `yt_caption_*.xml` — the chosen track's body, cut to 40 elements (a window
  around the first `&amp;#39;`, when there is one) with the wrapper re-closed.
- `yt_watch_en.html` — the watch page's `ytInitialPlayerResponse` object,
  trimmed as above, inside a minimal synthesized page.
- `hostile_page.html` — hand-written (Task 4), not captured.
- `yt_player_ios_dubbed.json` — captured 2026-10-18 the same way from
  `J9NQTYUP6k0`, an English video YouTube auto-dubbed into 20 languages:
  every caption track is ASR and Bangla is listed first. Trimmed as the
  other player captures, plus the renderer's `audioTracks[].{captionTrackIndices,
  audioTrackId}` and `defaultAudioTrackIndex`, which is how the original
  audio's track is found.

The captured timedtext XML uses `<transcript><text start="…" dur="…">…</text>
…</transcript>` — flat `<text>` elements directly under `<transcript>`, not
the `<body><p>` shape. `caption_text` handles both `<p>` and `<text>` by
stripping all tags generically (it never looks for a specific tag name), so
no adaptation was needed beyond confirming this shape against the real
capture.

Both captured caption tracks are `kind: "asr"` (auto-generated) — neither
video has a manual track. `grep -o '&amp;#39;' yt_caption_en.xml | wc -l`
(the file is a single line, so `grep -c` would only report `1` for "one
matching line") found **4** occurrences in `yt_caption_en.xml`, so the
double-decode case (`&amp;#39;` → `&#39;` → `'`)
is exercised by both the real capture and the inline
`caption_text_strips_tags_and_decodes_entities_twice` test.

Tests that need a case YouTube did not hand us (no captions, LOGIN_REQUIRED,
a manual track beside an ASR one) derive it from these captures in code and
say so at the call site.

- `tiktok_subtitle.vtt` — the whole subtitle track of one captured TikTok
  (`tt/2`, Thai, auto-generated): `WEBVTT`, then all 11 cues (the capture has
  fewer than the 12 the plan allowed, so nothing was cut). No URL, cookie or
  token appears in it — checked with a grep for `http`, `token`, `cookie`
  and `=`. TikTok's cues are long, non-overlapping, one text line each;
  `social::vtt_to_text` is tested against it (Task 3) and the TikTok parser
  reuses it (Task 4).

- `tiktok_video.html` — the real `tt/2` page (captured 2026-10-18), cut down to
  a minimal HTML shell holding only the `__UNIVERSAL_DATA_FOR_REHYDRATION__`
  script, and in it only the keys `tiktok.rs` parses:
  `webapp.video-detail.{statusCode,statusMsg,itemInfo.itemStruct.{id,desc,
  author.{uniqueId,nickname},video.{duration,playAddr,subtitleInfos[]}}}`.
  `desc`, handle, nickname, id and duration are verbatim. **Redacted:**
  `playAddr` keeps its host and path and its whole query string becomes
  `?redacted=1` (the real one carries `signature`, `tk`, `expire`…); the
  subtitle `Url` loses the two signature/expiry path segments (now
  `redacted/redacted`) and its query becomes `?redacted=1`. No cookie or
  token value was in the kept keys; the page's `Set-Cookie`s (`ttwid`,
  `tt_chain_token`, `msToken`, `tt_csrf_token`) are not in the file, and the
  mockito test supplies placeholder values (`TW`, `CT`, `MS`) instead.
- `tiktok_dark_post.html` — the real `tt/3` page reduced the same way to
  `{"statusCode":10240,"statusMsg":"item is dark post"}`: TikTok's answer for
  a post that is not public. Everything else on that page was dropped.
- Cases the owner's links did not give (no `uniqueId`, no `webvtt` track, a
  moved scope key, a refusal with no message) are inline strings in the tests
  and say so at the call site.

- `instagram_reel_mobile.html` — the `<head>` meta tags of the real `app/ig.html`
  capture (captured 2026-10-18 with the mobile Safari UA), cut down to a
  minimal HTML shell holding only `og:type`, `og:site_name`, `og:title`,
  `og:url` and `og:description`, byte for byte (the Thai caption keeps its
  `&#xe21;` references, `@` is `&#064;`). **Dropped:** `og:image`, whose
  signed CDN query string carries a signature and expiry, and everything else
  on the 670 KB page. No cookie or token was in the kept tags.
- `instagram_embed.html` — the real `/reel/<code>/embed/captioned/` capture
  (`app/ige.html`), cut to the one script neighbourhood that holds both
  `video_duration` and `video_url` (1.7 KB, about 1 750 characters apart in the
  page), inside a minimal `<script>`. It is kept **still escaped**, exactly as
  the page serves it: JSON inside a JSON string, so every quote is `\"` and
  every slash `\\\/`. Nothing in it was re-encoded. **Redacted:** every URL
  query string — the CDN video's and the owner's profile picture's — became
  `?redacted=1`; the hosts and paths stay (`instagram.fbkk22-4.fna.fbcdn.net`
  is a real CDN host, which is the point of the allowlist test). The capture's
  `video_duration` is 143.233.
- A login-redirect page (what the desktop UA gets), a captionless reel, a
  handle in an unexpected shape, a one-layer or unescaped `video_url`, and the
  `&` / `%3D` escapes (the capture's URL uses a literal `&`) are
  inline strings in the tests and say so at the call site. Separately from the
  committed tests, all four captures (`ig/{1..4}`) were parsed once and every
  unescaped `video_url` matched the independently captured `vurl.txt`.

- `facebook_reel_mobile.html` — the `<head>` meta tags of the real `fb2/1.m.html`
  capture (a `/reel/<id>` link, captured 2026-10-18 with the mobile Safari UA;
  `app/fb.html`, the `/share/r/<code>/` capture, carries the same tags), cut
  down to a minimal HTML shell holding `og:type`, `og:title`
  (`<caption> | <Page name> | Facebook`), `og:description`, `og:url` and the
  `og:video*` tags, byte for byte (Thai keeps its `&#xe14;` references).
  **Dropped:** `og:image` (signed CDN query string) and `og:locale`.
  **Redacted:** the three `og:video` URLs keep host and path (the real
  `video.fbkk22-2.fna.fbcdn.net` CDN host, which is the point of the allowlist
  test) and their whole query string — `_nc_*`, `oh`, `oe`, `bitrate` — became
  `?redacted=1`. Consequently the capture's `&amp;`-encoded query is **not** in
  the file; the `&amp;` decoding is asserted on an inline string.
- `facebook_post_mobile.html` — the same reduction of `fb2/3.m.html` (a
  `permalink.php?story_fbid=…&id=…` post, mobile UA): no `og:video`, `og:title`
  is the page name, `og:description` is the post text and ends in `...` where
  Facebook cut it (201 characters), `og:url` is the canonical post URL
  (percent-encoded path). `og:image`, `og:image:alt` and `og:locale` dropped.
  Nothing needed redacting. Its `og:type` is `video.other`, like a video
  page's: what keeps it a post is that `og:url` names a post.
- The desktop-UA answer (`/reel/` → HTTP 400, `<title>Error</title>`), the
  logged-out interstitial and a lookalike `og:url` are inline strings in the
  tests and say so at the call site.

- `x_tweet_video.json` — the syndication JSON
  (`cdn.syndication.twimg.com/tweet-result?id=2111113436314648758&token=a`,
  captured 2026-10-18, default UA, no cookie) of a post with one `video`,
  reduced to the keys `x.rs` parses: `text`, `user.{name,screen_name}` and per
  media `type`, `video_info.duration_millis`, `video_info.variants[]`. Bytes
  of every kept value are the wire's. **Dropped:** everything else (ids,
  counts, entities, `photos`, `quoted_tweet`, `edit_control`, avatar and
  thumbnail URLs). **Redacted:** the HLS variant's `?v=cfc` query became
  `?redacted=1`; the four `video/mp4` URLs carry no query. Note the capture's
  `text` ends in the `https://t.co/…` link for the attached media and is cut at
  `display_text_range` (277 characters) — it is the long post's truncated form. `x.rs` sets `caption_maybe_cut` for it: a `text` of 270+ characters with no readable `note_tweet` text. The gif reply's 237-character `text` does not set it.
- `x_tweet_gif_reply.json` — the same reduction of tweet
  `2111375646223933650` (a reply carrying four `animated_gif`s: `video_info`
  with one `video/mp4` variant of `bitrate: 0`, no `duration_millis`). Nothing
  needed redacting.
- **`note_tweet` is not in either fixture, deliberately.** Both captures carry
  it as `{"id": "…"}` alone — no `note_tweet_results`, so no full text — and
  the fixtures keep only parsed keys. The path `x.rs` reads for a long post's
  body (`note_tweet.note_tweet_results.result.text`) has therefore NOT been
  seen on the wire; the test for it uses an inline string and says so. Other
  inline strings: a mixed photo/gif/video list, variants without a bitrate or
  with an `http://` URL, handles of the wrong shape, and bodies that are not
  a tweet (`{}`, a tombstone, HTML).

## Audio remux fixtures (`audio/remux.rs`)

**Generated, not captured** — the real downloads are other people's clips.
A 3 s 440 Hz tone over a 64×64 blue frame; made on macOS 27.0.1 with
ffmpeg 8.1.2 and the system `afconvert`, which (unlike ffmpeg's own `aac`
encoder) writes HE-AAC. ffmpeg is a developer tool here, never a runtime
dependency.

```sh
ffmpeg -f lavfi -i "sine=frequency=440:duration=3:sample_rate=44100" -ac 2 tone.wav
afconvert -f mp4f -d aach -b 48000 tone.wav tone_he.m4a          # HE-AAC
V='-f lavfi -i color=c=blue:size=64x64:duration=3:rate=10'
X='-c:v libx264 -preset veryslow -crf 51 -pix_fmt yuv420p'
ffmpeg $V -i tone_he.m4a -map 0:v -map 1:a $X -c:a copy -movflags +faststart audio_he_aac.mp4
ffmpeg $V -i tone_he.m4a -map 0:v -map 1:a $X -c:a copy audio_moov_at_end.mp4
ffmpeg $V $X video_no_audio.mp4
```

(`$V`/`$X` word-split as in bash; in zsh write `$=V`/`$=X`.)

- `audio_he_aac.mp4` (7 926 B) — top level `ftyp moov free mdat`
  (`moov` first, from `+faststart`); a `vide` and a `soun` track. ffprobe:
  `codec_name=aac profile=HE-AAC sample_rate=44100 channels=2
  duration=3.016009 nb_frames=67`. The sound track has an edit list
  (`media_time` 4224, 3016 ms — the encoder's priming), which the remux
  keeps, and which is why ffprobe says 3.016 s, not the 3.111 s of samples.
- `audio_moov_at_end.mp4` (7 926 B) — the same streams, top level
  `ftyp free mdat moov`: ffmpeg without `+faststart` writes `moov` last.
- `video_no_audio.mp4` (2 238 B) — the video track alone.

Regenerating changes the bytes (x264 and afconvert versions); the tests
compare the remux's output with its own input, so any regenerated set works,
except `FFPROBE_AUDIO_SECS` in `remux.rs`, which must be re-read from
`ffprobe -show_streams`.
