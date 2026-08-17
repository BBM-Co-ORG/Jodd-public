//! Vertical #2 — Apple-via-Microsoft. Milestone 1 is READ ONLY.
//!
//! This is not "Gmail against another host". Apple syncs notes to an Outlook.com
//! account over **Exchange**, so the items are `IPM.StickyNote` objects, not
//! email: none of Apple's `X-` headers exist (they were never sent), identity is
//! `internetMessageId`, `mime822.rs` is unused, and `PATCH` is a real in-place
//! update so the Gmail insert-new + trash-old + `mark_pushed` dance has no
//! counterpart here. See docs/superpowers/specs/2026-08-14-microsoft-vertical-design.md.
//!
//! Everything the write path needs (`save`, folder create/rename/move, sidecars)
//! returns `TransportError::Permanent` naming Milestone 2 rather than a silent
//! no-op.

use super::{Capabilities, Vertical};

pub mod wire;
pub mod transport;

pub struct MicrosoftVertical {
    pub(crate) token: String,
    pub(crate) account_id: String,
    capabilities: Capabilities,
    /// Jodd folder path → Exchange folder id, from the local `folders` table's
    /// `label_id`. Exchange folders cannot be enumerated (gotcha #12), so this
    /// map — populated by an earlier full scan and cached in SQLite — is the
    /// only way a scoped read can address a folder directly instead of
    /// paginating the whole mailbox. See `wire::folder_read_plan`.
    folder_ids: HashMap<String, String>,
    /// At most ONE mailbox scan per vertical instance.
    ///
    /// `list_notes` calls `list_all_notes` and then `list_folders`, which used
    /// to be two complete `/me/messages` paginations. Sharing one scan halves
    /// that and, as a second effect, removes the `folder_paths` divergence
    /// between the two calls that the ledger previously recorded as accepted:
    /// within one instance both now read the same map by construction.
    ///
    /// A vertical is constructed per operation (`vertical_for`), so this cache
    /// cannot go stale across sync ticks — it dies with the instance.
    scan: tokio::sync::OnceCell<wire::Scan>,
}

impl MicrosoftVertical {
    pub fn new(token: String, account_id: String, folder_ids: HashMap<String, String>) -> Self {
        // has_trash: false — see the doc comment on `Capabilities::has_trash`
        // (backend/mod.rs) for what was and wasn't verified on 2026-08-14.
        Self {
            token,
            account_id,
            capabilities: Capabilities::for_backend(crate::accounts::BackendKind::Microsoft),
            folder_ids,
            scan: tokio::sync::OnceCell::new(),
        }
    }

    /// The instance's one mailbox scan, fetched on first use.
    ///
    /// Always `with_body = true`. The alternative — caching the light and full
    /// variants separately — would let `list_all_notes` and `list_folders` scan
    /// twice again, which is the thing this exists to stop. `list_index` pays
    /// for bodies it does not need as a result; that is one scan's worth of
    /// extra bytes on an explicit user action, against a whole extra
    /// pagination on every sync tick.
    pub(crate) async fn scan(&self) -> Result<&wire::Scan, TransportError> {
        self.scan.get_or_try_init(|| wire::scan(&self.token, true)).await
    }
}

impl Vertical for MicrosoftVertical {
    fn backend_id(&self) -> &str { "apple-via-microsoft" }
    fn capabilities(&self) -> &Capabilities { &self.capabilities }
}

impl Identity for MicrosoftVertical {
    /// Mints a **placeholder** uuid for a locally-created note, minted the
    /// same way Gmail does — local-first writes the row before any request
    /// to Exchange returns. This is NOT the note's eventual identity: Exchange
    /// assigns the real one (`internetMessageId`) only when `save_note_full`'s
    /// `create_note` call returns, and `push_one_dirty` (lib.rs) rekeys the
    /// cached row from this placeholder to that real identity via
    /// `Db::rekey_note_uuid` right after.
    fn mint(&self) -> String { crate::mime822::format_apple_uuid(uuid::Uuid::new_v4()) }
}

impl Deriver for MicrosoftVertical {
    /// Exchange hands back clean semantic HTML (`<b> <i> <u> <strike>`, no
    /// inline styling), so the shared Apple-HTML deriver applies unchanged and
    /// search/tags/edges span this backend and Gmail identically.
    fn derive(&self, kind: ContentKind, blob: &[u8]) -> Derived {
        crate::backend::deriver_applehtml::AppleHtmlDeriver.derive(kind, blob)
    }
}

use crate::backend::{
    Attachment, ContentKind, DedupSummary, Derived, Deriver, Identity, MessageIndex, Note,
    NoteStore, SaveOp, SavedNote, TrashedNote, TransportError,
};
use async_trait::async_trait;
use std::collections::HashMap;

/// The error every Milestone 2 method returns. A single spelling so a caller
/// (and a log reader) can tell "not built yet" from "the backend failed".
pub(crate) fn milestone_2() -> TransportError {
    TransportError::Permanent {
        source: anyhow::anyhow!(
            "write path lands in Milestone 2 — see docs/superpowers/specs/2026-08-14-microsoft-vertical-design.md"
        ),
    }
}

#[async_trait]
impl NoteStore for MicrosoftVertical {
    /// `cache_by_id` is unused, unlike Gmail's: a Graph list response already
    /// carries `body`, so there is no per-message GET to skip.
    ///
    /// **If a future change makes this read `cache_by_id` as a fan-out
    /// optimization** (natural, given Graph's per-request cost), do not let a
    /// cache hit fill `Note::version` from `CachedNote::to_frontend_note`'s
    /// `remote_version` without checking that field's own doc comment first:
    /// it is safe for Gmail (`id == version` there) but NOT generally safe,
    /// and reusing it uncritically on Microsoft reintroduces the exact
    /// silent-overwrite bug this task exists to eliminate — `remote_changed`
    /// would read `false` for a cache-hit note no matter what actually
    /// changed on Graph, with no test failing to catch it.
    ///
    /// `DedupSummary` is always empty. Gmail re-mints a message id on every
    /// content edit, which is what produces the transient duplicates it dedups;
    /// Exchange `PATCH`es in place, so a uuid never has two live messages.
    async fn list_all_notes(
        &self,
        _cache_by_id: &HashMap<String, Note>,
    ) -> Result<(Vec<Note>, DedupSummary), TransportError> {
        let scan = self.scan().await?;
        Ok((wire::to_notes(scan, &self.account_id), DedupSummary::default()))
    }

    /// Addresses `/me/mailFolders/{id}/messages` — ONE folder, not the mailbox.
    ///
    /// This is the hot path: `App.svelte` sweeps one folder every 2500 ms, so a
    /// whole-mailbox pagination here (with bodies, since notes need content)
    /// becomes hundreds of requests a minute on a real account and Graph
    /// answers 429. `wire::folder_read_plan` makes the choice and hands back
    /// the exact URL, so which endpoint is hit is a value a test can assert on.
    ///
    /// The folder path is turned into an Exchange folder id by `folder_ids`,
    /// which `vertical_for` fills from the `folders` table. A path that map
    /// does not know falls back to the instance's cached mailbox scan — bounded
    /// to one scan per operation and never to an empty list, since an empty
    /// list would let `prune_clean_in_label` delete the folder's cached rows.
    ///
    /// An unknown folder yields an empty Vec, never `NotFound` — the trait's
    /// callers rely on empty for a folder that exists locally but has no remote
    /// representation.
    ///
    /// That invariant covers a *stale-but-cached* folder id too, not just a
    /// path `folder_ids` never learned. `folder_ids` is a snapshot from the
    /// last full scan; if the folder was deleted server-side since (via
    /// Jodd's own `delete_folder` or externally) before the local `folders`
    /// row was pruned, the scoped `GET /me/mailFolders/{id}/messages` 404s —
    /// see `wire::folder_still_exists`'s doc comment for why that endpoint's
    /// 404 is trustworthy evidence of deletion, not a transient hiccup. The
    /// App.svelte 2500 ms sweep calls this on every poll tick, so surfacing
    /// that as a hard error would repeat it forever instead of the graceful
    /// empty state this invariant promises — pruning the stale row itself is
    /// `list_folders`'s job, not this call's.
    async fn list_notes_in_folder(
        &self,
        folder: &str,
        _cache_by_id: &HashMap<String, Note>,
    ) -> Result<Vec<Note>, TransportError> {
        match wire::folder_read_plan(&self.folder_ids, folder) {
            wire::FolderRead::Scoped(url) => {
                match wire::scan_folder(&self.token, url, folder).await {
                    Ok(scan) => Ok(wire::to_notes(&scan, &self.account_id)),
                    Err(TransportError::NotFound) => Ok(Vec::new()),
                    Err(e) => Err(e),
                }
            }
            wire::FolderRead::MailboxFallback => {
                crate::log!(
                    "microsoft: no Exchange folder id cached for '{}' — falling back to one \
                     mailbox scan for this operation",
                    folder
                );
                let scan = self.scan().await?;
                Ok(wire::to_notes(scan, &self.account_id)
                    .into_iter()
                    .filter(|n| n.label == folder)
                    .collect())
            }
        }
    }

    /// Shares the instance scan, so it carries bodies it does not need — see
    /// `MicrosoftVertical::scan`'s comment for why one cache beats two.
    async fn list_index(&self) -> Result<Vec<MessageIndex>, TransportError> {
        let scan = self.scan().await?;
        Ok(wire::to_index(scan))
    }

    /// Costs a full scan for one note. That is deliberate: the note's `label`
    /// has to come from the same folder-path map every other read produces, and
    /// a map built from one folder would disagree with it the moment two
    /// folders share a leaf name. Every caller (`refetch_note`,
    /// `preview_orphans`) is an explicit user action where latency is
    /// acceptable — see the doctrine's "explicit user-triggered refresh".
    async fn fetch_note(&self, remote_id: &str) -> Result<Note, TransportError> {
        let scan = self.scan().await?;
        scan.messages
            .iter()
            .find(|m| m.id == remote_id)
            .map(|m| wire::to_note(m, &scan.folder_paths, &self.account_id))
            .ok_or(TransportError::NotFound)
    }

    /// Create or update one note.
    ///
    /// `PATCH` is a real in-place update on this backend (measured, see
    /// `wire::patch_note`'s doc comment), so there is no insert-new +
    /// trash-old + id-repair dance the way Gmail needs one. The returned
    /// `uuid` is Exchange's `internetMessageId`, which differs from the
    /// caller's minted placeholder on a create — `push_one_dirty` (lib.rs)
    /// rekeys the cached row to it via `Db::rekey_note_uuid`.
    async fn save_note_full(&self, op: &SaveOp<'_>, attachments: &[Attachment]) -> Result<SavedNote, TransportError> {
        if !attachments.is_empty() {
            // Apple itself refuses attachments on Exchange accounts: a note is
            // a structured object with no multipart container (measured
            // 2026-08-14 — see this module's doc comment). Dropping them
            // silently here would discard user data behind a success result.
            return Err(TransportError::Permanent {
                source: anyhow::anyhow!(
                    "attachments are impossible on an Exchange account — Apple refuses \
                     them too. This note has {} and cannot be saved with them.",
                    attachments.len()
                ),
            });
        }

        let saved = match op.existing_remote_id {
            Some(id) => wire::patch_note(&self.token, id, op.title, op.body_html).await?,
            None => {
                let folder_id = self.folder_ids.get(op.label).cloned().ok_or_else(|| {
                    TransportError::Permanent {
                        source: anyhow::anyhow!(
                            "no Exchange folder id for '{}' — the folder must exist before \
                             a note can be filed into it",
                            op.label
                        ),
                    }
                })?;
                wire::create_note(&self.token, &folder_id, op.title, op.body_html).await?
            }
        };

        Ok(SavedNote {
            id: saved.id,
            // NOT saved.id — see `SavedNote::version`'s doc comment. `id` is
            // stable across a PATCH, so using it would make `Db::mark_pushed`
            // stamp `remote_version` with a value that never changes: every
            // push would look identical to the last one, and the *next* real
            // Apple-side edit would then also look unchanged and get
            // silently overwritten with no conflict copy — the pull-side bug
            // Task 1 fixed, reintroduced on the push side.
            version: saved.last_modified_date_time.clone(),
            uuid: saved.internet_message_id,
            date: wire::graph_time_to_rfc2822(&saved.last_modified_date_time),
            // EDITOR-VIEW, deliberately: the pull path stores post-strip
            // bodies, so storing the injected wire form here would make the
            // cache flip between "with title row" and "without" depending on
            // which side last touched the note.
            body_html: op.body_html.to_string(),
            local_version: 0,
        })
    }

    async fn find_ids_for_uuid(&self, _uuid: &str) -> Result<Vec<String>, TransportError> {
        Err(milestone_2())
    }

    /// This backend exposes no trash view — `Capabilities::has_trash` is
    /// false on measured evidence (see its doc comment: Deleted Items held
    /// zero items after an Apple-side delete, so no undo path exists to
    /// promise). An error here would surface to the user as a failure rather
    /// than as the empty state it actually is.
    async fn list_trashed(&self) -> Result<Vec<TrashedNote>, TransportError> {
        Ok(Vec::new())
    }

    async fn untrash(&self, _remote_id: &str) -> Result<(), TransportError> {
        Err(milestone_2())
    }
}

#[cfg(test)]
mod vertical_tests {
    use super::*;
    use crate::backend::Vertical;

    #[test]
    fn identifies_itself_and_reports_no_trash() {
        let v = MicrosoftVertical::new("tok".into(), "kaiwan.h@live.com".into(), Default::default());
        assert_eq!(v.backend_id(), "apple-via-microsoft");
        assert!(!v.capabilities().has_trash,
            "measured 2026-08-14 (M2): after an Apple-side delete, Deleted Items held zero \
             items and the note was absent from every scan — there is genuinely no undo path");
    }

    #[tokio::test]
    async fn an_account_with_no_trash_reports_an_empty_list_not_an_error() {
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), Default::default());
        assert!(v.list_trashed().await.unwrap().is_empty(),
            "an error here would show the user a failure instead of an empty state");
    }

    #[test]
    fn the_deriver_is_the_shared_apple_html_one() {
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), Default::default());
        let d = v.derive(ContentKind::AppleHtml, b"<div>hello #work [[Other Note-abcd1234]]</div>");
        assert!(d.tags.contains(&"work".to_string()));
        assert_eq!(d.edges.len(), 1);
        assert_eq!(d.edges[0].rel, "mentions");
    }

    #[test]
    fn mint_returns_a_fresh_apple_shaped_uuid() {
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), Default::default());
        assert_ne!(v.mint(), v.mint());
    }

    /// C1, proven over a real socket rather than asserted.
    ///
    /// `folder_read_plan` is unit-tested next door, but a test on the plan alone
    /// cannot fail if `list_notes_in_folder` builds the plan and then ignores it
    /// — which is exactly the shape of defect this branch has been punished for.
    /// So this one runs the actual method against a local HTTP server and looks
    /// at the request line it receives.
    ///
    /// The two cases share one server (via `server_returning`, below) purely
    /// for convenience — nothing requires it. `wire::set_graph_base_for_test`'s
    /// override is per-THREAD
    /// (as of Task 5, which added several more live-socket tests in
    /// `wire.rs` that each need their own redirect target — a process-wide
    /// set-once value would have made every test after the first silently
    /// talk to whichever test's server won the race), so these two scenarios
    /// could just as well be two separate tests today.
    ///
    /// The call to `list_notes_in_folder` below MUST stay directly in this
    /// test's body, never inside a spawned task: `#[tokio::test]`'s default
    /// current-thread runtime is what keeps `TEST_GRAPH_BASE` visible to the
    /// client call at all. See `wire.rs`'s `TEST_GRAPH_BASE` comment for what
    /// breaks if that changes.
    #[tokio::test]
    async fn a_scoped_read_hits_the_folder_endpoint_and_only_falls_back_when_the_id_is_unknown() {
        // An empty page: decodes, reports no nextLink, ends the walk.
        let (addr, seen) = server_returning(r#"{"value":[]}"#).await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));

        let folder_ids: HashMap<String, String> =
            [("Notes".to_string(), "AAMkA/Dg+X0=".to_string())].into_iter().collect();
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), folder_ids);
        v.list_notes_in_folder("Notes", &HashMap::new()).await.expect("scoped read");

        let scoped: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert!(
            scoped.iter().any(|l| l.contains("/me/mailFolders/AAMkA%2FDg%2BX0%3D/messages")),
            "the scoped read must address the folder itself; saw {scoped:?}"
        );
        assert!(
            !scoped.iter().any(|l| l.contains("/me/messages?")),
            "a 2500 ms UI sweep must never paginate the whole mailbox; saw {scoped:?}"
        );

        // Same call, no cached id for the path: the honest answer is the
        // mailbox, not an empty list (which would let prune_clean_in_label
        // delete every clean row under the folder).
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), HashMap::new());
        v.list_notes_in_folder("Notes", &HashMap::new()).await.expect("fallback read");

        let fallback: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert!(
            fallback.iter().any(|l| l.contains("/me/messages?")),
            "an unknown path must fall back to the mailbox scan; saw {fallback:?}"
        );
    }

    /// The regression this test guards: a stale cached Exchange folder id
    /// (the folder was deleted server-side after the last full scan) must
    /// read as "no notes here", exactly like an unknown path does — never
    /// as a hard error surfaced to the frontend on every poll tick.
    #[tokio::test]
    async fn a_stale_cached_folder_id_reads_as_empty_not_notfound() {
        let (addr, _seen) = server_returning_404().await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));

        let folder_ids: HashMap<String, String> =
            [("Notes/Gone".to_string(), "STALE-ID".to_string())].into_iter().collect();
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), folder_ids);

        let notes = v.list_notes_in_folder("Notes/Gone", &HashMap::new())
            .await
            .expect("a stale folder id must not surface as an error");
        assert!(notes.is_empty());
    }

    /// A local server that answers every request `404`, for the
    /// stale-folder-id test above. Distinct from `server_returning` (always
    /// `200`) rather than parameterizing it — the two shapes are used by
    /// exactly one test each and a status parameter would obscure both.
    async fn server_returning_404() -> (std::net::SocketAddr, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                if let Some(line) = req.lines().next() {
                    recorder.lock().unwrap().push(line.to_string());
                }
                let body = r#"{"error":{"code":"ErrorItemNotFound","message":"gone"}}"#;
                let resp = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, seen)
    }

    /// A local server that answers every request with the same fixed body,
    /// recording each request's start line — the one recording-listener
    /// shared by every test in this module that needs a live socket rather
    /// than `list_notes_in_folder`'s bespoke one above. Most callers (the
    /// `SavedNote`-shaped tests below) only care about the decoded result and
    /// pass a fixed body regardless of method/path;
    /// `save_note_full_dispatches_create_and_patch_to_the_right_endpoint_and_method`
    /// below is the one that reads `seen` back, to prove *which* request the
    /// dispatch actually sent.
    async fn server_returning(body: &str) -> (std::net::SocketAddr, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let recorder = Arc::clone(&seen);
        let body = body.to_string();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                if let Some(line) = req.lines().next() {
                    recorder.lock().unwrap().push(line.to_string());
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, seen)
    }

    #[tokio::test]
    async fn creating_a_note_returns_the_identity_exchange_assigned() {
        // Jodd mints a placeholder uuid because local-first writes the row before
        // the POST returns. The SavedNote must carry the REAL one so the caller
        // can rekey; otherwise the next pull inserts a second row.
        let (addr, _seen) = server_returning(r#"{
            "id":"GRAPH-ID","internetMessageId":"<real@exchange>",
            "subject":"T","body":{"contentType":"HTML","content":"<html><body>T</body></html>"},
            "parentFolderId":"F","createdDateTime":"2026-08-14T10:00:00Z",
            "lastModifiedDateTime":"2026-08-14T10:00:00Z"
        }"#).await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));

        let v = MicrosoftVertical::new("tok".into(), "acct".into(),
            [("Notes".to_string(), "F".to_string())].into_iter().collect());
        let op = SaveOp { title: "T", body_html: "<div>b</div>",
            existing_remote_id: None, existing_uuid: Some("temp-minted"),
            existing_created_date: None, label: "Notes" };

        let saved = v.save_note_full(&op, &[]).await.unwrap();
        assert_eq!(saved.id, "GRAPH-ID");
        // `raw_to_graph_message` strips exactly one layer of RFC 5322 angle
        // brackets (decode_tests::decodes_identity_folder_name_and_a_title_
        // stripped_body asserts the same thing) — the wire fixture carries
        // them because that is the real Graph shape, but the value that
        // reaches SavedNote.uuid, and therefore the DB's PK, is unbracketed.
        assert_eq!(saved.uuid, "real@exchange", "must be Exchange's id, not the minted one");
        assert_eq!(saved.version, "2026-08-14T10:00:00Z", "must be lastModifiedDateTime, not the stable id");
        assert_eq!(saved.body_html, "<div>b</div>", "editor-view, not the injected wire form");
    }

    /// `creating_a_note_returns_the_identity_exchange_assigned` only inspects
    /// the DECODED `SavedNote`, and `attachments_are_refused…` returns before
    /// dispatch ever runs — neither reaches `wire::patch_note`, and
    /// `server_returning`'s canned response is identical regardless of which
    /// method or path actually arrived, so neither test would notice the
    /// match arms being swapped. This one reads `seen` back to pin the
    /// dispatch itself: no `existing_remote_id` must POST to the folder's
    /// messages endpoint and never PATCH; `Some(id)` must PATCH that message
    /// directly and never POST (a POST there would be a second, duplicate
    /// create).
    #[tokio::test]
    async fn save_note_full_dispatches_create_and_patch_to_the_right_endpoint_and_method() {
        let (addr, seen) = server_returning(r#"{
            "id":"GRAPH-ID","internetMessageId":"<x@y>",
            "subject":"T","body":{"contentType":"HTML","content":"<html><body>T</body></html>"},
            "parentFolderId":"F","createdDateTime":"2026-08-14T10:00:00Z",
            "lastModifiedDateTime":"2026-08-14T10:00:00Z"
        }"#).await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));

        let v = MicrosoftVertical::new("tok".into(), "acct".into(),
            [("Notes".to_string(), "F".to_string())].into_iter().collect());

        // No existing_remote_id → CREATE: POST to the folder's endpoint.
        let create_op = SaveOp { title: "T", body_html: "<div>b</div>",
            existing_remote_id: None, existing_uuid: Some("temp"),
            existing_created_date: None, label: "Notes" };
        v.save_note_full(&create_op, &[]).await.unwrap();
        let create_reqs: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert!(
            create_reqs.iter().any(|l| l.starts_with("POST") && l.contains("/me/mailFolders/F/messages")),
            "no existing_remote_id must POST to the folder's messages endpoint; saw {create_reqs:?}"
        );
        assert!(
            !create_reqs.iter().any(|l| l.starts_with("PATCH")),
            "a create must never PATCH; saw {create_reqs:?}"
        );

        // existing_remote_id: Some(id) → PATCH that message directly, no
        // folder lookup involved at all.
        let update_op = SaveOp { title: "T2", body_html: "<div>b2</div>",
            existing_remote_id: Some("EXISTING-ID"), existing_uuid: Some("u"),
            existing_created_date: None, label: "Notes" };
        v.save_note_full(&update_op, &[]).await.unwrap();
        let update_reqs: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert!(
            update_reqs.iter().any(|l| l.starts_with("PATCH") && l.contains("/me/messages/EXISTING-ID")),
            "existing_remote_id must PATCH that message directly; saw {update_reqs:?}"
        );
        assert!(
            !update_reqs.iter().any(|l| l.starts_with("POST")),
            "an update must never POST — that would be a second, duplicate create; saw {update_reqs:?}"
        );
    }

    #[tokio::test]
    async fn attachments_are_refused_rather_than_silently_dropped() {
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), Default::default());
        let op = SaveOp { title: "T", body_html: "", existing_remote_id: Some("X"),
            existing_uuid: Some("u"), existing_created_date: None, label: "Notes" };
        let att = Attachment { content_id: "cid".into(), mime_type: "image/png".into(),
            filename: Some("a.png".into()), x_apple_part_url: None, data: vec![1, 2, 3] };
        let err = v.save_note_full(&op, std::slice::from_ref(&att)).await.unwrap_err();
        assert!(matches!(err, TransportError::Permanent { .. }));
        assert!(err.to_string().to_lowercase().contains("attachment"), "got: {err}");
    }

    /// The second half of finding 3 (fix round 1's review): a create whose
    /// `label` has no cached Exchange folder id. Needs no server — the error
    /// returns from the `folder_ids.get` miss before any dispatch (`wire::
    /// create_note` is never reached), unlike every other test in this
    /// module that exercises `save_note_full`.
    #[tokio::test]
    async fn creating_into_an_unknown_folder_names_the_label_rather_than_failing_generically() {
        // Empty folder_ids: "Notes" (or any label) has no cached Exchange id.
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), Default::default());
        let op = SaveOp { title: "T", body_html: "<div>b</div>",
            existing_remote_id: None, existing_uuid: Some("temp"),
            existing_created_date: None, label: "Notes" };

        let err = v.save_note_full(&op, &[]).await.unwrap_err();
        assert!(matches!(err, TransportError::Permanent { .. }), "got: {err:?}");
        // Discriminates against a generic/unhelpful message: a user hitting
        // this needs to know WHICH folder Jodd couldn't resolve.
        assert!(err.to_string().contains("Notes"), "must name the offending label; got: {err}");
    }
}
