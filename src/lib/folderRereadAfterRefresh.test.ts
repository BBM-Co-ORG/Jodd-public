// The sidebar's folder tree is a SQLite read (`list_folders`), and the full
// listing (`list_notes`) is what deletes folder rows the remote no longer has.
// scheduleRefresh fired the folder re-read BEFORE running the listing, so the
// read always saw the rows the listing was about to prune, and nothing read
// them again afterwards.
//
// Measured 2026-11-22, Tab S7, SSH account: at 02:02:22 the poll's listing
// logged `reconcile_folders_from_paths: pruned 5 clean folder row(s)`; at
// 02:02:58 the sidebar still showed `Restored…` and `sub1`. Gotcha #6: a change
// the backend makes needs a route back to the pixel.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

function scheduleRefreshBody(): string {
  const src = readFileSync('src/App.svelte', 'utf8');
  const start = src.indexOf('function scheduleRefresh(');
  expect(start).toBeGreaterThan(-1);
  const end = src.indexOf('\n  }\n', start);
  return src.slice(start, end).replace(/\/\/.*$/gm, '');
}

describe('scheduleRefresh re-reads folders after the listing', () => {
  it('requests a folder re-read after the refresh function has finished', () => {
    const body = scheduleRefreshBody();
    const ran = body.indexOf('await fn()');
    expect(ran).toBeGreaterThan(-1);
    expect(body.indexOf('requestFolderRefresh()', ran)).toBeGreaterThan(ran);
  });

  it('re-reads even when the refresh failed', () => {
    const body = scheduleRefreshBody();
    const fin = body.indexOf('finally');
    expect(fin).toBeGreaterThan(-1);
    expect(body.indexOf('requestFolderRefresh()', fin)).toBeGreaterThan(fin);
  });
});
