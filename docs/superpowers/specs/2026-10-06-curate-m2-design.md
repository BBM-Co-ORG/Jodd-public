# Curate (M2) — Jodd finds, the AI decides, the user approves

Status: design decided in conversation 2026-10-06; the user asked to finish
without further questions, so the open choice (§2 D5) was settled on the
stated recommendation. Parent: [agent knowledge loop](2026-10-06-agent-knowledge-loop-design.md) §4, M2.

## 1. Intent

The user's existing notes have problems they cannot express as a command.
They named three to fix first:

1. **Content duplicates** — different notes about the same thing (measured:
   three notes from one YouTube video, one covering 16% of it).
2. **Misfiled notes** — notes left in `Notes` or `Notes/Inbox` that belong
   in a topic folder.
3. **Secrets in notes** — passwords, PINs and API secrets in plain text
   (measured in the M1 UAT: four notes, reachable by any agent until
   `Notes/Personal` was hidden; one is still in an unhidden folder).

**Success:** one button finds a small batch of these, each with evidence and
a recommended fix; the user sees exactly what will change, approves or
dismisses each, and nothing is ever lost (a trashed note stays recoverable
in Gmail's trash).

## 2. Decisions

| # | Question | Decision |
|---|---|---|
| D1 | Problems in v1 | Content duplicates, misfiled notes, secrets |
| D2 | Who decides | **Jodd's own configured AI**, from an *Organize* button; the proposal store records a `source`, so agent-submitted proposals (MCP) can be added later without a new store |
| D3 | What a merge does | Per duplicate group the AI proposes **keep one, trash the rest** or **append the rest to one**; the user can switch before approving. No AI-rewritten merged note in v1 |
| D4 | Where proposals live | SQLite, on the device that ran the scan |
| D5 | Review on Android | **Desktop only in v1** — the scan needs a desktop AI provider anyway, and SQLite does not sync |
| D6 | Batch size | At most `MAX_PER_KIND` (8) new proposals per kind per scan — a reviewable batch, not a backlog |

## 3. Components

```
jodd_lib::curate
  candidates.rs   deterministic: duplicate groups, misfiled notes, secret findings
  secrets.rs      the secret detector (patterns + entropy), masks what it shows
  decide.rs       AI step: one chat call per duplicate group; suggest_folder per misfiled note
  store.rs        curate_proposals table (migration 21): insert, list, set status
  apply.rs        the execution engine: keep-one, append, move, move-to-hidden
src-tauri/src/curate_commands.rs   Tauri: scan, list, preview, apply, dismiss
src/lib/components/CurateReview.svelte   the review screen (Sidebar → Views → Organize)
```

### 3.1 Candidates (no AI)

Over the account's notes, excluding `deleted_pending` rows, hidden folders
(M1 `mcp_scope`) and the agent workspace `Notes/__Agent__`:

- **Duplicate groups** — two notes join a group when any holds:
  - they cite the same URL (`edges.rel = 'cites'`, the ingest Sources);
  - their normalized titles are equal;
  - the trigram Jaccard similarity of their first 2 000 characters of text
    is ≥ 0.6.

  Groups are connected components, capped at 4 notes. Pairs already
  dismissed (by note uuids) are skipped.
- **Misfiled** — notes whose folder is exactly `Notes` or `Notes/Inbox`,
  newest first. Notes created in the last 24 h are skipped, since they are
  probably still being written.
- **Secrets** — `secrets::scan(text)` over title + body text (§3.2).

### 3.2 Secret detector

Matches, case-insensitive, on the text form of the note:
- `password|passwd|pass|pwd|pin|passcode|secret|api[_ -]?key|token|app[_ ]?secret`,
  followed by `:` or `=` and a value of ≥ 4 non-space characters;
- known key shapes: `sk-[A-Za-z0-9]{20,}`, `AKIA[0-9A-Z]{16}`,
  `ghp_[A-Za-z0-9]{30,}`, `xox[bp]-…`, `-----BEGIN … PRIVATE KEY-----`;
- a token of ≥ 24 characters from `[A-Za-z0-9+/=_-]` with Shannon entropy
  ≥ 4.0 bits per character on a line that also has one of the keywords above.

A finding reports **the kind and a masked preview** (`pass: 8H••••••`). The
secret value itself never leaves `secrets.rs`: it is never in a proposal, a
log line or an IPC payload. The proposed fix moves the note to a hidden
folder (§3.4).

### 3.3 Decide (AI)

- **Duplicates:** one `chat` call per group. System prompt
  `CURATE_DUPLICATES_PROMPT` (in `llm/prompt.rs`, so it is hashed into
  `prompt_version`). The input is each note's uuid, title, folder, date,
  character count, cited URLs and its first 3 000 characters, framed as
  data. Output JSON, parsed with `parse_envelope_lenient`:
  `{verdict: duplicate|related|distinct, method: keep|append, keep: <uuid>, reason}`.
  `distinct` creates no proposal. `related` creates no proposal in v1
  (links are a later kind).
- **Misfiled:** the existing `suggest_folder` over the account's folder list
  minus hidden and workspace folders. A suggestion equal to the current
  folder, or absent, creates no proposal.
- **Secrets:** no AI.

The whole scan runs under `llm::receipts::run` (so it gets AI limits and a
receipt). Its attempt pre-check is the number of groups plus misfiled notes;
when that exceeds Max attempts, the batch is trimmed rather than refused.

### 3.4 Apply (the execution engine)

A proposal records, for every note it touches, `{uuid, title, label,
local_version}` at proposal time. **Apply first re-reads every note and
refuses with `Stale` if any is missing or its `local_version` changed**; the
proposal is then marked `stale`, and a rescan proposes afresh.

| Action | Effect, all SQLite-first (local-first doctrine; the worker pushes) |
|---|---|
| `keep` | the other notes → `deleted_pending` (Gmail trash). Inbound `[[Old title]]` links in other notes are rewritten to `[[Kept title]]` |
| `append` | the kept note's body + per other note: `<hr><h2>Merged from: {title}</h2>` + its body; then as `keep` |
| `move` | `move_notes_batch` to the suggested folder |
| `hide` | move the note to the account's first hidden folder; if there is none, create `Notes/Private` and hide it (M1 `mcp_scope::set_hidden`) |

Capability gates come first: `Write::Notes` for append and link rewrites,
`Write::Relocate` for trash and move, `Write::Folders` to create
`Notes/Private`. An account that cannot do the action gets no such proposal.

### 3.5 Store

Migration 21 adds:

```sql
CREATE TABLE curate_proposals (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account_id TEXT NOT NULL,
  kind TEXT NOT NULL,          -- duplicate | misfiled | secret
  payload TEXT NOT NULL,       -- JSON: notes[], action, alternatives, reason, evidence
  source TEXT NOT NULL,        -- 'jodd-ai' (later: 'agent:<client>')
  status TEXT NOT NULL,        -- pending | applied | dismissed | stale | failed
  created_at INTEGER NOT NULL,
  decided_at INTEGER,
  error TEXT
);
CREATE INDEX idx_curate_account_status ON curate_proposals (account_id, status);
```

A scan never duplicates a pending proposal for the same set of uuids, and
never re-proposes a dismissed one.

### 3.6 Review UI (desktop)

**Sidebar → Views → Organize** opens a modal for the current account:

- **Find problems** runs the scan, with progress and then a summary
  ("3 duplicate groups, 5 misfiled, 2 secrets").
- Cards are grouped by kind. Each card shows:
  - the notes involved, with title, folder and date;
  - the reason and the evidence (a shared URL, the similarity, the masked
    secret kind);
  - the action, with a selector for duplicates (Keep *A* / Keep *B* / Append
    into *A*);
  - a **Preview** that shows the text of each note side by side, and for
    append the resulting note.
- **Approve** applies and refreshes the note list (`$refreshNotes`, gotcha #6).
  **Dismiss** records the decision.
- The modal is hidden on Android.

## 4. Error handling

| Situation | Behavior |
|---|---|
| No AI provider configured | Secrets still scan; duplicates and misfiled are skipped, and the summary says why |
| AI call fails or the budget is reached | That group or note is skipped; the summary counts the skipped ones |
| Stale proposal | Refused, the card shows "changed since — rescan", status `stale` |
| A note is push-blocked or the backend refuses later | The existing push-blocked path (gotcha #14) surfaces it on the note; apply itself succeeded locally |
| Approve twice / concurrently | status transition is `pending → applied` with a compare in the UPDATE; the second gets "already decided" |

## 5. Testing

- **Unit:**
  - candidates: shared cite, equal title, similarity threshold, hidden and
    workspace excluded, group cap, dismissed pairs skipped;
  - secrets: every pattern, entropy, no false positive on prose, the mask
    never contains the value;
  - decide: fake provider JSON, lenient parse, distinct and related make
    nothing;
  - apply: keep, append, move and hide, stale refusal, link rewrite,
    capability refusal;
  - store: status compare-and-swap, no duplicate pending.
- **Frontend:** helpers plus a mount test of the card (preview, action
  switch, approve calls apply and then refresh).
- **Live UAT:** the scan on the user's account finds the three-notes-one-video
  group and the `BBTrade` secret note. Applying a proposal on throwaway
  duplicates the UAT creates reaches Gmail (trash).

## 6. Out of scope

AI-rewritten merges, link proposals (`related`), folder clean-up (empty or
near-duplicate folders), contradiction and staleness detection, MCP
`submit_proposal`, review on Android, scheduled scans.

## 7. As built — review fixes and known limitations (2026-10-06)

The fresh-agent review found, and this branch fixes:
- Microsoft deletes permanently, so duplicate proposals are neither made nor applied there (`has_trash`).
- Notes holding a secret are never sent to the AI (duplicates or misfiled).
- A remote edit keeps `local_version`, so staleness also compares a content hash.
- Append refuses notes with inline images (`cid:`). The AI's append verdict becomes keep for such groups.
- Append refuses a push-blocked kept note.
- Hide is offered only where apply can do it, and uses only a hidden folder that exists.
- An unreadable scope file aborts the scan and the hide (it fails closed).
- The O(n²) pass skips pairs whose size ratio cannot reach the threshold.
- Secrets: whole-word keywords, placeholder values (`none`, `true`, a URL…) ignored, and nothing of a value under 12 characters shown in the mask.
- `curate_dismiss` checks the account.

Deferred:
- Keep drops the trashed notes' Jodd-local tags and pin.
- Links inside the merged text itself still name the trashed titles.
- If a link rewrite fails after an append, the kept note is already merged and a later append would add the "Merged from" blocks again.
- iCloud's per-note writability (gotcha #24) is consulted only through `push_blocked_reason`.
- "Same title" chains through generic titles, such as "Meeting notes".
