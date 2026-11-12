import { describe, expect, it } from 'vitest';
import { whereDataGoes, type Status, type RowId } from './whereDataGoes';
import { FILE_ACCESS_NOTE } from '../../../cliPrivacy';
import { AGENTS_OWN_RULES, AGENTS_READ, NOT_ON_THIS_DEVICE } from '../../../settings/governanceCopy';

const NONE = { provider: 'none', data_allowed: false, http_base_url: null, agent_preset: null, agent_host: null } as const;

function base(over: Partial<Status> = {}): Status {
  return {
    android: false,
    app: { cfg: { llm: { ...NONE }, apply_to_accounts: false }, unreadable: null },
    accounts: [
      { id: 'gmail:a@x.com', email: 'a@x.com', llm: { ...NONE, data_allowed: true } },
      { id: 'local_fs:v', email: 'Vault', llm: { ...NONE } },
    ],
    presets: [],
    stt: { cfg: { base_url: '', confirmed_host: null } },
    links: { auto_follow: [] },
    youtube: false,
    tvs: [],
    backups: [],
    workspace: { account_id: null },
    files: { enabled: false, path: '', default_path: '/Users/me/Jodd for agents' },
    skills: { skills_root: '/Users/me/.claude/skills', skills: [] },
    updateAutoCheck: true,
    ...over,
  };
}

const row = (s: Status, id: RowId) => whereDataGoes(s).find((r) => r.id === id)!;

describe('whereDataGoes — every channel, in order', () => {
  it('lists the ten rows in the spec order', () => {
    expect(whereDataGoes(base()).map((r) => r.id)).toEqual(
      ['ai', 'speech', 'links', 'youtube', 'tvs', 'backups', 'files', 'skills', 'agents', 'updates'],
    );
  });
});

describe('off states say so plainly', () => {
  it('AI not set up', () => expect(row(base(), 'ai').lines[0]).toBe('Not set up'));
  it('speech off', () => expect(row(base(), 'speech').lines).toEqual(['Off. No audio leaves this device.']));
  it('an address that was never confirmed sends nothing', () => {
    const s = base({ stt: { cfg: { base_url: 'https://api.groq.com/openai/v1', confirmed_host: null } } });
    expect(row(s, 'speech').lines).toEqual(['Off. No audio leaves this device.']);
  });
  it('links off', () => expect(row(base(), 'links').lines).toEqual(['Off']));
  it('youtube off', () => expect(row(base(), 'youtube').lines).toEqual(['Off']));
  it('no TVs', () => expect(row(base(), 'tvs').lines).toEqual(['None']));
  it('no backups', () => expect(row(base(), 'backups').lines).toEqual(['None']));
  it('notes on disk off', () => expect(row(base(), 'files').lines).toEqual(['Off']));
  it('no skills', () => expect(row(base(), 'skills').lines).toEqual(['None']));
});

describe('on states name the destination', () => {
  it('HTTP provider: the origin', () => {
    const s = base({ app: { cfg: { llm: { ...NONE, provider: 'http', http_base_url: 'https://api.openai.com/v1' }, apply_to_accounts: true }, unreadable: null } });
    expect(row(s, 'ai').lines[0]).toBe('Sends to api.openai.com');
  });

  it('a CLI preset: its label, on this device', () => {
    const s = base({
      app: { cfg: { llm: { ...NONE, provider: 'agent_cli', agent_preset: 'claude' }, apply_to_accounts: false }, unreadable: null },
      presets: [{ id: 'claude', label: 'Claude Code', file_access: 'disabled', argv_prompt: false }],
    });
    expect(row(s, 'ai').lines[0]).toBe('Claude Code on this device');
    expect(row(s, 'ai').warn).toEqual([]);
  });

  it('an AI host: the server', () => {
    const s = base({
      app: { cfg: { llm: { ...NONE, provider: 'agent_cli', agent_preset: 'claude', agent_host: { target: 'jodd@box.example', port: 2222 } }, apply_to_accounts: false }, unreadable: null },
      presets: [{ id: 'claude', label: 'Claude Code', file_access: 'disabled', argv_prompt: false }],
    });
    expect(row(s, 'ai').lines[0]).toBe('Claude Code on AI host box.example:2222');
  });

  it('a CLI that can read files by itself carries its whole warning (E7)', () => {
    const s = base({
      app: { cfg: { llm: { ...NONE, provider: 'agent_cli', agent_preset: 'codex' }, apply_to_accounts: false }, unreadable: null },
      presets: [{ id: 'codex', label: 'Codex', file_access: 'enabled', argv_prompt: false }],
    });
    expect(row(s, 'ai').warn).toEqual([FILE_ACCESS_NOTE]);
  });

  it('counts the accounts that allow their data, and those with their own provider', () => {
    const s = base({
      accounts: [
        { id: 'a', email: 'a', llm: { ...NONE, data_allowed: true } },
        { id: 'b', email: 'b', llm: { ...NONE, provider: 'http', http_base_url: 'https://x.test/v1', data_allowed: false } },
        // Disabled denies even with the permission set (legacy opt-out).
        { id: 'c', email: 'c', llm: { ...NONE, provider: 'disabled', data_allowed: true } },
      ],
    });
    expect(row(s, 'ai').lines.slice(1)).toEqual(['1 of 3 accounts allow their data.', '1 account uses its own provider.']);
  });

  it('an account whose settings cannot be read is counted as such, not skipped', () => {
    const s = base({
      accounts: [
        { id: 'a', email: 'a', llm: { ...NONE, data_allowed: true } },
        { id: 'b', email: 'b', llm: null },
      ],
    });
    expect(row(s, 'ai').lines.slice(1)).toEqual(['1 of 1 account allows its data.', '1 account cannot be read.']);
  });

  it('speech: the confirmed host, and plain http says so', () => {
    const https = base({ stt: { cfg: { base_url: 'https://api.groq.com/openai/v1', confirmed_host: 'https://api.groq.com' } } });
    expect(row(https, 'speech').lines).toEqual(['Audio goes to api.groq.com']);
    const http = base({ stt: { cfg: { base_url: 'http://10.0.0.5:8000/v1', confirmed_host: 'http://10.0.0.5:8000' } } });
    expect(row(http, 'speech').lines).toEqual(['Audio goes to 10.0.0.5:8000 (not encrypted)']);
  });

  it('links, youtube, TVs', () => {
    const s = base({ links: { auto_follow: ['a', 'b'] }, youtube: true, tvs: [{}] });
    expect(row(s, 'links').lines).toEqual(['On for 2 accounts']);
    expect(row(s, 'youtube').lines).toEqual(["Key saved. Asks Google's YouTube API."]);
    expect(row(s, 'tvs').lines).toEqual(['1 linked. Asks YouTube.']);
  });

  it('backups are plain files, and removing a folder keeps them (E9)', () => {
    const s = base({ backups: [{ id: '1' }, { id: '2' }] });
    expect(row(s, 'backups').lines).toEqual(['2 folders. Plain files, not encrypted.', 'Removing a folder keeps its backups.']);
  });

  it('notes on disk: where, and not encrypted (E8)', () => {
    const s = base({ files: { enabled: true, path: '', default_path: '/Users/me/Jodd for agents' } });
    expect(row(s, 'files').lines).toEqual(['On. Plain Markdown in /Users/me/Jodd for agents, not encrypted.']);
  });

  it('skills and the agent workspace', () => {
    const s = base({ skills: { skills_root: '/s', skills: [{}, {}] }, workspace: { account_id: 'gmail:a@x.com' } });
    expect(row(s, 'skills').lines).toEqual(['2 published to /s']);
    expect(row(s, 'agents').lines).toEqual(['Agent workspace in a@x.com', AGENTS_READ, AGENTS_OWN_RULES]);
    expect(row(base(), 'agents').lines[0]).toBe('Agent workspace not set up');
    // A scope file that cannot be read is Unknown, never "not set up".
    expect(row(base({ workspace: { account_id: null, error: 'not valid JSON' } }), 'agents').lines).toEqual(['Unknown']);
  });
});

describe('platforms: a missing feature is a row, not silence (E15)', () => {
  // Since #318 Windows and macOS check for updates too, on by default.
  it('desktop: the update check is listed, as on Android', () => {
    expect(row(base(), 'updates')).toMatchObject({ lines: ["Once a day. Asks GitHub; sends no notes."], change: 'about' });
    expect(row(base({ updateAutoCheck: false }), 'updates').lines).toEqual(['Off']);
  });

  it('Android: the desktop-only channels are not on this device', () => {
    const s = base({ android: true, files: undefined, skills: undefined, workspace: undefined });
    for (const id of ['files', 'skills', 'agents'] as RowId[]) {
      expect(row(s, id)).toMatchObject({ lines: [NOT_ON_THIS_DEVICE], change: null });
    }
    expect(row(s, 'updates')).toMatchObject({ lines: ["Once a day. Asks GitHub; sends no notes."], change: 'about' });
    expect(row({ ...s, updateAutoCheck: false }, 'updates').lines).toEqual(['Off']);
  });
});

describe('a status that could not be read', () => {
  it('shows Unknown and no Change link, and the other rows survive', () => {
    const s = base({ backups: undefined, stt: undefined });
    expect(row(s, 'backups')).toMatchObject({ lines: ['Unknown'], change: null });
    expect(row(s, 'speech')).toMatchObject({ lines: ['Unknown'], change: null });
    expect(row(s, 'links').lines).toEqual(['Off']);
  });

  it('an unreadable app AI config is Unknown, not "Not set up"', () => {
    const s = base({ app: { cfg: { llm: { ...NONE }, apply_to_accounts: false }, unreadable: 'bad json' } });
    expect(row(s, 'ai').lines[0]).toBe('Unknown');
  });
});
