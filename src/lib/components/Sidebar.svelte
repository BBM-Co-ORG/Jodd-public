<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { open as openDialog } from '@tauri-apps/plugin-dialog';
  import { notes, selectedFolder, selectedNote, isAuthenticated, accounts, activeAccounts, currentAccount, refreshNotes, noteIndex, folderRefreshSignal, hydratedFolders, indexRewriteOnFolderRename, selectedTags, tagMatchMode, toggleSelectedTag, clearSelectedTags, tagsByAccount, renameTagInStore, deleteTagFromStore, setAccountNoteTags, searchQuery, accountDisplay, selectedSmartFolder, capabilitiesByAccount, setAccountCapabilities, shouldShowTrash, canWrite, type Writes } from '../stores/notes';
  import { partitionAccounts } from '../stores/accountStatus';
  import { isAndroid } from '../stores/platform';
  import { navigateToPane } from '../stores/phoneNav';
  import { longpress } from '../longpress';
  import type { Note, Account, DedupSummary, MessageIndex } from '../types';
  import DupReviewModal from './DupReviewModal.svelte';
  import AccountSettings from './AccountSettings.svelte';
  import { extractModalOpen, aboutModalOpen, appSettingsOpen, askModalOpen } from '../stores/ui';
  import { onMount, onDestroy, createEventDispatcher } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { get } from 'svelte/store';
  import Icon from './Icon.svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';

  // Shape returned by the `count_pending_pushes` command. Declared once —
  // both the Inactive-group poll and the remove-account confirmation read it,
  // and both must sum the SAME four fields (a field missed in one place would
  // read as "nothing queued" exactly where that is most dangerous).
  type PendingPushes = { notes: number; deletes: number; pins: number; folders: number };
  const totalPending = (c: PendingPushes): number =>
    c.notes + c.deletes + c.pins + c.folders;

  // Width is owned by App.svelte so the user can drag a resizer between panes.
  // Default keeps the original 200px look if the parent doesn't pass one.
  export let width: number = 200;
  const dispatch = createEventDispatcher<{ collapse: void }>();

  // Per-path expand state for the folder tree. Defaults to expanded for the
  // implicit "Notes" root of every account so a fresh session still shows
  // top-level folders without the user having to twist them open. Other
  // sub-folders start collapsed (file-explorer convention).
  let expandedPaths: Record<string, boolean> = {};

  // Guards the one-time Notes-root auto-expand below. A plain boolean, not
  // `!expandedPaths['Notes']` — that check couldn't tell "never touched" apart
  // from "user just collapsed it" (both falsy), so the effect kept re-forcing
  // Notes back open on every $activeAccounts change, including the one caused
  // by the collapse click itself re-running this same reactive block.
  let hasAutoExpandedNotesRoot = false;

  // Last $selectedFolder the ancestor-auto-expand block below has already
  // processed — see that block's comment for why this gate is needed.
  let lastAutoExpandedSelection: string | null = null;

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

  // Per-account path → kind map ('user' | 'system_workflow' | 'smart_query').
  // Populated alongside foldersByAccount via list_folder_kinds (Task 16).
  // Paths missing from the map default to 'user' at the render site — most
  // notably the implicit "Notes" root, which has no cache row until first sync.
  let folderKindsByAccount: Record<string, Record<string, string>> = {};

  // Latest-call-wins sequence number. Multiple refreshFolders can be in
  // flight at once — the reactive `$: if ($activeAccounts...)` fires whenever
  // accounts change, and the oauth-success handler kicks off its own call.
  // Without this counter, an in-flight call started with the OLD account
  // list could finish AFTER one that saw the NEW list and silently
  // overwrite the new account's folders with undefined.
  let refreshFoldersSeq = 0;
  async function refreshFolders() {
    const seq = ++refreshFoldersSeq;
    // Active only — matching the `$: if ($activeAccounts.length > 0)` trigger
    // below AND the `{#each $activeAccounts}` that renders the tree. Reading
    // the full list here meant a dismissed account still cost two invokes per
    // refresh for folders nothing renders.
    const list = get(activeAccounts);
    if (list.length === 0) {
      foldersByAccount = {};
      return;
    }
    try {
      const results = await Promise.allSettled(
        list.map((a) =>
          Promise.all([
            invoke<string[]>('list_folders', { accountId: a.id }),
            invoke<[string, string][]>('list_folder_kinds', { accountId: a.id })
              .catch(() => [] as [string, string][]),
          ]).then(([folders, kinds]) => [a.id, folders, kinds] as const),
        ),
      );
      // If a newer refreshFolders started while we were awaiting, drop our
      // result and let the newer one own foldersByAccount.
      if (seq !== refreshFoldersSeq) {
        console.log(`[jodd] refreshFolders: dropping stale seq=${seq}`);
        return;
      }
      const next: Record<string, string[]> = {};
      const nextKinds: Record<string, Record<string, string>> = {};
      results.forEach((r, i) => {
        if (r.status === 'fulfilled') {
          const [id, folders, kinds] = r.value;
          next[id] = folders;
          const kmap: Record<string, string> = {};
          for (const [p, k] of kinds) kmap[p] = k;
          nextKinds[id] = kmap;
          console.log(`[jodd] refreshFolders: ${id} → ${folders.length} folders`);
        } else {
          // Surface rejections — a silent failure here was leaving the new
          // account's folders blank in the sidebar.
          console.error(`[jodd] refreshFolders REJECTED for ${list[i].id}:`, r.reason);
        }
      });
      foldersByAccount = next;
      folderKindsByAccount = nextKinds;
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
  //
  // $folderRefreshSignal is the fourth trigger, and the only one that covers a
  // change touching ONLY the `folders` table — a folder created by jodd-mcp
  // writing straight to SQLite, or by the sync worker draining a dirty_new
  // row. $noteIndex cannot see those: it moves on cold start and on this
  // instance's own note saves, so a folder-only change stayed invisible until
  // restart. App.svelte fires the signal from scheduleRefresh(), the one
  // chokepoint every refresh path funnels through — poll, focus settle,
  // folder settle and the manual button alike.
  $: if ($activeAccounts.length > 0) {
    void $noteIndex;
    void $folderRefreshSignal;
    refreshFolders();
  }

  // Auto-expand the implicit "Notes" root once we know the account list, so a
  // fresh app launch shows the top-level tree without the user clicking ▸.
  // Fires exactly once — after that, user toggles are left alone.
  $: if ($activeAccounts.length > 0 && !hasAutoExpandedNotesRoot) {
    hasAutoExpandedNotesRoot = true;
    expandedPaths = { ...expandedPaths, Notes: true };
  }

  // Whenever selectedFolder points at a sub-path, auto-expand every ancestor
  // along the way. Without this, navigating to a deep folder (via folder
  // create, programmatic set, restoration, etc.) leaves the row rendered
  // but hidden under collapsed parents — looks like a deletion bug.
  //
  // Gated on an actual CHANGE of $selectedFolder (`lastAutoExpandedSelection`),
  // not merely on this block re-running — the block also reads `expandedPaths`,
  // so without the gate a manual collapse of an ancestor (itself an
  // `expandedPaths` write) would re-trigger this same block and immediately
  // force that ancestor back open, exactly like the Notes-root bug above.
  $: if ($selectedFolder !== lastAutoExpandedSelection) {
    const path = $selectedFolder;
    lastAutoExpandedSelection = path;
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
    for (const a of $activeAccounts) {
      out[a.id] = buildRows(
        folderCountsByAccount[a.id] ?? {},
        foldersByAccount[a.id] ?? [],
      );
    }
    return out;
  })();

  // Split rows into user-kind and system_workflow-kind for the Sidebar
  // Folders/Workflows group split (Task 16). A row belongs to the workflow
  // group iff itself or any ancestor is marked kind='system_workflow' — so
  // descendants of e.g. `Notes/Extracts` render under the parent's group
  // rather than getting orphaned back under "Folders". Paths absent from
  // the kinds map default to 'user' (notably the implicit "Notes" root).
  // Strip Jodd's reserved `__name__` markers from a system-folder segment so
  // the user sees a clean display name (e.g. `__Extracts__` → `Extracts`) in
  // the sidebar. Mirror of strip_workflow_markers in src-tauri/src/lib.rs —
  // any change to the marker pattern needs to land in both places.
  function stripWorkflowMarkers(name: string): string {
    return name.startsWith('__') && name.endsWith('__') && name.length > 4
      ? name.slice(2, -2)
      : name;
  }

  // Folder is protected from rename/move/delete when it's a CURRENTLY-canonical
  // workflow folder: kind='system_workflow' AND the leaf segment matches the
  // reserved `__name__` syntax. Folders with kind='system_workflow' but a
  // plain name (e.g. legacy `Notes/Lessons` or `Notes/Extracts` from before
  // we standardized on the underscore convention) are NOT protected — they're
  // historical artifacts the user can manage normally.
  //
  // Distinct from isWorkflowPath, which controls sidebar GROUPING and uses an
  // ancestor check (descendants of a workflow folder still render under the
  // Workflows group). Protection is about a specific folder's lifecycle, not
  // its visual placement.
  function isProtectedWorkflowFolder(
    kinds: Record<string, string>,
    path: string,
  ): boolean {
    if (kinds[path] !== 'system_workflow') return false;
    const leaf = path.split('/').pop() ?? '';
    return leaf.startsWith('__') && leaf.endsWith('__') && leaf.length > 4;
  }

  function isWorkflowPath(
    kinds: Record<string, string>,
    path: string,
  ): boolean {
    const segs = path.split('/');
    for (let i = 1; i <= segs.length; i++) {
      const ancestor = segs.slice(0, i).join('/');
      if (kinds[ancestor] === 'system_workflow') return true;
    }
    return false;
  }
  $: splitRowsByAccount = (() => {
    const out: Record<string, { user: FolderRow[]; workflow: FolderRow[] }> = {};
    for (const a of $activeAccounts) {
      const rows = rowsByAccount[a.id] ?? [];
      const kinds = folderKindsByAccount[a.id] ?? {};
      const user: FolderRow[] = [];
      const workflow: FolderRow[] = [];
      for (const r of rows) {
        if (isWorkflowPath(kinds, r.path)) workflow.push(r);
        else user.push(r);
      }
      out[a.id] = { user, workflow };
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
  $: if ($notes && $activeAccounts.length > 0) {
    for (const a of $activeAccounts) refreshDupStats(a.id);
  }

  // backend_capabilities — whether THIS account has a Trash concept at all
  // (Microsoft does not: a Graph DELETE leaves no restore path). Reads the
  // account's stored backend_kind, no network call, so it's safe to call on
  // every account-list change the same way refreshFolders/refreshDupStats
  // already are. Fetched per account (not just $currentAccount) because the
  // Recently Deleted row below is rendered once per account section in the
  // {#each $activeAccounts} loop.
  async function refreshCapabilities(accountId: string) {
    try {
      const caps = await invoke<{ has_trash: boolean; writes: Writes }>('backend_capabilities', { accountId });
      setAccountCapabilities(accountId, caps);
    } catch {
      // Leave whatever's cached (or nothing — shouldShowTrash defaults
      // undefined to true) rather than hide Trash on a transient error.
    }
  }

  $: if ($activeAccounts.length > 0) {
    for (const a of $activeAccounts) refreshCapabilities(a.id);
  }

  // Write affordances are hidden per-area, matching what each button/menu
  // item actually invokes — `create_folder`/`rename_folder`/`delete_folder`
  // are `Write::Folders` server-side, `rename_tag`/`delete_tag` are
  // `Write::Sidecars` (lib.rs). Using the coarse `canWriteAccount` here would
  // read as writable on Microsoft — `canWriteAccount` only checks `notes`,
  // which IS true there — even though folders never reach Apple Notes (a
  // measured negative, 2026-08-15) and sidecars stay refused until M4:
  // exactly the frontend/backend divergence this split exists to describe.
  // The refusal itself lives in `refuse_write` (lib.rs); this only keeps the
  // user from reaching for a button that would answer with an error. See
  // `canWrite` for the optimistic default.
  $: currentCanWriteFolders = canWrite(
    $currentAccount ? $capabilitiesByAccount[$currentAccount] : undefined,
    'folders',
  );
  $: menuCanWriteNotes = canWrite(
    menuAccountId ? $capabilitiesByAccount[menuAccountId] : undefined,
    'notes',
  );
  $: menuCanWriteFolders = canWrite(
    menuAccountId ? $capabilitiesByAccount[menuAccountId] : undefined,
    'folders',
  );
  $: tagMenuCanWriteSidecars = canWrite(
    tagMenuAccountId ? $capabilitiesByAccount[tagMenuAccountId] : undefined,
    'sidecars',
  );

  // If the account currently being viewed just turned out to have no Trash
  // (capabilities arrived, or the user switched to a no-trash account via a
  // path that doesn't already reassign selectedFolder — e.g. the ingest
  // button below, which changes $currentAccount without touching
  // $selectedFolder), the Recently Deleted row for it just vanished from
  // the list above. Bounce the selection back to the account's default view
  // rather than leaving $selectedFolder pointed at an entry with no visible
  // row — mirrors the reset the account-switcher panel already does at pick
  // time (see the footer "account-row-pick" onclick below).
  $: if (
    $selectedFolder === '__TRASH__' &&
    $currentAccount &&
    $capabilitiesByAccount[$currentAccount] &&
    !shouldShowTrash($capabilitiesByAccount[$currentAccount])
  ) {
    selectedFolder.set('Notes');
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
    // Trigger a fresh list_notes so dup_stats recomputes from authoritative
    // Gmail state. Previously the backend decremented dup_stats optimistically
    // here so the pill responded instantly, but that caused flicker against
    // Gmail's eventual consistency (the next poll often still saw the
    // just-trashed messages). Now the backend doesn't touch the count and we
    // fire a real refresh — slower but truthful. refreshDupStats runs
    // automatically when $notes updates via the reactive block below.
    void Promise.resolve(get(refreshNotes)());
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
    // Smart Folder, folder, and tag views are mutually exclusive — entering
    // a real folder leaves any active Smart Folder or tag filter.
    selectedSmartFolder.set(null);
    clearSelectedTags();
    // Folder navigation also exits search mode — a "results across folders"
    // view is incoherent with "show me this folder", and leaving search
    // active made the click feel like it did nothing.
    if ($searchQuery) searchQuery.set('');
    selectedFolder.set(path);
    selectedNote.set(null);
    // Phone stack: App.svelte's $selectedFolder watcher only calls
    // navigateToPane('list') when the VALUE changes, so re-tapping the
    // folder you're already in (a real Sidebar action, common from the
    // phone Folders pane) is invisible to that diff-based watcher. Call it
    // directly here — navigateToPane is a no-op if the list pane is already
    // active, and inert on tablet/desktop (isPhoneNavActive() gates it), so
    // this is safe to fire unconditionally on every folder tap.
    navigateToPane('list');
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
    selectedSmartFolder.set(null);
    toggleSelectedTag(tag);
    selectedNote.set(null);
  }

  // Smart Folders ("Orphaned"/"Stale") are fully virtual — no folders-table
  // row, no rename/move/delete. Selecting one leaves selectedFolder's VALUE
  // untouched (App.svelte's folder-reactive watcher is keyed off changes to
  // that store and must not re-fire); NoteList checks selectedSmartFolder
  // FIRST, before falling through to the folder/tag/search logic, so the
  // stale selectedFolder value is simply never consulted while a Smart
  // Folder is active.
  function selectSmartFolder(accountId: string, kind: 'orphaned' | 'stale') {
    if ($currentAccount !== accountId) {
      currentAccount.set(accountId);
    }
    clearSelectedTags();
    if ($searchQuery) searchQuery.set('');
    selectedNote.set(null);
    selectedSmartFolder.set({ account: accountId, kind });
  }

  // Per-account collapse of the Tags section.
  //
  // Tracks EXPANDED account ids, not collapsed ones, because the default is
  // now collapsed: an account nobody has touched must start closed, and a set
  // of "collapsed" ids cannot express that without first enumerating every
  // account. Inverting the set makes "unknown account" mean "collapsed" for
  // free.
  //
  // Why collapsed by default: an account with a few dozen tags (easy to reach
  // — Extract mints new ones per note, see roadmap item #0) pushed the real
  // folder tree into a cramped strip at the top of the pane.
  //
  // Persisted so the choice survives a relaunch. Same localStorage shape as
  // src/lib/whatsNew.ts rather than a new persistence layer.
  const TAGS_EXPANDED_KEY = 'jodd.sidebar.tagsExpanded';

  function loadTagsExpanded(): Set<string> {
    try {
      const raw = localStorage.getItem(TAGS_EXPANDED_KEY);
      if (!raw) return new Set();
      const parsed = JSON.parse(raw);
      return Array.isArray(parsed) ? new Set(parsed.filter((v) => typeof v === 'string')) : new Set();
    } catch {
      // Corrupt or unavailable storage must not take the sidebar down with it.
      return new Set();
    }
  }

  let expandedTags = loadTagsExpanded();

  function toggleTagsCollapsed(accountId: string) {
    const next = new Set(expandedTags);
    if (next.has(accountId)) next.delete(accountId);
    else next.add(accountId);
    expandedTags = next;
    try {
      localStorage.setItem(TAGS_EXPANDED_KEY, JSON.stringify([...next]));
    } catch {
      // Non-fatal: the toggle still works for this session.
    }
  }

  // Per-account collapse of the ENTIRE tree (folders + Views + Tags), not
  // just Tags. Tracks COLLAPSED account ids, not expanded ones, because the
  // default here is expanded — an account nobody has touched must start
  // open. Same localStorage shape as TAGS_EXPANDED_KEY above.
  const ACCOUNTS_COLLAPSED_KEY = 'jodd.sidebar.accountsCollapsed';

  function loadCollapsedAccounts(): Set<string> {
    try {
      const raw = localStorage.getItem(ACCOUNTS_COLLAPSED_KEY);
      if (!raw) return new Set();
      const parsed = JSON.parse(raw);
      return Array.isArray(parsed) ? new Set(parsed.filter((v) => typeof v === 'string')) : new Set();
    } catch {
      return new Set();
    }
  }

  let collapsedAccounts = loadCollapsedAccounts();

  function toggleAccountCollapsed(accountId: string) {
    const next = new Set(collapsedAccounts);
    if (next.has(accountId)) next.delete(accountId);
    else next.add(accountId);
    collapsedAccounts = next;
    try {
      localStorage.setItem(ACCOUNTS_COLLAPSED_KEY, JSON.stringify([...next]));
    } catch {
      // Non-fatal: the toggle still works for this session.
    }
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
  // near the bottom of a tall sidebar pushes Delete off the screen, where
  // it can't be clicked. "Move to" no longer inlines its folder list here —
  // it's a hover flyout (see fitSubmenu below), same as NoteContextMenu —
  // but this menu can still be a few items tall, so the fit stays.
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

  // Submenu viewport-fit for the "Move to" hover flyout — identical to
  // NoteContextMenu's fitSubmenu, kept in lockstep so the two Move-to
  // affordances behave the same, not just look the same.
  function fitSubmenu(e: MouseEvent) {
    const anchor = e.currentTarget as HTMLElement;
    const sub = anchor.querySelector(':scope > .submenu') as HTMLElement | null;
    if (!sub) return;
    const prevDisplay = sub.style.display;
    sub.style.display = 'block';
    sub.style.visibility = 'hidden';
    const rect = sub.getBoundingClientRect();
    sub.style.display = prevDisplay;
    sub.style.visibility = '';
    if (rect.right > window.innerWidth - 8) {
      sub.classList.add('flip-left');
    } else {
      sub.classList.remove('flip-left');
    }
    const overflow = rect.bottom - (window.innerHeight - 8);
    if (overflow > 0) {
      const newTop = Math.max(-(rect.height - 40), -overflow);
      sub.style.top = `${newTop}px`;
    } else {
      sub.style.top = '';
    }
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
    // Sub-folders are a structural fact the sidebar already renders from
    // this same foldersByAccount array — no staleness risk the way note
    // counts have (those are synthesized from $notes/$noteIndex, which can
    // drift — see the ghost-folder bug). If we already know for certain
    // this will fail, say so immediately instead of asking "are you sure?"
    // and then failing right after — that trains users to distrust the
    // confirm dialog. Note-count emptiness stays deferred to the backend
    // (the optimistic-attempt-then-rollback below), since that signal
    // genuinely can be stale.
    const hasSubfolders = (foldersByAccount[accountId] ?? []).some((p) =>
      p.startsWith(`${path}/`)
    );
    if (hasSubfolders) {
      alert(`Folder "${path}" has sub-folders. Delete those first.`);
      return;
    }
    const ok = await askConfirm(
      'Delete folder?',
      `Delete "${path}"? Folders can only be deleted when empty (no notes) — Jodd will cancel automatically if it isn't.`
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
  async function removeAccount(accountId: string) {
    let n: number | undefined;
    try {
      n = totalPending(await invoke<PendingPushes>('count_pending_pushes', { accountId }));
    } catch (e) {
      console.error(`count_pending_pushes failed for ${accountId}:`, e);
    }
    const warning =
      n === undefined
        ? ' This account may still have unsent edits — the pending count could not be confirmed. Anything unsent will be deleted with it.'
        : n > 0
          ? ` ${n} unsent edit(s) have not reached the server yet and will be deleted with it.`
          : '';
    const ok = await askConfirm('Remove account?', `Remove ${accountId} from Jodd?${warning}`);
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
  // returns an auth URL (Google or Microsoft, per `backend`), we open it in
  // the system browser, the localhost callback finishes the exchange and
  // emits 'oauth-success'. App.svelte's listener flips isAuthenticated
  // (already true here) — what we need instead is to refresh the accounts
  // list so the new account appears. Track this via the oauth-success event
  // below. Omitting `backend` still means Gmail — the existing Google call
  // sites keep working untouched.
  async function addAccount(backend?: 'microsoft') {
    accountPanelOpen = false;
    try {
      const url = await invoke<string>('get_auth_url', backend ? { backend } : undefined);
      await invoke('open_auth_url', { url });
    } catch (e) {
      alert(`Failed to start sign-in: ${e}`);
    }
  }

  // Add a local folder (LocalFS backend). Opens a native directory picker,
  // prompts for a vault name (default = folder basename), calls add_local_account,
  // then mirrors the same post-add refresh the OAuth flow uses so the new account's
  // notes appear in the sidebar.
  async function addLocalFolder() {
    accountPanelOpen = false;
    try {
      const dir = await openDialog({ directory: true, multiple: false, title: 'Choose a notes folder' });
      if (typeof dir !== 'string') return; // cancelled

      // Disclosure requirement (docs/superpowers/specs/2026-08-13-at-rest-encryption-design.md,
      // "Scope limitation: LocalFs vaults are NOT covered by this design"):
      // Gmail accounts' local cache is encrypted at rest; Local Folder vaults
      // are not, and this must be stated plainly at the point of choice, not
      // buried in settings or a privacy-policy footnote.
      const acknowledged = await askConfirm(
        'Notes in this folder are not encrypted',
        'Jodd stores notes in a Local Folder vault as plain, unencrypted files ' +
        'in the folder you choose. Anyone with access to this folder on disk ' +
        'can read them directly. Protecting this folder — full-disk ' +
        'encryption, access controls, backup handling — is your ' +
        'responsibility. This is different from Gmail accounts, whose local ' +
        'note cache Jodd encrypts automatically.'
      );
      if (!acknowledged) return;

      // Derive the folder basename as the default vault name.
      const basename = dir.replace(/\\/g, '/').split('/').filter(Boolean).pop() ?? dir;
      // Use the existing inline prompt (window.prompt is unreliable in Tauri WKWebView).
      const vaultName = await askName('Vault name (shown in sidebar):', basename);
      if (vaultName === null) return; // user cancelled the name prompt
      const name = vaultName.trim() || basename;
      const newAccount = await invoke<Account>('add_local_account', { path: dir, name });
      // Mirror the oauth-success post-add flow: update accounts store,
      // auto-switch to the new account, then refresh notes + folders.
      const list = await invoke<Account[]>('list_accounts');
      accounts.set(list);
      currentAccount.set(newAccount.id);
      $refreshNotes();
      refreshFolders();
    } catch (e) {
      console.error('add local folder failed', e);
      alert(`Failed to add local folder: ${e}`);
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

  // App version — fetched once on mount, shown in the footer version label.
  let appVersion = '';
  onMount(async () => {
    try { appVersion = await getVersion(); } catch { appVersion = ''; }
  });

  // Bottom-left account-management panel state. Click the footer chip to
  // open; click outside or pick an action to close.
  let accountPanelOpen = false;
  function toggleAccountPanel() {
    accountPanelOpen = !accountPanelOpen;
  }

  // Inactive group — draining/inactive accounts, tucked under a collapsible
  // header below the active list. `accounts` deliberately keeps everything
  // (see notes.ts activeAccounts doc comment); this is the one place in the
  // sidebar that reads the dismissed half of it.
  let inactiveOpen = false;
  let pending: Record<string, number> = {};
  // True for an account whose last count_pending_pushes call failed. Kept
  // separate from `pending` (rather than deleting the stale entry) so the
  // row can tell "never fetched yet" (shows "…") apart from "fetch keeps
  // failing" (shows an explicit warning) instead of both looking identical.
  let pendingError: Record<string, boolean> = {};

  $: dismissedAccounts = partitionAccounts($accounts).dismissed;

  // Polled while the group is open so "N left" counts DOWN. The worker
  // drains its queues on its own 5s tick and nothing else in this component
  // changes while that happens, so a one-shot fetch on open would freeze the
  // number at whatever it was the moment the panel opened. This file predates
  // runes (see the `$:` reactive statements throughout), so the poll is
  // started/stopped by a reactive block instead of an `$effect` teardown —
  // same lifecycle, plain-Svelte-4-compatible shape. `onDestroy` below is the
  // backstop in case the component unmounts while the group is still open.
  // Gated on accountPanelOpen too: the group's DOM only exists while the
  // panel is open, and inactiveOpen (a separate toggle) survives the panel
  // closing, so without this the poll would keep firing every 3s against
  // nothing visible for the rest of the session.
  let pendingPollHandle: ReturnType<typeof setInterval> | null = null;
  const PENDING_POLL_MS = 3_000;

  function refreshPending() {
    for (const a of dismissedAccounts.filter((x) => x.status === 'draining')) {
      invoke<PendingPushes>('count_pending_pushes', { accountId: a.id })
        .then((c) => {
          pending = { ...pending, [a.id]: totalPending(c) };
          if (pendingError[a.id]) pendingError = { ...pendingError, [a.id]: false };
        })
        .catch((e) => {
          // Logged, not swallowed — a command that keeps failing must be
          // visible (see pendingError below), not indistinguishable from a
          // slow load.
          console.error(`count_pending_pushes failed for ${a.id}:`, e);
          pendingError = { ...pendingError, [a.id]: true };
        });
    }
    // The worker flips draining -> inactive on its own 5s tick, entirely in
    // Rust, and emits no event — so the frontend `accounts` store (last
    // written by this component's own optimistic `setStatus`) otherwise never
    // learns the drain finished. Piggyback that pickup on this same poll
    // (already scoped to exactly when the Inactive group is open) instead of
    // adding a second timer. The backend is authoritative here, so replace
    // the whole list rather than patch individual fields.
    //
    // Same latest-call-wins guard as refreshFoldersSeq above: this fetch can
    // straddle a user click on "Stop waiting" / "Reactivate", whose
    // setStatus does its own optimistic snapshot/mutate/invoke/rollback. If
    // that happens, this response is a stale snapshot from before (or during)
    // that optimistic write and must not clobber it — drop it and let the
    // NEXT poll tick (3s later) pick up the settled state instead.
    const seq = statusChangeSeq;
    invoke<Account[]>('list_accounts')
      .then((list) => {
        if (statusChangeSeq !== seq) return;
        accounts.set(list);
      })
      .catch((e) => console.error('list_accounts (inactive-group poll) failed', e));
  }

  $: if (accountPanelOpen && inactiveOpen) {
    if (pendingPollHandle === null) {
      refreshPending();
      pendingPollHandle = setInterval(refreshPending, PENDING_POLL_MS);
    }
  } else if (pendingPollHandle !== null) {
    clearInterval(pendingPollHandle);
    pendingPollHandle = null;
  }

  onDestroy(() => {
    if (pendingPollHandle !== null) clearInterval(pendingPollHandle);
  });

  // Bumped at the start of every setStatus call. Lets refreshPending's
  // list_accounts poll (above) detect "a status change started or finished
  // while my fetch was in flight" and discard its own stale result instead
  // of overwriting a fresher optimistic write.
  let statusChangeSeq = 0;

  // Returns whether the flip stuck, so callers can chain follow-up work
  // (see reactivate) without repeating it after a rollback.
  async function setStatus(id: string, status: string): Promise<boolean> {
    statusChangeSeq++;
    const snapshot = get(accounts);
    accounts.update((list) => list.map((a) => (a.id === id ? { ...a, status } : a)));
    try {
      await invoke('set_account_status', { accountId: id, status });
      return true;
    } catch (e) {
      accounts.set(snapshot);
      console.error('set_account_status failed', e);
      return false;
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
  async function warmUpAccount(id: string) {
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
    refreshFolders();
    $refreshNotes();
  }

  async function reactivate(a: Account) {
    if (await setStatus(a.id, 'active')) void warmUpAccount(a.id);
  }

  // Stop waiting is the one path in this feature that can leave a user's
  // work unsent, so an unknown count must warn too — treating "not fetched
  // yet" or "fetch keeps failing" as 0 would silently skip the one
  // confirmation this design relies on. Only a CONFIRMED 0 skips it.
  async function stopWaiting(a: Account) {
    const n = pending[a.id];
    if (n === undefined || n > 0) {
      const message = n === undefined
        ? 'This account may still have unsent edits — the pending count could not be confirmed. Anything unsent will stay on this device and will be sent if you reactivate this account.'
        : `${n} edit(s) will stay on this device and will be sent if you reactivate this account.`;
      const ok = await askConfirm('Stop waiting?', message);
      if (!ok) return;
    }
    await setStatus(a.id, 'inactive');
  }

  // Settings modal — open per-account. settingsAccountId holds the id
  // currently being edited, or null when closed. Modal closes itself
  // via onClose; we don't need a separate close handler.
  let settingsAccountId: string | null = null;
  let settingsAccountEmail: string = '';
  let settingsBackendKind: string | undefined = undefined;
  function openSettings(a: Account) {
    settingsAccountId = a.id;
    settingsAccountEmail = accountDisplay(a);
    settingsBackendKind = a.backend_kind;
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
        title={currentCanWriteFolders ? 'New folder in active account' : "This account can't accept folder changes in Jodd yet"}
        aria-label="New folder"
        disabled={!$currentAccount || !currentCanWriteFolders}
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
    {#each $activeAccounts as acct (acct.id)}
      <div class="account-section">
        <div class="account-header" title={accountDisplay(acct)}>
          <span
            class="account-collapse-toggle"
            role="button"
            tabindex="0"
            aria-label={collapsedAccounts.has(acct.id) ? 'Expand account' : 'Collapse account'}
            onclick={() => toggleAccountCollapsed(acct.id)}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleAccountCollapsed(acct.id); } }}
          ><Icon name={collapsedAccounts.has(acct.id) ? 'chevron-right' : 'chevron-down'} size={12} /></span>
          {#if acct.backend_kind === 'local_fs'}
            <span class="account-kind-icon" title="Local folder account"><Icon name="folder" size={12} /></span>
          {/if}
          <span class="account-email">{accountDisplay(acct)}</span>
          <button
            type="button"
            class="account-ingest-btn"
            onclick={(e) => {
              e.stopPropagation();
              if ($currentAccount !== acct.id) currentAccount.set(acct.id);
              extractModalOpen.set(true);
            }}
            title="Ingest a source into {accountDisplay(acct)}"
            aria-label="Ingest a source into {accountDisplay(acct)}"
          ><Icon name="bulb" size={14} /></button>
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
        {#if !collapsedAccounts.has(acct.id)}
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
          <span class="folder-icon"><Icon name="folder" /></span>
          <span class="folder-name" title="All {accountDisplay(acct)}">All {accountDisplay(acct)}</span>
          <span class="folder-count">{Object.values(folderCountsByAccount[acct.id] ?? {}).reduce((s, c) => s + c, 0)}</span>
        </div>
        {#snippet folderRow(row: FolderRow, allRows: FolderRow[], isWorkflow: boolean)}
          {#if isRowVisible(row.path, expandedPaths)}
            {@const hasKids = rowHasChildren(allRows, row.path)}
            <div
              class="folder-item"
              class:active={$selectedTags.size === 0 && $selectedFolder === row.path && $currentAccount === acct.id}
              style="padding-left: {8 + row.depth * 14}px"
              role="button"
              tabindex="0"
              onclick={() => selectFolder(acct.id, row.path)}
              onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectFolder(acct.id, row.path); } }}
              oncontextmenu={(e) => openFolderMenu(e, acct.id, row.path)}
              use:longpress
            >
              {#if hasKids}
                <span
                  class="expand-toggle"
                  role="button"
                  tabindex="-1"
                  aria-label={expandedPaths[row.path] ? 'Collapse' : 'Expand'}
                  onclick={(e) => toggleExpand(e, row.path)}
                  onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') toggleExpand(e, row.path); }}
                ><Icon name={expandedPaths[row.path] ? 'chevron-down' : 'chevron-right'} size={12} /></span>
              {:else}
                <span class="expand-toggle empty"></span>
              {/if}
              <span class="folder-icon"><Icon name={isWorkflow ? 'bulb' : 'folder'} /></span>
              <span class="folder-name" title={stripWorkflowMarkers(row.name)}>{stripWorkflowMarkers(row.name)}</span>
              <span class="folder-count">{row.count}</span>
            </div>
          {/if}
        {/snippet}
        {#if splitRowsByAccount[acct.id]}
          {@const split = splitRowsByAccount[acct.id]}
          <div class="folder-group">
            {#each split.user as row (row.path)}
              {@render folderRow(row, split.user, false)}
            {/each}
          </div>
          {#if split.workflow.length > 0}
            <div class="folder-group">
              <h4 class="group-header"><span class="group-icon"><Icon name="bulb" size={13} /></span>Workflows</h4>
              {#each split.workflow as row (row.path)}
                {@render folderRow(row, split.workflow, true)}
              {/each}
            </div>
          {/if}
        {/if}
        <!-- Views — the read-only virtual entries, grouped under one header so
             they stop reading as ordinary Gmail-label folders. Everything here
             is a saved query over the cache: no sync state, no context menu, no
             rename/move/delete. Smart Folders (Orphaned + Stale) are fully
             virtual with no folders-table row (design spec decision 3/4);
             Recently Deleted reads Gmail Trash. They used to be styled exactly
             like real folders, and Recently Deleted sat outside any group
             entirely. -->
        <div class="folder-group">
          <h4 class="group-header"><span class="group-icon"><Icon name="eye" size={13} /></span>Views</h4>
          <div
            class="folder-item"
            class:active={$selectedSmartFolder?.account === acct.id && $selectedSmartFolder?.kind === 'orphaned'}
            style="padding-left: 16px"
            role="button"
            tabindex="0"
            onclick={() => selectSmartFolder(acct.id, 'orphaned')}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectSmartFolder(acct.id, 'orphaned'); } }}
          >
            <span class="folder-icon"><Icon name="search" /></span>
            <span class="folder-name">Orphaned</span>
          </div>
          <div
            class="folder-item"
            class:active={$selectedSmartFolder?.account === acct.id && $selectedSmartFolder?.kind === 'stale'}
            style="padding-left: 16px"
            role="button"
            tabindex="0"
            onclick={() => selectSmartFolder(acct.id, 'stale')}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectSmartFolder(acct.id, 'stale'); } }}
          >
            <span class="folder-icon"><Icon name="clock" /></span>
            <span class="folder-name">Stale</span>
          </div>
          <!-- Recently Deleted — reads from Gmail Trash (Apple's Recently
               Deleted over this backend). selectedFolder = "__TRASH__".
               Gated on backend_capabilities: Microsoft has no Trash at all —
               a Graph DELETE leaves no restore path — so offering this entry
               there would be a dead end rather than an empty list. -->
          {#if shouldShowTrash($capabilitiesByAccount[acct.id])}
            <div
              class="folder-item"
              class:active={$selectedTags.size === 0 && $selectedFolder === '__TRASH__' && $currentAccount === acct.id}
              style="padding-left: 16px"
              role="button"
              tabindex="0"
              onclick={() => selectFolder(acct.id, '__TRASH__')}
              onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectFolder(acct.id, '__TRASH__'); } }}
            >
              <span class="folder-icon"><Icon name="trash" /></span>
              <span class="folder-name">Recently Deleted</span>
            </div>
          {/if}
        </div>
        <!-- Tags section: Jodd-local, parallel to folders. Click filters the
             NoteList to notes carrying the tag. Hidden when the account has
             no tags. -->
        {#if ($tagsByAccount.get(acct.id) ?? []).length > 0}
          <div
            class="tags-header"
            role="button"
            tabindex="0"
            title="Show or hide tags"
            onclick={() => toggleTagsCollapsed(acct.id)}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleTagsCollapsed(acct.id); } }}
          >
            <span class="tags-caret"><Icon name={expandedTags.has(acct.id) ? 'chevron-down' : 'chevron-right'} size={12} /></span>
            <span>Tags</span>
            <span class="tags-num">{($tagsByAccount.get(acct.id) ?? []).length}</span>
            {#if $selectedTags.size > 0 && $currentAccount === acct.id}
              <span class="tag-controls">
                {#if $selectedTags.size > 1}
                  <button
                    type="button"
                    class="tag-mode"
                    title="Match notes with ALL (AND) or ANY (OR) of the selected tags"
                    onclick={(e) => { e.stopPropagation(); tagMatchMode.update((m) => (m === 'AND' ? 'OR' : 'AND')); }}
                  >{$tagMatchMode}</button>
                {/if}
                <button
                  type="button"
                  class="tag-clear"
                  title="Clear tag filter"
                  onclick={(e) => { e.stopPropagation(); clearSelectedTags(); }}
                >clear</button>
              </span>
            {/if}
          </div>
          {#if expandedTags.has(acct.id)}
            <div class="tag-chips">
              {#each $tagsByAccount.get(acct.id) ?? [] as t (t.tag)}
                <button
                  type="button"
                  class="tag-pill"
                  class:active={$selectedTags.has(t.tag) && $currentAccount === acct.id}
                  title="#{t.tag} · {t.count}"
                  onclick={() => selectTag(acct.id, t.tag)}
                  oncontextmenu={(e) => openTagMenu(e, acct.id, t.tag)}
                  use:longpress
                >
                  <span class="pill-name">{t.tag}</span>
                  <span class="pill-count">{t.count}</span>
                </button>
              {/each}
            </div>
          {/if}
        {/if}
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
      <span class="account-chip-email" title={accountDisplay($activeAccounts.find((a) => a.id === $currentAccount), $currentAccount ?? 'No account')}>
        {accountDisplay($activeAccounts.find((a) => a.id === $currentAccount), $currentAccount ?? 'No account')}
      </span>
      <span class="account-chip-caret"><Icon name="chevron-down" size={12} /></span>
    </button>

    {#if accountPanelOpen}
      <div class="account-panel" role="menu">
        <div class="account-panel-header">Accounts</div>
        {#each $activeAccounts as a (a.id)}
          <div class="account-row" class:active={a.id === $currentAccount}>
            <button
              class="account-row-pick"
              onclick={() => { currentAccount.set(a.id); selectedFolder.set('Notes'); accountPanelOpen = false; }}
              title="Switch to {accountDisplay(a)}"
            >
              <span class="account-row-dot">{a.id === $currentAccount ? '●' : '○'}</span>
              {#if a.backend_kind === 'local_fs'}<span class="account-row-kind-icon"><Icon name="folder" size={12} /></span>{/if}
              <span class="account-row-email">{accountDisplay(a)}</span>
            </button>
            <button
              class="account-row-settings"
              onclick={() => openSettings(a)}
              title="Account settings"
              aria-label="Account settings"
            ><Icon name="gear" size={13} /></button>
            <button
              class="account-row-remove"
              onclick={() => removeAccount(a.id)}
              title="Remove this account"
              aria-label="Remove account"
            ><Icon name="close" size={12} /></button>
          </div>
        {/each}
        {#if dismissedAccounts.length > 0}
          <button
            type="button"
            class="account-panel-header inactive-toggle"
            onclick={() => (inactiveOpen = !inactiveOpen)}
          >
            <Icon name={inactiveOpen ? 'chevron-down' : 'chevron-right'} size={12} />
            Inactive ({dismissedAccounts.length})
          </button>
          {#if inactiveOpen}
            {#each dismissedAccounts as a (a.id)}
              <div class="account-row dimmed">
                <span class="account-row-email" title={accountDisplay(a)}>{accountDisplay(a)}</span>
                {#if a.status === 'draining'}
                  <span class="account-row-note" class:warn={pendingError[a.id]}>
                    {#if pendingError[a.id]}
                      finishing sync — count unavailable
                    {:else if pending[a.id] === undefined}
                      finishing sync — … left
                    {:else}
                      finishing sync — {pending[a.id]} left
                    {/if}
                  </span>
                  <button type="button" onclick={() => stopWaiting(a)}>Stop waiting</button>
                {:else}
                  <button type="button" onclick={() => reactivate(a)}>Reactivate</button>
                  <button type="button" class="account-row-remove" onclick={() => removeAccount(a.id)}>Remove</button>
                {/if}
              </div>
            {/each}
          {/if}
        {/if}
        <div class="sep"></div>
        <button class="account-panel-action" onclick={() => addAccount()}>
          <span class="icon">＋</span>
          <span class="label">Add Gmail account</span>
        </button>
        {#if !$isAndroid}
          <!-- Microsoft sign-in needs a loopback redirect, which Android
               doesn't run yet (`backend_kind_for_signin` refuses it there
               with a clear message) — hide the entry point rather than let
               the user hit that error every time. -->
          <button class="account-panel-action" onclick={() => addAccount('microsoft')}>
            <span class="icon">＋</span>
            <span class="label">Add Microsoft account</span>
          </button>
        {/if}
        {#if !$isAndroid}
          <button class="account-panel-action" onclick={addLocalFolder}>
            <span class="icon"><Icon name="folder" /></span>
            <span class="label">Add Local Folder</span>
          </button>
        {/if}
      </div>
    {/if}

    <div class="footer-row">
      <!-- Global, not per-account: the modal's own scope selector already
           defaults to $currentAccount (see AskJoddModal.svelte's `scope()`),
           so this button has no currentAccount side effect of its own — one
           entry regardless of how many accounts are signed in. -->
      <button
        class="settings-gear"
        onclick={() => askModalOpen.set(true)}
        title="Ask Jodd about your notes"
        aria-label="Ask Jodd"
      ><Icon name="chat" size={14} /></button>
      <button
        class="settings-gear"
        onclick={() => appSettingsOpen.set(true)}
        title="App settings"
        aria-label="App settings"
      ><Icon name="gear" size={14} /></button>
      <button
        class="version-label"
        onclick={() => aboutModalOpen.set(true)}
        title="About Jodd"
      >
        Jodd{appVersion ? ` v${appVersion}` : ''}
      </button>
    </div>
  </div>
</aside>

{#if settingsAccountId}
  <AccountSettings
    accountId={settingsAccountId}
    accountEmail={settingsAccountEmail}
    backendKind={settingsBackendKind}
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
  <ConfirmDialog
    title={confirmTitle}
    message={confirmMessage}
    onConfirm={confirmOk}
    onCancel={confirmCancel}
  />
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
    {#if menuCanWriteNotes}
    <button class="item" onclick={() => newNoteInFolder(menuAccountId!, menuPath!)}>
      <span class="icon"><Icon name="note-plus" /></span>
      <span class="label">New note here</span>
    </button>
    {/if}
    {#if menuCanWriteFolders}
    <div class="sep"></div>
    <button class="item" onclick={() => createFolderUnder(menuAccountId!, menuPath)}>
      <span class="icon">＋</span>
      <span class="label">New sub-folder</span>
    </button>
    {/if}
    {#if menuCanWriteFolders && menuPath !== 'Notes' && !isProtectedWorkflowFolder(folderKindsByAccount[menuAccountId!] ?? {}, menuPath!)}
      <button class="item" onclick={() => renameFolder(menuAccountId!, menuPath!)}>
        <span class="icon"><Icon name="pencil" /></span>
        <span class="label">Rename</span>
      </button>
      <!-- Delete grouped with the other direct actions above, rather than
           isolated after "Move to" — a browsable folder list, not a single
           action, so it sits last. Mirrors NoteContextMenu's ordering. -->
      <div class="sep"></div>
      <button class="item danger" onclick={() => deleteFolder(menuAccountId!, menuPath!)}>
        <span class="icon"><Icon name="trash" /></span>
        <span class="label">Delete</span>
      </button>
      <div class="sep"></div>
      <!-- Hover flyout, not an inline list — matches NoteContextMenu's
           "Move to" exactly (same .submenu-anchor/.submenu/fitSubmenu
           mechanism). Folders skip the account-picker layer notes have
           because a folder can only move within its own account. -->
      <div class="submenu-anchor" role="none" onmouseenter={fitSubmenu}>
        <button class="item has-submenu" type="button">
          <span class="icon"><Icon name="folder" /></span>
          <span class="label">Move to</span>
          <span class="chevron"><Icon name="chevron-right" size={12} /></span>
        </button>
        <div class="submenu folder-list">
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
              type="button"
            >
              <span class="label">{r.name}</span>
              {#if r._state === 'parent'}
                <span class="here-tag">(current)</span>
              {/if}
            </button>
          {/each}
        </div>
      </div>
    {:else if !menuCanWriteFolders && menuPath !== 'Notes'}
      <!-- Folder writes unavailable (Microsoft, until Task 11): this is an
           ordinary folder from the user's own Exchange mailbox, not a
           Jodd-managed workflow folder — it must not be labeled as one.
           Rename/delete are simply not implemented for this backend yet
           (roadmap #4). -->
      <div class="sep"></div>
      <div class="item header" aria-hidden="true" style="font-style: italic; opacity: 0.7;">
        <span class="icon"><Icon name="eye" /></span>
        <span class="label">Folder changes aren't available yet — rename and delete are coming in a later update</span>
      </div>
    {:else if menuPath !== 'Notes'}
      <!-- Active workflow folders (matching __name__ pattern, e.g. __Extracts__):
           Jodd-managed. Rename/Move/Delete are hidden because (a) the folder
           auto-recreates on next workflow run, and (b) renaming would orphan
           the ensure_workflow_folder lookup. New note + New sub-folder stay
           enabled because users may want sub-organization.

           Legacy workflow folders without the __*__ markers (Notes/Lessons,
           Notes/Extracts from before we standardized) take the normal
           rename/move/delete branch above — they're historical artifacts. -->
      <div class="sep"></div>
      <div class="item header" aria-hidden="true" style="font-style: italic; opacity: 0.7;">
        <span class="icon"><Icon name="bulb" /></span>
        <span class="label">Workflow folder — managed by Jodd</span>
      </div>
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
    {#if tagMenuCanWriteSidecars}
    <button class="item" onclick={() => renameTag(tagMenuAccountId!, tagMenuName!)}>
      <span class="icon"><Icon name="pencil" /></span>
      <span class="label">Rename tag</span>
    </button>
    <button class="item danger" onclick={() => deleteTag(tagMenuAccountId!, tagMenuName!)}>
      <span class="icon"><Icon name="trash" /></span>
      <span class="label">Delete tag</span>
    </button>
    {:else}
    <div class="item header" aria-hidden="true" style="font-style: italic; opacity: 0.7;">
      <span class="label">Tag changes aren't available yet</span>
    </div>
    {/if}
  </div>
{/if}

<style>
  /* Folder/Workflow group split (Task 16). Workflow group only renders
     when the account has at least one system_workflow folder. */
  .folder-group {
    margin-bottom: 4px;
  }
  .group-header {
    font-size: var(--size-xs);
    text-transform: uppercase;
    color: var(--text-muted);
    margin: 8px 0 2px 8px;
    font-weight: 600;
    letter-spacing: 0.04em;
  }
  .group-icon {
    display: inline-flex;
    align-items: center;
    margin-right: 4px;
    vertical-align: middle;
  }

  .sidebar {
    /* width is set inline by the parent (App.svelte) so it can be resized */
    background: var(--surface-sidebar);
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
    height: 100vh;
    overflow: hidden;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .account-ingest-btn {
    flex: 0 0 auto;
    width: 20px;
    height: 20px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    border-radius: 4px;
    font-size: var(--size-sm);
    line-height: var(--leading-tight);
    cursor: pointer;
    opacity: 0.7;
    transition: opacity 0.15s, background 0.15s;
  }

  .account-ingest-btn:hover {
    opacity: 1;
    background: var(--accent-wash);
  }

  .sidebar-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    /* On the phone layout Sidebar renders bare, full-screen, as the Folders
       pane (App.svelte mounts it with no .phone-pane-header wrapper there) —
       so THIS is the surface that needs status-bar clearance, not
       .phone-pane-header (which never renders for this pane). Same pattern
       as .phone-pane-header's own fix: max() keeps the existing 20px as a
       floor on desktop/tablet, where the inset is 0. */
    padding: max(20px, env(safe-area-inset-top)) 16px 12px;
    border-bottom: 1px solid var(--border);
  }

  .app-title {
    font-size: var(--size);
    font-weight: 700;
    color: var(--text-muted);
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
    color: var(--text-muted);
    font-size: var(--size-lg);
    line-height: var(--leading-tight);
    padding: 0;
  }

  .new-folder-btn:hover {
    background: var(--surface-active);
    color: var(--text);
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
    font-size: var(--size-sm);
    font-weight: 600;
    color: var(--text-secondary);
    padding: 6px 16px 4px;
    white-space: nowrap;
    display: flex;
    align-items: center;
    gap: 6px;
    /* Allow the row to shrink the email rather than overflowing when a
       pill appears alongside. */
    overflow: hidden;
  }

  .account-collapse-toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex: 0 0 auto;
    color: var(--text-muted);
    border-radius: 3px;
    cursor: pointer;
    user-select: none;
  }

  .account-collapse-toggle:hover {
    background: var(--surface-active);
    color: var(--text);
  }

  .account-kind-icon {
    display: inline-flex;
    align-items: center;
    flex: 0 0 auto;
    opacity: 0.7;
  }

  .account-row-kind-icon {
    display: inline-flex;
    align-items: center;
    font-size: var(--size-xs);
    opacity: 0.8;
    margin-right: 1px;
  }

  .account-email {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 0 1 auto;
  }

  .dup-pill {
    font-size: var(--size-xs);
    font-weight: 500;
    line-height: var(--leading-tight);
    color: var(--accent-action);
    background: var(--accent-wash);
    border: 1px solid var(--accent-border);
    border-radius: 10px;
    padding: 2px 7px;
    cursor: pointer;
    font-family: inherit;
    /* Don't grow; sit quietly next to the email. */
    flex: 0 0 auto;
    transition: background 0.15s;
  }

  .dup-pill:hover {
    background: var(--accent-wash-strong);
  }

  .dup-pill.result {
    color: var(--text-muted);
    background: var(--surface-sunken);
    border-color: var(--border);
    cursor: default;
    font-style: italic;
  }

  .folder-item {
    display: flex;
    align-items: center;
    gap: 6px;
    /* Was `width: max-content` + `min-width: calc(100% - 8px)`, which let a long
       name extend the row past the pane so .folder-list scrolled horizontally
       rather than truncating. Two problems in practice:
         1. On macOS the scrollbar is an overlay that stays hidden until you
            actually scroll, so the only affordance saying "there is more text"
            was invisible — the name just ran under the right border mid-word.
         2. Scrolling a *tree* sideways hides the left edge, which is where the
            indentation, disclosure arrow and icon live — i.e. it hides the
            hierarchy, the one thing the tree exists to show.
       The account header (.account-email) already truncated with an ellipsis,
       so folder rows were also inconsistent with the pane above them. Now both
       ellipsize, and the full name stays reachable via the title tooltip. */
    width: calc(100% - 8px);
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
    background: var(--surface-hover);
  }

  .folder-item.active {
    background: var(--surface-active);
  }

  .folder-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    font-size: var(--size-md);
  }

  .folder-name {
    flex: 1;
    /* min-width: 0 is load-bearing: a flex item defaults to min-width: auto,
       which refuses to shrink below its own content, so the ellipsis would
       never trigger and the row would just overflow again. */
    min-width: 0;
    font-size: var(--size);
    line-height: var(--leading);
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .expand-toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    font-size: var(--size-md);
    line-height: var(--leading-tight);
    color: var(--text-muted);
    border-radius: 3px;
    flex-shrink: 0;
    user-select: none;
  }

  .expand-toggle:not(.empty):hover {
    background: var(--surface-active);
    color: var(--text);
  }

  .folder-count {
    font-size: var(--size-xs);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
    /* Opaque, not black-at-8%. This badge sits inside .folder-item, whose
       own background darkens on hover (0.06) and again when active (0.10), so
       an alpha badge composited THREE layers deep: on a selected row the count
       ended up as #696969 on #c7c3ba = 3.08:1, well under AA, and the darker
       the row got the worse it read. An opaque badge makes the ratio
       independent of row state — 5.44:1 everywhere. */
    background: var(--surface-badge);
    padding: 1px 6px;
    border-radius: 10px;
  }

  .tags-header {
    display: flex;
    align-items: center;
    gap: 5px;
    margin: 8px 0 2px;
    padding: 2px 12px 2px 12px;
    font-size: var(--size-xs);
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-muted);
    cursor: pointer;
    user-select: none;
  }
  .tags-header:hover { color: var(--text-muted); }
  .tags-caret { display: inline-flex; align-items: center; justify-content: center; }
  .tags-num {
    font-size: var(--size-xs);
    color: var(--text-muted);
    /* Opaque for the same reason as .folder-count — see that rule. */
    background: var(--surface-badge);
    padding: 0 5px;
    border-radius: 8px;
    text-transform: none;
    letter-spacing: 0;
  }

  .tag-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 2px 12px 8px 16px;
  }
  .tag-pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 100%;
    border: none;
    background: var(--surface-hover);
    color: var(--text-secondary);
    font-size: var(--size-xs);
    font-family: inherit;
    padding: 2px 4px 2px 8px;
    border-radius: 11px;
    cursor: pointer;
    line-height: var(--leading);
  }
  .tag-pill:hover { background: var(--surface-active); }
  .tag-pill.active { background: var(--accent-tint); color: var(--accent-on-tint); }
  .pill-name {
    line-height: var(--leading);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 110px;
  }
  .pill-count {
    font-size: var(--size-xs);
    /* Same opaque-badge treatment as .folder-count, and no opacity. The old
       `opacity: 0.7` over a doubly-composited alpha background put this at
       2.77:1 on a resting pill — fading text is the most expensive way to
       de-emphasise something, because it costs contrast directly. The smaller
       font size already does that job. */
    background: var(--surface-badge);
    border-radius: 8px;
    padding: 0 5px;
  }

  .tag-controls {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-left: auto;
  }

  .tag-mode {
    border: 1px solid var(--accent);
    background: var(--accent-tint);
    color: var(--accent-on-tint);
    font-size: var(--size-xs);
    font-weight: 700;
    letter-spacing: 0.04em;
    padding: 0 5px;
    border-radius: 8px;
    cursor: pointer;
    line-height: 1.5;
  }

  .tag-mode:hover {
    background: var(--accent-tint);
  }

  .tag-clear {
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: var(--size-xs);
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    cursor: pointer;
    padding: 0 2px;
  }

  .tag-clear:hover {
    color: var(--danger);
  }

  .sidebar-footer {
    position: relative;
    padding: 8px 10px 12px;
    border-top: 1px solid var(--border);
  }

  .footer-row {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    margin-top: 2px;
    /* Reserve space above Android's gesture nav bar so Ask Jodd / App
       settings / About stay tappable (reported on-device via screenshot,
       Galaxy S23 FE: this row sat behind the 3-button nav bar). .sidebar is
       height: 100vh, which on Android does not itself reserve nav-bar space
       the way env(safe-area-inset-bottom) does. No `max()` floor here —
       .footer-row had no pre-existing bottom padding, so an artificial
       minimum (e.g. max(8px, ...)) would add visible padding on desktop
       too, where the inset is 0. Plain env() resolves to exactly 0 there,
       an honest no-op; on Android it adds exactly the real inset, which is
       already the nav bar's height (no extra cushion needed on top of it). */
    padding-bottom: env(safe-area-inset-bottom);
  }
  .settings-gear {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-muted);
    font-size: var(--size);
    padding: 2px 4px;
    line-height: var(--leading-tight);
    flex-shrink: 0;
  }
  .settings-gear:hover { color: var(--text-secondary); }
  .version-label {
    display: block;
    text-align: center;
    padding: 4px 0;
    border: none;
    background: none;
    color: var(--text-muted);
    font-size: var(--size-xs);
    cursor: pointer;
  }
  .version-label:hover { color: var(--text-secondary); text-decoration: underline; }

  .account-chip {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 6px 8px;
    background: var(--surface-sunken);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    cursor: pointer;
    font-family: inherit;
    color: var(--text);
    text-align: left;
  }

  .account-chip:hover {
    background: var(--surface-hover);
  }

  .account-chip-dot {
    color: var(--success);
    font-size: var(--size-xs);
    flex-shrink: 0;
  }

  .account-chip-email {
    flex: 1;
    font-size: var(--size-sm);
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .account-chip-caret {
    display: inline-flex;
    align-items: center;
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .account-panel {
    position: absolute;
    bottom: calc(100% + 4px);
    left: 8px;
    right: 8px;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-menu-up);
    padding: 4px;
    z-index: 100;
    font-size: var(--size);
  }

  .account-panel-header {
    font-size: var(--size-xs);
    font-weight: 700;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    padding: 6px 10px 4px;
  }

  .account-row {
    display: flex;
    align-items: stretch;
  }

  .account-row.active {
    background: var(--accent-wash);
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
    color: var(--text);
    border-radius: 4px;
    font-family: inherit;
    min-width: 0;
  }

  .account-row-pick:hover {
    background: var(--surface-sunken);
  }

  /* --accent-action, not --accent: this dot's colour IS text (WCAG 1.4.11
     applies), and tokens.css reserves --accent for fills/tints/borders with
     no text on them -- --accent measured 2.77:1 here, failing the 3:1 floor
     for an informational non-text indicator. Not --focus either: this dot
     means "this is the active account", which is state the user reads, so
     it earns the brand colour; focus blue is reserved for where keyboard
     focus actually is. */
  .account-row-dot {
    font-size: var(--size-xs);
    color: var(--accent-action);
    flex-shrink: 0;
  }

  .account-row-email {
    flex: 1;
    font-size: var(--size-sm);
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .account-row-remove,
  .account-row-settings {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    background: none;
    border: none;
    font-size: var(--size);
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.15s;
    border-radius: 4px;
  }
  .account-row-remove { color: var(--danger); font-size: var(--size-sm); }
  .account-row-settings { color: var(--text-muted); }

  .account-row:hover .account-row-remove,
  .account-row:hover .account-row-settings {
    opacity: 1;
  }

  .account-row-remove:hover {
    background: var(--danger-wash);
  }
  .account-row-settings:hover {
    background: var(--surface-hover);
    color: var(--text);
  }

  .inactive-toggle {
    display: flex;
    align-items: center;
    gap: 4px;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    cursor: pointer;
    font-family: inherit;
  }
  .inactive-toggle:hover {
    background: var(--surface-hover);
    border-radius: 4px;
  }

  .account-row.dimmed {
    opacity: 0.7;
    gap: 6px;
    align-items: center;
    padding: 4px 10px;
    /* Wrap, so the name gets a line to itself and the actions fall below it.
       On one line the two buttons plus the "finishing sync — N left" note left
       the name about 60px, which truncated every account to "kaiw…" — with
       several dismissed accounts they were indistinguishable, and the group's
       whole job is to let you pick one out and bring it back. */
    flex-wrap: wrap;
  }
  .account-row.dimmed .account-row-email {
    /* Full width of its own row; ellipsis now only for genuinely long names. */
    flex: 1 0 100%;
    font-size: var(--size-sm);
  }
  .account-row-note {
    flex-shrink: 0;
    font-size: var(--size-xs);
    color: var(--text-muted);
    white-space: nowrap;
  }
  .account-row-note.warn {
    color: var(--danger);
  }
  .account-row.dimmed button {
    flex: none;
    width: auto;
    opacity: 1;
    padding: 3px 8px;
    font-size: var(--size-xs);
    border: 1px solid var(--border);
    border-radius: 4px;
    background: none;
    color: var(--text-secondary);
    cursor: pointer;
    font-family: inherit;
  }
  .account-row.dimmed button:hover {
    background: var(--surface-hover);
  }
  .account-row.dimmed .account-row-remove {
    color: var(--danger);
    border-color: var(--danger);
  }
  .account-row.dimmed .account-row-remove:hover {
    background: var(--danger-wash);
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
    color: var(--text);
    border-radius: 4px;
    font-family: inherit;
    font-size: var(--size);
  }

  .account-panel-action:hover {
    background: var(--surface-hover);
  }

  /* --text-muted, not --focus: these are decorative affordance icons on menu
     rows. They carry no state, so they should sit behind their labels rather
     than compete with them for attention. */
  .account-panel .icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    text-align: center;
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .account-panel .sep {
    height: 1px;
    background: var(--surface-active);
    margin: 4px 8px;
  }

  .folder-menu {
    position: fixed;
    min-width: 180px;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-menu);
    padding: 4px;
    z-index: 1000;
    font-size: var(--size);
    /* No max-height/overflow-y here (unlike the old D9 fix): this menu is
       now a fixed handful of items since Move-to is a flyout, not an inline
       list — see .submenu-anchor below. overflow on this ancestor would
       clip the flyout, which is position:absolute; left:100% and escapes
       this box on purpose. The flyout's own .submenu.folder-list carries
       max-height/overflow-y for its potentially-long folder list instead. */
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
    color: var(--text);
    border-radius: 4px;
  }

  .folder-menu .item:hover {
    background: var(--surface-hover);
  }

  .folder-menu .item.danger {
    color: var(--danger);
  }

  .folder-menu .item.header {
    color: var(--text-muted);
    font-size: var(--size-xs);
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
    color: var(--text-muted);
    cursor: default;
  }
  .folder-menu .item.folder.current-location:hover {
    background: none;
  }
  .folder-menu .here-tag {
    margin-left: 6px;
    font-size: var(--size-xs);
    color: var(--text-muted);
    font-style: italic;
  }

  .folder-menu .icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
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
    background: var(--surface-active);
    margin: 4px 8px;
  }

  /* "Move to" hover flyout — ported from NoteContextMenu's .submenu-anchor
     so both context menus' Move-to behaves identically. */
  .folder-menu .submenu-anchor {
    position: relative;
  }
  .folder-menu .submenu-anchor > .submenu {
    display: none;
    position: absolute;
    left: 100%;
    top: -4px;
    min-width: 180px;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-menu);
    padding: 4px;
    z-index: 1001;
  }
  :global(.folder-menu .submenu-anchor > .submenu.flip-left) {
    left: auto;
    right: 100%;
  }
  .folder-menu .submenu-anchor > .submenu.folder-list {
    max-height: 60vh;
    overflow-y: auto;
  }
  .folder-menu .submenu-anchor:hover > .submenu {
    display: block;
  }
  .folder-menu .chevron {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    margin-left: auto;
    color: var(--text-disabled);
    font-size: var(--size-xs);
  }
  .folder-menu .item.has-submenu {
    cursor: default;
  }

  .prompt-overlay {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 2000;
  }

  .prompt-dialog {
    background: var(--surface-panel);
    min-width: 320px;
    max-width: 80vw;
    padding: 18px 20px 14px;
    border-radius: 10px;
    box-shadow: var(--shadow-modal);
  }

  .prompt-title {
    font-size: var(--size);
    font-weight: 600;
    color: var(--text);
    line-height: var(--leading);
    margin-bottom: 10px;
  }

  .prompt-input {
    width: 100%;
    padding: 7px 9px;
    font-size: var(--size);
    line-height: var(--leading);
    border: 1px solid var(--border);
    border-radius: 5px;
    outline: none;
    /* An <input> with no background falls back to the user agent's own white,
       which does not follow the theme. That put --text (near-white in dark
       mode) on a white field and made this dialog unreadable. A form control
       must always state its background explicitly — inheriting is not an
       option, because the UA stylesheet wins over inheritance here. */
    background: var(--surface-panel);
    color: var(--text);
    font-family: inherit;
  }
  .prompt-input::placeholder {
    color: var(--text-muted);
  }

  .prompt-input:focus {
    border-color: var(--focus);
    box-shadow: var(--ring-focus);
  }

  /* .confirm-message moved to ConfirmDialog.svelte with confirmOpen's markup
     above (Task 10b) — promptOpen's askName dialog never used it (it shows
     an <input>, not a message), so there's nothing left depending on it
     here. */

  .prompt-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 12px;
  }

  .prompt-btn {
    padding: 5px 14px;
    font-size: var(--size-sm);
    border-radius: 5px;
    border: 1px solid var(--border);
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
  }

  .prompt-btn:hover {
    background: var(--surface-list);
  }

  .prompt-btn.primary {
    background: var(--accent-action);
    color: var(--text-inverse);
    border-color: var(--accent-action);
  }

  .prompt-btn.primary:hover {
    background: var(--accent-hover);
  }
</style>
