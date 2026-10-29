<script lang="ts">
  import Modal from './Modal.svelte';
  import { changelogInline } from '../changelogInline';

  // One changelog entry: a version, its date, and grouped bullet lists.
  type Entry = { version: string; date: string | null; sections: Record<string, string[]> };

  let { open = $bindable(false), versions = [] }: { open?: boolean; versions?: Entry[] } =
    $props();

  function close() {
    open = false;
  }
</script>

{#if open}
  <Modal title="What's New" size="md" phone="page" onclose={close}>
    {#if versions.length === 0}
      <p class="wn-empty">No release notes for this version.</p>
    {:else}
      {#each versions as v (v.version)}
        <section class="wn-version">
          <h3>
            {v.version}{#if v.date}<span class="wn-date"> · {v.date}</span>{/if}
          </h3>
          {#each Object.entries(v.sections) as [group, items] (group)}
            <h4>{group}</h4>
            <ul>
              <!-- CHANGELOG.md bullets lead with **bold** and quote `code`:
                   rendered as text runs, never {@html}, so nothing else in
                   them is markup. -->
              {#each items as item}<li>{#each changelogInline(item) as span}{#if span.bold && span.code}<strong><code>{span.text}</code></strong>{:else if span.code}<code>{span.text}</code>{:else if span.bold}<strong>{span.text}</strong>{:else}{span.text}{/if}{/each}</li>{/each}
            </ul>
          {/each}
        </section>
      {/each}
    {/if}
  </Modal>
{/if}

<style>
  .wn-version { margin-bottom: var(--space-4); }
  .wn-version h3 { margin: 0 0 var(--space-1-5); font-size: var(--size-md); }
  .wn-date { color: var(--text-muted); font-weight: 400; }
  .wn-version h4 { margin: var(--space-2) 0 var(--space-0-5); font-size: var(--size); color: var(--text-secondary); }
  .wn-version ul { margin: 0 0 var(--space-1); padding-left: var(--space-6); }
  .wn-version li { font-size: var(--size); line-height: 1.5; }
  .wn-version code { font-family: var(--font-mono); font-size: 0.9em; padding: 0 var(--space-0-5); border-radius: var(--radius-sm); background: var(--surface-sunken); overflow-wrap: anywhere; }
  .wn-empty { color: var(--text-muted); font-size: var(--size); }
</style>
