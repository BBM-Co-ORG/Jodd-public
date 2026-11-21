---
name: Jodd
description: A calm, dense, practical knowledge workspace on warm paper under one amber lamp.
colors:
  lamp-amber: "#c97c1f"
  amber-ink: "#8a4800"
  amber-ink-hover: "#8f5310"
  lamp-mark: "#9c5a0c"
  amber-tint: "#f3e4c0"
  ink-on-amber-tint: "#3a2f12"
  focus-blue: "#3d78bc"
  paper-editor: "#fffef9"
  paper-list: "#faf6ed"
  paper-sidebar: "#f0ebe0"
  panel-white: "#ffffff"
  border-subtle: "#e8e2d4"
  border: "#ddd8cc"
  border-strong: "#cfcabb"
  ink: "#222222"
  ink-secondary: "#3d3d3d"
  ink-muted: "#595959"
  ink-disabled: "#8c8c8c"
  ink-inverse: "#ffffff"
  danger: "#ac2517"
  success: "#0b6823"
  tag-violet: "#7b4ea8"
  graph-folder: "#a19063"
  graph-tag: "#ad7fce"
  graph-link: "#7495b2"
  graph-backlink: "#739b7b"
typography:
  headline:
    fontFamily: "IBM Plex Sans Thai, -apple-system, BlinkMacSystemFont, sans-serif"
    fontSize: "22px"
    fontWeight: 700
    lineHeight: 1.5
  title:
    fontFamily: "IBM Plex Sans Thai, -apple-system, BlinkMacSystemFont, sans-serif"
    fontSize: "16px"
    fontWeight: 600
    lineHeight: 1.5
  body-reading:
    fontFamily: "IBM Plex Sans Thai, -apple-system, BlinkMacSystemFont, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.7
  body:
    fontFamily: "IBM Plex Sans Thai, -apple-system, BlinkMacSystemFont, sans-serif"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.5
  label:
    fontFamily: "IBM Plex Sans Thai, -apple-system, BlinkMacSystemFont, sans-serif"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.5
  section-label:
    fontFamily: "IBM Plex Sans Thai, -apple-system, BlinkMacSystemFont, sans-serif"
    fontSize: "11px"
    fontWeight: 700
    lineHeight: 1.35
    letterSpacing: "0.04em"
  mono:
    fontFamily: "IBM Plex Mono, ui-monospace, SFMono-Regular, Menlo, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.5
rounded:
  sm: "4px"
  md: "6px"
  lg: "10px"
spacing:
  "0-5": "2px"
  "1": "4px"
  "1-5": "6px"
  "2": "8px"
  "3": "12px"
  "4": "16px"
  "6": "24px"
  "8": "32px"
components:
  button-secondary:
    backgroundColor: "{colors.panel-white}"
    textColor: "{colors.ink}"
    typography: "{typography.label}"
    rounded: "{rounded.md}"
    padding: "4px 12px"
  button-primary:
    backgroundColor: "{colors.amber-ink}"
    textColor: "{colors.ink-inverse}"
    typography: "{typography.label}"
    rounded: "{rounded.md}"
    padding: "4px 12px"
  button-primary-hover:
    backgroundColor: "{colors.amber-ink-hover}"
    textColor: "{colors.ink-inverse}"
  input:
    backgroundColor: "{colors.panel-white}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.md}"
    padding: "6px 8px"
  chip-selected:
    backgroundColor: "{colors.amber-tint}"
    textColor: "{colors.ink-on-amber-tint}"
    typography: "{typography.label}"
    rounded: "{rounded.sm}"
  dialog-panel:
    backgroundColor: "{colors.panel-white}"
    textColor: "{colors.ink}"
    rounded: "{rounded.lg}"
    width: "480px"
  note-row:
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    padding: "10px 16px"
---

# Design System: Jodd

> **Source of truth.** The values above mirror `src/styles/tokens.css` (light
> theme). If the two disagree, `tokens.css` wins and this file is stale: re-run
> `/impeccable document`. Many of these rules are also enforced in CI by
> `src/lib/designGuards.test.ts`.

## Overview

**Creative North Star: "The Lamplit Desk"**

Jodd is a desk you work at all day. The paper is warm, never stark white, and it
steps in tone from the sidebar (darkest) through the note list to the editor
(lightest), so the place where you write is always the brightest thing on the
desk. One amber lamp marks what is selected, active or actionable. Everything
else, the tools, the chrome and the status, stays quiet in the warm neutrals
until it is needed.

The feel is **calm, dense and practical**. Screens carry a lot at once (folders,
a long note list, a full note, a format bar) at small, exact type sizes, and none
of it is decoration. Calm comes from restraint, not emptiness: one accent hue,
flat panes, shadows only on things that float, and colour that always means
something. Precision is literal here. Every text colour was measured for WCAG
contrast, including on top of hover and selection overlays, and the measurements
are written next to the values in `tokens.css`.

Thai and Latin share the page as equals. One type superfamily sets both on the
same baseline, and line heights never drop to where Thai tone marks clip.

**Key Characteristics:**
- Warm paper surfaces in three steps, lightest where you write.
- One amber accent at two strengths: Lamp Amber for fills, Amber Ink for text.
- Blue appears only as the keyboard focus ring.
- IBM Plex Sans Thai and IBM Plex Mono, seven sizes from 11 to 28px.
- Flat panes; shadows only on menus, popovers, cards and dialogs.
- A full dark theme with the same structure, following the OS unless overridden.
- Three alternative looks (Field Instrument, Midnight Line, Slate Ink), each
  with light and dark, held to the same measured rules. See Looks.

## Colors

Warm paper neutrals and a single amber accent, with blue held back for focus and
a separate categorical palette for the graph.

### Primary
- **Lamp Amber** (#c97c1f): the glow. Fills, tints, borders and washes:
  multi-select washes, wash backgrounds, accent borders, the editor's quote bar.
  It never has text on it or as it (white on Lamp Amber is only 3.29:1).
- **Amber Ink** (#8a4800): the same amber, deep enough to write with. Primary
  buttons, links, "Learn more" summaries, pin icons and any text that must read
  as accent. 4.72:1 on the worst composited surface; white on it passes too.
  Hover darkens it slightly (#8f5310).
- **Amber Tint** (#f3e4c0) with **Ink on Amber Tint** (#3a2f12): the selected
  chip or pill, 10.45:1.
- **Lamp Mark** (#9c5a0c light, #d99a45 dark): the lamp's mark on a row, the
  3px rail on the open note and the open folder. A shade deeper than Lamp Amber
  in light because a state mark must hold 3:1 against the selected row it sits
  on, and Lamp Amber measures only 2.06 to 2.50:1 there. Lamp Mark measures
  3.38:1 at worst in light and 3.41:1 in dark. `--accent-mark`.

### Secondary
- **Focus Blue** (#3d78bc): the only blue in the app. The 2px keyboard focus
  outline and the input focus ring. At least 3.83:1 on every surface.

### Tertiary
- **Tag Violet** (#7b4ea8): the tag icon, so a tag reads as a tag at a glance.

### Neutral
- **Editor Paper** (#fffef9): the note editor, the brightest surface.
- **List Paper** (#faf6ed): the note list.
- **Sidebar Paper** (#f0ebe0): the sidebar, the darkest pane.
- **Panel White** (#ffffff): menus, dialogs and popovers that float above the desk.
- **Warm borders**: subtle (#e8e2d4), default (#ddd8cc) and strong (#cfcabb).
- **Ink ramp**: primary text (#222222), secondary (#3d3d3d), muted (#595959),
  disabled (#8c8c8c, deliberately below 4.5:1 because it must look unavailable).
  The three readable steps are spaced so each one is visibly different.
- **State overlays**: black at 4% (sunken), 6% (hover) and 10% (active).
- **Status**: danger (#ac2517) and success (#0b6823), each with a light wash.

### Graph palette
Four categorical colours for the note graph's relation types: folder (#a19063),
tag (#ad7fce), link (#7495b2) and backlink (#739b7b), each with a pale fill. Each
clears 3:1 as a stroke on Editor Paper, and every pair is at least ΔE 24 apart.

### Dark theme
The same desk at night, under the same lamp: warm near-black paper (editor
#1a1714, list #1f1b17, sidebar #26211c, panel #2a251f), cream ink (#efe8dc), and the amber
lifted for dark ground (Lamp Amber #d99a45, Amber Ink #e8b571). Focus becomes
#7fb2e8. Overlays switch to warm white at 3/6/11%, and shadows get heavier so
they still read as depth.

**The Same Desk at Night Rule.** Dark moves the hue, never the structure: every
dark neutral is warm and low in chroma, and keeps the luminance step of the role
it plays. A cool or grey neutral reads as a different app; a saturated one
competes with the lamp.

### Named Rules
**The Two Ambers Rule.** Lamp Amber never carries text. Anything that is text or
has text on it uses Amber Ink. Collapsing the two ships a contrast failure that
looks like a tidy-up.

**The Blue Means Focus Rule.** Blue is reserved for the keyboard focus ring. A
blue button, link or badge breaks the one signal a keyboard user relies on.

**The Overlaid Contrast Rule.** Measure contrast on the composited result, not
on the bare surface: muted text on a selected sidebar row is three layers deep,
and that is where failures hide. Badges are opaque (`--surface-badge`) for this
reason.

**The Lamp Marks Here Rule.** Each column has one lamp: the open folder in the
sidebar and the open note in the list carry a 3px Lamp Mark rail, rounded and
inset from the row's ends, over the neutral active overlay. Nothing else gets
the rail. Multi-select is a different state with its own sign: a checkbox
(Amber Ink when checked) and the amber wash. When a row is both open and
selected, it shows both. Guarded by `src/lib/lampGuards.test.ts`.

**The Meaningful Colour Rule.** Icons stay `currentColor` unless colour carries
meaning: delete is danger, pin is Amber Ink, tag is Tag Violet. Colouring every
icon removes colour's ability to mean anything.

**The Touched Amber Rule.** In the editor, amber as a state has one job:
another device or an agent touched this note. The "Edited on another device"
notice and the trust chip ("Changed elsewhere", "<agent> · 2h ago") share it on
every breakpoint: a Lamp Amber wash, with Amber Ink on the chip (6.23:1 light,
7.60:1 dark; 5.60:1 and 6.18:1 hovered) and body ink on the notice. Each is
said once: no toolbar or header copy of the notice. No other state borrows
amber (a draft is muted), and red (`--danger`) is for failures only: a push the
backend refused, an action that failed. Links, selection, find matches and the
primary action keep amber's app-wide roles; this rule is about states. Guarded
by `src/lib/editorStatusColour.test.ts`.

## Typography

**Body Font:** IBM Plex Sans Thai (with -apple-system, BlinkMacSystemFont, sans-serif)
**Label/Mono Font:** IBM Plex Mono (with ui-monospace, SFMono-Regular, Menlo, monospace)

**Character:** one superfamily with matching metrics, so Thai, Latin and code sit
on the same baseline with no per-script tuning. There is no separate display face:
headings are the same sans at 600 or 700.

### Hierarchy
- **Headline** (700, 22px, 1.5): pane titles, the phone bar, the note title field
  and Heading 1 inside notes. The 28px step is used only on the first-run screen.
- **Title** (600, 16px, 1.5): settings page titles and empty-state headlines.
  Heading 2 inside notes uses the same size at 700.
- **Body Reading** (400, 14px, 1.7): the note body, the one place for sustained reading.
- **Body** (400, 13px, 1.5): the default UI text, including list rows, dialogs and inputs.
- **Label** (400, 12px, 1.5): buttons, secondary lines and metadata.
- **Section Label** (700, 11px, 1.35, 0.04em, uppercase): group headers in the
  sidebar and note list. Chrome only, never user text.
- **Mono** (400, 12px): code, parameters and shortcut keys.

### Named Rules
**The Thai Floor Rule.** Anywhere user text can land, line height is at least 1.5.
1.7 is for the editor body. 1.35 is allowed only for chrome that never shows user
text. At line height 1, Thai tone marks clip.

**The Seven Sizes Rule.** Font sizes come from the seven size tokens (11, 12, 13,
14, 16, 22 and 28px), never a literal px value. Nothing goes below 11px: Thai
vowel marks lose legibility faster than Latin descenders. `--size-mark` (56px)
is not an eighth size: it belongs to the brand mark alone.

**The Loaded Weights Rule.** Use 400, 600 and 700, the weights the app bundles
for both Latin and Thai. Any other weight is not a face the app ships: a 500
quietly renders as 400. A weight bundled for one script only is worse: the font
files carry no unicode-range, so Thai text at that weight falls back to the OS's
own Thai font, a different face on Mac, Windows and Android. Add a weight for
both scripts or not at all; a design guard checks the pair. Components name a
weight by its token (`--weight-regular`, `--weight-semibold`, `--weight-bold`),
never a number or keyword, in `font-weight` or the `font` shorthand.

## Layout

The desktop layout is three flat panes side by side: sidebar, note list and
editor. The sidebar and list have user-resizable widths, and the panes are told
apart by paper tone and 1px warm borders, not by gaps or cards.

Android switches by width, not by device: below 700px is the **phone** layout, a
single-pane stack (folders, then tags, then list, then note)
with a phone bar and system Back moving one level up. From 700px it is
**tablet**, and from 1024px **wide tablet**, with panes side by side. Dialogs and
sheets respect the safe-area insets and the on-screen keyboard height
(`--kb-inset`).

Spacing is a 4px-based scale (2, 4, 6, 8, 12, 16, 24 and 32px). Density is high
on purpose: a list row is 10px by 16px of padding, and dialog footers are 12px by
16px. Hit areas are at least 28px on desktop and 48px on touch.

Dialogs are at most 480px wide (360px for the small size) and leave a 16px
gutter to the screen edge.

## Elevation & Depth

The desk is flat. Depth inside the app comes from tonal layering, with paper
stepping lighter toward the editor, and from 1px borders. Shadows belong only to
things that float above the desk.

### Shadow Vocabulary
- **Raised** (`box-shadow: 0 1px 4px rgba(0,0,0,0.15)`): buttons that sit slightly proud.
- **Card** (`box-shadow: 0 2px 16px rgba(0,0,0,0.11)`): the sign-in card and the find bar.
- **Menu** (`box-shadow: 0 6px 24px rgba(0,0,0,0.16)`): menus and popovers.
  **Menu-up** mirrors it for panels anchored at the bottom.
- **Modal** (`box-shadow: 0 12px 40px rgba(0,0,0,0.25)`): dialogs, over a 35% black
  scrim (also behind the tablet drawer).

In dark theme each shadow is the same shape at much higher opacity (0.45 to 0.70).

### Stacking Order
What floats stacks in one order, set by the `--z-` tokens in `tokens.css`, lowest
first: sticky headers (`--z-sticky`), the tablet drawer and account panel
(`--z-drawer`), the error bar (`--z-banner`), menus and popovers (`--z-popover`),
toasts (`--z-toast`). A menu opened near the bottom covers the error bar, never
the reverse. Dialogs are not in the order: `showModal()` puts them on the
browser's top layer, above every z-index.

### Named Rules
**The Floating-Only Shadow Rule.** If it does not float, it has no shadow. Panes,
list rows and inline panels separate by tone and border.

## Shapes

Gently rounded and consistent: 4px for badges, chips and small callouts, 6px for
buttons and inputs, and 10px for dialogs and large panels. Borders are 1px in the
warm border colours. Focus is a 2px Focus Blue outline offset 2px from the
element. The note body has no focus outline, because its caret is the focus
signal.

## Components

### Buttons
Small, quiet and exact. The primary action is the only filled one.
- **Shape:** gently rounded (6px).
- **Secondary (default):** Panel White with a 1px warm border, ink text, 12px
  type, 4px by 12px padding. Hover adds the 6% overlay.
- **Primary:** Amber Ink fill and border with white text. Hover darkens to #8f5310.
- **Disabled:** 60% opacity, default cursor.
- **Placement:** in dialog footers, secondary actions sit left and the primary
  action sits right.

### Chips
- **Style:** selected chips and pills use Amber Tint with dark amber text, 4px corners.
- **Badges:** opaque, never translucent, so their contrast holds when the row
  under them changes state.

### Cards / Containers
- **Corner Style:** 10px on dialogs.
- **Background:** Panel White.
- **Shadow Strategy:** Modal shadow (see Elevation & Depth).
- **Border:** none on the panel; footers are divided by a 1px subtle border.
- **Internal Padding:** 16px gutter; footer 12px by 16px.

### Inputs / Fields
- **Style:** 1px warm border, 6px corners, 6px by 8px padding, 13px text at 1.5.
  Every input, select and textarea sets an explicit background, so nothing falls
  back to the system's white in dark mode.
- **Focus:** the shared focus ring (2px Focus Blue at 25%).
- **Error / Disabled:** errors use Danger text with Danger Wash; disabled text
  uses the disabled ink.

### Navigation
- **Sidebar:** Sidebar Paper, uppercase 11px section labels, rows that take the
  hover and active overlays. The open folder adds the Lamp Mark rail (The Lamp
  Marks Here Rule). The wordmark is 13px, 700 weight, widely spaced.
- **Note list:** List Paper with section headers in the same uppercase label
  style. The list's title is Headline (22px), like every pane title, and New
  note is the one Amber Ink control in the header. The open note's row takes
  the 10% active overlay and the Lamp Mark rail. While any note is
  multi-selected, every row shows a checkbox; selected rows take the amber wash,
  and a stronger wash when a row is both open and selected.
- **One column, two sizes:** desktop, tablet and phone share one order and
  one set of names (spec 2026-11-12): the "For ⌄" account picker (only with
  two or more accounts), Views (the four computed lists), Tags, then each
  account with its header, its folders, its Agents group and its own
  Recently Deleted. The phone is that column at touch size; it differs only
  where 360px and touch force it (Tags opens its own pane, each view shows
  its meaning as a second line, and the bottom bar replaces the footer).
- **Phone:** a phone bar with a back label, showing one pane at a time.

### Note row (signature)
The unit of the whole app: a full-width, text-left button with 10px by 16px of
padding and a 0.15s background transition. Hover and the open row are neutral
overlays, never new colours, so the paper tone underneath always shows through.
Amber reaches a row two ways only: the Lamp Mark rail on the open row, and the
wash on multi-selected rows.

### Graph view (signature)
The local note graph draws folders, tags, links and backlinks in the four
categorical graph colours over Editor Paper, behind a warm, heavier scrim
(`rgba(20,16,8,0.45)`). Every node is clickable and has a hover title.

### Editor status (The One Voice Rule)
**The One Voice Rule.** Every state a note in the editor can be in is said in
**one** place, its actions live there, and only states that need the user are
announced to a screen reader. There are three voices:

- **the save word** in the toolbar (and the phone header's copy of it): routine
  progress — Unsaved, Saving…, Synced, Sync pending, On this device. Never a live
  region; it keeps its fixed width even while another voice speaks, so the
  formatting row does not reflow;
- **a notice** above the title: a state that needs the user (edited on another
  device, a conflict copy, a refused push, a read-only note). It carries the
  state's actions and is announced. It holds the line being typed in place as it
  comes or goes, and its buttons drop under its text in a narrow editor. A notice
  does not blink: a refused push's notice stays up, word for word, while the
  user types (an edit re-arms the push, but the last answer stands) and goes
  only when a push succeeds. Only a retry the user asked for (Try again, Move to
  Notes) changes its lead, to one no longer than the usual ("Trying again. Your
  text is saved here."), with its buttons held at 60% until the answer. And a question is not decided for the user
  while they read it, nor made blind: the autosave waits while the notice is
  new and nothing has been typed since (up to 10 seconds, so the edits still
  reach the device), while "Use other version" is open, or while the notice has
  focus; "See the other version" shows the other device's text before the user
  chooses. An action returns focus to the text, at the line and column the
  caret was on, never to the page or to line 1. Notices are announced by one
  always-present live region, by their lead, not by each notice's buttons;
- **the metadata chip** and its popover: a state that matters but does not
  interrupt (an unreviewed agent change, a refused pin). Never a state a notice
  already says.

A refusal is told in plain words with the next step ("Outlook can't find the
folder “Ideas” yet. Move the note to Notes and it will sync."); the backend's own
text sits behind Details. Its notice offers only the buttons that can work for
that refusal: Move to Notes for a folder the server refused (never a copy into
that same folder), Try again where trying again can work, and a copy as a new
note where the copy can sync. A note Apple has locked says first that the last
edit is on this device only, and offers the copy that keeps it. A notice's
promise matches the code exactly: "Edited on another device" says only added
lines are merged, because that is the editor's merge rule. The table is `src/lib/editor/editorStatus.ts`: a new
state joins it first, and `src/lib/editorStatus.test.ts` checks the mounted
editor against it. The ⋯ menu may list a state's action as a keyboard route;
nothing else repeats it on screen.

### Brand mark (signature)
The app's name in Thai, จด ("to jot", the word Jodd comes from), set in IBM
Plex Sans Thai Bold at `--size-mark` (56px) in Amber Ink, with the English
name beneath it as the heading. Behind it, the lamp: a soft radial pool of
`--lamp-glow` (Lamp Amber at 18%, 16% in dark) that comes on when the screen
opens. Amber Ink on the brightest part of the glow measures at least 4.5:1 in
both themes, though 56px bold only needs 3:1.

**The Brand Mark Rule.** The mark is the one piece of Thai in the interface,
and the one exception to English-only copy, because it is a mark and not
copy. It lives in `BrandMark.svelte` only, appears on the sign-in screen and
in About only, and is hidden from screen readers (`aria-hidden`): the English
name next to it is what gets read. It never becomes a label, a button, a
heading or a loading state. Guarded by `src/lib/brandMark.test.ts` and the
Thai guard in `designGuards.test.ts`.

### Motion
Motion is feedback, not decoration. Durations are tokens named by what they
do: `--dur-instant` (100ms, a press on a small control), `--dur-fast` (150ms,
a colour or opacity change), `--dur-move` (220ms, anything that changes
position or size) and `--dur-spin` (one turn of a spinner), all on
`--ease-out`, an exponential ease-out. Under reduced motion, movement stops
(`--dur-move` is 0) and spinners show a still busy state; colour and opacity
changes stay, because they move nothing.

**The One Lamp Moment Rule.** The app has one authored motion, and it is the
lamp coming on (`--dur-focal`, 700ms). On the sign-in screen and in About,
the glow behind the brand mark brightens and grows out from it; under reduced
motion it only brightens. After a first sign-in, the arrival line ("773
notes found in iCloud. They stay where they are.") arrives lit in Amber Tint
and settles to plain paper over about two seconds. Nothing else in the app
gets an entrance: a new authored moment replaces this one or is not added.

### Learn more
One inline disclosure style for explanations: a 12px muted block whose summary is
Amber Ink. It is not a tooltip (touch has no hover) and not a link (it works
offline).

Learn more holds the *how* and the *why*, never a governance fact. What leaves
the device, what is kept, what is not encrypted and what AI may see stays on
screen, on every platform (the Explicit Rule, spec 2026-11-11 Settings › Privacy &
Data; `explicitRule.test.ts` checks it).

## Looks

The Lamplit Desk is the default and the reference. Settings › General › Look
offers three other worlds beside it (chosen 2026-11-10): **Field Instrument**,
**Midnight Line** and **Slate Ink**. Look and light/dark are two separate
choices. Every look has both modes, and Appearance (System, Light, Dark) still
decides which one shows.

A look changes palette, type, corners and **one signature move**, and nothing
else. Layout, controls, states and copy are the same in every look, so a user
who switches loses nothing they had learned. The tokens live in
`src/styles/looks/<id>.css`, scoped to `:root[data-look='<id>']` and loaded
after `tokens.css`. The list is `LOOKS` in `src/lib/theme.ts`. Each signature
move is a scoped `:global(:root[data-look=…])` rule in the component it changes.

| Look | Panes (light / dark) | Accent | Type | Corners | Signature move |
|---|---|---|---|---|---|
| Field Instrument | aluminium #dcdcd8 → #f4f4f1 / graphite #1b1c1e → #2a2b2e | Signal Orange #ff4f00 (fill and mark only), Ink Orange #9a3000 for text | Bai Jamjuree; Geist Mono for counts, dates and status | 2 / 3 / 5px | The **segmented sync meter**: eight segments on the account row, one lit per change waiting; a refused change or lost sign-in is one taller segment in danger |
| Midnight Line | porcelain #e4e9f1 → #f7f8fb / midnight enamel #0e1830 → #182848 | iCloud blue #2a63e0 / #3e7bfa | Anuphan | Lamplit's | The **transit line**: each account is a 3px line in its own colour, its folders are stations, and the open folder is the lit station; a signed-out or blocked account's line is broken |
| Slate Ink | e-paper #dededa → #efefeb / night e-ink #1c1c1b → #262625 | none: ink #161616 | Anuphan for the UI; Noto Serif Thai for the note's title and body | 0 | **Selection by inversion**: the open folder and the open note are ink with paper text, and switch instantly (e-paper does not fade) |

Accounts take a colour only in the looks that use one, through
`--account-<backend>` (Field Instrument's key square, Midnight Line's line), and
always next to the account's name and backend label, never instead of them.

**The One Move Rule.** A look gets exactly one signature move, and it must do
work: the meter shows how much is waiting, the line shows which account a
folder belongs to, and inversion shows what is open. A second move, or one
that only decorates, makes the look a costume.

**The Same Bar Rule.** A look is held to every measured rule the Lamplit Desk
is: the full contrast audit with overlays, ramp separation, the graph's ΔE,
the wash chips, opaque badges, and 3:1 for the selection mark and every
account colour on a selected row. Every themeable token is restated in both
modes, so nothing warm leaks through. `src/styles/tokens.test.ts` runs all of
it per look, and `designGuards.test.ts` checks that each look's faces are
bundled for both Latin and Thai at 400/600/700.

**Which Lamplit rules translate.** Overlaid Contrast, Meaningful Colour and
Touched Amber hold in every look, with the look's accent in amber's role.
Lamp Marks Here holds as "one selection mark per column", and the mark is the
look's own: the rail in Field Instrument, the lit station in Midnight Line,
inversion in Slate Ink. Blue Means Focus is Lamplit's only, because blue is
Midnight Line's accent and its focus ring. Slate Ink keeps a muted colour for
danger, success and the graph, because those colours carry meaning.

## Do's and Don'ts

### Do:
- **Do** take every colour, size, radius and space from `tokens.css` variables.
- **Do** use Amber Ink for anything that is text or carries text, and Lamp Amber
  only for fills, tints, rails and borders.
- **Do** keep user text at line height 1.5 or more (1.7 in the editor body).
- **Do** give every icon-only button an accessible name.
- **Do** give every input, select and textarea an explicit themed background.
- **Do** style note-body content at display time, through the scoped
  `.editor-body` rules.
- **Do** edit dark values in the `prefers-color-scheme: dark` block and
  regenerate the `data-theme` blocks; never hand-edit the generated ones.
- **Do** honour reduced motion: movement stops, continuous spinners have a
  still busy state, and colour and opacity changes stay.
- **Do** show "another device or an agent touched this note" in amber, and
  only that, among the editor's states.

### Don't:
- **Don't** put text on Lamp Amber, or use blue for anything but focus.
- **Don't** use red for a state that is not a failure. Another device's edit
  is not one: the edits here are kept.
- **Don't** write raw `rgba()` colours in components; the `--accent-*`,
  `--danger-*` and surface tokens exist for that.
- **Don't** set a literal px font size or an ad-hoc monospace stack.
- **Don't** use emoji or control glyphs as icons in UI chrome.
- **Don't** put Thai in UI copy: the interface is English-only. Note content can
  be any language. The brand mark is the one exception, and it is a mark, not
  copy (The Brand Mark Rule).
- **Don't** rewrite or strip inline styles in a note's saved HTML to make it look
  right. That HTML round-trips to Apple Notes.
- **Don't** add a shadow to anything that does not float.
- **Don't** merge graph colours with theme colours. Two relation types would
  become indistinguishable.
