# How the Jodd AI host works

This page explains what happens between Jodd and your AI host, what Jodd's key
can and cannot do, and where your data goes. To set a host up, see the
[install guide](INSTALL.md). For the Jodd side, see the [user guide](USER-GUIDE.md).

**Your own CLI, your own account, on your own server, under the provider's
terms.** You install Claude Code or Codex on a machine you control and sign in
to it yourself. Jodd never sees that sign-in and never offers one of its own.
You are responsible for using the CLI as Anthropic's terms
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

## The parts

```text
 Your device                 Your AI host                               The provider
 ───────────                 ────────────                               ────────────
 Jodd ──── SSH, Jodd's key ──▶ account jodd-ai
                               └─ sshd forces Jodd's key to run:
                                  ~/.jodd/ai-run-v2  (the wrapper)
                                  └─ claude  or  codex  ──── HTTPS ───▶ Anthropic / OpenAI
                                     (signed in by you)
 Jodd ◀─── the answer ──────── wrapper ◀── CLI ◀────────────────────── answer
```

- **Jodd** (desktop or Android) opens a fresh SSH connection for each AI request.
- **The account** `jodd-ai` exists only for this. The kit creates it.
- **The wrapper** is a small shell script that Jodd installs in `~/.jodd`. It
  accepts three requests: report status, run Claude Code or Codex with a prompt,
  and remove Jodd's key. It refuses everything else.
- **The CLI** is the official `claude` or `codex` program, installed by its
  vendor's own installer, signed in by you.
- **The provider** (Anthropic or OpenAI) answers the CLI under your account.

## What Setup does with your password

In Jodd you enter the host, the port, the user and the password once.

1. Jodd shows the host's fingerprint. You compare it with what the host prints
   (`jodd-ai-host fingerprint`) and confirm it. Jodd pins that key: it never
   connects to a server whose key differs.
2. Jodd makes a new SSH key on your device.
3. Jodd signs in **once** with the password. It looks for `claude` and `codex`
   the way a login shell finds them, installs the wrapper as
   `~/.jodd/ai-run-v2`, and adds one line for its key to
   `~/.ssh/authorized_keys`. The line ends in `jodd-ai-` and 8 characters that
   identify this device's key.
4. Jodd disconnects, then connects again **with the key** and asks the wrapper
   for its status. That proves the restricted key works.

The password is used for that one connection. Jodd does not store, log or show
it. **Reinstall helper** uses it again the same way.

Because Jodd needs the password only during Setup, Change server and Reinstall helper, the kit lets you
turn password login off in between: `sudo jodd-ai-host lock`, and
`sudo jodd-ai-host unlock` before the next one.

## What the key can do, and what it cannot stop

The line in `authorized_keys` starts with `restrict,command="…/.jodd/ai-run-v2 --key …"`.
That means:

- **No shell and no terminal.** Whatever command Jodd's key asks for, sshd runs
  the wrapper instead.
- **No forwarding** of ports, agents or X11.
- **Only two programs.** The wrapper runs Claude Code or Codex, at the paths it
  found during Setup. Any other request is refused.

What the key cannot stop is what the CLI itself may do while it runs:

- **Claude Code** runs with all its built-in tools turned off, so it answers
  from the prompt alone.
- **Codex** runs in its read-only sandbox. Read-only blocks writing, not
  reading: **Codex can read any file the account can read**, and what it reads
  can end up in the answer that comes back to Jodd.

"Any file the account can read" is more than the account's own home. On a
normal Linux or Mac it includes everything readable by all users, such as most
of `/etc` and `/Users/Shared` on a Mac. It can include other people's home
folders too:

- Many Linux systems leave home folders readable by every account.
- On a Mac, every account is in the `staff` group by default, and every home
  folder lets `staff` in. Inside it only Desktop, Documents, Downloads, Movies,
  Music, Pictures and Library are closed; everything else that is not closed separately can be
  read. So the macOS installer gives `jodd-ai` a group of its own instead of
  `staff`, and closes `jodd-ai`'s own home folder to other accounts. Its
  summary lists the home folders that are still open, with the command that
  closes each one.

On a Mac you also use, close your own home folder (`chmod 700 /Users/<you>`)
and keep secrets such as API keys and `.env` files out of places other
accounts can read.

So the kit creates a dedicated account, `jodd-ai`, with nothing else in it.
Do not put other files there. Do not give the account `sudo`. The installers
refuse an existing account that can become root. Beyond that, use a machine or
virtual machine that does nothing else, or the Docker image: in a container,
Codex sees only the container's files and its two volumes, not the rest of the
machine.

## Where your data goes

1. On your device, Jodd prepares the request. Masking and hidden folders apply
   exactly as for any other AI provider.
2. The request travels over SSH to your host. On the host, the wrapper writes
   the prompt to a temporary folder for that run and deletes the folder when
   the run ends, whether it worked, failed or was stopped.
3. The CLI sends the request to Anthropic or OpenAI, under the account you
   signed in with, and receives the answer.
4. The answer travels back to your device the same way.

**Jodd's developers receive nothing.** There is no Jodd server in this path.
What each CLI keeps in its own folders on the host (`~/.claude`, `~/.codex`)
follows that CLI's own rules. Jodd asks Codex not to save a session file.

## Stopping a run

When you cancel an AI action in Jodd (for example **Cancel** in Ask Jodd),
Jodd hangs up the connection. The wrapper sees that and ends the CLI on the
host **within seconds**.

There are two limits:

- If the network drops silently (no hang-up reaches the host), the host
  notices only through SSH keepalive. The kit sets it to check every 30
  seconds and give up after 4 missed answers: **about 2 minutes**.
- **Every run has a time limit**: 120 seconds for Claude Code and 180 seconds
  for Codex. The wrapper ends the CLI when it is reached, whatever else happens.

## Versions and Update

The wrapper has a version number (it is part of its file name). When a newer
Jodd needs a newer wrapper, Jodd says the host **needs an update**. Press
**Reinstall helper** in Jodd's settings and enter the password: Jodd installs the new
wrapper next to the old one and refreshes its key's line.

Each Jodd device keeps working with its own wrapper version, so a phone and a
computer on different Jodd versions can share one host. Old wrapper files that
no key uses any more are deleted at the next Setup or Reinstall helper.

Use **Reinstall helper** too after you install a CLI on the host later: the wrapper
learns the CLI's location only during Setup and Reinstall helper.

## What is stored where

| Where | What |
|---|---|
| Your device | Jodd's private key for this host and the host's pinned public key, in Jodd's own data folder. Nothing else about the host is secret. |
| The host, in `~/.ssh/authorized_keys` | One line per Jodd device, ending in `jodd-ai-` and an 8-character id. |
| The host, in `~/.jodd/` | The wrapper, `ai-run-v2` (one file per version in use). |
| The host, in the CLIs' own folders | Their programs and your sign-in, written by the CLIs themselves. Jodd never reads them. |
| The host, in `/etc` | The kit's settings: `/etc/jodd-ai-host.conf` and the sshd rule `/etc/ssh/sshd_config.d/50-jodd-ai.conf`. In Docker, the rule and the host key live in the `state` volume. |
| The host, in `/var/lib/jodd-ai-host` | Linux: only if you set `JODD_AI_PASSWORD`, a salted hash of it, readable by root only, so a later run can tell whether you changed it. Docker: this is the whole `state` volume: the host key (so the fingerprint survives upgrades), the sshd rule (so `lock` survives), and that password hash. |

Jodd stores the password nowhere. The host keeps the password of `jodd-ai`
the way the operating system keeps every account's password.

## Removing

- **Remove** in Jodd's settings takes effect at once. Jodd asks the wrapper to
  delete its key's line from `authorized_keys`, then deletes the key on your
  device. If the host cannot be reached, Jodd only forgets it on your device;
  remove the line on the host yourself (below).
- On the host, `sudo jodd-ai-host keys` lists Jodd's keys by id, and
  `sudo jodd-ai-host revoke <id>` removes one. The device that used it will
  then report that the host no longer accepts its key.

To take the whole kit off a machine, see
[Remove everything](INSTALL.md#remove-everything) in the install guide.
