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
    background: rgba(0, 0, 0, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .wn-modal {
    background: #fdfcf7;
    border-radius: 10px;
    padding: 20px 24px;
    width: min(520px, 90vw);
    max-height: 80vh;
    overflow-y: auto;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25);
  }
  h2 { margin: 0 0 12px; font-size: 18px; }
  .wn-version { margin-bottom: 16px; }
  .wn-version h3 { margin: 0 0 6px; font-size: 15px; }
  .wn-date { color: #8a8a80; font-weight: 400; }
  .wn-version h4 { margin: 8px 0 2px; font-size: 13px; color: #5a5a52; }
  .wn-version ul { margin: 0 0 4px; padding-left: 20px; }
  .wn-version li { font-size: 13px; line-height: 1.5; }
  .wn-empty { color: #777; font-size: 13px; }
  .wn-actions { display: flex; justify-content: flex-end; margin-top: 8px; }
  .wn-close {
    padding: 6px 14px;
    border: 1px solid #cfcabb;
    border-radius: 6px;
    background: #efece2;
    cursor: pointer;
  }
</style>
