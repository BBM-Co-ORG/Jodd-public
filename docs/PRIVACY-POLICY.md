# Jodd — Privacy Policy

> **Superseded by the live site.** This content now lives at
> [jodd.bbmedia.co.th/privacy.html](https://jodd.bbmedia.co.th/privacy.html),
> maintained with the live Jodd website.
> Edit the policy in the website source, not here — this copy is kept only as an in-repo
> reference of what the live page says as of 2026-08-12.

**Last updated:** 2026-08-12

This policy covers **Jodd**, a cross-platform Developer Preview that lets you
view and edit your Apple Notes from Windows, macOS, and Android, by
reading and writing the same Gmail messages that Apple Notes itself creates
when you enable Notes sync for a Google account on your iPhone or Mac.

## 1. What Jodd accesses

When you connect a Google account to Jodd, the app requests the
`https://www.googleapis.com/auth/gmail.modify` OAuth scope. This Google
permission is broader than a single label. Jodd's application logic only
reads and writes messages carrying the Gmail label your Apple
device uses for Notes (`Notes` by default, or a label you've configured) —
plus the sub-labels used for Jodd's own folder structure under it. Jodd
does not read, search, or modify any other part of your mailbox: no other
labels, no Sent/Inbox/Spam contents, no contacts, no calendar.

Jodd never sends email on your behalf and never permanently deletes
messages — deleting a note in Jodd moves the underlying message to Gmail's
own Trash, which behaves exactly like deleting a message from Gmail's own
web interface (recoverable for the standard retention window).

## 2. Where your data is stored

Jodd is **local-first**: everything it reads from Gmail is cached in a
SQLite database file on your own device, inside your OS's normal per-user
app-data directory. Jodd does not operate a note-storage or note-sync server;
the app talks directly to the Gmail API using your OAuth-issued access token.
On Android, Jodd's website participates only in handing Google's authorization
callback back to the app and does not receive note content.

Nothing about your notes — titles, bodies, attachments, tags, or any other
content — is transmitted to BBMedia or to any analytics or crash-reporting
vendor. We do not have a copy of your data. Optional AI features are off by
default; when enabled, note content needed for a request is sent to the
provider the user selects under that provider's terms.

## 3. What we do with the access

The `gmail.modify` scope exists solely so Jodd can:

- **Read** messages under your Notes label, so it can display them as notes.
- **Insert** new messages under that label when you create or edit a note
  (Gmail's API has no in-place "update," so an edit is technically a new
  message insert followed by trashing the old one — this is invisible to
  you and to Apple Notes, which only sees the final state).
- **Modify labels** on those messages, so that Jodd's folder view (backed by
  Gmail labels, e.g. `Notes/Projects`) can create, rename, and move notes
  between folders.

BBMedia does not use this access for anything else: no advertising, no profiling,
no resale, no training of any model on your note content.

## 4. Credentials and authentication

Jodd uses OAuth 2.0 with PKCE (RFC 7636) to obtain access. Your refresh
token is stored in your operating system's protected credential store (macOS
Keychain, Windows Credential Manager, or Android secure storage) under a
per-account entry — never in a plain file, and never sent anywhere except
directly to Google's token
endpoint to obtain a fresh access token.

## 5. Revoking access

You can disconnect Jodd from your Google account at any time:

- **From Jodd:** remove the account from the account list in the sidebar.
- **From Google directly:** visit
  [myaccount.google.com/permissions](https://myaccount.google.com/permissions),
  find Jodd, and remove its access. This immediately invalidates Jodd's
  refresh token; Jodd can no longer read or write anything in your account
  afterward.

Removing an account from Jodd does not delete your notes from Gmail or from
Apple Notes — it only stops Jodd itself from syncing with that account.
You can remove data already cached locally on your device by clearing Jodd's
local app data; whether uninstalling also removes it depends on the operating
system.

## 6. Children's privacy

Jodd is not directed at children and we do not knowingly collect data from
anyone who does not already have their own Google account, per Google's own
account-age policies.

## 7. Changes to this policy

If this policy changes, the "Last updated" date above will change and, for
any material change, we'll surface a notice inside the app.

## 8. Contact

Questions about this policy or about Jodd's data handling:
**kaiwan@bbmedia.co.th**
