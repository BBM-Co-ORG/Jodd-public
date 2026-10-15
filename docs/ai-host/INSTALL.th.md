[English](INSTALL.md)

# ติดตั้ง Jodd AI host

AI host คือเครื่องของคุณเองที่ Jodd ใช้รัน Claude Code หรือ Codex แทนคุณผ่าน SSH
คู่มือนี้อธิบายการตั้งเครื่อง การเชื่อม Jodd เข้ากับเครื่อง และการดูแลให้ปลอดภัย
ส่วนการทำงานภายในอยู่ใน [How it works](HOW-IT-WORKS.md) (ภาษาอังกฤษ)
และฝั่งแอป Jodd อยู่ใน[คู่มือการใช้งาน](USER-GUIDE.th.md)

**CLI ของคุณเอง บัญชีของคุณเอง บนเซิร์ฟเวอร์ของคุณเอง ภายใต้ข้อกำหนดของผู้ให้บริการ**
คุณติดตั้ง CLI และลงชื่อเข้าใช้ (sign in) ด้วยตัวเอง Jodd ไม่เคยเห็นข้อมูลการลงชื่อเข้าใช้นั้น
คุณเป็นผู้รับผิดชอบให้การใช้งานเป็นไปตามที่ข้อกำหนดของ Anthropic
([สำหรับผู้บริโภค](https://www.anthropic.com/legal/consumer-terms),
[สำหรับธุรกิจ](https://www.anthropic.com/legal/commercial-terms)) หรือ[ข้อกำหนดของ OpenAI](https://openai.com/policies/terms-of-use) อนุญาต

## <a id="what-has-been-tested"></a>สิ่งที่ทดสอบแล้ว

| วิธีติดตั้ง | ทดสอบเมื่อ | ทดสอบอย่างไร |
|---|---|---|
| Docker | 2026-10-15 | ทดสอบ image โดยเริ่มด้วย `docker run`: เริ่มครั้งแรกโดยไม่มีเครือข่าย, ติดตั้ง CLI ทั้งสองตัว, โค้ดตั้งค่าของ Jodd เองเชื่อมต่อและพบ CLI ทั้งสองตัว, `lock` / `unlock` และสร้าง container ใหม่ (fingerprint, CLI ที่ติดตั้งไว้ และสถานะ lock ยังอยู่ครบ) ส่วนไฟล์ `compose.yaml` เองยังไม่ได้รันทดสอบตั้งแต่ต้นจนจบ |
| Ubuntu 24.04, Debian 12, Fedora, Alpine | 2026-10-15 | ทดสอบตัวติดตั้งใน container ที่สะอาด: ติดตั้ง CLI ตัวจริง, เข้าสู่ระบบด้วยรหัสผ่านจริง, โค้ดตั้งค่าของ Jodd เองเชื่อมต่อและพบ CLI ทั้งสองตัว, เซิร์ฟเวอร์ที่ sshd ปิดการเข้าสู่ระบบด้วยรหัสผ่านสำหรับทุกบัญชีไว้แล้ว (บัญชีอื่นทุกบัญชียังคงค่าเดิม) และการรันซ้ำที่ไม่เปลี่ยนอะไรเลย การสั่งเริ่ม sshd ผ่าน systemd หรือ OpenRC ไม่ได้อยู่ในการทดสอบนี้ |
| macOS | 2026-10-15 | ทดสอบบน Mac ชิป Apple Silicon หนึ่งเครื่องที่ใช้ macOS 27 (OpenSSH 10.3): สร้างบัญชีใหม่, จำกัดรายชื่อผู้ใช้ของ Remote Login ให้เหลือเฉพาะ `jodd-ai`, ติดตั้ง CLI ทั้งสองตัว, โค้ดตั้งค่าของ Jodd เองเชื่อมต่อด้วยรหัสผ่านและพบ CLI ทั้งสองตัว, fingerprint ตรงกับที่ sshd ส่งมา, `lock` และการรันซ้ำกับบัญชีเดิมที่ย้ายบัญชีไปอยู่ในกลุ่มของตัวเอง ยังไม่ได้ทดสอบ macOS 13–26, Mac ชิป Intel, `--no-sleep` และขั้นตอนการลบ |

การทดสอบทั้งหมดนี้ทำบนเครื่อง **ARM64** (aarch64) ยังไม่มีการทดสอบบน x86_64
(Intel หรือ AMD) เลย

## <a id="before-you-start"></a>ก่อนเริ่ม

สิ่งที่ต้องมี:

- **เครื่องที่เปิดทิ้งไว้ตลอด** Jodd ใช้เครื่อง host ได้เฉพาะตอนที่เครื่องตื่นอยู่และเข้าถึงได้
- **หน่วยความจำ 4 GB** ขึ้นไป บน x86 หรือ ARM แบบ 64 บิต
- **บัญชีกับผู้ให้บริการ** สำหรับ CLI แต่ละตัวที่ต้องการใช้:
  - Claude Code: แพ็กเกจ Claude Pro, Max, Team หรือ Enterprise หรือบัญชี Anthropic Console
  - Codex: แพ็กเกจ ChatGPT
- **Jodd 0.34.0 ขึ้นไป** บนอุปกรณ์ของคุณ
- **`curl`** บนเครื่อง host สำหรับดาวน์โหลดชุดติดตั้ง ระบบส่วนใหญ่มีอยู่แล้ว
  ถ้าไม่มี ให้ติดตั้งด้วย package manager ของระบบ (หรือดาวน์โหลด URL เดียวกันด้วย `wget -O`)

เลือกวิธีใดวิธีหนึ่ง:

1. [Docker](#docker): เครื่องใดก็ได้ที่มี Docker เปลี่ยนแปลงเครื่องน้อยที่สุด
   และปลอดภัยที่สุด เพราะ Codex อ่านได้เฉพาะสิ่งที่อยู่ใน container
   (ดู[เพิ่มความปลอดภัย](#harden))
2. [Ubuntu และ Debian](#ubuntu-and-debian)
3. [Fedora, RHEL และ Alpine](#fedora-rhel-and-alpine)
4. [macOS](#macos)
5. [Linux อื่น ๆ](#other-linux): ขั้นตอนเดียวกัน แต่ทำด้วยมือ

จากนั้น[ลงชื่อเข้าใช้ CLI](#sign-in-to-the-clis) [เชื่อมต่อจาก Jodd](#connect-from-jodd)
และ[เพิ่มความปลอดภัย](#harden)

## <a id="docker"></a>Docker

ต้องมี Docker พร้อม Compose plugin (`docker compose`)

สร้างโฟลเดอร์สำหรับ host แล้วเข้าไปในโฟลเดอร์นั้น:

```sh
mkdir jodd-ai-host
```

```sh
cd jodd-ai-host
```

ดาวน์โหลดไฟล์ compose:

```sh
curl -fsSLO https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/docker/compose.yaml
```

**ตั้งรหัสผ่านเอง (แนะนำ)** สร้างไฟล์ชื่อ `.env` ไว้ข้าง ๆ `compose.yaml`
โดยมีบรรทัดเดียว:

```text
JODD_AI_PASSWORD=choose-a-long-password
```

ถ้ารหัสผ่านมีตัว `$` ให้เขียนเป็น `$$` ถ้าไม่มีไฟล์นี้ container จะสร้างรหัสผ่านขึ้นมาเอง
และแสดงใน log เพียงครั้งเดียว วิธีนี้ก็ใช้ได้ แต่รหัสผ่านที่คุณเปลี่ยนเองภายหลังใน container
จะหายไปเมื่ออัปเกรด ส่วนรหัสผ่านใน `.env` จะยังอยู่

เริ่มการทำงาน:

```sh
docker compose up -d
```

อ่าน log:

```sh
docker compose logs
```

log จะแสดง fingerprint ของ host key, รหัสผ่านที่สร้างขึ้น (ถ้าคุณไม่ได้ตั้งเอง)
และข้อความ `jodd-ai-host ready` CLI ทั้งสองตัวจะติดตั้งอยู่เบื้องหลังในการเริ่มครั้งแรก
รอสักสองสามนาทีแล้วรัน `docker compose logs` อีกครั้ง มองหาข้อความ `CLIs installed`
ถ้าขึ้นว่า `could not install the CLIs` ให้ตรวจการเชื่อมต่ออินเทอร์เน็ตของเครื่อง แล้วรัน:

```sh
docker compose exec ai-host jodd-ai-host update-clis
```

**พอร์ตคือ 2222** ไม่ใช่ 22 Jodd จะเชื่อมต่อมาที่ที่อยู่ของเครื่องนี้ที่พอร์ต 2222

**ใครเข้าถึงพอร์ตได้บ้าง** โดยค่าเริ่มต้น พอร์ตจะเปิดบนทุกเครือข่ายที่เครื่องต่ออยู่
โทรศัพท์ที่ต่อ Wi-Fi เดียวกันจึงเข้าถึงได้ ซึ่งเหมาะกับเครื่องที่บ้าน
แต่ถ้าเป็น VPS หรือเซิร์ฟเวอร์บนคลาวด์ ให้เพิ่มบรรทัดใน `.env`
เพื่อเปิดพอร์ตเฉพาะบนที่อยู่ของ VPN (เช่นที่อยู่ [Tailscale](#away-from-home-tailscale) ของคุณ)
หรือเฉพาะบนเครื่องนั้นเอง:

```text
JODD_AI_BIND=100.101.102.103
```

Docker เปิดพอร์ตโดยไม่ผ่านกฎของ `ufw` และ `firewalld` กฎ firewall อย่างเดียวจึงปิดพอร์ตไม่ได้
ค่าอื่นใน `.env`: `JODD_AI_PORT` ใช้เปลี่ยนพอร์ต 2222 เป็นพอร์ตอื่น

**ใน Docker ทุกคำสั่ง `jodd-ai-host` รันผ่าน `docker compose exec`
โดยไม่ต้องใช้ `sudo`** ทุกที่ในคู่มือที่เขียนว่า `sudo jodd-ai-host <command>`
(หรือ `jodd-ai-host <command>`) ให้รันคำสั่งนี้จากโฟลเดอร์ที่มี `compose.yaml`
โดยแทน `status` ด้วยคำสั่งที่ต้องการ:

```sh
docker compose exec ai-host jodd-ai-host status
```

ตัวอย่างเช่น `sudo jodd-ai-host lock` จะกลายเป็น
`docker compose exec ai-host jodd-ai-host lock`

ถ้าพลาดรหัสผ่านที่สร้างขึ้น ให้ตั้งรหัสผ่านใหม่:

```sh
docker compose exec ai-host jodd-ai-host password
```

ขั้นต่อไป: [ลงชื่อเข้าใช้ CLI](#sign-in-to-the-clis)

## <a id="ubuntu-and-debian"></a>Ubuntu และ Debian

ทดสอบแล้วกับ OpenSSH 9.2 ถึง 10.3 บน Debian 12, Ubuntu 24.04, Fedora และ Alpine
เวอร์ชันที่เก่ากว่านั้นอาจใช้ได้ แต่ยังไม่ได้ทดสอบ ถ้า sshd รุ่นเก่าไม่ยอมรับการตั้งค่า
ตัวติดตั้งจะหยุดและคืนค่า SSH ที่แตะไปกลับเป็นเหมือนเดิม

**บน VPS หรือเครื่องใดก็ตามที่เข้าถึงได้จากอินเทอร์เน็ต:** เมื่อตัวติดตั้งทำงานเสร็จ
ใครก็ได้บนอินเทอร์เน็ตสามารถลองเดารหัสผ่านของ `jodd-ai` ได้ จนกว่าคุณจะรัน
`sudo jodd-ai-host lock` ดังนั้นให้ตั้งค่า Jodd โดยเร็ว แล้ว lock ทันที
และจำกัดการเข้าถึงพอร์ต SSH ด้วย firewall ([เพิ่มความปลอดภัย](#harden))

ดาวน์โหลดตัวติดตั้ง:

```sh
curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/install/linux.sh -o jodd-ai-host-install.sh
```

อ่านก่อนรัน เพราะตัวติดตั้งทำงานด้วยสิทธิ์ root:

```sh
less jodd-ai-host-install.sh
```

รัน:

```sh
sudo sh jodd-ai-host-install.sh
```

ตัวติดตั้งจะถามรหัสผ่านสำหรับบัญชีใหม่ `jodd-ai` (สองครั้ง) Jodd ต้องใช้รหัสผ่านนี้ครั้งเดียวตอน Setup ถ้ากด Enter โดยไม่พิมพ์อะไร ตัวติดตั้งจะสร้างรหัสผ่านให้และพิมพ์ออกมา

สิ่งที่ตัวติดตั้งทำ ตามลำดับ:

1. ติดตั้ง OpenSSH server, `curl`, `bash` และเครื่องมือพื้นฐานอีกเล็กน้อยด้วย
   package manager ของระบบ บน Ubuntu และ Debian จะไม่อัปเกรดแพ็กเกจที่มีอยู่แล้ว
2. สร้างบัญชี `jodd-ai` โดยใช้ `bash` เป็น login shell และเพิ่ม
   `~/.local/bin` ลงใน `PATH` ของ login shell
3. ตั้งรหัสผ่านของบัญชี
4. สร้าง SSH host key ที่ยังไม่มี โดยไม่แทนที่ key ที่มีอยู่แล้วเด็ดขาด
5. เขียนไฟล์ `/etc/ssh/sshd_config.d/50-jodd-ai.conf` ซึ่งเปิดการเข้าสู่ระบบด้วยรหัสผ่าน
   **เฉพาะ `jodd-ai` เท่านั้น** บัญชีอื่นทุกบัญชียังใช้ค่าเดิมของเซิร์ฟเวอร์
   ตัวติดตั้งจะทำให้ sshd อ่านไฟล์นี้ก่อน ตรวจผลด้วย `sshd -t`
   และเทียบค่าที่มีผลจริงของบัญชีอื่นก่อนและหลัง ถ้ามีอะไรผิดปกติ
   จะคืนค่า SSH กลับเป็นแบบเดิมทุกประการแล้วหยุด
6. ติดตั้ง `claude` และ `codex` ในนามบัญชี `jodd-ai` จากตัวติดตั้งทางการของแต่ละตัว
7. เริ่มหรือ reload sshd โดยไม่ตัด SSH session ที่เปิดอยู่
8. ติดตั้ง `/usr/local/bin/jodd-ai-host` รัน `jodd-ai-host doctor` แล้วพิมพ์ที่อยู่ของ host, พอร์ต, ชื่อบัญชี และ fingerprint

การรันซ้ำจะซ่อมสิ่งที่ขาดและไม่เปลี่ยนอย่างอื่น รหัสผ่านของบัญชียังคงเดิม
และถ้าคุณ lock ไว้ การเข้าสู่ระบบด้วยรหัสผ่านก็ยังปิดอยู่

ค่าที่ส่งให้ตัวติดตั้งได้:

| ค่า | ทำอะไร |
|---|---|
| `JODD_AI_USER` | ชื่อบัญชี (ค่าเริ่มต้น `jodd-ai`) ต้องเป็นบัญชีที่ยังไม่มี หรือเป็นบัญชีธรรมดาที่ไม่สามารถเป็น root ได้ |
| `JODD_AI_PASSWORD` | รหัสผ่าน สำหรับการรันที่ไม่มี terminal การรันครั้งถัดไปจะนำไปใช้ซ้ำก็ต่อเมื่อคุณเปลี่ยนค่านี้ |
| `JODD_AI_PORT` | เป็นแค่พอร์ตที่แสดงในสรุปท้ายการติดตั้ง (เช่นพอร์ตที่คุณ forward ไว้) ไม่ได้เปลี่ยนพอร์ตของ sshd |
| `JODD_AI_SKIP_CLIS=1` | ข้ามการติดตั้ง CLI สำหรับเครื่องที่ไม่มีอินเทอร์เน็ต ติดตั้งภายหลังได้ด้วย `sudo jodd-ai-host update-clis` |

ใส่ค่าเหล่านี้ไว้ระหว่าง `sudo` กับ `sh` เช่น:

```sh
sudo JODD_AI_SKIP_CLIS=1 sh jodd-ai-host-install.sh
```

ขั้นต่อไป: [ลงชื่อเข้าใช้ CLI](#sign-in-to-the-clis)

## <a id="fedora-rhel-and-alpine"></a>Fedora, RHEL และ Alpine

ใช้ตัวติดตั้งเดียวกันและคำสั่งสามคำสั่งเดียวกับ
[Ubuntu และ Debian](#ubuntu-and-debian) ตัวติดตั้งใช้ `dnf` บน Fedora และ RHEL
และใช้ `apk` บน Alpine

- **Fedora** ทดสอบแล้ว RHEL และระบบที่ build จาก RHEL ใช้ขั้นตอน `dnf` เดียวกัน
  แต่ยังไม่ได้ทดสอบ
- **Alpine** มักไม่มี `sudo` ให้รันตัวติดตั้งและทุกคำสั่ง `jodd-ai-host`
  ในฐานะ root โดยไม่ต้องใส่ `sudo` ตัวติดตั้งจะเพิ่มสิ่งที่ Claude Code ต้องใช้บน Alpine
  (`libgcc`, `libstdc++` และ `ripgrep` ของระบบ) และตั้งให้ Claude Code
  ใช้ `ripgrep` ตัวนั้น ทั้ง Claude Code และ Codex ทำงานบน Alpine ได้
  แต่เช่นเดียวกับส่วนอื่น ๆ ทดสอบบน ARM64 เท่านั้น

## <a id="macos"></a>macOS

ทดสอบแล้วบน Mac ชิป Apple Silicon หนึ่งเครื่องที่ใช้ macOS 27
(ดู[สิ่งที่ทดสอบแล้ว](#what-has-been-tested)) ให้รันโหมดจำลอง (dry run) ก่อนและอ่านสิ่งที่พิมพ์ออกมา
โหมดนี้ไม่เปลี่ยนอะไรเลย

ต้องใช้ macOS 13 (Ventura) ขึ้นไป และเป็น Mac ที่เปิดทิ้งไว้ตลอด

ดาวน์โหลดตัวติดตั้ง:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/install/macos.sh -o jodd-ai-host-install.sh
```

ดูทุกขั้นตอนที่ตัวติดตั้งจะทำ โดยไม่เปลี่ยนแปลงอะไรเลย (ไม่ต้องใช้ `sudo`):

```sh
sh jodd-ai-host-install.sh --dry-run
```

รัน:

```sh
sudo sh jodd-ai-host-install.sh
```

สิ่งที่ตัวติดตั้งทำ:

1. สร้างบัญชีมาตรฐาน `jodd-ai` ("Jodd AI host") ที่ซ่อนจากหน้าจอ login
   และถามรหัสผ่านของบัญชีนี้จากคุณ คำถามรหัสผ่านมาจาก `sysadminctl` ของ macOS เอง โดยแสดง `User password:`
   และถาม**ครั้งเดียว** ไม่มีการให้พิมพ์ยืนยันซ้ำ (จึงควรพิมพ์อย่างระมัดระวัง) และหน้าจอจะไม่แสดงสิ่งที่พิมพ์
   ก่อนหน้านั้น `sysadminctl` จะพิมพ์คำเตือนที่มีข้อความ "No clear text password or interactive option was specified … FDE"
   ซึ่งไม่เป็นอันตราย หมายความเพียงว่า `jodd-ai` ปลดล็อก FileVault ตอนเปิดเครื่องไม่ได้ ซึ่งบัญชีนี้ไม่จำเป็นต้องทำอยู่แล้ว
2. ให้ `jodd-ai` มีกลุ่ม (group) ของตัวเองแทน `staff` และปิดโฟลเดอร์ home
   ของบัญชีนี้ไม่ให้บัญชีอื่นเข้า (`chmod 700`) บน Mac ทุกบัญชีอยู่ในกลุ่ม `staff`
   และ `staff` เข้าโฟลเดอร์ home ของทุกคนได้ ขั้นนี้จึงกัน `jodd-ai` (และ Codex
   ซึ่งรันในนามบัญชีนี้) ออกจาก home ของคุณ ถ้ารันกับบัญชี `jodd-ai`
   ที่มีอยู่แล้วและยังอยู่ใน `staff` ตัวติดตั้งก็จะย้ายให้เช่นกัน ถ้าหลังจากนั้น macOS
   ยังนับว่าบัญชีนี้อยู่ใน `staff` ตัวติดตั้งจะเตือน ดู[รู้ไว้ว่า Codex อ่านอะไรได้บ้าง](#harden)
3. เขียนกฎ sshd แบบเดียวกับบน Linux: เปิดการเข้าสู่ระบบด้วยรหัสผ่านเฉพาะ `jodd-ai`
4. เปิด **Remote Login** ซึ่ง macOS ยอมให้เปิดจาก Terminal ได้ก็ต่อเมื่อ Terminal
   มีสิทธิ์ Full Disk Access ถ้าไม่มี ตัวติดตั้งจะบอกให้คุณเปิด
   **System Settings → General → Sharing** แล้วเปิด **Remote Login** เอง
   และรอจนคุณกด Enter
5. ติดตั้ง `claude` และ `codex` ในนามบัญชี `jodd-ai` จากตัวติดตั้งทางการของแต่ละตัว
6. ติดตั้ง `/usr/local/bin/jodd-ai-host` แล้วพิมพ์ที่อยู่, พอร์ต,
   ชื่อบัญชี และ fingerprint พร้อมรายชื่อโฟลเดอร์ home อื่นบน Mac
   ที่บัญชีอื่นยังเข้าได้ แต่ละรายการมีคำสั่ง `chmod 700` ที่ใช้ปิดโฟลเดอร์นั้น
   ตัวติดตั้งเพียงแสดงรายชื่อ ไม่ได้เปลี่ยนอะไรในโฟลเดอร์เหล่านั้น

**ใครอีกบ้างที่ได้สิทธิ์ SSH** Remote Login มีรายชื่อที่อนุญาต ("Allow access for")
บน Mac เครื่องใหม่ รายชื่อนี้มีกลุ่ม Administrators อยู่แล้ว ถ้า Remote Login ปิดอยู่
และรายชื่อนั้นมีใครอื่นนอกจากบัญชีใหม่ ตัวติดตั้งจะถามว่า:

- (a) ให้เฉพาะ "Jodd AI host" ใช้ SSH ได้: ลบคนอื่นออกจากรายชื่อ (แนะนำ)
- (b) คงรายชื่อไว้ตามเดิม
- (c) หยุด

เลือก (b) เฉพาะเมื่อคุณรู้ว่าใครอยู่ในรายชื่อ บน Mac เครื่องใหม่ การเลือก (b) หมายความว่า
**บัญชีผู้ดูแลระบบ (administrator) ทุกบัญชีบน Mac จะเข้าสู่ระบบผ่าน SSH ด้วยรหัสผ่านของตัวเองจากเครือข่ายของคุณได้** ทั้งที่ก่อนหน้านี้ไม่มีใครทำได้

สำหรับการรันที่ไม่มี terminal ให้เลือกด้วย flag แทน: `--ssh-only-ai`
สำหรับ (a) และ `--ssh-keep-list` สำหรับ (b) ถ้าไม่มีทั้ง terminal และ flag
ตัวติดตั้งจะหยุดและปล่อย Remote Login ไว้ในสถานะปิด

ถ้า Remote Login **เปิดอยู่แล้ว** ตัวติดตั้งจะไม่เปลี่ยนว่าใครอื่นเข้าสู่ระบบได้บ้าง:

- ถ้าจำกัดไว้ด้วยรายชื่อ ตัวติดตั้งจะเพิ่ม `jodd-ai` เข้าไปในรายชื่อนั้นโดยไม่ลบใครออก ทุกคนที่อยู่ในรายชื่อ (บน Mac ที่ไม่ได้ปรับแต่ง คือกลุ่ม Administrators)
  ยังเข้าสู่ระบบผ่าน SSH ด้วยรหัสผ่านได้เหมือนเดิม ถ้าต้องการจำกัดให้แคบลง: **System
  Settings → General → Sharing → Remote Login → Allow access for**
- ถ้าเปิดไว้สำหรับผู้ใช้ทุกคน ทุกบัญชีบน Mac จะเข้าสู่ระบบผ่าน SSH ด้วยรหัสผ่านของตัวเองได้
  ไม่ใช่เฉพาะ `jodd-ai` ตัวติดตั้งจะปล่อยไว้อย่างนั้นและเตือนคุณ
  ถ้าต้องการจำกัด ให้เลือก **Only these users** ในหน้าเดียวกัน แล้วเพิ่มคนที่จำเป็นต้องใช้
  และ "Jodd AI host"

**บน macOS พอร์ตเป็น 22 เสมอ**

**อย่าให้ Mac หลับ** Mac ที่หลับอยู่ตอบ Jodd ไม่ได้ ให้เปิด
**System Settings → Energy → Prevent automatic sleeping when the display is off**
(บนโน้ตบุ๊ก: **Battery → Options**) หรือรันตัวติดตั้งด้วย `--no-sleep`
ซึ่งจะทำให้ Mac ไม่หลับตราบที่เสียบสายชาร์จอยู่:

```sh
sudo sh jodd-ai-host-install.sh --no-sleep
```

`JODD_AI_USER` และ `JODD_AI_SKIP_CLIS=1` ทำงานเหมือนบน Linux แต่ `JODD_AI_PASSWORD`
ต่างออกไป: ใช้เฉพาะตอนที่ตัวติดตั้งสร้างบัญชีเท่านั้น และไม่เปลี่ยนรหัสผ่านของบัญชีที่มีอยู่แล้วเด็ดขาด (ถ้าจะเปลี่ยน ให้ใช้ `sudo jodd-ai-host password`)
ควรพิมพ์รหัสผ่านตอนที่ตัวติดตั้งถาม มากกว่าใช้ `JODD_AI_PASSWORD`
เพราะเครื่องมือจัดการบัญชีของ macOS รับค่านั้นผ่าน command line
ซึ่งผู้ใช้อื่นบน Mac อาจเห็นได้ชั่วขณะ

## <a id="other-linux"></a>Linux อื่น ๆ

สำหรับระบบที่ตัวติดตั้งไม่รู้จัก (ตัวติดตั้งรองรับ `apt`, `dnf` และ `apk`)
ให้ทำขั้นตอนเดียวกันด้วยมือ ขั้นตอนเหล่านี้ทำซ้ำสิ่งที่ตัวติดตั้งทำ แต่ยังไม่ได้ทดสอบทั้งชุด คำสั่งด้านล่างสมมติว่ามี `sudo` ถ้าไม่มีให้ใช้ root โดยตรง

เครื่อง host ต้องมี: OpenSSH server (ทดสอบแล้วกับ 9.2 ถึง 10.3 เวอร์ชันที่เก่ากว่าอาจใช้ได้
แต่ยังไม่ได้ทดสอบ), `sh`, `bash`, `curl`, `base64`,
`sha256sum` หรือ `shasum`, `mktemp`, `head` ที่รองรับ `-c` และ `setsid`
(จาก util-linux)

**1. สร้างบัญชี** โดยใช้ `bash` เป็น login shell:

```sh
sudo useradd -m -s /bin/bash jodd-ai
```

```sh
sudo passwd jodd-ai
```

**2. ติดตั้ง CLI ในนามบัญชีนี้** เปิด login shell ในนาม `jodd-ai`:

```sh
sudo -iu jodd-ai
```

ใน shell นั้น เพิ่ม `~/.local/bin` ลงใน `PATH` ของ login shell (ถ้าบัญชีมีไฟล์
`~/.bash_profile` ให้เพิ่มบรรทัดเดียวกันในไฟล์นั้นด้วย):

```sh
echo 'export PATH="$HOME/.local/bin:$HOME/.codex/bin:$PATH"' >> ~/.profile
```

ติดตั้ง Claude Code:

```sh
curl -fsSL https://claude.ai/install.sh | bash
```

ติดตั้ง Codex:

```sh
curl -fsSL https://chatgpt.com/codex/install.sh | sh
```

ออกจาก shell ของบัญชี:

```sh
exit
```

**3. เปิดการเข้าสู่ระบบด้วยรหัสผ่านเฉพาะบัญชีนี้** ก่อนอื่น จดค่าที่ sshd
ใช้กับ root อยู่ตอนนี้ (คำสั่งจะพิมพ์ออกมาหนึ่งบรรทัด ให้เก็บไว้):

```sh
sudo sshd -T -C user=root,host=x,addr=127.0.0.1 | grep -i '^passwordauthentication'
```

จากนั้นสร้างไฟล์
`/etc/ssh/sshd_config.d/50-jodd-ai.conf` โดยมีข้อความนี้ทุกตัวอักษร:

```text
# Managed by jodd-ai-host. Change with: jodd-ai-host lock | unlock
# Only the account below is affected; every other account keeps the server's settings.
Match User jodd-ai
    PasswordAuthentication yes
    KbdInteractiveAuthentication no
    ClientAliveInterval 30
    ClientAliveCountMax 4
```

**4. ทำให้ sshd อ่านไฟล์นี้ก่อน** sshd จะใช้ค่าแรกที่อ่านเจอ และบล็อก
`Match` จะกลืนทุกอย่างที่อยู่ถัดจากมัน ถ้าค่าแรกใน
`/etc/ssh/sshd_config` ยังไม่ใช่ `Include /etc/ssh/sshd_config.d/*.conf`
ให้เพิ่มบรรทัดนี้เป็นบรรทัดแรกของไฟล์นั้น:

```text
Include /etc/ssh/sshd_config.d/50-jodd-ai.conf
```

**5. ตรวจก่อน reload** คำสั่งนี้ต้องไม่พิมพ์อะไรออกมาเลย:

```sh
sudo sshd -t
```

คำสั่งนี้ต้องพิมพ์ `passwordauthentication yes`:

```sh
sudo sshd -T -C user=jodd-ai,host=x,addr=127.0.0.1 | grep -i '^passwordauthentication'
```

รันคำสั่งจากขั้นที่ 3 อีกครั้ง (คำสั่งที่มี `user=root`) ต้องได้บรรทัดเดียวกับก่อนหน้า ถ้ามีอะไรผิด ให้ย้อนขั้นที่ 3 และ 4 กลับ

**6. Reload sshd** ตามวิธีของระบบคุณ เช่น `sudo systemctl
reload sshd`, `sudo rc-service sshd reload` หรือ `sudo service sshd reload`

**7. ติดตั้งตัวช่วย (helper)**

```sh
sudo mkdir -p /usr/local/lib/jodd-ai-host
```

```sh
sudo curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/bin/jodd-ai-host -o /usr/local/bin/jodd-ai-host
```

```sh
sudo curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/lib/dropin.sh -o /usr/local/lib/jodd-ai-host/dropin.sh
```

```sh
sudo curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/lib/doctor.sh -o /usr/local/lib/jodd-ai-host/doctor.sh
```

```sh
sudo chmod 755 /usr/local/bin/jodd-ai-host
```

สร้างไฟล์ `/etc/jodd-ai-host.conf` โดยมีข้อความนี้ (ถ้า sshd ของคุณใช้พอร์ตอื่น
ให้เปลี่ยนพอร์ตด้วย):

```text
JODD_AI_USER=jodd-ai
JODD_AI_PORT=22
JODD_AI_KIND=linux
JODD_AI_DROPIN=/etc/ssh/sshd_config.d/50-jodd-ai.conf
JODD_AI_HOSTKEY=/etc/ssh/ssh_host_ed25519_key.pub
JODD_AI_LIB=/usr/local/lib/jodd-ai-host
```

**8. ตรวจทุกอย่าง:**

```sh
sudo jodd-ai-host doctor
```

## <a id="sign-in-to-the-clis"></a>ลงชื่อเข้าใช้ CLI

ลงชื่อเข้าใช้บนเครื่อง host ด้วยตัวคุณเอง ด้วยบัญชีของคุณเอง Jodd ไม่เคยเห็นขั้นตอนนี้
ลงชื่อเข้าใช้เฉพาะ CLI ที่คุณต้องการ ตัวเดียวก็พอ

**Claude Code:**

```sh
sudo jodd-ai-host signin claude
```

Claude Code จะเริ่มทำงาน ทำตามที่มันถาม เมื่อมันแสดงลิงก์สำหรับลงชื่อเข้าใช้
ให้เปิดลิงก์ในเบราว์เซอร์บนอุปกรณ์ใดก็ได้ ลงชื่อเข้าใช้ แล้วนำรหัสที่ได้กลับมาวางใน terminal เมื่อ Claude Code พร้อมรับคำถามแล้ว ให้พิมพ์ `/exit`

**Codex:**

```sh
sudo jodd-ai-host signin codex
```

คำสั่งนี้จะรัน `codex login --device-auth` ซึ่งแสดงลิงก์และรหัส ให้เปิดลิงก์ในเบราว์เซอร์บนอุปกรณ์ใดก็ได้ ลงชื่อเข้าใช้ด้วยบัญชี ChatGPT ของคุณ แล้วกรอกรหัสนั้น

ใน Docker ให้ใช้ `docker compose exec ai-host jodd-ai-host signin claude` และ
`docker compose exec ai-host jodd-ai-host signin codex`

## <a id="connect-from-jodd"></a>เชื่อมต่อจาก Jodd

ใน Jodd คุณกรอกข้อมูลสี่อย่าง [คู่มือการใช้งาน](USER-GUIDE.th.md#set-up-a-server)
อธิบายทีละหน้าจอ

| ช่อง | สิ่งที่ต้องกรอก |
|---|---|
| Server | ที่อยู่ของเครื่อง host ที่อุปกรณ์ของคุณเข้าถึงได้ (ดูด้านล่าง) |
| Port | `22` สำหรับ Linux และ macOS (เว้นแต่ sshd ของคุณใช้พอร์ตอื่น) และ `2222` สำหรับ Docker |
| Username | `jodd-ai` |
| Password | รหัสผ่านของบัญชี |

### <a id="same-wi-fi"></a>Wi-Fi เดียวกัน

ถ้าอุปกรณ์ของคุณกับเครื่อง host อยู่ในเครือข่ายเดียวกัน ให้ใช้ที่อยู่ในเครือข่ายภายในของเครื่อง host ซึ่งตัวติดตั้งพิมพ์ไว้ให้แล้ว ถ้าต้องการดูอีกครั้งบน Linux:

```sh
hostname -I
```

บน Mac:

```sh
ipconfig getifaddr en0
```

สำหรับ Docker ให้ใช้ที่อยู่ของเครื่องที่รัน Docker

### <a id="away-from-home-tailscale"></a>ใช้นอกบ้าน: Tailscale

ถ้าต้องการเข้าถึงเครื่อง host ได้จากทุกที่ ให้ทั้งสองฝั่งอยู่บนเครือข่ายส่วนตัว เช่น
[Tailscale](https://tailscale.com/download) (หรือ VPN อื่นที่คุณไว้ใจ):

1. ติดตั้ง Tailscale บนเครื่อง host แล้วลงชื่อเข้าใช้
2. ติดตั้งแอป Tailscale บนโทรศัพท์หรือคอมพิวเตอร์ของคุณ แล้วลงชื่อเข้าใช้บัญชีเดียวกัน
3. บนเครื่อง host ดูที่อยู่ Tailscale ของเครื่อง:

   ```sh
   tailscale ip -4
   ```

4. ใน Jodd ใช้ที่อยู่นั้น (หรือชื่อ Tailscale ของเครื่อง host) เป็น **Server**

สำหรับ Docker บน VPS ให้ตั้ง `JODD_AI_BIND` ใน `.env` เป็นที่อยู่นั้นด้วย
([Docker](#docker))

**อย่า forward พอร์ต SSH บนเราเตอร์** การเข้าสู่ระบบด้วยรหัสผ่านจะเปิดอยู่ระหว่าง
Setup และ Update และพอร์ต SSH บนอินเทอร์เน็ตถูกสแกนตลอดทั้งวัน

## <a id="check-the-fingerprint"></a>ตรวจ fingerprint

ครั้งแรกที่ Jodd เชื่อมต่อ จะแสดงตัวตนของเครื่อง host เช่น
`SHA256:dswu8P+mdeYHwkh6suxXWrbP9Mj1h0vpaVzXYZssz60 (ED25519)` บนเครื่อง host ให้รัน:

```sh
jodd-ai-host fingerprint
```

ใน Docker (log ของการเริ่มครั้งแรกก็แสดงค่านี้ด้วย):

```sh
docker compose exec ai-host jodd-ai-host fingerprint
```

คำสั่งนี้พิมพ์สองส่วนเดียวกัน แต่สลับลำดับกัน:
`ED25519 SHA256:dswu8P+mdeYHwkh6suxXWrbP9Mj1h0vpaVzXYZssz60` ส่วน `SHA256:`
ต้องตรงกันทุกตัวอักษร ถ้าไม่ตรง อย่าทำต่อ เพราะคุณอาจไม่ได้กำลังคุยกับเครื่อง host ของคุณ

fingerprint จะคงเดิมเมื่อคุณรันตัวติดตั้งซ้ำ รีสตาร์ตเครื่อง host
หรืออัปเกรด Docker container จะเปลี่ยนก็ต่อเมื่อ host key ถูกแทนที่
(ติดตั้งระบบใหม่ หรือลบ volume `state` ของ Docker) เมื่อนั้น Jodd
จะปฏิเสธเครื่อง host จนกว่าคุณจะตั้งค่าใหม่

## <a id="harden"></a>เพิ่มความปลอดภัย

**ปิดการเข้าสู่ระบบด้วยรหัสผ่านหลัง Setup** นับจากนั้น Jodd ใช้ key ของตัวเอง
และไม่ต้องใช้รหัสผ่านอีก:

```sh
sudo jodd-ai-host lock
```

ก่อนกด **Update** ใน Jodd ให้เปิดกลับมาก่อน แล้วปิดอีกครั้งหลังเสร็จ:

```sh
sudo jodd-ai-host unlock
```

ทั้ง `lock` และ `unlock` มีผลเฉพาะบัญชี `jodd-ai` เท่านั้น ใน Docker:
`docker compose exec ai-host jodd-ai-host lock` และ
`docker compose exec ai-host jodd-ai-host unlock`

**Firewall:** อนุญาตเฉพาะพอร์ต SSH และถ้าทำได้ ให้อนุญาตเฉพาะจากเครือข่ายหรือ VPN
ของคุณเอง ใน Docker ให้ใช้ `JODD_AI_BIND` แทน ([Docker](#docker))

**ให้บัญชีเป็นบัญชีธรรมดา** อย่าเพิ่ม `jodd-ai` เข้ากลุ่ม `sudo`, `wheel` หรือ
`admin` และอย่าเก็บไฟล์อื่นไว้ใน home ของบัญชีนี้

**รู้ไว้ว่า Codex อ่านอะไรได้บ้าง** Codex อ่านได้ทุกไฟล์ที่บัญชี `jodd-ai`
อ่านได้ บน Linux หรือ Mac ทั่วไป นั่นมากกว่าแค่ home ของบัญชีเอง: รวมทุกอย่างที่ผู้ใช้ทุกคนอ่านได้ เช่น ไฟล์ส่วนใหญ่ใน `/etc` และ `/Users/Shared` บน Mac รวมถึงโฟลเดอร์ home ของผู้ใช้อื่นบนระบบที่เปิดให้อ่านได้ (Linux หลายระบบเป็นแบบนั้น และ Mac ก็เช่นกัน ดูด้านล่าง)
สิ่งที่ Codex อ่านอาจไปอยู่ในคำตอบที่ส่งกลับมายัง Jodd
ด้วยเหตุนี้ ควรใช้**เครื่องหรือ virtual machine ที่ใช้สำหรับงานนี้อย่างเดียว** หรือ**[วิธี Docker](#docker)** เพราะใน Docker นั้น Codex เห็นเฉพาะสิ่งที่อยู่ใน
container นี่คือเหตุผลด้านความปลอดภัยจริง ๆ ที่ควรเลือก Docker บนเครื่องที่คุณใช้ทำอย่างอื่นด้วย

บน Mac ทุกบัญชีอยู่ในกลุ่ม `staff` โดยค่าเริ่มต้น และ `staff` เข้าโฟลเดอร์ home
ของทุกคนได้ ภายใน home นั้นมีเพียง Desktop, Documents, Downloads, Movies, Music, Pictures และ Library
ที่ปิดไว้ ตัวติดตั้งสำหรับ macOS จึงให้ `jodd-ai` มีกลุ่มของตัวเองแทน `staff`
และปิดโฟลเดอร์ home ของบัญชีนี้ไว้ด้วย สรุปท้ายการติดตั้งจะแสดงโฟลเดอร์ home
ที่ยังเปิดอยู่ พร้อมคำสั่งที่ใช้ปิดแต่ละโฟลเดอร์ ถ้าเป็น Mac ที่คุณใช้งานเองด้วย
ให้ปิดโฟลเดอร์ home ของคุณเอง และอย่าเก็บข้อมูลลับ เช่น API key และไฟล์ `.env`
ไว้ในที่ที่บัญชีอื่นอ่านได้:

```sh
chmod 700 /Users/<you>
```

**ใช้รหัสผ่านยาว ๆ** และไม่ซ้ำกับบัญชีอื่นของคุณ

## <a id="update"></a>อัปเดต

**CLI** รันตัวติดตั้งทางการของทั้งสองตัวอีกครั้ง:

```sh
sudo jodd-ai-host update-clis
```

ใน Docker:

```sh
docker compose exec ai-host jodd-ai-host update-clis
```

ถ้า CLI ตัวใดยังไม่ได้ติดตั้งตอนที่คุณตั้งค่า Jodd ให้กด **Update** ใน Jodd
หลังติดตั้งแล้ว (โดยเปิดการเข้าสู่ระบบด้วยรหัสผ่านไว้) เพื่อให้ Jodd รู้ว่า CLI อยู่ที่ไหน

**ชุดติดตั้งบน Linux และ macOS** ดาวน์โหลดตัวติดตั้งใหม่แล้วรัน ตัวติดตั้งจะเก็บบัญชี รหัสผ่าน การลงชื่อเข้าใช้ของคุณ key ของ Jodd และสถานะ lock ไว้เหมือนเดิม

**Docker** จากโฟลเดอร์ที่มี `compose.yaml`:

```sh
docker compose pull
```

```sh
docker compose up -d
```

fingerprint, CLI, การลงชื่อเข้าใช้ของคุณ, key ของ Jodd และสถานะ lock ยังอยู่ครบ เพราะเก็บไว้ใน volume ทั้งสอง
รหัสผ่านจาก `.env` จะถูกนำไปใช้อีกครั้ง ถ้าไม่มี container ใหม่จะสร้างรหัสผ่านใหม่และแสดงใน `docker compose logs` ซึ่งคุณต้องใช้เฉพาะตอน
Setup หรือ Update ครั้งถัดไปใน Jodd

**ส่วนของ Jodd บนเครื่อง host** เมื่อ Jodd บอกว่าเครื่อง host ต้องอัปเดต ให้เปิดการเข้าสู่ระบบด้วยรหัสผ่าน กด **Update** ใน Jodd แล้ว lock อีกครั้ง

## <a id="remove-everything"></a>ลบทุกอย่างออก

ขั้นแรก กด **Remove** ใน Jodd บนทุกอุปกรณ์ที่ใช้เครื่อง host นี้ ซึ่งจะลบ
key ของ Jodd ทั้งบนเครื่อง host และบนอุปกรณ์

**Docker** จากโฟลเดอร์ที่มี `compose.yaml` คำสั่งนี้ลบ
volume ด้วย ได้แก่ CLI, การลงชื่อเข้าใช้ของคุณ และ host key

```sh
docker compose down -v
```

**Linux** ลบบัญชีพร้อม home ของบัญชี (บน Alpine:
`deluser --remove-home jodd-ai`):

```sh
sudo userdel -r jodd-ai
```

ถ้าตัวติดตั้งเพิ่มบรรทัด `Include /etc/ssh/sshd_config.d/50-jodd-ai.conf`
(และ comment ที่อยู่เหนือบรรทัดนั้น) ไว้บนสุดของ `/etc/ssh/sshd_config` ให้ลบสองบรรทัดนั้นออก จากนั้นลบไฟล์ของชุดติดตั้ง:

```sh
sudo rm -f /etc/ssh/sshd_config.d/50-jodd-ai.conf /etc/jodd-ai-host.conf /usr/local/bin/jodd-ai-host
```

```sh
sudo rm -rf /usr/local/lib/jodd-ai-host /var/lib/jodd-ai-host
```

ตรวจการตั้งค่า SSH (ต้องไม่พิมพ์อะไรออกมา) แล้ว reload sshd ตาม[ขั้นที่ 6 ของ Linux อื่น ๆ](#other-linux):

```sh
sudo sshd -t
```

แพ็กเกจที่ตัวติดตั้งเพิ่มเข้าไป (OpenSSH server, `curl` และอื่น ๆ) ยังคงอยู่

**macOS** (ขั้นตอนเหล่านี้ยังไม่ได้ทดสอบ) ลบบัญชี แล้วลบกลุ่มที่ตัวติดตั้งสร้างให้บัญชีนี้:

```sh
sudo sysadminctl -deleteUser jodd-ai
sudo dseditgroup -o delete jodd-ai
```

ลบไฟล์ของชุดติดตั้งชุดเดียวกับบน Linux และปิด **Remote Login** ใน
**System Settings → General → Sharing** ถ้าคุณไม่ได้ใช้ทำอย่างอื่น

## <a id="troubleshooting"></a>แก้ปัญหา

เริ่มจาก doctor ซึ่งตรวจทุกอย่างที่ Jodd ต้องใช้ และพิมพ์บรรทัด `ok`, `note`
หรือ `FIX` ทุกบรรทัด `FIX` จะบอกว่าต้องทำอะไร

```sh
sudo jodd-ai-host doctor
```

`status` แสดงสิ่งที่ Jodd จะพบ: บัญชี, พอร์ต, fingerprint, การเข้าสู่ระบบด้วยรหัสผ่านเปิดอยู่หรือไม่, CLI แต่ละตัวและเวอร์ชัน และจำนวน key ของ Jodd

```sh
sudo jodd-ai-host status
```

ใน Docker ให้รันคำสั่งเหล่านี้และทุกคำสั่งด้านล่างเป็น
`docker compose exec ai-host jodd-ai-host <command>` โดยไม่ต้องใส่ `sudo`

| อาการ | สิ่งที่ต้องรัน |
|---|---|
| CLI แสดงว่า **not installed** ใน Jodd หรือ Jodd บอกว่ายังไม่ได้ติดตั้ง | `sudo jodd-ai-host update-clis` จากนั้น `unlock`, กด **Update** ใน Jodd แล้ว `lock` |
| ติดตั้ง CLI ไม่สำเร็จ (ไม่มีอินเทอร์เน็ตระหว่างติดตั้ง) | `sudo jodd-ai-host update-clis` |
| **Test connection** ใน Jodd บอกว่า Claude Code ยังไม่ได้ลงชื่อเข้าใช้ | `sudo jodd-ai-host signin claude` |
| Codex ตอบกลับเป็นข้อผิดพลาดเรื่องการลงชื่อเข้าใช้หรือการอนุญาต | `sudo jodd-ai-host signin codex` |
| Jodd บอกว่าเซิร์ฟเวอร์ไม่รับการลงชื่อเข้าใช้ด้วยรหัสผ่าน | การเข้าสู่ระบบด้วยรหัสผ่านถูก lock อยู่: `sudo jodd-ai-host unlock` (lock อีกครั้งหลัง Setup หรือ Update) ถ้าไม่ได้ lock ไว้ `/etc/ssh/sshd_config` อาจมี `AuthenticationMethods publickey` ที่ใช้กับทุกบัญชี: `sudo jodd-ai-host doctor` จะบอกชื่อค่าที่ต้องแก้ |
| Jodd บอกว่าเซิร์ฟเวอร์ปฏิเสธชื่อผู้ใช้หรือรหัสผ่าน | ตรวจว่าชื่อผู้ใช้คือ `jodd-ai` และตั้งรหัสผ่านใหม่ด้วย `sudo jodd-ai-host password` ถ้ารหัสผ่านถูกต้อง `AllowUsers` หรือ `AllowGroups` ของ sshd อาจไม่รวม `jodd-ai`: `sudo jodd-ai-host doctor` จะบอกชื่อค่าที่ต้องแก้ |
| doctor หรือตัวติดตั้งระบุชื่อ `AuthenticationMethods`, `AllowUsers`, `AllowGroups`, `DenyUsers` หรือ `DenyGroups` | แก้ค่านั้นใน `/etc/ssh/sshd_config` ด้วยตัวเองตามที่บรรทัดนั้นบอก ชุดติดตั้งไม่เคยเปลี่ยนค่าเหล่านี้ เพราะมีผลกับทุกบัญชี จากนั้นรัน `sudo jodd-ai-host doctor` อีกครั้ง |
| ลืมรหัสผ่านของ Docker | `docker compose exec ai-host jodd-ai-host password` |
| fingerprint ใน Jodd ไม่ตรงกัน | `jodd-ai-host fingerprint` บนเครื่อง host (Docker: `docker compose exec ai-host jodd-ai-host fingerprint`) ถ้าต่างกันให้หยุด |
| Jodd เข้าถึงเครื่อง host ไม่ได้ | เครื่อง host เปิดและตื่นอยู่หรือไม่? อยู่ Wi-Fi เดียวกันหรือใช้ Tailscale หรือไม่? พอร์ตถูกต้องหรือไม่ (2222 สำหรับ Docker)? จากนั้นรัน `sudo jodd-ai-host doctor` |
| อุปกรณ์ที่ไม่ได้ใช้แล้วยังมี key อยู่ | `sudo jodd-ai-host keys` จากนั้น `sudo jodd-ai-host revoke <id>` |
| doctor บอกว่าติดตั้ง CLI แล้ว แต่ login shell หาไม่เจอ | รันตัวติดตั้งซ้ำ หรือเพิ่มบรรทัดที่ doctor พิมพ์ไว้ลงใน `~/.profile` ของบัญชี |
| doctor บอกว่า sshd ไม่อนุญาตให้ `jodd-ai` เข้าสู่ระบบด้วยรหัสผ่าน | รันตัวติดตั้งซ้ำ หรือตรวจว่า `/etc/ssh/sshd_config` อ่าน drop-in ก่อน ([ขั้นที่ 4](#other-linux)) |

ทุกข้อความที่ Jodd อาจแสดง และสิ่งที่ต้องทำ อยู่ใน[คู่มือการใช้งาน](USER-GUIDE.th.md#messages-and-what-to-do)
