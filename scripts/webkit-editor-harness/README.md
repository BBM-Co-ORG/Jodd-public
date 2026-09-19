# WebKit editor harness

Measures what Jodd's note editor actually does in **WebKit** — the engine it
runs on macOS — under genuine keyboard input: after every keystroke, Cmd+Z and
Cmd+Shift+Z it records the editor's HTML and caret, and compares the run with
`expected/`.

```bash
sh scripts/webkit-editor-harness/run.sh            # check (macOS only)
sh scripts/webkit-editor-harness/run.sh --update   # re-record expected/
```

It is **not** in CI (CI is Linux, and jsdom has no `execCommand`, no
`Selection.modify` and no undo history). Run it by hand after touching
anything in `src/lib/editor/` (`markdownTriggers.ts`, `shortcutKeys.ts`,
`exitSpecialBlock.ts`, `backspaceMergeEmpty.ts`, `bodyEdits.ts`), or the
editor's keydown/input handlers in `NoteEditor.svelte`, or its tag,
find/replace and link-picker edits — and when you change those, update the
copy of them in `page.sh`.

## Why it exists

Built on 2026-09-14 to find out why undo/redo "felt like it repeated". It
showed that the markdown triggers and Enter-on-checklist edited the DOM
directly (`deleteContents`, `replaceChild`, `after`), which WebKit's undo
history — a list of edit commands, not DOM snapshots — never saw. Undo and
redo then replayed commands against a document that no longer existed:

- `- item`, undo, redo → `<ul><li>item</li></ul>- ` (the list **and** the
  literal marker it had replaced), and "item" lost on a later redo;
- `# Title`, undo → the heading could never be undone;
- checklist A↵B, undo → an empty checkbox row that no undo removed.

The same day, a second pass converted the edit paths that still bypassed the
history. Measured before conversion:

- Replace / Replace All (`tn.data =`, `deleteContents` + `insertNode`) → Cmd+Z
  did nothing afterwards, **not even for text typed before the replace**;
- a tag added from the tag field (`appendChild`) → never undoable, and the
  typing on either side of it merged into one undo step;
- Enter on the empty last line of a quote → redo re-inserted the quote
  *below* the paragraph that replaced it;
- Backspace into an empty line above a heading, and Enter at the start of the
  first block → neither could be undone;
- the checklist button → `<div><div><input>` on an empty line, a checkbox
  mid-line otherwise.

`keys/` replays those scenarios against the fixed code; `expected/` is what
they produce now. Three record WebKit, not Jodd, on purpose:
`first-block-enter*.txt` run no Jodd code — they watch for the Enter misfire
`splitFirstBlock.ts` worked around until 2026-09-14, which no longer
reproduces; `backspace-image-line.txt` is native Backspace demoting a heading
into an image line, which Jodd now declines to handle (the old helper deleted
the image). `replace.txt` and `tag-remove.txt` show one Cmd+Z taking back the
typing just before the edit too — coherent, and a known cost (see
`bodyEdits.ts`). `lines.txt` records a WebKit behaviour, not a Jodd one: one
Cmd+Z removes a whole run of typing, Enters included. `mac.txt` checks that on
macOS Ctrl+A/E/B keep their system text bindings and Ctrl+Cmd+Z does not undo.

## Ways to get a wrong answer from it

- **Running it while the display sleeps or the screen is locked.** WebKit then
  treats the off-screen harness window as hidden and stops running
  `requestAnimationFrame`, where the markdown triggers apply — `- item` stays
  literal text in every scenario. Measured on 2026-09-14 after ~18 idle
  minutes; `--update` recorded it over four correct files before anyone
  noticed. `run.sh` now probes rAF first and refuses to run without it.

- **Driving it with JS `execCommand` instead of key events.** Without real
  events NSUndoManager never closes a group, so every edit undoes in one step.
  That is how the first version of this harness "found" coarse undo that was
  partly its own artifact. `harness.swift` queues NSEvents for this reason.
- **Trusting it for something `page.sh` does not wire.** It runs the real
  modules but a copy of NoteEditor's handlers — only what that copy does is
  measured.
- **Probing a command sequence at one caret position.** An empty
  `insertHTML` looked like a harmless way to close the typing undo step when
  probed at the start of a block; everywhere else it splits the block, which
  only `replace.txt`, `tag-remove.txt` and `link-pick.txt` caught.
