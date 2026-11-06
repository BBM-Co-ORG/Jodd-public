//! Backup and restore: OKF v0.2 bundles written to a local folder or over SSH.

pub mod body;
pub mod bundle;
pub mod config;
pub mod destination;
pub mod manifest;
pub mod naming;
pub mod okf;
pub mod restore;
pub mod run;

use serde::{Deserialize, Serialize};

/// What started a backup; the version list shows it and pruning treats
/// `PreRestore` versions as ones the user did not ask to keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trigger {
    Manual,
    Scheduled,
    PreRestore,
}

/// How a note body is stored in a bundle.
/// - `Markdown`: the `.md` body is the note; it reproduces the HTML byte for byte.
/// - `HtmlFile`: the `.md` body is a readable rendering for people and other
///   OKF tools, and the exact HTML is its own file (`JoddMeta::html`), which
///   restore reads. Live pass 2026-10-24: with `Html`, three notes in four
///   carried raw HTML in their `.md` file.
/// - `Html`: the `.md` body is the HTML itself. Written by the first M1
///   builds only; still read, and `OpenBundle` turns `HtmlFile` into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyFormat {
    Markdown,
    Html,
    #[serde(rename = "html-file")]
    HtmlFile,
}

/// One provenance event, copied verbatim so a restore keeps the trust tier.
/// `at` is ISO-8601 with offset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    pub at: String,
}

/// An attachment a note references, with where its bytes sit in the bundle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub content_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub mime: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_apple_part_url: Option<String>,
}

/// Everything Jodd-specific about a note; lives under one `jodd:` key so the
/// top level of a concept stays plain OKF.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JoddMeta {
    pub uuid: String,
    pub account: String,
    pub backend: String,
    pub folder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    pub body: BodyFormat,
    /// Bundle-relative path (leading `/`) of the exact HTML when `body` is `HtmlFile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    pub content_sha256: String,
    pub sync_state: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<String>,
}

/// A note as an OKF concept document. `generated` and `verified` hold
/// `(by, at)` pairs; `sources` holds resource URLs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteDoc {
    pub title: String,
    pub tags: Vec<String>,
    pub sources: Vec<String>,
    pub generated: Option<(String, String)>,
    pub verified: Vec<(String, String)>,
    pub jodd: Option<JoddMeta>,
    pub body: String,
}
