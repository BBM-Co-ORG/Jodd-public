//! Resolve an account's configured LlmProvider.
//!
//! Returns a boxed trait object so the caller (extract_note command)
//! doesn't need to know which concrete provider is in play. Any missing
//! required configuration surfaces as ExtractError::NotConfigured, which
//! the UI maps to a friendly "open Account Settings" prompt.

use crate::accounts::{read_llm_api_key, Account, LlmConfig, LlmProviderKind};
use crate::llm::agent_cli::AgentCliProvider;
use crate::llm::http::HttpProvider;
use crate::llm::provider::{ExtractError, LlmProvider};

/// Which keychain entry holds the API key for the resolved config. The key
/// lives with whoever owns the config, so an inherited HTTP provider reads
/// the APP key, not the account's.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiKeyOwner {
    App,
    Account(String),
}

#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    pub llm: LlmConfig,
    pub api_key_owner: ApiKeyOwner,
}

/// The §4.2 cascade as a pure function — no I/O, so every row of the table is
/// a unit test. `None` return means "no provider": the caller surfaces
/// ExtractError::NotConfigured.
pub fn effective_config(
    app: Option<&LlmConfig>,
    apply_to_accounts: bool,
    account_llm: &LlmConfig,
    account_id: &str,
) -> Option<EffectiveConfig> {
    match account_llm.provider {
        // Explicit opt-out beats everything.
        LlmProviderKind::Disabled => None,
        // Unset = inherit, but only when the app opted in to sharing.
        LlmProviderKind::None => match (app, apply_to_accounts) {
            (Some(cfg), true) => Some(EffectiveConfig {
                llm: cfg.clone(),
                api_key_owner: ApiKeyOwner::App,
            }),
            _ => None,
        },
        // Any explicit account choice wins.
        _ => Some(EffectiveConfig {
            llm: account_llm.clone(),
            api_key_owner: ApiKeyOwner::Account(account_id.to_string()),
        }),
    }
}

/// Build a provider from an already-resolved config. `api_key_owner` decides
/// which keychain entry the HTTP provider reads.
fn build(eff: &EffectiveConfig) -> Result<Box<dyn LlmProvider>, ExtractError> {
    // Every agent-CLI provider resolves a binary with `which` and spawns it
    // as a child process. Android allows neither. HTTP providers are pure
    // reqwest and are unaffected — the cascade in resolve_provider_for_account
    // still reaches them normally.
    #[cfg(target_os = "android")]
    if matches!(
        eff.llm.provider,
        LlmProviderKind::ClaudeCode | LlmProviderKind::AgentCli
    ) {
        return Err(ExtractError::NotConfigured(
            "Agent CLI providers need a local binary and a child process, which Android cannot run. Configure an HTTP provider instead.".into(),
        ));
    }

    match eff.llm.provider {
        LlmProviderKind::None | LlmProviderKind::Disabled => Err(ExtractError::NotConfigured(
            "no LLM provider configured".into(),
        )),
        LlmProviderKind::ClaudeCode => {
            let spec = crate::llm::presets::preset_by_id("claude")
                .expect("claude preset is always present");
            Ok(Box::new(AgentCliProvider::new(spec)?) as Box<dyn LlmProvider>)
        }
        LlmProviderKind::AgentCli => {
            let id = eff.llm.agent_preset.as_deref().ok_or_else(|| {
                ExtractError::NotConfigured("no agent CLI selected".into())
            })?;
            let spec = if id == "custom" {
                eff.llm.agent_custom.clone().ok_or_else(|| {
                    ExtractError::NotConfigured("custom agent CLI selected but not configured".into())
                })?
            } else {
                crate::llm::presets::preset_by_id(id).ok_or_else(|| {
                    ExtractError::NotConfigured(format!("unknown agent CLI preset '{id}'"))
                })?
            };
            Ok(Box::new(AgentCliProvider::new(spec)?) as Box<dyn LlmProvider>)
        }
        LlmProviderKind::Http => {
            let base_url = eff.llm.http_base_url.clone().ok_or_else(|| {
                ExtractError::NotConfigured("http base_url missing".into())
            })?;
            let model = eff.llm.http_model.clone().ok_or_else(|| {
                ExtractError::NotConfigured("http model missing".into())
            })?;
            let api_key = match &eff.api_key_owner {
                ApiKeyOwner::App => crate::app_llm_config::load_secret(),
                ApiKeyOwner::Account(id) => read_llm_api_key(id),
            };
            Ok(Box::new(HttpProvider::new(
                base_url,
                model,
                api_key,
                std::time::Duration::from_secs(90),
            )?))
        }
    }
}

/// Ask Jodd's provider: always the app-level one, independent of
/// `apply_to_accounts` (spec §4.2). Ask Jodd is cross-account, so no single
/// account's provider is the right owner.
pub fn resolve_app_provider() -> Result<Box<dyn LlmProvider>, ExtractError> {
    let cfg = crate::app_llm_config::load().ok_or_else(|| {
        ExtractError::NotConfigured("no app-level LLM provider configured".into())
    })?;
    build(&EffectiveConfig { llm: cfg.llm, api_key_owner: ApiKeyOwner::App })
}

/// The agent-CLI preset id a config resolves to, if it uses one at all.
///
/// Kept beside `build` because it must agree with `build`'s own dispatch:
/// `ClaudeCode` is the legacy spelling that `build` resolves to the `claude`
/// preset, and a diagnosis keyed on a different id would silently find no
/// signatures. HTTP providers have no preset and answer `None`.
pub fn agent_preset_id_of(llm: &LlmConfig) -> Option<String> {
    match llm.provider {
        LlmProviderKind::ClaudeCode => Some("claude".to_string()),
        LlmProviderKind::AgentCli => llm.agent_preset.clone(),
        // Spelled out rather than `_`, right below a `build()` whose match on
        // this same enum is exhaustive: a future agent-CLI-shaped variant
        // must fail to compile here and force a decision, instead of
        // silently getting no failure dictionary (CLAUDE.md gotcha #18's
        // discipline — a canonicalization/mint policy with no wildcard arm).
        LlmProviderKind::None | LlmProviderKind::Disabled | LlmProviderKind::Http => None,
    }
}

/// The effective config for an account, as `resolve_provider_for_account`
/// computes it. Extracted so a caller that needs the CONFIG (to name which
/// CLI failed) cannot drift from the caller that needs the PROVIDER — both
/// go through the exact same §4.2 cascade call, in one place.
pub fn effective_for_account(account: &Account) -> Option<EffectiveConfig> {
    let app = crate::app_llm_config::load();
    effective_config(
        app.as_ref().map(|c| &c.llm),
        app.as_ref().map(|c| c.apply_to_accounts).unwrap_or(false),
        &account.llm,
        &account.id,
    )
}

/// Per-account workflows (Extract, auto-link). Implements the §4.2 cascade.
pub fn resolve_provider_for_account(
    account: &Account,
) -> Result<Box<dyn LlmProvider>, ExtractError> {
    let eff = effective_for_account(account).ok_or_else(|| {
        ExtractError::NotConfigured("no LLM provider configured for this account".into())
    })?;
    build(&eff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{LlmConfig, LlmProviderKind};

    fn http_cfg() -> LlmConfig {
        LlmConfig {
            provider: LlmProviderKind::Http,
            http_base_url: Some("https://api.example.com/v1".into()),
            http_model: Some("gpt-4o-mini".into()),
            http_api_key_keychain: None,
            agent_preset: None,
            agent_custom: None,
        }
    }

    fn agent_cfg() -> LlmConfig {
        LlmConfig {
            provider: LlmProviderKind::AgentCli,
            agent_preset: Some("claude".into()),
            ..LlmConfig::default()
        }
    }

    fn disabled_cfg() -> LlmConfig {
        LlmConfig { provider: LlmProviderKind::Disabled, ..LlmConfig::default() }
    }

    // Spec §4.2, one test per row.

    #[test]
    fn app_on_account_unset_inherits_app() {
        let eff = effective_config(Some(&http_cfg()), true, &LlmConfig::default(), "acct@x")
            .expect("should resolve");
        assert_eq!(eff.llm.provider, LlmProviderKind::Http);
        assert_eq!(eff.api_key_owner, ApiKeyOwner::App);
    }

    #[test]
    fn app_on_account_configured_account_wins() {
        let eff = effective_config(Some(&http_cfg()), true, &agent_cfg(), "acct@x")
            .expect("should resolve");
        assert_eq!(eff.llm.provider, LlmProviderKind::AgentCli);
        assert_eq!(eff.api_key_owner, ApiKeyOwner::Account("acct@x".into()));
    }

    #[test]
    fn app_on_account_disabled_yields_none() {
        assert!(effective_config(Some(&http_cfg()), true, &disabled_cfg(), "acct@x").is_none());
    }

    #[test]
    fn app_off_account_unset_yields_none() {
        assert!(effective_config(Some(&http_cfg()), false, &LlmConfig::default(), "acct@x").is_none());
    }

    #[test]
    fn app_off_account_configured_uses_account() {
        let eff = effective_config(Some(&http_cfg()), false, &agent_cfg(), "acct@x")
            .expect("should resolve");
        assert_eq!(eff.llm.provider, LlmProviderKind::AgentCli);
    }

    #[test]
    fn no_app_account_configured_uses_account() {
        let eff = effective_config(None, false, &agent_cfg(), "acct@x").expect("should resolve");
        assert_eq!(eff.llm.provider, LlmProviderKind::AgentCli);
        assert_eq!(eff.api_key_owner, ApiKeyOwner::Account("acct@x".into()));
    }

    #[test]
    fn no_app_account_unset_yields_none() {
        assert!(effective_config(None, false, &LlmConfig::default(), "acct@x").is_none());
    }

    #[test]
    fn legacy_claude_code_is_treated_as_configured_not_inherit() {
        // Back-compat: pre-v0.19 accounts.json says "claude_code". It is an
        // explicit account choice, so it must win over the app default.
        let legacy = LlmConfig { provider: LlmProviderKind::ClaudeCode, ..LlmConfig::default() };
        let eff = effective_config(Some(&http_cfg()), true, &legacy, "acct@x").expect("resolve");
        assert_eq!(eff.llm.provider, LlmProviderKind::ClaudeCode);
    }

    /// The command needs the preset id to look up that CLI's failure
    /// signatures. `ClaudeCode` is the legacy spelling of the `claude`
    /// preset and must resolve to the same id, or its dictionary is
    /// unreachable for anyone whose config predates the rename.
    #[test]
    fn agent_preset_id_covers_the_legacy_claude_code_spelling() {
        let legacy = LlmConfig {
            provider: LlmProviderKind::ClaudeCode,
            agent_preset: None,
            ..Default::default()
        };
        assert_eq!(agent_preset_id_of(&legacy), Some("claude".to_string()));

        let modern = LlmConfig {
            provider: LlmProviderKind::AgentCli,
            agent_preset: Some("codex".into()),
            ..Default::default()
        };
        assert_eq!(agent_preset_id_of(&modern), Some("codex".to_string()));

        let http = LlmConfig {
            provider: LlmProviderKind::Http,
            agent_preset: None,
            ..Default::default()
        };
        assert_eq!(agent_preset_id_of(&http), None);
    }
}
