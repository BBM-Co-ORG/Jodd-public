# Jodd — Notes across your devices

> **Thai จด** (jòt) — "to jot, to note down."

Jodd is a local-first Developer Preview for viewing, editing, and connecting
Gmail-backed Apple Notes across Windows, macOS, and Android. Apple Notes can
store notes in non-iCloud email accounts; when Gmail Notes sync is enabled,
those notes live as RFC 822 messages under a `Notes` label. Jodd reads and
writes that same Gmail-backed set. It cannot see notes stored only in iCloud.

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

Outlook / Microsoft Graph remains planned; it is not implemented today.

---

## What round-trips to Apple Notes vs. what stays in Jodd

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
[Architecture](ARCHITECTURE.md), then read the product and engineering
[History](docs/HISTORY.md) and [Direction](docs/DIRECTION.md). Selected design
specifications under [`docs/superpowers/specs/`](docs/superpowers/specs/) show
how decisions were framed before implementation; internal handoffs and
machine-specific execution plans are intentionally omitted from the public
snapshot.

---

## License

[Apache License 2.0](LICENSE).

Jodd is **not affiliated with Apple, Google, or Microsoft**.
