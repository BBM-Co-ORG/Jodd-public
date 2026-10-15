<script lang="ts">
  import AiReceipts from "./AiReceipts.svelte";
  import { onMount, tick, untrack } from 'svelte';
  import { get } from 'svelte/store';
  import { listen } from '@tauri-apps/api/event';
  import { invoke } from '@tauri-apps/api/core';
  import { accounts, accountDisplayById, currentAccount, selectedFolder, error } from '../stores/notes';
  import { appSettingsOpen } from '../stores/ui';
  import { renderAnswer, type CitedNote } from './askCitations';
  import { askScope, isRealFolderSelection, type AskScope } from './askScope';
  import { revealLatestQuestion } from './askScroll';
  import AskHistory from './AskHistory.svelte';
  import { statsLine, describeScope, type StoredConversation, type AskResume } from './askHistory';
  import { isConsentRefusal, allowAiForAccount, DATA_ALLOWED_LABEL } from '../aiConsent';

  // Bindable so the parent can both open and observe close, mirroring
  // LessonExtractModal's `open` prop.
  let { open = $bindable(false) }: { open: boolean } = $props();

  type Turn = { role: 'user' | 'assistant'; content: string };
  type AskAnswer = {
    markdown: string;
    cited: CitedNote[];
    notes_in_scope: number;
    notes_considered: number;
    notes_used: number;
    trimmed: boolean;
    dropped_citations: number;
    conversation_id: string | null;
  };
  type Rendered = Turn & { html?: string; stats?: string; error?: boolean; cancelled?: boolean };

  let turns = $state<Rendered[]>([]);
  let turnsEl = $state<HTMLElement | undefined>();
  let input = $state('');
  let busy = $state(false);
  let requestId = $state('');
  let receiptRequestId = $state('');
  // Default: current account (spec §5.6) — "all accounts" spans thousands of
  // notes, where the pre-filter thins hardest and answer quality is least
  // predictable.
  let scopeKind = $state<'account' | 'folder' | 'all'>(isRealFolderSelection(get(selectedFolder)) ? 'folder' : 'account');
  let folderScopeAvailable = $derived(isRealFolderSelection($selectedFolder));

  type Session = { session_id: string; destination: string; scope: string };
  let session = $state<Session | null>(null);
  let policyError = $state('');
  let providerMissing = $state(false);
  let consentAccount = $state(''); // the account a consent refusal in policyError is for
  let allowError = $state(''); // why the last Allow failed, shown inside the consent callout
  let consentName = $derived(accountDisplayById($accounts, consentAccount));
  let policyRevision = $state(0);
  let generation = 0;
  // The ask_jodd call in flight, and the one a Cancel left still unwinding.
  // The backend keeps the session busy until that call returns, so the next
  // question waits for it rather than being refused as "already running".
  let inFlight: Promise<unknown> | null = null;
  let settling: Promise<unknown> | null = null;
  let wasOpen = false;
  let accountEligibility = $derived(JSON.stringify($accounts.map(a => [a.id, a.status, a.blocked_reason])));

  // Saved conversations (spec §7). `loaded` and `conversationId` describe what
  // is on screen, not the backend session, so invalidate() keeps them.
  let loaded = $state<StoredConversation | null>(null);  // the opened saved conversation
  let conversationId = $state<string | null>(null);     // set by load or by the first saved answer
  let scopeOverride = $state<AskScope | null>(null);    // "Start new conversation with this scope"
  let resumeConfirm = $state<{ previous: string; current: string } | null>(null);
  let readOnlyReason = $state('');
  let historyKey = $state(0);
  let historyEnabled = $state(true);
  let showHistory = $state(false);                      // phone: list instead of chat
  // Bumped by every open / new / reset, so a slow load_ask_conversation that an
  // later click has superseded cannot overwrite what that click chose.
  let navigation = 0;

  function renderStored(c: StoredConversation): Rendered[] {
    return c.turns.flatMap((t) => [
      { role: 'user' as const, content: t.question },
      { role: 'assistant' as const, content: t.answer_markdown, html: renderAnswer(t.answer_markdown, t.cited), stats: statsLine(t.stats) },
    ]);
  }

  function invalidate() {
    generation++;
    const previous = session;
    session = null;
    if (requestId) void invoke('cancel_ask', { requestId }).catch(() => {});
    if (previous) void invoke('end_ask', { sessionId: previous.session_id }).catch(() => {});
    turns = [];
    busy = false;
    requestId = '';
    receiptRequestId = '';
    resumeConfirm = null;
    readOnlyReason = '';
  }

  async function prepare(scope: AskScope, acceptDestination?: string) {
    invalidate();
    policyError = '';
    consentAccount = '';
    allowError = '';
    providerMissing = false;
    const ownGeneration = generation;
    try {
      if (loaded) {
        // invalidate() cleared the turns; a saved conversation keeps them on
        // screen through a re-check, and only the session is re-established.
        turns = renderStored(loaded);
        if (loaded.archived) { readOnlyReason = 'This conversation is archived. Unarchive it to continue.'; return; }
        const r = await invoke<AskResume>('resume_ask', { conversationId: loaded.id, acceptDestination });
        if (ownGeneration !== generation) {
          if (r.kind === 'ready') void invoke('end_ask', { sessionId: r.session.session_id }).catch(() => {});
          return;
        }
        if (r.kind === 'ready') session = r.session;
        else if (r.kind === 'needs_confirm') resumeConfirm = { previous: r.previous, current: r.current };
        else readOnlyReason = r.reason;
        return;
      }
      // begin_ask always starts a new conversation, so whatever an earlier
      // session saved is no longer the one on screen.
      conversationId = null;
      const result = await invoke<Session>('begin_ask', { scope });
      if (ownGeneration !== generation) {
        void invoke('end_ask', { sessionId: result.session_id }).catch(() => {});
        return;
      }
      if (!result?.session_id) throw new Error('AI permission could not be verified. Reopen Ask Jodd.');
      session = result;
    } catch (e) {
      if (ownGeneration !== generation) return;
      policyError = String(e);
      consentAccount = isConsentRefusal(policyError) && 'account_id' in scope ? scope.account_id : '';
      providerMissing = policyError.includes('needs an LLM provider');
    }
  }

  // Privacy PR3. The re-check is not ours to trigger: the backend emits
  // ai-policy-changed on success, and the listener below bumps policyRevision.
  async function allowAi() {
    const acct = consentAccount;
    if (!acct) return;
    const own = generation;
    const previous = policyError;
    consentAccount = '';
    policyError = '';
    allowError = '';
    try {
      await allowAiForAccount(acct);
    } catch (e) {
      const message = `Could not allow AI for this account: ${e}`;
      if (own === generation) {
        policyError = previous;
        consentAccount = acct;
        allowError = message;
      } else {
        // The backend emits ai-policy-changed before it saves, so a failed save
        // has already re-run prepare() (generation++): the surface shows the
        // same refusal again and this failure would otherwise vanish.
        error.set(message);
      }
    }
  }

  $effect(() => {
    const scope = activeScope;
    const visible = open;
    policyRevision;
    accountEligibility;
    // activeScope already depends on the `loaded` object itself (it reads
    // `loaded.scope`), so assigning a new conversation — even one with the
    // same id — re-runs this effect. That is why a rename mutates
    // `loaded.title` in place instead of replacing the object. The id read
    // below documents the intent; it adds no dependency activeScope lacks.
    loaded?.id;
    untrack(() => {
      if (visible && !wasOpen) {
        scopeKind = isRealFolderSelection(get(selectedFolder)) ? 'folder' : 'account';
        readHistoryEnabled();
      }
      wasOpen = visible;
      if (visible && scope) void prepare(scope);
      else invalidate();
    });
  });

  onMount(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen('ai-policy-changed', () => { policyRevision++; }).then((unlisten) => {
      if (disposed) unlisten(); else stop = unlisten;
    }).catch(() => { policyError = 'AI change notifications unavailable. Reopen Ask before continuing.'; invalidate(); });
    return () => { disposed = true; stop?.(); invalidate(); };
  });

  // Re-read on every open, not once per mount: the modal stays mounted for
  // the app's lifetime and the setting can change in App Settings meanwhile.
  // Unknown → enabled: only an explicit false turns the copy to "discarded".
  let historyProbe = 0;
  function readHistoryEnabled() {
    const own = ++historyProbe;
    void invoke<boolean | null>('get_ask_history_enabled')
      .then((v) => { if (own === historyProbe && open) historyEnabled = v !== false; })
      .catch(() => {});
  }

  // `force` is for an archive toggle of the open conversation, which must
  // re-check even though its session is live.
  async function openConversation(id: string, force = false) {
    // Re-clicking the conversation already open and live is a no-op: tearing
    // its session down would drop a question in flight for nothing.
    if (!force && id === loaded?.id && session && !readOnlyReason && !resumeConfirm) {
      showHistory = false;
      return;
    }
    showHistory = false;
    const own = ++navigation;
    try {
      const c = await invoke<StoredConversation>('load_ask_conversation', { conversationId: id });
      if (own !== navigation) return; // a later open / new won
      scopeOverride = null;
      conversationId = c.id;
      // A new object, even for the same id, changes activeScope's reference,
      // so the effect re-runs prepare(), which resumes.
      loaded = c;
    } catch (e) {
      if (own !== navigation) return;
      error.set(`Could not open the conversation: ${e}`);
    }
  }

  function startNew(scope: AskScope | null = null) {
    navigation++;
    showHistory = false;
    loaded = null;
    conversationId = null;
    scopeOverride = scope;
    policyRevision++; // force a fresh begin_ask even if activeScope is unchanged
  }

  function onHistoryChanged(id: string, change: 'deleted' | 'archived' | 'unarchived' | 'renamed') {
    if (id !== conversationId) return;
    if (change === 'deleted') startNew();
    else if (change === 'archived' || change === 'unarchived') void openConversation(id, true);
    // 'renamed': nothing — the modal never renders the title, and replacing
    // `loaded` would re-run the effect and re-resume.
  }

  // Local-first (spec §7): flip first, IPC second, roll back on failure.
  async function unarchiveLoaded() {
    const c = loaded;
    if (!c || !c.archived) return;
    const own = navigation;
    c.archived = false;
    try {
      await invoke('set_ask_conversation_archived', { conversationId: c.id, archived: false });
    } catch (e) {
      if (loaded?.id === c.id) c.archived = true;
      error.set(`Could not unarchive the conversation: ${e}`);
      return;
    }
    historyKey++;
    // Resume only if nothing else was opened meanwhile.
    if (own === navigation && loaded?.id === c.id && activeScope) void prepare(activeScope);
  }

  function openAppSettings() {
    open = false;
    resetForm();
    appSettingsOpen.set(true);
  }

  // RFC4122 v4 UUID generator (no external dep) — matches
  // LessonExtractModal's newRequestId helper.
  function newRequestId(): string {
    return globalThis.crypto?.randomUUID?.() ?? `ask-${Date.now()}-${Math.random()}`;
  }

  // `null` means the selected scope is not expressible — no current account for
  // an account-anchored scope. Gating the Ask button on it is the whole point:
  // askScope refuses to invent an account_id, so the UI must refuse to send.
  // 'All accounts' needs no account and stays available, which is why the hint
  // below points at it.
  // A loaded conversation pins its own scope; "Start new conversation with this
  // scope" pins one until the selector is changed.
  let activeScope = $derived(
    loaded ? loaded.scope : scopeOverride ?? askScope(scopeKind, $currentAccount, $selectedFolder),
  );

  async function send() {
    const q = input.trim();
    // Captured once: an unsendable scope must not consume the user's question.
    const scope = activeScope;
    if (!q || busy || !scope || !session) return;
    const ownGeneration = generation;
    const ownRequest = newRequestId();
    const sessionId = session.session_id;
    // invalidate() ends the conversation and cancel() ends only this request;
    // both clear requestId, so either one makes this request's result stale.
    const live = () => ownGeneration === generation && requestId === ownRequest;
    // Brings this question to the top of the conversation (see askScroll.ts).
    // It measures the turn just pushed, so it waits for the DOM — and a request
    // cancelled or invalidated meanwhile must not move the view.
    const reveal = async () => {
      await tick();
      if (live() && turnsEl) revealLatestQuestion(turnsEl);
    };
    input = '';
    turns.push({ role: 'user', content: q });
    turns = turns;
    busy = true;
    requestId = ownRequest;
    receiptRequestId = ownRequest;
    // Not awaited: the question must not wait on a scroll.
    void reveal();

    try {
      if (settling) await settling;
      if (!live()) return;
      const call = invoke<AskAnswer>('ask_jodd', {
        sessionId,
        question: q,
        requestId: ownRequest,
      });
      inFlight = call.catch(() => {});
      const a = await call;
      if (!live()) return;
      turns.push({
        role: 'assistant',
        content: a.markdown,
        html: renderAnswer(a.markdown, a.cited),
        stats: statsLine(a),
      });
      turns = turns;
      if (a.conversation_id) {
        // Every saved answer moves its conversation to the top of the rail.
        conversationId = a.conversation_id;
        historyKey++;
        // A re-check re-renders a loaded conversation from `loaded.turns`, so
        // what was asked since it was opened has to be there too.
        if (loaded?.id === a.conversation_id) {
          loaded.turns.push({
            seq: loaded.turns.length + 1, question: q, answer_markdown: a.markdown, cited: a.cited,
            read: [], stats: a, created_at: Date.now(),
          });
        }
      }
      // Awaited, here and below: `finally` clears requestId, and live() with it.
      await reveal();
    } catch (e) {
      if (!live()) return;
      turns.push({ role: 'assistant', content: String(e), error: true });
      turns = turns;
      await reveal();
    } finally {
      if (live()) { busy = false; requestId = ''; }
    }
  }

  // Stops the question in flight and nothing else: the session and the turns
  // before it stay, because the backend keeps the history a cancelled request
  // never joined. Scope, policy and close still go through invalidate().
  // receiptRequestId stays too — what was already sent remains on record.
  function cancel() {
    if (!requestId) return;
    void invoke('cancel_ask', { requestId }).catch(() => {});
    const pending = turns.at(-1);
    if (pending?.role === 'user') pending.cancelled = true;
    settling = inFlight;
    busy = false;
    requestId = '';
  }

  function resetForm() {
    navigation++;
    turns = [];
    input = '';
    loaded = null;
    conversationId = null;
    scopeOverride = null;
    showHistory = false;
    invalidate();
    scopeKind = isRealFolderSelection(get(selectedFolder)) ? 'folder' : 'account';
  }

  function close() {
    if (busy) {
      // Mirrors LessonExtractModal's close(): a click while busy cancels the
      // in-flight request instead of leaving it orphaned server-side, and
      // keeps the conversation — a second click closes.
      cancel();
      return;
    }
    open = false;
    resetForm();
  }

  function onKey(e: KeyboardEvent) {
    if (!open) return;
    if (e.key === 'Escape') close();
  }

  function onInputKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  // Citation chips are rendered from a string via {@html}, so they cannot
  // carry Svelte handlers directly — delegate from the container instead.
  function onAnswerClick(e: MouseEvent) {
    const el = (e.target as HTMLElement).closest('.cite-chip') as HTMLElement | null;
    if (!el) return;
    const uuid = el.dataset.uuid;
    const accountId = el.dataset.account;
    if (uuid && accountId) {
      window.dispatchEvent(
        new CustomEvent('jodd:open-note', { detail: { uuid, accountId } }),
      );
      // Not close(): while busy, close() cancels the in-flight request and
      // returns WITHOUT clearing `open`, which would leave the just-opened
      // note behind a modal that never closed. A citation click always means
      // "take me to this note" — cancel any in-flight ask as a side effect,
      // but unconditionally close.
      cancel();
      open = false;
      resetForm();
    }
  }
</script>

<svelte:window on:keydown={onKey} />

{#if open}
  <div
    class="backdrop"
    role="presentation"
    onclick={(e) => { if (e.target === e.currentTarget) close(); }}
  >
    <div class="modal ask-modal" class:history-open={showHistory} role="dialog" aria-modal="true" aria-labelledby="ask-title">
      <header class="ask-header">
        <h2 id="ask-title">Ask Jodd</h2>
        <button type="button" class="history-toggle" onclick={() => (showHistory = !showHistory)}>History</button>
        {#if loaded || scopeOverride}
          <span class="scope-locked">Scope: {describeScope(loaded?.scope ?? scopeOverride!)}</span>
        {:else}
          <select class="field" bind:value={scopeKind} onchange={() => (scopeOverride = null)}>
            <option value="account">This account</option>
            <option value="folder" disabled={!folderScopeAvailable}>This folder and below</option>
            <option value="all">All accounts</option>
          </select>
        {/if}
      </header>

      <div class="ask-body" class:show-history={showHistory}>
        <AskHistory activeId={conversationId} refreshKey={historyKey}
          onSelect={(id) => openConversation(id)} onNew={() => startNew()} onChanged={onHistoryChanged} />
        <div class="ask-chat">
          <div class="hint" aria-live="polite">
            {#if session}
              <p>Destination: {session.destination}</p>
              <p>Scope: {session.scope}. Sends your questions, eligible conversation history,
                candidate titles/tags/folders, then selected note bodies.</p>
            {:else if consentAccount}
              <!-- The refusal itself is explained by the callout below; its raw text
                   ("provider not configured: …") told the user nothing. -->
            {:else if policyError && !providerMissing}
              <p role="alert">{policyError}</p>
              <button type="button" onclick={() => { policyRevision++; }}>Retry permission check</button>
            {:else if !providerMissing && activeScope && !resumeConfirm && !readOnlyReason}
              <p>Checking AI permission and destination…</p>
            {/if}
            <p>Data already sent cannot be recalled. Changing scope or AI settings starts a new conversation.
              Pasted text has no account provenance. External MCP clients use their separate access rules.</p>
          </div>
          <div class="ask-turns" bind:this={turnsEl} onclick={onAnswerClick} role="presentation">
            {#if providerMissing}
              <div class="no-provider">
                <p><strong>Ask Jodd needs an LLM provider.</strong></p>
                <p>
                  It reads your notes and asks a model to answer from them, so it
                  needs one configured at the app level — separate from any
                  per-account provider, because Ask Jodd can search across all of
                  your accounts at once.
                </p>
                <button type="button" class="primary" onclick={openAppSettings}>
                  Open App Settings
                </button>
              </div>
            {:else if consentAccount}
              <!-- Live 2026-10-14 (Tab S7): a raw refusal plus a link-styled button
                   read as an error, not as a question, and the enabled-looking
                   question box below hid that nothing could be asked. Never
                   auto-granted — the button is the consent. A resumed saved
                   conversation reaches this too: prepare() takes consentAccount
                   from loaded.scope when resume_ask refuses for consent. -->
              <div class="needs-consent" role="alert" aria-labelledby="ask-consent-title">
                <p id="ask-consent-title"><strong>Ask Jodd needs permission to read this account’s notes.</strong></p>
                <p>
                  AI data access is off for <strong>{consentName}</strong> on this device.
                  Allowing it lets Ask Jodd send your questions, candidate
                  titles/tags/folders, then selected note bodies to the AI provider
                  set in App Settings. Its exact destination is shown here before
                  you can ask anything.
                </p>
                <p class="fine">
                  This ticks “{DATA_ALLOWED_LABEL}” in Account Settings, so
                  extraction and enrichment may use it too. You can untick it there.
                </p>
                <button type="button" class="primary" onclick={allowAi}>Allow AI for {consentName}</button>
                {#if allowError}<p class="allow-error">{allowError}</p>{/if}
              </div>
            {:else if !activeScope}
              <p class="hint">
                No account is selected, so there is nothing to scope this to. Pick an
                account in the sidebar, or switch the scope above to
                <strong>All accounts</strong>.
              </p>
            {:else if turns.length === 0}
              <p class="hint">
                Ask a question about your notes. Answers cite the notes they came from.
                {#if historyEnabled}
                  Conversations are kept on this device under History — turn this off in App Settings.
                {:else}
                  History is off, so closing this window discards the conversation.
                {/if}
              </p>
            {/if}
            {#each turns as t}
              <div class="ask-turn {t.role}" class:error={t.error}>
                {#if t.role === 'user'}
                  <p>{t.content}</p>
                  {#if t.cancelled}<p class="cancelled-note">Cancelled — follow-up questions won't include it.</p>{/if}
                {:else if t.html}
                  <!-- t.html only ever comes from renderAnswer(), which escapes
                       every character of the model's markdown first and then
                       writes only a fixed, attribute-free tag set plus
                       citation-chip markup (see askCitations.ts) — safe to
                       {@html}. Stored turns go through the same renderAnswer().
                       The error branch below never sets t.html, so a
                       raw backend error string (which can embed an upstream
                       HTTP response body or subprocess stderr — see
                       src-tauri/src/llm/http.rs UpstreamError and
                       llm/agent_cli.rs UpstreamError) can never reach {@html}. -->
                  <div class="answer">{@html t.html}</div>
                  {#if t.stats}<p class="stats">{t.stats}</p>{/if}
                {:else}
                  <!-- Plain Svelte text interpolation — auto-escaped, not {@html}. -->
                  <div class="answer">{t.content}</div>
                {/if}
              </div>
            {/each}
            {#if busy}
              <div class="ask-turn assistant busy">
                <span>Reading your notes…</span>
                <button type="button" onclick={cancel}>Cancel</button>
              </div>
            {/if}
          </div>

          <AiReceipts requestId={receiptRequestId} />
          {#if resumeConfirm}
            <div class="resume-confirm" role="alert">
              <p>Earlier questions and answers will be sent to {resumeConfirm.current}. Continue?</p>
              <button type="button" class="primary" onclick={() => { const c = resumeConfirm!.current; if (activeScope) void prepare(activeScope, c); }}>Continue</button>
              <button type="button" onclick={() => startNew()}>Not now</button>
            </div>
          {/if}
          {#if readOnlyReason}
            <div class="read-only" role="status">
              <p>{readOnlyReason}</p>
              {#if loaded?.archived}
                <button type="button" class="primary" onclick={unarchiveLoaded}>Unarchive</button>
              {/if}
              <button type="button" onclick={() => startNew(loaded ? $state.snapshot(loaded.scope) : null)}>Start new conversation with this scope</button>
            </div>
          {:else}
            <label class="ask-input-label">
              <textarea
                class="field"
                bind:value={input}
                onkeydown={onInputKeydown}
                placeholder={providerMissing
                  ? 'Configure an LLM provider in App Settings to ask questions.'
                  : consentAccount
                  ? 'Allow AI for this account above to ask questions.'
                  : 'Ask about your notes… (Enter to send, Shift+Enter for a newline)'}
                disabled={busy || providerMissing || !session}
              ></textarea>
            </label>
          {/if}

          <div class="actions">
            <button type="button" onclick={() => startNew()}>New conversation</button>
            <button type="button" onclick={close}>
              {busy ? 'Cancel' : 'Close'}
            </button>
            <button
              type="button"
              onclick={send}
              disabled={busy || providerMissing || !session || !activeScope || !input.trim()}
              class="primary"
            >
              {busy ? 'Asking…' : 'Ask'}
            </button>
          </div>
        </div>
      </div>
    </div>
  </div>
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
    padding: 24px; border-radius: 8px;
    box-shadow: var(--shadow-modal);
  }
  .ask-modal {
    box-sizing: border-box;
    width: min(1000px, calc(100vw - 24px)); max-height: calc(100dvh - 24px);
    overflow-y: auto;
    display: flex; flex-direction: column;
  }
  .ask-header {
    display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between;
    gap: 12px; margin: 0 0 8px;
  }
  h2 { margin: 0; }
  .ask-body { display: flex; gap: 16px; min-height: 0; flex: 1; }
  .ask-body :global(.ask-rail) { flex: 0 0 220px; border-right: 1px solid var(--border-subtle); padding-right: 12px; max-height: 70vh; }
  .ask-chat { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .history-toggle { display: none; }
  /* Unclassed buttons get the WebView's native look on Android — style the
     header toggle and the banners' secondary buttons like the footer's. */
  .history-toggle, .resume-confirm button:not(.primary), .read-only button:not(.primary) {
    background: var(--surface-panel); color: inherit; border: 1px solid var(--border);
    border-radius: 4px; padding: 6px 12px; cursor: pointer; font: inherit;
  }
  .scope-locked { font-size: var(--size-sm); color: var(--text-muted); }
  .resume-confirm, .read-only {
    border: 1px solid var(--border); border-radius: 4px; padding: 8px 12px; margin: 8px 0 0;
    display: flex; flex-wrap: wrap; gap: 8px; align-items: center; font-size: var(--size-sm);
  }
  .resume-confirm p, .read-only p { margin: 0; flex: 1 1 100%; }
  @media (max-width: 767px) {
    .history-toggle { display: inline-block; }
    /* Spec §7: on a phone the list takes the whole screen. */
    .ask-modal.history-open { height: calc(100dvh - 24px); }
    .ask-body :global(.ask-rail) { display: none; }
    .ask-body.show-history :global(.ask-rail) { display: flex; flex: 1; border: none; padding: 0; max-height: none; }
    .ask-body.show-history .ask-chat { display: none; }
  }
  .hint { color: var(--text-muted); font-size: var(--size); margin: 0 0 16px; }
  .field {
    padding: 6px 8px; font: inherit; box-sizing: border-box;
    background: var(--surface-panel); color: inherit;
    border: 1px solid var(--border); border-radius: 4px;
  }
  select.field { flex: 0 0 auto; max-width: 100%; }
  .ask-turns {
    flex: 1; overflow-y: auto; min-height: 160px; max-height: 50vh;
    border: 1px solid var(--border-subtle); border-radius: 4px;
    padding: 12px; margin: 8px 0; background: var(--surface-panel);
  }
  .ask-turn { margin: 0 0 16px; }
  .ask-turn:last-child { margin-bottom: 0; }
  .ask-turn p { margin: 0; line-height: var(--leading); }
  .ask-turn.user p {
    font-weight: 600; color: var(--text);
  }
  .ask-turn.user .cancelled-note {
    margin: 4px 0 0; font-weight: normal;
    font-size: var(--size-xs); color: var(--text-muted);
  }
  .ask-turn.assistant .answer {
    line-height: var(--leading); color: var(--text);
  }
  /* The tags renderAnswer() can emit. {@html} content is outside Svelte's
     scoping, hence :global. */
  .answer :global(p) { margin: 0 0 8px; }
  .answer :global(ul), .answer :global(ol) { margin: 0 0 8px; padding-left: 22px; }
  .answer :global(li) { margin: 2px 0; }
  .answer :global(li > ul), .answer :global(li > ol) { margin: 2px 0 0; }
  .answer > :global(:last-child) { margin-bottom: 0; }
  .answer :global(code) {
    font-family: var(--font-mono); font-size: 0.92em;
    background: var(--surface-panel); border-radius: var(--radius-sm); padding: 0 3px;
  }
  .answer :global(pre) {
    margin: 0 0 8px; padding: 8px; overflow-x: auto;
    background: var(--surface-panel); border-radius: var(--radius-sm);
  }
  .answer :global(pre code) { background: none; padding: 0; }
  .answer :global(hr) { border: none; border-top: 1px solid var(--border); margin: 8px 0; }
  .ask-turn.assistant.error .answer {
    color: var(--danger);
    /* Error text is plain interpolation, not rendered — keep its line breaks
       (an upstream body or stderr is often multi-line). */
    white-space: pre-wrap;
  }
  .ask-turn.busy {
    display: flex; align-items: center; gap: 8px;
    color: var(--text-muted); font-size: var(--size-sm);
  }
  .ask-turn.busy button {
    background: var(--surface-panel); border: 1px solid var(--border);
    border-radius: 4px; padding: 2px 8px; cursor: pointer;
  }
  .stats {
    margin: 6px 0 0; font-size: var(--size-xs); color: var(--text-muted);
  }
  .ask-input-label {
    display: block; margin: 8px 0 0;
  }
  textarea.field {
    width: 100%; min-height: 64px; resize: vertical;
    font-family: inherit; font-size: var(--size);
  }
  .actions {
    display: flex; gap: 8px; justify-content: flex-end; margin-top: 12px;
  }
  .actions button {
    background: var(--surface-panel); color: inherit; border: 1px solid var(--border);
    border-radius: 4px; padding: 8px 16px; cursor: pointer;
  }
  .actions button.primary {
    background: var(--accent-action); color: var(--text-inverse); border: none;
    padding: 8px 16px; border-radius: 4px; cursor: pointer;
  }
  .actions button.primary:disabled { background: var(--border-strong); }

  /* The `.actions button.primary` rule above is scoped to the footer, so the
     empty state's button needs its own. */
  .no-provider { color: var(--text-muted); font-size: var(--size); }
  .no-provider p { margin: 0 0 10px; line-height: var(--leading); }
  .no-provider strong { color: var(--text); }
  .no-provider button.primary, .resume-confirm button.primary, .read-only button.primary {
    background: var(--accent-action); color: var(--text-inverse); border: none;
    padding: 8px 16px; border-radius: 4px; cursor: pointer; font: inherit;
  }

  .needs-consent { color: var(--text-muted); font-size: var(--size); }
  .needs-consent p { margin: 0 0 10px; line-height: var(--leading); overflow-wrap: anywhere; }
  .needs-consent strong { color: var(--text); }
  .needs-consent .fine { font-size: var(--size-sm); }
  .needs-consent button.primary {
    background: var(--accent-action); color: var(--text-inverse); border: none;
    padding: 10px 16px; border-radius: 4px; cursor: pointer; font: inherit;
    max-width: 100%; overflow-wrap: anywhere; text-align: center;
  }
  .needs-consent .allow-error { margin: 10px 0 0; color: var(--danger); }

  /* Citation chips rendered via {@html} in renderAnswer(). */
  :global(.ask-turns .cite-chip) {
    display: inline; padding: 1px 6px; margin: 0 2px;
    background: var(--accent-wash); color: var(--accent-action);
    border: 1px solid var(--accent-border); border-radius: 10px;
    font-size: var(--size-sm); cursor: pointer;
  }
  :global(.ask-turns .cite-chip:hover) {
    background: var(--accent-wash-strong);
  }
</style>
