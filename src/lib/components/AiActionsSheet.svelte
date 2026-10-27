<script module lang="ts">
  /** One row of the Sidebar's Workflows group, as the AI ✦ sheet lists it. */
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
  // The phone Folders bar's AI ✦ (spec 2026-10-20 §5.1): Ask Jodd, Extract,
  // and the Workflows group the phone Folders screen no longer shows (§5.2).
  // Each entry opens what it opened before — the same stores and the same
  // folder pick — so this sheet adds a way in, not a second implementation.
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';
  import { askModalOpen, extractModalOpen } from '../stores/ui';
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
  function openAsk() {
    void modal?.requestClose();
    askModalOpen.set(true);
  }
  function openExtract() {
    void modal?.requestClose();
    extractModalOpen.set(true);
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
    <button class="item" type="button" onclick={openAsk}>
      <span class="icon"><Icon name="chat" size={20} /></span>
      <span class="label">Ask Jodd</span>
    </button>
    <button class="item" type="button" onclick={openExtract}>
      <span class="icon"><Icon name="bulb" size={20} /></span>
      <span class="label">Extract</span>
    </button>
    {#if workflows.length > 0}
      <div class="sep"></div>
      <div class="group" aria-hidden="true">Workflows</div>
      {#each workflows as w (w.accountId + '\u0000' + w.path)}
        <button
          class="item"
          type="button"
          style="padding-left: calc(var(--space-3) + {Math.max(0, w.depth - 1)} * var(--space-4))"
          onclick={() => pick(w)}
        >
          <span class="icon"><Icon name="bulb" size={20} /></span>
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
