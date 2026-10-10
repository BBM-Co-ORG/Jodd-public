// Privacy warnings for an agent-CLI provider (spec 2026-10-08 §4.6).
export type FileAccess = 'disabled' | 'enabled' | 'unknown';

export const FILE_ACCESS_NOTE =
  'This provider can read files on your computer by itself. Masking and hidden folders protect what Jodd sends; they cannot stop the tool reading files directly (LocalFs accounts keep notes as plain .eml files).';
export const ARGV_NOTE =
  'This provider receives text on the command line, visible to other programs on this computer. The text is masked.';

export function cliPrivacyNotes(fileAccess: FileAccess, argv: boolean): string[] {
  const notes: string[] = [];
  if (fileAccess !== 'disabled') notes.push(FILE_ACCESS_NOTE);
  if (argv) notes.push(ARGV_NOTE);
  return notes;
}
