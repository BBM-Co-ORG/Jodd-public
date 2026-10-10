import { describe, it, expect } from 'vitest';
import { isShortcutMod, shortcutModLabel } from './shortcutKeys';

const keys = (metaKey: boolean, ctrlKey: boolean) => ({ metaKey, ctrlKey });

describe('isShortcutMod', () => {
  it('on macOS takes ⌘ alone', () => {
    expect(isShortcutMod(keys(true, false), true)).toBe(true);
  });

  it('on macOS leaves Ctrl to the system text bindings (Ctrl+E, Ctrl+K, …)', () => {
    expect(isShortcutMod(keys(false, true), true)).toBe(false);
  });

  it('on macOS does not treat Ctrl+Cmd as ⌘ — Ctrl+Cmd+Z is not undo', () => {
    expect(isShortcutMod(keys(true, true), true)).toBe(false);
  });

  it('elsewhere takes Ctrl alone, not the Windows/Super key', () => {
    expect(isShortcutMod(keys(false, true), false)).toBe(true);
    expect(isShortcutMod(keys(true, false), false)).toBe(false);
    expect(isShortcutMod(keys(true, true), false)).toBe(false);
  });

  it('is false with no modifier held', () => {
    expect(isShortcutMod(keys(false, false), true)).toBe(false);
    expect(isShortcutMod(keys(false, false), false)).toBe(false);
  });
});

describe('shortcutModLabel', () => {
  it('names the key the platform actually uses', () => {
    expect(shortcutModLabel(true)).toBe('Cmd');
    expect(shortcutModLabel(false)).toBe('Ctrl');
  });
});
