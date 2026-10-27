# Jodd AI host kit

An **AI host** is a machine you own where Jodd runs Claude Code or Codex for you.
Jodd connects to it over SSH with a key that can do one thing only: run one of
those two CLIs. This lets Jodd use them from Android, and from any computer that
does not have them installed.

This kit prepares that machine: a dedicated account (`jodd-ai`), password login
for that account only, both CLIs from their official installers, and a helper
command, `jodd-ai-host`.

**Your own CLI, your own account, on your own server, under the provider's
terms.** You install the CLI and sign in to it yourself. Jodd never sees that
sign-in. You are responsible for using it as Anthropic's terms
([consumer](https://www.anthropic.com/legal/consumer-terms),
[commercial](https://www.anthropic.com/legal/commercial-terms)) or
OpenAI's [consumer terms](https://openai.com/policies/terms-of-use) or
[business agreement](https://openai.com/policies/services-agreement/) allow.

**Scope and provider terms.** This setup is for one person using their own
provider account across their own devices. SSH connectivity and successful
sign-in do not establish permission for every app integration. Jodd submits
headless requests from its note features; it is more than a remote terminal.
Do not share a personal subscription, pool accounts, or resell access. Separate
OS accounts alone do not authorize a multi-user service. Account type,
authentication method, provider policies and any required provider agreement
still apply.

## Pick a path

**Docker** (any machine with Docker and the Compose plugin):

```sh
curl -fsSLO https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/docker/compose.yaml
docker compose up -d
docker compose logs
```

**Linux** (Ubuntu, Debian, Fedora and Alpine tested; RHEL not yet tested). Download, read, then run:

```sh
curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/install/linux.sh -o jodd-ai-host-install.sh
less jodd-ai-host-install.sh
sudo sh jodd-ai-host-install.sh
```

**macOS 13 or newer** (a Mac that stays on). Tested on one Apple Silicon Mac on
macOS 27; try the dry run first, it changes nothing:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/install/macos.sh -o jodd-ai-host-install.sh
sh jodd-ai-host-install.sh --dry-run
sudo sh jodd-ai-host-install.sh
```

**On a VPS or any machine reachable from the internet, read the
[install guide](../docs/ai-host/INSTALL.md) first.** Password login for the
AI account is open until you lock it, and the Docker port is published on
every address unless you set `JODD_AI_BIND`.

## Then

The commands below are for Linux and macOS. In Docker, drop `sudo` and put
`docker compose exec ai-host` in front, as shown in parentheses.

1. Sign in to the CLIs on the host, as yourself:
   `sudo jodd-ai-host signin claude` and `sudo jodd-ai-host signin codex`
   (Docker: `docker compose exec ai-host jodd-ai-host signin claude`, and the same with `codex`).
2. In Jodd: **App settings** → **LLM provider** → **Agent CLI (headless)** → **Runs on: A server**.
   Enter the host's address, its port (Docker: 2222), the user `jodd-ai` and the password.
   Check that the fingerprint matches `jodd-ai-host fingerprint`
   (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`).
3. After Setup, turn password login off: `sudo jodd-ai-host lock`
   (Docker: `docker compose exec ai-host jodd-ai-host lock`).

## Guides

- [Install guide](../docs/ai-host/INSTALL.md): every path, networking, hardening, updating, removing, troubleshooting.
- [User guide](../docs/ai-host/USER-GUIDE.md): the Jodd side, and every message Jodd can show with what to do.
- [How it works](../docs/ai-host/HOW-IT-WORKS.md): what the key can and cannot do, where your data goes, what is stored where.
