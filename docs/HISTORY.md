# Jodd — project history (archive)

> Moved out of `CLAUDE.md` on 2026-08-10. This file is the **record of what
> was built and why**; `CLAUDE.md` keeps only what still changes how you should
> work on the code today. Nothing here was edited — sections are verbatim.
> Read this when you need the reasoning behind a design that already shipped.

---

## 2026-08-17 — M4 pin sidecar: `list_pinned` died on a null string (fixed)

The first defect M4's Pin sidecar hit against a **real** mailbox rather than a
fixture. Observed in `~/Library/Application Support/jodd/logs/jodd.log`, on
every sync tick:

```
sync_worker: push dirty uuid=... failed: permanent: failed to decode Graph
list_pinned: invalid type: null, expected a string at line 1 column 20484
```

`permanent:`, so the affected push retried forever and never succeeded — not a
transient blip that would clear itself.

**`#[serde(default)]` does not mean null-tolerant, and that is the whole bug.**
`default` supplies a value when the KEY IS ABSENT. An explicit `"field": null`
still reaches the `String` visitor and fails the entire page. Graph types most
message properties as nullable and really does send `null` rather than omitting
the key, so every `#[serde(default)] field: String` in `microsoft/wire.rs` was a
latent copy of this defect.

**Why only `list_pinned` met it — the structural reason, worth keeping.**
`pin_scan_url` is the module's ONLY unfiltered whole-mailbox read. Every other
scan is narrowed to notes before the page is ever decoded: `messages_url`
carries `sticky_note_filter`, `folder_notes_url` is scoped to a folder holding
nothing but notes. So the pin scan is the one decoder that sees **ordinary
mail** — shapes a note never has. That is a property of the scan, not of this
one field: **anything added to `pin_scan_url`'s `$select` must be assumed
nullable for the same reason.**

The scan is unfiltered deliberately (see `pin_scan_url`'s doc comment: combining
a `$filter` on one property with an `$expand` on a *different* one has never
been measured, and gotcha #12's proven combination is filter-and-expand on the
SAME property). That reasoning still stands — the fix is null-tolerance in the
decoder, not a filter on the query.

**Fix:** one shared helper, `null_as_empty_string`, applied via
`#[serde(default, deserialize_with = ...)]` to the nullable Graph properties:

| Field | Graph property | `null` now means |
|---|---|---|
| `RawRow.internet_message_id` | `internetMessageId` | no identity → row dropped |
| `RawExtProp.value` | `singleValueExtendedProperties[].value` | no value → not pinned |
| `RawMessage.internet_message_id` | `internetMessageId` | no identity → counted in `skipped_without_identity` |

Each of those is the behavior the empty-string case ALREADY had, so nothing
changes except that the page stops failing. `RawExtProp` is shared by
`decode_pin_page`, `decode_message_classes` and `decode_first_folder_name`, so
the fix lands once rather than in three copies — the exact defect shape that
struct was extracted to prevent.

**Null-tolerant, not type-tolerant.** The helper routes through
`Option<String>`, so `null` becomes `""` while a number or object in the same
position still fails to deserialize, with a test pinning that distinction.
`null` is Graph saying "this property has no value", which every reader here
already handles correctly; a wrong TYPE is Graph — or something in front of it
— returning a shape this module does not understand, and silently defaulting
that would turn a real protocol change into quietly-missing notes and pins
instead of a loud error.

**Which field the live payload actually carried was never established, and the
obvious explanation is not as clean as it looks.** No response body is logged
and the session had no token, so both candidates were fixed rather than one
being proven. The tempting story is "a draft, which has no `internetMessageId`"
— but **this account's notes are themselves `isDraft: true`** (see the 2026-08-14
investigation below) *and* carry a perfectly good `internetMessageId`, so
"draft ⇒ null identity" is not a rule that holds here. What remains true is
narrower: `put_pin` only ever writes a well-formed, non-empty JSON string, and
only onto notes, so a null `JoddPin` **value** needs something other than Jodd
to have created it — which is why `internetMessageId` on some non-note item
remains the better guess. It is a guess.

Verified: `cargo test --workspace` → 620 passed, 0 failed, against fixtures
carrying an explicit `null` in each position. **Not verified live** — the real
confirmation is that log line disappearing on the next run against
`kaiwan.h@live.com`. If it persists, the null is in a field this fix did not
cover and the error's column offset will have moved.

---

## Current status (2026-08-16 — Microsoft/Outlook vertical investigation)

Moved out of `CLAUDE.md` on 2026-08-16 to keep that file to "what still changes
how you work on the code today" — this is the full forensic record behind the
condensed summaries CLAUDE.md now carries in "How Apple Notes ↔ Outlook.com
works" and gotcha #12. Nothing below was edited from what CLAUDE.md said at the
time; only the two live-investigation additions from this exact session (the
EWS documentation check and the Mac-vs-iPhone folder-nesting screenshots) are
appended at the end, in the order they happened.

### Microsoft Outlook vertical investigation (2026-08-14 to 2026-08-16)

Investigated end-to-end on 2026-08-14 against a live `@live.com` account; every
claim below was executed, not read. **This is a different protocol family from
Gmail, not the same trick against another host.**

The account is an **Exchange** account (macOS Internet Accounts shows the Exchange
icon and offers Mail/Contacts/Calendars/**Reminders**/Notes — Reminders is
impossible over IMAP). Notes land in an `IPF.StickyNote` container named `Notes`,
which Outlook.com renders with its own Notes UI at `outlook.live.com/mail/notes`.

What that changes:

- **None of Apple's `X-` headers exist.** Not in `$value`'s MIME (Exchange
  synthesises ~1 KB of its own), not in `internetMessageHeaders` (absent, not
  empty), not as MAPI named properties under `{00020386-…}`. EAS Notes carries a
  structured object, so the headers were never sent. `mime822.rs` has nothing to
  build or parse here.
- **Identity is `internetMessageId`,** verified stable across an Apple-side edit,
  a Graph-side `PATCH`, and an overwrite+undo cycle. (Graph's `id` also held, but
  it is documented to change on folder move unless `Prefer: IdType="ImmutableId"`.)
- **`PATCH` is a real in-place update.** No insert-new + trash-old, no `mark_pushed`
  id repair — the Gmail dance below does not apply.
- **Round trip is proven both ways:** an Apple edit appears in Graph with identity
  intact, and a Graph `PATCH` appears in Notes.app **and on iPhone** with no
  duplicate and the title unharmed.
- **Rich text arrives as clean semantic HTML** — `<b> <i> <u> <strike>`, correctly
  nested, no inline styling. The only style attribute is
  `line-break:after-white-space` on `<body>`. `AppleHtmlDeriver` should work as-is.
- **Attachments are impossible.** Apple itself refuses: "Exchange accounts don't
  support attachments." See the compatibility tiers.
- **Folders cannot be enumerated** — see the gotcha #12 log below.

**Creating a note works, but the item class is the whole trick.** A note is
`IPM.StickyNote` and an email is `IPM.Note` (`PR_MESSAGE_CLASS`, `0x001A`); notes
also carry `isDraft: true`. Graph's `POST` defaults to `IPM.Note`, so the class has
to be set explicitly at creation:

```
POST /me/mailFolders/{notesFolderId}/messages
{
  "subject": "graphpost-0345",
  "body": { "contentType": "HTML",
            "content": "<html><body>graphpost-0345<div>created by graph post</div></body></html>" },
  "singleValueExtendedProperties": [
    { "id": "String 0x001A", "value": "IPM.StickyNote" }
  ]
}
```

→ 201, and within ~60 s the note appears in Notes.app in the right folder with the
title/body split correct (title as the leading bare text node, per gotcha #11).
**Not tested: whether omitting the class actually fails** — only that setting it
works. Check before treating it as required.

**Query one extended property at a time.** An `$expand=…($filter=id eq 'String
0x0E05' or id eq 'String 0x001A')` returns **200 with no properties at all** rather
than an error, so an `or` reads exactly like "the property does not exist". Both
work fine when asked for singly. This wasted a cycle on 2026-08-14.

**The mechanism behind that, now measured (Task 8, still 2026-08-14, live account,
`scripts/ms_probe_filter.py`):** Graph's `singleValueExtendedProperties` matching
requires the `any()` lambda to pair exactly **one property id with one value
restriction** — no more, no fewer. `$expand` answers a violation of that rule with
the silent 200-and-nothing above; **`$filter` answers the same violation with a
400** instead — *"The filter expression for `$filter` does not match to a single
extended property and a value restriction"* — which is what an id-only `$filter`
(no `and ep/value eq …`) gets back. Pair the id with a value and ask for exactly
one property, and **`$filter` works**: `$filter=singleValueExtendedProperties/any(ep:
ep/id eq 'String 0x001A' and ep/value eq 'IPM.StickyNote')` returned 200 with
exactly the note subset (13 of 50 messages, matching the client-side baseline),
unchanged when combined with the `$expand` above in the same request, and still
pages correctly (`$top=2` still returned `@odata.nextLink`). The Microsoft
vertical now sends this on the pass-A `/me/messages` scan
(`messages_url`/`sticky_note_filter` in `backend/microsoft/wire.rs`) so Graph
narrows the whole mailbox — bodies included — before it crosses the wire, instead
of every ordinary email being downloaded just to be discarded by
`keep_sticky_notes`. **`keep_sticky_notes` stays regardless** — belt and braces,
deliberately: the `$filter` is an optimisation Graph could stop honouring without
warning, the client-side check is certain either way. The scoped
`/me/mailFolders/{id}/messages` read (`folder_notes_url`) deliberately does NOT
carry this filter — a Notes-tree folder already contains nothing but notes, so
there is nothing left to narrow.

**Deleting a note is a HARD delete, and there is no undo path — measured, not
inferred (2026-08-14).** A note deleted in Notes.app on the Mac vanished from
Jodd on the next poll, and the mailbox afterwards showed: 15 `IPM.StickyNote`
items via `/me/messages` (matching the UI), **0** of them with
`parentFolderId` equal to the `deleteditems` id, the deleted note **absent from
the scan entirely**, and `GET /me/mailFolders/deleteditems/messages` returning
**0 items**. So an Apple-side delete does not soft-delete into Deleted Items the
way an ordinary mail delete does — nothing is left anywhere Graph can see.

Two things follow. First, `Capabilities::has_trash = false` for this backend is
now backed by direct evidence rather than by the absence of a visible trash:
there is genuinely nothing to restore from, so **never offer an undo or a
Recently Deleted view here**. Second, the Deleted-Items exclusion in
`scan_messages` never fires on this path — keep it anyway (it costs one
well-known-folder lookup and guards a behaviour Microsoft could change), but do
not mistake it for the reason deleted notes stay gone.

`DELETE /me/messages/{id}` → `204`, and the note disappears from Notes.app within
~60 s (folder and account counts both drop). **CRUD is complete and proven end to
end.** Graph's own `DELETE` is documented as a soft delete to Deleted Items, but
that is beside the point for Jodd: the measurement above shows Apple's delete
leaves nothing there, so there is no undo path to offer either way.

**Auth is proven too — the door opens for a personal Microsoft account.** A public
client (no secret, PKCE, loopback `http://localhost:8765`) registered in the BB Media
tenant as multitenant + personal accounts, requesting delegated `Mail.ReadWrite` +
`offline_access` + `User.Read`, got consent from `kaiwan.h@live.com` and returned a
working token: `GET /me` → 200, `GET /me/messages` → 200 with every note folder
resolved by name. **A refresh token was issued**, so background sync is possible.

Two things the Azure portal will make you think are blockers and are not — for
consumers:

- The portal shows *"End users cannot grant consent to newly registered multitenant
  apps without verified publishers."* That policy is an **Entra** one. A personal
  Microsoft account consents through `account.live.com/Consent/Update`, a different
  surface, and it **allowed the grant**. Do not buy an MPN ID before testing.
- The consent screen does display an **`unverified`** badge next to the app name.
  Not blocking, but every user sees it — publisher verification is a
  release-polish item, not a prerequisite.

A **work/school account** (`kaiwan@bbmedia.in.th`, Microsoft 365 Business) consented
too — same `unverified` badge plus *"This application is not published by
Microsoft"*, and `Accept` was available without ticking **Consent on behalf of your
organisation**. Token worked: `GET /me` → 200, `GET /me/messages` → 200. So the
Entra "verified publisher" policy did not block here either.

**Read that result narrowly.** Two conditions made it the easy case, and neither
holds for a real Jodd user: the app is registered **in that same tenant**, and the
consenting user is a **tenant admin** (the org-consent checkbox only renders for
admins). The case that still matters — a **non-admin user in a different M365
tenant** — needed an outside account to test, and now has one.

### The outside-tenant case, measured 2026-08-17: it fails, permanently

`jodd@renny.co.th`, a **non-admin user in an outside Microsoft 365 tenant**,
**cannot consent.** Microsoft shows a **"Need admin approval"** page — *"Jodd
needs permission to access resources in your organization that only an admin
can grant"* — offering only "Have an admin account? Sign in with that account"
and "Return to the application without granting consent". No token, no
account added.

**This is a platform property, not a defect.** Microsoft's own guidance
(learn.microsoft.com/entra/identity/enterprise-apps/configure-user-consent)
recommends tenants restrict user consent, and delegated `Mail.ReadWrite` is
not classified low-impact — so a well-run M365 tenant is *expected* to land
here. Publisher verification would not change it either: this is the consent
*policy*, not the `unverified` badge.

**The finding that shaped the code — the refusal is indistinguishable from a
cancel.** Taking the "Return to the application" route produced exactly this
redirect, in full:

```text
http://localhost:8080/?error=access_denied&error_subcode=cancel&state=QljKk8On4aTpcb0O
```

**No `error_description`, no AADSTS code** — byte-identical to the same user
simply pressing Cancel on an ordinary consent screen. Microsoft's docs show an
`error_description` in most examples, so the obvious design (branch on the
AADSTS code) would branch on a field that never arrives. Jodd therefore does
**not** classify: `signin_denial` in `lib.rs` names *both* causes and always
offers the admin-consent link, and `auth::CallbackDenial` carries the comment
saying why. Anyone "improving" this into a classifier is reintroducing the
bug.

**What Jodd does about it (2026-08-17).** The desktop loopback listener used
to throw the whole refusal away — `param("code").ok_or(...)` discarded
`error`, `error_subcode` and `state` in one line, returned before answering
the browser (so the tab hung blank), and emitted a generic string into a store
(`$error`) that **is only rendered inside `NoteEditor`** — a component not
mounted while `AuthScreen` is showing. The net effect on first-run sign-in was
nothing at all on screen. Now: the listener reports the refusal as an outcome
rather than an error, answers the browser either way, and a `SignInBlocked`
panel (rendered above App.svelte's `isAuthenticated` branch, so it serves both
first-run and Add Account) offers the tenant admin-consent URL
`https://login.microsoftonline.com/organizations/adminconsent?client_id={MS_CLIENT_ID}`
with Copy link / Copy message for IT.

### The remedy works — measured end to end 2026-08-18

The admin-consent URL was doc-derived for one day. It is now proven, in one
continuous run:

1. `admin@renny.co.th` (tenant admin) opened
   `https://login.microsoftonline.com/organizations/adminconsent?client_id=f95a0627-…`
   and got **"Permissions requested — Review for your organization"**, listing
   read/write user mail, maintain access, sign in and read profile, with
   *"this app will get access to the specified resources for all users in your
   organization"*.
2. Accept → Microsoft answered `?admin_consent=True&tenant=abb8a2f4-…`.
3. `jodd@renny.co.th` — the same non-admin refused hours earlier — signed in to
   Jodd and **succeeded**: `complete_oauth: token exchange OK (refresh_token
   present=true)`, account added, notes list loaded.

**Two traps came out of it, neither guessable from the docs:**

- **Omitting `redirect_uri` does NOT show a Microsoft confirmation page.**
  Microsoft redirects to one of the app's *registered* redirect URIs, and it
  picked `http://localhost:8765` — the M1 probe's port, not the `:8080` this
  vertical uses. Nothing listens there, so the admin's browser shows
  `ERR_EMPTY_RESPONSE` **after the grant has already been recorded**. An admin
  who is not warned reports a success as a failure; `adminRequestText`
  (src/lib/signInBlocked.ts) warns them, with a test pinning it.
- The `unverified` badge plus *"This application is not published by Microsoft
  or your organization"* both render on the admin's screen. Neither blocks.

**What still does not work for that account, and why it is unrelated.** The
first note created in the freshly-added `jodd@renny.co.th` fails to push with
`permanent: no Exchange folder id for 'Notes'`. That is gotcha #12, not a
consent problem: folder ids are only reachable via `parentFolderId` on a
message, so a mailbox with zero notes has no discoverable `Notes` folder and
the *first* note can never be filed. Seeding one note from Apple Notes on that
account makes the folder discoverable and the queued push drains.

So: an ordinary M365 user cannot sign in until their IT grants tenant-wide
consent — and once they do, sign-in works.

**2026-08-17 — that outside-tenant test was designed and is awaiting a result;
two things about it were settled without needing the account.** The question
came up because release binaries embed Gmail's credentials at build time
(`build.rs`'s `KEYS`) but never `MS_CLIENT_ID`, which `auth_ms.rs` reads from
the environment only — so a downloaded build cannot sign in to Microsoft at
all. Whether to embed it hinges on this consent test, not on the plumbing.

First settled point: **the registration is multitenant, and that is derivable
from evidence already in this file rather than from the Azure portal.** A
personal `@live.com` account signed in, which in Entra requires
`signInAudience = AzureADandPersonalMicrosoftAccount` — the widest setting.
This matters because it pre-empts a misreading: a single-tenant registration
would answer an outside user with `AADSTS50020` ("user account does not exist
in tenant"), a *different* failure from a step-up-consent block, and the
screenshot would have been scored as a policy rejection.

Second: **the test needs no Jodd build and grants nothing.** The consent gate
is a pure browser flow — Microsoft renders either a consent page or a block
page *after* the user authenticates, because it must know the tenant before
applying the policy. So a hand-built authorize URL sent to any outside
non-admin user, plus a screenshot of the page that follows sign-in, is the
entire measurement; `Accept` never has to be clicked. Use `8080` as the
`redirect_uri` — both it and `8765` are registered, and an unregistered one
fails with `AADSTS50011` before any consent page renders, yielding a
screenshot that proves nothing.

**Score a negative result carefully — unlike a positive one, n=1 cannot
attribute it.** Consent succeeding is decisive for that class of account.
Consent being blocked has two causes that look nearly identical on screen:
risk-based step-up consent rejecting an unverified publisher (publisher
verification would fix it), or the tenant having disabled user consent for all
third-party apps, a common enterprise default (publisher verification would
change nothing). The exact page wording and any `AADSTS` code are the evidence
that separates them, and **two different tenants are worth far more than one.**
**Do not buy an MPN ID before the test** — unchanged from 2026-08-14.

**2026-08-17 result — consent was NOT blocked in an outside tenant, and the
publisher-verification policy did not fire.** `thearchitect@renny.co.th`
(Microsoft 365, `renny.co.th` — a tenant unrelated to BB Media) was shown the
ordinary consent page: the `unverified` badge, *"This application is not
published by Microsoft or your organization"*, the three requested scopes, and
an enabled **`Accept`**. Accept was clicked **without** ticking *Consent on
behalf of your organization*, and Microsoft redirected to
`http://localhost:8080/?code=1.AUkA…` — `ERR_CONNECTION_REFUSED`, which is the
expected and correct outcome for a browser-only test with no listener bound.
**A code was issued, so the grant completed.** So risk-based step-up consent
either is not enabled in that tenant or does not block this app there, and the
tenant's user-consent setting permitted delegated `Mail.ReadWrite` — consistent
with Microsoft's documented default, which names mailbox access as the very
example of a permission users may consent to themselves
(`entra/identity/enterprise-apps/configure-user-consent`). The stricter
`microsoft-user-default-low` policy ("verified publishers only, low-impact
permissions") is Microsoft's *recommended hardening*, not the default — worth
knowing, because a tenant that has adopted it would block this app regardless
of who signs in.

**One confound was cleared, the other was not — score this as a partial
result.** The *different tenant* condition is now genuinely satisfied, which
neither earlier test managed. But the screenshot shows the **Consent on behalf
of your organization** checkbox rendered, and that checkbox is the UI for
tenant-wide admin consent, an operation
`entra/identity/enterprise-apps/grant-admin-consent` restricts to Privileged
Role / Cloud Application / AI / Application Administrator or an equivalent
custom role. **The account owner confirmed it: `thearchitect@renny.co.th` was
an admin in that tenant at the time of the test** — the same admin confound
that disqualified `kaiwan@bbmedia.in.th`, now reproduced in a foreign tenant.
An admin may well be allowed past a step-up-consent gate that would stop a
plain user, so **this does not yet license telling a customer their staff can
sign in.** The decisive follow-up is a non-admin identity in `renny.co.th`
opening the same authorize URL.

**When you run that follow-up, two things will silently invalidate it.** First,
**consent is already granted for `thearchitect@`** — replaying the URL as that
same user shows no consent screen at all, just an immediate redirect carrying a
code, because the grant is recorded. That looks exactly like a pass and proves
nothing. The grant must be revoked first (`myapps.microsoft.com` → the app →
Revoke permissions, or delete the `Jodd` service principal from the tenant's
Enterprise applications, which clears every grant and forces re-provisioning).
Second, **demoting that account rather than using a fresh one is the riskier
route**: role changes need a full sign-out to leave the session's claims, the
user-consent grant survives the demotion regardless, and if it is the tenant's
only admin, demoting it can strand administrative access. A newly created
role-less user in `renny.co.th` avoids all three problems and needs no licence
for the consent measurement — though it would need an Exchange licence before
the `GET /me/messages` half could ever run.

**2026-08-17, the answer that was missing since 2026-08-14 — a non-admin user
in an outside tenant is BLOCKED.** `jodd@renny.co.th`, a role-less account in
the same foreign tenant, opened the identical authorize URL in a private window
and got **"Need admin approval"**:

> Jodd needs permission to access resources in your organization that only an
> admin can grant. Please ask an admin to grant permission to this app before
> you can use it.

with only *"Have an admin account? Sign in with that account"* and *"Return to
the application without granting consent"* offered — no `Accept`, no
`AADSTS` code on the page. That link is itself the proof the account was
non-admin, so this test did not depend on trusting a role claim. **Both earlier
successes are now explained: they were admin grants, not user grants.** Every
condition Microsoft's publisher-verification policy names was met and the
consent was refused, exactly as the policy predicts.

**Read the negative narrowly too — one tenant, and the wording does not name
the cause.** The block page is the generic admin-consent-required page; it is
identical whether the cause is risk-based step-up consent rejecting an
unverified publisher (which publisher verification would fix) or
`renny.co.th`'s own user-consent setting refusing `Mail.ReadWrite` from any
third party (which it would not). Distinguishing them takes one look at
**Entra ID → Enterprise apps → Consent and permissions → User consent
settings** in that tenant: `microsoft-user-default-legacy` ("Allow user consent
for apps") points at step-up consent as the cause; `microsoft-user-default-low`
or user consent disabled means verification alone changes nothing there,
because `Mail.ReadWrite` would additionally have to be classified *low impact*
by that tenant's own admin.

**The load-bearing consequence, whichever it is: Microsoft support for
corporate M365 accounts routes through the customer's IT, permanently.**
Microsoft's own guidance in `configure-user-consent` recommends restricting
user consent to verified publishers, so a large share of enterprise tenants
will require admin consent for `Mail.ReadWrite` regardless of anything Jodd
does. That is a platform property, not a Jodd gap, and it should be stated to
users rather than discovered by them. The constructive path is the
tenant-admin consent URL —
`https://login.microsoftonline.com/{tenant}/adminconsent?client_id={id}` — which
a blocked user can hand to their own IT; Entra also has an admin-consent
request workflow a tenant can enable. Neither needs publisher verification.

**None of this bears on whether `MS_CLIENT_ID` should be embedded, and that is
the point most easily got wrong here.** Consent policy is evaluated per tenant
against the app's identity; it is indifferent to whether the client id arrived
in the binary or from the environment. Not embedding does not help the blocked
enterprise user at all — it only additionally breaks the users who *can*
consent: personal Microsoft accounts (which consent through
`account.live.com`, outside Entra policy entirely — proven 2026-08-14) and
admins in any tenant (proven twice). For a notes app whose users hold personal
Outlook accounts, that is the primary population, and today they cannot sign in
from a downloaded build at all.

**2026-08-17 — the Microsoft vertical is proven to work against a tenant that
is not the app's home tenant, for everything except the first write.**
`TheArchitect@renny.co.th` was added live in a dev build. Consent, the token
exchange, `GET /me` (the address came back with its original casing) and the
mailbox scan all succeeded — the first time any of this was measured outside
`kaiwan.h@live.com` (personal) or `bbmedia.in.th` (the app's own tenant). The
account appears in the sidebar alongside an existing Microsoft account, so **two
Microsoft accounts in one install also works.**

**Note creation does not, and the reason is gotcha #12 read from the write
side — a bootstrap deadlock nobody had written down.** The mailbox is a fresh
one that has never been used with Apple Notes, so the scan reports
`1 page(s), 0 note(s)`. With no message in the mailbox there is no
`parentFolderId`, hence no Exchange folder id for `Notes`, and `push_one_dirty`
refuses:

> `push dirty uuid=71CFDC1C… failed: permanent: no Exchange folder id for
> 'Notes' — the folder must exist before a note can be filed into it`

So **a brand-new Microsoft account can never receive its first note from
Jodd**: finding the folder requires a note, and creating a note requires the
folder. Two notes sat dirty and retried every five seconds; the log contains
zero successful Microsoft pushes across the whole session. Outlook on the web
showed the `Notes` folder empty, which is not a sync anomaly — it is correct,
because nothing ever left the device. **This is not about the tenant, the admin
role, or consent**; the same deadlock would hit an empty personal mailbox.

**The UI said "Saved" throughout, and that is the more serious half.** The error
string says `permanent:` — the worker knows it will never succeed — yet the note
renders in the list like any other and nothing reaches the user but a log line
repeating every five seconds. A local-first UI is allowed to acknowledge a write
before the remote confirms it; it is not allowed to report success for a write
it has already classified as permanently unsendable. Anything that surfaces this
must read the queue rather than the sync state, the same distinction gotcha #2
draws for `Inactive`.

A separate permanent failure appeared in the same log and is unrelated to the
deadlock: `failed to decode Graph list_pinned: invalid type: null, expected a
string` — the pinned-property decode rejects a null where it expects a string,
which fails that push permanently too.

**A third finding came out of diagnosing the first two: `microsoft scan:` log
lines carry no account name, and Gmail's equivalents do.** Gmail brackets the
address into every line (`save_note[kaiwan@bbmedia.co.th]`,
`list_notes_in_label[kaiwan@bbmedia.co.th]`); the Microsoft scan prints only
`microsoft scan: 1 page(s), N note(s)`. With three Microsoft accounts signed in,
the worker's concurrent `list_notes` calls interleave those counts in one stream
with nothing to attribute them by — and a `26 note(s)` line belonging to
`kaiwan.h@live.com` was misread here as evidence that the empty mailbox had
suddenly filled, which briefly cast doubt on a correct diagnosis. The cost of the
missing bracket is a wrong conclusion, not untidy output.

The natural control experiment fell out of the same session: `kaiwan@bbmedia.in.th`
was added minutes later and scans `1 note(s)`, so its folder id resolves and it can
write. The only variable separating it from `renny.co.th` is whether the mailbox
already held a note — not the tenant, not the admin role, not consent.

**Live verification landed 2026-08-15, split down the middle.** The note path
passed outright: `cargo run --example ms_write_probe` drove create / patch /
retitle / move / delete against `kaiwan.h@live.com`, every step returned 2xx,
and Notes.app confirmed the retitled note in the Notes root plus the
empty-title note rendering correctly. Nothing about note writes needs to
change, and `Capabilities::for_backend(Microsoft).writes.notes` is `true`.

**Folder writes do not work, and as of M3 this is proven at the mechanism
level, not just "still absent."** Two folders created through Graph were
still absent from Notes.app 21 hours later and after a forced account
resync — ruling out sync lag. The control: two other folders created **by
hand** in Notes.app were visible the whole time, and notes Graph wrote
*into* those hand-made folders reached Apple fine — so the variable is the
folder's provenance, not who writes the note. The mechanism, in the order it
was found:

1. Microsoft's docs list the supported folder-write surface as
   `POST /me/mailFolders` and `PATCH /me/mailFolders/{id}`. The call Jodd
   needs to nest a folder under Notes, `POST /me/mailFolders/{id}/childFolders`,
   is not in that list — and it shows: the call returns **201** but silently
   drops the extended property that would mark the folder as a Notes
   container.
2. Trying to fix that after creation confirms the class is immutable:
   `PATCH /me/mailFolders/{id}` with `PR_CONTAINER_CLASS` →
   **`500 ErrorObjectTypeChanged`**, *"Operation would change object type,
   which is not permitted."* This is the decisive response — the container
   class **is** the object type, and Graph refuses to change it after the
   fact.
3. The documented creation path, `POST /me/mailFolders`, does work (**201**)
   but creates at mailbox **root**, not nested under Notes, and still echoes
   no extended properties back.

**M3 (2026-08-15, live, `scripts/ms_folder_move_probe.py`) closed the one
avenue M2 left untried:** create at root via `POST /me/mailFolders` with
`PR_CONTAINER_CLASS` set at creation, then `POST /me/mailFolders/{id}/move`
under the Notes root. Both requests returned 2xx and a witness note was filed
into each. Neither folder ever appeared in Notes.app — confirmed
independently on **both** Mac and iPhone, with `outlook.live.com`'s own Notes
UI as a third observer showing both the whole time as legitimate children of
`Notes` (so this is a genuine Apple-side rejection, not sync lag, a Jodd bug,
or a Graph-side failure — Graph and Outlook both agree the folders are real).
A forced account resync (toggling the account off/on) did not surface them
either.

**Reading the folders back afterward found the actual mechanism, not just
another absence.** `GET /me/mailFolders/{id}` works on these two — unlike
genuine Notes-tree members, they don't 404 (M3 addendum below) —
and it showed `PR_CONTAINER_CLASS` had come back as **`IPF.Note`** on *both*
folders, including the one whose creation payload explicitly set
`PR_CONTAINER_CLASS: IPF.StickyNote`. So `POST /me/mailFolders` silently
drops the class exactly like `childFolders` does. Combined with finding 2
above (the class is immutable after creation), this closes the question
completely: **no sequence of Graph API calls can create a folder classed
`IPF.StickyNote`**, and Apple's Notes sync almost certainly filters folders
on that class the same way it filters messages on `PR_MESSAGE_CLASS`.
(`isHidden` was checked and ruled out too — both folders read back
`isHidden: false`, Jodd never sets it, and the folders were visibly unhidden
in Outlook's own UI throughout.)

`Capabilities::for_backend(Microsoft).writes.folders` is therefore `false`
— off is the correct, **permanent** state: not a stopgap, and not deferred
to any future milestone. Folder create/rename/delete stay unavailable on this
backend for good.

### Gotcha #12 full investigation log

**Graph can use an Outlook.com folder but cannot find one.** `/me/mailFolders`
omits the `Notes` tree entirely — same result on v1.0, on beta, via
`msgfolderroot/childFolders`, and with `includeHiddenFolders=true`; `notes` is
also absent from `wellKnownFolderName`. The behaviour is **undocumented**: the
`user-list-mailfolders` page names only two exclusions (hidden folders, non-root
folders) and both were tried, so do not go hunting for a query parameter. The
mechanism shows up in a sibling API — `/admin/exchange/mailboxes/{id}/folders`
exposes a `type` field (`IPF.Note`, `IPF.Appointment`) and filters on it — but
that endpoint is "Not supported" for personal Microsoft accounts and needs admin
consent otherwise.

Exactly what an id does and does not buy you, all measured:

| request | result |
| --- | --- |
| `GET /mailFolders/{id}` | **404** — for the `Notes` folder *and* user folders alike. The folder object is never retrievable, so `childFolderCount` is unreadable |
| `GET /mailFolders/{id}/messages` | 200, correct notes |
| `GET /mailFolders/{id}/childFolders` | 200 but **always empty** |

**M3 addendum (2026-08-15): that 404 is specific to genuine native
Notes-tree members, not to "anything Graph will place under Notes."** Two
folders M3 created via `POST /me/mailFolders` and then moved under the
Notes root with `POST /me/mailFolders/{id}/move` (see "Folder writes do
not work" above) returned a full **200** on `GET /mailFolders/{id}`,
including `singleValueExtendedProperties`. The distinguishing factor is
very likely container class: these two came back classed `IPF.Note` (an
ordinary mail folder, not `IPF.StickyNote`) — which is also the reason
they never reached Notes.app. So the 404 row above still holds for the
real `Notes` tree, but do not assume it holds for every folder whose
`parentFolderId` happens to point at Notes — provenance and class matter,
not merely nesting.

That last row is the one that matters: `childFolders` on `TEST TEST` returns `[]`
**even with `includeHiddenFolders=true`, and even though `TEST TEST L2` (1 note)
is visibly its child in Notes.app.** Children are filtered exactly as the root
listing is, so **you cannot walk the tree down from a folder you already know**.

**The only route to an id is `parentFolderId` on a message**, which makes the gap
precise: **a folder holding zero notes cannot be discovered at all** (`New Folder`
in the test account is exactly this). An empty folder created on the iPhone will
not appear in Jodd until something is filed into it — that half of "the handling"
stays undecidable, because there is no id to query in the first place.

#### Correction 2026-08-18: emptiness is NOT why the folder is hidden

The sentence above is right about the consequence and **wrong about the
mechanism**, and the wrong mechanism sends the next reader hunting for a way
to "list empty folders" — a problem that does not exist.

The control that settles it: `kaiwan.h@live.com` was deliberately emptied to
**zero notes**, leaving only its `Notes` folder, which Outlook.com shows and
refuses to delete (Delete is greyed out — a system folder). Every folder
surface was then asked directly, and all five returned the *same* eight
folders:

```
Archive (1)   Conversation History (0)   Deleted Items (0)   Drafts (0)
Inbox (3199)  Junk Email (0)             Outbox (0)          Sent Items (11)
```

**Five of those hold zero items and every one of them appears.** `Notes` does
not — on v1.0, v1.0 + `includeHiddenFolders`, beta, beta + `includeHiddenFolders`,
or `msgfolderroot/childFolders`. So Graph does not hide empty folders at all;
it filters the whole Notes tree out **by container class (`IPF.StickyNote`),
regardless of item count**. A `Notes` folder with 500 notes in it is exactly as
invisible to `/me/mailFolders` as an empty one.

The accurate statement is therefore:

> The Notes tree is excluded from every folder-listing surface, always. The
> only working route to a Notes folder id is `parentFolderId` on a message —
> so a Notes folder with no messages is undiscoverable, **not because empty
> folders are hidden, but because the one route that works needs a message.**

Two new results from the same run, neither previously recorded:

| request | result |
| --- | --- |
| `GET /me/mailFolders?$filter=displayName eq 'Notes'` | **200 with an empty list** — a server-side name filter cannot reach it either |
| `GET /me/mailFolders/notes` (wellKnownFolderName) | **404** `The specified object was not found in the store.` |

Reproducing it needs no code — an access token and:

```bash
for u in \
  "me/mailFolders?\$top=100" \
  "me/mailFolders?includeHiddenFolders=true&\$top=100" \
  "me/mailFolders/msgfolderroot/childFolders?\$top=100" \
  "me/mailFolders?\$filter=displayName%20eq%20'Notes'" \
  "me/mailFolders/notes" ; do
  curl -s -H "Authorization: Bearer $TOKEN" "https://graph.microsoft.com/v1.0/$u" \
    | jq -r '.value[]?.displayName // .error.message'
done
```

**A throwaway Rust probe did this first and was deleted on purpose**: it would
have been compiled by `cargo test --workspace` forever, and every fresh build
of it costs a macOS keychain password prompt (the ACL is per-binary), which
`curl` avoids entirely. The findings are the artifact; the program was not.

**The practical consequence is narrower than it first looked — corrected the
same day, before this paragraph was a day old.** The first draft said Jodd
"cannot write the first note into a fresh Exchange mailbox at all". Measured
immediately afterwards on the same emptied mailbox: **it can.** A note created
in Jodd pushed successfully (`sync_worker: pushed dirty uuid=079D8692-…`) with
zero notes on the remote side.

The reason is that `save_note_full` does not ask Graph for the folder id at
all — `microsoft_folder_ids` (lib.rs) reads it out of the **local `folders`
table**, where a previous scan stored it in `label_id`. An account Jodd has
scanned even once keeps that id, and `wire::folder_still_exists` deliberately
keeps the row alive when the folder empties out (Task 9, M2).

So the boundary is **"has Jodd ever scanned this folder?"**, not "does the
mailbox hold notes". Both readings predict the same failure for a genuinely
cold account — Jodd has no cached id and Graph will not sell it one — but they
differ for every account already in use, which is most of them. Emptying a
synced mailbox does **not** break writing to it.

An account that has never synced still needs one note seeded from Apple Notes
before Jodd can write, which matches the product's own premise (the user
attaches the account to Apple Notes first) rather than being a workaround.

**The other half — a folder Jodd already has an id for, that then empties
out — is decided (Task 9, 2026-08-14, M2).** `list_folders` used to derive
the folder list purely from the current scan, so an already-known folder
sitting at zero notes right now was indistinguishable from one Apple had
deleted: both were simply absent from the listing, and `prune_clean_folders`
(db.rs) dropped the row either way. That conflation is what made a folder
created **in Jodd** vanish the moment its own push *succeeded* — the row
went `clean`, the next scan couldn't see an empty folder, and clean rows are
prunable. Read-only, that was defensible; M2's writes make it a bug, because
they create empty folders on purpose. Measured fix: `GET /mailFolders/{id}`
still 404s for everything (row above), but `GET /mailFolders/{id}/messages`
answers **200** for a folder that still exists — even empty — and **404**
only once it is actually deleted. `wire::folder_still_exists` uses that gap;
`list_folders` now reports the scan-derived set **union** any cached folder
id (`folder_ids`, from the local `folders` table) that scan didn't see but
still answers 200. Any response other than a clean 404 counts as "exists" —
a transient failure must never read as a deletion, since the caller prunes
on `false`. Cost: one extra `/messages?$top=1` request per stale-but-cached
folder id per pull, zero when every known folder showed up in the scan.
**The undiscoverable case above is untouched by this** — a folder Jodd has
never seen has no cached id to probe, so it stays invisible either way.

**The bootstrap deadlock, and the one thing that breaks it (2026-08-17,
`TheArchitect@renny.co.th`).** Everything above says a folder id is
reachable only through `parentFolderId` on a message. Follow that to its
conclusion on a mailbox that has never been used with Apple Notes and it
is a closed loop: the scan reports `1 page(s), 0 note(s)`, no id is ever
cached, and `save_note_full` refuses the create — *finding* the folder
needs a note, *creating* the note needs the folder. Measured, not
theorised: two notes sat `dirty` and the worker re-issued the same
refused push every 5 seconds for ~80 minutes (5,816 log lines, zero
successes) while Outlook web showed the `Notes` folder sitting there,
empty, the whole time.

The escape is that Graph's refusal is about *reading* the folder, not
about *addressing* it. Measured in one run:

| request | result |
| --- | --- |
| `GET /me/mailFolders/notes` | **404** `ErrorItemNotFound` — same as every real Notes-tree folder |
| `GET /me/mailFolders/notes/messages` | **200**, `@odata.context` names `mailFolders('notes')` |
| `GET /me/mailFolders/notes/childFolders` | **200**, empty |
| `GET /me/mailFolders/stickynotes/…` | **400** `ErrorInvalidIdMalformed` |
| `POST /me/mailFolders/notes/messages` (+ `String 0x001A` = `IPM.StickyNote`) | **201 Created**, `parentFolderId` = the real Notes id |

The 400 on `stickynotes` is what makes the 200 on `notes` mean
something: an unrecognised segment is rejected as a malformed id, so
`notes` is a *recognised well-known name* rather than a lucky string.
The created item came back from the whole-mailbox `IPM.StickyNote`
`$filter` scan and from `/messages` under the newly-revealed id, so it
is a real note in the real container — not a draft parked in Drafts,
which is where the other candidate avenue (`POST /me/messages` with the
item class but no destination) necessarily lands and why that avenue is
worthless: it would teach Jodd the *Drafts* id.

This does **not** reopen gotcha #12. The folder object is still
unreadable, the tree is still unenumerable, children still come back
empty, and a *subfolder* still has no name-addressable form — the
well-known name exists for the root and nothing beneath it. What changed
is one specific claim that was never separately tested: "cannot be
discovered" had been carrying "cannot be written to" along with it, and
only the first half is true.

Not verified here, and worth stating plainly: whether Apple's own
clients pick up a note created this way. The create is identical to the
M2-proven one in every respect except how the destination folder was
addressed (same endpoint shape, same `IPM.StickyNote` class, same
container), so there is no obvious reason it would differ — but "no
obvious reason" is not a measurement, and this file is for measurements.

**Folder *names* come from the message, not the folder.** Since the folder object
is unreadable, `displayName` would be unobtainable — except that every message
carries its parent's name in the MAPI property `PR_PARENT_DISPLAY`:

```
GET /me/messages?$select=subject
    &$expand=singleValueExtendedProperties($filter=id eq 'String 0x0E05')
```

That one call returns id **and** name together for every folder that holds a
note — verified to match the Notes.app tree exactly (`L1`, `L2`, `TEST TEST L2`,
`New Folder L1 too`, …). **It is the leaf name only**: `0x66B5`
(`PidTagFolderPathname`) and `0x3001` (`PidTagDisplayName`) both come back empty
on messages, so **the parent/child shape of the tree is still unrecoverable** —
two folders sharing a leaf name are told apart by id, but their nesting is not
known. A flat folder list is the honest first cut, and matches what iOS Mail's
own folder picker shows for this account anyway.

Tried and dead: `GET /mailFolders/{id}` on v1.0 **and beta**,
`/me/mailFolders/delta`. **The "not yet tried" list is now empty** — all three
remaining avenues were run against the live account on 2026-08-14 and all
three are dead:

| attempt | result |
| --- | --- |
| `GET /mailFolders/{id}?$expand=childFolders` on a folder id known to have children | **404 `ErrorItemNotFound`** — the folder object is unreadable, so there is nothing to expand from |
| `GET /me/mailFolders?$filter=displayName eq 'L1'` | **200 with an empty `value`** — the Notes tree is absent from `mailFolders` entirely, so no filter over that collection can reach it |
| `Binary 0x0E09` (`PidTagParentEntryId`) on a message | **returns a value** — but it is the EntryID of the folder the message is *in*, which we already have as `parentFolderId`. It is not the folder's own parent |

**So the flat folder list is not a first cut, it is the ceiling.** Treat
nesting as unavailable on this backend rather than as work waiting to be
done, and do not spend another cycle looking: the parent/child shape is not
exposed anywhere Graph reaches for a personal mailbox. Two visible
consequences to explain rather than fix — subfolders appear as siblings, and
a folder holding zero notes does not appear at all.

**EWS (`FindFolder`, `Traversal="Deep"`) is not an escape hatch — closed at
the auth gate, documented not measured (2026-08-16).** EWS is the *other*
Exchange API family (SOAP, `outlook.office365.com/EWS/Exchange.asmx`,
predates Graph) and its `FindFolder` operation is the one Exchange
surface actually designed to return a real parent/child hierarchy in one
recursive call — the shape gotcha #12 shows Graph flatly refuses. It was
never tried live here, and does not need to be: Microsoft's own docs state
plainly, *"OAuth authentication for EWS is only available in Exchange
Online as part of Microsoft 365"* — i.e. a real M365 tenant mailbox, not a
personal `outlook.com`/`live.com`/`hotmail.com` account. `kaiwan.h@live.com`,
the account every M1–M3 measurement above was run against, is exactly the
account type EWS OAuth refuses before a single `FindFolder` call could be
sent — this is a documented prerequisite failure, not a probed one, and no
probe is needed to know it fails. The one theoretical opening is the M365
work/school side (`kaiwan@bbmedia.in.th`) already shown able to mint a
Graph token — but that token is for the Graph resource, not the separate
`https://outlook.office365.com` resource EWS uses, so it does not carry
over; reaching EWS there would need the app registration granted a new
**`EWS.AccessAsUser.All`** permission and a fresh consent, on the same
non-representative admin-in-home-tenant account gotcha #12's Auth section
already says not to generalize from. Not worth spending a cycle on: even a
working `FindFolder` on that one account would not answer whether Apple's
sync exposes real nesting for an ordinary external, non-admin Microsoft
365 user, which is the case that actually matters and remains unknown
either way.

**Apple's own client disagrees with itself across devices — screenshot-verified
on BOTH sides, 2026-08-16, and the disagreement is now explained rather
than just observed.** Notes.app on macOS, for this exact
`kaiwan.h@live.com` account, genuinely renders nesting: `L1` shows a
disclosure triangle with `non-blank folder under L1` indented beneath it,
and `TEST TEST` likewise shows `TEST TEST L2` as a real child (confirming
the `TEST TEST L2` observation earlier in this gotcha). A phone screenshot
from the user of the same account's folder list on iPhone shows every one
of those same folders — `L1`, `non-blank folder under L1`, `TEST TEST`,
`TEST TEST L2` — as flat siblings at one indent level, no disclosure
affordance, no grouping at all. **iPhone's flat list matches what Graph
returns exactly.** The explanation that fits both screenshots: these
folders were created/organized via Notes.app **on this specific Mac**
during earlier M2/M3 testing, so the parent-child relationship is a
locally-cached artifact of that Mac's own Notes database — never actually
carried by the Exchange sync payload. iPhone, never having built that
local cache, renders the only thing it ever received: the same flat set
Graph's `childFolders` also returns. **This closes the question rather
than reopening it.** The nesting a Mac user sees for this backend is not
ground truth being hidden from Jodd — it is a single-device UI artifact
that does not even reach the account's *other* Apple client. Jodd's flat
list is not an approximation of a hierarchy it failed to fetch; it is the
same ground truth iPhone itself displays.

### Also asked, and answered: why can Notes.app create folders that Graph can't?

The user's frustration on 2026-08-16 — "we can't create folders, but Apple
Notes on Mac can" — is not a Jodd gap, it's the same mechanism as the folder
writes section above stated from the other direction. Notes.app on the Mac
isn't going through Microsoft Graph at all when it creates a folder; it talks
to the Exchange mailbox through whatever native sync channel macOS's Internet
Accounts framework uses (unidentified here — almost certainly not
Graph-shaped, since Graph itself cannot produce a folder classed
`IPF.StickyNote` no matter what's sent to it). That channel can evidently set
the container class Graph can't. A third-party OAuth app has no access to
that channel — only Apple's own client does. So "Mac can, Jodd can't" is the
correct, permanent state of affairs, not a missing feature: there's no
publicly documented API surface (Graph or EWS, per above) that lets a
third-party app reach whatever Notes.app itself uses.

---

## Earlier status (2026-07-30 — Ask Jodd)

**Ask Jodd** — an in-app, multi-turn, ephemeral chat over the local SQLite
cache, roadmap item #7 from
[docs/LLM-WIKI-GRAPHIFY-ROADMAP.md](LLM-WIKI-GRAPHIFY-ROADMAP.md) and
Feature 1 of
[HANDOFF-2026-07-29-tier1-copilot.md](superpowers/HANDOFF-2026-07-29-tier1-copilot.md),
via [spec](superpowers/specs/2026-07-29-ask-jodd-design.md) +
[plan](superpowers/plans/2026-07-30-ask-jodd.md).

A per-turn, four-stage pipeline (`src-tauri/src/ask/`): (1) a pure-SQL
pre-filter (`pool.rs`) unions FTS hits over `ask::terms::extract_query_terms`
(a new question-shaped keyword extractor — the existing
`autolink::extract_keywords` only keeps capitalized/repeated tokens and is
useless on a one-sentence question, Thai worst of all), the folder subtree
when folder-scoped, and a recency prior by `last_remote_modified_at`, deduped
and capped at `CANDIDATE_POOL_MAX = 400`; (2) a compact per-candidate catalog
line (`catalog.rs`); (3) LLM call 1 picks `uuid8`s from the catalog via a
lenient hex-token scan, not JSON (`catalog.rs` + `prompt.rs`); (4) LLM call 2
answers from the selected bodies, HTML-stripped and per-note-truncated before
the total budget is applied (`context.rs`), citing sources as
`[[<slug>-<uuid8>]]`. `run.rs` orchestrates all four stages; `RECENCY_K = 150`,
`MAX_SELECTED_NOTES = 12`, `MAX_NOTE_CHARS = 20_000`,
`MAX_CONTEXT_CHARS = 120_000` are defined once in `ask/mod.rs`. Retrieval
re-runs every turn against the accumulated conversation. Nothing is
persisted — no new table, no sidecar, no write to `notes` — closing the modal
discards the conversation.

Tauri commands `ask_jodd` / `cancel_ask`, with `AppState.in_flight_asks:
Mutex<HashMap<String, CancellationToken>>` mirroring `in_flight_extracts`.
`AskAnswer` carries `notes_in_scope` / `notes_considered` / `notes_used` so
the UI can show the honesty line **"N in scope → N considered → N read"** —
a heavily thinned pool is visible, not inferred from a weak answer.

**App-level LLM provider, with a per-account cascade.** Ask Jodd is
cross-account, so no single account's provider is the right owner. New
`src-tauri/src/app_llm_config.rs` (JSON config in the Tauri config dir, API
key in the keychain under `llm_api_key::__app__`) mirrors `oauth_config.rs`
exactly. `accounts::LlmProviderKind` gains a `Disabled` variant, and
**`LlmProviderKind::None` changes meaning from "unconfigured" to
"inherit"** — every `LlmConfig` field is already `#[serde(default)]`, so
every existing `accounts.json` parses unchanged and every current account
becomes an inheritor, which is the intended upgrade behavior.
`llm::resolve::resolve_app_provider()` — always the app-level provider,
independent of the `apply_to_accounts` toggle — is what Ask Jodd uses.
`llm::resolve::resolve_provider_for_account()` implements the cascade
(inherit / explicit override / `Disabled` refuses) for Extract and auto-link,
replacing the old `resolve_provider`. Account Settings relabels the empty
provider choice from "None" to "Use app default" and adds an explicit
"Disabled" option — required, not cosmetic, since the old label now states
the opposite of what the value does.

**Two known limitations, measured against the live vault, not assumed.**
The §5.1 SQL pre-filter is the recall ceiling of this design: on the
6,655-note flat test account, a conceptual question whose
wording matches no note and whose target isn't recent can be missed — the
"N in scope → N considered → N read" line exists specifically to make that
visible. Embeddings are the named successor, not dismissed, but blocked
structurally rather than by effort: agent-CLI providers (`claude -p`,
`codex`, …) expose no embedding endpoint, so an embedding index would work
only for HTTP providers and split the feature's behavior by provider type.

## Earlier status (2026-07-16 — LLM Wiki / Graphify session)

Three independent tracks from [docs/LLM-WIKI-GRAPHIFY-ROADMAP.md](LLM-WIKI-GRAPHIFY-ROADMAP.md)
(roadmap items #1, #2, #4), implemented via
[spec](superpowers/specs/2026-07-14-wiki-graphify-bundle-1-2-4-design.md) +
[plan](superpowers/plans/2026-07-14-wiki-graphify-bundle-1-2-4.md), on branch
`claude/jodd-llm-wiki-graphify-35997f`.

**Structured citations** — a new `rel='cites'` row type in the existing `edges`
table (no schema migration needed for the type itself; see the backfill note
below), derived from every note body on every write via a hand-rolled URL
scanner (`db::extract_urls` — no `regex` crate). Surfaced as a "📎 Sources"
group in the editor's Connections panel (`note_citations` command), plus a
soft duplicate-source warning in the Extract modal (`check_duplicate_citations`
pre-flight command, never a hard block — "Continue anyway" always available).
Jodd-local only, like tags/edges — never round-trips to Apple Notes.
Migration #15 (`DELETE FROM edges;`) forces a one-time repopulate on upgrade
so pre-existing notes' citations backfill immediately, not just on next edit
(same pattern as migration #13's precedent — `edges` is fully derived, so
clearing it is always safe).

**Smart Folders** — two fixed, per-account views: "🔍 Orphaned" (zero incoming
`[[wikilink]]` backlinks) and "🕰 Stale" (untouched 30+ days, hardcoded
threshold). **Fully virtual** — deliberately NOT a `folders` table row (despite
`folders.kind` already reserving `'smart_query'` for exactly this) to stay out
of Gmail-label sync semantics entirely; a separate `selectedSmartFolder` /
`smartFolderNotes` store pair in `notes.ts`, mutually exclusive with regular
folder/tag selection. `Db::list_orphaned_notes` / `list_stale_notes` (db.rs).
Read-only view — no rename/move/delete/context-menu.

**`jodd-mcp`** — a new read-only MCP server exposing `search_notes` and
`note_connections` (Jodd's graph, from any Claude Code session) via a brand
new Cargo **workspace** (see "Project structure" above) — `src-tauri` is no
longer a standalone crate. Calls `jodd_lib::db::Db` methods directly, no Tauri
runtime. Uses `rmcp` 2.2 (the plan's original `0.16` pin didn't resolve on
crates.io by the time this landed — SDKs move fast; check current version
before assuming). Manual one-time setup via `claude mcp add`, see
[jodd-mcp/README.md](../jodd-mcp/README.md). **Not part of the Tauri bundle** —
converting `src-tauri` into a workspace member silently broke two hardcoded
`src-tauri/target/...` paths in `.github/workflows/release.yml` (rust-cache
config + macOS bundle-verification step); both fixed in the same branch. If
you touch the workspace layout again, grep `.github/workflows/` for
`src-tauri/target` first.

### Earlier — 2026-06-12 — productivity-features session, v0.14.x

All shipped to `main` (pushed). Highlights:

**Tier 0 — correctness**
- **Attachments**: capture / preserve / render / stale-body safety-net, stored
  as SQLite BLOBs (gmail.rs `multipart/related`; db.rs migration #9; NoteEditor
  `hydrateAttachments`).

**Tier 1**
- **Search-as-index**: SQLite **FTS5**, `tokenize='trigram'` so Thai is
  substring-searchable, over title + HTML-stripped body. Derived on every write
  + backfilled (migration #10). `search_notes` with a **scope selector** (folder
  / account / **all accounts**). Inline #hashtags are searchable too.
- **Recently Deleted / Trash**: `list_trashed_notes` + `restore_note` (untrash +
  relabel), revision-vs-genuine-deletion filter, note-list-style UI + context
  menu. Scope kept to `gmail.modify` over `mail.google.com` (no permanent
  delete, narrower access) — both are classified **Restricted** by Google,
  so the choice narrows the verification story but doesn't avoid CASA by
  itself; see `docs/DISTRIBUTION.md`.
- **Richer text toolbar**: underline / strike / heading (`<h2>`) / ordered list;
  format buttons `onmousedown preventDefault` to keep editor focus.
- **Outline / nesting**: Tab / Shift-Tab indent — list items nest via
  execCommand (nested `<ul>`, round-trips); other lines (incl. checklists)
  indent by margin on the nearest LINE block. Enter on a checklist row
  continues the list (new task, same indent) / exits when empty; nested
  checklists **roll up** (a parent auto-ticks once all subtasks are done, and
  vice-versa). Editor-focus fixes: arrow keys no longer (a) get stolen by the
  note-list nav handler, (b) get captured by the `[[` picker inside an existing
  link.

**Tier 2 #6 — Tags: cutover from sidecar → inline #hashtag**
- Tags now live as inline `#hashtag` in the note **body** (single source of
  truth, round-trips to Apple). `note_tags` is a FULL-REPLACE derivation of body
  hashtags on every write (`reconcile_tags_from_body`); one-time
  `migrate_tags_to_body` injects legacy sidecar tags into bodies;
  `sync_tag_state` disabled. Chip add/remove edit the body; sidebar Tags render
  as compact collapsible **pills**; **cross-account** tag filter (scope
  selector); rename/delete tag **rewrite every carrying body** (HTML-aware) so
  they stick + round-trip.

**Tier 2 #5 — Fact-schema edges + backlinks**
- General **`edges`** table (migrations #11–13): `mentions` (`[[wikilinks]]`),
  `child_of` (note→folder), `tagged` (note→#tag) — derived on every write +
  backfilled.
- **Slug links**: `[[<title-slug>-<uuid8>]]` — unique (uuid id) + durable
  (re-derived from the round-tripping UUID) + readable + rename-safe; resolved by
  id, with a `[[` **autocomplete picker** (plain `[[Title]]` still works).
- **Connections panel** (→ links to / ← linked from) + a 🕸 local **graph view**
  modal (radial, colour-coded, clickable). Editor **context bar** (account ·
  folder · copyable slug).

**Cross-cutting fixes**: Apple-Notes IMAP sync-confusion recovery (toggle Notes
off/on — see memory), account-tagged + greppable `TRASHED`/`UNTRASHED` logs,
editor caret revert (the real culprit was the Windows mouse pointer, not the
caret), spurious-save guard (`userEdited`).

> **Doctrine note:** tags + edges follow the same model — derived from the body
> (which round-trips), indexed in SQLite, never stored as a sidecar/flag Apple
> would drop. See "Compatibility tiers".

### Earlier — 2026-06-09 architectural pass + Pin (v0.1.2)

**Content Extraction** (landed v0.16.1, hardened in v0.16.2, internal module now
`llm`):
LLM-backed paste-and-extract workflow that turns mixed unformatted source
text (Claude/ChatGPT conversation dumps, transcripts, debugging sessions,
articles, meeting notes, anything) into structured extract notes. Lives in
a Jodd-managed "system workflow folder" stored as `Notes/__Extracts__`
(kind='system_workflow' per migration #14), displayed as just "Extracts"
(with a 💡 icon) under a Workflows group in the sidebar after marker-strip.
Source text preserved verbatim in a collapsible `<details>` block at the
bottom of every extracted note, enabling re-extraction and verification
without re-pasting.

Internal naming was migrated to match the user-facing vocabulary on
2026-07-27: module `lessons` → `llm`, trait `LessonProvider` → `LlmProvider`,
commands `extract_lessons`/`re_extract_lessons`/`append_extract_lessons` →
`extract_note`/`re_extract_note`/`append_extract_note`. The original
churn-minimization decision was made when the module hosted a single
workflow; it now hosts two (Extract, auto-link). A multi-preset agent-CLI
provider layer is designed but not yet built — see
[the design spec](superpowers/specs/2026-07-27-agent-cli-llm-providers-design.md).
**`lessons_markdown` and `meta_lessons_markdown` were
deliberately NOT renamed — they are JSON keys in the LLM wire contract
(`prompt.rs`), not internal names.** User-facing labels ("Extract",
"Extracts", `__Extracts__`) are unchanged.

The reserved `__name__` syntax (any folder matching `__*__`) is documented
as Jodd-managed; legacy `Notes/Lessons` or `Notes/Extracts` folders from
before the standardization are treated as regular user folders.

- LLM provider abstraction: trait + two impls (HTTP for any OpenAI-
  compatible endpoint; subprocess for `claude -p`). Per-account config in
  accounts.json; API keys in OS keychain under `llm_api_key::{account_id}`.
- Cancellation propagated end-to-end (v0.16.2): the modal's Cancel button
  invokes `cancel_extraction(request_id)` which fires a `CancellationToken`
  stored in AppState.in_flight_extracts; the provider's tokio::select!
  branch unwinds — HttpProvider drops the in-flight reqwest future,
  the agent-CLI provider calls child.start_kill so the subprocess stops
  consuming Claude Code subscription quota. A cancelled extract does NOT
  create the fallback source note (user actively chose to abort).
- Output is markdown-bodied (LLMs produce dramatically cleaner markdown
  than HTML); pulldown-cmark with GFM extensions (tables, strikethrough,
  tasklists, footnotes) converts to HTML before storage. Matches Jodd's
  existing HTML body_html schema; round-trips to Apple Notes via existing
  Gmail sync.
- Folder protection: only folders matching BOTH kind='system_workflow' AND
  the `__name__` pattern get rename/move/delete hidden. `isProtectedWorkflowFolder`
  in Sidebar.svelte; `validate_folder_segment` in lib.rs rejects user
  attempts to create folders matching the pattern.
- Failure doctrine: source text is NEVER lost. On LLM error, a fallback
  note containing only the Source block is created so the paste survives.
- Tags emitted by the LLM (e.g. `#database-migrations`) populate `note_tags`
  via the existing inline-tag body parser; hyphen is part of the tag
  word-class (db.rs is_tag_word_char + NoteEditor.svelte client mirror).
- See [docs/superpowers/specs/2026-06-13-lesson-extraction-design.md] for
  the design spec and [docs/superpowers/plans/2026-06-13-lesson-extraction.md]
  for the implementation plan.

**Tags** (roadmap item #2) landed v0.14.3–v0.14.5 across three waves:

WAVE 1 — local-only tags (migration #5):
- `note_tags` join table — PK `(account_id, uuid, tag)`, index on
  `(account_id, tag)` covering both the sidebar "count per tag" query
  and the "notes carrying tag X" filter. Tags are pre-normalized by
  the write path: trimmed, leading `#` stripped, lowercased, charset
  `[a-z0-9_-]`.
- Jodd-local only despite the roadmap note about `#hashtag` in body:
  tags are NOT stored in the note body and do NOT round-trip to Apple
  Notes (Apple has no tagging system). The HTML body remains the
  Apple-compatible payload; tags are sidecar metadata in SQLite.

WAVE 2 — tombstone-based prune_clean race recovery (migration #6):
- `tag_tombstones` table (PK matches note_tags + `deleted_at`) acts as
  a recovery buffer for Gmail's eventually-consistent `q=label:Notes`.
  Before: a transient list-omission caused `prune_clean` to delete the
  cache row AND its `note_tags` entries — tags silently destroyed.
  Now: orphan-tag step moves rows to `tag_tombstones` instead of
  deleting; `upsert_from_remote` restores tombstoned tags when the
  note reappears. Old tombstones swept after TOMBSTONE_TTL_MS.

WAVE 3 — cross-Jodd-instance tag sync (migration #7):
- Mirrors Pin's sidecar pattern with one twist: pin is binary
  (sidecar exists = pinned), tags are a variable-length set, so the
  sidecar carries a JSON body `{"tags":["a","b",…]}` and
  `list_tag_sidecars` fetches WITH body (not metadata-only like
  pin's `list_meta_sidecars`).
- Subject convention `tags___<UUID>` — leading `tags` keeps the
  prefix disjoint from pin's `___<UUID>` so each sync's reader
  rejects the other's sidecars by prefix match alone.
- Columns: `tags_meta_msg_id TEXT` (current sidecar's Gmail message
  id, NULL = none yet) + `tags_dirty INTEGER` (orthogonal to
  `sync_state` AND `pin_dirty` — a row can be content-dirty, pin-
  dirty, AND tags-dirty simultaneously). Partial index on
  `tags_dirty = 1` covers the worker drain.
- Local-wins on inbound: `apply_remote_tags` skips when
  `tags_dirty=1`. Worker drains via `list_tags_dirty` → save or
  trash sidecar in `meta_label`, then `mark_tags_pushed`.

ONE-SHOT BACKFILL (migration #8):
- Runs ONCE per install (recorded in the migrations table). Bulk-
  flips `tags_dirty=1` for every uuid that already has a `note_tags`
  row, so notes tagged before v0.14.4 get a sidecar created on the
  first post-upgrade tick. No-op on fresh installs. If both Mac and
  Windows run #8 on differently-tagged sets, sidecar last-write-wins
  on the second push; local-wins blocks the inbound clobber on the
  loser, and divergent edits converge through normal sync afterwards.

**Pin** (roadmap item #1) landed 2026-06-09 in two waves:

WAVE 1 — local-only pin (commits 301435a, e392796):
- `pinned INTEGER NOT NULL DEFAULT 0` column on `notes` (migration #3),
  partial index `(account_id, pinned) WHERE pinned = 1`.
- `set_pin` + `set_pin_batch` Tauri commands — pure local-first SQLite
  writes.
- NoteList sort `pinned DESC, date DESC`, 📌 prefix on the title row.
- Context menu: single-note "Pin"/"Unpin" toggle at top; multi-select
  shows "Pin all"/"Unpin all"/both. snapshot/optimistic-update/rollback.

WAVE 2 — cross-Jodd-instance sync (commits b5b5deb–cc28c62):
The doctrine update: pin lives in SQLite AND in a Jodd-managed sidecar
message in a configurable Gmail meta-label. Multiple Jodd instances
signed into the same Gmail account share pin state through the sidecar
without involving Apple Notes (which ignores anything outside Notes/*).

- Per-account label config in Account: `notes_label` (default "Notes"),
  `meta_label` (default "Notes-Meta"). `get_account_settings` /
  `update_account_settings` Tauri commands; AccountSettings.svelte modal
  reachable from the ⚙ icon on each account row in the bottom panel.
- Migration #4: `meta_msg_id TEXT` (current sidecar's Gmail message id)
  and `pin_dirty INTEGER` (orthogonal to sync_state — a row can be
  content-dirty AND pin-dirty simultaneously). Partial index on
  `pin_dirty = 1` covers the worker drain query.
- `set_pin`/`set_pin_batch` mark pin_dirty=1. NEW: `apply_remote_pin`
  (skip if pin_dirty=1, local wins until pushed), `clear_pins_not_in`
  (drop pins whose sidecar disappeared remotely), `list_pin_dirty`,
  `mark_pin_pushed`.
- Gmail layer: `SidecarRef`, `ensure_label`, `list_meta_sidecars`
  (uses `format=metadata, metadataHeaders=Subject` so the read path
  never fetches sidecar bodies), `save_meta_sidecar`, `trash_meta_sidecar`.
  Subject convention: `___<note_uuid>` triple-underscore sentinel.
  X-UTI `app.jodd.metadata` so Apple Notes ignores them.
  Sidecar EXISTS = pinned. Unpin = trash sidecar (no falsy body needed).
- Worker: `push_one_pin` drains pin_dirty rows. Resolves meta_label
  for the account, ensures it exists, then save_meta_sidecar (pinned)
  or trash_meta_sidecar (unpinned). Runs AFTER content + deletes
  (lowest priority — pin is UX-only).
- Pull: list_notes already does inline sidecar reconciliation. NEW
  `sync_pin_state` Tauri command does the same as a dedicated cold-
  start trigger (list_notes is NOT called on cold start). App.svelte
  calls it in parallel across accounts after `indexAllAccounts()`
  completes, then loadCachedNotes() to re-paint.
- Verified end-to-end: pinned a note in Notes/pinsync, observed
  meta_msg_id populated by the worker, wiped local pin column to
  simulate a fresh second Jodd install, cold-started → sync_pin_state
  pulled the sidecar from Notes-Meta and re-applied pinned=1.

Local-first doctrine compliance landed for **D1-D4 and D8-D10** this
session. D5-D7 documented as deferred (out-of-scope minor variants of
D1-D4 patterns). Every closed defect verified end-to-end in the release
build via computer-use, scoped to a test `Notes/play5` subtree that did
not touch real user data.

Closed this session:
- **D1** (9669999) — `db::ensure_ancestors` auto-inserts missing folder
  ancestors as `dirty_new` in the same transaction as the leaf insert.
- **D2** (91e6984) — `list_cached_notes_in_folder` + `paintFolderFromCache`
  make navigation pure SQLite; Gmail-touching `list_notes_in_folder`
  reserved for explicit refresh (sweep, settle, poll, manual).
- **D3** (4568ad2) — `Sidebar.{createFolderUnder, renameFolder, deleteFolder}`
  rewritten to optimistic-first + rollback (mirroring `moveFolderTo`).
- **D4** (f8e1fff) — `move_notes_batch` + `delete_notes_batch` Tauri
  commands, one SQLite tx each; `NoteContextMenu.{moveBatchTo, deleteBatch}`
  collapsed from N invokes to 1.
- **D8** (3cb165f) — `db::list_deleted_pending_uuids` filter in
  `list_notes` / `list_notes_in_folder` / `refetch_note` so Gmail's
  eventual consistency can't resurrect just-deleted notes as UI ghosts.
- **D9** (988d304) — `.folder-menu` viewport-fit (`menuAdjustedX/Y`) +
  `max-height: calc(100vh - 16px); overflow-y: auto;`.
- **D10** (86e61f7) — `moveTargetState` replaces the binary
  `isValidDropTarget` filter for menu rendering: parent renders disabled
  with italic "(current)" tag instead of vanishing.

Deferred:
- **D5** — `Sidebar.removeAccount` is D3-shaped.
- **D6** — `NoteContextMenu.deleteNote` single-note path is mildly D3-shaped.
- **D7** — `delete_note` legacy id-fallback branch is D2-shaped; no live caller.

See "Known defects" below for full rationale and exact file/function references.

## Prior status
- [x] Google OAuth2 (PKCE) + refresh token rotation
- [x] Gmail REST API: list/fetch/save/delete/labels
- [x] SQLite local-first cache + 5s sync worker
- [x] Conflict detection with keep-both reconciliation
- [x] In-flight push tracking (no self-induced false conflicts)
- [x] Local-first folder ops (create/rename/delete/move)
- [x] Multi-account UI + per-account keychain storage
- [x] Forensic-test correctness pass (aa9a041 + docs/SYNC-BUGS-2026-06-07.md)
- [x] Cross-platform release CI (Windows, macOS ad-hoc signed)
- [x] **Multi-account hardening** (commit 15448c5): pushing-set cleanup on
      remove_account; per-account recentlySavedUuids; sync_worker_tick
      live-accounts check; label_map_cache async refresh-lock; move_note
      removed as dead code; safe orphan cleanup (preview_orphans +
      trash_specific_messages) with re-check immediately before each trash
- [x] **Duplicate review UI** (commit 15448c5): amber `N dup` pill in sidebar
      account header; DupReviewModal with keeper + orphan version preview,
      per-orphan checkbox, "Move N to Trash" confirm
- [x] **Checklist editor** (commit 15448c5): formatTask toolbar button;
      microtask-deferred attribute sync (no preventDefault); editor.contains()
      re-render guard. EML round-trip proven — Jodd writes `checked=""`, Apple
      preserves it on display but never writes it back. Tasks are Jodd-
      authoritative state.
- [x] **Multi-select notes** (commit 97f2671): selectedUuids store, cmd/shift-
      click + Cmd+A, batch move + batch delete in NoteContextMenu with
      optimistic per-item updates and per-item rollback. Amber multi-selected
      visual.
- [x] **Folder UX polish** (commit 97f2671): move-to submenu max-height 60vh
      (was 100vh — caused unreachable bottom items); auto-expand ancestors on
      any $selectedFolder change AND on folder create; folder command entry
      logging for future diagnostics

## Backend vertical abstraction — former edges #1 / #1b (both DONE, 2026-06-16)

1. **Backend trait abstraction — DONE (Vertical #0 extraction, 2026-06-16).**
   The email-backend abstraction was extracted out of `gmail.rs`; the app is now
   reframed as "Vertical #0 (Apple-via-Gmail)" behind a backend-agnostic trait
   surface. All ~70 `gmail::*` call sites in `lib.rs` route through a concrete
   `GmailVertical` (static dispatch); only 5 bootstrap calls remain
   (`get_label_map` ×3, `get_user_email` ×2 — they run before a token-bound
   vertical exists). No behavior change; Apple round-trip preserved (the
   RFC822 builder move is byte-identical + golden-tested). New module map:
   - `src-tauri/src/mime822.rs` — format-neutral MIME/Apple helpers + the RFC822
     builder (`build_note_mime`). Reusable by IMAP/JMAP/Graph. Zero `crate::` deps.
   - `src-tauri/src/backend/mod.rs` — the trait surface: `Transport`, `AtRest`
     (realized via `mime822` encode + Gmail JSON decode), `Identity`, `Deriver`,
     `MetadataSidecar`, `Vertical` + `Capabilities` (`folder_model` + `fidelity`).
   - `src-tauri/src/backend/gmail/{mod,transport,identity,deriver}.rs` —
     `GmailVertical`, wrapping the existing `gmail::*` fns. Fat `list_*`
     orchestration (dedup/sort) kept intact as inherent methods (Pragmatic scope).
   Spec: [docs/superpowers/specs/2026-06-16-vertical-0-gmail-extraction-design.md];
   plan: [docs/superpowers/plans/2026-06-16-vertical-0-gmail-extraction.md];
   north-star: [docs/superpowers/specs/2026-06-16-architecture-principles-design.md].
   **Deliberately deferred (door open via trait, not built):** `Box<dyn Vertical>`
   dynamic dispatch (added with backend #2); decomposing `list_notes` dedup/sort
   into core-side generic logic; `accounts.sync_cursor` storage + real cursor
   (`changes_since` defined + implemented as full-scan w/ inert cursor; worker not
   yet rewired onto it); `note_folders` M:N; `note_remote_ids`;
   `content_schema_version`; removal of the `gmail.rs` `pub use` re-export shim +
   relocation of the Gmail types/JSON structs into `backend/gmail/` (Phase 5 — the
   `gmail::` TYPE references in lib.rs and the shim remain until then). Adding
   Microsoft/Graph or JMAP now means: implement the `Transport` trait for the new
   wire (reusing `mime822` + `Identity`), add `Box<dyn>` dispatch, done. See
   [docs/REST-vs-IMAP-XOAUTH2.md](REST-vs-IMAP-XOAUTH2.md) for why the trait
   shape stays REST-based (Microsoft Graph is REST-shaped; IMAP deprecated upstream).
   **UPDATE (Vertical #1, 2026-06-16):** the items above marked "deferred until
   backend #2" are now BUILT — `Box<dyn Vertical>` dynamic dispatch exists
   (`vertical_for` in `lib.rs` dispatches on `Account.backend_kind`), and the fat
   `list_*` orchestration was promoted to a `NoteStore` trait (Gmail's dedup stays
   a Gmail-internal quirk; LocalFS implements its own one-file-per-uuid scan). The
   `gmail.rs` shim is also gone (Phase 5 done: Gmail wire lives in
   `backend/gmail/wire.rs`). Still deferred: `accounts.sync_cursor` real cursor,
   `note_folders` M:N, `note_remote_ids`, `content_schema_version`, JMAP/Graph.

1b. **Vertical #1 — LocalFS — DONE (2026-06-16).** A second backend vertical:
   notes stored as `.eml` files (RFC822 wrapping the SAME Apple-HTML body as Gmail,
   so it reuses `mime822` + the editor + `Identity`; `content_kind` stays
   `AppleHtml` — NOT markdown). Proves the federation: a genuinely divergent
   backend (filesystem transport, no OAuth/keychain, raw-RFC822 decode via
   `mail-parser`, stable remote-id = file path) plugs into the shared core
   (`Box<dyn Vertical>`, neutral index, conflict, sync_state) without bloating it.
   - `src-tauri/src/backend/localfs/{mod,transport,decode}.rs` — `LocalFsVertical`.
     Storage under the vault's `root_dir`: `Notes/<...folders...>/<uuid>.eml`;
     `.trash/<percent-encoded-relpath>` (delete = move to `.trash`, restore decodes
     back to the ORIGINAL subfolder); `.meta/<uuid>.{pin,tags.json}` sidecars.
     `decode.rs` parses raw `.eml` → neutral envelope (read the standard `Date`
     header via `msg.date()`, NOT `.as_text()` — it's structured).
   - Shared `AppleHtmlDeriver` (`backend/deriver_applehtml.rs`) — both verticals
     derive FTS/tags/edges identically; cross-vertical search/graph span both.
   - Account model: `accounts::BackendKind { Gmail, LocalFs }` + `root_dir`
     (serde-default = Gmail, back-compat). LocalFs account id = `localfs:<uuid>`;
     display name (vault name) in `email`, shown as `localfs:<vaultname>` everywhere
     (must be unique among local vaults). Readiness (`is_ready_local`) = dir exists,
     no network/keychain. Add via "Add Local Folder" (dialog plugin) → name prompt;
     rename via account settings (`rename_local_account`).
   - Deps added: `mail-parser`, `walkdir`, `tauri-plugin-dialog`. Verified by
     `examples/roundtrip_localfs.rs` (tempdir, no network) + live GUI test.
   Spec: [docs/superpowers/specs/2026-06-16-localfs-vertical-design.md]; plan:
   [docs/superpowers/plans/2026-06-16-localfs-vertical.md]. Follow-up DONE
   (slug rewrite-on-rename, branch `feat/slug-rewrite-on-rename`): `[[*-uuid8]]`
   wikilink DISPLAY text used to go stale after a note rename (resolution by
   uuid8 was always fine; only the frozen title-slug was wrong). Fixed by
   `db::rewrite_links_to_renamed_note_conn` (free fn mirroring
   `rewrite_tag_in_bodies`) hooked into `apply_local_edit`: it captures the
   previous title, and when `slugify(prev) != slugify(new)` rewrites every
   carrier's body (carriers found via the `edges` index — rel='mentions',
   dst_id=uuid8 — no full scan), flipping each clean→dirty so the worker
   re-syncs. Plain `[[Title]]` (no uuid8) left untouched by design. Backend-
   agnostic (operates on the SQLite cache; both Gmail + LocalFS verticals push
   the dirtied carriers). Spec:
   [docs/superpowers/specs/2026-06-16-slug-rewrite-on-rename-design.md]; plan:
   [docs/superpowers/plans/2026-06-16-slug-rewrite-on-rename.md].


## Closed local-first defects (D1–D11, 2026-06-09 architectural pass)

### Closed (session 2026-06-09 architectural pass)

- **D1. Orphan child folder rows.** ✅ CLOSED.
  `db::ensure_ancestors` inserts every missing strict ancestor below the
  implicit "Notes" root as `dirty_new` inside the same transaction as the
  leaf insert. Wired into both `insert_folder_local_new` and
  `upsert_folder_from_remote`. Root cause was the pull path:
  `upsert_folder_from_remote` accepted whatever labels Gmail returned, and
  Gmail allows `A/B` to exist without `A` (the slash is just a character),
  so a deletion of `Notes/play4` upstream while `Notes/play4/play4sub`
  survived created the orphan. The user's SQLite had already self-healed
  by the time the fix landed; no one-shot repair query was needed.

- **D2. `list_notes_in_folder` blocked on Gmail.** ✅ CLOSED.
  Added `list_cached_notes_in_folder` (pure SQLite read scoped to one
  label). Folder-click navigation in `App.svelte` now paints from cache
  immediately via the new `paintFolderFromCache`. The existing
  `list_notes_in_folder` stays as the Gmail-touching command and is still
  called by the 10s folder settle, the 2.5s sweep tick, the 10min poll,
  and the manual refresh button — all explicit reconciliation paths.

- **D3. Frontend state updates after awaited invoke.** ✅ CLOSED.
  `Sidebar.createFolderUnder`, `renameFolder`, and `deleteFolder`
  rewritten to mirror `moveFolderTo`: snapshot → optimistic mutate →
  invoke → rollback. Also dropped the post-success `$refreshNotes()`
  they used to fire (full Gmail re-fetches for state changes that
  didn't touch any note content).

- **D4. N sequential save_note / delete_note in batch ops.** ✅ CLOSED.
  Added `move_notes_batch` + `delete_notes_batch` Tauri commands. Each
  runs a single SQLite transaction over the supplied uuids.
  `NoteContextMenu.moveBatchTo` and `deleteBatch` now fire one invoke,
  with full snapshot/rollback semantics.

- **D8. Ghost notes after delete: list paths returned Gmail messages
  whose local row was `deleted_pending`.** ✅ CLOSED (2026-06-09,
  post-D10). Surfaced during Phase 6: after a batch delete, the 10s
  folder settle fired `list_notes_in_folder` → Gmail's index hadn't
  yet caught up to the worker's trash calls → returned the
  just-deleted messages → frontend merge re-added them to `$notes`.
  SQLite was correct throughout; only the UI showed ghosts.
  Fix: new `db::list_deleted_pending_uuids(account_id)` helper, called
  by all three Gmail-touching read paths (`list_notes`,
  `list_notes_in_folder`, `refetch_note`) to filter the result before
  returning. `refetch_note` returns an explicit error instead of
  filtering since the caller is asking for one specific message
  ("uuid is marked deleted locally — refusing to resurrect").

- **D9. Folder context menu clips off-screen on tall trees.** ✅ CLOSED
  (Phase 7, 2026-06-09). Sidebar's `.folder-menu` inlines the move-to
  folder list directly (no nested submenu like `NoteContextMenu`) and
  had no viewport-fit clamp or max-height. When the right-click landed
  low in the sidebar AND the account had many labels, Delete ran below
  the viewport bottom and was unreachable. Fix: mirror
  `NoteContextMenu`'s `adjustedX/Y` snap via a `menuEl` bind +
  reactive `getBoundingClientRect()`, plus
  `max-height: calc(100vh - 16px); overflow-y: auto;` on the menu so
  even a too-tall menu scrolls instead of clipping. Verified
  end-to-end by reaching the previously-unreachable Delete on a
  deeply-nested `play5/play5a/play5ab` triplet.

- **D10. Move-to filtered the parent entirely — looked like the folder
  vanished.** ✅ CLOSED (2026-06-09, post-Phase-7). Right-clicking a
  subfolder showed a move-to list with the parent removed (since moving
  a child to its parent is a no-op). On the play5/play5a/play5ab tree,
  right-clicking play5a hid play5 entirely — user couldn't see the
  folder structure they were working inside. Fix: replace
  `isValidDropTarget` filter in the move-to render with a three-way
  `moveTargetState` (`valid | parent | hide`). Self and descendants
  still hide (truly impossible targets). Parent renders disabled with
  a "(current)" italic tag so the structure is visible without being
  selectable.

- **D11. Empty folders invisible on cold start.** ✅ CLOSED (2026-06-09,
  post-Windows-OAuth-fix). Folders with no notes (e.g. `Notes/play2`,
  `Notes/play3`, `Notes/play4` and their subs) did not appear in the
  sidebar until the user navigated. Root cause: the folders cache was
  reconciled from the Gmail label set **only inside `list_notes`**, which
  is NOT called on cold start (cold start runs `index_account` +
  `sync_pin_state` only — see Pin sync wave 2 above). Folders that
  *contained* a note still appeared because the sidebar infers a folder
  path from note labels (`folderCountsByAccount`); empty labels have no
  note to infer from, so they stayed hidden. Verified against the user's
  real mailbox: SQLite `folders` table held all of play2/3/4 as `clean`,
  but the sidebar omitted them. Fix: extracted the list_notes folder
  reconciliation into `reconcile_folders_from_labels(db, account_id,
  label_map, prune)` and called it upsert-only (`prune=false`) from
  `index_account`, so the cold-start index pass populates empty folders.
  Pruning stays list_notes-only (`prune=true`) — cold start must not
  delete on a possibly-partial view. Frontend: `Sidebar` reactive folder
  refresh now also depends on `$noteIndex`, so it re-reads `list_folders`
  once the cold-start index (which now carries the reconciled folders)
  lands. NOTE: counts shown beside a folder are direct-label only (notes
  in that exact label, not descendants) — by design, matching Apple Notes;
  the "All" account total is the full mailbox count, so a small per-folder
  number next to a large total is correct, not a bug.


## Roadmap — shipped

### Done
- [x] **Pin** (2026-06-09) — `pinned` column + `set_pin` + sort.
- [x] **Attachments** (Tier 0) — SQLite-BLOB store, full round-trip.
- [x] **Search-as-index** — FTS5 (trigram, Thai) + cross-account scope.
- [x] **Recently Deleted / Trash** — list + restore (untrash + relabel).
- [x] **Richer text toolbar** — underline / strike / heading / ordered list.
- [x] **Tags inline `#hashtag`** — body = source of truth, round-trips; sidebar
      pills, cross-account filter, rename/delete (was "Tags via #hashtag").
- [x] **Fact-schema edges + backlinks** — `edges` (mentions/child_of/tagged) +
      `[[slug]]` links + `[[` picker + local graph view.
- [x] **Outline / nesting** — Tab/Shift-Tab indent (nested `<ul>` + margin),
      checklist Enter-continues + nested-checklist roll-up; editor-focus fixes.
- [x] **Content Extraction** (v0.16.1, hardened v0.16.2) — LLM-backed
      paste-and-extract workflow. Generic enough to handle debug sessions,
      meetings, articles, conversations — prompt adapts structure to source.
      `llm` Rust module behind a `LlmProvider` trait with HTTP
      (OpenAI-compatible) + `claude -p` subprocess impls.
      Output goes into a `system_workflow`-kind folder stored as
      `Notes/__Extracts__` (migration #14), displayed as "Extracts" in the
      sidebar Workflows group after marker-strip. The `__name__` syntax is
      RESERVED for Jodd-managed folders — `validate_folder_segment` rejects
      any user-create or rename matching `__*__`. Source preserved verbatim
      in a collapsible `<details>` block for re-extraction. Cancellation
      (v0.16.2) propagates through `CancellationToken` to both providers —
      drops the reqwest future (HTTP) or kills the child subprocess
      (Claude CLI). Tags emitted by the LLM (hyphens included) populate
      `note_tags` via the existing inline-tag parser.
- [x] **Agent-CLI LLM providers** (2026-07-28) — any headless agent CLI
      (`claude`, `codex`, `qwen`, `gemini`, `opencode`, `aider`, or a Custom
      spec) can back Extract and auto-link. One runner, a preset table, a
      Test-connection probe, and a single retry for CLIs with no JSON mode.
      Verified live: claude and codex. See
      [the design spec](superpowers/specs/2026-07-27-agent-cli-llm-providers-design.md).
- [x] **Structured citations** (2026-07-16) — `rel='cites'` edges, hand-rolled
      URL scanner, Sources panel, soft duplicate-source warning. Jodd-local
      only. See Current status.
- [x] **Smart Folders — Orphaned + Stale** (2026-07-16) — fully virtual,
      per-account, fixed set. See Current status and edge #3.
- [x] **`jodd-mcp` — read-only MCP graph server** (2026-07-16) —
      `search_notes` + `note_connections`, new Cargo workspace member. See
      Current status and edge #7.
- [x] **Account inactive status** (2026-07-30) — `AccountStatus`
      (Active/Draining/Inactive) on `Account`. Deactivating is a *quiesce*:
      the account leaves every view immediately and the worker keeps draining
      its outbound queues. The worker flips Draining → Inactive once
      `db::has_pending_pushes` returns false; that path means **`Inactive`
      is a guarantee that nothing is pending**. The exception: "Stop waiting"
      forces the flip immediately, and any unsent edits stay in SQLite as
      `dirty` rows — not lost, but left on the device. They drain normally
      if the account is reactivated. Anything relying on the guarantee (0c
      included) must check the queue rather than assume the state. `vertical_for`
      refuses `Inactive` only; `remove_account` refuses `Draining`, because it
      deletes the refresh token first. See edge #9.
- [x] **Android bring-up — Sub-project 1: headless core** (2026-08-03, branch
      `feat/android-core`, not yet merged). Sideload → Google sign-in →
      185 notes / 70 labels into SQLite → edit on the phone → the edit appears
      in Apple Notes on iPhone, verified on a Galaxy S23 FE (Android 16) and
      an Infinix X6821 (Android 13). New seams: `secrets.rs` (credential
      store, `keyring-core` + per-target providers) and `paths.rs`
      (config/data dirs). OAuth was the whole of the work — see edge #11 and
      [docs/android/APP-LINKS-SETUP.md](android/APP-LINKS-SETUP.md).
      **No UI work: the desktop three-pane layout is unusable at phone width,
      by design.** That is Sub-project 2; Sub-project 3 is the APK release
      pipeline. See roadmap item 8.
- [x] **At-rest encryption for `jodd.sqlite3`** (2026-08-13) — SQLCipher with
      a vendored OpenSSL crypto backend (`bundled-sqlcipher-vendored-openssl`,
      chosen over LibTomCrypt after a PoC found it unreachable as a Cargo
      feature and a confirmed crash bug in every vendored SQLCipher version).
      New `src-tauri/src/db_crypto.rs` owns key generation (`rand::OsRng`,
      256-bit, never a passphrase), OS-keychain storage (`db_cipher_key::v1`),
      plaintext detection, canary-classified encrypted opens, and the
      one-time plaintext→encrypted migration (synchronous, inside
      `Db::open()`, matching the existing schema-migration pattern).
      `Db::open()` is now the encryption-aware entry point;
      `Db::open_unencrypted()` covers test/temp-dir fixtures across both
      crates. A key-mismatch/corruption recovery path quarantines the
      undecryptable file (never deletes it) and prompts the user to
      re-index their Gmail accounts via a new banner, instead of the old
      silent fallback to a throwaway temp-dir DB. `jodd-mcp` gets
      `--self-test` and a fail-fast refusal to open a still-plaintext DB —
      it never migrates itself. **Scoped claim, not blanket:** encryption
      covers the local cache of Gmail-synced notes only — Local Folder
      vaults store notes as plain `.eml` files on disk regardless, since
      that's the backend's whole value proposition; the "Add Local Folder"
      flow now shows a blocking disclosure before folder selection
      completes, stating this plainly. Live testing after the plan's task
      review found and fixed one real crash bug beyond what code review
      caught: the key-mismatch recovery path only quarantined `jodd.sqlite3`
      itself, leaving stale `-wal`/`-shm` sidecars behind — SQLite matched
      them to the freshly-created replacement DB by filename alone, so
      recovery could fail on its own retry. See
      [design spec](superpowers/specs/2026-08-13-at-rest-encryption-design.md)
      and [plan](superpowers/plans/2026-08-13-at-rest-encryption.md).
