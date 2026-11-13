<script lang="ts" module>
  // All geometry on a 16x16 grid. `d` entries are stroked, never filled, so
  // every glyph inherits currentColor and follows the theme.
  const PATHS = {
    'folder':        ['M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z'],
    'folder-plus':   ['M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z', 'M8 7v3.5', 'M6.25 8.75h3.5'],
    'eye':           ['M1.5 8S3.9 3.5 8 3.5 14.5 8 14.5 8 12.1 12.5 8 12.5 1.5 8 1.5 8z', 'M8 10a2 2 0 1 0 0-4 2 2 0 0 0 0 4z'],
    'search':        ['M7 12A5 5 0 1 0 7 2a5 5 0 0 0 0 10z', 'M10.6 10.6 14 14'],
    'clock':         ['M8 14.5a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13z', 'M8 4.5V8l2.5 1.5'],
    'trash':         ['M2.5 4h11', 'M6.5 4V2.5h3V4', 'M3.5 4l.8 9.5h7.4L12.5 4'],
    'gear':          ['M8 10.2a2.2 2.2 0 1 0 0-4.4 2.2 2.2 0 0 0 0 4.4z', 'M13 8c0-.4 0-.8-.1-1.1l1.3-1-1.5-2.6-1.5.6a5.4 5.4 0 0 0-2-1.1L9 1.2H6l-.2 1.6c-.7.2-1.4.6-2 1.1l-1.5-.6L.8 5.9l1.3 1a5.6 5.6 0 0 0 0 2.2l-1.3 1 1.5 2.6 1.5-.6c.6.5 1.3.9 2 1.1l.2 1.6h3l.2-1.6c.7-.2 1.4-.6 2-1.1l1.5.6 1.5-2.6-1.3-1c.1-.3.1-.7.1-1.1z'],
    'close':         ['M4 4l8 8', 'M12 4l-8 8'],
    'pencil':        ['M11.3 2.7a1.7 1.7 0 0 1 2.4 2.4L5.5 13.3 2 14l.7-3.5z', 'M10 4l2 2'],
    'note-plus':     ['M12.5 7.5v-3L9.5 1.5H4a1 1 0 0 0-1 1v11a1 1 0 0 0 1 1h4', 'M9.5 1.5v3h3', 'M12 10v5', 'M9.5 12.5h5'],
    'chevron-right': ['M6 3.5L10.5 8 6 12.5'],
    'chevron-down':  ['M3.5 6L8 10.5 12.5 6'],
    'chevron-left':  ['M10 3.5L5.5 8 10 12.5'],
    'more':          ['M3.5 7.2a.8.8 0 1 0 0 1.6.8.8 0 0 0 0-1.6z', 'M8 7.2a.8.8 0 1 0 0 1.6.8.8 0 0 0 0-1.6z', 'M12.5 7.2a.8.8 0 1 0 0 1.6.8.8 0 0 0 0-1.6z'],
    'pin':           ['M6 1.5h4l-.5 4 2.5 2.5H4L6.5 5.5z', 'M8 8v6.5'],
    'graph':         ['M8 6.5a2 2 0 1 0 0-4 2 2 0 0 0 0 4z', 'M3 14.5a1.8 1.8 0 1 0 0-3.6 1.8 1.8 0 0 0 0 3.6z', 'M13 14.5a1.8 1.8 0 1 0 0-3.6 1.8 1.8 0 0 0 0 3.6z', 'M6.7 6.2 4 11', 'M9.3 6.2 12 11'],
    'person':        ['M8 8a3 3 0 1 0 0-6 3 3 0 0 0 0 6z', 'M2.5 14.5c0-2.8 2.5-4.5 5.5-4.5s5.5 1.7 5.5 4.5'],
    'check':         ['M3 8.5L6.5 12 13 4.5'],
    'link':          ['M6.8 9.2a2.8 2.8 0 0 0 4 0l2.3-2.3a2.8 2.8 0 1 0-4-4l-1 1', 'M9.2 6.8a2.8 2.8 0 0 0-4 0L2.9 9.1a2.8 2.8 0 1 0 4 4l1-1'],
    'quote':         ['M6.5 4.5C4.6 5.3 3.5 6.8 3.5 8.6c0 1.6 1 2.9 2.4 2.9 1.2 0 2.1-.9 2.1-2.1 0-1.1-.8-2-1.9-2-.2 0-.4 0-.6.1.2-1 1-1.9 2.2-2.4z', 'M13 4.5c-1.9.8-3 2.3-3 4.1 0 1.6 1 2.9 2.4 2.9 1.2 0 2.1-.9 2.1-2.1 0-1.1-.8-2-1.9-2-.2 0-.4 0-.6.1.2-1 1-1.9 2.2-2.4z'],
    'checkbox':      ['M3 2.5h10a.5.5 0 0 1 .5.5v10a.5.5 0 0 1-.5.5H3a.5.5 0 0 1-.5-.5V3a.5.5 0 0 1 .5-.5z'],
    'paperclip':     ['M13 7.3 8 12.3a3.2 3.2 0 0 1-4.5-4.5l5.4-5.4a2.1 2.1 0 0 1 3 3l-5.4 5.4a1 1 0 0 1-1.5-1.5l5-5'],
    'restore':       ['M2.5 8a5.5 5.5 0 1 0 1.7-4', 'M2 2.5V6h3.5'],
    'tag':           ['M2.5 2.5h5L14 9l-5 5-6.5-6.5z', 'M5.2 5.2h.01'],
    'copy':          ['M5.5 5.5h7a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1z',
                       'M2.5 10.5h-.5a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1h7a1 1 0 0 1 1 1v.5'],
    'refresh':       ['M13.5 8a5.5 5.5 0 1 1-1.7-4', 'M14 2.5V6h-3.5'],
    'eye-off':       ['M6.6 3.7A6.5 6.5 0 0 1 8 3.5c4.1 0 6.5 4.5 6.5 4.5a12 12 0 0 1-2.2 2.8',
                       'M4.3 4.8A12.4 12.4 0 0 0 1.5 8S3.9 12.5 8 12.5c1 0 1.9-.3 2.7-.7',
                       'M9.4 9.4a2 2 0 0 1-2.8-2.8', 'M2 2l12 12'],
    // AI actions that make something (spec 2026-10-28 §9): a four-point star
    // and a small plus beside it.
    'sparkle':       ['M7 3.5C7.4 7 8.5 8.6 12.5 9 8.5 9.4 7.4 11 7 14.5 6.6 11 5.5 9.4 1.5 9 5.5 8.6 6.6 7 7 3.5z', 'M12.5 1.5v4', 'M10.5 3.5h4'],
    'chat':          ['M2.5 3.5h11a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H6.5l-2.5 2.5v-2.5h-1.5a1 1 0 0 1-1-1v-6a1 1 0 0 1 1-1z'],
    // Find related notes: two notes side by side, joined. Not `graph`, which
    // is Show graph's, two rows above it in the ⋯ menu; and side by side, not
    // overlapping, so it does not read as `copy` (Duplicate).
    'related':       ['M1.5 3h4.5v10H1.5z', 'M10 3h4.5v10H10z', 'M6 8h4', 'M3 6h1.5', 'M11.5 6h1.5'],
    // Suggest a folder: the folder with the AI star inside. Not `folder`,
    // which is Move to…'s in the same menu.
    'folder-suggest': ['M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z',
                       'M8 6.5c.2 1.3.7 1.8 2 2-1.3.2-1.8.7-2 2-.2-1.3-.7-1.8-2-2 1.3-.2 1.8-.7 2-2z'],
    // Keyboard shortcuts: a keyboard, in place of the ⓘ text glyph the menu
    // used, which drew at the font's size, not the icons'.
    'keyboard':      ['M2.5 4h11a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1z',
                       'M4.5 6.75h.01', 'M7 6.75h.01', 'M9.5 6.75h.01', 'M12 6.75h.01', 'M5 9.25h6'],
    // Sidebar (spec 2026-11-10): add, in place of the `+` / `＋` text glyphs;
    // a backend's read-only folders; a refused folder, in place of `!`; and
    // an SSH server, which used to borrow the folder.
    'plus':          ['M8 3v10', 'M3 8h10'],
    'lock':          ['M4.5 7.5h7a1 1 0 0 1 1 1V13a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1V8.5a1 1 0 0 1 1-1z', 'M5.5 7.5V5.5a2.5 2.5 0 0 1 5 0v2'],
    'alert':         ['M8 14.5a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13z', 'M8 4.75V8.5', 'M8 11h.01'],
    'server':        ['M2.5 2.5h11v4.5h-11z', 'M2.5 9h11v4.5h-11z', 'M5 4.75h.01', 'M5 11.25h.01'],
    // ⋯ › Show line numbers: a short number tick beside each line of text.
    // Not `checkbox` or a list bullet: the ticks sit in their own column.
    'line-numbers':  ['M2 4h1.5', 'M2 8h1.5', 'M2 12h1.5', 'M5.5 4h8.5', 'M5.5 8h8.5', 'M5.5 12h6'],
    // The tablet's Folders toggle: three equal bars, the stroke of every other
    // icon (it was a "☰" glyph in the user's font, a different weight).
    'menu':          ['M2.5 4h11', 'M2.5 8h11', 'M2.5 12h11'],
  } as const;

  // A few icons carry meaning that colour communicates faster than shape.
  // Everything else stays currentColor: colouring every icon turns colour
  // into noise and strips its ability to mean anything. Set here rather than
  // at the 13 call sites so a 14th cannot drift, and so the value is stated
  // once. Each token is measured >= 3:1 (WCAG 1.4.11, non-text) on the worst
  // surface+overlay it can land on, in BOTH themes.
  const ACCENT: Partial<Record<keyof typeof PATHS, string>> = {
    trash: 'var(--icon-danger)',
    pin:   'var(--icon-pin)',
    tag:   'var(--icon-tag)',
  };

  export type IconName = keyof typeof PATHS;
  export const ICON_NAMES = Object.keys(PATHS) as IconName[];
  export { PATHS, ACCENT };
</script>

<script lang="ts">
  // No import needed: `<script module>` declarations are in scope here.
  // `inherit` opts out of the semantic accent — for a context that already
  // supplies the colour (a destructive button that is red on hover, say),
  // where a second red would fight it.
  let { name, size = 16, inherit = false }:
    { name: IconName; size?: number; inherit?: boolean } = $props();
  const stroke = $derived(inherit ? 'currentColor' : (ACCENT[name] ?? 'currentColor'));
</script>

<!-- aria-hidden by design: the accessible name belongs on the interactive
     parent (button/menuitem), not on decoration inside it. A labelled icon
     inside a labelled button is read twice. -->
<svg
  width={size}
  height={size}
  viewBox="0 0 16 16"
  fill="none"
  stroke={stroke}
  stroke-width="1.5"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
>
  {#each PATHS[name] as d}
    <path {d} stroke={stroke} />
  {/each}
</svg>
