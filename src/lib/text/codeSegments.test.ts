import { describe, it, expect } from 'vitest';
import { codeSegments } from './codeSegments';

describe('codeSegments', () => {
  it('marks backtick-quoted spans as code', () => {
    expect(codeSegments('Run `sudo jodd-ai-host enable codex`.')).toEqual([
      { text: 'Run ', code: false },
      { text: 'sudo jodd-ai-host enable codex', code: true },
      { text: '.', code: false },
    ]);
  });

  it('keeps text without backticks as one plain segment', () => {
    expect(codeSegments('plain <b>text</b>')).toEqual([{ text: 'plain <b>text</b>', code: false }]);
  });
});
