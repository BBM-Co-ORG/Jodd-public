//! System prompts for Ask Jodd's two calls.
//!
//! Deliberately NOT in llm/prompt.rs: that file holds the Extract workflow's
//! JSON-envelope contract, and these two return free text.

use std::sync::LazyLock;

use crate::ask::MAX_SELECTED_NOTES;

/// Stage 3 — pick notes from the catalog. The reply is parsed leniently
/// (catalog::parse_selected_uuid8s scans for 8-hex-char runs), so the format
/// instruction is a nudge, not a contract.
///
/// `LazyLock<String>` rather than a `&str` literal so the "at most N" figure
/// is built from `MAX_SELECTED_NOTES` instead of a hand-copied literal that
/// can drift from the actual cap applied in `context::build_answer_context`.
/// Derefs to `&str` at every call site via deref coercion.
pub static SELECT_SYSTEM_PROMPT: LazyLock<String> = LazyLock::new(|| format!("\
You are helping search a personal note collection.

Below is a catalog of notes. Each line is:
  <id> · <title> · <folder> · <#tags> · <date>

Read the conversation and reply with ONLY the ids of the notes worth opening
to answer it — most relevant first, at most {MAX_SELECTED_NOTES}. Reply with the bare ids
separated by newlines, nothing else.

Choose a note when its title, folder, or tags suggest it may contain the
answer, even if the wording differs from the question — the user may describe
something in different words than they used when writing it. If nothing in the
catalog is plausibly relevant, reply with the single word NONE."));

/// Stage 3 for a note scope with related notes on (spec 2026-10-23 §5.3).
/// The focus note is always read, so it is not in the catalog and the model
/// picks one fewer than `MAX_SELECTED_NOTES`.
pub static NOTE_SELECT_SYSTEM_PROMPT: LazyLock<String> = LazyLock::new(|| format!("\
You are helping search a personal note collection.

The user is asking about one particular note, named in the conversation. It
will be read in full regardless. Below is a catalog of notes related to it —
linked to or from it, or sharing a tag with it. Each line is:
  <id> · <title> · <folder> · <#tags> · <date>

Reply with ONLY the ids of the related notes that would help answer the
question about that note — most relevant first, at most {}. Reply with the
bare ids separated by newlines, nothing else. If none of them would help,
reply with the single word NONE.", MAX_SELECTED_NOTES - 1));

/// Stage 4 — answer from the fetched bodies.
///
/// The formatting rule names exactly the Markdown subset the frontend renders
/// (`renderAnswer` in `src/lib/components/askCitations.ts`); anything else —
/// tables, links, images, HTML — reaches the user as literal text. Widen the
/// two together or not at all: "Markdown is fine" here against a renderer
/// that escaped everything is how answers showed raw `**` and `1.` markers.
pub const ANSWER_SYSTEM_PROMPT: &str = "\
You are answering questions about the user's own notes. The notes below are
the ONLY source you may use.

Rules:
- Answer only from the notes provided. If they do not contain the answer, say
  so plainly. Never fill a gap with general knowledge.
- Cite the source of every claim inline, using the exact [[slug]] shown in that
  note's header — copy it verbatim, never invent or alter one.
- A note marked [truncated] was cut for length; say so if the answer depends on
  the part you cannot see.
- Answer in the language the user asked in.
- Be concise. Do not repeat the question back.
- Format with simple Markdown only: paragraphs, **bold**, *italic*, `code`, and
  numbered or bulleted lists. Do not use tables, Markdown links, images or
  HTML; they are shown as raw text. The [[slug]] citations are not links —
  keep writing them exactly as above.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_prompt_asks_only_for_the_markdown_the_frontend_renders() {
        // The renderer (askCitations.ts renderAnswer) shows tables, links and
        // HTML as literal text, so the prompt must steer away from them — and
        // must not read as "no [[...]] either", which would cost citations.
        assert!(!ANSWER_SYSTEM_PROMPT.contains("Markdown is fine"));
        for named in ["**bold**", "*italic*", "`code`", "lists", "tables", "links", "HTML"] {
            assert!(ANSWER_SYSTEM_PROMPT.contains(named), "prompt should name {named}");
        }
        assert!(ANSWER_SYSTEM_PROMPT.contains("[[slug]] citations are not links"));
    }

    #[test]
    fn select_prompt_states_the_actual_selection_cap() {
        // Pins that the prompt text is derived from MAX_SELECTED_NOTES, not
        // a hand-copied literal that can drift from the real cap enforced in
        // context::build_answer_context.
        assert!(
            SELECT_SYSTEM_PROMPT.contains(&format!("at most {MAX_SELECTED_NOTES}")),
            "prompt must state the real cap ({MAX_SELECTED_NOTES}), got: {}",
            &*SELECT_SYSTEM_PROMPT
        );
    }

    #[test]
    fn note_select_prompt_states_its_cap() {
        // The focus note is always read, so the model picks one fewer.
        assert!(NOTE_SELECT_SYSTEM_PROMPT.contains("at most 11"), "{}", &*NOTE_SELECT_SYSTEM_PROMPT);
        assert!(!NOTE_SELECT_SYSTEM_PROMPT.contains("at most 12"));
    }
}
