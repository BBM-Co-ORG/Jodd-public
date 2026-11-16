import { describe, expect, it } from 'vitest';
import { HOOK_SNIPPET } from './agentWorkspace';
import { heldBack, hookSnippet, permissionSnippet, summarize, withAccount, type MirrorStatus } from './agentFiles';

const status = (over: Partial<MirrorStatus> = {}): MirrorStatus => ({
  finished_at: '2026-11-05T09:12:03+07:00',
  outcome: 'ok',
  path: '/Users/k/Jodd for agents',
  accounts: [
    { account_id: 'gmail:a@x.com', notes: 800, written: 3, deleted: 0, masked_notes: 1, skipped_attachments: 0, overwritten_local_edits: 0, held_back: false },
    { account_id: 'icloud:a@x.com', notes: 12, written: 1, deleted: 0, masked_notes: 0, skipped_attachments: 0, overwritten_local_edits: 0, held_back: false },
  ],
  problems: [],
  ...over,
});

describe('hookSnippet', () => {
  it('is valid JSON and runs the sync before the briefing', () => {
    const hooks = JSON.parse(hookSnippet(true)).hooks.SessionStart[0].hooks;
    expect(hooks.map((h: { command: string }) => h.command)).toEqual([
      'jodd-mcp sync-agent-files --quiet',
      'jodd-mcp brief --cwd "$CLAUDE_PROJECT_DIR"',
    ]);
  });

  it('without the mirror is the briefing-only hook, same commands as before', () => {
    expect(JSON.parse(hookSnippet(false))).toEqual(JSON.parse(HOOK_SNIPPET));
  });
});

describe('permissionSnippet', () => {
  it('names the mirror folder exactly, spaces included', () => {
    expect(JSON.parse(permissionSnippet('/Users/k/Jodd for agents'))).toEqual({
      permissions: { additionalDirectories: ['/Users/k/Jodd for agents'] },
    });
  });
});

describe('withAccount', () => {
  it('adds once, removes, and never mutates its input', () => {
    const before = ['gmail:a@x.com'];
    expect(withAccount(before, 'icloud:a@x.com', true)).toEqual(['gmail:a@x.com', 'icloud:a@x.com']);
    expect(withAccount(before, 'gmail:a@x.com', true)).toBe(before);
    expect(withAccount(before, 'gmail:a@x.com', false)).toEqual([]);
    expect(before).toEqual(['gmail:a@x.com']);
  });
});

describe('summarize', () => {
  it('counts notes across accounts', () => {
    expect(summarize(status())).toMatch(/^812 notes on disk, 4 updated in the last sync/);
  });

  it('says when a run was cut short or failed, and is empty with no run', () => {
    expect(summarize(status({ outcome: 'partial' }))).toContain('next sync continues');
    expect(summarize(status({ outcome: 'failed' }))).toMatch(/^Last sync failed/);
    expect(summarize(null)).toBe('');
  });
});

describe('heldBack', () => {
  it('is true when any account held deletions back', () => {
    expect(heldBack(status())).toBe(false);
    const s = status();
    s.accounts[1].held_back = true;
    expect(heldBack(s)).toBe(true);
    expect(heldBack(null)).toBe(false);
  });
});
