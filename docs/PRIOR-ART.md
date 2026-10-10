# Prior art — open-source projects that do what Jodd does

Survey started 2026-08-18. Purpose: find what is worth adopting, and record what
was *checked and rejected* so nobody re-opens it. Findings here are from reading
the actual source, not from READMEs — where a claim is README-only it says so.

The landscape splits by **mechanism**, and mechanism is what decides whether a
project competes with Jodd or merely resembles it:

| Mechanism | Reaches | Projects |
|---|---|---|
| Email backend (IMAP/Gmail, `X-Uniform-Type-Identifier: com.apple.mail-note`) | non-iCloud accounts only | **Jodd**, ImapNotes3, ImapNote2, valinet/IMAPNotes (Thunderbird), findus/apnotes |
| Reverse-engineered CloudKit behind `icloud.com/notes` | every account, iCloud included | `icloud-md` + obsidian-icloud-notes |
| Local SQLite/protobuf on a Mac | needs a Mac; export only, not sync | obsidian-importer, apple-notes-liberator, apple_cloud_notes_parser, … |

Jodd is the only entry in row 1 that is a **cross-platform GUI app** (the rest
are Android-only, a mail-client plugin, or a CLI/TUI), and the only project in
any row with a **Microsoft/Exchange** backend.

---

## ImapNotes3 (niendo1) — same mechanism, opposite title design

Android-only, Java, GPLv3, ~942 commits, actively developed. Source read at
commit tip on 2026-08-18. This is the closest thing to a peer Jodd has on the
email-backend approach, and it is the one that has been exercised against the
widest set of servers (Gmail, iCloud, Yahoo, AOL, posteo.de).

### The one big architectural difference

**ImapNotes3 has no title field.** The editor is handed the whole body
including its first line (`NoteDetailActivity.java:176-180`), and the title is
*derived on save only*, for the list row and the `Subject` header
(`UpdateThread.java:170-172`):

```java
// Use the first line as the tile
String[] tok = Html.fromHtml(noteBody, Html.FROM_HTML_MODE_LEGACY).toString().split("\n", 2);
String title = tok[0];
```

`HtmlNote.GetNoteFromMessage()` — their whole read path — never touches the
title. There is no `strip_leading_title` equivalent because nothing is ever
injected.

**Why this matters to us:** that single choice deletes the entire class of bug
that gotchas #11 and #17 document. No double-title, no idempotency requirement,
no separator problem, no fused title+body. The cost is UX: the user has to know
that line one becomes the note's name.

**Decision: do not adopt.** Jodd's separate title field is a product promise
(the UI title and body editor stay separate), and we have already paid the
implementation cost and pinned it with
`the_round_trip_is_idempotent_across_repeated_edits`. But **record it as the
known escape hatch**: if the inject/strip pair ever becomes unmaintainable on a
third backend, "title lives in the body, UI derives it" is a proven shipping
design, not a hypothetical.

### It independently confirms gotcha #11

Their rule is **title = first _line_**, reached by rendering HTML to text and
splitting on the first `\n` — a completely different implementation from our
DOM-walking `first_line_split` (`mime822.rs:178`), arriving at the same rule.
Four years of running against five providers did not push them toward
"title = first node". Treat gotcha #11 as confirmed by an independent
implementation, not just by our own 2026-08-14 investigation.

### Checked against our code — three things NOT to change

1. **Hashtag extraction.** Their regex (`Utilities.java:65`) guards with
   `(?<=(\s|^))`, i.e. `#` must follow whitespace. Ours (`db::extract_hashtags`,
   `db.rs:3970`) guards with `i == 0 || !is_word(chars[i-1])`, which rejects the
   same `example.com#anchor` case *and* still accepts `(#tag)`. Both strip HTML
   to text first. **We are at parity or better — no change.**

2. **Lists/tables at the top of a note.** `Html.fromHtml(…, FROM_HTML_MODE_LEGACY)`
   does not understand `<table>` and flattens `<ul><li>` to bullets, so a note
   opening with a table or list gets a mangled title in their app. Ours can't hit
   this: `first_block_or_embed` (`mime822.rs:150`) tests against an **allowlist**
   of inline tags (`INLINE_TITLE_TAGS`), so any unknown tag — `<ul>`, `<table>`,
   anything future — correctly ends the title line. **The allowlist direction is
   the reason we're safe here; do not "improve" it into a blocklist.**

3. **Non-RFC 2047 `Subject` headers.** Some servers (they name posteo.de) send
   non-ASCII subjects unencoded, so the raw UTF-8 bytes come back mis-decoded as
   Latin-1 — which destroys Thai titles. They patch it with an
   ISO-8859-1 round-trip (`SyncUtils.java:398-411`). **We already have the
   general form of this fix**: `mime822::try_recover_mis_decoded_utf8`
   (`mime822.rs:31`), which does the Latin-1/CP1252 → UTF-8 recovery with a
   validity check, wired into the Gmail read path (`gmail/wire.rs:389,990`) and
   LocalFs (`localfs/decode.rs:16`). Not needed on Microsoft — Graph returns
   `subject` as a JSON string, never a MIME header.

   **Conditional TODO:** if a generic IMAP vertical is ever added (a real
   possibility given roadmap 0c and the number of providers ImapNotes3 supports),
   `try_recover_mis_decoded_utf8` must be wired into its read path *before* the
   first Thai-titled note round-trips. The bug is silent and the corruption is
   written back on the next save.

### Deliberate divergence worth knowing about

They set `X-Mailer: ImapNotes3` on every note (`HtmlNote.java:80`); Jodd sets no
such header, so a Jodd-written message is byte-shaped like one Apple wrote. That
is the right call for round-trip fidelity, but it means **we cannot tell from a
mailbox which notes we authored** — relevant if we ever need to forensically
scope a data-loss incident (cf. gotcha #17, where the blast radius had to be
found by re-reading bodies). Revisit only if such an incident recurs; adding the
header now changes bytes Apple parses, for no present benefit.

### Useful as a corpus, not as code

Their issue tracker is a free source of real-world note shapes from providers we
have never tested against. Worth mining if we ever widen provider support —
their bug reports are our test fixtures.

---

## icloud-md (coddingtonbear) — different mechanism, and the one that matters

TypeScript, MIT, v0.6.2, ~37k LOC with a test file beside nearly every module.
Source read on 2026-08-18. Ships as a CLI; the Obsidian plugin
`obsidian-icloud-notes` is a thin front-end that shells out to it with `--json`.

**It reaches what Jodd structurally cannot: iCloud-native accounts.** Our
mechanism requires the user to have added a non-iCloud (Gmail / Exchange)
account to Apple Notes. Most Apple Notes users have not. This is not a
competitor to out-feature; it is a different door into the same house.

### How it actually works

- **Transport:** CloudKit's private database web service for the
  `com.apple.notes` container —
  `https://p<N>-ckdatabasews.icloud.com/database/1/com.apple.notes/…` — using
  CloudKit JS request shapes: `records/query`, `records/lookup`,
  `records/modify`, `changes/zone`. Own notes are the `private` database, zone
  `Notes`; notes shared with you are the `shared` database, one zone per
  sharer (`src/cloudkit/databaseClient.ts`, 1032 lines).
- **Incremental sync is a first-class `syncToken` + `moreComing` loop**, with a
  documented fallback: a token the server rejects (zone-level `BAD_REQUEST`
  inside an HTTP 200) triggers a full refetch rather than an abort. They
  established the failure shape by live probing, and note that a merely *old*
  token (15 days) still syncs incrementally.
- **Auth is deliberately NOT reverse-engineered.** `clone` opens a **headed
  Playwright/Chromium window** on Apple's own sign-in pages, lets Apple's
  JavaScript run whatever this month's password/2FA/CAPTCHA flow is, and
  harvests session cookies once `setup.icloud.com/setup/ws/1/{accountLogin,validate}`
  returns a body that is *fully* signed in. The discriminator is
  `hsaChallengeRequired !== true` — a pre-2FA `accountLogin` returns 200 and
  even carries the `ckdatabasews` entry, so checking the status code alone
  closes the window in the user's face at the 2FA screen
  (`src/auth/browserLogin.ts`). A persistent browser profile dir means repeat
  logins usually skip 2FA.
- **Content is protobuf, not HTML.** Fields named `TitleEncrypted` /
  `TextDataEncrypted` are, on accounts *without* Advanced Data Protection,
  merely zlib-compressed — "encrypted" describes Apple's server-side at-rest
  encryption. Decompressed, they are the same internal format as on-device
  `NoteStore.sqlite`: a CRDT document. Their `proto/` directory
  (`crdt.proto` 220, `topotext.proto` 184, `versioned_document.proto` 50 —
  **454 lines total**) is the schema.
- **Writes are guarded three ways** (`src/commands/push.ts:1370-1392`):
  1. **Staleness** — a note whose `recordChangeTag` moved past the local
     baseline is reported as a conflict, never overwritten; the tag is also
     sent so the *server* re-checks (a real optimistic lock).
  2. **Round-trip** — the current remote document must **re-encode
     byte-for-byte** from their parsed model before they will edit it.
     `noteDocumentRoundTrips(raw)` false ⇒ refuse, note stays read-only.
  3. **Verification** — the rebuilt document is decoded *again* and must
     yield exactly the intended content before upload.

### The three hard limits, up front

- **Advanced Data Protection kills it.** With ADP on, `TextDataEncrypted` is
  genuinely end-to-end encrypted and there is no readable content. Any product
  plan built on this must assume a user can switch it on and disappear.
- **Version strings are captured, not derived.** `CKJS_BUILD_VERSION =
  "2310ProjectDev27"` and `DEFAULT_CLIENT_BUILD_NUMBER = "2624Build27"` are
  copied from a real browser session, "may need bumping if Apple ships a new
  web client build". This is a maintenance tax, forever.
- **Attachment upload is impossible by construction** — the iCloud *web*
  Notes editor cannot attach a file, so there is no client behavior to mimic.
  (Familiar shape: same reason our Microsoft vertical can't do attachments.)

### The strategic finding: CloudKit does not hand you HTML

This is the fact that decides everything below. **Jodd's entire content model
— `AppleHtmlDeriver`, FTS, `note_tags`, `edges`, the editor — assumes an HTML
body**, because the email backend delivers exactly that. CloudKit delivers
Apple's native CRDT document instead. icloud-md spends roughly 2.6k lines
(`noteDocument.ts` 1027, `renderNoteMarkdown.ts` 640, `noteFormat.ts` 436,
`parseNoteMarkdown.ts` 478) converting that document ↔ **Markdown**, plus 1.2k
more for tables alone.

An iCloud vertical for Jodd would need document ↔ **HTML**, which neither
project has written. The transport is the cheap part; the content model is the
project.

### Three options, with an honest cost on each

**Option A — shell out to the CLI (what the Obsidian plugin does).**
Cost: near zero. Fatal for the product: requires Node + Playwright + a
Chromium download on the user's machine, and **cannot exist on Android at
all**, which is half of why Jodd exists. Also breaks the single-binary Tauri
distribution. **Verdict: not a product path — but a legitimate research probe.
Run it against a live iCloud account to see the record shapes ourselves before
committing to anything.**

**Option B — port the transport, reuse the schemas, write the content layer.**
- Port `databaseClient.ts` (~1k lines) into a Rust `Transport` impl. It is
  plain HTTP + JSON; this is the *easy* thousand lines.
- **Reuse `proto/*.proto` verbatim (454 lines).** These are the crown jewels
  and they are *data*: `prost` compiles them in Rust with no translation.
  MIT, so attribution is the only obligation.
- **Replace Playwright with Tauri's own webview.** We already ship a webview on
  every platform, including Android. Opening Apple's real sign-in page in a
  Tauri window and harvesting cookies is the same trick without the 200 MB
  Chromium — and it is the one place where **Jodd's architecture is strictly
  better suited than icloud-md's**. Their auth design (own none of the
  idmsa.apple.com surface, depend only on the *result*) is exactly right and
  should be copied as a principle.
- Write document ↔ HTML ourselves. This is the real cost and it is large.
- **Verdict: the only viable product path, and it is a milestone, not a task.**
  Sequence it as M1-read / M2-write the way the Microsoft vertical went, and
  do the Option A probe first.

**Option C — adopt the disciplines without the backend. Do this regardless.**
These are free, apply to the backends we already have, and one of them would
have prevented a shipped data-loss bug:

1. **Byte-for-byte round-trip guard before any write.** Their rule: *reproduce
   the remote's current form exactly from your own model, or refuse to edit
   it.* **Gotcha #17 is precisely the failure this catches** — a fused
   title+body parsed to an empty body, which we then pushed back and destroyed
   real content with. We fixed the parser; we still have no guard that would
   catch the *next* parser bug. A `strip → inject == original` assertion on
   the pull path, with a refuse-and-flag on mismatch, is the cheap version.
   `push_blocked_reason` (migration #17) already gives us somewhere to put the
   refusal and a UI that shows it.
2. **Refuse rather than risk.** They mark whole categories read-only
   (attachments, asset-backed text, table reorders, embeds a splice would
   touch) instead of best-effort writing. We currently have one refusal
   mechanism and it is reactive (the backend said no). A *proactive* refusal —
   "we don't fully understand this note, so we won't write it" — is a
   different and stronger guarantee.
3. **`syncToken`-shaped incremental sync.** `accounts.sync_cursor` has been
   listed as "still deferred" in CLAUDE.md since the vertical split. Their
   implementation is the reference for the part that is not obvious: what to do
   when the server rejects the token (full refetch, reported, not an error).

### It is the third independent confirmation of gotcha #11

Their README: *"An Apple note has no title field of its own worth the name —
its title **is** its first line."* Three projects, three mechanisms (email
HTML, CloudKit protobuf, Android renderer), same rule.

They also give the **third answer** to the title-storage question that Jodd and
ImapNotes3 answer differently — they make it a **clone-time choice**:
`in-body` (default; file holds title as line one) or `--filename-as-title`
(file name *is* the title). Worth knowing when someone next argues Jodd's
separate-title-field design: the space of answers is three, not two, and all
three ship.

Two details from their title handling worth stealing if we ever expose notes
as files (OKF export, roadmap #7):
- Characters a filename can't hold (`/`, `:`, `?`) are swapped for **visually
  near Unicode look-alikes** and swapped back on the way up, so
  `Pat/Alex: notes` survives a round trip.
- A title no filename can carry at all is filed as `Untitled.md` with the real
  title in `apple-note-title` frontmatter — the fallback is explicit and
  reported, not silent.

### Where their limits differ from ours, and why

**Folders are create-only** — a renamed local directory reads as a new folder
plus a batch of note moves, because *a directory has nowhere to store an id*
(a note has `apple-note-id` frontmatter; a folder has no such place). Contrast
gotcha #12: our Microsoft folder limit is imposed by the *server* (Graph
cannot set `IPF.StickyNote`), theirs by the *local representation*. Jodd keeps
folder identity in SQLite and therefore has neither problem — worth remembering
that our cache is buying us something here.

---

## Practices worth taking, independent of any backend decision

Found while reading icloud-md on 2026-08-18. None of these depend on whether we
ever build a CloudKit vertical; they apply to the backends we already have.

### 1. Two-key containment for live tests — the one we need most

Jodd tests against real mailboxes constantly (`ms_write_probe` against
`kaiwan.h@live.com`, live Gmail verification) with **no rule stopping a bad run
from touching real notes**. icloud-md's `integration/containment.ts` states one:

> Two independent keys must both turn before anything is deleted:
> 1. the record lives inside the designated test folder, and
> 2. its title carries an `(itest-<runId>)` prefix.

with the reasoning spelled out — a prefix-only rule lets a typo'd folder name
delete something elsewhere; a folder-only rule ("empty the test folder")
deletes anything a human filed there. Two more rules around it:

- **The suite refuses to start** if the test folder holds anything unprefixed —
  an unexpected note there means the folder is not what we think it is.
- **A sweeper** (`integration/sweep.ts`) cleans debris from a crashed run,
  itself bounded to the folder and never deleting the folder.
- The test folder is created **by hand**, never by the suite.

**Adopt before the iCloud probe runs**, and retrofit onto the Microsoft live
probes. This is cheap and it is the difference between a bad test run being an
annoyance and being a data-loss incident on the maintainer's own account.

### 2. Redacted bug-report export

A sync app over private notes cannot ask a user for their data, so
`icloud-md bug-report --since 10m` produces a report with:

- **Identity fields dropped outright** (`firstName`, `lastName`, `primaryEmail`,
  the appleId aliases) — nobody debugging needs to correlate a real name.
- **`dsid`/`appleId` pseudonymized with a stable per-value alias** rather than
  dropped, because those *do* need correlating across a report ("is this URL
  and this response the same account"). Includes the bare `dsid=` query-string
  param, which is a substring of a URL rather than a JSON field.
- **A `--since <duration>` window**, so a report covers the reproduction and
  not the whole vault.
- **A `content-preview.md` written alongside**, decompressing everything the
  report contains so the user can *see what they would be leaking* before
  choosing to submit. The README then warns, in plain words, that a report
  taken right after a clone may expose every note.

Jodd has `log!` and a log file and nothing like this. The
"show them exactly what they're about to leak" step is the part worth copying —
it turns a privacy promise into something the user can verify.

### 3. Verify against an oracle outside your own code path

`integration/webOracle.ts` reads a note the way **Apple's own web client**
displays it, so a test asserts against something outside icloud-md's own
encode/decode entirely. A test that round-trips through your own parser proves
only that your parser agrees with itself — the same trap CLAUDE.md already
names for narrow CI commands ("a gate that differs from the merge gate only
proves it agrees with itself").

Two live-probed facts in there worth keeping even if we never build one: the
iCloud Notes SPA runs **inside an iframe** (`/applications/notes3/...`), and the
note body is **rendered to a `<canvas>`, not present in the DOM at all** — the
only way to read it is to focus the editor, select-all, copy, and take the
clipboard's `text/html`, whose spans carry Apple's native paragraph-style enum
in a `data-tt` attribute.

### 4. A "what would sync do" preview, including refusals

`push --dry-run` and `status` report exactly what a push will do *and what it
would refuse*, before it runs. Jodd's worker is invisible by design, and
gotchas #2 and #14 both describe cases where the user could not tell what the
queue was holding. We have `preview_orphans` and nothing more general.

### 5. Make the fragile constants loud

`CKJS_BUILD_VERSION` and `DEFAULT_CLIENT_BUILD_NUMBER` are captured from a real
browser session and carry a comment saying so, and saying they may need
bumping when Apple ships a new web client. Not clever — just named, isolated,
and honest about being a maintenance tax.

### 6. `importHar.ts` — a session can come from a HAR, not only from Playwright

`npm run import-har` accepts a HAR captured from an ordinary browser session as
an auth path. **This changes the plan for probe step A**: we may be able to sign
in with a normal browser, export a HAR from devtools, and feed it in — no
Playwright, no Chromium download, and the sign-in happens in a browser the user
already trusts. Try this route first.

### 7. Proto drift check in CI

`src/scripts/checkProtoDrift.ts` verifies the generated code still matches the
`.proto` sources. If we vendor their schemas (Option B), we need the equivalent
plus a way to notice when *upstream* changes theirs.

---

## Probe results — the door opens (2026-08-18, measured)

Ran icloud-md against a live account (`jodd.demo@gmail.com`, dsid `19507341918`)
to retire feasibility risk before any porting decision. **All three locks
opened.** What follows is measured, not inferred.

### Lock 1 — auth: OPEN, and no Playwright was needed for it

A HAR captured from an ordinary Chrome session was enough: `import-har`
verified it against the live server, and `clone --account <id> --non-interactive`
then reported *"Reusing jodd.demo@gmail.com's saved session"* and completed
**without launching a browser at all**. That is the whole architectural bet for
a Jodd vertical — user signs in wherever they normally do, we hold the resulting
cookie jar — and it is now evidence rather than a plan.

**Session lifetime is short, and this is a hard design input.** A session
captured at 15:58 worked; the same session at 19:33 returned HTTP 421
(expired) — under 3.5 h, possibly much less, since the live browser rotates
cookies and invalidates the copy. icloud-md's answer is a **persistent browser
profile** it can relaunch headless so Apple's own JS re-authenticates silently.

> **Consequence for Jodd:** the webview cannot be a one-shot login window we
> open and discard. It must be a long-lived, persisted profile we can revive
> to refresh the session. Decide this before writing the vertical, not after.

### Lock 2 — CloudKit: OPEN

`changes/zone` returned real records with real names and timestamps. A
never-used account holds exactly two: `DefaultFolder-CloudKit` ("Notes") and
`TrashFolder-CloudKit` ("Recently Deleted").

### Lock 3 — readable content: OPEN, and cheaper than expected

One note created by hand in the web client, then pulled. The `Note` record:

| Field | Type | What it actually is |
|---|---|---|
| `TitleEncrypted` | `ENCRYPTED_BYTES` | **base64 of plaintext.** Not encrypted, not compressed |
| `SnippetEncrypted` | `ENCRYPTED_BYTES` | base64 plaintext (`"No additional text"`) |
| `TextDataEncrypted` | `ENCRYPTED_BYTES` | base64 → **gzip *or* zlib** → protobuf document (see the correction below) |
| `Folder` | `REFERENCE` | → `DefaultFolder-CloudKit` |
| `Folders` | `REFERENCE_LIST` | same target, list form |
| `CreationDate` / `ModificationDate` / `FoldersModificationDate` | `TIMESTAMP` | ms epoch |
| `recordChangeTag` | — | `9` — the optimistic-lock token |

`recordName` is **`f8bf619a-1b84-40eb-932d-6318ee9aeeb4`** — a plain UUID, the
same shape Apple puts in `X-Universally-Unique-Identifier` on the email
backend and the same shape `format_apple_uuid` already mints. Identity may map
across mechanisms more directly than expected; worth checking whether the SAME
note carries the SAME uuid on both.

Formatting survives the decode: an auto-detected email became a real link
(`[jodd.demo@gmail.com](mailto:jodd.demo@gmail.com)`), so attribute runs decode,
not merely plain text. ADP was off on this account — still a per-user risk, not
a solved problem.

### ⚠️ Correction (2026-08-21): the container is gzip *or* zlib

The row above originally said **zlib** and cited the `789c` magic, because that
is what the one hand-made probe note carried. It is not the rule, and the
difference is not cosmetic.

icloud-md's own `noteText.ts` states the measurement plainly:

> The compression container isn't determined by which endpoint served the
> record — it's whatever format the data happened to be stored in, which
> depends on whichever client last wrote it (observed both gzip, magic
> `1f 8b`, and zlib, magic `78 9c`, coming back from the exact same
> `changes/zone` endpoint for different records). Both are tried.

**Why it mattered:** the iCloud M1 design used "does not begin with `78 9c`" as
half of its Advanced Data Protection test, since undecodable content is what ADP
looks like from outside. Under that rule every gzip-written note reads as
encrypted, so Jodd would have refused a perfectly readable account at sign-in and
told the user their notes were end-to-end encrypted — a false accusation with no
action available to the person receiving it. Caught while implementing
`backend/icloud/doc.rs`, before the ADP check was written.

A second, smaller trap in the same place: zlib's *second* header byte varies with
compression level (`78 01`, `78 5e`, `78 9c`, `78 da` are all valid), so even a
zlib-only decoder must not equality-test `78 9c`. Sniff gzip, attempt zlib,
let the decoder decide.

### 🔬 The 776-note measurement (2026-08-22) — read this before designing anything

`cargo run --example icloud_probe` against a **real, years-old Apple ID**
(`kaiwan@me.com`, 776 notes, 102 folders, 5 levels of nesting). Everything above
was measured on hand-made corpora of one to eight notes. This is what a real
account actually looks like, and it contradicts three things.

```
exact=520  prefix=41  differs=215  undecodable=0  title-only=33
compression containers: {"gzip": 774, "zlib": 2}
boundaries observed:    {"U+000A": 528}
deepest folder path: 5 segments;  1 folder title contains '/'
5 password-protected notes (separate record type)
```

**1. gzip is the MAJORITY, not the exception — 774 of 776.** Every document in
this repo said "base64 → zlib". The correction above already said "gzip *or*
zlib"; the real ratio is starker still. Cross-referencing the two accounts gives
the actual rule:

| account | how the notes were written | container |
|---|---|---|
| `jodd.demo` (6 notes) | iCloud **web** client | zlib 6/6 |
| `kaiwan@me.com` (776) | **Notes.app** on Mac/iPhone | gzip 774, zlib 2 |

So the container tracks the **writing client**, and the client most users have is
the one that writes gzip. A zlib-only decoder would have failed on 774 of 776
notes and — because undecodable content is the ADP signature — declared this
account end-to-end encrypted. The bug we caught from source reading would have
destroyed the product on the first real account.

**2. `undecodable=0` on 776 real notes.** The decode path (base64 → decompress →
`versioned_document` → `topotext`) survives a real corpus untouched. And the 5
password-protected notes arrive as a **separate record type**
(`PasswordProtectedNote`), never as unreadable `Note`s — so a per-note lock
cannot pollute an account-level ADP verdict. That was a prediction; it held.

**3. The title equals the body's first line only 67% of the time**, and the 215
exceptions have **measured, systematic causes** — not noise:

- **Apple TRUNCATES the title.** Body line 1 of 86 characters ⇒ title of 66,
  diverging at index 65 where the title carries `…` (U+2026). Another: 161 ⇒ 67,
  diverging at 66. So the title is a truncated prefix plus an ellipsis, cut
  somewhere around 65–66 characters.
- **Inline objects are `U+FFFC`** (OBJECT REPLACEMENT CHARACTER) in the body's
  text stream while the title renders them as text. Measured on a note whose
  title contains `#this`: the title has `#` at index 15, the body has `￼`.
- **The first line can be empty** — the title is the first NON-empty line.
- **`U+2028` is removed** from the title (recorded above).

**Consequence: `TitleEncrypted` cannot be used as a cut key by equality.** It is
a *derived, lossy display string*, not a copy of line one.

**4. `U+000A` on all 528 observations, no exceptions.** The title/body separator
question is closed.

**5. Nesting is real and deep** — 5 levels, 102 folders — and **one real folder
title contains `/`**, so the path-forging hazard is live on a real account rather
than hypothetical.

### ⚠️ Falsified: "hashtags come from the body"

The iCloud M1 design said tags would be derived from the note body by
`AppleHtmlDeriver`, like every other backend, citing "derive, don't migrate".
**The measurement kills that**: an inline hashtag is `U+FFFC` in
`topotext.String.string`, not the literal `#tag` text. A body-derived deriver
finds nothing on this backend.

The tag text lives elsewhere — in the attribute runs, and in the `Hashtag`
records CloudKit exposes and which that decision explicitly told us to ignore.
Re-open it with this evidence.

### Correction (2026-08-21): the `.proto` files are not in the npm package

"Reuse `proto/*.proto` verbatim (454 lines)" describes the upstream **git
repository**. icloud-md publishes `dist/` only, so the artifact carries no
`.proto` at all.

They are still recoverable exactly, which is better than it sounds.
`dist/notes/gen/*_pb.js` is generated by `protoc-gen-es`, and a Protobuf-ES
module embeds the serialized **`FileDescriptorProto`** of its source file as a
base64 string — protoc's own parse, not a copy of the text. Decoding it back to
`.proto` recovers field numbers, labels, types, nesting and defaults exactly,
rather than reconstructing a schema from note samples. Jodd's vendored copies in
`src-tauri/proto/` came from that route, with each descriptor re-serialized and
compared byte-for-byte before any text was emitted; `src-tauri/proto/PROVENANCE.md`
records the method.

One trap in it, found by the compiler rather than by reading: **`protoc-gen-es`
strips `FileDescriptorProto.dependency`** and passes imports as a second
JavaScript argument to `fileDesc(...)` instead. So a descriptor-only recovery
silently loses `crdt.proto`'s `import "topotext.proto"`, and the round-trip
check still passes because the bytes it compares never had the field. The
imports have to be read out of the JS call.

Sizes, measured: `crdt.proto` 112 lines, `topotext.proto` 89,
`versioned_document.proto` 14 — 215 total, not 454. The larger figure counted
upstream's comments, which a descriptor does not carry.

### The scoping consequence — this is the useful part

**Title, snippet, folder membership and every timestamp need ZERO protobuf
work.** Only the note *body* requires the CRDT decode. So a read-only M1 —
list notes with their real titles, folders and dates — is reachable **without
touching `crdt.proto` at all**, which is a far smaller first milestone than
"port the content model". The expensive half can wait for M2.

### The web client cannot create folders (measured 2026-08-22)

Relevant because it decides how a **test corpus** can be built, and it cost a
round trip to find out.

`icloud.com/notes` offers no folder creation at all. Backing out of a note to
the folder-list level shows only `All iCloud` and `Notes`, and the `+` button's
"Create New" sheet lists Email Message, Calendar Event, Note, Reminder, Pages
Document, Numbers Spreadsheet and Keynote Presentation — **no folder**.

So on an Apple ID reachable only through the web, the folder tree is frozen at
whatever Notes.app already created. Verifying anything about folder *nesting*
(the iCloud M1 design's verify-first #5) needs Notes.app on a Mac or an iPhone,
or an account that already has subfolders. Notes themselves are fine to create
from the web, so every content-shaped question is still answerable.

This is a limit on **building test data**, not on reading: `Folder` records
carry `ParentFolder` in the schema either way, and an account with no subfolders
proves nothing about whether nesting works.

### Two traps that cost time during the probe

- **Chrome's HAR export is sanitized by default** and strips cookies, producing
  a file that looks fine and is useless. The switch is in **DevTools Settings →
  Preferences → Network → "Allow to generate HAR with sensitive data"** — *not*
  in the Network panel's own gear. Once on, the right-click Copy submenu gains
  "Copy all as HAR (with sensitive data)".
- **Do not paste a HAR through a terminal.** Terminal focus-reporting escapes
  (`ESC[O`) get injected mid-stream and corrupt the JSON at a random offset.
  `pbpaste > file.har` writes it cleanly.

---

### Jodd's own code now walks through it — `examples/icloud_probe.rs`

Written and run 2026-08-18. Rust only: no Node, no Playwright, no protobuf.

```
✓ signed in as jodd.demo@gmail.com  (dsid 19507341918)
  CloudKit host: https://p149-ckdatabasews.icloud.com:443
  syncToken: HwoCCAkYACIWCLeA+fX36pnO… (44 chars)
✓ fetched 3 record(s) from the Notes zone
2026-08-18T12:25:51.931+00:00  Notes  this is the first jodd.demo@gmail.com note  213 B zlib+protobuf (not decoded)
```

Three things this pins down that the icloud-md run alone did not:

- **The partition host is discovered, never guessed.** `/validate` returned
  `p149-ckdatabasews.icloud.com` for this account; a different account gets a
  different `p<N>`. It also doubles as the account bootstrap (dsid, appleId), so
  it is the mandatory first call of any session.
- **`syncToken` is real and 44 chars here.** This is exactly what
  `accounts.sync_cursor` — "still deferred" since the vertical split — would
  hold. The incremental-sync model arrives for free on this backend rather than
  needing to be invented.
- **`clientBuildNumber` on the live session was `2628Build44`**, not the
  `2624Build27` icloud-md hardcodes as its fallback. The drift their comment
  warns about is real and already happened. **Read these from the session, never
  hardcode them** — the probe does, which is why it worked unmodified.

The probe borrows icloud-md's stored cookie jar from `~/.config/icloud-md/`.
That is scaffolding for the experiment, not a proposal: a real vertical owns a
long-lived webview profile, per the session-lifetime finding above.

### Identity: the uuid shapes match, the strings do not — and there is a trap

Checked 2026-08-18, before writing any vertical.

- **Email backend (Apple's own):** `X-Universally-Unique-Identifier` is
  **UPPERCASE** hyphenated `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`.
  [docs/GMAIL-SYNC.md](GMAIL-SYNC.md) records why this is load-bearing: Apple
  reconciles by `strcmp`, and Jodd's old hyphen-stripped form read as a
  *different* note — "the first major interop bug we fixed".
- **CloudKit:** `recordName` is **lowercase** —
  `f8bf619a-1b84-40eb-932d-6318ee9aeeb4`.

Same shape, different case, so they are **not** string-equal.

**The trap.** `save_note_db` (lib.rs) is backend-agnostic and runs every
incoming identity through `mime822::canonicalize_uuid`, which **uppercases**:

```rust
Some(u) if !u.is_empty() && !u.starts_with("tmp:") => {
    crate::mime822::canonicalize_uuid(u).unwrap_or_else(|| u.to_string())
}
```

Microsoft survives this **by accident**: `internetMessageId` (`<...@...>`) does
not parse as a UUID, so `canonicalize_uuid` returns `None` and the id passes
through untouched. A CloudKit `recordName` **does** parse — so it would be
silently uppercased on the user's first edit, stop matching the record it names,
and take the gotcha-#16 rekey path for no reason.

> **Decide before writing the vertical:** either store the recordName verbatim
> and exempt the iCloud backend from canonicalization, or keep a separate
> `remote_id` and let `uuid` stay Apple-email-shaped. Do not discover this from
> a lookup that mysteriously 404s after the first save.

**Whether identity is *continuous across mechanisms* cannot be answered as
posed.** An iCloud-native note and a Gmail-backed note live in *different Apple
Notes accounts*; the same note is never in both. The only way to create the
comparison is to move a note between accounts in Notes.app, which is expected
(not measured) to be copy-then-delete with a fresh identity in the destination.
It also would not change much: Jodd's PK is already `(uuid, account_id)`, so an
iCloud account is simply a third namespace. It matters only for a future
"migrate my notes between backends" feature that would need to dedup.

*The experiment, if it is ever worth running:* on a device holding both
accounts, note an iCloud note's recordName, move it to the Gmail account in
Notes.app, then read the resulting message's `X-Universally-Unique-Identifier`.

## Open questions for the next session

- Run Option A live (a throwaway iCloud account) and capture real record
  shapes — cheapest way to validate everything above against reality.
- Does the CloudKit note document carry enough to reconstruct the HTML Apple
  puts on the *email* backend, or are they lossy in different directions?
  If a note can round-trip document → HTML → document, one Jodd account could
  in principle span both mechanisms.
- What fraction of the target user base has ADP on? Decides whether Option B
  is a headline feature or a footnote.
