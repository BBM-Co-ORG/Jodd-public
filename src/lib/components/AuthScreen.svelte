<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount, onDestroy } from 'svelte';
  import { isAuthenticated, error } from '../stores/notes';
  import { appSettingsOpen } from '../stores/ui';
  import { isAndroid } from '../stores/platform';

  // Three states, not two. `loading` alone forced one label to cover both
  // "launching the browser" (a beat) and "waiting for Google to come back"
  // (up to two minutes), so the button read "Opening browser..." long after
  // the browser had opened, closed, and been dismissed. When the callback
  // silently never arrives — the exact Android failure this branch spent its
  // time on — the screen said the app was doing something it had finished
  // doing, and named no way out.
  type Phase = 'idle' | 'opening' | 'waiting';
  type Provider = 'google' | 'microsoft';
  let phase: Phase = 'idle';
  // Which button is driving the current phase — only one OAuth flow can be
  // in flight at a time (one `pending_pkce`/`pending_backend` slot on the
  // backend), so both buttons share `phase` and disable together; this just
  // decides which one shows the spinner/label.
  let provider: Provider | null = null;
  $: loading = phase !== 'idle';
  // Optimistic true avoids flashing a disabled button on users who have
  // embedded credentials (the common developer case). The check resolves
  // within one IPC round-trip, so the button re-enables almost immediately
  // if credentials are available but briefly appears disabled on cold start
  // for users who genuinely have no credentials.
  let credentialsAvailable = true;
  let pollHandle: ReturnType<typeof setInterval> | null = null;
  let pollDeadline = 0;

  // Polling fallback: the Rust side emits 'oauth-success' once tokens are saved,
  // but that event can be missed (listener race, window unfocused, duplicate
  // 8080 bind, etc.). The refresh token IS persisted to Keychain regardless,
  // so we poll is_authenticated() — backend truth wins over event delivery.
  function startPolling() {
    stopPolling();
    pollDeadline = Date.now() + 120_000; // 2 minutes
    pollHandle = setInterval(async () => {
      try {
        const authed = await invoke<boolean>('is_authenticated');
        if (authed) {
          stopPolling();
          isAuthenticated.set(true); // App.svelte reacts and unmounts AuthScreen
          return;
        }
      } catch {
        // ignore transient errors; keep polling until the deadline
      }
      if (Date.now() > pollDeadline) {
        stopPolling();
        phase = 'idle';
        // Name the two things that actually cause this, because neither is
        // guessable: the user abandoned the consent screen, or the redirect
        // was rendered in the browser instead of being handed to Jodd.
        error.set(
          'Sign-in did not complete. If a page in your browser asked you to ' +
            'return to Jodd, open it and tap the button there. Otherwise, try again.',
        );
      }
    }, 2000);
  }

  function stopPolling() {
    if (pollHandle !== null) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
  }

  onDestroy(stopPolling);

  onMount(async () => {
    try {
      const cfg = await invoke<{ credentials_available: boolean }>('get_oauth_config');
      credentialsAvailable = cfg.credentials_available;
    } catch {
      credentialsAvailable = false;
    }
  });

  // `which` also becomes the `backend` argument to `get_auth_url` — omitted
  // (undefined) for Google, matching the existing default the backend
  // resolves to Gmail, and `'microsoft'` for the new path.
  async function signIn(which: Provider) {
    phase = 'opening';
    provider = which;
    error.set(null);
    try {
      const url = await invoke<string>(
        'get_auth_url',
        which === 'microsoft' ? { backend: which } : undefined,
      );
      await invoke('open_auth_url', { url });
      phase = 'waiting';
      startPolling();
    } catch (e) {
      error.set(String(e));
      phase = 'idle';
      provider = null;
    }
  }

  // Let the user out without waiting for the deadline. The backend keeps its
  // own cancellation (the loopback listener's token, the persisted PKCE), so
  // this only resets what the screen is showing — the next `signIn` starts a
  // fresh flow and supersedes whatever the old one left behind.
  function startOver() {
    stopPolling();
    phase = 'idle';
    provider = null;
    error.set(null);
  }
</script>

<div class="auth-screen">
  <div class="auth-card">
    <div class="logo">
      <span class="logo-icon">🍎</span>
      <span class="logo-arrow">→</span>
      <span class="logo-icon">🪟</span>
    </div>
    <h1>Jodd</h1>
    <p class="subtitle">Developer Preview</p>
    <p class="description">
      Connect Gmail- or Microsoft-backed Apple Notes on Windows, macOS, or
      Android. This preview is intended for technical users.
    </p>
    <button class="google-btn" onclick={() => signIn('google')} disabled={loading || !credentialsAvailable}>
      {#if phase === 'opening' && provider === 'google'}
        <span class="spinner"></span>
        Opening browser...
      {:else if phase === 'waiting' && provider === 'google'}
        <span class="spinner"></span>
        Waiting for Google...
      {:else}
        <svg width="18" height="18" viewBox="0 0 18 18">
          <path fill="#4285F4" d="M16.51 8H8.98v3h4.3c-.18 1-.74 1.48-1.6 2.04v2.01h2.6a7.8 7.8 0 0 0 2.38-5.88c0-.57-.05-.66-.15-1.18z"/>
          <path fill="#34A853" d="M8.98 17c2.16 0 3.97-.72 5.3-1.94l-2.6-2a4.8 4.8 0 0 1-7.18-2.54H1.83v2.07A8 8 0 0 0 8.98 17z"/>
          <path fill="#FBBC05" d="M4.5 10.52a4.8 4.8 0 0 1 0-3.04V5.41H1.83a8 8 0 0 0 0 7.18l2.67-2.07z"/>
          <path fill="#EA4335" d="M8.98 4.18c1.17 0 2.23.4 3.06 1.2l2.3-2.3A8 8 0 0 0 1.83 5.4L4.5 7.49a4.77 4.77 0 0 1 4.48-3.31z"/>
        </svg>
        Sign in with Google
      {/if}
    </button>
    {#if !$isAndroid}
      <!-- Microsoft needs a loopback redirect, which Android does not run
           yet — the backend refuses the flow there with a clear message
           (`backend_kind_for_signin`); hide the entry point on that
           platform rather than let the user hit that error every time. -->
      <button class="microsoft-btn" onclick={() => signIn('microsoft')} disabled={loading}>
        {#if phase === 'opening' && provider === 'microsoft'}
          <span class="spinner"></span>
          Opening browser...
        {:else if phase === 'waiting' && provider === 'microsoft'}
          <span class="spinner"></span>
          Waiting for Microsoft...
        {:else}
          <svg width="18" height="18" viewBox="0 0 18 18">
            <rect x="1" y="1" width="7.5" height="7.5" fill="#F25022"/>
            <rect x="9.5" y="1" width="7.5" height="7.5" fill="#7FBA00"/>
            <rect x="1" y="9.5" width="7.5" height="7.5" fill="#00A4EF"/>
            <rect x="9.5" y="9.5" width="7.5" height="7.5" fill="#FFB900"/>
          </svg>
          Sign in with Microsoft
        {/if}
      </button>
    {/if}
    {#if phase === 'waiting'}
      <p class="waiting-hint">
        Finish signing in with {provider === 'microsoft' ? 'Microsoft' : 'Google'} in your browser, then come back here.
        <button class="creds-link" onclick={startOver}>Start over</button>
      </p>
    {/if}
    {#if !credentialsAvailable}
      <p class="creds-notice">
        Gmail sync requires credentials —
        <button class="creds-link" onclick={() => appSettingsOpen.set(true)}>
          Configure first
        </button>
      </p>
    {/if}
    <p class="note">
      Notes are cached locally on this device and sync directly with Google.
      BBMedia does not receive a copy.
    </p>
  </div>
</div>

<style>
  .auth-screen {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100vh;
    background: var(--surface-list);
  }

  .auth-card {
    background: var(--surface-panel);
    border-radius: 12px;
    padding: 48px 40px;
    text-align: center;
    box-shadow: var(--shadow-card);
    max-width: 360px;
    width: 100%;
  }

  .logo {
    font-size: 32px;
    margin-bottom: 16px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
  }

  .logo-arrow {
    font-size: var(--size-xl);
    color: var(--text-muted);
  }

  h1 {
    font-size: var(--size-2xl);
    font-weight: 700;
    color: var(--text);
    margin: 0 0 4px;
    letter-spacing: 2px;
  }

  .subtitle {
    font-size: var(--size-sm);
    color: var(--text-muted);
    margin: 0 0 24px;
    text-transform: uppercase;
    letter-spacing: 1px;
  }

  .description {
    font-size: var(--size-md);
    color: var(--text-secondary);
    line-height: 1.5;
    margin-bottom: 32px;
  }

  .google-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 10px;
    width: 100%;
    padding: 12px 24px;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    font-size: var(--size-md);
    font-weight: 500;
    color: var(--text);
    cursor: pointer;
    transition: background 0.2s, box-shadow 0.2s;
  }

  .google-btn:hover:not(:disabled) {
    background: var(--surface-editor);
    box-shadow: var(--shadow-raised);
  }

  .google-btn:disabled {
    opacity: 0.7;
    cursor: not-allowed;
  }

  .microsoft-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 10px;
    width: 100%;
    padding: 12px 24px;
    margin-top: 10px;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    font-size: var(--size-md);
    font-weight: 500;
    color: var(--text);
    cursor: pointer;
    transition: background 0.2s, box-shadow 0.2s;
  }

  .microsoft-btn:hover:not(:disabled) {
    background: var(--surface-editor);
    box-shadow: var(--shadow-raised);
  }

  .microsoft-btn:disabled {
    opacity: 0.7;
    cursor: not-allowed;
  }

  .creds-notice {
    font-size: var(--size-sm);
    color: var(--danger);
    margin-top: 10px;
    text-align: center;
  }
  /* Guidance, not a fault — muted rather than the danger colour above. */
  .waiting-hint {
    font-size: var(--size-sm);
    color: var(--text-muted);
    margin-top: 10px;
    text-align: center;
    line-height: 1.5;
  }
  .creds-link {
    background: none;
    border: none;
    color: var(--accent-action);
    cursor: pointer;
    padding: 0;
    font-size: var(--size-sm);
    text-decoration: underline;
  }
  .note {
    font-size: var(--size-xs);
    color: var(--text-muted);
    margin-top: 16px;
  }

  .spinner {
    width: 16px;
    height: 16px;
    border: 2px solid var(--border);
    /* Was #4285F4 — the Google brand blue from the "G" logo above, reused here
       by accident. This arc is a busy indicator, not a brand mark and not a
       focus ring, so it takes the theme accent. */
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    display: inline-block;
  }

  /* Same reasoning as NoteList's spinner: the global prefers-reduced-motion
     rule clamps this animation to 0.01ms, which would freeze the ring with one
     arbitrary accent-coloured arc and read as a broken shape rather than as
     "busy". Even the border out so it degrades to a deliberate static ring. */
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      border-color: var(--border-strong);
    }
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
