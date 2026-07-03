//! Lesson extraction — paste arbitrary source text, LLM returns structured
//! lessons, Jodd files them in a system workflow folder.
//!
//! See docs/superpowers/specs/2026-06-13-lesson-extraction-design.md

pub mod claude_code;
pub mod http;
pub mod markdown;
pub mod prompt;
pub mod provider;
pub mod resolve;
