<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import AuthScreen from './lib/components/AuthScreen.svelte';
  import Sidebar from './lib/components/Sidebar.svelte';
  import NoteList from './lib/components/NoteList.svelte';
  import TrashList from './lib/components/TrashList.svelte';
  import TrashPreview from './lib/components/TrashPreview.svelte';
  import NoteEditor from './lib/components/NoteEditor.svelte';
  import LessonExtractModal from './lib/components/LessonExtractModal.svelte';
  import AskJoddModal from './lib/components/AskJoddModal.svelte';
  import About from './lib/components/About.svelte';
  import WhatsNew from './lib/components/WhatsNew.svelte';
  import AppSettings from './lib/components/AppSettings.svelte';
  import ReindexRecoveryBanner from './lib/components/ReindexRecoveryBanner.svelte';
  import { isAuthenticated, notes, isLoading, isSaving, error, refreshNotes, accounts, activeAccounts, currentAccount, selectedNote, selectedFolder, recentlySavedUuids, recentSaveTimestamp, noteIndex, requestFolderRefresh, hydratedFolders, markFolderHydrated, selectedTags, setAccountNoteTags, selectedSmartFolder, smartFolderNotes, newNoteFn } from './lib/stores/notes';
  import { extractModalOpen, whatsNewOpen, whatsNewVersions, appSettingsOpen, askModalOpen } from './lib/stores/ui';
  import { whatsNewForLaunch } from './lib/whatsNew';
  import { shouldFlush } from './lib/lifecycle';
  import type { MessageIndex, Note } from './lib/types';
  import { get } from 'svelte/store';
  import type { Account } from './lib/types';
  import { isAndroid } from './lib/stores/platform';
  import { viewportWidth, androidLayoutMode } from './lib/stores/viewport';
  import { activePane, navigateToPane, initPhoneNavHistory, handlePopState } from './lib/stores/phoneNav';

  // Dev-only window-title stamp. It is normal to have an installed
  // /Applications/Jodd.app AND a `tauri dev` build running at the same time,
  // and both windows are titled "Jodd" — so screenshots, clicks and hotkeys
  // silently go to whichever one macOS fronted. Stamping the dev window with
  // its page-load time makes the two impossible to confuse (and distinguishes
  // two dev windows from each other). import.meta.env.DEV is compiled out by
  // `vite build`, so a release window keeps the plain "Jodd" title.
  // Needs `core:window:allow-set-title` in capabilities/default.json —
  // core:window:default does NOT include it, and without it setTitle()
  // rejects. Log rather than swallow, so a missing permission is visible
  // instead of silently leaving both windows titled "Jodd".
  if (import.meta.env.DEV) {
    const stamp = new Date().toLocaleTimeString([], { hour12: false });
    getCurrentWindow()
      .setTitle(`Jodd — DEV ${stamp}`)
      .catch((e) => console.warn('dev title stamp failed:', e));
  }

  // Long safety net: every 10 min while focused do a full Notes-tree refresh.
  // Catches anything that drifted in other folders we haven't visited.
  const POLL_MS = 600_000;
  const REFRESH_THROTTLE_MS = 2_000;
  // Settle windows — refresh only fires if user "stays" (focused / on a folder)
  // for this long. Skips API calls during rapid Cmd-Tab and folder hopping.
  const FOCUS_SETTLE_MS = 10_000;
  const FOLDER_SETTLE_MS = 10_000;
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let focusSettleTimer: ReturnType<typeof setTimeout> | null = null;
  let folderSettleTimer: ReturnType<typeof setTimeout> | null = null;
  let lastFolderSeen: string | null = null;
  let unlistenFocus: (() => void) | null = null;
  let lastRefreshAt = 0; // monotonic timestamp of the most recent loadNotes invocation
  let inFlightRefresh: Promise<void> | null = null; // currently-running refresh (null = idle)
  // Queued refresh — replaced (not appended) when a newer intent arrives.
  // "Coalesce to latest": three triggers during a save → only the last one's
  // function survives, so we run 1 API call when the blocker clears, not 3.
  let pendingRefresh: (() => Promise<void>) | null = null;

  // Queue + drain: register an intent to refresh. If the blockers (in-flight
  // refresh OR active save) are clear, runs immediately. Otherwise the
  // function is parked in pendingRefresh — a single slot, not a queue, so
  // newer intents replace older ones (coalesce-to-latest). Once the blocker
  // clears, drainQueue picks it up and runs it.
  function scheduleRefresh(fn: () => Promise<void>) {
    // Folders re-read on every refresh path, not just the note-shaped ones.
    // `fn` only ever reloads NOTES, so $noteIndex — Sidebar's other folder
    // trigger — cannot see a change that touched only the `folders` table
    // (jodd-mcp writing straight to SQLite, or the sync worker draining a
    // dirty_new row). Those stayed invisible until the app restarted.
    //
    // Fired here rather than in requestRefresh() because requestRefresh is
    // NOT the single gate its comment claims: startFocusSettle() calls
    // scheduleRefresh directly, so focusing the window bypassed it entirely
    // and the only surviving trigger was the 10-minute poll. scheduleRefresh
    // is the real chokepoint every path funnels through.
    requestFolderRefresh();
    pendingRefresh = fn;
    drainQueue();
  }

  function drainQueue() {
    if (!pendingRefresh) return;
    if ($isSaving) return;       // save will trigger drain when it completes
    if (inFlightRefresh) return; // current refresh's .finally will re-drain
    const fn = pendingRefresh;
    pendingRefresh = null;
    inFlightRefresh = fn()
      .catch((e) => { error.set(String(e)); })
      .finally(() => {
        inFlightRefresh = null;
        // Drain again — something might have been queued while we were running.
        drainQueue();
      });
  }

  // React to $isSaving falling false. The save path doesn't know about
  // pendingRefresh, but flipping $isSaving from true → false signals
  // "blocker cleared, try the queue".
  let lastSaving = false;
  $: {
    if (!$isSaving && lastSaving) drainQueue();
    lastSaving = $isSaving;
  }

  // Settles fire a SCOPED refresh (just the active folder) instead of the
  // full Notes-tree sweep. The 10-min poll handles drift in other folders.
  function startFocusSettle() {
    cancelFocusSettle();
    focusSettleTimer = setTimeout(() => {
      focusSettleTimer = null;
      // Queue rather than fire directly — if a save is in flight when this
      // settle hits, we want the refresh to happen as soon as the save
      // finishes, not be lost.
      scheduleRefresh(() => loadFolderNotes(get(selectedFolder)));
    }, FOCUS_SETTLE_MS);
  }
  function cancelFocusSettle() {
    if (focusSettleTimer !== null) {
      clearTimeout(focusSettleTimer);
      focusSettleTimer = null;
    }
  }

  function startFolderSettle() {
    cancelFolderSettle();
    folderSettleTimer = setTimeout(() => {
      folderSettleTimer = null;
      scheduleRefresh(() => loadFolderNotes(get(selectedFolder)));
    }, FOLDER_SETTLE_MS);
  }
  function cancelFolderSettle() {
    if (folderSettleTimer !== null) {
      clearTimeout(folderSettleTimer);
      folderSettleTimer = null;
    }
  }

  // A dismissed account must not stay selected — every read for it is either
  // filtered out or refused, so the UI would sit on an account that returns
  // nothing and explains nothing.
  $: if ($currentAccount && !$activeAccounts.some((a) => a.id === $currentAccount)) {
    currentAccount.set($activeAccounts[0]?.id ?? null);
  }

  // Watch $selectedFolder. Doctrine: navigation must never block on Gmail.
  // First step is ALWAYS to paint from the SQLite cache — sub-ms read,
  // synchronous to the user. Reconciliation against Gmail (refresh + merge
  // + prune) is handed off to the existing settle/sweep machinery as a
  // background concern. The brand-new-folder case "Folder not found" can't
  // happen here: list_cached_notes_in_folder returns [] for any unknown
  // label, no error.
  $: if ($selectedFolder !== lastFolderSeen) {
    lastFolderSeen = $selectedFolder;
    if ($isAuthenticated) {
      const acctNow = get(currentAccount);
      const f = $selectedFolder;
      // Recently Deleted ("__TRASH__") has its own loader (TrashList) and is
      // NOT a real Notes label — skip the cache paint + Gmail settle entirely.
      if (f !== '__TRASH__') {
        if (acctNow && f && f !== '__ALL__') {
          // Immediate cache paint — does not touch Gmail.
          paintFolderFromCache(acctNow, f);
        }
        // Background reconciliation: a fresh server fetch lands within ~10s
        // via the folder settle, which goes through the queue and respects
        // the $isSaving / inFlightRefresh blockers. The sweep tick handles
        // catch-up for folders the user lingered on long enough to miss.
        startFolderSettle();
      }
      navigateToPane('list');
    }
  }

  // Parallel to the folder-change watcher above: pushes the phone nav stack
  // to the note pane whenever a DIFFERENT note becomes selected. Covers
  // every path that calls selectedNote.set() — NoteList's click/keyboard
  // selection and new-note creation — without NoteList needing to know
  // navigation exists. Clearing the selection (uuid → null, e.g. a note
  // deleted out from under the editor) intentionally does not pop the
  // stack; the user backs out via the phone pane header or system back.
  let lastNoteUuidSeen: string | null = null;
  $: {
    const uuid = $selectedNote?.uuid ?? null;
    if (uuid !== lastNoteUuidSeen) {
      lastNoteUuidSeen = uuid;
      if (uuid) navigateToPane('note');
    }
  }

  // Tag navigation, parallel to the folder block above. Selecting tags paints
  // the UNION of their notes from the SQLite cache into $notes (one IPC, no
  // Gmail); NoteList then narrows to AND/OR per the match mode. Paint depends
  // only on WHICH tags are selected, not the mode — the union is a superset of
  // both, so toggling AND/OR re-filters without a re-fetch. Folder and tag
  // views are mutually exclusive — Sidebar.selectFolder clears the selection.
  let lastTagKey = '';
  $: {
    const key = [...$selectedTags].sort().join('');
    if (key !== lastTagKey) {
      lastTagKey = key;
      if ($isAuthenticated && $selectedTags.size > 0) {
        const acctNow = get(currentAccount);
        if (acctNow) paintTagsFromCache(acctNow, [...$selectedTags]);
        // Phone stack: selecting a tag from the Folders pane must land on the
        // list pane, same as picking a folder. Sidebar.selectTag never
        // touches $selectedFolder, so the folder watcher above can't see
        // this — it needs its own navigateToPane call.
        navigateToPane('list');
      }
    }
  }

  // Smart Folder navigation, parallel to the folder/tag blocks above.
  // Fully virtual (design spec decision 4) — no folders-table row, so this
  // is a dedicated fetch path, not a variant of paintFolderFromCache.
  let lastSmartFolderKey = '';
  $: {
    const sf = $selectedSmartFolder;
    const key = sf ? `${sf.account}:${sf.kind}` : '';
    if (key !== lastSmartFolderKey) {
      lastSmartFolderKey = key;
      if (sf) {
        loadSmartFolderNotes(sf.account, sf.kind);
        // Phone stack: entering a Smart Folder from the Folders pane must
        // land on the list pane. Sidebar.selectSmartFolder deliberately
        // leaves $selectedFolder untouched (see its comment), so the folder
        // watcher above never fires for this path.
        navigateToPane('list');
      } else {
        smartFolderNotes.set([]);
      }
    }
  }

  async function loadSmartFolderNotes(accountId: string, kind: 'orphaned' | 'stale') {
    try {
      const command = kind === 'orphaned' ? 'list_orphaned_notes' : 'list_stale_notes';
      const rows = await invoke<any[]>(command, { accountId });
      smartFolderNotes.set(rows);
    } catch (e) {
      console.error('loadSmartFolderNotes failed', e);
      smartFolderNotes.set([]);
    }
  }

  // Ensure every note carrying any selected tag is present and fresh in $notes.
  // Unlike the folder paint we don't drop anything — folder notes stay put;
  // they just won't match the tag filter. Existing copies of these uuids are
  // replaced with the cache rows so the list shows current content.
  async function paintTagsFromCache(accountId: string, tags: string[]) {
    if (tags.length === 0) return;
    try {
      const cached = await invoke<any[]>('list_cached_notes_with_tags', {
        accountId,
        tags,
      });
      if (cached.length === 0) return;
      const byUuid = new Set(cached.map((n: any) => n.uuid));
      notes.update((ns) => {
        const kept = ns.filter(
          (n) => !(n.account_id === accountId && byUuid.has(n.uuid)),
        );
        return [...kept, ...cached];
      });
      reconcileSelection(get(notes));
    } catch (e) {
      console.error('paintTagsFromCache failed', e);
    }
  }

  // Doctrine-compliant navigation: replace just this folder's notes with
  // the SQLite snapshot, in one synchronous IPC. Survivors (tmp: blanks
  // and notes saved within the last 30s) are preserved exactly as
  // loadFolderNotes does, so a save-then-navigate sequence doesn't drop
  // the user's in-flight work.
  async function paintFolderFromCache(accountId: string, folderPath: string) {
    try {
      const cached = await invoke<any[]>('list_cached_notes_in_folder', {
        accountId,
        path: folderPath,
      });
      const cachedUuids = new Set(cached.map((n: any) => n.uuid));
      const recents = get(recentlySavedUuids);
      const cutoff = Date.now() - 30_000;
      notes.update((ns) => {
        const out: any[] = [];
        for (const n of ns) {
          if (n.label !== folderPath) out.push(n);
        }
        const localInFolder = ns.filter((n) => n.label === folderPath);
        const survivors = localInFolder.filter(
          (n) =>
            n.uuid &&
            !cachedUuids.has(n.uuid) &&
            (n.uuid.startsWith('tmp:') ||
              recentSaveTimestamp(recents, n.account_id, n.uuid) > cutoff),
        );
        return [...out, ...survivors, ...cached];
      });
      reconcileSelection(get(notes));
      // Under-hydration guard. The server index ($noteIndex) drives the
      // sidebar count and may know this folder holds notes the local cache
      // hasn't fetched yet (notes created on another device, or a cache that
      // was pruned). The paint above is authoritative for what's local, but
      // leaving the user on an empty list while the badge says N is the
      // count-vs-list gap we're fixing. If the index reports more notes in
      // this folder than the cache returned, fetch it NOW instead of waiting
      // for the ~10s folder settle. Navigation still felt instant (we already
      // painted); this only closes the gap fast. loadFolderNotes reconciles,
      // so a stale over-count costs at most one harmless fetch.
      const idx = get(noteIndex).get(accountId) ?? [];
      let indexCount = 0;
      for (const s of idx) if (s.label === folderPath) indexCount++;
      if (indexCount > cached.length) {
        scheduleRefresh(() => loadFolderNotes(folderPath));
      }
    } catch (e) {
      // SQLite read failing is exceptional; log and let the next refresh
      // try again. Don't surface to the user — navigation should never
      // show errors that the cache can transparently recover from.
      console.error('paintFolderFromCache failed', e);
    }
  }

  // React to the false → true transition of $isAuthenticated, regardless of
  // which path flipped it (initial check, oauth-success event, or AuthScreen's
  // polling fallback). Avoids the case where the event is missed and polling
  // sets the flag but nobody triggers loadNotes.
  let lastAuthed = false;
  $: if ($isAuthenticated && !lastAuthed) {
    lastAuthed = true;
    (async () => {
      initPhoneNavHistory();
      await refreshAccounts();
      // Phase 2 paint: SQLite replica → UI in sub-ms, so the user sees a
      // populated list before any network call returns.
      await loadCachedNotes();
      // Phase C — fast index pass first: every account's {msg_id, label}
      // list. This is what populates the sidebar counts. No bodies yet.
      await indexAllAccounts();
      // Tags are Jodd-local SQLite state — load each account's full tag map
      // so the sidebar Tags section and note chips are populated on cold
      // start, before any Gmail fetch. Cheap, pure-local; failures log.
      await loadTags();
      // Cross-Jodd pin sync: pull each account's meta_label sidecars and
      // apply pin state to the cache. Runs in parallel across accounts —
      // each one only hits meta_label (small, scoped) so this completes
      // in a second or two even on mailboxes with many sidecars. Failures
      // log but don't block the cold-start path; pins will catch up on
      // the next sync_pin_state trigger or a list_notes refresh.
      try {
        const list = get(activeAccounts);
        // sync_tag_state is a disabled no-op (tags round-trip via inline
        // #hashtags in the body, not a sidecar) — kept in the Promise.allSettled
        // alongside sync_pin_state only so removing it doesn't require touching
        // this call site too.
        await Promise.allSettled(
          list.flatMap((a) => [
            invoke<number>('sync_pin_state', { accountId: a.id }).catch((e) => {
              console.warn(`sync_pin_state failed for ${a.id}:`, e);
              return 0;
            }),
            invoke<number>('sync_tag_state', { accountId: a.id }).catch((e) => {
              console.warn(`sync_tag_state failed for ${a.id}:`, e);
              return 0;
            }),
          ]),
        );
        // Re-paint from the cache: pin reorders the list, and tag rows
        // landing in note_tags need to be reloaded into the noteTagsByAccount
        // store so chips + sidebar count cloud reflect the synced state.
        await loadCachedNotes();
        await loadTags();
      } catch (e) {
        console.warn('cold-start sidecar sync failed:', e);
      }
      // Phase C — hydrate the focused folder FIRST so the visible NoteList
      // has fresh server data immediately. Other folders are filled in by
      // the background sweep below.
      const acctNow = get(currentAccount);
      const folderNow = get(selectedFolder);
      if (acctNow && folderNow && folderNow !== '__ALL__') {
        try { await loadFolderNotes(folderNow); } catch (e) { console.error(e); }
      }
      startBackgroundSweep();
    })();
    startFocusPolling();
  } else if (!$isAuthenticated && lastAuthed) {
    lastAuthed = false;
    stopPolling();
    cancelFocusSettle();
    cancelFolderSettle();
    stopBackgroundSweep();
  }

  function startPolling() {
    stopPolling();
    pollTimer = setInterval(() => {
      // Use the throttled gate so the periodic poll doesn't double-up
      // with focus/activity refreshes that fire on the same tick.
      requestRefresh('poll');
    }, POLL_MS);
  }

  // Single throttled gate for ALL refresh triggers. Coalesces:
  //   - focus events (onFocusChanged, tauri://focus, visibilitychange)
  //   - activity events (mouseenter, keydown)
  //   - periodic poll
  //   - manual refresh button
  //
  // Two layers of guarding:
  //   1) inFlightRefresh — if a fetch is already running, return its Promise.
  //      Without this, Cmd-Tab back fires onFocusChanged + tauri://focus +
  //      visibilitychange all within ~16ms, each calling loadNotes() in
  //      parallel — 3 round-trips for one user event.
  //   2) lastRefreshAt + per-source min-gap — protects against bursty
  //      triggers from the same source (e.g. mousemove storms).
  function requestRefresh(source: 'focus' | 'poll' | 'manual' | 'folder') {
    // Throttle prevents bursty triggers (poll + manual click in the same tick)
    // from queuing redundant work. Manual button skips it — explicit user
    // action wins. Settle/folder paths go through scheduleRefresh directly.
    const minGap = source === 'manual' ? 0 : REFRESH_THROTTLE_MS;
    if (Date.now() - lastRefreshAt < minGap) return Promise.resolve();
    lastRefreshAt = Date.now();
    scheduleRefresh(() => loadNotes());
    return Promise.resolve();
  }

  function stopPolling() {
    if (pollTimer !== null) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  async function startFocusPolling() {
    // Listen to native window focus (fires on Cmd-Tab away too, unlike browser
    // visibilitychange). Pause polling when unfocused to save battery/quota;
    // refresh immediately when focus returns (highest-value moment for sync).
    const win = getCurrentWindow();
    console.log('[jodd] registering onFocusChanged listener');

    // All three focus paths route through startFocusSettle(), which restarts
    // the same 10s settle timer each time — so the burst that fires when
    // Cmd-Tab back triggers onFocusChanged + tauri://focus + visibilitychange
    // within one animation frame collapses to a single refresh. Note this
    // path reaches scheduleRefresh DIRECTLY, not via requestRefresh(): the
    // throttle gate there covers the poll and manual paths only.
    const unlisten1 = await win.onFocusChanged(({ payload: focused }) => {
      if (focused) {
        startFocusSettle();
        startPolling();
      } else {
        cancelFocusSettle();
        stopPolling();
      }
    });
    const unlisten2 = await listen('tauri://focus', () => {
      startFocusSettle();
      startPolling();
    });
    const unlisten3 = await listen('tauri://blur', () => {
      cancelFocusSettle();
      stopPolling();
    });

    const onVis = () => {
      if (document.visibilityState === 'visible') {
        startFocusSettle();
        startPolling();
      } else {
        cancelFocusSettle();
        stopPolling();
      }
    };
    document.addEventListener('visibilitychange', onVis);

    // Activity-based refreshes (mouseenter/keydown) were removed — they
    // overlapped with focus-settle + the 10-min poll, and produced up to 6
    // calls/min for active users without adding signal. Focus settle + poll
    // + manual ⟳ button + folder settle cover all the cases.

    unlistenFocus = () => {
      unlisten1();
      unlisten2();
      unlisten3();
      document.removeEventListener('visibilitychange', onVis);
    };

    // Assume the window is focused on mount (it usually is — sign-in just happened).
    startPolling();
  }

  onMount(async () => {
    // What's New: show release notes once per version bump (see src/lib/whatsNew.ts).
    try {
      const versions = await whatsNewForLaunch();
      if (versions.length > 0) {
        whatsNewVersions.set(versions);
        whatsNewOpen.set(true);
      }
    } catch (e) {
      console.error('whats-new check failed', e);
    }

    const authed = await invoke<boolean>('is_authenticated');
    isAuthenticated.set(authed);

    await listen<string>('oauth-success', async () => {
      isAuthenticated.set(true);
    });

    await listen<string>('oauth-error', (event) => {
      error.set(event.payload);
    });

    window.addEventListener('jodd:open-note', onOpenNoteEvent);

    // Flush the sync worker on visibility transitions. Android freezes
    // backgrounded processes, which stops the worker's 5s sleep/tick loop
    // mid-cycle — a note edited just before switching apps would otherwise
    // sit `dirty` with no signal to the user. Going hidden is the last
    // chance to push before the freeze; becoming visible again is a chance
    // to catch up rather than waiting out the remaining sleep. Errors are
    // swallowed deliberately: a failed opportunistic flush must never
    // surface as a user-visible error — the row stays dirty and the 5s
    // loop retries. See src/lib/lifecycle.ts.
    document.addEventListener('visibilitychange', onVisibility);
    window.addEventListener('pagehide', onPageHide);
    window.addEventListener('popstate', handlePopState);
  });

  onDestroy(() => {
    stopPolling();
    cancelFocusSettle();
    cancelFolderSettle();
    unlistenFocus?.();
    window.removeEventListener('jodd:open-note', onOpenNoteEvent);
    document.removeEventListener('visibilitychange', onVisibility);
    window.removeEventListener('pagehide', onPageHide);
    window.removeEventListener('popstate', handlePopState);
  });

  function onVisibility() {
    if (shouldFlush(document.visibilityState)) {
      invoke('flush_sync').catch(() => {});
    }
  }

  function onPageHide() {
    invoke('flush_sync').catch(() => {});
  }

  // Ask Jodd citation chips dispatch this event (see AskJoddModal's
  // onAnswerClick — chips are rendered from a string via {@html} so they
  // can't carry Svelte handlers directly). Mirrors NoteEditor's
  // openConnection(): switch $currentAccount if the note lives in a
  // different account, then select it. Uses list_cached_notes (pure local
  // read, no Gmail touch — doctrine) rather than $notes, since a note cited
  // from a folder that hasn't been navigated to yet may not be loaded there.
  function onOpenNoteEvent(e: Event) {
    const { uuid, accountId } = (e as CustomEvent).detail;
    selectNoteByUuid(accountId, uuid);
  }

  async function selectNoteByUuid(accountId: string, uuid: string) {
    if (!accountId || !uuid) return;
    try {
      const cached = await invoke<Note[]>('list_cached_notes', {
        accountId,
      });
      const found = cached.find((n) => n.uuid === uuid);
      if (!found) {
        console.warn('selectNoteByUuid: uuid not found in cache', accountId, uuid);
        return;
      }
      if (get(currentAccount) !== accountId) currentAccount.set(accountId);
      selectedNote.set(found);
    } catch (e) {
      console.warn('selectNoteByUuid failed', e);
    }
  }

  // Tauri commands surface "no refresh token in keychain for <email>" when
  // an account's Keychain entry is missing/revoked. The accounts.json entry
  // still exists, so the app keeps thinking it's signed in. Catch the error
  // here, drop the dead account, and (if nothing's left) bounce to the
  // AuthScreen so the user can re-OAuth without restarting the app.
  function isAuthLostError(e: unknown): boolean {
    const s = typeof e === 'string' ? e : e instanceof Error ? e.message : String(e);
    return /no refresh token in keychain/i.test(s);
  }

  async function handleAuthLoss(accountId: string) {
    console.warn(`[jodd] auth lost for ${accountId} — removing locally`);
    try {
      await invoke('remove_account', { accountId });
    } catch (e) {
      console.error('remove_account during auth-loss recovery failed', e);
    }
    // Drop in-memory state for the dead account so its stub rows / hydrated
    // notes don't linger and confuse the sidebar counts.
    notes.update((ns) => ns.filter((n) => n.account_id !== accountId));
    noteIndex.update((m) => { m.delete(accountId); return m; });
    hydratedFolders.update((m) => { m.delete(accountId); return m; });
    await refreshAccounts();
    const remaining = get(accounts);
    if (remaining.length === 0) {
      isAuthenticated.set(false);
      error.set('Signed out — Keychain credentials were removed. Please sign in again.');
    } else {
      if (get(currentAccount) === accountId) {
        currentAccount.set(remaining[0].id);
        selectedFolder.set('Notes');
      }
      error.set(`Account ${accountId} needs to be signed in again.`);
    }
  }

  // Fetch the current account list from the backend (loaded from accounts.json).
  // Sets currentAccount to the first account if none is selected yet.
  async function refreshAccounts() {
    try {
      const list = await invoke<Account[]>('list_accounts');
      accounts.set(list);
      if (list.length > 0) {
        let cur: string | null = null;
        currentAccount.subscribe((v) => (cur = v))();
        if (!cur || !list.some((a) => a.id === cur)) {
          currentAccount.set(list[0].id);
        }
      } else {
        currentAccount.set(null);
      }
    } catch (e) {
      error.set(String(e));
    }
  }

  // Scoped per-folder refresh. Replaces only the notes in `folderPath` with
  // fresh server data; notes in OTHER folders are left untouched. Far
  // cheaper than the full Notes-tree sweep — and matches the user's intent
  // when they're focused on one folder. The 10-min poll catches anything
  // that drifted elsewhere.
  async function loadFolderNotes(folderPath: string) {
    // __ALL__ is a UI-only sentinel for the "All <account>" virtual row —
    // it has no Gmail label and no folders-table row, so a Gmail fetch
    // would error. The background sweep keeps the All view fresh.
    if (!folderPath || folderPath === '__ALL__') return;
    let accountId: string | null = null;
    currentAccount.subscribe((v) => (accountId = v))();
    if (!accountId) return;
    // No local $isSaving / inFlightRefresh guards here — scheduleRefresh
    // (the only caller path from settles/poll/manual) handles those before
    // we ever get invoked. Direct callers (initial auth load) run in a
    // known-quiet state where neither blocker applies.
    isLoading.set(true);
    try {
      const fetched = await invoke<any[]>('list_notes_in_folder', {
        accountId,
        path: folderPath,
      });
      const fetchedUuids = new Set(fetched.map((n: any) => n.uuid));
      const recents = get(recentlySavedUuids);
      const cutoff = Date.now() - 30_000;
      notes.update((ns) => {
        const out: any[] = [];
        // Keep notes from OTHER folders unchanged.
        for (const n of ns) {
          if (n.label !== folderPath) out.push(n);
        }
        // For THIS folder: protect tmp: blanks and notes saved within the
        // last 30s (Gmail may not have indexed them yet). Drop everything
        // else that isn't in the fetched list.
        const localInFolder = ns.filter((n) => n.label === folderPath);
        const survivors = localInFolder.filter(
          (n) =>
            n.uuid &&
            !fetchedUuids.has(n.uuid) &&
            (n.uuid.startsWith('tmp:') ||
              recentSaveTimestamp(recents, n.account_id, n.uuid) > cutoff),
        );
        return [...out, ...survivors, ...fetched];
      });
      reconcileSelection(get(notes));
      lastRefreshAt = Date.now();
      // Mark this folder as hydrated so the Phase-C background sweep skips
      // it. The currently-selected folder is normally first to be marked.
      markFolderHydrated(accountId, folderPath);
    } catch (e) {
      if (isAuthLostError(e) && accountId) {
        await handleAuthLoss(accountId);
      } else {
        error.set(String(e));
      }
    } finally {
      isLoading.set(false);
    }
  }

  // Phase 2: cache-first read. Returns the local replica's snapshot of
  // every account's notes — fast, sync to the user, no network. Called
  // ONCE on cold start before the first network fetch lands.
  //
  // We don't reconcile here (no recently-saved protection, no merging) —
  // it's the simplest possible "show me what I last saw, instantly". The
  // subsequent loadNotes() does the proper merge with fresh server data.
  // Every fan-out in this file reads `activeAccounts`, never `accounts`. A
  // dismissed account must be inert, not merely hidden: for a Draining one a
  // `list_notes` pull runs prune_clean, folder reconciliation and
  // reconcile_one, which can mint fresh dirty conflict copies and so EXTEND
  // the very drain the user is waiting on; for an Inactive one `vertical_for`
  // refuses the command every time, and the rejection drops it from `fetched`
  // so the merge below would wipe its cached rows out of the store on every
  // poll. (`accounts` still holds both halves — see notes.ts.)
  async function loadCachedNotes() {
    const accountList = get(activeAccounts);
    if (accountList.length === 0) return;
    try {
      const results = await Promise.allSettled(
        accountList.map((a) => invoke<any[]>('list_cached_notes', { accountId: a.id })),
      );
      const cached: any[] = [];
      results.forEach((r, i) => {
        if (r.status === 'fulfilled') {
          cached.push(...r.value);
        } else {
          console.error(`list_cached_notes failed for ${accountList[i].id}:`, r.reason);
        }
      });
      if (cached.length > 0) {
        notes.set(cached);
        console.log(`[jodd] cache-first: painted ${cached.length} notes`);
      }
    } catch (e) {
      error.set(String(e));
    }
  }

  // Phase C: build the per-account stub index. Each call is paginated and
  // cheap (no body fetches), so even a 6k-note mailbox completes in
  // ~5-10s. Runs across all signed-in accounts in parallel — they have
  // separate Gmail quota buckets so there's no point serializing them.
  async function indexAllAccounts() {
    const accountList = get(activeAccounts);
    if (accountList.length === 0) return;
    const results = await Promise.allSettled(
      accountList.map((a) =>
        invoke<MessageIndex[]>('index_account', { accountId: a.id }).then(
          (idx) => [a.id, idx] as const,
        ),
      ),
    );
    const authLost: string[] = [];
    noteIndex.update((m) => {
      results.forEach((r, i) => {
        if (r.status === 'fulfilled') {
          const [id, idx] = r.value;
          m.set(id, idx);
          console.log(`[jodd] index: ${id} → ${idx.length} stubs`);
        } else {
          console.error('index_account failed:', r.reason);
          if (isAuthLostError(r.reason)) authLost.push(accountList[i].id);
        }
      });
      return m;
    });
    for (const id of authLost) await handleAuthLoss(id);
  }

  // Load every account's Jodd-local tag map (uuid → tags) from SQLite into the
  // store. Parallel across accounts; failures log but never block cold start.
  async function loadTags() {
    const accountList = get(activeAccounts);
    if (accountList.length === 0) return;
    await Promise.allSettled(
      accountList.map(async (a) => {
        try {
          const rows = await invoke<{ uuid: string; tag: string }[]>('list_note_tags', {
            accountId: a.id,
          });
          setAccountNoteTags(a.id, rows);
        } catch (e) {
          console.warn(`list_note_tags failed for ${a.id}:`, e);
        }
      }),
    );
  }

  // Phase C background sweep: walk every folder in every account, hydrating
  // one folder per tick via the cache-aware list_notes_in_folder. After the
  // first pass each folder is in SQLite, so subsequent ticks return ~instantly
  // and reconcile_one / prune_clean keep state fresh.
  //
  // Priority is "focused folder first": we always pick a folder from the
  // current account, and within the account skip ones already hydrated this
  // session. Re-clicking a folder doesn't re-hydrate (cache covers it) — the
  // 10-min full poll handles real drift.
  const SWEEP_INTERVAL_MS = 2_500;
  let sweepTimer: ReturnType<typeof setInterval> | null = null;
  let sweepBusy = false;

  // Accounts whose tag map has been reloaded since their sweep last drained.
  //
  // Tags are DERIVED FROM NOTE BODIES, and the sweep is what fetches bodies —
  // so the tag table is still filling while this runs. Both cold-start
  // `loadTags()` calls happen before any body has landed, which on a fresh
  // install leaves the sidebar showing however many tags happened to exist at
  // that instant and never correcting: measured 23 against a true 28, with
  // SQLite holding all 28 the whole time.
  //
  // Existing installs hide this completely — their `note_tags` is already
  // populated from previous sessions, so the cold-start read is complete. It
  // only ever bites the first sign-in on a new device, which is why it
  // surfaced on Android rather than on desktop.
  //
  // Same shape as the AccountStatus bug in edge #10: each half was correct on
  // its own and the missing piece was a route back to the UI. Firing on the
  // has-candidates → none edge (rather than every tick) keeps it to one extra
  // local query per drain, and clearing the mark when candidates reappear
  // means a later poll that adds folders gets a fresh reload too.
  let tagsReloadedForAccount = new Set<string>();

  function startBackgroundSweep() {
    stopBackgroundSweep();
    sweepTimer = setInterval(sweepTick, SWEEP_INTERVAL_MS);
  }
  function stopBackgroundSweep() {
    if (sweepTimer !== null) {
      clearInterval(sweepTimer);
      sweepTimer = null;
    }
  }

  async function sweepTick() {
    if (sweepBusy) return;          // previous tick still hydrating
    if ($isSaving) return;          // save path owns the cache for a beat
    if (inFlightRefresh) return;    // full refresh has higher priority
    const acctId = get(currentAccount);
    if (!acctId) return;

    // Build the candidate set for THIS account: every folder appearing in
    // the index that hasn't been hydrated yet this session.
    const idx = $noteIndex.get(acctId) ?? [];
    if (idx.length === 0) return;
    const hyd = $hydratedFolders.get(acctId) ?? new Set<string>();
    const candidates = new Set<string>();
    for (const stub of idx) {
      if (!hyd.has(stub.label)) candidates.add(stub.label);
    }
    // Already prioritized: drop the currently-focused folder so we don't
    // re-fetch what loadFolderNotes just covered. (markFolderHydrated has
    // already added it to `hyd` by now anyway, but keep this defensive.)
    candidates.delete(get(selectedFolder));
    if (candidates.size === 0) {
      // Drain complete: every folder this account knows about now has its
      // bodies in SQLite, so `note_tags` is finally whole. Reload once.
      if (!tagsReloadedForAccount.has(acctId)) {
        tagsReloadedForAccount.add(acctId);
        void loadTags();
      }
      return;
    }
    // Candidates exist again (first pass, or a poll surfaced new folders), so
    // the next drain has something new to report.
    tagsReloadedForAccount.delete(acctId);

    const next = candidates.values().next().value as string;
    sweepBusy = true;
    try {
      console.log(`[jodd] sweep: hydrating ${acctId}:${next}`);
      await loadFolderNotes(next);
    } catch (e) {
      console.error('sweep tick failed', e);
    } finally {
      sweepBusy = false;
    }
  }

  async function loadNotes() {
    lastRefreshAt = Date.now();
    const accountList = get(activeAccounts);
    if (accountList.length === 0) {
      notes.set([]);
      reconcileSelection([]);
      return;
    }
    isLoading.set(true);
    try {
      // Multi-account: fan out across every signed-in account in parallel.
      // Each note carries its account_id in the response; downstream filters
      // (NoteList, Sidebar) use it to scope display to the active account.
      // Parallel because they're independent Gmail accounts with separate
      // rate-limit buckets — sequential would multiply latency by N accounts.
      const results = await Promise.allSettled(
        accountList.map((a) => invoke<any[]>('list_notes', { accountId: a.id })),
      );
      const fetched: any[] = [];
      const authLost: string[] = [];
      results.forEach((r, i) => {
        if (r.status === 'fulfilled') {
          fetched.push(...r.value);
        } else {
          console.error(`list_notes failed for ${accountList[i].id}:`, r.reason);
          if (isAuthLostError(r.reason)) authLost.push(accountList[i].id);
        }
      });
      // Run auth-loss recovery AFTER the fetched merge below so the dead
      // account's notes are dropped from the store as part of recovery,
      // not partially-replaced here.
      queueMicrotask(async () => {
        for (const id of authLost) await handleAuthLoss(id);
      });

      // Merge: protect locally-saved notes that haven't propagated to Gmail's
      // index yet AND client-side-only `tmp:` entries. Same protection as
      // before; now applies to notes from ANY account.
      const fetchedUuids = new Set(fetched.map((n: any) => n.uuid));
      const localBefore = get(notes);
      const recents = get(recentlySavedUuids);
      const cutoff = Date.now() - 30_000;
      const survivors = localBefore.filter(
        (n) =>
          n.uuid &&
          !fetchedUuids.has(n.uuid) &&
          (
            n.uuid.startsWith('tmp:') ||
            recentSaveTimestamp(recents, n.account_id, n.uuid) > cutoff
          ),
      );
      const merged = survivors.length > 0 ? [...survivors, ...fetched] : fetched;
      notes.set(merged);
      reconcileSelection(merged);
    } catch (e) {
      error.set(String(e));
    } finally {
      isLoading.set(false);
    }
  }

  // Reconcile the editor's selection with the fetched list:
  //   - Brand-new unsaved note (id=='') → keep; lives only in memory
  //   - Saved note no longer in fetched → deleted externally. Clear.
  //   - Saved note still in fetched AND its content changed → push the fresh
  //     version into selectedNote so NoteEditor's reactive picks it up.
  //     Without this, an Apple Notes edit updates the list preview but the
  //     editor pane keeps showing the stale content until the user clicks
  //     the note again.
  function reconcileSelection(fetched: any[]) {
    const cur = get(selectedNote);
    if (!cur) return;
    if (!cur.id) return; // new unsaved — preserve user's in-memory work
    const updated = fetched.find((n) => n.uuid === cur.uuid);
    if (!updated) {
      selectedNote.set(null);
      return;
    }
    // Update the selection only if something meaningfully changed.
    // (Reference equality fails because list_notes returns fresh objects every
    // call; comparing relevant fields avoids spurious editor re-renders.)
    if (
      updated.body_html !== cur.body_html ||
      updated.title !== cur.title ||
      updated.id !== cur.id ||
      updated.date !== cur.date
    ) {
      selectedNote.set(updated);
    }
  }

  // Expose a hook other components (the refresh button in NoteList) can call.
  // Setting the store-side function pointer here keeps the side effects in App.svelte.
  // Manual refresh button skips the throttle (source='manual') but still
  // dedup's against an in-flight fetch — so spam-clicking doesn't multiply load.
  refreshNotes.set(() => requestRefresh('manual'));

  // ─── Pane widths + sidebar collapse ──────────────────────────────────────
  // Lifted up from the child components so a thin resizer between panes can
  // mutate them on drag. Clamped to a sensible range so a user can't make a
  // pane unusably narrow or push the editor off-screen.
  const SIDEBAR_MIN = 140;
  const SIDEBAR_MAX = 480;
  const NOTELIST_MIN = 180;
  const NOTELIST_MAX = 600;

  let sidebarWidth = 200;
  let noteListWidth = 240;
  let sidebarCollapsed = false;

  // Android tablet only. Independent of desktop's sidebarCollapsed — that
  // one defaults to expanded and is user-toggled per session; the tablet
  // drawer defaults to CLOSED (list+editor get the space) and reopening it
  // is a deliberate per-tap action, not a resize.
  let tabletDrawerOpen = false;

  // Global hotkeys:
  //   Cmd/Ctrl+Shift+L — opens the lesson-extraction modal.
  //   Cmd/Ctrl+N       — new note in the current folder + account.
  // No other component listens for these combos (NoteEditor uses Cmd+F,
  // AccountSettings uses Cmd+Enter, NoteList uses Cmd+A) so we can
  // claim them at window scope without conflicts. Modal own onkeydown
  // handles Escape-to-close.
  function onGlobalKey(e: KeyboardEvent) {
    const cmd = e.metaKey || e.ctrlKey;
    if (cmd && e.shiftKey && (e.key === 'L' || e.key === 'l')) {
      e.preventDefault();
      extractModalOpen.set(true);
      return;
    }
    // Cmd+N yields to any editable surface that owns the focus — same guard
    // NoteList's Cmd+A uses. Swapping $selectedNote out from under a
    // half-typed note is not what the keystroke should do mid-sentence.
    if (cmd && !e.shiftKey && !e.altKey && (e.key === 'N' || e.key === 'n')) {
      if (isEditableFocused()) return;
      e.preventDefault();
      $newNoteFn();
    }
  }

  // Mirrors the check in NoteList.onKey — an INPUT/TEXTAREA, or the editor's
  // contenteditable (or anything inside it), currently has focus.
  function isEditableFocused(): boolean {
    const ae = document.activeElement;
    return !!(
      ae &&
      (ae.tagName === 'INPUT' ||
        ae.tagName === 'TEXTAREA' ||
        ae.getAttribute('contenteditable') === 'true' ||
        ae.closest('[contenteditable="true"]'))
    );
  }

  // Accepts MouseEvent (desktop) or TouchEvent (Android tablet) — the only
  // property read off the initiating event is clientX, and a Touch object
  // carries that too, so widening the signature is enough; no separate
  // touch-specific branch is needed beyond reading .touches[0] up front.
  function startResize(e: MouseEvent | TouchEvent, which: 'sidebar' | 'notelist') {
    e.preventDefault();
    const startX = 'touches' in e ? e.touches[0].clientX : e.clientX;
    const startW = which === 'sidebar' ? sidebarWidth : noteListWidth;
    const min = which === 'sidebar' ? SIDEBAR_MIN : NOTELIST_MIN;
    const max = which === 'sidebar' ? SIDEBAR_MAX : NOTELIST_MAX;
    // Disable text selection + show resize cursor for the duration of the drag,
    // otherwise WKWebView highlights pane contents as the user moves the mouse.
    const prevUserSelect = document.body.style.userSelect;
    const prevCursor = document.body.style.cursor;
    document.body.style.userSelect = 'none';
    document.body.style.cursor = 'col-resize';
    const clamp = (clientX: number) => {
      const next = Math.max(min, Math.min(max, startW + (clientX - startX)));
      if (which === 'sidebar') sidebarWidth = next;
      else noteListWidth = next;
    };
    const onMove = (ev: MouseEvent) => clamp(ev.clientX);
    const onTouchMove = (ev: TouchEvent) => clamp(ev.touches[0].clientX);
    const onUp = () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      window.removeEventListener('touchmove', onTouchMove);
      window.removeEventListener('touchend', onUp);
      window.removeEventListener('touchcancel', onUp);
      document.body.style.userSelect = prevUserSelect;
      document.body.style.cursor = prevCursor;
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    window.addEventListener('touchmove', onTouchMove, { passive: false });
    window.addEventListener('touchend', onUp);
    window.addEventListener('touchcancel', onUp);
  }
</script>

<svelte:window onkeydown={onGlobalKey} />

<ReindexRecoveryBanner />
<LessonExtractModal bind:open={$extractModalOpen} />
<AskJoddModal bind:open={$askModalOpen} />
<About />
<WhatsNew bind:open={$whatsNewOpen} versions={$whatsNewVersions} />
<AppSettings />

{#if !$isAuthenticated}
  <AuthScreen />
{:else if $isAndroid && $androidLayoutMode === 'phone'}
  <div class="app-layout phone-layout">
    {#if $activePane === 'folders'}
      <Sidebar width={$viewportWidth} />
    {:else if $activePane === 'list'}
      <div class="phone-pane-header">
        <button
          class="phone-nav-btn"
          onclick={() => navigateToPane('folders')}
        >☰ Folders</button>
      </div>
      {#if $selectedFolder === '__TRASH__'}
        <TrashList width={$viewportWidth} />
      {:else}
        <NoteList width={$viewportWidth} />
      {/if}
    {:else}
      <div class="phone-pane-header">
        <button class="phone-nav-btn" onclick={() => history.back()}>‹ Back</button>
      </div>
      {#if $selectedFolder === '__TRASH__'}
        <TrashPreview />
      {:else}
        <NoteEditor />
      {/if}
    {/if}
  </div>
{:else if $isAndroid && $androidLayoutMode === 'tablet'}
  <div class="app-layout tablet-layout">
    {#if tabletDrawerOpen}
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <div
        class="tablet-drawer-backdrop"
        role="button"
        tabindex="0"
        aria-label="Close folders"
        onclick={() => (tabletDrawerOpen = false)}
        onkeydown={(e) => { if (e.key === 'Escape') tabletDrawerOpen = false; }}
      ></div>
      <div class="tablet-drawer">
        <Sidebar width={280} on:collapse={() => (tabletDrawerOpen = false)} />
      </div>
    {/if}
    <button
      class="tablet-drawer-toggle"
      onclick={() => (tabletDrawerOpen = !tabletDrawerOpen)}
      aria-label="Folders"
    >☰</button>
    {#if $selectedFolder === '__TRASH__'}
      <TrashList width={noteListWidth} />
    {:else}
      <NoteList width={noteListWidth} />
    {/if}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize note list"
      onmousedown={(e) => startResize(e, 'notelist')}
      ontouchstart={(e) => startResize(e, 'notelist')}
    ></div>
    {#if $selectedFolder === '__TRASH__'}
      <TrashPreview />
    {:else}
      <NoteEditor />
    {/if}
  </div>
{:else}
  <div class="app-layout">
    {#if !sidebarCollapsed}
      <Sidebar width={sidebarWidth} on:collapse={() => (sidebarCollapsed = true)} />
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <div
        class="resizer"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize sidebar"
        onmousedown={(e) => startResize(e, 'sidebar')}
      ></div>
    {:else}
      <button
        class="expand-sidebar"
        onclick={() => (sidebarCollapsed = false)}
        title="Show sidebar"
        aria-label="Show sidebar"
      >›</button>
    {/if}
    {#if $selectedFolder === '__TRASH__'}
      <TrashList width={noteListWidth} />
    {:else}
      <NoteList width={noteListWidth} />
    {/if}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize note list"
      onmousedown={(e) => startResize(e, 'notelist')}
    ></div>
    {#if $selectedFolder === '__TRASH__'}
      <TrashPreview />
    {:else}
      <NoteEditor />
    {/if}
  </div>
{/if}

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
  }

  :global(body) {
    font-family: var(--font-sans);
    overflow: hidden;
    /* Without this, every element that does not set its own `color` inherits
       the user agent's default BLACK. In light mode that looked correct by
       coincidence; in dark mode it made headings, button labels and typed
       input text render black on a dark surface. 73 rules set a background
       without setting a colour, so this is the root fix rather than 73
       patches. */
    color: var(--text);
  }

  /* Form controls do NOT inherit colour — the user agent gives button, input,
     select and textarea their own black regardless of what `body` says. They
     have to be told explicitly. Component rules that set their own colour
     (e.g. .btn-save using --text-inverse) are more specific and still win. */
  :global(button, input, select, textarea) {
    color: inherit;
  }

  /* Keyboard focus indicator. Nothing in the app defined one before, so a
     keyboard user had no idea where focus was. :focus-visible (not :focus)
     so a mouse click on a button doesn't leave a ring behind. Matters more
     here than on the web: in WKWebView on macOS, Full Keyboard Access is
     off by default, so this only shows up once the user turns it on — but
     when they do, it has to be there. Colour matches .prompt-input:focus
     in Sidebar.svelte; keep the two in sync. */
  :global(:focus-visible) {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
    border-radius: 3px;
  }

  /* The note body is a full-pane contenteditable whose focus signal is the
     caret — NoteEditor already sets `outline: none` on it. That rule and the
     one above have equal specificity, so which wins would depend on the order
     Svelte emits the two component stylesheets. Pin it explicitly instead. */
  :global([contenteditable='true']:focus-visible) {
    outline: none;
  }

  /* Respect the OS "reduce motion" setting. Jodd runs two continuous spin
     animations (NoteList's ⟳ icon and the first-load spinner); for users who
     get motion sickness or vestibular symptoms, a permanently rotating
     element is a real problem. Those two have non-animated busy states
     under this query — see NoteList.svelte. */
  @media (prefers-reduced-motion: reduce) {
    :global(*), :global(*::before), :global(*::after) {
      animation-duration: 0.01ms !important;
      animation-iteration-count: 1 !important;
      transition-duration: 0.01ms !important;
    }
  }

  .app-layout {
    display: flex;
    height: 100vh;
    overflow: hidden;
  }

  .phone-layout {
    flex-direction: column;
  }

  .phone-pane-header {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    /* Reserve status-bar space so the button/title aren't hidden behind the
       clock/signal/battery icons (reported on-device via screenshot,
       Galaxy S23 FE). env(safe-area-inset-top) falls back to 0 in WebViews
       that don't populate it, so max(8px, ...) always keeps at least the
       original 8px. */
    padding: max(8px, env(safe-area-inset-top)) 12px 8px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-sidebar);
  }

  .phone-nav-btn {
    background: none;
    border: none;
    color: var(--text);
    font-size: var(--size-md);
    padding: 4px 8px;
    cursor: pointer;
  }

  .tablet-layout {
    position: relative;
  }

  .tablet-drawer-backdrop {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    z-index: 10;
    border: none;
  }

  .tablet-drawer {
    position: fixed;
    top: 0;
    left: 0;
    bottom: 0;
    z-index: 11;
    box-shadow: var(--shadow-menu);
  }

  .tablet-drawer-toggle {
    position: absolute;
    /* Same status-bar clearance as .phone-pane-header / Sidebar's
       .sidebar-header — this button sits at the very top of the tablet
       layout's Folders toggle and was missed by the original safe-area
       pass, which only covered the phone header and footer. */
    top: max(8px, env(safe-area-inset-top));
    left: 8px;
    z-index: 5;
    background: var(--surface-sidebar);
    border: 1px solid var(--border);
    border-radius: 4px;
    color: var(--text);
    font-size: var(--size-md);
    padding: 4px 8px;
    cursor: pointer;
  }

  /* Thin draggable divider between panes. 5px wide so it's grabbable
     without being visually loud; col-resize cursor signals the affordance. */
  .resizer {
    flex: 0 0 5px;
    width: 5px;
    cursor: col-resize;
    background: transparent;
    border-left: 1px solid transparent;
    border-right: 1px solid transparent;
    transition: background 0.15s;
  }
  .resizer:hover,
  .resizer:active {
    background: var(--accent-wash-strong);
  }

  /* Narrow strip shown when the sidebar is collapsed — one click to bring
     the sidebar back. Matches the sidebar's beige so it reads as the same
     surface, just minimized. */
  .expand-sidebar {
    flex: 0 0 18px;
    width: 18px;
    background: var(--surface-sidebar);
    border: none;
    border-right: 1px solid var(--border);
    cursor: pointer;
    color: var(--text-muted);
    font-size: var(--size-md);
    padding: 0;
  }
  .expand-sidebar:hover {
    background: var(--surface-sidebar);
    color: var(--text);
  }
</style>
