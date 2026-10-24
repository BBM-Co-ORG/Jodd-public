# Agent-CLI LLM Providers — Design Spec

**Status:** Draft for review
**Author:** Brainstorming session 2026-07-27 (Kaiwan + Claude)
**Target version:** v0.19.0
**Estimated scope:** ~450 LoC Rust (of which ~180 is deleted duplication), ~150 LoC Svelte, one mechanical rename across ~93 references, no DB migration, no new crate deps

---

## 1. Problem

Jodd's Extract workflow supports exactly two LLM providers today:

- `HttpProvider` — any OpenAI-compatible endpoint (needs an API key, costs per token)
- `ClaudeCodeProvider` — shells out to `claude -p --output-format json`, inheriting the user's existing Claude Code subscription auth

The second one is the interesting shape: **an AI coding agent running in headless mode is a zero-extra-cost, zero-extra-credential LLM backend** for a user who already has one installed. Its auth, model selection, and billing are already solved outside Jodd.

But that shape is currently hardcoded to one binary. Users who run other agent CLIs — `gemini`, `codex`, `qwen`, `opencode`, `aider`, `cursor-agent` — cannot use them, even though every one of those tools has a headless/non-interactive mode that is text-in / text-out.

A second, smaller problem: `claude_code.rs` is 411 lines of which roughly 180 are near-verbatim duplication between `extract()` and `suggest_links()`. The two differ only in which system prompt they send and which type they deserialize. Adding N more agent CLIs by copying that struct would multiply the duplication by N.

A third: the module is still named after the single workflow it launched with. The trait `LessonProvider` today exposes `suggest_links()`, an auto-link operation with nothing to do with lessons, and sits in a file whose own types are already named `ExtractError` and `ExtractEnvelope`. See §5.4.

## 2. Non-goals

- **Streaming output.** Jodd's Extract modal is a single request/response with a spinner and a Cancel button; nothing in the UI consumes tokens incrementally. Streaming capability differences between CLIs are therefore invisible to Jodd and stay out of the `LlmProvider` contract. If token-by-token display is wanted later, it arrives as a new `extract_streaming` trait method with a default impl that falls back to `extract()`.
- **Multi-turn / session resumption.** Every call is one-shot. `--resume`, `--continue`, session ids are ignored.
- **Tool use by the spawned agent.** Jodd wants text in, JSON out. Presets deliberately pass the most restrictive read-only flag each CLI offers.
- **Process-group kill on cancel.** See §9 (known limitations).
- **Replacing `HttpProvider`.** It stays exactly as it is; this spec only adds a sibling.
- **Per-extraction provider choice.** Provider stays per-account, matching today's model.
- **Auto-discovery / auto-configuration of installed CLIs.** Jodd detects presence for display purposes only; the user always chooses explicitly.

## 3. Goals

1. Support agent CLIs beyond `claude` — `gemini`, `codex`, `qwen`, `opencode`, `aider` in the shipped preset table.
2. Support a CLI Jodd has never heard of, via a user-supplied **Custom** spec (binary + args + delivery + unwrap), so a flag change upstream does not require a Jodd release.
3. Collapse the extract/suggest_links duplication into one shared runner.
4. Existing `claude`-configured accounts keep working with **byte-identical** argv and zero `accounts.json` rewriting.
5. The call sites see no *shape* change at all — the abstraction stays uniform; they change only by the mechanical rename in §5.4.
6. Differences in output reliability between CLIs are expressed as **data in the preset table**, surfaced to the user before they choose, not as branches scattered through the code.
7. A misconfigured Custom spec is diagnosable in seconds, not in 120-second timeout cycles.
8. Internal naming matches what the code actually does (§5.4).

## 4. Architecture overview

```
extract_note (lib.rs)             autolink::suggest_links
        │                                   │
        └──────────► Box<dyn LlmProvider> ◄────────────┘     ← unchanged surface
                            │
              ┌─────────────┴─────────────┐
              ▼                           ▼
        HttpProvider              AgentCliProvider          ← NEW (replaces ClaudeCodeProvider)
        (unchanged)                       │
                                          ▼
                                   run_json::<T>()          ← the ONE place CLI differences live
                                          │
                    ┌─────────────────────┼─────────────────────┐
                    ▼                     ▼                     ▼
              PromptDelivery        OutputSource            unwrap: Option<String>
              (stdin/argv)        (stdout / file)           (JSON field to dig into)
                                          │
                                          ▼
                              strip_ansi → parse_envelope_lenient::<T>
```

`AgentCliSpec` is the whole of the variation. Presets are rows in a const table; **Custom** is a user-authored row of the same type. There is no per-CLI struct.

## 5. Data model

### 5.1 `accounts.rs`

```rust
pub enum LlmProviderKind {
    #[default] None,
    ClaudeCode,   // RETAINED for back-compat; see §5.3
    Http,
    AgentCli,     // NEW
}

pub struct LlmConfig {
    pub provider: LlmProviderKind,
    pub http_base_url: Option<String>,
    pub http_model: Option<String>,
    pub http_api_key_keychain: Option<String>,
    #[serde(default)] pub agent_preset: Option<String>,       // NEW — preset id, or "custom"
    #[serde(default)] pub agent_custom: Option<AgentCliSpec>, // NEW — used iff agent_preset == "custom"
}
```

### 5.2 `llm/agent_cli.rs` (new module)

`AgentCliSpec` derives `Serialize`/`Deserialize` — the same type is both a const-table row and a user-authored value persisted inside `accounts.json`.

```rust
pub struct AgentCliSpec {
    pub binary: String,                  // "gemini", or an absolute path
    pub args: Vec<String>,               // placeholders: {system} {prompt} {out_file}
    pub prompt_delivery: PromptDelivery,
    pub output: OutputSource,
    pub unwrap: Option<String>,          // JSON field holding the model text; None = stdout is the text
    pub fidelity: OutputFidelity,
    pub timeout_secs: u64,               // default 120, matching today's ClaudeCodeProvider
}

pub enum PromptDelivery {
    StdinAll,               // system + payload → stdin; no prompt placeholder in args
    StdinPayloadSystemArg,  // system → {system} in args; payload → stdin
    Argv,                   // system + payload → {prompt} in args; nothing on stdin
}

pub enum OutputSource {
    Stdout,
    LastMessageFile,        // Jodd creates a temp path, substitutes {out_file}, reads it after exit
}

pub enum OutputFidelity {
    Structured,  // CLI has a real JSON output mode — model text location is known exactly
    Heuristic,   // raw stdout — JSON must be dug out of prose/ANSI; see §7.4 retry
}
```

**Why `args: Vec<String>` and not a single command string.** Args go straight into `Command::args()`, never through a shell. This removes cross-platform quoting differences (Windows is Jodd's primary target and has no `sh -c`) and removes shell-injection surface from user-editable config. It is the reason the "one command line string executed via `sh -c`" approach was rejected during brainstorming.

**Why `PromptDelivery` has three variants instead of an implicit rule.** `gemini` and `qwen` refuse to run non-interactively without `-p`, so their system prompt must go in argv — but the user's pasted source can be hundreds of KB and **Windows caps a command line at ~32,767 characters**. Splitting system-prompt-to-argv from payload-to-stdin is therefore mandatory for those two, and making it a named variant keeps `Argv` (the only variant that puts unbounded text on the command line) visible in the type rather than hidden in a substitution rule.

### 5.3 Back-compat: derive, don't migrate

`accounts.json` files in the wild contain `"provider": "claude_code"`. The `ClaudeCode` variant is **retained**, and `resolve_provider` maps it to the `claude` preset at read time. No file is rewritten, no migration runs, and downgrading to an older Jodd build keeps working.

This follows the existing doctrine recorded in CLAUDE.md edge #6 ("derive, don't migrate"): the truth lives in the preset table and is re-derived on every resolve.

A golden test asserts the derived argv equals today's hardcoded `["-p", "--output-format", "json"]`.

### 5.4 Naming migration

CLAUDE.md records a deliberate decision to keep the internal `lessons` naming after the user-facing rename to "Extract", for code-churn minimization. **This spec reverses that decision**, for a reason that did not exist when it was made: the module now hosts a second workflow. `LessonProvider::suggest_links()` is an auto-link operation with nothing to do with lessons, and the file's own types are already `ExtractError` and `ExtractEnvelope` — the vocabulary is half-migrated today, and this spec adds a six-preset provider layer underneath a name that claims a single purpose. Churn cost is fixed and grows with the codebase; wrong-name cost compounds with every workflow added, and CLAUDE.md's roadmap plans more (Summarize, Extract Action Items).

| from | to | refs | notes |
|---|---|---|---|
| trait `LessonProvider` | `LlmProvider` | 13 | Rust-only; the compiler finds every site |
| module `lessons` | `llm` | 48 | Rust-only; directory rename + `mod` declaration |
| command `extract_lessons` | `extract_note` | 32 | crosses the IPC boundary — Rust `#[tauri::command]` and the `invoke(...)` string in Svelte must change in the same commit |
| command `append_extract_lessons` | `append_extract_note` | (within the 32) | same IPC constraint; `lib.rs:3660`, called from `LessonExtractModal.svelte` |
| command `re_extract_lessons` | `re_extract_note` | (within the 32) | same IPC constraint; `lib.rs:3946`, called from `NoteContextMenu.svelte` |

All three commands must be renamed together with their `generate_handler!` entries and their `invoke(...)` call sites; a missed pair is a runtime "command not found", not a compile error — which is exactly why phase 0 ends with a live smoke test of all three, not just `cargo test`.

`extract_note` (not `extract_text`) follows the codebase's established `verb_noun` command convention where the noun is the thing produced or acted on — `save_note`, `delete_note`, `refetch_note`, `move_notes_batch`. The command produces one note.

**No persisted data references any of these names.** `accounts.json` stores provider *kind* values (`claude_code`, `http`), not Rust type or command names, and those string values are unchanged by this section — §5.3's back-compat guarantee is unaffected. The rename is therefore compile-time-only, with the single runtime-visible edge being the IPC command string, which is why Rust and Svelte must move together.

`LessonProvider` is **not** kept as a deprecated alias. A type alias would leave the misleading name discoverable by grep and by autocomplete, which is the entire cost being paid to remove.

## 6. Preset table

Shipped presets. **Every row except `claude` carries flag/field values that MUST be verified against the installed CLI before commit** (see §8.3); the table below is the starting hypothesis, not a verified fact.

| id | label | binary | args (starting hypothesis) | delivery | output | unwrap | fidelity |
|---|---|---|---|---|---|---|---|
| `claude` | Claude Code | `claude` | `-p --output-format json` | StdinAll | Stdout | `result` | Structured |
| `gemini` | Gemini CLI | `gemini` | `-o json --approval-mode plan -p {system}` | StdinPayloadSystemArg | Stdout | `response` | Structured |
| `qwen` | Qwen Code | `qwen` | `-o json --approval-mode plan -p {system}` | StdinPayloadSystemArg | Stdout | `response` | Structured |
| `codex` | Codex CLI | `codex` | `exec --sandbox read-only --output-last-message {out_file} -` | StdinAll | LastMessageFile | – | Structured |
| `opencode` | opencode | `opencode` | `run --agent plan {prompt}` | Argv | Stdout | – | Heuristic |
| `aider` | Aider | `aider` | `--no-auto-commits --message {prompt}` | Argv | Stdout | – | Heuristic |

Confidence notes carried into implementation:

- `claude` — verified in production since v0.16.1. Unchanged.
- `gemini`, `qwen` — `--help` on the installed versions confirms `-p` ("Appended to input on stdin (if any)"), `-o/--output-format json`, and `--approval-mode plan`. The **JSON field name (`response`) is unverified** and is the single most likely thing to be wrong.
- `codex` — every cell is unverified as of writing, but the binary was found on the development machine's PATH (confirmed 2026-07-28), so this row must be exercised for real during implementation rather than shipped unverified. (An earlier draft of this spec said codex was absent; that was true when first probed and is no longer.)
- `opencode` — `run --format json` emits a raw JSON *event stream*, not a simple wrapper, so the default (human) format plus heuristic extraction is the intended path.
- `aider` — unverified; `--message` + `--no-auto-commits` is the hypothesis.

Every preset passes the most restrictive tool-permission flag the CLI offers (`--approval-mode plan`, `--sandbox read-only`, `--agent plan`, `--no-auto-commits`). Jodd is spawning a coding agent with filesystem access to do a pure text transformation; nothing about this task requires the agent to touch the user's disk.

## 7. Runtime behavior

### 7.1 `run_json::<T>()`

Shared by `extract()` and `suggest_links()`; the only difference between the two call sites is the system prompt and the type parameter.

1. Substitute placeholders into `args` (`{system}`, `{prompt}`, `{out_file}`; unknown placeholders are left alone).
2. Spawn with stdin/stdout/stderr piped.
3. Write the stdin payload per `PromptDelivery`, then drop stdin (EOF is what makes these CLIs start work).
4. `tokio::select!` with `biased`, racing `cancel.cancelled()` against `timeout(child.wait())` — lifted unchanged from today's `ClaudeCodeProvider`.
5. Drain stdout and stderr.
6. Non-zero exit → `UpstreamError`.
7. Obtain raw text: `Stdout` → captured stdout; `LastMessageFile` → read the temp file, then delete it.
8. `strip_ansi` — agent CLIs emit colour and spinner escapes on stdout.
9. If `unwrap == Some(field)`: parse as JSON, take that field as a string. Missing/non-string field → `MalformedEnvelope`.
10. `parse_envelope_lenient::<T>` — the existing lenient parser (direct → strip code fence → brace-balanced slice), moved here from `claude_code.rs` unchanged.

### 7.2 Error mapping

| Condition | Error | UI consequence |
|---|---|---|
| binary not on PATH | `NotConfigured("<bin> not found in PATH")` | existing "open Account Settings" prompt |
| exit ≠ 0 | `UpstreamError` + **last ~2000 chars of stderr** | toast; the tail cap exists because agent CLIs log heavily |
| timeout | `Transport`; if stdout was empty, append the hint *"the CLI may have been waiting for interactive input — check its headless flags"* | toast |
| user cancelled | `Cancelled` + `start_kill` | **no fallback note is created** (existing v0.16.2 doctrine: the user chose to abort, they did not lose a paste) |
| JSON unextractable | `MalformedEnvelope { reason, raw }` | source-preservation fallback note is created (existing doctrine: a paste is never lost) |

### 7.3 Uniformity of the abstraction

Call sites see `Box<dyn LlmProvider>` with two methods, typed input and typed output. No caller learns which CLI is behind it. Differences in JSON support, streaming support, and flag syntax are absorbed entirely inside `run_json` and expressed as table data.

### 7.4 Fidelity-driven retry

The one place fidelity changes behavior: if a `Heuristic` preset fails JSON extraction on the first attempt, `run_json` retries **once**, appending an instruction to answer with raw JSON only and no surrounding prose. `Structured` presets never retry — for them a parse failure means something is genuinely wrong, and silently doubling latency would hide it.

## 8. UI

### 8.1 `list_agent_cli_presets` command

```rust
#[tauri::command]
fn list_agent_cli_presets() -> Vec<AgentCliPresetInfo>
// { id, label, fidelity, installed: bool, resolved_path: Option<String> }
```

`installed` comes from a live `which::which(binary)` each time settings opens — not cached, because a user may install a CLI while Jodd is running and `which` is cheap.

The preset list exists **only in Rust**. `LlmProviderSettings.svelte` currently hardcodes its own `'none' | 'claude_code' | 'http'` union; after this change the dropdown is generated from the command's response, so adding a preset is a one-row Rust edit.

### 8.2 Settings panel

```
LLM Provider
  ○ Agent CLI (headless)                      ← replaces the "Claude Code" radio
      [ dropdown, generated from list_agent_cli_presets ]
        Claude Code        ✓ available
        Gemini CLI         ✓ available
        Qwen Code          ✓ available
        opencode           ✓ · no JSON mode
        Codex CLI          ✗ not found
        Custom…
      [ Test connection ]
  ○ Custom endpoint (OpenAI-compatible)       ← unchanged
  ○ Disabled
```

Selecting **Custom…** reveals: binary, args (**a textarea, one argument per line** — not a space-separated field, which would reintroduce the quoting problem that ruled out the shell-string approach), prompt delivery, unwrap field, timeout.

Presets reporting `installed: false` remain selectable — the user may be about to install one — but are badged. Using one unconfigured yields `NotConfigured`, which the UI already handles.

### 8.3 `test_llm_provider` command

```rust
#[tauri::command]
async fn test_llm_provider(account_id: String) -> Result<TestResult, String>
// { ok, elapsed_ms, error: Option<String>, raw_head: String }  // raw_head = first ~500 chars
```

Sends a small fixed prompt that asks for a trivial envelope, and reports success, elapsed time, error, and the head of the raw output. It resolves the account's configured provider through the existing `resolve_provider`, so it also works for `Http` accounts — a free side benefit, not a separate code path.

This is the highest-value part of the UI work. The dominant failure mode for a Custom spec is *wrong flags → the CLI enters interactive mode and blocks*, which without this button presents to the user as "spun for 120 seconds, then failed" — after they have already pasted real source text. A short fixed-prompt round trip turns each config iteration into seconds. The same button is the instrument used to verify the preset table in §6, so one tool serves both the user and the implementer.

**Safety.** `run_json` and `test_llm_provider` always use `Command::args()`; user config becomes argv directly and is never re-parsed by a shell. Raw output shown in settings is rendered as plain text in a `<pre>`, never through the note editor's HTML path — it is data, not markup and not instructions.

## 9. Known limitations

- **`start_kill` kills only the direct child.** An agent CLI implemented in Node may spawn grandchildren that survive cancellation and keep consuming the user's subscription quota. This is the pre-existing behavior of `ClaudeCodeProvider`, not a regression introduced here. Process-group kill is deliberately out of scope for this round.
- **Preset flags will rot.** Upstream CLIs change flags frequently. This is mitigated, not solved, by the Custom escape hatch: a user hit by a flag change can fix their own config without waiting for a Jodd release.
- **`Heuristic` presets are best-effort.** `opencode` and `aider` were never designed to emit machine-readable output for a third party. Extraction may fail more often; the single retry and the source-preservation fallback note bound the damage.
- Any preset row that cannot be exercised ships marked `unverified` (see §6). As of 2026-07-28 all six CLIs are installed, so no row is expected to need this.

## 10. Testing strategy

### 10.1 Pure unit tests

- Placeholder substitution: `{system}`, `{prompt}`, `{out_file}`, and the no-placeholder case.
- `strip_ansi`: colours, cursor movement, and clean text passing through untouched.
- Unwrap: field present, field missing → `MalformedEnvelope`, `None` → passthrough.
- **Preset table invariants**, asserted across the whole table:
  - `delivery == Argv` ⟹ `{prompt}` appears in args
  - `delivery == StdinPayloadSystemArg` ⟹ `{system}` appears in args
  - `output == LastMessageFile` ⟹ `{out_file}` appears in args
  - all ids unique
- **Back-compat golden test:** `LlmProviderKind::ClaudeCode` resolves to argv exactly equal to today's `["-p", "--output-format", "json"]`.

### 10.2 Integration tests with a fake CLI

A fixture helper writes a throwaway script into a temp dir (`.sh` on unix, `.cmd` on Windows) and points `binary` at it, giving deterministic, offline, both-platform coverage without any real agent CLI installed.

| Fixture behavior | Asserts |
|---|---|
| stdout = JSON wrapper | `Structured` happy path |
| stdout = prose + ANSI around JSON | `Heuristic` happy path |
| exit 1 with stderr | `UpstreamError` carries the stderr tail |
| sleeps past the timeout | `Transport` timeout |
| sleeps, cancelled mid-run | `Cancelled`, **and the process is actually dead** |
| writes to `{out_file}` | `LastMessageFile` path reads it and cleans up |
| prose on call 1, JSON on call 2 (counter file) | `Heuristic` retries once and succeeds; `Structured` invokes exactly once |

### 10.3 Live verification (not in CI)

Each installed CLI is exercised for real via the Test button, and the observed output shape is pasted into a code comment beside its preset row, matching the existing `Empirical note (verified 2026-06-13, claude 1.0.24)` convention. Any CLI that cannot be exercised is marked `unverified` in both the comment and §6 — never presented as tested.

### 10.4 Regression

The existing `claude` path is covered by the golden test plus one live extraction run before merge.

## 11. Implementation phases

**Coordination note (decided 2026-07-27).** A parallel workstream, the Extract input router design (`docs/superpowers/specs/2026-07-27-extract-input-router-design.md`, authored on a parallel branch and not present on this one), targets the same module (it adds `classify.rs` and edits `LlmProviderSettings.svelte`). The two designs do not conflict conceptually — that spec routes incoming text, this one chooses who processes it — but they edit the same files, and phase 0's 93-reference rename conflicts with anything touching the module. **Phase 0 lands and merges first**, while `classify.rs` does not yet exist, so the router work simply adopts the new names instead of rebasing across a rename. The router spec's `append_extract_lessons` references become `append_extract_note`.

0. **Rename only** (§5.4), in three commits — trait, then module, then command. Nothing else changes; the existing test suite passes **unmodified** at each step. Kept first and separate so that a rename touching 93 references never shares a commit with a logic change, leaving `git bisect` useful for everything after it.
1. **Pure refactor, no behavior change.** Move `parse_envelope_lenient` and the runner out of `claude_code.rs` into `agent_cli.rs`; make `claude` the first preset row. The existing test suite must pass **unmodified**. (Same discipline as the Vertical #0 extraction from `gmail.rs`.) Separating this commit means that if `claude` extraction breaks after this work lands, `git bisect` distinguishes "the restructure" from "the new features" immediately.
2. Config schema (`AgentCli` variant, new `LlmConfig` fields) + `resolve_provider` + back-compat shim.
3. Preset table + live verification + recorded empirical notes.
4. UI: `list_agent_cli_presets`, generated dropdown, Custom form.
5. `test_llm_provider` command + Test connection button.
6. `OutputFidelity` marker + `Heuristic` single retry.
7. Docs: **CLAUDE.md edge #4 currently states "LLM provider abstraction is single-purpose"** — that stops being true with this change and must be updated in the same branch, along with the "Key files to understand" module map, the `src-tauri/src/lessons/` entry in the project-structure tree, and the Content Extraction section's paragraph recording the now-reversed decision to keep the `lessons` naming (§5.4). Documentation that contradicts the code is worse than no documentation.

## 12. Future work

- Streaming (`extract_streaming` with a default fallback impl) if the Extract modal ever shows live output.
- Process-group kill on cancel.
- Per-workflow provider override, once workflows beyond Extract exist (see CLAUDE.md edge #4).
- Sharing the agent-CLI runner with future non-Extract LLM workflows — it is workflow-agnostic by construction, since the system prompt is a parameter.
