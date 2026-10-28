<script lang="ts">
  // Test-only host that reproduces NoteEditor's contract for NoteMoreMenu:
  // the menu stays mounted and `open` is the host's state, which onclose
  // clears. Unlike a bare mount, the Popover (a sheet on phone) really
  // closes when the menu says so.
  import NoteMoreMenu from '../NoteMoreMenu.svelte';
  import type { Note } from '../../types';

  let { note, anchor, onclose }: { note: Note; anchor: HTMLElement; onclose: () => void } = $props();
  let open = $state(true);
</script>

<NoteMoreMenu
  {note}
  {anchor}
  {open}
  onclose={() => { open = false; onclose(); }}
  onFind={() => {}}
  onShowGraph={() => {}}
  onShortcuts={() => {}}
  trustPending={false}
  onMarkReviewed={() => {}}
  onTags={() => {}}
  showTags={false}
/>
