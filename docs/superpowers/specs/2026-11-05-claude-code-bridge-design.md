# Claude Code bridge — notes on disk, and folders as skills

Status: design written 2026-11-05 at the owner's request ("write the spec for
items 1 and 2"). **Phases 1–3 built 2026-11-05** (§13–§15 say what differs from the
design below). **Phases 4–5 designed and built the same day at the owner's
request ("design and implement every phase"), §16–§17.** Live passes owed. Calls
marked *(owner)* were made in that conversation; every other call is the spec
author's and is listed in §12 for checking.

Scope: two features that share one engine and one command —
**Part 1, the mirror** (a read-only Markdown copy of the user's notes on disk)
and **Part 2, skills** (a Jodd folder published as a Claude Code skill). The
next items from the same conversation — a `start_project` context pack, linking
a project to a brand, and semantic search — are out of scope (§11).

## 1. Problem and intent

The owner described two ways of working, the first one a friend's:

1. **A knowledge base as a project playbook.** The friend keeps working
   methods, document templates, past artifacts and domain knowledge in an
   Obsidian vault. To start a project (their example: a POS + ERP system) they
   give Claude Code a short brief, and Claude Code pulls the rest out of the
   vault on its own.
2. **Artifacts that follow a brand.** An agent makes images, video and slide
   decks that follow a brand guide and a style guide kept in the knowledge base.

The owner uses **Claude Code** *(owner)*. Jodd already gives Claude Code a
memory (spec 2026-10-06, M1: `remember`, `brief`, the SessionStart hook,
measured working on 2026-10-06). It does not give Claude Code what makes the
friend's vault work. Measured on `main` at `0ae8bb4`:

| What the vault gives Claude Code | What `jodd-mcp` gives it today |
|---|---|
| Browse: `ls` a folder, follow the tree | No tool lists a folder or its notes. `search_notes` needs a query and returns at most 50 matches, each a bounded preview (`jodd-mcp/src/main.rs:284`) |
| Read whole files, `grep` across all of them | `read_note` reads one note in 20 000-character pages, by uuid (`memory.rs:10`) |
| Images, fonts, templates beside the text | No tool reads an attachment. MCP returns text only |
| Procedures Claude Code applies on its own | Nothing. A note is data to Claude Code, never a procedure |

Claude Code works best with two things, and Jodd should feed both rather than
teach Claude Code a new habit:

- **Files on disk.** Claude Code's own tools (`Read`, `Grep`, `Glob`) are what
  the friend's setup runs on.
- **Skills.** A folder at `~/.claude/skills/<name>/` holding a `SKILL.md`
  plus supporting files. Claude Code loads every skill's `description` at
  session start and reads the skill's body when a task matches it — "make a
  deck for client X" picks up client X's brand skill without being told.

**Intent.** Jodd stays the single place the owner writes knowledge — on the
Mac, on the iPhone through Apple Notes, on Android — and Claude Code sees it in
the two forms it already uses best: a folder of Markdown files it can browse,
and skills it applies by itself.

**Success.**
- From a session with no MCP tool call, Claude Code answers "what do my notes
  say about stock costing?" by grepping the mirror.
- After the owner edits a brand-guide note on the iPhone, the next Claude Code
  session makes a slide that follows the edited rule, using the real logo file
  from the skill folder, with nothing typed beyond the request.

## 2. Decisions

| # | Question | Decision |
|---|---|---|
| D1 | What writes the files | **One engine in `jodd_lib` (`agent_files/`), two callers:** `jodd-mcp sync-agent-files` (Phases 1–2, run from the SessionStart hook) and the app, debounced, while it runs (Phase 3). The CLI comes first: it needs no worker-to-UI route (gotcha #6) and it runs when the app does not |
| D2 | Mirror format | **The backup bundle's OKF v0.2 concept shape** and its Markdown writer (`backup::body::encode_body`), so there is one Markdown writer, not two. The `jodd:` block is slimmer than a backup's (§13 item 1); trust is summarised in one field |
| D3 | What the mirror holds | **Opt-in per account.** In an included account, every note **except hidden folders** (`mcp_scope::ScopeFile::is_hidden`, gotcha #33) and inactive accounts — the same rule MCP read tools follow, so the mirror shows Claude Code nothing MCP would not |
| D4 | Plaintext on disk | The cache is encrypted at rest; the mirror is not. **Opt-in, said plainly in Settings, and secrets masked** with `privacy::detect::secrets` before a file is written |
| D5 | Writing back | **The mirror is read-only.** Files are written read-only. A change made in the mirror is overwritten at the next sync. Writes go through `create_note` / `update_note` / `remember`, which keep provenance and capability checks |
| D6 | What becomes a skill | **Any folder the user marks** "Publish as Claude Code skill", not only folders inside `Notes/__Agent__/` — a brand guide usually already lives in the owner's own folders |
| D7 | Which note is `SKILL.md` | The folder's **cover note**: the note whose title equals the folder's name, or the only note when the folder holds one. Without one, the skill is not published and Settings says why |
| D8 | Trust | **A skill is instructions, not data.** A folder agents may write on this device cannot be a skill. A note whose newest content came from an agent or a Jodd workflow (`Tier::Unreviewed` with `unreviewed_by` set) **keeps its last published version** until the owner marks it reviewed. An edit from another device (`remote`) is published (§6.5) |
| D9 | Where skills go | `~/.claude/skills/jodd-<name>/`, personal scope. Jodd only touches a directory that carries its own marker file |
| D10 | Claude Code settings | **Jodd never edits Claude Code's settings.** Settings shows snippets to copy, as it already does for the hook (`src/lib/agentWorkspace.ts:51`) |
| D11 | Platforms | Desktop only (macOS, Windows, Linux). Claude Code and `jodd-mcp` do not run on Android |
| D12 | Configuration home | A new `agent_files` key in `mcp_write_scope.json`, read and written only through `mcp_scope.rs` (`edit_at`), which is already the one model of agent-facing settings |

## 3. What is reused, and what is new

**Reused unchanged:**
- `Db::backup_read(account_id)` (`db.rs:1841`): one transaction, every live
  note with folders, tags, citations, provenance events, render-loss flags and
  attachment metadata. Its tag map already follows gotcha #38.
- `Db::attachment_bytes` (`db.rs:2693`): one attachment at a time.
- `backup::body::encode_body`: exact Markdown when it reproduces the HTML,
  otherwise readable text (`readable_text`). Either way it is good input for an agent.
- `backup::okf::{render_concept, render_index}`, `backup::naming::{account_slug,
  dir_for_folder, NameAllocator}` — the same file names a backup uses, which
  are already safe on Windows and handle collisions while ignoring case and NFC/NFD.
- `mcp_scope::{load_from, edit_at, ScopeFile::is_hidden}`, and the
  inactive-account filter `jodd-mcp` uses (`hidden_account_ids`, `main.rs:21`).
- `provenance::derive` → `Trust { tier, unreviewed_by, .. }`.
- `privacy::detect::secrets` → `SecretHit { span, label, mask_label }`.
- `brief`'s failure discipline (`main.rs:498`): every failure is one stderr line and exit 0.

**Changed:** `backup::naming::sanitize_segment` becomes `pub(crate)` (skill
reference files need it). `brief` gains one line (§5.7).

**New:** `src-tauri/src/agent_files/{mod.rs, config.rs, mirror.rs, skills.rs,
status.rs}`; the `sync-agent-files` subcommand in `jodd-mcp/src/main.rs`;
Settings sections and a folder-menu item (§7).

**Claude Code facts this design relies on** — from
<https://code.claude.com/docs/en/skills.md>, read 2026-11-05. Re-read it before
building; skills move fast.
- Personal skills live at `~/.claude/skills/<name>/SKILL.md`. Precedence is
  enterprise over personal over project.
- Descriptions are loaded at session start, and the body when a skill is used.
  The listing truncates `description` + `when_to_use` at **1 536 characters**.
- Changes to `SKILL.md` under `~/.claude/skills/` are picked up **within a
  running session**. A top-level skills directory that did not exist at session
  start needs `/reload-skills`.
- `SKILL.md` may link supporting files by relative path. Guidance: **keep
  `SKILL.md` under 500 lines**, with detail in separate files.
- Skill content can run shell commands at render time with the `` !`command` ``
  syntax (`disableSkillShellExecution` turns this off). **This is the reason
  for §6.6.**
- Outside Claude Code (claude.ai upload, Skills API), only `name`,
  `description`, `license`, `compatibility`, `metadata` and `allowed-tools`
  are accepted. Jodd writes only `name` and `description`, so an exported
  skill also uploads to claude.ai unchanged.
- Not documented: how binary assets in a skill folder are handled. Claude Code
  reads them by path like any file. That is what §6.4 assumes; the live pass
  checks it.

## 4. The shared engine

### 4.1 Configuration

```json
{
  "agent_workspace": { "account_id": "gmail:kaiwan@bbmedia.co.th" },
  "accounts": { "…": { "allowed_folders": ["…"], "hidden_folders": ["…"] } },
  "agent_files": {
    "mirror": {
      "enabled": true,
      "path": "/Users/kaiwan/Jodd for agents",
      "accounts": ["gmail:kaiwan@bbmedia.co.th", "icloud:kaiwan@me.com"]
    },
    "skills": [
      { "name": "brand-bbmedia",
        "account_id": "gmail:kaiwan@bbmedia.co.th",
        "folder": "Notes/BBMedia/Brand",
        "assets_dir": "/Users/kaiwan/Brand kits/BBMedia" }
    ]
  }
}
```

- Parsed by `agent_files::config` through `mcp_scope::load_from`. Edited only
  by `mcp_scope::edit_at`, which keeps unknown keys and writes atomically.
- `mirror.path` defaults to `<home>/Jodd for agents` — visible, so the owner
  can open it in Finder or in Obsidian. See §12 C1.
- An `agent_files` key that does not parse fails closed: **nothing is
  written and nothing is deleted**, and the status reads "settings unreadable".
  Same rule as `load_scope_for_read`.
- `assets_dir` is optional (§6.4).

### 4.2 `jodd-mcp sync-agent-files`

```
jodd-mcp sync-agent-files [--quiet] [--deadline-secs N]   (default 15)
```

1. Same setup as `brief`: `secrets::init`, resolve the DB, refuse a
   plaintext DB, open it. **If the open fails, write nothing and delete
   nothing** (gotcha #44: never act on a cache that did not open).
2. Load the scope file. Unparseable → stop (§4.1).
3. Take the lock `<mirror>/.jodd/lock` (`std::fs::File::try_lock`, no new dependency).
   Held by someone else → exit 0 with "another sync is running".
4. Mirror (§5), then skills (§6). Each account and each skill is independent:
   one failing does not stop the others.
5. Write `status.json` (§4.4).

**Stdout stays empty.** A SessionStart hook's stdout is injected into the
session; only `brief` speaks there. Every failure is one stderr line and exit
0. A deadline thread exits 0 when `--deadline-secs` runs out, exactly as
`brief` does. Progress survives that exit: the manifest is saved every 100
files (§5.5).

**The hook** (Settings shows it; it replaces today's `HOOK_SNIPPET`):

```json
{ "hooks": { "SessionStart": [ { "hooks": [
  { "type": "command", "command": "jodd-mcp sync-agent-files --quiet" },
  { "type": "command", "command": "jodd-mcp brief --cwd \"$CLAUDE_PROJECT_DIR\"" } ] } ] } }
```

Sync runs before `brief`, so the briefing describes the files as they now are.

### 4.3 Safety rules (both parts)

- **Touch only what Jodd wrote.** The mirror deletes only paths listed in its
  own manifest. A skill directory is touched only if its marker
  `.jodd-skill.json` is present. A file someone else put in either place
  survives.
- **An account Jodd has not finished listing is skipped**, and its previous
  files are kept (`Db::full_listing_at` is `None` — the same gate backups use,
  `backup/run.rs:101`). Gotcha #43: the cache is what Jodd knows. The mirror never
  infers deletions from a listing.
- **A run that would delete more than half of an account's mirrored notes, and
  more than 20 of them, deletes nothing** and reports "large change held
  back". Settings then offers "Apply anyway". This guards against an
  unforeseen empty read.
- Every file is written to a temp file and renamed into place.

### 4.4 Status

`<mirror>/.jodd/status.json` (Phase 2 decides where skills report when the mirror is off):

```json
{ "finished_at": "2026-11-05T09:12:03+07:00", "outcome": "ok",
  "accounts": [{ "account_id": "…", "notes": 812, "written": 3, "deleted": 0,
                 "masked_notes": 4, "skipped_attachments": 1, "held_back": false }],
  "skills": [{ "name": "brand-bbmedia", "state": "published",
               "waiting_for_review": ["Logo rules"], "problems": [] }] }
```

Settings reads this file when its section opens and when the window regains
focus. In Phases 1–2 **nothing on screen changes without one of those two
events**. That is the whole worker-to-UI route for this design. Phase 3 has to
trace a new one (gotcha #6).

## 5. Part 1 — the mirror

### 5.1 Layout

```
Jodd for agents/
  README.md                       generated: what this is, read-only, how to write back
  index.md                        accounts, with note counts (render_index)
  gmail-kaiwan-bbmedia-co-th/     account_slug
    index.md
    Notes/
      index.md                    this folder's notes and subfolders, one line each
      Stock costing.md
      BBMedia/
        Brand/
          Brand BBMedia.md
      _attachments/
        Stock costing/diagram.png
  .jodd/
    manifest.json                 uuid → path, content hash, attachment list
    status.json
    lock
```

- Directories come from `dir_for_folder(label)`, file names from
  `NameAllocator::file_for`. Names are allocated in sorted
  `(folder, title, uuid)` order, so a run with no edits produces the same names
  (no churn).
- `index.md` per directory uses OKF's reserved name. Each line is the note's
  title and its first body line, cut at 120 characters. This is what Claude
  Code reads to find its way, instead of a 50-result search.
- `[[wikilinks]]` are left as written. Files are named by title, so the
  links also resolve if the owner opens the folder in Obsidian.

### 5.2 One note file

```markdown
---
type: note
title: Stock costing
tags: [erp, inventory]
sources: [https://example.com/fifo]
jodd:
  uuid: 1f0c…
  account: gmail:kaiwan@bbmedia.co.th
  folder: Notes/ERP
  modified: 2026-10-30T14:02:11+07:00
  trust: human-written            # human-written | human-reviewed | unreviewed | not-recorded
  attachments: [_attachments/Stock costing/diagram.png]
---
Stock costing
…body from encode_body…
```

- Built from the same `NoteDoc` as a backup, with `jodd.history`,
  `content_sha256`, `sync_state` and `html` dropped. The mirror is for
  reading, not restoring. Restoring stays the backup's job.
- `trust` matters: an agent should weigh an `unreviewed` note (written by an
  agent or an ingest) as a claim, not as the owner's word. `brief` already
  says the same about workspace pages (`agent_memory/brief.rs:40`).
- The verbatim `Source` block an ingest saves is kept. Agents grep sources
  too, and the file has no paging limit.

### 5.3 Secrets

Before writing, the body text goes through `privacy::detect::secrets`. Each
hit is replaced by `[secret hidden by Jodd: <mask_label>]`, and the note counts
toward `masked_notes`. Only secrets are masked, not personal data: the mirror
is the owner's own notes, read by the owner's own agent, and masking names
would break the domain-knowledge use case. MCP read tools do not mask today;
§11 lists bringing them to parity as a follow-up.

### 5.4 Attachments

For each `AttachmentMeta` from `backup_read`, `attachment_bytes` is written to
`_attachments/<note file stem>/<filename or content id>`, unless it is larger
than **50 MB**, which is skipped and counted. Relative links to the files are
listed in frontmatter and appended to the body under `Attachments`.
Attachments exist in the cache where the backend delivered bytes. Before
building, check which backends fill the `attachments` table: iCloud inline
objects are expected to be missing. A note whose attachments are missing still
mirrors its text.

### 5.5 Incremental sync

Per account, from one `backup_read`:

1. Work out the target set: notes in included, active accounts, not in a
   hidden folder, not `deleted_pending`.
2. For each, the key is `sha256(rendered file)` — the rendered text, so a
   rename, a tag change or a new mask all count as changes. Equal to the
   manifest entry and the file exists → skip. Otherwise write it. If its path
   changed, remove the old path.
3. Manifest entries not in the target set → delete the file and its
   attachments (subject to §4.3's hold-back rule). This covers notes that were
   deleted, moved into a hidden folder, or belong to an account that was
   excluded or deactivated.
4. Rewrite the `index.md` files of directories that changed.
5. Save the manifest every 100 files and at the end.

Expected cost: the first run writes every note (about 800 for the owner's
main account). After that a run renders every note and writes only what
changed. Rendering is the cost to measure in the live pass. If it exceeds the
hook's budget, compare `content_sha256(body_html)` plus the metadata first and
render only the notes that differ.

### 5.6 Read-only

Files are set read-only (`0444` / the Windows read-only attribute) and made
writable again for the next rewrite. `README.md` says, in two lines, that
changes here are overwritten and how to write to Jodd instead. A file whose
on-disk hash no longer matches the manifest is overwritten, and the run counts
it as `overwritten_local_edits` in the status. That makes the loss visible
without making the mirror a second source of truth.

### 5.7 Claude Code setup

Settings → Agent workspace → **Notes on disk for Claude Code**:

1. Turn it on, choose the folder and the accounts. The sentence beside the
   switch: "Jodd keeps a readable copy of these accounts' notes in this folder,
   unencrypted, without hidden folders or secrets, for Claude Code to read."
2. Copy the hook (§4.2).
3. Copy the read permission, so Claude Code reads the folder without asking:
   ```json
   { "permissions": { "additionalDirectories": ["/Users/kaiwan/Jodd for agents"] } }
   ```
4. **Sync now** runs the same engine in-process (the app has the DB open). On
   macOS it also proves the keychain grant once (`jodd-mcp --self-test`
   remains the CLI's own check).

`brief` gains one line when the mirror is on and its last run succeeded:

> Your Jodd notes are also files at `/Users/kaiwan/Jodd for agents` (read-only,
> updated 09:12). Start from `index.md`; grep for anything. Write with
> `create_note`, `update_note` or `remember`.

## 6. Part 2 — folders as skills

### 6.1 Marking a folder

Folder menu → **Publish as Claude Code skill…** opens a small dialog: the skill
name (prefilled with an ASCII slug of the folder name, and required when the
slug is empty, as it is for a Thai name), and an optional assets folder on disk.
Saving adds the entry to `agent_files.skills`. **Stop publishing** removes the
entry, and the next sync removes the directory.

The dialog refuses to save in these cases:
- The folder, or any folder under it, is in this account's `allowed_folders`.
  An agent that can write a skill can write its own future instructions. The
  check also runs the other way: adding an allowed folder that overlaps a skill
  is refused.
- The folder is hidden. Hidden wins. Hiding a published folder later
  unpublishes it at the next sync.
- The name is taken by another skill entry.

The account needs no write capability, so a Microsoft account works too.

### 6.2 Folder → skill

```
~/.claude/skills/jodd-brand-bbmedia/
  SKILL.md                      the cover note (D7)
  reference/
    Logo rules.md               every other note in the folder, as in §5.2
    Colours and type.md
    Templates/Deck outline.md   subfolders kept
  assets/
    logo-primary.svg            attachments of every note in the folder
    BBMedia-deck.pptx           … and the assets_dir copy (§6.4)
  .jodd-skill.json              marker: entry, manifest, last published hashes
```

`SKILL.md`:

```markdown
---
name: jodd-brand-bbmedia
description: "Use when making slides, images, video or any visual for BBMedia …"
---
<!-- Generated by Jodd from Notes/BBMedia/Brand. Edit it in Jodd; changes here are overwritten. -->

<cover note body, from encode_body, after its title line>

## Files in this skill
- [Logo rules](reference/Logo%20rules.md) — <first line>
- [Colours and type](reference/Colours%20and%20type.md) — <first line>
- assets/logo-primary.svg · assets/BBMedia-deck.pptx
```

- **`name`** is `jodd-` plus the entry's name, lowercase ASCII letters,
  digits and hyphens, at most 64 characters. Stricter than Claude Code
  requires, so the skill also uploads to claude.ai. The prefix keeps Jodd away
  from the owner's own skills.
- **`description`** is the cover note's first paragraph after its title, cut
  at 1 000 characters, under the 1 536 listing cap. **No paragraph → not
  published**, with the problem "add a line under the title that says when to
  use this skill". The description decides whether Claude Code picks the skill
  at all, so Jodd does not invent one.
- Front matter is serialised by `serde_yaml_ng` (already the backup's YAML library), never by string
  formatting, so a quote or a `---` in the note cannot end it early.
- A cover note longer than 500 lines is published, with a problem: "long — move
  detail into other notes in this folder".

### 6.3 Brand guides that agents can follow

The guidance below goes into the dialog's help text and into the `Conventions`
note. The agent needs:
- **exact values**: colours as hex, font names, sizes, spacing, aspect ratios;
- **logo rules** that name the files in `assets/`;
- **image style as prompt text**, with a negative prompt;
- **do / don't examples**;
- **a checklist** the agent runs on its own output before handing it over;
- and one rule Jodd puts in the help text verbatim: *"Never ask an image or
  video model to draw the logo; place the file from `assets/` on top."*

### 6.4 Assets

- **Attachments** of every note in the folder go to `assets/`, with names made
  unique in the skill (§5.4 caps apply). Apple Notes on a Gmail account can
  carry images. Fonts and `.pptx` templates usually cannot, and Jodd cannot
  attach new files yet (roadmap #1).
- **`assets_dir`** covers those: its regular files, recursively, are copied to
  `assets/` (copied, not symlinked: Windows, and so a later change to the
  source is a visible re-sync). Limits are 200 files and 500 MB; past either,
  the copy stops and the skill reports a problem. Symlinks and anything outside
  `assets_dir` are not followed.
- Assets are compared by size and mtime from the marker, and copied only when
  changed.

### 6.5 Trust: what gets published

For each note in the folder, `provenance::derive` gives `Trust`. The decision:

| Trust | Published |
|---|---|
| `HumanWritten`, `HumanReviewed` | yes |
| `NotRecorded` (a note Jodd indexed but never saw written, e.g. typed in Apple Notes before this device synced) | yes |
| `Unreviewed`, newest pending write is `remote` (another device, usually the iPhone) | **yes** *(see C3)* |
| `Unreviewed`, pending write by an agent or a Jodd workflow (`unreviewed_by` set), or an unrecorded gap | **no: the last published version stays**; a note never published is left out. Listed as "waiting for review". **Mark reviewed** in the editor releases it at the next sync |

The `remote` row is the iPhone use case (§1 success). The risk it accepts: a
remote change could come from an agent on *another* device whose own scope
allows that folder, because scope files do not sync. §6.1's rule protects this
device only. Settings offers **"Only publish reviewed notes"** per skill, which
moves the `remote` row to "no".

A note that contains a secret (§5.3) is not published at all: masking it inside
instructions is worse than leaving it out. It is listed as a problem.

### 6.6 Text that must not become a command

Claude Code expands `` !`command` `` in skill content by running the command.
A note can contain that text — pasted from a web page, or written by an ingest.
**Every Markdown file Jodd writes into a skill has each `` !` `` rewritten as
`` ! ` ``** (a space inserted). A conformance test asserts that no file under a
published skill contains the two-character sequence. Jodd writes no
`allowed-tools`, `hooks`, `shell` or other front matter beyond `name` and
`description`. The help text suggests `"disableSkillShellExecution": true` to
owners who use only Jodd skills.

### 6.7 Publishing

Each run compares each skill's rendered files with the marker's hashes. It
writes changes atomically, and `SKILL.md` last, so a session that reloads
mid-write sees the old skill or the new one, never half of each. It removes
files that are gone. An entry whose folder no longer exists (renamed, moved,
deleted) is **unpublished**, which fails closed, and Settings shows "folder not
found — choose it again". A directory `jodd-<name>` that exists without a
marker is never touched; the problem reads "a skill with this name already
exists and was not made by Jodd".

## 7. UI

- **Settings → Agent workspace** gains two sections under the existing ones:
  *Notes on disk for Claude Code* (§5.7) and *Skills for Claude Code*. The
  skills section lists each skill with its state (`published`, `waiting for
  review: 2`, `problem: …`), the folder, Open in Finder/Explorer, Stop
  publishing, and the "only reviewed" switch. Both read `status.json` (§4.4).
- **Folder menu**: *Publish as Claude Code skill…* / *Skill settings…*. Gated
  like the other AI entries (`src/lib/controllers/aiActions.ts`, the 2026-10-28 UX spec): hidden
  folders and agent-writable folders show the item disabled, with the reason in
  the tooltip.
- Desktop only. The phone UI shows nothing new.
- Copy follows the one-line rule of the 2026-10-20 mobile spec §4.4 and the
  words above. "Skill" stays in English: it is Claude Code's term.

## 8. Errors

| Case | Behaviour |
|---|---|
| DB cannot open / cache locked (gotcha #44) | Nothing written or deleted; stderr line; status untouched |
| Scope file or `agent_files` unparseable | Nothing written or deleted; status `settings unreadable` |
| Account never fully listed | Skipped, old files kept |
| Deadline reached | Exit 0; manifest saved at the last batch; next run continues |
| Lock held | Exit 0, "another sync is running" |
| Mirror path missing / not writable | Mirror skipped, status says which path; skills still run |
| Large deletion | Held back (§4.3) |
| Skill folder gone, cover note missing, no description, name taken, secret in a note | Skill not (re)published; previous files kept except "folder gone", which unpublishes; problem listed |
| `assets_dir` missing or over limits | Note attachments still published; problem listed |
| Disk full | Per-file failure; the temp file is removed, the old file kept, the run continues with the next account or skill |

## 9. Testing

**Unit (`agent_files`):**
- Hidden folders and inactive accounts never reach disk; hiding a mirrored
  folder deletes its files at the next run.
- An unparseable `agent_files` deletes nothing. A DB that failed to open
  writes nothing. An account without a full listing keeps its files.
- The hold-back threshold triggers and "apply anyway" passes.
- Only manifest paths are deleted; a foreign file in the mirror survives.
  A skill directory without a marker is never touched.
- A second run with no changes writes zero files and produces the same names
  (sorted allocation).
- Secrets are masked in the mirror; a note with a secret is left out of a skill.
- Trust table §6.5, every row, including "last published version stays" and
  the "only reviewed" switch.
- §6.6: a note containing `` !`rm -rf ~` `` publishes with no `` !` `` anywhere
  under the skill.
- A description containing `"`, a newline and `---` round-trips through YAML.
- Marking an agent-writable folder is refused, and allowing a folder that
  overlaps a skill is refused.
- Thai folder name: an empty slug requires a name; NFC and NFD titles in one
  folder get two distinct files (NameAllocator).

**Conformance:** one test enumerates `mcp_scope`'s read paths and asserts the
mirror uses `is_hidden` and the inactive-account filter. This is the same idea
as the MCP read-tool test (`main.rs:670`), so the next filter added to MCP
reads cannot skip the mirror.

**CLI:** `sync-agent-files` writes nothing to stdout in every outcome,
including a panic (`catch_unwind` as in `brief`).

**Live pass (macOS, the owner's Gmail + iCloud accounts, Claude Code):**
1. Enable the mirror, install the hook and the permission; a fresh session
   with no MCP call answers a question from a note by grepping the mirror.
2. A note in a hidden folder is absent from disk; a note with an API key shows
   the mask.
3. Mark a brand folder with a logo attachment and an `assets_dir` holding a
   font and a `.pptx`; `/skills` lists `jodd-brand-…`; "make a 3-slide deck
   for BBMedia" uses the skill, the hex colours and the real logo file.
4. Edit the cover note on the iPhone; after the next session start, the skill
   carries the edit. Edit a note through `update_note` from a second agent
   setup; the skill keeps the old text until **Mark reviewed**.
5. Rename the folder in the app → the skill is unpublished and Settings says so.
6. Measure the run time of steps 1–4 for the ~800-note account (first run,
   no-change run). This decides whether §5.5's cheaper comparison is needed.
7. Windows: steps 1 and 3 (read-only attribute, `%USERPROFILE%\.claude\skills`).

## 10. Phasing

Each phase is one PR and ships alone.

1. **Mirror.** `agent_files::{config, mirror, status}`, `sync-agent-files`
   (mirror only), Settings section, new hook snippet, `brief` line.
2. **Skills.** `agent_files::skills`, folder menu dialog, the two-way
   allowed/skill refusal in `mcp_scope`, Settings section, trust hold.
3. **Live updates from the app.** The app runs the engine in-process,
   debounced 30 s after a local edit or a pull that changed a note in an
   included account, skipped while `cache_lock` is engaged. Claude Code then
   picks up a skill edit in the running session. **Before building, trace the
   route from that write to Settings' status on screen (gotcha #6)**; Phase
   1's "re-read on open and focus" may no longer be enough when Settings is the
   window already in focus.

## 11. Out of scope

- Writing back through files (a watched, two-way vault). Writes stay on MCP.
- `start_project` (a context pack from a brief), linking a project page to a
  brand skill, semantic search — the next items from the same conversation.
- Attaching new files to notes (roadmap #1), and iCloud inline objects as
  mirror attachments.
- Masking secrets in MCP read tools (`search_notes`, `read_note`). Follow-up
  for parity with §5.3.
- Hidden-folder renames: hiding is by path, as in spec 2026-10-06 §8 item 1.
  A renamed hidden folder becomes visible to the mirror exactly as it does to
  MCP. The fix belongs to that limitation and covers both at once.
- Other agents' skill formats (thClaws, Codex). The mirror already serves any
  agent that reads files.

## 12. Calls for the owner to check

| # | Call | Alternative |
|---|---|---|
| C1 | Mirror default path `~/Jodd for agents` (visible) | Inside the app data dir (hidden from casual browsing) |
| C2 | Mirror opt-in per account, everything not hidden | An include-list of folders |
| C3 | iPhone (`remote`) edits publish to skills without review — **decided by the owner 2026-11-05: as proposed** | Every skill change needs Mark reviewed |
| C4 | Cover note = title equals folder name | A fixed title such as `SKILL`, or the pinned note |
| C5 | Skills under `~/.claude/skills/jodd-<name>` (personal) | A project's `.claude/skills/` |
| C6 | Secrets masked in the mirror; personal data not masked | Mask personal data too |

## 13. Phase 1 as built (2026-11-05)

Code: `src-tauri/src/agent_files/{mod.rs, mirror.rs, status.rs}`,
`src-tauri/src/agent_files_commands.rs`, `mcp_scope::{MirrorScope, set_mirror}`,
`jodd-mcp sync-agent-files` and `brief`'s mirror line (`jodd-mcp/src/main.rs`),
`src/lib/agentFiles.ts` and `src/lib/components/AgentFilesSettings.svelte`
(Settings › AI, desktop only).

Where it differs from §4–§5, and why:

1. **Front matter is the mirror's own struct, not `render_concept`.** A
   backup's `JoddMeta` requires restore fields (`body`, `content_sha256`,
   `sync_state`) that §5.2 drops. The shape (`type`, `title`, `tags`,
   `sources`, then `jodd:`) is the same. `trust` is written as
   `provenance::Tier` serialises: `human_written`, `human_reviewed`,
   `unreviewed`, `not_recorded`.
2. **A folder that already holds files and is not a Jodd mirror is refused** —
   by Settings and by every run (`mirror::check_root`). The mirror writes
   `README.md` and `index.md` at its root and must never overwrite the user's
   files. Choose… therefore puts a `Jodd for agents` folder inside the folder
   picked.
3. **The hold-back rule counts only notes that vanished from the cache.**
   Hiding a folder, renaming a note or turning an account off are intended
   removals and are applied at once: hiding is a privacy change and must not
   wait for "Apply anyway".
4. **Masking runs on the text with Markdown escapes removed**, and the spans
   are mapped back. The encoder writes `api\_key`, which the detector does not
   read as `api_key`. A test found this; without it, a secret next to an
   underscore reached the disk unmasked.
5. **Turning the mirror off, or moving it, removes the old copy** — only the
   files in its manifest, then its bookkeeping. It is unencrypted, so it does
   not outlive the setting.
6. **`agent_files` lives inside `mcp_write_scope.json`**, so a hand-edit that
   breaks it makes the whole file unparseable, and MCP reads fail closed too.
   That is the existing rule for any key in that file; the app only writes it
   through `edit_at`.
7. An attachment is fetched again only when its path or content id changes;
   one over 50 MB is read and skipped on every run (counted in the status).
8. With no mirror there is no status file; `brief` adds its line only when the
   mirror is on and its last run did not fail.

Verified here: 15 unit tests (`agent_files::*`, `agent_files_commands::*`),
`mcp_scope` round trip, three `jodd-mcp` tests (stdout stays empty, the brief
line, argument parsing), 5 vitest cases (`agentFiles.test.ts`); the full
`cargo test --workspace --locked` passes except four live-sshd tests that fail
identically on unchanged `main` in this sandbox. The CLI was smoke-tested with
no database (exit 0, empty stdout).

**Not verified — the live pass (§9) is owed:** a real encrypted database and
keychain (`jodd-mcp --self-test` first on macOS), Claude Code reading the
mirror through the hook and `additionalDirectories`, read-only files on macOS
and Windows, and the run time for an ~800-note account (§5.5's cheaper
comparison waits on that number).

## 14. Phase 2 as built (2026-11-05)

Code: `src-tauri/src/agent_files/skills.rs`, `mcp_scope::{SkillScope,
set_skill, remove_skill, check_skill_name, ScopeFile::agent_writable}`,
`provenance::publishable_as_instructions`, the skill commands in
`agent_files_commands.rs`, `src/lib/agentSkills.ts`,
`src/lib/components/{SkillDialog,AgentSkillsSettings}.svelte`, and the folder
menu item in `Sidebar.svelte`.

Where it differs from §6–§7, and why:

1. **Reference files are `# Title` plus the note's Markdown, with no front
   matter.** They are instructions Claude Code reads, not records; §5.2's
   `jodd:` block would only cost context.
2. **Skills run with the mirror on or off.** Their lock and status sit beside
   `mcp_write_scope.json` (`agent-skills.lock`, `agent-skills-status.json`);
   the same hook (`jodd-mcp sync-agent-files`) runs both parts. `sync()` now
   returns both results (`SyncOutcome`).
3. **The agent-writable rule is checked at every run as well as on save**, so
   a hand-edited allowlist that reaches a skill folder unpublishes it. Enabling
   the agent workspace is refused while a skill sits under `Notes/__Agent__`,
   and the root `Notes` can never be a skill (it contains the workspace).
4. **A note holding a secret keeps its previously published version**, like a
   note waiting for review, and is listed as a problem. A note never published
   stays out.
5. **C3 as decided:** an edit whose only unreviewed content is `remote` is
   published; "Only publish reviewed notes" holds those back too, per skill.
6. **Cover note:** the title equal to the folder's name, ignoring case and
   Unicode normalisation, else the only note directly in the folder.
   **Description:** the first paragraph after the title — consecutive lines
   join, a blank line ends it.
7. **Stop publishing removes the skill folder at once**, not at the next sync:
   Claude Code should not keep following instructions the user withdrew.
8. Dot files in the assets folder are skipped. A run that creates
   `~/.claude/skills` says to run `/reload-skills` in an open session.
9. The folder menu item is desktop only and the one menu item the phone sheet
   does not have; `sidebarPhoneSheets.test.ts` pins that.

Verified here: 10 `skills::` tests (cover and references, front matter that a
`---` in the description cannot break, `` !` `` defused in every file, an agent
edit held until Mark reviewed, an iPhone edit published unless "only
reviewed", agent-writable / hidden / unconfigured skills removed, a hidden
subfolder and a secret left out, a foreign `jodd-*` folder untouched,
attachments and the assets folder in `assets/`); `mcp_scope` name and overlap
rules; the provenance trust table; the save → publish → withdraw command path;
4 vitest cases. Full `cargo test --workspace` passes except the four live-sshd
tests that also fail on `main` here, and one shell-timing test that failed
once under load and passed three runs alone.

**Not verified — the live pass is owed:** Claude Code listing and using a
`jodd-*` skill, picking up an edit mid-session, reading a binary asset by path
(§3 notes this is undocumented), and Windows paths.

## 15. Phase 3 as built (2026-11-05)

Code: `src-tauri/src/agent_files/live.rs` (what to watch, and when to run),
`agent_files_commands::spawn_live_sync` (the loop), `Db::change_counter`, the
`spawn_live_sync` call in `.setup()`, and the listener in both Settings
sections (`onAgentFilesSynced`, `src/lib/agentFiles.ts`).

**What it watches.** Not note edits one by one: every 10 s it reads a pair
SQLite already keeps — this connection's `total_changes()` and
`PRAGMA data_version`, which moves when another connection (`jodd-mcp`)
commits — plus the scope file's modified time and which accounts are active.
No table is read until one of those moved, and nothing at all happens while
nothing is configured (no mirror, no skill, no skill Jodd published before).

**When it runs.** Once things have been quiet for 30 s, or 2 minutes after the
first change if they never go quiet: the sync worker writes on every poll, so
"quiet" alone could wait forever. The first look after launch counts as a
change, so the files catch up shortly after the app opens. A run refused
because the hook or Sync now holds the lock is retried, not counted.

**Where it differs from §10 Phase 3, and why:** the design said "30 s after a
local edit or a pull that changed a note". A change counter is coarser — any
committed write counts, so a run can find nothing to write — but it needs no
hook in each of `db.rs`'s write paths, which a later write path could miss.
The engine is incremental, so a run with nothing to write costs one read of
the included accounts and no file writes.

**Route to the screen (gotcha #6), traced:** the loop runs `agent_files::sync`
→ writes `status.json` / `agent-skills-status.json` → emits
`agent-files-synced` → `AgentFilesSettings` and `AgentSkillsSettings`, which
listen while mounted, call their status command → the state they render
changes. `agentFilesLiveSync.test.ts` mounts both, fires the event, and checks
the repaint; `the_frontend_listens_for_this_event` pins the name on both
sides; `the_live_loop_emits_after_each_run_and_skips_a_locked_cache` and
`a_locked_launch_starts_neither_…` pin the order in the loop and in `.setup()`
(after the cache-lock return, gotcha #44). Desktop only.

**Not verified — the live pass is owed:** an edit on the iPhone reaching a
running Claude Code session's skill without a new session; CPU and disk cost of
the run that a busy sync worker triggers every 2 minutes on the ~800-note
account (if it is too high, narrow the counter to the `notes` and
`note_events` tables).

## 16. Phase 4 — start a project from the knowledge base

The friend's workflow in §1: a short brief, and the agent pulls the method,
the templates, the past artifacts and the domain knowledge out of the vault by
itself. Phases 1–2 put the knowledge where Claude Code can reach it; Phase 4
gives it one call that gathers what a new project needs, and a way to say
which skills a project uses. Designed 2026-11-05.

### 16.1 Decisions

| # | Question | Decision |
|---|---|---|
| D13 | Who turns the brief into search words | **The calling agent.** It is a language model: `start_project` takes the brief **and** `keywords` the agent writes in every language the notes may use (ระบบขายหน้าร้าน, POS, point of sale…). Jodd ranks; it does not guess meaning |
| D14 | How notes are ranked | By how many **different** keywords a note matches (title hits weigh more), then by recency. A note matching five words beats one matching one word five times |
| D15 | What else the pack carries | The project page from the agent workspace, the skills the project uses, the user's preferences (`About me`), notes one `[[link]]` away from the best matches, and each note's file in the mirror when the mirror holds it |
| D16 | How a project names its skills | A sixth line kind, **`[skill]`**, on the project page: `- [skill] jodd-brand-bbmedia — slides and video (2026-11-05 · claude-code)`. Written by `remember(kind: skill, project: …)` or typed by the user in Notes. The closed kind enum grows on purpose (spec 2026-10-06 §5.2) |
| D17 | Scope | Exactly the MCP read rule: hidden folders and inactive accounts never appear, and the workspace's own pages are not listed as notes (they arrive as the project page and preferences) |

### 16.2 `start_project`

Read-only MCP tool. Input: `brief` (text), `keywords` (1–30), optional
`project` (defaults to the one roots or `--cwd` detect, as `remember` does),
optional `limit` (default 20, at most 40). Output:

```json
{ "project": { "title": "Jodd", "text": "…the project page…" },
  "project_skills": ["jodd-brand-bbmedia"],
  "preferences": ["- [preference] Ask before force-pushing …"],
  "notes": [ { "account_id": "…", "uuid": "…", "title": "Stock costing", "folder": "Notes/ERP",
               "summary": "FIFO vs moving average …", "matched": ["fifo", "costing"],
               "trust": "human_written", "file": "/Users/k/Jodd for agents/…/Stock costing.md" } ],
  "related": [ { "...same shape...": "notes one [[link]] away from the matches" } ],
  "next": "Read the notes you will rely on (read_note, or the file), and follow project_skills." }
```

`remember(kind: skill)` refuses a line that does not start with a valid
`jodd-<name>` and refuses a skill line without a project: a skill used by
"everything" belongs in the skill's own description.

`brief`'s project section already prints the page's lines; a page with
`[skill]` lines gets one more sentence: "Lines marked [skill] name the Claude
Code skills this project uses — use them for that work."

### 16.3 As built (2026-11-05)

Code: `agent_memory::pack` (`project_pack`), `Kind::Skill` and
`agent_memory::skill_names`, the brief's skill hint, the `start_project` MCP
tool and `KindParam::Skill` in `jodd-mcp`, `agent_files::mirror::note_files`,
and one help line in Settings › Skills.

- **Ranking:** score = 3 × distinct keywords matched + title hits, then the
  most recently changed. Each keyword runs through `Db::search_notes` (FTS,
  Thai-aware trigram) — a widened term counts toward the keyword it came from.
- **Related:** outgoing links and backlinks of the 10 best matches, at most
  10 notes, none already in `notes`.
- **The workspace account must be active and its root not hidden** for the
  project page and preferences to come back; otherwise both are empty.
- `remember(kind: skill)` refuses an area, a missing project, and a line
  whose first word is not `jodd-<valid name>`.
- The server instructions now name `start_project` and `find_terms`, and a
  test holds them to their 2 000-character budget (the constant existed but
  nothing checked it).

Verified here: 6 `pack::` tests (ranking by distinct keywords, hidden /
inactive / workspace exclusions, related by link, widening by a folder name,
project page + skills + preferences + the `remember(skill)` refusals, the
mirror file path, keyword bounds), the brief's skill hint, `jodd-mcp`'s
read-tool classification with both new tools. **Not verified:** Claude Code
calling `start_project` on the owner's knowledge base (§17.3's measurement).

## 17. Phase 5 — finding notes by meaning

The owner's earlier concern: a brief says "ระบบขายหน้าร้าน" and the note says
"POS", and a word search misses it. Designed 2026-11-05.

### 17.1 Decision: no embeddings now

Three ways to search by meaning were weighed:

| Way | Why not now |
|---|---|
| Embeddings from the AI provider | Every note would be sent out to be embedded — the opposite of the outbound-privacy default (spec 2026-10-08), and not every provider has an embedding endpoint |
| Embeddings on the device | A model download of 100 MB or more and a new native runtime, with Thai quality unmeasured; nothing here can measure it |
| **The agent translates, Jodd shows it the knowledge base's own words** | Chosen. The caller is a language model already; what it lacks is the vocabulary the notes actually use |

So Phase 5 gives the agent that vocabulary, and lets `start_project` use it.

### 17.2 `find_terms`

Read-only MCP tool. Input: `words` (1–20). For each word, up to 8 terms the
knowledge base itself uses — **tags, folder names and note titles** of visible
notes — that are close to it, with what kind of term each is and how many
notes carry it. Closeness is character-trigram similarity after case and
Unicode folding, plus containment either way, so it works on Thai (no word
segmentation needed) and on spelling variants (`point-of-sale`, `Point of
Sale`). Hidden folders, their notes' titles and tags, and inactive accounts
never appear.

`start_project` widens each keyword with its close **tags and folder names**
(similarity ≥ 0.5, at most 3 per keyword) before it ranks; a note found that
way says so in `matched` (`"pos→tag:point-of-sale"`).

### 17.3 What would reopen embeddings

The live pass runs `start_project` on the owner's knowledge base with briefs
whose relevant notes are known. If those notes are missed while a person can
see the words that should have found them, that is a vocabulary gap this phase
should close; if they are missed with no shared words at all, that is the case
for embeddings, and the measurement says how often it happens.

### 17.4 As built (2026-11-05)

Code: `agent_memory::vocab` (`vocabulary`, `close_terms`, `similarity`),
`Db::note_titles` and `Db::note_tag_pairs` (no body is read), the
`find_terms` MCP tool, and the widening step in `pack::queries`.

- **Similarity:** NFKC + lower-case folding; equal after removing `-`, `_`,
  spaces and `/` scores 0.95; containment (the shorter at least 2 characters)
  scores 0.5 + 0.5 × coverage; otherwise trigram Jaccard.
- **Folder terms** are every path segment below the root, so "Retail" is found
  inside `Notes/Clients/Retail`.
- `find_terms` returns up to 8 terms per word at similarity ≥ 0.3;
  `start_project` widens with tags and folders at ≥ 0.5, at most 3 per keyword.

Verified here: 2 `vocab::` tests (similarity on case, separators, Thai
containment and typos; the vocabulary holds only visible titles, folder
segments and tags, never the workspace or a hidden folder) and the widening
test in `pack::`. **Not verified:** recall on the owner's knowledge base —
§17.3 says what that measurement decides.
