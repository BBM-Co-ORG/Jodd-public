# Direction — mantra, interop, gaps, and the build order

> Synthesis of the 2026-06-11 design session. Captures *why* (the mantra and its
> generalization), *what's missing* (gaps/features), *what's broken* (round-trip
> bugs found), and *in what order to build*. Companion to
> [FIDELITY-Gmail-Apple.md](./FIDELITY-Gmail-Apple.md) (the adapter's fact-by-fact
> faithfulness) and [FACT-SCHEMA.md](./FACT-SCHEMA.md) (the neutral fact store).

---

## 1. The mantra, generalized

The original mantra — "Apple Notes anywhere," round-trip correctness — turned out to
be an instance of a deeper pattern: **interop across multi-software / multi-device /
multi-platform, achieved through one shared, flexible, fact-based substrate.** That's
the **narrow-waist / hourglass** model (what made IP and the Web win): a thin neutral
interchange in the middle, many apps above, many transports below, and the key
property is **open-world preservation** — a participant carries data it doesn't
understand rather than dropping it.

"Fact-based" is precise, not loose: a fact is an assertion with identity + time that
any participant can store and re-emit without interpreting. Nodes ("note U has body
H") and **edges** ("A links-to B", "note child-of folder", "task done") are both
facts — which is why the `objects + edges` schema, JMAP's object model, and a triple
store are the same idea from three doors.

**The hard truth:** a shared fact base interops only as well as its *least faithful
participant*. Over the Gmail channel that's **Apple** — it preserves facts inside the
note body and under `Notes/*`, and silently drops everything else. Today Jodd lets
Apple's email schema masquerade as the neutral substrate, which strangles any fact
Apple can't express.

**The resolution (the product fork):**
- **(A) Faithful Apple Notes client** — round-trip is the master constraint; feature
  set bounded by what Apple Notes can represent.
- **(B) A second brain that uses Apple Notes as *one* store** — Jodd's SQLite is the
  real brain; the Gmail/Apple channel is one **adapter** that projects facts at its
  declared fidelity; round-trip becomes a *tier*, not the ceiling.

Direction is **(B)** — own a neutral fact model, demote Apple to the first adapter.
This makes "interop across many apps/devices" mechanical: every new participant
(Outlook, web, CLI, agent) talks to the waist; you write one projection per
transport. ✅ **Confirmed by the owner 2026-06-11.** Everything structural assumes (B).

---

## 2. Fidelity tiers (the durability doctrine)

Every custom feature must choose where its metadata lives. Jodd had already solved
this twice ad hoc (checklists, pin); named, the spectrum is:

| Tier | Mechanism | Survives Apple-only device? | Cross-Jodd? | Examples |
|---|---|---|---|---|
| **SHARED** | header / label / in-body HTML Apple authors | ✅ | ✅ | title, body, folders, `#hashtags`, rich text, **images** |
| **PRESERVED** | in-body markup Apple keeps verbatim but never authors | ✅ | ✅ | checklist `checked=""`, **task/GTD done-state**, inline due tokens |
| **SIDECAR** | `Notes-Meta` message Apple ignores | ❌ | ✅ | pin, (current) tags, saved-query defs |
| **LOCAL** | SQLite only | ❌ | ❌ | sync_state, versions |

**Rule:** if a fact must survive an Apple device, it lives in the body (PRESERVED) or
a header/label (SHARED) — never a custom header or sidecar. This is what makes
GTD/PKM features round-trip: encode due-date/context as inline body tokens, not
sidecars.

---

## 3. Round-trip bugs found (Jodd corrupting faithful Apple notes on edit)

Both surfaced from real captured specimens; both are the same class.

- **Bug A — attachment strip. OPEN (highest priority).** `save_note` emits single-part
  `text/html`; editing any note with an image drops the image MIME part and leaves a
  dangling `<object cid>` → **image destroyed on first Jodd edit** (survives only in
  the trashed old message ~30 days). Apple's format is fully known (F1): `multipart/
  related`, body refs image via `<object type="application/x-apple-msg-attachment"
  data="cid:X">`, image part has `Content-Id: <X>`, stable cid across edits.
- **Bug B — formatted-title duplication. ✅ FIXED (commit on this branch).**
  `strip_leading_title`/`inject_title_into_body` did per-shape exact-HTML matching
  that failed on inline-styled titles → title shown twice + plain dup prepended on
  re-save. Now compare tag-stripped first-line text vs Subject. 8 tests.

---

## 4. Gaps & features surfaced (with the tier each rides)

Search already exists as a **substring filter over the in-memory loaded subset**
(`NoteList`), and in-note find/replace exists. The gap is an **index** under it.

- **Search-as-index (FTS5)** — fixes the real bug that body search only covers
  *hydrated* notes; substrate for trash-search, backlinks, saved queries. SHARED-read.
- **Trash / Recently Deleted** — ~80% there (deletes already go to Gmail Trash;
  needs untrash + a Trash view + tombstone instead of prune). Round-trips with
  Apple's own 30-day Recently Deleted.
- **Richer text toolbar** — underline/strike/headings/ordered-list. Renders +
  round-trips already (plain HTML, proven); only the *authoring* buttons are missing.
- **Quick capture / Inbox** — GTD/BuJo/CODE all start with frictionless capture;
  Jodd has none. Native folder + global hotkey. SHARED.
- **Tasks as a queryable layer** — checklist state is already PRESERVED/Jodd-
  authoritative in the body; missing the cross-note "all open tasks" index + view.
- **Saved queries** (generalize smart-folders) — one mechanism powers Today / due /
  `@context` / has-task / tagged. The JMAP `query` concept.
- **Tags: SIDECAR → inline `#hashtag`** — promotes tags to SHARED (round-trips to
  iPhone); currently a `tags___` sidecar, invisible to Apple.
- **Daily notes + templates** — BuJo spine; templates multiply everything.
- **Backlinks / note-links** — PKM core; needs F2 capture first (see §5).
- **Provider trait + Microsoft/Outlook** — the big structural refactor (existing
  roadmap #4); the fact store makes it "one projection per transport."
- **Per-fact HLC / field-level merge** — when multi-device editing makes whole-note
  keep-both spam conflicts.

---

## 5. Forensic status (capture real Apple notes before building)

- **F1 attachments** ✅ closed — format fully known (§3 Bug A).
- **F3 header inventory** ✅ closed — Jodd's writer is *faithful* (reproduces Apple's
  full header set + content-adaptive charset/CTE). Earlier "lossy writer" claim was
  wrong. `Received: … HTTPREST` reliably marks Jodd/API-inserted vs Apple IMAP.
- **F2 note-links** ⬜ **likely unavailable on this backend.** Owner could not find
  the "link to note" feature for the Gmail account (2026-06-11). Strong hypothesis:
  Apple Notes' note-linking is an **iCloud-only** feature and does not exist for
  IMAP/Gmail-backed accounts. If so, F2 resolves *negatively* — there is no Apple
  note-link fact to round-trip — and **backlinks become a Jodd-local convention:**
  inline `[[note title or uuid]]` in the body (PRESERVED tier — round-trips to Apple
  as visible text even though Apple won't make it clickable; Jodd parses it into
  `edges` and renders it as a link). To confirm: on an **iCloud** note try linking
  (type `>>` then a note title, or select text → Add Link), see if it's *also*
  unavailable on the Gmail account. Either way, backlinks no longer block on Apple.
- **Apple proprietary types** (drawings, scans, rich links) ⬜ unobserved — likely
  OPAQUE-IN; capture when attachments expand beyond images.

---

## 6. Decisions (resolved 2026-06-11)

1. **Direction (B)** — ✅ confirmed. Neutral fact model; Apple is one adapter.
2. **Attachment storage** — ✅ SQLite BLOB table. **Backup caveat (owner-raised):**
   - The attachment bytes also live in Gmail (the `multipart/related` message), so
     the SQLite blob is a **cache/replica**, re-derivable from Gmail — *except* a
     newly-added, not-yet-pushed attachment, which is sole-copy until the worker
     pushes it (same as unpushed note content today).
   - **Therefore:** any backup/export must capture unpushed attachments. Simplest
     backup = copy `jodd.sqlite3` (blobs included). A future export-to-Markdown must
     spill blobs to files. No backup/export feature exists yet (`db.rs:144` is the
     only DB-file reference) — so this is a forward design constraint, not a fix.
   - **DB-size hygiene:** key by `(account_id, note_uuid, content_id)` and store each
     image once (cid is stable across edits — proven), not per-revision; periodic
     `VACUUM`; keep file-cache+path as a documented escape hatch if media gets large.
3. **F2 note-links** — reframed: likely iCloud-only / unavailable on the Gmail
   backend (§5). Backlinks proceed as a Jodd-local `[[…]]` convention; no longer
   blocked on a specimen.
4. **Build order** — ✅ agreed (§7). Note: "Trash / Recently Deleted" (Tier 1 #3) IS
   the opening-discussion feature.

---

## 7. Recommended build order

**Tier 0 — correctness (stop active data loss):**
1. **Attachment preservation (Bug A)** — read retains parts → SQLite `attachments`
   table → write rebuilds `multipart/related` (original cid) → editor renders
   `<object cid>` ↔ `<img>`. *In progress; Bug B already shipped.*

**Tier 1 — cheap, high-value, round-trip-safe:**
2. **Search-as-index (FTS5)** — the #1 missing primitive; also closes the hydration
   completeness gap.
3. **Trash / Recently Deleted** — data-safety; mostly already plumbed.
4. **Richer text toolbar** — underline/strike/headings/ordered list.

**Tier 2 — structural enabler (incremental; unlocks the rest):**
5. **Fact-schema slice 1** — `edges` + backfill `child_of` (retires the D1 orphan
   fragility as a side effect) + seed `adapter_fidelity` from the manifest.
6. **Tags SIDECAR → inline `#hashtag`** — first `edges` consumer; promotes to SHARED.

**Tier 3 — productivity layer (GTD / BuJo / PKM):**
7. **Quick capture / Inbox.**
8. **Tasks-as-queryable-layer + Saved queries** (built together — same query engine).
9. **Daily notes + templates.**
10. **Backlinks** (after F2 capture).

**Tier 4 — big structural:**
11. **Provider trait + Microsoft/Outlook backend.**
12. **Per-fact HLC / field-level merge** (when multi-device conflict-spam appears).

Rationale: fix what silently corrupts data first; then the cheap primitives that make
daily self-use viable and all round-trip; then the fact-store spine that every
PKM/GTD feature needs; then the provider refactor. Each item is independently
shippable and reversible.
