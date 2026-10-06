# ลอง Jodd v0.29.0 — Test cases ของ Packages A–H

เริ่มจาก [หน้าเลือก test case](http://127.0.0.1:1420/tests/browser/acceptance.html)
เมื่อเปิด Vite ด้วย `npm run dev -- --host 127.0.0.1` ทุก demo เป็น synthetic
browser fixture ไม่อ่านบัญชีจริง ไม่เรียก AI ไม่ส่ง sync และรีเซ็ตด้วย Reload
ปุ่มจำลองเหนือ demo ใช้ควบคุมเหตุการณ์ที่เกิดยาก โดยไม่ต้องเปิด Console
หน้า demo นี้เป็นเครื่องมือใน source checkout ไม่ได้บรรจุในแอปที่ติดตั้ง

สำหรับแอปจริง ให้ตรวจ About/What's New ว่าเป็น 0.29.0 ใช้โน้ตทดสอบใน
`Notes/Jodd-Demo` เท่านั้น ขั้นตอนที่ต้องใช้ AI จริงด้านล่างเป็นรายการให้ผู้ใช้
เลือกทดลองภายหลัง ไม่ใช่ผลที่ระบบได้ทดสอบแล้ว และอาจใช้โควตา/มีค่าใช้จ่าย
ผล model, remote sync และ Android จริงไม่อนุมานจาก demo

## A — ยืนยันคำสั่งและค้นหาไม่สับสน

**A1 · Cancel ต้องไม่กลายเป็นยืนยัน** — Demo A

1. กด Open confirmation (ปุ่มแรกใน demo) สังเกต focus เริ่มที่ Cancel
2. กด Enter หรือ Space → ต้องปิด dialog และเพิ่มตัวนับ Cancel เท่านั้น
3. เปิดใหม่ กด Tab ไป OK แล้ว Enter → เพิ่มตัวนับ Confirm
4. เปิดใหม่ ลอง Tab/Shift+Tab → focus วนเฉพาะใน dialog; Escape ยกเลิกครั้งเดียว
5. เมื่อปิด focus กลับปุ่มเปิด แม้เปิดผ่านเมนู context ก็ต้องไม่ค้าง

ในแอปจริง: เปิด dialog ลบ **โน้ตทดสอบ** แล้วกด Enter ขณะ focus ที่ Cancel
โน้ตต้องอยู่เหมือนเดิม อย่าทดสอบยืนยันลบกับโน้ตที่ต้องเก็บ

**A2 · Search loading / empty / error / retry** — Demo H

1. Try exact search → เลือก Search scope = This folder → พบโน้ต “ประชุม”
2. No matches → บอกว่าไม่มีผลลัพธ์ ไม่กล่าวว่าโฟลเดอร์ว่าง
3. Hold search → เห็น Searching; เปลี่ยนคำค้นหรือ Clear search แล้ว Release
4. คำตอบเก่าต้องไม่กลับมาใต้คำค้นใหม่
5. Search error → เห็นข้อผิดพลาดและ Retry; Release / restore search แล้ว Retry

ในแอปจริง: พิมพ์คำไทย เปลี่ยนคำ/ขอบเขตเร็ว ๆ และล้างคำค้น ผลต้องตรงขอบเขต
ล่าสุด ค้นหาใน This folder ต้องไม่รวมโฟลเดอร์ลูกโดยอัตโนมัติ (Ask เป็น subtree)

## B — สิทธิ์ข้อมูล AI และขอบเขตบทสนทนา

**B1 · เปลี่ยน scope แล้วทิ้งคำตอบเก่า** — Demo B

1. อ่าน Destination และ scope; เริ่มที่โฟลเดอร์ปัจจุบันรวมโฟลเดอร์ลูก
2. พิมพ์คำถามแล้ว Ask (คำตอบถูกหน่วงไว้)
3. เปลี่ยน scope เป็น All accounts แล้วกด “ตอบคำถามที่ค้าง” เหนือ demo
4. คำถาม/คำตอบของ scope เก่าต้องไม่กลับเข้าบทสนทนาใหม่

**B2 · เปลี่ยนสิทธิ์ระหว่างรอคำตอบ** — Reload Demo B

1. Ask แล้วกด “จำลองถอนสิทธิ์ AI” จากนั้น “ตอบคำถามที่ค้าง”
2. ผลเก่าต้องไม่ปรากฏ/ใช้ต่อ และ UI ให้เริ่มบทสนทนาใหม่

ในแอปจริง: Account Settings → AI → `Allow this account’s data in Jodd-managed AI`
ตรวจได้โดยไม่ส่งคำถาม หากจะทดสอบส่งจริง ให้ใช้บัญชี/เนื้อหาสังเคราะห์และ
provider ที่อนุมัติไว้ ข้อมูลที่ส่งออกไปแล้วเรียกคืนไม่ได้ การพิสูจน์ว่าไม่มี
metadata/content หลุดต้องอาศัย Rust recording-provider tests; UI เพียงอย่างเดียว
พิสูจน์ไม่ได้ และสิทธิ์นี้ไม่แทน MCP allowlist ของ external clients

## C — แยกบัญชีและสถานะบันทึกให้ตรงจริง

**C1 · Saved local ≠ Synced** — Demo C

1. เปิด Account A → เริ่ม Sync pending; พิมพ์ข้อความแล้วรอ autosave
2. กด “ยืนยันเวอร์ชันเก่า” → ต้องยัง pending
3. กด “จำลอง sync ถูกปฏิเสธ” → Sync blocked
4. กด “ปลดการปฏิเสธ” และ “ยืนยันเวอร์ชันปัจจุบัน + rekey” → Synced
5. ระหว่างพิมพ์เพิ่ม ผล sync เก่าต้องไม่ล้างข้อความที่ยังไม่บันทึก

**C2 · UUID ซ้ำคนละบัญชี** — Demo C

1. แก้ A แล้วเลือก Account B เหนือ demo → ต้องเห็น “ข้อความ B”
2. กลับ A → ข้อความ A ยังคงอยู่; การ rekey A ต้องไม่เปลี่ยน identity ของ B

ในแอปจริง: ใช้โน้ตทดสอบ ปิดเครือข่ายแล้วพิมพ์ → บันทึกในเครื่องได้แต่ pending
เปิดเครือข่าย → แสดง Synced เมื่อได้รับสถานะยืนยันจริง LocalFS ใช้ข้อความ
Saved to local folder ไม่กล่าวว่า cloud synced; ไม่ต้องสร้าง permanent refusal
กับข้อมูลจริงเพื่อทดลองป้าย blocked

## D — Execution receipts และความเป็นส่วนตัว

**D1 · ดูขั้นตอนและ usage ที่ไม่ทราบ** — Demo D

1. ขยาย Execution receipts ด้วย Enter → เห็น provider attempts/stages/model
2. HTTP fixture มี Reported tokens; CLI fixture แสดง Usage unknown
3. กด “จบ run แบบ cancelled” เหนือ demo → progress เปลี่ยนเป็น cancelled
4. Unknown ต้องไม่แสดงเป็น 0 หรือคำอ้างว่าใช้ฟรี

**D2 · Retention / export / delete** — Demo D

1. Keep metadata = 7 days; Export redacted metadata
2. export เป็น metadata ที่ลดรายละเอียด ไม่ใช่ source/prompt/คำตอบ
3. Delete receipt → รายการหาย (ใน demo ลบเฉพาะหน่วยความจำ)

ในแอปจริง: App Settings → AI → Execution receipts หรือเปิดใต้ผล workflow
receipt ID ไม่ใช่สิทธิ์แก้โน้ต; การลบ receipt ไม่คืน budget และไม่เรียกคืนค่าบริการ

## E — Budget ทั้ง workflow และ enrichment แบบเลือกเปิด

**E1 · ตั้งค่าแล้วเห็นผลการบันทึก** — Demo E

1. ขยาย AI limits and automatic enrichment → enrichment เริ่มปิด
2. เปิด Automatically suggest folders and related links; Attempts per workflow = 4
3. เลือก HTTP output limit support = Unsupported → Save AI limits
4. เห็นข้อความบันทึกสำเร็จและคำอธิบายว่าไม่ใช่ hard spending cap
5. ปิด enrichment แล้ว Save อีกครั้ง; ในแอปจริงอย่าเปลี่ยนค่าที่ใช้ประจำเพียงเพื่อ demo

**E2 · ถูกปฏิเสธก่อนส่ง attempt ถัดไป** — Demo H → Lesson = Budget denial

1. Open production Action Items → Action items → ใส่ข้อความ synthetic ใด ๆ → Preview
2. แสดง workflow limit reached; ไม่มีผล AI ปลอม/เปลี่ยน provider อัตโนมัติ
3. receipt ของ recording มี 0 provider attempts

ในแอปจริง: ค่าต่าง ๆ อยู่ App Settings → AI; limits ใช้ planning units
ไม่ใช่เงิน และมี scope ต่อ app session ต้องอนุมัติ live provider/budget ก่อน
ทดสอบการใช้โควตาจริง การแย่ง reservation พร้อมกันพิสูจน์ด้วย Rust tests

## F — Meeting-to-actions ที่ตรวจแหล่งอ้างอิงก่อนบันทึก

**F1 · Preview → evidence → explicit save** — Demo F

1. Action items → Source text: `ตกลงให้ส่ง QA checklist ยังไม่ได้กำหนดผู้รับผิดชอบหรือวันส่ง`
2. Preview meeting actions → ยังไม่มีการเขียนโน้ต
3. Owner/Due ต้อง Not specified; Tab ไป Evidence 1 แล้ว Enter → focus passage
4. Save separate draft → demo จำลองบันทึกแล้วปิด; source เดิมไม่ถูกแทนที่

**F2 · ร่างที่ target เปลี่ยนแล้วต้องไม่ append** — Reload Demo F

1. Action items → วาง source ข้างต้น → Append to existing note
2. ค้น Target note = Original → เลือก Original meeting → Preview
3. เปิด Unchanged existing content → ต้องเห็น Keep original content
4. กด “จำลอง target เปลี่ยนหลัง preview” เหนือ demo แล้ว Confirm append
5. ต้องปฏิเสธ Target changed since preview และเก็บ source ไว้ให้สร้าง preview ใหม่

ในแอปจริง: เปิด Extract → Action items ใช้ source/target ทดสอบเท่านั้น
มี provider call จริงตอน Preview; บันทึก separate draft เป็นค่าเริ่มต้น
Append ต้องยืนยันและตรวจ snapshot/version/backend eligibility ห้ามใช้ receipt
แทนสิทธิ์ apply หรือ rebase โดยไม่แจ้ง Quote match พิสูจน์ตำแหน่ง ไม่พิสูจน์
intent/ownership หรือว่าประโยคเสนอเป็นคำมั่นจริง

## G — Sync scheduler และ event → UI

**G1 · สถานะ update เฉพาะบัญชีที่เกี่ยวข้อง** — Demo G (full App, กว้าง ≥800px)

1. เลือก Account A notebook ในรายการ
2. กด “A pending” เหนือ demo → editor เป็น Sync pending
3. กด “แจ้ง event ของ B” (UUID เดียวกัน) → A ต้องยัง pending
4. กด “A synced” → A จึงเปลี่ยนเป็น Synced

**G2 · พิมพ์ใน LocalFS ระหว่างบัญชี remote ช้า** — แอปจริง, optional

เมื่อมีทั้ง LocalFS และบัญชี remote ทดสอบอยู่แล้ว ลองตัดเครือข่ายและพิมพ์ใน
LocalFS → UI/การบันทึกไฟล์ต้องเดินต่อ ปิด/เปิดเครือข่ายคืนเมื่อจบ
อย่าสร้างข้อมูลเสียเพื่อบังคับคิวค้าง

ไม่มีปุ่ม “scheduler” ใหม่ใน UI; remote 2 slots + LocalFS 1 slot และ
per-account ordering/FIFO/coalescing วัดด้วย deterministic host tests ใน G
G1 พิสูจน์ event→local-read→pixel เท่านั้น ไม่ใช่ live latency/fairness benchmark

## H — Sources อ่านง่ายและเปลี่ยนบริบทโดยไม่ทำข้อความหาย

**H1 · Sources + keyboard + theme** — Demo H

1. Editor มี Sources (9), แสดง 3 แหล่งพร้อมชื่อ host/path ภาษาไทย
2. focus Show all 9 sources แล้ว Enter; Tab ไปทุก source ได้
3. Show fewer sources แล้ว Space → กลับเหลือ 3 โดย focus ไม่หาย
4. ใช้ปุ่ม Light/Dark เหนือ demo และลองความกว้าง 360/800

**H2 · Pending edit → empty folder → reopen** — Demo H

1. พิมพ์ข้อความใน editor แล้ว Browse empty folder ทันที
2. ข้อความยังอยู่ พร้อม notice ว่ากำลัง browse คนละ folder; note ไม่ถูกย้าย
3. Close note → save ใช้ account/folder เดิม; focus ไป empty context
4. Reopen demo note → ข้อความล่าสุดยังอยู่

**H3 · แยกหลักฐานกับสิ่งที่ยังไม่รู้** — Demo H

ลอง Grounded meeting synthesis, Missing owner and date, Incomplete source,
Policy denial, Budget denial ตามคำแนะนำบนหน้า ทุกผลติดป้าย replay/synthetic
Live model quality, human acceptance, cost และ time savings ยัง unknown
Save ใน H ปฏิเสธเสมอ เพราะ recording ไม่ใช่ backend-held review draft
ต่างจาก F fixture ที่จำลอง apply เพื่อทดสอบ flow

## วิธีบันทึกผลทดลอง

กรอก `Case ID / app version หรือ demo URL / platform / Pass–Fail–Not run /
สิ่งที่เห็น / screenshot (เฉพาะ synthetic data)` แยก browser simulation,
packaged desktop, Android จริง และ live provider/sync ทุกครั้ง

Regression evidence ก่อน release: frontend 638 tests และ Rust workspace 1,464
ผ่านใน Package H; release รอบนี้ต้องตรวจ CI/build/encryption gate ของ release
commit อีกครั้ง ผลเหล่านั้นไม่ใช่ human UAT หรือ native cross-platform smoke
ครบชุด การทดสอบ clean-account OAuth, actual Android, remote round-trip และ
Apple delayed reconciliation ของ 0.29.0 ยังไม่ถูกอ้างว่าผ่าน
