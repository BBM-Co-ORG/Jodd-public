//! Answers verify-first item **#2** of the iCloud M1 design, and smoke-tests
//! **#1** and **#3** — the three questions about Jodd's own webview that decide
//! whether Component B (session harvesting) is buildable as designed.
//!
//! **Run this on macOS.** It opens a real Tauri webview, which is the whole
//! point: the questions are about `WKWebView` behaviour, and answering them
//! anywhere else answers a different question.
//!
//! ```text
//! npm run build                                   # generate_context! needs dist/
//! cargo run --example icloud_webview_probe
//! ```
//!
//! Sign in to iCloud in the window that opens. The probe prints a status line
//! every few seconds and a full report the moment a session appears. Close the
//! window when you are done.
//!
//! # What each question is, and why it is load-bearing
//!
//! **#2 — does an `initialization_script` run before Apple's own JS?**
//! The design reads `clientBuildNumber` / `clientMasteringNumber` / `clientId`
//! out of the live session rather than hardcoding them (the captured session
//! carried `2628Build44`; icloud-md hardcodes `2624Build27`, and the drift its
//! comment warns about has already happened). The mechanism is a script that
//! wraps `fetch`/`XHR` and reads the parameters off Apple's own first request
//! to `setup.icloud.com`. If Apple's code grabs a reference to `fetch` before
//! our script runs — or fires that request first — the wrapper sees nothing and
//! Component B5 needs a different mechanism. **Capturing the parameters IS the
//! proof**; there is no other way to ask.
//!
//! **#1 — does `cookies()` return HttpOnly cookies?** Apple's session cookies
//! all are. Tauri's own doc comment says yes ("including HTTP-only and secure
//! cookies") and wry's `cookie_from_wkwebview` reads `isHTTPOnly()` explicitly,
//! so this is a smoke test of a documented contract rather than an open
//! question — but the whole session model rests on it.
//!
//! **#3 — is a per-webview persistent data store available?** If it is, the
//! "one Apple ID per install" limit (design decision 8) is not a real
//! constraint. `WebviewWindowBuilder::data_store_identifier` exists in Tauri
//! 2.11 and wry maps it to `WKWebsiteDataStore(forIdentifier:)` on **macOS 14+**
//! — below that it falls back to the shared default store **silently, with no
//! error**, which is exactly the failure a feature-detect has to catch.
//!
//! # The bug this probe exists to demonstrate
//!
//! The design spec said to harvest with `cookies_for_url("https://www.icloud.com")`.
//! That filters with `cookie.domain() == url.domain()` — exact string equality —
//! while `cookie::Cookie::domain()` strips a leading dot. Apple's session
//! cookies are scoped to `.icloud.com` (they have to be: they are used against
//! `setup.icloud.com` and `p<N>-ckdatabasews.icloud.com` too), so the
//! comparison is `"icloud.com" == "www.icloud.com"` and **every one of them is
//! dropped**. The report below calls both APIs side by side so the difference
//! is measured on the real jar rather than argued from source.
//!
//! # Privacy
//!
//! **No cookie value is ever printed**, logged, or written anywhere — only
//! names, domains, flags and value *lengths*. A session harvested here is a
//! live credential for a real Apple ID.
//!
//! Lives in `examples/`, never `src/bin/` — gotcha #3.

use std::time::Duration;

use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Where the sign-in happens. Apple's own pages, start to finish: Jodd owns
/// none of `idmsa.apple.com` and depends only on the result.
const ICLOUD_URL: &str = "https://www.icloud.com/";

/// Fixed, so a second run reuses the store the first one created — which is
/// what makes "were cookies already there at startup?" mean persistence rather
/// than luck. A random id per run would prove nothing.
const DATA_STORE_ID: [u8; 16] = *b"jodd-icloud-prb1";

/// Wraps `fetch` and `XMLHttpRequest.open` and reports back through the ONE
/// channel Rust can already read: a cookie on the page's own origin.
///
/// **Why a cookie and not a fetch to a custom scheme.** icloud.com ships a
/// strict CSP and `connect-src` will block a request to an unfamiliar scheme.
/// `document.cookie` is not subject to CSP, and a WKWebView user script is not
/// subject to the page's CSP either. This is load-bearing, not a shortcut.
///
/// **Why not Tauri IPC.** That would hand Apple's page Jodd's IPC surface. The
/// cookie channel is one-directional and carries nothing but our own markers.
const INIT_SCRIPT: &str = r#"
(function () {
  var mark = function (name, value) {
    try {
      document.cookie = name + "=" + encodeURIComponent(value) + ";path=/;SameSite=Lax";
    } catch (e) {}
  };
  // Set immediately AND on DOMContentLoaded: at document-start the document
  // exists but is empty, and if that first write throws we would lose the only
  // evidence that this script ran at all.
  mark("jodd_probe_ran", "1");
  try {
    document.addEventListener("DOMContentLoaded", function () { mark("jodd_probe_ran", "1"); });
  } catch (e) {}

  var seen = 0;
  var captured = false;
  var note = function (raw) {
    seen++;
    mark("jodd_probe_seen", String(seen));
    if (captured) return;
    try {
      var u = new URL(raw, location.href);
      if (u.hostname.indexOf("setup.icloud.com") === -1) return;
      var out = [];
      ["clientBuildNumber", "clientMasteringNumber", "clientId"].forEach(function (k) {
        var v = u.searchParams.get(k);
        if (v) out.push(k + "=" + v);
      });
      if (!out.length) return;
      captured = true;
      mark("jodd_probe_cfg", out.join("&"));
      mark("jodd_probe_path", u.pathname);
    } catch (e) {}
  };

  try {
    var origFetch = window.fetch;
    window.fetch = function (input) {
      try {
        note(typeof input === "string" ? input : (input && input.url) || "");
      } catch (e) {}
      return origFetch.apply(this, arguments);
    };
  } catch (e) {}

  try {
    var origOpen = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (method, url) {
      try { note(url); } catch (e) {}
      return origOpen.apply(this, arguments);
    };
  } catch (e) {}
})();
"#;

fn main() {
    if !cfg!(target_os = "macos") {
        eprintln!(
            "⚠ This probe answers questions about macOS WKWebView. It will open a window \n\
             on this platform too, but the answers will be about a different engine."
        );
    }

    tauri::Builder::default()
        .setup(|app| {
            println!("{}", banner());

            let url = tauri::Url::parse(ICLOUD_URL)?;
            let win = WebviewWindowBuilder::new(app, "icloud-probe", WebviewUrl::External(url))
                .title("Jodd — iCloud webview probe (sign in here)")
                .inner_size(1100.0, 820.0)
                .initialization_script(INIT_SCRIPT)
                .data_store_identifier(DATA_STORE_ID)
                .build()?;

            // A background thread on purpose. `cookies()` bottoms out in
            // `WKHTTPCookieStore.getAllCookies`, which wry dispatches to the
            // main thread and then waits on a channel — calling it FROM the
            // main thread would deadlock waiting for itself. (Tauri's own doc
            // comment records that shape for Windows; the same reasoning
            // applies here.)
            std::thread::spawn(move || watch(win));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start the probe — did `npm run build` run? generate_context! needs dist/");
}

fn banner() -> String {
    // Mutated only under `cfg(target_os = "macos")` below, which is the one
    // platform whose answer this probe is about.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut s = String::from(
        "\n╭─ iCloud webview probe ──────────────────────────────────────────────╮\n\
         │ Sign in to iCloud in the window that just opened.                   │\n\
         │ A full report prints as soon as a session appears.                  │\n\
         │ No cookie VALUE is ever printed — names, flags and lengths only.    │\n\
         ╰─────────────────────────────────────────────────────────────────────╯\n",
    );
    #[cfg(target_os = "macos")]
    {
        // Version matters: per-webview data stores need macOS 14+, and below
        // that wry falls back to the shared store with no error at all.
        let v = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "<unknown>".into());
        let major: u32 = v.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
        s.push_str(&format!(
            "  macOS {v} — per-webview data store {}\n",
            if major >= 14 {
                "SUPPORTED (>= 14)"
            } else {
                "NOT supported (< 14): wry falls back to the shared store SILENTLY"
            }
        ));
    }
    s
}

/// One decoded cookie, reduced to what is safe to print.
struct Seen {
    name: String,
    domain: String,
    http_only: bool,
    secure: bool,
    len: usize,
}

/// Apple's session cookies do **not** share one prefix.
///
/// Measured on a real jar (2026-08-22, macOS 26.6.2): fifteen names begin
/// `X-APPLE-` and two begin `X_APPLE_WEB_KB-` — **underscores**, and both are
/// HttpOnly, Secure and 220 bytes, so they are not decoration. A filter written
/// as `starts_with("X-APPLE")` — which this probe itself had — silently drops
/// them, and would have under-reported the session by two cookies.
///
/// Component B should not filter by name at all: it sends whatever
/// domain-matches, the way a browser does. This exists only so the probe's
/// COUNT is honest about what a name-shaped rule would have missed.
fn is_apple_cookie(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("X-APPLE") || n.starts_with("X_APPLE")
}

fn watch(win: tauri::WebviewWindow) {
    let mut had_session_at_startup: Option<bool> = None;
    // `cookies()` goes through the webview's own IPC, so it fails once the
    // window starts tearing down. Retrying that forever prints one error every
    // three seconds under the finished report — which is what the first real
    // run did, and it reads like the probe failed after answering everything.
    let mut consecutive_failures = 0u8;

    for tick in 0..200 {
        std::thread::sleep(Duration::from_secs(3));

        let all: Vec<Seen> = match win.cookies() {
            Ok(cs) => cs
                .into_iter()
                .map(|c| Seen {
                    name: c.name().to_string(),
                    domain: c.domain().unwrap_or("<host-only>").to_string(),
                    http_only: c.http_only().unwrap_or(false),
                    secure: c.secure().unwrap_or(false),
                    len: c.value().len(),
                })
                .collect(),
            Err(e) => {
                consecutive_failures += 1;
                if consecutive_failures >= 3 {
                    println!("  cookies() unreachable — the webview is gone. Stopping.");
                    return;
                }
                println!("  cookies() failed: {e}");
                continue;
            }
        };
        consecutive_failures = 0;

        let apple: Vec<&Seen> = all.iter().filter(|c| is_apple_cookie(&c.name)).collect();

        // Recorded on the FIRST tick only: cookies present before the user has
        // done anything mean the data store persisted from a previous run.
        if had_session_at_startup.is_none() {
            had_session_at_startup = Some(!apple.is_empty());
        }

        if apple.is_empty() {
            println!(
                "  [{}s] waiting for sign-in — {} cookie(s), no X-APPLE-* yet",
                (tick + 1) * 3,
                all.len()
            );
            continue;
        }
        report(&win, &all, &apple, had_session_at_startup.unwrap_or(false));
        // The report is a one-shot: everything it answers is answered. Staying
        // in the loop only produces noise once the user closes the window.
        return;
    }
    println!("\n  (probe stopped after 10 minutes — close the window to exit)");
}

fn report(win: &tauri::WebviewWindow, all: &[Seen], apple: &[&Seen], persisted: bool) {
    println!("\n════════ REPORT ════════\n");

    println!("── every cookie in the jar (names and flags only) ──");
    println!("  {:<34} {:<18} {:>5} {:>7} {:>6}", "NAME", "DOMAIN", "HTTP", "SECURE", "LEN");
    for c in all {
        println!(
            "  {:<34} {:<18} {:>5} {:>7} {:>6}",
            truncate(&c.name, 34),
            truncate(&c.domain, 18),
            if c.http_only { "yes" } else { "no" },
            if c.secure { "yes" } else { "no" },
            c.len
        );
    }

    // ── #1 ──────────────────────────────────────────────────────────────
    let http_only = apple.iter().filter(|c| c.http_only).count();
    println!("\n── #1: does cookies() return HttpOnly cookies? ──");
    let underscored = apple.iter().filter(|c| c.name.to_ascii_uppercase().starts_with("X_APPLE")).count();
    println!("  Apple session cookies: {} total, {http_only} of them HttpOnly", apple.len());
    if underscored > 0 {
        println!(
            "  ⚠ {underscored} of them are named X_APPLE_… with UNDERSCORES, not X-APPLE-.\n               A name filter written as `starts_with(\"X-APPLE\")` drops those silently.\n               Component B must domain-match, not name-match."
        );
    }
    if http_only > 0 {
        println!("  ANSWER: YES. Harvesting through cookies() reaches the session.");
    } else {
        println!(
            "  ANSWER: NO — and that reshapes Component B entirely. Apple's session\n  \
             cookies are HttpOnly; if none came back, the webview cannot be the\n  \
             session store and the design needs another mechanism."
        );
    }

    // ── the cookies_for_url finding ─────────────────────────────────────
    println!("\n── cookies_for_url() vs cookies(): the domain-filter trap ──");
    for probe_url in ["https://www.icloud.com", "https://icloud.com"] {
        match tauri::Url::parse(probe_url).map_err(|e| e.to_string()).and_then(|u| {
            win.cookies_for_url(u).map_err(|e| e.to_string())
        }) {
            Ok(cs) => {
                let n_apple = cs
                    .iter()
                    .filter(|c| is_apple_cookie(c.name()))
                    .count();
                println!("  cookies_for_url({probe_url:<26}) → {:>3} cookie(s), {n_apple} X-APPLE-*", cs.len());
            }
            Err(e) => println!("  cookies_for_url({probe_url}) failed: {e}"),
        }
    }
    println!("  cookies()                                → {:>3} cookie(s), {} X-APPLE-*", all.len(), apple.len());
    println!(
        "\n  READ THIS AS: if the www row shows 0 X-APPLE-*, the exact-match domain\n  \
         filter confirmed — Component B must harvest with cookies() and do its own\n  \
         RFC 6265 domain matching. Do NOT 'fix' it by asking for the bare domain:\n  \
         that works by accident and drops host-only cookies on www."
    );

    // ── #2 ──────────────────────────────────────────────────────────────
    println!("\n── #2: did the initialization_script beat Apple's JS? ──");
    let marker = |n: &str| all.iter().find(|c| c.name == n);
    match (marker("jodd_probe_ran"), marker("jodd_probe_cfg")) {
        (None, _) => println!(
            "  ANSWER: the script did not run at all (no jodd_probe_ran cookie).\n  \
             Check that initialization_script is reaching a remote origin before\n  \
             concluding anything about timing."
        ),
        (Some(_), None) => println!(
            "  ANSWER: NO. The script ran, but never saw a setup.icloud.com request\n  \
             carrying the client parameters — Apple either captured `fetch` before\n  \
             us or issued that request first.\n  \
             CONSEQUENCE: Component B5's mechanism does not work. The version\n  \
             strings need another source, and hardcoding them is what the handoff\n  \
             forbids (icloud-md's hardcoded 2624Build27 is already stale)."
        ),
        (Some(_), Some(cfg)) => {
            println!(
                "  ANSWER: YES. Captured the client parameters off Apple's own first\n  \
                 request ({} bytes in jodd_probe_cfg).",
                cfg.len
            );
            println!(
                "  Read the values out of the webview's own cookie jar rather than from\n  \
                 here — they are not secret, but this probe prints no cookie values."
            );
            println!("  CONSEQUENCE: Component B5 works as designed.");
        }
    }
    if let Some(seen) = marker("jodd_probe_seen") {
        println!("  (the wrapper observed requests — jodd_probe_seen is {} bytes)", seen.len);
    }

    // ── #3 ──────────────────────────────────────────────────────────────
    println!("\n── #3: is the per-webview data store persistent? ──");
    if persisted {
        println!(
            "  A session was already present on the FIRST tick, before you signed in.\n  \
             ANSWER: the store persisted from a previous run under the same identifier."
        );
    } else {
        println!(
            "  No session at startup — expected on a first run. Sign in, quit, and run\n  \
             this probe AGAIN: if it reports a session on the first tick next time,\n  \
             the custom store persists and design decision 8 (one Apple ID per\n  \
             install) is not a real constraint."
        );
    }
    println!(
        "  ⚠ On macOS < 14 wry falls back to the SHARED default store with no error,\n  \
         so a pass here on an old macOS proves nothing about isolation. See the\n  \
         version line at the top of this run."
    );

    println!("\n════════ END ════════");
    println!("Close the window to exit. Nothing was written to disk.");
}

/// Character-aware so a non-ASCII cookie name is not cut mid-codepoint.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
    t.push('…');
    t
}
