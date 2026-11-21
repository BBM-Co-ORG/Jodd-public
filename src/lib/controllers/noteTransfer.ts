// Copy to / Move to another account (spec
// docs/superpowers/specs/2026-11-13-cross-account-move-copy-design.md).
//
// One entry point for every surface (the note list's menu and sheet, the
// editor's ⋯): `transferNotes(sources, targetAccount, folder, mode)`.
//
// - A move inside one account stays what it was: `moveNoteTo`, a relabel.
// - Anything else goes to Rust's transfer engine. Its preview is read first
//   (SQLite only); when a note would lose something, or a move would delete
//   an Outlook original for good, the user confirms (§2.3, §4.4).
// - A move hides its originals in the same step the write is sent, and puts
//   back any the write refused. The copies arrive through the `remote-changed`
//   the command emits; an open note follows its copy (§5).
import { invoke } from '@tauri-apps/api/core';
import { get, writable } from 'svelte/store';
import type { Note } from '../types';
import { noteKey, sameNote } from '../noteIdentity';
import {
  activeAccounts, currentAccount, error, noteMutationRevision, notes,
  pendingDeletedNoteKeys, selectedNote, selectedUuids, smartFolderNotes,
} from '../stores/notes';
import { accountNameWithBackend } from '../stores/listView';
import { flushEditors } from '../editorFlush';
import { showToast } from '../stores/toast';
import { failureText } from '../errorText';
import { displayTitle } from '../textDisplay';
import { moveNoteTo } from './noteActions';

export type TransferMode = 'copy' | 'move';

export type TransferLoss =
  | { kind: 'attachment'; filename: string }
  | { kind: 'pin' }
  | { kind: 'tagsAsKeywords' }
  | { kind: 'partial'; reason: string }
  | { kind: 'links' }
  | { kind: 'folderFlattened'; from: string };

export interface PlannedTransfer {
  source: { accountId: string; uuid: string };
  title: string;
  /** Why the note is not copied or moved; null when it is. */
  refused: string | null;
  losses: TransferLoss[];
  /** The copy's uuid, in a report from `transfer_notes` only. */
  targetUuid?: string;
}

export interface TransferReport {
  notes: PlannedTransfer[];
}

/** What a confirm asks, shown once by TransferConfirm.svelte (App). */
export interface TransferQuestion {
  title: string;
  message: string;
  confirmLabel: string;
  answer: (ok: boolean) => void;
}

export const transferQuestion = writable<TransferQuestion | null>(null);

function ask(q: Omit<TransferQuestion, 'answer'>): Promise<boolean> {
  return new Promise((resolve) => {
    transferQuestion.set({
      ...q,
      answer: (ok) => {
        transferQuestion.set(null);
        resolve(ok);
      },
    });
  });
}

/** One loss in plain words. */
export function lossText(l: TransferLoss): string {
  switch (l.kind) {
    case 'attachment': return `the attachment ${l.filename} becomes a placeholder`;
    case 'pin': return 'the pin is not kept';
    case 'tagsAsKeywords': return 'tags become a Keywords line';
    case 'partial': return `Jodd could not read all of it (${l.reason})`;
    case 'links': return 'links to and from notes left behind stop working';
    case 'folderFlattened': return `it lands in the top folder instead of ${l.from}`;
  }
}

const accountOf = (n: Note) => n.account_id || get(currentAccount) || '';

function accountName(id: string): string {
  const all = get(activeAccounts);
  const a = all.find((x) => x.id === id);
  return a ? accountNameWithBackend(a, all) : id;
}

const quoted = (title: string) => `“${displayTitle(title) || 'Untitled'}”`;

/**
 * The confirm's lines for a preview, or none when nothing needs saying: a
 * line per note that loses something, per note that is refused, and for a
 * move, one line per Outlook account it deletes originals from (§4.4).
 */
export function confirmLines(report: TransferReport, mode: TransferMode, targetAccount: string): string[] {
  const lines: string[] = [];
  for (const n of report.notes) {
    if (n.refused) lines.push(`${quoted(n.title)} is skipped: ${n.refused}`);
    else if (n.losses.length) lines.push(`${quoted(n.title)}: ${n.losses.map(lossText).join('; ')}.`);
  }
  if (mode === 'move') {
    const outlook = new Set(
      report.notes
        .filter((n) => !n.refused)
        .map((n) => n.source.accountId)
        .filter((id) => get(activeAccounts).find((a) => a.id === id)?.backend_kind === 'microsoft'),
    );
    for (const id of outlook) {
      lines.push(
        `The original will be deleted from ${accountName(id)} for good once the copy reaches ` +
          `${accountName(targetAccount)}. Outlook has no Recently Deleted.`,
      );
    }
  }
  return lines;
}

/**
 * Hide moved originals from every list, in one step. Returns the undo, which
 * puts back the ones whose keys it is given (every one when given none).
 */
function hideOriginals(sources: Note[]): (only?: Set<string>) => void {
  const keys = new Set(sources.map(noteKey));
  const stores = [notes, smartFolderNotes];
  const snapshots = stores.map((store) => ({ store, rows: get(store).filter((n) => keys.has(noteKey(n))) }));
  const wasMulti = [...get(selectedUuids)].filter((k) => keys.has(k));
  pendingDeletedNoteKeys.update((s) => new Set([...s, ...keys]));
  stores.forEach((store) => store.update((ns) => ns.filter((n) => !keys.has(noteKey(n)))));
  selectedUuids.update((s) => new Set([...s].filter((k) => !keys.has(k))));
  return (only?: Set<string>) => {
    const back = (k: string) => !only || only.has(k);
    snapshots.forEach(({ store, rows }) =>
      store.update((ns) => [...ns, ...rows.filter((r) => back(noteKey(r)) && !ns.some((n) => sameNote(n, r)))]),
    );
    const multi = wasMulti.filter(back);
    if (multi.length) selectedUuids.update((s) => new Set([...s, ...multi]));
  };
}

/**
 * Copy or move `sources` into `targetAccount`/`folder`. Resolves when done;
 * failures go to the error bar. Never throws.
 */
export async function transferNotes(sources: Note[], targetAccount: string, folder: string, mode: TransferMode): Promise<void> {
  const real = sources.filter((n) => n.uuid && !n.uuid.startsWith('tmp:') && n.id);
  // A move inside one account is a relabel, as it always was.
  const across = mode === 'move' ? real.filter((n) => accountOf(n) !== targetAccount) : real;
  if (mode === 'move') {
    for (const n of real.filter((n) => accountOf(n) === targetAccount)) void moveNoteTo(n, folder);
  }
  if (across.length === 0) return;

  const refs = across.map((n) => ({ accountId: accountOf(n), uuid: n.uuid }));
  const args = { sources: refs, targetAccount, targetFolder: folder, mode };
  const what = mode === 'copy' ? 'copy' : 'move';
  const noun = across.length === 1 ? 'the note' : `${across.length} notes`;
  try {
    // The copy must carry what the editor holds, not what it last saved.
    await flushEditors();
    const preview = await invoke<TransferReport>('preview_note_transfer', args);
    const going = preview.notes.filter((n) => !n.refused);
    const lines = confirmLines(preview, mode, targetAccount);
    if (going.length === 0) {
      error.set(lines.join('\n') || `Couldn't ${what} ${noun}.`);
      return;
    }
    if (lines.length) {
      const verb = mode === 'copy' ? 'Copy' : 'Move';
      const ok = await ask({
        title: `${verb} ${going.length === 1 ? quoted(going[0].title) : `${going.length} notes`} to ${accountName(targetAccount)}?`,
        message: lines.join('\n'),
        confirmLabel: verb,
      });
      if (!ok) return;
    }
  } catch (e) {
    error.set(failureText(`${what} ${noun}`, e));
    return;
  }

  const moving = mode === 'move' ? across : [];
  const open = get(selectedNote);
  const openMoving = open ? moving.find((n) => sameNote(n, open)) : undefined;
  const unhide = hideOriginals(moving);
  let report: TransferReport;
  try {
    report = await invoke<TransferReport>('transfer_notes', args);
  } catch (e) {
    unhide();
    error.set(failureText(`${what} ${noun}`, e));
    return;
  } finally {
    noteMutationRevision.update((n) => n + 1);
  }
  // Originals the write refused come back; the rest are gone from their lists.
  const refused = report.notes.filter((n) => n.refused);
  const keys = new Set(moving.map(noteKey));
  if (refused.length && moving.length) {
    unhide(new Set(moving
      .filter((n) => refused.some((r) => r.source.accountId === accountOf(n) && r.source.uuid === n.uuid))
      .map(noteKey)));
  }
  pendingDeletedNoteKeys.update((s) => new Set([...s].filter((k) => !keys.has(k))));

  // The open note follows its copy, and its account becomes current (the
  // all-accounts rule, spec 2026-11-13-all-accounts-scope §3.4).
  if (openMoving) {
    const done = report.notes.find((n) => n.source.uuid === openMoving.uuid && n.targetUuid);
    if (done?.targetUuid) {
      currentAccount.set(targetAccount);
      selectedNote.set({ ...openMoving, uuid: done.targetUuid, id: '', account_id: targetAccount, label: folder });
    }
  }

  const written = report.notes.length - refused.length;
  if (refused.length) {
    error.set(refused.map((n) => `${quoted(n.title)} was not ${mode === 'copy' ? 'copied' : 'moved'}: ${n.refused}`).join('\n'));
  }
  if (written > 0) {
    const verb = mode === 'copy' ? 'Copied' : 'Moved';
    showToast(`${verb} ${written === 1 ? 'to' : `${written} notes to`} ${accountName(targetAccount)}`, 2500);
  }
}

/** Why a move was called off, from the worker (`note-transfer-called-off`). */
export interface TransferCalledOff {
  sourceAccount: string;
  sourceUuid: string;
  targetAccount: string;
  reason: 'targetRefused' | 'sourceChanged';
}

/** The error bar's words for a move the worker called off (§4.3). */
export function calledOffText(e: TransferCalledOff): string {
  const to = accountName(e.targetAccount);
  const from = accountName(e.sourceAccount);
  return e.reason === 'targetRefused'
    ? `A note could not be moved to ${to}: it refused the copy. The original is back in ${from}, and the copy shows why.`
    : `A note changed in ${from} while it was being moved to ${to}, so both copies were kept.`;
}

