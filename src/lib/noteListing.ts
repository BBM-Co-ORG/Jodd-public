// What the note list shows, in the order it shows it — one pure function for
// NoteList's `filteredNotes` AND for the selection rules that must agree with
// it (controllers/selection.ts: "open the first note in list order", spec
// 2026-10-20 §5.3). Re-sorting anywhere else is how the two would drift: the
// editor would open a note that is not the one at the top of the list.
import type { Note } from './types';
import { getNoteTags, type SmartFolderKind } from './stores/notes';
import { noteKey, canonicalNote } from './noteIdentity';

export type TagScope = 'account' | 'all';

/** Cross-account tag results, tagged with the tag set they answer. */
export interface CrossAccountTagNotes {
  key: string;
  notes: Note[];
}

/** The key `CrossAccountTagNotes` carries for a tag set: order-free. */
export function tagSetKey(tags: Iterable<string>): string {
  return JSON.stringify([...tags].sort());
}

export interface ListingInput {
  /** The raw search query; any non-blank text means the list shows `searchResults`. */
  query: string;
  searchResults: Note[];
  smartFolder: { account: string; kind: SmartFolderKind } | null;
  smartFolderNotes: Note[];
  selectedTags: Set<string>;
  tagMatchMode: 'AND' | 'OR';
  noteTags: Map<string, Map<string, string[]>>;
  tagScope: TagScope;
  crossAccountTagNotes: CrossAccountTagNotes;
  notes: Note[];
  currentAccount: string | null;
  selectedFolder: string;
  pendingDeleted: Set<string>;
}

/**
 * The listing, pinned first and then newest first.
 *
 * Source by precedence: a search (`searchResults`), then a Smart Folder
 * (`smartFolderNotes`), then a tag filter (in-memory `notes` for the current
 * account, or the cross-account results when `tagScope` is 'all' — only those
 * that answer the CURRENT tag set), then the folder (`__ALL__` = every note of
 * the current account).
 *
 * Every row is deduplicated by identity, preferring the in-memory copy when it
 * is at least as new, and rows pending deletion are left out.
 */
export function listedNotes(i: ListingInput): Note[] {
  const q = i.query.trim().toLowerCase();
  const inAccount = (n: Note) => n.account_id === i.currentAccount;
  let base: Note[];
  if (!q) {
    if (i.smartFolder) {
      // Smart Folders are fully virtual — the note set comes straight from
      // App.svelte's loadSmartFolderNotes, not from filtering notes by label
      // (there is no label to match).
      base = i.smartFolderNotes;
    } else if (i.selectedTags.size > 0) {
      // Tag view takes precedence over the folder selection. AND = note has
      // every selected tag; OR = note has any. App.paintTagsFromCache has
      // already loaded the union of these tags' notes into notes.
      const sel = [...i.selectedTags];
      const matches = (n: Note) => {
        const tags = getNoteTags(i.noteTags, n.account_id, n.uuid);
        return i.tagMatchMode === 'AND'
          ? sel.every((t) => tags.includes(t))
          : sel.some((t) => tags.includes(t));
      };
      // 'all' → cross-account results from the backend (no inAccount filter);
      // 'account' → narrow the in-memory current-account notes.
      if (i.tagScope === 'all') {
        const cross = i.crossAccountTagNotes.key === tagSetKey(i.selectedTags) ? i.crossAccountTagNotes.notes : [];
        base = cross.filter(matches);
      } else {
        base = i.notes.filter((n) => inAccount(n) && matches(n));
      }
    } else {
      base = i.selectedFolder === '__ALL__'
        ? i.notes.filter(inAccount)
        : i.notes.filter((n) => inAccount(n) && n.label === i.selectedFolder);
    }
  } else {
    // FTS-backed: results come from the SQLite index (every note, Thai-aware),
    // not an in-memory substring scan limited to loaded notes.
    base = i.searchResults; // already scoped by the backend (may span accounts)
  }
  // Pinned-first sort. Date is RFC 2822 or ISO; Date.parse handles both
  // and returns NaN-safe when malformed (treated as 0, sinks to the
  // bottom of its group rather than crashing the sort). Stable sort
  // semantics keep within-group order consistent with the source list,
  // which matters for the "multiple pinned notes keep their relative
  // date order" check in the verification step.
  const live = new Map(i.notes.map((n) => [noteKey(n), n]));
  const unique = new Map<string, Note>();
  for (const row of base) {
    const key = noteKey(row);
    if (i.pendingDeleted.has(key)) continue;
    const local = live.get(key);
    const value = local && (local.local_version ?? 0) >= (row.local_version ?? 0) ? local : row;
    unique.set(key, { ...value, uuid: canonicalNote(value).uuid });
  }
  return [...unique.values()].sort((a, b) => {
    const ap = a.pinned ? 1 : 0;
    const bp = b.pinned ? 1 : 0;
    if (ap !== bp) return bp - ap; // pinned DESC
    const ad = Date.parse(a.date) || 0;
    const bd = Date.parse(b.date) || 0;
    return bd - ad; // date DESC
  });
}
