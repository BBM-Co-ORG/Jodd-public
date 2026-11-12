// Whether the sidebar's Views section is open. One choice per device, shared
// by the desktop/tablet Sidebar and the phone's Folders pane, so folding it on
// one layout folds it on the other. Closed by default, like Tags, so the
// folders start near the top; the stored value is whether it was OPENED, so a
// fresh install (or unreadable storage) reads as closed.
export const VIEWS_OPEN_KEY = 'jodd.sidebar.viewsOpen';

export function loadViewsOpen(): boolean {
  try {
    return localStorage.getItem(VIEWS_OPEN_KEY) === 'true';
  } catch {
    return false;
  }
}

export function saveViewsOpen(open: boolean): void {
  try {
    localStorage.setItem(VIEWS_OPEN_KEY, String(open));
  } catch {
    // Non-fatal: the toggle still works for this session.
  }
}
