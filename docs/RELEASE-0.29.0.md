# Release 0.29.0 — preparation and evidence

Requested by the owner on 2026-09-20 after Packages A–H were committed and pushed.
Implementation baseline: `50d25f9`. This request explicitly authorizes the version
bump, release commit, push and normal tagged binary publication workflow.

- All five version manifests/lockfiles are aligned to 0.29.0; CHANGELOG has the
  matching section used by the workflow and in-app What's New.
- [UI acceptance cases](UI-ACCEPTANCE-0.29.0.md) cover all eight packages in Thai,
  with a browser launcher at `tests/browser/acceptance.html`. Its buttons control
  synthetic pending replies, permission revocation, versioned sync, cancellation
  and stale-target errors without using the Console or a real provider/account.
- Host verification after version bump: 638 frontend tests, Svelte check 0 errors
  / 15 existing warnings, frontend build, and 1,464 Rust workspace tests including
  examples and MCP. Version agreement, changelog extraction and whitespace checked.
- Chrome 153.0.8010.52 passed all eight launcher pages and their manual
  simulation controls; no page errors or live AI. The test scrolls the parent
  page to the embedded fixture before interacting with its modal.
- Baseline GitHub CI run 35464992491 succeeded. The release commit's own CI and
  tagged Release run must be checked separately; this document does not assert
  publication or Android encryption success before those jobs finish.
- Desktop code signing remains ad-hoc on macOS and unsigned on Windows. Android
  remains a Developer Preview. Existing `android-encryption` publication gate is
  retained without bypass. macOS Apple Silicon, Windows, signed universal Android
  APK and the existing MCP distributions use the normal release workflow.

No real AI call, provider connection test or modification of real notes, accounts,
credentials or settings was performed. Clean-account native OAuth, native
cross-platform UX, actual-device round trips and Apple delayed reconciliation
were not exercised for this version. Browser demos and host tests do not replace
those manual smoke checks; they are explicitly left unverified in release notes
and acceptance cases. No provider quality/cost/default or human time-saving claim.

The public source snapshot is separate from binary release publication. Its
sanitizer now also excludes AGENTS, the Codex handoff and the two internal review
reports introduced alongside these packages. The snapshot script must show its
scan/review output and obtain confirmation before replacing public main history,
as required by RELEASE.md; no automatic affirmative input is authorized here.
