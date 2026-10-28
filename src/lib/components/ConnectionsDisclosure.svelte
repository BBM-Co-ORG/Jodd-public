<script lang="ts">
  // Task 5.4 (spec 2026-10-20 §6.5): Links to, Linked from and Sources used to
  // sit permanently under the body and take about a quarter of the editor's
  // height. They are one disclosure line now:
  //   ›  Sources 4 · Links 3 · Linked from 2
  // The caller owns the counts and the content; this component owns only the
  // line, the open/closed state and what the line says while sources load or
  // fail. A zero count drops its segment; the whole line disappears when there
  // is nothing to say. The open state is remembered per device.
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import { isAndroid } from '../stores/platform';

  let { sources, links, backlinks, sourcesState, onRetrySources, children }: {
    sources: number;
    links: number;
    backlinks: number;
    sourcesState: 'ok' | 'loading' | 'failed';
    onRetrySources: () => void;
    children: Snippet;
  } = $props();

  const OPEN_KEY = 'jodd.connections.open';
  function readOpen(): boolean {
    try { return localStorage.getItem(OPEN_KEY) === '1'; } catch { return false; }
  }
  function writeOpen(value: boolean) {
    try { localStorage.setItem(OPEN_KEY, value ? '1' : '0'); } catch { /* blocked storage: the state just is not remembered */ }
  }

  let open = $state(readOpen());
  const panelId = $props.id();

  function toggle() {
    open = !open;
    writeOpen(open);
  }

  // The first segment says what is known about Sources. A refresh of a list
  // that already has entries is 'ok' as far as this line is concerned: the
  // caller reports 'loading' only when it has nothing to count yet.
  const segments = $derived.by(() => {
    const out: string[] = [];
    if (sourcesState === 'failed') out.push('Sources unavailable');
    else if (sourcesState === 'loading' && sources === 0) out.push('Sources…');
    else if (sources > 0) out.push(`Sources ${sources}`);
    if (links > 0) out.push(`Links ${links}`);
    if (backlinks > 0) out.push(`Linked from ${backlinks}`);
    return out;
  });
  const visible = $derived(segments.length > 0);
</script>

{#if visible}
  <div class="connections-disclosure" class:touch={$isAndroid}>
    <div class="line">
      <button
        type="button"
        class="toggle"
        aria-expanded={open}
        aria-controls={panelId}
        onclick={toggle}
      >
        <Icon name={open ? 'chevron-down' : 'chevron-right'} size={12} />
        <span class="summary">{segments.join(' · ')}</span>
      </button>
      {#if sourcesState === 'failed'}
        <span class="sep" aria-hidden="true">·</span>
        <button type="button" class="retry" onclick={onRetrySources}>Retry</button>
      {/if}
    </div>
    {#if open}
      <div id={panelId} class="panel">
        {@render children()}
      </div>
    {/if}
  </div>
{/if}

<style>
  .connections-disclosure {
    border-top: 1px solid var(--border-subtle);
    background: var(--surface-editor);
    padding: var(--space-1) var(--space-6) var(--space-2);
    flex-shrink: 0;
  }
  .line {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    column-gap: var(--space-1-5);
    min-height: var(--hit-target-desktop);
    font-size: var(--size-sm);
    color: var(--text-muted);
  }
  .touch .line { min-height: var(--touch-target); }
  .toggle, .retry {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1-5);
    min-height: inherit;
    padding: 0 var(--space-1);
    border: none;
    background: none;
    font: inherit;
    cursor: pointer;
  }
  .toggle { color: var(--text-muted); }
  .toggle:hover { color: var(--text); }
  .retry { color: var(--accent-action); }
  .toggle:focus-visible, .retry:focus-visible {
    outline: 2px solid var(--text);
    outline-offset: 2px;
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-1-5);
    padding-bottom: var(--space-1);
  }
</style>
