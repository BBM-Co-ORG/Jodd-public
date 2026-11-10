# Remote agent CLI — run `claude` / `codex` on your own server over SSH

**Date:** 2026-10-13 · **Status:** design approved in conversation (approach A);
written spec self-reviewed, user review deferred to the PR at their request.

## 1. Why

Jodd's AI workflows (Extract, Ask Jodd, Summarize, URL ingest…) can run through
the user's **Claude** or **ChatGPT** subscription only by spawning the official
CLI (`claude -p`, `codex exec`) — the subscription's quota is bound to *that
program's* sign-in. Every other route bills API credits separately. Measured on
2026-10-13:

- The Agent SDK / Codex SDK are wrappers around the same binary
  (`@openai/codex-sdk` depends on `@openai/codex`; `@anthropic-ai/claude-agent-sdk`
  ships native binaries for darwin/linux/win32 only). They do not remove the
  process, and there is no Android build.
- Anthropic's Agent SDK page: third-party developers may not offer claude.ai
  login or rate limits "unless previously approved". This feature is therefore
  **personal use**: the user runs *their own* CLI, signed in *by them*, on *their
  own* machine. Jodd never handles a subscription credential.

Android cannot spawn a CLI (`resolve.rs` refuses `AgentCli` there today), so the
Android app has no subscription route at all. This design adds one: run the CLI
on an always-on server the user owns, reached over SSH. It is offered on **every
platform** — on desktop it is the alternative to "run on this computer".

## 2. Decisions taken in conversation

| Question | Decision |
|---|---|
| Where does the CLI run? | An always-on Linux server/VPS the user owns. (A Mac works mechanically but sleep/reachability is not designed for.) |
| Which SSH identity? | A **dedicated, restricted** key: its `authorized_keys` line is `restrict,command="…/ai-run"`. It can run the two CLIs and nothing else. |
| Architecture | **Approach A**: keep `LlmProviderKind::AgentCli` + presets as *what* runs; add an optional `agent_host` for *where*. Split the runner into "build the invocation" + "execute it" (local or SSH). |
| Platforms | All. Desktop gets "Runs on: this computer / a server"; Android gets "a server" only. |
| Testing | Fake CLIs. CI uses the in-process russh test server; a local Docker OpenSSH container proves the forced-command/`restrict` path. No real subscription calls in tests. |

## 3. Non-goals

- Custom agent-CLI specs and the qwen/other presets remotely. Only `claude` and
  `codex`. A remote Custom spec would make the wrapper accept arbitrary argv,
  which un-restricts the key.
- Signing the CLIs in from Jodd. The user does it once over normal SSH.
- Reusing a notes SSH account's key or session; connection pooling; streaming.
- A Jodd-hosted relay, or offering this to other users.
- Waking a sleeping Mac.

## 4. Architecture

```
LlmConfig { provider: AgentCli, agent_preset: "codex", agent_host: Some(AgentHost) }
        │
        ▼
resolve::build ──▶ AgentCliProvider { spec, runner: Runner }
                                          │
                     ┌────────────────────┴───────────────────┐
              Runner::Local(PathBuf)                 Runner::Remote(AgentHost)
              (today's tokio::process path)          russh: connect(Key) → exec
                                                     "jodd-ai-run run codex"
                                                              │ (forced command)
                                                              ▼
                                                 ~/.jodd/ai-run-v<n> (POSIX sh,
                                                 generated from presets.rs)
                                                 → fixed argv, watchdog, cleanup
```

### 4.1 Config (`accounts.rs`)

```rust
/// Where an agent CLI runs when it is not this machine. Absent = local.
pub struct AgentHost {
    pub target: String,          // "user@host", validated like ssh_target
    pub port: Option<u16>,
    pub key_dir: String,         // bare dir name under <data_base>/jodd/ai-hosts/
}
// LlmConfig gains:
#[serde(default)] pub agent_host: Option<AgentHost>,
```

`agent_host` is orthogonal to `agent_preset`, like `agent_custom`. Existing
files parse unchanged (`serde(default)`). It lives wherever the `LlmConfig`
lives — app-level (`app_llm.json`) or per-account (`accounts.json`).

Key material: `<data_base>/jodd/ai-hosts/<uuid>/{id_ed25519, id_ed25519.pub,
known_hosts}` — the same shape and permissions as Managed SSH
(`russh_setup::generate_key`), a separate root so removing a notes account can
never revoke the AI key.

### 4.2 Resolution (`llm/resolve.rs`)

- `build`: `AgentCli` with `agent_host = Some(h)` → `AgentCliProvider::remote(spec, h)`.
  The preset must be `claude` or `codex` (`remote_capable(preset_id)`), else
  `NotConfigured("only the Claude Code and Codex presets can run on a server")`.
  `ClaudeCode` (legacy) has no host and stays local.
- The Android gate narrows from "every AgentCli" to "AgentCli **without** a host".
- `agent_preset_id_of`, `prompt_delivery_of` unchanged in shape (no wildcard).
  `prompt_delivery_of` keeps returning the preset's delivery; a remote call never
  uses argv for the prompt, and both remote presets are `StdinAll` anyway.

### 4.3 Runner split (`llm/agent_cli.rs`)

`run_once_untracked` today does two jobs. It becomes:

1. **`Invocation::build(spec, system, payload, schema)`** — pure: the full
   prompt, the stdin payload, whether `{out_file}`/`{schema_file}` are used.
2. **`Runner::run(&Invocation, cancel) -> Result<RawOutput, ExtractError>`** where
   `RawOutput { stdout, stderr, exit: i32, out_file: Option<String> }`.
   - `Local`: today's code, moved verbatim (temp files, scratch cwd,
     `kill_on_drop`, timeout + kill, BrokenPipe tolerance).
   - `Remote`: §4.5.
3. The shared tail — non-zero exit → `UpstreamError(failure_detail…)`, choose
   out-file vs stdout, `strip_ansi`, empty check, `dig_unwrap` — runs once for
   both runners.

`run_json`, the Heuristic nudge retry, `run_once` (budget + receipts),
`CheckedProvider` masking: untouched.

### 4.4 The wrapper (`llm/remote/wrapper.rs` generates it)

A POSIX `sh` script, generated by Rust from `presets.rs` so the argv has **one
source of truth**, stamped `JODD_AI_RUN_VERSION=<n>` (bumped whenever the
generated text changes; a test pins the hash).

Invocation: the forced command is `…/ai-run-v<n> --key <uuid8>` (the `--key`
names which `authorized_keys` line invoked it). The request comes from
`$SSH_ORIGINAL_COMMAND` in the forced-command case, or from the remaining
`"$@"` when invoked directly (tests). A request is
`jodd-ai-run <version> <verb> [preset]`; a `<version>` other than the script's
own exits **65** (`version mismatch`) — so a stale wrapper is detected on the
very call that hits it, with no extra round trip.

| Verb | Does |
|---|---|
| `status` | Accepts any version. Prints `version=<n>` then, per preset, `preset=<id> path=<abs or ->`. Exit 0. |
| `run <preset>` | `<preset>` ∈ {claude, codex} else exit 64. Reads stdin: line 1 = base64 schema or `-`; the rest = prompt. Makes `mktemp -d`, `trap` removes it, `cd`s into it (empty scratch cwd, as locally). Writes prompt (and schema) to files there, runs the **baked absolute path** with the **baked argv** (placeholders → scratch files) with stdin from the prompt file, under a watchdog of `timeout_secs` (exit 124 on expiry). For `LastMessageFile` presets it `cat`s the out file to stdout; for `Stdout` presets the CLI's stdout passes through. Exit = the CLI's exit. |
| `forget` | Removes **its own** `authorized_keys` line (the one whose comment is `jodd-ai-<uuid8>` from `--key`), then exits. Lets "Remove AI host" clean up without a password. |
| anything else | exit 64, `unknown request` on stderr. |

- **PATH and absolute paths are baked at install**, probed under the user's
  login shell (`$SHELL -lc`, then `$SHELL -lic` for CLIs only an interactive rc
  finds, e.g. nvm). A forced command runs with a minimal PATH, and `codex` from
  npm is a `#!/usr/bin/env node` script, so the CLI's own path is not enough:
  the wrapper exports the probed PATH too. If a CLI is installed later,
  `status` reports it missing → settings offer "Update AI host" (password).
- **The CLI's stderr goes to a file and is replayed after it exits.** The
  watchdog needs job control (`set -m`) to kill the CLI's whole process group,
  and under `set -m` the shell prints job notices ("Terminated") on its own
  stderr — which would land in `failure_detail`. Measured on the prototype,
  2026-10-13. The wrapper therefore mutes its own stderr after spawning and
  replays the CLI's.
- **Watchdog, not `timeout(1)`**: `timeout` is absent on macOS. The script runs
  the CLI in the background, a sleeper kills it after `timeout_secs`, and the
  script reports 124. One code path everywhere.
- **No sign-in probe.** Whether a CLI is signed in is not reported by
  `status`: the exact probe commands are unmeasured, and a guessed one would be
  a status line that lies. A signed-out CLI surfaces on the first call (or the
  settings Test button) through the presets' measured failure signatures.
- Installed at `~/.jodd/ai-run-v<n>` — **one file per version**, mode 0700,
  written via base64 heredoc + sha256 check (the `scripts.rs` `decode_to_tmp`
  pattern). Per-version files let two devices on different Jodd versions (phone
  and Mac, same server) coexist instead of overwriting each other's wrapper.
  Install also deletes `ai-run-v*` files no `authorized_keys` line references.
- The `authorized_keys` line:
  `restrict,command="$HOME/.jodd/ai-run-v<n> --key <uuid8>" ssh-ed25519 AAAA… jodd-ai-<uuid8>`
  (`$HOME` expanded at install). `restrict` = no pty, no forwarding of any kind.
  Install is idempotent: an existing line for the same key body is replaced,
  other lines are never touched (same discipline as `install_authorized_key` /
  the same-directory atomic rewrite).

### 4.5 Remote execution (`llm/remote/exec.rs`)

Per call, **a fresh connection** (`russh_session::connect(target, port,
known_hosts, Credential::Key)`): never `session_for`, so a 3-minute AI call
cannot hold the notes sync mutex, and there is nothing to keep alive across
Android process death. Handshake cost (~1 s) is small next to an LLM call.

1. `channel_open_session`, `exec(true, "jodd-ai-run <version> run <preset>")`.
2. Send `base64(schema) | "-"`, `\n`, prompt bytes; `eof`.
3. Collect `Data` → stdout, `ExtendedData{ext:1}` → stderr, `ExitStatus` → exit,
   until `Close`.
4. Overall deadline = `spec.timeout_secs + 15 s` (the wrapper's watchdog fires
   first; the margin covers connect + transfer).
5. **Cancel**: send the `signal` channel request (`TERM`), then close the
   channel and drop the connection. Whether OpenSSH delivers the signal to a
   forced command with no pty is **measured in Docker** during implementation;
   the wrapper's watchdog bounds quota burn either way. The result goes in
   `docs/BACKEND-SSH.md`.

The connect and transfer go through a small seam so CI can test it:

```rust
#[async_trait] pub(crate) trait RemoteExec: Send + Sync {
    async fn exec(&self, command: &str, stdin: &[u8], deadline: Duration,
                  cancel: CancellationToken) -> Result<RawOutput, ExtractError>;
}
```

`RusshExec` is the real one. Tests use it against `test_server.rs`, which
learns to honour an `authorized_keys` `command="…"` option the way sshd does
(run it, with the client's request in `SSH_ORIGINAL_COMMAND`) — so CI exercises
the forced-command path, not a stand-in. Docker then proves the same against
real OpenSSH, including `restrict`.

### 4.6 Setup, status, removal (`ai_host_commands.rs`)

Tauri commands, desktop and Android alike (russh is compiled everywhere):

| Command | Does |
|---|---|
| `ai_host_fingerprint(host, port)` | Reuses `russh_setup::fetch_fingerprint` for the trust screen. |
| `ai_host_setup(target, port, password, fingerprint)` → `AgentHost` + `AiHostStatus` | Validate target; write `known_hosts` from the confirmed fingerprint; `generate_key` into a new `ai-hosts/<uuid>`; **connect with the password once** (never stored); probe absolute CLI paths under `$SHELL -lc`; install the generated wrapper; install the restricted line; disconnect; **reconnect with the key** and run `status` — proving the restriction is live. On any failure, delete the new key dir. |
| `ai_host_status(host)` → `AiHostStatus` | `status` over the key; compares `version` with Jodd's. Drives the per-preset ✓/✗ list and the "needs update" banner. |
| `ai_host_update(host, password)` | Re-probe paths, re-install wrapper + line over the password. Used when the version is stale or a CLI was installed later. |
| `ai_host_forget(host)` | Best-effort `forget` over the key, then delete the key dir. Removal succeeds locally even if the server is unreachable (the UI says so). |

Saving the provider config is the existing settings save path; `agent_host` is
just a field.

### 4.7 UI (`LlmProviderSettings.svelte`)

- The Agent CLI branch is no longer hidden on Android. Inside it, a
  **"Runs on"** choice: *This computer* (desktop only) / *A server*.
- *A server*: if no host — host, port, user, password → fingerprint confirm →
  progress → per-preset status ("Codex CLI ✓ signed in · Claude Code ✗ not
  installed"). If a host — the status list, **Update** (password) and **Remove**.
- Preset dropdown when remote: only presets the status reports present; Custom
  hidden.
- Test button: unchanged (`test_llm_provider` works once `build` returns the
  remote provider).

## 5. Errors

| Failure | Detected by | `ExtractError` / message |
|---|---|---|
| Unreachable / no network | connect error, 10 s connect timeout | `Transport`: "Couldn't reach AI host *host*" |
| Host key changed | `PinnedHost` | `Refused`: "The AI host's identity changed — set it up again." Never re-pinned silently. |
| Key rejected | `AuthRejected` | `NotConfigured`: "The AI host no longer accepts Jodd's key — set it up again." |
| Wrapper outdated | exit 65 on a call, or `status` version ≠ Jodd's in settings | `NotConfigured`: "AI host needs an update — open AI settings." |
| CLI missing | wrapper exit 127 | `NotConfigured`: "*preset* isn't installed on *host*" |
| CLI not signed in | existing preset failure signatures | `diagnose` text, **remote-aware**: "Sign in to *preset* on *host* (over SSH), then test again." |
| Timeout | exit 124 / deadline | `Transport`, with the existing no-output hint |
| Subscription limit | **unmeasured** | passed through via `failure_detail`; signatures are added only from observed text (gotcha #7) |

`diagnose` gains a `host: Option<&str>` so the action text names the right
machine; the needles are unchanged.

## 6. Privacy and wording

- Masking: unchanged — `CheckedProvider` wraps any provider.
- `cliPrivacy.ts`: `FILE_ACCESS_NOTE` gets a remote variant ("…can read files
  **on *host***"). `ARGV_NOTE` never applies remotely (prompt is on stdin).
- Ask Jodd destination line (`lib.rs` ~5964): "Agent CLI *preset* on *host* (over
  SSH) · destination/model follow the CLI configuration; may use cloud services".
- Receipts: remote runs record provider `agent_cli_remote` (local stays
  `agent_cli`); `AiReceipts.svelte` labels it "Agent CLI (server)". The hostname
  is **not** recorded — receipts keep only a closed set of public identifiers,
  because configured values can themselves be sensitive.
- `map_input_cap`: a remote runner is never `Argv`, so no Windows command-line
  cap applies (the existing `prompt_delivery_of` already says `StdinAll`).
- Consent / `data_allowed`: unchanged.

## 7. Testing

**CI (ubuntu, no Docker):**
- Wrapper generator: pin that each preset's baked argv equals `presets.rs`'s
  (`args` + `schema_args`), and pin the version↔hash pairing.
- Wrapper behaviour, run with `sh` against fake `claude`/`codex` scripts:
  `status`; `run` with/without schema; out-file vs stdout presets; prompt with
  Thai + a line containing only `-`; unknown preset → 64; watchdog → 124 and the
  child is gone; scratch dir removed on success, failure and timeout;
  `SSH_ORIGINAL_COMMAND` path; version mismatch → 65 (`status` exempt);
  `forget` removes only its own line; two keys at two versions coexist.
- `RusshExec` against `test_server.rs`: stdout/stderr/exit separation, large
  prompt (1 MB), cancel returns promptly, deadline.
- Runner split: the existing `agent_cli` tests keep passing unchanged against
  `Runner::Local` (they are the regression proof of the move).
- `resolve`: remote + claude/codex builds; remote + custom/qwen refuses; Android
  gate allows AgentCli only with a host (unit-tested as a pure predicate).
- Config: an `accounts.json` / `app_llm.json` without `agent_host` parses; one
  with it round-trips.

**Local Docker (`scripts/ai-host-docker/`, `#[ignore]` tests gated by env):**
A Debian-slim image with real OpenSSH (`PasswordAuthentication yes`), a user
with a password, fake `claude`/`codex` in `~/.local/bin` (so the login-shell PATH
probe is exercised). Proves what the in-process server cannot:
- `ai_host_setup` end to end over a password, then key login.
- **The restriction is real**: over the AI key, `exec("id")`, `exec("sh")` and a
  pty request are all refused / produce only the wrapper's "unknown request".
- `run` through the forced command; cancel → measure whether the fake CLI
  process is gone immediately (signal delivered) or only at the watchdog.
- `forget` removes the line; the key then fails auth.
- `update` after bumping the version.

**Live (user, later):** real `codex` signed in with ChatGPT and real `claude`
signed in with a subscription on the user's VPS; Extract from the S23 FE and
from macOS; cancel mid-call and `pgrep` on the server; airplane mode.

## 8. Files

New: `src-tauri/src/llm/remote/{mod.rs, wrapper.rs, exec.rs}`,
`src-tauri/src/ai_host_commands.rs`, `scripts/ai-host-docker/{Dockerfile,
fake-claude, fake-codex, run-tests.sh}`.
Changed: `accounts.rs` (AgentHost, field), `llm/agent_cli.rs` (split),
`llm/resolve.rs`, `llm/presets.rs` (`remote_capable`, `diagnose` host),
`lib.rs` (register commands, destination text), `LlmProviderSettings.svelte`,
`cliPrivacy.ts`, `AiReceipts.svelte`, `docs/PLATFORM-MATRIX.md`,
`docs/BACKEND-SSH.md` (signal measurement), `docs/USER-GUIDE-LEARNING-RESEARCH.md`.

## 9. Open items to measure (not guess)

1. Does OpenSSH deliver a `signal` channel request to a no-pty forced command?
   (Docker.) Determines whether cancel is immediate or watchdog-bounded.
2. The real "subscription limit reached" text — only from a live run; until
   then it surfaces raw.
