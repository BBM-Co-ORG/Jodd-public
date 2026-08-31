# Microsoft/Outlook — Vertical #2, Milestone 1 (read-only)

> Status: **design / approved** (2026-08-14). Adds a third backend vertical
> (Microsoft Graph over an **Exchange** mailbox) behind the existing trait
> surface. M1 is deliberately read-only: sign in, index, list, display. Writes,
> folder operations, sidecars and Android are later milestones.
>
> Builds on:
> - [2026-06-16-architecture-principles-design.md](2026-06-16-architecture-principles-design.md) (locked trait surface)
> - [2026-06-16-localfs-vertical-design.md](2026-06-16-localfs-vertical-design.md) (Vertical #1 — the dyn-dispatch precedent)
> - `CLAUDE.md` → "How Apple Notes ↔ Outlook.com works" and gotchas #11, #12
>
> **Acceptance bar:** a `@live.com` / `@outlook.com` account can be added from
> the real UI, its notes and folders appear in the sidebar and list, and every
> existing test stays green. `cargo test --workspace` — not a narrower gate.

## Goal & framing

Roadmap #4. Feasibility is no longer in question: on 2026-08-14 a live
investigation proved full CRUD round-trip to Notes.app and iPhone, stable
identity, clean semantic HTML, and a working third-party OAuth consent for both
a personal and a work Microsoft account. Everything below is engineering on top
of facts, not a bet.

**This is a different protocol family from Gmail, not the same trick against
another host.** Apple syncs to Outlook.com over **Exchange**; a note is an
`IPM.StickyNote` item in an `IPF.StickyNote` container, not an RFC822 message
with Apple `X-` headers. Three consequences shape this design:

1. **`mime822.rs` is unused here.** There is no MIME to build or parse. Exchange
   synthesises ~1 KB of its own on `$value` and none of Apple's headers survive.
2. **Identity is `internetMessageId`,** verified stable across an Apple-side
   edit, a Graph-side `PATCH`, and an overwrite+undo cycle.
3. **`PATCH` is a real in-place update.** The Gmail insert-new + trash-old +
   `mark_pushed` id-repair sequence must **not** be copied over (M2 concern,
   recorded here so it is not rediscovered).

M1 exercises none of the write path, so the divergences that would strain the
trait surface stay out of scope. That is the point: land a usable read-only
vertical, then let M2/M3 supply real evidence about whether `Transport` needs
reshaping.

## Decisions locked in brainstorming (2026-08-14)

- **M1 depth:** full integration with the SQLite cache and existing UI, but the
  background sync worker is **not** touched. Refresh is the ⟳ button and
  sign-in indexing only. Rationale: the worker is load-bearing code with a
  history of self-induced false conflicts; a read-only vertical is a complete,
  shippable slice without it.
- **Folder identity:** `folders.path` = the folder's **leaf name**; on a name
  collision, append a deterministic discriminator derived from the folder id.
  No schema change. Rejected: bare leaf name (silently merges two folders that
  share a name — data confusion with no error) and folder-id-as-path (correct
  but forces a migration and rewrites every display site).
- **Capabilities:** populate the existing-but-dead `Capabilities` struct now,
  with **exactly one** new field — the one M1 actually surfaces. Rejected:
  deferring to M2 (leaves an always-empty Trash view) and `backend_kind`
  conditionals in the frontend (the thing CLAUDE.md explicitly warns against).
- **Architecture:** implement the existing trait set as-is; `list_folders` is a
  *derived* operation, not a stub. Rejected: reshaping `Transport` into
  required + optional traits before writing a single line of the vertical —
  that designs from a guess. M3 will supply the evidence.

## Component A — Account model + auth

### A1. `BackendKind::Microsoft`

`accounts.rs` gains a third variant alongside `Gmail | LocalFs`. Existing
accounts keep defaulting to `Gmail` (the back-compat path already tested by
`backend_kind_default_is_gmail`). `vertical_for()` in `lib.rs` gains a match arm.

Refresh tokens keep the existing keychain shape — service `jodd`, key
`rt::{email}`. No new secret-storage concept.

### A2. `auth_ms.rs` — reuse what is already provider-neutral

`auth.rs` (463 lines) is hardwired to Google through module-level constants
(`AUTH_URL`, `TOKEN_URL`, `SCOPES`) and free functions (`client_id()`,
`client_secret()`). Its genuinely reusable parts are already `pub`:

| Concern | M1 approach |
| --- | --- |
| PKCE verifier/challenge | **reuse `auth::PkcePair`** |
| loopback listener | **reuse `auth::wait_for_callback_blocking`** |
| endpoints, scopes, client id | new, in `auth_ms.rs` |
| token exchange + refresh | new (different response shape) |

**`auth.rs` is not refactored in M1.** If `auth_ms.rs` turns out to duplicate
more than the provider-specific bits, merge them later with the duplication in
front of us. Refactoring a working OAuth module on a prediction is how the
Android chain in gotcha #8 got expensive.

Verified parameters (from the 2026-08-14 door test, both account types):

```
tenant     common                                   # personal AND work both consented
scope      Mail.ReadWrite offline_access User.Read
client     public client — no secret at all
redirect   http://localhost:{port}
env        MS_CLIENT_ID   (mirrors GOOGLE_CLIENT_ID)
```

Having **no client secret** removes half of the `.env`-shipped-empty failure
mode by construction.

**Android is explicitly out of scope for M1.** Google's Android OAuth turned
out to be a chain of four constraints, three invisible from the docs (gotcha
#8). Microsoft's will be its own chain. Desktop first; Android is M4.

## Component B — `backend/microsoft/` (the vertical)

Mirrors `backend/gmail/` exactly:

```
src-tauri/src/backend/microsoft/
├── mod.rs     # MicrosoftVertical + trait impls (thin, ~gmail/mod.rs's 105 lines)
└── wire.rs    # Graph REST + JSON decode — all the real work
```

`MicrosoftVertical` implements `Transport + MetadataSidecar + NoteStore +
Identity + Deriver`. M1 implements the read methods for real; write and folder
methods return `TransportError::Permanent` with a message naming the milestone
that will provide them. `Deriver` delegates to the existing
`AppleHtmlDeriver` — the body HTML is clean semantic markup (`<b> <i> <u>
<strike>`, correctly nested, no inline styling), so it needs no adaptation.

### B1. Two-phase read

**Phase 1 — index** (sign-in, and the ⟳ button):

```
GET /me/messages?$top=…&$select=subject,parentFolderId,createdDateTime,lastModifiedDateTime
    &$expand=singleValueExtendedProperties($filter=id eq 'String 0x0E05')
```

One pass yields notes **plus** each note's folder id (`parentFolderId`) **and**
folder display name (`PR_PARENT_DISPLAY`, `0x0E05`) — the only route to a folder
name that exists, because the folder object itself is unreadable (gotcha #12).
Folder rows are written to `folders`, with the Exchange folder id stored in the
existing `label_id` column.

**Phase 2 — per-folder reads:**

```
GET /me/mailFolders/{folder_id}/messages?$select=subject,body,…
```

Once a folder id is known it works normally, and the result is notes-only by
construction.

**⚠️ First thing to verify when implementation starts.** Phase 1 returns Inbox
mail mixed in. Filtering server-side on message class —
`$filter=singleValueExtendedProperties/any(ep: ep/id eq 'String 0x001A' and ep/value eq 'IPM.StickyNote')` —
is the intended approach but is **untested**, and this API has already been
observed to return `200` with no properties rather than an error when a filter
it dislikes is used (an `or` across two property ids does exactly that). If the
filter does not work, fall back to requesting `PR_MESSAGE_CLASS` (`0x001A`)
alongside `0x0E05` and classifying client-side — heavier, but certain.
**Query one extended property at a time when probing.**

### B2. Field mapping

| `Note` field | Source |
| --- | --- |
| `id` | Graph `id` |
| `uuid` | **`internetMessageId`**, angle brackets stripped |
| `title` | `subject` |
| `body_html` | `body.content`, leading title line removed (Component C) |
| `x_mail_created_date` | `createdDateTime` |
| `date` | `lastModifiedDateTime` |
| `label` | derived folder path (Component D) |
| `attachments` | always empty — impossible on this backend |

`uuid` carries `internetMessageId` rather than an Apple UUID because no Apple
UUID exists here. It satisfies what the cache needs: stable across edits,
unique per note. `notes` PK `(uuid, account_id)` is unaffected.

## Component C — title/body split (`strip_leading_title_ms`)

**Do not reuse Gmail's `strip_leading_title`.** On Gmail the title happens to be
the first *element*; on Exchange its markup follows whatever formatting the user
applied. Four notes from one real folder produced four shapes:

```
plain title          [0] TEXTNODE "test text format"
plain title          [0] TEXTNODE "note 1 in Note "
bold+italic title    [0] <b><i>… in bold + italic</i></b>
written by Graph     [0] <div>note 2 in Note</div>
```

The rule that fits all four: **take everything from the start of `<body>` up to
the first block element (`<div>`); if there is no leading text/inline run, the
first `<div>` is the title.**

M1 only needs the *stripping* half — `subject` already carries the title as
plain text, so nothing has to be parsed out of the body. The function removes
the leading line so it does not appear twice in the editor.

Two traps, both hit during the investigation and both of which production code
would hit identically: an element-only walk **skips text nodes**, and three
samples yield the tidy-but-wrong "title = first text node" rule — the
formatted-title case is what falsifies it.

Failure mode if this is wrong: the first `<div>` gets stripped instead, which is
**the second line of real content, silently, with no error and no log**, on
every note. This is the highest-risk function in M1.

## Component D — folder model

From Phase 1, collect `(folder_id, leaf_name)` pairs:

- name unique in the account → `path = "L2"`
- name collides → `path = "Ideas~a3f9c1"`, where the suffix is the first 6 hex
  chars of `Sha256(folder_id)` — deterministic, so a folder's path never
  changes between runs. `sha2` is already a direct dependency (`auth.rs` uses it
  for the PKCE challenge), so this adds nothing.

**Do not use `std::collections::hash_map::DefaultHasher`.** Its output is not
guaranteed stable across Rust versions, so a toolchain upgrade would silently
re-label every collided folder and orphan the notes filed under the old path.
The suffix must be reproducible across builds, machines and releases.

Truncating the folder id itself is also unsafe: the real ids observed differ
only in the middle (`…tOfQAAAA==`, `…tOgAAAAA==`, `…tOjQAAAA==`), so a tail
substring collides for exactly the folders this is meant to separate.

Consequences, all accepted for M1:

- No `/` in any path → the sidebar renders a **flat list**. This matches what
  iOS Mail's own folder picker shows for this account, so it is not a
  regression in user terms.
- Subtree queries (`label = ?1 OR label LIKE ?1 || '/%'`) still run correctly;
  they simply find no descendants, because there are none.
- **A folder holding zero notes cannot be discovered at all** and will not
  appear. Creating a folder on the iPhone and not seeing it in Jodd until the
  first note is filed reads as a sync bug — M1 does not error, and this is
  called out in the milestone's known limitations rather than papered over.

The discriminator character leaks to users and to agents through `notes.label`
and jodd-mcp. Pick one users are unlikely to type in a folder name; `~` is the
proposal. Changing it later re-labels every note, so it is worth one minute of
thought at implementation time.

## Component E — capabilities + UI

```rust
pub struct Capabilities {
    pub folder_model: FolderModel,
    pub fidelity: Fidelity,
    pub has_trash: bool,   // new
}
```

| vertical | `has_trash` | why |
| --- | --- | --- |
| Gmail | `true` | Gmail Trash is Apple's "Recently Deleted" over that backend |
| Microsoft | `false` | verified: a Graph `DELETE` does not land in Deleted Items |
| LocalFs | `true` | it keeps a real `trash_dir()` that `list_trashed` walks |

`Capabilities` exists today on `Vertical` but has **zero call sites** — both
enums have a single variant. M1 gives it its first real consumer: a Tauri
command `backend_capabilities(account_id)` that the frontend reads to hide the
Trash view for accounts without one.

Exactly one field is added. `attachments` and `in_place_update` wait for the
milestone that consumes them.

## Verification

Unit-testable with no live account, and worth writing first:

| Target | Why it matters |
| --- | --- |
| **`strip_leading_title_ms`** | Highest-risk function in M1. Table-driven over the four real shapes captured on 2026-08-14. |
| folder path disambiguation | A collision must produce the same path on every run. |
| Graph JSON decode | Fixtures taken from real responses captured during the investigation. |

Needs a live account (manual, recorded in the milestone's notes):

- whether the server-side message-class filter works (B1)
- end-to-end sign-in and first index

Gate — the commands CI runs, not narrower ones:

```bash
cargo test --workspace
npx vitest run
npx svelte-check --threshold error
npm run build
```

## Implementation order (phases)

1. `BackendKind::Microsoft` + `vertical_for` arm + account add/remove UI path
2. `auth_ms.rs` — sign-in produces and persists a refresh token
3. `microsoft/wire.rs` decode + `strip_leading_title_ms` **with tests first**
4. Phase-1 index → `folders` + `notes` rows in the cache
5. Phase-2 per-folder reads → list and detail render in the existing UI
6. `has_trash` capability + frontend consumption
7. Manual end-to-end pass on the live account

## Deferred (door open, not built)

- **M2 — write.** `PATCH` in place (never insert+trash), `POST` with
  `PR_MESSAGE_CLASS = IPM.StickyNote` set at creation, `DELETE`. Wire into the
  sync worker. Title changes need `subject` and the leading body line updated
  together. Adds `attachments: false` so the UI stops offering 📎.
- **M3 — folder operations and moves.** Where `move_note(add, remove)`'s
  label-shaped API meets real folder moves, and therefore where the "should
  `Transport` be reshaped?" question finally has evidence behind it.
- **M4 — sidecars and Android.** Pin/tags as sidecars on `IPM.StickyNote`
  items is untouched territory; Android OAuth is its own constraint chain.

## Open questions (non-blocking)

- Does omitting `PR_MESSAGE_CLASS` on `POST` actually fail? Only the positive
  case was tested — setting it works. Do not document it as required until the
  negative case is checked.
- Is a Graph-deleted note recoverable at all? It did not appear in Deleted
  Items or in Apple's UI. Until this is understood, Jodd should not promise an
  undo path on this backend — a Jodd-local soft delete in SQLite is the likely
  answer, and would make deletion behave the same across every backend.
- Can a **non-admin** user in a **different** M365 tenant consent? The door test
  covered a personal account and a tenant admin in the app's own tenant. The
  Entra "verified publisher" policy plausibly bites in the untested case.
- What does deleting from **Apple Notes** (rather than Graph) do, and does it
  leave anything behind?
