<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { error } from '../stores/notes';
  import { groupConversations, type ConversationSummary } from './askHistory';

  type Change = 'deleted' | 'archived' | 'unarchived' | 'renamed';
  let {
    activeId, refreshKey, onSelect, onNew, onChanged,
  }: {
    activeId: string | null;
    refreshKey: number;
    onSelect: (id: string) => void;
    onNew: () => void;
    onChanged: (id: string, change: Change) => void;
  } = $props();

  let active = $state<ConversationSummary[]>([]);
  let archived = $state<ConversationSummary[]>([]);
  let showArchived = $state(false);
  let menuFor = $state<string | null>(null);
  let confirmDelete = $state<string | null>(null);
  let renaming = $state<string | null>(null);
  let draft = $state('');
  let groups = $derived(groupConversations(active, Date.now()));

  async function load() {
    try {
      const [a, b] = await Promise.all([
        invoke<ConversationSummary[] | null>('list_ask_conversations', { archived: false }),
        invoke<ConversationSummary[] | null>('list_ask_conversations', { archived: true }),
      ]);
      // The modal's tests mock unknown commands as null — empty, not broken.
      active = a ?? [];
      archived = b ?? [];
    } catch (e) {
      error.set(`Could not load Ask history: ${e}`);
    }
  }

  $effect(() => { refreshKey; void load(); });

  function closeMenus() { menuFor = null; confirmDelete = null; }

  const byRecent = (x: ConversationSummary, y: ConversationSummary) => y.updated_at - x.updated_at;
  // A throwing parent callback is the parent's bug: log it, but it must neither
  // roll back a write the backend accepted nor become an unhandled rejection.
  function notify(id: string, change: Change) {
    try { onChanged(id, change); } catch (e) { console.error('AskHistory onChanged threw', e); }
  }
  const without = (list: ConversationSummary[], id: string) => list.filter((x) => x.id !== id);

  // Local-first: move first, IPC second. Rollback is per row — a whole-list
  // snapshot would resurrect rows that a concurrent, successful operation moved.
  async function setArchived(c: ConversationSummary, to: boolean) {
    closeMenus();
    const moved = { ...c, archived: to };
    if (to) { active = without(active, c.id); archived = [moved, ...without(archived, c.id)]; }
    else { archived = without(archived, c.id); active = [moved, ...without(active, c.id)].sort(byRecent); }
    let ok = false;
    try {
      await invoke('set_ask_conversation_archived', { conversationId: c.id, archived: to });
      ok = true;
    } catch (e) {
      const back = { ...c, archived: !to };
      if (to) { archived = without(archived, c.id); active = [back, ...without(active, c.id)].sort(byRecent); }
      else { active = without(active, c.id); archived = [back, ...without(archived, c.id)]; }
      error.set(`Could not ${to ? 'archive' : 'unarchive'} the conversation: ${e}`);
    }
    if (ok) notify(c.id, to ? 'archived' : 'unarchived');
  }

  async function remove(c: ConversationSummary) {
    closeMenus();
    const wasArchived = archived.some((x) => x.id === c.id);
    const index = archived.findIndex((x) => x.id === c.id);
    active = without(active, c.id);
    archived = without(archived, c.id);
    let ok = false;
    try {
      await invoke('delete_ask_conversation', { conversationId: c.id });
      ok = true;
    } catch (e) {
      if (wasArchived) {
        const next = without(archived, c.id);
        next.splice(Math.min(index, next.length), 0, c);
        archived = next;
      } else {
        active = [c, ...without(active, c.id)].sort(byRecent);
      }
      error.set(`Could not delete the conversation: ${e}`);
    }
    if (ok) notify(c.id, 'deleted');
  }

  function startRename(c: ConversationSummary) { closeMenus(); renaming = c.id; draft = c.title; }

  async function commitRename(c: ConversationSummary) {
    // Enter commits and removes the input; removing it fires blur, which lands
    // here a second time. Only the first call may send the IPC.
    if (renaming !== c.id) return;
    const title = draft;
    renaming = null;
    if (!title.trim() || title === c.title) return;
    const previous = c.title;
    const set = (t: string) => {
      active = active.map((x) => (x.id === c.id ? { ...x, title: t } : x));
      archived = archived.map((x) => (x.id === c.id ? { ...x, title: t } : x));
    };
    set(title.trim());
    let ok = false;
    try {
      set(await invoke<string>('rename_ask_conversation', { conversationId: c.id, title }));
      ok = true;
    } catch (e) {
      set(previous);
      error.set(`Could not rename the conversation: ${e}`);
    }
    if (ok) notify(c.id, 'renamed');
  }

  function onRenameKey(e: KeyboardEvent, c: ConversationSummary) {
    if (e.key === 'Enter') { e.preventDefault(); void commitRename(c); }
    if (e.key === 'Escape') { e.stopPropagation(); renaming = null; }
  }
</script>

{#snippet row(c: ConversationSummary)}
  <li class="ask-row" class:active={c.id === activeId} data-id={c.id}>
    {#if renaming === c.id}
      <!-- svelte-ignore a11y_autofocus -->
      <input class="field" bind:value={draft} autofocus
        onkeydown={(e) => onRenameKey(e, c)} onblur={() => commitRename(c)} />
    {:else}
      <button type="button" class="ask-row-title" title={c.title} onclick={() => onSelect(c.id)}>{c.title}</button>
      <button type="button" class="ask-row-menu" aria-label="Conversation actions"
        onclick={() => { confirmDelete = null; menuFor = menuFor === c.id ? null : c.id; }}>⋯</button>
    {/if}
    {#if menuFor === c.id}
      <div class="ask-row-actions" role="menu">
        {#if confirmDelete === c.id}
          <span>Delete permanently?</span>
          <button type="button" class="danger" onclick={() => remove(c)}>Delete conversation</button>
          <button type="button" onclick={() => (confirmDelete = null)}>Keep</button>
        {:else}
          <button type="button" onclick={() => startRename(c)}>Rename</button>
          <button type="button" onclick={() => setArchived(c, !c.archived)}>{c.archived ? 'Unarchive' : 'Archive'}</button>
          <button type="button" class="danger" onclick={() => (confirmDelete = c.id)}>Delete</button>
        {/if}
      </div>
    {/if}
  </li>
{/snippet}

<nav class="ask-rail" aria-label="Ask Jodd history">
  <button type="button" class="new" onclick={onNew}>+ New</button>
  {#each groups as g (g.label)}
    <h3>{g.label}</h3>
    <ul>{#each g.items as c (c.id)}{@render row(c)}{/each}</ul>
  {/each}
  {#if archived.length > 0}
    <button type="button" class="archived-toggle" onclick={() => (showArchived = !showArchived)}>Archived ({archived.length})</button>
    {#if showArchived}
      <ul>{#each archived as c (c.id)}{@render row(c)}{/each}</ul>
    {/if}
  {/if}
</nav>

<style>
  .ask-rail { display: flex; flex-direction: column; gap: 4px; overflow-y: auto; min-width: 0; }
  h3 { margin: 10px 0 2px; font-size: var(--size-xs); color: var(--text-muted); font-weight: 600; }
  ul { list-style: none; margin: 0; padding: 0; }
  .ask-row { display: flex; flex-wrap: wrap; align-items: center; border-radius: 4px; }
  .ask-row.active { background: var(--accent-wash); }
  .ask-row-title {
    flex: 1; min-width: 0; text-align: left; background: none; border: none; color: inherit;
    padding: 6px 8px; cursor: pointer; font: inherit; font-size: var(--size-sm);
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  }
  .ask-row-menu { background: none; border: none; color: var(--text-muted); cursor: pointer; padding: 4px 6px; }
  /* In the row's own flow, on a line of its own — never `position: absolute`.
     The rail scrolls (`overflow-y: auto`), so a popover hanging below the
     last row was clipped: on a phone the only row's menu was invisible and
     Delete unreachable (S23 FE live pass, 2026-10-15). */
  .ask-row-actions {
    flex: 1 1 100%; display: flex; flex-wrap: wrap; align-items: center; gap: 2px 8px;
    padding: 2px 4px 6px; font-size: var(--size-sm);
  }
  .ask-row-actions button { background: none; border: none; color: inherit; text-align: left; padding: 4px 8px; cursor: pointer; font: inherit; }
  .ask-row-actions .danger { color: var(--danger); }
  .new, .archived-toggle {
    background: var(--surface-panel); color: inherit; border: 1px solid var(--border);
    border-radius: 4px; padding: 6px 8px; cursor: pointer; font: inherit; text-align: left;
  }
  .archived-toggle { margin-top: 10px; border: none; background: none; color: var(--text-muted); font-size: var(--size-xs); }
  .field { width: 100%; padding: 4px 6px; font: inherit; background: var(--surface-panel); color: inherit; border: 1px solid var(--border); border-radius: 4px; }
</style>
