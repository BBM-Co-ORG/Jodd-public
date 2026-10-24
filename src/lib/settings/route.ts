// The Settings route (stores/ui.ts `settingsRoute`): `section` or
// `section/sub`. The sub is everything after the FIRST slash, so an account
// id (which may itself hold a slash in a vault name) survives intact.

export type SettingsSection = 'general' | 'accounts' | 'backup' | 'ai' | 'privacy' | 'speech' | 'share' | 'advanced';

/** In display order (spec §4.3 table). */
export const SETTINGS_SECTIONS: { id: SettingsSection; label: string }[] = [
  { id: 'general', label: 'General' },
  { id: 'accounts', label: 'Accounts' },
  { id: 'backup', label: 'Backup' },
  { id: 'ai', label: 'AI' },
  { id: 'privacy', label: 'Privacy' },
  { id: 'speech', label: 'Speech to text' },
  { id: 'share', label: 'Share to Jodd' },
  { id: 'advanced', label: 'Advanced' },
];

export function sectionLabel(id: SettingsSection): string {
  return SETTINGS_SECTIONS.find((s) => s.id === id)?.label ?? id;
}

/**
 * `section` is null for the section list (`''`) and for an unknown section;
 * `sub` is null when absent or empty.
 */
export function parseSettingsRoute(route: string): { section: SettingsSection | null; sub: string | null } {
  const slash = route.indexOf('/');
  const head = slash < 0 ? route : route.slice(0, slash);
  const rest = slash < 0 ? '' : route.slice(slash + 1);
  const section = SETTINGS_SECTIONS.some((s) => s.id === head) ? (head as SettingsSection) : null;
  return { section, sub: section && rest ? rest : null };
}
