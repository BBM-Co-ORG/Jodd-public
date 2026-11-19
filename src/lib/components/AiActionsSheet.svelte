<script lang="ts">
  // The phone Folders bar's AI ✦ (spec 2026-10-20 §5.1, 2026-10-28 §5): the
  // AI menu, the same list the desktop footer's popover shows (AiMenu). Each
  // entry opens what it opened before, so this sheet adds a way in, not a
  // second implementation.
  //
  // It used to list the Agents group (agent folders) too, because the phone
  // Folders screen did not show it. It does now, in the tree as on desktop
  // (spec 2026-11-12 §5), so the folders live in one place.
  import Modal from './Modal.svelte';
  import AiMenu from './AiMenu.svelte';

  let {
    onclose,
  }: {
    onclose: () => void;
  } = $props();

  type SheetModal = { requestClose(): Promise<boolean> };
  let modal: SheetModal | undefined = $state();

  // Close first, then open: the sheet's history entry is popped and the next
  // modal's entry is queued behind that back (phoneNav serializes them).
  function run(action: () => void) {
    void modal?.requestClose();
    action();
  }
</script>

<Modal bind:this={modal} phone="sheet" title="AI" {onclose}>
  <div class="sheet-menu" role="menu">
    <AiMenu {run} iconSize={20} />
  </div>
</Modal>
