---
target: "the note editor (after #292 status system)"
total_score: 28
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 3
target_identity: "file:/home/user/Jodd/src/lib/components/NoteEditor.svelte"
target_fingerprint: "sha256:d25c600d110ede28738ef6dba0e0acd76362a0ad678f8899f9d273552b042c9b"
target_path: /home/user/Jodd/src/lib/components/NoteEditor.svelte
timestamp: 2026-11-07T18-21-00Z
slug: src-lib-components-noteeditor-svelte
---
# Critique: the note editor after the status system (#292), main @ afbe538

Method: two independent design reviewers (R1, R2), averaged, plus one detector run (B). All three used one fixed list of states: rest, typing, edited elsewhere (also in an 80-line note), conflict copy, refused push, read-only lock, agent change, the 350px editor, and the phone (judged from code). Widths 1200, 800, 390 and 350; light and dark. The reviewers did not see the detector's output or earlier snapshots.

## Heuristics: 28/40 (average of R1 29 and R2 27; previous run 28)
| # | Heuristic | R1 | R2 | Avg |
|---|---|---|---|---|
| 1 | Visibility of system status | 3 | 3 | 3 |
| 2 | Match with the real world | 3 | 3 | 3 |
| 3 | User control and freedom | 3 | 3 | 3 |
| 4 | Consistency and standards | 3 | 2 | 2.5 |
| 5 | Error prevention | 3 | 3 | 3 |
| 6 | Recognition rather than recall | 3 | 2 | 2.5 |
| 7 | Flexibility and efficiency | 3 | 3 | 3 |
| 8 | Aesthetic and minimalist design | 3 | 3 | 3 |
| 9 | Help users recover from errors | 2 | 3 | 2.5 |
| 10 | Help and documentation | 3 | 2 | 2.5 |

## Confirmed fixed by #292 (all three passes agree)
- A refused push is said once: one Try again, no chip; the save word is held invisible, so the formatting row does not reflow.
- Refusals are in plain words, with the backend's text behind Details.
- The caret line holds still when a notice arrives in a scrollable note (measured 0px; 52/124px without the fix).
- The save word is no longer announced as a live region.
- The detector found no new findings: the blockquote side-tab and the mask #000 are false positives, and the 3px focus-ring radius is real but minor. The overlay was clean on 24 pages, with 0 console errors and no overflow.

## Priority issues (verified against code)
- [P1] The external-change notice promises "Changes on different lines are merged in", but the editor merges only when the other device ADDED lines (NoteEditor.svelte:1989, merge.rs). An edit to a different line becomes a conflict copy. Fix: make the sentence exactly true, or widen the merge. (R2) -> clarify
- [P1] Refused-push exits can fail. "Copy to a new note" saves into the same folder (noteActions.ts:195), so folder-class refusals refuse the copy too. The remedy in the sentence ("Move the note to Notes") is not offered as a button, and the sentence ignores which folder was named. Fix: each refusal class owns its actions (Move to…, copy to a folder that syncs). (R1, R2) -> harden
- [P1] A remote-locked note strands the edit that caused the lock. "Read-only in Jodd… Edit it in Apple Notes" has no actions, and the edit is not in Apple Notes. Fix: lead with "Your last edit is saved on this device only" and offer Copy to a new note. (R1; R2 agrees) -> clarify
- [P2] Text still moves:
  - the keep-both rename wraps the title (+33px at 350px), and the title sits outside keepCaretStill;
  - the refused notice vanishes on the first keystroke (-70px in a short note);
  - at 390x500 the notice takes 36% of the writing area. (R1, R2, B) -> adapt
- [P2] Announcements: each notice mounts as role=status with its text already in it, which many screen readers do not announce. The context notice is announced but is not in the table. The trust chip's name omits "unreviewed", and its dialog popover does not take focus. The toolbar has 11 tab stops and no roving tabindex. (R1, R2) -> harden
- [P2] Consistency:
  - read-only has two looks (a red lock notice vs a muted account line);
  - the notice buttons use two styles;
  - the save word is 11px vs 12px;
  - the copy title is "(conflict copy 2026-11-07 18:12)", jargon with an ISO date, and it syncs to Apple Notes. (R1, R2) -> polish
- [P2] On a narrow meta line, the date keeps 119px while the folder shrinks to "…o" and the trust chip to "clau…". (R1, R2) -> adapt

## Minor
- Dead CSS: .save-status:not(.external).
- Stale comments: editorSaveState.ts, EditorMetaLine.svelte header, NoteEditor phone-header note.
- The read-only banner repeats its text in its title.
- A read-only note keeps a 45px toolbar holding only ⋯.
- "Unsaved" flashes on every pause.
- "Refetch from the server".
- The remote-lock Details repeat the sentence.

## Strengths
- One voice per state, enforced by a table and tests.
- Reassurance-led notices; the Touched Amber rule is consistent in light and dark.
- Refusal copy that names the backend and the next step.
- Measured contrast is at least 6.1:1.
