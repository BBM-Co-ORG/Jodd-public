# Disclaimer

Please read this disclaimer carefully before using Jodd.

## No Warranty

Jodd is provided **"AS IS"** and **"AS AVAILABLE"**, without warranty of
any kind, express or implied. The authors and contributors make no
guarantees about:

- The accuracy, completeness, or reliability of the software.
- That the software will be uninterrupted, error-free, or free of
  security vulnerabilities.
- That data synchronized through the software will not be lost,
  duplicated, mis-routed, or corrupted.

By installing, building, or running Jodd you accept all risk of using it.

## Beta Software — Back Up Your Data

Jodd is **pre-1.0 software** and has not been hardened by long-term
production use. The synchronization model edits real messages in your
Gmail account.

**Before using Jodd against an account that contains data you care
about, back it up.** A simple approach: in Gmail, select all messages
under the `Notes` label, choose "Forward as attachment" to an archive
address, or export via [Google Takeout](https://takeout.google.com/).
On the Apple side, your iPhone/Mac maintains its own copy in
iCloud — but only for as long as the account stays signed in there.

## Not Affiliated with Apple, Google, or Microsoft

Jodd is an **independent open-source project**. It is not affiliated
with, endorsed by, sponsored by, or in any way officially connected to
Apple Inc., Google LLC, or Microsoft Corporation.

Jodd interoperates with these services by using their **public,
documented APIs** (Gmail REST API today; Microsoft Graph planned) and
the publicly observable storage format used by Apple Notes to
synchronize notes with third-party email accounts. No proprietary
protocols are reverse-engineered.

"Apple Notes" is a trademark of Apple Inc. "Gmail" is a trademark of
Google LLC. References to these trademarks are nominative — they
describe what Jodd interoperates with, nothing more.

## Your Data, Your Credentials

Jodd uses **your own** OAuth 2.0 credentials, obtained directly from
Google by you when you sign in. Access and refresh tokens are stored
**only on your device** — refresh tokens in the OS keychain, access
tokens in process memory. **Nothing is sent to a Jodd-operated server,
because no such server exists.**

Notes themselves are cached in a **local SQLite database** in your
operating system's user data directory:

- **macOS**: `~/Library/Application Support/app.jodd.dev/jodd.sqlite3`
- **Windows**: `%APPDATA%\app.jodd.dev\jodd.sqlite3`
- **Linux**: `~/.local/share/app.jodd.dev/jodd.sqlite3`

The local database is **not encrypted at rest**. Anyone with
read access to your user account on your device can read the
contents. If your device is shared or unattended, use full-disk
encryption (FileVault, BitLocker, LUKS).

## Independent Build

If you build Jodd yourself from this source, you use your own Google
OAuth 2.0 client ID. That client ID is **subject to Google's verification
process**. Until your project is verified by Google, sign-in may show
an "unverified app" warning, and only test users you explicitly list
in the Google Cloud Console will be able to sign in. This is **not a
Jodd bug** — it is Google's safeguard for any third-party OAuth app.

## No Security Audit

Jodd has **not undergone an independent security audit**. While the
code is public and contributions are welcome, you should not rely on
Jodd for storing or accessing data subject to regulatory requirements
(HIPAA, GDPR for organizational data, financial records, legal
discovery material, classified information, etc.) without performing
your own security review.

## AI-Assisted Development & Cleanroom Origin

A substantial portion of the code, documentation, and configuration
in Jodd was authored with the assistance of **large language model
("LLM") coding tools**, including (at various times) Anthropic Claude
and OpenAI ChatGPT/Codex. The maintainers reviewed, edited, tested,
and integrated the generated output, but the **first draft of much
of the source was produced by AI**.

We disclose this for three reasons: provenance, copyright risk, and
to be clear about what was *not* used.

### What the implementation is derived from

Jodd was built in a **cleanroom-style** fashion. The information
sources used to write the interoperability code were limited to:

1. **Direct observation of message format.** The Apple Notes ↔ Gmail
   wire format is plainly visible in any Gmail account that has Notes
   sync enabled: an authenticated user can read their own messages
   under the `Notes` label and inspect the custom headers
   (`X-Uniform-Type-Identifier`, `X-Universally-Unique-Identifier`,
   `X-Mail-Created-Date`) and the HTML body wrapper Apple writes. This
   is **observation of one's own data via a public API**, not reverse
   engineering of a proprietary protocol.
2. **Public, documented APIs.** The Gmail REST API and Microsoft Graph
   API are publicly documented by Google and Microsoft respectively;
   the Tauri 2, Svelte, and `rusqlite` APIs are publicly documented by
   their projects.
3. **Public standards.** RFC 5322 (Internet Message Format), RFC 7636
   (PKCE), RFC 6749 (OAuth 2.0), the WHATWG HTML spec, and the
   SQLite documentation.
4. **Open-source reference code** under permissive licenses, used as
   inspiration for patterns (never copied verbatim).

The maintainers have **not** inspected, decompiled, or studied any
proprietary Apple source code, the Apple Notes iOS/macOS application
binary, any Apple-internal documentation, or any source covered by a
non-disclosure agreement. The synchronization logic is an
**interoperability implementation**: a re-derivation of behavior
visible through observable inputs and outputs. Where applicable, this
falls within the kind of interoperability work that is recognized as
legitimate under, for example, Section 1201(f) of the U.S. DMCA and
Article 6 of the EU Software Directive (2009/24/EC). We are not
lawyers; if you are redistributing Jodd commercially in a jurisdiction
where the law in this area is unsettled, take your own legal advice.

### LLM output and third-party copyright

LLMs are trained on large corpora that include code under many
licenses. When such a model generates code, it is **possible**, in
theory, that fragments of training data could be reproduced.
Detection of such reproduction is an open research problem.

The maintainers have taken reasonable steps — review by humans,
preference for idiomatic standard-library patterns, avoidance of
verbatim long-form snippets without attribution — but we **cannot
warrant** that every line of AI-generated code is free of resemblance
to a copyrighted training-set source.

The AI providers' own terms address ownership but **not third-party
claims**:

- **Anthropic** (Claude): under the
  [Commercial Terms of Service](https://www.anthropic.com/legal/commercial-terms)
  and the
  [Consumer Terms](https://www.anthropic.com/legal/consumer-terms),
  Anthropic assigns to the customer all of Anthropic's right, title,
  and interest *(if any)* in and to Outputs, conditional on the
  customer's compliance with Anthropic's usage policies.
- **OpenAI** (ChatGPT/Codex/API): under the
  [Terms of Use](https://openai.com/policies/row-terms-of-use/),
  OpenAI assigns to the user all of OpenAI's right, title, and
  interest *(if any)* in and to Output, subject to the user's
  compliance with the terms.

In both cases the phrase **"if any"** is load-bearing: the providers
transfer whatever rights they hold, but **do not represent or warrant
that they hold all the rights necessary** to grant a clean title to
the Output, and do not indemnify the user against third-party
infringement claims arising from the Output. (Some enterprise tiers
offer indemnification — Anthropic Enterprise, OpenAI Business — but
the indemnification scope is bounded by the relevant agreement, and
those agreements do not flow downstream to redistributors.)

**Consequence for you, the user of Jodd:**

- If you only **use** Jodd as an end user, this is unlikely to ever
  matter to you in practice.
- If you **redistribute** Jodd, **fork** it, or **incorporate its
  code into your own product**, you accept this provenance situation
  as part of accepting the [Apache License 2.0](LICENSE). The license
  is granted by the maintainers under the terms of section 2 of the
  Apache License, but the maintainers make no warranty under section
  7 — including no warranty of non-infringement.
- If a third-party copyright claim is ever raised against a portion
  of this codebase that traces to AI-generated origin, the
  maintainers will cooperate in good faith on rewriting or removing
  the affected portion, but cannot offer indemnification.

### Documentation and design content

The same disclosure applies to written content (this document, the
README, code comments, error messages) and to architectural diagrams:
much was drafted by an LLM and then edited. We have written these
docs in our own voice and reviewed them for accuracy, but the same
"trained on public corpora" caveat applies in principle.

### How to verify

This source tree is fully public. If you have a concern about a
specific file or function, you can read it; if you suspect a
particular AI-origin fragment resembles an existing copyrighted work
known to you, please report it via the channels in
[SECURITY.md](SECURITY.md) (security-adjacent — please don't file a
public issue first) and we will address it.

## Limitation of Liability

To the maximum extent permitted by applicable law, in no event shall
the authors, contributors, or copyright holders of Jodd be liable for
any claim, damages, or other liability, whether in an action of
contract, tort, or otherwise, arising from, out of, or in connection
with Jodd or the use or other dealings in Jodd — including but not
limited to lost data, lost productivity, or any indirect, incidental,
special, or consequential damages.

This disclaimer is in addition to, and not a replacement for, the
warranty disclaimer and limitation of liability sections of the
[Apache License 2.0](LICENSE) under which Jodd is distributed.
