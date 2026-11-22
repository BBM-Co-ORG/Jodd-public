// Property-block and due-date grammar. Mirrors src-tauri/src/props.rs exactly;
// both read the fixtures in tests/fixtures/.

export interface Prop {
  pos: number;
  key: string;
  keyDisplay: string;
  value: string;
  valueDate: string | null;
}

const MAX_KEY_CHARS = 32;

const norm = (s: string): string => s.replace(/ /g, ' ');

const isAlphabetic = (c: string): boolean => /^\p{Alphabetic}$/u.test(c);
const isAlphanumeric = (c: string): boolean => /^[\p{Alphabetic}\p{N}]$/u.test(c);

/** Thai vowel/tone marks that are not all `alphanumeric`. */
function isKeyMark(c: string): boolean {
  const n = c.codePointAt(0)!;
  return n === 0x0e31 || (n >= 0x0e34 && n <= 0x0e3a) || (n >= 0x0e47 && n <= 0x0e4e);
}

/** Strict `YYYY-MM-DD` that names a real calendar day. */
function validIso(s: string): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
  if (!m) return null;
  const y = Number(m[1]);
  const mo = Number(m[2]);
  const d = Number(m[3]);
  const leap = (y % 4 === 0 && y % 100 !== 0) || y % 400 === 0;
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (mo < 1 || mo > 12 || d < 1 || d > days[mo - 1]) return null;
  return s;
}

export function parsePropertyLine(line: string): { key: string; value: string } | null {
  const text = norm(line);
  const colon = text.indexOf(':');
  if (colon < 0) return null;
  const key = text.slice(0, colon);
  const rest = text.slice(colon + 1);
  const chars = Array.from(key);
  if (chars.length === 0 || !isAlphabetic(chars[0])) return null;
  if (chars.length > MAX_KEY_CHARS || key.endsWith(' ')) return null;
  if (!chars.every((c) => isAlphanumeric(c) || isKeyMark(c) || c === ' ' || c === '-' || c === '_')) {
    return null;
  }
  if (!rest.startsWith(' ')) return null;
  const value = rest.trim();
  if (value === '') return null;
  return { key, value };
}

/** The run of property lines at the head of `linesAfterTitle`. */
export function parsePropertyBlock(linesAfterTitle: string[]): Prop[] {
  const out: Prop[] = [];
  for (let pos = 0; pos < linesAfterTitle.length; pos++) {
    const p = parsePropertyLine(linesAfterTitle[pos]);
    if (!p) break;
    out.push({
      pos,
      key: p.key.toLowerCase(),
      keyDisplay: p.key,
      value: p.value,
      valueDate: validIso(p.value),
    });
  }
  return out;
}

/** The due date in a task's text: `📅 YYYY-MM-DD` or `due: YYYY-MM-DD`; the last wins. */
export function parseDue(taskText: string): string | null {
  const text = norm(taskText);
  // ASCII-only lowercasing keeps offsets identical to `text`.
  const lower = text.replace(/[A-Z]/g, (c) => c.toLowerCase());
  let found: { at: number; date: string } | null = null;
  const consider = (at: number, after: number): void => {
    const tail = text.slice(after).replace(/^[ \t]+/, '');
    const date = validIso(tail.slice(0, 10));
    if (date === null || /^[0-9]$/.test(tail.charAt(10))) return;
    if (found === null || at > found.at) found = { at, date };
  };
  const cal = '\u{1F4C5}';
  for (let at = text.indexOf(cal); at >= 0; at = text.indexOf(cal, at + 1)) {
    let after = at + cal.length;
    if (text[after] === '️') after += 1;
    consider(at, after);
  }
  for (let at = lower.indexOf('due:'); at >= 0; at = lower.indexOf('due:', at + 1)) {
    const prev = Array.from(text.slice(0, at)).pop();
    if (prev === undefined || !isAlphanumeric(prev)) consider(at, at + 4);
  }
  return found === null ? null : (found as { at: number; date: string }).date;
}
