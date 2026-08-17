<script lang="ts">
  // Test-only host that reproduces NoteList.svelte's mount/unmount contract
  // for NoteContextMenu: the menu is rendered inside `{#if menuNote}` and
  // onClose() nulls `menuNote`. That is what makes the menu's `note` prop
  // read back as null once onClose() has run — the exact condition under
  // which a menu action that touches `note` after onClose() throws.
  // See NoteList.svelte's closeContextMenu() + `{#if menuNote}` block.
  import NoteContextMenu from '../NoteContextMenu.svelte';
  import type { Note } from '../../types';

  export let note: Note;
  // Optional multi-select batch, mirroring NoteList's `selection` prop. Empty
  // by default so existing single-note callers (reExtract.test.ts) are
  // unaffected; pass length > 1 to exercise NoteContextMenu's batch paths
  // (e.g. deleteBatch's permanent-delete confirm).
  export let selection: Note[] = [];

  let menuNote: Note | null = note;
  function closeContextMenu() {
    menuNote = null;
  }
</script>

{#if menuNote}
  <NoteContextMenu
    x={0}
    y={0}
    note={menuNote}
    selection={selection}
    onClose={closeContextMenu}
    onLinkSuggestions={() => {}}
  />
{/if}
