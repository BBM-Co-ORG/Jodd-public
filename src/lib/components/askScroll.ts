// Live 2026-10-14 (macOS): after a long first answer, a follow-up question and
// its answer were appended below the fold of `.ask-turns`, which stayed on the
// old answer — it looked as if nothing had happened. AskJoddModal now brings the
// newest question to the TOP of the conversation: scrolling to the bottom
// instead would hide the start of a long answer.

type ContainerBox = { scrollTop: number; clientTop: number; top: number; paddingTop: number };

/**
 * The `scrollTop` that puts a turn whose border box starts at viewport `turnTop`
 * just inside the container's top padding. Measured from bounding rects, not
 * `offsetTop`: `.ask-turns` is not positioned, so `offsetTop` would be
 * relative to some ancestor rather than to the container.
 */
export function scrollTopFor(container: ContainerBox, turnTop: number): number {
  const offset = turnTop - container.top - container.clientTop + container.scrollTop;
  return Math.max(0, offset - container.paddingTop);
}

function prefersReducedMotion(): boolean {
  // matchMedia is missing under jsdom; no answer means no preference.
  return globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
}

/** Scroll `container` so its newest `.ask-turn.user` sits at the top. */
export function revealLatestQuestion(container: HTMLElement, reduceMotion = prefersReducedMotion()): void {
  const questions = container.querySelectorAll<HTMLElement>('.ask-turn.user');
  const question = questions[questions.length - 1];
  if (!question) return;
  const top = scrollTopFor(
    {
      scrollTop: container.scrollTop,
      clientTop: container.clientTop,
      top: container.getBoundingClientRect().top,
      paddingTop: parseFloat(getComputedStyle(container).paddingTop) || 0,
    },
    question.getBoundingClientRect().top,
  );
  if (typeof container.scrollTo === 'function') {
    container.scrollTo({ top, behavior: reduceMotion ? 'auto' : 'smooth' });
  } else {
    container.scrollTop = top;
  }
}
