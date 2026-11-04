// Copy text to the system clipboard — the one copy helper; do not grow another.
//
// `navigator.clipboard` is not reliable inside the Tauri webview: it needs a
// secure-context permission the app window does not always satisfy, and it
// fails by rejecting, so a button that only tried it appeared to do nothing
// (reported from a live run of AccountPage's copy buttons). The fallback is a
// selection-based copy through a hidden textarea, which needs no permission.
//
// That fallback selects the textarea — and the window selection is the
// editor's caret (gotcha #34). It is saved before and restored after, with
// focus, so a copy leaves the caret exactly where it was.

/** Copy `text`. True when one of the two paths reports success. */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (!navigator.clipboard?.writeText) throw new Error('no navigator.clipboard');
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return copyViaSelection(text);
  }
}

function copyViaSelection(text: string): boolean {
  const sel = window.getSelection();
  const saved: Range[] = [];
  if (sel) for (let i = 0; i < sel.rangeCount; i++) saved.push(sel.getRangeAt(i).cloneRange());
  const focused = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  const ta = document.createElement('textarea');
  ta.value = text;
  ta.setAttribute('readonly', '');
  ta.setAttribute('aria-hidden', 'true');
  ta.style.position = 'fixed';
  ta.style.top = '0';
  ta.style.left = '0';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  let ok = false;
  try {
    ta.select();
    ok = document.execCommand('copy') === true;
  } catch {
    ok = false;
  } finally {
    ta.remove();
    // Focus first: focusing a contenteditable can move the selection itself.
    if (focused && focused !== document.body && focused.isConnected) focused.focus({ preventScroll: true });
    if (sel) {
      sel.removeAllRanges();
      for (const r of saved) sel.addRange(r);
    }
  }
  return ok;
}
