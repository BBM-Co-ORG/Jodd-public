// Keyboard for the Sidebar's anchored context menus (folder, account, tag):
// the keys their `role="menu"` promises (spec 2026-11-10, Keyboard and screen
// reader). Before this a menu opened from Shift+F10 left focus on the row
// behind it, so the only key that did anything was Esc.
//
//   open          focus the first item
//   ↑ ↓ Home End  move between the menu's own items (wrapping)
//   →             on an item that opens a submenu (Move to): open it, focus
//                 its first item
//   ← / Esc       inside a submenu: close it, back to the item that opened it
//   Tab           close the menu (the menu is not a stop in the page's order)
//   close         focus goes back to what opened the menu, unless something
//                 else (a dialog the item opened, another row) has taken it
//
// Esc on the menu itself is the window handler's, which closes every menu.

import { tick } from 'svelte';

const ITEM = '[role="menuitem"]';
const MENU = '[role="menu"]';

function items(menu: Element): HTMLElement[] {
  return Array.from(menu.querySelectorAll<HTMLElement>(ITEM)).filter(
    (el) => el.closest(MENU) === menu && !(el as HTMLButtonElement).disabled,
  );
}

/** The submenu a trigger opens: the `role="menu"` beside it. */
function submenuOf(trigger: HTMLElement): HTMLElement | null {
  return trigger.parentElement?.querySelector<HTMLElement>(`:scope > ${MENU}`) ?? null;
}

/** The trigger that opened a submenu: the `aria-haspopup` item beside it. */
function triggerOf(sub: Element): HTMLElement | null {
  return sub.parentElement?.querySelector<HTMLElement>(':scope > [aria-haspopup]') ?? null;
}

export function nextIndex(key: string, at: number, count: number): number | null {
  if (count === 0) return null;
  switch (key) {
    case 'ArrowDown': return at < 0 ? 0 : (at + 1) % count;
    case 'ArrowUp': return at < 0 ? count - 1 : (at - 1 + count) % count;
    case 'Home': return 0;
    case 'End': return count - 1;
    default: return null;
  }
}

export function menuKeys(node: HTMLElement, params: { onclose: () => void }) {
  let { onclose } = params;
  const active = document.activeElement;
  const opener = active instanceof HTMLElement && active !== document.body && !node.contains(active) ? active : null;

  // After the menu has painted at its fitted position.
  void tick().then(() => {
    if (node.isConnected && !node.contains(document.activeElement)) items(node)[0]?.focus({ preventScroll: true });
  });

  async function openSubmenu(trigger: HTMLElement) {
    if (trigger.getAttribute('aria-expanded') !== 'true') trigger.click();
    await tick();
    const sub = submenuOf(trigger);
    if (sub) items(sub)[0]?.focus({ preventScroll: true });
  }

  function closeSubmenu(sub: Element) {
    const trigger = triggerOf(sub);
    if (!trigger) return;
    if (trigger.getAttribute('aria-expanded') === 'true') trigger.click();
    trigger.focus({ preventScroll: true });
  }

  function onKeyDown(e: KeyboardEvent) {
    const el = document.activeElement as HTMLElement | null;
    const menu = el && node.contains(el) ? el.closest(MENU) ?? node : node;
    const inSubmenu = menu !== node;

    if (e.key === 'Tab') {
      e.preventDefault();
      onclose();
      return;
    }
    if ((e.key === 'ArrowLeft' || e.key === 'Escape') && inSubmenu) {
      e.preventDefault();
      e.stopPropagation();
      closeSubmenu(menu);
      return;
    }
    if (e.key === 'ArrowRight' && el?.matches('[aria-haspopup]')) {
      e.preventDefault();
      void openSubmenu(el);
      return;
    }
    const list = items(menu);
    const next = nextIndex(e.key, el ? list.indexOf(el) : -1, list.length);
    if (next === null) return;
    e.preventDefault();
    list[next].focus({ preventScroll: true });
  }

  node.addEventListener('keydown', onKeyDown);

  return {
    update(next: { onclose: () => void }) {
      onclose = next.onclose;
    },
    destroy() {
      node.removeEventListener('keydown', onKeyDown);
      const now = document.activeElement;
      const lost = !now || now === document.body || node.contains(now);
      if (opener?.isConnected && lost) opener.focus({ preventScroll: true });
    },
  };
}
