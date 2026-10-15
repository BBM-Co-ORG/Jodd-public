<script lang="ts">
  // One changelog entry: a version, its date, and grouped bullet lists.
  type Entry = { version: string; date: string | null; sections: Record<string, string[]> };

  let { open = $bindable(false), versions = [] }: { open?: boolean; versions?: Entry[] } =
    $props();

  function close() {
    open = false;
  }
</script>

<svelte:window onkeydown={(e) => { if (open && e.key === 'Escape') close(); }} />

{#if open}
  <div class="wn-overlay" role="presentation" onclick={close}>
    <div
      class="wn-modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="wn-title"
      onclick={(e) => e.stopPropagation()}
    >
      <h2 id="wn-title">What's New</h2>
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
                {#each items as item}<li>{item}</li>{/each}
              </ul>
            {/each}
          </section>
        {/each}
      {/if}
      <div class="wn-actions">
        <button class="wn-close" onclick={close}>Close</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .wn-overlay {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .wn-modal {
    background: var(--surface-editor);
    border-radius: 10px;
    padding: 20px 24px;
    width: min(520px, 90vw);
    max-height: 80vh;
    overflow-y: auto;
    box-shadow: var(--shadow-modal);
  }
  h2 { margin: 0 0 12px; font-size: var(--size-lg); }
  .wn-version { margin-bottom: 16px; }
  .wn-version h3 { margin: 0 0 6px; font-size: var(--size-md); }
  .wn-date { color: var(--text-muted); font-weight: 400; }
  .wn-version h4 { margin: 8px 0 2px; font-size: var(--size); color: var(--text-secondary); }
  .wn-version ul { margin: 0 0 4px; padding-left: 20px; }
  .wn-version li { font-size: var(--size); line-height: 1.5; }
  .wn-empty { color: var(--text-muted); font-size: var(--size); }
  .wn-actions { display: flex; justify-content: flex-end; margin-top: 8px; }
  .wn-close {
    padding: 6px 14px;
    border: 1px solid var(--border-strong);
    border-radius: 6px;
    background: var(--surface-sidebar);
    cursor: pointer;
  }
</style>
