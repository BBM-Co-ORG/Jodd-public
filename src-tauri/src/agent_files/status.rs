//! `<mirror>/.jodd/status.json` (spec 2026-11-05 §4.4): what the last sync
//! did. Settings reads it when its section opens and when the window regains
//! focus, and `brief` reads it for its one line about the mirror.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// RFC 3339, with offset.
    pub finished_at: String,
    /// `ok`, `partial` (the deadline cut the run short; the next run
    /// continues) or `failed`.
    pub outcome: String,
    /// Absolute path of the mirror this status describes.
    pub path: String,
    #[serde(default)]
    pub accounts: Vec<AccountStatus>,
    #[serde(default)]
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountStatus {
    pub account_id: String,
    /// Notes now in the mirror for this account.
    pub notes: usize,
    pub written: usize,
    pub deleted: usize,
    /// Notes with at least one secret replaced by a mask.
    pub masked_notes: usize,
    /// Attachments left out: too large, or gone from the cache.
    pub skipped_attachments: usize,
    /// A mirrored file changed on disk since Jodd wrote it, and was overwritten.
    pub overwritten_local_edits: usize,
    /// Many notes vanished at once; nothing was deleted (§4.3).
    pub held_back: bool,
    /// Why the account was not mirrored this run, e.g. not synced yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

pub const OK: &str = "ok";
pub const PARTIAL: &str = "partial";
pub const FAILED: &str = "failed";

pub fn read(path: &Path) -> Option<Status> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub fn write(path: &Path, status: &Status) -> Result<(), String> {
    let text = serde_json::to_string_pretty(status).map_err(|e| e.to_string())?;
    super::write_file(path, text.as_bytes(), false)
}
