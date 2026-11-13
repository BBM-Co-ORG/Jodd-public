[ภาษาไทย](USER-GUIDE.th.md)

# Using an AI host in Jodd

This guide covers the Jodd side: connecting Jodd to your AI host, choosing
Claude Code or Codex, and what to do about every message Jodd can show. The
host itself, and the AI host kit that runs on it, live in their own
repository, [BBM-Co-ORG/jodd-ai-host](https://github.com/BBM-Co-ORG/jodd-ai-host).
To prepare the host first, see its
[install guide](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.md).

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

- **App settings** (the gear button next to Jodd's version number) → **AI provider**.
  This is the provider for Ask Jodd. Tick **Also use for Extract and auto-link
  on accounts with no provider of their own** to let accounts use it too.
- **Account settings** (the gear button next to an account in the account list) → **LLM Provider**.
  This is one account's own provider, for Extract and auto-link.

In either place, choose **Agent CLI (headless)**, then under **Runs on** choose
**A server**. On desktop the other choice is **This computer**. On Android,
**A server** is the only choice.

Each device sets up its own connection with its own key. To use the host from
your phone and your computer, set it up on both. The host then has one key for
each device. Once the host is locked, set up the next device
[by command](#set-up-by-command); there is no need to unlock it.

## Set up a server

**Setup needs the AI host kit on the host.** Install it first, with Docker or
the installer for Linux or macOS
([install](https://github.com/BBM-Co-ORG/jodd-ai-host#install)). Jodd checks
for the kit before it changes anything: on a server without it, Setup stops
with *This host needs the AI host kit* and leaves the server as it was.

Then sign in to the CLIs on the host
([install guide](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.md#sign-in-to-the-clis)),
and have the host's fingerprint at hand: run `jodd-ai-host fingerprint` on the
host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`).

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
- Choose how Jodd's key reaches the host: **Password** (the default; enter the
  account's password) or **Run a command on the host**
  ([below](#set-up-by-command)).
- With **Password**, press **Trust and set up**.

Jodd signs in once with the password, checks that the kit is installed, adds
one line for its key that runs only the kit's `jodd-ai-mcp`, then checks that
the key works. This takes a few seconds. Jodd does not keep the password.

**Screen 3: the server.** See [What the server panel shows](#what-the-server-panel-shows).
Below the list are **Change server** and **Remove**.

**Then turn password login off on the host.** Jodd reminds you:
*Next, on the host: run `sudo jodd-ai-host lock` so the AI account can no
longer log in with its password.* Run it now. Jodd uses its key from here on
and does not need the password again until you change server
([Harden](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.md#harden)
in the install guide).

### Set up by command

**Run a command on the host** needs no password and no `unlock`. It needs
**AI host kit 1.1.0 or later** on the host.

1. On screen 2, choose **Run a command on the host** and press **Trust and
   set up**. Jodd makes its key and shows two commands, **On the host** and
   **Docker**, each with **Copy**.
2. Run the one that fits on the host, as an administrator:
   `sudo jodd-ai-host add-key '<Jodd's key>'`, or on the Docker host
   `docker compose exec ai-host jodd-ai-host add-key '<Jodd's key>'`. Copy it
   from Jodd rather than typing it.
3. Press **I've run it**. Jodd checks that the key works and saves the host.
   If it says the host **doesn't accept Jodd's key yet**, the command did not
   run or the host's kit is older than 1.1.0; fix that and press **I've run it**
   again. Jodd keeps the key meanwhile.

**Back** or **Cancel** deletes the key Jodd made, and removes it from the host if
the command already ran. The command works whether or not password login is
locked, so there is no `unlock` and `lock` here.

## Looking after the host

With kit 1.1.0 or later the host can stay locked for good. Each job below has
a way that needs no password:

| Job | What to do | Needs `unlock`? |
|---|---|---|
| Add another device | On that device, [Set up by command](#set-up-by-command). | No |
| Jodd says the host **needs an update**, or a CLI shows **not installed** (old helper only) | [Reinstall helper](#reinstall-helper) → **Run a command on the host**. | No |
| The host's address, port or login changed | [Change server](#change-server) → **Run a command on the host** on the new host. | No |
| Stop using a device | In Jodd on that device, [Remove](#remove). | No |
| A device is lost | On the host, `sudo jodd-ai-host keys` lists Jodd's keys, one per device; `sudo jodd-ai-host revoke <id>` removes one. | No |
| Upgrade the kit | Re-run the installer, or `docker compose pull && docker compose up -d`. Policy, keys and lock are kept. | No |
| A host with kit older than 1.1.0 | Upgrade the kit first. Until then the password way needs `unlock`, then `lock` again afterwards. | Yes |

On the Docker host, use `docker compose exec ai-host jodd-ai-host …` in place
of `sudo jodd-ai-host …`.

## What the server panel shows

For a host with the kit, the panel's first line is **AI host** *version* **on**
*host*, for example **AI host 1.0.0 on box.local**. The version is the kit's,
not Jodd's.

Below it is one line per CLI Jodd can use, **Claude Code** and **Codex CLI**:

- a tick: the host's administrator turned it on, and Jodd can use it;
- a cross, with the reason next to it: Jodd cannot use it on this host, and
  the reason says what to do.

The host decides which CLIs Jodd may run, not Jodd. The kit turns on every CLI
it finds when it is installed; after that, whoever runs the host turns them on
and off with `sudo jodd-ai-host enable <name>` and `sudo jodd-ai-host disable <name>`.
Jodd asks the host again when you open the panel and after every **Test
connection**, so a change on the host shows up then.

### Notices

When something on the host does not fit this Jodd, the panel says so in one of
four ways:

| Notice | What it means | What to do |
|---|---|---|
| *CLI* is turned off on this host. Ask its administrator to run `sudo jodd-ai-host enable` *name*. | The host's policy does not let Jodd run that CLI: it was turned off, or it was installed after the kit and never turned on. Shown next to the CLI. | On the host, run the command it names. |
| Upgrade the AI host to use *feature*: on the host, re-run the installer, or run `docker compose pull && docker compose up -d`. | The host's kit is older than the feature needs. | Update the kit as it says. Your policy, keys and lock are kept. |
| Update Jodd to use *feature* on this host. | The host is newer than this Jodd for that feature. | Update Jodd. Everything else keeps working. |
| This AI host uses the old helper. Upgrade it: install the AI host kit on the host (…), then run `sudo jodd-ai-host adopt-keys`. AI features keep working meanwhile. | The host was set up by an older Jodd, before the kit, and still runs the helper that Jodd installed then. | Install the kit, then run `sudo jodd-ai-host adopt-keys` once. Nothing is needed in Jodd. See [Hosts set up before the kit](#hosts-set-up-before-the-kit). |

A notice that ends with *Ask its administrator to run `jodd-ai-host doctor`*
means the host is not working as the kit should. The doctor says what to fix.

## Choose Claude Code or Codex

Under the server, open **Choose a CLI…** and pick **Claude Code** or **Codex
CLI**. With **A server**, only these two are offered, and only the ones the
host turned on can be picked; one that cannot be picked shows the host's
reason. Press **Save**.

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
  sees only the container. See
  [Harden](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.md#harden)
  in the install guide.

## Test

After **Save**, press **Test connection**. It runs the saved settings through
one real extraction, which usually takes 10 to 20 seconds and counts against
your plan with the provider like any other request. The result shows **OK** or
**Failed** and the time it took. When Jodd recognises the problem, it says the
cause and what to do; otherwise it shows the CLI's own message. Afterwards
Jodd asks the host again which CLIs are turned on, so the server panel is up to
date.

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
[How it works](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/HOW-IT-WORKS.md#where-your-data-goes).

## Stop a run

Cancel an AI action the usual way: **Cancel** in Ask Jodd, **Cancel extraction**
in Extract. Jodd tells the host to stop and hangs up, and the host ends the
CLI **within a few seconds**.

If the network drops without a hang-up reaching the host, the host notices
after **about 2 minutes**. Every run also stops on its own at the host's time
limit: by default **120 seconds** for Claude Code and **180 seconds** for
Codex. The host runs **two requests at a time** by default; a third is refused
at once with *is busy*.

## Hosts set up before the kit

A host that an older Jodd set up still works: new Jodd notices it, uses the
old helper for it, and shows the *uses the old helper* notice. The better fix
is to upgrade the host: install the kit, then run `sudo jodd-ai-host adopt-keys`
once. That points Jodd's existing keys at the kit; nothing is needed in Jodd,
and it keeps working while you do it. Within about 10 minutes, or at once when
you open the server panel, Jodd uses the kit.

### Reinstall helper

Only on a host that still runs the old helper, the panel shows
**Server:** *host*, each CLI with its path or **not installed**, and
**Reinstall helper**. A host with the kit has no Reinstall: what Reinstall used
to fix, the kit does on the host.

Use **Reinstall helper** on an old host when:

- Jodd says the host **needs an update** (after you update Jodd), or shows
  **Jodd's helper on this server needs an update.**
- You installed a CLI on the host after Setup, and Jodd still shows it as
  **not installed**.

Steps:

1. On the host, turn password login on: `sudo jodd-ai-host unlock`
   (Docker: `docker compose exec ai-host jodd-ai-host unlock`).
2. In Jodd, press **Reinstall helper**, enter the **Password**, and press **Reinstall now**.
3. On the host, turn password login off again: `sudo jodd-ai-host lock`
   (Docker: `docker compose exec ai-host jodd-ai-host lock`).

**By command instead:** press **Reinstall helper**, choose **Run a command on
the host**, press **Show command**, run it on the host (kit 1.1.0 or later),
then press **Check**. There is no `unlock` or `lock`, and Jodd keeps the same key.

If the kit is installed on the host by then, Reinstall moves Jodd's key to it
instead of reinstalling the old helper. Reinstalling keeps the same key. It
does not change the host's address or login: for that, use
[Change server](#change-server). If it fails, Jodd keeps its key, so you can
fix the cause and try again.

## Change server

Use **Change server** when the host's address, port or login changed, for
example when your router gave the host a new local address. The current host
keeps working until the new one is set up. The new host needs the kit, like
any Setup.

1. On the host, turn password login on: `sudo jodd-ai-host unlock`
   (Docker: `docker compose exec ai-host jodd-ai-host unlock`).
2. In Jodd, press **Change server**. The form opens with the current
   **Server**, **Port** and **Username**; change what is different and press
   **Continue**.
3. Check the fingerprint, enter the **Password**, and press **Trust and set up**.
4. On the host, turn password login off again: `sudo jodd-ai-host lock`.

**By command instead:** skip `unlock` and `lock`. Press **Change server**, change
what is different, press **Continue**, check the fingerprint, choose **Run a
command on the host**, press **Trust and set up**, run the command it shows on
the *new* host (kit 1.1.0 or later), and press **I've run it**. If Jodd says the
host doesn't accept its key yet, the old host is untouched and keeps working.

When the new host works, Jodd removes its old key from the old host (or only
forgets it on this device, if the old host cannot be reached). This takes
effect at once: saved settings that used the old host now use the new one,
and Jodd says **Server changed to** and the new host's name.

A host whose local address keeps changing will break again. Give it a fixed
address in your router, or use its Tailscale name.

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
**AI settings**, it means the **AI provider** section of **App settings** or
of **Account settings**, whichever you set the server up in.

Where the table says `sudo jodd-ai-host …`, on the Docker host run
`docker compose exec ai-host jodd-ai-host …` instead.

### While setting up

| Message | Why | What to do |
|---|---|---|
| Enter the server name only, without “@”. Put the login name in Username. | The **Server** field contains `@`. | Put only the address in **Server** and `jodd-ai` in **Username**. |
| Port must be a number between 1 and 65535. | The **Port** field is not a port number. | Enter `22`, or `2222` for Docker. |
| Enter a username. | **Username** is empty. | Enter `jodd-ai`. |
| Enter the password. | **Password** is empty. | Enter the account's password, or choose **Run a command on the host**. |
| The host doesn't accept Jodd's key yet. Run the command on the host, then try again. | After **I've run it**: Jodd's key is not on the host yet. The command was not run, was run on another account or host, or the host's kit is older than 1.1.0 and has no `add-key`. | Run the command Jodd shows, exactly as shown, on the AI host, as an administrator. If `add-key` prints the usage or command list instead of adding the key, the kit is older than 1.1.0: update it. Then press **I've run it** again. |
| This host doesn't have the AI host kit, which "Run a command" needs. Install the kit, or set up with the password. | After **I've run it**: the host answers, but without the AI host kit, which the command needs. | Install the kit ([install](https://github.com/BBM-Co-ORG/jodd-ai-host#install)), or go back and set up with the password. |
| The SSH target is empty. | **Server** is empty. | Enter the host's address. |
| An SSH target cannot contain spaces. | **Server** has a space or another invisible character in it. | Enter only the address, without spaces. |
| An SSH target cannot start with '-'. | **Server** starts with `-`. | Enter only the address. |
| could not reach *host*: no host key returned | On desktop, after **Continue**: nothing answered SSH at that address and port. Usually a wrong address, the wrong port (`22` vs `2222` for Docker), a host that is off or asleep, or a different network. | Check the address and port; check the host is on and reachable ([Connect from Jodd](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.md#connect-from-jodd)). On the host: `sudo jodd-ai-host doctor`. If the host's address changed, use [Change server](#change-server). |
| could not run ssh-keyscan: … | On desktop, after **Continue**: Jodd could not start `ssh-keyscan`, the SSH tool it uses to read the host's identity. | Make sure this computer has the OpenSSH client installed. |
| Can't reach the server. Check the address, and that the server is running. | Jodd cannot connect: wrong address or port, the host is off or asleep, or your device is on another network. | Check the address and port. On the same Wi-Fi, use the address the installer printed; away from home, use [Tailscale](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.md#away-from-home-tailscale). On the host: `sudo jodd-ai-host doctor`. If the host's address changed, use [Change server](#change-server). |
| This host needs the AI host kit — install it from https://github.com/BBM-Co-ORG/jodd-ai-host#install, then set up again. | The server does not have the kit's `jodd-ai-mcp`. Nothing was changed on it. | Install the kit ([install](https://github.com/BBM-Co-ORG/jodd-ai-host#install)), then set up again. |
| The AI host kit on this server did not run: … | The kit is there but `jodd-ai-mcp --version` failed; the rest is its own message. Nothing was changed. | `sudo jodd-ai-host doctor`, fix what it reports, or install the kit again. |
| This host doesn't accept password sign-in. Choose Run a command on the host (needs AI host kit 1.1.0 or later), or run sudo jodd-ai-host unlock on the host, use the password, then run sudo jodd-ai-host lock. | Password login for the account is off, usually because you ran `lock`. Or the server's sshd has a global `AuthenticationMethods publickey`, which refuses a password even though the kit allows one for the account. | Choose **Run a command on the host**: it needs no password, so the host can stay locked. Or run `sudo jodd-ai-host unlock`, use the password, then `sudo jodd-ai-host lock` again. If it was not locked, run `sudo jodd-ai-host doctor`: it names the sshd setting to change. |
| The server rejected that username or password. | Wrong password, or a username other than the kit's account. On a Mac, also when the account is not on Remote Login's allow-list. Or the server's sshd has `AllowUsers` or `AllowGroups` that leave the account out (or `DenyUsers` / `DenyGroups` that name it). | Check the username (`jodd-ai`) and the password. Set a new password with `sudo jodd-ai-host password`. On a Mac, run the installer again. If the password is right, run `sudo jodd-ai-host doctor`: it names the sshd setting to change. |
| The server's identity does not match the fingerprint you confirmed. Nothing was changed. | The host's key changed after you confirmed it, or you are not talking to your host. | Run `jodd-ai-host fingerprint` on the host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`) and start again. If the fingerprint keeps changing, stop and find out why. |
| The server stopped answering. Check your connection and try again. | The connection went quiet during setup. | Check the network and try again. |
| Could not talk to the server: … | The connection broke during setup. | Try again. If it keeps failing, `sudo jodd-ai-host doctor`. |
| Could not connect: … | Another connection problem; the rest of the message says which. | Act on the rest of the message, then `sudo jodd-ai-host doctor`. |
| the server has neither sha256sum nor shasum | Reinstall on an old host: the host lacks a tool the old helper needs. | Install `coreutils` on the host, or better, [install the kit](#hosts-set-up-before-the-kit). |
| Installing on the server failed: … | Jodd could not add its key; the rest says why. | `sudo jodd-ai-host doctor`, fix what it reports, try again. |
| Jodd may have left an entry ending in jodd-ai-… in ~/.ssh/authorized_keys on …; its key no longer exists on this computer, so you can remove that line. | Setup failed after it had started changing the host. | Remove that key on the host: `sudo jodd-ai-host revoke <the 8 characters after jodd-ai->`. Then fix the cause and set up again. |
| The AI host did not answer the status check within 30s. | The host accepted the connection but did not answer in time. | Check the network and the host's load, then open the panel again. On the host: `sudo jodd-ai-host doctor`. |
| The AI host did not answer the status check (exit …). … | An old host's helper did not answer as expected, for example because `~/.jodd` was deleted. | [Install the kit](#hosts-set-up-before-the-kit) and run `sudo jodd-ai-host adopt-keys`. Or unlock, press **Reinstall helper**, lock. If that fails, **Remove** and set up again. |
| Jodd's key is missing: … | This device lost its key for the host. | **Remove**, then set up again. |
| this AI host has no usable key directory | This device's saved settings for the host are damaged. | **Remove**, then set up again. |

### In the server panel and the provider settings

The notices the panel can show are under [Notices](#notices).

| Message | Why | What to do |
|---|---|---|
| Turned off on this host. (next to Claude Code or Codex CLI) | The host does not let Jodd run that CLI, and gave no reason of its own. | On the host: `sudo jodd-ai-host enable claude` or `sudo jodd-ai-host enable codex`. |
| Jodd's helper on this server needs an update. | An old host: Jodd was updated and needs a newer helper there. | [Install the kit](#hosts-set-up-before-the-kit), or [Reinstall helper](#reinstall-helper). |
| **not installed** (next to Claude Code or Codex CLI) | An old host: Jodd did not find that CLI there when it last set up or updated. | `sudo jodd-ai-host update-clis`, then [Reinstall helper](#reinstall-helper). |
| Not installed on the server — install it there, or AI actions will fail. | The CLI you picked is not on the host. | Same as above, or pick the other CLI. |
| Set up your server above before saving. | **A server** is chosen but no server is set up. | [Set up a server](#set-up-a-server), or choose **This computer**. |
| save: set up your server first, or choose This computer. | You pressed **Save** with **A server** and no server. | Same as above. |

### When AI runs, or in Test

| Message | Why | What to do |
|---|---|---|
| Couldn't reach AI host *host*: … | Your device cannot reach the host: it is off or asleep, you are away from its network, Tailscale is off, or the address changed. | Wake the host or check it is on. Away from home, turn Tailscale on. On the host: `sudo jodd-ai-host doctor`. If the host's address changed, use [Change server](#change-server). |
| The AI host *host*'s identity changed. Set it up again in AI settings. | The host's key changed. Usually the host was reinstalled, or the Docker `state` volume was deleted. It can also mean someone is in between. | Check `jodd-ai-host fingerprint` on the host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`). If you know why it changed, **Remove** and set up again, confirming the new fingerprint. If you don't, stop. |
| The AI host *host* no longer accepts Jodd's key. Set it up again in AI settings. | Jodd's key line is gone from the host: removed with `revoke`, the account was recreated, or the Docker `home` volume was deleted. | **Remove**, then set up again. |
| The AI host *host* is not set up. Set it up in AI settings. | This device has no saved connection for the host. | [Set up a server](#set-up-a-server). |
| On *host*, *cli* is not enabled on this AI host. … | The host's administrator turned that CLI off, or never turned it on. The message is the host's own, and names the command. | On the host: `sudo jodd-ai-host enable claude` or `sudo jodd-ai-host enable codex`, then test again. |
| *cli* isn't installed on *host*. | The CLI is missing on the host, or is no longer where it was when it was turned on. | `sudo jodd-ai-host update-clis`, then `sudo jodd-ai-host enable claude` or `codex`. On an old host: `update-clis`, then [Reinstall helper](#reinstall-helper). |
| The AI host *host* is busy — try again in a moment. | The host is already running as many requests as it allows (two by default), for example from your other device. | Wait for the other run to finish, then try again. |
| This request is larger than the AI host *host* accepts. … | The request is over the host's size limit; the CLI never started. | Send less text: a smaller selection or fewer notes. The limit is in the host's policy. |
| The AI host *host* needs an update. Open AI settings and choose Reinstall helper. | An old host: Jodd was updated and needs a newer helper there. | [Install the kit](#hosts-set-up-before-the-kit), or unlock, **Reinstall helper**, lock. |
| Claude Code is not signed in. | Claude Code on the host has no sign-in, or it expired. Jodd adds *Do this on host, over SSH.* | `sudo jodd-ai-host signin claude`, then test again. |
| *cli* timed out after *N*s on *host* | The run hit the host's time limit (by default 120 s for Claude Code, 180 s for Codex). | Try again with less text. |
| *cli* timed out after *N*s on *host* — no output at all; the CLI may have been waiting for interactive input | The CLI waited for something, usually a sign-in or a first-run question. | `sudo jodd-ai-host signin claude` or `sudo jodd-ai-host signin codex`, finish its prompts, then test again. |
| AI host *host* did not answer within *N*s | The connection stalled beyond the run's limit. | Check the network and the host's load, then try again. |
| *cli* on *host* ended without an exit status | The connection dropped, or the CLI was killed on the host (for example, out of memory). | Try again. If it repeats, check the host has 4 GB of memory free. |
| *cli* on *host* exit *code*: … | The CLI itself failed; the rest is its own message. | Act on the CLI's message. A sign-in problem: `sudo jodd-ai-host signin claude` or `sudo jodd-ai-host signin codex`. A plan or usage limit: check your account with the provider. |
| *cli* on *host* failed: … | The host could not run the CLI at all; the rest is the host's own message. | Act on it; `sudo jodd-ai-host doctor`. |
| CLI produced no output | The CLI finished without an answer. | Try again. If it repeats, sign in again on the host. |
| AI host *host* refused the request | The host's SSH server refused to run Jodd's request. | `sudo jodd-ai-host doctor`. If Jodd's key still runs an old helper, `sudo jodd-ai-host adopt-keys`. |
| AI host *host*: … | Another SSH or connection problem; the rest says which. | Act on the rest of the message; `sudo jodd-ai-host doctor`. |
| AI data access is not allowed for this account. Allow it to continue. | The account has not allowed AI ([AI permission](#ai-permission-per-account-and-per-device)). | Choose **Allow AI for this account**, or tick **Allow this account’s data in Jodd-managed AI** in Account settings. |
