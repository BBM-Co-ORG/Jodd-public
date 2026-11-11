// Pure Keep-a-Changelog parser. No I/O — used by both the build-time
// generator (gen-changelog.mjs) and the CI section extractor
// (changelog-section.mjs), and unit-tested directly.

const VERSION_HEADER = /^##\s+\[([^\]]+)\](?:\s*-\s*(.+))?\s*$/;
// NOTE: assumes changelog bodies do not contain fenced code blocks — a `### `
// line inside a code fence would be misread as a group header. Acceptable here.
const GROUP_HEADER = /^###\s+(.+?)\s*$/;
const BULLET = /^[-*]\s+(.+)\s*$/;

/**
 * Parse a CHANGELOG.md string into ordered entries (file order = newest first).
 * @param {string} md
 * @returns {{version: string, date: string|null, sections: Record<string,string[]>}[]}
 */
export function parseChangelog(md) {
  const lines = md.split(/\r?\n/);
  /** @type {{version: string, date: string|null, sections: Record<string,string[]>}[]} */
  const entries = [];
  /** @type {{version: string, date: string|null, sections: Record<string,string[]>} | null} */
  let cur = null;
  /** @type {string | null} */
  let group = null;
  for (const line of lines) {
    const vh = line.match(VERSION_HEADER);
    if (vh) {
      cur = { version: vh[1].trim(), date: vh[2] ? vh[2].trim() : null, sections: {} };
      entries.push(cur);
      group = null;
      continue;
    }
    if (!cur) continue;
    const gh = line.match(GROUP_HEADER);
    if (gh) {
      group = gh[1].trim();
      cur.sections[group] ??= [];
      continue;
    }
    const b = line.match(BULLET);
    if (b && group) cur.sections[group].push(b[1].trim());
  }
  return entries;
}

/**
 * Return the raw markdown body for one version (everything after its header
 * line up to the next `## ` header), trimmed. null if the version is absent.
 * @param {string} md
 * @param {string} version
 * @returns {string | null}
 */
export function sectionRawText(md, version) {
  const lines = md.split(/\r?\n/);
  let start = -1;
  for (let i = 0; i < lines.length; i++) {
    const vh = lines[i].match(VERSION_HEADER);
    if (vh && vh[1].trim() === version) { start = i + 1; break; }
  }
  if (start === -1) return null;
  let end = lines.length;
  for (let i = start; i < lines.length; i++) {
    if (/^##\s+/.test(lines[i])) { end = i; break; }
  }
  return lines.slice(start, end).join('\n').trim() || null;
}

/**
 * Sections written for the people shipping Jodd, not its users: kept in
 * CHANGELOG.md and the release notes, left out of What's New and of the
 * updater's `latest.json` notes. One list for both (src/lib/whatsNew.ts).
 */
/** @type {readonly string[]} */
export const DEVELOPER_ONLY_SECTIONS = ['Verification scope'];

/**
 * One version's raw section without the developer-only groups (each from its
 * `### ` header up to the next one). null if the version is absent.
 * @param {string} md
 * @param {string} version
 * @returns {string | null}
 */
export function userNotes(md, version) {
  const raw = sectionRawText(md, version);
  if (raw === null) return null;
  const out = [];
  let skipping = false;
  for (const line of raw.split(/\r?\n/)) {
    const gh = line.match(GROUP_HEADER);
    if (gh) skipping = DEVELOPER_ONLY_SECTIONS.includes(gh[1].trim());
    if (!skipping) out.push(line);
  }
  return out.join('\n').trim() || null;
}
