// Opening a link that sits inside the note body.
//
// The body is contenteditable, so a plain click on an <a> must keep doing
// what it does in every editor: put the caret there so the link text can be
// edited. Opening is the shortcut modifier + click (⌘ on macOS, Ctrl
// elsewhere — `isShortcutMod`), as in Apple Notes and Google Docs.
//
// tauri-plugin-opener also listens for link clicks, on `window`, and opens
// one in the system browser on Ctrl+click or Shift+click unless the click was
// already default-prevented. Left alone, Shift+click — extend the selection —
// would launch the browser, and on Windows Ctrl+click would open twice. So
// every click on an external link here is default-prevented: the caret and
// selection already moved on mousedown, and the stored HTML is never touched.

import { isShortcutMod } from './shortcutKeys';

const OPENABLE = ['http:', 'https:', 'mailto:'];

/** The http(s)/mailto URL of the link around `target`, or null. In-note
 * anchors (`#meeting-…`) and anything relative or scripted are not links out. */
export function externalHref(target: EventTarget | null): string | null {
  const anchor = (target as Element | null)?.closest?.('a');
  const raw = anchor?.getAttribute('href');
  if (!raw) return null;
  try {
    const url = new URL(raw);
    return OPENABLE.includes(url.protocol) ? raw : null;
  } catch {
    return null;
  }
}

/** Handle a click in the editor body. Returns true when it opened a link,
 * in which case the caller has nothing left to do with the click. */
export function handleEditorLinkClick(e: MouseEvent, mac: boolean, open: (href: string) => void): boolean {
  if (e.button !== 0) return false;
  const href = externalHref(e.target);
  if (!href) return false;
  e.preventDefault();
  if (!isShortcutMod(e, mac)) return false;
  open(href);
  return true;
}
