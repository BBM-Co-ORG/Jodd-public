---
target: "the note editor (after #293 promises fix)"
total_score: 28
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 2
target_identity: "file:/home/user/Jodd/src/lib/components/NoteEditor.svelte"
target_fingerprint: "sha256:3a16fd6d1165d6b6af47d66dd8cb1e667bc82b9b5fd18c072df3eb08c227e5de"
target_path: /home/user/Jodd/src/lib/components/NoteEditor.svelte
timestamp: 2026-11-07T19-26-53Z
slug: src-lib-components-noteeditor-svelte
---
# Critique: the note editor after the promises fix (#293), main @ c2cad7d

Method: two independent design reviewers (R1, R2), averaged, and one detector run (B), all on the same fixed list of states: rest, typing, edited elsewhere (also in an 80-line note), conflict copy, refused push in Notes and in a subfolder, read-only lock, agent change, the 350px editor, and the phone (judged from code). Widths 1200, 800, 390 and 350; light and dark. The reviewers did not see the detector's output or earlier snapshots.

## Heuristics: 28/40 (R1 28, R2 28; previous run 28)
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
| 10 | Help and documentation | 2 | 2 | 2 |

The two reviewers now agree heading by heading. Error recovery rose to 3 in both (it was 2/3 before). Control and freedom fell to 2 in both, because of the autosave race below.

## Confirmed fixed by #293 (all three passes)
- The merge sentence matches the code: it says only added lines are merged.
- Refusals offer only the buttons that can work. A refused subfolder shows Move to Notes only; the move clears the notice and relabels the note (B checked this with the move allowed).
- A locked note leads with "Your last edit is saved on this device only" and offers a copy.
- Detector: the same 3 findings as before (2 false positives; the 3px focus radius is real and minor). Overlay clean on 28 pages, 0 console errors, no overflow.

## Refuted
- "Copy to a new note may not sync for a note in Notes" (R1, R2; both from code reading): Outlook creates in Notes by its well-known name (microsoft/wire.rs:3863 create_destination), so the copy syncs. The sentence "Outlook can't reach its Notes folder yet" is still imprecise; that refusal comes from moving a note into Notes.

## Priority issues (both reviewers unless marked)
- [P1] The autosave race. "Edited on another device" lives about 1.5–1.9s while typing. Then the autosave merges or forks. The "Use other version" confirm is pulled away about 1.3s after it opens, and focus drops to body (NoteEditor.svelte:2340). Fix: pause the autosave while the confirm is open or focus is in the notice; or, while typing, show only the outcome. -> harden
- [P1] The refusal notice unmounts on the first keystroke (editPending, about NoteEditor.svelte:2493) and comes back after the push. In a short note the text jumps 66–124px each time. The save word then says "Sync pending" for a push that will be refused again. Fix: keep the notice mounted and change its lead in place ("Trying again with your latest edit…"); remove it only on success. -> harden
- [P2] The conflict copy retitles the note under the caret with "(conflict copy 2026-11-07 19:15)". The title wraps at 390px or less and moves the caret line 33px. keepCaretStill watches only the notice stack. Fix: observe the title too; name the copy in human terms. -> polish
- [P2] Screen-reader and focus plumbing:
  - Try again and Mark reviewed drop focus to body;
  - notices are role=status nodes inserted already full of content, which announce unreliably on WebKit;
  - the trust chip's name lacks "not reviewed";
  - the desktop toolbar has 11 tab stops and no role=toolbar. -> harden
- [P2] Narrow meta line: the date keeps 119px while the folder shrinks to "…o" and the chip to "clau…". The notice stack can reach 188px with three button styles. A folder suggestion can contradict Move to Notes (R2). -> adapt
- [P2] The lock notice ends with "Edit it in Apple Notes" after saying the edit is only here, which reads as two next steps (R1). -> clarify
- [P3] A failed Move to Notes shows no message inside the notice; the error goes only to the app error bar (B).

## Minor
- Stale comments: editorSaveState.ts:31 and NoteEditor.svelte:4102.
- "Unsaved" shows for about 1.5s after each pause.
- A locked note keeps a 45px toolbar holding only ⋯.
- "Refetch from the server".
- Two names for one action: "Duplicate" in ⋯, "Copy to a new note" in the notice.
- duplicateNote awaits IPC before the UI changes and fails with alert().
- Silent merges of appended text (R2).

## Strengths
- One voice per state, driven by a table, with buttons per refusal class.
- The save word's slot never reflows.
- The caret holds still in scrollable notes.
- Colour has one meaning per state.
