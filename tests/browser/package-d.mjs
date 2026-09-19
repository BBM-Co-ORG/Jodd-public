import assert from 'node:assert/strict';
const { chromium } = await import(process.env.JODD_PLAYWRIGHT || 'playwright');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = []; page.on('pageerror', e => errors.push(e.message));
await page.route('**/*', route => new URL(route.request().url()).hostname === '127.0.0.1' ? route.continue() : route.abort());
try {
  await page.goto('http://127.0.0.1:1420/tests/browser/package-d.html');
  await page.getByRole('status').waitFor();
  assert.match(await page.getByRole('status').textContent(), /Summarizing source/);
  assert.equal(await page.locator('details').evaluate(el => el.open), false);
  await page.locator('summary').focus(); await page.keyboard.press('Enter');
  await page.getByText('Usage unknown', { exact: false }).waitFor();
  assert.match(await page.locator('ol').textContent(), /Reported tokens: 120 in \/ 24 out/);
  assert.match(await page.locator('ol').textContent(), /model unknown or redacted/);
  await page.evaluate(() => window.packageDFixture.finish());
  await page.getByRole('status').waitFor({ state: 'detached' });
  assert.match(await page.locator('article').textContent(), /cancelled/);
  for (const width of [360, 800]) for (const theme of ['light', 'dark']) {
    await page.setViewportSize({ width, height: 1000 });
    await page.evaluate(t => document.documentElement.dataset.theme = t, theme);
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    await page.screenshot({ path: `/tmp/jodd-d-${width}-${theme}.png`, fullPage: true });
  }
  await page.getByRole('combobox').selectOption('7');
  await page.getByRole('button', { name: 'Export redacted metadata' }).click();
  assert.doesNotMatch(await page.getByLabel('Redacted receipt export').inputValue(), /synthetic-run|opaque/);
  await page.getByRole('button', { name: 'Delete receipt', exact: true }).click();
  await page.getByText('No execution metadata available.').waitFor();
  assert.deepEqual(errors, []);
  console.log(`PASS Chrome ${browser.version()}: synthetic receipt progress, keyboard disclosure, unknown CLI usage, cancellation, retention, redacted export, deletion, 360/800 light/dark; no live AI`);
} finally { await browser.close(); }
