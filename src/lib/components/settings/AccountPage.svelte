<script lang="ts">
  // Settings › Accounts › one account (`accounts/<id>`). The content moved
  // from the old AccountSettings.svelte dialog: the Gmail labels (or the
  // vault name; other backends have neither), the account's LLM provider, iCloud's diagnostics, Deactivate —
  // and Remove, which the Sidebar's account panel also offers. The labels
  // and the provider are this page's forms, saved by the page Save in
  // display order; a failure stops there and shows above that form.
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import { get } from 'svelte/store';
  import { dupStats } from '../../stores/dupStats';
  import { accounts, accountDisplay, backendLabel, refreshNotes } from '../../stores/notes';
  import DupReviewModal from '../DupReviewModal.svelte';
  import { settingsRoute } from '../../stores/ui';
  import { copyText } from '../../clipboard';
  import LlmProviderSettings from '../LlmProviderSettings.svelte';
  import PromptDialogs from '../PromptDialogs.svelte';
  import SaveError from './SaveError.svelte';
  import BackupNow from './BackupNow.svelte';
  import { refreshBackupState, watchBackupFinished } from '../../backup/state';
  import {
    setAccountActive, removeAccount as removeAccountAction,
    stopWaiting as stopWaitingAction, STOP_WAITING_FAILED,
  } from '../../controllers/accountActions';
  import type { PageSaveError, SectionForm } from '../../settings/sectionForm';

  let {
    accountId,
    saveError = null,
    saving = false,
    leave,
  }: {
    accountId: string;
    saveError?: PageSaveError;
    /** True while the page Save runs: the account actions wait for it. */
    saving?: boolean;
    /**
     * Leave this page for `to` through the dirty guard (Save / Discard /
     * Keep editing). Resolves false when the user chose to stay.
     */
    leave: (to: string) => Promise<boolean>;
  } = $props();

  const account = $derived($accounts.find((a) => a.id === accountId));
  const backendKind = $derived(account?.backend_kind);
  const isLocalFs = $derived(backendKind === 'local_fs');
  // `notes_label` / `meta_label` are Gmail labels: no other backend reads
  // them. An account with no `backend_kind` predates the field and is Gmail
  // (types.ts), and so is an id the store does not hold yet — what this page
  // showed every account before the fields were gated.
  const isGmail = $derived((backendKind ?? 'gmail') === 'gmail');
  const isICloud = $derived(backendKind === 'icloud');
  const isActive = $derived(account?.status === undefined || account?.status === 'active');

  // ── Reauthenticate ─────────────────────────────────────────────────
  //
  // iCloud sessions are a WKWebView cookie jar that Jodd never stores and
  // that Apple rotates and expires on its own (hours, not days). When one
  // dies mid-day the account still shows as configured — the only sign is
  // every CloudKit call failing — and until now the only fix was Remove
  // Account then Add Account again, which also wipes the local cache and
  // restarts the whole cold-start index. This re-runs just the sign-in
  // webview against the SAME account_id, so a broken session is a few
  // clicks, not a full re-onboarding.
  let reauthRunning = $state(false);
  let reauthErr: string | null = $state(null);
  let reauthDone = $state(false);

  async function runReauthenticate() {
    reauthRunning = true;
    reauthErr = null;
    reauthDone = false;
    try {
      await invoke<string>('icloud_reauthenticate', { accountId });
      reauthDone = true;
    } catch (e) {
      reauthErr = String(e);
    } finally {
      reauthRunning = false;
    }
  }

  // ── M2 write census ────────────────────────────────────────────────
  //
  // A one-click diagnostic rather than a devtools incantation. It answers
  // whether iCloud writing is viable on THIS account — how many of its notes
  // Jodd can write, and why not the rest — and it has to be run by the person
  // who owns the account, so it cannot live behind a console snippet they have
  // to be told about.
  //
  // Read-only: it walks the zone and sends nothing.
  let censusRunning = $state(false);
  let censusOut: string | null = $state(null);
  let censusErr: string | null = $state(null);
  // '' until a copy is tried; 'failed' when both clipboard paths refused.
  let censusCopied = $state<'' | 'copied' | 'failed'>('');

  /**
   * `navigator.clipboard` is not reliable inside the Tauri webview, so the
   * button appeared to do nothing (live run); `copyText` (lib/clipboard.ts)
   * falls back to a selection-based copy. It says "Copied" only when a path
   * reported success, and says so when both failed — never silent, never a
   * false "Copied".
   */
  async function copyCensus() {
    censusCopied = (await copyText(censusOut ?? '')) ? 'copied' : 'failed';
    if (censusCopied === 'copied') setTimeout(() => { if (censusCopied === 'copied') censusCopied = ''; }, 2000);
  }

  async function copyReloc() {
    relocCopied = (await copyText(relocOut ?? '')) ? 'copied' : 'failed';
    if (relocCopied === 'copied') setTimeout(() => { if (relocCopied === 'copied') relocCopied = ''; }, 2000);
  }

  // ── The relocation self-test ───────────────────────────────────────
  //
  // A SEPARATE question from the census, and the reason it gets its own
  // button rather than a line in that report: the census measured the note
  // DOCUMENT and answered no, because Apple's per-character CRDT identity is
  // on every note. A move sends the folder and the change tag and no document
  // at all; a folder record has no document to be refused over. Nothing has
  // measured those, so `writes.folders` is false for want of evidence rather
  // than because of any.
  //
  // **This one writes.** The folder is typed rather than picked, and the
  // backend refuses any folder that does not hold exactly one note — so the
  // person running it has to have prepared a throwaway folder on purpose.
  let relocFolder = $state('');
  let relocRunning = $state(false);
  let relocOut: string | null = $state(null);
  let relocErr: string | null = $state(null);
  let relocCopied = $state<'' | 'copied' | 'failed'>('');

  async function runReloc() {
    relocRunning = true;
    relocOut = null;
    relocErr = null;
    relocCopied = '';
    try {
      relocOut = await invoke<string>('icloud_relocation_selftest', {
        accountId,
        folder: relocFolder,
      });
    } catch (e) {
      relocErr = String(e);
    } finally {
      relocRunning = false;
    }
  }

  async function runCensus() {
    censusRunning = true;
    censusOut = null;
    censusErr = null;
    censusCopied = '';
    try {
      censusOut = await invoke<string>('icloud_census', { accountId });
    } catch (e) {
      censusErr = String(e);
    } finally {
      censusRunning = false;
    }
  }

  // The two configurable labels (notes root + metadata root). Loaded
  // from the backend on mount via get_account_settings — the backend
  // returns the effective values (defaults resolved), so we always have
  // concrete strings to render. Edits go back via update_account_settings.
  let notesLabel = $state('');
  let metaLabel = $state('');
  // As last loaded or saved; the fields form is dirty when the inputs differ.
  let savedNotesLabel = $state('');
  let savedMetaLabel = $state('');
  let loaded = $state(false);
  let busy = $state(false);
  let error: string | null = $state(null);

  // Vault name — only for LocalFs accounts. Pre-filled with the current
  // account email (which holds the vault display name). Saved via
  // rename_local_account and updates the accounts store optimistically.
  let vaultName = $state('');
  let savedVaultName = $state('');
  let rootDir: string | null = $state(null);

  // The Rust constants — duplicated here as placeholder text so the user
  // can see "what does empty mean" before they save. Keep in sync with
  // accounts.rs DEFAULT_NOTES_LABEL / DEFAULT_META_LABEL.
  const DEFAULT_NOTES_LABEL = 'Notes';
  const DEFAULT_META_LABEL = 'Notes-Meta';

  // The backup line below shows the shared status; load it and follow
  // `backup-finished` while this page is open.
  onMount(() => {
    void refreshBackupState();
    return watchBackupFinished();
  });

  onMount(async () => {
    try {
      if (isLocalFs) {
        // For LocalFs accounts, derive vaultName from the current account email
        // (which stores the vault display name). No Gmail label settings needed.
        vaultName = account?.email ?? '';
        savedVaultName = vaultName;
        rootDir = account?.root_dir ?? null;
      } else if (isGmail) {
        const cur = await invoke<{ notes_label: string; meta_label: string }>(
          'get_account_settings',
          { accountId },
        );
        notesLabel = savedNotesLabel = cur.notes_label;
        metaLabel = savedMetaLabel = cur.meta_label;
      }
    } catch (e) {
      console.error('get_account_settings failed', e);
      error = String(e);
    } finally {
      loaded = true;
    }
  });

  const fieldsForm: SectionForm = {
    isDirty: () =>
      loaded &&
      (isLocalFs
        ? vaultName !== savedVaultName
        : isGmail && (notesLabel !== savedNotesLabel || metaLabel !== savedMetaLabel)),
    save: saveFields,
  };

  let llm: SectionForm | undefined = $state();

  /** The section-form contract: the labels (or vault name), then the provider. */
  export function forms(): SectionForm[] {
    return llm ? [fieldsForm, llm] : [fieldsForm];
  }

  async function saveFields(): Promise<void> {
    try {
      if (isLocalFs) {
        // Rename the vault: update the display name (email field) on the account.
        const updated = await invoke<{ id: string; email: string; added_at: string; backend_kind: string; root_dir: string | null }>(
          'rename_local_account',
          { accountId, name: vaultName.trim() || vaultName },
        );
        // Update the accounts store so the sidebar re-renders immediately.
        accounts.update((list) =>
          list.map((a) => a.id === accountId ? { ...a, email: updated.email } : a),
        );
        vaultName = savedVaultName = updated.email;
      } else if (isGmail) {
        // Pass exactly what's in the inputs. Empty string is meaningful on
        // the Rust side — it resets the field to the DEFAULT_ constant.
        const updated = await invoke<{ notes_label: string; meta_label: string }>(
          'update_account_settings',
          { accountId, notesLabel, metaLabel },
        );
        notesLabel = savedNotesLabel = updated.notes_label;
        metaLabel = savedMetaLabel = updated.meta_label;
      }
    } catch (e) {
      // Shown above this form by the page (SaveError), so not in `error` too.
      console.error('save settings failed', e);
      throw new Error(String(e));
    }
  }

  // No confirmation: deactivating loses nothing (the account keeps flushing
  // its already-queued work in the background), so there is nothing to
  // confirm. The optimistic flip to `draining` and its rollback are
  // `setAccountActive` in controllers/accountActions.ts, shared with the
  // Sidebar; this keeps the page's own part — the busy flag, the error line,
  // and going back to the account list on success.
  //
  // The leave goes through the dirty guard AFTER the deactivate: Deactivate
  // is the action the user asked for and cannot lose anything, so it is not
  // held hostage to unrelated edits; the guard then decides only what happens
  // to those edits (Save keeps them, Discard drops them, Keep editing stays
  // on this page, where they can still be saved).
  async function deactivate() {
    busy = true;
    error = null;
    try {
      await setAccountActive(accountId, false);
    } catch (e) {
      error = String(e);
      return;
    } finally {
      busy = false;
    }
    await leave('accounts');
  }

  // Stop waiting, as the Sidebar's Inactive group offers it — the phone has
  // no Sidebar account panel (spec 2026-10-20 §5.2), so this page is its only
  // way in. The rule and its wording are accountActions' `stopWaiting`.
  async function stopWaiting() {
    if (!account || !promptDialogs) return;
    const dialogs = promptDialogs;
    busy = true;
    error = null;
    try {
      const outcome = await stopWaitingAction(account, {
        confirm: (title, message) => dialogs.askConfirm(title, message),
      });
      if (outcome === 'failed') error = STOP_WAITING_FAILED;
    } finally {
      busy = false;
    }
  }

  // Duplicate review — the Sidebar's dup pill on desktop; on the phone,
  // which has no pill (§5.2), only here.
  const dups = $derived($dupStats[accountId]?.collapsed ?? 0);
  let reviewingDups = $state(false);
  let dupResult = $state('');
  function onDupsTrashed(count: number) {
    dupResult = count === 0 ? 'Nothing was trashed.' : `Trashed ${count}.`;
    // A fresh list_notes recomputes the count from Gmail (see the Sidebar's
    // onTrashed for why it is not decremented here).
    void Promise.resolve(get(refreshNotes)());
  }

  // Reactivate, as the Sidebar's Inactive group offers it.
  async function reactivate() {
    busy = true;
    error = null;
    try {
      await setAccountActive(accountId, true);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  // Remove: the same action and confirm as the Sidebar's account panel,
  // including the Managed-SSH "also remove Jodd's key" checkbox.
  let promptDialogs: PromptDialogs | undefined = $state();
  let removeRevokeKey = $state(true);
  async function remove() {
    if (!account || !promptDialogs) return;
    const dialogs = promptDialogs;
    busy = true;
    try {
      const outcome = await removeAccountAction(account, {
        confirm: async (title, message, checkboxLabel, options) => {
          removeRevokeKey = true;
          const ok = await dialogs.askConfirm(title, message, checkboxLabel, options);
          return { ok, checked: removeRevokeKey };
        },
        // The Sidebar drops its per-account folder tree here; what Settings
        // holds per account is the duplicate count.
        onRemoved: (id) => dupStats.update(({ [id]: _gone, ...rest }) => rest),
      });
      // No dirty guard: the edits belonged to an account that is gone.
      // Removing the last account signs out, and Settings must not stay open
      // over the sign-in screen.
      if (outcome === 'removed') settingsRoute.set(get(accounts).length === 0 ? null : 'accounts');
    } finally {
      busy = false;
    }
  }
</script>

<PromptDialogs bind:this={promptDialogs} bind:checkboxChecked={removeRevokeKey} />

<div class="account-line">
  {account ? accountDisplay(account) : accountId}
  {#if backendKind}
    <span class="account-line-backend">{backendLabel(backendKind)}</span>
  {/if}
</div>

{#if dups > 0}
  <div class="dups-row">
    <p class="help">{dups} duplicate {dups === 1 ? 'copy' : 'copies'} of notes in this account.</p>
    <button type="button" class="census" onclick={() => { dupResult = ''; reviewingDups = true; }}>Review duplicates</button>
    {#if dupResult}<p class="help" role="status">{dupResult}</p>{/if}
  </div>
{/if}
{#if reviewingDups}
  <DupReviewModal {accountId} onClose={() => (reviewingDups = false)} onTrashed={onDupsTrashed} />
{/if}

{#if !loaded}
  <div class="loading">Loading…</div>
{:else}
  <SaveError error={saveError} index={0} />
  {#if isLocalFs}
    <!-- LocalFs: only show vault name field (no Gmail labels) -->
    <div class="field">
      <label>
        <span class="field-label">Vault name</span>
        <input
          type="text"
          bind:value={vaultName}
          placeholder="My Notes"
          spellcheck="false"
          autocomplete="off"
        />
      </label>
      <span class="help">Display name shown in the sidebar. The folder on disk is not renamed.</span>
      <details class="learn-more">
        <summary>Learn more</summary>
        <p>
          Shown as <code>localfs:&lt;name&gt;</code>. Changing this only affects
          how Jodd labels this folder.
        </p>
      </details>
    </div>

    {#if rootDir}
      <div class="field">
        <span class="field-label">Folder on disk</span>
        <div class="root-dir-display">{rootDir}</div>
      </div>
    {/if}
  {:else if isGmail}
    <!-- Gmail account: show full label config -->
    <div class="field">
      <label>
        <span class="field-label">Notes label</span>
        <input
          type="text"
          bind:value={notesLabel}
          placeholder={DEFAULT_NOTES_LABEL}
          spellcheck="false"
          autocomplete="off"
        />
      </label>
      <span class="help">
        Gmail label this account's notes live under. Keep <code>Notes</code> to
        stay compatible with Apple Notes.
      </span>
      <details class="learn-more">
        <summary>Learn more</summary>
        <p>
          Apple Notes only writes to this label, and changing it loses iPhone/Mac
          interop. Empty resets to default.
        </p>
      </details>
    </div>

    <div class="field">
      <label>
        <span class="field-label">Metadata label</span>
        <input
          type="text"
          bind:value={metaLabel}
          placeholder={DEFAULT_META_LABEL}
          spellcheck="false"
          autocomplete="off"
        />
      </label>
      <span class="help">Top-level Gmail label for Jodd's per-note metadata, such as pins.</span>
      <details class="learn-more">
        <summary>Learn more</summary>
        <p>
          Jodd stores it as small sidecar messages. Apple Notes ignores this label
          entirely. Multiple Jodd installs on the same Gmail account share pins
          through this label — use the same value on every Jodd install of this
          account. Empty resets to default.
        </p>
      </details>
    </div>
  {:else}
    <!-- iCloud, Outlook, SSH: nothing to configure here, and saying so is
         the honest panel — the label fields above are Gmail labels. -->
    <div class="field backend-fields">
      {#if backendKind === 'ssh'}
        {#if account?.ssh_target}
          <span class="field-label">Server</span>
          <div class="root-dir-display">{account.ssh_target}</div>
        {/if}
        {#if account?.root_dir}
          <span class="field-label folder-label">Folder on server</span>
          <div class="root-dir-display">{account.root_dir}</div>
        {/if}
        <span class="help">Notes are files in this folder. There are no labels to set.</span>
      {:else if backendKind === 'icloud'}
        <span class="help">Notes and folders are the ones in your iCloud account. There are no labels to set.</span>
      {:else if backendKind === 'microsoft'}
        <span class="help">Notes are in this account's Outlook Notes folder. There are no labels to set.</span>
      {:else}
        <span class="help">There are no labels to set for this account.</span>
      {/if}
    </div>
  {/if}

  {#if error}
    <div class="error">{error}</div>
  {/if}

  <hr />
  <SaveError error={saveError} index={1} />
  <LlmProviderSettings bind:this={llm} {accountId} />
{/if}

{#if isICloud}
  <hr />
  <button class="census reauth" onclick={runReauthenticate} disabled={reauthRunning}>
    {reauthRunning ? 'Waiting for Apple sign-in…' : 'Reauthenticate'}
  </button>
  <p class="help">Restores an expired iCloud session for this same account.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>
      iCloud sessions expire on their own, sometimes mid-day. If iCloud actions here
      are failing with an auth error, this opens Apple's sign-in page and restores the
      session for this same account — no data is lost or re-downloaded.
    </p>
  </details>
  {#if reauthErr}
    <div class="error">{reauthErr}</div>
  {/if}
  {#if reauthDone}
    <div class="reauth-done">Session restored.</div>
  {/if}

  <hr />
  <button class="census" onclick={runCensus} disabled={censusRunning}>
    {censusRunning ? 'Reading your notes…' : 'Check what Jodd can write here'}
  </button>
  <p class="help">Reads your notes and reports which Jodd can write to iCloud. Sends nothing.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>
      Reads every note in this account and reports how many Jodd can safely write
      back to iCloud, and why not the rest. Sends nothing — it only reads. On a
      large account this takes a minute or two.
    </p>
  </details>
  {#if censusErr}
    <div class="error">{censusErr}</div>
  {/if}
  {#if censusOut}
    <pre class="census-out">{censusOut}</pre>
    <button class="census" onclick={copyCensus}>
      {censusCopied === 'copied' ? 'Copied' : 'Copy report'}
    </button>
    {#if censusCopied === 'failed'}
      <div class="error" role="alert">Could not copy to the clipboard — select the report above and copy it by hand.</div>
    {/if}
  {/if}

  <hr />
  <p class="help warn">
    <strong>Writes to this account.</strong> Use a folder that holds
    <strong>exactly one note you are willing to lose</strong>.
  </p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>
      Tests the things that do not touch a note's contents — moving a note between
      folders, deleting and restoring it, and creating, renaming and removing a
      folder. Type the folder below; that note is the one it moves around. It is
      put back where it started at the end.
    </p>
  </details>
  <input
    class="reloc-folder"
    bind:value={relocFolder}
    placeholder="Notes/My Test Folder"
    disabled={relocRunning}
  />
  <button
    class="census reloc"
    onclick={runReloc}
    disabled={relocRunning || relocFolder.trim() === ''}
  >
    {relocRunning ? 'Writing and re-reading…' : 'Test moves and folders (writes)'}
  </button>
  {#if relocErr}
    <div class="error">{relocErr}</div>
  {/if}
  {#if relocOut}
    <pre class="census-out">{relocOut}</pre>
    <button class="census" onclick={copyReloc}>
      {relocCopied === 'copied' ? 'Copied' : 'Copy report'}
    </button>
    {#if relocCopied === 'failed'}
      <div class="error" role="alert">Could not copy to the clipboard — select the report above and copy it by hand.</div>
    {/if}
  {/if}
{/if}

<!-- Spec §7: an account waiting to be removed (Draining) can still be backed up. -->
{#if isActive || account?.status === 'draining'}
  <hr />
  <span class="field-label">Backup</span>
  <BackupNow {accountId} />
{/if}

<hr />
{#if isActive}
  <button class="deactivate" onclick={deactivate} disabled={busy || saving}>
    {busy ? 'Deactivating…' : 'Deactivate this account'}
  </button>
  <p class="help">Hides it everywhere and stops background sync. Nothing is deleted.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>
      Anything already queued still finishes sending first. Reactivate from the
      Inactive group in the account list.
    </p>
  </details>
{:else if account?.status === 'inactive'}
  <button class="deactivate" onclick={reactivate} disabled={busy || saving}>Reactivate</button>
{:else if account?.status === 'draining'}
  <button class="deactivate" onclick={stopWaiting} disabled={busy || saving}>Stop waiting</button>
  <p class="help">Stops sending this account's remaining edits; they stay on this device.</p>
{/if}
{#if account && !account.pending_removal}
  <button class="remove" onclick={remove} disabled={busy || saving}>Remove account</button>
{/if}

<style>
  .census {
    width: 100%;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
    font-size: var(--size-sm);
  }
  .census:disabled {
    opacity: 0.6;
    cursor: default;
  }
  /* Marked as destructive because it is: unlike the census beside it, this
     button sends writes to the user's real iCloud account. */
  .reloc {
    border-color: var(--danger);
    color: var(--danger);
  }
  .reauth-done {
    color: var(--text-secondary);
    font-size: var(--size-sm);
    margin-bottom: var(--space-3);
  }
  .reloc-folder {
    width: 100%;
    box-sizing: border-box;
    margin-bottom: var(--space-2);
    padding: var(--space-1-5) var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-sunken);
    color: var(--text);
    font-family: var(--font-mono);
    font-size: var(--size-sm);
  }
  /* Monospace and scrollable: the report is aligned columns of counts, and
     re-wrapping it makes it unreadable in exactly the place someone is trying
     to compare two numbers. */
  .census-out {
    margin: var(--space-2) 0;
    padding: var(--space-2);
    max-height: 320px;
    overflow: auto;
    background: var(--surface-sunken);
    border-radius: var(--radius);
    font-family: var(--font-mono);
    font-size: var(--size-xs);
    line-height: var(--leading-tight);
    white-space: pre;
  }
  .dups-row {
    margin-bottom: var(--space-4);
  }
  .account-line {
    color: var(--text-muted);
    font-size: var(--size-sm);
    line-height: var(--leading);
    margin-bottom: var(--space-4);
  }
  /* Named unconditionally beside the address, not only on collision —
     Account.id is now `{backend}:{email}`, so two accounts CAN share an
     address on different backends and the address alone can't tell them
     apart. */
  .account-line-backend {
    display: inline-block;
    margin-left: var(--space-1-5);
    font-size: var(--size-xs);
    color: var(--text-muted);
    background: var(--surface-badge);
    border-radius: var(--radius);
    padding: var(--space-0-5) var(--space-1-5);
  }
  .loading {
    color: var(--text-muted);
    padding: var(--space-6) 0;
    text-align: center;
  }
  .field {
    display: block;
    margin-bottom: var(--space-4);
  }
  .field label {
    display: block;
  }
  .field-label {
    display: block;
    font-weight: 600;
    font-size: var(--size-sm);
    color: var(--text-secondary);
    margin-bottom: var(--space-1-5);
  }
  input[type="text"] {
    width: 100%;
    box-sizing: border-box;
    padding: var(--space-2) var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    font-size: var(--size);
    line-height: var(--leading);
    font-family: inherit;
    background: var(--surface-editor);
    color: var(--text);
  }
  input[type="text"]:focus {
    outline: none;
    border-color: var(--focus);
    background: var(--surface-panel);
  }
  .field .help {
    display: block;
    margin-top: var(--space-1-5);
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
  }
  .folder-label {
    margin-top: var(--space-3);
  }
  .root-dir-display {
    font-family: var(--font-mono);
    font-size: var(--size-xs);
    color: var(--text-secondary);
    background: var(--surface-sunken);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius);
    padding: var(--space-1-5) var(--space-2);
    word-break: break-all;
    line-height: var(--leading);
  }
  .help code {
    background: var(--surface-hover);
    padding: var(--space-0-5) var(--space-1);
    border-radius: var(--radius-sm);
    font-size: var(--size-xs);
  }
  .error {
    background: var(--danger-wash);
    color: var(--danger);
    padding: var(--space-2) var(--space-2);
    border-radius: var(--radius);
    font-size: var(--size-sm);
    margin-bottom: var(--space-3);
  }
  hr {
    border: 0;
    border-top: 1px solid var(--border-subtle);
    margin: var(--space-4) 0;
  }
  .deactivate, .remove {
    width: 100%;
    padding: var(--space-1-5) var(--space-3);
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: none;
    color: var(--text-secondary);
    cursor: pointer;
    font-size: var(--size);
    font-family: inherit;
  }
  .remove {
    margin-top: var(--space-3);
    border-color: var(--danger);
    color: var(--danger);
  }
  .deactivate:hover:not(:disabled), .remove:hover:not(:disabled) {
    background: var(--surface-hover);
  }
  .deactivate:disabled, .remove:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .help {
    margin: var(--space-2) 0 0;
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
  }
  /* A warning that changes what the user should do stays visible, in colour. */
  .help.warn { color: var(--danger); }
</style>
