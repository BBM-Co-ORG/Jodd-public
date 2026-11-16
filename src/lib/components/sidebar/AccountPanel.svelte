<script lang="ts">
  // The footer's account panel: markup and styles only. Sidebar.svelte keeps
  // whether it is open (the chip, Esc, a click outside — onWindowKey /
  // onWindowPointerDown match `.account-panel` by class), the "N left" poll
  // for draining accounts, and what every button does. Split out of
  // Sidebar.svelte 2026-11-16.
  //
  // A disclosure: a panel of controls (pick, settings, remove per account),
  // not a menu of commands — its rows hold three buttons each, which a
  // role="menu" may not.
  import Icon from '../Icon.svelte';
  import { accountDisplay, backendLabel } from '../../stores/notes';
  import { accountNameBesideBackend, accountNameWithBackend } from '../../stores/listView';
  import type { AddAccountKind } from '../../controllers/accountActions';
  import type { Account } from '../../types';

  interface Props {
    /** The active accounts, one row each. */
    accounts: Account[];
    /** Draining and inactive accounts, under the Inactive fold. */
    dismissed: Account[];
    currentId: string | null;
    /** Remaining pushes per draining account; absent = not fetched yet. */
    pending: Record<string, number>;
    /** True where the last count failed: said, not shown as "…" forever. */
    pendingError: Record<string, boolean>;
    /** The Inactive fold. Bound: Sidebar polls the counts only while open. */
    inactiveOpen: boolean;
    /** Where an iCloud sign-in has been measured to work (`supportsIcloud`). */
    canAddIcloud: boolean;
    /** Desktop only: needs the user's own ssh-agent and ~/.ssh/config. */
    canAddAdvancedSsh: boolean;
    localFolderBusy: boolean;
    onpick: (a: Account) => void;
    onsettings: (a: Account) => void;
    onremove: (a: Account) => void;
    onstopwaiting: (a: Account) => void;
    onreactivate: (a: Account) => void;
    onadd: (kind: AddAccountKind) => void;
  }

  let {
    accounts,
    dismissed,
    currentId,
    pending,
    pendingError,
    inactiveOpen = $bindable(),
    canAddIcloud,
    canAddAdvancedSsh,
    localFolderBusy,
    onpick,
    onsettings,
    onremove,
    onstopwaiting,
    onreactivate,
    onadd,
  }: Props = $props();
</script>

<div class="account-panel" id="sidebar-account-panel" role="group" aria-label="Accounts">
  <div class="account-panel-header">Accounts</div>
  {#each accounts as a (a.id)}
    <div class="account-row" class:active={a.id === currentId}>
      <button
        class="account-row-pick"
        onclick={() => onpick(a)}
        title="Switch to {accountDisplay(a)}"
      >
        <span class="account-row-dot" class:on={a.id === currentId} aria-hidden="true"></span>
        {#if a.id === currentId}<span class="sr-only">Current account:</span>{/if}
        {#if a.backend_kind === 'local_fs'}<span class="account-row-kind-icon"><Icon name="folder" size={12} /></span>{/if}
        <span class="account-row-email" title={accountDisplay(a)}>{accountNameBesideBackend(a, accounts)}</span>
        {#if a.backend_kind}
          <span class="account-row-backend">{backendLabel(a.backend_kind)}</span>
        {/if}
      </button>
      <button
        class="account-row-settings"
        onclick={() => onsettings(a)}
        title="Settings for {accountNameWithBackend(a, accounts)}"
        aria-label="Settings for {accountNameWithBackend(a, accounts)}"
      ><Icon name="gear" size={13} /></button>
      <button
        class="account-row-remove"
        onclick={() => onremove(a)}
        title="Remove {accountNameWithBackend(a, accounts)}"
        aria-label="Remove {accountNameWithBackend(a, accounts)}"
      ><Icon name="close" size={12} /></button>
    </div>
    {#if a.blocked_reason}
      <!-- Its own row under the account, not a tooltip or a badge: the
           reason names a setting in Apple's own UI, and a message the
           user has to hover to find is a message they will not read.
           Backend-neutral by construction — the text is whatever the
           backend put in `blocked_reason`. -->
      <p class="account-row-blocked" role="status">{a.blocked_reason}</p>
    {/if}
  {/each}
  {#if dismissed.length > 0}
    <button
      type="button"
      class="account-panel-header inactive-toggle"
      onclick={() => (inactiveOpen = !inactiveOpen)}
    >
      <Icon name={inactiveOpen ? 'chevron-down' : 'chevron-right'} size={12} />
      Inactive ({dismissed.length})
    </button>
    {#if inactiveOpen}
      {#each dismissed as a (a.id)}
        <div class="account-row dimmed">
          <span class="account-row-email" title={accountDisplay(a)}>{accountDisplay(a)}</span>
          {#if a.backend_kind}
            <span class="account-row-backend">{backendLabel(a.backend_kind)}</span>
          {/if}
          {#if a.status === 'draining'}
            <span class="account-row-note" class:warn={pendingError[a.id]}>
              {#if pendingError[a.id]}
                finishing sync — count unavailable
              {:else if pending[a.id] === undefined}
                finishing sync — … left
              {:else}
                finishing sync — {pending[a.id]} left
              {/if}
            </span>
            <button type="button" onclick={() => onstopwaiting(a)}>Stop waiting</button>
            {#if a.pending_removal}
              <span class="account-row-note">removal queued</span>
            {:else}
              <button type="button" class="account-row-remove" onclick={() => onremove(a)}>Remove</button>
            {/if}
          {:else}
            <button type="button" onclick={() => onreactivate(a)}>Reactivate</button>
            <button type="button" class="account-row-remove" onclick={() => onremove(a)}>Remove</button>
          {/if}
        </div>
      {/each}
    {/if}
  {/if}
  <div class="sep"></div>
  <button class="account-panel-action" onclick={() => onadd('gmail')}>
    <span class="icon"><Icon name="plus" /></span>
    <span class="label">Add Gmail account</span>
  </button>
  <!-- Every platform, Android included: Microsoft redirects through the
       same App Links URL Gmail uses (`auth_ms::redirect_uri`), so the
       loopback listener that used to keep it off Android is no longer
       the only way back into the app. -->
  <button class="account-panel-action" onclick={() => onadd('microsoft')}>
    <span class="icon"><Icon name="plus" /></span>
    <span class="label">Add Microsoft account</span>
  </button>
  {#if canAddIcloud}
    <!-- Every desktop platform, not just macOS: `data_store_identifier`
         is Apple's isolation lever and `data_directory` is everyone
         else's (icloud_auth's `isolated()`), measured identical on both
         engines 2026-09-09; Android joined on a device pass 2026-09-10
         (stores/platform.ts, ICLOUD_PLATFORMS).

         Its own flow (`addICloudAccount`), not the OAuth one: there is
         no OAuth here at all — no url to open, no browser, no
         redirect — so it shares none of that path beyond the name. -->
    <button class="account-panel-action" onclick={() => onadd('icloud')}>
      <span class="icon"><Icon name="plus" /></span>
      <span class="label">Add iCloud account</span>
    </button>
  {/if}
  <button class="account-panel-action" onclick={() => onadd('localfs')} disabled={localFolderBusy}>
    <span class="icon"><Icon name="folder" /></span>
    <span class="label">Add Local Folder</span>
  </button>
  <!-- Managed SSH works on Android through the embedded russh client;
       Advanced SSH needs the user's own ssh-agent / ~/.ssh/config,
       which a phone does not have. -->
  <button class="account-panel-action" onclick={() => onadd('ssh')}>
    <span class="icon"><Icon name="server" /></span>
    <span class="label">Add SSH Server</span>
  </button>
  {#if canAddAdvancedSsh}
    <button class="account-panel-action" onclick={() => onadd('ssh_advanced')}>
      <span class="icon"><Icon name="server" /></span>
      <span class="label">Add SSH Server (advanced)</span>
    </button>
  {/if}
</div>

<style>
  .account-panel {
    position: absolute;
    bottom: calc(100% + 4px);
    left: 8px;
    right: 8px;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-menu-up);
    padding: 4px;
    z-index: var(--z-drawer);
    font-size: var(--size);
  }

  /* DESIGN.md's Section Label: 700, uppercase, 0.04em. */
  .account-panel-header {
    font-size: var(--size-xs);
    font-weight: var(--weight-bold);
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    line-height: var(--leading);
    padding: var(--space-1-5) var(--space-2) var(--space-1);
  }

  .account-row {
    display: flex;
    align-items: stretch;
  }

  /* The open account, the way an open row is marked everywhere else: the
     active surface, not an amber wash (amber means touched). */
  .account-row.active {
    background: var(--surface-active);
    border-radius: var(--radius-sm);
  }

  /* Tight on purpose: the panel is as wide as the column, and the two row
     buttons keep their width even while hidden, so every pixel of padding
     came out of the account's name. */
  .account-row-pick {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--space-1-5);
    padding: var(--space-1-5) var(--space-1-5) var(--space-1-5) var(--space-2);
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    color: var(--text);
    border-radius: var(--radius-sm);
    font-family: inherit;
    min-width: 0;
  }

  .account-row-pick:hover {
    background: var(--surface-sunken);
  }

  /* --accent-action, not --accent: this dot's colour IS text (WCAG 1.4.11
     applies), and tokens.css reserves --accent for fills/tints/borders with
     no text on them -- --accent measured 2.77:1 here, failing the 3:1 floor
     for an informational non-text indicator. Not --focus either: this dot
     means "this is the active account", which is state the user reads, so
     it earns the brand colour; focus blue is reserved for where keyboard
     focus actually is. */
  .account-row-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    box-sizing: border-box;
    border: 1.5px solid var(--text-muted);
    flex-shrink: 0;
  }
  .account-row-dot.on {
    background: var(--text-secondary);
    border-color: var(--text-secondary);
  }

  .account-row-email {
    flex: 1;
    font-size: var(--size-sm);
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .account-row-remove,
  .account-row-settings {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: var(--space-6);
    background: none;
    border: none;
    font-size: var(--size);
    cursor: pointer;
    /* Visible by default, and hidden only below where a pointer can reveal
       them again. The hiding used to live here, unconditionally, and a touch
       screen has no hover to undo it — so on Android both controls were
       invisible for good. `opacity: 0` hides paint and not hit-testing, which
       made the worse half: a live Remove-account button nobody could see, in a
       fixed spot at the end of every row.

       Stated in this direction on purpose. If `hover: hover` is ever
       unsupported or mis-evaluated, the fallback is a visible button rather
       than an invisible destructive one — the failure that costs a user an
       account is the one worth designing against. */
    opacity: 1;
    transition: opacity var(--dur-fast) var(--ease-out);
    border-radius: var(--radius-sm);
  }
  .account-row-remove { color: var(--danger); font-size: var(--size-sm); }
  .account-row-settings { color: var(--text-muted); }

  /* Pointer devices keep the reveal-on-hover affordance: the row stays quiet
     until it is pointed at. `hover: hover` asks the device what it can do,
     rather than inferring it from viewport width — a touch laptop and a phone
     both answer honestly, and a narrow desktop window is not mistaken for a
     phone. */
  @media (hover: hover) {
    .account-row-remove,
    .account-row-settings {
      opacity: 0;
    }
    .account-row:hover .account-row-remove,
    .account-row:hover .account-row-settings,
    /* Tabbing onto the row shows them too: a focused button that stays at
       opacity 0 is a focus ring around nothing (workspace review 2026-11-09). */
    .account-row:focus-within .account-row-remove,
    .account-row:focus-within .account-row-settings {
      opacity: 1;
    }
  }

  .account-row-remove:hover {
    background: var(--danger-wash);
  }
  .account-row-settings:hover {
    background: var(--surface-hover);
    color: var(--text);
  }

  .inactive-toggle {
    display: flex;
    align-items: center;
    gap: 4px;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    cursor: pointer;
    font-family: inherit;
  }
  .inactive-toggle:hover {
    background: var(--surface-hover);
    border-radius: var(--radius-sm);
  }

  /* Muted ink, not opacity: opacity also fades the action buttons and
     drops the name below its contrast floor. */
  .account-row.dimmed {
    color: var(--text-muted);
    gap: 6px;
    align-items: center;
    padding: 4px 10px;
    /* Wrap, so the name gets a line to itself and the actions fall below it.
       On one line the two buttons plus the "finishing sync — N left" note left
       the name about 60px, which truncated every account to "kaiw…" — with
       several dismissed accounts they were indistinguishable, and the group's
       whole job is to let you pick one out and bring it back. */
    flex-wrap: wrap;
  }
  .account-row.dimmed .account-row-email {
    /* Full width of its own row; ellipsis now only for genuinely long names. */
    flex: 1 0 100%;
    font-size: var(--size-sm);
  }
  .account-row-note {
    flex-shrink: 0;
    font-size: var(--size-xs);
    color: var(--text-muted);
    white-space: nowrap;
  }
  .account-row-note.warn {
    color: var(--danger);
  }
  /* Wraps, unlike `.account-row-note`: this one is a sentence naming a
     setting in Apple's own UI, and truncating it to one line would cut off
     the only actionable half. */
  .account-row-blocked {
    margin: 0 0 6px 26px;
    padding-right: 10px;
    font-size: var(--size-xs);
    line-height: 1.5;
    color: var(--danger);
    white-space: normal;
  }
  .account-row.dimmed button {
    flex: none;
    width: auto;
    opacity: 1;
    padding: 3px 8px;
    font-size: var(--size-xs);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: none;
    color: var(--text-secondary);
    cursor: pointer;
    font-family: inherit;
  }
  .account-row.dimmed button:hover {
    background: var(--surface-hover);
  }
  .account-row.dimmed .account-row-remove {
    color: var(--danger);
    border-color: var(--danger);
  }
  .account-row.dimmed .account-row-remove:hover {
    background: var(--danger-wash);
  }

  .account-panel-action {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 10px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    color: var(--text);
    border-radius: var(--radius-sm);
    font-family: inherit;
    font-size: var(--size);
  }

  .account-panel-action:hover {
    background: var(--surface-hover);
  }

  /* --text-muted, not --focus: these are decorative affordance icons on menu
     rows. They carry no state, so they should sit behind their labels rather
     than compete with them for attention. */
  .account-panel .icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    text-align: center;
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .account-panel .sep {
    height: 1px;
    background: var(--surface-active);
    margin: 4px 8px;
  }

  .account-row-kind-icon {
    display: inline-flex;
    align-items: center;
    color: var(--text-muted);
    margin-right: 1px;
  }

  /* The same badge as the sidebar's .account-backend-tag and
     .account-chip-backend (Sidebar.svelte): the backend named beside the
     address, muted, informational. */
  .account-row-backend {
    font-size: var(--size-xs);
    color: var(--text-muted);
    background: var(--surface-badge);
    border-radius: var(--radius-sm);
    padding: 0 var(--space-1);
    flex: 0 0 auto;
    white-space: nowrap;
  }

  /* Sidebar.svelte has its own: Svelte scopes each component's styles. */
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
    border: 0;
  }

  /* Touch (critique 2026-11-10, P1): a coarse pointer gets the 48px Touch
     Target; a mouse keeps the desktop density. Sidebar.svelte carries the
     same rule for its own rows and buttons. */
  @media (pointer: coarse) {
    .account-row-pick,
    .account-panel-action {
      min-height: var(--touch-target);
    }
    .account-row-settings,
    .account-row-remove {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      min-width: var(--touch-target);
      min-height: var(--touch-target);
    }
  }
</style>
