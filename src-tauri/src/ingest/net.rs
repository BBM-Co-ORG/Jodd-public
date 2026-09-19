//! The SSRF guard every ingest request goes through (spec "Security → SSRF").
//!
//! Links can come from notes other people edit, and fetched text is written
//! into a note that syncs to a cloud account — so an internal fetch is an
//! exfiltration path. Resolve first, refuse any private or local answer, PIN
//! the checked address so the connection cannot be re-resolved elsewhere (DNS
//! rebinding), and re-validate every redirect hop.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use reqwest::{Method, Url};
use tokio_util::sync::CancellationToken;

pub const MAX_REDIRECTS: usize = 5;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
pub const REFUSED: &str = "private or local address";
pub const CANCELLED: &str = "cancelled";

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
}

impl GuardedRequest {
    pub fn get(url: &str) -> Self {
        GuardedRequest { method: Method::GET, url: url.to_string(), headers: Vec::new(), body: None }
    }
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

/// Send `req`, following up to `MAX_REDIRECTS` redirects for a GET, each hop
/// re-vetted. A non-GET returns its 3xx as-is. No cookie store (reqwest has
/// none unless asked), no system proxy — a proxy resolves the host itself,
/// which would undo the pin.
pub async fn send_guarded(req: GuardedRequest, policy: FetchPolicy, cancel: &CancellationToken) -> Result<reqwest::Response, String> {
    send_guarded_with(req, policy, cancel, &TokioResolver).await
}

/// The real body of `send_guarded`, parameterized over the resolver so tests
/// can pin a name to a mockito port without touching real DNS.
async fn send_guarded_with(
    req: GuardedRequest,
    policy: FetchPolicy,
    cancel: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<reqwest::Response, String> {
    let mut url = Url::parse(&req.url).map_err(|_| "not a valid link".to_string())?;
    let mut redirects = 0usize;
    loop {
        let pin = vet_with(&url, policy, cancel, resolver).await?;
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(BROWSER_UA);
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
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(CANCELLED.into()),
            r = rb.send() => r.map_err(|e| if e.is_timeout() { "timed out".to_string() } else { format!("request failed: {}", e.without_url()) })?,
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
    let too_large = || format!("larger than {} MB", (max / (1024 * 1024)).max(1));
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
}
