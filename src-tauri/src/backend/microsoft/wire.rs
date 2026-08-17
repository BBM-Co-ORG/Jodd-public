//! Microsoft Graph wire format. There is no MIME here — Exchange stores a
//! structured item and synthesises MIME only on request, so `mime822.rs` is
//! unused on this backend.

/// Remove the note's title line from the body.
///
/// Apple always duplicates the title into the body, but its markup follows
/// whatever formatting the user applied, so no node-shape rule works on its
/// own. The rule this function applies: **everything from the start of
/// `<body>` up to the first block element is the title, UNLESS that block
/// element is a `<div>` whose own contents are themselves block-level** — in
/// that case the `<div>` is a container wrapping the whole note (the title
/// lives inside it, on its own line) and nothing is stripped from the outer
/// level. A `<div>` holding a single line of inline markup is a genuine title
/// wrapper and is stripped whole. This is decided by looking at what is
/// INSIDE the div, never by what follows it — trailing whitespace, a `<br>`,
/// an HTML comment, `&nbsp;`, or a stray text node after `</div>` must not
/// change the verdict. An unclosed leading `<div>`, and a body that contains
/// markup but no boundary this function recognises, both strip nothing rather
/// than guess.
///
/// `subject` already carries the title as plain text, so nothing has to be
/// parsed out — this only strips the duplicate so it does not appear twice in
/// the editor.
///
/// DO NOT reuse Gmail's `strip_leading_title`: it matches elements only, so a
/// bare-text-node title is invisible to it and it strips the first `<div>` —
/// the second line of real content — instead.
pub fn strip_leading_title_ms(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let body_open_end = match lower.find("<body") {
        Some(i) => match lower[i..].find('>') {
            Some(j) => i + j + 1,
            None => return html.to_string(),
        },
        None => return html.to_string(),
    };
    let body_close = lower.rfind("</body>").unwrap_or(html.len());
    if body_close <= body_open_end {
        return html.to_string();
    }

    let inner = &html[body_open_end..body_close];
    let lower_inner = &lower[body_open_end..body_close];

    // A <div> opening the body (allowing leading whitespace) is either the
    // title's own wrapper or a container holding the whole note. Tell them
    // apart by what is INSIDE the div, never by what follows it: a title
    // <div> holds one line of inline markup, a container holds block
    // content. Three earlier fixes asked "what follows?" and each was
    // defeated by a different trailing token — a newline, a <br>, a
    // comment, a stray text node.
    let cut = match first_line_boundary(lower_inner) {
        Some((i, "<div")) if lower_inner[..i].trim().is_empty() => {
            match matching_div_end(&lower_inner[i..]) {
                Some(rel_end) => {
                    let end = i + rel_end;
                    if div_holds_block_content(&lower_inner[i..end]) {
                        0 // container — the title lives inside it; strip nothing
                    } else {
                        end // a genuine title <div>
                    }
                }
                // Unclosed <div>: strip nothing rather than everything.
                None => 0,
            }
        }
        Some((i, _)) => i,
        // No line boundary anywhere. Plain text means the body really is one
        // line, and that line is the title. Any markup means something is here
        // that LINE_BOUNDARIES does not know about — keep it.
        None => if inner.contains('<') { 0 } else { inner.len() },
    };

    let mut out = String::with_capacity(html.len());
    out.push_str(&html[..body_open_end]);
    out.push_str(&inner[cut..]);
    out.push_str(&html[body_close..]);
    out
}

/// Tags that end the note's first *line*.
///
/// Only `<div` can WRAP a title — that is the shape a Graph-side write produces.
/// The rest merely mark where the title stops. This list is consulted in TWO
/// places with OPPOSITE failure modes, so a tag missing from it can cost the
/// whole note body, not just a cosmetic duplicate:
///
/// - As the cut point (`first_line_boundary`): a boundary we fail to list makes
///   the cut LATER, leaving a duplicated title visible but losing nothing.
/// - Inside `div_holds_block_content`: a boundary we fail to list is invisible
///   there too, so a leading `<div>` that actually wraps the whole note (its
///   "block content" is a tag not on this list) looks like a plain title `<div>`
///   instead of a container — and the entire note body gets stripped, silently.
///
/// Anything block-level belongs on this list. When in doubt, add the tag.
///
/// An extra boundary matched inside an HTML comment or attribute value cuts
/// mid-markup and emits malformed HTML (`<!-- <div>x</div> -->title…` leaves a
/// stray `-->`); this is accepted because Apple/Exchange note bodies are
/// machine-generated `<html><body>` + content with no comments and no
/// attributes carrying markup-like text. Do not add comment/attribute parsing.
const LINE_BOUNDARIES: &[&str] = &[
    "<div", "<br", "<p", "<ul", "<ol", "<li", "<h1", "<h2", "<h3", "<h4",
    "<h5", "<h6", "<table", "<blockquote", "<pre", "<hr",
    "<section", "<article", "<figure", "<dl", "<dd", "<dt", "<td", "<tr", "<th",
];

/// Byte offset of the earliest line boundary, and which tag it was.
fn first_line_boundary(lower_inner: &str) -> Option<(usize, &'static str)> {
    LINE_BOUNDARIES
        .iter()
        .filter_map(|t| lower_inner.find(t).map(|i| (i, *t)))
        .min_by_key(|(i, _)| *i)
}

/// Byte offset just past the `</div>` that closes the `<div>` at index 0.
///
/// Iterates by `char_indices`, never by byte: every index handed to a slice is
/// guaranteed to be a character boundary, so a Thai (or emoji, or curly-quote)
/// title cannot panic here. `checked_sub` makes a stray `</div>` return None
/// instead of underflowing.
fn matching_div_end(lower_inner: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, _) in lower_inner.char_indices() {
        let rest = &lower_inner[i..];
        if rest.starts_with("<div") {
            depth += 1;
        } else if rest.starts_with("</div>") {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i + 6);
            }
        }
    }
    None
}

/// Does this complete `<div>…</div>` hold block content, making it a container
/// rather than a title?
///
/// `div` is the whole element including both tags, already ASCII-lowercased. The
/// opening tag is skipped so that attribute text cannot be mistaken for content.
fn div_holds_block_content(div: &str) -> bool {
    let content_start = match div.find('>') {
        Some(i) => i + 1,
        None => return false,
    };
    let content_end = div.len().saturating_sub("</div>".len());
    if content_end <= content_start {
        return false;
    }
    let content = &div[content_start..content_end];
    LINE_BOUNDARIES.iter().any(|t| content.contains(t))
}

/// Write the title back as the body's first line, the inverse of
/// `strip_leading_title_ms`.
///
/// The shape is the one measured against the live account on 2026-08-14 and
/// confirmed in Notes.app: the title as a leading BARE TEXT NODE, then the
/// body's own block elements. Apple parsed it correctly, showing one note with
/// no duplicated title line.
///
/// **Wrapper-aware, and that is not an edge case.** `decode_messages` applies
/// `strip_leading_title_ms` at decode time, and the stripper PRESERVES the
/// `<html><body>` wrapper — so every pulled note's cached `body_html` already
/// carries one, while a note created or edited in Jodd carries editor-view
/// HTML that does not. Blindly wrapping would nest a second `<html><body>`
/// inside the first and send malformed HTML to Apple on every edit of a
/// pulled note, which is most of them.
///
/// Gotcha #11: the title is the first *line* of the body, not a node shape.
/// Do not reuse Gmail's `inject_title_into_body` — it wraps the title in a
/// `<div>`, which does not match what Apple itself writes for a plain title.
///
/// **Guards empty/whitespace titles, and this is a guarantee of the
/// function's contract, not caller-side hygiene.** `escape_html_text("")` and
/// `escape_html_text("   ")` both produce a string the stripper's
/// `.trim().is_empty()` check treats as nothing — so an unguarded injection
/// is byte-for-byte indistinguishable from "no title was ever injected",
/// which means `strip_leading_title_ms` reads the body's own first line as
/// the title and eats it. That is not a one-shot loss: push the ambiguous
/// body, the next poll decodes it through the stripper, `upsert_from_remote`
/// overwrites the cache with the truncation, and the next push writes the
/// truncation back — content destroyed both locally and on the remote,
/// silently, on every subsequent cycle. `title` is `pub`, and Tasks 5-6 are
/// not its only future callers (MCP write tools, LLM workflow output, and a
/// future cross-account move all reach this write path), so the invariant
/// belongs here rather than being re-derived independently by each caller.
pub fn inject_title_into_body_ms(title: &str, body_html: &str) -> String {
    // `<div></div>` round-trips exactly: `div_holds_block_content` returns
    // false whenever `content_end <= content_start`, which an EMPTY div
    // always satisfies (there is no content span at all), so the stripper
    // unconditionally classifies it as a genuine title <div> and consumes
    // exactly those 11 bytes — nothing from the real body. Verified against
    // `strip_leading_title_ms` by the round-trip property below, including
    // when it is the very first thing inside an existing wrapper. NOT yet
    // confirmed against a live Apple Notes render, though — added to Task
    // 12's live-probe list; this milestone is measured-not-inferred, and an
    // empty `<div>` immediately followed by real content is an unmeasured
    // shape until that probe runs.
    let escaped = if title.trim().is_empty() {
        "<div></div>".to_string()
    } else {
        escape_html_text(title)
    };
    // Find the end of an existing `<body …>` open tag, the same way the
    // stripper does, so the two halves agree by construction on what "inside
    // the body" means.
    let lower = body_html.to_ascii_lowercase();
    if let Some(i) = lower.find("<body") {
        if let Some(j) = lower[i..].find('>') {
            let at = i + j + 1;
            return format!("{}{}{}", &body_html[..at], escaped, &body_html[at..]);
        }
    }
    format!("<html><body>{}{}</body></html>", escaped, body_html)
}

/// Minimal text-node escaping. `&` first, or the other replacements' output
/// gets double-escaped.
///
/// Deliberately duplicates `llm::markdown::escape_html` rather than calling
/// it: `backend` must not depend on `llm` (the LLM module is a consumer of
/// note content, not a dependency of the wire format). It is not a byte-for-
/// byte copy — `llm::markdown::escape_html` also escapes `"`, because its
/// output can land inside an HTML attribute value; a title only ever lands
/// in a text node here, so quote-escaping would be needless work, not a
/// missing case.
fn escape_html_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod inject_title_tests {
    use super::*;

    /// The shape measured on 2026-08-14 (probe 5) and confirmed by eye in
    /// Notes.app: title as a leading bare text node, then a <div> per line.
    #[test]
    fn emits_the_shape_apple_accepted() {
        let out = inject_title_into_body_ms(
            "probe-title-after",
            "<div>the body line under the title</div>",
        );
        assert!(out.starts_with("<html><body>probe-title-after<div>"), "got: {out}");
        assert!(out.ends_with("</body></html>"), "got: {out}");
    }

    /// A body that ALREADY carries the wrapper must not get a second one.
    ///
    /// This is the real pipeline, not an edge case: `wire.rs:734` applies
    /// `strip_leading_title_ms` at decode time and the stripper PRESERVES the
    /// `<html><body>` wrapper, so every pulled Microsoft note's cached
    /// `body_html` is wrapped. Nesting a second wrapper would send malformed
    /// HTML to Apple.
    #[test]
    fn does_not_nest_a_second_wrapper() {
        let out = inject_title_into_body_ms(
            "T", "<html><body><div>one</div></body></html>");
        assert_eq!(out.matches("<body").count(), 1, "one <body> only: {out}");
        assert_eq!(out.matches("<html").count(), 1, "one <html> only: {out}");
        assert!(out.starts_with("<html><body>T<div>"),
            "the title goes INSIDE the existing body: {out}");

        // The measured Exchange shape (CLAUDE.md) carries a style attribute
        // on <body>, not a bare tag. This is the highest-risk function in
        // the milestone, so it must be asserted against what production
        // actually sends, not a convenient simplification.
        let out_attr = inject_title_into_body_ms(
            "T",
            "<html><body style=\"line-break:after-white-space\"><div>one</div></body></html>",
        );
        assert_eq!(out_attr.matches("<body").count(), 1, "one <body> only: {out_attr}");
        assert_eq!(out_attr.matches("<html").count(), 1, "one <html> only: {out_attr}");
        assert!(
            out_attr.starts_with("<html><body style=\"line-break:after-white-space\">T<div>"),
            "the title goes INSIDE the existing attributed body: {out_attr}"
        );
    }

    /// The contract. If this fails, the stripper eats the second line of real
    /// content on every note — silently, with no error and no log.
    ///
    /// It must hold for BOTH shapes, because both genuinely occur: a pulled
    /// note's cached body is wrapped (see above), while a note created or
    /// edited in Jodd carries editor-view HTML that is not.
    #[test]
    fn round_trips_through_the_stripper() {
        let bare = [
            ("plain", "<div>one</div><div>two</div>"),
            ("bold title", "<div><b>bold body</b></div>"),
            ("empty body", ""),
            ("thai", "<div>มี note ใน TEST TEST L2</div>"),
            ("entity", "<div>a &amp; b</div>"),
            // An empty or whitespace-only title is not a corner case — it is
            // the exact input class this contract exists to protect against.
            // Unguarded, its escaped form is indistinguishable from "no
            // title was injected at all", so the stripper reads the body's
            // own first line as the title and eats it.
            ("", "<div>one</div><div>two</div>"),
            ("   ", "<div>one</div><div>two</div>"),
            // Ties escaping to the round trip: without escaping, a raw '<'
            // in the title would move the cut the stripper computes,
            // corrupting the content boundary rather than merely leaving
            // markup visible.
            ("<b>x</b>", "<div>one</div><div>two</div>"),
            ("a & b", "<div>one</div><div>two</div>"),
            ("ชื่อเรื่อง", "<div>one</div><div>two</div>"),
        ];
        // A BARE body has no wrapper for the injector to insert into, so the
        // injector supplies one — which is exactly what Graph wants in
        // `body.content` (the measured working POST sent `<html><body>…`).
        // The stripper then preserves that wrapper, so the round trip returns
        // the NORMALISED form, not the bare input. Asserting bare equality
        // here is unsatisfiable for any injector that also wraps; what the
        // property must pin is that nothing except the title line is lost.
        for (title, body) in bare {
            let injected = inject_title_into_body_ms(title, body);
            assert_eq!(
                strip_leading_title_ms(&injected),
                format!("<html><body>{body}</body></html>"),
                "bare input must normalise to the wrapper shape, losing only the title: {title:?}"
            );
        }
        // A WRAPPED body is the production case — every pulled note's cached
        // body_html is wrapped — and here the round trip IS strict equality.
        for (title, body) in bare {
            let wrapped = format!("<html><body>{body}</body></html>");
            let injected = inject_title_into_body_ms(title, &wrapped);
            assert_eq!(
                strip_leading_title_ms(&injected), wrapped,
                "wrapped round trip failed for title {title:?}"
            );
        }
    }

    /// A title containing markup characters must not become markup.
    #[test]
    fn escapes_the_title() {
        let out = inject_title_into_body_ms("a < b & c > d", "<div>x</div>");
        assert!(!out.contains("a < b"), "raw '<' would open a bogus tag: {out}");
        assert!(
            out.contains("&lt;") && out.contains("&amp;") && out.contains("&gt;"),
            "got: {out}"
        );
    }
}

use std::collections::HashMap;

/// Map each Exchange folder id to the `folders.path` Jodd stores for it.
///
/// Exchange gives only a leaf display name (`PR_PARENT_DISPLAY`) and no
/// hierarchy, so two folders under different parents can share a name — and
/// `folders`'s PK is `(account_id, path)`, so a bare leaf name would silently
/// merge them. On collision, append the first 6 hex chars of SHA-256 of the
/// folder id.
///
/// A path is suffixed when its name collides **or when the name already
/// contains `~`**. That second clause is what makes the scheme sound: an
/// unsuffixed path then contains no `~` at all while a suffixed one always
/// does, so the two sets cannot intersect; and within the suffixed set the
/// hash is hex-only, so the last `~` always separates name from hash —
/// equality therefore requires the same name *and* the same id-hash. Without
/// it, a folder genuinely named `Ideas~db1ba9` collides with the suffix
/// generated for a folder named `Ideas`.
///
/// The suffix MUST be reproducible across builds, machines and releases:
/// `std::collections::hash_map::DefaultHasher` is NOT stable across Rust
/// versions and would re-label folders on a toolchain upgrade, orphaning every
/// note filed under the old path.
///
/// A `/` in a leaf display name does not create folder levels — it is
/// sanitized to `-` before collision detection so that ambiguity (a real
/// `A-B` alongside the sanitized `A/B`) is caught and suffixed apart.
/// Sanitisation happens first so the collision machinery disambiguates it.
///
/// A path is a function of the whole input SET, not of one folder: a name is
/// only suffixed when it collides. So adding a second "Ideas" renames the
/// first one. This is safe ONLY because notes and folders are derived from
/// the same `folder_paths` result in a single reconciliation pass, so
/// `notes.label` moves with `folders.path`. Two independent calls with
/// different inputs would produce two different paths for one folder.
/// (Graph returns each folder id once per reconciliation pass, so duplicate
/// ids in the input are unreachable; if present they would be last-write-wins
/// in the final `.collect()`.)
///
/// **The literal root name `"Notes"` is exempt from ordinary collision
/// counting.** `present_under_notes_root` keeps a folder named `"Notes"`
/// bare so it can serve as the tree's root — but nothing stops a user
/// *subfolder* from also being named `"Notes"` (Exchange enforces no
/// uniqueness, and gotcha #12 means Jodd cannot see real nesting to tell
/// the two apart). Suffixing on raw name collision, as every other name
/// does, would suffix BOTH — including the true root — since the root's own
/// name participates in `name_counts` like any other. That silently moves
/// every root-level note out of the top-level "Notes" group and makes
/// `notes_root_id` (which matches `path == "Notes"`) return `None`, so
/// `create_folder` starts refusing. Instead, exactly one folder named
/// `"Notes"` — deterministically the one with the lexicographically lowest
/// id, so the choice is stable across scans for the same input set — stays
/// bare; any other folder also named `"Notes"` is treated as an ordinary
/// collision and suffixed like `Ideas~ab12cd` would be, landing at
/// `"Notes/Notes~ab12cd"`.
pub fn folder_paths(pairs: &[(String, String)]) -> HashMap<String, String> {
    use sha2::{Digest, Sha256};

    // Sanitize names: replace "/" with "-" so folder hierarchy doesn't leak
    let sanitized: Vec<(String, String)> = pairs
        .iter()
        .map(|(id, name)| (id.clone(), name.replace('/', "-")))
        .collect();

    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for (_, name) in &sanitized {
        *name_counts.entry(name.as_str()).or_insert(0) += 1;
    }

    let notes_root_winner: Option<&str> = sanitized
        .iter()
        .filter(|(_, name)| name == "Notes")
        .map(|(id, _)| id.as_str())
        .min();

    sanitized
        .iter()
        .map(|(id, name)| {
            let is_notes_root_winner =
                name == "Notes" && Some(id.as_str()) == notes_root_winner;
            let needs_suffix = !is_notes_root_winner
                && (name_counts.get(name.as_str()).copied().unwrap_or(0) > 1
                    || name.contains('~'));
            let leaf = if needs_suffix {
                let digest = Sha256::digest(id.as_bytes());
                let short: String = digest.iter().take(3).map(|b| format!("{b:02x}")).collect();
                format!("{name}~{short}")
            } else {
                name.clone()
            };
            (id.clone(), present_under_notes_root(id, &leaf))
        })
        .collect()
}

/// Present a bare Exchange leaf name the way every other Jodd backend already
/// presents a folder path — rooted under `"Notes/"`.
///
/// Exchange has no path separator (gotcha #9's root cause): `parentFolderId`
/// only ever yields a leaf display name, so Milestone 1 stored it bare
/// (`"L1"`, `"Ideas"`, `"Notes"` for the root). That never mattered while the
/// vertical was read-only, but `lib.rs`'s folder commands
/// (`rename_folder`/`delete_folder`/`move_folder` all refuse anything that
/// doesn't start with `"Notes/"`; `create_folder` requires a `"Notes"`- or
/// `"Notes/…"`-rooted parent) and `db::ensure_workflow_folder`
/// (hardcodes `format!("Notes/{}", name)`) all assume Gmail's shape. Left
/// unfixed: every scanned folder refused rename/delete/sub-folder-create
/// (Critical 2), and `ensure_workflow_folder` inserting `Notes/__Extracts__`
/// while the next scan re-keyed the row to bare `__Extracts__` made
/// `prune_clean_folders` drop the "moved" row and the next Extract mint a
/// SECOND Exchange folder (Critical 3).
///
/// This is the single place that closes both: prefixing here, at the read
/// boundary, means `create_folder`'s already-`"Notes/"`-prefixed return value
/// is simply correct (no more "self-heals back to bare on the next scan"),
/// and every folder command downstream works unmodified. Fixing it by
/// loosening the four folder commands instead would mean touching five call
/// sites of shared code all three verticals rely on, for one backend's
/// leaf-vs-path mismatch — a larger, riskier change than this fix needs.
///
/// One genuinely nested level, not invented structure: every folder this
/// function sees IS a child of the Notes root (gotcha #12 means Jodd can
/// never see deeper than that), so prefixing it is honest, not a claim about
/// nesting Jodd cannot back up.
///
/// Two deliberate exceptions:
/// - The literal root leaf `"Notes"` stays bare `"Notes"`, not
///   `"Notes/Notes"` — it already IS what `create_folder`/`rename_folder`/
///   `delete_folder`/`move_folder`/`notes_root_id` special-case as the root.
/// - `UNFILED_FOLDER_ID` (the synthetic bucket `assemble_scan` adds for a
///   `parentFolderId` Pass B never named) stays bare too. It backs no real
///   Exchange folder — there is nothing to rename, delete or nest a
///   sub-folder under — so it must NOT satisfy the `"Notes/"`-prefix check
///   the four folder commands gate on, or the UI could offer to mutate a
///   folder that does not exist on the server.
///
/// **Accepted one-time re-key.** Every account that already scanned under
/// M1's bare paths gets its cached `folders` rows and every note's `label`
/// re-keyed from e.g. `"L1"` to `"Notes/L1"` the next time this runs. That is
/// exactly `upsert_folder_from_remote` / `reconcile_folders_from_vertical`'s
/// normal self-heal path (gotcha #4: derive, don't migrate) — bounded to one
/// poll, not a data-loss risk.
fn present_under_notes_root(id: &str, leaf: &str) -> String {
    if id == UNFILED_FOLDER_ID || leaf == "Notes" {
        leaf.to_string()
    } else {
        format!("Notes/{leaf}")
    }
}

#[cfg(test)]
mod folder_tests {
    use super::{folder_paths, FALLBACK_LABEL, UNFILED_FOLDER_ID};

    #[test]
    fn a_unique_name_is_used_as_is() {
        let pairs = vec![("id-a".into(), "L2".into()), ("id-b".into(), "Notes".into())];
        let map = folder_paths(&pairs);
        assert_eq!(map["id-a"], "Notes/L2", "a real folder must be rooted under Notes/");
        assert_eq!(map["id-b"], "Notes", "the root itself stays bare, not Notes/Notes");
    }

    /// The bug this test guards: a user subfolder literally named "Notes"
    /// used to suffix BOTH it and the true root (since the root's own name
    /// participated in the same collision count), breaking `notes_root_id`
    /// and re-nesting the real root under itself. Now exactly one "Notes"
    /// stays bare — deterministically the lower-id one — and the other is
    /// suffixed like any ordinary collision.
    #[test]
    fn a_subfolder_named_notes_does_not_corrupt_the_real_root() {
        let pairs = vec![
            ("root-id".into(), "Notes".into()),
            ("sub-id".into(), "Notes".into()),
            ("other-id".into(), "L2".into()),
        ];
        let map = folder_paths(&pairs);
        assert_eq!(map["root-id"], "Notes", "the lower id wins the bare root path");
        assert_ne!(map["sub-id"], "Notes", "the loser must not also claim the bare root path");
        assert!(
            map["sub-id"].starts_with("Notes/Notes~"),
            "the loser is suffixed and nested like any other collision, got {}",
            map["sub-id"]
        );
        assert_eq!(map["other-id"], "Notes/L2", "unrelated folders are unaffected");

        // Stable across call order and reruns, same as ordinary collisions.
        let reordered = vec![
            ("sub-id".into(), "Notes".into()),
            ("other-id".into(), "L2".into()),
            ("root-id".into(), "Notes".into()),
        ];
        let reordered_map = folder_paths(&reordered);
        assert_eq!(reordered_map["root-id"], map["root-id"]);
        assert_eq!(reordered_map["sub-id"], map["sub-id"]);
    }

    #[test]
    fn colliding_names_get_distinct_stable_suffixes() {
        let pairs = vec![("id-a".into(), "Ideas".into()), ("id-b".into(), "Ideas".into())];
        let map = folder_paths(&pairs);
        assert_ne!(map["id-a"], map["id-b"], "two folders must never share a path");
        assert!(map["id-a"].starts_with("Notes/Ideas~"));
        assert!(map["id-b"].starts_with("Notes/Ideas~"));

        // Stability is the whole point: a different call order, and a rerun,
        // must produce the same path — otherwise notes orphan on every sync.
        let reordered = vec![("id-b".into(), "Ideas".into()), ("id-a".into(), "Ideas".into())];
        assert_eq!(folder_paths(&reordered)["id-a"], map["id-a"]);
    }

    #[test]
    fn real_exchange_ids_that_differ_only_in_the_middle_still_separate() {
        // Truncating the id itself would collide here — these real ids share
        // their tail. This is why the suffix is a hash.
        let pairs = vec![
            ("AAAA5OtOfQAAAA==".into(), "Ideas".into()),
            ("AAAA5OtOgAAAAA==".into(), "Ideas".into()),
        ];
        let map = folder_paths(&pairs);
        assert_ne!(map["AAAA5OtOfQAAAA=="], map["AAAA5OtOgAAAAA=="]);
    }

    #[test]
    fn adding_a_colliding_folder_renames_the_existing_one() {
        let before = folder_paths(&[("id-a".into(), "Ideas".into())]);
        assert_eq!(before["id-a"], "Notes/Ideas");

        let after = folder_paths(&[("id-a".into(), "Ideas".into()), ("id-b".into(), "Ideas".into())]);
        assert_ne!(after["id-a"], before["id-a"], "documented: the existing folder is renamed");
        assert!(after["id-a"].starts_with("Notes/Ideas~"));
    }

    #[test]
    fn the_suffix_is_based_on_the_folder_id_content() {
        // SHA-256("id-a") = db1ba9f3cbe9915615dec2463c732ebf069ac845d917c29ef14250e3951d6c19
        // First 6 hex chars: db1ba9
        let pairs = vec![("id-a".into(), "Ideas".into()), ("id-b".into(), "Ideas".into())];
        let map = folder_paths(&pairs);
        assert_eq!(map["id-a"], "Notes/Ideas~db1ba9", "suffix must be derived from id content");
    }

    #[test]
    fn a_literal_tilde_name_cannot_collide_with_a_generated_suffix() {
        // If we have three folders: two named "Ideas" and one literally named
        // "Ideas~db1ba9", the first two get suffixed. The literal "Ideas~db1ba9"
        // also needs a suffix (it contains ~), so all three paths differ.
        let pairs = vec![
            ("id-a".into(), "Ideas".into()),
            ("id-b".into(), "Ideas".into()),
            ("id-c".into(), "Ideas~db1ba9".into()),
        ];
        let map = folder_paths(&pairs);
        let id_a_path = &map["id-a"];
        let id_b_path = &map["id-b"];
        let id_c_path = &map["id-c"];

        // All three must be distinct
        assert_ne!(id_a_path, id_b_path, "two Ideas folders must not share a path");
        assert_ne!(id_a_path, id_c_path, "Ideas literal cannot match Ideas~db1ba9 suffix");
        assert_ne!(id_b_path, id_c_path, "no two folders can share a path");

        // Both id-a and id-c must contain ~, proving both are suffixed
        assert!(id_a_path.contains('~'), "Ideas collision must be suffixed");
        assert!(id_c_path.contains('~'), "literal tilde name must be suffixed");
    }

    #[test]
    fn a_slash_in_a_leaf_name_does_not_create_a_folder_level() {
        let pairs = vec![("id-a".into(), "A/B".into())];
        let map = folder_paths(&pairs);
        // Exactly one '/' — the Notes/ root, not a second level the leaf's
        // own slash would otherwise have smuggled in.
        assert_eq!(map["id-a"].matches('/').count(), 1, "the leaf's own slash must not survive as a path level");
        assert_eq!(map["id-a"], "Notes/A-B", "slash must be replaced with dash, then rooted under Notes/");
    }

    #[test]
    fn a_slash_collision_with_a_literal_dash_is_caught_by_the_collision_machinery() {
        // A real "A-B" alongside a sanitized "A/B" would otherwise collide.
        // The collision machinery must catch this and suffix apart.
        let pairs = vec![
            ("id-1".into(), "A/B".into()),
            ("id-2".into(), "A-B".into()),
        ];
        let map = folder_paths(&pairs);
        let path_1 = &map["id-1"];
        let path_2 = &map["id-2"];
        assert_ne!(path_1, path_2, "slash-sanitized A/B must not collide with literal A-B");
        assert!(path_1.starts_with("Notes/A-B~"), "first folder must be suffixed");
        assert!(path_2.starts_with("Notes/A-B~"), "second folder must be suffixed");
    }

    /// Critical 2 from the whole-branch review: every real folder must satisfy
    /// the `"Notes/"`-prefix check `rename_folder`/`delete_folder`/
    /// `move_folder` (lib.rs) gate on, or every scanned folder is refused.
    #[test]
    fn every_real_folder_satisfies_the_notes_prefix_the_folder_commands_require() {
        let pairs = vec![
            ("root-id".into(), "Notes".into()),
            ("l1-id".into(), "L1".into()),
            ("ideas-id".into(), "Ideas".into()),
        ];
        let map = folder_paths(&pairs);
        assert_eq!(map["root-id"], "Notes");
        for id in ["l1-id", "ideas-id"] {
            assert!(
                map[id] == "Notes" || map[id].starts_with("Notes/"),
                "'{}' must satisfy the same Notes/ check rename_folder/delete_folder/move_folder \
                 gate on, or the folder is silently unrenameable/undeletable — got {:?}",
                id, map[id]
            );
        }
    }

    /// The synthetic "unfiled" bucket (`UNFILED_FOLDER_ID`) backs no real
    /// Exchange folder — nothing to rename, delete, or nest a sub-folder
    /// under. It must stay OUTSIDE the Notes/ tree so it does not accidentally
    /// pass the folder commands' `"Notes/"`-prefix gate.
    #[test]
    fn the_unfiled_bucket_never_gets_the_notes_prefix() {
        let pairs = vec![(UNFILED_FOLDER_ID.to_string(), FALLBACK_LABEL.to_string())];
        let map = folder_paths(&pairs);
        assert_eq!(map[UNFILED_FOLDER_ID], FALLBACK_LABEL, "the fallback bucket must stay bare");
        assert!(!map[UNFILED_FOLDER_ID].starts_with("Notes/"));
    }
}

#[cfg(test)]
mod tests {
    use super::strip_leading_title_ms;

    // Apple's native shape: title is a bare text node, unwrapped.
    #[test]
    fn strips_a_bare_text_node_title() {
        let html = "<html><body>test text format<div>bold</div><div>italic</div></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(!out.contains("test text format"), "title must not survive into the editor body");
        assert!(out.contains("bold") && out.contains("italic"), "content lines must survive");
    }

    // The case that breaks a node-shape rule: the user formatted the title.
    #[test]
    fn strips_an_inline_formatted_title() {
        let html = "<html><body><b><i>title in bold + italic</i></b>\
                    <div><u>second line with underline only</u></div></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(!out.contains("title in bold + italic"));
        assert!(out.contains("second line with underline only"),
            "the first <div> is CONTENT here, not the title — stripping it loses a real line");
        assert!(out.contains("<u>"), "formatting on surviving lines must be preserved");
    }

    // What a Graph-side write produces: title wrapped in its own <div>.
    #[test]
    fn strips_a_div_wrapped_title() {
        let html = "<html><body><div>note 2 in Note</div><div><br></div>\
                    <div>update from mac</div></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(!out.contains("note 2 in Note"));
        assert!(out.contains("update from mac"));
        assert!(out.contains("<div><br></div>"), "the <div><br></div> line must survive — an implementation that strips two <div>s fails");
    }

    // A note whose entire content is its title.
    #[test]
    fn a_title_only_note_yields_an_empty_body() {
        let html = "<html><body>test format in firstline</body></html>";
        let out = strip_leading_title_ms(html);
        assert!(!out.contains("test format in firstline"));
        // Extract text between <body> and </body> and assert it is empty
        let start = out.find("<body>").map(|i| i + 6).unwrap_or(0);
        let end = out.find("</body>").unwrap_or(out.len());
        let body_content = out[start..end].trim();
        assert!(body_content.is_empty(), "body must be actually empty");
    }

    #[test]
    fn a_body_with_no_recognisable_title_is_returned_unchanged_rather_than_emptied() {
        let html = "<html><body></body></html>";
        let out = strip_leading_title_ms(html);
        assert_eq!(out, html, "output must equal input exactly");
    }

    // Thai title with <div> should not panic
    #[test]
    fn a_thai_div_wrapped_title_does_not_panic() {
        let html = "<html><body><div>ทดสอบ</div><div>บรรทัดสอง</div></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(!out.contains("ทดสอบ"), "Thai title must be stripped");
        assert!(out.contains("บรรทัดสอง"), "Thai second line must survive");
    }

    // Curly apostrophe (which Apple autocorrects to) should not panic
    #[test]
    fn a_curly_apostrophe_title_does_not_panic() {
        let html = "<html><body><div>Kaiwan's note</div><div>line two</div></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(!out.contains("Kaiwan's note"), "curly apostrophe title must be stripped");
        assert!(out.contains("line two"), "second line must survive");
    }

    // A body with <br> instead of <div> should keep the second line
    #[test]
    fn a_br_separated_body_keeps_its_second_line() {
        let html = "<html><body>title<br>second line of real content</body></html>";
        let out = strip_leading_title_ms(html);
        assert!(!out.contains("title"), "title must be stripped");
        assert!(out.contains("second line of real content"), "the line after <br> must survive");
    }

    // An unclosed <div> at the start should strip nothing, not everything
    #[test]
    fn an_unclosed_div_title_strips_nothing_rather_than_everything() {
        let html = "<html><body><div>title<div>content</body></html>";
        let out = strip_leading_title_ms(html);
        assert!(out.contains("content"), "content must survive even with mismatched <div> tags");
    }

    // A wrapper <div> around the whole body should be treated as a container, not a title
    #[test]
    fn a_wrapper_div_around_the_whole_body_is_not_treated_as_a_title() {
        let html = "<html><body><div dir=\"ltr\">title<div>line two</div></div></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(out.contains("line two"), "line two must survive");
        let start = out.find("<body>").map(|i| i + 6).unwrap_or(0);
        let end = out.find("</body>").unwrap_or(out.len());
        let body_content = out[start..end].trim();
        assert!(!body_content.is_empty(), "body must not be empty");
    }

    // A nested wrapper div with divs inside should keep the inner content
    #[test]
    fn a_nested_wrapper_div_keeps_its_content() {
        let html = "<html><body><div><div>title</div><div>line two</div></div></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(out.contains("line two"), "line two must survive");
    }

    // A bullet list body should keep all items
    #[test]
    fn a_bullet_list_body_keeps_every_item() {
        let html = "<html><body>title<ul><li>alpha</li><li>beta</li></ul></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(!out.contains("title"), "title must be stripped");
        assert!(out.contains("alpha"), "alpha must survive");
        assert!(out.contains("beta"), "beta must survive");
    }

    // Thai bare text node title (not wrapped in div) should be stripped
    #[test]
    fn a_thai_bare_text_node_title_is_stripped() {
        let html = "<html><body>หัวข้อ<div>บรรทัดสอง</div></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(!out.contains("หัวข้อ"), "Thai bare text title must be stripped");
        assert!(out.contains("บรรทัดสอง"), "Thai second line must survive");
    }

    // A wrapper div followed by whitespace should still keep its content
    #[test]
    fn a_wrapper_div_followed_by_whitespace_still_keeps_its_content() {
        let html = "<html><body><div dir=\"ltr\">title<div>line two</div></div>\n</body></html>";
        let out = strip_leading_title_ms(html);
        assert!(out.contains("line two"), "line two must survive even with trailing whitespace");
        let start = out.find("<body>").map(|i| i + 6).unwrap_or(0);
        let end = out.find("</body>").unwrap_or(out.len());
        let body_content = out[start..end].trim();
        assert!(!body_content.is_empty(), "body must not be empty");
    }

    // A nested wrapper div followed by whitespace should keep its content
    #[test]
    fn a_nested_wrapper_div_followed_by_whitespace_keeps_its_content() {
        let html = "<html><body><div><div>title</div><div>line two</div></div>\n</body></html>";
        let out = strip_leading_title_ms(html);
        assert!(out.contains("line two"), "line two must survive with trailing whitespace");
    }

    // An inline-only body (no recognized block boundaries) should not be erased
    #[test]
    fn an_inline_only_body_is_not_erased() {
        let html = "<html><body>title<img src=\"x\"></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(out.contains("<img"), "the <img> tag must survive");
    }

    // Regression: a trailing <br> after a wrapper div's </div> must not flip
    // the container into looking like a title div.
    #[test]
    fn a_wrapper_div_with_a_trailing_br_keeps_its_content() {
        let html = "<html><body><div>title<br>line two</div>\n<br>\n</body></html>";
        let out = strip_leading_title_ms(html);
        assert!(out.contains("line two"), "line two must survive a trailing <br>");
    }

    // Regression: a trailing HTML comment after a wrapper div's </div> must
    // not flip the container into looking like a title div.
    #[test]
    fn a_wrapper_div_with_a_trailing_comment_keeps_its_content() {
        let html = "<html><body><div dir=\"ltr\">title<div>line two</div></div>\n<!--x--></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(out.contains("line two"), "line two must survive a trailing comment");
    }

    // Regression: a stray text node after a wrapper div's </div> must not
    // flip the container into looking like a title div.
    #[test]
    fn a_wrapper_div_with_a_trailing_text_node_keeps_its_content() {
        let html = "<html><body><div>title<div>line two</div></div>tail</body></html>";
        let out = strip_leading_title_ms(html);
        assert!(out.contains("line two"), "line two must survive a trailing text node");
        assert!(out.contains("tail"), "the trailing text node itself must survive");
    }

    // A <div> whose only content is the title (no nested block content) is a
    // genuine title-only note: the resulting body is correctly empty.
    #[test]
    fn a_div_holding_only_the_title_yields_an_empty_body() {
        let html = "<html><body><div>just a title</div></body></html>";
        let out = strip_leading_title_ms(html);
        let start = out.find("<body>").map(|i| i + 6).unwrap_or(0);
        let end = out.find("</body>").unwrap_or(out.len());
        let body_content = out[start..end].trim();
        assert!(body_content.is_empty(), "a div holding only the title must yield an empty body");
    }

    // Leading whitespace before a title <div> must not stop it being
    // recognised and stripped.
    #[test]
    fn leading_whitespace_before_a_div_title_still_strips_it() {
        let html = "<html><body>\n<div>title</div>\n<div>two</div></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(!out.contains("title"), "the title div must be stripped despite leading whitespace");
        assert!(out.contains("two"), "the second div must survive");
    }

    // Regression: a <section> inside the leading div (not previously in
    // LINE_BOUNDARIES) must be recognised as block content, so the div reads
    // as a container rather than a title — otherwise the whole body vanishes.
    #[test]
    fn a_container_div_holding_a_section_is_not_treated_as_a_title() {
        let html = "<html><body><div>title<section>line two</section></div></body></html>";
        let out = strip_leading_title_ms(html);
        assert!(out.contains("line two"), "line two must survive — the outer div is a container, not a title");
    }
}

/// One decoded message from a Graph `/messages` list response.
#[derive(Debug, Clone)]
pub struct GraphMessage {
    pub id: String,
    pub subject: String,
    pub internet_message_id: String,
    pub parent_folder_id: String,
    pub created_date_time: String,
    pub last_modified_date_time: String,
    /// Already title-stripped, editor-view.
    pub body_html: String,
    /// Leaf name only — `PidTagFolderPathname` and `PidTagDisplayName` both
    /// come back empty on messages, so the tree's shape is unavailable.
    pub parent_folder_name: String,
}

/// One page of a Graph list response.
pub struct GraphPage {
    pub messages: Vec<GraphMessage>,
    /// Graph's `@odata.nextLink`. A list response is PAGED — dropping this
    /// returns a subset of the mailbox with no error and no log, so it is
    /// surfaced as a value the caller must decide about rather than a detail
    /// the decoder can swallow.
    pub next_link: Option<String>,
    /// Messages dropped for carrying no `internetMessageId` — see below.
    pub skipped_without_identity: usize,
}

#[derive(serde::Deserialize)]
struct RawList {
    value: Vec<RawMessage>,
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMessage {
    id: String,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    internet_message_id: String,
    #[serde(default)]
    parent_folder_id: String,
    #[serde(default)]
    created_date_time: String,
    #[serde(default)]
    last_modified_date_time: String,
    #[serde(default)]
    body: Option<RawBody>,
    #[serde(default)]
    single_value_extended_properties: Vec<RawExtProp>,
}

#[derive(serde::Deserialize)]
struct RawBody {
    #[serde(default)]
    content: String,
}

#[derive(serde::Deserialize)]
struct RawExtProp {
    id: String,
    #[serde(default)]
    value: String,
}

/// Decode one page of a Graph `/messages` list response.
///
/// `internetMessageId` becomes the note's uuid and half its primary key
/// (`notes`'s PK is `(uuid, account_id)`), so a message that carries none must
/// not silently collapse onto the empty-string identity shared by every other
/// such message — it is dropped and counted in `skipped_without_identity`
/// instead. Angle brackets around the id (`<...>`, RFC 5322 msg-id syntax) are
/// stripped, exactly one from each end — a value like `<<a@b>>` keeps its
/// inner brackets, since Graph's own value is the source of truth beyond the
/// outermost wrapper.
///
/// Pagination is NOT followed here: Graph returns `@odata.nextLink` when more
/// results exist, and this function only surfaces that link — see
/// `GraphPage::next_link`'s doc comment for why silently dropping it is
/// dangerous.
pub fn decode_page(json: &str) -> Result<GraphPage, String> {
    let raw: RawList = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut messages = Vec::with_capacity(raw.value.len());
    let mut skipped_without_identity = 0usize;

    for m in raw.value {
        match raw_to_graph_message(m) {
            Some(msg) => messages.push(msg),
            None => skipped_without_identity += 1,
        }
    }

    Ok(GraphPage {
        messages,
        next_link: raw.next_link,
        skipped_without_identity,
    })
}

/// Thin wrapper over [`decode_page`] for callers that only need the messages.
pub fn decode_messages(json: &str) -> Result<Vec<GraphMessage>, String> {
    Ok(decode_page(json)?.messages)
}

/// Strip exactly one layer of RFC 5322 angle brackets from a raw
/// `internetMessageId`, the way Jodd's uuid has always been stored (`<<a@b>>`
/// keeps its inner brackets — Graph's own value is the source of truth beyond
/// the outermost wrapper). Shared by [`raw_to_graph_message`] (the read path)
/// and [`resolve_message_id_by_uuid`]'s decode (the M4 sidecar lookup path),
/// so both agree on exactly what a "uuid" is.
fn strip_message_id_brackets(raw: &str) -> String {
    let t = raw.trim();
    let t = t.strip_prefix('<').unwrap_or(t);
    t.strip_suffix('>').unwrap_or(t).to_string()
}

/// Per-item decode shared by [`decode_page`] (a list, one `RawMessage` per
/// entry) and [`decode_single_message`] (a lone `RawMessage`, the shape
/// `POST`/`PATCH`/move return). `None` means the message carried no usable
/// `internetMessageId` — [`decode_page`] counts that in
/// `skipped_without_identity`; [`decode_single_message`] turns it into an
/// `Err`, since there is no page to skip it from.
fn raw_to_graph_message(m: RawMessage) -> Option<GraphMessage> {
    let identity = strip_message_id_brackets(&m.internet_message_id);
    if identity.is_empty() {
        return None;
    }

    let body = m.body.map(|b| b.content).unwrap_or_default();
    let folder = m
        .single_value_extended_properties
        .iter()
        .find(|p| same_property_id(&p.id, PROP_PARENT_DISPLAY))
        .map(|p| p.value.clone())
        .unwrap_or_default();

    Some(GraphMessage {
        id: m.id,
        subject: m.subject,
        internet_message_id: identity,
        parent_folder_id: m.parent_folder_id,
        created_date_time: m.created_date_time,
        last_modified_date_time: m.last_modified_date_time,
        body_html: strip_leading_title_ms(&body),
        parent_folder_name: folder,
    })
}

/// Decode a single Graph message resource — the shape `POST /messages`,
/// `PATCH /messages/{id}` and `POST /messages/{id}/move` all return, as
/// opposed to [`decode_page`]'s list-shaped `{ "value": [...] }` response.
/// Reuses [`raw_to_graph_message`] rather than a second hand-rolled decoder —
/// see that function's doc comment for why a duplicate is exactly the shape
/// of defect this file has been burned by before (the property-id mismatch
/// documented on [`same_property_id`]).
///
/// **`parent_folder_name` is always empty on this path.** Only the read
/// path's [`folder_messages_url`] `$expand`s `PROP_PARENT_DISPLAY`; a write
/// response never carries it. Harmless today because nothing reads this
/// field off a write result — but a future caller feeding this straight into
/// `upsert_from_remote` would blank a cached folder name, so this is
/// deliberate, not an oversight to "fix" by adding the `$expand` here.
pub fn decode_single_message(json: &str) -> Result<GraphMessage, String> {
    let raw: RawMessage = serde_json::from_str(json).map_err(|e| e.to_string())?;
    raw_to_graph_message(raw).ok_or_else(|| "message has no internetMessageId".to_string())
}

#[cfg(test)]
mod decode_tests {
    use super::{decode_messages, decode_page};

    // Shape captured from the live account on 2026-08-14.
    const FIXTURE: &str = r#"{
      "value": [
        {
          "id": "AQMkADAwAAgERAAAA",
          "subject": "note 2 in Note",
          "internetMessageId": "<JH0PR04MB7174E99@JH0PR04MB7174.apcprd04.prod.outlook.com>",
          "parentFolderId": "AQMkADAwAAgERAAAA",
          "createdDateTime": "2026-08-13T17:08:23Z",
          "lastModifiedDateTime": "2026-08-13T19:48:36Z",
          "body": { "contentType": "html",
                    "content": "<html><body>note 2 in Note<div>update from mac</div></body></html>" },
          "singleValueExtendedProperties": [
            { "id": "String 0x0E05", "value": "Notes" }
          ]
        }
      ]
    }"#;

    #[test]
    fn decodes_identity_folder_name_and_a_title_stripped_body() {
        let msgs = decode_messages(FIXTURE).unwrap();
        assert_eq!(msgs.len(), 1);
        let m = &msgs[0];

        assert_eq!(m.subject, "note 2 in Note");
        assert_eq!(m.parent_folder_name, "Notes", "folder name comes from PR_PARENT_DISPLAY");
        assert_eq!(m.internet_message_id, "JH0PR04MB7174E99@JH0PR04MB7174.apcprd04.prod.outlook.com",
            "angle brackets must be stripped — this becomes the note uuid");
        assert!(m.body_html.contains("update from mac"));
        assert!(!m.body_html.contains(">note 2 in Note<"), "title line must already be stripped");
    }

    #[test]
    fn a_message_without_the_folder_property_decodes_with_an_empty_folder_name() {
        let json = r#"{"value":[{"id":"x","subject":"s","internetMessageId":"<a@b>",
                       "parentFolderId":"p","createdDateTime":"2026-01-01T00:00:00Z",
                       "lastModifiedDateTime":"2026-01-01T00:00:00Z",
                       "body":{"contentType":"html","content":"<html><body>s</body></html>"}}]}"#;
        let msgs = decode_messages(json).unwrap();
        assert_eq!(msgs[0].parent_folder_name, "", "absent property must not panic");
    }

    #[test]
    fn a_paged_response_surfaces_its_next_link() {
        let json = r#"{
          "value": [
            {"id":"x","subject":"s","internetMessageId":"<a@b>",
             "parentFolderId":"p","createdDateTime":"2026-01-01T00:00:00Z",
             "lastModifiedDateTime":"2026-01-01T00:00:00Z",
             "body":{"contentType":"html","content":"<html><body>s</body></html>"}}
          ],
          "@odata.nextLink": "https://graph.microsoft.com/v1.0/me/messages?$skip=10"
        }"#;
        let page = decode_page(json).unwrap();
        assert_eq!(
            page.next_link.as_deref(),
            Some("https://graph.microsoft.com/v1.0/me/messages?$skip=10")
        );
    }

    #[test]
    fn a_response_without_a_next_link_reports_none() {
        let page = decode_page(FIXTURE).unwrap();
        assert_eq!(page.next_link, None);
    }

    #[test]
    fn a_message_without_an_internet_message_id_is_skipped_and_counted() {
        let json = r#"{"value":[
            {"id":"has-id","subject":"s","internetMessageId":"<a@b>",
             "parentFolderId":"p","createdDateTime":"2026-01-01T00:00:00Z",
             "lastModifiedDateTime":"2026-01-01T00:00:00Z",
             "body":{"contentType":"html","content":"<html><body>s</body></html>"}},
            {"id":"no-id","subject":"s2",
             "parentFolderId":"p","createdDateTime":"2026-01-01T00:00:00Z",
             "lastModifiedDateTime":"2026-01-01T00:00:00Z",
             "body":{"contentType":"html","content":"<html><body>s2</body></html>"}}
        ]}"#;
        let page = decode_page(json).unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.skipped_without_identity, 1);
    }

    #[test]
    fn an_empty_internet_message_id_is_treated_as_missing() {
        let json = r#"{"value":[
            {"id":"x","subject":"s","internetMessageId":"",
             "parentFolderId":"p","createdDateTime":"2026-01-01T00:00:00Z",
             "lastModifiedDateTime":"2026-01-01T00:00:00Z",
             "body":{"contentType":"html","content":"<html><body>s</body></html>"}}
        ]}"#;
        let page = decode_page(json).unwrap();
        assert_eq!(page.messages.len(), 0, "empty internetMessageId must not be stored as an empty uuid");
        assert_eq!(page.skipped_without_identity, 1);
    }

    #[test]
    fn only_one_angle_bracket_is_stripped_from_each_end() {
        let json = r#"{"value":[
            {"id":"x","subject":"s","internetMessageId":"<<a@b>>",
             "parentFolderId":"p","createdDateTime":"2026-01-01T00:00:00Z",
             "lastModifiedDateTime":"2026-01-01T00:00:00Z",
             "body":{"contentType":"html","content":"<html><body>s</body></html>"}}
        ]}"#;
        let msgs = decode_messages(json).unwrap();
        assert_eq!(msgs[0].internet_message_id, "<a@b>");
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Graph read path — HTTP plus the pure decisions it makes
// ═════════════════════════════════════════════════════════════════════════════
//
// Everything above this line is pure decode/derive and is unit-tested against
// captured fixtures. Everything below splits deliberately into two halves: the
// `async fn`s do HTTP and nothing else, and every decision they make (what is a
// note, which folders exist, what a `Note` looks like) lives in a named pure
// function next to it, so the decisions are testable without a network.

use crate::backend::{MessageIndex, Note, SidecarKind, SidecarRecord, TransportError};
use std::collections::HashSet;
use std::time::Duration;

pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// `PR_MESSAGE_CLASS`. `IPM.StickyNote` is a note; `IPM.Note` is ordinary mail.
pub const PROP_MESSAGE_CLASS: &str = "String 0x001A";
/// `PR_PARENT_DISPLAY` — the parent folder's leaf display name. The only route
/// to a folder name that exists: the folder object itself 404s (gotcha #12).
pub const PROP_PARENT_DISPLAY: &str = "String 0x0E05";
/// The message class an Exchange note carries.
pub const CLASS_STICKY_NOTE: &str = "IPM.StickyNote";
/// `PR_CONTAINER_CLASS` — the folder-level counterpart of [`PROP_MESSAGE_CLASS`].
pub const PROP_CONTAINER_CLASS: &str = "String 0x3613";
/// The container class a Notes folder would carry. **Not required** — measured
/// 2026-08-14: a folder created with and without this property both appeared
/// in Notes.app. Sent anyway on the `PROP_MESSAGE_CLASS` precedent: it costs
/// nothing, and this API has already been seen to accept a wrong thing
/// quietly (an unclassed note POSTs 201 and lands as an email — see
/// `create_note`).
pub const CLASS_STICKY_CONTAINER: &str = "IPF.StickyNote";

/// A GUID-named MAPI property Jodd owns, carrying pin state directly on the
/// note's own message. Minted fresh (uuid4) for the M4 probe
/// (`scripts/ms_named_property_probe.py`, 2026-08-15/16) — not a
/// Microsoft-defined property set, no collision risk in practice.
///
/// Measured live: Graph round-trips this exactly on creation, it SURVIVES an
/// ordinary content `PATCH` (a save does not need to resend it), and a note
/// carrying it renders normally in Notes.app on Mac/iPhone and on
/// outlook.live.com/mail/notes — the M4 hypothesis ("Apple only reads fields
/// it knows about"), confirmed rather than assumed.
///
/// **Never compare this id with [`same_property_id`]/[`property_tag`].** Those
/// exist to normalize Graph's response-side reformatting of NUMBERED tags
/// (`String 0x001A` → `String 0x1a`) and do not apply to named ids — the probe
/// found Graph returns this id back unchanged, byte-for-byte, so a plain
/// string comparison is correct and `same_property_id` would be solving a
/// problem this id does not have.
pub const JODD_PIN_PROP: &str = "String {98d27db2-72ad-4511-a3b5-b73c7c42694b} Name JoddPin";

/// The JSON body [`JODD_PIN_PROP`] carries. An object rather than a bare
/// boolean so the shape can grow without a migration if a future milestone
/// ever needs a second field here — unlikely, since tags are scoped out of
/// M4 deliberately (see the M4 design spec), but cheap to allow.
pub fn pin_property_value(pinned: bool) -> String {
    serde_json::json!({ "pinned": pinned }).to_string()
}

/// Decode [`pin_property_value`]'s JSON body back to a bool. Anything that
/// does not parse, or parses without a `pinned` key, reads as `false` — a
/// property [`list_pinned`] cannot make sense of must never be treated as a
/// pin, since that would resurrect a note the user unpinned (or never pinned)
/// the moment its property value becomes unreadable for any reason.
pub fn decode_pin_value(value: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()
        .and_then(|v| v.get("pinned").and_then(|p| p.as_bool()))
        .unwrap_or(false)
}

#[cfg(test)]
mod pin_property_tests {
    use super::*;

    #[test]
    fn pin_value_round_trips_through_encode_and_decode() {
        assert!(decode_pin_value(&pin_property_value(true)));
        assert!(!decode_pin_value(&pin_property_value(false)));
    }

    #[test]
    fn the_property_id_uses_the_named_not_numbered_syntax() {
        // `String {GUID} Name X` — distinct from every numbered tag in this
        // module (`String 0x001A`), which is exactly why same_property_id
        // must not be used to compare it (see JODD_PIN_PROP's doc comment).
        assert!(JODD_PIN_PROP.starts_with("String {"));
        assert!(JODD_PIN_PROP.contains("Name JoddPin"));
    }

    #[test]
    fn malformed_or_absent_values_decode_as_not_pinned_never_as_pinned() {
        // A property list_pinned cannot make sense of must never read as a
        // pin — that would resurrect a note the user unpinned, or never
        // pinned, purely because its stored value became unreadable.
        assert!(!decode_pin_value(""));
        assert!(!decode_pin_value("not json"));
        assert!(!decode_pin_value(r#"{"tags":["x"]}"#), "no pinned key at all");
        assert!(!decode_pin_value(r#"{"pinned":"true"}"#), "pinned as a string, not a bool");
    }
}

/// Do these two `singleValueExtendedProperties` ids name the same property?
///
/// **Graph does not echo back the id you asked for.** A request for
/// `String 0x001A` returns items tagged `String 0x1a` — lower-cased, leading
/// zeros stripped — and `String 0x0E05` comes back as `String 0xe05`. The
/// request side must still send the padded form (that is what Graph accepts);
/// only the response differs.
///
/// Comparing a response id against the constant we sent therefore never matches
/// on real data. That is not a cosmetic mismatch: the class lookup found
/// nothing, `expand_was_rejected_silently` concluded the `$expand` had been
/// refused, and a mailbox with 13 notes read as **zero notes and zero folders**
/// with the error swallowed by `list_notes`' `?`. Measured live 2026-08-14.
///
/// Every unit test passed the whole time, because every fixture was hand-written
/// with the id the code *sends*. A fixture authored by whoever wrote the matcher
/// can only confirm the matcher agrees with itself.
///
/// So compare the numeric tag, and fall back to a case-insensitive string match
/// for any id that is not `<type> 0x<hex>` (named properties under a GUID, for
/// instance, which this backend does not use today).
pub fn same_property_id(a: &str, b: &str) -> bool {
    match (property_tag(a), property_tag(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// `"String 0x001A"` → `Some(("string", 0x1a))`; `"String 0x1a"` → the same.
fn property_tag(id: &str) -> Option<(String, u32)> {
    let (kind, hex) = id
        .split_once(" 0x")
        .or_else(|| id.split_once(" 0X"))?;
    u32::from_str_radix(hex.trim(), 16)
        .ok()
        .map(|n| (kind.trim().to_ascii_lowercase(), n))
}

/// Find one property's value among a message's `singleValueExtendedProperties`,
/// by whatever id-matching rule the caller passes — [`same_property_id`] for
/// a numbered property (`PROP_MESSAGE_CLASS`, `PROP_PARENT_DISPLAY`), or a
/// plain string comparison for a named one (`JODD_PIN_PROP` — see its doc
/// comment for why that property compares differently).
///
/// Shared by every decoder that reads one extended property off a message
/// list response (`decode_message_classes`, `decode_first_folder_name`,
/// `decode_pin_page`), which each used to hand-roll their own `{id, value}`
/// struct and search loop for exactly this — the same shape `RawExtProp`
/// (above) already existed to cover. That duplication was a real risk, not
/// just repetition: the id-matching logic here already caused one production
/// bug once (`same_property_id`'s own doc comment), which had to be fixed in
/// every copy separately; a second bug in the search itself would have
/// needed the same hunt across three near-duplicate decoders.
fn find_ext_prop<'a>(props: &'a [RawExtProp], matcher: impl Fn(&str) -> bool) -> Option<&'a str> {
    props.iter().find(|p| matcher(&p.id)).map(|p| p.value.as_str())
}

/// Label for a message whose `parentFolderId` resolved to no path. Unreachable
/// through `scan` (the map is built from the same message set), so this only
/// fires if Graph returns a message with an empty `parentFolderId`.
///
/// **Deliberately NOT `"Notes"`.** `Notes` is the real Exchange container Apple
/// files into — the user's primary folder. Naming the bucket `Notes` and routing
/// it through the collision machinery (which is correct, see `assemble_scan`)
/// would make the two collide the moment a single unfiled note existed, and the
/// user's main folder would render as `Notes~a1b2c3`. Certain and central.
///
/// `Unfiled` can still collide with a real folder a user named `Unfiled`, and
/// then both get suffixed — but that is rare and peripheral, which is the trade
/// being made here.
const FALLBACK_LABEL: &str = "Unfiled";
/// Name used for a folder whose `PR_PARENT_DISPLAY` came back empty. Collisions
/// between several such folders are resolved by `folder_paths` like any other.
const UNNAMED_FOLDER: &str = "Unnamed Folder";

/// `$select` for a pass that needs note content.
const SELECT_FULL: &str =
    "id,subject,internetMessageId,parentFolderId,createdDateTime,lastModifiedDateTime,body";
/// `$select` for a pass that only needs identity + folder placement. Bodies are
/// the bulk of a page, so the index/folder passes leave them out.
const SELECT_LIGHT: &str = "id,internetMessageId,parentFolderId";

/// **`PAGE_SIZE` and `MAX_PAGES` are a pair. The mailbox ceiling is their
/// PRODUCT, so raise the count, never the size.**
///
/// Two failure modes pull in opposite directions, and only the product resolves
/// both:
///
/// - Too small a *ceiling* and a real account errors out. `/me/messages` returns
///   the ENTIRE mailbox (notes are a rounding error next to years of email), so
///   the page count is driven by total message count. An early version paired
///   100 with a 2000-page cap; a 200k-message account crossed it and the user
///   would have seen **zero notes, permanently**.
/// - Too large a *page* and the request times out. Microsoft's own
///   `/me/messages` guidance warns that a page of hundreds of messages each
///   carrying a full body can trip a gateway timeout. A 504 classifies as
///   `Transient`, so the worker would retry **at the same `$top`**, hit the same
///   timeout, and never converge — an infinite retry loop that no amount of
///   backoff escapes.
///
/// So the page stays small and the count carries the ceiling: 100 × 100,000 is
/// ten million messages, unreachable in practice, with no timeout risk. Do not
/// "optimise" `PAGE_SIZE` upward; it buys nothing the count does not already
/// give, and it re-opens the 504 loop.
const PAGE_SIZE: usize = 100;

/// Runaway guard ONLY — not a mailbox-size limit. See [`PAGE_SIZE`].
///
/// A `@odata.nextLink` that never clears would otherwise loop forever,
/// accumulating the whole mailbox in memory. Crossing this means Graph is
/// misbehaving, not that the account is large. Failing loudly is right; failing
/// at a number a real account can reach is not.
const MAX_PAGES: usize = 100_000;

/// Map an HTTP status (+ optional `Retry-After`) onto a `TransportError`.
///
/// Deliberately NOT shared with `gmail::transport::classify_status`: the two
/// verticals are meant to be independent, and Gmail's copy documents itself as
/// the contract for *its* module. Eight lines of overlap is cheaper than a
/// microsoft → gmail dependency. The shared worker owns the retry policy; this
/// only classifies.
pub fn classify_status(status: u16, retry_after: Option<u64>, body: &str) -> TransportError {
    match status {
        429 => TransportError::RateLimited { retry_after: retry_after.map(Duration::from_secs) },
        401 | 403 => TransportError::Auth,
        404 => TransportError::NotFound,
        409 => TransportError::Conflict { remote_etag: None },
        500..=599 => TransportError::Transient { source: anyhow::anyhow!("HTTP {}: {}", status, body) },
        _ => TransportError::Permanent { source: anyhow::anyhow!("HTTP {}: {}", status, body) },
    }
}

/// `$expand` clause asking for exactly ONE extended property.
///
/// One per request is not a style choice: an `$expand=…($filter=id eq 'A' or id
/// eq 'B')` returns **200 with no properties at all** rather than an error, which
/// reads exactly like "the property does not exist" (measured 2026-08-14). Never
/// combine two ids here.
fn expand_one_property(prop_id: &str) -> String {
    format!(
        "$expand=singleValueExtendedProperties($filter=id%20eq%20'{}')",
        prop_id.replace(' ', "%20")
    )
}

/// Top-level `$filter` restricting a scan to `IPM.StickyNote` items.
///
/// Measured live 2026-08-14 with `scripts/ms_probe_filter.py`: this exact shape
/// — one extended property id ANDed with one value restriction inside a single
/// `any()` lambda — returned **200 with exactly the note subset** (13 of 50
/// messages, matching the client-side `keep_sticky_notes` baseline), including
/// when combined with the `$expand` above in the same request. The mechanism
/// behind gotcha #12's `$expand`-with-`or` failure turns out to be a general
/// Graph rule, not an `$expand`-only quirk: a `singleValueExtendedProperties`
/// filter must pair exactly ONE property id with ONE value restriction.
/// `$filter` enforces that rule with a **400** ("does not match to a single
/// extended property and a value restriction") rather than `$expand`'s
/// silent-empty, which is how this shape — one id, one value, joined by `and`,
/// never `or` — was found to be the one Graph accepts.
fn sticky_note_filter() -> String {
    format!(
        "$filter=singleValueExtendedProperties/any(ep:%20ep/id%20eq%20'{}'%20and%20ep/value%20eq%20'{}')",
        PROP_MESSAGE_CLASS.replace(' ', "%20"),
        CLASS_STICKY_NOTE,
    )
}

/// First-page URL for a whole-mailbox `/me/messages` scan.
///
/// **This is the expensive one.** `/me/messages` is the ENTIRE mailbox — every
/// ordinary email included — and with `SELECT_FULL` every one of those carries
/// its body. Only the full-index paths may use it; anything scoped to a single
/// folder must use [`folder_notes_url`] instead.
///
/// Carries [`sticky_note_filter`] so Graph does the `IPM.StickyNote` narrowing
/// server-side instead of this shipping every ordinary email's body over the
/// wire to be discarded by `keep_sticky_notes`. **The client-side check in
/// `keep_sticky_notes` stays anyway** — belt and braces, deliberately: this
/// `$filter` is an optimisation, not a correctness guarantee, and if Graph ever
/// stops honouring it (or silently narrows differently), the client-side check
/// is what stops ordinary mail from being shown as notes.
fn messages_url(select: &str) -> String {
    format!(
        "{GRAPH_BASE}/me/messages?$top={PAGE_SIZE}&$select={select}&{}&{}",
        expand_one_property(PROP_MESSAGE_CLASS),
        sticky_note_filter(),
    )
}

/// First-page URL for the notes of ONE folder.
///
/// `GET /me/mailFolders/{id}/messages` was proven live on 2026-08-14 to return
/// exactly that folder's items, and it is the difference between a scoped read
/// and a full mailbox pagination. The UI sweeps one folder every 2500 ms, so a
/// mailbox scan here multiplies into hundreds of body-carrying requests a
/// minute and Graph answers with 429s.
///
/// The id MUST be percent-encoded: Exchange folder ids are base64 and really do
/// contain `/`, `+` and `=`, so left raw a `/` splits the path and Graph 404s on
/// a truncated id.
///
/// **Deliberately does NOT carry [`sticky_note_filter`].** That filter earns its
/// keep on [`messages_url`] because `/me/messages` is the whole mailbox; a
/// folder in the Notes tree already contains nothing but notes (this is a
/// Notes-app-created container, not a mail folder the user filed things into),
/// so the filter would cost a query clause to narrow a set that is already
/// exactly the target set. The `$expand` stays — `scan_messages_from` still
/// needs the class value to satisfy `keep_sticky_notes` and the
/// silent-`$expand`-rejection guard, both shared with the mailbox-scan path.
fn folder_notes_url(folder_id: &str, select: &str) -> String {
    format!(
        "{GRAPH_BASE}/me/mailFolders/{}/messages?$top={PAGE_SIZE}&$select={select}&{}",
        urlencoding::encode(folder_id),
        expand_one_property(PROP_MESSAGE_CLASS)
    )
}

/// URL for Pass B — one message out of one folder, carrying that folder's name.
fn folder_messages_url(folder_id: &str) -> String {
    format!(
        "{GRAPH_BASE}/me/mailFolders/{}/messages?$top=1&$select=id&{}",
        urlencoding::encode(folder_id),
        expand_one_property(PROP_PARENT_DISPLAY)
    )
}

/// URL resolving a standard folder by `wellKnownFolderName`.
///
/// Unlike the Notes tree (gotcha #12: `GET /mailFolders/{id}` 404s for those),
/// the standard folders ARE addressable by name. Used only to learn the ids the
/// note scan must exclude — see [`excluded_folder_ids`].
fn well_known_folder_url(name: &str) -> String {
    format!("{GRAPH_BASE}/me/mailFolders/{name}?$select=id")
}

/// Header asking Graph for ids that survive a folder move.
///
/// Graph's `id` is documented to CHANGE when an item moves between folders
/// unless this is sent, and `fetch_note` matches on `id` to address a single
/// message. That addressing use is unaffected by `Note::version` (which
/// compares `lastModifiedDateTime`, not `id`, precisely so a folder move
/// doesn't itself read as a remote content change) — but a Graph-side id
/// change would still break re-fetch-by-id for a note the cache addresses via
/// its old id. Without this header, every folder move the user makes on their
/// iPhone risks a stale-id fetch failure on top of that.
///
/// **Untested live.** The 2026-08-14 investigation observed the default `id`
/// holding across edits but never exercised a folder move, and never sent this
/// header. Task 13 must verify both that Graph accepts it and that the ids it
/// returns are still accepted by `/me/messages/{id}`.
/// (`Prefer` is not one of `http`'s pre-defined header-name constants, hence the
/// literal.)
const PREFER_HEADER: &str = "Prefer";
const PREFER_IMMUTABLE_ID: &str = "IdType=\"ImmutableId\"";

/// One `reqwest::Client` for every Graph request this module makes.
///
/// `Client` owns the connection pool, so a fresh one per request threw away the
/// pooled TLS connection and re-handshaked — once per page, and a scan is many
/// pages. `Client` is internally `Arc`-based and documented as cheap to clone
/// and safe to share, so this changes nothing but the pooling.
///
/// Deliberately `Client::new()` and not a builder: Cargo.toml pins the TLS
/// backend per target precisely because nothing in this crate calls
/// `.use_rustls_tls()`, and every other client site here is `Client::new()`.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

// Redirect Graph traffic to a local server for the rest of this test.
//
// Only the ORIGIN is swapped, and only inside `graph_get`/`graph_send` —
// every URL is still BUILT from the real `GRAPH_BASE`, so tests that assert
// on a URL's shape keep seeing the production string no matter what order
// they run in. (An earlier version overrode the base at construction time
// and silently broke an unrelated URL test depending on test ordering.)
//
// Per-THREAD, not process-wide. M1 had exactly one test that needed a live
// socket, so a set-once `OnceLock` was enough. Task 5 adds several more
// (create/patch/move/delete each want their own recording server), and
// `cargo test`'s default runner gives every test its own OS thread — so a
// process-wide "first test to call this wins forever" would make every test
// after the first redirect to the FIRST test's server, which is not merely
// wrong, it is silently order-dependent (`let _ = OnceLock.set()` swallows
// the `Err`, so the losing test's requests go to the WINNING test's server,
// not nowhere).
//
// **The load-bearing rule this depends on: the CLIENT CALL — whichever of
// `graph_get`/`graph_send` ends up running `dispatch_url` — must be POLLED
// on the same thread that called `set_graph_base_for_test`.** The recording
// server's `tokio::spawn`'d accept loop is irrelevant to that rule: it never
// reads `TEST_GRAPH_BASE`, only the client call does. `#[tokio::test]`
// defaults to a current-thread runtime (no test here overrides `flavor`),
// and every test calls `create_note`/`patch_note`/etc. directly from its own
// body, so this holds today by construction — but it is NOT a green light to
// call the client from inside a `tokio::spawn`. A spawned task on a
// multi-threaded runtime can be polled on a different OS thread, where this
// thread_local reads back `None`, `dispatch_url` leaves `GRAPH_BASE`
// untouched, and the "unit test" sends a real request to
// `graph.microsoft.com`. See `db_crypto.rs`'s `kc_key_name` for the same
// per-thread fix applied to a different piece of process-wide test state,
// for the same underlying reason.
#[cfg(test)]
thread_local! {
    static TEST_GRAPH_BASE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_graph_base_for_test(base: &str) {
    TEST_GRAPH_BASE.with(|b| *b.borrow_mut() = Some(base.to_string()));
}

#[cfg(test)]
fn dispatch_url(url: &str) -> String {
    TEST_GRAPH_BASE.with(|b| match b.borrow().as_ref() {
        Some(base) => url.replacen(GRAPH_BASE, base, 1),
        None => url.to_string(),
    })
}

#[cfg(not(test))]
fn dispatch_url(url: &str) -> String {
    url.to_string()
}

/// Classifies a Graph HTTP response into its body (2xx) or a `TransportError`
/// — status, `Retry-After`, and the body read all handled once here, shared
/// by [`graph_get`] and [`graph_send`]. Before this existed, the two carried
/// near-identical ~15-line copies of this exact logic (their own comments
/// said as much — "Mirrors graph_get", "Same reasoning as graph_get" — but
/// were never actually merged), so a future fix to status/`Retry-After`
/// classification had to be applied twice or the read and write paths would
/// silently diverge in how they classify the same failure modes.
///
/// Returns the status alongside the body (not just the body) so
/// [`graph_send`]'s live-probe log line can still report the exact status
/// code, not just "2xx".
async fn read_graph_response(resp: reqwest::Response) -> Result<(u16, String), TransportError> {
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());

    // NOT `unwrap_or_default()`. A reset or truncated read mid-body is a
    // TRANSIENT network fault, but an empty string here reaches serde, fails to
    // decode, and surfaces as `Permanent` — which the worker never retries. The
    // status line already arrived, so this is the one place a 2xx can still be
    // a network failure.
    let body = resp.text().await.map_err(|e| TransportError::Transient {
        source: anyhow::anyhow!("graph response body read failed (HTTP {status}): {e}"),
    })?;

    if (200..300).contains(&status) {
        Ok((status, body))
    } else {
        Err(classify_status(status, retry_after, &body))
    }
}

/// GET a Graph URL with a bearer token, returning the body or a classified error.
async fn graph_get(token: &str, url: &str) -> Result<String, TransportError> {
    let resp = http_client()
        .get(dispatch_url(url))
        .bearer_auth(token)
        .header(PREFER_HEADER, PREFER_IMMUTABLE_ID)
        .send()
        .await
        // Connect/DNS/TLS/timeout — none of these say anything about the
        // request's validity, so they are retryable.
        .map_err(|e| TransportError::Transient { source: anyhow::anyhow!("graph request failed: {e}") })?;

    let (_, body) = read_graph_response(resp).await?;
    Ok(body)
}

fn decode_err(what: &str, e: String) -> TransportError {
    TransportError::Permanent { source: anyhow::anyhow!("failed to decode Graph {what}: {e}") }
}

/// `POST`/`PATCH`/`DELETE` a Graph URL with a bearer token and an optional
/// JSON body, returning the response body or a classified error.
///
/// The sibling of [`graph_get`] for every non-GET request the write path
/// needs — same status handling, same `classify_status`, but a method and
/// body the read path never had to carry.
///
/// **Every call sends [`PREFER_HEADER`]/[`PREFER_IMMUTABLE_ID`], with no
/// opt-out.** Measured with a contrast run on 2026-08-14: the same folder
/// move WITHOUT this header changed the note's `id`, which would make
/// `reconcile_one` manufacture a conflict copy on the very next poll. A
/// future write request that forgets to ask for this header is exactly the
/// kind of omission this function exists to make impossible.
/// Off by default. Gates BOTH of Task 12's live-probe log lines: the full
/// write response body logged in [`graph_send`] below, and the
/// `internetMessageId`-presence line logged in `decode_or_refetch`. The body
/// carries the note's actual content (subject, full HTML) — `crate::log!`
/// persists to a file on disk (`applog.rs` calls that exact tradeoff out:
/// "privacy / disk-space concern for a long-running app") — so logging it on
/// every write would put every real Microsoft-backend user's note content
/// into their own log file without them asking for it. The
/// `internetMessageId` line carries no note content, only an id, but it
/// still fires on every create/patch/move once shipped — this gate keeps it
/// scoped to the one measurement it exists for rather than becoming a
/// permanent, unrequested increase in production log volume. Nothing in the
/// shipped app or the test suite calls [`enable_write_body_logging`] — only
/// `examples/ms_write_probe.rs` does, to satisfy Task 12's requirement to
/// log a live write response for the one person running that probe against
/// their own account.
static LOG_WRITE_BODIES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn on Task 12's live-probe logging — see [`LOG_WRITE_BODIES`]'s doc
/// comment for exactly what that covers, why it defaults off, and who is
/// expected to call this.
pub fn enable_write_body_logging() {
    LOG_WRITE_BODIES.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn write_body_logging_enabled() -> bool {
    LOG_WRITE_BODIES.load(std::sync::atomic::Ordering::Relaxed)
}

async fn graph_send(
    token: &str,
    method: reqwest::Method,
    url: &str,
    body: Option<serde_json::Value>,
) -> Result<String, TransportError> {
    // Captured before `method` is moved into the request builder below —
    // needed for the response log line near the bottom of this function.
    let method_str = method.to_string();
    let mut req = http_client()
        .request(method, dispatch_url(url))
        .bearer_auth(token)
        .header(PREFER_HEADER, PREFER_IMMUTABLE_ID);
    if let Some(payload) = body {
        req = req.json(&payload);
    }

    let resp = req
        .send()
        .await
        // Connect/DNS/TLS/timeout — none of these say anything about the
        // request's validity, so they are retryable. Mirrors `graph_get`.
        .map_err(|e| TransportError::Transient { source: anyhow::anyhow!("graph request failed: {e}") })?;

    let (status, body) = read_graph_response(resp).await?;

    // Task 12: no prior measurement had ever looked at what a Graph
    // WRITE response actually contains — every earlier observation of
    // `internetMessageId` (M1, Task 5's own tests) was a READ-back. This
    // is the one place every write (create/patch/move/delete note,
    // create/rename/delete folder) passes through, so logging here
    // covers all of them without threading a raw body back through
    // every caller's return type. `crate::log!` goes to stderr and the
    // persistent app log (see its doc comment in lib.rs) — visible next
    // to this example's own printed report when run live. Gated by
    // `LOG_WRITE_BODIES` (see its doc comment just above this function)
    // so an ordinary user's note content never lands in their log file
    // by default. Deliberately NOT hoisted into `read_graph_response`:
    // that helper is shared with `graph_get`, and routing reads through
    // this same logging would start covering read bodies too — exactly
    // what the gate above says it must not do.
    if write_body_logging_enabled() {
        crate::log!("microsoft graph_send: {method_str} {url} -> HTTP {status} body: {body}");
    }
    Ok(body)
}

// ── Pass A: find the notes ───────────────────────────────────────────────────

/// `PR_MESSAGE_CLASS` per Graph message id, from the same JSON `decode_page`
/// consumes.
///
/// A second deserialize over the same body rather than a field on
/// `GraphMessage`: `GraphMessage` is Task 7's reviewed shape and carries the
/// *other* extended property (`PR_PARENT_DISPLAY`), which Pass A cannot ask for
/// at the same time. A message with no class property maps to `""`.
pub fn decode_message_classes(json: &str) -> Result<HashMap<String, String>, String> {
    #[derive(serde::Deserialize)]
    struct ClassList { value: Vec<ClassMessage> }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ClassMessage {
        id: String,
        #[serde(default)]
        single_value_extended_properties: Vec<RawExtProp>,
    }

    let list: ClassList = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(list
        .value
        .into_iter()
        .map(|m| {
            let class = find_ext_prop(&m.single_value_extended_properties, |id| {
                same_property_id(id, PROP_MESSAGE_CLASS)
            })
            .unwrap_or_default()
            .to_string();
            (m.id, class)
        })
        .collect())
}

/// Keep only the `IPM.StickyNote` messages.
///
/// `/me/messages` returns the ENTIRE mailbox — every ordinary email included —
/// so this filter is what separates notes from mail. A message whose class we
/// could not read is dropped: importing the user's Inbox as notes is far worse
/// than missing one note, and `scan_messages` raises a loud error if the class
/// is missing from *every* message (the silent-`$expand` failure).
///
/// Task 8 added a server-side `$filter` ([`sticky_note_filter`], on
/// [`messages_url`]) that does most of this narrowing before the page ever
/// downloads. **This check still runs regardless** — belt and braces,
/// deliberately: the server-side filter is an optimisation Graph could stop
/// honouring without warning, and this client-side check is certain no matter
/// what Graph does.
pub fn keep_sticky_notes(
    messages: Vec<GraphMessage>,
    classes: &HashMap<String, String>,
) -> Vec<GraphMessage> {
    messages
        .into_iter()
        .filter(|m| {
            classes
                .get(&m.id)
                .is_some_and(|c| c.eq_ignore_ascii_case(CLASS_STICKY_NOTE))
        })
        .collect()
}

/// Distinct `parentFolderId`s, in first-seen order.
///
/// Distinct is the point: Pass B costs one HTTP request per entry, so three
/// notes in two folders must yield two ids, not three. First-seen order (rather
/// than a `HashSet`'s) keeps the request sequence — and any log of it —
/// reproducible.
pub fn distinct_parent_folder_ids(messages: &[GraphMessage]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for m in messages {
        if !m.parent_folder_id.is_empty() && seen.insert(m.parent_folder_id.as_str()) {
            out.push(m.parent_folder_id.clone());
        }
    }
    out
}

/// What the pagination loop does once a page has been consumed.
#[derive(Debug, PartialEq, Eq)]
pub enum PageStep {
    /// No `@odata.nextLink` — the mailbox has been walked.
    Done,
    /// Fetch this URL next.
    Fetch(String),
}

/// Decide whether to fetch another page, stop, or refuse to continue.
///
/// Extracted from the loop precisely because it is the part that can silently
/// truncate a mailbox or hang forever, and neither failure is reachable from a
/// test while it lives inside an `async fn`. Three rules, in this order:
///
/// 1. **No link → done.** Note the ordering with an EMPTY page: an empty page
///    that still carries a link must keep going. Graph can return a page whose
///    `value` is empty (every message on it filtered, or a skip token landing on
///    a gap) while more pages follow, so emptiness must never terminate the walk.
/// 2. **A link we have already fetched → error.** A real `nextLink` always
///    carries a fresh skip token, so a repeat means Graph is cycling.
///    `seen` — not just the previous URL — is what makes this a real guard:
///    comparing against the last URL alone catches only `A→A`, while a
///    period-2 oscillation `A→B→A→B…` slips through and runs to `MAX_PAGES`.
///    That is bounded rather than infinite, but it is still a hundred thousand
///    requests and hours of wall clock before the user sees an error.
/// 3. **A link past the runaway cap → error.** `MAX_PAGES` is a malfunction
///    guard, not a size limit (see its doc comment).
pub fn next_page_step(
    next_link: Option<String>,
    seen: &HashSet<String>,
    pages_fetched: usize,
) -> Result<PageStep, TransportError> {
    let Some(next) = next_link else { return Ok(PageStep::Done) };

    if seen.contains(&next) {
        return Err(TransportError::Permanent {
            source: anyhow::anyhow!(
                "Graph returned an @odata.nextLink pointing at a page already fetched — the \
                 pagination is cycling and following it would never terminate. Repeated URL: \
                 {next}"
            ),
        });
    }

    if pages_fetched >= MAX_PAGES {
        return Err(TransportError::Permanent {
            source: anyhow::anyhow!(
                "Graph paged past the {MAX_PAGES}-page runaway guard ({} messages at \
                 $top={PAGE_SIZE}) and still reports more. That is far beyond any real \
                 mailbox, so this is a Graph malfunction, not a large account — refusing to \
                 report a truncated note list as complete.",
                MAX_PAGES * PAGE_SIZE
            ),
        });
    }

    Ok(PageStep::Fetch(next))
}

/// Did the `$expand` get rejected silently?
///
/// Graph answers an extended-property filter it dislikes with `200` and NO
/// properties at all rather than an error. Every message would then fail the
/// `IPM.StickyNote` check and the account would render as empty — a wrong answer
/// that looks exactly like a right one. Messages decoded but not one class value
/// anywhere is the signature.
pub fn expand_was_rejected_silently(decoded_any_message: bool, saw_any_class_value: bool) -> bool {
    decoded_any_message && !saw_any_class_value
}

/// Walk `/me/messages`, following `@odata.nextLink` to the end, keeping notes.
///
/// **The pagination loop is load-bearing.** Graph pages a list response; a
/// caller that reads only the first page gets a subset of the mailbox with no
/// error and no log, which is why Task 7 surfaced `next_link` as a value rather
/// than swallowing it. Every decision this loop makes lives in
/// [`next_page_step`] and [`expand_was_rejected_silently`], which are unit-tested.
async fn scan_messages(token: &str, select: &str) -> Result<Vec<GraphMessage>, TransportError> {
    scan_messages_from(token, messages_url(select)).await
}

/// The pagination walk itself, over whichever list endpoint `first_url` names.
///
/// Split from [`scan_messages`] so the folder-scoped read
/// ([`folder_notes_url`]) gets the identical paging, cycle and silent-`$expand`
/// guarantees rather than a second, thinner loop that would have to re-learn
/// them.
async fn scan_messages_from(token: &str, first_url: String) -> Result<Vec<GraphMessage>, TransportError> {
    let mut url = first_url;
    let mut kept: Vec<GraphMessage> = Vec::new();
    let mut decoded_any = false;
    let mut saw_any_class = false;
    let mut skipped = 0usize;
    let mut pages = 0usize;
    // Every URL fetched so far. Bounded by MAX_PAGES, and the cycle guard stops
    // long before that on any mailbox that is actually misbehaving.
    let mut seen: HashSet<String> = HashSet::new();

    loop {
        seen.insert(url.clone());
        let json = graph_get(token, &url).await?;
        let page = decode_page(&json).map_err(|e| decode_err("message page", e))?;
        let classes = decode_message_classes(&json).map_err(|e| decode_err("message classes", e))?;

        decoded_any |= !page.messages.is_empty();
        saw_any_class |= classes.values().any(|c| !c.is_empty());
        skipped += page.skipped_without_identity;
        kept.extend(keep_sticky_notes(page.messages, &classes));
        pages += 1;

        match next_page_step(page.next_link, &seen, pages)? {
            PageStep::Fetch(next) => url = next,
            PageStep::Done => break,
        }
    }

    if expand_was_rejected_silently(decoded_any, saw_any_class) {
        return Err(TransportError::Permanent {
            source: anyhow::anyhow!(
                "Graph returned messages but no {PROP_MESSAGE_CLASS} values — the \
                 singleValueExtendedProperties $expand was rejected silently (200 with no \
                 properties). See CLAUDE.md gotcha #12."
            ),
        });
    }

    // `skipped_without_identity` is a silent data loss if nobody looks at it:
    // a message with no `internetMessageId` has no uuid, so it cannot become a
    // note and is dropped. Expected to be 0 on a real mailbox — a non-zero value
    // is the only evidence that assumption broke.
    if skipped > 0 {
        crate::log!("microsoft scan: dropped {} message(s) carrying no internetMessageId", skipped);
    }
    crate::log!("microsoft scan: {} page(s), {} note(s)", pages, kept.len());

    Ok(kept)
}

// ── Deleted Items: keep trashed notes out of the live list ───────────────────

/// Well-known folders whose contents must never be reported as live notes.
///
/// `/me/messages` spans the WHOLE mailbox, Deleted Items included, and a note
/// the user trashed on their iPhone keeps its `IPM.StickyNote` class while it
/// sits there. Without this, [`keep_sticky_notes`] would happily bring it back
/// as a live note filed under a folder called "Deleted Items" — a deletion that
/// undoes itself.
///
/// `recoverableitemsdeletions` is the second stage (Exchange's "dumpster",
/// where an item goes after Deleted Items is emptied) and is excluded for the
/// same reason.
///
/// **HYPOTHESIS, NOT AN OBSERVATION.** This was reasoned about from the fact
/// that `/me/messages` is mailbox-wide and `PR_MESSAGE_CLASS` is unchanged by a
/// move; it was never seen happen against the live account, because the
/// 2026-08-14 investigation never trashed a note on the iPhone and re-read the
/// mailbox. The manual end-to-end task must confirm both halves: that a trashed
/// note really does come back through `/me/messages`, and that this exclusion
/// really does stop it.
const EXCLUDED_WELL_KNOWN_FOLDERS: &[&str] = &["deleteditems", "recoverableitemsdeletions"];

/// The `id` of a single folder object.
pub fn decode_folder_id(json: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Folder {
        id: String,
    }
    let f: Folder = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(f.id)
}

/// Drop every message that lives in one of `excluded`.
///
/// Pure so the decision is testable: the id comparison is exact, because both
/// sides come through [`graph_get`] and therefore through the same
/// `Prefer: IdType="ImmutableId"` header — mixing an immutable folder id with a
/// default-form `parentFolderId` would silently match nothing and quietly
/// restore the very notes this exists to hide.
pub fn exclude_folders(
    messages: Vec<GraphMessage>,
    excluded: &HashSet<String>,
) -> Vec<GraphMessage> {
    if excluded.is_empty() {
        return messages;
    }
    messages
        .into_iter()
        .filter(|m| !excluded.contains(&m.parent_folder_id))
        .collect()
}

/// Resolve the excluded folders' ids — best effort, once per scan.
///
/// **Never fails the read.** A folder that cannot be resolved is skipped with a
/// log line: showing the user their notes with a handful of trashed ones mixed
/// in is a far smaller failure than showing them nothing because a folder
/// lookup 404'd. The log is what makes the degradation visible.
async fn excluded_folder_ids(token: &str) -> HashSet<String> {
    let mut ids = HashSet::new();
    for name in EXCLUDED_WELL_KNOWN_FOLDERS {
        match graph_get(token, &well_known_folder_url(name)).await {
            Ok(json) => match decode_folder_id(&json) {
                Ok(id) if !id.is_empty() => {
                    ids.insert(id);
                }
                Ok(_) => crate::log!("microsoft scan: well-known folder '{}' returned no id", name),
                Err(e) => crate::log!("microsoft scan: could not decode '{}': {}", name, e),
            },
            Err(e) => crate::log!(
                "microsoft scan: could not resolve well-known folder '{}' ({}) — trashed notes \
                 in it may appear as live notes this tick",
                name,
                e
            ),
        }
    }
    ids
}

// ── Pass B: name the folders ─────────────────────────────────────────────────

/// The `PR_PARENT_DISPLAY` value on the first message of a folder-scoped list.
///
/// Deliberately not `decode_page`: that drops messages carrying no
/// `internetMessageId` (correctly — such a message cannot be a note), and here
/// we only want the folder's name off whatever message came back, so a dropped
/// message would cost the folder its name.
pub fn decode_first_folder_name(json: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct NameList { value: Vec<NameMessage> }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct NameMessage {
        #[serde(default)]
        single_value_extended_properties: Vec<RawExtProp>,
    }

    let list: NameList = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(list
        .value
        .first()
        .and_then(|m| {
            find_ext_prop(&m.single_value_extended_properties, |id| {
                same_property_id(id, PROP_PARENT_DISPLAY)
            })
        })
        .unwrap_or_default()
        .to_string())
}

/// One request per folder to learn its leaf display name.
///
/// `$top=1` because every message in the folder carries the same
/// `PR_PARENT_DISPLAY`; one is as good as all of them, and this costs one
/// request per FOLDER rather than a second pass over every message.
async fn folder_display_name(token: &str, folder_id: &str) -> Result<String, TransportError> {
    let json = graph_get(token, &folder_messages_url(folder_id)).await?;
    decode_first_folder_name(&json).map_err(|e| decode_err("folder name", e))
}

/// Turn one folder's Pass-B outcome into the name it will be filed under.
///
/// Two degradations, both deliberate, and extracted here so both are reachable
/// from a test:
///
/// - **`NotFound` → `UNNAMED_FOLDER`.** The folder id came from a message in
///   THIS scan, so a 404 means the folder was deleted between Pass A and Pass B.
///   Failing the whole account's sync over one mid-scan race is worse than
///   filing that folder's notes under a placeholder for one tick.
/// - **An empty or whitespace name → `UNNAMED_FOLDER`.** An empty path would
///   make an unlabelled folder row; collisions between several such folders are
///   resolved by `folder_paths` like any other name.
///
/// Everything else propagates: `Auth`, `RateLimited` and `Transient` say nothing
/// about this folder, and swallowing them would turn an expired token into a
/// mailbox full of folders named "Unnamed Folder".
pub fn folder_name_or_fallback(outcome: Result<String, TransportError>) -> Result<String, TransportError> {
    let name = match outcome {
        Ok(n) => n,
        Err(TransportError::NotFound) => String::new(),
        Err(e) => return Err(e),
    };
    Ok(if name.trim().is_empty() { UNNAMED_FOLDER.to_string() } else { name })
}

// ── One scan per operation ───────────────────────────────────────────────────

/// Everything a single read operation needs.
///
/// **The invariant is "stable within a scan, convergent across ticks" — not
/// stable outright.** `folder_paths` is computed ONCE per scan and drives BOTH
/// the folder list and every note's `label`, so within one `Scan` the two cannot
/// disagree. Across scans they can: `list_folders` runs its own `scan(false)`
/// and `list_all_notes` its own `scan(true)`, and a path is a function of the
/// whole input set (a name is only suffixed when it collides), so a folder can
/// legitimately be reported under a different path by two calls.
///
/// The divergence window is narrow and self-closing. It needs two folders
/// sharing a leaf name AND a membership change between the two scans — a folder
/// gaining or losing its last note, or a same-named folder appearing — inside
/// one sync tick. The next tick re-derives both sides from one truth and they
/// converge, which is this project's "derive, don't migrate" doctrine working as
/// designed rather than a hole in it.
///
/// A shared map across the two calls is structurally impossible: they are
/// separate trait methods on `Transport` and `NoteStore`, invoked at different
/// times by different callers. Do not restructure the scan to chase it.
pub struct Scan {
    pub messages: Vec<GraphMessage>,
    pub folder_paths: HashMap<String, String>,
}

/// Sentinel folder id for messages whose `parentFolderId` resolved to nothing.
///
/// `:` appears in no base64 alphabet, so this cannot collide with a real
/// Exchange folder id.
const UNFILED_FOLDER_ID: &str = "jodd:unfiled";

/// Build a `Scan` from Pass A's messages and Pass B's `(id, name)` pairs.
///
/// Split out of `scan` so the derivation the whole read path depends on runs in
/// tests exactly as it runs in production.
///
/// If any message points at a folder Pass B did not name, a synthetic
/// `(UNFILED_FOLDER_ID, FALLBACK_LABEL)` pair joins the input rather than the
/// label being invented downstream. That keeps the invariant that **every label
/// a note carries is a path `to_folders` also reports**: the fallback goes
/// through the same collision machinery, so a real folder sharing its name gets
/// suffixed apart from it instead of being silently merged with it. See
/// `FALLBACK_LABEL` for why that name is `Unfiled` and not `Notes`.
pub fn assemble_scan(messages: Vec<GraphMessage>, pairs: &[(String, String)]) -> Scan {
    let named: HashSet<&str> = pairs.iter().map(|(id, _)| id.as_str()).collect();
    let needs_fallback = messages.iter().any(|m| !named.contains(m.parent_folder_id.as_str()));

    let mut all = pairs.to_vec();
    if needs_fallback {
        all.push((UNFILED_FOLDER_ID.to_string(), FALLBACK_LABEL.to_string()));
    }

    Scan { folder_paths: folder_paths(&all), messages }
}

/// The path a message's `parentFolderId` maps to.
///
/// Falls through to the synthetic unfiled folder, which `assemble_scan`
/// guarantees is in the map whenever any message needs it — so this never
/// invents a label no folder row backs. `FALLBACK_LABEL` as a last resort covers
/// only a `Scan` built by hand.
fn label_for(paths: &HashMap<String, String>, parent_folder_id: &str) -> String {
    paths
        .get(parent_folder_id)
        .or_else(|| paths.get(UNFILED_FOLDER_ID))
        .cloned()
        .unwrap_or_else(|| FALLBACK_LABEL.to_string())
}

/// Pass A + Pass B. `with_body` drops `body` from the `$select` for callers that
/// only need identity and placement (the index and folder listings) — bodies are
/// the bulk of a page.
pub async fn scan(token: &str, with_body: bool) -> Result<Scan, TransportError> {
    let messages = scan_messages(token, if with_body { SELECT_FULL } else { SELECT_LIGHT }).await?;
    // Deleted Items is inside /me/messages. See EXCLUDED_WELL_KNOWN_FOLDERS.
    let messages = exclude_folders(messages, &excluded_folder_ids(token).await);

    let mut pairs: Vec<(String, String)> = Vec::new();
    for id in distinct_parent_folder_ids(&messages) {
        let name = folder_name_or_fallback(folder_display_name(token, &id).await)?;
        if name == UNNAMED_FOLDER {
            crate::log!("microsoft scan: folder {} has no readable name — filing it as {}", id, UNNAMED_FOLDER);
        }
        pairs.push((id, name));
    }

    Ok(assemble_scan(messages, &pairs))
}

/// What a scoped folder read should do, decided from the local path→id map.
#[derive(Debug, PartialEq, Eq)]
pub enum FolderRead {
    /// Address the folder directly. Carries the first-page URL.
    Scoped(String),
    /// No Exchange folder id is known for this path — fall back to the
    /// (cached) mailbox scan and filter it. See [`folder_read_plan`].
    MailboxFallback,
}

/// Choose between the folder-scoped endpoint and the mailbox fallback.
///
/// Extracted as a pure function because it is the whole of C1: the wrong branch
/// turns a two-and-a-half-second UI sweep into a full mailbox pagination with
/// bodies, which is invisible in every unit test that does not look at the URL.
///
/// A miss is NOT an error and NOT an empty list. `folder_ids` comes from the
/// local `folders` table, so a miss means the caller asked about a path that
/// table does not (or no longer) knows — a stale path the frontend is still
/// holding, most plausibly after a collision suffix changed (see
/// `folder_paths`). Returning empty there would let
/// `prune_clean_in_label` delete every cached row under it; falling back to the
/// mailbox scan returns the truth. The fallback is bounded: the vertical caches
/// its scan per instance, so it costs at most one mailbox pass per operation,
/// not one per folder.
///
/// An empty `label_id` counts as a miss — the `folders` row exists but carries
/// no Exchange id, and `/me/mailFolders//messages` is not a folder.
pub fn folder_read_plan(folder_ids: &HashMap<String, String>, path: &str) -> FolderRead {
    match folder_ids.get(path) {
        Some(id) if !id.trim().is_empty() => FolderRead::Scoped(folder_notes_url(id, SELECT_FULL)),
        _ => FolderRead::MailboxFallback,
    }
}

/// Read one folder's notes through `/me/mailFolders/{id}/messages`.
///
/// The returned `Scan` maps EVERY `parentFolderId` seen to `path`, not just the
/// id that was asked for. Every message came out of that folder by
/// construction, so its label is known without a Pass B — and hard-coding the
/// caller's own path removes any chance of a mismatch between the id form Graph
/// echoes back and the one stored in `folders.label_id`.
///
/// No Deleted-Items exclusion: this addresses one Notes folder, so nothing from
/// Deleted Items can be in the response.
pub async fn scan_folder(token: &str, url: String, path: &str) -> Result<Scan, TransportError> {
    let messages = scan_messages_from(token, url).await?;
    let mut folder_paths: HashMap<String, String> = distinct_parent_folder_ids(&messages)
        .into_iter()
        .map(|id| (id, path.to_string()))
        .collect();
    // A message with an empty parentFolderId contributes no entry above, so
    // keep the fallback pointing at this path too rather than at "Unfiled".
    folder_paths.insert(UNFILED_FOLDER_ID.to_string(), path.to_string());
    Ok(Scan { messages, folder_paths })
}

// ── Envelope mapping ─────────────────────────────────────────────────────────

/// Graph's ISO-8601 (`2026-08-13T17:08:23Z`) → RFC 2822.
///
/// **Not cosmetic.** `notes.date` is read back with
/// `chrono::DateTime::parse_from_rfc2822` in `db::CachedNote::from_remote`,
/// `lib::rfc2822_ms` and `list_trashed_notes`'s sort. An ISO-8601 string stored
/// there parses as `None`/`0` everywhere, so every Microsoft note would sort as
/// epoch and the dedupe-by-date comparison would be meaningless. The design
/// spec's field-mapping table says `date ← lastModifiedDateTime` without naming
/// a format; this is that mapping, in the format the rest of the codebase
/// already requires.
///
/// An unparseable value is passed through rather than dropped — the result is
/// the same `0` timestamp the cache already tolerates for a malformed header.
/// Callers that need to KNOW it failed use [`resolve_note_date`] instead.
pub fn graph_time_to_rfc2822(s: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.to_rfc2822())
        .unwrap_or_else(|_| s.to_string())
}

/// Which timestamp a note's `date` ended up coming from.
#[derive(Debug, PartialEq, Eq)]
pub enum DateSource {
    LastModified,
    /// `lastModifiedDateTime` was absent or unparseable; `createdDateTime` was used.
    Created,
    /// Neither parsed. The value is passed through raw and sorts as epoch.
    Unparseable,
}

/// Pick the timestamp for `notes.date`, preferring `lastModifiedDateTime`.
///
/// A note that sorts to 1970 sits at the bottom of every list forever, which
/// reads as a display bug rather than a bad timestamp — so an unparseable
/// `lastModifiedDateTime` falls back to `createdDateTime` (wrong by an edit's
/// worth, but in the right decade), and only when NEITHER parses does the note
/// take the epoch. That last case is reported through `DateSource::Unparseable`
/// so `to_note` can log it instead of it happening silently.
pub fn resolve_note_date(last_modified: &str, created: &str) -> (String, DateSource) {
    for (raw, source) in [(last_modified, DateSource::LastModified), (created, DateSource::Created)] {
        if let Ok(d) = chrono::DateTime::parse_from_rfc3339(raw) {
            return (d.to_rfc2822(), source);
        }
    }
    (last_modified.to_string(), DateSource::Unparseable)
}

/// Build the neutral `Note` envelope from a decoded Graph message.
///
/// `uuid` is `internetMessageId` — no Apple `X-` header exists on Exchange, and
/// this was verified stable across an Apple-side edit, a Graph `PATCH` and an
/// overwrite+undo cycle. `attachments` is always empty: Apple refuses
/// attachments on an Exchange account outright.
pub fn to_note(m: &GraphMessage, paths: &HashMap<String, String>, account_id: &str) -> Note {
    let (date, source) = resolve_note_date(&m.last_modified_date_time, &m.created_date_time);
    if source != DateSource::LastModified {
        crate::log!(
            "microsoft: note {} has no usable lastModifiedDateTime ({:?}) — using {:?}; \
             an Unparseable note sorts as epoch",
            m.id, m.last_modified_date_time, source
        );
    }

    Note {
        id: m.id.clone(),
        uuid: m.internet_message_id.clone(),
        title: m.subject.clone(),
        body_html: m.body_html.clone(),
        date,
        // NOT `id`: Exchange PATCHes in place, so the id is stable across an
        // edit and would report "unchanged" forever. Measured 2026-08-14.
        //
        // Deliberately the RAW field, NOT `date`'s `resolve_note_date`
        // fallback-to-`createdDateTime` value, even though `date` is filled
        // two lines above from that same resolver. `date` is a display/sort
        // value where "wrong by one edit's worth, but in the right decade" is
        // an acceptable landing spot. `version` is a change-detection token,
        // and `createdDateTime` does NOT change across edits — so if a note's
        // `lastModifiedDateTime` were persistently missing and this field fell
        // back to `createdDateTime` the way `date` does, every poll would
        // resolve to the SAME value and `remote_changed` would report
        // "unchanged" forever, silently overwriting an Apple-side edit. That
        // is precisely the bug class this field exists to prevent (see its
        // struct-level doc comment on `Note`). Keeping this raw means a
        // missing `lastModifiedDateTime` decodes to `""` (the field is
        // `#[serde(default)]`) instead, which `remote_changed` treats as
        // unconditionally changed — failing toward a conflict copy rather
        // than toward silent data loss. Do not "fix" this asymmetry by
        // matching `date`.
        version: m.last_modified_date_time.clone(),
        label: label_for(paths, &m.parent_folder_id),
        // None, not Some(""), for a message with no creation time: the write
        // path treats this as "preserve Apple's original", and an empty header
        // value is not that.
        x_mail_created_date: match graph_time_to_rfc2822(&m.created_date_time) {
            s if s.trim().is_empty() => None,
            s => Some(s),
        },
        account_id: Some(account_id.to_string()),
        pinned: false,
        local_version: 0,
        attachments: Vec::new(),
    }
}

/// `MessageIndex` rows from a scan — the same `label_for` derivation as
/// `to_note`, so the index and the hydrated notes cannot disagree about where a
/// note lives.
pub fn to_index(scan: &Scan) -> Vec<MessageIndex> {
    scan.messages
        .iter()
        .map(|m| MessageIndex {
            id: m.id.clone(),
            label: label_for(&scan.folder_paths, &m.parent_folder_id),
        })
        .collect()
}

/// Every note in the scan, newest first.
pub fn to_notes(scan: &Scan, account_id: &str) -> Vec<Note> {
    let mut notes: Vec<Note> = scan.messages.iter().map(|m| to_note(m, &scan.folder_paths, account_id)).collect();
    // Secondary key on uuid so notes sharing a timestamp keep a stable order
    // across refreshes (the same reason `list_trashed_notes` does it).
    notes.sort_by(|a, b| rfc2822_ms(&b.date).cmp(&rfc2822_ms(&a.date)).then_with(|| a.uuid.cmp(&b.uuid)));
    notes
}

fn rfc2822_ms(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc2822(s).map(|d| d.timestamp_millis()).unwrap_or(0)
}

/// `RemoteFolder`s from the SAME map the notes' labels came from.
pub fn to_folders(scan: &Scan) -> Vec<crate::backend::RemoteFolder> {
    let mut folders: Vec<crate::backend::RemoteFolder> = scan
        .folder_paths
        .iter()
        .map(|(id, path)| crate::backend::RemoteFolder { id: id.clone(), path: path.clone() })
        .collect();
    // HashMap iteration order is randomized per construction; sort so the
    // folder list does not reshuffle itself on every refresh.
    folders.sort_by(|a, b| a.path.cmp(&b.path));
    folders
}

/// The `Notes` root's Exchange id, if any note sits directly in it.
///
/// `/me/mailFolders` omits the Notes tree entirely and `GET /mailFolders/{id}`
/// 404s (gotcha #12), so `parentFolderId` on a message is the only route to a
/// folder id — and therefore an account with no note in the top-level folder
/// has no reachable root. `scan.folder_paths` is exactly the map `to_folders`
/// reads, so this stays in sync with whatever the folder list shows by
/// construction rather than by a second lookup that could disagree with it.
///
/// `None` is not the only failure mode. If the real root has no directly-filed
/// note (so it is absent from this scan) but some OTHER folder is visible and
/// also happens to be named `"Notes"`, that folder's id comes back unique
/// (`folder_paths` only suffixes on an actual collision within one scan) and
/// is silently accepted as the root — there is no way to tell "the true root"
/// from "a folder that merely shares its name" once the true root is
/// unreachable, because gotcha #12 leaves nesting unrecoverable either way.
pub fn notes_root_id(scan: &Scan) -> Option<String> {
    scan.folder_paths
        .iter()
        .find(|(_, path)| path.as_str() == "Notes")
        .map(|(id, _)| id.clone())
}

/// Does this folder id still exist?
///
/// `GET /mailFolders/{id}` 404s for every folder (gotcha #12), but
/// `/messages` under the same id answers 200 while the folder lives and 404
/// once it is deleted — measured 2026-08-14. That gap is the only way to tell
/// an EMPTIED folder from a DELETED one on a backend whose folder list is
/// derived from the notes inside it (`to_folders`/`list_folders`), so a
/// folder that still exists but currently holds zero notes would otherwise
/// look identical to one Apple deleted outright.
///
/// Any error other than a clean 404 answers `true`: the caller (`list_folders`)
/// prunes the local row on `false`, so a transient network failure must never
/// be read as "deleted" — that would wipe a user's folder for a reason that
/// has nothing to do with the folder.
pub async fn folder_still_exists(token: &str, folder_id: &str) -> bool {
    let url = format!(
        "{GRAPH_BASE}/me/mailFolders/{}/messages?$top=1",
        urlencoding::encode(folder_id)
    );
    !matches!(graph_get(token, &url).await, Err(TransportError::NotFound))
}

#[cfg(test)]
mod read_path_tests {
    use super::*;

    fn msg(id: &str, folder_id: &str) -> GraphMessage {
        GraphMessage {
            id: id.into(),
            subject: format!("subject {id}"),
            internet_message_id: format!("{id}@example.com"),
            parent_folder_id: folder_id.into(),
            created_date_time: "2026-08-13T17:08:23Z".into(),
            last_modified_date_time: "2026-08-13T19:48:36Z".into(),
            body_html: "<html><body><div>line</div></body></html>".into(),
            parent_folder_name: String::new(),
        }
    }

    // The whole point of Pass A's client-side check: /me/messages returns the
    // entire mailbox, ordinary email included.
    #[test]
    fn the_sticky_note_filter_keeps_notes_and_rejects_email() {
        let json = r#"{"value":[
            {"id":"note","singleValueExtendedProperties":[{"id":"String 0x001A","value":"IPM.StickyNote"}]},
            {"id":"mail","singleValueExtendedProperties":[{"id":"String 0x001A","value":"IPM.Note"}]}
        ]}"#;
        let classes = decode_message_classes(json).unwrap();
        let kept = keep_sticky_notes(vec![msg("note", "f1"), msg("mail", "f1")], &classes);
        assert_eq!(kept.len(), 1, "an IPM.Note email must not be imported as a note");
        assert_eq!(kept[0].id, "note");
    }

    #[test]
    fn a_message_whose_class_is_unreadable_is_dropped_not_imported() {
        // No property at all on this message — importing it would mean pulling
        // the user's Inbox into their notes.
        let classes = decode_message_classes(r#"{"value":[{"id":"mystery"}]}"#).unwrap();
        assert_eq!(classes["mystery"], "");
        assert!(keep_sticky_notes(vec![msg("mystery", "f1")], &classes).is_empty());
    }

    #[test]
    fn the_class_check_is_case_insensitive_on_the_property_id() {
        // Graph has been observed echoing the id with different casing.
        let json = r#"{"value":[{"id":"n","singleValueExtendedProperties":[
            {"id":"string 0x001a","value":"IPM.StickyNote"}]}]}"#;
        let classes = decode_message_classes(json).unwrap();
        assert_eq!(keep_sticky_notes(vec![msg("n", "f1")], &classes).len(), 1);
    }

    // Pass B costs one request per folder, so the ids must be DISTINCT.
    #[test]
    fn three_notes_in_two_folders_yield_two_folder_ids() {
        let msgs = vec![msg("a", "f1"), msg("b", "f2"), msg("c", "f1")];
        let ids = distinct_parent_folder_ids(&msgs);
        assert_eq!(ids, vec!["f1".to_string(), "f2".to_string()],
            "one request per FOLDER, in first-seen order — not one per message");
    }

    #[test]
    fn a_message_with_no_parent_folder_id_contributes_no_folder() {
        let msgs = vec![msg("a", ""), msg("b", "f1")];
        assert_eq!(distinct_parent_folder_ids(&msgs), vec!["f1".to_string()]);
    }

    /// Drive the whole read derivation the way `scan` does, minus HTTP:
    /// decode a Pass-A page → filter by message class → collect distinct folder
    /// ids → name them the way Pass B would → assemble.
    ///
    /// Building a `Scan` literal instead would let any implementation that
    /// merely reads `scan.folder_paths` pass, which is exactly the test that
    /// cannot fail.
    fn scan_from(pass_a_json: &str, pass_b_names: &[(&str, &str)]) -> Scan {
        let page = decode_page(pass_a_json).expect("fixture must decode");
        let classes = decode_message_classes(pass_a_json).expect("fixture must decode");
        let messages = keep_sticky_notes(page.messages, &classes);

        let pairs: Vec<(String, String)> = distinct_parent_folder_ids(&messages)
            .into_iter()
            .map(|id| {
                let name = pass_b_names
                    .iter()
                    .find(|(fid, _)| *fid == id)
                    .map(|(_, name)| (*name).to_string())
                    // Pass B answering with nothing is the same degradation the
                    // live path applies.
                    .unwrap_or_else(|| UNNAMED_FOLDER.to_string());
                (id, name)
            })
            .collect();

        assemble_scan(messages, &pairs)
    }

    fn pass_a_json(rows: &[(&str, &str, &str)]) -> String {
        let values: Vec<String> = rows
            .iter()
            .map(|(id, folder, class)| {
                format!(
                    r#"{{"id":"{id}","subject":"subject {id}","internetMessageId":"<{id}@example.com>",
                        "parentFolderId":"{folder}","createdDateTime":"2026-08-13T17:08:23Z",
                        "lastModifiedDateTime":"2026-08-13T19:48:36Z",
                        "body":{{"contentType":"html","content":"<html><body>subject {id}<div>line</div></body></html>"}},
                        "singleValueExtendedProperties":[{{"id":"String 0x001A","value":"{class}"}}]}}"#
                )
            })
            .collect();
        format!(r#"{{"value":[{}]}}"#, values.join(","))
    }

    // The divergence the one-map rule exists to prevent: if the label on a note
    // and the path in the folder list came from different folder_paths calls, a
    // collided folder would be reported under one path and its notes filed under
    // another. Driven end to end from a Pass-A page, not from a hand-built Scan.
    #[test]
    fn a_notes_label_matches_the_path_list_folders_reports() {
        // Two folders sharing the leaf name "Ideas" — the case that forces a
        // suffix — plus an ordinary email that must not become a note at all.
        let json = pass_a_json(&[
            ("a", "f1", "IPM.StickyNote"),
            ("b", "f2", "IPM.StickyNote"),
            ("mail", "inbox", "IPM.Note"),
        ]);
        let scan = scan_from(&json, &[("f1", "Ideas"), ("f2", "Ideas")]);

        let notes = to_notes(&scan, "kaiwan.h@live.com");
        let folders = to_folders(&scan);
        let index = to_index(&scan);

        assert_eq!(notes.len(), 2, "the IPM.Note email must not survive the derivation");
        assert_eq!(folders.len(), 2, "the email's Inbox must not become a folder");

        for n in &notes {
            assert!(folders.iter().any(|f| f.path == n.label),
                "note labelled '{}' has no folder with that path — it would be orphaned", n.label);
        }
        for i in &index {
            assert!(folders.iter().any(|f| f.path == i.label),
                "index row labelled '{}' has no folder with that path", i.label);
        }
        assert_ne!(notes[0].label, notes[1].label, "the two collided folders must stay distinct");
        assert!(notes.iter().all(|n| n.label.starts_with("Notes/Ideas~")), "both must be suffixed");
    }

    // The label must exist as a folder even when Pass B never named the folder.
    #[test]
    fn a_note_whose_folder_pass_b_never_named_still_lands_in_a_folder_that_exists() {
        let json = pass_a_json(&[("a", "f1", "IPM.StickyNote")]);
        let scan = scan_from(&json, &[]); // Pass B returned nothing at all
        let notes = to_notes(&scan, "acct");
        let folders = to_folders(&scan);
        assert_eq!(notes[0].label, format!("Notes/{UNNAMED_FOLDER}"), "a real folder, so it is Notes/-rooted");
        assert!(folders.iter().any(|f| f.path == notes[0].label));
    }

    // A message with an empty parentFolderId contributes no folder to Pass B, so
    // without the synthetic unfiled pair its label would name no folder row.
    #[test]
    fn an_unfiled_note_gets_a_label_that_the_folder_list_also_reports() {
        let scan = assemble_scan(vec![msg("a", "")], &[]);
        let notes = to_notes(&scan, "acct");
        let folders = to_folders(&scan);
        assert!(folders.iter().any(|f| f.path == notes[0].label),
            "label '{}' backs no folder row", notes[0].label);
    }

    // The regression the rename fixes. `Notes` is the real Exchange container
    // Apple files into — the user's PRIMARY folder. Naming the unfiled bucket
    // "Notes" and routing it through the collision machinery (correct in
    // itself) made the two collide the instant one unfiled note existed, and
    // the main folder rendered as `Notes~a1b2c3`.
    #[test]
    fn an_unfiled_note_does_not_rename_the_users_real_notes_folder() {
        let scan = assemble_scan(
            vec![msg("unfiled", ""), msg("filed", "f1")],
            &[("f1".to_string(), "Notes".to_string())],
        );
        let notes = to_notes(&scan, "acct");
        let by_id = |id: &str| notes.iter().find(|n| n.id == id).unwrap().label.clone();

        assert_eq!(by_id("filed"), "Notes",
            "the user's primary folder must keep its name when an unfiled note appears");
        assert_eq!(by_id("unfiled"), FALLBACK_LABEL);
        assert_ne!(by_id("unfiled"), by_id("filed"));
    }

    // The trade the rename accepts: a real folder a user named "Unfiled" DOES
    // still collide, and then both are suffixed apart. Rare and peripheral,
    // rather than certain and central.
    #[test]
    fn a_real_folder_sharing_the_fallback_name_is_suffixed_apart_not_merged() {
        let scan = assemble_scan(
            vec![msg("unfiled", ""), msg("filed", "f1")],
            &[("f1".to_string(), FALLBACK_LABEL.to_string())],
        );
        let notes = to_notes(&scan, "acct");
        let by_id = |id: &str| notes.iter().find(|n| n.id == id).unwrap().label.clone();
        assert_ne!(by_id("unfiled"), by_id("filed"),
            "the unfiled bucket must not absorb a genuine folder named {FALLBACK_LABEL}");
        let folders = to_folders(&scan);
        for n in &notes {
            assert!(folders.iter().any(|f| f.path == n.label), "label '{}' backs no folder row", n.label);
        }
    }

    #[test]
    fn a_note_carries_identity_created_date_and_no_attachments() {
        let scan = Scan {
            messages: vec![msg("a", "f1")],
            folder_paths: folder_paths(&[("f1".to_string(), "Notes".to_string())]),
        };
        let n = &to_notes(&scan, "acct")[0];
        assert_eq!(n.uuid, "a@example.com", "identity is internetMessageId, not an Apple X- header");
        assert_eq!(n.id, "a");
        assert_eq!(n.label, "Notes");
        assert_eq!(n.account_id.as_deref(), Some("acct"));
        assert!(n.attachments.is_empty(), "attachments are impossible on Exchange");
        assert!(n.x_mail_created_date.is_some());
    }

    // The format the rest of the codebase requires: db::CachedNote::from_remote
    // and lib::rfc2822_ms both parse `notes.date` as RFC 2822.
    #[test]
    fn dates_are_stored_in_the_rfc2822_form_the_cache_parses() {
        let scan = Scan {
            messages: vec![msg("a", "f1")],
            folder_paths: folder_paths(&[("f1".to_string(), "Notes".to_string())]),
        };
        let n = &to_notes(&scan, "acct")[0];
        assert!(chrono::DateTime::parse_from_rfc2822(&n.date).is_ok(),
            "an ISO-8601 date here sorts as epoch everywhere in db.rs/lib.rs: {}", n.date);
        assert!(chrono::DateTime::parse_from_rfc2822(n.x_mail_created_date.as_ref().unwrap()).is_ok());
    }

    #[test]
    fn an_unparseable_graph_timestamp_is_passed_through_rather_than_lost() {
        assert_eq!(graph_time_to_rfc2822("not a date"), "not a date");
    }

    #[test]
    fn notes_come_back_newest_first() {
        let mut older = msg("old", "f1");
        older.last_modified_date_time = "2020-01-01T00:00:00Z".into();
        let scan = Scan {
            messages: vec![older, msg("new", "f1")],
            folder_paths: folder_paths(&[("f1".to_string(), "Notes".to_string())]),
        };
        let notes = to_notes(&scan, "acct");
        assert_eq!(notes[0].id, "new");
    }

    #[test]
    fn the_folder_list_is_stably_ordered() {
        let pairs = vec![
            ("f1".to_string(), "Zebra".to_string()),
            ("f2".to_string(), "Alpha".to_string()),
        ];
        let scan = Scan { messages: vec![], folder_paths: folder_paths(&pairs) };
        let paths: Vec<String> = to_folders(&scan).into_iter().map(|f| f.path).collect();
        assert_eq!(paths, vec!["Notes/Alpha".to_string(), "Notes/Zebra".to_string()]);
    }

    /// A `Scan` built directly from `(id, path)` pairs with no messages —
    /// `notes_root_id` only reads `folder_paths`, so a real message set adds
    /// nothing this test needs to prove.
    fn scan_fixture(pairs: &[(&str, &str)]) -> Scan {
        let owned: Vec<(String, String)> =
            pairs.iter().map(|(id, name)| (id.to_string(), name.to_string())).collect();
        Scan { messages: Vec::new(), folder_paths: folder_paths(&owned) }
    }

    #[test]
    fn the_notes_root_is_found_only_through_a_note_filed_directly_in_it() {
        // Folder ids come only from parentFolderId on a message (gotcha #12), so
        // the root is unreachable unless a note sits directly in it.
        let with_root = scan_fixture(&[("ROOT-ID", "Notes"), ("L1-ID", "L1")]);
        assert_eq!(notes_root_id(&with_root).as_deref(), Some("ROOT-ID"));

        let without_root = scan_fixture(&[("L1-ID", "L1"), ("L2-ID", "L2")]);
        assert_eq!(notes_root_id(&without_root), None,
            "no note in the root means no id — create_folder must refuse, not guess");
    }

    #[test]
    fn a_folder_with_no_display_name_reads_the_empty_string_so_the_caller_can_substitute() {
        let json = r#"{"value":[{"id":"x"}]}"#;
        assert_eq!(decode_first_folder_name(json).unwrap(), "");
        assert_eq!(decode_first_folder_name(r#"{"value":[]}"#).unwrap(), "");
    }

    #[test]
    fn a_folder_name_is_read_from_pr_parent_display() {
        let json = r#"{"value":[{"id":"x","singleValueExtendedProperties":[
            {"id":"String 0x0E05","value":"TEST TEST L2"}]}]}"#;
        assert_eq!(decode_first_folder_name(json).unwrap(), "TEST TEST L2");
    }

    // decode_page drops a message with no internetMessageId; the folder-name
    // decoder must not, or the folder loses its name over one odd message.
    #[test]
    fn a_folder_name_survives_a_message_that_carries_no_internet_message_id() {
        let json = r#"{"value":[{"id":"x","singleValueExtendedProperties":[
            {"id":"String 0x0E05","value":"L1"}]}]}"#;
        assert!(decode_messages(json).unwrap().is_empty(), "decode_page would drop this message");
        assert_eq!(decode_first_folder_name(json).unwrap(), "L1");
    }

    // A real Exchange folder id, base64 with the characters that break a URL
    // path if they are not encoded.
    #[test]
    fn a_base64_folder_id_is_percent_encoded_into_the_path() {
        let url = folder_messages_url("AAMkA/Dg+X0AAAA==");
        assert!(!url.contains("A/Dg"), "a raw `/` would split the path segment: {url}");
        assert!(url.contains("AAMkA%2FDg%2BX0AAAA%3D%3D"), "got: {url}");
    }

    // reqwest parses every URL through `Url` before sending it. Whatever that
    // does has to be harmless, and it would otherwise only show up as a 400/404
    // against the live account.
    //
    // `Url` DOES rewrite one thing: `'` in a query becomes `%27`. That is safe —
    // the server percent-decodes before OData parses, so the string literal
    // still arrives quoted. What would NOT be safe is double-encoding our own
    // `%20` into `%2520` (the $filter would then be nonsense) or losing a path
    // segment, so those are what this asserts.
    #[test]
    fn url_parsing_neither_double_encodes_nor_splits_the_folder_id() {
        for url in [messages_url(SELECT_FULL), messages_url(SELECT_LIGHT), folder_messages_url("AAMkA/Dg+X0AAAA==")] {
            let parsed = reqwest::Url::parse(&url).unwrap_or_else(|e| panic!("{url} → {e}"));
            assert!(!parsed.as_str().contains("%25"), "an existing escape was re-encoded: {parsed}");
            // Parsing is a fixed point apart from the one quote rewrite, so a
            // retry (or a nextLink round trip) cannot drift further.
            assert_eq!(reqwest::Url::parse(parsed.as_str()).unwrap().as_str(), parsed.as_str());
            assert!(parsed.as_str().contains("%20eq%20"), "the $filter spacing must survive: {parsed}");
        }

        let folder = reqwest::Url::parse(&folder_messages_url("AAMkA/Dg+X0AAAA==")).unwrap();
        let segments: Vec<&str> = folder.path_segments().unwrap().collect();
        assert_eq!(segments, vec!["v1.0", "me", "mailFolders", "AAMkA%2FDg%2BX0AAAA%3D%3D", "messages"],
            "the folder id must stay ONE path segment");
    }

    #[test]
    fn the_scan_selects_every_field_the_note_envelope_needs() {
        // internetMessageId is load-bearing on BOTH passes: decode_page drops a
        // message without one, so omitting it from $select empties the account.
        for field in ["id", "internetMessageId", "parentFolderId"] {
            assert!(SELECT_FULL.contains(field), "SELECT_FULL missing {field}");
            assert!(SELECT_LIGHT.contains(field), "SELECT_LIGHT missing {field}");
        }
        for field in ["subject", "body", "createdDateTime", "lastModifiedDateTime"] {
            assert!(SELECT_FULL.contains(field), "SELECT_FULL missing {field}");
        }
    }

    #[test]
    fn only_one_extended_property_is_ever_requested_at_a_time() {
        let clause = expand_one_property(PROP_MESSAGE_CLASS);
        assert!(!clause.contains(" or "), "an `or` across two ids returns 200 with NO properties");
        assert!(clause.contains("String%200x001A"), "spaces must be encoded: {clause}");
    }

    /// Pins the shape measured live 2026-08-14: ONE property id ANDed with ONE
    /// value restriction inside a single `any()` lambda. Graph answers an
    /// id-only filter with a 400 ("does not match to a single extended property
    /// and a value restriction"), so a test asserting only the id would encode a
    /// request Graph refuses — both halves are asserted here for exactly that
    /// reason.
    #[test]
    fn the_sticky_note_filter_pairs_the_property_id_with_a_value_restriction() {
        let clause = sticky_note_filter();
        assert!(clause.contains("singleValueExtendedProperties/any("), "got: {clause}");
        assert!(clause.contains("String%200x001A"), "the property id must be present: {clause}");
        assert!(clause.contains("IPM.StickyNote"), "the value restriction must be present: {clause}");
        assert!(clause.contains("%20and%20"), "id and value must be ANDed, not left as id-only: {clause}");
        assert!(!clause.contains(" or "), "an `or` here is the shape gotcha #12 already found rejected");
    }

    /// The whole-mailbox pass A URL must carry the server-side filter — this is
    /// the Task 8 change itself. The `$expand` stays alongside it: `$filter`
    /// narrows which messages come back, `$expand` is still what lets
    /// `keep_sticky_notes` read the class value on the ones that do.
    #[test]
    fn the_mailbox_scan_url_carries_the_sticky_note_filter_alongside_the_expand() {
        let url = messages_url(SELECT_FULL);
        assert!(url.contains("$filter=singleValueExtendedProperties/any("), "got: {url}");
        assert!(url.contains("String%200x001A"), "got: {url}");
        assert!(url.contains("IPM.StickyNote"), "got: {url}");
        assert!(url.contains("$expand=singleValueExtendedProperties"), "the $expand must survive: {url}");
    }

    /// A folder in the Notes tree already contains nothing but notes, so the
    /// server-side filter is redundant there — see the doc comment on
    /// [`folder_notes_url`] for why it is deliberately absent. The `$expand`
    /// must still be present: `scan_messages_from` is shared with the
    /// mailbox-scan path and needs the class value for `keep_sticky_notes` and
    /// the silent-`$expand`-rejection guard either way.
    #[test]
    fn the_folder_scoped_url_has_no_sticky_note_filter() {
        let FolderRead::Scoped(url) = folder_read_plan(&ids(&[("Notes", "AAMkAGI2")]), "Notes")
        else { panic!("expected a scoped read") };
        // `$expand`'s own nested clause also literally contains the substring
        // "$filter=" (`$expand=…($filter=id eq …)`), so the marker checked here
        // is the top-level filter's distinguishing shape, not the bare string.
        assert!(!url.contains("$filter=singleValueExtendedProperties/any("),
            "a Notes-tree folder is already all notes: {url}");
        assert!(url.contains("$expand=singleValueExtendedProperties"), "the $expand must survive: {url}");
    }

    // ── pagination ───────────────────────────────────────────────────────────

    const URL_A: &str = "https://graph.microsoft.com/v1.0/me/messages?$skiptoken=A";
    const URL_B: &str = "https://graph.microsoft.com/v1.0/me/messages?$skiptoken=B";

    fn seen(urls: &[&str]) -> HashSet<String> {
        urls.iter().map(|u| (*u).to_string()).collect()
    }

    /// Walk a canned `url -> next_link` map exactly as `scan_messages` does, so
    /// the loop's termination is exercised rather than argued about.
    ///
    /// `remember_all` picks which guard is in force, and it is what lets the
    /// period-2 gap be DEMONSTRATED instead of described. `false` hands
    /// `next_page_step` a set holding only the URL just fetched, which is
    /// exactly the rule the previous version applied (`next == current_url`);
    /// `true` hands it every URL seen so far, the rule now in force.
    ///
    /// A budget stops a runaway walk so a broken guard fails the test rather
    /// than hanging CI.
    fn walk_remembering(
        pages_map: &[(&str, Option<&str>)],
        start: &str,
        remember_all: bool,
        budget: usize,
    ) -> Result<usize, TransportError> {
        let mut url = start.to_string();
        let mut visited: HashSet<String> = HashSet::new();
        let mut pages = 0usize;
        loop {
            visited.insert(url.clone());
            pages += 1;
            if pages > budget {
                return Err(TransportError::Permanent {
                    source: anyhow::anyhow!("harness budget of {budget} pages exhausted — the walk ran away"),
                });
            }

            let next = pages_map
                .iter()
                .find(|(u, _)| *u == url)
                .and_then(|(_, n)| n.map(|s| s.to_string()));

            let guard = if remember_all { visited.clone() } else { seen(&[url.as_str()]) };
            match next_page_step(next, &guard, pages)? {
                PageStep::Fetch(n) => url = n,
                PageStep::Done => return Ok(pages),
            }
        }
    }

    fn walk(pages_map: &[(&str, Option<&str>)], start: &str) -> Result<usize, TransportError> {
        walk_remembering(pages_map, start, true, 10_000)
    }

    #[test]
    fn no_next_link_ends_the_walk() {
        assert_eq!(next_page_step(None, &seen(&[URL_A]), 1).unwrap(), PageStep::Done);
    }

    #[test]
    fn a_next_link_continues_the_walk() {
        assert_eq!(
            next_page_step(Some(URL_B.into()), &seen(&[URL_A]), 1).unwrap(),
            PageStep::Fetch(URL_B.into())
        );
    }

    // Graph can hand back a page whose `value` is empty while more pages follow.
    // Stopping there would silently truncate the mailbox — the exact failure
    // Task 7 surfaced `next_link` to prevent.
    #[test]
    fn an_empty_page_carrying_a_next_link_keeps_going() {
        let json = format!(r#"{{"value":[],"@odata.nextLink":"{URL_B}"}}"#);
        let page = decode_page(&json).unwrap();
        assert!(page.messages.is_empty(), "the page really is empty");
        assert_eq!(
            next_page_step(page.next_link, &seen(&[URL_A]), 1).unwrap(),
            PageStep::Fetch(URL_B.into()),
            "an empty page must not be mistaken for the end of the mailbox"
        );
    }

    // A nextLink pointing at the page just fetched loops forever if followed.
    #[test]
    fn a_self_referential_next_link_terminates_instead_of_looping() {
        let err = walk(&[(URL_A, Some(URL_A))], URL_A)
            .expect_err("A -> A must not be followed — that is the infinite loop");
        assert!(matches!(err, TransportError::Permanent { .. }));
        assert!(err.to_string().contains("cycling"), "got: {err}");
    }

    // The guard the previous version did NOT have. Both halves are run here, so
    // the gap is demonstrated rather than described:
    //
    //   remember_all = false  → the old rule (compare against the current URL
    //                           only). A->B->A->B is never detected and the walk
    //                           runs until something else stops it — in
    //                           production, MAX_PAGES: bounded, but a hundred
    //                           thousand requests and hours before the user sees
    //                           an error.
    //   remember_all = true   → the rule now in force. Caught on the third step.
    #[test]
    fn a_two_url_cycle_terminates_instead_of_running_to_the_cap() {
        let cycle = [(URL_A, Some(URL_B)), (URL_B, Some(URL_A))];

        let old = walk_remembering(&cycle, URL_A, false, 500)
            .expect_err("the old rule cannot detect a period-2 cycle");
        assert!(old.to_string().contains("ran away"),
            "the previous guard should have run to the budget, not stopped: {old}");

        let now = walk_remembering(&cycle, URL_A, true, 500)
            .expect_err("a period-2 cycle must be caught");
        assert!(now.to_string().contains("cycling"), "got: {now}");
        assert!(now.to_string().contains(URL_A), "the message must name the repeated URL: {now}");
    }

    // A three-URL cycle is the same defect one step further out.
    #[test]
    fn a_longer_cycle_is_caught_too() {
        const URL_C: &str = "https://graph.microsoft.com/v1.0/me/messages?$skiptoken=C";
        let err = walk(
            &[(URL_A, Some(URL_B)), (URL_B, Some(URL_C)), (URL_C, Some(URL_A))],
            URL_A,
        )
        .expect_err("a period-3 cycle must be caught");
        assert!(err.to_string().contains("cycling"), "got: {err}");
    }

    // ...while a genuine multi-page walk still completes.
    #[test]
    fn a_normal_three_page_walk_completes() {
        const URL_C: &str = "https://graph.microsoft.com/v1.0/me/messages?$skiptoken=C";
        let pages = walk(
            &[(URL_A, Some(URL_B)), (URL_B, Some(URL_C)), (URL_C, None)],
            URL_A,
        )
        .expect("a normal walk must not be mistaken for a cycle");
        assert_eq!(pages, 3);
    }

    #[test]
    fn crossing_the_runaway_cap_errors_rather_than_truncating() {
        // One page below the cap still continues...
        assert!(matches!(
            next_page_step(Some(URL_B.into()), &seen(&[URL_A]), MAX_PAGES - 1),
            Ok(PageStep::Fetch(_))
        ));
        // ...and at the cap it refuses.
        let err = next_page_step(Some(URL_B.into()), &seen(&[URL_A]), MAX_PAGES).unwrap_err();
        assert!(matches!(err, TransportError::Permanent { .. }));
        let msg = err.to_string();
        assert!(msg.contains("runaway guard"), "the message must say what was hit: {msg}");
        assert!(msg.contains("not a large account"), "and what it means: {msg}");
    }

    // The two constants are a pair and the ceiling is their PRODUCT. Both halves
    // are asserted, because each guards a different real failure:

    // Ceiling too low → a real account errors out and shows zero notes. That
    // happened at 100 × 2000.
    #[test]
    fn the_mailbox_ceiling_is_the_product_and_no_real_account_reaches_it() {
        let biggest_plausible_mailbox = 1_000_000usize;
        let ceiling = MAX_PAGES * PAGE_SIZE;
        assert!(ceiling >= biggest_plausible_mailbox * 10,
            "ceiling is {ceiling} messages ({MAX_PAGES} pages x {PAGE_SIZE}); a \
             {biggest_plausible_mailbox}-message mailbox must sit an order of magnitude below it, \
             or a real account gets a hard error and sees zero notes");

        let pages_needed = biggest_plausible_mailbox.div_ceil(PAGE_SIZE);
        assert!(pages_needed < MAX_PAGES,
            "{pages_needed} pages vs a cap of {MAX_PAGES} — the cap must be a malfunction guard, \
             not a size limit");
    }

    // Page too large → a page of full bodies trips a gateway timeout, the 504
    // classifies as Transient, and the worker retries at the same $top forever.
    // Raise MAX_PAGES to grow the ceiling, never PAGE_SIZE.
    #[test]
    fn the_page_size_stays_small_enough_to_avoid_a_gateway_timeout() {
        assert!(PAGE_SIZE <= 100,
            "PAGE_SIZE is {PAGE_SIZE}: a page of that many messages each carrying a full body can \
             trip a Graph gateway timeout, and a 504 is Transient — the worker would retry at the \
             same $top forever and never converge. Grow the ceiling via MAX_PAGES instead.");
    }

    // ── the silent-$expand detector ──────────────────────────────────────────

    #[test]
    fn messages_with_no_class_value_anywhere_reads_as_a_rejected_expand() {
        assert!(expand_was_rejected_silently(true, false),
            "200 with no properties must be caught, not rendered as an empty account");
    }

    #[test]
    fn a_genuinely_empty_mailbox_is_not_mistaken_for_a_rejected_expand() {
        assert!(!expand_was_rejected_silently(false, false));
    }

    #[test]
    fn a_mailbox_of_pure_email_is_not_mistaken_for_a_rejected_expand() {
        // Messages decoded AND classes present — they are just all IPM.Note.
        assert!(!expand_was_rejected_silently(true, true));
    }

    // ── Pass B degradation ───────────────────────────────────────────────────

    #[test]
    fn a_folder_deleted_between_the_two_passes_degrades_instead_of_failing_the_sync() {
        let name = folder_name_or_fallback(Err(TransportError::NotFound)).unwrap();
        assert_eq!(name, UNNAMED_FOLDER);
    }

    #[test]
    fn an_empty_or_blank_folder_name_becomes_the_placeholder() {
        assert_eq!(folder_name_or_fallback(Ok(String::new())).unwrap(), UNNAMED_FOLDER);
        assert_eq!(folder_name_or_fallback(Ok("   ".into())).unwrap(), UNNAMED_FOLDER);
    }

    #[test]
    fn a_real_folder_name_passes_through_untouched() {
        assert_eq!(folder_name_or_fallback(Ok("TEST TEST L2".into())).unwrap(), "TEST TEST L2");
    }

    // Swallowing these would turn an expired token into a mailbox of folders all
    // named "Unnamed Folder", which then get written to the cache.
    #[test]
    fn auth_and_transient_folder_errors_still_propagate() {
        assert!(matches!(folder_name_or_fallback(Err(TransportError::Auth)), Err(TransportError::Auth)));
        assert!(matches!(
            folder_name_or_fallback(Err(TransportError::RateLimited { retry_after: None })),
            Err(TransportError::RateLimited { .. })
        ));
        assert!(matches!(
            folder_name_or_fallback(Err(TransportError::Transient { source: anyhow::anyhow!("x") })),
            Err(TransportError::Transient { .. })
        ));
    }

    // ── timestamps ───────────────────────────────────────────────────────────

    #[test]
    fn last_modified_is_preferred_when_it_parses() {
        let (date, source) = resolve_note_date("2026-08-13T19:48:36Z", "2026-08-13T17:08:23Z");
        assert_eq!(source, DateSource::LastModified);
        assert_eq!(chrono::DateTime::parse_from_rfc2822(&date).unwrap().timestamp(),
                   chrono::DateTime::parse_from_rfc3339("2026-08-13T19:48:36Z").unwrap().timestamp());
    }

    // A note that sorts to 1970 sits at the bottom of every list forever, which
    // reads as a display bug. The creation time is wrong by an edit, not by 56 years.
    #[test]
    fn an_unusable_last_modified_falls_back_to_the_creation_time() {
        let (date, source) = resolve_note_date("", "2026-08-13T17:08:23Z");
        assert_eq!(source, DateSource::Created);
        assert!(chrono::DateTime::parse_from_rfc2822(&date).is_ok(), "got: {date}");

        let (_, source) = resolve_note_date("not a date", "2026-08-13T17:08:23Z");
        assert_eq!(source, DateSource::Created);
    }

    #[test]
    fn neither_timestamp_parsing_is_reported_rather_than_silently_epoch() {
        let (date, source) = resolve_note_date("garbage", "also garbage");
        assert_eq!(source, DateSource::Unparseable, "the caller must be able to log this");
        assert_eq!(date, "garbage", "the raw value is passed through, not invented");
    }

    #[test]
    fn a_message_with_no_creation_time_carries_none_not_an_empty_header() {
        let mut m = msg("a", "f1");
        m.created_date_time = String::new();
        let scan = assemble_scan(vec![m], &[("f1".to_string(), "Notes".to_string())]);
        assert_eq!(to_notes(&scan, "acct")[0].x_mail_created_date, None);
    }

    #[test]
    fn graph_fractional_seconds_are_accepted() {
        // Graph emits both `…:36Z` and `…:36.0000000Z` depending on the endpoint.
        let (_, source) = resolve_note_date("2026-08-13T19:48:36.0000000Z", "");
        assert_eq!(source, DateSource::LastModified);
    }

    #[test]
    fn http_statuses_map_to_the_variants_the_worker_retries_on() {
        assert!(matches!(classify_status(401, None, ""), TransportError::Auth));
        assert!(matches!(classify_status(403, None, ""), TransportError::Auth));
        assert!(matches!(classify_status(404, None, ""), TransportError::NotFound));
        assert!(matches!(classify_status(503, None, ""), TransportError::Transient { .. }));
        assert!(matches!(classify_status(400, None, ""), TransportError::Permanent { .. }));
        assert!(matches!(
            classify_status(429, Some(30), ""),
            TransportError::RateLimited { retry_after: Some(d) } if d == Duration::from_secs(30)
        ));
        assert!(matches!(classify_status(429, None, ""), TransportError::RateLimited { retry_after: None }));
    }
    // ── C1: the scoped folder read ───────────────────────────────────────────

    fn ids(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(p, i)| ((*p).to_string(), (*i).to_string())).collect()
    }

    /// The defect this whole fix exists for: a scoped read that paginates
    /// `/me/messages`. The UI sweeps one folder every 2500 ms, so the endpoint
    /// the plan names is the difference between one request and a full mailbox
    /// pass with bodies.
    #[test]
    fn a_known_folder_is_addressed_directly_never_through_the_mailbox() {
        let plan = folder_read_plan(&ids(&[("Notes", "AAMkAGI2")]), "Notes");
        let FolderRead::Scoped(url) = plan else { panic!("a known folder must not scan the mailbox") };
        assert!(url.contains("/me/mailFolders/AAMkAGI2/messages"), "got: {url}");
        assert!(!url.contains("/me/messages"), "this is the whole-mailbox endpoint: {url}");
        assert!(url.contains(PROP_MESSAGE_CLASS.replace(' ', "%20").as_str()),
            "the scoped read still has to tell notes from mail: {url}");
    }

    /// Exchange folder ids are base64 and really do contain `/`, `+` and `=`.
    /// Left raw, the `/` splits the path and Graph 404s on a truncated id.
    #[test]
    fn the_scoped_read_percent_encodes_a_base64_folder_id() {
        let FolderRead::Scoped(url) = folder_read_plan(&ids(&[("Notes", "a/b+c=")]), "Notes")
        else { panic!("expected a scoped read") };
        assert!(url.contains("a%2Fb%2Bc%3D"), "got: {url}");
        assert!(!url.contains("/me/mailFolders/a/b"), "an unencoded id truncates the folder: {url}");
    }

    /// A miss must not become an empty list: `prune_clean_in_label` would then
    /// delete every clean cached row under the path.
    #[test]
    fn an_unknown_or_idless_path_falls_back_to_the_mailbox_rather_than_to_nothing() {
        assert_eq!(folder_read_plan(&ids(&[("Notes", "x")]), "Notes/Gone"), FolderRead::MailboxFallback);
        assert_eq!(folder_read_plan(&ids(&[("Notes", "   ")]), "Notes"), FolderRead::MailboxFallback);
        assert_eq!(folder_read_plan(&HashMap::new(), "Notes"), FolderRead::MailboxFallback);
    }

    /// Every message came out of the folder that was asked for, so its label is
    /// the caller's own path — no Pass B, and no chance of disagreeing with the
    /// id form Graph echoes back in `parentFolderId`.
    #[test]
    fn a_scoped_scan_labels_every_note_with_the_requested_path() {
        let messages = vec![msg("a", "echoed-in-another-form"), msg("b", "")];
        let mut folder_paths: HashMap<String, String> = distinct_parent_folder_ids(&messages)
            .into_iter()
            .map(|id| (id, "Notes/Ideas~ab12cd".to_string()))
            .collect();
        folder_paths.insert(UNFILED_FOLDER_ID.to_string(), "Notes/Ideas~ab12cd".to_string());
        let scan = Scan { messages, folder_paths };

        for n in to_notes(&scan, "acct") {
            assert_eq!(n.label, "Notes/Ideas~ab12cd",
                "a scoped read knows the folder — nothing may land in Unfiled");
        }
    }

    // ── C2: Deleted Items is inside /me/messages ─────────────────────────────

    #[test]
    fn a_note_sitting_in_deleted_items_is_not_reported_as_live() {
        let messages = vec![msg("live", "notes-folder"), msg("trashed", "deleted-items")];
        let excluded: HashSet<String> = ["deleted-items".to_string()].into_iter().collect();
        let kept = exclude_folders(messages, &excluded);
        assert_eq!(kept.len(), 1, "a note trashed on the iPhone must not resurrect");
        assert_eq!(kept[0].id, "live");
    }

    /// Resolution is best effort — an empty exclusion set must leave the scan
    /// untouched rather than dropping everything.
    #[test]
    fn an_unresolvable_deleted_items_folder_costs_nothing_but_the_exclusion() {
        let messages = vec![msg("a", "f1"), msg("b", "f2")];
        assert_eq!(exclude_folders(messages, &HashSet::new()).len(), 2);
    }

    #[test]
    fn the_well_known_folder_lookup_reads_the_id_graph_returns() {
        assert_eq!(decode_folder_id(r#"{"id":"AAMkAGdel","displayName":"Deleted Items"}"#).unwrap(), "AAMkAGdel");
        assert!(decode_folder_id(r#"{"error":{"code":"ErrorItemNotFound"}}"#).is_err(),
            "a Graph error body must not decode as a folder id");
    }

    #[test]
    fn both_stages_of_exchange_deletion_are_excluded() {
        assert!(EXCLUDED_WELL_KNOWN_FOLDERS.contains(&"deleteditems"));
        assert!(EXCLUDED_WELL_KNOWN_FOLDERS.contains(&"recoverableitemsdeletions"),
            "emptying Deleted Items moves the item to the dumpster, still IPM.StickyNote");
    }
}

/// Tests written against the shape Graph **returns**, not the shape we send.
///
/// Every other fixture in this file was hand-written with `String 0x001A` —
/// the id the request carries — and all of them passed while a real mailbox
/// read as empty, because Graph answers with `String 0x1a`. These use the ids
/// captured from a live `/me/messages` response on 2026-08-14, so they fail if
/// anyone reintroduces a literal comparison against the constant.
#[cfg(test)]
mod live_response_shape_tests {
    use super::*;

    /// Verbatim from the live account: lower-cased, leading zeros stripped.
    const LIVE_CLASS_ID: &str = "String 0x1a";
    const LIVE_PARENT_ID: &str = "String 0xe05";

    #[test]
    fn the_id_graph_returns_matches_the_id_we_sent() {
        assert!(same_property_id(LIVE_CLASS_ID, PROP_MESSAGE_CLASS));
        assert!(same_property_id(LIVE_PARENT_ID, PROP_PARENT_DISPLAY));
        // and symmetrically, since call sites differ in argument order
        assert!(same_property_id(PROP_MESSAGE_CLASS, LIVE_CLASS_ID));
    }

    #[test]
    fn different_properties_still_do_not_match() {
        assert!(!same_property_id(LIVE_CLASS_ID, PROP_PARENT_DISPLAY));
        assert!(!same_property_id("String 0x1a", "Integer 0x1a"),
            "the type half is part of the identity, not decoration");
        assert!(!same_property_id("String 0x1a", "String 0x1b"));
    }

    #[test]
    fn an_unparseable_id_falls_back_to_a_string_compare() {
        // Named properties under a GUID are not `<type> 0x<hex>`; this backend
        // does not use them, but the helper must not claim two are equal.
        let guid = "String {00020386-0000-0000-C000-000000000046} Name x-custom";
        assert!(same_property_id(guid, guid));
        assert!(!same_property_id(guid, PROP_MESSAGE_CLASS));
    }

    #[test]
    fn a_live_shaped_response_yields_notes_rather_than_a_silent_empty() {
        // Two messages as Graph really returns them — one note, one ordinary
        // mail — with the response-side property id.
        let json = format!(
            r#"{{"value":[
              {{"id":"m1","subject":"a note","internetMessageId":"<n1@x>",
                "parentFolderId":"F1","createdDateTime":"2026-01-01T00:00:00Z",
                "lastModifiedDateTime":"2026-01-01T00:00:00Z",
                "body":{{"contentType":"html","content":"<html><body>a note</body></html>"}},
                "singleValueExtendedProperties":[{{"id":"{LIVE_CLASS_ID}","value":"IPM.StickyNote"}}]}},
              {{"id":"m2","subject":"an email","internetMessageId":"<e1@x>",
                "parentFolderId":"F2","createdDateTime":"2026-01-01T00:00:00Z",
                "lastModifiedDateTime":"2026-01-01T00:00:00Z",
                "body":{{"contentType":"html","content":"<html><body>an email</body></html>"}},
                "singleValueExtendedProperties":[{{"id":"{LIVE_CLASS_ID}","value":"IPM.Note"}}]}}
            ]}}"#
        );
        let page = decode_page(&json).expect("decodes");
        let classes = decode_message_classes(&json).expect("class map parses");

        // The whole bug in one assertion: against a live-shaped id this map was
        // empty, so nothing matched and the scan concluded the $expand had been
        // refused.
        assert_eq!(classes.get("m1").map(String::as_str), Some("IPM.StickyNote"));
        assert_eq!(classes.get("m2").map(String::as_str), Some("IPM.Note"));

        let kept = keep_sticky_notes(page.messages, &classes);
        assert_eq!(kept.len(), 1, "the note survives and the email does not");
        assert_eq!(kept[0].subject, "a note");
        assert!(
            !expand_was_rejected_silently(true, !classes.is_empty()),
            "reading a live-shaped class value must not look like a refused $expand"
        );
    }

    #[test]
    fn a_live_shaped_folder_name_is_read_from_the_parent_display_property() {
        let json = format!(
            r#"{{"value":[
              {{"id":"m1","subject":"s","internetMessageId":"<n1@x>","parentFolderId":"F1",
                "createdDateTime":"2026-01-01T00:00:00Z","lastModifiedDateTime":"2026-01-01T00:00:00Z",
                "body":{{"contentType":"html","content":"<html><body>s</body></html>"}},
                "singleValueExtendedProperties":[{{"id":"{LIVE_PARENT_ID}","value":"TEST TEST L2"}}]}}
            ]}}"#
        );
        let page = decode_page(&json).expect("decodes");
        assert_eq!(page.messages[0].parent_folder_name, "TEST TEST L2");
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Graph write path — create, patch, move, delete
// ═════════════════════════════════════════════════════════════════════════════
//
// Milestone 1 was read-only. These four are Task 5's write requests: pure
// request builders over `graph_send`, decoded with `decode_single_message`.
// `PATCH` is a REAL in-place update — Gmail's insert-new + trash-old +
// `mark_pushed` id-repair sequence has no counterpart here and must not be
// copied over; doing so would leave two live messages per edit.

/// Best-effort recovery when a write response's body doesn't decode.
///
/// **Unmeasured, not routine.** Every observation of `internetMessageId` to
/// date (M1, and Task 5's own tests) has been a READ-back — nobody has yet
/// looked at what a Graph write **response** body actually contains. If it
/// ever omits `internetMessageId`, the naive path (`decode_single_message`
/// straight into `decode_err`) would report a note Graph actually created —
/// or patched, or moved — as a `Permanent` failure, and the caller (Task 6)
/// would either retry the push (a duplicate note) or drop the edit. Neither
/// is acceptable for something that already SUCCEEDED server-side.
///
/// So: on a decode failure, pull `id` out of the same raw body (a field
/// [`RawMessage`] always requires, independent of `internetMessageId`) and
/// issue exactly ONE follow-up `GET /me/messages/{id}` — the read path is
/// already proven to return a decodable body — then decode that. If the
/// refetch itself fails, that error (properly classified — rate limit, auth,
/// transient) propagates as-is rather than being swallowed into a
/// `Permanent`. Only if the id can't even be extracted, or the refetch ALSO
/// omits `internetMessageId`, is this genuinely unrecoverable: the note
/// exists but cannot be identified, and `Permanent` is the honest answer.
///
/// Task 12's live-probe list now includes logging a real write response body
/// and recording whether `internetMessageId` is present — this function is
/// correct either way that probe comes back; the probe only tells us whether
/// the fallback path below ever actually fires.
async fn decode_or_refetch(token: &str, what: &str, raw: &str) -> Result<GraphMessage, TransportError> {
    match decode_single_message(raw) {
        Ok(msg) => {
            // The direct answer to Task 12's deferred question: the write
            // response decoded on the first try, so `internetMessageId` WAS
            // present and non-empty — the fallback GET below did not fire.
            // Gated behind the SAME toggle `graph_send`'s body log uses (see
            // `LOG_WRITE_BODIES`'s doc comment) — this line names only an id,
            // never note content, but it still fires on every create/patch/
            // move, forever, once shipped, and the measurement it exists for
            // is the probe's alone. If a future task wants this permanently
            // (not just for a one-off live probe), that's a decision to make
            // explicitly, not a default this line should assume for itself.
            if write_body_logging_enabled() {
                crate::log!(
                    "microsoft {what}: internetMessageId present in the write response \
                     ({}) — no fallback GET needed",
                    msg.internet_message_id
                );
            }
            Ok(msg)
        }
        Err(e) => {
            let Some(id) = extract_message_id(raw).filter(|id| !id.is_empty()) else {
                return Err(decode_err(what, e));
            };
            if write_body_logging_enabled() {
                crate::log!(
                    "microsoft {what}: write response had no usable internetMessageId \
                     (decode error: {e}) — falling back to GET /me/messages/{id}"
                );
            }
            let url = format!("{GRAPH_BASE}/me/messages/{}?$select={SELECT_FULL}", urlencoding::encode(&id));
            let refetched = graph_get(token, &url).await?;
            decode_single_message(&refetched).map_err(|e2| decode_err(what, e2))
        }
    }
}

/// Pull just `id` out of a Graph message body, independent of whether the
/// rest of it deserializes as a [`RawMessage`] — the one field
/// [`decode_or_refetch`] needs when the full decode has already failed.
fn extract_message_id(json: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct JustId {
        id: String,
    }
    serde_json::from_str::<JustId>(json).ok().map(|j| j.id)
}

/// `POST` a new note into a folder.
///
/// **The message class is not optional.** Measured 2026-08-14: Graph defaults
/// to `IPM.Note` and answers 201, so the item lands as an EMAIL in the user's
/// Notes folder with no error anywhere. `isDraft: true` comes back either
/// way, so it is not the discriminator — the class is.
pub async fn create_note(
    token: &str,
    folder_id: &str,
    title: &str,
    body_html: &str,
) -> Result<GraphMessage, TransportError> {
    let payload = serde_json::json!({
        "subject": title,
        "body": { "contentType": "HTML",
                  "content": inject_title_into_body_ms(title, body_html) },
        "singleValueExtendedProperties": [
            { "id": PROP_MESSAGE_CLASS, "value": CLASS_STICKY_NOTE }
        ],
    });
    let url = format!("{GRAPH_BASE}/me/mailFolders/{}/messages", urlencoding::encode(folder_id));
    let raw = graph_send(token, reqwest::Method::POST, &url, Some(payload)).await?;
    decode_or_refetch(token, "create_note", &raw).await
}

/// `PATCH` a note in place. Subject and the leading body line move together,
/// because Apple duplicates the title into the body.
///
/// This is a REAL in-place update — the id is unchanged (measured). Do NOT
/// copy Gmail's insert-new + trash-old sequence; it would leave two live
/// messages per edit.
pub async fn patch_note(
    token: &str,
    remote_id: &str,
    title: &str,
    body_html: &str,
) -> Result<GraphMessage, TransportError> {
    let payload = serde_json::json!({
        "subject": title,
        "body": { "contentType": "HTML",
                  "content": inject_title_into_body_ms(title, body_html) },
    });
    let url = format!("{GRAPH_BASE}/me/messages/{}", urlencoding::encode(remote_id));
    let raw = graph_send(token, reqwest::Method::PATCH, &url, Some(payload)).await?;
    decode_or_refetch(token, "patch_note", &raw).await
}

/// Move a note between folders. The id survives ONLY because every request
/// carries `Prefer: IdType="ImmutableId"` — measured, with the contrast run
/// documented on [`PREFER_HEADER`]/[`graph_send`].
pub async fn move_note_to_folder(
    token: &str,
    remote_id: &str,
    folder_id: &str,
) -> Result<GraphMessage, TransportError> {
    let payload = serde_json::json!({ "destinationId": folder_id });
    let url = format!("{GRAPH_BASE}/me/messages/{}/move", urlencoding::encode(remote_id));
    let raw = graph_send(token, reqwest::Method::POST, &url, Some(payload)).await?;
    decode_or_refetch(token, "move_note", &raw).await
}

/// Delete a note. **Hard delete, no undo** — measured: after an Apple-side
/// delete, Deleted Items held zero items and the note was absent from every
/// scan. `Capabilities::has_trash` is false on this evidence.
pub async fn delete_message(token: &str, remote_id: &str) -> Result<(), TransportError> {
    let url = format!("{GRAPH_BASE}/me/messages/{}", urlencoding::encode(remote_id));
    graph_send(token, reqwest::Method::DELETE, &url, None).await.map(|_| ())
}

// ── M4: pin sidecar (a named property on the note itself, no second object) ─

/// `$filter` URL resolving a note's Exchange message id from its uuid (which
/// IS the note's `internetMessageId` on this backend — no Apple `X-` header
/// exists here, see this module's doc comment). `MetadataSidecar` callers
/// (`push_one_pin`/`push_one_deletion` in lib.rs) only ever hold the uuid, not
/// a Graph message id, so this lookup is the one step `save_note_full`'s
/// caller-supplied `existing_remote_id` never needed.
///
/// **The uuid is wrapped back in angle brackets before filtering.**
/// `strip_message_id_brackets` strips exactly one layer to produce Jodd's
/// uuid on the read path; Graph's own stored `internetMessageId` value (what
/// `$filter` actually matches against) carries them, per every live fixture
/// captured so far (see `decode_tests`). **Not yet live-verified** — this is
/// the M4 design spec's one open mechanism question, to be confirmed before
/// this function ships (Phase 1 of that spec's implementation order).
///
/// `$top=2`, not `1`: a single unexpected extra match must be visible as
/// "more than one row came back", not silently truncated to whichever the
/// server happened to return first.
///
/// **Escapes an embedded `'` before percent-encoding.** `uuid` here is not a
/// value Jodd controls — it's Exchange's own `internetMessageId` (see the
/// doc comment above), and RFC 5322's `msg-id` grammar permits `'` in the
/// `id-left`/`id-right` `dot-atom` parts. `urlencoding::encode` alone only
/// percent-encodes the byte (`'` → `%27`), which Graph decodes right back to
/// a literal `'` before OData parses the `$filter` — landing an unescaped
/// quote inside the single-quoted string literal instead of the doubled
/// `''` OData string-literal escaping requires, and either 400ing or
/// truncating the filter at the wrong point. Doubling first, THEN
/// percent-encoding, is what every other `$filter`/`$expand` value in this
/// file gets away without: they interpolate fixed, code-owned constants
/// (`PROP_MESSAGE_CLASS`, `CLASS_STICKY_NOTE`, …) that never contain a
/// quote, so this is the one call site that needs it.
fn resolve_by_uuid_url(uuid: &str) -> String {
    let raw_id = format!("<{uuid}>");
    let escaped = raw_id.replace('\'', "''");
    format!(
        "{GRAPH_BASE}/me/messages?$top=2&$select=id&$filter=internetMessageId%20eq%20'{}'",
        urlencoding::encode(&escaped)
    )
}

/// Pull just the `id` field out of a Graph messages-list response —
/// [`resolve_message_id_by_uuid`]'s decode. Deliberately not
/// [`decode_messages`]: that decoder requires `internetMessageId` on every
/// item (dropping anything without one) and this `$select` never asks Graph
/// for that field at all, only `id`.
fn decode_message_ids(json: &str) -> Result<Vec<String>, String> {
    #[derive(serde::Deserialize)]
    struct RawList { value: Vec<RawIdOnly> }
    #[derive(serde::Deserialize)]
    struct RawIdOnly { id: String }

    let list: RawList = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(list.value.into_iter().map(|m| m.id).collect())
}

#[cfg(test)]
mod resolve_by_uuid_url_tests {
    use super::resolve_by_uuid_url;

    /// The regression this guards: an `internetMessageId` containing an
    /// apostrophe (RFC 5322 `msg-id` grammar permits one) used to land an
    /// unescaped `'` inside the OData `$filter`'s single-quoted string
    /// literal, breaking the filter instead of matching the note.
    #[test]
    fn an_apostrophe_in_the_uuid_is_doubled_not_left_bare() {
        let url = resolve_by_uuid_url("o'brien@example.com");
        assert!(
            url.contains(&urlencoding::encode("<o''brien@example.com>").into_owned()),
            "the apostrophe must be doubled (OData's escape for an embedded quote) \
             before percent-encoding, got: {url}"
        );
        assert!(
            !url.contains(&urlencoding::encode("<o'brien@example.com>").into_owned()),
            "a single, unescaped quote must never reach the filter: {url}"
        );
    }

    #[test]
    fn a_uuid_with_no_apostrophe_is_unaffected() {
        let url = resolve_by_uuid_url("plain-uuid@example.com");
        assert!(url.contains(&urlencoding::encode("<plain-uuid@example.com>").into_owned()));
    }

    /// Two apostrophes must each be doubled independently, not merged or
    /// miscounted — a plausible off-by-one for a naive escape.
    #[test]
    fn multiple_apostrophes_are_each_doubled() {
        let url = resolve_by_uuid_url("o'brien's-note@example.com");
        assert!(
            url.contains(&urlencoding::encode("<o''brien''s-note@example.com>").into_owned()),
            "got: {url}"
        );
    }
}

/// Resolve a note's uuid to its current Exchange message id. Exactly one
/// match is the only acceptable answer: zero means the note is not (or not
/// yet) on the server, and Graph returning more than one for a single
/// `internetMessageId` would mean Jodd's identity assumption for this backend
/// (uuid == `internetMessageId`, unique) no longer holds — either way, a
/// caller silently picking "the first one" would be guessing.
pub async fn resolve_message_id_by_uuid(token: &str, uuid: &str) -> Result<String, TransportError> {
    let url = resolve_by_uuid_url(uuid);
    let json = graph_get(token, &url).await?;
    let ids = decode_message_ids(&json).map_err(|e| decode_err("resolve_message_id_by_uuid", e))?;
    match ids.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(TransportError::NotFound),
        many => Err(TransportError::Permanent {
            source: anyhow::anyhow!(
                "internetMessageId '{uuid}' matched {} messages, expected exactly one — \
                 the uuid==internetMessageId identity assumption for this backend does not \
                 hold for this note",
                many.len()
            ),
        }),
    }
}

/// `PATCH` just [`JODD_PIN_PROP`] on a message already resolved to a Graph id
/// — no `subject`/`body` in the payload, so a note's content is untouched.
/// Measured live: this survives (and is survived by) an ordinary content
/// `PATCH` — see [`JODD_PIN_PROP`]'s doc comment — so the pin push path and
/// the save path never need to coordinate.
///
/// **Returns the resulting `GraphMessage`, not `()`.** A property-only PATCH
/// is still a PATCH to the note's own message object; Graph's ordinary PATCH
/// semantics bump `lastModifiedDateTime` on any field change, property
/// updates included, and this backend's whole conflict-detection scheme
/// depends on `notes.remote_version` tracking that field exactly (see
/// `SavedNote::version`'s doc comment on the content-push path). Discarding
/// this response — as this function did before — left the pin push silently
/// desyncing the cached version: `reconcile_one` would then read a fast
/// follow-up local edit as a remote conflict that never happened. Reuses
/// [`decode_or_refetch`] rather than a bespoke decode, same as
/// [`patch_note`]/[`move_note_to_folder`].
async fn patch_pin_property(token: &str, message_id: &str, value_json: &str) -> Result<GraphMessage, TransportError> {
    let payload = serde_json::json!({
        "singleValueExtendedProperties": [
            { "id": JODD_PIN_PROP, "value": value_json }
        ],
    });
    let url = format!("{GRAPH_BASE}/me/messages/{}", urlencoding::encode(message_id));
    let raw = graph_send(token, reqwest::Method::PATCH, &url, Some(payload)).await?;
    decode_or_refetch(token, "patch_pin_property", &raw).await
}

/// `MetadataSidecar::put_sidecar(Pin)`'s implementation. Resolves the uuid to
/// a message id, then writes `value_json` (the caller's payload, forwarded
/// as-is — Pin's body is `{"pinned":true}` today, but this does not assume
/// that shape) onto the note.
///
/// Returns the uuid itself, not a second object's id — there is no second
/// object on this backend, the property lives on the note. The caller
/// (`push_one_pin`, lib.rs) stores this back as `meta_msg_id` and later hands
/// it straight to [`remove_pin`], which is why self-referential round-trips
/// correctly with no special-casing on the caller's side. Also returns the
/// note's resulting remote version/date — see [`patch_pin_property`]'s doc
/// comment for why that must not be discarded.
pub async fn put_pin(
    token: &str,
    uuid: &str,
    value_json: &str,
) -> Result<(String, crate::backend::RemoteNoteVersion), TransportError> {
    let message_id = resolve_message_id_by_uuid(token, uuid).await?;
    let msg = patch_pin_property(token, &message_id, value_json).await?;
    Ok((uuid.to_string(), graph_message_to_remote_version(&msg)))
}

/// `MetadataSidecar::remove_sidecar`'s implementation for a pin-originated
/// id (the only kind Microsoft ever receives one for — see the M4 design
/// spec's "Decisions locked in brainstorming").
///
/// **Writes `pinned: false`; does not delete the property.** Untested and
/// unnecessary risk for a behavior-equivalent outcome — `list_sidecars(Pin)`
/// reads whatever value is stored, so leaving a stale `pinned: true` behind
/// would make an unpinned note re-pin itself on the next cold start, and a
/// property-only PATCH already proved it can update independently of
/// content (see [`JODD_PIN_PROP`]'s doc comment). Also returns the note's
/// resulting remote version/date — see [`patch_pin_property`]'s doc comment.
pub async fn remove_pin(
    token: &str,
    uuid: &str,
) -> Result<crate::backend::RemoteNoteVersion, TransportError> {
    let message_id = resolve_message_id_by_uuid(token, uuid).await?;
    let msg = patch_pin_property(token, &message_id, &pin_property_value(false)).await?;
    Ok(graph_message_to_remote_version(&msg))
}

/// Shared conversion from a decoded `GraphMessage` to the
/// `(version, date)` pair `notes.remote_version`/`notes.date` expect — the
/// same derivation `MicrosoftVertical::save_note_full` uses for the
/// content-push path, factored out so every other write that also touches
/// the note's own object (the pin PATCH, `Transport::move_note`) stays in
/// lock-step with it rather than re-deriving it and risking the two drifting
/// apart. `pub(crate)` so `transport.rs`'s `move_note` can reuse it too.
pub(crate) fn graph_message_to_remote_version(msg: &GraphMessage) -> crate::backend::RemoteNoteVersion {
    crate::backend::RemoteNoteVersion {
        version: msg.last_modified_date_time.clone(),
        date: graph_time_to_rfc2822(&msg.last_modified_date_time),
    }
}

/// First-page URL for `MetadataSidecar::list_sidecars(Pin)`'s reconciliation
/// scan — every message's `internetMessageId` + [`JODD_PIN_PROP`], no bodies.
///
/// Deliberately lighter than [`messages_url`]: this needs neither
/// [`sticky_note_filter`] nor [`PROP_MESSAGE_CLASS`] — a message that is not a
/// note simply carries no `JODD_PIN_PROP` value (only `put_pin` ever writes
/// one, and it only runs against notes), so it decodes to "not pinned" and is
/// dropped by [`list_pinned`] like any other unpinned message, with no need
/// to ask Graph to pre-filter by class. Combining a `$filter` on one property
/// with an `$expand` on a DIFFERENT one has not been measured — gotcha #12's
/// proven combination is filter-and-expand on the SAME property
/// (`PROP_MESSAGE_CLASS`) — so this stays a plain, unfiltered scan rather
/// than assuming an unproven combination is safe.
fn pin_scan_url() -> String {
    format!(
        "{GRAPH_BASE}/me/messages?$top={PAGE_SIZE}&$select=id,internetMessageId&{}",
        expand_one_property(JODD_PIN_PROP)
    )
}

/// One decoded pin-scan row. `pinned` is the raw [`decode_pin_value`] read —
/// [`list_pinned`] is what filters to pinned-only, so a false value still
/// flows through this far (unlike [`raw_to_graph_message`], nothing here
/// drops a row for having no property at all; that is indistinguishable from
/// an explicit `pinned:false` and both mean "not pinned").
struct PinScanRow {
    uuid: String,
    pinned: bool,
}

/// Decode one page of [`pin_scan_url`]'s response.
///
/// **Does not use [`same_property_id`]** — see [`JODD_PIN_PROP`]'s doc
/// comment for why a named property compares by plain string equality
/// instead. A row whose `internetMessageId` is empty (or bracket-only) is
/// dropped: it has no uuid, so `SidecarRecord::note_uuid` would have nothing
/// to name.
fn decode_pin_page(json: &str) -> Result<(Vec<PinScanRow>, Option<String>), String> {
    #[derive(serde::Deserialize)]
    struct RawList {
        value: Vec<RawRow>,
        #[serde(rename = "@odata.nextLink", default)]
        next_link: Option<String>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawRow {
        #[serde(default)]
        internet_message_id: String,
        #[serde(default)]
        single_value_extended_properties: Vec<RawExtProp>,
    }

    let list: RawList = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let rows = list
        .value
        .into_iter()
        .filter_map(|m| {
            let uuid = strip_message_id_brackets(&m.internet_message_id);
            if uuid.is_empty() {
                return None;
            }
            let pinned = find_ext_prop(&m.single_value_extended_properties, |id| id == JODD_PIN_PROP)
                .is_some_and(decode_pin_value);
            Some(PinScanRow { uuid, pinned })
        })
        .collect();
    Ok((rows, list.next_link))
}

/// `MetadataSidecar::list_sidecars(Pin)`'s implementation — a full paginated
/// mailbox scan, kept to pinned-only rows.
///
/// **Always `Ok(vec)`, never a "store absent" signal.** Unlike Gmail's
/// meta-label sidecar, there is no separate store that can be missing here —
/// the property either exists on a note or it doesn't. `sync_pin_state`
/// (lib.rs) treats an empty vec as "prune every local pin", which is correct
/// for an account with zero pinned notes; it is Component A/Capabilities'
/// job (not this function's) to keep this off the reconciliation path until
/// M4 ships.
///
/// **Cost, accepted:** one full mailbox pagination per call — today that is
/// cold-start only (`App.svelte` calls `sync_pin_state` there), not a
/// per-sync-tick cost. See the M4 design spec's Component E for the
/// reasoning and the condition under which this should be revisited.
pub async fn list_pinned(token: &str) -> Result<Vec<SidecarRecord>, TransportError> {
    let mut url = pin_scan_url();
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pages = 0usize;

    loop {
        seen.insert(url.clone());
        let json = graph_get(token, &url).await?;
        let (rows, next_link) = decode_pin_page(&json).map_err(|e| decode_err("list_pinned", e))?;
        out.extend(rows.into_iter().filter(|r| r.pinned).map(|r| SidecarRecord {
            id: r.uuid.clone(),
            note_uuid: r.uuid,
            kind: SidecarKind::Pin,
            body: None,
        }));
        pages += 1;

        match next_page_step(next_link, &seen, pages)? {
            PageStep::Fetch(next) => url = next,
            PageStep::Done => break,
        }
    }

    Ok(out)
}

#[cfg(test)]
mod pin_sidecar_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One page with a pinned note, an explicitly-unpinned note, a note that
    /// never got the property at all, and a note whose property value is
    /// garbage — the mixed fixture the M4 design spec's Verification table
    /// asks for ("a fixture with mixed pinned/unpinned/malformed-property
    /// notes — only pinned ones come back").
    const MIXED_FIXTURE: &str = r#"{
      "value": [
        {"id":"m1","internetMessageId":"<pinned@x>",
         "singleValueExtendedProperties":[
           {"id":"String {98d27db2-72ad-4511-a3b5-b73c7c42694b} Name JoddPin","value":"{\"pinned\":true}"}
         ]},
        {"id":"m2","internetMessageId":"<unpinned@x>",
         "singleValueExtendedProperties":[
           {"id":"String {98d27db2-72ad-4511-a3b5-b73c7c42694b} Name JoddPin","value":"{\"pinned\":false}"}
         ]},
        {"id":"m3","internetMessageId":"<never-pinned@x>","singleValueExtendedProperties":[]},
        {"id":"m4","internetMessageId":"<malformed@x>",
         "singleValueExtendedProperties":[
           {"id":"String {98d27db2-72ad-4511-a3b5-b73c7c42694b} Name JoddPin","value":"not json"}
         ]}
      ]
    }"#;

    #[test]
    fn decode_pin_page_keeps_every_row_but_flags_only_the_real_pin() {
        let (rows, next_link) = decode_pin_page(MIXED_FIXTURE).expect("valid fixture");
        assert!(next_link.is_none());
        assert_eq!(rows.len(), 4, "every row with a uuid survives decode — filtering is list_pinned's job");

        let pinned: Vec<&str> = rows.iter().filter(|r| r.pinned).map(|r| r.uuid.as_str()).collect();
        assert_eq!(pinned, vec!["pinned@x"], "only the explicit pinned:true row must flag as pinned");
    }

    /// A local server serving [`MIXED_FIXTURE`] — proves `list_pinned` end to
    /// end, not just its decode half: a bug that decoded correctly but forgot
    /// to filter (or filtered the wrong way) would still pass
    /// `decode_pin_page_keeps_every_row_but_flags_only_the_real_pin` above.
    #[tokio::test]
    async fn list_pinned_returns_only_the_pinned_note_from_a_mixed_fixture() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let mut buf = vec![0u8; 16 * 1024];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    MIXED_FIXTURE.len(),
                    MIXED_FIXTURE
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        set_graph_base_for_test(&format!("http://{addr}"));

        let sidecars = list_pinned("tok").await.expect("a well-formed mixed page must decode");
        assert_eq!(sidecars.len(), 1, "only the pinned note may come back; got {sidecars:?}");
        assert_eq!(sidecars[0].note_uuid, "pinned@x");
        assert_eq!(sidecars[0].id, "pinned@x", "self-referential id — no second object exists on this backend");
        assert_eq!(sidecars[0].kind, SidecarKind::Pin);
    }

    #[test]
    fn a_row_with_no_uuid_at_all_is_dropped_not_defaulted() {
        let empty_id = r#"{"value":[{"id":"m1","internetMessageId":"","singleValueExtendedProperties":[]}]}"#;
        let (rows, _) = decode_pin_page(empty_id).expect("valid JSON, just no identity");
        assert!(rows.is_empty(), "a message with no internetMessageId has no uuid to key a SidecarRecord by");
    }
}

/// Create a folder under `parent_id`.
///
/// `PR_CONTAINER_CLASS` is sent on the `PR_MESSAGE_CLASS` precedent, but it
/// does not matter either way: this call silently drops it regardless
/// (confirmed 2026-08-15, M3 — a folder created here reads back
/// `PR_CONTAINER_CLASS: IPF.Note` whether or not this property was sent),
/// and folders created via this call never reach Notes.app either way — see
/// `Capabilities::for_backend`'s Microsoft arm (backend/mod.rs) and CLAUDE.md's
/// "Folder writes do not work" for the full, now-closed mechanism. Kept as a
/// best-effort send rather than removed: it costs nothing, and this API has
/// already been seen to accept a wrong thing quietly elsewhere (see
/// `create_note`).
///
/// `path` on the returned `RemoteFolder` is `name` as given — not run through
/// `folder_paths`'s collision suffixing, because that machinery operates on a
/// whole scan's worth of siblings at once (see `folder_paths`'s doc comment)
/// and this call sees only the one folder it just created. The next scan
/// reconciles it into the real path if a collision exists.
pub async fn create_child_folder(
    token: &str,
    parent_id: &str,
    name: &str,
) -> Result<crate::backend::RemoteFolder, TransportError> {
    let payload = serde_json::json!({
        "displayName": name,
        "singleValueExtendedProperties": [
            { "id": PROP_CONTAINER_CLASS, "value": CLASS_STICKY_CONTAINER }
        ],
    });
    let url = format!("{GRAPH_BASE}/me/mailFolders/{}/childFolders", urlencoding::encode(parent_id));
    let raw = graph_send(token, reqwest::Method::POST, &url, Some(payload)).await?;
    let id = decode_folder_id(&raw).map_err(|e| decode_err("create_child_folder", e))?;
    Ok(crate::backend::RemoteFolder { id, path: name.to_string() })
}

/// `PATCH` a folder's display name. Confirmed through `PR_PARENT_DISPLAY` on a
/// witness note (2026-08-14) — `GET /mailFolders/{id}` 404s on this backend
/// (gotcha #12), so the rename itself is unverifiable by reading the folder
/// object back; a note filed in it is the only way to observe the new name.
pub async fn rename_folder_req(token: &str, id: &str, new_name: &str) -> Result<(), TransportError> {
    let payload = serde_json::json!({ "displayName": new_name });
    let url = format!("{GRAPH_BASE}/me/mailFolders/{}", urlencoding::encode(id));
    graph_send(token, reqwest::Method::PATCH, &url, Some(payload)).await.map(|_| ())
}

/// `DELETE` a folder outright. Measured live 2026-08-14 — `scripts/
/// ms_write_probe.py`'s step `[3]` issues exactly this request against a
/// folder it created (`destroy("folders", …)`, which only counts it as
/// successful on a `200`/`204`), then re-`GET`s the folder's messages to
/// establish the 404-vs-200 verdict Task 9's existence check rests on. So the
/// request itself is confirmed, not speculative — but that follow-up probe
/// was about DISTINGUISHING an emptied folder from a deleted one, not about
/// recoverability: whether a deleted folder can be undone (the way
/// `Capabilities::has_trash` answers for messages, see `delete_message`) is a
/// separate, still-unmeasured question this call does not answer either way.
pub async fn delete_folder_req(token: &str, id: &str) -> Result<(), TransportError> {
    let url = format!("{GRAPH_BASE}/me/mailFolders/{}", urlencoding::encode(id));
    graph_send(token, reqwest::Method::DELETE, &url, None).await.map(|_| ())
}

#[cfg(test)]
mod write_path_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A single Graph message resource — the shape `POST`/`PATCH`/move return
    /// — good enough for `decode_single_message` to succeed on. The tests
    /// below assert on the REQUEST, not this response, but a well-formed
    /// response keeps `create_note`/`patch_note`/`move_note_to_folder`'s `?`
    /// from turning a passing request assertion into a confusing decode
    /// panic somewhere else in the same test.
    const SINGLE_MESSAGE_FIXTURE: &str = r#"{
        "id": "MSG-ID",
        "subject": "s",
        "internetMessageId": "<a@b>",
        "parentFolderId": "F",
        "createdDateTime": "2026-08-13T17:08:23Z",
        "lastModifiedDateTime": "2026-08-13T19:48:36Z",
        "body": { "contentType": "html", "content": "<html><body>s</body></html>" }
    }"#;

    /// Read one HTTP request off `sock` in full, regardless of how many TCP
    /// segments it arrives in.
    ///
    /// A single `read()` call (the shape `mod.rs`'s scoped-read test uses,
    /// and this file's own first draft copied) assumes headers and body land
    /// together in one packet. That is true almost always on loopback with
    /// payloads this small, but "almost always" is exactly the kind of
    /// assumption that turns into an intermittent CI failure: a split would
    /// hand the caller an empty body, and `serde_json::from_str(&body)
    /// .expect("json body")` downstream would panic with a message that
    /// looks like a decode bug, not a truncated read. So: read until the
    /// `\r\n\r\n` header/body separator has fully arrived, then read exactly
    /// `Content-Length` more bytes if the body wasn't already all there.
    async fn read_full_request(sock: &mut tokio::net::TcpStream) -> String {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];

        let header_end = loop {
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => return String::from_utf8_lossy(&buf).to_string(),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };

        let headers = String::from_utf8_lossy(&buf[..header_end]);
        let content_length: usize = headers
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                if name.trim().eq_ignore_ascii_case("content-length") {
                    value.trim().parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);

        while buf.len() < header_end + content_length {
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }

        String::from_utf8_lossy(&buf).to_string()
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// [`read_full_request`], split into its request line and body.
    async fn read_request_line_and_body(sock: &mut tokio::net::TcpStream) -> (String, String) {
        let req = read_full_request(sock).await;
        let line = req.lines().next().unwrap_or("").to_string();
        let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (line, body)
    }

    async fn respond_ok(sock: &mut tokio::net::TcpStream) {
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            SINGLE_MESSAGE_FIXTURE.len(),
            SINGLE_MESSAGE_FIXTURE
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
    }

    /// A local server recording each request's line and body.
    ///
    /// Runs for the life of this test's own thread — `TEST_GRAPH_BASE` is
    /// per-thread (see its doc comment) and `#[tokio::test]` here uses the
    /// default current-thread runtime, so this listener is only ever reached
    /// by requests this same test makes, no matter what else `cargo test`
    /// runs concurrently.
    async fn recording_server() -> (std::net::SocketAddr, Arc<Mutex<Vec<(String, String)>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let (line, body) = read_request_line_and_body(&mut sock).await;
                recorder.lock().unwrap().push((line, body));
                respond_ok(&mut sock).await;
            }
        });
        (addr, seen)
    }

    /// Like [`recording_server`], but records the FULL raw request (headers
    /// included) rather than just the line and body — what the `Prefer`
    /// header assertion needs.
    async fn recording_server_with_headers() -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let req = read_full_request(&mut sock).await;
                recorder.lock().unwrap().push(req);
                respond_ok(&mut sock).await;
            }
        });
        (addr, seen)
    }

    /// A local server that answers each successive request with the next
    /// body in `responses`, in order, falling back to
    /// [`SINGLE_MESSAGE_FIXTURE`] once the script runs out.
    ///
    /// What [`decode_or_refetch`]'s fallback needs to prove itself: the
    /// FIRST response can be missing `internetMessageId` (the unmeasured
    /// write-response shape) while the SECOND (the follow-up `GET`) carries
    /// it, and the test can assert on exactly which requests were sent and
    /// in what order.
    async fn scripted_server(
        responses: Vec<String>,
    ) -> (std::net::SocketAddr, Arc<Mutex<Vec<(String, String)>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let script: Arc<Mutex<std::vec::IntoIter<String>>> = Arc::new(Mutex::new(responses.into_iter()));

        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let (line, body) = read_request_line_and_body(&mut sock).await;
                recorder.lock().unwrap().push((line, body));
                let resp_body = script
                    .lock()
                    .unwrap()
                    .next()
                    .unwrap_or_else(|| SINGLE_MESSAGE_FIXTURE.to_string());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    resp_body.len(),
                    resp_body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, seen)
    }

    #[tokio::test]
    async fn a_created_note_always_carries_the_sticky_note_class() {
        // Measured 2026-08-14: omit PR_MESSAGE_CLASS and Graph returns 201
        // with an IPM.Note — an EMAIL, silently filed in the user's Notes
        // folder.
        let (addr, seen) = recording_server().await;
        set_graph_base_for_test(&format!("http://{addr}"));

        let _ = create_note("tok", "FOLDER-ID", "My Title", "<div>body</div>").await;

        let (line, body) = seen.lock().unwrap().first().cloned().expect("a request");
        assert!(line.starts_with("POST /me/mailFolders/FOLDER-ID/messages"), "got: {line}");
        let sent: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(sent["subject"], "My Title");
        assert_eq!(sent["body"]["contentType"], "HTML");
        assert_eq!(
            sent["body"]["content"],
            "<html><body>My Title<div>body</div></body></html>",
            "the title must be injected as the body's first line"
        );
        let props = sent["singleValueExtendedProperties"].as_array().expect("props");
        assert_eq!(props.len(), 1, "query ONE extended property at a time");
        assert_eq!(props[0]["id"], "String 0x001A");
        assert_eq!(props[0]["value"], "IPM.StickyNote");
    }

    #[tokio::test]
    async fn a_patch_updates_in_place_and_never_inserts() {
        let (addr, seen) = recording_server().await;
        set_graph_base_for_test(&format!("http://{addr}"));

        let _ = patch_note("tok", "MSG-ID", "New Title", "<div>b</div>").await;

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "one request — not insert-new + trash-old");
        assert!(calls[0].0.starts_with("PATCH /me/messages/MSG-ID"), "got: {}", calls[0].0);
        let sent: serde_json::Value = serde_json::from_str(&calls[0].1).unwrap();
        assert_eq!(sent["subject"], "New Title");
        assert!(sent["body"]["content"].as_str().unwrap().starts_with("<html><body>New Title"));
    }

    #[tokio::test]
    async fn a_move_addresses_the_move_endpoint_with_the_destination_folder() {
        let (addr, seen) = recording_server().await;
        set_graph_base_for_test(&format!("http://{addr}"));

        let _ = move_note_to_folder("tok", "MSG-ID", "FOLDER-2").await;

        let (line, body) = seen.lock().unwrap().first().cloned().expect("a request");
        assert!(line.starts_with("POST /me/messages/MSG-ID/move"), "got: {line}");
        let sent: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(sent["destinationId"], "FOLDER-2");
    }

    #[tokio::test]
    async fn a_delete_addresses_the_message_with_no_body() {
        let (addr, seen) = recording_server().await;
        set_graph_base_for_test(&format!("http://{addr}"));

        delete_message("tok", "MSG-ID").await.expect("delete succeeds on 200");

        let (line, _) = seen.lock().unwrap().first().cloned().expect("a request");
        assert!(line.starts_with("DELETE /me/messages/MSG-ID"), "got: {line}");
    }

    #[tokio::test]
    async fn every_write_sends_the_immutable_id_header() {
        // Measured: the same move WITHOUT this header changed the note's id,
        // which would make reconcile_one manufacture a conflict copy on
        // every folder move.
        let (addr, seen) = recording_server_with_headers().await;
        set_graph_base_for_test(&format!("http://{addr}"));

        let _ = create_note("tok", "F", "t", "").await;
        let _ = patch_note("tok", "M", "t", "").await;
        let _ = move_note_to_folder("tok", "M", "F2").await;
        let _ = delete_message("tok", "M").await;

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 4, "all four write requests must have been sent");
        for raw in calls.iter() {
            assert!(
                raw.to_lowercase().contains(r#"prefer: idtype="immutableid""#),
                "a write went out without the Prefer header:\n{raw}"
            );
        }
    }

    // All four write tests above use ids ("MSG-ID", "FOLDER-ID", "F", "M",
    // "F2") that contain nothing `urlencoding::encode` would touch — so
    // deleting the `encode` call would leave every one of them green. Real
    // Exchange immutable ids are base64 and genuinely contain `/`, `+`, `=`
    // (`mod.rs`'s scoped-read test uses `"AAMkA/Dg+X0="` for the same
    // reason); this test uses the same shape so a dropped `encode` call
    // actually fails something.
    #[tokio::test]
    async fn a_realistic_immutable_id_is_percent_encoded_not_left_raw() {
        let (addr, seen) = recording_server().await;
        set_graph_base_for_test(&format!("http://{addr}"));

        let _ = patch_note("tok", "AAMkA/Dg+X0=", "t", "").await;

        let (line, _) = seen.lock().unwrap().first().cloned().expect("a request");
        assert!(
            line.starts_with("PATCH /me/messages/AAMkA%2FDg%2BX0%3D"),
            "the id must be percent-encoded, not left raw — got: {line}"
        );
    }

    /// A write response missing `internetMessageId` — the unmeasured shape
    /// I1 exists for. No prior observation (M1's or Task 5's own tests) has
    /// ever looked at a write RESPONSE body; every prior sample was a READ.
    fn message_without_identity(id: &str) -> String {
        format!(
            r#"{{"id":"{id}","subject":"s","parentFolderId":"F",
                "createdDateTime":"2026-08-13T17:08:23Z","lastModifiedDateTime":"2026-08-13T19:48:36Z",
                "body":{{"contentType":"html","content":"<html><body>s</body></html>"}}}}"#
        )
    }

    fn message_with_identity(id: &str) -> String {
        format!(
            r#"{{"id":"{id}","subject":"s","internetMessageId":"<n@x>","parentFolderId":"F",
                "createdDateTime":"2026-08-13T17:08:23Z","lastModifiedDateTime":"2026-08-13T19:48:36Z",
                "body":{{"contentType":"html","content":"<html><body>s</body></html>"}}}}"#
        )
    }

    #[tokio::test]
    async fn a_write_response_missing_internet_message_id_recovers_via_one_refetch() {
        let (addr, seen) = scripted_server(vec![
            message_without_identity("NEW-ID"),
            message_with_identity("NEW-ID"),
        ])
        .await;
        set_graph_base_for_test(&format!("http://{addr}"));

        let note = create_note("tok", "F", "s", "")
            .await
            .expect("the fallback GET must recover a note Graph actually created");
        assert_eq!(note.internet_message_id, "n@x");

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "exactly one follow-up request — not a retry loop");
        assert!(calls[0].0.starts_with("POST /me/mailFolders/F/messages"), "got: {}", calls[0].0);
        assert!(calls[1].0.starts_with("GET /me/messages/NEW-ID"), "got: {}", calls[1].0);
    }

    #[tokio::test]
    async fn a_write_response_still_missing_the_id_after_one_refetch_is_a_permanent_error() {
        // Both the write response AND the refetch omit internetMessageId —
        // genuinely unidentifiable, so this must fail rather than loop.
        let (addr, seen) = scripted_server(vec![
            message_without_identity("NEW-ID"),
            message_without_identity("NEW-ID"),
        ])
        .await;
        set_graph_base_for_test(&format!("http://{addr}"));

        let err = create_note("tok", "F", "s", "")
            .await
            .expect_err("still unidentifiable after the one retry must not read as success");
        assert!(
            matches!(err, TransportError::Permanent { .. }),
            "got: {err:?}"
        );

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "the fallback must attempt exactly one refetch, then stop");
    }

    #[tokio::test]
    async fn creating_a_folder_targets_the_notes_root() {
        // scripted_server's script has one entry, so the one create request
        // gets this body; a second request (there should be none) would fall
        // back to SINGLE_MESSAGE_FIXTURE and fail the id/path assertions below.
        let (addr, seen) =
            scripted_server(vec![r#"{"id":"NEW-ID","displayName":"Ideas"}"#.to_string()]).await;
        set_graph_base_for_test(&format!("http://{addr}"));

        let f = create_child_folder("tok", "ROOT-ID", "Ideas").await.unwrap();
        assert_eq!(f.id, "NEW-ID");
        assert_eq!(f.path, "Ideas");

        let (line, body) = seen.lock().unwrap().first().cloned().unwrap();
        assert!(line.starts_with("POST /me/mailFolders/ROOT-ID/childFolders"), "got: {line}");
        let sent: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(sent["displayName"], "Ideas");
        let props = sent["singleValueExtendedProperties"].as_array().expect("props");
        assert_eq!(props[0]["id"], "String 0x3613");
        assert_eq!(props[0]["value"], "IPF.StickyNote");
    }

    #[tokio::test]
    async fn a_rename_patches_the_display_name() {
        let (addr, seen) = recording_server().await;
        set_graph_base_for_test(&format!("http://{addr}"));

        rename_folder_req("tok", "F-ID", "New Name").await.expect("204/200 is success");

        let (line, body) = seen.lock().unwrap().first().cloned().unwrap();
        assert!(line.starts_with("PATCH /me/mailFolders/F-ID"), "got: {line}");
        let sent: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(sent["displayName"], "New Name");
    }

    #[tokio::test]
    async fn a_folder_delete_addresses_the_folder_with_no_body() {
        let (addr, seen) = recording_server().await;
        set_graph_base_for_test(&format!("http://{addr}"));

        delete_folder_req("tok", "F-ID").await.expect("204/200 is success");

        let (line, body) = seen.lock().unwrap().first().cloned().unwrap();
        assert!(line.starts_with("DELETE /me/mailFolders/F-ID"), "got: {line}");
        assert!(body.is_empty(), "a DELETE must carry no request body, got: {body:?}");
    }

    /// A local server that answers by REQUEST PATH rather than by request
    /// order. `recording_server`/`scripted_server` both answer every request
    /// identically (or in a fixed sequence) regardless of which URL was
    /// actually hit — no good for a test that needs two DIFFERENT ids to get
    /// two DIFFERENT answers in the same run, which is exactly what proving
    /// "emptied survives, deleted is pruned" requires.
    async fn server_routing(
        routes: &[(&str, u16, &str)],
    ) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let routes: Vec<(String, u16, String)> =
            routes.iter().map(|(p, s, b)| (p.to_string(), *s, b.to_string())).collect();

        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let (line, _) = read_request_line_and_body(&mut sock).await;
                recorder.lock().unwrap().push(line.clone());
                let (status, body) = routes
                    .iter()
                    .find(|(path, _, _)| line.contains(path.as_str()))
                    .map(|(_, s, b)| (*s, b.as_str()))
                    .unwrap_or((404, "{}"));
                let status_text = match status {
                    200 => "OK",
                    404 => "Not Found",
                    _ => "Unknown",
                };
                let resp = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    status_text,
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, seen)
    }

    #[tokio::test]
    async fn an_emptied_folder_survives_and_a_deleted_one_is_pruned() {
        // Measured 2026-08-14: GET /mailFolders/{id}/messages returns 200 for a
        // live folder and 404 once the folder is deleted. That is the only way to
        // tell "emptied" from "deleted" on a backend where the folder list is
        // DERIVED from the notes inside it.
        let (addr, _) = server_routing(&[
            ("/me/mailFolders/LIVE-EMPTY/messages", 200, r#"{"value":[]}"#),
            ("/me/mailFolders/GONE/messages", 404, r#"{"error":{"code":"ErrorItemNotFound"}}"#),
        ])
        .await;
        set_graph_base_for_test(&format!("http://{addr}"));

        assert!(
            folder_still_exists("tok", "LIVE-EMPTY").await,
            "an empty folder that still exists must survive the prune"
        );
        assert!(
            !folder_still_exists("tok", "GONE").await,
            "a deleted folder must still be pruned"
        );
    }
}
