# Research Studio stage 2a — related notes from other folders

Roadmap #16, stage 2, first half. Builds on stage 1
([2026-10-23-research-studio-design.md](2026-10-23-research-studio-design.md)),
which is shipped (v0.37.0) and is the reference for everything this document
does not change. The owner asked for a recommendation "by best practice" and
then said "plan and implement"; every design call below is mine unless marked
as the owner's, and §10 lists them for checking.

## 1. Intent

A Studio run reads only the research folder. The notes that matter for a
topic are often elsewhere: a note filed under another project, a source
ingested into Inbox, a note that links to the folder's notes or shares their
tags. Today the owner has to find and move them by hand before generating.

Stage 2a lets the Studio dialog **suggest notes from other folders of the same
account**, found on the device without any AI call, so the owner can tick
them in as sources. A ticked suggestion becomes a source exactly like a
folder note: sent, cited, and given the backlink.

### Success

- Opening the dialog on a folder whose notes link to, or share tags or title
  words with, notes elsewhere lists those notes under **From other folders**,
  each with the reason it is suggested.
- Ticking one adds it to the estimate and, on Generate, to the run; the
  generated note cites it and the cited note shows the backlink.
- Nothing about a suggestion leaves the device unless it is ticked.

## 2. Goals, non-goals, anti-goals

**Goals**

- Deterministic suggestions from three signals: links, shared tags, text.
- Same account only; hidden folders never suggested and never walked through.
- The stage 1 dialog stays as fast as today: suggestions load after it.
- Ticked suggestions run through the same re-check, estimate, citation and
  backlink paths as folder notes.

**Non-goals**

- Web research (stage 2b — its own spec; the owner chose "your own search
  server", SearXNG on the AI host, on 2026-10-25).
- AI ranking or AI-judged relevance of suggestions.
- Suggestions from other accounts (a `[[link]]` only opens within an account).
- Moving notes into the research folder, or editing any existing note.
- A "See also" list of unticked suggestions in the generated note (decided
  2026-10-25: ticked notes are already linked through citations; unread links
  are the noise stage 1 avoided by skipping auto-link).
- Remembering ticks between dialog openings.
- Changing Ask Jodd, auto-link or Organize. (Auto-link's Thai keyword split is
  being checked in a separate session.)

**Anti-goals — it fails even if it "works" when:**

- Any title or text of an unticked note reaches an AI provider, or the
  finder adds anything to what is sent. (Stage 1 already sends the account's
  tag vocabulary — the tag names of every non-hidden folder,
  `ai.tags_visible_to_ai`, `studio_commands.rs:73, :97`,
  `llm/prompt.rs:527` — so the output reuses existing tags, roadmap #0. That
  is unchanged here and out of scope.)
- A suggestion, or a reason shown for one, comes from or through a hidden
  folder.
- A suggestion is ticked by default (the owner would pay for notes they did
  not choose).
- The folder list appears later than it does today.
- The list fills with noise: notes that share only a tag carried by a large
  share of the account (`#todo`).

## 3. What the owner sees

In `StudioModal.svelte`, below the folder's note groups, a new section:

- Heading **From other folders**, with the count. While loading:
  "Looking for related notes…". With none: the section is not shown.
- Each row: an unticked checkbox, the title (`displayTitle`), the folder
  (`viewName`), and up to three short reason chips:
  - **Links** — "Linked with 3 notes" (in either direction).
  - **Tags** — `#ev #battery` (the shared tags, at most three shown).
  - **Words** — "Mentions “battery”" (the matched terms, at most two shown).
- **Select all** keeps ticking only folder notes. Suggestions are ticked one
  at a time.
- When the suggestions were capped: "Showing the 20 closest."
- The section re-loads (debounced like the plan) when Focus changes, because
  Focus words are one of the text terms. Ticks on suggestions that are still
  listed survive the reload; a ticked note that drops out stays ticked and
  stays shown (so the owner never loses a choice silently).

Phone and tablet use the same component, so the section appears there too.

## 4. How suggestions are found

New module `src-tauri/src/studio/related.rs`, Tauri-free, called by a new
command `studio_related(account_id, label, focus) -> StudioRelated`.

```rust
pub struct RelatedNote {
    pub uuid: String, pub title: String, pub label: String,
    pub date_ms: i64, pub chars: usize,
    pub linked_with: usize,        // distinct folder notes linked either way
    pub tags: Vec<String>,         // shared, non-noise tags, sorted
    pub terms: Vec<String>,        // matched search terms, in term order
}
pub struct StudioRelated { pub notes: Vec<RelatedNote>, pub capped: bool }
```

### 4.1 Seeds

The seeds are the research folder's notes as `plan::list_candidates` would
list them: `list_notes_in_subtree` filtered by `AiScope::allows`, Studio
notes (`is_studio_note`) **excluded as seeds** — their citations would make
every source of an earlier run look "linked". At most `MAX_LISTED_NOTES`
(200) seeds, newest first. If `ai.check_folder` refuses the research folder,
the command refuses with the same message `studio_plan` gives.

### 4.2 Signals

For every candidate note `c` (any note of the account):

1. **Links.** For each seed, `db.outgoing_links` and `db.backlinks`
   (the queries `ask/related.rs` uses). `linked_with` = the number of distinct
   seeds `c` is linked with. Direct links only, no second hop.
2. **Tags.** The seeds' tags (`db.list_tags_for`), minus **noise tags**: a tag
   whose account-wide note count (`db.list_all_tags`) is greater than
   `max(NOISE_TAG_FLOOR, account_note_count / 5)`, with
   `NOISE_TAG_FLOOR = 10`. For each remaining tag,
   `db.notes_sharing_tags`. `tags` = the shared non-noise tags.
3. **Words.** Terms come from `ask::terms::extract_query_terms` (it keeps
   Thai words whole; autolink's extractor does not), at most
   `MAX_TERMS = 8`:
   - first the Focus line's terms, in order;
   - then terms from the seeds' titles, ranked by how many seed titles
     contain the term, then alphabetically; a title term found in only one
     seed title is used only while fewer than 8 terms are chosen.
   A term with no letter (`2026`, `2026-10-25`) is never used: a folder of
   dated notes would otherwise suggest every note that mentions the year.
   Each term is searched with `db.search_notes(Some(account), None, term, &[])`.
   **`search_notes` also returns notes that match only by folder name**
   (its second pass, `db.rs:2061–2063`, `folder_text_sql`). Drop those: keep
   a hit only when the term occurs (case-insensitively) in its title or in
   `strip_html_to_text(body_html)`. Otherwise a title word like "battery"
   would suggest every note in `Notes/Battery` as "Mentions". Builder's
   choice between that post-filter and a text-only variant of the query.
   `terms` = the terms whose kept hits include `c`.
   **As built (review fix):** the text-only variant,
   `db.search_text_outside_subtree(account, label, term, 200)` — `{title
   body}` only, and the research subtree excluded in SQL *before* the cap
   (the `list_notes_in_subtree` literal prefix, negated). With
   `search_notes`' account-wide cap of 200, the research folder's own notes —
   which contain the title words by construction — could fill every slot and
   hide all outside matches.
   **Whole words and common words (live pass 2026-10-26).** On a real
   446-note Gmail account, all 20 suggestions for a five-note office-move
   folder were word-only noise: the trigram index matched "move" inside
   "remove", and everyday title words ("draft", "meeting", "internet") hit
   ingested notes through their saved transcripts. So:
   - a term with no letter of a script written without spaces (Thai, Lao,
     Myanmar, Khmer, kana, CJK) counts only as a **whole word** in the
     lowercased title + text — no letter or digit right before or after it
     (`move-in` and `move.` count; `remove` and `moves` do not). Thai and
     the others keep the substring match, since a boundary cannot be seen;
   - a term found (after that check) in more than
     `max(NOISE_WORD_FLOOR = 10, account_notes / 20)` notes outside the
     folder is **noise** and dropped, like a common tag. Stricter than the
     tag rule's 1/5 because words are far commoner than tags. It applies to
     Focus terms too: a common Focus word would otherwise fill the list.
   - **a word from the folder's titles must be in the other note's TITLE**;
     only a **Focus** word may match the note's text. With the two rules
     above alone, the second live check still listed 20 unrelated notes:
     raw transcript notes ("Start a 1-Person Business Using Claude (6 Hour
     Course)" is a six-hour transcript as its body) contain nearly every
     everyday word as a whole word. A title word names a topic; a Focus
     word is the owner's own choice.
   - the text a Focus word is checked against is the note's **own** text
     (`agent_memory::text::html_to_text(body, false)`): an ingest's
     verbatim source block (after `SOURCE_MARKER`) is left out.
   Re-checked live on 2026-10-26 (same folder, same account): 20 noise
   suggestions → 1 ("new note test move here", "move" in its title); with
   Focus "Figma", the Vanta note joins it through its text.
   Known limit, accepted: Thai has no word breaks, so a Thai title is one
   long term and matches only notes containing that exact run of text.

### 4.3 Filters (applied to every candidate, before ranking)

- Not a seed, and not in the research folder's subtree at all.
- Same account (defence in depth; the queries are per account).
- `ai.allows(account, c.label)` — a hidden note is dropped. Hidden notes are
  never seeds either, so nothing is reached *through* a hidden note.
- Not `deleted_pending` (the queries already exclude it).
- Not a Studio note (`is_studio_note`, using `db.creators_for`).
- At least one signal.

### 4.4 Ranking and cap

Sort by: number of signals present (0–3) descending; then `linked_with`
descending; then `tags.len()` descending; then `terms.len()` descending; then
newest first (`date_ms`, as `plan` computes it); then uuid. Keep the first
`RELATED_MAX = 20`; `capped` is true when more qualified.

### 4.5 Cost

Local SQL only, at most 2×200 link queries, one tag query per seed, one
`notes_sharing_tags` per kept tag, and 8 FTS searches. Builder: add a unit
test that runs the finder over a fixture of 200 seeds and asserts it stays
under a generous bound (5 s, for a loaded CI runner) so a regression is visible;
if a real folder is slow in the live pass, batch the link queries.

## 5. Running with suggestions ticked

### 5.1 Plan and estimate

`studio_plan` today keeps only `selected` uuids that are listed folder notes.
It changes to keep, besides those, any uuid that resolves (through
`resolve_note_uuid`, gotcha #16) to a note of the same account that
`ai.allows` — so a ticked suggestion counts in the estimate. The returned
`selected` contains both.

### 5.2 `load_sources`

`plan::load_sources` changes from "the subtree, filtered by the wanted set" to
resolving each wanted uuid:

- resolve the alias, then `note_by_uuid(account, uuid)` (already excludes
  `deleted_pending`);
- refuse the whole run with `FOLDER_CHANGED` when a note is gone or
  `ai.allows` refuses its folder — the same all-or-nothing rule as stage 1,
  now for notes anywhere in the account;
- a note in the research subtree is a folder source; any other note is an
  **outside source**.

**At least one folder source is required.** A run made only of outside notes
is refused with the existing "tick at least one note" message
(`NOTHING_SELECTED`), and the dialog's `canGenerate` keeps requiring a ticked
folder note (my call: Studio is "this folder, plus related", and the note
lands in this folder).

Keys stay `S1…Sn`, oldest first, over all sources together. `render_header`
already prints `Folder:` when a source's label differs from the research
folder, so the AI sees where an outside source lives.

Rename `FOLDER_CHANGED`'s text if it names "the folder" only: it should read
true for an outside note too (builder: pick wording in the file's voice, e.g.
"Some notes changed or moved since the list was made. Reopen the dialog.").

### 5.3 The generated note

- It still lands in the research folder.
- The marker line (`header_lines`) keeps its prefix — `is_studio_note`
  matches only `STUDIO_MARKER` — and gains the outside count when there is
  one: `Made with Jodd Studio from 5 notes in Research/EV and 2 from other
  folders · 2026-10-25`. Here the first number is the folder sources only
  (today it is `sources.len()`, `run.rs:121–122`); the second is the outside
  sources. Without outside sources the line is unchanged
  byte for byte (stage 1 tests pin it).
- Citations, the Sources list and backlinks work unchanged: `cite.rs` maps
  keys to slugs regardless of folder, and edges are per account.

### 5.4 Receipts and logs

The receipt's `source_version` already hashes `(account, label, uuids)`.
The `generate_studio_note` log line adds `outside={n}`. No note text in logs.

## 6. Privacy

- What leaves the device is unchanged in kind: the title, date, folder and
  text of the notes the owner ticks, plus Focus — now possibly from other
  folders of the same account — plus the tag vocabulary stage 1 already
  sends. Suggestions themselves are computed locally, and nothing the finder
  computes (reasons, counts, unticked notes) is sent.
- This implementation does **not** edit the Jodd-Homepage repo; it only
  hands over the wording below.
- Hidden folders: excluded as seeds, as candidates and at run time (§4.3,
  §5.2).
- **privacy.html** (Jodd-Homepage): the Studio line drafted on 2026-10-25
  (uncommitted, branch `site/privacy-studio`) says "the notes you tick in a
  folder", which is right for 0.37.0 and can be published now. With the
  release that ships this, change it to: "turns the notes you tick in a
  folder, and any related notes from your other folders that you tick as
  well, into a Briefing, FAQ, Timeline or Study guide; it sends their title,
  date, folder and text, plus any focus line you type; notes it suggests but
  you do not tick are never sent".

## 7. Testing

Rust (`studio/related.rs`, `studio/plan.rs`, `studio/run.rs`):

- links in both directions count; `linked_with` counts distinct seeds;
- a note linked only through a hidden note is not suggested; a hidden note
  is never suggested even when strongly linked;
- noise tags are ignored (threshold boundary both sides); a shared rare tag
  suggests;
- Focus terms come first; a Thai Focus term is searched whole;
- a note that matches a term only through its folder name is not suggested
  for that term;
- `load_sources` refuses a selection of outside notes only;
- seeds, subtree notes, Studio notes, other-account notes and deleted notes
  are never suggested; an earlier Studio note is not a seed;
- ranking order and the cap (`capped` true at 21 qualified);
- `load_sources` accepts an outside note, keys it with the rest oldest-first,
  and refuses the run when an outside note was deleted or its folder became
  hidden; a rekeyed outside note resolves through its alias;
- `studio_plan` keeps an outside uuid in `selected` and counts it;
- the marker line with and without outside sources (the latter unchanged);
- the cost bound test of §4.5.

Vitest (`StudioModal.test.ts`): the section renders from a mocked
`studio_related`; rows start unticked; ticking sends the uuid in
`studio_plan`'s `selected` and `generate_studio_note`'s `uuids`; Select all
leaves suggestions alone; a Focus change re-requests suggestions and keeps a
ticked suggestion; the section is absent with no suggestions.

Gates: the CI commands in CLAUDE.md (`gen-changelog`, `cargo test
--workspace --locked`, `vitest`, `svelte-check`, `npm run build`).

## 8. Live pass (owner's machine, after merge)

1. Gmail: a research folder whose notes link to two notes elsewhere and share
   a rare tag with a third. Open the dialog: all three are suggested with the
   right reasons; nothing is ticked.
2. Tick two, make a Briefing. The marker line counts them; both are cited;
   both show the backlink; both citations open the right note.
3. Hide one suggested note's folder, reopen: it is gone. Tick a suggestion,
   then hide its folder before pressing Generate: the run is refused and no
   note appears.
4. Type a Focus word that occurs in a note elsewhere: it appears with a Words
   reason.
5. Android (S23 FE): the section shows and a ticked suggestion is cited.

## 9. Risks

- **Noise vs recall.** The tag threshold and term choice are guesses; the
  live pass judges them. They are constants in one file so tuning is cheap.
- **Thai text recall is weak** (§4.2, one long term per title). Links and
  tags carry Thai folders; a later word segmenter would help Ask too.
- **Big accounts.** The finder is bounded by seeds (200), terms (8) and the
  FTS per-search cap (200); the cost test guards it.

## 10. Calls I made (for the owner to check)

1. Suggestions only add sources; no "See also" list (owner agreed to my
   recommendation, 2026-10-25).
2. A separate `studio_related` command, so the stage 1 list is not slowed.
3. Direct links only; no second hop.
4. Noise tag rule: more than `max(10, notes/5)` notes in the account.
5. At most 8 text terms: Focus first, then the most shared title words.
6. At most 20 suggestions, ranked by number of signals first.
7. Earlier Studio notes are neither seeds nor suggestions.
8. Select all does not tick suggestions.
9. A ticked suggestion that drops out after a Focus change stays ticked and
   shown.
10. The marker line names the outside count; without outside sources it is
    unchanged.
11. A run needs at least one folder note; outside notes only add to it.
12. Word matches count only in a note's title or text, never its folder name.
13. The tag vocabulary stage 1 sends is left as it is.
14. Spaced-script words match as whole words; Thai, Lao, Myanmar, Khmer and
    CJK keep substring matching (owner asked for this, 2026-10-26).
15. The common-word cutoff is 1/20 of the account (tags: 1/5), floor 10,
    and applies to Focus terms too.
16. Title words match other notes' titles only; Focus words match title or
    the note's own text (saved source transcripts excluded).
