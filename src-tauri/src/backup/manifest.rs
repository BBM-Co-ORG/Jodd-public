//! The JSON that describes a backup version: written beside the zip as
//! `<stamp>.jodd-backup.json` (the version exists only when this file does)
//! and, minus the zip's own size and hash, inside the zip as
//! `.jodd/bundle.json`.

use super::Trigger;
use serde::{Deserialize, Serialize};

/// The only `jodd_backup` value this build reads or writes.
pub const FORMAT_VERSION: u32 = 1;

/// One note as the version list and restore picker see it, without opening
/// the zip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestNote {
    pub uuid: String,
    pub title: String,
    pub folder: String,
    pub modified: Option<String>,
    pub content_sha256: String,
    pub attachments: usize,
    pub partial: Option<String>,
}

/// `.jodd/bundle.json` is this type with `zip_bytes`, `zip_sha256` and `keep`
/// at their defaults: a zip cannot contain its own size or hash, and `keep`
/// is a property of the version on disk, not of the bundle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub jodd_backup: u32,
    pub account_id: String,
    pub backend: String,
    pub root_label: String,
    pub created_ms: i64,
    pub app_version: String,
    pub device: String,
    pub trigger: Trigger,
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub zip_bytes: u64,
    #[serde(default)]
    pub zip_sha256: String,
    pub notes: Vec<ManifestNote>,
    pub folders: Vec<String>,
    /// `(uuid, reason)` for every note the writer could not put in the bundle.
    pub skipped: Vec<(String, String)>,
}
