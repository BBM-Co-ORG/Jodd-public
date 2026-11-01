// Development-only controls over existing, same-origin synthetic fixtures.
const frame=document.querySelector('#demo');
const status=document.querySelector('#status');
const cases={
 A:{title:'A · Cancel, keyboard และค้นหา',path:'package-a.html',steps:'กด Open confirmation → Enter บน Cancel ต้องยกเลิก; เปิดใหม่ Tab ไป OK แล้ว Enter จึงยืนยัน; Escape ปิดและคืน focus',notice:'ตัวนับคือ confirm,cancel,closed-menu,background-click; search loading/error/retry อยู่ Demo H',controls:[]},
 B:{title:'B · ขอบเขต Ask และการถอนสิทธิ์',path:'package-b.html',steps:'พิมพ์คำถาม → Ask → เปลี่ยน scope → กดตอบคำถามที่ค้าง: คำตอบเก่าต้องไม่กลับมา ลอง Reload แล้วถอนสิทธิ์ระหว่างรอด้วย',notice:'Destination และ scope เป็น synthetic; ไม่มี provider จริง',controls:[['ตอบคำถามที่ค้าง',w=>w.packageBFixture.resolve()],['จำลองถอนสิทธิ์ AI',w=>w.packageBFixture.revoke()]]},
 C:{title:'C · Saved locally / pending / blocked / synced',path:'package-c.html',steps:'แก้ A รอ autosave → ยืนยันเวอร์ชันเก่า (ยัง pending) → block → unblock → ยืนยันปัจจุบัน/rekey; สลับ B ต้องยังเห็นข้อความ B',notice:'A และ B มี UUID เริ่มต้นซ้ำกัน จึงเห็นผลของ account-qualified identity',controls:[['Account A',w=>w.packageCFixture.select('synthetic-a')],['Account B',w=>w.packageCFixture.select('synthetic-b')],['ยืนยันเวอร์ชันเก่า',w=>w.packageCFixture.push(0)],['จำลอง sync ถูกปฏิเสธ',w=>w.packageCFixture.block()],['ปลดการปฏิเสธ',w=>w.packageCFixture.unblock()],['ยืนยันเวอร์ชันปัจจุบัน + rekey',w=>{const f=w.packageCFixture;return f.push(f.snapshot().notes.find(n=>n.account_id==='synthetic-a').local_version,'assigned');}]]},
 D:{title:'D · Execution receipts',path:'package-d.html',steps:'ขยาย receipt ด้วย Enter → เปรียบเทียบ Reported/Unknown → จบ run แบบ cancelled → เลือก retention → Export → Delete receipt',notice:'token/model/timing ใน fixture ไม่ใช่ผล benchmark จริง; deletion นี้เกิดในหน่วยความจำ',controls:[['จบ run แบบ cancelled',w=>w.packageDFixture.finish()]]},
 E:{title:'E · Budget และ enrichment',path:'package-e.html',steps:'ขยาย AI limits → เปิด enrichment → Attempts = 4 → Unsupported → Save → ปิด enrichment แล้ว Save อีกครั้ง',notice:'ตั้งค่าเฉพาะ fixture; planning units ไม่ใช่เงิน; denial ลองได้ใน Demo H',controls:[]},
 F:{title:'F · Preview, passage evidence และ stale target',path:'package-f.html',steps:'Action items → วาง source ตามคู่มือ → Preview → Evidence 1 → Save separate draft; Reload แล้ว Append to existing note → Original → Preview → จำลอง target เปลี่ยน → Confirm append ต้องปฏิเสธ',notice:'Source: ตกลงให้ส่ง QA checklist ยังไม่ได้กำหนดผู้รับผิดชอบหรือวันส่ง',controls:[['จำลอง target เปลี่ยนหลัง preview',w=>w.packageFFixture.refuseApply()]]},
 G:{title:'G · Event → local state → visible status',path:'full-app.html',steps:'เลือก Account A notebook → A pending → แจ้ง event ของ B: A ยัง pending → A synced',notice:'ใช้ความกว้าง ≥800px สำหรับ full desktop App; ไม่ใช่ scheduler latency/fairness benchmark',controls:[['A pending',w=>{w.showcase.setDirty('a',true);w.showcase.emit('note-persistence-changed',{accountId:'demo-a',uuid:'shared-uuid'});}],['แจ้ง event ของ B',w=>w.showcase.emit('note-persistence-changed',{accountId:'demo-b',uuid:'shared-uuid'})],['A synced',w=>{w.showcase.setDirty('a',false);w.showcase.emit('note-persistence-changed',{accountId:'demo-a',uuid:'shared-uuid'});}]]},
 H:{title:'H · Sources, pending edits และ teaching replay',path:'package-h.html',steps:'Show all sources → Tab ทุก link → collapse; พิมพ์ใน editor → Browse empty folder → Close note → Reopen; ทดลอง search states และ 5 lessons บนหน้า',notice:'H ไม่อนุญาต apply recording เป็นโน้ต; human acceptance/cost/time savings ยัง unknown',controls:[]}
};
let current='A';
function select(key){
 current=key;const c=cases[key];status.textContent='';
 for(const b of document.querySelectorAll('#packages button'))b.setAttribute('aria-pressed',String(b.textContent===key));
 document.querySelector('#title').textContent=c.title;document.querySelector('#steps').textContent=c.steps;document.querySelector('#notice').textContent=c.notice;
 const url=new URL(c.path,location.href);frame.src=url.href;document.querySelector('#direct').href=url.href;
 const controls=document.querySelector('#controls');controls.replaceChildren();
 for(const [label,run] of c.controls){const b=document.createElement('button');b.textContent=label;b.onclick=async()=>{
  try{if(frame.contentWindow.location.href!==url.href)throw Error('fixture not ready');await run(frame.contentWindow);status.textContent=`ส่งเหตุการณ์จำลองแล้ว: ${label}`;}catch{status.textContent='รอ demo โหลดเสร็จ หรือกด Reload แล้วลองใหม่';}
 };controls.append(b);}
}
for(const key of Object.keys(cases)){const b=document.createElement('button');b.textContent=key;b.onclick=()=>select(key);document.querySelector('#packages').append(b);}
document.querySelector('#reload').onclick=()=>select(current);
for(const theme of ['light','dark'])document.querySelector(`#${theme}`).onclick=()=>{frame.contentDocument.documentElement.dataset.theme=theme;};
select('A');
