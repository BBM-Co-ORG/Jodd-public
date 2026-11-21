import { describe, it, expect } from 'vitest';
import { readFileSync, statSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildManifest, checkManifest, signedVersion, ASSET_KEYS, DOWNLOAD_PREFIX } from './updater-manifest.mjs';

const FIX = resolve(dirname(fileURLToPath(import.meta.url)), 'fixtures/updater');
const NAMES = {
  'windows-x86_64-nsis': 'Jodd_0.47.0_x64-setup.exe',
  'windows-x86_64-msi': 'Jodd_0.47.0_x64_en-US.msi',
  'darwin-aarch64-app': 'Jodd_aarch64.app.tar.gz',
};
const sig = (name) => readFileSync(resolve(FIX, `${name}.sig`), 'utf8').trim();
const size = (name) => statSync(resolve(FIX, name)).size;

function assets() {
  return Object.entries(NAMES).map(([key, name]) => ({ key, name, size: size(name), signature: sig(name) }));
}
function build(tag = 'v0.47.0') {
  return buildManifest({ version: '0.47.0', tag, notes: 'Notes.', pubDate: '2026-11-11T00:00:00.000Z', assets: assets() });
}
function ctx(tag = 'v0.47.0') {
  return {
    version: '0.47.0',
    tag,
    draftAssets: Object.values(NAMES).map((name) => ({ name, size: size(name) })).concat([{ name: 'Jodd_0.47.0_aarch64.dmg', size: 9 }]),
    signatures: Object.fromEntries(Object.values(NAMES).map((n) => [n, sig(n)])),
  };
}

describe('buildManifest', () => {
  it('builds the three-platform manifest from fixtures', () => {
    const m = build();
    expect(m.version).toBe('0.47.0');
    expect(m.pub_date).toBe('2026-11-11T00:00:00.000Z');
    expect(m.notes).toBe('Notes.');
    expect(Object.keys(m.platforms).sort()).toEqual([...ASSET_KEYS].sort());
    expect(m.platforms['windows-x86_64-nsis']).toEqual({
      url: 'https://github.com/BBM-Co-ORG/Jodd-public/releases/download/v0.47.0/Jodd_0.47.0_x64-setup.exe',
      signature: sig('Jodd_0.47.0_x64-setup.exe'),
      size: size('Jodd_0.47.0_x64-setup.exe'),
    });
    expect(checkManifest(m, ctx())).toEqual([]);
  });

  it('a dry-run tag puts its own tag in every URL', () => {
    const m = build('v0.47.0-dryrun.12');
    expect(m.version).toBe('0.47.0');
    for (const p of Object.values(m.platforms)) expect(p.url).toContain('/download/v0.47.0-dryrun.12/');
    expect(checkManifest(m, ctx('v0.47.0-dryrun.12'))).toEqual([]);
  });

  it('refuses a key it does not know', () => {
    expect(() => buildManifest({ version: '0.47.0', tag: 'v0.47.0', notes: '', pubDate: 'x',
      assets: [{ key: 'windows-x86_64', name: 'a', size: 1, signature: 's' }] })).toThrow(/windows-x86_64/);
  });
});

describe('signedVersion', () => {
  it('reads the version from the signed trusted comment', () => {
    expect(signedVersion(sig('Jodd_0.47.0_x64-setup.exe'))).toBe('0.47.0');
    expect(signedVersion(sig('other-version.bin'))).toBe('0.46.0');
    expect(signedVersion('not base64 at all')).toBeNull();
  });
});

describe('checkManifest refuses', () => {
  const defects = (mutate, c = ctx()) => {
    const m = structuredClone(build());
    mutate(m, c);
    return checkManifest(m, c).join('\n');
  };

  it('another version', () => {
    expect(defects((m) => { m.version = '0.48.0'; })).toMatch(/version/);
  });
  it('a bare windows-x86_64 key', () => {
    expect(defects((m) => { m.platforms['windows-x86_64'] = m.platforms['windows-x86_64-nsis']; })).toMatch(/windows-x86_64\b.*not allowed|not allowed.*windows-x86_64/);
  });
  it('a key outside the three', () => {
    expect(defects((m) => { m.platforms['linux-x86_64'] = m.platforms['windows-x86_64-nsis']; })).toMatch(/linux-x86_64/);
  });
  it('a missing key', () => {
    expect(defects((m) => { delete m.platforms['darwin-aarch64-app']; })).toMatch(/darwin-aarch64-app/);
  });
  it('a URL outside Jodd-public for this tag', () => {
    expect(defects((m) => { m.platforms['windows-x86_64-nsis'].url = 'https://example.com/Jodd_0.47.0_x64-setup.exe'; })).toMatch(/url/i);
    expect(defects((m) => { m.platforms['windows-x86_64-nsis'].url = `${DOWNLOAD_PREFIX}v0.46.0/Jodd_0.47.0_x64-setup.exe`; })).toMatch(/url/i);
  });
  it('a file the draft does not have', () => {
    expect(defects((m, c) => { c.draftAssets = c.draftAssets.filter((a) => a.name !== 'Jodd_0.47.0_x64_en-US.msi'); })).toMatch(/Jodd_0\.47\.0_x64_en-US\.msi/);
  });
  it('a size that is not the draft asset size', () => {
    expect(defects((m) => { m.platforms['windows-x86_64-msi'].size += 1; })).toMatch(/size/);
  });
  it('a signature that is not the one just made', () => {
    expect(defects((m) => { m.platforms['darwin-aarch64-app'].signature = sig('Jodd_0.47.0_x64-setup.exe'); })).toMatch(/signature/);
  });
  it('a signature that names another version', () => {
    expect(defects((m, c) => {
      m.platforms['windows-x86_64-nsis'].signature = sig('other-version.bin');
      c.signatures['Jodd_0.47.0_x64-setup.exe'] = sig('other-version.bin');
    })).toMatch(/0\.46\.0/);
  });
});
