# Jodd — Apple Notes, anywhere

> **Thai จด** (jòt) — "to jot, to note down."

Apple Notes uses email as its sync backbone for non-iCloud accounts.
Every note is a plain RFC 822 message with custom headers living in a
`Notes` label on your Gmail, Outlook, or Exchange mailbox. Jodd reads
and writes those same messages — so your notes are yours, wherever you are.

---

## Two ways to use Jodd

### 1. Standalone — no account needed

Store notes as `.eml` files in any folder on your computer.
Zero sign-in, zero cloud, works offline. A great place to start.

### 2. Gmail sync — Apple Notes round-trip

Connect your Gmail account (the one your iPhone syncs Apple Notes to)
and Jodd becomes a full desktop client for Apple Notes — create, edit,
and organise notes that show up on your iPhone the next time it syncs.

> ⚠️ **Gmail sync requires your own Google Cloud credentials (BYO).**
> Jodd is open-source and ships without an OAuth client ID. You create a
> free Desktop OAuth client in Google Cloud Console and point Jodd at it.
> See [Build from source](#build-from-source) for instructions.
> Pre-built binaries do not include credentials — you must supply them.

Outlook / Microsoft Graph is on the roadmap.

---

## What round-trips to Apple Notes vs. what stays in Jodd

| Feature | Round-trips to Apple? |
|---|---|
| Note title & rich-text body | ✅ Yes |
| Folder hierarchy (`Notes/Work/Projects`) | ✅ Yes |
| Inline `#hashtags` in body | ✅ Yes |
| Checklists | ✅ Yes (Jodd-authoritative state) |
| Attachments (images, PDFs) | ✅ Yes |
| Pin 📌 | Jodd-only — visible across your Jodd devices, invisible on iPhone |
| `[[wikilinks]]` + graph view | Jodd-only — stored as text in the body; Apple shows plain text |
| AI-extracted notes | Jodd-only — folder lives in Gmail, iPhone ignores it |

*"Jodd-only" means the data is safe and lives in your Gmail or local files —
it just won't render the same way in Apple Notes on iPhone.*

---

## Status

Pre-1.0 beta. The Gmail backend works end-to-end. Major features shipped:

- Full Apple Notes round-trip fidelity (title, body, folders, attachments)
- Conflict resolution (keep-both) when the same note is edited on two devices
- Multi-account — connect several Gmail accounts simultaneously
- Rich text: headings, bold/italic/underline, checklists, ordered & unordered lists
- Inline `#hashtags` with sidebar filtering, rename, and cross-account search
- `[[wikilinks]]` with autocomplete, a connections panel, and a local graph view
- AI-assisted note extraction (paste any text → structured extract note)
- Pin notes, multi-select batch move/delete, recently-deleted restore
- **Standalone Local Folder** — `.eml` vault, no cloud account required

---

## Install pre-built binaries

Download from the [Releases page](https://github.com/BBM-Co-ORG/Jodd-public/releases).

Pre-built binaries are **ad-hoc signed** (macOS) and **unsigned** (Windows).
Your OS will warn on first run — this is expected for an unsigned build:

- **macOS** — "Apple cannot check this app…" → right-click → **Open** → confirm.
  After the first launch, subsequent opens are normal.
- **Windows** — SmartScreen: click **More info → Run anyway**.

If you're not comfortable bypassing these warnings, build from source instead
(same code, signed by your own toolchain).

---

## Build from source

**Requirements:** Rust stable (`rustup`), Node.js ≥ 20

### Standalone use (no Gmail)

```bash
git clone https://github.com/BBM-Co-ORG/Jodd-public
cd Jodd-public
npm install
npm run tauri build
```

### Gmail sync — BYO credentials

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

> The client secret for a Desktop OAuth app is not truly confidential
> ([per Google's own docs](https://developers.google.com/identity/protocols/oauth2));
> PKCE provides the per-flow security on top of it.

---

## Contributing

PRs are welcome — bug fixes especially.

Development happens on a private upstream repository. This public repository
is a periodic sanitized snapshot. Open your PR here; maintainers will
cherry-pick into upstream with attribution.

For security issues, see [SECURITY.md](SECURITY.md) — do not file public issues.

---

## License

[Apache License 2.0](LICENSE).

Jodd is **not affiliated with Apple, Google, or Microsoft**.
