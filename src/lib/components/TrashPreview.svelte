<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { currentAccount, selectedTrashedNote } from '../stores/notes';
  import type { Note } from '../types';

  let loading = false;
  let errorMsg = '';
  let content: Note | null = null;
  // Tracks which (account, id) the current fetch is for, so a slow fetch
  // that resolves after the user clicked a different row doesn't clobber it.
  let loadedFor = '';

  function extractBody(html: string): string {
    const match = html.match(/<body[^>]*>([\s\S]*)<\/body>/i);
    return match ? match[1] : html;
  }

  async function loadPreview(accountId: string, id: string) {
    const key = `${accountId}::${id}`;
    loadedFor = key;
    loading = true;
    errorMsg = '';
    content = null;
    try {
      const note = await invoke<Note>('get_trashed_note_preview', { accountId, id });
      if (loadedFor === key) content = note;
    } catch (e) {
      if (loadedFor === key) errorMsg = String(e);
    } finally {
      if (loadedFor === key) loading = false;
    }
  }

  $: if ($selectedTrashedNote && $currentAccount) {
    void loadPreview($currentAccount, $selectedTrashedNote.id);
  } else {
    loadedFor = '';
    content = null;
    errorMsg = '';
    loading = false;
  }

  function fmtDate(s: string): string {
    const d = new Date(s);
    return isNaN(d.getTime()) ? s : d.toLocaleString();
  }
</script>

<div class="editor">
  {#if !$selectedTrashedNote}
    <div class="placeholder">Select a note or create a new one</div>
  {:else}
    <div class="banner">🗑️ In Recently Deleted — read-only. Right-click the note in the list to restore it.</div>
    {#if loading}
      <div class="placeholder"><div class="spinner"></div></div>
    {:else if errorMsg}
      <div class="placeholder err">{errorMsg}</div>
    {:else if content}
      <div class="title">{content.title || 'Untitled'}</div>
      <div class="meta">{fmtDate(content.date)} · {$selectedTrashedNote.label}</div>
      <!-- Read-only render — same body_html trust boundary as NoteEditor's -->
      <!-- innerHTML assignment; this content is never fed back into save. -->
      <div class="body">{@html extractBody(content.body_html)}</div>
    {/if}
  {/if}
</div>

<style>
  .editor {
    flex: 1;
    height: 100vh;
    overflow-y: auto;
    background: #fff;
    display: flex;
    flex-direction: column;
  }
  .placeholder {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #aaa;
    font-size: 14px;
  }
  .placeholder.err {
    color: #c0392b;
    padding: 24px;
    text-align: center;
  }
  .spinner {
    width: 22px;
    height: 22px;
    border: 2px solid #e0d8c8;
    border-top-color: #b08a4a;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .banner {
    background: #fdf3e2;
    color: #8a6a2f;
    font-size: 12px;
    padding: 8px 24px;
    border-bottom: 1px solid #f0e0bd;
  }
  .title {
    font-size: 24px;
    font-weight: 600;
    color: #2a2a2a;
    padding: 20px 24px 4px;
  }
  .meta {
    font-size: 12px;
    color: #999;
    padding: 0 24px 16px;
  }
  .body {
    padding: 0 24px 40px;
    font-size: 14px;
    line-height: 1.6;
    color: #333;
    word-wrap: break-word;
  }
  .body :global(a) {
    color: #4a7cc9;
  }
  .body :global(img) {
    max-width: 100%;
  }
</style>
