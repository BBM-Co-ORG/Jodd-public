---
target: the note editor (final state)
total_score: 28
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 2
target_identity: "file:/home/user/Jodd/src/lib/components/NoteEditor.svelte"
target_fingerprint: "sha256:70d37d15f1c48c5d1e31baa3c8759f0d3657b364e77552eab000bac2c3450b87"
target_path: /home/user/Jodd/src/lib/components/NoteEditor.svelte
timestamp: 2026-11-07T16-46-42Z
slug: src-lib-components-noteeditor-svelte
---
Method: dual-agent (A: design review sub-agent · B: detector + browser sub-agent)

## Design Health Score
| # | Heuristic | Score | Key Issue |
|---|---|---|---|
| 1 | Visibility of System Status | 3 | Refused push: toolbar/header word now hidden; only one chip at a time |
| 2 | Match System / Real World | 3 | "Refetch", "Copy wiki link", symbol-soup placeholder |
| 3 | User Control and Freedom | 3 | Good exits; Tab cannot leave the body |
| 4 | Consistency and Standards | 3 | Three notice-button styles; find bar on literals; native blue checkbox |
| 5 | Error Prevention | 3 | Keep-both, confirm before discard, writes hidden not failing |
| 6 | Recognition Rather Than Recall | 3 | "#N", shortcuts only under ⋯ |
| 7 | Flexibility and Efficiency | 3 | No shortcut for ⋯/Tags/Move/Graph; Aa modal |
| 8 | Aesthetic and Minimalist Design | 3 | "Edited on another device" said twice; notice stack on phone |
| 9 | Error Recovery | 2 | Refused push: red, no "your text is safe here", only Try again on a permanent refusal |
| 10 | Help and Documentation | 2 | No help or shortcuts entry on Android |
| **Total** | | **28/40** | **Good** |

## Priority Issues
- [P1] A refused push loses all reassurance (regression from step 7's one-voice change). Fix: popover leads "Saved on this device. Not synced to {Backend}."; plain reason; real exit; keep a muted save word. /impeccable clarify
- [P1] Keyboard trap: Tab always indents in .editor-body (NoteEditor.svelte ~882-885), WCAG 2.1.2. Fix: indent only in lists/multi-line selections, or Esc-then-Tab leaves. /impeccable harden
- [P2] "Edited on another device" said twice; notice eats phone writing space. /impeccable distill
- [P2] Save live region chatters (Unsaved -> Saving... -> Sync pending -> Synced announced each burst; made persistent in step 7). Announce settled states only. /impeccable harden
- [P2] Notice band has three button styles; find bar on literals; Replace is a native checkbox (blue, 14px tall label). /impeccable polish

## Detector
3 findings: quote bar side-tab (false positive), FormatBar mask #000 (false positive: mask alpha), global focus ring 3px radius (true, low). Overlay clean at 1200/800/390 and staged state. Only desktop control under 28px: find "Replace" label (14px).
