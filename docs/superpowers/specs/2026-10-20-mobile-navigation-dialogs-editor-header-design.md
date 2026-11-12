# Mobile navigation, dialogs and the editor header — design

Date: 2026-10-20 · Status: approved by the owner 2026-10-20 · Plan: [../plans/2026-10-20-mobile-navigation-dialogs-editor-header.md](../plans/2026-10-20-mobile-navigation-dialogs-editor-header.md)

Builds on [2026-08-03-android-mobile-ui-shell-design.md](2026-08-03-android-mobile-ui-shell-design.md)
(the phone stack and tablet drawer). Where this spec changes a decision made
there, §2 says so.

## 1. What this is for

Jodd works on desktop, but its editor header is cluttered there too. On a phone
or tablet the problems are structural:

- **Dialogs are desktop cards squeezed onto a phone.** About 14 modals are
  centred `min(Npx, 90vw) × 85vh` cards. App Settings is one long scroll with
  five Save buttons, and closing it silently drops unsaved AI and
  speech-to-text edits.
- **Moving between panes gives no sense of place.** Phone headers say only
  "☰ Folders" or "‹ Back". The phone Folders screen is the whole desktop
  Sidebar.
- **Chrome takes the space that belongs to content.** The editor stacks six
  rows above the body. Tags appear twice, once as chips and once in the body.
  The editor toolbar wraps onto 2–3 rows of 22px buttons.

**Success:**

- On a phone, every screen says where you are, and Back says where it goes.
- Every dialog is usable one-handed on a phone, with the keyboard open.
- The editor shows the note within one line of metadata.
- Settings has one save model, and nothing is lost without asking.

The owner handed the design calls to this spec ("proceed based on your finding
and best practice"). Every decision below is the spec author's call, made from
the code audit (shared notes: `mobile-nav-dialogs-header-audit.md`) and common
practice in Apple Notes and Material 3. The owner approves the spec as a whole.

### Assumed already fixed (separate quick-fix branch)

- The tablet drawer closes on selection.
- Re-tapping a Smart Folder or note navigates on phone.
- The account status dot.
- Move-to works by touch.
- Gmail-only fields are no longer shown on iCloud.
- Tag chips are removed from note-list rows.

If any of these has not landed when a phase starts, that phase's plan absorbs
it rather than designing around it.

## 2. Goals, non-goals, anti-goals

### Goals

1. One shared `Modal` primitive that every dialog uses. It is a card on
   desktop and tablet, and a full-screen page, bottom sheet or alert on
   phone. It owns focus, Esc, Android Back and the dirty guard.
2. Settings becomes a section list that drills into pages, with one save
   model.
3. A one-line rule for explanatory copy.
4. Phone pane headers have titles, and Back names its parent.
5. A simplified phone Folders screen.
6. A defined after-selection rule for each layout. Desktop and tablet open
   the first note.
7. Tablet landscape keeps the folders column visible; portrait keeps the
   drawer.
8. A phone editor with a 48px touch bar that sits above the keyboard, and
   Delete moved into ⋯.
9. One compact metadata line above the title.
10. A tags popover, `#`-autocomplete in the body, and a collapsible
    connections/Sources section.
11. Locale-aware dates.
12. Spacing, z-index and touch-target tokens.

### Non-goals

- **No Rust changes**, except if Phase 3 finds a missing command (§9). Every
  setting and action already has an IPC command.
- **No i18n or translation of UI strings.** Dates follow the locale; the words
  stay English.
- **No new routing library.** History stays the stack (2026-08-03 rule).
- **No change to what a tag is.** Tags remain body hashtags (gotcha #38).
  Creating iCloud hashtags is roadmap #4b stage 2 and stays out of scope.
- **No redesign of the content inside complex dialogs** (Extract, Ask, Curate,
  Dup review). They move onto the primitive and get the phone page treatment;
  their insides are untouched except for spacing tokens and copy length.
- **No change to desktop keyboard shortcuts.**

### Anti-goals

These would make the work a failure even if it technically ran:

- Any navigation or open-a-note step that waits on the network (local-first
  doctrine).
- Android Back that ever calls `finish()`, or that leaves the app while a
  modal is open (gotcha #31).
- A save path that loses an unsaved edit silently. Closing with unsaved
  changes must always ask.
- Breaking the title round trip (#11, #17, #27): the title stays its own
  field, never read from or written into the body.
- A second implementation of note actions that drifts from the context menu.
  ⋯ and long-press must call the same functions.
- Desktop markup changing because of a phone rule. Layout decisions stay
  gated on `$isAndroid` first (2026-08-03 rule).

### What this changes from the 2026-08-03 spec

- **"No `mobile={true}` props in leaf components" is relaxed to "leaf
  components may read the layout store for chrome they own".**
  - Example: NoteList hides its own title row when the phone pane header
    already shows it.
  - Props still never fork a component into a phone version.
- **The tablet "overlay drawer" becomes width-dependent** (§5.4).
- **Phone note pane headers gain a title and actions.** That spec had only
  "‹ Back".

## 3. Foundations

### 3.1 Tokens (`src/styles/tokens.css`)

**Spacing.** Add a 4px-based scale. The audit found 25 distinct lengths;
these eight cover 90% of uses.

```
--space-0-5: 2px;  --space-1: 4px;  --space-1-5: 6px;  --space-2: 8px;
--space-3: 12px;   --space-4: 16px; --space-6: 24px;   --space-8: 32px;
```

- Migrate odd values to the nearest step (5→4/6, 10→8/12, 14→12/16) only
  when a file is touched for another reason.
- No big-bang sweep.

**Touch.**
- `--touch-target: 48px` (Material's minimum).
- `--hit-target-desktop: 28px`.

**Layers.** Native `<dialog>` uses the top layer, so modals need no z-index.

```
--z-sticky: 10; --z-drawer: 200; --z-popover: 300; --z-toast: 400;
```

The existing 40/41/1000/1100/1500/2000 values go away as each dialog moves
onto `Modal`.

**Breakpoints** cannot be CSS variables inside `@media`, so they live in
`src/lib/stores/viewport.ts`:
- `ANDROID_TABLET_BREAKPOINT = 700` (existing).
- `ANDROID_TABLET_WIDE_BREAKPOINT = 1024` (new).

`androidLayoutMode` becomes `'phone' | 'tablet' | 'tabletWide'`. Callers that
only care about phone-vs-not keep working, because they compare against
`'phone'`.

**Ratchet test.** A new `designGuards.test.ts` case counts raw `px` values in
`padding`/`margin`/`gap` across `src/`. It fails if the count rises above a
checked-in baseline. Files created by this work must use tokens only.

### 3.2 Keyboard inset

A new readable store, `keyboardInset` (`src/lib/stores/viewport.ts`):

- **Value:** `max(0, innerHeight − visualViewport.height − visualViewport.offsetTop)`.
- **Published as** a CSS variable `--kb-inset` on `:root`.
- **Off Android** it is constantly 0.

The Android activity sets no `windowSoftInputMode` today, and nothing in
`src/` reacts to the keyboard. So a Phase 1 task is a **measurement on
the S23 FE**: does `visualViewport` shrink while `innerHeight` stays fixed, or
do both shrink (adjustResize)?

- **If both shrink:** `--kb-inset` stays 0, and a layout sized with `100dvh`
  is already correct.
- **If only `visualViewport` shrinks:** bottom bars use
  `bottom: var(--kb-inset)`.

The store handles both cases. The measurement only decides which tests carry
weight.

**Every full-height rule moves from `vh` to `dvh`.** This includes
`.editor-pane { height: 100vh }`.

### 3.3 Note actions are one module

The single-note action bodies move out of `NoteContextMenu.svelte` into
`src/lib/controllers/noteActions.ts`:

- `togglePinOne`, `moveTo`, `duplicateNote`, `refetchFromRemote`,
  `reExtractLessons`, `linkIntoWiki`, `suggestFolder`, `deleteNote`

Each keeps its current optimistic → rollback shape.

**Who calls them:**
- The context menu (right-click and long-press).
- The new editor ⋯ menu.
- The phone note-pane ⋯.

**Batch variants** (`deleteBatch`, `moveBatchTo`, `setPinBatch`) stay in the
context menu. They are list-only.

## 4. Dialogs (problem A)

### 4.1 The `Modal` primitive (`src/lib/components/Modal.svelte`)

**Built on the ConfirmDialog pattern:** a native `<dialog>` and `showModal()`.

- The background becomes inert, so Tab cannot leave.
- The opener is recorded on mount and refocused on unmount, if it is still
  connected.
- Every key event stops at the dialog, so app shortcuts do not fire.
- Settle-once guard.

**Props (Svelte 5 runes):**

| Prop | Meaning |
|---|---|
| `title` | Shown in the header; also `aria-labelledby` |
| `size` | `'sm' \| 'md' \| 'lg' \| 'xl'` — desktop card width `min(360/480/640/960px, 100vw − 32px)` |
| `phone` | `'page' \| 'sheet' \| 'alert'` — presentation when `$isAndroid && $androidLayoutMode === 'phone'` |
| `dirty` | When true, any close attempt first asks "Discard changes?" |
| `onclose` | Called once, after the guard |
| `primary` | Optional `{ label, onclick, disabled?, busy? }` — the header action on a phone page, the footer primary elsewhere |
| snippets | `children` (body), optional `footer`, optional `headerActions` |

**Presentation:**

| Where | `page` | `sheet` | `alert` |
|---|---|---|---|
| Desktop, tablet | Centred card, `max-height: calc(100dvh − 48px)`, body scrolls, footer sticks | Same card | Same card at `sm` |
| Phone | Full screen. Header: `‹ <parent>` · title · primary. Body scrolls. Safe-area insets. Content padded by `--kb-inset` | Bottom sheet, rounded top, grab handle, `max-height: 90dvh`. Drag-down or tap the scrim to close (through the guard) | Centred card `min(320px, 100vw − 32px)`, buttons full width and stacked |

- Tablet uses the desktop presentation, with `--touch-target` sizing for
  controls.
- A desktop window narrowed below 700px stays a card (2026-08-03 rule).

**Closing.** Every close path goes through one function, `requestClose()`:
Esc, backdrop click, ‹, Cancel, drag-down, and Android Back. If `dirty`, it
opens a nested `alert`:

- Message: "Discard unsaved changes?"
- Buttons: **Keep editing** (focused) and **Discard**.
- Where the caller supplies `onsave`, a third button: **Save**.

**Android Back** — phone presentations only:

- **On open**, the modal pushes a history entry
  `{ pane: <pane beneath>, depth: <depth beneath> + 1, overlay: <id> }`.
  - Overlay entries **count toward depth**, so `navigateToPane('folders')`'s
    `go(-depth)` unwinds through them correctly.
  - `activePane` stays the pane beneath.
- **On a `popstate` that removes the entry**, `requestClose()` runs.
  - If the guard keeps the modal open, the entry is pushed back.
- **On closing from the UI**, the modal calls `history.back()` exactly once,
  then sets a flag so the resulting `popstate` is ignored.
- **The root-Back path (`moveTaskToBack`) is untouched.** An open modal
  always has its own entry above the root.
- **A modal action that navigates** (an Ask Jodd citation, Extract opening
  its note, any future one) closes the modal *into* the destination.
  - phoneNav's new `closeOverlayInto(pane)` turns the overlay entry into the
    destination entry with `replaceState`, at the same depth.
  - It does not call `history.back()` and then push: `history.back()` is
    asynchronous, so the two would race.
  - Back from that note returns to where the user was **before** the modal,
    not into the modal. *Spec author's call: Ask conversations are saved, so
    nothing is lost.*
- **Tests:**
  - Phone Back closes the modal, not the pane.
  - The dirty guard re-pushes the entry.
  - `go(-depth)` with an overlay on top lands on the root.
  - Nested overlays: Settings page → its discard alert.
  - `closeOverlayInto('note')` then Back lands on the pane that was beneath
    the modal.

**Copy and layout inside a Modal:**
- Header title in sentence case; no "Dialog" or "Modal" in names.
- Footer: secondary actions left, primary right. At most one primary button.
- Destructive actions are never the primary. They sit left, in danger
  colour.

`designGuards.test.ts`'s "modal scrolling" rule keeps passing, because
`Modal` sets `max-height` and owns the scroller.

### 4.2 Migration map

| Dialog | Phone | Notes |
|---|---|---|
| ConfirmDialog | `alert` | Becomes a thin wrapper over `Modal`; props unchanged so its 7 callers don't move |
| AppSettings + AccountSettings | `page` | Replaced by Settings (§4.3) |
| AskJoddModal | `page` | Its 767px rule is removed. On phone the History rail opens as a `sheet` |
| LessonExtractModal | `page` | Primary = the button whose label changes |
| CaptureSheet | `page` | Its 600px rule is removed. Snooze-on-dismiss keeps working through `onclose` |
| CurateReview, DupReviewModal, LinkSuggestionsModal | `page` | Each gains Esc and backdrop close, which they lack today |
| SshAccountDialog, SshEasySetupDialog | `page` | Esc stays blocked during the progress step: `dirty` true and the guard says "Setup is running" |
| About, WhatsNew, SignInBlocked | `page` | |
| Sidebar text prompt (new folder, rename) | `alert` | Input autofocused; Enter confirms |
| NoteEditor graph, shortcuts cheat sheet | `page` | Gains `role="dialog"` and a title |
| NoteContextMenu on phone | `sheet` | An action sheet. Desktop keeps the anchored menu |

**Order.** Migrate one dialog per commit, simplest first:
About → WhatsNew → SignInBlocked → ConfirmDialog → … → Settings last.

### 4.3 Settings

**One component, `Settings.svelte`, replaces AppSettings's shell** and absorbs
AccountSettings as a sub-page. Opened by the existing `appSettingsOpen` store.
That store becomes `settingsRoute`, a writable `string | null` such as
`'ai'` or `'accounts/gmail:x@y'`, so callers can deep-link. AuthScreen opens
`'advanced/oauth'`.

**Shape:**
- **Desktop and tablet:** `Modal size="xl"` with two columns — a section list
  (~180px) on the left, the page on the right.
- **Phone:** the section list is the root page. Tapping a section pushes a
  page with `‹ Settings`, each with its own overlay history entry.

**Sections, in order:**

| Section | Contents | Saves |
|---|---|---|
| **General** | Appearance (System/Light/Dark); Date display (§6.6); Ask Jodd history on/off, with Keep/Delete | Instant |
| **Accounts** | One row per account (name, backend, status dot, duplicate count when > 0). The row drills into today's AccountSettings content, which also holds Deactivate and Remove. "Add account" opens the existing add flow | Account page: page Save |
| **AI** | LLM provider (incl. AI host); AI limits; Agent workspace (desktop only, unchanged) | Page Save |
| **Privacy** | Hidden folders, per account, on **all platforms** (§4.3.1); AI receipts (retention, export, delete) | Folder toggles and retention: instant. Delete: confirm |
| **Speech to text** | Today's `SpeechToTextSettings` | Page Save, including its host-confirm step |
| **Share to Jodd** | Today's section, per platform | Actions only |
| **Advanced** | Google OAuth credentials; Microsoft OAuth (desktop only); Diagnostics (file logging, reveal/copy log path, clear log) | OAuth: page Save. Diagnostics: instant |

**The save model, stated once in the UI:**

> Switches and choices apply as soon as you change them. Pages with text
> fields have one Save button. You can't leave such a page with unsaved
> changes without choosing Save or Discard.

**Mechanics:**

- **Each form section component exports** `isDirty(): boolean` and
  `save(): Promise<void>`. This generalises `LlmProviderSettings.saveIfDirty()`.
  - `save()` throws a user-readable message on failure.
- **The page:**
  - sums the dirty flags into `Modal`'s `dirty`;
  - renders the single primary **Save**;
  - runs each dirty section's `save()` in order;
  - stops at the first failure, showing the error inline above that section.
- **Saves are local IPC.** Settings writes config files, not the remote. The
  page waits for the result only to report failure honestly, as
  AccountSettings does today. No navigation waits on the network.
- **The footer "Clear" for OAuth** becomes a destructive "Remove credentials"
  inside Advanced, behind `ConfirmDialog`. It must not use `window.confirm()`,
  which WKWebView drops — today's footer uses it.
- **"Test connection" stays per section** (LLM, STT). It stays disabled while
  that section is dirty, as today.

#### 4.3.1 Hidden folders on Android

`set_folder_hidden_from_agents` is registered on all platforms and writes the
scope file through `mcp_scope::scope_path()`. Hidden folders also gate what
leaves the device for an AI provider (spec 2026-10-08 §4.4), and Android uses
AI providers. So the list moves out of "Agent workspace" into Privacy and
shows everywhere.

**Phase 3's first task** verifies on Android that `scope_path()` resolves to a
writable app-data path. If it does not, that is the one Rust change (§2).

### 4.4 Explanatory copy rule

- **One line.** Every section and every field gets at most one line of help
  under its label, about 90 characters, ending with a period.
- **The rest goes behind "Learn more".** This is an inline disclosure
  (`<details>`), not a tooltip (touch has no hover) and not an external link
  (works offline).
- **Warnings that change what the user should do stay visible.** Examples: a
  privacy warning from `cliPrivacy.ts`, "Read-only in Jodd". They are capped
  at two lines, with Learn more for the rest.
- **The implementer rewrites existing paragraphs to this rule.** Each
  rewritten string is listed in the PR description, so the owner can review
  the wording in one place.

## 5. Navigation (problem B)

### 5.1 Phone pane headers

`PaneHeader.svelte` is used by App.svelte's phone branch.

- **Height and padding:** `--touch-target` plus the safe-area top.
- **Parts:**
  - **Left:** a back button labelled with the parent's name. It
    ellipsizes at 40% width.
  - **Below the bar:** a large title (`--size-xl`). It collapses into the bar
    on scroll, as in Apple Notes; the collapse is optional and left to the
    builder.
  - **Right:** up to two icon actions, each a 48px target.

| Pane | Back label | Title | Subtitle | Right actions |
|---|---|---|---|---|
| Folders (root) | none | "Folders" | — | Settings (gear) |
| List | "Folders" | View name: folder leaf, Smart Folder name, `#tag`, or "Search" | `N notes · <account short name>` (account only when > 1 active account) | Search (focuses the existing search field) |
| Note | List title (e.g. "Projects") | none (the editor's title is the title) | — | Save state (text, §6.1) · ⋯ |

**Implementation notes:**
- The list's title comes from the existing `viewName.ts`.
- NoteList's own `<h2>` title row hides when the phone header shows it. It
  reads `$androidLayoutMode`, per the relaxed rule in §2.
- **Back is `history.back()` everywhere.** For its label to be honest it must
  name the entry **below**.
  - phoneNav keeps a title stack indexed by depth (`paneTitles[depth]`),
    written on every push and replace.
  - The Back label is `paneTitles[depth − 1]`. The table's "Back label" column
    is what that yields on the common path, not a fixed string.
- **Cold start seeds two entries:** `folders` (depth 0), then `list`
  (depth 1). *Spec author's call.*
  - Today `initPhoneNavHistory()` makes `list` the root, so Back from a
    cold-start list sends the app to the background.
  - With two entries the stack is always
    folders → [tags | smart] → list → note, and Back from a list always shows
    Folders.
  - Leaving the app takes one more press than today. The 2026-08-03 press
    counts and `phoneNav.test.ts` change to match.
- **The Folders bar's New note** pushes `list` (the last-viewed folder), then
  `note`. Back from the new note lands on that folder's list.
- The Sidebar's own header and its dead `‹` collapse button do not render on
  phone.

**Bottom bars:**
- **Folders:** New folder (left), AI ✦ (centre), New note (right). New note
  creates the note in the last-viewed folder.
- **List:** New note (right).

The AI ✦ button opens a sheet listing Ask Jodd, Extract, and the existing
Workflows entries.

### 5.2 Phone Folders screen

The Sidebar renders a phone variant of its own markup. Same component,
reading the layout store.

**Shown, in order:**

1. **Each active account:** a small label (account short name · backend),
   then its folder tree.
   - Rows are 48px, with counts.
   - Long-press keeps the folder context menu, which becomes a `sheet`
     on phone.
2. **Inside each account block, after its folders.** *Spec author's call: per
   account, because tag selection and Smart Folders are per account in
   today's stores, so no store changes.*
   - **"Tags"**: one row with a count. It drills into that account's Tags
     pane.
     - Tapping a tag selects just that tag and pushes `list`.
     - A **Select** button in the header turns on multi-select. Rows toggle
       with a check mark, the AND/OR toggle appears once 2 or more are
       selected, and a bottom **Show N notes** button pushes `list`.
   - **"Smart folders"**: one row that drills into that account's Smart
     Folders.
   - **"Recently deleted"**: one row, when the backend has trash.

**Removed from the phone Folders screen, and where each goes:**

| Item | Goes to |
|---|---|
| "All <account>" rows | The account label is tappable and opens "All" |
| Workflows group | AI ✦ sheet |
| Organize | Already hidden on Android |
| Dup pill | Settings › Accounts row and account page. A dot on the Settings gear when any account has duplicates |
| Account chip, add-account panel, Ask Jodd, App settings, About in the footer | Settings (gear), AI ✦ |

**Desktop keeps its Sidebar account controls:** the footer chip and panel,
the dup pill, Remove and Deactivate.

- Settings › Accounts is a second way in on every platform, and the only way
  in on phone.
- To keep the two from drifting (the same rule as §3.3), account actions move
  into `src/lib/controllers/accountActions.ts`: remove with its
  `RemoveAccountOutcome` handling, deactivate, and starting the add-account
  flows. Sidebar and Settings both call it.
- The move does not change behaviour. Fixing open defect D5 (remove awaits
  IPC before mutating) is not part of it.

**Tags and Smart folders panes.** These are new `pane` values:
`'tags' | 'smart'`, between `folders` and `list`. Picking one pushes `list`.
`phoneNav` gains them as ordinary push targets. Depth arithmetic is
unchanged, because the pane depth stays derived from the history entries.

### 5.3 After a selection

| Action | Desktop | Tablet | Phone |
|---|---|---|---|
| Pick folder / tag / Smart Folder | Open the **first note in list order** (pinned first, then sort order); focus stays in the sidebar | Same, and the portrait drawer closes | Push `list`; nothing auto-opens |
| Pick a note | Open it | Open it | Push `note` |
| Folder is empty | Empty state (§5.5) | Same | List shows its empty state |
| Delete the open note | Open the **next** note in the list, else the previous, else the empty state | Same | Pop to `list` |
| Search typing | No auto-open (it would steal the editor while typing) | Same | — |

**Mechanics of "open the first note":**

- **Synchronous.** `selectFolder`, `selectTag` and `selectSmartFolder` set
  `selectedFolder` and `selectedNote` in the same synchronous step. They read
  the first note from the already-loaded, SQLite-backed listing for that
  view.
  - **Gotcha #28:** assigning identity and the values compared against it in
    one turn. Done by a later reactive watcher, the editor would flash the
    old note or blank.
- **If the listing is not loaded yet** (first visit to a folder), selection
  waits for the existing local listing load. It is applied only if the view
  is still the same — `viewSnapshot()`/`viewUnchanged()`. It never waits on
  the remote.
- **Only explicit user picks auto-open.** A `remote-changed` refresh that
  reorders the list never changes the selection (gotcha #6). An open note
  stays open.
- **Gmail `tmp:` drafts** that are unsaved and empty are discarded as today
  when switching.

### 5.4 Tablet

| Width | Folders | List + editor |
|---|---|---|
| 700–1023 (`'tablet'`, mostly portrait) | Overlay drawer, as today: closes on pick, scrim tap or Back | Side by side, resizable |
| ≥ 1024 (`'tabletWide'`, landscape) | Persistent column, 260px, collapsible with the ☰ button (state kept in `localStorage`) | Side by side, resizable |

- **A persistent column, not an icon rail.** Jodd's folders are text; icons
  would not identify them.
- **The top bar keeps ☰** and gains the current view name as its title.
- **Android Back on tablet:**
  - closes an open drawer or modal first, through history entries the same
    way §4.1 does on phone;
  - otherwise reaches the root path (`moveTaskToBack`).
  This is new: today the tablet pushes no entries.

### 5.5 The editor's empty state

Desktop and tablet only. Shown when no note is selected.

- **Folder has notes but none is selected** (rare after §5.3):
  "Select a note" plus the view name.
- **Folder is empty:**
  - Headline: "No notes in <view name>".
  - One line: "Notes you add here sync to <backend>."
  - Primary button: **New note**, with its shortcut hint.
  - In a read-only account the button is absent, and the line says so in one
    sentence.
- **Search with no results:** "No notes match "<q>"" plus **Clear search**.

### 5.6 The phone editor toolbar

- **Top:** the note `PaneHeader` from §5.1 (‹ parent · save state · ⋯).
- **The desktop toolbar row does not render on phone.**
- **Format bar:**
  - one horizontally scrollable row, 48px tall;
  - shown only while the title or body has focus;
  - positioned at `bottom: var(--kb-inset)` (§3.2).
- **Format bar order:**
  - Aa (heading level, which opens a small sheet)
  - B, I, U, S
  - bullet list, numbered list, checklist, quote, code, link
  - `#` (inserts `#` and opens autocomplete, §6.4)
  - `[[` (opens the wikilink picker)
  - hide keyboard ⌄
- **Buttons:**
  - 48 × 48 hit areas, with icon or glyph at 20px;
  - `pointerdown` + `preventDefault` keeps the caret in the editor (the
    touch equivalent of today's `onmousedown`).
- **The body scrolls** so the caret line stays above the bar. The bar's
  height plus `--kb-inset` becomes `scroll-padding-bottom` on the editor
  scroller.
- **The ⋯ sheet holds the note actions:**
  - Pin/Unpin, Move to…, Tags…, Find in note, Copy wiki link, Show graph,
    Duplicate, Refetch.
  - Mark reviewed, when §6.3 applies.
  - Delete, in danger colour, last.
  - Delete's confirm behaviour is unchanged: a confirm only where the
    backend deletes permanently.
  - All actions call §3.3's module.
- **Tablet and desktop** keep the single-row toolbar. Phase 6 makes it
  non-wrapping: overflow formatting buttons collapse into a "More formatting"
  menu when the editor is narrow. Delete moves to ⋯ there too.

## 6. The editor header (problem C)

### 6.1 Layout, top to bottom (desktop and tablet)

**1. Toolbar row.**
- Formatting buttons on the left.
- On the right: **save state** as short text, then **⋯**.
- Save-state strings, from the existing `{#if}` chain, shortened:
  - "Saving…"
  - "Unsaved"
  - "Draft"
  - "Saved"
  - "Not synced" + Try again
  - "Edited on another device" + Reload — this stays visible beside the
    others, as today.
- The full sentence each one replaces becomes its `title`/`aria-label`.

**2. Find bar and link prompt** — unchanged, when open.

**3. Metadata line.**
- **Format:** one line, `--size-sm`, secondary colour, truncating from the
  folder path's left:

  ```
  [account ›] Folder › Sub · <date>   [● status]                    [#N]
  ```

- **Account prefix:** only when more than one account is active. It shows the
  account short name and the backend (`backendLabel`).
- **Folder path:** a button. It keeps today's "reveal folder" behaviour
  (`.ctx-folder`, tests unchanged in intent).
- **Date:** §6.6.
- **Status:** at most one compact indicator. When several apply, the
  priority is:
  1. "Read-only"
  2. "Not synced"
  3. "Changed elsewhere" / `writer · ago` — the trust state, §6.3
  4. "Pin not synced"

  Clicking it opens a popover with the full sentence and its action (Try
  again, Mark reviewed, Reload). The current one-line banners keep their
  wording, but live inside that popover.
  **Exception:** the read-only banner for a whole account (`!canWriteNotes`)
  stays a visible one-line notice under the metadata line, because it
  explains why typing does nothing.
- **`#N`:** the tags button, §6.2. Hidden when N = 0 and tags are not
  editable on this account.

**4. Notices.** At most these, each one line:
- the context notice ("Browsing X. This editor remains on Y" + Close note);
- the folder suggestion ("Suggested folder: X" + Move · Keep here).

**5. Title input** — unchanged (gotchas #11, #17, #27, #28). The orphan-mark
warning stays directly under it.

**6. Body.**

**7. Connections section** (§6.5).

**Removed from above the body:**
- the breadcrumb row;
- the centred monospace date;
- the trust chip;
- the `[[slug]]` chip;
- the tag row.

Each lands somewhere in this section or in ⋯.

**Phone** uses the same metadata line under the `PaneHeader`, with these
changes:
- no account prefix (the Back label already names the place);
- the folder path is dropped when it equals the Back label.

### 6.2 Tags popover

**The `#N` button** opens a `Popover` on desktop and tablet (anchored, with
the `--z-popover` token) and a `sheet` on phone. A small `Popover.svelte`
primitive is added beside `Modal`; it has the same focus and Esc rules, but is
non-modal.

**Contents when tags are editable** (`canEditTags`):
- Current tags as rows: `#tag` plus a remove ×.
  - Remove uses today's `removeTag` path:
    `replaceRanges(findTextRanges(hashMarkMatches(tag)), '')`. It deletes the
    `#` and keeps the word, as now.
- An input with suggestions from `tagsByAccount`, as today's
  `tagSuggestions`.
  - Enter adds the tag via `appendTagToBody`, then `refreshBodyTags` and
    `onInput`. This is the same path as today's add, so it is undoable.
- **Focus.** `appendTagToBody` operates on the live editor. The popover input
  holds focus while typing; the add then runs against the editor DOM, the
  same way today's tag-row input works.
- **Closing** returns focus to the `#N` button.

**On iCloud** (tags not editable, gotcha #38):
- the current tags, read-only;
- one line: "Add or remove tags in Apple Notes.";
- a Learn more disclosure that names the reason.

The permanent hint in the editor is gone.

### 6.3 Review state and the wiki link

**Trust indicator.** The trust chip's information moves into the metadata
status (§6.1). Its label strings come from `trustLabel()`, unchanged:
"changed elsewhere", "changed without a record", `writer · ago`.

Its popover holds:
- the full sentence;
- **Mark reviewed**, with today's disabled-while-`reviewBlocked` rule and
  today's optimistic → rollback;
- the error line.

**Mark reviewed** also appears in ⋯ while the indicator is showing.

**Wiki link.** "Copy wiki link" in ⋯:
- copies `[[slug]]` with today's `copySlug`;
- shows a 1.5s toast: "Copied [[slug]]" (truncated);
- is absent for `tmp:` notes.

**Tests to retarget:**
- `noteEditorTrustChip.test.ts` and `noteEditorReviewCurrency.test.ts`
  retarget `data-testid="trust-chip"` onto the indicator and popover. They
  keep their assertions.
- `noteEditorFolderSuggestion.test.ts` and
  `noteEditorAccountBackendLabel.test.ts` likewise. The backend label shows
  in the account prefix when > 1 account is active, and the test fixture has
  two accounts.

### 6.4 `#`-autocomplete in the body

**Trigger:** typing `#` at the start of a text node or after whitespace or
punctuation, followed by at least zero characters.
- Not inside `pre`/`code`, a link, or a URL-looking token.
- Not on accounts where `tagModelOf(...) === 'inline_objects'` (iCloud).
  There, `#word` is text, not a tag (#38), and suggesting it would mislead.

**List:**
- up to 8 tags from `tagsByAccount`, matching by prefix after
  `normalizeTag`, then by substring;
- anchored at the caret, reusing the `[[` picker's positioning and keyboard
  code;
- on phone, it renders above the format bar.

**Keys:**
- **↑/↓** move.
- **Enter/Tab** replace the typed `#prefix` with `#tag ` through
  `execCommand('insertText')`, so the change is undoable.
- **Esc** closes it, and so does typing a space or punctuation.
- **No match:** the list shows nothing, and the typed text stays as typed. A
  new tag is just text, as today.

### 6.5 Connections and Sources, collapsible

The block after the body — Links to, Linked from, Sources — becomes one
disclosure:

```
›  Sources 4 · Links 3 · Linked from 2
```

- **Collapsed by default.** The open/closed state persists per device
  (`localStorage`, key `jodd.connections.open`).
- **Expanded,** the three groups show as today. SourceList keeps its "Show
  all N" toggle and its refresh behaviour (`sourceListRefresh.test.ts`).
- **The disclosure line is hidden** when all three counts are 0 and nothing
  is loading or failed.
- **"Sources unavailable"** shows in the collapsed line as
  "Sources unavailable · Retry", so a failure is not hidden.

### 6.6 Dates

**One module, `src/lib/format/dates.ts`, owns every displayed date:**

- **`formatNoteDateTime(ms)`** — the editor metadata line:
  `Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short', calendar })`.
- **`formatListDate(ms)`** — NoteList rows, keeping today's
  today/yesterday/weekday/date logic, with the same locale and calendar.
- **Trash** (`TrashList`, `TrashPreview`) moves to these too.

**Locale.** `undefined`, meaning the system locale.

**Calendar** — the General › Date display setting, stored in `localStorage`
like the theme:
- **Automatic** (default): Intl's resolved calendar for the locale.
- **Gregorian.**
- **Buddhist (BE)** — `calendar: 'buddhist'`.

**Upgrade continuity.** Today's code shows BE whenever the timezone offset is
UTC+7. The first time `dates.ts` runs with no stored preference, it checks:
the time zone is `Asia/Bangkok` and the resolved calendar is Gregorian. If
so, it stores **Buddhist**. A Thai user who sees "BE 2569" today keeps seeing
a Buddhist year.

**`formatNoteDate`'s hand-built English-month BE branch is deleted.**

## 7. Constraints that bite (read the full entries in docs/GOTCHAS.md)

| Gotcha | Where it applies here |
|---|---|
| #6 | Auto-open-first-note must not re-fire on worker refreshes; nothing new depends on worker state without the `remote-changed` route |
| #11, #17, #27 | Title input and body stay separate; nothing in the header reads the title from the body |
| #28 | Folder pick sets folder and note in one synchronous step; header elements keep deriving identity + data in one `$:`/`$derived` |
| #31 | Back: modals and drawers own history entries; root still `moveTaskToBack`; never `finish()` |
| #34 | Find bar unchanged; it moves into ⋯ ("Find in note") on phone only |
| #37 | Tag insertion stays on the live DOM (`appendTagToBody`); any string-level append uses `appendHtml`, tested on a whole-document body |
| #38 | iCloud: tags read-only in the popover, no `#` autocomplete |
| Local-first doctrine | No step here awaits the network. Settings saves await local IPC only to report failure; note actions keep optimistic → rollback |

## 8. Testing

**Unit (vitest):**
- `Modal`: focus return; Esc and backdrop routed through the guard; the dirty
  guard; phone presentations chosen only when `$isAndroid` and
  `'phone'`; history entry push/pop and re-push on keep-editing.
- `phoneNav` with overlay entries and the new `tags` / `smart` panes.
- `androidLayoutMode` with three modes.
- `keyboardInset` with a mocked `visualViewport`.
- `dates.ts`: calendar resolution, and the upgrade-continuity rule.
- `#`-autocomplete trigger rules, including iCloud off, code blocks and URLs.
- The auto-open-first-note rule, including the view-changed-while-loading
  case and the delete-selects-next case.
- Settings dirty aggregation and stop-at-first-failure.
- The `noteActions` module, moved with its existing tests.

**Design guards:**
- the ratchet on raw spacing;
- "modal scrolling";
- a new guard that no component except `Modal`/`Popover` sets
  `role="dialog"` or calls `showModal()`, once migration finishes.

**Device live passes:**
- **S23 FE (phone) and Tab S7 (tablet), portrait and landscape.**
  - Keyboard + format bar.
  - Back through note → list → folders → background, with a modal open at
    each level.
  - Settings dirty guard via hardware Back.
  - Long-press sheets.
- **macOS desktop:**
  - the header;
  - the ⋯ menu;
  - the tags popover;
  - auto-open first note;
  - every migrated dialog by keyboard only (Tab cycles inside, Esc closes,
    focus returns).

## 9. Phasing

Each phase is its own PR, shippable alone, in this order:

1. **Foundations:**
   - tokens;
   - `keyboardInset` (plus the S23 FE measurement);
   - `androidLayoutMode` with three modes;
   - `dates.ts` and the Date display setting;
   - the `noteActions` extraction.
2. **`Modal` + `Popover` primitives,** and migration of every dialog except
   Settings.
3. **Settings IA:**
   - the save model;
   - the Accounts section, with `accountActions.ts`;
   - Privacy on Android (verify `scope_path()` first);
   - the copy rule applied.

   This comes before navigation so the phone Folders screen never loses its
   account controls before Settings › Accounts exists.
4. **Phone and tablet navigation:**
   - `PaneHeader`;
   - Folders screen;
   - tags/smart panes;
   - the two-entry cold start;
   - tablet wide column and Back entries;
   - after-selection rules on all layouts;
   - the empty state.
5. **Editor header:**
   - metadata line;
   - ⋯ menu;
   - status popover;
   - tags popover;
   - connections disclosure.
6. **Phone editor:**
   - format bar;
   - ⋯ sheet;
   - `#`-autocomplete (all layouts);
   - non-wrapping desktop toolbar.

## 10. Left to the builder

- Exact icon choices and the large-title collapse animation.
- The precise popover positioning code. Reuse the `[[` picker's where it
  fits.
- The order of dialog migrations within Phase 2, beyond simplest-first.
- The wording of rewritten help text. It must follow §4.4, and every changed
  string is listed in the PR.
- How Sidebar splits its phone markup: an internal `{#if}` or child
  components inside Sidebar's folder. Either is fine, as long as no prop
  forks it.

## 11. Follow-ups found during the audit (not part of this design)

- AppSettings's footer **Clear** uses `window.confirm()`, which WKWebView
  drops. Clearing OAuth credentials probably happens with no confirmation on
  macOS. §4.3 removes it in Phase 3. If Phase 3 is far off, fix it sooner on
  its own.
- LinkSuggestionsModal and DupReviewModal ignore Esc. Phase 2 fixes this.
