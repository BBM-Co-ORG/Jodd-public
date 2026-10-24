<script lang="ts">
  import ConfirmDialog from '../../src/lib/components/ConfirmDialog.svelte';
  import NoteContextMenu from '../../src/lib/components/NoteContextMenu.svelte';
  import { accounts, capabilitiesByAccount, currentAccount } from '../../src/lib/stores/notes';
  import type { Note } from '../../src/lib/types';
  let open = $state(false);
  let menu = $state(false);
  let showOpener = $state(true);
  let confirmed = $state(0);
  let cancelled = $state(0);
  let closedMenus = $state(0);
  let background = $state(0);
  const note: Note = { id: 'fixture', uuid: 'fixture', title: 'Synthetic fixture',
    account_id: 'fixture', body_html: '', date: '2026-09-19', label: 'Notes' };
  accounts.set([]); currentAccount.set('fixture');
  capabilitiesByAccount.set({ fixture: { has_trash: false } });
</script>
<main>
  <h1>Isolated confirmation fixture</h1>
  {#if showOpener}<button id="opener" onclick={() => open = true}>Open confirmation</button>{/if}
  <button id="removed-opener" onclick={() => { open = true; showOpener = false; }}>Open without retained opener</button>
  <button id="menu-opener" onclick={() => menu = true}>Open context menu</button>
  <button id="background" onclick={() => background++}>Background action</button>
  <output id="counts">{confirmed},{cancelled},{closedMenus},{background}</output>
  {#if open}
    <ConfirmDialog title="Delete fixture?" message="ข้อมูลทดสอบเท่านั้น — no real deletion" destructive
      onConfirm={() => { confirmed++; open = false; }} onCancel={() => { cancelled++; open = false; }} />
  {/if}
  {#if menu}
    <NoteContextMenu {note} x={10} y={160} onClose={() => { closedMenus++; menu = false; }} onLinkSuggestions={() => {}} />
  {/if}
</main>
<style>
  main { font-family: sans-serif; padding: 24px; color: var(--text); background: var(--surface-panel); }
  output { display: block; margin-top: 20px; }
</style>
