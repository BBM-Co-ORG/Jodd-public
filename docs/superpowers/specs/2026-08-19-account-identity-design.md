# Account identity: `{backend}:{email}`

**Status:** design approved 2026-08-19, not yet implemented.
**Scope:** identity only. Adding `BackendKind::ICloud`, any CloudKit code, and
the webview sign-in are the *next* project and are explicitly out of scope here.

## Goal & framing

`Account.id` is the email address (`accounts.rs:96`, `// = email`), and it keys
the OS credential store (`rt::{account_id}`, `accounts.rs:299`), `accounts.json`,
and the primary key of seven SQLite tables. So the system's real invariant is
not "one account per address per backend" — it is **one account per address,
full stop**. Two accounts that share an address cannot coexist at all.

That is a live limitation today, not a hypothetical one: you cannot add
`kaiwan.h@live.com` on both Microsoft and Gmail. It became urgent because an
Apple ID is frequently an ordinary email address — the probe account
(`docs/PRIOR-ART.md`) signs into iCloud as `jodd.demo@gmail.com`, which is
already a Gmail account in this profile.

**This is not a new scheme; it is an unfinished one.** LocalFs already mints
`localfs:{uuid}` (`lib.rs:1516`), so `Account.id`'s own doc comment is already
false for one backend. This project makes Gmail and Microsoft consistent with
LocalFs.

## Decisions locked in brainstorming (2026-08-19)

1. **`{backend}:{email}` system-wide**, not a namespace applied only to new
   backends. An asymmetric scheme was rejected: it would leave the collision in
   the primary key and simply defer the migration.
2. **Sequenced before the iCloud vertical**, as its own spec/plan/PR. The
   migration rewrites the PK of tables holding live Gmail and Microsoft notes
   and delivers nothing user-visible on its own; a bug in it damages working
   accounts, whereas a bug in a new read-only backend cannot. Keeping the two
   apart means a failure has one obvious cause.
3. **One transactional rewrite** (approach A). Two alternatives were rejected:
   composing the id only in memory (leaves the collision in the PK — it looks
   cheap because it does not fix the problem), and a dual-written `account_key`
   column (two sources of truth for identity, for a database holding two
   accounts).
4. **Keychain migrates lazily with fallback**, never eagerly. A failed eager
   migration logs the user out of a working account.
5. **Backend shown wherever an account is named**, unconditionally rather than
   only when two accounts collide.

## Component A — the identity function

```rust
pub fn account_id_for(kind: BackendKind, email: &str) -> AccountId
```

Takes `BackendKind`, never a string, so a typo cannot mint a new namespace.
Every producer calls it: new-account creation, migration #19, and tests.
Nothing hand-writes `"gmail:"`. This is `derive_workflow_kind`'s discipline
(gotcha #4) — one source of truth, so the migration and the live path cannot
drift into disagreeing about what an id looks like.

A companion predicate decides idempotency:

```rust
fn is_qualified(id: &str) -> bool
```

True when `id` starts with a known `BackendKind` prefix followed by `:`. This is
what makes migration #19 safe to re-run, and it is what makes LocalFs a no-op
rather than producing `localfs:localfs:{uuid}`.

`Account.email` is unchanged and remains the only thing presented to a user as
an address. On LocalFs it already holds a display name rather than an email;
that stays true and is not this project's problem to tidy.

## Component B — migration mechanism

The runner is `&[(i64, &str)]` — pure SQL strings (`db.rs:450`). Migration #19
cannot be expressed in it, because it must know each account's backend kind and
that lives in `accounts.json`, not the database.

The runner therefore grows exactly one variant:

```rust
enum Step {
    Sql(&'static str),
    Rust(fn(&Connection) -> SqlResult<()>),
}
```

Existing entries become `Step::Sql(...)` mechanically — no behaviour change. A
parallel migration system running outside `migrate()` was considered and
rejected: it would split "what migrations exist" across two places, and the
`migrations` table would no longer be the whole answer.

### Migration #19

**The runner holds no transaction** — verified 2026-08-19: the apply loop
(`db.rs:895`) is `conn.execute_batch(sql)` per migration in autocommit, and the
version row is a *separate* `INSERT` after it. So a crash between the two
re-runs a completed migration on the next start. Migration #19 therefore opens
its own explicit transaction (`Connection::unchecked_transaction()`), and its
idempotency is **load-bearing rather than defensive** — the runner can genuinely
hand it a database it has already converted.

Inside that transaction:

1. Load `accounts.json`.
2. For each account whose id is **not** already qualified, rewrite `account_id`
   in all seven tables.
3. Re-derive FTS from `notes`.
4. Rewrite `accounts.json` with the new ids (atomically — temp file + rename).

The seven tables, all of which carry `account_id` in their PRIMARY KEY:

| Table | Primary key |
|---|---|
| `notes` | `(uuid, account_id)` |
| `folders` | `(account_id, path)` |
| `note_tags` | `(account_id, uuid, tag)` |
| `tag_tombstones` | `(account_id, uuid, tag)` |
| `attachments` | `(account_id, note_uuid, content_id)` |
| `edges` | `(account_id, src_uuid, dst_id, dst_title, rel)` |
| `note_uuid_aliases` | `(account_id, old_uuid)` |

Indexes that embed `account_id` follow their tables and need no separate
handling: `idx_notes_account_label`, `idx_notes_pinned`, `idx_note_tags_tag`,
`idx_tag_tombstones_uuid`, `idx_attachments_note`, `idx_edges_dst_id`,
`idx_edges_dst_title`, `idx_edges_src`.

**`notes_fts` is an eighth carrier and must not be overlooked.** It declares
`account_id UNINDEXED` (`db.rs:678`), so a rewrite that stops at the seven base
tables leaves every search result pointing at an account id that no longer
exists. It is not updated in place: FTS content is *derived* from
`notes.title` + `notes.body_html`, so it is re-derived from `notes` after the
rewrite. That is gotcha #4's rule applied to the one table here that has a
truth source elsewhere.

Three properties:

- **Idempotent.** Driven by `is_qualified`, so a crash mid-run cannot
  double-prefix, and LocalFs rows are skipped on the first run.
- **Fails closed.** An `account_id` present in the database with no matching
  entry in `accounts.json` aborts the transaction rather than guessing a
  backend. Orphan rows are a real state — `remove_account` and the Draining
  paths (gotcha #2) can leave them — and guessing would silently misfile
  someone's notes under the wrong account.
- **No rollback path, and the migration comment says so.** A downgrade after
  this migration is unsupported. Stated rather than half-built.

### The transaction does not cover `accounts.json`

`accounts.json` is a file on disk, so it cannot join the SQLite transaction.
Whichever order the two writes happen in, a crash between them leaves the
database and the account list disagreeing — and the fail-closed rule above
would then refuse to open the app at all, turning a recoverable interruption
into a wedge.

**The mapping is therefore resolved by `email`, not only by the old `id`.** A
bare `account_id` in the database equals the `email` of exactly one account, so
the backend kind is recoverable from `accounts.json` whether or not that file
has already been rewritten. This makes the step re-runnable from either
half-finished state rather than depending on write order:

- DB rewritten, JSON not → next run matches the remaining bare rows by email
  and finishes the job.
- JSON rewritten, DB not → the same lookup still resolves, because `email` did
  not change.

`accounts.json` is written temp-file-plus-rename so it is never observed
half-written. The fail-closed abort therefore fires only for a genuinely
orphaned `account_id` — one matching no account by id *or* by email — which is
the case it was meant for.

## Component C — credential store

The key becomes `rt::{backend}:{email}` — that is, `rt::{account_id}` continues
to be the rule, and it changes only because `account_id` changed.

**Reads try the new key, then fall back to the bare-email key. The next
successful save writes only the new key.** There is no migration moment, so a
denied or unavailable keychain can never log a user out of a working account.
Stale bare-email entries linger and are harmless.

This must respect gotcha #15. The fallback read happens **only** on a miss in
`accounts::RT_PRESENT`; the steady state stays at zero extra credential-store
reads per `is_authenticated` poll. `RT_PRESENT` lives beside the write
functions so invalidation stays structural — the fallback must not introduce a
second cache anywhere else.

## Component D — display

Wherever an account is named — Sidebar, AccountSettings, the removal warning —
the backend appears alongside the address, **unconditionally**. Not only when
two accounts collide.

Two sidebar rows both reading `jodd.demo@gmail.com` with nothing to separate
them is precisely the state this project makes reachable, so the UI should not
wait for a collision to become legible. `backend_kind` already reaches the
frontend in the `list_accounts` response, so this is presentation only.

## Verification

**Unit**
- `account_id_for` produces the expected shape per `BackendKind`.
- `is_qualified` is true for every `BackendKind` prefix and false for a bare
  email; `localfs:{uuid}` is already qualified.
- Migration #19 run twice leaves ids unchanged after the first run.
- A row whose `account_id` matches no account by id *or* by email aborts the
  transaction and leaves the database untouched.
- A half-finished state (database rewritten, `accounts.json` not, and the
  reverse) is resolved on the next run rather than aborting.
- `notes_fts` rows carry the new `account_id` after migration; a search
  performed immediately afterwards returns the note.
- Credential fallback: reads the old key when the new one is absent, and the
  next save writes only the new key.
- The `RT_PRESENT` fast path still performs zero credential reads on a cache
  hit (the gotcha #15 measurement — count reads, not dialogs).

**Live, on the maintainer's real profile — this is the one that matters.**
The risk here is to data that already exists, not to new code. After migration,
both existing accounts (`gmail:jodd.demo@gmail.com`,
`microsoft:kaiwan.h@live.com`) must still sync, still show every note and
folder, and must not re-prompt for sign-in.

**Gates** — the set CI runs, not a narrower one:

```
cargo test --workspace
node scripts/gen-changelog.mjs
npx vitest run
npx svelte-check --threshold error
npm run build
```

## Implementation order

1. `account_id_for` + `is_qualified`, with tests. No call sites changed yet.
2. Runner gains `Step`; existing migrations become `Step::Sql`. Pure refactor,
   green tests.
3. Migration #19, with its idempotency and fail-closed tests.
4. New-account creation switches to `account_id_for` (Gmail, Microsoft).
5. Credential fallback.
6. Display.
7. Live verification on the real profile.

## Deferred (door open, not built)

- `BackendKind::ICloud` and everything CloudKit — the next project.
- Tidying LocalFs's `email` field, which holds a display name rather than an
  address.
- Any downgrade/rollback support.
