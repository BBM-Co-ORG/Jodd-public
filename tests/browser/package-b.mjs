// Start Vite; optionally set JODD_PLAYWRIGHT to an installed Playwright module.
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.JODD_PLAYWRIGHT || 'playwright');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = [];
page.on('pageerror', e => errors.push(e.message));
await page.route('**/*', route => new URL(route.request().url()).hostname === '127.0.0.1' ? route.continue() : route.abort());
try {
  await page.goto('http://127.0.0.1:1420/tests/browser/package-b.html');
  await page.getByText('Destination:', { exact: false }).waitFor();
  assert.equal(await page.locator('select').inputValue(), 'folder');
  assert.match(await page.locator('.ask-modal').textContent(), /may use cloud services/);
  assert.match(await page.locator('.ask-modal').textContent(), /Data already sent cannot be recalled/);
  await page.locator('textarea').fill('old question');
  await page.getByRole('button', { name: 'Ask', exact: true }).click();
  await page.locator('select').selectOption('all');
  await page.evaluate(() => window.packageBFixture.resolve());
  assert.doesNotMatch(await page.locator('.ask-turns').textContent(), /Synthetic answer|old question/);
  await page.locator('textarea').fill('new question');
  await page.getByRole('button', { name: 'Ask', exact: true }).click();
  await page.evaluate(() => window.packageBFixture.revoke());
  await page.evaluate(() => window.packageBFixture.resolve());
  assert.doesNotMatch(await page.locator('.ask-turns').textContent(), /Synthetic answer|new question/);
  const calls = await page.evaluate(() => window.packageBFixture.calls);
  for (const call of calls.filter(c => c.command === 'ask_jodd')) {
    assert.ok(call.args.sessionId); assert.ok(call.args.question);
    assert.equal(call.args.turns, undefined);
  }
  for (const width of [800, 360]) for (const theme of ['light', 'dark']) {
    await page.setViewportSize({ width, height: 850 });
    await page.evaluate(theme => document.documentElement.dataset.theme = theme, theme);
    const bounds = await page.locator('.ask-modal').boundingBox();
    assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= width);
    assert.ok(bounds.y >= 0 && bounds.y + bounds.height <= 850);
    await page.screenshot({ path: `/tmp/jodd-package-b-${width}-${theme}.png` });
  }
  assert.deepEqual(errors, []);
  console.log(`PASS Chrome ${browser.version()}: synthetic Ask scope/permission races, disclosure, backend-session IPC; no live AI`);
} finally { await browser.close(); }
