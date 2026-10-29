# Backup and restore — design

Date: 2026-10-23 · Status: proposed, builder-checked; the one owner question
(iCloud sources, §11) answered 2026-10-24. The owner skipped brainstorming
questions and asked for a design; every decision below not quoted from the
request or §11 is the designer's call and is marked **[call]**.

## 1. What this is for

The owner asked for four things, verbatim in substance:

1. Back up from one account and restore into **another** account.
2. Keep **multiple versions** of a backup and allow **selective** restore.
3. Use the **latest OKF** (Open Knowledge Format) as the canonical format.
4. Backup media: **local filesystem** and **remote SSH**.

Why it matters for Jodd specifically **[call — inferred, not stated]**:

- Jodd is local-first, but every backend is a *replica the owner does not
  fully control* — Gmail dedupe, an iCloud merge 15 minutes after a write
  (BACKEND-ICLOUD), a mis-set label (gotcha #9). Today the only copy of a
  not-yet-pushed note or attachment is the SQLite cache (`db.rs:1173` says so
  in a comment). A backup is the first copy that lives outside both.
- Cross-account move (roadmap #6) has a UI and no backend. Restoring a backup
  into another account is the same operation done carefully, and gives the
  owner a way to migrate between backends (e.g. Gmail-backed Notes → iCloud,
  or iCloud → an SSH vault) that does not exist today.

Success looks like:

- **Back up now** on any account writes a new version to a local folder or an
  SSH server in seconds, without blocking editing or sync.
- The owner can open a destination, pick an account's version from a list,
  tick some notes or folders, pick **any** target account, and restore.
- Running the same restore twice does **not** create duplicates.
- Every piece of a note that the target cannot hold (attachment, folder, pin,
  tag form) is **reported**, never silently dropped.
- A version is a valid OKF v0.2 bundle that another OKF tool (e.g. the
  reference `visualize` command) can read.

## 2. Goals, non-goals, anti-goals

**Goals**

- Per-account backups to one or more destinations (local folder, SSH).
- Many versions per account per destination, with a retention rule.
- Selective restore (notes and/or folders) into the same or any other account,
  any backend.
- Manual and scheduled (while the app is running) backups.
- Restore works on a fresh install pointed at an existing destination
  (disaster recovery), not only on the device that made the backup.

**Non-goals (v1)**

- Backing up app settings, LLM/AI config, Ask history, keychain secrets,
  agent-CLI config. Notes, folders, pins, tags, attachments, provenance only.
- Background backups while the app is closed (no WorkManager / launchd job).
- Storage dedupe across versions (each version is self-contained).
- Cloud object stores (S3, Drive, Dropbox) — a later destination type. A local
  folder that a sync client mirrors already covers them.
- Round-tripping unknown frontmatter keys from a *foreign* OKF bundle back out
  (roadmap #10's SQLite storage problem). Import of foreign bundles is one-way.
- Rewriting `[[wikilinks]]` into OKF markdown links in the bundle body (that is
  roadmap #6 "OKF Export"; a backup must stay byte-faithful, §4.3).
- Encryption in the first milestone (it is milestone 3, §9) — but the layout
  is designed so it fits without a format break.

**Anti-goals — it fails even if it "works" when:**

- A restore run twice, or retried after a crash, duplicates notes. (Jodd has
  shipped duplicate bugs repeatedly — gotchas #39, #40, the 12-cause audit.)
- A restore silently loses content: an attachment, a folder, a pin, a body
  formatting the target cannot render. Loss must be listed before and after.
- A backup or restore blocks the editor or a navigation read on the network
  (local-first doctrine).
- A restore pushes a note in a shape the target backend refuses forever,
  without telling the owner (gotcha #14's push-blocked state is the fallback,
  not the plan).
- A backup is taken from a torn state (half an edit, a note without its
  attachments).

## 3. The shape of it

```
                Back up                                   Restore
 SQLite cache ─────────▶ OKF bundle (.zip) ──────────▶ SQLite cache (target acct)
 (one account,           + manifest (.json)           (dirty rows, Actor restore)
  read snapshot)         at a Destination                    │
                         (local dir | SSH dir)               ▼
                                                     sync worker pushes as usual
```

- **Backup reads SQLite** — what the owner sees in Jodd, including unpushed
  edits and unpushed attachments, is what is backed up. A backup refuses to
  run before the account's first complete listing (§6.2's marker)
  **[call]**. That is also what makes it whole: `index_account` lists ids
  only and writes no note rows; a row reaches `notes` through `reconcile_one`
  with its body (plan-time check, 2026-10-24), so after one complete
  `list_notes` every note is in the cache with its body. No remote read is
  needed.
- **Restore writes only SQLite** — new rows go in `dirty` (folders
  `dirty_new`) through the same functions a new note uses; the existing sync
  worker pushes them. Restore never talks to the target backend itself.
- **Destinations are independent of accounts.** One destination can hold
  backups of many accounts; a fresh install can browse it.

## 4. The format — an OKF v0.2 bundle

OKF v0.2 (2026-07-24, timestamps tightened 2026-08-20) is still the current
version on 2026-10-23 (`GoogleCloudPlatform/open-knowledge-format/SPEC.md`,
checked). Its conformance rules that matter here: every non-reserved `.md` has
YAML frontmatter with a non-empty `type`; `index.md` / `log.md` are reserved;
non-`.md` files are unconstrained; a bundle may be shipped as a zip; consumers
must tolerate unknown keys.

### 4.1 One version = two files

```
<destination root>/jodd-backups/<account-slug>/
    2026-10-23T09-15-00Z.jodd-backup.zip     # the OKF bundle
    2026-10-23T09-15-00Z.jodd-backup.json    # the manifest (small, read for listing)
```

- `account-slug` = the account id (`gmail:kaiwan@…`) made filesystem-safe
  **[call]**; the manifest also carries the exact id.
- A version **exists only when its `.json` exists.** The zip is written first
  (to a temp name, then renamed), the manifest last. A listing ignores a zip
  without a manifest; a crash mid-backup leaves an orphan that the next backup
  run deletes. This is what makes backups atomic on both media.
- Why a zip, not a directory per version **[call]**: one file per version
  makes the SSH upload a single (chunked) stream instead of hundreds of
  round trips, makes atomic publish a single rename, makes retention a file
  delete, and opens with a double-click on every OS. OKF §3 allows it.

### 4.2 Inside the zip

```
index.md                      # okf_version: "0.2" + listing (OKF §8)
log.md                        # one dated entry: who/what made this backup
.jodd/bundle.json             # bundle-level facts, see below
Notes/                        # folder tree mirrors the account's labels
  index.md
  Work/
    index.md
    Meeting with Ann.md
    Meeting with Ann (2).md   # title clash in one folder → suffix
references/attachments/<note-uuid>/<content-id>-<filename>
```

- Directory names come from the folder path; file names from the title,
  sanitised (path separators, control chars, Windows-reserved names and
  characters), Thai and other scripts kept as UTF-8 (zip UTF-8 flag), length
  capped. Empty title → `Untitled`. A title that would be `index.md`/`log.md`
  gets a suffix. **The restore never trusts these paths** — it reads folder
  and title from frontmatter — so sanitising may be lossy.
- `.jodd/bundle.json`: format version (`jodd_backup: 1`), source account id
  and backend, app version, created-at, **the complete folder list** (so empty
  folders survive), counts, and the list of notes skipped and why (e.g. an
  iCloud note Jodd cannot read).

### 4.3 One note = one concept document

```yaml
---
type: Note
title: Meeting with Ann
tags: [project-x, followup]
generated: { by: "human:owner", at: "2026-10-22T14:03:11+07:00" }
verified: [{ by: "human:owner", at: "2026-10-22T15:00:00+07:00" }]
sources:
  - { id: s1, resource: "https://example.com/article" }
jodd:
  uuid: 3F2A…                     # identity in the source account
  account: "gmail:kaiwan@bbmedia.co.th"
  backend: gmail
  folder: "Notes/Work"
  created: "2026-10-01T08:00:00+07:00"   # X-Mail-Created-Date
  modified: "2026-10-22T14:03:11+07:00"
  pinned: true
  body: markdown                  # or: html
  content_sha256: "…"             # sha256 of the body HTML (never of the Markdown), see §6.3
  sync_state: dirty               # informational
  attachments:
    - { content_id: "abc@x", filename: "photo.jpg", mime: "image/jpeg",
        path: "/references/attachments/3F2A…/abc@x-photo.jpg",
        x_apple_part_url: "…" }
  extra: [ … ]                    # SSH frontmatter lines Jodd does not model
---
<body>
```

- **Everything Jodd-specific lives under one `jodd:` key** **[call]**, so the
  top level stays plain OKF and other tools see a tidy concept.
- `type` is always `Note` in v1 **[call]** (roadmap item #3 per-note types was
  dropped; OKF does not register types).
- `generated` / `verified` come from `provenance.rs` (`Trust`, `note_events`,
  `Actor::as_okf()`), which already speaks OKF actor strings. A note with no
  recorded provenance omits them (OKF: unverified). The full event list is
  also kept as `jodd.history: [{kind, by, at}]`, so a restore copies it
  verbatim (as `copy_events_conn` does for a duplicate) and the tier derives
  identically **[call, plan-time]**.
- `sources` come from `cites` edges. No footnotes are added to the body.
- `tags` are what `note_tags` holds — **informational**: in Jodd tags are
  derived from the body, so the body is the source of truth on restore (§6.4).

**The body is byte-faithful** — this is the core lossless rule:

- If `ssh/mdconv.rs::html_to_md` returns `Some(md)` **and**
  `md_to_html(md)` is **byte-equal** to the HTML, write Markdown and
  `body: markdown`. (`html_to_md`'s own check is "exact in the projection" —
  parsed formatting, not bytes, `mdconv.rs:355` — so the backup adds the byte
  check itself.)
- Otherwise write **readable text** as the `.md` body (the SSH vault's
  Markdown when its styles allow, else the note's lines), set
  `body: html-file`, and put the exact HTML in its own file named by
  `jodd.html` (`/references/bodies/<uuid>.html`); restore reads that file.
  *Changed 2026-10-24 after the live pass:* the first builds wrote the HTML
  itself as the body (`body: html`), and three notes in four of a real Gmail
  account came out as raw HTML in their `.md`. Those bundles are still read.
- The body stored is **the note as the editor would hand it to `save_note`**
  (title separate, body as edited), so restore can go through the same save
  path and each backend's title handling (gotchas #11, #17, #27) runs exactly
  as on any new note. *Builder: confirm whether `notes.body_html` already is
  that form for every backend or needs `strip_leading_title`-style
  normalisation, and pin it with a test per backend.*

**YAML needs a real parser** — `ssh/frontmatter.rs` is deliberately not one
and cannot read nested keys. The crate choice is the builder's (`serde_yaml`
is archived; pick a maintained one), with tests for Thai titles, titles that
look like YAML (`yes`, `null`, `- x`, `: `), and multi-line strings.

### 4.4 The manifest (`.jodd-backup.json`)

Readable without downloading the zip — this is what makes listing versions and
"versions of this note" cheap over SSH:

- format version, account id/backend, created-at, app version, device name,
  trigger (`manual` | `scheduled` | `pre-restore`), `keep` flag, zip byte size
  and sha256;
- per note: `uuid`, `title`, `folder`, `modified`, `content_sha256`,
  attachment count;
- folder list.

## 5. Destinations

A destination is a saved record (app config, not an account) **[call]**:
`{ id, name, kind: local | ssh, path | {target, port, root_dir, key} }`.

**Local folder**

- Desktop: the existing directory picker (`@tauri-apps/plugin-dialog`, as
  `AccountAddHost.svelte` uses). A folder inside Dropbox/iCloud Drive/OneDrive
  works and is the owner's off-site path.
- Android: reuse `android_storage::pick_local_folder` and the All-files-access
  flow; same limits as the Local Folder backend (primary storage only, not
  `Android/`). Losing access is transient (`ACCESS_LOST`), shown as an error
  on the destination, not a crash.
- Writes: temp file in the same directory + rename (`localfs` `write_atomic`
  pattern).

**SSH**

- Reuse the SSH backend's session layer standalone — it already supports a
  non-account key (`BROWSE_KEY = "ssh-browse"` in `ssh_commands.rs`):
  `session_for("backup:<destination-id>", target, &SshOptions)`.
- Setup offers the two existing paths: **Managed** (reuse
  `setup_managed_core`: Jodd's own key, password used once, never stored) and
  **Advanced** (the owner's `~/.ssh/config` / agent; desktop only). Also offer
  "use the same server as SSH account X", which copies that account's target,
  port and key reference **[call]**.
- Android can only use Managed keys (`russh_session.rs:358`) — say so in the
  setup UI, as the SSH account setup already does.
- **Upload must be chunked.** `scripts.rs` embeds content as a base64 heredoc
  inside one `exec` with a 60 s timeout; a multi-megabyte zip will not fit
  one request. Append chunks to `.jodd/tmp/<name>.partial`, verify the sha256
  remotely (`PROBE` already detects the sha flavour), then `mv` into place,
  then write the manifest. Chunk size is the builder's, measured on the
  slowest test link and on Android's one-channel-per-request russh.
- Download for restore: same, chunked, into the app cache dir, sha256-checked
  against the manifest before use.

## 6. Restore

### 6.1 Flow

1. **Pick a version**: Destination → account (any slug found there, including
   accounts not on this device) → version list (date, trigger, note count,
   size, `keep`).
2. **Pick what**: a folder/note tree with checkboxes and search, from the
   manifest. Ticking a folder ticks everything under it.
3. **Pick where**: target account (any account whose `Capabilities.writes.notes`
   is true) and placement:
   - *Original folders* (default) — recreate the folder paths, or
   - *Under a new folder* `Restored <source> <date>/…` **[call]** — keeps a
     cross-account merge from mixing into existing folders.
4. **Preview** (no writes yet): counts of create / skip / replace, and the
   **loss list** (§6.4) per note. The owner confirms.
5. **Apply**: download + verify the zip, then write SQLite. Rows appear in the
   UI at once (local-first); pushing proceeds through the normal worker and its
   usual pending/blocked indicators.

### 6.2 Identity and duplicates

- **A restored note always gets a new uuid** from `backend::mint_uuid_for(kind)`
  **[call]**, except in *Replace* (below). Reusing the source uuid across
  accounts is technically allowed by the PK `(account_id, uuid)`, but two
  accounts carrying the same Apple identity on one Apple device is untested
  ground; new identity is the safe default.
- **"Already restored" is decided by two checks, either one is enough**
  **[call]**:
  1. New table `restore_map(source_account, source_uuid, target_account,
     target_uuid, content_sha256, backup_id, restored_at)` (a migration),
     looked up by `(source_account, source_uuid, target_account)` —
     **not** by version, so restoring an unchanged note from Monday's and then
     Tuesday's version is still one note. If the row exists, the target note
     still exists (resolve through `note_uuid_aliases`, gotcha #16, since
     Microsoft rekeys on first push), and its `content_sha256` matches the
     version being restored → skip. A different hash means the note changed
     between versions → treated like "same account, note still present"
     below (Keep both / Replace / Skip), against the earlier restored copy.
  2. **Content match in the target**: a note in the target account with the
     same title and the same body sha256 → skip. This is what catches a
     re-run on another device or after a reinstall, where the local
     `restore_map` rows do not exist; it needs no marker carried through the
     backends.
  The map row and the note row are written in **one transaction**, like
  `insert_divergent_copy`, so a crash cannot leave one without the other.
- **Restore requires a complete target.** A restore into an account is only
  offered once that account has finished one complete listing in this
  install; until then the target shows "Waiting for the first full sync".
  Without it, a fresh-install restore cannot see the originals and the
  following full refresh brings them in beside the restored copies. This
  needs a new per-account marker set when a full pull completes *and* was
  not `incomplete` (gotcha #43) **[call]**.
- **Same account, note still present** (its source uuid resolves in the
  target): the owner picks per restore, default **Keep both** **[call]**:
  - *Keep both* — create a copy titled `<title> (restored <date>)`.
  - *Replace* — a normal local edit of the existing note
    (`apply_local_edit`), so `check_push_base` (gotcha #40) still protects the
    remote. **Before any Replace, Jodd writes a `pre-restore` backup version of
    exactly the notes about to be replaced** to the same destination, so a
    Replace is itself undoable.
  - *Skip*.
- Notes that were deleted since the backup simply do not resolve, so they are
  created new.

### 6.3 Content

- Body: `body: html` → verbatim; `body: markdown` → `mdconv::md_to_html`.
  Verify the result's sha256 equals `content_sha256` and refuse the note (and
  report it) if not — a corrupted or hand-edited bundle must not restore
  silently wrong text.
- Created date: pass the original as `x_mail_created_date` so Apple's sort
  order survives.
- Links between restored notes: a wikilink can carry an 8-hex id suffix of the
  target's uuid (`src/lib/links/target.ts` `idSuffix`). When both ends are in
  the restore set, rewrite the suffix to the new uuid's **[call]**; links to
  notes outside the set are left alone (they resolve by title as before).
- Provenance: the restored note must keep its trust tier — a human-written note
  stays human-written. Model on `insert_duplicate` (R12 in the provenance spec):
  copy `jodd.history` verbatim with NULL versions and add **no** event of the
  restore's own — any non-human content event would flip the tier; the
  `restore_map` row is the record that a restore happened. A *Replace* is an
  edit by `Actor::Process("restore")` (restoring is not authorship), and a
  note from a foreign bundle gets one `create` by the same actor. Pinned by a
  tier test.

### 6.4 What a target cannot hold — the loss list

Computed at preview from `Capabilities::for_backend(target)` and
`TagModel::for_backend(target)`, shown before confirm and saved as a restore
report afterwards:

| Piece | Gmail / LocalFs | SSH | Microsoft | iCloud |
|---|---|---|---|---|
| Body | ✓ | ✓ | ✓ | ✓, but per-note refusals may push-block some (gotcha #24) |
| Folders | ✓ | ✓ | ✗ → flattened to the root (permanent, gotcha #12) | ✓ |
| Attachments | ✓ (insert rows into `attachments`, same content-id) | ✗ — a `cid:` body is refused forever (`ssh/write.rs:61`) | ✗ (Apple refuses on Exchange) | ✗ (ignored) |
| Pin | ✓ (`set_pin`) | ✓ | ✓ | ✗ (Apple's pin wins, sidecars false) |
| Tags | `#tag` text in body ✓ | ✓ | ✓ | `#tag` text is not a tag on iCloud (gotcha #38) → append a `Keywords:` line, as the LLM paths do |

- For a target without attachments, replace each `<object data="cid:…">` /
  `<img src="cid:…">` with a short visible placeholder naming the file
  **[call]**, so the body pushes instead of being blocked, and the owner sees
  where something was.
- iCloud writability: if `compose`'s refusal check can be run on a new body
  before push, run it at preview and list the notes that will be held back.
  *Builder: establish whether a newly created note can be refused at all.*

### 6.5 Foreign OKF bundles

Because the reader takes folder and title from frontmatter and tolerates
missing `jodd:` keys, a bundle made by another tool restores too: `title` (or
the file name), `tags` → `#tag` line, body via `md_to_html`, folder from the
directory path, new uuid, no provenance. This is roadmap #10 (OKF Import)
almost for free; v1 only needs it **not to be impossible** and tested once
against the reference `bundles/acme_retail`. Re-export does not preserve their
unknown keys (non-goal).

## 7. Backup

- **Consistent read**: one SQLite read transaction for the account's notes,
  folders, attachments, tags, edges and provenance, so a concurrent edit lands
  wholly before or wholly after. Rows in `deleted_pending` are excluded;
  `dirty`, `conflict` and push-blocked notes are included.
- **Unchanged → no new version** for scheduled runs **[call]**: if every
  note's `content_sha256`, pin, folder and the folder list equal the latest
  version's manifest, record "checked, unchanged" and write nothing. Manual
  "Back up now" always writes.
- Build the zip in the app cache dir, stream it to the destination, publish.
- Runs off the command thread; must not hold the per-account sync lane for the
  upload (`sync_schedule` admission is for remote sync work). Progress and
  completion reach the UI by an emitted event — **trace it to the pixel**
  (gotcha #6).
- An account in `pending_removal` (roadmap #0d) can still be backed up — that
  is arguably the most useful moment **[call]**.

### 7.1 Schedule and retention

- Per (account, destination): **Off / Daily / Weekly** **[call]**, default
  Off. Checked on a slow timer while the app runs; a missed run fires on next
  launch. Status line: "Last backup 2 h ago to <destination>" or the last
  error.
- Retention per (account, destination): keep the newest **N** versions,
  default 30 **[call]**; versions marked **Keep** and `pre-restore` versions
  are never auto-deleted. Manual delete of a version asks first.

## 8. UI

- **Settings → Backup** (new `SettingsSection` `'backup'`,
  `settings/BackupPage.svelte`, phone = its own page like the others):
  destinations (add / test / remove), per-account schedule + retention, and
  **Browse backups…** which opens the restore flow (§6.1) as a dialog.
- **Settings → Accounts → <account>**: "Back up now" and last-backup status.
- **Note context menu → "Earlier versions…"** (milestone 2): reads manifests
  only, lists the versions where this note's `content_sha256` changed, opens a
  read-only preview of one (downloads that zip), and offers restore of that
  single note with the §6.2 choices.
- Long work (download, restore apply) shows progress and is cancellable before
  the SQLite write step; the SQLite write itself is one short transaction per
  batch.

## 9. Milestones

1. **M1 — Back up and restore, local folder.** Format (§4), local
   destination, manual backup, version list, selective restore into any
   account with the loss list, `restore_map`, Replace's pre-restore backup,
   the complete-listing marker (§6.2), body hydration (§3), and the iCloud
   partial flag (§11).
   Tests: round-trip every backend's body shapes through the bundle and back
   to identical `save_note` input; restore twice → no duplicates; crash
   between zip and manifest → no visible version.
2. **M2 — SSH, schedule, versions of a note.** SSH destination with chunked
   upload/download (desktop + Android Managed), schedule + retention,
   "Earlier versions…", foreign-bundle smoke test against `acme_retail`.
3. **M3 — Encryption.** Optional passphrase per destination (a vetted
   passphrase-based file format, e.g. `age` with scrypt — the builder's
   choice). The zip becomes `….zip.age`; the manifest keeps only counts and
   dates in clear, its note list moves inside the encrypted archive. The
   passphrase is stored in the keychain for scheduled runs (read only by the
   backup job — never from a polled command, gotcha #15) and asked for on
   restore on another device. Until M3, the backup page says plainly that
   backups are not encrypted.

## 10. Left to the builder

- YAML crate, zip crate, chunk size, the settle on `account-slug` encoding.
- Exact `note_events` shape for a restore, as long as the tier rule in §6.3
  holds.
- Whether the version list is cached locally (it must still be correct when
  another device added versions).
- UI layout details within Settings' existing patterns.

## 11. iCloud sources — decided by the owner (2026-10-24): option (a)

For iCloud, `notes.body_html` is Jodd's rendering of Apple's document;
drawings, tables, images and other inline objects Jodd cannot render are not
in SQLite at all, and which notes lose something is computed only at push time
(`compose` refusals — 2026-08-27: ~180 of 773 notes). The owner chose:

**Back up what Jodd shows, and flag each note that loses content.**

- Cache a per-note "partial rendering" flag (with the refusal reason) at pull
  time, from the same check `compose` runs against the remote document — the
  flag must come from that check, not a second heuristic that can disagree.
  Storage shape is the builder's (a column or a side table, via migration).
- The backup writes `jodd.partial: <reason>` in the note's frontmatter and
  lists the note in `.jodd/bundle.json` and the manifest.
- The backup's completion report and the restore preview show these notes as
  **partial**, with the count, before the owner confirms.
- Same-account *Replace* of a partial iCloud note is not offered (it would be
  refused at push, gotcha #24); *Keep both* and *Skip* remain.
- Rejected: (b) storing Apple's raw document (new storage or a remote fetch,
  lossless only back into the same iCloud account); (c) leaving those notes
  out (the owner would lose even the readable part).

## 12. Where it touches existing rules

- Local-first doctrine: restore = SQLite rows + worker; backup = read-only.
- Gotcha #6 (progress/completion events), #14 (push-blocked is the fallback),
  #15 (keychain in M3), #16 (aliases in `restore_map` lookups), #18 (mint per
  backend), #24 (iCloud refusals), #38 (iCloud tags), #39/#40 (no duplicate
  creates; Replace goes through `check_push_base`).
- `Capabilities::for_backend` / `refuse_write` gate the restore target exactly
  as they gate the editor.
- After this ships: update `privacy.html` on Jodd-Homepage if the copy says
  where note data goes (backups go only where the owner points them), and run
  `/graphify --update`.
