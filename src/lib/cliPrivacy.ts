// Privacy warnings for an agent-CLI provider (spec 2026-10-08 §4.6).
export type FileAccess = 'disabled' | 'enabled' | 'unknown';

export const FILE_ACCESS_NOTE =
  'This provider can read files on your computer by itself. Masking and hidden folders protect what Jodd sends; they cannot stop the tool reading files directly (LocalFs accounts keep notes as plain .eml files).';
export const ARGV_NOTE =
  'This provider receives text on the command line, visible to other programs on this computer. The text is masked.';

/** The same warning for a CLI that runs on the user's server over SSH. */
export function remoteFileAccessNote(host: string): string {
  return `This provider can read files on ${host} by itself. Masking and hidden folders protect what Jodd sends; they cannot stop the tool reading files on that server.`;
}

/** `host` is set for a CLI that runs on the user's server (see `hostLabel`). */
export function cliPrivacyNotes(fileAccess: FileAccess, argv: boolean, host: string | null = null): string[] {
  const notes: string[] = [];
  if (fileAccess !== 'disabled') notes.push(host ? remoteFileAccessNote(host) : FILE_ACCESS_NOTE);
  // A remote prompt always travels on stdin.
  if (argv && !host) notes.push(ARGV_NOTE);
  return notes;
}

/**
 * The visible half of the file-access warning's consequence. The first
 * sentence says the tool reads files; this says what that means for the
 * user's own protection — hiding a folder does not keep it from the tool —
 * which changes what they should do, so it stays visible (spec 2026-10-20
 * §4.4) instead of going under Learn more with the full explanation.
 */
export const FILE_ACCESS_TAIL = 'Hidden folders do not stop it.';

function isFileAccessNote(note: string): boolean {
  return note.startsWith('This provider can read files on ');
}

/**
 * A privacy note for a settings page: the first sentence stays visible (it is
 * what the user must act on), the rest goes under "Learn more" -- verbatim, so
 * nothing the note says is lost. A file-access note also keeps
 * `FILE_ACCESS_TAIL` visible. A note short enough for two lines is not split.
 */
export function splitNote(note: string, maxVisible = 120): { lead: string; more: string } {
  if (note.length <= maxVisible) return { lead: note, more: '' };
  const cut = note.indexOf('. ');
  if (cut < 0) return { lead: note, more: '' };
  const first = note.slice(0, cut + 1);
  const lead = isFileAccessNote(note) ? `${first} ${FILE_ACCESS_TAIL}` : first;
  return { lead, more: note.slice(cut + 2) };
}

/** Mirrors Rust's `host_label`: the host part of `target`, plus `:port` unless it is 22. */
export function hostLabel(h: { target: string; port: number | null }): string {
  const host = h.target.slice(h.target.lastIndexOf('@') + 1);
  return h.port != null && h.port !== 22 ? `${host}:${h.port}` : host;
}
