// One-line corrections offered for near-miss property lines.
import { parsePropertyLine } from './grammar';

const pad = (n: number): string => String(n).padStart(2, '0');

/** `DD/MM`, `DD/MM/YY`, `DD/MM/YYYY` (Buddhist years > 2400 allowed) -> ISO, or null. */
function fixDate(value: string, today: Date): string | null {
  const m = /^(\d{1,2})\/(\d{1,2})(?:\/(\d{4}|\d{2}))?$/.exec(value);
  if (!m) return null;
  const d = Number(m[1]);
  const mo = Number(m[2]);
  let y = today.getFullYear();
  if (m[3] !== undefined) {
    y = Number(m[3]);
    if (m[3].length === 2) y += 2000;
    else if (y > 2400) y -= 543;
  }
  const probe = new Date(Date.UTC(2000, mo - 1, d));
  probe.setUTCFullYear(y);
  if (mo < 1 || mo > 12 || d < 1 || probe.getUTCMonth() !== mo - 1 || probe.getUTCDate() !== d) {
    return null;
  }
  return `${String(y).padStart(4, '0')}-${pad(mo)}-${pad(d)}`;
}

/** The corrected whole line, or null when it is valid already or not a near-miss. */
export function suggestFix(line: string, today: Date): string | null {
  let key: string;
  let value: string;
  let changed = false;
  const ok = parsePropertyLine(line);
  if (ok) {
    ({ key, value } = ok);
  } else {
    const m = /^([^:]+):(\S.*)$/.exec(line.replace(/ /g, ' '));
    if (!m || m[2].startsWith('//')) return null;
    const fixed = parsePropertyLine(`${m[1]}: ${m[2]}`);
    if (!fixed) return null;
    ({ key, value } = fixed);
    changed = true;
  }
  const date = fixDate(value, today);
  if (date !== null) {
    value = date;
    changed = true;
  }
  return changed ? `${key}: ${value}` : null;
}
