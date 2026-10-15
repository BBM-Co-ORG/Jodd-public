import assert from 'node:assert/strict';
const {chromium}=await import(process.env.JODD_PLAYWRIGHT||'playwright');
const browser=await chromium.launch({channel:'chrome',headless:true});
const page=await browser.newPage({viewport:{width:1400,height:1000}});const errors=[];
page.on('pageerror',e=>errors.push(e.message));
await page.route('**/*',r=>new URL(r.request().url()).origin==='http://127.0.0.1:1420'?r.continue():r.abort());
const f=page.frameLocator('#demo');
async function choose(id){await page.getByRole('button',{name:id,exact:true}).click();await page.locator('#demo').scrollIntoViewIfNeeded();}
async function status(t){await f.locator('.save-status').filter({hasText:t}).waitFor();}
try{
 await page.goto('http://127.0.0.1:1420/tests/browser/acceptance.html');
 await f.getByRole('button',{name:'Open confirmation',exact:true}).click();await page.keyboard.press('Enter');
 assert.equal(await f.locator('#counts').textContent(),'0,1,0,0');
 await choose('B');await f.getByText('Destination:',{exact:false}).waitFor();
 await f.locator('textarea').fill('old scope question');await f.getByRole('button',{name:'Ask',exact:true}).click();await f.locator('select').selectOption('all');
 await page.getByRole('button',{name:'ตอบคำถามที่ค้าง',exact:true}).click();assert.doesNotMatch(await f.locator('.ask-turns').textContent(),/Synthetic answer|old scope question/);
 await choose('C');await status('Sync pending');await page.getByRole('button',{name:'จำลอง sync ถูกปฏิเสธ',exact:true}).click();await status('Sync blocked');await page.getByRole('button',{name:'ปลดการปฏิเสธ',exact:true}).click();await page.getByRole('button',{name:'ยืนยันเวอร์ชันปัจจุบัน + rekey',exact:true}).click();await status('Synced');
 await choose('D');await f.locator('summary').click();await page.getByRole('button',{name:'จบ run แบบ cancelled',exact:true}).click();await f.locator('article').filter({hasText:'cancelled'}).waitFor();
 await choose('E');await f.locator('summary').click();await f.getByLabel('Attempts per workflow',{exact:true}).fill('4');await f.getByRole('button',{name:'Save AI limits'}).click();await f.getByRole('status').waitFor();
 await choose('F');await f.getByRole('button',{name:'Action items',exact:true}).click();await f.getByRole('textbox',{name:'Source text',exact:true}).fill('ตกลงให้ส่ง QA checklist ยังไม่ได้กำหนดผู้รับผิดชอบหรือวันส่ง');await f.getByRole('button',{name:'Append to existing note'}).click();await f.getByRole('textbox',{name:'Target note',exact:true}).fill('Original');await f.getByRole('button',{name:'Original meeting Notes'}).click();await f.getByRole('button',{name:'Preview meeting actions'}).click();await f.getByRole('button',{name:'Confirm append'}).waitFor();await page.getByRole('button',{name:'จำลอง target เปลี่ยนหลัง preview',exact:true}).click();await page.locator('#demo').scrollIntoViewIfNeeded();await f.getByRole('button',{name:'Confirm append'}).click();await f.getByText('Target changed since preview',{exact:false}).waitFor();
 await choose('G');await f.locator('.note-btn').filter({hasText:'Account A notebook'}).first().click();await status('Synced');await page.getByRole('button',{name:'A pending',exact:true}).click();await status('Sync pending');await page.getByRole('button',{name:'แจ้ง event ของ B',exact:true}).click();await status('Sync pending');await page.getByRole('button',{name:'A synced',exact:true}).click();await status('Synced');
 await choose('H');await f.getByRole('button',{name:'Show all 9 sources'}).click();assert.equal(await f.locator('.source-list a').count(),9);await page.getByRole('button',{name:'Dark',exact:true}).click();
 await page.screenshot({path:'/tmp/jodd-release-acceptance.png',fullPage:true});
 assert.deepEqual(errors,[]);assert.equal(await page.locator('#status').textContent(),'');
 console.log(`PASS Chrome ${browser.version()}: all eight acceptance pages and manual simulation controls, no live AI`);
}catch(e){await page.screenshot({path:'/tmp/jodd-acceptance-failure.png',fullPage:true});console.log(await f.getByRole('button',{name:'Ask',exact:true}).boundingBox().catch(()=>null));console.log(await page.locator('#demo').boundingBox());throw e;}finally{await browser.close();}
