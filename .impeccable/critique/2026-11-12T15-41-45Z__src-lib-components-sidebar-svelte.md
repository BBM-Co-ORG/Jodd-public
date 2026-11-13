---
target: sidebar navigation, all platforms
total_score: 23
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 3
target_identity: "file:/home/user/Jodd/src/lib/components/Sidebar.svelte"
target_fingerprint: "sha256:b4345fe9810f0801bb66b1f723385616e0a90f7f16e48756efb551145f036188"
target_path: /home/user/Jodd/src/lib/components/Sidebar.svelte
timestamp: 2026-11-12T15-41-45Z
slug: src-lib-components-sidebar-svelte
---
Method: dual-agent (A: design review · B: detector + browser evidence)
Target: sidebar navigation on every platform — src/lib/components/Sidebar.svelte (desktop, tablet drawer) and src/lib/components/sidebar/PhoneFolders.svelte with PhoneTagsPane/PhoneSmartPane (phone). Mode: Operate.

## Design Health Score: 23/40 (Acceptable)
| # | Heuristic | Score | Key issue |
|---|---|---|---|
| 1 | Visibility of system status | 2 | Phone shows only signed-out/blocked: no waiting ring, no read-only lock. Current account marked by #222 vs #3d3d3d only. |
| 2 | Match system / real world | 2 | One concept, three names: Views, Smart views, Smart folders. Recently Deleted (a backend's trash) filed as a "view". |
| 3 | User control and freedom | 3 | Changing which account Views/Tags show requires navigating into that account, which also changes the list. |
| 4 | Consistency and standards | 1 | "Views" holds different things per platform; eye icon means three things; phone account label has no chevron/backend tag/marks. |
| 5 | Error prevention | 3 | Microsoft trash hidden for a stated reason; Remove has undo. |
| 6 | Recognition rather than recall | 2 | Tags header never names its account; view meanings live in tooltips touch never shows. |
| 7 | Flexibility and efficiency | 2 | 528 tags, A–Z only, no filter anywhere; phone needs a drill-in to reach 4 views. |
| 8 | Aesthetic and minimalist design | 3 | Calm closed; opening Tags floods the column (uncapped .tag-chips). |
| 9 | Error recovery | 3 | Signed-out note under its account with Sign in again; folder-blocked marker. |
| 10 | Help and documentation | 2 | Tooltips only. |

Cognitive load: 4/8 checklist items fail (chunking, minimal choices, working memory, progressive disclosure).

## Design specificity
States and copy are Jodd's own (signed-out note "Its notes and unsent changes are kept here", Microsoft trash rule, user-words view names). Structure is stock notes-app; the phone Folders screen reads as a settings hub. Detector: 0 findings on the five files (regex-only scan for .svelte — an undercount). Overlay: 1 finding, cream-palette on <body> (html background = --border-subtle showing through pane seams) — false positive against the declared warm-paper identity.

## Priority issues
- [P1] Naming and grouping differ by platform; Recently Deleted is a per-account place filed as a workspace view. Fix: Views = the four computed lists only, same rows on both layouts; drop "Smart views"; Tags its own section; Recently Deleted last row of each account. (/impeccable clarify)
- [P1] 528 tags have no way in: desktop cloud uncapped (buries every account), phone list ~25,000px, no filter. Fix: filter field on both; cap the desktop cloud. (/impeccable distill)
- [P1] The account Views and Tags act on is half-labelled (Tags never) and changes as a side effect of navigation. Fix: one visible, changeable scope above both, shown only with 2+ accounts. (/impeccable layout)
- [P2] Phone shows less account state than desktop: no waiting ring, no lock, no chevron to fold an account. Fix: phone account header = desktop's header at touch size; poll the SQLite count while Folders is on screen. (/impeccable harden)
- [P2] Account header crowding: Outlook name cut to "kai…" by backend tag + dot + lock + count in ~200px; desktop hit areas under 28px (+/‹ 22px, fold toggles 23px, tag pills 21px, expand 18px). (/impeccable polish)

## Persona red flags
- Owner (1 Gmail, 528 tags, S23 FE + Mac): Mac Views→Not linked is 1 click, phone is Views→Smart views→Not linked with Tags inside Views; phone Tags is an A–Z scroll to reach "music"; on the Mac opening Tags hides all 12 folders.
- Riley (4 accounts): tag cloud silently swaps to iCloud's after picking an iCloud folder; Recently Deleted vanishes when Outlook is current; iCloud's 3 waiting changes invisible on phone.
- Casey (mobile, distracted): disabled New folder and view meanings explained only by tooltips.

## Minor
"VIEWS· kaiwan" spacing on phone; magnifier icon on Not linked reads as search; red trash icon on a navigation row; phone Views rows indented 60px while account labels sit at 16px; desktop account names align with the row icons above, not their text.

## Questions
- If Views covered all accounts, would a scope label be needed at all?
- With 528 tags, are tags navigation or search?
- Would the phone feel like the Mac if its Folders screen were the desktop markup at touch size?
