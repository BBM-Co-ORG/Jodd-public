# REST vs IMAP-XOAUTH2 — backend protocol decision

**Status:** Decided. Gmail REST API is the chosen backend protocol.
**Originally landed:** commit 6b3aa03 (development-momentum decision, doc never
written at the time).
**Re-evaluated:** 2026-06-15 — decision reaffirmed on the merits, not just
on momentum. This document captures the reasoning so future-us doesn't
re-litigate it without new information.

## TL;DR

Jodd uses **Gmail REST API** (not IMAP with XOAUTH2) to talk to Gmail.
The `Provider` trait abstraction on the roadmap (CLAUDE.md edge #1) will
be REST-shaped, so Microsoft Graph slots in as a second impl without
contortion. We will NOT switch to IMAP — it does not solve the
verification/scope problem people assume it solves, it costs a full
rewrite of `gmail.rs` plus ~37 callsites in `lib.rs`, and it makes the
Microsoft path harder, not easier.

## The trap: IMAP doesn't fix verification

The most common reason to consider switching is "maybe IMAP avoids the
unverified-app / CASA review." It does not.

| Scope                                                   | Tier        | CASA? |
|---------------------------------------------------------|-------------|-------|
| `gmail.modify` (what Jodd uses today, REST)             | Restricted  | Yes   |
| `https://mail.google.com/` (required for IMAP/SMTP)     | Restricted  | Yes   |
| `gmail.readonly`                                        | Restricted  | Yes   |
| `gmail.send`                                            | Sensitive   | No    |

To talk IMAP to Gmail at all you must request `https://mail.google.com/`,
the broadest restricted scope. Switching backends keeps you in the
restricted tier (same CASA process) and arguably makes the review story
harder ("why do you need full mailbox + SMTP?").

**If verification cost is the motivation: switching is the wrong lever.**
The right levers are Testing mode (free, 100 test users, 7-day refresh-
token expiry) or a Bring-Your-Own-Client (BYOC) setup flow where each
user creates their own Cloud project. Neither requires changing the
protocol.

## What IMAP would actually buy

1. **Apple Notes wire-format parity.** Apple Notes writes notes *via*
   IMAP, so the X-headers / multipart layout / label-as-folder semantics
   map 1:1. Less impedance mismatch than Gmail REST.
2. **IDLE = push.** Real push instead of 5s tick + 10min poll. Faster
   cross-device sync, lower API quota burn.
3. **No "no REPLACE" hack.** IMAP `APPEND` + `STORE \Deleted` + `EXPUNGE`
   replaces Gmail REST's "insert new + trash old + remap cache id"
   dance. Could eliminate `AppState.pushing` bookkeeping and the
   `mark_pushed` id swap.
4. **Multi-provider parity is "free" on the Apple side** — every mail
   backend speaks IMAP. The same code path could in principle talk to
   Fastmail, iCloud-bridged accounts, self-hosted Dovecot, etc.

## What IMAP would cost

1. **Full rewrite of `gmail.rs` + ~37 callsites in `lib.rs`.** At least
   as big as the planned `Provider` trait refactor. Probably bigger:
   IMAP is **stateful** (connections, selected mailbox, sequence
   numbers, IDLE) where REST is stateless. `AppState` grows a
   per-account connection pool.
2. **IMAP-XOAUTH2 token refresh is fiddly.** When the access token
   expires mid-session you must drop the connection, refresh, reconnect,
   re-SELECT, restart IDLE. REST refreshes per request, transparently.
   This is a documented source of bugs in OSS IMAP clients (`mbsync`,
   `offlineimap`).
3. **Sync becomes harder to reason about.** Today's reconciler is
   stateless: "Gmail says X + SQLite says Y → resolve." With IMAP we'd
   track UIDs, UIDVALIDITY changes (which invalidate everything),
   `\Deleted` flag sync, IDLE drop/reconnect cycles. The
   `docs/SYNC-BUGS-2026-06-07.md` correctness pass would be redone from
   scratch.
4. **Read-path multipart parsing becomes ours, always.** Gmail REST
   decomposes parts into a JSON tree on the read path; with IMAP we
   fetch raw RFC 822 and parse MIME on every read, not just on save.
   Our attachments BLOB pipeline (migration #9) was tractable partly
   because REST hands us decomposed parts on read.
5. **Microsoft path gets harder, not easier.** Microsoft is
   **deprecating IMAP** for personal Outlook accounts (basic auth gone;
   OAuth-IMAP support being pulled back) and Exchange Online tenants
   increasingly disable IMAP by default. The "real" Microsoft backend
   is **Graph API** (`/me/messages`), which is REST-shaped. A
   `Provider` trait designed around IMAP would force Microsoft Graph
   into a stateful-connection shape it doesn't want; a REST-shaped
   trait fits both naturally.

## Decision rule

The right shape for the `Provider` trait is **REST**. Gmail (today) and
Microsoft Graph (tomorrow) both speak that shape. If we ever add a
third backend that's IMAP-native (Fastmail, iCloud bridging,
self-hosted), we add a single IMAP impl of the same trait *for that
backend only* — Gmail and Microsoft don't pay the stateful-protocol
tax for someone else's protocol choice.

The Apple-Notes wire-format parity argument turns out to be a category
error: what Apple requires from the wire is the *message format*
(specific X-headers, multipart layout, label-as-folder semantics), not
the *protocol*. Those invariants are protocol-independent — `gmail.rs`
already preserves every Apple invariant via REST. Protocol is
interchangeable; message format is not.

## What we would reconsider on

We'd reopen this decision only on evidence we don't have today:

- **Concrete round-trip bugs that only IMAP could fix** (e.g. specific
  header preservation issues Gmail REST mangles). Today's evidence is
  the opposite: the fidelity work
  ([FIDELITY-Gmail-Apple.md](FIDELITY-Gmail-Apple.md), attachments
  round-trip, header injection) shows REST is faithful.
- **Gmail materially changing its API tier** in a way that flips the
  CASA cost equation. (As of 2026-06, no signal of this.)
- **A backend we want to support that has no REST option.** Hypothetical
  for now; would only justify *adding* an IMAP impl alongside Gmail/Graph,
  not replacing them.

## Recommendation summary

Don't switch. Build the REST-shaped `Provider` trait (CLAUDE.md edge
#1), land Microsoft Graph as the second impl, and treat IMAP as an
addition we'd make *if and only if* a future backend forces it.

For the verification problem that often motivates this question, see
the OAuth verification options: stay in Testing mode (free, capped),
publish + CASA ($$$), or ship BYOC (Bring Your Own Client) as a power-
user setup flow.
