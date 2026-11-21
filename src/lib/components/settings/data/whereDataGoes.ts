// Settings › Privacy & Data › Where your data goes (spec 2026-11-11 §1): one row
// per channel that can send data off this device, from status DTOs the page
// read without touching the credential store (D7). Pure, so every on, off,
// unknown and platform state is unit-tested.
import type { SettingsSection } from '../../../settings/route';
import { cliPrivacyNotes, hostLabel } from '../../../cliPrivacy';
import { AGENTS_OWN_RULES, AGENTS_READ, NOT_ON_THIS_DEVICE } from '../../../settings/governanceCopy';

export type RowId = 'ai' | 'speech' | 'links' | 'youtube' | 'tvs' | 'backups' | 'files' | 'skills' | 'agents' | 'updates';

export type Row = {
  id: RowId;
  name: string;
  lines: string[];
  /** Privacy warnings shown in full (E7): a CLI that reads files itself, a prompt on argv. */
  warn: string[];
  /** Where it is set; null when there is nothing to change here (unknown, or not on this device). */
  change: SettingsSection | null;
};

type Llm = {
  provider: string;
  data_allowed?: boolean | null;
  http_base_url: string | null;
  agent_preset: string | null;
  agent_host: { target: string; port: number | null } | null;
};
type Preset = { id: string; label: string; file_access: 'disabled' | 'enabled' | 'unknown'; argv_prompt: boolean };

/** Each field is undefined when its status call failed: that row reads "Unknown". */
export type Status = {
  android: boolean;
  app?: { cfg: { llm: Llm; apply_to_accounts: boolean }; unreadable?: string | null };
  /** Active accounts; `llm` null when it could not be read. */
  accounts: { id: string; email: string; llm: Llm | null }[];
  presets?: Preset[];
  stt?: { cfg: { base_url: string; confirmed_host: string | null } };
  links?: { auto_follow: string[] };
  youtube?: boolean | null;
  tvs?: unknown[];
  backups?: unknown[];
  workspace?: { account_id: string | null; error?: string | null };
  files?: { enabled: boolean; path: string; default_path: string };
  skills?: { skills_root: string; skills: unknown[] };
  updateAutoCheck?: boolean;
};

const UNKNOWN = ['Unknown'];
const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** The account may send data: the same rule as Rust `account_allowed` (minus status, which the caller filters). */
function allowed(llm: Llm): boolean {
  return llm.provider !== 'disabled' && llm.data_allowed === true;
}

function originOf(url: string): { origin: string; host: string; secure: boolean } | null {
  try {
    const u = new URL(url);
    if (u.protocol !== 'https:' && u.protocol !== 'http:') return null;
    return { origin: `${u.protocol}//${u.host}`.toLowerCase(), host: u.host, secure: u.protocol === 'https:' };
  } catch {
    return null;
  }
}

function providerLine(llm: Llm, presets: Preset[]): string {
  switch (llm.provider) {
    case 'http':
      return llm.http_base_url ? `Sends to ${originOf(llm.http_base_url)?.host ?? llm.http_base_url}` : 'Not set up';
    case 'claude_code':
      return 'Claude Code on this device';
    case 'agent_cli': {
      const label = presets.find((p) => p.id === llm.agent_preset)?.label ?? 'Agent CLI';
      return llm.agent_host ? `${label} on AI host ${hostLabel(llm.agent_host)}` : `${label} on this device`;
    }
    default:
      return 'Not set up';
  }
}

function cliWarnings(llm: Llm, presets: Preset[]): string[] {
  if (llm.provider !== 'agent_cli') return [];
  const p = presets.find((x) => x.id === llm.agent_preset);
  if (!p) return [];
  return cliPrivacyNotes(p.file_access, p.argv_prompt, llm.agent_host ? hostLabel(llm.agent_host) : null);
}

function aiRow(s: Status): Row {
  const row: Row = { id: 'ai', name: 'AI provider', lines: UNKNOWN, warn: [], change: null };
  if (!s.app || s.app.unreadable) return row;
  const presets = s.presets ?? [];
  const known = s.accounts.filter((a) => a.llm !== null) as { id: string; email: string; llm: Llm }[];
  const n = known.filter((a) => allowed(a.llm)).length;
  const own = known.filter((a) => a.llm.provider !== 'none' && a.llm.provider !== 'disabled');
  const lines = [providerLine(s.app.cfg.llm, presets)];
  if (known.length > 0) {
    lines.push(known.length === 1
      ? `${n} of 1 account allows its data.`
      : `${n} of ${known.length} accounts allow their data.`);
  }
  // An account whose settings could not be read is said as such, not left
  // out: a count that silently skips it would read as the whole truth.
  const unread = s.accounts.length - known.length;
  if (unread > 0) lines.push(unread === 1 ? '1 account cannot be read.' : `${unread} accounts cannot be read.`);
  if (own.length > 0) lines.push(own.length === 1 ? '1 account uses its own provider.' : `${own.length} accounts use their own provider.`);
  const warn = [...new Set([s.app.cfg.llm, ...own.map((a) => a.llm)].flatMap((l) => cliWarnings(l, presets)))];
  return { ...row, lines, warn, change: 'ai' };
}

function speechLines(stt: NonNullable<Status['stt']>): string[] {
  const o = stt.cfg.base_url ? originOf(stt.cfg.base_url) : null;
  // Audio goes only while the confirmed origin still equals the address's
  // origin (stt_config::ready); an unconfirmed edit sends nothing.
  if (!o || stt.cfg.confirmed_host?.toLowerCase() !== o.origin) return ['Off. No audio leaves this device.'];
  return [`Audio goes to ${o.host}${o.secure ? '' : ' (not encrypted)'}`];
}

/** `value` undefined → Unknown; otherwise `lines(value)` with a Change link. */
function simple<T>(id: RowId, name: string, change: SettingsSection, value: T | undefined, lines: (v: T) => string[]): Row {
  return value === undefined
    ? { id, name, lines: UNKNOWN, warn: [], change: null }
    : { id, name, lines: lines(value), warn: [], change };
}

const notHere = (id: RowId, name: string): Row => ({ id, name, lines: [NOT_ON_THIS_DEVICE], warn: [], change: null });

export function whereDataGoes(s: Status): Row[] {
  const emailOf = (id: string) => s.accounts.find((a) => a.id === id)?.email ?? id;
  return [
    aiRow(s),
    simple('speech', 'Speech to text', 'speech', s.stt, speechLines),
    simple('links', 'Followed links', 'links', s.links, (l) =>
      l.auto_follow.length === 0 ? ['Off'] : [`On for ${plural(l.auto_follow.length, 'account', 'accounts')}`]),
    simple('youtube', 'YouTube comments', 'links', s.youtube, (k) =>
      k === true ? ["Key saved. Asks Google's YouTube API."] : ['Off']),
    simple('tvs', 'Linked TVs', 'share', s.tvs, (t) => (t.length === 0 ? ['None'] : [`${t.length} linked. Asks YouTube.`])),
    simple('backups', 'Backups', 'backup', s.backups, (b) =>
      b.length === 0
        ? ['None']
        : [`${plural(b.length, 'folder', 'folders')}. Plain files, not encrypted.`, 'Removing a folder keeps its backups.']),
    s.android
      ? notHere('files', 'Notes on disk')
      : simple('files', 'Notes on disk', 'ai', s.files, (f) =>
          f.enabled ? [`On. Plain Markdown in ${f.path || f.default_path}, not encrypted.`] : ['Off']),
    s.android
      ? notHere('skills', 'Skills for Claude Code')
      : simple('skills', 'Skills for Claude Code', 'ai', s.skills, (k) =>
          k.skills.length === 0 ? ['None'] : [`${k.skills.length} published to ${k.skills_root}`]),
    s.android
      ? notHere('agents', 'AI agents (jodd-mcp)')
      // An unreadable scope file is not "not set up": jodd-mcp then refuses
      // every read, and the row cannot say what agents see.
      : simple('agents', 'AI agents (jodd-mcp)', 'ai', s.workspace?.error ? undefined : s.workspace, (w) => [
          w.account_id ? `Agent workspace in ${emailOf(w.account_id)}` : 'Agent workspace not set up',
          AGENTS_READ,
          AGENTS_OWN_RULES,
        ]),
    // Every platform checks since the desktop updater (#318).
    simple('updates', 'Update check', 'about', s.updateAutoCheck, (on) =>
      on ? ['Once a day. Asks GitHub; sends no notes.'] : ['Off']),
  ];
}
