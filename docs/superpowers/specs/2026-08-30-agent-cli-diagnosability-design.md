# Agent-CLI diagnosability — make the failure say what to do

**Date:** 2026-08-30
**Status:** design, approved in chat. Plan to follow.
**Scope:** `src-tauri/src/lib.rs` (`test_llm_provider`), `src-tauri/src/llm/prompt.rs`,
`src-tauri/src/llm/presets.rs`, `src-tauri/src/llm/agent_cli.rs`,
`src/lib/components/LlmProviderSettings.svelte`.

## 1. The problem, from three failures measured in one day

Jodd runs whichever headless agent CLI the user already has. That makes the
provider config **dependent on an environment Jodd does not own**: which CLI,
which model that CLI resolves, which providers it holds credentials for, and
what each CLI's flags mean. On 2026-08-30 three separate failures reached a
real user in the real app:

| # | What failed | Whose fault | What the user saw |
|---|---|---|---|
| 1 | `codex --output-schema` rejected Jodd's JSON Schema | Jodd | nothing — empty output file |
| 2 | `claude --json-schema` broke Extract | Jodd | `claude exit exit status: 1:` — a blank |
| 3 | opencode configured for a model it has no credential for | the user's environment | `UnknownError: Unexpected server error` |

**They share one shape, and that shape is the actual defect:** each failed at
runtime, and none of the three messages told the user what to do next. Two of
the three were detectable before shipping, and all three were detectable by a
"Test connection" button that had run a representative workload.

**The button existed and passed.** `test_llm_provider` (lib.rs) already calls
the production path — `provider.extract(...)`, the same function Extract uses.
What it passed was a toy: `"Jodd connection test. Reply with one short
lesson."`. The model answers that in one turn and never enters the state that
breaks. The author of the change (me) verified the same way, with the same
one-line probe, and reported the work as safe. **The code path was right and
the input was wrong**, which is why eight reviews and 1,193 tests did not catch
it and one real user did.

## 2. Decisions taken before design

Both were the user's call, and they bound everything below.

- **Jodd does not own model or credentials.** It will not send `--model`, will
  not manage any CLI's auth, and will not add a model setting. The user's CLI
  config stays authoritative. Jodd's obligation is to **detect and explain**.
  (This keeps `Capabilities`-style honesty: Jodd advertises what it knows, not
  what it wishes.)
- **Checks run only when the user presses Test connection** — never on a timer,
  never before an Extract. Every check is a real billed call, and an app that
  silently spends the user's quota to reassure itself is not acceptable. The
  trade is that the button must be worth pressing, which section 3 buys by
  making it representative.

## 3. Probe with the real workload

`test_llm_provider` keeps calling `provider.extract(...)` — the production path
is correct and stays. Two changes:

**3.1 A representative sample replaces the one-liner.** A `const` beside
`SYSTEM_PROMPT` in `prompt.rs`, so the two cannot drift apart: multi-paragraph
mixed prose with enough distinct content that a correct answer needs several
`## H2` sections. That is what exercises the structured-output path, the retry
budget, and the envelope — the states failures 1 and 2 lived in.

**Two constraints on the sample, both load-bearing:**

- **It is a constant, never the user's own notes.** A test must not ship a
  user's private content to an external model because they pressed a
  diagnostic button.
- **It lives next to the system prompt it is paired with.** The failure being
  guarded against is an interaction between the two; separating them
  reintroduces the drift.

**3.2 Success means a usable envelope, not a zero exit.** Today the test
reports OK whenever `extract` returns `Ok`. That is already stronger than exit
status, but failure 1 produced a run that exited 0 with an empty output file,
so the check tightens to: deserializes into `ExtractEnvelope` **and**
`lessons_markdown` is non-empty.

## 4. `diagnose()` — a dictionary of measured failures

A pure function, unit-testable without a subprocess:

```
diagnose(preset_id, raw_error) -> Option<Diagnosis { cause, action }>
```

**Where each half lives, stated so it is not re-decided during
implementation:** the signature table is **data in `presets.rs`**, beside the
flags it belongs with — a row's failure modes are a property of that CLI, the
same way its argv is. The matching function is **code in `agent_cli.rs`**,
next to `failure_detail`, because it operates on the same raw error text that
function produces and is tested the same way. `presets.rs` gains no logic and
`agent_cli.rs` gains no per-CLI knowledge; that split is what keeps editing a
row from disturbing the runner's tests, which is the reason `presets.rs`
exists as its own file.

Both halves sit under the same Empirical-note discipline gotcha #7 already
enforces: **every entry
quotes a string that was actually observed**. Invented signatures are how a
dictionary starts lying.

Seed entries, all captured on 2026-08-30:

| preset | signature observed | cause shown | action shown |
|---|---|---|---|
| claude | `structured_output_retry_exhausted` | the model could not produce the requested shape | try a different model in your CLI |
| claude | `safeguards flagged this message` | the model declined this request's content | try a different model |
| claude | `Not logged in` / `Please run /login` | this CLI is not signed in | run `claude` once and sign in |
| codex | `invalid_json_schema` | this CLI rejected the output schema | report it — Jodd is sending a shape it will not accept |
| opencode | `UnknownError` + `Unexpected server error` | the configured model has no credential | compare `opencode auth list` against `model` in `~/.config/opencode/opencode.jsonc` |

**No wildcard arm.** An unrecognised failure returns `None` and falls through
to section 5's raw view. A dictionary that guesses is worse than one that
admits it does not know — the opencode entry is precisely a case where the
CLI's own message named neither the model nor the provider, and a guess would
have sent the user somewhere wrong.

## 5. The UI: name the cause, always keep the evidence

`LlmTestResult` grows `cause: Option<String>` and `action: Option<String>`.
`LlmProviderSettings.svelte` renders, in place of today's bare error line:

```
Failed · 11405 ms
The model declined this request's content.
→ Try a different model in your CLI, then test again.
  [ Show details ▸ ]      ← collapsed; the full raw text
```

**"Show details" is not a nicety, it is the fallback for everything the
dictionary does not know**, and it never goes stale. Its content is the raw
error that `failure_detail` (added 2026-08-30) now produces — which already
knows a CLI may report on stdout with stderr empty, the reason failure 2 was
undiagnosable.

When `diagnose` returns `None`, the cause and action lines are absent and the
details block is expanded by default: an unknown failure should show its
evidence without a click.

## 6. Testing

- **`diagnose()` is tested against the verbatim strings captured on
  2026-08-30**, not paraphrases. Paraphrase-testing is how a dictionary passes
  its tests and fails the user.
- **A test pins that the probe sample is non-trivial** — a minimum length and
  more than one line — with a comment saying why. Without it, a future
  "let's make the test suite faster" shrinks the sample and reopens exactly
  this hole.
- **A test pins that `test_llm_provider` validates the envelope**, not just
  `Ok`.
- No network test in CI. The dictionary is pure; the probe needs a live CLI
  and stays a manual gate.

## 7. Non-goals

- Sending `--model`, or any model selection UI.
- Managing, reading, or repairing any CLI's credentials.
- Automatic or scheduled probing.
- A capability matrix of sub-probes (considered as approach B, rejected: it
  multiplies a billed call, and every failure measured so far is caught by one
  representative run).
- Changing which CLI an account uses.

## 8. Accepted costs

Test connection goes from ~1–8 s to roughly **10–20 s** and spends one real
Extract's worth of quota. The button's helper text will say it runs a real
extraction, so the wait reads as work rather than a hang. This is the price of
the button telling the truth, and it is the trade the user chose in §2.

## 9. What this does not fix

The dictionary only covers failures someone has already met. It shortens the
distance from symptom to action for those; it does not make Jodd omniscient
about an environment it deliberately does not own (§2). The honest guarantee
is: **a known failure names itself, and an unknown one shows its evidence
instead of a blank.**
