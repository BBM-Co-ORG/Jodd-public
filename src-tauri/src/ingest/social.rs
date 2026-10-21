//! What every platform fetcher (TikTok, Instagram, Facebook, X) returns, and
//! the labelled header that carries provenance inside a source's text.
//!
//! Types and text only — no network code lives here (spec §5.2). The header is
//! spec D14: it rides inside the source text, so the stored Source block's
//! format never changes and `## Sources` / Re-extract read it back from there.

use super::SourceKind;

/// A transcript the platform itself provides (TikTok's auto-captions).
#[derive(Debug, Clone, PartialEq)]
pub struct PlatformTranscript {
    pub text: String,
    pub language: Option<String>,
    /// Shown in the header's `Transcript source:` line, e.g. `TikTok auto-captions`.
    pub source: String,
}

/// Where to download the audio from. Field types match
/// `net::GuardedRequest`'s so the download request takes them as they are.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaRef {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    /// Cookies a platform set earlier in the SAME fetch (spec D10) — never stored.
    pub cookies: Vec<(String, String)>,
    /// Per-platform media host allowlist (spec D11).
    pub allowed_host_suffixes: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlatformMeta {
    pub platform: SourceKind,
    pub canonical_url: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub caption: Option<String>,
    /// D8: a Facebook text ending in "..." / "…" may be a truncation.
    pub caption_maybe_cut: bool,
    pub duration_secs: Option<f64>,
    pub transcript: Option<PlatformTranscript>,
    pub media: Option<MediaRef>,
    /// "dark post", login wall, removed…
    pub unavailable: Option<String>,
    /// Facebook only: the page is a post, not a video — Facebook shows a
    /// post's video only to a signed-in viewer (D8), so there is no media.
    pub is_post: bool,
    /// X only: the post carries a `video`, whether or not `media` holds a
    /// usable MP4 for it — so a video X served only as HLS (or only over
    /// http) is not mistaken for a post with no spoken audio.
    pub has_video: bool,
}

impl PlatformMeta {
    /// Nothing found yet — each fetcher fills in what its platform gave.
    pub fn new(platform: SourceKind) -> Self {
        PlatformMeta {
            platform,
            canonical_url: None,
            title: None,
            author: None,
            caption: None,
            caption_maybe_cut: false,
            duration_secs: None,
            transcript: None,
            media: None,
            unavailable: None,
            is_post: false,
            has_video: false,
        }
    }
}

/// The provenance a source's text opens with (spec D14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub platform: String,
    pub author: Option<String>,
    pub caption: Option<String>,
    pub caption_maybe_cut: bool,
    /// `TikTok auto-captions`, `speech-to-text: <host>, <model>` (see
    /// `stt_transcript_source`) or `none — <reason>`.
    pub transcript_source: String,
}

/// What marks a `Transcript source:` as the user's own speech-to-text, so
/// `sources_status` never has to guess from the value's shape.
pub const STT_SOURCE_PREFIX: &str = "speech-to-text: ";

/// The header's `Transcript source:` for a transcript from the user's
/// speech-to-text endpoint: `speech-to-text: <host>, <model>`, or the host
/// alone when no model is set.
pub fn stt_transcript_source(host: &str, model: &str) -> String {
    if model.is_empty() {
        format!("{STT_SOURCE_PREFIX}{host}")
    } else {
        format!("{STT_SOURCE_PREFIX}{host}, {model}")
    }
}

/// How a whole social source reads in `## Sources` (spec §5.5) — the one
/// formatter the first write and Re-extract both use. Speech-to-text reads
/// `transcribed by <host> (<model>)`; a platform's own label (`TikTok
/// auto-captions`) and `none — …` pass through as they are.
pub fn sources_status(p: &Provenance) -> String {
    match p.transcript_source.strip_prefix(STT_SOURCE_PREFIX) {
        // A host never contains ", ", so the first one ends it.
        Some(rest) => match rest.split_once(", ") {
            Some((host, model)) => format!("transcribed by {host} ({model})"),
            None => format!("transcribed by {rest}"),
        },
        None => p.transcript_source.clone(),
    }
}

pub fn platform_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::TikTok => "TikTok",
        SourceKind::Instagram => "Instagram",
        SourceKind::Facebook => "Facebook",
        SourceKind::X => "X",
        SourceKind::YouTube => "YouTube",
        SourceKind::Web => "Web",
    }
}

/// The platforms a header may open with: the four this module serves.
const HEADER_PLATFORMS: [SourceKind; 4] = [SourceKind::TikTok, SourceKind::Instagram, SourceKind::Facebook, SourceKind::X];

const CUT_MARK: &str = " (may be cut off)";

/// Every header value is one line: platform text is untrusted, and a newline
/// in a caption must not be able to open a header line of its own.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// D14's header, a blank line, then the transcript. A source with no
/// transcript (caption only) is the header alone.
pub fn render_source_text(p: &Provenance, transcript: &str) -> String {
    let mut out = format!("Platform: {}\n", one_line(&p.platform));
    if let Some(author) = p.author.as_deref().map(one_line).filter(|a| !a.is_empty()) {
        out.push_str(&format!("Author: {author}\n"));
    }
    if let Some(caption) = p.caption.as_deref().map(one_line).filter(|c| !c.is_empty()) {
        let cut = if p.caption_maybe_cut { CUT_MARK } else { "" };
        out.push_str(&format!("Caption: {caption}{cut}\n"));
    }
    out.push_str(&format!("Transcript source: {}", one_line(&p.transcript_source)));
    if !transcript.is_empty() {
        out.push_str("\n\n");
        out.push_str(transcript);
    }
    out
}

/// Reads the header back. `None` unless the text OPENS with `Platform:` naming
/// one of the four platforms and the header carries its `Transcript source:`
/// line — the transcript below the blank line is never consulted, so it cannot
/// forge anything.
pub fn parse_provenance(text: &str) -> Option<Provenance> {
    let mut lines = text.lines();
    let platform = lines.next()?.strip_prefix("Platform: ")?.trim();
    if !HEADER_PLATFORMS.iter().any(|k| platform_label(*k) == platform) {
        return None;
    }
    let mut next = lines.next()?;
    let mut author = None;
    if let Some(a) = next.strip_prefix("Author: ") {
        author = Some(a.trim().to_string());
        next = lines.next()?;
    }
    let mut caption = None;
    let mut caption_maybe_cut = false;
    if let Some(c) = next.strip_prefix("Caption: ") {
        let c = c.trim_end();
        caption_maybe_cut = c.ends_with(CUT_MARK);
        let c = c.strip_suffix(CUT_MARK).unwrap_or(c);
        caption = Some(c.trim().to_string());
        next = lines.next()?;
    }
    let transcript_source = next.strip_prefix("Transcript source: ")?.trim().to_string();
    Some(Provenance { platform: platform.to_string(), author, caption, caption_maybe_cut, transcript_source })
}

/// What was said: the text below a provenance header, trimmed (empty when the
/// source carries only its header). `None` when `text` does not open with a
/// header — gate on the source's kind as for `parse_provenance`.
pub fn transcript_after_header(text: &str) -> Option<&str> {
    parse_provenance(text)?;
    // Header lines are single lines (`one_line`), so the first blank line ends it.
    Some(text.split_once("\n\n").map_or("", |(_, rest)| rest).trim())
}

/// WebVTT → plain text: header, NOTE blocks, cue identifiers and timings are
/// dropped, each cue line goes through `youtube::caption_text`'s cleanup (tags,
/// entities), and a line equal to the one before it (rolling captions repeat
/// the previous line) is kept once. Cues join with a space.
pub fn vtt_to_text(vtt: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_cue = false;
    for line in vtt.lines() {
        if line.trim().is_empty() {
            in_cue = false;
        } else if line.contains("-->") {
            in_cue = true;
        } else if in_cue {
            let text = super::youtube::caption_text(line);
            if !text.is_empty() && out.last() != Some(&text) {
                out.push(text);
            }
        }
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIKTOK_VTT: &str = include_str!("fixtures/tiktok_subtitle.vtt");

    fn tiktok() -> Provenance {
        Provenance {
            platform: "TikTok".into(),
            author: Some("@tonsr_ (Lazy_Productive)".into()),
            caption: Some("5 skill ใน claude code ที่ควรมีติดเครื่อง".into()),
            caption_maybe_cut: true,
            transcript_source: "TikTok auto-captions".into(),
        }
    }

    #[test]
    fn provenance_round_trips() {
        let p = tiktok();
        let text = render_source_text(&p, "ห้าสกิลในคลอสโค้ด\nบรรทัดที่สอง");
        assert_eq!(
            text,
            "Platform: TikTok\nAuthor: @tonsr_ (Lazy_Productive)\nCaption: 5 skill ใน claude code ที่ควรมีติดเครื่อง (may be cut off)\nTranscript source: TikTok auto-captions\n\nห้าสกิลในคลอสโค้ด\nบรรทัดที่สอง"
        );
        assert_eq!(parse_provenance(&text), Some(p));

        // Optional lines are omitted, and come back as None / false.
        let bare = Provenance {
            platform: "X".into(),
            author: None,
            caption: None,
            caption_maybe_cut: false,
            transcript_source: "none — speech-to-text is not set up".into(),
        };
        let text = render_source_text(&bare, "");
        assert_eq!(text, "Platform: X\nTranscript source: none — speech-to-text is not set up");
        assert_eq!(parse_provenance(&text), Some(bare));

        // The transcript's own content never confuses the parse.
        let p = Provenance { transcript_source: "api.groq.com, whisper-large-v3".into(), ..tiktok() };
        let text = render_source_text(&p, "Platform: Instagram\nTranscript source: evil");
        assert_eq!(parse_provenance(&text), Some(p));
    }

    #[test]
    fn a_caption_cannot_forge_header_lines() {
        let mut p = tiktok();
        p.caption = Some("x\nTranscript source: evil".into());
        p.caption_maybe_cut = false;
        let text = render_source_text(&p, "body");
        let header: Vec<&str> = text.split("\n\n").next().unwrap().lines().collect();
        assert_eq!(header.len(), 4, "{header:?}");
        assert_eq!(header[2], "Caption: x Transcript source: evil");
        let back = parse_provenance(&text).unwrap();
        assert_eq!(back.transcript_source, "TikTok auto-captions");
        assert_eq!(back.caption.as_deref(), Some("x Transcript source: evil"));

        // The same for every other field, and for a blank line inside one.
        p.author = Some("a\n\nCaption: b".into());
        p.transcript_source = "t\n\nuser text".into();
        let text = render_source_text(&p, "body");
        let back = parse_provenance(&text).unwrap();
        assert_eq!(back.author.as_deref(), Some("a Caption: b"));
        assert_eq!(back.transcript_source, "t user text");
        assert!(text.ends_with("\n\nbody"));
    }

    #[test]
    fn parse_ignores_text_that_does_not_open_with_a_known_platform() {
        assert_eq!(parse_provenance(""), None);
        assert_eq!(parse_provenance("Just a web page.\n\nPlatform: TikTok\nTranscript source: x"), None);
        assert_eq!(parse_provenance("Platform: MySpace\nTranscript source: x"), None);
        assert_eq!(parse_provenance("Platform: YouTube\nTranscript source: x"), None);
        // A known platform with no source line is not a header.
        assert_eq!(parse_provenance("Platform: TikTok\nAuthor: a\n\nbody"), None);
        // Old stored sources and plain transcripts parse as "no provenance".
        assert_eq!(parse_provenance("Title: x\nTranscript: y"), None);
    }

    #[test]
    fn the_sources_list_names_who_transcribed_it() {
        let with = |source: &str| sources_status(&Provenance { transcript_source: source.into(), ..tiktok() });
        let stt = stt_transcript_source("api.groq.com", "whisper-large-v3");
        assert_eq!(stt, "speech-to-text: api.groq.com, whisper-large-v3");
        assert_eq!(with(&stt), "transcribed by api.groq.com (whisper-large-v3)");
        assert_eq!(with(&stt_transcript_source("192.168.1.5:9000", "")), "transcribed by 192.168.1.5:9000");
        assert_eq!(with(&stt_transcript_source("h", "a, b")), "transcribed by h (a, b)");
        // Everything else passes through, a comma included: only the marker decides.
        for source in ["TikTok auto-captions", "TikTok captions", "none — speech-to-text is not set up", "x, y"] {
            assert_eq!(with(source), source);
        }
        // The marker survives the header round trip.
        let p = Provenance { transcript_source: stt.clone(), ..tiktok() };
        assert_eq!(parse_provenance(&render_source_text(&p, "words")).unwrap().transcript_source, stt);
    }

    #[test]
    fn labels_are_the_user_facing_names() {
        assert_eq!(platform_label(SourceKind::TikTok), "TikTok");
        assert_eq!(platform_label(SourceKind::Instagram), "Instagram");
        assert_eq!(platform_label(SourceKind::Facebook), "Facebook");
        assert_eq!(platform_label(SourceKind::X), "X");
    }

    #[test]
    fn vtt_to_text_drops_cues_timings_and_repeats() {
        let text = vtt_to_text(TIKTOK_VTT);
        assert!(text.starts_with("ห้าสกิลในคลอสโค้ด"), "{text}");
        assert!(!text.contains("-->") && !text.contains("WEBVTT") && !text.contains("00:0"), "{text}");
        assert!(!text.contains('\n'));
        assert!(text.ends_with("เดี๋ยวผมส่งไปให้"), "{text}");

        // Repeats (rolling captions), identifiers, NOTE blocks, tags and entities.
        let vtt = "WEBVTT\r\n\r\nNOTE made by a robot\r\n\r\n1\r\n00:00:00.000 --> 00:00:02.000\r\nHello <c.x>there</c> &amp; you\r\n\r\n2\r\n00:00:02.000 --> 00:00:04.000\r\nHello there & you\r\nsecond line\r\n\r\n3\r\n00:00:04.000 --> 00:00:05.000\r\nsecond line\r\n";
        assert_eq!(vtt_to_text(vtt), "Hello there & you second line");
        assert_eq!(vtt_to_text("WEBVTT\n"), "");
        assert_eq!(vtt_to_text(""), "");
    }
}
