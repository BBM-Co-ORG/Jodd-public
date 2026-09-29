# Jodd — Notes across your devices

> **Thai จด** (jòt) — "to jot, to note down."

Jodd is a local-first Developer Preview for viewing and editing notes across
Windows, macOS, and Android. It supports four backends: **iCloud**,
**Gmail**, **Microsoft**, and a fully offline **Local Folder** vault.

**Which accounts you can add depends on your platform, and what you can do
depends on the account.** Both are laid out in
[docs/PLATFORM-MATRIX.md](docs/PLATFORM-MATRIX.md) — read that before relying
on any single capability.

Apple Notes can store notes in non-iCloud accounts; when Notes sync is
enabled for Gmail, notes live as RFC 822 messages under a `Notes` label, and
Jodd reads/writes that same set. When Notes sync is enabled for a Microsoft
account (`outlook.com`, `live.com`, Microsoft 365), Apple treats it as an
**Exchange** account, not an email one — notes are structured Exchange
items, not messages — and Jodd talks to that same backend over the
Microsoft Graph API.

**Notes stored only in iCloud are reachable too, as of 0.25.0** — through
CloudKit, the same private web service `icloud.com` uses, with no OAuth and no
email account in the middle. **That backend signs in on macOS only** (its
credential is a live Apple browser session held in a macOS 14+ per-webview data
store, an API Windows and Android have no counterpart for), and it edits most
notes rather than all of them. See [Status](#status) and the matrix.

The **Local Folder** backend is different in kind, not just in transport:
it has no account, no OAuth, and no network connection at all, and it does
**not** sync with Apple Notes — it's a private, standalone vault that
stores notes as plain `.eml`-formatted files in a folder you choose on your
own device. Reaches for the same code paths as Gmail/Microsoft internally
(same content model, same local cache), but there's no iPhone or Mac on the
other end of it.

> **The Microsoft/Outlook.com backend and local at-rest encryption (AES-256,
> SQLCipher) are both in this snapshot's source tree and in the current
> release.** Note one version boundary: builds **0.24.1 and earlier** carry no
> Microsoft OAuth client, so signing in to a Microsoft account from those
> binaries needs your own registered application supplied as `MS_CLIENT_ID` in
> the environment. Later builds embed one, and that variable still overrides it
> if you prefer your own. Either way, **a work/school Microsoft 365 account may
> be refused by your own organisation** rather than by Jodd — see
> [Status](#status) below. Gmail sign-in is unaffected.

This is a working technical product and a public build story by
[BBMedia](https://bbmedia.co.th/), not yet a frictionless consumer app. Read
the [current limitations](https://jodd.bbmedia.co.th/#limitations) before
installing.

---

## Ways to evaluate Jodd

### 1. Download the Developer Preview

The [Releases page](https://github.com/BBM-Co-ORG/Jodd-public/releases)
provides Apple Silicon macOS, Windows, and signed universal Android builds.
Release builds contain the OAuth client configuration needed for Gmail
sign-in, but onboarding and platform trust prompts are still technical.

### 2. Build from source

Developers can build Jodd with their own Google OAuth client. Environment or
`.env` credentials override the embedded release configuration; see
[Build from source](#build-from-source).

### 3. Use a desktop Local Folder vault

The desktop app also has a Local Folder backend that stores the same `.eml`
format in a folder you choose, with no cloud account and no network. This
backend is implemented and useful for development, but it is not yet exposed
on the first-run screen; today the **Add Local Folder** action appears after
the initial Gmail onboarding. It is not available on Android.

Outlook / Microsoft Graph is implemented and verified end-to-end against a
live account (create, edit, move, and delete all round-trip correctly to
Apple Notes and the iPhone; pin is Jodd-only there too — it rides along as a
named property on the note that Graph round-trips but Apple ignores), and
its source ships here under `src-tauri/src/backend/microsoft/`. On builds
0.24.1 and earlier, which embed no Microsoft OAuth client, trying it means
supplying your own registered application's
`MS_CLIENT_ID` via the environment at runtime and restarting — not
rebuilding — see the note at the top of this README and
[Status](#status).

---

## What round-trips to Apple Notes vs. what stays in Jodd

*Covers the Gmail backend, which is what a downloaded release build can sign
into today. The Microsoft backend round-trips title/body/folder-move/delete
the same way (pin stays Jodd-only there too — Graph carries it as a named
property on the note, but Apple ignores it), with the two exceptions noted
in [Status](#status): no folder create/rename/delete, and no attachments.
Local Folder isn't in this table at all — it's not Apple Notes sync, so
there's nothing to round-trip.*

| Feature | Round-trips to Apple? |
|---|---|
| Note title & rich-text body | ✅ Yes |
| Folder hierarchy (`Notes/Work/Projects`) | ✅ Yes |
| Inline `#hashtags` in body | ✅ Yes |
| Checklists | ✅ Yes (Jodd-authoritative state) |
| Existing attachments (images, PDFs) | ✅ Displayed and preserved on round-trip |
| Add a new attachment from Jodd's UI | ❌ Not yet |
| Pin 📌 | Jodd-only — visible across your Jodd devices, invisible on iPhone |
| `[[wikilinks]]` + graph view | Jodd-only — stored as text in the body; Apple shows plain text |
| AI-extracted notes | Jodd-only — folder lives in Gmail, iPhone ignores it |

*"Jodd-only" means the data is safe and lives in your Gmail or local files —
it just won't render the same way in Apple Notes on iPhone.*

---

## Status

Pre-1.0 Developer Preview. The Gmail backend works end-to-end, with broader
device and onboarding validation still in progress. Major features shipped:

- Gmail-backed Apple Notes round-trip for title, body, folders, and preservation
  of existing attachments
- Conflict resolution (keep-both) when the same note is edited on two devices
- Multi-account — connect several Gmail accounts simultaneously
- Rich text: headings, bold/italic/underline, checklists, ordered & unordered lists
- Inline `#hashtags` with sidebar filtering, rename, and cross-account search
- `[[wikilinks]]` with autocomplete, a connections panel, and a local graph view
- AI-assisted note extraction (paste any text → structured extract note)
- Pin notes, multi-select batch move/delete, recently-deleted restore
- **Standalone Local Folder** — `.eml` vault, no cloud account required
- Optional **jodd-mcp** server for searching and connecting notes, plus
  deny-by-default, folder-allowlisted note and checklist writes
- Optional diagnostics logging (App Settings → Diagnostics), off by
  default, to help debug sync issues after the fact

**Also in this snapshot, with limitations worth stating up front:**

- **iCloud backend — macOS only.** Reads your Apple Notes and their real
  nested folder tree directly from CloudKit, and writes note text, formatting,
  folder moves, folder create/rename/delete, deletes and restores. Verified
  live against a real 773-note / 101-folder account, agreeing with Notes.app
  note for note and folder for folder, with edits confirmed on the Mac, on
  icloud.com and on an iPhone. **Five limitations to know before you rely on
  it.** *Sign-in is macOS-only* — there is no OAuth here at all, and the
  session storage it needs is a macOS 14+ API with no Windows or Android
  counterpart; that support is not scheduled. *Not every note is editable* —
  measured at **593 of 773 (76.7%)**: an iCloud note is Apple's own document
  format carrying per-character identity and formatting Jodd does not fully
  interpret, so rather than rebuild the note from the text it can read (which
  would destroy formatting on Apple's servers, silently) Jodd refuses the write
  when it cannot prove it preserved everything — and the note itself says so,
  never a silent failure. *Pins are Apple's own* and are shown but not
  editable, because they live on a separate record Apple owns. *A `#hashtag`
  cannot be created from Jodd yet*, though existing ones display and index.
  And *an account with Advanced Data Protection enabled cannot work at all* —
  note bodies are then genuinely end-to-end encrypted; Jodd detects this at
  sign-in and refuses before creating anything, rather than leaving you a
  broken account to clean up.
- **Microsoft/Outlook.com backend.** Sign in, read, create, edit, move, and
  delete notes — all measured end-to-end against a live
  `outlook.com`/`live.com` account, with a real in-place update rather than
  Gmail's insert-and-trash approach. Pin works there too, but stays
  Jodd-only exactly as on Gmail:
  Graph carries it as a named property on the note, which Apple ignores, so
  a pinned note still renders normally on iPhone with no pin shown. Builds
  **0.24.1 and earlier** embed no Microsoft OAuth client, so reaching this
  backend from one of those means supplying your own registered application's
  `MS_CLIENT_ID` via the environment at runtime and restarting — no rebuild
  required. Later builds embed one; the variable still overrides it.
  **Three permanent limitations, by
  design of Microsoft Graph, not by Jodd's choice:** creating, renaming, or
  deleting a *folder* from Jodd doesn't reach Apple Notes (Graph cannot set
  the container property Apple's sync requires); attachments aren't
  supported at all (Apple itself refuses them on Exchange accounts); and
  **a work/school Microsoft 365 account may not be yours to connect.**
  Microsoft lets each organisation decide whether its staff can consent to
  third-party apps, and read/write mailbox access is not a permission most
  grant without an administrator — measured against an outside tenant, an
  ordinary employee account is refused with *"Need admin approval"* and no way
  to proceed. Your IT can approve Jodd for the organisation if they choose to;
  nothing Jodd does from its side changes that answer. Personal
  `@outlook.com`/`@live.com` accounts are not affected — they consent normally.
  Reminders/tasks — which the same Exchange account exposes — aren't read
  or written yet either.
- **At-rest encryption.** The local SQLite cache is encrypted (AES-256,
  SQLCipher), meaningfully so for both Gmail and Microsoft accounts, since
  both are a cache of a remote source of truth. It does **not** protect a
  Local Folder vault, whose whole point is a plain, directly-readable
  folder of files on disk. The encryption key lives in the OS credential
  store (macOS Keychain / Windows Credential Manager / Android Keystore),
  the same place OAuth refresh tokens already live — on macOS, that's one
  more per-entry "Allow" prompt, and it can re-trigger on a re-signed
  build since ad-hoc/dev signing isn't stable across builds. Not a bug;
  expected Keychain behavior for a preview build in this state.

---

## Install pre-built binaries

Download from the [Releases page](https://github.com/BBM-Co-ORG/Jodd-public/releases).

Desktop binaries are **ad-hoc signed** (macOS) and **unsigned** (Windows).
Android APKs are release-signed. macOS and Windows warn on first run:

- **macOS** — "Apple cannot check this app…" → right-click → **Open** → confirm.
  After the first launch, subsequent opens are normal.
- **Windows** — SmartScreen: click **More info → Run anyway**.
- **Android** — install the universal APK from Releases. Sideloading may
  require allowing installs from the browser or file manager you use.

If you're not comfortable bypassing these warnings, build from source instead
(same code, signed by your own toolchain).

---

## Build from source

**Requirements:** Rust stable (`rustup`), Node.js ≥ 20

### Build the app

```bash
git clone https://github.com/BBM-Co-ORG/Jodd-public
cd Jodd-public
npm install
npm run tauri build
```

### Gmail sync — optional BYO credentials

1. [Google Cloud Console](https://console.cloud.google.com/) → create a project
2. **APIs & Services → Library** → enable **Gmail API**
3. **OAuth consent screen** → External → add your email as a test user →
   scope `https://www.googleapis.com/auth/gmail.modify`
4. **Credentials → Create → OAuth client ID → Desktop application**
5. Copy the **Client ID** and **Client Secret**

```bash
cp .env.example .env
# Edit .env and fill in GOOGLE_CLIENT_ID and GOOGLE_CLIENT_SECRET
npm install
npm run tauri build
```

> Source builds need your own OAuth client if you do not supply BBMedia's
> release build environment. The client secret for a Desktop OAuth app is not truly confidential
> ([per Google's own docs](https://developers.google.com/identity/protocols/oauth2));
> PKCE provides the per-flow security on top of it.

---

## Contributing

PRs are welcome — bug fixes especially.

Development happens on a private upstream repository. This public repository
is a periodic sanitized snapshot. Open your PR here; maintainers will
cherry-pick into upstream with attribution.

For security issues, see [SECURITY.md](SECURITY.md) — do not file public issues.

## Follow the build and learning trail

Jodd is also an open technical case study. Start with
[Architecture](ARCHITECTURE.md) for how the system works, then
[Engineering practice](ENGINEERING-PRACTICE.md) for how it got built — the
operating loop and the artifacts it produces, what the review gates actually
caught, two decisions worked through end to end, and how every published
figure was measured.

From there, the product and engineering [History](docs/HISTORY.md) and
[Direction](docs/DIRECTION.md) cover what changed and why. Selected design
specifications under [`docs/superpowers/specs/`](docs/superpowers/specs/)
show how decisions were framed before implementation; internal handoffs and
machine-specific execution plans are intentionally omitted from the public
snapshot.

A narrative version of the same material, with diagrams, is at
[jodd.bbmedia.co.th/case-study.html](https://jodd.bbmedia.co.th/case-study.html).

---

## License

[Apache License 2.0](LICENSE).

Jodd is **not affiliated with Apple, Google, or Microsoft**.
