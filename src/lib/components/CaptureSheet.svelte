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
  import { extractModalOpen, settingsRoute, settingsDirty } from '../stores/ui';
  import {
    pendingCaptures,
    captureSnoozed,
    focusCapture,
    extractPrefill,
    drainCaptures,
    writableAccounts,
    defaultAccount,
    prefillText,
    showSavedCapture,
    sourceLabel,
    filterFolders,
    AI_MODES,
    AI_HINTS,
    loadCaptureMake,
    saveCaptureMake,
    type CaptureMake,
    requestFolderPull,
    type PendingCapture,
  } from '../capture';
  import type { ExtractedNote, ModalWorkflow, Note } from '../types';
  import Modal from './Modal.svelte';

  let title = $state('');
  // Whether the person typed in the title field (spec D13). The field starts
  // as a default (the host or first line) that Jodd chose, and may be replaced
  // by a peeked title (D9); neither is something the person said, so neither
  // is forwarded to Extract as an override.
  let titleEdited = $state(false);
  // Newest capture's peek wins; an older answer is dropped.
  let peekSeq = 0;
  let accountId = $state<string | null>(null);
  let busy = $state(false);
  let errorMsg = $state('');
  let showAllText = $state(false);
  // Escape / backdrop / ✕ / Android Back (every Modal close path) puts the
  // sheet away until the next share arrives or Jodd is launched again; the
  // capture itself stays queued. Coming back to the foreground does NOT bring
  // it back: on a Tab S7 that fired on screen-off/on and app switches and
  // covered a note being read (2026-11-14).
  let shownId = '';

  // What to make, as in Make a note with AI: the capture as it is, or one AI
  // mode. Picked here, then the primary acts on it — the AI modes used to be
  // buttons that left the sheet at once, at the bottom of it (2026-11-20).
  // Remembered, so someone who always makes Key points starts there.
  let make = $state<CaptureMake>(loadCaptureMake());
  const withAiMode = $derived(make !== 'as_is');
  function setMake(next: CaptureMake) {
    make = next;
    saveCaptureMake(next);
    errorMsg = '';
  }

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
  // A search over the folders above, not a <select>: with many folders a
  // dropdown was scroll-only. Filtering is local — the list is already here.
  let folderQuery = $state('');
  // The list is folded away until asked for. Always open, it filled a phone's
  // screen and every swipe scrolled it instead of the sheet, so nothing below
  // it could be reached (2026-11-20). Open, it is part of the page, not a
  // scroller of its own: a swipe that started on it still scrolled only the
  // list (device pass, 2026-11-21).
  let folderOpen = $state(false);
  const folderMatches = $derived(filterFolders(folders, folderQuery));
  // The default row is searchable by the name it shows.
  const showDefault = $derived(filterFolders([`${defaultName} (default)`], folderQuery).length > 0);
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
  const wanted = $derived(!!current && $activeAccounts.length > 0 && !$extractModalOpen && !$captureSnoozed);
  // A share is a new task, so Settings left open closes first (live
  // 2026-11-07, Tab S7: the note the AI made opened UNDER Settings and looked
  // lost). Not when it holds unsaved edits — those stay, under the sheet.
  // The sheet waits one flush for Settings to go: its overlays must leave
  // Android's history (one back each) before the sheet pushes its own entry,
  // or Back pops the wrong one.
  const settingsInTheWay = $derived($settingsRoute !== null && !$settingsDirty);
  const visible = $derived(wanted && !settingsInTheWay);
  $effect(() => {
    if (wanted && settingsInTheWay) settingsRoute.set(null);
  });

  // A new capture at the head of the queue resets the form — in the same
  // reactive turn that picks it (gotcha #28).
  $effect(() => {
    const c = current;
    // The queue emptied (Save of the last item): a peek still in flight has no
    // field left to write.
    if (!c) peekSeq++;
    if (c && c.id !== shownId) {
      shownId = c.id;
      title = c.default_title;
      titleEdited = false;
      startPeek(c);
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

  // Spec D9: a shared video link's caption and author, read in the background
  // so the title field is not "instagram.com". Once per capture; saving never
  // waits for it; a failure is silent and the default stays. Identity
  // (`shownId`) and the values compared (`title`, `titleEdited`) are assigned
  // together by the effect above (gotcha #28), so this check is against the
  // capture and the default that were actually shown.
  function startPeek(c: PendingCapture) {
    const link = c.links[0];
    const seq = ++peekSeq;
    if (!link) return;
    const shown = c.default_title;
    invoke<{ title?: string | null } | null>('peek_link_metadata', { url: link })
      .then((r) => {
        if (seq !== peekSeq || shownId !== c.id) return;
        if (r?.title && !titleEdited && title === shown) title = r.title;
      })
      .catch((e) => console.warn('capture: peek failed', e));
  }

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
    folderQuery = '';
    folderOpen = false;
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

  // Enter takes the first row shown, so "type, Enter, Save" needs no mouse.
  function onFolderKey(e: KeyboardEvent) {
    if (e.key !== 'Enter' || e.isComposing) return;
    e.preventDefault();
    if (showDefault) pickFolder('');
    else if (folderMatches.length > 0) pickFolder(folderMatches[0]);
  }

  // A pick folds the list away again, so the sheet scrolls freely.
  function pickFolder(f: string) {
    folder = f;
    folderQuery = '';
    folderOpen = false;
  }

  function pickTarget(n: Note) {
    targetNote = n;
    targetResults = [];
    targetQuery = n.title;
    // Fresh before the append if the read wins; harmless if Save does.
    const acct = n.account_id ?? accountId;
    if (acct && n.label) requestFolderPull({ accountId: acct, folder: n.label });
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
      captureSnoozed.set(false);
      void refresh();
    }));
    keep(listen<string>('capture-error', (e) => {
      shareError = e.payload;
      error.set(e.payload);
    }));
    const onVis = () => {
      // Refresh the queue, but leave a dismissed sheet dismissed.
      if (document.visibilityState === 'visible') void refresh();
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
    // D13: only a title the person typed is an override; a default or peeked
    // one leaves the AI to title its own note.
    extractPrefill.set({ kind: 'capture', captureId: current.id, text: prefillText(current), title: titleEdited ? title.trim() : '', workflow });
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

  // Every Modal close path snoozes; Discard and Save are the only ways a
  // capture leaves the queue.
  function snooze() {
    captureSnoozed.set(true);
  }

  // The queue is the person's to work through in any order: picking one makes
  // it the sheet's current capture (the form resets for it, gotcha #28).
  let showQueue = $state(false);
  function pick(id: string) {
    if (busy) return;
    focusCapture(id);
    showQueue = false;
  }
  // The pill: the sheet is away but captures wait — Settings or Extract in the
  // way count as "away" too, so only show it when the sheet could open.
  const pill = $derived(
    !!current && $activeAccounts.length > 0 && $captureSnoozed && !$extractModalOpen,
  );
  function reopen() {
    captureSnoozed.set(false);
  }

  // The sheet's first line says where the result goes, as Make a note with
  // AI's does. The AI path files in the account's default (the dialog reads
  // the same `list_capture_folders` answer), so a picked folder is as-is only.
  const accountName = $derived.by(() => {
    const a = targets.find((t) => t.id === accountId);
    return a ? accountDisplay(a) : '';
  });
  const destinationLine = $derived(
    !withAiMode && mode === 'existing'
      ? targetNote
        ? `Adds to “${targetNote.title}”.`
        : 'Adds to the note you pick below.'
      : `Saves to ${withAiMode ? defaultName : folder || defaultName} in ${accountName}.`,
  );

  const TEXT_PREVIEW_CHARS = 600;
  const text = $derived(current?.payload.text ?? '');
  const textShown = $derived(showAllText || text.length <= TEXT_PREVIEW_CHARS ? text : text.slice(0, TEXT_PREVIEW_CHARS) + '…');
</script>

{#if pill}
  <button type="button" class="capture-pill" onclick={reopen} aria-label="Open {$pendingCaptures.length} shared to Jodd">
    Shared to Jodd · {$pendingCaptures.length}
  </button>
{/if}

{#if visible && current}
  <Modal
    title="Shared to Jodd"
    size="md"
    phone="page"
    onclose={snooze}
    headerActions={$pendingCaptures.length > 1 ? captureCount : undefined}
    primary={targets.length > 0
      ? {
          label: busy ? 'Saving…' : withAiMode ? 'Make note' : mode === 'existing' ? 'Add to note' : 'Save',
          onclick: () => (make === 'as_is' ? save() : withAi(make)),
          disabled: !accountId || (!withAiMode && mode === 'existing' && !targetNote),
          busy,
        }
      : undefined}
    {footer}
  >
    <div class="capture-sheet">
      {#if saved && saved.forId === current.id}
        <div class="capture-saved" role="status">{saved.text} · next shared item</div>
      {/if}

      {#if showQueue && $pendingCaptures.length > 1}
        <section class="capture-queue" aria-label="Waiting to be saved">
          {#each $pendingCaptures as c, i (c.id)}
            <button type="button" class="capture-result" class:active={i === 0} aria-pressed={i === 0} disabled={busy} onclick={() => pick(c.id)}>
              {c.default_title || sourceLabel(c) || 'Shared item'}
              <span class="capture-source">{sourceLabel(c)}</span>
            </button>
          {/each}
        </section>
      {/if}

      <!-- The same order as Make a note with AI (spec 2026-10-28 §7): where it
           goes, what to make, what from, then the options for that. What to
           make used to sit under the whole form, out of a phone's reach
           (2026-11-20). -->
      {#if targets.length > 0}
        <p class="capture-note capture-destination">{destinationLine}</p>
        <div class="capture-section-title" id="capture-make-heading">What to make</div>
        <div class="capture-make" role="radiogroup" aria-labelledby="capture-make-heading">
          <button type="button" role="radio" aria-checked={make === 'as_is'} class:active={make === 'as_is'} disabled={busy} onclick={() => setMake('as_is')}>As it is</button>
          {#each AI_MODES as m (m.value)}
            <button type="button" role="radio" aria-checked={make === m.value} class:active={make === m.value} disabled={busy} onclick={() => setMake(m.value)}>{m.label}</button>
          {/each}
        </div>
        <p class="capture-note capture-make-hint">{make === 'as_is' ? 'Jodd will save it as it is, with no AI.' : `${AI_HINTS[make]}. Next, you see what it will read.`}</p>
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
        {#if !withAiMode && mode === 'new'}
          <label class="capture-label" for="capture-title">Title</label>
          <input id="capture-title" class="capture-input" type="text" bind:value={title} oninput={() => (titleEdited = true)} maxlength="300" disabled={busy} />
        {/if}
        <label class="capture-label" for="capture-account">Account</label>
        <select id="capture-account" class="capture-input" bind:value={accountId} onchange={forgetDestination} disabled={busy}>
          {#each targets as a (a.id)}
            <option value={a.id}>{accountDisplay(a)}</option>
          {/each}
        </select>
        {#if withAiMode}
          <div class="capture-note">Title, links and where it goes are on the next page.</div>
        {:else if mode === 'new'}
          {#if folderOpen}
            <label class="capture-label" for="capture-folder">Folder</label>
          {:else}
            <span class="capture-label">Folder</span>
          {/if}
          <!-- Above the list, so Change and Done stay in one place: the list
               is part of the page and can run long (2026-11-21). -->
          <div class="capture-folder-row">
            <div class="capture-picked">Saving to <strong>{folder || defaultName}</strong></div>
            <button
              type="button"
              class="capture-link-btn capture-folder-change"
              aria-expanded={folderOpen}
              disabled={busy}
              onclick={() => (folderOpen ? pickFolder(folder) : (folderOpen = true))}
            >{folderOpen ? 'Done' : 'Change'}</button>
          </div>
          {#if folderOpen}
            <!-- svelte-ignore a11y_autofocus -->
            <input
              id="capture-folder"
              class="capture-input"
              type="text"
              placeholder="Search folders…"
              bind:value={folderQuery}
              onkeydown={onFolderKey}
              disabled={busy}
              autofocus
            />
            <div class="capture-results capture-folders">
              {#if showDefault}
                <button type="button" class="capture-result" class:active={folder === ''} aria-pressed={folder === ''} data-folder="" disabled={busy} onclick={() => pickFolder('')}>
                  {defaultName} (default)
                </button>
              {/if}
              {#each folderMatches as f (f)}
                <button type="button" class="capture-result" class:active={folder === f} aria-pressed={folder === f} data-folder={f} disabled={busy} onclick={() => pickFolder(f)}>
                  {f}
                </button>
              {/each}
              {#if !showDefault && folderMatches.length === 0}
                <div class="capture-result capture-result-none">No folder matches “{folderQuery.trim()}”.</div>
              {/if}
            </div>
          {/if}
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
        {#if !withAiMode}
          <!-- Last, as "Add to" is in Make a note with AI. -->
          <div class="capture-section-title" id="capture-addto-heading">Add to</div>
          <div class="capture-make" role="radiogroup" aria-labelledby="capture-addto-heading">
            <button type="button" role="radio" aria-checked={mode === 'new'} class:active={mode === 'new'} disabled={busy} onclick={() => setMode('new')}>New note</button>
            <button type="button" role="radio" aria-checked={mode === 'existing'} class:active={mode === 'existing'} disabled={busy} onclick={() => setMode('existing')}>Existing note</button>
          </div>
        {/if}
      {/if}

      {#if errorMsg}
        <div class="capture-error" role="alert">{errorMsg}</div>
      {/if}

    </div>
  </Modal>
{/if}

{#snippet captureCount()}
  <button
    type="button"
    class="capture-count capture-count-btn"
    aria-expanded={showQueue}
    onclick={() => (showQueue = !showQueue)}
  >1 of {$pendingCaptures.length}</button>
{/snippet}

{#snippet footer()}
  <button type="button" class="capture-btn" onclick={discard} disabled={busy}>Discard</button>
{/snippet}

<style>
  /* The sheet's chrome (card, page, scrim, scroller) is Modal's; this is
     only the form's column. */
  .capture-sheet {
    display: flex;
    flex-direction: column;
    gap: var(--space-1-5);
    color: var(--text);
    font-size: var(--size);
    line-height: var(--leading);
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
    border-radius: var(--radius);
    background: var(--surface-editor);
    color: var(--text);
  }

  .capture-shared {
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
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
    font-weight: var(--weight-semibold);
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

  .capture-count-btn {
    background: none;
    border: 0;
    padding: 2px 4px;
    cursor: pointer;
    text-decoration: underline;
  }

  .capture-queue {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 30vh;
    overflow-y: auto;
  }

  .capture-pill {
    position: fixed;
    right: 16px;
    bottom: calc(16px + env(safe-area-inset-bottom, 0px));
    z-index: var(--z-sticky);
    padding: 8px 14px;
    border-radius: var(--radius-lg);
    border: 1px solid var(--border);
    background: var(--surface-list);
    color: var(--text);
    font-size: var(--size-sm);
    box-shadow: 0 2px 8px rgb(0 0 0 / 0.2);
    cursor: pointer;
  }

  .capture-saved {
    font-size: var(--size-sm);
    color: var(--text-secondary);
    background: var(--surface-list);
    border-radius: var(--radius);
    padding: 4px 8px;
  }

  /* Neither the list nor its rows may shrink to fit: the sheet is itself a
     flex column, and a shrinking row clips its text instead of scrolling.
     No max-height or overflow here: a list that scrolls on its own takes
     every swipe that starts on it, and on a phone the page under it stops
     moving (S23 device pass, 2026-11-21). The sheet scrolls; the list grows.
     That holds for every class on the list: a height cap without overflow
     keeps the box short while its rows draw over Add to below it (0.52.0,
     `.capture-folders { max-height: 24vh }`). */
  .capture-results {
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: var(--radius);
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

  .capture-result.active {
    background: var(--surface-list);
    box-shadow: inset 3px 0 0 var(--accent-action);
    font-weight: var(--weight-semibold);
  }

  .capture-result-none {
    color: var(--text-secondary);
    cursor: default;
  }

  .capture-result-folder {
    color: var(--text-secondary);
    margin-left: 6px;
  }

  .capture-picked {
    font-size: var(--size-sm);
    overflow-wrap: anywhere;
  }

  .capture-folder-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
  }

  .capture-folder-change {
    flex-shrink: 0;
  }

  /* The same buttons as Make a note with AI's "What to make", wrapping
     rather than running off a phone's edge. */
  .capture-make {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 4px;
  }

  .capture-make button {
    flex: 1 1 auto;
    padding: 8px 12px;
    font-size: var(--size-sm);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
  }

  .capture-make button.active {
    border-color: var(--accent-action);
    background: var(--accent-wash);
    font-weight: var(--weight-semibold);
  }

  .capture-destination {
    margin: 0;
  }

  .capture-make-hint {
    margin: 0;
  }

  .capture-text {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: var(--size-sm);
    background: var(--surface-editor);
    border-radius: var(--radius);
    padding: 6px 8px;
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

  .capture-btn {
    padding: 5px 14px;
    font-size: var(--size-sm);
    border-radius: var(--radius);
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
</style>
