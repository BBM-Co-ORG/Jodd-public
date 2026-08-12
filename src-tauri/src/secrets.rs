//! Credential-store registration.
//!
//! keyring-core deliberately does not pick a platform store for you:
//! `Entry::new` fails with `NoDefaultStore` until `set_default_store` is
//! called. That explicitness is why we use it directly instead of the
//! `keyring` distribution crate, whose v1 compatibility shim (a) never
//! registers anything on Android at all, and (b) initializes lazily behind an
//! AtomicBool guard that lets a late thread skip initialization rather than
//! wait for it, so concurrent first-callers get NoDefaultStore
//! (keyring-4.1.5/src/v1.rs:47).
//!
//! The per-platform selection below mirrors that shim's `set_credential_store`
//! for the desktop targets, and adds the Android arm it omits.

use std::sync::Arc;
use keyring_core::CredentialStore;

/// Build this platform's credential store.
fn platform_store() -> Result<Arc<CredentialStore>, String> {
    #[cfg(target_os = "macos")]
    {
        Ok(apple_native_keyring_store::keychain::Store::new()
            .map_err(|e| format!("macOS keychain store: {e}"))?)
    }
    #[cfg(target_os = "windows")]
    {
        Ok(windows_native_keyring_store::Store::new()
            .map_err(|e| format!("Windows credential store: {e}"))?)
    }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
    {
        Ok(zbus_secret_service_keyring_store::Store::new()
            .map_err(|e| format!("secret-service store: {e}"))?)
    }
    #[cfg(target_os = "android")]
    {
        init_ndk_context()?;
        // NOT LegacyStore — the crate's README deprecates the by_service
        // layout. `Store` is the by_store implementation, re-exported at the
        // crate root. It encrypts SharedPreferences with keys held in the
        // Android Keystore.
        Ok(android_native_keyring_store::Store::new()
            .map_err(|e| format!("Android keystore: {e}"))?)
    }
}

/// Hand the JavaVM and Activity to `ndk-context` before anything asks it for
/// them.
///
/// `android-native-keyring-store` reaches the Android Keystore through
/// `ndk_context::android_context()`, and that function **panics** —
/// `.expect("android context was not initialized")` — rather than returning an
/// error. The panic happens inside the JNI-called `_start_app`, where unwinding
/// across the FFI boundary is not allowed, so Tauri's `stop_unwind` turns it
/// into `abort()`: the whole process dies at launch, before the WebView exists.
/// Observed on both an emulator and a physical Android 13 device.
///
/// `ndk-context`'s own docs say `ndk-glue` initializes it before `main`, and
/// this crate's design spec repeated the claim that Tauri Mobile does the same.
/// It does not: `cargo tree -i ndk-context` shows the only path to it is
/// `android-native-keyring-store`, with tauri/wry/tao nowhere in that subtree.
/// Nobody was ever going to call it for us.
///
/// tao does hold the values, though — it stores them when the Activity is
/// created, which the crash backtrace shows happening before `_start_app`
/// (`ndk_glue::create` → `_start_app`). So `main_android_context()` is already
/// populated by the time `run()` starts, and this stays a plain synchronous
/// call. The alternative, `RuntimeHandle::run_on_android_context`, dispatches a
/// closure to run later on the Android main thread — which would leave every
/// credential read racing the initialization instead of following it.
#[cfg(target_os = "android")]
fn init_ndk_context() -> Result<(), String> {
    // `prelude`, not `ndk_glue`: tao's platform/android.rs flattens the module
    // with `pub use crate::platform_impl::ndk_glue::*` inside `prelude`, so
    // there is no `platform::android::ndk_glue` path to import.
    use tauri::tao::platform::android::prelude as ndk_glue;

    // `initialize_android_context` asserts the slot was empty, so a second call
    // would panic exactly like the problem being fixed. `Once` also makes this
    // safe under the concurrent first-callers that `init()` already guards for.
    static NDK: std::sync::Once = std::sync::Once::new();
    let mut outcome = Ok(());
    NDK.call_once(|| {
        match ndk_glue::main_android_context() {
            Some(ctx) => unsafe {
                ndk_context::initialize_android_context(ctx.java_vm, ctx.context_jobject);
            },
            // Returning an error rather than letting the panic through is the
            // whole point: a missing context becomes a credential store that
            // fails to register, which `init()`'s caller logs, instead of an
            // abort with no diagnosis path.
            None => {
                outcome = Err(
                    "tao has no Android context yet — secrets::init() ran before the Activity"
                        .to_string(),
                )
            }
        }
    });
    outcome
}

static INIT: std::sync::Once = std::sync::Once::new();
static INIT_RESULT: std::sync::Mutex<Option<Result<(), String>>> = std::sync::Mutex::new(None);

/// Register this platform's credential store. Idempotent — later calls return
/// the first call's outcome. MUST run before any credential access, and while
/// the process is still single-threaded: `Once` makes concurrent callers wait
/// rather than race, which is precisely what the shim we are replacing failed
/// to do.
pub fn init() -> Result<(), String> {
    INIT.call_once(|| {
        let outcome = platform_store().map(keyring_core::set_default_store);
        *INIT_RESULT.lock().expect("INIT_RESULT poisoned") = Some(outcome);
    });
    INIT_RESULT
        .lock()
        .expect("INIT_RESULT poisoned")
        .clone()
        .expect("call_once always sets INIT_RESULT")
}

// ─── Pending PKCE persistence ────────────────────────────────────────────────
//
// `AppState.pending_pkce` (lib.rs) lives only in process memory, which is
// fine on desktop — the OAuth loopback listener is a spawned task that dies
// with the process too, so there is nothing to resume after a crash — but
// dead by construction on Android: the OS is free to evict Jodd's process
// while the user is still looking at Google's consent screen in Chrome
// ("Don't keep activities" makes this the common case, not the rare one),
// and the redirect Intent then cold-launches a fresh process with
// `pending_pkce` reset to `None`. `complete_oauth` would take the `None`
// branch and report "PKCE verifier missing" — true of the fresh process,
// misleading about the actual cause (process death, not a crypto bug).
//
// These three functions give the PKCE pair a second, durable home through
// the same credential store as everything else in this file, so
// `complete_oauth` can fall back to it when the in-memory slot is empty.
// Single slot, like `AppState.pending_pkce` itself: only one OAuth flow can
// be in progress at a time, and starting a new one (`get_auth_url`)
// overwrites whatever was here.
//
// Called unconditionally on every platform, not `#[cfg(target_os =
// "android")]`-gated: one code path is worth more than a saved keychain
// write, and on desktop this is harmless — the loopback listener still dies
// with the process, so nothing there depends on this ever being read back.

const PKCE_KC_SERVICE: &str = "jodd";
const PENDING_PKCE_KEY: &str = "pending_pkce";

/// Persist the in-progress OAuth PKCE pair. Called from `get_auth_url` at the
/// same moment the in-memory `pending_pkce` slot is set.
pub fn save_pending_pkce(pkce: &crate::auth::PkcePair) -> Result<(), String> {
    let json = serde_json::to_string(pkce).map_err(|e| format!("encode PKCE pair: {e}"))?;
    let entry = keyring_core::Entry::new(PKCE_KC_SERVICE, PENDING_PKCE_KEY)
        .map_err(|e| format!("keychain open: {e}"))?;
    entry
        .set_password(&json)
        .map_err(|e| format!("keychain write: {e}"))
}

/// Read back a persisted PKCE pair, if one exists. Does NOT clear it — the
/// caller (`complete_oauth`) is responsible for clearing on every terminal
/// outcome so a stale verifier can never be replayed. Malformed JSON (should
/// not happen — this process is the only writer) is treated the same as
/// "nothing persisted" rather than surfaced as an error.
pub fn load_pending_pkce() -> Option<crate::auth::PkcePair> {
    let entry = keyring_core::Entry::new(PKCE_KC_SERVICE, PENDING_PKCE_KEY).ok()?;
    let json = entry.get_password().ok()?;
    serde_json::from_str(&json).ok()
}

/// Remove the persisted PKCE pair. Idempotent — a missing entry is not an
/// error, matching `accounts::delete_refresh_token`'s shape. Must be called
/// on every terminal outcome of `complete_oauth` (successful consume, state
/// mismatch, exchange failure) — a leftover pair is worse than none, since it
/// would let a stray retry replay a verifier that should be single-use.
pub fn clear_pending_pkce() {
    if let Ok(entry) = keyring_core::Entry::new(PKCE_KC_SERVICE, PENDING_PKCE_KEY) {
        let _ = entry.delete_credential();
    }
}

/// The probe credential. An `.invalid` address (RFC 2606) so it can never
/// collide with a real account id, which is what the key is derived from.
const PROBE_ACCOUNT: &str = "jodd-selftest@example.invalid";
const PROBE_VALUE: &str = "jodd-selftest-value";

/// Write → read → delete a probe credential, in one process.
///
/// This proves the store is registered and round-trips. It deliberately does
/// NOT prove persistence across process death — the failure that motivated
/// this module (keyring 2's silent in-memory `mock`) passes a single-process
/// round trip perfectly and loses everything when the process dies. That half
/// was covered by a phased `write` / force-stop / `read` probe exposed as a
/// Tauri command; it has served its purpose on real hardware and was removed
/// with the command (see the note where it lived in lib.rs). Only the unit
/// tests call this now.
pub fn self_test() -> Result<String, String> {
    crate::accounts::save_refresh_token(PROBE_ACCOUNT, PROBE_VALUE)?;
    let read_back = crate::accounts::load_refresh_token(PROBE_ACCOUNT);
    crate::accounts::delete_refresh_token(PROBE_ACCOUNT);

    match read_back {
        Some(v) if v == PROBE_VALUE => Ok(v),
        Some(v) => Err(format!("probe mismatch: wrote {PROBE_VALUE:?}, read {v:?}")),
        None => Err("probe vanished: credential store is not persisting".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_succeeds_on_this_platform() {
        init().expect("credential store must initialize");
    }

    #[test]
    fn init_is_idempotent() {
        init().expect("first init");
        init().expect("second init must not fail");
    }

    #[test]
    fn self_test_round_trips_a_probe_credential() {
        init().expect("credential store must initialize");
        let v = self_test().expect("probe must survive a write/read round trip");
        assert_eq!(v, "jodd-selftest-value");
    }

    // Regression guard for the whole reason we bypass keyring's v1 shim. If
    // someone later "simplifies" init() into a lazy path, or reintroduces the
    // shim, concurrent first-callers start failing with NoDefaultStore. Jodd
    // reaches this at cold start from three directions at once:
    // migrate_legacy_keychain and the sync worker are spawned together
    // (lib.rs:4750, :4757) and the frontend fans sync_pin_state across every
    // account (App.svelte:322). Without this test nothing in the suite would
    // notice — the symptom only appears in a shipped build, on some cold
    // starts, as an account that cannot load its token until restart.
    #[test]
    fn concurrent_first_calls_all_succeed() {
        init().expect("credential store must initialize");

        let failures: Vec<String> = (0..16)
            .map(|i| {
                std::thread::spawn(move || {
                    keyring_core::Entry::new("jodd-concurrency-guard", &format!("user-{i}"))
                        .err()
                        .map(|e| e.to_string())
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|h| h.join().expect("thread must not panic"))
            .collect();

        assert!(
            failures.is_empty(),
            "{} of 16 concurrent Entry::new calls failed after init(): {:?}",
            failures.len(),
            failures
        );
    }

    // Pure — no keychain involved. Guards the wire format save/load_pending_pkce
    // agree on, independent of whatever credential store backs a given platform.
    #[test]
    fn pkce_pair_serde_roundtrips_through_json() {
        let pair = crate::auth::PkcePair {
            verifier: "verifier-value".to_string(),
            challenge: "challenge-value".to_string(),
            state: "state-value".to_string(),
        };
        let json = serde_json::to_string(&pair).expect("encode");
        let back: crate::auth::PkcePair = serde_json::from_str(&json).expect("decode");
        assert_eq!(back.verifier, pair.verifier);
        assert_eq!(back.challenge, pair.challenge);
        assert_eq!(back.state, pair.state);
    }

    // save/load/clear all target one fixed keychain key (by design — see the
    // module comment: it mirrors AppState.pending_pkce's single slot). That
    // makes the three behaviors below unsafe to split into separate #[test]
    // fns: cargo test runs them concurrently by default, and two tests each
    // clearing or overwriting the same real keychain entry would race each
    // other. One test, one sequence, matching how `self_test`'s probe key
    // stays test-exclusive by using an account name no other test touches.
    #[test]
    fn pending_pkce_save_load_overwrite_and_clear() {
        init().expect("credential store must initialize");

        // Start from a known-empty state; also proves clear is idempotent
        // when nothing is persisted (does not panic on a missing entry).
        clear_pending_pkce();
        assert!(load_pending_pkce().is_none());

        // Save, then load back the same pair.
        let first = crate::auth::PkcePair::generate();
        save_pending_pkce(&first).expect("save first must succeed");
        let loaded = load_pending_pkce().expect("must load back what was just saved");
        assert_eq!(loaded.verifier, first.verifier);
        assert_eq!(loaded.challenge, first.challenge);
        assert_eq!(loaded.state, first.state);

        // Single slot: a second save overwrites the first outright.
        let second = crate::auth::PkcePair::generate();
        save_pending_pkce(&second).expect("save second must succeed");
        let loaded = load_pending_pkce().expect("must load back the second pair");
        assert_eq!(loaded.verifier, second.verifier);
        assert_ne!(loaded.verifier, first.verifier);

        // Clear removes it; a stale verifier must never be reusable.
        clear_pending_pkce();
        assert!(load_pending_pkce().is_none());
        clear_pending_pkce(); // idempotent a second time too
    }
}
