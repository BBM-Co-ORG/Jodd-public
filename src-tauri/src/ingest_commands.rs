//! URL ingest: analyse pasted links, discover and pick link candidates,
//! render a source, and `ingest_sources`. Moved out of `lib.rs` 2026-11-16.

use super::*;

// docs/superpowers/specs/2026-09-15-url-ingest-design.md

#[derive(serde::Serialize)]
pub struct DuplicateOwnerDto {
    pub(crate) uuid: String,
    pub(crate) title: String,
}

#[derive(serde::Serialize)]
pub struct IngestSourceDto {
    pub(crate) url: String,
    pub(crate) kind: &'static str,
    pub(crate) supported: bool,
    pub(crate) reason: Option<String>,
    pub(crate) duplicate_owner: Option<DuplicateOwnerDto>,
    /// A video link whose audio speech-to-text would hear when the platform has
    /// no transcript (spec §5.5) — whether or not speech-to-text is set up.
    pub(crate) may_transcribe: bool,
    /// Its comments need a login: the modal offers a paste box (spec 2026-10-28 D8).
    pub(crate) comments_closed: bool,
}

#[derive(serde::Serialize)]
pub struct IngestAnalysisDto {
    pub(crate) sources: Vec<IngestSourceDto>,
    pub(crate) mostly_urls: bool,
    pub(crate) context_text: String,
    pub(crate) context_chars: usize,
    /// Where audio would be sent (D5), `None` when speech-to-text is not set
    /// up or its address is not confirmed.
    pub(crate) stt_host: Option<String>,
    /// That address is plain `http`: the modal adds "(not encrypted)".
    pub(crate) stt_insecure: bool,
}

/// Links whose audio may go to speech-to-text: the four social kinds. The
/// fetched page decides whether there is a video (a Facebook link's
/// `og:video`, an X post's media), never the link's shape — so this is also
/// the one gate `ingest_sources` reads speech-to-text by.
pub(crate) fn may_transcribe(kind: &crate::ingest::urls::UrlKind) -> bool {
    crate::ingest::urls::social_kind(kind).is_some()
}

/// The note, other than `excluded`, that already cites `url`. A note cites
/// the canonical link (D12) — with or without the trailing slash, which
/// Facebook's `og:url` adds and a pasted link often lacks.
pub(crate) fn ingest_duplicate_owner(db: &db::Db, account_id: &str, url: &str, excluded: &[String]) -> Result<Option<DuplicateOwnerDto>, String> {
    for form in crate::ingest::urls::citation_forms(url) {
        if let Some(n) = db.find_citation_owner_excluding(account_id, &form, excluded).map_err(|e| e.to_string())? {
            return Ok(Some(DuplicateOwnerDto { uuid: n.uuid, title: n.title }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod ingest_analysis_tests {
    /// `og:url` gives Facebook's canonical form a trailing slash; a link pasted
    /// without it is the same post.
    #[test]
    fn a_duplicate_matches_with_or_without_the_trailing_slash() {
        let db = crate::test_support::temp_db();
        let acct = "gmail:x@example.com";
        let a = "AAAAAAAA-0000-0000-0000-00000000000A";
        crate::test_support::note(acct, a).body("URL: https://www.facebook.com/reel/1120404190712037/").insert(&db);
        let owner = |url: &str| super::ingest_duplicate_owner(&db, acct, url, &[]).unwrap().map(|o| o.uuid);
        assert_eq!(owner("https://www.facebook.com/reel/1120404190712037").as_deref(), Some(a));
        assert_eq!(owner("https://www.facebook.com/reel/1120404190712037/?mibextid=x").as_deref(), Some(a));
        assert_eq!(owner("https://www.facebook.com/reel/112040419071203"), None);
        assert_eq!(super::ingest_duplicate_owner(&db, acct, "https://www.facebook.com/reel/1120404190712037", &[a.to_string()]).unwrap().map(|o| o.uuid), None);

        let b = "BBBBBBBB-0000-0000-0000-00000000000B";
        crate::test_support::note(acct, b).body("URL: https://x.com/a/status/1").insert(&db);
        assert_eq!(owner("https://x.com/a/status/1/?s=20").as_deref(), Some(b));
    }
}

/// What the Extract modal's "Sources from links" section shows. No network.
///
/// `is_html`: existing-note mode passes a `body_html`. URLs are detected in
/// the RAW text (an `href` is a link even when its text is a title); the
/// context and the Decision 9 count use the stripped text.
#[tauri::command]
pub fn analyze_ingest_sources(
    account_id: String,
    text: String,
    is_html: bool,
    exclude_uuids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<IngestAnalysisDto, String> {
    use crate::ingest::urls::{self, UrlKind};
    // Held uuids may predate a rekey (gotcha #16).
    let excluded: Vec<String> = exclude_uuids
        .iter()
        .map(|u| state.db.resolve_note_uuid(u, &account_id).unwrap_or_else(|_| u.clone()))
        .collect();
    let plain = if is_html { db::strip_html_to_text(&text) } else { text.clone() };
    let mut sources = Vec::new();
    for url in urls::detect(&text) {
        let classified = urls::classify(&url);
        let may_transcribe = may_transcribe(&classified);
        let (kind, supported, reason) = match classified {
            UrlKind::Unsupported(r) => ("unsupported", false, Some(r)),
            ref other => (urls::kind_name(other), true, None),
        };
        let duplicate_owner = ingest_duplicate_owner(&state.db, &account_id, &url, &excluded)?;
        let comments_closed = supported && urls::comments_closed(&url, &urls::classify(&url));
        sources.push(IngestSourceDto { url, kind, supported, reason, duplicate_owner, may_transcribe, comments_closed });
    }
    let context_text = urls::context_text(&plain);
    // The config file only: the keychain read that sending audio needs must not
    // happen here, because this runs as the user types (gotcha #15).
    let stt = stt_config::load();
    let stt_host = stt.as_ref().and_then(stt_config::confirmed_display_host);
    let stt_insecure = stt.as_ref().is_some_and(stt_config::confirmed_insecure);
    Ok(IngestAnalysisDto {
        sources,
        mostly_urls: urls::is_mostly_urls(&plain),
        context_chars: context_text.chars().count(),
        context_text,
        stt_host,
        stt_insecure,
    })
}

#[derive(serde::Serialize)]
pub struct LinkCandidateDto {
    #[serde(flatten)]
    pub(crate) candidate: crate::ingest::links::LinkCandidate,
    /// An existing note already cites this link.
    pub(crate) duplicate_owner: Option<DuplicateOwnerDto>,
}

#[derive(serde::Serialize)]
pub struct LinkDiscoveryDto {
    pub(crate) url: String,
    pub(crate) title: Option<String>,
    pub(crate) candidates: Vec<LinkCandidateDto>,
    pub(crate) not_shown: usize,
    pub(crate) skipped: Vec<crate::ingest::links::SkippedCount>,
}

/// "Find links" on one source (spec 2026-10-28 §5): read that page — and only
/// that page, plus a few short-link hops — for the links inside its content,
/// ranked, the best pre-ticked. An explicit user action, so it may touch the
/// network; nothing it finds is fetched until the user ingests. Cancellable
/// through `cancel_extraction(request_id)`. No AI and no keychain.
#[tauri::command]
pub async fn discover_link_candidates(
    account_id: String,
    url: String,
    // Links already in this ingest: never offered again.
    exclude_urls: Vec<String>,
    // Notes whose own citations do not count as "already in a note".
    exclude_uuids: Vec<String>,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<LinkDiscoveryDto, String> {
    log!("discover_link_candidates: {} request_id={request_id}", crate::ingest::urls::log_form(&url));
    let cancel = tokio_util::sync::CancellationToken::new();
    state.in_flight_extracts.lock().unwrap().insert(request_id.clone(), cancel.clone());
    // A YouTube link's author comments need the saved key: the keychain is
    // read only then, off the async runtime (gotcha #15).
    let youtube_comments = if matches!(crate::ingest::urls::classify(&url), crate::ingest::urls::UrlKind::YouTube { .. }) {
        tokio::task::spawn_blocking(youtube_key::ready).await.unwrap_or(None)
    } else {
        None
    };
    let readers = crate::ingest::links::Readers { youtube_comments, ..Default::default() };
    let found = crate::ingest::links::discover(&url, &exclude_urls, &readers, crate::ingest::FetchPolicy::default(), &cancel).await;
    state.in_flight_extracts.lock().unwrap().remove(&request_id);
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    let found = found?;
    let excluded: Vec<String> = exclude_uuids
        .iter()
        .map(|u| state.db.resolve_note_uuid(u, &account_id).unwrap_or_else(|_| u.clone()))
        .collect();
    let mut candidates = Vec::with_capacity(found.candidates.len());
    for c in found.candidates {
        let duplicate_owner = ingest_duplicate_owner(&state.db, &account_id, &c.url, &excluded)?;
        candidates.push(LinkCandidateDto { candidate: c, duplicate_owner });
    }
    log!("discover_link_candidates: {} candidates, {} not shown", candidates.len(), found.not_shown);
    Ok(LinkDiscoveryDto { url: found.url, title: found.title, candidates, not_shown: found.not_shown, skipped: found.skipped })
}

/// What the private reader saw (spec 2026-10-28 §7.3): enough for the source
/// row to say what was read before the user makes the note.
#[derive(serde::Serialize)]
pub struct RenderedSourceDto {
    pub url: String,
    pub title: Option<String>,
    pub words: usize,
}

/// "Open in a private reader" (spec 2026-10-28 §7.3, P4): render one web page
/// that came back "no readable text" in a hidden, throwaway webview with no
/// IPC, and keep its HTML for the `ingest_sources` call that names it in
/// `rendered`. Only ever on this button — never automatic (§8). Desktop only.
/// Cancellable through `cancel_extraction(request_id)`.
#[tauri::command]
pub async fn render_source_for_ingest(
    url: String,
    request_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<RenderedSourceDto, String> {
    #[cfg(mobile)]
    {
        let _ = (url, request_id, app, state);
        Err(crate::ingest::render::DESKTOP_ONLY.to_string())
    }
    #[cfg(desktop)]
    {
        // Keyed like `ingest_sources` keys its links, so the ingest finds it.
        let url = crate::ingest::urls::canonical_unique(std::slice::from_ref(&url)).into_iter().next().ok_or("not a valid link")?;
        if !matches!(crate::ingest::urls::classify(&url), crate::ingest::urls::UrlKind::Web) {
            return Err("The private reader is for web pages; this link has its own reader.".into());
        }
        log!("render_source_for_ingest: {} request_id={request_id}", crate::ingest::urls::log_form(&url));
        let cancel = tokio_util::sync::CancellationToken::new();
        state.in_flight_extracts.lock().unwrap().insert(request_id.clone(), cancel.clone());
        let result = crate::ingest::render::render(&app, &url, &cancel).await;
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let page = result?;
        let readable = crate::ingest::web::extract_readable(&page.html);
        if readable.text.trim().is_empty() {
            return Err("Even rendered, the page shows no readable text — it may need a sign-in.".into());
        }
        let dto = RenderedSourceDto { url: url.clone(), title: readable.title, words: readable.text.split_whitespace().count() };
        crate::ingest::render::remember(page);
        Ok(dto)
    }
}

/// "Let AI choose" (spec 2026-10-28 §5.4): the account's AI picks the links
/// worth following among those already listed under one source. It can only
/// re-tick listed links (`links::parse_picks`); every URL leaves as its
/// `display_url`. A user action, through the same gates as any AI call:
/// receipts and budget, hidden folders (`AiScope`), the account's AI
/// permission and outbound masking (`checked_account_provider`). Cancellable
/// through `cancel_extraction(request_id)`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn pick_link_candidates(
    account_id: String,
    page_url: String,
    page_title: Option<String>,
    context_text: String,
    candidates: Vec<crate::ingest::links::PickInput>,
    request_id: String,
    source: crate::privacy::ai_scope::AiSource,
    state: State<'_, AppState>,
) -> Result<Vec<crate::ingest::links::Pick>, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "pick_links", async {
        llm::receipts::source_version(account_id.as_bytes());
        crate::privacy::ai_scope::AiScope::load()?.check_source(&state.db, &account_id, &source)?;
        if candidates.len() > crate::ingest::links::MAX_CANDIDATES_SHOWN {
            return Err(format!("At most {} links can be judged at once.", crate::ingest::links::MAX_CANDIDATES_SHOWN));
        }
        let account = {
            let list = state.accounts.lock().unwrap();
            list.iter().find(|a| a.id == account_id).cloned().ok_or_else(|| format!("account not found: {account_id}"))?
        };
        let provider = checked_account_provider(&state, &account).map_err(|e| e.to_string())?;
        let cancel = tokio_util::sync::CancellationToken::new();
        state.in_flight_extracts.lock().unwrap().insert(request_id.clone(), cancel.clone());
        log!("pick_link_candidates: {} candidates={} request_id={request_id}", crate::ingest::urls::log_form(&page_url), candidates.len());
        let result = crate::ingest::links::ai_pick(provider.as_ref(), page_title.as_deref(), &page_url, &context_text, &candidates, cancel).await;
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        match result {
            Ok(picks) => {
                log!("pick_link_candidates: {} picked", picks.len());
                Ok(picks)
            }
            Err(llm::provider::ExtractError::Cancelled) => Err("cancelled".into()),
            Err(e) => Err(e.to_string()),
        }
    })
    .await
}

/// The links in what the user pasted from a source's comments (spec
/// 2026-10-28 D8), ranked like "Find links" — pasted links arrive ticked. No
/// network: the text is the user's own.
#[tauri::command]
pub fn pasted_link_candidates(
    account_id: String,
    from_url: String,
    text: String,
    exclude_urls: Vec<String>,
    exclude_uuids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<LinkDiscoveryDto, String> {
    let mut exclude = exclude_urls;
    exclude.push(from_url.clone());
    let found = crate::ingest::links::pasted_candidates(&text, &exclude);
    let excluded: Vec<String> = exclude_uuids
        .iter()
        .map(|u| state.db.resolve_note_uuid(u, &account_id).unwrap_or_else(|_| u.clone()))
        .collect();
    let mut candidates = Vec::with_capacity(found.candidates.len());
    for c in found.candidates {
        let duplicate_owner = ingest_duplicate_owner(&state.db, &account_id, &c.url, &excluded)?;
        candidates.push(LinkCandidateDto { candidate: c, duplicate_owner });
    }
    Ok(LinkDiscoveryDto { url: from_url, title: None, candidates, not_shown: found.not_shown, skipped: found.skipped })
}

/// Shared by `ingest_sources` and the multi-source `re_extract_note`: resolve
/// the provider, run `ingest::run::ingest_to_note`, map its errors to the
/// strings the modal already understands (`"cancelled"`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_ingest_command(
    state: &State<'_, AppState>,
    account_id: &str,
    input: crate::ingest::run::IngestInput,
    context: String,
    title_override: Option<String>,
    destination: crate::ingest::run::IngestDestination,
    mode: crate::ingest::run::IngestMode,
    fetcher: crate::ingest::HttpFetcher,
    followed_from: std::collections::HashMap<String, String>,
    pasted: std::collections::HashMap<String, String>,
    cancel: tokio_util::sync::CancellationToken,
    progress: &(dyn Fn(crate::ingest::run::IngestProgress) + Send + Sync),
) -> Result<ExtractedNoteDto, String> {
    use crate::ingest::run::{ingest_to_note, map_input_cap, IngestError, IngestRequest};
    let account = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == account_id).cloned().ok_or_else(|| format!("account not found: {account_id}"))?
    };
    let provider = checked_account_provider(&state, &account).map_err(|e| e.to_string())?;
    let req = IngestRequest {
        account_id: account_id.to_string(),
        backend_kind: account.backend_kind,
        can_create_folders: account_can_create_folders(state, account_id),
        context,
        title_override,
        destination,
        map_cap: map_input_cap(crate::llm::resolve::prompt_delivery_for_account(&account), std::env::consts::OS),
        mode,
        clean_limits: crate::llm::transcript::CleanLimits::from_settings(&llm::budget::get_ai_limits().unwrap_or_default()),
        actor: Actor::workflow("ingest", &account),
        followed_from,
        pasted,
    };
    match ingest_to_note(&state.db, provider.as_ref(), &fetcher, input, &req, cancel, progress).await {
        Ok(note) => {
            log!("ingest: created note uuid={} in {}", note.uuid, note.label);
            Ok(ExtractedNoteDto { uuid: note.uuid, label: note.label })
        }
        Err(IngestError::Cancelled) => {
            log!("ingest: cancelled by user");
            Err("cancelled".to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Fetch the chosen links, condense each, combine them, write ONE note.
/// An explicit user-triggered remote operation, which the local-first
/// doctrine permits; the note itself is still one synchronous SQLite write.
/// Cancellable through the existing `cancel_extraction(request_id)`.
#[tauri::command]
pub async fn ingest_sources(
    account_id: String,
    urls: Vec<String>,
    context_text: String,
    title_override: Option<String>,
    request_id: String,
    on_progress: tauri::ipc::Channel<crate::ingest::run::IngestProgress>,
    mode: Option<crate::ingest::run::IngestMode>,
    source: crate::privacy::ai_scope::AiSource,
    // "Transcribe the audio myself" (D2); absent from an older frontend.
    transcribe_audio: Option<bool>,
    // Links ticked under a source (spec 2026-10-28 §5); absent from an older frontend.
    followed: Option<Vec<crate::ingest::links::FollowedLink>>,
    // What the user pasted from comments Jodd cannot read (spec 2026-10-28 D8).
    pasted: Option<Vec<crate::ingest::links::PastedComments>>,
    // Sources the user opened in the private reader (spec 2026-10-28 §7.3).
    rendered: Option<Vec<String>>,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "ingest_sources", async {
        llm::receipts::source_version(account_id.as_bytes());
        // D12: the note, the `cites` edge and the LLM see one string per post;
        // shares of one post collapse before the count check below.
        let urls = crate::ingest::urls::canonical_unique(&urls);
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        // Spec 2026-10-08 §4.4: in existing-note mode the links and context
        // come from a note's body. Checked before the in-flight entry is
        // registered and before any provider, so a refusal leaves nothing.
        crate::privacy::ai_scope::AiScope::load()?.check_source(&state.db, &account_id, &source)?;
        let mode = mode.unwrap_or_default();
        mode.validate()?;
        if urls.is_empty() {
            return Err("Pick at least one link to ingest.".into());
        }
        if urls.len() > crate::ingest::MAX_URLS_PER_INGEST {
            return Err(format!("At most {} links can be ingested at once.", crate::ingest::MAX_URLS_PER_INGEST));
        }
        let pasted = crate::ingest::links::plan_pasted(&urls, &pasted.unwrap_or_default())?;
        let (urls, followed_from) = crate::ingest::links::plan_followed(&urls, &followed.unwrap_or_default())?;
        log!(
            "ingest_sources: account={account_id} sources={} followed={} mode={mode:?} request_id={request_id}",
            urls.len(),
            followed_from.len()
        );
        // Once per run, off the async runtime: `ready()` reads the keychain
        // (gotcha #15), and only a link that may be transcribed needs it.
        let any_clip = urls.iter().any(|u| may_transcribe(&crate::ingest::urls::classify(u)));
        let stt = if any_clip {
            tokio::task::spawn_blocking(stt_config::ready).await.unwrap_or_else(|e| {
                // A panic or abort on the blocking pool; `ready()` itself logs nothing secret.
                log!("ingest_sources: reading the speech-to-text setting failed ({}); treating it as not set up", if e.is_panic() { "panicked" } else { "cancelled" });
                None
            })
        } else {
            None
        };
        // Spec 2026-10-28 §6.2: the saved YouTube key, read once and only when
        // a YouTube link is in the run.
        let any_youtube = urls.iter().any(|u| matches!(crate::ingest::urls::classify(u), crate::ingest::urls::UrlKind::YouTube { .. }));
        let youtube_comments = if any_youtube { tokio::task::spawn_blocking(youtube_key::ready).await.unwrap_or(None) } else { None };
        // Only pages rendered for a link in this run; keyed canonically, like `urls`.
        let rendered = crate::ingest::urls::canonical_unique(&rendered.unwrap_or_default());
        let rendered: Vec<String> = rendered.into_iter().filter(|u| urls.contains(u)).collect();
        let rendered = crate::ingest::render::cached(&rendered);
        if !rendered.is_empty() {
            log!("ingest_sources: {} source(s) read from the private reader", rendered.len());
        }
        let fetcher = crate::ingest::HttpFetcher {
            stt,
            prefer_own_transcription: transcribe_audio == Some(true),
            youtube_comments,
            rendered: std::sync::Arc::new(rendered),
            ..crate::ingest::HttpFetcher::default()
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        state.in_flight_extracts.lock().unwrap().insert(request_id.clone(), cancel.clone());
        let result = run_ingest_command(
            &state,
            &account_id,
            crate::ingest::run::IngestInput::Urls(urls),
            context_text,
            title_override,
            crate::ingest::run::IngestDestination::Resolve,
            mode,
            fetcher,
            followed_from,
            pasted,
            cancel,
            // A closed modal drops the channel; a failed send is not an ingest failure.
            &|p| {
                let _ = on_progress.send(p);
            },
        )
        .await;
        // One removal on every exit path — the orchestrator returns, never `?`s out of here.
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        result
    }).await
}
