<script lang="ts" generics="T extends { uuid: string; title: string; label: string }">
  // The `[[` note-link autocomplete: markup and styles only. NoteEditor.svelte
  // keeps what to list (search_note_links), which row is active (the caret
  // lists' keys), where it sits (linkPickerStyle) and what a pick inserts.
  // Split out of NoteEditor.svelte 2026-11-16; HashtagPicker.svelte is its
  // sibling for `#`.
  //
  // A pick runs on mousedown, with the default cancelled, so the body keeps
  // focus and the caret the insert is measured against.

  // Generic over the row, so a pick hands back the editor's own candidate
  // (it carries the slug the insert needs) and not a narrowed copy.
  interface Props {
    results: T[];
    activeIndex: number;
    /** Inline position from NoteEditor's linkPickerStyle. Its max-height and
     *  widest width there must match .link-picker below. */
    style: string;
    /** Android: 48px rows. */
    touch: boolean;
    /** The listbox id; row i is `${id}-${i}`. The body points
     *  aria-activedescendant at the active row, since focus stays there. */
    id: string;
    onpick: (c: T) => void;
  }

  let { results, activeIndex, style, touch, id, onpick }: Props = $props();
</script>

<div class="link-picker" class:touch {style} role="listbox" aria-label="Notes to link" {id}>
  {#each results as c, i (c.uuid)}
    <button
      class="link-opt"
      class:active={i === activeIndex}
      role="option"
      id={`${id}-${i}`}
      aria-selected={i === activeIndex}
      tabindex="-1"
      type="button"
      onmousedown={(e) => { e.preventDefault(); onpick(c); }}
    >
      <span class="link-opt-title" title={c.title || 'Untitled'}>{c.title || 'Untitled'}</span>
      <span class="link-opt-label">{c.label}</span>
    </button>
  {/each}
</div>

<style>
  .link-picker {
    position: fixed;
    z-index: var(--z-popover);
    min-width: 220px;
    max-width: 340px;
    max-height: 240px;
    overflow-y: auto;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-menu);
    padding: 4px;
  }
  .link-opt {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    border: none;
    background: none;
    text-align: left;
    padding: 6px 9px;
    border-radius: var(--radius);
    cursor: pointer;
    font-family: inherit;
  }
  .link-opt:hover,
  .link-opt.active {
    background: var(--accent-tint);
  }
  .link-opt-title {
    font-size: var(--size);
    color: var(--text);
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 1;
  }
  .link-opt-label {
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
    white-space: nowrap;
  }

  /* Android: a finger-sized row (--touch-target, 48px); rows were about 31px.
     The desktop sizes are untouched. */
  .link-picker.touch .link-opt {
    min-height: var(--touch-target);
    align-items: center;
  }
</style>
