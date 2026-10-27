// Single-note actions — one implementation shared by every surface that
// offers them: the note-list context menu (NoteContextMenu.svelte) and the
// editor's ⋯ menu (NoteMoreMenu.svelte, a sheet on phone). Each body moved here verbatim from NoteContextMenu.svelte
// (same optimistic → rollback shape, same IPC calls, same store writes), so
// the surfaces cannot drift.
//
// What did NOT move is the surface's own business: closing its menu and
// owning its ConfirmDialog. The caller closes its menu after calling the
// action. Everything an action does before its first `await` — the optimistic
// store write, the IPC send — therefore still happens before the menu closes,
// exactly as it did when the menu called `onClose()` mid-body. Delete is the
// one exception, because its confirm dialog renders inside the still-open
// menu: see `deleteNoteWithConfirm`'s `onDecided`.
//
// The `note` argument is a plain value captured at the call, never a Svelte
// prop, so it stays readable after the caller's menu unmounts. The field
// snapshots below predate that and are kept as they were.
import { invoke } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import { syncBlocks, forgetPinBlocks } from '../syncBlocks';
import { deleteNoteOptimistic } from './noteMutations';
import { sameNote } from '../noteIdentity';
import { appendHtml } from '../noteHtml';
import {
  error, notes, selectedNote, navigateToFolder, viewSnapshot, viewUnchanged, currentAccount,
  indexUpsertOnSave, setAccountNoteTags, capabilitiesByAccount, needsPermanentDeleteConfirm,
} from '../stores/notes';
import { moveNoteOptimistic } from '../moveNotes';
import { refreshUnreviewed } from '../unreviewed';
import { requestFolderSuggestion, recordFolderSuggestion, explicitOutcomeMessage, explicitErrorMessage } from '../folderSuggestion';
import { offerAiConsent } from '../aiConsent';
import type { Note } from '../types';

export type ProposedAppend = { uuid: string; title: string; addition_text: string };

/** Restore `fields` on the rows `matches` selects from a pre-write snapshot.
 *  Shared with NoteContextMenu's batch pin and batch move. */
export function rollbackFields(before: Note[], selected: Note | null, fields: ('pinned' | 'label')[], matches: (n: Note) => boolean) {
  const restore = (n: Note) => {
    const old = before.find(x => sameNote(x, n)) ?? (sameNote(selected, n) ? selected : null);
    if (!old || !matches(n)) return n;
    return { ...n, ...Object.fromEntries(fields.map(f => [f, old[f]])) };
  };
  notes.update(ns => ns.map(restore));
  selectedNote.update(n => n ? restore(n) : n);
}

// ─── Pin (single-note path) ──────────────────────────────────────────
//
// Doctrine-compliant: snapshot, flip the in-memory pinned flag and let
// the reactive sort in NoteList float the row to the top, fire the
// local-first set_pin command (SQLite UPDATE, no Gmail), roll back on
// failure. The backend never waits on Gmail because pin doesn't
// round-trip through the email backend.
export async function togglePin(note: Note): Promise<void> {
  const accountId = note.account_id || get(currentAccount);
  if (!accountId || !note.uuid) {
    return;
  }
  const targetUuid = note.uuid;
  const nextPinned = !note.pinned;

  const prevNotes = get(notes);
  const prevSelectedNote = get(selectedNote);
  const prevBlocks = get(syncBlocks);

  // `set_pin` re-arms a refused pin push (gotcha #14), so its marker goes too.
  forgetPinBlocks(accountId, [targetUuid]);
  notes.update((ns) =>
    ns.map((n) => (sameNote(n, { account_id: accountId, uuid: targetUuid }) ? { ...n, pinned: nextPinned } : n)),
  );
  selectedNote.update((cur) =>
    cur && sameNote(cur, { account_id: accountId, uuid: targetUuid }) ? { ...cur, pinned: nextPinned } : cur,
  );

  try {
    await invoke('set_pin', { accountId, uuid: targetUuid, pinned: nextPinned });
  } catch (e) {
    console.error('pin failed', e);
    rollbackFields(prevNotes, prevSelectedNote, ['pinned'], n => sameNote(n, { account_id: accountId, uuid: targetUuid }));
    syncBlocks.set(prevBlocks);
    error.set(`Failed to ${nextPinned ? 'pin' : 'unpin'} note: ${e}`);
  }
}

// **`move_notes_batch`, not `save_note` — this used to be the one write
// path the context menu reused a content command for.** A single-note move
// predates `move_notes_batch` and, on Gmail, reusing `save_note` cost
// nothing extra: `RelocatesOnContentPush` carries the folder as part of
// any content push anyway. It is wrong on `Write::Relocate`-only backends
// — iCloud can move a note but cannot push its content — so `save_note`
// was refused there even though the move itself is measured safe (M2
// relocation live pass, 2026-08-24). The menu's batch move already went
// through `move_notes_batch`; this makes the single-note case match it
// instead of carrying its own, capability-incorrect path.
export async function moveNoteTo(note: Note, target: string): Promise<void> {
  if (target === note.label) {
    return;
  }
  const accountId = note.account_id || get(currentAccount);
  if (!accountId) {
    return;
  }

  // The helper relabels synchronously, before this function's first await,
  // so the note moves before the caller closes its menu.
  const pending = moveNoteOptimistic({
    accountId,
    uuid: note.uuid,
    fromLabel: note.label,
    toLabel: target,
    remoteId: note.id,
  });

  const result = await pending;
  if (!result.ok) {
    console.error('move failed', result.error);
    alert(`Failed to move note: ${result.error}`);
  }
}

export async function duplicateNote(note: Note): Promise<void> {
  if (!note.id) {
    // Unsaved blank — nothing to duplicate from Gmail.
    return;
  }
  const accountId = note.account_id || get(currentAccount);
  if (!accountId) {
    return;
  }
  try {
    // Save without existingUuid/existingGmailId → Rust generates a fresh
    // UUID and inserts a brand-new message. Same title/body/label as the
    // source. The original note is untouched.
    const newTitle = `${note.title} copy`;
    const saved = await invoke<{ id: string; uuid: string }>('save_note', {
      accountId,
      title: newTitle,
      bodyHtml: note.body_html,
      existingGmailId: null,
      existingUuid: null,
      existingXMailCreatedDate: null,
      label: note.label,
      // The copy inherits the source's provenance history, so agent-written
      // content is not laundered into human_written by duplicating it.
      duplicateOf: note.uuid,
    });
    // A copy of an unreviewed note is unreviewed too (it inherits the history).
    void refreshUnreviewed([accountId]);
    const dup: Note = {
      id: saved.id,
      uuid: saved.uuid,
      title: newTitle,
      body_html: note.body_html,
      date: new Date().toISOString(),
      label: note.label,
      x_mail_created_date: null,
      account_id: accountId,
    };
    notes.update((ns) => [dup, ...ns]);
    selectedNote.set(dup);
    // Duplicate is a brand-new message — no previous id to clear.
    indexUpsertOnSave(accountId, null, saved.id, note.label);
  } catch (e) {
    console.error('duplicate failed', e);
    alert(`Failed to duplicate: ${e}`);
  }
}

export async function refetchNote(note: Note): Promise<void> {
  if (!note.id) {
    // No remote version yet — nothing to refetch.
    return;
  }
  const accountId = note.account_id || get(currentAccount);
  if (!accountId) {
    return;
  }
  try {
    const fresh = await invoke<Note>('refetch_note', { accountId, id: note.id });
    // Replace the in-memory copy. By matching on uuid (not id — a refetch
    // may surface that Apple appended a new message id and our local id
    // is now stale), we update the correct row even if the id rotated.
    notes.update((ns) => {
      const idx = ns.findIndex((n) => sameNote(n, fresh));
      if (idx >= 0) ns[idx] = fresh;
      return ns;
    });
    if (sameNote(get(selectedNote), fresh)) selectedNote.set(fresh);
  } catch (e) {
    console.error('refetch failed', e);
    alert(`Refetch failed: ${e}`);
  }
}

// ─── Re-extract lessons ───────────────────────────────────────────────
// Only offered when the note carries the Source verbatim block — that's
// the only shape the lesson extractor knows how to consume. The backend
// creates a NEW note beside the source (or, if that folder is gone, in
// Notes/Inbox or the root) and tells us where; we repaint that folder
// from the cache so the freshly-inserted row appears.
export async function reExtractLessons(note: Note): Promise<void> {
  const accountId = note.account_id || get(currentAccount);
  if (!accountId || !note.uuid) {
    return;
  }

  // Snapshot EVERY field we'll need. When this body lived in the menu, its
  // `note` was a Svelte prop that resolved to null once the menu unmounted —
  // touching `note.uuid` (etc.) after that threw `null is not an object
  // (evaluating 'note().uuid')`, the invoke never ran and Re-extract failed
  // instantly with no backend call. Mirrors linkIntoWiki() below.
  const noteUuid = note.uuid;

  // Background work with no bound: the user may open another view before it
  // lands, and must not be pulled out of it (local-first doctrine).
  const view = viewSnapshot();
  try {
    // request_id lets the user cancel the in-flight extract via
    // cancel_extraction. The Re-extract path doesn't currently expose a
    // Cancel UI (right-click → menu close → background work) so this id
    // is fire-and-forget; future iteration could add a small toast with a
    // Cancel link.
    const requestId =
      globalThis.crypto?.randomUUID?.() ?? `req-${Date.now()}-${Math.random()}`;
    const created = await invoke<{ uuid: string; label: string }>('re_extract_note', {
      accountId,
      uuid: noteUuid,
      requestId,
    });
    // The backend filed the new note beside its source (or, if that folder
    // is gone, in Notes/Inbox or the root) and says where. selectedFolder
    // .set() is a NO-OP when that folder is already open — the usual case —
    // so repaint it from the cache explicitly. Mirrors LessonExtractModal.
    // The note's own account: it may not be the one on screen (a
    // cross-account search or tag result), and the path means nothing in
    // another account.
    const navigated = viewUnchanged(view);
    if (navigated) {
      if (get(currentAccount) !== accountId) currentAccount.set(accountId);
      navigateToFolder(created.label);
    } else {
      console.log(`[jodd] re-extract filed its note in '${created.label}'; the view changed meanwhile, so staying put`);
    }
    const landed = viewSnapshot();
    const openNote = get(selectedNote);
    try {
      const cached = await invoke<Note[]>('list_cached_notes_in_folder', {
        accountId,
        path: created.label,
      });
      notes.update((ns) => {
        const others = ns.filter((n) => !(n.account_id === accountId && n.label === created.label));
        return [...others, ...cached];
      });
    } catch (e) {
      console.warn('post-re-extract cache paint failed', e);
    }
    // Open the new note — after another await, so only if the user has
    // neither moved nor opened a different note meanwhile. By key, not by
    // object: App's folder paint may refresh the open note in place.
    const nowOpen = get(selectedNote);
    const sameOpen = openNote ? sameNote(nowOpen, openNote) : nowOpen === null;
    if (navigated && viewUnchanged(landed) && sameOpen) {
      const found = get(notes).find((n) => n.account_id === accountId && n.uuid === created.uuid);
      if (found) selectedNote.set(found);
    }
    // The new note carries LLM-suggested tags. Without this the sidebar tag
    // cloud and the editor chip row stay stale until an unrelated tag write.
    try {
      const rows = await invoke<{ uuid: string; tag: string }[]>('list_note_tags', {
        accountId,
      });
      setAccountNoteTags(accountId, rows);
    } catch (e) {
      console.warn('post-re-extract tag refresh failed', e);
    }
  } catch (e) {
    if (await offerAiConsent(e, accountId)) return;
    console.error('re-extract failed:', e);
    alert(`Re-extract failed: ${e}`);
  }
}

// ─── Link into wiki ───────────────────────────────────────────────────
// #2b: retroactively link an existing note into the wiki — no LLM
// distillation, runs suggest_wiki_links directly against the note's
// CURRENT body. See docs/superpowers/specs/2026-07-20-auto-link-ingest-design.md.
//
// `onLinkSuggestions` receives the `proposed_appends` for review. The review
// modal is owned by the caller's parent (NoteList.svelte for the context
// menu) so it survives past the caller's own menu closing.
export async function linkIntoWiki(
  note: Note,
  onLinkSuggestions: (accountId: string, proposedAppends: ProposedAppend[]) => void,
): Promise<void> {
  const accountId = note.account_id || get(currentAccount);
  if (!accountId || !note.uuid) {
    return;
  }

  // Snapshot EVERY field we'll need — see reExtractLessons().
  const noteUuid = note.uuid;
  const noteTitle = note.title;
  const noteBodyHtml = note.body_html;
  const noteId = note.id;
  const noteXMailCreated = note.x_mail_created_date ?? null;
  const noteLabel = note.label;

  try {
    const requestId =
      globalThis.crypto?.randomUUID?.() ?? `req-${Date.now()}-${Math.random()}`;
    const res = await invoke<{
      ai_result_id: string;
      auto_links: { uuid: string; title: string; slug: string }[];
      proposed_appends: ProposedAppend[];
    }>('suggest_wiki_links', {
      accountId,
      text: noteBodyHtml,
      excludeUuid: noteUuid,
      newNoteTitle: noteTitle,
      newNoteUuid: noteUuid,
      requestId,
    });

    if (res.auto_links.length > 0) {
      const linksLine = `<p>Related: ${res.auto_links
        .map((l) => `[[${l.slug}]]`)
        .join(', ')}</p>`;
      // Same save_note signature note as LessonExtractModal.svelte's
      // runAutoLinkSuggestions (lib.rs:1168-1180) — existingUuid (not
      // `uuid`) is the required key that makes this an edit, not a new note.
      await invoke('save_note', {
          aiResultId: res.ai_result_id ?? '',
        accountId,
        title: noteTitle,
        bodyHtml: appendHtml(noteBodyHtml, linksLine),
        existingGmailId: noteId || null,
        existingUuid: noteUuid,
        existingXMailCreatedDate: noteXMailCreated,
        label: noteLabel,
      });
      void refreshUnreviewed([accountId]); // R15: an AI write makes a note unreviewed with no remote-changed behind it.
    }

    if (res.proposed_appends.length > 0) {
      onLinkSuggestions(accountId, res.proposed_appends);
    } else if (res.auto_links.length === 0) {
      alert('No related notes found.');
    }
  } catch (e) {
    if (await offerAiConsent(e, accountId)) return;
    console.error('link into wiki failed:', e);
    alert(`Link into wiki failed: ${e}`);
  }
}

// ─── Suggest folder ───────────────────────────────────────────────────
// The explicit caller of suggest_note_folder (extract filing). Same
// command as the automatic post-Extract call; unlike it, this one says
// what happened. A proposal lands in the folderSuggestions store and shows
// as the editor chip; accepting it is an ordinary relocation.
export async function suggestFolder(note: Note): Promise<void> {
  const accountId = note.account_id || get(currentAccount);
  if (!accountId || !note.uuid) {
    return;
  }
  // Snapshot — see reExtractLessons().
  const noteUuid = note.uuid;

  try {
    const outcome = await requestFolderSuggestion(accountId, noteUuid);
    recordFolderSuggestion(accountId, noteUuid, outcome);
    // A backend can rekey a note's uuid mid-call (gotcha #16); a
    // `suggested` outcome carries the note's CURRENT uuid, while an editor
    // opened before the rekey still holds the uuid asked about. Either
    // one means the note is open — and the chip is recorded under both.
    const openUuid = get(selectedNote)?.uuid;
    const noteIsOpen = !!openUuid
      && (openUuid === noteUuid || (outcome.kind === 'suggested' && openUuid === outcome.uuid));
    const message = explicitOutcomeMessage(outcome, noteIsOpen);
    if (message) alert(message);
  } catch (e) {
    if (await offerAiConsent(e, accountId)) return;
    const message = explicitErrorMessage(e);
    if (message) {
      console.error('suggest folder failed:', e);
      alert(message);
    }
  }
}

// ─── Delete (single-note path) ────────────────────────────────────────
//
// `confirm` is the caller's own in-DOM ConfirmDialog (never native
// confirm(): WKWebView eats it silently in some configurations), asked only
// when needsPermanentDeleteConfirm() is true.
//
// `onDecided` is where the caller closes its menu. It cannot simply close
// after calling this, the way it does for every other action: the confirm
// dialog renders inside the still-open menu, and closing it would unmount
// the dialog with the confirm Promise still pending. Nor can it close after
// awaiting this, which would hold the menu open on the network. So it is
// called exactly where the menu used to call onClose(): after a cancel,
// right after the optimistic removal began, or after an error.
export async function deleteNoteWithConfirm(
  note: Note,
  confirm: (title: string, message: string) => Promise<boolean>,
  // Required: every caller hosts the confirm dialog inside its own menu, so
  // it must say when that menu may close — after a cancel, right after the
  // optimistic removal begins, or after a failure. A default no-op would let
  // a caller close too early (unmounting the dialog with `confirm` pending)
  // or never close at all.
  onDecided: () => void,
): Promise<void> {
  // The Delete menu item is already an explicit action; trash is recoverable
  // via Gmail's "Trash" view for 30 days.
  // No `if (!note.id)` guard — local-only notes (never synced, id='') are
  // still deletable; Rust's delete_note path uses uuid + checks remote_version.
  try {
    const accountId = note.account_id || get(currentAccount);
    if (!accountId) return;
    // On a backend with no trash the delete below has no restore path IN
    // JODD — the one-click behavior above is safe only because Gmail/LocalFs
    // both have a real Trash. The message says exactly that and no more:
    // Microsoft's delete really is unrecoverable (measured — Deleted Items
    // held zero items afterward), but iCloud's files the note in Apple's own
    // Recently Deleted, where the user CAN restore it, just not from here.
    // One capability, two mechanisms, so the sentence must be true of both —
    // claiming the note is destroyed would be a false statement to half the
    // users who read it. Ask first here,
    // using the caller's in-DOM dialog rather than native confirm() for the
    // same WKWebView reason. Capability-driven, never a backend_kind
    // conditional — see needsPermanentDeleteConfirm in stores/notes.ts.
    if (needsPermanentDeleteConfirm(get(capabilitiesByAccount)[accountId])) {
      const ok = await confirm(
        `Delete "${note.title || 'Untitled'}" permanently?`,
        'Jodd has no Recently Deleted view for this account, so it cannot undo this.',
      );
      if (!ok) {
        onDecided();
        return;
      }
    }
    // `note` is the value the caller captured when its menu opened — it
    // doesn't track later $notes updates. A note that was still mid-save
    // then (id not yet assigned) carries id '' here forever, and
    // indexRemoveOnDelete no-ops on an empty id, permanently orphaning the
    // index stub that indexUpsertOnSave added when the save completed — a
    // ghost folder with count 0 that lingers in the sidebar for the rest of
    // the session. deleteNoteOptimistic therefore looks the current id up
    // from the live store by uuid (stable across saves), before it removes
    // the row.
    const pending = deleteNoteOptimistic({ ...note }, accountId);
    onDecided();
    await pending;
    return;
  } catch (e) {
    error.set(`Could not delete note: ${String(e)}`);
  }
  onDecided();
}
