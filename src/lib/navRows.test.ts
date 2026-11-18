import { describe, it, expect } from 'vitest';
import { navKeyMove, type NavRowInfo } from './navRows';

// Notes (expanded) > Work (collapsed, has kids) ; Notes > Home ; Views row
const rows: NavRowInfo[] = [
  { depth: 0, hasKids: true, expanded: true },   // 0 Notes
  { depth: 1, hasKids: true, expanded: false },  // 1 Notes/Work
  { depth: 1, hasKids: false, expanded: false }, // 2 Notes/Home
  { depth: 0, hasKids: false, expanded: false }, // 3 a Views row
];

describe('navKeyMove', () => {
  it('moves up and down, stopping at the ends', () => {
    expect(navKeyMove('ArrowDown', false, rows, 0)).toEqual({ kind: 'focus', index: 1 });
    expect(navKeyMove('ArrowUp', false, rows, 2)).toEqual({ kind: 'focus', index: 1 });
    expect(navKeyMove('ArrowDown', false, rows, 3)).toBeNull();
    expect(navKeyMove('ArrowUp', false, rows, 0)).toBeNull();
  });

  it('jumps to the first and last row', () => {
    expect(navKeyMove('Home', false, rows, 2)).toEqual({ kind: 'focus', index: 0 });
    expect(navKeyMove('End', false, rows, 0)).toEqual({ kind: 'focus', index: 3 });
  });

  it('Right expands a closed folder, then steps into its first child', () => {
    expect(navKeyMove('ArrowRight', false, rows, 1)).toEqual({ kind: 'expand' });
    expect(navKeyMove('ArrowRight', false, rows, 0)).toEqual({ kind: 'focus', index: 1 });
    expect(navKeyMove('ArrowRight', false, rows, 2)).toBeNull();
  });

  it('Left collapses an open folder, otherwise goes to the parent', () => {
    expect(navKeyMove('ArrowLeft', false, rows, 0)).toEqual({ kind: 'collapse' });
    expect(navKeyMove('ArrowLeft', false, rows, 2)).toEqual({ kind: 'focus', index: 0 });
    expect(navKeyMove('ArrowLeft', false, rows, 3)).toBeNull();
  });

  it('Shift+F10 and the ContextMenu key open the row menu', () => {
    expect(navKeyMove('F10', true, rows, 1)).toEqual({ kind: 'menu' });
    expect(navKeyMove('ContextMenu', false, rows, 1)).toEqual({ kind: 'menu' });
    expect(navKeyMove('F10', false, rows, 1)).toBeNull();
  });

  it('ignores every other key', () => {
    expect(navKeyMove('a', false, rows, 0)).toBeNull();
    expect(navKeyMove('Enter', false, rows, 0)).toBeNull();
  });
});
