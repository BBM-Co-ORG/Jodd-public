// Copy guards (2026-11-09 workspace review, harden-copy pass).
//
// The error bar used to show the backend's log text: String(e), or
// "Failed to …: ${e}", which offline read as
// "transient: error sending request for url (https://…)". Every message now
// goes through errorText.ts. These scans keep the old shapes from coming
// back, file by file, in every .svelte and .ts the app ships.
import { describe, it, expect } from 'vitest';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

function shipped(dir = 'src'): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return name === 'generated' ? [] : shipped(path);
    return /\.(svelte|ts)$/.test(name) && !/\.test\.ts$/.test(name) && !path.includes('__fixtures__') ? [path] : [];
  });
}

function scan(re: RegExp): string[] {
  const hits: string[] = [];
  for (const path of shipped()) {
    readFileSync(path, 'utf8').split('\n').forEach((line, i) => {
      if (re.test(line)) hits.push(`${path}:${i + 1}  ${line.trim()}`);
    });
  }
  return hits;
}

describe('error messages', () => {
  it('never put a raw error in the bar', () => {
    expect(scan(/error\.set\(\s*String\(/)).toEqual([]);
  });

  it('never end a message with the raw error', () => {
    expect(scan(/error\.set\(`[^`]*\$\{(e|err|error|String\(e\))\}`\)/)).toEqual([]);
  });

  it('count with plural(), never "note(s)"', () => {
    expect(scan(/\b(note|folder|item|message|tag|file|account|link|duplicate|conversation|source|character)\(s\)/i)).toEqual([]);
  });

  it('speak in one voice: "Couldn\'t", not "Failed to"', () => {
    expect(scan(/error\.set\([`'"](Failed to|Could not)/)).toEqual([]);
  });
});

describe('claims', () => {
  // Microsoft folder writes are permanently false (CLAUDE.md, gotcha #12):
  // the menu promised "a later update" that cannot come.
  it('promise no later update', () => {
    expect(scan(/later update|coming soon/i)).toEqual([]);
  });

  it('the disclaimer names every company whose service Jodd signs in to', () => {
    const about = readFileSync('src/lib/components/About.svelte', 'utf8').replace(/\s+/g, ' ');
    for (const name of ['Apple Inc.', 'Google LLC', 'Microsoft Corporation']) expect(about).toContain(name);
  });
});
