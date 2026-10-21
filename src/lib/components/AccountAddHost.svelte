<script lang="ts">
  // The add-account flows that need a component, in a component that is
  // always mounted while signed in. App.svelte mounts this once, outside the
  // layout switch, so it exists in the phone, tablet and desktop layouts
  // alike — unlike the Sidebar, which the phone layout unmounts whenever the
  // Folders pane is not showing, and which Settings › Accounts (the only add
  // path on a phone) must not depend on.
  //
  // It owns:
  // - the `addAccountRequest` subscription (the only one — see stores/ui.ts);
  // - Local Folder (`addLocalFolder`), with its Android plugin calls and
  //   visibility watcher, and the confirm/name dialogs that flow asks;
  // - the SSH dialogs (easy and advanced) and their shared `onSshAdded`;
  // - the `oauth-success` listener's post-add refresh and auto-select.
  //
  // Each body moved here verbatim from Sidebar.svelte, with two changes:
  // the Sidebar-local `refreshFolders()` became `requestFolderRefresh()`
  // (the Sidebar's folder block listens to that signal, and a Sidebar that
  // mounts later reads folders on mount anyway), and the oauth-success
  // auto-select snapshots the known accounts BEFORE writing the new list —
  // see the listener.
  //
  // Not mounted on AuthScreen on purpose: first-run sign-in never calls
  // `startAddAccount`, and its completion is App.svelte's isAuthenticated
  // transition (`refreshAccounts`, which selects the first account), exactly
  // as it was when this listener lived in the Sidebar, which AuthScreen does
  // not mount either.
  import { invoke } from '@tauri-apps/api/core';
  import { open as openDialog } from '@tauri-apps/plugin-dialog';
  import { onMount, onDestroy } from 'svelte';
  import { get } from 'svelte/store';
  import { accounts, currentAccount, refreshNotes, requestFolderRefresh } from '../stores/notes';
  import { addAccountRequest, localFolderBusy, type HostAddAccountKind } from '../stores/ui';
  import { isAndroid } from '../stores/platform';
  import type { Account } from '../types';
  import PromptDialogs from './PromptDialogs.svelte';
  import SshAccountDialog from './SshAccountDialog.svelte';
  import SshEasySetupDialog from './SshEasySetupDialog.svelte';

  // ─── Requests ─────────────────────────────────────────────────────────────
  // `startAddAccount` publishes Local Folder and SSH requests on
  // `addAccountRequest`. Consuming one sets the store back to null, so a
  // request made before this mounted is handled now (subscribe delivers the
  // current value) and a handled one is never replayed by a later mount.
  function runAddAccountRequest(kind: HostAddAccountKind) {
    if (kind === 'localfs') void addLocalFolder();
    else if (kind === 'ssh') openEasySsh();
    else openAddSsh();
  }
  onMount(() =>
    addAccountRequest.subscribe((req) => {
      if (!req) return;
      addAccountRequest.set(null);
      runAddAccountRequest(req.kind);
    }),
  );

  // ─── Inline prompt and confirm ────────────────────────────────────────────
  // The Local Folder flow asks for a confirmation and a vault name, through
  // the same PromptDialogs the Sidebar uses.
  let promptDialogs: PromptDialogs;
  function askName(title: string, defaultValue: string = ''): Promise<string | null> {
    return promptDialogs.askName(title, defaultValue);
  }
  function askConfirm(title: string, message: string, checkboxLabel: string | null = null): Promise<boolean> {
    return promptDialogs.askConfirm(title, message, checkboxLabel);
  }

  // ─── Local Folder ─────────────────────────────────────────────────────────
  // Add a local folder (LocalFS backend). Opens a native directory picker,
  // prompts for a vault name (default = folder basename), calls add_local_account,
  // then mirrors the same post-add refresh the OAuth flow uses so the new account's
  // notes appear in the sidebar.
  // Tauri's Android PluginManager has ONE activity-result slot: a second
  // launch while the first is open orphans the first call forever, so a tap
  // while the flow is in flight is ignored. The busy flag is the
  // `localFolderBusy` store, so the buttons that start this flow can show it.
  // Generation counter: a stale (orphaned) flow's `finally` must not clear a
  // newer flow's state.
  let localFolderFlow = 0;
  // True ONLY while one of the three plugin invokes is awaited.
  let awaitingPlugin = false;
  // Identifies WHICH plugin call is pending, so a timer armed for call N
  // never frees call N+1.
  let pluginCallSeq = 0;
  let orphanTimer: ReturnType<typeof setTimeout> | null = null;
  // Whether THIS host set `localFolderBusy`. Only this host writes the flag
  // today; the guard is for a second writer (Settings › Accounts' Local
  // Folder button is next), so an unmount frees only a flag this host holds.
  let ownsBusy = false;
  function setBusy(v: boolean) {
    ownsBusy = v;
    localFolderBusy.set(v);
  }
  // If Android destroys and recreates the activity while Settings or the
  // picker is open, Tauri 2.11.5 never resolves the pending call. A live
  // flow's plugin result arrives within milliseconds of returning to the app;
  // if the call is still pending 2 s after `visible`, it is orphaned: free
  // the button (the flow is not resumed).
  function onLocalFolderVisibility() {
    if (orphanTimer) { clearTimeout(orphanTimer); orphanTimer = null; }
    if (document.visibilityState !== 'visible') return; // hidden: re-arm on the next visible
    const mine = localFolderFlow;
    const armedFor = pluginCallSeq;
    orphanTimer = setTimeout(() => {
      orphanTimer = null;
      if (awaitingPlugin && mine === localFolderFlow && armedFor === pluginCallSeq) {
        setBusy(false);
        awaitingPlugin = false;
        localFolderFlow++;
        stopLocalFolderWatch();
      }
    }, 2000);
  }
  function stopLocalFolderWatch() {
    document.removeEventListener('visibilitychange', onLocalFolderVisibility);
    if (orphanTimer) { clearTimeout(orphanTimer); orphanTimer = null; }
  }
  // The busy flag is a store and outlives this component, which the old
  // Sidebar-local flag did not. If the host unmounts mid-flow (signing out
  // of the last account), retire the flow and free the flag, or the next
  // host would find Add Local Folder disabled until restart.
  onDestroy(() => {
    stopLocalFolderWatch();
    if (ownsBusy) {
      localFolderFlow++;
      setBusy(false);
    }
  });
  async function pluginCall<T>(call: Promise<T>, mine: number): Promise<T> {
    awaitingPlugin = true;
    const seq = ++pluginCallSeq;
    try {
      return await call;
    } finally {
      if (mine === localFolderFlow && seq === pluginCallSeq) awaitingPlugin = false;
    }
  }

  async function addLocalFolder() {
    if (get(localFolderBusy)) return;
    setBusy(true);
    const mine = ++localFolderFlow;
    if ($isAndroid) document.addEventListener('visibilitychange', onLocalFolderVisibility);
    try {
      let dir: string | null;
      if ($isAndroid) {
        // Android: the Kotlin StoragePlugin, never the desktop dialog (which
        // has no folder picker on Android). Spec 2026-09-29-localfs-android.
        let access = await pluginCall(invoke<{ granted: boolean; supported: boolean }>('local_folder_access'), mine);
        if (mine !== localFolderFlow) return; // orphaned and freed; a stale flow must not continue
        if (!access.supported) {
          alert('Local Folder on Android needs Android 11 or newer.');
          return;
        }
        if (!access.granted) {
          const go = await askConfirm(
            'All files access',
            'To share a folder with Syncthing, Jodd needs All files access. Android will open a settings page: turn Jodd on, then come back.'
          );
          if (!go) return;
          access = await pluginCall(invoke<{ granted: boolean; supported: boolean }>('request_local_folder_access'), mine);
          if (mine !== localFolderFlow) return;
          if (!access.granted) {
            alert("Without All files access, Jodd can't use a folder that other apps can see.");
            return;
          }
        }
        dir = await pluginCall(invoke<string | null>('pick_local_folder'), mine);
      } else {
        const picked = await openDialog({ directory: true, multiple: false, title: 'Choose a notes folder' });
        dir = typeof picked === 'string' ? picked : null;
      }
      if (mine !== localFolderFlow) return;
      if (!dir) return; // cancelled

      // Disclosure requirement (docs/superpowers/specs/2026-08-13-at-rest-encryption-design.md,
      // "Scope limitation: LocalFs vaults are NOT covered by this design"):
      // Gmail accounts' local cache is encrypted at rest; Local Folder vaults
      // are not, and this must be stated plainly at the point of choice, not
      // buried in settings or a privacy-policy footnote.
      const acknowledged = await askConfirm(
        'Notes in this folder are not encrypted',
        'Jodd stores notes in a Local Folder vault as plain, unencrypted files ' +
        'in the folder you choose. Anyone with access to this folder on disk ' +
        'can read them directly. Protecting this folder — full-disk ' +
        'encryption, access controls, backup handling — is your ' +
        'responsibility. This is different from Gmail accounts, whose local ' +
        'note cache Jodd encrypts automatically.'
      );
      if (!acknowledged) return;

      // Derive the folder basename as the default vault name.
      const basename = dir.replace(/\\/g, '/').split('/').filter(Boolean).pop() ?? dir;
      // Use the existing inline prompt (window.prompt is unreliable in Tauri WKWebView).
      const vaultName = await askName('Vault name (shown in sidebar):', basename);
      if (vaultName === null) return; // user cancelled the name prompt
      const name = vaultName.trim() || basename;
      const newAccount = await invoke<Account>('add_local_account', { path: dir, name });
      // Mirror the oauth-success post-add flow: update accounts store,
      // auto-switch to the new account, then refresh notes + folders.
      const list = await invoke<Account[]>('list_accounts');
      accounts.set(list);
      currentAccount.set(newAccount.id);
      $refreshNotes();
      requestFolderRefresh();
    } catch (e) {
      console.error('add local folder failed', e);
      alert(`Failed to add local folder: ${e}`);
    } finally {
      if (mine === localFolderFlow) {
        stopLocalFolderWatch();
        setBusy(false);
      }
    }
  }

  // ─── SSH ──────────────────────────────────────────────────────────────────
  // Add an SSH server (Ssh backend). Opens SshAccountDialog, which browses
  // and validates on the Rust side; this mirrors the same post-add refresh
  // every other backend uses (gotcha #6).
  let sshDialogOpen = $state(false);
  function openAddSsh() {
    sshDialogOpen = true;
  }

  // The "easy" Managed setup (Task 7): fingerprint confirm, key install over
  // a password-authenticated run, then key-only auth from then on — no
  // ssh-agent or ~/.ssh needed. Shares onSshAdded with the advanced dialog
  // above: same Account shape in, same post-add refresh.
  let easySshDialogOpen = $state(false);
  function openEasySsh() {
    easySshDialogOpen = true;
  }

  async function onSshAdded(newAccount: Account) {
    sshDialogOpen = false;
    easySshDialogOpen = false;
    const list = await invoke<Account[]>('list_accounts');
    accounts.set(list);
    currentAccount.set(newAccount.id);
    $refreshNotes();
    requestFolderRefresh();
  }

  // ─── oauth-success ────────────────────────────────────────────────────────
  // Listen for oauth-success — fired when a new account finishes the PKCE
  // exchange (and by icloud_sign_in). Refetch the accounts list, then trigger
  // a full refresh so the new account's notes appear in the sidebar. App.svelte
  // has its own listener, which only flips isAuthenticated; this is the one
  // that refreshes and auto-selects.
  //
  // The known-account snapshot is taken BEFORE `accounts.set(list)`. In the
  // Sidebar it was read from `$accounts` after the set, and a store
  // subscription updates synchronously on `set`, so the "fresh" account was
  // always already known and the auto-select never happened.
  onMount(() => {
    const unlisten = (async () => {
      const { listen } = await import('@tauri-apps/api/event');
      return await listen('oauth-success', async () => {
        // Logged rather than assumed: this listener firing is the difference
        // between "the event never arrived" and "it arrived and the refresh
        // failed", and on a release build there is no devtools to ask.
        console.log('[jodd] oauth-success received — refetching accounts');
        const known = new Set(get(accounts).map((a) => a.id));
        try {
          const list = await invoke<Account[]>('list_accounts');
          accounts.set(list);
          // Auto-switch to the new account so the user sees its content.
          const fresh = list.find((a) => !known.has(a.id));
          if (fresh) currentAccount.set(fresh.id);
          $refreshNotes();
          requestFolderRefresh();
        } catch (e) {
          console.error('post-oauth refresh failed', e);
        }
      });
    })();
    return () => { unlisten.then((fn) => fn()); };
  });
</script>

<PromptDialogs bind:this={promptDialogs} />

{#if sshDialogOpen}
  <SshAccountDialog onAdded={onSshAdded} onClose={() => (sshDialogOpen = false)} />
{/if}

{#if easySshDialogOpen}
  <SshEasySetupDialog onAdded={onSshAdded} onClose={() => (easySshDialogOpen = false)} />
{/if}
