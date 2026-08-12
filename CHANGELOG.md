# Changelog

All notable changes to Jodd are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); versions follow the app version.

## [Unreleased]

## [0.23.1] - 2026-08-12
### Fixed
- **Closed 12 open dependency security alerts** (1 critical, 4 high, 7 medium), verified against `Jodd-public`'s Dependabot alerts and an independent `npm audit` pass. `vitest` 2→3.2.6 closes the critical Vitest UI arbitrary file read/execute CVE and, by widening vitest's own `vite` peer range, also dedupes away a vulnerable nested `vite`/`esbuild` copy vitest 2.x had been installing alongside the already-patched top-level `vite`. `postcss`, `undici` (via `jsdom`), and `nanoid` are pinned to patched versions via `overrides`. All affected packages are dev/build-time only — no runtime or feature code changed. `npm audit` is now clean (0 vulnerabilities). A Rust `glib` medium-severity alert is tracked but not fixed here: it's pinned transitively by Tauri's own GTK-rs stack (not by anything in this repo), and Linux/GTK isn't a shipped release target.

## [0.23.0] - 2026-08-12
### Added
- **jodd-mcp can now write to your vault**, not just search it. Six new tools, gated by a per-account allowlist of folders you set yourself in `mcp_write_scope.json` (deny-by-default — an account with no entry gets no write access at all): `list_accounts` (the accounts and folders an agent may touch), `create_note`, `update_note` (append is the default and never touches existing bytes; a full replace is refused on any note holding a checklist or attachment unless you force it), `create_folder`, `list_tasks`, and `set_task_state` (ticks or unticks one checklist item without rewriting the rest of the note). An agent writes plain Markdown — never raw HTML — and Jodd converts and sanitizes it before it ever reaches a note body; GFM tasklists become real, tickable Jodd/Apple checklist rows, and `#hashtag`s in the text become tags the same way they already do everywhere else. Works identically for Gmail and local-vault (LocalFs) accounts. **Setup instructions — where `mcp_write_scope.json` goes, what a grant covers, and what it does not protect — are in [`jodd-mcp/README.md`](jodd-mcp/README.md).**

### Changed
- **Trust copy and platform status now match the implementation.** The app and current-state documentation identify Jodd as a BBMedia Developer Preview, disclose the local SQLite cache, list Android as available, and distinguish attachment display/round-trip from authoring new attachments.

### Fixed
- **Two data-loss windows closed for anything writing to `jodd.sqlite3` outside the app** — `jodd-mcp` surfaced both, but both apply to the app itself too. `mark_pushed` is now version-guarded, so a note edited while the sync worker is mid-push no longer has that edit silently reverted when the push completes. `Db::open` now sets `PRAGMA busy_timeout`, so a write racing another process's write waits briefly instead of failing immediately with `SQLITE_BUSY`.
- **MCP write operations now fail safely when their target is ambiguous or stale.** Agent responses and previews are bounded, note/folder inputs reject unsafe paths, and `set_task_state` requires the expected task text before it changes a checklist item, preventing an agent from ticking the wrong row after a concurrent edit.
- **Multi-word search is more forgiving.** If an exact phrase has no result, Jodd falls back to matching the individual terms; cross-account results also keep notes that share a UUID but belong to different accounts.
- **Apple Notes and Local Folder titles round-trip without disappearing or duplicating.** Title wrappers are now injected and stripped consistently at the storage boundary.
- **A second local writer editing the same note could silently discard the first writer's edit.** `Db::apply_local_edit` — used by the app's own editor, `jodd-mcp`'s write tools, Extract's re-ingest, and auto-link's appends — had no version guard, unlike `mark_pushed` above. `apply_local_edit_versioned` closes it: the four append-style callers now retry against freshly re-read state when they lose the race; the App's own full-body `save_note` can't safely retry (there's nothing to recompute), so it surfaces an explicit conflict error instead. Making that guard actually catch the real case (two *separate* saves, not just two writes inside one call) required threading the note's `local_version` from when the editor loaded it through to the save — re-deriving it inside `save_note` itself, which an earlier pass at this fix did, only protects against a sub-millisecond window and nothing else. Verified end-to-end against a running app: a genuine conflict now surfaces the error with no data lost on either side, and a subsequent save after reloading succeeds normally.

## [0.22.0] - 2026-08-03
### Added
- **Android mobile UI shell.** Jodd on Android now gets a real phone layout instead of the squeezed desktop three-pane view: a single-pane stack (folders → notes → editor) that follows the system back gesture the way any other Android app does, plus a two-pane tablet layout with the folder tree in a slide-out drawer. Long-press now opens the same move/delete/pin/refetch menus that right-click opens on desktop.

### Fixed
- **Android now shows Jodd's actual icon.** Every debug install since Android bring-up was showing Tauri's generic placeholder icon instead — the real one existed but was never copied into the generated Android project.
- **Android release builds are now 16 KB page-size compatible**, closing a Google Play requirement for apps targeting Android 15+ and removing the compatibility warning every install showed.
- **"Reveal log file" now does something on Android** — Android has no file-manager "reveal" surface for a sandboxed app's own private storage, so the button now copies the log's path to the clipboard instead of failing silently.
### Added
- **About**: the About dialog now states plainly that Jodd is an independent, unofficial project with no affiliation to Apple or Google, and links out to the privacy policy and terms.

## [0.21.0] - 2026-07-31
### Added
- **Deactivate an account**: an account you are not using can be switched off from Account Settings. It disappears from the sidebar, from search, from Ask Jodd and from background sync, but nothing is deleted — its notes, folders, tags and settings all stay, and reactivating brings them straight back with no re-download. Switching off is not abrupt: anything you edited that had not reached Gmail yet keeps sending in the background, and the account only goes fully quiet once nothing is left to send. Deactivated accounts live in an **Inactive** group at the bottom of the account list, where you can reactivate or remove them.
- **Ask Jodd**: a new 💬 Ask Jodd entry in the sidebar opens an in-app chat over your own notes — ask a question in plain language (Thai works too) and get an answer with clickable citations back to the notes it used. Nothing is saved: closing the chat discards it. Each answer shows how many notes were in scope, how many were actually considered, and how many were read, so a thin result is visible rather than a mystery. Scope can be the current folder (and everything under it), the current account, or all accounts.
- **App-level LLM provider**: LLM providers can now be configured once for the whole app, in Settings, instead of per account. Accounts that don't set their own provider adopt the app default automatically; accounts can still set their own provider to override it, or explicitly turn it off. Ask Jodd always uses the app-level provider.

## [0.20.1] - 2026-07-29
### Changed
- **A few icons have colour again.** Delete is red, pinned is amber, and tags are purple — the three places where colour says what the icon means faster than its shape does. The rest stay neutral and follow the theme: colouring everything would make colour stop meaning anything.

### Fixed
- **Highlighted text in a note is no longer stripped.** 0.20.0 removed the white background some notes carry from Apple Notes or a web page — but it removed *every* background, including a highlight you applied on purpose. Only near-white backgrounds are cleared now; your highlights stay. (No highlight was actually lost: no note in a real vault had one.)

## [0.20.0] - 2026-07-29
### Added
- **Dark mode**, with a System / Light / Dark setting under Settings → Appearance. "System" follows your OS and switches with it live, without a restart; picking Light or Dark explicitly overrides the OS in either direction and is remembered between launches.

### Changed
- **New typeface, and Thai finally renders properly.** Jodd now ships IBM Plex Sans Thai and IBM Plex Mono with the app instead of borrowing whatever the system had. The previous font stack named Segoe UI, which contains no Thai at all — so on Windows, Thai text fell back to whatever the system chose. Note titles, tags and folder names with stacked Thai vowels and tone marks now have room to render without clipping.
- **Note metadata reads as a message field.** Slugs, dates, note counts and account names are set in monospace with fixed-width figures, so a note list stops shifting sideways every time a number changes.
- **New icon set.** The emoji used throughout the sidebar, menus and editor are replaced with a single set of drawn icons. Emoji rendered as a completely different set on Windows, changed the app's look depending on which machine you opened it on, and could not follow a colour theme — which is what made dark mode possible here.
- **The interface is now all English.** A few screens — Recently Deleted, the search-scope selector, the connections empty state — were still in Thai, so an English button could open a Thai screen. Thai *notes* are unaffected: Thai content, Thai search and Thai tags all work exactly as before.
- **Text is easier to read throughout.** Muted, secondary, accent, danger and success text were all measured against the surfaces they actually sit on — including hovered and selected rows, where a translucent highlight changes the background under the text — and several were too faint to meet accessibility contrast standards. They are now slightly darker.
- **The connections graph legend is clearer.** Two of its four relation colours were nearly identical, so folder and tag links looked the same. Tags are now purple and the whole legend is easier to tell apart at a glance.
- **Every node in the connections graph is now clickable.** Previously only linked notes responded; folder and tag nodes did nothing. A folder node now takes you to that folder, and a tag node filters by that tag — landing you in exactly the same place the sidebar would. They can be reached by keyboard too.
- **Truncated text shows its full value on hover.** Long note titles, folder names, tags and account names are cut off to fit, and there was no way to see the rest — including the node labels in the connections graph, which cut at 16 characters.

### Fixed
- **Dark mode is readable.** Several places rendered dark text on a dark background: the settings section headings, the Clear and Cancel button labels, text typed into Client Secret, and the folder-name prompt. The interface never declared its own text colour, so anything that didn't set one fell back to the system default black — which looked correct in light mode purely by coincidence.
- **Notes that arrived with their own formatting are readable again.** Some notes — usually ones written in Apple Notes or pasted from a web page — carry a hardcoded font and white background inside them. Those notes previously ignored the app's typeface, and in dark mode they would have appeared as bright white blocks with near-black text. They now follow the app's appearance. Text you deliberately coloured yourself is left alone, and the note's own content is never rewritten.
- **Keyboard focus is visible in one consistent style.** Two different focus rings were in use for the same kind of input; there is now one, and it stays visible in dark mode.
- **Icon-only buttons are now labelled** for screen readers, including the settings, close, edit and editor toolbar buttons.

## [0.19.0] - 2026-07-28
### Added
- **Bring your own agent CLI**: Extract and auto-link can now be driven by any headless agent CLI you already have installed — Claude Code, Codex, Qwen, Gemini, OpenCode, Aider — or a custom command you define yourself. Pick one under the account's LLM settings; "Test connection" verifies the binary actually answers before you run an extract. Existing Claude Code setups keep working with no changes. (Claude and Codex verified end-to-end.)

### Fixed
- **Re-extract** failed instantly on every attempt, before it ever reached the LLM — the note was left untouched with no visible error. It now runs as intended.
- **Re-extract** no longer looks like it did nothing on success: the new note appears at the top of Extracts and the sidebar tags update right away, without a manual refresh.

## [0.18.2] - 2026-07-26
### Fixed
- **Deleting a blank new note** no longer flashes a "delete_note" error — an unsaved draft is now discarded instantly without a round-trip.
- **Opening a folder whose notes hadn't loaded yet** (e.g. right after launch) no longer shows an empty list beside a non-zero count with a misleading "No notes in this folder". The notes now fetch immediately instead of after a delay, and a brief "Loading notes…" is shown while they arrive.

## [0.18.1] - 2026-07-22
### Changed
- **Quieter background sync**: a note you're actively editing is now pushed to Gmail once you pause typing (with a periodic safety sync), instead of every few seconds. This sharply cuts duplicate-message churn in your mailbox and the Apple Notes sync stalls that churn can trigger.

## [0.18.0] - 2026-07-21
### Added
- **Sources panel**: notes now show a "📎 Sources" list of URLs cited in the body, with a heads-up in Extract if you're about to cite something you've already extracted from elsewhere.
- **Smart Folders**: two new per-account views in the sidebar — "🔍 Orphaned" (notes nothing links to) and "🕰 Stale" (untouched 30+ days) — for spotting notes that have drifted out of your wiki.
- **Auto-link ingest**: Extract can now pull its source text from an existing note (not just pasted text), and after extracting or linking, Jodd automatically links the result to related notes and offers to add a short reference in other notes it's connected to — you review and confirm before anything else gets edited.
- **Link into wiki**: a new "🕸 Link into wiki" action on any note's right-click menu — finds and links related notes without rewriting the note itself.
- **Ingest source button** moved from a single sidebar-wide button to a 💡 button on each account, so it's always clear which account you're ingesting into.
- **jodd-mcp**: an optional read-only MCP server exposing note search and the note graph to Claude Code sessions (see `jodd-mcp/README.md`).

### Fixed
- **Citations**: URLs containing `&` (e.g. links with query parameters) weren't matched correctly for duplicate-source detection — they're decoded properly now.

## [0.17.10] - 2026-07-10
### Fixed
- **Sidebar**: a folder deleted right after its only note (both created and deleted within a couple seconds) could keep showing up in the sidebar with a "0" count for the rest of the session, even though it was fully gone from Gmail. Caused by deleting a note through a context menu that was still holding an unsynced snapshot of it. Restarting the app cleared it (fresh session), but it's fixed properly now.

## [0.17.9] - 2026-07-10
### Fixed
- **Restore**: fixed the 0.17.8 orphan-folder restore fallback — it checked a display label that could already be a synthetic "Notes" placeholder (not the note's real prior folder), so it never actually caught the case it was meant to fix. A note whose folder was deleted then restored would come back with no folder at all and vanish from the app entirely. Restore now checks the message's actual current Gmail labels after untrash instead of trusting that string.

## [0.17.8] - 2026-07-08
### Fixed
- **Recently Deleted**: a note deleted from a folder that was later deleted itself used to vanish from Recently Deleted for good, even though the message was still sitting in Gmail's own Trash — trash lookup no longer depends on the folder's Gmail label still existing.
- **Restore**: restoring a note whose original folder had been deleted now lands it back in the root "Notes" folder instead of leaving it with no folder at all.
- **Recently Deleted**: list order no longer shuffles on refresh for notes with identical timestamps.
- **Delete folder dialog**: reworded so the always-shown "must be empty" reminder doesn't read like a failed check.

## [0.17.7] - 2026-07-03
### Fixed
- **About → What's New**: clicking "What's New" always opened an empty dialog ("No release notes for this version") once the automatic first-launch popup had already fired for the running version. The manual button now always shows the current version's own release notes, independent of the once-per-upgrade "seen" bookkeeping used by the automatic popup.

## [0.17.6] - 2026-07-03
### Added
- **App Settings → Diagnostics**: optional persistent file logging, on by default, so sync issues are diagnosable after the fact (the app window doesn't show its own log). Saved to `~/Library/Application Support/jodd/logs/jodd.log`, auto-trimmed past 20 MB, with a file-size display and a "Clear log" button to reset on demand.

### Fixed
- **Review duplicates**: the dedup-review modal (and Cleanup Orphans) no longer re-scans the entire mailbox once per note — that was thousands of sequential Gmail API calls on a normal-sized account and could make the modal appear to hang indefinitely. Now scans once and looks up each note's duplicates from the result.

## [0.17.5] - 2026-07-03
### Added
- **Recently Deleted**: clicking a trashed note now shows its content (read-only) instead of a blank pane.

## [0.17.4] - 2026-07-03
### Fixed
- **Editor**: pressing Enter at the very start of the first line in a note now correctly pushes the existing text down to a new line, instead of leaving it in place and inserting a stray blank line after it.
- **Editor**: pressing Backspace to remove a blank line directly above a heading no longer demotes the heading to plain text.
- **Editor**: fixed a race where editing the note title right after editing the body (while the body edit was still pending autosave) could let a background sync overwrite the unsaved body edit and silently reset undo history.

## [0.17.3] - 2026-06-18
### Fixed
- **Local Folder (LocalFS)**: folder rename now writes the new name to disk and propagates correctly across nested subfolders.
- **Local Folder (LocalFS)**: renamed folders no longer get stuck in a `dirty_renamed` loop after a transient ENOENT error during sync.
- **Local Folder (LocalFS)**: vault path is now shown in Account Settings so you can see where your notes are stored.
- **Local Folder (LocalFS)**: deleting a note no longer wipes the whole vault when `label_id` is empty.
- **Local Folder (LocalFS)**: notes are now written to disk synchronously on save, preventing data loss on fast quit.
- **Local Folder (LocalFS)**: cascade-deleting a note now also removes orphaned pin and tags sidecars from `.meta/`.

## [0.17.2] - 2026-06-17
### Added
- **App Settings** (⚙ gear in sidebar footer) — enter your own Google OAuth credentials so Gmail sync works from the pre-built binary without a source build.
- **What's New** — release notes shown automatically on first launch after a version upgrade, and accessible from the About dialog.
- **About dialog** — version number, build date, and link to What's New; opened by clicking the version label in the sidebar footer.
- Sidebar footer now shows the app version; clicking it opens About.

## [0.17.1] - 2026-06-16
### Added
- Links to a note now update their displayed text automatically when you rename that note.

## [0.16.6] - 2026-06-15
### Changed
- Internal stability improvements.
