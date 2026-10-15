<script lang="ts" module>
  // All geometry on a 16x16 grid. `d` entries are stroked, never filled, so
  // every glyph inherits currentColor and follows the theme.
  const PATHS = {
    'folder':        ['M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z'],
    'bulb':          ['M6 12.5h4', 'M6.5 14.5h3', 'M8 1.5a4 4 0 0 0-2.4 7.2c.4.3.6.8.6 1.3h3.6c0-.5.2-1 .6-1.3A4 4 0 0 0 8 1.5z'],
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
    'chat':          ['M2.5 3.5h11a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H6.5l-2.5 2.5v-2.5h-1.5a1 1 0 0 1-1-1v-6a1 1 0 0 1 1-1z'],
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
