import { describe, it, expect } from 'vitest';
import { overflowCount, overflowWithMore } from './toolbarOverflow';

const thirty = (n: number) => Array.from({ length: n }, () => 30);

describe('overflowCount', () => {
  it('moves the buttons that do not fit, from the right', () => {
    // 300 - 120 reserved = 180 for buttons: six 30px buttons fit, five do not.
    expect(overflowCount(300, thirty(11), 120)).toBe(5);
  });

  it('is 0 when everything fits, exactly or with room to spare', () => {
    expect(overflowCount(1000, thirty(11), 120)).toBe(0);
    expect(overflowCount(450, thirty(11), 120)).toBe(0); // 330 === 330
    expect(overflowCount(449, thirty(11), 120)).toBe(1);
  });

  it('counts from the right even when widths differ', () => {
    // room 100: 40 + 40 fit, the 40 after them does not; the 10 behind it is
    // not squeezed in — overflow is a suffix, never a gap.
    expect(overflowCount(220, [40, 40, 40, 10], 120)).toBe(2);
  });

  it('overflows everything when the reserved side alone fills the row', () => {
    expect(overflowCount(120, thirty(11), 120)).toBe(11);
    expect(overflowCount(50, thirty(11), 120)).toBe(11);
  });

  it('handles no buttons and a zero-width row', () => {
    expect(overflowCount(300, [], 120)).toBe(0);
    expect(overflowCount(0, thirty(3), 0)).toBe(3);
  });

  it('never reports more than the number of buttons, or a negative count', () => {
    expect(overflowCount(-10, thirty(4), 5)).toBe(4);
    expect(overflowCount(10_000, thirty(4), 0)).toBe(0);
  });

  it('treats a non-finite measurement as "do not hide anything"', () => {
    // A hidden or detached toolbar measures NaN/0; hiding every button on
    // that reading would flash an empty toolbar.
    expect(overflowCount(Number.NaN, thirty(4), 10)).toBe(0);
    expect(overflowCount(300, thirty(4), Number.NaN)).toBe(0);
  });
});

describe('overflowWithMore', () => {
  it('needs no More button when nothing overflows', () => {
    expect(overflowWithMore(450, thirty(11), 120, 30)).toEqual({ count: 0, more: false });
  });

  it('reserves room for the More button once something overflows', () => {
    // Without More: 180 room, six fit (5 overflow). With 30px More: 150, five fit.
    expect(overflowWithMore(300, thirty(11), 120, 30)).toEqual({ count: 6, more: true });
  });

  it('can overflow everything, leaving only More', () => {
    expect(overflowWithMore(130, thirty(11), 120, 30)).toEqual({ count: 11, more: true });
  });
});
