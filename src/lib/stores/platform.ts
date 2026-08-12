import { readable } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';

/** Pure so the gating rule is testable without a Tauri runtime. */
export function deriveIsAndroid(platform: string | null): boolean {
  return platform === 'android';
}

/**
 * True only on Android. Used to hide capabilities the platform genuinely
 * cannot provide: LocalFS vaults (no arbitrary filesystem) and agent-CLI LLM
 * providers (no child processes).
 *
 * **BYO OAuth credentials are NOT one of them, and used to be.** The reason
 * given was that an Android OAuth client is bound to package name plus signing
 * fingerprint, so a user's own client could not work. True of the Android
 * client type — which Jodd no longer uses, because Google withdrew the custom
 * URI scheme it depends on. Android now uses a Web-application client, and
 * `auth::client_id()` consults a stored override before the platform split, so
 * the backend has honored BYO on Android since that change. The settings
 * section stayed hidden for another day on reasoning that had already been
 * deleted from `auth.rs`.
 *
 * Starts false so the UI never flashes features away on desktop.
 */
export const isAndroid = readable(false, (set) => {
  invoke<string>('platform_name')
    .then((p) => set(deriveIsAndroid(p)))
    .catch(() => set(false));
});
