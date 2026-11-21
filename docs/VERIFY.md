# Verification gates — the evidence

> Moved out of `CLAUDE.md` on 2026-10-16, verbatim, the same way
> [GOTCHAS.md](GOTCHAS.md) was on 2026-09-11: `CLAUDE.md` had grown past its own
> ~40 000-character budget. `CLAUDE.md` keeps the commands and one line per rule;
> this file keeps the failures that made each rule. Read it before changing
> `ci.yml`, a gate, or the commands an agent is told to run.

## Verify with the commands CI runs, not narrower ones

```bash
cargo test --workspace              # NOT `cargo test -p jodd --lib`
node scripts/gen-changelog.mjs      # FIRST on a fresh clone/worktree — see below
npx vitest run
npx svelte-check --threshold error
npm run build
```

**The `gen-changelog.mjs` line is not optional scaffolding — CI runs it as its own
step**, before both `vitest` and `svelte-check` (`ci.yml`). It writes
`src/lib/generated/changelog.json`, which is gitignored and which `whatsNew.ts`
imports directly. The `predev`/`prebuild` npm hooks create it, so anyone who has
run the app locally already has it and will never see the problem — but in a
**fresh clone or a new git worktree** it does not exist, and `svelte-check`
reports `Cannot find module './generated/changelog.json'`. That error is an
artifact of the missing generate step, not a real type error: run the line above
(or `npm run build`) and it goes to 0 errors. Omitting it from this list made the
documented gate set disagree with the merge gate in exactly the direction the
heading above warns about.

`cargo test -p jodd --lib` does not compile `src-tauri/examples/`, and neither does
`cargo build --workspace`. CI runs workspace-wide `cargo test`, which does. On
2026-07-30 a signature change left `examples/ask_jodd_probe.rs` uncompilable; eleven
task reviews, a whole-branch review and a fix wave all passed, because the plan
specified the narrower command. It only surfaced on `main`, as red CI. **A gate that
differs from the merge gate only proves it agrees with itself.**

**`protoc` is deliberately absent from the build, and the drift check does not
need it either.** `src-tauri/src/backend/icloud/gen/` is generated Rust that is
**committed**, so no codegen runs during a build — which keeps a protobuf
compiler off every developer machine, out of CI, and out of the Android NDK
cross-build. `icloud_gen_is_current` (in that module) regenerates and diffs using
`protox`, a protobuf compiler written in Rust, so it runs everywhere instead of
skipping wherever `protoc` happens to be missing. **A guard that skips on the
machine where the edit is made is not a guard.** After editing anything in
`proto/`:

```bash
JODD_UPDATE_PROTO_GEN=1 cargo test -p jodd --lib icloud_gen_is_current  # rewrite
cargo test -p jodd --lib icloud_gen_is_current                          # verify
```

Generating and checking are the **same function** on purpose — a separate
generator binary would be a second place that knows the codegen settings, which
is the defect this test exists to catch, one level up. (It would also have to
live in `examples/`, per gotcha #3.)

**On Windows, run cargo from PowerShell, not from a Git Bash shell.** The
vendored OpenSSL build shells out to `perl`, and an MSYS/Git-Bash `PATH` puts
its own perl first — which lacks `Locale::Maketext::Simple`, so `./Configure`
dies with `Can't locate Locale/Maketext/Simple.pm in @INC` and the build fails
at `openssl-sys` before compiling a line of Jodd. The Windows `PATH` finds
Strawberry Perl and it builds. **The failure names OpenSSL and a Perl module,
so it reads as a missing dependency rather than a wrong shell**, which is the
detour it cost on 2026-09-07.

Nine `cargo test -p jodd --lib` tests also fail on Windows and are **not**
regressions: seven `llm::agent_cli` ones (their fixtures are shell scripts),
`llm::http::a_refused_connection…`, and `icloud_gen_is_current` (most likely
CRLF against the committed codegen). CI is `ubuntu-latest`, so it never sees
them. Verify a Windows change against that baseline rather than against zero.

**There is a second gate the command above cannot reproduce, and its name is
misleading.** `cargo test --workspace` *does* run
`db_crypto::tests::android_round_trip_key_encrypt_reopen` — on the **host**, via the
tempdir branch. Passing it locally says nothing about Android: the whole point is
whether vendored OpenSSL + SQLCipher, cross-compiled with the NDK, actually encrypt
and reopen on a real Android runtime. That proof only exists on an emulator, in
`.github/workflows/android-encryption.yml`, and it **blocks release publication** —
see gotcha #13. A PR touching `db_crypto.rs` or the Cargo manifests (beyond Jodd's own
version line) triggers it; run it yourself with `gh workflow run
android-encryption.yml` (~13 min) rather than dispatching the whole release pipeline.

## What CI skips, and why that is not a weaker gate

Since 2026-10-25 (the Actions-budget PR), `ci.yml` does not run on a change
that touches only `docs/**` or Markdown files. **`CHANGELOG.md` is the
exception**: `gen-changelog.mjs` turns it into the `changelog.json` that
`vitest`, `svelte-check` and the build import, so a CHANGELOG-only change still
runs everything. A gate that starts reading another doc must add that path back
to both `paths:` lists in `ci.yml`, or a change to only that doc will skip it.
No check on `main` is required (there is no branch protection), so a docs-only
PR simply shows no checks.

On a push to `main` only the `rust` job runs. It is the job that saves the
Rust cache every PR restores, and the only check on the tree `main` actually
has: nothing makes a PR be up to date before it merges, and on 2026-10-11 #182
passed, merged 15 s after another PR, and `main` stopped compiling (`E0061`). The frontend job on
`main` re-tested the same tree its PR had just passed and kept nothing for later.
