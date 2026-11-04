<script module lang="ts">
  /** One row of the Sidebar's Agents group, as the AI ✦ sheet lists it. */
  export interface WorkflowEntry {
    accountId: string;
    path: string;
    /** Display name, `__name__` markers already stripped. */
    name: string;
    depth: number;
    /** The account's short name when more than one is active, else ''. */
    account: string;
  }
</script>

<script lang="ts">
  // The phone Folders bar's AI ✦ (spec 2026-10-20 §5.1, 2026-10-28 §5): the
  // AI menu (AiMenu, the same list the desktop footer's popover shows), then
  // the Agents group (agent folders) the phone Folders screen does not show
  // (2026-10-20 §5.2, 2026-10-28 §6).
  // Each entry opens what it opened before — the same stores and the same
  // folder pick — so this sheet adds a way in, not a second implementation.
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';
  import AiMenu from './AiMenu.svelte';
  import { viewName } from '../viewName';
  import type { Pane } from '../stores/phoneNav';

  let {
    workflows,
    onpick,
    onclose,
  }: {
    workflows: WorkflowEntry[];
    /** Open a workflow folder (the Sidebar's own folder pick). */
    onpick: (accountId: string, path: string) => void;
    onclose: () => void;
  } = $props();

  type SheetModal = { requestClose(): Promise<boolean>; closeInto(pane: Pane, title?: string): boolean };
  let modal: SheetModal | undefined = $state();

  // Close first, then open: the sheet's history entry is popped and the next
  // modal's entry is queued behind that back (phoneNav serializes them).
  function run(action: () => void) {
    void modal?.requestClose();
    action();
  }
  // A workflow folder is a list: the sheet's entry becomes that list's, so
  // Back from it returns to Folders, not to a dismissed sheet.
  function pick(w: WorkflowEntry) {
    if (modal && !modal.closeInto('list', viewName(null, w.path, ''))) return;
    onpick(w.accountId, w.path);
  }
</script>

<Modal bind:this={modal} phone="sheet" title="AI" {onclose}>
  <div class="sheet-menu" role="menu">
    <AiMenu {run} iconSize={20} />
    {#if workflows.length > 0}
      <div class="sep"></div>
      <div class="group" aria-hidden="true">Agents</div>
      {#each workflows as w (w.accountId + '\u0000' + w.path)}
        <button
          class="item"
          type="button"
          style="padding-left: calc(var(--space-3) + {Math.max(0, w.depth - 1)} * var(--space-4))"
          onclick={() => pick(w)}
        >
          <span class="icon"><Icon name="folder" size={20} /></span>
          <span class="label">{w.name}</span>
          {#if w.account}<span class="account">{w.account}</span>{/if}
        </button>
      {/each}
    {/if}
  </div>
</Modal>

<style>
  /* Rows: src/styles/sheet-menu.css (shared by every sheet menu). */
  .sheet-menu .icon {
    color: var(--accent-action);
  }
  .account {
    font-size: var(--size-xs);
    color: var(--text-muted);
  }
  .group {
    padding: var(--space-2) var(--space-3) var(--space-1);
    font-size: var(--size-xs);
    font-weight: 600;
    text-transform: uppercase;
    color: var(--text-muted);
  }
</style>
