import { mount } from 'svelte';

// Self-hosted, no CDN: Jodd runs offline and a fonts.googleapis.com link
// would be both a startup dependency and a CSP problem. Only the subsets and
// weights actually used -- no variable build of Plex Sans Thai exists on npm,
// so every weight is a separate file and the list has to stay short.
import '@fontsource/ibm-plex-sans-thai/latin-400.css';
import '@fontsource/ibm-plex-sans-thai/latin-600.css';
import '@fontsource/ibm-plex-sans-thai/latin-700.css';
import '@fontsource/ibm-plex-sans-thai/thai-400.css';
import '@fontsource/ibm-plex-sans-thai/thai-600.css';
import '@fontsource/ibm-plex-mono/latin-400.css';
import '@fontsource/ibm-plex-mono/latin-500.css';

import './styles/tokens.css';
// Phone bottom-sheet menu rows, shared by every sheet menu (no per-component copies).
import './styles/sheet-menu.css';
import App from './App.svelte';
import CacheLockedScreen from './lib/components/CacheLockedScreen.svelte';
import { readCacheLock } from './lib/cacheLock';
import { applyTheme, getThemePref } from './lib/theme';

// Before mount: stamping data-theme after the first paint shows a flash of
// the wrong theme on every launch.
applyTheme(getThemePref());

const target = document.getElementById('app')!;

// The local cache did not open (src-tauri/src/cache_lock.rs): mount the unlock
// screen and NOT App. Decided before App exists, so no component mounts and
// invokes beside a throwaway cache — 2026-10-26 a Remove click there deleted a
// live account's refresh token.
const cacheLock = readCacheLock();
const app = cacheLock
  ? mount(CacheLockedScreen, { target, props: { lock: cacheLock } })
  : mount(App, { target });

export default app;
