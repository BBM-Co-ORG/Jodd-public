import assert from 'node:assert/strict';
const {chromium} = await import(process.env.JODD_PLAYWRIGHT || 'playwright');
const browser=await chromium.launch({channel:'chrome',headless:true});
const page=await browser.newPage();const errors=[];
page.on('pageerror',e=>errors.push(e.message));
await page.route('**/*',r=>new URL(r.request().url()).hostname==='127.0.0.1'?r.continue():r.abort());
async function start() {
  await page.goto('http://127.0.0.1:1420/tests/browser/package-f.html');
  await page.getByRole('button',{name:'Action items',exact:true}).click();
  await page.getByRole('textbox',{name:'Source text',exact:true}).fill('ตกลงให้ส่ง QA checklist ยังไม่ได้กำหนดผู้รับผิดชอบหรือวันส่ง');
}
try {
  await start();
  await page.getByRole('button',{name:'Preview meeting actions'}).click();
  await page.getByRole('button',{name:'Save separate draft'}).waitFor();
  assert.equal(await page.evaluate(()=>window.packageFFixture.calls.some(c=>c.command==='apply_action_items')),false);
  const link=page.getByRole('link',{name:'Evidence 1'});await link.focus();await page.keyboard.press('Enter');
  assert.equal(await page.evaluate(()=>document.activeElement.id),'meeting-fixture-1');
  for(const width of [360,800]) for(const theme of ['light','dark']) {
    await page.setViewportSize({width,height:1000});await page.evaluate(t=>document.documentElement.dataset.theme=t,theme);
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
    await page.screenshot({path:`/tmp/jodd-f-${width}-${theme}.png`,fullPage:true});
  }
  await page.getByRole('button',{name:'Save separate draft'}).focus();await page.keyboard.press('Enter');
  await page.getByRole('dialog').waitFor({state:'hidden'});
  assert.equal(await page.evaluate(()=>window.packageFFixture.calls.filter(c=>c.command==='apply_action_items').length),1);
  await start();await page.getByRole('button',{name:'Append to existing note'}).click();
  await page.getByRole('textbox',{name:'Target note',exact:true}).fill('Original');
  await page.getByRole('button',{name:'Original meeting Notes'}).click();
  await page.getByRole('button',{name:'Preview meeting actions'}).click();
  await page.getByText('Unchanged existing content').click();
  assert.ok(await page.getByText('Keep original content').isVisible());
  await page.evaluate(()=>window.packageFFixture.refuseApply());
  await page.getByRole('button',{name:'Confirm append'}).click();
  await page.getByText('Target changed since preview',{exact:false}).waitFor();
  assert.ok((await page.getByRole('textbox',{name:'Source text',exact:true}).inputValue()).includes('QA checklist'));
  assert.deepEqual(errors,[]);
  console.log(`PASS Chrome ${browser.version()}: synthetic review, evidence keyboard navigation, explicit draft/append, stale refusal, 360/800 light/dark; no live provider`);
} finally {await browser.close();}
