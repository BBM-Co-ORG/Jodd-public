---
target: Sidebar
total_score: 24
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 4
target_identity: "file:/Users/kaiwanh/projects/Jodd/.claude/worktrees/impeccable-jodd-934645/src/lib/components/Sidebar.svelte"
target_fingerprint: "sha256:38e8f50b698ee16693a1f4ea72ca542e5dba8986b4227912f4c5b472df477f90"
target_path: /Users/kaiwanh/projects/Jodd/.claude/worktrees/impeccable-jodd-934645/src/lib/components/Sidebar.svelte
timestamp: 2026-11-10T03-55-10Z
slug: src-lib-components-sidebar-svelte
---
Method: dual-agent (A: design review sub-agent · B: detector sub-agent)

Run notes: source-only. Tauri on macOS (WKWebView) exposes no CDP, and the `tauri dev` window is a bare binary the background screenshot tool cannot capture, so no rendered pass and no browser overlay. Scope: src/lib/components/Sidebar.svelte (2,990 lines) and sidebar/PhoneFolders.svelte (390 lines); the tablet column/drawer reuse the desktop Sidebar.

## Design Health Score

| # | Heuristic | Score | Key Issue |
|---|-----------|-------|-----------|
| 1 | Visibility of System Status | 2 | Only the current account has a sync dot (footer chip). Signed-out / blocked / read-only accounts look identical to healthy ones in the tree; phone shows no sync state at all |
| 2 | Match System / Real World | 3 | Apple-like "All …" / "Recently Deleted"; but `localfs:` leaks into labels and prompts show raw `Notes/…` paths |
| 3 | User Control and Freedom | 3 | Optimistic edits roll back, collapse state persists; account panel has no Esc / outside-click close |
| 4 | Consistency and Standards | 2 | "Views" (desktop) vs "Smart folders" (phone); three section-label styles; text glyphs as icons; no lamp rail on phone; native alert() for errors |
| 5 | Error Prevention | 3 | Writes hidden per capability, only empty folders deletable; Remove is a red × at 24px beside the gear |
| 6 | Recognition Rather Than Recall | 2 | The "current account" that `+` and the AI menu act on is not marked in the tree; read-only reasons only after a right-click |
| 7 | Flexibility and Efficiency | 3 | Roving tabindex, arrows, Home/End, Shift+F10 work; no folder filter / type-ahead across several trees |
| 8 | Aesthetic and Minimalist Design | 2 | Five Views rows + All + Tags repeated per account; "0" pill on every empty folder; crowded account header |
| 9 | Error Recovery | 2 | alert() with raw `${e}` (7 sites); a failed account removal closes the panel silently |
| 10 | Help and Documentation | 2 | Help lives in `title` tooltips, which touch screens never show |
| **Total** | | **24/40** | **Acceptable** |

## Design Specificity Verdict

**LLM assessment:** the surface is Jodd's (lamp rail on the open folder, opaque measured badges, per-backend write gating with plain-words reasons, Agents group, refused-folder badge, backend tag), but the structure is a generic multi-account mail sidebar: header, then one full tree per account, then an account switcher in the footer. Each account repeats All, Notes, five Views rows and Tags, so accounts read as silos. PRODUCT principle 4 "One workspace, many doors" does not show in the information architecture.

**Deterministic scan:** `impeccable detect` returned 0 findings across 61 rules (exit 0), also with config, inline ignores and design-system suppression off. A probe file proved the detector parses .svelte, so the clean result is real: the token/colour/icon guards from #309 hold. The detector's rules cover none of the problems below; static checks by B added: 10 controls under the 28px desktop target and no `(pointer: coarse)` rule in either file; account email, Move-to folder names and the tag-menu header inherit line-height `normal` (~1.2, under the Thai floor of 1.5); Move-to flyout labels never truncate; PhoneFolders expand toggle is tabindex -1 with no row-level arrow handler (keyboard-unreachable); `role="menu"` on 7 containers with 0 `menuitem`; 47 off-scale px literals and inline indent math in Sidebar (PhoneFolders uses tokens).

**Visual overlays:** none. Browser injection was not possible (no CDP on WKWebView, no app data without the Tauri runtime).

## Overall Impression

Calm, honest at the level of single actions, and the keyboard tree from #303 is solid. The weakness is one level up: as accounts multiply the sidebar grows linearly in identical chrome, and the state that matters most per account (signed out, blocked, read-only, unsent) lives in a footer chip that only covers one account. Biggest opportunity: make the account header carry its own state and stop repeating Views per account.

## What's Working

1. **Honest write gating.** Each affordance is hidden by the capability it would use (folders vs sidecars vs notes, Sidebar.svelte:366–392), and missing actions are explained in plain words in the menu (Outlook 1909–1919, agent folders 1928). PRODUCT principle 3 at the action level.
2. **Lamp and measured contrast.** One rail, on the open folder, in --accent-mark (2288–2300); opaque counts and badges with their reasoning beside the rule; the chip dot is never green when it can't tell and is not colour-only (accountDot.ts).
3. **Keyboard tree and touch-safe destructive controls.** navRows gives one Tab stop with arrows, Home/End, ←/→ expand and Shift+F10. Hover-revealed account actions also show on :focus-within and stay visible unless the device has hover (2669–2704), so a touch screen never gets an invisible live Remove.

## Priority Issues

**[P1] Each account's state is invisible in the tree.** Account headers (1384–1410) show email, backend tag and dup pill, nothing about sync, signed-out, blocked or read-only. Sync status covers only the current account via the footer chip (1292–1311); `blocked_reason` hides inside the account panel (1654–1661); the phone shows none. *Why:* with 3–5 accounts a signed-out iCloud and a healthy Gmail look the same, and edits pile up unsent while the user believes they synced (the auth-loss case #301 just made non-destructive). *Fix:* a status slot on every account header and phone account label (signed-out/blocked from account fields, pending from the existing poll for all active accounts); an inline "Sign in again" row under a blocked header; a quiet read-only marker for capability-limited backends. *Command:* /impeccable harden

**[P1] Keyboard and screen-reader dead ends.** Desktop "Move to" has no click/key handler (1875) and its flyout opens only on :hover (2962): keyboard users cannot move a folder (both assessments found this). Menus opened with Shift+F10 don't take focus and Esc doesn't return it (652–664). Five accounts produce five identical "Remove account" / "Account settings" / "Expand account" labels. Rows have no aria-level; read-only explanation lines are aria-hidden (1916, 2033); the account panel is role=menu without menuitems and no Esc. PhoneFolders: expand toggle unreachable, no aria-current / aria-expanded. *Fix:* open Move-to on click/Enter/→; focus first item on open and return focus on close; per-account labels (`Remove ${accountDisplay(a)}`); aria-level on rows; explanations as real text; panel as a disclosure with Esc/outside close; phone aria-current + reachable toggle. *Command:* /impeccable harden

**[P1] Tablet touch targets far under 48px.** The tablet column and drawer reuse the desktop markup and Sidebar has no (pointer: coarse) rule. Measured: folder rows ~36px, new-folder 22×22, account collapse 16×16, expand toggle 18×18, remove/settings 24px wide, settings gear ~18×22, tag pills ~20–22px, tag mode/clear 16–18px. Tab S7 is a primary device. *Fix:* one `@media (pointer: coarse)` block: rows min-height var(--touch-target), icon buttons get a 48px hit area via padding or ::after. *Command:* /impeccable adapt

**[P1] The information architecture doesn't scale past two accounts.** Per account: header, All, Notes tree, Views (4 smart folders + Recently Deleted), Tags (1411–1595). Five accounts ≈ 45 chrome rows, 20 of them identical smart-folder rows, before any user folder. "All <email>" repeats the header (1426). The current account is unmarked (1360). *Fix:* header opens All (as the phone does) and the All row goes; Views hoisted into one cross-account group with the account as a list filter, or accounts after the first collapsed by default; mark the current account's header. *Command:* /impeccable distill

**[P2] Native alerts with raw backend text; silent remove failure.** alert() at 920, 931, 981, 1068, 1080, 1096, 1126 (several interpolate `${e}`), bypassing errorText.ts from #308. `removeAccount` → 'failed' just closes the panel (1147–1148). *Fix:* route through failureText + ErrorBar or PromptDialogs; show Delete disabled with a menu-reason line when a folder has sub-folders instead of alerting after the click; keep the panel open with an inline danger line on remove failure. *Command:* /impeccable clarify

## Persona Red Flags

**Alex (power user):** no folder filter or type-ahead across five trees; switching account is chip → panel → row; account collapse toggle is a separate Tab stop outside the roving set; tag pills are one Tab stop each, unreachable by arrows; the folder menu has 8 items with two identical sparkle icons.

**Sam (accessibility):** cannot move a folder; menus don't take focus; five identical "Remove account" buttons; no aria-level; read-only reasons aria-hidden; refused-folder "!" reason only in `title`; unlabelled account role=group; collapse toggle and tags header lack aria-expanded.

**Casey (mobile):** no sync state anywhere on phone; folder actions only by long-press with no visible affordance; nested role=button inside role=button (PhoneFolders 163–171); Tags, Smart folders, Recently deleted repeat per account; active phone row has no lamp rail and no aria-current; every phone folder icon is amber (--accent-action, PhoneFolders 348), diluting the lamp. The bottom bar (New folder · AI · New note) in the thumb zone is good.

**Nok (Apple Notes refugee: iCloud, Gmail, Outlook, Local Folder; Thai + English):** mono email at 200px loses the domain, the part that tells two addresses apart; Local Folder appears as `localfs:…`; Outlook's read-only folders only discoverable by right-click; a signed-out iCloud is invisible; the remove confirm never says the Apple Notes copies are untouched; Thai folder names and tags (110px) truncated with tooltip only; account email sits on line-height ~1.2.

## Minor Observations

- Text glyphs as icons against DESIGN.md: `+` (1364), `＋` (1705, 1713, 1726, 1841), `●`/`○` (1614, 1634), `!` (1469); Local Folder, SSH and SSH-advanced reuse the folder icon. The #309 lone-glyph guard misses glyphs that sit beside a label.
- Three section-label styles (.group-header 600/0.04em, .tags-header 700/0.06em, .account-panel-header 700/0.5px); DESIGN's Section Label is 700/0.04em.
- Literal indent `{8 + depth*14}px` and 16px rows misalign icons: All/Views icons sit ~16px left of Notes'.
- Amber used outside its two jobs: current account in the panel (--accent-wash), account-row dot, chip "pending", dup-pill CTA.
- `.tag-clear:hover` turns --danger (red on a non-failure); `.tags-header:hover` and `.tag-mode:hover` change nothing.
- Naming drift: Views/Smart folders, Recently Deleted/Recently deleted; the eye icon means four things.
- Folder counts are direct notes only: a collapsed parent reads 0 while its children hold notes.
- Unmeasured opacity: inactive rows 0.7, inline 0.7 explanation text, disabled items 0.4.
- Folder-load failure goes to console only; nothing shows while folders load.
- Tag match mode shown as "AND"/"OR".
- Ask Jodd, a positioning pillar, is a 14px muted sparkle in the footer.
- Move-to flyout has min-width 180px and no max-width; `.account-row-note` nowraps without ellipsis.
- PhoneFolders:382 stale comment ("10px" pill; rule is --radius-sm, 4px).

## Questions to Consider

1. If the principle is "one workspace, many doors", why must the user pick a door before every view? Should Views, Tags and All belong to the workspace, with accounts as a filter?
2. Should each account's state be as loud as a refused folder push, instead of living in one footer chip that covers one account?
3. Does the header need the email at all? Should accounts be nameable ("Work iCloud", "Personal Gmail") so a truncated mono address stops being their identity?
