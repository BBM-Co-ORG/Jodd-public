#!/usr/bin/env node
// Print the CHANGELOG.md body for one version (used by CI to fill release
// notes). Usage: node scripts/changelog-section.mjs 0.17.1
// Exits 1 (with a stderr message) if the version has no section — so a
// release can never go out with empty notes.
import { readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { sectionRawText } from './changelog-parse.mjs';

const version = process.argv[2];
if (!version) {
  console.error('usage: changelog-section.mjs <version>');
  process.exit(2);
}
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const md = readFileSync(resolve(root, 'CHANGELOG.md'), 'utf8');
const body = sectionRawText(md, version);
if (!body) {
  console.error(`changelog-section: no CHANGELOG section for version "${version}"`);
  process.exit(1);
}
process.stdout.write(body + '\n');
