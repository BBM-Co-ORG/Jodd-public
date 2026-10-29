import assert from 'node:assert/strict';
const { chromium } = await import(process.env.JODD_PLAYWRIGHT || 'playwright');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', e => errors.push(e.message));
await page.route('**/*', r => new URL(r.request().url()).hostname === '127.0.0.1' ? r.continue() : r.abort());
try {
  await page.goto('http://127.0.0.1:1420/tests/browser/full-app.html');
  await page.locator('.note-btn').filter({hasText:'Account A notebook'}).first().click();
  const status = page.locator('.save-status');
  // The toolbar shows the short save state; the full sentence is its title (Task 5.2, spec §6.1).
  await page.waitForFunction(() => (document.querySelector('.save-status')?.getAttribute('title')?.includes('Synced') && document.querySelector('.save-status')?.textContent.trim() === 'Saved'));
  await page.evaluate(() => {
    window.showcase.setDirty('a', true);
    window.showcase.emit('note-persistence-changed', {accountId:'demo-a', uuid:'shared-uuid'});
  });
  await page.waitForFunction(() => (document.querySelector('.save-status')?.getAttribute('title')?.includes('Sync pending') && document.querySelector('.save-status')?.textContent.trim() === 'Saved'));
  // Identical UUID on B must not claim that A has synced.
  await page.evaluate(() => window.showcase.emit('note-persistence-changed', {accountId:'demo-b', uuid:'shared-uuid'}));
  await page.waitForFunction(() => window.showcase.calls.some(c => c.command === 'note_persistence' && c.args.accountId === 'demo-b'));
  assert.match(await status.getAttribute('title'), /Sync pending/);
  assert.equal((await status.textContent()).trim(), 'Saved');
  await page.evaluate(() => {
    window.showcase.setDirty('a', false);
    window.showcase.emit('note-persistence-changed', {accountId:'demo-a', uuid:'shared-uuid'});
  });
  // The toolbar shows the short save state; the full sentence is its title (Task 5.2, spec §6.1).
  await page.waitForFunction(() => (document.querySelector('.save-status')?.getAttribute('title')?.includes('Synced') && document.querySelector('.save-status')?.textContent.trim() === 'Saved'));
  for (const width of [360, 800]) for (const theme of ['light','dark']) {
    await page.setViewportSize({width,height:850});
    await page.evaluate(t => document.documentElement.dataset.theme=t,theme);
    await page.screenshot({path:`/tmp/jodd-g-${width}-${theme}.png`});
  }
  assert.deepEqual(errors,[]);
  console.log(`PASS Chrome ${browser.version()}: production App event -> local persistence IPC -> account-qualified rendered status; synthetic IPC only`);
} finally { await browser.close(); }
