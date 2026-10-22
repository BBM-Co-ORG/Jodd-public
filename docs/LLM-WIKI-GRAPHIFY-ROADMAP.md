# LLM Wiki / Graphify / OKF-inspired knowledge management — handoff

> Status: **Phase 1 shipped** (2026-07-13, `main` @ `1b6c764`). This doc exists so a
> future session (or a compacted context) can pick up Phase 2+ without re-deriving
> the framing from scratch. Read this, then start with `superpowers:brainstorming`
> on whichever item you pick from "Next" below.

## Origin & framing

Three external patterns motivated this thread — see the conversation that opened it
for the full research, condensed here:

- **Karpathy's "LLM Wiki" pattern** ([gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)):
  a persistent, compounding wiki instead of RAG's retrieve-and-re-derive-every-time.
  Three layers — raw sources (immutable), the wiki (LLM-maintained, cross-linked
  pages), and a schema (conventions doc). Workflow: **ingest** (new source → LLM
  reads it → updates ~10-15 *existing* pages, not just creates new ones) → **query**
  (search the wiki index, not the raw sources) → **lint** (periodic health-check for
  orphans, contradictions, staleness). A `log.md` (append-only chronological) and
  `index.md` (catalog with one-line summaries) are the concrete artifacts.
- **Graphify** ([site](https://graphify.net/)): turns notes/code into a typed
  knowledge graph (nodes = entities, edges = typed relations like
  `IMPORTS`/`REFERENCES`) queryable three ways — Cypher, natural language, or as
  an **agent-callable tool** — the pitch being an AI agent queries the compressed
  graph instead of reading raw files, at a fraction of the token cost. For
  free-text corpora (not code) its other differentiator is **NER-based
  auto-entity-linking**: automatically extracting entities (people, places,
  concepts) and connecting any notes that mention the same one, without
  requiring an explicit link.
- **Open Knowledge Format (OKF)** — Google Cloud's open spec that **formalizes
  the LLM Wiki pattern into a portable, vendor-neutral interop format**: a
  directory of `.md` files with YAML frontmatter, meant to let wikis written by
  different producers be consumed by different AI agents without translation.
  ([Google Cloud announcement](https://cloud.google.com/blog/products/data-analytics/how-the-open-knowledge-format-can-improve-data-sharing/) ·
  [repo + spec](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md))

  **Re-checked 2026-10-07 — this section describes v0.2.** It was first written
  against v0.1 (published 2026-06-12). Two things have since moved:
  - **The spec is v0.2** (2026-07-24; timestamps tightened to ISO 8601 with an
    explicit offset on 2026-08-20). It is a minor bump with **two deliberate
    breaking changes**: `timestamp` → `generated.at`, and the body
    `# Citations` section → frontmatter `sources` plus markdown footnotes.
    v0.2 consumers MAY still read both legacy forms; producers should not
    emit them.
  - **The canonical home moved** to `GoogleCloudPlatform/open-knowledge-format`
    (2026-08-14). The `okf/` directory in `knowledge-catalog` — which the blog
    post and this doc originally linked — is a frozen snapshot whose own README
    says to stop using it.

  Concretely, as of v0.2: every concept file needs exactly one required
  frontmatter field, `type` (freeform string, not centrally registered);
  recommended fields are `title`, `description`, `resource` (a URI), `tags`.
  A concept's identity is its path without `.md`. Two filenames are reserved —
  `index.md` (directory listing; the bundle-root one may carry
  `okf_version: "0.2"`, the only frontmatter an index may have) and `log.md`
  (update history grouped under `## YYYY-MM-DD` headings) — everything else is
  a concept document. Concepts link to each other via plain markdown links
  (bundle-relative `/…` paths preferred); a link is an *untyped* directed edge
  — the relationship kind lives in the surrounding prose, not the link syntax.

  **v0.2's new layer is trust, provenance and lifecycle**, all optional,
  all in frontmatter:
  - `sources:` — a list of `{ id, resource, title, author, last_modified, … }`;
    a claim is attributed by a footnote whose label is a source `id`
    (`[^margin-standard]`).
  - `generated: { by, at }` — who produced the current content and when it
    last meaningfully changed.
  - `verified: [{ by, at }, …]` — confirmation events, kept separate from
    `generated` because who *wrote* a concept need not be who *confirmed* it.
    Consumers derive a **trust tier**: no `verified` ⇒ *unverified*; only
    non-human actors ⇒ *machine-confirmed*; any `human:` actor ⇒
    *human-reviewed*. Advisory, not access control.
  - **Actor convention** for every `by`: `<producer>/<version>` for agents and
    tools, `human:<id>` for people (producers MUST use it for hand-written or
    human-confirmed content), `process:<id>` for automation.
  - `status: draft | stable | deprecated` (absent ⇒ `stable`) and
    `stale_after: <instant>` — an **absolute** instant, not a relative TTL, so
    "is it stale" is a plain `now >= stale_after` with no reference to when it
    was read.
  - An `Attested Computation` concept type for sanctioned SQL plus
    deterministic verification — data-warehouse machinery with no Jodd
    counterpart.

  Conformance stays permissive: consumers MUST NOT reject a concept for a
  missing optional field, an unknown `type`, an unknown key or a broken link —
  and **MUST preserve unknown keys when round-tripping** (this matters for
  Import, #10). Google still frames OKF as early — worth tracking, not worth
  over-investing in exact compliance with any one version yet. v0.1 → v0.2
  took six weeks and broke two fields.

  **Reference tooling worth knowing** (in the canonical repo): `python -m
  reference_agent visualize <bundle>` renders any bundle as one self-contained
  interactive graph HTML file with no backend; four sample bundles, all
  already v0.2-shaped (`generated`, no `timestamp`) — `bundles/acme_retail`
  is the one that exercises the whole signal layer (`verified`, `status`,
  `stale_after`, `sources` + footnotes, a deprecated concept); `ga4`,
  `stackoverflow`, `crypto_bitcoin` are thinner BigQuery dumps. The rest — the
  BigQuery enrichment agent, Knowledge Catalog, the Dataplex `kcmd` demo — is
  Google Cloud data-catalog machinery and does not apply to Jodd.

The throughline for Jodd: **Extract is the "ingest" step**; the `edges` table is
already a mini Graphify-style graph; what's missing is the LLM Wiki "update existing
pages" behavior, Graphify's "query the graph, don't just store it" behavior (both
the agent-callable and NER-linking halves of it), and — newly — a standard,
interoperable way to get Jodd's derived knowledge (and external AI-curated
knowledge) in and out of the app.

**Where OKF conflicts with Jodd's own doctrine, and why that's fine:** OKF's core
mechanism is literal YAML frontmatter inside the markdown file. Jodd already ruled
this out for primary note storage (see CLAUDE.md's "Compatibility tiers" section) —
frontmatter would round-trip to Apple Notes as ugly visible text in the iPhone UI,
exactly the mistake the "standards" discussion earlier in this thread flagged and
avoided by keeping tags/metadata in SQLite instead of the body. **OKF is not a candidate
replacement for Gmail/LocalFS note storage.** It's an **import/export format** —
which Jodd is unusually well-positioned to support cheaply, because the SQLite
cache already derives almost everything OKF needs:

| OKF concept | Jodd's existing equivalent |
|---|---|
| `type` frontmatter field | Could derive from folder (`Notes/__Extracts__` → `type: Extract`) or be generic `Note` |
| `title` / `tags` | Already columns/derived tables (`notes.title`, `note_tags`) |
| `generated.at` (was `timestamp` in v0.1) | `last_local_modified_at` |
| `generated.by` | Agent-workspace lines already record it per line: `remember` writes `(date · client)` (`agent_memory::format_line`). Extract/ingest notes know their LLM provider. Everything else is `human:` |
| `verified` → trust tier | Curate's approved proposals are a human confirmation of AI-proposed content. Jodd has no per-note record of it yet |
| `status` / `stale_after` | Nothing per-note. The virtual "Stale" smart folder uses a fixed 30-day relative threshold, which is the design OKF deliberately rejected |
| `resource` (URI) | The note's UUID is already a stable, durable identifier |
| markdown links between concepts | The `edges` table (`mentions`) already tracks exactly this, derived from `[[wikilinks]]` |
| `sources` + footnotes (was `# Citations` in v0.1) | `cites` edges, and Extract's `<details>Source (verbatim)</details>` block, already hold the provenance. URL ingest knows its source URL |
| `index.md` | Exactly backlog item #6 below (OKF Export) — building it as real OKF gives interop for free |
| `log.md` | The agent workspace's log page (`agent_memory::log_line`, `## [date] remember \| …`) is already this shape; otherwise a chronological export log |

**The trust rows are the part worth taking even without any export.** The actor
convention and the three tiers could be stored in SQLite (never in the body)
and surfaced in `jodd-mcp` search results and the UI. An agent could then tell
a note a human wrote or confirmed from one another agent wrote and nobody
checked. The agent-knowledge-loop design
([spec](superpowers/specs/2026-10-06-agent-knowledge-loop-design.md) §3) names
exactly that gap as one no prior art solves. **Built as note provenance M1**
([spec](superpowers/specs/2026-10-07-note-provenance-design.md)) — see
"Shipped — Note provenance M1" below.

## What Jodd already had going in (pre-existing, unrelated to this thread)

- `edges` table — `mentions` (`[[wikilinks]]`), `child_of` (note→folder), `tagged`
  (note→#tag), derived on every write (`db.rs`).
- `[[slug-uuid8]]` wikilinks with autocomplete picker + rewrite-on-rename.
- Local graph view — per-note, radial, clickable (not global/multi-hop yet).
- FTS5 search (`search_notes`, trigram-tokenized, Thai-aware) with an
  account/folder/all-accounts scope selector.
- Extract ("Lessons") workflow — `LessonProvider` trait, HTTP (OpenAI-compatible)
  + Claude CLI subprocess implementations, output into `Notes/__Extracts__`
  (until 2026-09-15 — see Extract filing below).

## Shipped this session — Phase 1: Ingest entry point + manual append

- Spec: [`docs/superpowers/specs/2026-07-10-extract-ingest-entrypoint-design.md`](superpowers/specs/2026-07-10-extract-ingest-entrypoint-design.md)
- Plan: [`docs/superpowers/plans/2026-07-10-extract-ingest-entrypoint.md`](superpowers/plans/2026-07-10-extract-ingest-entrypoint.md)
- Merged to `main`: `08c94b1` (8 commits — see plan for the task-by-task breakdown)
- Follow-up merged: `1b6c764` — `HttpProvider` `response_format` compatibility fix
  (some OpenAI-compatible gateways, e.g. Kilo Code, reject it; now retries once
  without it on a 400 naming that param)

**What it does:** relocated the Extract trigger from a cramped account-management
dropdown to a persistent "💡 Ingest source" Sidebar row; added a destination toggle
to the Extract modal — **New note** (unchanged behavior) or **Append to existing
note** (manual search picker, any note in the account, pure-concatenation write via
the new `append_to_note_body` + `append_extract_lessons`).

**Explicitly out of scope for Phase 1** (see the spec's own "Deferred" section) —
this is the exact backlog for what follows:

## Shipped — Note provenance M1 (2026-10-07)

The OKF "trust rows" (who generated a note, whether a human verified it) were
not designed before; they are now built as local `note_events` plus a derived
tier, shown on the chip, in the Unreviewed smart folder, and returned by
`jodd-mcp` as `trust`. Spec:
[`docs/superpowers/specs/2026-10-07-note-provenance-design.md`](superpowers/specs/2026-10-07-note-provenance-design.md)
(§10 "As built"). Lifecycle (`status`, `stale_after`) and the OKF export itself
remain M2 / unbuilt.

- Branch: `claude/note-provenance-spec` — live pass pending (spec §10)

## Shipped — Extract filing (2026-09-15)

- Spec: [`docs/superpowers/specs/2026-09-15-extract-filing-design.md`](superpowers/specs/2026-09-15-extract-filing-design.md)
- Plan: [`docs/superpowers/plans/2026-09-15-extract-filing.md`](superpowers/plans/2026-09-15-extract-filing.md)
- Branch: `claude/extract-filing` — live pass pending (see the spec's Testing section)

**What it does:** an extract's provenance (the Source block) no longer decides
its location. New extracts land in an ordinary `Notes/Inbox` — the root on
Outlook, which can now use Extract at all. A separate `suggest_folder` LLM
call proposes one EXISTING folder (Rust rejects anything it did not offer),
shown as a Move / Keep here chip in the editor breadcrumb and also available
as "Suggest folder" on any note. A proposal follows the note if a backend
rekeys its uuid mid-call (gotcha #16). `__Extracts__` is a virtual "Extracts"
view derived from the Source block, and the old folder can be renamed or
deleted; `__Claude__` stays protected for `jodd-mcp`.

**Next:** Spec B — URL ingest — reuses `resolve_destination` and the
suggestion flow unchanged.

## Next — prioritized backlog (ordered by effort × impact)

Each item traces back to a specific gap identified against LLM Wiki/Graphify/OKF.
None of these are designed yet — brainstorm each before building. **Ordering
rule: fastest genuinely-useful win first**, not grouped by theme — each item is
tagged `Effort` / `Impact` so the ranking is auditable, not just asserted, and
carries an explicit user-benefit statement rather than just an architectural one.
See "Suggested starting point" below for the condensed version.

1. **Structured citations** *(Effort: low · Impact: high)*
   Right now Extract's source block is one opaque `<details><pre>` dump — if the
   pasted source had a URL in it, it's buried, unclickable, and invisible to any
   query. Model it as a new `cites` edge type in the existing `edges` table
   (`rel='cites'`, `dst_id` = the URL) rather than a separate table — keeps
   citations queryable through the same graph-traversal code path as
   `mentions`/`child_of`/`tagged` instead of forking the schema.
   **Caveat on that choice:** `dst_id` today points at another note's uuid8 for
   `mentions`; a `cites` edge's `dst_id` is an external URL instead, so the
   traversal/rendering code needs to branch on `rel` to know which kind of
   target it's looking at — a small, worth-noting wrinkle, not a blocker.
   **Scope: every note, not just Extract output.** `mentions` edges and tags
   already get re-derived on *every* note write (`apply_local_edit` /
   `insert_local_new` → `reconcile_edges_from_body_conn` /
   `reconcile_tags_from_body_conn`), regardless of whether the note came from
   Extract, manual typing, or an Apple Notes/Gmail sync — that's the general
   "derive from body on every write" doctrine, not an Extract-specific thing.
   Citations should follow the same pattern: scan any note's `body_html` for
   URLs in that same reconciliation pass, not just Extract's source text during
   `assemble_note_body`/`append_to_note_body`. Concretely, paste a URL into a
   plain hand-written note (no Extract involved) and it still shows up in the
   Sources list and the dedup check below — same mechanism, same payoff,
   everywhere, for no extra engineering cost since the reconciliation pass
   already runs on every write regardless of note origin. **User-visible
   payoffs:** (a) a real clickable "📎 Sources" list in the editor, mirroring the
   existing Connections panel already built for `[[wikilinks]]` backlinks — no
   more scrolling a wall of raw pasted text hoping to spot a link; (b)
   exact-URL-match "you already have a note citing this" detection whenever the
   same URL shows up in a second note (most useful for Extract's re-paste case,
   but not limited to it) — a much cheaper first cut of item #5's dedup problem
   than semantic similarity, no LLM needed; (c) filter/search notes by source
   domain, the same way tag filtering works today, across the whole account, not
   just Extracts. Also happens to be exactly the structured data OKF Export (#6)
   needs for its provenance (`sources` frontmatter since OKF v0.2; a
   `# Citations` section in v0.1), so it's not wasted if that ships later
   either. **Tradeoff:** depends on reliably finding URLs in messy note bodies,
   and plenty of notes (a debugging session, a meeting transcript, most
   day-to-day writing) have none at all — needs a graceful "no citations found,
   and that's fine" fallback, not a requirement that every note has one.

2. **Orphan/staleness lint** *(Effort: low · Impact: medium-high)*
   LLM Wiki's "Lint" pass, pure SQL: notes with zero backlinks (orphans, findable
   via the existing `edges` table) or untouched for N days
   (`last_local_modified_at`). Natural fit for the roadmap's existing "Smart /
   saved folders" item (CLAUDE.md roadmap #2) — build as a Smart Folder
   ("Orphaned", "Stale") rather than a standalone feature. **User-visible
   payoff:** a "spring cleaning" view — open one Smart Folder and see every note
   that's disconnected or forgotten, instead of never noticing they exist.
   **Locked (brainstorm 2026-07-13):**
   - Two fixed folders, "Orphaned" + "Stale (30d+)." Hardcoded 30-day threshold,
     no config UI — matches this item's own "Effort: low" framing; add a
     settings knob later only if 30 days ever proves wrong for real use.
   - Per-account, not cross-account. The underlying data is account-scoped
     either way (`edges`/backlinks never cross accounts — UUIDs are namespaced
     by `account_id`), so this is a presentation choice, not a query one.
     Per-account reuses existing folder-row rendering; a combined cross-account
     view (like Tags' scope selector) is a bigger, separate UI investment,
     worth revisiting only if per-account proves tedious with several accounts.
   - **Fully virtual — no `folders`-table rows.** `db.rs` already reserves
     `folders.kind='smart_query'` for this, which made "insert real rows with
     that kind" tempting, but real rows in the same table as syncable folders
     risk getting caught by `reconcile_folders_from_labels`' prune pass (no
     Gmail label backs them), the sync worker's dirty-folder push loop, and the
     folder context menu's rename/move/delete actions — the exact class of bug
     this codebase's CLAUDE.md documents repeatedly (defects D1/D8/D11). Ship
     as new `list_orphaned_notes`/`list_stale_notes` commands + a
     frontend-only `selectedSmartFolder` concept instead, entirely decoupled
     from folder sync semantics.

3. **A single `type`/`kind` per note, distinct from tags** *(Effort: low · Impact: low-medium)* — ❌
   **DROPPED (by 2026-07-20) for lack of a consumer — never built.** The narrowed
   scope below was locked and specced
   ([spec](superpowers/specs/2026-07-15-citations-smartfolders-notetype-design.md),
   decisions 9–10), then dropped before Task 6 of its plan was implemented: the
   [auto-link spec](superpowers/specs/2026-07-20-auto-link-ingest-design.md)'s
   decision 7 records note `type` as "considered and dropped earlier this session
   for lack of a consumer" and declines entity/concept typing for the same reason.
   No commit has ever added `notes.note_type` or `ExtractEnvelope.source_type`.
   #1 and #2 from the same bundle shipped. A `#meeting`-style tag remains the de
   facto type; reopen only with a concrete consumer (a type-filtered Smart Folder,
   or OKF Export's `type` field). The text below is the original proposal, kept
   as history.
   OKF's `type` field is for coarse *routing* — "this is a Meeting," "this is a
   Person" — one canonical value per concept, unlike Jodd's multi-valued
   freeform `#hashtag` tags. Jodd already classifies *folders* this way (the
   `__name__` → `system_workflow` `kind` convention) but has nothing equivalent
   for user note content. **Honest caveat:** a `#meeting` tag already serves as
   a de facto type for most practical purposes today, so this solves a real but
   modest gap, not an acute pain point — worth building only once there's a
   concrete consumer that needs a *single* canonical value rather than "does
   this note happen to carry the right tag" (a type-filtered Smart Folder, or
   feeding OKF Export's `type` frontmatter field cleanly). **User-visible
   payoff (once that consumer exists):** a reliable "show me only Meetings" /
   "only People" filter that doesn't depend on remembering to tag consistently.
   Cheap enough (schema-trivial — a column, or even just a reserved tag prefix
   convention like `#type:meeting`) that it's fine to bundle in alongside #1 or
   #2 rather than schedule separately.
   **Locked (brainstorm 2026-07-13) — narrowed scope, no longer deferred:**
   `lessons/prompt.rs:33-35` (now `llm/prompt.rs:33-36`) already tells the LLM to classify the source
   ("Debugging session → root causes+fixes...", "Meeting transcript →
   decisions+action items...") purely to shape `lessons_markdown` — that
   classification is thrown away today. Add one field to `ExtractEnvelope`
   (`source_type: Option<String>`) and a one-line prompt addition to keep it,
   auto-applied at ingest with a small UI affordance to correct it if wrong —
   marginal cost, not a new subsystem. This resolves the "no concrete consumer
   yet" objection differently than expected: the value isn't a new consumer,
   it's that population is nearly free, riding on a classification the LLM
   already makes. **Real caveat this doesn't remove:** auto-derivation only
   applies to Extract-originated notes (there's an LLM in that path making the
   call already) — manually-typed or synced notes stay untyped unless assigned
   by hand. Scope is "Extract gets free auto-populated types," not "solve type
   for every note in the account." Build alongside #1/#2/#4 now, not deferred.

4. **Agent-callable graph exposure (MCP server)** *(Effort: low-medium · Impact: high — for how you actually work)*
   Graphify's third query mode: expose the graph as tool calls for an external
   AI agent, not just a human typing Cypher or Jodd's own internal query UI. A
   thin read-only MCP server wrapping what already exists — `search_notes`
   (FTS5), backlinks/edge-traversal, folder listing — would let any Claude
   session (this one included) query "what does the user already know about X"
   directly against Jodd's SQLite cache. **User-visible payoff:** you stop
   re-explaining context you've already captured in Jodd — any Claude Code
   session, in any project, can pull it in on demand instead of you copy-pasting
   old notes into a new chat every time. **Distinct from #7** ("Ask your
   notes"): this serves *other agents* querying Jodd from the outside; #7 serves
   Jodd's own user, inside the app — different consumer, not a duplicate. Mostly
   plumbing (an MCP SDK/stdio server + a handful of read-only command wrappers
   around queries that already exist) — closer to #1-#3's effort tier than to
   #7's.
   **Locked (brainstorm 2026-07-13):**
   - **New Cargo workspace member** (e.g. `jodd-mcp/`, own `Cargo.toml`,
     depends on the existing `jodd_lib` rlib as a path dependency), not
     `src-tauri/examples/`. Structurally outside Tauri's bundler scan, so it
     can't trigger CLAUDE.md's defect #5 (extra binaries breaking the macOS
     bundle) — safe by construction, not just by convention. Runs via
     `cargo run -p jodd-mcp` or a built release binary.
   - **v1 tool set: `search_notes` + backlinks/outgoing_links only.** These two
     are the actual core of the item — search finds relevant notes, connections
     pull in the graph context around them, which is the "query the graph, not
     just keyword match" value this item exists for. Deferred: `list_folders`
     (less essential — the core use case is search-driven, not
     structure-browsing) and citations lookup (coupled to #1 shipping first;
     ships empty until then). Both are zero-risk additions later — just more
     thin wrappers, not new architecture.
   - **DB path resolution:** Jodd doesn't actually use Tauri's `app_data_dir()`
     API — `lib.rs:3949-3951` uses the plain `dirs` crate
     (`dirs::data_dir().join("jodd")`), which has nothing to do with a running
     Tauri app and is trivially callable from a standalone binary. `jodd-mcp`
     reuses that exact same call as its default, with `--db-path`/
     `JODD_DB_PATH` as an override for edge cases (alternate profiles, testing
     against a DB copy) — not an either/or between "auto-detect" and "explicit
     config" as originally framed, since auto-detect carries no duplication
     risk here.

5. **Auto-link / auto-suggest** *(Effort: high · Impact: high)*
   LLM Wiki's "ingest updates existing pages," done automatically instead of the
   user manually searching. When ingesting, search existing notes
   (`search_notes` — already FTS5-backed, already used by Phase 1's picker) for
   related content, then either auto-insert `[[wikilinks]]` into the new content
   or suggest an append target instead of requiring a manual pick. **User-visible
   payoff:** ingest stops producing disconnected islands by default — related
   notes actually end up linked without you doing the searching. **If #1 ships
   first, this gets partially de-risked:** exact-URL-match dedup (the easy case)
   is already solved by the citations edge, leaving only the harder "no URL,
   judge relatedness from content" case for this item's design work. **Open
   design question worth a dedicated brainstorm:** how is "related enough"
   decided for that remaining case — LLM-judged relevance, a similarity/keyword
   threshold, or a hybrid (search narrows candidates, LLM picks among them)?
   Hooks into `llm/prompt.rs` (was `lessons/prompt.rs`) or a new
   pre/post-processing pass around `append_extract_note` (was
   `append_extract_lessons`, renamed in `75e03e8`).

6. **OKF Export** *(Effort: medium-high · Impact: medium — real but speculative for a single-user app)*
   Export a folder (originally `Notes/__Extracts__`; since 2026-09-15 that is a
   virtual view derived from the Source block, so "export a folder or a view")
   as an OKF v0.2 bundle on disk: one `.md` per note with frontmatter derived
   from existing SQLite data (`type` from the folder — item #3 was dropped, so there is no per-note type — `title`/`tags`
   already columns, `generated.at` = `last_local_modified_at`,
   `generated.by` = the agent's client name or `human:<id>`, `resource` = the
   note's UUID), `mentions` edges rewritten as bundle-relative markdown links,
   `cites` edges as a frontmatter `sources` list plus footnotes (item #1 makes
   this a data-copy instead of a from-scratch parse), an auto-generated
   `index.md` with `okf_version: "0.2"` at the root, and the agent workspace's
   log page as `log.md`. **Do not emit the v0.1 `timestamp` or
   `# Citations` forms.** A note whose filename would be `index.md` or
   `log.md` needs renaming — both are reserved.

   **Two pieces now exist that this item originally assumed it had to
   build** (both arrived with the SSH backend, `docs/BACKEND-SSH.md`):
   - **HTML→Markdown.** `backend/ssh/mdconv.rs::html_to_md` returns `Some` only
     when the round trip is exact and `None` otherwise (the SSH backend then
     writes `.html`). An exporter needs its own answer for `None`: OKF concepts
     are `.md` only.
   - **Markdown files with frontmatter, unknown keys preserved.** An SSH vault
     is already `.md` files with frontmatter (`uuid`, `title`, `created`,
     `pinned`), and `frontmatter.rs` keeps every unrecognized line verbatim in
     `extra`. Missing for OKF conformance: a `type` key. A different naming:
     `created` is not `generated.at`. **Caveat:** `frontmatter.rs` is
     deliberately not a YAML parser. It *preserves* OKF's nested values
     (`generated: { by, at }`, `sources:` lists) but cannot *read* them.

   **A ready external consumer now exists:** the reference repo's
   `visualize` command renders any bundle as one self-contained graph HTML.
   That gives export a test against someone else's reader, not only Jodd's own,
   and a throwaway global graph view (item #9) for free. **User-visible
   payoff:** any OKF-aware external agent or tool can read Jodd's Extracts
   directly, without you manually re-exporting/re-explaining them. **Re-ranked
   down from earlier discussion:** that payoff is real, but for a solo-use app
   today there's no concrete consumer waiting on it yet. Items #1-#4 pay off
   immediately in the tools you actually use daily; this pays off only once
   something external wants to read Jodd's data. (Re-checked 2026-10-07: still
   true. The visualizer is a viewer, not a consumer anything depends on.)

7. **"Ask your notes" — graph-grounded retrieval, plus NER auto-linking** — ✅
   **SHIPPED (2026-07-30) as "Ask Jodd", the query-time-retrieval half; the
   NER auto-entity-linking extension below is unbuilt and remains open.** See
   [spec](superpowers/specs/2026-07-29-ask-jodd-design.md) +
   [plan](superpowers/plans/2026-07-30-ask-jodd.md), and CLAUDE.md's Current
   status / edges #4 and #8.
   The shipped design diverged from this item's original framing in one
   important way, discovered by measuring the live vault rather than assumed:
   graph expansion (the "compressed graph" half of the pitch) turned out to
   be a dead end — the wikilink graph is effectively empty (7 `mentions`
   edges over 190 notes on the smaller account) — so retrieval is a four-stage
   SQL-pre-filter → LLM-select → LLM-answer pipeline instead, with no `edges`
   traversal in the loop. Two limitations are now known from real data rather
   than assumed:
   - **The SQL pre-filter is the recall ceiling of the design.** On the
     6,655-note flat test account, a conceptual question
     whose wording matches no note and whose target isn't recent can be
     missed. The UI reports "N in scope → N considered → N read" so a thin
     pool is visible instead of inferred from a disappointing answer.
   - **Embeddings are the named successor, not dismissed — blocked
     structurally, not by effort.** Agent-CLI providers (`claude -p`,
     `codex`, …) expose no embedding endpoint, so an embedding index would
     work only for HTTP providers and would split the feature's behavior by
     provider type. Revisit once either a provider gap closes or a local
     embedding model is acceptable as a dependency.

   Original framing, retained for context:
   Graphify's core pitch: query the compressed graph (edges + FTS5) as context
   for an LLM call, instead of Extract's current model (which only ever sees
   freshly-pasted text). This is a new workflow, not a modification of Extract —
   fits alongside the roadmap's "Additional workflows" item (CLAUDE.md roadmap
   #3: Summarize, Extract Action Items) as a sibling. **User-visible payoff:**
   ask Jodd a question in plain language and get an answer grounded in your own
   notes, instead of manually searching and reading through them yourself. A
   fuller version could extend beyond query-time retrieval into **NER-based
   auto-entity-linking** — extracting entities (people, concepts, places) across
   all notes in the background and connecting any that share one, even without
   an explicit `[[wikilink]]` — Graphify's other big idea for free-text corpora.
   **User-visible payoff of that extension:** two notes that both mention "Dolt"
   or a specific person end up connected automatically, instead of staying
   islands unless you remembered to link them yourself. That's a distinct,
   ongoing background process (not just per-query retrieval) with its own
   precision/false-positive design question — scope it as a follow-on once the
   retrieval half is proven, not as a prerequisite. Needs a full brainstorm from
   scratch; nothing here is decided yet — high potential impact but the least
   de-risked item on this list.

8. **Schema doc for create-vs-edit** *(Effort: high · Impact: medium — refinement, not new capability)*
   The "smart" half of #5: give the LLM enough context during ingest to actively
   recommend "this should go into existing note X" rather than the user deciding.
   Ship #5 first with dumb keyword-search suggestions, then layer this on as a
   refinement. **User-visible payoff:** ingest starts making the create-vs-append
   call for you in the obvious cases, instead of always asking. Needs prompt
   engineering + real evaluation against sample ingests, not just a spec —
   sequenced strictly after #5, not before.

9. **Global / multi-hop graph view** *(Effort: medium · Impact: low-medium)*
   Already an existing roadmap item (CLAUDE.md roadmap #4) that predates this
   LLM Wiki thread — but it's exactly Graphify's "global graph, not just
   per-note local view" ask. Same item, two motivations now pointing at it.
   **User-visible payoff:** see how your whole knowledge base connects at a
   glance, not just one note's immediate neighbors. Lower urgency: the existing
   per-note local graph already covers the day-to-day "what links to this" need;
   the global view is a nicer-to-have visualization on top, not a missing core
   capability. Not started.

10. **OKF Import** *(Effort: medium · Impact: low — no demand signal yet)*
    The inverse of #6: let a user point Jodd at an existing OKF bundle (e.g. one
    produced by another AI agent or tool) and pull it in as notes — parse
    frontmatter into title/tags, convert body Markdown to HTML via the existing
    `pulldown-cmark` pipeline (`llm/markdown.rs`, or `ssh/mdconv.rs::md_to_html`,
    which reads any text as something), follow bundle-relative links to seed
    `mentions` edges, treat `sources` as `cites` edges / the source block.
    Accept the legacy v0.1 forms too (`timestamp`, a body `# Citations` list),
    as v0.2 permits — anything produced between 2026-06-12 and 2026-07-24
    carries them, even though the reference sample bundles no longer do.
    Architecturally closest to `LocalFsVertical`
    (`src-tauri/src/backend/localfs/`) — a "reads files off disk" precedent
    already exists — but this is an import/one-shot action, not a new sync
    `Vertical`. The SSH backend is the closer precedent now: it already reads
    `.md` + frontmatter files and adopts ones that arrive without Jodd's keys.
    Whether a file with someone else's frontmatter but no `uuid` gets adopted
    like a frontmatter-less one is **not checked**. Read `frontmatter::split`
    before assuming it.

    **The spec constraint this item must design for:** consumers MUST preserve
    unknown frontmatter keys when round-tripping. Jodd cannot put them in a
    note body (Apple round trip), so an import that may ever be re-exported
    needs somewhere in SQLite to hold them. Two precedents already solve this
    the same way: the SSH backend's verbatim `extra`, and Google's own
    Knowledge Catalog bridge (`toolbox/mdcode/demo`), which stores everything
    its template does not model as a JSON list of `[path, value]` pairs. A
    one-way import that never re-exports can skip this, but should say so.
    Fixtures: the reference repo's `bundles/acme_retail` exercises every v0.2
    field. **User-visible payoff:** knowledge another AI agent curated
    outside Jodd becomes searchable/linkable alongside everything else, instead
    of living in a separate folder you never open. Do this only if OKF Export
    (#6) ships and someone actually hands Jodd an OKF bundle to import — last on
    the list because both its prerequisite and its demand are hypothetical right
    now.

## Suggested starting point

**#1, #2, #3, and #4 first — bundle them, they're all cheap and independent.**
None need an LLM, none need a design brainstorm beyond "confirm the exact
schema," and together they cover the biggest low-effort wins (clickable sources +
dedup-by-URL, hygiene lint, agent-callable queries against your own knowledge)
plus one cheap-to-tack-on convenience (note type — later dropped, see #3). #4 (MCP exposure) is worth
calling out specifically as a companion pick alongside #1-#3 — similarly
low-effort, and arguably the highest-value-for-you item on the whole list given
how much of your actual workflow already runs through Claude Code sessions like
this one. Realistic as a short work session, maybe two.

**#5 (Auto-link) next** — the natural high-impact follow-up, now smaller in
scope than it would have been because #1 already solves the exact-URL-match half
of dedup. Still needs its own brainstorm for the "no URL, judge by content" case.

**#6 (OKF Export) is worth keeping in view but isn't a "do next"** — it's real,
externally-facing value, but nothing today is waiting to consume it. Revisit
once #1-#5 are shipped and either (a) there's an actual reason to hand Jodd data
to another tool, or (b) it's just the next item left standing.

## How to resume

- `superpowers:brainstorming` on whichever item you pick, pointing it at this doc
  plus the Phase 1 spec/plan for house style and precedent.
- Key files to reference: `src-tauri/src/db.rs` (`search_notes`, `edges` table,
  smart-folder-adjacent queries would live here), `src-tauri/src/llm/` module (was `lessons/`),
  `src/lib/components/LessonExtractModal.svelte`, `src/lib/components/Sidebar.svelte`.
- Update this doc as items ship — move them into a "Shipped" section with commit
  refs, the way Phase 1 is recorded above.
