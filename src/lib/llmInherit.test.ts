import { describe, it, expect } from 'vitest';
import { describeProvider, inheritedProvider } from './llmInherit';

const PRESETS = [
  { id: 'claude', label: 'Claude Code' },
  { id: 'codex', label: 'Codex CLI' },
];

const base = {
  provider: 'none' as const,
  http_base_url: null,
  http_model: null,
  agent_preset: null,
  agent_host: null,
};

describe('describeProvider', () => {
  it('names an agent CLI by its preset label and where it runs', () => {
    expect(describeProvider({ ...base, provider: 'agent_cli', agent_preset: 'codex' }, PRESETS))
      .toBe('Codex CLI on this computer');
    expect(describeProvider(
      { ...base, provider: 'agent_cli', agent_preset: 'codex', agent_host: { target: 'jodd@jodd-zf', port: null } },
      PRESETS,
    )).toBe('Codex CLI on jodd-zf');
  });

  it('falls back to the preset id, and to "an agent CLI" with no preset', () => {
    expect(describeProvider({ ...base, provider: 'agent_cli', agent_preset: 'aider' }, PRESETS))
      .toBe('aider on this computer');
    expect(describeProvider({ ...base, provider: 'agent_cli' }, PRESETS)).toBe('an agent CLI on this computer');
  });

  it('reads the legacy claude_code spelling as Claude Code', () => {
    expect(describeProvider({ ...base, provider: 'claude_code' }, PRESETS)).toBe('Claude Code on this computer');
  });

  it('names a custom endpoint by model and host', () => {
    expect(describeProvider(
      { ...base, provider: 'http', http_base_url: 'https://api.openai.com/v1', http_model: 'gpt-4o-mini' },
      PRESETS,
    )).toBe('gpt-4o-mini at api.openai.com');
    expect(describeProvider({ ...base, provider: 'http', http_base_url: 'not a url' }, PRESETS))
      .toBe('a custom endpoint');
  });

  it('is null when there is no provider', () => {
    expect(describeProvider({ ...base }, PRESETS)).toBeNull();
    expect(describeProvider({ ...base, provider: 'disabled' }, PRESETS)).toBeNull();
  });
});

describe('inheritedProvider', () => {
  const codexOnServer = {
    ...base, provider: 'agent_cli' as const, agent_preset: 'codex', agent_host: { target: 'jodd-zf', port: null },
  };

  it('shared and set up: names what the account gets', () => {
    expect(inheritedProvider({ llm: codexOnServer, apply_to_accounts: true }, PRESETS))
      .toEqual({ name: 'Codex CLI on jodd-zf', noAi: false });
  });

  it('not shared: the account gets no AI, whatever the app has', () => {
    expect(inheritedProvider({ llm: codexOnServer, apply_to_accounts: false }, PRESETS))
      .toEqual({ name: null, noAi: true, reason: 'not_shared' });
  });

  it('nothing set up in the app: no AI', () => {
    expect(inheritedProvider({ llm: { ...base }, apply_to_accounts: true }, PRESETS))
      .toEqual({ name: null, noAi: true, reason: 'not_configured' });
  });

  it('unknown (the app config could not be read): says nothing it cannot back', () => {
    expect(inheritedProvider(null, PRESETS)).toBeNull();
  });
});
