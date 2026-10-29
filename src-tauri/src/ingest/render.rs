//! The private reader (spec 2026-10-28-follow-links-and-discussions-design.md
//! §7.3, phase P4): a page that ships an empty `<body>` and builds itself in
//! JavaScript, read the way a browser would show it.
//!
//! **Only when the user presses "Open in a private reader"** on a source that
//! came back "no readable text" — never automatic, never for a followed link on
//! its own (§8: a webview resolves DNS itself, so the SSRF guard cannot pin it).
//!
//! What keeps it private, each a rule:
//! - **Its own throwaway store.** Incognito on every engine (WebView2 InPrivate,
//!   WebKit's non-persistent data store, WebKitGTK's ephemeral context), plus,
//!   on Windows, a fresh WebView2 user data folder per read, deleted after.
//!   Never the iCloud webview's store (gotcha #19), never the app's own.
//! - **No IPC.** Tauri refuses every command from a remote origin unless a
//!   capability names it (`remote`); none does, and
//!   `no_capability_reaches_the_private_reader` keeps it so.
//! - **The top-level URL passes the SSRF check first**, and every top-level
//!   navigation after that must stay on http(s) to a host that is not a literal
//!   private address or a local name ([`navigation_allowed`]). The final page's
//!   host is vetted again before its HTML is used.
//! - **Nothing is clicked, typed or scrolled.** One script reads
//!   `document.documentElement.outerHTML` back through the engine's own
//!   script-result channel (`eval_with_callback`), not through IPC.
//!
//! Desktop only: gotcha #31 lists the Android webview calls an invisible,
//! isolated, closable webview depends on, and they do nothing there.

use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Every reader window's label starts with this. The capability test matches
/// against it, so a capability can never reach a reader by accident.
pub const READER_LABEL_PREFIX: &str = "jodd-reader-";

/// What the command says on Android (spec §7.3).
pub const DESKTOP_ONLY: &str = "The private reader is desktop only for now.";

/// First line of a rendered source's text. Kept in the stored source block, so
/// a Re-extract says "rendered" exactly as the ingest did (spec §8: the note
/// records that the page was rendered).
pub const RENDERED_MARK: &str = "[Read from the page as rendered in a private reader]";

/// What the note's Sources list adds after such a source's status.
pub const RENDERED_STATUS: &str = "rendered in a private reader";

/// Load plus network idle, at most (spec §7.3).
pub const RENDER_DEADLINE: Duration = Duration::from_secs(15);
const PROBE_EVERY: Duration = Duration::from_millis(500);
/// A probe answers well inside this, or the page is busy and the next tick asks again.
const PROBE_WAIT: Duration = Duration::from_secs(2);
const READ_WAIT: Duration = Duration::from_secs(5);
/// Unchanged probes in a row that count as "idle" — 1.5 s of no new resources
/// and no new text after `load`.
const QUIET_PROBES: u32 = 3;
/// The HTML read back, at most. Matches the plain fetch's body cap.
pub const MAX_RENDERED_CHARS: usize = 4_000_000;

/// Rendered pages wait here for the ingest that uses them.
const CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const CACHE_MAX: usize = 8;

/// Runs before any of the page's own scripts. A hidden window must not stall
/// on a dialog nobody can see, open another window, or play sound.
pub const INIT_SCRIPT: &str = r#"(function () {
  try {
    window.alert = function () {};
    window.confirm = function () { return false; };
    window.prompt = function () { return null; };
    window.print = function () {};
    window.open = function () { return null; };
    document.addEventListener('play', function (e) {
      try { e.target.muted = true; e.target.pause(); } catch (_) {}
    }, true);
  } catch (_) {}
})();"#;

/// Where the page is, whether it finished loading, how many resources it has
/// asked for and how much text it shows. Returned as a JSON string, so every
/// engine hands back the same shape (each JSON-encodes the script's result).
pub const PROBE_SCRIPT: &str = r#"(function () {
  try {
    var n = (performance.getEntriesByType && performance.getEntriesByType('resource') || []).length;
    var t = (document.body && document.body.innerText || '').length;
    return JSON.stringify({ u: String(location.href), r: String(document.readyState), n: n, t: t });
  } catch (e) {
    return JSON.stringify({ u: '', r: '', n: 0, t: 0 });
  }
})()"#;

/// The one read (spec §7.3).
pub fn read_script() -> String {
    format!(
        r#"(function () {{
  try {{
    return JSON.stringify({{ u: String(location.href), h: document.documentElement.outerHTML.slice(0, {MAX_RENDERED_CHARS}) }});
  }} catch (e) {{
    return JSON.stringify({{ u: '', h: '' }});
  }}
}})()"#
    )
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Probe {
    pub u: String,
    pub r: String,
    pub n: u64,
    pub t: u64,
}

#[derive(Debug, Deserialize)]
pub struct Read {
    pub u: String,
    pub h: String,
}

/// What `eval_with_callback` hands back is the JSON encoding of the script's
/// result; the result is itself a JSON string. Anything else is `None`.
pub fn decode<T: DeserializeOwned>(raw: &str) -> Option<T> {
    let inner: String = serde_json::from_str(raw).ok()?;
    serde_json::from_str(&inner).ok()
}

/// Load + network idle: the page says `complete`, and neither its resource
/// count nor its text length moved for [`QUIET_PROBES`] probes in a row.
#[derive(Default)]
pub struct Settle {
    last: Option<(String, u64, u64)>,
    quiet: u32,
}

impl Settle {
    pub fn observe(&mut self, p: &Probe) -> bool {
        let on_page = p.u.starts_with("http://") || p.u.starts_with("https://");
        if p.r != "complete" || !on_page {
            self.last = None;
            self.quiet = 0;
            return false;
        }
        let key = (p.u.clone(), p.n, p.t);
        if self.last.as_ref() == Some(&key) {
            self.quiet += 1;
        } else {
            self.last = Some(key);
            self.quiet = 0;
        }
        self.quiet >= QUIET_PROBES
    }
}

/// A name that only means something on this machine's network.
fn is_local_name(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h == "localhost"
        || !h.contains('.')
        || [".localhost", ".local", ".internal", ".home.arpa", ".lan"].iter().any(|s| h.ends_with(s))
}

/// Whether the reader may go to `url` as a top-level page. Synchronous — the
/// engine asks on its UI thread — so names are not resolved here; literal
/// private addresses and local names are refused, and the final page's host is
/// resolved and vetted before its HTML is used ([`vet_final`]).
pub fn navigation_allowed(url: &reqwest::Url) -> bool {
    match url.scheme() {
        "http" | "https" => {}
        // The blank page an engine starts from.
        "about" => return url.as_str() == "about:blank",
        _ => return false,
    }
    let Some(host) = url.host_str() else { return false };
    match host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
        Ok(ip) => !super::net::is_forbidden(ip),
        Err(_) => !is_local_name(host),
    }
}

/// The final page — after the page's own redirects — before its HTML is used.
pub async fn vet_final(final_url: &str, policy: super::FetchPolicy, cancel: &tokio_util::sync::CancellationToken) -> Result<(), String> {
    let parsed = reqwest::Url::parse(final_url).map_err(|_| "the page ended on an address Jodd cannot read".to_string())?;
    if !navigation_allowed(&parsed) {
        return Err("the page ended on an address Jodd does not open".into());
    }
    super::net::vet(final_url, policy, cancel).await
}

/// One page as the reader saw it.
#[derive(Clone)]
pub struct RenderedPage {
    /// The source URL the user pressed the button on.
    pub url: String,
    /// Where the page ended, after its own redirects.
    pub final_url: String,
    pub html: String,
    at: Instant,
}

impl RenderedPage {
    pub fn new(url: &str, final_url: &str, html: String) -> Self {
        RenderedPage { url: url.to_string(), final_url: final_url.to_string(), html, at: Instant::now() }
    }
}

static CACHE: Mutex<Vec<RenderedPage>> = Mutex::new(Vec::new());

/// Keep a rendered page for the ingest that follows. Newest wins per URL; at
/// most [`CACHE_MAX`], none older than [`CACHE_TTL`].
pub fn remember(page: RenderedPage) {
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    cache.retain(|p| p.url != page.url && p.at.elapsed() < CACHE_TTL);
    cache.push(page);
    let excess = cache.len().saturating_sub(CACHE_MAX);
    cache.drain(..excess);
}

/// The rendered pages for these URLs. Copied, not taken: an ingest that fails
/// for another reason can be retried without rendering again.
pub fn cached(urls: &[String]) -> HashMap<String, RenderedPage> {
    let cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    cache
        .iter()
        .filter(|p| p.at.elapsed() < CACHE_TTL && urls.contains(&p.url))
        .map(|p| (p.url.clone(), p.clone()))
        .collect()
}

/// Whether a source's text came from the reader.
pub fn was_rendered(text: &str) -> bool {
    text.starts_with(RENDERED_MARK)
}

#[cfg(desktop)]
mod window {
    use super::*;
    use tauri::{webview::NewWindowResponse, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
    use tokio_util::sync::CancellationToken;

    const CANCELLED: &str = "cancelled";

    /// A fresh directory per read (WebView2 only), under the app's own data
    /// dir — a sibling of nothing the iCloud webview or the main window uses.
    fn reader_dir(label: &str) -> std::path::PathBuf {
        crate::paths::data_base().unwrap_or_else(std::env::temp_dir).join("jodd").join("reader-webview").join(label)
    }

    /// The engine can hold files a moment after the window is gone (WebView2's
    /// browser process exits on its own time), so removal retries for a while.
    /// Whatever is left is swept by [`sweep_leftovers`] on the next read.
    fn forget_dir(dir: std::path::PathBuf) {
        tauri::async_runtime::spawn(async move {
            for _ in 0..20 {
                if !dir.exists() || std::fs::remove_dir_all(&dir).is_ok() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            crate::log!("private reader: could not delete a reader data directory yet; the next read sweeps it");
        });
    }

    /// Directories a crash or a busy engine left behind.
    fn sweep_leftovers(app: &tauri::AppHandle) {
        use tauri::Manager;
        let Some(root) = reader_dir("x").parent().map(|p| p.to_path_buf()) else { return };
        let Ok(entries) = std::fs::read_dir(&root) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            // A reader still open keeps its directory.
            if name.starts_with(READER_LABEL_PREFIX) && app.get_webview_window(&name).is_none() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    async fn eval_json<R: tauri::Runtime>(win: &WebviewWindow<R>, js: &str, wait: Duration) -> Option<String> {
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let tx = std::sync::Mutex::new(Some(tx));
        win.eval_with_callback(js, move |s| {
            if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
                let _ = tx.send(s);
            }
        })
        .ok()?;
        tokio::time::timeout(wait, rx).await.ok()?.ok()
    }

    async fn drive<R: tauri::Runtime>(win: &WebviewWindow<R>, cancel: &CancellationToken) -> Result<Read, String> {
        let start = Instant::now();
        let mut settle = Settle::default();
        while start.elapsed() < RENDER_DEADLINE {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(CANCELLED.into()),
                _ = tokio::time::sleep(PROBE_EVERY) => {}
            }
            if let Some(p) = eval_json(win, PROBE_SCRIPT, PROBE_WAIT).await.and_then(|s| decode::<Probe>(&s)) {
                if settle.observe(&p) {
                    break;
                }
            }
        }
        if cancel.is_cancelled() {
            return Err(CANCELLED.into());
        }
        // At the deadline, whatever the page shows by then.
        eval_json(win, &read_script(), READ_WAIT)
            .await
            .and_then(|s| decode::<Read>(&s))
            .filter(|r| !r.h.trim().is_empty())
            .ok_or_else(|| "the private reader could not read the page".to_string())
    }

    /// The reader window as every engine gets it: hidden, incognito, and
    /// unable to navigate off the public web, open a window or download.
    /// Public so `examples/reader_window_probe.rs` measures this builder, not
    /// a copy of it.
    pub fn reader_window_builder<'a>(
        app: &'a tauri::AppHandle,
        label: &str,
        url: reqwest::Url,
    ) -> WebviewWindowBuilder<'a, tauri::Wry, tauri::AppHandle> {
        WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
            .title("Jodd — private reader")
            .visible(false)
            .focused(false)
            .skip_taskbar(true)
            .incognito(true)
            .initialization_script(INIT_SCRIPT)
            .on_navigation(|u| {
                let ok = navigation_allowed(u);
                if !ok {
                    crate::log!("private reader: refused a navigation to {}://{}", u.scheme(), u.host_str().unwrap_or(""));
                }
                ok
            })
            .on_new_window(|_, _| NewWindowResponse::Deny)
            .on_download(|_, _| false)
    }

    /// Open `url` in a hidden private reader, wait for it to settle, read its
    /// HTML, and close it. The caller has already checked the URL is a web page.
    pub async fn render(app: &tauri::AppHandle, url: &str, cancel: &CancellationToken) -> Result<RenderedPage, String> {
        let policy = crate::ingest::FetchPolicy::default();
        let parsed = reqwest::Url::parse(url).map_err(|_| "not a valid link".to_string())?;
        if !navigation_allowed(&parsed) {
            return Err("Jodd does not open this address in the private reader".into());
        }
        // §8: the top-level URL passes the same check every fetch does.
        crate::ingest::net::vet(url, policy, cancel).await?;
        sweep_leftovers(app);

        let label = format!("{READER_LABEL_PREFIX}{}", uuid::Uuid::new_v4().simple());
        let dir = reader_dir(&label);
        let builder = reader_window_builder(app, &label, parsed);
        // WebView2 also gets its own user data folder, so the reader shares no
        // browser process or profile with the main window. WebKit has no
        // per-webview directory (incognito is a non-persistent
        // `WKWebsiteDataStore`), and on WebKitGTK incognito is an ephemeral
        // context — where Tauri never drops a context keyed on a directory,
        // so one per read would leak.
        #[cfg(windows)]
        let builder = builder.data_directory(dir.clone());
        let win = builder.build().map_err(|e| format!("could not open the private reader: {e}"))?;
        crate::log!("private reader: opened for {}", crate::ingest::urls::log_form(url));

        let read = drive(&win, cancel).await;
        // `destroy`, not `close`: nothing may keep a reader alive. On macOS
        // the native window used to outlive this anyway — a tao bug, fixed in
        // the vendored fork (Cargo.toml, `[patch.crates-io] tao`); the probe
        // `examples/reader_window_probe.rs` counts what a read leaves behind.
        if let Err(e) = win.destroy() {
            crate::log!("private reader: could not close the reader window: {e}");
        }
        forget_dir(dir);
        let read = read?;

        vet_final(&read.u, policy, cancel).await?;
        crate::log!("private reader: read {} characters", read.h.chars().count());
        Ok(RenderedPage::new(url, &read.u, read.h))
    }
}

#[cfg(desktop)]
pub use window::{reader_window_builder, render};

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(u: &str, r: &str, n: u64, t: u64) -> Probe {
        Probe { u: u.into(), r: r.into(), n, t }
    }

    #[test]
    fn idle_needs_load_and_three_quiet_probes() {
        let mut s = Settle::default();
        assert!(!s.observe(&probe("about:blank", "complete", 0, 0)), "the blank start page is not the page");
        assert!(!s.observe(&probe("https://a.example/", "interactive", 3, 10)));
        assert!(!s.observe(&probe("https://a.example/", "complete", 3, 10)));
        assert!(!s.observe(&probe("https://a.example/", "complete", 3, 10)));
        assert!(!s.observe(&probe("https://a.example/", "complete", 3, 10)));
        // Text still arriving starts the count again.
        assert!(!s.observe(&probe("https://a.example/", "complete", 3, 40)));
        assert!(!s.observe(&probe("https://a.example/", "complete", 3, 40)));
        assert!(!s.observe(&probe("https://a.example/", "complete", 3, 40)));
        assert!(s.observe(&probe("https://a.example/", "complete", 3, 40)));
    }

    #[test]
    fn script_results_decode_from_their_json_string() {
        let raw = serde_json::to_string(r#"{"u":"https://a.example/","r":"complete","n":2,"t":5}"#).unwrap();
        assert_eq!(decode::<Probe>(&raw), Some(probe("https://a.example/", "complete", 2, 5)));
        // An engine that returned nothing, or a bare object, reads as nothing.
        assert_eq!(decode::<Probe>("null"), None);
        assert_eq!(decode::<Probe>(r#"{"u":"x","r":"complete","n":1,"t":1}"#), None);
        assert!(read_script().contains(&MAX_RENDERED_CHARS.to_string()));
    }

    #[test]
    fn navigation_stays_on_public_web_pages() {
        let ok = |s: &str| navigation_allowed(&reqwest::Url::parse(s).unwrap());
        assert!(ok("https://news.example.com/a"));
        assert!(ok("http://93.184.216.34/"));
        assert!(ok("about:blank"));
        for refused in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hi",
            "jodd://capture",
            "about:config",
            "http://localhost:1420/",
            "http://router.local/",
            "http://intranet/",
            "http://ipc.localhost/",
            "http://127.0.0.1/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[fd00::1]/",
        ] {
            assert!(!ok(refused), "{refused} must be refused");
        }
    }

    #[test]
    fn the_cache_keeps_the_newest_page_per_url_and_copies_on_read() {
        // Unique URLs: the cache is process-wide and tests run in parallel.
        let a = "https://render-cache-test.example/a".to_string();
        let b = "https://render-cache-test.example/b".to_string();
        remember(RenderedPage::new(&a, &a, "<p>old</p>".into()));
        remember(RenderedPage::new(&a, &a, "<p>new</p>".into()));
        let got = cached(&[a.clone(), b.clone()]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[&a].html, "<p>new</p>");
        assert_eq!(cached(std::slice::from_ref(&a)).len(), 1, "a read leaves the page for a retry");
    }

    /// Spec §8: "no capability matches the reader window's label or
    /// `remote.urls`". Tauri refuses any command from a remote origin no
    /// capability names — this keeps it so.
    #[test]
    fn no_capability_reaches_the_private_reader() {
        fn glob(p: &str, s: &str) -> bool {
            match (p.chars().next(), s.chars().next()) {
                (None, None) => true,
                (Some('*'), _) => glob(&p[1..], s) || (!s.is_empty() && glob(p, &s[s.chars().next().unwrap().len_utf8()..])),
                (Some('?'), Some(c)) => glob(&p[1..], &s[c.len_utf8()..]),
                (Some(a), Some(b)) if a == b => glob(&p[a.len_utf8()..], &s[b.len_utf8()..]),
                _ => false,
            }
        }
        assert!(glob("*", "x") && glob("jodd-*", "jodd-reader-1") && !glob("main", "jodd-reader-1"));
        let label = format!("{READER_LABEL_PREFIX}0123456789abcdef");
        let check = |where_: &str, cap: &serde_json::Value| {
            assert!(cap.get("remote").is_none(), "{where_}: a capability with `remote` lets web pages call Jodd");
            for key in ["windows", "webviews"] {
                for p in cap.get(key).and_then(|v| v.as_array()).into_iter().flatten() {
                    let p = p.as_str().unwrap_or_default();
                    assert!(!glob(p, &label), "{where_}: `{p}` in `{key}` matches the private reader's label");
                }
            }
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        let mut seen = 0;
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = e.path();
            let name = path.display().to_string();
            match path.extension().and_then(|x| x.to_str()) {
                Some("json") => {
                    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                    check(&name, &v);
                    seen += 1;
                }
                other => panic!("{name}: capability files other than JSON ({other:?}) are not checked yet — extend this test"),
            }
        }
        assert!(seen > 0);
        // Capabilities can also live inline in the config.
        let conf: serde_json::Value = serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        for cap in conf.pointer("/app/security/capabilities").and_then(|v| v.as_array()).into_iter().flatten() {
            if cap.is_object() {
                check("tauri.conf.json", cap);
            }
        }
    }
}
