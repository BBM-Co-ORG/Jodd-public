import { describe, it, expect } from 'vitest';
import { formFingerprint, hasUnsavedEdits } from './llmFormDirty';

const claude = { provider: 'agent_cli', agent_preset: 'claude' };
const codex = { provider: 'agent_cli', agent_preset: 'codex' };

describe('formFingerprint', () => {
  it('differs when the saved payload differs', () => {
    expect(formFingerprint(claude, false)).not.toBe(formFingerprint(codex, false));
  });

  it('matches for an identical payload', () => {
    expect(formFingerprint(claude, false)).toBe(formFingerprint({ ...claude }, false));
  });

  /**
   * apply_to_accounts is saved by the same button and changes what the
   * configuration DOES — an inheriting account starts using this provider for
   * Extract. Leaving it out would call a real pending change "saved".
   */
  it('covers apply_to_accounts, which Save persists alongside the provider', () => {
    expect(formFingerprint(claude, true)).not.toBe(formFingerprint(claude, false));
  });
});

describe('hasUnsavedEdits', () => {
  it('reports none when the form matches what was saved', () => {
    const f = formFingerprint(claude, false);
    expect(hasUnsavedEdits(f, f, '')).toBe(false);
  });

  it('reports an edit when the picked provider differs from the saved one', () => {
    const saved = formFingerprint(claude, false);
    const now = formFingerprint(codex, false);
    expect(hasUnsavedEdits(saved, now, '')).toBe(true);
  });

  /**
   * A typed key can never show up in the fingerprint: the payload carries only
   * the keychain ENTRY NAME, and the secret itself is cleared from memory the
   * moment Save succeeds. So a key sitting in the box is, by construction,
   * unsaved — and it is the one edit most likely to decide whether the probe
   * passes.
   */
  it('treats a typed API key as an unsaved edit', () => {
    const f = formFingerprint(claude, false);
    expect(hasUnsavedEdits(f, f, 'sk-typed-but-not-saved')).toBe(true);
  });

  /**
   * Between mount and the first load there is nothing to compare against.
   * Guessing "dirty" there would disable Test on a form the user has not
   * touched, which is the same unexplained dead end from the other direction.
   */
  it('reports none before the saved configuration has loaded', () => {
    expect(hasUnsavedEdits(null, formFingerprint(claude, false), '')).toBe(false);
  });
});
