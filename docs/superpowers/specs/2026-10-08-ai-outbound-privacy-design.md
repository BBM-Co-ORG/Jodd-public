# AI outbound privacy — mask what leaves, keep hidden folders home

Status: design decided in conversation 2026-10-07/08, section by section
(six sections, each approved). Scope is the **AI side only**: what Jodd sends
to an LLM provider. What Jodd stores on a backend (Gmail, Exchange, CloudKit)
is out of scope — Apple Notes must be able to read it, so it cannot be masked
without breaking the round trip.

## 1. Intent

Data on the user's own device is protected already (SQLCipher at rest,
keychain credentials). What leaves the device to an **AI provider** is not:
apart from URL query-stripping in Ask Jodd and Organize skipping notes with
secrets, note text goes out raw.

The user named three concerns (2026-10-07):

1. **Secrets leak** — passwords, API keys, tokens, PINs in notes reach a
   vendor's logs or training data.
2. **PII / PDPA** — other people's e-mail, phone, national ID, card and bank
   numbers leave the country with no legal basis.
3. **Some folders must never leave** — masked or not.

Transparency ("show me before sending") was explicitly *not* a goal.

**Success:**

- No detected secret or structured PII value reaches any provider; the AI
  sees `⟦EMAIL_1⟧`, and the user gets the real value back in the result.
- No content, title, snippet or folder name from a hidden folder reaches any
  provider, through any AI feature.
- A new account does not send data to AI until the user allows it.

## 2. Decisions

| # | Question | Decision |
|---|---|---|
| D1 | PII scope, round 1 | **Structured only**: e-mail, phone, Thai national ID, card number, bank account, passport. No NER — names and addresses are not detected |
| D2 | What happens to a detected value | **Reversible tokenization**: replaced by `⟦KIND_N⟧` before sending, restored in the result |
| D3 | Where masking happens | **Inside `CheckedProvider`** (approach A) — the one type every production AI call already passes through |
| D4 | Which folders are excluded from AI | **The same `hidden_folders` list** that hides folders from MCP agents (`mcp_write_scope.json`) — one switch, gotcha #33's single scope model |
| D5 | Agent CLIs that can read files themselves | **Warn now, lock down later**: a settings badge in this work; disabling each preset's tools is a separate roadmap item, measured per preset (gotcha #7) |
| D6 | `data_allowed` default | **Opt-in for new accounts only**; existing accounts keep their current behavior through a one-time migration |
| D7 | Can masking be turned off | No. Always on; a toggle is not needed yet |

## 3. Where data leaves today (measured 2026-10-07, read from code)

> **Re-checked 2026-10-09 against `main` after #133–#164.**
> - **Design unchanged.** The provider construction sites are the same eleven.
> - **The Ask leak grew.** Since #159, `notes_fts` carries each note's folder, so a question that *names* a hidden folder pulls its notes in as FTS hits, not just through the recency prior. Autolink's `search_notes` candidates widen the same way.
> - **Filing now excludes sync-blocked folders.** #162 and #164 added `OnlyBlockedFolders`, so the hidden-folder filter must run before that split; PR1 Task 4 does this.
> - **Provenance (#134) does not interact with masking.** It records AI writes as `Actor::Workflow` (name + model, never content). Unmasking happens before any write, so no token reaches a note's history.

Every production AI call goes through `llm::policy::CheckedProvider`
(`llm/policy.rs:148-265`), whose `call()` wraps all six `LlmProvider` text
methods. The one call outside it is `test_llm_provider` with no account
(`lib.rs:7219`), which sends only the fixed `CONNECTION_TEST_SAMPLE`.

| Path | Note data sent | Hidden folders excluded today? |
|---|---|---|
| Ask Jodd | catalog (title/folder/tags/date of ≤400 notes) + selected bodies, in `chat`'s `system` | **No** — `ask/pool.rs` `in_scope` checks account and folder subtree only |
| Extract, Summarize/ActionItems/Expand/Clean, append variants | `source_text` from the frontend | **No** — backend never learns where the text came from |
| `preview_action_items` | note body by `source_uuid`, or IPC text | No |
| `re_extract_note` | note body by uuid | No |
| Autolink (`suggest_wiki_links`) | new text + other notes' titles and 200-char snippets | **No** — `autolink.rs` candidates |
| Filing (`suggest_note_folder`) | note text + every folder path | **No** — `filing::candidate_folders` |
| URL / YouTube ingest | fetched pages, user context | n/a (not note data) |
| Organize (curate) | excerpts | **Yes** — `curate/candidates.rs:27`, `decide.rs:92`; notes with secrets skipped (`decide.rs:116`) |

Existing redaction: `ingest::urls::redact_url_secrets` (Ask context only),
`display_url` (ingest), `text_for_suggestions` (cuts Sources blocks).

Agent CLI presets differ in whether the CLI can reach data **beyond the
prompt** (`llm/presets.rs`): `claude` (`--tools ""`) and `thclaws`
(`--allowed-tools ""`) cannot; `codex` (`--sandbox read-only` blocks writes,
not reads) can; `gemini`/`qwen` (plan mode), `opencode`, `aider` and custom
presets are unverified. `opencode`, `aider` and `thclaws` deliver the prompt
on argv (`PromptDelivery::Argv`), visible to `ps`.

## 4. Components

```
jodd_lib::privacy            (new)
  detect.rs    text → Vec<Span{kind, start, end}>; owns all secret + PII rules
  vault.rs     per-call token table: mask(text), unmask(text), summary()
  wire.rs      mask/unmask for each provider input and output type
  ai_scope.rs  AiScope: hidden-folder snapshot for AI selection
jodd_lib::curate::secrets    becomes a thin wrapper over privacy::detect
jodd_lib::llm::policy        CheckedProvider masks/unmasks around inner
jodd_lib::llm::presets       + file_access per preset
jodd_lib::accounts           + AccountsFile.ai_consent_version migration
```

### 4.1 Detector — `privacy/detect.rs`

One job: return every sensitive span in a text. `Span { kind, start, end }`,
byte offsets that always fall on char boundaries. It decides nothing else.

**One secret detector for the whole app.** The rules in
`curate/secrets.rs` move here and learn to return spans;
`curate::secrets::scan` becomes a wrapper that dedupes by kind and masks as
before. Its existing tests must pass unchanged. Two places that know what a
secret is would drift.

| Group | Kind (token name) | Rule (hand-rolled, no `regex` dependency — crate convention) |
|---|---|---|
| Secret | `PASSWORD`, `PIN`, `API_KEY`, `SECRET` | keyword then `:`/`=` (existing rules), **plus Thai keywords**: รหัสผ่าน, รหัส, พาสเวิร์ด, พิน |
| Secret | `API_KEY` (AWS, GitHub, Slack, `sk-`) | known key shapes (existing) |
| Secret | `PRIVATE_KEY` | the **whole** PEM block, BEGIN to END |
| Secret | `SECRET` | high-entropy token beside a keyword (existing) |
| PII | `EMAIL` | `local@domain.tld` |
| PII | `PHONE` | Thai `0[2-9]…` 9–10 digits, `+66…`, international `+` and 8–15 digits; `-` and space separators allowed |
| PII | `THAI_ID` | 13 digits, dashed or not, **mod-11 checksum must pass** |
| PII | `CARD` | 13–19 digits, separators allowed, **Luhn must pass** |
| PII | `BANK_ACCOUNT` | 10–12 digits **with a keyword on the same line**: บัญชี, เลขที่บัญชี, account, a/c, acct |
| PII | `PASSPORT` | 1–2 capital letters + 6–8 digits **with a keyword**: passport, หนังสือเดินทาง |

- **Thai keyword `รหัส`.** It is broad: รหัสไปรษณีย์ is a postal code and
  รหัสสินค้า a product code. It counts only when `:`/`=` follows it
  *directly*, which the existing `value_after` rule already enforces. So
  `รหัสไปรษณีย์: 10110` is not matched, and `รหัส: ABC-001` is masked as a
  password. That false hit is accepted.
- **Overlap:** a secret beats PII; within a group the longer span wins.
- **Only the value is masked**, never the keyword:
  `password: hunter22` → `password: ⟦PASSWORD_1⟧`. The AI still knows a
  password is there.
- **Tuning.** A miss leaks; a false hit only shows the AI a token in place of
  an ordinary number, which can make a summary worse. So long numbers need a
  checksum or a keyword. Bare long numbers are not masked.
- **Not detected, by design (state it to users):** names, addresses, and
  secrets with neither a keyword nor a known shape.
- The existing `aligned` guard (skip keyword search when `to_lowercase`
  changes byte length) must still hold. Spans are now offsets into the
  original text, so Thai and emoji input needs test coverage.

### 4.2 Vault — `privacy/vault.rs`

`Vault::new()`, `mask(&mut self, &str) -> String`,
`unmask(&self, &str) -> String`, `summary() -> MaskSummary`.

- **Token shape:** `⟦KIND_N⟧` (U+27E6/U+27E7, KIND from §4.1, N from 1).
  Brackets almost never occur in real notes.
- **Identity key is the exact string `(kind, raw value)`.** Same value gives
  the same token within a call. No normalization: `081-234-5678` and
  `0812345678` get two tokens. *Changed during design:* normalizing would
  restore the first-seen form, and an ActionItems quote of the second form
  would then fail `meeting::validate`'s verbatim check.
- **Lifetime: one vault per provider call.** Nothing persists across calls.
  Ask Jodd resends the whole conversation each call (history is held
  unmasked in `policy::Runtime`), so tokens are consistent within every call.
  Token numbers may differ between turns, which is harmless because the model
  sees one call at a time.
- **Pre-existing token-shaped text** (a note about this feature, an old note
  holding a token, a hostile web page) is scanned first, and its numbers are
  reserved. `unmask` replaces **only tokens this vault issued**. An injected
  `⟦PASSWORD_1⟧` therefore can never be turned into a real password.
- **Tolerant unmask:**

  | Model wrote | Result |
  |---|---|
  | `⟦EMAIL_1⟧` | restored |
  | `[EMAIL_1]`, `【EMAIL_1】`, `⟦EMAIL 1⟧` | restored |
  | bare `EMAIL_1` at word boundaries | restored if this vault issued it |
  | `⟦EMAIL_9⟧` never issued | left as is, counted `unresolved` |
  | `email_1` | not restored (case-sensitive) |

- **Hygiene:**
  - `Vault` has a hand-written `Debug` that prints counts only.
  - Never written to disk, logs or receipts; dropped at the end of the call.
- **Not covered:** streaming. `chat` returns a whole `String` today. Streaming
  would need buffering across chunk boundaries.

### 4.3 Wiring — `CheckedProvider` + `privacy/wire.rs`

Each of the six text methods of `CheckedProvider` (`llm/policy.rs:213-265`)
does:

```rust
let mut v = Vault::new();
let masked = wire::mask_<input>(&mut v, …);
let out = self.call(c.clone(), self.inner.<method>(masked…, c)).await?;
v.record_metrics();            // receipts::metric, counts only
Ok(wire::unmask_<output>(out, &v))
```

Why inside `CheckedProvider` and not a separate wrapper:

- `CheckedProvider` has two constructors (`build_account_provider` and
  Ask's `resolve_app_provider` path). A separate `MaskingProvider` would have
  to be remembered at both — gotcha #6's shape.
- Every non-default trait method must be implemented by `CheckedProvider`.
  The compiler therefore forces a new method through this file.

The per-type code lives in `privacy/wire.rs`, so `policy.rs` does not grow.

| Method | Mask (input) | Untouched | Unmask (output) |
|---|---|---|---|
| `extract` | `source`, `existing_tags` | — | `title`, `lessons_markdown`, `meta_lessons_markdown`, `tags` |
| `run_workflow` | `source`, `existing_tags` | `WorkflowKind` | as `extract` |
| `suggest_links` | `source`, candidate `title`, `snippet` | candidate `uuid` | `addition_text` (`uuid`, bools untouched) |
| `suggest_folder` | `note_text`, `folders` | — | `folder`, `reason` |
| `synthesize` | `context`, digest `title`, `display_url`, `lessons_markdown` | `status`, `part` | as `extract` |
| `chat` | `system` (Ask puts catalog + bodies here), every `turns[].content` | `role` | the answer |

`confidence` is enum-like and is never touched.

- **Validation still holds.** ActionItems quotes are validated *inside* the
  inner provider, tokenized against tokenized. After unmask they equal the
  real source byte for byte (the exact-string key above). Filing's "answer
  must be an offered folder" runs in the caller, after unmask. Uuids and
  uuid8s are never masked, so autolink and Ask selection still match.
- **Errors stay masked, on purpose.** `MalformedEnvelope { raw }` and
  provider error text keep what the provider saw. Errors travel to the UI,
  `applog` and bug-report screenshots; results travel only into the user's
  own notes.
- **Receipts:**
  - Each step gets content-free metrics through the existing
    `receipts::metric`: `masked_email`, `masked_phone`, …, `mask_unresolved`.
  - No values and no tokens are stored.
  - AI Receipts can show "masked 2 e-mail, 1 password".
- **`test_llm_provider` without an account** stays outside, with a comment
  saying why: it sends a fixed sample.
- **Field guard.** A test builds every output envelope with a token in every
  `String` field, unmasks, serializes, and asserts no issued token remains
  outside an explicit allowlist (`uuid`, `confidence`). Adding a `String`
  field without wiring it turns this test red.

### 4.4 Hidden folders — `privacy/ai_scope.rs`

Exclusion is about **which data is selected**, which `CheckedProvider` cannot
see. Once a note becomes a `String` its origin is gone. So exclusion is
enforced where data is selected, through one shared type.

**`AiScope`** is a snapshot loaded once per command via
`mcp_scope::load_from`:

- `note_allowed(account, label)` uses `mcp_scope::is_hidden`, which goes
  through the gotcha #33 matcher: subtree, path-aware.
- `folder_allowed(account, path)` does the same for folder paths.
- Missing file (`NotConfigured`): nothing is hidden.
- **Unparseable file: fail closed.** Every AI command refuses with "can't
  read mcp_write_scope.json — fix it first". This matches
  `curate/apply.rs:186`.

| Path | Enforcement |
|---|---|
| Ask pool | filter in `pool.rs`'s `push` closure — one point covers FTS, subtree and recency sources |
| Ask context | re-check in `ask/context.rs` before reading a body (defense in depth against an unoffered uuid) |
| Ask scoped to a hidden folder | refuse: "this folder is hidden from AI" |
| Autolink | filter candidates in `autolink.rs`; refuse if `source` is a hidden note |
| Filing | filter `candidate_folders`; refuse if the note itself is hidden |
| `preview_action_items`, `re_extract_note` | check the source note's label |
| Extract, workflows, append variants, autolink | new **required** IPC parameter `source: AiSource` (below) |
| Organize | switch to `AiScope` (behavior unchanged) |

**`AiSource`** is an enum, `Note { uuid } | Pasted`, and is **required** on
`extract_note`, `run_llm_workflow`, `append_extract_note`,
`append_llm_workflow_note` and `suggest_wiki_links`.

- Required, not optional: every frontend call site must decide, and a
  forgotten one fails loudly in Tauri instead of passing silently. Same idea
  as gotcha #18's no-wildcard rule.
- For `Note`, the backend reads the label itself and does not trust the
  frontend.
- `Pasted` is the user's explicit act. Text the user copies out of a hidden
  note and pastes cannot be stopped; the docs say so.

**UI mirror** (like `canWriteAccount`): AI buttons on a hidden note or folder
are disabled with a tooltip giving the reason. The backend remains the
enforcement.

### 4.5 `data_allowed` opt-in for new accounts

**Problem.** `Account.llm.data_allowed: Option<bool>` (`accounts.rs:151`) is
`None` both for old accounts that never chose and for brand-new ones.
`account_allowed` reads `unwrap_or(true)` (`policy.rs:17`).

**Fix: a one-time file-level migration.**

- `AccountsFile` gains `ai_consent_version: u32` (serde default 0).
- On load, when the version is 0:
  1. every account whose `data_allowed` is `None` becomes `Some(true)`, which
     preserves today's behavior;
  2. the version is set to 1 and the file saved.
- After that, `account_allowed` reads `unwrap_or(false)`. Every account
  creation path (Gmail, Microsoft, iCloud, SSH, LocalFs, Android) yields
  "not allowed" by doing nothing, so a forgotten path fails closed.
- **On every load at version ≥ 1, any remaining `None` becomes
  `Some(false)` and is saved.** Without this the version marker is a single
  point of failure. An older Jodd rewrites `accounts.json` without the field;
  per memory, `tauri dev` and the installed app share the file and can run at
  once. The next load would then see version 0, re-run the migration, and
  silently turn a new account's `None` into `Some(true)`. Once every `None`
  has been materialized, a lost marker can only affect accounts that the old
  binary itself created. Those are legacy by definition.
- If the save fails, the migrated values are used in memory and the save is
  retried on the next load. A full disk must not switch AI off for existing
  accounts.
- Downgrade is safe: an older Jodd reads `Some(false)` and honors it.

**Frontend mirror.** `LlmProviderSettings.svelte:118,208` change
`data_allowed ?? true` to `?? false`. Otherwise a new account shows the box
ticked while the backend refuses. A test pins the two sides together.

**Refusal UX.** The existing message ("AI data access is disabled… Review its
AI data permission in Account Settings") gains a button, **"Allow AI for this
account"**, that sets `data_allowed = true`. Pressing it is the consent.

### 4.6 Agent CLI warnings

A new field per preset in `llm/presets.rs`:
`file_access: Disabled | Enabled | Unknown`.

| Preset | `file_access` | Basis |
|---|---|---|
| claude | Disabled | `--tools ""`, `--strict-mcp-config` |
| thclaws | Disabled | `--allowed-tools ""` |
| codex | Enabled | `--sandbox read-only` blocks writes, not reads |
| gemini, qwen | Unknown | plan mode, unmeasured |
| opencode, aider | Unknown | no tool-disabling flag |
| custom | Unknown | user-defined args |

This table is **derived from flags, not measured**. Per gotcha #7, `Unknown`
must not be upgraded to `Disabled` without a live run.

Settings badges:

- When `file_access != Disabled`: "This provider can read files on your
  computer by itself. Masking and hidden folders protect what Jodd sends;
  they cannot stop the tool reading files directly (LocalFs accounts keep
  notes as plain .eml files)."
- When `PromptDelivery::Argv` (derived from the existing field, not a new
  one): "This provider receives text on the command line, visible to other
  programs on this computer. The text is masked."

## 5. Error handling

| Situation | Behavior |
|---|---|
| `mcp_write_scope.json` unparseable | every AI command refuses with a fix-it message (fail closed) |
| Source note is in a hidden folder | refuse with "this note's folder is hidden from AI" |
| Account not allowed | existing refusal + "Allow AI for this account" button |
| Model returns an unissued token | left in the text, counted `mask_unresolved` in the receipt |
| Model mangles a token beyond §4.2's tolerance | stays as text in the result; no guessing |
| Provider error / malformed envelope | error text stays masked |
| accounts.json save fails during migration | no save on load (§9c): migrated values live in memory and the next ordinary save materializes them |

## 6. Testing

All test values are **synthetic**: Thai IDs generated to pass the checksum,
Luhn-valid card numbers. Key shapes that match `sync-to-public.sh`'s
credential scan are assembled at runtime, as in #130.

| Layer | Tests |
|---|---|
| detect | per kind: must-find / must-not-find. Includes Thai text, emoji, dates, order numbers and uuid8s. Every span is on a char boundary. A bad checksum or Luhn is not matched. **`curate::secrets` tests pass unchanged** |
| vault | `unmask(mask(x)) == x` over a fixture corpus plus a dependency-free random loop; reserved pre-existing tokens; injected token never resolved; §4.2 tolerance table; `{:?}` prints no values |
| provider | a recording fake sees tokens and never raw values, for all six methods; outputs restored; an ActionItems quote containing PII passes `meeting::validate`; `MalformedEnvelope.raw` stays masked |
| field guard | §4.3's serde test over every output envelope |
| hidden folders | **canary test**: a note in a hidden folder carries a canary string; every AI command runs against a recording fake, which must never receive the canary (title, snippet, folder name or body); unparseable scope fails closed; `AiSource::Note` in a hidden folder refused |
| data_allowed | v0 file → `Some(true)` + v1; account added after migration refused, and its `None` materialized to `Some(false)` on next load; **marker lost after that (older binary rewrote the file) does not flip the new account to allowed**; failed save keeps old accounts allowed; vitest pins frontend `?? false` |
| presets | `file_access` table; argv badge derived from `PromptDelivery` |
| receipts | metrics present; no values, no tokens |

**Gate:** the CI set from CLAUDE.md (`cargo test --workspace`,
`node scripts/gen-changelog.mjs`, `npx vitest run`,
`npx svelte-check --threshold error`, `npm run build`).

**Live pass per PR, on the real app:**

- Create a note with synthetic PII.
- Run Extract and Ask through the `claude` preset. Its stdin is observable,
  so the outbound payload can be checked directly.
- Check that the receipt counts are right and the result is restored.

## 7. Delivery

Three PRs, **sequential, not stacked**, in this worktree. Each starts after
the previous one merges.

1. **Hidden folders → AI** (§4.4). A confirmed leak, independent of the rest;
   includes the `AiSource` IPC change.
2. **Detector + vault + wiring + receipt metrics** (§4.1–4.3).
3. **`data_allowed` opt-in + CLI badges** (§4.5–4.6).

Docs: `docs/ARCHITECTURE.md` gains the masking layer. `docs/ROADMAP.md`
gains two items: per-preset tool lockdown (measured) and NER for names and
addresses. **No new gotcha**: by this repo's rule a gotcha comes from a real
failure, and none has happened yet.

## 8. Out of scope

- Masking or encrypting what backends store.
- Names and addresses (NER).
- Disabling tools in CLI presets (D5 — badge only).
- A pre-send preview of the outbound payload.
- Text the user pastes by hand from a hidden note.
- Ingested web pages are masked like any other input. Detecting *prompt
  injection* in them is a separate problem.

## 9. As built (PR1)

PR1 (hidden folders, §4.4) shipped as designed except for the following.

1. **`suggest_wiki_links` takes no `AiSource`.** Its text is always the body of
   `new_note_uuid`, so it checks that note's folder through `check_source`
   instead. It now refuses before the provider is built. `autolink::suggest_links`
   keeps its own check as defense in depth.
2. **`ExtractError::Refused`** was added so a refusal displays without the
   `provider not configured:` prefix that `ExtractError::NotConfigured` carries.
3. **Filing filters hidden folders before #162/#164's blocked/offerable split**,
   so `OnlyBlockedFolders` never counts or names a hidden folder.
4. **`autolink::suggest_links` fails closed on a database error** while reading
   the source note. Only "note not found" proceeds.
5. **`ask_jodd` refuses a hidden folder scope before resolving the app
   provider**, because a folder can be hidden mid-conversation. `run_ask` keeps
   its own check.
6. **`notes_in_scope`** (the count Ask shows the user) still counts hidden notes.
   It is never sent to a provider.
7. **`src-tauri/examples/ask_jodd_probe.rs`** applies the real vault's scope.
8. **The teaching replay** (`tests/browser/teaching-replay.json`) was
   regenerated, because PR1 changed `llm/policy.rs` and `llm/provider.rs`
   (see `docs/TEACHING-DEMO.md`, "Recorded fixture provenance").

The final whole-branch review found paths this spec and its plan had missed.
They were fixed before merge:

9. **The tag vocabulary is scope-filtered.** Extract, every workflow and both
   append variants send the account's existing tags in the system prompt
   (roadmap #0). They read `list_all_tags`, unfiltered, so a tag carried only
   by a note in a hidden folder reached the provider even for pasted text.
   §3's inventory and the table above both missed this path. Those commands
   now use `AiScope::tags_visible_to_ai`, which keeps a tag only if it occurs
   on at least one note in an allowed folder (`Db::list_tags_with_labels`).
   The sidebar tag cloud keeps `list_all_tags`. The four commands share
   `ai_scope_for_source` (lib.rs), which refuses a hidden source and returns
   the scope the vocabulary is filtered by.
10. **`ingest_sources` takes a required `AiSource`.** In existing-note mode its
    links and context text come from the picked note's body. It checks the
    source before registering the in-flight entry or building a provider.
    `run_ingest_command` stays exempt from the tripwire because both of its
    callers check first (`ingest_sources` its `AiSource`, `re_extract_note` its
    stored-source note).
11. **`suggest_note_folder` refuses a hidden note before building the
    provider.** `filing::suggest_folder` keeps its own check as defense in
    depth.
12. **Hiding a folder invalidates AI policy.** `set_folder_hidden_from_agents`
    calls `invalidate_ai` under the policy gate once the scope file is
    written, as `update_llm_settings` does. In-flight Extract and Ask work ends
    ("AI permission or provider changed") and issued results expire.
13. **A dangling symlink at the scope path fails closed.** `AiScope::load_from`
    tests existence with `symlink_metadata`, so a link to nothing refuses
    instead of reading as "nothing hidden".
14. **The tripwire is stronger and has a stated reach.** It accepts
    `ai_scope_for_source(` as a scope load. It fails any provider-building
    function that reads `list_all_tags(`. It asserts that `ingest_sources`
    calls `check_source(` before `run_ingest_command(` and that
    `suggest_note_folder` calls it before `checked_account_provider(`. It
    scans only `lib.rs` and `curate_commands.rs`; a provider built anywhere
    else is not seen.

## 9b. As built (PR2)

PR2 (masking, §4.1–§4.3) shipped as designed except for the following.

1. **Exhaustive destructuring replaces the serde field-guard test.**
   `privacy/wire.rs` destructures every provider-facing type without `..`, so
   adding a field is a compile error until it is classified. That is a
   compile-time guarantee where the spec's test was a runtime one.
2. **`unmask` also restores tokens encoded by `meeting::literal`** (`&#95;`),
   and `Vault::reserving` therefore also reserves literal-encoded and glued
   plants. This closes a spoof the vault review found: a hostile page planting
   `⟦PASSWORD&#95;1⟧` so the reply would resolve to a real secret.
3. **`receipts::metric_add` was added.** `record_metrics` runs after
   unmasking, so `mask_unresolved` counts tokens the model invented.
4. **The detector is wider than §4.1's table**, after the review found leaks:
   - keywords use ASCII boundaries (Thai keywords skip the left boundary);
   - `_`/`-`-prefixed keys match (`DB_PASSWORD=`);
   - separators include quoted-JSON, `=>`, ` is ` and `คือ`, and quoted
     multi-word values are taken whole;
   - overlapping spans merge rather than drop;
   - phones accept `.`, `()` and comma lists;
   - Thai bank names count as bank keywords;
   - more key shapes: `sk_live_`, `AIza`, `GOCSPX-`, `gh*_`, JWT, `Bearer`;
   - matching uses `to_ascii_lowercase`, which keeps byte offsets stable;
   - scanning stays linear on the long-line inputs the tests probe. That is a
     tested property, not a proof: the final review found one O(k·n) path
     (M2 below).

   Accepted false hits: prose after ` is `, and leading-zero decimals read as
   phones. Masking over-hides rather than leaks, and the token round-trips.
5. **Organize's secret findings follow the wider detector**
   (`curate` is a wrapper over `privacy::detect`), so it raises more Hide
   proposals than before.
6. **API names differ from the spec's sketch:** `Vault::reserving`, `counts`,
   `unresolved` and `unmask(&mut self)`.
7. **Reservation also covers candidate `uuid` and digest `status`/`part`**, the
   fields of provider-facing types that are copied through unmasked.
8. **Not detected, as before:** names, addresses, and secrets with neither a
   keyword nor a known key shape. In HTML, a value in the next block element
   (the next line of `body_html`) is not found — the deferred "line after its
   keyword" case below. Also an accepted limit: when the label is itself
   marked up, so the text as written holds only markup after the separator,
   and any tag the view blanks (`<` followed by a letter, `/`, `!` or `?`)
   splits the value after at least four characters, only the part before
   the tag is masked — `<b>Password:</b> hunter<b>22x</b>` and
   `<b>Password:</b> hunter<x>22x` both send `22x`.
   Deferred: lowercase or 9-digit passports, Thai numerals, exotic e-mail local
   parts, and values on the line after their keyword.
9. **`test_llm_provider`'s no-account arm bypasses masking on purpose.** It
   sends only the fixed `CONNECTION_TEST_SAMPLE` (commented in `lib.rs`).
10. **The teaching replay** (`tests/browser/teaching-replay.json`) was
11. **"Downgrade is safe" (§4.5) holds from v0.29.0 on**, the first binary with `data_allowed` (08557aa). A pre-0.29 binary that rewrites `accounts.json` drops the field and the marker, and a new account would then load as allowed.
    regenerated, because PR2 changed `llm/meeting.rs` and `llm/policy.rs` —
    and again after the final-review fixes below changed both.

### Found by the final review

Each is fixed with a test that failed first, except M3 (documentation).

- **C1 — HTML/markdown labels.** Extract sends raw `body_html`, and the
  detector masked the markup after a label, not the value:
  `<b>Password:</b> hunter22x` masked `</b>`. The keyword-value rules now read
  a same-byte-length view with HTML tags (a closed element list) and
  `&nbsp;`/`&#160;` blanked to spaces, so spans map 1:1 back; `*`/`_`
  emphasis around the label is skipped; a lone tag is never a plausible
  value. When the view cuts a bare value below plausible (`a<b>&c99` → `a`),
  the value is read from the text as written.
  *Round 2 (re-review):* blanking whole tags hid keyword=value secrets inside
  attributes (`href="…?password=hunter22x"`), which 407c513 had masked, and
  cut `Pass<a>1234` / `abcd&nbsp;1234` short. Keyword values are now read in
  both the text as written and the view, and every hit is kept (overlaps
  merge; a value made only of tags is never plausible). Blanking is generic:
  any `<` followed by a letter, `/`, `!` or `?`, through the next `>` on the
  line, so Word's `<o:p>`, `<wbr>` and `<colgroup>` are markup too; `a < b`
  and `x<5` stay text.
- **C2 — a value found only by its context leaked wherever it repeated.**
  §4.2's premise, that re-masking the history each call keeps it consistent,
  is false for context-detected kinds: Ask stores the unmasked answer
  ("Use Tr0ub4dor to join…"), and the next turn has nothing to re-detect it
  by. Now `Vault::reserving` detects over every input first and `mask`
  replaces every verbatim repeat of a known value, longest first, only on an
  ASCII-alphanumeric boundary (Ask's uuid8 ids stay intact); the key is still
  `(kind, value)`. Across calls, a memory-only seed (`privacy::vault::Known`)
  lives on `CheckedProvider` — one per command, so the chunked transcript and
  ingest's map/synthesize share it — and on Ask's `Session`, cleared with the
  history. Never persisted or logged; `Debug` prints a count.
  *Round 3 (re-review):* reading the text as written (C1, round 2) widened a
  merged span to `Tr0ub4dor</div>`, which then became the only known value,
  so a bare repeat in HTML input leaked again. Every component hit
  (`detect::components`) is now registered as a known value alongside each
  merged span; the merged span is still masked in place as one token.
- **I1 — PIN/password phrasings.** One connector word (`code`, `no`, `no.`,
  `number`, `#`) may sit between `pin`/`password`/`passcode` and the
  separator; `pw` is a password keyword; `Bearer:` counts like `Bearer `.
- **I2 — armored PGP private keys.** The header needs `PRIVATE KEY`, so
  `-----BEGIN PGP PRIVATE KEY BLOCK-----` is masked as one block.
- **M1 — ActionItems evidence.** The evidence passage is rendered through
  `meeting::literal`, so a restored value comes back escaped (`a&lt;b…`), not
  as raw markup.
- **M2 — `private_keys` was O(k·n) on one line.** A header line without the
  marker is now skipped in one step (100 000 headers: 326 s → 0.27 s, debug).
- **M3 — receipt counts are per call, summed.** `masked_<kind>` is the number
  of values masked per provider call, summed over the step's calls; Ask's two
  calls count one address twice. Documented, not changed.

## 9c. As built (PR3)

PR3 (§4.5–§4.6) shipped as designed except for the following.

1. **No save on load.** The migration runs in memory on every load; `save_accounts` (`encode_accounts`) always writes `ai_consent_version: 1` and materializes every `None` as `Some(false)`. This replaces §4.5's "the version is set to 1 and the file saved", its "any remaining `None` … is saved", and §5's "save retried next load". jodd-mcp loads `accounts.json` in ten places and must stay a reader. A lost marker still cannot flip a new account: after one save it is `Some(false)`.
2. **The v0 rule is `Some(provider != Disabled)`**, stricter than §4.5's `Some(true)`. It is `preserve_permission`'s rule: a legacy account whose provider was `Disabled` migrates to denied.
3. **The consent refusal is its own string**, `AI_CONSENT_NEEDED`: "AI data access is not allowed for this account. Allow it to continue." It has no "yet": after one save a new account is indistinguishable from one the user denied, so both see it. `needs_consent` therefore covers an explicit `Some(false)`. The general message stays for an unavailable account or a `Disabled` provider.
4. **Where the allow is offered.** Inline in the Extract modal (extract, ingest, meeting actions), Ask Jodd (Account and Folder scope) and Organize; a hint in Settings → Test, whose form owns the checkbox; and one app-level in-DOM prompt (`AiConsentPrompt`) for Re-extract, Link into wiki and Suggest folder, because the note context menu has closed itself before its action fails and WKWebView drops native `confirm()`. Not offered: Ask "All accounts" when no account allows (no single account to name; its own message stays), and the automatic post-Extract folder suggestion (silent by design). An AI result applied after the permission was withdrawn is refused with the same text by `validate_ai_result`; only the Extract modal's meeting-actions apply offers the button there.
5. **`allow_ai_for_account` decides before it invalidates** (`policy::plan_allow`). `invalidate_ai` cancels every account's in-flight Extract and Ask, so an Allow that is refused (`Disabled` provider, unknown id) or changes nothing (already allowed) returns first. Pinned by a source-order test. The mutate/save step (`apply_allow`) restores the previous permission if `save_accounts` fails, so a retried Allow does not report success against a file that still denies.
6. **Organize names the refusal.** `curate_scan` reads it from the provider error (`is_consent_refusal`) and returns `ScanSummary.ai = "consent_needed"` with a consent note in place of "No AI provider is set up".
7. **thClaws is `Unknown`, not §4.6's `Disabled`.** Its own measured note says `Task` and `WorkflowRun` survive `--allowed-tools ""`, and a `Task` sub-agent's reach was never measured; under §4.6's own rule that is `Unknown`. Selecting thClaws shows both warnings. Roadmap #10 is the way to `Disabled`.
8. **The frontend/Rust mirror is pinned on each side separately.** vitest pins `?? false` (and the app scope sending `data_allowed: null`); Rust pins `None` → denied. No single test spans both.
9. **An HTTP provider pointed at a local agent server gets no warning** — `file_access` is per preset (§4.6's scope). Recorded in roadmap #10.
10. **The teaching replay** (`tests/browser/teaching-replay.json`) was regenerated, because PR3 changed `llm/policy.rs`.

### Known limits (PR3)

- An Allow that is already in effect (`AlreadyAllowed`) returns Ok without emitting `ai-policy-changed`; nothing changed, so nothing needs to re-read.
- `curate_scan`'s consent branch is inline in the command; only `ScanSummary`'s consent shape is covered by a unit test.
