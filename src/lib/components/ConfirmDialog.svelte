<script lang="ts">
  import { onMount } from 'svelte';

  // The native modal top layer makes background controls inert, including
  // when this component is nested inside a context menu. Callers own the
  // Promise/state; stop keyboard bubbling before their window handlers run.
  export let title: string;
  export let message: string;
  export let confirmLabel: string = 'OK';
  export let cancelLabel: string = 'Cancel';
  // Danger styling (red) for irreversible actions like a permanent delete;
  // default is the neutral primary (accent) styling used for ordinary
  // confirmations (e.g. Sidebar's "Delete folder?", "Remove account?").
  export let destructive: boolean = false;
  export let onConfirm: () => void;
  export let onCancel: () => void;

  let dialog: HTMLDialogElement;
  let cancelButton: HTMLButtonElement;
  let confirmButton: HTMLButtonElement;
  let settled = false;

  function finish(confirmed: boolean) {
    if (settled) return;
    settled = true;
    if (confirmed) onConfirm();
    else onCancel();
  }

  onMount(() => {
    const opener = document.activeElement;
    dialog.showModal();
    cancelButton.focus();
    return () => {
      dialog.close();
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    };
  });

  function onKey(e: KeyboardEvent) {
    e.stopPropagation(); // Includes app shortcuts; the modal owns interaction.
    if (e.key === 'Escape') {
      e.preventDefault();
      finish(false);
    } else if (e.key === 'Tab') {
      if (e.shiftKey && document.activeElement === cancelButton) {
        e.preventDefault(); confirmButton.focus();
      } else if (!e.shiftKey && document.activeElement === confirmButton) {
        e.preventDefault(); cancelButton.focus();
      }
    }
    // Enter/Space use native activation of the focused button.
  }
</script>

<dialog
  bind:this={dialog}
  class="prompt-overlay"
  aria-modal="true"
  aria-label={title}
  oncancel={(e) => { e.preventDefault(); e.stopPropagation(); finish(false); }}
  onclick={(e) => { if (e.target === e.currentTarget) finish(false); }}
  onkeydown={onKey}
>
  <div class="prompt-dialog">
    <div class="prompt-title">{title}</div>
    <div class="confirm-message">{message}</div>
    <div class="prompt-actions">
      <button type="button" bind:this={cancelButton} class="prompt-btn" onclick={() => finish(false)}>{cancelLabel}</button>
      <button
        class="prompt-btn"
        class:primary={!destructive}
        class:danger={destructive}
        type="button"
        bind:this={confirmButton}
        onclick={() => finish(true)}
      >
        {confirmLabel}
      </button>
    </div>
  </div>
</dialog>

<style>
  .prompt-overlay {
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
  }

  .prompt-overlay::backdrop {
    background: transparent; /* The full-viewport overlay already draws the scrim. */
  }

  .prompt-overlay[open] {
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 2000;
  }

  .prompt-dialog {
    background: var(--surface-panel);
    min-width: min(320px, 80vw);
    box-sizing: border-box;
    max-width: 80vw;
    /* designGuards.test.ts "modal scrolling": every role="dialog" component
       needs a bound + a scroller somewhere in its own stylesheet. Message
       text here is normally one or two lines, but nothing enforces that at
       the call site (a future caller could pass something long), and the
       overlay's fixed/inset:0 centering means an overgrown panel would
       overflow at both ends with no way to reach the buttons. */
    max-height: 80vh;
    overflow-y: auto;
    padding: 18px 20px 14px;
    border-radius: 10px;
    box-shadow: var(--shadow-modal);
  }

  .prompt-title {
    font-size: var(--size);
    font-weight: 600;
    color: var(--text);
    line-height: var(--leading);
    margin-bottom: 10px;
  }

  .confirm-message {
    font-size: var(--size);
    color: var(--text-secondary);
    line-height: var(--leading);
  }

  .prompt-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 12px;
  }

  .prompt-btn {
    padding: 5px 14px;
    font-size: var(--size-sm);
    border-radius: 5px;
    border: 1px solid var(--border);
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
  }

  .prompt-btn:hover {
    background: var(--surface-list);
  }

  .prompt-btn.primary {
    background: var(--accent-action);
    color: var(--text-inverse);
    border-color: var(--accent-action);
  }

  .prompt-btn.primary:hover {
    background: var(--accent-hover);
  }

  .prompt-btn.danger {
    background: var(--danger);
    color: var(--text-inverse);
    border-color: var(--danger);
  }

  .prompt-btn.danger:hover {
    filter: brightness(1.1);
  }
</style>
