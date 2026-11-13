<script lang="ts">
  // The inline name prompt and confirm, shared by every component that asks
  // them (the Sidebar's folder, tag and remove-account flows; AccountAddHost's
  // Local Folder flow). A caller mounts one with `bind:this` and keeps its
  // own `askName()` / `askConfirm()` call sites, which forward here.
  //
  // window.prompt() is not implemented in macOS WKWebView and native
  // confirm() is unreliable there, so both are Modal-based: Modal owns Esc,
  // the backdrop, focus and the Android Back entry. The confirm is the shared
  // ConfirmDialog, including its optional checkbox (Managed SSH removal's
  // "Also remove Jodd's key"), whose state the caller may bind.
  import Modal from './Modal.svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import type { ConfirmOptions } from '../controllers/confirmGate';

  let { checkboxChecked = $bindable(false) }: { checkboxChecked?: boolean } = $props();

  // ─── Name prompt ──────────────────────────────────────────────────────────
  let promptOpen = $state(false);
  let promptTitle = $state('');
  let promptValue = $state('');
  let promptInputEl: HTMLInputElement | undefined = $state();
  let promptResolve: ((value: string | null) => void) | null = null;

  /** Resolves with the trimmed value, or null for Cancel, Esc or an empty value. */
  export function askName(title: string, defaultValue: string = ''): Promise<string | null> {
    promptTitle = title;
    promptValue = defaultValue;
    promptOpen = true;
    // Focus the input next frame, after Svelte renders the dialog.
    setTimeout(() => {
      promptInputEl?.focus();
      promptInputEl?.select();
    }, 0);
    return new Promise((resolve) => { promptResolve = resolve; });
  }
  function promptOk() {
    const v = promptValue.trim();
    promptOpen = false;
    promptResolve?.(v.length ? v : null);
    promptResolve = null;
  }
  function promptCancel() {
    promptOpen = false;
    promptResolve?.(null);
    promptResolve = null;
  }
  function onPromptKey(e: KeyboardEvent) {
    // Enter that commits an IME composition (Thai on Gboard) is not a confirm.
    if (e.key === 'Enter' && !e.isComposing) { e.preventDefault(); promptOk(); }
  }

  // ─── Confirm ──────────────────────────────────────────────────────────────
  let confirmOpen = $state(false);
  let confirmTitle = $state('');
  let confirmMessage = $state('');
  // null hides the checkbox, which is every confirmation but one.
  let confirmCheckboxLabel: string | null = $state(null);
  let confirmDestructive = $state(false);
  let confirmButtonLabel = $state('OK');
  let confirmResolve: ((value: boolean) => void) | null = null;

  /**
   * `options` styles the confirm button: a destructive one (remove account,
   * delete folder or tag) is danger-styled and labelled with its verb, never
   * a primary "OK".
   */
  export function askConfirm(
    title: string,
    message: string,
    checkboxLabel: string | null = null,
    options: ConfirmOptions = {},
  ): Promise<boolean> {
    confirmTitle = title;
    confirmMessage = message;
    confirmCheckboxLabel = checkboxLabel;
    confirmDestructive = options.destructive ?? false;
    confirmButtonLabel = options.confirmLabel ?? 'OK';
    confirmOpen = true;
    return new Promise((resolve) => { confirmResolve = resolve; });
  }
  function confirmOk() {
    confirmOpen = false;
    confirmResolve?.(true);
    confirmResolve = null;
  }
  function confirmCancel() {
    confirmOpen = false;
    confirmResolve?.(false);
    confirmResolve = null;
  }
</script>

{#if promptOpen}
  <Modal size="sm" phone="alert" title={promptTitle} onclose={promptCancel}>
    <input
      bind:this={promptInputEl}
      bind:value={promptValue}
      class="prompt-input"
      type="text"
      data-autofocus
      onkeydown={onPromptKey}
    />
    {#snippet footer()}
      <div class="prompt-actions">
        <button class="prompt-btn" onclick={promptCancel}>Cancel</button>
        <button class="prompt-btn primary" onclick={promptOk}>OK</button>
      </div>
    {/snippet}
  </Modal>
{/if}

{#if confirmOpen}
  <ConfirmDialog
    title={confirmTitle}
    message={confirmMessage}
    checkboxLabel={confirmCheckboxLabel}
    destructive={confirmDestructive}
    confirmLabel={confirmButtonLabel}
    bind:checkboxChecked
    onConfirm={confirmOk}
    onCancel={confirmCancel}
  />
{/if}

<style>
  .prompt-input {
    width: 100%;
    box-sizing: border-box;
    padding: var(--space-1-5) var(--space-2);
    font-size: var(--size);
    line-height: var(--leading);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    outline: none;
    /* An <input> with no background falls back to the user agent's own white,
       which does not follow the theme. That put --text (near-white in dark
       mode) on a white field and made this dialog unreadable. A form control
       must always state its background explicitly — inheriting is not an
       option, because the UA stylesheet wins over inheritance here. */
    background: var(--surface-panel);
    color: var(--text);
    font-family: inherit;
  }

  .prompt-input::placeholder {
    color: var(--text-muted);
  }

  .prompt-input:focus {
    border-color: var(--focus);
    box-shadow: var(--ring-focus);
  }

  /* Cancel then OK, together on the right of Modal's footer. */
  .prompt-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
    width: 100%;
  }

  .prompt-btn {
    padding: var(--space-1) var(--space-3);
    font-size: var(--size-sm);
    border-radius: var(--radius);
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

  /* Touch-sized on Android, matching Modal's own buttons. */
  :global(dialog.touch) .prompt-btn {
    min-height: var(--touch-target);
    min-width: var(--touch-target);
  }
</style>
