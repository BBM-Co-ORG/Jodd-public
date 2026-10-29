//! The SSRF guard every ingest request goes through (spec "Security → SSRF").
//!
//! Links can come from notes other people edit, and fetched text is written
//! into a note that syncs to a cloud account — so an internal fetch is an
//! exfiltration path. Resolve first, refuse any private or local answer, PIN
//! the checked address so the connection cannot be re-resolved elsewhere (DNS
//! rebinding), and re-validate every redirect hop.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use reqwest::{Method, Url};
use tokio_util::sync::CancellationToken;

pub const MAX_REDIRECTS: usize = 5;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
pub const REFUSED: &str = "private or local address";
pub const CANCELLED: &str = "cancelled";
/// How a size-capped read or download says it stopped: `"{TOO_LARGE} N MB"`.
pub const TOO_LARGE: &str = "larger than";
/// A request or a download that went quiet for its whole time limit.
pub const TIMED_OUT: &str = "timed out";

/// A desktop browser's UA for web pages (the spike's). YouTube's InnerTube
/// requests set their own, from `youtube::IOS`.
const BROWSER_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0 Safari/537.36";

#[derive(Debug, Clone, Copy, Default)]
pub struct FetchPolicy {
    /// **Tests only.** mockito listens on loopback. Production never sets
    /// this; `the_default_policy_refuses_the_mock_server` proves the default
    /// refuses that same server.
    pub allow_loopback: bool,
}

impl FetchPolicy {
    pub fn permits(&self, ip: IpAddr) -> bool {
        !is_forbidden(ip) || (self.allow_loopback && ip.is_loopback())
    }
}

pub fn is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => forbidden_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return forbidden_v4(v4);
            }
            let s = v6.segments();
            let embedded = || Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
            // NAT64 well-known prefix 64:ff9b::/96 embeds an IPv4 address.
            if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
                return forbidden_v4(embedded());
            }
            // Deprecated IPv4-compatible form ::a.b.c.d.
            if s[0..6] == [0, 0, 0, 0, 0, 0] && !v6.is_unspecified() && !v6.is_loopback() {
                return forbidden_v4(embedded());
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

fn forbidden_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || o[0] == 0 // 0.0.0.0/8
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT 100.64.0.0/10
}

pub fn check_resolved(addrs: &[SocketAddr], policy: FetchPolicy) -> Result<SocketAddr, String> {
    let first = *addrs.first().ok_or_else(|| "the host did not resolve".to_string())?;
    if addrs.iter().any(|a| !policy.permits(a.ip())) {
        return Err(REFUSED.to_string());
    }
    Ok(first)
}

pub struct GuardedRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Option<Vec<u8>>,
    /// Name/value pairs sent as ONE `Cookie` header (spec D10). They are the
    /// cookies a platform set in an earlier response of the same fetch — never
    /// stored, never the user's. Sent to the request's own host only: a
    /// redirect to another host drops them.
    pub cookies: Vec<(String, String)>,
}

impl GuardedRequest {
    pub fn get(url: &str) -> Self {
        GuardedRequest { method: Method::GET, url: url.to_string(), headers: Vec::new(), body: None, cookies: Vec::new() }
    }
}

/// The `Set-Cookie`s of a response as name/value pairs, attributes dropped —
/// what a follow-up request needs to carry (spec D10). No cookie store exists,
/// so this is the only way a cookie outlives its response.
pub fn set_cookies(resp: &reqwest::Response) -> Vec<(String, String)> {
    resp.headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|line| {
            let (name, value) = line.split(';').next()?.split_once('=')?;
            let name = name.trim();
            (!name.is_empty()).then(|| (name.to_string(), value.trim().to_string()))
        })
        .collect()
}

/// A DNS resolution, injectable so `send_guarded`'s hostname pin
/// (`builder.resolve(host, addr)` below) has a regression test that can
/// prove it matters (finding F4): a fixed test resolver maps a name to a
/// real mockito port, and the test would fail if the pin were ever deleted
/// — deleting it left the rest of the suite green, against this file's own
/// "PIN the checked address" rule. Production always uses `TokioResolver`;
/// tests are the only other implementor.
type ResolveFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>>;

trait Resolver: Sync {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a>;
}

struct TokioResolver;

impl Resolver for TokioResolver {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a> {
        Box::pin(async move { Ok(tokio::net::lookup_host((host, port)).await?.collect()) })
    }
}

/// `None` = the host is an IP literal that passed; `Some((host, addr))` = a
/// name resolved to `addr`, which the client must be pinned to.
async fn vet_with(
    url: &Url,
    policy: FetchPolicy,
    cancel: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Option<(String, SocketAddr)>, String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https links can be fetched".into());
    }
    let host = url.host_str().ok_or_else(|| "the link has no host".to_string())?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return if policy.permits(ip) { Ok(None) } else { Err(REFUSED.into()) };
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs: Vec<SocketAddr> = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(CANCELLED.into()),
        r = resolver.resolve(bare, port) => r.map_err(|_| format!("could not resolve {bare}"))?,
    };
    Ok(Some((bare.to_string(), check_resolved(&addrs, policy)?)))
}

/// The check `send_guarded` makes before each request, on its own: for a
/// client that resolves the name itself and cannot be pinned — the private
/// reader (spec 2026-10-28 §8). Refused, unresolvable or cancelled is `Err`.
pub async fn vet(url: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Result<(), String> {
    let parsed = Url::parse(url).map_err(|_| "not a valid link".to_string())?;
    vet_with(&parsed, policy, cancel, &TokioResolver).await.map(|_| ())
}

/// Send `req`, following up to `MAX_REDIRECTS` redirects for a GET, each hop
/// re-vetted. A non-GET returns its 3xx as-is. No cookie store (reqwest has
/// none unless asked), no system proxy — a proxy resolves the host itself,
/// which would undo the pin.
pub async fn send_guarded(req: GuardedRequest, policy: FetchPolicy, cancel: &CancellationToken) -> Result<reqwest::Response, String> {
    send_guarded_with(req, policy, cancel, &TokioResolver).await
}

/// How long each phase of one request may take. A page fetch is bounded as a
/// whole; a media download cannot be (a 28 MB clip on a phone connection
/// outlasts any fixed total), so it is bounded per phase instead.
#[derive(Clone, Copy)]
struct Timing {
    /// reqwest's whole-request timeout, body included. `None` for a download.
    whole: Option<Duration>,
    /// Longest to wait for the response headers. Only set where `whole` is not,
    /// so a server that accepts the connection and then says nothing still ends.
    headers: Option<Duration>,
}

const PAGE_TIMING: Timing = Timing { whole: Some(REQUEST_TIMEOUT), headers: None };

/// The real body of `send_guarded`, parameterized over the resolver so tests
/// can pin a name to a mockito port without touching real DNS.
async fn send_guarded_with(
    req: GuardedRequest,
    policy: FetchPolicy,
    cancel: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<reqwest::Response, String> {
    send_timed(req, policy, cancel, resolver, PAGE_TIMING).await
}

async fn send_timed(
    req: GuardedRequest,
    policy: FetchPolicy,
    cancel: &CancellationToken,
    resolver: &dyn Resolver,
    timing: Timing,
) -> Result<reqwest::Response, String> {
    let mut url = Url::parse(&req.url).map_err(|_| "not a valid link".to_string())?;
    let cookie_host = url.host_str().map(str::to_ascii_lowercase);
    let mut redirects = 0usize;
    loop {
        let pin = vet_with(&url, policy, cancel, resolver).await?;
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(REQUEST_TIMEOUT)
            .user_agent(BROWSER_UA);
        if let Some(whole) = timing.whole {
            builder = builder.timeout(whole);
        }
        if let Some((host, addr)) = &pin {
            builder = builder.resolve(host, *addr);
        }
        let client = builder.build().map_err(|e| format!("http client: {}", e.without_url()))?;
        let mut rb = client.request(req.method.clone(), url.clone());
        for (k, v) in &req.headers {
            rb = rb.header(*k, v);
        }
        if let Some(body) = &req.body {
            rb = rb.body(body.clone());
        }
        if !req.cookies.is_empty() && url.host_str().map(str::to_ascii_lowercase) == cookie_host {
            let line = req.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
            rb = rb.header(reqwest::header::COOKIE, line);
        }
        let sent = async {
            let r = match timing.headers {
                Some(limit) => tokio::time::timeout(limit, rb.send()).await.map_err(|_| TIMED_OUT.to_string())?,
                None => rb.send().await,
            };
            r.map_err(|e| if e.is_timeout() { TIMED_OUT.to_string() } else { format!("request failed: {}", e.without_url()) })
        };
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(CANCELLED.into()),
            r = sent => r?,
        };
        let location = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).map(str::to_string);
        match location {
            Some(loc) if resp.status().is_redirection() && req.method == Method::GET => {
                redirects += 1;
                if redirects > MAX_REDIRECTS {
                    return Err(format!("more than {MAX_REDIRECTS} redirects"));
                }
                url = url.join(&loc).map_err(|_| "a redirect pointed at an invalid link".to_string())?;
            }
            _ => return Ok(resp),
        }
    }
}

pub async fn read_capped(mut resp: reqwest::Response, max: usize, cancel: &CancellationToken) -> Result<Vec<u8>, String> {
    let too_large = || format!("{TOO_LARGE} {} MB", (max / (1024 * 1024)).max(1));
    if resp.content_length().is_some_and(|n| n as usize > max) {
        return Err(too_large());
    }
    let mut out = Vec::new();
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(CANCELLED.into()),
            c = resp.chunk() => c.map_err(|e| format!("reading the response failed: {}", e.without_url()))?,
        };
        let Some(chunk) = chunk else { break };
        if out.len() + chunk.len() > max {
            return Err(too_large());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// What a media download may cost (spec D7, D11). The download path has no
/// whole-request timeout — only a per-read idle timeout — so the size cap and
/// the host allowlist are what bound it.
pub struct MediaPolicy {
    /// The final URL's host must equal one of these or end with `.` + one.
    pub allowed_host_suffixes: &'static [&'static str],
    pub max_bytes: u64,
    /// Longest to wait for the headers or for the next chunk.
    pub idle_timeout: Duration,
}

/// Dot-anchored: `fbcdn.net` matches `video.fbkk22-2.fna.fbcdn.net` and
/// `fbcdn.net`, never `evilfbcdn.net` or `fbcdn.net.evil.com`.
pub fn host_matches_suffixes(host: &str, suffixes: &[&str]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    suffixes.iter().any(|s| host == *s || host.strip_suffix(*s).is_some_and(|rest| rest.ends_with('.')))
}

/// Stream a media URL into `dest`. Goes through the SSRF guard like every
/// other request; additionally refuses a final URL (after redirects) whose host
/// is outside `media.allowed_host_suffixes`, and anything over `media.max_bytes`
/// — judged from `Content-Length`/`Content-Range` before reading, and again
/// while streaming. `dest` is deleted on any error, so a failed or cancelled
/// download leaves nothing behind. Returns the bytes written.
pub async fn download_to_file(
    req: GuardedRequest,
    media: &MediaPolicy,
    policy: FetchPolicy,
    cancel: &CancellationToken,
    dest: &Path,
) -> Result<u64, String> {
    download_to_file_with(req, media, policy, cancel, dest, &TokioResolver).await
}

async fn download_to_file_with(
    req: GuardedRequest,
    media: &MediaPolicy,
    policy: FetchPolicy,
    cancel: &CancellationToken,
    dest: &Path,
    resolver: &dyn Resolver,
) -> Result<u64, String> {
    let result = stream_to_file(req, media, policy, cancel, dest, resolver).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(dest).await;
    }
    result
}

async fn stream_to_file(
    req: GuardedRequest,
    media: &MediaPolicy,
    policy: FetchPolicy,
    cancel: &CancellationToken,
    dest: &Path,
    resolver: &dyn Resolver,
) -> Result<u64, String> {
    use tokio::io::AsyncWriteExt;

    let too_large = || format!("{TOO_LARGE} {} MB", (media.max_bytes / (1024 * 1024)).max(1));
    let timing = Timing { whole: None, headers: Some(media.idle_timeout) };
    let mut resp = send_timed(req, policy, cancel, resolver, timing).await?;

    // The host the bytes actually come from, not the one the JSON named.
    if !resp.url().host_str().is_some_and(|h| host_matches_suffixes(h, media.allowed_host_suffixes)) {
        return Err("unexpected media host".into());
    }
    if !resp.status().is_success() {
        return Err(format!("the media request returned HTTP {}", resp.status().as_u16()));
    }
    let announced = resp.content_length().into_iter().chain(
        resp.headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|total| total.trim().parse::<u64>().ok()),
    );
    if announced.into_iter().any(|n| n > media.max_bytes) {
        return Err(too_large());
    }

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| format!("could not create the download folder: {e}"))?;
    }
    let mut file = tokio::fs::File::create(dest).await.map_err(|e| format!("could not create the download file: {e}"))?;
    let mut written = 0u64;
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(CANCELLED.into()),
            c = tokio::time::timeout(media.idle_timeout, resp.chunk()) => match c {
                Err(_) => return Err(TIMED_OUT.into()),
                Ok(c) => c.map_err(|e| format!("reading the response failed: {}", e.without_url()))?,
            },
        };
        let Some(chunk) = chunk else { break };
        written += chunk.len() as u64;
        if written > media.max_bytes {
            return Err(too_large());
        }
        file.write_all(&chunk).await.map_err(|e| format!("writing the download failed: {e}"))?;
    }
    file.flush().await.map_err(|e| format!("writing the download failed: {e}"))?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn refuses_every_private_and_local_address_the_spec_lists() {
        for ip in [
            "127.0.0.1", "::1", "10.0.0.1", "172.16.0.1", "192.168.1.1", "169.254.169.254",
            "100.64.0.1", "fc00::1", "fe80::1", "0.0.0.0", "::", "224.0.0.1", "ff02::1",
            "::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:169.254.169.254", "64:ff9b::a00:1",
        ] {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(is_forbidden(parsed), "{ip} must be refused");
            assert!(!FetchPolicy::default().permits(parsed), "{ip} must be refused by the default policy");
        }
    }

    #[test]
    fn allows_a_public_address() {
        for ip in ["93.184.216.34", "2606:2800:220:1:248:1893:25c8:1946", "100.128.0.1", "172.32.0.1"] {
            assert!(!is_forbidden(ip.parse().unwrap()), "{ip} is public");
        }
    }

    #[test]
    fn allow_loopback_opens_loopback_and_nothing_else() {
        let p = FetchPolicy { allow_loopback: true };
        assert!(p.permits(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(p.permits(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!p.permits("10.0.0.1".parse().unwrap()));
    }

    /// One bad answer among several is enough: a DNS response mixing a
    /// public and a private address is the rebinding shape.
    #[test]
    fn a_resolution_with_any_forbidden_address_is_refused() {
        let addrs: Vec<SocketAddr> = vec!["93.184.216.34:443".parse().unwrap(), "10.0.0.1:443".parse().unwrap()];
        assert_eq!(check_resolved(&addrs, FetchPolicy::default()), Err(REFUSED.to_string()));
        assert!(check_resolved(&[], FetchPolicy::default()).is_err());
    }

    #[tokio::test]
    async fn non_http_schemes_are_refused() {
        let err = send_guarded(GuardedRequest::get("file:///etc/passwd"), FetchPolicy::default(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.contains("http"), "{err}");
    }

    /// Every other HTTP test in `ingest` opts into `allow_loopback` to reach
    /// mockito. Without THIS test the suite would pass with the guard deleted.
    #[tokio::test]
    async fn the_default_policy_refuses_the_mock_server() {
        let mut server = mockito::Server::new_async().await;
        let m = server.mock("GET", "/").with_status(200).with_body("hi").expect(0).create_async().await;
        let err = send_guarded(GuardedRequest::get(&server.url()), FetchPolicy::default(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err, REFUSED);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn localhost_by_name_is_refused_after_resolution() {
        let err = send_guarded(GuardedRequest::get("http://localhost:9/"), FetchPolicy::default(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err, REFUSED);
    }

    #[tokio::test]
    async fn a_redirect_whose_second_hop_is_private_is_refused() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/start").with_status(302).with_header("location", "http://10.0.0.1/secret").create_async().await;
        let err = send_guarded(
            GuardedRequest::get(&format!("{}/start", server.url())),
            FetchPolicy { allow_loopback: true },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, REFUSED);
    }

    async fn redirect_chain(server: &mut mockito::ServerGuard, hops: usize) -> Vec<mockito::Mock> {
        let mut mocks = Vec::new();
        for i in 0..hops {
            mocks.push(server.mock("GET", format!("/r{i}").as_str()).with_status(302).with_header("location", &format!("/r{}", i + 1)).create_async().await);
        }
        mocks.push(server.mock("GET", format!("/r{hops}").as_str()).with_status(200).with_body("end").create_async().await);
        mocks
    }

    #[tokio::test]
    async fn five_redirects_are_followed() {
        let mut server = mockito::Server::new_async().await;
        let _m = redirect_chain(&mut server, 5).await;
        let resp = send_guarded(GuardedRequest::get(&format!("{}/r0", server.url())), FetchPolicy { allow_loopback: true }, &CancellationToken::new())
            .await
            .expect("five hops are within the limit");
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn a_sixth_redirect_fails() {
        let mut server = mockito::Server::new_async().await;
        let _m = redirect_chain(&mut server, 6).await;
        let err = send_guarded(GuardedRequest::get(&format!("{}/r0", server.url())), FetchPolicy { allow_loopback: true }, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.contains("redirects"), "{err}");
    }

    #[tokio::test]
    async fn a_body_over_the_cap_fails_with_a_reason() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/big").with_status(200).with_body("x".repeat(2048)).create_async().await;
        let resp = send_guarded(GuardedRequest::get(&format!("{}/big", server.url())), FetchPolicy { allow_loopback: true }, &CancellationToken::new())
            .await
            .unwrap();
        let err = read_capped(resp, 1024, &CancellationToken::new()).await.unwrap_err();
        assert!(err.contains("larger than"), "{err}");
    }

    /// A resolver that answers exactly one hostname with a fixed address and
    /// refuses every other — the SSRF check's own resolution must never leak
    /// through as the connection's actual peer.
    struct FixedResolver {
        host: &'static str,
        addr: SocketAddr,
    }

    impl Resolver for FixedResolver {
        fn resolve<'a>(&'a self, host: &'a str, _port: u16) -> ResolveFuture<'a> {
            let hit = host == self.host;
            let addr = self.addr;
            Box::pin(async move {
                if hit {
                    Ok(vec![addr])
                } else {
                    Err(std::io::Error::new(std::io::ErrorKind::Other, "no route for this test host"))
                }
            })
        }
    }

    /// Finding F4: `builder.resolve(host, addr)` in `send_guarded_with` is
    /// the ONLY defence against DNS rebinding (spec "Security → SSRF") — vet
    /// the name once, then pin the connection to exactly that address so a
    /// second, differently-answering lookup can never substitute another one
    /// mid-request. Before this test, deleting that one line left every other
    /// test in this file green.
    ///
    /// RED evidence (see the final report): commenting out
    /// `builder = builder.resolve(host, *addr);` in `send_guarded_with` makes
    /// this test fail — `rebind.invalid` is not a real hostname, so without
    /// the pin reqwest's own DNS resolution for it fails outright.
    #[tokio::test]
    async fn the_resolved_address_is_pinned() {
        let mut server = mockito::Server::new_async().await;
        let m = server.mock("GET", "/").with_status(200).with_body("ok").create_async().await;
        let addr = server.socket_address();
        let resolver = FixedResolver { host: "rebind.invalid", addr };

        let resp = send_guarded_with(
            GuardedRequest::get(&format!("http://rebind.invalid:{}/", addr.port())),
            FetchPolicy { allow_loopback: true },
            &CancellationToken::new(),
            &resolver,
        )
        .await
        .expect("the pinned address must be used for the actual connection");

        assert_eq!(resp.status(), 200);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn a_cancelled_token_stops_the_request() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/").with_status(200).create_async().await;
        let err = send_guarded(GuardedRequest::get(&server.url()), FetchPolicy { allow_loopback: true }, &cancel).await.unwrap_err();
        assert_eq!(err, CANCELLED);
    }

    /// A closed port: bind, take the port, then drop the listener so nothing
    /// answers. `reqwest::Error`'s `Display` appends `" for url (...)"` unless
    /// `.without_url()` is applied first — a connection-level failure must not
    /// leak the request's URL (query strings, signed tokens) into the reason.
    #[tokio::test]
    async fn a_connection_failure_reason_carries_no_url() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let url = format!("http://127.0.0.1:{port}/path?token=SECRET");
        let err = send_guarded(GuardedRequest::get(&url), FetchPolicy { allow_loopback: true }, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(!err.contains("SECRET"), "{err}");
        assert!(!err.contains("127.0.0.1"), "{err}");
    }

    // ---- per-fetch cookies and capped media downloads (spec D7, D10, D11, §5.3) ----

    /// Answers every hostname with one fixed address, so a test can walk a redirect
    /// across "different" hosts that all land on the same mockito server.
    struct AnyResolver(SocketAddr);

    impl Resolver for AnyResolver {
        fn resolve<'a>(&'a self, _host: &'a str, _port: u16) -> ResolveFuture<'a> {
            let addr = self.0;
            Box::pin(async move { Ok(vec![addr]) })
        }
    }

    const LOOPBACK: FetchPolicy = FetchPolicy { allow_loopback: true };

    fn media(suffixes: &'static [&'static str], max_bytes: u64) -> MediaPolicy {
        MediaPolicy { allowed_host_suffixes: suffixes, max_bytes, idle_timeout: Duration::from_secs(5) }
    }

    #[tokio::test]
    async fn cookies_set_on_the_page_reach_the_media_request() {
        let mut server = mockito::Server::new_async().await;
        let _page = server
            .mock("GET", "/page")
            .with_status(200)
            .with_header("set-cookie", "tt_chain_token=abc123; Path=/; Max-Age=3600; HttpOnly")
            .with_header("set-cookie", "ttwid=xyz; Domain=.tiktok.com; Secure")
            .with_body("<html></html>")
            .create_async()
            .await;
        let video = server
            .mock("GET", "/video")
            .match_header("cookie", "tt_chain_token=abc123; ttwid=xyz")
            .with_status(200)
            .with_body("VIDEO")
            .create_async()
            .await;
        let cancel = CancellationToken::new();

        let resp = send_guarded(GuardedRequest::get(&format!("{}/page", server.url())), LOOPBACK, &cancel).await.unwrap();
        let cookies = set_cookies(&resp);
        assert_eq!(cookies, vec![("tt_chain_token".to_string(), "abc123".to_string()), ("ttwid".to_string(), "xyz".to_string())]);

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clip.mp4");
        let req = GuardedRequest { cookies, ..GuardedRequest::get(&format!("{}/video", server.url())) };
        let n = download_to_file(req, &media(&["127.0.0.1"], 1024), LOOPBACK, &cancel, &dest).await.unwrap();
        assert_eq!(n, 5);
        assert_eq!(std::fs::read(&dest).unwrap(), b"VIDEO");
        video.assert_async().await;
    }

    #[tokio::test]
    async fn cookies_follow_a_same_host_redirect_but_not_a_cross_host_one() {
        let mut server = mockito::Server::new_async().await;
        let addr = server.socket_address();
        let port = addr.port();
        let _a = server
            .mock("GET", "/a")
            .match_header("cookie", "a=1")
            .with_status(302)
            .with_header("location", "/b")
            .create_async()
            .await;
        let b = server
            .mock("GET", "/b")
            .match_header("cookie", "a=1")
            .with_status(302)
            .with_header("location", &format!("http://elsewhere.test:{port}/c"))
            .create_async()
            .await;
        let c = server
            .mock("GET", "/c")
            .match_header("cookie", mockito::Matcher::Missing)
            .with_status(200)
            .with_body("ok")
            .create_async()
            .await;
        let req = GuardedRequest { cookies: vec![("a".into(), "1".into())], ..GuardedRequest::get(&format!("http://start.test:{port}/a")) };
        let resp = send_guarded_with(req, LOOPBACK, &CancellationToken::new(), &AnyResolver(addr)).await.unwrap();
        assert_eq!(resp.status(), 200);
        b.assert_async().await;
        c.assert_async().await;
    }

    #[tokio::test]
    async fn a_media_host_outside_the_allowlist_is_refused() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/v").with_status(200).with_body("VIDEO").create_async().await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clip.mp4");
        let err = download_to_file(
            GuardedRequest::get(&format!("{}/v", server.url())),
            &media(&["fbcdn.net"], 1024),
            LOOPBACK,
            &CancellationToken::new(),
            &dest,
        )
        .await
        .unwrap_err();
        assert!(err.contains("unexpected media host"), "{err}");
        assert!(!dest.exists());
    }

    #[test]
    fn host_suffix_matching_is_dot_anchored() {
        let s = &["fbcdn.net", "tiktokcdn.com"];
        assert!(host_matches_suffixes("video.fbkk22-2.fna.fbcdn.net", s));
        assert!(host_matches_suffixes("fbcdn.net", s));
        assert!(host_matches_suffixes("VIDEO.FBCDN.NET", s));
        assert!(host_matches_suffixes("v16.tiktokcdn.com.", s));
        assert!(!host_matches_suffixes("evilfbcdn.net", s));
        assert!(!host_matches_suffixes("fbcdn.net.evil.com", s));
        assert!(!host_matches_suffixes("net", s));
    }

    #[tokio::test]
    async fn the_allowlist_is_checked_on_the_final_url_after_redirects() {
        let mut server = mockito::Server::new_async().await;
        let _hop = server
            .mock("GET", "/start")
            .with_status(302)
            .with_header("location", &format!("http://cdn.allowed.test:{}/v", server.socket_address().port()))
            .create_async()
            .await;
        let _v = server.mock("GET", "/v").with_status(200).with_body("VIDEO").create_async().await;
        let addr = server.socket_address();
        let resolver = AnyResolver(addr);
        let dir = tempfile::tempdir().unwrap();

        // Starts on a host that is not allowed, ends on one that is: accepted.
        let ok = dir.path().join("ok.mp4");
        let n = download_to_file_with(
            GuardedRequest::get(&format!("http://front.other.test:{}/start", addr.port())),
            &media(&["allowed.test"], 1024),
            LOOPBACK,
            &CancellationToken::new(),
            &ok,
            &resolver,
        )
        .await
        .unwrap();
        assert_eq!(n, 5);

        // Starts on an allowed host, ends on one that is not: refused, no file.
        let bad = dir.path().join("bad.mp4");
        let err = download_to_file_with(
            GuardedRequest::get(&format!("http://front.other.test:{}/start", addr.port())),
            &media(&["fbcdn.net"], 1024),
            LOOPBACK,
            &CancellationToken::new(),
            &bad,
            &resolver,
        )
        .await
        .unwrap_err();
        assert!(err.contains("unexpected media host"), "{err}");
        assert!(!bad.exists());
    }

    #[tokio::test]
    async fn download_stops_at_max_bytes_and_leaves_no_file() {
        let mut server = mockito::Server::new_async().await;
        // Chunked, so there is no Content-Length to refuse it up front: the cap
        // has to bite while streaming.
        let _m = server
            .mock("GET", "/big")
            .with_status(200)
            .with_chunked_body(|w| {
                for _ in 0..20 {
                    w.write_all(&[7u8; 100])?;
                    w.flush()?;
                }
                Ok(())
            })
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clip.mp4");
        let err = download_to_file(
            GuardedRequest::get(&format!("{}/big", server.url())),
            &media(&["127.0.0.1"], 1000),
            LOOPBACK,
            &CancellationToken::new(),
            &dest,
        )
        .await
        .unwrap_err();
        assert!(err.contains("larger than"), "{err}");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn content_length_over_the_cap_is_refused_before_reading() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/big").with_status(200).with_body(vec![1u8; 4096]).create_async().await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clip.mp4");
        let err = download_to_file(
            GuardedRequest::get(&format!("{}/big", server.url())),
            &media(&["127.0.0.1"], 1000),
            LOOPBACK,
            &CancellationToken::new(),
            &dest,
        )
        .await
        .unwrap_err();
        assert!(err.contains("larger than"), "{err}");
        assert!(!dest.exists(), "nothing may be written for a response that announced its size");
    }

    #[tokio::test]
    async fn a_content_range_total_over_the_cap_is_refused() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/part")
            .with_status(206)
            .with_header("content-range", "bytes 0-9/999999999")
            .with_body("0123456789")
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clip.mp4");
        let err = download_to_file(
            GuardedRequest::get(&format!("{}/part", server.url())),
            &media(&["127.0.0.1"], 1000),
            LOOPBACK,
            &CancellationToken::new(),
            &dest,
        )
        .await
        .unwrap_err();
        assert!(err.contains("larger than"), "{err}");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn a_failing_status_is_an_error_with_no_file() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("GET", "/gone").with_status(403).with_body("<html>login</html>").create_async().await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clip.mp4");
        let err = download_to_file(
            GuardedRequest::get(&format!("{}/gone", server.url())),
            &media(&["127.0.0.1"], 1024),
            LOOPBACK,
            &CancellationToken::new(),
            &dest,
        )
        .await
        .unwrap_err();
        assert!(err.contains("403"), "{err}");
        assert!(!dest.exists());
    }

    /// A one-request HTTP server on a bare listener: a chunked 200 whose
    /// chunks go out `gap` apart, then the terminating chunk when `finish`,
    /// else nothing more (a stall). Not mockito: a mockito body that sleeps
    /// keeps its pooled server busy after the guard drops, and stalls the
    /// next test that is handed it. Abort the handle when done.
    async fn chunked_server(chunks: Vec<Vec<u8>>, gap: Duration, finish: bool) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n").await;
            for chunk in chunks {
                let _ = sock.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await;
                let _ = sock.write_all(&chunk).await;
                let _ = sock.write_all(b"\r\n").await;
                let _ = sock.flush().await;
                tokio::time::sleep(gap).await;
            }
            if finish {
                let _ = sock.write_all(b"0\r\n\r\n").await;
                let _ = sock.flush().await;
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        (format!("http://{addr}/clip"), task)
    }

    #[tokio::test]
    async fn a_stalled_download_fails_after_the_idle_timeout() {
        let (url, server) = chunked_server(vec![vec![1u8; 64]], Duration::ZERO, false).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clip.mp4");
        let policy = MediaPolicy { allowed_host_suffixes: &["127.0.0.1"], max_bytes: 1024 * 1024, idle_timeout: Duration::from_secs(1) };
        let started = std::time::Instant::now();
        let err = download_to_file(GuardedRequest::get(&url), &policy, LOOPBACK, &CancellationToken::new(), &dest).await.unwrap_err();
        server.abort();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3), "the idle timeout, not the stall, ended it");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn a_steady_download_longer_than_the_idle_timeout_completes() {
        let (url, server) = chunked_server(vec![vec![2u8; 10]; 7], Duration::from_millis(300), true).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clip.mp4");
        let policy = MediaPolicy { allowed_host_suffixes: &["127.0.0.1"], max_bytes: 1024 * 1024, idle_timeout: Duration::from_secs(1) };
        let started = std::time::Instant::now();
        let n = download_to_file(GuardedRequest::get(&url), &policy, LOOPBACK, &CancellationToken::new(), &dest).await.unwrap();
        server.abort();
        assert!(started.elapsed() > Duration::from_secs(1), "the transfer must outlast the idle timeout for this to prove anything");
        assert_eq!(n, 70);
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), 70);
    }

    #[tokio::test]
    async fn cancelling_mid_download_deletes_the_file() {
        let (url, server) = chunked_server(vec![vec![1u8; 64]], Duration::ZERO, false).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clip.mp4");
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            c2.cancel();
        });
        let err = download_to_file(GuardedRequest::get(&url), &media(&["127.0.0.1"], 1024), LOOPBACK, &cancel, &dest).await.unwrap_err();
        server.abort();
        assert_eq!(err, CANCELLED);
        assert!(!dest.exists());
    }
}
