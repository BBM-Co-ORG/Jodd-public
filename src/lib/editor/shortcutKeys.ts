// The primary modifier for editor shortcuts: ⌘ on macOS, Ctrl everywhere
// else — never either-or.
//
// The editor used to accept `metaKey || ctrlKey` on every platform. On macOS
// that made Ctrl+Cmd+Z an undo (it reads like a redo chord and is not one),
// made Cmd+Y a redo nobody documented, and took Ctrl+E / Ctrl+K / Ctrl+B away
// from the system's own text bindings (end of line, delete to end of line,
// back one character) to apply inline code, a link and bold instead.

export interface ModifierState {
  metaKey: boolean;
  ctrlKey: boolean;
}

export function isShortcutMod(e: ModifierState, mac: boolean): boolean {
  return mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
}

// The key name the shortcut cheatsheet shows for that modifier.
export function shortcutModLabel(mac: boolean): string {
  return mac ? 'Cmd' : 'Ctrl';
}
