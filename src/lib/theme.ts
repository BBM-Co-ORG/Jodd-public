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

// ─── Look ───────────────────────────────────────────────────────────────────
// A second, independent axis: which visual world (palette, type, one
// signature move), while ThemePref above still picks light, dark or the OS.
// Every look ships both, so the two choices combine freely. Lamplit Desk is
// tokens.css itself and sets no attribute; each other look is a stylesheet in
// src/styles/looks/ scoped to :root[data-look='<id>'].

export type LookPref = 'lamplit' | 'instrument' | 'midnight' | 'slate';

export const LOOK_KEY = 'jodd:look';

export interface LookInfo {
  id: LookPref;
  label: string;
  /** One line for Settings. */
  blurb: string;
  /** Sidebar, list, editor, accent: the picker's swatch, one row per mode. */
  light: [string, string, string, string];
  dark: [string, string, string, string];
  /** The look's UI face, for its name in the picker. */
  face: string;
}

export const LOOKS: LookInfo[] = [
  { id: 'lamplit', label: 'Lamplit Desk', blurb: 'Warm paper under an amber lamp.', light: ['#f0ebe0', '#faf6ed', '#fffef9', '#c97c1f'], dark: ['#26211c', '#1f1b17', '#1a1714', '#d99a45'], face: "'IBM Plex Sans Thai'" },
  { id: 'instrument', label: 'Field Instrument', blurb: 'Cool aluminium, crisp labels, one signal orange.', light: ['#dcdcd8', '#e8e8e4', '#f4f4f1', '#ff4f00'], dark: ['#1b1c1e', '#222326', '#2a2b2e', '#ff6a1a'], face: "'Bai Jamjuree'" },
  { id: 'midnight', label: 'Midnight Line', blurb: 'Each account a transit line, each folder a station.', light: ['#e4e9f1', '#eef1f6', '#f7f8fb', '#2a63e0'], dark: ['#0e1830', '#13203c', '#182848', '#3e7bfa'], face: "'Anuphan'" },
  { id: 'slate', label: 'Slate Ink', blurb: 'E-paper greys and black ink, no colour.', light: ['#dededa', '#e6e6e2', '#efefeb', '#161616'], dark: ['#1c1c1b', '#212120', '#262625', '#e8e8e4'], face: "'Anuphan'" },
];

const LOOK_IDS = LOOKS.map((l) => l.id);

// Same guard as getThemePref: this runs before mount, so it never throws.
export function getLookPref(): LookPref {
  try {
    const v = localStorage.getItem(LOOK_KEY);
    return LOOK_IDS.includes(v as LookPref) ? (v as LookPref) : 'lamplit';
  } catch {
    return 'lamplit';
  }
}

export function setLookPref(look: LookPref): void {
  try {
    localStorage.setItem(LOOK_KEY, look);
  } catch {
    // Best-effort persistence only; applyLook below still runs.
  }
  applyLook(look);
}

export function applyLook(look: LookPref): void {
  const root = document.documentElement;
  if (look === 'lamplit') root.removeAttribute('data-look');
  else root.setAttribute('data-look', look);
}
