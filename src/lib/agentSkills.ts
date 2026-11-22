// Folders published as Claude Code skills (spec 2026-11-05 §6–§7). Pure
// helpers and the store the folder menu sets, so the rules the dialog and the
// Settings list follow are tested without mounting them.
import { writable } from 'svelte/store';

/** Set by the folder menu; the dialog (mounted by App) is open while set. */
export const skillRequest = writable<{ accountId: string; folder: string } | null>(null);

export const SKILL_PREFIX = 'jodd-';
/** Mirrors Rust's `mcp_scope::SKILL_NAME_MAX`: `jodd-<name>` fits in 64. */
export const SKILL_NAME_MAX = 58;

export interface SkillStatus {
  name: string;
  account_id: string;
  folder: string;
  state: 'published' | 'not_published' | 'removed' | string;
  waiting_for_review: string[];
  problems: string[];
  files: number;
}

export interface SkillsStatus {
  finished_at: string;
  skills_root: string;
  skills: SkillStatus[];
  problems: string[];
}

export interface SkillRow {
  name: string;
  account_id: string;
  folder: string;
  assets_dir: string | null;
  only_reviewed: boolean;
  email: string;
  status: SkillStatus | null;
}

export interface SkillsView {
  skills_root: string;
  skills: SkillRow[];
  last_finished_at: string | null;
  problems: string[];
  error: string | null;
}

/** The same rule as Rust's `check_skill_name`. */
export function validSkillName(name: string): boolean {
  return name.length > 0 && name.length <= SKILL_NAME_MAX && /^[a-z0-9]+(-[a-z0-9]+)*$/.test(name);
}

/** A name from the folder's last segment, or '' when nothing usable is left
 * (a Thai name, say) — the dialog then asks for one. */
export function suggestSkillName(folder: string): string {
  const leaf = folder.split('/').pop() ?? '';
  const ascii = leaf.normalize('NFKD').replace(/[̀-ͯ]/g, '').toLowerCase();
  const slug = ascii.replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  return slug.slice(0, SKILL_NAME_MAX).replace(/-+$/, '');
}

/** One line for a skill's state. */
export function skillStateLine(row: SkillRow): string {
  const st = row.status;
  if (!st) return 'Not synced yet — it is published at the next sync.';
  const waiting = st.waiting_for_review.length;
  const tail = waiting ? ` ${waiting} ${waiting === 1 ? 'note waits' : 'notes wait'} for Mark reviewed.` : '';
  if (st.state === 'published') return `Published as ${SKILL_PREFIX}${row.name} (${st.files} files).${tail}`;
  if (st.state === 'removed') return 'Not published: removed from Claude Code.';
  return `Not updated.${tail}`;
}

/** The skill this folder is already published as, if any. */
export function skillForFolder(view: SkillsView | null, accountId: string, folder: string): SkillRow | null {
  return view?.skills.find(s => s.account_id === accountId && s.folder === folder) ?? null;
}
