use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
}

/// Render a system prompt + conversation as one plain-text transcript, for
/// providers that take a single prompt string rather than a message array
/// (every agent CLI). Kept as a free function so it is unit-testable without
/// spawning a subprocess.
pub fn flatten_turns(system: &str, turns: &[ChatTurn]) -> String {
    let mut out = String::with_capacity(system.len() + 256);
    out.push_str(system);
    out.push_str("\n\n");
    for t in turns {
        let label = match t.role {
            ChatRole::User => "User:",
            ChatRole::Assistant => "Assistant:",
        };
        out.push_str(label);
        out.push('\n');
        out.push_str(&t.content);
        out.push_str("\n\n");
    }
    out.push_str("Assistant:");
    out
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("provider not configured: {0}")]
    NotConfigured(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("malformed envelope: {reason}")]
    MalformedEnvelope { reason: String, raw: String },
    #[error("upstream error: {0}")]
    UpstreamError(String),
    /// Constructed when the caller cancelled the in-flight extract via a
    /// CancellationToken — for example, the user clicked Cancel on the
    /// extraction modal. The caller should distinguish this from a real
    /// error and NOT create the source-preservation fallback note: the user
    /// actively chose to abort, not "lose" their paste.
    #[error("cancelled")]
    Cancelled,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ExtractEnvelope {
    pub title: Option<String>,
    pub lessons_markdown: String,
    #[serde(default)]
    pub meta_lessons_markdown: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub confidence: Option<String>,
}

impl ExtractEnvelope {
    /// Hand-written rather than derived: adding `schemars` for two constants
    /// is the trade `strip_ansi` already declined in agent_cli.rs.
    ///
    /// **Every property is listed in `required`, and every object carries
    /// `"additionalProperties": false`.** That is not a style choice and it
    /// is not the rule this file used to state ("required only when the
    /// field is neither `Option<_>` nor `#[serde(default)]`"). That earlier
    /// rule was measured wrong on 2026-08-29: `codex exec --output-schema`
    /// enforces OpenAI strict structured-output rules and answers
    /// **HTTP 400 `invalid_json_schema`** — *"'additionalProperties' is
    /// required to be supplied and to be false"* — refusing the run outright
    /// and writing an EMPTY last-message file. Every Extract on a codex
    /// account failed. Verified live, both directions: the permissive shape
    /// exits 1 with no output, this shape exits 0 and returns a valid
    /// envelope.
    ///
    /// Optionality is therefore expressed in the TYPE (`["string", "null"]`),
    /// never by omission from `required` — which round-trips into
    /// `Option<_>` unchanged, since serde reads an explicit `null` as `None`.
    ///
    /// The tests in `mod tests` are what keep this
    /// schema honest: they derive the struct's actual property-KEY SET via
    /// `serde_json::to_value` on a fully-populated instance and assert it
    /// equals this schema's `properties` keys, which catches a typo'd
    /// property name, a stale extra property, or a missing struct field in
    /// one assertion — they do not validate the full schema (types, nesting).
    pub const JSON_SCHEMA: &'static str = r#"{
  "type": "object",
  "properties": {
    "title": { "type": ["string", "null"] },
    "lessons_markdown": { "type": "string" },
    "meta_lessons_markdown": { "type": ["string", "null"] },
    "tags": { "type": "array", "items": { "type": "string" } },
    "confidence": { "type": ["string", "null"] }
  },
  "required": ["title", "lessons_markdown", "meta_lessons_markdown", "tags", "confidence"],
  "additionalProperties": false
}"#;
}

impl ExtractEnvelope {
    /// Whether this envelope carries an answer worth showing.
    ///
    /// The success criterion for a connection test is NOT "the process exited
    /// 0" and not "serde accepted the JSON" — codex was measured returning a
    /// successful exit with an empty output file, and an envelope whose body
    /// is blank tells the user nothing about whether their setup works.
    pub fn usable(&self) -> Result<(), String> {
        if self.lessons_markdown.trim().is_empty() {
            return Err("the provider returned an empty result".into());
        }
        Ok(())
    }
}

/// A candidate related note, summarized for the LLM's relatedness judgment
/// (design spec Step 2) — title + a short snippet, not the full body, to
/// keep the prompt compact when many candidates are involved.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CandidateSummary {
    pub uuid: String,
    pub title: String,
    pub snippet: String,
}

/// One candidate's relatedness judgment — see LINK_SUGGESTION_SYSTEM_PROMPT
/// for the exact contract the LLM must follow.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct LinkSuggestion {
    pub uuid: String,
    pub related: bool,
    #[serde(default)]
    pub should_append: bool,
    #[serde(default)]
    pub addition_text: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct LinkSuggestionsEnvelope {
    #[serde(default)]
    pub suggestions: Vec<LinkSuggestion>,
}

impl LinkSuggestionsEnvelope {
    /// Same strict shape as [`ExtractEnvelope::JSON_SCHEMA`], for the same
    /// measured reason: every property required, every object carrying
    /// `"additionalProperties": false`, optionality carried by the type.
    /// `codex exec --output-schema` rejects anything looser with HTTP 400
    /// before the model is ever called. Nested objects are not exempt —
    /// the rule applies to `suggestions.items` too, not just the envelope.
    pub const JSON_SCHEMA: &'static str = r#"{
  "type": "object",
  "properties": {
    "suggestions": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "uuid": { "type": "string" },
          "related": { "type": "boolean" },
          "should_append": { "type": "boolean" },
          "addition_text": { "type": ["string", "null"] }
        },
        "required": ["uuid", "related", "should_append", "addition_text"],
        "additionalProperties": false
      }
    }
  },
  "required": ["suggestions"],
  "additionalProperties": false
}"#;
}

#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// Run the extraction. The CancellationToken signals user-initiated
    /// abort — implementations should race their I/O against `cancel.cancelled()`
    /// and return `ExtractError::Cancelled` when the token fires. For the
    /// HTTP provider this means dropping the in-flight reqwest future; for
    /// the subprocess provider it means killing the child process.
    async fn extract(
        &self,
        source: &str,
        cancel: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError>;

    /// Judge which of `candidates` are related to `source` and whether each
    /// warrants a one-line addition to that existing note. Same cancellation
    /// contract as `extract`. NOTE: this trait method and the free function
    /// `crate::llm::autolink::suggest_links` share a name but are two
    /// distinct things at two different layers — this is the raw LLM call;
    /// the free function in autolink.rs orchestrates keyword extraction +
    /// candidate search + this call + placeholder substitution. Same
    /// relationship as `extract` (this trait) vs `extract_note` (the
    /// Tauri command that orchestrates around it).
    async fn suggest_links(
        &self,
        source: &str,
        candidates: &[CandidateSummary],
        cancel: CancellationToken,
    ) -> Result<LinkSuggestionsEnvelope, ExtractError>;

    /// Multi-turn free-text completion. Returns the model's raw text — there
    /// is no JSON envelope, because an answer is prose, not a schema. This
    /// also means the single-retry-on-malformed-envelope behavior in the
    /// agent-CLI provider does not apply here: there is no envelope to
    /// malform. Same cancellation contract as `extract`.
    async fn chat(
        &self,
        system: &str,
        turns: &[ChatTurn],
        cancel: CancellationToken,
    ) -> Result<String, ExtractError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_parses_full_response() {
        let json = "{ \"title\": \"Test lesson\", \"lessons_markdown\": \"## Lesson 1\\nbody\", \"meta_lessons_markdown\": \"## Meta\\nbody\", \"tags\": [\"tag-a\", \"tag-b\"], \"confidence\": \"high\" }";
        let env: ExtractEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.title.as_deref(), Some("Test lesson"));
        assert_eq!(env.tags.len(), 2);
    }

    #[test]
    fn envelope_parses_minimal_response() {
        // Optional fields all missing — only lessons_markdown required.
        let json = "{ \"lessons_markdown\": \"## L1\\nbody\" }";
        let env: ExtractEnvelope = serde_json::from_str(json).unwrap();
        assert!(env.title.is_none());
        assert!(env.tags.is_empty());
        assert!(env.meta_lessons_markdown.is_none());
    }

    #[test]
    fn flatten_turns_labels_roles_and_keeps_order() {
        let turns = vec![
            ChatTurn { role: ChatRole::User, content: "first question".into() },
            ChatTurn { role: ChatRole::Assistant, content: "an answer".into() },
            ChatTurn { role: ChatRole::User, content: "follow-up".into() },
        ];
        let out = flatten_turns("SYSTEM RULES", &turns);
        let first = out.find("first question").unwrap();
        let second = out.find("an answer").unwrap();
        let third = out.find("follow-up").unwrap();
        assert!(first < second && second < third, "turn order must be preserved");
        assert!(out.starts_with("SYSTEM RULES"), "system prompt leads the transcript");
        assert!(out.contains("User:") && out.contains("Assistant:"));
    }

    #[test]
    fn flatten_turns_handles_a_single_turn() {
        let turns = vec![ChatTurn { role: ChatRole::User, content: "only".into() }];
        let out = flatten_turns("S", &turns);
        assert!(out.contains("only"));
        // The trailing "Assistant:" cues the model to respond. A single user turn
        // produces no assistant TURN in the conversation, but the cue still appears.
        assert!(out.trim_end().ends_with("Assistant:"));
        assert_eq!(out.matches("Assistant:").count(), 1);
    }

    /// Collects a JSON object's top-level keys, so a struct's real field set
    /// (via `serde_json::to_value` on a fully-populated instance — `Option`
    /// fields serialize to `null` rather than vanishing, so every field
    /// appears as a key either way) can be compared against a schema's
    /// `properties` keys. Set equality, not a one-way subset check: it
    /// catches a schema property missing from the struct AND a struct field
    /// missing from the schema in the same assertion.
    fn object_keys(v: &serde_json::Value) -> std::collections::BTreeSet<String> {
        v.as_object().expect("expected a JSON object").keys().cloned().collect()
    }

    /// Asserts the two rules OpenAI strict structured output enforces, which
    /// `codex exec --output-schema` applies to every object it is handed.
    ///
    /// This is a REGRESSION PIN, not a style preference. Measured live on
    /// 2026-08-29 against codex-cli 0.147.0: a schema missing
    /// `"additionalProperties": false` is refused before the model runs with
    /// HTTP 400 `invalid_json_schema` — *"'additionalProperties' is required
    /// to be supplied and to be false"* — and codex then exits 1 having
    /// written an EMPTY last-message file, so every Extract on a codex
    /// account fails with no output to salvage. The same run with both rules
    /// applied exits 0 and returns a valid envelope.
    ///
    /// Because strict mode requires EVERY property in `required`,
    /// optionality has to be carried by the type (`["string", "null"]`)
    /// rather than by omission. That still round-trips into `Option<_>`:
    /// serde reads an explicit `null` as `None`.
    ///
    /// Applies to nested objects too — `suggestions.items` is checked
    /// separately for exactly that reason.
    fn assert_strict_structured_output(schema: &serde_json::Value, what: &str) {
        assert_eq!(
            schema.get("additionalProperties"),
            Some(&serde_json::Value::Bool(false)),
            "{what}: strict structured output requires \"additionalProperties\": false; \
             codex rejects the schema with HTTP 400 without it"
        );
        let properties = object_keys(&schema["properties"]);
        let required: std::collections::BTreeSet<String> = schema["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{what}: strict structured output requires a `required` list"))
            .iter()
            .map(|v| v.as_str().expect("required entries are strings").to_string())
            .collect();
        assert_eq!(
            properties, required,
            "{what}: strict structured output requires EVERY property in `required` \
             (express optionality in the type, e.g. [\"string\", \"null\"])"
        );
    }

    /// The schema is hand-written, so nothing but this test keeps it
    /// agreeing with the struct. A hardcoded literal compared against the
    /// schema's own hardcoded literal (the previous version of this test)
    /// would pass undetected on a typo'd property name, an extra stale
    /// property, or a struct field missing from the schema entirely — so the
    /// property-key SET is derived from a real, fully-populated
    /// `ExtractEnvelope` instead of restated. The sample-parses assertion is
    /// kept alongside it: it exercises deserialization, which the key-set
    /// check does not.
    #[test]
    fn extract_schema_agrees_with_the_envelope() {
        let schema: serde_json::Value =
            serde_json::from_str(ExtractEnvelope::JSON_SCHEMA).expect("schema is valid JSON");

        let env = ExtractEnvelope {
            title: Some("t".into()),
            lessons_markdown: "body".into(),
            meta_lessons_markdown: Some("meta".into()),
            tags: vec!["a".into()],
            confidence: Some("high".into()),
        };
        let struct_keys = object_keys(&serde_json::to_value(&env).expect("envelope serializes"));
        let schema_keys = object_keys(&schema["properties"]);
        assert_eq!(
            struct_keys, schema_keys,
            "schema properties must match ExtractEnvelope's fields exactly"
        );

        assert_strict_structured_output(&schema, "ExtractEnvelope");

        let sample = r#"{"title":"t","lessons_markdown":"body","tags":["a"]}"#;
        let env: ExtractEnvelope = serde_json::from_str(sample).expect("sample parses");
        assert_eq!(env.lessons_markdown, "body");
    }

    /// Same discipline as `extract_schema_agrees_with_the_envelope`, applied
    /// to both the envelope AND the inner `LinkSuggestion` object nested
    /// under `suggestions.items` — a typo inside the nested object would
    /// otherwise slip past a check that only looked at the outer envelope.
    #[test]
    fn link_suggestions_schema_agrees_with_the_envelope() {
        let schema: serde_json::Value = serde_json::from_str(LinkSuggestionsEnvelope::JSON_SCHEMA)
            .expect("schema is valid JSON");
        assert_eq!(schema["properties"]["suggestions"]["type"], "array");

        let suggestion = LinkSuggestion {
            uuid: "u".into(),
            related: true,
            should_append: false,
            addition_text: Some("x".into()),
        };
        let envelope = LinkSuggestionsEnvelope { suggestions: vec![suggestion.clone()] };

        let envelope_struct_keys = object_keys(&serde_json::to_value(&envelope).expect("envelope serializes"));
        let envelope_schema_keys = object_keys(&schema["properties"]);
        assert_eq!(
            envelope_struct_keys, envelope_schema_keys,
            "envelope schema properties must match LinkSuggestionsEnvelope's fields exactly"
        );

        assert_strict_structured_output(&schema, "LinkSuggestionsEnvelope");

        let item_schema = &schema["properties"]["suggestions"]["items"];
        let item_struct_keys = object_keys(&serde_json::to_value(&suggestion).expect("suggestion serializes"));
        let item_schema_keys = object_keys(&item_schema["properties"]);
        assert_eq!(
            item_struct_keys, item_schema_keys,
            "the inner suggestion schema properties must match LinkSuggestion's fields exactly"
        );

        assert_strict_structured_output(item_schema, "LinkSuggestion (nested)");

        let sample = r#"{"suggestions":[{"uuid":"u","related":true,"should_append":false}]}"#;
        let env: LinkSuggestionsEnvelope = serde_json::from_str(sample).expect("sample parses");
        assert_eq!(env.suggestions.len(), 1);
    }

    /// A run can exit 0 and still return nothing usable — measured on codex,
    /// which answered success with an EMPTY last-message file. "No error" is
    /// therefore not the success criterion; a usable answer is.
    #[test]
    fn an_envelope_with_no_body_is_not_usable() {
        let empty = ExtractEnvelope {
            title: Some("t".into()),
            lessons_markdown: "   \n  ".into(),
            meta_lessons_markdown: None,
            tags: vec![],
            confidence: None,
        };
        assert!(empty.usable().is_err());

        let good = ExtractEnvelope {
            title: None,
            lessons_markdown: "## A point\n\nBody.".into(),
            meta_lessons_markdown: None,
            tags: vec![],
            confidence: None,
        };
        assert!(good.usable().is_ok());
    }
}
