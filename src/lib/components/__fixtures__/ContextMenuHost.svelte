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
    selection={[]}
    onClose={closeContextMenu}
    onLinkSuggestions={() => {}}
  />
{/if}
