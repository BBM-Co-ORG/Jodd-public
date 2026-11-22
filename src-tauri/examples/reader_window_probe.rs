//! Does a destroyed private-reader window really go away? (macOS)
//!
//! The 2026-10-29 live pass (spec 2026-10-28-follow-links-and-discussions-design.md
//! §19.3) found that every private read left an empty `NSWindow` behind until
//! quit. This probe opens and destroys webview windows the way `ingest::render`
//! does, a few times per variant, and counts the app's windows afterwards —
//! in AppKit (`NSApp.windows`) and in the window server
//! (`CGWindowListCopyWindowInfo` for this pid).
//!
//! ```text
//! npm run build                                   # generate_context! needs dist/
//! cargo run --example reader_window_probe                      # every variant
//! cargo run --example reader_window_probe -- --only reader,hidden --n 5
//! cargo run --example reader_window_probe -- --only hidden --trace --log
//! cargo run --example reader_window_probe -- --visible   # + the 2 on-screen variants
//! ```
//!
//! **What it found.** Every variant left one window per read — visible or
//! hidden, with or without incognito, the init script or any handler, and
//! `close()` as much as `destroy()` — so no builder option was the cause.
//! `--trace` showed tao's window and its delegate dropped, `windowWillClose:`
//! ran and the WKWebView was freed, yet the `NSWindow` kept exactly one
//! reference. That one was tao's own: `create_window` took the owned result of
//! `alloc` + `init…` with `Retained::retain`, a second reference nothing
//! released (tao #1120). Fixed first in Jodd's tao fork, then for good by
//! tao 0.37's `define_class!` window, which tauri 2.12 takes from crates.io;
//! with either, every row reads 0.
//!
//! Each variant names the builder options it turns on, so a leak can be pinned
//! to one option by comparing rows. `reader` is the shipped reader itself —
//! `jodd_lib::ingest::render::reader_window_builder`, not a copy of it.
//!
//! A row's `left` counts titled windows AppKit holds after the variant's reads
//! that it did not hold before. 0 is the goal. The first window ever shown
//! makes AppKit create one untitled window of its own (500×500, off-screen,
//! plus the menu-bar strips in the window server) — listed, never counted.
//!
//! Options:
//! - `--only a,b` — run these variants; `--n N` — reads per variant (3);
//!   `--url U` — what the windows load (`https://example.com/`).
//! - `--visible` — also run `visible` and `all-options-visible`, which put a
//!   window on screen for 2.5 s per read. Off by default: a probe run must not
//!   pop windows over whatever the owner is doing. (Hidden variants still make
//!   the probe the active app for a moment per read — wry activates the app
//!   whenever it creates a webview.)
//! - `--trace` — per read: does the `NSWindow`, its content view and its
//!   WKWebView survive the destroy, and did Tauri's `Destroyed` event fire.
//! - `--log` — tao's and wry's own trace lines about closing and dropping.
//! - `--pre a,b` — undo things on the window just before destroying it:
//!   `responder` (makeFirstResponder nil), `content` (setContentView nil),
//!   `detach` (WKWebView removeFromSuperview), `orderout`.
//! - `--hold S` — stay alive S seconds at the end, for `leaks --traceTree`.

use std::time::Duration;
use tauri::{webview::NewWindowResponse, RunEvent, WebviewUrl, WebviewWindowBuilder};

/// (name, options). Options: `hidden`, `incognito`, `init`, `nav`, `newwin`,
/// `download`, `close` (close() instead of destroy()), `shipped` (the
/// reader's own builder; the other options are ignored then).
const VARIANTS: &[(&str, &str)] = &[
    ("visible", ""),
    ("hidden", "hidden"),
    ("hidden-incognito", "hidden,incognito"),
    ("hidden-init", "hidden,init"),
    ("hidden-nav", "hidden,nav"),
    ("hidden-newwin", "hidden,newwin"),
    ("hidden-download", "hidden,download"),
    ("all-options", "hidden,incognito,init,nav,newwin,download"),
    ("all-options-visible", "incognito,init,nav,newwin,download"),
    ("all-options-close", "hidden,incognito,init,nav,newwin,download,close"),
    ("reader", "shipped"),
];

/// How long a window lives, and how long to wait after destroying it before
/// counting. A read is 3–6 s; the page only needs to have started.
const LIVE: Duration = Duration::from_millis(2500);
const SETTLE: Duration = Duration::from_millis(2000);

struct Config {
    only: Option<Vec<String>>,
    n: usize,
    url: String,
    trace: bool,
    visible: bool,
    pre: Vec<String>,
    hold: u64,
}

fn config() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let list = |flag: &str| value(flag).map(|s| s.split(',').map(str::to_string).collect::<Vec<_>>());
    Config {
        only: list("--only"),
        n: value("--n").and_then(|s| s.parse().ok()).unwrap_or(3),
        url: value("--url").unwrap_or_else(|| "https://example.com/".into()),
        trace: args.iter().any(|a| a == "--trace"),
        visible: args.iter().any(|a| a == "--visible"),
        pre: list("--pre").unwrap_or_default(),
        hold: value("--hold").and_then(|s| s.parse().ok()).unwrap_or(0),
    }
}

/// `--log`: tao's and wry's own `trace!` lines about windows closing and
/// dropping, so a run shows which teardown steps actually happen.
struct WindowLog;
impl log::Log for WindowLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.target().starts_with("tao") || m.target().starts_with("wry")
    }
    fn log(&self, r: &log::Record) {
        let msg = r.args().to_string();
        if self.enabled(r.metadata()) && (msg.contains("Drop") || msg.contains("lose") || msg.contains("estroy")) {
            println!("      [{}] {msg}", r.target());
        }
    }
    fn flush(&self) {}
}

fn main() {
    let cfg = config();
    if std::env::args().any(|a| a == "--log") {
        let _ = log::set_logger(&WindowLog);
        log::set_max_level(log::LevelFilter::Trace);
    }
    let app = tauri::Builder::default()
        .setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let left = run(&handle, cfg).await;
                handle.exit(if left == 0 { 0 } else { 1 });
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start the probe — did `npm run build` run? generate_context! needs dist/");
    // The probe has no main window, so destroying the last reader would end
    // the app before it can count. An explicit exit (a code) still quits.
    app.run(|_, e| {
        if let RunEvent::ExitRequested { code: None, api, .. } = e {
            api.prevent_exit();
        }
    });
}

/// Runs the variants; returns how many titled windows they left in all.
async fn run(app: &tauri::AppHandle, cfg: Config) -> i64 {
    println!("reader_window_probe — {} read(s) per variant against {}", cfg.n, cfg.url);
    println!("{:<22} {:>6} {:>6} {:>5}", "variant", "before", "after", "left");
    let mut total_left = 0i64;
    for (name, opts) in VARIANTS {
        if cfg.only.as_ref().is_some_and(|o| !o.iter().any(|x| x == name)) {
            continue;
        }
        // On-screen variants run only when asked for, by --visible or --only.
        if !opts.split(',').any(|o| o == "hidden" || o == "shipped") && !cfg.visible && cfg.only.is_none() {
            continue;
        }
        let before = windows_now(app);
        for i in 0..cfg.n {
            let label = format!("probe-{name}-{i}");
            let win = match build(app, &label, &cfg.url, opts) {
                Ok(w) => w,
                Err(e) => {
                    println!("{name}: could not build: {e}");
                    break;
                }
            };
            if cfg.trace {
                track(&win, &label);
            }
            tokio::time::sleep(LIVE).await;
            pre_destroy(&win, &cfg.pre);
            let res = if opts.contains("close") { win.close() } else { win.destroy() };
            if let Err(e) = res {
                println!("{name}: destroy/close returned an error: {e}");
            }
            drop(win);
            tokio::time::sleep(SETTLE).await;
            if cfg.trace {
                report(app, &label);
            }
        }
        let after = windows_now(app);
        let new: Vec<&Win> = after.iter().filter(|w| !before.contains(w)).collect();
        let left = new.iter().filter(|w| !w.title.is_empty()).count() as i64;
        total_left += left;
        println!("{name:<22} {:>6} {:>6} {:>5}", before.len(), after.len(), left);
        for w in new {
            let whose = if w.title.is_empty() { "  (untitled: AppKit's own, not counted)" } else { "" };
            println!("    left behind: {}{whose}", w.line);
        }
    }
    println!("titled windows left in total: {total_left}");
    window_server_windows();
    if cfg.hold > 0 {
        println!("holding {} s — pid {}", cfg.hold, std::process::id());
        tokio::time::sleep(Duration::from_secs(cfg.hold)).await;
    }
    total_left
}

fn build(app: &tauri::AppHandle, label: &str, url: &str, opts: &str) -> tauri::Result<tauri::WebviewWindow> {
    let parsed: tauri::Url = url.parse().expect("--url must be a URL");
    if opts.contains("shipped") {
        return jodd_lib::ingest::render::reader_window_builder(app, label, parsed).build();
    }
    let has = |o: &str| opts.split(',').any(|x| x == o);
    let mut b = WebviewWindowBuilder::new(app, label, WebviewUrl::External(parsed)).title("probe window");
    if has("hidden") {
        b = b.visible(false).focused(false).skip_taskbar(true);
    }
    if has("incognito") {
        b = b.incognito(true);
    }
    if has("init") {
        b = b.initialization_script("window.__probe = 1;");
    }
    if has("nav") {
        b = b.on_navigation(|_| true);
    }
    if has("newwin") {
        b = b.on_new_window(|_, _| NewWindowResponse::Deny);
    }
    if has("download") {
        b = b.on_download(|_, _| false);
    }
    b.build()
}

/// One `NSWindow` as AppKit lists it.
#[derive(PartialEq)]
struct Win {
    title: String,
    /// `#number "title" visible=…`
    line: String,
}

/// AppKit's view: every `NSWindow` the app still holds. A closed window stays
/// in this list for as long as anything retains it.
#[cfg(target_os = "macos")]
fn windows_now(app: &tauri::AppHandle) -> Vec<Win> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = app.run_on_main_thread(move || {
        let mtm = objc2::MainThreadMarker::new().expect("run_on_main_thread runs on the main thread");
        let ns_app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        let list = ns_app
            .windows()
            .iter()
            .map(|w| {
                let title = w.title().to_string();
                let line = format!("#{} \"{title}\" visible={}", w.windowNumber(), w.isVisible());
                Win { title, line }
            })
            .collect::<Vec<_>>();
        let _ = tx.send(list);
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
fn windows_now(_: &tauri::AppHandle) -> Vec<Win> {
    eprintln!("this probe counts windows on macOS only");
    std::process::exit(2);
}

/// The window server's view, from outside AppKit: windows owned by this pid.
/// Names need Screen Recording permission, so only numbers and bounds print.
fn window_server_windows() {
    if !cfg!(target_os = "macos") {
        return;
    }
    const SCRIPT: &str = r#"
import CoreGraphics
let pid = Int(CommandLine.arguments[1])!
let all = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
let mine = all.filter { ($0[kCGWindowOwnerPID as String] as? Int) == pid }
print("window server: \(mine.count) window(s) for pid \(pid)")
for w in mine {
    let b = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
    print("    #\(w[kCGWindowNumber as String]!) layer=\(w[kCGWindowLayer as String]!) onscreen=\(w[kCGWindowIsOnscreen as String] ?? false) \(b["Width"]!)x\(b["Height"]!)")
}
"#;
    let path = std::env::temp_dir().join("jodd_reader_window_probe.swift");
    if std::fs::write(&path, SCRIPT).is_err() {
        return;
    }
    match std::process::Command::new("swift").arg(&path).arg(std::process::id().to_string()).output() {
        Ok(o) => print!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
        Err(e) => println!("window server: could not run swift: {e}"),
    }
    let _ = std::fs::remove_file(path);
}

// `with_webview` LENDS the WKWebView, its content controller and the NSWindow
// (`Retained::as_ptr`, tauri-runtime-wry 2.12): never release them here.
// 2.11.4 handed them out through `Retained::into_raw`, a +1 per call, and this
// probe released them to compensate — on 2.12 that same release freed live
// objects and `--trace` died with SIGSEGV (exit 139) on its first read.

#[cfg(target_os = "macos")]
fn pre_destroy(win: &tauri::WebviewWindow, pre: &[String]) {
    use objc2_app_kit::{NSView, NSWindow};
    if pre.is_empty() {
        return;
    }
    let pre = pre.to_vec();
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = win.with_webview(move |pw| {
        // SAFETY: with_webview runs on the main thread with live pointers.
        let (window, webview) = unsafe { (&*(pw.ns_window() as *const NSWindow), &*(pw.inner() as *const NSView)) };
        for p in &pre {
            match p.as_str() {
                "responder" => {
                    window.makeFirstResponder(None);
                }
                "content" => window.setContentView(None),
                "detach" => webview.removeFromSuperview(),
                "orderout" => window.orderOut(None),
                other => println!("unknown --pre {other}"),
            }
        }
        let _ = tx.send(());
    });
    let _ = rx.recv_timeout(Duration::from_secs(5));
}

#[cfg(not(target_os = "macos"))]
fn pre_destroy(_: &tauri::WebviewWindow, _: &[String]) {}

/// `--trace`: weak references to one read's window, its content view and its
/// WKWebView, taken on the main thread while it is open, and checked after it
/// is destroyed — which of them survives, and in what state.
#[cfg(target_os = "macos")]
mod tracking {
    use objc2::rc::Weak;
    use objc2_app_kit::{NSView, NSWindow};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct Tracked {
        window: Weak<NSWindow>,
        content: Weak<NSView>,
        webview: Weak<NSView>,
        destroyed_event: Arc<AtomicBool>,
    }

    thread_local! {
        // Main thread only: Weak is not Send.
        static TRACKED: RefCell<HashMap<String, Tracked>> = RefCell::new(HashMap::new());
    }

    pub fn track(win: &tauri::WebviewWindow, label: &str) {
        let destroyed = Arc::new(AtomicBool::new(false));
        {
            let d = destroyed.clone();
            win.on_window_event(move |e| {
                if matches!(e, tauri::WindowEvent::Destroyed) {
                    d.store(true, Ordering::SeqCst);
                }
            });
        }
        let label = label.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = win.with_webview(move |pw| {
            // SAFETY: with_webview runs on the main thread with live pointers.
            let (window, webview) = unsafe { (&*(pw.ns_window() as *const NSWindow), &*(pw.inner() as *const NSView)) };
            let content = window.contentView();
            TRACKED.with(|t| {
                t.borrow_mut().insert(
                    label,
                    Tracked {
                        window: Weak::from(window),
                        content: content.as_deref().map(Weak::from).unwrap_or_default(),
                        webview: Weak::from(webview),
                        destroyed_event: destroyed,
                    },
                )
            });
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
    }

    /// Strong references, not counting the one `Weak::load` just took.
    fn rc<T: objc2::Message>(o: &T) -> usize {
        let n: usize = unsafe { objc2::msg_send![o, retainCount] };
        n - 1
    }

    pub fn report(app: &tauri::AppHandle, label: &str) {
        let label = label.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = app.run_on_main_thread(move || {
            let line = TRACKED.with(|t| {
                let t = t.borrow();
                let Some(x) = t.get(&label) else { return format!("{label}: not tracked") };
                let w = match x.window.load() {
                    None => "window: gone".to_string(),
                    Some(w) => format!(
                        "window: ALIVE retain={} visible={} delegate={} contentView={}",
                        rc(&*w),
                        w.isVisible(),
                        w.delegate().is_some(),
                        w.contentView().is_some(),
                    ),
                };
                let c = match x.content.load() {
                    None => "content view: gone".to_string(),
                    Some(v) => format!("content view: ALIVE retain={} in window={}", rc(&*v), v.window().is_some()),
                };
                let v = match x.webview.load() {
                    None => "WKWebView: gone".to_string(),
                    Some(v) => format!(
                        "WKWebView: ALIVE retain={} superview={} in window={}",
                        rc(&*v),
                        unsafe { v.superview() }.is_some(),
                        v.window().is_some()
                    ),
                };
                let d = x.destroyed_event.load(Ordering::SeqCst);
                format!("{label}: Destroyed event={d}\n      {w}\n      {c}\n      {v}")
            });
            let _ = tx.send(line);
        });
        if let Ok(line) = rx.recv_timeout(std::time::Duration::from_secs(5)) {
            println!("    {line}");
        }
    }
}

#[cfg(target_os = "macos")]
use tracking::{report, track};

#[cfg(not(target_os = "macos"))]
fn track(_: &tauri::WebviewWindow, _: &str) {}
#[cfg(not(target_os = "macos"))]
fn report(_: &tauri::AppHandle, _: &str) {}
