<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { get } from 'svelte/store';
  import {
    currentAccount,
    notes,
    selectedNote,
    selectedFolder,
    setAccountNoteTags,
    indexUpsertOnSave,
  } from '../stores/notes';
  import type { Note } from '../types';

  // Bindable so the parent can both open and observe close.
  let { open = $bindable(false) }: { open: boolean } = $props();

  let sourceText = $state('');
  let titleOverride = $state('');
  let busy = $state(false);
  let errorMsg = $state('');
  // request_id for the in-flight extract — generated per extraction so the
  // Cancel button can pass it to cancel_extraction. Empty when no extract
  // is running.
  let requestId = $state('');

  // RFC4122 v4 UUID generator (no external dep). The backend doesn't validate
  // the format — it's just used as a HashMap key — so any unique-per-call
  // string works. crypto.randomUUID is available in modern browsers / Tauri.
  function newRequestId(): string {
    return globalThis.crypto?.randomUUID?.() ?? `req-${Date.now()}-${Math.random()}`;
  }

  async function extract() {
    const acct = get(currentAccount);
    if (!acct) {
      errorMsg = 'No account selected.';
      return;
    }
    if (!sourceText.trim()) {
      errorMsg = 'Paste some source text first.';
      return;
    }
    busy = true;
    errorMsg = '';
    requestId = newRequestId();
    try {
      // Backend command takes snake_case fields in Rust; Tauri's serde
      // bridge expects camelCase on the JS side (matches every other
      // invoke in this codebase — see AccountSettings, NoteContextMenu).
      const newUuid = await invoke<string>('extract_lessons', {
        accountId: acct,
        sourceText,
        titleOverride: titleOverride.trim() || null,
        requestId,
      });

      // Close + reset before navigation so the modal doesn't flash
      // while we repaint.
      open = false;
      sourceText = '';
      titleOverride = '';

      // Navigate to Notes/__Extracts__ (sidebar displays it as just
      // "Extracts" after stripping the workflow markers). The backend already wrote the row
      // synchronously to SQLite; setting selectedFolder triggers App.svelte's
      // paintFolderFromCache watcher (doctrine: cache-only, no Gmail touch
      // on a normal navigation/write path). Then merge the new note row
      // explicitly so we can select it without racing the watcher.
      selectedFolder.set('Notes/__Extracts__');
      try {
        const cached = await invoke<Note[]>('list_cached_notes_in_folder', {
          accountId: acct,
          path: 'Notes/__Extracts__',
        });
        notes.update((ns) => {
          const others = ns.filter(
            (n) => !(n.account_id === acct && n.label === 'Notes/__Extracts__'),
          );
          return [...others, ...cached];
        });
      } catch (e) {
        console.warn('post-extract cache paint failed', e);
      }
      // Refresh the tag store so the LLM-suggested tags appear in the
      // sidebar tag cloud and editor chip row without waiting for the next
      // app startup or unrelated tag change.
      try {
        const rows = await invoke<{ uuid: string; tag: string }[]>(
          'list_note_tags',
          { accountId: acct },
        );
        setAccountNoteTags(acct, rows);
      } catch (e) {
        console.warn('post-extract tag refresh failed', e);
      }
      const found = get(notes).find((n: Note) => n.uuid === newUuid);
      if (found) {
        selectedNote.set(found);
        // Bump $noteIndex via indexUpsertOnSave. Sidebar's reactive
        // refreshFolders block fires on $noteIndex changes, which is how it
        // discovers the newly-created Notes/__Extracts__ workflow folder AND
        // refetches its kind (so the sidebar moves it under the "Workflows"
        // group with the 💡 icon). Without this bump, the folder appears as
        // a regular user folder under Notes until the next unrelated note
        // save or app restart.
        indexUpsertOnSave(acct, null, found.id, found.label);
      }
    } catch (e) {
      const msg = String(e);
      if (msg === 'cancelled' || msg.endsWith(': cancelled')) {
        // Cancel landed before any DB write — source still in textarea,
        // user can retry or paste new content. No "error" UI state needed.
        errorMsg = '';
      } else {
        // Preserve sourceText so the user can retry without re-pasting.
        errorMsg = msg;
      }
    } finally {
      busy = false;
      requestId = '';
    }
  }

  // Cancel the in-flight extraction, if any. Best-effort: a stale request_id
  // (extract already completed) just returns Ok(false) on the backend.
  async function cancelInFlight() {
    if (!busy || !requestId) return;
    try {
      await invoke('cancel_extraction', { requestId });
    } catch (e) {
      console.warn('cancel_extraction invoke failed', e);
    }
  }

  function close() {
    if (busy) {
      // User clicked Cancel while extraction is running — propagate to the
      // backend, then close. The extract() finally block clears state.
      void cancelInFlight();
      return;
    }
    open = false;
    sourceText = '';
    titleOverride = '';
    errorMsg = '';
  }

  function onKey(e: KeyboardEvent) {
    if (!open) return;
    if (e.key === 'Escape') close();
  }
</script>

<svelte:window on:keydown={onKey} />

{#if open}
  <div
    class="backdrop"
    role="presentation"
    onclick={(e) => { if (e.target === e.currentTarget) close(); }}
  >
    <div class="modal" role="dialog" aria-modal="true" aria-labelledby="extract-title">
      <h2 id="extract-title">Extract</h2>
      <p class="hint">
        Paste source text from a conversation, transcript, article, or other source.
        Jodd will distill it into structured key points and file the result in Notes/Extracts.
      </p>

      <label>
        Source text
        <textarea
          bind:value={sourceText}
          rows="15"
          disabled={busy}
          placeholder="Paste here..."
        ></textarea>
      </label>

      <label>
        Title (optional)
        <input
          type="text"
          bind:value={titleOverride}
          disabled={busy}
          placeholder="Auto-derived from first lesson"
        />
      </label>

      {#if errorMsg}
        <div class="error">{errorMsg}</div>
      {/if}

      <div class="actions">
        <!-- Cancel stays enabled during extraction — clicking it now
             propagates to cancel_extraction(requestId), kills the in-flight
             provider call, and closes the modal once unwinding completes. -->
        <button onclick={close}>
          {busy ? 'Cancel extraction' : 'Cancel'}
        </button>
        <button
          onclick={extract}
          disabled={busy || !sourceText.trim()}
          class="primary"
        >
          {busy ? 'Extracting…' : 'Extract'}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed; inset: 0;
    background: rgba(0, 0, 0, 0.3);
    display: flex; align-items: center; justify-content: center;
    z-index: 1000;
  }
  .modal {
    background: #fffef9;
    width: 600px; max-width: 90vw; max-height: 85vh; overflow-y: auto;
    padding: 24px; border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.2);
  }
  h2 { margin: 0 0 8px; }
  .hint { color: #666; font-size: 13px; margin: 0 0 16px; }
  label { display: block; margin: 12px 0; font-size: 13px; color: #555; }
  textarea, input {
    width: 100%; padding: 8px; font: inherit; box-sizing: border-box;
    margin-top: 4px;
  }
  textarea { font-family: monospace; font-size: 12px; }
  .error {
    color: #c33; padding: 8px; background: #fee; border-radius: 4px;
    margin: 12px 0; white-space: pre-wrap;
  }
  .actions {
    display: flex; gap: 8px; justify-content: flex-end; margin-top: 16px;
  }
  .actions button.primary {
    background: #2563eb; color: white; border: none;
    padding: 8px 16px; border-radius: 4px; cursor: pointer;
  }
  .actions button.primary:disabled { background: #ccc; }
</style>
