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
const MAX_TITLE_QUESTION_CHARS: usize = 80;

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
    format!("Ask Jodd — {q}")
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
    fn title_is_the_first_line_of_the_question_cut_to_80() {
        assert_eq!(answer_title("  \nWhat did we decide?\nmore"), "Ask Jodd — What did we decide?");
        assert_eq!(answer_title("   "), "Ask Jodd");
        let long = "ก".repeat(100);
        let t = answer_title(&long);
        assert_eq!(t, format!("Ask Jodd — {}…", "ก".repeat(80)));
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
