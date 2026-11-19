# Source notes, phase 1 (2026-11-19)

**Status: Approved 2026-11-19.** The one open question (§10) was answered the same day.

**Owner's words** (2026-11-19, while fixing how jodd-mcp shows a stored
source): keep the source as its own **source copy**. Add a helper that checks
whether the copy still matches the original, how much the original has
changed, and what to do about it. A note **derived** from a source keeps its
relationship to the source, in case the source changes. Jodd has several
derive styles to choose from, and every result should point back to **one**
source copy: "ไม่อยากให้ต่างคนต่างเก็บ" (each one should not keep its own).

**Phase 1** is the foundation: the one shared copy and the link to it.
Checking for changes is phase 2, and acting on changes is phase 3 (§9). Phase
1 does not depend on them, and they cannot be built without it.

**Where source notes live:** the owner chose a `Notes/__Sources__` system
folder (option A, §3).

## 1. What happens today

URL ingest (`ingest::run::ingest_to_note`) fetches each link, runs the LLM
over the text, and writes **one note**. That note holds:

- the result,
- a visible Sources list (URL, title and status of each link),
- a collapsed Source block with every fetched text:
  `<hr><details><summary>Source (verbatim)</summary><pre>…</pre></details>`.

Inside the `<pre>`, each source is

```
=== Jodd source K of N ===
URL: …
Title: …
Status: …

<text>
```

That format is written by `ingest::stored::render_sources` and read back by
`parse_sources`. It is capped at 100 000 characters per source and 400 000 in
total. `defuse_headers` stops a page from forging a boundary.

The same raw text gets copied more than once:

- **Redo key points / Re-extract** (`ai_commands::re_extract_note`) parses the
  block and derives again without refetching. It writes a **new** note beside
  the old one, with its own full copy of the block.
- Ingesting the same link again, in another mode (Key points, Summarize, Expand
  bullets, Transcript) or on another day, fetches again and stores another copy.
- Nothing records when a copy was fetched or what it contained, so there is
  nothing to compare against later.

## 2. Goals, non-goals, anti-goals

**Goals**

1. A link that has been ingested has **one source note per account**. It holds
   the fetched text in the existing Source block format.
2. A note derived from link ingest, in **any** mode, carries no copy of the
   text. It **links** to its source notes.
3. Every way of deriving again from a note made under this design reads from
   its source notes and never refetches: Redo key points and Re-extract. A new
   ingest of the same link follows §5.2. Legacy notes keep today's inline path
   (§6).
4. Each source note records **when** it was fetched and a **hash** of what was
   fetched. Each derived note records which hash it was made from. Phase 2
   needs both.
5. All of it syncs and round-trips on every backend that has note writes:
   Gmail, iCloud, Microsoft, LocalFs and Ssh. All of it is readable in Apple
   Notes.

**Non-goals (phase 1)**

- Checking a source on demand, diffing, or flagging stale derived notes
  (phases 2 and 3). Phase 1's only check is the quick one when a link is
  ingested again (§5.2).
- Moving existing notes over. Notes with an inline Source block stay as they
  are and keep working (§6).
- Extract from **pasted text** (`extract_note_into`). Its Source block has no
  URL to key on, so it stays inline in phase 1. Phase 2 gives it a source note
  keyed by its content (§9).
- Share capture (`capture.rs`) and Save link / Save without AI. They store no
  fetched text.
- One source note shared across accounts. Wikilinks resolve within one account
  (hypermedia spec 2026-10-22), so each account keeps its own.
- Removing a source note that no derived note uses any more.

**Anti-goals** (each would be a failure even if the feature worked)

- **Jodd rewrites a derived note on its own.** No phase ever does this. Phase 3
  flags; the user decides.
- **A derived note stops saying where it came from.** It keeps its Sources
  list as it is today, so it still names its links if the source note is
  deleted, moved, or in another account.
- **A source note's text ends up in search ranking, related notes,
  start_project, or AI context where today's inline block is kept out.** Every
  existing exclusion is keyed on the Source block, so the source note must use
  that same block (§5.1).
- **Source notes clutter the user's folders.** They sit in a system folder that
  appears under Workflows, not in the Inbox.
- **A migration that rewrites note bodies.** See gotcha #38 and the
  `migrate_tags_to_body` lesson: that kind of "repair" pushes edits into the
  user's real notes.

## 3. Approaches considered

| | Where the copy lives | Trade-off |
|---|---|---|
| **A. `Notes/__Sources__` system folder** (**chosen**) | A note in a system folder. `derive_workflow_kind` makes any `__x__` leaf a `system_workflow`, which the sidebar groups under Workflows | Out of the user's way, and readable in Apple Notes. Costs one more synced system folder or label. On a backend that cannot create folders it falls back (§5.6) |
| B. Beside the derived note | Same folder as the result | Simplest. But every ingest leaves two notes in the Inbox, and the copy looks like a note the user wrote |
| C. A SQLite table plus a sync sidecar | Not a note | Needs a new sync mechanism on every backend. Apple Notes cannot read it, and backup/OKF would not carry it. Ruled out |

## 4. Constraints found in the code

These bind the design. Each one was checked on `main` at `dc8cb6c7`.

1. **Folders cannot be how a source note is recognised.** Microsoft folder
   writes are permanently `false`, and a user can move a note anywhere. A
   source note is recognised by its **body** (§5.1), the way Studio notes are
   recognised by `STUDIO_MARKER`. The folder is a convenience only.
2. **The relationship has to live in the body.** The `edges` table is fully
   derived and always safe to clear. A wikilink `[[note_slug]]` in the derived
   note's body syncs, and it becomes a `mentions` edge when edges are derived.
3. **Wikilinks resolve by `uuid8` within one account.** A rename rewrites the
   text of links that point at the note (`rewrite_links_to_renamed_note_conn`).
   **Gotcha #16:** a backend that assigns identity at CREATE rekeys the note.
   Resolution already follows `note_uuid_aliases` (`alias_short_id_matches`,
   `db.rs` ~3562), so a link written before the source note's first push
   resolves after the rekey. Test #7 pins it; no build step is needed.
4. **Six places find "AI-made notes" by `SOURCE_MARKER`**, which is
   `<summary>Source (verbatim)</summary>`:
   - `db/search.rs` `list_extract_notes` (the Extracts view);
   - `noteActions.ts` `hasSourceBlock` (gates Redo key points and the other AI
     actions);
   - `re_extract_note`;
   - the source exclusions in `agent_memory/text.rs` / `pack.rs`;
   - `studio/related.rs` (live pass 2026-10-26);
   - jodd-mcp `read_note`.

   A derived note without the block would drop out of the first two. Each
   place must also recognise the derived note's link line (§5.3), and the
   Extracts view must leave source notes out.
5. **`canonical_url` normalises only the four social kinds** (D12). YouTube and
   ordinary web links come back unchanged, so the same video can appear as
   `youtu.be/ID` and `youtube.com/watch?v=ID`. The source key (§5.2) needs its
   own rule.
6. **On iCloud and SSH, `<details>`/`<summary>` and HTML attributes do not
   come back.** The text survives, but the markup does not. A pulled iCloud
   body is rebuilt from paragraphs (`icloud/doc.rs` `render_paragraphs`, ~539).
   `format_html.rs` ~1048 turns `<summary>Source (verbatim)</summary>` into a
   plain "Source (verbatim)" paragraph followed by Monospaced lines. SSH bodies
   go through the same renderer (`ssh/mdconv.rs` ~367).

   So **`SOURCE_MARKER` cannot be how a source note is recognised.** That
   applies to the exclusions too: after a pull on another device, a
   `SOURCE_MARKER` check would let the copy's text into search and AI context,
   and the lookup would miss it and write a second copy. Anything Jodd must
   find again has to be **visible text** (§5.1).

   Today's inline blocks on iCloud already lose their exclusions after a pull.
   That is an existing gap and is out of scope here, but the source note must
   not inherit it.
7. **Size.** A source note holds one source, capped at 100 000 characters,
   which is less than today's inline block can hold (400 000). No backend
   limit changes.
8. **Gotcha #37:** anything appended goes inside the document
   (`mime822::append_html` / `appendHtml`).
9. **AI scope** (`AiScope::check_label`) is checked per label. **MCP scope**
   denies by default (gotcha #33). `Notes/__Sources__` is a folder like any
   other for both.

## 5. Design

### 5.1 A source note

- **One fetched source per note.** Locally it is written as the existing
  Source block with `1 of 1`, so `extract_source` and `parse_sources` read it
  unchanged on Gmail, Microsoft and LocalFs.
- **It is recognised by visible text, not markup** (constraint 6). A source
  note is a note whose first line after the title is the marker line below,
  followed somewhere by the plain-text header `=== Jodd source 1 of 1 ===`.
  Each check that keys on `SOURCE_MARKER` (constraint 4) gains this recogniser
  for source notes:
  - the exclusions treat everything after the marker line as source text;
  - the lookup (§5.2) finds the note;
  - deriving again (§5.5) parses from the header line, whether or not
    `<details>` survived.

  An iCloud or SSH test must **pull** the note back and check all three on the
  re-read body, not just on the push.
- **Title:** `Source — <page title>`, falling back to the display URL. The title
  is the first line of the body (gotcha #11).
- **The marker line** is the first line after the title:

  ```
  Jodd source · fetched 2026-11-19 14:02 · sha256 <64 hex>
  ```

  That line, together with the `=== Jodd source 1 of 1 ===` header text, is what makes
  a note a source note. Neither one alone is enough. *Our call:* the date and
  hash go on this visible line rather than as new `Fetched:`/`Hash:` headers
  inside the block. `parse_sources` checks for an exact five-line boundary, and
  new headers would make every existing block parse differently.
- **The hash** is SHA-256 of the **fetched** text before truncation, so phase 2
  can compare it to a fresh fetch even when the stored copy was cut. It is
  written **in full, as visible text** on the marker line. Attributes and
  hidden spans do not survive iCloud or SSH (constraint 6).
- **HTTP validators.** When a web fetch returns them, the response's `ETag`
  and `Last-Modified` go on a visible line under the marker line, for example
  `etag "abc123" · last-modified Tue, 18 Nov 2026 09:00:00 GMT`. They are what
  makes the §5.2 quick check cheap. They must be visible text, for the same
  reason as the hash (constraint 6). No line is written when the server sends
  neither. *Today the fetcher (`ingest/web.rs`, `net.rs`) reads neither header,
  so this is new.*
- **Comments the user pasted** (`append_pasted`, D8) are the user's own words,
  not what the link says. They are kept in the source note but are **not
  covered by the hash**, so phase 2 compares only what was fetched. *Our call.*
  The builder may store them as a separately headed section after the fetched
  text, as long as `parse_sources` gives Redo the same text the first
  derivation saw.
- **The URL** appears once more as a plain link under the marker line, so the
  note is useful when opened on its own in Apple Notes.
- **Failed sources get no source note.** `FetchStatus::Failed` has no text. A
  `Partial` source does get one, and its status says why it is partial.

### 5.2 One per link: the source key

Before a source note is written, Jodd looks for one that already exists in the
same account under the same **source key**:

| Link kind | Source key |
|---|---|
| YouTube (`UrlKind::YouTube { id }`) | `youtube:<id>` |
| The four social kinds | `urls::canonical_url` |
| Everything else | the URL as fetched, without its fragment |

The lookup uses the body (constraint 1) and never the folder. A derived index
(`source_key → uuid` per account) is allowed if, like `edges`, it can be
rebuilt from bodies and cleared safely. Deleted (`deleted_pending`) notes do
not count.

**When the key already exists, Jodd does a quick check first, by default.**
The owner decided this on 2026-11-19: quick check by default, with an opt-out.

| Link kind | Quick check | Unchanged → | Changed → |
|---|---|---|---|
| YouTube and the four social kinds | Is the post or video still there? Ask the platform without downloading captions, a transcript or media. The content is fixed by its ID, so availability is the whole question | reuse the copy | the post is gone: reuse the copy and say so |
| Web page with saved validators | A conditional GET that sends back the saved `ETag` (`If-None-Match`) and `Last-Modified` (`If-Modified-Since`) | **304**: reuse the copy, with nothing downloaded | **200**: the new page is already in the response. Extract it, hash it, and compare with the saved hash. Same hash (HTML churn only): reuse. Different: **update the source note in place** |
| Web page with no validators, or one read in the private reader | None is possible | — | a full fetch, then compare hashes as for a 200 |

- **The setting (opt-out).** "Check saved links for changes before reusing
  them": **on** by default. Off means Jodd uses the saved copy with no network
  request, and the result says which day's copy it used. Where the setting
  lives, and whether it syncs across devices, is left to the builder (§8). A
  check that fails (offline, timeout, robots.txt refuses) falls back to the
  saved copy and says so. It never fails the ingest.
- **Update in place keeps one copy.** Derived notes made earlier keep the hash
  they were made from, so phase 2 can still tell they came from the older
  copy. On Gmail, Trash keeps the old text (every save trashes the previous
  message).
- **The saved copy has less than the user asks for now.** It may be `Partial`
  (no captions, say), or the user may now ask for something it was fetched
  without: transcribing the audio, YouTube comments, the private reader, or
  pasted comments (`ingest_commands.rs` ~380–450). Jodd then does a **full
  fetch** with the new options, **whatever the setting**, and updates the copy
  in place. Otherwise the user would quietly get the thinner copy.
- **The result always says which happened:** "saved copy unchanged", "saved
  copy updated", "used the saved copy from 2026-11-19 (not checked)", or "check
  failed: used the saved copy from …".

### 5.3 A derived note

- The content is as today: title, result, Keywords and the Sources list.
- **The inline Source block is replaced** by one line per source that has a
  source note:

  ```
  Derived from [[source-the-page-title-3f9a1b2c]] · copy of 2026-11-19 · sha256 3f9a…c21e
  ```

  The hash is the one the derivation read. Phase 2 compares it to the source
  note's current hash to tell "made from an older copy".
- A failed source has no line. It still appears in the Sources list with its
  status.
- **Following the line.** Redo and jodd-mcp resolve the link by its uuid8
  (following a rekey alias, gotcha #16). Failing that, they take the saved copy
  of the Sources-list URL at the same position, only if its sha256 is the
  line's (Ruling 14). The link is never rewritten after a rekey. **Known
  limitation:** Microsoft rekeys a note at CREATE and the alias exists only on
  the device that pushed it, so on another Microsoft device the `[[…]]` link in
  the editor stays dead; only Redo and jodd-mcp find the source note, by URL
  and hash. A body that lost its markup in a sync has no Sources-list `href`s,
  so there the fallback has no URL to try.
- **The `Derived from` line is what marks the note as AI-made.**
  `list_extract_notes` and `hasSourceBlock` (constraint 4) must accept either
  an inline Source block or at least one `Derived from` line. The Extracts view
  must leave out source notes themselves.

### 5.4 The ingest pipeline

`ingest_to_note` becomes two steps:

1. **Sources.** For each link: look up the source key (§5.2). If a source note
   exists, parse it. If not, fetch the link (robots.txt, the quality gate,
   followed links and pasted comments all unchanged), then render a source
   note in memory.
2. **Derive.** Run the chosen mode over the sources, as
   `IngestInput::Stored` does today. Then write the source notes that are new
   and the derived note **in one local transaction**.

*Our call:* new source notes are written only when the derived note is written.
A cancelled or empty ingest then still leaves nothing behind, which is today's
promise ("a cancelled or empty ingest never leaves an empty Inbox behind").
Keeping a fetch whose derivation was cancelled is a possible later change.

Every mode goes through this path: Key points, Summarize, Expand bullets and
Transcript, plus followed links (roadmap #18).

### 5.5 Deriving again

- **From a derived note** (Redo key points, Re-extract, and the other AI
  actions gated by `hasSourceBlock`): follow its `Derived from` links, parse
  each source note, and run `Stored` as today. The output goes beside the
  derived note, as today.
- **From a source note** (the user opens it and runs an action): the source
  note has a Source block, so the existing path works. The output **must not**
  land beside it in `__Sources__`. It uses `resolve_destination` (the Inbox).
- **A missing source note** (deleted, moved to another account, or out of
  scope): the action stops with the link's URL and a plain message saying the
  saved copy is gone and the link can be ingested again. It does not fall back
  to fetching.
- **A legacy note with an inline block:** unchanged.

### 5.6 The folder

- The path is `Notes/__Sources__`, created on demand the way other system
  folders are, only when `can_create_folders`. It respects `notes_label`
  wherever the folder side honours it (gotcha #9).
- **When the account cannot create folders** (Microsoft), or the folder is
  blocked, the source note goes where the derived note goes. It is still
  recognised by its body.
- The user may move or rename source notes. Recognition and links do not
  depend on where they are.

### 5.7 Scope, search and agents

- **AI scope.** Deriving again checks the scope of the source note's label
  **and** the destination, before anything is sent. A user who hides
  `__Sources__` from AI can still ingest new links: the fetched text goes to
  the LLM as today, but deriving again from a hidden source note is refused,
  like any hidden folder.
- **Search.** The block's text stays out of everything it is kept out of today
  (constraint 4). The source note's title and marker line are ordinary text.
- **jodd-mcp `read_note` with `include_source: true` on a derived note**
  follows the `Derived from` links and includes each source note's text, but
  only for source notes the MCP scope allows. For any it does not allow, it
  says so. *Our call.* The builder may move this to a follow-up if it grows,
  as long as the reply then says "source in note X" rather than nothing.

### 5.8 Delete and move

- **Deleting a source note** leaves the derived notes alone. Their link
  dangles the way any wikilink to a deleted note does, and deriving again says
  why (§5.5).
- **Deleting every derived note** leaves the source note alone. Phase 1 has no
  garbage collection.
- **Cross-account move or copy** (`transfer.rs`) does not carry the source
  note. Afterwards the moved note still has its Sources list, and deriving
  again says the copy is in the other account. Carrying it is a later change.

## 6. Existing notes (§2, non-goal)

Nothing is migrated. A note with an inline block keeps its Redo key points, its
place in the Extracts view, and its exclusions. *Our call:* Redo on a legacy
note runs exactly as today. It writes no source note and produces an inline
block. If it wrote source notes it would be a migration by the back door, and
would have to choose between its own copy and a different saved copy of the
same link.

`ALL_MAPS_FAILED` (`ingest/run.rs` ~315) says the fetched text "is preserved
below". For a new derived note, that must point to the source note instead. "Split the source out"
(per-note, user-run) can come later. It is not part of phase 1.

## 7. Tests the builder owes

Each test pins a promise above. Test **save → DB → save** where identity is
involved (gotcha #16).

1. Two ingests of the same link, in different modes, produce **one** source
   note and two derived notes. Neither derived note contains `SOURCE_MARKER`.
   Both link to the source note.
2. `youtu.be/ID` and `youtube.com/watch?v=ID` share a source note. Two web
   links that differ only in fragment share one.
3. Redo key points on a derived note makes no fetch calls (the fetcher counts
   them), reads the source note, and writes beside the derived note.
4. An action run on a source note writes to the Inbox, never into
   `__Sources__`.
5. The Extracts view lists derived notes and legacy notes, and does not list
   source notes. `hasSourceBlock` (or its successor) is true for a derived
   note.
6. A source note's text does not reach `find_focus`, start_project, or
   related-notes matching. Copy the 2026-10-26 "internet" test from
   `studio/related.rs` and point it at a source note.
7. **Rekey:** on a backend that assigns identity at CREATE, a derived note's
   link resolves to the source note after the source note's first push.
8. Cancel after fetch writes nothing: no source note and no derived note.
9. Microsoft (no folder writes, and whether `<details>` survives Graph is unproven): the source note lands beside the derived
   note, is recognised, and is reused.
10. A failed source gets no source note and no `Derived from` line, and still
    appears in the Sources list.
11. `defuse_headers` still holds: a page containing a forged
    `=== Jodd source 2 of 2 ===` line, stored in a source note, still parses as
    `1 of 1`.
12. The hash is computed over the untruncated fetched text. Two fetches that
    differ only after the cap have different hashes, and pasted comments do
    not change the hash.
13. **Pull-back, per backend** (iCloud and SSH at least): push a source note,
    pull it back, and check against the **re-read** body that it is still
    recognised, still excluded from search, found by the lookup (no second
    copy), and parsed by Redo.
14. **Quick check** (with a fake fetcher that records requests):
    - a saved web copy with an `ETag` sends `If-None-Match`, gets a 304, and
      downloads no body;
    - a 200 whose extracted text hashes the same reuses the copy and does not
      rewrite the note;
    - a 200 with a new hash updates the source note in place, and the earlier
      derived note keeps its old hash;
    - with the setting off, no request is made at all;
    - a failed check (timeout, or robots.txt refuses) uses the saved copy and
      says so;
    - a `Partial` copy, or a request for transcription the copy lacks, does a
      full fetch even with the setting off.

**Live pass:** Gmail and iCloud on the Mac. Ingest a link twice, then open
`__Sources__` in Apple Notes on an Apple device. Check that the source note
reads properly there and that the derived note's link is readable text. Re-check
the iCloud note after 20 minutes (CLAUDE.md, iCloud).

## 8. Left to the builder

- Where the quick-check setting (§5.2) lives in Settings, its wording, and
  whether it is per device or synced. Its default is fixed: on.
- How each social platform's availability check is made, as long as it
  downloads no captions, transcript or media.
- Whether the source-key lookup scans bodies or keeps a derived index.
- The exact wording of the marker line, the `Derived from` line, and the
  messages for a reused copy and for a missing source note. All wording must
  be clear in both Thai and English UI.
- How `noteActions.ts` learns a note is derived: it can test the body itself,
  or ask Rust.
- Splitting `ingest_to_note` into functions. The one-transaction write in §5.4
  is the fixed part.

## 9. Later phases, for context

- **Phase 2: check a source on demand.** From a source note, or from the
  `Derived from` line, "Check for changes" runs the same quick check as §5.2. If they differ, it shows a diff of the text, how much changed, and
  which derived notes were made from the older copy. The user chooses what to
  do: keep the old copy, replace it (the old text stays recoverable; on Gmail,
  Trash already holds it), or derive again. Robots.txt applies (D11).
- **Phase 2: pasted text gets a source note too** (added by the owner
  2026-11-19). Extract from pasted text (`extract_note_into`) keeps the pasted
  text as a source note in `Notes/__Sources__` instead of an inline Source
  block, and the derived note carries the same `Derived from` line (§5.3), so
  every derive style reads one copy, as for links.
  - **The key is the content.** With no URL, the source key is the hash
    itself (`text:<sha256>`), taken over the pasted text exactly as the
    derivation reads it. Pasting the same text again reuses the note and
    writes no second copy; changing one character is a new source note.
  - **No quick check.** There is nothing to ask a server, so §5.2's check
    does not apply: an existing copy with the same key is always reused, and
    the result says so.
  - **What the note shows instead of a URL.** The marker line says `pasted`
    where a link's says `fetched`; the title is `Source — ` plus the text's
    first line, cut short. *Left to the phase 2 design:* the exact marker and
    title formats, whether to trim or normalise whitespace before hashing,
    and whether a note picked as the AI source (rather than pasted text)
    should also get one — it already lives in a note, so probably not.
  - **Size.** The same 100 000-character cap as a fetched source; the hash is
    over the untruncated text (§5.2's rule), so a cut copy is never mistaken
    for the whole text.
  - Existing pasted-text notes are not moved over (§6 applies unchanged).
- **Phase 3: check in the background.** An occasional, rate-limited check for
  source notes that derived notes still use. It **flags** derived notes made
  from an older copy, and never rewrites one (§2, anti-goals).

## 10. Decisions by the owner

1. **Ingesting a link that already has a source note** (2026-11-19): do a
   quick check first, by default, and let the user opt out. See §5.2. Opting
   out is read as "use the saved copy without checking". If the owner meant
   "always fetch again", only the setting's off value changes.
2. **Pasted text** (2026-11-19): it gets a source note too, in phase 2, keyed
   by its content. See §9. Phase 1 keeps it inline.
