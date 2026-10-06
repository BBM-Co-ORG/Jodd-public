<script lang="ts">
  import { signInBlocked } from '../stores/ui';
  import { adminRequestText } from '../signInBlocked';

  // Rendered above App.svelte's isAuthenticated branch, so this one panel
  // serves both first-run sign-in (AuthScreen) and Add Account (Sidebar).

  let copied: 'link' | 'message' | null = null;
  let copyFailed = false;

  async function copy(what: 'link' | 'message', text: string) {
    copyFailed = false;
    try {
      await navigator.clipboard.writeText(text);
      copied = what;
      setTimeout(() => (copied = null), 2000);
    } catch (e) {
      // The URL is on screen and selectable, so a clipboard refusal is
      // recoverable by hand — say so rather than failing silently.
      console.error('clipboard write failed', e);
      copyFailed = true;
    }
  }

  function close() {
    signInBlocked.set(null);
  }
</script>

<svelte:window onkeydown={(e) => { if ($signInBlocked && e.key === 'Escape') close(); }} />

{#if $signInBlocked}
  <div class="blocked-overlay" role="presentation" onclick={close}>
    <div
      class="blocked-modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="blocked-title"
      onclick={(e) => e.stopPropagation()}
    >
      {#if $signInBlocked.adminConsentUrl}
        <h2 id="blocked-title">Sign-in needs your organisation's approval</h2>
        <p class="blocked-lead">
          Microsoft does not tell Jodd why the sign-in stopped. There are only two
          possibilities:
        </p>
        <ul class="blocked-causes">
          <li>You closed the sign-in, or chose not to continue.</li>
          <li>
            Your organisation requires an administrator to approve Jodd before anyone
            there can use it. If you saw a page headed <em>Need admin approval</em>,
            this is your case.
          </li>
        </ul>
        <p class="blocked-lead">
          Send this link to your IT administrator. It lets them approve Jodd for your
          whole organisation — there is nothing you can do from this device on your
          own.
        </p>
        <code class="blocked-url">{$signInBlocked.adminConsentUrl}</code>
        <div class="blocked-actions">
          <button class="blocked-btn" onclick={() => copy('link', $signInBlocked!.adminConsentUrl!)}>
            {copied === 'link' ? 'Copied' : 'Copy link'}
          </button>
          <button
            class="blocked-btn"
            onclick={() => copy('message', adminRequestText($signInBlocked!.adminConsentUrl!))}
          >
            {copied === 'message' ? 'Copied' : 'Copy message for IT'}
          </button>
          <button class="blocked-btn" onclick={close}>Close</button>
        </div>
        {#if copyFailed}
          <p class="blocked-note">Could not reach the clipboard — select the link above and copy it by hand.</p>
        {/if}
        <p class="blocked-note">
          Jodd shows as "unverified" on Microsoft's consent screen. That is expected
          for a Developer Preview, and your administrator will see it too.
        </p>
      {:else}
        <h2 id="blocked-title">Sign-in was not completed</h2>
        <p class="blocked-lead">{$signInBlocked.message}</p>
        <div class="blocked-actions">
          <button class="blocked-btn" onclick={close}>Close</button>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .blocked-overlay {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .blocked-modal {
    background: var(--surface-editor);
    border-radius: 10px;
    padding: 20px 24px;
    width: min(460px, 90vw);
    box-shadow: var(--shadow-modal);
    max-height: 85vh;
    overflow-y: auto;
  }
  h2 {
    margin: 0 0 10px;
    font-size: var(--size-lg);
  }
  .blocked-lead {
    font-size: var(--size);
    color: var(--text-secondary);
    line-height: var(--leading);
    margin: 0 0 10px;
  }
  .blocked-causes {
    font-size: var(--size);
    color: var(--text-secondary);
    line-height: var(--leading);
    margin: 0 0 12px;
    padding-left: 20px;
  }
  .blocked-causes li {
    margin-bottom: 6px;
  }
  .blocked-url {
    display: block;
    font-family: var(--font-mono);
    font-size: var(--size-xs);
    line-height: var(--leading);
    background: var(--surface-sidebar);
    border: 1px solid var(--border-strong);
    border-radius: 6px;
    padding: 8px 10px;
    margin-bottom: 12px;
    /* The link is long and must stay readable rather than being clipped:
       a truncated consent URL forwarded to IT simply does not work. */
    overflow-wrap: anywhere;
    /* Selectable by hand, so a clipboard refusal is not a dead end. */
    user-select: all;
  }
  .blocked-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .blocked-btn {
    padding: 6px 14px;
    border: 1px solid var(--border-strong);
    border-radius: 6px;
    background: var(--surface-sidebar);
    cursor: pointer;
    font-size: var(--size);
  }
  .blocked-note {
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
    margin: 12px 0 0;
  }
</style>
