fn main() {
    // Embed Google OAuth credentials at compile time so the release binary
    // doesn't need a .env at the user's install location. Two input paths:
    //
    //   1. Process env vars (CI/CD path — GitHub Actions injects secrets here)
    //   2. ../.env file (dev path — convenient on the developer machine)
    //
    // Process env wins when both are present. Both client_id and client_secret
    // are required by Google's Desktop OAuth flow; PKCE (RFC 7636) protects
    // intercepted auth codes from being exchanged without the per-flow verifier.
    // Google's docs explicitly state the Desktop client_secret is not truly
    // secret — it's embeddable in distributed binaries by design.
    //
    // Android needs its own pair, and the reason is a Google-side constraint on
    // the CLIENT rather than anything about the device: Android's redirect is
    // an https App Links URL (see auth::redirect_uri), and a Desktop-type
    // client only accepts `http://localhost`. An https redirect requires a
    // **Web application** client. So GOOGLE_CLIENT_ID_ANDROID /
    // GOOGLE_CLIENT_SECRET_ANDROID hold a Web client, despite the name.
    //
    // The project has now had three Android client configurations — an
    // Android-type client (custom scheme, no secret), the Desktop client
    // (loopback), and this one. Only the last survives contact with both
    // Google and a Samsung device; see docs/android/APP-LINKS-SETUP.md.
    //
    // All four are emitted unconditionally. `auth.rs` selects between the pairs
    // with `cfg(target_os)` at compile time, so the unused pair costs a string
    // in the binary and nothing else — and emitting both keeps this file free
    // of target detection, which build scripts get wrong easily (they run for
    // the HOST, so `cfg!(target_os)` here would describe the wrong machine).
    const KEYS: &[&str] = &[
        "GOOGLE_CLIENT_ID",
        "GOOGLE_CLIENT_SECRET",
        "GOOGLE_CLIENT_ID_ANDROID",
        "GOOGLE_CLIENT_SECRET_ANDROID",
    ];


    // Path 1: process env (preferred — used by CI). `rerun-if-env-changed`
    // forces a rebuild when the env value changes, so secrets rotate cleanly.
    let mut from_env: std::collections::HashMap<&str, String> = Default::default();
    for k in KEYS {
        println!("cargo:rerun-if-env-changed={}", k);
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                from_env.insert(*k, v);
            }
        }
    }

    // Path 2: ../.env file (dev fallback). `rerun-if-changed` triggers rebuild
    // when the file is edited.
    println!("cargo:rerun-if-changed=../.env");
    let from_file: std::collections::HashMap<String, String> =
        std::fs::read_to_string("../.env")
            .map(|s| {
                s.lines()
                    .filter_map(|l| l.split_once('='))
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                    .collect()
            })
            .unwrap_or_default();

    let lookup = |k: &str| -> Option<String> {
        from_env
            .get(k)
            .or_else(|| from_file.get(k))
            .filter(|v| !v.is_empty())
            .cloned()
    };

    // A key with no value is simply not emitted, which leaves `option_env!`
    // returning None and `auth::embedded_or_runtime` falling through to the
    // runtime env var. That matters for desktop developers who have never set
    // the *_ANDROID pair: their build stays green rather than failing on a
    // credential they have no use for.
    for k in KEYS {
        if let Some(v) = lookup(k) {
            println!("cargo:rustc-env={}={}", k, v);
        }
    }
    tauri_build::build();
}
