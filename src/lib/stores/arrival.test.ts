// @vitest-environment jsdom
import { describe, it, expect, vi, afterEach } from 'vitest';
import { get } from 'svelte/store';
import { mount, unmount, flushSync } from 'svelte';
import { readFileSync } from 'node:fs';
import { arrival, arrivalLines, announceArrival, dismissArrival, ARRIVAL_MS } from './arrival';
import { parseTokens, parseColor, contrast, type RGB } from '../color';
import Arrival from '../components/Arrival.svelte';

describe('the arrival line, in words', () => {
  it('says how many notes arrived, where, and that they stayed there', () => {
    expect(arrivalLines([{ kind: 'icloud', count: 773 }])).toEqual([
      '773 notes found in iCloud.',
      'They stay where they are. Jodd keeps a copy on this device.',
    ]);
  });

  it('counts with separators and says "note" for one', () => {
    expect(arrivalLines([{ kind: 'gmail', count: 6012 }])[0]).toBe('6,012 notes found in Gmail.');
    expect(arrivalLines([{ kind: 'microsoft', count: 1 }])[0]).toBe('1 note found in Outlook.');
  });

  it('an empty account says so, and what happens next, with no reassurance about notes it has not got', () => {
    expect(arrivalLines([{ kind: 'local_fs', count: 0 }])).toEqual([
      'No notes in your Local Folder yet.',
      'Notes you write in Jodd will sync there.',
    ]);
  });

  it('one line per account when several arrive at once', () => {
    expect(arrivalLines([{ kind: 'icloud', count: 2 }, { kind: 'ssh', count: 0 }])).toEqual([
      '2 notes found in iCloud.',
      'No notes in your SSH server yet.',
      'They stay where they are. Jodd keeps a copy on this device.',
    ]);
  });

  it('nothing indexed, nothing said', () => {
    expect(arrivalLines([])).toEqual([]);
  });
});

describe('the arrival line, on screen', () => {
  let host: HTMLElement | null = null;
  let app: ReturnType<typeof mount> | null = null;
  afterEach(async () => {
    vi.useRealTimers();
    dismissArrival();
    if (app) await unmount(app);
    app = null;
    host?.remove();
  });

  function show() {
    host = document.createElement('div');
    document.body.append(host);
    app = mount(Arrival, { target: host });
    flushSync();
    return host;
  }

  it('stays long enough to read, then goes', () => {
    vi.useFakeTimers();
    announceArrival([{ kind: 'icloud', count: 3 }]);
    vi.advanceTimersByTime(ARRIVAL_MS - 1);
    expect(get(arrival)).not.toBeNull();
    vi.advanceTimersByTime(1);
    expect(get(arrival)).toBeNull();
  });

  it('the live region holds the sentence, not the Dismiss button', () => {
    const el = show();
    const region = el.querySelector('[role="status"]')!;
    expect(region, 'the live region must exist before anything arrives').not.toBeNull();
    announceArrival([{ kind: 'icloud', count: 3 }]);
    flushSync();
    expect(region.textContent).toMatch(/3 notes found in iCloud\./);
    expect(region.querySelector('button')).toBeNull();
    expect(el.querySelector('.arrival.open')).not.toBeNull();
  });

  it('Dismiss closes it at once', () => {
    const el = show();
    announceArrival([{ kind: 'gmail', count: 9 }]);
    flushSync();
    el.querySelector<HTMLButtonElement>('button[aria-label="Dismiss"]')!.click();
    flushSync();
    expect(get(arrival)).toBeNull();
    expect(el.querySelector('.arrival.open')).toBeNull();
    expect(el.querySelector('button')).toBeNull();
  });

  // It arrives on Amber Tint, so its words must read there too, in both
  // themes, before it settles to the panel.
  const CSS = readFileSync('src/styles/tokens.css', 'utf8');
  const LIGHT = parseTokens(CSS, ':root');
  const DARK = { ...LIGHT, ...parseTokens(CSS, '@media (prefers-color-scheme: dark)') };
  const rgb = (t: Record<string, string>, n: string): RGB => parseColor(t[n])!.rgb;
  for (const [theme, t] of [['light', LIGHT], ['dark', DARK]] as const) {
    it(`its text reads on the lamp-lit first frame in the ${theme} theme`, () => {
      for (const ink of ['--text', '--text-secondary']) {
        expect(contrast(rgb(t, ink), rgb(t, '--accent-tint')), ink).toBeGreaterThanOrEqual(4.5);
      }
    });
  }

  it('the rise is movement (gone under reduced motion); the warmth is colour (kept)', () => {
    const css = readFileSync('src/lib/components/Arrival.svelte', 'utf8');
    expect(css).toMatch(/arrival-rise var\(--dur-move\)/);
    expect(css).toMatch(/arrival-settle calc\(var\(--dur-focal\) \* 3\)/);
    const settle = css.slice(css.indexOf('@keyframes arrival-settle'));
    expect(settle).not.toMatch(/transform/);
  });
});
