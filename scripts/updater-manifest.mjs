#!/usr/bin/env node
// Build and check the updater's latest.json (spec 2026-11-11-app-updater-design.md §7.3).
// Run by release-sign.yml, after it has signed the desktop installers:
//
//   node scripts/updater-manifest.mjs build --version 0.47.0 --tag v0.47.0 \
//        --changelog CHANGELOG.md --asset windows-x86_64-nsis=desktop/Jodd_0.47.0_x64-setup.exe ... > latest.json
//   node scripts/updater-manifest.mjs check latest.json --version 0.47.0 --tag v0.47.0 \
//        --assets-json draft-assets.json --sig-dir desktop
//
// `build` reads each file's size and its `<file>.sig` (what `tauri signer sign`
// writes). `check` exits 1 with one line per defect. It does not verify the
// signatures cryptographically — release-sign.yml runs `minisign -V` for that —
// but it does refuse a manifest that would not match the draft it goes into.

import { readFileSync, statSync } from 'node:fs';
import { basename } from 'node:path';
import { userNotes } from './changelog-parse.mjs';

export const DOWNLOAD_PREFIX = 'https://github.com/BBM-Co-ORG/Jodd-public/releases/download/';

/**
 * The only platform keys latest.json may carry. Deliberately no bare
 * `windows-x86_64`: the updater falls back to it when it cannot tell how Jodd
 * was installed, and running the NSIS installer over an MSI install (or the
 * reverse) is worse than offering the release page (spec §5.2).
 */
export const ASSET_KEYS = ['windows-x86_64-nsis', 'windows-x86_64-msi', 'darwin-aarch64-app'];

/** The asset each key must point at, as release.yml's tauri-action names them. */
export function assetNameFor(key, version) {
  switch (key) {
    case 'windows-x86_64-nsis': return `Jodd_${version}_x64-setup.exe`;
    case 'windows-x86_64-msi': return `Jodd_${version}_x64_en-US.msi`;
    case 'darwin-aarch64-app': return 'Jodd_aarch64.app.tar.gz';
    default: return null;
  }
}

/**
 * The version a Tauri signature was bound to (`tauri signer sign
 * --app-version`), from its trusted comment. null when there is none. Read
 * here only to catch a mistake; minisign -V is what authenticates it.
 */
export function signedVersion(signature) {
  let text;
  try {
    text = Buffer.from(signature, 'base64').toString('utf8');
  } catch {
    return null;
  }
  const line = text.split('\n').find((l) => l.startsWith('trusted comment: '));
  if (!line) return null;
  const field = line.slice('trusted comment: '.length).split('\t').find((f) => f.startsWith('version:'));
  return field ? field.slice('version:'.length) : null;
}

/** @param {{version: string, tag: string, notes: string, pubDate: string, assets: {key: string, name: string, size: number, signature: string}[]}} o */
export function buildManifest({ version, tag, notes, pubDate, assets }) {
  const platforms = {};
  for (const a of assets) {
    if (!ASSET_KEYS.includes(a.key)) throw new Error(`unknown platform key ${a.key}; expected one of ${ASSET_KEYS.join(', ')}`);
    platforms[a.key] = { url: `${DOWNLOAD_PREFIX}${tag}/${a.name}`, signature: a.signature, size: a.size };
  }
  return { version, notes, pub_date: pubDate, platforms };
}

/**
 * Every way `manifest` would not match the release it goes into.
 * @param {{version: string, tag: string, draftAssets: {name: string, size: number}[], signatures: Record<string, string>}} c
 * @returns {string[]}
 */
export function checkManifest(manifest, { version, tag, draftAssets, signatures }) {
  const out = [];
  if (manifest.version !== version) out.push(`version is ${JSON.stringify(manifest.version)}, not ${version}`);
  const platforms = manifest.platforms ?? {};
  for (const key of Object.keys(platforms)) {
    if (!ASSET_KEYS.includes(key)) out.push(`platform key ${key} is not allowed (only ${ASSET_KEYS.join(', ')})`);
  }
  for (const key of ASSET_KEYS) {
    const p = platforms[key];
    if (!p) { out.push(`platform ${key} is missing`); continue; }
    const name = assetNameFor(key, version);
    const want = `${DOWNLOAD_PREFIX}${tag}/${name}`;
    if (p.url !== want) out.push(`${key}: url is ${p.url}, expected ${want}`);
    const asset = draftAssets.find((a) => a.name === name);
    if (!asset) out.push(`${key}: the draft has no asset ${name}`);
    else if (p.size !== asset.size) out.push(`${key}: size ${p.size} is not the draft asset's size ${asset.size}`);
    if (p.signature !== signatures[name]) out.push(`${key}: signature is not the one just made for ${name}`);
    const signed = signedVersion(p.signature ?? '');
    if (signed !== version) out.push(`${key}: signature is bound to version ${signed ?? '(none)'}, not ${version}`);
  }
  return out;
}

// ─── CLI ─────────────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const opts = { asset: [] };
  const positional = [];
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith('--')) { positional.push(a); continue; }
    const key = a.slice(2);
    const val = argv[++i];
    if (val === undefined) throw new Error(`${a} needs a value`);
    if (key === 'asset') opts.asset.push(val); else opts[key] = val;
  }
  return { positional, opts };
}

function need(opts, ...keys) {
  for (const k of keys) if (!opts[k]) throw new Error(`--${k} is required`);
}

function main(argv) {
  const [cmd, ...rest] = argv;
  const { positional, opts } = parseArgs(rest);
  if (cmd === 'build') {
    need(opts, 'version', 'tag', 'changelog');
    const notes = userNotes(readFileSync(opts.changelog, 'utf8'), opts.version);
    if (!notes) throw new Error(`CHANGELOG has no user-facing section for ${opts.version}`);
    const assets = opts.asset.map((spec) => {
      const eq = spec.indexOf('=');
      if (eq < 1) throw new Error(`--asset must be key=path (got ${spec})`);
      const key = spec.slice(0, eq);
      const path = spec.slice(eq + 1);
      return { key, name: basename(path), size: statSync(path).size, signature: readFileSync(`${path}.sig`, 'utf8').trim() };
    });
    const m = buildManifest({ version: opts.version, tag: opts.tag, notes, pubDate: opts['pub-date'] ?? new Date().toISOString(), assets });
    process.stdout.write(JSON.stringify(m, null, 2) + '\n');
    return 0;
  }
  if (cmd === 'check') {
    need(opts, 'version', 'tag', 'assets-json', 'sig-dir');
    if (positional.length !== 1) throw new Error('check takes one manifest path');
    const manifest = JSON.parse(readFileSync(positional[0], 'utf8'));
    // `gh release view --json assets` → {assets: [{name, size, ...}]}
    const draft = JSON.parse(readFileSync(opts['assets-json'], 'utf8'));
    const draftAssets = (draft.assets ?? draft).map((a) => ({ name: a.name, size: a.size }));
    const signatures = {};
    for (const key of ASSET_KEYS) {
      const name = assetNameFor(key, opts.version);
      try { signatures[name] = readFileSync(`${opts['sig-dir']}/${name}.sig`, 'utf8').trim(); } catch { /* reported as a mismatch */ }
    }
    const defects = checkManifest(manifest, { version: opts.version, tag: opts.tag, draftAssets, signatures });
    for (const d of defects) console.error(`latest.json: ${d}`);
    if (defects.length) return 1;
    console.log(`OK: latest.json matches draft ${opts.tag} (${ASSET_KEYS.length} platforms, version ${opts.version}).`);
    return 0;
  }
  console.error('usage: updater-manifest.mjs build|check …  (see the header of this file)');
  return 2;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    process.exit(main(process.argv.slice(2)));
  } catch (e) {
    console.error(`updater-manifest: ${e.message}`);
    process.exit(2);
  }
}
