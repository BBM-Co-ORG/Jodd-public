---
target: Sidebar
total_score: 31
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 0
target_identity: "file:/Users/kaiwanh/projects/Jodd/.claude/worktrees/impeccable-jodd-934645/src/lib/components/Sidebar.svelte"
target_fingerprint: "sha256:969ef289174a04ea630f3dc76132c0d20c3828a2a71d3e5f5346de701519f18d"
target_path: /Users/kaiwanh/projects/Jodd/.claude/worktrees/impeccable-jodd-934645/src/lib/components/Sidebar.svelte
timestamp: 2026-11-10T05-41-18Z
slug: src-lib-components-sidebar-svelte
previous: 2026-11-10T03-55-10Z__src-lib-components-sidebar-svelte.md (24/40)
---
Method: re-score after the fixes (spec docs/superpowers/specs/2026-11-10-sidebar-workspace-design.md), single reviewer.

Run notes: rendered this time. The desktop Sidebar ran in a browser against the Vite dev server with a throwaway fake Tauri backend (three accounts: Gmail current, iCloud with 3 changes waiting, Outlook signed out), in light and dark, at 800 and 1280 wide. Checked by eye: the Views scope, account headers and their marks, the signed-out notice, the duplicates notice, the folder menu with Delete disabled, Move to opened from the keyboard, focus back on the row after Esc, the account panel. Not rendered: the phone pane and a coarse pointer (the tablet's 48px targets are verified from CSS and tests only). `impeccable detect` on Sidebar.svelte and PhoneFolders.svelte: 0 findings.

## Design Health Score

| # | Heuristic | Before | Now | What changed / what is left |
|---|-----------|--------|-----|-----------------------------|
| 1 | Visibility of System Status | 2 | 4 | Every account header carries its own state: alert dot when signed out or refused, a muted ring while changes wait, nothing when synced. Signed-out gets a notice row with Sign in again. The phone shows signed-out and blocked (it polls nothing, by design, so no waiting ring there) |
| 2 | Match System / Real World | 3 | 3 | Prompts, confirms and errors name the folder ("Rename "Work"", not `Notes/Work`); the remove confirm says the notes stay in Gmail / iCloud / Outlook. Left: a Local Folder still reads `localfs:…` in the remove confirm and tooltips (accountDisplay) |
| 3 | User Control and Freedom | 3 | 4 | Account panel closes on Esc (focus back on the chip) and on a click outside; menus hand focus back to their row |
| 4 | Consistency and Standards | 2 | 3 | One Section Label style; drawn icons for +, ●/○, ! and SSH; the lamp rail and aria-current on the phone; errors in the app's error bar, not alert(). Left: the phone row says "Smart folders" under a "Views" label |
| 5 | Error Prevention | 3 | 3 | Delete is shown disabled with its reason on a folder that holds folders; the remove confirm says what is and isn't lost. Left: Remove is still an × beside the gear |
| 6 | Recognition Rather Than Recall | 2 | 3 | The current account's header is in full ink and announced; "Views · kaiwan.h · Gmail" names the scope; a lock marks read-only folders. Left: the lock's reason is a tooltip |
| 7 | Flexibility and Efficiency | 3 | 3 | Menus take focus, move with arrows, → opens Move to. Left: no folder filter or type-ahead |
| 8 | Aesthetic and Minimalist Design | 2 | 3 | Views and Tags once, not per account; the All row folded into the header; empty folders show no "0". With three accounts the column lost 14 rows of chrome |
| 9 | Error Recovery | 2 | 3 | failureText through the error bar; a failed remove keeps the panel open and says so; Sign in again from the tree |
| 10 | Help and Documentation | 2 | 2 | More reasons are visible text (Delete, signed out), but read-only and refused-folder reasons still live in tooltips, which touch never shows |
| **Total** | | **24** | **31/40** | **Good** |

## Priority issues — status

- **[P1] Each account's state invisible in the tree — fixed.** Header marks, notice rows, Sign in again; desktop polls `count_pending_pushes` (SQLite only, gotcha #15) for every active account on the 5s cadence.
- **[P1] Keyboard and screen-reader dead ends — fixed.** `menuKeys` (focus first item, ↑↓ Home End, → / ← for Move to, Tab closes, focus returns); every item is a `menuitem`; Move to opens on click, Enter or →; per-account labels ("Remove kai · Outlook"); explanations are real text; the account panel is a disclosure; the phone's rows have aria-current, aria-expanded and navRows. Not done: aria-level on rows.
- **[P1] Tablet touch targets — fixed in CSS, not seen on a device.** One `@media (pointer: coarse)` block: rows and icon buttons at 48px, pills at 40px with 8px between them.
- **[P1] Structure past two accounts — fixed as specified.** Views and Tags once at the top for the current account; the header is the account's All row. The all-accounts aggregate was left out on purpose (it touches the save path; spec decision 1).
- **[P2] alert() and the silent remove failure — fixed.**

## Still open

1. Phone and tablet need a device look (Tab S7, S23 FE): the new Views block, the signed-out notice and the 48px rows.
2. Read-only and refused-folder reasons are tooltip-only (heuristic 10).
3. Local Folder's `localfs:` prefix in accountDisplay.
4. Nameable accounts and a folder filter (the critique's questions 2 and 3) stay out of scope.
