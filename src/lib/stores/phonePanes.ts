import { writable } from 'svelte/store';
export const phonePaneAccount = writable<string | null>(null);
