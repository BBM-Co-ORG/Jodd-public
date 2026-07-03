//! Resolve an account's configured LessonProvider.
//!
//! Returns a boxed trait object so the caller (extract_lessons command)
//! doesn't need to know which concrete provider is in play. Any missing
//! required configuration surfaces as ExtractError::NotConfigured, which
//! the UI maps to a friendly "open Account Settings" prompt.

use std::time::Duration;

use crate::accounts::{read_llm_api_key, Account, LlmProviderKind};
use crate::lessons::claude_code::ClaudeCodeProvider;
use crate::lessons::http::HttpProvider;
use crate::lessons::provider::{ExtractError, LessonProvider};

pub fn resolve_provider(
    account: &Account,
) -> Result<Box<dyn LessonProvider>, ExtractError> {
    match account.llm.provider {
        LlmProviderKind::None => Err(ExtractError::NotConfigured(
            "no LLM provider configured for this account".into(),
        )),
        LlmProviderKind::ClaudeCode => ClaudeCodeProvider::detect()
            .map(|p| Box::new(p) as Box<dyn LessonProvider>)
            .ok_or_else(|| {
                ExtractError::NotConfigured(
                    "claude binary not found in PATH".into(),
                )
            }),
        LlmProviderKind::Http => {
            let base_url = account.llm.http_base_url.clone().ok_or_else(|| {
                ExtractError::NotConfigured("http base_url missing".into())
            })?;
            let model = account.llm.http_model.clone().ok_or_else(|| {
                ExtractError::NotConfigured("http model missing".into())
            })?;
            let api_key = read_llm_api_key(&account.id);
            Ok(Box::new(HttpProvider::new(
                base_url,
                model,
                api_key,
                Duration::from_secs(90),
            )?))
        }
    }
}
