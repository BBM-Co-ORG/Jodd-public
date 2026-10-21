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
 * True only on macOS. Kept for anything genuinely Apple-specific.
 *
 * **It no longer gates iCloud sign-in**, which is what it was introduced for.
 * See `supportsIcloud`.
 *
 * Starts false so an Apple-only affordance never flashes on other platforms.
 */
export const isMacos = readable(false, (set) => {
  invoke<string>('platform_name')
    .then((p) => set(deriveIsMacos(p)))
    .catch(() => set(false));
});

/**
 * Whether this platform can hold an iCloud session at all.
 *
 * **This used to be `isMacos`, on a premise that turned out to be false.** The
 * reason given was that the session lives in a per-webview persistent data
 * store, "a macOS 14+ API with no counterpart on the other desktop platforms".
 * Windows has one: `data_store_identifier` is Apple's lever and
 * `data_directory` is everyone else's, and Tauri keys its web context on the
 * path exactly as WebKit keys its store on the identifier. `icloud_auth`'s
 * `isolated()` is the seam.
 *
 * Measured live on WebView2 on 2026-09-09 with
 * `examples/icloud_webview_probe`: sign-in completes, the HttpOnly session
 * cookies are harvestable, the injected script wins its race, the jar is
 * isolated from Jodd's own profile, the hidden session webview
 * re-authenticates silently, and the session survives quitting the app —
 * every answer identical to macOS. The shipped cookie-matching layer needed
 * no changes.
 *
 * **Only the platforms a probe or a device pass has actually run on are
 * listed.** Android joined on 2026-09-10, on a device pass rather than an
 * argument: the cookie jar there is the **process's**
 * (`CookieManager.getInstance()`), not a webview's, so the session outlives
 * the sign-in webview with no hidden successor to keep alive. The three
 * Android APIs that made this look impossible — `cookies()`, `set_visible`,
 * `clear_all_browsing_data` — each compile and perform no part of their
 * name; see `icloud_auth.rs` for what replaced them. Linux is still absent
 * for the reason this list exists: the probe would probably pass there and
 * nobody has run it, and Jodd does not ship a Linux target. Unmeasured is
 * not offered; add it by running the probe (desktop) or the device pass
 * (mobile), not by reasoning about it.
 *
 * Starts false so an unsupported platform never flashes the button.
 *
 * **An allowlist, not `!== 'android'`.** This gate must fail CLOSED — unlike
 * `isAndroid`, which fails open so an unrecognised desktop keeps its features
 * — and a negative rule fails closed only for the one string it names. It
 * would answer `true` for `''`, and for whatever a future Tauri reports for a
 * platform nobody has measured. Naming the platforms means adding one is a
 * deliberate act with a probe run behind it, which is the same discipline
 * `canonical_uuid_for` and `remote_pin_policy` enforce with no wildcard arm.
 */
const ICLOUD_PLATFORMS = ['macos', 'windows', 'android'] as const;

export function deriveSupportsIcloud(platform: string | null): boolean {
  return ICLOUD_PLATFORMS.includes(platform as (typeof ICLOUD_PLATFORMS)[number]);
}

/** See {@link deriveSupportsIcloud}. */
export const supportsIcloud = readable(false, (set) => {
  invoke<string>('platform_name')
    .then((p) => set(deriveSupportsIcloud(p)))
    .catch(() => set(false));
});
