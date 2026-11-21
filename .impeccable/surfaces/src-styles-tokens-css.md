---
version: 1
slug: "src-styles-tokens-css"
primary_target: "src/styles/tokens.css"
related_targets: ["src/lib/theme.ts"]
---

# Looks: themes beyond the Lamplit Desk

Scope: app-wide theme ("Look") layer over tokens.css. Mode: Operate. Each look ships light and dark, follows the OS unless the user picks a mode, and keeps every layout, control and state of the app untouched. Lamplit Desk stays the default.

Looks chosen 2026-11-10: Field Instrument (the roll's direction, built first and the reference), Midnight Line and Slate Ink (user also adopted these from the same round).

## Direction contract

THESIS: Jodd as a precise handheld instrument for all-day knowledge work. It refuses both the warm-paper desk and the category's near-black-plus-violet: cool anodised neutrals, exact small type, one signal colour that only ever means "selected" or "act here".

OWN-WORLD: bead-blasted aluminium greys stepping lighter toward the editor (graphite in dark); ink near-black; signal orange #ff4f00 as fill only, ink-orange for text; account key colours (iCloud blue, Gmail green, Outlook grey) as small solid squares; Bai Jamjuree for UI and notes, Geist Mono only for counts, dates and status; 3px corners; hairline borders.

STORY: the user sees at a glance what is open (orange rail), which account they are in (key colour), and how much is waiting to sync (lit segments), then writes without the chrome asking for attention.

FIRST VIEWPORT: the existing three panes unchanged: sidebar, note list with New note as the one orange fill, editor with the toolbar and the status word at its right.

FORM: assigned direction, candidate 5 of 7 (pocket-synthesizer / field-recorder product language); seed:94bf7dfa3819853a. Signature move: the segmented sync meter, lit segments = changes waiting.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Adopted alternates (same round)

- Midnight Line (catalog wayfinding-cartography-signage-midnight-transit-diagram): midnight enamel panes, porcelain ink, accounts as line colours; signature: the folder tree drawn as a transit line with stations, the open folder's station lit. Light variant: porcelain ground, same lines.
- Slate Ink: e-paper greys, carbon ink, no hue; signature: selection by inversion. Dark variant: night e-ink.

## Build verdict (2026-11-10, branch design/looks)

Shipped all three looks in light and dark, behind Settings › General › Look. Each look passes every Lamplit audit (`tokens.test.ts`, 887 tests). They were checked in a browser at 1440×900 in all six look/mode pairs, plus the picker. Departures from the mocks, each for a reason:

- Field Instrument's meter is on the account row only. The mock also put one by the editor's "Sync pending", but that word belongs to the note and the count belongs to the account. Segments are 2px with a 1px gap (the mock had 3px), because the full width squeezed a short account name to "k…" in a 200px sidebar. Lit segments use Signal Orange as a mark (#c23900, 3.36:1 on a selected row), because #ff4f00 is only 2:1 on aluminium.
- Midnight Line puts every station on one straight line, with a child folder's station smaller. The mock had 45° branches, which the folder tree's DOM cannot draw without per-row geometry. The account's line badge is a 14×4 bar beside its name.
- Slate Ink's chips keep a pale tint (#cfcfcb): four components put `--text` on `--accent-tint`, and an ink tint would hide them. Inversion is the open folder, the open note, primary buttons and the current find match. The rows switch without a fade.
- Found on the way, and fixed for every look: the pane seams showed the webview's white in dark mode. `html` now has a themed background.

Verdict: matches the direction contract. The one move per look does work (it shows the waiting count, the account a folder belongs to, and what is open), and no chrome was added.

## Open decisions

- Slate Ink's e-ink refresh flash on save needs a "just saved" hook in the editor; not in the first build.
- The phone layout (PhoneFolders, phone list) gets each look's palette and type but not its signature move.
- Device check owed: the WKWebView/WebView2 font rendering of Bai Jamjuree, Anuphan and Noto Serif Thai in Thai, and the macOS window behind the seams.
