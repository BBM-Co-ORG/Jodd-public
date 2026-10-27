import { describe, it, expect } from 'vitest';
import { parseSettingsRoute, SETTINGS_SECTIONS, sectionLabel } from './route';

describe('settings route: backup', () => {
  it('parses backup', () => {
    expect(parseSettingsRoute('backup').section).toBe('backup');
    expect(sectionLabel('backup')).toBe('Backup');
  });

  it('lists Backup right after Accounts', () => {
    const ids = SETTINGS_SECTIONS.map((s) => s.id);
    expect(ids[ids.indexOf('accounts') + 1]).toBe('backup');
  });
});
