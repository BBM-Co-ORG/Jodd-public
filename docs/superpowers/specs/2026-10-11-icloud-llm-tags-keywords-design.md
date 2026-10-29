# iCloud: LLM-suggested tags become a Keywords line (stage 1)

**Date:** 2026-10-11 · **Status:** design approved in chat, awaiting spec review
**Builds on:** PR #178 (`claude/cool-curie-2ad06d`, f3fc387) — `TagModel`,
`db::tags_for_account`, gotcha #38. **This PR opens against `main` only after
#178 merges** (rebase onto `main` first; no stacked PR).

## Problem

Since #178, a tag on an iCloud account is only Apple's inline hashtag object
(`<span data-jodd-inline="hashtag">`); plain `#word` text is not a tag, to
Apple or to Jodd. Several Jodd write paths still put LLM-suggested tags into
the body as a `<p>#tag #tag</p>` line. On iCloud that line now produces a note
that *looks* tagged and has no tags, in Jodd and in Apple's Tag Browser alike.

## Intent and success criteria

On an iCloud account, what Jodd writes, what Jodd shows and what Apple lists
must agree: no write path pretends to create a tag it cannot create.

- An Extract / LLM workflow / URL ingest into an iCloud note writes the LLM's
  suggestions as a plain, labelled **Keywords** line — readable in Notes.app,
  FTS-searchable, and never derived as a tag.
- The user is told, before running, that this is what happens on iCloud.
- jodd-mcp stops promising that `#hashtags` become tags on iCloud.
- Gmail, Microsoft, LocalFs and SSH behaviour is **byte-identical** to today.

Creating real Apple hashtags from Jodd is **stage 2**, a separate spec (see
"Stage 2" below).

## Confirmed write-path inventory

Checked on #178's head:

| Path | Writes `#tag` text? | Where |
|---|---|---|
| Extract / LLM workflow, new note | yes, via `tags_line` | `llm/markdown.rs` `assemble_note_body` ← `lib.rs` `save_workflow_envelope` |
| Action-items preview (meeting draft) | yes, via `tags_line` | `assemble_note_body` ← `lib.rs` `preview_action_items`; appended by `llm/meeting.rs` `append_reviewed` |
| Append to an existing note | yes, via `tags_line` | `append_to_note_body` ← `lib.rs` `append_llm_workflow_note`, `append_extract_note` |
| URL ingest | yes, via `tags_line` | `assemble_ingest_body` ← `ingest/run.rs` |
| jodd-mcp `create_note` / `update_note` | none of its own; tool descriptions *promise* `#hashtags` become tags | `jodd-mcp/src/main.rs` (param docs + `create_note` description) |
| `migrate_tags_to_body` | already skips iCloud (#178) | `db.rs` |
| `remember` | no — escapes `#` | `agent_memory/mod.rs` `escape_inline` |
| Curate / Organize apply | no tag writes | `curate/` |

Every LLM path goes through one private function, `llm::markdown::tags_line`.

## Design

### 1. Backend — one choke point

`tags_line(tags: &[String], model: TagModel) -> String`:

- **`TagModel::BodyText`**: unchanged — `<p>#a #b-c</p>\n` (leading `#`
  trimmed, inner whitespace → `-`, `escape_html`). Existing tests pin it.
- **`TagModel::InlineObjects`**: `<p>Keywords: a, b c</p>\n` — always the
  first block of the fragment, label exactly `Keywords: `, separator `, `.
  Per keyword: trim, trim leading `#`, keep inner whitespace, `escape_html`;
  drop empties. No keywords left → empty string (no line). The line contains
  no `#`, so `tags_for_account` derives zero tags from it and no repair path
  ever acts on it.

The fixed shape is deliberate: stage 2 can recognise these lines if it ever
offers "convert Keywords lines to real tags".

`assemble_note_body`, `append_to_note_body` and `assemble_ingest_body` each
gain a `model: TagModel` parameter (and the private `build_ingest_fragment`).
Every caller passes `db::tag_model_for_account(&account_id)` — the function
#178 added; nothing new decides "is this iCloud". `markdown.rs` takes the model
rather than an account id so it stays pure and testable without an accounts
file.

The "Tag persistence" comment in `lib.rs` (`save_workflow_envelope`) is
rewritten to state what each model does.

**Out of scope:** iCloud notes that already carry an old `<p>#tag</p>` line
from earlier Extracts are left alone. Rewriting them would be Jodd writing into
Apple notes by migration (gotcha #17: repair by re-reading, never by
migration), and since #178 that text is harmless.

**Unchanged and noted:** appending to an iCloud note that already holds a real
Apple tag is still refused at push time (`Unwritable::InlineObjects`, compose.rs
refusal 5). Stage 2 owns that.

### 2. Frontend — one hint, one component

All five workflows (`extract`, `summarize`, `action_items`, `expand_bullets`,
`transcript`) and URL ingest run from `LessonExtractModal.svelte`.

- New pure predicate `suggestedTagsBecomeKeywords(model: TagModel | undefined)`
  in `src/lib/editor/bodyTags.ts`, true for `'inline_objects'`. Same test as
  `tagsEditable` today but named separately, because the two can diverge in
  stage 2.
- The model comes from `$capabilitiesByAccount[acct]?.tag_model` (already
  populated since #178) — no new IPC.
- **Shown when** the account the run will write to is iCloud **and** the
  selected workflow can produce tags. The implementation plan determines that
  set by reading each workflow's envelope builder (a workflow whose envelope
  always carries `tags: []` gets no hint) and a test pins it. The plan also
  names which of the modal's account sources (`$currentAccount`,
  `previewAccount`, `consentAccount`) the run actually targets; the hint reads
  that same one.
- **Text** (muted, one line, near the run button): *"On iCloud, suggested tags
  are saved as a Keywords line. Jodd can't create Apple tags yet."* The UI is
  English-only; no i18n.
- Purely derived from store state: no awaits, no state mutated after an IPC.

### 3. jodd-mcp

- `list_accounts` output gains `tag_model` (`"body_text"` / `"inline_objects"`)
  per account, from `TagModel::for_backend(account.backend_kind)`, next to the
  existing `backend_kind`. Its serde strings are already pinned for the
  frontend in `backend/mod.rs`.
- Corrected text, everywhere the tools promise tags (`CreateNoteParams.
  body_markdown`, `UpdateNoteParams.body_markdown`, the `create_note`
  description; the `update_note` description gains the same clause): "#hashtags
  become tags on accounts whose `tag_model` is `body_text`; on `inline_objects`
  (iCloud) they stay plain text — Jodd cannot create Apple tags yet."
- **Not doing:** a warning in write-tool results. `list_accounts` is already
  "call this FIRST" and now carries the fact as data.
- **Rollout:** the corrected strings only reach agents once the installed
  binary is rebuilt — rebuild, `cp` to temp, `mv` into `~/.local/bin`,
  `xattr -c`, verify `initialize`. Warn before anything opens the real DB (a
  rebuilt binary triggers a keychain prompt).

### 4. Docs

- **CLAUDE.md: untouched.** It is at 40 597 chars, past the ~40 000 budget, and
  row 38's trigger stays accurate.
- **docs/GOTCHAS.md #38**, corrected in place: LLM write paths render tags via
  `tags_line(tags, TagModel)`; `InlineObjects` writes `<p>Keywords: …</p>`; old
  iCloud `#tag` lines are deliberately left (gotcha #17).
- **docs/BACKEND-ICLOUD.md**, `#hashtag` bullet: stage 1 shipped (Keywords
  line); creating a real hashtag is stage 2; the two open questions kept
  verbatim.
- **docs/ROADMAP.md #4b**: a "Hashtag creation (stage 2)" sub-item (below).
- After the docs land: `/graphify --update`, asking before copying anything
  into the main checkout's `graphify-out`.

## Testing

TDD; each new test fails before its change.

- `markdown.rs`: `tags_line` under both models — BodyText byte-identical to
  today's output; InlineObjects shape, `#` trimming, inner-space retention,
  HTML escaping, empty-tag dropping, no line for no tags. One test per public
  wrapper under `InlineObjects` asserting `db::tags_from_body_for(InlineObjects,
  body)` is empty and the body contains `Keywords: `.
- A caller-level test that an Extract into an iCloud account id yields a body
  with the Keywords line and zero `note_tags` rows; the same into a Gmail
  account still yields the tags.
- vitest: `suggestedTagsBecomeKeywords`; a mounted `LessonExtractModal` test —
  hint present for an iCloud account on a tag-producing workflow, absent for
  Gmail and for a workflow that produces no tags (gotcha #28: mount it).
- jodd-mcp: `list_accounts` reports `tag_model` for an iCloud and a Gmail
  account.
- Gate set, in CI's order: `node scripts/gen-changelog.mjs`, `cargo test
  --workspace` (prefix `DEVELOPER_DIR=/Library/Developer/CommandLineTools` if
  linking fails), `npx vitest run`, `npx svelte-check --threshold error`,
  `npm run build`.

### Live check (iCloud)

1. Copy the main checkout's `.env` into the worktree; `tauri dev`.
2. One Extract into a **new** note in `Notes/Jodd.M3.test` on
   `icloud:kaiwan@me.com`. Never touch the fixture "Tag lab 2026-10-11"
   (`C7793F2B-C4C3-4B28-82E3-D2248BD4899B`).
3. Check immediately and again **≥ 20 minutes later**: Notes.app shows the
   `Keywords:` line; Apple's Tag Browser gains nothing; Jodd shows no chips; the
   note is neither duplicated nor corrupted. Observe only — never edit the note
   through Apple Notes.
4. Delete the worktree's `.env`.

## Stage 2 — real Apple hashtags (separate spec, not this PR)

Recorded so the next spec starts from facts, not guesses:

- Anatomy measured 2026-08-27 (BACKEND-ICLOUD.md `#hashtag`): find-or-mint a
  `Hashtag` record **by standardized text**; one `InlineAttachment` per
  occurrence; `U+FFFC` + an `attachment_info` run in the document, without
  `systemAttachmentClassName` / `systemAttachmentData`.
- Open, needs a live re-measure: whether `records/modify` is one operation or
  three, and the `Note` reference `action`. icloud-md@0.6.2's `dist/notes/*.js`
  is the wire-shape reference.
- Must narrow `Unwritable::InlineObjects` (compose.rs refusal 5), or a note Jodd
  just tagged becomes unwritable to Jodd on its next edit.
- Would flip the `InlineObjects` arm of `tags_line` from "Keywords line" to
  "hashtag objects", and could offer converting existing Keywords lines.
