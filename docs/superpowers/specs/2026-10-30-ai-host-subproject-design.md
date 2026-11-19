# AI host as its own project — an MCP server agent apps reach over SSH

**Date:** 2026-10-30 · **Status:** written after a brainstorm with the user and
a builder check; awaiting the user's review. Calls made by the author rather
than the user are marked "my call".

Builds on, and partly replaces:
[2026-10-13 remote agent CLI](2026-10-13-remote-agent-cli-design.md) (the
`ai-run-v2` wrapper, §4.4–4.6) and
[2026-10-15 AI host publishing](2026-10-15-ai-host-publishing-design.md) (the
kit, image and guides). Facts already measured there are not re-derived here.

## 1. Why

Today the AI host is part of Jodd. Jodd generates a shell wrapper
(`llm/remote/wrapper.rs`), pushes it to the host over a password login, and
the wrapper knows exactly three requests: `status`, `run claude|codex`,
`forget`. That shape has three costs the user wants gone:

1. **Every change on the host needs a Jodd release and a password.** A new
   wrapper version means "Reinstall helper" on every host, with the password.
2. **It cannot grow.** Adding a harness (Pi is the next candidate) or a host
   tool (speech-to-text, roadmap #13) means editing a hard-coded shell `case`
   inside Jodd's source.
3. **Nobody else can use it.** Other people who want a host for their own
   agent apps would have to adopt Jodd's private wire format.

The user's goal, in their words: let an agent app call **inference and tools
over the network** — over SSH as today, later over MCP's HTTP transport — in a
way that **can gain capabilities later but stays safe**.

## 2. Decisions taken in the conversation

| Question | Decision | Whose |
|---|---|---|
| First client | Jodd. Others later. | user |
| Future growth | (1) more harnesses beyond Claude Code and Codex, e.g. Pi; (2) host tools Jodd's features call. | user |
| Who turns a new harness or tool on | **The host's administrator, on the host.** A client can never enable anything for itself. | user |
| Client/host mismatch | Must not fail silently: tell the user to **upgrade** (and which side). | user |
| Repo | **Its own public GitHub repo**, so other people can use it easily. | user (after asking about cross-repo dependency cost; see §6) |
| First milestone | **Parity**: Claude Code and Codex as today, through the new path. | user |
| Who owns a subscription | **Each user owns their own.** A host has one owner. | user |
| Wire protocol | **MCP** (JSON-RPC 2.0). Transport 1 = **stdio over the existing SSH forced command**. Transport 2 = MCP Streamable HTTP + OAuth, later. | my proposal, user agreed by asking to write it |
| Host-side implementation | **One Rust binary using `rmcp`** (the crate `jodd-mcp` already uses). | my call |
| Tools move data how | The client **sends input with the request**; the host never fetches a client's data, and a harness gets no tools. | my proposal, user agreed |

## 3. Goals, non-goals, anti-goals

**Goals**

- G1. A separate, versioned, public project that owns everything that runs on
  the host: the server, the admin helper, the installers, the image, the
  host-side docs.
- G2. Jodd talks to it with MCP over SSH and reaches parity with today's
  Claude Code / Codex behaviour (§7).
- G3. Capability discovery: the client learns what the host can do from the
  host, and explains a mismatch with the action that fixes it (§8).
- G4. Upgrading a host never needs Jodd's password.
- G5. Hosts and Jodd installs already in the field keep working through the
  change (§10).
- G6. The design admits new harnesses, new tools and the HTTP transport
  without changing what is already there (§11).

**Non-goals for this project** (designed for in §11, not built)

- Pi or any harness beyond Claude Code and Codex.
- Any host tool other than inference (no speech-to-text yet).
- The HTTP + OAuth transport.
- Streaming partial answers, progress, or connection reuse across calls.
- A UI for per-key permissions.

**Anti-goals** (would make this a failure even if it worked)

- **Several people on one host / one subscription.** The host must not grow
  accounts, quotas or billing for multiple people. That would be a tool for
  sharing a personal subscription, which the providers' terms forbid
  (`docs/ai-host/TERMS-REVIEW.md`). Several *devices* of one owner is fine.
- **A client widening its own permissions.** Nothing a client sends — a
  request, an argument, an argv, a path — may enable a harness or tool, raise a
  limit, or reach a shell.
- **A Jodd-run relay** or anything that puts Jodd's developers in the data
  path.
- **A harness with host access because a client asked.** Harnesses keep the
  confinement they have today (§7.3).

## 4. The shape

```text
 Client device                          AI host (one owner)
 ─────────────                          ───────────────────
 Jodd ── ssh, Jodd's per-device key ──▶ sshd
                                        └ authorized_keys line:
                                          restrict,command="/usr/local/bin/jodd-ai-mcp --key <id>"
                                          └ jodd-ai-mcp  (root-owned binary)
                                             ├ reads /etc/jodd-ai-host/policy.toml (root-owned)
                                             ├ speaks MCP on stdin/stdout
                                             └ runs claude / codex as the AI account
                                                └ HTTPS ─▶ Anthropic / OpenAI
```

Two things change from today, and both are the security story:

1. **The program behind the key is the host's, not Jodd's.** Today the
   wrapper is a file in `~jodd-ai/.jodd/` that Jodd wrote and the AI account
   can modify. Now the forced command is a root-owned binary installed by the
   host's own installer, and what it allows comes from a root-owned policy
   file only the administrator can edit. A compromised client cannot widen
   what the key can do, and nothing running as the AI account can change the
   policy. **One gap stays open in v1 (user's decision, 2026-10-30):**
   `~/.ssh/authorized_keys` is still owned by the AI account, so code that
   could write files as `jodd-ai` could drop `restrict,command=` from a line.
   Today nothing can: Claude Code runs with tools off and Codex in a read-only
   sandbox. See §11 for when this must close.
2. **What a key can do is listed by the host.** `tools/list` returns only
   what the policy enables. That list *is* capability discovery (§8).

What stays as it is: `restrict` (no pty, no forwarding), one SSH key per
client device with comment `jodd-ai-<id>`, host-key pinning on the client,
password used only at Setup, the dedicated `jodd-ai` account, the scratch
directory per run deleted on exit.

## 5. The host project

### 5.1 Repo and what moves into it

A new public repo, `BBM-Co-ORG/jodd-ai-host`, licensed Apache-2.0 (§13). It takes, with history
where practical (builder's call on `git filter-repo` vs a fresh start):

| From Jodd | Into the new repo |
|---|---|
| `ai-host/` (helper, docker, install, lib, test) | root of the repo |
| `docs/ai-host/HOW-IT-WORKS.md`, `INSTALL.md`, `INSTALL.th.md`, `TERMS-REVIEW.md` | `docs/` |
| `.github/workflows/ai-host.yml` | `.github/workflows/` |
| `scripts/ai-host-e2e/`, `scripts/ai-host-docker/` (fake CLIs) | `test/` |
| — | new: the `jodd-ai-mcp` Rust crate, `CHANGELOG.md`, `LICENSE` |

`docs/ai-host/USER-GUIDE*.md` describes Jodd's screens, so it **stays in
Jodd** and links to the new repo for installing a host. `docs/BACKEND-SSH.md`
§10 stays in Jodd (internal) and is rewritten to describe the client side.

The GHCR image name `ghcr.io/bbm-co-org/jodd-ai-host` **does not change**, so
existing `compose.yaml` files keep pulling updates.

### 5.2 Release

- Semver tags on the new repo, independent of Jodd's version.
- Artifacts: the multi-arch image (as today), plus `jodd-ai-mcp` binaries for
  Linux x86_64/aarch64 (static, musl) and macOS arm64/x86_64 (standalone, not
  static — macOS has no static libc), each with a SHA-256.
- **The new repo builds and publishes on its own** (my call): its own GitHub
  Actions, `GITHUB_TOKEN` for GHCR and releases, no dependency on Jodd's
  private signing pipeline. macOS binaries carry the linker's ad-hoc signature
  only; the installers fetch with `curl`, which sets no quarantine attribute, so
  Developer ID signing and notarisation are not needed for v1. Revisit if a
  binary is ever offered as a browser download.
- Installers (`install/linux.sh`, `install/macos.sh`) download the binary for
  the platform, verify its SHA-256, and install it root-owned at
  `/usr/local/bin/jodd-ai-mcp`. **Upgrading = re-running the installer** (or
  `docker compose pull && docker compose up -d`). The installers are already
  idempotent; keep them so.
- Our own binary **is** baked into the image (it is ours to distribute). The
  rule from the publishing spec stands: the CLIs are never baked in.

### 5.3 The admin helper

`jodd-ai-host` (today's POSIX-sh helper) keeps every command it has (`status`,
`fingerprint`, `signin`, `password`, `lock`/`unlock`, `update-clis`, `keys`,
`revoke`, `doctor`) and gains:

| Command | Does |
|---|---|
| `enable <name>` / `disable <name>` | Turns a harness or tool on/off in the policy. `enable claude` / `enable codex` also probe the CLI's absolute path and login-shell `PATH` as the AI account (the probe `install.rs` does today) and record them in the policy. |
| `list` | Every harness and tool this binary knows, whether enabled, and for harnesses the recorded path. |
| `adopt-keys` | Re-points every `jodd-ai-*` line in the AI account's `authorized_keys` from a legacy `ai-run-v*` wrapper to `jodd-ai-mcp --key <id>`, keeping the key and comment. §10. |

**The policy file has one owner: the Rust binary.** The shell helper never
writes `policy.toml` itself; it calls `jodd-ai-mcp admin …` (builder decides the
sub-command shape) so the format is parsed and validated in one place. Policy
edits require root.

**The policy records the administrator's decisions, and an absent entry means
"not decided yet"** (my call). The installers, and the Docker entrypoint after
its background CLI install, enable a harness **only when the policy has no entry
for it** and its CLI is found. So a fresh host works out of the box, an
explicit `disable` survives every re-install and image update, and a CLI whose
first install failed is enabled on the first start that finds it. A harness
whose CLI is not installed cannot be enabled.

### 5.4 The policy file

`/etc/jodd-ai-host/policy.toml`, root-owned, mode 0644 (readable so the server
running as the AI account can load it; not writable by it). **In the Docker
image it lives on the `state` volume** (`/var/lib/jodd-ai-host/policy.toml`,
which `/etc/jodd-ai-host/policy.toml` points to), because `/etc` is rebuilt on
every image update and the administrator's choices must survive
`docker compose pull`. Builder chooses the exact schema; it must hold at least:

- per harness: `enabled`, absolute CLI path, the probed `PATH`, `timeout_secs`
  (defaults 120 Claude Code, 180 Codex — today's values);
- per tool: `enabled` and its limits;
- `max_request_bytes` (my call: default 8 MiB — far above any Jodd prompt
  today, low enough that a client cannot fill the disk);
- `max_concurrent_runs` (my call: default 2; a call over the limit gets a
  `busy` error, not a queue);
- a reserved, unused-for-now place for **per-key** overrides (`[keys.<id>]`),
  so per-device permissions later do not need a format change.

An absent or unparseable policy means **nothing is enabled** and every call
gets an error naming `jodd-ai-host doctor`. Fail closed.

## 6. Two repos without lockstep

The user's concern: code that depends on each other, split across repos. The
answer is that **nothing is shared at compile time**. The contract is MCP plus
a written list of tool names and argument/result shapes (published in the new
repo as `docs/PROTOCOL.md`), and the host announces it at run time in
`tools/list`. Three rules keep the two repos from needing simultaneous merges:

1. **Additive only.** A new capability is a new tool name. A change that would
   break existing callers ships as a new tool (`infer_v2`), and the old one
   stays for at least one documented deprecation window. Order is therefore
   always: host releases first, Jodd adopts after.
2. **Jodd pins the host in CI.** Jodd's CI pulls the GHCR image at a pinned
   tag and runs its client contract tests against it, with fake CLIs (the
   `scripts/ai-host-docker` fakes, now in the new repo, are built into a test
   image or tag the builder chooses). Bumping the pin is an ordinary Jodd PR.
3. **The host repo has its own conformance suite**: drive `jodd-ai-mcp` over
   stdio with an MCP client and fake CLIs, and assert every behaviour in §7. Any
   other client author can run the same suite.

## 7. MCP surface, version 1 (parity)

Transport: stdio. MCP protocol version: whatever `rmcp` negotiates; the server
must refuse a client that cannot negotiate one it supports.

### 7.1 Entry

The forced command is `jodd-ai-mcp --key <id>`. The server reads
`$SSH_ORIGINAL_COMMAND` and dispatches:

| Request | Mode |
|---|---|
| `jodd-ai-mcp` | MCP over stdin/stdout |
| `jodd-ai-run 2 <verb> [preset]` | legacy shim (§10) |
| anything else | exit 64, `unknown request` on stderr (as today) |

`--key <id>` identifies which `authorized_keys` line was used. Run directly
without `SSH_ORIGINAL_COMMAND` (tests), the request comes from argv, as the
wrapper does today.

### 7.2 Tools

`initialize` returns `serverInfo.version` = the host project's version.
`tools/list` returns only enabled tools. Each tool carries
`_meta: { "jodd-ai-host/tool-version": <n> }`.

**`infer`** — run one harness on one prompt.

- Arguments: `harness` (enum of the **enabled** harnesses only), `prompt`
  (string), `system` (optional string), `schema` (optional JSON Schema, for the
  harness's structured-output mode, as `{schema_file}` today),
  `timeout_secs` (optional; may only **lower** the policy's value).
- **The host normalises; the client sees no harness-specific format** (my
  call — a client other than Jodd must not have to parse Claude's JSON
  envelope or know Codex's last-message file). The host owns, per harness:
  how `system` and `prompt` are combined for that CLI (today Jodd joins them
  into stdin, `PromptDelivery::StdinAll`; that join moves to the host for
  remote runs, byte-identical to what Jodd sends today), and how the final
  answer is extracted (Claude Code's `--output-format json` envelope unwrapped
  as Jodd's preset `unwrap` does today; Codex's last-message file).
- Success: `content: [{ type: "text", text: <the answer text> }]`, plus
  `structuredContent: { harness, exit_code }`. For Jodd, the client-side
  `unwrap` step becomes a no-op on remote runs.
- Failure: `isError: true` with
  `structuredContent: { kind, exit_code?, stdout?, stderr? }`, where `kind` is
  one of `not_enabled`, `cli_missing`, `timeout`, `cli_failed`, `busy`,
  `too_large`. `stdout`/`stderr` are the CLI's **raw** output, so Jodd's
  measured failure signatures (`diagnose`, gotcha #7) keep working on them.
  Classifying failures on the host (e.g. a future `not_signed_in`) is an
  additive later step; **clients must treat an unknown `kind` as
  `cli_failed`.**

**`forget_key`** — removes the `authorized_keys` line whose comment is
`jodd-ai-<id>` for the calling `--key`, then the server exits. **Always
available, never subject to policy**: a client must always be able to revoke
itself. Same line-matching rule as today's `forget`.

### 7.3 What a run does

Parity with the wrapper, which is the specification here; its tests in
`llm/remote/wrapper.rs` move to the host repo as the conformance suite's
starting point:

- Claude Code runs with all built-in tools off; Codex runs in its read-only
  sandbox with no session file — the exact argv `presets.rs` builds today. The
  argv is fixed in the binary per harness, never taken from the client.
- **After the split, the host owns the remote argv and Jodd's `presets.rs`
  owns the local argv** (my call). They start identical — a one-time test at
  the split asserts the host's argv equals what `wrapper.rs` generates from
  `presets.rs` today — and may then drift: a CLI change is fixed in each
  project on its own schedule. The measurement notes behind each isolation
  flag (`--bare`, Codex `--ignore-user-config`, the read-only sandbox) are
  copied into the host repo's docs so the host's maintainers know why each
  flag is there. The host's conformance tests pin its argv.
- A fresh scratch directory per run as the cwd, holding prompt and schema
  files; removed when the run ends, however it ends.
- The CLI's whole process group is killed on: timeout, the client's MCP
  `notifications/cancelled` for that request, stdin closing, or the SSH
  channel closing. The wrapper's measured behaviour (TERM, then KILL after a
  grace period; job notices kept out of stderr) is the bar.
- The server logs one metadata line per call — key id, tool, harness,
  duration, outcome — and **never** the prompt or the answer. Builder chooses
  where (syslog/journald, or a file under `/var/log`).

## 8. Discovery and mismatch

Jodd decides what to show from the host's own answers, never from version
arithmetic alone:

| Situation | Jodd learns it from | Jodd says |
|---|---|---|
| Host is legacy (`ai-run-v*`, no MCP) | the MCP request exits 64 | "This AI host uses the old helper. Upgrade it: on the host run … then `sudo jodd-ai-host adopt-keys`." AI features keep working through the legacy path meanwhile. |
| Host lacks a tool Jodd needs | absent from `tools/list`, and the binary does not know it (`serverInfo.version` older than the version that introduced it, from `PROTOCOL.md`) | "Upgrade the AI host to use *feature*." with the upgrade command for its kind. |
| Host knows the tool but it is off | absent from `tools/list` while `serverInfo.version` is new enough | "Ask the host's administrator to run `sudo jodd-ai-host enable <name>`." |
| Host has a tool version newer than Jodd understands | `tool-version` higher than Jodd's | "Update Jodd to use *feature* on this host." Jodd keeps using what it understands if the host still offers it. |

The feature's button is disabled **with that reason**, not left to fail on
click. Where Jodd caches the last discovery, and when it refreshes (settings
open, before each call, a TTL), is the builder's call; it must refresh on the
settings Test button and after any `not_enabled` error.

## 9. Jodd's side

- `Runner::Remote` gains an MCP path: one fresh SSH connection per call (as
  today), `exec("jodd-ai-mcp")`, an `rmcp` client over the channel's byte
  stream, `initialize`, `tools/call infer`, close. Cancel = send
  `notifications/cancelled`, then close the channel.
- Desktop and Android both use the russh client Jodd already has. **Verify
  early** that the `rmcp` client compiles and runs on Android over a russh
  channel; it is the one piece with no precedent in Jodd.
- Mapping `infer` errors onto `ExtractError` follows the existing table in the
  2026-10-13 spec §5 (`not_enabled` → `NotConfigured` with the §8 text;
  `cli_missing` → "*preset* isn't installed on *host*"; `timeout` →
  `Transport`; `busy` → `Transport` "the AI host is busy — try again";
  `cli_failed` → today's `diagnose` path).
- **Setup** (`ai_host_setup`) no longer probes paths or installs a wrapper. With
  the password, once, it: checks that `/usr/local/bin/jodd-ai-mcp` exists (if
  not: "This host needs the AI host kit — install it from …"), installs the
  key's `restrict,command="/usr/local/bin/jodd-ai-mcp --key <id>"` line,
  disconnects, reconnects with the key and runs `tools/list`. Path probing
  moved to `jodd-ai-host enable`.
- **Reinstall helper** disappears for MCP hosts (G4); it remains only for
  legacy hosts until they upgrade.
- **Remove AI host** calls `forget_key` on MCP hosts, `forget` on legacy ones.
- `REMOTE_PRESETS` stops being the gate: the harnesses Jodd may run remotely
  are those the host lists **and** Jodd has a preset for.
- Settings show the host's version and its enabled harnesses, replacing the
  per-preset ✓/✗ derived from the wrapper's `status`.

## 10. Migration — nothing in the field breaks

Today's hosts have per-device lines pointing at `~/.jodd/ai-run-v2`, and
today's Jodd installs (including phones that update late) speak only the
`jodd-ai-run 2 …` format.

1. **New host, old Jodd.** The server's legacy shim answers
   `jodd-ai-run 2 status|run|forget` with byte-identical behaviour to wrapper
   v2, for enabled harnesses only (a disabled one reports path `-`, which old
   Jodd already shows as not installed). So after `adopt-keys`, a phone still
   on an old Jodd keeps working.
2. **Old host, new Jodd.** New Jodd tries MCP, gets exit 64, and falls back to
   its existing legacy client, while showing "upgrade the host" (§8).
3. **Upgrading a host**: install the new kit (binary + policy, CLIs enabled by
   the installer), then `sudo jodd-ai-host adopt-keys`. No Jodd password, no
   Jodd action. Old `ai-run-v*` files are left in place and unused; `adopt-keys`
   may offer to delete them.
4. **An old Jodd can undo `adopt-keys` for its own device**: its Setup or
   "Reinstall helper" re-points its line to `~/.jodd/ai-run-v2` and so
   bypasses the policy. Accepted during the transition (my call), because it
   needs the AI account's password: `sudo jodd-ai-host lock` prevents it, and
   `jodd-ai-host status` / `doctor` list every `jodd-ai-*` line that does not
   point at `jodd-ai-mcp`, with `adopt-keys` (re-runnable) as the fix.
5. **Removing legacy support** from Jodd's client and from the server's shim is
   a later, separate decision with a date; not part of this project.

New Jodd does **not** keep the ability to *install* a legacy wrapper on a fresh
host. Setting up a new host requires the new kit (my call: it halves the code
kept alive for the transition, and the kit is one command).

## 11. How it grows later (designed for, not built)

- **A new harness (Pi first).** A harness is code in `jodd-ai-mcp`: its fixed
  argv, how its answer is read, and **how it is confined** (tools off,
  read-only, or equivalent). A harness that cannot be confined is not added.
  It appears as a new value of `infer`'s `harness` enum — additive — and the
  administrator enables it. Whether administrators may also define their own
  harness argv in the policy (safe in principle, since only root writes it,
  but the confinement then becomes their responsibility) is left open.
- **Root-owned key file — a precondition, not an option.** Before any harness
  or tool that can write files as the AI account is added, the keys move to a
  root-owned `AuthorizedKeysFile` set in the sshd drop-in. Adding a device then
  becomes: Jodd drops its public key in a pending area over the password
  session, and the administrator runs `sudo jodd-ai-host approve`, comparing
  the fingerprint Jodd shows. That changes Setup, `forget_key`, `revoke`,
  `adopt-keys` and the Docker volume layout, so it gets its own design.
- **A host tool (speech-to-text first).** A new tool name with its own
  arguments. Input travels inside the request — base64 in the arguments,
  bounded by `max_request_bytes` and the tool's own limit. If audio outgrows a
  single message, the tool may take chunks over several calls; MCP has no
  byte-stream primitive, so the tool defines its own. The host never reads a
  client's files and never calls back to the client.
- **Per-key permissions.** The reserved `[keys.<id>]` table narrows what one
  device's key sees in `tools/list`. Narrow only, never widen beyond the
  global policy.
- **The HTTP transport.** A second listener in the same binary speaking MCP
  Streamable HTTP, behind OAuth 2.1 per the MCP authorization spec, off by
  default, enabled by the administrator. Same tools, same policy. Its own
  design will have to settle TLS, where the authorization server lives, and
  how an HTTP client maps to a policy key. SSH remains the default.

## 12. Testing

- **Host repo:** unit tests for the policy (fail-closed on absent/invalid), the
  entry dispatch (§7.1), the legacy shim (byte-identical to wrapper v2 — port
  the wrapper's tests), and the run lifecycle (cancel, timeout, scratch
  cleanup, process-group kill) with fake CLIs. An end-to-end suite in Docker:
  real OpenSSH, `restrict`, forced command, `adopt-keys`, legacy and MCP
  clients against the same host. shellcheck on the helper and installers, as
  today.
- **Jodd:** the client against the pinned image in CI (§6 rule 2); the legacy
  fallback against a wrapper-v2 host; the error mapping (§9) as unit tests;
  a russh-channel `rmcp` client test that runs in the Android build's test
  set or an emulator job.
- **Live pass before release:** Mac and one Android device, against (a) an
  upgraded host after `adopt-keys` and (b) a legacy host; cancel mid-run
  observed on the host (`ps`) within seconds.

## 13. Questions the user decided after the builder check

**Q1 — Names. Decided 2026-10-30: `jodd-ai-host`.** The repo is
`BBM-Co-ORG/jodd-ai-host`; the image keeps `ghcr.io/bbm-co-org/jodd-ai-host`;
the names in §4–§7 (`jodd-ai-mcp`, `jodd-ai-host`, the `_meta` key, the key
comment `jodd-ai-<id>`, `/etc/jodd-ai-host/`) are permanent from v1.

**Q2 — Must the AI account lose write access to `authorized_keys`?
Decided 2026-10-30: not in v1.** Today's harnesses cannot write files, so v1
keeps the account-owned file and says so (§4). A root-owned key file is a
precondition for any harness or tool that can write as the AI account (§11).
Separately, the guides and Jodd's Setup completion screen must recommend
`sudo jodd-ai-host lock` after Setup: an open password login is an
unrestricted shell for the AI account, a larger hole than this one.

**Q3 — Licence. Decided 2026-10-30: Apache-2.0** for the new repo.

## 14. Left to the builder

- `git filter-repo` with history vs a fresh repo with a pointer to Jodd's
  history.
- The exact policy schema and the `jodd-ai-mcp admin` sub-command shape.
- Where the per-call metadata log goes.
- Discovery caching and refresh timing within §8's constraints.
- How fake-CLI test images are built and tagged.
