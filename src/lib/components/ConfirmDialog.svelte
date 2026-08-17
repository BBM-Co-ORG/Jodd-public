<script lang="ts">
  // Shared in-DOM confirm overlay. Tauri's WKWebView makes native confirm()
  // unreliable, so every call site rendered its own styled dialog instead —
  // this was copied verbatim into Sidebar.svelte's askConfirm and
  // NoteContextMenu.svelte's single-note permanent-delete dialog (Svelte's
  // per-component style scoping means a shared CSS class alone doesn't
  // dedupe it). Task 10b was about to land two more copies — NoteContextMenu's
  // batch delete and NoteEditor's trash icon — so it extracts this first.
  //
  // Presentation only. The caller owns the open/closed state and the
  // Promise<boolean> plumbing (the askXConfirm()-shaped helper each call site
  // already has) and passes onConfirm/onCancel to resolve it. Callers that
  // can be unmounted by an ancestor's outside-click/Esc handler while this is
  // open (NoteContextMenu, whose onClose() nulls the menu prop one level up)
  // must keep their own no-op guard so that handler doesn't fire while this
  // dialog owns the interaction — otherwise the awaited confirm Promise never
  // resolves and the caller's action wedges. See NoteContextMenu's
  // onPointerDown/onKey for that guard; it stays there, not here, because it
  // depends on each caller's own outside-click/Esc listener.
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

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Enter') { e.preventDefault(); onConfirm(); }
    else if (e.key === 'Escape') { e.preventDefault(); onCancel(); }
  }
</script>

<div
  class="prompt-overlay"
  role="dialog"
  aria-modal="true"
  onclick={(e) => { if (e.target === e.currentTarget) onCancel(); }}
  onkeydown={onKey}
  tabindex="-1"
>
  <div class="prompt-dialog">
    <div class="prompt-title">{title}</div>
    <div class="confirm-message">{message}</div>
    <div class="prompt-actions">
      <button class="prompt-btn" onclick={onCancel}>{cancelLabel}</button>
      <button
        class="prompt-btn"
        class:primary={!destructive}
        class:danger={destructive}
        onclick={onConfirm}
      >
        {confirmLabel}
      </button>
    </div>
  </div>
</div>

<style>
  .prompt-overlay {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 2000;
  }

  .prompt-dialog {
    background: var(--surface-panel);
    min-width: 320px;
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
