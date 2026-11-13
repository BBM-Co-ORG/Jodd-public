import { writable } from 'svelte/store';
/** The account the phone Tags pane lists, or null for "the current one". */
export const phonePaneAccount = writable<string | null>(null);
/** The Tags pane lists every active account's tags instead (spec 2026-11-13 §3.7). */
export const phonePaneAllAccounts = writable(false);
