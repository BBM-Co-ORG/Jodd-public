# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

Jodd is a Tauri 2 app: a Svelte 5 interface in a webview inside a native shell,
shipping on macOS, Windows and Android (Android is a Developer Preview). The
design language is web, not native. Phone and tablet layouts still owe the host
OS its guarantees: system Back always works, content clears the status bar,
navigation bar and keyboard, and touch targets are comfortable.

## Users

The primary user is one person running their own knowledge work: capturing
notes, links, meetings and videos, organising them, linking them into a wiki,
tracking tasks and workflows, and asking questions of everything they have
saved. Their AI agents work on the same notes alongside them.

Many of them arrive through Apple Notes. Their notes already live there, and they
need those notes on Windows or Android, or next to tools Apple Notes does not
have. Apple Notes reach is the door in; the knowledge workspace is the job.

## Product Purpose

Jodd began as Apple Notes for non-Apple platforms and has grown into a personal
knowledge workspace: notes, a linked wiki, task and workflow management, and AI
help over all of it. It exists so a person's notes are reachable from every
device and every tool they use, without moving them out of the store they
already trust.

Success: the user reaches for Jodd for every kind of capture and lookup, their
agents use the same notes, and nothing they wrote in Apple Notes or elsewhere is
lost or damaged on the way.

## Positioning

- **One brain, every store.** One local knowledge base over Apple Notes (through
  iCloud, Gmail or Microsoft), a Local Folder vault, or an SSH server the user
  owns. Notes stay where they live, and the Apple Notes round trip stays safe.
- **Local-first, never waits.** Every edit lands on the device first and sync
  runs in the background. Typing, saving and normal navigation never wait on the
  network.
- **Notes your AI can use.** Ask Jodd, Research Studio, Extract and an MCP server
  let the user and their agents read, link and append to the same notes, with
  hidden folders and privacy masking deciding what leaves the device.

## Operating Context

- Used across several devices at once, typically a Mac, a Windows PC, an Android
  phone and an Android tablet, often with Apple's own Notes apps editing the same
  notes in parallel.
- Capture arrives from many directions: typing, Share to Jodd from other apps,
  pasted links (YouTube, social video, articles), meeting notes, and agents
  writing through MCP.
- Notes are frequently in Thai, in English, or mixed.
- Jodd is also the owner's hands-on project for learning AI development, and a
  public build story by BBMedia shown on the Jodd homepage and case study. That
  is context for the homepage and showcase surfaces; it does not steer the app's
  working screens.

## Capabilities and Constraints

- Backends: Gmail, Microsoft (Outlook.com/Exchange), iCloud (CloudKit), Local
  Folder (`.eml` on disk) and SSH. What each can write is declared per backend,
  and the UI must reflect a read-only or partially writable account honestly.
- Features: note editor with find and replace, folders, tags, pins, wikilinks and
  a link graph, full-text search, tasks and workflow folders, Ask Jodd (questions
  over notes, with saved conversations), Extract and URL ingest, Research Studio
  (briefings, FAQs, decks and more from a folder), Organize, backup and restore,
  Share to Jodd, and an MCP server for agents.
- **Round trip is a correctness constraint, not a style choice.** A note's body
  HTML is synced back to Apple Notes. Styling inside a note body is applied at
  display time only and never changes the saved HTML.
- **Local-first doctrine.** A user action updates local state and the screen
  immediately; no normal navigation or editing waits on the remote.
- **UI copy is English only**, decided 2026-07-29 after the interface became
  accidentally bilingual. Note content may be in any language.
- **Thai content must render correctly**: user text never sits on a line height
  below the Thai floor (1.5), so tone marks are not clipped. Search matches Thai
  substrings.
- Light and dark themes, following the OS by default with an in-app override.
- Developer Preview: not yet a frictionless consumer app. Gmail sign-in needs
  OAuth setup, builds are not signed or notarised for every platform, and adding
  new attachments is not supported yet.

## Brand Commitments

- Name: **Jodd**, from Thai จด, "to jot". Renamed from the codename ANB4W.
- Maker: BBMedia. Bundle identifier `co.bbmedia.jodd`.
- App icon: `src-tauri/icons/`; web and social versions in the Jodd-Homepage repo
  (`assets/jodd-icon-1024.png`, `assets/jodd-social-card.png`).
- Voice on the homepage: plain and candid about limits ("a working technical
  product and a public build story by BBMedia, not yet a frictionless consumer
  app").

## Evidence on Hand

- Product direction: `docs/DIRECTION.md` (the second-brain direction, confirmed by
  the owner 2026-06-11) and `docs/ROADMAP.md`.
- Release history: `CHANGELOG.md`.
- Public site and case study: `~/projects/Jodd-Homepage/index.html` and
  `case-study.html`.
- The existing visual system: `src/styles/tokens.css` (measured contrast) and
  `src/lib/designGuards.test.ts` (colour, icon, type, copy and round-trip rules
  enforced in CI).
- **Absent, so never invent:** testimonials, user counts, customer logos, press,
  benchmarks and pricing. Jodd is a Developer Preview with no public pricing.

## Product Principles

1. **Never lose or damage a note.** Correctness of the round trip outranks any
   feature or visual change.
2. **The device is the source of truth for the moment.** The interface responds
   at once; sync is invisible until it needs the user.
3. **Be honest about state.** Say what is syncing, blocked, read-only or in
   conflict, and what each account can and cannot do.
4. **One workspace, many doors.** Each backend, device and agent is another way
   into the same notes, not a separate product.
5. **AI works for the user, inside limits they set.** Hidden folders and masking
   decide what leaves the device, and the user can see what an AI wrote.

## Accessibility & Inclusion

- WCAG 2.1 AA contrast, measured rather than eyeballed, in both light and dark
  themes, including translucent layers composited against their real backdrop.
- Every icon-only button has an accessible name (enforced by a design guard).
- Thai-safe line heights wherever user text can appear.
