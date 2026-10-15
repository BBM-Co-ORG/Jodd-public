import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
const {chromium}=await import(process.env.JODD_PLAYWRIGHT||'playwright');
const replay=JSON.parse(readFileSync(new URL('./teaching-replay.json',import.meta.url),'utf8'));
const browser=await chromium.launch({channel:'chrome',headless:true});
const page=await browser.newPage();const errors=[];const blocked=[];
page.on('pageerror',e=>errors.push(e.message));
await page.route('**/*',r=>{const u=new URL(r.request().url());if(u.origin==='http://127.0.0.1:1420') return r.continue();blocked.push(u.origin);return r.abort();});
async function noOverflow(){assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'page overflow');}
try {
 for(const platform of ['macos','android']) {
  await page.goto(`http://127.0.0.1:1420/tests/browser/package-h.html?platform=${platform}`);
  await page.getByRole('heading',{name:'Sources before certainty'}).waitFor();
  await page.getByRole('button',{name:'Show all 9 sources'}).focus();await page.keyboard.press('Enter');
  assert.equal(await page.locator('.source-list a').count(),9);
  // Native Tab from the disclosure reaches every exposed link in order.
  for(let i=1;i<=9;i++){await page.keyboard.press('Tab');assert.ok((await page.evaluate(()=>document.activeElement?.getAttribute('href'))).endsWith(`/${i}`));}
  await page.getByRole('button',{name:'Show fewer sources'}).focus();await page.keyboard.press('Space');
  assert.equal(await page.locator('.source-list a').count(),3);
  for(const width of [360,800]) for(const theme of ['light','dark']) {
   await page.setViewportSize({width,height:1000});await page.evaluate(t=>document.documentElement.dataset.theme=t,theme);await noOverflow();
   await page.screenshot({path:`/tmp/jodd-h-${platform}-${width}-${theme}.png`,fullPage:true});
  }
 }
 await page.getByRole('button',{name:'Try exact search'}).click();
 await page.getByRole('combobox',{name:'Search scope'}).selectOption('folder');
 await page.locator('.note-item').waitFor();
 assert.equal(await page.evaluate(()=>window.packageHFixture.calls.filter(c=>c.command==='search_notes').at(-1).args.label),'Notes/Jodd-Demo');
 assert.equal(await page.evaluate(()=>window.packageHFixture.calls.some(c=>c.command==='preview_action_items')),false);
 await page.getByRole('button',{name:'No matches',exact:true}).click();await page.getByText('No results for',{exact:false}).waitFor();await noOverflow();
 await page.getByRole('button',{name:'Hold search',exact:true}).click();await page.locator('.empty-state').getByText('Searching…',{exact:true}).waitFor();await noOverflow();
 await page.getByRole('button',{name:'Release / restore search'}).click();
 await page.getByRole('button',{name:'Search error',exact:true}).click();await page.getByRole('button',{name:'Retry',exact:true}).waitFor();await noOverflow();
 await page.getByRole('button',{name:'Release / restore search'}).click();await page.getByRole('button',{name:'Retry',exact:true}).click();
 await page.getByRole('button',{name:'Editor',exact:true}).click();
 const editor=page.locator('.editor-body');await editor.fill('ข้อความค้างก่อนเปลี่ยนโฟลเดอร์');
 await page.getByRole('button',{name:'Browse empty folder'}).click();
 assert.ok(await editor.innerText()==='ข้อความค้างก่อนเปลี่ยนโฟลเดอร์');
 await page.getByRole('button',{name:'Close note',exact:true}).click();await page.locator('.empty-editor').waitFor();
 assert.ok((await page.locator('.empty-editor').innerText()).includes('Notes/ว่าง'));
 await page.getByRole('button',{name:'Reopen demo note'}).click();await editor.waitFor();
 assert.equal(await editor.innerText(),'ข้อความค้างก่อนเปลี่ยนโฟลเดอร์');
 for(const id of ['th-01','th-02','th-10','policy','budget']) {
  await page.getByRole('combobox',{name:'Lesson',exact:true}).selectOption(id);
  await page.locator('.lesson summary').filter({hasText:'Execution receipts'}).click();
  await page.getByText('0 provider attempt(s)',{exact:false}).waitFor();
  await noOverflow();
  await page.getByRole('button',{name:'Open production Action Items'}).click();
  await page.getByRole('button',{name:'Action items',exact:true}).click();
  await page.getByRole('textbox',{name:'Source text',exact:true}).fill(replay.lessons.find(l=>l.id===id)?.source ?? 'Synthetic source only');
  await page.getByRole('button',{name:'Preview meeting actions'}).click();
  if(id==='policy'||id==='budget') await page.getByText(replay[id].error,{exact:false}).waitFor();
  else {
   await page.getByRole('button',{name:'Save separate draft'}).waitFor();
   const evidence=page.getByRole('link',{name:'Evidence 1',exact:true});await evidence.focus();await page.keyboard.press('Enter');
   assert.ok((await page.evaluate(()=>document.activeElement.id)).startsWith('meeting-'));
   for(const width of [360,800]) for(const theme of ['light','dark']){await page.setViewportSize({width,height:1000});await page.evaluate(t=>document.documentElement.dataset.theme=t,theme);await noOverflow();}
   await page.screenshot({path:`/tmp/jodd-h-review-${id}.png`,fullPage:true});
   await page.getByRole('button',{name:'Save separate draft'}).click();await page.getByText('Replay is read-only.',{exact:false}).waitFor();
  }
  await page.screenshot({path:`/tmp/jodd-h-lesson-${id}.png`,fullPage:true});
  await page.keyboard.press('Escape');await page.getByRole('dialog').waitFor({state:'hidden'});
 }
 // Unknown commands and unmatched sources must fail closed, without fallback.
 const refused=await page.evaluate(async()=>{
  const invoke=window.__TAURI_INTERNALS__.invoke;
  const result=[];
  for(const [command,args] of [['test_connection',{}],['preview_action_items',{accountId:'other',sourceText:'unmatched',requestId:'negative'}]]) {
   try{await invoke(command,args);result.push(false);}catch{result.push(true);}
  }return result;
 });assert.deepEqual(refused,[true,true]);
 assert.deepEqual(errors,[]);assert.deepEqual(blocked,[]);
 console.log(`PASS Chrome ${browser.version()}: production list/editor/modal/receipts; native Tab/Enter/Space, sources, pending edit context, search empty/loading/error, replay synthesis/missing/incomplete/policy/budget, read-only apply; 360/800 light/dark desktop and simulated mobile. No live provider/native Tauri/Android proof.`);
} finally {await browser.close();}
