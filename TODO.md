# Jodd — TODO

> Current release candidate: **v0.23.0**. For full architectural state and
> "Done / Remaining" lists, see `CLAUDE.md` — this file tracks the
> short-form roadmap and deferred polish items.

## 🟡 Next up (roadmap)

- [ ] **Microsoft/Outlook backend** — Graph API (`/me/messages`,
  `/me/mailFolders`) implementing the existing `Vertical` trait. Cache,
  conflict model, and sync worker are already backend-agnostic (proven
  by the LocalFS vertical); only a new `backend/graph/` module and
  `Box<dyn Vertical>` dispatch entry are needed. ~3–5 days.
- [ ] **Cross-account move** — UI in `NoteContextMenu.svelte` already
  shows the destination account picker but disables non-current targets.
  Implement after providers settle so cross-provider moves
  (e.g. Gmail → Outlook) land in one pass.
- [ ] **Reminders / Tasks** — Microsoft Graph `/me/todo/*` only (Gmail
  has no equivalent Apple uses). Backend-gated: visible only when a
  Microsoft account is connected; Jodd-local fallback for Gmail users.

## 🟢 Polish / deferred

- [ ] **A11y sweep on `NoteEditor.svelte`** — interactive divs at
  lines ~1384, 1396, 1456, 1458 need keyboard handlers + ARIA roles.
- [ ] **Legacy folder consolidation utility** — one-shot helper to
  migrate notes out of old `Notes/Lessons` (pre-rename) into
  `Notes/__Extracts__`. Currently user can move them manually.
- [ ] **Multi-workflow extract menu** — beyond "Extract", add
  "Summarize", "Action items", "Q&A from source", each filing under
  its own `__name__` workflow folder.
- [ ] **Cancellation toast on re-extract** — the right-click
  re-extract path generates a `request_id` and supports cancel via
  `cancel_extraction`, but there's no UI affordance yet (modal-only).
- [ ] **Cross-instance `folders.kind` sync** — `kind='system_workflow'`
  is currently local-only; two Jodd instances on the same account
  rediscover workflow status independently. Sidecar in `Notes-Meta`
  (same channel as pin sync) would make it durable.

## 💡 Future / open questions

- [ ] **Smart/dynamic folders** — query SQLite by `date` for
  "last 30 days", "previous month", saved searches. UI in
  `Sidebar.svelte`. ~1 day.
- [ ] **Image / attachment authoring** — display already round-trips;
  authoring new attachments from Jodd is unimplemented.

## ✅ Recently shipped (see CLAUDE.md for full history)

- v0.23.0 — allowlist-gated MCP note/task writing, safer concurrent writes,
  bounded agent responses, search fallback fixes, and Apple/LocalFS title
  round-trip fixes
- v0.22.0 — Android phone/tablet UI, Android release pipeline, app icon,
  and 16 KB page-size compatibility
- v0.17.6 — optional diagnostics/file logging; dedup-review perf fix
  (no more O(N×M) mailbox rescan)
- v0.17.2 — BYO Google OAuth credentials (App Settings modal), so
  pre-built binaries don't need to ship a shared client ID
- v0.17.1 — **LocalFS vertical**: `.eml`-file backend behind the same
  `Vertical` trait as Gmail, proving the backend abstraction is real
- v0.16.x — backend trait extraction (`Transport`/`NoteStore`/`Identity`/
  `Deriver`/`MetadataSidecar`/`Vertical`) out of the old monolithic
  `gmail.rs` — `AtRest` was designed but realized via `mime822` encode +
  the Gmail JSON decode path, never shipped as a separate trait; rename-safe
  `[[slug-uuid8]]` wikilinks
- v0.16.2 — content extraction polish: real cancellation, hyphen tags,
  `__Extracts__` rename, marker-stripping at display time, deletable
  legacy user folders
- v0.16.1 — content extraction workflow (HTTP + `claude -p` providers,
  `Notes/__Extracts__` filing, body-as-source-of-truth tags)
- v0.14.5 — backfill existing tags into sync pipeline
- v0.14.4 — cross-instance tag sync via Notes-Meta sidecars
- v0.14.3 — tag durability + dup-cleanup correctness
