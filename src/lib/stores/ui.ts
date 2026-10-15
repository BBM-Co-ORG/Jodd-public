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

// Ask Jodd modal — opened by the sidebar 💬 entry. The conversation lives in
// the component and is discarded on close: answers are ephemeral by design.
export const askModalOpen = writable(false);

/**
 * An authorization server refused the sign-in and there is something the user
 * can do about it.
 */
export interface SignInBlock {
  message: string;
  /**
   * Where a tenant administrator grants Jodd consent for their organisation.
   * Present on every Microsoft refusal — Microsoft's redirect for "your org
   * must approve this app" is byte-identical to a plain cancel, so Jodd
   * cannot tell which happened and always offers the link rather than
   * guessing. See `signin_denial` in src-tauri/src/lib.rs.
   */
  adminConsentUrl: string | null;
}

// Populated by App.svelte's `oauth-error` listener, and the reason it exists
// as a store rather than reusing `error`: `$error` is only ever rendered
// inside NoteEditor, which is not mounted while AuthScreen is showing — so a
// failure during first-run sign-in had no surface at all. A store read by a
// component rendered ABOVE the isAuthenticated branch covers both first-run
// and add-a-second-account with one panel.
export const signInBlocked = writable<SignInBlock | null>(null);
