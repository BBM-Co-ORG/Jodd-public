# Headless agent-CLI efficiency — how the mechanism works, per CLI, and what Jodd should do

**Date:** 2026-08-29
**Status:** spec / reference. Plan lives at
[docs/superpowers/plans/2026-08-29-headless-agent-cli-efficiency.md](../plans/2026-08-29-headless-agent-cli-efficiency.md).
**Scope:** `src-tauri/src/llm/agent_cli.rs`, `src-tauri/src/llm/presets.rs`, and
the three call sites in `lib.rs` / `ask/run.rs` / `llm/autolink.rs`.

Every number and every flag in this document was read off the binaries on this
machine on 2026-08-29, per the Empirical-note discipline gotcha #7 sets for the
preset table. Where something could **not** be measured, it says so; those lines
are the ones a future reader must not promote to fact.

---

## 1. What "headless" costs, in general

A headless agent CLI is not an HTTP client with a different syntax. It is the
*same program* as the interactive tool, started fresh, and it does its whole
startup routine before your prompt is looked at. Four costs, in the order they
are paid:

### 1.1 Process cold start + auto-discovery

The CLI walks its config hierarchy: user settings, project settings, memory
files, hooks, skills, plugins, subagents, MCP registrations. None of that is
free, and almost none of it is what a programmatic caller wants.

**Measured, this machine, `claude -p` from an empty tempdir:**

| run | wall clock | `duration_api_ms` | `duration_ms` |
|---|---|---|---|
| first (cold FS cache) | **6.31 s** | 0 | 75 |
| second (warm) | **3.01 s** | 0 | 67 |

The run terminated at authentication, so the API was never reached — which is
exactly what makes the number useful: **~3 s elapsed before the first API byte
would have been sent.** That is pure startup. It is a floor, not the full cost:
a run that got past auth would also pay MCP handshakes and prompt-cache misses
on top.

### 1.2 The MCP handshake

Each registered MCP server costs twice: connection time at startup, and its tool
schemas occupying context on *every* turn. A programmatic caller that needs no
tools pays both for nothing.

### 1.3 Prompt-cache miss

A new process is a new conversation is a new prefix. The system prompt and tool
schemas — usually the largest single block — get no cache hit. Two one-shot
calls pay this twice; a resumed session pays it once **only if the second call's
prefix genuinely extends the first's**. See §4.2 for why that condition fails in
Jodd's case.

### 1.4 Tool round trips

An agent left holding Read/Grep/Bash will use them. Every exploratory turn is a
model round trip that a caller who already has the context could have skipped by
putting it on stdin.

### 1.5 The four levers, stated CLI-neutrally

Everything below is a variation on these:

1. **Isolate** — refuse the ambient config the CLI would otherwise discover.
2. **Disarm** — take away tools the task does not need.
3. **Pin the contract** — demand structured output instead of parsing prose.
4. **Feed, don't fetch** — hand over context rather than letting the agent hunt.

Two further levers exist but are *not* CLI-neutral and must be decided per
product: **reuse the session** (§4.2) and **route the model** (§4.3).

---

## 2. Where the CLIs differ — the part that does not generalise

Every CLI has an isolation flag. They have different names, **different scope**,
and — critically — **different consequences for authentication**. Read off
`--help` on 2026-08-29:

| CLI | version | isolation flag | what it actually strips | auth survives? |
|---|---|---|---|---|
| **claude** | 2.1.246 | `--bare` | hooks, LSP, plugin sync, attribution, auto-memory, background prefetches, **keychain reads**, CLAUDE.md discovery; sets `CLAUDE_CODE_SIMPLE=1` | **NO** — "OAuth and keychain are never read"; requires `ANTHROPIC_API_KEY` |
| **claude** | 2.1.246 | `--setting-sources` + `--strict-mcp-config` + `--tools` | the same ground, granularly | **yes** |
| **codex** | 0.147.0 | `--ignore-user-config` | `$CODEX_HOME/config.toml` (which is also where `mcp_servers` lives) | **yes** — help states "auth still uses `CODEX_HOME`" |
| **codex** | 0.147.0 | `--ephemeral` | writing session files to disk | yes |
| **qwen** | 0.21.0 | `--safe-mode` | context files, hooks, extensions, skills, MCP servers | **unknown — not stated in help** |
| **opencode** | 1.3.13 | `--pure` | external plugins **only** (narrower than the others) | yes |
| **cursor-agent** | 2026.07.23 | none found | — | — |

**This table is the whole reason a single "headless best practice" cannot be
copy-pasted across the table.** `claude --bare` and `codex --ignore-user-config`
sound like the same flag and are not: one silently moves the workload off the
user's subscription onto an API key, the other explicitly guarantees it does
not.

### 2.1 Structured output

| CLI | how | Jodd's parse |
|---|---|---|
| claude | `--output-format json` → object, text in `result`; `--json-schema <schema>` validates the final shape | `unwrap: Some("result")` |
| codex | `--output-schema <FILE>` (a JSON Schema **file**); `-o/--output-last-message <FILE>` writes only the final message | `OutputSource::LastMessageFile`, `unwrap: None` |
| qwen | `-o json` → a JSON **array** of events | `unwrap: Some("result")` + array search |
| opencode | `--format json` → raw event stream, no single wrapper | deliberately unused; plain stdout is already clean when piped |

Note the shape difference: claude takes the schema **inline**, codex takes a
**file path**. Any Jodd-side abstraction has to express both.

### 2.2 Session reuse

`claude --resume/--continue` · `codex exec resume [--last]` · `qwen -c/-r` ·
`opencode -c/-s/--fork` · `cursor-agent --resume/--continue`. Universally
available, universally *not* automatic.

### 2.3 Sandboxing and model selection

Sandbox: `codex -s read-only` (Jodd already sends this), `qwen -s`,
`cursor-agent --sandbox`. claude has no sandbox flag; it has `--tools` and
`--permission-mode` instead.

Model: `claude --model` · `codex -m` · `qwen -m` · `opencode -m provider/model`
· `cursor-agent --model`. All present, all meaning slightly different things.

---

## 3. What Jodd does today

`AgentCliProvider::run_once` ([agent_cli.rs:342](../../../src-tauri/src/llm/agent_cli.rs))
builds one process per call, in a fresh `tempfile::tempdir()`, and waits.

```
claude   -p --output-format json                                    ← no isolation at all
codex    exec --sandbox read-only --skip-git-repo-check
              --output-last-message {out_file} -                    ← already good
qwen     -o json --approval-mode plan -p {system}
opencode run {prompt}
aider    --no-auto-commits --yes --message {prompt}
```

### 3.1 What is already right, and should not be "improved"

- **The scratch cwd.** `.current_dir(scratch.path())` kills *project*-scope
  discovery and, per the comment there, a real macOS TCC bug where the prompt
  was attributed to Jodd. Keep.
- **codex's row.** `--sandbox read-only` + `--skip-git-repo-check` +
  `--output-last-message` is three of the four levers already applied.
- **Context is fed, not fetched.** The whole prompt goes in on stdin. §1.4's
  first half is already satisfied.
- **`fidelity`'s single retry** is scoped to `Heuristic` presets only;
  `Structured` never retries. Correct as-is.
- **Kill-on-timeout and kill-on-cancel** both `start_kill()` then `wait()`.
  Correct — a dropped timeout future leaves the child burning the user's quota.

### 3.2 The gaps

| # | Gap | Where |
|---|---|---|
| G1 | `claude` row sends **no** isolation flag, so it inherits user scope: 31 KB `settings.json`, 3 SessionStart hooks (one shelling out to `jcode setup-hotkey`), 21 skills, `enabledPlugins`, and `jodd-mcp` registered at the top level of `~/.claude.json` | `presets.rs:44` |
| G2 | No `--tools`/equivalent anywhere: every agent holds Read/Grep/Bash it cannot usefully aim at an empty tempdir | all rows |
| G3 | No structured-output schema, though `ExtractEnvelope` is a fixed shape and both claude and codex can enforce it | all rows |
| G4 | codex writes session files to disk on every call (`--ephemeral` unused) and reads `~/.codex/config.toml` (`--ignore-user-config` unused, and **safe** here) | `presets.rs:62` |
| G5 | Ask Jodd spends **two** full cold starts per question | `ask/run.rs:68`, `:104` |
| G6 | The `gemini` row's user-facing text asserts the CLI is installed on this machine. It is not (`gemini` is absent from PATH as of 2026-08-29) | `presets.rs` |

### 3.3 Two claims from the earlier review that did NOT survive measurement

Recorded because the correction is the useful part.

- **"Every Extract/Ask Jodd spawn starts a `jodd-mcp` process."** *Unproven.*
  With a corrected detector, `jodd-mcp` was observed spawning on **no** variant
  — not even the no-flags baseline. The run dies at OAuth before MCP servers
  would start, so this cannot be settled until a working session exists. What
  remains true is the *config* fact: `jodd-mcp` is registered at user scope, so
  nothing in Jodd's invocation excludes it. `--strict-mcp-config` stays worth
  sending as defence in depth, with **unknown** measured payoff.
- **The first detector reported "spawned: yes" for all four variants,
  `--bare` included** — because `pgrep -f jodd-mcp` matched the probe script's
  own argv. Splitting the literal (`PAT="jodd""-mcp"`) and running a control
  that spawns nothing at all flipped every answer to "no". *An instrument that
  cannot fail its own control is not measuring the thing it names* — the same
  lesson `census::field_shape` and gotcha #25 already record.

### 3.3.1 The measurement harness, run 2026-08-29 — still not closable

Task 6 built `scripts/measure-headless.sh` (three invocations on one prompt:
as-shipped, isolated, and a repeat of the isolated variant as a control) and
ran it on this machine, `claude 2.1.246`, `2026-08-29 04:50 UTC`. Real output,
verbatim:

```
claude: as shipped before this plan        wall=  3.23s api_ms=0 in=0 out=0
claude: isolated (this plan)               wall=  1.16s api_ms=0 in=0 out=0
claude: isolated, repeat                   wall=  1.15s api_ms=0 in=0 out=0
```

(A second full run a few minutes later reproduced the same shape: 3.2–3.8s
for the as-shipped row, 1.15–1.16s for both isolated rows, every time. `in`/
`out` read `0` rather than the brief's predicted `?` because the isolated
run's JSON *does* parse — `usage` is present with every counter at `0`, not
absent — so the harness's own `except: '?'` branch is never reached. The
value stayed as informative as `?` would have been.)

The raw stdout behind the isolated row, captured separately (not part of the
harness's own tail-1 output, shown here to explain the numbers above):

```json
{"is_error":true,"duration_api_ms":0,"num_turns":1,"stop_reason":"stop_sequence",
"session_id":"...","total_cost_usd":0,"usage":{...all counters 0...},
"result":"Failed to authenticate: OAuth session expired and could not be refreshed",
"type":"result","duration_ms":60,...}
```

**What this does and does not show.** The control does its one job: the two
isolated rows agree with each other to the hundredth of a second (1.16s,
1.15s), so the harness is measuring something real and repeatable, not noise
— and the isolated rows are consistently faster than the as-shipped row (by
roughly 2–2.6s), which is directionally consistent with the isolation flags
skipping settings/hook/skill/plugin/MCP discovery. **But every run dies at
`Failed to authenticate: OAuth session expired and could not be refreshed`
with `duration_api_ms: 0` before a single byte reaches the API.** The gap
between the as-shipped and isolated rows is therefore a difference in
**local discovery cost only** — it says nothing about MCP handshake time,
prompt-cache behavior, `--tools ''` actually suppressing tool use mid-turn, or
whether `--json-schema` changes the shape of a real answer. None of the three
Empirical notes this task exists to close ("ARGUMENT PARSING VERIFIED ONLY" /
"HELP-TEXT VERIFIED ONLY" in `presets.rs`) can be promoted on the strength of
this run, and none were touched.

**What a future authenticated run would need to show, per note:**

- **`presets.rs` claude row, isolation flags** (`--strict-mcp-config
  --setting-sources '' --tools ''`): a successful `result` string (not an
  auth failure), `duration_api_ms > 0`, and — to confirm `--tools ''` actually
  disarms the agent rather than merely being accepted — a prompt that would
  tempt a real agent into using Read/Grep/Bash (e.g. "list the files in this
  directory") answered without any tool-use turns appearing in the
  transcript/`num_turns`.
- **`presets.rs` claude row, `--json-schema`**: a successful run with a real
  schema (e.g. `ExtractEnvelope`'s) and `stop_reason`/`result` showing the
  output actually conforms — not just that the flag was accepted.
- **`presets.rs` codex row, `--ignore-user-config` / `--ephemeral`**: a
  successful `codex exec` run whose `--output-last-message` file contains the
  real answer, plus confirmation that no session file was written under
  `~/.codex/sessions` (or wherever a non-ephemeral run would have put one) and
  that `~/.codex/config.toml`'s `mcp_servers` did not get consulted.
  `scripts/measure-headless.sh` does not exercise codex yet — the brief scopes
  it to claude only, since claude is what Tasks 1 and 4 touched most and what
  this machine can run without an extra binary check; extending the harness
  to codex, following the same wall-clock/`api_ms` pattern against
  `--output-last-message`'s file instead of stdout, is the natural next step
  once a session can authenticate at all.

Until one of those runs happens, the honest status of every isolation and
schema flag added by Tasks 1–5 is unchanged: accepted by the real binary,
effect unmeasured.

---

### 3.4 Live results — codex, 2026-08-29 (authenticated)

`claude` could not authenticate on this machine, but **`codex` could**, so
the codex row's flags were exercised end to end rather than left at
help-text confidence.

| what was run | result |
|---|---|
| `codex exec --ignore-user-config --ephemeral --sandbox read-only --skip-git-repo-check --output-last-message <f> -` | **exit 0**, answer written to the file. Auth survived, exactly as `--ignore-user-config`'s help promises. |
| the same, plus `--output-schema` with Jodd's schema **as it was first written** | **exit 1, EMPTY last-message file** |
| the same, with the corrected schema | **exit 0**, valid `ExtractEnvelope` returned |

**The middle row is a defect this work introduced and shipped no further
than `main`.** `codex exec --output-schema` does not take an arbitrary JSON
Schema — it enforces OpenAI's strict structured-output rules and refuses
before the model is called:

```
400 invalid_json_schema — "Invalid schema for response_format
'codex_output_schema': 'additionalProperties' is required to be supplied
and to be false."
```

codex then exits 1 with **nothing** in the output file, and the `codex` row
is `OutputFidelity::Structured`, so there is no lenient retry to fall back
on. Every Extract and every auto-link on a codex account would have failed.

Two rules follow, both now pinned by `assert_strict_structured_output` in
`provider.rs`: every object carries `"additionalProperties": false`, and
**every** property is listed in `required`, with optionality expressed in
the type (`["string", "null"]`) rather than by omission. That still
round-trips into `Option<_>`, because serde reads an explicit `null` as
`None`.

**This reverses a rule §4 and the plan both asserted** — "required only when
the field is neither `Option<_>` nor `#[serde(default)]`". That rule was
adopted because a reviewer proposed it and it read as principled. It was
never run. The whole point of §3.3 is that argument parsing is not
behaviour, and the first flag anyone actually executed proved it.

**Still open:** the `claude` row's `--strict-mcp-config`,
`--setting-sources ''`, `--tools ''` and `--json-schema` remain
argument-parsing-verified only. codex's result raises rather than lowers
the priority of closing them — one of the two schema-capable CLIs turned
out to reject the obvious schema shape outright.

### 3.5 Live results — claude, 2026-08-30 (authenticated). Every note is now closed.

The session that could not authenticate on 2026-08-29 could on 2026-08-30, so
the `claude` row — Jodd's DEFAULT preset, and the one whose failure would
reach the most users — was finally exercised rather than argued about.

| what was run | result |
|---|---|
| `claude -p --output-format json --strict-mcp-config --setting-sources '' --tools ''` | **`is_error: false`**, model text returned |
| the same, plus `--json-schema` with the shipping `ExtractEnvelope::JSON_SCHEMA` | **`is_error: false`**; inner payload carries all five required properties and deserializes into `ExtractEnvelope` |

**The risk this was hedging against did not materialise.** codex rejected
Jodd's first schema outright (§3.4); claude accepted the corrected one. Note
that the corrected schema is the STRICTER of the two shapes, which is why one
constant can serve both — a strict schema is still a valid ordinary JSON
Schema.

**One shape is now pinned rather than assumed:** claude's `result` came back
a **string**, not a parsed object. That satisfies `dig_unwrap`'s `.as_str()`
on the happy path, so the final review's Important finding describes a
hazard that does not occur today; the stringify fallback stays as insurance
against a future change, not as a workaround.

#### The speed claim, stated with its noise

Two rounds of `scripts/measure-headless.sh`, subtracting `duration_api_ms`
from wall clock to isolate LOCAL startup:

| variant | round 1 | round 2 |
|---|---|---|
| as shipped before this work | 5.54 s | 5.29 s |
| isolated (this work) | 1.84 s | 2.08 s |
| isolated, control repeat | 2.14 s | **4.55 s** |

**Direction is unambiguous: isolated was faster in 4 runs out of 4, and the
as-shipped overhead is stable at ~5.3-5.5 s.** Magnitude is not: the round-2
control spread 2.4 s between two identical invocations, which is as large as
part of the effect being claimed. So **~3 s saved per call is typical, not
guaranteed**, and any future comparison needs more than one run per variant
to say anything sharper. The control earned its place here — a single round
would have reported a clean 3.4 s and hidden that variance entirely.

## 4. Decisions

### 4.1 `--bare` is refused for Jodd — REJECTED, not deferred

`--bare` gives the largest single reduction and it is the wrong trade here.
Its help text is explicit: *"OAuth and keychain are never read"*, requiring
`ANTHROPIC_API_KEY`.

The entire premise of the agent-CLI provider is *use the CLI the user has
already signed into* — every `Availability::NotAvailable` string in
`presets.rs` is about auth. `--bare` would move every Max/Pro user's workload
onto an API key they have not configured, failing with an error that says
nothing about what they did.

`--setting-sources` + `--strict-mcp-config` + `--tools` reach the same ground
without touching the credential path. **Granular over `--bare`, permanently.**

### 4.2 Persistent sessions are refused for Ask Jodd — and the reason is
structural, not a preference

Session reuse is the top recommendation in every headless guide, and it is
near-worthless here. Jodd's call graph is Extract = 1 call, autolink = 1 call,
Ask Jodd = 2 calls. There is no long conversation to keep warm.

Worse, Ask Jodd's two calls **cannot share a prefix**: `ask/run.rs:68` sends
`SELECT_SYSTEM_PROMPT` + a `CATALOG:` block, `ask/run.rs:104` sends
`ANSWER_SYSTEM_PROMPT` + a `NOTES:` block. Only `turns` is common, and it is the
smallest of the three. Resuming would carry the entire catalog into the answer
turn's context — **more tokens, not fewer**.

The real lever on Ask Jodd is *one fewer call*, not *a cheaper second call*.
That is a prompt-design question and is deliberately out of this plan's scope.

### 4.3 Model routing is a product decision, not a perf fix

Jodd deliberately does not choose the model; the user's own CLI config does
(`presets.rs` already records opencode resolving its model from
`~/.config/opencode/opencode.jsonc`). Adding `--model` means overriding a
setting the user made elsewhere. If it lands it should be a visible setting,
never a hardcoded arg. Out of scope.

### 4.4 Streaming is real but is a separate piece of work

`--include-partial-messages` / `--output-format stream-json` would cut
*perceived* latency for Ask Jodd, where a person is watching a modal. It needs a
new `OutputSource` **and** a route from the backend to the frontend — gotcha #6
territory, where three separate features have now shipped broken by assuming
that route exists. Not folded in here.

### 4.5 Isolation flags belong in `args`, not in a new `AgentCliSpec` field

`AgentCliSpec.args` is already a per-preset argv template. "Isolate this CLI" has
a different flag name, spelling and scope on every row (§2), so an
`isolate: bool` field would just be a switch whose meaning is re-decided per
backend — the shape gotcha #18 warns about. Keep it in `args`.

**The one thing that genuinely does not fit `args`: the output schema.**
It is *workflow*-dependent, not preset-dependent — `extract` and `suggest_links`
have a schema, `chat` (Ask Jodd) returns prose and has none. And claude takes it
inline while codex takes a file path (§2.1). So it needs a new placeholder
following the existing `{out_file}` precedent, plus a way to omit the whole
argument when there is no schema.

---

## 5. Non-goals

- Changing which LLM provider or model any account uses.
- `--bare`, or anything else that alters how a CLI authenticates.
- Persistent/resumed sessions.
- Streaming output and the frontend channel it needs.
- Reducing Ask Jodd's two calls to one (prompt design, tracked separately).
- Touching `HttpProvider` — none of this applies to it.
