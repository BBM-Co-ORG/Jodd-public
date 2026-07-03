<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';

  let { accountId }: { accountId: string } = $props();

  type LlmConfig = {
    provider: 'none' | 'claude_code' | 'http';
    http_base_url: string | null;
    http_model: string | null;
    http_api_key_keychain: string | null;
  };

  let cfg: LlmConfig = $state({
    provider: 'none',
    http_base_url: '',
    http_model: '',
    http_api_key_keychain: null,
  });
  let apiKey = $state('');
  let saving = $state(false);
  let msg = $state('');

  onMount(async () => {
    try {
      cfg = await invoke<LlmConfig>('get_llm_settings', { accountId });
    } catch (e) {
      msg = `load: ${e}`;
    }
  });

  async function save() {
    saving = true;
    msg = '';
    try {
      await invoke('update_llm_settings', {
        accountId,
        cfg: {
          provider: cfg.provider,
          http_base_url: cfg.provider === 'http' ? cfg.http_base_url : null,
          http_model: cfg.provider === 'http' ? cfg.http_model : null,
          http_api_key_keychain: cfg.provider === 'http' ? `llm_api_key::${accountId}` : null,
        },
        apiKey: cfg.provider === 'http' && apiKey ? apiKey : null,
      });
      msg = 'Saved.';
      apiKey = '';  // never keep the cleartext in memory
    } catch (e) {
      msg = `save: ${e}`;
    } finally {
      saving = false;
    }
  }
</script>

<div class="llm-settings">
  <h3>LLM Provider</h3>

  <label>
    <input type="radio" bind:group={cfg.provider} value="claude_code" />
    Claude Code (CLI) — uses your existing claude installation
  </label>

  <label>
    <input type="radio" bind:group={cfg.provider} value="http" />
    Custom endpoint (OpenAI-compatible)
  </label>

  {#if cfg.provider === 'http'}
    <div class="http-fields">
      <label>
        Base URL
        <input type="text" bind:value={cfg.http_base_url} placeholder="https://api.openai.com/v1" />
      </label>
      <label>
        Model
        <input type="text" bind:value={cfg.http_model} placeholder="gpt-4o-mini" />
      </label>
      <label>
        API key
        <input type="password" bind:value={apiKey} placeholder="(leave blank to keep existing)" />
      </label>
    </div>
  {/if}

  <label>
    <input type="radio" bind:group={cfg.provider} value="none" />
    Disabled
  </label>

  <button onclick={save} disabled={saving}>{saving ? 'Saving…' : 'Save'}</button>
  {#if msg}<p class="msg">{msg}</p>{/if}
</div>

<style>
  .llm-settings { padding: 12px 0; }
  label { display: block; margin: 8px 0; font-size: 13px; }
  .http-fields { margin-left: 24px; padding: 8px; background: #f5f5f0; border-radius: 4px; }
  input[type="text"], input[type="password"] { width: 100%; padding: 6px; font: inherit; box-sizing: border-box; margin-top: 4px; }
  button { padding: 8px 16px; font: inherit; }
  .msg { font-size: 12px; color: #666; }
</style>
