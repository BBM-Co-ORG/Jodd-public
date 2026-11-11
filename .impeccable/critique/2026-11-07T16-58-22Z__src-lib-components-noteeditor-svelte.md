---
target: the note editor (final state, re-run)
total_score: 29
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 3
target_identity: "file:/home/user/Jodd/src/lib/components/NoteEditor.svelte"
target_fingerprint: "sha256:70d37d15f1c48c5d1e31baa3c8759f0d3657b364e77552eab000bac2c3450b87"
target_path: /home/user/Jodd/src/lib/components/NoteEditor.svelte
timestamp: 2026-11-07T16-58-22Z
slug: src-lib-components-noteeditor-svelte
---
Method: dual-agent (A: design review sub-agent · B: detector + browser sub-agent). Final state, main at a9f085f.

## Design Health Score
| # | Heuristic | Score | Key Issue |
|---|---|---|---|
| 1 | Visibility of System Status | 3 | Honest save words; find "1 / 2" with nothing marked when the match is in the title; a refused push unmounts the live region |
| 2 | Match System / Real World | 3 | Apple Notes style names; "Changed without a record", "Refetch", "Copy wiki link" leak |
| 3 | User Control and Freedom | 3 | Esc everywhere, safe default; Tab in the body always indents (focus cannot leave) |
| 4 | Consistency and Standards | 3 | One fmtButton snippet; find bar on literals, Replace a checkbox among aria-pressed toggles |
| 5 | Error Prevention | 3 | Keep-both, confirm before discard, row hidden when read-only |
| 6 | Recognition Rather Than Recall | 3 | Shortcut tooltips and menu column; refusal reason only behind a click |
| 7 | Flexibility and Efficiency | 3 | Full shortcut set; none for Replace, ⋯, Tags, Graph |
| 8 | Aesthetic and Minimalist Design | 3 | Calm at rest; "Edited on another device" said twice; notice stack |
| 9 | Error Recovery | 2 | Refused push: 12px chip, reason in popover, only Try again on a permanent refusal |
| 10 | Help and Documentation | 3 | Cheat sheet, placeholder hint, inline merge explanation; nothing equivalent on Android |
| **Total** | | **29/40** | **Good** |

## Priority Issues
- [P1] Find bar overflows the desktop editor at the minimum window width (481px row in a 350px pane; Close, count, ‹ › off-screen). Fix: container query wraps the row like .find-bar.touch. /impeccable adapt
- [P1] Refused push silent to assistive tech, little recovery (live region unmounted; chip not live; reason behind a click; Try again only). /impeccable harden
- [P1] Tab in the body is a keyboard trap (NoteEditor.svelte ~882), WCAG 2.1.2. /impeccable harden
- [P2] "Edited on another device" said twice; the toolbar copy reflows four buttons into overflow at 390px. /impeccable distill
- [P2] Current find match invisible when it is in the title. /impeccable polish

## Detector
3 CLI findings: quote bar side-tab (false positive, sanctioned), FormatBar mask #000 (false positive: mask alpha only), global focus ring 3px radius (real, advisory). Overlay clean at 1200/800/390 and simulated Android; staged collision: tiny-text on the 11px toolbar "Edited on another device" label. Only desktop control under 28px tall: find "Replace" label (14px).
