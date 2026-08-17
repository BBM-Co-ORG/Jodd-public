//! Auto-link ingest core: deterministic candidate-term extraction, FTS-backed
//! candidate search, and LLM-judged relatedness/append decisions. See
//! docs/superpowers/specs/2026-07-20-auto-link-ingest-design.md.

/// English + Thai stopwords to exclude from keyword extraction — a short,
/// hand-picked list (not exhaustive; the capitalization/repetition signal
/// below does most of the actual filtering work). No regex crate in this
/// crate — matches extract_hashtags/extract_wikilinks/extract_urls's style.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "of", "to", "in", "on", "at", "for",
    "with", "is", "are", "was", "were", "be", "been", "this", "that", "it",
    "as", "by", "from", "not", "we", "you", "he", "she", "they",
    "และ", "หรือ", "ใน", "ที่", "เป็น", "จะ", "ได้", "มี", "ไม่", "ให้",
];

/// Extract up to `max_terms` candidate search terms from `text` — a
/// deterministic, no-LLM-call pre-filter for the FTS candidate search (design
/// spec decision 4). Splits on non-alphanumeric/non-hyphen characters, drops
/// stopwords and sub-3-character tokens, keeps a token if it appears
/// capitalized anywhere in the text (proper-noun-shaped) or repeats >= 2
/// times. Ranked by frequency descending, alphabetical on ties (determinism).
pub(crate) fn extract_keywords(text: &str, max_terms: usize) -> Vec<String> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut capitalized: std::collections::HashSet<String> = std::collections::HashSet::new();

    for raw_word in text.split(|c: char| !c.is_alphanumeric() && c != '-') {
        let word = raw_word.trim();
        if word.chars().count() < 3 {
            continue;
        }
        let lower = word.to_lowercase();
        if STOPWORDS.contains(&lower.as_str()) {
            continue;
        }
        if word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            capitalized.insert(lower.clone());
        }
        *counts.entry(lower).or_insert(0) += 1;
    }

    let mut candidates: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(word, count)| capitalized.contains(word) || *count >= 2)
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    candidates.into_iter().take(max_terms).map(|(w, _)| w).collect()
}

use crate::db::Db;
use crate::llm::provider::CandidateSummary;

/// Search for candidate related notes using keyword-extracted terms (Task 1),
/// merging results across terms and ranking by number of distinct terms
/// matched (design spec Step 1). Excludes `exclude_uuid` (the note being
/// linked can't suggest linking to itself) and caps the pool at
/// `max_candidates`.
pub(crate) fn find_candidates(
    db: &Db,
    account_id: &str,
    keywords: &[String],
    exclude_uuid: Option<&str>,
    max_candidates: usize,
) -> rusqlite::Result<Vec<CandidateSummary>> {
    let mut match_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut notes_by_uuid: std::collections::HashMap<String, crate::db::CachedNote> =
        std::collections::HashMap::new();

    for term in keywords {
        let results = db.search_notes(Some(account_id), None, term, &[])?;
        for note in results {
            if Some(note.uuid.as_str()) == exclude_uuid {
                continue;
            }
            *match_counts.entry(note.uuid.clone()).or_insert(0) += 1;
            notes_by_uuid.entry(note.uuid.clone()).or_insert(note);
        }
    }

    let mut ranked: Vec<(String, usize)> = match_counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    Ok(ranked
        .into_iter()
        .take(max_candidates)
        .filter_map(|(uuid, _)| notes_by_uuid.remove(&uuid))
        .map(|n| {
            let text = crate::db::strip_html_to_text(&n.body_html);
            let snippet: String = text.chars().take(200).collect();
            CandidateSummary { uuid: n.uuid, title: n.title, snippet }
        })
        .collect())
}

pub struct LinkTarget {
    pub uuid: String,
    pub title: String,
}

pub struct ProposedAppend {
    pub uuid: String,
    pub title: String,
    pub addition_text: String,
}

pub struct LinkSuggestions {
    pub auto_links: Vec<LinkTarget>,
    pub proposed_appends: Vec<ProposedAppend>,
}

/// Orchestrates the full auto-link flow (design spec Approach §1): extract
/// keywords (Task 1, no LLM call) → find candidates via FTS (Task 2) → one
/// LLM call judges relatedness/append (Tasks 3-5) → split results into
/// auto_links (every related candidate) and proposed_appends (only
/// should_append=true candidates, with the [[new-note-slug]] placeholder in
/// their addition_text substituted for the real slug of the note being
/// linked). NOTE: shares its name with `LlmProvider::suggest_links` (the
/// trait method it calls) — see that method's doc comment for why.
pub async fn suggest_links(
    provider: &dyn crate::llm::provider::LlmProvider,
    db: &Db,
    account_id: &str,
    exclude_uuid: Option<&str>,
    new_note_title: &str,
    new_note_uuid: &str,
    text: &str,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<LinkSuggestions, crate::llm::provider::ExtractError> {
    const MAX_KEYWORDS: usize = 8;
    const MAX_CANDIDATES: usize = 20;

    let keywords = extract_keywords(text, MAX_KEYWORDS);
    if keywords.is_empty() {
        return Ok(LinkSuggestions { auto_links: vec![], proposed_appends: vec![] });
    }

    let candidates = find_candidates(db, account_id, &keywords, exclude_uuid, MAX_CANDIDATES)
        .map_err(|e| crate::llm::provider::ExtractError::Transport(e.to_string()))?;
    if candidates.is_empty() {
        return Ok(LinkSuggestions { auto_links: vec![], proposed_appends: vec![] });
    }

    let envelope = provider.suggest_links(text, &candidates, cancel).await?;

    let titles_by_uuid: std::collections::HashMap<&str, &str> = candidates
        .iter()
        .map(|c| (c.uuid.as_str(), c.title.as_str()))
        .collect();

    let new_note_slug = crate::db::note_slug(new_note_title, new_note_uuid);
    let mut auto_links = Vec::new();
    let mut proposed_appends = Vec::new();

    for suggestion in envelope.suggestions {
        if !suggestion.related {
            continue;
        }
        let Some(&title) = titles_by_uuid.get(suggestion.uuid.as_str()) else {
            // LLM referenced a uuid we didn't send — ignore rather than
            // trust unverified data from the model.
            continue;
        };
        auto_links.push(LinkTarget { uuid: suggestion.uuid.clone(), title: title.to_string() });

        if suggestion.should_append {
            if let Some(addition_text) = suggestion.addition_text {
                let substituted = addition_text.replace("[[new-note-slug]]", &format!("[[{new_note_slug}]]"));
                proposed_appends.push(ProposedAppend {
                    uuid: suggestion.uuid,
                    title: title.to_string(),
                    addition_text: substituted,
                });
            }
        }
    }

    Ok(LinkSuggestions { auto_links, proposed_appends })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Db {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        Db::open_unencrypted(&path).expect("open temp db")
    }

    fn mk_note(account_id: &str, uuid: &str, title: &str, body_html: &str) -> crate::db::CachedNote {
        crate::db::CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: title.to_string(),
            body_html: body_html.to_string(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 0,
            remote_version: None,
            sync_state: crate::db::SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: 0,
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
        }
    }

    #[test]
    fn find_candidates_ranks_by_distinct_term_matches() {
        let db = temp_db();
        let acct = "test@example.com";
        db.insert_local_new(&mk_note(acct, "AAAAAAAA-0000-0000-0000-000000000000", "Kubernetes Deployment", "<div>kubernetes scaling notes</div>")).unwrap();
        db.insert_local_new(&mk_note(acct, "BBBBBBBB-0000-0000-0000-000000000000", "Scaling Only", "<div>just scaling here</div>")).unwrap();

        let keywords = vec!["kubernetes".to_string(), "scaling".to_string()];
        let candidates = find_candidates(&db, acct, &keywords, None, 20).unwrap();

        assert_eq!(candidates.len(), 2);
        // The note matching both terms ranks first.
        assert_eq!(candidates[0].title, "Kubernetes Deployment");
    }

    #[test]
    fn find_candidates_excludes_self() {
        let db = temp_db();
        let acct = "test@example.com";
        let uuid = "CCCCCCCC-0000-0000-0000-000000000000";
        db.insert_local_new(&mk_note(acct, uuid, "Self Note", "<div>kubernetes here</div>")).unwrap();

        let keywords = vec!["kubernetes".to_string()];
        let candidates = find_candidates(&db, acct, &keywords, Some(uuid), 20).unwrap();
        assert!(candidates.is_empty());
    }

    #[test]
    fn find_candidates_caps_pool_size() {
        let db = temp_db();
        let acct = "test@example.com";
        for i in 0..5 {
            let uuid = format!("{i:08}-0000-0000-0000-000000000000");
            db.insert_local_new(&mk_note(acct, &uuid, &format!("Note {i}"), "<div>kubernetes</div>")).unwrap();
        }
        let keywords = vec!["kubernetes".to_string()];
        let candidates = find_candidates(&db, acct, &keywords, None, 3).unwrap();
        assert_eq!(candidates.len(), 3);
    }

    #[test]
    fn extract_keywords_prefers_capitalized_and_repeated() {
        let text = "We discussed Kubernetes deployment. Kubernetes handles scaling well. The team liked it.";
        let keywords = extract_keywords(text, 8);
        assert!(keywords.contains(&"kubernetes".to_string()));
    }

    #[test]
    fn extract_keywords_drops_stopwords_and_short_tokens() {
        let text = "the and or a an is at to be";
        assert_eq!(extract_keywords(text, 8), Vec::<String>::new());
    }

    #[test]
    fn extract_keywords_caps_at_max_terms() {
        let text = "Alpha Beta Gamma Delta Epsilon Zeta Eta Theta Iota Kappa";
        let keywords = extract_keywords(text, 3);
        assert_eq!(keywords.len(), 3);
    }

    #[test]
    fn extract_keywords_ranks_by_frequency_then_alphabetical() {
        let text = "Zebra Zebra Apple Apple Mango";
        // "zebra" and "apple" both repeat (count=2), "mango" is capitalized-once
        // (count=1, but capitalized so it qualifies too). Frequency desc, then
        // alphabetical on the count=2 tie: "apple" before "zebra".
        let keywords = extract_keywords(text, 3);
        assert_eq!(keywords, vec!["apple".to_string(), "zebra".to_string(), "mango".to_string()]);
    }

    struct FakeProvider {
        response: crate::llm::provider::LinkSuggestionsEnvelope,
    }

    #[async_trait::async_trait]
    impl crate::llm::provider::LlmProvider for FakeProvider {
        async fn extract(
            &self,
            _source: &str,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<crate::llm::provider::ExtractEnvelope, crate::llm::provider::ExtractError> {
            unimplemented!("not used by this test")
        }

        async fn suggest_links(
            &self,
            _source: &str,
            _candidates: &[crate::llm::provider::CandidateSummary],
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<crate::llm::provider::LinkSuggestionsEnvelope, crate::llm::provider::ExtractError> {
            Ok(self.response.clone())
        }

        async fn chat(
            &self,
            _system: &str,
            _turns: &[crate::llm::provider::ChatTurn],
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<String, crate::llm::provider::ExtractError> {
            unimplemented!("not used by this test")
        }
    }

    #[tokio::test]
    async fn suggest_links_splits_auto_links_and_proposed_appends() {
        let db = temp_db();
        let acct = "test@example.com";
        let related_uuid = "DDDDDDDD-0000-0000-0000-000000000000";
        db.insert_local_new(&mk_note(acct, related_uuid, "Related Note", "<div>kubernetes deployment</div>")).unwrap();

        let provider = FakeProvider {
            response: crate::llm::provider::LinkSuggestionsEnvelope {
                suggestions: vec![crate::llm::provider::LinkSuggestion {
                    uuid: related_uuid.to_string(),
                    related: true,
                    should_append: true,
                    addition_text: Some("See [[new-note-slug]] for more.".to_string()),
                }],
            },
        };

        let new_uuid = "EEEEEEEE-0000-0000-0000-000000000000";
        let result = suggest_links(
            &provider,
            &db,
            acct,
            Some(new_uuid),
            "New Note",
            new_uuid,
            // Capitalized so extract_keywords' proper-noun signal qualifies
            // "kubernetes" as a keyword even though it appears only once in
            // this text (extract_keywords otherwise requires count >= 2 —
            // an all-lowercase, no-repeats text like "kubernetes deployment
            // discussion" yields zero keywords and never reaches
            // find_candidates/the provider at all; see task-6-report.md).
            "Kubernetes deployment discussion",
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(result.auto_links.len(), 1);
        assert_eq!(result.auto_links[0].uuid, related_uuid);
        assert_eq!(result.proposed_appends.len(), 1);
        // The [[new-note-slug]] placeholder must be substituted with the
        // real slug of the note being linked (new_note_title/new_note_uuid).
        let expected_slug = crate::db::note_slug("New Note", new_uuid);
        assert!(result.proposed_appends[0].addition_text.contains(&expected_slug));
        assert!(!result.proposed_appends[0].addition_text.contains("[[new-note-slug]]"));
    }

    #[tokio::test]
    async fn suggest_links_ignores_uuid_llm_did_not_receive() {
        // Guards the titles_by_uuid lookup in suggest_links: a suggestion
        // referencing a uuid outside the candidate set we actually sent to
        // the provider (a hallucinated or malformed model response) must be
        // dropped, not trusted — it must not appear in auto_links or
        // proposed_appends, and must not prevent a legitimate suggestion in
        // the same response from being applied.
        let db = temp_db();
        let acct = "test@example.com";
        let real_uuid = "AAAAAAAA-0000-0000-0000-000000000000";
        db.insert_local_new(&mk_note(acct, real_uuid, "Real Note", "<div>kubernetes deployment</div>")).unwrap();

        let hallucinated_uuid = "BBBBBBBB-0000-0000-0000-000000000000";
        let provider = FakeProvider {
            response: crate::llm::provider::LinkSuggestionsEnvelope {
                suggestions: vec![
                    crate::llm::provider::LinkSuggestion {
                        uuid: real_uuid.to_string(),
                        related: true,
                        should_append: false,
                        addition_text: None,
                    },
                    crate::llm::provider::LinkSuggestion {
                        uuid: hallucinated_uuid.to_string(),
                        related: true,
                        should_append: true,
                        addition_text: Some("See [[new-note-slug]] for more.".to_string()),
                    },
                ],
            },
        };

        let new_uuid = "CCCCCCCC-0000-0000-0000-000000000000";
        let result = suggest_links(
            &provider,
            &db,
            acct,
            Some(new_uuid),
            "New Note",
            new_uuid,
            "Kubernetes deployment discussion",
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(result.auto_links.len(), 1);
        assert_eq!(result.auto_links[0].uuid, real_uuid);
        assert!(result.proposed_appends.is_empty());
    }

    #[tokio::test]
    async fn suggest_links_empty_candidate_pool_returns_empty_suggestions() {
        let db = temp_db();
        let acct = "test@example.com";
        let provider = FakeProvider {
            response: crate::llm::provider::LinkSuggestionsEnvelope { suggestions: vec![] },
        };
        let result = suggest_links(
            &provider,
            &db,
            acct,
            None,
            "New Note",
            "FFFFFFFF-0000-0000-0000-000000000000",
            "a completely unrelated novel topic with no matches",
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(result.auto_links.is_empty());
        assert!(result.proposed_appends.is_empty());
    }
}
