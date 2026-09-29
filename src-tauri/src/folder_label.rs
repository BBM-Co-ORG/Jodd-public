//! Folder-label syntax — the one place that decides whether a `Notes/…`
//! label string is safe.
//!
//! **Why this is its own module and not a helper inside `lib.rs`.** A folder
//! label is not one thing. To `db.rs` it is an opaque primary-key string; to
//! the Gmail vertical it is a literal label name; to
//! `LocalFsVertical::folder_path` it is a **filesystem path**, `Path::join`ed
//! onto the vault root with no normalization. The MCP write tools'
//! allowlist (`jodd-mcp/src/scope.rs`) is a pure string-prefix test over the
//! same strings, so a label containing `..` was simultaneously *inside* the
//! allowlist and *outside* the vault root — the traversal escape this module
//! exists to close. Three layers, three meanings, one syntax; the syntax
//! therefore lives above all three, beside `paths.rs` (the other filesystem
//! seam) rather than inside any one of them.
//!
//! Two entry points, deliberately split:
//!
//! - [`validate_label_segment`] — one path component. The shared safety core.
//!   `lib.rs`'s `validate_folder_segment` wraps it and adds the app-only
//!   `__name__` reservation (gotcha #4), which the MCP path must NOT inherit:
//!   an agent creating `Notes/__Claude__/…` is the intended use.
//! - [`validate_label_path`] — a whole `Notes/…` label. Every writer that
//!   accepts a full path from outside (today: `do_create_note` and
//!   `do_create_folder` in jodd-mcp) must call this, and only this, so a
//!   future third writer inherits the rules instead of re-deriving them.

/// Byte cap on one path segment. Matches the pre-existing app-side limit,
/// which was chosen for Gmail's label-name limit and is comfortably under
/// every filesystem's 255-byte component limit. Bytes, not chars, because
/// both of those limits are byte limits — Thai labels are correspondingly
/// shorter in characters, which is the safe direction to err in.
pub const MAX_SEGMENT_BYTES: usize = 200;

/// Byte cap on a whole `Notes/…` label. Well under the 4096-byte path limit
/// on Linux and the 32k extended-path limit on Windows, and far past any
/// folder tree a human will build by hand.
pub const MAX_LABEL_BYTES: usize = 1000;

/// Byte cap on a note title. It becomes the `Subject:` header of an RFC822
/// message; long subjects get folded and mangled by intermediaries.
pub const MAX_TITLE_BYTES: usize = 500;

/// The root every Jodd folder label hangs off. Apple Notes uses this literal
/// name — see gotcha #9 on why `notes_label` does not reach the folder side.
pub const NOTES_ROOT: &str = "Notes";

/// Is this segment shaped like a Windows drive prefix (`C:`, `d:\…`)?
///
/// On Windows, `Path::join` does not concatenate a component carrying a
/// prefix — it *replaces* the whole path with it. `root.join("D:evil")`
/// yields `D:evil`, an escape that has nothing to do with `..`. Exactly one
/// ASCII letter followed by `:` is what the platform parses as a prefix, so
/// this check is narrow on purpose: a folder genuinely named
/// `Meeting: Monday` has no prefix and stays legal.
fn looks_like_windows_drive(seg: &str) -> bool {
    let b = seg.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Is this segment nothing but dots and whitespace (`.`, `..`, `...`,
/// `" .. "`, `".. ."`)?
///
/// The obvious rule is `seg == "." || seg == ".."`, and it is not enough.
/// The Win32 layer strips trailing dots and spaces from a path component
/// before resolving it, so `".. "`, `".."` and `".. ."` all reach the
/// filesystem as `..`. Collapsing the whole class is the only version that
/// is correct on Windows, which is Jodd's primary target.
///
/// Note what this does NOT reject: dots that are part of a real name.
/// `my..notes`, `..hidden` and `v1.2` all pass. Blanket-rejecting any
/// segment *containing* `..` would refuse a legitimate folder name to buy
/// nothing — traversal needs the component to *be* a dot-run, not merely
/// contain one.
fn is_dot_run(seg: &str) -> bool {
    !seg.is_empty() && seg.chars().all(|c| c == '.' || c.is_whitespace())
}

/// Validate one path component. Returns the trimmed name on success.
///
/// This is the shared safety core: everything here is about the segment
/// being storable as a Gmail label AND joinable onto a filesystem path
/// without escaping. Policy that is not about safety — notably the
/// `__name__` system-folder reservation — belongs to the caller, because
/// the MCP write path deliberately wants to allow `__Claude__` leaves.
pub fn validate_label_segment(name: &str) -> Result<&str, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Folder name cannot be empty".to_string());
    }
    if trimmed.contains('/') {
        return Err("Folder name cannot contain '/'".to_string());
    }
    if trimmed.contains('\\') {
        return Err(format!(
            "Folder name '{trimmed}' cannot contain '\\' — it is a path separator on Windows. \
             Use '/' to separate folders in a path, and a plain name here."
        ));
    }
    if is_dot_run(trimmed) {
        return Err(format!(
            "Folder name cannot be '.' or '..' (got '{trimmed}') — those are relative-path \
             references, not folder names. Pass the full label you want, e.g. \
             'Notes/Research/Sources'."
        ));
    }
    if looks_like_windows_drive(trimmed) {
        return Err(format!(
            "Folder name '{trimmed}' starts with a Windows drive prefix ('X:'), which would \
             point outside the notes root. Pick a name that does not begin with a drive letter."
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(format!(
            "Folder name '{}' contains a control character (e.g. a newline or NUL). \
             Use printable text only.",
            trimmed.escape_debug()
        ));
    }
    if trimmed.len() > MAX_SEGMENT_BYTES {
        return Err(format!(
            "Folder name is too long ({} bytes; the limit is {MAX_SEGMENT_BYTES}). \
             Shorten this level of the path, or split it into nested folders.",
            trimmed.len()
        ));
    }
    Ok(trimmed)
}

/// Validate a whole `Notes/…` folder label supplied from outside Jodd.
///
/// Stricter than [`validate_label_segment`] in one way on purpose: a segment
/// may not carry leading or trailing whitespace. The segment validator trims
/// (its caller — a text field in the sidebar — then stores the trimmed name),
/// but a full path is stored *verbatim*, so `"Notes/a "` and `"Notes/a"`
/// would become two folder rows that Windows resolves to one directory.
/// A path handed to this function must already be canonical.
pub fn validate_label_path(path: &str) -> Result<(), String> {
    if path != NOTES_ROOT && !path.starts_with(&format!("{NOTES_ROOT}/")) {
        return Err(format!(
            "Folder path '{path}' must be '{NOTES_ROOT}' or start with '{NOTES_ROOT}/' — \
             every Jodd folder lives under the {NOTES_ROOT} root that Apple Notes syncs."
        ));
    }
    if path.ends_with('/') {
        return Err(format!(
            "Folder path '{path}' must not end with '/'. Drop the trailing slash."
        ));
    }
    if path.len() > MAX_LABEL_BYTES {
        return Err(format!(
            "Folder path is too long ({} bytes; the limit is {MAX_LABEL_BYTES}). \
             Use a shallower or shorter path.",
            path.len()
        ));
    }
    for seg in path.split('/') {
        // Safety first, canonicality second: `".. "` is both a dot-run and
        // padded, and the caller is much better served by being told it
        // looks like `..` than by being told it has a trailing space.
        validate_label_segment(seg)
            .map_err(|e| format!("Invalid folder path '{path}': {e}."))?;
        if seg != seg.trim() {
            return Err(format!(
                "Invalid folder path '{path}': the segment '{seg}' has leading or trailing \
                 whitespace. Pass the exact label, without padding."
            ));
        }
    }
    Ok(())
}

/// Validate a note title supplied from outside Jodd. Titles are far freer
/// than labels — they are message subjects, not paths — so this is only a
/// length cap plus a control-character check (a newline in a `Subject:`
/// header is header injection).
pub fn validate_note_title(title: &str) -> Result<(), String> {
    if title.chars().any(char::is_control) {
        return Err(format!(
            "Title contains a control character (e.g. a newline): '{}'. \
             A note title is a single line — put the rest in the body.",
            title.escape_debug()
        ));
    }
    if title.len() > MAX_TITLE_BYTES {
        return Err(format!(
            "Title is too long ({} bytes; the limit is {MAX_TITLE_BYTES}). \
             Shorten the title and put the detail in the body.",
            title.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paths_are_accepted() {
        validate_label_path("Notes").unwrap();
        validate_label_path("Notes/__Claude__").unwrap();
        validate_label_path("Notes/__Claude__/Research").unwrap();
        // Thai, and a name that merely *contains* dots.
        validate_label_path("Notes/บันทึก").unwrap();
        validate_label_path("Notes/v1.2/my..notes").unwrap();
        validate_label_path("Notes/..hidden").unwrap();
        // A colon that is not a drive prefix stays legal — real Gmail labels
        // look like this.
        validate_label_path("Notes/Meeting: Monday").unwrap();
    }

    #[test]
    fn traversal_is_refused_in_every_position() {
        for bad in [
            "Notes/..",
            "Notes/../x",
            "Notes/__Claude__/../../x",
            "Notes/__Claude__/./x",
            "Notes/__Claude__/.. /x", // Windows strips the trailing space
            "Notes/__Claude__/... /x",
            "Notes/__Claude__/.. ./x",
        ] {
            let err = validate_label_path(bad).unwrap_err();
            assert!(err.contains("'.' or '..'"), "for {bad:?}: {err}");
        }
    }

    #[test]
    fn windows_separators_and_drive_prefixes_are_refused() {
        // `..\..` never contains a '/', so a POSIX-only view of the string
        // sees one innocuous segment.
        let err = validate_label_path(r"Notes/..\..\pwned").unwrap_err();
        assert!(err.contains("Windows"), "{err}");
        let err = validate_label_path("Notes/D:evil").unwrap_err();
        assert!(err.contains("drive prefix"), "{err}");
    }

    #[test]
    fn structural_rules() {
        assert!(validate_label_path("Elsewhere").is_err());
        assert!(validate_label_path("NotesX/a").is_err(), "prefix must be a whole segment");
        assert!(validate_label_path("Notes/").is_err());
        assert!(validate_label_path("Notes//x").is_err());
        assert!(validate_label_path("Notes/ padded ").is_err());
        assert!(validate_label_path("Notes/a\nb").is_err());
    }

    #[test]
    fn length_caps_are_enforced_at_both_levels() {
        let long_seg = "x".repeat(MAX_SEGMENT_BYTES + 1);
        let err = validate_label_path(&format!("Notes/{long_seg}")).unwrap_err();
        assert!(err.contains("too long"), "{err}");
        assert!(validate_label_path(&format!("Notes/{}", "x".repeat(MAX_SEGMENT_BYTES))).is_ok());

        // A path of individually-legal segments can still bust the total cap.
        let deep: String = std::iter::repeat("Notes")
            .take(MAX_LABEL_BYTES / 6 + 2)
            .collect::<Vec<_>>()
            .join("/");
        let err = validate_label_path(&format!("Notes/{deep}")).unwrap_err();
        assert!(err.contains("too long"), "{err}");
    }

    #[test]
    fn segment_validator_allows_the_reserved_pattern() {
        // The __name__ reservation is app-layer policy (gotcha #4), NOT part
        // of the safety core — the MCP write path must be able to create
        // Notes/__Claude__. If this ever starts failing, lib.rs's extra
        // check has leaked down into the shared validator.
        assert_eq!(validate_label_segment("__Claude__").unwrap(), "__Claude__");
    }

    #[test]
    fn titles_are_capped_and_single_line() {
        validate_note_title("A perfectly ordinary title").unwrap();
        assert!(validate_note_title("two\nlines").is_err());
        assert!(validate_note_title(&"t".repeat(MAX_TITLE_BYTES + 1)).is_err());
    }
}
