# URL ingest — design

> Status: **design / approved in brainstorming** (2026-09-15). Lets Extract
> ingest the *content* behind links — web pages and YouTube transcripts —
> instead of the bare URL text, NotebookLM-style: Jodd fetches each source
> deterministically, a tool-less LLM condenses each one, and a synthesis call
> combines them into a single note.
>
> This is **Spec B**. It builds on **Spec A**,
> [2026-09-15-extract-filing-design.md](2026-09-15-extract-filing-design.md)
> (Inbox destination, folder suggestion), and must be implemented after it.
> **Prerequisite state, 2026-09-15:** both prerequisites are on `main`. Spec A
> landed at `69be98d`; the Extract output-sanitizing fix this spec requires
> (Decision 10) landed at `9b18d51`, with its docs at `82532ad`.
> It supersedes two decisions of the approved-but-unbuilt router spec,
> [2026-07-27-extract-input-router-design.md](2026-07-27-extract-input-router-design.md)
> (roadmap #0b) — see "Relationship to other work".

## Problem

Extract only ever sees the text it is handed. A pasted link is 47 characters
of URL, so the LLM either refuses or invents: in the measured vault
(2026-09-12, `jodd-mcp`), 5 of 11 notes in `Notes/__Extracts__` are records of
an LLM failing to read a bare YouTube URL, and the router spec measured four
such notes from one URL pasted four times in a night.

How links actually arrive is different from how Extract assumes they do.
Searching the same vault (each search capped at 50 results, so indicative
only):

- **YouTube links are the most common kind** — about 37 notes matched
  `youtu`, most of them *link-collection notes*: several video links filed
  into a topic folder, often with a line of the user's own text beside each.
- **GitHub and ordinary web links** are frequent in work folders.
- **Links are frequently glued together without whitespace** — e.g.
  `…Silfrainhttps://www.youtube.com/watch?v=…` — which `db::extract_urls`
  reads as one URL.

So the value is less "paste one URL" than "turn the links I already
collected into knowledge", which the Extract modal's existing
`sourceMode: 'existing'` (ingest an existing note's body) is positioned for.

## Spike — YouTube transcript access (2026-09-15)

Every unofficial route to a transcript meets the same wall: YouTube's
proof-of-origin (PO) token, which only its own player JavaScript can mint. So
the route was measured before it was designed. Throwaway Python probes, run
from a residential connection in Thailand, against two public videos:
`jXtnhyro-QE` (English) and `ve4f7oz-UPs` (Thai, auto-captions only).

| Route | English | Thai (auto-caption) |
|---|---|---|
| InnerTube `youtubei/v1/player`, **ANDROID** client (`clientVersion` `20.10.38`) | ✅ 64,039 chars | ✅ 51,917 chars — **space between every word** (`ยก ตัว อย่าง`), per-word timing format |
| InnerTube `youtubei/v1/player`, **IOS** client (`clientVersion` `20.10.4`, `iPhone16,2`, iOS `18.3.2.22D82`) | ✅ 66,791 chars | ✅ 41,549 chars — **normal Thai** (`ยกตัวอย่าง`), ~4.5× smaller payload |
| WEB watch page → `captionTracks[].baseUrl` | ❌ URL carries `exp=xpe`; body **0 bytes** | ❌ 0 bytes |
| WEB watch page metadata (`videoDetails.title`, `shortDescription`) | ✅ no token needed | ✅ |

Conclusions, and their limits:

- **InnerTube via a mobile client works today, in plain HTTP**, for English
  and Thai — reachable from Rust `reqwest` on every platform, Android
  included. **IOS is the primary client** because its Thai is correct.
- The WEB PO-token wall is confirmed, so the watch page is useful only for
  title and description.
- **One measurement:** one IP, two videos, one moment. Rate limits,
  age-restricted and caption-less videos were not exercised; mobile
  `clientVersion` values are retired by YouTube over time; the route is
  unofficial. The design therefore isolates YouTube and ships a probe to
  re-measure it (see Testing).

Rejected by evidence rather than preference:

| Route | Why not |
|---|---|
| YouTube Data API `captions.download` | Requires the caller to **own** the video; third-party videos answer 403. |
| `yt-dlp` | Frequently meets "Sign in to confirm you're not a bot" and needs browser cookies; and it is a child process, which **Android cannot spawn** (`docs/PLATFORM-MATRIX.md`). |
| Local Whisper transcription | Downloading the audio stream meets the same PO-token wall — it moves the fetch problem, it does not remove it. |
| Hidden webview running YouTube's player | Would mint the token naturally, but is heavy and is exactly the webview surface gotcha #31 shows is unreliable on Android. Kept as the fallback idea if the mobile clients stop working. |

## Decisions (locked in brainstorming)

1. **Jodd fetches; the LLM never holds a tool.** Fetching is deterministic
   Rust; the fetched text reaches the LLM only as data. This works for every
   provider on every platform (agent-CLI providers do not exist on Android,
   and HTTP providers — including a local llama.cpp — can never fetch), and
   limits a hostile page to corrupting the content of one note, the same
   exposure as pasting that page by hand. **This reverses the router spec's
   Decision 1** ("capability delegation, not a home-grown fetcher") and
   **supersedes its Decision 4** (fetching via an agent-CLI web tool, gated
   behind a prompt-injection review): no provider is ever granted a tool.
2. **Web pages and YouTube ship together, with YouTube isolated.** A YouTube
   failure degrades that one source to `Partial` or `Failed`; it never fails
   the ingest or any web source.
3. **YouTube uses InnerTube with the IOS client**, per the spike, falling back
   to title + description from the watch page. No `yt-dlp`, no webview, no
   Whisper.
4. **Two entry points, one mechanism:** text pasted into the Extract modal,
   and an existing note chosen in the modal's existing-note mode. Both go
   through the same URL detection.
5. **Several URLs produce one combined note**, NotebookLM-notebook style.
6. **Combination is sequential map-reduce.** Each fetched source is condensed
   by the existing Extract `SYSTEM_PROMPT` (map); a synthesis call combines
   the condensed digests (reduce). **A single source skips the reduce step**,
   which makes one-URL ingest exactly today's Extract. Rejected: one budgeted
   call (a 50,000-character transcript cut to 20,000 loses 60% of the
   source), and a size-dependent hybrid (two code paths, and output whose
   shape the user cannot predict).
7. **Progress travels over a `tauri::ipc::Channel` scoped to the command
   call**, not an app-wide event: only the waiting modal listens. This is the
   project's first `Channel`; it must be exercised on Android.
8. **All fetched sources live in one Source block**, delimited per source.
   `llm::markdown::extract_source` reads only the *first* Source block
   (`split_once`), so several blocks would silently lose every source after
   the first on Re-extract. Re-extract of a multi-source note re-runs
   map-reduce over the stored text **without refetching**.
9. **Fetching is on by default only when the input is mostly links** (fewer
   than 80 alphanumeric characters remain after removing URLs — the router
   spec's `UrlOnly` rule). An article that merely contains links is extracted
   as text unless the user opts in, so nothing is fetched by surprise.
10. **Sanitized output is a prerequisite.** Extract stored `md_to_html`
    output unsanitized (see "Security"). The fix was relayed to the Spec A
    implementation session on 2026-09-15 and landed on `main` at `9b18d51`:
    `build_ingest_fragment` now renders both markdown fields through
    `render_llm_markdown` (`md_to_html → taskify_checklists →
    sanitize_note_html`, jodd-mcp's pipeline). URL ingest's own output must go
    through the same function.
11. **The surrounding text is context, not a source.** What the user wrote
    beside the links goes to the synthesis call as the user's own statement of
    why these sources were collected.
12. **Table-cell `style` is narrowed to alignment, here.** The shared ammonia
    allowlist still passes `style` on `th`/`td` unfiltered (kept for markdown
    table alignment), so a table cell can carry arbitrary inline CSS — the
    overlay vector the CSP's `style-src 'unsafe-inline'` does not stop. The
    sanitize fix deliberately left it, because narrowing it touches
    `jodd-mcp` and the `is_replace_safe` strict list. This spec owns it,
    since URL ingest is what puts third-party text in front of the LLM.

## Approach

```
Extract modal (pasted text, or an existing note's body)
   │ analyze_ingest_sources(text, exclude_uuids)      — no network
   ▼
☑ detected URLs (pre-checked, max 8) · kinds · duplicate badges
   │ [Ingest N sources]
   ▼
ingest_sources(account, urls, context, title_override, request_id, on_progress)
   │
   ├─ 1. Fetch   ingest::web / ingest::youtube          HTTP only, no LLM
   ├─ 2. Map     provider.extract(source)               Extract SYSTEM_PROMPT, one source at a time
   ├─ 3. Reduce  provider.synthesize(digests, context)  skipped when one source succeeded
   └─ 4. Write   one note → resolve_destination (Spec A) → folder suggestion (Spec A)
```

The result note is: the synthesized body (tags, points) → a `## Sources`
list (title, link, status per source, failures included) → one
`<details>` Source block holding every fetched text.

This is an explicit user-triggered remote operation, which the local-first
doctrine permits: the modal waits with progress and a Cancel button, and the
note is still written once, synchronously, to SQLite.

## Components

### Rust — new module `src-tauri/src/ingest/`

Named as `docs/LOCAL-AI-RESEARCH.md` §7 proposed, so later modalities (audio,
image) land beside it.

| File | One job | Detail |
|---|---|---|
| `ingest/urls.rs` | Find sources in text | Split URLs glued without whitespace (a second `http://`/`https://` inside a match) before `db::extract_urls` (which already dedupes, keeps first-seen order and decodes entities). Classify `Web`, `YouTube`, or `Unsupported(reason)` — YouTube playlists (`playlist?list=`), channels (`/@name`) and search pages are unsupported. `context_text()` is the input with URLs removed. `is_mostly_urls()` applies Decision 9's threshold. |
| `ingest/web.rs` | Page → readable text | `reqwest` client: 20 s timeout, manual redirects (≤ 5 hops, each re-validated — see "Security"), 5 MB body cap, `text/html` and `text/plain` only, no cookie store. Charset from the header, else sniffed from `<meta charset>` / `http-equiv` via `encoding_rs` (Thai sites still declare `tis-620`/`windows-874`). `html5ever` parse; drop `script, style, noscript, nav, header, footer, aside, form, svg`; prefer `<article>`, then `<main>`, then `<body>`; title from `og:title`, else `<title>`; block elements become line breaks. No GitHub special case — a repo page renders its README inside `<article>`. |
| `ingest/youtube.rs` | Video → transcript | Parse the id from `watch?v=`, `youtu.be/`, `shorts/`, `embed/`, `live/`, ignoring `t`, `si`, `list`. POST `youtubei/v1/player` as the IOS client; **every client constant in one clearly labelled block** (name, version, device, OS, user agent). Prefer a manual caption track over `kind: "asr"`. Strip tags and **decode entities twice** (the IOS payload carries `&amp;#39;`). Fallback: title + `shortDescription` from the watch page → `Partial`. Endpoints are injectable (`YoutubeEndpoints`) for tests. |
| `ingest/mod.rs` | Shared types | `SourceKind { Web, YouTube }`; `FetchStatus { Ok, Partial(String), Failed(String) }`; `FetchedSource { url, kind, title: Option<String>, text: String, status }`; `trait SourceFetcher` (one real implementation, fakes in tests); `FetchPolicy` (see "Security"). Every fetch races the `CancellationToken`. |
| `ingest/stored.rs` | The multi-source Source block | `render_sources(&[FetchedSource])` writes, per source, a header line `=== Jodd source K of N ===` followed by `URL:`, `Title:`, `Status:` lines, a blank line and the text. `parse_sources(&str)` inverts it; a header counts only when its `K`/`N` are consistent with the headers around it, and text that does not parse is treated as one legacy source. |
| `ingest/run.rs` | The map-reduce orchestrator | `run_ingest(provider, fetcher, sources, context, cancel, progress)`, shaped like `ask/run.rs`. Fetch all; map each usable source through `provider.extract` with input capped by `map_input_cap`; reduce when two or more digests succeeded; return an `ExtractEnvelope` plus per-source outcomes. |

### Rust — LLM layer

| File | Change |
|---|---|
| `llm/provider.rs` | New trait method, **no default implementation**: `synthesize(&self, digests: &[SourceDigest], context: &str, cancel) -> Result<ExtractEnvelope, ExtractError>`. Returns the existing envelope, so the existing `ExtractEnvelope::JSON_SCHEMA` applies unchanged. `SourceDigest { title, display_url, status, lessons_markdown }`, where `display_url` is the URL with its query string and fragment removed — except YouTube, which becomes the canonical `https://youtu.be/<id>`, so the video stays identifiable without passing any other parameter. The two test fakes (`ask/run.rs`, `llm/autolink.rs`) implement it. |
| `llm/prompt.rs` | `SYNTHESIS_SYSTEM_PROMPT`: combine digests into cross-source points; attribute each point to its source(s); call out where sources disagree; treat the user context as the reason the sources were collected; same JSON envelope rules as `SYSTEM_PROMPT`. |
| `llm/http.rs`, `llm/agent_cli.rs` | `synthesize` via `send_json_request` (introduced by Spec A) / `run_json` with `ExtractEnvelope::JSON_SCHEMA`; responses parse through `parse_envelope_lenient` (gotcha #7b). |
| `llm/markdown.rs` | `assemble_ingest_body(envelope, outcomes, fetched)`: synthesized body through the existing private `render_llm_markdown` (so it shares Extract's sanitizing) → `## Sources` → one Source block from `ingest::stored::render_sources`. Also Decision 12's `th`/`td` style filter in `strict_note_html_builder`. |

`map_input_cap(delivery, target_os)`: **60 000** characters, or **24 000** when
the resolved agent-CLI preset uses `PromptDelivery::Argv` on Windows — the
`thclaws`, `opencode` and `aider` presets put the whole prompt on the command
line, and Windows caps it at 32 767 characters (`presets.rs`). Truncation
appends a visible `[truncated: kept X of Y characters]` marker.

### Rust — `lib.rs` commands

| Command | Behaviour |
|---|---|
| `analyze_ingest_sources(account_id, text, exclude_uuids: Vec<String>)` **new** | No network. Returns `{ sources: [{ url, kind, supported, reason, duplicate_owner }], mostly_urls, context_chars }`. Duplicate detection uses `find_citation_owner` excluding **every** uuid given — the source note and the append target — because a link-collection note always "cites" its own links. |
| `ingest_sources(account_id, urls, context_text, title_override, request_id, on_progress: Channel<IngestProgress>)` **new** | `refuse_write(Write::Notes)`. Registers its token in `in_flight_extracts`, so the existing `cancel_extraction` cancels it. Runs `ingest::run`, writes one note filed by `ExtractDestination::Resolve` (`filing::resolve_destination`), returns the landed `ExtractedNoteDto { uuid, label }`. |
| `re_extract_note` | When `ingest::stored::parse_sources` recognises a Source block (one or more sources), runs map-reduce over the stored text with **no fetcher call**, filing with `ExtractDestination::BesideSource` (`filing::destination_beside`) as the landed single-source path does; otherwise unchanged. (Amended 2026-09-15 after the whole-branch review: a 1-of-1 block can only come from ingest.) |
| `generate_handler!` | Both new commands registered; `every_literal_invoke_name_is_a_registered_command` enforces it. |

`IngestProgress` is `{ stage: Fetching | Summarizing | Synthesizing | Writing | Done, index, total, url_host }` — counts and a host, never content.

### Frontend

| File | Change |
|---|---|
| `LessonExtractModal.svelte` | Debounced, sequence-guarded `analyze_ingest_sources` on the source text (both modes). When supported URLs exist, a **"Sources from links"** section: a checklist with kind icons and duplicate badges, at most 8 checked, pre-checked only when `mostly_urls` (Decision 9). With the section enabled the primary button reads "Ingest N sources" and calls `ingest_sources` with a `Channel` feeding a per-source progress list. Cancel and closing while busy call `cancel_extraction`. In existing-note mode the source note's uuid is passed in `exclude_uuids`. |
| `types.ts` | `IngestSource`, `IngestProgress`, `ExtractedNote`. |

## Security

### HTML injection through the LLM — a prerequisite (Decision 10)

Read from code on 2026-09-15; not reproduced by a running test:

- `build_ingest_fragment` stores `md_to_html(lessons_markdown)` directly.
  `md_to_html` is pulldown-cmark's `push_html`, which passes raw HTML in the
  markdown through.
- No in-app path calls `sanitize_note_html`; `jodd-mcp` does
  (`md_to_html → taskify_checklists → sanitize_note_html`, `write.rs`).
- The editor assigns `editorEl.innerHTML` with no sanitizer.
- The CSP `script-src 'self'` (no `'unsafe-inline'`) blocks `<script>`,
  inline handlers and `javascript:` URLs, so this does **not** reach
  `invoke`. It does allow remote-image beacons (`img-src https:`), UI-spoofing
  CSS (`style-src 'unsafe-inline'`), and HTML that syncs out to Apple Notes.

URL ingest turns the LLM's input from text the user pasted into text a third
party wrote, so the in-app write path must use jodd-mcp's pipeline before
ingest ships. **Status:** fixed on `main` at `9b18d51`, with tests
`extract_body_drops_raw_html_the_llm_emits`,
`extract_body_keeps_ordinary_markdown` and
`extract_body_turns_tasklists_into_task_rows`. Behaviour change that fix introduced: GFM tasklists in Extract
output become tickable Jodd task rows.

### Residual — inline CSS in table cells (Decision 12)

`strict_note_html_builder` (`llm/markdown.rs`) allows `style` and `align` on
`th` and `td`. Filter `style` on those two tags to a single
`text-align: left|center|right` declaration (ammonia attribute filter),
dropping any other declaration. Before choosing the filter, confirm by test
the exact shape pulldown-cmark emits for an aligned table cell, so ordinary
markdown tables keep their alignment. The change affects every consumer of
the builder: update the `is_replace_safe` strict-list expectations and the
`jodd-mcp` write tests alongside it.

### SSRF

Links can come from notes other people edit (shared Apple Notes), and the
user's machine may run local services (e.g. `thclaws --serve` or llama.cpp on
`127.0.0.1`, gotcha #7b). Fetched text is written into a note that syncs to a
cloud account, so an internal fetch is an exfiltration path.

- Schemes: `http` and `https` only.
- Resolve the host first; **refuse** if any resolved address is loopback,
  private (RFC 1918), link-local (incl. `169.254.169.254`), CGNAT
  (`100.64.0.0/10`), unique-local (`fc00::/7`), link-local IPv6, multicast,
  unspecified, or an IPv4-mapped IPv6 form of any of these. Pin the checked
  address with `reqwest::ClientBuilder::resolve` so the connection cannot be
  re-resolved elsewhere (DNS rebinding).
- Redirects are followed manually, at most 5 hops, and every hop is
  re-validated.
- No cookie store, and no webview cookie jar is ever consulted.
- `FetchPolicy::allow_loopback` exists **only** so tests can reach a local
  mock server; production never sets it, and a test proves the default policy
  refuses that same server.

### Prompt injection

The LLM holds no tools; fetched text is delimited as data in the user message;
output is sanitized. The residual risk is misleading content inside the
resulting note — the exposure of pasting the page by hand.

### URLs that carry secrets

- The LLM sees each URL only as its `display_url`: **query string and
  fragment removed**, with YouTube rewritten to `https://youtu.be/<id>`. So
  signed-URL tokens are never sent to a provider.
- `applog` records host and path only.
- The note stores the full URL, which the user already had.

### Accepted risk

InnerTube is an unofficial interface. It may conflict with YouTube's terms
of service, and the client constants will go stale. This is recorded as a
risk accepted by the project owner, not hidden; the probe (Testing) is how a
breakage is diagnosed.

## Size budget

| Constant | Value | Basis |
|---|---|---|
| `MAX_URLS_PER_INGEST` | 8 | Measured link-collection notes hold 3–6+ links |
| Fetched body per URL | 5 MB | Guards against pathological pages |
| `MAP_INPUT_CHARS` | 60 000 | Measured transcripts are 41 000–67 000 characters |
| `MAP_INPUT_CHARS`, `Argv` preset on Windows | 24 000 | 32 767-character command line minus prompt and margin |
| Stored text per source | 100 000 | Truncation marker when exceeded |
| Stored Source block total | 400 000 | The vault's largest existing note is 1 037 880 characters |

**Unmeasured, verified in the live pass:** the largest note body each backend
accepts — Gmail's JSON `raw` insert, a Microsoft Graph request, and CloudKit,
where Jodd writes the body inline and sends `TextDataAsset` as `null`
(`icloud/wire.rs` `NULL_FIELDS`). A refusal must surface through
`push_blocked_reason` (gotcha #14), never silently. Likewise unmeasured: how
long a 60 000-character map call takes against each preset's 120–180 s
timeout (gotcha #7).

## Error handling

| Stage | Situation | Outcome |
|---|---|---|
| Analyze | Unsupported kind (YouTube playlist, channel, search) | Listed as unsupported with its reason; never fetched |
| Fetch, web | Timeout, non-2xx, unsupported content type (e.g. PDF), over 5 MB, empty readable text (JavaScript-only pages) | `Failed(reason)`; other sources continue |
| Fetch, any | Address refused by `FetchPolicy` | `Failed("private or local address")` |
| Fetch, YouTube | `playabilityStatus` not OK (private, age-restricted, region) | `Failed(YouTube's reason)` |
| | No caption tracks | `Partial` — title + description |
| | Empty caption body, `LOGIN_REQUIRED`, or a bot check | `Partial`, and a loud log line naming the IOS `clientVersion` as possibly stale |
| After fetch | No source has usable text | **No LLM call**; per-source reasons shown; modal input kept |
| Map | One source's call fails or times out | That source recorded as failed; the next continues |
| | Every map call fails | Fallback note holding all fetched text — fetched content is never lost |
| Reduce | Synthesis fails | Note built from the per-source digests in order, with a "synthesis failed" notice — nothing lost |
| Any | Cancel, or the modal closed while busy | In-flight request dropped / child process killed; **nothing written** |
| Write | Backend refuses the note's size | Honest `push_blocked_reason` (gotcha #14) |
| Re-extract | Stored block does not parse as multi-source | Treated as a single legacy source |

## Testing

### Rust — pure units

| Unit | Cases |
|---|---|
| `ingest::urls` | Glued URLs (`…Silfrainhttps://www.youtube.com/…`, `…EtsNhttps://www.youtube.com/shorts/…`) split into two · ids from `watch?v=`, `youtu.be/`, `shorts/`, `embed/`, `live/` with `t`, `si`, `list` present · playlist, `/@channel`, `results?search_query=` unsupported · `is_mostly_urls` at 79 / 80 / 81 · `context_text` keeps the user's text |
| SSRF policy (table-driven) | Refuses `127.0.0.1`, `::1`, `10.0.0.1`, `172.16.0.1`, `192.168.1.1`, `169.254.169.254`, `100.64.0.1`, `fc00::1`, `fe80::1`, `0.0.0.0`, and IPv4-mapped IPv6 forms · allows a public address · refuses non-HTTP schemes |
| `web::extract_readable` | Scripts/styles/nav removed · `<article>` over `<main>` over `<body>` · `og:title` over `<title>` · a TIS-620 page declaring its charset only in `<meta>` decodes to correct Thai · empty readable text → `Failed` |
| `youtube` parsing | IOS player JSON **captured from the spike's videos**, trimmed · manual track preferred over ASR · double entity decode · non-OK playability → YouTube's reason · empty caption → `Partial` |
| `ingest::stored` | render → parse round trip · legacy single source · text containing a header-shaped line is not split |
| `map_input_cap` | `Argv` + Windows → 24 000 · all other pairs → 60 000 |
| `assemble_ingest_body` | A hostile page fixture (`<img onerror>`, `<iframe>`, inline `style`) leaves no dangerous markup · `## Sources` carries statuses · exactly one Source block · truncation marker present when capped |
| `th`/`td` style filter (Decision 12) | An aligned markdown table keeps its alignment · `<td style="position:fixed;inset:0">` loses the declaration · `is_replace_safe` and `jodd-mcp` write tests updated and passing |

### Rust — HTTP (mockito)

- **The default `FetchPolicy` refuses the mockito server** (it listens on
  loopback); every other HTTP test opts in with `allow_loopback`. Without this
  test, the suite would pass with the SSRF guard deleted.
- A redirect whose second hop targets a private address is refused · a sixth
  hop fails.
- Over-size body, PDF content type, and 404 each fail with a reason.
- YouTube endpoints pointed at mockito: playable with captions, no captions,
  empty caption body, non-OK playability.

### Rust — orchestrator (`FakeFetcher` + call-counting `FakeProvider`)

| Case | Asserted |
|---|---|
| One URL | 1 map call, **0 reduce calls** |
| Three sources, the second a failing YouTube | 2 map calls, 1 reduce call; the note names the failed source |
| Nothing fetched | **0 LLM calls** |
| Every map call fails | Fallback note containing all fetched text |
| Reduce fails | Note from the digests, with the notice |
| Cancel during the second map call | **No note written**; provider calls bounded |
| Progress | `Fetching 1..N → Summarizing 1..N → Synthesizing → Writing → Done`, in order |
| Multi-source Re-extract | **Fetcher called 0 times** |

Both existing fake providers implement `synthesize`;
`every_literal_invoke_name_is_a_registered_command` passes.

### Frontend (vitest; mock `invoke` **and `Channel`** in `@tauri-apps/api/core`)

`extractModalIngest.test.ts`: a links-only analysis pre-checks the section,
an article with inline links does not · more than 8 URLs → 8 checked with a
notice · duplicate badge · "Ingest N sources" label · progress messages sent
through the mocked `Channel` update the per-source list · Cancel invokes
`cancel_extraction` with the same `request_id` · existing-note mode sends the
source uuid in `exclude_uuids`.

### Probe — `src-tauri/examples/ingest_probe.rs`

Turns the spike into a re-runnable measurement through the real
`ingest::youtube` and `ingest::web` code: given URLs on the command line, it
prints playability, caption tracks, character counts and a text sample. It
needs the network, so CI never runs it; it is the first thing to run when
YouTube changes behaviour (the `icloud_webview_probe` pattern; gotcha #3
places it in `examples/`).

### Gates — the commands CI runs

```bash
cargo test --workspace
node scripts/gen-changelog.mjs
npx vitest run
npx svelte-check --threshold error
npm run build
```

### Live pass

Before starting: confirm the binary under test by mtime and `ps`, and that only
one Jodd is running.

| Area | Verify by observation |
|---|---|
| YouTube | English video · Thai auto-caption video (**reads as normal Thai**) · a Short · an age-restricted video · a video with no captions (`Partial`) |
| Web | A Thai news site · a GitHub repository · `x.com` (fails with a reason) · a PDF link (unsupported) |
| Real link-collection note | A note with six YouTube links: **record total wall-clock time**; cancel mid-run and confirm no note is created |
| SSRF, by hand | `http://127.0.0.1:7878` and `http://192.168.1.1` are refused with a reason |
| Largest note, per backend | An ingest filling the 400 000-character Source block on Gmail, Outlook, iCloud and LocalFs; record each outcome; a refusal appears as `push_blocked_reason`; iCloud re-checked more than 20 minutes after writing |
| Providers (gotcha #7) | Time a 60 000-character map call on each preset in use (at least `claude` and one HTTP provider) · `thclaws` on **Windows** caps at 24 000 and does not fail |
| Android | `Channel` progress arrives · InnerTube returns a transcript over the phone's network |

## Scope / files

- **New:** `src-tauri/src/ingest/{mod,urls,web,youtube,stored,run}.rs`;
  `src-tauri/examples/ingest_probe.rs`; test fixtures under
  `src-tauri/src/ingest/fixtures/` (trimmed captured responses, a hostile
  page, a TIS-620 page); `src/lib/components/extractModalIngest.test.ts`.
- **Changed:** `src-tauri/src/lib.rs`, `src-tauri/src/llm/{provider,prompt,http,agent_cli,markdown}.rs`,
  `src-tauri/src/ask/run.rs` and `src-tauri/src/llm/autolink.rs` (test fakes),
  `src-tauri/Cargo.toml` (`encoding_rs` as a direct dependency — already in
  the lockfile through `reqwest`'s `charset` feature, so nothing new is
  downloaded), `src/lib/components/LessonExtractModal.svelte`,
  `src/lib/types.ts`, and `jodd-mcp/src/write.rs` tests (Decision 12's
  table-cell style filter reaches every consumer of the shared allowlist).
- **Docs after landing:** amend the router spec with a note that Decisions 1
  and 4 are superseded here; add the probe's measured results to
  `docs/ROADMAP.md`'s 0b entry; record the per-backend size and per-preset
  timing measurements where their gotchas live (#14, #7).

## Relationship to other work

- **Spec A (Extract filing).** Landed on `main` (`69be98d`). This spec reuses
  what it shipped, under the names it shipped: `llm::filing::{resolve_destination,
  destination_beside}`, `HttpProvider::send_json_request`, `ExtractedNoteDto`,
  `extract_note_into` with `ExtractDestination`, and the folder suggestion
  that runs after the write. The sanitize fix requested of that session
  landed on `main` at `9b18d51` (Decision 10).
- **Roadmap #0b (input router).** Still unbuilt. Its `UrlOnly` classification
  becomes `ingest::urls` plus `is_mostly_urls`, and its disabled
  `[🔒 Let AI fetch it]` chip becomes this feature. Its Decision 1 is reversed
  and Decision 4 superseded (Decision 1 above). Its per-account gate on LLM
  calls the user did not trigger is unaffected: every call here is
  user-triggered.
- **`docs/LOCAL-AI-RESEARCH.md`.** `ingest/` is the module that document
  proposed; this spec is its text-and-web phase.

## Deferred (not built)

- One note per URL, and a "synthesize these" action over existing notes.
- PDF sources.
- YouTube playlists and channels.
- A hidden-webview YouTube fallback (only if the mobile clients stop working).
- Local transcription or OCR of media.
- Writing ingested links back into the source link-collection note.
