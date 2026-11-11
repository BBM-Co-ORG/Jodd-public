# SSH easy setup — "Managed" connections for people who have a VPS, not a terminal

Status: design, awaiting review. Builds on
[2026-09-26-ssh-storage-backend-design.md](2026-09-26-ssh-storage-backend-design.md)
and [docs/BACKEND-SSH.md](../../BACKEND-SSH.md). Nothing here is implemented.

## Why

Adding an SSH account today assumes the user is comfortable with a terminal. The
dialog asks for `me@host` *or an `~/.ssh/config` alias*, and the backend runs
`ssh` with `BatchMode=yes`, so it can never ask for a password or a passphrase.
Before Jodd will connect, the user must already have: an ssh key, that key
installed on the server, the key loaded into ssh-agent (`ssh-add`), the host key
accepted by running `ssh <target>` once by hand, and usually a hand-written
`~/.ssh/config` alias. The 2026-09-26 live pass needed every one of those steps.

The person this feature is for is an everyday user with a VPS or hosting plan
that has shell access. Their welcome email gives them a host, a username and a
password, or a key file downloaded from the hosting panel. That is all they
should need.

**Success:** with only what the hosting provider gave them, the user fills in at
most three fields, confirms the server's identity once, and the account works
and keeps working without a password. No terminal, no `ssh-add`, no `~/.ssh`.

## Decisions taken in brainstorming (2026-09-27)

1. **Credentials vary, so support both.** The user may have a password *or* a
   provider-issued key file. The dialog offers "Password" and "Key file".
2. **Jodd owns a key, kept as a file in its own data folder** (owner-only
   permissions, no passphrase). Chosen over the OS keychain (needs a temp file
   per connection plus keychain prompts on every rebuild/update) and over a
   Jodd-private ssh-agent (awkward on Windows, most moving parts). The trade-off
   is accepted: anyone who can read the user's files on that computer can use the
   key.
3. **Approach A: drive the system `ssh` with an askpass helper.** Not an
   embedded `russh` (heavy dependency on every build incl. the Android NDK
   cross-build, and a second SSH stack) and not a full replacement of system
   `ssh` (a separate, much larger project).
4. **The simple flow fixes the notes folder at `~/Jodd-Notes`.** Anyone who
   wants another folder, or the Browse picker, uses the Advanced dialog.
5. **Removing a managed account offers to revoke its key on the server**, on by
   default, best-effort.

## 1. Two kinds of SSH account

| | Advanced (exists today) | Managed (new) |
|---|---|---|
| Identifies the server by | `ssh_target`: `user@host` or a config alias | `ssh_target` = `user@host`, plus `ssh_port` |
| Credentials | ssh-agent / `~/.ssh/config` | Jodd's own key file, named by `ssh_key` |
| Host trust | the user's `~/.ssh/known_hosts` | Jodd's own `known_hosts` |
| Added by | `add_ssh_account` | `ssh_setup_managed` (new) |

An account is Managed exactly when `ssh_key` is set. Existing accounts have
neither new field; both are `#[serde(default)]` and load as Advanced with no
migration. Advanced accounts, including the tester's `jodd-zf`, are untouched.

### Account fields (`accounts.rs`)

- `ssh_port: Option<u16>` — `None` means ssh's default.
- `ssh_key: Option<String>` — the account's managed-directory name. Neither field
  is a secret. `Account::is_usable` (`ssh_target` and `root_dir` set) is
  unchanged.

### Where things live

`<data_base>/ssh/<dir>/` where `<dir>` is the account's uuid **without** the
`ssh:` prefix — `:` is illegal in Windows directory names, and the uuid is
already unique per account (`account_id_for`). It holds:

- `id_ed25519` and `id_ed25519.pub` — the key pair, generated with the system
  `ssh-keygen -t ed25519 -N ""`. No new cryptography code, no new dependency.
- `known_hosts` — one line, the host key the user confirmed.

Permissions: `0600` on the key file (Unix). On Windows OpenSSH refuses a key
whose ACL is loose, so setup runs
`icacls <key> /inheritance:r /grant:r "%USERNAME%:F"`. **Unverified on a real
Windows machine — it is a live-pass item, not an assumption.**

Each account gets its **own** key, so removing one account can never revoke or
break another that points at the same server.

### Spawning ssh (`backend/ssh/session.rs`, `backend/ssh/mod.rs`)

`SpawnSpec::ssh(target)` gains an options struct:

```rust
pub struct SshOptions {          // None fields = Advanced behaviour
    pub port: Option<u16>,
    pub key: Option<PathBuf>,
    pub known_hosts: Option<PathBuf>,
}
```

For a Managed account the argv adds `-p <port>`, `-i <key>`,
`-o IdentitiesOnly=yes`, `-o UserKnownHostsFile=<known_hosts>` and
`-o StrictHostKeyChecking=yes`. Everything else — `BatchMode=yes`,
`ServerAliveInterval`, the persistent `sh`, the framing, the scripts — is
unchanged, so sync behaves identically. `session_for(account_id, target)`
becomes `session_for(account_id, target, opts)` and treats a change in either as
a new session (today it compares `target` only). The existing test asserting
`!args.contains("StrictHostKeyChecking")` is for the Advanced argv and stays; a
new test pins the Managed argv.

## 2. What the person sees

### Screen 1 — "Connect to your server"

Fields: **Server address**, **Username**, and a choice of **Password** or
**Key file**. The address accepts `host`, `host:port` or `user@host` (a supplied
`user@` pre-fills Username). **Password** shows a password box; **Key file** shows
a file picker, and a passphrase box appears only when the chosen key turns out to
be encrypted. A small link, "Use my own ssh setup (advanced)", opens today's
dialog unchanged.

**Continue** contacts the server for the first time, to fetch its host key.

### Screen 2 — "Is this your server?"

Shows the address and the fingerprint (`SHA256:…`), with plain wording: "First
time connecting. If your hosting provider shows a fingerprint in their panel or
email, check that it matches. Otherwise it is normal to continue." It also says
that Jodd's key will give this computer the same access to the server's shell as
the password does — not just to the notes — because the backend runs `sh` and
cannot be confined to a folder. Buttons: **Trust and connect**, **Back**.

### Screen 3 — progress, then done

Four lines tick off; a failure stops at its line with the plain message from §4.

1. **Securing the connection** — write the confirmed host-key line to Jodd's
   `known_hosts`.
2. **Setting up Jodd's key** — generate it, or import the user's file.
3. **Signing in** — for a password, run ssh once with the askpass helper and add
   the public key to `~/.ssh/authorized_keys`; for a key file, connect with it.
   Either way, finish by connecting with Jodd's key alone to prove it works.
4. **Creating your notes folder** — `~/Jodd-Notes`, through the existing
   `check_vault(create = true)`.

On success the account appears in the sidebar and behaves like any SSH account.
If setup fails before the account is saved, the key directory just created is
deleted, so nothing half-configured is left behind.

## 3. Mechanics

### Fingerprint step — `ssh_fingerprint(host, port)`

`ssh-keyscan -T 10 [-p <port>] -t ed25519,ecdsa,rsa <host>` (it ships next to
`ssh` on macOS and Windows OpenSSH), preferring ed25519. The fingerprint comes
from `ssh-keygen -lf <tempfile>` (a temp file, not `-lf -`, whose Windows support
is unconfirmed). It returns `{ algorithm, fingerprint, known_hosts_line }`.
`known_hosts_line` uses the `[host]:port` form when the port is not 22.

**The frontend returns that same `known_hosts_line` to `ssh_setup_managed`, and
Jodd writes exactly what the user was shown** rather than re-scanning. That
closes the window in which a different host key could be swapped in between
"Trust" and the connection.

### Password handoff — Jodd is its own askpass helper

`ssh_setup_managed` runs ssh once with `SSH_ASKPASS=<the jodd executable>`,
`SSH_ASKPASS_REQUIRE=force`, a dummy `DISPLAY` (older OpenSSH, notably some
Windows builds, only honours askpass when `DISPLAY` is set), stdin from the
script, and the password in the environment variable `JODD_ASKPASS_SECRET` **of
that one child process only**. Extra options for this run:
`-o BatchMode=no -o PubkeyAuthentication=no -o NumberOfPasswordPrompts=1`.

`main()` checks, before Tauri starts, whether `JODD_ASKPASS_SECRET` is set. If
so it is being run as a helper: when the prompt in `argv[1]` mentions a password
or passphrase it prints the secret and exits 0; for any other prompt (notably a
yes/no host-key question) it prints nothing and exits 1, so ssh fails safe. This
keeps the **one-binary-in-`Contents/MacOS`** rule (gotcha #3): no second helper
executable to bundle.

`ProcessSession` needs an env map on `SpawnSpec` to carry this.

### Installing the key — `scripts::install_authorized_key`

One idempotent script, run over the password-authenticated connection:
`umask 077; mkdir -p ~/.ssh; touch ~/.ssh/authorized_keys; chmod 700 ~/.ssh;
chmod 600 ~/.ssh/authorized_keys`, then append the public key **only if its key
body is not already present**. The key line carries a comment
`jodd-<short-id>@<computer name>` so the user can recognise it later. Re-running
setup never duplicates the line.

### Importing a key file

The chosen file is copied into the managed directory (never referenced in
place, so moving or deleting the original cannot break sync) and set to owner-only
permissions. If it is encrypted (`ssh-keygen -y -P ""` fails), the passphrase is
supplied through the same askpass mechanism to `ssh-keygen -p -N ""`, which
rewrites the **copy** without a passphrase, because background sync cannot answer
a passphrase prompt every few seconds. The dialog says so: "Jodd keeps an
unlocked copy of this key." That `ssh-keygen` honours `SSH_ASKPASS` here is to be
confirmed on macOS and Windows; using `ssh-keygen -P <passphrase>` instead would
put it in argv, visible to other processes, and is the fallback only if askpass
proves unusable.

### New commands (`ssh_commands.rs`)

- `ssh_fingerprint(host, port)` — screen 1 → 2.
- `ssh_setup_managed(host, port, user, method, secret_or_keypath, passphrase,
  known_hosts_line)` — screen 2 → 3; returns the new `Account`.
- `remove_account` gains an optional `revoke_key: bool`; see below.
- `add_ssh_account` and `list_remote_dirs` are unchanged and remain the
  Advanced path.

### Removing a managed account

Removal always deletes the local key directory and `known_hosts`. A checkbox in
the removal dialog, **"Also remove Jodd's key from the server"**, on by default
for Managed accounts, first runs `scripts::revoke_authorized_key` while the
connection still works: it deletes only the line containing this key's body
(rewriting through a temp file and `mv`, preserving permissions) and leaves every
other key alone. It is best-effort: if the server is unreachable the removal
still completes, and the dialog says the key remains and names its `jodd-…`
comment so the user can delete it.

## 4. Errors — plain language, still pure functions

Classification stays in `classify.rs` as pure functions over ssh's stderr, so
each real OpenSSH wording is a test fixture and no live server is needed.

| Signal | Message |
|---|---|
| `Could not resolve`, `Connection refused`, `timed out` | "Can't reach `<host>`. Check the address, and that the server is running." (adds the port when it is not 22) |
| `Permission denied (publickey)` — password not among the methods offered | "This server only accepts key files. Choose the Key file option." |
| `Permission denied (…password…)` | "Wrong username or password, or this host does not allow password login." — the two causes cannot be told apart, so the message names both and no classifier guesses (same rule as the Microsoft admin-consent refusal) |
| Chosen file starts with `PuTTY-User-Key-File` | "This is a PuTTY key. In PuTTYgen choose Conversions → Export OpenSSH key, then pick that file." Conversion is not built: `ssh-keygen` cannot read `.ppk`. |
| Key needs a passphrase and none / a wrong one was given | "This key is protected by a passphrase" / "That passphrase didn't unlock the key." |
| File is not a private key | "That file isn't a private key." |
| `REMOTE HOST IDENTIFICATION HAS CHANGED` or a `known_hosts` mismatch on a later connect | "This server's identity is different from when you trusted it. That happens after a server reinstall, and also if someone is intercepting the connection." Remedy in v1: remove the account and add it again — cheap, because notes and their ids live on the server. |
| Vault-directory refusals | existing messages (`check_vault`, exit 4 / 5 / 97) |

Advanced accounts keep today's messages, including the `ssh-add` hint. Session
failures remain `Transient` (A9) and never push-block a note (gotcha #14).

## 5. Security

- **Password:** never written to disk, never in argv, present only in the
  environment of the one child `ssh`, cleared from the form after submit, and
  never logged. The existing code logs ssh's stderr, so the stderr-forwarding
  path must be checked to ensure no prompt text or secret can appear in it.
- **Key:** an owner-only file with no passphrase. The UI states plainly (screen 2)
  that it grants shell-level access to the server as that user.
- **Host trust:** Jodd's own `known_hosts` with `StrictHostKeyChecking=yes`. A
  changed host key is refused and never silently accepted. Jodd still never
  passes `StrictHostKeyChecking=accept-new`; the only trust decision is the
  explicit "Trust and connect" click, and what is written is what was shown.
- **The password prompt is a real trust point.** Most hosts do not publish their
  fingerprint, so a typical user will click Trust without comparing anything.
  That is the same trade-off PuTTY and Termius make, and is weaker than today's
  "Jodd never trusts a host on its own". It is accepted for this audience and
  named in the UI.
- Environment variables of a process are readable by other processes of the same
  user. That is the same trust boundary as the key file itself, and is why the
  password lives in that one child's environment for seconds, not in a file.

## 6. Testing

**Pure unit tests** (no server): parsing the address; classifying every stderr
fixture in §4; detecting `.ppk` and encrypted keys; building the Managed argv;
parsing `ssh-keyscan` and `ssh-keygen -l` output; the `[host]:port` known-hosts
form.

**Script tests** (a local `sh` in a temp directory, the existing pattern):
install is idempotent, leaves `~/.ssh` at 700 and `authorized_keys` at 600; and
revoke removes only the matching line and preserves every other key.

**Key-file path end to end in CI:** a throwaway unprivileged `sshd` on a random
port with public-key login as the current user. That needs no root, so it runs on
ubuntu-latest.

**Password path:** password login needs root and cannot run in CI. CI therefore
tests only Jodd's half of the handoff, using a stub `ssh` that invokes
`$SSH_ASKPASS` and asserts it receives the secret for a password prompt and
nothing for a yes/no prompt. **That proves our helper's contract, not that real
OpenSSH accepts it — the live pass does.**

**Live pass** (new rows in `BACKEND-SSH.md` §7): password setup against a real
VPS; key-file setup including a passphrase-protected key; a key-only server; a
wrong password; a changed host key; removal with and without the revoke box; and
the whole flow on Windows, which cannot be run from the current machine.

## 7. Documentation

`BACKEND-SSH.md` gains a §8 "Managed connections" and updates §2 (spawn options)
and §5 (error table). `CLAUDE.md` gets one line under the SSH pointer. A gotcha
is added only if the live pass finds a real trap.

## Out of scope for v1

Android; converting `.ppk` keys; a re-verify screen after a host-key change;
a custom notes folder in the simple flow; revoking keys held by other devices;
an embedded `russh`; RSA fallback for a server that rejects ed25519.

## Open items to settle in planning

- Whether `ssh-keygen -p` and `ssh-keygen -y` honour `SSH_ASKPASS` on the
  OpenSSH builds shipped with current macOS and Windows 10/11 (§3).
- The exact `icacls` invocation and whether OpenSSH accepts the resulting ACL
  (§1).
- Which OpenSSH versions on Windows require the dummy `DISPLAY` (§3).

## Amendments from planning

Found in the final whole-branch code review, once the removal path was
exercised against a real local `sshd` rather than read on the page.

- **A1 — revoke-on-removal never runs for a key-file (imported) account.**
  §3's "Removing a managed account" and Decision 5 describe one revoke
  checkbox for every Managed account, on by default. That is correct for the
  Password path, where the key in `~/.ssh/authorized_keys` is one Jodd itself
  generated and installed. It is wrong for the Key-file path: there the key is
  the user's OWN provider-issued key, which Jodd only verified, never
  installed. Revoking it on removal could lock the user out of their only
  access to the server, and if two Managed accounts happen to import the same
  key file (not unusual — the same provider-issued key covering more than one
  logical account), revoking it via one account's removal silently breaks the
  other's access too, quietly violating "each account gets its own key" for
  exactly the accounts where that promise was never true to begin with.

  **Decision:** `ssh_setup_managed` now writes a one-word marker file
  (`source`, containing `generated` or `imported`) into the account's key
  directory at the point it already knows which `PublicKeySource` variant was
  used — no new `Account` field, since the directory already exists and
  outlives the account row until removal deletes it. `perform_account_
  removal`'s revoke step reads that marker before ever attempting a revoke,
  and treats a missing or unreadable marker as `imported` — the safe default,
  so any ambiguity (an older account predating this marker, a filesystem
  hiccup) fails toward NOT revoking rather than toward it. This check is
  unconditional: it runs the same way whether `revoke_key` came from the
  user's checkbox (`remove_account`) or from the hardcoded `true` the
  deferred-removal completion path in `sync_worker.rs` sends for an account
  that was still Draining when removal was requested — both call the same
  `perform_account_removal`, so neither can bypass it.

  The removal dialog's checkbox itself is unchanged by this amendment — it
  still offers "Also remove Jodd's key from the server" for every Managed
  account, including a Key-file one, where checking it is now a harmless
  no-op rather than a lockout risk. Making the checkbox itself disappear for
  a Key-file account is a real but separate UX polish item, not required for
  the lockout risk to be closed, since the backend gate is authoritative
  regardless of what the checkbox sends.
