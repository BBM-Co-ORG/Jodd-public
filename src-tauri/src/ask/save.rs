//! "Save as note" for an Ask Jodd answer: the question and the answer as a
//! new note, filed where Extract files (`filing::resolve_destination`) and
//! recorded as AI-written — the owner cannot edit the answer before saving,
//! so it is never laundered into a human-written note.
//!
//! Citations are `[[slug]]` in the same `db::note_slug` form a note links
//! with, so they stay working links in the saved note. A citation to a note
//! in another account (an all-accounts answer) could never resolve there, so
//! it becomes the cited note's title in quotes instead.

use crate::ask::CitedNote;
use crate::backend::TagModel;

/// Longest question kept in the title; the whole question is in the body.
/// 40 keeps the title readable in the note list (the owner found 80 too
/// long after the live pass, 2026-11-12).
const MAX_TITLE_QUESTION_CHARS: usize = 40;

/// What every saved answer's title starts with.
pub const TITLE_PREFIX: &str = "Ask Jodd — ";

/// `Ask Jodd — <first line of the question>`, cut on a character boundary.
pub fn answer_title(question: &str) -> String {
    let line = question.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if line.is_empty() {
        return "Ask Jodd".into();
    }
    let mut q: String = line.chars().take(MAX_TITLE_QUESTION_CHARS).collect();
    if line.chars().count() > MAX_TITLE_QUESTION_CHARS {
        q = q.trim_end().to_string() + "…";
    }
    format!("{TITLE_PREFIX}{q}")
}

/// The shorter title for a note saved before the 40-character cut (it was
/// 80 until 2026-11-12), or `None` to leave it alone.
///
/// Renames only a title Jodd wrote and the owner never changed: the question
/// part, without its `…`, must still be the start of the `Question:` line in
/// the body. A title the owner edited fails that test and is kept, as is one
/// already short. Whitespace is compared collapsed, since the body is read
/// through `strip_html_to_text`.
pub fn shortened_title(title: &str, body_html: &str) -> Option<String> {
    let part = title.strip_prefix(TITLE_PREFIX)?;
    let part = part.strip_suffix('…').unwrap_or(part);
    if part.chars().count() <= MAX_TITLE_QUESTION_CHARS {
        return None;
    }
    let collapsed = part.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = crate::db::strip_html_to_text(body_html);
    if !text.contains(&format!("Question: {collapsed}")) {
        return None;
    }
    let short = answer_title(part);
    (short != title).then_some(short)
}

/// Startup repair: shorten the titles of answers saved before the cut.
/// Only notes created by Save as note (their recorded creator is the
/// `ask-jodd` workflow) and only through `shortened_title`'s proof that the
/// title is still Jodd's. A local edit like any other, so the sync worker
/// pushes it; idempotent, since a shortened title no longer qualifies.
/// Returns how many notes it renamed.
pub fn shorten_saved_titles(db: &crate::db::Db) -> Result<usize, String> {
    let candidates = db.notes_with_title_prefix(TITLE_PREFIX).map_err(|e| e.to_string())?;
    let mut renamed = 0;
    for n in candidates {
        let Some(short) = shortened_title(&n.title, &n.body_html) else { continue };
        let creators = db.creators_for(&n.account_id, std::slice::from_ref(&n.uuid)).map_err(|e| e.to_string())?;
        if !creators.get(&n.uuid).is_some_and(|a| a.starts_with("jodd-ask-jodd/")) {
            continue;
        }
        // Versioned: a concurrent edit wins and this note waits for the next launch.
        let applied = db
            .apply_local_edit_versioned(
                &n.uuid, &n.account_id, &short, &n.body_html, &n.label, n.local_version,
                &crate::provenance::Actor::Process("ask-title-shorten"),
            )
            .map_err(|e| e.to_string())?;
        if applied {
            renamed += 1;
        }
    }
    Ok(renamed)
}

/// The note body: a dated header and the question, the answer, then a
/// Sources list of the cited notes in `account_id`.
pub fn answer_body(account_id: &str, question: &str, markdown: &str, cited: &[CitedNote], date: &str, model: TagModel) -> String {
    let mut answer = markdown.to_string();
    let mut sources: Vec<String> = Vec::new();
    for c in cited {
        if c.account_id == account_id {
            if !sources.contains(&c.slug) {
                sources.push(c.slug.clone());
            }
        } else {
            let title = if c.title.trim().is_empty() { "Untitled" } else { c.title.trim() };
            answer = answer.replace(&format!("[[{}]]", c.slug), &format!("\u{201c}{title}\u{201d}"));
        }
    }
    let header = vec![
        format!("Asked Jodd · {date}"),
        format!("Question: {}", question.trim()),
    ];
    let sources_md = if sources.is_empty() { String::new() } else { crate::studio::cite::sources_markdown(&sources) };
    crate::llm::markdown::assemble_studio_body(&header, &answer, &sources_md, &[], model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cited(account_id: &str, slug: &str, title: &str) -> CitedNote {
        CitedNote { uuid: "u".into(), account_id: account_id.into(), title: title.into(), slug: slug.into() }
    }

    #[test]
    fn title_is_the_first_line_of_the_question_cut_to_40() {
        assert_eq!(answer_title("  \nWhat did we decide?\nmore"), "Ask Jodd — What did we decide?");
        assert_eq!(answer_title("   "), "Ask Jodd");
        let long = "ก".repeat(100);
        let t = answer_title(&long);
        assert_eq!(t, format!("Ask Jodd — {}…", "ก".repeat(40)));
        // Exactly 40 is kept whole, with no ellipsis.
        let forty = "a".repeat(40);
        assert_eq!(answer_title(&forty), format!("Ask Jodd — {forty}"));
    }

    fn saved(db: &crate::db::Db, uuid: &str, title: &str, question: &str, actor: &crate::provenance::Actor) {
        let mut n = crate::test_support::note("a", uuid).title(title).build();
        n.body_html = answer_body("a", question, "The answer.", &[], "2026-11-10", TagModel::BodyText);
        db.insert_local_new(&n, actor).unwrap();
    }

    fn ask_actor() -> crate::provenance::Actor {
        crate::provenance::Actor::Workflow { name: "ask-jodd".into(), model: "m".into() }
    }

    /// The title the 80-character rule produced (before 2026-11-12).
    fn old_title(question: &str) -> String {
        let q: String = question.chars().take(80).collect();
        if question.chars().count() > 80 { format!("{TITLE_PREFIX}{}…", q.trim_end()) } else { format!("{TITLE_PREFIX}{q}") }
    }

    #[test]
    fn an_old_long_title_is_shortened_like_a_new_save() {
        let q = "ก".repeat(100);
        let body = answer_body("a", &q, "x", &[], "d", TagModel::BodyText);
        assert_eq!(shortened_title(&old_title(&q), &body), Some(answer_title(&q)));
        let q = "What did we decide about the sync conflict policy for shared notes";
        let body = answer_body("a", q, "x", &[], "d", TagModel::BodyText);
        assert_eq!(shortened_title(&old_title(q), &body), Some(answer_title(q)));
    }

    #[test]
    fn a_short_or_owner_edited_title_is_left_alone() {
        let q = "What did we decide about the sync conflict policy for shared notes";
        let body = answer_body("a", q, "x", &[], "d", TagModel::BodyText);
        assert_eq!(shortened_title(&answer_title(q), &body), None, "already short");
        let edited = format!("{TITLE_PREFIX}My own long title for this answer about conflicts and more");
        assert_eq!(shortened_title(&edited, &body), None, "the owner's title, not Jodd's");
        assert_eq!(shortened_title("Meeting notes", &body), None);
    }

    #[test]
    fn startup_repair_renames_only_save_as_note_notes_and_only_once() {
        let db = crate::test_support::temp_db();
        let q = "What did we decide about the sync conflict policy for shared notes";
        saved(&db, "mine", &old_title(q), q, &ask_actor());
        // Same shape, but the owner made it: never renamed.
        saved(&db, "human", &old_title(q), q, &crate::provenance::Actor::Human);
        assert_eq!(shorten_saved_titles(&db).unwrap(), 1);
        let mine = db.get("mine", "a").unwrap().unwrap();
        assert_eq!(mine.title, answer_title(q));
        assert_eq!(mine.sync_state, crate::db::SyncState::Dirty, "the worker must push the new title");
        assert!(mine.body_html.contains("The answer."), "the body is untouched");
        assert_eq!(db.get("human", "a").unwrap().unwrap().title, old_title(q));
        assert_eq!(shorten_saved_titles(&db).unwrap(), 0, "idempotent");
    }

    #[test]
    fn body_keeps_same_account_citations_as_links_and_lists_them() {
        let body = answer_body(
            "a",
            "Why keep-both?",
            "We chose keep-both. [[sync-aabbccdd]] Also [[other-11223344]].",
            &[cited("a", "sync-aabbccdd", "Sync"), cited("b", "other-11223344", "Other")],
            "2026-11-10",
            TagModel::BodyText,
        );
        assert!(body.starts_with("<p>Asked Jodd · 2026-11-10</p>\n<p>Question: Why keep-both?</p>\n"), "{body}");
        assert!(body.contains("[[sync-aabbccdd]]"), "{body}");
        assert!(!body.contains("[[other-11223344]]"), "a link to another account would never resolve: {body}");
        assert!(body.contains("\u{201c}Other\u{201d}"), "{body}");
        assert!(body.contains("Sources"), "{body}");
    }

    #[test]
    fn body_without_citations_has_no_sources_section() {
        let body = answer_body("a", "q", "Just an answer.", &[], "2026-11-10", TagModel::BodyText);
        assert!(!body.contains("Sources"), "{body}");
        assert!(body.contains("Just an answer."), "{body}");
    }

    #[test]
    fn answer_markup_is_sanitized_and_the_question_escaped() {
        let body = answer_body("a", "<b>q</b>", "hi <img src=\"https://x.test/beacon\"> <script>x()</script>", &[], "d", TagModel::BodyText);
        assert!(!body.contains("<img"), "{body}");
        assert!(!body.contains("<script"), "{body}");
        assert!(body.contains("Question: &lt;b&gt;q&lt;/b&gt;"), "{body}");
    }
}
