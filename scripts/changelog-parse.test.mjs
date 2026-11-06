import { describe, it, expect } from 'vitest';
import { parseChangelog, sectionRawText } from './changelog-parse.mjs';

const SAMPLE = `# Changelog

## [Unreleased]
### Added
- work in progress

## [0.17.1] - 2026-06-16
### Added
- Slug links rewrite their displayed text when the target note is renamed.
### Fixed
- Stale link text after rename.

## [0.16.6] - 2026-06-15
### Changed
- Internal cleanup.
`;

describe('parseChangelog', () => {
  it('returns entries newest-first in file order, including Unreleased', () => {
    const entries = parseChangelog(SAMPLE);
    expect(entries.map((e) => e.version)).toEqual(['Unreleased', '0.17.1', '0.16.6']);
  });

  it('parses date and grouped bullet sections', () => {
    const v = parseChangelog(SAMPLE).find((e) => e.version === '0.17.1');
    expect(v.date).toBe('2026-06-16');
    expect(v.sections.Added).toEqual([
      'Slug links rewrite their displayed text when the target note is renamed.',
    ]);
    expect(v.sections.Fixed).toEqual(['Stale link text after rename.']);
  });

  it('Unreleased has a null date', () => {
    const u = parseChangelog(SAMPLE).find((e) => e.version === 'Unreleased');
    expect(u.date).toBeNull();
  });
});

describe('sectionRawText', () => {
  it('returns the markdown body for a version, excluding its header', () => {
    const txt = sectionRawText(SAMPLE, '0.17.1');
    expect(txt).toContain('### Added');
    expect(txt).toContain('- Stale link text after rename.');
    expect(txt).not.toContain('## [0.17.1]');
    expect(txt).not.toContain('## [0.16.6]'); // stops at next version
  });

  it('returns null for a missing version', () => {
    expect(sectionRawText(SAMPLE, '9.9.9')).toBeNull();
  });

  it('returns null for an empty section body (Unreleased with no bullets)', () => {
    const md = '# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - 2026-01-01\n### Added\n- first\n';
    expect(sectionRawText(md, 'Unreleased')).toBeNull();
  });
});

describe('CRLF tolerance', () => {
  it('parses CRLF line endings identically to LF', () => {
    const crlf = SAMPLE.replace(/\n/g, '\r\n');
    const entries = parseChangelog(crlf);
    expect(entries.map((e) => e.version)).toEqual(['Unreleased', '0.17.1', '0.16.6']);
    const v = entries.find((e) => e.version === '0.17.1');
    expect(v.sections.Added).toEqual([
      'Slug links rewrite their displayed text when the target note is renamed.',
    ]);
  });
});
