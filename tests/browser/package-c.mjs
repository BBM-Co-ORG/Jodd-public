import assert from 'node:assert/strict';
const { chromium } = await import(process.env.JODD_PLAYWRIGHT || 'playwright');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage(); const errors = [];
page.on('pageerror', e => errors.push(e.message));
await page.route('**/*', r => new URL(r.request().url()).hostname === '127.0.0.1' ? r.continue() : r.abort());
const status = page.locator('.save-status');
const waitStatus = text => page.waitForFunction(t => document.querySelector('.save-status')?.textContent.includes(t), text);
try {
  await page.goto('http://127.0.0.1:1420/tests/browser/package-c.html');
  await waitStatus('Sync pending');
  await page.locator('.editor-body').fill('edited offline');
  await waitStatus('Sync pending');
  let snapshot = await page.evaluate(() => window.packageCFixture.snapshot());
  assert.equal(snapshot.selected.local_version, 2);
  await page.evaluate(() => window.packageCFixture.push(1)); await waitStatus('Sync pending');
  await page.evaluate(() => window.packageCFixture.block()); await waitStatus('Sync blocked');
  await page.evaluate(() => window.packageCFixture.unblock());
  await page.evaluate(() => window.packageCFixture.push(2, 'assigned')); await waitStatus('Synced');
  snapshot = await page.evaluate(() => window.packageCFixture.snapshot());
  assert.equal(snapshot.selected.uuid, 'assigned');
  assert.equal(snapshot.notes.find(n => n.account_id === 'synthetic-b').uuid, 'same');
  await page.locator('.editor-body').fill('unsaved during push');
  await page.evaluate(() => window.packageCFixture.push(2));
  assert.equal(await page.locator('.editor-body').innerText(), 'unsaved during push');
  assert.match(await status.textContent(), /Unsaved|Saving/);
  await page.evaluate(() => window.packageCFixture.select('synthetic-b'));
  await page.waitForFunction(() => document.querySelector('.editor-body')?.textContent === 'ข้อความ B');
  await waitStatus('Synced');
  await page.evaluate(() => window.packageCFixture.select('synthetic-a')); await waitStatus('Sync pending');
  for (const width of [800, 360]) for (const theme of ['light', 'dark']) {
    await page.setViewportSize({ width, height: 850 });
    await page.evaluate(t => document.documentElement.dataset.theme = t, theme);
    const bounds = await status.boundingBox(); assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= width);
    await page.screenshot({ path: `/tmp/jodd-package-c-${width}-${theme}.png` });
  }
  assert.deepEqual(errors, []);
  console.log(`PASS Chrome ${browser.version()}: synthetic offline save, stale push, blocked, rekey, edit during push, reopen and duplicate account UUID; no live data/AI`);
} finally { await browser.close(); }
