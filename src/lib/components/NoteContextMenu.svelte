<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { activeAccounts, notes, selectedNote, selectedFolder, refreshNotes, currentAccount, indexRemoveOnDelete, indexUpsertOnSave, noteTagsByAccount, getNoteTags, setNoteTags, setAccountNoteTags, accountDisplay, capabilitiesByAccount, canWrite, needsPermanentDeleteConfirm } from '../stores/notes';
  import type { Note } from '../types';
  import { onMount, onDestroy } from 'svelte';
  import { get } from 'svelte/store';
  import Icon from './Icon.svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';

  // Position is in viewport coords from the contextmenu event.
  export let x: number;
  export let y: number;
  export let note: Note;
  // Optional multi-note batch. When length > 1, the menu renders in
  // bulk mode: single-only items (New here, Duplicate, Refetch) hide,
  // Move-to + Delete iterate over every entry. When empty or length === 1,
  // the menu behaves exactly like before (single-note actions on `note`).
  export let selection: Note[] = [];
  export let onClose: () => void;
  // #2b retroactive linking: the review modal for `proposed_appends` is
  // owned by the parent (NoteList.svelte) so it survives past this menu's
  // own onClose()-triggered unmount. See linkIntoWiki() below.
  export let onLinkSuggestions: (
    accountId: string,
    proposedAppends: { uuid: string; title: string; addition_text: string }[],
  ) => void;

  // Per-area write gates, matching what each item below actually invokes:
  // pin is `Write::Sidecars` (`set_pin`/`set_pin_batch`), everything else —
  // new note, duplicate, delete, move, re-extract, link into wiki — is
  // `Write::Notes` (`save_note`/`delete_note`/`move_notes_batch`/
  // `re_extract_note`/`apply_wiki_link_appends`). Re-extract additionally
  // needs `Write::Folders` (`ensure_workflow_folder`), which is now refused
  // on Microsoft too (folders never reach Apple Notes, measured
  // 2026-08-14/15) — so re-extract is unavailable there even though plain
  // note writes are not. Using one coarse gate here would show Pin as
  // available on Microsoft, where `notes` is true but `sidecars` is not —
  // exactly the frontend/backend divergence this split exists to describe.
  // Every mutating item is hidden rather than shown-and-failing; the real
  // enforcement is `refuse_write` in lib.rs — a write that reached SQLite
  // here would leave a row the worker can never push, which permanently
  // blocks draining the account.
  $: writeAccountId = note.account_id || $currentAccount;
  $: canWriteNotes = canWrite(
    writeAccountId ? $capabilitiesByAccount[writeAccountId] : undefined,
    'notes',
  );
  $: canWriteFolders = canWrite(
    writeAccountId ? $capabilitiesByAccount[writeAccountId] : undefined,
    'folders',
  );
  $: canWriteSidecars = canWrite(
    writeAccountId ? $capabilitiesByAccount[writeAccountId] : undefined,
    'sidecars',
  );

  $: isMulti = selection.length > 1;
  $: multiCount = selection.length;
  // For Move-to: in multi-mode all selected notes must be in the same
  // account (cross-account batch move would need separate plumbing).
  // We pick that account from the first entry; if any others differ we
  // disable the move-to submenu.
  $: multiAccountId = isMulti ? (selection[0].account_id ?? $currentAccount) : null;
  $: multiAccountUniform = isMulti
    ? selection.every((n) => (n.account_id ?? $currentAccount) === multiAccountId)
    : true;

  // Group folders by account. Each account's Notes tree lives in its own
  // Gmail mailbox, so the cascade reads "Move to → account → folder".
  //
  // Folder source per account, in priority order:
  //   1. The backend's `list_folders` (queries that account's DB rows) —
  //      authoritative even when only one account's notes are loaded in
  //      memory. Fetched once on mount per signed-in account.
  //   2. Paths reconstructed from $notes (live, includes any folders
  //      created in this session that haven't been flushed to DB yet).
  // Union of the two so newly-created folders show up immediately AND
  // accounts whose notes aren't in $notes still appear with their tree.
  type FolderRow = { path: string; name: string; depth: number };
  let foldersFromBackend: Map<string, string[]> = new Map();

  onMount(async () => {
    // Fetch folders for every signed-in account in parallel. Each call hits
    // SQLite only (cached_label_map is read-only here) so it's cheap; doing
    // them in parallel keeps the menu responsive even with several accounts.
    const list = $activeAccounts;
    const results = await Promise.all(
      list.map(async (a) => {
        try {
          const paths = await invoke<string[]>('list_folders', { accountId: a.id });
          return [a.id, paths] as const;
        } catch (e) {
          console.warn(`list_folders failed for ${a.id}:`, e);
          // `['Notes'] as const` would infer `readonly ['Notes']`, which does not
          // assign to the `string[]` the success branch yields — so the union of
          // the two branches failed to satisfy `new Map<string, string[]>` below.
          return [a.id, ['Notes'] as string[]] as const;
        }
      }),
    );
    foldersFromBackend = new Map(results);
  });

  $: foldersByAccount = (() => {
    const byAcct = new Map<string, Set<string>>();
    // Seed each signed-in account with its DB folders (if loaded yet) so
    // the submenu shows the real tree even for accounts whose notes haven't
    // been hydrated into $notes.
    for (const a of $activeAccounts) {
      const seed = foldersFromBackend.get(a.id) ?? ['Notes'];
      byAcct.set(a.id, new Set<string>(seed));
    }
    // Defensive: ensure the note's own account exists in the map.
    const noteAcct = note.account_id || $currentAccount;
    if (noteAcct && !byAcct.has(noteAcct)) {
      byAcct.set(noteAcct, new Set<string>(['Notes']));
    }
    // Also union live $notes folders — catches folders created this session
    // that haven't reached the folders table yet.
    for (const n of $notes) {
      const acct = n.account_id || $currentAccount;
      if (!acct) continue;
      let set = byAcct.get(acct);
      if (!set) {
        set = new Set<string>(['Notes']);
        byAcct.set(acct, set);
      }
      const segs = n.label.split('/');
      for (let i = 1; i <= segs.length; i++) {
        set.add(segs.slice(0, i).join('/'));
      }
    }
    // Convert each set to a sorted folderRows[] for rendering.
    const result = new Map<string, FolderRow[]>();
    for (const [acct, paths] of byAcct) {
      const rows = Array.from(paths)
        .sort((a, b) => a.localeCompare(b))
        .map((p) => {
          const segs = p.split('/');
          return { path: p, name: segs[segs.length - 1], depth: segs.length - 1 };
        });
      result.set(acct, rows);
    }
    return result;
  })();

  $: noteAccountId = note.account_id || $currentAccount || '';

  // Ordered list of (account, folders) for the submenu. The note's current
  // account floats to the top so the most likely move target is one hover
  // away, not buried at the bottom of the account list.
  $: accountEntries = (() => {
    const entries = Array.from(foldersByAccount.entries());
    entries.sort(([a], [b]) => {
      if (a === noteAccountId) return -1;
      if (b === noteAccountId) return 1;
      return a.localeCompare(b);
    });
    return entries;
  })();

  function newNoteHere() {
    const blank: Note = {
      id: '', uuid: '',
      title: 'New Note',
      body_html: '<html><head></head><body></body></html>',
      date: new Date().toISOString(),
      label: note.label, // same folder as the clicked note
      x_mail_created_date: null,
    };
    selectedNote.set(blank);
    onClose();
  }

  async function moveTo(target: string) {
    if (target === note.label) {
      onClose();
      return;
    }
    const accountId = note.account_id || $currentAccount;
    if (!accountId) {
      onClose();
      return;
    }

    // Snapshot EVERY field we'll need before calling onClose(). After
    // onClose() destroys this menu component, Svelte 5's prop reactivity
    // means `note` resolves to null — touching `note.title` (etc.) throws
    // `null is not an object (evaluating 'note().title')`. Save_note then
    // never runs and the move silently fails.
    const prevLabel = note.label;
    const prevId = note.id;
    const targetUuid = note.uuid;
    const noteTitle = note.title;
    const noteBodyHtml = note.body_html;
    const noteXMailCreated = note.x_mail_created_date ?? null;
    const noteLocalVersion = note.local_version ?? null;

    notes.update((ns) => {
      const idx = ns.findIndex((n) => n.uuid === targetUuid);
      if (idx >= 0) ns[idx] = { ...ns[idx], label: target };
      return ns;
    });
    if ($selectedNote?.uuid === targetUuid) {
      selectedNote.update((n) => (n ? { ...n, label: target } : n));
    }
    onClose();

    try {
      const saved = await invoke<{ id: string; uuid: string; local_version: number }>('save_note', {
        accountId,
        title: noteTitle,
        bodyHtml: noteBodyHtml,
        existingGmailId: prevId,
        existingUuid: targetUuid,
        existingXMailCreatedDate: noteXMailCreated,
        label: target,
        // Without this, a move-to-folder write is unguarded (save_note
        // treats a missing value as None → plain apply_local_edit) and a
        // concurrent jodd-mcp append during the move would be silently
        // discarded. Matches the guard doSaveNote sends in NoteEditor.svelte.
        expectedLocalVersion: noteLocalVersion,
      });
      // Backfill the new Gmail id and local_version once known. Label was
      // already updated. Keeping local_version fresh here matters: the next
      // edit's CAS otherwise reads the pre-move version and gets a false
      // conflict against this move's own write.
      notes.update((ns) => {
        const idx = ns.findIndex((n) => n.uuid === saved.uuid);
        if (idx >= 0) ns[idx] = { ...ns[idx], id: saved.id, local_version: saved.local_version };
        return ns;
      });
      if ($selectedNote?.uuid === saved.uuid) {
        selectedNote.update((n) => (n ? { ...n, id: saved.id, local_version: saved.local_version } : n));
      }
      // Index: remove the source-label stub, add destination-label stub.
      indexUpsertOnSave(accountId, prevId || null, saved.id, target);
    } catch (e) {
      // Rollback the label to where it was. The user sees the note snap back.
      notes.update((ns) => {
        const idx = ns.findIndex((n) => n.uuid === targetUuid);
        if (idx >= 0) ns[idx] = { ...ns[idx], label: prevLabel };
        return ns;
      });
      if ($selectedNote?.uuid === targetUuid) {
        selectedNote.update((n) => (n ? { ...n, label: prevLabel } : n));
      }
      console.error('move failed', e);
      alert(`Failed to move note: ${e}`);
    }
  }

  async function duplicateNote() {
    if (!note.id) {
      // Unsaved blank — nothing to duplicate from Gmail. Just close.
      onClose();
      return;
    }
    const accountId = note.account_id || $currentAccount;
    if (!accountId) {
      onClose();
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
      });
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
    onClose();
  }

  async function refetchFromGmail() {
    if (!note.id) {
      // No remote version yet — nothing to refetch.
      onClose();
      return;
    }
    const accountId = note.account_id || $currentAccount;
    if (!accountId) {
      onClose();
      return;
    }
    try {
      const fresh = await invoke<Note>('refetch_note', { accountId, id: note.id });
      // Replace the in-memory copy. By matching on uuid (not id — a refetch
      // may surface that Apple appended a new message id and our local id
      // is now stale), we update the correct row even if the id rotated.
      notes.update((ns) => {
        const idx = ns.findIndex((n) => n.uuid === fresh.uuid);
        if (idx >= 0) ns[idx] = fresh;
        return ns;
      });
      if ($selectedNote?.uuid === fresh.uuid) selectedNote.set(fresh);
    } catch (e) {
      console.error('refetch failed', e);
      alert(`Refetch failed: ${e}`);
    }
    onClose();
  }

  // ─── Re-extract lessons ───────────────────────────────────────────────
  // Only offered when the note carries the Source verbatim block — that's
  // the only shape the lesson extractor knows how to consume. The backend
  // creates a NEW note in Notes/Lessons (does not mutate the source), so
  // we just navigate the sidebar there; the existing folder-paint flow
  // will surface the freshly-inserted row.
  function hasSourceBlock(body: string): boolean {
    return body.includes('<summary>Source (verbatim)</summary>');
  }

  async function reExtractLessons() {
    const accountId = note.account_id || $currentAccount;
    if (!accountId || !note.uuid) {
      onClose();
      return;
    }

    // Snapshot EVERY field we'll need before calling onClose(). After
    // onClose() destroys this menu component, Svelte 5's prop reactivity
    // means `note` resolves to null — touching `note.uuid` (etc.) throws
    // `null is not an object (evaluating 'note().uuid')`. The invoke then
    // never runs and Re-extract fails instantly with no backend call.
    // Mirrors moveTo() / linkIntoWiki() above.
    const noteUuid = note.uuid;

    onClose();
    try {
      // request_id lets the user cancel the in-flight extract via
      // cancel_extraction. The Re-extract path doesn't currently expose a
      // Cancel UI (right-click → menu close → background work) so this id
      // is fire-and-forget; future iteration could add a small toast with a
      // Cancel link.
      const requestId =
        globalThis.crypto?.randomUUID?.() ?? `req-${Date.now()}-${Math.random()}`;
      await invoke<string>('re_extract_note', {
        accountId,
        uuid: noteUuid,
        requestId,
      });
      selectedFolder.set('Notes/__Extracts__');
      // selectedFolder.set() above is a NO-OP whenever the value is unchanged
      // (Svelte's safe_not_equal skips subscribers), and it always is here:
      // Re-extract is only offered on a note that already lives in
      // __Extracts__, so the user is already viewing that folder. Relying on
      // the folder-watcher alone left the new note invisible until a manual
      // refresh, which read as "Re-extract did nothing" even on success.
      // Explicitly repaint from the cache instead. Mirrors
      // LessonExtractModal's post-extract paint.
      try {
        const cached = await invoke<Note[]>('list_cached_notes_in_folder', {
          accountId,
          path: 'Notes/__Extracts__',
        });
        notes.update((ns) => {
          const others = ns.filter(
            (n) => !(n.account_id === accountId && n.label === 'Notes/__Extracts__'),
          );
          return [...others, ...cached];
        });
      } catch (e) {
        console.warn('post-re-extract cache paint failed', e);
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
      console.error('re-extract failed:', e);
      alert(`Re-extract failed: ${e}`);
    }
  }

  // ─── Link into wiki ───────────────────────────────────────────────────
  // #2b: retroactively link an existing note into the wiki — no LLM
  // distillation, runs suggest_wiki_links directly against the note's
  // CURRENT body. See docs/superpowers/specs/2026-07-20-auto-link-ingest-design.md.
  async function linkIntoWiki() {
    const accountId = note.account_id || $currentAccount;
    if (!accountId || !note.uuid) {
      onClose();
      return;
    }

    // Snapshot EVERY field we'll need before calling onClose(). After
    // onClose() destroys this menu component, Svelte 5's prop reactivity
    // means `note` resolves to null — touching `note.title` (etc.) throws.
    // Mirrors moveTo()'s established pattern above.
    const noteUuid = note.uuid;
    const noteTitle = note.title;
    const noteBodyHtml = note.body_html;
    const noteId = note.id;
    const noteXMailCreated = note.x_mail_created_date ?? null;
    const noteLabel = note.label;

    onClose();
    try {
      const requestId =
        globalThis.crypto?.randomUUID?.() ?? `req-${Date.now()}-${Math.random()}`;
      const res = await invoke<{
        auto_links: { uuid: string; title: string; slug: string }[];
        proposed_appends: { uuid: string; title: string; addition_text: string }[];
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
          accountId,
          title: noteTitle,
          bodyHtml: `${noteBodyHtml}${linksLine}`,
          existingGmailId: noteId || null,
          existingUuid: noteUuid,
          existingXMailCreatedDate: noteXMailCreated,
          label: noteLabel,
        });
      }

      if (res.proposed_appends.length > 0) {
        onLinkSuggestions(accountId, res.proposed_appends);
      } else if (res.auto_links.length === 0) {
        alert('No related notes found.');
      }
    } catch (e) {
      console.error('link into wiki failed:', e);
      alert(`Link into wiki failed: ${e}`);
    }
  }

  // ─── Pin (single-note path) ──────────────────────────────────────────
  //
  // Doctrine-compliant: snapshot, flip the in-memory pinned flag and let
  // the reactive sort in NoteList float the row to the top, fire the
  // local-first set_pin command (SQLite UPDATE, no Gmail), roll back on
  // failure. The backend never waits on Gmail because pin doesn't
  // round-trip through the email backend.
  async function togglePinOne() {
    const accountId = note.account_id || $currentAccount;
    if (!accountId || !note.uuid) {
      onClose();
      return;
    }
    const targetUuid = note.uuid;
    const nextPinned = !note.pinned;

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);
    onClose();

    notes.update((ns) =>
      ns.map((n) => (n.uuid === targetUuid ? { ...n, pinned: nextPinned } : n)),
    );
    selectedNote.update((cur) =>
      cur && cur.uuid === targetUuid ? { ...cur, pinned: nextPinned } : cur,
    );

    try {
      await invoke('set_pin', { accountId, uuid: targetUuid, pinned: nextPinned });
    } catch (e) {
      console.error('pin failed', e);
      notes.set(prevNotes);
      selectedNote.set(prevSelectedNote);
      alert(`Failed to ${nextPinned ? 'pin' : 'unpin'} note: ${e}`);
    }
  }

  // ─── Pin (batch path) ────────────────────────────────────────────────
  //
  // Same shape as deleteBatch / moveBatchTo: snapshot $notes + $selectedNote,
  // flip every selected uuid in one optimistic store update, fire the
  // atomic backend call, roll back the entire batch on failure.
  async function setPinBatch(nextPinned: boolean) {
    if (!multiAccountUniform || !multiAccountId) {
      onClose();
      return;
    }
    const accountId = multiAccountId;
    const uuids = selection.map((n) => n.uuid).filter((u) => !!u);
    onClose();
    if (uuids.length === 0) return;

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);

    const uuidSet = new Set(uuids);
    notes.update((ns) =>
      ns.map((n) => (uuidSet.has(n.uuid) ? { ...n, pinned: nextPinned } : n)),
    );
    selectedNote.update((cur) =>
      cur && uuidSet.has(cur.uuid) ? { ...cur, pinned: nextPinned } : cur,
    );

    try {
      await invoke<number>('set_pin_batch', { accountId, uuids, pinned: nextPinned });
    } catch (e) {
      console.error('batch pin failed', e);
      notes.set(prevNotes);
      selectedNote.set(prevSelectedNote);
      alert(`Failed to ${nextPinned ? 'pin' : 'unpin'} ${uuids.length} note(s): ${e}`);
    }
  }

  // For multi-select, the menu shows either "Pin all" or "Unpin all" when
  // the selection is uniform, OR both items (no single combined toggle)
  // when the selection is mixed. Mixed state would be ambiguous — does
  // "Pin all" mean "pin every unpinned one" or "make every one pinned"?
  // The spec says: when uniformly pinned/unpinned, show one toggle;
  // otherwise show "Pin all" + "Unpin all" as two distinct items.
  $: multiAllPinned = isMulti && selection.every((n) => !!n.pinned);
  $: multiAllUnpinned = isMulti && selection.every((n) => !n.pinned);
  $: multiMixedPin = isMulti && !multiAllPinned && !multiAllUnpinned;

  // Inline confirm dialog (ConfirmDialog.svelte) for the permanent-delete
  // case — same reason as Sidebar.svelte's askConfirm: Tauri WKWebView's
  // native confirm() is unreliable, and an in-DOM dialog can be styled and
  // supports Enter/Esc. Only reachable when needsPermanentDeleteConfirm() is
  // true (Microsoft: no trash, hard delete) — Gmail/LocalFs keep the
  // one-click delete below. Shared between the single-note path (deleteNote)
  // and the batch path (deleteBatch) — only one of the two menu modes is
  // ever rendered at a time, so one set of state suffices for both.
  let permDeleteConfirmOpen = false;
  let permDeleteConfirmTitle = '';
  let permDeleteConfirmMessage = '';
  let permDeleteResolve: ((value: boolean) => void) | null = null;

  function askPermDeleteConfirm(dialogTitle: string, message: string): Promise<boolean> {
    permDeleteConfirmTitle = dialogTitle;
    permDeleteConfirmMessage = message;
    permDeleteConfirmOpen = true;
    return new Promise((resolve) => { permDeleteResolve = resolve; });
  }
  function permDeleteConfirmOk() {
    permDeleteConfirmOpen = false;
    permDeleteResolve?.(true);
    permDeleteResolve = null;
  }
  function permDeleteConfirmCancel() {
    permDeleteConfirmOpen = false;
    permDeleteResolve?.(false);
    permDeleteResolve = null;
  }

  async function deleteNote() {
    // No confirm() — WKWebView eats it silently in some configurations.
    // The Delete menu item is already an explicit action; trash is recoverable
    // via Gmail's "Trash" view for 30 days.
    // No `if (!note.id)` guard — local-only notes (never synced, id='') are
    // still deletable; Rust's delete_note path uses uuid + checks remote_version.
    try {
      const accountId = note.account_id || $currentAccount;
      if (!accountId) return;
      // On a backend with no trash (Microsoft) the delete below is a HARD
      // delete with no restore path — the one-click behavior above is safe
      // only because Gmail/LocalFs both have a real Trash. Ask first here,
      // using the in-DOM dialog above rather than native confirm() for the
      // same WKWebView reason. Capability-driven, never a backend_kind
      // conditional — see needsPermanentDeleteConfirm in stores/notes.ts.
      if (needsPermanentDeleteConfirm($capabilitiesByAccount[accountId])) {
        const ok = await askPermDeleteConfirm(
          `Delete "${note.title || 'Untitled'}" permanently?`,
          'This account has no Trash — the note will be deleted with no way to undo it.',
        );
        if (!ok) {
          onClose();
          return;
        }
      }
      // `note` is a snapshot captured when the context menu opened (menuNote
      // in NoteList.svelte) — it doesn't reactively track later $notes
      // updates. Right-click a note that's still mid-save (id not yet
      // assigned) and its `id` here stays '' forever. indexRemoveOnDelete
      // no-ops on an empty id, permanently orphaning the index stub that
      // indexUpsertOnSave added when the save completed — a ghost folder
      // with count 0 that lingers in the sidebar for the rest of the
      // session. Look up the note's current id from the live store by uuid
      // (stable across saves) instead of trusting the stale prop — must
      // happen before the filter below removes it from the store.
      const currentId = get(notes).find((n) => n.uuid === note.uuid)?.id || note.id;
      await invoke('delete_note', { accountId, id: note.id, uuid: note.uuid });
      notes.update((ns) => ns.filter((n) => n.uuid !== note.uuid));
      // Backend drops the note's note_tags rows; mirror that in the store so
      // the sidebar tag counts update immediately.
      setNoteTags(accountId, note.uuid, []);
      indexRemoveOnDelete(accountId, currentId);
      if ($selectedNote?.uuid === note.uuid) selectedNote.set(null);
    } catch (e) {
      console.error('delete failed', e);
    }
    onClose();
  }

  // ─── Batch (multi-select) actions ────────────────────────────────────
  //
  // Both batch paths close the menu first and run the network work in the
  // background. The UI updates optimistically and per-item; partial failures
  // get rolled back individually (move) or just logged (delete).

  // Doctrine-compliant batch delete: optimistic removal from the store
  // BEFORE the awaited invoke, single atomic backend call instead of N
  // sequential ones, rollback on failure. The user no longer sees the
  // batch land row-by-row — every selected note vanishes in one frame,
  // and either the backend confirms (in which case nothing changes) or
  // it fails (in which case every removed row comes back).
  //
  // All selected notes must belong to the same account for this to be
  // a single backend call — the menu's multiAccountUniform check
  // guarantees that's the case before this is reachable.
  async function deleteBatch() {
    if (!multiAccountUniform || !multiAccountId) {
      onClose();
      return;
    }
    const accountId = multiAccountId;
    const batch = selection.slice();
    const uuids = batch.map((n) => n.uuid).filter((u) => !!u);

    // Same hard-delete gate as deleteNote() above (Task 10b) — batch is the
    // most dangerous of the three delete surfaces: one click destroys every
    // selected note, with the least friction of the three. Must run BEFORE
    // onClose() below, same reasoning as deleteNote() — the confirm dialog
    // renders inside this still-mounted menu, and onClose() unmounts it.
    if (needsPermanentDeleteConfirm($capabilitiesByAccount[accountId])) {
      const ok = await askPermDeleteConfirm(
        `Delete ${batch.length} notes permanently?`,
        'This account has no Trash — they will be deleted with no way to undo it.',
      );
      if (!ok) {
        onClose();
        return;
      }
    }
    onClose();

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);
    // Snapshot each note's tags so we can restore them if the backend rejects
    // the batch (the backend clears note_tags as part of the delete).
    const tagSnap = new Map(
      uuids.map((u) => [u, getNoteTags(get(noteTagsByAccount), accountId, u)] as const),
    );

    notes.update((ns) => ns.filter((x) => !uuids.includes(x.uuid)));
    if ($selectedNote && uuids.includes($selectedNote.uuid)) {
      selectedNote.set(null);
    }
    // `batch`/`selection` entries can be stale snapshots (same reasoning as
    // the single-delete path above) — look each id up from `prevNotes`
    // (captured live, before the filter) rather than trusting the prop.
    const liveById = new Map(prevNotes.map((n) => [n.uuid, n.id] as const));
    for (const n of batch) indexRemoveOnDelete(accountId, liveById.get(n.uuid) || n.id);
    for (const u of uuids) setNoteTags(accountId, u, []);

    try {
      await invoke<number>('delete_notes_batch', { accountId, uuids });
    } catch (e) {
      console.error('batch delete failed', e);
      notes.set(prevNotes);
      selectedNote.set(prevSelectedNote);
      for (const [u, tags] of tagSnap) setNoteTags(accountId, u, tags);
      alert(`Failed to delete ${batch.length} note(s): ${e}`);
    }
  }

  // Doctrine-compliant batch move: optimistic label rewrite for every
  // selected note BEFORE the awaited invoke, single atomic backend call,
  // rollback the entire batch on failure. The sync worker pushes each
  // moved row to Gmail in the background — the user sees the move land
  // instantly and never waits on N round-trips.
  async function moveBatchTo(target: string) {
    if (!multiAccountUniform || !multiAccountId) {
      onClose();
      return;
    }
    const accountId = multiAccountId;
    const batch = selection.slice();
    const movingUuids = batch.filter((n) => n.label !== target).map((n) => n.uuid);
    onClose();
    if (movingUuids.length === 0) return;

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);

    notes.update((ns) =>
      ns.map((n) =>
        movingUuids.includes(n.uuid) ? { ...n, label: target } : n,
      ),
    );
    selectedNote.update((cur) =>
      cur && movingUuids.includes(cur.uuid) ? { ...cur, label: target } : cur,
    );

    try {
      await invoke<number>('move_notes_batch', {
        accountId,
        uuids: movingUuids,
        targetLabel: target,
      });
      // Keep the per-folder index in sync. The Gmail message id doesn't
      // change here — the move is a pure label rewrite in SQLite — so we
      // map source-label stub → destination-label stub by re-using the
      // same id. (Gmail-side, the sync worker will insert-new + trash-old
      // for each row, which will rotate the id; the next index refresh
      // catches that, but until then the count remains correct.)
      for (const n of batch) {
        if (movingUuids.includes(n.uuid)) {
          indexUpsertOnSave(accountId, n.id || null, n.id || '', target);
        }
      }
    } catch (e) {
      console.error('batch move failed', e);
      notes.set(prevNotes);
      selectedNote.set(prevSelectedNote);
      alert(`Failed to move ${batch.length} note(s): ${e}`);
    }
  }

  // Close on outside click or Esc. Pointerdown fires before click, so we catch
  // it earlier — avoids the "menu closes on the click that should trigger an item"
  // race when items use onclick.
  // While the permanent-delete confirm dialog is open, it — not this
  // handler — owns outside-click/Esc: it renders on top of `.context-menu`
  // (this component stays mounted so the dialog has somewhere to render),
  // and letting this handler fire `onClose()` here would unmount the whole
  // component (NoteList's `menuNote = null`) with the confirm Promise still
  // pending, wedging `deleteNote()` on a promise nothing will ever resolve.
  function onPointerDown(e: PointerEvent) {
    if (permDeleteConfirmOpen) return;
    const target = e.target as HTMLElement;
    if (!target.closest('.context-menu')) onClose();
  }
  function onKey(e: KeyboardEvent) {
    if (permDeleteConfirmOpen) return;
    if (e.key === 'Escape') onClose();
  }

  onMount(() => {
    window.addEventListener('pointerdown', onPointerDown, true);
    window.addEventListener('keydown', onKey);
  });
  onDestroy(() => {
    window.removeEventListener('pointerdown', onPointerDown, true);
    window.removeEventListener('keydown', onKey);
  });

  // Keep the menu within the viewport — flip up/left if it would overflow.
  let menuEl: HTMLDivElement;
  let adjustedX = x;
  let adjustedY = y;
  $: if (menuEl) {
    const rect = menuEl.getBoundingClientRect();
    if (x + rect.width > window.innerWidth) adjustedX = window.innerWidth - rect.width - 8;
    if (y + rect.height > window.innerHeight) adjustedY = window.innerHeight - rect.height - 8;
  }

  // Submenu viewport-fit: when the user hovers a submenu-anchor, measure whether
  // the child .submenu would overflow the right or bottom edge and toggle CSS
  // classes to flip it. Mirrors the Sidebar folder-menu adjustedX/adjustedY approach
  // but applied to nested CSS-hover submenus where absolute positioning is inherited.
  function fitSubmenu(e: MouseEvent) {
    const anchor = (e.currentTarget as HTMLElement);
    const sub = anchor.querySelector(':scope > .submenu') as HTMLElement | null;
    if (!sub) return;
    // Temporarily make it visible (without display:block, getBoundingClientRect returns 0).
    const prevDisplay = sub.style.display;
    sub.style.display = 'block';
    sub.style.visibility = 'hidden';
    const rect = sub.getBoundingClientRect();
    sub.style.display = prevDisplay;
    sub.style.visibility = '';
    // Flip horizontally if it would overflow the right edge.
    if (rect.right > window.innerWidth - 8) {
      sub.classList.add('flip-left');
    } else {
      sub.classList.remove('flip-left');
    }
    // Clamp vertically if it would overflow the bottom edge.
    const overflow = rect.bottom - (window.innerHeight - 8);
    if (overflow > 0) {
      const newTop = Math.max(-(rect.height - 40), -overflow);
      sub.style.top = `${newTop}px`;
    } else {
      sub.style.top = '';
    }
  }
</script>


<div
  bind:this={menuEl}
  class="context-menu"
  style="left: {adjustedX}px; top: {adjustedY}px"
  role="menu"
>
  {#if isMulti}
    <div class="multi-header" title="Acts on every selected note">
      {multiCount} notes selected
    </div>
    <div class="sep"></div>
    {#if !canWriteSidecars}
      <div class="multi-header">Pin isn't available on this account yet</div>
    {:else if multiAllPinned}
      <button class="item" onclick={() => setPinBatch(false)}>
        <span class="icon"><Icon name="pin" /></span>
        <span class="label">Unpin all</span>
      </button>
    {:else if multiAllUnpinned}
      <button class="item" onclick={() => setPinBatch(true)}>
        <span class="icon"><Icon name="pin" /></span>
        <span class="label">Pin all</span>
      </button>
    {:else if multiMixedPin}
      <button class="item" onclick={() => setPinBatch(true)}>
        <span class="icon"><Icon name="pin" /></span>
        <span class="label">Pin all</span>
      </button>
      <button class="item" onclick={() => setPinBatch(false)}>
        <span class="icon"><Icon name="pin" /></span>
        <span class="label">Unpin all</span>
      </button>
    {/if}
  {/if}

  {#if !isMulti}
    {#if canWriteSidecars}
    <button class="item" onclick={togglePinOne}>
      <span class="icon"><Icon name="pin" /></span>
      <span class="label">{note.pinned ? 'Unpin' : 'Pin'}</span>
    </button>
    {/if}
    {#if canWriteSidecars && canWriteNotes}
    <div class="sep"></div>
    {/if}
    {#if canWriteNotes}
    <button class="item" onclick={newNoteHere}>
      <span class="icon">＋</span>
      <span class="label">New Note</span>
    </button>
    <button class="item" onclick={duplicateNote} disabled={!note.id}>
      <span class="icon"><Icon name="copy" /></span>
      <span class="label">Duplicate</span>
    </button>
    {/if}
    <button
      class="item"
      onclick={refetchFromGmail}
      disabled={!note.id}
      title="Bypass cache and re-pull this note's content from Gmail"
    >
      <span class="icon"><Icon name="refresh" /></span>
      <span class="label">Refetch from Gmail</span>
    </button>
    {#if canWriteNotes && canWriteFolders && hasSourceBlock(note.body_html ?? '')}
      <button
        class="item"
        onclick={reExtractLessons}
        title="Re-run the lesson extractor over this note's Source block"
      >
        <span class="icon"><Icon name="bulb" /></span>
        <span class="label">Re-extract</span>
      </button>
    {/if}
    {#if canWriteNotes}
      <button class="item" onclick={linkIntoWiki} title="Find and link related notes">
        <span class="icon"><Icon name="graph" /></span>
        <span class="label">Link into wiki</span>
      </button>
    {/if}
  {/if}

  <!-- Delete grouped with the other direct actions above, rather than
       isolated after "Move to" — a browsable folder list, not a single
       action, so it sits last. Delete + Move to are both `Write::Notes`
       (delete_note/delete_notes_batch, save_note/move_notes_batch). -->
  {#if canWriteNotes}
  <div class="sep"></div>
  <button class="item danger" onclick={() => (isMulti ? deleteBatch() : deleteNote())}>
    <span class="icon"><Icon name="trash" /></span>
    <span class="label">{isMulti ? `Delete ${multiCount} notes` : 'Delete'}</span>
  </button>
  <div class="sep"></div>

  <!-- role="none" suppresses a11y lint: onmouseenter is for viewport-fit measurement only, not interaction -->
  <div class="submenu-anchor" role="none" onmouseenter={fitSubmenu}>
    <button class="item has-submenu" type="button">
      <span class="icon"><Icon name="folder" /></span>
      <span class="label">Move to</span>
      <span class="chevron"><Icon name="chevron-right" size={12} /></span>
    </button>
    <div class="submenu">
      {#each accountEntries as [acct, folders] (acct)}
        {@const allowedAcct = isMulti ? multiAccountId : noteAccountId}
        {@const acctEnabled = acct === allowedAcct && (!isMulti || multiAccountUniform)}
        <div class="submenu-anchor" role="none" onmouseenter={fitSubmenu}>
          <button
            class="item has-submenu"
            class:disabled-account={!acctEnabled}
            disabled={!acctEnabled}
            title={!acctEnabled
              ? (isMulti && !multiAccountUniform
                  ? 'Selected notes span multiple accounts — not yet supported'
                  : 'Cross-account move not supported yet')
              : accountDisplay($activeAccounts.find((a) => a.id === acct), acct)}
            type="button"
          >
            <span class="icon"><Icon name="person" /></span>
            <span class="label">{accountDisplay($activeAccounts.find((a) => a.id === acct), acct)}</span>
            <span class="chevron"><Icon name="chevron-right" size={12} /></span>
          </button>
          {#if acctEnabled}
            <div class="submenu folder-list">
              {#each folders as row (row.path)}
                {@const isCurrentFolder = !isMulti && row.path === note.label}
                <button
                  class="item folder"
                  class:current={isCurrentFolder}
                  style="padding-left: {28 + row.depth * 14}px"
                  onclick={() => (isMulti ? moveBatchTo(row.path) : moveTo(row.path))}
                  disabled={isCurrentFolder}
                  title={row.path}
                  type="button"
                >
                  <span class="label">{row.name}</span>
                  {#if isCurrentFolder}
                    <span class="check" role="img" aria-label="Current folder"><Icon name="check" size={12} /></span>
                  {/if}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </div>
  {/if}
</div>

{#if permDeleteConfirmOpen}
  <ConfirmDialog
    title={permDeleteConfirmTitle}
    message={permDeleteConfirmMessage}
    confirmLabel="Delete permanently"
    destructive={true}
    onConfirm={permDeleteConfirmOk}
    onCancel={permDeleteConfirmCancel}
  />
{/if}

<style>
  .context-menu {
    position: fixed;
    min-width: 220px;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-menu);
    padding: 4px;
    z-index: 1000;
    font-size: var(--size);
  }

  /* Cascading submenu container. The anchor stays in normal flow; the
     .submenu inside it is absolutely positioned to the right of the
     parent button and only appears on hover. Nesting works recursively
     because every level uses the same .submenu-anchor structure. */
  .submenu-anchor {
    position: relative;
  }
  .submenu-anchor > .submenu {
    display: none;
    position: absolute;
    left: 100%;
    top: -4px;            /* align with parent's first item, accounting for menu padding */
    min-width: 220px;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-menu);
    padding: 4px;
    z-index: 1001;        /* above the parent menu */
    /* NO overflow here — overflow on a parent .submenu would clip its
       absolutely-positioned grandchild .submenu (the folder list living
       to the right). Apply overflow only on the leaf list below. */
  }
  /* Viewport-fit: flip submenu to the left when it would overflow the right edge.
     The class is added by fitSubmenu() at runtime, so use :global to prevent
     Svelte's unused-selector warning from removing it. */
  :global(.submenu-anchor > .submenu.flip-left) {
    left: auto;
    right: 100%;
  }
  /* The actual long list (folders) — this is the only level that can
     exceed the viewport, so this is the only one that needs scroll.
     max-height must be small enough that the box always fits within the
     viewport regardless of where the submenu anchor opens. `calc(100vh -
     24px)` was a bug: when the anchor is partway down the screen, the
     box's bottom extends past the viewport edge, making the bottom items
     unreachable. 60vh keeps the box compact enough to fit even when the
     anchor is at the middle of the screen, while still showing ~15 rows. */
  .submenu-anchor > .submenu.folder-list {
    max-height: 60vh;
    overflow-y: auto;
  }
  /* Reveal one level at a time. Without :hover on the anchor, the
     submenu stays hidden even if the parent menu is open. */
  .submenu-anchor:hover > .submenu {
    display: block;
  }
  .chevron {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    margin-left: auto;
    color: var(--text-disabled);
    font-size: var(--size-xs);
  }
  .item.has-submenu {
    /* Visual cue that this item leads somewhere — and prevents the row
       from feeling like a dead-end since the click handler will still
       fire (moving to the folder), but the hover-only submenu is the
       primary affordance. */
    cursor: default;
  }

  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 7px 12px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    color: var(--text);
    border-radius: 4px;
    transition: background 0.1s;
  }

  .item:hover:not(:disabled):not(.header) {
    background: var(--surface-hover);
  }

  .item:disabled {
    cursor: default;
    opacity: 0.4;
    /* Defeat the .item:hover background so disabled rows look inert even
       when the cursor is over them. Without this, a disabled cross-account
       folder still highlighted blue on hover and looked clickable. */
    background: transparent !important;
  }

  .item.folder {
    padding-top: 5px;
    padding-bottom: 5px;
  }

  .item.folder.current {
    color: var(--text-muted);
  }

  .item.danger {
    color: var(--danger);
  }

  .icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    text-align: center;
    flex-shrink: 0;
  }

  .label {
    flex: 1;
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .check {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--text-muted);
    font-size: var(--size-xs);
  }

  .sep {
    height: 1px;
    background: var(--surface-active);
    margin: 4px 8px;
  }

  .multi-header {
    font-size: var(--size-xs);
    font-weight: 600;
    color: var(--accent-action);
    background: var(--accent-wash);
    padding: 6px 12px;
    border-radius: 4px;
    margin: 2px 4px 0;
  }

  /* Permanent-delete confirm dialog now lives in ConfirmDialog.svelte — see
     the {#if permDeleteConfirmOpen} block above. */
</style>
