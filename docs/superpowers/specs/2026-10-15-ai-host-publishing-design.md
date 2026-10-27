# AI host for everyone: install kit, image and published guides

**Date:** 2026-10-15 · **Status:** approach A approved in conversation; this
spec awaits the user's review.

## 1. Why

v0.34.0 shipped the AI host: Jodd (desktop and Android) can run the `claude` /
`codex` CLI on a server the user owns, over a restricted SSH key
(docs/BACKEND-SSH.md §10). Today the only working host is the author's own
`~/jodd-ai-host` container. Other users have no instructions, no tooling, and
would have to rediscover every trap we already hit: the login-shell PATH, a host
key that changes on rebuild (Jodd then refuses it), password login that a
hardened VPS has turned off, a phone that cannot reach `127.0.0.1`, a Mac that
sleeps.

Goal: **preparing a host should be as easy as it can be**, on Docker, on a
Linux server, or on a Mac. Everything is published for other users: in the repo
(so it reaches Jodd-public) and on jodd.bbmedia.co.th.

## 2. Decisions taken in conversation

| Question | Decision |
|---|---|
| Approach | **A**: one helper command, `jodd-ai-host`, that is the same everywhere, plus a thin installer per platform. |
| Where published | The repo (`ai-host/` + `docs/ai-host/`, which reach Jodd-public through the snapshot sync) **and** a page on jodd.bbmedia.co.th that links to them. |
| Language | Full guides in **English**; **Thai** versions of the install guide and the user guide. "How it works" is English only. |
| Docker | A **ready-made image on GHCR**: the user downloads one `compose.yaml` and runs `docker compose up -d`. |
| Without Docker | **Ubuntu/Debian** (scripted and tested), **macOS** (scripted where macOS allows, guided where it doesn't), **other Linux** (the script also covers dnf and apk; anything else gets written steps). |

## 3. Facts this design rests on (checked 2026-10-15)

- **Claude Code** installs without Node: `curl -fsSL https://claude.ai/install.sh | bash`
  puts a native binary behind `~/.local/bin/claude` and updates itself.
  Supported: Ubuntu 20.04+, Debian 10+, Alpine 3.19+ (needs `bash curl libgcc
  libstdc++ ripgrep` and `USE_BUILTIN_RIPGREP=0`), macOS 13+; x64 and ARM64.
  The npm package needs **Node ≥ 22**, more than Ubuntu 24.04's Node 18, so
  the kit never uses npm for it. Signed apt/dnf/apk repositories also exist.
  Its licence is **not open source** ("SEE LICENSE IN README.md").
- **Codex** installs without Node: `curl -fsSL https://chatgpt.com/codex/install.sh | sh`.
  Licence Apache-2.0. Headless sign-in: `codex login --device-auth`
  (used live on 2026-10-14).
- **Claude Code sign-in** is interactive (`claude`, then the login prompts).
  Driving it from a non-terminal hangs on the theme picker; the helper must
  hand it a real TTY.
- **Jodd's own requirements of a host** (from the wrapper and setup code): a
  POSIX `sh`, `base64`, `sha256sum` or `shasum`, `mktemp`, `head -c`,
  `setsid` or a working `set -m`, OpenSSH ≥ 7.2 (`restrict`), password login
  for the account at Setup/Update time (plain password or keyboard-interactive
  password prompts; a 2FA prompt is refused), and the CLIs findable through
  the account's login shell (`$SHELL -lc`, then `-lic`).
- **Bake no CLI into a published image.** Redistributing Claude Code in a
  public image is redistributing proprietary software. The image installs both
  CLIs from their official installers **on the user's machine at first start**.

## 4. Terms of use (frame, not legal advice)

Anthropic states that third-party developers may not offer claude.ai login or
rate limits in their products without prior approval. Jodd does not: the user
installs the official CLI on their own server and signs in themselves; Jodd
never sees the login. Every published page therefore:

- says **"your own CLI, your own account, on your own server, under the
  provider's terms"** and links to Anthropic's and OpenAI's terms;
- never markets the feature as a way to use a subscription's quota or to avoid
  API costs;
- says the user is responsible for complying with those terms.

The maintainer may wish to ask Anthropic before promoting the feature widely.
That decision is outside this spec.

## 5. Layout

```
ai-host/                         # public; reaches Jodd-public
├── README.md                    # one screen: what it is, pick a path, links
├── bin/jodd-ai-host             # the helper (POSIX sh), same on every host
├── docker/
│   ├── Dockerfile               # debian:bookworm-slim + OpenSSH, no CLI baked
│   ├── entrypoint.sh
│   ├── sshd_config              # drop-in
│   ├── compose.yaml             # uses ghcr.io/bbm-co-org/jodd-ai-host
│   └── .env.example
└── install/
    ├── linux.sh                 # apt / dnf / apk; root
    └── macos.sh                 # sudo
docs/ai-host/                    # public
├── HOW-IT-WORKS.md              # EN: architecture, security model, limits
├── INSTALL.md / INSTALL.th.md   # every path, networking, hardening, removal
└── USER-GUIDE.md / USER-GUIDE.th.md   # using it in Jodd, every message
.github/workflows/ai-host.yml    # shellcheck + multi-arch image to GHCR
scripts/ai-host-e2e/             # local-only end-to-end tests (Docker)
```

`docs/BACKEND-SSH.md` stays internal (it is in the sync's EXCLUDES).
`docs/ai-host/HOW-IT-WORKS.md` is a public rewrite of its §10, without internal
history or defect references. `scripts/ai-host-docker/` (the existing test
harness with fake CLIs) is unchanged.

## 6. Components

### 6.1 `jodd-ai-host` (the helper)

One POSIX-sh script installed at `/usr/local/bin/jodd-ai-host` on every kind of
host. It reads its settings from `/etc/jodd-ai-host.conf` (`USER=jodd-ai`,
`PORT=…`, `SSHD_DROPIN=…`, `KIND=docker|linux|macos`), written by the installer
or the image. Commands:

| Command | Does | Needs root |
|---|---|---|
| `status` | Account, port, host-key fingerprint, password login on/off, each CLI's path and version (or "not installed"), Jodd keys installed (count of `jodd-ai-*` lines) and wrapper files. | no |
| `fingerprint` | The ED25519 host key's `SHA256:…` fingerprint, in the format Jodd's trust screen shows. | no |
| `signin claude` / `signin codex` | Runs the CLI's own sign-in as the AI account with a real TTY (`claude`; `codex login --device-auth`). | yes (switches user) |
| `password` | Sets a new password for the AI account (prompts). | yes |
| `lock` / `unlock` | Turns password login **for the AI account only** off / on and reloads sshd. Jodd needs it on only during Setup and Update. | yes |
| `update-clis` | Re-runs both official installers as the AI account. | yes |
| `keys` / `revoke <id>` | Lists Jodd's keys (`jodd-ai-<id>` lines) / removes one (same `grep -a` rule as the wrapper's `forget`). | yes |
| `doctor` | Checks every requirement in §3 (`sh`, `head -c`, `setsid`, `restrict` support via `sshd -T`, login-shell PATH finds the CLIs, sshd running, port listening) and prints what to fix. | no |

Every message names the next command to run. Exit codes: 0 ok, 1 a check
failed, 2 usage.

### 6.2 Linux installer (`install/linux.sh`)

`curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/install/linux.sh -o jodd-ai-host-install.sh && sudo sh jodd-ai-host-install.sh`
(the guides show downloading first, so it can be read before it runs). The URL
works only after the snapshot sync has published `ai-host/` to Jodd-public, so
the sync is part of releasing this kit (§6.6). POSIX
sh, idempotent: running it again repairs instead of duplicating.

1. Detect `apt-get` / `dnf` / `apk`. Anything else: stop with a link to the
   manual steps in INSTALL.md.
2. Install `openssh-server curl bash ca-certificates procps` plus `util-linux`
   (setsid) and, on Alpine, `libgcc libstdc++ ripgrep coreutils`.
3. Create the account (`JODD_AI_USER`, default `jodd-ai`) with bash as its
   login shell, and put `~/.local/bin` on its login PATH.
4. Password: `JODD_AI_PASSWORD` if set; else prompt when there is a TTY; else
   generate one and print it once.
5. sshd drop-in `/etc/ssh/sshd_config.d/50-jodd-ai.conf`:
   `Match User <user>` → `PasswordAuthentication yes`, `ClientAliveInterval 30`,
   `ClientAliveCountMax 4`. **The server's other accounts keep their settings.**
   Make sure the main `sshd_config` includes `sshd_config.d/*.conf` (add the
   line if a distro lacks it), then validate with `sshd -t` and confirm the
   effect with `sshd -T -C user=<user>,host=x,addr=127.0.0.1` before reloading.
   If validation fails, restore the previous file and stop.
6. Install both CLIs as the account via their official installers. A failure
   (no network) is reported and does not undo the rest; `update-clis` retries.
7. Enable and (re)start sshd (`systemctl`, `rc-service` or `service`; in a
   container without an init system, say how to start it).
8. Install the helper and its config, run `jodd-ai-host doctor`, and print the
   next steps: host, port, account, fingerprint, the two `signin` commands,
   and "then in Jodd: AI settings → Agent CLI → Runs on → A server".

### 6.3 macOS installer (`install/macos.sh`)

Run with `sudo`. Creates a standard account (`sysadminctl -addUser`, hidden
from the login window), installs both CLIs as it, writes the same sshd drop-in
(macOS's `sshd_config` includes `sshd_config.d`; verified at implementation),
installs the helper. **Remote Login** is turned on with
`systemsetup -setremotelogin on` when the terminal has Full Disk Access;
otherwise the script prints the exact System Settings path
(General → Sharing → Remote Login, allow the new account) and waits for the
user to come back. Sleep: prints the setting to change (or `--no-sleep` runs
`pmset -c sleep 0`), because a sleeping Mac is an unreachable host.

### 6.4 Docker image (`ghcr.io/bbm-co-org/jodd-ai-host`)

- `debian:bookworm-slim`, OpenSSH, `bash curl ca-certificates procps
  util-linux ripgrep`, the helper. **No CLI in the image.**
- The account is `jodd-ai`, same as the scripts.
- Volumes: `home` (CLI binaries and sign-ins, `authorized_keys`, `~/.jodd`) and
  `state` (host key, so the fingerprint never changes across upgrades;
  first-start password).
- First start: generate the host key; set the password from
  `JODD_AI_PASSWORD`, or generate one, store it in `state` and print it once in
  the log; install both CLIs into `home` if missing (a failure keeps sshd up and
  logs the retry command); print the fingerprint and next steps in the log.
- `compose.yaml`: `init: true`, `restart: unless-stopped`, port
  `${JODD_AI_PORT:-2222}:22` on all interfaces (a phone on the same Wi-Fi must
  reach it), with the exposure explained in a comment and in INSTALL.
- Workflow `.github/workflows/ai-host.yml`: on changes under `ai-host/**` and
  weekly (base-image security updates), run shellcheck on every script, build
  linux/amd64 and linux/arm64, push `latest` and `YYYY-MM-DD` tags, with
  `org.opencontainers.image.source` pointing at Jodd-public. Making the package
  **public** is a one-time setting the maintainer changes on GitHub.
  *(Amended 2026-10-15, after merge:)* the push happens only in **Jodd-public**
  (its `main` after a snapshot sync, and its weekly schedule). The private repo
  builds without pushing, so development on its `main` never reaches users
  before the sync that also updates the guides and installer URLs. The package
  is connected to Jodd-public and grants it Write under Manage Actions access.
  Pushes to `main` are not path-filtered: a synced snapshot has no parent
  commit, so a `paths` filter matched nothing and the first sync published
  nothing until the workflow was dispatched by hand.

### 6.5 Guides

- **ai-host/README.md**: what it is, the terms-of-use frame, the three paths as
  copy-paste blocks, links.
- **HOW-IT-WORKS.md**: the parts (Jodd, SSH, restricted key, wrapper, CLI,
  provider); what Setup does with the password; what the key can and cannot do
  (it runs only the wrapper; **Codex can read any file the account can**, hence
  a dedicated account); where data goes (device → your server → Anthropic /
  OpenAI); Stop and its limits (a silent network drop is bounded by sshd's
  keepalive and the 120 s / 180 s timeout); protocol versions and **Update**;
  what is stored where.
- **INSTALL(.th).md**: requirements; Docker (GHCR); Ubuntu/Debian; Fedora/RHEL
  and Alpine (same script); macOS; other systems (manual steps that reproduce
  §6.2 by hand); signing in; **networking** (same Wi-Fi: the LAN address;
  away from home: Tailscale or another VPN, **not** a router port forward with
  password login on); hardening (`lock` after Setup, `unlock` before Update,
  firewall); updating; removing everything; troubleshooting with `doctor`.
- **USER-GUIDE(.th).md**: the Jodd side: where the setting is, setup screens
  (fingerprint check against `jodd-ai-host fingerprint`), Test, choosing Claude
  Code or Codex, Update, Remove, Stop, AI consent per account and per device,
  and **every message Jodd can show** with what to do.

### 6.6 Website, app and release

- **Jodd-public**: the kit and guides reach it only through
  `sync-to-public.sh`, which the maintainer runs (it force-pushes). Until then
  the raw URLs in the guides return 404, so the website page goes live after
  the sync, not before.

- **jodd.bbmedia.co.th** (`~/projects/Jodd-Homepage`): an `ai-host.html` page
  (EN with a TH section) summarising the paths and linking to the GitHub guides;
  `privacy.html` gains a paragraph on the AI host data path. **Pushing that
  repo's `main` publishes the site**, so the change is prepared on a branch and
  pushed only with the user's explicit go-ahead.
- **Jodd app**: the AI host panel gains a "How to set up a server" link to the
  website page. This is the only app change.

## 7. Testing

- **Shell:** shellcheck and `sh -n` on every script (in the workflow and
  locally). The helper's pure functions (parsing `authorized_keys`, building
  the drop-in) get small shell tests.
- **Installer end to end** (`scripts/ai-host-e2e/`, local, Docker): run
  `install/linux.sh` with real network installs in `ubuntu:24.04`, `debian:12`,
  `fedora:latest` and `alpine:latest`. Then point Jodd's real setup code at
  each one (the `#[ignore]` Docker tests, `JODD_AI_DOCKER`) and assert: setup
  succeeds, `status` lists both CLIs at absolute paths, `jodd-ai-host doctor`
  passes, `lock` makes setup fail cleanly with Jodd's password-rejected
  message, `unlock` makes it work again, `revoke` removes exactly one line, a
  re-run of the installer changes nothing. Real CLIs are not signed in during
  tests: run and Stop are already proven with stand-in CLIs.
- **Image:** build locally, `docker compose up -d`, the same assertions, plus
  the fingerprint and sign-in state surviving `docker compose up -d
  --force-recreate` and an image upgrade.
- **macOS:** run `install/macos.sh` on a real Mac **only with the user's
  explicit permission** (it creates an account and changes Sharing settings);
  until then the guide marks the macOS path as not yet verified.
- **Docs:** every command in the guides is one the tests ran; the docs say
  what was verified and when.

## 8. Non-goals

- Windows as a host.
- Opening router ports, dynamic DNS or setting up Tailscale for the user (the
  guides explain them).
- Signing in to the CLIs automatically.
- Monitoring, multi-user hosts, or quotas.
- Changing Jodd's protocol or wrapper.

## 9. Open items (decided during implementation, by measurement)

1. Whether macOS's `sshd_config` includes `sshd_config.d/*.conf` on the
   user's macOS version, and whether `ClientAlive*` is accepted inside `Match`
   on every target sshd (`sshd -t` decides; if not, the drop-in sets it
   globally and the guide says so).
2. The exact install location each official installer uses on Alpine (musl) and
   whether Codex's installer supports it; if not, Alpine documents Claude only.
3. The fingerprint format Jodd's trust screen shows, so `jodd-ai-host
   fingerprint` matches it character for character.
