//! Gmail Transport + MetadataSidecar impls — thin wrappers over the existing
//! crate::gmail free functions, plus HTTP-status classification into
//! TransportError.

use crate::backend::TransportError;
use std::time::Duration;

/// Map an HTTP status (+ optional Retry-After secs) to a TransportError.
///
/// This is the *canonical* classifier the design intends — it reads the real
/// status code and Retry-After header. It is reserved for a transport that
/// talks raw HTTP itself (JMAP, or a future Gmail rewrite that stops returning
/// `String`). Today's wrappers delegate to the existing `crate::backend::gmail::wire::*`
/// functions, which return `String`, so they use `classify_str` instead. Kept
/// (not deleted) so the next backend author has the correct contract in place;
/// `#[allow(dead_code)]` because it has no caller until that backend lands.
#[allow(dead_code)]
pub(crate) fn classify_status(status: u16, retry_after: Option<u64>, body: &str) -> TransportError {
    match status {
        429 => TransportError::RateLimited { retry_after: retry_after.map(Duration::from_secs) },
        401 | 403 => TransportError::Auth,
        404 => TransportError::NotFound,
        409 => TransportError::Conflict { remote_etag: None },
        500..=599 => TransportError::Transient { source: anyhow::anyhow!("HTTP {}: {}", status, body) },
        _ => TransportError::Permanent { source: anyhow::anyhow!("HTTP {}: {}", status, body) },
    }
}

/// Classify an error string produced by the existing crate::gmail functions,
/// which embed " HTTP {status}" / " {status}" markers. Bridge classifier used by
/// the current wrappers; see `classify_status` for the canonical status-based
/// form a raw-HTTP transport should use.
pub(crate) fn classify_str(err: &str) -> TransportError {
    let has = |code: &str| err.contains(code);
    if has(" 401") || has("UNAUTHENTICATED") || has("Invalid Credentials") || has(" 403") {
        TransportError::Auth
    } else if has(" 429") {
        TransportError::RateLimited { retry_after: None }
    } else if has(" 404") {
        TransportError::NotFound
    } else if has(" 409") {
        TransportError::Conflict { remote_etag: None }
    } else if has(" 500") || has(" 502") || has(" 503") || has(" 504") {
        TransportError::Transient { source: anyhow::anyhow!("{}", err) }
    } else {
        TransportError::Permanent { source: anyhow::anyhow!("{}", err) }
    }
}

#[cfg(test)]
mod classify_tests {
    use super::*;
    #[test]
    fn classify_str_maps_known_codes() {
        assert!(matches!(classify_str("Save failed 401: x"), TransportError::Auth));
        assert!(matches!(classify_str("HTTP 404 (id=z)"), TransportError::NotFound));
        assert!(matches!(classify_str("foo 503 bar"), TransportError::Transient { .. }));
        assert!(matches!(classify_str("weird 418"), TransportError::Permanent { .. }));
    }
}

// ── Transport impl ───────────────────────────────────────────────────────────

use crate::backend::{
    ChangeKind, ChangeSet, NoteStore, RemoteChange, RemoteFolder, SaveOp, SaveOutcome,
    SyncCursor, Transport,
};
use super::GmailVertical;
use async_trait::async_trait;

#[async_trait]
impl Transport for GmailVertical {
    async fn changes_since(&self, _cursor: Option<&SyncCursor>) -> Result<ChangeSet, TransportError> {
        // Gmail full-scans today (no cursor wired). Every Notes message id as an
        // Upsert with an inert cursor. The worker is NOT yet driven by this
        // (Pragmatic scope) — it exists so the surface is real and JMAP can
        // implement a real cursor without changing the trait.
        let idx = crate::backend::gmail::wire::list_account_index(&self.token, &self.user_email, &self.label_map)
            .await.map_err(|e| classify_str(&e))?;
        let changes = idx.into_iter().map(|m| RemoteChange {
            remote_id: m.id, kind: ChangeKind::Upserted, folder_hint: Some(m.label),
        }).collect();
        Ok(ChangeSet { changes, next_cursor: SyncCursor::default(), more: false })
    }

    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError> {
        let saved = self.save_note_full(&op, &[]).await?;
        Ok(SaveOutcome { remote_id: saved.id, cursor_hint: None })
    }

    async fn delete(&self, remote_id: &str) -> Result<(), TransportError> {
        crate::backend::gmail::wire::delete_note(&self.token, &self.user_email, remote_id)
            .await.map_err(|e| classify_str(&e))
    }

    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError> {
        // Intentionally fetches a FRESH label map rather than reusing
        // `self.label_map`: this is a backend-discovery primitive (the generic
        // sync loop wants current backend state), unlike ensure_folder/save
        // which resolve a known path from the construction-time snapshot.
        let map = crate::backend::gmail::wire::get_label_map(&self.token).await.map_err(|e| classify_str(&e))?;
        Ok(map.into_iter()
            .filter(|(_, n)| n == "Notes" || n.starts_with("Notes/"))
            .map(|(id, path)| RemoteFolder { id, path })
            .collect())
    }

    async fn ensure_folder(&self, path: &str) -> Result<RemoteFolder, TransportError> {
        let id = crate::backend::gmail::wire::ensure_label(&self.token, path, &self.label_map)
            .await.map_err(|e| classify_str(&e))?;
        Ok(RemoteFolder { id, path: path.to_string() })
    }

    async fn create_folder(&self, name: &str) -> Result<RemoteFolder, TransportError> {
        let info = crate::backend::gmail::wire::create_label(&self.token, name).await.map_err(|e| classify_str(&e))?;
        Ok(RemoteFolder { id: info.id, path: info.name })
    }

    async fn rename_folder(&self, id: &str, new_name: &str) -> Result<(), TransportError> {
        crate::backend::gmail::wire::rename_label(&self.token, id, new_name).await.map_err(|e| classify_str(&e))
    }

    async fn delete_folder(&self, id: &str) -> Result<(), TransportError> {
        crate::backend::gmail::wire::delete_label(&self.token, id).await.map_err(|e| classify_str(&e))
    }

    async fn move_note(&self, remote_id: &str, add: &[String], remove: &[String]) -> Result<(), TransportError> {
        crate::backend::gmail::wire::modify_message_labels(&self.token, remote_id, add, remove)
            .await.map_err(|e| classify_str(&e))
    }
}

// ── MetadataSidecar impl ─────────────────────────────────────────────────────

use crate::backend::{MetadataSidecar, SidecarKind, SidecarRecord};

#[async_trait]
impl MetadataSidecar for GmailVertical {
    async fn list_sidecars(&self, kind: SidecarKind) -> Result<Option<Vec<SidecarRecord>>, TransportError> {
        let meta_id = match self.label_map.iter().find(|(_, n)| n.as_str() == self.meta_label) {
            // Meta-label not yet created on Gmail — signal "store not initialized"
            // so callers skip pruning local state rather than wiping it.
            Some((id, _)) => id.clone(),
            None => return Ok(None),
        };
        match kind {
            SidecarKind::Pin => {
                let refs = crate::backend::gmail::wire::list_meta_sidecars(&self.token, &meta_id)
                    .await.map_err(|e| classify_str(&e))?;
                Ok(Some(refs.into_iter().map(|r| SidecarRecord {
                    id: r.id,
                    note_uuid: r.note_uuid,
                    kind: SidecarKind::Pin,
                    body: None,
                }).collect()))
            }
            SidecarKind::Tags => {
                let refs = crate::backend::gmail::wire::list_tag_sidecars(&self.token, &meta_id)
                    .await.map_err(|e| classify_str(&e))?;
                Ok(Some(refs.into_iter().map(|r| {
                    let body = serde_json::to_vec(&serde_json::json!({"tags": r.tags})).ok();
                    SidecarRecord { id: r.id, note_uuid: r.note_uuid, kind: SidecarKind::Tags, body }
                }).collect()))
            }
        }
    }

    async fn put_sidecar(&self, note_uuid: &str, kind: SidecarKind, body: Option<&[u8]>, replace: Option<&str>) -> Result<String, TransportError> {
        let meta_id = crate::backend::gmail::wire::ensure_label(&self.token, &self.meta_label, &self.label_map)
            .await.map_err(|e| classify_str(&e))?;
        let payload = body
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| "{}".to_string());
        match kind {
            SidecarKind::Pin => crate::backend::gmail::wire::save_meta_sidecar(
                &self.token, note_uuid, &payload, &meta_id, replace, &self.user_email,
            ).await,
            SidecarKind::Tags => crate::backend::gmail::wire::save_tag_sidecar(
                &self.token, note_uuid, &payload, &meta_id, replace, &self.user_email,
            ).await,
        }.map_err(|e| classify_str(&e))
    }

    async fn remove_sidecar(&self, id: &str) -> Result<(), TransportError> {
        crate::backend::gmail::wire::trash_meta_sidecar(&self.token, id).await.map_err(|e| classify_str(&e))
    }
}
