# Jodd — Apple Notes anywhere

Jodd (Thai *จด*, "to jot") is a desktop app that brings **Apple Notes**
to **non-Apple devices** — Windows, Linux, and macOS — by reading and
writing the same email-based backend Apple Notes uses when you connect
a non-iCloud account (Gmail today; Outlook planned).

If you live half in the Apple ecosystem and half outside it, Jodd lets
you keep one set of notes across both worlds without copy-paste or
third-party note apps.

> ⚠️ **Beta software.** Read the [Disclaimer](DISCLAIMER.md) before you
> use Jodd against an account that contains data you care about. The
> disclaimer also covers Jodd's **AI-assisted development origin** and
> the **cleanroom interoperability** approach used for the
> Apple Notes ↔ Gmail wire format — important reading if you intend
> to fork or redistribute.

## How it works

Apple Notes already syncs notes to non-iCloud accounts by storing each
note as an email message in a special `Notes` label/folder, with custom
headers (`X-Uniform-Type-Identifier: com.apple.mail-note`,
`X-Universally-Unique-Identifier: <UUID>`, etc.) and an HTML body.
Folder hierarchy is mapped onto Gmail labels (`Notes/Work/Projects`).

Jodd talks to the **same backend** — the Gmail REST API today, the
Microsoft Graph API tomorrow — so your iPhone, your Mac, and your
Windows machine all read and write the same set of messages. Round-trip
fidelity with Apple Notes is the central correctness goal.

Local storage is a **SQLite cache** acting as the source of truth for
the UI. A 5-second background worker pushes local edits to the remote
and pulls remote changes back. The architecture is described in
[ARCHITECTURE.md](ARCHITECTURE.md).

## Status

Jodd is **pre-1.0**. The Gmail backend works end-to-end (notes,
folders, conflict resolution, multi-account, pin via Jodd-managed
sidecar messages). Microsoft/Outlook is on the roadmap. No mobile
client — use the native Apple Notes app on iPhone/iPad.

## Build from source

You will need:

- **Rust** stable (`rustup` recommended)
- **Node.js** ≥ 20 with `npm`
- A **Google Cloud project** of your own (see below) — you cannot use
  someone else's OAuth credentials.

### 1. Create a Google OAuth 2.0 Desktop client

1. Open [Google Cloud Console](https://console.cloud.google.com/) →
   create a new project (or pick an existing one).
2. **APIs & Services → Library** → enable the **Gmail API**.
3. **APIs & Services → OAuth consent screen** → configure (External,
   add yourself as a test user, request scope
   `https://www.googleapis.com/auth/gmail.modify`).
4. **APIs & Services → Credentials → Create credentials → OAuth client
   ID** → application type **Desktop application**.
5. Note the **Client ID** and **Client secret**. (Google's docs are
   explicit that the secret for Desktop clients is *not* truly secret;
   PKCE on top of it provides the per-flow protection. Jodd uses PKCE.)

### 2. Configure environment

```bash
git clone https://github.com/BBM-Co-ORG/Jodd-public
cd Jodd-public
cp .env.example .env
# Edit .env and fill in GOOGLE_CLIENT_ID + GOOGLE_CLIENT_SECRET
```

### 3. Build

```bash
npm install
npm run tauri build           # release bundle
# or
npm run tauri dev             # development mode with hot reload
```

Bundles land in `src-tauri/target/release/bundle/`:

- **macOS**: `bundle/dmg/Jodd_<version>_aarch64.dmg`
- **Windows**: `bundle/msi/Jodd_<version>_x64.msi`
- **Linux**: `bundle/appimage/jodd_<version>_amd64.AppImage`

A binary you build yourself is **ad-hoc signed** on macOS and
**unsigned** on Windows — your OS will warn the first time you run it.
That is normal for a self-build.

## Install pre-built binaries

Releases are published to the
[Releases page](https://github.com/BBM-Co-ORG/Jodd-public/releases).
Until the project is code-signed, the OS will show:

- **macOS** — "Apple cannot check this app for malicious software."
  Right-click the app → **Open** → confirm. Once allowed the first
  time, subsequent launches are normal.
- **Windows** — "Windows protected your PC" (SmartScreen). Click
  **More info → Run anyway**.

If you are not comfortable bypassing these warnings, **build from
source instead** — the same code, just signed locally by your own
toolchain.

## First-run setup

1. Launch Jodd. The sign-in screen opens a browser window.
2. Choose the Google account that holds your Apple Notes mailbox. (The
   account must already have Notes synced to it — set this up once on
   an iPhone or Mac under **Settings → Notes → Accounts**.)
3. Grant the Gmail scope. Refresh token is stored in your OS keychain.
4. Wait for the index pass to complete (a few seconds for small
   mailboxes, up to a minute for 5k+ notes). Folders appear in the
   sidebar as the index lands.

To add a second account: **Sidebar → + Add account**. Each account has
independent storage; UUIDs are namespaced per account.

## Contributing

PRs are welcome — bug fixes especially.

Development happens on a separate **private upstream** repository.
This public repository is a periodic sanitized snapshot. The workflow
is described in [CONTRIBUTING.md](CONTRIBUTING.md). In short: open
your PR here, the maintainers will cherry-pick the patch into the
upstream repo, and the next snapshot will include it (with attribution).

For security issues, please follow [SECURITY.md](SECURITY.md) — do not
file public issues for security reports.

## License

[Apache License 2.0](LICENSE). See [NOTICE](NOTICE) for attribution.

Jodd is **not affiliated with Apple, Google, or Microsoft**. See the
full [Disclaimer](DISCLAIMER.md).
