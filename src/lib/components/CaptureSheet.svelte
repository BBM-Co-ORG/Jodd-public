<script lang="ts">
  // Share to Jodd — the capture sheet (spec 2026-10-06-share-to-jodd §3.5).
  //
  // Nothing is written until a tap: any web page can open a jodd:// link, so
  // the sheet IS the guard. It reads the queue from Rust (pull, not just the
  // event), so a capture queued before this component was listening — a
  // cold start from the share sheet — is still shown (gotcha #6, #32).
  import { onMount } from 'svelte';
  import { get } from 'svelte/store';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import { activeAccounts, capabilitiesByAccount, currentAccount, error, accountDisplay } from '../stores/notes';
  import { extractModalOpen } from '../stores/ui';
  import {
    pendingCaptures,
    extractPrefill,
    drainCaptures,
    writableAccounts,
    defaultAccount,
    prefillText,
    showSavedCapture,
    sourceLabel,
    AI_MODES,
    type PendingCapture,
  } from '../capture';
  import type { ExtractedNote, ModalWorkflow, Note } from '../types';

  let title = $state('');
  let accountId = $state<string | null>(null);
  let busy = $state(false);
  let errorMsg = $state('');
  let showAllText = $state(false);
  // Escape / backdrop puts the sheet away until the next share arrives or
  // Jodd comes back to the foreground; the capture itself stays queued.
  let snoozed = $state(false);
  let shownId = '';

  // 'new' saves a new note; 'existing' appends to a note picked below.
  type Mode = 'new' | 'existing';
  let mode = $state<Mode>('new');
  // '' is the default destination (Rust's resolve_destination).
  let folder = $state('');
  // Tagged with the account it was read for, so another account's list is
  // never shown, whatever order the effects below run in.
  type FolderList = { acct: string; default: string; folders: string[] };
  let folderList = $state<FolderList>({ acct: '', default: '', folders: [] });
  const folders = $derived(folderList.acct === accountId ? folderList.folders : []);
  // Where "no choice" goes: the Inbox, or Notes when the account can't use one.
  const defaultFolder = $derived(folderList.acct === accountId ? folderList.default : 'Notes/Inbox');
  const defaultName = $derived(defaultFolder === 'Notes/Inbox' ? 'Inbox' : defaultFolder);
  let folderSeq = 0;
  let targetQuery = $state('');
  let targetResults = $state<Note[]>([]);
  let targetNote = $state<Note | null>(null);
  let targetTimer: ReturnType<typeof setTimeout> | undefined;
  let targetSeq = 0;
  // Set by a successful save just before the queue moves on: the next item
  // keeps where things go (account, mode, folder, note) and only what was
  // shared changes. Several shares in a row are usually going to one place,
  // and a destination that silently reset filed the second one in Inbox
  // (live pass, 2026-10-09). Plain, not $state: the effect only consumes it.
  let keepDestination = false;
  // "Saved to …" over the next item, so its arrival is visible. Tied to that
  // item's id: it never shows over a share that arrives later.
  let saved = $state<{ text: string; forId: string } | null>(null);

  const current = $derived<PendingCapture | undefined>($pendingCaptures[0]);
  // A string, so effects keyed on it don't re-run on every drain (each one
  // replaces the queue's objects).
  const currentId = $derived(current?.id ?? '');
  const targets = $derived(writableAccounts($activeAccounts, $capabilitiesByAccount));
  // No account at all (AuthScreen): the capture waits for sign-in.
  const visible = $derived(!!current && $activeAccounts.length > 0 && !$extractModalOpen && !snoozed);

  // A new capture at the head of the queue resets the form — in the same
  // reactive turn that picks it (gotcha #28).
  $effect(() => {
    const c = current;
    if (c && c.id !== shownId) {
      shownId = c.id;
      title = c.default_title;
      errorMsg = '';
      showAllText = false;
      if (!keepDestination) {
        accountId = defaultAccount(targets, $currentAccount);
        mode = 'new';
        forgetDestination();
      }
      keepDestination = false;
    }
    // Also when NO account was chosen yet: on a cold start the capture can be
    // drained before the account list has loaded, which left Save disabled.
    if (!accountId || !targets.some((a) => a.id === accountId)) {
      accountId = defaultAccount(targets, $currentAccount);
      forgetDestination();
    }
  });

  // The folders Rust will accept for this account (list_capture_folders),
  // re-read for each capture so a folder made since then is offered.
  $effect(() => {
    const acct = accountId;
    if (!acct || !currentId) return;
    const seq = ++folderSeq;
    invoke<{ default: string; folders: string[] }>('list_capture_folders', { accountId: acct })
      .then((r) => {
        if (seq !== folderSeq) return;
        folderList = { acct, default: r.default, folders: r.folders };
        // A folder kept from the last item that Rust no longer offers.
        if (folder && !r.folders.includes(folder)) folder = '';
      })
      .catch((e) => console.warn('capture: list folders failed', e));
  });

  // Folder and target note belong to one account: forget both in the same
  // turn that changes it (gotcha #28), so Save never sends a mismatched pair.
  function forgetDestination() {
    folder = '';
    clearTimeout(targetTimer);
    targetSeq++;
    targetQuery = '';
    targetResults = [];
    targetNote = null;
  }

  function setMode(next: Mode) {
    mode = next;
    errorMsg = '';
  }

  // Debounced, sequence-guarded — LessonExtractModal's scheduleTargetSearch.
  function scheduleTargetSearch() {
    clearTimeout(targetTimer);
    targetNote = null;
    const seq = ++targetSeq;
    const q = targetQuery.trim();
    const acct = accountId;
    if (!q || !acct) {
      targetResults = [];
      return;
    }
    targetTimer = setTimeout(async () => {
      try {
        const res = await invoke<Note[]>('search_notes', { accountId: acct, label: null, query: q });
        // A note its own app refuses is locked in the editor; append_capture
        // refuses it too, so don't offer it.
        if (seq === targetSeq) targetResults = res.filter((n) => !n.push_blocked_by_remote).slice(0, 20);
      } catch (e) {
        console.warn('capture: note search failed', e);
      }
    }, 150);
  }

  function pickTarget(n: Note) {
    targetNote = n;
    targetResults = [];
    targetQuery = n.title;
  }

  async function refresh() {
    try {
      await drainCaptures();
    } catch (e) {
      console.warn('capture: drain failed', e);
    }
  }

  onMount(() => {
    let disposed = false;
    const stops: Array<() => void> = [];
    const keep = (p: Promise<() => void>) =>
      void p.then((fn) => (disposed ? fn() : stops.push(fn))).catch(() => {});
    // The error a failed share put on the bar. ErrorBar keeps a message until
    // it is dismissed, so a share that then works takes this one down — but
    // never an error something else has put there since.
    let shareError: string | null = null;
    keep(listen('capture-received', () => {
      if (shareError !== null && $error === shareError) error.set(null);
      shareError = null;
      snoozed = false;
      void refresh();
    }));
    keep(listen<string>('capture-error', (e) => {
      shareError = e.payload;
      error.set(e.payload);
    }));
    const onVis = () => {
      if (document.visibilityState === 'visible') { snoozed = false; void refresh(); }
    };
    document.addEventListener('visibilitychange', onVis);
    void refresh();
    return () => {
      disposed = true;
      stops.forEach((fn) => fn());
      document.removeEventListener('visibilitychange', onVis);
    };
  });

  async function save() {
    if (!current || !accountId || busy) return;
    if (mode === 'existing' && !targetNote) return;
    const acct = accountId;
    busy = true;
    errorMsg = '';
    saved = null;
    const where = mode === 'existing' && targetNote ? `Added to ${targetNote.title}` : null;
    try {
      const created =
        mode === 'existing' && targetNote
          ? await invoke<ExtractedNote>('append_capture', {
              accountId: acct,
              captureId: current.id,
              targetUuid: targetNote.uuid,
            })
          : await invoke<ExtractedNote>('save_capture', {
              accountId: acct,
              captureId: current.id,
              title: title.trim() || null,
              folder: folder || null,
            });
      // Before the queue moves on, so the next item's reset sees it.
      keepDestination = true;
      await refresh();
      const next = get(pendingCaptures)[0];
      if (next) saved = { text: where ?? `Saved to ${created.label}`, forId: next.id };
      else keepDestination = false;
      await showSavedCapture(acct, created);
    } catch (e) {
      errorMsg = String(e);
      await refresh();
    } finally {
      busy = false;
    }
  }

  // Hand the capture to the Extract modal with this mode already chosen —
  // every AI path it offers (Key points, Summarize, Action items, Expand
  // bullets, Transcript, and ingesting the links) stays in one place.
  function withAi(workflow: ModalWorkflow) {
    if (!current || !accountId) return;
    saved = null;
    // The AI path runs in the current account (LessonExtractModal's rule), so
    // picking another account here is a navigation to it.
    if ($currentAccount !== accountId) currentAccount.set(accountId);
    extractPrefill.set({ captureId: current.id, text: prefillText(current), title: title.trim(), workflow });
    extractModalOpen.set(true);
  }

  function open(link: string) {
    void openUrl(link).catch((e) => console.warn('capture: open link failed', e));
  }

  async function discard() {
    if (!current || busy) return;
    saved = null;
    try {
      await invoke('discard_capture', { captureId: current.id });
    } finally {
      await refresh();
    }
  }

  function modal(node: HTMLDialogElement) {
    node.showModal();
    return { destroy: () => node.close() };
  }

  const TEXT_PREVIEW_CHARS = 600;
  const text = $derived(current?.payload.text ?? '');
  const textShown = $derived(showAllText || text.length <= TEXT_PREVIEW_CHARS ? text : text.slice(0, TEXT_PREVIEW_CHARS) + '…');
</script>

{#if visible && current}
  <dialog
    use:modal
    class="capture-overlay"
    aria-modal="true"
    aria-label="Shared to Jodd"
    oncancel={(e) => { e.preventDefault(); snoozed = true; }}
    onclick={(e) => { if (e.target === e.currentTarget) snoozed = true; }}
    onkeydown={(e) => e.stopPropagation()}
  >
    <div class="capture-sheet" role="document">
      <div class="capture-head">
        <span class="capture-heading">Shared to Jodd</span>
        {#if $pendingCaptures.length > 1}
          <span class="capture-count">1 of {$pendingCaptures.length}</span>
        {/if}
      </div>

      {#if saved && saved.forId === current.id}
        <div class="capture-saved" role="status">{saved.text} · next shared item</div>
      {/if}

      <section class="capture-shared" aria-label="What was shared">
        <div class="capture-shared-head">
          <span class="capture-section-title">What was shared</span>
          <span class="capture-source">{sourceLabel(current)}</span>
        </div>
        {#each current.links as link (link)}
          <button type="button" class="capture-url" title="Open {link}" onclick={() => open(link)}>{link}</button>
        {/each}
        {#if text}
          <div class="capture-text">{textShown}</div>
          {#if text.length > TEXT_PREVIEW_CHARS}
            <button type="button" class="capture-link-btn" onclick={() => (showAllText = !showAllText)}>
              {showAllText ? 'Show less' : 'Show all'}
            </button>
          {/if}
        {/if}
      </section>

      {#if targets.length === 0}
        <div class="capture-note">None of your accounts can accept new notes in Jodd. You can only discard this.</div>
      {:else}
        <div class="capture-section-title">Save as a note</div>
        <div class="capture-mode" role="group" aria-label="Save as">
          <button type="button" class:active={mode === 'new'} aria-pressed={mode === 'new'} disabled={busy} onclick={() => setMode('new')}>New note</button>
          <button type="button" class:active={mode === 'existing'} aria-pressed={mode === 'existing'} disabled={busy} onclick={() => setMode('existing')}>Add to existing note</button>
        </div>
        {#if mode === 'new'}
          <label class="capture-label" for="capture-title">Title</label>
          <input id="capture-title" class="capture-input" type="text" bind:value={title} maxlength="300" disabled={busy} />
        {/if}
        <label class="capture-label" for="capture-account">Account</label>
        <select id="capture-account" class="capture-input" bind:value={accountId} onchange={forgetDestination} disabled={busy}>
          {#each targets as a (a.id)}
            <option value={a.id}>{accountDisplay(a)}</option>
          {/each}
        </select>
        {#if mode === 'new'}
          <label class="capture-label" for="capture-folder">Folder</label>
          <select id="capture-folder" class="capture-input" bind:value={folder} disabled={busy}>
            <option value="">{defaultName} (default)</option>
            {#each folders as f (f)}
              <option value={f}>{f}</option>
            {/each}
          </select>
          <div class="capture-note capture-folder-hint">
            Saved as-is.{#if !folder && defaultFolder === 'Notes/Inbox'}{' '}In Inbox, Organize can file it later.{/if}
          </div>
        {:else}
          <label class="capture-label" for="capture-target">Note</label>
          <input
            id="capture-target"
            class="capture-input"
            type="text"
            placeholder="Search notes by title or content…"
            bind:value={targetQuery}
            oninput={scheduleTargetSearch}
            disabled={busy}
          />
          {#if targetNote}
            <div class="capture-picked">Adding to <strong>{targetNote.title}</strong> <span class="capture-result-folder">{targetNote.label}</span></div>
          {:else if targetResults.length > 0}
            <div class="capture-results">
              {#each targetResults as r (r.uuid)}
                <button type="button" class="capture-result" onclick={() => pickTarget(r)}>
                  {r.title} <span class="capture-result-folder">{r.label}</span>
                </button>
              {/each}
            </div>
          {/if}
          <div class="capture-note">Added at the end of the note, under today's date.</div>
        {/if}
      {/if}

      {#if errorMsg}
        <div class="capture-error" role="alert">{errorMsg}</div>
      {/if}

      <div class="capture-actions">
        <button type="button" class="capture-btn" onclick={discard} disabled={busy}>Discard</button>
        <span class="capture-spacer"></span>
        {#if targets.length > 0}
          <button type="button" class="capture-btn primary" onclick={save} disabled={busy || !accountId || (mode === 'existing' && !targetNote)}>
            {busy ? 'Saving…' : mode === 'existing' ? 'Add to note' : 'Save'}
          </button>
        {/if}
      </div>

      {#if targets.length > 0}
        <div class="capture-ai">
          <span class="capture-section-title">Or make a note with AI</span>
          <div class="capture-ai-modes">
            {#each AI_MODES as m (m.value)}
              <button type="button" class="capture-chip" onclick={() => withAi(m.value)} disabled={busy}>{m.label}</button>
            {/each}
          </div>
        </div>
      {/if}
    </div>
  </dialog>
{/if}

<style>
  .capture-overlay {
    position: fixed;
    inset: 0;
    width: 100%;
    height: 100%;
    max-width: none;
    max-height: none;
    margin: 0;
    padding: 0;
    border: 0;
    box-sizing: border-box;
    background: var(--scrim);
  }

  .capture-overlay::backdrop {
    background: transparent;
  }

  .capture-overlay[open] {
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 2000;
  }

  .capture-sheet {
    background: var(--surface-panel);
    width: min(520px, 92vw);
    box-sizing: border-box;
    max-height: 86vh;
    overflow-y: auto;
    padding: 18px 20px 14px;
    border-radius: 10px;
    box-shadow: var(--shadow-modal);
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: var(--text);
    font-size: var(--size);
    line-height: var(--leading);
  }

  /* Phone: the sheet is the whole screen, like the app's other panes. */
  @media (max-width: 600px) {
    .capture-sheet {
      width: 100%;
      max-height: none;
      height: 100%;
      border-radius: 0;
    }
  }

  .capture-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: 6px;
  }

  .capture-heading {
    font-weight: 600;
  }

  .capture-count,
  .capture-label,
  .capture-note {
    font-size: var(--size-sm);
    color: var(--text-secondary);
  }

  .capture-label {
    margin-top: 6px;
  }

  .capture-input {
    font-size: var(--size);
    line-height: var(--leading);
    padding: 5px 8px;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: var(--surface-editor);
    color: var(--text);
  }

  .capture-shared {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: var(--surface-list);
  }

  .capture-shared-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
  }

  .capture-section-title {
    font-size: var(--size-sm);
    font-weight: 600;
    color: var(--text);
    margin-top: 6px;
  }

  .capture-shared-head .capture-section-title {
    margin-top: 0;
  }

  .capture-source {
    font-size: var(--size-sm);
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .capture-url {
    align-self: stretch;
    text-align: left;
    border: 0;
    background: none;
    padding: 0;
    color: var(--accent-action);
    font-size: var(--size-sm);
    line-height: var(--leading);
    text-decoration: underline;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .capture-saved {
    font-size: var(--size-sm);
    color: var(--text-secondary);
    background: var(--surface-list);
    border-radius: 5px;
    padding: 4px 8px;
  }

  .capture-mode {
    display: flex;
    gap: 0;
    margin-top: 4px;
  }

  .capture-mode button {
    flex: 1;
    padding: 4px 10px;
    font-size: var(--size-sm);
    border: 1px solid var(--border);
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
  }

  .capture-mode button:first-child {
    border-radius: 5px 0 0 5px;
  }

  .capture-mode button:last-child {
    border-radius: 0 5px 5px 0;
    border-left: 0;
  }

  .capture-mode button.active {
    background: var(--accent-action);
    border-color: var(--accent-action);
    color: var(--text-inverse);
  }

  /* Neither the list nor its rows may shrink to fit: the sheet is itself a
     flex column, and a shrinking row clips its text instead of scrolling. */
  .capture-results {
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: 5px;
    max-height: 30vh;
    overflow-y: auto;
  }

  .capture-result {
    flex-shrink: 0;
    text-align: left;
    border: 0;
    border-bottom: 1px solid var(--border);
    background: var(--surface-editor);
    color: var(--text);
    padding: 5px 8px;
    font-size: var(--size-sm);
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .capture-result:last-child {
    border-bottom: 0;
  }

  .capture-result:hover {
    background: var(--surface-list);
  }

  .capture-result-folder {
    color: var(--text-secondary);
    margin-left: 6px;
  }

  .capture-picked {
    font-size: var(--size-sm);
    overflow-wrap: anywhere;
  }

  .capture-ai {
    border-top: 1px solid var(--border);
    margin-top: 10px;
    padding-top: 4px;
  }

  .capture-ai-modes {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 6px;
  }

  .capture-chip {
    padding: 4px 10px;
    font-size: var(--size-sm);
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
  }

  .capture-chip:hover:not(:disabled) {
    background: var(--surface-list);
  }

  .capture-text {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: var(--size-sm);
    background: var(--surface-editor);
    border-radius: 5px;
    padding: 6px 8px;
    max-height: 30vh;
    overflow-y: auto;
  }

  .capture-link-btn {
    align-self: flex-start;
    border: 0;
    background: none;
    color: var(--accent-action);
    font-size: var(--size-sm);
    padding: 0;
    cursor: pointer;
  }

  .capture-error {
    color: var(--danger);
    font-size: var(--size-sm);
  }

  .capture-actions {
    display: flex;
    gap: 8px;
    margin-top: 12px;
    flex-wrap: wrap;
  }

  .capture-spacer {
    flex: 1;
  }

  .capture-btn {
    padding: 5px 14px;
    font-size: var(--size-sm);
    border-radius: 5px;
    border: 1px solid var(--border);
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
  }

  .capture-btn:hover:not(:disabled) {
    background: var(--surface-list);
  }

  .capture-btn:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .capture-btn.primary {
    background: var(--accent-action);
    color: var(--text-inverse);
    border-color: var(--accent-action);
  }

  .capture-btn.primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }
</style>
