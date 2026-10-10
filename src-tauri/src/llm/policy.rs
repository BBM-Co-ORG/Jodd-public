//! Jodd-managed AI boundary. This does not govern external MCP clients or
//! establish provenance for arbitrary text pasted by a user.
use super::provider::*;
use crate::privacy::{vault::{Known, Vault}, wire};
use crate::accounts::{Account, AccountStatus, LlmProviderKind};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use tokio_util::sync::CancellationToken;

/// Shown verbatim; `src/lib/aiConsent.ts` matches on it to offer a one-click
/// allow (pinned by `the_frontend_recognises_the_consent_refusal`). No "yet":
/// it is also shown to an account the user denied (see `needs_consent`).
pub const AI_CONSENT_NEEDED: &str = "AI data access is not allowed for this account. Allow it to continue.";

const ALLOW_REFUSED_DISABLED: &str = "AI is turned off for this account. Choose a provider in Account Settings first.";

fn usable(account: &Account) -> bool {
    account.status == AccountStatus::Active && account.blocked_reason.is_none() && !account.pending_removal
}

/// Spec 2026-10-08 §4.5: absent means "not decided", which denies. Legacy
/// accounts were given an explicit permission by the accounts.json migration.
pub fn account_allowed(account: &Account) -> bool {
    usable(account) && account.llm.provider != LlmProviderKind::Disabled && account.llm.data_allowed == Some(true)
}

/// A usable account whose only obstacle is the data permission. Covers an
/// explicit `Some(false)` too: after one save a new account is written as
/// `Some(false)` and is indistinguishable from one the user denied.
pub fn needs_consent(account: &Account) -> bool {
    usable(account) && account.llm.provider != LlmProviderKind::Disabled && account.llm.data_allowed != Some(true)
}

/// Exactly the refusal `require_account` gives when `needs_consent` holds.
/// Organize uses it to say why the AI step did not run.
pub fn is_consent_refusal(e: &ExtractError) -> bool {
    matches!(e, ExtractError::NotConfigured(m) if m == AI_CONSENT_NEEDED)
}

/// What `allow_ai_for_account` will do, decided before it cancels anything:
/// `invalidate_ai` ends every account's in-flight AI work, so an Allow that is
/// refused or changes nothing must not reach it.
#[derive(Debug, PartialEq, Eq)]
pub enum AllowPlan {
    /// Already `Some(true)`: nothing to change, nothing to invalidate.
    AlreadyAllowed,
    /// Invalidate, then set `Some(true)` and save.
    Allow,
}

pub fn plan_allow(accounts: &[Account], id: &str) -> Result<AllowPlan, String> {
    let a = accounts.iter().find(|a| a.id == id).ok_or_else(|| format!("account not found: {id}"))?;
    if a.llm.provider == LlmProviderKind::Disabled {
        return Err(ALLOW_REFUSED_DISABLED.into());
    }
    Ok(if a.llm.data_allowed == Some(true) { AllowPlan::AlreadyAllowed } else { AllowPlan::Allow })
}

/// The one-click consent. Never lifts an explicit provider opt-out.
pub fn allow_ai(account: &mut Account) -> Result<(), String> {
    if account.llm.provider == LlmProviderKind::Disabled {
        return Err(ALLOW_REFUSED_DISABLED.into());
    }
    account.llm.data_allowed = Some(true);
    Ok(())
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
            previous.provider != LlmProviderKind::Disabled && previous.data_allowed == Some(true),
        );
    }
}

pub fn require_account(accounts: &[Account], id: &str) -> Result<(), ExtractError> {
    match accounts.iter().find(|a| a.id == id) {
        Some(a) if account_allowed(a) => Ok(()),
        Some(a) if needs_consent(a) => Err(ExtractError::NotConfigured(AI_CONSENT_NEEDED.into())),
        _ => Err(ExtractError::NotConfigured("AI data access is disabled or this account is unavailable. Review its AI data permission in Account Settings.".into())),
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
    pub results: Mutex<HashMap<String, (String, serde_json::Value, crate::provenance::Actor)>>,
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
    /// Values masked in earlier turns (final review C2), shared with each
    /// turn's `CheckedProvider`. Cleared with `turns`.
    pub known: Arc<Mutex<Known>>,
}
impl Session {
    /// History and the values masked in it go together: a seed outliving its
    /// history would only mask more, but nothing should outlive a reset.
    fn reset(&mut self) {
        self.turns.clear();
        self.known.lock().unwrap().clear();
        self.cancel.cancel();
    }
    pub fn begin_turn(
        &mut self,
        stamp: &serde_json::Value,
        question: &str,
    ) -> Result<Vec<ChatTurn>, String> {
        if &self.stamp != stamp || self.cancel.is_cancelled() {
            self.reset();
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
            self.reset();
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
    pub fn issue_result(&self, account_id: &str, stamp: serde_json::Value, actor: crate::provenance::Actor) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let mut results = self.results.lock().unwrap();
        if results.len() >= 128 {
            results.clear();
        }
        results.insert(id.clone(), (account_id.into(), stamp, actor));
        id
    }
    pub fn result_valid(&self, id: &str, account_id: &str, stamp: serde_json::Value) -> bool {
        self.results.lock().unwrap().get(id).is_some_and(|(a, s, _)| a == account_id && *s == stamp)
    }
    /// Who wrote the content behind a live AI result, if the result is live.
    pub fn result_actor(&self, id: &str) -> Option<crate::provenance::Actor> {
        self.results.lock().unwrap().get(id).map(|(_, _, a)| a.clone())
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
    /// Values detected by earlier calls through this provider (or this Ask
    /// conversation): each call masks them too, and adds what it detected.
    pub known: Arc<Mutex<Known>>,
}
impl CheckedProvider<'_> {
    /// This call's vault: reserving + detecting over every input, seeded with
    /// what earlier calls detected, and adding what this one detected — before
    /// dispatch, so a failed call still teaches the next one.
    fn vault<'a>(&self, inputs: impl IntoIterator<Item = &'a str>) -> Vault {
        let mut v = Vault::reserving(inputs);
        let mut known = self.known.lock().unwrap();
        v.seed(&known);
        v.remember(&mut known);
        v
    }
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
    // Spec 2026-10-08 §4.3: every text method masks its user-text inputs with
    // a fresh per-call vault, dispatches through `call`, unmasks the result,
    // THEN records content-free counts (`unresolved` is only known after
    // unmasking). Errors pass through still masked, on purpose — they travel
    // to logs and the UI.
    async fn extract(
        &self,
        s: &str,
        tags: &[String],
        c: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let mut v = self.vault(std::iter::once(s).chain(tags.iter().map(String::as_str)));
        let (s, tags) = (v.mask(s), wire::mask_all(&mut v, tags));
        let out = self.call(c.clone(), self.inner.extract(&s, &tags, c)).await;
        let out = out.map(|e| wire::unmask_envelope(&mut v, e));
        wire::record_metrics(&v);
        out
    }
    async fn run_workflow(
        &self,
        w: WorkflowKind,
        s: &str,
        tags: &[String],
        c: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let mut v = self.vault(std::iter::once(s).chain(tags.iter().map(String::as_str)));
        let (s, tags) = (v.mask(s), wire::mask_all(&mut v, tags));
        let out = self.call(c.clone(), self.inner.run_workflow(w, &s, &tags, c)).await;
        let out = out.map(|e| wire::unmask_envelope(&mut v, e));
        wire::record_metrics(&v);
        out
    }
    async fn suggest_links(
        &self,
        s: &str,
        candidates: &[CandidateSummary],
        c: CancellationToken,
    ) -> Result<LinkSuggestionsEnvelope, ExtractError> {
        let mut v = self.vault(std::iter::once(s).chain(wire::candidate_texts(candidates)));
        let (s, candidates) = (v.mask(s), wire::mask_candidates(&mut v, candidates));
        let out = self.call(c.clone(), self.inner.suggest_links(&s, &candidates, c)).await;
        let out = out.map(|e| wire::unmask_links(&mut v, e));
        wire::record_metrics(&v);
        out
    }
    async fn suggest_folder(
        &self,
        s: &str,
        folders: &[String],
        c: CancellationToken,
    ) -> Result<FolderSuggestionEnvelope, ExtractError> {
        let mut v = self.vault(std::iter::once(s).chain(folders.iter().map(String::as_str)));
        let (s, folders) = (v.mask(s), wire::mask_all(&mut v, folders));
        let out = self.call(c.clone(), self.inner.suggest_folder(&s, &folders, c)).await;
        let out = out.map(|e| wire::unmask_folder(&mut v, e));
        wire::record_metrics(&v);
        out
    }
    async fn synthesize(
        &self,
        d: &[SourceDigest],
        context: &str,
        c: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let mut v = self.vault(std::iter::once(context).chain(wire::digest_texts(d)));
        let (d, context) = (wire::mask_digests(&mut v, d), v.mask(context));
        let out = self.call(c.clone(), self.inner.synthesize(&d, &context, c)).await;
        let out = out.map(|e| wire::unmask_envelope(&mut v, e));
        wire::record_metrics(&v);
        out
    }
    async fn chat(
        &self,
        system: &str,
        turns: &[ChatTurn],
        c: CancellationToken,
    ) -> Result<String, ExtractError> {
        let mut v = self.vault(std::iter::once(system).chain(turns.iter().map(|t| t.content.as_str())));
        let (system, turns) = (v.mask(system), wire::mask_turns(&mut v, turns));
        let out = self.call(c.clone(), self.inner.chat(&system, &turns, c)).await;
        let out = out.map(|a| v.unmask(&a));
        wire::record_metrics(&v);
        out
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
            known: Default::default(),
        }
    }

    #[test]
    fn an_undecided_or_denied_account_is_refused_with_the_consent_message() {
        // Explicit Some(false) too: after one save a new account is written
        // as Some(false), indistinguishable from a deliberate untick (C13).
        for permission in [None, Some(false)] {
            let a = account("new", "none", permission);
            assert!(!account_allowed(&a));
            assert!(needs_consent(&a));
            let err = require_account(&[a], "new").unwrap_err();
            assert!(is_consent_refusal(&err));
            assert!(err.to_string().ends_with(AI_CONSENT_NEEDED), "{err}");
        }
        // An unavailable account, or a Disabled provider, still gets the
        // general message: allowing data would not make either usable.
        let mut gone = account("gone", "none", Some(true));
        gone.status = AccountStatus::Inactive;
        for a in [gone, account("off", "disabled", None)] {
            assert!(!needs_consent(&a));
            let id = a.id.clone();
            let err = require_account(&[a], &id).unwrap_err();
            assert!(!is_consent_refusal(&err));
            assert!(!err.to_string().contains(AI_CONSENT_NEEDED));
        }
    }

    #[test]
    fn allow_sets_the_permission() {
        let mut a = account("a", "none", None);
        allow_ai(&mut a).unwrap();
        assert_eq!(a.llm.data_allowed, Some(true));
        assert!(account_allowed(&a));
    }

    #[test]
    fn allow_refuses_an_account_with_the_provider_disabled() {
        let mut a = account("a", "disabled", None);
        assert!(allow_ai(&mut a).is_err());
        assert_ne!(a.llm.data_allowed, Some(true));
    }

    #[test]
    fn an_allow_is_planned_before_anything_is_cancelled() {
        // Pre-flight W2: invalidate_ai cancels every account's in-flight AI
        // work, so an Allow that is refused or changes nothing must be
        // decided before it, from the accounts alone.
        let accounts = [
            account("new", "none", None),
            account("denied", "http", Some(false)),
            account("on", "none", Some(true)),
            account("off", "disabled", None),
        ];
        assert_eq!(plan_allow(&accounts, "new"), Ok(AllowPlan::Allow));
        assert_eq!(plan_allow(&accounts, "denied"), Ok(AllowPlan::Allow));
        assert_eq!(plan_allow(&accounts, "on"), Ok(AllowPlan::AlreadyAllowed));
        assert!(plan_allow(&accounts, "off").is_err());
        assert!(plan_allow(&accounts, "missing").is_err());
    }

    #[test]
    fn the_frontend_recognises_the_consent_refusal() {
        let ts = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../src/lib/aiConsent.ts")).unwrap();
        assert!(ts.contains(AI_CONSENT_NEEDED), "src/lib/aiConsent.ts must carry the exact refusal text");
    }

    #[test]
    fn legacy_disabled_and_explicit_denial_cannot_be_overridden_by_provider_selection() {
        assert!(!account_allowed(&account("a", "disabled", None)));
        assert!(!account_allowed(&account("a", "disabled", Some(true))));
        assert!(!account_allowed(&account("a", "http", Some(false))));
        // Legacy eligibility is preserved by the accounts.json migration now
        // (accounts::apply_ai_consent); in memory, absent means undecided.
        assert!(!account_allowed(&account("a", "none", None)));
        assert!(account_allowed(&account("a", "none", Some(true))));
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
            account("allowed", "none", Some(true)),
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
                &excluded, &crate::privacy::ai_scope::AiScope::default(),
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
            WorkflowKind::CleanTranscript,
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
            crate::llm::filing::suggest_folder(&p, &db, "denied", "deadbeef", c.clone(), &crate::privacy::ai_scope::AiScope::default())
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
            c,
            &crate::privacy::ai_scope::AiScope::default()
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
            &[], &crate::privacy::ai_scope::AiScope::default(),
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
                known: Default::default(),
            },
        );
        runtime
            .results
            .lock()
            .unwrap()
            .insert("result".into(), ("a".into(), serde_json::json!(0), crate::provenance::Actor::Human));
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
            known: Default::default(),
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

    /// C2 (final review): a value Ask's turn-1 answer repeats is stored
    /// unmasked in history; turn 2 carries it with no context that would
    /// re-detect it. The session's seed must mask it anyway.
    #[tokio::test]
    async fn a_value_masked_in_one_ask_turn_never_leaves_in_a_later_turn() {
        struct Repeats {
            seen: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl LlmProvider for Repeats {
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
            async fn chat(&self, s: &str, t: &[ChatTurn], _: CancellationToken) -> Result<String, ExtractError> {
                let all = format!("{s} {t:?}");
                self.seen.lock().unwrap().push(all.clone());
                if t.last().is_some_and(|t| t.content.starts_with("CATALOG:")) {
                    return Ok("aabbccdd".into());
                }
                // The answer repeats the password the way a model does: by
                // echoing the token it was given.
                let tok = all.find('\u{27E6}').map(|i| {
                    let rest = &all[i..];
                    rest[..rest.find('\u{27E7}').unwrap() + '\u{27E7}'.len_utf8()].to_string()
                });
                Ok(format!("Use {} to join the wifi.", tok.unwrap_or_default()))
            }
        }
        let db = temp_db();
        note("allowed", "aabbccdd-0000")
            .title("Office wifi")
            .body("Office wifi pass: Tr0ub4dor")
            .label("Notes/Office")
            .insert(&db);
        let stamp = serde_json::json!(1);
        let mut session = Session {
            stamp: stamp.clone(),
            scope: AskScope::AllAccounts,
            turns: vec![],
            cancel: CancellationToken::new(),
            busy: false,
            known: Default::default(),
        };
        let gate = Mutex::new(());
        let seen = Arc::new(Mutex::new(vec![]));
        let mut answers = vec![];
        for question in ["office wifi?", "and what did you say it was?"] {
            let turns = session.begin_turn(&stamp, question).unwrap();
            let p = CheckedProvider {
                cancel: Mutex::new(None),
                gate: &gate,
                inner: Box::new(Repeats { seen: seen.clone() }),
                valid: Box::new(|| true),
                known: session.known.clone(),
            };
            let a = run_ask(&db, &p, &session.scope, &turns, CancellationToken::new(), &[], &crate::privacy::ai_scope::AiScope::default())
                .await
                .unwrap();
            session.finish_turn(&stamp, question.into(), Some(&a.markdown), false).unwrap();
            answers.push(a.markdown);
        }
        assert!(answers[0].contains("Tr0ub4dor"), "turn 1's stored answer holds the raw value: {}", answers[0]);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4, "two calls per turn");
        for sent in seen.iter() {
            assert!(!sent.contains("Tr0ub4dor"), "the password reached the provider: {sent}");
        }
    }

    #[test]
    fn the_known_values_seed_is_cleared_with_the_history() {
        let stamp = serde_json::json!(1);
        let fresh = || Session {
            stamp: stamp.clone(),
            scope: AskScope::AllAccounts,
            turns: vec![],
            cancel: CancellationToken::new(),
            busy: false,
            known: Default::default(),
        };
        let fill = |s: &Session| crate::privacy::vault::Vault::reserving(["pass: hunter22x"]).remember(&mut s.known.lock().unwrap());
        // A stamp change at the start of a turn.
        let mut s = fresh();
        fill(&s);
        assert!(s.begin_turn(&serde_json::json!(2), "q").is_err());
        assert!(s.known.lock().unwrap().is_empty(), "stamp change");
        // A cancelled turn, and a stamp change at its end.
        for (end_stamp, cancelled) in [(serde_json::json!(1), true), (serde_json::json!(2), false)] {
            let mut s = fresh();
            s.begin_turn(&stamp, "q").unwrap();
            fill(&s);
            assert!(s.finish_turn(&end_stamp, "q".into(), Some("a"), cancelled).is_err());
            assert!(s.known.lock().unwrap().is_empty(), "finish_turn error");
        }
        // A good turn keeps it.
        let mut s = fresh();
        s.begin_turn(&stamp, "q").unwrap();
        fill(&s);
        s.finish_turn(&stamp, "q".into(), Some("a"), false).unwrap();
        assert_eq!(s.known.lock().unwrap().len(), 1);
    }

    #[test]
    fn an_issued_result_remembers_its_actor() {
        let rt = Runtime::default();
        let a = crate::provenance::Actor::Workflow { name: "autolink".into(), model: "m".into() };
        let id = rt.issue_result("acct", serde_json::json!(1), a.clone());
        assert_eq!(rt.result_actor(&id), Some(a));
        assert_eq!(rt.result_actor("nope"), None);
        assert!(rt.result_valid(&id, "acct", serde_json::json!(1)), "validation is unchanged");
    }

    #[test]
    fn deferred_result_cannot_cross_accounts_or_policy_revisions() {
        let runtime = Runtime::default();
        let stamp = serde_json::json!(["provider", 1]);
        let id = runtime.issue_result("a", stamp.clone(), crate::provenance::Actor::Human);
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
            mode: IngestMode::KeyPoints,
            clean_limits: crate::llm::transcript::CleanLimits { chunk_chars: 4_000, max_attempts: 16 },
            actor: crate::provenance::Actor::test(),
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
