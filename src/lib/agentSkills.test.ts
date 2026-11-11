import { describe, expect, it } from 'vitest';
import { skillForFolder, skillStateLine, suggestSkillName, validSkillName, SKILL_NAME_MAX, type SkillRow, type SkillsView } from './agentSkills';

const row = (over: Partial<SkillRow> = {}): SkillRow => ({
  name: 'brand-bbmedia',
  account_id: 'gmail:a@x.com',
  folder: 'Notes/Brand BBMedia',
  assets_dir: null,
  only_reviewed: false,
  email: 'a@x.com',
  status: null,
  ...over,
});

describe('validSkillName', () => {
  it('matches the Rust rule', () => {
    for (const ok of ['brand', 'brand-bbmedia', 'a1-b2']) expect(validSkillName(ok)).toBe(true);
    for (const bad of ['', 'Brand', 'brand_x', '-brand', 'brand-', 'a--b', 'แบรนด์', 'a'.repeat(SKILL_NAME_MAX + 1)]) {
      expect(validSkillName(bad)).toBe(false);
    }
  });
});

describe('suggestSkillName', () => {
  it('slugs the folder leaf, and gives up on a name with no ASCII letters', () => {
    expect(suggestSkillName('Notes/BBMedia/Brand Guide')).toBe('brand-guide');
    expect(suggestSkillName('Notes/Café Menü')).toBe('cafe-menu');
    expect(suggestSkillName('Notes/แบรนด์')).toBe('');
    expect(validSkillName(suggestSkillName(`Notes/${'Long '.repeat(30)}`))).toBe(true);
  });
});

describe('skillStateLine', () => {
  it('says what the last run did, including notes waiting for review', () => {
    expect(skillStateLine(row())).toMatch(/next sync/);
    const published = row({ status: { name: 'brand-bbmedia', account_id: 'gmail:a@x.com', folder: 'Notes/Brand BBMedia', state: 'published', waiting_for_review: ['Logo rules'], problems: [], files: 4 } });
    expect(skillStateLine(published)).toBe('Published as jodd-brand-bbmedia (4 files). 1 note waits for Mark reviewed.');
    expect(skillStateLine(row({ status: { ...published.status!, state: 'removed', waiting_for_review: [] } }))).toMatch(/removed/);
  });
});

describe('skillForFolder', () => {
  it('finds the skill by account and folder', () => {
    const view: SkillsView = { skills_root: '/h/.claude/skills', skills: [row()], last_finished_at: null, problems: [], error: null };
    expect(skillForFolder(view, 'gmail:a@x.com', 'Notes/Brand BBMedia')?.name).toBe('brand-bbmedia');
    expect(skillForFolder(view, 'gmail:b@x.com', 'Notes/Brand BBMedia')).toBeNull();
    expect(skillForFolder(null, 'gmail:a@x.com', 'Notes/Brand BBMedia')).toBeNull();
  });
});
