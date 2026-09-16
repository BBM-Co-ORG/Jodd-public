# Changelog

All notable changes to Jodd are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); versions follow the app version.

## [Unreleased]

## [0.28.3] - 2026-09-16
### Fixed
- **On Android, a newly added iCloud account shows up in the sidebar straight away.** After signing in, the account was saved and its notes were already loaded, but the account list kept showing only the old accounts until Jodd was closed and reopened. Jodd now re-reads the account list the moment its own screen is back in front of you.

## [0.28.2] - 2026-09-16
### Fixed
- **On Android, Jodd comes back on screen by itself after an iCloud sign-in.** Apple's page used to stay in front of everything once the sign-in had finished — the account was already added and the notes were already loading behind it, but the only way back to Jodd was to close the app from the app switcher and reopen it. Jodd now puts its own screen back as soon as the sign-in is done.

## [0.28.1] - 2026-09-16
### Fixed
- **On Android, signing in to iCloud no longer closes Jodd instantly.** The released build of 0.28.0 was missing a piece the app needs to read its own sign-in session, so tapping **Add iCloud account** and completing sign-in shut the app down every time. Gmail and Outlook accounts were never affected, nothing on your account or in your notes was harmed, and the test builds this was developed against never showed it.

## [0.28.0] - 2026-09-16
### Added
- **New extracts go to an Inbox folder, and Jodd suggests where to file each one.** An extract now lands in a folder named Inbox — which shows as Inbox on your iPhone too — instead of a Jodd-only Extracts folder. After extracting, Jodd asks your AI provider which of your existing folders the note belongs in and shows the answer above the note: **Move** files it there, **Keep here** leaves it in the Inbox. It only ever suggests folders you already have. Right-click any note and choose **Suggest folder** to ask again, including for notes extracted before this change.
- **An Extracts view lists every extracted note, wherever it is filed.**
- **Extract can read the pages and videos behind links.** Paste text that contains links — or pick an existing note full of them — and the Extract window lists each one as a Page, a Video or Skipped. Tick up to eight and Jodd fetches each page's readable text or each YouTube video's transcript (Thai included), condenses each one, and combines them into one new note that lists every source at the end. Jodd does the fetching itself; your AI provider only ever sees the text. A link it cannot read is skipped with the reason — a playlist or channel, a PDF, a page that is only JavaScript, a private or age-restricted video — and the other sources still go through. Links to addresses on your own network are never fetched.

### Changed
- **The old Extracts folder is now an ordinary folder.** Nothing is moved out of it automatically; rename it, delete it, or file its notes with **Suggest folder**.
- **Re-extract puts the new note beside the note it came from** rather than in the Extracts folder.

### Fixed
- **On Android, Jodd no longer opens to the sign-in screen when you are already signed in.** At launch the app could miss the answer to its very first request and stay on the sign-in page — without the iCloud button — for minutes, until something else woke it. Your account and notes were never affected, and signing in again was never needed. Found on a Samsung Galaxy running Android 16.
- **Outlook accounts can use Extract.** Extract used to refuse every Outlook account because it needed to create a folder, which Outlook's Apple Notes folders do not allow. Extracts on Outlook go to the top level of Notes, or to an Inbox folder you have already made.
- **A dropped connection no longer stops a Gmail note from syncing.** On a Gmail account, a network blip while Jodd was sending a note, a deletion or a move was treated as a refusal that could never succeed: the note stopped syncing until you edited it again, the banner read "permanent: error sending request", and a folder created during the blip could disappear. Jodd now simply tries again on the next sync. A note already stuck this way syncs again after you edit it, or when you press **Try again** in the editor.

## [0.27.3] - 2026-09-15
### Changed
- **The checklist button turns the line you are on into a task.** It used to insert a new, empty task line wherever the cursor was, sometimes in the middle of a line. Now ⌘⇧9 (Ctrl+Shift+9 on Windows) or the toolbar button puts a checkbox at the start of the current line — and leaves a line that is already a task as it is.
- **A tag added from the tag field goes on a line of tags at the end of the note.** It used to be stuck onto the end of whatever came last. Now it joins the trailing line of tags if the note has one, and otherwise starts a new line after the last list, quote, code block or checklist rather than inside it.
- **An empty line in the middle of a quote no longer ends the quote.** Pressing Enter on an empty line still leaves a quote or heading when that line is its last.

### Fixed
- **Undo and redo now work after Replace, adding or removing a tag, picking a [[link]], leaving a quote or heading with Enter, and Backspace on an empty line above a heading.** These changed the note in a way the editor's undo history never saw: after a Replace, Cmd+Z stopped working entirely, even for text typed before it; a tag added from the tag field could never be undone; and redo after leaving a quote put the quote back in the wrong place. One Cmd+Z now takes back a replace, a tag removal or a link pick together with what you typed just before it.
- **Find and Replace no longer go wrong after a character that changes length when lower-cased, such as `İ`.** On such a line Replace could change the wrong characters, or fail outright.

## [0.27.2] - 2026-09-14
### Added
- **A Disable thinking option for local AI servers running reasoning models.** A local llama.cpp serving a Qwen3-style model thinks at length before it answers, and on a consumer GPU that can run past Jodd's time limit — so it looked like a broken provider rather than a slow one. Tick **Disable thinking** under the Custom endpoint fields and Jodd asks the server to skip that step: measured against a real server, 66 seconds with thinking and 27 without. Jodd also now waits up to three minutes, rather than ninety seconds, for an AI provider to answer. Requests to hosted providers such as Anthropic and OpenAI are sent exactly as before.

### Changed
- **On macOS, editor shortcuts use ⌘ only.** Ctrl keeps its usual Mac text-editing meaning again — Ctrl+A jumps to the start of the line, Ctrl+E to the end — instead of applying formatting, and Ctrl+⌘+Z no longer undoes. On Windows the shortcuts use Ctrl, and Ctrl+Y still redoes. The shortcut sheet (ⓘ) now shows the keys for the computer you are on.
- **Pressing Enter on an empty, indented checklist line keeps the line at that indent** instead of moving it back to the left edge.

### Fixed
- **"Edited on another device" no longer appears when nobody else touched the note.** If you kept typing — or pressed Cmd+Z — just after Jodd saved, a background refresh could hand the editor the copy Jodd had saved on this computer but not yet sent, and the editor mistook its own save for someone else's edit. It now recognises both the copy the server confirmed and the one the editor last saved. Found on a Gmail account with a single Jodd running; the same check covers every account type.
- **Undo and redo now step correctly through markdown shortcuts and checklists.** Typing `# `, `- `, `1. ` or `> ` turns a line into a heading, list or quote, but undo could not turn it back, redo could lose what you had typed after it, and Enter on a checklist line left an empty checkbox row that no undo removed. Now Cmd+Z after `# Title` gives back the literal `# `, then an empty line, and redo walks back up the same steps. Measured with real keystrokes in the same web engine Jodd uses on macOS.
- **Pasting text that starts with `# `, `- ` or `> ` no longer reformats the line.** Those shortcuts fire only as you type, as the shortcut sheet has always said; a paste now stays exactly as pasted.

## [0.27.1] - 2026-09-10
### Fixed
- **Jodd no longer closes on its own when you leave it with the Back gesture on Android.** This is the known issue 0.27.0 shipped with. The cause was in a third-party component Jodd builds on: Android removes an app's screen from its own bookkeeping the moment you leave it, while the app itself keeps running, and a lookup made in that window aborted the whole process instead of simply finding nothing. Said plainly, because it is worth being plain about: the fault is understood and the cause is gone, but the crash was intermittent and could never be triggered on demand, so this is "the cause we identified has been removed" rather than "we watched it stop happening". Android only, and only with an iCloud account signed in.

### Known issue
- **After signing in to iCloud on Android, Apple's page stays on screen and Jodd appears stuck behind it.** Closing that page is one call, Jodd makes it, and the component underneath reports success while leaving the page where it is — the same class of problem as the crash above, in the same place. Nothing is wrong with your account: the sign-in has already completed by then, and your notes are there. Close Jodd from the app switcher and reopen it, and you will find the account signed in and your notes listed. On the same screen you may also see Apple's page drawn slightly under the phone's clock and battery row; that is cosmetic and has the same cause. Android only.

## [0.27.0] - 2026-09-10
### Added
- **iCloud notes now work on the Android Developer Preview.** Until now Android could reach Apple Notes only through a Gmail or Outlook account attached to Notes — a door most Apple Notes users have never opened. An iCloud account now signs in on the phone and reads and writes the same notes the desktop apps do. On the account this was tested against, Jodd showed all **778 notes and 103 folders**, matching Apple Notes exactly, and a note written on the phone appeared in Apple Notes within a minute and was still intact twenty minutes later.
- **Two things work differently on Android from the desktop, both deliberate.** Your iCloud session lives in the phone's own cookie store rather than inside a hidden window, so it survives Jodd being closed or even killed — but when it eventually expires, Jodd cannot renew it quietly in the background the way the desktop apps do. You press **Reconnect** in Account Settings instead. Ticking **Keep me signed in** on Apple's page is what decides how often that happens.

### Known issue
- **With an iCloud account signed in on Android, leaving Jodd with the Back gesture can occasionally close the app.** It is intermittent rather than every time, and **nothing is lost** — your notes are on the phone and on Apple's servers, and reopening Jodd carries straight on. The cause is a one-line defect in a third-party component Jodd builds on, in the same file as a defect already fixed for this release; the fix is written and tested but is not in this build. Android only, and only with iCloud — Gmail and Outlook accounts are unaffected, as are all desktop platforms.

## [0.26.1] - 2026-09-09
### Fixed
- **The Android build compiles again.** Recent iCloud-on-Windows work left two constants marked desktop-only while the code using them was not, so the Android target failed to build — invisible to the desktop test runs, caught only by the release's Android encryption check. No effect on the desktop apps; this restores the Android Developer Preview build.
- **A title that starts with an invisible character no longer looks empty in Jodd while showing a stray mark on your iPhone.** Some characters — a lone Thai vowel with no letter under it, for example — are drawn by Windows with nothing visible, yet they are really there and sync to Apple Notes, which shows them as a small dotted circle. Jodd now shows that same dotted circle in the note list and the title, so a note that looked blank reveals what it actually holds, and the editor offers a one-click **Remove it** to clear a character you could not see before. iCloud accounts.
- **An edit made right after creating an iCloud note now reaches Apple immediately, instead of after up to ten minutes.** Apple changes a brand-new note's internal version the instant it is created, which left Jodd's very next save pointing at the old one and being refused; Jodd then retried the same rejected save every few seconds until a full refresh happened to catch up. In the meantime your iPhone kept showing the note as it was before that edit — so a character you had just deleted looked like it was still there. Jodd now refreshes and retries at once. iCloud accounts.

## [0.26.0] - 2026-09-06
### Added
- **thClaws can now be your AI tool for Extract, auto-link and Ask Jodd**, by either of the two routes it offers. Pick **thClaws** under Agent CLI and Jodd runs it directly — nothing to keep running, and thClaws decides which model to use, so changing the model there changes it here with nothing to configure in Jodd. Or run `thclaws --serve` and point Jodd's custom endpoint at it. Every flag Jodd sends was measured against the real tool rather than read off its help text.

### Fixed
- **A note whose first line repeats its title no longer loses that line.** Apple stores the title inside the body as well as beside it, so Jodd adds that line when saving and removes it when loading. When your own first line happened to match the title — a note called `Shopping` that opens with the word `Shopping` — the removal ran twice and took your line with it, on the first edit, with the editor still showing "Saved". Nothing was lost on the server; the copy Jodd kept was the damaged one. Gmail and local-folder accounts.
- **"Edited on another device" no longer appears on notes nobody else has touched.** 0.25.3 fixed one cause of this; a second remained and was the more common one. On opening a note, the editor briefly compared it against the body of the note you were looking at *before* — never a match, so it announced a conflict that did not exist, and the notice stayed up until you switched notes or reloaded.
- **Extract and auto-link no longer throw away good answers from local AI servers.** Ollama, LM Studio, llama.cpp and thClaws' own server all accept Jodd's request for plain JSON and then wrap the answer in a code fence anyway. Jodd was discarding those answers as malformed. It now reads them the same way it has always read the command-line tools' output. Answers that are genuinely unusable still fail as before.
- **A connection test that cannot reach your endpoint now says why.** "Connection refused — nothing is listening there; check the Base URL and that the server is running" instead of one sentence that read the same for a wrong port, a stopped server and a bad address.
- **The app icon renders correctly again** — full-bleed on macOS 26, which had been adding its own plate around it, and inset into the safe zone on Android so the launcher stops cropping it.

### Changed
- **Fewer keychain prompts at startup.** The OAuth client secret is read once per run instead of on every check that needed it.


## [0.25.3] - 2026-08-31
### Fixed
- **"Edited on another device" no longer appears on a note only you edited.** Between saving a note and Jodd pushing it, the editor compared what came back against the copy it had just written locally rather than the copy the server had actually accepted — so a refresh landing in that window looked like someone else's edit, and the notice latched until you switched notes. The worker now hands the editor a receipt naming the bytes the backend confirmed.
- **A failing AI CLI now explains itself.** When Extract, auto-link or Ask Jodd cannot reach the AI tool you configured, Jodd names the likely cause and one thing you can do — the tool is signed out, the model refused the request, the model in your config has no credential — instead of showing a bare exit code. When nothing recognises the failure, the tool's own words are shown in full rather than hidden.

### Changed
- **Test connection now runs a real extraction.** It used to send a one-line greeting, which every model answers easily — so it reported success on setups that failed the moment real work arrived. It now runs the same kind of workload Extract does and only reports success if the result is usable. That takes about 10–20 seconds and uses your AI tool's quota, which the button now says.
- **Diagnostic logs record every credential read**, and the startup line says how many of your accounts are actually active rather than only how many exist. Both make it possible to answer "why did it ask for my password again" from the log.

## [0.25.2] - 2026-08-30
### Fixed
- **Extract and auto-link work again on Claude Code accounts.** v0.25.1 asked Claude Code to return its answer in a strictly-defined shape. Against the short prompt it was tested with that worked; against the real extraction prompt it did not, and every extraction on a Claude Code account failed — silently on some models, and with a content refusal on others. Jodd no longer asks for that, which is how it worked before v0.25.1. Codex CLI is unaffected and keeps the stricter contract, which was verified against the real prompt.
- **A failing AI CLI now tells you what went wrong.** When one of these tools fails, some of them explain themselves on a channel Jodd was ignoring — so a real failure could arrive as "exit status: 1:" and nothing else, which is what made the problem above so hard to place. Jodd now reads both channels and shows the tool's own words.

## [0.25.1] - 2026-08-30
### Fixed
- **Extract and auto-link no longer fail outright on Codex CLI accounts.** When Jodd asks Codex to return a strictly-shaped answer, Codex applies OpenAI's strict structured-output rules and rejects anything looser before the model even runs — returning nothing at all, with no error you could act on. Jodd's request now satisfies those rules. Measured against the real Codex CLI, both before and after.

### Changed
- **Headless AI CLIs start faster and no longer inherit your personal setup.** When Jodd runs Claude Code, Codex or opencode for Extract, auto-link or Ask Jodd, it now asks each one to skip the configuration it would otherwise load first — your hooks, skills, plugins, saved settings and connected MCP servers — and to leave its file-reading tools switched off. Jodd already hands over everything the request needs, so none of that work was doing anything except delaying the answer, and a startup of roughly three seconds was being paid before a single word reached the model. Your sign-in for each CLI is untouched: nothing here changes how any of them authenticates.
- **Codex runs no longer leave session files behind on your disk.**

## [0.25.0] - 2026-08-29
### Added
- **iCloud accounts — Apple Notes directly, with no Gmail or Outlook account in the middle.** Every earlier Jodd backend needed you to have attached a non-Apple account to Apple Notes first; most people never have, so most Apple Notes users were unreachable by construction. This one talks to CloudKit the way `icloud.com` does. Read your notes and your real nested folder tree, edit note text, apply formatting, move notes between folders, create and rename and delete folders, delete and restore. Verified live against a real 773-note / 101-folder account, agreeing with Notes.app note for note and folder for folder, with edits confirmed on the Mac, on icloud.com and on an iPhone.

  **Sign-in is macOS-only, and that is an operating-system limit rather than a policy.** There is no OAuth on this backend at all — the credential is a live Apple browser session held in a per-webview data store, which is a macOS 14+ API with no counterpart on Windows or Android. Windows and Android iCloud support are not scheduled.

  **Not every note is editable, and the ones that are not say so on the note.** Measured on that live account: **593 of 773 notes (76.7%) accept edits**. An iCloud note is not HTML or email — it is Apple's own document format, carrying per-character identity and formatting Jodd does not fully interpret. Rather than rebuild the note from the text it can read, which would silently destroy formatting on Apple's servers, Jodd preserves what it cannot read and refuses the write when it cannot prove it did so. The refusals, with counts: 95 notes Jodd cannot re-encode byte for byte, 50 containing an attachment, table or inline tag, 35 whose formatting layers do not round-trip, plus password-protected notes.

  **Also on this backend:** pins are Apple's own and are shown but not editable (they live on a separate record Apple owns); a restore asks which folder to restore into, because a trashed note's original folder is overwritten by the Trash's; password-protected notes appear with their real title, folder and dates and a placeholder body; adding a `#hashtag` from Jodd is not supported yet, though existing ones display and index; **an account with Advanced Data Protection turned on cannot work** — note bodies are then genuinely end-to-end encrypted, and Jodd detects this at sign-in and refuses before creating anything; and one Apple ID per install, because macOS gives the app a single app-wide session store.

- **Rich text written in Jodd now reaches Apple Notes on iCloud.** Bold, italic, underline, strikethrough, headings, bulleted and numbered lists, and checklists all round-trip, live-verified past the delayed-merge window where Apple reconciles a third-party write.

- **A capability and limitation matrix**, at [docs/PLATFORM-MATRIX.md](docs/PLATFORM-MATRIX.md) and on the homepage: which account types each platform can add, and exactly what each account type can and cannot do, derived from the same code the app reads to decide what to show you.

- **Reauthenticate for a broken iCloud session.** Apple sessions expire in hours; the account now offers to re-establish one instead of going quiet.

### Fixed
- **An edit made in Apple Notes could sit unseen in Jodd for up to ten minutes.** The change detector noticed within a minute and dropped its cache, but nothing told the interface to look again, so the screen kept showing stale content until the next long poll. The sync worker now signals the frontend directly.
- **An edit arriving while your cursor was in the editor could leave the note list and the editor pane showing different versions of the same note**, with a banner that never cleared. Moving focus out of the editor now resolves it.
- **Notes created in Jodd on iCloud came back empty in Apple Notes**, and an in-place edit could merge as a duplicated note — the whole note appearing twice, once old and once new, hours after the write looked clean. Root-caused to the order in which Apple's clients serialize their own edit history; both are fixed and confirmed by live passes that survived the delayed-merge window.
- **A note Jodd cannot write is now refused once, not retried forever.** A locked note's delete no longer produces a retry storm.
- **A pasted note is never "corrected".** A list pasted from Apple Notes and a paragraph that merely begins with a bullet character are identical at the character level; stripping the glyph would break the real case to fix an imaginary one.

- **Microsoft sign-in now works in a downloaded build.** Release binaries embed a Microsoft OAuth client the same way they already embedded the Google one, so signing in to an `@outlook.com`/`@live.com` or Microsoft 365 account no longer requires registering your own application and supplying `MS_CLIENT_ID` yourself. Setting that variable still works and still wins, for anyone who prefers their own registration. **This does not change what a corporate Microsoft 365 account can do:** measured against an outside tenant, an ordinary (non-admin) work account is refused by Microsoft with *"Need admin approval"* — organisations control whether staff may consent to third-party apps, and read/write mailbox access is not a permission most allow without an administrator. Personal Microsoft accounts are unaffected and consent normally.

### Changed
- **The iCloud folder placement rule follows Apple's own convention.** Apple Notes offers no way to create a folder inside `Notes` itself, so a folder Jodd creates is placed as a sibling — matching where Apple's own client would put it, rather than somewhere CloudKit accepts but no Apple client has ever produced.

## [0.24.1] - 2026-08-16
### Added
- **Microsoft/Outlook accounts.** Jodd now talks to Apple Notes over the same Exchange-backed sync `@outlook.com`/`@live.com`/Microsoft 365 accounts use, alongside Gmail. Sign in, read, and see your existing notes and folders — measured end-to-end against a live account. Creating, editing, moving, and deleting notes round-trips to Apple Notes and the iPhone with no duplicates: this backend does a real in-place `PATCH` update rather than Gmail's insert-new-and-trash-old dance. Pinning a note round-trips too, stored in a named property on the note itself rather than a second sidecar message the way Gmail's pin sidecar works. **Folder creation, rename, and delete do not work on this backend, and this is permanent** — two independent Graph API creation paths were tried and both were traced to an immutable container-class property that Graph silently refuses to set, so no folder Jodd creates can ever be recognized as part of Apple's Notes tree. Attachments are also unavailable here — Apple itself refuses them on Exchange accounts.
- **Local data is now encrypted at rest.** `jodd.sqlite3` is protected with SQLCipher; existing installs migrate automatically on first launch after updating, with a recovery path if the encryption key can't be read back.
- **jodd-mcp can pin and unpin notes**, mirroring the in-app pin toggle, gated by the same per-account write allowlist as its other write tools.
- **Every permanent delete now asks first.** Previously only the sidebar's right-click delete confirmed before deleting — the editor's trash icon and multi-select batch delete (the most destructive of the three, capable of destroying many notes in one click) went straight through with no warning.
- **jodd-mcp binaries are now built and attached to every release**, for macOS (arm64) and Windows, instead of requiring a local build from source.

### Fixed
- **A folder actually named "Notes" anywhere in the tree could hijack the real Notes root**, re-nesting it under itself and scattering notes that belonged at the top level. Now exactly one "Notes" — the genuine root — stays unsuffixed; any other folder sharing that name is disambiguated like an ordinary collision.
- **A folder-only change could go unnoticed in the sidebar until the app restarted.** The folder tree only re-read on note changes, so a folder created or renamed directly by `jodd-mcp` — or one created empty — stayed invisible until something else happened to trigger a refresh.
- **A pin toggle or a note move could produce a spurious conflict on the next sync**, because both discarded the server's response instead of updating the cached remote version Jodd uses to tell a real conflict from its own recent write.
- Search results and note previews now decode HTML character references (e.g. `&amp;`) instead of showing them literally.
- Closed a further set of correctness issues found in a full review of the Microsoft backend: a Microsoft account's orphan-note cleanup could reach a Gmail-only endpoint with a Graph token, an embedded apostrophe in a note's identity could break the server-side lookup that finds it, and a stale cached folder id could surface a hard error instead of an empty folder on a routine poll.

### Removed
- **The unused tag-sidecar mechanism** (Gmail's `Notes-Meta` tag label and LocalFs's `.tags.json` file) — tags have round-tripped via inline `#hashtags` in the note body for a while now, and the read side of the sidecar had been dead for longer than that. It was pure write overhead with nothing on the other end to read it.

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
