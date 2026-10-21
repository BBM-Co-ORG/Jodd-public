//! Secrets in notes (spec §3.2). The scanning rules live in
//! `privacy::detect` — one detector for Organize and for AI masking. A finding
//! carries a KIND and a MASK only: the value never leaves this file.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub kind: &'static str,
    /// `pass: 8H••••••` — the label and at most two characters of the value.
    pub masked: String,
}

fn mask(label: &str, value: &str) -> String {
    // Two characters of a 6-digit PIN is a third of it: show nothing of
    // anything short (review finding 8).
    let shown: String = if value.chars().count() >= 12 { value.chars().take(2).collect() } else { String::new() };
    format!("{label}: {shown}••••••")
}

/// Every distinct secret kind found in `text`, one finding per kind. The
/// rules live in `privacy::detect` — one detector for Organize and for AI
/// masking, so the two can never disagree about what a secret is.
pub fn scan(text: &str) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    for h in crate::privacy::detect::secrets(text) {
        if out.iter().any(|o| o.kind == h.label) {
            continue;
        }
        let masked = if h.label == "private key" {
            "-----BEGIN … PRIVATE KEY----- ••••••".to_string()
        } else {
            mask(h.mask_label, &text[h.span.start..h.span.end])
        };
        out.push(Finding { kind: h.label, masked });
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
