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
import App from './App.svelte';
import { applyTheme, getThemePref } from './lib/theme';

// Before mount: stamping data-theme after the first paint shows a flash of
// the wrong theme on every launch.
applyTheme(getThemePref());

const app = mount(App, {
  target: document.getElementById('app')!,
});

export default app;
