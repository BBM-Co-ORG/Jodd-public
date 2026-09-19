<script lang="ts">
  let { urls, loading = false, failed = false, onretry }: {
    urls: string[]; loading?: boolean; failed?: boolean; onretry?: () => void;
  } = $props();
  let expanded = $state(false);
  const id = $props.id();
  function label(value: string) {
    try {
      const url = new URL(value);
      let path = url.pathname;
      try { path = decodeURIComponent(path); } catch { /* retain malformed escaping */ }
      return `${url.host}${path === '/' ? '' : path}${url.search}${url.hash}`;
    } catch { return value; }
  }
  function safeHref(value: string) {
    try { const url = new URL(value); return ['https:', 'http:'].includes(url.protocol) ? url.href : undefined; }
    catch { return undefined; }
  }
</script>
<div class="source-list" aria-label="Sources">
  {#if loading}<p role="status">Loading sources…</p>
  {:else if failed}<p role="status">Sources unavailable. <button type="button" onclick={onretry}>Retry sources</button></p>
  {:else if urls.length}
    <span>Sources ({urls.length})</span>
    {#if urls.length > 3}
      <button type="button" aria-expanded={expanded} aria-controls={id} onclick={() => expanded = !expanded}>
        {expanded ? 'Show fewer sources' : `Show all ${urls.length} sources`}
      </button>
    {/if}
    <ul {id}>
      {#each (expanded ? urls : urls.slice(0, 3)) as url (url)}
        <li>{#if safeHref(url)}<a href={safeHref(url)} title={url} target="_blank" rel="noopener noreferrer">{label(url)}</a>{:else}<span>{label(url)}</span>{/if}</li>
      {/each}
    </ul>
    {#if !expanded && urls.length > 3}<span>{urls.length - 3} more sources available</span>{/if}
  {/if}
</div>
<style>
  .source-list { min-width:0; font-size:0.8rem; color:var(--text-muted); }
  button { font:inherit; color:var(--text); background:var(--surface-panel); border:1px solid var(--border); border-radius:4px; padding:6px 10px; margin:4px; cursor:pointer; }
  ul { display:flex; flex-wrap:wrap; gap:6px; padding:0; margin:6px 0; list-style:none; max-height:30vh; overflow-y:auto; }
  li { min-width:0; max-width:100%; }
  a, li span { display:block; overflow-wrap:anywhere; border:1px solid var(--border); border-radius:4px; padding:6px 8px; color:var(--text); }
  a:focus-visible, button:focus-visible { outline:2px solid var(--text); outline-offset:2px; }
</style>
