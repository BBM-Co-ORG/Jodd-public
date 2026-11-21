// The order the Tags lists show: the tags on the most notes first, so the
// ones in use are at the top of a long cloud (528 tags on the owner's
// account, 2026-11-13). Ties go A–Z, with the same localeCompare the store
// uses, so Thai and Latin sort as they do everywhere else.
//
// Only the Tags lists use it (the Sidebar's cloud and the phone Tags pane).
// The store (tagsByAccount) stays alphabetical: the editor's # picker reads
// that order.
export function byUse<T extends { tag: string; count: number }>(tags: readonly T[]): T[] {
  return [...tags].sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag));
}

/**
 * Several accounts' tag lists as one, for the All accounts cloud (spec
 * 2026-11-13 §2): the same name in two accounts is one tag, its counts
 * summed — what the cross-account tag filter already means by a tag
 * (`list_cached_notes_with_tags(null)` matches by name). Unordered: callers
 * sort with `byUse`.
 */
export function unionTags(lists: readonly (readonly { tag: string; count: number }[])[]): { tag: string; count: number }[] {
  const counts = new Map<string, number>();
  for (const list of lists) for (const t of list) counts.set(t.tag, (counts.get(t.tag) ?? 0) + t.count);
  return [...counts].map(([tag, count]) => ({ tag, count }));
}
