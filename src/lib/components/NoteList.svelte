<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { get } from 'svelte/store';
  import { pendingDeletedNoteKeys, noteMutationRevision, notes, selectedFolder, selectedNote, isLoading, refreshNotes, currentAccount, activeAccounts, selectedUuids, clearSelectedUuids, selectedTags, tagMatchMode, toggleSelectedTag, noteTagsByAccount, getNoteTags, searchQuery, accountDisplay, selectedSmartFolder, smartFolderNotes, noteIndex, hydratedFolders, newNoteFn, currentAccountCanWrite } from '../stores/notes';
  import type { Note } from '../types';
  import { navigateToPane } from '../stores/phoneNav';
  import { longpress } from '../longpress';
  import { notePreview } from '../notePreview';
  import NoteContextMenu from './NoteContextMenu.svelte';
  import LinkSuggestionsModal from './LinkSuggestionsModal.svelte';
  import { noteKey, sameNote, canonicalNote } from '../noteIdentity';
  import { invoke } from '@tauri-apps/api/core';
  import Icon from './Icon.svelte';
  // A title that is only, or starts with, an orphaned combining mark (e.g. a
  // lone Thai sara-uee, U+0E37) renders BLANK in Jodd on Windows while iCloud
  // shows a dotted circle plus the mark; this makes the hidden character
  // visible for display only (never stored). See src/lib/textDisplay.ts.
  import { displayTitle } from '../textDisplay';

  // Width is owned by App.svelte so the divider between this pane and the
  // sidebar can be dragged. Default matches the pre-resizer 240px look.
  export let width: number = 240;

  // Cross-note search query (shared store — Sidebar/Editor can clear it).
  // When non-empty, the filtered list switches from "notes in current folder"
  // to "notes matching query across all folders" — mirrors Apple Notes' search.

  // FTS-backed search — covers ALL notes via the SQLite index (Thai-aware,
  // trigram), at a user-chosen scope. Filled by a debounced backend call; a
  // sequence guard drops stale responses.
  type SearchScope = 'folder' | 'account' | 'all';
  let searchScope: SearchScope = 'account';
  let searchResults: Note[] = [];
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  let searchSeq = 0;
  let destroyed = false;
  type RequestState = 'idle' | 'loading' | 'ready' | 'error';
  let searchState: RequestState = 'idle';
  let tagState: RequestState = 'idle';
  $: hasSearch = !!$searchQuery.trim();
  $: hasFolderScope = !!$selectedFolder && $selectedFolder !== '__ALL__' && $selectedFolder !== '__TRASH__';
  $: scopeName = searchScope === 'all' ? 'all accounts'
    : searchScope === 'folder' && hasFolderScope ? 'this folder' : 'this account';
  $: requestState = hasSearch ? searchState
    : $selectedTags.size > 0 && tagScope === 'all' ? tagState : 'idle';
  $: resultStatus = hasSearch
    ? searchState === 'loading' ? 'Searching…'
      : searchState === 'error' ? 'Could not search. Please retry.'
      : `${filteredNotes.length} results in ${scopeName}`
    : $selectedTags.size > 0
      ? tagState === 'loading' && tagScope === 'all' ? 'Loading tagged notes…'
        : tagState === 'error' && tagScope === 'all' ? 'Could not load tagged notes. Please retry.'
        : `${filteredNotes.length} tagged notes`
      : '';

  function retryQuery() {
    if (hasSearch) scheduleSearch($searchQuery, searchScope, $currentAccount, $selectedFolder);
    else fetchCrossAccountTags($selectedTags, tagScope, $currentAccount);
  }

  onDestroy(() => {
    destroyed = true;
    ++searchSeq;
    ++tagSeq;
    clearTimeout(searchTimer);
  });
  $: { void $noteMutationRevision; scheduleSearch($searchQuery, searchScope, $currentAccount, $selectedFolder); }

  function scheduleSearch(raw: string, scope: SearchScope, account: string | null, folder: string) {
    clearTimeout(searchTimer);
    // Invalidate before the debounce, including clear and missing-account paths.
    const seq = ++searchSeq;
    searchResults = [];
    searchState = 'idle';
    const query = raw.trim();
    if (!query) return;
    // Resolve scope → optional account/label filters (null = don't filter).
    let accountId: string | null = null;
    let label: string | null = null;
    if (scope === 'folder') {
      accountId = account;
      label = folder && folder !== '__ALL__' && folder !== '__TRASH__' ? folder : null;
    } else if (scope === 'account') {
      accountId = account;
    } // 'all' → both null = every account
    if (scope !== 'all' && !account) return;
    searchState = 'loading';
    const isCurrent = () => !destroyed && seq === searchSeq
      && get(searchQuery) === raw && searchScope === scope
      && get(currentAccount) === account && get(selectedFolder) === folder;
    searchTimer = setTimeout(async () => {
      if (!isCurrent()) return;
      try {
        const res = await invoke<Note[]>('search_notes', { accountId, label, query });
        if (!isCurrent()) return;
        searchResults = res;
        searchState = 'ready';
      } catch (e) {
        if (isCurrent()) searchState = 'error';
      }
    }, 150);
  }

  // Context menu state. menuNote === null means hidden.
  // menuSelection is the multi-note batch to operate on; populated when the
  // user right-clicks a note that's part of the current multi-select. When
  // length <= 1, the menu falls back to single-note mode.
  let menuNote: Note | null = null;
  let menuSelection: Note[] = [];
  let menuX = 0;
  let menuY = 0;

  function openContextMenu(e: MouseEvent, note: Note) {
    e.preventDefault(); // suppress the OS default menu
    e.stopPropagation();
    menuX = e.clientX;
    menuY = e.clientY;
    menuNote = note;
    // If the right-clicked note is part of the active multi-select, the
    // menu operates on the whole batch. Otherwise treat this as a single-
    // note action (and don't disturb the existing selection — the user
    // may have selected a batch and is just inspecting a different note).
    const sel = $selectedUuids;
    if (sel.size > 1 && sel.has(noteKey(note))) {
      menuSelection = filteredNotes.filter((n) => sel.has(noteKey(n)));
    } else {
      menuSelection = [];
    }
  }

  function closeContextMenu() {
    menuNote = null;
    menuSelection = [];
  }

  // #2b retroactive linking review modal (NoteContextMenu's "Link into
  // wiki" action). Owned here, not inside NoteContextMenu, because that
  // menu unmounts itself via onClose() before the async suggest/save calls
  // resolve — a modal opened from local state inside the menu would never
  // render. NoteContextMenu reports the result via onLinkSuggestions.
  let linkSuggestionsOpen = false;
  let linkSuggestionsAccountId = '';
  let proposedAppends: { uuid: string; title: string; addition_text: string }[] = [];

  function handleLinkSuggestions(
    accountId: string,
    appends: { uuid: string; title: string; addition_text: string }[],
  ) {
    linkSuggestionsAccountId = accountId;
    proposedAppends = appends;
    linkSuggestionsOpen = true;
  }

  // Last single-clicked (non-modifier) uuid. Used as the anchor for shift-
  // click range selection. Reset whenever the user does a plain click.
  let selectionAnchor: string | null = null;

  async function refresh() {
    await $refreshNotes();
  }

  // Display only the last path segment in the header so deep folders don't
  // push the action buttons out of the column. Full path stays on
  // $selectedFolder for filtering and "new note" label assignment.
  // Display name in the list-pane header. The __ALL__ sentinel reads as a
  // friendly "All <email>" so users see what scope they're looking at.
  // Workflow folder names like `__Extracts__` get their underscore markers
  // stripped for display, mirroring the Sidebar's stripWorkflowMarkers helper.
  function stripWorkflowMarkers(name: string): string {
    return name.startsWith('__') && name.endsWith('__') && name.length > 4
      ? name.slice(2, -2)
      : name;
  }
  $: currentAccountDisplay = accountDisplay($activeAccounts.find((a) => a.id === $currentAccount), $currentAccount ?? '');
  $: headerName = $selectedFolder === '__ALL__'
    ? `All ${currentAccountDisplay}`
    : stripWorkflowMarkers($selectedFolder?.split('/').pop() || $selectedFolder || '');

  // True when the current folder shows an empty list ONLY because its notes
  // haven't been hydrated into the local cache yet — as opposed to being
  // genuinely empty. The server index ($noteIndex, id + label, no bodies)
  // lands on cold start before the per-folder cache, so it can know a folder
  // has notes the painted list doesn't yet.
  //   - Only meaningful in the plain single-folder view; search / tags / smart
  //     folders / __ALL__ + __TRASH__ don't map to one label.
  //   - Gated on $hydratedFolders: once loadFolderNotes has reconciled this
  //     folder against Gmail this session, the cache is authoritative, so an
  //     empty list means empty — even if the (never-updated-on-delete) index
  //     still counts a just-deleted note. Prevents a stuck "Loading…" after
  //     deleting a folder's last note.
  $: folderLoading = (() => {
    if ($searchQuery.trim() || $selectedTags.size > 0 || $selectedSmartFolder) return false;
    if (!$selectedFolder || $selectedFolder === '__ALL__' || $selectedFolder === '__TRASH__') return false;
    const acct = $currentAccount ?? '';
    if (($hydratedFolders.get(acct) ?? new Set<string>()).has($selectedFolder)) return false;
    const idx = $noteIndex.get(acct) ?? [];
    return idx.some((s) => s.label === $selectedFolder);
  })();

  // Keyboard navigation: arrow up/down moves the selection within the current
  // filtered list. Only active when a note in this folder is selected so we
  // don't intercept arrows in the editor. Cmd+A in the list pane selects all
  // notes in the current filtered view (but yields to the editor's native
  // Cmd+A when the editor is focused).
  function onKey(e: KeyboardEvent) {
    const cmd = e.metaKey || e.ctrlKey;
    if (cmd && e.key.toLowerCase() === 'a') {
      const ae = document.activeElement;
      // Only intercept when no editable surface owns the focus.
      if (ae && (ae.tagName === 'INPUT' || ae.tagName === 'TEXTAREA' || ae.getAttribute('contenteditable') === 'true')) {
        return;
      }
      e.preventDefault();
      const all = new Set(filteredNotes.map(noteKey));
      selectedUuids.set(all);
      return;
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    // Don't hijack arrows while an editable surface owns the focus — the caret
    // should move inside the editor / input, not jump to the next note.
    const ae = document.activeElement;
    if (
      ae &&
      (ae.tagName === 'INPUT' ||
        ae.tagName === 'TEXTAREA' ||
        ae.getAttribute('contenteditable') === 'true' ||
        ae.closest('[contenteditable="true"]'))
    ) {
      return;
    }
    if (!$selectedNote) return;
    const idx = filteredNotes.findIndex(n => sameNote(n, $selectedNote));
    if (idx < 0) return;
    e.preventDefault();
    const nextIdx = e.key === 'ArrowDown'
      ? Math.min(idx + 1, filteredNotes.length - 1)
      : Math.max(idx - 1, 0);
    if (nextIdx !== idx) {
      clearSelectedUuids();
      selectedNote.set(filteredNotes[nextIdx]);
      selectionAnchor = noteKey(filteredNotes[nextIdx]);
    }
  }

  onMount(() => window.addEventListener('keydown', onKey));
  onDestroy(() => window.removeEventListener('keydown', onKey));

  // Search-aware filter:
  //  - When query is empty → notes in current folder only.
  //  - When query has text → notes whose title OR plain-text body contains
  //    the query, across ALL folders. Case-insensitive.
  // Filter scope:
  //   selectedFolder === '__ALL__' → every note in currentAccount
  //   no search → notes in (currentAccount, selectedFolder)
  //   with search → notes in currentAccount across all its folders
  // The '__ALL__' sentinel is the convention for Sidebar's "All <account>"
  // virtual folder (matches Apple Notes' per-account aggregate view).
  // Tag-filter scope (parallel to searchScope): 'account' = current account
  // (in-memory $notes), 'all' = every account, fetched from the backend.
  let tagScope: 'account' | 'all' = 'account';
  let tagResults: Note[] = [];
  let tagSeq = 0;
  $: { void $noteMutationRevision; fetchCrossAccountTags($selectedTags, tagScope, $currentAccount); }
  async function fetchCrossAccountTags(tags: Set<string>, scope: 'account' | 'all', account: string | null) {
    const seq = ++tagSeq;
    tagResults = [];
    tagState = 'idle';
    if (scope !== 'all' || tags.size === 0) return;
    const requestedTags = [...tags];
    const isCurrent = () => !destroyed && seq === tagSeq && scope === tagScope
      && get(currentAccount) === account
      && requestedTags.length === get(selectedTags).size
      && requestedTags.every((tag) => get(selectedTags).has(tag));
    tagState = 'loading';
    try {
      const result = await invoke<Note[]>('list_cached_notes_with_tags', {
        accountId: null, // null → every account; preserve backend union semantics
        tags: requestedTags,
      });
      if (!isCurrent()) return;
      tagResults = result;
      tagState = 'ready';
    } catch (e) {
      if (isCurrent()) tagState = 'error';
    }
  }

  // Apple splits the list into a "Pinned" group and everything else, with a
  // heading over each. filteredNotes is already sorted pinned-first, so the
  // boundary is just the count — no second pass, and no risk of the headings
  // disagreeing with the order.
  //
  // Suppressed when nothing is pinned (Apple shows no headings then either)
  // and while searching, where the result set is the point and a "Pinned"
  // heading over part of it reads as a filter the user did not ask for.
  $: pinnedCount = filteredNotes.filter((n) => n.pinned).length;
  $: showPinSections = pinnedCount > 0 && !$searchQuery.trim();

  $: filteredNotes = (() => {
    const q = $searchQuery.trim().toLowerCase();
    const inAccount = (n: Note) => n.account_id === $currentAccount;
    let base: Note[];
    if (!q) {
      if ($selectedSmartFolder) {
        // Smart Folders are fully virtual — the note set comes straight from
        // App.svelte's loadSmartFolderNotes, not from filtering $notes by
        // label (there is no label to match).
        base = $smartFolderNotes;
      } else if ($selectedTags.size > 0) {
        // Tag view takes precedence over the folder selection. AND = note has
        // every selected tag; OR = note has any. App.paintTagsFromCache has
        // already loaded the union of these tags' notes into $notes.
        const sel = [...$selectedTags];
        const mode = $tagMatchMode;
        const matches = (n: Note) => {
          const tags = getNoteTags($noteTagsByAccount, n.account_id, n.uuid);
          return mode === 'AND'
            ? sel.every((t) => tags.includes(t))
            : sel.some((t) => tags.includes(t));
        };
        // 'all' → cross-account results from the backend (no inAccount filter);
        // 'account' → narrow the in-memory current-account notes.
        base = tagScope === 'all'
          ? tagResults.filter(matches)
          : $notes.filter((n) => inAccount(n) && matches(n));
      } else {
        base = $selectedFolder === '__ALL__'
          ? $notes.filter(inAccount)
          : $notes.filter((n) => inAccount(n) && n.label === $selectedFolder);
      }
    } else {
      // FTS-backed: results come from the SQLite index (every note, Thai-aware),
      // not an in-memory substring scan limited to loaded notes.
      base = searchResults; // already scoped by the backend (may span accounts)
    }
    // Pinned-first sort. Date is RFC 2822 or ISO; Date.parse handles both
    // and returns NaN-safe when malformed (treated as 0, sinks to the
    // bottom of its group rather than crashing the sort). Stable sort
    // semantics keep within-group order consistent with the source list,
    // which matters for the "multiple pinned notes keep their relative
    // date order" check in the verification step.
    const live = new Map($notes.map(n => [noteKey(n), n]));
    const unique = new Map<string, Note>();
    for (const row of base) {
      const key = noteKey(row);
      if ($pendingDeletedNoteKeys.has(key)) continue;
      const local = live.get(key);
      const value = local && (local.local_version ?? 0) >= (row.local_version ?? 0) ? local : row;
      unique.set(key, { ...value, uuid: canonicalNote(value).uuid });
    }
    return [...unique.values()].sort((a, b) => {
      const ap = a.pinned ? 1 : 0;
      const bp = b.pinned ? 1 : 0;
      if (ap !== bp) return bp - ap;            // pinned DESC
      const ad = Date.parse(a.date) || 0;
      const bd = Date.parse(b.date) || 0;
      return bd - ad;                            // date DESC
    });
  })();

  // Three click modes:
  //   - plain click      → single select (clears multi), sets editor focus
  //   - cmd/ctrl+click   → toggle this note in the multi-select set
  //   - shift+click      → range select from anchor to this note in
  //                        filteredNotes order; if no anchor, falls back
  //                        to single
  // The "primary" $selectedNote (what the editor shows) is the most
  // recently clicked note regardless of mode, so cmd-clicking adds to the
  // batch AND shifts the editor to that note. Matches Apple Notes' feel.
  // When the user picks a note from a search-result list, light up its
  // containing folder in the sidebar so they can see where the note lives.
  // Keep search active — clearing it would collapse the result list and
  // throw away the context they navigated through. Skipped outside search
  // to honour the virtual "All <account>" view (user intentionally chose
  // the aggregate; per-note folder switching would feel jumpy).
  function followNoteContext(note: Note) {
    if (note.account_id && note.account_id !== $currentAccount) {
      currentAccount.set(note.account_id);
    }
    if ($searchQuery && note.label && $selectedFolder !== note.label) {
      selectedFolder.set(note.label);
    }
  }

  function onNoteClick(e: MouseEvent, note: Note) {
    const cmd = e.metaKey || e.ctrlKey;
    const shift = e.shiftKey;

    if (cmd) {
      selectedUuids.update((s) => {
        const next = new Set(s);
        if (next.has(noteKey(note))) next.delete(noteKey(note));
        else next.add(noteKey(note));
        return next;
      });
      selectedNote.set(note);
      selectionAnchor = noteKey(note);
      return;
    }

    if (shift && selectionAnchor) {
      const list = filteredNotes;
      const a = list.findIndex((n) => noteKey(n) === selectionAnchor);
      const b = list.findIndex((n) => sameNote(n, note));
      if (a >= 0 && b >= 0) {
        const [lo, hi] = a <= b ? [a, b] : [b, a];
        const range = new Set<string>();
        for (let i = lo; i <= hi; i++) range.add(noteKey(list[i]));
        selectedUuids.set(range);
        selectedNote.set(note);
        return;
      }
    }

    // Plain click: clear multi, single-select.
    clearSelectedUuids();
    selectedNote.set(note);
    selectionAnchor = noteKey(note);
    followNoteContext(note);
    // App.svelte opens the note pane when the selection CHANGES; a tap on the
    // note that is already selected (the one Extract or ingest just created)
    // changes nothing, so open it here. A no-op off the phone layout and when
    // the note pane is already showing.
    navigateToPane('note');
  }

  // Legacy helper for keyboard activation paths.
  function selectNote(note: Note) {
    clearSelectedUuids();
    followNoteContext(note);
    selectedNote.set(note);
    selectionAnchor = noteKey(note);
  }

  // Clear multi-select whenever the folder or account scope changes — keeps
  // selections from leaking across views in a way users can't see.
  $: if ($selectedFolder || $currentAccount) {
    clearSelectedUuids();
    selectionAnchor = null;
  }

  // (Cmd+A is wired in the main `onKey` listener below.)

  function formatDate(dateStr: string): string {
    try {
      const d = new Date(dateStr);
      const now = new Date();
      // Calendar-day delta in LOCAL timezone, not UTC ms / 86_400_000 which
      // would mis-bucket near-midnight events when crossing UTC.
      const dStart = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
      const nowStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
      const days = Math.round((nowStart - dStart) / 86400000);
      if (days === 0) return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false });
      if (days === 1) return 'Yesterday';
      if (days < 7) return d.toLocaleDateString([], { weekday: 'long' });
      return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
    } catch { return dateStr; }
  }

  function newNote() {
    // Same gate the button/prompt below hide behind, checked again here so
    // App.svelte's ⌘N — which calls this function directly via `newNoteFn`,
    // bypassing whatever's rendered — can't create a note the backend will
    // then refuse. `refuse_if_read_only` (lib.rs) refuses the eventual push
    // regardless, but that leaves a `tmp:` row stuck in the list forever,
    // since nothing ever calls mark_pushed for it — a ghost note that looks
    // like data loss. See `canWriteAccount` for the optimistic default.
    if (!get(currentAccountCanWrite)) return;
    const tmpUuid = 'tmp:' + Math.random().toString(36).slice(2, 10);
    // When the user is viewing the virtual "All <account>" folder, default
    // the new note to the account's "Notes" root — '__ALL__' is not a real
    // Gmail label and would either silently fall back or fail on save.
    const label = $selectedFolder === '__ALL__' ? 'Notes' : $selectedFolder;
    const blank: Note = {
      id: '', uuid: tmpUuid,
      title: 'New Note',
      body_html: '<html><head></head><body></body></html>',
      date: new Date().toISOString(),
      label,
      x_mail_created_date: null,
      account_id: $currentAccount,
    };
    // Prepend so it shows at the top of the list immediately.
    notes.update((ns) => [blank, ...ns]);
    selectedNote.set(blank);
  }

  // Publish newNote() so App.svelte's global ⌘N can reach it — same
  // function-pointer idiom as $refreshNotes, pointing the other way. Reset on
  // destroy so a stale pointer into an unmounted NoteList can't be invoked.
  newNoteFn.set(newNote);
  onDestroy(() => newNoteFn.set(() => {}));
</script>

<div class="note-list" style="width: {width}px; min-width: {width}px;">
  <div class="search-bar">
    <svg class="search-icon" width="12" height="12" viewBox="0 0 16 16" fill="none">
      <circle cx="7" cy="7" r="5" stroke="currentColor" stroke-width="1.5"/>
      <path d="M11 11l3 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
    </svg>
    <input
      class="search-input"
      type="search"
      bind:value={$searchQuery}
      placeholder={`Search ${scopeName}...`}
      aria-label={`Search ${scopeName}`}
    />
    {#if $searchQuery}
      <button class="clear-btn" onclick={() => ($searchQuery = '')} aria-label="Clear search"><Icon name="close" size={12} /></button>
    {/if}
  </div>
  {#if hasSearch}
    <div class="search-scope">
      <span class="scope-label">Search in:</span>
      <select class="scope-select" aria-label="Search scope" bind:value={searchScope}>
        <option value="folder">{hasFolderScope ? 'This folder' : 'This account (all folders)'}</option>
        <option value="account">This account</option>
        <option value="all">All accounts</option>
      </select>
    </div>
  {:else if $selectedTags.size > 0}
    <div class="search-scope">
      <span class="scope-label"><Icon name="tag" size={12} /> {[...$selectedTags].map((t) => '#' + t).join(' ')} — search in:</span>
      <select class="scope-select" aria-label="Tag scope" bind:value={tagScope}>
        <option value="account">This account</option>
        <option value="all">All accounts</option>
      </select>
    </div>
  {/if}
  <div class="list-header">
    <h2 title={$selectedFolder}>{hasSearch ? (searchState === 'loading' ? 'Searching…' : searchState === 'error' ? 'Search unavailable' : `Results: ${filteredNotes.length}`) : headerName}</h2>
    <div class="header-actions">
      <button
        class="icon-btn"
        onclick={refresh}
        disabled={$isLoading}
        title="Refresh"
        aria-label="Refresh notes"
      >
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" class:spinning={$isLoading}>
          <path d="M13.5 8a5.5 5.5 0 1 1-1.61-3.89L13.5 5.5M13.5 2v3.5H10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
      </button>
      {#if $currentAccountCanWrite}
      <button class="icon-btn" onclick={newNote} title="New Note" aria-label="New note">
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
          <path d="M8 3v10M3 8h10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
        </svg>
      </button>
      {/if}
    </div>
  </div>

  <!-- Only show the full-pane spinner on first load (no notes in store yet).
       Background refreshes (focus/poll/activity) keep the current list visible
       — the ⟳ button's spinning icon already signals "refresh in progress". -->
  <div class="query-status" role="status" aria-live="polite" aria-atomic="true">{resultStatus}</div>
  {#if requestState === 'loading'}
    <div class="empty-state" aria-busy="true"><div class="spinner"></div><p>{hasSearch ? 'Searching…' : 'Loading tagged notes…'}</p></div>
  {:else if requestState === 'error'}
    <div class="empty-state"><p>{hasSearch ? 'Could not search.' : 'Could not load tagged notes.'}</p><button class="new-note-prompt" onclick={retryQuery}>Retry</button></div>
  {:else if hasSearch && filteredNotes.length === 0}
    <div class="empty-state"><p>No results for “{$searchQuery.trim()}” in {scopeName}</p><button class="new-note-prompt" onclick={() => ($searchQuery = '')}>Clear search</button></div>
  {:else if $selectedTags.size > 0 && filteredNotes.length === 0}
    <div class="empty-state"><p>No notes match the selected tags</p></div>
  {:else if $isLoading && $notes.length === 0}
    <div class="empty-state">
      <div class="spinner"></div>
      <p>Loading notes...</p>
    </div>
  {:else if filteredNotes.length === 0 && folderLoading}
    <!-- Not empty — the server index knows this folder has notes the local
         cache hasn't hydrated yet. Show a loading state, NOT "No notes /
         Create one", which wrongly implies the folder is empty. paintFolder-
         FromCache has already kicked off an immediate fetch to fill it. -->
    <div class="empty-state">
      <div class="spinner"></div>
      <p>Loading notes...</p>
    </div>
  {:else if filteredNotes.length === 0}
    <div class="empty-state">
      <p>No notes in this folder</p>
      {#if $currentAccountCanWrite}
      <button class="new-note-prompt" onclick={newNote}>Create one</button>
      {/if}
    </div>
  {:else}
    <ul>
      {#each filteredNotes as note, i (noteKey(note))}
        {#if showPinSections && i === 0}
          <li class="list-section" role="presentation">Pinned</li>
        {:else if showPinSections && i === pinnedCount}
          <li class="list-section" role="presentation">Notes</li>
        {/if}
        <li
          class="note-item"
          class:active={sameNote($selectedNote, note)}
          class:multi-selected={$selectedUuids.has(noteKey(note))}
        >
          <div
            class="note-btn"
            role="button"
            tabindex="0"
            onclick={(e) => onNoteClick(e, note)}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectNote(note); } }}
            oncontextmenu={(e) => openContextMenu(e, note)}
            use:longpress
            draggable={!!note.id}
            ondragstart={(e) => {
              console.log('[dnd] dragstart note', note.uuid);
              if (!note.id || !e.dataTransfer) return;
              // Payload schema understood by Sidebar's onDrop. Custom MIME
              // distinguishes note drags from folder drags on the same drop target.
              e.dataTransfer.effectAllowed = 'move';
              e.dataTransfer.setData(
                'application/x-jodd-note',
                JSON.stringify({ id: note.id, uuid: note.uuid, label: note.label, account_id: note.account_id })
              );
            }}
          >
            <div class="note-title" title={displayTitle(note.title) || 'Untitled'}>
              {#if note.pinned}<span class="pin-indicator" role="img" aria-label="Pinned" title="Pinned"><Icon name="pin" size={12} /></span>{/if}
              <!-- The worker has stopped trying to send this note. It is in
                   this list only because list_notes/list_notes_in_folder now
                   merge blocked cache rows back in (append_blocked_notes,
                   lib.rs) — a note whose create was refused has no remote
                   object, so before that it disappeared 30s after the user
                   saved it. Marked here so it doesn't read as an ordinary
                   synced note; the reason and the retry live in the editor. -->
              {#if note.push_blocked_reason}<span
                class="unsynced-indicator"
                role="img"
                aria-label="Not synced"
                title={`Not synced — ${note.push_blocked_reason}`}>!</span>{/if}
              {displayTitle(note.title) || 'Untitled'}
            </div>
            <div class="note-meta">
              {#if note.account_id && (($searchQuery && searchScope === 'all') || ($selectedTags.size > 0 && tagScope === 'all'))}
                <span class="acct-badge" title={note.account_id}>{note.account_id.split('@')[0]}</span>
              {/if}
              <span class="note-date">{formatDate(note.date)}</span>
              <span class="note-preview" title={notePreview(note.body_html)}>{notePreview(note.body_html)}</span>
            </div>
            {#if getNoteTags($noteTagsByAccount, note.account_id, note.uuid).length > 0}
              <div class="note-tags">
                {#each getNoteTags($noteTagsByAccount, note.account_id, note.uuid) as tag (tag)}
                  <button
                    type="button"
                    class="note-tag-chip"
                    class:active={$selectedTags.has(tag)}
                    onclick={(e) => { e.stopPropagation(); toggleSelectedTag(tag); }}
                    title="Filter by #{tag}"
                  >#{tag}</button>
                {/each}
              </div>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</div>

{#if menuNote}
  <NoteContextMenu
    x={menuX}
    y={menuY}
    note={menuNote}
    selection={menuSelection}
    onClose={closeContextMenu}
    onLinkSuggestions={handleLinkSuggestions}
  />
{/if}

{#if linkSuggestionsOpen}
  <LinkSuggestionsModal
    accountId={linkSuggestionsAccountId}
    {proposedAppends}
    onClose={() => { linkSuggestionsOpen = false; proposedAppends = []; }}
  />
{/if}

<style>
  .query-status {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .note-list {
    /* width is set inline by the parent (App.svelte) so it can be resized */
    background: var(--surface-list);
    border-right: 1px solid var(--border-subtle);
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }

  .search-bar {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 10px 12px 0;
    padding: 6px 10px;
    background: var(--surface-sunken);
    border-radius: 6px;
  }

  .search-icon {
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .search-scope {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 6px 12px 0;
    font-size: var(--size-xs);
    color: var(--text-muted);
  }
  .scope-label {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .scope-select {
    font-size: var(--size-xs);
    padding: 2px 4px;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: var(--surface-panel);
    color: var(--text-secondary);
  }
  .acct-badge {
    background: var(--surface-sidebar);
    color: var(--accent-action);
    border-radius: 4px;
    padding: 0 5px;
    font-size: var(--size-xs);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .search-input {
    flex: 1;
    background: none;
    border: none;
    outline: none;
    font-size: var(--size-sm);
    line-height: var(--leading);
    color: var(--text);
    font-family: inherit;
    min-width: 0;
  }

  .search-input::placeholder { color: var(--text-muted); }

  .clear-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: var(--size-xs);
    padding: 0 4px;
  }

  .clear-btn:hover { color: var(--text-secondary); }

  .list-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 16px 12px;
    border-bottom: 1px solid var(--border-subtle);
  }

  h2 {
    font-size: var(--size-md);
    font-weight: 600;
    color: var(--text);
    margin: 0;
    min-width: 0;
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .header-actions {
    flex-shrink: 0;
  }

  .header-actions {
    display: flex;
    gap: 4px;
  }

  .icon-btn {
    width: 28px;
    height: 28px;
    border-radius: 6px;
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-muted);
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background 0.15s, color 0.15s;
  }

  .icon-btn:hover:not(:disabled) {
    background: var(--surface-active);
    color: var(--text);
  }

  .icon-btn:disabled {
    cursor: not-allowed;
    opacity: 0.6;
  }

  .spinning {
    animation: spin 0.9s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 8px 0;
    overflow-y: auto;
    flex: 1;
  }

  .note-item {
    border-bottom: 1px solid var(--border-subtle);
  }

  .list-section {
    padding: 10px 16px 4px;
    font-size: 0.75rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-muted);
    /* Thai tone marks sit above the ascender; anything under 1.5 clips them. */
    line-height: 1.5;
  }

  .note-item.active .note-btn {
    background: var(--surface-active);
  }

  /* The 0.08 overlay drops --text-muted to 4.25:1 on this row — under AA, and
     these two are body text, not decoration. They step up one rung on the
     selected row (5.90:1) instead of darkening --text-muted globally: to clear
     AA on the darkest overlay that token would have to go below
     --text-secondary, inverting the muted/secondary ladder everywhere else. */
  .note-item.active .note-date,
  .note-item.active .note-preview {
    color: var(--text-secondary);
  }

  /* Multi-select: amber tint + left rail. Subtle so the "active" (primary
     editor focus) row is still distinguishable when both states overlap. */
  .note-item.multi-selected .note-btn {
    background: var(--accent-wash);
    box-shadow: inset 3px 0 0 var(--accent-rail);
  }
  .note-item.multi-selected.active .note-btn {
    background: var(--accent-wash-strong);
  }

  .note-btn {
    width: 100%;
    padding: 10px 16px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    display: block;
    transition: background 0.15s;
  }

  .note-btn:hover {
    background: var(--surface-sunken);
  }

  .note-title {
    font-size: var(--size);
    font-weight: 600;
    color: var(--text);
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin-bottom: 3px;
  }

  /* Small leading pushpin for pinned notes. Sized down so it doesn't crowd
     the title; aligned visually with cap-height by nudging baseline. */
  .pin-indicator {
    display: inline-flex;
    align-items: center;
    margin-right: 4px;
    vertical-align: middle;
  }

  /* Same slot as the pushpin, opposite meaning: this note is NOT anywhere but
     here. Text glyph rather than an Icon because the icon set has no
     "unsynced" and inventing one is a bigger change than this warrants. */
  .unsynced-indicator {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    height: 14px;
    margin-right: 4px;
    border-radius: 50%;
    background: var(--danger-wash);
    color: var(--danger);
    font-size: var(--size-xs);
    font-weight: 700;
    vertical-align: middle;
  }

  .note-meta {
    display: flex;
    gap: 8px;
    align-items: baseline;
  }

  .note-date {
    font-size: var(--size-xs);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .note-preview {
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .note-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 4px;
  }

  .note-tag-chip {
    font-size: var(--size-xs);
    line-height: var(--leading);
    color: var(--text-secondary);
    background: var(--surface-hover);
    border: none;
    padding: 1px 7px;
    border-radius: 9px;
    cursor: pointer;
  }

  .note-tag-chip:hover {
    background: var(--surface-active);
  }

  /* --accent-tint, not --accent: this chip carries text, and the plain accent
     is reserved for fills with nothing on top. Also makes the active tag chip
     here match .tag-pill.active in the sidebar, which was already tint+on-tint. */
  .note-tag-chip.active {
    background: var(--accent-tint);
    color: var(--accent-on-tint);
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    flex: 1;
    color: var(--text-muted);
    font-size: var(--size);
    gap: 8px;
  }

  .new-note-prompt {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: var(--size);
    cursor: pointer;
    text-decoration: underline;
  }

  .spinner {
    width: 20px;
    height: 20px;
    border: 2px solid var(--border-subtle);
    border-top-color: var(--text-muted);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  /* App.svelte's prefers-reduced-motion block clamps every animation to
     0.01ms, which would freeze this ring mid-rotation with one arbitrary dark
     arc — reading as a broken shape, not a busy indicator. Even out the border
     so it degrades to a deliberate static ring; the adjacent "Loading notes…"
     text is what actually carries the busy state in this mode. */
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      border-color: var(--border-strong);
    }
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
