<script lang="ts">
  // Test stand-in for Sidebar.svelte: same props and events, none of the app
  // wiring. The buttons fire the two events a frame reacts to.
  import { createEventDispatcher } from 'svelte';
  export let width: number = 200;
  export let tabletFrame: 'column' | 'drawer' | null = null;
  const dispatch = createEventDispatcher<{ collapse: void; pick: void }>();
</script>

<div class="stub-sidebar" style="width: {width}px" data-tablet-frame={tabletFrame}>
  <button type="button" class="stub-pick" onclick={() => dispatch('pick')}>Pick</button>
  <button type="button" class="stub-collapse" onclick={() => dispatch('collapse')}>Collapse</button>
</div>
