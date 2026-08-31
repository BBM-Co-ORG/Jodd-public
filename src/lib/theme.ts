// Theme preference. localStorage, matching the jodd: namespace whatsNew.ts
// already uses -- a second persistence layer for one enum would be silly.

export type ThemePref = 'system' | 'light' | 'dark';

export const THEME_KEY = 'jodd:theme';

const VALID: ThemePref[] = ['system', 'light', 'dark'];

// getThemePref() runs at module load in main.ts, before mount() -- an
// unguarded read that throws would white-screen the whole app before Svelte
// ever gets a chance to render. localStorage access can throw, not just
// return null (Safari private-mode style restrictions, and more relevantly
// here: a Tauri WKWebView/WebView2 instance with storage disabled by
// policy). Any failure falls back to 'system' rather than propagating.
export function getThemePref(): ThemePref {
  try {
    const v = localStorage.getItem(THEME_KEY);
    return VALID.includes(v as ThemePref) ? (v as ThemePref) : 'system';
  } catch {
    return 'system';
  }
}

// If the write throws, the choice won't survive a relaunch, but the user
// still sees it applied for this session -- applyTheme runs either way,
// never skipped because persistence failed.
export function setThemePref(pref: ThemePref): void {
  try {
    localStorage.setItem(THEME_KEY, pref);
  } catch {
    // Best-effort persistence only; applyTheme below still runs.
  }
  applyTheme(pref);
}

// 'system' removes the attribute rather than resolving it, so the
// prefers-color-scheme media query stays live and the app follows the OS
// when the user flips it mid-session -- no listener needed.
//
// Not guarded with try/catch: it only touches document.documentElement
// (setAttribute/removeAttribute on the root element Svelte itself mounts
// into), which isn't a storage API and isn't gated by the same webview
// policies that can disable localStorage. If this throws, the DOM itself is
// unusable and mount() is going to fail regardless.
export function applyTheme(pref: ThemePref): void {
  const root = document.documentElement;
  if (pref === 'system') root.removeAttribute('data-theme');
  else root.setAttribute('data-theme', pref);
}
