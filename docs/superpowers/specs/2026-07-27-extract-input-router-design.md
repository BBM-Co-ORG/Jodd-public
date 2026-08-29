# Extract input router — design

> Status: **design / approved** (2026-07-27). Revised 2026-07-28 against
> `origin/main` @ `0ed3353`, after the `lessons` → `llm` module rename
> (`1fb48e0`, `75e03e8`) and the agent-CLI provider PR (`3ce8d94`, #14) landed.
>
> Adds a deterministic pre-flight classifier to the Extract workflow: it
> inspects pasted text, decides what kind of input it is, and offers a grouped
> set of actions — each mapping to a Jodd command that already exists. Also
> introduces a per-account setting gating LLM calls the user did not explicitly
> trigger, and starts consuming the `confidence` field the provider has been
> returning (and Jodd discarding) since v0.16.1.
>
> Motivated by a quality review of the six notes in `Notes/__Extracts__`: four
> of them (67%) are permanent notes recording the LLM's inability to fetch a
> YouTube transcript, produced by pasting the same URL four times in one night.

## Problem

### 1. Nothing inspects the input before spending an LLM call

`extract_note` ([lib.rs:3523](../../../src-tauri/src/lib.rs)) sends whatever
`source_text` it receives straight to the provider. There is no pre-flight
validation. A 47-character YouTube URL costs a full LLM round-trip and produces
a note.

### 2. The failure gate is at the wrong layer

The lesson-extraction spec's failure doctrine — *source text is NEVER lost* — is
implemented as: on provider **error**, create a fallback note preserving the
source ([lib.rs:3578](../../../src-tauri/src/lib.rs)). But the four YouTube
notes were not provider errors. The provider **succeeded**: it returned a
well-formed `ExtractEnvelope` whose content happened to be an apology. The gate
tests *transport success*, not *semantic success*, so the apology flowed through
the normal path and was written as a real note, synced to Gmail, and propagated
to Apple Notes on every device.

Evidence from the live DB (`Notes/__Extracts__`, all one account, all the same
source URL `youtube.com/watch?v=ZXg70X9RhKs&t=2831s`):

| created | title |
|---|---|
| 2026-07-20 00:52 | YouTube Video Extraction Limitation |
| 2026-07-20 00:56 | YouTube Video Link Without Transcript |
| 2026-07-20 02:36 | YouTube Video Link (ZXg70X9RhKs) - Missing Transcript |
| 2026-07-20 02:46 | Video transcript unavailable for extraction |

Their tags are worse than useless — they describe the tool's failure rather than
any subject matter (`url-input`, `missing-context`, `extraction-limitation`,
`missing-transcript`, `url-only`, `transcript-needed`, `content-extraction`),
and they are mutually inconsistent across four runs of identical input
(`url-input` vs `url-only`; `missing-transcript` vs `transcript-needed`). These
now sit permanently in the sidebar's Tags pills beside `autophagy` and
`settrade-sdk`.

### 3. `confidence` is parsed and thrown away

`ExtractEnvelope.confidence` ([llm/provider.rs:32](../../../src-tauri/src/llm/provider.rs))
is deserialized on every extract. Across `origin/main`, the only other mentions
are the prompt that asks for it and one parser test — there is no consumer. The
signal that would most cheaply have flagged these four notes was already in hand
and discarded.

### 4. Every Extract silently costs two LLM calls

`runAutoLinkSuggestions` is invoked unconditionally after both extract paths
([LessonExtractModal.svelte:343](../../../src/lib/components/LessonExtractModal.svelte)
and [:407](../../../src/lib/components/LessonExtractModal.svelte)), per auto-link
spec decision 3 (*"runs automatically as part of every relevant flow — not a
separate manual trigger the user has to remember to press"*). That was framed as
a convenience, but it is a background LLM call the user never requested and
cannot turn off.

## Decisions (locked in brainstorming)

1. **Capability delegation, not a home-grown fetcher.** Jodd will not add
   reqwest-based article fetching, an HTML→text pipeline, a YouTube transcript
   client, or git-repo reading. Where content retrieval is possible at all it is
   because the configured provider can do it (e.g. an agent CLI granted a
   web-fetch tool). Rejected: building fetchers in Rust — a whole new subsystem
   (network, HTML parsing, rate limiting, per-host error handling) that would
   need its own spec.

2. **"Command" means an existing Jodd action.** The router never invents a new
   surface. Every action it offers resolves to a command that already ships.
   Explicitly rejected: building a conversational/chat surface inside Jodd. If
   the input looks like a question, the router offers `search_notes` — it does
   not answer.

3. **Live on paste, and always soft.** Classification runs as the user types
   (debounced), not as a gate on the Extract button. Every warning carries an
   implicit "Continue anyway": the Extract button is never disabled by the
   router. This follows the duplicate-citation precedent
   (`check_duplicate_citations` — soft warning, never a hard block).

4. **This phase declares the fetch capability but never enables it.** No
   tool-granting flag is added to any agent-CLI preset's `args`
   ([llm/presets.rs:46](../../../src-tauri/src/llm/presets.rs) is the `claude`
   row). The `FetchThenExtract` action exists in the model and renders as a
   disabled chip with a reason. Granting tools to a subprocess whose target URL
   is controlled by pasted text is a prompt-injection surface that deserves its
   own security review, and gating it behind a later spec costs this phase
   nothing — the deterministic classifier alone resolves every observed failure.

5. **Deterministic classification only; consume the existing `confidence`.** No
   LLM intent-detection stage in this phase. The classifier is pure Rust. The
   only new quality signal is `confidence`, which costs zero additional calls
   because the provider already returns it.

6. **Any LLM call the user did not explicitly trigger must be gated by a
   per-account setting — including the existing auto-link call.** This decision
   is retroactive: `suggest_wiki_links` stops firing unconditionally.

7. **Architecture: one Rust command returning analysis *and* actions.** Chosen
   over (a) returning only the classification and mapping kind→actions in
   TypeScript, and (b) a trait-based handler registry mirroring `Vertical`.
   Reasoning:
   - provider capability is resolved from `accounts.json`, which lives on the
     Rust side — a TS mapping would require exporting the whole config;
   - `db::extract_urls` is reusable as-is rather than reimplemented in TS;
   - a single command is reusable by `jodd-mcp` or any future entry point.

   Note that "there is no frontend test setup" is **no longer** a reason: #14
   added Vitest (`npm test`), jsdom, and component fixtures, and a TS classifier
   could now be tested properly. The three reasons above still decide it.

   The trait registry is YAGNI at five kinds — revisit when the taxonomy
   stabilizes around 8–10 and action shapes visibly repeat.

## Approach

### 1. Classifier (`src-tauri/src/llm/classify.rs`, new)

Operates on the trimmed source text. Uses the existing
`db::extract_urls` ([db.rs:2968](../../../src-tauri/src/db.rs)) — no new URL
scanner, no `regex` crate.

```rust
pub enum HostKind { Media, Repo, Article }

pub enum InputKind {
    Empty,
    UrlOnly { urls: Vec<String>, host_kind: HostKind },
    Question,
    ShortText { char_count: usize },
    Prose,
}
```

Detection rules, evaluated in order:

| Kind | Rule |
|---|---|
| `Empty` | text is empty or whitespace only |
| `UrlOnly` | after removing every detected URL, fewer than **80** alphanumeric characters remain |
| `Question` | no URL; at most 2 non-empty lines; ends with `?` **or** starts with a question word (Thai: ทำไม / อะไร / ยังไง / ไหม / หรือเปล่า; English: what / why / how / should / can / is) |
| `ShortText` | no URL; fewer than **120** alphanumeric characters; not a question |
| `Prose` | everything else — the normal, healthy case |

`host_kind` is derived from the URL host: `Media` for youtube.com / youtu.be /
vimeo.com / spotify.com; `Repo` for github.com / gitlab.com / bitbucket.org;
`Article` otherwise.

The 80 and 120 thresholds are deliberately low. Because every warning is soft, a
false positive (nagging about input that was fine) is more annoying than a false
negative, so the rules err toward silence. They are tunable constants, not
config.

**`ChatLog` is deliberately NOT a kind.** It is detectable (speaker prefixes
`User:` / `Assistant:` / `Human:`, the `★ Insight` marker) but yields exactly the
same action set as `Prose`, so it would be a branch with no distinct
destination. Revisit if a chat-log-specific prompt ever exists.

### 2. Capability model and actions

Capability is a property of the **resolved provider**, not a global constant.
Since #14 an account resolves to one of four things
([accounts.rs:30](../../../src-tauri/src/accounts.rs)): `None` (unconfigured),
`Http`, `AgentCli { preset }`, or the legacy `ClaudeCode` variant, which is read
as the `claude` agent-CLI preset and never rewritten to disk. Each agent-CLI
preset carries its own `AgentCliSpec { binary, args, … }`, so whether fetching is
possible is a per-preset question — a preset given a web-fetch tool could fetch;
the HTTP provider never can.

```rust
pub struct ProviderCapabilities {
    pub configured: bool,     // is any LLM provider set up for this account
    pub can_fetch_url: bool,  // phase 1: false for every provider — decision 4
}

pub enum ActionId {
    ExtractNew,
    ExtractAppend,
    SearchNotes,
    SaveAsPlainNote,
    FetchThenExtract,
}

pub struct OfferedAction {
    pub id: ActionId,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

pub struct InputAnalysis {
    pub kind: InputKind,
    pub evidence: String,          // human-readable, rendered in the strip
    pub actions: Vec<OfferedAction>,
}
```

`unavailable_reason` being a user-facing `String` rather than an enum follows the
precedent set by `Availability::NotAvailable(&'static str)`
([llm/presets.rs:21](../../../src-tauri/src/llm/presets.rs)), whose doc comment
states the rule directly: *"The text is shown to the user verbatim, so it must
say what THEY can do about it."* An enum would force the frontend to enumerate
reasons it has no need to distinguish.

Every action maps to a command that already exists:

| Action | Command | LLM call |
|---|---|---|
| `ExtractNew` | `extract_note` | yes |
| `ExtractAppend` | `append_extract_note` | yes |
| `SearchNotes` | `search_notes` (via the `searchQuery` store) | no |
| `SaveAsPlainNote` | `save_note` | **no** |
| `FetchThenExtract` | *(phase 2)* | — always `available: false` |

`ExtractNew` and `ExtractAppend` are never rendered as chips — they are the
modal's existing Extract button plus its destination toggle, and they stay
available for every kind (decision 3: the router never disables Extract). They
appear in `actions` so that a non-UI consumer such as `jodd-mcp` receives the
complete action set rather than only the advisory subset.

`SaveAsPlainNote` is new to this design and is the cheapest useful escape hatch:
keep the pasted material as an ordinary note without involving an LLM at all.
Citation (`cites`) edges are still derived, because `AppleHtmlDeriver` runs on
every write regardless of how the note was created.

New Tauri command:

```rust
#[tauri::command]
fn classify_source_input(account_id: String, text: String) -> InputAnalysis
```

**It must be added to `generate_handler!`** (the list containing
`check_duplicate_citations` at [lib.rs:4349](../../../src-tauri/src/lib.rs)).
`ee7e775` added `every_literal_invoke_name_is_a_registered_command`
([lib.rs:4560](../../../src-tauri/src/lib.rs)), which scans the Svelte tree for
literal `invoke()` names and fails the build if one is not registered — so
forgetting this is caught by `cargo test`, not by a user.

### 3. Per-account LLM-call gating

New field on `LlmConfig` ([accounts.rs:41](../../../src-tauri/src/accounts.rs),
reached as `Account.llm`), beside the `agent_preset` / `agent_custom` fields #14
added — not on `Account` directly, because this is LLM configuration and
`LlmProviderSettings.svelte` already edits exactly this struct:

```rust
#[serde(default)]
pub secondary_llm_calls: Option<SecondaryLlmPolicy>,  // None resolves to Ask

pub enum SecondaryLlmPolicy { Auto, Ask, Off }
```

`Option` + `#[serde(default)]` keeps pre-existing `accounts.json` files parsing,
matching the `agent_preset` / `notes_label` precedent.

| Value | Behavior |
|---|---|
| `Auto` | the auto-link checkbox renders pre-checked |
| `Ask` | the checkbox renders unchecked — **default** |
| `Off` | the checkbox is hidden; `suggest_wiki_links` is never called |

**The default is `Ask`, which changes existing behavior on upgrade.** This is
intentional: spending a user's quota without asking is not a defensible default,
and the current unconditional behavior is precisely what this decision exists to
correct.

**UX is a checkbox, not a prompt.** The modal closes immediately on a successful
extract ([:343](../../../src/lib/components/LessonExtractModal.svelte)), so a
post-hoc confirmation dialog would appear after the context is gone. Instead, a
checkbox sits beside the Extract button:

> ☐ 🕸 Link into wiki after extract *(uses one additional LLM call)*

The cost is visible at the moment of decision, and `Ask` is satisfied without
introducing any new modal — "unchecked by default" achieves what asking every
time would, without the interruption.

Settings UI: one row in `LlmProviderSettings.svelte`.

### 4. `confidence` gate

After the provider returns and **before any note is written**:

- `confidence == "low"` → do not write. Render a preview in the modal (title,
  tags, and the body that would be created) with `[Keep]` and `[Discard]`.
- `"high"` / `"medium"` / absent → write as today. No behavior change.

Applies to **both** `extract_note` and `append_extract_note`. Appending
low-confidence output to an existing note is worse than creating a bad new note,
because it is harder to undo.

`[Discard]` leaves the modal open with the source text intact in the textarea.
Nothing was written to SQLite, so the failure doctrine holds — the source is not
lost.

Implemented as a pure, separately testable predicate:

```rust
pub fn should_gate(confidence: Option<&str>) -> bool
```

**Honest limitation:** it is not known whether the four YouTube extracts
returned `confidence: "low"`, because the value was never persisted. The
classifier catches that class of input deterministically and with certainty; the
confidence gate is a second net for cases the classifier cannot anticipate. It
should not be relied on as the primary defense.

### 5. UI — the analysis strip

A strip between the textarea and the action row in `LessonExtractModal.svelte`
(the component keeps its name; only the Rust module was renamed). It renders
**only when `kind` is neither `Prose` nor `Empty`** — the healthy path stays
completely silent.

| Kind | Message | Chips |
|---|---|---|
| `UrlOnly{Media}` | 🎬 Video link only — nothing to extract yet. Try pasting the transcript too. | `[💾 Save as plain note]` `[🔒 Let AI fetch it]` |
| `UrlOnly{Article}` | 🔗 Article link only | `[💾 Save as plain note]` `[🔒 Let AI fetch it]` |
| `UrlOnly{Repo}` | 📦 Repo link only | `[💾 Save as plain note]` `[🔒 Let AI fetch it]` |
| `Question` | ❓ Looks like a question, not source material | `[🔍 Search your notes]` `[💾 Save as plain note]` |
| `ShortText` | ✏️ Very short (N characters) — extraction may yield less than what you pasted | `[💾 Save as plain note]` |

The Extract button remains enabled in every case. Guidance lives in the message
line; chips are reserved for real actions. `[🔒 Let AI fetch it]` renders
disabled with its `unavailable_reason` as a tooltip — a visible seam for phase 2.

`[🔍 Search your notes]` sets the existing `searchQuery` store
([notes.ts:41](../../../src/lib/stores/notes.ts), already promoted to a store so
other components can drive it) and closes the modal.

### 6. Data flow and races

```
paste / type
  → debounce 300 ms
  → invoke('classify_source_input', { accountId, text })
  → InputAnalysis
  → render strip
```

Stale responses are dropped via a sequence number, matching the existing
`targetSearchSeq` / `sourceNoteSearchSeq` pattern already in this modal.
Re-classification is skipped when the text is unchanged.

### 7. Error handling

If `classify_source_input` fails for any reason, the strip does not render and
Extract behaves exactly as it does today. An advisory feature must never block
the primary path — this mirrors the `check_duplicate_citations` catch
([:279](../../../src/lib/components/LessonExtractModal.svelte)), which logs and
proceeds as if no warnings were found.

The classifier is a pure function touching neither network nor database, so in
practice only IPC can fail.

### 8. Net effect on LLM usage

| Scenario | Before | After |
|---|---|---|
| Ordinary extract | 2 calls (extract + auto-link) | **1** |
| Extract + wiki linking | 2 calls | 2 (user opted in) |
| Pasting a bare URL | 2 calls → a junk note | **0** |
| Keeping a link for later | not possible | **0** (`SaveAsPlainNote`) |

This spec introduces no new LLM call.

## Testing

**Rust** — unit tests in `llm/classify.rs`, using the **six real sources from the
live database** as fixtures — data that actually produced the failures, not
invented cases:

- the four YouTube URLs → `UrlOnly { host_kind: Media }`
- the Settrade debugging source → `Prose`
- the hormones/autophagy source → `Prose`

Additional cases:

- a URL followed by a full paragraph → `Prose` (an inline URL must not be
  mistaken for a URL-only paste)
- Thai and English questions → `Question`
- github.com / gitlab.com URLs → `UrlOnly { host_kind: Repo }`
- boundary: 79 / 80 / 81 alphanumeric characters remaining after URL removal
- `actions_for(kind, caps)` — table-driven across every kind × capability pair
- `should_gate(confidence)` — pure predicate, no `AppState` required
- `every_literal_invoke_name_is_a_registered_command` must still pass, which it
  will only if `classify_source_input` is registered

**Frontend (Vitest, added by #14)** — `reExtract.test.ts` established the
pattern, so the strip's behavior is now testable directly rather than by
inspection:

- strip does not render for `Prose` or `Empty`
- strip renders the right chips per kind
- Extract stays enabled for every kind (decision 3)
- the auto-link checkbox reflects `Auto` / `Ask` and is absent for `Off`

## Deferred

- **Granting a web-fetch tool to an agent-CLI preset**, and the
  prompt-injection review that must precede it. This is what turns
  `FetchThenExtract` from a disabled chip into a working action. Note it is now
  a per-preset decision, not one global flag.
- **LLM intent detection** for genuinely ambiguous input. Rejected for this
  phase on cost and latency grounds; the deterministic rules resolve every
  observed failure.
- **`ChatLog` as a distinct kind** — until a chat-log-specific prompt exists to
  give it a different destination.
- **A conversational surface in Jodd.** Out of scope by decision 2.
- **Prompt improvements identified while reviewing the six extracts** — the
  `unresolved threads` rule is stated only as an inline example and was dropped
  by both good extracts (the Settrade source's closing question, the hormones
  source's cortisol-reduction list); caveats were dropped from the health
  extract. These are extraction-quality issues, unrelated to routing, and belong
  in their own spec.
- **Cleaning up the four junk notes and the failure-describing tags they
  introduced.** A one-time data cleanup, not a code change.
