# Jodd — Notes across your devices

> **Thai จด** (jòt) — "to jot, to note down."

Jodd is a local-first Developer Preview for viewing and editing notes across
Windows, macOS, and Android. It supports five backends: **iCloud**,
**Gmail** and **Microsoft** for Apple Notes, plus two stores of your own —
a fully offline **Local Folder** vault and an **SSH server** you control.

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
email account in the middle. It signs in on Windows, macOS and Android (its
credential is a live Apple browser session kept in an isolated web store; each
platform was added only after it was measured — Windows 2026-09-09, Android
2026-09-10), and it edits most notes rather than all of them. See
[Status](#status) and the matrix.

The **Local Folder** backend is different in kind, not just in transport:
it has no account, no OAuth, and no network connection at all, and it does
**not** sync with Apple Notes — it's a private, standalone vault that
stores notes as plain `.eml`-formatted files in a folder you choose on your
own device. Reaches for the same code paths as Gmail/Microsoft internally
(same content model, same local cache), but there's no iPhone or Mac on the
other end of it.

The **SSH server** backend is the same idea on a machine you control: plain
Markdown (or HTML) files in a folder on any server you can reach over SSH, a
vault several Jodd installs — and scripts or agents on that server — can
share. It does not sync with Apple Notes either. On desktop it can use your
own `ssh-agent` and `~/.ssh/config`; on every platform, including Android, a
managed setup makes and installs Jodd's own key from a password typed once.

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
- AI note workflows from pasted text or links (web pages, YouTube): Key
  points, Summarize, Action items, Expand bullets, Transcript
- **Share to Jodd** — send links and text from other apps (Android Share
  sheet, macOS Services menu and Share sheet, a browser bookmarklet)
- **Organize** (desktop) — finds duplicate notes, unfiled notes and
  plain-text secrets, and fixes only what you approve
- Optional **jodd-mcp** server so AI agents (Claude Code and others) can
  search and read your notes, with deny-by-default, folder-allowlisted
  writes, folders you can hide from agents, and an opt-in agent workspace
- **SSH server** vaults, and Local Folder vaults on Android 11+
- Optional diagnostics logging (App Settings → Diagnostics), off by
  default, to help debug sync issues after the fact

**Also in this snapshot, with limitations worth stating up front:**

- **iCloud backend.** Reads your Apple Notes and their real
  nested folder tree directly from CloudKit, and writes note text, formatting,
  folder moves, folder create/rename/delete, deletes and restores. Verified
  live against a real 773-note / 101-folder account, agreeing with Notes.app
  note for note and folder for folder, with edits confirmed on the Mac, on
  icloud.com and on an iPhone. **Four limitations to know before you rely on
  it.** *Not every note is editable* —
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

## Install

Everything is on the [Releases page](https://github.com/BBM-Co-ORG/Jodd-public/releases).
Desktop builds are **ad-hoc signed** (macOS) and **unsigned** (Windows), so
both warn on first run; Android APKs are release-signed. If you're not
comfortable bypassing these warnings, [build from source](#build-from-source)
instead — same code, signed by your own toolchain.

### macOS (Apple Silicon)

1. Download `Jodd_<version>_aarch64.dmg`, open it, and drag **Jodd** into
   Applications. (There is no Intel build at the moment.)
2. Open Jodd. macOS refuses the first time ("Apple could not verify…"):
   - **macOS 15 or later:** open **System Settings → Privacy & Security**,
     scroll down, and click **Open Anyway** next to Jodd.
   - **macOS 14:** right-click Jodd → **Open** → **Open**.
3. When macOS asks to let Jodd use its keychain item (the key that encrypts
   Jodd's local cache), enter your password and choose **Always Allow**. It
   asks again after each update, because preview builds are not signed with a
   stable identity.

### Windows

Run `Jodd_<version>_x64-setup.exe` (or the `.msi`). At the SmartScreen
warning, click **More info → Run anyway**.

### Android

Download `Jodd_<version>_android-universal.apk` on the phone and open it.
Android may ask you to allow installs from your browser or file manager
first.

### Updating

There is no in-app updater yet. Install the new release **over** the old one
— **don't uninstall first**: uninstalling deletes Jodd's local data, and any
edit that has not synced yet would be lost. Your accounts and notes carry
over an in-place update.

### Share to Jodd (optional)

- **Android:** nothing to set up — tap **Share** in any app and pick **Jodd**.
- **macOS:** in Jodd, **Settings → Share to Jodd → Add to Share menu**, then
  **Add Shortcut** in the Shortcuts window. After that, select text anywhere
  and right-click → **Services → Share to Jodd**, or use the Share button in
  Safari. (Chrome's own File → Share menu does not list Shortcuts; use
  Services or the bookmarklet there.)
- **Any desktop browser:** **Settings → Share to Jodd → Copy bookmarklet**,
  then add a bookmark with what you copied as its address. Clicking it sends
  the page's link, title and selected text. Not yet tried on Windows.

Jodd always shows what arrived and saves nothing until you choose.

### jodd-mcp — notes for your AI agents (optional, desktop)

`jodd-mcp` lets an MCP client such as Claude Code search and read your notes,
and write only where you allow. Run the Jodd app at least once first: the
server reads the same encrypted cache. What it can do, and what it sends
where, is in the [privacy policy](https://jodd.bbmedia.co.th/privacy.html#mcp).

**macOS:**

```bash
mkdir -p ~/.local/bin
tar -xzf ~/Downloads/jodd-mcp_<version>_aarch64-apple-darwin.tar.gz -C ~/.local/bin --strip-components 1
xattr -c ~/.local/bin/jodd-mcp
claude mcp add jodd -- ~/.local/bin/jodd-mcp
```

Don't skip `xattr -c`: a downloaded binary keeps macOS's quarantine flag
through `tar`, and Gatekeeper then kills it on every start (exit 137, no
error message). The first time it opens your notes, allow its keychain
prompt with **Always Allow** — and again after you replace the binary.

**Windows (PowerShell):**

```powershell
Expand-Archive "$HOME\Downloads\jodd-mcp_<version>_x86_64-pc-windows-msvc.zip" "$env:LOCALAPPDATA\jodd-mcp"
claude mcp add jodd -- "$env:LOCALAPPDATA\jodd-mcp\jodd-mcp_<version>_x86_64-pc-windows-msvc\jodd-mcp.exe"
```

**Then, in Jodd's Settings → Agent workspace:** hide any folder agents must
never see, and turn on the workspace if you want agents to keep decisions and
lessons in `Notes/__Agent__`. Writing anywhere else stays off until you allow
specific folders — see [jodd-mcp/README.md](jodd-mcp/README.md#granting-write-access).

**Optional — a briefing at the start of every Claude Code session.** Add this
to Claude Code's `settings.json` (`jodd-mcp` must be on your `PATH`, or use its
full path):

```json
{ "hooks": { "SessionStart": [ { "hooks": [
  { "type": "command", "command": "jodd-mcp brief --cwd \"$CLAUDE_PROJECT_DIR\"" } ] } ] } }
```

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
