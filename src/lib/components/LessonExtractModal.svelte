<script lang="ts">
  import { invoke, Channel } from '@tauri-apps/api/core';
  import { get } from 'svelte/store';
  import {
    currentAccount,
    notes,
    selectedNote,
    selectedFolder,
    setAccountNoteTags,
    indexUpsertOnSave,
  } from '../stores/notes';
  import type { Note, IngestAnalysis, IngestProgress, ExtractedNote } from '../types';
  import LinkSuggestionsModal from './LinkSuggestionsModal.svelte';
  import { requestFolderSuggestion, recordFolderSuggestion } from '../folderSuggestion';
  import { ANALYZE_DEBOUNCE_MS, MAX_URLS_PER_INGEST, initialSelection, progressLine, toggleUrl } from '../ingestSources';

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
        // $notes is a view of whichever folder last painted, not the truth: a
        // folder settle that runs before the new note reaches the remote
        // drops it (measured 2026-09-16). Fall back to SQLite, and write
        // nothing if the row is in neither — `label: ''` would file the note
        // under the root `Notes`, moving it out of Inbox.
        const current =
          get(notes).find((n: Note) => n.uuid === savedUuid) ??
          (await invoke<Note[]>('list_cached_notes', { accountId: acct })).find(
            (n: Note) => n.uuid === savedUuid,
          );
        if (!current) {
          console.warn('auto-link: saved note not found locally — skipping the Related line', savedUuid);
        } else {
          // save_note's real signature (lib.rs:1168-1180) requires
          // existingGmailId/existingUuid/existingXMailCreatedDate — passing
          // `uuid` (not a real param) and omitting the rest would silently
          // create a SECOND note instead of editing this one, since a missing
          // existingUuid makes the backend treat it as brand-new (lib.rs:1186-1191).
          await invoke('save_note', {
            accountId: acct,
            title: savedTitle,
            bodyHtml: `${current.body_html ?? savedBody}${linksLine}`,
            existingGmailId: current.id ?? null,
            existingUuid: savedUuid,
            existingXMailCreatedDate: current.x_mail_created_date ?? null,
            label: current.label,
          });
        }
      }

      if (res.proposed_appends.length > 0) {
        proposedAppends = res.proposed_appends;
        linkSuggestionsOpen = true;
      }
    } catch (e) {
      console.warn('suggest_wiki_links failed — skipping auto-link suggestions', e);
    }
  }

  // Automatic caller of suggest_note_folder (extract filing). SOFT failure,
  // like auto-link: logged, never shown — the note is already saved and a
  // missing proposal costs nothing. Every non-suggestion outcome is silent.
  async function runFolderSuggestion(acct: string, uuid: string) {
    try {
      recordFolderSuggestion(acct, uuid, await requestFolderSuggestion(acct, uuid));
    } catch (e) {
      console.warn('suggest_note_folder failed — no folder suggestion', e);
    }
  }

  // Both post-Extract LLM calls, strictly in order: nothing records whether
  // two agent-CLI subprocesses may run at once (gotcha #7). Counts as part of
  // the Extract the user triggered (spec Decision 8) — roadmap #0b's
  // per-account gate on untriggered calls must cover both.
  async function runPostExtractSuggestions(acct: string, uuid: string, title: string, body: string) {
    await runFolderSuggestion(acct, uuid);
    await runAutoLinkSuggestions(acct, uuid, title, body);
  }

  // Destination: 'new' creates a fresh note in Notes/Inbox (or the root);
  // 'existing' appends into a note the user picks below.
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

  // ── URL ingest (spec 2026-09-15-url-ingest-design.md) ──────────────────
  let analysis = $state<IngestAnalysis | null>(null);
  let selectedUrls = $state<string[]>([]);
  let progressLog = $state<string[]>([]);
  let analyzeTimer: ReturnType<typeof setTimeout> | undefined;
  let analyzeSeq = 0;

  const supportedCount = $derived(analysis ? analysis.sources.filter((s) => s.supported).length : 0);
  // Ingest always creates a new note: in append mode the section is shown disabled.
  const ingesting = $derived(destination === 'new' && selectedUrls.length > 0);

  function excludeUuids(): string[] {
    return [sourceNote?.uuid, targetNote?.uuid].filter((u): u is string => !!u);
  }

  // The stale analysis and its selection are cleared in the SAME turn as the
  // text change, and the fresh pair is assigned together (gotcha #28) —
  // never a selection that belongs to text no longer in the box.
  function scheduleAnalyze(text: string, isHtml: boolean) {
    clearTimeout(analyzeTimer);
    const seq = ++analyzeSeq;
    analysis = null;
    selectedUrls = [];
    const acct = get(currentAccount);
    if (!acct || !text.trim()) return;
    analyzeTimer = setTimeout(async () => {
      try {
        const res = await invoke<IngestAnalysis>('analyze_ingest_sources', {
          accountId: acct,
          text,
          isHtml,
          excludeUuids: excludeUuids(),
        });
        if (seq !== analyzeSeq) return;
        analysis = res;
        selectedUrls = initialSelection(res);
      } catch (e) {
        // Advisory, like check_duplicate_citations: Extract still works.
        console.warn('analyze_ingest_sources failed — no link sources offered', e);
      }
    }, ANALYZE_DEBOUNCE_MS);
  }

  $effect(() => {
    scheduleAnalyze(sourceText, sourceMode === 'existing');
  });

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
    analysis = null;
    selectedUrls = [];
    progressLog = [];
  }

  // RFC4122 v4 UUID generator (no external dep). The backend doesn't validate
  // the format — it's just used as a HashMap key — so any unique-per-call
  // string works. crypto.randomUUID is available in modern browsers / Tauri.
  function newRequestId(): string {
    return globalThis.crypto?.randomUUID?.() ?? `req-${Date.now()}-${Math.random()}`;
  }

  // After a new note exists (Extract or Ingest): navigate to where the
  // backend filed it, paint it from cache, refresh tags, select it, and run
  // the post-Extract suggestions (extract filing spec Decision 8).
  async function showCreatedNote(acct: string, created: ExtractedNote) {
    // Navigate to the folder the backend actually filed the note into —
    // Notes/Inbox, or the root when the backend cannot create folders.
    // The backend already wrote the row synchronously to SQLite; setting
    // selectedFolder triggers App.svelte's paintFolderFromCache watcher
    // (doctrine: cache-only, no Gmail touch on a normal navigation/write
    // path). Then merge the new note row explicitly so we can select it
    // without racing the watcher.
    selectedFolder.set(created.label);
    try {
      const cached = await invoke<Note[]>('list_cached_notes_in_folder', {
        accountId: acct,
        path: created.label,
      });
      notes.update((ns) => {
        const others = ns.filter(
          (n) => !(n.account_id === acct && n.label === created.label),
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
    const found = get(notes).find((n: Note) => n.uuid === created.uuid);
    if (found) {
      selectedNote.set(found);
      // Bump $noteIndex via indexUpsertOnSave. Sidebar's reactive
      // refreshFolders block fires on $noteIndex changes, which is how it
      // discovers a freshly-created Notes/Inbox. Inbox is an ordinary
      // `user`-kind folder, so it stays under Folders — the bump only makes
      // it appear. Without it, the new folder is missing from the sidebar
      // until the next unrelated note save or app restart.
      indexUpsertOnSave(acct, null, found.id, found.label);
      // Not awaited: the modal is closed and the note is saved — nobody
      // waits on either call (spec data flow).
      void runPostExtractSuggestions(acct, created.uuid, found.title, found.body_html);
    }
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
        // necessarily wherever a new-note extract would land) and refresh
        // tags, mirroring the new-note path below.
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

      const created = await invoke<ExtractedNote>('extract_note', {
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

      await showCreatedNote(acct, created);
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

  async function ingest() {
    const acct = get(currentAccount);
    if (!acct || selectedUrls.length === 0) return;
    busy = true;
    errorMsg = '';
    progressLog = [];
    requestId = newRequestId();
    const onProgress = new Channel<IngestProgress>();
    onProgress.onmessage = (p) => {
      progressLog = [...progressLog, progressLine(p)];
    };
    try {
      const created = await invoke<ExtractedNote>('ingest_sources', {
        accountId: acct,
        urls: selectedUrls,
        contextText: analysis?.context_text ?? '',
        titleOverride: titleOverride.trim() || null,
        requestId,
        onProgress,
      });
      open = false;
      resetForm();
      await showCreatedNote(acct, created);
    } catch (e) {
      const msg = String(e);
      // Cancel wrote nothing; anything else keeps the input for a retry.
      errorMsg = msg === 'cancelled' || msg.endsWith(': cancelled') ? '' : msg;
    } finally {
      busy = false;
      requestId = '';
      progressLog = [];
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
        Jodd will distill it into structured key points and file the result in Notes/Inbox (or Notes, on an account that cannot create folders).
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

      {#if analysis && supportedCount > 0}
        <section class="ingest-sources" aria-label="Sources from links">
          <h3>Sources from links</h3>
          {#if destination === 'existing'}
            <p class="ingest-note">Ingesting links creates a new note. Switch to “New note” to use them.</p>
          {:else if supportedCount > MAX_URLS_PER_INGEST}
            <p class="ingest-note">At most {MAX_URLS_PER_INGEST} links can be ingested at once.</p>
          {/if}
          <ul class="source-list">
            {#each analysis.sources as s (s.url)}
              <li>
                <label class="source-row">
                  <input
                    type="checkbox"
                    checked={selectedUrls.includes(s.url)}
                    disabled={busy || !s.supported || destination === 'existing'}
                    onchange={() => (selectedUrls = toggleUrl(selectedUrls, s.url))}
                  />
                  <span class="source-kind">{s.kind === 'youtube' ? 'Video' : s.kind === 'web' ? 'Page' : 'Skipped'}</span>
                  <span class="source-url">{s.url}</span>
                  {#if s.duplicate_owner}
                    <span class="dup-badge">already in “{s.duplicate_owner.title}”</span>
                  {/if}
                  {#if s.reason}
                    <span class="source-reason">{s.reason}</span>
                  {/if}
                </label>
              </li>
            {/each}
          </ul>
          {#if progressLog.length > 0}
            <ol class="ingest-progress" aria-live="polite">
              {#each progressLog as line, i (i)}
                <li>{line}</li>
              {/each}
            </ol>
          {/if}
        </section>
      {/if}

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
          onclick={() => (ingesting ? ingest() : extract())}
          disabled={busy || !sourceText.trim() || (destination === 'existing' && !targetNote) || (sourceMode === 'existing' && !sourceNote)}
          class="primary"
        >
          {#if busy}
            {ingesting ? 'Ingesting…' : 'Extracting…'}
          {:else if ingesting}
            Ingest {selectedUrls.length} {selectedUrls.length === 1 ? 'source' : 'sources'}
          {:else}
            {destination === 'existing' ? 'Append' : 'Extract'}
          {/if}
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
  .ingest-sources { margin: 12px 0; padding: 8px; border: 1px solid var(--border); border-radius: 4px; }
  .ingest-sources h3 { margin: 0 0 6px; font-size: var(--size); }
  .ingest-note { margin: 0 0 6px; color: var(--text-muted); font-size: var(--size-sm); }
  .source-list { list-style: none; margin: 0; padding: 0; }
  .source-row { display: flex; flex-wrap: wrap; gap: 6px; align-items: baseline; margin: 4px 0; font-size: var(--size-sm); color: var(--text); }
  /* The modal's `textarea, input { width: 100% }` also matches this checkbox,
     which then fills its own flex line and renders centred above the URL
     (seen on an Android phone). The row's checkbox sizes to itself. */
  .source-row input[type='checkbox'] { width: auto; padding: 0; margin: 0; flex: none; align-self: center; }
  .source-kind { color: var(--text-muted); }
  .source-url { overflow-wrap: anywhere; }
  .dup-badge { background: var(--warn-wash); color: var(--accent-action); border-radius: 4px; padding: 0 6px; }
  .source-reason { color: var(--text-muted); }
  .ingest-progress { margin: 8px 0 0; padding-left: 20px; font-size: var(--size-sm); color: var(--text-secondary); }
</style>
