---
target: "the note editor (after #296 caret fix)"
total_score: 28
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 1
target_identity: "file:/home/user/Jodd/src/lib/components/NoteEditor.svelte"
target_fingerprint: "sha256:0b1e1734812e78c167d527039f9dc05352cac748c2002b5bb7b7cb01ad603cc3"
target_path: /home/user/Jodd/src/lib/components/NoteEditor.svelte
timestamp: 2026-11-08T02-28-47Z
slug: src-lib-components-noteeditor-svelte
---
# Critique: the note editor after the caret fix (#296), main @ 42db7ab

Method: two independent design reviewers (R1, R2), averaged, plus one detector run (B). All three used the fixed list of states, plus probes for: Keep and Discard, typing after them and checking the caret line; typing bursts in a refused note; Try again. Widths 1200, 800, 390 and 350; light and dark; the phone judged from code. The reviewers saw neither the detector nor earlier snapshots.

## Heuristics: 28/40 (R1 28, R2 28; previous run 28.5)
| # | Heuristic | R1 | R2 | Avg |
|---|---|---|---|---|
| 1 | Visibility of system status | 3 | 3 | 3 |
| 2 | Match with the real world | 3 | 3 | 3 |
| 3 | User control and freedom | 2 | 2 | 2 |
| 4 | Consistency and standards | 3 | 3 | 3 |
| 5 | Error prevention | 3 | 3 | 3 |
| 6 | Recognition rather than recall | 2 | 3 | 2.5 |
| 7 | Flexibility and efficiency | 3 | 3 | 3 |
| 8 | Aesthetic and minimalist design | 3 | 3 | 3 |
| 9 | Help users recover from errors | 3 | 3 | 3 |
| 10 | Help and documentation | 3 | 2 | 2.5 |

## Confirmed fixed by #296 (all three passes)
- Keep my edits returns the caret to its line (60 of 80) and holds the scroll.
- Discard does the same when nothing was typed after the notice.
- The refused-push notice still holds still through typing bursts. Try again keeps its height and returns focus to the body.
- Detector: the same 3 findings (2 false positives; the 3px focus radius is minor). Overlay clean on 24 pages, 0 console errors, no overflow.

## Priority issues (verified against code where marked)
- [P1, verified] "Discard my edits" does nothing if the user typed after "Edited on another device" appeared (R1, R2, both rendered).
  - Cause: onInput writes the live body into $selectedNote on every keystroke (NoteEditor.svelte:1693), overwriting the held-back remote body. reloadFromRemote (2427) only bumps externalReloadNonce, so it re-renders the user's own text.
  - It also clears the pending draft (added in #295), so the save word wrongly reads "Sync pending".
  - The discard path predates this pass; the false status word is from #295.
  - Fix: keep the held-back remote body apart from $selectedNote (or re-read the row) inside reloadFromRemote; clear the draft only after the replacement renders. Add a test: type after the banner, then Discard. -> harden
- [P2] Discard after unrelated remote text: the caret is mapped by character offset into different text (line 72, off-screen in B).
- [P2] The collision question expires about 1.5s after typing stops, and the user decides blind: no view of the other version (R1, R2). Product decision: report instead of ask; offer Compare.
- [P2] Copy to a new note drops focus to body, switches notes silently, awaits IPC before the UI changes, and is offered where the copy may not sync (R1, R2; the "may not sync" part was refuted earlier: Outlook creates in Notes by its well-known name).
- [P2] After Move to Notes the retrying notice repeats "Move the note to Notes" and stacks with the context notice (R1, R2).
- [P2] Narrow meta line: the date keeps its width while the chip shows "clau…". The chip's name lacks "unreviewed" (R1, R2).
- [P2] The conflict-copy title wraps at 390px or less and moves the caret 33px (R1, R2, B).

## Minor
- 11–14 tab stops before the title; the popover takes no focus; notices are role=status mounted already populated.
- "Unsaved" flashes on every pause.
- While refused and typing, the lead says "saved" about 1.5s before the autosave.
- A locked note keeps a toolbar holding only ⋯.
- The phone header word unmounts and has no accessible name.
- Silent splice of text appended to an unwrapped last line.

## Strengths
- One Voice is real in the pixels.
- The caret holds when notices arrive and after Keep.
- Refusal copy is candid and specific to its class.
