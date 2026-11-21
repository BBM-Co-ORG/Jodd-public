// What the error bar says (2026-11-09 workspace review: "raw String(e)
// errors in a translucent red bar; offline/rate-limit shown as red
// failures").
//
// The backend's errors are written for logs: TransportError's Display gives
// "transient: error sending request for url (https://…)" or
// "rate limited (retry_after=Some(30s))". Shown as they are, being offline
// reads like a crash, and the user cannot tell what still works.
//
// Two rules:
//  - Say what did not happen, in the user's words ("Couldn't delete the
//    note."). A backend reason that is already a sentence stays as the
//    reason; machine text moves to a details line the bar shows smaller.
//  - Offline and rate limits are not failures in a local-first app: the
//    edit is in SQLite and the worker retries. A listing that hits either
//    says so calmly (isCalm), and the bar drops the alarm colour.

/** The details line's prefix: ErrorBar shows such a line smaller, without it. */
export const DETAIL_PREFIX = 'Details: ';

/** Where an account's notes live, said the way the user knows the place. */
export function placeName(kind: string | null | undefined): string {
  switch (kind) {
    case 'icloud': return 'iCloud';
    case 'gmail': return 'Gmail';
    case 'microsoft': return 'Outlook';
    case 'local_fs': return 'your Local Folder';
    case 'ssh': return 'your SSH server';
    default: return kind || 'your account';
  }
}

/** "1 note", "3 notes", "1,204 notes". */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString('en-US')} ${n === 1 ? one : many}`;
}

const OFFLINE =
  /\btransient:|error sending request|timed out|dns error|tcp connect|connection (refused|reset|closed)|network is unreachable|network error|no route to host|failed to lookup address|could not resolve host|internet connection appears to be offline/i;
const RATE_LIMITED = /rate.?limit|too many requests|\b429\b|quota exceeded/i;

export type ErrorKind = 'offline' | 'rate_limited' | 'other';

export function errorKind(e: unknown): ErrorKind {
  const raw = String(e ?? '');
  if (RATE_LIMITED.test(raw)) return 'rate_limited';
  if (OFFLINE.test(raw)) return 'offline';
  return 'other';
}

/** The error's own text, without JS's "Error: " or TransportError's kind prefix. */
export function rawText(e: unknown): string {
  const s = e instanceof Error ? e.message : String(e ?? '');
  return s.replace(/^(Error|permanent|transient):\s*/i, '').trim();
}

// Already a sentence for a person: starts with a capital, ends like one, and
// carries nothing that reads as code.
function isSentence(s: string): boolean {
  return /^[A-Z]/.test(s) && /[.!?]$/.test(s) && !/[{}=]|\bSome\(|https?:\/\//.test(s);
}

/**
 * A failed action, said as what did not happen and why.
 * `action` completes "Couldn't …": "delete the note", "move 3 notes".
 */
export function failureText(action: string, e: unknown): string {
  const lead = `Couldn't ${action}.`;
  switch (errorKind(e)) {
    case 'offline':
      return `${lead} Jodd can't reach the internet right now. Try again when you're back online.`;
    case 'rate_limited':
      return `${lead} The server asked Jodd to slow down. Try again in a minute.`;
  }
  const raw = rawText(e);
  if (!raw) return lead;
  if (isSentence(raw)) return `${lead} ${raw}`;
  return `${lead}\n${DETAIL_PREFIX}${raw}`;
}

const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/**
 * One account's listing failed. `place` is placeName(), with the address
 * added by the caller when two accounts share a backend.
 */
export function listingFailureText(place: string, e: unknown): string {
  switch (errorKind(e)) {
    case 'offline':
      return `Can't reach ${place} right now. Your notes are safe on this device and will sync when you're back online.`;
    case 'rate_limited':
      return `${capitalize(place)} asked Jodd to slow down. Syncing will resume on its own.`;
  }
  return failureText(`refresh ${place}`, e);
}

const CALM = [
  /^Can't reach .+ right now\. Your notes are safe on this device and will sync when you're back online\.$/,
  /^.+ asked Jodd to slow down\. Syncing will resume on its own\.$/,
];

/**
 * True when every line is a calm listing message: nothing failed that the
 * user must act on, so the bar takes the neutral tone and is announced
 * politely rather than as an alert.
 */
export function isCalm(text: string | null | undefined): boolean {
  if (!text) return false;
  const lines = text.split('\n').filter((l) => l.trim() !== '');
  return lines.length > 0 && lines.every((l) => CALM.some((re) => re.test(l)));
}
