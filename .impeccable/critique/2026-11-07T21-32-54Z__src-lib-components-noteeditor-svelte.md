---
target: "the note editor (after #294 timing fixes)"
total_score: 28.5
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 3
target_identity: "file:/home/user/Jodd/src/lib/components/NoteEditor.svelte"
target_fingerprint: "sha256:d85412e8c8f21412b24950fb6cd13c6f4c4f65aaad5d17ec2eda7f52b0fa23ed"
target_path: /home/user/Jodd/src/lib/components/NoteEditor.svelte
timestamp: 2026-11-07T21-32-54Z
slug: src-lib-components-noteeditor-svelte
---
# Critique: the note editor after the timing fixes (#294), main @ 8756119

Method: two independent design reviewers (R1, R2), averaged, plus one detector run (B). All used one fixed list of states plus two timing probes: typing in a refused note, and waiting 3–5s before answering "Use other version". Widths 1200, 800, 390 and 350; light and dark; phone judged from code. The reviewers saw neither the detector nor earlier snapshots.

## Heuristics: 28.5/40 (R1 29, R2 28; previous run 28)
| # | Heuristic | R1 | R2 | Avg |
|---|---|---|---|---|
| 1 | Visibility of system status | 3 | 3 | 3 |
| 2 | Match with the real world | 3 | 3 | 3 |
| 3 | User control and freedom | 3 | 2 | 2.5 |
| 4 | Consistency and standards | 3 | 3 | 3 |
| 5 | Error prevention | 3 | 3 | 3 |
| 6 | Recognition rather than recall | 3 | 3 | 3 |
| 7 | Flexibility and efficiency | 3 | 3 | 3 |
| 8 | Aesthetic and minimalist design | 3 | 3 | 3 |
| 9 | Help users recover from errors | 3 | 3 | 3 |
| 10 | Help and documentation | 2 | 2 | 2 |

Control and freedom rose in R1 (2 to 3). R2 kept it at 2: the question still answers itself about 1.5s after typing stops if the user is only reading.

## Confirmed fixed by #294 (all three passes)
- The "Use other version" dialog stays open while the user thinks: 4.8–5s sampled, autosave waiting. Keep my edits returns focus to the text.
- Typing in a refused note: the notice stays mounted and the body never moved at 1200 or 390 (B: about 242 samples). Words change in place.
- The caret holds when a notice arrives in a long note, at every width.
- Detector: the same 3 findings (2 false positives; the 3px focus radius is minor). Overlay clean on 24 pages, 0 console errors, no overflow.

## Refuted again
- "Copy to a new note can't sync for a note in Notes" (R1, R2; code reading only): Outlook creates in Notes by its well-known name (microsoft/wire.rs:3863).

## Priority issues (verified against code)
- [P1] Discard my edits:
  - focus drops to body, because confirmUseOtherVersion never returns it and reloadFromRemote blurs (NoteEditor.svelte:2387, 2410);
  - the save word says "Unsaved" indefinitely, because reloadFromRemote leaves the discarded draft in pendingLocalEdits. (R1, R2) -> harden
- [P1] Typing in a refused note still churns:
  - the lead swaps red and ink, and the buttons disable and re-enable on every pause;
  - disabled buttons have no :disabled style;
  - the save word flashes "Unsaved";
  - at 350px the longer retry lead wraps (120 to 138px), so the body moves 18px twice per burst (R1, R2; B saw 390 stable).
  Fix: keep the lead the same length; blocked takes precedence over unsaved in the save word; style disabled buttons. -> harden
- [P1/P2] "Edited on another device" still resolves about 1.5s after typing stops when the user is only reading (no focus, no dialog): a time-limited question. Fix: hold the resolve for a minimum reading window, or until the next keystroke or blur. (R2) -> harden
- [P2] The conflict copy renames the title under the caret, "(conflict copy 2026-11-07 21:20)". At 390px or less it wraps mid-date and the caret moves +33px; keepCaretStill does not watch the title. (R1, R2, B) -> layout
- [P2] Narrow meta line: the date (flex:none) beats the folder ("…o") and the trust chip ("clau…"). The chip's name lacks "unreviewed". (R1, R2) -> adapt
- [P3] A refusal on the row before persistence can read "Trying again" for up to 2.5s. Seen only with fixture staging order (B, R1); unproven in the app.

## Minor
- Notice actions are not mirrored in the ⋯ menu.
- The desktop toolbar has 12 tab stops and no role=toolbar.
- The status popover is role=dialog without moving focus into it.
- The trust popover shows "Created by claude-code/2.1.0".
- "Add tag" vs "Add tags".
- A locked note keeps a toolbar holding only ⋯.
- The phone header unmounts the save word instead of holding it.
- Silent merge of text appended to the last line.

## Strengths
- One voice per state, real in the pixels: the save word slot is fixed, and there is one status node.
- Keep-both is well choreographed now.
- Refusal copy is specific to its class.
