// What an account set to "Use app default" actually gets. Mirrors the
// cascade in src-tauri/src/llm/resolve.rs (`effective_config`): an account
// whose provider is `none` inherits the app provider only when the app's
// `apply_to_accounts` is on; otherwise it has no provider at all.

import { hostLabel } from './cliPrivacy';

type ProviderKind = 'none' | 'disabled' | 'claude_code' | 'http' | 'agent_cli';

export type DescribableLlm = {
  provider: ProviderKind;
  http_base_url: string | null;
  http_model: string | null;
  agent_preset: string | null;
  agent_host?: { target: string; port: number | null } | null;
};

/** A short name for a provider config, or null when it is no provider. */
export function describeProvider(
  llm: DescribableLlm,
  presets: { id: string; label: string }[],
): string | null {
  switch (llm.provider) {
    case 'none':
    case 'disabled':
      return null;
    case 'claude_code':
    case 'agent_cli': {
      // `claude_code` is the pre-v0.19 spelling of the claude preset.
      const id = llm.provider === 'claude_code' ? 'claude' : llm.agent_preset;
      const name = id ? (presets.find((p) => p.id === id)?.label ?? id) : 'an agent CLI';
      return `${name} on ${llm.agent_host ? hostLabel(llm.agent_host) : 'this computer'}`;
    }
    case 'http': {
      let host: string | null = null;
      try {
        host = llm.http_base_url ? new URL(llm.http_base_url).host : null;
      } catch {
        host = null;
      }
      if (!host) return 'a custom endpoint';
      return llm.http_model ? `${llm.http_model} at ${host}` : `a custom endpoint at ${host}`;
    }
  }
}

export type Inherited =
  | { name: string; noAi: false }
  | { name: null; noAi: true; reason: 'not_shared' | 'not_configured' };

/**
 * What "Use app default" resolves to. Null when the app config is unknown
 * (it could not be read): the form then says nothing it cannot back.
 */
export function inheritedProvider(
  app: { llm: DescribableLlm; apply_to_accounts: boolean } | null,
  presets: { id: string; label: string }[],
): Inherited | null {
  if (!app) return null;
  const name = describeProvider(app.llm, presets);
  if (!name) return { name: null, noAi: true, reason: 'not_configured' };
  if (!app.apply_to_accounts) return { name: null, noAi: true, reason: 'not_shared' };
  return { name, noAi: false };
}
