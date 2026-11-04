<script lang="ts" module>
  let nextModalId = 0;
  const DEFAULT_DIRTY_MESSAGE = 'Discard unsaved changes?';
</script>

<script lang="ts">
  // The one dialog primitive (spec 2026-10-20 §4.1). Built on ConfirmDialog's
  // pattern: a native <dialog> + showModal(), so the background is inert and
  // Tab cannot leave (no hand-rolled focus trap); the opener is refocused on
  // unmount; every key event stops here so app shortcuts never fire; onclose
  // is called at most once.
  //
  // Every close path — Esc, backdrop, ‹, the close button, drag-down, Android
  // Back — goes through requestClose(), which runs the dirty guard first.
  //
  // Presentation gate (global constraint): phone presentations only when
  // $isAndroid && $androidLayoutMode === 'phone'. A narrow desktop window is
  // always a card; tablet is a card with touch-sized controls.
  import { onMount, flushSync, type Snippet } from 'svelte';
  import { get } from 'svelte/store';
  import { isAndroid } from '../stores/platform';
  import { androidLayoutMode } from '../stores/viewport';
  import {
    pushOverlay,
    popOverlay,
    closeOverlayInto,
    registerOverlayCloser,
    type Pane,
  } from '../stores/phoneNav';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  interface Props {
    title: string;
    size?: 'sm' | 'md' | 'lg' | 'xl';
    phone?: 'page' | 'sheet' | 'alert';
    dirty?: boolean;
    /** The guard alert's message (its title). Default 'Discard unsaved changes?'. */
    dirtyMessage?: string;
    /** While true nothing can close the modal: every close path is refused, no guard. */
    locked?: boolean;
    /**
     * Asked first on every close request (Esc, cancel, backdrop, ‹/✕, drag,
     * Android Back), before the dirty guard. Return false to keep the modal
     * open because the caller handled the attempt itself — Ask Jodd and
     * Extract cancel the work in flight and stay open.
     */
    onbeforeclose?: () => boolean;
    onclose: () => void;
    onsave?: () => Promise<void>;
    primary?: { label: string; onclick: () => void; disabled?: boolean; busy?: boolean };
    parentLabel?: string;
    children: Snippet;
    footer?: Snippet;
    headerActions?: Snippet;
  }

  let {
    title,
    size = 'md',
    phone = 'page',
    dirty = false,
    dirtyMessage = DEFAULT_DIRTY_MESSAGE,
    locked = false,
    onbeforeclose,
    onclose,
    onsave,
    primary,
    parentLabel = 'Back',
    children,
    footer,
    headerActions,
  }: Props = $props();

  const id = `modal-${++nextModalId}`;
  const titleId = `${id}-title`;

  const presentation = $derived<'card' | 'page' | 'sheet' | 'alert'>(
    $isAndroid && $androidLayoutMode === 'phone' ? phone : 'card',
  );
  // An alert is a small card wherever it is not a phone alert.
  const cardSize = $derived(phone === 'alert' ? 'sm' : size);
  const showFooter = $derived(!!footer || (!!primary && presentation !== 'page'));

  let dialog: HTMLDialogElement | undefined = $state();
  let settled = false;

  // Dirty guard: a nested alert Modal. One pending decision at a time; every
  // requestClose() while it is open shares the same promise.
  let guardOpen = $state(false);
  let guardSaving = $state(false);
  let guardError = $state('');
  let guardPromise: Promise<boolean> | null = null;
  let guardResolve: ((closed: boolean) => void) | null = null;
  // The pending decision is a move inside the modal (confirmLeave), not a
  // close: Save or Discard then lets the caller move on and the modal stays.
  let guardLeaving = false;

  // Sheet drag-down.
  const DRAG_CLOSE_PX = 80;
  let dragStartY: number | null = null;
  let dragDy = $state(0);

  function finish(): void {
    if (settled) return;
    settled = true;
    popOverlay(id); // no-op unless this modal still owns a history entry
    onclose();
  }

  /** The single close path. Resolves true if the modal closed. */
  export function requestClose(): Promise<boolean> {
    if (settled) return Promise.resolve(true);
    // Locked: refuse outright (Android Back's closer returning false
    // re-pushes the overlay entry, as for a dirty guard's Keep editing).
    if (locked) return Promise.resolve(false);
    // The caller's own say comes first, once per decision: a request made
    // while the guard is already asking shares that pending answer.
    if (!guardPromise && onbeforeclose?.() === false) return Promise.resolve(false);
    if (!dirty) {
      finish();
      return Promise.resolve(true);
    }
    // A close joining a pending confirmLeave() turns it into a close.
    guardLeaving = false;
    return openGuard();
  }

  /**
   * The dirty guard for a move INSIDE the modal (Settings switching pages):
   * the same Save / Discard / Keep editing alert, but the modal stays open.
   * Resolves true when the caller may move on (clean, saved or discarded),
   * false for Keep editing or a failed save the user then backed out of.
   */
  export function confirmLeave(): Promise<boolean> {
    if (settled || locked) return Promise.resolve(false);
    if (!dirty) return Promise.resolve(true);
    if (!guardPromise) guardLeaving = true;
    return openGuard();
  }

  function openGuard(): Promise<boolean> {
    if (!guardPromise) {
      guardError = '';
      guardPromise = new Promise<boolean>((resolve) => {
        guardResolve = resolve;
      });
      guardOpen = true;
    }
    return guardPromise;
  }

  /**
   * A modal action that navigates (a citation, "open the note it made")
   * closes the modal INTO its destination: the overlay entry becomes the
   * destination pane's entry, so Back from there lands beneath the modal.
   * Skips the dirty guard — the caller's action is the user's decision.
   * Refused (returns false) while the guard alert is open: the alert owns
   * the top history entry, so the modal's own entry is not the one to turn
   * into the destination. Returns true once closed. `title` names the
   * destination in phoneNav's title stack (the Back label of what opens
   * above it); omitted, the pane's default title is used.
   */
  export function closeInto(pane: Pane, title?: string): boolean {
    if (settled) return true;
    if (guardOpen || locked) return false;
    settled = true;
    closeOverlayInto(id, pane, title);
    onclose();
    return true;
  }

  function settleGuard(closed: boolean): void {
    const resolve = guardResolve;
    const leaving = guardLeaving;
    guardResolve = null;
    guardPromise = null;
    guardLeaving = false;
    guardOpen = false;
    guardSaving = false;
    guardError = '';
    // Unmount the alert now, so its own history.back() is issued before any
    // re-push of this modal's entry that resolving `false` triggers.
    flushSync();
    if (closed && !leaving) finish();
    resolve?.(closed);
  }

  async function guardSave(): Promise<void> {
    if (!onsave || guardSaving) return;
    // The decision this Save answers. If the user settles it another way
    // while the save runs (Keep editing, Discard, Back), the save's late
    // result must not close the modal or reopen an error on a later guard.
    const decision = guardPromise;
    guardSaving = true;
    guardError = '';
    try {
      await onsave();
    } catch (err) {
      if (guardPromise !== decision || !guardOpen) return;
      // Stay open, keep the edit, say why.
      guardError = err instanceof Error ? err.message : String(err);
      guardSaving = false;
      return;
    }
    if (guardPromise !== decision || !guardOpen) return;
    settleGuard(true);
  }

  function show(): void {
    if (!dialog || dialog.open) return;
    dialog.showModal();
    dialog.querySelector<HTMLElement>('[data-autofocus]')?.focus();
  }

  onMount(() => {
    const opener = document.activeElement;
    show();
    const unregister = registerOverlayCloser(id, requestClose);
    // Decided once, at open (controller ruling R7): on Android every modal
    // owns an entry in any layout, so Back closes the modal first on tablet
    // too (spec §5.4) and a rotation either way keeps Back working. Desktop
    // pushes nothing.
    if (get(isAndroid)) pushOverlay(id);
    return () => {
      unregister();
      popOverlay(id);
      // Unmounted with a decision pending: the modal is gone, so it closed.
      guardResolve?.(true);
      guardResolve = null;
      dialog?.close();
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    };
  });

  // What Tab can reach: native controls, <summary>, contenteditable hosts and
  // anything with a tabindex — minus every negative tabindex, which is
  // focusable by script but skipped by Tab (so it may not count as an end).
  const TABBABLE = [
    'a[href]',
    'button',
    'input:not([type="hidden"])',
    'select',
    'textarea',
    'summary',
    '[contenteditable]:not([contenteditable="false"])',
    '[tabindex]',
  ]
    .map((sel) => `${sel}:not([tabindex^="-"])`)
    .join(', ');

  /**
   * A closed <details> renders only its own <summary>; everything else in it
   * is not a Tab stop (R11). Walks outward, so a summary nested inside a
   * closed outer <details> is skipped too.
   */
  function inClosedDetails(el: Element, root: Element): boolean {
    let node: Element = el;
    for (;;) {
      const details = node.parentElement?.closest('details');
      if (!details || !root.contains(details)) return false;
      if (!details.open) {
        const summary = details.querySelector(':scope > summary');
        if (!summary || !summary.contains(node)) return true;
      }
      node = details;
    }
  }

  /** Tabbable elements of this dialog (not of a nested one), queried at keydown time. */
  function tabbables(): HTMLElement[] {
    if (!dialog) return [];
    const root = dialog;
    return [...root.querySelectorAll<HTMLElement>(TABBABLE)].filter((el) => {
      if (el.closest('dialog') !== root) return false; // a nested dialog traps itself
      if (el.matches(':disabled') || el.closest('[hidden], [inert]')) return false;
      if (inClosedDetails(el, root)) return false;
      const style = getComputedStyle(el);
      return style.display !== 'none' && style.visibility !== 'hidden';
    });
  }

  // A native modal <dialog> makes the page inert but does not wrap Tab inside
  // itself: in Chrome focus escapes to the browser chrome. Wrap by hand.
  function trapTab(e: KeyboardEvent): void {
    const items = tabbables();
    if (items.length === 0) {
      e.preventDefault();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement;
    const inside = active instanceof HTMLElement && items.includes(active);
    if (e.shiftKey && (!inside || active === first)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && (!inside || active === last)) {
      e.preventDefault();
      first.focus();
    }
  }

  function onKey(e: KeyboardEvent): void {
    e.stopPropagation(); // Includes app shortcuts; the modal owns interaction.
    if (e.key === 'Escape') {
      e.preventDefault();
      void requestClose();
    } else if (e.key === 'Tab') {
      trapTab(e);
    }
  }

  function onGrabDown(e: PointerEvent): void {
    dragStartY = e.clientY;
    try {
      (e.currentTarget as Element).setPointerCapture(e.pointerId);
    } catch {
      // No pointer capture (synthetic event): moves still arrive on the handle.
    }
  }

  function onGrabMove(e: PointerEvent): void {
    if (dragStartY === null) return;
    dragDy = Math.max(0, e.clientY - dragStartY);
  }

  function onGrabEnd(e: PointerEvent, cancelled = false): void {
    if (dragStartY === null) return;
    const dy = Math.max(0, e.clientY - dragStartY);
    dragStartY = null;
    dragDy = 0;
    if (!cancelled && dy > DRAG_CLOSE_PX) void requestClose();
  }
</script>

<dialog
  bind:this={dialog}
  class="modal"
  class:touch={$isAndroid}
  data-presentation={presentation}
  data-size={cardSize}
  aria-modal="true"
  aria-labelledby={titleId}
  oncancel={(e) => { e.preventDefault(); e.stopPropagation(); void requestClose(); }}
  onclick={(e) => { if (e.target === e.currentTarget) void requestClose(); }}
  onkeydown={onKey}
>
  <div class="panel" style:transform={dragDy > 0 ? `translateY(${dragDy}px)` : undefined}>
    {#if presentation === 'sheet'}
      <div
        class="grab"
        role="presentation"
        onpointerdown={onGrabDown}
        onpointermove={onGrabMove}
        onpointerup={(e) => onGrabEnd(e)}
        onpointercancel={(e) => onGrabEnd(e, true)}
      >
        <span class="grab-bar"></span>
      </div>
    {/if}
    <header class="head">
      {#if presentation === 'page'}
        <button type="button" class="back" aria-disabled={locked ? 'true' : undefined} onclick={() => void requestClose()}>‹ {parentLabel}</button>
      {/if}
      <h2 id={titleId} class="title">{title}</h2>
      {#if headerActions}
        <div class="head-actions">{@render headerActions()}</div>
      {/if}
      {#if presentation === 'page' && primary}
        <button
          type="button"
          class="btn primary"
          disabled={primary.disabled || primary.busy}
          aria-busy={primary.busy ? 'true' : undefined}
          onclick={primary.onclick}
        >{primary.label}</button>
      {:else if (presentation === 'card' || presentation === 'sheet') && phone !== 'alert'}
        <button type="button" class="close" aria-label="Close" aria-disabled={locked ? 'true' : undefined} onclick={() => void requestClose()}>
          <Icon name="close" />
        </button>
      {/if}
    </header>
    <div class="body">
      {@render children()}
    </div>
    {#if showFooter}
      <footer class="foot">
        {#if footer}
          <div class="foot-secondary">{@render footer()}</div>
        {/if}
        {#if primary && presentation !== 'page'}
          <button
            type="button"
            class="btn primary"
            disabled={primary.disabled || primary.busy}
            aria-busy={primary.busy ? 'true' : undefined}
            onclick={primary.onclick}
          >{primary.label}</button>
        {/if}
      </footer>
    {/if}
  </div>
</dialog>

{#if guardOpen}
  <Modal
    title={dirtyMessage}
    phone="alert"
    size="sm"
    onclose={() => settleGuard(false)}
    primary={onsave ? { label: 'Save', onclick: () => void guardSave(), busy: guardSaving } : undefined}
  >
    {#if guardError}
      <p class="guard-error" role="alert">Could not save: {guardError}</p>
    {:else if dirtyMessage === DEFAULT_DIRTY_MESSAGE}
      <p class="guard-text">Your changes will be lost.</p>
    {/if}
    {#snippet footer()}
      <button type="button" class="btn danger" onclick={() => settleGuard(true)}>Discard</button>
      <span class="foot-spacer"></span>
      <button type="button" class="btn" data-autofocus onclick={() => settleGuard(false)}>Keep editing</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .modal {
    /* Keyboard and safe-area insets, with their fallbacks, live in custom
       properties so the padding rules below stay token-only. */
    --modal-kb: var(--kb-inset, 0px);
    --modal-safe-top: env(safe-area-inset-top, 0px);
    --modal-safe-bottom: env(safe-area-inset-bottom, 0px);
    --modal-safe-left: env(safe-area-inset-left, 0px);
    --modal-safe-right: env(safe-area-inset-right, 0px);
    --modal-gutter: var(--space-4);
    position: fixed;
    inset: 0;
    width: 100%;
    height: 100%;
    max-width: none;
    max-height: none;
    margin: 0;
    padding: 0;
    border: 0;
    box-sizing: border-box;
    background: var(--scrim);
    color: var(--text);
  }

  .modal::backdrop {
    background: transparent; /* The full-viewport overlay already draws the scrim. */
  }

  .modal[open] {
    display: flex;
    align-items: center;
    justify-content: center;
  }

  /* ---- Card (desktop, tablet, and every phone-less presentation) ---- */
  .panel {
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    width: min(480px, calc(100vw - 2 * var(--modal-gutter)));
    max-height: calc(100dvh - 48px);
    background: var(--surface-panel);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-modal);
    overflow: hidden;
  }

  .modal[data-size='sm'] .panel { width: min(360px, calc(100vw - 2 * var(--modal-gutter))); }
  .modal[data-size='lg'] .panel { width: min(640px, calc(100vw - 2 * var(--modal-gutter))); }
  .modal[data-size='xl'] .panel { width: min(960px, calc(100vw - 2 * var(--modal-gutter))); }

  .head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex: none;
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--border-subtle);
  }

  .title {
    flex: 1 1 auto;
    min-width: 0;
    margin: 0;
    font-size: var(--size-md);
    font-weight: 600;
    line-height: var(--leading);
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .head-actions {
    display: flex;
    align-items: center;
    gap: var(--space-1);
  }

  /* The body owns the scroller (designGuards "modal scrolling"). */
  .body {
    flex: 1 1 auto;
    min-height: 0;
    max-height: 100%;
    overflow-y: auto;
    padding: var(--space-4);
  }

  .foot {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex: none;
    padding: var(--space-3) var(--space-4);
    border-top: 1px solid var(--border-subtle);
  }

  /* Secondary actions left, primary right. */
  .foot-secondary {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex: 1 1 auto;
    min-width: 0;
  }

  .foot-spacer { flex: 1 1 auto; }

  .btn,
  .back,
  .close {
    font: inherit;
    font-size: var(--size-sm);
    line-height: var(--leading);
    color: var(--text);
    cursor: pointer;
  }

  .btn {
    padding: var(--space-1) var(--space-3);
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-panel);
  }

  .btn:hover:not(:disabled) { background: var(--surface-hover); }
  .btn:disabled { cursor: default; opacity: 0.6; }

  .btn.primary {
    background: var(--accent-action);
    border-color: var(--accent-action);
    color: var(--text-inverse);
  }

  .btn.primary:hover:not(:disabled) { background: var(--accent-hover); }

  .btn.danger {
    background: var(--danger);
    border-color: var(--danger);
    color: var(--text-inverse);
  }

  .btn.danger:hover:not(:disabled) { filter: brightness(1.1); }

  .close,
  .back {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-1);
    border: 0;
    border-radius: var(--radius);
    background: transparent;
  }

  .close:hover,
  .back:hover { background: var(--surface-hover); }

  .close[aria-disabled='true'],
  .back[aria-disabled='true'] { opacity: 0.4; cursor: default; }
  .close[aria-disabled='true']:hover,
  .back[aria-disabled='true']:hover { background: transparent; }

  .back {
    color: var(--accent-action);
    padding: var(--space-1) var(--space-2) var(--space-1) 0;
    white-space: nowrap;
  }

  .guard-text,
  .guard-error {
    margin: 0;
    font-size: var(--size);
    line-height: var(--leading);
  }

  .guard-text { color: var(--text-secondary); }
  .guard-error { color: var(--danger); }

  /* Tablet and phone: touch-sized controls. */
  .modal.touch .btn,
  .modal.touch .back,
  .modal.touch .close {
    min-height: var(--touch-target);
    min-width: var(--touch-target);
  }

  /* ---- Phone page: full screen ---- */
  .modal[data-presentation='page'] .panel {
    width: 100%;
    height: 100%;
    max-height: 100dvh;
    border-radius: 0;
    box-shadow: none;
    padding-top: var(--modal-safe-top);
    padding-left: var(--modal-safe-left);
    padding-right: var(--modal-safe-right);
  }

  .modal[data-presentation='page'] .title { text-align: center; }

  .modal[data-presentation='page'] .body,
  .modal[data-presentation='sheet'] .body {
    padding-bottom: calc(var(--space-4) + var(--modal-kb) + var(--modal-safe-bottom));
  }

  /* With a footer below it, the inset moves to the footer. */
  .modal[data-presentation='page'] .body:has(+ .foot),
  .modal[data-presentation='sheet'] .body:has(+ .foot) { padding-bottom: var(--space-4); }

  .modal[data-presentation='page'] .foot,
  .modal[data-presentation='sheet'] .foot {
    padding-bottom: calc(var(--space-3) + var(--modal-kb) + var(--modal-safe-bottom));
  }

  /* ---- Phone sheet: bottom, rounded top, grab handle ---- */
  .modal[data-presentation='sheet'][open] { align-items: flex-end; }

  .modal[data-presentation='sheet'] .panel {
    width: 100%;
    max-height: 90dvh;
    border-radius: var(--radius-lg) var(--radius-lg) 0 0;
  }

  .grab {
    display: flex;
    justify-content: center;
    flex: none;
    padding: var(--space-2) 0 var(--space-1);
    touch-action: none;
    cursor: grab;
  }

  .grab-bar {
    display: block;
    width: var(--space-8);
    height: var(--space-1);
    border-radius: var(--radius-sm);
    background: var(--border-strong);
  }

  .modal[data-presentation='sheet'] .head { border-bottom: 0; padding-top: 0; }

  /* ---- Phone alert: small centred card, stacked full-width buttons ---- */
  .modal[data-presentation='alert'] .panel { width: min(320px, calc(100vw - 2 * var(--modal-gutter))); }

  .modal[data-presentation='alert'] .head { border-bottom: 0; }
  .modal[data-presentation='alert'] .body { padding-top: 0; }

  .modal[data-presentation='alert'] .foot,
  .modal[data-presentation='alert'] .foot-secondary {
    flex-direction: column;
    align-items: stretch;
  }

  .modal[data-presentation='alert'] .foot { border-top: 0; }
  .modal[data-presentation='alert'] :global(.foot-spacer) { display: none; }
  .modal[data-presentation='alert'] .btn { width: 100%; }
</style>
