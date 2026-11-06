//! Per-turn orchestration (spec §5). Deliberately free of Tauri types so it
//! is testable with a fake provider and a temp-dir DB — no app handle, no
//! runtime.

use std::collections::HashSet;

use tokio_util::sync::CancellationToken;

use crate::ask::{catalog, context, pool, prompt, AskAnswer, AskScope, CitedNote};
use crate::db::Db;
use crate::llm::provider::{ChatRole, ChatTurn, ExtractError, LlmProvider};

pub async fn run_ask(
    db: &Db,
    provider: &dyn LlmProvider,
    scope: &AskScope,
    turns: &[ChatTurn],
    cancel: CancellationToken,
    exclude_accounts: &[String],
    ai: &crate::privacy::ai_scope::AiScope,
) -> Result<AskAnswer, ExtractError> {
    provider.check()?;
    if let AskScope::Note { account_id, uuid, links, tags, .. } = scope {
        return run_note_ask(db, provider, scope, account_id, uuid, *links, *tags, turns, cancel, exclude_accounts, ai).await;
    }
    if let AskScope::Folder { account_id, label } = scope {
        ai.check_folder(account_id, label).map_err(ExtractError::Refused)?;
    }
    crate::llm::receipts::stage("retrieving");
    crate::llm::receipts::source_version(&serde_json::to_vec(scope).unwrap_or_default());
    let notes_in_scope = if scope.account_id().is_some_and(|id| exclude_accounts.iter().any(|x| x == id)) {
        0
    } else {
        db.count_notes_in_scope(scope.account_id(), scope.label(), exclude_accounts)
            .map_err(|e| ExtractError::Transport(format!("count_notes_in_scope: {e}")))?
    };

    // The question that drives retrieval is the latest user turn; the whole
    // conversation still goes to the model, so a follow-up like "what about
    // the other one?" retrieves against accumulated context.
    let question = turns
        .iter()
        .rev()
        .find(|t| t.role == ChatRole::User)
        .map(|t| t.content.as_str())
        .unwrap_or("");

    // ── Stage 1: pre-filter ───────────────────────────────────────────────
    let candidates = pool::build_candidate_pool(db, scope, question, exclude_accounts, ai)
        .map_err(|e| ExtractError::Transport(format!("build_candidate_pool: {e}")))?;
    let notes_considered = candidates.len();
    crate::llm::receipts::metric("notes_in_scope", notes_in_scope);
    crate::llm::receipts::metric("notes_considered", notes_considered);
    // Includes only the admitted retrieval snapshot; the salted digest is stored,
    // never the catalog itself or account/title/tag/folder values.
    crate::llm::receipts::source_version(format!("{:?}", candidates).as_bytes());
    crate::llm::receipts::stage("selecting_sources");

    if candidates.is_empty() {
        // Nothing to read: answering without an LLM call is both cheaper and
        // more honest than asking a model to say "I found nothing".
        return Ok(AskAnswer {
            markdown: "I couldn't find any notes in this scope.".into(),
            cited: Vec::new(),
            notes_in_scope,
            notes_considered: 0,
            notes_used: 0,
            trimmed: false,
            dropped_citations: 0,
            read: Vec::new(),
            conversation_id: None,
        });
    }

    let fts_uuids: HashSet<String> = candidates
        .iter()
        .filter(|c| c.from_fts)
        .map(|c| c.uuid.clone())
        .collect();

    // ── Stage 2 + 3: catalog, then the model selects ──────────────────────
    let (catalog_text, index) = catalog::build_catalog(&candidates);
    let mut select_turns = turns.to_vec();
    select_turns.push(ChatTurn {
        role: ChatRole::User,
        content: format!("CATALOG:\n{catalog_text}"),
    });
    let selection_raw = provider
        .chat(&prompt::SELECT_SYSTEM_PROMPT, &select_turns, cancel.clone())
        .await?;
    crate::llm::receipts::check("selection_ids_checked");
    let selected = catalog::parse_selected_uuid8s(&selection_raw, &index);

    if selected.is_empty() {
        // The model saw the catalog and picked nothing resolvable. Spending a
        // second call to have it say so anyway would be pure cost.
        return Ok(AskAnswer {
            markdown: "I couldn't find anything relevant in your notes for that.".into(),
            cited: Vec::new(),
            notes_in_scope,
            notes_considered,
            notes_used: 0,
            trimmed: false,
            dropped_citations: 0,
            read: Vec::new(),
            conversation_id: None,
        });
    }

    // ── Stage 4: answer ───────────────────────────────────────────────────
    // Check before spending stage 4's LLM call: `AgentCliProvider::run_once`
    // (agent_cli.rs:379-411) spawns the child process before it reaches its
    // `tokio::select!`, so a cancel landing here would otherwise launch an
    // agent CLI only to immediately kill it.
    if cancel.is_cancelled() {
        return Err(ExtractError::Cancelled);
    }

    provider.check()?;
    let ctx = context::build_answer_context(db, &selected, &fts_uuids, ai)
        .map_err(|e| ExtractError::Transport(format!("build_answer_context: {e}")))?;
    crate::llm::receipts::source_version(format!("{:?}", ctx).as_bytes());
    crate::llm::receipts::metric("notes_read", ctx.notes.len());
    if ctx.trimmed { crate::llm::receipts::check("context_trimmed"); }

    let mut answer_turns = turns.to_vec();
    answer_turns.push(ChatTurn {
        role: ChatRole::User,
        content: format!("NOTES:\n{}", context::render_context(&ctx)),
    });
    crate::llm::receipts::stage("answering");
    let raw = provider
        .chat(prompt::ANSWER_SYSTEM_PROMPT, &answer_turns, cancel)
        .await?;

    provider.check()?;
    crate::llm::receipts::stage("validating_citations");
    crate::llm::receipts::check("citation_ids_checked_not_grounding");
    let (markdown, cited, dropped_citations) = resolve_citations(&raw, &ctx);
    crate::llm::receipts::metric("invalid_citations_removed", dropped_citations);

    Ok(AskAnswer {
        markdown,
        cited,
        notes_in_scope,
        notes_considered,
        notes_used: ctx.notes.len(),
        trimmed: ctx.trimmed,
        dropped_citations,
        read: ctx
            .notes
            .iter()
            .map(|n| crate::ask::history::NoteRef {
                uuid: n.uuid.clone(),
                account_id: n.account_id.clone(),
            })
            .collect(),
        conversation_id: None,
    })
}

/// A note-scoped turn (spec 2026-10-23 §5.2–5.4). The focus note is always
/// read; with no related notes the selection call is skipped, so a lone note
/// costs one LLM call and can never come back "nothing relevant".
#[allow(clippy::too_many_arguments)]
async fn run_note_ask(
    db: &Db,
    provider: &dyn LlmProvider,
    scope: &AskScope,
    account_id: &str,
    uuid: &str,
    links: bool,
    tags: bool,
    turns: &[ChatTurn],
    cancel: CancellationToken,
    exclude_accounts: &[String],
    ai: &crate::privacy::ai_scope::AiScope,
) -> Result<AskAnswer, ExtractError> {
    let transport = |what: &str, e: rusqlite::Error| ExtractError::Transport(format!("{what}: {e}"));
    if exclude_accounts.iter().any(|x| x == account_id) {
        return Err(ExtractError::Refused(crate::ask::NOTE_GONE.into()));
    }
    // A note created moments ago may have been rekeyed by its first push
    // (gotcha #16): follow the alias before reading.
    let uuid = db.resolve_note_uuid(uuid, account_id).map_err(|e| transport("resolve_note_uuid", e))?;
    let focus = db
        .note_by_uuid(account_id, &uuid)
        .map_err(|e| transport("note_by_uuid", e))?
        .ok_or_else(|| ExtractError::Refused(crate::ask::NOTE_GONE.into()))?;
    ai.check_label(account_id, &focus.label).map_err(ExtractError::Refused)?;

    crate::llm::receipts::stage("retrieving");
    crate::llm::receipts::source_version(&serde_json::to_vec(scope).unwrap_or_default());
    let related = crate::ask::related::walk_related(
        db, &focus, links, tags, scope.note_depth(), crate::ask::CANDIDATE_POOL_MAX, ai,
    )
    .map_err(|e| transport("walk_related", e))?;
    let notes_in_scope = 1 + related.notes.len();
    crate::llm::receipts::metric("notes_in_scope", notes_in_scope);
    crate::llm::receipts::metric("notes_considered", notes_in_scope);

    let mut selected: Vec<(String, String)> = Vec::new();
    let mut fts_uuids: HashSet<String> = HashSet::new();
    if !related.notes.is_empty() {
        let question = turns.iter().rev().find(|t| t.role == ChatRole::User).map(|t| t.content.as_str()).unwrap_or("");
        let candidates = pool::candidates_from_related(db, &related.notes, question, exclude_accounts)
            .map_err(|e| transport("candidates_from_related", e))?;
        crate::llm::receipts::source_version(format!("{:?}", candidates).as_bytes());
        crate::llm::receipts::stage("selecting_sources");
        fts_uuids = candidates.iter().filter(|c| c.from_fts).map(|c| c.uuid.clone()).collect();
        let focus_tags = db.list_tags_for(account_id, &focus.uuid).map_err(|e| transport("list_tags_for", e))?;
        let (catalog_text, index) = catalog::build_catalog(&candidates);
        let mut select_turns = turns.to_vec();
        select_turns.push(ChatTurn {
            role: ChatRole::User,
            content: format!(
                "The question is about the note \u{201c}{}\u{201d} (tags: {}). Pick related notes that help answer it.\n\nCATALOG:\n{catalog_text}",
                focus.title,
                if focus_tags.is_empty() { "none".to_string() } else { focus_tags.join(", ") },
            ),
        });
        let raw = provider.chat(&prompt::NOTE_SELECT_SYSTEM_PROMPT, &select_turns, cancel.clone()).await?;
        crate::llm::receipts::check("selection_ids_checked");
        selected = catalog::parse_selected_uuid8s(&raw, &index);
        // No pick is fine: the focus note alone answers.
    }

    if cancel.is_cancelled() {
        return Err(ExtractError::Cancelled);
    }
    provider.check()?;
    let ctx = context::build_note_answer_context(db, &focus, &selected, &fts_uuids, ai)
        .map_err(|e| transport("build_note_answer_context", e))?;
    let Some(focus_slug) = ctx.notes.first().map(|n| n.slug.clone()) else {
        return Err(ExtractError::Refused(crate::privacy::ai_scope::HIDDEN_NOTE.into()));
    };
    crate::llm::receipts::source_version(format!("{:?}", ctx).as_bytes());
    crate::llm::receipts::metric("notes_read", ctx.notes.len());
    if ctx.trimmed { crate::llm::receipts::check("context_trimmed"); }

    let mut answer_turns = turns.to_vec();
    answer_turns.push(ChatTurn {
        role: ChatRole::User,
        // Not cited: the reader already knows the answer comes from this note,
        // and a chip on every line buried the answer (live pass 2026-10-24).
        content: format!(
            "The question is about the note \u{201c}{}\u{201d}. Do not cite it: the reader already knows the answer comes from it.{}\n\nNOTES:\n{}",
            focus.title,
            if ctx.notes.len() > 1 { " Cite the other, related notes with their [[slug]] as usual." } else { "" },
            context::render_context(&ctx)
        ),
    });
    crate::llm::receipts::stage("answering");
    let raw = provider.chat(prompt::ANSWER_SYSTEM_PROMPT, &answer_turns, cancel).await?;

    provider.check()?;
    crate::llm::receipts::stage("validating_citations");
    crate::llm::receipts::check("citation_ids_checked_not_grounding");
    let (markdown, mut cited, dropped_citations) = resolve_citations(&raw, &ctx);
    crate::llm::receipts::metric("invalid_citations_removed", dropped_citations);
    // The prompt asks for no focus citation; this guarantees it. The note
    // stays in `read`, which is what history's resume checks use.
    let markdown = strip_citation(&markdown, &focus_slug);
    cited.retain(|c| c.slug != focus_slug);

    Ok(AskAnswer {
        markdown,
        cited,
        notes_in_scope,
        notes_considered: notes_in_scope,
        notes_used: ctx.notes.len(),
        trimmed: ctx.trimmed,
        dropped_citations,
        read: ctx
            .notes
            .iter()
            .map(|n| crate::ask::history::NoteRef { uuid: n.uuid.clone(), account_id: n.account_id.clone() })
            .collect(),
        conversation_id: None,
    })
}

/// Remove every `[[slug]]` for one note, with the spaces before it, and the
/// trailing whitespace that leaves — "claim [[slug]]." becomes "claim.", and a
/// line that held only the citation becomes empty.
fn strip_citation(markdown: &str, slug: &str) -> String {
    let needle = format!("[[{slug}]]");
    let out: Vec<String> = markdown
        .lines()
        .map(|line| {
            let mut l = line.to_string();
            while let Some(at) = l.find(&needle) {
                let start = l[..at].trim_end_matches([' ', '\t']).len();
                l.replace_range(start..at + needle.len(), "");
            }
            l.trim_end().to_string()
        })
        .collect();
    out.join("\n").trim_end().to_string()
}

/// Keep `[[slug]]` citations that name a note actually in context; strip the
/// rest and count them. A model that invents a citation is the failure mode
/// that most undermines trust in a RAG answer, so it is neutralized here
/// rather than surfaced as plausible-looking text.
fn resolve_citations(raw: &str, ctx: &crate::ask::AnswerContext) -> (String, Vec<CitedNote>, usize) {
    let known: std::collections::HashMap<&str, &crate::ask::SelectedNote> =
        ctx.notes.iter().map(|n| (n.slug.as_str(), n)).collect();

    let mut out = String::with_capacity(raw.len());
    let mut cited: Vec<CitedNote> = Vec::new();
    let mut cited_slugs: HashSet<String> = HashSet::new();
    let mut dropped = 0usize;

    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        // Hand-rolled scan, matching db.rs's extract_wikilinks style (no regex
        // crate anywhere in this codebase).
        if i + 1 < chars.len() && chars[i] == '[' && chars[i + 1] == '[' {
            if let Some(close) = find_close(&chars, i + 2) {
                let inner: String = chars[i + 2..close].iter().collect();
                let slug = inner.trim();
                match known.get(slug) {
                    Some(n) => {
                        out.push_str(&format!("[[{slug}]]"));
                        if cited_slugs.insert(slug.to_string()) {
                            cited.push(CitedNote {
                                uuid: n.uuid.clone(),
                                account_id: n.account_id.clone(),
                                title: n.title.clone(),
                                slug: n.slug.clone(),
                            });
                        }
                    }
                    None => dropped += 1, // stripped: emit nothing
                }
                i = close + 2;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, cited, dropped)
}

/// Index of the `]]` that closes a link opened before `from`, or None.
fn find_close(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == ']' && chars[i + 1] == ']' {
            return Some(i);
        }
        // A newline before the close means it was never a link.
        if chars[i] == '\n' {
            return None;
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::llm::provider::{
        CandidateSummary, ChatRole, ChatTurn, ExtractEnvelope, ExtractError, FolderSuggestionEnvelope,
        LinkSuggestionsEnvelope, LlmProvider, SourceDigest,
    };
    use crate::test_support::temp_db;
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    /// Returns canned text per call, and records what it was asked.
    struct FakeProvider {
        replies: Mutex<Vec<String>>,
        seen: Arc<Mutex<Vec<String>>>,
        /// Cancel the token when the Nth chat call arrives (0-based), to test
        /// that stage 4 is never reached.
        cancel_on_call: Option<usize>,
        calls: Mutex<usize>,
    }

    impl FakeProvider {
        fn new(replies: Vec<&str>) -> (Self, Arc<Mutex<Vec<String>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (
                FakeProvider {
                    replies: Mutex::new(replies.into_iter().map(String::from).collect()),
                    seen: seen.clone(),
                    cancel_on_call: None,
                    calls: Mutex::new(0),
                },
                seen,
            )
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for FakeProvider {
        async fn extract(&self, _s: &str, _t: &[String], _c: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("Ask Jodd never calls extract")
        }
        async fn run_workflow(
            &self,
            _w: crate::llm::provider::WorkflowKind,
            _s: &str,
            _t: &[String],
            _c: CancellationToken,
        ) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("Ask Jodd never calls run_workflow")
        }
        async fn suggest_links(
            &self,
            _s: &str,
            _c: &[CandidateSummary],
            _t: CancellationToken,
        ) -> Result<LinkSuggestionsEnvelope, ExtractError> {
            unreachable!("Ask Jodd never calls suggest_links")
        }
        async fn suggest_folder(
            &self,
            _t: &str,
            _f: &[String],
            _c: CancellationToken,
        ) -> Result<FolderSuggestionEnvelope, ExtractError> {
            unreachable!("Ask Jodd never calls suggest_folder")
        }
        async fn synthesize(
            &self,
            _d: &[SourceDigest],
            _c: &str,
            _t: CancellationToken,
        ) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("Ask Jodd never calls synthesize")
        }
        async fn chat(
            &self,
            system: &str,
            turns: &[ChatTurn],
            cancel: CancellationToken,
        ) -> Result<String, ExtractError> {
            let mut n = self.calls.lock().unwrap();
            let this_call = *n;
            *n += 1;
            drop(n);

            if Some(this_call) == self.cancel_on_call {
                cancel.cancel();
                return Err(ExtractError::Cancelled);
            }
            let mut all = String::from(system);
            for t in turns {
                all.push_str(&t.content);
            }
            self.seen.lock().unwrap().push(all);
            let mut r = self.replies.lock().unwrap();
            if r.is_empty() {
                Ok(String::new())
            } else {
                Ok(r.remove(0))
            }
        }
    }

    fn seed(db: &Db, acct: &str, uuid: &str, title: &str, body: &str) {
        crate::test_support::note(acct, uuid).title(title).body(body).insert(db);
    }

    fn ask(q: &str) -> Vec<ChatTurn> {
        vec![ChatTurn { role: ChatRole::User, content: q.into() }]
    }

    #[tokio::test]
    async fn the_answer_lists_every_note_it_read_not_only_the_cited_ones() {
        // Two notes selected, one cited. Both are in `read`: the uncited one
        // still shaped the answer (spec 2026-10-14 §4, R2/R4).
        let db = temp_db();
        let a = "acct@x";
        let first = "aabbccdd-0000-0000-0000-000000000000";
        let second = "11223344-0000-0000-0000-000000000000";
        seed(&db, a, first, "Sync conflicts", "we chose keep-both reconciliation");
        seed(&db, a, second, "Sync notes", "background worker drains dirty rows");
        let (p, _) = FakeProvider::new(vec![
            "aabbccdd 11223344",
            "We chose keep-both. [[sync-conflicts-aabbccdd]]",
        ]);
        let out = run_ask(
            &db, &p, &crate::ask::AskScope::Account { account_id: a.into() },
            &ask("what about sync?"), CancellationToken::new(),
            &[], &crate::privacy::ai_scope::AiScope::default(),
        ).await.unwrap();

        assert_eq!(out.cited.len(), 1);
        let mut read: Vec<&str> = out.read.iter().map(|r| r.uuid.as_str()).collect();
        read.sort();
        assert_eq!(read, [second, first], "both notes, sorted ('1122…' < 'aabb…')");
        assert!(out.read.iter().all(|r| r.account_id == a));
        assert_eq!(out.conversation_id, None, "run_ask never sets it");
    }

    #[tokio::test]
    async fn excluded_folder_never_sends_metadata_or_body() {
        let db = temp_db();
        crate::test_support::note("denied", "deadbeef-0000")
            .title("FORBIDDEN_TITLE").body("FORBIDDEN_BODY #FORBIDDEN_TAG")
            .label("Notes/FORBIDDEN_FOLDER").insert(&db);
        let (p, seen) = FakeProvider::new(vec!["deadbeef", "answer"]);
        run_ask(&db, &p, &AskScope::Folder {
            account_id: "denied".into(), label: "Notes/FORBIDDEN_FOLDER".into(),
        }, &ask("anything"), CancellationToken::new(), &["denied".into()], &crate::privacy::ai_scope::AiScope::default()).await.unwrap();
        assert!(seen.lock().unwrap().is_empty(), "excluded folder must never dispatch");
    }

    #[tokio::test]
    async fn selection_reaches_the_answer_call_and_citations_resolve() {
        let db = temp_db();
        let a = "acct@x";
        let uuid = "aabbccdd-0000-0000-0000-000000000000";
        seed(&db, a, uuid, "Sync conflicts", "we chose keep-both reconciliation");

        // Call 1 selects the note by its uuid8; call 2 answers citing its slug.
        let (p, seen) = FakeProvider::new(vec![
            "aabbccdd",
            "We chose keep-both. [[sync-conflicts-aabbccdd]]",
        ]);
        let out = run_ask(
            &db,
            &p,
            &crate::ask::AskScope::Account { account_id: a.into() },
            &ask("what did I decide about sync conflicts?"),
            CancellationToken::new(),
            &[], &crate::privacy::ai_scope::AiScope::default(),
        )
        .await
        .unwrap();

        assert_eq!(out.notes_used, 1);
        assert_eq!(out.cited.len(), 1);
        assert_eq!(out.cited[0].uuid, uuid);
        assert_eq!(out.dropped_citations, 0);
        // The answer call must have received the note body.
        let calls = seen.lock().unwrap();
        assert!(calls[1].contains("keep-both reconciliation"), "answer call lacks the body");
    }

    #[tokio::test]
    async fn reports_scope_and_pool_sizes() {
        let db = temp_db();
        let a = "acct@x";
        for i in 0..5 {
            seed(&db, a, &format!("u{i}"), "t", "body");
        }
        let (p, _) = FakeProvider::new(vec!["NONE", "nothing relevant"]);
        let out = run_ask(
            &db,
            &p,
            &crate::ask::AskScope::Account { account_id: a.into() },
            &ask("anything"),
            CancellationToken::new(),
            &[], &crate::privacy::ai_scope::AiScope::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.notes_in_scope, 5);
        assert_eq!(out.notes_considered, 5);
    }

    #[tokio::test]
    async fn empty_scope_answers_without_any_llm_call() {
        let db = temp_db();
        let (p, seen) = FakeProvider::new(vec!["should never be used"]);
        let out = run_ask(
            &db,
            &p,
            &crate::ask::AskScope::Account { account_id: "empty@x".into() },
            &ask("anything"),
            CancellationToken::new(),
            &[], &crate::privacy::ai_scope::AiScope::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.notes_used, 0);
        assert!(seen.lock().unwrap().is_empty(), "no LLM call for an empty scope");
    }

    #[tokio::test]
    async fn no_resolvable_selection_skips_the_answer_call() {
        let db = temp_db();
        let a = "acct@x";
        seed(&db, a, "u1", "Alpha", "aaa");
        // Call 1 returns an id that was never in the catalog.
        let (p, seen) = FakeProvider::new(vec!["deadbeef", "unused"]);
        let out = run_ask(
            &db,
            &p,
            &crate::ask::AskScope::Account { account_id: a.into() },
            &ask("zzz qqq www"),
            CancellationToken::new(),
            &[], &crate::privacy::ai_scope::AiScope::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.notes_used, 0);
        assert_eq!(seen.lock().unwrap().len(), 1, "only the selection call ran");
    }

    #[tokio::test]
    async fn unknown_citations_are_stripped_and_counted() {
        let db = temp_db();
        let a = "acct@x";
        let uuid = "aabbccdd-0000-0000-0000-000000000000";
        seed(&db, a, uuid, "Sync conflicts", "keep-both");
        let (p, _) = FakeProvider::new(vec![
            "aabbccdd",
            "Real [[sync-conflicts-aabbccdd]] and invented [[made-up-ffffffff]].",
        ]);
        let out = run_ask(
            &db,
            &p,
            &crate::ask::AskScope::Account { account_id: a.into() },
            &ask("sync conflicts"),
            CancellationToken::new(),
            &[], &crate::privacy::ai_scope::AiScope::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.dropped_citations, 1);
        assert!(!out.markdown.contains("ffffffff"), "got {}", out.markdown);
        assert!(out.markdown.contains("sync-conflicts-aabbccdd"));
    }

    #[tokio::test]
    async fn cancelling_during_selection_never_reaches_the_answer_call() {
        let db = temp_db();
        let a = "acct@x";
        seed(&db, a, "u1", "Alpha", "aaa");
        let (mut p, seen) = FakeProvider::new(vec!["u1", "unused"]);
        p.cancel_on_call = Some(0);

        let err = run_ask(
            &db,
            &p,
            &crate::ask::AskScope::Account { account_id: a.into() },
            &ask("alpha"),
            CancellationToken::new(),
            &[], &crate::privacy::ai_scope::AiScope::default(),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, ExtractError::Cancelled));
        assert!(seen.lock().unwrap().is_empty(), "the answer call must not run");
    }

    #[tokio::test]
    async fn follow_up_turns_are_all_sent_to_the_selection_call() {
        let db = temp_db();
        let a = "acct@x";
        seed(&db, a, "u1", "Alpha", "aaa");
        let (p, seen) = FakeProvider::new(vec!["NONE", "n/a"]);
        let turns = vec![
            ChatTurn { role: ChatRole::User, content: "first question".into() },
            ChatTurn { role: ChatRole::Assistant, content: "first answer".into() },
            ChatTurn { role: ChatRole::User, content: "what about the other one?".into() },
        ];
        run_ask(&db, &p, &crate::ask::AskScope::Account { account_id: a.into() }, &turns, CancellationToken::new(), &[], &crate::privacy::ai_scope::AiScope::default())
            .await
            .unwrap();
        let calls = seen.lock().unwrap();
        assert!(calls[0].contains("first question"), "retrieval must see the whole conversation");
        assert!(calls[0].contains("what about the other one?"));
    }

    /// Guards against the uuid8-vs-uuid confusion in `run_ask`'s `fts_uuids`
    /// construction: `Candidate.uuid` is the full uuid, `Candidate.uuid8` is
    /// only the first 8 hex chars, but `build_answer_context` matches
    /// `fts_uuids.contains(uuid)` against the FULL uuid. If `run_ask` built
    /// `fts_uuids` from `c.uuid8` instead of `c.uuid`, the set would never
    /// match anything passed into `build_answer_context`, and FTS
    /// prioritisation would silently become a no-op.
    ///
    /// Setup: seed MAX_SELECTED_NOTES plain notes (no distinctive words) plus
    /// one note whose body contains a word unique to the question, so only
    /// that note lands in the pool's FTS-sourced tier (`from_fts = true`).
    /// The fake model "selects" all of them, with the FTS note listed LAST —
    /// one more than the selection cap. `build_answer_context` keeps
    /// FTS-flagged notes ahead of the rest on a stable sort, so if (and only
    /// if) the FTS flag survives from pool → run_ask → context intact on the
    /// full uuid, the last-listed note is the one that gets bubbled to the
    /// front and kept, not dropped. Built from uuid8 instead, the flag would
    /// never match and the FTS note — being last in selection order — would
    /// be the one the count-cap trims away.
    #[tokio::test]
    async fn fts_prioritisation_uses_full_uuid_not_uuid8() {
        let db = temp_db();
        let a = "acct@x";

        let plain_uuid8s: Vec<String> = (0..crate::ask::MAX_SELECTED_NOTES)
            .map(|i| {
                let uuid = format!("{i:08x}-0000-0000-0000-000000000000");
                seed(&db, a, &uuid, &format!("Plain {i}"), "nothing distinctive here");
                crate::db::uuid_short(&uuid)
            })
            .collect();

        let fts_uuid = "ffffff00-0000-0000-0000-000000000000";
        seed(&db, a, fts_uuid, "The Match", "reconciliationkeyword lives only here");
        let fts_uuid8 = crate::db::uuid_short(fts_uuid);

        // FTS note listed LAST: only survives if it is correctly prioritised.
        let mut selection = plain_uuid8s.join(" ");
        selection.push(' ');
        selection.push_str(&fts_uuid8);

        let (p, seen) = FakeProvider::new(vec![&selection, "answered"]);
        let out = run_ask(
            &db,
            &p,
            &crate::ask::AskScope::Account { account_id: a.into() },
            &ask("what about reconciliationkeyword?"),
            CancellationToken::new(),
            &[], &crate::privacy::ai_scope::AiScope::default(),
        )
        .await
        .unwrap();

        assert_eq!(out.notes_used, crate::ask::MAX_SELECTED_NOTES);
        assert!(out.trimmed);
        let calls = seen.lock().unwrap();
        assert!(
            calls[1].contains("reconciliationkeyword lives only here"),
            "the FTS-matched note must survive the trim and reach the answer call; got {}",
            calls[1]
        );
    }

    #[tokio::test]
    async fn hidden_folder_canary_never_reaches_the_provider() {
        let db = temp_db();
        // Both notes share the word "zebra", so FTS finds the hidden one too —
        // the pool, not the search, has to drop it. The question itself must
        // not contain "CANARY": it is sent, legitimately, on every call.
        crate::test_support::note("a", "c0ffee00-0000")
            .title("zebra CANARY_TITLE").body("zebra CANARY_BODY #CANARY_TAG")
            .label("Notes/CANARY_FOLDER").insert(&db);
        crate::test_support::note("a", "0babe000-0000")
            .title("zebra open note").body("zebra open body").label("Notes/Work").insert(&db);
        let ai = crate::privacy::ai_scope::AiScope::hiding("a", &["Notes/CANARY_FOLDER"]);
        // The model "selects" the hidden note's uuid8 as well: context must
        // still refuse to read it.
        let (p, seen) = FakeProvider::new(vec!["c0ffee00 0babe000", "answer"]);
        run_ask(&db, &p, &AskScope::Account { account_id: "a".into() }, &ask("what about zebra?"),
            CancellationToken::new(), &[], &ai).await.unwrap();
        for sent in seen.lock().unwrap().iter() {
            assert!(!sent.contains("CANARY"), "hidden data reached the provider: {sent}");
        }
    }

    #[tokio::test]
    async fn asking_about_a_hidden_folder_is_refused_before_any_call() {
        let db = temp_db();
        let ai = crate::privacy::ai_scope::AiScope::hiding("a", &["Notes/Private"]);
        let (p, seen) = FakeProvider::new(vec!["x", "y"]);
        let err = run_ask(&db, &p, &AskScope::Folder { account_id: "a".into(), label: "Notes/Private/Bank".into() },
            &ask("anything"), CancellationToken::new(), &[], &ai).await.unwrap_err();
        assert_eq!(err.to_string(), crate::privacy::ai_scope::HIDDEN_FOLDER);
        assert!(seen.lock().unwrap().is_empty());
    }

    // ── Note scope (spec 2026-10-23) ─────────────────────────────────────
    const FOCUS: &str = "aaaaaaaa-0000-0000-0000-000000000001";
    const REL: &str = "bbbbbbbb-0000-0000-0000-000000000002";

    fn note_scope(uuid: &str, links: bool) -> AskScope {
        AskScope::Note { account_id: "a".into(), uuid: uuid.into(), title: String::new(), links, tags: false, depth: 1 }
    }

    #[tokio::test]
    async fn a_lone_note_costs_one_call() {
        let db = temp_db();
        seed(&db, "a", FOCUS, "Focus title", "FOCUSBODY text");
        seed(&db, "a", REL, "Other", "OTHERBODY");
        let (p, seen) = FakeProvider::new(vec!["the answer"]);
        let r = run_ask(&db, &p, &note_scope(FOCUS, false), &ask("what is this?"),
            CancellationToken::new(), &[], &Default::default()).await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one LLM call for a lone note");
        assert!(seen[0].contains("FOCUSBODY"));
        assert!(!seen[0].contains("OTHERBODY"));
        assert_eq!(r.markdown, "the answer");
        assert_eq!((r.notes_in_scope, r.notes_considered, r.notes_used), (1, 1, 1));
        assert_eq!(r.read.len(), 1);
        assert_eq!(r.read[0].uuid, FOCUS);
    }

    #[tokio::test]
    async fn a_lone_long_note_reads_past_the_per_note_cap() {
        let db = temp_db();
        seed(&db, "a", FOCUS, "Long", &"y".repeat(50_000));
        let (p, seen) = FakeProvider::new(vec!["ok"]);
        let r = run_ask(&db, &p, &note_scope(FOCUS, false), &ask("q"),
            CancellationToken::new(), &[], &Default::default()).await.unwrap();
        let ys = seen.lock().unwrap()[0].matches('y').count();
        assert!(ys >= 50_000, "the whole 50k note fits under 120k; sent {ys}");
        assert!(!r.trimmed);
    }

    #[tokio::test]
    async fn a_ticked_box_with_no_neighbours_is_one_call() {
        let db = temp_db();
        seed(&db, "a", FOCUS, "Focus title", "no links here");
        let (p, seen) = FakeProvider::new(vec!["ok"]);
        run_ask(&db, &p, &note_scope(FOCUS, true), &ask("q"),
            CancellationToken::new(), &[], &Default::default()).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn related_scope_keeps_the_focus_note_when_nothing_is_selected() {
        let db = temp_db();
        seed(&db, "a", FOCUS, "Focus title", "FOCUSBODY see [[Other]]");
        seed(&db, "a", REL, "Other", "OTHERBODY");
        let (p, seen) = FakeProvider::new(vec!["NONE", "the answer"]);
        let r = run_ask(&db, &p, &note_scope(FOCUS, true), &ask("q"),
            CancellationToken::new(), &[], &Default::default()).await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].contains("FOCUSBODY"));
        assert!(!seen[1].contains("OTHERBODY"));
        assert_eq!(r.markdown, "the answer");
        assert_eq!(r.notes_used, 1);
        assert_eq!(r.notes_in_scope, 2);
    }

    #[tokio::test]
    async fn selection_call_names_the_focus_note_but_not_its_body() {
        let db = temp_db();
        seed(&db, "a", FOCUS, "Focus title", "FOCUSBODY see [[Other]]");
        seed(&db, "a", REL, "Other", "OTHERBODY");
        let (p, seen) = FakeProvider::new(vec!["bbbbbbbb", "answer"]);
        let r = run_ask(&db, &p, &note_scope(FOCUS, true), &ask("q"),
            CancellationToken::new(), &[], &Default::default()).await.unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen[0].contains("Focus title"), "selector is told the focus note");
        assert!(!seen[0].contains("FOCUSBODY"), "but not its body");
        assert!(seen[0].contains("bbbbbbbb"), "related note is in the catalog");
        assert!(!seen[0].contains("aaaaaaaa"), "focus note is not a catalog choice");
        assert!(seen[0].contains("at most 11"));
        assert!(seen[1].contains("FOCUSBODY") && seen[1].contains("OTHERBODY"));
        assert_eq!(r.notes_used, 2);
    }

    #[tokio::test]
    async fn focus_note_is_capped_at_60k_when_related_are_used() {
        let db = temp_db();
        seed(&db, "a", FOCUS, "Focus title", &format!("see [[Other]] {}", "z".repeat(100_000)));
        seed(&db, "a", REL, "Other", "OTHERBODY");
        let (p, seen) = FakeProvider::new(vec!["bbbbbbbb", "answer"]);
        let r = run_ask(&db, &p, &note_scope(FOCUS, true), &ask("q"),
            CancellationToken::new(), &[], &Default::default()).await.unwrap();
        let seen = seen.lock().unwrap();
        let zs = seen[1].matches('z').count();
        assert!(zs <= 60_000 && zs > 20_000, "focus note share was {zs}");
        assert!(seen[1].contains("OTHERBODY"));
        assert!(seen[1].contains("[truncated]"));
        assert_eq!(r.notes_used, 2);
    }

    #[tokio::test]
    async fn a_hidden_focus_note_is_refused_before_any_call() {
        let db = temp_db();
        crate::test_support::note("a", FOCUS).title("Secret").body("x").label("Notes/Private").insert(&db);
        let ai = crate::privacy::ai_scope::AiScope::hiding("a", &["Notes/Private"]);
        let (p, seen) = FakeProvider::new(vec!["x"]);
        let err = run_ask(&db, &p, &note_scope(FOCUS, false), &ask("q"),
            CancellationToken::new(), &[], &ai).await.unwrap_err();
        assert_eq!(err.to_string(), crate::privacy::ai_scope::HIDDEN_NOTE);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_missing_focus_note_is_refused() {
        let db = temp_db();
        let (p, seen) = FakeProvider::new(vec!["x"]);
        let err = run_ask(&db, &p, &note_scope(FOCUS, false), &ask("q"),
            CancellationToken::new(), &[], &Default::default()).await.unwrap_err();
        assert_eq!(err.to_string(), crate::ask::NOTE_GONE);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_rekeyed_focus_note_is_found() {
        let db = temp_db();
        seed(&db, "a", "old-local-uuid", "Fresh", "FRESHBODY");
        db.rekey_note_uuid("old-local-uuid", FOCUS, "a").unwrap();
        let (p, seen) = FakeProvider::new(vec!["ok"]);
        let r = run_ask(&db, &p, &note_scope("old-local-uuid", false), &ask("q"),
            CancellationToken::new(), &[], &Default::default()).await.unwrap();
        assert!(seen.lock().unwrap()[0].contains("FRESHBODY"));
        assert_eq!(r.read[0].uuid, FOCUS);
    }

    // Live pass 2026-10-24: a note-scoped answer ended every line with a chip
    // naming the note the user was already asking about. The focus note is
    // never cited; related notes still are.
    #[tokio::test]
    async fn a_lone_note_answer_carries_no_citation_of_itself() {
        let db = temp_db();
        seed(&db, "a", FOCUS, "Focus title", "FOCUSBODY");
        let slug = crate::db::note_slug("Focus title", FOCUS);
        let reply = format!("Point one. [[{slug}]]\n* item [[{slug}]]\n[[{slug}]]");
        let (p, seen) = FakeProvider::new(vec![reply.as_str()]);
        let r = run_ask(&db, &p, &note_scope(FOCUS, false), &ask("q"),
            CancellationToken::new(), &[], &Default::default()).await.unwrap();
        assert_eq!(r.markdown, "Point one.\n* item");
        assert!(r.cited.is_empty(), "{:?}", r.cited);
        assert_eq!(r.dropped_citations, 0);
        assert_eq!(r.read[0].uuid, FOCUS, "the note is still recorded as read");
        assert!(seen.lock().unwrap()[0].contains("Do not cite it"));
    }

    #[tokio::test]
    async fn related_answers_cite_related_notes_but_not_the_focus() {
        let db = temp_db();
        seed(&db, "a", FOCUS, "Focus title", "FOCUSBODY see [[Other]]");
        seed(&db, "a", REL, "Other", "OTHERBODY");
        let focus = crate::db::note_slug("Focus title", FOCUS);
        let other = crate::db::note_slug("Other", REL);
        let reply = format!("From the note [[{focus}]] and its neighbour [[{other}]].");
        let (p, _seen) = FakeProvider::new(vec!["bbbbbbbb", reply.as_str()]);
        let r = run_ask(&db, &p, &note_scope(FOCUS, true), &ask("q"),
            CancellationToken::new(), &[], &Default::default()).await.unwrap();
        assert_eq!(r.markdown, format!("From the note and its neighbour [[{other}]]."));
        assert_eq!(r.cited.iter().map(|c| c.uuid.as_str()).collect::<Vec<_>>(), vec![REL]);
    }
}
