# Showcase Evidence Layer — Design

**Date:** 2026-08-16
**Status:** Approved for planning
**Repos touched:** `Jodd` (private upstream), `Jodd-Homepage` (the site), `Jodd-public` (receives docs via snapshot)

---

## 1. Problem

`jodd.bbmedia.co.th/case-study.html` already exists and already makes the
argument. Its structure is sound — *Starting point → What shipped → AI
operating model → Open learning trail → Work with BBMedia* — and its hero
promises the reader **"working evidence."**

The page does not deliver any.

| The page has | The page lacks |
|---|---|
| A four-step AI operating model, stated as claims | Any number at all except `v0.23.1` and "three decades" |
| "local-first architecture", "bounded agent access" as one-line summaries | Diagrams — the page contains zero |
| Links to `ARCHITECTURE.md`, `HISTORY.md`, `SECURITY.md` | Depth a CTO would accept without leaving the page |
| An honest "what this does not claim" boundary | A single worked decision showing *how* a call was made |

The gap is precise. **This project is not "build a showcase." It is "attach
evidence to a showcase that already exists."** Everything written stays;
the work is insertion, not replacement.

A second, smaller gap: the page's source links are pinned to `v0.23.1`
while the product is at `v0.24.1`.

## 2. Goals

1. A reader who never scrolls past the hero leaves knowing the scale of the
   work, in numbers they could verify.
2. A founder-level reader understands *what was built and how it is run*
   from diagrams and prose, without expanding anything.
3. A CTO-level reader can reach real symbol names, file paths, and one
   fully-worked decision without leaving the page.
4. Two angles that currently have no home on the page — **enterprise-grade
   data handling** and **evidence-based decision making** — get one.
5. The public repo gains a document explaining *how the system got built*,
   to sit alongside the one explaining *how the system works*.

## 3. Non-goals

- Rewriting the existing prose. It is good and it is the author's voice.
- Adding user counts, adoption metrics, performance benchmarks, or a
  roadmap. None are measured; inventing them would destroy the one asset
  this page has, which is that its claims survive inspection.
- A separate marketing site or landing page. The channel is the existing
  page at its existing URL.
- Publishing `CLAUDE.md`, `docs/superpowers/plans/`, `.claude/`, or
  `memory/`. These are deliberately excluded by
  `public-mirror-prep/scripts/sync-to-public.sh` and stay excluded. Any
  process evidence that must be public gets **restated** in a public
  document, not exposed by relaxing that list.

## 4. Audience model — two layers, both sharp

The reader is layered: a founder evaluating whether to talk, and a CTO
deciding whether to trust. The failure mode is writing one paragraph aimed
between them, which lands shallow for the CTO and jargon-heavy for the
founder.

The resolution is structural, not editorial:

- **Top layer** — claim sentence + diagram + number. Always visible. Reads
  complete on its own.
- **Bottom layer** — a `<details>` block titled *"How this actually works"*
  under each new section. Contains symbol names, file paths, and the
  reasoning. Collapsed by default.

Neither layer is softened to accommodate the other.

## 5. Verified metrics

Every number below was measured on 2026-08-16 **at the point `main` stood
when this spec was written**, and is left at that value deliberately: this is
a historical record of what the design was working from, not a live table.
The measurement command is recorded so any reader can reproduce it, and so a
future update does not have to guess how a figure was originally derived.

The published figures are the ones in `ENGINEERING-PRACTICE.md`, under "How
these numbers were measured"; they are pinned to commit `72c312f` and they
are the authority. One row here disagrees with that table: `Commits` reads
784, because `main` gained six commits between this spec being written and
the figures being pinned, where the published value is **790**. Every other
row agrees.

| Figure | Value | How measured |
|---|---|---|
| Commits | 784 | `git rev-list --count HEAD` |
| Span | 2026-06-03 → 2026-08-16 (74 days) | first/last commit date |
| Version | v0.24.1 | `package.json` |
| Tagged releases | 38 | `git tag \| grep -c '^v'` |
| Rust lines | 36,611 | `src-tauri/src` + `jodd-mcp/src`, `*.rs` |
| Svelte + TS lines | 15,307 | `src/**`, excluding `*.test.ts` |
| Automated tests | **878** | Rust 680 (`#[test]` / `#[tokio::test]`) + Vitest 198 (`it(` / `test(`) |
| Design specs | 24 | `docs/superpowers/specs/*.md` |
| Implementation plans | 27 | `docs/superpowers/plans/*.md` |
| Recorded gotchas | 13 | numbered items in CLAUDE.md's "Gotchas that still bite" |
| CI workflows | 3 | `.github/workflows/*.yml` |
| Backends | 3 | Gmail, Microsoft, LocalFS |

**Rule for the page:** a number appears only if this table backs it. If a
figure cannot be reproduced by the command beside it, it does not ship.

**Presentation caveat.** Line counts and commit counts measure volume, not
quality, and the page must not imply otherwise. They are framed as scale of
surface area — the thing that makes the *test* and *spec* counts meaningful
— not as achievement in themselves.

## 6. Diagrams

Five diagrams, hand-authored SVG, **inlined into the HTML** rather than
referenced via `<img>`.

Inlining is the load-bearing decision. `Jodd-Homepage/css/style.css`
already defines the palette as custom properties (`--ink`, `--ink-soft`,
`--rule`, `--paper-raised`, `--paper`) with a `prefers-color-scheme: dark`
override. An inlined SVG inherits those; an `<img src="…svg">` is a separate
document that cannot see them. Inlining therefore gives correct light/dark
behaviour for free and **introduces no new colour decisions** — the
diagrams are on-brand by construction.

Constraints for all five:

- Colour only from existing custom properties. No new hex values.
- The signature `--grad` is reserved for the site's existing accent use and
  is not spent on diagram chrome.
- Every label is real: actual symbol names (`apply_local_edit`,
  `mark_pushed`, `reconcile_one`) and actual file paths. No invented boxes.
- `role="img"` with an `<title>` and a `<desc>`, so the diagram is not
  invisible to a screen reader.
- Legible at 360 px wide. Each sits in an `overflow-x: auto` wrapper so a
  wide diagram scrolls inside itself rather than making the page scroll.

### D1 — The local-first write path

**Claim it supports:** the user never waits for the network.

Two flows on one canvas, separated by a vertical **latency boundary** line:

- *Left of the line (synchronous, sub-millisecond):* keystroke →
  optimistic DOM update → `apply_local_edit` → SQLite transaction →
  `sync_state = dirty` → control returns to the user.
- *Right of the line (asynchronous, 5-second tick):* sync worker →
  `list_dirty` → `Box<dyn Vertical>` → backend → `mark_pushed` →
  `sync_state = clean`.

The visual argument is that **the user's path never crosses the boundary.**
Annotate the doctrine line from CLAUDE.md: *any normal navigation or
editing command that blocks on the remote is a bug.*

### D2 — The vertical abstraction

**Claim it supports:** the abstraction is real, not aspirational.

A shared core (SQLite cache, sync worker, conflict reconciler,
`AppleHtmlDeriver`, `mime822`) sitting on a single trait seam
(`Box<dyn Vertical>`), with three implementations below it, each annotated
with the one thing that makes it genuinely different:

| Vertical | Transport | Save semantics | Auth |
|---|---|---|---|
| `GmailVertical` | Gmail REST | insert-new + trash-old | OAuth 2.0 + PKCE |
| `MicrosoftVertical` | Graph (Exchange) | `PATCH` in place | OAuth 2.0 + PKCE |
| `LocalFsVertical` | filesystem | write file + remove old | none — a directory exists |

The point to land: three transports with *different auth models, different
identity anchors, and different write semantics* share one cache, one
conflict model, and one sync worker. `mime822` is reused by two of them and
deliberately not by the third.

### D3 — Sync states and keep-both conflict resolution

**Claim it supports:** concurrent edits are handled by a stated policy, not
by last-write-wins.

State machine over `clean | dirty | pull_needed | conflict |
deleted_pending`, with the keep-both branch drawn explicitly: on conflict,
a second row is created with a fresh UUID and a
`(conflict from {Device} {Date})` title suffix carrying the local content,
while the primary row converges to remote. **Neither version is discarded.**

Include the in-flight guard (`AppState.pushing`) as an annotation on the
poll edge, since it is the reason self-induced false conflicts do not occur.

### D4 — Trust boundaries

**Claim it supports:** enterprise-grade data handling, and the honesty
about where it stops.

Four zones, left to right, with what crosses each line labelled:

1. **Device** — SQLite cache encrypted at rest (AES-256, SQLCipher); key in
   the OS credential store (macOS Keychain / Windows Credential Manager /
   Android Keystore); OAuth refresh tokens in the same store; access tokens
   in process memory only; `accounts.json` metadata in plaintext.
2. **Network** — TLS. OAuth 2.0 with PKCE.
3. **Provider** — Gmail or Microsoft Graph. The user's own account.
4. **Apple devices** — iPhone / Mac reading the same store.

Two labels must appear, both stated on the site already and both worth
making visual:

- **BBMedia receives no copy of any note.** There is no Jodd server in this
  diagram, and its absence is the point.
- **A Local Folder vault is deliberately plaintext.** Encryption does not
  cover it, because a directly-readable folder of files is the entire
  purpose of that backend. Drawn as a labelled exception, not omitted.

### D5 — The AI operating loop, with artifact counts

**Claim it supports:** the four-step model is a real pipeline that leaves
artifacts, not a slide.

The existing Frame → Delegate → Surface decisions → Own the result loop,
redrawn with what each step deposits and where the human gates sit:

```
Frame ──▶ design spec (24)     ── human gate: spec approved
      ──▶ implementation plan (27)
Delegate ──▶ bounded agent tasks
Surface ──▶ review findings     ── human gate: findings triaged
Own ──▶ 878 automated tests
     ──▶ 3 CI workflows          ── machine gate: Android encryption
                                    proof runs on a real emulator and
                                    blocks publication
     ──▶ 38 tagged releases
```

The on-device Android encryption gate is worth its own callout: it verifies
SQLCipher on a real Android runtime and gates *publication* rather than
*merge*, which is a distinction most teams never draw.

## 7. Page changes — `Jodd-Homepage/case-study.html`

Existing prose is preserved. Sections renumber to accommodate two
insertions.

| # | Section | Change |
|---|---|---|
| — | Hero / At a glance | **Add** a metrics strip beneath the existing `<dl>`: commits, span, tests, specs, releases. Each figure carries the measurement date, and the strip links to the *How these numbers were measured* section of `ENGINEERING-PRACTICE.md` (§8) — the numbers are only an asset if the reader can check them. |
| 01 | Starting point | Unchanged. |
| 02 | What shipped | Unchanged prose. Update the four cards' links from `v0.23.1` to `v0.24.1`. |
| **03** | **How it is built** | **New.** D1 + D2, two claim sentences, one `<details>`. |
| **04** | **How the data is handled** | **New.** D3 + D4, plus the plaintext-vault exception stated plainly. |
| 05 | AI operating model | Existing loop preserved; **add** D5 and a new *"A decision, worked"* block — the Microsoft folder-writes case. Existing "what this does not claim" boundary stays as the section's closer. |
| 06 | Open learning trail | **Add** a link to the new `ENGINEERING-PRACTICE.md`. Repoint existing links to `v0.24.1`. |
| — | Work with BBMedia | Unchanged. |
| — | Nav | Add anchors for the two new sections. |

### The worked decision (section 05)

One case, told in full, because a single traceable decision is more
persuasive than four claimed ones:

> **Question:** can Jodd create folders in Apple Notes over a Microsoft
> account?
> **What was tried:** both Graph folder-creation surfaces
> (`POST /me/mailFolders`, `POST /me/mailFolders/{id}/childFolders`),
> against a live account.
> **What was found:** neither can set the `IPF.StickyNote` container class
> Apple's sync requires, and the class is immutable after creation —
> `PATCH` returns `500 ErrorObjectTypeChanged`.
> **The control that proves it is not a Jodd bug:** folders created *by
> hand* in Notes.app work immediately, and notes written by Graph into a
> hand-made folder reach Apple without issue. The variable is folder
> provenance, not note writing.
> **What shipped:** `Capabilities::for_backend(Microsoft).writes.folders`
> is `false` — permanently. The implementation exists and is unit-tested;
> the capability gate keeps it off.
> **Why this is the interesting part:** the codebase records the boundary
> of its own competence, in a place the compiler enforces, rather than
> shipping a feature that fails quietly on a user's iPhone.

This is the section that distinguishes the page from every other "we use
AI" page, so it gets the most careful writing.

### CSS

New rules append to `css/style.css` following existing conventions
(`case-*` prefix, custom properties only). Expected additions: `.diagram`
wrapper with `overflow-x: auto`, `.metrics-strip`, `.detail-block` styling
for `<details>`, `.decision-block`. No new colour tokens.

No JavaScript. `<details>` is native. `js/reveal.js` is untouched.

## 8. Documentation changes

### `public-mirror-prep/ENGINEERING-PRACTICE.md` — new

The public account of **how the system got built**, complementing
`ARCHITECTURE.md`'s account of **how the system works**. This split matches
the two things the showcase is asked to demonstrate: the artifact, and the
method.

Contents:

1. The operating loop, with the artifact counts from §5.
2. Why a spec precedes a plan, and what each is for.
3. What the human gates are and what has actually been caught at them.
4. Verification: the merge gate versus the release gate, and why the
   Android encryption proof is the latter.
5. Two worked decisions: the Microsoft folder-writes case above, and the
   Android OAuth redirect chain (gotcha #8) — four constraints of which
   three are invisible from the vendor documentation, including a method
   Google's own Android guide still describes and Google's servers now
   reject, and a loopback listener that completed a sign-in on Android 13
   and was killed mid-consent on Android 16. Its lesson generalises past
   this codebase: *a port verified on one Android device is not verified.*
6. **How these numbers were measured** — the table from §5, reproduced with
   its commands, so every figure on the case-study page is checkable. This
   section is the link target for the page's metrics strip.
7. An honest limits section — what this model costs and where it does not
   apply.

Diagrams here use **Mermaid**, not the inlined SVG. GitHub renders Mermaid
natively in Markdown, which avoids plumbing an asset path across the
public-snapshot boundary — a path that would break silently at snapshot
time. This means D1, D2 and D5 exist in two representations. That
duplication is accepted deliberately: the alternative fails quietly, and
this one does not. The Markdown docs carry only the three diagrams that
earn their place there, not all five.

### `public-mirror-prep/ARCHITECTURE.md` — extend

Two sections appended, neither currently present:

- **At-rest encryption and trust boundaries** — what `db_crypto.rs` does,
  where the key lives, what the plaintext-vault exception is and why it is
  deliberate.
- **The capability model** — `Capabilities`, `Writes`, `SaveSemantics`, and
  why a permanent `false` is expressed in code rather than in a comment.

### `public-mirror-prep/scripts/sync-to-public.sh` — one line

`ENGINEERING-PRACTICE.md` is appended to `PUBLIC_DOCS[]`. Without this the
document never reaches `Jodd-public` and every link to it from the case
study 404s.

## 9. Verification

Documentation has no test suite, so verification is explicit:

1. **Numbers** — re-run every command in §5 and diff against the table.
   Any drift is fixed before publishing.
2. **Links** — every URL in the changed page and both documents is
   requested; all must resolve. The `v0.23.1` → `v0.24.1` repointing makes
   this mandatory, not optional.
3. **Symbols** — every symbol named in a diagram or a `<details>` block is
   confirmed to exist at the path given, by reading the file.
4. **Themes** — the page is rendered at both `prefers-color-scheme` values
   and all five diagrams checked for legibility in each.
5. **Widths** — rendered at 360 px and 1280 px; the page body must not
   scroll horizontally at either.
6. **Snapshot** — `sync-to-public.sh` is run in inspect mode (declining the
   push) to confirm `ENGINEERING-PRACTICE.md` lands and nothing from
   `EXCLUDES` leaks.

## 10. Risks

| Risk | Mitigation |
|---|---|
| Numbers go stale as the repo moves | §5 records the command beside each figure, so refreshing is mechanical. The page states the measurement date. |
| The page becomes too long to finish | Two layers, and every section reads complete without expanding. Length lands in the collapsed layer. |
| Diagram duplication (SVG + Mermaid) drifts | Only three diagrams are duplicated, and §9.3 checks symbols in both. Accepted knowingly over a silently-breaking asset path. |
| Reads as marketing and loses the page's credibility | Every claim added is backed by a reproducible number, a real symbol, or a documented decision. The existing "what this does not claim" boundary stays. |
| Work spans three repos and gets out of sync | The doc changes and the site changes are independent — links point at `Jodd-public`, so the docs must land there **first**, then the page. Ordering is fixed in the plan. |
