//! LLM-backed workflows over note text — Extract (paste arbitrary source
//! text, LLM returns a structured note) and auto-link (judge which existing
//! notes a new note should link to).
//!
//! Provider selection lives in `resolve.rs`; the provider contract is the
//! `LlmProvider` trait in `provider.rs`.
//!
//! See docs/superpowers/specs/2026-06-13-lesson-extraction-design.md
//! and docs/superpowers/specs/2026-07-27-agent-cli-llm-providers-design.md

pub mod agent_cli;
pub mod autolink;
pub mod http;
pub mod markdown;
pub mod presets;
pub mod prompt;
pub mod provider;
pub mod resolve;
