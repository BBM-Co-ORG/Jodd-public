//! Vertical #0 — Apple-via-Gmail. Composes the Gmail transport + identity +
//! deriver behind the backend trait surface. Constructed per-operation from
//! the already-fetched (token, label_map, user_email); the shared core's
//! ensure_token / cached_label_map run first and unchanged.

use super::{Capabilities, Fidelity, FolderModel, Vertical};
use std::collections::HashMap;

pub mod wire;
pub mod transport;
pub mod identity;
pub mod deriver;

pub struct GmailVertical {
    pub(crate) token: String,
    pub(crate) label_map: HashMap<String, String>,
    pub(crate) user_email: String,
    pub(crate) meta_label: String,
    capabilities: Capabilities,
}

impl GmailVertical {
    pub fn new(token: String, label_map: HashMap<String, String>, user_email: String, meta_label: String) -> Self {
        Self {
            token, label_map, user_email, meta_label,
            capabilities: Capabilities {
                folder_model: FolderModel::SingleExclusive,
                fidelity: Fidelity::Full,
            },
        }
    }
}

impl Vertical for GmailVertical {
    fn backend_id(&self) -> &str { "apple-via-gmail" }
    fn capabilities(&self) -> &Capabilities { &self.capabilities }
}

// ─── NoteStore impl (Pragmatic scope) ──────────────────────────────────────
//
// The fat list/fetch operations keep their existing dedup/sort/cache-reuse
// logic (intricate, race-condition-sensitive — see gmail.rs + docs/SYNC-BUGS).
// They're exposed as NoteStore trait methods that delegate to the existing
// `crate::backend::gmail::wire::*` functions.
// Decomposing dedup/sort into core-side generic logic is deferred to the JMAP work.
use crate::backend::{NoteStore, Note, DedupSummary, MessageIndex, TrashedNote, SaveOp, SavedNote, Attachment, TransportError};
use crate::backend::gmail::transport::classify_str;
use async_trait::async_trait;

#[async_trait]
impl NoteStore for GmailVertical {
    async fn list_all_notes(
        &self,
        cache_by_id: &HashMap<String, Note>,
    ) -> Result<(Vec<Note>, DedupSummary), TransportError> {
        crate::backend::gmail::wire::list_notes(&self.token, &self.label_map, cache_by_id)
            .await.map_err(|e| classify_str(&e))
    }

    async fn list_notes_in_folder(
        &self,
        folder: &str,
        cache_by_id: &HashMap<String, Note>,
    ) -> Result<Vec<Note>, TransportError> {
        let label_id = match self.label_map.iter().find(|(_, n)| n.as_str() == folder) {
            Some((id, _)) => id.clone(),
            None => return Ok(vec![]),
        };
        crate::backend::gmail::wire::list_notes_in_label(&self.token, &self.user_email, &label_id, &self.label_map, cache_by_id)
            .await.map_err(|e| classify_str(&e))
    }

    async fn list_index(&self) -> Result<Vec<MessageIndex>, TransportError> {
        crate::backend::gmail::wire::list_account_index(&self.token, &self.user_email, &self.label_map)
            .await.map_err(|e| classify_str(&e))
    }

    async fn fetch_note(&self, remote_id: &str) -> Result<Note, TransportError> {
        crate::backend::gmail::wire::fetch_note(&self.token, remote_id, &self.label_map)
            .await.map_err(|e| classify_str(&e))
    }

    async fn save_note_full(&self, op: &SaveOp<'_>, attachments: &[Attachment]) -> Result<SavedNote, TransportError> {
        crate::backend::gmail::wire::save_note(
            &self.token, op.title, op.body_html,
            op.existing_remote_id, op.existing_uuid, op.existing_created_date,
            op.label, &self.user_email, &self.label_map, attachments,
        ).await.map_err(|e| classify_str(&e))
    }

    async fn find_ids_for_uuid(&self, uuid: &str) -> Result<Vec<String>, TransportError> {
        crate::backend::gmail::wire::find_gmail_ids_for_uuid(&self.token, uuid, &self.label_map)
            .await.map_err(|e| classify_str(&e))
    }

    async fn list_trashed(&self) -> Result<Vec<TrashedNote>, TransportError> {
        crate::backend::gmail::wire::list_trashed_notes(&self.token, &self.label_map)
            .await.map_err(|e| classify_str(&e))
    }

    async fn untrash(&self, remote_id: &str) -> Result<(), TransportError> {
        crate::backend::gmail::wire::untrash_note(&self.token, remote_id)
            .await.map_err(|e| classify_str(&e))
    }
}
