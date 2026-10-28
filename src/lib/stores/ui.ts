import { writable } from 'svelte/store';
import type { WhatsNewEntry } from '../whatsNew';
import type { AskScope } from '../components/askScope';
import type { ProposedAppend } from '../controllers/noteActions';

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

/**
 * Settings (spec 2026-10-20 §4.3): null while closed, otherwise the page to
 * show, so any opener can deep-link. Grammar: `section` or `section/sub`.
 * Sections: general | accounts | ai | privacy | speech | share | advanced.
 * Subs: `accounts/<accountId>` (that account's page) and `advanced/oauth`.
 * `''` is the section list itself, which only the phone shows on its own.
 * Parsed by `parseSettingsRoute` (lib/settings/route.ts).
 */
export const settingsRoute = writable<string | null>(null);

// Ask Jodd modal — opened by the sidebar 💬 entry. The conversation lives in
// the component and is discarded on close: answers are ephemeral by design.
export const askModalOpen = writable(false);

/**
 * A scope for Ask Jodd to start a fresh conversation in — set by the note,
 * folder and account context menus (spec 2026-10-23 §6.1). AskJoddModal
 * watches it whether or not it is already open, consumes it, and sets it
 * back to null.
 */
export const pendingAskScope = writable<AskScope | null>(null);

export function openAskWith(scope: AskScope): void {
  pendingAskScope.set(scope);
  askModalOpen.set(true);
}

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

/**
 * The add-account flows that need a component rather than a module.
 *
 * Local Folder holds Android plugin watchers and a busy guard (Tauri's one
 * activity-result slot), and both SSH flows open a dialog someone has to
 * mount. `startAddAccount` asks for them here, and `AccountAddHost` — mounted
 * once by App.svelte in every authenticated layout — runs them. Each request
 * is a new object with a higher `seq`, so asking twice for the same kind is
 * two requests.
 *
 * The host is the ONLY subscriber, and it consumes a request by setting the
 * store back to null. A request made before the host mounts is therefore
 * kept, not dropped, and handled when it mounts; a consumed one is never
 * replayed.
 */
export type HostAddAccountKind = 'localfs' | 'ssh' | 'ssh_advanced';
export const addAccountRequest = writable<{ kind: HostAddAccountKind; seq: number } | null>(null);

/**
 * True while AccountAddHost's Local Folder flow is in flight. A store, not
 * host state, because the buttons that start the flow live elsewhere (the
 * Sidebar's account panel, Settings › Accounts) and must show it disabled:
 * a second launch would orphan the first plugin call forever.
 */
export const localFolderBusy = writable(false);

/**
 * "Find related notes" proposals waiting for review (LinkSuggestionsModal,
 * mounted once in App.svelte). App-level because the menu that started the
 * run has closed by the time it answers, and on a phone the note list that
 * used to own the dialog is not even on screen while the editor is.
 */
export const linkSuggestions = writable<{ accountId: string; appends: ProposedAppend[] } | null>(null);
export function showLinkSuggestions(accountId: string, appends: ProposedAppend[]): void {
  linkSuggestions.set({ accountId, appends });
}
