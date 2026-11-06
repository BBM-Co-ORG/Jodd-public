<script lang="ts">
  import Modal from './Modal.svelte';

  // Rendered through the shared Modal primitive: a small card everywhere, a
  // centred alert on an Android phone. Modal owns the native <dialog> (inert
  // background, Tab trap, opener refocus), the Esc/backdrop/Android-Back close
  // path and the stopped keyboard bubbling that keeps app shortcuts from
  // firing. Callers own the Promise/state; this component only reports one
  // decision, exactly once.
  export let title: string;
  export let message: string;
  export let confirmLabel: string = 'OK';
  export let cancelLabel: string = 'Cancel';
  // Danger styling (red), never the primary, for a destructive action: a
  // permanent delete, "Remove account?", "Delete folder?", "Delete tag?"
  // (PromptDialogs' askConfirm options). Default is the neutral primary
  // (accent) styling for an ordinary confirmation.
  export let destructive: boolean = false;
  export let onConfirm: () => void;
  export let onCancel: () => void;
  // Optional checkbox slot (e.g. Sidebar's "Also remove Jodd's key from the
  // server" on a Managed SSH account's removal). Defaults to not rendering a
  // checkbox at all, so every other caller of this component is unaffected.
  export let checkboxLabel: string | null = null;
  export let checkboxChecked: boolean = false;

  let settled = false;

  function finish(confirmed: boolean) {
    if (settled) return;
    settled = true;
    if (confirmed) onConfirm();
    else onCancel();
  }
</script>

<Modal size="sm" phone="alert" {title} onclose={() => finish(false)}>
  <div class="confirm-message">{message}</div>
  {#if checkboxLabel}
    <label class="confirm-checkbox">
      <input type="checkbox" bind:checked={checkboxChecked} />
      {checkboxLabel}
    </label>
  {/if}
  {#snippet footer()}
    <div class="confirm-actions">
      <button type="button" class="confirm-btn" data-autofocus onclick={() => finish(false)}>{cancelLabel}</button>
      <button
        type="button"
        class="confirm-btn"
        class:primary={!destructive}
        class:danger={destructive}
        onclick={() => finish(true)}
      >
        {confirmLabel}
      </button>
    </div>
  {/snippet}
</Modal>

<style>
  .confirm-message {
    font-size: var(--size);
    color: var(--text-secondary);
    line-height: var(--leading);
  }

  .confirm-checkbox {
    display: flex;
    gap: var(--space-1-5);
    align-items: center;
    margin-top: var(--space-3);
    font-size: var(--size);
    color: var(--text);
  }

  /* Cancel then confirm, together on the right of Modal's footer. */
  .confirm-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
    width: 100%;
  }

  .confirm-btn {
    padding: var(--space-1) var(--space-3);
    font: inherit;
    font-size: var(--size-sm);
    line-height: var(--leading);
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
  }

  .confirm-btn:hover {
    background: var(--surface-hover);
  }

  .confirm-btn.primary {
    background: var(--accent-action);
    color: var(--text-inverse);
    border-color: var(--accent-action);
  }

  .confirm-btn.primary:hover {
    background: var(--accent-hover);
  }

  .confirm-btn.danger {
    background: var(--danger);
    color: var(--text-inverse);
    border-color: var(--danger);
  }

  .confirm-btn.danger:hover {
    filter: brightness(1.1);
  }

  /* Touch-sized on Android, matching Modal's own buttons (Modal sets
     class="touch" on its <dialog>). */
  :global(dialog.touch) .confirm-btn {
    min-height: var(--touch-target);
    min-width: var(--touch-target);
  }
</style>
