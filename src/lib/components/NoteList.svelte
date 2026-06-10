<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { notes, selectedFolder, selectedNote, isLoading, refreshNotes, currentAccount, selectedUuids, clearSelectedUuids, selectedTags, tagMatchMode, toggleSelectedTag, noteTagsByAccount, getNoteTags } from '../stores/notes';
  import type { Note } from '../types';
  import NoteContextMenu from './NoteContextMenu.svelte';

  // Width is owned by App.svelte so the divider between this pane and the
  // sidebar can be dragged. Default matches the pre-resizer 240px look.
  export let width: number = 240;

  // Cross-note search query. When non-empty, the filtered list switches
  // from "notes in current folder" to "notes matching query across all folders" —
  // mirrors how Apple Notes' search behaves.
  let searchQuery = '';

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
    if (sel.size > 1 && sel.has(note.uuid)) {
      menuSelection = filteredNotes.filter((n) => sel.has(n.uuid));
    } else {
      menuSelection = [];
    }
  }

  function closeContextMenu() {
    menuNote = null;
    menuSelection = [];
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
  $: headerName = $selectedFolder === '__ALL__'
    ? `All ${$currentAccount ?? ''}`
    : ($selectedFolder?.split('/').pop() || $selectedFolder);

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
      const all = new Set(filteredNotes.map((n) => n.uuid));
      selectedUuids.set(all);
      return;
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    if (!$selectedNote) return;
    const idx = filteredNotes.findIndex(n => n.uuid === $selectedNote!.uuid);
    if (idx < 0) return;
    e.preventDefault();
    const nextIdx = e.key === 'ArrowDown'
      ? Math.min(idx + 1, filteredNotes.length - 1)
      : Math.max(idx - 1, 0);
    if (nextIdx !== idx) {
      clearSelectedUuids();
      selectedNote.set(filteredNotes[nextIdx]);
      selectionAnchor = filteredNotes[nextIdx].uuid;
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
  $: filteredNotes = (() => {
    const q = searchQuery.trim().toLowerCase();
    const inAccount = (n: Note) => n.account_id === $currentAccount;
    let base: Note[];
    if (!q) {
      if ($selectedTags.size > 0) {
        // Tag view takes precedence over the folder selection. AND = note has
        // every selected tag; OR = note has any. App.paintTagsFromCache has
        // already loaded the union of these tags' notes into $notes.
        const sel = [...$selectedTags];
        const mode = $tagMatchMode;
        base = $notes.filter((n) => {
          if (!inAccount(n)) return false;
          const tags = getNoteTags($noteTagsByAccount, n.account_id, n.uuid);
          return mode === 'AND'
            ? sel.every((t) => tags.includes(t))
            : sel.some((t) => tags.includes(t));
        });
      } else {
        base = $selectedFolder === '__ALL__'
          ? $notes.filter(inAccount)
          : $notes.filter((n) => inAccount(n) && n.label === $selectedFolder);
      }
    } else {
      base = $notes.filter((n) => {
        if (!inAccount(n)) return false;
        if (n.title.toLowerCase().includes(q)) return true;
        const plain = n.body_html.replace(/<[^>]*>/g, ' ').toLowerCase();
        return plain.includes(q);
      });
    }
    // Pinned-first sort. Date is RFC 2822 or ISO; Date.parse handles both
    // and returns NaN-safe when malformed (treated as 0, sinks to the
    // bottom of its group rather than crashing the sort). Stable sort
    // semantics keep within-group order consistent with the source list,
    // which matters for the "multiple pinned notes keep their relative
    // date order" check in the verification step.
    return base.slice().sort((a, b) => {
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
  function onNoteClick(e: MouseEvent, note: Note) {
    const cmd = e.metaKey || e.ctrlKey;
    const shift = e.shiftKey;

    if (cmd) {
      selectedUuids.update((s) => {
        const next = new Set(s);
        if (next.has(note.uuid)) next.delete(note.uuid);
        else next.add(note.uuid);
        return next;
      });
      selectedNote.set(note);
      selectionAnchor = note.uuid;
      return;
    }

    if (shift && selectionAnchor) {
      const list = filteredNotes;
      const a = list.findIndex((n) => n.uuid === selectionAnchor);
      const b = list.findIndex((n) => n.uuid === note.uuid);
      if (a >= 0 && b >= 0) {
        const [lo, hi] = a <= b ? [a, b] : [b, a];
        const range = new Set<string>();
        for (let i = lo; i <= hi; i++) range.add(list[i].uuid);
        selectedUuids.set(range);
        selectedNote.set(note);
        return;
      }
    }

    // Plain click: clear multi, single-select.
    clearSelectedUuids();
    selectedNote.set(note);
    selectionAnchor = note.uuid;
  }

  // Legacy helper for keyboard activation paths.
  function selectNote(note: Note) {
    clearSelectedUuids();
    selectedNote.set(note);
    selectionAnchor = note.uuid;
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

  function stripHtml(html: string): string {
    return html.replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim().slice(0, 80);
  }

  function newNote() {
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
      bind:value={searchQuery}
      placeholder="Search all notes..."
    />
    {#if searchQuery}
      <button class="clear-btn" onclick={() => (searchQuery = '')} aria-label="Clear search">✕</button>
    {/if}
  </div>
  <div class="list-header">
    <h2 title={$selectedFolder}>{searchQuery ? `Results: ${filteredNotes.length}` : headerName}</h2>
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
      <button class="icon-btn" onclick={newNote} title="New Note" aria-label="New note">
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
          <path d="M8 3v10M3 8h10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
        </svg>
      </button>
    </div>
  </div>

  <!-- Only show the full-pane spinner on first load (no notes in store yet).
       Background refreshes (focus/poll/activity) keep the current list visible
       — the ⟳ button's spinning icon already signals "refresh in progress". -->
  {#if $isLoading && $notes.length === 0}
    <div class="empty-state">
      <div class="spinner"></div>
      <p>Loading notes...</p>
    </div>
  {:else if filteredNotes.length === 0}
    <div class="empty-state">
      <p>No notes in this folder</p>
      <button class="new-note-prompt" onclick={newNote}>Create one</button>
    </div>
  {:else}
    <ul>
      {#each filteredNotes as note (note.uuid)}
        <li
          class="note-item"
          class:active={$selectedNote?.uuid === note.uuid}
          class:multi-selected={$selectedUuids.has(note.uuid)}
        >
          <div
            class="note-btn"
            role="button"
            tabindex="0"
            onclick={(e) => onNoteClick(e, note)}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectNote(note); } }}
            oncontextmenu={(e) => openContextMenu(e, note)}
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
            <div class="note-title">
              {#if note.pinned}<span class="pin-indicator" aria-label="Pinned" title="Pinned">📌</span>{/if}
              {note.title || 'Untitled'}
            </div>
            <div class="note-meta">
              <span class="note-date">{formatDate(note.date)}</span>
              <span class="note-preview">{stripHtml(note.body_html)}</span>
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
  />
{/if}

<style>
  .note-list {
    /* width is set inline by the parent (App.svelte) so it can be resized */
    background: #faf6ed;
    border-right: 1px solid #e8e2d4;
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
    background: rgba(0, 0, 0, 0.04);
    border-radius: 6px;
  }

  .search-icon {
    color: #999;
    flex-shrink: 0;
  }

  .search-input {
    flex: 1;
    background: none;
    border: none;
    outline: none;
    font-size: 12px;
    color: #222;
    font-family: inherit;
    min-width: 0;
  }

  .search-input::placeholder { color: #aaa; }

  .clear-btn {
    background: none;
    border: none;
    color: #999;
    cursor: pointer;
    font-size: 11px;
    padding: 0 4px;
  }

  .clear-btn:hover { color: #555; }

  .list-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 16px 12px;
    border-bottom: 1px solid #e8e2d4;
  }

  h2 {
    font-size: 15px;
    font-weight: 600;
    color: #333;
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
    color: #888;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background 0.15s, color 0.15s;
  }

  .icon-btn:hover:not(:disabled) {
    background: rgba(0,0,0,0.08);
    color: #333;
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
    border-bottom: 1px solid rgba(0,0,0,0.04);
  }

  .note-item.active .note-btn {
    background: rgba(0,0,0,0.08);
  }

  /* Multi-select: amber tint + left rail. Subtle so the "active" (primary
     editor focus) row is still distinguishable when both states overlap. */
  .note-item.multi-selected .note-btn {
    background: rgba(201, 124, 31, 0.10);
    box-shadow: inset 3px 0 0 rgba(201, 124, 31, 0.55);
  }
  .note-item.multi-selected.active .note-btn {
    background: rgba(201, 124, 31, 0.18);
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
    background: rgba(0,0,0,0.04);
  }

  .note-title {
    font-size: 13px;
    font-weight: 600;
    color: #222;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin-bottom: 3px;
  }

  /* Small leading pushpin for pinned notes. Sized down so it doesn't crowd
     the title; aligned visually with cap-height by nudging baseline. */
  .pin-indicator {
    font-size: 10px;
    margin-right: 4px;
    vertical-align: 1px;
  }

  .note-meta {
    display: flex;
    gap: 8px;
    align-items: baseline;
  }

  .note-date {
    font-size: 11px;
    color: #aaa;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .note-preview {
    font-size: 11px;
    color: #999;
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
    font-size: 10px;
    line-height: 1.4;
    color: #6b6150;
    background: rgba(0, 0, 0, 0.06);
    border: none;
    padding: 1px 7px;
    border-radius: 9px;
    cursor: pointer;
  }

  .note-tag-chip:hover {
    background: rgba(0, 0, 0, 0.12);
  }

  .note-tag-chip.active {
    background: #d8b25e;
    color: #3a2f12;
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    flex: 1;
    color: #aaa;
    font-size: 13px;
    gap: 8px;
  }

  .new-note-prompt {
    background: none;
    border: none;
    color: #888;
    font-size: 13px;
    cursor: pointer;
    text-decoration: underline;
  }

  .spinner {
    width: 20px;
    height: 20px;
    border: 2px solid #e8e2d4;
    border-top-color: #888;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
