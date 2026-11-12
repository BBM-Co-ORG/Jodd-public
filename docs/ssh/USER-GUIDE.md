[ภาษาไทย](USER-GUIDE.th.md)

# Keeping your notes on your own server (SSH)

This guide covers SSH accounts: what the server needs, how to add one in Jodd,
and how to look after it later. Notes on an SSH server are plain Markdown files
in a folder you choose. Every Jodd you connect, and any program you run on the
server, works on the same files.

**They do not sync with Apple Notes.** An SSH account is Jodd's own store.
To use Apple Notes, add a Gmail, Outlook or iCloud account instead.

The internals (file format, conflicts, scripts) are in
[BACKEND-SSH.md](../BACKEND-SSH.md). The AI host, which also uses SSH, has
[its own guide](../ai-host/USER-GUIDE.md).

## What the server needs

- A Linux, BSD or macOS server you can reach over SSH, with a user account
  on it. A native Windows SSH server does not work: it has no `sh`.
- The usual command-line tools, which almost every server already has:
  `sh`, `find`, `awk`, `sed`, `base64`, `mktemp`, `ln`, `mkdir`, `mv`, `rm`,
  `stat`, and `sha256sum` or `shasum`.

**Use a dedicated account if you can**, for example one called `jodd`. Jodd's
key can run a shell as the account it signs in to; the notes need that. With
a dedicated account, a lost phone can reach your notes and nothing else on the
server.

## Two kinds of SSH account

| | **Add SSH Server** | **Add SSH Server (advanced)** |
|---|---|---|
| For | anyone with a server address and a username | people who already use `ssh` in a terminal |
| Key | Jodd makes its own key for this device | your own keys, from `ssh-agent` and `~/.ssh/config` |
| Desktop | yes | yes |
| Android | yes | no |

Both are in **Settings › Accounts**. On a fresh install, the first screen also
has **Start with an SSH server**, which opens **Add SSH Server**. The rest of
this guide is about **Add SSH Server**.

## Add SSH Server

**Screen 1, Connect to your server.** Fill in:

- **Server address**: for example `box.example.com`. Add `:2222` if the
  server does not use port 22.
- **Username**: the account on the server.
- **How you sign in**: pick one of the three ways below.
- **Notes folder on the server**: `~/Jodd-Notes` unless you want another
  folder. Jodd creates it if it is missing.
- **Name in the sidebar (optional)**.

Press **Continue**.

**Screen 2, Is this your server?** Jodd shows the server's identity (its
fingerprint). If your hosting provider shows a fingerprint in its panel or
email, check that it matches. Then press **Trust and connect**.

Whichever way you pick, Jodd ends up with **its own key for this device**.
It stores no password, and no key of yours.

### Password

Type the account's password. Jodd uses it once to add its key to the
server, then never again. Works on desktop and Android.

If the server does not accept passwords (*This server only accepts keys*),
go back and choose **Run a command on the server**, or **Key file** on desktop.

### Run a command on the server

No password is typed into Jodd. Works on desktop and Android, including on a
server that accepts only keys.

1. After **Trust and connect**, Jodd shows **Add Jodd's key on your server**
   with a one-line command. Press **Copy**.
2. Sign in to the server the way you normally do, **as the same user you
   entered on screen 1**, not as root (unless that user is root). Paste the
   command and run it. Use bash, zsh or sh; if your shell is fish or csh,
   type `sh` first.
3. The command says `Jodd's key was added.` Go back to Jodd and press
   **I've run it**.

Jodd checks that its key works, then creates the notes folder and the
account. If it says *The server doesn't accept Jodd's key yet*, the command
didn't run, or ran as another user. Run it again as the right user, then
press **I've run it** again. Jodd keeps the screen and the command meanwhile.

The command only appends one line to `~/.ssh/authorized_keys`. It checks it
is running as the right user first, and refuses otherwise. The line limits
the key: no port forwarding, no agent or X11 forwarding, no terminal.

**Back** from this screen removes the key Jodd made, and removes it from the
server too if Jodd had already checked it.

### Key file (desktop)

Choose the private key your hosting provider gave you, and its
**Passphrase (only if the key needs one)**. Jodd uses that key **once** to
add its own key, and keeps no copy of yours. RSA keys work with current
servers. A PuTTY `.ppk` key must first be exported to OpenSSH format with
PuTTYgen.

On Android, key files aren't supported yet: use **Run a command on the
server**, or **Password**.

## Looking after it

### Use it on another device

Each device has its own key. On the new device, add the same server and the
same **Notes folder on the server** with **Add SSH Server**. Any of the three
ways works. If the server no longer accepts passwords, use **Run a command on
the server**. Both devices then work on the same notes.

### A second notes folder on the same server

Add the server again with another **Notes folder on the server**. Each folder
is its own account with its own key, so removing one never cuts off the other.
Jodd refuses a folder that is already an account on this device.

### Turning password login off on the server

Go ahead: Jodd already uses its key and does not need the password again.
From then on, set up new devices with **Run a command on the server** (or
**Key file** on desktop).

### Changing the account's password on the server

Nothing to do in Jodd. Its key keeps working.

### Remove an account

Press **Remove** next to the account in the sidebar's account list, or
**Remove account** in its account settings. **Also remove Jodd's key from the server** is
ticked by default. Jodd removes the account at once, then removes its line
from the server in the background. Your notes stay on the server; only this
device's access goes.

If the server can't be reached at that moment, the line stays. It is
harmless, but you can delete it yourself (next section).

### A lost or retired device

Remove its key on the server: open `~/.ssh/authorized_keys` of the notes
account and delete the line for that device. Jodd's lines end with
`jodd-` and eight letters or digits, one line per device and folder. If you
can't tell which line is the lost device, delete every `jodd-` line, then set
up your remaining devices again (remove the account in Jodd and add it back;
**Run a command on the server** takes a minute).

### When Jodd can't sign in

If Jodd says the server refused its key, the line was probably removed from
the server, or the server's sign-in rules changed. Remove the account and add
it again. Notes you already synced are still on the server.

### When the server was rebuilt or moved

A rebuilt server has a new identity, and Jodd refuses to connect to a server
whose identity changed (*This server's identity is different from when you
trusted it*). **If you did not rebuild or move the server, stop here:** a
changed identity can also mean someone is intercepting the connection. Ask
your hosting provider first. There is no "trust the new identity" screen yet:
remove the account and add it again, and check the new fingerprint on
screen 2. If you edited notes on this device since its last sync, copy that
text somewhere first: Jodd can't send it to a server it refuses to connect to.

### The notes folder went missing

Jodd says *the vault directory is missing on the server*. Restore or recreate
the folder on the server (and its files, from a backup), and Jodd carries on.
