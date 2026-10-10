# Ask Jodd — in-app RAG chat over the user's own note graph

> **Status:** design approved 2026-07-29, ready for implementation planning
> **Origin:** [HANDOFF-2026-07-29-tier1-copilot.md](../HANDOFF-2026-07-29-tier1-copilot.md) Feature 1
> **Scope:** Feature 1 only. Quick-capture (Feature 2) is a separate spec.

## 1. Problem

`jodd-mcp` exposes `search_notes` + `note_connections` to *Claude Code* — i.e. to
developers, from a terminal. The note owner has no equivalent inside the app.
Jodd can store, link, tag and extract, but it cannot answer a question about
what the owner already wrote. That is the gap between "note app" and "copilot".

Ask Jodd is a multi-turn, ephemeral chat over the local SQLite cache that
answers questions in natural language and cites the notes it used.

## 2. Findings that shaped the design

Measured against the live vault (`~/Library/Application
Support/jodd/jodd.sqlite3`), not assumed. **The measurement was taken twice:**
first on 2026-07-29 against two small accounts, then again on 2026-07-30 after
a third, far larger account was added. The second measurement falsified the
first design — that history is kept here deliberately, because it is the
evidence for why the pipeline is shaped the way it is.

| Measure | small Gmail account | **large Gmail account** | local test folder |
|---|---|---|---|
| Notes | 185 | **6,655** | 5 (fixture) |
| Total `body_html` | 305 KB | **18 MB** | 16 KB |
| Avg / max note | 1.7 KB / 40 KB | 2.7 KB / **1,037,880 chars** | 3.3 KB / 6 KB |
| Notes > 100 KB | 0 | **18** (4.5 MB combined) | 0 |
| Notes < 1 KB | — | 4,397 | — |
| Distinct labels | ~30 | 16, but **6,623 of 6,655 in one flat `Notes`** | 2 |

Vault-wide: `edges` by rel — `child_of` 190, `cites` 371, `tagged` 65,
**`mentions` 7**; 54 distinct tags.

Five consequences:

**F1 — The corpus does *not* fit in a context window, and a catalog of it does
not either.** A catalog line (`uuid8 · title · label · tags · date`) runs ~75
chars at this account's real title length (avg 43). 6,655 lines ≈ 475 KB ≈
**130–160k tokens for the catalog alone**, before any note body. The original
design made a full catalog the primary mechanism and an FTS pre-filter the
fallback at 2,000 notes; the live vault is 6,841 notes across accounts, so that
"fallback" is the only viable path from day one. **The pre-filter is therefore
primary (§5.1), and the catalog describes candidates only.**

**F1b — The size distribution defeats any average-based budget.** 4,397 notes
under 1 KB coexist with 18 notes holding 4.5 MB, one of them 1 MB alone — 380×
the mean. A per-selection character cap is insufficient; a **per-note**
truncation cap is mandatory (§5.4).

**F1c — Structure cannot do the filtering.** 99.5% of the large account is a
single flat `Notes` label. Folder scope is real and useful on `bbmedia`'s tree
(§5.6) and filters essentially nothing here. Reduction must come from content.

**F2 — The wikilink graph is effectively empty: 7 `mentions` edges over 190
notes.** The handoff proposes "expand 1 hop via edges" for context. On real
data that expansion returns almost nothing. `cites` is dense (371) but points
at URLs, not notes, so it cannot pull note context in either. **Graph
expansion is explicitly out of scope for v1** — it would be building for a
graph that does not exist.

**F3 — `autolink::extract_keywords` is unusable for question-shaped input.**
Verified empirically (throwaway test, run and reverted):

```
"what did I decide about sync conflicts?"            -> []
"ผมสรุปอะไรไว้เกี่ยวกับ ATLAS บ้าง"                    -> ["atlas"]
"summarize what I learned about agent CLI providers" -> ["cli"]
"do I have anything on Tahoe bundle signing"         -> ["tahoe"]
```

The filter keeps a token only if it is capitalized somewhere or repeats twice
([autolink.rs:44](../../../src-tauri/src/llm/autolink.rs)). Note bodies are long
and repetitive; questions are neither. Thai fails both branches structurally —
no capitalization, no spaces — which matters given Thai folders in the vault.

**F4 — `notes.date` is an RFC822 string and sorts lexically by weekday name.**
Ordering by it returns:

```
Wed, 9 Sep 2020 …
Wed, 9 Oct 2019 …
Wed, 9 Oct 2013 …
```

`ORDER BY n.date DESC` appeared in `search_notes`'s LIKE fallback, `backlinks`
and `outgoing_links` — a pre-existing latent bug, invisible at 185 notes and
badly wrong at 6,655. `last_remote_modified_at` is a proper epoch-ms column
(1321502623000 → 1783914496000). **Any recency ranking in this feature uses
`last_remote_modified_at`, never `date`.**

> **RESOLVED 2026-07-30 (commit `1ee9b7c`), separately from this feature.** The
> sweep found **five** sites, not the three noted above — `list_orphaned_notes`
> and `search_titles` were also ordering by `date`. All five now share a single
> documented `Db::NEWEST_FIRST` const:
> `COALESCE(n.last_remote_modified_at, n.last_local_modified_at) DESC`. The
> `COALESCE` exists because `last_remote_modified_at` is NULL for a note created
> locally and not yet pushed (`insert_local_new`, e.g. the Extract workflow), so
> the fallback keeps a brand-new note at the top of a list rather than the
> bottom; it was non-NULL for all 6,846 rows across all three live accounts and
> both verticals at the time of the fix. `x_mail_created_date` is deliberately
> NOT in the `COALESCE` — it is another RFC822 TEXT column and would reintroduce
> the same class of bug.
>
> This does not change the design above. The directive to rank by
> `last_remote_modified_at` (§5.1, and its unit test at the end of this doc)
> stands on its own merits and is now simply consistent with the rest of `db.rs`
> instead of working around it. Two things worth carrying forward: the frontend
> was never affected (`NoteList` sorts via `Date.parse`, which handles RFC822
> correctly — verified, not assumed), and the sites that actually misbehaved for
> users were the two carrying a `LIMIT`, where a bad `ORDER BY` silently changes
> *which rows come back* rather than just their order. Ask Jodd's stage-1
> pre-filter is `LIMIT`-shaped in exactly that way, which is why F4 was worth
> chasing down before building on it.

## 3. Non-goals

- No persistence of conversations. Answers are ephemeral; nothing is written to
  `notes`, no sidecar, no new table. (Handoff doctrine.)
- No graph expansion via `edges` in v1 (F2).
- No agentic tool-loop (model calling `search_notes` itself). Requires
  function-calling in `HttpProvider` and a loop in `AgentCliProvider`, both of
  which are one-shot today. Recorded as the eventual successor to §5.
- No refactor of `extract` / `suggest_links` onto the new chat primitive.
  They work; this feature does not justify the risk.
- No embeddings / vector store **in v1 — but named as the known successor, not
  dismissed.** At 6,655 mostly-unfoldered notes (F1, F1c), the §5.1 pre-filter
  is the recall ceiling of this design, and embeddings are the honest fix. The
  blocker is structural rather than one of effort: agent-CLI providers
  (`claude -p`, `codex`, …) expose no embedding endpoint, so an embedding index
  would work only for HTTP providers and would split the feature's behavior by
  provider type. Revisit when either a provider gap closes or a local
  embedding model is acceptable as a dependency.

## 4. Configuration: app-level LLM provider with per-account cascade

### 4.1 Why this is in scope

Provider resolution is shared by Extract and auto-link
([resolve.rs:15](../../../src-tauri/src/llm/resolve.rs)). Ask Jodd is
cross-account, so no single account's provider is the right owner. And the live
`accounts.json` shows the real Gmail account with `llm.provider: "none"` —
Extract cannot run there today either. Making provider config inheritable is the
prerequisite, and it fixes an existing papercut.

### 4.2 Resolution table

Effective provider for (account, workflow):

| App-level | `apply_to_accounts` | Account setting | Result |
|---|---|---|---|
| configured | ON | *unset* (inherit) | **app provider** |
| configured | ON | configured | **account wins** |
| configured | ON | `disabled` | **no provider** (workflow refuses) |
| configured | OFF | *unset* | no provider |
| configured | OFF | configured | account's own |
| not configured | — | configured | account's own (today's behavior) |
| not configured | — | *unset* | no provider (today's behavior) |

**Ask Jodd always uses the app-level provider**, independent of
`apply_to_accounts`. That toggle governs only whether *per-account* workflows
(Extract, auto-link) adopt the app default.

### 4.3 Changes

- **`LlmProviderKind`** gains a `Disabled` variant. `None` (the serde default)
  changes meaning from "unconfigured" to **"inherit"**. Every field in
  `LlmConfig` is already `#[serde(default)]`, so existing `accounts.json` files
  parse unchanged and every current account becomes an inheritor — the intended
  behavior.
- **New `src-tauri/src/app_llm_config.rs`**, mirroring `oauth_config.rs`
  exactly: a JSON file in the Tauri config dir for non-secret fields, API key in
  the OS keychain under service `jodd`, key `llm_api_key::__app__`. Holds an
  `LlmConfig` plus `apply_to_accounts: bool`.
- **`resolve.rs`** grows two entry points, both returning `Box<dyn LlmProvider>`:
  - `resolve_app_provider()` — Ask Jodd's, from app config alone.
  - `resolve_provider_for_account(account)` — implements §4.2 and replaces the
    current `resolve_provider`; all existing call sites move to it.
- **UI**: an LLM Provider section in `AppSettings.svelte` (reusing the existing
  `LlmProviderSettings.svelte` form) with the `apply_to_accounts` toggle.
  `AccountSettings.svelte` relabels the empty state from "None" to **"Use app
  default"** and adds an explicit "Disabled" choice. The relabel is required:
  "None" reads as "off" and would be actively misleading once inheritance
  exists.

## 5. Retrieval: pre-filter → candidate catalog → select → answer

Per turn, four stages. Stages 1–2 are pure SQLite; stages 3–4 are the two LLM
calls.

```
scope ──▶ 5.1 pre-filter (SQL)   6,841 notes ──▶ ≤400 candidates
       ──▶ 5.2 catalog (SQL)     ≤400 lines ≈ 12k tokens
       ──▶ 5.3 select  (LLM 1)   ──▶ ≤12 uuid8s
       ──▶ 5.4 answer  (LLM 2)   ──▶ markdown + citations
```

### 5.1 Stage 1 — SQL pre-filter (the primary reduction)

This stage, not the catalog, is what makes the feature tractable (F1). It
builds a candidate pool as the **union of three cheap SQL sources**, deduped by
uuid, capped at `CANDIDATE_POOL_MAX = 400`.

**`CANDIDATE_POOL_MAX` is a safety net, not the number that governs pool size
in practice.** Measured against the real vault, the cap never actually binds:
max observed pool size was 320 candidates before the Thai FTS fix (§5.5) and
169 after, against catalogs of 3.4k–10.4k tokens versus the ~12k token budget.
In practice `RECENCY_K = 150` plus however many terms the question's FTS hits
return is what determines pool size — `CANDIDATE_POOL_MAX` is the constant
that keeps the worst case bounded: 8 query terms × `search_notes`'s own
`LIMIT 200` = 1,600 possible FTS rows, plus an unbounded structural subtree,
plus 150 recency, could otherwise blow the catalog past its budget on a
pathological query or a large folder. Keep the constant for that reason; just
don't read it as the typical pool size.

1. **FTS hits** — `ask::extract_query_terms` (§5.5) over the conversation's
   latest question, each term through `Db::search_notes`.
2. **Recency** — top `RECENCY_K = 150` by **`last_remote_modified_at`**, the
   epoch-ms column. Never `notes.date`, which sorts by weekday name (F4).
3. **Structural scope** — every note in the selected folder subtree (§5.6) when
   folder scope is active; skipped for account/all-accounts scope, where it
   would be the whole pool.

Filling order when the cap binds: FTS hits, then structural, then recency.
FTS hits are the only source with evidence of relevance to *this* question;
recency is a prior, and it yields first.

**This is the recall ceiling of the design, and it is a real limitation.** On
the 6,655-note flat account, a conceptual question in Thai whose wording
matches no note and whose target is not recent can miss. The mitigation is
honesty, not silence: the UI reports how many notes were considered out of how
many were in scope (§8), so a narrow pool is visible rather than inferred from
a disappointing answer. Embeddings are the real fix and are named in §3.

### 5.2 Stage 2 — build the candidate catalog

One compact line per candidate:

```
<uuid8> · <title> · <folder label> · <#tags> · <YYYY-MM-DD>
```

~75 chars at the vault's real average title length (43 chars), so **≤400
candidates ≈ 30 KB ≈ 12k tokens**. Bodies are not included. Because the pool is
capped upstream, the catalog has a hard size bound regardless of vault size —
which is the property the original catalog-first design lacked.

### 5.3 Stage 3 — the model selects notes (LLM call 1)

The catalog plus the conversation so far goes to the model, which replies with
the `uuid8`s worth opening.

**Lenient parsing, not JSON.** The response is scanned for 8-character
hex tokens and intersected with the catalog's known `uuid8` set. Anything that
does not resolve is discarded. This survives every provider's formatting habits
(prose, bullets, code fences, event-array wrappers) without a JSON envelope
contract, and it doubles as the hallucination guard.

**Retrieval re-runs every turn**, and the model sees the full conversation when
selecting — so "what about the other one?" retrieves against the accumulated
context rather than four ambiguous words.

### 5.4 Stage 4 — answer (LLM call 2)

Selected note bodies (HTML stripped via the existing
`db::strip_html_to_text`) plus the conversation go to the model.

**Budget caps**, applied in this order:
1. `MAX_NOTE_CHARS = 20_000` — **per note**, applied first. A truncated note is
   marked inline (`… [truncated]`) so the model does not treat it as complete.
2. `MAX_SELECTED_NOTES = 12` notes.
3. `MAX_CONTEXT_CHARS = 120_000` characters of stripped body in total.

The per-note cap is not defensive tidiness: the largest live note is 1,037,880
characters (F1b), which alone would exceed the whole-turn budget more than
eightfold and silently crowd out every other note. Ordering matters — capping
per note *before* the total is what keeps one oversized note from consuming the
turn.

When any cap trims, the UI says so. Ranking for the trim: FTS hits first
(§5.5), then the model's stated order.

**Citations.** The system prompt requires every claim to cite its source as
`[[<title-slug>-<uuid8>]]` — Jodd's existing durable slug form. Citations whose
`uuid8` was not in the provided context are stripped from the rendered answer
and counted; if any were stripped, the UI shows a quiet notice. Answers are
never written to a note, so no `edges` are derived from them.

### 5.5 `ask::extract_query_terms`

A **new** extractor, because the existing one is unusable on questions (F3):
stopword removal only, keep tokens ≥3 chars, no capitalization or repetition
filter. Feeds stage 1's FTS source.

`autolink::extract_keywords` is left untouched — it is correct for its own
input distribution (long, repetitive note bodies), and this is a different
distribution, not a bug in it.

**Scope is applied to FTS results, not inside `search_notes`.** For folder
scope the call passes `label = None` (account filter only) and hits are then
filtered in Rust to the same subtree the rest of the pipeline uses (§5.6).
Passing `search_notes`'s exact-match `label` filter down would make the FTS
source blind to descendants the other two sources can see.

**Shipped behavior (post-579f2ec) is starker than "weak": the FTS source is
silent on pure-Thai questions.** `extract_query_terms` now splits on
`db::is_tag_word_char`, which stopped Thai words being shredded at their tone
marks — a real fix — but Thai has no inter-word spaces, so a pure-Thai
question yields **one token spanning the whole phrase**, which the trigram
index matches against essentially nothing. Measured against the real
6,846-note vault: a content-free Thai question went from 137 FTS hits (the
pre-fix shredded-token behavior) to 0 hits (post-fix). A Thai question that
also contains a Latin content word retrieves normally — e.g. one containing
"ATLAS" produces byte-identically the same 24 hits as the bare term "ATLAS"
alone. In that failure case the turn does not go empty: the recency prior
(§5.1) plus the model reading the catalog still produces an answer, just
without FTS-sourced evidence. This is the §5.1 recall ceiling at its
sharpest, and it is why §3 names embeddings rather than a better tokenizer as
the successor — the real fix is Thai word segmentation or language-agnostic
embeddings, neither in scope here.

### 5.6 Scope selector

Reuses the existing search scope semantics — current folder / current account /
all accounts — mapping onto `Db::search_notes(account_id, label, …)`'s existing
optional filters.

**Default: current account.** Cross-account was the point of §4 and stays
available, but "all accounts" now means 6,841 notes, where the §5.1 pre-filter
does the most aggressive thinning and answer quality is least predictable. The
default should be the scope that behaves best, with the broad one an explicit
choice.

**Folder scope is recursive — "This folder and below" — and this is Ask Jodd's
one deliberate divergence from the existing search box.**

A note carries exactly one label string. Jodd's folder tree is *derived* by
splitting that string on `/`; there is no containment relation underneath it.
In the live vault the 50 notes in `Notes/Projects/ATLAS` and the 14 in
`Notes/Projects/ATLAS/ITO` are two independent labels that merely share a
prefix — Gmail maps them to unrelated ids (`Label_47`, `Label_34`), and for the
LocalFS vertical the path *is* the id. Consequently `search_notes`'s
`label = ?` filter excludes descendants, which is why the sidebar shows 50 next
to ATLAS and 14 next to ITO, never 64. That is correct for a note list, and it
matches Apple Notes.

It is wrong for a question. "What do I know about ATLAS?" means the subtree.
Ask Jodd therefore matches `label = ?1 OR label LIKE ?1 || '/%'` in its own
retrieval path, and the scope selector names the option **"This folder and
below"** so the behavior is stated rather than inferred.

`Db::search_notes` itself is **not** changed: the search box keeps today's
exact-match semantics. Making search recursive is defensible but touches a path
used on every navigation, and this feature does not justify that regression
risk. Ask Jodd gets its own scoped query (`Db::list_notes_in_subtree`, or an
optional `recursive: bool` on a new retrieval helper — the plan picks one);
either way `search_notes`'s existing signature and behavior are untouched.

## 6. Provider layer: one generic `chat` primitive

Add to `LlmProvider`:

```rust
/// Multi-turn free-text completion. Returns the model's raw text — no JSON
/// envelope, because an answer is prose, not a schema. Same cancellation
/// contract as `extract`.
async fn chat(
    &self,
    system: &str,
    turns: &[ChatTurn],
    cancel: CancellationToken,
) -> Result<String, ExtractError>;
```

with `ChatTurn { role: Role::User | Role::Assistant, content: String }`.

- **`HttpProvider`** maps `turns` onto its message array. Today it hardcodes
  exactly two messages ([http.rs:87](../../../src-tauri/src/llm/http.rs)); this
  generalizes that construction. No `response_format` is sent — free text.
- **`AgentCliProvider`** flattens `turns` into a transcript string and feeds it
  through the existing `PromptDelivery` path. The CLI's own JSON wrapper is
  still unwrapped via `dig_unwrap`; what is skipped is only *Jodd's* envelope
  parse. Consequently the single-retry-on-malformed-envelope logic does not
  apply to `chat` — there is no envelope to malform.
- `extract` and `suggest_links` are unchanged.

`ExtractError` is reused as-is, including `Cancelled`. Its name is now
imprecise for a chat call; renaming it would touch every existing call site for
no functional gain, so it stays. This is noted rather than fixed.

## 7. Tauri commands

| Command | Shape | Notes |
|---|---|---|
| `ask_jodd` | `(request_id, scope, turns) -> AskAnswer` | read-only; never touches Gmail |
| `cancel_ask` | `(request_id) -> ()` | mirrors `cancel_extraction` |
| `get_app_llm_config` / `set_app_llm_config` | app-level config | mirrors `get/save_oauth_config` |

`AskAnswer { markdown, cited: Vec<CitedNote>, notes_in_scope, notes_considered,
notes_used, trimmed: bool, dropped_citations: usize }`.

`notes_in_scope` vs `notes_considered` is the pair that makes §5.1's recall
ceiling visible: 6,655 in scope, 400 considered, 9 used.

**Cancellation** mirrors Extract exactly: `AppState.in_flight_asks:
HashMap<String, CancellationToken>`, fired by `cancel_ask`, raced inside the
provider. Both LLM calls in a turn observe the same token, so cancelling during
stage 3 does not proceed to stage 4.

## 8. UI

A modal, following `LessonExtractModal.svelte` — the closest precedent and the
lowest-risk surface. **Shipped placement differs from this spec's original
"above the Smart Folders group":** the entry is a global icon button in the
sidebar **footer**, next to the gear (`Sidebar.svelte:1370-1379`), not a
per-account row. This was a deliberate change made during implementation —
a per-account entry produced one unlabelled duplicate button per account plus
a hidden side effect of changing `currentAccount` on click, neither of which
a single global entry has.

- Scrollable turn list; input pinned at the bottom; Enter sends, Shift+Enter
  newlines.
- Per-turn status line, kept after the turn completes: **"6,655 in scope → 400
  considered → 9 read"**. This is the feature's honesty surface (§5.1) — a
  heavily thinned pool must be visible, not inferred from a weak answer.
- Cancel button during flight, wired to `cancel_ask`.
- Citations render as clickable chips; clicking closes the modal and opens
  that note.
- Closing the modal discards the conversation. No warning — ephemerality is the
  contract, and stating it once in the empty state is enough.

**Not shipped, deliberately deferred:**
- **Markdown rendering.** This spec originally promised the answer would
  render through `llm::markdown`'s pulldown-cmark path, matching Extract.
  What shipped instead is `.answer` styled with `white-space: pre-wrap` —
  literal `**bold**` and `- list` markers stay visible as text. Deferred
  rather than a bug: a markdown step has to run *before* citation
  substitution and preserve `renderAnswer`'s escaping guarantees (the answer
  is spliced via `{@html}` — see `AskJoddModal.svelte`'s safety comment and
  its regression test), so adding one is its own security review, not a
  follow-on to this feature.
- **Empty state linking into App Settings when no app-level provider is
  configured.** Not built. The user currently gets an accurate red error turn
  from the failed `ask_jodd` call instead of a dedicated configure-now
  prompt. Recorded here as a known gap and a candidate follow-up, not as
  something to build under this fix wave.

A docked side panel is the plausible alternative and is deliberately deferred:
it competes for width with the editor and the note list, and nothing about the
feature requires it.

## 9. Doctrine compliance

- **Read-only over the cache.** No writes, no `sync_state` transitions, no
  Gmail calls on any path.
- **Never blocks navigation.** All work happens inside the modal; the LLM call
  is `async` with cancellation. Closing the modal mid-flight fires `cancel_ask`.
- **Nothing round-trips to Apple.** Answers are ephemeral by construction.
- **Local-first is not weakened**: retrieval reads the same SQLite that is
  already the truth-of-the-moment for the UI.

## 10. Error handling

| Case | Behavior |
|---|---|
| No app-level provider | Modal shows the configure prompt; no LLM call |
| Provider `NotConfigured` / `Transport` / `Upstream` | Error shown in the turn list; conversation survives; retry allowed |
| Cancelled | Turn removed, conversation survives, no error styling |
| Stage 1 pool is empty | Answer "nothing found in scope" immediately; no LLM call at all |
| Stage 3 returns no resolvable `uuid8` | Fall back to the FTS hits alone; if those are empty too, answer "nothing found in scope" **without** a stage-4 call |
| Stage 4 cites unknown `uuid8`s | Strip them, report the count |
| Scope has zero notes | Answer immediately, no LLM call |

Nothing here is destructive, so no confirmation gates are required.

## 11. Testing

Unit (Rust):
- **stage-1 pre-filter (§5.1)** — union/dedup of the three sources;
  `CANDIDATE_POOL_MAX` filling order (FTS → structural → recency) when the cap
  binds; recency ordered by `last_remote_modified_at` and **not** by `date`
  (a fixture with RFC822 strings whose lexical order contradicts their true
  order, per F4, so the wrong column fails the test)
- catalog builder — line format and bounded size for a full 400-note pool
- **per-note truncation (§5.4)** — a 1 MB note is cut to `MAX_NOTE_CHARS`,
  marked truncated, and does **not** displace the other 11 selections
- recursive folder scope (§5.6) — a fixture with `A`, `A/B`, `A/BB` and `AB`
  asserting that scoping to `A` includes `A/B` and `A/BB` but **not** `AB`
  (the `LIKE 'A%'` bug the `|| '/%'` guards against), and that the FTS net
  filters to the same set
- `uuid8` lenient parser — prose, bullets, code fences, unknown ids, duplicates
- `extract_query_terms` — the four F3 probe strings must now yield useful terms
- §4.2 cascade — one test per row of the resolution table
- budget caps — `MAX_SELECTED_NOTES` / `MAX_CONTEXT_CHARS` trimming and ordering

Integration (Rust): a fake `LlmProvider` whose `chat` returns canned text,
driving `ask_jodd` end-to-end against a temp-dir DB — asserting that stage 3's
selection reaches stage 4, that citations resolve, and that cancellation
between the two calls prevents stage 4. This mirrors
`examples/roundtrip_localfs.rs`'s
no-network posture.

Frontend (vitest): citation-chip rendering and stripping of unresolvable
citations, following `Icon.test.ts` / `reExtract.test.ts`.

Manual, against the live vault: one factual-recall question, one synthesis
question, one Thai question, one deliberately unanswerable question.

## 12. Open follow-ups (not this spec)

- Agentic tool-loop (§3) once a provider supports function-calling.
- Graph expansion once `mentions` density justifies it (F2).
- Tag-vocabulary control for Extract — roadmap item #0; unrelated to Ask Jodd
  but visible in the same data (27 tags from 4 Extract notes).
- Refactoring `extract` / `suggest_links` onto `chat`.
