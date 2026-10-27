<script lang="ts">
  // Settings (spec 2026-10-20 §4.3): one sectioned surface with one save
  // model, replacing the old AppSettings overlay and the per-account
  // AccountSettings dialog. Opened by any caller through `settingsRoute`
  // (stores/ui.ts), which also deep-links: `ai`, `accounts/<id>`,
  // `advanced/oauth`.
  //
  // Desktop and tablet: one Modal, size xl — the section list on the left, the
  // page on the right, the page's Save in the footer. Phone: the section list
  // is the root page, and each section (and an account under Accounts) is its
  // own page on top, with ‹ back and Save in its header. Every Modal owns its
  // Android Back entry, so Back walks back one page at a time.
  //
  // One save model: a page whose sections have text fields exports forms();
  // the page is dirty when any form is, its one Save runs saveAll() (dirty
  // forms in order, stopping at the first failure, shown above that form),
  // and leaving it dirty — closing, Back, switching section — goes through
  // Modal's guard: Save / Discard / Keep editing.
  import { untrack } from 'svelte';
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';
  import { settingsRoute } from '../stores/ui';
  import { isAndroid } from '../stores/platform';
  import { androidLayoutMode } from '../stores/viewport';
  import { SETTINGS_SECTIONS, parseSettingsRoute, sectionLabel, type SettingsSection } from '../settings/route';
  import { saveAll, type PageSaveError, type SectionForm } from '../settings/sectionForm';
  import GeneralPage from './settings/GeneralPage.svelte';
  import AccountsPage from './settings/AccountsPage.svelte';
  import AccountPage from './settings/AccountPage.svelte';
  import BackupPage from './settings/BackupPage.svelte';
  import AiPage from './settings/AiPage.svelte';
  import PrivacyPage from './settings/PrivacyPage.svelte';
  import SpeechPage from './settings/SpeechPage.svelte';
  import SharePage from './settings/SharePage.svelte';
  import AdvancedPage from './settings/AdvancedPage.svelte';

  /**
   * Stated once in the UI (spec §4.3), above the section list: the one-line
   * help, then the rest behind Learn more (spec §4.4).
   */
  const SAVE_MODEL = 'Choices apply at once. Pages with text fields have one Save button.';
  const SAVE_MODEL_MORE =
    "You can't leave such a page with unsaved changes without choosing Save or Discard.";

  type Page = { forms?: () => SectionForm[] };

  const route = $derived($settingsRoute);
  const isOpen = $derived(route !== null);
  const parsed = $derived(route === null ? null : parseSettingsRoute(route));
  const section = $derived<SettingsSection | null>(parsed?.section ?? null);
  const sub = $derived(parsed?.sub ?? null);
  const accountSub = $derived(section === 'accounts' ? sub : null);

  // Phone or not is decided once per opening, from the same gate Modal uses:
  // a rotation must not swap the structure under a page holding edits.
  const phone = $derived(isOpen ? untrack(() => $isAndroid && $androidLayoutMode === 'phone') : false);

  // Desktop shows a page even for the bare list route.
  const desktopSection = $derived<SettingsSection>(section ?? 'general');

  let sectionPage: Page | undefined = $state();
  let subPage: Page | undefined = $state();
  let modal: Modal | undefined = $state();
  let accountModal: Modal | undefined = $state();

  const formsOf = (p: Page | undefined): SectionForm[] => p?.forms?.() ?? [];
  const sectionHasForms = $derived(formsOf(sectionPage).length > 0);
  const sectionDirty = $derived(formsOf(sectionPage).some((f) => f.isDirty()));
  const subHasForms = $derived(formsOf(subPage).length > 0);
  const subDirty = $derived(formsOf(subPage).some((f) => f.isDirty()));
  // Desktop: the right column is the account page when one is open.
  const activeHasForms = $derived(accountSub ? subHasForms : sectionHasForms);
  const activeDirty = $derived(accountSub ? subDirty : sectionDirty);

  let saving = $state(false);
  let saveError = $state<PageSaveError>(null);
  let inFlight: Promise<void> | null = null;

  // A failure belongs to the page it happened on.
  $effect(() => {
    void route;
    untrack(() => { saveError = null; });
  });

  /**
   * The page Save. Saves are local IPC (config files, never the remote); the
   * page waits only to report a failure honestly. Throws that failure, so the
   * guard's Save keeps the page open and says why.
   */
  function savePage(page: Page | undefined): Promise<void> {
    if (inFlight) return inFlight;
    const run = (async () => {
      saving = true;
      saveError = null;
      try {
        const result = await saveAll(formsOf(page));
        if (!result.ok) {
          saveError = { index: result.index, message: result.message };
          throw new Error(result.message);
        }
      } finally {
        saving = false;
      }
    })().finally(() => {
      if (inFlight === run) inFlight = null;
    });
    inFlight = run;
    return run;
  }

  // The Save button: the failure is already shown above its form.
  function pressSave(page: Page | undefined): void {
    savePage(page).catch(() => {});
  }

  // Cmd/Ctrl+Enter saves the page, as the old Account settings window did.
  function onPageKey(e: KeyboardEvent, page: Page | undefined, hasForms: boolean): void {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && hasForms) {
      e.preventDefault();
      pressSave(page);
    }
  }

  /** Desktop: move to another page, through the dirty guard. False if the user stayed. */
  async function go(to: string): Promise<boolean> {
    if (modal && !(await modal.confirmLeave())) return false;
    settingsRoute.set(to);
    return true;
  }

  /** Phone: leave the account page (its own Modal) through its dirty guard. */
  async function leaveAccountPage(to: string): Promise<boolean> {
    if (accountModal && !(await accountModal.confirmLeave())) return false;
    settingsRoute.set(to);
    return true;
  }
</script>

{#snippet sectionList(onpick: (id: SettingsSection) => void, current: SettingsSection | null)}
  <p class="help">{SAVE_MODEL}</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>{SAVE_MODEL_MORE}</p>
  </details>
  <ul class="sections">
    {#each SETTINGS_SECTIONS as s (s.id)}
      <li>
        <button
          type="button"
          class="section-btn"
          class:active={current === s.id}
          aria-current={current === s.id ? 'page' : undefined}
          onclick={() => onpick(s.id)}
        >
          <span class="section-label">{s.label}</span>
          {#if phone}<span class="chev"><Icon name="chevron-right" size={12} /></span>{/if}
        </button>
      </li>
    {/each}
  </ul>
{/snippet}

{#snippet pageFor(s: SettingsSection)}
  {#if s === 'general'}
    <GeneralPage />
  {:else if s === 'accounts'}
    <AccountsPage />
  {:else if s === 'backup'}
    <BackupPage />
  {:else if s === 'ai'}
    <AiPage bind:this={sectionPage} {saveError} />
  {:else if s === 'privacy'}
    <PrivacyPage />
  {:else if s === 'speech'}
    <SpeechPage bind:this={sectionPage} {saveError} />
  {:else if s === 'share'}
    <SharePage />
  {:else if s === 'advanced'}
    <AdvancedPage bind:this={sectionPage} {sub} {saveError} />
  {/if}
{/snippet}

{#snippet unsavedHint()}
  {#if activeDirty}<span class="unsaved" role="status">Unsaved changes</span>{/if}
{/snippet}

{#if isOpen}
  {#if phone}
    <!-- Root: the section list. Siblings, in this order, so each page opens
         (and owns its Back entry) above the one before it. -->
    <Modal title="Settings" phone="page" onclose={() => settingsRoute.set(null)}>
      {@render sectionList((id) => settingsRoute.set(id), null)}
    </Modal>
    {#if section}
      {#key section}
        <Modal
          title={sectionLabel(section)}
          phone="page"
          parentLabel="Settings"
          dirty={sectionDirty}
          onsave={() => savePage(sectionPage)}
          primary={sectionHasForms
            ? { label: 'Save', onclick: () => pressSave(sectionPage), disabled: !sectionDirty, busy: saving }
            : undefined}
          onclose={() => settingsRoute.set('')}
        >
          <div class="page" role="presentation" onkeydown={(e) => onPageKey(e, sectionPage, sectionHasForms)}>
            {@render pageFor(section)}
          </div>
        </Modal>
      {/key}
      {#if accountSub}
        {#key accountSub}
          <Modal
            bind:this={accountModal}
            title="Account settings"
            phone="page"
            parentLabel="Accounts"
            dirty={subDirty}
            onsave={() => savePage(subPage)}
            primary={subHasForms
              ? { label: 'Save', onclick: () => pressSave(subPage), disabled: !subDirty, busy: saving }
              : undefined}
            onclose={() => settingsRoute.set('accounts')}
          >
            <div class="page" role="presentation" onkeydown={(e) => onPageKey(e, subPage, subHasForms)}>
              <AccountPage bind:this={subPage} accountId={accountSub} {saveError} {saving} leave={leaveAccountPage} />
            </div>
          </Modal>
        {/key}
      {/if}
    {/if}
  {:else}
    <Modal
      bind:this={modal}
      title="Settings"
      size="xl"
      dirty={activeDirty}
      onsave={() => savePage(accountSub ? subPage : sectionPage)}
      primary={activeHasForms
        ? { label: 'Save', onclick: () => pressSave(accountSub ? subPage : sectionPage), disabled: !activeDirty, busy: saving }
        : undefined}
      footer={activeHasForms ? unsavedHint : undefined}
      onclose={() => settingsRoute.set(null)}
    >
      <div class="columns">
        <nav class="nav" aria-label="Settings sections">
          {@render sectionList((id) => void go(id), desktopSection)}
        </nav>
        <div
          class="page"
          role="presentation"
          onkeydown={(e) => onPageKey(e, accountSub ? subPage : sectionPage, activeHasForms)}
        >
          {#if accountSub}
            <button type="button" class="crumb" onclick={() => void go('accounts')}>‹ Accounts</button>
            <h3 class="page-title">Account settings</h3>
            {#key accountSub}
              <AccountPage bind:this={subPage} accountId={accountSub} {saveError} {saving} leave={go} />
            {/key}
          {:else}
            <h3 class="page-title">{sectionLabel(desktopSection)}</h3>
            {#key desktopSection}
              {@render pageFor(desktopSection)}
            {/key}
          {/if}
        </div>
      </div>
    </Modal>
  {/if}
{/if}

<style>
  .columns {
    display: grid;
    grid-template-columns: 180px minmax(0, 1fr);
    gap: var(--space-6);
    align-items: start;
  }
  .nav {
    position: sticky;
    top: 0;
  }
  .help {
    margin: 0 0 var(--space-3);
    font-size: var(--size-xs);
    line-height: var(--leading);
    color: var(--text-muted);
  }
  .sections {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-0-5);
  }
  .section-btn {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    min-height: var(--hit-target-desktop);
    padding: var(--space-1-5) var(--space-2);
    border: 0;
    border-radius: var(--radius);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: var(--size);
    text-align: left;
    cursor: pointer;
  }
  .section-btn:hover { background: var(--surface-hover); }
  .section-btn:focus-visible { box-shadow: var(--ring-focus); }
  .section-btn.active {
    background: var(--accent-tint);
    color: var(--accent-on-tint);
    font-weight: 600;
  }
  :global(.modal.touch) .section-btn { min-height: var(--touch-target); }
  .section-label { flex: 1 1 auto; min-width: 0; }
  .chev { flex: none; color: var(--text-muted); display: inline-flex; }
  .page { min-width: 0; }
  .page-title {
    margin: 0 0 var(--space-3);
    font-size: var(--size-lg);
    font-weight: 600;
  }
  .crumb {
    padding: 0;
    margin: 0 0 var(--space-2);
    border: 0;
    background: none;
    color: var(--accent-action);
    font: inherit;
    font-size: var(--size-sm);
    cursor: pointer;
  }
  .unsaved {
    font-size: var(--size-sm);
    color: var(--text-muted);
  }
</style>
