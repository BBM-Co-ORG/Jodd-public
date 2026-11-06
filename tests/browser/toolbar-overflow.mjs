// Task 6.4: the desktop/tablet editor toolbar is one row; what does not fit
// moves into a "More formatting" menu, and a format picked there applies to the
// editor's selection. Real Chrome, on the Package C synthetic NoteEditor fixture.
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.JODD_PLAYWRIGHT || 'playwright');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
const page = await browser.newPage(); const errors = [];
page.on('pageerror', e => errors.push(e.message));
await page.route('**/*', r => new URL(r.request().url()).hostname === '127.0.0.1' ? r.continue() : r.abort());
const rows = () => page.evaluate(() => {
  const tb = document.querySelector('.editor-toolbar');
  const vis = [...tb.querySelectorAll('.fmt-btn')].filter(b => !b.closest('.fmt-menu') && !b.classList.contains('fmt-overflowed'));
  const tops = new Set(vis.map(b => { const r = b.getBoundingClientRect(); return Math.round((r.top + r.bottom) / 2 / 4); }));
  return {
    wrap: getComputedStyle(tb).flexWrap, rowCount: tops.size, height: tb.getBoundingClientRect().height,
    shown: vis.map(b => b.getAttribute('data-fmt') || b.getAttribute('aria-label')),
    hidden: [...tb.querySelectorAll('.fmt-overflowed')].map(b => b.getAttribute('data-fmt')),
    more: !!tb.querySelector('.more-fmt-btn'),
    rightInside: tb.querySelector('.toolbar-right').getBoundingClientRect().right <= tb.getBoundingClientRect().right + 0.5,
    pageOverflow: document.documentElement.scrollWidth > innerWidth,
  };
});
// Resize, then wait for the toolbar to settle on that width instead of a fixed
// sleep: the viewport has the new width, and the overflow state (what is hidden,
// whether More shows, the row's width) has stayed the same for three frames —
// the ResizeObserver has run and Svelte has flushed what it decided.
async function resizeAndSettle(width) {
  await page.setViewportSize({ width, height: 850 });
  await page.evaluate(() => { window.__tbKey = null; window.__tbSame = 0; });
  await page.waitForFunction((w) => {
    if (innerWidth !== w) return false;
    const tb = document.querySelector('.editor-toolbar');
    const key = [
      tb.querySelectorAll('.fmt-overflowed').length,
      !!tb.querySelector('.more-fmt-btn'),
      Math.round(tb.getBoundingClientRect().width),
    ].join('|');
    window.__tbSame = key === window.__tbKey ? window.__tbSame + 1 : 0;
    window.__tbKey = key;
    return window.__tbSame >= 2;
  }, width, { polling: 'raf', timeout: 5000 });
}
try {
  await page.goto('http://127.0.0.1:1420/tests/browser/package-c.html');
  await page.locator('.editor-toolbar').waitFor();
  let firstHeight;
  for (const width of [1200, 700, 500, 360]) {
    await resizeAndSettle(width);
    const r = await rows();
    assert.equal(r.wrap, 'nowrap', `nowrap @${width}`);
    assert.equal(r.rowCount, 1, `one row @${width}: ${JSON.stringify(r)}`);
    assert.ok(r.rightInside, `right side inside @${width}`);
    assert.ok(!r.pageOverflow, `no page overflow @${width}`);
    firstHeight ??= r.height;
    assert.ok(Math.abs(r.height - firstHeight) < 1, `toolbar height stable @${width}: ${r.height} vs ${firstHeight}`);
    assert.equal(r.more, r.hidden.length > 0, `More iff something is hidden @${width}`);
    console.log(width, 'shown', r.shown.length, 'hidden', r.hidden.length);
  }
  // Wide: nothing hidden. Narrow: something is.
  await resizeAndSettle(1200);
  assert.equal((await rows()).hidden.length, 0);
  await resizeAndSettle(360);
  const narrow = await rows();
  assert.ok(narrow.hidden.length > 0 && narrow.more, 'overflow at 360');
  // A suffix, from the right.
  const order = ['bold', 'italic', 'underline', 'strike', 'link', 'heading', 'quote', 'code', 'bullets', 'numbered', 'task'];
  assert.deepEqual(narrow.hidden, order.slice(order.length - narrow.hidden.length));
  // Hidden buttons are not focusable / not in the a11y tree.
  assert.ok(await page.evaluate(() => [...document.querySelectorAll('.fmt-overflowed')].every(b => b.matches(':is([inert], [inert] *)'))));

  // Format from the menu applies to the selection.
  const body = page.locator('.editor-body');
  await body.click();
  await page.keyboard.press('ControlOrMeta+A');
  const selected = await page.evaluate(() => getSelection().toString());
  assert.ok(selected.length > 0);
  const moreBtn = page.getByRole('button', { name: 'More formatting' });
  await moreBtn.click();
  const menu = page.getByTestId('more-formatting-menu');
  await menu.waitFor();
  assert.equal(await page.evaluate(() => getSelection().toString()), selected, 'opening the menu keeps the selection');
  const pick = narrow.hidden.includes('bullets') ? 'Bullet List' : 'Numbered List';
  await menu.getByRole('menuitem', { name: new RegExp('^' + pick) }).click();
  await menu.waitFor({ state: 'detached' });
  const tag = pick === 'Bullet List' ? 'ul' : 'ol';
  const html = await page.evaluate(() => document.querySelector('.editor-body').innerHTML);
  assert.ok(new RegExp(`<${tag}>[\\s\\S]*ข้อความ`).test(html), `selection became a ${tag}: ${html}`);
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains('editor-body')), true, 'focus returned to the editor');

  // Keyboard: Enter on the More button opens the menu, Escape closes it.
  await moreBtn.focus(); await page.keyboard.press('Enter');
  await menu.waitFor();
  await page.keyboard.press('Escape');
  await menu.waitFor({ state: 'detached' });
  for (const theme of ['light', 'dark']) {
    await page.evaluate(t => document.documentElement.dataset.theme = t, theme);
    await moreBtn.click(); await menu.waitFor();
    await page.screenshot({ path: `/tmp/jodd-toolbar-overflow-${theme}.png` });
    await page.keyboard.press('Escape');
  }
  assert.deepEqual(errors, []);
  console.log(`PASS Chrome ${browser.version()}: toolbar stays one row at 1200/700/500/360; overflow is a right suffix; a format picked from More formatting applies to the selection`);
} finally { await browser.close(); }
