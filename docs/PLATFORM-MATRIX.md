# What Jodd can do, per platform and per account

Two independent questions, so two tables.

1. **Which accounts can you add at all?** Decided by the operating system —
   an account type whose sign-in needs an OS API the platform doesn't have is
   not offered there.
2. **What can you do once the account is added?** Decided by the account's
   backend, not by the platform. A Gmail account behaves identically on
   Windows, macOS and Android.

Everything below is derived from
[`Capabilities::for_backend`](../src-tauri/src/backend/mod.rs) and the two
platform gates in [`platform.ts`](../src/lib/stores/platform.ts), which are the
code the app itself reads to decide what to show. If this file and that code
ever disagree, the code is right.

Jodd is a **Developer Preview**. It is an independent project, not affiliated
with or endorsed by Apple, Google or Microsoft.

---

## 1. Account types available, per platform

| Account type | Windows | macOS | Android (Dev Preview) |
|---|:--:|:--:|:--:|
| **Gmail** — Apple Notes synced through a Google account | ✅ | ✅ | ✅ |
| **Microsoft** — Apple Notes over Outlook.com / Microsoft 365 (Exchange) | ✅ | ✅ | ❌ |
| **iCloud** — Apple Notes directly, via CloudKit | ❌ | ✅ | ❌ |
| **Local Folder** — an offline `.eml` vault on disk, no cloud account | ✅ | ✅ | ❌ |

**Why the gaps are where they are:**

- **iCloud is macOS-only, and this is a real OS constraint, not a policy.**
  There is no OAuth on this backend at all. The credential is a live browser
  session living in a per-webview persistent data store, which maps to
  `WKWebsiteDataStore(forIdentifier:)` — a **macOS 14+ API with no counterpart
  on Windows or Android**. Windows and Android support each need their own
  session-storage design and are not scheduled.
- **Microsoft is desktop-only** because its sign-in uses a loopback redirect
  Android does not run. (Android's OAuth path is a verified App Link, built for
  Google's flow; Microsoft's would need the same treatment.)
- **Local Folder is desktop-only** because Android gives an app no arbitrary
  filesystem to point a vault at.

Also platform-dependent, and unrelated to accounts:

| | Windows | macOS | Android |
|---|:--:|:--:|:--:|
| Agent-CLI LLM providers (`claude`, `codex`, …) for Ask Jodd / Extract | ✅ | ✅ | ❌ — no child processes |
| HTTP LLM providers (API key) | ✅ | ✅ | ✅ |
| Local cache encrypted at rest (SQLCipher) | ✅ | ✅ | ✅ |

---

## 2. What each account type can do

✅ works · ⚠️ works with a stated limit · ❌ does not work

| | Gmail | Microsoft | **iCloud** | Local Folder |
|---|:--:|:--:|:--:|:--:|
| Read notes and folders | ✅ | ✅ | ✅ | ✅ |
| Create / edit a note's title and body | ✅ | ✅ | ⚠️ **most notes** | ✅ |
| Rich text (bold, italic, underline, strike, headings, lists, checklists) | ✅ | ✅ | ✅ | ✅ |
| Move a note between folders | ✅ | ✅ | ✅ | ✅ |
| Delete a note | ✅ | ⚠️ **permanent, no undo** | ✅ | ✅ |
| Recently Deleted / restore | ✅ | ❌ | ⚠️ **asks where to restore** | ✅ |
| Create / rename / delete a folder | ✅ | ❌ **permanent limitation** | ✅ | ✅ |
| Move a folder to a different parent | ✅ | ❌ | ❌ | ✅ |
| Nested folder tree (`Notes/A/B`) | ✅ | ❌ **flat list only** | ✅ | ✅ |
| Pin a note | ✅ | ✅ | ⚠️ **read-only** | ✅ |
| Read inline `#hashtags` | ✅ | ✅ | ✅ | ✅ |
| Create a new `#hashtag` from Jodd | ✅ | ✅ | ❌ | ✅ |
| Display an existing attachment | ✅ | ❌ | ❌ | ✅ |
| Add a new attachment | ❌ | ❌ | ❌ | ❌ |
| Reminders / tasks | ❌ | ❌ | ❌ | ❌ |
| Full-text search, tags, graph, Ask Jodd | ✅ | ✅ | ✅ | ✅ |

Everything in the last row is Jodd's own layer over the local cache, so it
works the same everywhere regardless of backend.

---

## 3. The limits worth reading before you rely on them

### iCloud — the newest backend, and the most conditional

This is the one that reaches Apple Notes users who never attached a Google or
Microsoft account to Notes, which is most of them. It reads everything and
writes most things, but the write path is gated **per note**, not per account.

- **About 3 notes in 4 are editable.** Measured on a real 773-note account:
  **593 writable (76.7%)**. The remaining notes are refused individually and
  **say so on the note itself** — Jodd never silently declines to save. The
  refusal reasons, with their measured counts: 95 notes whose document Jodd
  cannot re-encode byte for byte, 50 notes containing an attachment, table or
  inline tag, 35 whose formatting layers do not round-trip, plus any
  password-protected note.
- **Why the gate exists at all:** an iCloud note is not HTML or email. It is
  Apple's own CRDT document, carrying per-character identity and formatting
  Jodd does not fully interpret. Rather than rebuild the document from the text
  it *can* read — which would destroy every formatting decision on the note,
  server-side and silently — Jodd preserves what it cannot read and **refuses
  the write when it cannot prove it did so**.
- **Password-protected notes are visible but not readable.** Title, folder and
  dates come through; the body shows a placeholder, and Jodd will not write to
  the note.
- **Pins are Apple's, and read-only here.** The pin lives on a separate
  per-user CloudKit record that Apple owns. Jodd shows it correctly and does
  not offer to change it — a control that visibly did nothing would be worse.
- **A restore asks you where to put it.** A trashed note's original folder is
  overwritten by the Trash's, so Jodd offers "Restore into…" rather than
  filing every restored note at the root and calling that a restore.
- **Advanced Data Protection ends this backend for an account.** With ADP on,
  note bodies are genuinely end-to-end encrypted. Jodd detects this at sign-in
  and refuses before creating anything, rather than leaving a broken account
  behind.
- **One Apple ID per install.** macOS gives the app a single app-wide cookie
  jar, so a second iCloud account would silently share the first one's session.

### Microsoft (Outlook.com / Microsoft 365) — notes yes, folders never

- **Folder create / rename / delete will never work here.** Not a pending
  milestone: both of Graph's folder-creation surfaces silently drop the
  container class that marks a folder as part of Apple's Notes tree, and that
  class is immutable after creation. Folders created by hand in Notes.app work
  fine, and Jodd writes notes into them without trouble.
- **The folder tree is flat, and that matches what Apple itself shows.**
  Nesting is unrecoverable from anything Graph exposes. Folder nesting a Mac
  shows for this account is a single-device artifact — the same account's
  iPhone shows the same folders flat.
- **Delete is permanent.** Nothing lands in Deleted Items, so there is no
  Recently Deleted view to offer. Jodd asks for confirmation instead.
- **Attachments are impossible**, because Apple refuses them on Exchange
  accounts outright.
- **A work or school account may need your IT administrator's approval.**
  Personal `@outlook.com` / `@live.com` accounts consent normally. In an
  organisation that restricts third-party app consent — most do for mailbox
  access — an ordinary user is refused with *"Need admin approval"* until an
  administrator grants consent tenant-wide. Jodd shows the exact link to send
  them.

### Gmail

The oldest and most complete backend. Its one structural quirk: Gmail has no
in-place replace, so every save inserts a new message and trashes the old one.
That is invisible in normal use and is why a note's identity is preserved
through an Apple-compatible header rather than through the message id.

### Local Folder

A vault of `.eml` files on disk that never touches a network. Everything works,
including attachment display, but nothing syncs to Apple Notes — it is a local
store, not a bridge.

### Everywhere

- **Adding a new attachment is not implemented on any backend yet.** Existing
  attachments display and round-trip where the backend supports them.
- **Anything Jodd stores outside the note body is Jodd-local** and will not
  appear on your iPhone: workflow output folders, smart folders, the note
  graph, and citations. Tags are the exception — they live in the body as
  `#hashtags`, so they round-trip.
