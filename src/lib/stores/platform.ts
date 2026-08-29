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

/** Pure, for the same reason `deriveIsAndroid` is. */
export function deriveIsMacos(platform: string | null): boolean {
  return platform === 'macos';
}

/**
 * True only on macOS.
 *
 * Gates the iCloud sign-in entry point. That backend's credential is a live
 * `WKWebView` cookie jar living in a per-webview persistent data store, which
 * wry maps to `WKWebsiteDataStore(forIdentifier:)` — a macOS 14+ API with no
 * counterpart on the other desktop platforms. Windows and Android sign-in are
 * each their own constraint chain and are deferred to M3; gotcha #8 is the
 * precedent for how expensive assuming otherwise gets.
 *
 * Starts false so an unsupported platform never flashes the button.
 */
export const isMacos = readable(false, (set) => {
  invoke<string>('platform_name')
    .then((p) => set(deriveIsMacos(p)))
    .catch(() => set(false));
});
