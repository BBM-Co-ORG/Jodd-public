<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import AuthScreen from './lib/components/AuthScreen.svelte';
  import Sidebar from './lib/components/Sidebar.svelte';
  import NoteList from './lib/components/NoteList.svelte';
  import NoteEditor from './lib/components/NoteEditor.svelte';
  import { isAuthenticated, notes, isLoading, isSaving, error, refreshNotes, accounts, currentAccount, selectedNote, selectedFolder, recentlySavedUuids, recentSaveTimestamp, noteIndex, hydratedFolders, markFolderHydrated, selectedTags, setAccountNoteTags } from './lib/stores/notes';
  import type { MessageIndex } from './lib/types';
  import { get } from 'svelte/store';
  import type { Account } from './lib/types';

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
      }
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
        const list = get(accounts);
        // Pin AND tag sidecar pulls run in parallel — they hit the same
        // meta_label on Gmail but read different sidecar shapes (pin uses
        // metadata-only Subject fetch, tags fetches full body). The two
        // can't conflict in SQLite either: pin writes notes.pinned/meta_msg_id;
        // tags writes notes.tags_meta_msg_id + the note_tags rows. Disjoint
        // columns, disjoint subject prefixes, full parallel safety.
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

    // All three focus paths route through requestRefresh('focus') so the
    // inFlightRefresh + 2s throttle deduplicates the burst that fires when
    // Cmd-Tab back triggers onFocusChanged + tauri://focus + visibilitychange
    // within the same animation frame.
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
    const authed = await invoke<boolean>('is_authenticated');
    isAuthenticated.set(authed);

    await listen<string>('oauth-success', async () => {
      isAuthenticated.set(true);
    });

    await listen<string>('oauth-error', (event) => {
      error.set(event.payload);
    });
  });

  onDestroy(() => {
    stopPolling();
    cancelFocusSettle();
    cancelFolderSettle();
    unlistenFocus?.();
  });

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
    let accountId: string | null = null;
    currentAccount.subscribe((v) => (accountId = v))();
    if (!accountId || !folderPath) return;
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
  async function loadCachedNotes() {
    const accountList = get(accounts);
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
    const accountList = get(accounts);
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
    const accountList = get(accounts);
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
    if (candidates.size === 0) return;

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
    const accountList = get(accounts);
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

  function startResize(e: MouseEvent, which: 'sidebar' | 'notelist') {
    e.preventDefault();
    const startX = e.clientX;
    const startW = which === 'sidebar' ? sidebarWidth : noteListWidth;
    const min = which === 'sidebar' ? SIDEBAR_MIN : NOTELIST_MIN;
    const max = which === 'sidebar' ? SIDEBAR_MAX : NOTELIST_MAX;
    // Disable text selection + show resize cursor for the duration of the drag,
    // otherwise WKWebView highlights pane contents as the user moves the mouse.
    const prevUserSelect = document.body.style.userSelect;
    const prevCursor = document.body.style.cursor;
    document.body.style.userSelect = 'none';
    document.body.style.cursor = 'col-resize';
    const onMove = (ev: MouseEvent) => {
      const next = Math.max(min, Math.min(max, startW + (ev.clientX - startX)));
      if (which === 'sidebar') sidebarWidth = next;
      else noteListWidth = next;
    };
    const onUp = () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      document.body.style.userSelect = prevUserSelect;
      document.body.style.cursor = prevCursor;
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  }
</script>

{#if !$isAuthenticated}
  <AuthScreen />
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
    <NoteList width={noteListWidth} />
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize note list"
      onmousedown={(e) => startResize(e, 'notelist')}
    ></div>
    <NoteEditor />
  </div>
{/if}

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
  }

  :global(body) {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
    overflow: hidden;
  }

  .app-layout {
    display: flex;
    height: 100vh;
    overflow: hidden;
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
    background: rgba(74, 144, 226, 0.25);
  }

  /* Narrow strip shown when the sidebar is collapsed — one click to bring
     the sidebar back. Matches the sidebar's beige so it reads as the same
     surface, just minimized. */
  .expand-sidebar {
    flex: 0 0 18px;
    width: 18px;
    background: #f0ebe0;
    border: none;
    border-right: 1px solid #ddd8cc;
    cursor: pointer;
    color: #888;
    font-size: 14px;
    padding: 0;
  }
  .expand-sidebar:hover {
    background: #e9e1cf;
    color: #333;
  }
</style>
