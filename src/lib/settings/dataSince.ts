// Settings › Advanced › Diagnostics: when Jodd's data on this device began
// (src-tauri/src/data_since.rs). It is the one clue that tells "a setting was
// lost" apart from "everything was wiped" (an uninstall or Clear storage).

import { formatNoteDateTime } from '../format/dates';

export interface DataSince {
  since_ms: number;
  /** Settings were already there when the date was first recorded. */
  found_existing: boolean;
}

export function dataSinceText(d: DataSince | null): string | null {
  if (!d || !Number.isFinite(d.since_ms) || d.since_ms <= 0) return null;
  const when = formatNoteDateTime(new Date(d.since_ms).toISOString());
  return d.found_existing
    ? `Jodd's data on this device dates from ${when} or earlier.`
    : `Jodd's data on this device starts ${when}.`;
}
