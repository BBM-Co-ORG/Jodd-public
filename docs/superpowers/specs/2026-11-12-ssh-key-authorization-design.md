# SSH key authorization without a password — design

Date: 2026-11-12. Status: draft, decisions in §10, awaiting approval of the
whole document.
Covers: Managed SSH notes accounts and AI hosts, on desktop and Android.
Supersedes, if approved: roadmap #8b's framing ("key-file import for SSH on
Android") — import becomes the secondary path, see §6.

## 1. Why

Jodd already works with keys after setup: every Managed SSH account and every
AI host gets its own Jodd-generated ed25519 key, one per device per account,
and uses nothing else afterwards. What still needs a password is **getting
that key onto the server** — and that has bitten in two ways (the user,
2026-11-12):

1. **AI host after `lock`.** The kit tells you to `sudo jodd-ai-host lock`
   after setup. From then on, Reinstall and Change server need the password
   again, so every one is `unlock` → type the password → `lock`, with the
   risk of forgetting the last step.
2. **Key-only servers.** A VPS with `PasswordAuthentication no` cannot be set
   up at all on Android (password only), and on desktop only by giving Jodd a
   copy of a key the server already trusts.

Goal: a way to authorize Jodd's per-device key that needs **no password and
no private-key transfer**, and works the same on desktop and Android.

## 2. The key model (unchanged, stated so nothing violates it)

| Key | Owner | Lives | Can do |
|---|---|---|---|
| Admin key | the user | the user's laptop (`~/.ssh`, ssh-agent) | full shell |
| Device key | Jodd, one per device per account | `<data_base>/jodd/ssh/<uuid>/id_ed25519` (notes) / the AI host key dir | only what its `authorized_keys` line allows |
| Host key | the server | Jodd's own `known_hosts` per account | proves the server's identity; confirmed once by fingerprint |

Rules every path in this design keeps:

- **A private key never crosses devices.** It is created where it is used;
  only the public key leaves.
- **One key per device per account,** so revoking one device leaves the
  others working (already true; `retire_managed_key`, `forget_key`).
- **Least privilege on the device key.** AI host lines are already
  `restrict,command="…jodd-ai-mcp --key <id>"`. Notes lines are not
  restricted today — see §4.3.
- **The bootstrap credential is not the working credential.** Whatever opens
  the door the first time (password, admin key, a command the user runs) is
  used once and never stored.

## 3. Goals, non-goals, anti-goals

**Goals**

- G1. A new setup method, **"Run a command on the server"**, for Managed SSH
  and AI host, on desktop and Android: Jodd generates the key, shows one
  command containing the public key, the user runs it on the server, Jodd
  verifies.
- G2. AI host Reinstall and Change server work without `unlock`.
- G3. `sudo jodd-ai-host add-key` in the kit, which writes the restricted
  line itself.
- G4. New Managed SSH lines carry `restrict`.

**Non-goals (this design)**

- A "Devices" screen listing and revoking keys across devices — a follow-up
  (§9).
- Wrapping Android's stored device key with Android Keystore — a follow-up
  (§9).
- QR codes. Copy and the Android share sheet carry text well enough.
- Advanced SSH accounts on Android. They mean "use my own keys", and the
  user's admin key should not live on a phone.
- Removing the password method. It stays for people without a terminal.

**Anti-goals (failure even if it works)**

- Jodd ever holding the user's admin key on Android, or keeping a copy of
  it anywhere after setup on the new paths.
- A command whose text Jodd composes such that running it as root, or as the
  wrong user, silently gives a device key more than the intended line.
- The AI host line being composed by Jodd on the new path. The kit composes
  it; Jodd only supplies the public key.
- A setup screen that appears to succeed before Jodd has logged in with the
  key.

## 4. Managed SSH notes account

### 4.1 Flow — "Run a command on the server"

Same dialog (`SshEasySetupDialog.svelte`), third method beside Password and
Key file (Android: second, beside Password).

1. User enters host, port, username, notes folder (as today).
2. Jodd fetches the fingerprint; user confirms it (as today —
   `ssh_fingerprint`, desktop `ssh-keyscan`, Android russh).
3. Jodd generates the device key into a fresh `<data_base>/jodd/ssh/<uuid>/`
   (desktop `setup::prepare_key(Generate)`, Android
   `russh_setup::generate_key`) and writes `known_hosts`, exactly as the
   Password path does before it installs.
4. Jodd shows the command (§4.2) with **Copy** (*my call*: Copy only in
   v1. Android has no outbound share in Jodd today — `MainActivity.kt`
   handles only incoming `ACTION_SEND`, and WebView has no `navigator.share`
   — so Share means a new native bridge with gotcha #31's R8 risk. That is a
   follow-up, §9), plus: *Run this on `<host>` as `<user>` — not as root.* and a **I've run
   it** button.
5. **I've run it** → `verify_key_login` with the device key, then
   `check_vault` and account creation exactly as the Password path continues
   today (`setup_managed_core` after the install step).
6. If verify fails, the screen stays put with the command still shown and
   the message: *The server doesn't accept Jodd's key yet. Check that you ran
   the command on `<host>` as `<user>`, and that it printed "Jodd's key was
   added."* The user can retry. This is a
   **third** verify-failure wording: `verify_failure_message` today has the
   Password-path one (`key_just_installed = true`, "Jodd installed its key,
   but…") and the Key-file one; neither fits. Verify is already one-shot on
   both platforms (`setup::verify_key_login`, `russh_setup::verify_key_login`
   → `run_script_once`), so `AUTH_BACKOFF` does not apply — checked.
7. **A failure after verify** (e.g. `check_vault`, `duplicate_vault`) must
   **not** delete the key directory on this path, unlike `setup_managed_core`'s
   `fail` closure today: the user's line is on the server, and deleting the
   key would orphan it and force a new command. The screen keeps the key and
   lets the user fix the folder and retry the finish step.
8. **Cancel** or closing the dialog: if verify ever succeeded, revoke the
   line with the key first (as `abandon_managed_setup` does), then delete the
   key directory; otherwise just delete it. A crash or process death leaves
   the directory to `sweep_orphan_key_dirs` (already sweeps dirs older than
   an hour with no account) — and the line, if added, stays on the server,
   harmless, as after a killed Password setup today (BACKEND-SSH.md §6).

Implementation shape: a third `SetupCredential` variant, e.g.
`SetupCredential::Command`, which takes the "generate" branch and skips the
install step. Because steps 3–4 and 5 are separated by user action, the
command needs to be split: one call that prepares the key and returns the
command text (and the uuid), one that verifies and creates the account from
that uuid. *My call:* two Tauri commands, `ssh_setup_prepare_command` and
`ssh_setup_finish_command`; the builder may choose a different split as long
as the key directory is the only state carried between them.

### 4.2 The command text

```sh
( u='me'; [ "$(id -un)" = "$u" ] || { echo "Run this as $u, not as $(id -un)." >&2; exit 1; }; umask 077; mkdir -p ~/.ssh && touch ~/.ssh/authorized_keys && printf '\n%s\n' 'no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-pty ssh-ed25519 AAAA… jodd-1a2b3c4d' >> ~/.ssh/authorized_keys && echo "Jodd's key was added." )
```

- One line, POSIX `sh`, so it pastes into any login shell (bash, zsh, sh).
- **The whole command is a subshell `( … )`.** Pasted into an interactive
  shell, a bare `exit 1` would close the user's SSH session; inside the
  subshell it only ends the command. The `umask` also stays inside it.
- **`printf '\n%s\n'`** starts the line on a fresh line. An
  `authorized_keys` whose last line has no newline would otherwise glue
  Jodd's line onto it and break both. The extra blank line is ignored by
  sshd. `install_authorized_key` has the same flaw today; the plan fixes it
  there too.
- **The user guard comes first and is required.** A notes line has no
  `command=`, so a line that landed in root's `authorized_keys` (command run
  under `sudo -i` by mistake) would give the device key a full root shell —
  sshd's default `PermitRootLogin prohibit-password` accepts keys — and
  removing the account revokes only from `<user>`'s file. That is §3's
  anti-goal; the guard is what prevents it. `<user>` goes through `sh_quote`.
- The key part is `pubkey_line_for`'s line with the options of §4.3
  prepended **at command-build time**, single-quoted with `sh_quote`. The
  public key is base64 plus a fixed comment, so it cannot contain `'`;
  assert that anyway.
- `umask 077` makes a fresh `~/.ssh` 700 and a fresh `authorized_keys` 600.
  An existing file keeps its mode — not Jodd's to change from a pasted line.
- **`touch` before `>>`**: zsh with `setopt noclobber` refuses `>>` to a file
  that does not exist yet. The `touch` creates it first (600, from the
  `umask`); on an existing file it only updates the time.
- Not idempotent (running twice adds two identical lines). Harmless to
  sshd, and `revoke_authorized_key` matches the body with `grep -F` anywhere
  in the line, so it removes every copy. Do not grow the command further.

### 4.3 Options on notes lines

New lines (Password path and Command path) are written as
`no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-pty <type> <body> jodd-<id>`.
Jodd's notes transport needs none of these (desktop spawns `ssh -T … sh`;
russh opens `exec(true, "sh")` with no PTY request — both checked).

*My call:* the explicit list, not the shorter `restrict`. `restrict` arrived
in OpenSSH 7.2 and a server that does not know an option refuses the whole
line, which would break Password setups that work today — with the
misleading "Jodd installed its key, but…" message. The four named options
are far older. *(Builder: if Dropbear is easy to try, confirm it accepts the
list; if it rejects any, drop that one rather than the whole idea.)*

Where the options are added matters:

- **Only when building the install script and the command.** Not in
  `pubkey_line_for`, `pubkey_line` or `id_ed25519.pub`.
- `scripts::key_body` takes the first two whitespace fields
  (`scripts.rs:384`), so a line with options would give it `<options> <type>`.
  Revoke would then refuse that as implausible, and the
  already-installed check in `install_authorized_key` would grep for the
  wrong text.
- Server-side matching uses `grep -F` on the body, so lines with options are
  still found by revoke.

Existing lines are left alone — no migration touches a server.

This does not stop a stolen notes key from running a shell as `<user>` —
the vault needs `sh`. The user guide should recommend a dedicated server
account for the vault (e.g. `jodd`) so a lost device costs the notes, not
the user's main account.

### 4.4 Platforms

| | Desktop | Android |
|---|---|---|
| Password | as today | as today |
| Run a command | **new** | **new** |
| Key file | **changed**: opens the door, no copy kept (§6.2) | later (§6.3) |

## 5. AI host

### 5.1 Kit: `jodd-ai-host add-key`

New subcommand in `BBM-Co-ORG/jodd-ai-host`'s `bin/jodd-ai-host`:

```sh
sudo jodd-ai-host add-key 'ssh-ed25519 AAAA… jodd-ai-1a2b3c4d'
# Docker:
docker compose exec ai-host jodd-ai-host add-key 'ssh-ed25519 AAAA… jodd-ai-1a2b3c4d'
```

- `need_root; need_mcp; need_home`, like `adopt-keys`.
- Accepts exactly `<type> <base64> jodd-ai-<8 hex>`. Refuses anything with
  options, a different comment shape, or a key type outside an allow-list
  (ed25519 at least; whatever `jodd-ai-mcp` already accepts). The id comes
  from the comment — the same 8-hex handle `keys` and `revoke` use.
- Writes `restrict,command="<MCP> --key <id>" <type> <body> jodd-ai-<id>` —
  byte-identical to what Jodd's `install::mcp_line_script` writes today, so
  `keys`, `revoke`, `adopt-keys` and the MCP server treat both the same.
- Runs the edit **as the account**, using the same pattern and the same
  symlink checks as `revoke` (root never writes in a directory the account
  controls).
- Idempotent: the same id, same body, already on the kit's line →
  `Key jodd-ai-<id> is already there.`, exit 0. The same id and body on a
  **different** line (typically a legacy `$HOME/.jodd/ai-run-v2` forced
  command) → rewrite that one line to the kit's line, i.e. `adopt-keys` for
  one key, and say so (*my call*: this is exactly what a Reinstall on a
  legacy host is for, §5.3). The same id with a different body → refuse,
  name `revoke <id>`.
- Kit 1.1.0 also updates the messages that assume a password Setup: `lock`,
  `unlock`, `status` ("unlock before Setup / Update"), the installers'
  `AuthenticationMethods publickey` warning (`install/linux.sh`,
  `install/macos.sh`) and `lib/doctor.sh` — with `add-key`, a key-only sshd
  no longer blocks Setup. Publishing the release needs the user's go-ahead,
  as every kit release does.
- Works whether password login is locked or not. `lock` stays the
  recommendation; `add-key` makes it permanent.
- Lands in a kit minor release (1.1.0). **No contract pin bump is needed:**
  Jodd never calls `add-key` — the user runs it — and the MCP tools Jodd
  uses are unchanged, so the Jodd side does not wait on the kit release.
  Jodd's command screen says the command needs kit 1.1.0 or later.

### 5.2 Jodd: AI host setup with a command

`AiHostPanel.svelte` gains a method choice on the setup step: **Password**
(today) / **Run a command on the host** (new; default when the user picks it
once — *my call*: no persisted default, Password stays first because the
kit's install guide teaches it).

1. Fingerprint, confirmed (as today).
2. Jodd generates the AI host key (as `setup_core` does before installing).
3. Jodd shows the `add-key` command, both forms (sudo and Docker), Copy,
   and **I've run it**.
4. **I've run it** → the same verification the Password setup ends with:
   `mcp_info` over the new key (`ai_host_commands.rs` ~443), which treats a
   legacy answer as failure. **Not** `status_core`, which falls back to
   `legacy_status` and would report a legacy host as success. Success saves
   the host.
5. Failure keeps the screen and the key, with: *The host doesn't accept
   Jodd's key yet. Run the command on the host, then try again.* A legacy
   answer gets §7's kit-needed message.
6. Cancel deletes the key directory, as a failed Password setup does.
7. Process death while the user is away running the command (likely on
   Android) leaves a key directory under `<data_base>/jodd/ai-hosts/`.
   Nothing sweeps that tree today — `sweep_orphan_key_dirs` covers only
   `ssh/`. Add the same sweep for AI host key directories that no saved host
   names, older than an hour.

No unlock/lock step appears in this flow's text.

### 5.3 Reinstall and Change server

- **Reinstall** offers the same choice. "Run a command" shows `add-key` for
  the **existing** key — idempotent, so it is safe on a host that still has
  the line. Most kit hosts will never need Reinstall; it stays for the
  legacy → kit move.
- **Change server** = setup against the new host (either method), then the
  existing "remove the old key from the old host" step, unchanged.
- The user guide's unlock → Reinstall → lock and unlock → Change server →
  lock recipes get a second, shorter recipe using `add-key`.

### 5.4 Why the AI host gets no "Key file" method

A key file would be used to sign in as the `jodd-ai` account. That account's
`authorized_keys` would then need an **unrestricted** line for the user's
key — exactly what the kit is built to avoid. Admin access belongs to the
admin's own account plus `sudo`, which is what `add-key` uses.

## 6. Key files

### 6.1 Desktop, today

`SetupCredential::KeyFile` copies the user's key into Jodd's key directory,
**strips its passphrase**, and uses it as the account's working key; removing
the account never revokes it (C5). That keeps an unencrypted copy of a key
the user also uses elsewhere — against §2's rules.

### 6.2 Proposal: a key file opens the door, then Jodd's own key takes over

- The chosen key file is read and decrypted (passphrase) **in memory**, used
  for one authenticated session that runs `install_authorized_key` with a
  freshly generated device key, then dropped. Nothing of it is written to
  disk.
- After that the account is identical to a Password-path account: its own
  key, revoked on removal.
- Desktop: the system `ssh` cannot take a key from memory, and it ignores
  a key file that others can read ("UNPROTECTED PRIVATE KEY FILE"; Windows
  checks the ACL the same way). A provider key in Downloads is usually 0644,
  which is why import copies it to 0600 today (`setup.rs` ~211). So `-i`
  in place does not work. The only path that keeps "nothing written to disk"
  is doing this one install over **russh**, which desktop already links for
  AI hosts — the same code as Android's §6.3.
- Existing imported-key accounts keep working unchanged
  (`key_was_generated = false` still means "never revoke").

**This changes what an existing button does** — approved by the user
2026-11-12 (§10 Q1). The dialog text for Key file changes to say Jodd uses
the key once to add its own and keeps no copy. The new in-memory credential
(`russh_session::Credential` takes a key **path** today) is shared with
§6.3 when that ships.

### 6.3 Android key files (was roadmap #8b)

Same "opens the door" semantics as §6.2, on russh:

- Picker: Android's document picker (`ACTION_OPEN_DOCUMENT`) → content URI →
  bytes read through the platform, never copied to app storage.
- Decrypt in process: `russh::keys::decode_secret_key(text, passphrase)`.
- `.ppk`: `setup::classify_ppk` (already shared) → the PuTTYgen message.
- After the install, tell the user Jodd kept no copy and that the file they
  sent to the phone is still where they put it (Downloads, Drive) — theirs
  to delete.
- Notes accounts only (§5.4).

**Deferred** — the user, 2026-11-12 (§10 Q2). "Run a command" covers
key-only servers without a key file on the phone. Recorded here so the later
piece starts from these semantics; roadmap #8b stays open, reworded to point
here.

## 7. Errors and wording

New strings, in the voice of the existing setup errors (BACKEND-SSH.md §5,
USER-GUIDE.md):

| When | Text |
|---|---|
| Notes command run as the wrong user (printed by the command itself) | Run this as `<user>`, not as `<who>`. |
| Verify fails after "I've run it" (notes) | The server doesn't accept Jodd's key yet. Check that you ran the command on `<host>` as `<user>`, and that it printed "Jodd's key was added." |
| Verify fails (AI host) | The host doesn't accept Jodd's key yet. Run the command on the host, then try again. |
| AI host verify reaches a legacy host | This host doesn't have the AI host kit, which "Run a command" needs. Install the kit, or set up with the password. |
| `add-key`: bad input | usage: jodd-ai-host add-key '<type> <key> jodd-ai-<8 hex>' (Jodd shows this line) |
| `add-key`: id taken by another key | Key jodd-ai-`<id>` already exists with a different key. Remove it first: sudo jodd-ai-host revoke `<id>` |

UI strings are English, like the rest of the app's screens (there is no UI
translation layer). Thai goes in `USER-GUIDE.th.md` only.

## 8. Testing

- Unit: the command text — quoting, `restrict` prefix, exact line equality
  with `pubkey_line_for`; running the generated command through a real `sh`
  against a temp `HOME` (the `run_local` pattern in `scripts.rs` tests)
  produces a file sshd would accept and `revoke_authorized_key` removes.
- Live (`test_server`, unix): Command path end to end — prepare, append the
  line the command would write into the server's `authorized_keys`, finish,
  account usable; and finish **without** appending → the step-6 error, key dir
  still present, retry succeeds after appending.
- Live: an options line (§4.3) serves the notes transport on both the
  `ProcessSession` (desktop) and `RusshSession` (Android) paths. This needs
  the **real sshd** in `test_sshd.rs`; the russh `test_server` reads only
  `command=` and ignores other options (`test_server.rs` ~291), so it cannot
  prove this. Point the russh client at `test_sshd` for this test.
- Unit: the command's user guard — run as another user it exits non-zero and
  leaves `authorized_keys` untouched.
- AI host: the in-test fake host gains `add-key`-equivalent setup (write the
  kit's line) and the Command path is driven through `setup`/`status_core`.
- Kit repo: `add-key` tests next to `revoke`'s — accepts, refuses options,
  refuses bad comment, idempotent, refuses id clash, symlinked `~/.ssh`
  refused. (No contract image bump in Jodd — see §5.1.)
- Device live pass on S23 FE: Command path for a notes account and an AI
  host (Copy → paste into a laptop terminal on the host → I've run it),
  including leaving Jodd while running the command and coming back.

## 9. Follow-ups (not in this design)

- **Devices screen**: list `jodd-*` / `jodd-ai-*` lines on a server (the kit
  already has `keys`), revoke any one from desktop — the lost-phone path.
- **Android Share** for the command text — a native `ACTION_SEND` bridge.
- **Android key at rest**: wrap the stored device key with an Android
  Keystore key (today it is a plain file in app-private storage).
- Optional: a recommended dedicated vault account in the server setup docs
  (§4.3), possibly a one-line `useradd` recipe.

## 10. Decisions

Answered by the user, 2026-11-12:

- **Q1. Desktop Key file → "opens the door" (§6.2):** yes, change it, done
  over russh in memory. Existing imported-key accounts are unchanged.
- **Q2. Android key-file import (§6.3, roadmap #8b):** later, as its own
  piece.

Calls made by the designer, open to override:

- Notes command guards the user (`id -un`) before writing (§4.2).
- Notes lines use the explicit four `no-*` options, not `restrict` (§4.3).
- Options are added at script/command-build time, never in `pubkey_line`.
- Command-path failures after verify keep the key; Cancel revokes then
  deletes (§4.1).
- `add-key` on a legacy line for the same key rewrites it to the kit's line
  (§5.1); kit 1.1.0 also fixes the password-assuming messages.
- AI host verify uses `mcp_info`, not `status_core` (§5.2); AI host key
  directories get an orphan sweep.
- Android shows Copy only; Share is a follow-up (§4.1, §9).
- Two Tauri commands (prepare / finish) for the Command path (§4.1).

## 10a. Build order

1. Notes: options on install lines + Command method, desktop and Android.
2. Kit `add-key` (kit repo, 1.1.0), and independently the AI host Command
   method in Jodd, plus the AI host orphan sweep.
3. Desktop Key file → "opens the door" over russh.

Steps 1 and 3 touch the same setup code; do them in one branch, in order.
Step 2's kit half can run in parallel in the kit repo.

## 11. Docs to update when built

- `docs/BACKEND-SSH.md` §6 (Android: Command method), §8 (third method,
  `restrict`, key-file semantics), §10 (Command path, `add-key`, no unlock).
- `docs/ai-host/USER-GUIDE.md` and `USER-GUIDE.th.md` — setup, Reinstall,
  Change server recipes.
- `docs/ROADMAP.md` #8b — reword to point at §6.3 (still deferred).
- Kit repo: `INSTALL.md`, `HOW-IT-WORKS.md`, `CHANGELOG.md`.
- `CHANGELOG.md` at release.
