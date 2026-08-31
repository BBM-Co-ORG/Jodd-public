//! System prompt for content extraction. v1 hardcoded; v2 will allow
//! per-account customization via account settings.
//!
//! NAMING NOTE: the module was renamed `lessons` -> `llm` on 2026-07-27
//! (it now also hosts auto-link, not just Extract) — the earlier
//! churn-minimization decision to keep the historical name no longer
//! applies. The user-facing vocabulary is unchanged: "Extract",
//! "Extracts", and the `__Extracts__` folder marker still read that way
//! everywhere.

pub const SYSTEM_PROMPT: &str = r###"Your response MUST be a single JSON object and nothing else.

The very first character of your response MUST be `{`. Do not write any preamble. Do not write any explanation. Do not wrap your response in a markdown code fence (no ```json, no ```). Do not write any trailing prose. Just the JSON object.

You are an expert content extractor for a personal knowledge management tool. Given mixed unformatted text from any source — debugging session, meeting transcript, technical conversation, article, blog post, slack thread, video transcript, anything — distill it into structured key points that capture the essence and drop the noise.

The JSON object you produce must have this exact shape:

{
  "title": "<short topic summary, max 80 chars — describes what the source is about, NOT 'Extract from X'>",
  "lessons_markdown": "<full markdown body with ## H2 headings per key point>",
  "meta_lessons_markdown": "<optional ## Meta-points section for higher-order observations, or empty string>",
  "tags": ["lowercase-kebab", "no-spaces", "max-8-tags"],
  "confidence": "high|medium|low"
}

Rules for lessons_markdown (the main body):
- Each key point is a ## H2 heading. The heading is the topic itself (e.g. "## Migration versions are claimed by live DB state, not just current code"), NOT a label like "## Point 1 — topic" or "## Lesson 1 — topic". Just the topic.
- Under each H2, use bullets, code blocks, and markdown links to support the point with specifics from the source.
- Preserve file:line references (e.g. [foo.rs:42](src/foo.rs:42)) if present in the source.
- Be specific; cite source content where possible.
- 1-7 points typically; extract only genuinely distinct ones (don't pad). For long sources, prefer fewer well-developed points over many shallow ones.
- The shape of the points adapts to the source:
  - Debugging session → root causes + fixes + meta-lessons
  - Meeting transcript → decisions + action items + open questions
  - Article → key claims + supporting evidence + counter-points
  - Conversation → main threads + conclusions + unresolved threads
  - Tutorial → core concepts + steps + caveats
  Match the structure to what the source actually contains; don't force a "lesson" framing if the source is decisions or claims.

Rules for meta_lessons_markdown (optional):
- Use ## H2 for the section heading (e.g. "## Meta-points" or "## Higher-order observations").
- Reserve this for genuinely cross-cutting observations that don't fit under any one main point.
- Often empty — only include when meta-level signal exists.

Rules for tags:
- 2-8 tags, lowercase, kebab-case.
- Prefer specific tags (e.g. "macos-keychain") over generic ones ("computers").
- Include domain tags ("debugging", "rust", "oauth", "negotiation") relevant to the source's subject matter.

Rules for title:
- Should reflect what the source is ABOUT, not "Extract from X" or "Lessons from X". Just the subject.
- Examples: "SQLite migration numbering fragility", "OAuth PKCE flow design", "Q3 planning decisions".

Remember: your entire response is the JSON object. Nothing before it. Nothing after it. The first character is `{`. The last character is `}`."###;

/// The payload "Test connection" sends, kept HERE rather than in lib.rs
/// because the failure it guards against is an interaction between this text
/// and `SYSTEM_PROMPT` above — separating them lets the pair drift, which is
/// exactly how v0.25.1 shipped a broken default preset.
///
/// It has to be long enough and varied enough that a correct answer needs
/// several `## H2` sections. A one-line prompt is answered in one turn and
/// never reaches the paths that actually break.
pub const CONNECTION_TEST_SAMPLE: &str = r###"Notes from Tuesday's incident review.

The deploy went out at 14:10 and the error rate climbed from about 0.2% to
just under 9% within four minutes. Nobody noticed until a customer emailed,
because the alert threshold was set on absolute error count and traffic was
low that afternoon. We rolled back at 14:52.

Root cause was a config change that pointed the image resizer at the new
bucket before the bucket's permissions were applied. The change was correct
on its own; the ordering was not. Terraform applied both in the same plan and
the ordering between them was never pinned, so it worked on staging by luck
and failed in production.

Marta pointed out that we have hit ordering problems in this repo twice
before, both times with storage permissions, and both times the fix was to
add an explicit depends_on rather than to change the resources themselves.

Open question nobody could answer: why did the staging run pass? We think
staging's bucket already had the permission from an earlier manual fix, so
the ordering never mattered there. That would mean staging has drifted from
production and is no longer a real rehearsal.

Action items: pin the ordering, move the alert to a rate rather than a count,
and audit staging for manual changes that production never received."###;

/// System prompt for LlmProvider::suggest_links — given new text and a
/// list of candidate existing notes, judge relatedness and decide whether
/// each candidate warrants a one-line addition. See design spec Approach §1
/// Step 2 (docs/superpowers/specs/2026-07-20-auto-link-ingest-design.md).
pub const LINK_SUGGESTION_SYSTEM_PROMPT: &str = r#"You are helping connect a piece of text to a personal wiki of existing notes. You will be given the new text and a list of candidate existing notes (each with a uuid, title, and a short snippet of its content). Decide which candidates are genuinely related to the new text.

For each candidate in the input, respond with an object containing:
- "uuid": the candidate's uuid, copied exactly from the input
- "related": true if the new text and this candidate are about the same topic, entity, or concept; false if unrelated or only superficially similar
- "should_append": true only if the connection is significant enough that a reader of the EXISTING note would want to know about the new text. Most related notes should just get a link (should_append: false) — only propose an append when it adds real value.
- "addition_text": if should_append is true, ONE short sentence (not a paragraph) to append to the existing note, written as it would read once appended. Use the literal placeholder [[new-note-slug]] wherever you want to reference the new text's note — the caller will substitute the real link. Example: "Also discussed in relation to [[new-note-slug]]." Omit this field (or use null) when should_append is false.

Be conservative: when in doubt, mark related: false rather than creating a weak connection.

Respond with ONLY a JSON object matching exactly this shape, one entry per candidate provided, no additional commentary, no markdown code fences:
{ "suggestions": [ { "uuid": "...", "related": true, "should_append": false, "addition_text": null }, ... ] }"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The connection probe must be REPRESENTATIVE, not a greeting — and not
    /// merely bulky. Byte count and line count are both satisfiable by a
    /// degenerate sample (six near-duplicate padded one-liners would pass
    /// while being trivially answerable in a single `## H2`), so what this
    /// test pins is topical diversity: several genuinely distinct threads a
    /// correct answer has to address separately.
    ///
    /// v0.25.1 shipped a broken claude preset past every review because the
    /// probe was "Jodd connection test. Reply with one short lesson." — a
    /// model answers that in one turn and never reaches the structured-output
    /// path, the retry budget, or a multi-section envelope, which is where the
    /// real failures lived. Anyone tempted to shrink this to make the test
    /// faster is reopening that hole; the cost of the probe IS the feature.
    #[test]
    fn the_connection_test_sample_is_a_real_workload() {
        let s = CONNECTION_TEST_SAMPLE;
        assert!(
            s.len() >= 600,
            "probe sample must be substantial, got {} bytes",
            s.len()
        );
        let paragraphs = s
            .split("\n\n")
            .filter(|p| !p.trim().is_empty())
            .count();
        assert!(
            paragraphs >= 4,
            "probe sample must carry several distinct threads, found {paragraphs} paragraphs"
        );
    }
}
