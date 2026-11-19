// The one rule every AI entry point uses to decide whether it shows and
// whether it can be used (spec 2026-10-28 §4).
import { describe, it, expect } from 'vitest';
import { aiGate, NO_FOLDER_HINT } from './aiActions';
import { AI_HIDDEN_HINT } from '../stores/aiScope';

describe('aiGate', () => {
  it.each([
    // read-only actions (Ask): never hidden, only disabled
    [{ hidden: false }, { show: true, disabled: false, reason: '' }],
    [{ hidden: true }, { show: true, disabled: true, reason: AI_HIDDEN_HINT }],
    // write actions: hidden when the account cannot write, whatever else holds
    [{ canWrite: true, hidden: false }, { show: true, disabled: false, reason: '' }],
    [{ canWrite: false, hidden: false }, { show: false, disabled: true, reason: '' }],
    [{ canWrite: false, hidden: true }, { show: false, disabled: true, reason: '' }],
    [{ canWrite: true, hidden: true }, { show: true, disabled: true, reason: AI_HIDDEN_HINT }],
    // actions that need a selected folder
    [{ canWrite: true, hidden: false, hasFolder: false }, { show: true, disabled: true, reason: NO_FOLDER_HINT }],
    // hidden from AI is the reason given first: selecting a folder would not fix it
    [{ canWrite: true, hidden: true, hasFolder: false }, { show: true, disabled: true, reason: AI_HIDDEN_HINT }],
  ])('%o → %o', (input, expected) => {
    expect(aiGate(input)).toEqual(expected);
  });
});
