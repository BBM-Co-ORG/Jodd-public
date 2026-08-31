<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { currentAccount, selectedFolder } from '../stores/notes';
  import { appSettingsOpen } from '../stores/ui';
  import { renderAnswer, type CitedNote } from './askCitations';
  import { askScope, isRealFolderSelection } from './askScope';

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
  };
  type Rendered = Turn & { html?: string; stats?: string; error?: boolean };

  let turns = $state<Rendered[]>([]);
  let input = $state('');
  let busy = $state(false);
  let requestId = $state('');
  // Default: current account (spec §5.6) — "all accounts" spans thousands of
  // notes, where the pre-filter thins hardest and answer quality is least
  // predictable.
  let scopeKind = $state<'account' | 'folder' | 'all'>('account');
  let folderScopeAvailable = $derived(isRealFolderSelection($selectedFolder));

  // Ask Jodd runs exclusively on the APP-level provider (spec §4.2) — it is
  // cross-account, so no account's provider owns it. Without one, every
  // question came back as a red error turn; §8 asked for a real empty state
  // pointing at App Settings instead, matching how Extract handles
  // NotConfigured.
  //
  // Deliberately fails OPEN: only an affirmative read of a config whose
  // provider is unset flips this. A probe that errors leaves the input usable
  // and lets the backend be the authority, rather than locking the user out of
  // a feature that might work fine.
  let providerMissing = $state(false);

  async function checkProvider() {
    try {
      const res = await invoke<{
        cfg: { llm: { provider: string } };
        has_api_key: boolean;
      }>('get_app_llm_config');
      const p = res?.cfg?.llm?.provider;
      providerMissing = p === 'none' || p === 'disabled';
    } catch {
      providerMissing = false;
    }
  }

  // Re-probed on every open, not once at mount: the user may well fix the
  // config in App Settings and come straight back.
  $effect(() => {
    if (open) void checkProvider();
  });

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
  let activeScope = $derived(askScope(scopeKind, $currentAccount, $selectedFolder));

  async function send() {
    const q = input.trim();
    // Captured once: an unsendable scope must not consume the user's question.
    const scope = activeScope;
    if (!q || busy || !scope) return;
    input = '';
    turns.push({ role: 'user', content: q });
    turns = turns;
    busy = true;
    requestId = newRequestId();

    // Only role+content go to the backend — the rendered HTML is UI-only.
    // Error turns are excluded: they are a UI-side failure notice (e.g. "HTTP
    // 502: <html>...", a provider timeout, a cancelled request), not a real
    // prior answer, and sending one back as `assistant: <error text>` would
    // have the model treat its own failure message as established context.
    const wire = turns.filter((t) => !t.error).map((t) => ({ role: t.role, content: t.content }));
    try {
      const a = await invoke<AskAnswer>('ask_jodd', {
        scope,
        turns: wire,
        requestId,
      });
      const bits = [
        `${a.notes_in_scope.toLocaleString()} in scope`,
        `${a.notes_considered.toLocaleString()} considered`,
        `${a.notes_used} read`,
      ];
      if (a.trimmed) bits.push('trimmed to fit');
      if (a.dropped_citations > 0) bits.push(`${a.dropped_citations} unverifiable citation(s) removed`);
      turns.push({
        role: 'assistant',
        content: a.markdown,
        html: renderAnswer(a.markdown, a.cited),
        stats: bits.join(' → '),
      });
      turns = turns;
    } catch (e) {
      turns.push({ role: 'assistant', content: String(e), error: true });
      turns = turns;
    } finally {
      busy = false;
      requestId = '';
    }
  }

  async function cancel() {
    if (requestId) await invoke('cancel_ask', { requestId });
  }

  function resetForm() {
    turns = [];
    input = '';
    scopeKind = 'account';
  }

  function close() {
    if (busy) {
      // Mirrors LessonExtractModal's close(): a click while busy cancels the
      // in-flight request instead of leaving it orphaned server-side. The
      // finally block in send() clears busy/requestId once it unwinds.
      void cancel();
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
      if (requestId) void cancel();
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
    <div class="modal ask-modal" role="dialog" aria-modal="true" aria-labelledby="ask-title">
      <header class="ask-header">
        <h2 id="ask-title">Ask Jodd</h2>
        <select class="field" bind:value={scopeKind}>
          <option value="account">This account</option>
          <option value="folder" disabled={!folderScopeAvailable}>This folder and below</option>
          <option value="all">All accounts</option>
        </select>
      </header>

      <div class="ask-turns" onclick={onAnswerClick} role="presentation">
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
        {:else if !activeScope}
          <p class="hint">
            No account is selected, so there is nothing to scope this to. Pick an
            account in the sidebar, or switch the scope above to
            <strong>All accounts</strong>.
          </p>
        {:else if turns.length === 0}
          <p class="hint">
            Ask a question about your notes. Answers cite the notes they came
            from, and are not saved — closing this window discards the
            conversation.
          </p>
        {/if}
        {#each turns as t}
          <div class="ask-turn {t.role}" class:error={t.error}>
            {#if t.role === 'user'}
              <p>{t.content}</p>
            {:else if t.html}
              <!-- t.html only ever comes from renderAnswer(), which escapes
                   every character of the model's markdown before splicing in
                   citation-chip markup (see askCitations.ts) — safe to
                   {@html}. The error branch below never sets t.html, so a
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

      <label class="ask-input-label">
        <textarea
          class="field"
          bind:value={input}
          onkeydown={onInputKeydown}
          placeholder={providerMissing
            ? 'Configure an LLM provider in App Settings to ask questions.'
            : 'Ask about your notes… (Enter to send, Shift+Enter for a newline)'}
          disabled={busy || providerMissing}
        ></textarea>
      </label>

      <div class="actions">
        <button type="button" onclick={close}>
          {busy ? 'Cancel' : 'Close'}
        </button>
        <button
          type="button"
          onclick={send}
          disabled={busy || providerMissing || !activeScope || !input.trim()}
          class="primary"
        >
          {busy ? 'Asking…' : 'Ask'}
        </button>
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
    width: 640px; max-width: 90vw; max-height: 85vh;
    display: flex; flex-direction: column;
  }
  .ask-header {
    display: flex; align-items: center; justify-content: space-between;
    gap: 12px; margin: 0 0 8px;
  }
  h2 { margin: 0; }
  .hint { color: var(--text-muted); font-size: var(--size); margin: 0 0 16px; }
  .field {
    padding: 6px 8px; font: inherit; box-sizing: border-box;
    background: var(--surface-panel); color: inherit;
    border: 1px solid var(--border); border-radius: 4px;
  }
  select.field { flex: 0 0 auto; }
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
  .ask-turn.assistant .answer {
    line-height: var(--leading); color: var(--text);
    /* Preserve the model's markdown structure (newlines, list dashes,
       heading lines) without converting it to HTML this round — escaping
       happens in renderAnswer() before this ever reaches the DOM, so
       whitespace is the only thing left to restore. A real markdown->HTML
       step is a follow-up: it would need to run BEFORE citation
       substitution and keep renderAnswer's escaping guarantees, which is
       its own review. */
    white-space: pre-wrap;
  }
  .ask-turn.assistant.error .answer {
    color: var(--danger);
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
  .no-provider button.primary {
    background: var(--accent-action); color: var(--text-inverse); border: none;
    padding: 8px 16px; border-radius: 4px; cursor: pointer; font: inherit;
  }

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
