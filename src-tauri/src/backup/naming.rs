//! Names inside a bundle and at the destination: account slugs, version
//! stamps, folder directories and collision-free note file names.

use chrono::{DateTime, NaiveDateTime};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

const STAMP_FORMAT: &str = "%Y-%m-%dT%H-%M-%SZ";
/// Longest file or directory name stem, in bytes, before any suffix.
const MAX_NAME_BYTES: usize = 120;
const WINDOWS_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// One path segment made safe on every platform Jodd runs on: unsafe
/// characters become `_`, trailing dots and spaces go (Windows drops them),
/// an empty result is `Untitled`, a Windows-reserved stem gets a `_`
/// suffix, and the result is cut to 120 bytes on a char boundary.
pub(super) fn sanitize_segment(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let mut name = truncate_bytes(mapped.trim().trim_end_matches(['.', ' ']), MAX_NAME_BYTES)
        .trim_end_matches(['.', ' '])
        .to_string();
    if name.is_empty() {
        name = "Untitled".to_string();
    }
    let stem = name.split('.').next().unwrap_or("");
    if WINDOWS_RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        name.insert_str(stem.len(), "_");
    }
    name
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The account's folder name at a destination (a persisted format: existing
/// backups are found by it). Unsafe characters become `_`. Every non-ASCII
/// character does too, so two ids that differ only there (two Thai-named
/// vaults of one length) would share a folder: such an id also gets `-` and
/// the first 8 hex digits of its sha256. An ASCII-only id has no suffix.
pub fn account_slug(account_id: &str) -> String {
    let mut slug: String = account_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !account_id.is_ascii() {
        let digest = Sha256::digest(account_id.as_bytes());
        slug.push('-');
        slug.extend(digest.iter().take(4).map(|b| format!("{b:02x}")));
    }
    slug
}

/// UTC `YYYY-MM-DDTHH-MM-SSZ`; sub-second precision is dropped.
pub fn version_stamp(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format(STAMP_FORMAT)
        .to_string()
}

pub fn parse_stamp(s: &str) -> Option<i64> {
    let parsed = NaiveDateTime::parse_from_str(s, STAMP_FORMAT).ok()?;
    Some(parsed.and_utc().timestamp_millis())
}

/// A folder label (`Notes/Work/2026`) as a directory path, each `/` segment
/// sanitised on its own.
pub fn dir_for_folder(label: &str) -> String {
    label
        .split('/')
        .map(sanitize_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn fold(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

#[derive(Default)]
pub struct NameAllocator {
    taken: HashSet<(String, String)>,
}

impl NameAllocator {
    pub fn new() -> Self {
        Self::default()
    }

    /// `dir/<name>.md`, unique within `dir` ignoring case and Unicode
    /// normalisation (the destination may be case-insensitive or NFD-folding).
    /// `index` and `log` are the bundle's own files, so a note by that title
    /// becomes `index (note)` / `log (note)`.
    pub fn file_for(&mut self, dir: &str, title: &str) -> String {
        let mut base = sanitize_segment(title);
        if base.eq_ignore_ascii_case("index") || base.eq_ignore_ascii_case("log") {
            base.push_str(" (note)");
        }
        let mut name = base.clone();
        let mut n = 1;
        while !self.taken.insert((dir.to_string(), fold(&name))) {
            n += 1;
            name = format!("{base} ({n})");
        }
        format!("{dir}/{name}.md")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_round_trips() {
        let s = version_stamp(1_761_210_900_000);
        assert_eq!(s, "2025-10-23T09-15-00Z");
        assert_eq!(parse_stamp(&s), Some(1_761_210_900_000));
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(
            account_slug("gmail:kaiwan@bbmedia.co.th"),
            "gmail_kaiwan@bbmedia.co.th"
        );
        assert_eq!(account_slug("ssh:me@host/vault"), "ssh_me@host_vault");
    }

    #[test]
    fn non_ascii_ids_get_a_hash_so_they_cannot_collide() {
        // Every Thai letter becomes `_`, so two vaults of equal length would
        // share a folder without the id's hash.
        let a = account_slug("localfs:/บันทึก");
        let b = account_slug("localfs:/ทดสอบก");
        assert_ne!(a, b);
        assert!(a.starts_with("localfs__"), "{a}");
        let hex = a.rsplit('-').next().unwrap();
        assert_eq!(hex.len(), 8, "{a}");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{a}");
        assert_eq!(account_slug("localfs:/บันทึก"), a, "stable");
        assert!(a.chars().all(|c| c.is_ascii()), "{a}");
    }

    #[test]
    fn names_collide_case_insensitively_and_by_nfc() {
        let mut a = NameAllocator::new();
        assert_eq!(a.file_for("Notes", "Note"), "Notes/Note.md");
        assert_eq!(a.file_for("Notes", "note"), "Notes/note (2).md");
        let nfd = "e\u{301}";
        let nfc = "\u{e9}";
        let first = a.file_for("Notes", nfc);
        let second = a.file_for("Notes", nfd);
        assert_ne!(first.to_lowercase(), second.to_lowercase());
        assert_eq!(first, "Notes/\u{e9}.md");
        assert_eq!(second, "Notes/e\u{301} (2).md");
        // The same title in another directory is independent.
        assert_eq!(a.file_for("Other", "Note"), "Other/Note.md");
    }

    #[test]
    fn directories_are_sanitised_per_segment() {
        assert_eq!(dir_for_folder("Notes/Work: 2026/nul"), "Notes/Work_ 2026/nul_");
        assert_eq!(dir_for_folder("Notes/../x"), "Notes/Untitled/x");
    }

    #[test]
    fn reserved_empty_and_unsafe_titles() {
        let mut a = NameAllocator::new();
        assert_eq!(a.file_for("Notes", "index"), "Notes/index (note).md");
        assert_eq!(a.file_for("Notes", "log"), "Notes/log (note).md");
        assert_eq!(a.file_for("Notes", ""), "Notes/Untitled.md");
        assert_eq!(a.file_for("Notes", "a/b:c*?"), "Notes/a_b_c__.md");
        assert_eq!(a.file_for("Notes", "CON"), "Notes/CON_.md");
        assert_eq!(a.file_for("Notes", "บันทึก"), "Notes/บันทึก.md");
        assert!(
            a.file_for("Notes", &"ก".repeat(200)).len()
                <= "Notes/".len() + 120 + ".md".len() + 8
        );
    }
}
