// @vitest-environment jsdom
//
// Component test — mounts Icon.svelte and inspects the rendered <svg>.
// Needs a DOM; vitest defaults to node, so this file opts into jsdom
// explicitly, matching the convention in reExtract.test.ts /
// newNoteFn.test.ts / whatsNew.test.ts.
import { describe, it, expect } from 'vitest';
import { mount, unmount } from 'svelte';
import type { ComponentProps } from 'svelte';
import Icon, { ICON_NAMES } from './Icon.svelte';

function render(props: ComponentProps<typeof Icon>) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const comp = mount(Icon, { target, props });
  return { target, cleanup: () => { unmount(comp); target.remove(); } };
}

describe('Icon', () => {
  it('exports every name the app needs', () => {
    // Derived from the Tier 2 icon inventory; a missing name is a broken glyph.
    const required = [
      'folder', 'bulb', 'eye', 'search', 'clock', 'trash', 'gear', 'close',
      'pencil', 'note-plus', 'chevron-right', 'chevron-down', 'pin', 'graph',
      'person', 'check', 'link', 'quote', 'checkbox', 'paperclip', 'restore', 'tag',
      'copy', 'refresh', 'eye-off',
    ];
    for (const n of required) expect(ICON_NAMES).toContain(n);
  });

  it('renders a 16px svg by default', () => {
    const { target, cleanup } = render({ name: 'folder' });
    const svg = target.querySelector('svg')!;
    expect(svg.getAttribute('width')).toBe('16');
    expect(svg.getAttribute('height')).toBe('16');
    expect(svg.getAttribute('viewBox')).toBe('0 0 16 16');
    cleanup();
  });

  it('is hidden from assistive tech — the label lives on the parent', () => {
    const { target, cleanup } = render({ name: 'trash' });
    const svg = target.querySelector('svg')!;
    expect(svg.getAttribute('aria-hidden')).toBe('true');
    expect(svg.getAttribute('focusable')).toBe('false');
    cleanup();
  });

  it('inherits colour rather than baking one in', () => {
    const { target, cleanup } = render({ name: 'gear' });
    const svg = target.querySelector('svg')!;
    expect(svg.getAttribute('fill')).toBe('none');
    // Every drawn element must stroke with currentColor, or dark mode breaks.
    const drawn = svg.querySelectorAll('path, circle, rect, line, polyline');
    expect(drawn.length).toBeGreaterThan(0);
    for (const el of drawn) {
      const stroke = el.getAttribute('stroke');
      const fill = el.getAttribute('fill');
      expect(stroke === 'currentColor' || fill === 'currentColor').toBe(true);
      expect(stroke).not.toMatch(/#|rgb/);
    }
    cleanup();
  });

  it('honours an explicit size', () => {
    const { target, cleanup } = render({ name: 'link', size: 12 });
    const svg = target.querySelector('svg')!;
    expect(svg.getAttribute('width')).toBe('12');
    expect(svg.getAttribute('viewBox')).toBe('0 0 16 16'); // grid is fixed
    cleanup();
  });

  it('renders every declared name without throwing', () => {
    for (const name of ICON_NAMES) {
      const { target, cleanup } = render({ name });
      expect(target.querySelector('svg'), `icon "${name}" rendered nothing`).toBeTruthy();
      cleanup();
    }
  });
});
