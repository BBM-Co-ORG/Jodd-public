//! Jodd-managed AI boundary. This does not govern external MCP clients or
//! establish provenance for arbitrary text pasted by a user.
use super::provider::*;
use crate::accounts::{Account, AccountStatus, LlmProviderKind};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use tokio_util::sync::CancellationToken;

pub fn account_allowed(account: &Account) -> bool {
    account.status == AccountStatus::Active
        && account.blocked_reason.is_none()
        && !account.pending_removal
        && account.llm.provider != LlmProviderKind::Disabled
        && account.llm.data_allowed.unwrap_or(true)
}

/// Saving an unrelated provider preference must not lift an existing denial.
/// In particular, materialize the legacy Disabled opt-out before leaving it.
pub fn preserve_permission(
    previous: &crate::accounts::LlmConfig,
    next: &mut crate::accounts::LlmConfig,
) {
    if next.provider == LlmProviderKind::Disabled {
        next.data_allowed = Some(false);
    } else if next.data_allowed.is_none() {
        next.data_allowed = Some(
            previous.provider != LlmProviderKind::Disabled && previous.data_allowed.unwrap_or(true),
        );
    }
}

pub fn require_account(accounts: &[Account], id: &str) -> Result<(), ExtractError> {
    if accounts.iter().any(|a| a.id == id && account_allowed(a)) {
        Ok(())
    } else {
        Err(ExtractError::NotConfigured("AI data access is disabled or this account is unavailable. Review its AI data permission in Account Settings.".into()))
    }
}

/// Command admission seam: run the policy before provider construction (and
/// therefore before credentials, retrieval, or dispatch). Tests inject only a
/// recording provider here; production injects the existing resolver.
pub fn build_account_provider(
    accounts: &[Account],
    id: &str,
    build: impl FnOnce(&Account) -> Result<Box<dyn LlmProvider>, ExtractError>,
) -> Result<Box<dyn LlmProvider>, ExtractError> {
    require_account(accounts, id)?;
    build(accounts.iter().find(|a| a.id == id).unwrap())
}

#[derive(Default)]
pub struct Runtime {
    pub meeting_drafts: Mutex<HashMap<String, super::meeting::Draft>>,
    pub gate: Mutex<()>,
    pub results: Mutex<HashMap<String, (String, serde_json::Value)>>,
    pub revision: AtomicU64,
    pub sessions: Mutex<HashMap<String, Session>>,
}
#[derive(Clone)]
pub struct Session {
    pub stamp: serde_json::Value,
    pub scope: crate::ask::AskScope,
    pub turns: Vec<ChatTurn>,
    pub cancel: CancellationToken,
    pub busy: bool,
}
impl Session {
    pub fn begin_turn(
        &mut self,
        stamp: &serde_json::Value,
        question: &str,
    ) -> Result<Vec<ChatTurn>, String> {
        if &self.stamp != stamp || self.cancel.is_cancelled() {
            self.turns.clear();
            self.cancel.cancel();
            return Err("AI permission or provider changed. Start a new conversation.".into());
        }
        if self.busy {
            return Err("A question is already running.".into());
        }
        if self.turns.len() >= 100 {
            return Err("Conversation limit reached. Start again.".into());
        }
        self.busy = true;
        let mut turns = self.turns.clone();
        turns.push(ChatTurn {
            role: ChatRole::User,
            content: question.into(),
        });
        Ok(turns)
    }
    pub fn finish_turn(
        &mut self,
        stamp: &serde_json::Value,
        question: String,
        answer: Option<&str>,
        cancelled: bool,
    ) -> Result<(), String> {
        self.busy = false;
        if &self.stamp != stamp || self.cancel.is_cancelled() || cancelled {
            self.turns.clear();
            self.cancel.cancel();
            return Err("AI request cancelled or settings changed. Start again.".into());
        }
        if let Some(answer) = answer {
            self.turns.push(ChatTurn {
                role: ChatRole::User,
                content: question,
            });
            self.turns.push(ChatTurn {
                role: ChatRole::Assistant,
                content: answer.into(),
            });
        }
        Ok(())
    }
}
impl Runtime {
    pub fn issue_result(&self, account_id: &str, stamp: serde_json::Value) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let mut results = self.results.lock().unwrap();
        if results.len() >= 128 {
            results.clear();
        }
        results.insert(id.clone(), (account_id.into(), stamp));
        id
    }
    pub fn result_valid(&self, id: &str, account_id: &str, stamp: serde_json::Value) -> bool {
        self.results.lock().unwrap().get(id) == Some(&(account_id.into(), stamp))
    }
    pub fn invalidate(&self) {
        self.meeting_drafts.lock().unwrap().clear();
        self.results.lock().unwrap().clear();
        self.revision.fetch_add(1, Ordering::SeqCst);
        for (_, session) in self.sessions.lock().unwrap().drain() {
            session.cancel.cancel();
        }
    }
}

/// All built-in provider methods pass this check before dispatch and after
/// completion. A cancelled/changed run cannot produce a fallback write.
/// The monitor also stops a provider waiting on I/O after account changes.
pub struct CheckedProvider<'a> {
    pub cancel: Mutex<Option<CancellationToken>>,
    pub gate: &'a Mutex<()>,
    pub inner: Box<dyn LlmProvider>,
    pub valid: Box<dyn Fn() -> bool + Send + Sync + 'a>,
}
impl CheckedProvider<'_> {
    async fn call<T>(
        &self,
        cancel: CancellationToken,
        future: impl std::future::Future<Output = Result<T, ExtractError>>,
    ) -> Result<T, ExtractError> {
        self.check()?;
        super::receipts::check("permission_before_dispatch");
        *self.cancel.lock().unwrap() = Some(cancel.clone());
        if cancel.is_cancelled() {
            return Err(ExtractError::Cancelled);
        }
        let monitor = async {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                if !(self.valid)() {
                    cancel.cancel();
                    return;
                }
            }
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ExtractError::Cancelled),
            _ = monitor => Err(ExtractError::Cancelled),
            result = future => result,
        };
        self.check()?;
        if cancel.is_cancelled() {
            return Err(ExtractError::Cancelled);
        }
        super::receipts::check(if result.is_ok() { "provider_output_accepted" } else { "provider_output_failed" });
        result
    }
}
#[async_trait::async_trait]
impl LlmProvider for CheckedProvider<'_> {
    fn mutation_guard(&self) -> Result<Option<std::sync::MutexGuard<'_, ()>>, ExtractError> {
        let guard = self.gate.lock().unwrap();
        self.check()?;
        super::receipts::check("permission_before_apply");
        Ok(Some(guard))
    }
    fn check(&self) -> Result<(), ExtractError> {
        if (self.valid)()
            && !self
                .cancel
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        {
            Ok(())
        } else {
            super::receipts::check("permission_or_cancellation_refused");
            Err(ExtractError::Cancelled)
        }
    }
    async fn extract(
        &self,
        s: &str,
        tags: &[String],
        c: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        self.call(c.clone(), self.inner.extract(s, tags, c)).await
    }
    async fn run_workflow(
        &self,
        w: WorkflowKind,
        s: &str,
        tags: &[String],
        c: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        self.call(c.clone(), self.inner.run_workflow(w, s, tags, c))
            .await
    }
    async fn suggest_links(
        &self,
        s: &str,
        candidates: &[CandidateSummary],
        c: CancellationToken,
    ) -> Result<LinkSuggestionsEnvelope, ExtractError> {
        self.call(c.clone(), self.inner.suggest_links(s, candidates, c))
            .await
    }
    async fn suggest_folder(
        &self,
        s: &str,
        folders: &[String],
        c: CancellationToken,
    ) -> Result<FolderSuggestionEnvelope, ExtractError> {
        self.call(c.clone(), self.inner.suggest_folder(s, folders, c))
            .await
    }
    async fn synthesize(
        &self,
        d: &[SourceDigest],
        context: &str,
        c: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        self.call(c.clone(), self.inner.synthesize(d, context, c))
            .await
    }
    async fn chat(
        &self,
        system: &str,
        turns: &[ChatTurn],
        c: CancellationToken,
    ) -> Result<String, ExtractError> {
        self.call(c.clone(), self.inner.chat(system, turns, c))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ask::{run::run_ask, AskScope},
        test_support::{note, temp_db},
    };
    use std::sync::{atomic::AtomicBool, Arc};

    fn account(id: &str, provider: &str, permission: Option<bool>) -> Account {
        serde_json::from_value(serde_json::json!({
            "id": id, "email": "synthetic@example.test", "added_at": "2026-09-19",
            "llm": { "provider": provider, "data_allowed": permission }
        }))
        .unwrap()
    }
    fn envelope() -> ExtractEnvelope {
        serde_json::from_value(serde_json::json!({"lessons_markdown": "Synthetic answer"})).unwrap()
    }
    struct Recording {
        seen: Arc<Mutex<Vec<String>>>,
        valid: Arc<AtomicBool>,
        revoke_on_call: bool,
    }
    impl Recording {
        fn record(&self, text: String) {
            self.seen.lock().unwrap().push(text);
            if self.revoke_on_call {
                self.valid.store(false, Ordering::SeqCst);
            }
        }
    }
    #[async_trait::async_trait]
    impl LlmProvider for Recording {
        async fn extract(
            &self,
            s: &str,
            t: &[String],
            _: CancellationToken,
        ) -> Result<ExtractEnvelope, ExtractError> {
            self.record(format!("{s} {t:?}"));
            Ok(envelope())
        }
        async fn run_workflow(
            &self,
            _: WorkflowKind,
            s: &str,
            t: &[String],
            _: CancellationToken,
        ) -> Result<ExtractEnvelope, ExtractError> {
            self.record(format!("{s} {t:?}"));
            Ok(envelope())
        }
        async fn suggest_links(
            &self,
            s: &str,
            c: &[CandidateSummary],
            _: CancellationToken,
        ) -> Result<LinkSuggestionsEnvelope, ExtractError> {
            self.record(format!("{s} {c:?}"));
            Ok(LinkSuggestionsEnvelope {
                suggestions: vec![],
            })
        }
        async fn suggest_folder(
            &self,
            s: &str,
            f: &[String],
            _: CancellationToken,
        ) -> Result<FolderSuggestionEnvelope, ExtractError> {
            self.record(format!("{s} {f:?}"));
            Ok(FolderSuggestionEnvelope {
                folder: None,
                reason: None,
            })
        }
        async fn synthesize(
            &self,
            d: &[SourceDigest],
            c: &str,
            _: CancellationToken,
        ) -> Result<ExtractEnvelope, ExtractError> {
            self.record(format!("{d:?} {c}"));
            Ok(envelope())
        }
        async fn chat(
            &self,
            s: &str,
            t: &[ChatTurn],
            _: CancellationToken,
        ) -> Result<String, ExtractError> {
            self.record(format!("{s} {t:?}"));
            Ok("aabbccdd".into())
        }
    }
    fn checked(
        gate: &Mutex<()>,
        valid: Arc<AtomicBool>,
        seen: Arc<Mutex<Vec<String>>>,
        revoke: bool,
    ) -> CheckedProvider<'_> {
        CheckedProvider {
            cancel: Mutex::new(None),
            gate,
            inner: Box::new(Recording {
                seen,
                valid: valid.clone(),
                revoke_on_call: revoke,
            }),
            valid: Box::new(move || valid.load(Ordering::SeqCst)),
        }
    }

    #[test]
    fn legacy_disabled_and_explicit_denial_cannot_be_overridden_by_provider_selection() {
        assert!(!account_allowed(&account("a", "disabled", None)));
        assert!(!account_allowed(&account("a", "disabled", Some(true))));
        assert!(!account_allowed(&account("a", "http", Some(false))));
        assert!(
            account_allowed(&account("a", "none", None)),
            "legacy Ask eligibility preserved"
        );
        let mut a = account("a", "http", Some(true));
        a.status = AccountStatus::Draining;
        assert!(!account_allowed(&a));
        a.status = AccountStatus::Inactive;
        assert!(!account_allowed(&a));
        assert!(require_account(&[a], "unknown").is_err());
        assert!(serde_json::from_value::<Account>(serde_json::json!({
            "id": "a", "email": "x", "added_at": "x", "llm": {"data_allowed": "yes"}
        }))
        .is_err());
    }

    #[tokio::test]
    async fn ask_all_and_explicit_scopes_never_send_denied_or_unknown_data() {
        let db = temp_db();
        note("allowed", "aabbccdd-0000")
            .title("Allowed title")
            .body("Allowed body #allowedtag")
            .label("Notes/Allowed")
            .insert(&db);
        for id in ["denied", "orphaned", "inactive"] {
            note(id, "deadbeef-0000")
                .title("FORBIDDEN_TITLE")
                .body("FORBIDDEN_BODY #FORBIDDEN_TAG")
                .label("Notes/FORBIDDEN_FOLDER")
                .insert(&db);
        }
        let accounts = [
            account("allowed", "none", None),
            account("denied", "disabled", None),
        ];
        let allowed = accounts
            .iter()
            .filter(|a| account_allowed(a))
            .map(|a| a.id.clone())
            .collect::<Vec<_>>();
        let excluded = db.ai_excluded_accounts(&allowed).unwrap();
        for scope in [
            AskScope::AllAccounts,
            AskScope::Account {
                account_id: "allowed".into(),
            },
            AskScope::Folder {
                account_id: "allowed".into(),
                label: "Notes/Allowed".into(),
            },
        ] {
            let gate = Mutex::new(());
            let seen = Arc::new(Mutex::new(vec![]));
            let p = checked(&gate, Arc::new(AtomicBool::new(true)), seen.clone(), false);
            let result = run_ask(
                &db,
                &p,
                &scope,
                &[ChatTurn {
                    role: ChatRole::User,
                    content: "anything".into(),
                }],
                CancellationToken::new(),
                &excluded,
            )
            .await
            .unwrap();
            assert_eq!(result.notes_in_scope, 1);
            let calls = seen.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert!(calls[0].contains("Allowed title"));
            assert!(calls[1].contains("Allowed body"));
            assert!(calls.iter().all(|s| !s.contains("FORBIDDEN")));
        }
    }

    #[tokio::test]
    async fn denied_direct_methods_and_enrichment_dispatch_nothing() {
        let db = temp_db();
        note("denied", "deadbeef")
            .title("FORBIDDEN_TITLE")
            .body("FORBIDDEN_BODY #FORBIDDEN_TAG")
            .insert(&db);
        db.create_folder_local_new("denied", "Notes/FORBIDDEN_FOLDER")
            .unwrap();
        let gate = Mutex::new(());
        let seen = Arc::new(Mutex::new(vec![]));
        let p = checked(&gate, Arc::new(AtomicBool::new(false)), seen.clone(), false);
        let c = CancellationToken::new();
        assert!(p
            .extract("FORBIDDEN_BODY", &["FORBIDDEN_TAG".into()], c.clone())
            .await
            .is_err());
        for w in [
            WorkflowKind::Summarize,
            WorkflowKind::ActionItems,
            WorkflowKind::ExpandBullets,
        ] {
            assert!(p
                .run_workflow(w, "FORBIDDEN_BODY", &[], c.clone())
                .await
                .is_err());
        }
        assert!(p
            .suggest_links("FORBIDDEN_TITLE", &[], c.clone())
            .await
            .is_err());
        assert!(p
            .suggest_folder("FORBIDDEN_BODY", &["FORBIDDEN_FOLDER".into()], c.clone())
            .await
            .is_err());
        assert!(p
            .synthesize(&[], "FORBIDDEN_BODY", c.clone())
            .await
            .is_err());
        assert!(p
            .chat(
                "test",
                &[ChatTurn {
                    role: ChatRole::Assistant,
                    content: "FORBIDDEN_HISTORY".into()
                }],
                c.clone()
            )
            .await
            .is_err());
        assert!(
            crate::llm::filing::suggest_folder(&p, &db, "denied", "deadbeef", c.clone())
                .await
                .is_err()
        );
        assert!(crate::llm::autolink::suggest_links(
            &p,
            &db,
            "denied",
            None,
            "FORBIDDEN_TITLE",
            "new",
            "FORBIDDEN_BODY",
            c
        )
        .await
        .is_err());
        assert!(seen.lock().unwrap().is_empty());
        assert!(p.mutation_guard().is_err());
    }

    #[tokio::test]
    async fn permission_change_during_selection_prevents_answer_and_discards_late_result() {
        let db = temp_db();
        note("allowed", "aabbccdd")
            .title("Allowed title")
            .body("body")
            .insert(&db);
        let gate = Mutex::new(());
        let seen = Arc::new(Mutex::new(vec![]));
        let valid = Arc::new(AtomicBool::new(true));
        let p = checked(&gate, valid, seen.clone(), true);
        let result = run_ask(
            &db,
            &p,
            &AskScope::AllAccounts,
            &[ChatTurn {
                role: ChatRole::User,
                content: "anything".into(),
            }],
            CancellationToken::new(),
            &[],
        )
        .await;
        assert!(matches!(result, Err(ExtractError::Cancelled)));
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(p.mutation_guard().is_err());
    }

    #[tokio::test]
    async fn cancelled_before_dispatch_spends_no_call_even_if_provider_ignores_cancel() {
        let gate = Mutex::new(());
        let seen = Arc::new(Mutex::new(vec![]));
        let p = checked(&gate, Arc::new(AtomicBool::new(true)), seen.clone(), false);
        let c = CancellationToken::new();
        c.cancel();
        assert!(matches!(
            p.extract("text", &[], c).await,
            Err(ExtractError::Cancelled)
        ));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn settings_revision_clears_history_and_results_even_when_settings_change_back() {
        let runtime = Runtime::default();
        let cancel = CancellationToken::new();
        runtime.sessions.lock().unwrap().insert(
            "old".into(),
            Session {
                stamp: serde_json::json!(0),
                scope: AskScope::AllAccounts,
                turns: vec![ChatTurn {
                    role: ChatRole::Assistant,
                    content: "FORBIDDEN_HISTORY".into(),
                }],
                cancel: cancel.clone(),
                busy: true,
            },
        );
        runtime
            .results
            .lock()
            .unwrap()
            .insert("result".into(), ("a".into(), serde_json::json!(0)));
        runtime.invalidate();
        runtime.invalidate();
        assert_eq!(runtime.revision.load(Ordering::SeqCst), 2);
        assert!(runtime.sessions.lock().unwrap().is_empty());
        assert!(runtime.results.lock().unwrap().is_empty());
        assert!(cancel.is_cancelled());
    }
    #[tokio::test]
    async fn retained_history_is_only_sent_under_its_original_eligibility() {
        let stamp = serde_json::json!({"provider": "one", "permission_revision": 1});
        let mut session = Session {
            stamp: stamp.clone(),
            scope: AskScope::AllAccounts,
            turns: vec![],
            cancel: CancellationToken::new(),
            busy: false,
        };
        session.begin_turn(&stamp, "first").unwrap();
        session
            .finish_turn(&stamp, "first".into(), Some("NOTE_DERIVED_HISTORY"), false)
            .unwrap();
        let gate = Mutex::new(());
        let seen = Arc::new(Mutex::new(vec![]));
        let p = checked(&gate, Arc::new(AtomicBool::new(true)), seen.clone(), false);
        let turns = session.begin_turn(&stamp, "follow up").unwrap();
        p.chat("system", &turns, CancellationToken::new())
            .await
            .unwrap();
        assert!(seen.lock().unwrap()[0].contains("NOTE_DERIVED_HISTORY"));
        session
            .finish_turn(&stamp, "follow up".into(), None, false)
            .unwrap();
        for changed in [
            serde_json::json!({"provider": "two"}),
            serde_json::json!({"permission_revision": 2}),
        ] {
            assert!(session.begin_turn(&changed, "new question").is_err());
            assert!(session.turns.is_empty());
        }
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "expired history never reaches a provider"
        );
        assert!(session
            .finish_turn(&stamp, "old question".into(), Some("LATE_ANSWER"), false)
            .is_err());
        assert!(session.turns.is_empty());
    }

    #[test]
    fn deferred_result_cannot_cross_accounts_or_policy_revisions() {
        let runtime = Runtime::default();
        let stamp = serde_json::json!(["provider", 1]);
        let id = runtime.issue_result("a", stamp.clone());
        assert!(runtime.result_valid(&id, "a", stamp.clone()));
        assert!(!runtime.result_valid(&id, "b", stamp.clone()));
        assert!(!runtime.result_valid(&id, "a", serde_json::json!(["provider", 2])));
        assert!(!runtime.result_valid("forged", "a", stamp.clone()));
        runtime.invalidate();
        assert!(!runtime.result_valid(&id, "a", stamp));
    }

    #[tokio::test]
    async fn ingest_permission_change_discards_digest_and_never_writes_a_note_or_folder() {
        use crate::ingest::run::*;
        use crate::ingest::{FetchStatus, FetchedSource, SourceFetcher, SourceKind};
        struct NeverFetch;
        #[async_trait::async_trait]
        impl SourceFetcher for NeverFetch {
            async fn fetch(&self, _: &str, _: SourceKind, _: CancellationToken) -> FetchedSource {
                panic!("stored fixture must not fetch");
            }
        }
        let db = temp_db();
        let gate = Mutex::new(());
        let seen = Arc::new(Mutex::new(vec![]));
        let p = checked(&gate, Arc::new(AtomicBool::new(true)), seen.clone(), true);
        let sources = (0..2)
            .map(|i| FetchedSource {
                url: format!("https://example.test/{i}"),
                kind: SourceKind::Web,
                title: Some("Synthetic source".into()),
                text: "Synthetic content".into(),
                status: FetchStatus::Ok,
            })
            .collect();
        let request = IngestRequest {
            account_id: "a".into(),
            backend_kind: crate::accounts::BackendKind::LocalFs,
            can_create_folders: true,
            context: "context".into(),
            title_override: None,
            destination: IngestDestination::Resolve,
            map_cap: 1000,
        };
        let result = ingest_to_note(
            &db,
            &p,
            &NeverFetch,
            IngestInput::Stored(sources),
            &request,
            CancellationToken::new(),
            &|_| {},
        )
        .await;
        assert!(matches!(result, Err(IngestError::Cancelled)));
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "no second map or reduce after revocation"
        );
        assert!(db.list_notes("a").unwrap().is_empty());
        assert!(db.list_folders("a").unwrap().is_empty());
    }

    #[test]
    fn saving_provider_preferences_preserves_denial_without_touching_credentials() {
        let previous = crate::accounts::LlmConfig {
            provider: LlmProviderKind::Disabled,
            ..Default::default()
        };
        let mut next = crate::accounts::LlmConfig {
            provider: LlmProviderKind::Http,
            http_api_key_keychain: Some("unchanged-reference".into()),
            ..Default::default()
        };
        preserve_permission(&previous, &mut next);
        assert_eq!(next.data_allowed, Some(false));
        assert_eq!(
            next.http_api_key_keychain.as_deref(),
            Some("unchanged-reference")
        );
        next.data_allowed = Some(true); // explicit user opt-in, separate from route
        preserve_permission(&previous, &mut next);
        assert_eq!(next.data_allowed, Some(true));
        next.provider = LlmProviderKind::Disabled;
        preserve_permission(&previous, &mut next);
        assert_eq!(next.data_allowed, Some(false));
    }
    #[tokio::test]
    async fn direct_command_admission_denies_before_provider_construction() {
        let seen = Arc::new(Mutex::new(vec![]));
        for a in [
            account("denied", "disabled", None),
            account("denied", "http", Some(false)),
        ] {
            let provider = build_account_provider(&[a], "denied", |_| {
                Ok(Box::new(Recording {
                    seen: seen.clone(),
                    valid: Arc::new(AtomicBool::new(true)),
                    revoke_on_call: false,
                }))
            });
            match provider {
                Ok(p) => {
                    let _ = p
                        .extract(
                            "FORBIDDEN_BODY",
                            &["FORBIDDEN_TAG".into()],
                            CancellationToken::new(),
                        )
                        .await;
                }
                Err(ExtractError::NotConfigured(_)) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert!(seen.lock().unwrap().is_empty());
        assert!(build_account_provider(&[], "unknown", |_| panic!(
            "must refuse before construction"
        ))
        .is_err());
    }

    #[tokio::test]
    async fn permission_monitor_cancels_waiting_io_without_waiting_for_a_provider_response() {
        let gate = Mutex::new(());
        let valid = Arc::new(AtomicBool::new(true));
        let p = checked(&gate, valid.clone(), Arc::new(Mutex::new(vec![])), false);
        let started = tokio::sync::Notify::new();
        let cancel = CancellationToken::new();
        let pending = p.call(cancel.clone(), async {
            started.notify_one();
            std::future::pending::<Result<(), ExtractError>>().await
        });
        let revoke = async {
            started.notified().await;
            valid.store(false, Ordering::SeqCst);
        };
        let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(pending, revoke)
        })
        .await
        .unwrap();
        assert!(matches!(result, Err(ExtractError::Cancelled)));
        assert!(cancel.is_cancelled());
    }
    #[tokio::test]
    async fn cancellation_after_provider_completion_still_refuses_result_application() {
        let gate = Mutex::new(());
        let p = checked(
            &gate,
            Arc::new(AtomicBool::new(true)),
            Arc::new(Mutex::new(vec![])),
            false,
        );
        let cancel = CancellationToken::new();
        p.extract("synthetic", &[], cancel.clone()).await.unwrap();
        cancel.cancel();
        assert!(p.mutation_guard().is_err());
    }
}
