<script lang="ts" module>
  // How long Google/Microsoft sign-in may take before the screen gives up.
  // Picking an account and passing two-step verification on a phone took
  // 2 min 27 s on a Galaxy S23 FE (2026-10-29) — past the old 2 minutes, so
  // the user saw "did not complete" for a sign-in that then succeeded.
  export const SIGN_IN_WAIT_MS = 5 * 60_000;
</script>

<script lang="ts">
  import { failureText } from '../errorText';
  import { invoke } from '@tauri-apps/api/core';
  import { onMount, onDestroy } from 'svelte';
  import { get } from 'svelte/store';
  import { isAuthenticated, error } from '../stores/notes';
  import { settingsRoute, signInBlocked, localFolderBusy } from '../stores/ui';
  import { supportsIcloud } from '../stores/platform';
  import { startAddAccount } from '../controllers/accountActions';
  import AccountAddHost from './AccountAddHost.svelte';
  import BrandMark from './BrandMark.svelte';

  // Three states, not two. `loading` alone forced one label to cover both
  // "launching the browser" (a beat) and "waiting for Google to come back"
  // (up to SIGN_IN_WAIT_MS), so the button read "Opening browser..." long after
  // the browser had opened, closed, and been dismissed. When the callback
  // silently never arrives — the exact Android failure this branch spent its
  // time on — the screen said the app was doing something it had finished
  // doing, and named no way out.
  type Phase = 'idle' | 'opening' | 'waiting';
  // iCloud is a provider here even though it shares none of the OAuth flow:
  // it drives the same `phase`/`provider` pair so the three buttons disable
  // together, which is what stops a user starting a second sign-in while a
  // webview from the first is still open.
  type Provider = 'google' | 'microsoft' | 'icloud';
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
    pollDeadline = Date.now() + SIGN_IN_WAIT_MS;
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
        error.set(TIMED_OUT_MESSAGE);
      }
    }, 2000);
  }

  const TIMED_OUT_MESSAGE =
    'Sign-in did not complete. If a page in your browser asked you to ' +
    'return to Jodd, open it and tap the button there. Otherwise, try again.';

  function stopPolling() {
    if (pollHandle !== null) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
  }

  // A sign-in can still land after the deadline: the backend keeps its PKCE
  // and emits `oauth-success`, and App.svelte unmounts this screen. The
  // timeout message is then false, and `error` is shared with the signed-in
  // app, so it would sit on the notes list. Clear it — but only this
  // message, so an error something else reported stays visible.
  onDestroy(() => {
    stopPolling();
    if (get(isAuthenticated) && get(error) === TIMED_OUT_MESSAGE) error.set(null);
  });

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
      error.set(failureText(`sign in with ${which === 'microsoft' ? 'Microsoft' : 'Google'}`, e));
      phase = 'idle';
      provider = null;
    }
  }

  /**
   * iCloud, which is not an OAuth flow at all.
   *
   * No browser, no redirect, no polling: `icloud_sign_in` opens Apple's own
   * pages in a Tauri webview and does not resolve until the session is
   * complete, the user closes the window, or it times out. So the promise IS
   * the completion signal — the `is_authenticated` poll the other two buttons
   * need (because a redirect can be lost) has nothing to add here, and running
   * it would only race the command.
   *
   * A rejection is the real outcome, not a lost callback: a cancelled window,
   * or — the case worth showing carefully — an account with Advanced Data
   * Protection on, where the backend refuses before creating anything and the
   * message names ADP and what to do about it.
   */
  async function signInICloud() {
    phase = 'waiting';
    provider = 'icloud';
    error.set(null);
    try {
      await invoke<string>('icloud_sign_in');
      isAuthenticated.set(true); // App.svelte reacts and unmounts AuthScreen
    } catch (e) {
      error.set(failureText('sign in with iCloud', e));
    } finally {
      phase = 'idle';
      provider = null;
    }
  }

  // A refusal is a finished flow, so stop pretending one is still in progress.
  // Without this the button keeps spinning "Waiting for Microsoft..." behind
  // the panel until the poll deadline — while the panel in front of
  // it is already explaining that the sign-in is over.
  $: if ($signInBlocked) {
    stopPolling();
    phase = 'idle';
    provider = null;
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
    <!-- The mark, then the name in English: the mark is hidden from screen
         readers, so the heading is what names the app. -->
    <BrandMark />
    <h1>Jodd</h1>
    <p class="subtitle">Developer Preview</p>
    <p class="lead">Your Apple Notes, on every device.</p>
    <p class="description">
      Sign in to the account your notes sync to. They stay there: Jodd keeps
      a copy on this device and syncs in the background.
    </p>
    <!-- iCloud first: it is where most Apple Notes live, and the only door
         for anyone who never attached a Google or Outlook account to Notes
         (CLAUDE.md, "How Apple Notes <-> iCloud works"). -->
    <div class="signin-options">
      {#if $supportsIcloud}
        <!-- Every desktop platform, not just macOS. The session lives in a
             per-webview persistent store; `data_store_identifier` is Apple's
             lever and `data_directory` is everyone else's (icloud_auth's
             `isolated()`). Measured identical on both engines 2026-09-09.
             Android holds the session in the PROCESS's cookie jar
             (`CookieManager.getInstance()`) rather than a webview's, so it
             reconnects on demand through the app's own webview instead of
             keeping a hidden one alive in the background. Device-pass verified
             2026-09-10. -->
        <button class="signin-btn icloud-btn" onclick={signInICloud} disabled={loading}>
          {#if phase === 'waiting' && provider === 'icloud'}
            <span class="spinner"></span>
            Waiting for iCloud...
          {:else}
            <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true">
              <path fill="#3693F3" d="M13.9 7.3a4 4 0 0 0-7.6-1.4 3 3 0 0 0-4 2.8A3.1 3.1 0 0 0 5.4 12h8.2a2.4 2.4 0 0 0 .3-4.7z"/>
            </svg>
            Sign in with iCloud
          {/if}
        </button>
        <p class="icloud-hint">
          <!-- The "Keep me signed in" line is not a nicety. Apple decides cookie
               lifetime from that checkbox, and without it every session cookie
               is session-scoped: measured 16 of 18 unticked against 3 of 16
               ticked, i.e. signing in again on every launch. It is the same on
               every platform, which is why the wording names none. -->
          Tick <strong>Keep me signed in</strong> on Apple's page, or you will
          have to sign in again each time Jodd starts.
          Notes and folders are editable in Jodd. <strong>Pins, attachments and
          new #hashtags are not</strong> — make those in Apple Notes. A note Jodd
          cannot write safely says so on the note itself.
        </p>
      {/if}
      <button class="signin-btn google-btn" onclick={() => signIn('google')} disabled={loading || !credentialsAvailable}>
        {#if phase === 'opening' && provider === 'google'}
          <span class="spinner"></span>
          Opening browser...
        {:else if phase === 'waiting' && provider === 'google'}
          <span class="spinner"></span>
          Waiting for Google...
        {:else}
          <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true">
            <path fill="#4285F4" d="M16.51 8H8.98v3h4.3c-.18 1-.74 1.48-1.6 2.04v2.01h2.6a7.8 7.8 0 0 0 2.38-5.88c0-.57-.05-.66-.15-1.18z"/>
            <path fill="#34A853" d="M8.98 17c2.16 0 3.97-.72 5.3-1.94l-2.6-2a4.8 4.8 0 0 1-7.18-2.54H1.83v2.07A8 8 0 0 0 8.98 17z"/>
            <path fill="#FBBC05" d="M4.5 10.52a4.8 4.8 0 0 1 0-3.04V5.41H1.83a8 8 0 0 0 0 7.18l2.67-2.07z"/>
            <path fill="#EA4335" d="M8.98 4.18c1.17 0 2.23.4 3.06 1.2l2.3-2.3A8 8 0 0 0 1.83 5.4L4.5 7.49a4.77 4.77 0 0 1 4.48-3.31z"/>
          </svg>
          Sign in with Google
        {/if}
      </button>
      <!-- Every platform, Android included: Microsoft redirects through the
           same App Links URL Gmail uses (`auth_ms::redirect_uri`), so the
           loopback listener that used to keep it off Android no longer decides. -->
      <button class="signin-btn microsoft-btn" onclick={() => signIn('microsoft')} disabled={loading}>
        {#if phase === 'opening' && provider === 'microsoft'}
          <span class="spinner"></span>
          Opening browser...
        {:else if phase === 'waiting' && provider === 'microsoft'}
          <span class="spinner"></span>
          Waiting for Microsoft...
        {:else}
          <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true">
            <rect x="1" y="1" width="7.5" height="7.5" fill="#F25022"/>
            <rect x="9.5" y="1" width="7.5" height="7.5" fill="#7FBA00"/>
            <rect x="1" y="9.5" width="7.5" height="7.5" fill="#00A4EF"/>
            <rect x="9.5" y="9.5" width="7.5" height="7.5" fill="#FFB900"/>
          </svg>
          Sign in with Microsoft
        {/if}
      </button>
    </div>
    {#if phase === 'waiting' && provider !== 'icloud'}
      <p class="waiting-hint">
        Finish signing in with {provider === 'microsoft' ? 'Microsoft' : 'Google'} in your browser, then come back here.
        <button class="creds-link" onclick={startOver}>Start over</button>
      </p>
    {/if}
    <!-- No sign-in needed: a folder on this device, or a server the user owns.
         Also the only way a new device restores a backup into a Local Folder
         without first signing in somewhere else (live pass 2026-10-24). -->
    <div class="other-start" data-testid="other-start">
      <p class="other-start-lead">Or start without signing in:</p>
      <div class="other-start-actions">
        <button class="creds-link" onclick={() => startAddAccount('localfs')} disabled={loading || $localFolderBusy}>
          Start with a Local Folder
        </button>
        <button class="creds-link" onclick={() => startAddAccount('ssh')} disabled={loading}>
          Start with an SSH server
        </button>
      </div>
      <p class="other-start-hint">Setting up from a backup? Start here, then restore it in Settings › Backup.</p>
    </div>
    {#if !credentialsAvailable}
      <p class="creds-notice">
        Gmail sync requires credentials —
        <button class="creds-link" onclick={() => settingsRoute.set('advanced/oauth')}>
          Configure first
        </button>
      </p>
    {/if}
    <!-- True for every door: each backend syncs straight between this device
         and the account (or folder, or server) the user picked. -->
    <p class="note">
      Notes sync directly between this device and the account you choose.
      BBMedia never receives a copy.
    </p>
  </div>
</div>
<AccountAddHost firstRun />

<style>
  /* The screen scrolls rather than clipping: on a short window or a phone
     in landscape the card is taller than the view, and `align-items:
     center` would push its top out of reach. `margin: auto` on the card
     centres it when it fits and lets it scroll from the top when it does
     not. */
  .auth-screen {
    display: flex;
    box-sizing: border-box;
    height: 100dvh;
    overflow-y: auto;
    padding: max(var(--space-4), env(safe-area-inset-top)) var(--space-4)
      max(var(--space-4), env(safe-area-inset-bottom));
    background: var(--surface-list);
  }

  .auth-card {
    box-sizing: border-box;
    margin: auto;
    background: var(--surface-panel);
    border-radius: var(--radius-lg);
    padding: var(--space-8) var(--space-8) var(--space-6);
    text-align: center;
    box-shadow: var(--shadow-card);
    max-width: 380px;
    width: 100%;
  }

  h1 {
    font-size: var(--size-2xl);
    font-weight: var(--weight-bold);
    line-height: var(--leading);
    color: var(--text);
    margin: 0;
    letter-spacing: 0.06em;
  }

  /* Sentence case under the name: a plain fact about the build, not a
     label shouting over the heading. */
  .subtitle {
    font-size: var(--size-sm);
    color: var(--text-muted);
    margin: 0 0 var(--space-6);
  }

  .lead {
    font-size: var(--size-lg);
    font-weight: var(--weight-semibold);
    line-height: var(--leading);
    color: var(--text);
    margin: 0 0 var(--space-2);
  }

  .description {
    font-size: var(--size-md);
    color: var(--text-secondary);
    line-height: var(--leading);
    margin: 0 0 var(--space-6);
  }

  .signin-options {
    display: flex;
    flex-direction: column;
    gap: calc(var(--space-2) + var(--space-0-5));
  }

  /* The three sign-in options share one shape: options that look alike is
     the point, and the provider mark is what tells them apart. One rule, not
     three copies of it (the #281 review): the radius is the button token
     (DESIGN.md, 6px, was a literal 8px), the spacing from the scale, and a
     disabled button is 60% opacity like every other one. The provider
     classes stay as hooks (google-btn, microsoft-btn, icloud-btn). */
  .signin-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: calc(var(--space-2) + var(--space-0-5));
    width: 100%;
    padding: var(--space-3) var(--space-6);
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: var(--size-md);
    font-weight: var(--weight-regular);
    color: var(--text);
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out), box-shadow var(--dur-fast) var(--ease-out);
  }
  .signin-btn:hover:not(:disabled) {
    background: var(--surface-editor);
    box-shadow: var(--shadow-raised);
  }
  .signin-btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  /* Said BEFORE the user signs in, not after: what this backend cannot do is
     narrower than it was in M1 but still real, and discovering a limit by
     hitting it is a worse way to learn it than one line under the button.
     Deliberately does NOT enumerate the per-note write refusals — those are
     per note, discovered on read, and belong on the row that carries them
     (M2 spec, Component N), not in a warning everyone reads once. */
  .icloud-hint {
    font-size: var(--size-sm);
    color: var(--text-muted);
    /* Sits under its own button, closer to it than to the next one. */
    margin: calc(var(--space-0-5) - var(--space-1)) 0 var(--space-1);
    text-align: center;
    line-height: var(--leading);
  }

  .creds-notice {
    font-size: var(--size-sm);
    color: var(--danger);
    margin-top: calc(var(--space-2) + var(--space-0-5));
    text-align: center;
  }
  /* Guidance, not a fault — muted rather than the danger colour above. */
  .waiting-hint {
    font-size: var(--size-sm);
    color: var(--text-muted);
    margin-top: calc(var(--space-2) + var(--space-0-5));
    text-align: center;
    line-height: var(--leading);
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
  .creds-link:disabled {
    color: var(--text-muted);
    cursor: default;
  }
  .other-start {
    margin-top: var(--space-6);
    padding-top: var(--space-4);
    border-top: 1px solid var(--border);
    font-size: var(--size-sm);
    color: var(--text-muted);
  }
  .other-start-lead {
    margin: 0 0 var(--space-1-5);
  }
  .other-start-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--space-1-5) var(--space-4);
  }
  .other-start-hint {
    font-size: var(--size-xs);
    margin: var(--space-2) 0 0;
    line-height: var(--leading);
  }
  .note {
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
    margin: var(--space-4) 0 0;
  }

  /* Touch: every control on the door is at least a thumb wide and tall,
     the text links included (Start over, Local Folder, SSH, Configure). */
  @media (pointer: coarse) {
    .signin-btn {
      min-height: var(--touch-target);
    }
    .creds-link {
      min-height: var(--touch-target);
      padding: 0 var(--space-1);
    }
  }

  /* A phone: the card fills the width, so its own gutter can be smaller. */
  @media (max-width: 480px) {
    .auth-card {
      padding: var(--space-6) var(--space-4) var(--space-4);
    }
  }

  .spinner {
    width: var(--space-4);
    height: var(--space-4);
    border: 2px solid var(--border);
    /* Was #4285F4 — the Google brand blue from the "G" logo above, reused here
       by accident. This arc is a busy indicator, not a brand mark and not a
       focus ring, so it takes the theme accent. */
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin var(--dur-spin) linear infinite;
    display: inline-block;
  }

  /* Reduced motion: the ring stops. Stopped as drawn it would keep one
     arbitrary accent-coloured arc and read as a broken shape rather than as
     "busy", so the border evens out into a deliberate still ring. */
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation: none;
      border-color: var(--border-strong);
    }
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
