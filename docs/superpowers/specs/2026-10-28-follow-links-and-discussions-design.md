# Follow links and read discussions — open content behind a link

> Status: **P1–P4 built 2026-10-28** (owner: "start building phase 1",
> "phase 2", "phase 3", "phase 4"); live pass owed for all four — §15 (P1),
> §16 (P2), §17 (P3) and §18 (P4) say what was built and where it departs
> from the draft below. P4 has never run: no webview opens in the sandbox
> that built it. **First live pass 2026-10-29 (macOS) — §19**: what passed,
> what failed, the measurements, and what is still owed.
> Builds on [2026-09-15-url-ingest-design.md](2026-09-15-url-ingest-design.md)
> (the fetch → map → reduce pipeline, `ingest/`, the SSRF guard) and
> [2026-10-18-social-video-ingest-design.md](2026-10-18-social-video-ingest-design.md)
> (the four platform fetchers, D8's honest `Partial`, the anti-goal on the
> user's own platform session). Roadmap #18.
>
> **Nothing in this document was measured.** The session that wrote it could
> not reach any of the hosts it names (the sandbox's network policy refused
> every one). Platform facts below are from the vendors' own documentation
> where that is cited, and from **secondary reporting** everywhere else, and
> say which. §9 lists what the builder measures before relying on it — the
> same rule the social-video spec followed, for the same reason.

## 1. Intent

The owner shared a Facebook Reel whose real content is in its comments, and
asked three things in one conversation:

1. **Can Jodd read the comments?** (For Facebook, TikTok and Instagram too.)
2. **Can Jodd follow a page's outbound links like a spider** — "smart
   somewhat, not follow any and every thing which somehow not about real value
   content but junk"?
3. **Is there a generic mechanism to open content**, rather than one hand-made
   reader per site?

Today URL ingest reads exactly the links the user gave it, at most
`MAX_URLS_PER_INGEST` (8). `web::extract_readable` keeps an `<article>`/`<main>`
and drops `nav`/`header`/`footer`/`aside`/`form` — and with them every `href`:
`collect_text` returns text only. Nothing follows a link, and no fetcher reads
a comment.

This spec adds three layers that share one pick list:

| Layer | What it does | Reaches |
|---|---|---|
| **A. Follow links** | Find the links *inside the content*, throw out junk by rule, rank the rest, let the user tick them, fetch them like any other source. | every web page, YouTube descriptions, X post text, comments read by B |
| **B. Read discussions** | A post's comments as part of the source, the author's own first. | platforms with an open comment interface (§3) + any page that publishes its comments in a standard shape |
| **C. Open content generically** | One ladder of standard, site-agnostic ways to get at what a link holds, before any site-specific code and after it fails. | the long tail |

### Success

- A blog post that cites three studies and a GitHub repo → one tap shows those
  four links pre-ticked, and not its tag cloud, share buttons, "related posts"
  or newsletter box. The resulting note covers all five sources and says which
  came from which.
- A YouTube how-to whose creator put "full code here →" in a pinned comment →
  that link is offered, pre-ticked, marked "from the author's comment".
- A Facebook / TikTok / Instagram teaser that says "link in the comments" →
  Jodd says plainly that it cannot read that platform's comments, and offers a
  box to paste what the user found there. The pasted link is then followed
  like any other.

## 2. Goals, non-goals, anti-goals

**Goals**

- Link discovery, junk rules, ranking and a pick list in the Extract modal's
  existing "Sources from links" section (§5).
- Discussions for **YouTube, Bluesky, Mastodon and Hacker News**, plus any
  page that ships its comments as schema.org JSON-LD or a comment feed (§3, §6).
- A **paste box per link** for platforms whose comments need a login.
- A quality gate after fetching: a followed page that turns out thin, walled
  or a duplicate is reported and left out of the map step, not silently
  summarised.
- Every source in the note says how it got there: given by the user,
  followed from source *K*, or pasted by the user.

**Non-goals**

- **Comments on Facebook, Instagram, TikTok, X and Reddit** — all need a login
  or a paid/approved API (§3). The paste box is the answer for these.
- Crawling a whole site, a sitemap, a feed of new posts, or anything on a
  schedule. Depth is bounded and every hop is a user action (D3).
- Following links in comments written by anyone but the post's author (D6).
- PDF, images, file downloads (roadmap #16 stage 3 owns PDF).

**Anti-goals — a failure even if it technically works**

- **Any use of the user's own platform session.** Unchanged from the
  social-video spec: no signed-in webview, no imported cookies, no stored
  platform credential. Layer C's rendered reader (§7.3) uses an *empty,
  isolated, throw-away* webview store — never the iCloud sign-in webview's
  (gotcha #19, #31).
- **A paid scraping service wired in by default.** Apify-style services do read
  Facebook/TikTok/Instagram comments, from their own accounts, against those
  platforms' terms. That is the owner's call, not this design's (§11).
- **A note that looks like it covered what it did not.** A followed page that
  was walled, cut, or dropped by the quality gate is listed with its status,
  exactly as D8 lists a cut Facebook caption.
- **Third parties' names in the user's notes by default** (D7).
- **Following a link the user cannot see before it is fetched**, in the
  default mode (D2).

## 3. What each platform allows (2026-10, unmeasured — see banner)

| Platform | Post | Comments, logged out | Source of the claim | This spec |
|---|---|---|---|---|
| **YouTube** | captions + description (built) | **Yes.** Data API v3 `commentThreads.list` reads public comments with an **API key only** (no OAuth), 1 quota unit per page of ≤100, 10 000 units/day free; `commentsDisabled` → 403. The InnerTube `next` continuation also carries them, unofficially, without a key. | third-party guides ([Phyllo](https://www.getphyllo.com/post/is-the-youtube-api-free-in-2026-quota-limits-costs-when-to-pay), [OutlierKit](https://outlierkit.com/resources/youtube-comments-api/)); Google's docs not re-read | **Build** (§6.2) |
| **Bluesky** | yes | **Yes.** `app.bsky.feed.getPostThread` on `https://public.api.bsky.app`, no auth. | official [Bluesky docs](https://docs.bsky.app/docs/api/app-bsky-feed-get-post-thread) | **Build** |
| **Mastodon / fediverse** | yes | **Usually.** `GET /api/v1/statuses/:id/context` on the post's own server; an instance can require auth for its API. | Mastodon API (from memory) | **Build** |
| **Hacker News** | yes | **Yes.** Algolia `GET /api/v1/items/:id` returns the whole tree. | from memory | **Build** |
| **Generic pages** | yes | **Sometimes:** schema.org JSON-LD `comment` / `DiscussionForumPosting` / `QAPage`; WordPress per-post comment feeds (`wfw:commentRss`). | standards | **Build** (§7.1) |
| **Reddit** | — | **No (closed ~2026-05-28).** Logged-out `.json` answers 403 whatever the UA; an approved OAuth app is the only route; registration deadlines reported (dates conflict). | secondary only ([DEV](https://dev.to/tonywangca/why-reddit-blocked-unauthenticated-json-in-2026-and-how-to-still-get-reddit-data-58b9), [bigideasdb](https://bigideasdb.com/reddit-json-403-forbidden)) | **Out.** Re-measure; if `.json` answers, revisit |
| **Facebook** | og: tags (built; posts cut "…") | **No.** Graph API reads comments only on Pages you manage; other Pages need *Page Public Content Access* — App Review + business verification, granted case by case. | [Meta docs](https://developers.facebook.com/docs/features-reference/page-public-content-access) | **Out** → paste box |
| **Instagram** | og: tags + embed (built) | **No, reliably.** A logged-out view shows 0–2 comments or a sign-in prompt, varying by load. Graph API: own business media only. | secondary ([peekstories](https://peekstories.com/blog/see-instagram-comments-without-account-2026)) | **Out** → paste box |
| **TikTok** | rehydration JSON (built) | **No.** `/api/comment/list/` needs a signed request *and* a browser-minted `msToken`; logged out it returns an empty HTTP 200. Research API: academics only. | secondary ([tiktok-cli](https://pkg.go.dev/github.com/tamnd/tiktok-cli), [Apify](https://apify.com/outspoken_strategy/tiktok-comments-scraper)) | **Out** → paste box |
| **X** | syndication JSON (built) | **No.** Replies need the paid API. | known | **Out** → paste box |
| **Threads** | — | No comments; post via oEmbed (below). | Meta | oEmbed only |

**One Meta change helps the *post*, not the comments.** Since
**2026-06-15** Meta's oEmbed endpoints — Facebook post and video, Instagram,
Threads — answer **without an access token** for public content, capped at
1 000 requests per endpoint per hour
([Meta blog](https://developers.facebook.com/blog/post/2026/06/15/tokenless-access-to-meta-oembed-apis/)).
An oEmbed answer's `html` is the embed markup, which for a Facebook post may
carry the post text **uncut** — the thing D8 of the social-video spec had to
label "possibly cut off". Measure first (§9); if it holds, `facebook.rs` gains
oEmbed as a second route for posts. It does not carry comments.

## 4. Decisions

| # | Decision |
|---|---|
| D1 | **Three layers, one pick list.** Links found on a page, links in an author's comment, and links pasted by the user all become *candidates* in the same list, with the same rules and the same caps. |
| D2 | **Default is "show, then fetch".** Discovery fetches only the page the user already chose; followed pages are fetched only after the user ticks them. The best candidates arrive pre-ticked, so the common case is one tap. An **auto-follow** setting per account (off by default) skips the list: depth 1, top `AUTO_FOLLOW_MAX` (3) above `AUTO_FOLLOW_MIN_SCORE`. |
| D3 | **Depth is a user action.** Default depth 1. A followed source can itself be opened ("Look inside") for depth 2; there is no depth 3. Caps: `MAX_FOLLOWED_PER_SOURCE` 5, `MAX_SOURCES_TOTAL` 16 (the existing 8 user-given plus followed). |
| D4 | **Rules first, AI optional.** Junk is removed by deterministic rules (§5.2) and the rest ranked by a deterministic score (§5.3). An optional **AI pick** (§5.4) sees only the surviving candidates' anchor text, the sentence around each and its `display_url` — never a fetched page. |
| D5 | **A followed page has to earn its place after fetching** (§5.5): thin, walled, link-farm, or a near-duplicate of an earlier source → `FetchStatus::Failed("not worth reading: <why>")`, listed in Sources, not mapped. |
| D6 | **Comments: the author first, then the best few.** Order: the post author's own comments and replies (`is_op`), pinned, then the top by likes. Spam and empty comments are dropped (§6.3). **Only the author's comments contribute link candidates.** Anyone can post a link under someone else's video. |
| D7 | **No third-party names in the note by default.** Comments are rendered as `Author:` (the post's author, already in the note) or `Commenter:` with the like count, not the commenter's handle. A setting can turn handles on. Comment text is still third-party content — it goes through the existing AI-outbound privacy layer like any fetched text. |
| D8 | **Paste box for the closed platforms.** Each Facebook / Instagram / TikTok / X / Reddit link gets "Add what's in the comments". The pasted text becomes part of that link's source as `Pasted by you:`. Links in it become candidates (D1), pre-ticked. |
| D9 | **Generic before specific, specific before rendered.** For a link with no platform reader, Layer C tries standard structured data, then readable HTML, then — only if the user asks — the rendered reader (§7). |
| D10 | **The SSRF guard applies to every request this spec adds** — followed pages, comment APIs, oEmbed, comment feeds. Only the rendered reader cannot be guarded the same way (§8). |
| D11 | **robots.txt is honoured for followed links**, not for links the user gave. A link the user pasted is a browser visit; a link Jodd chose is a crawl. User agent token `Jodd`. |

## 5. Layer A — follow links

### 5.1 Finding links

`web::extract_readable` becomes `extract_readable_with_links` (the old name
stays as a wrapper so its callers do not change). It walks the **same root**
(`article` → `main` → `body`, minus `DROPPED`) and, for each `<a href>`,
records:

```rust
pub struct LinkCandidate {
    pub url: String,          // absolute, resolved against the page's <base>/URL
    pub anchor: String,       // collapsed text, or img alt / aria-label
    pub context: String,      // the sentence around the link, ≤ 300 chars
    pub region: Region,       // Body | AuthorComment | Pasted | Description
    pub block_link_density: f32, // link chars / text chars of the enclosing block
    pub position: f32,        // 0.0 = top of content, 1.0 = bottom
    pub from_source: usize,   // index of the source it was found in
    pub score: i32,           // §5.3
    pub reasons: Vec<&'static str>, // shown in the UI: "cited in text", "docs site"
}
```

Other places that already hold link text feed the same list: YouTube's
`description`, X's post text (after the `t.co` media strip `x.rs` already
does), TikTok/Instagram/Facebook captions, author comments (§6), pasted text
(D8). Their `region` says so.

Inside the root, a subtree is skipped when its `class`/`id`/`role`/`aria-label`
matches the **boilerplate pattern**: `related`, `recommend`, `share`, `social`,
`comment-form`, `sidebar`, `newsletter`, `subscribe`, `promo`, `sponsor`, `ad-`,
`advert`, `breadcrumb`, `tag-list`/`tags`, `author-bio`, `pagination`,
`cookie`, `consent`, `menu`, `toc` — and the Thai words sites actually use for
the same boxes (`บทความที่เกี่ยวข้อง`, `แชร์`, `ข่าวที่เกี่ยวข้อง`). A block whose
`block_link_density` > 0.6 is a link list, not prose, and is skipped the same
way. These patterns live in one constants block in `ingest/links.rs`, like
the platform constants (social-video spec §5.2).

### 5.2 Junk rules (deterministic, a candidate is dropped)

| Rule | Examples |
|---|---|
| Not `http(s)`, or same page | `mailto:`, `tel:`, `javascript:`, `#section`, the page's own canonical URL |
| Account / commerce | `/login`, `/signin`, `/signup`, `/register`, `/account`, `/cart`, `/checkout`, `/subscribe`, `/donate` |
| Taxonomy / listing | `/tag/`, `/tags/`, `/category/`, `/categories/`, `/topics/`, `/archive/`, `/author/`, `/page/<n>`, `?page=`, `/search` |
| Legal / boilerplate | `/privacy`, `/terms`, `/cookies`, `/legal`, `/about`, `/contact`, `/careers` |
| Share intents | `facebook.com/sharer`, `twitter.com/intent`, `x.com/intent`, `linkedin.com/shareArticle`, `line.me/R/msg`, `api.whatsapp.com/send`, `pinterest.com/pin/create` |
| Ads / tracking / affiliate | ad and click-tracking hosts (`doubleclick.net`, `googleadservices.com`, `adclick`, `/aff/`, `?ref=` on known affiliate hosts, `amzn.to`, `shope.ee`, `s.lazada`) |
| Not a document | by extension: images, audio, archives, installers, `.ics`, `.vcf`; PDF is dropped *for now* (roadmap #16) |
| Profile, not content | a social link that `urls::classify` says is a profile or home page, not a post |
| Already a source | its canonical form (`urls::canonical_unique`) matches a source in this ingest or an existing note (`ingest_duplicate_owner`) — the latter is shown as "already in note *X*", not dropped |

Shorteners (`bit.ly`, `t.co`, `lnkd.in`, `buff.ly`) are **resolved**, not
dropped: one guarded `HEAD`/`GET` with redirects off, re-classified on the
target. That costs a request per short link, so it happens only for the top
`MAX_FOLLOWED_PER_SOURCE × 2` after ranking.

### 5.3 Ranking (deterministic)

Each surviving candidate starts at 0:

| Signal | Points |
|---|---|
| `region = AuthorComment` or `Pasted` | +40 |
| link is in a sentence (context has a verb-ish length ≥ 40 chars) | +15 |
| context says *why*: `source`, `study`, `paper`, `according to`, `docs`, `full video`, `code`, `repo`, `download`, `ที่มา`, `อ้างอิง`, `ดูเพิ่มเติม`, `ลิงก์`, `คลิป`, `ฉบับเต็ม` | +20 |
| descriptive anchor (≥ 3 words, not a bare URL) | +10 |
| generic anchor (`click here`, `here`, `link`, `read more`, `คลิก`, `ที่นี่`) | −10 |
| host is a different site from the page | +10 |
| host class: docs / paper / code / standards / government (`docs.`, `arxiv.org`, `doi.org`, `github.com`, `gitlab.com`, `*.gov`, `*.go.th`, `*.ac.th`, `*.edu`, `w3.org`, `ietf.org`) | +15 |
| a supported video/post link (`urls::classify` ≠ `Web`) | +10 |
| `position` in the top half of the content | +5 |
| same host, deep path (≥ 2 segments), not caught by §5.2 | 0 |
| same host, shallow path (`/`, `/x`) | −15 |

Pre-ticked: the top `MAX_FOLLOWED_PER_SOURCE` with score ≥ `PRETICK_MIN_SCORE`
(25). Shown: up to 20, the rest behind "Show N more". The numbers are a
starting point and are tuned against the fixture set (§10) before shipping,
not argued about in review.

### 5.4 AI pick (optional, per ingest)

A checkbox under the list: "Let AI choose". It sends one request to the
account's provider:

- input: the source's title, the user's context text (already sent today), and
  for each candidate `anchor`, `context`, `display_url` (query and fragment
  removed — the URL-ingest rule);
- output: JSON `{"pick": [indices], "why": {index: "…"}}`, parsed with
  `parse_envelope_lenient` (gotcha #7b);
- it can only *re-tick* candidates already in the list. It cannot add a URL.

It goes through `llm::receipts` and the AI-outbound privacy layer like every
other call, and it counts toward the ingest's attempt budget.

### 5.5 Quality gate (after fetching a followed page)

A followed source becomes `Failed("not worth reading: …")` when:

| Check | Threshold |
|---|---|
| thin | readable text < 400 chars |
| link farm | link chars / text chars > 0.5 over the whole root |
| wall | the readable text is dominated by a paywall / consent / sign-in phrase list (EN + TH), *and* is under 1 500 chars |
| duplicate | its canonical URL (`og:url` / `<link rel=canonical>`) equals an earlier source's, or 5-word-shingle Jaccard ≥ 0.8 with an earlier source |
| blocked | robots.txt disallows it (D11) |

Sources the user gave are **not** gated — today's behaviour stays.

## 6. Layer B — discussions

### 6.1 Shape

```rust
pub struct Discussion {
    pub platform: SourceKind,
    pub total: Option<u64>,      // as the platform reports it
    pub read: usize,             // how many Jodd read
    pub comments: Vec<Comment>,  // already selected and ordered (§6.3)
    pub status: FetchStatus,     // Ok / Partial("first 200 of 3 400") / Failed
}

pub struct Comment {
    pub is_op: bool,
    pub pinned: bool,
    pub likes: Option<u64>,
    pub depth: u8,               // 0 = top level
    pub author_handle: Option<String>, // dropped at render time unless D7's setting
    pub text: String,
}

#[async_trait::async_trait]
pub trait DiscussionReader: Send + Sync {
    /// False: another reader (or none) handles the link.
    fn handles(&self, kind: &urls::UrlKind) -> bool;
    async fn read(&self, url: &str, op: Option<&str>, policy: FetchPolicy, cancel: &CancellationToken) -> Discussion;
}
```

The fetched source's `text` gains a delimited section after the post:

```
--- Comments (18 of 1 240 read; the author's first) ---
Author (pinned): Full code is here https://github.com/…
Commenter (312 likes): The part at 4:10 is wrong, use …
```

`stored.rs` needs no format change: this is text inside the source. Re-extract
from stored text keeps working.

### 6.2 Readers

| Reader | Request | Notes |
|---|---|---|
| `youtube` | InnerTube `next` → comment continuation, using the client constants already in `youtube.rs`. If the owner supplies a YouTube Data API key in Settings, `commentThreads.list?videoId=&order=relevance&maxResults=100` instead. | The InnerTube route carries the same accepted ToS risk as captions (URL-ingest spec "Accepted risk"); the key route is official. Pinned and creator-reply flags come from both. |
| `bluesky` | resolve handle → DID (`com.atproto.identity.resolveHandle`), then `app.bsky.feed.getPostThread?uri=at://<did>/app.bsky.feed.post/<rkey>&depth=6` on `public.api.bsky.app` | `urls::classify` learns `bsky.app/profile/<handle>/post/<rkey>`. |
| `mastodon` | `GET https://<host>/api/v1/statuses/<id>/context` | Recognised by **path shape** (`/@<user>/<id>` with a numeric id) on any host, then confirmed by the JSON shape. An instance that requires auth answers 401 → `Partial("this server does not show replies without signing in")`. |
| `hn` | `GET https://hn.algolia.com/api/v1/items/<id>` | `news.ycombinator.com/item?id=`. The linked story URL becomes a pre-ticked candidate. |
| `generic` | §7.1 | Any page. |

Caps: `MAX_COMMENTS_READ` 200 (≤ 2 pages per API), `MAX_COMMENTS_KEPT` 30,
`MAX_COMMENT_CHARS` 12 000 per source.

### 6.3 Selecting comments

1. All `is_op` comments and replies, and pinned ones, in thread order.
2. Then top-level comments by likes, each with its single best reply.
3. Drop: < 15 chars after removing emoji and mentions; emoji-only; exact
   duplicates; text that is mostly a link from a non-author; the platform's
   own spam/hidden flags.
4. Stop at `MAX_COMMENTS_KEPT` or `MAX_COMMENT_CHARS`.

### 6.4 The closed platforms

For a Facebook / Instagram / TikTok / X / Reddit link, the source row shows:

> Facebook doesn't show comments without signing in. Paste anything useful
> from them here (a link or text):  [ text box ]

The text joins that source as `--- Pasted by you ---`, and its links become
candidates (D8). The status line for that source says the comments were not
read, so a note made without a paste does not imply it covered them.

## 7. Layer C — opening content generically

For any link, the fetcher walks this ladder and stops at the first rung that
returns usable text. Platform fetchers stay where they are; the ladder is what
`web::fetch_web` grows into, and what a platform fetcher falls back to when it
returns `Failed`.

### 7.1 Structured data (no rendering, no site code)

Read from the page Jodd already fetched:

- **JSON-LD** `<script type="application/ld+json">`: `Article`/`NewsArticle`/
  `BlogPosting` (`articleBody`, `author`, `datePublished`),
  `SocialMediaPosting`/`DiscussionForumPosting` (`text`, `comment[]`),
  `QAPage` (`mainEntity.acceptedAnswer`, `suggestedAnswer[]`), `VideoObject`
  (`description`, `transcript` when present). `comment[]` becomes a
  `Discussion` with no `is_op` flags.
- **oEmbed discovery**: `<link rel="alternate" type="application/json+oembed">`
  → one guarded GET → `title`, `author_name`, and the text inside `html`.
  Plus Meta's tokenless endpoints for Facebook / Instagram / Threads (§3).
- **Comment feeds**: `<link rel="replies">` or `wfw:commentRss` (WordPress
  and most blog engines) → one guarded GET of an RSS/Atom feed → comments.
- **Discourse forums**: a page whose `<meta name="generator">` says Discourse
  → `<topic-url>.json` (public topics answer without auth).

`extract_readable` currently drops `<script>` wholesale; JSON-LD is parsed
from the raw HTML first, so dropping scripts afterwards is unchanged.

### 7.2 Readable HTML

Today's `extract_readable`, plus §5.1's link capture. Unchanged for the user.

### 7.3 Rendered reader (opt-in, desktop first)

Some pages ship an empty `<body>` and build everything in JavaScript. For
those, a **"Open in a private reader"** action on a source whose status is
"no readable text":

- a hidden Tauri webview with an **ephemeral, isolated data store** (a fresh
  data directory per ingest, deleted after; never the iCloud webview's store,
  gotcha #19);
- **no IPC capability** matches its window label or its remote origin — the
  page can call nothing in Jodd;
- load the URL, wait for `load` + network idle (≤ 15 s), run one injected
  script that returns `document.documentElement.outerHTML`, close and delete
  the store; Jodd then runs §7.1–7.2 on that HTML;
- no cookie consent is clicked, no login is attempted, no scrolling for
  "load more" comments.

**Android is out of this phase.** Gotcha #31 lists three Android webview APIs
that compile and do nothing (`cookies()`, `set_visible`,
`clear_all_browsing_data`) and a fourth (`close()`) still open — an invisible,
deletable, isolated webview is exactly what those break. Android shows the
action disabled with "desktop only for now".

## 8. Security and privacy

- **SSRF.** Every new request goes through `net::send_guarded` (scheme check,
  resolve-then-pin, private ranges refused, ≤ 5 re-validated redirects, no
  cookie store). That includes shortener resolution, robots.txt, oEmbed,
  comment APIs and comment feeds. A link found on a page is *more* hostile
  than one the user typed — the guard is why following it is safe.
- **The rendered reader cannot be pinned.** A webview resolves DNS itself and
  will load private-network subresources. It is therefore never automatic,
  never used for a followed link without the user pressing the button, and
  the top-level URL must pass `check_resolved` first. This is the same
  exposure as the user opening the link in their own browser; the note
  records that the page was rendered.
- **No IPC from remote content.** A test asserts that no capability file
  matches the reader window's label or `remote.urls` (§10).
- **Prompt injection.** Comments are the most attacker-controlled text Jodd has
  ever fed to a model. They stay inside the delimited source block, the model
  holds no tools, and output is sanitised — unchanged. New: an author comment
  is labelled `Author` only when the platform says so (`is_op`), never from
  the text.
- **Third-party data.** D7: handles out by default. Comment text goes through
  `privacy/` masking before it reaches a provider, like any fetched text.
- **Secrets in URLs.** Candidates are shown and sent to the AI pick only as
  `display_url`. `applog` gets host and path only (existing rule).
- **The YouTube Data API key** (if given) is stored the way the STT key is
  (`stt_config`'s pattern: config file + keychain), and is not read by any
  command the frontend polls (gotcha #15).

## 9. Not measured — the builder measures before relying on it

Add `--links <url>` and `--comments <url>` to `examples/ingest_probe.rs`
first, run them from a residential connection, and record results in this
section before writing the reader that depends on each:

1. Bluesky `getPostThread` logged out — shape, depth, rate limit.
2. Mastodon `context` on mastodon.social and two smaller instances.
3. HN Algolia `items/<id>` — size of a 500-comment thread.
4. YouTube InnerTube `next` comment continuation with the current client
   constants; and `commentThreads.list` with a key (pinned / creator flags).
5. Meta tokenless oEmbed for the owner's Facebook **post** links (social-video
   spec Appendix A) — is the post text uncut in `html`?
6. Instagram `/embed/captioned/` — does it carry any comment text at all?
7. Reddit `.json` logged out — confirm the 403 from the secondary reports.
8. JSON-LD `comment[]` frequency across the owner's own saved web links
   (`ingest_duplicate_owner`'s URL set is a ready sample).
9. The junk rules and score (§5.2–5.3) against ≥ 20 real pages: Thai news,
   Medium, Substack, a docs site, a GitHub README, a YouTube description.
   Precision of pre-ticked candidates is the number to report.

## 10. Testing

- **Pure units** (`ingest/links.rs`): each §5.2 rule with a positive and a
  negative case; boilerplate-pattern skip; link density; relative-URL
  resolution against `<base>`; scoring order on a fixed fixture page.
- **Fixtures**: a trimmed real page per class in §9.9, plus one JSON-LD
  `DiscussionForumPosting`, one WordPress comment feed, and one recorded
  response per reader — trimmed like the existing `fixtures/` (README there).
- **HTTP (mockito)**: each reader, including the 401/403/empty-body paths;
  shortener resolution through the guard; robots.txt allow/deny.
- **Orchestrator**: followed sources count toward `MAX_SOURCES_TOTAL` and the
  attempt budget; a gated source appears in `## Sources` with its reason and
  is not mapped; a user-given source is never gated.
- **Security**: a candidate resolving to `127.0.0.1` / `10.0.0.0/8` is refused
  by the default policy; the reader window matches no capability; the
  rendered reader's data directory is gone after the ingest.
- **Frontend (vitest)**: the candidate list under its source, pre-ticks,
  "Show N more", the paste box on closed platforms, D7's handle setting.
- **Gates**: the commands in CLAUDE.md "Verify", unchanged.

## 11. Calls the designer made (the owner may overrule)

- **Show-then-fetch by default, auto-follow opt-in per account** (D2).
- **Depth 1, user-driven depth 2, never 3** (D3).
- **Commenter handles off by default** (D7).
- **robots.txt honoured for followed links only** (D11).
- **No paid scraper.** If the owner wants Facebook/TikTok/Instagram comments
  badly enough to pay per run and accept those platforms' terms risk, the
  shape is a `DiscussionReader` that calls a user-configured service with the
  user's own key, off by default, disclosed in the privacy page. Not designed
  further here.
- **YouTube via InnerTube first**, Data API key as an optional upgrade — keeps
  the zero-setup path; the key is the escape hatch when InnerTube breaks.

## 12. Phases

| Phase | Ships | Depends on |
|---|---|---|
| **P1** | Layer A without AI: link capture, junk rules, ranking, pick list, quality gate, robots.txt; candidates from web pages, YouTube descriptions, X text, captions. §7.1 JSON-LD `articleBody` as a better main-text source. | §9.9 |
| **P2** | Layer B: `DiscussionReader` + YouTube, Bluesky, Mastodon, HN, JSON-LD `comment[]`, comment feeds, Discourse; author-comment links → candidates; paste box for closed platforms. Meta tokenless oEmbed for Facebook posts if §9.5 holds. | §9.1–9.8 |
| **P3** | AI pick; per-account auto-follow. | P1 in use |
| **P4** | Rendered reader, desktop only. | its own security review |

## 13. Files

| File | Change |
|---|---|
| `ingest/links.rs` (new) | `LinkCandidate`, capture, constants block, junk rules, score |
| `ingest/quality.rs` (new) | §5.5 gate |
| `ingest/robots.rs` (new) | fetch + cache per host per ingest, longest-match rules |
| `ingest/discuss/{mod,youtube,bluesky,mastodon,hn,generic}.rs` (new) | §6 |
| `ingest/structured.rs` (new) | JSON-LD, oEmbed discovery, comment feeds, Discourse |
| `ingest/web.rs` | `extract_readable_with_links`; ladder §7.1 → 7.2 |
| `ingest/urls.rs` | classify Bluesky, Mastodon-shaped, HN; profile-vs-post |
| `ingest/youtube.rs`, `x.rs`, `facebook.rs` | expose description / text links; Facebook oEmbed route (P2) |
| `ingest/run.rs` | followed-source bookkeeping, `MAX_SOURCES_TOTAL`, provenance per source |
| `lib.rs` | `discover_link_candidates(account_id, url, request_id)` — network, user-triggered, cancellable through `cancel_extraction` |
| `LessonExtractModal.svelte`, `ingestSources.ts` | candidates nested under their source, paste box, AI-pick checkbox |
| `examples/ingest_probe.rs` | `--links`, `--comments` |
| `docs/ROADMAP.md`, `CLAUDE.md` | roadmap #18 |

## 14. Relationship to other work

- **Roadmap 0b (Extract input router)** — its fetch half was superseded by URL
  ingest; this spec does not touch its classifier or its per-account gate on
  untriggered LLM calls. D2's auto-follow *is* an untriggered fetch and must
  respect that gate when it is built.
- **Roadmap #16 stage 2b (Research Studio, the web)** — will want §5's junk
  rules and §5.5's gate for search results. Build them as a library, not
  inside the modal's command.
- **Roadmap #7 (auto-link / OKF)** — a followed source is a `cites` edge from
  the new note; nothing new in `edges`.

## 15. Phase 1 as built (2026-10-28)

Built: Layer A without AI. "Find links" on any supported source in the
Extract modal → `discover_link_candidates` → a ranked pick list under the
source, best pre-ticked; a ticked candidate can itself be opened for depth 2.
Followed links travel to `ingest_sources` as `followed: [{url, from_url}]`,
are fetched after the user's links, pass robots.txt and the quality gate, and
say "followed from …" in the note's Sources list. JSON-LD `articleBody` is
read as a better main text (§7.1, first rung only).

| Where | What |
|---|---|
| `ingest/web.rs` | `extract_readable_with_links`, `fetch_page_links`, `text_links`, JSON-LD `articleBody` |
| `ingest/links.rs` | rules, score, `rank`, `resolve_short`, `discover`, `plan_followed` (caps, depth ≤ 2) |
| `ingest/robots.rs` | RFC 9309 parse/match, per-ingest cache in `HttpFetcher::robots` |
| `ingest/quality.rs` | §5.5 gate, `NOT_WORTH_READING` |
| `ingest/run.rs` | `IngestRequest::followed_from`; robots + gate for followed links only; provenance in Sources |
| `ingest/x.rs`, `social.rs` | `PlatformMeta::expanded_links` from X's `entities.urls` |
| `lib.rs` | `discover_link_candidates`; `ingest_sources(followed)` |
| `LessonExtractModal.svelte`, `ingestSources.ts` | the pick list; `followedToSend` sends only links whose chain reaches a ticked source |

**Departures from the draft, and why:**

- **Link density does not drop a block (§5.1).** A list made only of links is
  also how an article ends with its *References*, which are the best links on
  the page. Density is a −10 score (only when the container holds ≥ 3 links);
  furniture is removed by `class`/`id`/`role` words and by a box whose first
  heading reads "Related" (EN/TH) instead. The content root itself is exempt —
  WordPress puts `tag-*` and `category-*` classes on `<article>`.
- **Class words match whole words** (`read-more` is not `ad`), split on `-`/`_`.
- **The "link farm" check reads text, not link characters (§5.5):** a fetched
  `FetchedSource` carries text only. ≥ 20 lines, ≥ 85 % of them under 50
  characters → "a list of links, not an article".
- **Duplicates are by canonical URL and by 12-character shingles**, not by the
  page's `<link rel=canonical>` (not carried either). The shingle check
  catches the mirror case the canonical tag would have.
- **A gated source keeps no text** — its Source block entry is empty, so a
  wall or a list is never stored in a synced note.
- **An unreadable robots.txt allows everything**, 5xx included (RFC 9309 says
  disallow on 5xx); see `robots.rs`'s module comment.
- **X short links use X's own expansion** (`entities.urls`) before any
  request. Unmeasured: the fixture set has no post with `entities`; a post
  without them falls back to resolving `t.co` like any short link.
- **Shown: 8, then "Show N more"** up to the 20 returned (§5.3 said 20).
- **Provenance is in the Sources list only.** The stored Source block has no
  "followed from" line, so a Re-extract from stored text lists the sources
  without it.

**Live pass owed** (nothing above was run against a real site from here —
the build session's network reached none): §9 item 9 (the rules and score on
≥ 20 real pages, Thai news first), a robots.txt-disallowed followed link, a
short link, an X post with a link, and a YouTube description with links.

## 16. Phase 2 as built (2026-10-28)

Built: Layer B and the paste box. **Nothing here met a live server** — every
reader was written from the platform's documentation and tested against
mocks; the first live pass is what proves the shapes (§9 items 1–8).

| Where | What |
|---|---|
| `ingest/discuss/mod.rs` | `Discussion`/`Comment`, `select` (author and pinned first, then top-level by likes with each one's best reply; junk, link drops and duplicates out; 30 comments / 12 000 chars), `render` (`Author` / `Commenter`, never a handle), `append_to`, `append_pasted`, `without_discussion`, `Thread`, `fetch_thread` |
| `discuss/bluesky.rs` | public AppView: `resolveHandle` → `getPostThread` (depth 6); link facets restore full URLs; `external` embed = shared link |
| `discuss/mastodon.rs` | `/api/v1/statuses/:id` + `/context` on the post's own server; mentions and hashtags are not links; a 401/403 on `/context` is a `Partial`; a non-Mastodon answer falls back to reading the page |
| `discuss/hn.rs` | Algolia `items/:id` — the whole tree in one request; the story URL is the shared link |
| `discuss/generic.rs` | Discourse `/t/<id>.json` (first post becomes the page's text), JSON-LD `comment[]` and `Question` answers (accepted = pinned; a comment the page already renders is skipped), the post's own RSS/Atom comment feed (`quick-xml`, already in the lockfile via `plist`) |
| `discuss/youtube.rs` + `youtube_key.rs` | Data API v3 `commentThreads` (relevance order, ≤ 2 pages) with an app-level key; keychain + a status flag file, read only when a YouTube link is in the run (gotcha #15) |
| `ingest/urls.rs` | `UrlKind::{Bluesky, Mastodon, HackerNews}`, `fetch_kind`, `kind_name`, `comments_closed` |
| `ingest/links.rs` | `Region::{Shared, AuthorComment, Pasted}` (+30/+40/+40); `Readers`; thread and author-comment links in `discover`; `plan_pasted`, `pasted_candidates` |
| `ingest/run.rs` | pasted text joins its source (or is the whole source, `Partial`, when the page failed); the Sources list says "comments not read" / "with what you pasted" for closed platforms, read from the source so a Re-extract says the same; Transcript mode ignores comments and pastes |
| `lib.rs` | `pasted_link_candidates`; `ingest_sources(pasted)`; `get/set/clear_youtube_key`; `comments_closed` in the analysis |
| frontend | thread kinds labelled; "Add what's in the comments" box under Facebook / Instagram / TikTok / X / Reddit sources, its links ranked and ticked as you type; Settings › Links (the YouTube key) |
| `examples/ingest_probe.rs` | `--comments <url>` (YouTube with `YOUTUBE_API_KEY` in the environment) |

**Departures from the draft, and why:**

- **YouTube is the Data API only, not InnerTube first (§11).** The InnerTube
  comment continuation could not be measured from here, and a blind
  implementation of an undocumented shape is the kind of code that fails
  silently. The Data API's shape is documented; without a key, YouTube links
  read exactly as before. InnerTube stays a follow-up once measured.
- **No setting to show commenter handles (D7).** Handles are never written;
  the setting can come when someone asks for it.
- **Meta's tokenless oEmbed for Facebook posts is not built** — it was
  conditional on §9 item 5, which needs a live measurement.
- **Comments are read whenever a reader exists** — no per-ingest switch.
  They are capped (30 / 12 000 characters) and Transcript mode ignores them.
- **`Thread` sources have their own kinds** (`bluesky`, `mastodon`,
  `hackernews`), so the quality gate's length rules (web pages only) do not
  judge a short post with good replies as "too little text".
- **Pasted links are not resolved** (no network while typing); a short link
  in a paste is resolved when it is fetched.

## 17. Phase 3 as built (2026-10-28)

Built: the AI pick (§5.4) and per-account auto-follow (D2).

| Where | What |
|---|---|
| `llm/prompt.rs` | `LINK_PICK_PROMPT` — prompts live here so receipts version them |
| `ingest/links.rs` | `pick_message` (every URL as `display_url`, and URLs inside the text fields redacted), `parse_picks` (lenient shape, strict content: listed indices only, no repeats, ≤ 5), `ai_pick` (one `chat` call) |
| `lib.rs` | `pick_link_candidates` — receipts + budget (`"pick_links"`), `AiScope::check_source`, `checked_account_provider` (AI permission, masking), cancellable; `get_link_settings` / `set_link_settings` |
| `link_settings.rs` | `app_links.json`: the accounts that auto-follow; `AUTO_FOLLOW_MAX` 3, `AUTO_FOLLOW_MIN_SCORE` 40 |
| frontend | "Let AI choose" under each Find-links list (re-ticks that source's list; the AI's reason shows as "AI: …"; a refused AI permission offers Allow); auto-follow on the Extract modal; per-account toggles in Settings › Links; a per-modal discovery cache so editing the text never re-reads a page |

**Departures from the draft, and why:**

- **Auto-follow does not skip the list (D2 said it would).** With it on, the
  modal runs Find links on every ticked source by itself and ticks the
  strongest (≤ 3 per source, score ≥ 40 — stricter than the pre-tick's 25);
  the list is still on screen before Make note. Same number of taps as
  skipping it, and nothing is fetched or summarised that the user could not
  have seen. A line under the sources says auto-follow is on.
- **The setting lives in Settings › Links, not on the account.** A per-account
  list in `app_links.json` keeps `accounts.json` (and every `Account`
  literal) untouched, and sits beside the YouTube key it belongs with.
- **The AI pick is a button, not a checkbox.** It acts once, on one source's
  list, replacing that list's ticks (and anything followed from the unticked
  ones); ticking by hand afterwards works as before.
- **The AI pick is its own request**, with its own receipt and budget
  admission, not a call inside the ingest's attempt budget — it happens
  before the ingest exists.
- **Roadmap 0b's gate on untriggered LLM calls is still not built.**
  Auto-follow triggers page reads, never an AI call; the AI pick is always
  a tap.
- **URLs inside the anchor, sentence, title and the person's own words are
  redacted too** (`urls::redact_url_secrets`) — a bare-URL anchor is the
  URL, token and all. A test caught it.

**Live pass owed:** the AI pick on a real provider (Thai and English pages),
and auto-follow on a links-only paste.

## 18. Phase 4 as built (2026-10-28)

Built: the private reader (§7.3), desktop only.

| Where | What |
|---|---|
| `ingest/render.rs` | the reader: label prefix `jodd-reader-`, init script, probe and read scripts, `Settle` (load + idle), `navigation_allowed`, `vet_final`, the in-memory page cache, `RENDERED_MARK`; desktop `render()` builds, drives and destroys the window |
| `ingest/net.rs` | `vet` — `send_guarded`'s per-request check on its own, for a client that cannot be pinned |
| `ingest/web.rs` | `fetch_web` split into fetch + `read_document`; `read_rendered` reads a rendered page exactly like a fetched one (JSON-LD, readable text, published comments) and marks it |
| `ingest/mod.rs`, `run.rs` | `HttpFetcher.rendered`: a web source with a rendered page is read from it, not fetched; the Sources line adds "rendered in a private reader" |
| `lib.rs` | `render_source_for_ingest` (web links only, cancellable, Android → "desktop only for now"); `ingest_sources(rendered)`; the quit guard destroys any reader still open |
| frontend | after an ingest fails with "no readable text", a box under the sources offers "Open in a private reader" per page; it says what it read ("Read “title” — N words"), and the next Make note sends those pages as `rendered`. Disabled on Android with "desktop only for now" |

**How each §7.3 / §8 rule is met — the security review P4 waited on:**

| Rule | How | Pinned by |
|---|---|---|
| Never automatic | Only the button calls `render_source_for_ingest`; nothing in `ingest_sources`, Find links or auto-follow opens a webview | — |
| Ephemeral, isolated store | `incognito(true)` on every engine (WebView2 InPrivate; WebKit's non-persistent `WKWebsiteDataStore`; WebKitGTK's ephemeral context), plus on Windows a **fresh WebView2 user data folder per read**, `…/jodd/reader-webview/<label>`, deleted after (retried while WebView2's browser process lets go; leftovers swept on the next read). Never `icloud_webview_dir`, never `data_store_identifier` | — (live pass) |
| No IPC | Tauri 2.11 refuses every command — app or plugin — from a remote origin no capability names (`webview/mod.rs`, the `!is_local` arm). No capability has `remote`, and none matches `jodd-reader-*` | `no_capability_reaches_the_private_reader` (reads every file in `capabilities/` and `tauri.conf.json`; a non-JSON capability file fails it until the test learns that format) |
| Read back without IPC | `WebviewWindow::eval_with_callback` — the engine's own script-result channel (`ExecuteScript` / `evaluateJavaScript` / `evaluate_javascript`), each JSON-encoding the result. The script returns a JSON *string*, so all three hand back one shape | `script_results_decode_from_their_json_string` |
| Top-level URL checked first | `net::vet` before the window exists | `the_private_reader_is_desktop_only_and_for_web_pages_only` (order of kind check and render) |
| Where the page may go | `on_navigation`: http(s) only, no literal private address, no local name (`localhost`, `.local`, `.internal`, `.lan`, `.home.arpa`, single-label). The final page is resolved and vetted again (`vet_final`) before its HTML is used. Popups denied (`on_new_window`), downloads denied (`on_download`) | `navigation_stays_on_public_web_pages` |
| Nothing clicked, typed, scrolled | One probe script and one read script; the init script only stubs `alert`/`confirm`/`prompt`/`print`/`window.open` (a dialog in a hidden window would stall it) and mutes media | — |
| Load + idle ≤ 15 s | 500 ms probes; idle = `readyState complete` and no new resources and no new text for three probes; at 15 s, whatever is there | `idle_needs_load_and_three_quiet_probes` |
| The note says it was rendered | `RENDERED_MARK` is the source text's first line, so it survives into the stored block and a Re-extract; the Sources line adds "rendered in a private reader" | `a_rendered_page_says_so_in_the_note_and_on_re_extract`, `a_rendered_page_is_read_like_a_fetched_one_and_marked` |
| The app can quit | The quit guard (no visible window left) destroys every `jodd-reader-*` window | — |

**Accepted, as §8 said:** subresources (scripts, images, XHR) are fetched by
the engine with its own DNS and are not vetted — a page can make the user's
machine request a private address, exactly as opening it in their browser
would. That is why the reader is a button.

**Departures from the draft, and why:**

- **The offer appears after a failed ingest, not on the source row before
  one.** Nothing before an ingest knows a page has no readable text (the
  analysis does not fetch). The box appears when an ingest fails with every
  source unreadable and names the pages that need JavaScript; the URLs are
  the backend's canonical spelling, which the reader and the ingest key on.
  A mixed run that wrote a note does not offer it — ingest that link on its
  own. Worth revisiting if the live pass finds it awkward.
- **The rendered HTML never touches disk and never reaches the frontend.**
  It waits in process memory (≤ 8 pages, 30 min) for the `ingest_sources`
  call that names it; the frontend gets a title and a word count. Copied,
  not taken, so a retry after an AI failure does not render again.
- **Re-rendering is not offered for a page that is still empty.** Its
  status becomes "no readable text, even rendered in a private reader (the
  page may need a sign-in)", which the button's matcher does not match.

**Not verified — no webview runs here.** Every line of `render()` compiles
and none has executed. The live pass, per engine (WebView2, WKWebView,
WebKitGTK): the hidden window loads and runs the page's JavaScript; the
idle rule ends before 15 s on a typical app page; nothing shows in the
taskbar or Dock; a sign-in done in the main app or the iCloud webview is
not visible to the reader (and the reverse); the data directory is gone
after the read (Windows); quitting mid-read quits; Android shows the
button disabled.

## 19. Live pass (2026-10-29, macOS)

Run against `main` at `ee47a899` (PR #251) from a residential connection in
Thailand: the probes from this branch, the app as a debug `.app` bundle built
from this branch at `0744c35c` (#251 plus the first fixes below — `render.rs`,
the AI pick and auto-follow unchanged), opened on the owner's real database.
In-app notes went to the SSH account's Inbox. Times are from `jodd.log` unless
said otherwise. "Good" in a precision count is the tester's judgement: would a
reader of this page want this link summarised next to it.

### 19.1 Find links — the rules and score (§9 item 9)

`ingest_probe --links` on 18 real pages: BBC, The Verge, Simon Willison, a
Lexifina blog post, Prachatai, Matichon, Thairath, The Standard, The Matter,
MDN, the Rust book, a GitHub README, Netflix's tech blog (Medium), Astral Codex
Ten and the Pragmatic Engineer (Substack), a YouTube description, Thai and
English Wikipedia. Two short of the ≥ 20 §9 asked for.

**Pre-tick precision: 39 / 51 good (76 %) as shipped; 42 / 50 (84 %) with the
fixes below.** Per page, as shipped: Verge 5/5, Lexifina 5/5 (all primary
law sources), MDN 5/5, GitHub 5/5, English Wikipedia 5/5, Rust book 3/3,
Prachatai 1/1, YouTube 1/1, Pragmatic 4/5, Simon Willison 3/5, Thai Wikipedia
2/5, Netflix 0/1, Astral Codex Ten 0/5. Find links takes 0.1–2 s a page; BBC
took 9.4 s.

Fixed on this branch (each with a test named for the live pass):

| Page | What went wrong | Fix |
|---|---|---|
| Netflix (Medium) | The blog's own home page was pre-ticked: Medium appends `?source=post_page…` to every link, so the home-page rule (`query().is_none()`) missed it | A query of only tracking keys (`utm_*`, `source`, `ref`, `fbclid`, …) is no query |
| Netflix (Medium) | The two best links (earlier posts, cited in sentences) scored 0: "a top-level page of the same site" (−15) hit Medium's one-segment article slugs | A single segment of ≥ 4 hyphenated words is an article slug, not a section |
| Astral Codex Ten | Five `docs.google.com` links earned "docs, paper or official site" | `docs.google.com` is someone's document, not documentation |
| Lexifina | `<www.law.cornell.edu/…>` anchors counted as "descriptive link text" | An anchor that is the link's own URL — bracketed or without a scheme — is not |
| Thai Wikipedia | 3 of 5 pre-ticks were the "needs more sources — please help improve it" banner's policy and edit links | MediaWiki's `ambox`, `metadata` and `mw-editsection` classes are furniture (same in every language edition) |

Not fixed — recorded for whoever tunes next:

- **Unreadable links win ticks.** All five Astral Codex Ten pre-ticks are
  Google Docs (still 50–60 without the docs bonus): fetched, a Doc is 226
  characters of editor menu, so the §5.5 gate drops each one and the
  follow slots are wasted. Reading a public Doc through `/export?format=txt`
  would turn these into good sources. JSTOR answers a "Client Challenge"
  page (209 characters, status ok) — gated the same way.
- **Thai news links out rarely.** The Standard, The Matter and Thairath had
  no outbound link in the body at all; Prachatai one. Find links offers
  nothing on most Thai news, which is the page, not the rules. Matichon is
  a Cloudflare challenge (`cf-mitigated: challenge`, HTTP 403) and cannot
  be read at all.
- **Left in, never ticked:** `/news/topics/…` (BBC) and `/authors/…` (Verge)
  are listing pages the rules miss (`LISTING_SEGMENTS` checks the first
  segment and `author`, not `authors`); `google.com/preferences/source`
  (Thairath's "add as preferred source"); `github.com/sponsors/…`.
- A private `claude.ai/code/…` session link is pre-ticked on Simon
  Willison's post (60); the AI pick drops it (§19.4).

§15's other owed checks: a robots.txt-disallowed followed link — **passed**
(Thai Wikipedia `/w/index.php`: "not worth reading: the site asks automated
readers not to open this page (robots.txt)"); a YouTube description — passed
(the `manim` repo pre-ticked; Patreon/Spotify/Bandcamp left unticked). A
short link and an X post with a link were **not exercised**.

### 19.2 Discussions (§9 items 1–3, 8)

| Reader | Thread | Read / platform count | Kept | Time |
|---|---|---|---|---|
| Bluesky, logged out | pfrazee.com post (handle → DID resolved) | 28 / 12 | 15, author 4 | 1.6 s |
| Bluesky | DID-form URL | 10 / 10 | 6 | 0.9 s |
| Mastodon | mastodon.social, fosstodon.org, hachyderm.io | 5/5, 4/4, 2/2 | 4, 4, 2 | 0.7–2.4 s |
| HN (Algolia) | 333-comment story | 200 / 333 | 13 | 1.9 s |
| HN (Algolia) | xz backdoor, 1 773 comments | 200 / 1 773 | 16 | 3.4 s |
| Discourse (meta.discourse.org) | 21-post topic | 19 / 21 | 11, author 6 | 4.4 s |
| WordPress comment feed (ma.tt) | 25 and 15 comments | 25 / 25, 15 / 15 | 17, 15 | 4.0–4.2 s |

- **Bluesky (§9 item 1):** `getPostThread` works logged out on the public
  AppView. The post's `replyCount` counts direct replies only (12), the
  thread holds 28. Rate limit not measured.
- **Mastodon (§9 item 2):** `/context` answered unauthenticated on all three
  servers; no `Partial`.
- **HN (§9 item 3):** Algolia's `items/<id>` is 171 KB for 333 comments and
  1.04 MB / 6.5 s (curl) for 1 773. `MAX_COMMENTS_READ` (200) cuts in tree
  order, so a big thread is chosen from its first subtrees only.
- **JSON-LD `comment[]` (§9 item 8):** not sampled across the owner's saved
  links. Discourse carried JSON-LD; WordPress did not and was read from its
  comment feed.

Fixed on this branch:

- **An arrow pointed at the wrong comment.** The author's comments are
  listed first, but an author *reply* kept its `↳`, so it read as answering
  the line above (pfrazee: "it's 6 ligma" appeared under an unrelated
  author comment). An arrow now appears only directly under its parent;
  elsewhere a reply says "(a reply, …)".
- **Commenter names (D7).** WordPress opens a threaded comment with "In
  reply to <a href=…#comment-N>Name</a>." — the name went into the source,
  and the self-link into the author's links. The paragraph is dropped when
  its one link points at `#comment-`, so it works in any site language.
- **Mastodon handles (D7).** Every reply opened with the `@handle`s its
  client adds ("@Gargron These are gorgeous!"); the leading ones are
  dropped, a mention inside the sentence stays.

Not measured: §9 items 4–7 (YouTube comments need the owner's key; Meta
oEmbed, Instagram captioned embed, Reddit `.json`).

### 19.3 The private reader (P4) — macOS / WKWebView

| Check (§18) | Result |
|---|---|
| Offered after "no readable text" | **Passed** — Conduit (RealWorld demo, Angular), `icloud.com/notes`, `figma.com/community` each failed with "no readable text (the page may need JavaScript)" and got the box |
| Reads, idle rule ends before 15 s | **Passed** — Conduit 6.0 s (42 027 characters of HTML), iCloud 3.1 s (31 580) |
| The note says so | **Passed** — Sources: "Conduit — ok — rendered in a private reader"; the page's published comments reached the note |
| Nothing in the Dock | **Passed** — foreground apps (`lsappinfo`) sampled every 0.5 s: 5 throughout; the reader window is 400 × 300, off-screen, never shown |
| Can't see the owner's sign-ins | **Passed** — `icloud.com/notes` rendered signed out ("Notes - Apple iCloud", 25 readable words, same as a clean browser's Sign In page) while the iCloud webview is signed in with 773 notes |
| The iCloud webview can't see its pages | **Passed** — after two reads, Jodd's persistent WebKit stores (the iCloud one included) hold the same 212 files as before, none mentioning the rendered site |
| Quitting mid-read quits | **Passed** — Quit 2 s into a read; the process was gone within ~1 s |
| **The window is destroyed** | **Failed on the first pass; passed after the tao fix** (2026-10-29, below) |

**Every read left an empty native window behind.** `render()` called
`win.destroy()` (result ignored); the webview went — the window had no web
content or accessibility tree left and one WebKit content process exited —
but the `NSWindow` ("Jodd — private reader") stayed in the app's window list
and the window server's (`CGWindowListCopyWindowInfo`, off-screen) until
quit: two reads, two windows. Quit was not blocked.

**Cause: tao, not the reader — and every macOS window, not only this one.**
`examples/reader_window_probe.rs` opens and destroys windows the way
`render()` does, three per variant, and counts `NSApp.windows` and the
window server's windows for the pid. Before the fix **every** variant left
one window per cycle: visible or hidden, incognito or not, with or without
the init script and the navigation / new-window / download handlers,
`close()` as much as `destroy()`, and the shipped builder itself. With
`--trace`, the teardown all ran — tao dropped its window and delegate,
`windowWillClose:` fired, the WKWebView was freed — yet the `NSWindow`
kept exactly one strong reference. It was tao's own: `create_window` takes
the owned (+1) result of `alloc` + `initWithContentRect:` with
`Retained::retain`, adding a second reference nothing releases (tao #1120,
2025-08; `set_focusable` leaks one per call the same way). Upstream `dev`
no longer has it — the window became a `define_class!` type — but that tao
is out of reach of tauri 2.x, like #1304 (gotcha #32).

**Fixed in the vendored fork:** `bbmediaco/tao` rev `707f5c4` (branch
`macos-window-leak-0.35`, on top of the #1304 rev), which takes the
reference with `Retained::from_raw`. `render()` now logs a failed
`destroy()` instead of ignoring it.

| Measurement (macOS 27.0.1) | Before | After |
|---|---|---|
| Probe, 11 variants × 3 open/destroy cycles | 1 window left per cycle, every variant | 0 in every variant, 4 runs |
| Probe `--trace`, one read | `NSWindow` alive, 1 strong reference | window, content view and WKWebView all freed |
| Debug `.app` on the owner's database, two reads (Conduit — "Conduit", 156 words, 5.0 s; `icloud.com/notes` — 25 words, 3.4 s) | 2 extra 400 × 300 windows | window server: the same 7 windows (same IDs) before the first read and after the second; AppKit lists only "Jodd" |

The window server's 7 are the main window plus windows AppKit makes once
per process (a 500 × 500 untitled one and the menu-bar strips); the probe
lists the untitled one and does not count it.

Side finding, not fixed: on macOS `tauri-runtime-wry` 2.11.4's
`with_webview` hands out the WKWebView, its content controller and the
`NSWindow` with `Retained::into_raw` and never releases them — one
reference each per call, which would keep that window alive after it
closes. Jodd's own `with_webview` calls are all Android-only today.

**Superseded by the tauri 2.12 upgrade (2026-10-29).** tauri 2.12 takes tao
0.37 from crates.io, whose `define_class!` window is ownership-correct, so
the `bbmediaco/tao` fork is gone; tauri-runtime-wry 2.12's `with_webview`
lends its pointers (`Retained::as_ptr`), which fixes the side finding. On
0.37.2 the probe still reads 0 in every variant (four full runs; 33/33
traced windows freed). Details in gotcha #32.

Also seen:

- **The offer needs a completely empty page.** A JavaScript app whose shell
  has a few words (a Google Doc's 226 characters of menu) reads as "ok" and
  is never offered the reader; a bot challenge (Matichon's 403) fails
  differently and is not offered either.
- The title shown is the page's `<title>` — "Conduit" for every article of
  that app, which never updates it.

Not exercised: WebView2 (Windows: per-read data folder deleted), WebKitGTK,
Android's disabled button.

### 19.4 The AI pick and auto-follow (P3)

On the owner's configured provider:

- **English** (Simon Willison, 20 candidates): 5 picked in **7.1 s**, each
  with a reason — the spec file, the post defining "vibe coding", the chat
  transcript, two earlier posts the article builds on. It dropped the
  rules' private `claude.ai/code` session link and two tangential ones.
- **Thai** (Thai Wikipedia, the build without the banner fix): 5 picked in
  **5.6 s** — it un-ticked all three maintenance-banner links and the
  bot-walled ASEAN page, and kept a DOI chapter, an archive.org book and a
  ResearchGate paper on Thai tones. Reasons came back in English.
- Two of the five English picks were below "Show 12 more": **ticked links
  the user cannot see** until they expand the list.
- **Auto-follow on a links-only paste** (two URLs, nothing else): Find links
  ran on both sources by itself (0.3 s, 0.7 s), ticked exactly 3 per source,
  all ≥ 40 (a 40-point link left unticked at the cap), and showed
  "Following the best links automatically — change it in Settings › Links."
  Turned back off afterwards; saving the setting with an empty YouTube key
  never touches the keychain.

End to end (Thai Wikipedia, 5 followed — the click landed on Make note
instead of Let AI choose): the note took 2 min 37 s; its Sources list says
"— followed from <page>" on each followed link and lists the gated ones
with their reason (robots.txt, "HTTP 307", no readable text). The followed
`วิกิพีเดีย:RS` policy page made it into the note's title — "…; Thai
Wikipedia sourcing policies" — which is what the banner fix prevents.

Also seen:

- Thai Wikipedia URLs show percent-encoded (`%E0%B8%A0…`) in the pick list
  and in the note's "followed from" lines; decoded, they would be readable
  Thai.
- `asean.org` sits behind Sucuri: a 307 with **no `Location`** (a JavaScript
  challenge). Failing is right; "HTTP 307" says nothing a reader can use.
- Opened from Share to Jodd, the modal sometimes showed no source list or
  Find links until the Source box had been focused and left.
- Only the SSH and Gmail accounts are listed under auto-follow; the iCloud
  and Microsoft accounts (AI off) are not.

### 19.5 Still owed

Windows (WebView2) and Linux for the private reader; Android's disabled
button; YouTube comments with a key; §9 items 4–8; a short link and an X post
through Find links; the rules on two more pages to reach §9's 20.
