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

/// Check that `ndk-context` holds the JavaVM and a Context before anything
/// asks it for them. **Never initialize it here — tao already has** (gotcha #45).
///
/// `android-native-keyring-store` reaches the Android Keystore through
/// `ndk_context::android_context()`, and that function **panics** —
/// `.expect("android context was not initialized")` — rather than returning an
/// error. The release profile is `panic = "abort"`, so either panic kills the
/// whole process at launch, before the WebView exists.
///
/// Who fills the slot changed under us. Until 2026-10-29 Jodd ran on a tao
/// fork (0.35.3) that never touched `ndk-context`, so this function called
/// `initialize_android_context` itself. tao 0.37.2 — taken from crates.io by
/// the Tauri 2.12 upgrade (#259) — calls it in `ndk_glue::onCreate`, inside
/// `APPLICATION_CONTEXT.get_or_init`, with the Application context. That
/// function asserts the slot was empty, so Jodd's call became the second one
/// and v0.41.0 aborted on every Android launch:
/// `ndk-context-0.1.1/src/lib.rs:87 assertion failed: previous.is_none()`
/// (S23 FE, 2026-10-29). `tao_initializes_ndk_context` pins the assumption.
///
/// What is left to check is order. tao populates `main_android_context()` in
/// the same `onCreate`, after `APPLICATION_CONTEXT`, so `Some` here proves the
/// `ndk-context` slot is filled. `None` would mean `secrets::init()` ran before
/// any Activity, and the keystore's `android_context()` would abort — so it
/// becomes an error that `init()`'s caller logs instead.
#[cfg(target_os = "android")]
fn init_ndk_context() -> Result<(), String> {
    // `prelude`, not `ndk_glue`: tao's platform/android.rs flattens the module
    // with `pub use crate::platform_impl::ndk_glue::*` inside `prelude`, so
    // there is no `platform::android::ndk_glue` path to import.
    use tauri::tao::platform::android::prelude as ndk_glue;

    match ndk_glue::main_android_context() {
        Some(_) => Ok(()),
        None => Err(
            "tao has no Android context yet — secrets::init() ran before the Activity".to_string(),
        ),
    }
}

/// tao must be the one filling `ndk-context` (see `init_ndk_context`). If a
/// tao upgrade, downgrade or fork stops doing it, the keystore's
/// `android_context()` aborts every Android launch — and no PR job builds
/// Android, so nothing else would notice before a release. This reads the
/// exact tao source that Cargo.lock resolves and fails if the call is gone.
#[cfg(test)]
mod ndk_context_owner {
    #[test]
    fn tao_initializes_ndk_context() {
        let lock = include_str!("../../Cargo.lock");
        let tao = lock
            .split("[[package]]")
            .find(|p| p.contains("\nname = \"tao\"\n"))
            .expect("tao in Cargo.lock");
        let field = |k: &str| {
            tao.lines()
                .find_map(|l| l.strip_prefix(&format!("{k} = \"")))
                .map(|v| v.trim_end_matches('"').to_string())
        };
        let version = field("version").expect("tao version");
        let source = field("source").unwrap_or_default();
        assert!(
            source.starts_with("registry+https://github.com/rust-lang/crates.io-index"),
            "tao now comes from {source:?}. Check whether its android ndk_glue::onCreate still \
             calls ndk_context::initialize_android_context; if it does not, secrets.rs must \
             initialize it again (gotcha #45), then update this test."
        );
        let home = std::env::var("CARGO_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let user = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .expect("HOME or USERPROFILE");
                std::path::PathBuf::from(user).join(".cargo")
            });
        let registry = home.join("registry/src");
        let glue = std::fs::read_dir(&registry)
            .expect("cargo registry src")
            .filter_map(|d| d.ok())
            .map(|d| {
                d.path()
                    .join(format!("tao-{version}/src/platform_impl/android/ndk_glue.rs"))
            })
            .find(|p| p.exists())
            .unwrap_or_else(|| panic!("tao-{version} android ndk_glue.rs under {registry:?}"));
        let src = std::fs::read_to_string(&glue).expect("read ndk_glue.rs");
        assert!(
            src.contains("ndk_context::initialize_android_context("),
            "tao {version} no longer initializes ndk-context ({glue:?}). secrets.rs must do it \
             again, exactly once, or every Android launch aborts in the keystore (gotcha #45)."
        );
    }
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
// These three functions give the PKCE pair — and the backend it belongs to,
// see `PendingSignIn` — a second, durable home through
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

/// Everything a cold-started process needs to finish a sign-in it did not
/// start: the PKCE pair AND which provider the pair was minted for.
///
/// The backend is the half that was missing until Microsoft joined the
/// Android flow (2026-09-09). `AppState.pending_backend` lives only in
/// memory, exactly like `pending_pkce`, so a process cold-launched by the
/// redirect Intent starts with its DEFAULT there — Gmail — whatever flow the
/// user actually began. Gmail never noticed, because the default was itself;
/// a Microsoft flow would have had its code exchanged at Google's token
/// endpoint and failed with nothing naming the cause.
///
/// `flatten` + `default` keep the wire shape backward-compatible: an entry
/// written before this struct existed is the bare pair, and it loads as the
/// Gmail flow that is the only kind which could have written it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct PendingSignIn {
    #[serde(flatten)]
    pub pkce: crate::auth::PkcePair,
    #[serde(default)]
    pub backend: crate::accounts::BackendKind,
}

/// Persist the in-progress sign-in. Called from `get_auth_url` at the same
/// moment the in-memory `pending_pkce` / `pending_backend` slots are set.
pub fn save_pending_signin(pending: &PendingSignIn) -> Result<(), String> {
    let json =
        serde_json::to_string(pending).map_err(|e| format!("encode pending sign-in: {e}"))?;
    let entry = keyring_core::Entry::new(PKCE_KC_SERVICE, PENDING_PKCE_KEY)
        .map_err(|e| format!("keychain open: {e}"))?;
    entry
        .set_password(&json)
        .map_err(|e| format!("keychain write: {e}"))
}

/// Read back a persisted sign-in, if one exists. Does NOT clear it — the
/// caller (`complete_oauth`) is responsible for clearing on every terminal
/// outcome so a stale verifier can never be replayed. Malformed JSON (should
/// not happen — this process is the only writer) is treated the same as
/// "nothing persisted" rather than surfaced as an error.
pub fn load_pending_signin() -> Option<PendingSignIn> {
    let entry = keyring_core::Entry::new(PKCE_KC_SERVICE, PENDING_PKCE_KEY).ok()?;
    let json = entry.get_password().ok()?;
    serde_json::from_str(&json).ok()
}

/// Remove the persisted sign-in. Idempotent — a missing entry is not an
/// error, matching `accounts::delete_refresh_token`'s shape. Must be called
/// on every terminal outcome of `complete_oauth` (successful consume, state
/// mismatch, exchange failure) — a leftover pair is worse than none, since it
/// would let a stray retry replay a verifier that should be single-use.
pub fn clear_pending_signin() {
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

    // Pure — no keychain involved. Guards the wire format save/load_pending_signin
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

    /// The keychain entry written before `backend` existed carried the bare
    /// PKCE pair. It must still load — as a Gmail flow, which is the only
    /// kind that could have written it — or an upgrade mid-sign-in ends in
    /// "PKCE verifier missing" for a verifier that is sitting right there.
    #[test]
    fn a_pre_backend_pending_entry_decodes_as_a_gmail_flow() {
        let legacy = r#"{"verifier":"v","challenge":"c","state":"s"}"#;
        let back: PendingSignIn = serde_json::from_str(legacy).expect("decode legacy shape");
        assert_eq!(back.backend, crate::accounts::BackendKind::Gmail);
        assert_eq!(back.pkce.verifier, "v");
        assert_eq!(back.pkce.state, "s");
    }

    /// The whole reason the backend rides along: a Microsoft flow whose
    /// process Android killed mid-consent must come back as a Microsoft
    /// flow, or the fresh process exchanges Microsoft's code at Google.
    #[test]
    fn a_microsoft_pending_sign_in_round_trips_as_microsoft() {
        let pending = PendingSignIn {
            pkce: crate::auth::PkcePair::generate(),
            backend: crate::accounts::BackendKind::Microsoft,
        };
        let json = serde_json::to_string(&pending).expect("encode");
        let back: PendingSignIn = serde_json::from_str(&json).expect("decode");
        assert_eq!(back.backend, crate::accounts::BackendKind::Microsoft);
        assert_eq!(back.pkce.verifier, pending.pkce.verifier);
    }

    // save/load/clear all target one fixed keychain key (by design — see the
    // module comment: it mirrors AppState.pending_pkce's single slot). That
    // makes the three behaviors below unsafe to split into separate #[test]
    // fns: cargo test runs them concurrently by default, and two tests each
    // clearing or overwriting the same real keychain entry would race each
    // other. One test, one sequence, matching how `self_test`'s probe key
    // stays test-exclusive by using an account name no other test touches.
    #[test]
    fn pending_signin_save_load_overwrite_and_clear() {
        init().expect("credential store must initialize");

        // Start from a known-empty state; also proves clear is idempotent
        // when nothing is persisted (does not panic on a missing entry).
        clear_pending_signin();
        assert!(load_pending_signin().is_none());

        // Save, then load back the same pair — and the backend it belongs to.
        let first = PendingSignIn {
            pkce: crate::auth::PkcePair::generate(),
            backend: crate::accounts::BackendKind::Microsoft,
        };
        save_pending_signin(&first).expect("save first must succeed");
        let loaded = load_pending_signin().expect("must load back what was just saved");
        assert_eq!(loaded.pkce.verifier, first.pkce.verifier);
        assert_eq!(loaded.pkce.challenge, first.pkce.challenge);
        assert_eq!(loaded.pkce.state, first.pkce.state);
        assert_eq!(loaded.backend, crate::accounts::BackendKind::Microsoft);

        // Single slot: a second save overwrites the first outright.
        let second = PendingSignIn {
            pkce: crate::auth::PkcePair::generate(),
            backend: crate::accounts::BackendKind::Gmail,
        };
        save_pending_signin(&second).expect("save second must succeed");
        let loaded = load_pending_signin().expect("must load back the second pair");
        assert_eq!(loaded.pkce.verifier, second.pkce.verifier);
        assert_ne!(loaded.pkce.verifier, first.pkce.verifier);
        assert_eq!(loaded.backend, crate::accounts::BackendKind::Gmail);

        // Clear removes it; a stale verifier must never be reusable.
        clear_pending_signin();
        assert!(load_pending_signin().is_none());
        clear_pending_signin(); // idempotent a second time too
    }
}
