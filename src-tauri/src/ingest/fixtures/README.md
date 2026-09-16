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
