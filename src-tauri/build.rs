fn main() {
    // Embed OAuth client credentials at compile time so the release binary
    // doesn't need a .env at the user's install location. Google's are the bulk
    // of what follows; Microsoft's single public-client id is covered at `KEYS`.
    // Two input paths:
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
    //
    // `MS_CLIENT_ID` joins them (2026-08-17) and has no secret counterpart at
    // all: the Microsoft registration is a **public client**, so there is
    // nothing to pair it with. Until this was added, a downloaded release
    // carried no Microsoft client id and could not start a Microsoft sign-in at
    // any price — `auth_ms::client_id()` read the environment only.
    //
    // Embedding it does NOT make corporate Microsoft 365 accounts work, and it
    // was never going to: consent is evaluated per tenant against the app's
    // identity, so it is indifferent to where the client id came from. Measured
    // 2026-08-17, a non-admin user in an outside tenant is refused with "Need
    // admin approval" regardless (docs/HISTORY.md). What embedding unlocks is
    // everyone who *can* consent — personal Microsoft accounts, which consent
    // outside Entra policy entirely, and admins in any tenant — and who were
    // previously locked out for an unrelated reason.
    const KEYS: &[&str] = &[
        "GOOGLE_CLIENT_ID",
        "GOOGLE_CLIENT_SECRET",
        "GOOGLE_CLIENT_ID_ANDROID",
        "GOOGLE_CLIENT_SECRET_ANDROID",
        "MS_CLIENT_ID",
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
    manifest_the_gui_examples();
    gate_the_icloud_webview();
    tauri_build::build();
}

/// Emits `icloud_webview` for every target that can host Apple's sign-in pages
/// in a webview Jodd controls — which, as of 2026-09-10, is everything but iOS.
///
/// **Why a cfg rather than `any(desktop, target_os = "android")` at fifteen
/// call sites.** The rule is one decision about one backend, and fifteen copies
/// of it drift: gotcha #27 is the record of a shared boundary that two backends
/// implemented in opposite directions because nobody diffed the pair. One name
/// also makes the exclusion greppable — `iOS is unmeasured` has exactly one
/// place to live.
///
/// **Read the target from `CARGO_CFG_TARGET_OS`, never `cfg!`** — the trap this
/// file's `KEYS` comment already names. A build script runs for the HOST, so
/// `cfg!` here would describe the wrong machine.
fn gate_the_icloud_webview() {
    // Without this, rustc 1.80+ warns `unexpected cfg condition name` on every
    // one of the attributes below — a warning that reads as a typo and is not.
    println!("cargo:rustc-check-cfg=cfg(icloud_webview)");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("ios") {
        println!("cargo:rustc-cfg=icloud_webview");
    }
}

/// Embeds a Common Controls v6 manifest into **examples**, and nothing else.
///
/// `tauri_build::build()` manifests the app binary; cargo examples get no
/// manifest at all, and a Tauri example (`examples/icloud_webview_probe*`) is a
/// full GUI process. `tao` imports `TaskDialogIndirect` / `SetWindowSubclass` /
/// `RemoveWindowSubclass` / `DefSubclassProc`, which only comctl32 **v6**
/// exports. With no manifest the loader binds System32's v5.82, finds none of
/// them, and kills the process before `main` with STATUS_ENTRYPOINT_NOT_FOUND
/// (0xc0000139) — no panic, no message, no log. Measured 2026-09-07 while
/// bringing the iCloud probe up on Windows.
///
/// `rustc-link-arg-examples` is the narrow key on purpose: it cannot reach the
/// shipped binary, so this cannot disturb what `tauri_build` embeds there. The
/// console examples pick up a manifest they have no use for, which costs a few
/// hundred bytes and nothing else.
///
/// **Read the target from `CARGO_CFG_*`, never `cfg!`** — the same trap this
/// file's `KEYS` comment names. A build script runs for the HOST, so `cfg!`
/// here would describe the wrong machine: cross-compiling to Android from
/// Windows would emit MSVC linker flags at the NDK linker.
fn manifest_the_gui_examples() {
    let is_windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    // The `/MANIFEST` family is `link.exe`'s. The gnu toolchain takes a
    // `.rc`-compiled resource instead, which is a different mechanism and not
    // one anything here builds with.
    let is_msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !(is_windows && is_msvc) {
        return;
    }

    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("windows-gui-example.manifest");
    println!("cargo:rerun-if-changed=examples/windows-gui-example.manifest");
    println!("cargo:rustc-link-arg-examples=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-examples=/MANIFESTINPUT:{}", manifest.display());
}
