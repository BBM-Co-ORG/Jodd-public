---
target: "the note editor (after #291 fixes)"
total_score: 28
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 2
target_identity: "file:/home/user/Jodd/src/lib/components/NoteEditor.svelte"
target_fingerprint: "sha256:965e98ce1f1ec6b825f764a383d71b3e3e458532de7f52a75d56531e9d89caf7"
target_path: /home/user/Jodd/src/lib/components/NoteEditor.svelte
timestamp: 2026-11-07T17-38-08Z
slug: src-lib-components-noteeditor-svelte
---
# Critique: the note editor (after #291), main @ efbe461

Method: two independent passes. A was a design review in Playwright at 1200/800/390/350, light and dark, with five staged states (rest, external edit, keep-both, refused push, trust chip). B was the detector CLI on 8 files, plus the live overlay over the same states. A never saw B's output. The phone header, FormatBar, Android touch sizing and the phone sheets were judged from code only.

## Heuristics: 28/40 (Good, the bottom of the band; previous run 29)
| # | Heuristic | Score |
|---|---|---|
| 1 | Visibility of system status | 3 |
| 2 | Match with the real world | 3 |
| 3 | User control and freedom | 3 |
| 4 | Consistency and standards | 2 |
| 5 | Error prevention | 3 |
| 6 | Recognition rather than recall | 3 |
| 7 | Flexibility and efficiency | 3 |
| 8 | Aesthetic and minimalist design | 3 |
| 9 | Help users recover from errors | 2 |
| 10 | Help and documentation | 3 |

## Detector
CLI: 3 findings.
- The blockquote `side-tab` is a false positive: DESIGN.md sanctions the quote bar.
- The FormatBar mask `#000` is a false positive: a mask's alpha is never painted.
- The App.svelte `:focus-visible` `border-radius: 3px` is real: it is off the 4/6/10 scale.

Overlay: clean in all 9 states. Console: 0 errors. No page overflow at any width. Every desktop control is at least 28px, except the bare Replace checkbox (13px; its label is 28px).

## Fixed since the last run (verified)
- The find bar wraps cleanly at 350px.
- The Tab trap has an exit (Esc then Tab) and a hint.
- "Edited on another device" is said once.
- A title match is highlighted.
- A refused push has a visible notice with an exit ("Save as a copy").

## Priority issues
- [P1] A refused push is said three times, with two Try again buttons: the notSynced/readonly chip, its popover, and the blocked notice (introduced by #291 adding the notice without demoting the chip). Fix: while blockedNotice shows, the chip is a plain label with no action and no popover. Update the stale comments (noteStatus.ts:110, NoteEditor.svelte:4152, App.svelte:1486). -> distill
- [P1] Raw transport reasons are the user-facing explanation ("no Exchange folder id for 'Notes'"); blockedReasonText only strips "permanent:". Fix: map refusal classes to plain sentences that each carry a next step, put the raw text behind Details, and fall back to "The server refused this change." -> clarify
- [P2] Notices above the title shift the text under the caret: the external notice is 66/120/156px tall at 800/390/350 and appears mid-typing. At 350px the buttons squeeze the reason into a three-word column. "Save as a copy" collides with "Saved as a separate copy". -> layout
- [P2] At a narrow editor only B I U S stay visible (overflow takes from the right, not by priority). The blocked state unmounts the 7em save word, so the toolbar reflows. The meta date never shrinks while the trust chip truncates to "cla…". -> adapt
- [P2] Screen reader and keyboard: the role=status save word announces every autosave cycle and is unmounted when blocked; the title has no label; the toolbar has no role=toolbar or roving tabindex (the title is the 19th tab stop in the blocked state). -> harden

## Minor
- Stale comments.
- FormatBar's 20px glyph is an eighth size.
- Some controls use glyphs as icons (‹ › → ← !).
- Amber is used on hover for .conn-link, .ctx-suggestion-btn and .meta-tags.
- SourceList repeats "Sources (N)" and Retry.
- The save word is 11px on desktop and 12px on the phone.
- About 102 literal px values remain in NoteEditor.
- .conn-link is about 22px with no touch override.
- aria-label on a plain div in SourceList.
- The conflict copy's title uses an ISO timestamp.
- The .has-error trust chip turns wholly red.

## Strengths
- Touched Amber is applied with discipline and the notice is said once.
- One honest save vocabulary is shared by the toolbar and the phone header.
- Measured contrast is 6.0–15.8:1; desktop hit areas are at least 28px; Thai titles render at line height 1.5.
