// A brief, non-blocking confirmation ("Copied [[slug]]"), shown once at the
// app level by Toast.svelte (App.svelte). One at a time: a new toast replaces
// the one showing and restarts the timer. Errors do not go here — they go to
// the `error` store and ErrorBar, which waits for the user to dismiss them.
import { writable } from 'svelte/store';

export interface ToastMessage {
  id: number;
  text: string;
  /** Set by `showToastLines`: one wrapped line each, never cut short. */
  lines?: string[];
}

export const toast = writable<ToastMessage | null>(null);

let seq = 0;
let timer: ReturnType<typeof setTimeout> | undefined;

export function showToast(text: string, ms = 1500): void {
  show({ id: ++seq, text }, ms);
}

/** Time to read a line-per-item toast: more lines, longer on screen. */
export const LINES_BASE_MS = 2500;
export const LINES_PER_LINE_MS = 2500;

/** Several short results, one line each — the saved-copy notices of a link
 *  ingest (spec 2026-11-19 §5.2: the result always says which happened).
 *  Lines wrap rather than being cut with an ellipsis. */
export function showToastLines(lines: string[]): void {
  if (lines.length === 0) return;
  show({ id: ++seq, text: lines.join('\n'), lines }, LINES_BASE_MS + LINES_PER_LINE_MS * lines.length);
}

function show(message: ToastMessage, ms: number): void {
  clearTimeout(timer);
  toast.set(message);
  timer = setTimeout(() => toast.update((t) => (t?.id === message.id ? null : t)), ms);
}
