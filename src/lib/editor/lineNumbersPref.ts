// Whether the editor shows its line-number gutter. Off by default: a note
// looks as it always has until the user asks for numbers (⋯ › Show line
// numbers). One setting for every note, kept on this device, like the
// calendar preference (format/dates.ts).
import { writable } from 'svelte/store';

const STORAGE_KEY = 'jodd.lineNumbers';

function readStored(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === 'on';
  } catch {
    return false;
  }
}

export const lineNumbers = writable<boolean>(readStored());

export function setLineNumbers(on: boolean): void {
  try {
    localStorage.setItem(STORAGE_KEY, on ? 'on' : 'off');
  } catch {
    // Storage blocked: the store still carries the choice for this session.
  }
  lineNumbers.set(on);
}
