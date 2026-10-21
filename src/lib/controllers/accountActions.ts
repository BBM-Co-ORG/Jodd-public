// Account actions — one implementation shared by every surface that offers
// them: the desktop Sidebar's account panel today, Settings › Accounts next.
// Each body moved here verbatim from Sidebar.svelte (remove, reactivate,
// "Stop waiting"'s status write, the Gmail/Microsoft/iCloud sign-in starts)
// or the old AccountSettings.svelte (deactivate; now settings/AccountPage.svelte),
// so the surfaces cannot drift.
//
// What did NOT move is the surface's own business: its account panel, its
// ConfirmDialog and the revoke checkbox bound into it, and its folder tree.
// A caller hands `removeAccount` a `confirm` and an `onRemoved` for that.
// Local Folder and both SSH flows live in AccountAddHost (always mounted),
// along with the oauth-success post-add refresh — see `startAddAccount`.
//
// Known and deliberately unchanged: open defect D5 — `removeAccount` awaits
// `remove_account` before it mutates any store.
import { invoke } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import {
  accounts, notes, selectedNote, selectedFolder, currentAccount, isAuthenticated,
  refreshNotes, noteIndex, setAccountNoteTags, requestFolderRefresh, accountDisplay,
} from '../stores/notes';
import { addAccountRequest, type HostAddAccountKind } from '../stores/ui';
import type { Account, MessageIndex } from '../types';
import type { ConfirmOptions } from './confirmGate';

// Shape returned by the `count_pending_pushes` command. Declared once —
// both the Inactive-group poll and the remove-account confirmation read it,
// and both must sum the SAME four fields (a field missed in one place would
// read as "nothing queued" exactly where that is most dangerous).
export type PendingPushes = {
  notes: number;
  deletes: number;
  pins: number;
  folders: number;
  // Notes holding unsent content that the worker has STOPPED trying to
  // send (the backend refused permanently). Not queued work — see the two
  // helpers below for why that distinction has to be kept.
  blocked: number;
};
// What the worker still owes the backend. Drives "finishing sync — N left",
// and must exclude `blocked` or that number would never reach zero: nothing
// is going to send those, so the account would sit in Draining forever
// showing a count that never moves.
export const totalPending = (c: PendingPushes): number =>
  c.notes + c.deletes + c.pins + c.folders;
// What the user LOSES if this account is deleted right now. Includes
// `blocked`, because a note the worker gave up on is still the user's
// content and `db.delete_account` wipes it with everything else. Leaving it
// out of the removal warning would hide exactly the notes most likely to be
// unsent — the ones that could not be sent.
export const totalUnsent = (c: PendingPushes): number => totalPending(c) + c.blocked;

/**
 * The body of the "Remove account?" confirmation.
 *
 * Names the account with `accountDisplay`, the same label the sidebar shows.
 * It used to interpolate `a.id`, which for a Local Folder or SSH account is
 * `localfs:<UUID>` / `ssh:<UUID>` — two vaults could not be told apart in the
 * one dialog that destroys one of them (device pass, 2026-09-29).
 *
 * `unsent` is `totalUnsent(count_pending_pushes)`, or undefined when that
 * count could not be read. It is ignored for a draining account: the backend
 * queues that removal until the queue is empty, so nothing is deleted unsent.
 */
export function removeAccountMessage(a: Account, unsent: number | undefined): string {
  const name = accountDisplay(a);
  if (a.status === 'draining') {
    return `Remove ${name} from Jodd? It's still finishing a sync — removal will happen automatically once that's done. Anything still syncing will be sent first, not lost.`;
  }
  const warning =
    unsent === undefined
      ? ' This account may still have unsent edits — the pending count could not be confirmed. Anything unsent will be deleted with it.'
      : unsent > 0
        ? ` ${unsent} unsent edit(s) have not reached the server yet and will be deleted with it.`
        : '';
  return `Remove ${name} from Jodd?${warning}`;
}

/** `remove_account`'s two answers, plus the two ways no removal was sent or
 *  took effect. A caller closes its account panel on anything but
 *  `cancelled`, as the Sidebar always has. */
export type RemoveAccountOutcome = 'removed' | 'queued' | 'cancelled' | 'failed';

/** Asks the user. `checkboxLabel` null means no checkbox; `checked` is the
 *  checkbox's state when the user answered (ignored when there is none). The
 *  caller owns the checkbox, including its default (on, per spec). */
export type ConfirmAccountRemoval = (
  title: string,
  message: string,
  checkboxLabel: string | null,
  options: ConfirmOptions,
) => Promise<{ ok: boolean; checked: boolean }>;

/** Removing an account deletes its local cache: a destructive confirm. */
export const REMOVE_ACCOUNT_CONFIRM: ConfirmOptions = { destructive: true, confirmLabel: 'Remove' };

// Remove ANY signed-in account (not necessarily the active one). If we
// removed the last account, the UI naturally drops back to AuthScreen.
//
// Removal is the one destructive step in the account lifecycle:
// `remove_account` calls `db.delete_account`, which wipes the cache rows —
// including any edit still queued for push. An `Inactive` account USED to
// be safe by construction (the worker only flips it once the queue is
// empty), but "Stop waiting" forces that flip with work still queued, and
// its own confirmation promises those edits "will be sent if you
// reactivate". So this path must CHECK the queue rather than infer it from
// the status — see CLAUDE.md edge #9. Same shape as stopWaiting: an
// unknown count warns too, because a failed count read as 0 would skip the
// one confirmation standing between the user and losing that work.
//
// A `draining` account no longer gets that treatment at all: the backend
// (roadmap #0d) queues the request instead of destroying anything, and
// only performs the real removal once its queue is confirmed empty — so
// there is nothing here for the "will be deleted" warning to apply to.
// `remove_account`'s return tells us which of the two actually happened
// (`RemoveAccountOutcome`, serialized as "removed" | "queued") rather than
// inferring it from `a.status`, which can race the worker's own
// Draining -> Inactive flip between this click and the command's reply.
//
// `onRemoved` is the caller's own cleanup for a removed account (the
// Sidebar drops it from its folder tree), run at the point in the sequence
// where the Sidebar used to do that inline.
export async function removeAccount(
  a: Account,
  deps: { confirm: ConfirmAccountRemoval; onRemoved?: (accountId: string) => void },
): Promise<RemoveAccountOutcome> {
  const accountId = a.id;
  // Managed SSH exactly: Jodd holds the private key (ssh_key set), vs.
  // Advanced SSH's ssh_target-only accounts using the user's own
  // ssh-agent/config — nothing there for Jodd to revoke.
  const isManagedSsh = a.backend_kind === 'ssh' && !!a.ssh_key;
  let n: number | undefined;
  if (a.status !== 'draining') {
    try {
      // totalUnsent, not totalPending: removal destroys blocked notes too,
      // and those are the ones the user is least likely to have another copy
      // of — nothing ever sent them anywhere.
      n = totalUnsent(await invoke<PendingPushes>('count_pending_pushes', { accountId }));
    } catch (e) {
      console.error(`count_pending_pushes failed for ${accountId}:`, e);
    }
  }
  const confirmMessage = removeAccountMessage(a, n);
  const { ok, checked } = await deps.confirm(
    'Remove account?',
    confirmMessage,
    isManagedSsh ? "Also remove Jodd's key from the server" : null,
    REMOVE_ACCOUNT_CONFIRM,
  );
  if (!ok) return 'cancelled';
  try {
    const outcome = await invoke<'removed' | 'queued'>('remove_account', {
      accountId,
      revokeKey: isManagedSsh ? checked : null,
    });
    if (outcome === 'queued') {
      // Still here, still draining server-side — just record the request
      // so the row shows "removal queued" instead of a re-clickable
      // button. list_accounts' periodic poll (refreshPending, while the
      // Inactive group is open) picks up the real disappearance once
      // sync_worker_tick finishes it.
      accounts.update((list) =>
        list.map((x) => (x.id === accountId ? { ...x, pending_removal: true } : x))
      );
      return 'queued';
    }
    const remaining = get(accounts).filter((x: Account) => x.id !== accountId);
    accounts.set(remaining);
    // Drop its notes from the store so the sidebar reflects the removal.
    notes.update((ns) => ns.filter((n) => n.account_id !== accountId));
    deps.onRemoved?.(accountId);
    if (get(currentAccount) === accountId) {
      if (remaining.length > 0) {
        currentAccount.set(remaining[0].id);
        selectedFolder.set('Notes');
      } else {
        currentAccount.set(null);
        isAuthenticated.set(false);
        notes.set([]);
        selectedNote.set(null);
      }
    }
    return 'removed';
  } catch (e) {
    console.error('removeAccount failed', e);
    return 'failed';
  }
}

// ─── Account status ──────────────────────────────────────────────────

// Bumped at the start of every status write. Lets the Sidebar's
// Inactive-group list_accounts poll detect "a status change started or
// finished while my fetch was in flight" and discard its own stale result
// instead of overwriting a fresher optimistic write. Module-level because
// the writes now come from more than one surface; the poll reads it through
// `accountStatusChangeSeq()`.
let statusChangeSeq = 0;
export function accountStatusChangeSeq(): number {
  return statusChangeSeq;
}

// Optimistic-first per the local-first doctrine: flip the in-memory
// `accounts` store's status BEFORE the invoke resolves, so the account
// moves between $activeAccounts and the Inactive group at once rather than
// after a round trip. Roll back on failure, then rethrow so the caller can
// show the error (settings/AccountPage.svelte does) or swallow it (setAccountStatus).
async function writeAccountStatus(id: string, status: string): Promise<void> {
  statusChangeSeq++;
  const snapshot = get(accounts);
  accounts.update((list) => list.map((a) => (a.id === id ? { ...a, status } : a)));
  try {
    await invoke('set_account_status', { accountId: id, status });
  } catch (e) {
    accounts.set(snapshot);
    console.error('set_account_status failed', e);
    throw e;
  }
}

/** Any status write — today "Stop waiting"'s `inactive`. Returns whether the
 *  flip stuck; a refusal is logged and rolled back, never thrown. */
export async function setAccountStatus(id: string, status: string): Promise<boolean> {
  try {
    await writeAccountStatus(id, status);
    return true;
  } catch {
    return false;
  }
}

/**
 * The body of "Stop waiting?". `n` is `totalPending(count_pending_pushes)`,
 * or undefined when that count could not be read.
 */
export function stopWaitingMessage(n: number | undefined): string {
  return n === undefined
    ? 'This account may still have unsent edits — the pending count could not be confirmed. Anything unsent will stay on this device and will be sent if you reactivate this account.'
    : `${n} edit(s) will stay on this device and will be sent if you reactivate this account.`;
}

/** `stopWaiting`'s result: `failed` = the status write was refused (logged
 *  and rolled back); the caller shows it. */
export type StopWaitingOutcome = 'stopped' | 'cancelled' | 'failed';

/**
 * "Stop waiting" on a draining account: flip it to `inactive` with work still
 * queued. Shared by the Sidebar's Inactive group and Settings › Accounts'
 * account page, so the rule and its wording cannot drift.
 *
 * It is the one path in the account lifecycle that can leave a user's work
 * unsent, so an unknown count must warn too — treating "could not count" as
 * 0 would skip the one confirmation this design relies on. Only a CONFIRMED
 * 0 skips it. The count is read fresh at the click (a pure SQLite read), not
 * taken from a poll that may be seconds old.
 */
export async function stopWaiting(
  a: Account,
  deps: { confirm: (title: string, message: string) => Promise<boolean> },
): Promise<StopWaitingOutcome> {
  let n: number | undefined;
  try {
    n = totalPending(await invoke<PendingPushes>('count_pending_pushes', { accountId: a.id }));
  } catch (e) {
    // Logged, not swallowed: the confirm below then says the count is unknown.
    console.error(`count_pending_pushes failed for ${a.id}:`, e);
  }
  if (n === undefined || n > 0) {
    if (!(await deps.confirm('Stop waiting?', stopWaitingMessage(n)))) return 'cancelled';
  }
  return (await setAccountStatus(a.id, 'inactive')) ? 'stopped' : 'failed';
}

/** What a surface shows when `stopWaiting` returns `failed`. */
export const STOP_WAITING_FAILED = 'Could not stop waiting for this account. Try again.';

/**
 * Deactivate (`active: false`) or reactivate (`active: true`) an account.
 *
 * Deactivating writes `draining`, not `inactive`: no confirmation, because
 * it loses nothing — the account keeps flushing its already-queued work in
 * the background and the worker flips it to `inactive` once that is done.
 *
 * Reactivating writes `active` and then, fire-and-forget, warms the account
 * up (`warmUpAccount`): the status flip is optimistic and instant, and the
 * warm-up must not hold the click.
 *
 * A refusal rolls the store back and rejects with the backend's error.
 */
export async function setAccountActive(accountId: string, active: boolean): Promise<void> {
  if (active) {
    await writeAccountStatus(accountId, 'active');
    void warmUpAccount(accountId);
  } else {
    await writeAccountStatus(accountId, 'draining');
  }
}

// An account can come back after weeks away, and while it was dismissed
// every fan-out skipped it — including cold start, whose `index_account`
// is refused outright once the account is Inactive. So a reactivated
// account can have NO `noteIndex` entry at all, which is what hydrates the
// sidebar's folders and drives the background sweep; without this it stays
// empty until a focus or poll refresh happens to land.
//
// Same warm-up cold start runs for a visible account (App.svelte): the
// index pass first, then the two meta_label sidecar pulls in parallel
// (disjoint columns, disjoint subject prefixes — safe together), then a
// re-read of the local tag rows and a repaint. Fire-and-forget: the status
// flip is optimistic and instant, and this must not hold the click.
//
// The folder tree is the Sidebar's; `requestFolderRefresh` is the signal
// its refresh already listens to (gotcha #6).
export async function warmUpAccount(id: string): Promise<void> {
  try {
    const idx = await invoke<MessageIndex[]>('index_account', { accountId: id });
    noteIndex.update((m) => { m.set(id, idx); return m; });
  } catch (e) {
    console.warn(`reactivate warm-up: index_account failed for ${id}:`, e);
  }
  await Promise.allSettled([
    invoke<number>('sync_pin_state', { accountId: id }).catch((e) => {
      console.warn(`reactivate warm-up: sync_pin_state failed for ${id}:`, e);
      return 0;
    }),
    invoke<number>('sync_tag_state', { accountId: id }).catch((e) => {
      console.warn(`reactivate warm-up: sync_tag_state failed for ${id}:`, e);
      return 0;
    }),
  ]);
  try {
    const rows = await invoke<{ uuid: string; tag: string }[]>('list_note_tags', { accountId: id });
    setAccountNoteTags(id, rows);
  } catch (e) {
    console.warn(`reactivate warm-up: list_note_tags failed for ${id}:`, e);
  }
  requestFolderRefresh();
  get(refreshNotes)();
}

// ─── Add account ─────────────────────────────────────────────────────

export type AddAccountKind = 'gmail' | 'microsoft' | 'icloud' | HostAddAccountKind;

// Kick off OAuth for a NEW account. Same flow as initial sign-in: backend
// returns an auth URL (Google or Microsoft, per `backend`), we open it in
// the system browser, the localhost callback finishes the exchange and
// emits 'oauth-success'. App.svelte's listener flips isAuthenticated
// (already true here) — what we need instead is to refresh the accounts
// list so the new account appears. AccountAddHost's oauth-success listener
// does that. Omitting `backend` still means Gmail.
export async function addOAuthAccount(backend?: 'microsoft'): Promise<void> {
  try {
    const url = await invoke<string>('get_auth_url', backend ? { backend } : undefined);
    await invoke('open_auth_url', { url });
  } catch (e) {
    alert(`Failed to start sign-in: ${e}`);
  }
}

/**
 * Add an iCloud account.
 *
 * Not an OAuth flow: `icloud_sign_in` opens Apple's own pages in a webview
 * and does not resolve until the session is complete, the user closes the
 * window, or it times out. On success it emits `oauth-success`, so
 * AccountAddHost's listener does the same post-add refresh every other
 * backend gets — no second refresh path to keep in step.
 *
 * A rejection is a real outcome worth showing, not a lost callback: a
 * cancelled window, or an account with Advanced Data Protection on, where
 * the backend refuses before creating anything and the message names ADP and
 * what to do about it.
 */
// The post-add refresh is NOT optional here, and its absence was invisible
// because everything else worked. `icloud_sign_in` creates the account,
// indexes it and returns — and this function used to drop that on the floor,
// so the sidebar kept rendering the account list it had loaded at startup.
// The account existed in accounts.json, its notes were in SQLite, and the
// screen showed neither until the app was restarted. Clicking the menu item
// again then produced "one iCloud account at a time" about an account the
// user could not see, which is how this was found.
//
// **Gotcha #6, for the fourth time**: state the backend changes needs a
// route back to the frontend, and reciting the rule is not applying it.
// AccountAddHost's `addLocalFolder` carries the same three lines with a comment
// saying it mirrors the oauth-success flow; this one never got them. When a
// second caller re-implements a shared boundary, diff the pair.
export async function addICloudAccount(): Promise<void> {
  try {
    console.log('[jodd] icloud_sign_in: invoking');
    await invoke<string>('icloud_sign_in');
    console.log('[jodd] icloud_sign_in: reply received');
    // Mirror the oauth-success post-add flow: refetch accounts, switch to
    // the new one, then refresh notes + folders.
    const list = await invoke<Account[]>('list_accounts');
    accounts.set(list);
    // By id rather than by diffing the list, because sign-in is idempotent:
    // it succeeds without adding a row when the account is already there
    // (a resumed sign-in), and a diff finds nothing to select in that case.
    // `icloud_sign_in` resolves to the Apple ID, and `account_id_for` builds
    // the id as `{backend}:{email}` — see accounts.rs.
    const icloud = list.find((a) => a.backend_kind === 'icloud');
    if (icloud) currentAccount.set(icloud.id);
    get(refreshNotes)();
    requestFolderRefresh();
  } catch (e) {
    alert(`iCloud sign-in did not complete: ${e}`);
  }
}

let addRequestSeq = 0;

/**
 * Start adding an account of `kind`. Does not gate on platform: the caller
 * offers only the kinds its platform supports (`$supportsIcloud`, no
 * advanced SSH on Android), as the Sidebar's panel does.
 *
 * - `gmail`, `microsoft`, `icloud` are pure IPC flows and run here.
 * - `localfs`, `ssh`, `ssh_advanced` need a component (Android plugin
 *   watchers and busy guard; dialogs to mount), so they are published on
 *   `addAccountRequest` and AccountAddHost runs them. The host is always
 *   mounted while signed in, and a request made before it mounts waits in
 *   the store until it does.
 */
export function startAddAccount(kind: AddAccountKind): void {
  switch (kind) {
    case 'gmail':
      void addOAuthAccount();
      return;
    case 'microsoft':
      void addOAuthAccount('microsoft');
      return;
    case 'icloud':
      void addICloudAccount();
      return;
    default:
      addAccountRequest.set({ kind, seq: ++addRequestSeq });
  }
}
