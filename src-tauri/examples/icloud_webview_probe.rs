//! The iCloud webview probe — **one instrument, every desktop platform**.
//!
//! It opens a real Tauri webview against Apple's own pages and answers the
//! questions that decide whether Component B (session harvesting) is buildable
//! on the engine you run it on. Run it on macOS to learn about `WKWebView`; run
//! it on Windows to learn about `ICoreWebView2`. **The answers do not
//! transfer** — gotcha #7's "a product's CLI and its HTTP server are two
//! different measurements" applies just as hard to two browser engines, which
//! is why the report always names the engine it measured.
//!
//! ```text
//! npm run build                                  # generate_context! needs dist/
//! cargo run --example icloud_webview_probe
//! cargo run --example icloud_webview_probe -- --forget   # Q8, destructively
//! ```
//!
//! Sign in to iCloud in the window that opens. A status line prints every half
//! minute and a full report once Apple confirms the session, after which the
//! probe quits itself. **No cookie value is printed** except Jodd's own
//! `jodd_icloud_*` markers — a live Apple jar is a real credential.
//!
//! # Why this is one file and not two
//!
//! It was two for a day, and the split caused the exact defect it should have
//! prevented. The Windows probe began as a copy of the macOS one; the copy
//! carried its own duplicate of the injected script with the marker names
//! changed, and [`ClientConfig::from_jar`] looks for the cookie named exactly
//! [`CLIENT_CFG_COOKIE`] — so the config was never found and a run burned its
//! whole timeout waiting for a cookie it had made impossible. Meanwhile
//! [`cookie_header_for`] strips markers by [`MARKER_PREFIX`], so the renamed
//! ones would have been **sent to Apple**, which is precisely what that filter
//! exists to prevent.
//!
//! So the probe now calls the shipped functions rather than restating them, and
//! the only thing that varies by platform is the isolation lever — see
//! [`isolated`], which is also the seam the shipped code needs.
//!
//! # Readiness is `/validate`, not a cookie name
//!
//! **This is the correction that matters, and it invalidates an earlier
//! answer.** The original probe reported as soon as any `X-APPLE-*` cookie
//! appeared. An anonymous visit to icloud.com sets one — `x-apple-group`, five
//! bytes, HttpOnly, an A/B bucket — so the probe would print a full report,
//! with confident YES answers, against a jar nobody had signed into. On Windows
//! it did exactly that, and contradicted itself in print (the banner said the
//! profile was new; persistence said a session was already there) which was the
//! only clue.
//!
//! **Q7 below is the answer that instrument got wrong**, and CLAUDE.md's
//! "the per-webview data store persists" rests on it. Re-run this probe on a
//! Mac before trusting that line.
//!
//! Readiness is now what the shipped code uses: harvest with `cookies()`, build
//! the header with [`cookie_header_for`], call `/validate`, believe
//! [`classify_validate`]. That is ground truth instead of a name-shaped guess,
//! and it buys a measurement the cookie test could never make — **whether the
//! shipped pure-function layer produces a header Apple accepts on this engine
//! at all.** Those functions were written against WebKit's cookie shapes; a
//! `Complete` on Windows means they survive the other engine unchanged.
//!
//! The lesson is CLAUDE.md's own, from the hashtag census: an instrument that
//! cannot tell "absent" from "present and wrong" reports the wrong thing
//! confidently. Ask for the relation you care about, and where a shipped
//! function already knows the answer, call it.
//!
//! # What is settled by reading the sources, and is NOT asked here
//!
//! Checked against wry 0.55.1 / tauri 2.11, so a live sign-in is not spent
//! re-confirming them:
//!
//! - `WebView::cookies()` is implemented on WebKit **and** WebView2 (the latter
//!   via `CookieManager::GetCookies` with a null URI); wry documents **Android**
//!   as the only unsupported target.
//! - `data_store_identifier` is **not** cfg-gated in Tauri, so it compiles
//!   everywhere and silently does nothing off Apple. That is why [`isolated`]
//!   exists and why Q5 is a real question rather than a formality.
//! - Tauri keys its `WebContextStore` on `Option<PathBuf>` (`web_context_key =
//!   webview_attributes.data_directory`), and wry hands that value to
//!   `CreateCoreWebView2EnvironmentWithOptions` as the user data folder — so on
//!   Windows a distinct directory is a distinct profile, hence a distinct jar.
//! - **The thread rule does not flip between engines.** On macOS `cookies()`
//!   must not run on the main thread (wry dispatches there and blocks); on
//!   Windows it bottoms out in `webview2_com::wait_with_pump`, and
//!   tauri-runtime-wry's `send_user_message` runs it on the event-loop thread
//!   either way. The shipped off-the-main-thread call shape is correct on both,
//!   and this probe uses it.
//!
//! # Privacy
//!
//! No Apple cookie value is ever printed, logged or written. Names, domains,
//! flags, expiries and value *lengths* only. The Apple ID and CloudKit
//! partition from `/validate` are printed because they are the proof Q1 asks
//! for, and they are the running user's own.
//!
//! Lives in `examples/`, never `src/bin/` — gotcha #3.

use std::time::Duration;

use jodd_lib::icloud_auth::{
    cookie_header_for, validate_at, ClientConfig, HarvestedCookie, ValidateOutcome,
    CLIENT_CFG_COOKIE, INIT_SCRIPT, MARKER_PREFIX, SETUP_HOST, SETUP_HOSTNAME, VALIDATE_PATH,
};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// Where sign-in happens. Apple's own pages, start to finish: Jodd owns none of
/// `idmsa.apple.com` and depends only on the result.
const ICLOUD_URL: &str = "https://www.icloud.com/";

/// The visible window the user signs in to.
const SIGNIN: &str = "icloud-probe-signin";

/// The Component B4 rehearsal: a second window on the same profile, opened only
/// after a session exists, to see whether it inherits one silently.
const SESSION: &str = "icloud-probe-session";

/// The app's own window, from `tauri.conf.json`. It runs on the DEFAULT
/// web context because nothing gave it an isolation lever, which makes it a
/// free control for Q5.
const CONTROL: &str = "main";

/// Fixed, so a second run reuses what the first created — which is what makes
/// Q7 mean persistence rather than luck. A random id per run would prove
/// nothing.
#[cfg(target_vendor = "apple")]
const DATA_STORE_ID: [u8; 16] = *b"jodd-icloud-prb1";

/// The non-Apple counterpart of [`DATA_STORE_ID`]: an identifier there, a path
/// here, and Tauri keys the web context on the path.
///
/// Deliberately outside Jodd's own storage. Reusing Jodd's would make Q5
/// meaningless, and a temp directory that a cleaner might remove would make Q7
/// unanswerable.
#[cfg(not(target_vendor = "apple"))]
fn probe_data_dir() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("jodd-icloud-probe")
}

/// Gives a window its own persistent cookie jar, by whatever mechanism this
/// platform has.
///
/// **This is the whole platform seam, and it is the same one the shipped code
/// needs.** WebKit has no per-webview data *directory* — `data_store_identifier`
/// exists precisely as its replacement, and wry maps it to
/// `WKWebsiteDataStore(forIdentifier:)` on macOS 14+. WebView2 has no data
/// *store identifier* — Tauri's method compiles there and does nothing at all —
/// but it does take a user data folder. Each platform ignores the other's lever
/// **silently**, which is why calling the wrong one looks like it worked.
///
/// **Two definitions rather than one with `#[cfg]` blocks inside**: a cfg'd
/// block in tail position is exactly the construct whose validity cannot be
/// checked from the other platform, and this file is edited far more often on
/// the machine that cannot compile the Apple arm.
#[cfg(target_vendor = "apple")]
fn isolated<'a, R: tauri::Runtime, M: Manager<R>>(
    b: WebviewWindowBuilder<'a, R, M>,
) -> WebviewWindowBuilder<'a, R, M> {
    b.data_store_identifier(DATA_STORE_ID)
}

/// See the Apple definition above for what this is and why there are two.
#[cfg(not(target_vendor = "apple"))]
fn isolated<'a, R: tauri::Runtime, M: Manager<R>>(
    b: WebviewWindowBuilder<'a, R, M>,
) -> WebviewWindowBuilder<'a, R, M> {
    b.data_directory(probe_data_dir())
}

/// Runs **after** the shipped [`INIT_SCRIPT`], adding only what the shipped one
/// has no reason to carry: whether a script ran at all, the user agent, the
/// page title, and a count of requests the wrapper saw.
///
/// **The client-config capture is deliberately NOT duplicated here** — see this
/// module's "Why this is one file" note for what a duplicate cost. Every marker
/// starts with [`MARKER_PREFIX`], so [`cookie_header_for`] strips these too and
/// none of the probe's bookkeeping reaches Apple.
///
/// The user agent and title exist because Q1's failure mode has no other
/// instrument: if Apple serves an unsupported-browser wall, `/validate` simply
/// keeps answering `NotSignedIn` for a reason unrelated to the mechanism under
/// test. Printing what Apple was told, and what it served, is the difference
/// between a measurement and a mystery.
///
/// Every statement is inside a `try`. A throw here runs inside Apple's page
/// before Apple's own code, and breaking the sign-in form is a far worse
/// failure than losing a marker.
const PROBE_EXTRA: &str = r#"
(function () {
  var mark = function (name, value) {
    try {
      document.cookie = name + "=" + encodeURIComponent(value) + ";path=/;SameSite=Lax";
    } catch (e) {}
  };
  // At document-start the document exists but is empty. Write immediately AND
  // on DOMContentLoaded: if the first write throws we lose the only evidence
  // that any script ran at all.
  mark("jodd_icloud_probe_ran", "1");
  try { mark("jodd_icloud_probe_ua", navigator.userAgent); } catch (e) {}
  try {
    document.addEventListener("DOMContentLoaded", function () {
      mark("jodd_icloud_probe_ran", "1");
      try { mark("jodd_icloud_probe_title", document.title || "<empty>"); } catch (e) {}
    });
  } catch (e) {}

  // Wraps the SHIPPED wrapper, which is already in place. Calls flow
  // probe -> shipped -> real, so both see every URL; this one only counts.
  var seen = 0;
  try {
    var origFetch = window.fetch;
    window.fetch = function () {
      try { mark("jodd_icloud_probe_seen", String(++seen)); } catch (e) {}
      return origFetch.apply(this, arguments);
    };
  } catch (e) {}
  try {
    var origOpen = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function () {
      try { mark("jodd_icloud_probe_seen", String(++seen)); } catch (e) {}
      return origOpen.apply(this, arguments);
    };
  } catch (e) {}
})();
"#;

/// How often the session is re-checked. A session that goes valid should be
/// noticed within a few seconds.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// How long to wait for a human — an hour.
///
/// Ten minutes was the first value, and it was chosen for the machine rather
/// than the person: signing in to iCloud means a password, 2FA on another
/// device, and often a browser prompt. Two runs expired mid-flow because the
/// probe had put its user on a stopwatch they could not see.
const TIMEOUT_TICKS: u32 = 1200;

/// One status line per this many polls, so an hour of patience is 120 lines of
/// scrollback rather than 1200.
const ANNOUNCE_EVERY: u32 = 10;

fn main() {
    let forget = std::env::args().any(|a| a == "--forget");
    // Recorded BEFORE any webview is built, because building one creates the
    // store — asking afterwards would answer Q7 "yes" on every run.
    let store_existed = store_exists();

    tauri::Builder::default()
        .setup(move |app| {
            println!("{}", banner(forget, store_existed));

            let url = tauri::Url::parse(ICLOUD_URL)?;
            let win = isolated(WebviewWindowBuilder::new(
                app,
                SIGNIN,
                WebviewUrl::External(url),
            ))
            .title("Jodd — iCloud webview probe (sign in here)")
            .inner_size(1100.0, 820.0)
            .initialization_script(INIT_SCRIPT)
            .initialization_script(PROBE_EXTRA)
            .build()?;

            let handle = app.handle().clone();
            // A background thread on purpose — see the module header's note on
            // why the thread rule does not flip between engines.
            std::thread::spawn(move || watch(handle, win, forget, store_existed));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start the probe — did `npm run build` run? generate_context! needs dist/");
}

/// Whether a previous run left a store behind.
///
/// On Apple this cannot be asked cheaply — the store is WebKit's, keyed by
/// identifier, with no path to stat — so Q7 there leans entirely on the first
/// `/validate`, and the banner says so rather than implying a check happened.
#[cfg(target_vendor = "apple")]
fn store_exists() -> bool {
    false
}

/// See the Apple definition above.
#[cfg(not(target_vendor = "apple"))]
fn store_exists() -> bool {
    probe_data_dir().exists()
}

fn banner(forget: bool, store_existed: bool) -> String {
    let mut s = String::from(
        "\n╭─ iCloud webview probe ──────────────────────────────────────────────╮\n\
         │ Sign in to iCloud in the window that just opened.                   │\n\
         │ Readiness is Apple's own /validate, not a cookie name.              │\n\
         │ No cookie VALUE is printed except Jodd's own jodd_icloud_* ones.    │\n\
         ╰─────────────────────────────────────────────────────────────────────╯\n",
    );
    s.push_str(&format!("  engine:  {}\n", engine()));

    #[cfg(target_vendor = "apple")]
    {
        // Version matters: per-webview data stores need macOS 14+, and below
        // that wry falls back to the shared store with no error at all — so a
        // pass on an older macOS proves nothing about isolation.
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
        s.push_str(
            "  Q7 (persistence): WebKit's store cannot be stat'ed, so the answer is\n  \
             whether the FIRST /validate below already says Complete.\n",
        );
        let _ = store_existed;
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        s.push_str(&format!("  profile: {}\n", probe_data_dir().display()));
        s.push_str(&format!(
            "  Q7 (persistence): {}\n",
            if store_existed {
                "the profile directory already existed. That is half the answer; the \
                 other half is whether the FIRST /validate below says Complete."
            } else {
                "the profile directory did not exist, so this is a first run and Q7 \
                 cannot be answered by it. Sign in, let the probe exit, then run again."
            }
        ));
    }

    if forget {
        s.push_str("  --forget: Q8 will run instead of the report and DESTROY this session.\n");
    }
    s
}

/// Named in every verdict, because an answer here is about an engine rather
/// than about "desktop".
fn engine() -> &'static str {
    if cfg!(target_vendor = "apple") {
        "WKWebView (Apple)"
    } else if cfg!(target_os = "windows") {
        "WebView2 (Windows)"
    } else {
        "WebKitGTK (Linux)"
    }
}

/// One decoded cookie, reduced to what is safe to print.
///
/// `domain` is kept **raw** — not dot-stripped — because Q3 is precisely a
/// question about the shape wry hands over. [`harvested`] does the stripping
/// separately, for the copy the shipped functions consume.
struct Seen {
    name: String,
    domain: String,
    path: String,
    http_only: bool,
    secure: bool,
    /// `"session"` for a cookie with no expiry — one the engine is entitled to
    /// drop when the profile shuts down. **This is the field Q7a turns on**,
    /// and its absence is why the first persistence attempt could not tell
    /// Apple's cookie policy from the way the probe had been killed.
    expires: String,
    len: usize,
    /// Values are captured only for Jodd's own markers. See [`is_marker`].
    value: Option<String>,
}

/// Apple's session cookies do **not** share one prefix.
///
/// Measured on a real WebKit jar (2026-08-22): fifteen names begin `X-APPLE-`
/// and two begin `X_APPLE_WEB_KB-` — **underscores** — and those are HttpOnly,
/// Secure and 220 bytes, so they are not decoration. A filter written
/// `starts_with("X-APPLE")` drops them silently. On WebView2 (2026-09-09) the
/// same jar shape appears with **one** underscored cookie rather than two,
/// which is a real difference between the engines and costs nothing here
/// because nothing filters by name.
///
/// Component B does not filter by name at all; it sends whatever
/// domain-matches, the way a browser does. This exists only so the probe's
/// COUNT is honest about what a name-shaped rule would have missed — it is
/// **not** the readiness test, which is the mistake this probe used to make.
fn is_apple_cookie(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("X-APPLE") || n.starts_with("X_APPLE")
}

/// Jodd's own markers, the only cookies whose VALUE is printed.
///
/// Keyed on the shipped [`MARKER_PREFIX`] rather than a literal, so it covers
/// the client-config cookie and this probe's extras with one rule — and cannot
/// drift from the prefix [`cookie_header_for`] strips.
fn is_marker(name: &str) -> bool {
    name.starts_with(MARKER_PREFIX)
}

/// The report's copy: raw, printable, never fed to the shipped functions.
fn read_jar(win: &tauri::WebviewWindow) -> Result<Vec<Seen>, String> {
    win.cookies().map_err(|e| e.to_string()).map(|cs| {
        cs.into_iter()
            .map(|c| Seen {
                name: c.name().to_string(),
                domain: c.domain().unwrap_or("<none>").to_string(),
                path: c.path().unwrap_or("<none>").to_string(),
                http_only: c.http_only().unwrap_or(false),
                secure: c.secure().unwrap_or(false),
                expires: match c.expires() {
                    None => "<unset>".to_string(),
                    Some(tauri::webview::cookie::Expiration::Session) => "session".to_string(),
                    Some(tauri::webview::cookie::Expiration::DateTime(d)) => {
                        format!("{}", d.date())
                    }
                },
                len: c.value().len(),
                value: is_marker(c.name()).then(|| c.value().to_string()),
            })
            .collect()
    })
}

/// The shipped copy, built exactly the way `icloud_auth::harvest_one` builds it.
///
/// Deliberately a duplicate of six lines rather than a call: `harvest_one` is
/// private, and reaching for it would mean widening the shipped module's
/// surface for a probe. What must not drift is the SHAPE — dot stripped,
/// `host_only` false — so any change there belongs here too.
fn harvested(win: &tauri::WebviewWindow) -> Result<Vec<HarvestedCookie>, String> {
    win.cookies().map_err(|e| e.to_string()).map(|cs| {
        cs.iter()
            .map(|c| HarvestedCookie {
                name: c.name().to_string(),
                value: c.value().to_string(),
                domain: c.domain().unwrap_or_default().trim_start_matches('.').to_string(),
                path: c.path().unwrap_or("/").to_string(),
                host_only: false,
                secure: c.secure().unwrap_or(false),
            })
            .collect()
    })
}

/// Asks Apple what this window's jar is worth, through the shipped layer.
///
/// `None` means the client config has not been captured yet — Apple's own first
/// request has not gone out, so there is nothing to validate *with*. That is
/// the normal state for the first tick or two and is not a verdict.
async fn validate_window(
    http: &reqwest::Client,
    win: &tauri::WebviewWindow,
) -> Option<ValidateOutcome> {
    let jar = harvested(win).ok()?;
    let client = ClientConfig::from_jar(&jar)?;
    let header = cookie_header_for(SETUP_HOSTNAME, VALIDATE_PATH, &jar);
    if header.is_empty() {
        return None;
    }
    Some(validate_at(http, SETUP_HOST, &header, &client).await)
}

fn watch(app: tauri::AppHandle, win: tauri::WebviewWindow, forget: bool, store_existed: bool) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            println!("  could not start a tokio runtime: {e}");
            return;
        }
    };
    let http = reqwest::Client::new();

    // Q7's other half: whether the session was ALREADY valid before the user
    // touched anything. Recorded on the first tick that gets an answer at all,
    // so a slow first page load does not count as "no".
    let mut first_answer: Option<ValidateOutcome> = None;
    // `cookies()` rides the webview's own message channel, so it fails once the
    // window starts tearing down. Retrying forever prints one error every few
    // seconds underneath a finished report, which reads like a failure.
    let mut consecutive_failures = 0u8;

    for tick in 0..TIMEOUT_TICKS {
        std::thread::sleep(POLL_INTERVAL);
        let announce = tick % ANNOUNCE_EVERY == 0;

        if read_jar(&win).is_err() {
            consecutive_failures += 1;
            if consecutive_failures >= 3 {
                println!("  cookies() unreachable — the webview is gone. Stopping.");
                return;
            }
            continue;
        }
        consecutive_failures = 0;

        let outcome = rt.block_on(validate_window(&http, &win));
        if first_answer.is_none() {
            first_answer = outcome.clone();
        }

        // **Q8 does not need a session, and making it wait for one was a cost
        // paid by a person rather than the machine.** Its question is whether
        // the forget call empties the jar and in which order it must run — both
        // answerable against whatever cookies are present, and an anonymous
        // icloud.com load leaves plenty. Gating it behind a Complete verdict
        // meant every Q8 run spent another password and 2FA on evidence it did
        // not use. What an anonymous jar cannot show is a *session* cookie
        // surviving the clear, so the output says which jar it cleared.
        if forget {
            if outcome.is_none() {
                continue; // the page has not reached Apple yet; nothing to clear
            }
            let signed_in = matches!(outcome, Some(ValidateOutcome::Complete { .. }));
            run_forget(&app, &win, &rt, signed_in);
            app.exit(0);
            return;
        }

        match &outcome {
            Some(ValidateOutcome::Complete { .. }) => {
                let already = matches!(first_answer, Some(ValidateOutcome::Complete { .. }));
                report(
                    &app,
                    &win,
                    &rt,
                    &http,
                    outcome.as_ref().unwrap(),
                    store_existed,
                    already,
                );
                // **Exit cleanly, and that is a measurement decision rather
                // than tidiness.** Leaving the process up means the next run
                // begins by killing it, and a force-kill denies the engine any
                // chance to flush its cookie store — which makes Q7b
                // unreadable, as it did on the first attempt. Ending through
                // Tauri lets the runtime tear the web context down the way a
                // real quit would.
                println!("\n  (exiting cleanly so the next run can answer Q7b)");
                std::thread::sleep(Duration::from_millis(500));
                app.exit(0);
                return;
            }
            Some(ValidateOutcome::NoCloudKitService) => {
                println!(
                    "\n  Apple says this Apple ID is signed in but has no CloudKit Notes\n  \
                     service. Turn Notes on for iCloud on an Apple device and re-run.\n  \
                     (A real answer about the ACCOUNT, not about this engine.)"
                );
                app.exit(0);
                return;
            }
            // Two-factor is always worth a line: it is the one transition that
            // tells the user Apple accepted the password, and swallowing it
            // would make the most anxious moment of the flow look like nothing
            // had happened.
            Some(ValidateOutcome::TwoFactorPending) => {
                println!("  [{}] password accepted — waiting for two-factor", elapsed(tick))
            }
            Some(ValidateOutcome::Failed(m)) if announce => {
                println!("  [{}] /validate: {m}", elapsed(tick))
            }
            Some(ValidateOutcome::NotSignedIn) if announce => {
                println!("  [{}] not signed in yet — sign in whenever you are ready", elapsed(tick))
            }
            None if announce => println!(
                "  [{}] waiting for the page to reach Apple — no client config captured yet",
                elapsed(tick)
            ),
            _ => {}
        }
    }
    println!(
        "\n  (gave up after {} minutes without a completed sign-in — close the window to exit)",
        TIMEOUT_TICKS * POLL_INTERVAL.as_secs() as u32 / 60
    );
}

/// `[4m30s]` rather than `[270s]` — the number is read by a person deciding
/// whether anything is wrong, and minutes are what they think in.
fn elapsed(tick: u32) -> String {
    let secs = (tick + 1) * POLL_INTERVAL.as_secs() as u32;
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

fn report(
    app: &tauri::AppHandle,
    win: &tauri::WebviewWindow,
    rt: &tokio::runtime::Runtime,
    http: &reqwest::Client,
    complete: &ValidateOutcome,
    store_existed: bool,
    already_signed_in_at_startup: bool,
) {
    let all = match read_jar(win) {
        Ok(v) => v,
        Err(e) => {
            println!("  the jar became unreadable just as the report began: {e}");
            return;
        }
    };

    println!("\n════════ REPORT — {} ════════\n", engine());

    println!("── every cookie in the probe profile (names, flags, raw domain) ──");
    println!(
        "  {:<32} {:<16} {:>5} {:>7} {:<11} {:>6}",
        "NAME", "DOMAIN (raw)", "HTTP", "SECURE", "EXPIRES", "LEN"
    );
    for c in &all {
        println!(
            "  {:<32} {:<16} {:>5} {:>7} {:<11} {:>6}",
            truncate(&c.name, 32),
            truncate(&c.domain, 16),
            if c.http_only { "yes" } else { "no" },
            if c.secure { "yes" } else { "no" },
            truncate(&c.expires, 11),
            c.len
        );
    }
    // Path is off the table to keep it narrow, but it is not decoration:
    // `cookie_header_for` does RFC 6265 path matching, so a cookie scoped below
    // `/` is one the header builder can legitimately withhold. Name the
    // exceptions rather than printing a column of slashes.
    let scoped: Vec<&str> = all.iter().filter(|c| c.path != "/").map(|c| c.name.as_str()).collect();
    println!(
        "  path: all at / except {}",
        if scoped.is_empty() { "none".to_string() } else { scoped.join(", ") }
    );

    let apple: Vec<&Seen> = all.iter().filter(|c| is_apple_cookie(&c.name)).collect();
    let marker = |n: &str| all.iter().find(|c| c.name == n).and_then(decoded);

    // ── Q1 ──────────────────────────────────────────────────────────────
    println!("\n── Q1: does Apple's sign-in complete inside {}? ──", engine());
    println!(
        "  user agent Apple was given: {}",
        marker("jodd_icloud_probe_ua").unwrap_or_else(|| "<not captured>".into())
    );
    println!(
        "  page title Apple served:    {}",
        marker("jodd_icloud_probe_title").unwrap_or_else(|| "<not captured>".into())
    );
    if let ValidateOutcome::Complete { apple_id, ck_host, .. } = complete {
        println!("  ANSWER: YES. /validate returned Complete.");
        println!("    Apple ID:            {apple_id}");
        println!("    CloudKit partition:  {ck_host}");
        println!(
            "  The strong form of the answer: the verdict travelled through the\n  \
             SHIPPED layer to get here — harvested with cookies(), matched by\n  \
             cookie_header_for, classified by classify_validate."
        );
    }

    // ── Q2 ──────────────────────────────────────────────────────────────
    let http_only = apple.iter().filter(|c| c.http_only).count();
    let underscored = apple
        .iter()
        .filter(|c| c.name.to_ascii_uppercase().starts_with("X_APPLE"))
        .count();
    println!("\n── Q2: does cookies() return HttpOnly cookies? ──");
    println!("  Apple cookies in a SIGNED-IN jar: {} total, {http_only} HttpOnly", apple.len());
    println!(
        "  ({} X_APPLE_…-with-underscores. WebKit measured 17 total / 2 underscored\n  \
         on 2026-08-22; WebView2 measured 18 / 1 on 2026-09-09.)",
        underscored
    );
    if underscored > 0 {
        println!(
            "  ⚠ A name filter written `starts_with(\"X-APPLE\")` drops the underscored\n  \
             ones silently. Component B must domain-match, not name-match."
        );
    }
    if http_only > 0 {
        println!("  ANSWER: YES. Harvesting through cookies() reaches the session.");
    } else {
        println!(
            "  ANSWER: NO — yet /validate said Complete, so the session travelled some\n  \
             other way. Read the table before concluding anything."
        );
    }

    // ── Q3 ──────────────────────────────────────────────────────────────
    println!("\n── Q3: what shape is domain() on this engine? ──");
    let dotted = all.iter().filter(|c| c.domain.starts_with('.')).count();
    let none = all.iter().filter(|c| c.domain == "<none>").count();
    println!(
        "  {dotted} cookie(s) carry a LEADING DOT, {none} report no domain at all,\n  \
         {} report a bare host.",
        all.len() - dotted - none
    );
    if dotted > 0 || none > 0 {
        println!(
            "  ANSWER: the host-only distinction IS recoverable here — a leading dot\n  \
             (or an absent domain) tells a domain cookie from a host-only one.\n  \
             CONSEQUENCE: `HarvestedCookie::host_only` could be populated truthfully\n  \
             on this engine. Its doc comment records the WebKit loss as unavoidable;\n  \
             that sentence is about WebKit and must not be widened to 'always false'."
        );
    } else {
        println!(
            "  ANSWER: the same loss WebKit has — a host-only cookie on icloud.com is\n  \
             indistinguishable from a domain cookie for .icloud.com, so\n  \
             `host_only: false` stays correct and the over-send it causes stays\n  \
             confined to Apple's own hosts.\n  \
             (NOT the same as 'no information': a cookie scoped to www.icloud.com does\n  \
             report www.icloud.com. What is lost is only the dot, i.e. the\n  \
             domain-vs-host-only flag on the SAME name.)"
        );
    }

    // ── the cookies_for_url finding ─────────────────────────────────────
    println!("\n── cookies_for_url() vs cookies(): is the WebKit trap the same trap? ──");
    println!(
        "  On WebKit, cookies_for_url compares cookie.domain() == url.domain() as\n  \
         exact strings while cookie::Cookie::domain() strips a leading dot, so it\n  \
         returned 0 of Apple's .icloud.com cookies for www.icloud.com. WebView2\n  \
         passes the URI to GetCookies and lets the engine match, so it can answer\n  \
         differently — measure, do not assume either way."
    );
    for probe_url in ["https://www.icloud.com", "https://icloud.com", "https://setup.icloud.com"] {
        match tauri::Url::parse(probe_url)
            .map_err(|e| e.to_string())
            .and_then(|u| win.cookies_for_url(u).map_err(|e| e.to_string()))
        {
            Ok(cs) => {
                let n = cs.iter().filter(|c| is_apple_cookie(c.name())).count();
                println!(
                    "  cookies_for_url({probe_url:<28}) → {:>3} cookie(s), {n} X-APPLE-*",
                    cs.len()
                );
            }
            Err(e) => println!("  cookies_for_url({probe_url}) failed: {e}"),
        }
    }
    println!(
        "  cookies()                                    → {:>3} cookie(s), {} X-APPLE-*",
        all.len(),
        apple.len()
    );
    println!(
        "  READ THIS AS: even where the www row is healthy, the SHARED code must keep\n  \
         using cookies() — WebKit needs it, and one harvest path that behaves\n  \
         identically on both engines beats two that agree by coincidence. Do NOT\n  \
         'fix' WebKit by asking for the bare domain: that works by accident and\n  \
         drops host-only cookies on www."
    );

    // ── Q4 ──────────────────────────────────────────────────────────────
    println!("\n── Q4: did the initialization_script beat Apple's JS? ──");
    match (marker("jodd_icloud_probe_ran"), marker(CLIENT_CFG_COOKIE)) {
        (None, _) => println!(
            "  ANSWER: no script ran at all (no jodd_icloud_probe_ran cookie) — yet\n  \
             /validate succeeded, which cannot happen without the client config.\n  \
             Something is inconsistent; read the table."
        ),
        (Some(_), None) => println!(
            "  ANSWER: NO. A script ran but captured nothing — inconsistent with a\n  \
             Complete verdict, which needs the config. Read the table."
        ),
        (Some(_), Some(cfg)) => {
            println!("  ANSWER: YES. Captured the client parameters off Apple's own request.");
            println!("  {cfg}");
            println!(
                "  ⚠ Compare clientBuildNumber against what CLAUDE.md records (2628Build44,\n  \
                   itself already drifted from icloud-md's hardcoded 2624Build27). Drift\n  \
                   is expected and is exactly why Component B5 reads it live."
            );
            println!("  CONSEQUENCE: Component B5 works on this engine.");
        }
    }
    if let Some(s) = marker("jodd_icloud_probe_seen") {
        println!("  (the wrapper observed {s} request(s) before reporting)");
    }

    // ── Q5 ──────────────────────────────────────────────────────────────
    println!("\n── Q5: does the isolation lever keep this jar out of the app's? ──");
    match app.get_webview_window(CONTROL) {
        None => println!(
            "  The '{CONTROL}' window is not there, so this run has no control and Q5\n  \
             is UNANSWERED. It comes from tauri.conf.json; if that changed, point\n  \
             CONTROL at whatever window now runs on the default web context."
        ),
        Some(control) => match read_jar(&control) {
            Err(e) => println!("  could not read the control window's jar: {e} — Q5 UNANSWERED"),
            Ok(cs) => {
                let leaked = cs.iter().filter(|c| is_apple_cookie(&c.name)).count();
                let ours = cs.iter().filter(|c| is_marker(&c.name)).count();
                println!(
                    "  default-context window '{CONTROL}': {} cookie(s), {leaked} X-APPLE-*, \
                     {ours} jodd_icloud_*",
                    cs.len()
                );
                if leaked == 0 && ours == 0 {
                    println!(
                        "  ANSWER: ISOLATED. cookies() with a null URI returns the WHOLE\n  \
                         profile regardless of what the window has loaded, so seeing none\n  \
                         of the probe's {} cookies is evidence of two profiles rather than\n  \
                         an artifact of this window never visiting icloud.com.",
                        all.len()
                    );
                } else {
                    println!(
                        "  ANSWER: NOT ISOLATED — the app's own context can see the probe's\n  \
                         cookies. Two things break at once: sign-out would have to wipe\n  \
                         Jodd's own storage along with the session, and a second Apple ID\n  \
                         would share the first one's jar with nothing to separate them.\n  \
                         On macOS < 14 this is the documented SILENT fallback to the shared\n  \
                         store — check the version line at the top of this run first."
                    );
                }
            }
        },
    }

    // ── Q6 ──────────────────────────────────────────────────────────────
    println!("\n── Q6: does a second window on the same store re-authenticate silently? ──");
    match open_session_window(app) {
        Err(e) => println!("  could not open the session window: {e} — Q6 UNANSWERED"),
        Ok(session) => {
            // Component B4's own cold-start allowance, for the same reason: a
            // window created microseconds ago has an empty jar because the page
            // has not loaded, not because the session is dead.
            let mut verdict: Option<ValidateOutcome> = None;
            for _ in 0..40 {
                std::thread::sleep(Duration::from_millis(500));
                if let Some(o) = rt.block_on(validate_window(http, &session)) {
                    let done = matches!(o, ValidateOutcome::Complete { .. });
                    verdict = Some(o);
                    if done {
                        break;
                    }
                }
            }
            match verdict {
                Some(ValidateOutcome::Complete { ref apple_id, .. }) => println!(
                    "  ANSWER: YES — the hidden window's OWN jar validated as {apple_id},\n  \
                     with no interaction.\n  \
                     CONSEQUENCE: Component B4 (the hidden session webview) works here, so\n  \
                     a session outliving the sign-in window is real on this engine."
                ),
                other => println!(
                    "  ANSWER: NO — twenty seconds passed and the hidden window's jar never\n  \
                     validated (last: {other:?}).\n  \
                     CONSEQUENCE: B4 needs a different mechanism here, or the sign-in\n  \
                     window has to stay resident. Re-run before believing it: twenty\n  \
                     seconds is the shipped cold-start allowance (WARMUP_TICKS in\n  \
                     icloud_auth.rs) and a slow machine can exceed it."
                ),
            }
            let _ = session.close();
        }
    }

    // ── Q7 ──────────────────────────────────────────────────────────────
    //
    // Split in two, because the first attempt could not tell its own method
    // from the finding. A second run showed the session gone, but the previous
    // run had been force-killed, which gives the engine no chance to flush its
    // cookie store — so "Apple's cookies do not persist" and "the probe
    // destroyed them on the way out" were the same observation. Q7a settles
    // which, from the cookies themselves, before Q7b is allowed to mean
    // anything.
    println!("\n── Q7a: are Apple's session cookies PERSISTENT or session-scoped? ──");
    let session_scoped: Vec<&&Seen> = apple
        .iter()
        .filter(|c| c.expires == "session" || c.expires == "<unset>")
        .collect();
    println!(
        "  Of {} Apple cookies, {} carry no expiry (session-scoped) and {} carry a date.",
        apple.len(),
        session_scoped.len(),
        apple.len() - session_scoped.len()
    );
    for c in &session_scoped {
        println!("    session-scoped: {}", c.name);
    }
    if session_scoped.is_empty() {
        println!(
            "  ANSWER: all persistent. A store that survives shutdown carries the session\n  \
             with it, so Q7b below is a clean measurement of persistence."
        );
    } else {
        println!(
            "  ANSWER: {} of Apple's cookies are session-scoped — an engine may drop them\n  \
             at profile shutdown WHATEVER the store keeps on disk. Session scoping is\n  \
             APPLE'S choice, not the engine's, so expect this number to be similar on\n  \
             both and compare them before calling either platform worse.\n  \
             CONSEQUENCE: read Q7b against this. If the names above are the ones\n  \
             /validate needs, no amount of graceful shutdown will keep the session, and\n  \
             this platform means signing in per launch — a product fact, not a bug.",
            session_scoped.len()
        );
    }

    println!("\n── Q7b: did the session survive process exit? ──");
    let known_store = cfg!(not(target_vendor = "apple"));
    match (known_store && store_existed, already_signed_in_at_startup) {
        (_, true) => println!(
            "  The very first /validate — before you touched the window — already\n  \
             answered Complete.\n  \
             ANSWER: YES. The session survives process exit on this engine."
        ),
        (true, false) => println!(
            "  The store existed but the first /validate did NOT answer Complete.\n  \
             ANSWER: the session did not survive — but only trust this if the previous\n  \
             run exited GRACEFULLY. A force-kill denies the engine its flush, and that\n  \
             confound produced this exact line once already. This probe calls\n  \
             app.exit(0) after reporting, so a run that ends on its own is clean."
        ),
        (false, false) => println!(
            "  No prior session was found at startup. If this was a first run that is\n  \
             expected; the probe will now exit cleanly, and a Complete on the FIRST tick\n  \
             of the next run is the answer.\n  \
             ANSWER: UNANSWERED — which is not the same as no."
        ),
    }

    println!("\n════════ END — {} ════════", engine());
}

/// Opens the Component B4 rehearsal window: same store, hidden, same scripts.
///
/// Reuses one if a previous report left it around, so a re-run does not fail on
/// a duplicate label.
fn open_session_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(w) = app.get_webview_window(SESSION) {
        return Ok(w);
    }
    let url = tauri::Url::parse(ICLOUD_URL).map_err(|e| e.to_string())?;
    // The SAME store, or Apple's JS has nothing to re-authenticate against and
    // this window is a stranger.
    isolated(WebviewWindowBuilder::new(app, SESSION, WebviewUrl::External(url)))
        .title("Jodd — iCloud session (probe)")
        .visible(false)
        .initialization_script(INIT_SCRIPT)
        .initialization_script(PROBE_EXTRA)
        .build()
        .map_err(|e| e.to_string())
}

/// Q8 — the forget path, and **the ordering that inverts between engines**.
///
/// WebKit refuses to delete a store that is still in use, so the Apple arm
/// closes the windows and only then calls `remove_data_store`. WebView2 reaches
/// the profile *through* a live webview's `ICoreWebView2Profile2`, so the other
/// arm must clear FIRST and close after. One job, two opposite orders — the
/// kind of asymmetry that ships as a silent no-op if it is assumed rather than
/// measured, which is why this exists.
///
/// Destructive, hence `--forget`. It does not wait for a sign-in: an anonymous
/// icloud.com load leaves plenty of cookies to answer whether the call empties
/// the jar. `signed_in` is carried through only so the output can say which
/// kind of jar was cleared — an anonymous one cannot show a real session cookie
/// surviving, and the report must not let a reader assume more than was
/// measured.
fn run_forget(
    app: &tauri::AppHandle,
    win: &tauri::WebviewWindow,
    rt: &tokio::runtime::Runtime,
    signed_in: bool,
) {
    println!("\n── Q8: does the forget path empty the jar, on {}? ──", engine());
    println!(
        "  jar under test: {}",
        if signed_in {
            "a SIGNED-IN session — the full answer, session cookies included"
        } else {
            "an ANONYMOUS session: the API and its ordering, not a real session cookie"
        }
    );
    let before = read_jar(win).map(|v| v.len()).unwrap_or(0);

    #[cfg(target_vendor = "apple")]
    {
        for label in [SIGNIN, SESSION] {
            if let Some(w) = app.get_webview_window(label) {
                let _ = w.close();
            }
        }
        // A window closed microseconds ago may still hold the store open, and
        // WebKit's refusal in that case is indistinguishable from any other
        // failure. One short wait costs nothing on the path that already
        // succeeds.
        std::thread::sleep(Duration::from_millis(300));
        match rt.block_on(app.remove_data_store(DATA_STORE_ID)) {
            Ok(()) => println!(
                "  remove_data_store: OK.\n  \
                 ANSWER: YES — and note it ran AFTER the windows closed, which is the\n  \
                 opposite order from the WebView2 arm."
            ),
            Err(e) => println!(
                "  remove_data_store failed: {e}\n  \
                 ANSWER: the store was not removed. A later sign-in may find the\n  \
                 previous Apple ID still signed in; /validate reports whose session it\n  \
                 is. Check nothing still holds the store open."
            ),
        }
        let _ = before;
        return;
    }

    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = app;
        // **Clear BEFORE closing** — see this function's doc comment.
        if let Err(e) = win.clear_all_browsing_data() {
            println!(
                "  clear_all_browsing_data() failed: {e}\n  \
                 ANSWER: unavailable — forget_session needs another arm here.\n  \
                 (WebView2 needs runtime 96+ for ICoreWebView2Profile2; check the\n  \
                 installed runtime version before concluding the API is wrong.)"
            );
            return;
        }

        // Fire-and-forget: wry hands `ClearBrowsingDataAll` a completion handler
        // that discards the result, so the only way to see the effect is to read
        // the jar again.
        let mut after = before;
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(500));
            match read_jar(win) {
                Ok(v) => {
                    after = v.len();
                    if after == 0 {
                        break;
                    }
                }
                // The webview going away mid-clear is not a failure of the clear.
                Err(_) => break,
            }
        }

        println!("  jar: {before} cookie(s) before → {after} after");
        if after == 0 {
            println!(
                "  ANSWER: YES. This is forget_session's arm here, and it must run BEFORE\n  \
                 the windows are closed — the opposite order from the Apple arm."
            );
        } else {
            println!(
                "  ANSWER: NOT FULLY — {after} cookie(s) survived. Re-run and read the table\n  \
                 to see WHICH: session cookies surviving means forget_session would leave\n  \
                 the previous Apple ID signed in, which is the wrong-account bug gotcha\n  \
                 #19's counterpart exists to prevent, not merely a leak."
            );
        }
        let _ = rt;
    }
}

/// Jodd's own markers are written with `encodeURIComponent`, so they arrive
/// percent-encoded. Printing them raw turns a user agent into line noise.
fn decoded(c: &Seen) -> Option<String> {
    let raw = c.value.as_deref()?;
    Some(urlencoding::decode(raw).map(|s| s.into_owned()).unwrap_or_else(|_| raw.to_string()))
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
