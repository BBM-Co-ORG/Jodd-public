<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { notes, selectedFolder, selectedNote, isAuthenticated, accounts, currentAccount, refreshNotes, noteIndex, hydratedFolders, indexRewriteOnFolderRename, selectedTags, tagMatchMode, toggleSelectedTag, clearSelectedTags, tagsByAccount, renameTagInStore, deleteTagFromStore, setAccountNoteTags } from '../stores/notes';
  import type { Note, Account, DedupSummary } from '../types';
  import DupReviewModal from './DupReviewModal.svelte';
  import AccountSettings from './AccountSettings.svelte';
  import { onMount, onDestroy, createEventDispatcher } from 'svelte';
  import { get } from 'svelte/store';

  // Width is owned by App.svelte so the user can drag a resizer between panes.
  // Default keeps the original 200px look if the parent doesn't pass one.
  export let width: number = 200;
  const dispatch = createEventDispatcher<{ collapse: void }>();

  // Per-path expand state for the folder tree. Defaults to expanded for the
  // implicit "Notes" root of every account so a fresh session still shows
  // top-level folders without the user having to twist them open. Other
  // sub-folders start collapsed (file-explorer convention).
  let expandedPaths: Record<string, boolean> = {};

  function toggleExpand(e: MouseEvent | KeyboardEvent, path: string) {
    e.stopPropagation();
    expandedPaths = { ...expandedPaths, [path]: !expandedPaths[path] };
  }

  // A row is visible only if every ancestor path is expanded. The implicit
  // "Notes" row itself is always visible (top of every account section).
  function isRowVisible(path: string, expanded: Record<string, boolean>): boolean {
    if (!path.includes('/')) return true;
    const segs = path.split('/');
    for (let i = 1; i < segs.length; i++) {
      const ancestor = segs.slice(0, i).join('/');
      if (!expanded[ancestor]) return false;
    }
    return true;
  }

  function rowHasChildren(rows: FolderRow[], path: string): boolean {
    const prefix = path + '/';
    return rows.some((r) => r.path.startsWith(prefix));
  }

  // Each unique label path → its own row. Depth = number of '/' in the path,
  // which the template uses as the indent multiplier. The displayed name is
  // the last path segment ("mySubNotes") not the full path ("Notes/myNotes/mySubNotes").
  type FolderRow = { path: string; name: string; depth: number; count: number };

  // Per-account folder cache. Each account's "Notes/..." labels — including
  // empty ones the user just created — live here. Keyed by accountId.
  // Apple Notes uses the same model: each account section has its own tree.
  let foldersByAccount: Record<string, string[]> = {};

  // Latest-call-wins sequence number. Multiple refreshFolders can be in
  // flight at once — the reactive `$: if ($accounts...)` fires whenever
  // accounts change, and the oauth-success handler kicks off its own call.
  // Without this counter, an in-flight call started with the OLD account
  // list could finish AFTER one that saw the NEW list and silently
  // overwrite the new account's folders with undefined.
  let refreshFoldersSeq = 0;
  async function refreshFolders() {
    const seq = ++refreshFoldersSeq;
    const list = get(accounts);
    if (list.length === 0) {
      foldersByAccount = {};
      return;
    }
    try {
      const results = await Promise.allSettled(
        list.map((a) =>
          invoke<string[]>('list_folders', { accountId: a.id })
            .then((folders) => [a.id, folders] as const),
        ),
      );
      // If a newer refreshFolders started while we were awaiting, drop our
      // result and let the newer one own foldersByAccount.
      if (seq !== refreshFoldersSeq) {
        console.log(`[jodd] refreshFolders: dropping stale seq=${seq}`);
        return;
      }
      const next: Record<string, string[]> = {};
      results.forEach((r, i) => {
        if (r.status === 'fulfilled') {
          const [id, folders] = r.value;
          next[id] = folders;
          console.log(`[jodd] refreshFolders: ${id} → ${folders.length} folders`);
        } else {
          // Surface rejections — a silent failure here was leaving the new
          // account's folders blank in the sidebar.
          console.error(`[jodd] refreshFolders REJECTED for ${list[i].id}:`, r.reason);
        }
      });
      foldersByAccount = next;
    } catch (e) {
      console.error('refreshFolders failed', e);
    }
  }

  // Refresh on mount, whenever the account list changes, AND whenever the note
  // index lands. The index arrives a moment after mount on cold start (via
  // index_account), and that same backend pass now reconciles the folders
  // cache — so re-reading list_folders here is what surfaces empty folders
  // (e.g. `Notes/play2`) that have no notes to infer their path from.
  // refreshFolders is a cheap pure-SQLite read with its own in-flight guard,
  // so firing it a few extra times during cold start is harmless.
  $: if ($accounts.length > 0) {
    void $noteIndex;
    refreshFolders();
  }

  // Auto-expand the implicit "Notes" root once we know the account list, so a
  // fresh app launch shows the top-level tree without the user clicking ▸.
  // Only add — never overwrite — so user toggles survive subsequent refreshes.
  $: if ($accounts.length > 0 && !expandedPaths['Notes']) {
    expandedPaths = { ...expandedPaths, Notes: true };
  }

  // Whenever selectedFolder points at a sub-path, auto-expand every ancestor
  // along the way. Without this, navigating to a deep folder (via folder
  // create, programmatic set, restoration, etc.) leaves the row rendered
  // but hidden under collapsed parents — looks like a deletion bug.
  $: {
    const path = $selectedFolder;
    if (path && path !== '__ALL__' && path.includes('/')) {
      const segs = path.split('/');
      let needsUpdate = false;
      const updates: Record<string, boolean> = {};
      for (let i = 1; i < segs.length; i++) {
        const ancestor = segs.slice(0, i).join('/');
        if (!expandedPaths[ancestor]) {
          updates[ancestor] = true;
          needsUpdate = true;
        }
      }
      if (needsUpdate) expandedPaths = { ...expandedPaths, ...updates };
    }
  }

  // Per-account folder counts. Strategy by folder:
  //   - If the folder has been HYDRATED this session ($hydratedFolders),
  //     the live $notes count is truth. SQLite is authoritative; the index
  //     can carry ghost stubs from server-side cleanup events the frontend
  //     didn't observe (e.g. cleanup_stale_uuid_duplicates).
  //   - Otherwise use the server-side INDEX. It lands within seconds of
  //     sign-in even for 6k-note mailboxes, so counts are accurate long
  //     before bodies arrive — but it never updates after that snapshot,
  //     so for hydrated folders the live count beats it.
  $: folderCountsByAccount = (() => {
    // Live counts from $notes (per account, per label).
    const liveByAcct: Record<string, Record<string, number>> = {};
    for (const n of $notes) {
      const aid = n.account_id ?? '';
      if (!aid) continue;
      if (!liveByAcct[aid]) liveByAcct[aid] = {};
      liveByAcct[aid][n.label] = (liveByAcct[aid][n.label] || 0) + 1;
    }
    // Index counts (per account, per label).
    const indexByAcct: Record<string, Record<string, number>> = {};
    for (const [accountId, idx] of $noteIndex) {
      const m: Record<string, number> = {};
      for (const stub of idx) m[stub.label] = (m[stub.label] || 0) + 1;
      indexByAcct[accountId] = m;
    }
    // Per-folder choice. Iterate every (account, label) seen in either map.
    const out: Record<string, Record<string, number>> = {};
    const allAccts = new Set<string>([
      ...Object.keys(liveByAcct),
      ...Object.keys(indexByAcct),
    ]);
    for (const aid of allAccts) {
      const hydrated = $hydratedFolders.get(aid) ?? new Set<string>();
      const live = liveByAcct[aid] ?? {};
      const idx = indexByAcct[aid] ?? {};
      const allPaths = new Set<string>([
        ...Object.keys(live),
        ...Object.keys(idx),
      ]);
      const m: Record<string, number> = {};
      for (const path of allPaths) {
        m[path] = hydrated.has(path) ? (live[path] ?? 0) : (idx[path] ?? live[path] ?? 0);
      }
      out[aid] = m;
    }
    return out;
  })();

  // Build the folder tree for ONE account from:
  //   1) labels of that account's notes (folderCountsByAccount)
  //   2) any "Notes/..." paths from list_folders (foldersByAccount)
  //   3) the implicit "Notes" root
  function buildRows(
    counts: Record<string, number>,
    fromBackend: string[],
  ): FolderRow[] {
    const paths = new Set<string>(['Notes']);
    for (const p of Object.keys(counts)) {
      const segs = p.split('/');
      for (let i = 1; i <= segs.length; i++) paths.add(segs.slice(0, i).join('/'));
    }
    for (const p of fromBackend) {
      const segs = p.split('/');
      for (let i = 1; i <= segs.length; i++) paths.add(segs.slice(0, i).join('/'));
    }
    return Array.from(paths)
      .map((path) => {
        const segs = path.split('/');
        return {
          path,
          name: segs[segs.length - 1],
          depth: segs.length - 1,
          count: counts[path] || 0,
        };
      })
      .sort((a, b) => a.path.localeCompare(b.path));
  }

  // Top-level reactive: rows for every account, recomputed whenever any of
  // the underlying maps change. The template reads from this map instead of
  // calling buildRows(acct.id) inline — that previously hid the dependency
  // on folderCountsByAccount/foldersByAccount from Svelte's reactivity, so
  // the sub-folder tree never re-rendered after the data arrived.
  $: rowsByAccount = (() => {
    const out: Record<string, FolderRow[]> = {};
    for (const a of $accounts) {
      out[a.id] = buildRows(
        folderCountsByAccount[a.id] ?? {},
        foldersByAccount[a.id] ?? [],
      );
    }
    return out;
  })();

  // ─── Duplicate-count surfacing ──────────────────────────────────────
  // Passive indicator: the backend's list_notes pipeline already collapses
  // Gmail-side duplicate messages by uuid and records the count per account.
  // We expose that count as a tiny pill in the account header so the user
  // has a signal that cleanup_orphans is worth running. Click = run cleanup.
  let dupStats: Record<string, DedupSummary> = {};
  let cleanupResult: Record<string, string> = {};
  // Which account, if any, is currently being reviewed in the modal.
  let reviewingAccount: string | null = null;

  async function refreshDupStats(accountId: string) {
    try {
      const s = await invoke<DedupSummary>('get_dup_stats', { accountId });
      dupStats = { ...dupStats, [accountId]: s };
    } catch {
      // get_dup_stats can't really fail — but if it does, just leave the
      // previous value in place rather than zeroing the pill mid-flight.
    }
  }

  // Reactive: whenever the notes store changes (signals that a list_notes
  // pass just completed and the backend updated its summary), refetch the
  // stats for every signed-in account. Cheap — the command is a Mutex read.
  $: if ($notes && $accounts.length > 0) {
    for (const a of $accounts) refreshDupStats(a.id);
  }

  function openReview(accountId: string) {
    reviewingAccount = accountId;
  }

  function closeReview() {
    reviewingAccount = null;
  }

  function onTrashed(accountId: string, count: number) {
    cleanupResult = {
      ...cleanupResult,
      [accountId]: count === 0 ? 'none trashed' : `trashed ${count}`,
    };
    refreshDupStats(accountId);
    setTimeout(() => {
      cleanupResult = { ...cleanupResult, [accountId]: '' };
    }, 4000);
  }

  // Clicking a folder under account X sets BOTH currentAccount AND
  // selectedFolder atomically. The order matters: switching account first
  // lets downstream observers (NoteList filter, loadFolderNotes scope)
  // see the correct context by the time selectedFolder fires.
  function selectFolder(accountId: string, path: string) {
    if ($currentAccount !== accountId) {
      currentAccount.set(accountId);
    }
    // Folder and tag views are mutually exclusive — entering a folder leaves
    // any active tag filter.
    clearSelectedTags();
    selectedFolder.set(path);
    selectedNote.set(null);
  }

  // Toggle a tag in/out of the multi-tag filter. Parallel to selectFolder;
  // App.svelte's $selectedTags reactive paints the union from cache, and
  // NoteList narrows to AND/OR. Tags are per-account, so switching account
  // starts a fresh selection.
  function selectTag(accountId: string, tag: string) {
    if ($currentAccount !== accountId) {
      currentAccount.set(accountId);
      clearSelectedTags();
    }
    toggleSelectedTag(tag);
    selectedNote.set(null);
  }

  // ─── Tag context menu (right-click a sidebar tag) ─────────────────────────
  // Mirrors the folder menu: Rename / Delete operate globally across every note
  // in the account. Reuses the .folder-menu styling and viewport-fit snap.
  let tagMenuName: string | null = null;
  let tagMenuAccountId: string | null = null;
  let tagMenuX = 0;
  let tagMenuY = 0;
  let tagMenuEl: HTMLDivElement | undefined;
  let tagMenuAdjustedX = 0;
  let tagMenuAdjustedY = 0;
  $: if (tagMenuEl && tagMenuName !== null) {
    const rect = tagMenuEl.getBoundingClientRect();
    tagMenuAdjustedX =
      tagMenuX + rect.width > window.innerWidth
        ? Math.max(8, window.innerWidth - rect.width - 8)
        : tagMenuX;
    tagMenuAdjustedY =
      tagMenuY + rect.height > window.innerHeight
        ? Math.max(8, window.innerHeight - rect.height - 8)
        : tagMenuY;
  }

  function openTagMenu(e: MouseEvent, accountId: string, tag: string) {
    e.preventDefault();
    e.stopPropagation();
    tagMenuX = e.clientX;
    tagMenuY = e.clientY;
    tagMenuAdjustedX = e.clientX;
    tagMenuAdjustedY = e.clientY;
    tagMenuAccountId = accountId;
    tagMenuName = tag;
  }
  function closeTagMenu() {
    tagMenuName = null;
    tagMenuAccountId = null;
  }

  // Re-pull an account's tag map from SQLite — used to roll back the optimistic
  // store mutation if the backend op fails (the DB is unchanged on failure).
  async function reloadAccountTags(accountId: string) {
    try {
      const rows = await invoke<{ uuid: string; tag: string }[]>('list_note_tags', { accountId });
      setAccountNoteTags(accountId, rows);
    } catch (e) {
      console.error('reloadAccountTags failed', e);
    }
  }

  async function renameTag(accountId: string, tag: string) {
    closeTagMenu();
    const input = await askName(`Rename tag "${tag}"`, tag);
    if (input === null) return;
    // Client normalization mirrors the backend (trim, lowercase, drop spaces/
    // control/#). Unicode-friendly so Thai survives.
    const next = input.trim().toLowerCase().replace(/[#\p{White_Space}\p{Cc}]/gu, '');
    if (!next || next === tag) return;
    renameTagInStore(accountId, tag, next); // optimistic
    try {
      await invoke('rename_tag', { accountId, oldTag: tag, newTag: next });
    } catch (e) {
      await reloadAccountTags(accountId); // rollback from DB
      alert(`Failed to rename tag: ${e}`);
    }
  }

  async function deleteTag(accountId: string, tag: string) {
    closeTagMenu();
    const ok = await askConfirm('Delete tag?', `Remove #${tag} from every note in this account?`);
    if (!ok) return;
    deleteTagFromStore(accountId, tag); // optimistic
    try {
      await invoke('delete_tag', { accountId, tag });
    } catch (e) {
      await reloadAccountTags(accountId); // rollback from DB
      alert(`Failed to delete tag: ${e}`);
    }
  }

  // ─── Folder context menu ──────────────────────────────────────────────────
  // Right-click on a folder shows: New sub-folder / Rename / Delete.
  // We track BOTH the folder path AND the account it belongs to — folder ops
  // run against a specific account's Gmail, not necessarily the current one.
  let menuPath: string | null = null;
  let menuAccountId: string | null = null;
  let menuX = 0;
  let menuY = 0;
  // Viewport-fit (D9 fix): on render, getBoundingClientRect of the menu and
  // snap left/top back inside the viewport. Without this, opening the menu
  // near the bottom of a tall sidebar (every folder + the move-to list)
  // pushes Delete off the screen, where it can't be clicked. CSS
  // `max-height: calc(100vh - 16px); overflow-y: auto;` on the menu itself
  // then guarantees the menu fits even when the inline move-to list alone
  // is taller than the viewport.
  let menuEl: HTMLDivElement | undefined;
  let menuAdjustedX = 0;
  let menuAdjustedY = 0;
  $: if (menuEl && menuPath !== null) {
    const rect = menuEl.getBoundingClientRect();
    menuAdjustedX =
      menuX + rect.width > window.innerWidth
        ? Math.max(8, window.innerWidth - rect.width - 8)
        : menuX;
    menuAdjustedY =
      menuY + rect.height > window.innerHeight
        ? Math.max(8, window.innerHeight - rect.height - 8)
        : menuY;
  }

  function openFolderMenu(e: MouseEvent, accountId: string, path: string) {
    e.preventDefault();
    e.stopPropagation();
    menuX = e.clientX;
    menuY = e.clientY;
    // Seed adjusted coords with the raw click so the first frame paints at
    // a sensible position. The reactive above will fix them up once menuEl
    // binds and we have a real bounding rect to measure against.
    menuAdjustedX = e.clientX;
    menuAdjustedY = e.clientY;
    menuAccountId = accountId;
    menuPath = path;
  }

  function closeFolderMenu() {
    menuPath = null;
    menuAccountId = null;
  }

  function onWindowKey(e: KeyboardEvent) {
    if (e.key === 'Escape') { closeFolderMenu(); closeTagMenu(); }
  }
  function onWindowPointerDown(e: PointerEvent) {
    if (!(e.target as HTMLElement).closest('.folder-menu')) { closeFolderMenu(); closeTagMenu(); }
  }

  onMount(() => {
    window.addEventListener('keydown', onWindowKey);
    window.addEventListener('pointerdown', onWindowPointerDown, true);
  });
  onDestroy(() => {
    window.removeEventListener('keydown', onWindowKey);
    window.removeEventListener('pointerdown', onWindowPointerDown, true);
  });

  // ─── Inline prompt dialog ─────────────────────────────────────────────────
  // window.prompt() is not implemented in macOS WKWebView, so we render an
  // inline overlay with an input field instead. State is small enough to
  // keep here rather than a dedicated component.
  let promptOpen = false;
  let promptTitle = '';
  let promptValue = '';
  let promptInputEl: HTMLInputElement | undefined;
  let promptResolve: ((value: string | null) => void) | null = null;

  function askName(title: string, defaultValue: string = ''): Promise<string | null> {
    promptTitle = title;
    promptValue = defaultValue;
    promptOpen = true;
    // Focus the input next frame, after Svelte renders the dialog.
    setTimeout(() => {
      promptInputEl?.focus();
      promptInputEl?.select();
    }, 0);
    return new Promise((resolve) => { promptResolve = resolve; });
  }

  function promptOk() {
    const v = promptValue.trim();
    promptOpen = false;
    promptResolve?.(v.length ? v : null);
    promptResolve = null;
  }
  function promptCancel() {
    promptOpen = false;
    promptResolve?.(null);
    promptResolve = null;
  }
  function onPromptKey(e: KeyboardEvent) {
    if (e.key === 'Enter') { e.preventDefault(); promptOk(); }
    else if (e.key === 'Escape') { e.preventDefault(); promptCancel(); }
  }

  // Inline confirm dialog. Same reason as the prompt above — Tauri WKWebView's
  // native confirm() can be unreliable, and an in-DOM dialog can be styled
  // and supports Enter/Esc keyboard shortcuts.
  let confirmOpen = false;
  let confirmTitle = '';
  let confirmMessage = '';
  let confirmResolve: ((value: boolean) => void) | null = null;

  function askConfirm(title: string, message: string): Promise<boolean> {
    confirmTitle = title;
    confirmMessage = message;
    confirmOpen = true;
    return new Promise((resolve) => { confirmResolve = resolve; });
  }
  function confirmOk() {
    confirmOpen = false;
    confirmResolve?.(true);
    confirmResolve = null;
  }
  function confirmCancel() {
    confirmOpen = false;
    confirmResolve?.(false);
    confirmResolve = null;
  }
  function onConfirmKey(e: KeyboardEvent) {
    if (e.key === 'Enter') { e.preventDefault(); confirmOk(); }
    else if (e.key === 'Escape') { e.preventDefault(); confirmCancel(); }
  }

  // Create a fresh tmp blank note labelled with the given folder under the
  // given account. Lifecycle same as the + button in NoteList — tmp: UUID,
  // autosave on first keystroke, dropped if abandoned.
  function newNoteInFolder(accountId: string, path: string) {
    closeFolderMenu();
    const tmpUuid = 'tmp:' + Math.random().toString(36).slice(2, 10);
    const blank: Note = {
      id: '',
      uuid: tmpUuid,
      title: 'New Note',
      body_html: '<html><head></head><body></body></html>',
      date: new Date().toISOString(),
      label: path,
      x_mail_created_date: null,
      account_id: accountId,
    };
    // Switch context to the account so the editor shows the right thing.
    if ($currentAccount !== accountId) currentAccount.set(accountId);
    selectedFolder.set(path);
    notes.update((ns) => [blank, ...ns]);
    selectedNote.set(blank);
  }

  // ─── Folder operations ────────────────────────────────────────────────────
  // All take an explicit accountId — each Gmail account has its own label
  // namespace, so we can't rely on $currentAccount inside the handler.

  // Doctrine-compliant: optimistic write to the store FIRST, then invoke,
  // then rollback on failure. Backend `create_folder` is local-first (it
  // commits to SQLite synchronously and the worker pushes to Gmail in the
  // background), so the only way the await can fail is a validation error
  // (duplicate name, empty name, etc.) — quick + deterministic. We do the
  // ancestor-expand and selection move BEFORE awaiting so the user sees
  // the new folder land instantly.
  //
  // The predicted path matches what `create_folder` constructs internally
  // (`{parent}/{segment}` or `Notes/{segment}`). If the backend disagrees
  // we log and let refreshFolders() sort it out — that's a code-version
  // skew situation, not a user-visible issue.
  async function createFolderUnder(accountId: string, parentPath: string | null) {
    closeFolderMenu();
    const name = await askName(
      parentPath ? `New sub-folder under "${parentPath}"` : 'New folder name'
    );
    if (!name) return;

    const segment = name.trim();
    if (!segment) return;
    const predicted = parentPath ? `${parentPath}/${segment}` : `Notes/${segment}`;

    const prevByAccount = foldersByAccount;
    const prevExpanded = expandedPaths;
    const prevSelectedFolder = get(selectedFolder);
    const prevCurrentAccount = get(currentAccount);

    // Optimistic store mutation: folder list, expanded ancestors, focus.
    foldersByAccount = {
      ...foldersByAccount,
      [accountId]: [...(foldersByAccount[accountId] ?? []), predicted],
    };
    const segs = predicted.split('/');
    const ancestorUpdates: Record<string, boolean> = {};
    for (let i = 1; i < segs.length; i++) {
      ancestorUpdates[segs.slice(0, i).join('/')] = true;
    }
    expandedPaths = { ...expandedPaths, ...ancestorUpdates };
    if ($currentAccount !== accountId) currentAccount.set(accountId);
    selectedFolder.set(predicted);

    try {
      const result = await invoke<{ id: string; name: string }>('create_folder', {
        accountId,
        name,
        parentPath,
      });
      if (result.name !== predicted) {
        console.warn('[jodd] create_folder: predicted', predicted, 'returned', result.name);
        // Reconcile the store with the actual path. Cheap — the row count
        // doesn't change, just the string.
        foldersByAccount = {
          ...foldersByAccount,
          [accountId]: (foldersByAccount[accountId] ?? []).map((p) =>
            p === predicted ? result.name : p,
          ),
        };
        if (get(selectedFolder) === predicted) selectedFolder.set(result.name);
      }
      refreshFolders();
    } catch (e) {
      foldersByAccount = prevByAccount;
      expandedPaths = prevExpanded;
      selectedFolder.set(prevSelectedFolder);
      if (prevCurrentAccount && get(currentAccount) !== prevCurrentAccount) {
        currentAccount.set(prevCurrentAccount);
      }
      alert(`Failed to create folder: ${e}`);
    }
  }

  // Doctrine-compliant: optimistic rename + selection-fix BEFORE the
  // await. Backend rename_folder runs a single SQLite transaction
  // (rename_subtree) and returns; failure modes are validation errors
  // surfaced from validate_folder_segment / sibling-collision check.
  async function renameFolder(accountId: string, path: string) {
    closeFolderMenu();
    if (path === 'Notes') {
      alert("The root 'Notes' folder can't be renamed.");
      return;
    }
    const currentName = path.split('/').pop() ?? '';
    const next = await askName(`Rename "${path}"`, currentName);
    if (!next || next === currentName) return;

    const parentPath = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '';
    const predicted = parentPath ? `${parentPath}/${next}` : next;

    const prevByAccount = foldersByAccount;
    const prevNotes = get(notes);
    const prevIndex = get(noteIndex);
    const prevSelected = get(selectedFolder);

    const rewriteLabel = (label: string): string => {
      if (label === path) return predicted;
      if (label.startsWith(`${path}/`)) return predicted + label.slice(path.length);
      return label;
    };
    foldersByAccount = {
      ...foldersByAccount,
      [accountId]: (foldersByAccount[accountId] ?? []).map(rewriteLabel),
    };
    notes.update((ns) =>
      ns.map((n) =>
        n.account_id === accountId && (n.label === path || n.label.startsWith(`${path}/`))
          ? { ...n, label: rewriteLabel(n.label) }
          : n,
      ),
    );
    indexRewriteOnFolderRename(accountId, path, predicted);
    if ($selectedFolder === path && $currentAccount === accountId) {
      selectedFolder.set(predicted);
    }

    try {
      const result = await invoke<{ id: string; name: string }>('rename_folder', {
        accountId,
        path,
        newName: next,
      });
      if (result.name !== predicted) {
        console.warn('[jodd] rename_folder: predicted', predicted, 'returned', result.name);
      }
      refreshFolders();
    } catch (e) {
      foldersByAccount = prevByAccount;
      notes.set(prevNotes);
      noteIndex.set(prevIndex);
      selectedFolder.set(prevSelected);
      alert(`Failed to rename folder: ${e}`);
    }
  }

  // Drag-and-drop was removed — WKWebView's HTML5 DnD silently swallowed
  // dragenter/dragover events, so the right-click "Move to" menu handles
  // folder reorganization instead. Same backend (`move_folder`); just no
  // gesture wiring. Validity check kept here for the Move-to submenu filter.
  function isValidDropTarget(src: string, target: string): boolean {
    if (target === src) return false;
    if (target.startsWith(`${src}/`)) return false; // descendant
    const parent = src.includes('/') ? src.slice(0, src.lastIndexOf('/')) : '';
    if (target === parent) return false; // no-op
    return true;
  }

  // Three-way classification for move-to submenu rendering. Filtering out
  // the parent entirely makes it look like the folder vanished (e.g. when
  // right-clicking play5a, play5 disappears from the move-to list because
  // moving play5a into play5 is a no-op). Render the parent disabled with
  // a "(current)" tag instead so the user can see the structure they're
  // working inside. Self and descendants are still hidden — those targets
  // are truly impossible, not just no-ops.
  function moveTargetState(src: string, target: string): 'valid' | 'parent' | 'hide' {
    if (target === src) return 'hide';
    if (target.startsWith(`${src}/`)) return 'hide';
    const parent = src.includes('/') ? src.slice(0, src.lastIndexOf('/')) : '';
    if (target === parent) return 'parent';
    return 'valid';
  }

  // Re-home a folder under a new parent within one account. Cross-account
  // moves aren't supported (each Gmail account has its own label namespace).
  // Optimistic update on the local store; rollback on backend failure.
  async function moveFolderTo(accountId: string, src: string, newParent: string) {
    closeFolderMenu();
    if (!isValidDropTarget(src, newParent)) return;

    const leaf = src.split('/').pop()!;
    const predicted = `${newParent}/${leaf}`;

    const prevByAccount = foldersByAccount;
    const prevNotes = get(notes);
    const prevIndex = get(noteIndex);
    const prevSelected = get(selectedFolder);

    const rewriteLabel = (label: string): string => {
      if (label === src) return predicted;
      if (label.startsWith(`${src}/`)) return predicted + label.slice(src.length);
      return label;
    };
    foldersByAccount = {
      ...foldersByAccount,
      [accountId]: (foldersByAccount[accountId] ?? []).map(rewriteLabel),
    };
    notes.update((ns) =>
      ns.map((n) =>
        n.account_id === accountId && (n.label === src || n.label.startsWith(`${src}/`))
          ? { ...n, label: rewriteLabel(n.label) }
          : n,
      ),
    );
    indexRewriteOnFolderRename(accountId, src, predicted);
    if (
      $currentAccount === accountId &&
      ($selectedFolder === src || $selectedFolder.startsWith(`${src}/`))
    ) {
      selectedFolder.set(rewriteLabel($selectedFolder));
    }

    try {
      const actualNewPath = await invoke<string>('move_folder', {
        accountId,
        fromPath: src,
        toParentPath: newParent,
      });
      if (actualNewPath !== predicted) {
        console.warn('[jodd] move_folder: predicted', predicted, 'returned', actualNewPath);
      }
      refreshFolders();
    } catch (err) {
      foldersByAccount = prevByAccount;
      notes.set(prevNotes);
      noteIndex.set(prevIndex);
      selectedFolder.set(prevSelected);
      alert(`Failed to move folder: ${err}`);
    }
  }

  // Doctrine-compliant: optimistic removal + selection-bounce BEFORE the
  // await. Backend delete_folder enforces empty-folder + no-children
  // invariants against the cache — if either check fails the catch
  // restores the folder. Worker propagates the trash to Gmail in the
  // background.
  async function deleteFolder(accountId: string, path: string) {
    closeFolderMenu();
    if (path === 'Notes') {
      alert("The root 'Notes' folder can't be deleted.");
      return;
    }
    const ok = await askConfirm(
      'Delete folder?',
      `"${path}" must be empty (no notes, no sub-folders).`
    );
    if (!ok) return;

    const prevByAccount = foldersByAccount;
    const prevSelected = get(selectedFolder);

    foldersByAccount = {
      ...foldersByAccount,
      [accountId]: (foldersByAccount[accountId] ?? []).filter((p) => p !== path),
    };
    if ($selectedFolder === path && $currentAccount === accountId) {
      selectedFolder.set('Notes');
    }

    try {
      await invoke('delete_folder', { accountId, path });
      refreshFolders();
    } catch (e) {
      foldersByAccount = prevByAccount;
      selectedFolder.set(prevSelected);
      alert(`Failed to delete folder: ${e}`);
    }
  }

  // Remove ANY signed-in account (not necessarily the active one). If we
  // removed the last account, the UI naturally drops back to AuthScreen.
  async function removeAccount(accountId: string) {
    const ok = await askConfirm('Remove account?', `Remove ${accountId} from Jodd?`);
    if (!ok) return;
    try {
      await invoke('remove_account', { accountId });
      const remaining = $accounts.filter((a: Account) => a.id !== accountId);
      accounts.set(remaining);
      // Drop its notes from the store so the sidebar reflects the removal.
      notes.update((ns) => ns.filter((n) => n.account_id !== accountId));
      const updated = { ...foldersByAccount };
      delete updated[accountId];
      foldersByAccount = updated;
      if ($currentAccount === accountId) {
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
    } catch (e) {
      console.error('removeAccount failed', e);
    }
    accountPanelOpen = false;
  }

  // Kick off OAuth for a NEW account. Same flow as initial sign-in: backend
  // returns a Google URL, we open it in the system browser, the localhost
  // callback finishes the exchange and emits 'oauth-success'. App.svelte's
  // listener flips isAuthenticated (already true here) — what we need
  // instead is to refresh the accounts list so the new account appears.
  // Track this via the oauth-success event below.
  async function addAccount() {
    accountPanelOpen = false;
    try {
      const url = await invoke<string>('get_auth_url');
      await invoke('open_auth_url', { url });
    } catch (e) {
      alert(`Failed to start sign-in: ${e}`);
    }
  }

  // Listen for oauth-success — fired when a new account finishes the PKCE
  // exchange. Refetch the accounts list, then trigger a full refresh so the
  // new account's notes appear in the sidebar.
  onMount(() => {
    const unlisten = (async () => {
      const { listen } = await import('@tauri-apps/api/event');
      return await listen('oauth-success', async () => {
        try {
          const list = await invoke<Account[]>('list_accounts');
          accounts.set(list);
          // Auto-switch to the new account so the user sees its content.
          const known = new Set($accounts.map((a) => a.id));
          const fresh = list.find((a) => !known.has(a.id));
          if (fresh) currentAccount.set(fresh.id);
          $refreshNotes();
          refreshFolders();
        } catch (e) {
          console.error('post-oauth refresh failed', e);
        }
      });
    })();
    return () => { unlisten.then((fn) => fn()); };
  });

  // Bottom-left account-management panel state. Click the footer chip to
  // open; click outside or pick an action to close.
  let accountPanelOpen = false;
  function toggleAccountPanel() {
    accountPanelOpen = !accountPanelOpen;
  }

  // Settings modal — open per-account. settingsAccountId holds the id
  // currently being edited, or null when closed. Modal closes itself
  // via onClose; we don't need a separate close handler.
  let settingsAccountId: string | null = null;
  let settingsAccountEmail: string = '';
  function openSettings(a: Account) {
    settingsAccountId = a.id;
    settingsAccountEmail = a.email;
    accountPanelOpen = false; // hide the panel under the modal
  }
</script>

<aside class="sidebar" style="width: {width}px; min-width: {width}px;">
  <div class="sidebar-header">
    <span class="app-title">Jodd</span>
    <div class="header-actions">
      <button
        class="new-folder-btn"
        onclick={() => { if ($currentAccount) createFolderUnder($currentAccount, null); }}
        title="New folder in active account"
        aria-label="New folder"
        disabled={!$currentAccount}
      >+</button>
      <button
        class="new-folder-btn"
        onclick={() => dispatch('collapse')}
        title="Collapse sidebar"
        aria-label="Collapse sidebar"
      >‹</button>
    </div>
  </div>

  <nav class="folder-list">
    {#each $accounts as acct (acct.id)}
      <div class="account-section">
        <div class="account-header" title={acct.email}>
          <span class="account-email">{acct.email}</span>
          {#if cleanupResult[acct.id]}
            <span class="dup-pill result" title="Last cleanup result">{cleanupResult[acct.id]}</span>
          {:else if (dupStats[acct.id]?.collapsed ?? 0) > 0}
            <button
              type="button"
              class="dup-pill"
              onclick={(e) => { e.stopPropagation(); openReview(acct.id); }}
              title="{dupStats[acct.id].collapsed} duplicate Gmail message(s) across {dupStats[acct.id].uuids_affected} note(s). Click to review which to trash."
            >{dupStats[acct.id].collapsed} dup</button>
          {/if}
        </div>
        <!-- "All <account>" virtual folder — selectedFolder = "__ALL__"
             signals NoteList to show every note in this account regardless
             of label. Matches Apple Notes' top-of-account aggregate view. -->
        <div
          class="folder-item"
          class:active={$selectedTags.size === 0 && $selectedFolder === '__ALL__' && $currentAccount === acct.id}
          style="padding-left: 16px"
          role="button"
          tabindex="0"
          onclick={() => selectFolder(acct.id, '__ALL__')}
          onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectFolder(acct.id, '__ALL__'); } }}
        >
          <span class="folder-icon">📁</span>
          <span class="folder-name">All {acct.email}</span>
          <span class="folder-count">{$noteIndex.get(acct.id)?.length ?? $notes.filter((n) => n.account_id === acct.id).length}</span>
        </div>
        {#each rowsByAccount[acct.id] ?? [] as row (row.path)}
          {#if isRowVisible(row.path, expandedPaths)}
            {@const hasKids = rowHasChildren(rowsByAccount[acct.id] ?? [], row.path)}
            <div
              class="folder-item"
              class:active={$selectedTags.size === 0 && $selectedFolder === row.path && $currentAccount === acct.id}
              style="padding-left: {8 + row.depth * 14}px"
              role="button"
              tabindex="0"
              onclick={() => selectFolder(acct.id, row.path)}
              onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectFolder(acct.id, row.path); } }}
              oncontextmenu={(e) => openFolderMenu(e, acct.id, row.path)}
            >
              {#if hasKids}
                <span
                  class="expand-toggle"
                  role="button"
                  tabindex="-1"
                  aria-label={expandedPaths[row.path] ? 'Collapse' : 'Expand'}
                  onclick={(e) => toggleExpand(e, row.path)}
                  onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') toggleExpand(e, row.path); }}
                >{expandedPaths[row.path] ? '▾' : '▸'}</span>
              {:else}
                <span class="expand-toggle empty"></span>
              {/if}
              <span class="folder-icon">📁</span>
              <span class="folder-name">{row.name}</span>
              <span class="folder-count">{row.count}</span>
            </div>
          {/if}
        {/each}
        <!-- Tags section: Jodd-local, parallel to folders. Click filters the
             NoteList to notes carrying the tag. Hidden when the account has
             no tags. -->
        {#if ($tagsByAccount.get(acct.id) ?? []).length > 0}
          <div class="tags-header">
            <span>Tags</span>
            {#if $selectedTags.size > 0 && $currentAccount === acct.id}
              <span class="tag-controls">
                {#if $selectedTags.size > 1}
                  <button
                    type="button"
                    class="tag-mode"
                    title="Match notes with ALL (AND) or ANY (OR) of the selected tags"
                    onclick={() => tagMatchMode.update((m) => (m === 'AND' ? 'OR' : 'AND'))}
                  >{$tagMatchMode}</button>
                {/if}
                <button
                  type="button"
                  class="tag-clear"
                  title="Clear tag filter"
                  onclick={() => clearSelectedTags()}
                >clear</button>
              </span>
            {/if}
          </div>
          {#each $tagsByAccount.get(acct.id) ?? [] as t (t.tag)}
            <div
              class="folder-item tag-item"
              class:active={$selectedTags.has(t.tag) && $currentAccount === acct.id}
              style="padding-left: 16px"
              role="button"
              tabindex="0"
              onclick={() => selectTag(acct.id, t.tag)}
              onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectTag(acct.id, t.tag); } }}
              oncontextmenu={(e) => openTagMenu(e, acct.id, t.tag)}
            >
              <span class="folder-icon">🏷️</span>
              <span class="folder-name">{t.tag}</span>
              <span class="folder-count">{t.count}</span>
            </div>
          {/each}
        {/if}
      </div>
    {/each}
  </nav>

  <div class="sidebar-footer">
    <button
      class="account-chip"
      onclick={toggleAccountPanel}
      title="Manage accounts"
      aria-haspopup="menu"
      aria-expanded={accountPanelOpen}
    >
      <span class="account-chip-dot">●</span>
      <span class="account-chip-email">
        {$currentAccount ?? 'No account'}
      </span>
      <span class="account-chip-caret">▾</span>
    </button>

    {#if accountPanelOpen}
      <div class="account-panel" role="menu">
        <div class="account-panel-header">Accounts</div>
        {#each $accounts as a (a.id)}
          <div class="account-row" class:active={a.id === $currentAccount}>
            <button
              class="account-row-pick"
              onclick={() => { currentAccount.set(a.id); selectedFolder.set('Notes'); accountPanelOpen = false; }}
              title="Switch to {a.email}"
            >
              <span class="account-row-dot">{a.id === $currentAccount ? '●' : '○'}</span>
              <span class="account-row-email">{a.email}</span>
            </button>
            <button
              class="account-row-settings"
              onclick={() => openSettings(a)}
              title="Account settings"
              aria-label="Account settings"
            >⚙</button>
            <button
              class="account-row-remove"
              onclick={() => removeAccount(a.id)}
              title="Remove this account"
              aria-label="Remove account"
            >✕</button>
          </div>
        {/each}
        <div class="sep"></div>
        <button class="account-panel-action" onclick={addAccount}>
          <span class="icon">＋</span>
          <span class="label">Add Gmail account</span>
        </button>
      </div>
    {/if}
  </div>
</aside>

{#if settingsAccountId}
  <AccountSettings
    accountId={settingsAccountId}
    accountEmail={settingsAccountEmail}
    onClose={() => { settingsAccountId = null; }}
  />
{/if}

{#if promptOpen}
  <div
    class="prompt-overlay"
    role="dialog"
    aria-modal="true"
    onclick={(e) => { if (e.target === e.currentTarget) promptCancel(); }}
    onkeydown={onPromptKey}
    tabindex="-1"
  >
    <div class="prompt-dialog">
      <div class="prompt-title">{promptTitle}</div>
      <input
        bind:this={promptInputEl}
        bind:value={promptValue}
        class="prompt-input"
        type="text"
        onkeydown={onPromptKey}
      />
      <div class="prompt-actions">
        <button class="prompt-btn" onclick={promptCancel}>Cancel</button>
        <button class="prompt-btn primary" onclick={promptOk}>OK</button>
      </div>
    </div>
  </div>
{/if}

{#if confirmOpen}
  <div
    class="prompt-overlay"
    role="dialog"
    aria-modal="true"
    onclick={(e) => { if (e.target === e.currentTarget) confirmCancel(); }}
    onkeydown={onConfirmKey}
    tabindex="-1"
  >
    <div class="prompt-dialog">
      <div class="prompt-title">{confirmTitle}</div>
      <div class="confirm-message">{confirmMessage}</div>
      <div class="prompt-actions">
        <button class="prompt-btn" onclick={confirmCancel}>Cancel</button>
        <button class="prompt-btn primary" onclick={confirmOk}>OK</button>
      </div>
    </div>
  </div>
{/if}

{#if reviewingAccount}
  <DupReviewModal
    accountId={reviewingAccount}
    onClose={closeReview}
    onTrashed={(count) => onTrashed(reviewingAccount!, count)}
  />
{/if}

{#if menuPath !== null && menuAccountId !== null}
  <div
    bind:this={menuEl}
    class="folder-menu"
    style="left: {menuAdjustedX}px; top: {menuAdjustedY}px"
    role="menu"
  >
    <button class="item" onclick={() => newNoteInFolder(menuAccountId!, menuPath!)}>
      <span class="icon">📝</span>
      <span class="label">New note here</span>
    </button>
    <div class="sep"></div>
    <button class="item" onclick={() => createFolderUnder(menuAccountId!, menuPath)}>
      <span class="icon">＋</span>
      <span class="label">New sub-folder</span>
    </button>
    {#if menuPath !== 'Notes'}
      <button class="item" onclick={() => renameFolder(menuAccountId!, menuPath!)}>
        <span class="icon">✎</span>
        <span class="label">Rename</span>
      </button>
      <div class="sep"></div>
      <div class="item header" aria-hidden="true">
        <span class="icon">📁</span>
        <span class="label">Move to</span>
      </div>
      {#each (rowsByAccount[menuAccountId!] ?? [])
        .map((r) => ({ ...r, _state: moveTargetState(menuPath!, r.path) }))
        .filter((r) => r._state !== 'hide') as r (r.path)}
        <button
          class="item folder"
          class:current-location={r._state === 'parent'}
          style="padding-left: {28 + r.depth * 14}px"
          onclick={() => r._state === 'valid' && moveFolderTo(menuAccountId!, menuPath!, r.path)}
          disabled={r._state === 'parent'}
          title={r._state === 'parent' ? `${r.path} — folder is already here` : r.path}
        >
          <span class="label">{r.name}</span>
          {#if r._state === 'parent'}
            <span class="here-tag">(current)</span>
          {/if}
        </button>
      {/each}
      <div class="sep"></div>
      <button class="item danger" onclick={() => deleteFolder(menuAccountId!, menuPath!)}>
        <span class="icon">🗑</span>
        <span class="label">Delete</span>
      </button>
    {/if}
  </div>
{/if}

<!-- Tag context menu — Rename / Delete operate across every note in the
     account. Reuses .folder-menu styling. -->
{#if tagMenuName !== null && tagMenuAccountId !== null}
  <div
    bind:this={tagMenuEl}
    class="folder-menu"
    style="left: {tagMenuAdjustedX}px; top: {tagMenuAdjustedY}px;"
    role="menu"
    tabindex="-1"
  >
    <div class="item header">#{tagMenuName}</div>
    <button class="item" onclick={() => renameTag(tagMenuAccountId!, tagMenuName!)}>
      <span class="icon">✏️</span>
      <span class="label">Rename tag</span>
    </button>
    <button class="item danger" onclick={() => deleteTag(tagMenuAccountId!, tagMenuName!)}>
      <span class="icon">🗑</span>
      <span class="label">Delete tag</span>
    </button>
  </div>
{/if}

<style>
  .sidebar {
    /* width is set inline by the parent (App.svelte) so it can be resized */
    background: #f0ebe0;
    display: flex;
    flex-direction: column;
    border-right: 1px solid #ddd8cc;
    height: 100vh;
    overflow: hidden;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .sidebar-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 20px 16px 12px;
    border-bottom: 1px solid #ddd8cc;
  }

  .app-title {
    font-size: 13px;
    font-weight: 700;
    color: #888;
    letter-spacing: 2px;
    text-transform: uppercase;
  }

  .new-folder-btn {
    width: 22px;
    height: 22px;
    border-radius: 4px;
    background: none;
    border: none;
    cursor: pointer;
    color: #888;
    font-size: 16px;
    line-height: 1;
    padding: 0;
  }

  .new-folder-btn:hover {
    background: rgba(0, 0, 0, 0.08);
    color: #333;
  }

  .folder-list {
    flex: 1;
    overflow: auto;
    padding: 4px 0;
  }

  .account-section + .account-section {
    margin-top: 12px;
  }

  .account-header {
    font-size: 12px;
    font-weight: 600;
    color: #555;
    padding: 6px 16px 4px;
    white-space: nowrap;
    display: flex;
    align-items: center;
    gap: 6px;
    /* Allow the row to shrink the email rather than overflowing when a
       pill appears alongside. */
    overflow: hidden;
  }

  .account-email {
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 0 1 auto;
  }

  .dup-pill {
    font-size: 10px;
    font-weight: 500;
    line-height: 1;
    color: #8a6a2a;
    background: rgba(201, 124, 31, 0.10);
    border: 1px solid rgba(201, 124, 31, 0.25);
    border-radius: 10px;
    padding: 2px 7px;
    cursor: pointer;
    font-family: inherit;
    /* Don't grow; sit quietly next to the email. */
    flex: 0 0 auto;
    transition: background 0.15s;
  }

  .dup-pill:hover {
    background: rgba(201, 124, 31, 0.20);
  }

  .dup-pill.result {
    color: #888;
    background: rgba(0, 0, 0, 0.04);
    border-color: rgba(0, 0, 0, 0.10);
    cursor: default;
    font-style: italic;
  }

  .folder-item {
    display: flex;
    align-items: center;
    gap: 6px;
    /* width: max-content + min-width: 100% lets long names extend the row
       past the sidebar width, making the folder-list scroll horizontally
       instead of truncating with an ellipsis. */
    min-width: calc(100% - 8px);
    width: max-content;
    padding: 8px 10px 8px 8px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    border-radius: 6px;
    margin: 0 4px;
    transition: background 0.15s;
  }

  .folder-item:hover {
    background: rgba(0,0,0,0.06);
  }

  .folder-item.active {
    background: rgba(0,0,0,0.1);
  }

  .folder-icon {
    font-size: 14px;
  }

  .folder-name {
    flex: 1;
    font-size: 13px;
    color: #333;
    white-space: nowrap;
  }

  .expand-toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    font-size: 14px;
    line-height: 1;
    color: #666;
    border-radius: 3px;
    flex-shrink: 0;
    user-select: none;
  }

  .expand-toggle:not(.empty):hover {
    background: rgba(0, 0, 0, 0.1);
    color: #333;
  }

  .folder-count {
    font-size: 11px;
    color: #999;
    background: rgba(0,0,0,0.08);
    padding: 1px 6px;
    border-radius: 10px;
  }

  .tags-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin: 8px 0 2px;
    padding: 2px 12px 2px 16px;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: #b0a99a;
  }

  .tag-controls {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .tag-mode {
    border: 1px solid #d8b25e;
    background: #f3e4c0;
    color: #6b5320;
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.04em;
    padding: 0 5px;
    border-radius: 8px;
    cursor: pointer;
    line-height: 1.5;
  }

  .tag-mode:hover {
    background: #ead6a4;
  }

  .tag-clear {
    border: none;
    background: transparent;
    color: #b0a99a;
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    cursor: pointer;
    padding: 0 2px;
  }

  .tag-clear:hover {
    color: #c0392b;
  }

  .sidebar-footer {
    position: relative;
    padding: 8px 10px 12px;
    border-top: 1px solid #ddd8cc;
  }

  .account-chip {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 6px 8px;
    background: rgba(0, 0, 0, 0.03);
    border: 1px solid rgba(0, 0, 0, 0.06);
    border-radius: 6px;
    cursor: pointer;
    font-family: inherit;
    color: #333;
    text-align: left;
  }

  .account-chip:hover {
    background: rgba(0, 0, 0, 0.06);
  }

  .account-chip-dot {
    color: #46a35e;
    font-size: 10px;
    flex-shrink: 0;
  }

  .account-chip-email {
    flex: 1;
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .account-chip-caret {
    font-size: 10px;
    color: #888;
    flex-shrink: 0;
  }

  .account-panel {
    position: absolute;
    bottom: calc(100% + 4px);
    left: 8px;
    right: 8px;
    background: white;
    border: 1px solid rgba(0, 0, 0, 0.12);
    border-radius: 8px;
    box-shadow: 0 -6px 24px rgba(0, 0, 0, 0.16);
    padding: 4px;
    z-index: 100;
    font-size: 13px;
  }

  .account-panel-header {
    font-size: 10px;
    font-weight: 700;
    color: #888;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    padding: 6px 10px 4px;
  }

  .account-row {
    display: flex;
    align-items: stretch;
  }

  .account-row.active {
    background: rgba(74, 144, 226, 0.08);
    border-radius: 4px;
  }

  .account-row-pick {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    color: #222;
    border-radius: 4px;
    font-family: inherit;
    min-width: 0;
  }

  .account-row-pick:hover {
    background: rgba(0, 0, 0, 0.04);
  }

  .account-row-dot {
    font-size: 9px;
    color: #4a90e2;
    flex-shrink: 0;
  }

  .account-row-email {
    flex: 1;
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .account-row-remove,
  .account-row-settings {
    width: 24px;
    background: none;
    border: none;
    font-size: 13px;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.15s;
    border-radius: 4px;
  }
  .account-row-remove { color: #c0392b; font-size: 12px; }
  .account-row-settings { color: #888; }

  .account-row:hover .account-row-remove,
  .account-row:hover .account-row-settings {
    opacity: 1;
  }

  .account-row-remove:hover {
    background: rgba(192, 57, 43, 0.08);
  }
  .account-row-settings:hover {
    background: rgba(0, 0, 0, 0.06);
    color: #333;
  }

  .account-panel-action {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 10px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    color: #222;
    border-radius: 4px;
    font-family: inherit;
    font-size: 13px;
  }

  .account-panel-action:hover {
    background: rgba(0, 0, 0, 0.06);
  }

  .account-panel .icon {
    width: 16px;
    text-align: center;
    color: #4a90e2;
    flex-shrink: 0;
  }

  .account-panel .sep {
    height: 1px;
    background: rgba(0, 0, 0, 0.08);
    margin: 4px 8px;
  }

  .folder-menu {
    position: fixed;
    min-width: 180px;
    background: white;
    border: 1px solid rgba(0, 0, 0, 0.12);
    border-radius: 8px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.16);
    padding: 4px;
    z-index: 1000;
    font-size: 13px;
    /* D9 fix: hard cap on menu height so an inline move-to folder list (one
       row per Notes label) can't push Delete past the viewport bottom.
       Pairs with viewport-fit positioning (menuAdjustedX/Y) — that snaps
       a tall menu to the top edge; this guarantees it scrolls instead of
       overflowing if it's still too tall to fit even there. */
    max-height: calc(100vh - 16px);
    overflow-y: auto;
  }

  .folder-menu .item {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 7px 12px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    color: #222;
    border-radius: 4px;
  }

  .folder-menu .item:hover {
    background: rgba(0, 0, 0, 0.06);
  }

  .folder-menu .item.danger {
    color: #c0392b;
  }

  .folder-menu .item.header {
    color: #888;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    cursor: default;
    padding-top: 6px;
    padding-bottom: 4px;
  }

  .folder-menu .item.header:hover {
    background: none;
  }

  .folder-menu .item.folder {
    padding-top: 5px;
    padding-bottom: 5px;
  }

  /* D10: parent row appears in the move-to list but disabled, with a
     "(current)" tag, so the user sees that the parent exists rather than
     thinking the folder vanished. Pointer cursor neutralized; hover
     background suppressed so it reads as inert. */
  .folder-menu .item.folder.current-location {
    color: #888;
    cursor: default;
  }
  .folder-menu .item.folder.current-location:hover {
    background: none;
  }
  .folder-menu .here-tag {
    margin-left: 6px;
    font-size: 10px;
    color: #aaa;
    font-style: italic;
  }

  .folder-menu .icon {
    width: 16px;
    text-align: center;
    flex-shrink: 0;
  }

  .folder-menu .label {
    flex: 1;
    white-space: nowrap;
  }

  .folder-menu .sep {
    height: 1px;
    background: rgba(0, 0, 0, 0.08);
    margin: 4px 8px;
  }

  .prompt-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.32);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 2000;
  }

  .prompt-dialog {
    background: white;
    min-width: 320px;
    max-width: 80vw;
    padding: 18px 20px 14px;
    border-radius: 10px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.24);
  }

  .prompt-title {
    font-size: 13px;
    font-weight: 600;
    color: #222;
    margin-bottom: 10px;
  }

  .prompt-input {
    width: 100%;
    padding: 7px 9px;
    font-size: 13px;
    border: 1px solid #ccc;
    border-radius: 5px;
    outline: none;
    color: #222;
    font-family: inherit;
  }

  .prompt-input:focus {
    border-color: #4a90e2;
    box-shadow: 0 0 0 2px rgba(74, 144, 226, 0.2);
  }

  .confirm-message {
    font-size: 13px;
    color: #555;
    line-height: 1.4;
  }

  .prompt-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 12px;
  }

  .prompt-btn {
    padding: 5px 14px;
    font-size: 12px;
    border-radius: 5px;
    border: 1px solid #ccc;
    background: white;
    color: #333;
    cursor: pointer;
  }

  .prompt-btn:hover {
    background: #f5f5f5;
  }

  .prompt-btn.primary {
    background: #4a90e2;
    color: white;
    border-color: #4a90e2;
  }

  .prompt-btn.primary:hover {
    background: #3a7fcf;
  }
</style>
