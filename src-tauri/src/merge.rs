//! Merging two edits of one note instead of keeping both (roadmap P1).
//!
//! Starts with the case that produced the 2026-10-21 conflicts: another
//! device only APPENDED to the revision this device last synced — Share to
//! Jodd's dated `— stamp —` blocks — while this device edited elsewhere in
//! the note. Anything else still keeps both (`reconcile::reconcile_one_db`).

use crate::mime822::{append_html, body_inner};

/// `local` with whatever `remote` appended to `base`, or `None` when `remote`
/// is not `base` plus an ending — then the edits may overlap and the caller
/// keeps both.
///
/// All three are note bodies as SQLite stores them (whole document or
/// fragment); the comparison is on what is inside `<body>`, byte for byte.
/// Strict on purpose: a re-encoded base reads as "changed", which costs a
/// conflict copy, never a merge that silently drops or doubles text. The
/// appended part goes in INSIDE `local`'s document (`append_html`, gotcha
/// #37).
pub fn merge_remote_appends(base: &str, local: &str, remote: &str) -> Option<String> {
    let base = body_inner(base);
    if base.trim().is_empty() {
        return None;
    }
    let remote = body_inner(remote);
    let appended = remote.strip_prefix(base.as_str())?.to_string();
    if appended.is_empty() {
        return Some(local.to_string());
    }
    // The ending is already here: Jodd's own push landed but its answer was
    // lost, so the row never moved off the old base while the remote holds
    // what it pushed (and maybe the user typed on). Appending would double it.
    let local_inner = body_inner(local);
    if local_inner.starts_with(remote.as_str()) || local_inner.ends_with(appended.as_str()) {
        return Some(local.to_string());
    }
    // An appended attachment keeps both, as in the editor (`appendedSuffix`):
    // on iCloud it is an inline object compose refuses (gotcha #24), and the
    // merged row would sit unpushable instead of being preserved.
    if appended.to_ascii_lowercase().contains("<object") {
        return None;
    }
    Some(append_html(local, &appended))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note 80D6C8E1 on 2026-10-21, reduced: the 00:40 revision both devices
    // started from, the Mac's edit (a link moved inside the list) and the
    // phone's two appends at 06:49 and 07:32.
    const BASE: &str = "<div>https://www.youtube.com/watch?v=jXZvDtXg5co</div><div>https://www.facebook.com/watch/?v=965106449453157</div><div><br></div><div>— 2026-10-21 00:40 —</div>";
    const MAC: &str = "<div>https://www.youtube.com/watch?v=jXZvDtXg5co</div><div>https://www.youtube.com/watch?v=cvN7svEoIxc</div><div>https://www.facebook.com/watch/?v=965106449453157</div><div><br></div><div>— 2026-10-21 00:40 —</div>";
    const APPENDED: &str = "<div><br></div><div>— 2026-10-21 06:49 —</div><div><br></div><div>— 2026-10-21 07:32 —</div>";

    fn doc(inner: &str) -> String {
        format!("<html><head></head><body style=\"overflow-wrap: break-word;\">{inner}</body></html>")
    }

    #[test]
    fn a_remote_that_only_appended_merges_onto_the_local_edit() {
        let remote = format!("{BASE}{APPENDED}");
        assert_eq!(merge_remote_appends(BASE, MAC, &remote).as_deref(), Some(format!("{MAC}{APPENDED}").as_str()));
    }

    /// Whole documents: the appended blocks land INSIDE the local document,
    /// never after `</html>` (gotcha #37).
    #[test]
    fn whole_documents_merge_inside_the_body() {
        let merged = merge_remote_appends(&doc(BASE), &doc(MAC), &doc(&format!("{BASE}{APPENDED}"))).unwrap();
        assert_eq!(merged, doc(&format!("{MAC}{APPENDED}")));
    }

    /// The version moved but the content did not: the local edit stands.
    #[test]
    fn a_remote_with_the_bases_content_leaves_the_local_edit() {
        assert_eq!(merge_remote_appends(&doc(BASE), MAC, &doc(BASE)).as_deref(), Some(MAC));
    }

    #[test]
    fn a_remote_that_changed_anything_before_its_end_does_not_merge() {
        let rewritten = BASE.replace("jXZvDtXg5co", "SOMETHINGELSE");
        assert_eq!(merge_remote_appends(BASE, MAC, &format!("{rewritten}{APPENDED}")), None);
        // Truncated is not appended either.
        assert_eq!(merge_remote_appends(BASE, MAC, "<div>https://www.youtube.com/watch?v=jXZvDtXg5co</div>"), None);
    }

    /// Review finding: Jodd's own push landed but its answer was lost, so the
    /// row still sits on the old base while the remote holds what it pushed.
    /// That remote is "base plus an ending" — an ending the local edit
    /// already has. Appending it again would double the user's text.
    #[test]
    fn an_ending_the_local_edit_already_holds_is_not_appended_again() {
        let pushed = format!("{MAC}{APPENDED}");
        assert_eq!(merge_remote_appends(MAC, &pushed, &pushed).as_deref(), Some(pushed.as_str()));
        // …and typing that followed the lost push.
        let typed_on = format!("{pushed}<div>typed after</div>");
        assert_eq!(merge_remote_appends(MAC, &typed_on, &pushed).as_deref(), Some(typed_on.as_str()));
    }

    /// An appended attachment keeps both — the editor's rule too
    /// (`appendedSuffix`): on iCloud it is an inline object compose refuses,
    /// and a merged row would sit unpushable instead of being preserved.
    #[test]
    fn an_appended_attachment_does_not_merge() {
        let remote = format!("{BASE}<div><object data=\"cid:x\"></object></div>");
        assert_eq!(merge_remote_appends(BASE, MAC, &remote), None);
    }

    /// No recorded base, no merge: an empty base is a prefix of everything,
    /// and "everything was appended" would duplicate the whole remote.
    #[test]
    fn an_empty_base_never_merges() {
        assert_eq!(merge_remote_appends("", MAC, &format!("{BASE}{APPENDED}")), None);
        assert_eq!(merge_remote_appends(&doc(""), MAC, &doc(&format!("{BASE}{APPENDED}"))), None);
    }
}
