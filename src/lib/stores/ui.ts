import { writable } from 'svelte/store';
import type { WhatsNewEntry } from '../whatsNew';

// Cross-component UI toggle for the lesson-extraction modal.
// Opened by the sidebar 💡 button, the Cmd+Shift+L global hotkey,
// and (closed) by the modal itself on submit/cancel.
export const extractModalOpen = writable(false);

// App-level "About" modal — opened by the Sidebar footer version label.
export const aboutModalOpen = writable(false);

// "What's New" modal — opened from About, and auto-shown once per version
// bump by App.svelte (compares getVersion() to a localStorage last-seen value).
export const whatsNewOpen = writable(false);
// Entries to render in the modal. Populated by whichever trigger opened it
// (see src/lib/whatsNew.ts) — the store only holds the result, not the logic.
export const whatsNewVersions = writable<WhatsNewEntry[]>([]);

// App-level settings modal (Google OAuth credentials, future global prefs).
// Opened by the ⚙ gear button in the sidebar footer.
export const appSettingsOpen = writable(false);
