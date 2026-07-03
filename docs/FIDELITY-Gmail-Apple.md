# Fidelity Manifest — Gmail / Apple Notes adapter

> **What this is.** The first concrete piece of the "shared fact base" waist: an
> enumeration of every *fact* Jodd asserts, and exactly how faithfully the
> **Gmail-as-transport / Apple-Notes-as-peer** channel carries it. It converts the
> scattered compatibility-tier lore (CLAUDE.md "Compatibility tiers", "Local-first
> doctrine", the checklist/pin/tag precedents) into a single table the sync
> reconciler — and every future feature — can reason against.
>
> **Why an adapter has a fidelity, not the product.** A shared fact base interops
> only as well as its *least faithful participant*. Over this channel that
> participant is **Apple**, which preserves facts inside the note body and under
> `Notes/*` and silently drops everything else. This document is the map of where
> Apple leaks, so we know per fact whether it round-trips, needs residue stashed
> elsewhere, or simply cannot cross this channel.

Grounded in `src-tauri/src/gmail.rs` as of v0.14.5.

---

## Fidelity classes

| Class | Meaning | Durable across an **Apple-only** device? | Durable across a **second Jodd** instance? |
|---|---|---|---|
| **SHARED** | Apple and Jodd both author *and* read it faithfully. The true waist facts. | ✅ | ✅ |
| **PRESERVED** | Jodd authors it inside the body; Apple displays and re-emits it **verbatim** but never originates or edits it. "Jodd-authoritative, channel-durable." | ✅ (Apple won't clobber it) | ✅ |
| **SIDECAR** | Lives in a separate `Notes-Meta` message Apple ignores (`X-UTI: app.jodd.metadata`, outside `Notes/*`). | ❌ (Apple never sees it) | ✅ |
| **LOCAL** | SQLite only; never put on the wire. | ❌ | ❌ |
| **DROPPED** | Jodd *could* place it on the note message, but Apple discards it on its next sync. **Never rely on this for durability.** | ❌ | ❌ |
| **OPAQUE-IN** | Apple authors it; Jodd can receive but cannot faithfully re-author. Read-degraded; write = pass-through bytes or refuse. | ✅ (if untouched) | ✅ (if untouched) |
| **OFF-CHANNEL** | Apple holds the fact but it never enters the email backend at all. Invisible and unsettable through this adapter. | n/a | n/a |
| **UNKNOWN** | Not yet classified. Requires a forensic capture of a real Apple message before any write path is built. | ? | ? |

---

## Master table

### Note facts

| Fact | Class | Wire location | Apple behaviour | Residue | Notes / risk |
|---|---|---|---|---|---|
| Note identity (UUID) | **SHARED** | `X-Universally-Unique-Identifier` header | Authors it, preserves across every edit | none | The cornerstone shared fact — Apple handed us stable cross-peer identity for free. `gmail.rs:1015,1134`. |
| Note kind | **SHARED** | `X-Uniform-Type-Identifier: com.apple.mail-note` | Required for Apple to treat the message as a note | none | Drop it and the message stops being a note. `gmail.rs:1005,1127`. |
| Title | **SHARED** | `Subject:` **and** mirrored `<div>`/`<span>` at body head | Authors both; sorts/displays from them | none | Jodd strips on read / injects on write at the boundary (`strip_leading_title` / `inject_title_into_body`, `gmail.rs:250,313`). |
| Body — basic rich text (bold, italic, lists, links, headings) | **SHARED** | `text/html` body | Authors and renders standard HTML | none | Round-trips because it's just HTML Apple already emits. The current editor only exposes B/I/list/task, but the *channel* carries more. |
| Created date | **SHARED** | `X-Mail-Created-Date` header | Authors it; also drives IMAP `INTERNALDATE` and sort | none | Immutable-ish; never regenerate on edit. `gmail.rs:1020,1132,1176`. |
| Modified date | **SHARED** | message `Date:` header | Apple stamps it on its writes | none | Used by the conflict reconciler and date-DESC sort. No dedicated `X-Mail-Modified-Date` exists. |
| Folder / hierarchy | **SHARED** | Gmail label `Notes/a/b` | Mirrors as the Notes folder tree | none | ⚠️ Gmail allows `A/B` to exist without `A` (the slash is just a char) — parent is a *derived*, not asserted, fact. Root cause of the D1 orphan-folder bug; `ensure_ancestors` patches it locally. |
| Checklist item + **checked-state** | **PRESERVED** | body HTML `<... checked="">` | Renders it; preserves verbatim; **never writes `checked` back** | none (durable in body) | The canonical PRESERVED tier — proven end-to-end. Tasks are Jodd-authoritative yet survive an Apple-only round-trip. This is the template for durable-but-Jodd-owned metadata. |
| Inline `#hashtag` | **SHARED** | body text | Apple Notes recognises hashtags natively | none | Round-trips fully — but **not currently how Jodd stores tags** (see next row). The intended migration target for tags. |
| Tags (**current** implementation) | **SIDECAR** | `tags___<uuid>` message in `Notes-Meta`, body `{"tags":[...]}`, `X-UTI: app.jodd.metadata` | Ignores it (outside `Notes/*`) | the sidecar *is* the residue | **DROPS at Apple** today. Cross-Jodd only. To make tags round-trip, move them to inline `#hashtag` (SHARED) and keep the sidecar only as a cache. `gmail.rs:1415,1710`. |
| Pin | **SIDECAR** | `___<uuid>` message in `Notes-Meta` | Ignores it | sidecar + SQLite `pinned` mirror | DROPS at Apple by design; Apple's own pin is OFF-CHANNEL anyway (next-to-last row). `gmail.rs:1407`. |
| Attachments / inline images | **SHARED** (target) — **DROPPED by bug today** | `multipart/related; type="text/html"` → part 0 text/html with `<object type="application/x-apple-msg-attachment" data="cid:X">`; part 1 `image/png; x-apple-part-url="X"`, `Content-Disposition: inline; filename=`, base64, `Content-Id: <X>` | Authors it; round-trips natively | none (SHARED) once write builds multipart | **F1 CLOSED 2026-06-11.** Fully reproducible → no sidecar needed. ⚠️ `save_note` emits single-part text/html, so it **strips the image part and leaves a dangling `<object cid>` on re-save = data loss on first edit.** Fix needs: read path retains parts; write path rebuilds multipart/related with matching Content-Id. |
| Note-links / backlinks | **UNKNOWN** | likely `applenotes:` or inline anchor in body | unknown if it resolves cross-device | TBD | Forensic capture §F2 before building the PKM links layer. |
| Apple drawings / scanned docs | **OPAQUE-IN** | proprietary MIME parts | Authors; Jodd can't re-author | pass-through verbatim | On save (insert-new+trash-old) these parts must be echoed byte-for-byte, never re-encoded. |
| Locked / encrypted note | **OPAQUE-IN** (likely **OFF-CHANNEL**) | encrypted payload, if present at all | Jodd cannot read | n/a | Probably appears opaque or never enters the Notes label. Treat as do-not-touch. |
| Apple **native** pin / Apple's own metadata | **OFF-CHANNEL** | iCloud metadata, *not* the email backend | invisible to this adapter | n/a | This is *why* Jodd's pin is a sidecar rather than a mirror of Apple's pin. |

### Folder facts

| Fact | Class | Wire location | Notes |
|---|---|---|---|
| Folder path / identity | **SHARED** | Gmail label name under `Notes/` | See hierarchy caveat above. |
| Folder sync bookkeeping | **LOCAL** | SQLite `folders` table | `sync_state`, `last_synced_at`, etc. |
| Smart / saved-query "folders" | **LOCAL** | SQLite only | A query, not a label — never goes on the channel (and shouldn't). |

### Internal bookkeeping

| Fact | Class | Notes |
|---|---|---|
| `sync_state`, `local_version`, `remote_version`, `last_synced_at`, `last_*_modified_at` | **LOCAL** | The reconciler's own state. Never on wire. |
| `meta_msg_id`, `pin_dirty` | **LOCAL** | Sidecar bookkeeping (current sidecar id; orthogonal pin-dirty flag). |

---

## Adapter write-rules (invariants the Gmail/Apple adapter MUST honour)

1. **Durable ⇒ SHARED or PRESERVED.** If a fact must survive a device that only
   runs Apple Notes, it has to live in the body (PRESERVED) or in a header/label
   Apple authors (SHARED). A custom header or a sidecar will **not** survive Apple
   — sidecars are Jodd↔Jodd only.

2. **Preserve unknown bytes verbatim.** `save_note` is insert-new + trash-old
   (Gmail has no replace). The new message must reproduce *everything* Jodd
   doesn't understand — foreign HTML, OPAQUE-IN attachment parts — unchanged.
   ✅ **Headers: confirmed faithful (F3, 2026-06-11).** The template
   (`gmail.rs:1194-1208`) reproduces Apple's *complete* header vocabulary —
   `From`, UTI, `Content-Type`, `Content-Transfer-Encoding`, `Mime-Version`,
   `Date`, `X-Mail-Created-Date`, `Subject`, `X-Universally-Unique-Identifier`,
   `Message-Id` — and mirrors Apple's content-adaptive charset/CTE choice
   (ASCII→us-ascii/7bit, non-ASCII→utf-8/quoted-printable, `gmail.rs:1165-1185`).
   A Jodd-saved note is byte-indistinguishable from a Mac Notes 4.13 note; the
   earlier "fixed 5-header set / lossy writer" claim was wrong. The only mutation
   is cosmetic: `APPLE_MIME_VERSION` is hardcoded to `Mac OS X Notes 4.13
   (3146.121.7)` (`gmail.rs:184`), so re-saving a note authored on an older
   client bumps its version annotation (e.g. 4.12.6 → 4.13). Apple ignores
   `Mime-Version` content on read, so this is provenance-only, not a fidelity bug.
   ⚠️ **The real write-path lossiness is body MIME structure, not headers.** The
   template is hardcoded *single-part* `text/html`. The moment a note has a
   multipart body (inline image / attachment), `save_note` reconstructs it as
   one text/html part and **drops every non-HTML part.** This is item §F1, now
   sharpened: the danger isn't *classifying* attachments — it's that the current
   save will actively strip them on the next edit. Gate attachment write on this.

3. **OPAQUE-IN = pass-through or refuse.** Never re-encode a part Jodd can't
   author (drawings, scans, encrypted blobs). Echo the original bytes or decline
   the edit.

4. **Sidecars stay invisible to Apple.** `X-UTI: app.jodd.metadata` (never
   `com.apple.mail-note`) and live in the `Notes-Meta` label, never under
   `Notes/*`, so Apple neither round-trips nor surfaces them. `gmail.rs:1748,1754`.

5. **Title boundary is sacred.** Always `strip_leading_title` on read,
   `inject_title_into_body` on write. The Subject and the body-head title are two
   projections of one fact and must stay consistent.

6. **Created-date is identity-adjacent.** Never regenerate on edit; it anchors
   Apple's sort and INTERNALDATE.

---

## Forensic backlog (capture a real Apple message before classifying)

Same discipline that made checklists work: add the thing in Apple Notes on an
iPhone, let it sync to Gmail, pull the raw RFC822, read ground truth.

- **F1 — Attachments/images.** ✅ **CLOSED 2026-06-11** (captured a real iPhone
  note with bold/italic/underline/strike + an inline PNG; see master-table row).
  Format: `multipart/related; type="text/html"; boundary=Apple-Mail-<UUID>`.
  Part 0 = the note body, image referenced **not** by `<img>` but by
  `<object type="application/x-apple-msg-attachment" data="cid:X">`. Part 1 =
  `image/png; name=image.png; x-apple-part-url="X"`, `Content-Disposition:
  inline; filename=image.png`, `Content-Transfer-Encoding: base64`, `Content-Id:
  <X>` (the cid). Rich text = plain `<b><i><u><strike>` (SHARED). Mime-Version
  here is the iOS variant `1.0 (iOS/26.5 (23F77) dataaccessd/1.0)`. **Confirmed
  bug:** single-part `save_note` strips the PNG and leaves a dangling `<object
  cid>` → image destroyed on first Jodd edit. Still **unobserved:** Apple's
  proprietary types (drawings, scans, rich links) — likely OPAQUE-IN, capture
  separately when needed.
  - **Title handling: OK for plain titles, BROKEN for formatted titles.**
    A bare-text/`<div>`/`<span>` title strips fine (`strip_leading_title`
    Cases 1-3). But a title with inline markup — e.g. `new from iphone
    <b><i>เพิ่มภาษาไทย(bold italic)</i></b>` — matches **none** of the three
    cases (pre-`<div>` content isn't whitespace; `strip_prefix(plain_subject)`
    diverges at the first tag). Result: title rendered twice in Jodd, and on
    re-save `inject_title_into_body` prepends a plain duplicate → **two title
    rows on the wire → Apple shows a duplicated title.** Hits any Apple note
    with a styled title. **Fix:** compare the *tag-stripped text* of the first
    line (up to the first `<div>`/`<br>`) against the Subject, not the raw HTML.
    The Subject is always the tag-stripped concatenation of the title row.
  - **Thai round-trips cleanly.** Subject RFC2047 Q-encoded, body utf-8/QP —
    matches Jodd's non-ASCII save branch (`gmail.rs:1180-1184`). Verified.
  - **`Content-Id` is stable across edits** (same `03D58874…` cid on two
    revisions of this note) → the write path must preserve the original cid per
    attachment, not generate a fresh one.
  - **Gmail stores the MIME verbatim.** Apple-export vs. Gmail "download
    original" are byte-identical → the REST read path sees true Apple bytes, not
    a Gmail rewrite. Gmail also renders it natively (rich text + "One attachment")
    → the format is standard, not exotic.
  - **Provenance marker.** `Received: … gmailapi.google.com with HTTPREST`
    appears only on Gmail-API-inserted messages = Jodd's own saves; Apple's IMAP
    APPEND produces no such header. Reliable "touched by Jodd?" test — more so
    than `Mime-Version`, which Jodd spoofs to `Mac OS X Notes 4.13`.
- **F2 — Note-links.** How does an Apple note-link serialise in the body, and does
  it resolve on a *different* device? → unblocks PKM backlinks.
- **F3 — Header inventory.** ✅ **CLOSED 2026-06-11.** Sampled 40 pristine
  Apple-authored notes from `kaiwan.h@gmail.com` (6657-note mailbox, never
  Jodd-sidecar-touched) via the throwaway `dump_headers` command. Result: Apple's
  authored header set for a simple text note is exactly `{From, X-Uniform-Type-
  Identifier, Content-Type, Content-Transfer-Encoding, Mime-Version, Date,
  X-Mail-Created-Date, Subject, X-Universally-Unique-Identifier, Message-Id}`
  (plus a Gmail-added `Received` on API-inserted msgs). **Jodd reproduces all of
  it** — write-rule #2's header gap does not exist. The `Message-Id` UUID is
  distinct from the `X-UUID` (per-message vs. note identity), as expected.
  `X-Mail-Created-Date` correctly diverges from `Date` on Jodd-re-saved notes
  (creation vs. last-modified), confirming identity/created-date preservation.
  No `X-Apple-*` headers beyond the three Jodd already models. No multipart in
  the sample → attachments remain unobserved (see F1).

---

## What this tells the roadmap (the payoff)

Reading features off the manifest, before writing any of them:

- **Free — ride SHARED/PRESERVED, full Apple round-trip:** Inbox folder, daily
  notes, note templates (output), richer rich-text + highlight (`<mark>`),
  **tags via inline `#hashtag`** (migrate off the sidecar), and crucially
  **task/GTD done-state encoded in the body exactly like checklists** — durable
  *and* Apple-faithful.
- **Cross-Jodd only — SIDECAR, dropped by Apple:** pin (done), the current tag
  sidecar, saved-query definitions, anything genuinely Jodd-private.
- **The actionable GTD insight:** task *done-state* can be PRESERVED (body), but
  due/defer dates and `@context` are **DROPPED** unless encoded as inline body
  tokens. So if GTD metadata must survive an Apple device, encode it inline
  (`#context`, an inline `!due:2026-06-15`-style token) to inherit the
  PRESERVED/SHARED tier — **not** as a sidecar or custom header.
- **Jodd-local — can't and shouldn't round-trip:** smart folders, graph view,
  reminders-with-notification.
- **Blocked on forensics:** attachments (F1), backlinks (F2).
