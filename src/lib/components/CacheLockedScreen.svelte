<script lang="ts">
  // Mounted by main.ts INSTEAD of App when the backend locked the process
  // (src-tauri/src/cache_lock.rs). It must stay this small: it shows no
  // account, note or setting, and its only command is `retry_cache_unlock` —
  // the one command the backend still runs while locked.
  import { invoke } from '@tauri-apps/api/core';
  import { cacheLockCopy, type CacheLock } from '../cacheLock';

  let { lock }: { lock: CacheLock } = $props();

  const copy = $derived(cacheLockCopy(lock.kind));
  let retrying = $state(false);
  let retryError = $state<string | null>(null);

  async function retry() {
    retrying = true;
    retryError = null;
    try {
      // Relaunches the app; on success this page is gone before it resolves.
      await invoke('retry_cache_unlock');
    } catch (e) {
      retrying = false;
      retryError = String(e);
    }
  }
</script>

<main class="locked-screen">
  <section class="locked-card" role="alert" aria-labelledby="cache-lock-title">
    <h1 id="cache-lock-title">{copy.title}</h1>
    {#each copy.body as para}
      <p>{para}</p>
    {/each}
    <button class="retry-btn" onclick={retry} disabled={retrying}>
      {retrying ? 'Restarting…' : copy.action}
    </button>
    {#if retryError}
      <p class="retry-error">Couldn't restart Jodd: {retryError}. Quit Jodd and open it again.</p>
    {/if}
    {#if lock.detail}
      <details>
        <summary>Details</summary>
        <code>{lock.detail}</code>
      </details>
    {/if}
  </section>
</main>

<style>
  .locked-screen {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 100dvh;
    padding: 16px;
    box-sizing: border-box;
    background: var(--surface-list);
  }

  .locked-card {
    background: var(--surface-panel);
    border-radius: 12px;
    padding: 40px 36px;
    box-shadow: var(--shadow-card);
    max-width: 440px;
    width: 100%;
    color: var(--text);
  }

  h1 {
    font-size: var(--size-xl);
    font-weight: 700;
    margin: 0 0 16px;
  }

  p {
    color: var(--text-secondary);
    line-height: 1.5;
    margin: 0 0 12px;
  }

  .retry-btn {
    margin-top: 12px;
    padding: 10px 20px;
    border: none;
    border-radius: 8px;
    background: var(--accent-action);
    color: var(--text-inverse);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }

  .retry-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .retry-btn:focus-visible {
    outline: none;
    box-shadow: var(--ring-focus);
  }

  .retry-btn:disabled {
    cursor: default;
    opacity: 0.7;
  }

  .retry-error {
    margin-top: 12px;
    color: var(--danger);
  }

  details {
    margin-top: 20px;
    color: var(--text-muted);
  }

  code {
    display: block;
    margin-top: 8px;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: var(--font-mono);
    font-size: var(--size-sm);
  }
</style>
