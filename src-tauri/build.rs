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
    const KEYS: &[&str] = &["GOOGLE_CLIENT_ID", "GOOGLE_CLIENT_SECRET"];

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

    for k in KEYS {
        if let Some(v) = from_env.get(k).or_else(|| from_file.get(*k)) {
            println!("cargo:rustc-env={}={}", k, v);
        }
    }
    tauri_build::build();
}
