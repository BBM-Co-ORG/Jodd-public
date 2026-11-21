<script lang="ts">
  // App.svelte's shape around the editor: the phone layout and the tablet
  // layout each mount their OWN NoteEditor, so a rotation destroys one
  // editor and creates another (Svelte 5.56: init incoming → destroy
  // outgoing → mount incoming). Tests that only flip the layout store under
  // one mounted editor cannot see what a rotation does to its state.
  import { androidLayoutMode } from '../../stores/viewport';
  import NoteEditor from '../NoteEditor.svelte';
</script>

{#if $androidLayoutMode === 'phone'}
  <div class="phone-layout"><NoteEditor /></div>
{:else}
  <div class="tablet-layout"><NoteEditor /></div>
{/if}
