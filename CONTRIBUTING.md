# Contributing to Jodd

Thanks for your interest. This document explains how the project is
organized and what to expect when you contribute.

## Repository model

Jodd uses a **private-upstream / public-mirror** model:

- **Public repository** (this repo, `BBM-Co-ORG/Jodd-public`) is a
  periodic sanitized snapshot of the upstream. PRs land here.
- **Private upstream** is where day-to-day development happens. It
  contains internal notes, in-flight experiments, and commit history
  that is not appropriate to share publicly.

When your PR is accepted:

1. A maintainer cherry-picks the patch into the private upstream,
   preserving your authorship (`git cherry-pick -x` carries the
   "(cherry picked from commit …)" trailer).
2. Your PR is closed (not merged into the public branch directly).
3. The next snapshot push to the public repo will include your change,
   with your name in the commit history.

This means **the public main branch may rewind** when a new snapshot
is pushed. Don't base long-running forks on it; rebase frequently, or
ask for a topic branch if you need stability.

## Before you start

For anything larger than a small bug fix:

1. **Open an issue first** describing what you want to change and why.
   This avoids you spending a weekend on something that conflicts with
   in-flight upstream work.
2. Wait for a maintainer reply confirming the change is wanted and
   that the design is acceptable. We try to respond within a few days.
3. Reference the issue number in your PR description.

## What kinds of changes are easiest to land

- **Bug fixes with a clear reproduction.** A failing test (or a clear
  set of manual reproduction steps if the area is hard to test) helps
  a lot.
- **Documentation improvements** — README clarifications, typo fixes,
  build instructions for new platforms.
- **Platform support fixes** — Linux build issues, Windows-specific
  bugs, accessibility improvements.
- **Provider work behind the existing `gmail` module's shape** — if
  your work fits the existing module structure cleanly, it's easier
  to review.

## What is harder to land without a discussion first

- **New providers** (Outlook/Graph, Yahoo, custom IMAP). Provider work
  needs to slot into a `Provider` trait abstraction that does not yet
  exist; the upstream team plans to introduce it before the second
  provider lands. Coordinate before starting.
- **Schema migrations** to the SQLite cache (anything that adds a
  migration number).
- **Sync-state machine changes** — adding new states, changing
  transition semantics. These are correctness-critical and require
  careful end-to-end testing against a real Apple Notes round trip.
- **UI/UX redesigns** beyond a single screen.

## Coding style

- **Rust**: `cargo fmt` and `cargo clippy --all-targets` clean before
  submitting. We do not turn clippy warnings into errors in CI, but
  PRs with new warnings are likely to bounce.
- **TypeScript/Svelte**: keep the existing patterns. We use Svelte 5
  runes (`$state`, `$derived`, `$effect`) in new components; existing
  components mix runes and legacy reactivity — match the file you're
  editing.
- **Comments explain *why*, not *what*.** The codebase already has
  detailed inline rationale for non-obvious decisions; keep that style.

## Commit messages

- One concern per commit.
- Subject line under 70 characters, imperative mood
  ("Add X" not "Added X").
- Body explains the **why** and any non-obvious **how**.
- Sign your commits with your real name and email
  (`git config user.name`, `git config user.email`).
- We use the
  [Developer Certificate of Origin](https://developercertificate.org/)
  — by signing off your commit (`git commit -s`) you confirm you have
  the right to submit the contribution under Apache 2.0.

## Testing your change

Before opening a PR:

1. **Build clean**: `npm run tauri build` succeeds on your platform.
2. **Manual round trip**: if your change touches sync, edit, folder
   ops, or pin, verify against a real Gmail account that:
   - Edits made in Jodd appear in Apple Notes on iPhone/Mac.
   - Edits made in Apple Notes appear in Jodd after the next poll.
   - At least one edge case (e.g. conflict, offline edit, folder move).
3. **Don't commit your `.env`** — it contains your Google client
   credentials. The repo's `.gitignore` excludes it; double-check
   `git status` before pushing.

## Reporting bugs

Open an issue with:

- **Platform** (macOS / Windows / Linux + version).
- **Jodd version** (visible in the about screen / from the binary name).
- **Steps to reproduce.** A failing reproduction is worth ten well-
  written paragraphs.
- **What you expected** vs **what happened**.
- Log output if you can get it. Jodd does not currently write a log
  file; running `npm run tauri dev` in a terminal surfaces console
  output.

**Security issues**: please follow [SECURITY.md](SECURITY.md) instead.
Do not file public issues for security reports.

## License

By contributing, you agree that your contributions will be licensed
under the [Apache License 2.0](LICENSE). The patent grant in section 3
of the license is intentional and important — please don't try to
exempt your contribution from it.

## Code of conduct

Be kind. Assume good faith. Critique code, not people. Maintainers
reserve the right to lock or close discussions that become
unproductive.
