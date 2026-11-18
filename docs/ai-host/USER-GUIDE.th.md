[English](USER-GUIDE.md)

# การใช้ AI host ใน Jodd

คู่มือนี้อธิบายฝั่งแอป Jodd: การเชื่อม Jodd เข้ากับ AI host ของคุณ การเลือก
Claude Code หรือ Codex และสิ่งที่ต้องทำเมื่อเห็นข้อความแต่ละข้อความที่ Jodd อาจแสดง
ตัวเครื่อง host และชุดติดตั้ง AI host (AI host kit) ที่รันอยู่บนเครื่องนั้นแยกไปอยู่ใน repository ของตัวเองแล้ว
คือ [BBM-Co-ORG/jodd-ai-host](https://github.com/BBM-Co-ORG/jodd-ai-host)
ถ้ายังไม่ได้เตรียมเครื่อง host ให้ดู[คู่มือการติดตั้ง](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.th.md)ก่อน

**CLI ของคุณเอง บัญชีของคุณเอง บนเซิร์ฟเวอร์ของคุณเอง ภายใต้ข้อกำหนดของผู้ให้บริการ**
Jodd รัน CLI ที่คุณติดตั้งและลงชื่อเข้าใช้ (sign in) ด้วยตัวเอง และไม่เคยเห็นข้อมูลการลงชื่อเข้าใช้นั้น คุณเป็นผู้รับผิดชอบให้การใช้งานเป็นไปตามที่ข้อกำหนดของ Anthropic
([สำหรับผู้บริโภค](https://www.anthropic.com/legal/consumer-terms),
[สำหรับธุรกิจ](https://www.anthropic.com/legal/commercial-terms)) หรือข้อกำหนดของ OpenAI [สำหรับบุคคล](https://openai.com/policies/terms-of-use)
หรือ[สำหรับธุรกิจ](https://openai.com/policies/services-agreement/) อนุญาต

**ขอบเขตและข้อกำหนดผู้ให้บริการ:** การตั้งค่านี้สำหรับคนเดียวใช้บัญชีผู้ให้บริการของตน
ผ่านอุปกรณ์ของตน การเชื่อม SSH และลงชื่อเข้าใช้สำเร็จไม่ได้ยืนยันสิทธิ์สำหรับการเชื่อมแอปทุกรูปแบบ
Jodd ส่งคำขอแบบ headless จากฟีเจอร์โน้ต จึงมีบทบาทมากกว่า remote terminal
อย่าแบ่งใช้บัญชีสมาชิกส่วนบุคคล รวมบัญชีเพื่อใช้โควตา หรือขายสิทธิ์เข้าถึงต่อ
การแยกบัญชีระบบปฏิบัติการอย่างเดียวไม่ได้ทำให้บริการหลายผู้ใช้ได้รับอนุญาต
ยังต้องพิจารณาประเภทบัญชี วิธีลงชื่อเข้าใช้ นโยบาย และข้อตกลงที่ผู้ให้บริการกำหนด

## <a id="where-the-setting-is"></a>การตั้งค่าอยู่ที่ไหน

มีสองที่ และทั้งสองที่หน้าตาเหมือนกัน:

- **App settings** (ปุ่มรูปเฟืองข้างหมายเลขเวอร์ชันของ Jodd) → **AI provider**
  คือผู้ให้บริการ AI สำหรับ Ask Jodd ติ๊ก **Also use for Extract and auto-link
  on accounts with no provider of their own** ถ้าต้องการให้บัญชีต่าง ๆ ใช้ด้วย
- **Account settings** (ปุ่มรูปเฟืองข้างบัญชีในรายการบัญชี) → **LLM Provider**
  คือผู้ให้บริการของบัญชีนั้นบัญชีเดียว สำหรับ Extract และ auto-link

ไม่ว่าที่ไหน ให้เลือก **Agent CLI (headless)** แล้วใต้ **Runs on** เลือก
**A server** บนเดสก์ท็อป ตัวเลือกอีกตัวคือ **This computer** ส่วนบน Android
มีแค่ **A server** ให้เลือก

แต่ละอุปกรณ์ตั้งค่าการเชื่อมต่อของตัวเองด้วย key ของตัวเอง ถ้าจะใช้เครื่อง host
จากทั้งโทรศัพท์และคอมพิวเตอร์ ให้ตั้งค่าบนทั้งสองเครื่อง แล้วเครื่อง host จะมี key
หนึ่งอันต่ออุปกรณ์หนึ่งเครื่อง เมื่อ lock เครื่อง host แล้ว ให้ตั้งค่าอุปกรณ์เครื่องถัดไป
[ด้วยคำสั่ง](#set-up-by-command) ไม่ต้อง unlock

## <a id="set-up-a-server"></a>ตั้งค่าเซิร์ฟเวอร์

**การตั้งค่าต้องมีชุดติดตั้ง AI host อยู่บนเครื่อง host ก่อน** ติดตั้งด้วย Docker
หรือตัวติดตั้งสำหรับ Linux หรือ macOS ก็ได้
([วิธีติดตั้ง](https://github.com/BBM-Co-ORG/jodd-ai-host#install)) Jodd
จะตรวจหาชุดติดตั้งก่อนแตะต้องอะไรบนเซิร์ฟเวอร์ ถ้าไม่พบ การตั้งค่าจะหยุดพร้อมข้อความ
*This host needs the AI host kit* และเซิร์ฟเวอร์จะอยู่ในสภาพเดิมทุกอย่าง

จากนั้นลงชื่อเข้าใช้ CLI บนเครื่อง host
([คู่มือการติดตั้ง](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.th.md#sign-in-to-the-clis))
และเตรียม fingerprint ของเครื่อง host ไว้: รัน `jodd-ai-host fingerprint` บนเครื่อง host
(Docker: `docker compose exec ai-host jodd-ai-host fingerprint`)

คำสั่งในคู่มือนี้เขียนสำหรับ Linux และ macOS บนเครื่อง host ที่ใช้ Docker
ให้ตัด `sudo` ออกแล้วใส่ `docker compose exec ai-host` ไว้ข้างหน้า: `sudo jodd-ai-host lock`
จะกลายเป็น `docker compose exec ai-host jodd-ai-host lock`

**หน้าจอ 1: ที่อยู่** กรอก:

- **Server**: เฉพาะที่อยู่ของเครื่อง host ห้ามใส่ชื่อผู้ใช้หรือ `@`
- **Port**: `22` หรือ `2222` สำหรับเครื่อง host ที่ใช้ Docker ถ้าเว้นว่างไว้จะหมายถึง 22
- **Username**: `jodd-ai`

กด **Continue** Jodd จะเชื่อมต่อและอ่านตัวตนของเครื่อง host

**หน้าจอ 2: ยืนยันความน่าเชื่อถือ** Jodd จะแสดง *First time connecting to …* และตัวตนของเครื่อง host เช่น `SHA256:dswu8P+mdeYHwkh6suxXWrbP9Mj1h0vpaVzXYZssz60 (ED25519)`

- เทียบส่วน `SHA256:` กับสิ่งที่ `jodd-ai-host fingerprint` พิมพ์ออกมาบนเครื่อง host ต้องตรงกันทุกตัวอักษร ถ้าไม่ตรง ให้กด **Back** แล้วหยุด
- อ่านหมายเหตุ: key ของ Jodd ใช้รันได้แค่ Claude Code หรือ Codex แต่ Codex
  อ่านได้ทุกไฟล์ที่บัญชีนั้นอ่านได้ เครื่อง host จึงใช้บัญชีที่แยกไว้เฉพาะ
- เลือกวิธีที่ key ของ Jodd จะไปถึงเครื่อง host: **Password** (ค่าเริ่มต้น กรอกรหัสผ่านของบัญชี)
  หรือ **Run a command on the host** ([ดูด้านล่าง](#set-up-by-command))
- ถ้าเลือก **Password** ให้กด **Trust and set up**

Jodd จะลงชื่อเข้าใช้ด้วยรหัสผ่านหนึ่งครั้ง ตรวจว่ามีชุดติดตั้งอยู่ เพิ่มบรรทัด key
ของตัวเองหนึ่งบรรทัดซึ่งรันได้แค่ `jodd-ai-mcp` ของชุดติดตั้ง แล้วตรวจว่า key ใช้งานได้
ขั้นตอนนี้ใช้เวลาไม่กี่วินาที Jodd ไม่เก็บรหัสผ่านไว้

**หน้าจอ 3: เซิร์ฟเวอร์** ดู[สิ่งที่แผงเซิร์ฟเวอร์แสดง](#what-the-server-panel-shows)
ใต้รายการมีปุ่ม **Change server** และ **Remove**

**จากนั้นปิดการเข้าสู่ระบบด้วยรหัสผ่านบนเครื่อง host** Jodd จะเตือนว่า
*Next, on the host: run `sudo jodd-ai-host lock` so the AI account can no
longer log in with its password.* ให้รันคำสั่งนี้เลย ต่อจากนี้ Jodd ใช้ key
ของตัวเอง และไม่ต้องใช้รหัสผ่านอีกจนกว่าคุณจะเปลี่ยนเซิร์ฟเวอร์
(ดู[เพิ่มความปลอดภัย](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.th.md#harden)ในคู่มือการติดตั้ง)

### <a id="set-up-by-command"></a>ตั้งค่าด้วยคำสั่ง

**Run a command on the host** ไม่ต้องใช้รหัสผ่านและไม่ต้อง `unlock`
แต่ต้องใช้**ชุดติดตั้ง AI host เวอร์ชัน 1.1.0 ขึ้นไป** บนเครื่อง host

1. ในหน้าจอ 2 เลือก **Run a command on the host** แล้วกด **Trust and set up**
   Jodd จะสร้าง key ของตัวเองและแสดงคำสั่งสองแบบ คือ **On the host** และ **Docker**
   แต่ละแบบมีปุ่ม **Copy**
2. รันแบบที่ตรงกับเครื่อง host ของคุณ ในฐานะผู้ดูแลระบบ:
   `sudo jodd-ai-host add-key '<key ของ Jodd>'` หรือบนเครื่อง host ที่ใช้ Docker
   `docker compose exec ai-host jodd-ai-host add-key '<key ของ Jodd>'`
   ให้คัดลอกจาก Jodd อย่าพิมพ์เอง
3. กด **I've run it** Jodd จะตรวจว่า key ใช้งานได้แล้วบันทึกเครื่อง host
   ถ้า Jodd บอกว่าเครื่อง host **doesn't accept Jodd's key yet** แปลว่าคำสั่งยังไม่ได้รัน
   หรือชุดติดตั้งบนเครื่อง host เก่ากว่า 1.1.0 ให้แก้แล้วกด **I've run it** อีกครั้ง
   ระหว่างนั้น Jodd ยังเก็บ key ไว้

กด **Back** หรือ **Cancel** จะลบ key ที่ Jodd สร้างไว้ และลบออกจากเครื่อง host ด้วยถ้าคำสั่งถูกรันไปแล้ว
คำสั่งนี้ใช้ได้ไม่ว่าจะปิดการเข้าสู่ระบบด้วยรหัสผ่านไว้หรือไม่ จึงไม่มีขั้นตอน `unlock` และ `lock` ที่นี่

## <a id="looking-after-the-host"></a>การดูแลเครื่อง host

ถ้าใช้ชุดติดตั้ง 1.1.0 ขึ้นไป เครื่อง host จะ lock ไว้ถาวรได้เลย งานแต่ละอย่างด้านล่างมีวิธีที่ไม่ต้องใช้รหัสผ่าน:

| งาน | สิ่งที่ต้องทำ | ต้อง `unlock` ไหม |
|---|---|---|
| เพิ่มอุปกรณ์อีกเครื่อง | บนอุปกรณ์นั้น [ตั้งค่าด้วยคำสั่ง](#set-up-by-command) | ไม่ต้อง |
| Jodd บอกว่าเครื่อง host **needs an update** หรือ CLI แสดง **not installed** (เฉพาะตัวช่วยแบบเก่า) | [ติดตั้งตัวช่วยใหม่](#reinstall-helper) → **Run a command on the host** | ไม่ต้อง |
| ที่อยู่ พอร์ต หรือบัญชีของเครื่อง host เปลี่ยน | [เปลี่ยนเซิร์ฟเวอร์](#change-server) → **Run a command on the host** บนเครื่องใหม่ | ไม่ต้อง |
| เลิกใช้อุปกรณ์เครื่องหนึ่ง | ใน Jodd บนอุปกรณ์นั้น [ลบเซิร์ฟเวอร์](#remove) | ไม่ต้อง |
| อุปกรณ์หาย | บนเครื่อง host `sudo jodd-ai-host keys` แสดง key ของ Jodd หนึ่งอันต่ออุปกรณ์ และ `sudo jodd-ai-host revoke <id>` ลบออกทีละอัน | ไม่ต้อง |
| อัปเกรดชุดติดตั้ง | รันตัวติดตั้งอีกครั้ง หรือ `docker compose pull && docker compose up -d` นโยบาย key และการ lock ยังอยู่ครบ | ไม่ต้อง |
| เครื่อง host ที่ชุดติดตั้งเก่ากว่า 1.1.0 | อัปเกรดชุดติดตั้งก่อน ระหว่างนี้วิธีใช้รหัสผ่านต้อง `unlock` แล้ว `lock` อีกครั้งหลังเสร็จ | ต้อง |

บนเครื่อง host ที่ใช้ Docker ให้ใช้ `docker compose exec ai-host jodd-ai-host …` แทน `sudo jodd-ai-host …`

## <a id="what-the-server-panel-shows"></a>สิ่งที่แผงเซิร์ฟเวอร์แสดง

สำหรับเครื่อง host ที่มีชุดติดตั้ง บรรทัดแรกของแผงคือ **AI host** *เวอร์ชัน* **on**
*host* เช่น **AI host 1.0.0 on box.local** เวอร์ชันนี้เป็นของชุดติดตั้ง ไม่ใช่ของ Jodd

ถัดลงมามีหนึ่งบรรทัดต่อ CLI แต่ละตัวที่ Jodd ใช้ได้ คือ **Claude Code** และ **Codex CLI**:

- เครื่องหมายถูก: ผู้ดูแลเครื่อง host เปิดให้ใช้แล้ว Jodd ใช้ตัวนี้ได้
- เครื่องหมายกากบาทพร้อมเหตุผล: Jodd ใช้ตัวนี้บนเครื่อง host นี้ไม่ได้
  และเหตุผลนั้นบอกว่าต้องทำอะไร

เครื่อง host เป็นฝ่ายกำหนดว่า Jodd รัน CLI ตัวไหนได้ ไม่ใช่ Jodd ตอนติดตั้ง
ชุดติดตั้งจะเปิดทุก CLI ที่พบ หลังจากนั้นผู้ดูแลเครื่อง host เป็นคนเปิดปิดเองด้วย
`sudo jodd-ai-host enable <name>` และ `sudo jodd-ai-host disable <name>`
Jodd จะถามเครื่อง host ใหม่ทุกครั้งที่คุณเปิดแผงนี้ และหลังกด **Test connection**
ทุกครั้ง การเปลี่ยนแปลงบนเครื่อง host จึงแสดงขึ้นตอนนั้น

### <a id="notices"></a>คำแจ้งเตือน

เมื่อมีบางอย่างบนเครื่อง host ที่ไม่เข้ากับ Jodd เวอร์ชันนี้ แผงจะแจ้งด้วยข้อความแบบใดแบบหนึ่งในสี่แบบนี้
(ข้อความในคอลัมน์แรกแสดงเป็นภาษาอังกฤษตามที่ปรากฏในแอป):

| คำแจ้งเตือน | หมายความว่า | สิ่งที่ต้องทำ |
|---|---|---|
| *CLI* is turned off on this host. Ask its administrator to run `sudo jodd-ai-host enable` *name*. | นโยบายของเครื่อง host ไม่ให้ Jodd รัน CLI ตัวนั้น อาจถูกปิดไว้ หรือติดตั้งหลังชุดติดตั้งแล้วยังไม่เคยเปิด แสดงข้าง CLI ตัวนั้น | รันคำสั่งที่ข้อความระบุบนเครื่อง host |
| Upgrade the AI host to use *feature*: on the host, re-run the installer, or run `docker compose pull && docker compose up -d`. | ชุดติดตั้งบนเครื่อง host เก่ากว่าที่ฟีเจอร์นั้นต้องการ | อัปเดตชุดติดตั้งตามที่ข้อความบอก นโยบาย key และการ lock ยังอยู่ครบ |
| Update Jodd to use *feature* on this host. | เครื่อง host ใหม่กว่า Jodd เวอร์ชันนี้สำหรับฟีเจอร์นั้น | อัปเดต Jodd ส่วนอื่นยังใช้งานได้ตามปกติ |
| This AI host uses the old helper. Upgrade it: install the AI host kit on the host (…), then run `sudo jodd-ai-host adopt-keys`. AI features keep working meanwhile. | เครื่อง host นี้ตั้งค่าไว้ด้วย Jodd รุ่นเก่า ก่อนมีชุดติดตั้ง และยังใช้ตัวช่วย (helper) ที่ Jodd ติดตั้งไว้ตอนนั้น | ติดตั้งชุดติดตั้ง แล้วรัน `sudo jodd-ai-host adopt-keys` หนึ่งครั้ง ไม่ต้องทำอะไรใน Jodd ดู[เครื่อง host ที่ตั้งค่าไว้ก่อนมีชุดติดตั้ง](#hosts-set-up-before-the-kit) |

ถ้าคำแจ้งเตือนลงท้ายด้วย *Ask its administrator to run `jodd-ai-host doctor`*
แปลว่าเครื่อง host ทำงานไม่เป็นไปตามที่ชุดติดตั้งควรเป็น ให้รัน doctor ซึ่งจะบอกว่าต้องแก้อะไร

## <a id="choose-claude-code-or-codex"></a>เลือก Claude Code หรือ Codex

ใต้เซิร์ฟเวอร์ เปิด **Choose a CLI…** แล้วเลือก **Claude Code** หรือ **Codex
CLI** เมื่อใช้ **A server** จะมีให้เลือกแค่สองตัวนี้ และเลือกได้เฉพาะตัวที่เครื่อง host
เปิดให้ใช้ ตัวที่เลือกไม่ได้จะแสดงเหตุผลจากเครื่อง host กด **Save**

ทั้งสองตัวต่างกันในจุดหนึ่งที่สำคัญเรื่องความเป็นส่วนตัว:

- **Claude Code** รันโดยปิดเครื่องมือ (tools) ทั้งหมด และตอบจากสิ่งที่ Jodd ส่งไปเท่านั้น
- **Codex** รันใน sandbox แบบอ่านอย่างเดียว แต่**อ่านได้ทุกไฟล์บนเครื่อง host
  ที่บัญชีนั้นอ่านได้** รวมถึงไฟล์ที่ผู้ใช้ทุกคนอ่านได้ เช่น ไฟล์ส่วนใหญ่ใน
  `/etc` และบน Linux หลายระบบ ก็รวมถึงโฟลเดอร์ home ของผู้ใช้อื่นด้วย บน Mac
  ทุกบัญชีอยู่ในกลุ่ม `staff` โดยค่าเริ่มต้น และเข้าโฟลเดอร์ home ของคนอื่นได้
  ซึ่งในนั้นมีเพียง Desktop, Documents, Downloads, Movies, Music, Pictures และ Library ที่ปิดไว้
  ตัวติดตั้งให้ `jodd-ai` มีกลุ่มของตัวเองแทน `staff` แต่ถ้าเป็น Mac
  ที่คุณใช้งานเองด้วย ก็ควรปิดโฟลเดอร์ home ของคุณด้วย `chmod 700`
  และอย่าเก็บข้อมูลลับไว้ในที่ที่บัญชีอื่นอ่านได้ Jodd
  แสดงหมายเหตุเรื่องนี้ไว้ใต้ตัวเลือก บนเครื่อง host ที่ใช้ Docker นั้น Codex เห็นเฉพาะภายใน container
  ดู[เพิ่มความปลอดภัย](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.th.md#harden)ในคู่มือการติดตั้ง

## <a id="test"></a>ทดสอบ

หลังกด **Save** ให้กด **Test connection** Jodd จะใช้การตั้งค่าที่บันทึกไว้รันการ extract จริงหนึ่งครั้ง ซึ่งปกติใช้เวลา 10 ถึง 20 วินาที และนับรวมในการใช้งานแพ็กเกจของคุณกับผู้ให้บริการเหมือนคำขออื่น ๆ ผลลัพธ์จะแสดง **OK** หรือ
**Failed** พร้อมเวลาที่ใช้ ถ้า Jodd รู้จักปัญหานั้น จะบอกสาเหตุและสิ่งที่ต้องทำ
ถ้าไม่รู้จัก จะแสดงข้อความของ CLI เอง หลังทดสอบเสร็จ Jodd จะถามเครื่อง host
อีกครั้งว่าเปิด CLI ตัวไหนไว้บ้าง แผงเซิร์ฟเวอร์จึงเป็นข้อมูลล่าสุด

การทดสอบใช้การตั้งค่าที่**บันทึกไว้** ถ้าคุณเปลี่ยนอะไร ให้กด **Save** ก่อน

## <a id="ai-permission-per-account-and-per-device"></a>สิทธิ์ใช้ AI แยกตามบัญชีและตามอุปกรณ์

Jodd ส่งโน้ตของบัญชีไปยังผู้ให้บริการ AI ก็ต่อเมื่อบัญชีนั้นอนุญาตเท่านั้น
กฎนี้ใช้กับ AI host เหมือนกับผู้ให้บริการอื่นทุกราย:

- ใน **Account settings** ติ๊ก **Allow this account’s data in Jodd-managed AI**
- หรือเมื่อคุณสั่งงาน AI กับบัญชีที่ยังไม่อนุญาต Jodd
  จะถามว่า **Allow AI for this account?** ให้เลือก **Allow AI for this account** เพื่ออนุญาต แล้วสั่งงานนั้นอีกครั้ง

การตั้งค่าเหล่านี้เก็บแยกในแต่ละอุปกรณ์ เช่นเดียวกับการเชื่อมต่อ AI host
การอนุญาต AI ให้บัญชีหนึ่งบนคอมพิวเตอร์ ไม่ได้อนุญาตบนโทรศัพท์ไปด้วย

ข้อมูลที่ออกจากอุปกรณ์ของคุณเหมือนกับผู้ให้บริการรายอื่น: การปิดบังข้อมูล (masking)
และโฟลเดอร์ที่ซ่อนไว้มีผลก่อน ข้อความจะไปที่เครื่อง host ของคุณ และจากที่นั่นไปยัง
Anthropic หรือ OpenAI ไม่มีอะไรส่งไปถึงผู้พัฒนา Jodd รายละเอียดอยู่ใน
[How it works](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/HOW-IT-WORKS.md#where-your-data-goes) (ภาษาอังกฤษ)

## <a id="stop-a-run"></a>หยุดงานที่กำลังรัน

ยกเลิกงาน AI ตามปกติ: กด **Cancel** ใน Ask Jodd หรือ **Cancel extraction**
ใน Extract Jodd จะบอกเครื่อง host ให้หยุดแล้วตัดการเชื่อมต่อ เครื่อง host
จะหยุด CLI **ภายในไม่กี่วินาที**

ถ้าเครือข่ายหลุดก่อนที่การตัดการเชื่อมต่อจะไปถึงเครื่อง host เครื่อง host จะรู้ตัวหลังจาก**ประมาณ 2 นาที**
นอกจากนี้ทุกงานจะหยุดเองเมื่อถึงเวลาจำกัดของเครื่อง host ค่าเริ่มต้นคือ **120 วินาที**
สำหรับ Claude Code และ **180 วินาที** สำหรับ Codex โดยปกติเครื่อง host
รับงานพร้อมกันได้ **สองงาน** ถ้ามีงานที่สามเข้ามาจะถูกปฏิเสธทันทีพร้อมข้อความ *is busy*

## <a id="hosts-set-up-before-the-kit"></a>เครื่อง host ที่ตั้งค่าไว้ก่อนมีชุดติดตั้ง

เครื่อง host ที่ Jodd รุ่นเก่าตั้งค่าไว้ยังใช้งานได้: Jodd รุ่นใหม่จะรู้ว่าเป็นเครื่องแบบเก่า
ใช้ตัวช่วยเดิมกับเครื่องนั้น และแสดงคำแจ้งเตือน *uses the old helper*
ทางที่ดีกว่าคืออัปเกรดเครื่อง host: ติดตั้งชุดติดตั้ง แล้วรัน `sudo jodd-ai-host adopt-keys`
หนึ่งครั้ง คำสั่งนี้ย้าย key เดิมของ Jodd ไปใช้ชุดติดตั้ง ไม่ต้องทำอะไรใน Jodd
และ Jodd ยังใช้งานได้ระหว่างที่คุณทำ ภายในประมาณ 10 นาที หรือทันทีที่คุณเปิดแผงเซิร์ฟเวอร์
Jodd จะเปลี่ยนไปใช้ชุดติดตั้ง

### <a id="reinstall-helper"></a>ติดตั้งตัวช่วยใหม่

เฉพาะเครื่อง host ที่ยังใช้ตัวช่วยเดิม แผงจะแสดง **Server:** *host* แสดง CLI
แต่ละตัวพร้อมตำแหน่งบนเครื่อง หรือ **not installed** และมีปุ่ม **Reinstall helper**
เครื่อง host ที่มีชุดติดตั้งแล้วจะไม่มีปุ่มนี้ เพราะสิ่งที่เคยต้องแก้ด้วยการติดตั้งตัวช่วยใหม่
ตอนนี้ชุดติดตั้งจัดการเองบนเครื่อง host

ใช้ **Reinstall helper** กับเครื่อง host แบบเก่าเมื่อ:

- Jodd บอกว่าเครื่อง host **needs an update** (ต้องอัปเดต) หลังจากคุณอัปเดต Jodd หรือแสดง
  **Jodd's helper on this server needs an update.**
- คุณติดตั้ง CLI บนเครื่อง host หลัง Setup แล้ว แต่ Jodd ยังแสดงว่า
  **not installed**

ขั้นตอน:

1. บนเครื่อง host เปิดการเข้าสู่ระบบด้วยรหัสผ่าน: `sudo jodd-ai-host unlock`
   (Docker: `docker compose exec ai-host jodd-ai-host unlock`)
2. ใน Jodd กด **Reinstall helper** กรอก **Password** แล้วกด **Reinstall now**
3. บนเครื่อง host ปิดการเข้าสู่ระบบด้วยรหัสผ่านอีกครั้ง: `sudo jodd-ai-host lock`
   (Docker: `docker compose exec ai-host jodd-ai-host lock`)

**ถ้าใช้คำสั่งแทน:** กด **Reinstall helper** เลือก **Run a command on the host**
กด **Show command** รันคำสั่งบนเครื่อง host (ชุดติดตั้ง 1.1.0 ขึ้นไป) แล้วกด **Check**
ไม่ต้อง `unlock` หรือ `lock` และ Jodd ใช้ key เดิม

ถ้าตอนนั้นติดตั้งชุดติดตั้งบนเครื่อง host ไว้แล้ว การกด Reinstall จะย้าย key ของ Jodd
ไปใช้ชุดติดตั้งแทนการติดตั้งตัวช่วยเดิมซ้ำ การติดตั้งตัวช่วยใหม่ใช้ key เดิม
และไม่เปลี่ยนที่อยู่หรือบัญชีของเครื่อง host ถ้าต้องการเปลี่ยน ให้ใช้[เปลี่ยนเซิร์ฟเวอร์](#change-server)
ถ้าไม่สำเร็จ Jodd ก็ยังเก็บ key ไว้ คุณจึงแก้สาเหตุแล้วลองใหม่ได้

## <a id="change-server"></a>เปลี่ยนเซิร์ฟเวอร์

ใช้ **Change server** เมื่อที่อยู่ พอร์ต หรือบัญชีที่ใช้เข้าเครื่อง host เปลี่ยนไป
เช่น เราเตอร์ให้ที่อยู่ในเครือข่ายใหม่กับเครื่อง host เครื่องเดิมยังใช้งานได้จนกว่าเครื่องใหม่จะตั้งค่าเสร็จ
เครื่องใหม่ต้องมีชุดติดตั้งเหมือนการตั้งค่าทุกครั้ง

1. บนเครื่อง host เปิดการเข้าสู่ระบบด้วยรหัสผ่าน: `sudo jodd-ai-host unlock`
   (Docker: `docker compose exec ai-host jodd-ai-host unlock`)
2. ใน Jodd กด **Change server** ฟอร์มจะเปิดขึ้นพร้อม **Server**, **Port** และ **Username** เดิม
   แก้ส่วนที่เปลี่ยนแล้วกด **Continue**
3. ตรวจ fingerprint กรอก **Password** แล้วกด **Trust and set up**
4. บนเครื่อง host ปิดการเข้าสู่ระบบด้วยรหัสผ่านอีกครั้ง: `sudo jodd-ai-host lock`

**ถ้าใช้คำสั่งแทน:** ข้าม `unlock` และ `lock` กด **Change server** แก้ส่วนที่เปลี่ยน
กด **Continue** ตรวจ fingerprint เลือก **Run a command on the host** กด **Trust and set up**
รันคำสั่งที่ Jodd แสดงบนเครื่อง host *ใหม่* (ชุดติดตั้ง 1.1.0 ขึ้นไป) แล้วกด **I've run it**
ถ้า Jodd บอกว่า **doesn't accept Jodd's key yet** เครื่องเดิมจะไม่ถูกแตะต้องและยังใช้งานได้

เมื่อเครื่องใหม่ใช้งานได้ Jodd จะลบ key เดิมออกจากเครื่องเดิม (หรือแค่ลืมไว้บนอุปกรณ์นี้ ถ้าติดต่อเครื่องเดิมไม่ได้)
มีผลทันที: การตั้งค่าที่บันทึกไว้ซึ่งใช้เครื่องเดิมจะเปลี่ยนไปใช้เครื่องใหม่ และ Jodd แสดง **Server changed to** ตามด้วยชื่อเครื่องใหม่

ถ้าที่อยู่ในเครือข่ายของเครื่อง host เปลี่ยนบ่อย ปัญหาจะกลับมาอีก ให้กำหนดที่อยู่ตายตัวในเราเตอร์ หรือใช้ชื่อ Tailscale ของเครื่อง

## <a id="remove"></a>ลบเซิร์ฟเวอร์

กด **Remove** แล้วกด **Remove server** มีผลทันที ไม่ต้องรอกด **Save**:

- Jodd ลบบรรทัด key ของตัวเองบนเครื่อง host แล้วจึงลบ key บนอุปกรณ์นี้
- CLI บนเครื่อง host และการลงชื่อเข้าใช้ของคุณที่นั่นไม่ถูกแตะต้อง
- ถ้าเข้าถึงเครื่อง host ไม่ได้ Jodd จะลืมเครื่องนั้นบนอุปกรณ์นี้เท่านั้น ให้ลบ
  key บนเครื่อง host เอง: `sudo jodd-ai-host keys` แสดงรายการ key ของ Jodd
  และ `sudo jodd-ai-host revoke <id>` ลบทีละอัน

จากนั้นเลือกผู้ให้บริการรายอื่น ใน **App settings** Jodd จะแสดง
**Server removed. Choose an AI provider.** ส่วนใน **Account settings** บัญชีจะใช้ผู้ให้บริการของแอป ถ้าแอปเปิดให้บัญชีต่าง ๆ ใช้ร่วมกัน

## <a id="messages-and-what-to-do"></a>ข้อความและสิ่งที่ต้องทำ

ข้อความของ Jodd จะแสดงชื่อเครื่อง host ของคุณตรงที่ตารางนี้เขียนว่า *host* และแสดงชื่อ CLI
(`claude` หรือ `codex`) ตรงที่เขียนว่า *cli* บางข้อความมีคำนำหน้าสั้น ๆ
เช่น `transport error:` หรือ `provider not configured:` ข้อความที่พูดถึง
**AI settings** หมายถึงส่วน **AI provider** ใน **App settings** หรือใน **Account settings** แล้วแต่ว่าคุณตั้งค่าเซิร์ฟเวอร์ไว้ที่ไหน

ตรงที่ตารางเขียนว่า `sudo jodd-ai-host …` บนเครื่อง host ที่ใช้ Docker ให้รัน
`docker compose exec ai-host jodd-ai-host …` แทน

ข้อความในคอลัมน์แรกแสดงเป็นภาษาอังกฤษตามที่ปรากฏในแอป

### <a id="while-setting-up"></a>ระหว่างตั้งค่า

| ข้อความ | สาเหตุ | สิ่งที่ต้องทำ |
|---|---|---|
| Enter the server name only, without “@”. Put the login name in Username. | ช่อง **Server** มี `@` | ใส่เฉพาะที่อยู่ในช่อง **Server** และใส่ `jodd-ai` ในช่อง **Username** |
| Port must be a number between 1 and 65535. | ช่อง **Port** ไม่ใช่หมายเลขพอร์ต | กรอก `22` หรือ `2222` สำหรับ Docker |
| Enter a username. | ช่อง **Username** ว่าง | กรอก `jodd-ai` |
| Enter the password. | ช่อง **Password** ว่าง | กรอกรหัสผ่านของบัญชี หรือเลือก **Run a command on the host** |
| The host doesn't accept Jodd's key yet. Run the command on the host, then try again. | หลังกด **I've run it**: key ของ Jodd ยังไม่อยู่บนเครื่อง host คำสั่งยังไม่ได้รัน หรือรันบนบัญชีหรือเครื่องอื่น หรือชุดติดตั้งบนเครื่อง host เก่ากว่า 1.1.0 จึงไม่มี `add-key` | รันคำสั่งที่ Jodd แสดงให้ตรงตามที่แสดง บนเครื่อง AI host ในฐานะผู้ดูแลระบบ ถ้า `add-key` พิมพ์วิธีใช้หรือรายการคำสั่งออกมาแทนที่จะเพิ่ม key แปลว่าชุดติดตั้งเก่ากว่า 1.1.0 ให้อัปเดต แล้วกด **I've run it** อีกครั้ง |
| This host doesn't have the AI host kit, which "Run a command" needs. Install the kit, or set up with the password. | หลังกด **I've run it**: เครื่อง host ตอบกลับ แต่ไม่มีชุดติดตั้ง AI host ซึ่งคำสั่งนี้ต้องใช้ | ติดตั้งชุดติดตั้ง ([วิธีติดตั้ง](https://github.com/BBM-Co-ORG/jodd-ai-host#install)) หรือย้อนกลับไปตั้งค่าด้วยรหัสผ่าน |
| The SSH target is empty. | ช่อง **Server** ว่าง | กรอกที่อยู่ของเครื่อง host |
| An SSH target cannot contain spaces. | ช่อง **Server** มีช่องว่างหรืออักขระที่มองไม่เห็นอยู่ | กรอกเฉพาะที่อยู่ ไม่มีช่องว่าง |
| An SSH target cannot start with '-'. | ช่อง **Server** ขึ้นต้นด้วย `-` | กรอกเฉพาะที่อยู่ |
| could not reach *host*: no host key returned | บนเดสก์ท็อป หลังกด **Continue**: ไม่มีอะไรตอบ SSH ที่ที่อยู่และพอร์ตนั้น ส่วนใหญ่เกิดจากที่อยู่ผิด พอร์ตผิด (`22` กับ `2222` สำหรับ Docker) เครื่อง host ปิดหรือหลับอยู่ หรืออยู่คนละเครือข่าย | ตรวจที่อยู่และพอร์ต และตรวจว่าเครื่อง host เปิดอยู่และเข้าถึงได้ ([เชื่อมต่อจาก Jodd](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.th.md#connect-from-jodd)) บนเครื่อง host: `sudo jodd-ai-host doctor` ถ้าที่อยู่ของเครื่อง host เปลี่ยน ให้ใช้[เปลี่ยนเซิร์ฟเวอร์](#change-server) |
| could not run ssh-keyscan: … | บนเดสก์ท็อป หลังกด **Continue**: Jodd เริ่ม `ssh-keyscan` ไม่ได้ ซึ่งเป็นเครื่องมือ SSH ที่ใช้อ่านตัวตนของเครื่อง host | ตรวจว่าคอมพิวเตอร์เครื่องนี้ติดตั้ง OpenSSH client แล้ว |
| Can't reach the server. Check the address, and that the server is running. | Jodd เชื่อมต่อไม่ได้: ที่อยู่หรือพอร์ตผิด เครื่อง host ปิดหรือหลับอยู่ หรืออุปกรณ์ของคุณอยู่คนละเครือข่าย | ตรวจที่อยู่และพอร์ต ถ้าอยู่ Wi-Fi เดียวกัน ให้ใช้ที่อยู่ที่ตัวติดตั้งพิมพ์ไว้ ถ้าอยู่นอกบ้าน ให้ใช้ [Tailscale](https://github.com/BBM-Co-ORG/jodd-ai-host/blob/main/docs/INSTALL.th.md#away-from-home-tailscale) บนเครื่อง host: `sudo jodd-ai-host doctor` ถ้าที่อยู่ของเครื่อง host เปลี่ยน ให้ใช้[เปลี่ยนเซิร์ฟเวอร์](#change-server) |
| This host needs the AI host kit — install it from https://github.com/BBM-Co-ORG/jodd-ai-host#install, then set up again. | เซิร์ฟเวอร์ยังไม่มี `jodd-ai-mcp` ของชุดติดตั้ง และไม่มีอะไรบนเซิร์ฟเวอร์ถูกเปลี่ยน | ติดตั้งชุดติดตั้ง ([วิธีติดตั้ง](https://github.com/BBM-Co-ORG/jodd-ai-host#install)) แล้วตั้งค่าใหม่ |
| The AI host kit on this server did not run: … | มีชุดติดตั้งอยู่ แต่ `jodd-ai-mcp --version` ทำงานไม่สำเร็จ ส่วนที่เหลือคือข้อความของโปรแกรมนั้นเอง ไม่มีอะไรถูกเปลี่ยน | รัน `sudo jodd-ai-host doctor` แล้วแก้ตามที่รายงาน หรือติดตั้งชุดติดตั้งใหม่ |
| This host doesn't accept password sign-in. Choose Run a command on the host (needs AI host kit 1.1.0 or later), or run sudo jodd-ai-host unlock on the host, use the password, then run sudo jodd-ai-host lock. | การเข้าสู่ระบบด้วยรหัสผ่านของบัญชีปิดอยู่ ส่วนใหญ่เพราะคุณรัน `lock` ไว้ หรือ sshd ของเซิร์ฟเวอร์มี `AuthenticationMethods publickey` ที่ใช้กับทุกบัญชี ซึ่งปฏิเสธรหัสผ่านแม้ชุดติดตั้งจะเปิดให้บัญชีนี้ใช้รหัสผ่านแล้ว | เลือก **Run a command on the host** ซึ่งไม่ต้องใช้รหัสผ่าน เครื่องจึง lock ไว้ต่อได้ หรือรัน `sudo jodd-ai-host unlock` แล้วใช้รหัสผ่าน จากนั้นรัน `sudo jodd-ai-host lock` อีกครั้ง ถ้าไม่ได้ lock ไว้ ให้รัน `sudo jodd-ai-host doctor` ซึ่งจะบอกชื่อค่าของ sshd ที่ต้องแก้ |
| The server rejected that username or password. | รหัสผ่านผิด หรือใช้ชื่อผู้ใช้อื่นที่ไม่ใช่บัญชีของชุดติดตั้ง บน Mac อาจเกิดเมื่อบัญชีไม่อยู่ในรายชื่อที่อนุญาตของ Remote Login ด้วย หรือ sshd ของเซิร์ฟเวอร์มี `AllowUsers` หรือ `AllowGroups` ที่ไม่รวมบัญชีนี้ (หรือมี `DenyUsers` / `DenyGroups` ที่ระบุบัญชีนี้) | ตรวจชื่อผู้ใช้ (`jodd-ai`) และรหัสผ่าน ตั้งรหัสผ่านใหม่ด้วย `sudo jodd-ai-host password` บน Mac ให้รันตัวติดตั้งอีกครั้ง ถ้ารหัสผ่านถูกต้อง ให้รัน `sudo jodd-ai-host doctor` ซึ่งจะบอกชื่อค่าของ sshd ที่ต้องแก้ |
| The server's identity does not match the fingerprint you confirmed. Nothing was changed. | key ของเครื่อง host เปลี่ยนไปหลังจากที่คุณยืนยัน หรือคุณไม่ได้กำลังคุยกับเครื่อง host ของคุณ | รัน `jodd-ai-host fingerprint` บนเครื่อง host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`) แล้วเริ่มใหม่ ถ้า fingerprint เปลี่ยนไปเรื่อย ๆ ให้หยุดและหาสาเหตุ |
| The server stopped answering. Check your connection and try again. | การเชื่อมต่อเงียบไประหว่างตั้งค่า | ตรวจเครือข่ายแล้วลองใหม่ |
| Could not talk to the server: … | การเชื่อมต่อขาดระหว่างตั้งค่า | ลองใหม่ ถ้ายังไม่สำเร็จ ให้รัน `sudo jodd-ai-host doctor` |
| Could not connect: … | ปัญหาการเชื่อมต่อแบบอื่น ส่วนที่เหลือของข้อความบอกว่าเป็นแบบไหน | ทำตามส่วนที่เหลือของข้อความ แล้วรัน `sudo jodd-ai-host doctor` |
| the server has neither sha256sum nor shasum | ตอนกด Reinstall บนเครื่อง host แบบเก่า: เครื่องขาดเครื่องมือที่ตัวช่วยเดิมต้องใช้ | ติดตั้ง `coreutils` บนเครื่อง host หรือดีกว่านั้นคือ[ติดตั้งชุดติดตั้ง](#hosts-set-up-before-the-kit) |
| Installing on the server failed: … | Jodd เพิ่ม key ของตัวเองไม่ได้ ส่วนที่เหลือบอกสาเหตุ | รัน `sudo jodd-ai-host doctor` แก้ตามที่รายงาน แล้วลองใหม่ |
| Jodd may have left an entry ending in jodd-ai-… in ~/.ssh/authorized_keys on …; its key no longer exists on this computer, so you can remove that line. | Setup ล้มเหลวหลังจากเริ่มเปลี่ยนแปลงเครื่อง host ไปแล้ว | ลบ key นั้นบนเครื่อง host: `sudo jodd-ai-host revoke <the 8 characters after jodd-ai->` (ตัวอักษร 8 ตัวหลัง jodd-ai-) จากนั้นแก้สาเหตุแล้วตั้งค่าใหม่ |
| The AI host did not answer the status check within 30s. | เครื่อง host รับการเชื่อมต่อแล้ว แต่ไม่ตอบภายในเวลาที่กำหนด | ตรวจเครือข่ายและภาระงานของเครื่อง host แล้วเปิดแผงอีกครั้ง บนเครื่อง host: `sudo jodd-ai-host doctor` |
| The AI host did not answer the status check (exit …). … | ตัวช่วยบนเครื่อง host แบบเก่าไม่ตอบตามที่ควร เช่น เพราะ `~/.jodd` ถูกลบ | [ติดตั้งชุดติดตั้ง](#hosts-set-up-before-the-kit) แล้วรัน `sudo jodd-ai-host adopt-keys` หรือ unlock กด **Reinstall helper** แล้ว lock ถ้ายังไม่ได้ ให้ **Remove** แล้วตั้งค่าใหม่ |
| Jodd's key is missing: … | อุปกรณ์นี้ทำ key สำหรับเครื่อง host หาย | **Remove** แล้วตั้งค่าใหม่ |
| this AI host has no usable key directory | การตั้งค่าที่บันทึกไว้ของเครื่อง host บนอุปกรณ์นี้เสียหาย | **Remove** แล้วตั้งค่าใหม่ |

### <a id="in-the-server-panel-and-the-provider-settings"></a>ในแผงเซิร์ฟเวอร์และการตั้งค่าผู้ให้บริการ

คำแจ้งเตือนที่แผงอาจแสดงอยู่ใน[คำแจ้งเตือน](#notices)

| ข้อความ | สาเหตุ | สิ่งที่ต้องทำ |
|---|---|---|
| Turned off on this host. (ข้าง Claude Code หรือ Codex CLI) | เครื่อง host ไม่ให้ Jodd รัน CLI ตัวนั้น และไม่ได้ให้เหตุผลมา | บนเครื่อง host: `sudo jodd-ai-host enable claude` หรือ `sudo jodd-ai-host enable codex` |
| Jodd's helper on this server needs an update. | เครื่อง host แบบเก่า: Jodd ถูกอัปเดตและต้องใช้ตัวช่วยเวอร์ชันใหม่กว่าบนเครื่องนั้น | [ติดตั้งชุดติดตั้ง](#hosts-set-up-before-the-kit) หรือ[ติดตั้งตัวช่วยใหม่](#reinstall-helper) |
| **not installed** (ข้าง Claude Code หรือ Codex CLI) | เครื่อง host แบบเก่า: Jodd ไม่พบ CLI ตัวนั้นบนเครื่องตอนตั้งค่าหรืออัปเดตครั้งล่าสุด | `sudo jodd-ai-host update-clis` แล้ว[ติดตั้งตัวช่วยใหม่](#reinstall-helper) |
| Not installed on the server — install it there, or AI actions will fail. | CLI ที่คุณเลือกไม่มีอยู่บนเครื่อง host | ทำเหมือนข้อด้านบน หรือเลือก CLI อีกตัว |
| Set up your server above before saving. | เลือก **A server** แต่ยังไม่ได้ตั้งค่าเซิร์ฟเวอร์ | [ตั้งค่าเซิร์ฟเวอร์](#set-up-a-server) หรือเลือก **This computer** |
| save: set up your server first, or choose This computer. | คุณกด **Save** โดยเลือก **A server** แต่ยังไม่มีเซิร์ฟเวอร์ | ทำเหมือนข้อด้านบน |

### <a id="when-ai-runs-or-in-test"></a>ระหว่างที่ AI ทำงาน หรือตอนทดสอบ

| ข้อความ | สาเหตุ | สิ่งที่ต้องทำ |
|---|---|---|
| Couldn't reach AI host *host*: … | อุปกรณ์ของคุณเข้าถึงเครื่อง host ไม่ได้: เครื่องปิดหรือหลับอยู่ คุณอยู่นอกเครือข่ายของเครื่อง Tailscale ปิดอยู่ หรือที่อยู่เปลี่ยนไป | ปลุกเครื่อง host หรือตรวจว่าเปิดอยู่ ถ้าอยู่นอกบ้าน ให้เปิด Tailscale บนเครื่อง host: `sudo jodd-ai-host doctor` ถ้าที่อยู่ของเครื่อง host เปลี่ยน ให้ใช้[เปลี่ยนเซิร์ฟเวอร์](#change-server) |
| The AI host *host*'s identity changed. Set it up again in AI settings. | key ของเครื่อง host เปลี่ยนไป ส่วนใหญ่เพราะติดตั้งระบบใหม่ หรือลบ volume `state` ของ Docker แต่ก็อาจหมายความว่ามีคนดักอยู่ตรงกลาง | ตรวจ `jodd-ai-host fingerprint` บนเครื่อง host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`) ถ้ารู้ว่าเปลี่ยนเพราะอะไร ให้ **Remove** แล้วตั้งค่าใหม่ โดยยืนยัน fingerprint ใหม่ ถ้าไม่รู้ ให้หยุด |
| The AI host *host* no longer accepts Jodd's key. Set it up again in AI settings. | บรรทัด key ของ Jodd หายไปจากเครื่อง host: ถูกลบด้วย `revoke` บัญชีถูกสร้างใหม่ หรือลบ volume `home` ของ Docker | **Remove** แล้วตั้งค่าใหม่ |
| The AI host *host* is not set up. Set it up in AI settings. | อุปกรณ์นี้ไม่มีการเชื่อมต่อที่บันทึกไว้สำหรับเครื่อง host | [ตั้งค่าเซิร์ฟเวอร์](#set-up-a-server) |
| On *host*, *cli* is not enabled on this AI host. … | ผู้ดูแลเครื่อง host ปิด CLI ตัวนั้นไว้ หรือยังไม่เคยเปิด ข้อความนี้มาจากเครื่อง host เอง และบอกคำสั่งที่ต้องใช้ | บนเครื่อง host: `sudo jodd-ai-host enable claude` หรือ `sudo jodd-ai-host enable codex` แล้วทดสอบอีกครั้ง |
| *cli* isn't installed on *host*. | ไม่มี CLI บนเครื่อง host หรือ CLI ไม่ได้อยู่ที่เดิมแล้วตั้งแต่ตอนที่เปิดใช้ | `sudo jodd-ai-host update-clis` แล้ว `sudo jodd-ai-host enable claude` หรือ `codex` บนเครื่อง host แบบเก่า: `update-clis` แล้ว[ติดตั้งตัวช่วยใหม่](#reinstall-helper) |
| The AI host *host* is busy — try again in a moment. | เครื่อง host กำลังรันงานเต็มจำนวนที่รับได้อยู่แล้ว (ค่าเริ่มต้นคือสองงาน) เช่น งานจากอุปกรณ์อีกเครื่องของคุณ | รอให้งานอื่นเสร็จ แล้วลองใหม่ |
| This request is larger than the AI host *host* accepts. … | คำขอใหญ่เกินขนาดที่เครื่อง host รับได้ และ CLI ยังไม่ได้เริ่มทำงานเลย | ส่งข้อความให้น้อยลง เช่น เลือกข้อความสั้นลงหรือใช้โน้ตน้อยลง ขนาดสูงสุดกำหนดไว้ในนโยบายของเครื่อง host |
| The AI host *host* needs an update. Open AI settings and choose Reinstall helper. | เครื่อง host แบบเก่า: Jodd ถูกอัปเดตและต้องใช้ตัวช่วยเวอร์ชันใหม่กว่าบนเครื่องนั้น | [ติดตั้งชุดติดตั้ง](#hosts-set-up-before-the-kit) หรือ unlock กด **Reinstall helper** แล้ว lock |
| Claude Code is not signed in. | Claude Code บนเครื่อง host ยังไม่ได้ลงชื่อเข้าใช้ หรือการลงชื่อเข้าใช้หมดอายุ Jodd จะต่อท้ายว่า *Do this on host, over SSH.* | `sudo jodd-ai-host signin claude` แล้วทดสอบอีกครั้ง |
| *cli* timed out after *N*s on *host* | งานใช้เวลาเกินขีดจำกัดของเครื่อง host (ค่าเริ่มต้นคือ 120 วินาทีสำหรับ Claude Code, 180 วินาทีสำหรับ Codex) | ลองใหม่โดยใช้ข้อความที่สั้นลง |
| *cli* timed out after *N*s on *host* — no output at all; the CLI may have been waiting for interactive input | CLI รออะไรบางอย่างอยู่ ส่วนใหญ่คือการลงชื่อเข้าใช้หรือคำถามตอนใช้ครั้งแรก | `sudo jodd-ai-host signin claude` หรือ `sudo jodd-ai-host signin codex` ตอบคำถามให้ครบ แล้วทดสอบอีกครั้ง |
| AI host *host* did not answer within *N*s | การเชื่อมต่อค้างนานเกินขีดจำกัดของงาน | ตรวจเครือข่ายและภาระงานของเครื่อง host แล้วลองใหม่ |
| *cli* on *host* ended without an exit status | การเชื่อมต่อหลุด หรือ CLI ถูกหยุดบนเครื่อง host (เช่น หน่วยความจำไม่พอ) | ลองใหม่ ถ้ายังเกิดซ้ำ ให้ตรวจว่าเครื่อง host มีหน่วยความจำว่าง 4 GB |
| *cli* on *host* exit *code*: … | CLI เองทำงานล้มเหลว ส่วนที่เหลือคือข้อความของ CLI | ทำตามข้อความของ CLI ถ้าเป็นปัญหาการลงชื่อเข้าใช้: `sudo jodd-ai-host signin claude` หรือ `sudo jodd-ai-host signin codex` ถ้าเป็นขีดจำกัดของแพ็กเกจหรือการใช้งาน: ตรวจบัญชีของคุณกับผู้ให้บริการ |
| *cli* on *host* failed: … | เครื่อง host รัน CLI ไม่ได้เลย ส่วนที่เหลือคือข้อความจากเครื่อง host เอง | ทำตามข้อความนั้น และรัน `sudo jodd-ai-host doctor` |
| CLI produced no output | CLI ทำงานจบโดยไม่มีคำตอบ | ลองใหม่ ถ้ายังเกิดซ้ำ ให้ลงชื่อเข้าใช้บนเครื่อง host ใหม่ |
| AI host *host* refused the request | SSH server ของเครื่อง host ปฏิเสธไม่รันคำขอของ Jodd | รัน `sudo jodd-ai-host doctor` ถ้า key ของ Jodd ยังใช้ตัวช่วยเดิมอยู่ ให้รัน `sudo jodd-ai-host adopt-keys` |
| AI host *host*: … | ปัญหา SSH หรือการเชื่อมต่อแบบอื่น ส่วนที่เหลือบอกว่าเป็นแบบไหน | ทำตามส่วนที่เหลือของข้อความ และรัน `sudo jodd-ai-host doctor` |
| AI data access is not allowed for this account. Allow it to continue. | บัญชียังไม่อนุญาตให้ใช้ AI ([สิทธิ์ใช้ AI](#ai-permission-per-account-and-per-device)) | เลือก **Allow AI for this account** หรือติ๊ก **Allow this account’s data in Jodd-managed AI** ใน Account settings |
