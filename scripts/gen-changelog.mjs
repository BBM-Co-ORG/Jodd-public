// Build hook: parse CHANGELOG.md → src/lib/generated/changelog.json so the
// frontend can bundle release notes (offline, version-matched). Run via the
// npm predev/prebuild hooks. Never hand-edit the output.
import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseChangelog } from './changelog-parse.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const srcPath = resolve(root, 'CHANGELOG.md');
const outPath = resolve(root, 'src/lib/generated/changelog.json');

if (!existsSync(srcPath)) {
  console.error(`gen-changelog: CHANGELOG.md not found at ${srcPath}`);
  process.exit(1);
}
const md = readFileSync(srcPath, 'utf8');
const entries = parseChangelog(md);
mkdirSync(dirname(outPath), { recursive: true });
writeFileSync(outPath, JSON.stringify(entries, null, 2) + '\n', 'utf8');
console.log(`gen-changelog: wrote ${entries.length} entries → ${outPath}`);
