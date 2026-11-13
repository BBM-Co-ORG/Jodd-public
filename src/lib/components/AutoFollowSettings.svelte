<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import { activeAccounts, accountDisplay } from '../stores/notes';
  import type { LinkSettings } from '../types';

  // Settings › Links: which accounts follow the best links automatically
  // (spec 2026-10-28 D2). Off for every account until turned on here. No Save
  // button of its own: the page's one Save calls save().

  let saved = $state<string[] | null>(null);
  let on = $state<string[]>([]);
  let rule = $state<{ max: number; min: number } | null>(null);
  let msg = $state('');

  onMount(async () => {
    try {
      const s = await invoke<LinkSettings>('get_link_settings');
      saved = [...s.auto_follow];
      on = [...s.auto_follow];
      rule = { max: s.auto_follow_max, min: s.auto_follow_min_score };
    } catch (e) {
      msg = `load: ${e}`;
    }
  });

  function toggle(id: string) {
    on = on.includes(id) ? on.filter((x) => x !== id) : [...on, id];
  }

  /** Section-form contract. */
  export function isDirty(): boolean {
    if (saved === null) return false;
    return on.length !== saved.length || on.some((id) => !saved!.includes(id));
  }

  export async function save(): Promise<void> {
    if (!isDirty()) return;
    msg = '';
    try {
      await invoke('set_link_settings', { autoFollow: on });
      saved = [...on];
    } catch (e) {
      msg = String(e);
      throw new Error(msg);
    }
  }
</script>

<div class="auto-follow">
  <p class="label">Follow the best links automatically</p>
  {#if $activeAccounts.length === 0}
    <p class="help">No accounts yet.</p>
  {/if}
  {#each $activeAccounts as a (a.id)}
    <label class="row">
      <input type="checkbox" checked={on.includes(a.id)} disabled={saved === null} onchange={() => toggle(a.id)} />
      {accountDisplay(a)}
    </label>
  {/each}
  {#if rule}
    <p class="help">Up to {rule.max} links per source, only the strongest. You still see them before the note is made.</p>
  {/if}
  {#if msg}<p class="msg" role="alert">{msg}</p>{/if}
</div>

<style>
  .auto-follow { padding: 12px 0; }
  .label { margin: 0 0 6px; font-size: var(--size); }
  .row { display: flex; align-items: center; gap: 8px; margin: 4px 0; font-size: var(--size); }
  .row input { width: auto; margin: 0; }
  .help { font-size: var(--size-sm); color: var(--accent-action); margin: 4px 0 0; }
  .msg { font-size: var(--size-sm); color: var(--danger); }
</style>
