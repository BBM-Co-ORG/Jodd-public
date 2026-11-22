<script lang="ts">
  // The editor's ⌘K link prompt: markup and styles only. NoteEditor.svelte
  // keeps the selection it will wrap and decides what Add link does; this
  // reports keys and clicks back through the callbacks below. Split out of
  // NoteEditor.svelte 2026-11-16.

  interface Props {
    /** The link's text, asked for only when nothing is selected. */
    text: string;
    url: string;
    urlEl?: HTMLInputElement | null;
    /** A selection becomes the link's text, so no text field. */
    hasSelection: boolean;
    /** Android: every control gets the 48px touch target. */
    touch: boolean;
    /** Keys in either input: Enter adds, Escape cancels. */
    onkey: (e: KeyboardEvent) => void;
    onapply: () => void;
    oncancel: () => void;
  }

  let {
    text = $bindable(),
    url = $bindable(),
    urlEl = $bindable(),
    hasSelection,
    touch,
    onkey,
    onapply,
    oncancel,
  }: Props = $props();
</script>

<div class="link-prompt" class:touch>
  {#if !hasSelection}
    <input
      type="text"
      bind:value={text}
      placeholder="Link text"
      aria-label="Link text"
      class="link-prompt-input link-prompt-text"
      onkeydown={onkey}
    />
  {/if}
  <input
    type="url"
    bind:value={url}
    bind:this={urlEl}
    placeholder="https://example.com"
    aria-label="Link address"
    class="link-prompt-input link-prompt-url"
    onkeydown={onkey}
  />
  <button type="button" class="link-prompt-btn" onclick={onapply}>Add link</button>
  <button type="button" class="link-prompt-btn link-prompt-cancel" onclick={oncancel}>Cancel</button>
</div>

<style>
  .link-prompt {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 8px 12px;
    background: var(--surface-list);
    border-bottom: 1px solid var(--border-subtle);
  }
  .link-prompt-input {
    flex: 1;
    padding: 5px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    font-size: var(--size);
    line-height: var(--leading);
    background: var(--surface-panel);
    color: var(--text);
    font-family: inherit;
  }
  .link-prompt-text {
    flex: 0 0 30%;
  }
  .link-prompt-input:focus {
    outline: none;
    border-color: var(--focus);
    box-shadow: var(--ring-focus);
  }
  .link-prompt-btn {
    min-height: var(--hit-target-desktop);
    padding: 5px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-panel);
    color: var(--text);
    font-size: var(--size-sm);
    cursor: pointer;
  }
  .link-prompt-btn:hover {
    background: var(--accent-tint);
  }
  .link-prompt-cancel {
    color: var(--text-muted);
  }

  /* Android: a finger-sized hit area (--touch-target, 48px); the desktop
     sizes are untouched. NoteEditor's toolbar and FindBar carry the same rule. */
  .link-prompt.touch button {
    min-height: var(--touch-target);
    min-width: var(--touch-target);
  }
  .link-prompt.touch .link-prompt-input {
    min-height: var(--touch-target);
  }
</style>
