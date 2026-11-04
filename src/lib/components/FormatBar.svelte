<script module lang="ts">
  /** The bar's commands, in the order the bar shows them (spec 2026-10-20 §5.6). */
  export const FORMAT_COMMANDS = [
    'heading',
    'bold',
    'italic',
    'underline',
    'strike',
    'bullet',
    'numbered',
    'task',
    'quote',
    'code',
    'link',
    'hashtag',
    'wikilink',
    'dismiss',
  ] as const;

  export type FormatCommand = (typeof FORMAT_COMMANDS)[number];

  /** The bar's height in px: `--touch-target`, for script that places things
   *  above it (the `[[` picker). Keep in step with tokens.css. */
  export const FORMAT_BAR_HEIGHT = 48;
</script>

<script lang="ts">
  // The phone format bar (spec 2026-10-20 §5.6): one horizontally scrollable
  // 48px row pinned above the soft keyboard, shown by NoteEditor while the
  // title or the body has focus. It only presents buttons: every command is
  // NoteEditor's own handler, passed in — the same ones the desktop toolbar
  // calls — so formatting has one implementation.
  //
  // A tap must not take focus from the editor, or the command runs against
  // nothing (and the keyboard drops). `pointerdown` + preventDefault is the
  // touch equivalent of the toolbar's `onmousedown` one; the mousedown one is
  // kept too, because a tap's compatibility mousedown is where WebKit and
  // Chromium move focus, and a mouse on a Chromebook-class device sends no
  // cancelled pointerdown first.
  import Icon, { type IconName } from './Icon.svelte';

  let {
    commands,
    disabled = [],
  }: {
    commands: Record<FormatCommand, () => void>;
    /** Commands that would do the wrong thing where the caret is (NoteEditor:
     *  every body-only one while the title has focus). Shown dimmed with
     *  aria-disabled — not `disabled`, which would stop the pointerdown that
     *  keeps the caret put — and a tap does nothing. */
    disabled?: readonly FormatCommand[];
  } = $props();

  function run(command: FormatCommand): void {
    if (disabled.includes(command)) return;
    commands[command]();
  }

  type Item = { command: FormatCommand; label: string; icon?: IconName; glyph?: string; glyphClass?: string };

  const ITEMS: Item[] = [
    { command: 'heading', label: 'Text style', glyph: 'Aa' },
    { command: 'bold', label: 'Bold', glyph: 'B', glyphClass: 'b' },
    { command: 'italic', label: 'Italic', glyph: 'I', glyphClass: 'i' },
    { command: 'underline', label: 'Underline', glyph: 'U', glyphClass: 'u' },
    { command: 'strike', label: 'Strikethrough', glyph: 'S', glyphClass: 's' },
    { command: 'bullet', label: 'Bullet list', glyph: '•' },
    { command: 'numbered', label: 'Numbered list', glyph: '1.' },
    { command: 'task', label: 'Checklist', icon: 'checkbox' },
    { command: 'quote', label: 'Blockquote', icon: 'quote' },
    { command: 'code', label: 'Inline code', glyph: '</>', glyphClass: 'mono' },
    { command: 'link', label: 'Link', icon: 'link' },
    { command: 'hashtag', label: 'Tag', glyph: '#' },
    { command: 'wikilink', label: 'Link to a note', glyph: '[[' },
    { command: 'dismiss', label: 'Hide keyboard', icon: 'chevron-down' },
  ];

  function keepFocus(e: Event): void {
    e.preventDefault();
  }
</script>

<div class="format-bar" data-testid="format-bar" role="toolbar" aria-label="Formatting">
  {#each ITEMS as item (item.command)}
    <button
      type="button"
      data-command={item.command}
      aria-label={item.label}
      title={item.label}
      onpointerdown={keepFocus}
      onmousedown={keepFocus}
      aria-disabled={disabled.includes(item.command) ? 'true' : undefined}
      onclick={() => run(item.command)}
    >
      {#if item.icon}
        <Icon name={item.icon} size={20} />
      {:else}
        <span class="glyph {item.glyphClass ?? ''}" aria-hidden="true">{item.glyph}</span>
      {/if}
    </button>
  {/each}
</div>

<style>
  .format-bar {
    position: fixed;
    /* Fixed, so the layout's side insets do not reach it: its own. */
    left: env(safe-area-inset-left);
    right: env(safe-area-inset-right);
    /* On the keyboard, which covers the navigation bar; with no keyboard
       (one that closed under a focused body), above the navigation bar. */
    bottom: max(var(--kb-inset, 0px), env(safe-area-inset-bottom));
    z-index: var(--z-sticky);
    height: var(--touch-target);
    display: flex;
    align-items: center;
    overflow-x: auto;
    overflow-y: hidden;
    overscroll-behavior-x: contain;
    scrollbar-width: none;
    background: var(--surface-panel);
    border-top: 1px solid var(--border);
    box-shadow: var(--shadow-menu-up);
    padding: 0 var(--space-1);
  }
  .format-bar::-webkit-scrollbar {
    display: none;
  }

  .format-bar button {
    flex: 0 0 auto;
    width: var(--touch-target);
    height: var(--touch-target);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    margin: 0;
    background: none;
    border: none;
    border-radius: var(--radius-sm);
    color: var(--text);
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
  }
  .format-bar button:active:not([aria-disabled='true']) {
    background: var(--surface-active);
  }
  .format-bar button[aria-disabled='true'] {
    color: var(--text-muted);
    opacity: 0.5;
    cursor: default;
  }

  /* Spec §5.6: glyphs at 20px, the same box as the 20px icons. */
  .glyph {
    font-size: calc(var(--size-lg) + var(--space-1));
    line-height: var(--leading);
    white-space: nowrap;
  }
  .glyph.b { font-weight: 700; }
  .glyph.i { font-style: italic; }
  .glyph.u { text-decoration: underline; }
  .glyph.s { text-decoration: line-through; }
  .glyph.mono {
    font-family: var(--font-mono);
    font-size: var(--size-md);
  }
</style>
