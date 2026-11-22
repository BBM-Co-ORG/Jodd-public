// The bookkeeping and painting behind the editor's Find / Replace bar, kept
// apart from NoteEditor.svelte's reactive state so it can be tested. The
// matching itself lives in ./findMatches.ts.

import { afterNextPaint } from './visibleRanges';

/** A run of text as [start, end) offsets — title offsets, or note-wide body
 *  offsets from `findBodyMatches`. */
export type Span = [number, number];

/** Whether [start, end) touches any span. */
export const overlapsAny = (spans: Span[], start: number, end: number): boolean =>
  spans.some(([a, b]) => start < b && end > a);

/**
 * What Replace put in is not a match. Find ignores case by default, so a
 * replacement can still match the query: "line" → "LINE" re-replaced LINE1 on
 * every press with the count stuck at 5 / 5, "note" → "notes" grew "notess"
 * (dev build, 2026-10-06). So the inserted text is remembered as offsets —
 * never as Ranges: a Gmail save re-renders a fresh editor element and every
 * Range would die with the old one.
 *
 * Records that [start, end) became `length` characters of replacement, and
 * moves every remembered span after it by the change in length.
 */
export function recordReplaced(spans: Span[], start: number, end: number, length: number): Span[] {
  const delta = length - (end - start);
  const moved = spans.map(([a, b]): Span => (a >= end ? [a + delta, b + delta] : [a, b]));
  if (length > 0) moved.push([start, start + length]);
  return moved;
}

/**
 * Replace every title match, left to right. `matches` are in order and do not
 * overlap; `texts[k]` replaces `matches[k]`. Returns the new title and the
 * remembered spans, each placed where its replacement now sits.
 */
export function replaceAllInTitle(
  title: string,
  matches: Array<{ start: number; end: number }>,
  texts: string[],
  spans: Span[],
): { title: string; spans: Span[] } {
  let out = '';
  let i = 0;
  matches.forEach((m, k) => {
    const at = out.length + (m.start - i);
    out += title.slice(i, m.start) + texts[k];
    spans = recordReplaced(spans, at, at + (m.end - m.start), texts[k].length);
    i = m.end;
  });
  return { title: out + title.slice(i), spans };
}

/**
 * Remember body replacements made in one pass. `matches` are in document
 * order with offsets from BEFORE the pass, so each is shifted by the change
 * in length of the replacements before it.
 */
export function recordBodyReplacements(
  spans: Span[],
  matches: Array<{ absStart: number; absEnd: number }>,
  texts: string[],
): Span[] {
  let shift = 0;
  matches.forEach((m, k) => {
    spans = recordReplaced(spans, m.absStart + shift, m.absEnd + shift, texts[k].length);
    shift += texts[k].length - (m.absEnd - m.absStart);
  });
  return spans;
}

// While the bar is open the current match is PAINTED, never selected. Inside
// a contenteditable the selection is the caret: putting it on a match moved
// focus into the note, so the next keystroke meant for the query overwrote
// the match ("hello" + "lo" → "lolo", 2026-10-06). The CSS Custom Highlight
// API marks ranges without touching selection or focus.
export const FIND_HL = 'jodd-find';
export const FIND_HL_CURRENT = 'jodd-find-current';
// Transparent. WebKit does not repaint a range that leaves EVERY highlight,
// so a match that stopped matching kept its old colour until something else
// repainted — linE1's "lin" stayed dark after typing "line" with Aa on (dev
// build, and in Safari every time, 2026-10-06). Moving a range between
// highlights does repaint it, so what was painted and no longer matches
// spends one paint here. When it later leaves this one too, its stale colour
// is transparent. CLAUDE.md gotcha #34.
export const FIND_HL_GONE = 'jodd-find-gone';

// Explicit priorities, above the link highlights (links/paint.ts,
// LINK_HL_PRIORITY = 0): a match inside a link must still show as a match.
export const FIND_HL_PRIORITY = 1;
export const FIND_HL_CURRENT_PRIORITY = 2;

export const canHighlight = (): boolean =>
  typeof CSS !== 'undefined' && 'highlights' in CSS && typeof Highlight !== 'undefined';

function highlight(priority: number, ...ranges: Range[]): Highlight {
  const h = new Highlight(...ranges);
  h.priority = priority;
  return h;
}

/** Paints the find matches. One per editor; `dispose` when it unmounts. */
export class FindPainter {
  private painted: Range[] = [];
  private generation = 0;

  /** `afterPaint` runs a callback once the browser has painted (./visibleRanges.ts
   *  `afterNextPaint`); injected so tests can crank it by hand. */
  constructor(private readonly afterPaint: (cb: () => void) => void = afterNextPaint) {}

  /** Paint `body` as matches and `current` (when the current match is in the
   *  body) as the current one. Whatever was painted last time passes through
   *  the transparent `jodd-find-gone` highlight first — for ONE paint: then
   *  gone is emptied, because WebKit re-validates every range it holds on
   *  every rendering update (./visibleRanges.ts). Leaving gone is safe: what
   *  was in it last painted transparent. */
  paint(body: Range[], current: Range | undefined): void {
    if (!canHighlight()) return;
    CSS.highlights.set(FIND_HL_GONE, highlight(FIND_HL_PRIORITY, ...this.painted));
    CSS.highlights.set(FIND_HL, highlight(FIND_HL_PRIORITY, ...body));
    CSS.highlights.set(
      FIND_HL_CURRENT,
      current ? highlight(FIND_HL_CURRENT_PRIORITY, current) : highlight(FIND_HL_CURRENT_PRIORITY),
    );
    // The current match is painted wherever it is, on screen or not, so it
    // must leave through gone too.
    this.painted = current && !body.includes(current) ? [...body, current] : body;
    const mine = ++this.generation;
    this.afterPaint(() => {
      if (mine === this.generation && canHighlight()) CSS.highlights.set(FIND_HL_GONE, highlight(FIND_HL_PRIORITY));
    });
  }

  clear(): void {
    this.paint([], undefined);
  }

  /** Removes the three highlights and cancels an emptying still waiting for
   *  its frame — which would otherwise put `jodd-find-gone` back. */
  dispose(): void {
    this.generation++;
    if (canHighlight()) for (const name of [FIND_HL, FIND_HL_CURRENT, FIND_HL_GONE]) CSS.highlights.delete(name);
  }
}

/** How long Find waits after the last keystroke before it searches and
 *  paints. Long enough to skip the one-letter queries of a word being typed —
 *  the most matches, the slowest paint — and short enough to feel immediate. */
export const FIND_INPUT_DELAY_MS = 200;

/** `call` (re)starts the wait; `run` happens once the calls stop for `ms`. */
export function createDebounce(run: () => void, ms: number): { call(): void; cancel(): void } {
  let timer: ReturnType<typeof setTimeout> | undefined;
  return {
    call() {
      clearTimeout(timer);
      timer = setTimeout(() => {
        timer = undefined;
        run();
      }, ms);
    },
    cancel() {
      clearTimeout(timer);
      timer = undefined;
    },
  };
}
