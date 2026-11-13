// @vitest-environment jsdom
import { describe, it, expect, vi, afterEach } from 'vitest';
import { scrollTopFor, revealLatestQuestion } from './askScroll';

// jsdom does no layout, so every box here is stubbed.
function box(el: Element, top: number) {
  el.getBoundingClientRect = () => ({ top } as DOMRect);
}

function conversation(userTops: number[]) {
  const container = document.createElement('div');
  container.className = 'ask-turns';
  box(container, 100);
  for (const top of userTops) {
    const q = document.createElement('div');
    q.className = 'ask-turn user';
    box(q, top);
    const a = document.createElement('div');
    a.className = 'ask-turn assistant';
    container.append(q, a);
  }
  return container;
}

describe('scrollTopFor', () => {
  it('puts the turn just inside the container padding', () => {
    // Turn is 450px below the container's top edge while scrolled 200px down:
    // its content offset is 649 (minus the 1px border), minus 12px padding.
    expect(scrollTopFor({ scrollTop: 200, clientTop: 1, top: 100, paddingTop: 12 }, 550)).toBe(637);
  });

  it('never asks for a negative scroll', () => {
    expect(scrollTopFor({ scrollTop: 0, clientTop: 1, top: 100, paddingTop: 12 }, 105)).toBe(0);
  });
});

describe('revealLatestQuestion', () => {
  afterEach(() => { vi.unstubAllGlobals(); });

  it('scrolls to the newest user question, not the last turn', () => {
    const container = conversation([110, 900]);
    const scrollTo = vi.fn();
    container.scrollTo = scrollTo as unknown as typeof container.scrollTo;
    revealLatestQuestion(container, false);
    expect(scrollTo).toHaveBeenCalledWith({ top: 800, behavior: 'smooth' });
  });

  it('jumps instead of animating under reduced motion', () => {
    const container = conversation([900]);
    const scrollTo = vi.fn();
    container.scrollTo = scrollTo as unknown as typeof container.scrollTo;
    revealLatestQuestion(container, true);
    expect(scrollTo).toHaveBeenCalledWith({ top: 800, behavior: 'auto' });
  });

  it('reads reduced motion from the OS setting by default', () => {
    vi.stubGlobal('matchMedia', (q: string) => ({ matches: q === '(prefers-reduced-motion: reduce)' }));
    const container = conversation([900]);
    const scrollTo = vi.fn();
    container.scrollTo = scrollTo as unknown as typeof container.scrollTo;
    revealLatestQuestion(container);
    expect(scrollTo).toHaveBeenCalledWith({ top: 800, behavior: 'auto' });
  });

  it('falls back to scrollTop where scrollTo is missing', () => {
    const container = conversation([900]);
    let set = -1;
    Object.defineProperty(container, 'scrollTop', { get: () => 0, set: (v: number) => { set = v; } });
    revealLatestQuestion(container, false);
    expect(set).toBe(800);
  });

  it('does nothing when there is no question yet', () => {
    const container = conversation([]);
    const scrollTo = vi.fn();
    container.scrollTo = scrollTo as unknown as typeof container.scrollTo;
    revealLatestQuestion(container, false);
    expect(scrollTo).not.toHaveBeenCalled();
  });
});
