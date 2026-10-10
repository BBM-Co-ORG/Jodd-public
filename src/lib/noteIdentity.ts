import type { Note } from './types';

// JSON tuples avoid separator collisions (account IDs and Exchange UUIDs may
// themselves contain punctuation). Apple identifiers are never rewritten.
export type NoteIdentity = Pick<Note, 'account_id' | 'uuid'>;
const aliases = new Map<string, string>();
const rawKey = (n: NoteIdentity) => JSON.stringify([n.account_id ?? null, n.uuid]);
export function canonicalNote(n: NoteIdentity): NoteIdentity {
  let uuid = n.uuid;
  const seen = new Set<string>();
  while (!seen.has(uuid)) {
    seen.add(uuid);
    const next = aliases.get(rawKey({ ...n, uuid }));
    if (!next) break;
    uuid = next;
  }
  return { account_id: n.account_id, uuid };
}
export function noteKey(n: NoteIdentity): string { return rawKey(canonicalNote(n)); }
export function sameNote(a: NoteIdentity | null | undefined, b: NoteIdentity | null | undefined): boolean {
  return !!a && !!b && noteKey(a) === noteKey(b);
}
/** Only call with an authoritative save/alias-resolving SQLite response. */
export function forwardNoteIdentity(accountId: string, oldUuid: string, uuid: string): void {
  if (oldUuid !== uuid) aliases.set(rawKey({ account_id: accountId, uuid: oldUuid }), uuid);
}
export function clearNoteAliases(): void { aliases.clear(); }
