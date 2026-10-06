// Start Vite on 127.0.0.1:1420, then:
// JODD_PLAYWRIGHT=/absolute/path/to/playwright/index.mjs node tests/browser/package-a.mjs
// Uses installed Chrome with a fresh temporary profile. No live Jodd data/IPC.
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.JODD_PLAYWRIGHT || 'playwright');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', e => errors.push(e.message));
try {
  await page.goto('http://127.0.0.1:1420/tests/browser/package-a.html');
  const opener = page.locator('#opener');
  const dialog = page.getByRole('dialog', { name: 'Delete fixture?' });
  const cancel = dialog.getByRole('button', { name: 'Cancel', exact: true });
  const confirm = dialog.getByRole('button', { name: 'OK', exact: true });
  const counts = () => page.locator('#counts').textContent();
  const focused = () => page.evaluate(() => document.activeElement?.textContent?.trim());
  await opener.click();
  assert.equal(await focused(), 'Cancel');
  await page.keyboard.press('Enter');
  await dialog.waitFor({ state: 'detached' });
  assert.equal(await counts(), '0,1,0,0');
  assert.equal(await page.evaluate(() => document.activeElement?.id), 'opener');
  await opener.click(); await page.keyboard.press('Space');
  await dialog.waitFor({ state: 'detached' }); assert.equal(await counts(), '0,2,0,0');
  for (const key of ['Enter', 'Space']) {
    await opener.click(); await page.keyboard.press('Tab'); assert.equal(await focused(), 'OK');
    await page.keyboard.press(key); await dialog.waitFor({ state: 'detached' });
  }
  assert.equal(await counts(), '2,2,0,0');
  await opener.click();
  await page.keyboard.press('Shift+Tab'); assert.equal(await focused(), 'OK');
  await page.keyboard.press('Tab'); assert.equal(await focused(), 'Cancel');
  await page.locator('#background').evaluate(el => el.focus());
  assert.equal(await focused(), 'Cancel'); // Native modal inertness, not jsdom.
  const blocked = await page.locator('#background').click({ timeout: 300 }).then(() => false, () => true);
  assert.equal(blocked, true);
  await page.keyboard.press('Escape'); await dialog.waitFor({ state: 'detached' });
  assert.equal(await counts(), '2,3,0,0');
  await page.locator('#menu-opener').click();
  await page.locator('.context-menu .danger').click();
  await page.getByRole('dialog').waitFor(); assert.equal(await focused(), 'Cancel');
  await page.keyboard.press('Escape'); await page.locator('.context-menu').waitFor({ state: 'detached' });
  assert.equal(await counts(), '2,3,1,0'); // Escape settled the awaiting menu action once.
  await opener.click(); await opener.evaluate(el => el.remove());
  await page.keyboard.press('Escape'); await dialog.waitFor({ state: 'detached' });
  assert.equal(await page.evaluate(() => document.activeElement?.isConnected), true);
  await page.setViewportSize({ width: 360, height: 640 });
  await page.locator('#removed-opener').click();
  for (const theme of ['light', 'dark']) {
    await page.evaluate(theme => document.documentElement.dataset.theme = theme, theme);
    await page.screenshot({ path: `/tmp/jodd-package-a-${theme}.png` });
    const bounds = await page.locator('.prompt-dialog').boundingBox();
    assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= 360);
  }
  await cancel.click();
  assert.deepEqual(errors, []);
  console.log(`PASS native Chrome ${browser.version()}: Enter/Space, focus/Tab, background inertness, Escape, nested menu, removed opener, narrow light/dark fixture`);
} finally { await browser.close(); }
