//! Sensitive spans in text — the ONE detector for secrets and structured PII
//! (spec 2026-10-08 §4.1). Hand-rolled like the rest of the crate: no regex.
//!
//! `secrets` keeps the exact emission order the Organize scanner has always
//! had (`curate::secrets::scan` is now a wrapper over it); `detect` adds PII
//! and resolves overlaps for the masking vault.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Password,
    Pin,
    ApiKey,
    Secret,
    PrivateKey,
    Email,
    Phone,
    ThaiId,
    Card,
    BankAccount,
    Passport,
}

impl Kind {
    pub const ALL: [Kind; 11] = [
        Kind::Password,
        Kind::Pin,
        Kind::ApiKey,
        Kind::Secret,
        Kind::PrivateKey,
        Kind::Email,
        Kind::Phone,
        Kind::ThaiId,
        Kind::Card,
        Kind::BankAccount,
        Kind::Passport,
    ];

    /// The KIND in `⟦KIND_N⟧`.
    pub fn token_name(self) -> &'static str {
        match self {
            Kind::Password => "PASSWORD",
            Kind::Pin => "PIN",
            Kind::ApiKey => "API_KEY",
            Kind::Secret => "SECRET",
            Kind::PrivateKey => "PRIVATE_KEY",
            Kind::Email => "EMAIL",
            Kind::Phone => "PHONE",
            Kind::ThaiId => "THAI_ID",
            Kind::Card => "CARD",
            Kind::BankAccount => "BANK_ACCOUNT",
            Kind::Passport => "PASSPORT",
        }
    }

    /// Receipt metric name — static, content-free. Its value is the number of
    /// distinct values masked PER PROVIDER CALL, summed over the step's calls:
    /// Ask's two calls count one address twice (final review M3).
    pub fn metric_name(self) -> &'static str {
        match self {
            Kind::Password => "masked_password",
            Kind::Pin => "masked_pin",
            Kind::ApiKey => "masked_api_key",
            Kind::Secret => "masked_secret",
            Kind::PrivateKey => "masked_private_key",
            Kind::Email => "masked_email",
            Kind::Phone => "masked_phone",
            Kind::ThaiId => "masked_thai_id",
            Kind::Card => "masked_card",
            Kind::BankAccount => "masked_bank_account",
            Kind::Passport => "masked_passport",
        }
    }

    pub fn is_secret(self) -> bool {
        matches!(self, Kind::Password | Kind::Pin | Kind::ApiKey | Kind::Secret | Kind::PrivateKey)
    }
}

/// Byte offsets into the scanned text; always on char boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
}

/// A secret as Organize reports it: `label` is its finding kind ("password",
/// "AWS key"), `mask_label` the word its mask is shown under ("pass").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretHit {
    pub span: Span,
    pub label: &'static str,
    pub mask_label: &'static str,
}

/// `(keyword, label)` — longest first, so `app_secret` wins over `secret`.
/// An ASCII keyword needs an ASCII boundary before it (so `DB_PASSWORD` and
/// `ใส่password` count, `shipping` does not); a Thai keyword cannot have one —
/// Thai is written without spaces — so it only needs the separator after it.
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
    ("pw", "password"),
    ("pin", "PIN"),
    ("รหัสผ่าน", "password"),
    ("พาสเวิร์ด", "password"),
    ("รหัส", "password"),
    ("พิน", "PIN"),
];

const BANK_KEYWORDS: &[&str] = &[
    "บัญชี",
    "account",
    "a/c",
    "acct",
    "ธนาคาร",
    "bank",
    "kbank",
    "scb",
    "ktb",
    "bbl",
    "ttb",
    "krungsri",
    "กสิกร",
    "กรุงเทพ",
    "กรุงไทย",
    "ไทยพาณิชย์",
    "กรุงศรี",
    "พร้อมเพย์",
    "promptpay",
];
const PASSPORT_KEYWORDS: &[&str] = &["passport", "หนังสือเดินทาง", "พาสปอร์ต"];

const MIN_VALUE: usize = 4;
const MIN_ENTROPY_TOKEN: usize = 24;
const MIN_ENTROPY: f64 = 4.0;
const MIN_BEARER: usize = 20;
/// No card number has more digits than this, so no classifiable run has more
/// groups: it bounds the window tried at each position.
const MAX_DIGITS: usize = 19;

fn kind_of(label: &str) -> Kind {
    match label {
        "password" => Kind::Password,
        "PIN" => Kind::Pin,
        "API key" | "AWS key" | "GitHub token" | "Slack token" => Kind::ApiKey,
        "private key" => Kind::PrivateKey,
        _ => Kind::Secret,
    }
}

/// `(offset, line)` for every line, without its `\n` / `\r\n`.
fn lines_with_offsets(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut off = 0;
    for l in text.split_inclusive('\n') {
        let start = off;
        off += l.len();
        let l = l.strip_suffix('\n').unwrap_or(l);
        let l = l.strip_suffix('\r').unwrap_or(l);
        out.push((start, l));
    }
    out
}

fn is_placeholder(value: &str) -> bool {
    matches!(
        value.to_lowercase().as_str(),
        "none" | "true" | "false" | "done" | "null" | "n/a" | "required" | "optional" | "same" | "todo" | "tbd"
    ) || value.starts_with("http://")
        || value.starts_with("https://")
}

/// A credential has at least one ASCII letter or digit; "ไม่มี" (none) and
/// placeholder words are prose, and so is a lone HTML tag (`</b>`) — the
/// markup between a label and its value, never the value.
fn plausible(value: &str) -> bool {
    value.chars().count() >= MIN_VALUE
        && value.chars().any(|c| c.is_ascii_alphanumeric())
        && !is_placeholder(value)
        && !only_tags(value)
}

/// `value` is nothing but HTML tags (`</b>`, `</b><wbr>`, `<o:p></o:p>`).
fn only_tags(value: &str) -> bool {
    let mut rest = value;
    while let Some(n) = html_tag_len(rest) {
        rest = &rest[n..];
    }
    rest.is_empty() && !value.is_empty()
}

/// Element names that make a value "only markup" for `plausible`. A closed
/// list on purpose: in plain text `<hunter22x>` may be the value itself, and a
/// miss leaks. (Blanking for the view is generic: `markup_tag_len`.)
const HTML_TAGS: &[&str] = &[
    "colgroup", "nobr", "wbr",
    "a", "abbr", "article", "b", "big", "blockquote", "body", "br", "caption", "center", "cite", "code", "col",
    "dd", "del", "dfn", "div", "dl", "dt", "em", "figcaption", "figure", "font", "footer", "h1", "h2", "h3", "h4",
    "h5", "h6", "head", "header", "hr", "html", "i", "img", "input", "ins", "kbd", "label", "li", "main", "mark",
    "meta", "nav", "object", "ol", "p", "pre", "q", "s", "samp", "section", "small", "span", "strike", "strong",
    "style", "sub", "sup", "table", "tbody", "td", "tfoot", "th", "thead", "title", "tr", "tt", "u", "ul", "var",
];

/// The byte length of the HTML tag `s` starts with (`<b>`, `</span>`,
/// `<br/>`, `<span class="x">`, Word's namespaced `<o:p>`), if it starts with
/// one on a single line and names a listed element (or any namespaced one).
fn html_tag_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.first() != Some(&b'<') {
        return None;
    }
    let mut i = 1 + usize::from(b.get(1) == Some(&b'/'));
    let n0 = i;
    while b.get(i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b':') {
        i += 1;
    }
    let name = s[n0..i].to_ascii_lowercase();
    let namespaced = name.split_once(':').is_some_and(|(p, l)| !p.is_empty() && !l.is_empty());
    if !(namespaced || HTML_TAGS.contains(&name.as_str())) {
        return None;
    }
    match b.get(i)? {
        b'>' => return Some(i + 1),
        b' ' | b'\t' | b'/' => {}
        _ => return None,
    }
    let close = i + s[i..].find(['>', '<', '\n'])?;
    (b[close] == b'>').then_some(close + 1)
}

/// The byte length of the markup `s` starts with, for blanking: a `<` followed
/// by an ASCII letter, `/`, `!` or `?`, through the next `>` on its line. A
/// `<` followed by anything else (`a < b`, `x<5`, `<=`) is text. Stopping at
/// the next `<` keeps a line full of unclosed `<x` linear.
fn markup_tag_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.first() != Some(&b'<') || !b.get(1).is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'/' | b'!' | b'?')) {
        return None;
    }
    let close = 1 + s[1..].find(['>', '<', '\n'])?;
    (b[close] == b'>').then_some(close + 1)
}

/// `text` with every HTML tag and non-breaking-space entity replaced by ASCII
/// spaces of the same byte length (spec §9b, final review C1). Extract sends
/// raw `body_html`, so `<b>Password:</b>&nbsp;x` must read as `Password: x` to
/// the keyword rules — while every offset still maps 1:1 back to `text`.
fn markup_blanked(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(['<', '&']) {
        return std::borrow::Cow::Borrowed(text);
    }
    const NBSP: [&[u8]; 3] = [b"&nbsp;", b"&#160;", b"&#xa0;"];
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let blank = match rest.as_bytes()[0] {
            b'<' => markup_tag_len(rest),
            b'&' => NBSP
                .iter()
                .find(|e| rest.len() >= e.len() && rest.as_bytes()[..e.len()].eq_ignore_ascii_case(e))
                .map(|e| e.len()),
            _ => None,
        };
        match blank {
            Some(n) => {
                out.push_str(&" ".repeat(n));
                i += n;
            }
            None => {
                let c = rest.chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Where the value starts when a keyword ending at `end` is followed by a
/// separator: an optional closing quote, then `:`, `=`, `=>`, the Thai `คือ`,
/// or the word `is`. `lower` is `line` lowercased and byte-aligned with it.
fn value_start(line: &str, lower: &str, end: usize) -> Option<usize> {
    let b = line.as_bytes();
    let skip = |i: usize| line.len() - line[i..].trim_start().len();
    // Markdown emphasis around the label: `**Password**: x`, `**Password:** x`.
    let emphasis = |i: usize| i + b[i..].iter().take_while(|c| matches!(c, b'*' | b'_')).count();
    let mut i = skip(emphasis(end));
    if matches!(b.get(i), Some(b'"' | b'\'')) {
        i = skip(i + 1);
    }
    let after_sep = match b.get(i)? {
        b':' => i + 1,
        b'=' if b.get(i + 1) == Some(&b'>') => i + 2,
        b'=' => i + 1,
        _ if lower[i..].starts_with("คือ") => i + "คือ".len(),
        _ if line[..i].chars().next_back().is_some_and(char::is_whitespace)
            && lower[i..].starts_with("is")
            && line[i + 2..].chars().next().is_some_and(char::is_whitespace) =>
        {
            i + 2
        }
        _ => return None,
    };
    let v = skip(after_sep);
    // A run closing the label's emphasis is followed by a space; one glued to
    // the value (`password: _secret_`) is part of it.
    let e = emphasis(v);
    let closes_label = e > v && line[e..].chars().next().is_none_or(char::is_whitespace);
    Some(if closes_label { skip(e) } else { v })
}

/// Where a connector word ends, if one follows a PIN/password keyword ending
/// at `end`: `PIN code`, `pin-no.`, `password number`, `PIN #`. Words need a
/// space, `-` or `_` before them and a word boundary after (final review I1).
fn connector_end(lower: &str, keyword: &str, end: usize) -> Option<usize> {
    if !matches!(keyword, "pin" | "password" | "passcode") {
        return None;
    }
    let b = lower.as_bytes();
    let joined = matches!(b.get(end), Some(b' ' | b'-' | b'_'));
    let at = end + usize::from(joined);
    if b.get(at) == Some(&b'#') {
        return Some(at + 1);
    }
    if !joined {
        return None;
    }
    ["number", "code", "no.", "no"].iter().find_map(|w| {
        let e = at + w.len();
        (lower[at..].starts_with(w) && b.get(e).is_none_or(|c| !c.is_ascii_alphanumeric())).then_some(e)
    })
}

/// Every value after `keyword` and a separator, as offsets into `line`. A
/// quoted value runs to the closing quote (a passphrase may hold spaces);
/// anything else runs to the next whitespace.
///
/// `view` is `line` with markup blanked (`markup_blanked`) and `lower` is
/// `view` lowercased; both are byte-aligned with `line`. Values are read in
/// the view — unless the view cuts a bare value too short to be one
/// (`a<b>&c99` → `a`), when it is read from `line` as written.
fn values_after(line: &str, view: &str, lower: &str, keyword: &str) -> Vec<(usize, usize)> {
    let bounded = keyword.is_ascii();
    let lb = lower.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    let bare = |rest: &str| {
        let raw_len: usize = rest.chars().take_while(|c| !c.is_whitespace()).map(char::len_utf8).sum();
        let raw = &rest[..raw_len];
        let trim = |c: char| matches!(c, '"' | '\'' | ',' | ';' | '.');
        let lead = raw.len() - raw.trim_start_matches(trim).len();
        let value = raw.trim_matches(trim);
        plausible(value).then_some((lead, lead + value.len()))
    };
    while let Some(off) = lower[from..].find(keyword) {
        let start = from + off;
        let end = start + keyword.len();
        from = end;
        if bounded && start > 0 && lb[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let Some(v0) = connector_end(lower, keyword, end)
            .and_then(|c| value_start(view, lower, c))
            .or_else(|| value_start(view, lower, end))
        else {
            continue;
        };
        let rest = &view[v0..];
        if let Some(q) = rest.chars().next().filter(|c| matches!(c, '"' | '\'')) {
            if let Some(close) = rest[1..].find(q) {
                if plausible(&rest[1..1 + close]) {
                    out.push((v0 + 1, v0 + 1 + close));
                }
                continue;
            }
        }
        if let Some((a, b)) = bare(rest).or_else(|| bare(&line[v0..])) {
            out.push((v0 + a, v0 + b));
        }
    }
    out
}

/// `kw` as a whole word: "pin" in "pin code", not in "shipping".
fn has_word(lower: &str, kw: &str) -> bool {
    let b = lower.as_bytes();
    lower.match_indices(kw).any(|(i, _)| {
        let before = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let after = b.get(i + kw.len()).is_none_or(|c| !c.is_ascii_alphanumeric());
        before && after
    })
}

fn entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let n = s.chars().count() as f64;
    counts.values().map(|&k| {
        let p = k as f64 / n;
        -p * p.log2()
    }).sum()
}

/// Three base64url parts, the first two starting `eyJ` (a JSON object).
fn is_jwt(t: &str) -> bool {
    let parts: Vec<&str> = t.split('.').collect();
    t.len() >= 40
        && parts.len() == 3
        && parts[0].starts_with("eyJ")
        && parts[1].starts_with("eyJ")
        && !parts[2].is_empty()
        && parts.iter().all(|p| p.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'))
}

/// `(label, start, end)` of a known key shape inside `token`.
fn key_shape(token: &str) -> Option<(&'static str, usize, usize)> {
    let keep = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let lead = token.len() - token.trim_start_matches(|c: char| !keep(c)).len();
    let t = token.trim_matches(|c: char| !keep(c));
    let tail_ok = |prefix: &str, min: usize, ok: fn(char) -> bool| {
        t.strip_prefix(prefix).is_some_and(|rest| rest.chars().count() >= min && rest.chars().all(ok))
    };
    let alnum = |c: char| c.is_ascii_alphanumeric();
    let alnum_dash = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let label = if tail_ok("sk-", 20, alnum_dash)
        || tail_ok("sk_live_", 16, alnum)
        || tail_ok("sk_test_", 16, alnum)
        || tail_ok("rk_live_", 16, alnum)
        || (t.len() == 39 && tail_ok("AIza", 35, alnum_dash))
    {
        "API key"
    } else if t.len() == 20 && tail_ok("AKIA", 16, |c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        "AWS key"
    } else if ["ghp_", "gho_", "ghs_", "ghu_", "ghr_"].iter().any(|p| tail_ok(p, 30, alnum))
        || tail_ok("github_pat_", 30, alnum_dash)
    {
        "GitHub token"
    } else if tail_ok("xoxb-", 10, alnum_dash) || tail_ok("xoxp-", 10, alnum_dash) {
        "Slack token"
    } else if tail_ok("GOCSPX-", 20, alnum_dash) {
        "secret"
    } else if is_jwt(t) {
        "token"
    } else {
        return None;
    };
    Some((label, lead, lead + t.len()))
}

/// `Bearer <token>` or `Bearer: <token>`: the token, as offsets into `line`.
fn bearer_tokens(line: &str, lower: &str) -> Vec<(usize, usize)> {
    let lb = lower.as_bytes();
    let mut out = Vec::new();
    for (i, _) in lower.match_indices("bearer") {
        if i > 0 && lb[i - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut s0 = i + "bearer".len();
        let colon = line.as_bytes().get(s0) == Some(&b':');
        s0 += usize::from(colon);
        let after = &line[s0..];
        let t = after.trim_start();
        if t.len() == after.len() && !colon {
            continue;
        }
        let len = t.bytes().take_while(|c| c.is_ascii_alphanumeric() || b"._~+/=-".contains(c)).count();
        let len = t[..len].trim_end_matches('.').len();
        if len >= MIN_BEARER {
            let s = s0 + (after.len() - t.len());
            out.push((s, s + len));
        }
    }
    out
}

/// Tokens split the way the Organize scanner always split them, with offsets.
fn tokens(line: &str) -> Vec<(usize, &str)> {
    let is_sep = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | '<' | '>' | '=' | ':');
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        if is_sep(c) {
            if let Some(s) = start.take() {
                out.push((s, &line[s..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push((s, &line[s..]));
    }
    out
}

/// Whole PEM blocks — and armored PGP blocks (`PRIVATE KEY BLOCK-----`) —
/// whose header line names a PRIVATE KEY. An unterminated block runs to the
/// end of the text: a miss leaks, a long mask does not.
fn private_keys(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(off) = text[from..].find("-----BEGIN") {
        let start = from + off;
        let header_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
        if !text[start..header_end].contains("PRIVATE KEY") {
            // Every later `-----BEGIN` on this line has a suffix of this
            // header as its own, so none has the marker either: skip the
            // line, or one long line of headers is O(k·n) (final review M2).
            from = header_end;
            continue;
        }
        let end = match text[header_end..].find("-----END") {
            Some(i) => {
                let e0 = header_end + i + "-----END".len();
                text[e0..].find("-----").map_or(text.len(), |j| e0 + j + "-----".len())
            }
            None => text.len(),
        };
        out.push((start, end));
        from = end;
    }
    out
}

/// Every secret, in the order the Organize scanner has always reported them:
/// private keys, then line by line — keyword values in `KEYWORDS` order, then
/// key shapes and high-entropy tokens, then bearer tokens. May overlap;
/// `detect` resolves that.
pub fn secrets(text: &str) -> Vec<SecretHit> {
    let mut out: Vec<SecretHit> = private_keys(text)
        .into_iter()
        .map(|(start, end)| SecretHit {
            span: Span { kind: Kind::PrivateKey, start, end },
            label: "private key",
            mask_label: "private key",
        })
        .collect();
    // Keyword values are read through the markup-blanked view (same byte
    // length, so its offsets are `text`'s); shapes and tokens use the text.
    let view = markup_blanked(text);
    for ((off, line), (_, vline)) in lines_with_offsets(text).into_iter().zip(lines_with_offsets(&view)) {
        // ASCII-only lowering keeps every byte offset aligned with `line`;
        // the Unicode one changes lengths (U+0130) and used to skip the line.
        let lower = line.to_ascii_lowercase();
        let vlower = vline.to_ascii_lowercase();
        let mut keyword_on_line = false;
        // Read keyword values in BOTH the view and the text as written, and
        // keep every hit: the view finds `<b>Password:</b> x`, the text finds
        // `href="…?password=x"` (blanked with its tag) and `Pass<a>1234` whole.
        // Overlaps merge in `detect`; a lone tag is never plausible.
        let marked = vline != line;
        for &(kw, label) in KEYWORDS {
            let mut hits = values_after(line, vline, &vlower, kw);
            if marked {
                // Sort + dedup, not `contains` per hit: one long marked-up
                // line can hold tens of thousands of hits (round 3, F2).
                hits.extend(values_after(line, line, &lower, kw));
                hits.sort_unstable();
                hits.dedup();
            }
            for (s, e) in hits {
                out.push(SecretHit { span: Span { kind: kind_of(label), start: off + s, end: off + e }, label, mask_label: kw });
                keyword_on_line = true;
            }
        }
        keyword_on_line |= KEYWORDS
            .iter()
            .any(|(kw, _)| if kw.is_ascii() { has_word(&lower, kw) } else { lower.contains(kw) });
        for (ts, token) in tokens(line) {
            if let Some((label, a, b)) = key_shape(token) {
                out.push(SecretHit { span: Span { kind: kind_of(label), start: off + ts + a, end: off + ts + b }, label, mask_label: label });
            } else if keyword_on_line
                && token.chars().count() >= MIN_ENTROPY_TOKEN
                && token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
                && entropy(token) >= MIN_ENTROPY
            {
                out.push(SecretHit {
                    span: Span { kind: Kind::Secret, start: off + ts, end: off + ts + token.len() },
                    label: "secret",
                    mask_label: "secret",
                });
            }
        }
        for (s, e) in bearer_tokens(line, &lower) {
            out.push(SecretHit {
                span: Span { kind: Kind::Secret, start: off + s, end: off + e },
                label: "token",
                mask_label: "bearer",
            });
        }
    }
    out
}

fn emails(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let local_ok = |c: u8| c.is_ascii_alphanumeric() || b"._%+-".contains(&c);
    let dom_ok = |c: u8| c.is_ascii_alphanumeric() || c == b'.' || c == b'-';
    let mut out = Vec::new();
    for (at, _) in text.match_indices('@') {
        let mut s = at;
        while s > 0 && local_ok(b[s - 1]) {
            s -= 1;
        }
        while s < at && b[s] == b'.' {
            s += 1;
        }
        let mut e = at + 1;
        while e < b.len() && dom_ok(b[e]) {
            e += 1;
        }
        while e > at + 1 && (b[e - 1] == b'.' || b[e - 1] == b'-') {
            e -= 1;
        }
        if s == at || e <= at + 1 {
            continue;
        }
        let domain = &text[at + 1..e];
        let Some(dot) = domain.rfind('.') else { continue };
        let tld = &domain[dot + 1..];
        if dot == 0 || tld.len() < 2 || !tld.bytes().all(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        out.push(Span { kind: Kind::Email, start: s, end: e });
    }
    out
}

fn thai_id_ok(d: &[u8]) -> bool {
    let sum: u32 = d[..12].iter().enumerate().map(|(i, c)| u32::from(c - b'0') * (13 - i as u32)).sum();
    u32::from(d[12] - b'0') == (11 - sum % 11) % 10
}

fn luhn_ok(d: &[u8]) -> bool {
    let sum: u32 = d
        .iter()
        .rev()
        .enumerate()
        .map(|(i, c)| {
            let mut v = u32::from(c - b'0');
            if i % 2 == 1 {
                v *= 2;
                if v > 9 {
                    v -= 9;
                }
            }
            v
        })
        .sum();
    sum % 10 == 0
}

fn card_grouping(lens: &[usize]) -> bool {
    match lens {
        [_] | [4, 6, 5] | [4, 6, 4] => true,
        _ => lens.len() >= 3 && lens[..lens.len() - 1].iter().all(|&l| l == 4) && (1..=4).contains(&lens[lens.len() - 1]),
    }
}

/// Whether the line holding a byte offset mentions one of `keywords`. The
/// line is lowercased only when asked, and the last answer is kept, so a
/// scan down a long line pays for it once rather than once per number.
struct LineKeywords<'a> {
    text: &'a str,
    keywords: &'static [&'static str],
    last: Option<(usize, usize, bool)>,
}

impl<'a> LineKeywords<'a> {
    fn new(text: &'a str, keywords: &'static [&'static str]) -> Self {
        Self { text, keywords, last: None }
    }

    fn at(&mut self, at: usize) -> bool {
        if let Some((s, e, r)) = self.last {
            if at >= s && at < e {
                return r;
            }
        }
        let s = self.text[..at].rfind('\n').map_or(0, |i| i + 1);
        let e = self.text[at..].find('\n').map_or(self.text.len(), |i| at + i);
        let line = self.text[s..e].to_lowercase();
        let r = self.keywords.iter().any(|k| line.contains(k));
        self.last = Some((s, e, r));
        r
    }
}

fn classify(text: &str, plus: bool, groups: &[(usize, usize)], bank_line: &mut dyn FnMut() -> bool) -> Option<Kind> {
    let lens: Vec<usize> = groups.iter().map(|&(a, b)| b - a).collect();
    let n: usize = lens.iter().sum();
    if !(8..=MAX_DIGITS).contains(&n) {
        return None;
    }
    let digits: Vec<u8> = groups.iter().flat_map(|&(a, b)| text.as_bytes()[a..b].iter().copied()).collect();
    if plus {
        return (8..=15).contains(&n).then_some(Kind::Phone);
    }
    if n == 13 && (lens == [13] || lens == [1, 4, 5, 2, 1]) && thai_id_ok(&digits) {
        return Some(Kind::ThaiId);
    }
    if (13..=19).contains(&n) && card_grouping(&lens) && luhn_ok(&digits) {
        return Some(Kind::Card);
    }
    if (n == 9 || n == 10) && digits[0] == b'0' && digits[1] != b'0' && lens.len() <= 3 {
        return Some(Kind::Phone);
    }
    if (10..=12).contains(&n) && bank_line() {
        return Some(Kind::BankAccount);
    }
    None
}

/// One run of digits inside a number: where it is, the `(` that opened it,
/// and the byte that joined it to the group before.
struct Group {
    a: usize,
    b: usize,
    open: Option<usize>,
    join: u8,
}

/// How the run continues after a group ending at `i`, if it does: the
/// offset of the next group (or its `(`) and the joining byte. `)` joins only
/// a group a `(` opened; `,` joins only a thousands group (at most three
/// digits before it, exactly three after), so `0812345678,0898765432` is two
/// numbers.
fn next_join(b: &[u8], i: usize, open: bool, glen: usize) -> Option<(usize, u8)> {
    let c = *b.get(i)?;
    let digit = |j: usize| b.get(j).is_some_and(u8::is_ascii_digit);
    match c {
        b')' if open => {
            let j = if b.get(i + 1) == Some(&b' ') { i + 2 } else { i + 1 };
            digit(j).then_some((j, b')'))
        }
        b' ' | b'-' | b'.' => {
            if digit(i + 1) {
                Some((i + 1, c))
            } else if c == b' ' && b.get(i + 1) == Some(&b'(') && digit(i + 2) {
                Some((i + 1, b' '))
            } else {
                None
            }
        }
        b',' if glen <= 3 && digit(i + 1) => {
            let n = b[i + 1..].iter().take(4).take_while(|c| c.is_ascii_digit()).count();
            (n == 3).then_some((i + 1, b','))
        }
        _ => None,
    }
}

/// Phone, national ID, card and bank numbers. A run is digit groups joined by
/// a space, `-` or `.` (and `(…)` around a group); it must not touch an ASCII
/// letter or digit. Within a run the longest classifiable group sequence
/// wins, so `0812345678 5 คน` still finds the phone — but never starting or
/// ending beside a `.` or `,`, which would cut a decimal in two.
fn numbers(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let mut bank = LineKeywords::new(text, BANK_KEYWORDS);
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let next_digit = b.get(i + 1).is_some_and(u8::is_ascii_digit);
        let plus = b[i] == b'+' && next_digit;
        let paren = b[i] == b'(' && next_digit;
        let left_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if !((b[i].is_ascii_digit() || plus || paren) && left_ok) {
            i += 1;
            continue;
        }
        let start = i;
        if plus {
            i += 1;
        }
        let mut groups: Vec<Group> = Vec::new();
        let mut join = 0u8;
        loop {
            let open = if b[i] == b'(' {
                i += 1;
                Some(i - 1)
            } else {
                None
            };
            let g = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            groups.push(Group { a: g, b: i, open, join });
            match next_join(b, i, open.is_some(), i - g) {
                Some((ni, j)) => {
                    i = ni;
                    join = j;
                }
                None => break,
            }
        }
        let last_len = groups.last().map_or(0, |g| g.b - g.a);
        let right_ok = i >= b.len()
            || !(b[i].is_ascii_alphanumeric() || (b[i] == b',' && b.get(i + 1).is_some_and(u8::is_ascii_digit) && last_len <= 3));
        if right_ok {
            let cuts_decimal = |g: &Group| matches!(g.join, b'.' | b',');
            let mut g = 0;
            while g < groups.len() {
                let lead_plus = plus && g == 0;
                let mut hit = None;
                if g == 0 || !cuts_decimal(&groups[g]) {
                    let at = groups[g].a;
                    for j in (g + 1..=groups.len().min(g + MAX_DIGITS)).rev() {
                        if j < groups.len() && cuts_decimal(&groups[j]) {
                            continue;
                        }
                        let spans: Vec<(usize, usize)> = groups[g..j].iter().map(|x| (x.a, x.b)).collect();
                        if let Some(k) = classify(text, lead_plus, &spans, &mut || bank.at(at)) {
                            hit = Some((j, k));
                            break;
                        }
                    }
                }
                match hit {
                    Some((j, kind)) => {
                        let s = match groups[g].open {
                            _ if lead_plus => start,
                            Some(p) if j - g >= 2 => p,
                            _ => groups[g].a,
                        };
                        out.push(Span { kind, start: s, end: groups[j - 1].b });
                        g = j;
                    }
                    None => g += 1,
                }
            }
        } else {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b',') {
                i += 1;
            }
        }
    }
    out
}

/// `[A-Z]{1,2}[0-9]{6,8}` as a whole ASCII word, on a line that says passport.
fn passports(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let mut keywords = LineKeywords::new(text, PASSPORT_KEYWORDS);
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_alphanumeric() || (i > 0 && b[i - 1].is_ascii_alphanumeric()) {
            i += 1;
            continue;
        }
        let s = i;
        while i < b.len() && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        let w = &b[s..i];
        let letters = w.iter().take_while(|c| c.is_ascii_uppercase()).count();
        let digits = w.len() - letters;
        if (1..=2).contains(&letters)
            && (6..=8).contains(&digits)
            && w[letters..].iter().all(u8::is_ascii_digit)
            && keywords.at(s)
        {
            out.push(Span { kind: Kind::Passport, start: s, end: i });
        }
    }
    out
}

/// Every sensitive span, non-overlapping, sorted by start. Spans that
/// overlap are merged into their union — dropping the loser would leave part
/// of it in the clear (`pass: 4111 1111 1111 1111` masks the whole card). The
/// merged kind is the first secret's if any member is a secret, else the
/// longest member's.
pub fn detect(text: &str) -> Vec<Span> {
    merge(&components(text))
}

/// Every individual hit, before `merge`: may overlap, sorted by start. The
/// vault registers each one's value as well as each merged span's, so a value
/// whose merged span also took trailing markup (`Tr0ub4dor</div>`, read from
/// the text as written) is still masked where it repeats bare (round 3, F1).
pub fn components(text: &str) -> Vec<Span> {
    let mut all: Vec<Span> = secrets(text).into_iter().map(|h| h.span).collect();
    all.extend(emails(text));
    all.extend(numbers(text));
    all.extend(passports(text));
    all.retain(|s| s.start < s.end);
    all.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    all
}

/// `components` merged as `detect` documents: non-overlapping, sorted.
pub fn merge(all: &[Span]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    // (start, end, first secret kind, longest kind, longest length)
    let mut cur: Option<(usize, usize, Option<Kind>, Kind, usize)> = None;
    let flush = |cur: Option<(usize, usize, Option<Kind>, Kind, usize)>, out: &mut Vec<Span>| {
        if let Some((start, end, secret, longest, _)) = cur {
            out.push(Span { kind: secret.unwrap_or(longest), start, end });
        }
    };
    for &s in all {
        let len = s.end - s.start;
        match &mut cur {
            Some((_, end, secret, longest, longest_len)) if s.start < *end => {
                *end = (*end).max(s.end);
                if secret.is_none() && s.kind.is_secret() {
                    *secret = Some(s.kind);
                }
                if len > *longest_len {
                    *longest = s.kind;
                    *longest_len = len;
                }
            }
            _ => {
                flush(cur.take(), &mut out);
                cur = Some((s.start, s.end, s.kind.is_secret().then_some(s.kind), s.kind, len));
            }
        }
    }
    flush(cur, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every value below is synthetic. Thai IDs were generated to pass the
    // mod-11 checksum; card numbers are the payment networks' published test
    // numbers.
    const THAI_ID: &str = "1101700123456";
    const THAI_ID_DASHED: &str = "1-1017-00123-45-6";
    const THAI_ID_BAD: &str = "1101700123457";
    const VISA_TEST: &str = "4111111111111111";

    fn found(text: &str) -> Vec<(Kind, &str)> {
        detect(text).into_iter().map(|s| (s.kind, &text[s.start..s.end])).collect()
    }

    /// `value` lies wholly inside one secret span.
    fn covered(text: &str, value: &str) -> bool {
        let at = text.find(value).unwrap();
        detect(text).iter().any(|s| s.kind.is_secret() && s.start <= at && at + value.len() <= s.end)
    }

    #[test]
    fn emails_are_found_and_trailing_punctuation_is_not_part_of_them() {
        assert_eq!(found("mail alice.b+x@mail.example.co.th."), [(Kind::Email, "alice.b+x@mail.example.co.th")]);
        // Thai is written without spaces: the address must still be found.
        assert_eq!(found("ติดต่อalice@example.testครับ"), [(Kind::Email, "alice@example.test")]);
        assert_eq!(found("ติดต่อ alice@example.test ครับ"), [(Kind::Email, "alice@example.test")]);
        assert!(found("user@localhost and @handle and a@b.c").is_empty());
    }

    #[test]
    fn thai_and_international_phones_are_found_in_any_common_grouping() {
        for p in ["0812345678", "081-234-5678", "081 234 5678", "02-123-4567", "+66 81 234 5678", "+14155550123"] {
            assert_eq!(found(&format!("โทร {p} นะ")), [(Kind::Phone, p)], "{p}");
        }
        assert_eq!(found("โทร0812345678"), [(Kind::Phone, "0812345678")], "Thai text without a space");
        assert_eq!(found("0812345678 5 คน"), [(Kind::Phone, "0812345678")], "a trailing count is not part of it");
    }

    #[test]
    fn thai_ids_need_a_valid_checksum() {
        assert_eq!(found(&format!("บัตร {THAI_ID}")), [(Kind::ThaiId, THAI_ID)]);
        assert_eq!(found(&format!("ID {THAI_ID_DASHED}")), [(Kind::ThaiId, THAI_ID_DASHED)]);
        assert!(found(&format!("ref {THAI_ID_BAD}")).is_empty());
    }

    #[test]
    fn cards_need_luhn_and_a_card_like_grouping() {
        assert_eq!(found(&format!("card {VISA_TEST}")), [(Kind::Card, VISA_TEST)]);
        assert_eq!(found("card 4111 1111 1111 1111"), [(Kind::Card, "4111 1111 1111 1111")]);
        assert_eq!(found("amex 3782 822463 10005"), [(Kind::Card, "3782 822463 10005")]);
        assert!(found("order 4111111111111112").is_empty(), "Luhn fails");
    }

    #[test]
    fn bank_accounts_and_passports_need_a_keyword_on_the_line() {
        assert_eq!(found("เลขที่บัญชี 123-4-56789-0"), [(Kind::BankAccount, "123-4-56789-0")]);
        assert!(found("ref 123-4-56789-0").is_empty());
        assert_eq!(found("passport AA1234567"), [(Kind::Passport, "AA1234567")]);
        assert!(found("SKU AA1234567").is_empty());
    }

    #[test]
    fn ordinary_numbers_are_not_pii() {
        for t in [
            "2026-10-07 meeting at 14:30",
            "order #123456789",
            "pi is 3.14159265358979",
            "uuid 550e8400-e29b-41d4-a716-446655440000",
            "uuid8 0babe000 and c0ffee00",
            "total 1,234,567,890,123 baht",
            "version 0.32.0",
        ] {
            assert!(found(t).is_empty(), "{t} -> {:?}", found(t));
        }
    }

    #[test]
    fn a_secret_beats_pii_on_the_same_value() {
        assert_eq!(found("password: 0812345678"), [(Kind::Password, "0812345678")]);
    }

    #[test]
    fn thai_keywords_find_values_without_spaces_before_them() {
        assert_eq!(found("กรุณาใส่รหัสผ่าน: hunter22x"), [(Kind::Password, "hunter22x")]);
        assert_eq!(found("รหัส: ABC-001"), [(Kind::Password, "ABC-001")], "accepted false hit");
        assert!(found("รหัสไปรษณีย์: 10110").is_empty(), "a postal code is not a password");
    }

    #[test]
    fn every_occurrence_is_found_not_just_the_first() {
        let t = "pass: aaaa1111 then pass: bbbb2222";
        assert_eq!(found(t), [(Kind::Password, "aaaa1111"), (Kind::Password, "bbbb2222")]);
    }

    #[test]
    fn a_private_key_is_masked_as_one_whole_block() {
        let pem = ["-----BEGIN RSA ", "PRIVATE KEY-----\nMIIEabc\nxyz\n-----END RSA PRIVATE KEY-----"].concat();
        let text = format!("before\n{pem}\nafter");
        assert_eq!(found(&text), [(Kind::PrivateKey, pem.as_str())]);
    }

    #[test]
    fn spans_fall_on_char_boundaries_in_mixed_scripts() {
        let t = "ลูกค้า 😀 alice@example.test โทร 081-234-5678 รหัสผ่าน: pässwörd9 ✅";
        for s in detect(t) {
            assert!(t.is_char_boundary(s.start) && t.is_char_boundary(s.end), "{s:?}");
        }
        assert_eq!(detect(t).len(), 3);
    }

    // ---- fix round 1 -------------------------------------------------------

    #[test]
    fn ascii_keywords_are_bounded_by_ascii_so_thai_before_them_is_fine() {
        assert_eq!(found("ตั้งpin: 4829"), [(Kind::Pin, "4829")]);
        assert_eq!(found("ใส่password: hunter22x"), [(Kind::Password, "hunter22x")]);
    }

    #[test]
    fn underscore_and_dash_prefixed_keywords_are_found() {
        assert_eq!(found("DB_PASSWORD=hunter22x"), [(Kind::Password, "hunter22x")]);
        assert_eq!(found("client_secret: abcd1234"), [(Kind::Secret, "abcd1234")]);
        assert_eq!(found("GITHUB_TOKEN=abcd1234efgh"), [(Kind::Secret, "abcd1234efgh")]);
        assert_eq!(found("my-api-key: abcd1234"), [(Kind::ApiKey, "abcd1234")]);
        assert!(found("pass_through: xxxxxxxx").is_empty(), "the after side is still strict");
    }

    #[test]
    fn separators_quotes_arrows_and_words_are_accepted() {
        assert_eq!(found(r#"{"password": "hunter22x"}"#), [(Kind::Password, "hunter22x")]);
        assert_eq!(found("password => 'hunter22x'"), [(Kind::Password, "hunter22x")]);
        assert_eq!(found("my password is hunter22x"), [(Kind::Password, "hunter22x")]);
        assert_eq!(found("รหัสผ่านคือ hunter22x"), [(Kind::Password, "hunter22x")]);
        assert_eq!(found(r#"password: "correct horse battery""#), [(Kind::Password, "correct horse battery")]);
        assert!(found("password is required").is_empty());
    }

    #[test]
    fn overlaps_merge_into_the_union_instead_of_dropping_the_longer_pii() {
        assert_eq!(found("pass: 4111 1111 1111 1111"), [(Kind::Password, "4111 1111 1111 1111")]);
        assert_eq!(found("password: 0812345678"), [(Kind::Password, "0812345678")]);
    }

    #[test]
    fn phones_in_lists_dotted_and_bracketed_forms() {
        assert_eq!(
            found("0812345678,0898765432"),
            [(Kind::Phone, "0812345678"), (Kind::Phone, "0898765432")]
        );
        for p in ["081.234.5678", "02.123.4567", "(02) 123-4567", "+1 (415) 555-0123"] {
            assert_eq!(found(&format!("โทร {p} นะ")), [(Kind::Phone, p)], "{p}");
        }
    }

    #[test]
    fn more_ordinary_numbers_are_not_pii() {
        for t in ["ip 192.168.1.100", "ratio 1.5", "1,234,567", "pi 3.14159265358979", "v 10.0.0.1"] {
            assert!(found(t).is_empty(), "{t} -> {:?}", found(t));
        }
    }

    #[test]
    fn bank_names_count_as_bank_keywords() {
        assert_eq!(found("กสิกร 123-4-56789-0"), [(Kind::BankAccount, "123-4-56789-0")]);
        assert_eq!(found("kbank 123-4-56789-0"), [(Kind::BankAccount, "123-4-56789-0")]);
        assert_eq!(found("พร้อมเพย์ 123-4-56789-0"), [(Kind::BankAccount, "123-4-56789-0")]);
    }

    #[test]
    fn a_long_single_line_is_not_scanned_quadratically() {
        let t0 = std::time::Instant::now();
        let t = "x 12 ".repeat(40_000);
        assert!(detect(&t).is_empty());
        let t = "AB123456 ".repeat(20_000);
        assert!(detect(&t).is_empty());
        let t = "1 ".repeat(20_000);
        assert!(detect(&t).is_empty());
        let t = "pass: <a".repeat(20_000);
        let _ = detect(&t);
        assert!(t0.elapsed() < std::time::Duration::from_secs(2), "took {:?}", t0.elapsed());
    }

    #[test]
    fn lowercasing_never_skips_the_keyword_scan() {
        // U+0130 lowercases to THREE bytes (it is two), so a Unicode
        // `to_lowercase` shifted every later offset and the scan was skipped.
        assert_eq!(found("İ password: hunter22x"), [(Kind::Password, "hunter22x")]);
        assert_eq!(found("Ω password: hunter22x"), [(Kind::Password, "hunter22x")]);
    }

    #[test]
    fn more_key_shapes_are_found() {
        let alnum20 = "abcdefghijklmnop1234";
        for pre in [["sk_", "live_"], ["sk_", "test_"], ["rk_", "live_"]] {
            let k = [pre[0], pre[1], alnum20].concat();
            assert_eq!(found(&format!("key {k}")), [(Kind::ApiKey, k.as_str())]);
        }
        let g = ["AIza", "SyA-abcdefghijklmnopqrstuvwxyz01234"].concat();
        assert_eq!(found(&format!("maps {g}")), [(Kind::ApiKey, g.as_str())]);
        let short = ["AIza", "SyA-abcdefghijklmnopqrstuvwxyz0123"].concat();
        assert!(found(&format!("maps {short}")).is_empty(), "exactly 35 after the prefix");
        let c = ["GOCSPX", "-abcdefghijklmnopqrst1234"].concat();
        assert_eq!(found(&format!("client {c}")), [(Kind::Secret, c.as_str())]);
        for pre in ["gho_", "ghs_", "ghu_", "ghr_"] {
            let k = [pre, "abcdefghijklmnopqrstuvwxyz0123456789"].concat();
            assert_eq!(found(&format!("t {k}")), [(Kind::ApiKey, k.as_str())], "{pre}");
        }
    }

    #[test]
    fn jwts_and_bearer_tokens_are_found() {
        let jwt = ["eyJ", "hbGciOiJIUzI1NiJ9", ".", "eyJ", "zdWIiOiJhYmNkZWZnaCJ9", ".", "abcDEF_-xyzQRS"].concat();
        assert_eq!(found(&format!("jwt {jwt} end")), [(Kind::Secret, jwt.as_str())]);
        assert_eq!(found(&format!("Authorization: Bearer {jwt}")), [(Kind::Secret, jwt.as_str())]);
        let opaque = "abcdefghijklmnopqrst12345";
        assert_eq!(found(&format!("Authorization: Bearer {opaque}")), [(Kind::Secret, opaque)]);
        assert!(found("Authorization: Bearer short").is_empty());
        assert!(found("eyJ.eyJ.x").is_empty(), "too short to be a JWT");
    }

    #[test]
    fn mixed_script_spans_are_exact() {
        let t = "ลูกค้า 😀 alice@example.test โทร 081-234-5678 รหัสผ่าน: pässwörd9 ✅";
        assert_eq!(
            found(t),
            [(Kind::Email, "alice@example.test"), (Kind::Phone, "081-234-5678"), (Kind::Password, "pässwörd9")]
        );
    }

    #[test]
    fn a_high_entropy_token_is_a_secret_only_beside_a_keyword() {
        let tok = "ANcPqNyjkmMysiM9rCEki9ZeHBgW0xTBV5NlQx";
        assert_eq!(found(&format!("secret value {tok}")), [(Kind::Secret, tok)]);
        assert!(found(&format!("video id {tok}")).is_empty());
    }

    #[test]
    fn an_unterminated_pem_masks_to_the_end_of_the_text() {
        let head = ["-----BEGIN ", "PRIVATE KEY-----"].concat();
        let text = format!("note\n{head}\nMIIEabc\nxyz");
        assert_eq!(found(&text), [(Kind::PrivateKey, &text[5..])]);
    }

    #[test]
    fn degenerate_input_finds_nothing() {
        assert!(detect("").is_empty());
        assert!(found("@ + @@ ++ a @ b +x").is_empty());
    }

    #[test]
    fn dashed_cards_are_found() {
        assert_eq!(found("card 4111-1111-1111-1111"), [(Kind::Card, "4111-1111-1111-1111")]);
    }

    #[test]
    fn arbitrary_mixes_never_panic_and_always_yield_ordered_char_aligned_spans() {
        let alphabet: Vec<&str> = vec![
            "0", "1", "4", "8", "9", " ", "-", ".", ",", "(", ")", "+", "@", ":", "=", ">", "\"", "'", "\n", "a", "z", "A",
            "pass", "pin", "is ", "bearer ", "token", "คือ", "รหัส", "ก", "😀", "İ", "Ω", "eyJ", "-----BEGIN ", "account",
            "<b>", "</span>", "<span title=\"ไทย\">", "&nbsp;", "&#160;", "**", "_", " code", "#", "bearer:", "pw",
            "PRIVATE KEY", "<o:p>", "<wbr>", "<!--", "<5", "?password=", "\">",
        ];
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..3000 {
            let mut t = String::new();
            for _ in 0..40 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                t.push_str(alphabet[(x >> 33) as usize % alphabet.len()]);
            }
            let mut prev_end = 0;
            for s in detect(&t) {
                assert!(s.start < s.end && s.start >= prev_end, "{t:?} -> {s:?}");
                assert!(t.is_char_boundary(s.start) && t.is_char_boundary(s.end), "{t:?} -> {s:?}");
                prev_end = s.end;
            }
        }
    }

    // ---- final review --------------------------------------------------------

    /// C1: Extract sends raw `body_html`. The value after an HTML or markdown
    /// label must be masked, not the markup between the label and the value.
    #[test]
    fn a_value_after_an_html_or_markdown_label_is_the_span() {
        for (t, v) in [
            ("<div><b>Password:</b> hunter22x</div>", "hunter22x"),
            ("<b>API key:</b> abcd1234efgh", "abcd1234efgh"),
            ("<span>Password:</span> <span>hunter22x</span>", "hunter22x"),
            ("<b>Password</b>: hunter22x", "hunter22x"),
            ("**Password:** hunter22x", "hunter22x"),
            ("**Password**: hunter22x", "hunter22x"),
        ] {
            let spans = detect(t);
            assert_eq!(spans.len(), 1, "{t} -> {:?}", found(t));
            assert_eq!(&t[spans[0].start..spans[0].end], v, "{t}");
            assert!(spans[0].kind.is_secret(), "{t}");
        }
        // Round 2: the text as written is read too, so the span may also
        // cover the entity and closing tag. Masking more is safe.
        for (t, v) in [("<div>Password:&nbsp;hunter22x</div>", "hunter22x"), ("<p>pin:&#160;4821</p>", "4821")] {
            assert!(covered(t, v), "{t} -> {:?}", found(t));
        }
    }

    #[test]
    fn a_value_that_is_only_a_tag_is_not_plausible() {
        assert!(!plausible("</b>"));
        assert!(!plausible("<span class=\"x\">"));
        assert!(plausible("<hunter22x>"), "not an HTML tag name: could be a value");
        assert!(!plausible("</b><wbr>") && !plausible("<o:p></o:p>"), "markup only");
        assert!(plausible("</b>hunter22x"));
    }

    #[test]
    fn markup_blanking_does_not_disturb_plain_text_values() {
        assert_eq!(found("password: _secret_"), [(Kind::Password, "_secret_")]);
        assert_eq!(found("password: *hunter*"), [(Kind::Password, "*hunter*")]);
        assert_eq!(found("password: <hunter22x>"), [(Kind::Password, "<hunter22x>")]);
        // Blanking `<b>` leaves `a`, too short to be a value: the value is
        // then read from the text as written, or the password leaks.
        assert_eq!(found("pass: a<b>&c99"), [(Kind::Password, "a<b>&c99")]);
        assert_eq!(found("<b>pass:</b> ab<br>cd99"), [(Kind::Password, "ab<br>cd99")]);
    }

    /// Round 2, R1: blanking a whole tag hid keyword=value secrets inside its
    /// attributes. Keyword values are read from the text as written too.
    #[test]
    fn a_keyword_value_inside_an_html_attribute_is_masked() {
        for (t, v) in [
            ("<a href=\"https://x.test/login?password=hunter22x\">login</a>", "hunter22x"),
            ("<a href=\"https://x.test/cb?token=Zx9kQ2mP7vR4tY8w\">click here</a>", "Zx9kQ2mP7vR4tY8w"),
            ("<a href=\"https://x.test/docs?api_key=abcd1234efgh5678\">docs</a>", "abcd1234efgh5678"),
            ("<img src=\"https://x.test/i.png?access_token=Zx9kQ2mP7vR4tY8wAAA\">", "Zx9kQ2mP7vR4tY8wAAA"),
        ] {
            assert!(covered(t, v), "{t} -> {:?}", found(t));
        }
    }

    /// Round 2, R1: the view cut these plain-text values short.
    #[test]
    fn a_value_holding_markup_is_covered_whole() {
        for (t, v) in [("password: Pass<a>1234", "Pass<a>1234"), ("password: abcd&nbsp;1234", "abcd&nbsp;1234")] {
            assert!(covered(t, v), "{t} -> {:?}", found(t));
        }
    }

    /// Round 2, R2: markup outside the old element list (Word paste `<o:p>`,
    /// `<wbr>`, `<colgroup>`) is blanked too, and is never the value.
    #[test]
    fn unlisted_markup_between_label_and_value_is_not_the_value() {
        for t in [
            "<b>Password:</b><wbr> hunter22x",
            "<b>Password:</b><o:p></o:p> hunter22x",
            "<td>Password:</td><colgroup></colgroup> hunter22x",
        ] {
            assert_eq!(found(t), [(Kind::Password, "hunter22x")], "{t}");
        }
        assert!(found("if a < b and x<5 then pass: <= 3").is_empty());
    }

    /// I1: common PIN/password phrasings.
    #[test]
    fn a_connector_word_between_keyword_and_separator_is_accepted() {
        for t in ["PIN code: 4821", "pin-code: 4821", "pin_code: 4821", "PIN no: 4821", "PIN no.: 4821", "PIN number: 4821", "PIN #: 4821", "PIN#: 4821"] {
            assert_eq!(found(t), [(Kind::Pin, "4821")], "{t}");
        }
        for t in ["password number: hunter22x", "passcode-no: hunter22x", "Password code = hunter22x"] {
            assert_eq!(found(t), [(Kind::Password, "hunter22x")], "{t}");
        }
        assert!(found("pin codes are: none").is_empty(), "the connector is a whole word");
        assert!(found("pass_through: xxxxxxxx").is_empty(), "`pass` takes no connector");
    }

    #[test]
    fn pw_and_pwd_are_password_keywords() {
        assert_eq!(found("pw: hunter22x"), [(Kind::Password, "hunter22x")]);
        assert_eq!(found("wifi PWD=hunter22x"), [(Kind::Password, "hunter22x")]);
        assert!(found("kpw: hunter22x").is_empty(), "word-bounded");
    }

    #[test]
    fn bearer_accepts_a_colon() {
        let opaque = "abcdefghijklmnopqrst12345";
        assert_eq!(found(&format!("Bearer: {opaque}")), [(Kind::Secret, opaque)]);
        assert_eq!(found(&format!("bearer:{opaque}")), [(Kind::Secret, opaque)]);
    }

    /// I2: armored PGP private keys, header assembled at runtime.
    #[test]
    fn an_armored_pgp_private_key_is_one_block() {
        let head = ["-----BEGIN PGP ", "PRIVATE KEY BLOCK-----"].concat();
        let block = format!("{head}\n\nlQOYBGabc\nxyz=\n-----END PGP PRIVATE KEY BLOCK-----");
        let text = format!("keys\n{block}\nafter the block");
        assert_eq!(found(&text), [(Kind::PrivateKey, block.as_str())]);
        let public = ["-----BEGIN PGP ", "PUBLIC KEY BLOCK-----\nmQENBGabc\n-----END PGP PUBLIC KEY BLOCK-----"].concat();
        assert!(found(&public).is_empty(), "a public key is not a secret");
    }

    /// Round 3, F2: the view/text union de-duplicated with `Vec::contains`,
    /// quadratic in the keyword hits on one line.
    #[test]
    fn many_keyword_hits_on_one_marked_up_line_are_not_deduplicated_quadratically() {
        let t = "pass: <i>abcd1234</i> ".repeat(20_000);
        let t0 = std::time::Instant::now();
        assert_eq!(detect(&t).len(), 20_000);
        assert!(t0.elapsed() < std::time::Duration::from_secs(2), "took {:?}", t0.elapsed());
    }

    /// M2: a header without the marker used to be re-scanned to the end of
    /// its line for every `-----BEGIN` on it — O(k·n) on one long line.
    #[test]
    fn many_pem_headers_on_one_line_are_not_scanned_quadratically() {
        let t = "-----BEGIN ".repeat(100_000);
        let t0 = std::time::Instant::now();
        assert!(detect(&t).is_empty());
        assert!(t0.elapsed() < std::time::Duration::from_secs(2), "took {:?}", t0.elapsed());
        // The skip must not lose a real block later on the same line.
        let head = ["-----BEGIN ", "PRIVATE KEY-----"].concat();
        let t = format!("{}{head}\nMIIE\n-----END PRIVATE KEY-----", "-----BEGIN x ".repeat(3));
        assert_eq!(found(&t).len(), 1);
    }

    #[test]
    fn keywords_on_another_line_do_not_apply() {
        assert!(found("account\n123-4-56789-0").is_empty());
        assert!(found("passport\nAA1234567").is_empty());
        assert_eq!(found("passport\nAA1234567 passport"), [(Kind::Passport, "AA1234567")]);
    }
}

/// The Organize scanner's existing tests, run against a wrapper identical to
/// the one `curate::secrets::scan` becomes in Task 2. If these pass, the
/// swap is behaviour-preserving.
#[cfg(test)]
mod curate_compat {
    use super::*;

    struct Finding {
        kind: &'static str,
        masked: String,
    }

    fn mask(label: &str, value: &str) -> String {
        let shown: String = if value.chars().count() >= 12 { value.chars().take(2).collect() } else { String::new() };
        format!("{label}: {shown}••••••")
    }

    fn scan(text: &str) -> Vec<Finding> {
        let mut out: Vec<Finding> = Vec::new();
        for h in secrets(text) {
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
