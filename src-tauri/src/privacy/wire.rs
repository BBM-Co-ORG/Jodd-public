//! Mask/unmask for each provider input and output type (spec 2026-10-08
//! §4.3). This file is the one table of which fields are user text:
//! identifiers (`uuid`), enums, bools and `confidence` are never touched.
//!
//! Every struct is DESTRUCTURED exhaustively, never accessed by field: a new
//! field on any of these types is a compile error here, so it cannot reach a
//! provider (or come back) unmasked by omission.

use super::vault::Vault;
use crate::llm::provider::{
    CandidateSummary, ChatTurn, ExtractEnvelope, FolderSuggestionEnvelope, LinkSuggestion, LinkSuggestionsEnvelope,
    SourceDigest,
};

pub fn mask_all(v: &mut Vault, xs: &[String]) -> Vec<String> {
    xs.iter().map(|x| v.mask(x)).collect()
}

pub fn mask_candidates(v: &mut Vault, cs: &[CandidateSummary]) -> Vec<CandidateSummary> {
    cs.iter()
        .map(|c| {
            let CandidateSummary { uuid, title, snippet } = c;
            CandidateSummary { uuid: uuid.clone(), title: v.mask(title), snippet: v.mask(snippet) }
        })
        .collect()
}

pub fn mask_digests(v: &mut Vault, ds: &[SourceDigest]) -> Vec<SourceDigest> {
    ds.iter()
        .map(|d| {
            let SourceDigest { title, display_url, status, part, lessons_markdown } = d;
            SourceDigest {
                title: title.as_deref().map(|t| v.mask(t)),
                display_url: v.mask(display_url),
                status: status.clone(),
                part: part.clone(),
                lessons_markdown: v.mask(lessons_markdown),
            }
        })
        .collect()
}

pub fn mask_turns(v: &mut Vault, ts: &[ChatTurn]) -> Vec<ChatTurn> {
    ts.iter()
        .map(|t| {
            let ChatTurn { role, content } = t;
            ChatTurn { role: *role, content: v.mask(content) }
        })
        .collect()
}

/// Every string an input type carries, for `Vault::reserving`. Exhaustive
/// destructuring, like the maskers: a new field is a compile error here, so it
/// cannot be left unreserved. Identifiers and enums are reserved too —
/// over-reserving is harmless, under-reserving lets a planted token resolve.
pub fn candidate_texts(cs: &[CandidateSummary]) -> impl Iterator<Item = &str> {
    cs.iter().flat_map(|c| {
        let CandidateSummary { uuid, title, snippet } = c;
        [uuid.as_str(), title.as_str(), snippet.as_str()]
    })
}

pub fn digest_texts(ds: &[SourceDigest]) -> impl Iterator<Item = &str> {
    ds.iter().flat_map(|d| {
        let SourceDigest { title, display_url, status, part, lessons_markdown } = d;
        [
            title.as_deref().unwrap_or(""),
            display_url.as_str(),
            status.as_str(),
            part.as_deref().unwrap_or(""),
            lessons_markdown.as_str(),
        ]
    })
}

pub fn unmask_envelope(v: &mut Vault, e: ExtractEnvelope) -> ExtractEnvelope {
    let ExtractEnvelope { title, lessons_markdown, meta_lessons_markdown, tags, confidence } = e;
    ExtractEnvelope {
        title: title.map(|t| v.unmask(&t)),
        lessons_markdown: v.unmask(&lessons_markdown),
        meta_lessons_markdown: meta_lessons_markdown.map(|t| v.unmask(&t)),
        tags: tags.iter().map(|t| v.unmask(t)).collect(),
        confidence,
    }
}

pub fn unmask_links(v: &mut Vault, e: LinkSuggestionsEnvelope) -> LinkSuggestionsEnvelope {
    let LinkSuggestionsEnvelope { suggestions } = e;
    LinkSuggestionsEnvelope {
        suggestions: suggestions
            .into_iter()
            .map(|s| {
                let LinkSuggestion { uuid, related, should_append, addition_text } = s;
                LinkSuggestion { uuid, related, should_append, addition_text: addition_text.map(|t| v.unmask(&t)) }
            })
            .collect(),
    }
}

pub fn unmask_folder(v: &mut Vault, e: FolderSuggestionEnvelope) -> FolderSuggestionEnvelope {
    let FolderSuggestionEnvelope { folder, reason } = e;
    FolderSuggestionEnvelope { folder: folder.map(|f| v.unmask(&f)), reason: reason.map(|r| v.unmask(&r)) }
}

/// Content-free counts into the current receipt step. `metric_add`, not
/// `metric`: Ask makes two calls in one step, and both count. So a
/// `masked_<kind>` metric is "values masked per provider call, summed over
/// calls" — not distinct values per step: one address in both of Ask's calls
/// counts 2 (final review M3).
///
/// Must run AFTER unmasking: `Vault::unresolved` is only filled by `unmask`,
/// so recording earlier would always report `mask_unresolved` as 0.
pub fn record_metrics(v: &Vault) {
    for (kind, n) in v.counts() {
        crate::llm::receipts::metric_add(kind.metric_name(), n);
    }
    if v.unresolved() > 0 {
        crate::llm::receipts::metric_add("mask_unresolved", v.unresolved());
    }
}

#[cfg(test)]
mod tests {
    use crate::llm::policy::CheckedProvider;
    use crate::llm::provider::*;
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    // Synthetic values only.
    const EMAIL: &str = "alice@example.test";
    const PHONE: &str = "081-234-5678";
    const PASS: &str = "hunter22x";

    /// Records every input and echoes it back into every text output field —
    /// the worst case for unmasking, since every output carries tokens.
    struct Echo {
        seen: Arc<Mutex<Vec<String>>>,
    }
    fn env(text: &str) -> ExtractEnvelope {
        ExtractEnvelope {
            title: Some(text.into()),
            lessons_markdown: text.into(),
            meta_lessons_markdown: Some(text.into()),
            tags: vec![text.into()],
            confidence: Some("high".into()),
        }
    }
    #[async_trait::async_trait]
    impl LlmProvider for Echo {
        async fn extract(&self, s: &str, t: &[String], _: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            self.seen.lock().unwrap().push(format!("{s} {t:?}"));
            Ok(env(s))
        }
        async fn run_workflow(&self, w: WorkflowKind, s: &str, t: &[String], _: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            self.seen.lock().unwrap().push(format!("{s} {t:?}"));
            if w == WorkflowKind::ActionItems {
                // What the real providers do with a well-behaved answer:
                // the row quotes passage 1 verbatim, then validate + envelope.
                use crate::llm::meeting::{envelope, passages, validate, Item, Kind, Meeting};
                let first = passages(s)?[0].to_string();
                let m = Meeting {
                    items: vec![Item { kind: Kind::Action, text: first, owner: None, due: None, passage: 1 }],
                    incomplete: false,
                };
                validate(&m, s)?;
                return envelope(&m, s);
            }
            if s.contains("MALFORMED") {
                return Err(ExtractError::MalformedEnvelope { reason: "test".into(), raw: s.into() });
            }
            Ok(env(s))
        }
        async fn suggest_links(&self, s: &str, c: &[CandidateSummary], _: CancellationToken) -> Result<LinkSuggestionsEnvelope, ExtractError> {
            self.seen.lock().unwrap().push(format!("{s} {c:?}"));
            Ok(LinkSuggestionsEnvelope {
                suggestions: c
                    .iter()
                    .map(|c| LinkSuggestion { uuid: c.uuid.clone(), related: true, should_append: true, addition_text: Some(c.snippet.clone()) })
                    .collect(),
            })
        }
        async fn suggest_folder(&self, s: &str, f: &[String], _: CancellationToken) -> Result<FolderSuggestionEnvelope, ExtractError> {
            self.seen.lock().unwrap().push(format!("{s} {f:?}"));
            Ok(FolderSuggestionEnvelope { folder: f.first().cloned(), reason: Some(s.into()) })
        }
        async fn synthesize(&self, d: &[SourceDigest], c: &str, _: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            self.seen.lock().unwrap().push(format!("{d:?} {c}"));
            Ok(env(&format!("{} | {c}", d.iter().map(|d| d.lessons_markdown.as_str()).collect::<Vec<_>>().join(" | "))))
        }
        async fn chat(&self, s: &str, t: &[ChatTurn], _: CancellationToken) -> Result<String, ExtractError> {
            self.seen.lock().unwrap().push(format!("{s} {t:?}"));
            Ok(t.last().map(|t| t.content.clone()).unwrap_or_default())
        }
    }

    fn checked(gate: &Mutex<()>, seen: Arc<Mutex<Vec<String>>>) -> CheckedProvider<'_> {
        CheckedProvider { cancel: Mutex::new(None), gate, inner: Box::new(Echo { seen }), valid: Box::new(|| true), known: Default::default() }
    }

    fn assert_never_sent(seen: &Arc<Mutex<Vec<String>>>, raw: &[&str]) {
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty(), "the provider was never called");
        for sent in seen.iter() {
            for r in raw {
                assert!(!sent.contains(r), "{r} reached the provider: {sent}");
            }
            assert!(sent.contains('⟦'), "expected tokens in: {sent}");
        }
    }

    fn source() -> String {
        format!("Mail {EMAIL} or call {PHONE}. password: {PASS}")
    }

    #[tokio::test]
    async fn extract_masks_source_and_tags_and_restores_every_output_field() {
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        let p = checked(&gate, seen.clone());
        let tags = vec![format!("#{EMAIL}")];
        let e = p.extract(&source(), &tags, CancellationToken::new()).await.unwrap();
        assert_never_sent(&seen, &[EMAIL, PHONE, PASS]);
        assert_eq!(e.lessons_markdown, source());
        assert_eq!(e.title.as_deref(), Some(source().as_str()));
        assert_eq!(e.meta_lessons_markdown.as_deref(), Some(source().as_str()));
        assert_eq!(e.tags, vec![source()]);
        assert_eq!(e.confidence.as_deref(), Some("high"));
    }

    /// C1 (final review): Extract sends raw `body_html`. The masked span must
    /// be the value after the label, not the `</b>` between them.
    #[tokio::test]
    async fn extract_on_an_html_body_never_sends_the_value_after_a_bold_label() {
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        let p = checked(&gate, seen.clone());
        let body = format!("<div><b>Password:</b> {PASS}</div>");
        let e = p.extract(&body, &[], CancellationToken::new()).await.unwrap();
        assert_never_sent(&seen, &[PASS]);
        assert!(seen.lock().unwrap()[0].contains("</b>"), "the markup itself is not a secret");
        assert_eq!(e.lessons_markdown, body);
    }

    /// C2 (final review): a value found by its context in one call is masked
    /// where a later call through the same provider repeats it bare — the
    /// chunked transcript and ingest's map/synthesize share one provider.
    #[tokio::test]
    async fn a_value_detected_in_one_call_is_masked_in_the_next() {
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        let p = checked(&gate, seen.clone());
        let c = CancellationToken::new();
        p.run_workflow(WorkflowKind::CleanTranscript, "wifi pass: Tr0ub4dor", &[], c.clone()).await.unwrap();
        let e = p.run_workflow(WorkflowKind::CleanTranscript, "he typed Tr0ub4dor again", &[], c.clone()).await.unwrap();
        let d = vec![SourceDigest {
            title: None,
            display_url: "https://example.test/a".into(),
            status: "ok".into(),
            part: None,
            lessons_markdown: "the password Tr0ub4dor".into(),
        }];
        p.synthesize(&d, "context", c).await.unwrap();
        assert_never_sent(&seen, &["Tr0ub4dor"]);
        assert_eq!(e.lessons_markdown, "he typed Tr0ub4dor again");
    }

    #[tokio::test]
    async fn run_workflow_masks_and_restores() {
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        let p = checked(&gate, seen.clone());
        let e = p.run_workflow(WorkflowKind::Summarize, &source(), &[], CancellationToken::new()).await.unwrap();
        assert_never_sent(&seen, &[EMAIL, PHONE, PASS]);
        assert_eq!(e.lessons_markdown, source());
    }

    #[tokio::test]
    async fn an_action_items_quote_holding_pii_validates_and_renders_as_if_unmasked() {
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        let p = checked(&gate, seen.clone());
        // M1 (final review): a value holding `<` and `&` must come back
        // escaped in the evidence passage too, not as raw markup.
        let secret = "a<b>&c99";
        let row = format!("Call Somchai on {PHONE} about pass: {secret}");
        let src = format!("{row}\nSecond line");
        let e = p.run_workflow(WorkflowKind::ActionItems, &src, &[], CancellationToken::new()).await.unwrap();
        assert_never_sent(&seen, &[PHONE, secret]);
        // The row AND the evidence passage are rendered through
        // `meeting::literal` (punctuation as HTML entities, the token's `_`
        // included): both must come back exactly as an unmasked run renders.
        let lit = crate::llm::meeting::literal(&row);
        assert_eq!(e.lessons_markdown.matches(&lit).count(), 2, "{}", e.lessons_markdown);
        assert!(!e.lessons_markdown.contains('⟦'), "{}", e.lessons_markdown);
        assert!(!e.lessons_markdown.contains(secret), "unescaped: {}", e.lessons_markdown);
        // The note as saved: row, evidence and the source section all escaped.
        let html = crate::llm::markdown::assemble_note_body(&e, &src);
        assert!(html.matches("a&lt;b&gt;&amp;c99").count() >= 2, "{html}");
        assert!(!html.contains(secret), "{html}");
    }

    #[tokio::test]
    async fn suggest_links_masks_candidate_text_but_never_their_uuids() {
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        let p = checked(&gate, seen.clone());
        let uuid = "AAAAAAAA-0000-0000-0000-000000000000";
        let c = vec![CandidateSummary { uuid: uuid.into(), title: format!("Contact {EMAIL}"), snippet: format!("call {PHONE}") }];
        let out = p.suggest_links(&source(), &c, CancellationToken::new()).await.unwrap();
        assert_never_sent(&seen, &[EMAIL, PHONE, PASS]);
        assert!(seen.lock().unwrap()[0].contains(uuid), "uuids are identifiers, never masked");
        assert_eq!(out.suggestions[0].uuid, uuid);
        assert_eq!(out.suggestions[0].addition_text.as_deref(), Some(format!("call {PHONE}").as_str()));
    }

    #[tokio::test]
    async fn suggest_folder_restores_the_offered_path_exactly() {
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        let p = checked(&gate, seen.clone());
        // A folder named after a person's e-mail: the answer must still equal
        // an offered path byte for byte, or filing rejects it.
        let folders = vec![format!("Notes/Clients/{EMAIL}"), "Notes/Work".to_string()];
        let out = p.suggest_folder(&source(), &folders, CancellationToken::new()).await.unwrap();
        assert_never_sent(&seen, &[EMAIL, PHONE, PASS]);
        assert_eq!(out.folder.as_deref(), Some(folders[0].as_str()));
        assert_eq!(out.reason.as_deref(), Some(source().as_str()));
    }

    #[tokio::test]
    async fn synthesize_masks_digests_and_context() {
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        let p = checked(&gate, seen.clone());
        let d = vec![SourceDigest {
            title: Some(format!("by {EMAIL}")),
            display_url: "https://example.test/a".into(),
            status: "ok".into(),
            part: None,
            lessons_markdown: format!("call {PHONE}"),
        }];
        let e = p.synthesize(&d, &format!("for {EMAIL}"), CancellationToken::new()).await.unwrap();
        assert_never_sent(&seen, &[EMAIL, PHONE]);
        assert_eq!(e.lessons_markdown, format!("call {PHONE} | for {EMAIL}"));
    }

    #[tokio::test]
    async fn chat_masks_system_and_every_turn_and_restores_the_answer() {
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        let p = checked(&gate, seen.clone());
        let turns = vec![
            ChatTurn { role: ChatRole::User, content: format!("who is {EMAIL}?") },
            ChatTurn { role: ChatRole::Assistant, content: format!("they use {PHONE}") },
            ChatTurn { role: ChatRole::User, content: format!("NOTES:\n{}", source()) },
        ];
        let answer = p.chat(&format!("system with {EMAIL}"), &turns, CancellationToken::new()).await.unwrap();
        assert_never_sent(&seen, &[EMAIL, PHONE, PASS]);
        assert_eq!(answer, format!("NOTES:\n{}", source()));
    }

    #[tokio::test]
    async fn an_error_keeps_the_masked_text_it_carries() {
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        let p = checked(&gate, seen.clone());
        let err = p
            .run_workflow(WorkflowKind::Summarize, &format!("MALFORMED {EMAIL}"), &[], CancellationToken::new())
            .await
            .unwrap_err();
        let ExtractError::MalformedEnvelope { raw, .. } = err else { panic!("unexpected error") };
        assert!(!raw.contains(EMAIL) && raw.contains("⟦EMAIL_1⟧"), "errors travel to logs and UI: {raw}");
    }

    #[tokio::test]
    async fn receipt_metrics_count_masked_values_without_content() {
        let store = std::sync::Arc::new(std::sync::Mutex::new(crate::llm::receipts::Store::open(None).unwrap()));
        let (gate, seen) = (Mutex::new(()), Arc::new(Mutex::new(vec![])));
        crate::llm::receipts::run_in(store.clone(), "mask-metrics", None, "ask", async {
            let p = checked(&gate, seen.clone());
            let turns = vec![ChatTurn { role: ChatRole::User, content: source() }];
            p.chat("system", &turns, CancellationToken::new()).await.map_err(|e| e.to_string())?;
            p.chat("system", &turns, CancellationToken::new()).await.map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .await
        .unwrap();
        let row = store.lock().unwrap().list(None).remove(0);
        let m = &row.steps[0].metrics;
        assert_eq!(m.get("masked_email"), Some(&2), "two calls in one step both count: {m:?}");
        assert_eq!(m.get("masked_phone"), Some(&2));
        assert_eq!(m.get("masked_password"), Some(&2));
        let stored = serde_json::to_string(&row).unwrap();
        assert!(!stored.contains(EMAIL) && !stored.contains('⟦'), "{stored}");
    }

    /// A provider that invents a token it was never given.
    struct Inventor;
    #[async_trait::async_trait]
    impl LlmProvider for Inventor {
        async fn extract(&self, _: &str, _: &[String], _: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!()
        }
        async fn run_workflow(&self, _: WorkflowKind, _: &str, _: &[String], _: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!()
        }
        async fn suggest_links(&self, _: &str, _: &[CandidateSummary], _: CancellationToken) -> Result<LinkSuggestionsEnvelope, ExtractError> {
            unreachable!()
        }
        async fn suggest_folder(&self, _: &str, _: &[String], _: CancellationToken) -> Result<FolderSuggestionEnvelope, ExtractError> {
            unreachable!()
        }
        async fn synthesize(&self, _: &[SourceDigest], _: &str, _: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!()
        }
        async fn chat(&self, _: &str, _: &[ChatTurn], _: CancellationToken) -> Result<String, ExtractError> {
            Ok("\u{27E6}PHONE_9\u{27E7}".into())
        }
    }

    #[tokio::test]
    async fn an_invented_token_is_recorded_as_unresolved_after_unmasking() {
        let store = std::sync::Arc::new(std::sync::Mutex::new(crate::llm::receipts::Store::open(None).unwrap()));
        let gate = Mutex::new(());
        crate::llm::receipts::run_in(store.clone(), "mask-unresolved", None, "ask", async {
            let p = CheckedProvider { cancel: Mutex::new(None), gate: &gate, inner: Box::new(Inventor), valid: Box::new(|| true), known: Default::default() };
            let turns = vec![ChatTurn { role: ChatRole::User, content: "hello".into() }];
            let a = p.chat("system", &turns, CancellationToken::new()).await.map_err(|e| e.to_string())?;
            assert!(a.contains("PHONE_9"), "an unresolved token stays as the model wrote it: {a}");
            Ok::<(), String>(())
        })
        .await
        .unwrap();
        let row = store.lock().unwrap().list(None).remove(0);
        assert_eq!(row.steps[0].metrics.get("mask_unresolved"), Some(&1), "{:?}", row.steps[0].metrics);
    }
}
