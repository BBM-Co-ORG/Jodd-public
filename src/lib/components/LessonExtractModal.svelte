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
  import LinkSuggestionsModal from './LinkSuggestionsModal.svelte';

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

  // Populated by check_duplicate_citations when the pasted source's URL(s)
  // already exist as a citation elsewhere in this account. Non-empty → the
  // modal shows a soft warning with a "Continue anyway" resubmit instead of
  // calling the LLM. Never a hard block (see design spec decision 2).
  let duplicateWarnings = $state<
    { url: string; existing_note_uuid: string; existing_note_title: string }[]
  >([]);

  // Populated after a successful extract/append by suggest_wiki_links.
  // Non-empty proposedAppends opens LinkSuggestionsModal for confirmation —
  // auto_links have ALREADY been inserted into the note by this point
  // (design spec decision 5: automatic, no confirmation needed for the
  // note's own outgoing links).
  let linkSuggestionsOpen = $state(false);
  let proposedAppends = $state<{ uuid: string; title: string; addition_text: string }[]>([]);

  // Runs after extract_note/append_extract_note succeeds (design
  // spec decision 3: automatic, both paths). Failure here is a SOFT
  // failure — logs and returns silently, never surfaces an error to the
  // user or undoes the primary save.
  async function runAutoLinkSuggestions(
    acct: string,
    savedUuid: string,
    savedTitle: string,
    savedBody: string,
  ) {
    try {
      const res = await invoke<{
        auto_links: { uuid: string; title: string; slug: string }[];
        proposed_appends: { uuid: string; title: string; addition_text: string }[];
      }>('suggest_wiki_links', {
        accountId: acct,
        text: savedBody,
        excludeUuid: savedUuid,
        newNoteTitle: savedTitle,
        newNoteUuid: savedUuid,
        requestId: newRequestId(),
      });

      if (res.auto_links.length > 0) {
        const linksLine = `<p>Related: ${res.auto_links
          .map((l) => `[[${l.slug}]]`)
          .join(', ')}</p>`;
        const current = get(notes).find((n: Note) => n.uuid === savedUuid);
        const bodyToExtend = current?.body_html ?? savedBody;
        // save_note's real signature (lib.rs:1168-1180) requires
        // existingGmailId/existingUuid/existingXMailCreatedDate — passing
        // `uuid` (not a real param) and omitting the rest would silently
        // create a SECOND note instead of editing this one, since a missing
        // existingUuid makes the backend treat it as brand-new (lib.rs:1186-1191).
        await invoke('save_note', {
          accountId: acct,
          title: savedTitle,
          bodyHtml: `${bodyToExtend}${linksLine}`,
          existingGmailId: current?.id ?? null,
          existingUuid: savedUuid,
          existingXMailCreatedDate: current?.x_mail_created_date ?? null,
          label: current?.label ?? '',
        });
      }

      if (res.proposed_appends.length > 0) {
        proposedAppends = res.proposed_appends;
        linkSuggestionsOpen = true;
      }
    } catch (e) {
      console.warn('suggest_wiki_links failed — skipping auto-link suggestions', e);
    }
  }

  // Destination: 'new' creates a fresh note in Notes/__Extracts__ (today's
  // behavior); 'existing' appends into a note the user picks below.
  type Destination = 'new' | 'existing';
  let destination = $state<Destination>('new');

  let targetQuery = $state('');
  let targetResults = $state<Note[]>([]);
  let targetNote = $state<Note | null>(null);
  let targetSearchTimer: ReturnType<typeof setTimeout> | undefined;
  let targetSearchSeq = 0;

  // Source: 'paste' is today's behavior (paste raw text). 'existing' feeds
  // an already-saved note's body through the same pipeline as pasted text —
  // the source note itself is never modified (design spec #2a).
  type SourceMode = 'paste' | 'existing';
  let sourceMode = $state<SourceMode>('paste');

  let sourceNoteQuery = $state('');
  let sourceNoteResults = $state<Note[]>([]);
  let sourceNote = $state<Note | null>(null);
  let sourceNoteSearchTimer: ReturnType<typeof setTimeout> | undefined;
  let sourceNoteSearchSeq = 0;

  // Debounced, sequence-guarded search — mirrors NoteList.svelte's
  // scheduleSearch (same 150ms delay, same stale-response guard).
  function scheduleTargetSearch(query: string) {
    clearTimeout(targetSearchTimer);
    const q = query.trim();
    if (!q) {
      targetResults = [];
      return;
    }
    const acct = get(currentAccount);
    if (!acct) {
      targetResults = [];
      return;
    }
    targetSearchTimer = setTimeout(async () => {
      const seq = ++targetSearchSeq;
      try {
        const res = await invoke<Note[]>('search_notes', {
          accountId: acct,
          label: null,
          query: q,
        });
        if (seq === targetSearchSeq) targetResults = res;
      } catch (e) {
        console.warn('target note search failed', e);
      }
    }, 150);
  }

  function pickTarget(note: Note) {
    targetNote = note;
    targetResults = [];
    targetQuery = note.title;
  }

  function setDestination(next: Destination) {
    destination = next;
    if (next === 'new') {
      targetNote = null;
      targetQuery = '';
      targetResults = [];
    }
  }

  function scheduleSourceNoteSearch(query: string) {
    clearTimeout(sourceNoteSearchTimer);
    const q = query.trim();
    if (!q) {
      sourceNoteResults = [];
      return;
    }
    const acct = get(currentAccount);
    if (!acct) {
      sourceNoteResults = [];
      return;
    }
    sourceNoteSearchTimer = setTimeout(async () => {
      const seq = ++sourceNoteSearchSeq;
      try {
        const res = await invoke<Note[]>('search_notes', {
          accountId: acct,
          label: null,
          query: q,
        });
        if (seq === sourceNoteSearchSeq) sourceNoteResults = res;
      } catch (e) {
        console.warn('source note search failed', e);
      }
    }, 150);
  }

  function pickSourceNote(note: Note) {
    sourceNote = note;
    sourceNoteResults = [];
    sourceNoteQuery = note.title;
    // Feed the picked note's body as source_text, matching pasted-text
    // shape (HTML tags stripped isn't needed here — the LLM already
    // handles HTML-ish pasted content today via the same field).
    sourceText = note.body_html;
  }

  function setSourceMode(next: SourceMode) {
    sourceMode = next;
    if (next === 'paste') {
      sourceNote = null;
      sourceNoteQuery = '';
      sourceNoteResults = [];
      sourceText = '';
    }
  }

  // Resets every piece of form state, including the append-picker fields.
  // The modal component instance persists across open/close cycles, so
  // without this a user who switches to "Append to existing note", picks a
  // target, then cancels would re-open the modal still in append mode with
  // the stale picked target shown.
  function resetForm() {
    sourceText = '';
    titleOverride = '';
    errorMsg = '';
    destination = 'new';
    targetNote = null;
    targetQuery = '';
    targetResults = [];
    sourceMode = 'paste';
    sourceNote = null;
    sourceNoteQuery = '';
    sourceNoteResults = [];
  }

  // RFC4122 v4 UUID generator (no external dep). The backend doesn't validate
  // the format — it's just used as a HashMap key — so any unique-per-call
  // string works. crypto.randomUUID is available in modern browsers / Tauri.
  function newRequestId(): string {
    return globalThis.crypto?.randomUUID?.() ?? `req-${Date.now()}-${Math.random()}`;
  }

  async function extract(skipDuplicateCheck = false) {
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
    duplicateWarnings = [];
    requestId = newRequestId();
    if (destination === 'existing' && !targetNote) {
      errorMsg = 'Pick a note to append to, or switch to "New note".';
      busy = false;
      requestId = '';
      return;
    }
    if (sourceMode === 'existing' && !sourceNote) {
      errorMsg = 'Pick a source note, or switch to "Paste text".';
      busy = false;
      requestId = '';
      return;
    }

    if (!skipDuplicateCheck) {
      try {
        const excludeUuid = destination === 'existing' ? (targetNote?.uuid ?? null) : null;
        const warnings = await invoke<
          { url: string; existing_note_uuid: string; existing_note_title: string }[]
        >('check_duplicate_citations', { accountId: acct, sourceText, excludeUuid });
        if (warnings.length > 0) {
          duplicateWarnings = warnings;
          busy = false;
          requestId = '';
          return;
        }
      } catch (e) {
        // Dedup check is advisory — a transient failure must not block
        // extraction. Log and proceed as if no duplicates were found.
        console.warn('check_duplicate_citations failed — proceeding without dedup check', e);
      }
    }

    try {
      // Backend command takes snake_case fields in Rust; Tauri's serde
      // bridge expects camelCase on the JS side (matches every other
      // invoke in this codebase — see AccountSettings, NoteContextMenu).
      if (destination === 'existing' && targetNote) {
        // Capture the label before the invoke — targetNote gets reset to
        // null right after a successful append, and the note's own body_html
        // in the `notes` store won't reflect the new content until the
        // lookup below, so this is the only reliable source for navigation.
        const targetLabel = targetNote.label;
        const targetUuid = await invoke<string>('append_extract_note', {
          accountId: acct,
          targetUuid: targetNote.uuid,
          sourceText,
          requestId,
        });
        open = false;
        sourceText = '';
        titleOverride = '';
        targetNote = null;
        targetQuery = '';
        destination = 'new';
        // Navigate to wherever the target note lives (its own folder, not
        // necessarily Notes/__Extracts__) and refresh tags, mirroring the
        // new-note path below.
        selectedFolder.set(targetLabel);
        // selectedFolder.set() above is a no-op when targetLabel already
        // equals the current folder (Svelte's safe_not_equal skips
        // subscribers on an unchanged value) — the very likely case when
        // appending into a note in the folder the user is already viewing.
        // Don't rely on the folder-watcher side effect alone: explicitly
        // re-fetch and merge this folder's rows so the `find()` below sees
        // the freshly-appended body_html instead of a stale snapshot.
        try {
          const cached = await invoke<Note[]>('list_cached_notes_in_folder', {
            accountId: acct,
            path: targetLabel,
          });
          notes.update((ns) => {
            const others = ns.filter(
              (n) => !(n.account_id === acct && n.label === targetLabel),
            );
            return [...others, ...cached];
          });
        } catch (e) {
          console.warn('post-append cache paint failed', e);
        }
        try {
          const rows = await invoke<{ uuid: string; tag: string }[]>(
            'list_note_tags',
            { accountId: acct },
          );
          setAccountNoteTags(acct, rows);
        } catch (e) {
          console.warn('post-append tag refresh failed', e);
        }
        const found = get(notes).find((n: Note) => n.uuid === targetUuid);
        if (found) {
          selectedNote.set(found);
          indexUpsertOnSave(acct, null, found.id, found.label);
          await runAutoLinkSuggestions(acct, targetUuid, found.title, found.body_html);
        }
        busy = false;
        requestId = '';
        return;
      }

      const newUuid = await invoke<string>('extract_note', {
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
        await runAutoLinkSuggestions(acct, newUuid, found.title, found.body_html);
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

  function continueAnyway() {
    duplicateWarnings = [];
    extract(true);
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
    resetForm();
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

      <div class="destination-toggle" role="radiogroup" aria-label="Ingest source">
        <button
          type="button"
          class:active={sourceMode === 'paste'}
          onclick={() => setSourceMode('paste')}
          disabled={busy}
        >Paste text</button>
        <button
          type="button"
          class:active={sourceMode === 'existing'}
          onclick={() => setSourceMode('existing')}
          disabled={busy}
        >Pick existing note</button>
      </div>

      {#if sourceMode === 'existing'}
        <label>
          Source note
          <input
            type="text"
            bind:value={sourceNoteQuery}
            oninput={() => { sourceNote = null; scheduleSourceNoteSearch(sourceNoteQuery); }}
            disabled={busy}
            placeholder="Search notes by title or content..."
          />
        </label>
        {#if sourceNote}
          <p class="target-picked">Source: <strong>{sourceNote.title}</strong> ({sourceNote.label})</p>
        {:else if sourceNoteResults.length > 0}
          <ul class="target-results">
            {#each sourceNoteResults as r (r.uuid)}
              <li>
                <button type="button" onclick={() => pickSourceNote(r)}>
                  {r.title} <span class="target-folder">{r.label}</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      {/if}

      <div class="destination-toggle" role="radiogroup" aria-label="Ingest destination">
        <button
          type="button"
          class:active={destination === 'new'}
          disabled={busy}
          onclick={() => setDestination('new')}
        >New note</button>
        <button
          type="button"
          class:active={destination === 'existing'}
          disabled={busy}
          onclick={() => setDestination('existing')}
        >Append to existing note</button>
      </div>

      {#if destination === 'existing'}
        <label>
          Target note
          <input
            type="text"
            bind:value={targetQuery}
            oninput={() => { targetNote = null; scheduleTargetSearch(targetQuery); }}
            disabled={busy}
            placeholder="Search notes by title or content..."
          />
        </label>
        {#if targetNote}
          <p class="target-picked">Appending to: <strong>{targetNote.title}</strong> ({targetNote.label})</p>
        {:else if targetResults.length > 0}
          <ul class="target-results">
            {#each targetResults as r (r.uuid)}
              <li>
                <button type="button" onclick={() => pickTarget(r)}>
                  {r.title} <span class="target-folder">{r.label}</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      {/if}

      <label>
        Source text
        <textarea
          bind:value={sourceText}
          rows="15"
          disabled={busy}
          placeholder="Paste here..."
        ></textarea>
      </label>

      {#if destination === 'new'}
        <label>
          Title (optional)
          <input
            type="text"
            bind:value={titleOverride}
            disabled={busy}
            placeholder="Auto-derived from first lesson"
          />
        </label>
      {/if}

      {#if duplicateWarnings.length > 0}
        <div class="dup-warning">
          {#each duplicateWarnings as w (w.url)}
            <p>
              Already extracted from <strong>{w.url}</strong> in
              <em>{w.existing_note_title}</em>.
            </p>
          {/each}
          <button type="button" onclick={continueAnyway}>Continue anyway</button>
        </div>
      {/if}

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
          onclick={() => extract()}
          disabled={busy || !sourceText.trim() || (destination === 'existing' && !targetNote) || (sourceMode === 'existing' && !sourceNote)}
          class="primary"
        >
          {busy ? 'Extracting…' : destination === 'existing' ? 'Append' : 'Extract'}
        </button>
      </div>
    </div>
  </div>
{/if}

{#if linkSuggestionsOpen}
  <LinkSuggestionsModal
    accountId={$currentAccount ?? ''}
    {proposedAppends}
    onClose={() => { linkSuggestionsOpen = false; proposedAppends = []; }}
  />
{/if}

<style>
  .backdrop {
    position: fixed; inset: 0;
    background: var(--scrim);
    display: flex; align-items: center; justify-content: center;
    z-index: 1000;
  }
  .modal {
    background: var(--surface-editor);
    width: 600px; max-width: 90vw; max-height: 85vh; overflow-y: auto;
    padding: 24px; border-radius: 8px;
    box-shadow: var(--shadow-modal);
  }
  h2 { margin: 0 0 8px; }
  .hint { color: var(--text-muted); font-size: var(--size); margin: 0 0 16px; }
  label { display: block; margin: 12px 0; font-size: var(--size); color: var(--text-secondary); }
  .destination-toggle {
    display: flex;
    gap: 8px;
    margin: 12px 0;
  }
  .destination-toggle button {
    flex: 1;
    padding: 8px;
    font-size: var(--size-sm);
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--surface-panel);
    cursor: pointer;
  }
  .destination-toggle button.active {
    border-color: var(--accent-action);
    background: var(--accent-wash);
    font-weight: 600;
  }
  .target-picked {
    font-size: var(--size);
    color: var(--text);
    line-height: var(--leading);
    margin: 8px 0;
  }
  .target-results {
    list-style: none;
    margin: 4px 0 12px;
    padding: 0;
    max-height: 160px;
    overflow-y: auto;
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
  }
  .target-results li button {
    display: block;
    width: 100%;
    text-align: left;
    padding: 6px 8px;
    background: none;
    border: none;
    cursor: pointer;
    font-size: var(--size-sm);
    line-height: var(--leading);
  }
  .target-results li button:hover {
    background: var(--surface-hover);
  }
  .target-folder {
    color: var(--text-muted);
    font-size: var(--size-xs);
    line-height: var(--leading);
  }
  textarea, input {
    width: 100%; padding: 8px; font: inherit; box-sizing: border-box;
    margin-top: 4px;
    /* Stated, never inherited: the UA stylesheet beats inheritance for form
       controls, so an unset background is the UA's white regardless of theme —
       measured 1.25:1 against --text in dark mode. */
    background: var(--surface-panel);
    color: var(--text);
  }
  textarea { font-family: var(--font-mono); font-size: var(--size-sm); }
  .error {
    color: var(--danger); padding: 8px; background: var(--danger-wash); border-radius: 4px;
    margin: 12px 0; white-space: pre-wrap;
  }
  .dup-warning {
    color: var(--accent-action); padding: 8px; background: var(--warn-wash); border-radius: 4px;
    margin: 12px 0;
  }
  .dup-warning p { margin: 0 0 6px; }
  .dup-warning button {
    background: var(--surface-panel); border: 1px solid var(--accent); border-radius: 4px;
    padding: 4px 10px; cursor: pointer;
  }
  .actions {
    display: flex; gap: 8px; justify-content: flex-end; margin-top: 16px;
  }
  .actions button.primary {
    background: var(--accent-action); color: var(--text-inverse); border: none;
    padding: 8px 16px; border-radius: 4px; cursor: pointer;
  }
  .actions button.primary:disabled { background: var(--border-strong); }
</style>
