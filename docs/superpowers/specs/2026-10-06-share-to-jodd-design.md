# Share to Jodd — one `jodd://capture` contract, a capture sheet, three platforms

**Status:** approved in conversation 2026-10-06 (§1–§4 each confirmed).
**Goal:** a person can send a link or text from any other app or the OS into
Jodd, on Android, macOS and Windows, and decide in Jodd what becomes of it.

## 1. Intent and decisions

| # | Decision | Why |
|---|---|---|
| D1 | Source = other apps / the OS sharing **one thing at a time** (not scripts, bulk import or service pulls) | the user's pick |
| D2 | Android, macOS **and** Windows in v1 | the user's pick |
| D3 | A **capture sheet** confirms before anything is written: Save as-is or Summarize with AI | the user's pick; also the drive-by guard (§4) |
| D4 | Approach A: one `jodd://capture?url=&text=&title=` contract; thin per-platform adapters | cheapest; testable without a device; can later back native Share-menu extensions unchanged |
| D5 | v1 carries **links and text** only | inserting attachments is unbuilt and Gmail-only (roadmap #1) |
| D6 | No folder picker: new captures land where Extract's do (`resolve_destination` → `Notes/Inbox`, or `Notes` on a folders-off backend) | Organize (Curate M2, #125) sorts the Inbox |
| D7 | The pending queue is **in memory** | persisting it would store untrusted payloads in the DB; accepted limit |
| D8 | Summarize reuses `LessonExtractModal` (URL ingest / Extract) via a prefill | no second AI pipeline. When agent-loop M3's `save_ingested_note` lands, it and this button must call the same ingest body |

Out of scope: native macOS Share extension, Windows Share Target (MSIX), a
global hotkey, images/files, a loopback HTTP endpoint.

## 2. Architecture

```
 Android share sheet ─(ACTION_SEND → rewritten ACTION_VIEW, MainActivity.kt)─┐
 bookmarklet / Raycast / Shortcuts / PowerToys / `open jodd://…` ────────────┼─▶ jodd://capture?…
 second process launch (Windows) ─(tauri-plugin-single-instance, deep-link)──┘          │
                                                                                       ▼
                                   deep-link on_open_url DISPATCHER (all platforms)
                                     ├─ OAuth callback → complete_oauth (unchanged)
                                     └─ jodd://capture → capture::parse_capture_url (pure)
                                                                │
                                              capture::Queue in AppState (cap 20, 10 s dedupe)
                                                                │
                                     emit("capture-received") + take_pending_captures()  ← pull, so a
                                                                │                          capture queued
                                                                ▼                          before the page
                                                      CaptureSheet.svelte                  listens is not lost
                                          Save as-is ─▶ save_capture(account, id, title)   (gotcha #6, #32)
                                          Summarize  ─▶ extractPrefill → LessonExtractModal
                                          Discard    ─▶ discard_capture(id)
```

## 3. Components

### 3.1 `capture.rs` (pure, tested on Linux CI)

- `CapturePayload { url: Option<String>, text: Option<String>, title: Option<String> }`.
- `parse_capture_url(&str) -> Result<CapturePayload, CaptureError>`: scheme
  `jodd`, host `capture` (also accepts `jodd:///capture` / `jodd:capture`
  spellings browsers produce), percent-decoding, `+` as space. At least one
  of `url`/`text` non-blank. Caps: text ≤ 100 000 chars, title ≤ 300, url ≤
  4 096. Over-cap → error, never truncate silently. Unknown keys ignored.
- `links(&payload) -> Vec<String>`: `url` plus `http(s)` URLs found in `text`,
  via the existing `ingest::urls` extractor, deduped, order-preserving.
- `body_html(&payload) -> String`: everything HTML-escaped; only `http(s)`
  URLs become `<a>`; text paragraphs as `<div>` lines. `javascript:`,
  `data:`, `file:` stay inert text.
- `default_title(&payload)`: explicit title → first non-blank text line that
  is not just a URL → URL host → `Shared <date>`.
- `Queue`: `push(payload, now) -> Option<id>` (None when full or duplicate of
  one pushed < 10 s ago), `pending()`, `get(id)`, `remove(id)`.

### 3.2 Commands (`capture_commands.rs`)

- `take_pending_captures() -> Vec<PendingCapture>` (id, payload, links,
  default title). Read-only: the sheet removes items only by acting.
- `save_capture(account_id, capture_id, title) -> ExtractedNoteDto`:
  `refuse_write(Notes)` **first**, then queue lookup, `resolve_destination`,
  `mint_uuid_for` (gotcha #18), `insert_local_new` (dirty), then remove from
  the queue. One synchronous SQLite write; the worker pushes.
- `discard_capture(capture_id)`.

### 3.3 Wiring

- `tauri.conf.json`: `deep-link.desktop.schemes = ["jodd"]`; `mobile` gains
  `{ scheme: ["jodd"], host: "capture" }` so `DeepLinkPlugin.isDeepLink()`
  forwards the rewritten intent.
- `tauri-plugin-single-instance` (feature `deep-link`), desktop only,
  registered **first**. Its callback focuses the main window.
- `register_all()` at startup on Windows (portable/dev runs).
- The `on_open_url` + `get_current()` block leaves its Android-only `cfg`;
  OAuth dedup unchanged.

### 3.4 Android adapter

- Manifest: `ACTION_SEND` + `text/plain` intent filter on `MainActivity`
  (`singleTask` already → `onNewIntent`, never a second activity).
- `MainActivity.kt`: in `onCreate` (before `super`) and `onNewIntent`, a SEND
  intent is replaced by `ACTION_VIEW jodd://capture?text=<EXTRA_TEXT>&title=<EXTRA_SUBJECT>`.
  No new JNI/reflective entry point, so R8 has nothing to strip (gotcha #31).
- Inference from `DeepLinkPlugin.kt` source, to be proven on a release APK.

### 3.5 Frontend

- `CaptureSheet.svelte`, mounted in `App.svelte`: drains on mount, on
  `capture-received`, and on `visibilitychange` → visible.
- Account list = `canWriteAccount`; default = current account.
- Summarize is NOT pre-checked against the provider (as built): the Extract
  modal already reports a missing or refused provider in its own words, and
  a second check in the sheet would be a copy of that rule to drift. The
  sheet hides while the modal is open; closing it without a note brings the
  still-queued capture back. The modal retires a capture only for a note
  made in the same opening (`fromCapture`, reset on open, never on close).
- `extractPrefill` store consumed once by `LessonExtractModal` on open.
- Settings → "Share to Jodd": bookmarklet with copy button, and the `open`
  one-liner for Raycast / Shortcuts / PowerToys.

### 3.6 macOS Share menu (added after the first live pass)

The "Share to Jodd" Shortcut (`src-tauri/assets/Share to Jodd.shortcut`,
generated by `scripts/gen-share-shortcut.py`, signed with `shortcuts sign
--mode anyone`) is marked Show in Share Sheet: it URL-encodes its input and
opens `jodd://capture?text=…`. Settings → Share to Jodd → "Add to Share menu"
(`install_share_shortcut`) writes it to a temp file and `open`s it; Shortcuts
asks before adding. A native Share extension stays out of scope.

The sheet (as built after that pass) frames "What was shared" (source site,
links that open in the browser, text) apart from "Save as a note", and offers
every Extract mode directly (`ExtractPrefill.workflow`).

### 3.7 Choose the folder; add to an existing note (2026-10-09)

"Save as a note" became two modes, **New note** and **Add to existing note**.

- **New note** gains a Folder picker. Its default sends `folder: null`, so it
  still goes through `resolve_destination` as described in §3.2. The default
  is labelled with where that will actually put the note
  (`filing::default_destination`, which creates nothing): "Inbox", or
  "Notes" on a backend that cannot create folders or when the Inbox is
  blocked. `list_capture_folders` returns that default plus
  `filing::chosen_destinations` (the root plus `candidate_folders`: never
  blocked, `deleted_pending` or `system_workflow`). `save_capture(…, folder)`
  checks a chosen folder against **that same list** and refuses one that has
  left it, rather than filing the note somewhere else (#162).
- **Add to existing note** searches with `search_notes` and leaves out notes
  marked `push_blocked_by_remote` (the editor locks those).
  `append_capture(account_id, capture_id, target_uuid)` calls
  `refuse_write(Notes)` first and refuses an inactive account, as
  `save_capture` does. It then resolves the uuid
  (`resolve_note_uuid`, gotcha #16) and appends with
  `apply_local_edit_with_retry`, which re-reads the note on each attempt. The
  appended block is `capture::appended_html`: a blank line,
  `— YYYY-MM-DD HH:MM —` in local time, then exactly what `body_html` would
  have saved. It is added with `mime822::append_html` (gotcha #37). The title
  and folder do not change. A note that is gone, `deleted_pending` or locked
  by the remote is refused.
- Both commands still take the capture's **id** (§4), never its content.
- Changing the account clears the folder and the target note in the same
  turn (gotcha #28).
- After a Save, the **next waiting item keeps where things go**: account,
  mode, folder and target note. Its title and shared text are its own, and a
  "Saved to … · next shared item" line shows that the item has changed. A
  kept folder the next item can no longer use goes back to the default. A
  share that arrives after the queue has emptied starts fresh. Live pass
  2026-10-09: two shares were waiting, the second appeared with the folder
  silently reset to Inbox, and a second Save filed it there.

## 4. Security

Any web page can open `jodd://capture`. Mitigations: nothing is written
without a tap; queue cap 20 + 10 s dedupe (no flood, no focus-stealing loop);
field caps; body built and escaped in Rust; the frontend passes a capture id,
never content, to `save_capture`.

Added after review (as built): a payload seen inside the dedupe window stays
deduped even after it was saved or discarded (`Queue.recent`), so a looping
page cannot re-open the sheet; Jodd is raised only for a capture that was
actually queued, at most once per 3 s; a full queue tells the person once per
window instead of dropping their share silently.

**Side effect on development:** single-instance is registered on every
desktop OS, so `npm run tauri dev` while an installed Jodd is running hands
its launch to that Jodd and exits. Quit Jodd first. This is the intended
cure for two sync workers on one SQLite file, not a regression.

## 5. Errors

Invalid payload → `capture-error` event → `ErrorBar` ("Couldn't read what was
shared: …"); the sheet does not open. Android: a recreated activity or a
relaunch from Recents re-delivers the original SEND, so `MainActivity` only
rewrites it on a fresh launch. No account yet → stays queued until
sign-in. Quit before acting → lost (D7).

## 6. Testing

Rust: parser table tests, dispatcher keeps OAuth untouched, queue cap/dedupe,
XSS corpus on `body_html`, `save_capture` refusal-before-write pinned by
source, and a temp-DB write test (Inbox / root on folders-off / uuid shape).
Vitest: sheet behaviour, cold-start drain, prefill consumed once. Live: macOS
built `.app` (bookmarklet + `open`, running and quit), Windows (single process
after a share), Android **release** APK (Chrome, YouTube; warm, backgrounded,
force-stopped; `probe_setup_delay`). Gates: the five CI commands.
