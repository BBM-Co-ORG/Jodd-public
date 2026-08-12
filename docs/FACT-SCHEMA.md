# Canonical Fact Schema — the waist

> **What this is.** The shared, transport-neutral object model that the
> [Fidelity Manifest](./FIDELITY-Gmail-Apple.md) hangs off. Today Jodd's truth is
> `notes` + `folders` tables shaped like *an Apple-Notes-over-Gmail message*. That
> makes Apple's projection masquerade as the neutral substrate, so any fact Apple's
> schema can't express gets strangled at the waist. This schema flips it: **Jodd
> owns a neutral fact store; the Gmail/Apple channel becomes one adapter that
> projects facts onto the wire at its declared fidelity.**
>
> **Design stance — migration, not rewrite.** `notes` and `folders` stay. They
> become the *hot materialized view* over the fact store, exactly as SQLite is
> already a cache of Gmail. The new tables are additive. Nothing existing breaks on
> day one; features migrate onto the fact store one at a time.
>
> Status: **design draft for reaction.** DDL is illustrative, not final.

---

## Three things a shared fact base needs that Jodd lacks today

(From the [architecture discussion](../CLAUDE.md) — restated as schema requirements.)

1. **Relations as first-class facts.** "A links-to B", "note child-of folder",
   "note tagged X" are *edges*. Today folder-parent is *derived* from a label
   string (`Notes/a/b`), which is why Gmail allowing `A/B` without `A` produced the
   D1 orphan bug — the parent was never an asserted fact. Edges fix that.

2. **Per-fact time, not per-note.** Today versioning is `local_version` /
   `remote_version` on the whole note. Two devices editing *different fields* of one
   note trip the keep-both conflict machinery for a non-conflict. Facts carry their
   own clock → field-level merge.

3. **Fidelity as data, not lore.** The manifest must be a *table the reconciler
   reads*, so "where does this fact go on the wire, and where's its residue" is a
   lookup, not tribal knowledge scattered across `gmail.rs` comments.

---

## The tables

### 1. Object identity — already exists, just named

Every note/folder/(future)task is an **object** keyed by `(uuid, account_id)` —
which is *already* the `notes`/`folders` primary key. No new table needed; the
object registry is logical. `type` distinguishes them.

```
-- conceptual; realised as the union of typed tables + the overlay below
object := (uuid, account_id, type)   type ∈ {note, folder, task, ...}
```

The cornerstone identity fact — `X-Universally-Unique-Identifier` — *is* `uuid`.
Apple handed us cross-peer identity for free; the whole schema is built on it.

### 2. `edges` — relations as facts

```sql
CREATE TABLE edges (
  src_uuid     TEXT NOT NULL,
  src_account  TEXT NOT NULL,
  kind         TEXT NOT NULL,          -- 'child_of' | 'links_to' | 'tagged' | 'has_task' | ...
  dst_uuid     TEXT NOT NULL,          -- for 'tagged', dst_uuid = the tag's canonical id
  dst_account  TEXT NOT NULL,
  clock        TEXT NOT NULL,          -- HLC of the assertion (see §Clock)
  origin       TEXT NOT NULL,          -- which adapter/device asserted it
  sync_state   TEXT NOT NULL DEFAULT 'dirty',
  PRIMARY KEY (src_uuid, src_account, kind, dst_uuid, dst_account)
);
CREATE INDEX edges_by_dst  ON edges(dst_uuid, dst_account, kind);  -- backlinks: who points at me
CREATE INDEX edges_dirty   ON edges(sync_state) WHERE sync_state != 'clean';
```

- **Backlinks** = `SELECT src FROM edges WHERE dst_uuid=? AND kind='links_to'` — free.
- **Folder hierarchy** becomes `child_of` edges → the D1 fragility disappears; an
  orphan is a missing `child_of`, detectable and repairable as a fact.
- **Tags** = `tagged` edges → "all notes tagged #x", tag rename, tag graph — all
  one query. (The label `Notes/a/b` is still the *projection* of the `child_of`
  chain onto the Gmail adapter; see §Views.)

### 3. `attributes` — the extensible per-object overlay (EAV)

For facts that don't earn a dedicated column — due/defer dates, `@context`, review
date, source URL, etc. Keeps the schema open-world without a migration per feature.

```sql
CREATE TABLE attributes (
  uuid        TEXT NOT NULL,
  account_id  TEXT NOT NULL,
  key         TEXT NOT NULL,           -- 'due' | 'defer' | 'context' | 'source_url' | 'pinned' | ...
  value_json  TEXT NOT NULL,
  clock       TEXT NOT NULL,           -- HLC — enables field-level last-writer-wins
  origin      TEXT NOT NULL,
  sync_state  TEXT NOT NULL DEFAULT 'dirty',
  PRIMARY KEY (uuid, account_id, key)
);
CREATE INDEX attr_by_key   ON attributes(account_id, key, value_json);  -- saved queries
CREATE INDEX attr_dirty    ON attributes(sync_state) WHERE sync_state != 'clean';
```

Hot facts (title, body_html, pinned) **stay as columns on `notes`** for read speed;
`attributes` is for the long tail. A fact can be promoted column→overlay or back
without changing its meaning.

### 4. `adapters` + `adapter_fidelity` — the manifest as data

This is the [Fidelity Manifest](./FIDELITY-Gmail-Apple.md) turned into rows the
reconciler queries.

```sql
CREATE TABLE adapters (
  id      TEXT PRIMARY KEY,            -- 'gmail-apple'
  kind    TEXT NOT NULL                -- 'email-notes' | 'graph' | 'local-md' | ...
);

CREATE TABLE adapter_fidelity (
  adapter_id     TEXT NOT NULL,
  fact_type      TEXT NOT NULL,        -- 'note.title' | 'note.body' | 'edge.tagged' | 'attr.due' | ...
  class          TEXT NOT NULL,        -- SHARED | PRESERVED | SIDECAR | DROPPED | OPAQUE_IN | OFF_CHANNEL
  wire_location  TEXT,                 -- 'header:X-...' | 'subject' | 'body' | 'label' | 'sidecar:Notes-Meta' | NULL
  residue_target TEXT,                 -- NULL | 'sqlite' | 'sidecar:Notes-Meta' | 'body-json'
  PRIMARY KEY (adapter_id, fact_type)
);
```

Seed rows for `gmail-apple` are a direct transcription of the manifest table —
e.g. `('gmail-apple','note.body','SHARED','body',NULL)`,
`('gmail-apple','edge.tagged','SHARED','body',NULL)` *(after the #hashtag
migration)* vs the current `('gmail-apple','edge.tagged','SIDECAR','sidecar:Notes-Meta','sidecar:Notes-Meta')`,
`('gmail-apple','attr.pinned','SIDECAR','sidecar:Notes-Meta','sidecar:Notes-Meta')`,
`('gmail-apple','attr.due','DROPPED',NULL,'body-json')` ← the actionable GTD rule:
encode inline or it won't survive Apple.

### Clock — Hybrid Logical Clock (HLC)

`clock = "<wall_ms>:<counter>:<device_id>"`. Wall-clock for human-meaningful
ordering, counter to break ties within a millisecond, device id to break ties
across devices deterministically. Comparison is lexical on the tuple.

- Per-fact (edge/attribute/column-group) HLC → **field-level merge**: two devices
  touching different keys never conflict; same key → higher HLC wins (or, for facts
  the user shouldn't silently lose, fall back to today's keep-both *at field
  granularity* instead of whole-note).
- `Date.now()` is available in the Tauri app (only the workflow runtime forbids it),
  so HLC is cheap to stamp on write.

---

## How `notes` / `folders` become a view

`notes` is no longer *the* truth — it's the **materialized projection** of a
note-object's SHARED/PRESERVED facts, denormalised for the UI's hot path:

| `notes` column | Backing fact |
|---|---|
| `title`, `body_html`, `date`, `x_mail_created_date` | SHARED facts (columns stay — hot) |
| `label` | the `child_of` edge chain, flattened to a path |
| `pinned` | `attributes(key='pinned')` cache (or stays a column) |
| `sync_state`, `*_version` | per-object roll-up of the facts' own `sync_state`/clocks |
| *(checklist checked-state)* | **not a separate fact** — lives inside `body_html` (PRESERVED) |

A note's tags/links/due-date are *no longer crammed into the note row* — they're
edges/attributes, queried alongside. The UI keeps reading `notes` for speed; the
reconciler reads the facts.

---

## What changes in the reconciler

Today: `save_note` = insert-new + trash-old on *one* transport, body-only.
Under the fact store, **save = project each dirty fact onto every subscribed
adapter at its declared fidelity:**

```
for each dirty fact f of object O:
    for each adapter A subscribed to O.account:
        (class, wire, residue) = adapter_fidelity[A, f.type]
        match class:
            SHARED | PRESERVED -> write f into wire (header/subject/body/label)
            SIDECAR            -> upsert sidecar in residue label
            DROPPED            -> write into residue (body-json or sqlite); skip wire
            OPAQUE_IN          -> never authored by Jodd; pass through on rebuild
            OFF_CHANNEL        -> noop (Apple owns it)
```

Read becomes the dual: ingest each adapter, map wire→facts via the same table,
merge by HLC. The manifest stops being prose and becomes the control flow.

---

## Migration path (incremental, non-breaking)

1. **Add `edges` + `attributes` + `adapters`/`adapter_fidelity`** (migrations #5–#7).
   Seed `adapter_fidelity` from the manifest. Nothing reads them yet.
2. **Backfill `child_of` edges** from existing `notes.label` strings; keep `label`
   as the projection. Folder ops now assert edges; D1 orphan check becomes a fact
   query.
3. **Move tags off the sidecar** onto `tagged` edges + inline `#hashtag` projection
   (promotes tags SIDECAR→SHARED — full Apple round-trip).
4. **First overlay attribute = a GTD due-date**, projected `body-json` (DROPPED→
   inline residue) so it survives an Apple device — proves the open-world path.
5. **Per-fact HLC** replaces whole-note versioning for the migrated fact types; the
   conflict reconciler narrows from note-granular to field-granular.

Each step is shippable alone and reversible. The fact store earns its place one
feature at a time rather than as a big-bang rewrite.

---

## Open questions (for reaction)

- **EAV vs. typed tables for tasks.** Tasks could be `attributes` on a note, or a
  first-class `type='task'` object with its own edges (`has_task` from the note).
  Leaning first-class once tasks get due-dates/state — they're queried independently
  ("all open tasks"), which an overlay makes awkward.
- **Does the HLC need vector clocks?** For 2–3 devices, HLC last-writer-wins +
  field-granular keep-both is almost certainly enough. Vector clocks only if true
  concurrent-merge correctness across many writers becomes a goal.
- **Where does the `<body style="…">` wrapper live?** It's an Apple-authored
  SHARED fact (F3); the projection must reproduce it verbatim, which argues for
  storing the raw inbound body envelope, not just inner HTML.
