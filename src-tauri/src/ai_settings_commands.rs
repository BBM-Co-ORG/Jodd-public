//! AI provider settings: `test_llm_provider`, the agent-CLI presets, app LLM
//! settings and the per-account AI allow / deny. Moved out of `lib.rs`
//! 2026-11-16.

use super::*;

#[derive(serde::Serialize)]
pub struct LlmTestResult {
    pub ok: bool,
    pub elapsed_ms: u64,
    pub error: Option<String>,
    /// First ~500 chars of whatever came back, for diagnosing a Custom spec.
    pub raw_head: String,
    /// A named cause, when this CLI's measured failure modes recognise the
    /// error. `None` means show the raw text — never a guess.
    pub cause: Option<String>,
    /// What the user can do about `cause`. Always present when `cause` is.
    pub action: Option<String>,
}

/// Round-trip a configured provider with a fixed, representative workload
/// (`crate::llm::prompt::CONNECTION_TEST_SAMPLE`) — the same shape of input
/// Extract sees, not a greeting.
///
/// The dominant failure mode for a Custom agent-CLI spec is wrong flags: the
/// CLI waits for interactive input and the user sees "spun for two minutes,
/// then failed" *after* pasting real source text. This probe surfaces that in
/// one run of the real pipeline (structured output, retries, a multi-section
/// envelope) instead of the user's first real paste — at the real workload's
/// real cost, about 10-20 seconds, not a fixed-prompt shortcut. Works for any
/// provider kind, since it goes through the same resolvers the real
/// workflows use.
///
/// `account_id`:
///   - `Some(id)` → the §4.2 cascade for that account (Extract, auto-link)
///   - `None`     → the app-level provider (Ask Jodd), via resolve_app_provider
///
/// The `None` arm is what makes an app-level provider verifiable at all: Ask
/// Jodd is cross-account, so there is no account whose cascade would reach it.
#[tauri::command]
pub async fn test_llm_provider(
    account_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<LlmTestResult, String> {
    let receipt_request = uuid::Uuid::new_v4().to_string();
    llm::receipts::run(&receipt_request, None, "test_llm_provider", async {
        // `preset_id` is hoisted alongside `provider` (not computed separately
        // after the match) so both arms bind it from the exact config each one
        // resolved — an account's `preset_id` must come from its own cascade,
        // never from the app-level config the `None` arm reads.
        let (provider, preset_id, host_label) = match account_id {
            None => {
                // Raw provider on purpose: it bypasses `CheckedProvider` masking
                // because the connection test sends only the fixed
                // `CONNECTION_TEST_SAMPLE`, never user data (spec §4.3).
                let provider =
                    crate::llm::resolve::resolve_app_provider().map_err(|e| e.to_string())?;
                let cfg = crate::app_llm_config::load();
                let preset_id = cfg
                    .as_ref()
                    .and_then(|c| crate::llm::resolve::agent_preset_id_of(&c.llm));
                let host_label = cfg
                    .as_ref()
                    .and_then(|c| c.llm.agent_host.as_ref().map(crate::llm::remote::host_label));
                (provider, preset_id, host_label)
            }
            Some(id) => {
                // Clone the account and drop the guard before any await:
                // `state.accounts` is a std Mutex and a guard held across an await
                // point is not Send.
                let account = {
                    let list = state.accounts.lock().unwrap();
                    list.iter()
                        .find(|a| a.id == id)
                        .cloned()
                        .ok_or_else(|| format!("account not found: {id}"))?
                };
                let provider = checked_account_provider(&state, &account)
                    .map_err(|e| e.to_string())?;
                // Goes through the same `effective_for_account` helper
                // `resolve_provider_for_account` uses internally, so the preset
                // id named here can never drift from the config the provider
                // above was actually built from.
                let eff = crate::llm::resolve::effective_for_account(&account);
                let preset_id = eff
                    .as_ref()
                    .and_then(|e| crate::llm::resolve::agent_preset_id_of(&e.llm));
                let host_label = eff
                    .as_ref()
                    .and_then(|e| e.llm.agent_host.as_ref().map(crate::llm::remote::host_label));
                (provider, preset_id, host_label)
            }
        };

        let started = std::time::Instant::now();
        let cancel = tokio_util::sync::CancellationToken::new();
        // No account tag vocabulary to show here — this probes connectivity
        // only, not a real extraction into a specific account.
        llm::receipts::stage("connection_test");
        let result = provider
            .extract(crate::llm::prompt::CONNECTION_TEST_SAMPLE, &[], cancel)
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let result = match result {
            Ok(env) => match env.usable() {
                Ok(()) => LlmTestResult {
                    ok: true,
                    elapsed_ms,
                    error: None,
                    raw_head: env.lessons_markdown.chars().take(500).collect(),
                    cause: None,
                    action: None,
                },
                Err(reason) => {
                    // `lessons_markdown` is empty (that's `reason`), but the
                    // envelope may still carry `title`/`tags`/`confidence` — the
                    // difference between "the model answered into the wrong
                    // field" and "nothing came back at all". Surface it instead
                    // of throwing it away, same ~500-char cap as the other sites.
                    let raw_head: String = format!(
                        "title={:?} tags={:?} confidence={:?}",
                        env.title, env.tags, env.confidence
                    )
                    .chars()
                    .take(500)
                    .collect();
                    LlmTestResult {
                        ok: false,
                        elapsed_ms,
                        error: Some(reason),
                        raw_head,
                        cause: None,
                        action: None,
                    }
                }
            },
            Err(e) => {
                let raw_head = match &e {
                    crate::llm::provider::ExtractError::MalformedEnvelope { raw, .. } => {
                        raw.chars().take(500).collect()
                    }
                    _ => String::new(),
                };
                let text = e.to_string();
                let diag = preset_id
                    .as_deref()
                    .and_then(|id| crate::llm::agent_cli::diagnose_on(id, &text, host_label.as_deref()));
                LlmTestResult {
                    ok: false,
                    elapsed_ms,
                    error: Some(text),
                    raw_head,
                    cause: diag.as_ref().map(|d| d.cause.clone()),
                    action: diag.as_ref().map(|d| d.action.clone()),
                }
            }
        };
        if !result.ok { llm::receipts::check("workflow_result_failed"); }
        Ok(result)
    }).await
}

/// Enumerate shipped agent-CLI presets with live PATH-detection status.
///
/// The preset list lives only in Rust; the settings UI is generated from this
/// response, so adding a CLI is a one-row edit in `llm/presets.rs`.
#[tauri::command]
pub fn list_agent_cli_presets() -> Vec<crate::llm::presets::AgentCliPresetInfo> {
    crate::llm::presets::preset_infos()
}

/// Read the per-account LlmConfig. Frontend uses this to populate the LLM
/// settings modal.
#[tauri::command]
pub fn get_llm_settings(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<crate::accounts::LlmConfig, String> {
    let list = state.accounts.lock().unwrap();
    list.iter()
        .find(|a| a.id == account_id)
        .map(|a| a.llm.clone())
        .ok_or_else(|| format!("account not found: {account_id}"))
}

/// Persist LlmConfig to accounts.json and (optionally) write or clear the
/// API key in the OS keychain. The API key never enters accounts.json.
///
/// `api_key` semantics:
///   - `None`             → leave keychain untouched
///   - `Some("")` (blank) → delete keychain entry
///   - `Some(key)`        → write key to keychain
#[tauri::command]
pub fn update_llm_settings(
    account_id: String,
    mut cfg: crate::accounts::LlmConfig,
    api_key: Option<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _policy_gate = state.ai_policy.gate.lock().unwrap();
    invalidate_ai(&state, &app);
    // Mutate in-memory under the lock, clone for I/O, release lock before
    // touching disk. Local-first doctrine: in-memory state is updated
    // synchronously; the disk write follows.
    let snapshot = {
        let mut list = state.accounts.lock().unwrap();
        let acct = list
            .iter_mut()
            .find(|a| a.id == account_id)
            .ok_or_else(|| format!("account not found: {account_id}"))?;
        llm::policy::preserve_permission(&acct.llm, &mut cfg);
        acct.llm = cfg;
        list.clone()
    };

    crate::accounts::save_accounts(&snapshot).map_err(|e| format!("save accounts: {e}"))?;

    if let Some(key) = api_key {
        if key.trim().is_empty() {
            crate::accounts::delete_llm_api_key(&account_id);
        } else {
            crate::accounts::write_llm_api_key(&account_id, &key)?;
        }
    }
    Ok(())
}

/// One-click consent from a refusal (spec 2026-10-08 §4.5, privacy PR3).
/// Decides first: an Allow that is refused (provider Disabled, unknown id) or
/// changes nothing (already allowed) returns without `invalidate_ai`, which
/// would cancel the user's other in-flight AI work on every account. Then the
/// update_llm_settings order: invalidate, mutate in memory, save. Touches
/// accounts.json only, never the credential store (gotcha #15).
#[tauri::command]
pub fn allow_ai_for_account(
    account_id: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _policy_gate = state.ai_policy.gate.lock().unwrap();
    let plan = llm::policy::plan_allow(&state.accounts.lock().unwrap(), &account_id)?;
    if plan == llm::policy::AllowPlan::AlreadyAllowed {
        return Ok(());
    }
    invalidate_ai(&state, &app);
    apply_allow(&state.accounts, &account_id, |snapshot| {
        crate::accounts::save_accounts(snapshot).map_err(|e| format!("save accounts: {e}"))
    })
}

/// Mutate in memory, save, and put the previous permission back if the save
/// fails. Without the rollback a retried Allow would see `AlreadyAllowed` in
/// `plan_allow`, return Ok without saving, and report success while
/// accounts.json still denies. `save` is injected so a test never touches the
/// real accounts.json.
pub(crate) fn apply_allow(
    accounts: &std::sync::Mutex<Vec<crate::accounts::Account>>,
    account_id: &str,
    save: impl FnOnce(&[crate::accounts::Account]) -> Result<(), String>,
) -> Result<(), String> {
    let (previous, snapshot) = {
        let mut list = accounts.lock().unwrap();
        let acct = list
            .iter_mut()
            .find(|a| a.id == account_id)
            .ok_or_else(|| format!("account not found: {account_id}"))?;
        let previous = acct.llm.data_allowed;
        llm::policy::allow_ai(acct)?;
        (previous, list.clone())
    };
    if let Err(e) = save(&snapshot) {
        if let Some(acct) = accounts.lock().unwrap().iter_mut().find(|a| a.id == account_id) {
            acct.llm.data_allowed = previous;
        }
        return Err(e);
    }
    Ok(())
}

/// The off half of Settings › Privacy & Data's per-account switch (spec
/// 2026-11-11 D3). Same order as `allow_ai_for_account`: decide first, so a
/// no-op Deny cancels nothing; then invalidate (a tighter policy must end
/// in-flight work for this account), mutate in memory, save, roll back on a
/// failed save. Touches accounts.json only, never the credential store.
#[tauri::command]
pub fn deny_ai_for_account(
    account_id: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _policy_gate = state.ai_policy.gate.lock().unwrap();
    let plan = llm::policy::plan_deny(&state.accounts.lock().unwrap(), &account_id)?;
    if plan == llm::policy::DenyPlan::AlreadyDenied {
        return Ok(());
    }
    invalidate_ai(&state, &app);
    apply_deny(&state.accounts, &account_id, |snapshot| {
        crate::accounts::save_accounts(snapshot).map_err(|e| format!("save accounts: {e}"))
    })
}

/// `apply_allow`'s twin: mutate, save, and restore the previous permission if
/// the save fails, so the switch's rollback matches what is on disk.
pub(crate) fn apply_deny(
    accounts: &std::sync::Mutex<Vec<crate::accounts::Account>>,
    account_id: &str,
    save: impl FnOnce(&[crate::accounts::Account]) -> Result<(), String>,
) -> Result<(), String> {
    let (previous, snapshot) = {
        let mut list = accounts.lock().unwrap();
        let acct = list
            .iter_mut()
            .find(|a| a.id == account_id)
            .ok_or_else(|| format!("account not found: {account_id}"))?;
        let previous = acct.llm.data_allowed;
        llm::policy::deny_ai(acct);
        (previous, list.clone())
    };
    if let Err(e) = save(&snapshot) {
        if let Some(acct) = accounts.lock().unwrap().iter_mut().find(|a| a.id == account_id) {
            acct.llm.data_allowed = previous;
        }
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod allow_ai_for_account_tests {
    /// Pre-flight W2: the decision precedes `invalidate_ai`, so a refused or
    /// no-op Allow cancels nothing. Commands can't be invoked in tests (no
    /// `State` harness), so this reads the source, like `ai_scope_tripwire`.
    /// The signature is split so this test's own text never matches it —
    /// otherwise, with the command missing, `extract_fn_body` would land
    /// here and find `plan_allow(` before `invalidate_ai(` in the lines below.
    #[test]
    fn allow_ai_for_account_decides_before_it_invalidates() {
        let sig = concat!("fn allow_ai", "_for_account(");
        let body = crate::test_support::extract_fn_body(include_str!("ai_settings_commands.rs"), sig);
        let plan = body.find("plan_allow(").expect("allow_ai_for_account decides first");
        let invalidate = body.find("invalidate_ai(").expect("and invalidates only when it will mutate");
        assert!(plan < invalidate);
    }

    fn account(id: &str, permission: Option<bool>) -> crate::accounts::Account {
        serde_json::from_value(serde_json::json!({
            "id": id, "email": "synthetic@example.test", "added_at": "2026-09-19",
            "llm": { "provider": "none", "data_allowed": permission }
        }))
        .unwrap()
    }

    #[test]
    fn a_failed_save_puts_the_permission_back() {
        for previous in [None, Some(false)] {
            let accounts = std::sync::Mutex::new(vec![account("a", previous), account("b", Some(false))]);
            let err = super::apply_allow(&accounts, "a", |snapshot| {
                assert_eq!(snapshot[0].llm.data_allowed, Some(true), "the save sees the allowed state");
                Err("disk full".into())
            })
            .unwrap_err();
            assert_eq!(err, "disk full");
            let list = accounts.lock().unwrap();
            assert_eq!(list[0].llm.data_allowed, previous, "rolled back, so a retry plans Allow again");
            assert_eq!(list[1].llm.data_allowed, Some(false), "other accounts untouched");
            assert_eq!(
                crate::llm::policy::plan_allow(&list, "a"),
                Ok(crate::llm::policy::AllowPlan::Allow)
            );
        }
    }

    #[test]
    fn deny_ai_for_account_decides_before_it_invalidates() {
        let sig = concat!("fn deny_ai", "_for_account(");
        let body = crate::test_support::extract_fn_body(include_str!("ai_settings_commands.rs"), sig);
        let plan = body.find("plan_deny(").expect("deny_ai_for_account decides first");
        let invalidate = body.find("invalidate_ai(").expect("and invalidates only when it will mutate");
        assert!(plan < invalidate);
    }

    #[test]
    fn a_failed_deny_save_puts_the_permission_back() {
        let accounts = std::sync::Mutex::new(vec![account("a", Some(true)), account("b", Some(true))]);
        let err = super::apply_deny(&accounts, "a", |snapshot| {
            assert_eq!(snapshot[0].llm.data_allowed, Some(false), "the save sees the denied state");
            Err("disk full".into())
        })
        .unwrap_err();
        assert_eq!(err, "disk full");
        let list = accounts.lock().unwrap();
        assert_eq!(list[0].llm.data_allowed, Some(true), "rolled back, so a retry plans Deny again");
        assert_eq!(list[1].llm.data_allowed, Some(true), "other accounts untouched");
        assert_eq!(crate::llm::policy::plan_deny(&list, "a"), Ok(crate::llm::policy::DenyPlan::Deny));
    }

    #[test]
    fn a_successful_deny_keeps_the_denial() {
        let accounts = std::sync::Mutex::new(vec![account("a", Some(true))]);
        super::apply_deny(&accounts, "a", |_| Ok(())).unwrap();
        assert_eq!(accounts.lock().unwrap()[0].llm.data_allowed, Some(false));
    }

    #[test]
    fn a_successful_save_keeps_the_permission() {
        let accounts = std::sync::Mutex::new(vec![account("a", None)]);
        super::apply_allow(&accounts, "a", |_| Ok(())).unwrap();
        assert_eq!(accounts.lock().unwrap()[0].llm.data_allowed, Some(true));
    }
}
