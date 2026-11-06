// A brief, non-blocking confirmation ("Copied [[slug]]"), shown once at the
// app level by Toast.svelte (App.svelte). One at a time: a new toast replaces
// the one showing and restarts the timer. Errors do not go here — they go to
// the `error` store and ErrorBar, which waits for the user to dismiss them.
import { writable } from 'svelte/store';

export interface ToastMessage {
  id: number;
  text: string;
}

export const toast = writable<ToastMessage | null>(null);

let seq = 0;
let timer: ReturnType<typeof setTimeout> | undefined;

export function showToast(text: string, ms = 1500): void {
  clearTimeout(timer);
  const id = ++seq;
  toast.set({ id, text });
  timer = setTimeout(() => toast.update((t) => (t?.id === id ? null : t)), ms);
}
