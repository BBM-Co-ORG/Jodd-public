// @vitest-environment jsdom
//
// Settings › Share to Jodd › TVs (spec 2026-11-02-tv-now-playing-ingest §4, §5.3).
// The list reads a flag file only (gotcha #15); local edits are optimistic
// with a snapshot rollback; network actions show a busy state.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import TvLinkSettings from './TvLinkSettings.svelte';
import { tvs, type TvRow } from '../tvs';

async function settle() {
  for (let i = 0; i < 8; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

const row = (id: string, label: string, extra: Partial<TvRow> = {}): TvRow => ({
  tv_id: id,
  label,
  model: null,
  is_default: false,
  needs_repair: false,
  last_status: null,
  ...extra,
});

describe('TvLinkSettings', () => {
  let host: HTMLElement;
  let comp: object;
  /** What list_tvs returns; tests change it to model the backend. */
  let backend: TvRow[];
  /** Per-command overrides. */
  let handlers: Record<string, (args: any) => unknown>;

  async function open(rows: TvRow[]) {
    backend = rows;
    tvs.set([]);
    invoke.mockClear();
    comp = mount(TvLinkSettings, { target: host });
    await settle();
  }

  beforeEach(() => {
    invoke.mockReset();
    handlers = {};
    backend = [];
    invoke.mockImplementation((cmd: string, args: unknown) => {
      if (handlers[cmd]) return Promise.resolve().then(() => handlers[cmd](args));
      if (cmd === 'list_tvs') return Promise.resolve(backend);
      return Promise.reject(new Error(`unexpected ${cmd}`));
    });
    host = document.createElement('div');
    document.body.appendChild(host);
  });

  afterEach(() => {
    unmount(comp as never);
    host.remove();
    document.querySelectorAll('dialog').forEach((d) => d.remove());
  });

  const text = () => (host.textContent ?? '').replace(/\s+/g, ' ');
  const error = () => host.querySelector('[data-testid="tv-link-error"]')?.textContent ?? null;
  const rowEl = (label: string) =>
    [...host.querySelectorAll<HTMLElement>('[data-tv-id]')].find((r) => r.textContent!.includes(label))!;
  const btn = (scope: ParentNode, name: string) =>
    [...scope.querySelectorAll<HTMLButtonElement>('button')].find((b) => b.textContent!.trim() === name)!;
  async function type(input: HTMLInputElement, value: string) {
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
  }
  async function openMenu(label: string) {
    rowEl(label).querySelector<HTMLButtonElement>('button.kebab')!.click();
    await settle();
  }
  const linkInput = () => host.querySelector<HTMLInputElement>('.link input')!;
  const calls = (cmd: string) => invoke.mock.calls.filter(([c]) => c === cmd);

  it('empty_list_shows_help_and_link', async () => {
    await open([]);
    expect(text()).toContain('On the TV: YouTube → Settings → Link with TV code');
    expect(text()).toContain('No TV linked yet.');
    expect(btn(host, 'Link')).toBeTruthy();
  });

  it('list_render_calls_only_list_tvs', async () => {
    await open([row('a', 'Living room')]);
    expect(invoke.mock.calls.map(([c]) => c)).toEqual(['list_tvs']);
  });

  it('link_success_adds_row', async () => {
    await open([]);
    handlers.link_tv = () => {
      backend = [row('a', 'Hisense HiSmartTV A4', { is_default: true })];
      return { row: backend[0], already_linked: false };
    };
    await type(linkInput(), '1234 5678 9012');
    btn(host, 'Link').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('link_tv', { code: '1234 5678 9012' });
    expect(text()).toContain('Hisense HiSmartTV A4');
    expect(text()).not.toContain('No TV linked yet.');
    expect(linkInput().value).toBe('');
  });

  it('link_bad_code_message', async () => {
    await open([]);
    handlers.link_tv = () => Promise.reject({ code: 'bad_code' });
    await type(linkInput(), '000000000000');
    btn(host, 'Link').click();
    await settle();
    expect(error()).toBe("That code didn't work — the TV shows a new one every few minutes.");
  });

  it('link_error_copy_by_code', async () => {
    await open([]);
    const cases: [unknown, string][] = [
      [{ code: 'network' }, "Couldn't reach the TV — check the connection."],
      [{ code: 'store' }, "Couldn't save the TV list."],
      [{ code: 'too_many' }, 'Unpair one first'],
    ];
    for (const [err, copy] of cases) {
      handlers.link_tv = () => Promise.reject(err);
      await type(linkInput(), '111111111111');
      btn(host, 'Link').click();
      await settle();
      expect(error()).toBe(copy);
    }
  });

  it('link_already_linked', async () => {
    await open([row('a', 'Living room', { is_default: true })]);
    handlers.link_tv = () => ({ row: backend[0], already_linked: true });
    await type(linkInput(), '123456789012');
    btn(host, 'Link').click();
    await settle();
    expect(host.querySelector('[data-testid="tv-link-note"]')!.textContent).toBe('Already linked as Living room');
  });

  it('link_disabled_at_five', async () => {
    await open(['a', 'b', 'c', 'd', 'e'].map((x, i) => row(x, `TV ${i + 1}`, { is_default: i === 0 })));
    expect(text()).toContain('Unpair one first');
    expect(linkInput().disabled).toBe(true);
    expect(btn(host, 'Link').disabled).toBe(true);
    expect(calls('link_tv')).toHaveLength(0);
  });

  it('row_menu_has_default_refresh_repair_rename_unpair', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    await openMenu('Bedroom');
    const items = [...rowEl('Bedroom').querySelectorAll('[role="menuitem"]')].map((e) => e.textContent!.trim());
    expect(items).toEqual(['★ Default', 'Refresh', 'Re-pair', 'Rename', 'Unpair']);
  });

  it('refresh_shows_status', async () => {
    await open([row('a', 'Living room', { is_default: true })]);
    const outcomes: [Partial<TvRow>, string][] = [
      [{ last_status: 'online', model: 'Hisense HiSmartTV A4' }, 'Online · Hisense HiSmartTV A4'],
      [{ last_status: 'online', model: null }, 'Online'],
      [{ last_status: 'unreachable' }, "Couldn't reach it"],
      [{ last_status: 'needs_repair', needs_repair: true }, 'Needs re-pairing'],
    ];
    for (const [patch, expected] of outcomes) {
      // Start each round from a row that has a menu, so Refresh is reachable.
      tvs.set([row('a', 'Living room', { is_default: true })]);
      await settle();
      handlers.refresh_tv = () => {
        backend = [row('a', 'Living room', { is_default: true, ...patch })];
        return backend[0];
      };
      await openMenu('Living room');
      btn(rowEl('Living room'), 'Refresh').click();
      await settle();
      expect(rowEl('Living room').querySelector('[data-testid="tv-link-status"]')!.textContent!.trim()).toBe(expected);
    }
    expect(invoke).toHaveBeenCalledWith('refresh_tv', { tvId: 'a' });
  });

  it('refresh_busy_state_while_in_flight', async () => {
    await open([row('a', 'Living room', { is_default: true })]);
    let release!: () => void;
    handlers.refresh_tv = () => new Promise<void>((r) => { release = r; });
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Refresh').click();
    await settle();
    expect(rowEl('Living room').textContent).toContain('Checking…');
    release();
    await settle();
    expect(rowEl('Living room').textContent).not.toContain('Checking…');
  });

  it('repair_other_rows_code', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    handlers.repair_tv = () => Promise.reject({ code: 'belongs_to_other', label: 'Bedroom' });
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Re-pair').click();
    await settle();
    await type(rowEl('Living room').querySelector<HTMLInputElement>('form input')!, '999999999999');
    btn(rowEl('Living room'), 'Pair').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('repair_tv', { tvId: 'a', code: '999999999999' });
    expect(error()).toBe("That code is Bedroom's TV.");
  });

  it('repair_success_closes_form_and_reloads', async () => {
    await open([row('a', 'Living room', { is_default: true, needs_repair: true, last_status: 'needs_repair' })]);
    handlers.repair_tv = () => {
      backend = [row('a', 'Living room', { is_default: true, last_status: 'online' })];
      return backend[0];
    };
    btn(rowEl('Living room'), 'Re-pair').click();
    await settle();
    await type(rowEl('Living room').querySelector<HTMLInputElement>('form input')!, '123456789012');
    btn(rowEl('Living room'), 'Pair').click();
    await settle();
    expect(rowEl('Living room').querySelector('form')).toBeNull();
    expect(rowEl('Living room').textContent).toContain('Online');
  });

  it('needs_repair_row_offers_repair_and_unpair', async () => {
    await open([row('a', 'Living room', { is_default: true, needs_repair: true, last_status: 'needs_repair' })]);
    const r = rowEl('Living room');
    expect(r.textContent).toContain('Needs re-pairing');
    expect(btn(r, 'Re-pair')).toBeTruthy();
    expect(btn(r, 'Unpair')).toBeTruthy();
  });

  it('unpair_copy_does_not_claim_tv_side_revocation', async () => {
    await open([row('a', 'Living room', { is_default: true })]);
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Unpair').click();
    await settle();
    const copy = (document.body.textContent ?? '').replace(/\s+/g, ' ');
    expect(copy).toContain('"Jodd" under YouTube → Settings → Linked devices');
    expect(copy).toContain('tidy');
    expect(copy).not.toMatch(/stops?\b|cuts? .* off|revoke|no longer (work|reach)/i);
    expect(calls('unpair_tv')).toHaveLength(0); // nothing until confirmed
  });

  it('unpair_default_is_optimistic_and_promotes_the_next_row', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    let seen: TvRow[] = [];
    handlers.unpair_tv = () => {
      seen = get(tvs); // the store was already updated when the invoke ran
      backend = [row('b', 'Bedroom', { is_default: true })];
      return null;
    };
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Unpair').click();
    await settle();
    const confirm = [...document.querySelectorAll<HTMLButtonElement>('dialog button')].find((b) => b.textContent!.trim() === 'Unpair')!;
    confirm.click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('unpair_tv', { tvId: 'a' });
    expect(seen.map((r) => [r.tv_id, r.is_default])).toEqual([['b', true]]);
    expect(text()).not.toContain('Living room');
    expect(text()).toContain('Bedroom');
  });

  it('unpair_failure_rolls_back_and_shows_the_error', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    handlers.unpair_tv = () => Promise.reject({ code: 'store' });
    await openMenu('Bedroom');
    btn(rowEl('Bedroom'), 'Unpair').click();
    await settle();
    [...document.querySelectorAll<HTMLButtonElement>('dialog button')].find((b) => b.textContent!.trim() === 'Unpair')!.click();
    await settle();
    expect(get(tvs).map((r) => r.tv_id)).toEqual(['a', 'b']);
    expect(text()).toContain('Bedroom');
    expect(error()).toBe("Couldn't save the TV list.");
  });

  it('set_default_is_optimistic_and_rolls_back_on_error', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    let seen: TvRow[] = [];
    handlers.set_default_tv = () => {
      seen = get(tvs);
      return Promise.reject({ code: 'store' });
    };
    await openMenu('Bedroom');
    btn(rowEl('Bedroom'), '★ Default').click();
    await settle();
    expect(seen.find((r) => r.tv_id === 'b')!.is_default).toBe(true);
    expect(get(tvs).find((r) => r.tv_id === 'a')!.is_default).toBe(true);
    expect(get(tvs).find((r) => r.tv_id === 'b')!.is_default).toBe(false);
    expect(error()).toBe("Couldn't save the TV list.");
  });

  it('rename_blank_is_rejected_without_invoke', async () => {
    await open([row('a', 'Living room', { is_default: true })]);
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Rename').click();
    await settle();
    await type(rowEl('Living room').querySelector<HTMLInputElement>('form input')!, '   ');
    btn(rowEl('Living room'), 'Save name').click();
    await settle();
    expect(calls('rename_tv')).toHaveLength(0);
    expect(error()).toBe('Give the TV a name.');
  });

  it('rename_updates_then_invokes_and_reloads', async () => {
    await open([row('a', 'Hisense HiSmartTV A4', { is_default: true })]);
    handlers.rename_tv = () => {
      backend = [row('a', 'Living room', { is_default: true })];
      return backend[0];
    };
    await openMenu('Hisense');
    btn(rowEl('Hisense'), 'Rename').click();
    await settle();
    await type(rowEl('Hisense').querySelector<HTMLInputElement>('form input')!, '  Living room ');
    btn(rowEl('Hisense'), 'Save name').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('rename_tv', { tvId: 'a', label: 'Living room' });
    expect(text()).toContain('Living room');
  });

  it('unpair_failure_reloads_the_list_and_shows_what_list_tvs_returned', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    handlers.unpair_tv = () => {
      // Another edit landed meanwhile: the rollback snapshot is stale.
      backend = [row('a', 'Den', { is_default: true }), row('b', 'Bedroom')];
      return Promise.reject({ code: 'store' });
    };
    const before = calls('list_tvs').length;
    await openMenu('Bedroom');
    btn(rowEl('Bedroom'), 'Unpair').click();
    await settle();
    [...document.querySelectorAll<HTMLButtonElement>('dialog button')].find((b) => b.textContent!.trim() === 'Unpair')!.click();
    await settle();
    expect(calls('list_tvs').length).toBe(before + 1);
    expect(get(tvs).map((r) => r.label)).toEqual(['Den', 'Bedroom']);
    expect(text()).toContain('Den');
    expect(text()).not.toContain('Living room');
    expect(error()).toBe("Couldn't save the TV list.");
  });

  it('two_refreshes_keep_their_own_busy_state', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    const release: Record<string, () => void> = {};
    handlers.refresh_tv = (args: { tvId: string }) => new Promise<void>((r) => { release[args.tvId] = r; });
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Refresh').click();
    await settle();
    await openMenu('Bedroom');
    btn(rowEl('Bedroom'), 'Refresh').click();
    await settle();
    expect(rowEl('Living room').textContent).toContain('Checking…');
    expect(rowEl('Bedroom').textContent).toContain('Checking…');
    release.a();
    await settle();
    expect(rowEl('Living room').textContent).not.toContain('Checking…');
    expect(rowEl('Bedroom').textContent).toContain('Checking…');
    await openMenu('Bedroom');
    for (const name of ['Refresh', 'Re-pair', 'Rename', 'Unpair', '★ Default']) {
      expect(btn(rowEl('Bedroom'), name).disabled).toBe(true);
    }
    release.b();
    await settle();
    expect(rowEl('Bedroom').textContent).not.toContain('Checking…');
  });

  it('checking_shows_even_while_another_row_has_a_repair_form_open', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    handlers.refresh_tv = () => new Promise<void>(() => {});
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Re-pair').click();
    await settle();
    await openMenu('Bedroom');
    btn(rowEl('Bedroom'), 'Refresh').click();
    await settle();
    expect(rowEl('Bedroom').textContent).toContain('Checking…');
  });

  it('pair_is_not_blocked_by_another_row_being_busy', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    handlers.refresh_tv = () => new Promise<void>(() => {});
    handlers.repair_tv = () => backend[0];
    await openMenu('Bedroom');
    btn(rowEl('Bedroom'), 'Refresh').click();
    await settle();
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Re-pair').click();
    await settle();
    await type(rowEl('Living room').querySelector<HTMLInputElement>('form input')!, '123456789012');
    expect(btn(rowEl('Living room'), 'Pair').disabled).toBe(false);
    btn(rowEl('Living room'), 'Pair').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('repair_tv', { tvId: 'a', code: '123456789012' });
  });

  it('default_row_star_default_item_is_disabled', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    await openMenu('Living room');
    expect(btn(rowEl('Living room'), '★ Default').disabled).toBe(true);
    btn(rowEl('Living room'), '★ Default').click();
    await settle();
    expect(calls('set_default_tv')).toHaveLength(0);
  });

  it('escape_closes_the_menu_refocuses_the_kebab_and_does_not_bubble', async () => {
    await open([row('a', 'Living room', { is_default: true })]);
    const outer = vi.fn();
    document.addEventListener('keydown', outer);
    await openMenu('Living room');
    const kebab = rowEl('Living room').querySelector<HTMLButtonElement>('button.kebab')!;
    const item = btn(rowEl('Living room'), 'Refresh');
    item.focus();
    const ev = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
    item.dispatchEvent(ev);
    await settle();
    document.removeEventListener('keydown', outer);
    expect(rowEl('Living room').querySelector('[role="menu"]')).toBeNull();
    expect(document.activeElement).toBe(kebab);
    expect(outer).not.toHaveBeenCalled();
  });

  it('rename_failure_keeps_the_form_and_the_typed_text', async () => {
    await open([row('a', 'Living room', { is_default: true })]);
    handlers.rename_tv = () => Promise.reject({ code: 'store' });
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Rename').click();
    await settle();
    await type(rowEl('Living room').querySelector<HTMLInputElement>('form input')!, 'Den');
    btn(rowEl('Living room'), 'Save name').click();
    await settle();
    expect(error()).toBe("Couldn't save the TV list.");
    expect(rowEl('Living room').querySelector<HTMLInputElement>('form input')!.value).toBe('Den');
  });

  it('rename_closes_at_once_and_shows_the_new_label_while_pending', async () => {
    await open([row('a', 'Living room', { is_default: true })]);
    let release!: () => void;
    handlers.rename_tv = () => new Promise<void>((r) => { release = r; });
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Rename').click();
    await settle();
    await type(rowEl('Living room').querySelector<HTMLInputElement>('form input')!, 'Den');
    btn(rowEl('Living room'), 'Save name').click();
    await settle();
    expect(host.querySelector('[data-tv-id="a"] form')).toBeNull();
    expect(rowEl('Den').querySelector('.label')!.textContent).toBe('Den');
    backend = [row('a', 'Den', { is_default: true })];
    release();
    await settle();
    expect(host.querySelector('[data-tv-id="a"] form')).toBeNull();
  });

  it('rename_failure_does_not_steal_a_rename_opened_on_another_row', async () => {
    await open([row('a', 'Living room', { is_default: true }), row('b', 'Bedroom')]);
    let fail!: () => void;
    handlers.rename_tv = () => new Promise<void>((_, rej) => { fail = () => rej({ code: 'store' }); });
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Rename').click();
    await settle();
    await type(rowEl('Living room').querySelector<HTMLInputElement>('form input')!, 'Den');
    btn(rowEl('Living room'), 'Save name').click();
    await settle();
    await openMenu('Bedroom');
    btn(rowEl('Bedroom'), 'Rename').click();
    await settle();
    fail();
    await settle();
    expect(rowEl('Bedroom').querySelector('form')).not.toBeNull();
    expect(rowEl('Living room').querySelector('form')).toBeNull();
    expect(error()).toBe("Couldn't save the TV list.");
  });

  it('not_found_reloads_silently', async () => {
    await open([row('a', 'Living room', { is_default: true })]);
    handlers.rename_tv = () => Promise.reject({ code: 'not_found' });
    await openMenu('Living room');
    btn(rowEl('Living room'), 'Rename').click();
    await settle();
    await type(rowEl('Living room').querySelector<HTMLInputElement>('form input')!, 'Den');
    btn(rowEl('Living room'), 'Save name').click();
    await settle();
    expect(error()).toBeNull();
    expect(calls('list_tvs').length).toBeGreaterThanOrEqual(1);
  });
});
