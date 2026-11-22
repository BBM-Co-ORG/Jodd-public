// Keyboard movement through a column of navigation rows: the Sidebar's
// folders, views and Recently Deleted (2026-11-09 workspace review, audit P1).
//
// The tree-view keys, applied to the rows as they are laid out:
//   ↑ ↓      previous / next row          Home End   first / last row
//   →        open a closed folder; on an open one, step into its first child
//   ←        close an open folder; otherwise step out to the parent
//   Shift+F10 / ContextMenu   the row's menu (rename, move, …)
// plus one Tab stop for the whole column (roving tabindex), so Tab leaves the
// sidebar in one press instead of one press per folder.
//
// Rows stay `role="button"` with `aria-expanded` and `aria-current`: the
// column mixes folders with group headers and other controls, which a
// `role="tree"` may not contain.

export interface NavRowInfo {
  depth: number;
  hasKids: boolean;
  expanded: boolean;
}

export type NavMove =
  | { kind: 'focus'; index: number }
  | { kind: 'expand' }
  | { kind: 'collapse' }
  | { kind: 'menu' }
  | null;

export function navKeyMove(key: string, shift: boolean, rows: NavRowInfo[], i: number): NavMove {
  const last = rows.length - 1;
  const row = rows[i];
  if (!row) return null;
  const to = (index: number): NavMove => (index === i ? null : { kind: 'focus', index });
  switch (key) {
    case 'ArrowDown': return to(Math.min(i + 1, last));
    case 'ArrowUp': return to(Math.max(i - 1, 0));
    case 'Home': return to(0);
    case 'End': return to(last);
    case 'ArrowRight':
      if (!row.hasKids) return null;
      return row.expanded ? to(Math.min(i + 1, last)) : { kind: 'expand' };
    case 'ArrowLeft': {
      if (row.hasKids && row.expanded) return { kind: 'collapse' };
      for (let p = i - 1; p >= 0; p--) if (rows[p].depth < row.depth) return to(p);
      return null;
    }
    case 'ContextMenu': return { kind: 'menu' };
    case 'F10': return shift ? { kind: 'menu' } : null;
    default: return null;
  }
}

const ROW = '.folder-item';

function info(el: HTMLElement): NavRowInfo {
  const exp = el.getAttribute('aria-expanded');
  return { depth: Number(el.dataset.depth ?? 0), hasKids: exp !== null, expanded: exp === 'true' };
}

/**
 * Svelte action for the column's root. Rows are the `.folder-item`
 * elements inside it; a row with children carries `aria-expanded` and an
 * `.expand-toggle`, and a nested row carries `data-depth`.
 */
export function navRows(root: HTMLElement) {
  let lastFocused: HTMLElement | null = null;

  const all = () => Array.from(root.querySelectorAll<HTMLElement>(ROW));

  function syncTabStops() {
    const rows = all();
    const stop =
      (lastFocused && lastFocused.isConnected && root.contains(lastFocused) ? lastFocused : null) ??
      root.querySelector<HTMLElement>(`${ROW}.active`) ??
      rows[0];
    for (const r of rows) {
      const want = r === stop ? '0' : '-1';
      if (r.getAttribute('tabindex') !== want) r.setAttribute('tabindex', want);
    }
  }

  function onFocusIn(e: FocusEvent) {
    const row = (e.target as HTMLElement).closest<HTMLElement>(ROW);
    if (row && root.contains(row)) {
      lastFocused = row;
      syncTabStops();
    }
  }

  function onKeyDown(e: KeyboardEvent) {
    const row = e.target as HTMLElement;
    if (!row.matches?.(ROW) || !root.contains(row)) return;
    const rows = all();
    const i = rows.indexOf(row);
    const move = navKeyMove(e.key, e.shiftKey, rows.map(info), i);
    if (!move) return;
    e.preventDefault();
    if (move.kind === 'focus') {
      rows[move.index].focus();
      rows[move.index].scrollIntoView?.({ block: 'nearest' });
    } else if (move.kind === 'expand' || move.kind === 'collapse') {
      row.querySelector<HTMLElement>('.expand-toggle')?.click();
    } else {
      // The row's own contextmenu handler opens its menu; anchored to the row.
      const r = row.getBoundingClientRect();
      row.dispatchEvent(new MouseEvent('contextmenu', {
        bubbles: true, cancelable: true, clientX: r.left + 24, clientY: r.bottom,
      }));
    }
  }

  // Rows come and go (expand, account switch, a new folder): keep exactly
  // one Tab stop, preferring the row focused last, then the open one.
  const observer = new MutationObserver(syncTabStops);
  observer.observe(root, { childList: true, subtree: true, attributes: true, attributeFilter: ['class'] });
  root.addEventListener('focusin', onFocusIn);
  root.addEventListener('keydown', onKeyDown);
  syncTabStops();

  return {
    destroy() {
      observer.disconnect();
      root.removeEventListener('focusin', onFocusIn);
      root.removeEventListener('keydown', onKeyDown);
    },
  };
}
