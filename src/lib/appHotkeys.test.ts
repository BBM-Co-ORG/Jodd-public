import { describe, expect, it, vi } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { appHotkey, hotkeyLabel, runAppHotkey, type HotkeyEvent } from './appHotkeys';

// A US-layout keydown: a letter's code is its physical key ("KeyJ").
const press = (key: string, mods: Partial<HotkeyEvent> = {}): HotkeyEvent => ({
  key,
  code: /^[a-z]$/i.test(key) ? `Key${key.toUpperCase()}` : key,
  metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, ...mods,
});

describe('appHotkey', () => {
  it('Mod+Shift+J asks Jodd, with Cmd or Ctrl, whatever case the key reports', () => {
    expect(appHotkey(press('J', { metaKey: true, shiftKey: true }))).toBe('ask');
    expect(appHotkey(press('j', { ctrlKey: true, shiftKey: true }))).toBe('ask');
  });

  it('keeps Make a note with AI on Mod+Shift+L and New note on Mod+N', () => {
    expect(appHotkey(press('L', { metaKey: true, shiftKey: true }))).toBe('makeNote');
    expect(appHotkey(press('l', { ctrlKey: true, shiftKey: true }))).toBe('makeNote');
    expect(appHotkey(press('n', { metaKey: true }))).toBe('newNote');
  });

  it('J needs both the modifier and Shift, and no Alt', () => {
    expect(appHotkey(press('j'))).toBeNull();
    expect(appHotkey(press('J', { shiftKey: true }))).toBeNull();
    expect(appHotkey(press('j', { metaKey: true }))).toBeNull();
    expect(appHotkey(press('J', { metaKey: true, shiftKey: true, altKey: true }))).toBeNull();
  });

  it('leaves the editor’s and the list’s chords alone', () => {
    for (const key of ['a', 'A', 'b', 'k', 'f', 'z', 'Z', 'x', 'X', 'c', 'C', 'Enter']) {
      expect(appHotkey(press(key, { metaKey: true })), key).not.toBe('ask');
      expect(appHotkey(press(key, { metaKey: true, shiftKey: true })), key).not.toBe('ask');
    }
  });
});

describe('appHotkey on a non-Latin layout', () => {
  // Thai Kedmanee: the J key types a Thai mark, so e.key is not "j"; the
  // physical key (e.code) is what identifies the chord.
  it('matches the physical key, whatever character the layout types', () => {
    expect(appHotkey(press('่', { code: 'KeyJ', ctrlKey: true, shiftKey: true }))).toBe('ask');
    expect(appHotkey(press('ส', { code: 'KeyL', metaKey: true, shiftKey: true }))).toBe('makeNote');
    expect(appHotkey(press('ื', { code: 'KeyN', ctrlKey: true }))).toBe('newNote');
  });

  it('falls back to e.key only when the platform reports no code', () => {
    expect(appHotkey(press('J', { code: '', ctrlKey: true, shiftKey: true }))).toBe('ask');
    expect(appHotkey(press('่', { code: '', ctrlKey: true, shiftKey: true }))).toBeNull();
  });

  it('a code for another key wins over a "j" in e.key', () => {
    expect(appHotkey(press('j', { code: 'KeyH', ctrlKey: true, shiftKey: true }))).toBeNull();
  });
});

describe('runAppHotkey (App.svelte\'s window keydown)', () => {
  function setup(signedIn = true, editableFocused = false) {
    const to = { signedIn, editableFocused: () => editableFocused, makeNote: vi.fn(), ask: vi.fn(), newNote: vi.fn() };
    const fire = (key: string, mods: Partial<HotkeyEvent>) => {
      const e = { ...press(key, mods), preventDefault: vi.fn() };
      return { took: runAppHotkey(e, to), prevented: e.preventDefault.mock.calls.length > 0 };
    };
    return { to, fire };
  }

  it('Ctrl+Shift+J opens Ask Jodd and takes the key', () => {
    const { to, fire } = setup();
    expect(fire('J', { ctrlKey: true, shiftKey: true })).toEqual({ took: true, prevented: true });
    expect(to.ask).toHaveBeenCalledOnce();
    expect(to.makeNote).not.toHaveBeenCalled();
  });

  it('routes L and N to their own actions', () => {
    const { to, fire } = setup();
    fire('L', { metaKey: true, shiftKey: true });
    fire('n', { metaKey: true });
    expect(to.makeNote).toHaveBeenCalledOnce();
    expect(to.newNote).toHaveBeenCalledOnce();
  });

  it('does nothing on the sign-in screen, and lets the key through', () => {
    const { to, fire } = setup(false);
    for (const [key, mods] of [['J', { shiftKey: true }], ['L', { shiftKey: true }], ['n', {}]] as const) {
      expect(fire(key, { ctrlKey: true, ...mods })).toEqual({ took: false, prevented: false });
    }
    expect(to.ask).not.toHaveBeenCalled();
    expect(to.makeNote).not.toHaveBeenCalled();
    expect(to.newNote).not.toHaveBeenCalled();
  });

  it('New note yields to a focused editable; Ask and Make a note do not', () => {
    const { to, fire } = setup(true, true);
    expect(fire('n', { ctrlKey: true }).took).toBe(false);
    expect(fire('J', { ctrlKey: true, shiftKey: true }).took).toBe(true);
    expect(to.newNote).not.toHaveBeenCalled();
    expect(to.ask).toHaveBeenCalledOnce();
  });

  it('App.svelte wires its window keydown through it, gated on sign-in', () => {
    const src = readFileSync('src/App.svelte', 'utf8');
    expect(src).toMatch(/<svelte:window onkeydown=\{onGlobalKey\}/);
    expect(src).toMatch(/runAppHotkey\(e, \{\s*signedIn: \$isAuthenticated,/);
    expect(src).toContain('ask: () => askModalOpen.set(true)');
  });
});

describe('hotkeyLabel', () => {
  it('names the platform’s own modifier', () => {
    expect(hotkeyLabel('ask', true)).toBe('Cmd+Shift+J');
    expect(hotkeyLabel('ask', false)).toBe('Ctrl+Shift+J');
    expect(hotkeyLabel('makeNote', false)).toBe('Ctrl+Shift+L');
  });
});

// The claim in appHotkeys.ts's header, kept true: nothing else in the app
// binds J. A component that starts matching it (by e.key or e.code) must
// pick another key, or move Ask's.
describe('no other binding on J', () => {
  function sources(dir: string): string[] {
    return readdirSync(dir, { withFileTypes: true }).flatMap((d) => {
      const p = join(dir, d.name);
      if (d.isDirectory()) return d.name === 'generated' ? [] : sources(p);
      return /\.(svelte|ts)$/.test(d.name) && !/\.test\.ts$/.test(d.name) ? [p] : [];
    });
  }
  const J_BINDING = /['"]KeyJ['"]|key(?:\.toLowerCase\(\))?\s*===\s*['"][jJ]['"]/;

  it('appears only in appHotkeys.ts', () => {
    const files = sources('src');
    expect(files.length).toBeGreaterThan(50);
    const binders = files.filter((f) => J_BINDING.test(readFileSync(f, 'utf8')));
    expect(binders.map((f) => f.replace(/\\/g, '/'))).toEqual(['src/lib/appHotkeys.ts']);
  });
});
