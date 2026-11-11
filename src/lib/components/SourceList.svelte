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
<!-- Every save re-reads the citations. A list already on screen stays put
     while that runs: swapping it for one loading line changed this panel's
     height, which resized the editor above it and moved the text under the
     user's caret (sourceListRefresh.test.ts). -->
<div class="source-list" aria-label="Sources">
  {#if loading && !urls.length}<p role="status">Loading sources…</p>
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
  {/if}
</div>
<style>
  /* Tokens throughout (re-critique, step 7): sizes, spaces and radii from
     the scale, the Thai line-height floor for URL paths, links in Amber Ink,
     and Focus Blue for the ring. "Show all N sources" above the list says how
     many are hidden; the line that repeated it below is gone. */
  .source-list { min-width:0; font-size:var(--size-sm); line-height:var(--leading); color:var(--text-muted); }
  button { font:inherit; color:var(--text); background:var(--surface-panel); border:1px solid var(--border); border-radius:var(--radius-sm); padding:var(--space-1-5) var(--space-2); margin:var(--space-1); min-height:var(--hit-target-desktop); cursor:pointer; }
  ul { display:flex; flex-wrap:wrap; gap:var(--space-1-5); padding:0; margin:var(--space-1-5) 0; list-style:none; max-height:30vh; overflow-y:auto; }
  li { min-width:0; max-width:100%; }
  a, li span { display:block; overflow-wrap:anywhere; border:1px solid var(--border); border-radius:var(--radius-sm); padding:var(--space-1-5) var(--space-2); color:var(--text); }
  a { color:var(--accent-action); }
  a:focus-visible, button:focus-visible { outline:2px solid var(--focus); outline-offset:2px; }
</style>
