/**
 * Orders an account's tags for the tags sheet / # list as the user types:
 * tags that START with the query first, then tags with a word starting with
 * it (after `-`, `_`, `/` or `.`), then any other tag containing it — each
 * tier in the order given. `query` is already normalized. Live pass
 * 2026-10-21: "l" listed #ai-agentic-workflows and #ai-tools ahead of
 * #live-commerce. The limit applies after ranking, so a prefix match is
 * never cut by earlier substring ones.
 */
export function rankTagSuggestions(tags: readonly string[], query: string, limit = Infinity): string[] {
  if (query === '') return tags.slice(0, limit);
  const prefix: string[] = [];
  const wordStart: string[] = [];
  const inside: string[] = [];
  for (const t of tags) {
    const at = t.indexOf(query);
    if (at < 0) continue;
    if (at === 0) prefix.push(t);
    else if (isWordStart(t, query)) wordStart.push(t);
    else inside.push(t);
  }
  return [...prefix, ...wordStart, ...inside].slice(0, limit);
}

function isWordStart(tag: string, query: string): boolean {
  for (let i = tag.indexOf(query); i > 0; i = tag.indexOf(query, i + 1)) {
    if ('-_/.'.includes(tag[i - 1])) return true;
  }
  return false;
}
