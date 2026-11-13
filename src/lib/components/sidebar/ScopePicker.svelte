<script lang="ts">
  // The "For ⌄" picker (spec 2026-11-12 §3): which account the sidebar's
  // Views and Tags show. One component for both layouts — an anchored menu
  // on desktop and tablet, a bottom sheet on the phone (Popover decides) —
  // and only with more than one active account: with one there is nothing
  // to choose, so the column reads exactly as it does for one account today.
  //
  // Choosing changes what Views and Tags list and nothing else: not the open
  // list, not the current account (stores/sidebarScope.ts). The first row is
  // All accounts (spec 2026-11-13 §3.1); the button then names that and shows
  // no mark — each account's header carries its own.
  import Icon from '../Icon.svelte';
  import Popover from '../Popover.svelte';
  import { activeAccounts, backendLabel } from '../../stores/notes';
  import { accountNameBesideBackend } from '../../stores/listView';
  import { ALL_ACCOUNTS, chooseAllAccounts, chooseScopeAccount, scope } from '../../stores/sidebarScope';
  import { accountHeaderMark, type AccountDot } from '../../accountDot';

  let {
    dots,
    touch = false,
  }: {
    /** Per account: its sync state, as the account headers show it. */
    dots: Record<string, AccountDot | undefined>;
    /** Phone: rows at touch size. */
    touch?: boolean;
  } = $props();

  let anchor: HTMLButtonElement | undefined = $state();
  let open = $state(false);

  const all = $derived($scope?.kind === 'all');
  const scoped = $derived.by(() => {
    const s = $scope;
    return s?.kind === 'account' ? $activeAccounts.find((a) => a.id === s.accountId) : undefined;
  });

  function pick(accountId: string | null) {
    if (accountId === null) chooseAllAccounts();
    else chooseScopeAccount(accountId);
    open = false;
    anchor?.focus();
  }
</script>

{#if $activeAccounts.length > 1 && (all || scoped)}
  {@const name = scoped ? accountNameBesideBackend(scoped, $activeAccounts) : 'All accounts'}
  {@const backend = scoped ? backendLabel(scoped.backend_kind ?? 'gmail') : ''}
  {@const mark = scoped && dots[scoped.id] ? accountHeaderMark(dots[scoped.id]!) : null}
  <div class="scope" class:touch>
    <button
      bind:this={anchor}
      type="button"
      class="scope-button"
      data-scope-account={scoped ? scoped.id : ALL_ACCOUNTS}
      aria-haspopup="menu"
      aria-expanded={open}
      aria-label="Views and tags for {backend ? `${name} · ${backend}` : name}. Change account"
      title="Views and tags show this account. Choose another."
      onclick={() => (open = !open)}
    >
      <span class="scope-for">For</span>
      <span class="scope-name">{name}</span>
      {#if backend}<span class="scope-backend">{backend}</span>{/if}
      {#if mark}<span class="scope-mark" data-mark={mark} aria-hidden="true"></span>{/if}
      <span class="scope-caret"><Icon name="chevron-down" size={touch ? 16 : 12} /></span>
    </button>
  </div>
  {#if anchor}
    <Popover {anchor} bind:open onclose={() => (open = false)} label="Show views and tags for">
      <div class="sheet-menu scope-menu" role="menu" aria-label="Accounts">
        <button
          type="button"
          class="item"
          role="menuitem"
          data-account-id={ALL_ACCOUNTS}
          aria-current={all ? 'true' : undefined}
          onclick={() => pick(null)}
        >
          <span class="icon">{#if all}<Icon name="check" size={touch ? 20 : 14} />{/if}</span>
          <span class="label">All accounts</span>
        </button>
        <div class="scope-sep" role="separator"></div>
        {#each $activeAccounts as a (a.id)}
          {@const isScope = !all && a.id === scoped?.id}
          {@const dot = dots[a.id]}
          {@const m = dot ? accountHeaderMark(dot) : null}
          <button
            type="button"
            class="item"
            role="menuitem"
            data-account-id={a.id}
            aria-current={isScope ? 'true' : undefined}
            onclick={() => pick(a.id)}
          >
            <span class="icon">{#if isScope}<Icon name="check" size={touch ? 20 : 14} />{/if}</span>
            <span class="label">{accountNameBesideBackend(a, $activeAccounts)}</span>
            <span class="backend">{backendLabel(a.backend_kind ?? 'gmail')}</span>
            {#if m && dot}<span class="scope-mark" data-mark={m} role="img" aria-label={dot.text} title={dot.text}></span>{/if}
          </button>
        {/each}
      </div>
    </Popover>
  {/if}
{/if}

<style>
  .scope {
    padding: var(--space-1) var(--space-2) var(--space-0-5);
  }
  .scope.touch {
    padding: var(--space-1) var(--space-2);
  }
  .scope-button {
    display: flex;
    align-items: center;
    gap: var(--space-1-5);
    box-sizing: border-box;
    width: 100%;
    min-height: var(--hit-target-desktop);
    padding: 0 var(--space-2);
    background: var(--surface-badge);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font: inherit;
    font-size: var(--size-sm);
    line-height: var(--leading);
    color: var(--text);
    text-align: left;
    cursor: pointer;
    transition: border-color var(--dur-fast) var(--ease-out);
  }
  .touch .scope-button {
    min-height: var(--touch-target);
    padding: 0 var(--space-3);
    font-size: var(--size);
  }
  /* Opaque in both states (The Overlaid Contrast Rule): the border moves. */
  .scope-button:hover {
    border-color: var(--text-muted);
  }
  .scope-for {
    flex: 0 0 auto;
    color: var(--text-muted);
  }
  .scope-name {
    flex: 0 1 auto;
    min-width: 0;
    font-weight: var(--weight-semibold);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .scope-backend,
  .backend {
    flex: 0 0 auto;
    font-size: var(--size-xs);
    color: var(--text-muted);
    white-space: nowrap;
  }
  .scope-caret {
    display: inline-flex;
    margin-left: auto;
    color: var(--text-muted);
  }
  /* The account header's marks, same meaning: danger for a lost sign-in or a
     block, a hollow ring for changes waiting. The text is the row's name. */
  .scope-mark {
    flex: 0 0 auto;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    box-sizing: border-box;
  }
  .scope-mark[data-mark='alert'] {
    background: var(--danger);
  }
  .scope-mark[data-mark='waiting'] {
    border: 1.5px solid var(--text-muted);
  }

  /* Desktop menu rows (the phone's come from sheet-menu.css). */
  .scope-menu {
    display: flex;
    flex-direction: column;
    min-width: 220px;
  }
  .scope-menu .item {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-height: var(--hit-target-desktop);
    padding: var(--space-1) var(--space-2);
    background: none;
    border: none;
    border-radius: var(--radius-sm);
    font: inherit;
    font-size: var(--size);
    line-height: var(--leading);
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }
  .scope-menu .item:hover {
    background: var(--surface-hover);
  }
  .scope-menu .icon {
    display: inline-flex;
    justify-content: center;
    width: var(--space-4);
    flex: 0 0 auto;
    color: var(--accent-action);
  }
  .scope-sep {
    height: 1px;
    margin: var(--space-1) var(--space-2);
    background: var(--border-subtle);
  }
  .scope-menu .label {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
