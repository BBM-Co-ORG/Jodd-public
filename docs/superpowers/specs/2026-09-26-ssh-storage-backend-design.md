# SSH storage backend — design

**Date:** 2026-09-26
**Status:** approved in brainstorming, awaiting spec review
**Scope:** sub-project 1 of 2. Sub-project 2 — running programs installed on
the server (remote execution, e.g. an agent CLI as an `LlmProvider`) — gets its
own spec and reuses the `SshSession` seam defined here.

## Why

Every existing backend is either proprietary (Gmail, Microsoft, iCloud) or
confined to one machine (LocalFs). An SSH server is something any user can
obtain, owns outright, can share between their devices, and can run their own
programs against. This backend stores notes as plain files on such a server so
that:

- several Jodd installs (Mac, Windows, later Android) share one vault, and
- **programs on the server read and write the same files** — scripts, cron
  jobs, agents. The file format is therefore a contract with outside programs,
  not an internal detail.

**This backend does not round-trip with Apple Notes.** Apple cannot sync to an
SSH server any more than to LocalFs. It is a Jodd-owned store, a deliberate
exception to the project's central correctness goal.

## Decisions taken in brainstorming

| # | Question | Decision |
|---|---|---|
| 1 | Storage or execution first | Storage first; it builds the connection layer execution will reuse |
| 2 | Platforms | Desktop first; a `SshSession` seam leaves room for a pure-Rust Android implementation |
| 3 | How the user names the server | An SSH target string (`user@host` or a `~/.ssh/config` alias) plus a root path. Jodd stores **no credential** |
| 4 | Use case | A hub shared by devices **and** by programs on the server |
| 5 | File format | Markdown + YAML frontmatter |
| 6 | Content Markdown cannot hold | Fall back to `.html` + frontmatter for that note |
| 7 | Files created by outside programs without frontmatter | Adopt: mint a uuid, write the frontmatter back, keep the file name |
| — | Transport | System `ssh` running one persistent remote `sh`; every operation is an `exec` of a POSIX shell script. No SFTP, no ControlMaster |

## 1. Account model and layout

### Account

- New `BackendKind::Ssh`, prefix `"ssh"`. `ALL_BACKENDS`, `backend_prefix`,
  `canonical_uuid_for`, `mint_uuid_for`, `Capabilities::for_backend`,
  `incremental_pull_supported` and `orphan_cleanup_supported` all match
  exhaustively, so the compiler lists every site (gotcha #18).
- Follows the LocalFs precedent in `add_local_account`: **id = `ssh:<uuid>`**,
  `email` = display name (renamable without touching the id), `root_dir` =
  the remote root path. One new field, `ssh_target: Option<String>`,
  `#[serde(default)]`.
- `accounts.json` holds target and path only. Keys come from `ssh-agent` /
  `~/.ssh/config` / `IdentityAgent`; nothing is read from or written to the OS
  credential store (keeps gotcha #15 out of reach entirely).
- uuid shape: lowercase-hyphenated `Uuid::new_v4()`. There is no Apple wire
  format to conform to, and lowercase is what scripts produce.

### Capabilities

```rust
BackendKind::Ssh => Capabilities {
    folder_model: FolderModel::SingleExclusive,
    fidelity: Fidelity::Full,
    save_semantics: SaveSemantics::InPlaceUpdateNeedsExplicitMove,
    has_trash: true,
    writes: Writes { notes: true, relocate: true, folders: true, sidecars: false },
}
```

`InPlaceUpdateNeedsExplicitMove` because a content save rewrites the same file
in place (the name never changes on edit) and a folder move is a separate `mv`
— the same shape as Microsoft, already branched on in `push_one_dirty`.
`sidecars: false` because the pin lives in the frontmatter.

### Layout under the root

```
<root>/
  Notes/                  label "Notes"; a sub-folder is a directory
    Work/
      meeting-notes.md    name = slug of the title at creation; never renamed after
      colored.html        a note that does not fit Markdown
  .jodd/
    trash/                flat; names via LocalFs's trash_encode
    tmp/                  staging for atomic write → mv
```

- Slug collision within a folder → `-2`, `-3`, …
- Only `*.md` and `*.html` under `Notes/` are notes. Everything else is ignored.

### File contract

```
---
uuid: 5f3a9c1e-7b2d-4e8a-9c01-2f6d8a4b1e33
title: Meeting notes
created: 2026-09-26T10:00:00+07:00
pinned: false
---
# Meeting notes

- [ ] follow up #work
```

- **Identity is `uuid` alone** — never the path or extension, which both
  change over a note's life.
- **The title is the body's first line** with a leading `#`/`# ` stripped
  (gotchas #11, #21). `title:` verifies; it never decides. On mismatch the
  first line wins.
- Tags are inline `#hashtags`, derived by `AppleHtmlDeriver` as on every
  backend.
- Unknown frontmatter keys written by outside programs are **preserved** on
  rewrite.
- **Attachments are out of scope for v1.** A note carrying an inline image is
  refused with `push_blocked_reason = "attachments are not supported on SSH
  accounts yet"` (gotcha #14's four obligations apply).

## 2. Session and read path

### The seam

```rust
#[async_trait]
pub trait SshSession: Send + Sync {
    async fn exec(&self, script: &str, stdin: &[u8]) -> Result<ExecOutput, SshError>;
}
pub struct ExecOutput { pub stdout: Vec<u8>, pub stderr: String, pub exit: i32 }
```

Three implementations:

- **`SystemSsh`** (desktop, v1). Spawns
  `ssh -o BatchMode=yes -o ServerAliveInterval=15 -T <target> sh` once per
  account and keeps it alive. Each request is framed:
  `printf '__JODD_BEGIN_%s\n' <nonce>; ( <script> ) <<stdin>>; printf '\n__JODD_END_%s %d\n' <nonce> $?`.
  Requests are serialised behind a `Mutex`. EOF, a broken pipe, or a
  per-request timeout kills the child; the next request respawns it.
  - macOS: `/usr/bin/ssh` by absolute path — a Finder-launched app has a
    minimal `PATH`, which `tauri dev` from a terminal would never reveal.
  - Windows: `%SystemRoot%\System32\OpenSSH\ssh.exe`, spawned with
    `CREATE_NO_WINDOW`. ControlMaster is unavailable on Win32-OpenSSH, which
    is why the session is a persistent `sh` rather than per-op `ssh`.
- **`LocalShSession`** (tests). Runs the same scripts through the local `sh`
  in a tempdir.
- **`RusshSession`** (Android, follow-up — not in this spec). One `russh` exec
  channel per request satisfies the same trait; no SFTP is needed anywhere.

All file content crosses the session as base64, so framing markers inside a
note and CRLF translation cannot corrupt it. Every path goes through
`sh_quote()` (single quotes, `'` → `'\''`); file names now come from outside
programs, so this is an injection surface. File names containing a newline
are skipped and counted.

### Flavour probe

Once per session: `uname`, and which of `sha256sum` / `shasum -a 256` exists.
Both print `<hash>  <path>`, so the flavour changes one command name and
nothing downstream.

### Listing and change detection

- `list_all_notes(cache_by_id)`: one exec —
  `find Notes -type f \( -name '*.md' -o -name '*.html' \) -exec <sha> {} +`.
  Returns path + sha256 for every note file.
- **`Note::version` = the file's sha256.** Content hash, not mtime+size: an
  outside program can rewrite a file within the same second at the same size.
  The same value is the compare-and-swap token (§3), so there is one version
  concept.
- Path + sha equal to the cached row → reuse the cached note (Gmail's
  `cache_by_id` pattern). Changed files are fetched in batches of ~50 per exec.
- `changes_since` is a cheap detector: cursor = sha256 of the sorted listing.
  `incremental_pull_supported(Ssh) = true`.
- Decode: `.md` → `md_to_html` → `taskify_checklists` → `sanitize_note_html`
  (the existing jodd-mcp write path in `llm/markdown.rs`); `.html` →
  `sanitize_note_html`.
- **Adoption** of a file without frontmatter: mint a uuid, derive the title
  from the first line (or the file name if the body is empty), and write the
  frontmatter back with a CAS write in the same round. The file name is kept.
  A CAS failure leaves it for the next round.
- **One uuid in two files** (`x.md` + `x.html`, or a copy made by a script) →
  a dup, reported through the existing `DedupSummary`.

## 3. Write path

### Format choice per save — `backend/ssh/mdconv.rs`

`html_to_md(html) -> Option<String>` returns `Some(md)` **only if**
`normalize(md_to_html_pipeline(md)) == normalize(html)`. `None` → the note is
written as `.html`.

The decision is a measurement of the round trip, not a predicted whitelist
(the doctrine of gotcha #24). The converter emits:

- Jodd task rows (`<div class="jodd-task" style="margin-left:N·28px"><input
  type="checkbox" [checked]>&nbsp;…</div>`) ↔ `- [ ]` / `- [x]`, with nesting
  level `margin / 28` ↔ two-space indentation.
- Headings, bold, italic, strike, links, ordered/unordered lists, blockquote,
  inline code and code blocks.

Colour, font size, underline, Apple tables and anything else that fails the
round trip fall back to `.html`.

On disk a note is frontmatter + `# {title}` + body.

### Compare-and-swap write

`SaveOp` gains **`base_version: Option<&'a str>`**, populated from the row's
`remote_version`. This is the one shared-interface change; the other verticals
ignore it. (About ten construction sites, tests included — planning enumerates them.)

```sh
cur=$(<sha> "$f" 2>/dev/null | cut -d' ' -f1)   # empty if absent
[ "$cur" = "$expected" ] || exit 3               # changed under us
base64 -d > "$tmp" && mv "$tmp" "$f" && <sha> "$f"
```

- Create: `expected` is empty — the file must not exist. On a slug collision,
  try `-2`, `-3`, …
- `.md` ↔ `.html` switch: CAS-write the new file first, then remove the old
  one under a CAS check of its own hash. `SaveOutcome.remote_id` changes and
  the core re-points the cache id (already supported).
- Pin: rewrite the frontmatter through the same CAS write.

**Accepted race window.** Between the hash check and the `mv` there is a
window of milliseconds in which an outside program's write can be lost.
Outside programs will not honour any lock Jodd invents, so v1 accepts the
window and documents it in `docs/BACKEND-SSH.md`.

### Conflicts

Script exit 3 → `TransportError::Conflict`, **retryable**. The next pull sees
the new sha and the existing `reconcile_one` creates the keep-both conflict
copy. No SSH-specific conflict path. Planning must verify that the push path
does not treat `Conflict` as permanent (no branch for it exists today in
`sync_worker.rs` or `reconcile.rs`).

### Other operations — one exec each

| Operation | Script |
|---|---|
| `delete` | `mv` into `.jodd/trash/<trash_encode(path)>` |
| `untrash` | the reverse; recreate the parent directory if needed |
| `move_note` | `mv` into the target directory; `-2` on a name collision |
| `create_folder` / `ensure_folder` | `mkdir -p` |
| `rename_folder` | `mv` |
| `delete_folder` | same semantics as LocalFs |

## 4. Adding an account, errors, platforms

### `add_ssh_account(target, root, name, create)`

1. **Probe before persisting anything:** flavour probe, `test -d root`,
   `test -w root`. Any failure returns an error and `accounts.json` is not
   touched.
2. If the root is missing and `create` is true → `mkdir -p`.
3. Refuse a duplicate `(target, root)`.
4. On Android: `Err("SSH accounts are desktop-only for now")`, the same gate
   as `add_local_account`.

The UI gets an **Add SSH account** entry beside the local-vault one: a target
field, a root field with a **Browse** button (a remote directory picker driven
by `list_remote_dirs(target, path)` →
`cd "$p" && ls -1p | grep '/$'`), and a "create this directory" checkbox.

### Error classification

A pure function `classify(exit, stderr) -> TransportError`:

| Signal | Class | User-facing message |
|---|---|---|
| exit 255 + `Permission denied (publickey` | `Auth` | Key not loaded — run `ssh-add` |
| exit 255 + `Host key verification failed` | `Auth` | Unknown server — run `ssh <target>` once in a terminal to accept its host key |
| `Could not resolve` / `Connection refused` / `timed out` / EOF | `Transient` | — (retried) |
| script exit 3 | `Conflict` | — (keep-both) |
| script exit 4 | `NotFound` | — |
| anything else | `Permanent` (stderr attached) | stderr |

Jodd **never** sets `StrictHostKeyChecking=accept-new`: trusting a new host is
the user's decision. `Auth` sets the account's existing `blocked_reason`.

## 5. Testing

Storage tests run the **real scripts** through `LocalShSession`, so no SSH
server is needed in CI.

1. **`mdconv`:** `html → md → html` equality for every task-row shape
   (checked, unchecked, nested 1–3, bold/link inside, `&nbsp;` after the box),
   headings, emphasis, lists, links, blockquote, code. Negative cases →
   `None`: colour span, `<u>`, font size, Apple table, `<img>`. Fixtures come
   from real bodies already in `format_html.rs` / editor tests.
2. **Frontmatter:** parse/emit, unknown keys preserved, adoption (uuid minted,
   name kept), first-line title beats `title:`.
3. **Shell safety:** `sh_quote` with `'`, `$`, backtick, space, Thai, a
   leading `-` — **executed through `sh`**, not string-compared. Newline
   names skipped and counted.
4. **Framing:** a note whose content contains `__JODD_END_` does not truncate
   output.
5. **Flows via `LocalShSession`:** slug collision → `-2`; edit after listing →
   `Conflict`; `.md → .html → .md` with the old file gone only after the new
   one exists; **with the real `Db`:** an outside `sed` edit while the row is
   dirty → push `Conflict` → next pull → `reconcile_one` produces the conflict
   copy; **`sed -i 's/- \[ \]/- [x]/'` → next pull → the row's HTML carries a
   checked box.**
6. **`classify`:** OpenSSH stderr fixtures from macOS, Linux and Windows.
7. **Exhaustive tables:** extend `every_backend_declares_its_identity_policy`,
   `all_backends_lists_every_variant` and the capability tests.

Gate notes:

- CI (ubuntu) has both `sha256sum` and `shasum`, so **both flavours run in
  CI**; neither is skipped on the machine where it was written.
- `LocalShSession` tests are `#[cfg(unix)]` and join the documented Windows
  baseline beside the `agent_cli` tests.
- The gate is the full CLAUDE.md set: `cargo test --workspace`,
  `node scripts/gen-changelog.mjs`, `npx vitest run`,
  `npx svelte-check --threshold error`, `npm run build`.

### Live pass (manual, before merge)

Against a real server: add an account and browse to the root;
create/edit/move/delete/pin; tick a checkbox with `sed` on the server; have a
program on the server write a new `.md` without frontmatter; edit one note
from two Jodd instances at once (conflict copy); drop the network mid-sync;
repeat the add-and-sync on Windows. Record results in `docs/BACKEND-SSH.md`.

## Documentation

- New `docs/BACKEND-SSH.md`: protocol, file contract, the race window, the
  error table, live-pass results.
- CLAUDE.md: one row in the "Read it before" table (`backend/ssh/`) and one
  row in the Mechanism table. No new gotcha numbers until a real failure
  earns one.

## Out of scope

- Android (`RusshSession`) — follow-up; the seam is shaped for it.
- Attachments.
- Password authentication and any credential storage.
- Auto-accepting host keys.
- Locking against outside programs.
- Remote execution — sub-project 2.

## Amendments from planning (2026-09-26)

Found while writing the implementation plan by reading the code the spec
leans on. Each one replaces the text above where they disagree.

- **A1 — the Markdown pivot is the paragraph projection, not `md_to_html`.**
  `md_to_html` emits `<p>`; the editor stores `<div>` per line. Compared that
  way, almost every note would fall back to `.html`. Both directions go
  through `icloud::format_html::{parse_editor_html_objects_as_text,
  render_paragraphs}` — already the exact inverse of the editor's shapes —
  and the Markdown dialect is **line-oriented** (one paragraph = one line, an
  empty paragraph = an empty line; `pulldown-cmark` parses inline spans only).
  The gate is: every element/attribute is in the vocabulary
  `render_paragraphs` itself emits, **and**
  `formats_round_trip_equal(parsed, md_to_paragraphs(paragraphs_to_md(parsed)))`.
- **A2 — titles.** `.md`: the first line (`# ` stripped), then exactly one
  blank separator line, which the reader drops. `.html`: the frontmatter
  `title`, falling back to the file stem.
- **A3 — `.html` bodies are stored and read verbatim**, as LocalFs does for
  `.eml`. Sanitizing would strip the very markup the fallback exists to keep.
- **A4 — pin.** `writes.sidecars = true`, the Microsoft pattern: the pin
  lives on the note's own file, `put_sidecar`/`remove_sidecar` rewrite the
  frontmatter under CAS and return the new sha as `RemoteNoteVersion`,
  `list_sidecars` is a frontmatter header scan. `remote_pin_policy(Ssh) =
  LocalWins`.
- **A5 — `save_semantics = RelocatesOnContentPush`**, as LocalFs. The saved
  path is a *hint*: `save_note_full` resolves the file by `uuid` when the
  hint is gone, because programs on the server may `mv` files. A folder
  change writes into the new directory and retires the old file. With
  `InPlaceUpdateNeedsExplicitMove` the cached id would go stale after every
  move and the next CAS would read it as a conflict.
- **A6 — `exec(script)` takes one argument.** Payloads travel inside the
  script as base64 heredocs, so the framing never has to multiplex stdin.
- **A7 — `Note::date` is the file's mtime**, from `stat` in the same listing
  exec. The flavour probe therefore also picks GNU/BSD `stat` and
  `base64 -d`/`-D`.
- **A8 — no incremental detector.** The detector in `sync_worker` primes its
  cursor through the iCloud vertical, so `incremental_pull_supported(Ssh) =
  false` and `changes_since` is inert, as on LocalFs. The UI's existing
  `list_notes` polling drives SSH; `list_notes_in_folder` hashes one
  directory (`-maxdepth 1`), not the vault.
- **A9 — auth failures surface as `Transient` carrying the hint text.**
  Nothing in the shared worker consumes `TransportError::Auth`, and its
  `Display` is the bare word `auth`, which would throw the hint away. Every
  session-level failure is `Transient`: it is account-level, and `Permanent`
  would block individual notes.
- **A10 — restore** uses `RestoreKind::UntrashToEncodedPath`, as LocalFs.
  Restoring onto an occupied path is a `Conflict`.
- **A11 — the root is resolved at add time**: `~` is expanded on the server
  and the stored `root_dir` is the absolute `pwd -P`.
