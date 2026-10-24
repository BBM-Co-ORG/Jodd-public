[ภาษาไทย](USER-GUIDE.th.md)

# Using an AI host in Jodd

This guide covers the Jodd side: connecting Jodd to your AI host, choosing
Claude Code or Codex, and what to do about every message Jodd can show. To
prepare the host first, see the [install guide](INSTALL.md).

**Your own CLI, your own account, on your own server, under the provider's
terms.** Jodd runs the CLI you installed and signed in to yourself; it never
sees that sign-in. You are responsible for using it as Anthropic's terms
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

## Where the setting is

There are two places, and both look the same:

- **App settings** (the gear button next to Jodd's version number) → **LLM provider**.
  This is the provider for Ask Jodd. Tick **Also use for Extract and auto-link
  on accounts with no provider of their own** to let accounts use it too.
- **Account settings** (the gear button next to an account in the account list) → **LLM Provider**.
  This is one account's own provider, for Extract and auto-link.

In either place, choose **Agent CLI (headless)**, then under **Runs on** choose
**A server**. On desktop the other choice is **This computer**. On Android,
**A server** is the only choice.

Each device sets up its own connection with its own key. To use the host from
your phone and your computer, set it up on both. The host then has one key for
each device.

## Set up a server

Before you start, sign in to the CLIs on the host
([install guide](INSTALL.md#sign-in-to-the-clis)). Have the host's fingerprint
at hand: run `jodd-ai-host fingerprint` on the host (Docker:
`docker compose exec ai-host jodd-ai-host fingerprint`).

Commands in this guide are written for Linux and macOS. On the Docker host,
drop `sudo` and put `docker compose exec ai-host` in front: `sudo jodd-ai-host lock`
becomes `docker compose exec ai-host jodd-ai-host lock`.

**Screen 1: the address.** Fill in:

- **Server**: the host's address only, without a user name or `@`.
- **Port**: `22`, or `2222` for the Docker host. Empty means 22.
- **Username**: `jodd-ai`.

Press **Continue**. Jodd connects and reads the host's identity.

**Screen 2: trust.** Jodd shows *First time connecting to …* and the host's
identity, for example `SHA256:dswu8P+mdeYHwkh6suxXWrbP9Mj1h0vpaVzXYZssz60 (ED25519)`.

- Compare the `SHA256:` part with what `jodd-ai-host fingerprint` printed on
  the host. It must match exactly. If it does not, press **Back** and stop.
- Read the note: Jodd's key can only run Claude Code or Codex, but Codex can
  read any file the account can. That is why the host uses a dedicated account.
- Enter the account's **Password** and press **Trust and set up**.

Jodd signs in once with the password, installs its helper and its key, then
checks that the key works. This takes a few seconds. Jodd does not keep the
password.

**Screen 3: the server.** Jodd shows **Server:** and the host's name, and one
line per CLI: **Claude Code** and **Codex CLI**, each with its path on the host
or **not installed**. Below are **Update** and **Remove**.

Now is a good time to turn password login off on the host:
`sudo jodd-ai-host lock` (Docker: `docker compose exec ai-host jodd-ai-host lock`;
see the [install guide](INSTALL.md#harden)).

## Choose Claude Code or Codex

Under the server, open **Choose a CLI…** and pick **Claude Code** or **Codex
CLI**. With **A server**, only these two are offered, and only the ones
installed on the host can be picked. Press **Save**.

The two differ in one way that matters for privacy:

- **Claude Code** runs with its tools turned off. It answers from what Jodd sends.
- **Codex** runs in a read-only sandbox, but it **can read any file on the host
  that the account can read**. That includes files every user can read, such as
  most of `/etc` and, on many Linux systems, other users' home folders. On a
  Mac, every account is in the `staff` group by default and can enter other
  people's home folders, where only Desktop, Documents, Downloads, Movies,
  Music, Pictures and Library are closed. The installer gives `jodd-ai` a group of its own, so it is not
  in `staff`. On a Mac you also use, close your own home folder with
  `chmod 700` anyway, and keep secrets out of places other accounts can read.
  Jodd shows a note about this under the choice. On a host in Docker, Codex
  sees only the container. See [Harden](INSTALL.md#harden) in the install guide.

## Test

After **Save**, press **Test connection**. It runs the saved settings through
one real extraction, which usually takes 10 to 20 seconds and counts against
your plan with the provider like any other request. The result shows **OK** or
**Failed** and the time it took. When Jodd recognises the problem, it says the
cause and what to do; otherwise it shows the CLI's own message.

Test runs the **saved** settings. If you changed something, press **Save** first.

## AI permission, per account and per device

Jodd sends an account's notes to an AI provider only when that account allows
it. This is the same for an AI host as for any other provider:

- In **Account settings**, tick **Allow this account’s data in Jodd-managed AI**.
- Or, when you run an AI action on an account that does not allow it yet, Jodd
  asks **Allow AI for this account?** Choose **Allow AI for this account** to
  allow it, then run the action again.

These settings live on each device, like the AI host connection itself.
Allowing AI for an account on your computer does not allow it on your phone.

What leaves your device is the same as with any provider: masking and hidden
folders apply first. The text goes to your host, and from there to Anthropic or
OpenAI. Nothing goes to Jodd's developers. Details are in
[How it works](HOW-IT-WORKS.md#where-your-data-goes).

## Stop a run

Cancel an AI action the usual way: **Cancel** in Ask Jodd, **Cancel extraction**
in Extract. Jodd hangs up, and the host ends the CLI **within seconds**.

If the network drops without a hang-up reaching the host, the host notices
after **about 2 minutes**. And every run stops on its own after **120 seconds**
(Claude Code) or **180 seconds** (Codex).

## Update

Use **Update** when:

- Jodd says the host **needs an update** (after you update Jodd), or shows
  **Jodd's helper on this server needs an update.**
- You installed a CLI on the host after Setup, and Jodd still shows it as
  **not installed**.

Steps:

1. On the host, turn password login on: `sudo jodd-ai-host unlock`
   (Docker: `docker compose exec ai-host jodd-ai-host unlock`).
2. In Jodd, press **Update**, enter the **Password**, and press **Update now**.
3. On the host, turn password login off again: `sudo jodd-ai-host lock`
   (Docker: `docker compose exec ai-host jodd-ai-host lock`).

Update keeps the same key. If it fails, Jodd keeps its key, so you can fix the cause and try again.

## Remove

Press **Remove**, then **Remove server**. This takes effect at once, not when
you press **Save**:

- Jodd deletes its key's line on the host, then the key on this device.
- The CLIs on the host and your sign-ins there are not touched.
- If the host cannot be reached, Jodd only forgets it on this device. Remove
  the key on the host yourself: `sudo jodd-ai-host keys` lists Jodd's keys,
  and `sudo jodd-ai-host revoke <id>` removes one.

Then choose another provider. In **App settings**, Jodd says
**Server removed. Choose an AI provider.** In **Account settings**, the account
follows the app's provider if the app shares one with accounts.

## Messages and what to do

Jodd's messages name your host where this table says *host*, and the CLI
(`claude` or `codex`) where it says *cli*. Some arrive with a short prefix such
as `transport error:` or `provider not configured:`. Where a message says
**AI settings**, it means the **LLM provider** section of **App settings** or
of **Account settings**, whichever you set the server up in.

Where the table says `sudo jodd-ai-host …`, on the Docker host run
`docker compose exec ai-host jodd-ai-host …` instead.

### While setting up

| Message | Why | What to do |
|---|---|---|
| Enter the server name only, without “@”. Put the login name in Username. | The **Server** field contains `@`. | Put only the address in **Server** and `jodd-ai` in **Username**. |
| Port must be a number between 1 and 65535. | The **Port** field is not a port number. | Enter `22`, or `2222` for Docker. |
| Enter a username. | **Username** is empty. | Enter `jodd-ai`. |
| Enter the password. | **Password** is empty. | Enter the account's password. |
| The SSH target is empty. | **Server** is empty. | Enter the host's address. |
| An SSH target cannot contain spaces. | **Server** has a space or another invisible character in it. | Enter only the address, without spaces. |
| An SSH target cannot start with '-'. | **Server** starts with `-`. | Enter only the address. |
| could not reach *host*: no host key returned | On desktop, after **Continue**: nothing answered SSH at that address and port. Usually a wrong address, the wrong port (`22` vs `2222` for Docker), a host that is off or asleep, or a different network. | Check the address and port; check the host is on and reachable ([Connect from Jodd](INSTALL.md#connect-from-jodd)). On the host: `sudo jodd-ai-host doctor`. |
| could not run ssh-keyscan: … | On desktop, after **Continue**: Jodd could not start `ssh-keyscan`, the SSH tool it uses to read the host's identity. | Make sure this computer has the OpenSSH client installed. |
| Can't reach the server. Check the address, and that the server is running. | Jodd cannot connect: wrong address or port, the host is off or asleep, or your device is on another network. | Check the address and port. On the same Wi-Fi, use the address the installer printed; away from home, use [Tailscale](INSTALL.md#away-from-home-tailscale). On the host: `sudo jodd-ai-host doctor`. |
| This server does not accept password sign-in, which setting up an AI host needs once. | Password login for the account is off, usually because you ran `lock`. Or the server's sshd has a global `AuthenticationMethods publickey`, which refuses a password even though the kit allows one for the account. | `sudo jodd-ai-host unlock`, set up or update, then `sudo jodd-ai-host lock`. If it was not locked, run `sudo jodd-ai-host doctor`: it names the sshd setting to change. |
| The server rejected that username or password. | Wrong password, or a username other than the kit's account. On a Mac, also when the account is not on Remote Login's allow-list. Or the server's sshd has `AllowUsers` or `AllowGroups` that leave the account out (or `DenyUsers` / `DenyGroups` that name it). | Check the username (`jodd-ai`) and the password. Set a new password with `sudo jodd-ai-host password`. On a Mac, run the installer again. If the password is right, run `sudo jodd-ai-host doctor`: it names the sshd setting to change. |
| The server's identity does not match the fingerprint you confirmed. Nothing was changed. | The host's key changed after you confirmed it, or you are not talking to your host. | Run `jodd-ai-host fingerprint` on the host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`) and start again. If the fingerprint keeps changing, stop and find out why. |
| The server stopped answering. Check your connection and try again. | The connection went quiet during setup. | Check the network and try again. |
| Could not talk to the server: … | The connection broke during setup. | Try again. If it keeps failing, `sudo jodd-ai-host doctor`. |
| Could not connect: … | Another connection problem; the rest of the message says which. | Act on the rest of the message, then `sudo jodd-ai-host doctor`. |
| the server has neither sha256sum nor shasum | The host lacks a tool Jodd needs. | Install `coreutils` on the host. `sudo jodd-ai-host doctor` lists anything else missing. |
| Installing on the server failed: … | Jodd could not install its helper or key; the rest says why. | `sudo jodd-ai-host doctor`, fix what it reports, try again. |
| Jodd may have left an entry ending in jodd-ai-… in ~/.ssh/authorized_keys on … (and a file ~/.jodd/ai-run-v2); its key no longer exists on this computer, so you can remove that line. | Setup failed after it had started changing the host. | Remove that key on the host: `sudo jodd-ai-host revoke <the 8 characters after jodd-ai->`. Then fix the cause and set up again. |
| The AI host did not answer the status check (exit …). … | Jodd's helper on the host did not answer as expected, for example because `~/.jodd` was deleted. | Unlock, press **Update**, lock. If that fails, **Remove** and set up again. |
| Jodd's key is missing: … | This device lost its key for the host. | **Remove**, then set up again. |
| this AI host has no usable key directory | This device's saved settings for the host are damaged. | **Remove**, then set up again. |

### In the server panel and the provider settings

| Message | Why | What to do |
|---|---|---|
| Jodd's helper on this server needs an update. | Jodd was updated and needs a newer helper on the host. | [Update](#update). |
| **not installed** (next to Claude Code or Codex CLI) | Jodd did not find that CLI on the host when it last set up or updated. | `sudo jodd-ai-host update-clis`, then [Update](#update). |
| Not installed on the server — install it there, or extraction will fail. | The CLI you picked is not on the host. | Same as above, or pick the other CLI. |
| Set up your server above before saving. | **A server** is chosen but no server is set up. | [Set up a server](#set-up-a-server), or choose **This computer**. |
| save: set up your server first, or choose This computer. | You pressed **Save** with **A server** and no server. | Same as above. |

### When AI runs, or in Test

| Message | Why | What to do |
|---|---|---|
| Couldn't reach AI host *host*: … | Your device cannot reach the host: it is off or asleep, you are away from its network, Tailscale is off, or the address changed. | Wake the host or check it is on. Away from home, turn Tailscale on. On the host: `sudo jodd-ai-host doctor`. |
| The AI host *host*'s identity changed. Set it up again in AI settings. | The host's key changed. Usually the host was reinstalled, or the Docker `state` volume was deleted. It can also mean someone is in between. | Check `jodd-ai-host fingerprint` on the host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`). If you know why it changed, **Remove** and set up again, confirming the new fingerprint. If you don't, stop. |
| The AI host *host* no longer accepts Jodd's key. Set it up again in AI settings. | Jodd's key line is gone from the host: removed with `revoke`, the account was recreated, or the Docker `home` volume was deleted. | **Remove**, then set up again. |
| The AI host *host* is not set up. Set it up in AI settings. | This device has no saved connection for the host. | [Set up a server](#set-up-a-server). |
| The AI host *host* needs an update. Open AI settings and choose Update. | Jodd was updated and needs a newer helper on the host. | [Update](#update): unlock, **Update**, lock. |
| *cli* isn't installed on *host*. | The CLI is missing on the host, or was installed after Setup. | `sudo jodd-ai-host update-clis`, then [Update](#update). |
| Claude Code is not signed in. | Claude Code on the host has no sign-in, or it expired. Jodd adds *Do this on host, over SSH.* | `sudo jodd-ai-host signin claude`, then test again. |
| *cli* timed out after *N*s on *host* | The run hit its limit (120 s for Claude Code, 180 s for Codex). | Try again with less text. |
| *cli* timed out after *N*s on *host* — no output at all; the CLI may have been waiting for interactive input | The CLI waited for something, usually a sign-in or a first-run question. | `sudo jodd-ai-host signin claude` or `sudo jodd-ai-host signin codex`, finish its prompts, then test again. |
| AI host *host* did not answer within *N*s | The connection stalled beyond the run's limit. | Check the network and the host's load, then try again. |
| *cli* on *host* ended without an exit status | The connection dropped, or the CLI was killed on the host (for example, out of memory). | Try again. If it repeats, check the host has 4 GB of memory free. |
| *cli* on *host* exit *code*: … | The CLI itself failed; the rest is its own message. | Act on the CLI's message. A sign-in problem: `sudo jodd-ai-host signin claude` or `sudo jodd-ai-host signin codex`. A plan or usage limit: check your account with the provider. |
| CLI produced no output | The CLI finished without an answer. | Try again. If it repeats, sign in again on the host. |
| AI host *host* refused the request | The host's SSH server refused to run Jodd's request. | `sudo jodd-ai-host doctor`, then unlock, **Update**, lock. |
| AI host *host*: … | Another SSH problem; the rest says which. | Act on the rest of the message; `sudo jodd-ai-host doctor`. |
| AI data access is not allowed for this account. Allow it to continue. | The account has not allowed AI ([AI permission](#ai-permission-per-account-and-per-device)). | Choose **Allow AI for this account**, or tick **Allow this account’s data in Jodd-managed AI** in Account settings. |
