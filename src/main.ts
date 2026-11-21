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
// Thai at 700 must ship too: these subset files carry no unicode-range, so a
// missing Thai 700 face makes Thai text set at 700 (note titles, Heading 1)
// fall through to the OS Thai font -- a different face on Mac, Windows and
// Android.
import '@fontsource/ibm-plex-sans-thai/thai-700.css';
import '@fontsource/ibm-plex-mono/latin-400.css';
import '@fontsource/ibm-plex-mono/latin-500.css';
// The other looks' faces (src/styles/looks/). @font-face only downloads a
// file when text actually uses that family, so a look you never pick costs
// bundle size, not startup. Same weights as Plex, Latin and Thai both, for
// the same reason (designGuards: bundled weights).
import '@fontsource/bai-jamjuree/latin-400.css';
import '@fontsource/bai-jamjuree/latin-600.css';
import '@fontsource/bai-jamjuree/latin-700.css';
import '@fontsource/bai-jamjuree/thai-400.css';
import '@fontsource/bai-jamjuree/thai-600.css';
import '@fontsource/bai-jamjuree/thai-700.css';
import '@fontsource/anuphan/latin-400.css';
import '@fontsource/anuphan/latin-600.css';
import '@fontsource/anuphan/latin-700.css';
import '@fontsource/anuphan/thai-400.css';
import '@fontsource/anuphan/thai-600.css';
import '@fontsource/anuphan/thai-700.css';
import '@fontsource/noto-serif-thai/latin-400.css';
import '@fontsource/noto-serif-thai/latin-600.css';
import '@fontsource/noto-serif-thai/latin-700.css';
import '@fontsource/noto-serif-thai/thai-400.css';
import '@fontsource/noto-serif-thai/thai-600.css';
import '@fontsource/noto-serif-thai/thai-700.css';
import '@fontsource/geist-mono/latin-400.css';
import '@fontsource/geist-mono/latin-500.css';

import './styles/tokens.css';
// After tokens.css: each look overrides the Lamplit tokens it re-colours.
import './styles/looks/instrument.css';
import './styles/looks/midnight.css';
import './styles/looks/slate.css';
// Phone bottom-sheet menu rows, shared by every sheet menu (no per-component copies).
import './styles/sheet-menu.css';
import App from './App.svelte';
import CacheLockedScreen from './lib/components/CacheLockedScreen.svelte';
import { readCacheLock } from './lib/cacheLock';
import { applyLook, applyTheme, getLookPref, getThemePref } from './lib/theme';

// Before mount: stamping data-theme after the first paint shows a flash of
// the wrong theme on every launch. The look, likewise.
applyTheme(getThemePref());
applyLook(getLookPref());

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
