# Settings › Data & AI: one home for data and AI governance (2026-11-11)

Source: a review of Settings on 2026-11-11 (inventory in the side-session notes,
`settings-governance-review.md`). The owner asked that "configurations/settings
and operations related to data and ai governance have dedicated settings, so we
can focus and be explicit about it", and chose **Option A**: one Data & AI page.
The three details left open in that review are decided below (D1, D2, D6) and
can be changed at spec review.

## The problem

Governance is spread over seven of the nine Settings pages and every account
page:

- The per-account AI permission ("Allow this account's data in Jodd-managed
  AI") is a checkbox inside each account's AI provider form
  (`LlmProviderSettings.svelte:479`).
- Hidden folders and AI receipts are on Privacy (`PrivacyPage.svelte`).
- AI limits and automatic enrichment are on AI (`AiPage.svelte:43`).
- Ask Jodd history is on General (`GeneralPage.svelte:131-153`).
- App logs are on Advanced › Diagnostics (`AdvancedPage.svelte:345-377`).
- Nine channels that send data off the device (AI provider, AI host, speech to
  text, followed links, YouTube comments, linked TVs, backups, notes on disk,
  the Android update check) are set on six different pages, and no page lists
  them together.

Five findings from the review are not about placement but come with it:

1. **Every one of these choices is per device.** `accounts.json` and
   `mcp_write_scope.json` live on the device. Denying AI or hiding a folder on
   the Mac does not change the tablet. Nothing says so.
2. **Masking has no UI.** It is always on (`privacy/detect.rs`, spec
   2026-10-08 D7), and PRODUCT.md principle 5 names it, but no screen
   mentions it.
3. **Copy already points to the wrong place.** For example:
   - `AskJoddModal.svelte` says "App Settings" and "Account Settings".
   - `LessonExtractModal.svelte:166` sends the user to "Settings → Links" for
     TVs, which are on Share.
   - The homepage names "Settings → Agent workspace" for hidden folders.
4. **App logs are on by default** and hold account e-mail addresses and folder
   paths. The switch sits under Advanced.
5. **The notes-on-disk copy is unencrypted Markdown** outside the SQLCipher
   database. It is shown as a Claude Code setup step, not as a place the notes
   now also live.

## The split

- **AI** keeps the *how*: which provider, the AI host, and connecting agents
  (agent workspace, notes on disk, skills).
- **Data & AI** holds the *what*: what is allowed, what is limited, what is
  kept, and where data goes.
- **Speech to text, Links, Share and Backup** keep their setup forms. Data & AI
  shows their state and links to them.

## Decisions

| # | Question | Decision |
|---|---|---|
| D1 | Name and route | **Data & AI**, route `data`. It takes Privacy's place in the section list: General · Accounts · Backup · AI · **Data & AI** · Speech to text · Links · Share to Jodd · Advanced. `privacy` stays as an alias that opens `data`. |
| D2 | The allow switch: move or mirror | **Move.** Data & AI owns it. The account page shows a status line and a link instead. One switch in two places would be two save models (instant here, page Save there) for one value. |
| D3 | How the switch saves | **At once**, like every other switch. On calls the existing `allow_ai_for_account`. Off calls a new `deny_ai_for_account`, built the same way: decide first (a no-op cancels nothing), then `invalidate_ai`, then mutate in memory, save, and roll back on failure. The UI moves at once and rolls back on refusal. |
| D4 | The account form after the move | `LlmProviderSettings` (account scope) **stops sending `data_allowed`**. Rust `preserve_permission` already keeps the stored value when the field is absent, so saving a provider can never change the permission. Choosing Disabled still forces `Some(false)`, as today. |
| D5 | An account whose provider is Disabled | Its switch is off and disabled. Help line: "AI is Disabled for this account." Link to its account page. `allow_ai_for_account` already refuses this case (`ALLOW_REFUSED_DISABLED`). |
| D6 | App logs | **Move** the whole log block (switch, path, size, Reveal / Copy path, Clear) to Records. Advanced › Diagnostics keeps "data since" and gains one line linking to Records. |
| D7 | The overview reads the credential store? | **Never** (gotcha #15). Every row comes from a flag file or memory: `get_app_llm_config`, `get_llm_settings`, `get_stt_config`, `get_link_settings`, `get_youtube_key_status`, `list_tvs`, `backup_destinations`, `agent_workspace_status`, `agent_files_status`, `agent_skills_view`, `list_agent_cli_presets` (static), plus the `appUpdate.ts` store. A test pins this allowlist. |
| D8 | Masking | One fixed line, always shown. No setting (spec 2026-10-08 D7 stands). |
| D9 | Per device | One fixed line under the page title: "These choices apply on this device. Your other devices keep their own." Syncing them is out of scope. |
| D10 | Stale copy | Fix every in-app path this change moves, plus the ones already wrong (finding 3). Changed strings use "›". The homepage gets its own PR (Delivery). |
| D11 | Hidden governance facts | **Visible, never disclosed** (the owner, 2026-11-11: Jodd should practise being explicit about this). See "The Explicit Rule" below: no governance fact or option sits in a collapsed section, behind Learn more, or off a platform's screen. |

## The Explicit Rule: what must never be hidden

The owner added this requirement during spec writing: Jodd should practise
being explicit about governance, so the user stays aware of it. Settings'
copy rule (spec 2026-10-20 §4.4) puts everything past one line under Learn
more. For governance, that rule is reversed.

**A fact must be visible on screen when it says any of these:**

1. **What leaves the device, and to whom.**
2. **What is kept, and where.**
3. **What is not protected:** not encrypted, not masked, cannot be recalled,
   or a tool that can read files by itself.
4. **A default that is on.**
5. **A limit on a promise:** for example, per device only, or external agents
   following other rules.

Visible means:
- not under Learn more;
- not inside a collapsed `<details>`;
- not left off a platform's screen. A feature this device lacks gets a row
  that says "Not on this device", not silence.

Learn more keeps only mechanics: how to set something up, why it works the way
it does, retention and quota arithmetic. Each visible fact is still one short
line (guard limit 110 characters), so a long fact becomes two lines, not one
long one.

### Audit: what is hidden today, and the decision

| # | Today | Hidden how | Decision |
|---|---|---|---|
| E1 | AI limits, including the **Automatically suggest folders and related links** switch, which runs AI without a click (`AiLimits.svelte:46`) | whole section collapsed | **Open.** The enrichment switch comes first, with its state visible. The numbers follow. The planning-unit arithmetic stays under Learn more. |
| E2 | AI receipts, with retention, Delete all and Export (`AiReceipts.svelte:56`) | whole section collapsed | **Open.** Retention, Delete all and Export are visible. Only the run list itself may fold ("Show N receipts"). |
| E3 | "No note text, prompts, answers or conversation history" in receipts (`AiReceipts.svelte:59`) | Learn more | **Visible** (what is kept). |
| E4 | Hidden folders per account (`PrivacyPage.svelte:70`) | collapsed; the count is shown only when > 0 | **Visible names:** "Hidden: Finance, Health" or "No folders hidden". Only the checkbox list folds ("Choose folders"). |
| E5 | The allow switch covers "titles, tags and folders" (`LlmProviderSettings.svelte:484`) | Learn more | **Visible:** "Covers note text, titles, tags and folder names." |
| E6 | "External MCP clients have separate access rules" (same block) | Learn more | **Visible**, on the AI agents row. |
| E7 | An agent CLI "can read files on your computer by itself. Masking and hidden folders … cannot stop the tool" (`cliPrivacy.ts`, rendered split at `LlmProviderSettings.svelte:470`) | second half under Learn more | **Visible in full**, on the AI page and as a warning line on the overview's AI provider row when the preset's file access is `enabled` or `unknown` (read from `list_agent_cli_presets`, a static list). The same applies to the argv note and the AI-host variant. |
| E8 | Notes on disk: "The copy is not encrypted, and only the accounts you choose are in it" (`AgentFilesSettings.svelte:120`) | Learn more | **Visible**, on the AI page and on the overview row. |
| E9 | Backups are plain files, said only in the add-folder confirm (`BackupPage.svelte:42-45`); "Removing a folder only makes Jodd forget it; the backups stay" (`:74`) | dialog only, and Learn more | **Visible** on Backup and on the overview row. |
| E10 | App logs are on by default and trimmed past 20 MB (`AdvancedPage.svelte:348-357`) | under Advanced, and Learn more | **Visible** in Records: the switch state, what is logged, the size, the trim. |
| E11 | Ask Jodd history "saves your questions and answers" (`GeneralPage.svelte:138`) | Learn more | **Visible** in Records: "Keeps your questions and answers on this device. Never synced." |
| E12 | Links: "commenters' names are never saved"; Jodd "never uses your accounts" to read comments (`LinksPage.svelte:23`) | Learn more | **Visible**, one line on Links. The YouTube key setup steps stay under Learn more. |
| E13 | Masking of secrets and personal data | not shown anywhere | **Visible** (D8). "Names and addresses are not detected" and "Audio for speech to text is not masked" are visible too (rule 3). |
| E14 | Choices are per device | not shown anywhere | **Visible** (D9). |
| E15 | Desktop-only channels (notes on disk, skills, jodd-mcp) on Android (the update check was Android-only too until #318; it is now a row on every platform) | the row is absent | **Row shown:** "Not on this device." |

These stay under Learn more (mechanics only):
- receipt retention arithmetic (`AiReceipts.svelte:71`)
- the "unknown usage is not zero" note (`:101`)
- the CLI JSON-unwrap note
- the credential-store prompt notes
- the Test timing notes
- speech-to-text's platform list (`SpeechPage.svelte:20`)
- the update installer steps (`GeneralPage.svelte:166`)
- the "data since" explanation
- the OAuth setup steps
- the Share setup notes

**DESIGN.md** gains this as a named rule under *Learn more*, so later pages
inherit it: "The Explicit Rule: what leaves, what is kept, what is
unprotected, defaults that are on, and limits on a promise are never behind
Learn more."

## The page, top to bottom

Every help line obeys the copy rule: one line of at most about 90 characters
(guard limit 110), with more under one Learn more.

### 1. Where your data goes

A read-only list. Each row has a name, its state, and a **Change** link to the
page where it is set. A row whose channel is off says so plainly; nothing is
hidden because it is off.

| Row | State shown | Change → | Platforms |
|---|---|---|---|
| AI provider | "Not set up" · the CLI preset name ("Claude Code on this device") · HTTP origin ("api.openai.com") · AI host ("AI host user@host"). Second line: "N of M accounts allow their data." If any account picked its own provider: "K accounts use their own provider." If a CLI in use can read files by itself, its warning line (E7) | `ai` | all |
| Speech to text | "Off. No audio leaves this device." · "Audio goes to host" (plus "(not encrypted)" when not https) | `speech` | all |
| Followed links | "Off" · "On for N accounts" | `links` | all |
| YouTube comments | "Off" · "Key saved. Asks Google's YouTube API." | `links` | all |
| Linked TVs | "None" · "N linked. Asks YouTube." | `share` | all |
| Backups | "None" · "N folders. Plain files, not encrypted. Removing a folder keeps its backups." | `backup` | all |
| Notes on disk | "Off" · "On. Plain Markdown in path, not encrypted." | `ai` | desktop; Android row says "Not on this device." |
| Skills for Claude Code | "None" · "N published to ~/.claude/skills" | `ai` | desktop; Android: "Not on this device." |
| AI agents (jodd-mcp) | "Agent workspace in account" · "Not set up". Fixed lines: "Registered agents read every active account except hidden folders." "They follow their own rules, not the allow switch below." | `ai` | desktop; Android: "Not on this device." |
| Update check | "Once a day. Asks GitHub; sends no notes." · "Off" | `general` | Android; desktop: "Not on this device." (the updater redesign may change this row) |

Under the list, all visible (E13):
- "Before any AI request, passwords, keys, e-mail addresses and phone, ID,
  card, bank and passport numbers are replaced with placeholders. Always on."
- "Names and addresses are not detected. Audio for speech to text is not
  masked."

A row whose status call fails shows "Unknown" with no Change link guess. The
failure is logged to the console. It must not hide the row.

### 2. AI access by account

One block per active account:
- The account's name.
- The **Allow this account's data in Jodd-managed AI** switch (D2–D5).
- Its hidden folders by name, always visible ("Hidden: Finance, Health" or
  "No folders hidden", E4). Under it, the checkbox list from
  `PrivacyPage.svelte`, folded as "Choose folders". The toggle logic and the
  `mcp_write_scope.json` read error move unchanged.

Visible lines under the heading:
- "Jodd's AI features and agents never see notes in hidden folders."
- "Allowing covers note text, titles, tags and folder names. Data already sent
  cannot be recalled." (E5)

### 3. Limits

`AiLimits.svelte`, moved from AI. It is **open, not collapsed** (E1), and the
automatic-enrichment switch comes first. It is this page's one form, so the
page gets the Save button and the dirty guard. Nothing else on the page is a
form.

### 4. Records on this device

- **AI receipts:** `AiReceipts history`, moved from Privacy. It is open (E2),
  and "No note text, prompts or answers are kept" is visible (E3). Only the run
  list folds.
- **Ask Jodd history:** keep on/off, and the "Also delete existing history?"
  offer. Moved from General, with the visible line from E11.
- **App logs:** moved from Advanced (D6). The switch label stays. The help line
  becomes "Holds account addresses and folder names, never note text. Trimmed
  past 20 MB." The "never note text" claim must be checked against the
  `log!` calls during implementation; if it does not hold, the line says what
  is logged instead.

## What changes elsewhere

- **AI page:**
  - Its forms become `[llm]` only.
  - The provider help line no longer mentions limits.
  - The CLI privacy notes show in full (E7).
  - Notes on disk shows "not encrypted" visibly (E8).
- **Backup page:** "Plain files, not encrypted" and "Removing a folder keeps
  its backups" become visible lines (E9).
- **Links page:** "Commenters' names are never saved, and Jodd never signs in
  to read comments" becomes a visible line (E12).
- **Account page:**
  - The checkbox is replaced by a line that says, for example, "AI data: Allowed."
  - A **Change in Data & AI** link opens `data` through `leave`.
- **`LlmProviderSettings` (account scope):**
  - "Tick it above" (inherit warning, test hint) becomes "Allow it in Settings › Data & AI".
  - `data_allowed` is dropped from the saved payload (D4).
- **General:** loses the Ask Jodd section.
- **Advanced › Diagnostics:** loses the log block; keeps "data since" and a link.
- **Settings shell:**
  - Pages that link to other pages get a `leave(to)` prop, which goes through
    the dirty guard. Data & AI uses it on both desktop and phone. On phone,
    `leave` calls the section Modal's `confirmLeave` first.
  - The phone section Modal gains `bind:this`.
- **Copy:**
  - `stores/aiScope.ts` `AI_HIDDEN_HINT` becomes "…Unhide it in Settings › Data & AI."
  - `AskJoddModal.svelte`: "App Settings" becomes "Settings › AI".
  - `AskJoddModal.svelte`: "Account Settings" becomes "Settings › Data & AI".
  - Rust `llm/policy.rs:90` `require_account`'s message becomes "…in Settings › Data & AI."
  - `LessonExtractModal.svelte:166` becomes "…in Settings › Share to Jodd."
  - `AI_CONSENT_NEEDED` is unchanged (a Rust test pins it).

## Keyboard, screen reader, phone

- The overview is a list (`<ul>`). Each Change control is a button whose
  accessible name includes the row, for example "Change speech to text".
- Each switch keeps its `<label>`.
- On phone the page is one column. The section rows stack name, state, Change.
  Touch targets follow the existing `.modal.touch` sizing. Android Back walks
  out through the same Modal entries as today.

## Delivery

- **One Jodd PR:** Rust `deny_ai_for_account` plus the policy copy, the new
  `DataPage.svelte`, the moves, the copy fixes, and tests.
- **Then a Jodd-Homepage PR**, merged before the release that ships this:
  - `privacy.html:328` and `index.html:777`: "Settings → Agent workspace"
    becomes "Settings → Data & AI".
  - `privacy.html:391`: "Settings → Links" becomes "Settings → Share to Jodd".
  - The Updates path is left to the updater redesign.
  - Never pushed to the homepage's main.
- **Coordination:** the updater redesign owns GeneralPage › Updates. This
  change removes only the Ask Jodd block above it. Whichever lands second
  rebases.
- **After merge:** `/graphify --update`, CHANGELOG at the next release, and a
  desktop and phone look at the page.

## Tests

- `route.test.ts`:
  - `data` is in place of `privacy`, labelled "Data & AI".
  - `privacy` parses to `data`.
- `settings.test.ts`: the Privacy, General › Ask history and Advanced › logs
  cases move to `data`. New cases:
  - **Overview rows.** Each row renders its on and off state from mocked
    commands. Desktop-only rows are absent on Android, and the update row is
    absent on desktop.
  - **No keychain.** Opening `data` invokes only the D7 allowlist.
  - **Allow switch.** On calls `allow_ai_for_account` and off calls
    `deny_ai_for_account`. A refusal rolls back only that account. A Disabled
    account's switch is disabled.
  - **Account page.** No checkbox; the status line and link are shown.
    Saving the provider form sends no `data_allowed`.
  - **Page Save.** Limits are saved by the page Save. The AI page's forms are
    `[llm]`.
  - **Change link.** It goes through the dirty guard.
  - **The Explicit Rule.** For each E1–E15 fact, a test renders its page and
    asserts the text is visible: present, and not inside a closed `<details>`.
    The test runs on desktop and on Android, so a missing row fails it.
- `settingsCopyButtons.test.ts`: Copy log path moves to `data`.
- `designGuards.test.ts`:
  - `DataPage.svelte` joins the settings page lists.
  - `PrivacyPage.svelte` leaves them.
  - The Learn more count still passes.
- Rust: `deny_ai_for_account` mirrors the `allow_ai_for_account` tests:
  - The decision comes before `invalidate_ai` (a source-order tripwire).
  - Already denied is a no-op.
  - A failed save rolls back.
  - An unknown id is refused.
- Every test that pins `AI_HIDDEN_HINT` or the moved copy is updated to the
  new text, not loosened.

## Out of scope

- Syncing governance choices across devices (finding 1). The page says they
  are per device; making them follow the user is its own design.
- A masking toggle (spec 2026-10-08 D7).
- The per-account gate on untriggered AI calls (roadmap #0b). When built, it
  belongs in Limits.
- Locking down agent CLI tools (roadmap #10), backup encryption (roadmap #17
  M3), and the updater (another session).
- Editing jodd-mcp's write allowlist from the UI.

## Addendum after the first live look (2026-11-11)

- **D12. Name: Privacy & Data.** "Data & AI" sat next to "AI" and read as a
  second AI page. The page is labelled **Privacy & Data**; the route stays
  `data`, and `privacy` still aliases to it. Copy says "Settings › Privacy & Data".
- **D13. Two groups, one home per setting.** The page mixed controls that act
  in place with overview rows that send you elsewhere. It now has two Section
  Label groups: **Set here** (AI access by account with the masking facts,
  Limits, Records) and **Set on other pages** (Where your data goes, read
  only). A row's link names the page it opens ("Speech to text ›") instead of
  "Change". Settings that need a setup form (URL, key, folder picker, Test)
  stay on their own page: one home per setting, and opening this page still
  reads no credential (D7).
