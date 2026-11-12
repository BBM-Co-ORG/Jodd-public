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

/// P1 first (`merge_remote_appends`), then the line-by-line three-way merge
/// (`merge_three_way`). What reconcile and the editor's refused save call.
pub fn merge_edits(base: &str, local: &str, remote: &str) -> Option<String> {
    merge_remote_appends(base, local, remote).or_else(|| merge_three_way(base, local, remote))
}

/// `local` and `remote` merged line by line against the `base` both were
/// built on, or `None` when they changed the same lines — then the caller
/// keeps both, as before.
///
/// A line is one top-level node of `<body>` (`lines_of`): an element with all
/// it contains, a run of text, a comment. Measured 2026-11-05 (note 80D6C8E1):
/// a Share to Jodd on a Tab S7 a day behind appended after the note's
/// trailing empty lines while another device had typed two links in front of
/// them. Neither touched the other's lines, yet the "base plus an ending"
/// rule saw no append and forked a copy.
///
/// The rules that keep it from losing or inventing text:
/// - `base` must be what BOTH were built on, and the newest such body
///   (gotcha #41): against an older one, lines another device deleted would
///   read as this device's insertions and come back.
/// - Changed regions that overlap keep both, unless the change is identical.
///   An insertion strictly inside a region the other side replaced overlaps.
/// - Two insertions at the same place keep both texts, local's first (the
///   order P1 produces), unless one starts with the other — Jodd's own push
///   whose answer was lost — when the longer one alone is kept.
/// - A remote change carrying an `<object>` keeps both (P1's rule: iCloud
///   compose refuses new inline objects, gotcha #24).
/// - An empty base, or markup that does not close, never merges.
///
/// Byte-exact per line, like P1: a re-encoded line reads as changed, which
/// costs a conflict copy, never text. The result keeps `local`'s document
/// shell (gotcha #37).
pub fn merge_three_way(base: &str, local: &str, remote: &str) -> Option<String> {
    let base_inner = body_inner(base);
    if base_inner.trim().is_empty() {
        return None;
    }
    let (local_inner, remote_inner) = (body_inner(local), body_inner(remote));
    let base_lines = lines_of(&base_inner)?;
    let local_hunks = hunks(&base_lines, &lines_of(&local_inner)?)?;
    let remote_hunks = hunks(&base_lines, &lines_of(&remote_inner)?)?;
    if remote_hunks.iter().any(|h| h.lines.iter().any(|l| l.to_ascii_lowercase().contains("<object"))) {
        return None;
    }
    let merged = apply_both(&base_lines, &local_hunks, &remote_hunks)?;
    Some(with_body_inner(local, &merged.concat()))
}

/// What the editor needs to merge a row that moved under it without
/// replacing its own DOM (which would cost the user's undo history and caret,
/// `editorRenderDecision.ts`): the merged body to save, and the row's new
/// lines as insertions into the editor's own top-level nodes.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorMerge {
    pub body_html: String,
    /// How many top-level nodes the editor's body had. The editor checks it
    /// against its DOM before inserting, so an index never lands elsewhere.
    pub local_lines: usize,
    /// `(index, html)`: put `html` before the editor's `index`-th top-level
    /// node (or at the end when `index == local_lines`). Indexes are into the
    /// editor's lines as sent, ascending.
    pub inserts: Vec<(usize, String)>,
}

/// The editor's text (`editor`, built on `base`, the body it last saved or
/// rendered) merged with the row that moved underneath — or `None`, and the
/// editor keeps both as before.
///
/// Stricter than `merge_three_way` in one way: the row may only have ADDED
/// lines. When reconcile kept both, the row is the other device's version,
/// built on an older base than the editor's, so "the row lacks a line the
/// editor's base had" is not a deletion anyone made — merging it would drop
/// the line from the note. Insertions are safe either way.
pub fn merge_onto_moved_row(base: &str, editor: &str, row: &str) -> Option<EditorMerge> {
    let base_inner = body_inner(base);
    if base_inner.trim().is_empty() {
        return None;
    }
    let (editor_inner, row_inner) = (body_inner(editor), body_inner(row));
    let base_lines = lines_of(&base_inner)?;
    let editor_lines = lines_of(&editor_inner)?;
    let editor_hunks = hunks(&base_lines, &editor_lines)?;
    let row_hunks = hunks(&base_lines, &lines_of(&row_inner)?)?;
    if row_hunks.iter().any(|h| !h.is_insert() || h.lines.iter().any(|l| l.to_ascii_lowercase().contains("<object"))) {
        return None;
    }
    let merged = apply_both(&base_lines, &editor_hunks, &row_hunks)?.concat();

    let mut inserts: Vec<(usize, String)> = Vec::new();
    for r in &row_hunks {
        // The editor's own insertion at the same point goes first
        // (`apply_both`), and whatever of the row's it already holds is not
        // inserted again.
        let mut lines: &[&str] = &r.lines;
        if let Some(e) = editor_hunks.iter().find(|e| e.is_insert() && e.start == r.start) {
            if e.lines.starts_with(lines) {
                continue;
            }
            if lines.starts_with(&e.lines) {
                lines = &lines[e.lines.len()..];
            }
        }
        // Base position → editor position: every editor hunk that ends at or
        // before the point shifts it by what it added or removed.
        let shift: isize = editor_hunks
            .iter()
            .filter(|e| e.end <= r.start)
            .map(|e| e.lines.len() as isize - (e.end - e.start) as isize)
            .sum();
        inserts.push(((r.start as isize + shift) as usize, lines.concat()));
    }

    // The insertions must rebuild exactly the merged body, or the editor would
    // show one thing and save another.
    let mut rebuilt = String::new();
    let mut next = inserts.iter().peekable();
    for (i, line) in editor_lines.iter().enumerate() {
        while let Some((_, html)) = next.next_if(|(at, _)| *at == i) {
            rebuilt.push_str(html);
        }
        rebuilt.push_str(line);
    }
    for (_, html) in next {
        rebuilt.push_str(html);
    }
    if rebuilt != merged {
        return None;
    }
    Some(EditorMerge { body_html: with_body_inner(editor, &merged), local_lines: editor_lines.len(), inserts })
}

/// A run of base lines `start..end` replaced by `lines` (an insertion when
/// `start == end`), in base coordinates.
#[derive(Debug, Clone, PartialEq)]
struct Hunk<'a> {
    start: usize,
    end: usize,
    lines: Vec<&'a str>,
}

impl Hunk<'_> {
    fn is_insert(&self) -> bool {
        self.start == self.end
    }
}

/// Beyond this many line pairs in the part that differs, keep both rather
/// than spend the memory: a note that long and that different is no merge.
const MAX_DIFF_CELLS: usize = 4_000_000;

/// `side`'s changes against `base`, as non-overlapping hunks in base order.
/// Longest common subsequence over the lines that differ, after trimming the
/// common start and end.
fn hunks<'a>(base: &[&'a str], side: &[&'a str]) -> Option<Vec<Hunk<'a>>> {
    let prefix = base.iter().zip(side).take_while(|(a, b)| a == b).count();
    let max_suffix = base.len().min(side.len()) - prefix;
    let suffix = base.iter().rev().zip(side.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    let (b, s) = (&base[prefix..base.len() - suffix], &side[prefix..side.len() - suffix]);
    if b.is_empty() && s.is_empty() {
        return Some(Vec::new());
    }
    let (n, m) = (b.len(), s.len());
    if (n + 1).saturating_mul(m + 1) > MAX_DIFF_CELLS {
        return None;
    }
    // lcs[i * (m + 1) + j] = LCS length of b[i..] and s[j..].
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * (m + 1) + j] = if b[i] == s[j] {
                lcs[(i + 1) * (m + 1) + j + 1] + 1
            } else {
                lcs[(i + 1) * (m + 1) + j].max(lcs[i * (m + 1) + j + 1])
            };
        }
    }
    let mut out: Vec<Hunk<'a>> = Vec::new();
    let mut open: Option<Hunk<'a>> = None;
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && b[i] == s[j] {
            out.extend(open.take());
            i += 1;
            j += 1;
            continue;
        }
        let h = open.get_or_insert_with(|| Hunk { start: prefix + i, end: prefix + i, lines: Vec::new() });
        if j < m && (i == n || lcs[i * (m + 1) + j + 1] >= lcs[(i + 1) * (m + 1) + j]) {
            h.lines.push(s[j]);
            j += 1;
        } else {
            i += 1;
            h.end = prefix + i;
        }
    }
    out.extend(open);
    Some(out)
}

/// `base` with both sides' hunks applied, or `None` on a conflict.
fn apply_both<'a>(base: &[&'a str], local: &[Hunk<'a>], remote: &[Hunk<'a>]) -> Option<Vec<&'a str>> {
    let mut out: Vec<&'a str> = Vec::new();
    let mut pos = 0;
    let (mut li, mut ri) = (0, 0);
    let mut take = |out: &mut Vec<&'a str>, pos: &mut usize, start: usize, end: usize, lines: &[&'a str]| {
        out.extend_from_slice(&base[*pos..start]);
        out.extend_from_slice(lines);
        *pos = end;
    };
    loop {
        match (local.get(li), remote.get(ri)) {
            (None, None) => break,
            (Some(h), None) => {
                take(&mut out, &mut pos, h.start, h.end, &h.lines);
                li += 1;
            }
            (None, Some(h)) => {
                take(&mut out, &mut pos, h.start, h.end, &h.lines);
                ri += 1;
            }
            (Some(l), Some(r)) => {
                if l == r {
                    take(&mut out, &mut pos, l.start, l.end, &l.lines);
                    li += 1;
                    ri += 1;
                } else if l.is_insert() && r.is_insert() && l.start == r.start {
                    let lines: Vec<&str> = if l.lines.starts_with(&r.lines) {
                        l.lines.clone()
                    } else if r.lines.starts_with(&l.lines) {
                        r.lines.clone()
                    } else {
                        l.lines.iter().chain(&r.lines).copied().collect()
                    };
                    take(&mut out, &mut pos, l.start, l.end, &lines);
                    li += 1;
                    ri += 1;
                } else if before(l, r) {
                    take(&mut out, &mut pos, l.start, l.end, &l.lines);
                    li += 1;
                } else if before(r, l) {
                    take(&mut out, &mut pos, r.start, r.end, &r.lines);
                    ri += 1;
                } else {
                    return None;
                }
            }
        }
    }
    out.extend_from_slice(&base[pos..]);
    Some(out)
}

/// Whether `a` can be applied entirely before `b` touches the base: it ends
/// before `b` starts, or it is an insertion at the point `b`'s region starts.
/// (Two insertions at one point are `apply_both`'s own case.)
fn before(a: &Hunk, b: &Hunk) -> bool {
    if a.is_insert() {
        a.start < b.start || (a.start == b.start && !b.is_insert())
    } else {
        a.end <= b.start && !(b.is_insert() && b.start == a.start)
    }
}

/// `doc` with what is inside its `<body>` replaced by `inner`. A fragment is
/// replaced whole. Content an older build stranded after `</html>` is part of
/// `inner` already (`body_inner`), so the shell is closed cleanly after it.
fn with_body_inner(doc: &str, inner: &str) -> String {
    let Some(open) = doc.find("<body") else { return inner.to_string() };
    let Some(gt) = doc[open..].find('>') else { return inner.to_string() };
    let start = open + gt + 1;
    match doc.rfind("</body>") {
        Some(close) if close >= start => {
            let tail = &doc[close..];
            let after = tail["</body>".len()..].trim_start();
            let stranded = after.strip_prefix("</html>").unwrap_or(after);
            let closing = if stranded.is_empty() { tail } else { "</body></html>" };
            format!("{}{}{}", &doc[..start], inner, closing)
        }
        _ => format!("{}{}", &doc[..start], inner),
    }
}

/// HTML elements that never have content or a closing tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
];

/// A body's content cut into lines: each top-level element with everything
/// inside it, each run of text between elements, each comment. Joining the
/// lines gives back the same bytes. `None` when the markup does not close.
fn lines_of(inner: &str) -> Option<Vec<&str>> {
    let mut lines = Vec::new();
    let mut i = 0;
    while i < inner.len() {
        let end = if starts_markup(inner, i) { element_end(inner, i)? } else { next_markup(inner, i) };
        lines.push(&inner[i..end]);
        i = end;
    }
    Some(lines)
}

/// Whether a `<` at `i` opens markup (a tag, a closing tag or a comment),
/// rather than being a stray character in text.
fn starts_markup(s: &str, i: usize) -> bool {
    s.as_bytes().get(i) == Some(&b'<')
        && matches!(s.as_bytes().get(i + 1), Some(c) if c.is_ascii_alphabetic() || *c == b'/' || *c == b'!')
}

fn next_markup(s: &str, from: usize) -> usize {
    let mut i = from + 1;
    while i < s.len() && !starts_markup(s, i) {
        i += 1;
    }
    i.min(s.len())
}

/// The end of one tag starting at `i` (just past its `>`), skipping quoted
/// attribute values.
fn tag_end(s: &str, i: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (k, &c) in s.as_bytes()[i..].iter().enumerate() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'>' => return Some(i + k + 1),
            None => {}
        }
    }
    None
}

fn tag_name(tag: &str) -> String {
    tag.trim_start_matches(['<', '/'])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The end of the top-level node starting at `i`: a comment, a void or
/// self-closing tag, or an element through its matching closing tag.
fn element_end(s: &str, i: usize) -> Option<usize> {
    if s[i..].starts_with("<!--") {
        return s[i + 4..].find("-->").map(|k| i + 4 + k + 3);
    }
    if s[i..].starts_with("</") || s[i..].starts_with("<!") {
        return None; // a stray closing tag at top level, or a doctype in a body
    }
    let mut stack: Vec<String> = Vec::new();
    let mut k = i;
    loop {
        if !starts_markup(s, k) {
            k = next_markup(s, k);
            if k >= s.len() {
                return None;
            }
            continue;
        }
        if s[k..].starts_with("<!--") {
            k = k + 4 + s[k + 4..].find("-->")? + 3;
            continue;
        }
        let end = tag_end(s, k)?;
        let tag = &s[k..end];
        let name = tag_name(tag);
        if tag.starts_with("</") {
            if stack.pop()? != name {
                return None;
            }
        } else if !tag.ends_with("/>") && !VOID.contains(&name.as_str()) {
            stack.push(name);
        }
        k = end;
        if stack.is_empty() {
            return Some(k);
        }
    }
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

/// The line-by-line three-way merge. Fixtures are note 80D6C8E1 on
/// 2026-11-05, reduced: the Tab S7 a day behind (`S7_BASE`), another device's
/// two links typed in BEFORE the note's trailing empty lines (`OTHER`), and
/// the S7's Share to Jodd block appended after them (`SHARE`).
#[cfg(test)]
mod three_way_tests {
    use super::*;

    const S7_BASE: &str = "https://www.youtube.com/watch?v=jXZvDtXg5co<div>https://www.youtube.com/watch?v=LZ4iJr7dnbk<br></div><div><br></div><div><br></div><div></div>";
    const OTHER: &str = "https://www.youtube.com/watch?v=jXZvDtXg5co<div>https://www.youtube.com/watch?v=LZ4iJr7dnbk<br></div><div>https://youtu.be/C9xnQ8lyLwk?t=259</div><div>https://youtu.be/P53RGZP6VzY?t=411</div><div><br></div><div><br></div><div></div>";
    const MARKER: &str = "<div><br></div><div>— 2026-11-05 22:40 —</div>";
    const LINK: &str = "<div><a href=\"https://youtube.com/watch?v=rNh9kqV5l2g&amp;si=L1VILIicNyNl-NoZ\">https://youtube.com/watch?v=rNh9kqV5l2g&amp;si=L1VILIicNyNl-NoZ</a></div>";

    fn doc(inner: &str) -> String {
        format!("<html><head></head><body style=\"overflow-wrap: break-word;\">{inner}</body></html>")
    }

    fn share() -> String {
        format!("{MARKER}{LINK}")
    }

    /// The measured case. P1 refuses it — the other device's version is not
    /// the S7's base plus an ending — so it became a keep-both copy.
    #[test]
    fn a_share_on_a_stale_device_merges_with_lines_typed_elsewhere() {
        let local = doc(&format!("{S7_BASE}{}", share()));
        assert_eq!(merge_remote_appends(&doc(S7_BASE), &local, &doc(OTHER)), None, "precondition: P1 cannot");

        let merged = merge_three_way(&doc(S7_BASE), &local, &doc(OTHER));

        assert_eq!(merged, Some(doc(&format!("{OTHER}{}", share()))));
    }

    /// The editor's half of the same evening: the editor showed the S7's text,
    /// the user deleted the marker, and the row had meanwhile become the merge
    /// above. Their deletion and the other device's links are different lines.
    #[test]
    fn deleting_the_share_marker_merges_with_the_row_that_moved_underneath() {
        let base = doc(&format!("{S7_BASE}{}", share()));
        let editor = doc(&format!("{S7_BASE}{LINK}"));
        let row = doc(&format!("{OTHER}{}", share()));

        assert_eq!(merge_three_way(&base, &editor, &row), Some(doc(&format!("{OTHER}{LINK}"))));
    }

    #[test]
    fn the_same_line_changed_differently_on_both_sides_keeps_both() {
        let base = "<div>one</div><div>two</div><div>three</div>";
        let local = "<div>one</div><div>two, edited here</div><div>three</div>";
        let remote = "<div>one</div><div>two, edited there</div><div>three</div>";
        assert_eq!(merge_three_way(base, local, remote), None);
    }

    /// An insertion inside a stretch the other side replaced has no right
    /// place to go.
    #[test]
    fn an_insert_inside_a_region_the_other_side_rewrote_keeps_both() {
        let base = "<div>one</div><div>two</div>";
        let local = "<div>one</div><div>new</div><div>two</div>";
        assert_eq!(merge_three_way(base, local, "<div>rewritten</div>"), None);
    }

    #[test]
    fn the_same_change_on_both_sides_is_applied_once() {
        let base = "<div>one</div><div>two</div><div>three</div>";
        let both = "<div>one</div><div>three</div><div>added</div>";
        assert_eq!(merge_three_way(base, both, both).as_deref(), Some(both));
    }

    /// Two appends at the same end: nothing was removed, so both stay —
    /// local's first, the order P1 has always produced.
    #[test]
    fn two_appends_at_the_same_place_keep_both_texts() {
        let base = "<div>one</div>";
        let merged = merge_three_way(base, "<div>one</div><div>mine</div>", "<div>one</div><div>theirs</div>");
        assert_eq!(merged.as_deref(), Some("<div>one</div><div>mine</div><div>theirs</div>"));
    }

    /// Jodd's own push landed but its answer was lost: the remote holds what
    /// this device sent, and the user typed on. Appending it again would
    /// double their text (the P1 rule, gotcha #41).
    #[test]
    fn an_insert_the_other_side_already_holds_is_not_doubled() {
        let base = "<div>one</div>";
        let pushed = "<div>one</div><div>sent</div>";
        let typed_on = "<div>one</div><div>sent</div><div>typed after</div>";
        assert_eq!(merge_three_way(base, typed_on, pushed).as_deref(), Some(typed_on));
        assert_eq!(merge_three_way(base, pushed, typed_on).as_deref(), Some(typed_on));
    }

    #[test]
    fn a_deletion_next_to_the_other_sides_insert_merges() {
        let base = "<div>one</div><div>two</div><div>three</div>";
        let local = "<div>one</div><div>three</div>";
        let remote = "<div>one</div><div>two</div><div>inserted</div><div>three</div>";
        assert_eq!(merge_three_way(base, local, remote).as_deref(), Some("<div>one</div><div>inserted</div><div>three</div>"));
    }

    #[test]
    fn one_side_unchanged_takes_the_other() {
        let base = "<div>one</div><div>two</div>";
        let changed = "<div>uno</div><div>two</div><div>three</div>";
        assert_eq!(merge_three_way(base, base, changed).as_deref(), Some(changed));
        assert_eq!(merge_three_way(base, changed, base).as_deref(), Some(changed));
    }

    /// Nested markup is one line: a list edited inside is a change to that
    /// line, not to its items.
    #[test]
    fn a_nested_block_is_one_line() {
        let base = "<div>one</div><ul><li>a</li><li>b</li></ul><div>end</div>";
        let local = "<div>one, edited</div><ul><li>a</li><li>b</li></ul><div>end</div>";
        let remote = "<div>one</div><ul><li>a</li><li>b</li><li>c</li></ul><div>end</div>";
        assert_eq!(
            merge_three_way(base, local, remote).as_deref(),
            Some("<div>one, edited</div><ul><li>a</li><li>b</li><li>c</li></ul><div>end</div>")
        );
    }

    /// The same rule as P1 and the editor (gotcha #41): an attachment is an
    /// inline object iCloud compose refuses (#24), so a merged row carrying a
    /// new one would sit unpushable instead of being preserved.
    #[test]
    fn a_change_that_carries_an_attachment_keeps_both() {
        let base = "<div>one</div><div>two</div>";
        let local = "<div>one, edited</div><div>two</div>";
        let remote = "<div>one</div><div>two</div><div><object data=\"cid:x\"></object></div>";
        assert_eq!(merge_three_way(base, local, remote), None);
    }

    #[test]
    fn an_empty_base_never_merges() {
        assert_eq!(merge_three_way(&doc(""), &doc("<div>a</div>"), &doc("<div>b</div>")), None);
    }

    /// Markup that does not close cannot be split into lines with confidence;
    /// a guess here could drop text, a copy cannot.
    #[test]
    fn markup_that_does_not_close_keeps_both() {
        let base = "<div>one</div><div>two";
        assert_eq!(merge_three_way(base, "<div>one!</div><div>two", "<div>one</div><div>two!"), None);
    }

    /// Splitting into lines and joining them again is the identity, for every
    /// shape a note body takes: a bare leading text line (gotcha #11), void
    /// elements, comments, attributes holding `>`.
    #[test]
    fn lines_join_back_into_the_same_bytes() {
        for inner in [
            S7_BASE,
            OTHER,
            "plain<br>text<div a=\"x>y\">q</div><!-- c --><img src=\"cid:1\"><div><div>nested</div></div>",
            "",
        ] {
            let lines = lines_of(inner).expect(inner);
            assert_eq!(lines.concat(), inner);
        }
    }

    /// The editor's half: its text, the body it was built on, and the row that
    /// moved underneath. The row only ADDED lines (the other device's links),
    /// so they are handed back as insertions into the editor's own lines.
    #[test]
    fn the_editor_gets_the_rows_new_lines_as_insertions_into_its_text() {
        let base = doc(&format!("{S7_BASE}{}", share()));
        let editor = doc(&format!("{S7_BASE}{LINK}"));
        let row = doc(&format!("{OTHER}{}", share()));

        let m = merge_onto_moved_row(&base, &editor, &row).expect("merges");

        assert_eq!(m.body_html, doc(&format!("{OTHER}{LINK}")));
        assert_eq!(m.local_lines, 6, "the editor's top-level nodes");
        assert_eq!(
            m.inserts,
            vec![(2, "<div>https://youtu.be/C9xnQ8lyLwk?t=259</div><div>https://youtu.be/P53RGZP6VzY?t=411</div>".to_string())],
            "before the editor's third line"
        );
    }

    /// After a keep-both the row is the OTHER device's version, built on an
    /// older base than the editor's: "the row lacks my line" is not a deletion
    /// anyone made. So the editor merges only a row that added lines.
    #[test]
    fn the_editor_does_not_merge_a_row_that_lost_lines_it_had() {
        let mine = "<div>shared</div><div>typed here</div>";
        let editor = "<div>shared</div><div>typed here</div><div>and more</div>";
        let theirs = "<div>shared</div><div>written there</div>";
        assert_eq!(merge_onto_moved_row(mine, editor, theirs), None);
        // A line deleted elsewhere is the same shape: keep both.
        assert_eq!(merge_onto_moved_row(mine, editor, "<div>shared</div>"), None);
    }

    #[test]
    fn the_editor_merges_lines_added_on_both_sides_of_its_edit() {
        let base = "<div>one</div><div>two</div>";
        let editor = "<div>one, edited</div><div>two</div>";
        let row = "<div>top</div><div>one</div><div>two</div><div>end</div>";
        let m = merge_onto_moved_row(base, editor, row).expect("merges");
        assert_eq!(m.body_html, "<div>top</div><div>one, edited</div><div>two</div><div>end</div>");
        assert_eq!(m.inserts, vec![(0, "<div>top</div>".to_string()), (2, "<div>end</div>".to_string())]);
    }

    /// `merge_edits` is P1 first, then the three-way merge.
    #[test]
    fn merge_edits_falls_back_from_the_append_rule_to_the_three_way_merge() {
        let local = doc(&format!("{S7_BASE}{}", share()));
        assert_eq!(merge_edits(&doc(S7_BASE), &local, &doc(OTHER)), Some(doc(&format!("{OTHER}{}", share()))));
        // A pure remote append still goes through P1, unchanged.
        let edited = doc("<div>one, edited</div>");
        assert_eq!(
            merge_edits(&doc("<div>one</div>"), &edited, &doc("<div>one</div><div>appended</div>")),
            merge_remote_appends(&doc("<div>one</div>"), &edited, &doc("<div>one</div><div>appended</div>")),
        );
    }
}
