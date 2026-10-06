//! Secrets in notes (spec §3.2). Hand-rolled scanning, like the rest of the
//! crate (no regex dependency). A finding carries a KIND and a MASK only:
//! the value never leaves this file.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub kind: &'static str,
    /// `pass: 8H••••••` — the label and at most two characters of the value.
    pub masked: String,
}

/// `(keyword, kind)` — longest first, so `app_secret` wins over `secret`.
const KEYWORDS: &[(&str, &str)] = &[
    ("app_secret", "secret"),
    ("app secret", "secret"),
    ("api_key", "API key"),
    ("api key", "API key"),
    ("api-key", "API key"),
    ("apikey", "API key"),
    ("passcode", "password"),
    ("password", "password"),
    ("passwd", "password"),
    ("secret", "secret"),
    ("token", "token"),
    ("pass", "password"),
    ("pwd", "password"),
    ("pin", "PIN"),
];

const MIN_VALUE: usize = 4;
const MIN_ENTROPY_TOKEN: usize = 24;
const MIN_ENTROPY: f64 = 4.0;

fn mask(label: &str, value: &str) -> String {
    // Two characters of a 6-digit PIN is a third of it: show nothing of
    // anything short (review finding 8).
    let shown: String = if value.chars().count() >= 12 { value.chars().take(2).collect() } else { String::new() };
    format!("{label}: {shown}••••••")
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The value after `keyword` + optional spaces + `:`/`=`, if the keyword is
/// a whole word there.
fn value_after(line: &str, lower: &str, keyword: &str) -> Option<(usize, String)> {
    let mut from = 0;
    while let Some(off) = lower[from..].find(keyword) {
        let start = from + off;
        let end = start + keyword.len();
        let before_ok = lower[..start].chars().last().is_none_or(|c| !is_word_char(c));
        let rest = &line[end..];
        let after = rest.trim_start();
        if before_ok && (after.starts_with(':') || after.starts_with('=')) {
            let value: String = after[1..].trim_start().chars().take_while(|c| !c.is_whitespace()).collect();
            let value = value.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '.')).to_string();
            // A credential has at least one ASCII letter or digit; "ไม่มี" (none)
            // and placeholder words are prose.
            let placeholder = matches!(value.to_lowercase().as_str(), "none" | "true" | "false" | "done" | "null" | "n/a" | "required" | "optional" | "same" | "todo" | "tbd")
                || value.starts_with("http://") || value.starts_with("https://");
            if value.chars().count() >= MIN_VALUE && value.chars().any(|c| c.is_ascii_alphanumeric()) && !placeholder {
                return Some((start, value));
            }
        }
        from = end;
    }
    None
}

/// `kw` as a whole word: "pin" in "pin code", not in "shipping".
fn has_word(lower: &str, kw: &str) -> bool {
    lower.match_indices(kw).any(|(i, _)| {
        let before = lower[..i].chars().last().is_none_or(|c| !is_word_char(c));
        let after = lower[i + kw.len()..].chars().next().is_none_or(|c| !is_word_char(c));
        before && after
    })
}

fn entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let n = s.chars().count() as f64;
    counts.values().map(|&k| { let p = k as f64 / n; -p * p.log2() }).sum()
}

fn key_shape(token: &str) -> Option<&'static str> {
    let t = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
    let tail_ok = |prefix: &str, min: usize, ok: fn(char) -> bool| {
        t.strip_prefix(prefix).is_some_and(|rest| rest.chars().count() >= min && rest.chars().all(ok))
    };
    let alnum = |c: char| c.is_ascii_alphanumeric();
    let alnum_dash = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    if tail_ok("sk-", 20, alnum_dash) {
        return Some("API key");
    }
    if t.len() == 20 && tail_ok("AKIA", 16, |c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        return Some("AWS key");
    }
    if tail_ok("ghp_", 30, alnum) || tail_ok("github_pat_", 30, alnum_dash) {
        return Some("GitHub token");
    }
    if tail_ok("xoxb-", 10, alnum_dash) || tail_ok("xoxp-", 10, alnum_dash) {
        return Some("Slack token");
    }
    None
}

/// Every distinct secret kind found in `text`, one finding per kind.
pub fn scan(text: &str) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let mut push = |f: Finding, out: &mut Vec<Finding>| {
        if !out.iter().any(|o| o.kind == f.kind) {
            out.push(f);
        }
    };
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
        push(Finding { kind: "private key", masked: "-----BEGIN … PRIVATE KEY----- ••••••".into() }, &mut out);
    }
    for line in text.lines() {
        let lower = line.to_lowercase();
        // `to_lowercase` can change byte lengths outside ASCII; only scan
        // keyword positions when the two stay aligned.
        let aligned = lower.len() == line.len();
        let mut keyword_on_line = false;
        if aligned {
            for (kw, kind) in KEYWORDS {
                if let Some((_, value)) = value_after(line, &lower, kw) {
                    push(Finding { kind, masked: mask(kw, &value) }, &mut out);
                    keyword_on_line = true;
                }
            }
            keyword_on_line |= KEYWORDS.iter().any(|(kw, _)| has_word(&lower, kw));
        }
        for token in line.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | '<' | '>' | '=' | ':')) {
            if let Some(kind) = key_shape(token) {
                push(Finding { kind, masked: mask(kind, token) }, &mut out);
            } else if keyword_on_line
                && token.chars().count() >= MIN_ENTROPY_TOKEN
                && token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
                && entropy(token) >= MIN_ENTROPY
            {
                push(Finding { kind: "secret", masked: mask("secret", token) }, &mut out);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values below are synthetic, generated for these tests.
    const FAKE_PASS: &str = "Zq7%Lm2pRt9x";

    fn kinds(t: &str) -> Vec<&'static str> {
        scan(t).into_iter().map(|f| f.kind).collect()
    }

    #[test]
    fn labelled_credentials_are_found() {
        assert_eq!(kinds(&format!("user: 1234 pass: {FAKE_PASS} pin: 482913")), ["password", "PIN"]);
        assert_eq!(kinds("app_secret=Qm9vZm9vYmFyYmF6cXV4Cg+abc"), ["secret"]);
        assert_eq!(kinds("API key = abcd1234efgh"), ["API key"]);
        assert_eq!(kinds("Password:hunter22"), ["password"]);
    }

    #[test]
    fn known_key_shapes_are_found_anywhere() {
        assert_eq!(kinds("export X=sk-abcdefghijklmnopqrstuvwxyz123"), ["API key"]);
        // The AWS id and the PEM header are assembled at runtime: written as
        // one literal, each matches sync-to-public.sh's credential scan and
        // the public mirror refuses to push (the same fix as #112).
        assert_eq!(kinds(&format!("{}{} is the id", "AKIA", "ABCDEFGHIJKLMNOP")), ["AWS key"]);
        assert_eq!(kinds("token ghp_abcdefghijklmnopqrstuvwxyz0123456789"), ["GitHub token"]);
        let pem = ["-----BEGIN RSA ", "PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----"].concat();
        assert_eq!(kinds(&pem), ["private key"]);
    }

    #[test]
    fn a_high_entropy_token_counts_only_beside_a_keyword() {
        let tok = "ANcPqNyjkmMysiM9rCEki9ZeHBgW0xTBV5NlQx";
        assert_eq!(kinds(&format!("secret value {tok}")), ["secret"]);
        assert!(kinds(&format!("video id {tok}")).is_empty());
    }

    #[test]
    fn prose_is_not_a_secret() {
        for t in [
            "Remember to pass the test tomorrow.",
            "The token economy is a design pattern.",
            "PIN the note to the top",
            "pass: no",
            "Spinning: done",
            "ราคา pass: ไม่มี",
            "token: none",
            "password: Required",
            "pin: https://example.com/map",
            "shipping ab12cd34ef56gh78ij90kl12mn34",
        ] {
            assert!(kinds(t).is_empty(), "{t} -> {:?}", kinds(t));
        }
    }

    #[test]
    fn the_mask_never_contains_the_value() {
        for f in scan(&format!("pass: {FAKE_PASS}\npin: 482913\nsk-abcdefghijklmnopqrstuvwxyz123")) {
            assert!(!f.masked.contains(FAKE_PASS) && !f.masked.contains("482913") && !f.masked.contains("abcdefghijklmnop"), "{}", f.masked);
            assert!(f.masked.contains("••••••"));
        }
        assert_eq!(scan("pin: 482913")[0].masked, "pin: ••••••", "nothing of a short value");
        assert_eq!(scan("password: Zq7%Lm2pRt9xAbc")[0].masked, "password: Zq••••••");
    }
}
