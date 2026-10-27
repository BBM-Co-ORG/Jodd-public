<script lang="ts">
  // The one non-modal anchored panel (spec 2026-10-20 §4.1): the editor's tags
  // popover, status popover and ⋯ menu are built on it. It sits below its
  // anchor, clamped to the viewport, and closes on Esc or a pointerdown
  // outside it; focus then returns to the anchor.
  //
  // Presentation gate (global constraint): phone only when
  // $isAndroid && $androidLayoutMode === 'phone', where it is a bottom sheet
  // (Modal phone="sheet") instead. A narrow desktop window and an Android
  // tablet keep the anchored popover.
  import { tick, type Snippet } from 'svelte';
  import { isAndroid } from '../stores/platform';
  import { androidLayoutMode } from '../stores/viewport';
  import { anchorPosition, type AnchorPlacement } from '../editor/anchorPosition';
  import Modal from './Modal.svelte';

  interface Props {
    anchor: HTMLElement;
    open: boolean;
    onclose: () => void;
    placement?: AnchorPlacement;
    /** The phone sheet's title; by default the anchor's name. */
    label?: string;
    children: Snippet;
  }

  let { anchor, open = $bindable(), onclose, placement = 'bottom-start', label, children }: Props = $props();

  const asSheet = $derived($isAndroid && $androidLayoutMode === 'phone');

  let pop: HTMLElement | undefined = $state();
  let left = $state(0);
  let top = $state(0);

  // Modal needs a title for aria-labelledby; the anchor already names the
  // popover's purpose, unless one anchor serves several (⋯).
  const sheetTitle = $derived(
    label || anchor.getAttribute('aria-label') || anchor.title || anchor.textContent?.trim() || 'Options',
  );

  function close(): void {
    if (!open) return;
    const wasSheet = asSheet;
    open = false;
    onclose();
    anchor.focus();
    // A sheet is a modal <dialog>: until it unmounts the anchor is inert and
    // refuses focus, and the Modal then refocuses whatever opened it — ⋯, for
    // a popover opened from a menu item rather than its own anchor. Focus
    // the anchor again once the sheet has gone.
    if (wasSheet) {
      void tick().then(() => {
        if (anchor.isConnected) anchor.focus();
      });
    }
  }

  function place(): void {
    if (!pop) return;
    const r = pop.getBoundingClientRect();
    ({ left, top } = anchorPosition(
      anchor.getBoundingClientRect(),
      { width: r.width, height: r.height },
      { width: window.innerWidth, height: window.innerHeight },
      placement,
    ));
  }

  $effect(() => {
    if (!open || asSheet || !pop) return;
    void placement;
    void anchor;
    place();
    // Content may render after the first pass (async lists): measure again.
    void tick().then(place);
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
    };
  });

  $effect(() => {
    if (!open || asSheet) return;
    const onPointerDown = (e: Event) => {
      const t = e.target as Node | null;
      // The anchor is excluded so its own click can toggle without a
      // close-then-reopen.
      if (t && (pop?.contains(t) || anchor.contains(t))) return;
      close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      close();
    };
    document.addEventListener('pointerdown', onPointerDown, true);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown, true);
      document.removeEventListener('keydown', onKey);
    };
  });
</script>

{#if open}
  {#if asSheet}
    <Modal title={sheetTitle} phone="sheet" onclose={close}>
      {@render children()}
    </Modal>
  {:else}
    <div bind:this={pop} class="popover" style:left="{left}px" style:top="{top}px">
      {@render children()}
    </div>
  {/if}
{/if}

<style>
  .popover {
    position: fixed;
    z-index: var(--z-popover);
    box-sizing: border-box;
    max-width: calc(100vw - 2 * var(--space-2));
    max-height: calc(100dvh - 2 * var(--space-2));
    overflow-y: auto;
    padding: var(--space-1);
    background: var(--surface-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-menu);
  }
</style>
