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
the image). `replace.txt`, `tag-add*.txt` and `tag-remove.txt` type a
character, then edit from outside the editor: the character and the edit are
separate undo steps. Until 2026-09-27 these files recorded one Cmd+Z taking
back both — a harness artifact, not WebKit coalescing (see "Why full-suite
runs were flaky" below). Consecutive `js` steps with no key between them
share one undo step (two `addTag`s, a Replace then a Replace All): the
harness sends no event between them, whereas in the app each is its own
click or Enter. `lines.txt` records a WebKit behaviour, not a Jodd one: one
Cmd+Z removes a whole run of typing, Enters included. `mac.txt` checks that on
macOS Ctrl+A/E/B keep their system text bindings and Ctrl+Cmd+Z does not undo.
`render-undo.txt` (2026-09-27) is a full body render — a note switch, or an
external change applied while the editor is idle. `innerHTML =` leaves WebKit's
undo history pointing into the same editor root, so Cmd+Z after it put a line
deleted in note A into note B; NoteEditor now mounts a fresh element per render
(`{#key editorRenderKey}`), and `page.sh`'s `window.render` mirrors that.

`paste-*.txt` and `type-then-paste-plain.txt` (2026-09-30) drive a real Cmd+V:
`clip <text>` / `cliphtml <html>` put the text (a URL copied from an address bar
has no HTML) on the system pasteboard first — the harness borrows the general
pasteboard and restores your text when it exits. Measured before the fix: two
plain-text pastes, or typing then pasting, undid in ONE Cmd+Z, because
`execCommand('insertText')` joins the typing command still open; as HTML each
paste was its own step. `NoteEditor` now pastes text-only clipboards through
`insertHTML` (`src/lib/editor/pastePlainText.ts`). `page.sh` uses the real
module, so these scenarios pin it. Inside `<pre>`/`<code>` paste is still
`insertText` and still merges — not covered here.

## Ways to get a wrong answer from it

- **Running it while the display sleeps or the screen is locked.** WebKit then
  treats the harness window as hidden and stops running
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
- **Running two harnesses at once** — two sessions, or a loop beside
  `run.sh`. Their windows overlap, and in 2 of 3 measured trials one of them
  never got a `requestAnimationFrame`, so no markdown trigger would fire.
  `harness.swift` now stops with a message naming this (and a sleeping
  display) instead of hanging for 60 s or recording it.
- **Believing a run that was clean minutes after a flaky one proves
  anything.** The flakes below depended on where the cursor happened to be
  and on system load; 75 traced runs of the five known-flaky scenarios with
  nobody at the machine produced zero failures.

## Why full-suite runs were flaky (fixed 2026-09-27)

Symptom: a few scenarios per full run (`exit`, `quote-exit-last-line`,
`replace-after-dotted-capital-i`, `tag-add`, `tag-remove`, differing each
time) showed one undo step split in two, an intermediate DOM state appearing
at `cmd+z #2` / `redo #1`; each passed 5/5 alone.

The mechanism, traced with `HARNESS_TRACE` (below): WebKit runs
`execCommand` in the WebContent process and registers each undo step with
the UI process's NSUndoManager **over IPC, asynchronously**. NSUndoManager
(`groupsByEvent`) keeps its group open until `NSApp.run` dequeues the **next
event of any kind** — idle time does not close it (a group stayed open 300 ms
until the next key). So which registrations share an undo step is decided by
where events land in the IPC stream. The harness let three kinds of event
land at random:

1. **keyUp, queued straight behind keyDown.** It was dequeued ~1-2 ms
   *before* the keystroke's registrations arrived (every trace), so the whole
   burst normally formed one group ending at the *next* key. When a
   registration beat the keyUp, the keyUp cut the burst — e.g. between
   `continueChecklist`'s `insertParagraph` and `insertHTML`, which is exactly
   the `exit.txt` flake (an extra `<input type="checkbox"><br>` row).
   Reproduced on demand: with each `execCommand` slowed by 30 ms and the
   keyUp posted 15 ms after keyDown, 3/3 runs gave that diff. Now `press()`
   posts keyUp only after the page has handled the key and run one animation
   frame (the markdown triggers apply in rAF) — as a real key is held ~100 ms.
   Same slowed page: 3/3 match.
2. **Real `mouseMoved`.** The "off-screen" window at (-3000, -3000) was put
   mid-screen by AppKit (measured frame 360,328 700×532). A WKWebView receives
   `mouseMoved` at 60 Hz while the cursor crosses it, even in this inactive
   accessory app — a trace caught one ending a group halfway through a single
   `replaceAll`. Swallowing the event in a local monitor does not help; the
   group still ends (measured). The window is now `ignoresMouseEvents`, placed
   at the screen's corner. Not verified with a moving cursor: this process
   may not post HID events, so the check was never run.
3. **The window's own startup events.** Twelve `appKitDefined` events every
   run, in two bursts (subtypes 1, 22, 22, 23, 23, 4 … 22, 23, 22, 23, 23, 4),
   the second ~15-120 ms before the first step. A traced `tag-add` failure had
   it arrive 111 ms after typing `x`, closing that step before the tags were
   added. Steps now start after the second windowMoved plus 300 ms of quiet
   (3 s cap).

After the startup events, any event that ends an open undo group is printed
as `STRAY EVENT …` into the output, so a disturbed run fails and names the
cause rather than passing or failing at random.

Verified 2026-09-28: six consecutive full runs 44/44, no stray events. A
run during which the display went to sleep stopped with the rAF message
instead of recording anything, as it should.

Consequence for `expected/`: (1) also changed what the *usual* outcome was.
A typed character's registration arrived after its keyUp, so its step stayed
open and absorbed whatever `js` edit came next — which is why six files
recorded Replace / addTag / removeTag undoing together with the typing
before it. With keyUp after the registrations (as in the app, where the user
must click the find bar or tag field in between), they are separate steps;
those six files were re-recorded, and every changed line is that split, at
`cmd+z #1` and `redo #1`. `bodyEdits.ts`'s comment that these edits "join
the undo step of whatever the user typed just before" was measured with the
old harness and inherits the artifact.

Also: AppKit logs to stderr at random (`NSSpellServer … timed out`), and a
re-record once wrote that line into `expected/mac.txt`. `run.sh` now prints
such lines as `note` and leaves them out of the comparison.

Tracing one scenario:

```bash
sh scripts/webkit-editor-harness/page.sh /tmp/h && swiftc -O scripts/webkit-editor-harness/harness.swift -o /tmp/h/harness
HARNESS_TRACE=/tmp/h/trace.log /tmp/h/harness /tmp/h/page.html scripts/webkit-editor-harness/keys/exit.txt
```

Each line is `<ms> [<step>] key down|up <code>`, `undo group OPEN|CLOSE`,
`undid`/`redid`, or `other event type=<NSEvent.EventType> [subtype=…]`.
