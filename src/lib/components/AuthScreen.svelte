<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onDestroy } from 'svelte';
  import { isAuthenticated, error } from '../stores/notes';

  let loading = false;
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
        loading = false;
        error.set('Sign-in timed out. Please try again.');
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

  async function signIn() {
    loading = true;
    error.set(null);
    try {
      const url = await invoke<string>('get_auth_url');
      await invoke('open_auth_url', { url });
      startPolling();
    } catch (e) {
      error.set(String(e));
      loading = false;
    }
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
    <p class="subtitle">Apple Notes Bridge for Windows</p>
    <p class="description">
      Access your Apple Notes synced with Google on any Windows device.
    </p>
    <button class="google-btn" onclick={signIn} disabled={loading}>
      {#if loading}
        <span class="spinner"></span>
        Opening browser...
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
    <p class="note">Your notes stay in your Google account. We never store them.</p>
  </div>
</div>

<style>
  .auth-screen {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100vh;
    background: #f5f5f5;
  }

  .auth-card {
    background: white;
    border-radius: 12px;
    padding: 48px 40px;
    text-align: center;
    box-shadow: 0 2px 20px rgba(0,0,0,0.1);
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
    font-size: 20px;
    color: #999;
  }

  h1 {
    font-size: 28px;
    font-weight: 700;
    color: #1a1a1a;
    margin: 0 0 4px;
    letter-spacing: 2px;
  }

  .subtitle {
    font-size: 12px;
    color: #999;
    margin: 0 0 24px;
    text-transform: uppercase;
    letter-spacing: 1px;
  }

  .description {
    font-size: 14px;
    color: #555;
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
    background: white;
    border: 1px solid #dadce0;
    border-radius: 8px;
    font-size: 14px;
    font-weight: 500;
    color: #3c4043;
    cursor: pointer;
    transition: background 0.2s, box-shadow 0.2s;
  }

  .google-btn:hover:not(:disabled) {
    background: #f8f9fa;
    box-shadow: 0 1px 4px rgba(0,0,0,0.15);
  }

  .google-btn:disabled {
    opacity: 0.7;
    cursor: not-allowed;
  }

  .note {
    font-size: 11px;
    color: #aaa;
    margin-top: 16px;
  }

  .spinner {
    width: 16px;
    height: 16px;
    border: 2px solid #dadce0;
    border-top-color: #4285F4;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    display: inline-block;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
