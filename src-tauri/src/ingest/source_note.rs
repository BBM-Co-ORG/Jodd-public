//! The source note: one fetched page, stored as a note of its own (spec
//! 2026-11-19-source-notes-design.md §5.1–§5.3), and the `Derived from` line
//! a derived note carries to point back at it.
//!
//! Everything a later step reads back is VISIBLE TEXT. iCloud and SSH drop
//! `<details>`, `<summary>`, `<pre>` and every attribute when a note syncs
//! back, so `recognise` / `parse_source_note` / `parse_derived_from` all work
//! from `html_to_text` lines, never from markup.

use crate::agent_memory::text::html_to_text;
use crate::db::note_slug;
use crate::ingest::discuss::PASTED_HEADER;
use crate::ingest::stored::{parse_sources, render_sources, HEADER_PREFIX};
use crate::ingest::urls::{self, UrlKind};
use crate::ingest::FetchedSource;
use crate::llm::markdown::{escape_html, source_block};
use sha2::{Digest, Sha256};

pub const MARKER_PREFIX: &str = "Jodd source · fetched ";
const DERIVED_PREFIX: &str = "Derived from [[";
const SEP: &str = " · ";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FetchOpt {
    Transcription,
    Comments,
    Reader,
    Pasted,
}

impl FetchOpt {
    fn word(self) -> &'static str {
        match self {
            FetchOpt::Transcription => "transcription",
            FetchOpt::Comments => "comments",
            FetchOpt::Reader => "reader",
            FetchOpt::Pasted => "pasted",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [FetchOpt::Transcription, FetchOpt::Comments, FetchOpt::Reader, FetchOpt::Pasted].into_iter().find(|o| o.word() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceHeader {
    /// `YYYY-MM-DD HH:MM`.
    pub fetched_at: String,
    pub sha256: String,
    pub with: Vec<FetchOpt>,
    pub validators: Validators,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedFrom {
    /// The text inside `[[ ]]`.
    pub link_key: String,
    pub copy_date: String,
    pub sha256: String,
}

/// The identity of a link for "have I fetched this already": the video id for
/// YouTube, `canonical_url` for the social kinds, otherwise the URL without
/// its fragment.
pub fn source_key(url: &str) -> String {
    match urls::classify(url) {
        UrlKind::YouTube { id } => format!("youtube:{id}"),
        kind if urls::social_kind(&kind).is_some() => urls::canonical_url(url),
        _ => url.split('#').next().unwrap_or(url).to_string(),
    }
}

/// A substring every body of a source note for `key` holds, in its HTML form
/// and every synced-back plain form, so `Db::source_note_for_key` can narrow
/// in SQL before parsing anything (I3). `None`: no such substring is known,
/// and every source note is a candidate.
///
/// The body keeps the link as the user first gave it — in an `href`, and as
/// text on the block's `URL:` line — and different paths escape `&`, `<`,
/// `>`, `"` and `'` differently. So: a YouTube key's video id; a social
/// key's post id (the key is `canonical_url`, which can differ from the
/// stored URL anywhere but the id); otherwise the longest run of the key
/// (the URL without its fragment, a prefix of the stored URL) with none of
/// those five characters.
pub fn key_needle(key: &str) -> Option<String> {
    if let Some(id) = key.strip_prefix("youtube:") {
        return (!id.is_empty()).then(|| id.to_string());
    }
    let id = match urls::classify(key) {
        UrlKind::X { id } | UrlKind::TikTok { id: Some(id) } => Some(id),
        UrlKind::Instagram { shortcode } => Some(shortcode),
        kind if urls::social_kind(&kind).is_some() => return None,
        _ => None,
    };
    let needle = id.unwrap_or_else(|| {
        key.split(['&', '<', '>', '"', '\'']).max_by_key(|run| run.len()).unwrap_or("").to_string()
    });
    (!needle.is_empty()).then_some(needle)
}

/// SHA-256 (lowercase hex) of the fetched text before truncation, with the
/// pasted-comments tail cut off: the user's own paste is not the page.
pub fn content_hash(fetched_text_untruncated: &str) -> String {
    let page = fetched_text_untruncated.split(PASTED_HEADER).next().unwrap_or("").trim_end();
    Sha256::digest(page.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Ruling 16: every line break as `\n`. A `\r` (a text/plain page, a JSON
/// caption) or a U+2028 / U+2029 (a paste) would ride an iCloud CREATE — which
/// has no writability gate — and then make `compose::layers_round_trip`
/// refuse every later in-place update, so the note is push-blocked for good
/// (gotcha #14/#24). Applied before `content_hash`, so the stored text and its
/// hash agree.
pub fn plain_line_breaks(text: &str) -> String {
    text.replace("\r\n", "\n").replace(['\r', '\u{2028}', '\u{2029}'], "\n")
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn marker_line(h: &SourceHeader) -> String {
    let mut s = format!("{MARKER_PREFIX}{}{SEP}sha256 {}", h.fetched_at, h.sha256);
    if !h.with.is_empty() {
        s.push_str(&format!("{SEP}with {}", h.with.iter().map(|o| o.word()).collect::<Vec<_>>().join(", ")));
    }
    s
}

fn validators_line(v: &Validators) -> Option<String> {
    let parts: Vec<String> = [("etag", &v.etag), ("last-modified", &v.last_modified)]
        .into_iter()
        .filter_map(|(k, val)| val.as_ref().map(|val| format!("{k} {}", one_line(val))))
        .collect();
    (!parts.is_empty()).then(|| parts.join(SEP))
}

/// The note's title and its HTML body.
pub fn render_source_note(src: &FetchedSource, header: &SourceHeader) -> (String, String) {
    let shown = urls::display_url(&src.url);
    let title = match src.title.as_deref().map(one_line).filter(|t| !t.is_empty()) {
        Some(t) => format!("Source — {t}"),
        None => format!("Source — {shown}"),
    };
    let mut body = format!("<div>{}</div>\n", escape_html(&marker_line(header)));
    if let Some(v) = validators_line(&header.validators) {
        body.push_str(&format!("<div>{}</div>\n", escape_html(&v)));
    }
    body.push_str(&format!("<div><a href=\"{}\">{}</a></div>\n", escape_html(&src.url), escape_html(&shown)));
    body.push_str(&source_block(&render_sources(std::slice::from_ref(src))));
    (title, body)
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

fn is_date_time(s: &str) -> bool {
    s.len() == 16 && is_date(&s[..10]) && s.as_bytes()[10] == b' ' && s.as_bytes()[13] == b':'
        && s[11..13].bytes().all(|c| c.is_ascii_digit()) && s[14..].bytes().all(|c| c.is_ascii_digit())
}

fn parse_marker(line: &str) -> Option<SourceHeader> {
    let rest = line.strip_prefix(MARKER_PREFIX)?;
    let mut parts = rest.split(SEP);
    let fetched_at = parts.next()?.to_string();
    let sha256 = parts.next()?.strip_prefix("sha256 ")?.to_string();
    if !is_date_time(&fetched_at) || !is_hex64(&sha256) {
        return None;
    }
    let mut with = Vec::new();
    if let Some(w) = parts.next() {
        for word in w.strip_prefix("with ")?.split(", ") {
            with.push(FetchOpt::parse(word)?);
        }
    }
    parts.next().is_none().then_some(SourceHeader { fetched_at, sha256, with, validators: Validators::default() })
}

fn parse_validators(line: &str) -> Option<Validators> {
    let (etag, rest) = match line.strip_prefix("etag ") {
        Some(r) => match r.split_once(&format!("{SEP}last-modified ")) {
            Some((e, lm)) => (Some(e.to_string()), Some(lm.to_string())),
            None => (Some(r.to_string()), None),
        },
        None => (None, line.strip_prefix("last-modified ").map(str::to_string)),
    };
    (etag.is_some() || rest.is_some()).then_some(Validators { etag, last_modified: rest })
}

/// The header and the source, read from the note's visible text.
pub fn parse_source_note(body_html: &str) -> Option<(SourceHeader, FetchedSource)> {
    let text = html_to_text(body_html, true);
    let lines: Vec<&str> = text.split('\n').collect();
    let block_at = lines.iter().position(|l| l.starts_with(HEADER_PREFIX))?;
    let marker_at = lines.iter().position(|l| l.starts_with(MARKER_PREFIX))?;
    if marker_at > block_at {
        return None;
    }
    let mut header = parse_marker(lines[marker_at])?;
    if let Some(v) = lines.get(marker_at + 1).filter(|_| marker_at + 1 < block_at).and_then(|l| parse_validators(l)) {
        header.validators = v;
    }
    // A synced-back body has lost the blank line after the Status line (the
    // text tidy-up collapses it away); the block parser needs it.
    let mut block: Vec<&str> = Vec::new();
    for (i, l) in lines[block_at..].iter().enumerate() {
        block.push(l);
        if i == 3 && lines.get(block_at + 4).is_some_and(|n| !n.is_empty()) {
            block.push("");
        }
    }
    let mut sources = parse_sources(&block.join("\n"))?;
    (sources.len() == 1).then(|| (header, sources.remove(0)))
}

pub fn recognise(body_html: &str) -> Option<SourceHeader> {
    parse_source_note(body_html).map(|(h, _)| h)
}

/// One HTML `<div>`: the line a derived note carries to point at its source.
pub fn derived_from_line(source_title: &str, source_uuid: &str, header: &SourceHeader) -> String {
    let copy_date = header.fetched_at.get(..10).unwrap_or(&header.fetched_at);
    let line = format!("{DERIVED_PREFIX}{}]]{SEP}copy of {copy_date}{SEP}sha256 {}", note_slug(source_title, source_uuid), header.sha256);
    format!("<div>{}</div>", escape_html(&line))
}

fn parse_derived_line(line: &str) -> Option<DerivedFrom> {
    let (link_key, rest) = line.strip_prefix(DERIVED_PREFIX)?.split_once("]]")?;
    let mut parts = rest.strip_prefix(SEP)?.split(SEP);
    let copy_date = parts.next()?.strip_prefix("copy of ")?.to_string();
    let sha256 = parts.next()?.strip_prefix("sha256 ")?.to_string();
    (parts.next().is_none() && !link_key.is_empty() && is_date(&copy_date) && is_hex64(&sha256))
        .then(|| DerivedFrom { link_key: link_key.to_string(), copy_date, sha256 })
}

/// Every `Derived from` line, in order. Reading stops at an inline Source
/// block: what is inside it is fetched text, not the note's own lines.
pub fn parse_derived_from(body_html: &str) -> Vec<DerivedFrom> {
    html_to_text(body_html, true)
        .split('\n')
        .take_while(|l| !l.starts_with(HEADER_PREFIX))
        .filter_map(parse_derived_line)
        .collect()
}

pub fn is_derived(body_html: &str) -> bool {
    !parse_derived_from(body_html).is_empty()
}

/// The URLs of a derived note's `## Sources` list (as `assemble_ingest_body`
/// writes it), in order, leaving out the sources whose fetch failed — those
/// have no source note and so no `Derived from` line, so the k-th URL pairs
/// with the k-th line. Read from the links' `href`: a body that lost its
/// markup in a sync gives none.
pub fn derived_source_urls(body_html: &str) -> Vec<String> {
    let Some((_, after)) = body_html.split_once("<h2>Sources</h2>") else { return Vec::new() };
    let list = after.split("</ul>").next().unwrap_or("");
    list.split("<li>")
        .skip(1)
        .filter_map(|li| {
            let li = li.split("</li>").next().unwrap_or(li);
            let href = li.split_once("href=\"")?.1.split('"').next()?;
            let status = li.rsplit_once("</a> — ").map_or("", |(_, s)| s);
            let failed = crate::db::decode_html_entities(status).starts_with("failed: ");
            (!failed).then(|| crate::db::decode_html_entities(href).into_owned())
        })
        .collect()
}

/// The source note a `Derived from` line names, for Redo and jodd-mcp alike.
/// First through `resolve_note_link`, which follows a rekey alias (gotcha
/// #16). Failing that — a Microsoft note is rekeyed at CREATE and the alias
/// lives only on the device that pushed it (Ruling 14) — the saved copy of
/// `url` (the line's Sources-list URL), accepted only if its header's sha256
/// is the line's: the very copy the note was derived from, not a later one.
/// The link itself is never rewritten.
pub fn resolve_derived_from(
    db: &crate::db::Db,
    account_id: &str,
    link: &DerivedFrom,
    url: Option<&str>,
) -> rusqlite::Result<Option<crate::db::CachedNote>> {
    if let Some(found) = db.resolve_note_link(account_id, &link.link_key)? {
        return Ok(Some(found));
    }
    let Some(url) = url else { return Ok(None) };
    Ok(db
        .source_note_for_key(account_id, &source_key(url))?
        .filter(|n| recognise(&n.body_html).is_some_and(|h| h.sha256 == link.sha256)))
}

/// What iCloud/SSH give back: the same lines, no details/summary/pre.
#[cfg(test)]
pub(crate) fn plain_form(html: &str) -> String {
    html_to_text(html, true).lines().map(|l| format!("<div>{}</div>", escape_html(l))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{FetchStatus, SourceKind};

    fn header() -> SourceHeader {
        SourceHeader {
            fetched_at: "2026-11-19 14:05".into(),
            sha256: content_hash("hello"),
            with: vec![FetchOpt::Comments],
            validators: Validators { etag: Some("W/\"abc\"".into()), last_modified: Some("Wed, 18 Nov 2026 10:00:00 GMT".into()) },
        }
    }

    fn src() -> FetchedSource {
        FetchedSource {
            url: "https://example.com/a?x=1".into(),
            kind: SourceKind::Web,
            title: Some("The Page <b> & more".into()),
            text: "first line\nsecond <tag> & line\nlast line".into(),
            status: FetchStatus::Ok,
        }
    }

    fn plain(html: &str) -> String {
        plain_form(html)
    }

    #[test]
    fn source_key_rules() {
        assert_eq!(source_key("https://youtu.be/abcDEF12345"), "youtube:abcDEF12345");
        assert_eq!(source_key("https://www.youtube.com/watch?v=abcDEF12345&t=3"), "youtube:abcDEF12345");
        assert_eq!(source_key("https://example.com/a?x=1#frag"), "https://example.com/a?x=1");
        let x = "https://x.com/u/status/1?s=20";
        assert_eq!(source_key(x), urls::canonical_url(x));
    }

    #[test]
    fn hash_ignores_pasted_comments_and_covers_text_past_the_cap() {
        let a = "body".to_string();
        let b = format!("body\n\n{}\nmine", PASTED_HEADER);
        assert_eq!(content_hash(&a), content_hash(&b));
        let cap = crate::ingest::stored::MAX_STORED_CHARS_PER_SOURCE;
        let long1 = format!("{}X", "a".repeat(cap));
        let long2 = format!("{}Y", "a".repeat(cap));
        assert_ne!(content_hash(&long1), content_hash(&long2));
        assert_eq!(content_hash("x").len(), 64);
    }

    #[test]
    fn render_then_recognise_and_parse_round_trip() {
        let (src, header) = (src(), header());
        let (title, html) = render_source_note(&src, &header);
        assert_eq!(title, "Source — The Page <b> & more");
        assert!(html.contains("sha256 ") && html.contains("etag W/&quot;abc&quot;"), "{html}");
        assert_eq!(recognise(&html), Some(header.clone()));
        let (h, s) = parse_source_note(&html).unwrap();
        assert_eq!(h, header);
        assert_eq!(s.url, src.url);
        assert_eq!(s.text, src.text);
        assert_eq!(s.title, src.title);
        // No title: the display URL names it.
        let (t2, _) = render_source_note(&FetchedSource { title: None, ..src.clone() }, &header);
        assert_eq!(t2, format!("Source — {}", urls::display_url(&src.url)));
    }

    #[test]
    fn blank_lines_and_indentation_survive_the_real_form() {
        let mut s = src();
        s.text = "a\n\n  indented\n\n\nb".into();
        assert_eq!(parse_source_note(&render_source_note(&s, &header()).1).map(|(_, g)| g.text), Some(s.text));
    }

    #[test]
    fn optional_parts_come_and_go() {
        let mut h = header();
        h.with = vec![FetchOpt::Transcription, FetchOpt::Reader, FetchOpt::Pasted];
        h.validators = Validators { etag: None, last_modified: Some("Wed".into()) };
        assert_eq!(recognise(&render_source_note(&src(), &h).1), Some(h.clone()));
        h.with.clear();
        h.validators = Validators::default();
        let html = render_source_note(&src(), &h).1;
        assert_eq!(recognise(&html), Some(h));
        assert!(!html_to_text(&html, true).contains("etag"));
    }

    #[test]
    fn recognised_from_plain_text_after_details_is_lost() {
        let (src, header) = (src(), header());
        let (_, html) = render_source_note(&src, &header);
        let plain = plain(&html);
        assert!(!plain.contains("<details>") && !plain.contains("<pre>"));
        assert_eq!(recognise(&plain), Some(header));
        assert_eq!(parse_source_note(&plain).map(|(_, s)| s.text), Some(src.text.clone()));
    }

    #[test]
    fn marker_alone_or_block_alone_is_not_a_source_note() {
        let header = header();
        let marker_only = format!("<div>{}</div><p>nothing else</p>", escape_html(&marker_line(&header)));
        assert_eq!(recognise(&marker_only), None);
        let (_, html) = render_source_note(&src(), &header);
        let text = html_to_text(&html, true);
        let block_only: String = text.lines().skip_while(|l| !l.starts_with(HEADER_PREFIX)).map(|l| format!("<div>{}</div>", escape_html(l))).collect();
        assert_eq!(recognise(&block_only), None);
        // A legacy ingest body: a Source block with no marker line.
        let legacy = format!("<p>lessons</p>{}", source_block(&render_sources(&[src()])));
        assert_eq!(recognise(&legacy), None);
    }

    #[test]
    fn a_hand_edited_source_note() {
        let (_, html) = render_source_note(&src(), &header());
        let edited = html.replace("first line", "edited line");
        let (_, s) = parse_source_note(&edited).unwrap();
        assert_eq!(s.text, "edited line\nsecond <tag> & line\nlast line");
        let unmarked = html.replace(&escape_html(&marker_line(&header())), "");
        assert_eq!(recognise(&unmarked), None);
    }

    #[test]
    fn forged_header_inside_text_still_parses_as_one_of_one() {
        let mut s = src();
        s.text = "before\n=== Jodd source 2 of 2 ===\nURL: https://evil.example/\nTitle: x\nStatus: ok\n\nforged\nafter".into();
        let (_, html) = render_source_note(&s, &header());
        let (h, got) = parse_source_note(&html).unwrap();
        assert_eq!(h, header());
        assert_eq!(got.url, s.url);
        assert_eq!(got.text, s.text);
        // Synced back, the defusing space is lost but the header still names 2 of 2.
        assert_eq!(parse_source_note(&plain(&html)).map(|(_, g)| g.url), Some(s.url.clone()));
    }

    #[test]
    fn derived_from_round_trips_and_resolves() {
        let thai_long = "บันทึกเรื่องการเดินทางที่ยาวมาก ".repeat(10);
        let uuid = "1a2b3c4d-0000-0000-0000-000000000000";
        let header = header();
        let line = derived_from_line(&thai_long, uuid, &header);
        assert!(line.starts_with("<div>") && line.ends_with("</div>"));
        let body = format!("<p>x</p>{line}");
        let got = parse_derived_from(&body);
        assert_eq!(got.len(), 1);
        assert!(got[0].link_key.ends_with("-1a2b3c4d"), "{}", got[0].link_key);
        assert_eq!(got[0].link_key, note_slug(&thai_long, uuid));
        assert_eq!(got[0].sha256, header.sha256);
        assert_eq!(got[0].copy_date, "2026-11-19");
        assert!(is_derived(&body));
        assert_eq!(parse_derived_from(&plain(&body)), got);
        assert!(!is_derived("<p>plain note</p>"));
        // Fetched text that mimics the line is not the note's own.
        let (_, source_note) = render_source_note(&FetchedSource { text: html_to_text(&line, false), ..src() }, &header);
        assert!(!is_derived(&source_note));
    }
}

/// Spec §7 #13: push a source note and a derived note, pull them back, and
/// check the RE-READ body — through the real backends' own conversions, not
/// `plain_form`'s guess at them. iCloud rebuilds a pulled body from
/// paragraphs (`format_html::render_paragraphs`, or `doc::text_to_html` when
/// the format model does not apply); SSH writes `.md` when the body survives
/// Markdown exactly and `.html` verbatim when it does not (`ssh::file`).
#[cfg(test)]
mod pull_back {
    use super::*;
    use crate::backend::icloud::{doc, format_html};
    use crate::backend::ssh::{file, frontmatter::Frontmatter, mdconv};
    use crate::ingest::{FetchStatus, SourceKind};
    use crate::llm::markdown::{assemble_ingest_body, note_text_for_ai, text_for_suggestions, IngestTail, SourceLine};
    use crate::llm::provider::ExtractEnvelope;
    use crate::test_support::{note, temp_db};

    const ACCT: &str = "icloud:test@example.com";
    /// The SSH legs' account: the derived note is shaped as Jodd writes it
    /// there (`TagModel::BodyText`, hashtags), not as for iCloud.
    const SSH_ACCT: &str = "ssh:test@example.com";
    const SOURCE_UUID: &str = "5f3a9c1e-7b2d-4e8a-9c01-2f6d8a4b1e33";
    const DERIVED_UUID: &str = "0a1b2c3d-0000-4000-8000-000000000001";

    /// Words that appear only in the fetched text, never in the header lines.
    const ONLY_IN_TEXT: [&str; 4] = ["ampersand", "indented", "ไทยบรรทัด", "closing"];

    /// Text that stresses every conversion: markup characters, blank lines,
    /// leading spaces, lines that look like Markdown, a bare URL and Thai.
    fn fetched() -> FetchedSource {
        FetchedSource {
            url: "https://example.com/post?id=7&lang=th".into(),
            kind: SourceKind::Web,
            title: Some("A <page> & its title".into()),
            text: [
                "Opening line with <tag> & ampersand",
                "",
                "  indented two spaces",
                "- looks like a list",
                "* starred *word*",
                "1. numbered",
                "# not a heading",
                "> not a quote",
                "https://example.com/bare",
                "ไทยบรรทัด ภาษาไทย",
                "",
                "",
                "closing line",
            ]
            .join("\n"),
            status: FetchStatus::Ok,
        }
    }

    fn header(with: Vec<FetchOpt>, validators: Validators) -> SourceHeader {
        SourceHeader { fetched_at: "2026-11-19 14:02".into(), sha256: content_hash(&fetched().text), with, validators }
    }

    fn full_header() -> SourceHeader {
        header(
            vec![FetchOpt::Reader, FetchOpt::Comments],
            Validators { etag: Some("W/\"5e-abc\"".into()), last_modified: Some("Tue, 18 Nov 2026 09:00:00 GMT".into()) },
        )
    }

    fn icloud_formatted(html: &str) -> String {
        format_html::render_paragraphs(&format_html::parse_editor_html(html).paragraphs, &[])
    }

    fn icloud_plain(html: &str) -> String {
        doc::text_to_html(&format_html::parse_editor_html(html).text)
    }

    /// `ssh::file::encode` then `decode`: what the SSH vertical writes and
    /// reads back, whichever of `.md` / `.html` it chose.
    fn ssh_file(title: &str, html: &str) -> (file::Ext, String) {
        let fm = Frontmatter { uuid: Some("u".into()), title: Some(title.into()), created: None, pinned: false, extra: vec![] };
        let (ext, text) = file::encode(&fm, title, html);
        let back = file::decode(&format!("Notes/n.{}", ext.as_str()), text.as_bytes()).expect("decodes");
        assert_eq!(back.title, title);
        (ext, back.body_html)
    }

    /// The Markdown leg on its own, as the brief names it. `None` when the
    /// body does not survive Markdown — SSH then writes `.html` instead.
    fn ssh_markdown(html: &str) -> Option<String> {
        mdconv::html_to_md(html).map(|md| mdconv::md_to_html(&md))
    }

    /// Every promise §7 #13 makes about a source note, on one re-read body.
    fn assert_source_note_survives(how: &str, body: &str, src: &FetchedSource, h: &SourceHeader) {
        assert_source_note_survives_as(how, body, src, h, &src.text)
    }

    /// The text iCloud's PLAIN read path gives back (`doc::text_to_html`, taken
    /// when the note's text holds `\r` or U+2029 or its format runs do not
    /// decode): every line becomes a `<div>`, so `html_to_text` drops a
    /// line's leading spaces and keeps at most one blank line in a row, as a
    /// browser shows it. Pinned exactly, so a change either way shows here.
    const PLAIN_PATH_TEXT: &str = "Opening line with <tag> & ampersand\n\nindented two spaces\n- looks like a list\n* starred *word*\n1. numbered\n# not a heading\n> not a quote\nhttps://example.com/bare\nไทยบรรทัด ภาษาไทย\n\nclosing line";

    fn assert_source_note_survives_as(how: &str, body: &str, src: &FetchedSource, h: &SourceHeader, want_text: &str) {
        assert!((!body.contains("<details") && !body.contains("<summary")) || how.starts_with("ssh .html"), "{how}: markup expected gone\n{body}");
        // Recognised.
        assert_eq!(recognise(body).as_ref(), Some(h), "{how}: not recognised\n{body}");
        // Parsed by Redo: the same text, URL and title.
        let (got_h, got) = parse_source_note(body).unwrap_or_else(|| panic!("{how}: does not parse\n{body}"));
        assert_eq!(&got_h, h, "{how}");
        assert_eq!(got.text, want_text, "{how}: text changed\n{body}");
        assert_eq!(got.url, src.url, "{how}");
        assert_eq!(got.title, src.title, "{how}");
        // Excluded: the header lines stay, the fetched text does not.
        let shown = html_to_text(body, false);
        assert!(shown.contains("Jodd source · fetched 2026-11-19 14:02 · sha256 "), "{how}: {shown}");
        assert!(shown.ends_with("[Source text omitted — read_note with include_source: true]"), "{how}: {shown}");
        let for_ai = note_text_for_ai(body).into_owned();
        for cut in [shown.as_str(), for_ai.as_str(), text_for_suggestions(body)] {
            for w in ONLY_IN_TEXT {
                assert!(!cut.contains(w), "{how}: {w:?} leaked\n{cut}");
            }
        }
        // Found by the lookup, so a second ingest reuses it.
        let db = temp_db();
        let mut n = note(ACCT, SOURCE_UUID).label("Notes/__Sources__").build();
        n.title = "Source — A <page> & its title".into();
        n.body_html = body.to_string();
        db.insert_local_new(&n, &crate::provenance::Actor::test()).unwrap();
        let found = db.source_note_for_key(ACCT, &source_key(&src.url)).unwrap();
        assert_eq!(found.map(|n| n.uuid).as_deref(), Some(SOURCE_UUID), "{how}: lookup missed it\n{body}");
    }

    #[test]
    fn a_source_note_survives_an_icloud_pull() {
        let src = fetched();
        for h in [full_header(), header(vec![], Validators::default())] {
            let (_, html) = render_source_note(&src, &h);
            assert_source_note_survives("icloud formatted", &icloud_formatted(&html), &src, &h);
            assert_source_note_survives_as("icloud plain", &icloud_plain(&html), &src, &h, PLAIN_PATH_TEXT);
            // A second round trip (pulled, edited elsewhere, pulled again).
            assert_source_note_survives("icloud twice", &icloud_formatted(&icloud_formatted(&html)), &src, &h);
        }
    }

    #[test]
    fn a_source_note_survives_an_ssh_pull() {
        let src = fetched();
        for h in [full_header(), header(vec![FetchOpt::Transcription], Validators { etag: None, last_modified: Some("Wed".into()) })] {
            let (title, html) = render_source_note(&src, &h);
            let (ext, body) = ssh_file(&title, &html);
            assert_source_note_survives(&format!("ssh .{}", ext.as_str()), &body, &src, &h);
            // The Markdown leg, whenever it is taken: on the body as written,
            // and on the markup-free form an iCloud-style edit leaves.
            for (how, input) in [("ssh md (as written)", html.clone()), ("ssh md (plain)", icloud_formatted(&html))] {
                if let Some(back) = ssh_markdown(&input) {
                    assert_source_note_survives(how, &back, &src, &h);
                }
            }
        }
    }

    fn derived_body(acct: &str, source_title: &str, h: &SourceHeader) -> String {
        let envelope = ExtractEnvelope {
            title: None,
            lessons_markdown: "- a key point\n- another".into(),
            meta_lessons_markdown: None,
            tags: vec!["research".into()],
            confidence: None,
        };
        let lines = vec![
            SourceLine { url: fetched().url, title: fetched().title, status: "ok".into() },
            SourceLine { url: "https://gone.example/".into(), title: None, status: "failed: HTTP 404".into() },
        ];
        let tail = IngestTail::Derived(vec![derived_from_line(source_title, SOURCE_UUID, h)]);
        assemble_ingest_body(&envelope, None, &lines, tail, crate::db::tag_model_for_account(acct))
    }

    /// The `Derived from` line, and Redo following it to the source note,
    /// with BOTH notes as a pull gives them back.
    fn assert_derived_survives(acct: &str, how: &str, derived: &str, source: &str, src: &FetchedSource, h: &SourceHeader, title: &str) {
        assert_derived_survives_as(acct, how, derived, source, h, title, &src.text)
    }

    #[allow(clippy::too_many_arguments)]
    fn assert_derived_survives_as(acct: &str, how: &str, derived: &str, source: &str, h: &SourceHeader, title: &str, want_text: &str) {
        let want = DerivedFrom { link_key: note_slug(title, SOURCE_UUID), copy_date: "2026-11-19".into(), sha256: h.sha256.clone() };
        assert_eq!(parse_derived_from(derived), vec![want], "{how}\n{derived}");
        assert!(is_derived(derived), "{how}");
        assert!(recognise(derived).is_none(), "{how}: a derived note is not a source note");
        let shown = html_to_text(derived, false);
        assert!(shown.contains(&format!("Derived from [[{}]] · copy of 2026-11-19 · sha256 {}", note_slug(title, SOURCE_UUID), h.sha256)), "{how}: {shown}");

        let db = temp_db();
        let mut s = note(acct, SOURCE_UUID).label("Notes/__Sources__").build();
        s.title = title.into();
        s.body_html = source.into();
        db.insert_local_new(&s, &crate::provenance::Actor::test()).unwrap();
        let mut d = note(acct, DERIVED_UUID).label("Notes/Research").build();
        d.title = "Key points".into();
        d.body_html = derived.into();
        db.insert_local_new(&d, &crate::provenance::Actor::test()).unwrap();
        let d = db.get(DERIVED_UUID, acct).unwrap().unwrap();
        match crate::ai_commands::plan_re_extract(&db, &crate::privacy::ai_scope::AiScope::default(), acct, &d) {
            Ok(crate::ai_commands::ReExtractPlan::Ingest(crate::ingest::run::IngestInput::SourceNotes(linked), _)) => {
                assert_eq!(linked.len(), 1, "{how}");
                assert_eq!(linked[0].uuid, SOURCE_UUID, "{how}");
                assert_eq!(&linked[0].header, h, "{how}");
                assert_eq!(linked[0].source.text, want_text, "{how}");
            }
            Ok(_) => panic!("{how}: Redo did not follow the Derived from line"),
            Err(e) => panic!("{how}: Redo refused: {e}"),
        }
    }

    #[test]
    fn a_derived_note_survives_an_icloud_pull() {
        let (src, h) = (fetched(), full_header());
        let (title, source) = render_source_note(&src, &h);
        let derived = derived_body(ACCT, &title, &h);
        assert_derived_survives(ACCT, "icloud formatted", &icloud_formatted(&derived), &icloud_formatted(&source), &src, &h, &title);
        assert_derived_survives_as(ACCT, "icloud plain", &icloud_plain(&derived), &icloud_plain(&source), &h, &title, PLAIN_PATH_TEXT);
    }

    #[test]
    fn a_derived_note_survives_an_ssh_pull() {
        let (src, h) = (fetched(), full_header());
        let (title, source) = render_source_note(&src, &h);
        let derived = derived_body(SSH_ACCT, &title, &h);
        // Shaped as Jodd writes it for SSH: hashtags, not iCloud's Keywords line.
        assert!(derived.contains("#research") && !derived.contains("Keywords:"), "{derived}");
        let (_, source_back) = ssh_file(&title, &source);
        let (ext, derived_back) = ssh_file("Key points", &derived);
        assert_derived_survives(SSH_ACCT, &format!("ssh .{}", ext.as_str()), &derived_back, &source_back, &src, &h, &title);
        for (how, input) in [("ssh md (as written)", derived.clone()), ("ssh md (plain)", icloud_formatted(&derived))] {
            if let Some(back) = ssh_markdown(&input) {
                assert_derived_survives(SSH_ACCT, how, &back, &source_back, &src, &h, &title);
            }
        }
    }

    /// Which path each body takes on SSH, measured 2026-11-19: both notes are
    /// written as `.html` (`html_to_md` returns `None` for each as Jodd writes
    /// it), so on SSH they come back verbatim. Only a markup-free derived note passes the gate, and the
    /// tests above take that Markdown leg too. Pinned so a change in
    /// `html_to_md`'s gate shows up here rather than silently.
    #[test]
    fn which_ssh_file_each_note_becomes() {
        let (src, h) = (fetched(), full_header());
        let (title, source) = render_source_note(&src, &h);
        let derived = derived_body(SSH_ACCT, &title, &h);
        assert_eq!(ssh_file(&title, &source).0, file::Ext::Html);
        assert_eq!(ssh_file("Key points", &derived).0, file::Ext::Html);
        assert!(ssh_markdown(&source).is_none());
        assert!(ssh_markdown(&icloud_formatted(&source)).is_none());
        assert!(ssh_markdown(&derived).is_none());
        assert!(ssh_markdown(&icloud_formatted(&derived)).is_some());
    }
}
