//! Studio orchestration: the map step per batch, the reduce step, and writing the note.
//!
//! Tauri-free and shaped like `ingest::run::ingest_to_note`: it calls the AI
//! (one direct call, or map → reduce), checks the citations, and writes the
//! new note itself — one synchronous SQLite insert at the very end, so a
//! cancel or any failed call writes nothing (spec §5.4 step 4, §5.5).

use std::collections::HashMap;

use tokio_util::sync::CancellationToken;

use super::cite::{resolve_keys, sources_markdown};
use super::plan::{self, Batch, Source};
use super::{StudioOutput, STUDIO_MARKER};
use crate::accounts::BackendKind;
use crate::db::{self, CachedNote, Db};
use crate::llm::markdown;
use crate::llm::provider::{ExtractEnvelope, ExtractError, LlmProvider, WorkflowKind};
use crate::llm::transcript;
use crate::provenance::Actor;

pub mod msg {
    pub const AT_LIMIT: &str = "Stopped at the AI limit. Untick some notes, or raise AI limits in Settings.";
    pub const CUT_OFF: &str = "The AI's reply was cut off or unreadable. Raise Output tokens in AI limits and try again.";
}

pub struct StudioRequest {
    pub account_id: String,
    pub backend_kind: BackendKind,
    /// The research folder. The note lands here, never in a subfolder.
    pub label: String,
    pub output: StudioOutput,
    pub focus: String,
    pub map_cap: usize,
    /// `AI limits → Output tokens`, the value the plan's estimate used: `pack`
    /// bounds each map batch by what a reply this long can carry.
    pub output_tokens: u64,
    pub existing_tags: Vec<String>,
    pub actor: Actor,
    /// `YYYY-MM-DD`, for the marker line.
    pub today: String,
}

/// Counts only, never note text (spec §5.4 step 5).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum StudioProgress {
    Reading { total: usize },
    /// One map call; `index` is 1-based.
    Summarizing { index: usize, total: usize },
    /// The direct call or the reduce call.
    Writing,
    Saving,
    Done,
}

#[derive(Debug, thiserror::Error)]
pub enum StudioError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Ai(String),
    #[error("{0}")]
    Local(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StudioNote {
    pub uuid: String,
    pub label: String,
}

fn ai_error(e: ExtractError) -> StudioError {
    match e {
        ExtractError::Cancelled => StudioError::Cancelled,
        e if transcript::is_budget_exhausted(&e) => StudioError::Ai(msg::AT_LIMIT.into()),
        ExtractError::MalformedEnvelope { .. } => StudioError::Ai(msg::CUT_OFF.into()),
        ExtractError::Refused(s) => StudioError::Refused(s),
        e => StudioError::Ai(e.to_string()),
    }
}

/// One AI call. Any failure ends the run; an envelope with no body is a cut-off reply.
async fn call(
    provider: &dyn LlmProvider,
    kind: WorkflowKind,
    payload: &str,
    existing_tags: &[String],
    cancel: &CancellationToken,
) -> Result<ExtractEnvelope, StudioError> {
    if cancel.is_cancelled() {
        return Err(StudioError::Cancelled);
    }
    let envelope = provider.run_workflow(kind, payload, existing_tags, cancel.clone()).await.map_err(ai_error)?;
    if envelope.usable().is_err() {
        return Err(StudioError::Ai(msg::CUT_OFF.into()));
    }
    Ok(envelope)
}

/// The longest AI title kept (the prompt asks for at most 80).
const MAX_AI_TITLE_CHARS: usize = 120;

/// The AI's title as one short line: its first non-empty line, trimmed, cut
/// to `MAX_AI_TITLE_CHARS` characters (never inside one). `None` when blank.
fn ai_title(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(MAX_AI_TITLE_CHARS).collect::<String>().trim_end().to_string())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The plain-text lines above the output (spec §4 note layout, §5.7). The
/// first is the marker line Studio-note detection reads.
fn header_lines(req: &StudioRequest, sources: &[Source], batches: &[Batch], removed: usize) -> Vec<String> {
    let folder = req.label.strip_prefix("Notes/").unwrap_or(&req.label);
    let n = sources.len();
    let mut lines = vec![format!("{STUDIO_MARKER} from {n} note{} in {folder} · {}", plural(n), req.today)];
    let focus = req.focus.trim();
    if !focus.is_empty() {
        lines.push(format!("Focus: {focus}"));
    }
    for b in batches {
        if let Batch::Part { source, k: 1, n, .. } = b {
            lines.push(format!("“{}” was sent in {n} parts and summarised.", plan::header_title(&sources[*source].title)));
        }
    }
    if removed > 0 {
        lines.push(format!("{removed} citation{} pointed at notes that were not sent and were removed.", plural(removed)));
    }
    lines
}

/// Generate the output over `sources` and write it as a new note in
/// `req.label`. Nothing is written unless every call succeeded.
pub async fn generate(
    db: &Db,
    provider: &dyn LlmProvider,
    sources: Vec<Source>,
    req: &StudioRequest,
    cancel: CancellationToken,
    progress: &(dyn Fn(StudioProgress) + Send + Sync),
) -> Result<StudioNote, StudioError> {
    if sources.is_empty() {
        return Err(StudioError::Refused(plan::msg::NOTHING_SELECTED.into()));
    }
    let output = req.output;
    progress(StudioProgress::Reading { total: sources.len() });
    let batches = plan::pack(&sources, &req.label, req.map_cap, req.output_tokens);

    let envelope = if batches.is_empty() {
        progress(StudioProgress::Writing);
        let payload = plan::direct_payload(output, &req.focus, &sources, &req.label);
        call(provider, output.workflow(), &payload, &req.existing_tags, &cancel).await?
    } else {
        let mut digests = Vec::with_capacity(batches.len());
        for (i, batch) in batches.iter().enumerate() {
            progress(StudioProgress::Summarizing { index: i + 1, total: batches.len() });
            let payload = plan::digest_payload(output, &req.focus, batch, &sources, &req.label, req.output_tokens);
            digests.push(call(provider, WorkflowKind::StudioDigest, &payload, &[], &cancel).await?.lessons_markdown);
        }
        // The same measure the plan's estimate uses: there is no second reduce level.
        if plan::reduce_too_large(output, &req.focus, &digests, req.map_cap) {
            return Err(StudioError::Refused(plan::msg::TOO_MANY_TO_COMBINE.into()));
        }
        progress(StudioProgress::Writing);
        let payload = plan::reduce_payload(output, &req.focus, &digests);
        call(provider, output.workflow(), &payload, &req.existing_tags, &cancel).await?
    };

    if cancel.is_cancelled() {
        return Err(StudioError::Cancelled);
    }
    let _policy_gate = provider.mutation_guard().map_err(|_| StudioError::Cancelled)?;
    progress(StudioProgress::Saving);

    let keys: HashMap<String, String> = sources.iter().map(|s| (s.key.clone(), s.slug.clone())).collect();
    let resolved = resolve_keys(&envelope.lessons_markdown, &keys);
    let slugs: Vec<String> = sources.iter().map(|s| s.slug.clone()).collect();
    let body_html = markdown::assemble_studio_body(
        &header_lines(req, &sources, &batches, resolved.removed_keys),
        &resolved.markdown,
        &sources_markdown(&slugs),
        &envelope.tags,
        db::tag_model_for_account(&req.account_id),
    );
    let leaf = req.label.rsplit('/').next().unwrap_or(&req.label);
    let named = envelope.title.as_deref().and_then(ai_title).unwrap_or_else(|| leaf.to_string());
    let title = format!("{} — {named}", output.title_prefix());

    let uuid = crate::backend::mint_uuid_for(req.backend_kind);
    let now = db::now_ms();
    db.insert_local_new(&CachedNote {
        uuid: uuid.clone(),
        account_id: req.account_id.clone(),
        id: String::new(),
        title,
        body_html,
        date: chrono::Local::now().to_rfc2822(),
        x_mail_created_date: None,
        label: req.label.clone(),
        local_version: 1,
        remote_version: None,
        sync_state: db::SyncState::Dirty,
        last_synced_at: None,
        last_local_modified_at: now,
        last_remote_modified_at: None,
        pinned: false,
        meta_msg_id: None,
        pin_dirty: false,
        push_blocked_reason: None,
        push_blocked_by_remote: false,
    }, &req.actor)
    .map_err(|e| StudioError::Local(format!("insert_local_new: {e}")))?;

    progress(StudioProgress::Done);
    Ok(StudioNote { uuid, label: req.label.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use crate::llm::provider::{CandidateSummary, ChatTurn, FolderSuggestionEnvelope, LinkSuggestionsEnvelope, SourceDigest};
    use crate::privacy::ai_scope::AiScope;
    use crate::test_support::{note, temp_db};

    const A: &str = "gmail:studio@example.com";
    const R: &str = "Notes/Research/EV";
    const OT: u64 = 4096;

    fn env(title: Option<&str>, md: &str) -> ExtractEnvelope {
        ExtractEnvelope { title: title.map(Into::into), lessons_markdown: md.into(), meta_lessons_markdown: None, tags: vec![], confidence: None }
    }

    /// Records `(kind, payload)` per call and answers from a queue; an empty
    /// queue answers a cited envelope. `cancel_on` cancels the run at that call.
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(WorkflowKind, String)>>,
        queue: Mutex<VecDeque<Result<ExtractEnvelope, ExtractError>>>,
        cancel_on: Option<usize>,
    }

    impl Fake {
        fn answering(answers: Vec<Result<ExtractEnvelope, ExtractError>>) -> Self {
            Fake { queue: Mutex::new(answers.into()), ..Default::default() }
        }
        fn kinds(&self) -> Vec<WorkflowKind> {
            self.calls.lock().unwrap().iter().map(|(k, _)| *k).collect()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for Fake {
        async fn extract(&self, _s: &str, _t: &[String], _c: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("Studio never calls extract")
        }
        async fn run_workflow(&self, w: WorkflowKind, s: &str, _t: &[String], c: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            let n = {
                let mut calls = self.calls.lock().unwrap();
                calls.push((w, s.to_string()));
                calls.len() - 1
            };
            if self.cancel_on == Some(n) {
                c.cancel();
                return Err(ExtractError::Cancelled);
            }
            self.queue.lock().unwrap().pop_front().unwrap_or_else(|| Ok(env(Some("X"), "## Out\n\nA fact [S1].")))
        }
        async fn synthesize(&self, _d: &[SourceDigest], _c: &str, _t: CancellationToken) -> Result<ExtractEnvelope, ExtractError> {
            unreachable!("Studio never calls synthesize")
        }
        async fn suggest_links(&self, _s: &str, _c: &[CandidateSummary], _t: CancellationToken) -> Result<LinkSuggestionsEnvelope, ExtractError> {
            unreachable!("Studio never calls suggest_links")
        }
        async fn suggest_folder(&self, _t: &str, _f: &[String], _c: CancellationToken) -> Result<FolderSuggestionEnvelope, ExtractError> {
            unreachable!("Studio never calls suggest_folder")
        }
        async fn chat(&self, _s: &str, _t: &[ChatTurn], _c: CancellationToken) -> Result<String, ExtractError> {
            unreachable!("Studio never calls chat")
        }
    }

    fn uuid(i: usize) -> String {
        format!("00000000-0000-4000-8000-{i:012x}")
    }

    /// `texts.len()` notes in `R` (or `labels[i]`), oldest first, as `load_sources` keys them.
    fn seed(db: &Db, texts: &[&str], labels: &[&str]) -> Vec<Source> {
        let uuids: Vec<String> = (0..texts.len()).map(uuid).collect();
        for (i, t) in texts.iter().enumerate() {
            let label = labels.get(i).copied().unwrap_or(R);
            note(A, &uuids[i]).label(label).title(&format!("Source {i}")).body(&format!("<div>{t}</div>")).modified_ms(1_000 + i as i64).insert(db);
        }
        plan::load_sources(db, &AiScope::hiding(A, &[]), A, R, &uuids).unwrap()
    }

    fn request(output: StudioOutput, map_cap: usize) -> StudioRequest {
        StudioRequest {
            account_id: A.into(),
            backend_kind: BackendKind::Gmail,
            label: R.into(),
            output,
            focus: String::new(),
            map_cap,
            output_tokens: OT,
            existing_tags: vec![],
            actor: Actor::Workflow { name: output.workflow().slug().into(), model: "m".into() },
            today: "2026-10-23".into(),
        }
    }

    async fn run(db: &Db, p: &Fake, sources: Vec<Source>, req: &StudioRequest) -> Result<StudioNote, StudioError> {
        generate(db, p, sources, req, CancellationToken::new(), &|_| {}).await
    }

    fn body(db: &Db, uuid: &str) -> String {
        db.get(uuid, A).unwrap().unwrap().body_html
    }

    /// Nothing new: the seeded sources only, and only their own create events.
    fn assert_nothing_written(db: &Db, seeded: usize) {
        assert_eq!(db.list_notes(A).unwrap().len(), seeded, "no note was written");
        assert_eq!(db.note_event_count(A), seeded, "no event was written");
    }

    const MEDIUM: &str = "Chargers cost more in the north than in the south this year. ";

    #[tokio::test]
    async fn a_small_folder_is_one_call_with_the_output_kind() {
        let db = temp_db();
        let sources = seed(&db, &["one", "two"], &[]);
        let p = Fake::default();
        run(&db, &p, sources, &request(StudioOutput::Briefing, 60_000)).await.unwrap();
        assert_eq!(p.kinds(), vec![WorkflowKind::StudioBriefing]);
        let payload = &p.calls.lock().unwrap()[0].1;
        assert!(payload.starts_with("Output: Briefing\n") && payload.contains("[S2]"), "{payload}");
    }

    #[tokio::test]
    async fn a_large_folder_maps_then_reduces() {
        let db = temp_db();
        let texts = vec![MEDIUM.repeat(3); 4];
        let sources = seed(&db, &texts.iter().map(String::as_str).collect::<Vec<_>>(), &[]);
        let batches = plan::pack(&sources, R, 400, OT).len();
        assert!(batches >= 2);
        let p = Fake::default();
        run(&db, &p, sources, &request(StudioOutput::Timeline, 400)).await.unwrap();
        let mut want = vec![WorkflowKind::StudioDigest; batches];
        want.push(WorkflowKind::StudioTimeline);
        assert_eq!(p.kinds(), want);
    }

    #[tokio::test]
    async fn digest_keys_reach_the_reduce() {
        let db = temp_db();
        let texts = vec![MEDIUM.repeat(3); 3];
        let sources = seed(&db, &texts.iter().map(String::as_str).collect::<Vec<_>>(), &[]);
        let batches = plan::pack(&sources, R, 400, OT).len();
        let mut answers: Vec<_> = (0..batches).map(|i| Ok(env(None, &format!("- digest {i} fact [S1]")))).collect();
        answers.push(Ok(env(Some("X"), "## Out\n\nfinal [S1]")));
        let p = Fake::answering(answers);
        run(&db, &p, sources, &request(StudioOutput::Faq, 400)).await.unwrap();
        let calls = p.calls.lock().unwrap();
        let (kind, reduce) = calls.last().unwrap();
        assert_eq!(*kind, WorkflowKind::StudioFaq);
        assert!(reduce.contains("- digest 0 fact [S1]") && reduce.contains("--- BEGIN DIGESTS ---"), "{reduce}");
    }

    #[tokio::test]
    async fn cancel_writes_nothing() {
        let db = temp_db();
        let texts = vec![MEDIUM.repeat(3); 4];
        let sources = seed(&db, &texts.iter().map(String::as_str).collect::<Vec<_>>(), &[]);
        let p = Fake { cancel_on: Some(1), ..Default::default() };
        let err = run(&db, &p, sources, &request(StudioOutput::Briefing, 400)).await.unwrap_err();
        assert!(matches!(err, StudioError::Cancelled), "{err}");
        assert_eq!(p.kinds().len(), 2, "no call after the cancel");
        assert_nothing_written(&db, 4);
    }

    #[tokio::test]
    async fn any_failed_call_writes_nothing() {
        let budget = ExtractError::UpstreamError(format!("{}; no more", crate::llm::transcript::BUDGET_EXHAUSTED_PREFIX));
        let transport = ExtractError::Transport("boom".into());
        let malformed = ExtractError::MalformedEnvelope { reason: "cut".into(), raw: "{".into() };
        let texts = vec![MEDIUM.repeat(3); 3];
        let cases: Vec<(Vec<Result<ExtractEnvelope, ExtractError>>, &str)> = vec![
            (vec![Err(budget)], msg::AT_LIMIT),
            (vec![Ok(env(None, "- d")), Err(transport)], "transport error: boom"),
            ((0..3).map(|_| Ok(env(None, "- d"))).chain([Err(malformed)]).collect(), msg::CUT_OFF),
            (vec![Ok(env(None, "  "))], msg::CUT_OFF),
            (vec![Err(ExtractError::Refused("hidden".into()))], "hidden"),
        ];
        for (answers, want) in cases {
            let db = temp_db();
            let sources = seed(&db, &texts.iter().map(String::as_str).collect::<Vec<_>>(), &[]);
            assert_eq!(plan::pack(&sources, R, 400, OT).len(), 3, "one batch per note");
            let p = Fake::answering(answers);
            let err = run(&db, &p, sources, &request(StudioOutput::Briefing, 400)).await.unwrap_err();
            assert!(matches!(&err, StudioError::Ai(_) | StudioError::Refused(_)), "{err:?}");
            assert_eq!(err.to_string(), want);
            assert_nothing_written(&db, 3);
        }
    }

    #[tokio::test]
    async fn too_many_digests_refuse_before_the_reduce() {
        let db = temp_db();
        let texts = vec![MEDIUM.repeat(3); 3];
        let sources = seed(&db, &texts.iter().map(String::as_str).collect::<Vec<_>>(), &[]);
        let p = Fake::answering((0..3).map(|_| Ok(env(None, &"long digest ".repeat(20)))).collect());
        let err = run(&db, &p, sources, &request(StudioOutput::Briefing, 400)).await.unwrap_err();
        assert_eq!(err.to_string(), plan::msg::TOO_MANY_TO_COMBINE);
        assert!(!p.kinds().contains(&WorkflowKind::StudioBriefing), "no reduce call");
        assert_nothing_written(&db, 3);
    }

    #[tokio::test]
    async fn the_note_lands_in_the_research_folder_untitled_body() {
        let db = temp_db();
        let sources = seed(&db, &["top", "sub"], &[R, "Notes/Research/EV/deep"]);
        let p = Fake::answering(vec![Ok(env(Some("X"), "## Out\n\nfact [S2]"))]);
        let made = run(&db, &p, sources, &request(StudioOutput::Briefing, 60_000)).await.unwrap();
        assert_eq!(made.label, R);
        let row = db.get(&made.uuid, A).unwrap().unwrap();
        assert_eq!((row.label.as_str(), row.title.as_str(), row.sync_state, row.local_version), (R, "Briefing — X", db::SyncState::Dirty, 1));
        assert!(!row.body_html.contains("Briefing — X"), "{}", row.body_html);
        assert!(row.body_html.contains("from 2 notes in Research/EV · 2026-10-23"), "{}", row.body_html);

        // No AI title: the folder's leaf.
        let sources = plan::load_sources(&db, &AiScope::hiding(A, &[]), A, R, &[uuid(0)]).unwrap();
        let p = Fake::answering(vec![Ok(env(Some("  "), "fact [S1]"))]);
        let made = run(&db, &p, sources, &request(StudioOutput::StudyGuide, 60_000)).await.unwrap();
        let row = db.get(&made.uuid, A).unwrap().unwrap();
        assert_eq!(row.title, "Study guide — EV");
        assert!(row.body_html.contains("from 1 note in Research/EV"), "{}", row.body_html);
    }

    #[tokio::test]
    async fn the_first_line_is_the_studio_marker() {
        let db = temp_db();
        let sources = seed(&db, &["one"], &[]);
        let mut req = request(StudioOutput::Briefing, 60_000);
        req.focus = "pricing".into();
        let p = Fake::answering(vec![Ok(ExtractEnvelope { tags: vec!["ev".into()], ..env(Some("X"), "fact [S1]") })]);
        let made = run(&db, &p, sources, &req).await.unwrap();
        let html = body(&db, &made.uuid);
        assert!(html.starts_with(&format!("<p>{STUDIO_MARKER} from 1 note in Research/EV · 2026-10-23</p>\n<p>Focus: pricing</p>\n<p>#ev</p>")), "{html}");
        assert!(db::strip_html_to_text(&html).lines().next().unwrap().starts_with(STUDIO_MARKER));

        // Synced to a fresh device: no local history, recognised by the line alone.
        let fresh = temp_db();
        let mut synced = note(A, &uuid(77)).label(R).title("Briefing — X").build();
        synced.body_html = html.clone(); // the cache holds the body without its title

        fresh.upsert_from_remote(&synced, db::RemotePin::LocalWins).unwrap();
        assert_eq!(fresh.note_event_count(A), 0);
        let (listed, _) = plan::list_candidates(&fresh, &AiScope::hiding(A, &[]), A, R).unwrap();
        assert!(listed.iter().find(|n| n.uuid == uuid(77)).unwrap().is_studio);
    }

    #[tokio::test]
    async fn citations_become_backlinks() {
        let db = temp_db();
        let sources = seed(&db, &["one", "two"], &[]);
        let p = Fake::answering(vec![Ok(env(Some("X"), "Fact two [S2]. Made up [S9]. See [[Other note]] and [[Third]]."))]);
        let made = run(&db, &p, sources.clone(), &request(StudioOutput::Briefing, 60_000)).await.unwrap();
        let back: Vec<String> = db.backlinks(A, &uuid(1)).unwrap().into_iter().map(|n| n.uuid).collect();
        assert!(back.contains(&made.uuid), "{back:?}");
        let html = body(&db, &made.uuid);
        assert!(!html.contains("S9"), "{html}");
        assert!(html.contains("See Other note and Third."), "AI-written links are unwrapped: {html}");
        assert!(html.contains("<p>1 citation pointed at notes that were not sent and were removed.</p>"), "only the unknown key counts: {html}");

        // AI-written links alone: unwrapped silently, no notice.
        let p = Fake::answering(vec![Ok(env(Some("X"), "Fact [S1]. See [[Other note]]."))]);
        let made = run(&db, &p, sources, &request(StudioOutput::Briefing, 60_000)).await.unwrap();
        let html = body(&db, &made.uuid);
        assert!(!html.contains("were removed"), "{html}");
    }

    #[tokio::test]
    async fn the_ai_title_is_one_short_line() {
        let db = temp_db();
        let sources = seed(&db, &["one"], &[]);
        let p = Fake::answering(vec![Ok(env(Some("\n  \n  EV chargers  \nsecond line"), "fact [S1]"))]);
        let made = run(&db, &p, sources.clone(), &request(StudioOutput::Briefing, 60_000)).await.unwrap();
        assert_eq!(db.get(&made.uuid, A).unwrap().unwrap().title, "Briefing — EV chargers");

        let long = format!("{}🔋{}", "ก".repeat(119), "x".repeat(180));
        let p = Fake::answering(vec![Ok(env(Some(&long), "fact [S1]"))]);
        let made = run(&db, &p, sources, &request(StudioOutput::Faq, 60_000)).await.unwrap();
        let title = db.get(&made.uuid, A).unwrap().unwrap().title;
        assert_eq!(title, format!("FAQ — {}🔋", "ก".repeat(119)), "cut to 120 characters, never inside one");
    }

    #[tokio::test]
    async fn every_sent_note_is_in_sources() {
        let db = temp_db();
        let sources = seed(&db, &["one", "two", "three"], &[]);
        let slugs: Vec<String> = sources.iter().map(|s| s.slug.clone()).collect();
        let p = Fake::answering(vec![Ok(env(Some("X"), "Only one [S1]."))]);
        let made = run(&db, &p, sources, &request(StudioOutput::Briefing, 60_000)).await.unwrap();
        let html = body(&db, &made.uuid);
        let (_, listed) = html.split_once("<h2>Sources</h2>").expect(&html);
        for slug in &slugs {
            assert!(listed.contains(&format!("[[{slug}]]")), "{slug} in {listed}");
        }
        assert!(!html.contains("were removed"), "no removed-citations line when none were removed: {html}");
    }

    #[tokio::test]
    async fn split_notes_say_so() {
        let db = temp_db();
        let big = "word ".repeat(100);
        let sources = seed(&db, &[&big, "small"], &[]);
        let batches = plan::pack(&sources, R, 400, OT);
        let parts = batches.iter().filter(|b| matches!(b, Batch::Part { source: 0, .. })).count();
        assert_eq!(parts, 2, "{batches:?}");
        let p = Fake::default();
        let made = run(&db, &p, sources, &request(StudioOutput::Briefing, 400)).await.unwrap();
        let html = body(&db, &made.uuid);
        assert!(html.contains("<p>“Source 0” was sent in 2 parts and summarised.</p>"), "{html}");
        assert!(!html.contains("“Source 1”"), "{html}");
    }

    #[tokio::test]
    async fn a_split_notes_huge_title_is_cut_in_its_notice() {
        let db = temp_db();
        let big = "word ".repeat(400);
        note(A, &uuid(0)).label(R).title(&"ห".repeat(5_000)).body(&format!("<div>{big}</div>")).modified_ms(1_000).insert(&db);
        let sources = plan::load_sources(&db, &AiScope::hiding(A, &[]), A, R, &[uuid(0)]).unwrap();
        let p = Fake::default();
        let made = run(&db, &p, sources, &request(StudioOutput::Briefing, 1_000)).await.unwrap();
        let html = body(&db, &made.uuid);
        let want = format!("<p>“{}…” was sent in ", "ห".repeat(200));
        assert!(html.contains(&want), "{}", html.chars().take(400).collect::<String>());
        let notice = html.lines().find(|l| l.contains("was sent in")).unwrap();
        assert!(!notice.contains(&"ห".repeat(201)), "the whole title is not repeated in the notice");
    }

    #[tokio::test]
    async fn the_actor_is_the_studio_workflow() {
        let db = temp_db();
        let sources = seed(&db, &["one"], &[]);
        let p = Fake::default();
        let made = run(&db, &p, sources, &request(StudioOutput::Faq, 60_000)).await.unwrap();
        let creators = db.creators_for(A, &[made.uuid.clone()]).unwrap();
        assert_eq!(creators.get(&made.uuid).map(String::as_str), Some("jodd-studio-faq/m"));
    }

    #[tokio::test]
    async fn progress_carries_no_text() {
        let db = temp_db();
        let texts = vec![format!("SECRETBODY {}", MEDIUM.repeat(3)); 3];
        let sources = seed(&db, &texts.iter().map(String::as_str).collect::<Vec<_>>(), &[]);
        let seen = Mutex::new(Vec::new());
        let p = Fake::default();
        generate(&db, &p, sources, &request(StudioOutput::Briefing, 400), CancellationToken::new(), &|e| seen.lock().unwrap().push(e))
            .await
            .unwrap();
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.first(), Some(&StudioProgress::Reading { total: 3 }));
        assert!(seen.contains(&StudioProgress::Summarizing { index: 1, total: 3 }));
        assert!(seen.contains(&StudioProgress::Summarizing { index: 3, total: 3 }));
        assert_eq!(&seen[seen.len() - 3..], &[StudioProgress::Writing, StudioProgress::Saving, StudioProgress::Done]);
        for e in &seen {
            let json = serde_json::to_string(e).unwrap();
            assert!(!json.contains("SECRETBODY") && !json.contains("Source "), "{json}");
        }
    }

    #[tokio::test]
    async fn the_run_batches_and_frames_exactly_as_the_plan_estimates() {
        let db = temp_db();
        let texts: Vec<String> = (0..12).map(|i| format!("{i} {}", "ภาษาไทย ".repeat(375))).collect();
        let sources = seed(&db, &texts.iter().map(String::as_str).collect::<Vec<_>>(), &[]);
        let req = request(StudioOutput::Briefing, 20_000);
        let limits = crate::llm::budget::Settings { output_tokens: req.output_tokens, ..Default::default() };
        let batches = plan::pack(&sources, R, req.map_cap, req.output_tokens);
        assert_eq!(batches.len(), 4, "three digests per reply, not the six notes the input cap would allow");
        let e = plan::estimate(req.output, &req.focus, &sources, R, req.map_cap, &limits, &[]);
        assert_eq!((e.calls, e.refusal.as_deref()), (batches.len() + 1, None));

        let p = Fake::default();
        run(&db, &p, sources.clone(), &req).await.unwrap();
        let calls = p.calls.lock().unwrap();
        assert_eq!(calls.len(), e.calls, "the run makes the calls the plan counted");
        let sent: Vec<&str> = calls.iter().filter(|(k, _)| *k == WorkflowKind::StudioDigest).map(|(_, p)| p.as_str()).collect();
        let planned: Vec<String> =
            batches.iter().map(|b| plan::digest_payload(req.output, &req.focus, b, &sources, R, req.output_tokens)).collect();
        assert_eq!(sent, planned.iter().map(String::as_str).collect::<Vec<_>>());
    }
}
