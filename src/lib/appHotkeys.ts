// The app's window-scope hotkeys, matched in one place so the handler
// (App.svelte's onGlobalKey) and every label that names a key (the AI menu's
// hint column, the editor's Keyboard shortcuts sheet) read the same table.
//
// Every other keyboard shortcut is claimed by a component, and none of them
// may reuse these chords. Taken when Ask's was picked (spec 2026-10-28 §15):
//   NoteEditor  Mod+Z · Mod+Shift+Z · Ctrl+Y · B · I · U · E · K · F ·
//               Mod+Shift+X · C · 7 · 8 · 9 · . · Mod+Alt+1/2/3 · Mod+Enter
//   NoteList    Mod+A — with or without Shift (it does not check Shift),
//               which is why Ask is not Mod+Shift+A
//   Settings    Mod+Enter
// appHotkeys.test.ts scans the sources so a new binding on J fails there.
//
// The modifier is Cmd or Ctrl on any platform, as onGlobalKey has always
// accepted it; the label names the platform's own (shortcutModLabel).
//
// The key is matched by e.code (the physical key), as NoteEditor's own
// shortcuts are: with a Thai layout e.key is a Thai character, so a
// key-only match never fires. e.key is the fallback only
// when a platform reports no code (some Android IMEs send code "").

import { shortcutModLabel } from './editor/shortcutKeys';

export type AppHotkey = 'makeNote' | 'ask' | 'newNote';

export interface HotkeyEvent {
  key: string;
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

/** The action a keydown asks for, or null. */
export function appHotkey(e: HotkeyEvent): AppHotkey | null {
  const cmd = e.metaKey || e.ctrlKey;
  if (!cmd) return null;
  const key = letter(e);
  if (e.shiftKey && key === 'l') return 'makeNote';
  // Ask Jodd: J for Jodd. Alt stays free so Mod+Alt+Shift+J is not taken.
  if (e.shiftKey && !e.altKey && key === 'j') return 'ask';
  if (!e.shiftKey && !e.altKey && key === 'n') return 'newNote';
  return null;
}

// "KeyJ" → "j"; with no code, the produced character, lowercased.
function letter(e: HotkeyEvent): string {
  const m = /^Key([A-Z])$/.exec(e.code ?? '');
  if (m) return m[1].toLowerCase();
  return e.code ? '' : e.key.toLowerCase();
}

export interface HotkeyTargets {
  /** Signed in. AuthScreen shows otherwise, and the dialogs these keys open
   *  are mounted app-wide, so they would open over it. */
  signedIn: boolean;
  /** An input, textarea or the editor holds the focus (New note yields). */
  editableFocused: () => boolean;
  makeNote: () => void;
  ask: () => void;
  newNote: () => void;
}

/** App.svelte's window keydown: run the chord's action. Returns whether it
 *  took the key (and then called preventDefault). */
export function runAppHotkey(e: HotkeyEvent & { preventDefault(): void }, to: HotkeyTargets): boolean {
  const action = appHotkey(e);
  if (!action || !to.signedIn) return false;
  // New note yields to a half-typed line: swapping the selected note out
  // from under it is not what the keystroke should do mid-sentence.
  if (action === 'newNote' && to.editableFocused()) return false;
  e.preventDefault();
  to[action]();
  return true;
}

const KEYS: Record<AppHotkey, string> = { makeNote: 'Shift+L', ask: 'Shift+J', newNote: 'N' };

/** The chord as the UI writes it: "Cmd+Shift+J" on macOS, "Ctrl+Shift+J" elsewhere. */
export function hotkeyLabel(action: AppHotkey, mac: boolean): string {
  return `${shortcutModLabel(mac)}+${KEYS[action]}`;
}
