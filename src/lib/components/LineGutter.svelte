<script lang="ts">
  // The editor's line-number gutter (⋯ › Show line numbers, off by default).
  //
  // It numbers noteLines(editor) — the same lines the date tooltip counts —
  // so its last number is the tooltip's "N lines". One number per paragraph,
  // list item, heading or table row, on its first screen line; a paragraph
  // that wraps keeps one number.
  //
  // Not inside the editor: the body is contenteditable and is saved as it
  // is, so a number element in it would be pushed to Apple Notes. The gutter
  // is the editor's sibling, laid over its left padding (same offsetParent),
  // and its layer moves with the editor's scroll.
  import { untrack } from 'svelte';
  import { noteLines, type NoteLine } from '../editor/noteLines';

  interface Props {
    /** The editor body; a new element on every re-render ({#key}). */
    editor: HTMLElement | null;
  }
  let { editor }: Props = $props();

  // Content coordinates: from the top of the editor's scrolled content.
  type Mark = { n: number; top: number; height: number };
  let marks = $state<Mark[]>([]);
  let box = $state({ top: 0, left: 0, height: 0 });
  let scrollTop = $state(0);
  // Lines beyond the viewport by this much are drawn too, so a fast scroll
  // does not show an empty gutter before the next frame.
  const OVERSCAN = 400;

  const raf: (f: () => void) => unknown =
    typeof requestAnimationFrame === 'function' ? requestAnimationFrame : (f) => setTimeout(f, 16);

  function firstRect(line: NoteLine): DOMRect | null {
    const first = line.nodes[0];
    const last = line.nodes[line.nodes.length - 1];
    const range = document.createRange();
    if (line.nodes.length === 1 && first.nodeType === Node.ELEMENT_NODE) range.selectNodeContents(first);
    else {
      range.setStartBefore(first);
      range.setEndAfter(last);
    }
    // jsdom has no Range.getClientRects.
    const rects = typeof range.getClientRects === 'function' ? [...range.getClientRects()] : [];
    const r = rects.find((x) => x.height > 0);
    if (r) return r;
    // A blank line (<div><br></div>) or an image: the element's own box.
    return first.nodeType === Node.ELEMENT_NODE ? (first as Element).getBoundingClientRect() : null;
  }

  function measure(el: HTMLElement): void {
    const origin = el.getBoundingClientRect();
    const base = origin.top + el.clientTop - el.scrollTop;
    const next: Mark[] = [];
    noteLines(el).forEach((line, i) => {
      const r = firstRect(line);
      // A line with no box (hidden) keeps its number but draws none.
      if (!r || (r.height === 0 && r.width === 0)) return;
      next.push({ n: i + 1, top: r.top - base, height: r.height });
    });
    marks = next;
    box = { top: el.offsetTop + el.clientTop, left: el.offsetLeft, height: el.clientHeight };
    scrollTop = el.scrollTop;
  }

  $effect(() => {
    const el = editor;
    if (!el) return;
    let queued = false;
    let alive = true;
    const schedule = () => {
      if (queued) return;
      queued = true;
      raf(() => {
        queued = false;
        if (alive) untrack(() => measure(el));
      });
    };
    schedule();
    const onScroll = () => (scrollTop = el.scrollTop);
    el.addEventListener('scroll', onScroll, { passive: true });
    // An image or attachment that loads changes the heights below it.
    el.addEventListener('load', schedule, true);
    // Any edit, a <details> opening, an attachment swapped in.
    const mo = new MutationObserver(schedule);
    mo.observe(el, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ['open', 'style', 'class'] });
    // A narrower pane re-wraps every paragraph.
    const ro = typeof ResizeObserver === 'function' ? new ResizeObserver(schedule) : null;
    ro?.observe(el);
    void document.fonts?.ready.then(schedule);
    return () => {
      alive = false;
      el.removeEventListener('scroll', onScroll);
      el.removeEventListener('load', schedule, true);
      mo.disconnect();
      ro?.disconnect();
    };
  });

  const shown = $derived(
    marks.filter((m) => m.top + m.height >= scrollTop - OVERSCAN && m.top <= scrollTop + box.height + OVERSCAN),
  );
</script>

<div
  class="line-gutter"
  aria-hidden="true"
  data-testid="line-gutter"
  style="top: {box.top}px; left: {box.left}px; height: {box.height}px;"
>
  <div class="line-gutter-layer" style="transform: translateY({-scrollTop}px);">
    {#each shown as m (m.n)}
      <span class="line-no" style="top: {m.top}px; height: {m.height}px;">{m.n}</span>
    {/each}
  </div>
</div>

<style>
  /* Over the editor's left padding, which .editor-body.line-numbers widens
     to make room (NoteEditor.svelte). Decoration only: hidden from assistive
     tech and from the pointer, so a click there still lands in the note. */
  .line-gutter {
    position: absolute;
    width: var(--line-gutter-width);
    overflow: hidden;
    pointer-events: none;
    user-select: none;
  }
  .line-gutter-layer {
    position: relative;
  }
  .line-no {
    position: absolute;
    right: 0;
    display: flex;
    align-items: center;
    font-size: var(--size-xs);
    font-variant-numeric: tabular-nums;
    line-height: var(--leading-tight);
    color: var(--text-muted);
  }
</style>
