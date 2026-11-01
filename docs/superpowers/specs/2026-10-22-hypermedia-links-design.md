# Hypermedia links in the note editor — design

Date: 2026-10-22. Status: for approval. Scope: the note editor
(`NoteEditor.svelte`) on desktop and Android, and the Rust link extractors it
must agree with.

Decided by the user: links are URIs routed by scheme; web opens in the OS
browser; a click opens a link chip; phase 1 adds no markup to the stored
note; the link rules are fixed on both sides with a one-time rebuild; other
apps' schemes open only from a named allowlist. Calls marked *my call* are
the author's and open to change.

## Why

A note should read like a web page: anything that points somewhere looks like
a link, and following it works the same way whatever it points at. Today it
does not:

| In the note | What a click does today |
|---|---|
| `<a href="https://…">` (Share to Jodd, the Link button) | Places the caret. ⌘/Ctrl+click opens the browser (#188). On Android nothing opens it. |
| `<a href="#meeting-…">` (Action Items evidence) | A plain click scrolls to the passage (`followMeetingEvidence`). |
| `https://…` typed or pasted as plain text | Nothing. It is plain text. On iCloud this includes every bare URL Apple auto-linked: `format::normalize_spans` collapses a link whose target equals its own text to plain text (a link the user made with other text stays an `<a>`, `format_html.rs`), and notes read through the `text_to_html` fallback have no links at all. |
| `[[title-slug-a1b2c3d4]]` | Nothing. Plain text. Reachable only from the connections panel. |

The user's model, which this design adopts:

1. A link is a URI.
2. A wikilink is a URI too, with its own scheme.
3. Each scheme gets its own handler. A wikilink goes to a note (and
   optionally a place in it), web goes out, and other schemes deep-link to
   whatever renders them.
4. Web content opens in the **OS browser**, not an in-app browser (decided
   2026-10-22).

## Goals

- One router: every link, whatever its form in the note, resolves to a URI
  and goes to the handler for that URI.
- One set of gestures for every kind of link, on desktop and on touch.
- Plain-text URLs and `[[wikilinks]]` look like and act like links.
- Links work on Android.

## Non-goals

- An in-app web browser. The chip may later offer "Summarize into Jodd"
  through ingest, which is the useful part of reading a page inside Jodd.
- Rewriting plain-text URLs into `<a>` tags, or `[[…]]` into anything else.
- A page preview fetched from the network on hover.
- Cross-account wikilinks. A wikilink resolves inside the note's own account,
  as the `mentions` edges do today.

## Anti-goals (a failure even if everything else works)

- **The stored note changes because a link was drawn, hovered or clicked.**
  The body syncs to Apple Notes; any markup this feature adds would reach
  Apple. Opening a note, hovering, clicking links, opening and closing the
  chip, then leaving the note must not produce a save.
- **The chip takes focus from the note.** The selection is the caret
  (gotcha #34). Typing after a click must go into the note.
- **What looks clickable disagrees with what Jodd records.** A plain-text URL
  is clickable exactly when it is a `cites` edge, and a wikilink exactly when
  it is a `mentions` edge.

## The model

### Link sources

The scanner finds links in the editor body from three sources:

| Source | Found by | Already a DOM element? |
|---|---|---|
| `<a href>` | `closest('a')` (as `externalHref` does now) | yes |
| plain-text URL | text scan, same rules as Rust `db::extract_urls` | no |
| `[[…]]` | text scan, same rules as Rust `db::extract_wikilinks` | no |

Text inside an `<a>` is not scanned again: the anchor is the link.
`[[…]]` inside inline code or a code block is still a link, because Rust's
extractor counts it as an edge (*my call: agreeing with the graph beats
special-casing code*).

### URIs

Each link resolves to a `LinkTarget { kind, uri, label }`:

| Found | URI | kind |
|---|---|---|
| `[[slug-a1b2c3d4]]` | `jodd://note/a1b2c3d4` | `note` |
| `[[Plain title]]` (typed by hand, no id) | `jodd://note?title=<normalized title>` | `note` |
| `[[slug-a1b2c3d4#Budget]]` (phase 2) | `jodd://note/a1b2c3d4#Budget` | `note` |
| `href="#meeting-…"` | `#meeting-…` | `anchor` |
| `http:` / `https:` | as is | `web` |
| `mailto:` / `tel:` | as is | `os` |
| a scheme on the app allowlist (below) | as is | `os` — opened from the chip only, never by ⌘+click (*my call: another app gets one deliberate tap*) |
| any other scheme | as is | `inert` — drawn as a link; the chip shows the full URI and Copy, and nothing opens it |
| `javascript:`, `data:`, `file:`, relative | — | not a link |

The slug is resolved the way `db::parse_link_key` resolves it: a trailing
`-<8 hex>` is an id, anything else is a title.

### Handlers

`src/lib/links/handlers.ts` holds a registry. A handler declares:

- `matches(target)` — which targets it takes (by kind, scheme, host);
- `label` and an icon for the chip;
- `open(target)` — what following the link does;
- `actions(target)` — extra chip buttons.

Phase 1 handlers:

| Handler | Open | Chip actions |
|---|---|---|
| note | Resolve via a new command (below), then open the way `openConnection` does: switch account if needed, upsert into `$notes`, set `selectedNote`. On a phone, also go to the `note` pane. | Open, Copy text |
| anchor | Scroll to the element with that id (today's `followMeetingEvidence`) | Go to |
| web | `openUrl` from `@tauri-apps/plugin-opener` | Open in browser, Copy link; for an `<a>` also Edit link and Remove link (unwrap the anchor, keep the text). Edit link is new work: today's prompt (`formatLink`) always starts at `https://` and only creates links, so it needs an edit mode prefilled with the anchor's href and text that replaces that anchor. |
| os | `openUrl` | Open in <app>, Copy |
| inert | — | Copy |

### Which schemes may open — a named allowlist (decided 2026-10-22)

Note HTML arrives from Apple sync, Share to Jodd, URL ingest, AI workflows and
MCP agent writes, and some OS schemes (`ms-msdt:`, `search-ms:` on Windows)
have been exploit vectors. So Jodd opens a scheme only if it is named:

- **Always:** `http`, `https`, `mailto`, `tel` — exactly what
  `opener:default` in `src-tauri/capabilities/default.json` already allows
  (tauri-plugin-opener 2.5.4).
- **Named apps:** one constant list in `handlers.ts`, each entry a scheme
  and the app name the chip shows. **It ships empty** — the user has not
  named any apps yet. Adding one means adding it to that list **and**
  widening the opener capability for that scheme alone (an `opener:allow-open-url`
  scope entry, never a wildcard). The two must stay in step; a test reads the
  capability file and fails if they differ.
- `jodd:` is Jodd's own and never goes through the opener.

Edit link and Remove link change the note. That is fine: the user asked for
the change. The anti-goal is only about changes the user did not ask for.

Adding a resource type later (a YouTube handler with "Transcribe into this
note", a `jodd://` route) means adding one registry entry.

### Resolving a wikilink — new command

`resolve_note_link(account_id, key) -> Option<NotePreview>`. It is a pure
SQLite read, no remote (local-first doctrine). It reuses `parse_link_key`
and the same match that `Db::outgoing_links` uses (8-hex id prefix of the
uuid, or normalized title; skip `deleted_pending`). It returns what
`openConnection` needs (the frontend `Note`) plus the first line of the
body after the title for the chip. Several title matches: return the most
recently modified one (*my call; an id-slug never has this problem*).

No match: the chip says "No note called …" and offers only Copy text.
Creating the missing note is not in phase 1.

## Gestures — the same for every link

| Gesture | Effect |
|---|---|
| Hover (pointer) | Pointer cursor. The editor's `title` hint, as now, reads "⌘+click to open <target>"; for a note the target is its current title (resolved, cached per note open). |
| Click / tap | The caret goes where it was clicked (unchanged), and the **link chip** opens under the link. |
| ⌘+click (macOS) / Ctrl+click (elsewhere) | Opens at once, no chip — today's #188 behaviour, now for every kind. |
| ⌘/Ctrl+Enter with the caret inside a link | Opens it — the keyboard path (*my call*). The shortcut table lists nothing for it, but `onEditorKeydown`'s checklist branch (`e.key === 'Enter' && !e.shiftKey`) ignores modifiers, so ⌘/Ctrl+Enter on a checklist row continues the checklist today. Handle the link case **before** that branch; ⌘/Ctrl+Enter outside a link keeps today's behaviour. |
| Esc, typing, clicking elsewhere | Closes the chip. |
| Scrolling the note, the soft keyboard opening or closing | The chip follows its link; it closes only when the link leaves the visible area (controller Ruling 16). |

This changes `#meeting-…` anchors: a plain click now opens the chip instead
of scrolling straight away (*my call: one gesture for every link is the
point; ⌘+click still jumps*).

**The chip never takes focus.** Its buttons prevent the default of **both**
`pointerdown` and `mousedown`, exactly as `FormatBar.svelte` does and for
the reason its comment gives: a tap's compatibility mousedown is where
WebKit and Chromium move focus, and a mouse on a Chromebook-class device
sends no cancelled pointerdown first. It is positioned with the
existing caret-list placement (`linkPickerStyle`): under the link on
desktop and tablet; on a phone, clear of the format bar and keyboard.

Shift+click and every other click on an `<a>` stay default-prevented, as
`linkOpen.ts` explains (tauri-plugin-opener's window listener).

## Drawing links without changing the note

Phase 1 adds **no markup** to the body.

- **Scan.** Build the body's link text from the DOM (see "Link rules"
  below — not `findMatches.ts`'s model, which differs on inline markup),
  run the two rules, and keep the result as **offsets**, not `Range`s —
  a Gmail save re-renders `.editor-body` and kills every Range (gotcha #34).
  Rescan on note load and after each input, coalesced to one scan per
  animation frame.
- **Paint.** Turn the offsets into Ranges and put them in a
  `CSS.highlights` entry, `jodd-link`, styled with the accent colour and an
  underline — what `.editor-body a` already gets. **Follow gotcha #34's
  WebKit rule:** ranges that stop being links move into a transparent
  `jodd-link-gone` highlight rather than being dropped, or WebKit leaves them
  painted. Check this in Safari / the macOS app; jsdom cannot see it.
- **Hit-test.** On click, find the link under the pointer by testing the
  pointer against each candidate Range's `getClientRects()`, not by reading
  the caret: a click past the end of a line puts the caret at the end of the
  link without touching it. `<a>` elements keep using `closest('a')`.
- **Cursor.** `::highlight()` cannot set `cursor`, so `onEditorMouseMove`
  sets the editor's cursor when the pointer is over a link, next to the
  `title` hint it already sets.
- Where `CSS.highlights` is missing (Find has a `canHighlight()` check),
  plain-text links are still hit-tested and still open the chip; they just
  are not coloured.

## Link rules — fixed on both sides (decided 2026-10-22)

The editor must agree with what Rust records (anti-goal 3), and today's Rust
rules are wrong in ways a reader would see. So both sides move to **one new
rule set**, and every note's derived links are rebuilt once.

### What is wrong today

- `strip_html_to_text` turns **every** tag into a space and has no line
  breaks, so a URL or wikilink with inline formatting inside it
  (`https://x.co/<b>path</b>`) splits, and nothing stops at a paragraph.
- `extract_wikilinks`: an unterminated `[[draft` runs on to the next `]]`
  paragraphs later, records one junk edge ("draft … [[real") and the real
  `[[Real]]` gets no edge.
- `extract_urls`: stops at `)`, so `…/wiki/Foo_(bar)` becomes
  `…/wiki/Foo_(bar`; treats `[` as part of a URL, so `https://x/[[a]]`
  yields the URL `https://x/[[a` **and** the wikilink `a`.
- Two inputs: `AppleHtmlDeriver` runs `extract_wikilinks` on **raw HTML**,
  `reconcile_edges_from_body_conn` on stripped text. `<style>` / `<title>`
  text of a whole-document body survives stripping and can become a `cites`
  edge nobody can see.

### The rule set

**Text model.** Links are found in the body's *link text*:
- content of `<head>`, `<style>`, `<script>`, `<title>` is dropped;
- inline elements (`a b i u s strike em strong code span font sub sup mark`)
  are transparent — their text joins its neighbours with nothing between;
- every other element boundary, and `<br>`, is a line break;
- entities are decoded (`&nbsp;` is whitespace, as today).

Rust gets a new `link_text(html)` for this; it does **not** change
`strip_html_to_text`, which FTS and others rely on. The editor builds the
same string from the DOM, keeping a map from each character back to its text
node and offset. Text inside an `<a>` is part of the link text (so Rust still
sees it) but the editor does not draw a second link over an anchor.

**Wikilink.** `[[`, then one or more characters on the same line containing
neither `[[` nor `]]`, then `]]`. Inner text is trimmed; empty is no link.
An unterminated `[[` is skipped and scanning resumes right after it.

**URL.**
- Starts with `http://` or `https://`, case-insensitive, **not preceded by
  an ASCII letter or digit** (`foohttps://` is no URL; `ดูhttps://` is one —
  Thai is written without spaces).
- Runs until whitespace, a line break, or one of `< > " [ ]`.
- `)` belongs to the URL only while it closes a `(` opened inside the URL.
- Then trailing `. , ; : ! ? '` are trimmed, repeatedly.
- Ranges never overlap; if they would, the one that starts first wins.

### Who changes

- `db::extract_wikilinks`, `db::extract_urls` take link text and follow the
  rules above. Every caller passes `link_text(body)`: the two `reconcile_*`
  functions in `db.rs`, `AppleHtmlDeriver`, `lib.rs` (~4107) and
  `ingest/urls.rs` (~64 — check its own glued-text case still holds).
- **Rebuild once:** a schema-migration step `DELETE FROM edges;`, the same
  move as migration #15 — `edges` is fully derived, and
  `edges_backfill_if_empty` repopulates every rel on the next start from the
  stored `body_html`. Local only, no remote.
- What moves after the rebuild: the graph, Ask Jodd's pool, MCP
  `note_connections`, Sources, and the Extract / ingest duplicate-source
  check (`find_citation_owner` matches exact URL strings — a URL that now
  keeps its `)` matches differently; that is the fix, not a regression).

### Shared cases

`tests/fixtures/link-detection.json`, run from Rust and from vitest (the way
`tests/fixtures/folder-scope.json` is shared). Input is **HTML**, expected
output is the links with their kind and text. It must cover every bullet in
"What is wrong today", plus Thai text next to a URL, a URL followed by
`&nbsp;`, a URL inside `<a>`, a whole-document body with `<style>`, and a
wikilink across two paragraphs (no link).

## Code layout

- `src/lib/links/target.ts` — link source → `LinkTarget` (pure).
- `src/lib/links/scan.ts` — text-node scan → offsets (pure over a DOM tree).
- `src/lib/links/handlers.ts` — the registry.
- `src/lib/links/paint.ts` — highlights, including the gone-highlight pass.
- `src/lib/components/LinkChip.svelte`.
- `src/lib/editor/linkOpen.ts` — folded into the above; its tests move with
  it.
- `followMeetingEvidence` — replaced by the anchor handler.
- Rust: `resolve_note_link` next to `note_connections` (`lib.rs`), a test in
  `db.rs` that runs the shared fixture.

## Phases

1. **This spec.** The new link rules on both sides and the one-time rebuild,
   router, handlers, the scheme allowlist, chip, plain-text URLs and
   wikilinks drawn and clickable, ⌘/Ctrl+click and ⌘/Ctrl+Enter for every
   link, Android tap → chip → Open. The rules and rebuild can land as their
   own PR first; the editor work depends on them.
2. Back / Forward between followed notes (⌘[ / ⌘], the Android back button
   via `phoneNav`), `[[slug#Heading]]` anchors (needs `parse_link_key` and
   `extract_wikilinks` to drop `#…` and `|alias`, or the edge breaks),
   `jodd://note/<id>` as an OS deep link (`capture::route` today ignores any
   `jodd://` that is not capture), and "Copy link to note".
3. A wikilink drawn as a pill with the target's current title (needs
   display-only markup removed on save — gotchas #28 and #37 apply), hover
   cards, host handlers such as "Transcribe into this note" for social video.

## Testing

- Vitest, pure: `target.ts`, `scan.ts` against the shared fixture,
  handler matching.
- Vitest, **mounted** `NoteEditor` (gotcha #28: only mounting observes
  reactive re-entry): open a note with each link kind, click each, assert
  the chip, assert ⌘+click calls `openUrl` / selects the note, and assert
  **no save was issued and `innerHTML` is byte-identical** afterwards. Use a
  whole-document body (`<html><head></head><body>…`), per gotcha #37.
- Rust: the shared fixture through `link_text` + `extract_urls` /
  `extract_wikilinks`; `resolve_note_link` by id, by title, missing, and
  `deleted_pending`; the migration on a DB with old junk edges ends with
  exactly the edges the new rules derive.
- The allowlist test: `handlers.ts`'s named apps equal the extra schemes the
  opener capability allows.
- Live pass (macOS app, then Android phone): a Gmail note and an iCloud note
  each with a pasted URL, a wikilink and a Share to Jodd link. Check colour,
  chip, open, and that nothing syncs — the note's remote version must not
  move.

## Left to the builder

- Chip visual design within the existing tokens (`--accent-action` etc.).
- The rescan trigger details, as long as a keystroke never waits on a scan
  of a long note (measure on the longest note in the test account).
- Whether the hover-title cache lives in the editor or the links module.
