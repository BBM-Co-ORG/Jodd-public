import assert from 'node:assert/strict';
const { chromium } = await import(process.env.JODD_PLAYWRIGHT || 'playwright');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage();
const errors = []; page.on('pageerror', e => errors.push(e.message));
await page.route('**/*', route => new URL(route.request().url()).hostname === '127.0.0.1' ? route.continue() : route.abort());
try {
  await page.goto('http://127.0.0.1:1420/tests/browser/package-e.html');
  await page.locator('summary').focus(); await page.keyboard.press('Enter');
  const toggle = page.getByRole('checkbox'); await toggle.waitFor();
  assert.equal(await toggle.isChecked(), false);
  await toggle.check();
  await page.getByLabel('Attempts per workflow', { exact: true }).fill('4');
  await page.getByRole('combobox').selectOption('unsupported');
  await page.getByRole('button', { name: 'Save AI limits' }).click();
  await page.getByRole('status').waitFor();
  const settings = await page.evaluate(() => window.packageEFixture.settings());
  assert.equal(settings.automatic_enrichment, true); assert.equal(settings.max_attempts, 4); assert.equal(settings.output_parameter, 'unsupported');
  assert.match(await page.locator('details').textContent(), /not a hard spending cap/);
  for (const width of [360, 800]) for (const theme of ['light', 'dark']) {
    await page.setViewportSize({ width, height: 1000 });
    await page.evaluate(t => document.documentElement.dataset.theme = t, theme);
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    await page.screenshot({ path: `/tmp/jodd-e-${width}-${theme}.png`, fullPage: true });
  }
  await toggle.uncheck(); await page.getByRole('button', { name: 'Save AI limits' }).click();
  assert.equal((await page.evaluate(() => window.packageEFixture.settings())).automatic_enrichment, false);
  assert.deepEqual(errors, []);
  console.log(`PASS Chrome ${browser.version()}: mock IPC limits, keyboard, preference, output support, 360/800 light/dark; no live AI`);
} finally { await browser.close(); }
