//! Content-free execution telemetry. IDs are observability only, never AI eligibility.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Actual,
    Estimated,
    #[default]
    Unknown,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub provenance: Provenance,
}
impl Usage {
    pub fn from_response(raw: &str) -> Self {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
            return Self::default();
        };
        let input = v["usage"]["prompt_tokens"].as_u64();
        let output = v["usage"]["completion_tokens"].as_u64();
        Self {
            input_tokens: input,
            output_tokens: output,
            provenance: if input.is_some() || output.is_some() {
                Provenance::Actual
            } else {
                Provenance::Unknown
            },
        }
    }
}
/// Output remains in memory; only metadata is copied into a receipt.
#[derive(Debug)]
pub struct AiResult<T> {
    pub value: T,
    pub usage: Usage,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reported_usage_is_actual_and_absence_is_unknown() {
        let u = Usage::from_response(r#"{"usage":{"prompt_tokens":23,"completion_tokens":7}}"#);
        assert_eq!(u.input_tokens, Some(23));
        assert_eq!(u.output_tokens, Some(7));
        assert_eq!(u.provenance, Provenance::Actual);
        assert_eq!(Usage::from_response("{}"), Usage::default());
        assert_eq!(
            Usage::from_response(r#"{"usage":{"prompt_tokens":"secret"}}"#),
            Usage::default()
        );
    }
}

use super::provider::ExtractError;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Running,
    Succeeded,
    Partial,
    Failed,
    Cancelled,
    Interrupted,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Call {
    pub step_id: String,
    pub provider: String,
    pub model: Option<String>,
    pub model_source: String,
    pub stage: String,
    pub outcome: Outcome,
    pub latency_ms: u64,
    pub usage: Usage,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub kind: String,
    pub stage: String,
    pub outcome: Outcome,
    pub latency_ms: u64,
    pub scope_version: Option<String>,
    pub checks: Vec<String>,
    #[serde(default)]
    pub metrics: std::collections::BTreeMap<String, u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub run_id: String,
    pub started_ms: i64,
    pub prompt_version: String,
    pub steps: Vec<Step>,
    pub calls: Vec<Call>,
    pub storage_failed: bool,
}
#[derive(Serialize, Deserialize)]
struct Disk {
    retention_days: u16,
    receipts: Vec<Receipt>,
}
/// All mutations serialize through this store. Deleting a receipt invalidates
/// in-flight handles too: late completions cannot recreate deleted metadata.
pub struct Store {
    path: Option<PathBuf>,
    retention_days: u16,
    rows: Vec<Receipt>,
    requests: HashMap<String, String>,
}
impl Store {
    pub fn open(path: Option<PathBuf>) -> Result<Self, String> {
        let mut store = Self {
            path,
            retention_days: 30,
            rows: vec![],
            requests: HashMap::new(),
        };
        if let Some(p) = &store.path {
            match std::fs::read(p) {
                Ok(bytes) => {
                    let disk: Disk = serde_json::from_slice(&bytes)
                        .map_err(|_| "Receipt history could not be read".to_string())?;
                    store.retention_days = disk.retention_days.min(90);
                    store.rows = disk.receipts;
                    for r in &mut store.rows {
                        for s in &mut r.steps {
                            if s.outcome == Outcome::Running {
                                s.outcome = Outcome::Interrupted;
                            }
                        }
                        for c in &mut r.calls {
                            if c.outcome == Outcome::Running {
                                c.outcome = Outcome::Interrupted;
                            }
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("Receipt history could not be read".into()),
            }
        }
        store.prune();
        store.persist()?;
        Ok(store)
    }
    fn prune(&mut self) {
        let cutoff = crate::db::now_ms() - i64::from(self.retention_days.max(1)) * 86_400_000;
        self.rows.retain(|r| r.started_ms >= cutoff);
        if self.rows.len() > 200 {
            self.rows.drain(..self.rows.len() - 200);
        }
        self.requests
            .retain(|_, id| self.rows.iter().any(|r| &r.run_id == id));
    }
    fn persist(&mut self) -> Result<(), String> {
        self.prune();
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path.parent().ok_or("Receipt storage unavailable")?;
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            std::fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            let disk = Disk {
                retention_days: self.retention_days,
                receipts: if self.retention_days == 0 {
                    vec![]
                } else {
                    self.rows.clone()
                },
            };
            use std::io::Write;
            file.write_all(&serde_json::to_vec(&disk)?)?;
            file.as_file().sync_all()?;
            file.persist(path)?;
            Ok(())
        })();
        if result.is_err() {
            for r in &mut self.rows {
                r.storage_failed = true;
            }
            return Err("Receipt metadata could not be saved on this device".into());
        }
        Ok(())
    }
    pub fn list(&mut self, request: Option<&str>) -> Vec<Receipt> {
        let _ = self.persist();
        let id = request.and_then(|r| self.requests.get(&request_key(r)));
        self.rows
            .iter()
            .rev()
            .filter(|r| request.is_none() || id == Some(&r.run_id))
            .cloned()
            .collect()
    }
    pub fn delete(&mut self, id: Option<&str>) -> Result<(), String> {
        self.rows.retain(|r| id.is_some_and(|id| r.run_id != id));
        self.requests
            .retain(|_, id| self.rows.iter().any(|r| &r.run_id == id));
        self.persist()
    }
    pub fn retention(&mut self, days: u16) -> Result<(), String> {
        if ![0, 7, 30, 90].contains(&days) {
            return Err("Choose session only, 7, 30 or 90 days".into());
        }
        self.retention_days = days;
        if days == 0 {
            self.rows.clear();
            self.requests.clear();
        }
        self.persist()
    }
    pub fn export(&mut self) -> Result<String, String> {
        let rows: Vec<_> = self.list(None).into_iter().enumerate().map(|(i, r)| {
            // Export excludes timestamps, correlation IDs, scope fingerprints and
            // configured model IDs. No provider URL, error text or content exists.
            serde_json::json!({"run": i + 1, "prompt_version": r.prompt_version,
                "steps": r.steps.iter().enumerate().map(|(step, s)| serde_json::json!({"step":step + 1,"kind":s.kind,"outcome":s.outcome,"latency_ms":s.latency_ms,"checks":s.checks,"metrics":s.metrics})).collect::<Vec<_>>(),
                "calls":r.calls.iter().map(|c| serde_json::json!({"step":r.steps.iter().position(|s| s.id == c.step_id).map(|n| n + 1),"provider":c.provider,"stage":c.stage,"outcome":c.outcome,"latency_ms":c.latency_ms,"usage":c.usage})).collect::<Vec<_>>()})
        }).collect();
        serde_json::to_string_pretty(&rows).map_err(|_| "Receipt export failed".into())
    }
}
fn request_key(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
static STORE: OnceLock<Result<Arc<Mutex<Store>>, String>> = OnceLock::new();
pub fn store() -> Result<Arc<Mutex<Store>>, String> {
    STORE
        .get_or_init(|| {
            #[cfg(test)]
            let path = None;
            #[cfg(not(test))]
            let path = Some(
                crate::paths::data_base()
                    .ok_or("Receipt storage unavailable")?
                    .join("jodd/ai-receipts.json"),
            );
            Store::open(path).map(|s| Arc::new(Mutex::new(s)))
        })
        .clone()
}
#[derive(Clone)]
struct Context {
    store: Arc<Mutex<Store>>,
    run_id: String,
    step_id: String,
    salt: String,
}
tokio::task_local! { static CURRENT: Context; }
fn change(ctx: &Context, f: impl FnOnce(&mut Receipt)) {
    let mut s = ctx.store.lock().unwrap();
    if let Some(r) = s.rows.iter_mut().find(|r| r.run_id == ctx.run_id) {
        f(r);
        let _ = s.persist();
    }
}
/// Only static, code-owned labels may enter stored metadata.
pub fn stage(label: &'static str) {
    let _ = CURRENT.try_with(|c| {
        change(c, |r| {
            if let Some(s) = r.steps.iter_mut().find(|s| s.id == c.step_id) {
                s.stage = label.into();
            }
        })
    });
}
pub fn check(label: &'static str) {
    let _ = CURRENT.try_with(|c| {
        change(c, |r| {
            if let Some(s) = r.steps.iter_mut().find(|s| s.id == c.step_id) {
                if !s.checks.iter().any(|v| v == label) {
                    s.checks.push(label.into());
                }
            }
        })
    });
}
/// A per-step salted fingerprint, not anonymization or an authorization token.
/// Salt is memory-only; exports remove the fingerprint entirely.
pub fn metric(name: &'static str, value: usize) {
    let _ = CURRENT.try_with(|c| {
        change(c, |r| {
            if let Some(s) = r.steps.iter_mut().find(|s| s.id == c.step_id) {
                s.metrics.insert(name.into(), value as u64);
            }
        })
    });
}
fn prompt_version() -> String {
    let mut hash = Sha256::new();
    hash.update(include_str!("prompt.rs"));
    hash.update(include_str!("meeting.rs")); // action schema and evidence contract
    hash.update(include_str!("../ask/prompt.rs"));
    hash.update(include_str!("provider.rs")); // structured output schemas
    hash.update(include_str!("agent_cli.rs")); // repair nudge and CLI prompt framing
    format!("jodd-prompts-v1-{:x}", hash.finalize())
}
pub fn source_version(value: &[u8]) {
    let _ = CURRENT.try_with(|c| {
        change(c, |r| {
            if let Some(s) = r.steps.iter_mut().find(|s| s.id == c.step_id) {
                let mut hash = Sha256::new();
                hash.update(c.salt.as_bytes());
                if let Some(previous) = &s.scope_version {
                    hash.update(previous.as_bytes());
                }
                hash.update(value);
                s.scope_version = Some(format!("{:x}", hash.finalize()));
            }
        });
    });
}
pub async fn run<T>(
    request: &str,
    parent: Option<&str>,
    kind: &'static str,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    // Nested helpers remain in the admitted command's run.
    if CURRENT.try_with(|_| ()).is_ok() {
        return future.await;
    }
    super::budget::run(request, parent, async {
        let Ok(store) = store() else { return future.await; };
        run_in(store, request, parent, kind, future).await
    }).await
}
pub async fn run_in<T>(
    store: Arc<Mutex<Store>>,
    request: &str,
    parent: Option<&str>,
    kind: &'static str,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let step_id = uuid::Uuid::new_v4().to_string();
    let run_id = (|| {
        let mut s = store.lock().unwrap();
        s.prune();
        let parent_id = parent
            .and_then(|p| s.requests.get(&request_key(p)))
            .cloned();
        // Resolve parent and insert while holding the same lock as deletion.
        // A late child cannot recreate metadata deleted during its admission.
        if parent.is_some() && parent_id.is_none() {
            return None;
        }
        let id = parent_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if !s.rows.iter().any(|r| r.run_id == id) {
            s.rows.push(Receipt {
                run_id: id.clone(),
                started_ms: crate::db::now_ms(),
                prompt_version: prompt_version(),
                steps: vec![],
                calls: vec![],
                storage_failed: false,
            });
        }
        s.rows
            .iter_mut()
            .find(|r| r.run_id == id)
            .unwrap()
            .steps
            .push(Step {
                id: step_id.clone(),
                kind: kind.into(),
                stage: "admission".into(),
                outcome: Outcome::Running,
                latency_ms: 0,
                scope_version: None,
                checks: vec![],
                metrics: Default::default(),
            });
        s.requests.insert(request_key(request), id.clone());
        let _ = s.persist();
        Some(id)
    })();
    // This suppresses metadata only, never the existing permission checks.
    let Some(run_id) = run_id else {
        return future.await;
    };
    let ctx = Context {
        store,
        run_id,
        step_id,
        salt: uuid::Uuid::new_v4().to_string(),
    };
    let mut finish = StepGuard {
        ctx: ctx.clone(),
        start: Instant::now(),
        outcome: Outcome::Interrupted,
    };
    let result = CURRENT.scope(ctx, future).await;
    finish.outcome = match &result {
        Ok(_) => Outcome::Succeeded,
        Err(e)
            if e == "cancelled"
                || e.ends_with(": cancelled")
                || e.starts_with("AI request cancelled")
                || e.starts_with("AI permission or provider changed") =>
        {
            Outcome::Cancelled
        }
        Err(_) => Outcome::Failed,
    };
    result
}
struct StepGuard {
    ctx: Context,
    start: Instant,
    outcome: Outcome,
}
impl Drop for StepGuard {
    fn drop(&mut self) {
        change(&self.ctx, |r| {
            if let Some(s) = r.steps.iter_mut().find(|s| s.id == self.ctx.step_id) {
                s.outcome = if self.outcome == Outcome::Failed
                    && s.checks
                        .iter()
                        .any(|c| c == "permission_or_cancellation_refused")
                {
                    Outcome::Cancelled
                } else if self.outcome == Outcome::Succeeded
                    && s.checks.iter().any(|c| c == "workflow_result_failed")
                {
                    Outcome::Failed
                } else if self.outcome == Outcome::Succeeded
                    && s.checks.iter().any(|c| c == "partial_result")
                {
                    Outcome::Partial
                } else {
                    self.outcome
                };
                s.latency_ms = self.start.elapsed().as_millis() as u64;
            }
        });
    }
}
/// Physical dispatch guard: each HTTP retry / CLI subprocess gets one entry.
/// Dropping a waiting future records cancellation with unknown usage, never zero.
pub struct Attempt {
    ctx: Option<Context>,
    index: usize,
    start: Instant,
    outcome: Outcome,
    usage: Usage,
}
impl Attempt {
    pub fn start(provider: &'static str, model: Option<&str>) -> Self {
        let ctx = CURRENT.try_with(Clone::clone).ok();
        let mut index = 0;
        if let Some(c) = &ctx {
            change(c, |r| {
                index = r.calls.len();
                let stage = r
                    .steps
                    .iter()
                    .find(|s| s.id == c.step_id)
                    .map(|s| s.stage.clone())
                    .unwrap_or_default();
                // Arbitrary configured IDs can themselves contain secrets. Only a
                // closed set of public model identifiers is retained; never CLI guesses.
                let model = model
                    .filter(|m| {
                        [
                            "gpt-4o",
                            "gpt-4o-mini",
                            "gpt-4.1",
                            "gpt-4.1-mini",
                            "o3",
                            "o4-mini",
                        ]
                        .contains(m)
                    })
                    .map(str::to_string);
                r.calls.push(Call {
                    step_id: c.step_id.clone(),
                    provider: provider.into(),
                    model_source: if model.is_some() {
                        "configured"
                    } else {
                        "unknown_or_redacted"
                    }
                    .into(),
                    model,
                    stage,
                    outcome: Outcome::Running,
                    latency_ms: 0,
                    usage: Usage::default(),
                });
            });
        }
        Self {
            ctx,
            index,
            start: Instant::now(),
            outcome: Outcome::Cancelled,
            usage: Usage::default(),
        }
    }
    pub fn record_usage(&mut self, usage: Usage) {
        self.usage = usage;
    }
    #[cfg(test)]
    pub fn response(&mut self, raw: &str) {
        self.record_usage(Usage::from_response(raw));
    }
    pub fn finish<T>(&mut self, result: &Result<T, ExtractError>) {
        self.outcome = match result {
            Ok(_) => Outcome::Succeeded,
            Err(ExtractError::Cancelled) => Outcome::Cancelled,
            Err(_) => Outcome::Failed,
        };
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if let Some(c) = &self.ctx {
            change(c, |r| {
                if let Some(call) = r.calls.get_mut(self.index) {
                    call.outcome = self.outcome;
                    call.latency_ms = self.start.elapsed().as_millis() as u64;
                    call.usage = self.usage.clone();
                }
            });
        }
    }
}
#[tauri::command]
pub fn list_ai_receipts(request_id: Option<String>) -> Result<Vec<Receipt>, String> {
    Ok(store()?.lock().unwrap().list(request_id.as_deref()))
}
#[tauri::command]
pub fn delete_ai_receipts(run_id: Option<String>) -> Result<(), String> {
    store()?.lock().unwrap().delete(run_id.as_deref())
}
#[tauri::command]
pub fn set_ai_receipt_retention(days: u16) -> Result<(), String> {
    store()?.lock().unwrap().retention(days)
}
#[tauri::command]
pub fn get_ai_receipt_retention() -> Result<u16, String> {
    Ok(store()?.lock().unwrap().retention_days)
}
#[tauri::command]
pub fn export_ai_receipts() -> Result<String, String> {
    store()?.lock().unwrap().export()
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::llm::{
        http::HttpProvider,
        policy::CheckedProvider,
        provider::{ChatRole, ChatTurn, LlmProvider},
    };
    use tokio_util::sync::CancellationToken;
    fn memory() -> Arc<Mutex<Store>> {
        Arc::new(Mutex::new(Store::open(None).unwrap()))
    }
    #[tokio::test]
    async fn parent_children_fail_cancel_and_privacy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipts.json");
        let store = Arc::new(Mutex::new(Store::open(Some(path.clone())).unwrap()));
        run_in(store.clone(), "SECRET request", None, "extract", async {
            source_version(b"SECRET title body tags folder account credential");
            let mut attempt = Attempt::start("http", Some("SECRET credential"));
            attempt.response(r#"{"usage":{"prompt_tokens":12,"completion_tokens":4}}"#);
            attempt.finish(&Ok(()));
            Ok(())
        })
        .await
        .unwrap();
        let _: Result<(), String> = run_in(
            store.clone(),
            "child",
            Some("SECRET request"),
            "links",
            async {
                let mut call = Attempt::start("agent_cli", None);
                call.finish::<()>(&Err(ExtractError::UpstreamError(
                    "SECRET output credential".into(),
                )));
                Err("SECRET error credential".into())
            },
        )
        .await;
        let _: Result<(), String> = run_in(
            store.clone(),
            "cancel",
            Some("SECRET request"),
            "folder",
            async {
                let _call = Attempt::start("agent_cli", None);
                Err("cancelled".into())
            },
        )
        .await;
        let mut locked = store.lock().unwrap();
        let rows = locked.list(None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].steps.len(), 3);
        assert_eq!(rows[0].calls.len(), 3);
        assert_eq!(rows[0].steps[1].outcome, Outcome::Failed);
        assert_eq!(rows[0].steps[2].outcome, Outcome::Cancelled);
        assert_eq!(rows[0].calls[1].usage, Usage::default());
        let disk = std::fs::read_to_string(path).unwrap();
        assert!(!disk.contains("SECRET"));
        let export = locked.export().unwrap();
        let exported: serde_json::Value = serde_json::from_str(&export).unwrap();
        assert_eq!(exported[0]["calls"][1]["step"], 2);
        assert_eq!(exported[0]["steps"][1]["step"], 2);
        for forbidden in [
            "SECRET",
            "scope_version",
            "started_ms",
            rows[0].run_id.as_str(),
        ] {
            assert!(!export.contains(forbidden));
        }
    }
    #[tokio::test]
    async fn deletion_cannot_be_undone_by_late_completion_or_child() {
        let store = memory();
        run_in(store.clone(), "root", None, "extract", async {
            let _call = Attempt::start("http", None);
            store.lock().unwrap().delete(None).unwrap();
            Ok(())
        })
        .await
        .unwrap();
        run_in(store.clone(), "late child", Some("root"), "links", async {
            Ok(())
        })
        .await
        .unwrap();
        assert!(store.lock().unwrap().list(None).is_empty());
    }
    #[tokio::test]
    async fn aborted_future_records_interruption_and_unknown_usage() {
        let store = memory();
        let other = store.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            run_in(other, "abort", None, "ask", async {
                let _call = Attempt::start("agent_cli", None);
                tx.send(()).unwrap();
                std::future::pending::<Result<(), String>>().await
            })
            .await
        });
        rx.await.unwrap();
        task.abort();
        let _ = task.await;
        let row = store.lock().unwrap().list(None).remove(0);
        assert_eq!(row.steps[0].outcome, Outcome::Interrupted);
        assert_eq!(row.calls[0].outcome, Outcome::Cancelled);
        assert_eq!(row.calls[0].usage, Usage::default());
    }
    #[tokio::test]
    async fn retention_and_restart_prune_disk_and_session_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.json");
        let store = Arc::new(Mutex::new(Store::open(Some(path.clone())).unwrap()));
        run_in(store.clone(), "r", None, "ask", async { Ok(()) })
            .await
            .unwrap();
        {
            let mut s = store.lock().unwrap();
            s.rows[0].started_ms -= 8 * 86_400_000;
            s.retention(7).unwrap();
            assert!(s.list(None).is_empty());
        }
        assert!(Store::open(Some(path.clone()))
            .unwrap()
            .list(None)
            .is_empty());
        store.lock().unwrap().retention(0).unwrap();
        run_in(store.clone(), "s", None, "ask", async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(store.lock().unwrap().list(None).len(), 1);
        assert!(Store::open(Some(path)).unwrap().list(None).is_empty());
    }
    #[tokio::test]
    async fn concurrent_runs_do_not_mix_attempts() {
        let store = memory();
        let work = |id: &'static str, n| {
            let store = store.clone();
            async move {
                run_in(store, id, None, "map", async move {
                    for _ in 0..n {
                        let mut a = Attempt::start("http", None);
                        tokio::task::yield_now().await;
                        a.finish(&Ok(()));
                    }
                    Ok(())
                })
                .await
                .unwrap();
            }
        };
        tokio::join!(work("a", 2), work("b", 3));
        let mut counts: Vec<_> = store
            .lock()
            .unwrap()
            .list(None)
            .iter()
            .map(|r| r.calls.len())
            .collect();
        counts.sort();
        assert_eq!(counts, vec![2, 3]);
    }
    #[tokio::test]
    async fn http_retry_counts_every_attempt_and_preserves_parsing() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("POST", "/chat/completions")
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"response_format":{"type":"json_object"}}),
            ))
            .with_status(400)
            .with_body("unsupported response_format SECRET error")
            .expect(1)
            .create_async()
            .await;
        let second=server.mock("POST","/chat/completions").match_body(mockito::Matcher::Json(serde_json::json!({"model":"gpt-4o-mini","messages":[{"role":"system","content":crate::llm::prompt::extract_system_prompt(&[])},{"role":"user","content":"SECRET input"}],"temperature":0.2}))).with_status(200).with_body(r#"{"usage":{"prompt_tokens":23,"completion_tokens":7},"choices":[{"message":{"content":"```json\n{\"lessons_markdown\":\"SECRET output\"}\n```"}}]}"#).expect(1).create_async().await;
        let p = HttpProvider::new(
            server.url(),
            "gpt-4o-mini".into(),
            None,
            false,
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        let store = memory();
        let env = run_in(store.clone(), "r", None, "extract", async {
            p.extract("SECRET input", &[], CancellationToken::new())
                .await
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
        assert_eq!(env.lessons_markdown, "SECRET output");
        first.assert_async().await;
        second.assert_async().await;
        let row = store.lock().unwrap().list(None).remove(0);
        assert_eq!(row.calls.len(), 2);
        assert_eq!(row.calls[0].outcome, Outcome::Failed);
        assert_eq!(row.calls[0].usage, Usage::default());
        assert_eq!(row.calls[1].usage.input_tokens, Some(23));
        assert!(row.steps[0]
            .checks
            .contains(&"retry_without_response_format".into()));
    }
    #[tokio::test]
    async fn permission_change_drops_waiting_call_without_more_dispatches() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let mut server = mockito::Server::new_async().await;
        let response = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_chunked_body(|w| {
                std::thread::sleep(std::time::Duration::from_millis(250));
                w.write_all(b"SECRET")
            })
            .create_async()
            .await;
        let valid = AtomicBool::new(true);
        let gate = Mutex::new(());
        let p = CheckedProvider {
            cancel: Mutex::new(None),
            gate: &gate,
            inner: Box::new(
                HttpProvider::new(
                    server.url(),
                    "test".into(),
                    None,
                    false,
                    std::time::Duration::from_secs(5),
                )
                .unwrap(),
            ),
            valid: Box::new(|| valid.load(Ordering::SeqCst)),
        };
        let store = memory();
        let work = run_in(store.clone(), "r", None, "ask", async {
            let turns = vec![ChatTurn {
                role: ChatRole::User,
                content: "SECRET".into(),
            }];
            let first = p.chat("SECRET", &turns, CancellationToken::new()).await;
            assert!(matches!(first, Err(ExtractError::Cancelled)));
            assert!(matches!(
                p.chat("SECRET", &turns, CancellationToken::new()).await,
                Err(ExtractError::Cancelled)
            ));
            Err::<(), _>("cancelled".into())
        });
        let revoke = async {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            valid.store(false, Ordering::SeqCst);
        };
        let _ = tokio::join!(work, revoke);
        response.assert_async().await;
        let row = store.lock().unwrap().list(None).remove(0);
        assert_eq!(row.calls.len(), 1);
        assert_eq!(row.steps[0].outcome, Outcome::Cancelled);
        assert_eq!(row.calls[0].usage, Usage::default());
    }
}
