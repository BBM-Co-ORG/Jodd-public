# Android Sub-project 2 — mobile UI shell: navigation model

**Written 2026-08-03**, following
[HANDOFF-2026-08-03-android-subproject-2.md](../HANDOFF-2026-08-03-android-subproject-2.md).
Scope: the navigation skeleton only — how the three desktop panes (folder
tree, note list, note editor) map onto phone and tablet Android layouts, and
how back navigation works. Touch-target sizing for the format toolbar, the
`[[` wikilink picker, and hover-only affordances (folder action buttons, tag
tooltips) are real gaps from the handoff but are component-level polish that
doesn't depend on this decision — deferred to a follow-up spec once the shell
itself is walkable.

## Problem

Jodd's UI is a fixed three-pane desktop layout (`App.svelte`): Sidebar (folder
tree / accounts / tags / Smart Folders), NoteList, NoteEditor, laid out with
draggable resizers. Sub-project 1 shipped this layout unchanged on Android —
functionally correct, but at phone width all three panes are squeezed and the
editor is clipped. There is no long-press replacement for the desktop's
right-click context menus, and no back-button handling — the Android system
back gesture currently does nothing useful inside the app.

## Non-goals

- No change to `src-tauri/` — this is frontend-only, per the handoff's
  premise that the split (headless core vs. UI shell) held because the
  frontend never touches Gmail or the host OS directly.
- No change to the desktop DOM/CSS path. Windows is the primary platform;
  whatever this introduces must be inert there.
- No new routing library. `history.pushState`/`popstate` is used directly —
  introducing `svelte-spa-router` or similar would add a new navigation
  concept on top of the reactive-store navigation `App.svelte` already owns,
  for no benefit here.
- No touch-target rework of existing components (toolbar, `[[` picker, hover
  affordances) — see Scope above.

## Design

### Layout mode is Android-gated first, viewport-gated second

`isAndroid` (`src/lib/stores/platform.ts`, already wired) is the **outer**
gate. When `$isAndroid` is `false`, `App.svelte` renders exactly the existing
three-pane markup — unchanged, so desktop cannot regress by construction, not
by convention.

When `$isAndroid` is `true`, a new derived value picks between two Android
layouts based on viewport width:

```ts
// src/lib/stores/viewport.ts
export const viewportWidth = readable(window.innerWidth, (set) => {
  const onResize = () => set(window.innerWidth);
  window.addEventListener('resize', onResize);
  return () => window.removeEventListener('resize', onResize);
});

const ANDROID_TABLET_BREAKPOINT = 700; // px; tune on-device, see Testing

export const androidLayoutMode = derived(
  [isAndroid, viewportWidth],
  ([$isAndroid, $width]) => ($isAndroid && $width >= ANDROID_TABLET_BREAKPOINT ? 'tablet' : 'phone'),
);
```

Only consulted inside the `$isAndroid` branch of `App.svelte`'s template — a
desktop window narrowed below 700px does not trigger either Android layout.

### Phone layout: one pane at a time, on a browser-history stack

A new store, `activePane: 'folders' | 'list' | 'note'`, decides which single
component `App.svelte` mounts full-screen: Sidebar, or (NoteList /
TrashList), or (NoteEditor / TrashPreview) — same components as desktop,
mounted with the same props, no new wrapper components (per the earlier
decision to keep divergence in `App.svelte` rather than spreading `mobile=
{true}` conditionals through every leaf component).

Navigation stack semantics:

- **Stack root is `list`**, not `folders`. `selectedFolder` already persists
  across the session (existing store), so cold start goes straight to the
  last-viewed folder's note list — matching desktop's existing behavior and
  avoiding an extra tap on every launch.
  **Superseded during implementation:** on-device verification (Task 7)
  established that `folders` is the effective root of the phone nav stack,
  not `list` as assumed here — see
  `.superpowers/sdd/2026-08-03-android-mobile-ui-shell/task-7-report.md` for
  the full trace. This is shipped, verified-correct behavior; the bullets
  below describing push/pop mechanics still apply, just with `folders` as
  the root entry.
- Opening a folder from the Sidebar pushes `{pane: 'list', folder}` via
  `history.pushState` and sets `activePane = 'list'`.
- Opening a note from the list pushes `{pane: 'note', uuid}` and sets
  `activePane = 'note'`.
- Explicitly navigating to the folder tree (a "Folders" button in the list
  pane's header — the phone layout's only new chrome) pushes `{pane:
  'folders'}`.
- A `popstate` listener reads `event.state.pane` and sets `activePane`
  accordingly — no manual stack array to keep in sync; `history` already is
  the stack.
- Android's system back gesture / hardware back button triggers the
  WebView's native `history.back()` when there is history, which fires
  `popstate` — this requires no Tauri plugin and no custom back-button
  listener. Verify this assumption on-device early (see Testing) since it is
  the one part of this design not already confirmed against a real device.
- Back from `list` → `folders`; back from `folders` (no more history) → the
  Activity closes, matching standard Android behavior for a root screen.
- The Trash pane (`$selectedFolder === '__TRASH__'`) is just another `list`
  destination in the stack — no special-casing beyond what `App.svelte`
  already does to pick `TrashList`/`TrashPreview` over `NoteList`/
  `NoteEditor`.

State ownership: `activePane` and the history stack live in `App.svelte`
alongside the existing `selectedFolder`/`selectedNote` store wiring they
mirror. They do not replace those stores — `selectedFolder`/`selectedNote`
still drive *what* is shown; `activePane` only drives *which pane is visible*
on a screen too narrow to show more than one.

### Tablet layout: two panes, folder tree as a drawer

Landscape tablets get NoteList + NoteEditor side by side, permanently — the
same two-pane relationship desktop already has between its last two columns.
The folder tree does not get a permanent third column (there usually isn't
width for three comfortably at tablet size); instead it reuses the **existing
`sidebarCollapsed` mechanic** already in `App.svelte` for desktop's collapse
button:

- `sidebarCollapsed` defaults to `true` on tablet.
  **Superseded during implementation:** the shipped code does not reuse
  `sidebarCollapsed` for tablet at all — it introduces a separate local
  `tabletDrawerOpen` variable (defaulting to closed) instead, since desktop's
  `sidebarCollapsed` semantics (expanded-by-default, user-toggled per
  session) didn't match the drawer's needs. See
  `.superpowers/sdd/2026-08-03-android-mobile-ui-shell/task-7-report.md`.
- Expanding it does not push the list/editor columns over (as desktop's
  resizable sidebar does) — it renders as an overlay drawer positioned above
  the list pane, dismissed by tapping outside or picking a folder.
- No `history.pushState` involved on tablet — both remaining panes are
  simultaneously visible, matching desktop's existing note-selection model
  where there's nothing to "navigate back" from.

### Long-press replaces right-click, reusing the existing menus

A new Svelte action, `src/lib/longpress.ts`, detects a sustained touch
(~500ms, cancelled on move past a small threshold) and dispatches the same
event shape `NoteContextMenu.svelte` and Sidebar's folder context menu
already consume from their `oncontextmenu` handlers (synthesizing `clientX`/
`clientY` from the touch point). This changes only the **trigger** — every
existing menu option (move-to-folder, delete, pin, refetch) and the desktop
right-click path are untouched. `use:longpress` is added to the same
elements that already have `oncontextmenu`, not a parallel menu
implementation.

### Known trade-off: the `isAndroid` store starts `false`

`isAndroid` (`platform.ts`) resolves asynchronously (`invoke('platform_name')`)
and starts `false` "so the UI never flashes features away on desktop" — a
deliberate existing choice. On Android this means the very first paint
briefly renders the desktop three-pane layout before flipping to the phone/
tablet layout once the invoke resolves. This is accepted as a minor,
sub-second flash rather than engineered around (e.g. blocking initial render
on the platform check) — consistent with YAGNI and because the invoke is
local, not network-bound. Revisit only if it's visible on-device and
bothers users.

## Testing

- Unit: `androidLayoutMode`'s derivation logic (pure function of `isAndroid`
  + width, testable without a Tauri runtime, same pattern as
  `deriveIsAndroid` in `platform.ts`).
- On-device (Galaxy S23 FE + Infinix X6821, matching Sub-project 1's
  verification devices): confirm the WebView's back-button-triggers-
  `history.back()` assumption holds on both — the handoff's edge #11 lesson
  ("a port verified on one Android device is not verified") applies here
  too, and this is exactly the kind of platform-behavior assumption that
  bit Sub-project 1 more than once. Also confirm the tablet breakpoint value
  against whatever landscape tablet/DeX is available.
- Manual pass through the full stack: folders → list → note → back → back →
  exits app; long-press on a note opens the existing context menu with all
  actions working; tablet layout shows two panes with the folder drawer
  overlay opening/closing without disturbing list/editor width.
- Desktop regression check: existing desktop e2e/manual pass (three-pane
  layout, resizers, right-click menus) unchanged — no automated test exists
  for this today per the codebase, so this is a manual confirm.
