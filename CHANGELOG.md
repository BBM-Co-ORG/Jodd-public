# Changelog

All notable changes to Jodd are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); versions follow the app version.

## [Unreleased]

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
