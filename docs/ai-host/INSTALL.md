[ภาษาไทย](INSTALL.th.md)

# Install a Jodd AI host

An AI host is a machine you own where Jodd runs Claude Code or Codex for you,
over SSH. This guide sets one up, connects Jodd to it, and keeps it safe. How
it works inside is in [How it works](HOW-IT-WORKS.md); the Jodd side is in the
[user guide](USER-GUIDE.md).

**Your own CLI, your own account, on your own server, under the provider's
terms.** You install the CLI and sign in to it yourself; Jodd never sees that
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

## What has been tested

| Path | Tested | How |
|---|---|---|
| Docker | 2026-10-15 | The image, started with `docker run`: first start without network, both CLIs installed, Jodd's own setup code connecting and finding both CLIs, `lock` / `unlock`, and recreating the container (fingerprint, installed CLIs and lock kept). The `compose.yaml` file itself has not been run end to end yet. |
| Ubuntu 24.04, Debian 12, Fedora, Alpine | 2026-10-15 | The installer in clean containers: real CLIs installed, real password logins, Jodd's own setup code connecting and finding both CLIs, a server whose sshd already turned password login off for everyone (every other account kept its settings), re-runs that change nothing. Starting sshd through systemd or OpenRC was not part of these tests. |
| macOS | 2026-10-15 | One Apple Silicon Mac on macOS 27 (OpenSSH 10.3): a new account created, the Remote Login allow-list limited to `jodd-ai`, both CLIs installed, Jodd's own setup code connecting with the password and finding both CLIs, the fingerprint matching what sshd serves, `lock`, and a re-run on the existing account that moved it into a group of its own. macOS 13–26, Intel Macs, `--no-sleep` and the removal steps were not tested. |

All of these runs were on **ARM64** (aarch64) machines. Nothing has been run
on x86_64 (Intel or AMD) yet.

## Before you start

You need:

- **A machine that stays on.** Jodd can only use the host while it is awake and reachable.
- **4 GB of memory** or more, on 64-bit x86 or ARM.
- **An account with the provider**, for each CLI you want:
  - Claude Code: a Claude Pro, Max, Team or Enterprise plan, or an Anthropic Console account.
  - Codex: a ChatGPT plan.
- **Jodd 0.34.0 or newer** on your devices.
- **`curl`** on the host, to download the kit. Most systems have it; if not,
  install it with your package manager (or download the same URLs with `wget -O`).

Choose one path:

1. [Docker](#docker): any machine with Docker. The least to change on the
   machine, and the safest choice: Codex can only read what is inside the
   container (see [Harden](#harden)).
2. [Ubuntu and Debian](#ubuntu-and-debian)
3. [Fedora, RHEL and Alpine](#fedora-rhel-and-alpine)
4. [macOS](#macos)
5. [Other Linux](#other-linux): the same steps, by hand.

Then [sign in to the CLIs](#sign-in-to-the-clis), [connect from Jodd](#connect-from-jodd)
and [harden](#harden).

## Docker

You need Docker with the Compose plugin (`docker compose`).

Make a folder for the host and go into it:

```sh
mkdir jodd-ai-host
```

```sh
cd jodd-ai-host
```

Download the compose file:

```sh
curl -fsSLO https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/docker/compose.yaml
```

**Choose a password (recommended).** Create a file named `.env` next to
`compose.yaml`, with one line:

```text
JODD_AI_PASSWORD=choose-a-long-password
```

Write a `$` in the password as `$$`. Without this file the container makes up
a password and shows it once in its log. That works, but a password you set
later inside the container does not survive an upgrade, and the one in `.env`
does.

Start it:

```sh
docker compose up -d
```

Read its log:

```sh
docker compose logs
```

The log shows the host-key fingerprint, the generated password (if you did not
set one), and `jodd-ai-host ready`. Both CLIs install in the background on the
first start; after a few minutes, run `docker compose logs` again and look for
`CLIs installed`. If it says `could not install the CLIs`, check the machine's
internet connection, then run:

```sh
docker compose exec ai-host jodd-ai-host update-clis
```

**The port is 2222**, not 22. Jodd connects to this machine's address on port 2222.

**Who can reach the port.** By default the port is open on every network the
machine is on, so a phone on the same Wi-Fi can reach it. That is right for a
machine at home. On a VPS or a cloud server, add a line to `.env` so the port
is published only on a VPN address (for example your [Tailscale](#away-from-home-tailscale)
address) or only on the machine itself:

```text
JODD_AI_BIND=100.101.102.103
```

Docker publishes ports past `ufw` and `firewalld`, so a firewall rule alone
does not close them. Other settings in `.env`: `JODD_AI_PORT` changes 2222 to
another port.

**In Docker, every `jodd-ai-host` command runs through `docker compose exec`,
without `sudo`.** Wherever these guides say `sudo jodd-ai-host <command>` (or
`jodd-ai-host <command>`), run this from the folder with `compose.yaml`,
with your command in place of `status`:

```sh
docker compose exec ai-host jodd-ai-host status
```

For example, `sudo jodd-ai-host lock` becomes
`docker compose exec ai-host jodd-ai-host lock`.

If you missed the generated password, set a new one:

```sh
docker compose exec ai-host jodd-ai-host password
```

Next: [sign in to the CLIs](#sign-in-to-the-clis).

## Ubuntu and Debian

Tested with OpenSSH 9.2 to 10.3, on Debian 12, Ubuntu 24.04, Fedora and
Alpine. Older versions may work but have not been tested. If an older sshd
rejects the settings, the installer stops and restores the SSH settings it
touched.

**On a VPS or any machine reachable from the internet:** once the installer
finishes, anyone on the internet can try passwords for `jodd-ai` until you run
`sudo jodd-ai-host lock`. Set up Jodd soon after, lock straight away, and
limit the SSH port with a firewall ([Harden](#harden)).

Download the installer:

```sh
curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/install/linux.sh -o jodd-ai-host-install.sh
```

Read it before you run it. It runs as root:

```sh
less jodd-ai-host-install.sh
```

Run it:

```sh
sudo sh jodd-ai-host-install.sh
```

It asks for a password for the new account `jodd-ai` (twice). Jodd needs that
password once, for Setup. Press Enter without typing one and the installer
makes one up and prints it.

What it does, in order:

1. Installs the OpenSSH server, `curl`, `bash` and a few basic tools with your
   package manager. On Ubuntu and Debian it does not upgrade packages you
   already have.
2. Creates the account `jodd-ai` with `bash` as its login shell, and puts
   `~/.local/bin` on its login `PATH`.
3. Sets the account's password.
4. Creates missing SSH host keys. It never replaces existing ones.
5. Writes `/etc/ssh/sshd_config.d/50-jodd-ai.conf`. It allows password login
   **for `jodd-ai` only**; every other account keeps the server's settings.
   It makes sure sshd reads that file first, checks the result with `sshd -t`,
   and compares the effective settings of other accounts before and after. If
   anything is off, it puts the SSH configuration back exactly as it was and stops.
6. Installs `claude` and `codex` as `jodd-ai`, from their official installers.
7. Starts or reloads sshd. Open SSH sessions are not dropped.
8. Installs `/usr/local/bin/jodd-ai-host`, runs `jodd-ai-host doctor`, and
   prints the host address, port, account and fingerprint.

Running it again repairs what is missing and changes nothing else. It keeps
the account's password, and keeps password login off if you locked it.

Settings you can pass to the installer:

| Setting | Does |
|---|---|
| `JODD_AI_USER` | The account name (default `jodd-ai`). It must not exist yet, or must be an ordinary account that cannot become root. |
| `JODD_AI_PASSWORD` | The password, for a run without a terminal. A later run applies it again only if you change it. |
| `JODD_AI_PORT` | Only the port the summary shows (for example a port you forward). It does not change sshd's port. |
| `JODD_AI_SKIP_CLIS=1` | Skip the CLI install, for a machine without internet. Install them later with `sudo jodd-ai-host update-clis`. |

Put them between `sudo` and `sh`, for example:

```sh
sudo JODD_AI_SKIP_CLIS=1 sh jodd-ai-host-install.sh
```

Next: [sign in to the CLIs](#sign-in-to-the-clis).

## Fedora, RHEL and Alpine

Use the same installer and the same three commands as
[Ubuntu and Debian](#ubuntu-and-debian). It uses `dnf` on Fedora and RHEL, and
`apk` on Alpine.

- **Fedora** was tested. RHEL and its rebuilds use the same `dnf` steps but
  were not tested.
- **Alpine** often has no `sudo`. Run the installer and every `jodd-ai-host`
  command as root, without `sudo`. The installer adds what Claude Code needs
  on Alpine (`libgcc`, `libstdc++`, the system `ripgrep`) and tells Claude Code
  to use that `ripgrep`. Both Claude Code and Codex work on Alpine; this was
  like everything else, tested on ARM64 only.

## macOS

Tested on one Apple Silicon Mac on macOS 27 (see
[What has been tested](#what-has-been-tested)). Run the dry run first and read
what it prints; it changes nothing.

Needs macOS 13 (Ventura) or newer, and a Mac that stays on.

Download the installer:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/install/macos.sh -o jodd-ai-host-install.sh
```

See every step it would take, without changing anything (no `sudo` needed):

```sh
sh jodd-ai-host-install.sh --dry-run
```

Run it:

```sh
sudo sh jodd-ai-host-install.sh
```

What it does:

1. Creates a standard account, `jodd-ai` ("Jodd AI host"), hidden from the
   login window, and asks you for its password. The prompt comes from macOS's
   own `sysadminctl`: it shows `User password:`, asks **once** with no
   confirmation (type carefully), and shows nothing as you type. Just before
   it, `sysadminctl` prints a warning containing "No clear text password or interactive option was specified … FDE";
   that is harmless: it only means `jodd-ai` cannot unlock FileVault at
   startup, which it never needs.
2. Gives `jodd-ai` a group of its own instead of `staff`, and closes its home
   folder to other accounts (`chmod 700`). On a Mac every account is in
   `staff`, and `staff` can enter every home folder, so this keeps `jodd-ai`
   (and Codex, which runs as it) out of yours. Run on an existing `jodd-ai`
   that is still in `staff`, the installer moves it too. If macOS still counts
   it in `staff` afterwards, the installer warns you. See
   [Know what Codex can read](#harden).
3. Writes the same sshd rule as on Linux: password login for `jodd-ai` only.
4. Turns **Remote Login** on. macOS allows that from Terminal only when Terminal
   has Full Disk Access. If it does not, the installer tells you to open
   **System Settings → General → Sharing** and turn **Remote Login** on, and
   waits until you press Enter.
5. Installs `claude` and `codex` as `jodd-ai`, from their official installers.
6. Installs `/usr/local/bin/jodd-ai-host` and prints the address, port,
   account and fingerprint. It also lists other home folders on the Mac that
   other accounts can still enter, each with the `chmod 700` command that
   closes it. It only lists them; it changes nothing there.

**Who else gets SSH.** Remote Login has an allow-list ("Allow access for"). On
a new Mac it already lists the Administrators group. If Remote Login is off and
that list names anyone besides the new account, the installer asks:

- (a) only "Jodd AI host" may use SSH: remove the others from the list (recommended),
- (b) keep the list as it is,
- (c) stop.

Choose (b) only if you know who is on the list. On a new Mac, (b) means that
**every administrator account on the Mac can log in over SSH with its password
from your network**, where nobody could before.

For a run without a terminal, choose with a flag instead: `--ssh-only-ai`
for (a), `--ssh-keep-list` for (b). Without a terminal and without a flag, the
installer stops and leaves Remote Login off.

If Remote Login was **already on**, the installer does not change who else may
log in:

- If it was limited to a list, the installer adds `jodd-ai` to that list and
  removes no one. Whoever the list names (on a stock Mac, the Administrators
  group) can still log in over SSH with a password. To narrow it: **System
  Settings → General → Sharing → Remote Login → Allow access for**.
- If it was on for all users, every account on the Mac can log in over SSH with
  its password, not only `jodd-ai`. The installer leaves that as it is and warns
  you. To restrict it, choose **Only these users** there and add the people who
  need it and "Jodd AI host".

**The port is always 22** on macOS.

**Keep the Mac awake.** A sleeping Mac cannot answer Jodd. Turn on
**System Settings → Energy → Prevent automatic sleeping when the display is off**
(on a laptop: **Battery → Options**). Or run the installer with `--no-sleep`,
which keeps the Mac awake while it is on its charger:

```sh
sudo sh jodd-ai-host-install.sh --no-sleep
```

`JODD_AI_USER` and `JODD_AI_SKIP_CLIS=1` work as on Linux. `JODD_AI_PASSWORD`
is different: it is used only when the installer creates the account. It never
changes the password of an account that already exists (use
`sudo jodd-ai-host password` for that). Prefer the password prompt to
`JODD_AI_PASSWORD`: macOS's account tool receives that value on its command
line, where other users of the Mac could see it for a moment.

## Other Linux

For a system the installer does not know (it supports `apt`, `dnf` and `apk`),
do the same steps by hand. These steps reproduce what the installer does; they
have not been tested as a whole. Commands below assume `sudo`; use root
directly if you do not have it.

The host needs: an OpenSSH server (tested with 9.2 to 10.3; older versions
may work but have not been tested), `sh`, `bash`, `curl`, `base64`,
`sha256sum` or `shasum`, `mktemp`, a `head` that supports `-c`, and `setsid`
(from util-linux).

**1. Create the account** with `bash` as its login shell:

```sh
sudo useradd -m -s /bin/bash jodd-ai
```

```sh
sudo passwd jodd-ai
```

**2. Install the CLIs as the account.** Open a login shell as `jodd-ai`:

```sh
sudo -iu jodd-ai
```

In that shell, put `~/.local/bin` on the login `PATH` (if the account has a
`~/.bash_profile`, add the same line there too):

```sh
echo 'export PATH="$HOME/.local/bin:$HOME/.codex/bin:$PATH"' >> ~/.profile
```

Install Claude Code:

```sh
curl -fsSL https://claude.ai/install.sh | bash
```

Install Codex:

```sh
curl -fsSL https://chatgpt.com/codex/install.sh | sh
```

Leave the account's shell:

```sh
exit
```

**3. Allow password login for this account only.** First note what sshd
applies to root today (it prints one line; keep it):

```sh
sudo sshd -T -C user=root,host=x,addr=127.0.0.1 | grep -i '^passwordauthentication'
```

Then create
`/etc/ssh/sshd_config.d/50-jodd-ai.conf` with exactly this text:

```text
# Managed by jodd-ai-host. Change with: jodd-ai-host lock | unlock
# Only the account below is affected; every other account keeps the server's settings.
Match User jodd-ai
    PasswordAuthentication yes
    KbdInteractiveAuthentication no
    ClientAliveInterval 30
    ClientAliveCountMax 4
```

**4. Make sshd read it first.** sshd keeps the first value it reads, and a
`Match` block swallows everything after it. If the first setting in
`/etc/ssh/sshd_config` is not already `Include /etc/ssh/sshd_config.d/*.conf`,
add this as the first line of that file:

```text
Include /etc/ssh/sshd_config.d/50-jodd-ai.conf
```

**5. Check before you reload.** This must print nothing:

```sh
sudo sshd -t
```

This must print `passwordauthentication yes`:

```sh
sudo sshd -T -C user=jodd-ai,host=x,addr=127.0.0.1 | grep -i '^passwordauthentication'
```

Run the command from step 3 again (the one with `user=root`): it must print
the same line as before. If anything is wrong, undo steps 3 and 4.

**6. Reload sshd** the way your system does it, for example `sudo systemctl
reload sshd`, `sudo rc-service sshd reload` or `sudo service sshd reload`.

**7. Install the helper.**

```sh
sudo mkdir -p /usr/local/lib/jodd-ai-host
```

```sh
sudo curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/bin/jodd-ai-host -o /usr/local/bin/jodd-ai-host
```

```sh
sudo curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/lib/dropin.sh -o /usr/local/lib/jodd-ai-host/dropin.sh
```

```sh
sudo curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/lib/doctor.sh -o /usr/local/lib/jodd-ai-host/doctor.sh
```

```sh
sudo chmod 755 /usr/local/bin/jodd-ai-host
```

Create `/etc/jodd-ai-host.conf` with this text (change the port if your sshd
uses another one):

```text
JODD_AI_USER=jodd-ai
JODD_AI_PORT=22
JODD_AI_KIND=linux
JODD_AI_DROPIN=/etc/ssh/sshd_config.d/50-jodd-ai.conf
JODD_AI_HOSTKEY=/etc/ssh/ssh_host_ed25519_key.pub
JODD_AI_LIB=/usr/local/lib/jodd-ai-host
```

**8. Check everything:**

```sh
sudo jodd-ai-host doctor
```

## Sign in to the CLIs

Sign in on the host, as yourself, with your own account. Jodd never sees this.
Sign in to the CLIs you want; one is enough.

**Claude Code:**

```sh
sudo jodd-ai-host signin claude
```

Claude Code starts. Follow its prompts. When it shows a sign-in link, open the
link in a browser on any device, sign in, and paste the code it gives you back
into the terminal. When Claude Code is ready for a question, type `/exit`.

**Codex:**

```sh
sudo jodd-ai-host signin codex
```

This runs `codex login --device-auth`. It shows a link and a code. Open the
link in a browser on any device, sign in with your ChatGPT account, and enter
the code.

In Docker, use `docker compose exec ai-host jodd-ai-host signin claude` and
`docker compose exec ai-host jodd-ai-host signin codex`.

## Connect from Jodd

In Jodd you enter four things. The [user guide](USER-GUIDE.md#set-up-a-server)
walks through the screens.

| Field | What to enter |
|---|---|
| Server | The host's address, as your device can reach it (below). |
| Port | `22` for Linux and macOS (unless your sshd uses another port); `2222` for Docker. |
| Username | `jodd-ai` |
| Password | The account's password. |

### Same Wi-Fi

If your device and the host are on the same network, use the host's local
address. The installer prints it. To see it again on Linux:

```sh
hostname -I
```

On a Mac:

```sh
ipconfig getifaddr en0
```

For Docker, use the address of the machine that runs Docker.

### Away from home: Tailscale

To reach the host from anywhere, put both ends on a private network such as
[Tailscale](https://tailscale.com/download) (or another VPN you trust):

1. Install Tailscale on the host and sign in.
2. Install the Tailscale app on your phone or computer and sign in to the same
   account.
3. On the host, find its Tailscale address:

   ```sh
   tailscale ip -4
   ```

4. In Jodd, use that address (or the host's Tailscale name) as **Server**.

For Docker on a VPS, also set `JODD_AI_BIND` in `.env` to that address
([Docker](#docker)).

**Do not forward the SSH port on your router.** Password login is on during
Setup, Change server and Reinstall helper, and SSH ports on the internet are scanned all day.

## Check the fingerprint

The first time Jodd connects, it shows the host's identity, for example
`SHA256:dswu8P+mdeYHwkh6suxXWrbP9Mj1h0vpaVzXYZssz60 (ED25519)`. On the host, run:

```sh
jodd-ai-host fingerprint
```

In Docker (the log of the first start shows it too):

```sh
docker compose exec ai-host jodd-ai-host fingerprint
```

It prints the same two parts, the other way round:
`ED25519 SHA256:dswu8P+mdeYHwkh6suxXWrbP9Mj1h0vpaVzXYZssz60`. The `SHA256:` part
must match exactly. If it does not, do not continue: you may not be talking to
your host.

The fingerprint stays the same when you re-run the installer, restart the
host, or upgrade the Docker container. It changes only when the host key is
replaced (a reinstalled system, or a deleted Docker `state` volume). Jodd then
refuses the host until you set it up again.

## Harden

**Turn password login off after Setup.** Jodd uses its key from then on and
does not need the password:

```sh
sudo jodd-ai-host lock
```

Before you press **Reinstall helper** in Jodd, turn it on again, and off after:

```sh
sudo jodd-ai-host unlock
```

This affects the `jodd-ai` account only. In Docker:
`docker compose exec ai-host jodd-ai-host lock` and
`docker compose exec ai-host jodd-ai-host unlock`.

**Firewall:** allow only the SSH port, and only from your own network or VPN
if you can. In Docker, use `JODD_AI_BIND` instead ([Docker](#docker)).

**Keep the account plain.** Do not add `jodd-ai` to `sudo`, `wheel` or
`admin`, and do not keep other files in its home.

**Know what Codex can read.** Codex can read every file the `jodd-ai` account
can read. On a normal Linux or Mac that is more than its own home: everything
readable by all users, such as most of `/etc` and `/Users/Shared` on a Mac,
and other users' home folders on systems that leave them readable (many Linux
systems do, and so does a Mac: see below). What it reads can end up in an answer that comes back to Jodd.
For that reason, prefer **a machine or virtual machine used only for this**, or
the **[Docker](#docker) path**: in Docker, Codex only sees what is inside the
container. That is a real security reason to choose Docker on a machine you
also use for other things.

On a Mac, every account is in the `staff` group by default, and `staff` can
enter every home folder: inside it only Desktop, Documents, Downloads,
Movies, Music, Pictures and Library are closed. The macOS installer gives `jodd-ai` a group of its own
instead, so it is not in `staff`, and closes its home folder. Its summary
lists the home folders that are still open, with the command that closes each
one. On a Mac you also use, close your own home folder and keep secrets such
as API keys and `.env` files out of places other accounts can read:

```sh
chmod 700 /Users/<you>
```

**Use a long password**, and a different one from your other accounts.

## Update

**The CLIs.** Re-run both official installers:

```sh
sudo jodd-ai-host update-clis
```

In Docker:

```sh
docker compose exec ai-host jodd-ai-host update-clis
```

If a CLI was not installed when you set up Jodd, press **Reinstall helper** in Jodd
afterwards (with password login unlocked), so Jodd learns where it is.

**The kit on Linux and macOS.** Download the installer again and run it. It
keeps the account, its password, your sign-ins, Jodd's keys and the lock.

**Docker.** From the folder with `compose.yaml`:

```sh
docker compose pull
```

```sh
docker compose up -d
```

The fingerprint, the CLIs, your sign-ins, Jodd's keys and the lock are kept: they live in the two volumes. The password
from `.env` is applied again. Without one, the new container makes up a new
password and shows it in `docker compose logs`; you only need it for the next
Setup, Change server or Reinstall helper in Jodd.

**Jodd's part on the host.** When Jodd says the host needs an update, unlock
password login, press **Reinstall helper** in Jodd, then lock again.

## Remove everything

First press **Remove** in Jodd on every device that uses the host. That deletes
Jodd's key on the host and on the device.

**Docker.** From the folder with `compose.yaml`. This also deletes the
volumes: the CLIs, your sign-ins and the host key.

```sh
docker compose down -v
```

**Linux.** Delete the account and its home (on Alpine:
`deluser --remove-home jodd-ai`):

```sh
sudo userdel -r jodd-ai
```

If the installer added a line `Include /etc/ssh/sshd_config.d/50-jodd-ai.conf`
(and the comment above it) at the top of `/etc/ssh/sshd_config`, delete those
two lines. Then remove the kit's files:

```sh
sudo rm -f /etc/ssh/sshd_config.d/50-jodd-ai.conf /etc/jodd-ai-host.conf /usr/local/bin/jodd-ai-host
```

```sh
sudo rm -rf /usr/local/lib/jodd-ai-host /var/lib/jodd-ai-host
```

Check the SSH configuration (it must print nothing), then reload sshd as in
[step 6 of Other Linux](#other-linux):

```sh
sudo sshd -t
```

The packages the installer added (OpenSSH server, `curl` and so on) stay.

**macOS** (these steps have not been tested). Delete the account, then the group the installer
made for it:

```sh
sudo sysadminctl -deleteUser jodd-ai
sudo dseditgroup -o delete jodd-ai
```

Remove the same kit files as on Linux, and turn **Remote Login** off in
**System Settings → General → Sharing** if you do not use it for anything else.

## Troubleshooting

Start with the doctor. It checks everything Jodd needs and prints `ok`, `note`
or `FIX` lines; every `FIX` line says what to do.

```sh
sudo jodd-ai-host doctor
```

`status` shows what Jodd will find: the account, port, fingerprint, whether
password login is on, each CLI and its version, and how many Jodd keys there are.

```sh
sudo jodd-ai-host status
```

In Docker, run these and every command below as
`docker compose exec ai-host jodd-ai-host <command>`, without `sudo`.

| Symptom | What to run |
|---|---|
| A CLI shows **not installed** in Jodd, or Jodd says it isn't installed | `sudo jodd-ai-host update-clis`, then `unlock`, **Reinstall helper** in Jodd, `lock` |
| The CLI install failed (no internet during install) | `sudo jodd-ai-host update-clis` |
| Jodd's **Test connection** says Claude Code is not signed in | `sudo jodd-ai-host signin claude` |
| Codex answers with a sign-in or authorization error | `sudo jodd-ai-host signin codex` |
| Jodd says the server does not accept password sign-in | Password login is locked: `sudo jodd-ai-host unlock` (lock again after Setup, Change server or Reinstall helper). If it was not locked, `/etc/ssh/sshd_config` may have a global `AuthenticationMethods publickey`: `sudo jodd-ai-host doctor` names the setting to change |
| Jodd says the server rejected the username or password | Check the username is `jodd-ai`; set a new password with `sudo jodd-ai-host password`. If the password is right, sshd's `AllowUsers` or `AllowGroups` may leave `jodd-ai` out: `sudo jodd-ai-host doctor` names the setting to change |
| The doctor or the installer names `AuthenticationMethods`, `AllowUsers`, `AllowGroups`, `DenyUsers` or `DenyGroups` | Change that setting in `/etc/ssh/sshd_config` yourself, as the line says. The kit never changes these settings; they apply to every account. Then `sudo jodd-ai-host doctor` again |
| You lost the Docker password | `docker compose exec ai-host jodd-ai-host password` |
| The fingerprint in Jodd does not match | `jodd-ai-host fingerprint` on the host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`); stop if they differ |
| Jodd can't reach the host | Is the host on and awake? Same Wi-Fi or Tailscale? Right port (2222 for Docker)? Then `sudo jodd-ai-host doctor` |
| A device you no longer use still has a key | `sudo jodd-ai-host keys`, then `sudo jodd-ai-host revoke <id>` |
| The doctor says the CLI is installed but the login shell does not find it | Re-run the installer, or add the line the doctor prints to the account's `~/.profile` |
| The doctor says sshd does not allow password login for `jodd-ai` | Re-run the installer, or check that `/etc/ssh/sshd_config` reads the drop-in first ([step 4](#other-linux)) |

Every message Jodd can show, and what to do about it, is in the
[user guide](USER-GUIDE.md#messages-and-what-to-do).
