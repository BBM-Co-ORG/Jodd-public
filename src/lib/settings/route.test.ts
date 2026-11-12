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

describe('settings route: Privacy & Data (spec 2026-11-11 D1)', () => {
  it('takes Privacy’s place in the list', () => {
    expect(SETTINGS_SECTIONS.map((s) => s.id)).toEqual(
      ['appearance', 'accounts', 'backup', 'ai', 'data', 'speech', 'links', 'share', 'advanced', 'about'],
    );
    expect(sectionLabel('data')).toBe('Privacy & Data');
  });

  it('opens Appearance for the old general route (0.48.1)', () => {
    expect(parseSettingsRoute('general')).toEqual({ section: 'appearance', sub: null });
    expect(sectionLabel('appearance')).toBe('Appearance');
    expect(sectionLabel('about')).toBe('About & updates');
  });

  it('opens Privacy & Data for the old privacy route, keeping a sub', () => {
    expect(parseSettingsRoute('privacy')).toEqual({ section: 'data', sub: null });
    expect(parseSettingsRoute('privacy/x')).toEqual({ section: 'data', sub: 'x' });
  });
});
