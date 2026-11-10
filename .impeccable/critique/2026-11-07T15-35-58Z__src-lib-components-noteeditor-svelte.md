---
target: the note editor
total_score: 28
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 1
target_identity: "file:/home/user/Jodd/src/lib/components/NoteEditor.svelte"
target_fingerprint: "sha256:92a53e06ef08cfe9d03e9ff7ed455e578f3508932e619747e5c787590d0f307c"
target_path: /home/user/Jodd/src/lib/components/NoteEditor.svelte
timestamp: 2026-11-07T15-35-58Z
slug: src-lib-components-noteeditor-svelte
closed: true
---
Method: dual-agent (A: design review sub-agent · B: detector + browser sub-agent)

## Design Health Score
| # | Heuristic | Score | Key Issue |
|---|---|---|---|
| 1 | Visibility of System Status | 3 | Blocked state named twice ("On this device" / "Not synced"); save-state live region re-created per change |
| 2 | Match System / Real World | 3 | "#0", "Refetch", lowercase "changed without a record" |
| 3 | User Control and Freedom | 3 | Find bar closed on Esc only from its inputs |
| 4 | Consistency and Standards | 2 | Two vocabularies for "touched"; buttons in UA font; focus not blue in places; mixed disabled opacity |
| 5 | Error Prevention | 3 | Strong gating and confirms |
| 6 | Recognition Rather Than Recall | 3 | Phone format bar had no pressed state |
| 7 | Flexibility and Efficiency | 3 | No arrow keys in menus |
| 8 | Aesthetic and Minimalist Design | 3 | Notices stack at one muted weight |
| 9 | Error Recovery | 3 | Some raw error strings |
| 10 | Help and Documentation | 2 | Merge-or-copy explanation title-only (unreachable by touch) |
| **Total** | | **28/40** | **Good** (was 23/40 Acceptable) |

## Priority Issues
- [P1] Collision reassurance was the quietest text on screen -> fixed (touched notice: body ink on wash, wraps, detail visible).
- [P2] One state, two words; touched vocabulary lowercase -> fixed (no toolbar word when blocked; sentence case).
- [P2] Menus/find broke keyboard and focus rules -> fixed (arrow keys, find Esc, blue focus).
- [P2] Buttons in UA font -> fixed (font-family inherit; menu rows body size).
- [P2] Trust chip cut off who/when -> fixed (wider cap).

## Detector
3 findings: quote bar side-tab (false positive, sanctioned), .conn-link 11px radius (fixed), focus ring 3px radius (low impact). Browser overlay: clean at 1200/800/390.
