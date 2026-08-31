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
) -> Result<AskAnswer, ExtractError> {
    let notes_in_scope = db
        .count_notes_in_scope(scope.account_id(), scope.label(), exclude_accounts)
        .map_err(|e| ExtractError::Transport(format!("count_notes_in_scope: {e}")))?;

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
    let candidates = pool::build_candidate_pool(db, scope, question, exclude_accounts)
        .map_err(|e| ExtractError::Transport(format!("build_candidate_pool: {e}")))?;
    let notes_considered = candidates.len();

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

    let ctx = context::build_answer_context(db, &selected, &fts_uuids)
        .map_err(|e| ExtractError::Transport(format!("build_answer_context: {e}")))?;

    let mut answer_turns = turns.to_vec();
    answer_turns.push(ChatTurn {
        role: ChatRole::User,
        content: format!("NOTES:\n{}", context::render_context(&ctx)),
    });
    let raw = provider
        .chat(prompt::ANSWER_SYSTEM_PROMPT, &answer_turns, cancel)
        .await?;

    let (markdown, cited, dropped_citations) = resolve_citations(&raw, &ctx);

    Ok(AskAnswer {
        markdown,
        cited,
        notes_in_scope,
        notes_considered,
        notes_used: ctx.notes.len(),
        trimmed: ctx.trimmed,
        dropped_citations,
    })
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
        CandidateSummary, ChatRole, ChatTurn, ExtractEnvelope, ExtractError, LinkSuggestionsEnvelope,
        LlmProvider,
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
        async fn extract(&self, _s: &str, _c: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("Ask Jodd never calls extract")
        }
        async fn suggest_links(
            &self,
            _s: &str,
            _c: &[CandidateSummary],
            _t: CancellationToken,
        ) -> Result<LinkSuggestionsEnvelope, ExtractError> {
            unreachable!("Ask Jodd never calls suggest_links")
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
        run_ask(&db, &p, &crate::ask::AskScope::Account { account_id: a.into() }, &turns, CancellationToken::new(), &[])
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
            &[],
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
}
