<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { get } from 'svelte/store';
  import { currentAccount, refreshNotes, selectedTrashedNote } from '../stores/notes';
  import type { TrashedNote } from '../types';

  export let width: number;

  let items: TrashedNote[] = [];
  let folders: string[] = [];
  let loading = false;
  let errorMsg = '';
  let loadedAccount = '';
  let busy = new Set<string>();

  // Right-click context menu state.
  let menuOpen = false;
  let menuX = 0;
  let menuY = 0;
  let menuItem: TrashedNote | null = null;

  async function load() {
    const acct = $currentAccount;
    // Switching accounts invalidates any open preview — it belongs to the
    // previous account's mailbox and get_trashed_note_preview would fetch
    // against the wrong one.
    selectedTrashedNote.set(null);
    if (!acct) {
      items = [];
      folders = [];
      return;
    }
    loading = true;
    errorMsg = '';
    try {
      const [tn, fl] = await Promise.all([
        invoke<TrashedNote[]>('list_trashed_notes', { accountId: acct }),
        invoke<string[]>('list_folders', { accountId: acct }).catch(() => [] as string[]),
      ]);
      items = tn;
      folders = fl;
    } catch (e) {
      errorMsg = String(e);
      items = [];
    } finally {
      loading = false;
    }
  }

  // Load on entry / account change, guarded so it can't loop.
  $: if ($currentAccount && $currentAccount !== loadedAccount) {
    loadedAccount = $currentAccount;
    load();
  }

  function selectItem(t: TrashedNote) {
    selectedTrashedNote.set(t);
  }

  function openMenu(e: MouseEvent, t: TrashedNote) {
    e.preventDefault();
    menuItem = t;
    menuX = e.clientX;
    menuY = e.clientY;
    menuOpen = true;
  }
  function closeMenu() {
    menuOpen = false;
    menuItem = null;
  }

  async function restore(t: TrashedNote, target: string) {
    closeMenu();
    if (busy.has(t.id)) return;
    busy = new Set(busy).add(t.id);
    try {
      await invoke('restore_note', {
        accountId: $currentAccount,
        uuid: t.uuid,
        id: t.id,
        originalLabel: t.label,
        targetLabel: target,
      });
      items = items.filter((x) => x.id !== t.id);
      if (get(selectedTrashedNote)?.id === t.id) selectedTrashedNote.set(null);
      // Refresh the main note list so the restored note reappears there
      // immediately (critical for LocalFS which has no background poll).
      void Promise.resolve(get(refreshNotes)());
    } catch (e) {
      alert(`กู้คืนไม่สำเร็จ: ${e}`);
    } finally {
      const n = new Set(busy);
      n.delete(t.id);
      busy = n;
    }
  }

  function fmtDate(s: string): string {
    const d = new Date(s);
    return isNaN(d.getTime())
      ? s
      : d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
  }

  function onWindowClick() {
    if (menuOpen) closeMenu();
  }
  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') closeMenu();
  }
</script>

<svelte:window on:click={onWindowClick} on:keydown={onKey} />

<div class="note-list" style="width: {width}px; min-width: {width}px;">
  <div class="list-header">
    <h2>🗑️ Recently Deleted</h2>
    <button class="icon-btn" onclick={load} disabled={loading} title="Refresh" aria-label="Refresh">
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" class:spinning={loading}>
        <path d="M13.5 8a5.5 5.5 0 1 1-1.61-3.89L13.5 5.5M13.5 2v3.5H10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
  </div>
  <p class="hint">เก็บ ~30 วันแล้ว Gmail ล้างเอง • <b>คลิกขวา</b>เพื่อกู้คืน</p>

  {#if loading && items.length === 0}
    <div class="empty-state"><div class="spinner"></div><p>Loading…</p></div>
  {:else if errorMsg}
    <div class="empty-state"><p class="err">{errorMsg}</p><button class="retry" onclick={load}>Retry</button></div>
  {:else if items.length === 0}
    <div class="empty-state"><p>ไม่มีโน้ตที่ลบเร็วๆ นี้</p></div>
  {:else}
    <ul>
      {#each items as t (t.id)}
        <li class="note-item" class:busy={busy.has(t.id)}>
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="note-btn"
            class:active={$selectedTrashedNote?.id === t.id}
            role="button"
            tabindex="0"
            onclick={() => selectItem(t)}
            oncontextmenu={(e) => openMenu(e, t)}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') selectItem(t); }}
          >
            <div class="note-title">{t.title || 'Untitled'}</div>
            <div class="note-meta">
              <span class="note-date">{fmtDate(t.date)}</span>
              <span class="note-preview">{t.label}</span>
            </div>
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</div>

{#if menuOpen && menuItem}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="context-menu" style="left: {menuX}px; top: {menuY}px" role="menu" onclick={(e) => e.stopPropagation()}>
    <button class="item" onclick={() => restore(menuItem!, menuItem!.label)}>
      <span class="icon">↩︎</span><span class="label">กู้คืน</span>
    </button>
    {#if folders.length > 0}
      <div class="submenu-anchor">
        <button class="item has-submenu" type="button">
          <span class="icon">📁</span><span class="label">กู้คืนไปที่</span><span class="chevron">▸</span>
        </button>
        <div class="submenu">
          {#each folders as f (f)}
            <button class="item sub" onclick={() => restore(menuItem!, f)}>
              <span class="label">{f}</span>
            </button>
          {/each}
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .note-list {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: #faf8f3;
    border-right: 1px solid #e8e3d8;
    overflow-y: auto;
  }
  .list-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 16px 6px;
  }
  .list-header h2 {
    font-size: 15px;
    font-weight: 600;
    color: #3a3a3a;
  }
  .icon-btn {
    background: none;
    border: none;
    color: #888;
    cursor: pointer;
    padding: 4px;
    border-radius: 6px;
    display: flex;
  }
  .icon-btn:hover:not(:disabled) {
    background: #efe9dd;
    color: #444;
  }
  .icon-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .spinning {
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .hint {
    font-size: 11px;
    color: #aaa;
    padding: 0 16px 8px;
  }
  ul {
    list-style: none;
    flex: 1;
  }
  .note-item {
    border-bottom: 1px solid #efe9dd;
  }
  .note-item.busy {
    opacity: 0.5;
  }
  .note-btn {
    padding: 10px 16px;
    cursor: context-menu;
  }
  .note-btn:hover {
    background: #f3eee2;
  }
  .note-btn.active {
    background: #ece4d2;
  }
  .note-title {
    font-size: 14px;
    font-weight: 500;
    color: #333;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .note-meta {
    display: flex;
    gap: 8px;
    font-size: 11px;
    color: #999;
    margin-top: 3px;
  }
  .note-preview {
    color: #b08a4a;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .empty-state {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    color: #aaa;
    font-size: 13px;
    padding: 40px 16px;
  }
  .err {
    color: #c0392b;
    text-align: center;
    word-break: break-word;
  }
  .retry {
    background: #efe9dd;
    border: none;
    padding: 6px 14px;
    border-radius: 6px;
    cursor: pointer;
    color: #555;
  }
  .spinner {
    width: 22px;
    height: 22px;
    border: 2px solid #e0d8c8;
    border-top-color: #b08a4a;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  /* Context menu — mirrors NoteContextMenu look. */
  .context-menu {
    position: fixed;
    z-index: 1000;
    min-width: 180px;
    background: #fff;
    border: 1px solid #e0d8c8;
    border-radius: 8px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.15);
    padding: 4px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    background: none;
    border: none;
    text-align: left;
    padding: 7px 10px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 13px;
    color: #333;
  }
  .item:hover {
    background: #f3eee2;
  }
  .item .icon {
    width: 16px;
    text-align: center;
  }
  .item .label {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .item .chevron {
    color: #aaa;
  }
  .submenu-anchor {
    position: relative;
  }
  .submenu {
    display: none;
    position: absolute;
    left: 100%;
    top: 0;
    min-width: 180px;
    max-height: 60vh;
    overflow-y: auto;
    background: #fff;
    border: 1px solid #e0d8c8;
    border-radius: 8px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.15);
    padding: 4px;
  }
  .submenu-anchor:hover .submenu {
    display: block;
  }
  .item.sub {
    font-size: 12px;
  }
</style>
