---
target: "the note editor (after #295 Discard and churn fix)"
total_score: 28.5
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 2
target_identity: "file:/home/user/Jodd/src/lib/components/NoteEditor.svelte"
target_fingerprint: "sha256:0fef9e9427e1f7c00d758ec66f27609e4766486e64f6591078b1a910eaa91c68"
target_path: /home/user/Jodd/src/lib/components/NoteEditor.svelte
timestamp: 2026-11-07T22-20-05Z
slug: src-lib-components-noteeditor-svelte
---
# Critique: the note editor after the Discard/churn fix (#295), main @ 709a852

Method: two independent design reviewers (R1, R2), averaged, plus one detector run (B). All three used the fixed list of states, plus probes for: Keep and Discard after waiting; typing bursts in a refused note; Try again. Widths 1200, 800, 390 and 350; light and dark; the phone judged from code. The reviewers saw neither the detector output nor earlier snapshots.

## Heuristics: 28.5/40 (R1 28, R2 29; previous run 28.5)
| # | Heuristic | R1 | R2 | Avg |
|---|---|---|---|---|
| 1 | Visibility of system status | 3 | 3 | 3 |
| 2 | Match with the real world | 3 | 3 | 3 |
| 3 | User control and freedom | 2 | 2 | 2 |
| 4 | Consistency and standards | 3 | 3 | 3 |
| 5 | Error prevention | 3 | 3 | 3 |
| 6 | Recognition rather than recall | 3 | 3 | 3 |
| 7 | Flexibility and efficiency | 3 | 3 | 3 |
| 8 | Aesthetic and minimalist design | 3 | 3 | 3 |
| 9 | Help users recover from errors | 3 | 3 | 3 |
| 10 | Help and documentation | 2 | 3 | 2.5 |

## Confirmed fixed by #295 (all three passes)
- Typing in a refused note: nothing moves or changes at any width (text, height, buttons, held word). Try again shows the short lead at the same height.
- Discard returns focus to the editor; the save word no longer reads "Unsaved".
- Detector: the same 3 findings (2 false positives; the 3px focus radius is minor). Overlay clean on 24 pages, 0 console errors.

## Priority issues
- [P1, regression from #294] Keep my edits and Discard my edits put the caret on line 1 and scroll to the top (R1, R2, rendered in both). returnFocusToText calls editorEl.focus() without restoring the selection the dialog took (NoteEditor.svelte:2372). Fix: save the caret (as offsets) and scrollTop when the notice or dialog takes focus; restore them with focus({preventScroll:true}). -> harden
- [P1/P2] The collision question expires: "Use other version" is up about 0.5–1.5s after typing stops, then the autosave forks and renames the title (R1, R2). Reading does not hold it; only focus or the dialog does. Open product question: report the outcome instead of asking? -> harden or clarify
- [P2] The fork renames the title under the caret: "(conflict copy …)" vs the notice's "separate copy". At 390px or less it wraps and the caret moves 33px; keepCaretStill does not watch the title (R1, R2, B).
- [P2] Move to Notes reuses the retry lead with the old "Move the note to Notes" sentence, and adds a context notice. A successful retry or move ends silently, with nothing announced (R2).
- [P2] Narrow meta line: the date keeps its width while the folder shrinks to "…o" and the chip to "clau…" (R1, R2).
- [P2] Screen reader: notices are role=status nodes inserted already populated; the trust chip's name lacks "unreviewed"; the popover takes no focus; 14 tab stops before the title (R1, R2).
- [unconfirmed] After Keep, the save word stayed "Unsaved" in B's staging (userEdited after saveAsConflictCopy). Possibly a fixture artifact.

## Minor
- Copy to a new note doesn't return focus.
- A failed duplicate shows alert().
- Stale comment in editorSaveState.ts.
- A locked note keeps a toolbar holding only ⋯.
- The phone header unmounts the save word.
- Silent merge of text appended to the last line (unwrapped bodies).

## Strengths
- The refused-push notice holds still word for word.
- The caret holds when a notice arrives.
- One Voice discipline is real; the save word slot never reflows.
- Refusal actions are specific to their class.
