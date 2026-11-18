<script lang="ts">
  import Icon from './Icon.svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import { get } from 'svelte/store';
  import { tvs, loadTvs, type TvRow } from '../tvs';
  import ConfirmDialog from './ConfirmDialog.svelte';

  // Settings › Share to Jodd › TVs: the YouTube TVs linked on this device (spec
  // 2026-11-02-tv-now-playing-ingest §4). Acts immediately, with no page Save,
  // so it is not a section form. The list comes from the `tvs` store, filled
  // from a flag file only (gotcha #15); a screen ID or token never reaches
  // here. Local-only edits (rename / default / unpair) are optimistic with a
  // snapshot rollback; link / re-pair / refresh reach the network and show a
  // busy state instead. Every success ends with loadTvs().

  const MAX_TVS = 5;

  const list = $derived($tvs);

  let code = $state('');
  let linking = $state(false);
  let msg = $state('');
  let note = $state('');
  // Per-row network work in flight (refresh / re-pair). A Record so two rows
  // can be busy at once without one's `finally` clearing the other's state.
  let busy = $state<Record<string, 'refresh' | 'repair'>>({});
  let renamePending = false;
  let openMenu = $state<string | null>(null);
  let repairing = $state<string | null>(null);
  let repairCode = $state('');
  let renaming = $state<string | null>(null);
  let renameValue = $state('');
  let confirmUnpair = $state<TvRow | null>(null);

  const full = $derived(list.length >= MAX_TVS);

  onMount(() => {
    void loadTvs();
  });

  function errorCopy(e: unknown): string {
    const err = (e && typeof e === 'object' ? e : {}) as { code?: string; label?: string };
    switch (err.code) {
      case 'bad_code':
        return "That code didn't work — the TV shows a new one every few minutes.";
      case 'too_many':
        return 'Unpair one first';
      case 'belongs_to_other':
        return `That code is ${err.label ?? 'another'}'s TV.`;
      case 'network':
        return "Couldn't reach the TV — check the connection.";
      case 'store':
        return "Couldn't save the TV list.";
      default:
        return typeof e === 'string' ? e : "Couldn't save the TV list.";
    }
  }

  /** Show the error for `e`; `not_found` just re-reads the list. */
  function report(e: unknown) {
    if ((e as { code?: string } | null)?.code === 'not_found') {
      void loadTvs();
      return;
    }
    msg = errorCopy(e);
  }

  function clearMessages() {
    msg = '';
    note = '';
  }

  function statusText(t: TvRow): string {
    if (t.needs_repair || t.last_status === 'needs_repair') return 'Needs re-pairing';
    if (t.last_status === 'online') return t.model ? `Online · ${t.model}` : 'Online';
    if (t.last_status === 'unreachable') return "Couldn't reach it";
    return t.model ?? 'Not checked yet';
  }

  const needsRepair = (t: TvRow) => t.needs_repair || t.last_status === 'needs_repair';

  async function link() {
    const c = code.trim();
    if (!c || full || linking) return;
    clearMessages();
    linking = true;
    try {
      const res = await invoke<{ row: TvRow; already_linked: boolean }>('link_tv', { code: c });
      code = '';
      if (res.already_linked) note = `Already linked as ${res.row.label}`;
      await loadTvs();
    } catch (e) {
      report(e);
    } finally {
      linking = false;
    }
  }

  async function refresh(t: TvRow) {
    openMenu = null;
    clearMessages();
    if (busy[t.tv_id]) return;
    busy[t.tv_id] = 'refresh';
    try {
      await invoke('refresh_tv', { tvId: t.tv_id });
      await loadTvs();
    } catch (e) {
      report(e);
    } finally {
      delete busy[t.tv_id];
    }
  }

  function startRepair(t: TvRow) {
    openMenu = null;
    renaming = null;
    clearMessages();
    repairing = t.tv_id;
    repairCode = '';
  }

  async function repair(t: TvRow) {
    const c = repairCode.trim();
    if (!c || busy[t.tv_id]) return;
    clearMessages();
    busy[t.tv_id] = 'repair';
    try {
      await invoke('repair_tv', { tvId: t.tv_id, code: c });
      repairing = null;
      repairCode = '';
      await loadTvs();
    } catch (e) {
      report(e);
    } finally {
      delete busy[t.tv_id];
    }
  }

  function startRename(t: TvRow) {
    openMenu = null;
    repairing = null;
    clearMessages();
    renaming = t.tv_id;
    renameValue = t.label;
  }

  /**
   * Run a local-file edit optimistically. On error roll back at once, then
   * re-read the list: the snapshot can be stale (another edit may have landed
   * meanwhile) and the file is the truth. Returns whether it succeeded.
   */
  async function optimistic(apply: (l: TvRow[]) => TvRow[], run: () => Promise<unknown>): Promise<boolean> {
    clearMessages();
    const snapshot = get(tvs);
    try {
      tvs.set(apply(snapshot));
      await run();
      await loadTvs();
      return true;
    } catch (e) {
      tvs.set(snapshot);
      void loadTvs();
      report(e);
      return false;
    }
  }

  async function rename(t: TvRow) {
    const label = renameValue.trim();
    if (!label) {
      msg = 'Give the TV a name.';
      return;
    }
    if (renamePending) return;
    renamePending = true;
    // Close at once (local-first); only a failure reopens it, and only if the
    // user has not opened a rename elsewhere meanwhile (a guarded rollback).
    const typed = renameValue;
    renaming = null;
    let ok = false;
    try {
      ok = await optimistic(
        (l) => l.map((r) => (r.tv_id === t.tv_id ? { ...r, label } : r)),
        () => invoke('rename_tv', { tvId: t.tv_id, label }),
      );
    } finally {
      renamePending = false;
    }
    if (!ok && renaming === null) {
      renaming = t.tv_id;
      renameValue = typed;
    }
  }

  async function makeDefault(t: TvRow) {
    openMenu = null;
    if (t.is_default) return;
    await optimistic(
      (l) => l.map((r) => ({ ...r, is_default: r.tv_id === t.tv_id })),
      () => invoke('set_default_tv', { tvId: t.tv_id }),
    );
  }

  async function unpair(t: TvRow) {
    confirmUnpair = null;
    await optimistic(
      (l) => {
        const rest = l.filter((r) => r.tv_id !== t.tv_id);
        // Unpairing the default promotes the oldest remaining row.
        if (t.is_default && rest.length > 0 && !rest.some((r) => r.is_default)) {
          rest[0] = { ...rest[0], is_default: true };
        }
        return rest;
      },
      () => invoke('unpair_tv', { tvId: t.tv_id }),
    );
  }

  function onMenuKeydown(ev: KeyboardEvent) {
    if (ev.key !== 'Escape' || !openMenu) return;
    ev.stopPropagation();
    ev.preventDefault();
    openMenu = null;
    (ev.currentTarget as HTMLElement).querySelector<HTMLButtonElement>('.kebab')?.focus();
  }

  function onWindowClick(ev: MouseEvent) {
    if (openMenu && !(ev.target as Element | null)?.closest?.('[data-tv-menu]')) openMenu = null;
  }
</script>

<svelte:window onclick={onWindowClick} />

{#if confirmUnpair}
  {@const target = confirmUnpair}
  <ConfirmDialog
    title={`Unpair ${target.label}?`}
    message={`Jodd forgets this TV on this device; your other TVs are untouched. The TV may still list "Jodd" under YouTube → Settings → Linked devices — you can tidy it there.`}
    confirmLabel="Unpair"
    destructive
    onConfirm={() => unpair(target)}
    onCancel={() => { confirmUnpair = null; }}
  />
{/if}

<div class="tvs">
  <h3 class="title">TVs</h3>
  {#if list.length === 0}
    <p class="help" data-testid="tv-link-empty">No TV linked yet.</p>
  {/if}

  <ul class="rows">
    {#each list as t (t.tv_id)}
      <li class="row" data-tv-id={t.tv_id}>
        <div class="row-main">
          <div class="row-text">
            <span class="label">{t.label}</span>
            {#if t.is_default}<span class="badge" title="Asked first">★ Default</span>{/if}
            <div class="status" class:warn={needsRepair(t) || t.last_status === 'unreachable'} data-testid="tv-link-status">
              {busy[t.tv_id] === 'refresh' ? 'Checking…' : statusText(t)}
            </div>
          </div>
          {#if needsRepair(t)}
            <div class="inline-actions">
              <button type="button" onclick={() => startRepair(t)} disabled={!!busy[t.tv_id]}>Re-pair</button>
              <button type="button" onclick={() => { confirmUnpair = t; }} disabled={!!busy[t.tv_id]}>Unpair</button>
            </div>
          {:else}
            <div class="menu-wrap" data-tv-menu role="presentation" onkeydown={onMenuKeydown}>
              <button
                type="button"
                class="kebab"
                aria-label={`Actions for ${t.label}`}
                aria-haspopup="menu"
                aria-expanded={openMenu === t.tv_id}
                onclick={() => { openMenu = openMenu === t.tv_id ? null : t.tv_id; }}
              ><Icon name="more" /></button>
              {#if openMenu === t.tv_id}
                <div class="menu" role="menu">
                  <button type="button" role="menuitem" disabled={t.is_default || !!busy[t.tv_id]} onclick={() => makeDefault(t)}>★ Default</button>
                  <button type="button" role="menuitem" onclick={() => refresh(t)} disabled={!!busy[t.tv_id]}>Refresh</button>
                  <button type="button" role="menuitem" disabled={!!busy[t.tv_id]} onclick={() => startRepair(t)}>Re-pair</button>
                  <button type="button" role="menuitem" disabled={!!busy[t.tv_id]} onclick={() => startRename(t)}>Rename</button>
                  <button type="button" role="menuitem" disabled={!!busy[t.tv_id]} onclick={() => { openMenu = null; confirmUnpair = t; }}>Unpair</button>
                </div>
              {/if}
            </div>
          {/if}
        </div>

        {#if repairing === t.tv_id}
          <form class="inline-form" onsubmit={(e) => { e.preventDefault(); void repair(t); }}>
            <label>
              New code for {t.label}
              <input type="text" inputmode="numeric" autocomplete="off" bind:value={repairCode} disabled={!!busy[t.tv_id]} placeholder="12-digit code from the TV" />
            </label>
            <button type="submit" disabled={!!busy[t.tv_id] || repairCode.trim() === ''}>{busy[t.tv_id] === 'repair' ? 'Pairing…' : 'Pair'}</button>
            <button type="button" onclick={() => { repairing = null; }} disabled={busy[t.tv_id] === 'repair'}>Cancel</button>
          </form>
        {/if}

        {#if renaming === t.tv_id}
          <form class="inline-form" onsubmit={(e) => { e.preventDefault(); void rename(t); }}>
            <label>
              Name
              <input type="text" autocomplete="off" bind:value={renameValue} />
            </label>
            <button type="submit">Save name</button>
            <button type="button" onclick={() => { renaming = null; }}>Cancel</button>
          </form>
        {/if}
      </li>
    {/each}
  </ul>

  <div class="link">
    <p class="help">On the TV: YouTube → Settings → Link with TV code</p>
    <form class="inline-form" onsubmit={(e) => { e.preventDefault(); void link(); }}>
      <label>
        TV code
        <input
          type="text"
          inputmode="numeric"
          autocomplete="off"
          placeholder="12-digit code"
          bind:value={code}
          disabled={linking || full}
        />
      </label>
      <button type="submit" disabled={linking || full || code.trim() === ''}>{linking ? 'Linking…' : 'Link'}</button>
    </form>
    {#if full}<p class="help" data-testid="tv-link-full">Unpair one first</p>{/if}
  </div>

  {#if note}<p class="note" role="status" data-testid="tv-link-note">{note}</p>{/if}
  {#if msg}<p class="msg" role="alert" data-testid="tv-link-error">{msg}</p>{/if}
</div>

<style>
  .tvs { padding: 12px 0; }
  .title { margin: 0 0 4px; font-size: var(--size); font-weight: var(--weight-semibold); }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 4px 0; }
  .rows { list-style: none; margin: 8px 0; padding: 0; }
  .row { padding: 8px 0; border-bottom: 1px solid var(--border, transparent); }
  .row-main { display: flex; align-items: flex-start; justify-content: space-between; gap: 8px; }
  .label { font-size: var(--size); }
  .badge { margin-left: 8px; font-size: var(--size-sm); color: var(--accent-action); }
  .status { font-size: var(--size-sm); color: var(--text-muted); }
  .status.warn { color: var(--danger); }
  .inline-actions { display: flex; gap: 8px; }
  .menu-wrap { position: relative; }
  .kebab { display: inline-flex; align-items: center; justify-content: center; padding: 4px 10px; font: inherit; }
  .menu {
    position: absolute; right: 0; z-index: var(--z-popover); min-width: 140px;
    display: flex; flex-direction: column;
    background: var(--surface-panel); border: 1px solid var(--border, transparent);
    border-radius: var(--radius); padding: 4px;
  }
  .menu button { text-align: left; background: none; border: none; padding: 6px 10px; font: inherit; color: var(--text); cursor: pointer; }
  .menu button:disabled { opacity: 0.5; cursor: default; }
  .inline-form { display: flex; flex-wrap: wrap; align-items: flex-end; gap: 8px; margin: 8px 0; }
  .inline-form label { flex: 1 1 200px; display: block; font-size: var(--size); }
  input { width: 100%; padding: 6px; font: inherit; box-sizing: border-box; margin-top: 4px; background: var(--surface-panel); color: var(--text); }
  button { padding: 8px 16px; font: inherit; }
  .menu button, .kebab { padding: 6px 10px; }
  .note { font-size: var(--size-sm); color: var(--text-muted); }
  .msg { font-size: var(--size-sm); color: var(--danger); }
</style>
