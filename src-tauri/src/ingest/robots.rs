//! robots.txt for FOLLOWED links only (spec 2026-10-28 D11).
//!
//! A link the user pasted is a browser visit and is fetched as today. A link
//! Jodd chose from a page is a crawl, so the site's robots.txt decides — the
//! RFC 9309 matching rules, with the product token `Jodd`.
//!
//! An unreadable robots.txt (4xx, 5xx, network, too large) allows everything.
//! RFC 9309 asks crawlers to treat a 5xx as "disallow all"; Jodd reads one
//! page the user ticked, not a site, and a flaky server should not silently
//! empty the user's list. The status line names the rule when it does apply.

use reqwest::Url;
use tokio_util::sync::CancellationToken;

use super::net::{read_capped, send_guarded, GuardedRequest};
use super::FetchPolicy;

/// The product token matched against `User-agent:` lines.
pub const AGENT: &str = "jodd";
/// RFC 9309 §2.5: crawlers must parse at least 500 KiB.
const MAX_ROBOTS_BYTES: usize = 500 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    /// `(allow, pattern)` from the group(s) that apply to Jodd.
    rules: Vec<(bool, String)>,
}

impl Rules {
    pub fn allow_all() -> Self {
        Rules::default()
    }

    /// Longest matching pattern wins; on a tie, Allow wins (RFC 9309 §2.2.2).
    pub fn allows(&self, path_and_query: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for (allow, pattern) in &self.rules {
            if pattern_matches(pattern, path_and_query) {
                let len = pattern.len();
                best = match best {
                    Some((l, a)) if l > len || (l == len && a) => Some((l, a)),
                    _ => Some((len, *allow)),
                };
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

/// `*` matches any run of characters; a trailing `$` anchors the end.
/// A rule matches a PREFIX of the path unless it ends in `$`, so an
/// unanchored rule is the same glob with a `*` appended.
fn pattern_matches(pattern: &str, path: &str) -> bool {
    let (body, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let mut glob: Vec<u8> = body.as_bytes().to_vec();
    if !anchored {
        glob.push(b'*');
    }
    glob_matches(&glob, path.as_bytes())
}

/// Whole-string match, `*` = any run of bytes. Linear backtracking on the
/// last `*` — robots.txt is page-served, so no exponential matcher.
fn glob_matches(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while si < s.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// The rules for `agent` (case-insensitive product token), else for `*`.
pub fn parse(txt: &str, agent: &str) -> Rules {
    struct Group {
        agents: Vec<String>,
        rules: Vec<(bool, String)>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut last_was_agent = false;
    for line in txt.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "user-agent" => {
                let token = value.split('/').next().unwrap_or("").trim().to_ascii_lowercase();
                if !last_was_agent || groups.is_empty() {
                    groups.push(Group { agents: Vec::new(), rules: Vec::new() });
                }
                groups.last_mut().unwrap().agents.push(token);
                last_was_agent = true;
            }
            "allow" | "disallow" => {
                last_was_agent = false;
                let Some(g) = groups.last_mut() else { continue };
                // An empty Disallow allows everything: it adds no rule.
                if !value.is_empty() {
                    g.rules.push((key == "allow", value.to_string()));
                }
            }
            _ => {
                last_was_agent = false;
            }
        }
    }
    let agent = agent.to_ascii_lowercase();
    let pick = |want: &str| -> Option<Vec<(bool, String)>> {
        let matched: Vec<&Group> = groups.iter().filter(|g| g.agents.iter().any(|a| a == want)).collect();
        (!matched.is_empty()).then(|| matched.iter().flat_map(|g| g.rules.clone()).collect())
    };
    Rules { rules: pick(&agent).or_else(|| pick("*")).unwrap_or_default() }
}

/// `scheme://host[:port]` — the key robots.txt applies to.
pub fn origin_of(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(p) => format!("{}://{host}:{p}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// The path and query a rule is matched against.
pub fn path_of(url: &Url) -> String {
    match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    }
}

/// Fetch and parse `origin`'s robots.txt through the SSRF guard. Anything but
/// a readable 2xx allows everything (see the module comment).
pub async fn fetch(origin: &str, policy: FetchPolicy, cancel: &CancellationToken) -> Rules {
    let Ok(resp) = send_guarded(GuardedRequest::get(&format!("{origin}/robots.txt")), policy, cancel).await else {
        return Rules::allow_all();
    };
    if !resp.status().is_success() {
        return Rules::allow_all();
    }
    match read_capped(resp, MAX_ROBOTS_BYTES, cancel).await {
        Ok(bytes) => parse(&String::from_utf8_lossy(&bytes), AGENT),
        Err(_) => Rules::allow_all(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jodds_own_group_beats_the_wildcard_and_groups_merge() {
        let txt = "User-agent: *\nDisallow: /\n\nUser-agent: Googlebot\nUser-agent: Jodd/1.0\nDisallow: /private\n\nuser-agent: jodd\nallow: /private/ok\n";
        let r = parse(txt, AGENT);
        assert!(r.allows("/public"));
        assert!(!r.allows("/private/x"));
        assert!(r.allows("/private/ok/page"));
        // Another crawler gets the wildcard group.
        assert!(!parse(txt, "otherbot").allows("/public"));
    }

    #[test]
    fn longest_match_wins_and_allow_wins_a_tie() {
        let r = parse("User-agent: *\nDisallow: /docs\nAllow: /docs/public\nDisallow: /x\nAllow: /x\n", AGENT);
        assert!(!r.allows("/docs/secret"));
        assert!(r.allows("/docs/public/a"));
        assert!(r.allows("/x"));
    }

    #[test]
    fn wildcards_and_end_anchors() {
        let r = parse("User-agent: *\nDisallow: /*.pdf$\nDisallow: /search*q=\nDisallow: /exact$\n", AGENT);
        assert!(!r.allows("/files/a.pdf"));
        assert!(r.allows("/files/a.pdf.html"));
        assert!(!r.allows("/search?q=rust"));
        assert!(r.allows("/search"));
        assert!(!r.allows("/exact"));
        assert!(r.allows("/exact/more"));
    }

    #[test]
    fn empty_disallow_comments_and_no_group_allow_everything() {
        assert!(parse("User-agent: *\nDisallow:\n", AGENT).allows("/anything"));
        assert!(parse("# just a comment\nSitemap: https://a.example/s.xml\n", AGENT).allows("/x"));
        assert!(parse("Disallow: /orphan-rule-before-any-group\n", AGENT).allows("/orphan-rule-before-any-group"));
        assert!(!parse("User-agent: * # all\nDisallow: /a # not a\n", AGENT).allows("/a/b"));
    }

    #[test]
    fn origin_and_path_forms() {
        let u = Url::parse("https://a.example:8443/p/q?x=1#frag").unwrap();
        assert_eq!(origin_of(&u).as_deref(), Some("https://a.example:8443"));
        assert_eq!(path_of(&u), "/p/q?x=1");
    }

    const LOOPBACK: FetchPolicy = FetchPolicy { allow_loopback: true };

    #[tokio::test]
    async fn fetch_reads_2xx_and_allows_all_on_anything_else() {
        let mut ok = mockito::Server::new_async().await;
        let _m = ok.mock("GET", "/robots.txt").with_status(200).with_body("User-agent: *\nDisallow: /no\n").create_async().await;
        let r = fetch(&ok.url(), LOOPBACK, &CancellationToken::new()).await;
        assert!(!r.allows("/no/x") && r.allows("/yes"));
        for status in [404, 500] {
            let mut s = mockito::Server::new_async().await;
            let _m = s.mock("GET", "/robots.txt").with_status(status).with_body("User-agent: *\nDisallow: /\n").create_async().await;
            assert!(fetch(&s.url(), LOOPBACK, &CancellationToken::new()).await.allows("/anything"), "{status}");
        }
    }
}
