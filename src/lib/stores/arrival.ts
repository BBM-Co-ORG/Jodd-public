// The arrival: the one moment Jodd marks on purpose (2026-11-09 workspace
// review). Right after someone signs in from the door, their notes appear,
// and the question in their head is "did that just move my notes?" One line
// answers it: how many arrived, and that they stayed where they were.
//
// Rendered once by Arrival.svelte (App.svelte). Not the toast: the toast is a
// one-line, 1.5 s confirmation that cannot be read slowly or dismissed, and
// this is a sentence worth reading.
import { writable } from 'svelte/store';
import { placeName, plural } from '../errorText';

export interface ArrivalMessage {
  id: number;
  lines: string[];
}

/** One account's first index: its backend and how many notes it listed. */
export interface Arrived {
  kind: string;
  count: number;
}

export const arrival = writable<ArrivalMessage | null>(null);

/** Long enough to read twice; the close button is there for anyone faster. */
export const ARRIVAL_MS = 12_000;

let seq = 0;
let timer: ReturnType<typeof setTimeout> | undefined;

/**
 * The words, separate from the timer so they can be tested as words.
 * Returns no lines when there is nothing to say (no account was indexed).
 */
export function arrivalLines(found: Arrived[]): string[] {
  if (found.length === 0) return [];
  const lines = found.map(({ kind, count }) =>
    count === 0
      ? `No notes in ${placeName(kind)} yet.`
      : `${plural(count, 'note')} found in ${placeName(kind)}.`,
  );
  // True of every backend: the index only reads, and the cache is a copy.
  lines.push(
    found.some((f) => f.count > 0)
      ? 'They stay where they are. Jodd keeps a copy on this device.'
      : 'Notes you write in Jodd will sync there.',
  );
  return lines;
}

export function announceArrival(found: Arrived[]): void {
  const lines = arrivalLines(found);
  if (lines.length === 0) return;
  clearTimeout(timer);
  const id = ++seq;
  arrival.set({ id, lines });
  timer = setTimeout(() => arrival.update((a) => (a?.id === id ? null : a)), ARRIVAL_MS);
}

export function dismissArrival(): void {
  clearTimeout(timer);
  arrival.set(null);
}
