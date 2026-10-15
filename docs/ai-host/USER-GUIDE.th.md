[English](USER-GUIDE.md)

# การใช้ AI host ใน Jodd

คู่มือนี้อธิบายฝั่งแอป Jodd: การเชื่อม Jodd เข้ากับ AI host ของคุณ การเลือก
Claude Code หรือ Codex และสิ่งที่ต้องทำเมื่อเห็นข้อความแต่ละข้อความที่ Jodd อาจแสดง
ถ้ายังไม่ได้เตรียมเครื่อง host ให้ดู[คู่มือการติดตั้ง](INSTALL.th.md)ก่อน

**CLI ของคุณเอง บัญชีของคุณเอง บนเซิร์ฟเวอร์ของคุณเอง ภายใต้ข้อกำหนดของผู้ให้บริการ**
Jodd รัน CLI ที่คุณติดตั้งและลงชื่อเข้าใช้ (sign in) ด้วยตัวเอง และไม่เคยเห็นข้อมูลการลงชื่อเข้าใช้นั้น คุณเป็นผู้รับผิดชอบให้การใช้งานเป็นไปตามที่ข้อกำหนดของ Anthropic
([สำหรับผู้บริโภค](https://www.anthropic.com/legal/consumer-terms),
[สำหรับธุรกิจ](https://www.anthropic.com/legal/commercial-terms)) หรือ[ข้อกำหนดของ OpenAI](https://openai.com/policies/terms-of-use) อนุญาต

## <a id="where-the-setting-is"></a>การตั้งค่าอยู่ที่ไหน

มีสองที่ และทั้งสองที่หน้าตาเหมือนกัน:

- **App settings** (ปุ่มรูปเฟืองข้างหมายเลขเวอร์ชันของ Jodd) → **LLM provider**
  คือผู้ให้บริการ AI สำหรับ Ask Jodd ติ๊ก **Also use for Extract and auto-link
  on accounts with no provider of their own** ถ้าต้องการให้บัญชีต่าง ๆ ใช้ด้วย
- **Account settings** (ปุ่มรูปเฟืองข้างบัญชีในรายการบัญชี) → **LLM Provider**
  คือผู้ให้บริการของบัญชีนั้นบัญชีเดียว สำหรับ Extract และ auto-link

ไม่ว่าที่ไหน ให้เลือก **Agent CLI (headless)** แล้วใต้ **Runs on** เลือก
**A server** บนเดสก์ท็อป ตัวเลือกอีกตัวคือ **This computer** ส่วนบน Android
มีแค่ **A server** ให้เลือก

แต่ละอุปกรณ์ตั้งค่าการเชื่อมต่อของตัวเองด้วย key ของตัวเอง ถ้าจะใช้เครื่อง host
จากทั้งโทรศัพท์และคอมพิวเตอร์ ให้ตั้งค่าบนทั้งสองเครื่อง แล้วเครื่อง host จะมี key
หนึ่งอันต่ออุปกรณ์หนึ่งเครื่อง

## <a id="set-up-a-server"></a>ตั้งค่าเซิร์ฟเวอร์

ก่อนเริ่ม ให้ลงชื่อเข้าใช้ CLI บนเครื่อง host ก่อน
([คู่มือการติดตั้ง](INSTALL.th.md#sign-in-to-the-clis)) และเตรียม fingerprint
ของเครื่อง host ไว้: รัน `jodd-ai-host fingerprint` บนเครื่อง host (Docker:
`docker compose exec ai-host jodd-ai-host fingerprint`)

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
- กรอก **Password** ของบัญชี แล้วกด **Trust and set up**

Jodd จะลงชื่อเข้าใช้ด้วยรหัสผ่านหนึ่งครั้ง ติดตั้งตัวช่วย (helper) และ key ของตัวเอง
แล้วตรวจว่า key ใช้งานได้ ขั้นตอนนี้ใช้เวลาไม่กี่วินาที Jodd ไม่เก็บรหัสผ่านไว้

**หน้าจอ 3: เซิร์ฟเวอร์** Jodd แสดง **Server:** และชื่อของเครื่อง host พร้อมบรรทัดของ CLI แต่ละตัว: **Claude Code** และ **Codex CLI** แต่ละตัวแสดงตำแหน่งบนเครื่อง host
หรือ **not installed** ด้านล่างมีปุ่ม **Update** และ **Remove**

ตอนนี้เป็นจังหวะที่ดีที่จะปิดการเข้าสู่ระบบด้วยรหัสผ่านบนเครื่อง host:
`sudo jodd-ai-host lock` (Docker: `docker compose exec ai-host jodd-ai-host lock`;
ดู[คู่มือการติดตั้ง](INSTALL.th.md#harden))

## <a id="choose-claude-code-or-codex"></a>เลือก Claude Code หรือ Codex

ใต้เซิร์ฟเวอร์ เปิด **Choose a CLI…** แล้วเลือก **Claude Code** หรือ **Codex
CLI** เมื่อใช้ **A server** จะมีให้เลือกแค่สองตัวนี้ และเลือกได้เฉพาะตัวที่ติดตั้งอยู่บนเครื่อง host กด **Save**

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
  แสดงหมายเหตุเรื่องนี้ไว้ใต้ตัวเลือก บนเครื่อง host ที่ใช้ Docker นั้น Codex เห็นเฉพาะภายใน container ดู[เพิ่มความปลอดภัย](INSTALL.th.md#harden)ในคู่มือการติดตั้ง

## <a id="test"></a>ทดสอบ

หลังกด **Save** ให้กด **Test connection** Jodd จะใช้การตั้งค่าที่บันทึกไว้รันการ extract จริงหนึ่งครั้ง ซึ่งปกติใช้เวลา 10 ถึง 20 วินาที และนับรวมในการใช้งานแพ็กเกจของคุณกับผู้ให้บริการเหมือนคำขออื่น ๆ ผลลัพธ์จะแสดง **OK** หรือ
**Failed** พร้อมเวลาที่ใช้ ถ้า Jodd รู้จักปัญหานั้น จะบอกสาเหตุและสิ่งที่ต้องทำ
ถ้าไม่รู้จัก จะแสดงข้อความของ CLI เอง

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
[How it works](HOW-IT-WORKS.md#where-your-data-goes) (ภาษาอังกฤษ)

## <a id="stop-a-run"></a>หยุดงานที่กำลังรัน

ยกเลิกงาน AI ตามปกติ: กด **Cancel** ใน Ask Jodd หรือ **Cancel extraction**
ใน Extract Jodd จะตัดการเชื่อมต่อ และเครื่อง host จะหยุด CLI **ภายในไม่กี่วินาที**

ถ้าเครือข่ายหลุดก่อนที่การตัดการเชื่อมต่อจะไปถึงเครื่อง host เครื่อง host จะรู้ตัวหลังจาก**ประมาณ 2 นาที** และทุกงานจะหยุดเองหลัง **120 วินาที**
(Claude Code) หรือ **180 วินาที** (Codex)

## <a id="update"></a>อัปเดต

ใช้ **Update** เมื่อ:

- Jodd บอกว่าเครื่อง host **needs an update** (ต้องอัปเดต) หลังจากคุณอัปเดต Jodd หรือแสดง
  **Jodd's helper on this server needs an update.**
- คุณติดตั้ง CLI บนเครื่อง host หลัง Setup แล้ว แต่ Jodd ยังแสดงว่า
  **not installed**

ขั้นตอน:

1. บนเครื่อง host เปิดการเข้าสู่ระบบด้วยรหัสผ่าน: `sudo jodd-ai-host unlock`
   (Docker: `docker compose exec ai-host jodd-ai-host unlock`)
2. ใน Jodd กด **Update** กรอก **Password** แล้วกด **Update now**
3. บนเครื่อง host ปิดการเข้าสู่ระบบด้วยรหัสผ่านอีกครั้ง: `sudo jodd-ai-host lock`
   (Docker: `docker compose exec ai-host jodd-ai-host lock`)

การ Update ใช้ key เดิม ถ้าไม่สำเร็จ Jodd ก็ยังเก็บ key ไว้ คุณจึงแก้สาเหตุแล้วลองใหม่ได้

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
**AI settings** หมายถึงส่วน **LLM provider** ใน **App settings** หรือใน **Account settings** แล้วแต่ว่าคุณตั้งค่าเซิร์ฟเวอร์ไว้ที่ไหน

ตรงที่ตารางเขียนว่า `sudo jodd-ai-host …` บนเครื่อง host ที่ใช้ Docker ให้รัน
`docker compose exec ai-host jodd-ai-host …` แทน

ข้อความในคอลัมน์แรกแสดงเป็นภาษาอังกฤษตามที่ปรากฏในแอป

### <a id="while-setting-up"></a>ระหว่างตั้งค่า

| ข้อความ | สาเหตุ | สิ่งที่ต้องทำ |
|---|---|---|
| Enter the server name only, without “@”. Put the login name in Username. | ช่อง **Server** มี `@` | ใส่เฉพาะที่อยู่ในช่อง **Server** และใส่ `jodd-ai` ในช่อง **Username** |
| Port must be a number between 1 and 65535. | ช่อง **Port** ไม่ใช่หมายเลขพอร์ต | กรอก `22` หรือ `2222` สำหรับ Docker |
| Enter a username. | ช่อง **Username** ว่าง | กรอก `jodd-ai` |
| Enter the password. | ช่อง **Password** ว่าง | กรอกรหัสผ่านของบัญชี |
| The SSH target is empty. | ช่อง **Server** ว่าง | กรอกที่อยู่ของเครื่อง host |
| An SSH target cannot contain spaces. | ช่อง **Server** มีช่องว่างหรืออักขระที่มองไม่เห็นอยู่ | กรอกเฉพาะที่อยู่ ไม่มีช่องว่าง |
| An SSH target cannot start with '-'. | ช่อง **Server** ขึ้นต้นด้วย `-` | กรอกเฉพาะที่อยู่ |
| could not reach *host*: no host key returned | บนเดสก์ท็อป หลังกด **Continue**: ไม่มีอะไรตอบ SSH ที่ที่อยู่และพอร์ตนั้น ส่วนใหญ่เกิดจากที่อยู่ผิด พอร์ตผิด (`22` กับ `2222` สำหรับ Docker) เครื่อง host ปิดหรือหลับอยู่ หรืออยู่คนละเครือข่าย | ตรวจที่อยู่และพอร์ต และตรวจว่าเครื่อง host เปิดอยู่และเข้าถึงได้ ([เชื่อมต่อจาก Jodd](INSTALL.th.md#connect-from-jodd)) บนเครื่อง host: `sudo jodd-ai-host doctor` |
| could not run ssh-keyscan: … | บนเดสก์ท็อป หลังกด **Continue**: Jodd เริ่ม `ssh-keyscan` ไม่ได้ ซึ่งเป็นเครื่องมือ SSH ที่ใช้อ่านตัวตนของเครื่อง host | ตรวจว่าคอมพิวเตอร์เครื่องนี้ติดตั้ง OpenSSH client แล้ว |
| Can't reach the server. Check the address, and that the server is running. | Jodd เชื่อมต่อไม่ได้: ที่อยู่หรือพอร์ตผิด เครื่อง host ปิดหรือหลับอยู่ หรืออุปกรณ์ของคุณอยู่คนละเครือข่าย | ตรวจที่อยู่และพอร์ต ถ้าอยู่ Wi-Fi เดียวกัน ให้ใช้ที่อยู่ที่ตัวติดตั้งพิมพ์ไว้ ถ้าอยู่นอกบ้าน ให้ใช้ [Tailscale](INSTALL.th.md#away-from-home-tailscale) บนเครื่อง host: `sudo jodd-ai-host doctor` |
| This server does not accept password sign-in, which setting up an AI host needs once. | การเข้าสู่ระบบด้วยรหัสผ่านของบัญชีปิดอยู่ ส่วนใหญ่เพราะคุณรัน `lock` ไว้ หรือ sshd ของเซิร์ฟเวอร์มี `AuthenticationMethods publickey` ที่ใช้กับทุกบัญชี ซึ่งปฏิเสธรหัสผ่านแม้ชุดติดตั้งจะเปิดให้บัญชีนี้ใช้รหัสผ่านแล้ว | `sudo jodd-ai-host unlock` ตั้งค่าหรืออัปเดต แล้ว `sudo jodd-ai-host lock` ถ้าไม่ได้ lock ไว้ ให้รัน `sudo jodd-ai-host doctor` ซึ่งจะบอกชื่อค่าของ sshd ที่ต้องแก้ |
| The server rejected that username or password. | รหัสผ่านผิด หรือใช้ชื่อผู้ใช้อื่นที่ไม่ใช่บัญชีของชุดติดตั้ง บน Mac อาจเกิดเมื่อบัญชีไม่อยู่ในรายชื่อที่อนุญาตของ Remote Login ด้วย หรือ sshd ของเซิร์ฟเวอร์มี `AllowUsers` หรือ `AllowGroups` ที่ไม่รวมบัญชีนี้ (หรือมี `DenyUsers` / `DenyGroups` ที่ระบุบัญชีนี้) | ตรวจชื่อผู้ใช้ (`jodd-ai`) และรหัสผ่าน ตั้งรหัสผ่านใหม่ด้วย `sudo jodd-ai-host password` บน Mac ให้รันตัวติดตั้งอีกครั้ง ถ้ารหัสผ่านถูกต้อง ให้รัน `sudo jodd-ai-host doctor` ซึ่งจะบอกชื่อค่าของ sshd ที่ต้องแก้ |
| The server's identity does not match the fingerprint you confirmed. Nothing was changed. | key ของเครื่อง host เปลี่ยนไปหลังจากที่คุณยืนยัน หรือคุณไม่ได้กำลังคุยกับเครื่อง host ของคุณ | รัน `jodd-ai-host fingerprint` บนเครื่อง host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`) แล้วเริ่มใหม่ ถ้า fingerprint เปลี่ยนไปเรื่อย ๆ ให้หยุดและหาสาเหตุ |
| The server stopped answering. Check your connection and try again. | การเชื่อมต่อเงียบไประหว่างตั้งค่า | ตรวจเครือข่ายแล้วลองใหม่ |
| Could not talk to the server: … | การเชื่อมต่อขาดระหว่างตั้งค่า | ลองใหม่ ถ้ายังไม่สำเร็จ ให้รัน `sudo jodd-ai-host doctor` |
| Could not connect: … | ปัญหาการเชื่อมต่อแบบอื่น ส่วนที่เหลือของข้อความบอกว่าเป็นแบบไหน | ทำตามส่วนที่เหลือของข้อความ แล้วรัน `sudo jodd-ai-host doctor` |
| the server has neither sha256sum nor shasum | เครื่อง host ขาดเครื่องมือที่ Jodd ต้องใช้ | ติดตั้ง `coreutils` บนเครื่อง host `sudo jodd-ai-host doctor` จะแสดงรายการสิ่งที่ยังขาดอยู่ |
| Installing on the server failed: … | Jodd ติดตั้งตัวช่วยหรือ key ไม่ได้ ส่วนที่เหลือบอกสาเหตุ | รัน `sudo jodd-ai-host doctor` แก้ตามที่รายงาน แล้วลองใหม่ |
| Jodd may have left an entry ending in jodd-ai-… in ~/.ssh/authorized_keys on … (and a file ~/.jodd/ai-run-v2); its key no longer exists on this computer, so you can remove that line. | Setup ล้มเหลวหลังจากเริ่มเปลี่ยนแปลงเครื่อง host ไปแล้ว | ลบ key นั้นบนเครื่อง host: `sudo jodd-ai-host revoke <the 8 characters after jodd-ai->` (ตัวอักษร 8 ตัวหลัง jodd-ai-) จากนั้นแก้สาเหตุแล้วตั้งค่าใหม่ |
| The AI host did not answer the status check (exit …). … | ตัวช่วยของ Jodd บนเครื่อง host ไม่ตอบตามที่ควร เช่น เพราะ `~/.jodd` ถูกลบ | unlock กด **Update** แล้ว lock ถ้ายังไม่ได้ ให้ **Remove** แล้วตั้งค่าใหม่ |
| Jodd's key is missing: … | อุปกรณ์นี้ทำ key สำหรับเครื่อง host หาย | **Remove** แล้วตั้งค่าใหม่ |
| this AI host has no usable key directory | การตั้งค่าที่บันทึกไว้ของเครื่อง host บนอุปกรณ์นี้เสียหาย | **Remove** แล้วตั้งค่าใหม่ |

### <a id="in-the-server-panel-and-the-provider-settings"></a>ในแผงเซิร์ฟเวอร์และการตั้งค่าผู้ให้บริการ

| ข้อความ | สาเหตุ | สิ่งที่ต้องทำ |
|---|---|---|
| Jodd's helper on this server needs an update. | Jodd ถูกอัปเดตและต้องใช้ตัวช่วยเวอร์ชันใหม่กว่าบนเครื่อง host | [อัปเดต](#update) |
| **not installed** (ข้าง Claude Code หรือ Codex CLI) | Jodd ไม่พบ CLI ตัวนั้นบนเครื่อง host ตอนตั้งค่าหรืออัปเดตครั้งล่าสุด | `sudo jodd-ai-host update-clis` แล้ว[อัปเดต](#update) |
| Not installed on the server — install it there, or extraction will fail. | CLI ที่คุณเลือกไม่มีอยู่บนเครื่อง host | ทำเหมือนข้อด้านบน หรือเลือก CLI อีกตัว |
| Set up your server above before saving. | เลือก **A server** แต่ยังไม่ได้ตั้งค่าเซิร์ฟเวอร์ | [ตั้งค่าเซิร์ฟเวอร์](#set-up-a-server) หรือเลือก **This computer** |
| save: set up your server first, or choose This computer. | คุณกด **Save** โดยเลือก **A server** แต่ยังไม่มีเซิร์ฟเวอร์ | ทำเหมือนข้อด้านบน |

### <a id="when-ai-runs-or-in-test"></a>ระหว่างที่ AI ทำงาน หรือตอนทดสอบ

| ข้อความ | สาเหตุ | สิ่งที่ต้องทำ |
|---|---|---|
| Couldn't reach AI host *host*: … | อุปกรณ์ของคุณเข้าถึงเครื่อง host ไม่ได้: เครื่องปิดหรือหลับอยู่ คุณอยู่นอกเครือข่ายของเครื่อง Tailscale ปิดอยู่ หรือที่อยู่เปลี่ยนไป | ปลุกเครื่อง host หรือตรวจว่าเปิดอยู่ ถ้าอยู่นอกบ้าน ให้เปิด Tailscale บนเครื่อง host: `sudo jodd-ai-host doctor` |
| The AI host *host*'s identity changed. Set it up again in AI settings. | key ของเครื่อง host เปลี่ยนไป ส่วนใหญ่เพราะติดตั้งระบบใหม่ หรือลบ volume `state` ของ Docker แต่ก็อาจหมายความว่ามีคนดักอยู่ตรงกลาง | ตรวจ `jodd-ai-host fingerprint` บนเครื่อง host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`) ถ้ารู้ว่าเปลี่ยนเพราะอะไร ให้ **Remove** แล้วตั้งค่าใหม่ โดยยืนยัน fingerprint ใหม่ ถ้าไม่รู้ ให้หยุด |
| The AI host *host* no longer accepts Jodd's key. Set it up again in AI settings. | บรรทัด key ของ Jodd หายไปจากเครื่อง host: ถูกลบด้วย `revoke` บัญชีถูกสร้างใหม่ หรือลบ volume `home` ของ Docker | **Remove** แล้วตั้งค่าใหม่ |
| The AI host *host* is not set up. Set it up in AI settings. | อุปกรณ์นี้ไม่มีการเชื่อมต่อที่บันทึกไว้สำหรับเครื่อง host | [ตั้งค่าเซิร์ฟเวอร์](#set-up-a-server) |
| The AI host *host* needs an update. Open AI settings and choose Update. | Jodd ถูกอัปเดตและต้องใช้ตัวช่วยเวอร์ชันใหม่กว่าบนเครื่อง host | [อัปเดต](#update): unlock กด **Update** แล้ว lock |
| *cli* isn't installed on *host*. | CLI ไม่มีบนเครื่อง host หรือติดตั้งหลัง Setup | `sudo jodd-ai-host update-clis` แล้ว[อัปเดต](#update) |
| Claude Code is not signed in. | Claude Code บนเครื่อง host ยังไม่ได้ลงชื่อเข้าใช้ หรือการลงชื่อเข้าใช้หมดอายุ Jodd จะต่อท้ายว่า *Do this on host, over SSH.* | `sudo jodd-ai-host signin claude` แล้วทดสอบอีกครั้ง |
| *cli* timed out after *N*s on *host* | งานใช้เวลาเกินขีดจำกัด (120 วินาทีสำหรับ Claude Code, 180 วินาทีสำหรับ Codex) | ลองใหม่โดยใช้ข้อความที่สั้นลง |
| *cli* timed out after *N*s on *host* — no output at all; the CLI may have been waiting for interactive input | CLI รออะไรบางอย่างอยู่ ส่วนใหญ่คือการลงชื่อเข้าใช้หรือคำถามตอนใช้ครั้งแรก | `sudo jodd-ai-host signin claude` หรือ `sudo jodd-ai-host signin codex` ตอบคำถามให้ครบ แล้วทดสอบอีกครั้ง |
| AI host *host* did not answer within *N*s | การเชื่อมต่อค้างนานเกินขีดจำกัดของงาน | ตรวจเครือข่ายและภาระงานของเครื่อง host แล้วลองใหม่ |
| *cli* on *host* ended without an exit status | การเชื่อมต่อหลุด หรือ CLI ถูกหยุดบนเครื่อง host (เช่น หน่วยความจำไม่พอ) | ลองใหม่ ถ้ายังเกิดซ้ำ ให้ตรวจว่าเครื่อง host มีหน่วยความจำว่าง 4 GB |
| *cli* on *host* exit *code*: … | CLI เองทำงานล้มเหลว ส่วนที่เหลือคือข้อความของ CLI | ทำตามข้อความของ CLI ถ้าเป็นปัญหาการลงชื่อเข้าใช้: `sudo jodd-ai-host signin claude` หรือ `sudo jodd-ai-host signin codex` ถ้าเป็นขีดจำกัดของแพ็กเกจหรือการใช้งาน: ตรวจบัญชีของคุณกับผู้ให้บริการ |
| CLI produced no output | CLI ทำงานจบโดยไม่มีคำตอบ | ลองใหม่ ถ้ายังเกิดซ้ำ ให้ลงชื่อเข้าใช้บนเครื่อง host ใหม่ |
| AI host *host* refused the request | SSH server ของเครื่อง host ปฏิเสธไม่รันคำขอของ Jodd | `sudo jodd-ai-host doctor` แล้ว unlock กด **Update** แล้ว lock |
| AI host *host*: … | ปัญหา SSH แบบอื่น ส่วนที่เหลือบอกว่าเป็นแบบไหน | ทำตามส่วนที่เหลือของข้อความ และรัน `sudo jodd-ai-host doctor` |
| AI data access is not allowed for this account. Allow it to continue. | บัญชียังไม่อนุญาตให้ใช้ AI ([สิทธิ์ใช้ AI](#ai-permission-per-account-and-per-device)) | เลือก **Allow AI for this account** หรือติ๊ก **Allow this account’s data in Jodd-managed AI** ใน Account settings |
