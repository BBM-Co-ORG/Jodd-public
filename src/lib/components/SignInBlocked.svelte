<script lang="ts">
  import { signInBlocked } from '../stores/ui';
  import { adminRequestText } from '../signInBlocked';
  import Modal from './Modal.svelte';

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

{#if $signInBlocked}
  <Modal
    title={$signInBlocked.adminConsentUrl
      ? "Sign-in needs your organisation's approval"
      : 'Sign-in was not completed'}
    size="md"
    phone="page"
    onclose={close}
  >
    {#if $signInBlocked.adminConsentUrl}
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
      </div>
      {#if copyFailed}
        <p class="blocked-note">Could not reach the clipboard — select the link above and copy it by hand.</p>
      {/if}
      <p class="blocked-note">
        Jodd shows as "unverified" on Microsoft's consent screen. That is expected
        for a Developer Preview, and your administrator will see it too.
      </p>
    {:else}
      <p class="blocked-lead">{$signInBlocked.message}</p>
    {/if}
  </Modal>
{/if}

<style>
  .blocked-lead {
    font-size: var(--size);
    color: var(--text-secondary);
    line-height: var(--leading);
    margin: 0 0 var(--space-3);
  }
  .blocked-causes {
    font-size: var(--size);
    color: var(--text-secondary);
    line-height: var(--leading);
    margin: 0 0 var(--space-3);
    padding-left: var(--space-6);
  }
  .blocked-causes li {
    margin-bottom: var(--space-1-5);
  }
  .blocked-url {
    display: block;
    font-family: var(--font-mono);
    font-size: var(--size-xs);
    line-height: var(--leading);
    background: var(--surface-sidebar);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    padding: var(--space-2) var(--space-3);
    margin-bottom: var(--space-3);
    /* The link is long and must stay readable rather than being clipped:
       a truncated consent URL forwarded to IT simply does not work. */
    overflow-wrap: anywhere;
    /* Selectable by hand, so a clipboard refusal is not a dead end. */
    user-select: all;
  }
  .blocked-actions {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .blocked-btn {
    padding: var(--space-1-5) var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: var(--surface-sidebar);
    cursor: pointer;
    font-size: var(--size);
  }
  :global(dialog.touch) .blocked-btn {
    min-height: var(--touch-target);
    min-width: var(--touch-target);
  }
  .blocked-note {
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
    margin: var(--space-3) 0 0;
  }
</style>
