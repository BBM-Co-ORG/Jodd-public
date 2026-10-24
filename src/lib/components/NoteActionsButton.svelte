<script lang="ts">
  // The phone note PaneHeader's ⋯ (spec 2026-10-20 §5.6). It owns no menu: it
  // asks the open NoteEditor to toggle its own NoteMoreMenu, which a phone
  // shows as a sheet (stores/noteActionsMenu.ts). Renders nothing while no
  // editor on a phone has published that — so there is exactly one "Note
  // actions" on a phone, and none here off it.
  import Icon from './Icon.svelte';
  import { noteActionsMenu } from '../stores/noteActionsMenu';

  let btn: HTMLButtonElement | undefined = $state();
</script>

{#if $noteActionsMenu}
  <button
    bind:this={btn}
    type="button"
    class="note-actions-btn"
    aria-label="Note actions"
    title="Note actions"
    aria-haspopup="menu"
    aria-expanded={$noteActionsMenu.open}
    onclick={() => { if (btn) $noteActionsMenu?.toggle(btn); }}
  ><Icon name="more" size={20} /></button>
{/if}
