//! Process-session admission limits, independent of deletable execution receipts.
//! Estimated capacity is not money, a tokenizer, or an enforceable provider spend cap.
use super::{
    provider::ExtractError,
    receipts::{Provenance, Usage},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub automatic_enrichment: bool,
    pub max_concurrent: usize,
    pub max_attempts: u32,
    pub workflow_units: u64,
    pub session_units: u64,
    pub output_tokens: u64,
    /// Explicit compatibility choice; never retry by silently removing a cap.
    pub output_parameter: OutputParameter,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputParameter {
    #[default]
    MaxTokens,
    MaxCompletionTokens,
    Unsupported,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            automatic_enrichment: false,
            max_concurrent: 2,
            max_attempts: 16,
            workflow_units: 512_000,
            session_units: 4_000_000,
            output_tokens: 4096,
            output_parameter: OutputParameter::MaxTokens,
        }
    }
}
impl Settings {
    fn validate(&self) -> Result<(), String> {
        if !(1..=8).contains(&self.max_concurrent)
            || !(1..=64).contains(&self.max_attempts)
            || !(256..=32768).contains(&self.output_tokens)
            || self.workflow_units < self.output_tokens
            || self.workflow_units > 10_000_000
            || self.session_units < self.workflow_units
            || self.session_units > 100_000_000
        {
            return Err("Invalid AI limits".into());
        }
        Ok(())
    }
}
struct Run {
    remaining: u64,
    attempts: u32,
    active: usize,
    expires: Instant,
    closed: bool,
    settings: Settings,
}
pub struct Ledger {
    settings: Settings,
    runs: HashMap<String, Run>,
    spent: u64,
    active: usize,
    path: Option<PathBuf>,
}
impl Ledger {
    pub fn memory(settings: Settings) -> Self {
        Self {
            settings,
            runs: HashMap::new(),
            spent: 0,
            active: 0,
            path: None,
        }
    }
    fn prune(&mut self) {
        self.runs
            .retain(|_, r| r.active > 0 || (!r.closed && r.expires > Instant::now()));
    }
    fn admit(&mut self, request: &str, parent: Option<&str>) -> Result<String, String> {
        self.prune();
        if self.active >= self.settings.max_concurrent {
            return Err(
                "AI concurrency limit reached. Wait for the current work to finish.".into(),
            );
        }
        let id = key(parent.unwrap_or(request));
        if parent.is_some() {
            if !self.settings.automatic_enrichment {
                return Err("Automatic enrichment is off. The main result remains saved.".into());
            }
            let run = self
                .runs
                .get_mut(&id)
                .ok_or("AI workflow reservation expired. Use an explicit suggestion action.")?;
            if run.closed {
                return Err("AI workflow is closed".into());
            }
            run.active += 1;
        } else {
            if self.runs.contains_key(&id) {
                return Err("AI request already admitted".into());
            }
            let reserved: u64 = self.runs.values().map(|r| r.remaining).sum();
            if self
                .spent
                .saturating_add(reserved)
                .saturating_add(self.settings.workflow_units)
                > self.settings.session_units
            {
                return Err("AI session capacity exhausted or reserved. Wait for reservations to expire or change AI limits.".into());
            }
            self.runs.insert(
                id.clone(),
                Run {
                    remaining: self.settings.workflow_units,
                    attempts: 0,
                    active: 1,
                    expires: Instant::now() + Duration::from_secs(600),
                    closed: false,
                    settings: self.settings.clone(),
                },
            );
        }
        self.active += 1;
        Ok(id)
    }
}
fn key(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
static LEDGER: OnceLock<Result<Arc<Mutex<Ledger>>, String>> = OnceLock::new();
fn ledger() -> Result<Arc<Mutex<Ledger>>, String> {
    LEDGER
        .get_or_init(|| {
            #[cfg(test)]
            let path: Option<PathBuf> = None;
            #[cfg(not(test))]
            let path = Some(
                crate::paths::config_base()
                    .ok_or("AI limits storage unavailable")?
                    .join("jodd/ai-limits.json"),
            );
            let settings = match path.as_ref().map(std::fs::read) {
                Some(Ok(bytes)) => serde_json::from_slice::<Settings>(&bytes)
                    .map_err(|_| "AI limits could not be read")?,
                Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err("AI limits could not be read".into())
                }
                _ => Settings::default(),
            };
            settings.validate()?;
            let mut ledger = Ledger::memory(settings);
            ledger.path = path;
            Ok(Arc::new(Mutex::new(ledger)))
        })
        .clone()
}
#[derive(Clone)]
struct Context {
    ledger: Arc<Mutex<Ledger>>,
    id: String,
    automatic: bool,
}
tokio::task_local! { static CURRENT: Context; }
struct Admission {
    ctx: Context,
    success: bool,
}
impl Drop for Admission {
    fn drop(&mut self) {
        let mut l = self.ctx.ledger.lock().unwrap();
        l.active -= 1;
        let enrichment = l.settings.automatic_enrichment;
        if let Some(r) = l.runs.get_mut(&self.ctx.id) {
            r.active -= 1;
            r.expires = Instant::now() + Duration::from_secs(600);
            if !self.success || !enrichment {
                r.closed = true;
            }
        }
        l.prune();
    }
}
pub async fn run<T>(
    request: &str,
    parent: Option<&str>,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    if CURRENT.try_with(|_| ()).is_ok() {
        return future.await;
    }
    run_in(ledger()?, request, parent, future).await
}
pub async fn run_in<T>(
    ledger: Arc<Mutex<Ledger>>,
    request: &str,
    parent: Option<&str>,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let id = ledger.lock().unwrap().admit(request, parent)?;
    let ctx = Context {
        ledger,
        id,
        automatic: parent.is_some(),
    };
    let mut guard = Admission {
        ctx: ctx.clone(),
        success: false,
    };
    let result = CURRENT.scope(ctx, future).await;
    guard.success = result.is_ok();
    result
}
fn exhausted() -> ExtractError {
    super::receipts::check("budget_exhausted");
    ExtractError::UpstreamError("AI workflow limit reached; available results are preserved. No additional provider attempt was sent.".into())
}
/// Full serialized request bytes (including history/catalog/system/schema) plus
/// framing margin, at one planning unit per UTF-8 byte. Deliberately conservative,
/// still only an estimate: provider tokenization and hidden CLI context are unknown.
pub struct Attempt {
    ctx: Option<Context>,
    reserved: u64,
}
impl Attempt {
    pub fn start(input_bytes: usize) -> Result<Self, ExtractError> {
        let Some(ctx) = CURRENT.try_with(Clone::clone).ok() else {
            return Ok(Self {
                ctx: None,
                reserved: 0,
            });
        };
        let mut l = ctx.ledger.lock().unwrap();
        if ctx.automatic && !l.settings.automatic_enrichment {
            return Err(exhausted());
        }
        let outstanding: u64 = l.runs.values().map(|r| r.remaining).sum();
        if l.spent.saturating_add(outstanding) > l.settings.session_units {
            return Err(exhausted());
        }
        let r = l.runs.get_mut(&ctx.id).ok_or_else(exhausted)?;
        let reserved = (input_bytes as u64)
            .saturating_add(256)
            .saturating_add(r.settings.output_tokens);
        if r.closed || r.attempts >= r.settings.max_attempts || reserved > r.remaining {
            return Err(exhausted());
        }
        r.attempts += 1;
        r.remaining -= reserved;
        l.spent = l.spent.saturating_add(reserved);
        drop(l);
        super::receipts::metric("last_attempt_reserved_units", reserved as usize);
        super::receipts::check("estimated_capacity_not_billing");
        Ok(Self {
            ctx: Some(ctx),
            reserved,
        })
    }
    pub fn reconcile(&mut self, usage: &Usage) {
        let Some(ctx) = self.ctx.take() else {
            return;
        };
        // Missing components, estimates, failure/cancel and opaque CLI attempts
        // retain the entire reservation. Unknown never turns into a zero refund.
        let (Provenance::Actual, Some(input), Some(output)) =
            (&usage.provenance, usage.input_tokens, usage.output_tokens)
        else {
            return;
        };
        let actual = input.saturating_add(output);
        let mut l = ctx.ledger.lock().unwrap();
        l.spent = l.spent.saturating_sub(self.reserved).saturating_add(actual);
        if let Some(r) = l.runs.get_mut(&ctx.id) {
            if actual <= self.reserved {
                r.remaining = r.remaining.saturating_add(self.reserved - actual);
            } else {
                r.remaining = r.remaining.saturating_sub(actual - self.reserved);
                r.closed = true;
            }
        }
    }
}
pub fn output_limit() -> Option<(OutputParameter, u64)> {
    CURRENT
        .try_with(|c| {
            c.ledger
                .lock()
                .unwrap()
                .runs
                .get(&c.id)
                .map(|r| (r.settings.output_parameter, r.settings.output_tokens))
        })
        .ok()
        .flatten()
}
#[tauri::command]
pub fn get_ai_limits() -> Result<Settings, String> {
    Ok(ledger()?.lock().unwrap().settings.clone())
}
#[tauri::command]
pub fn set_ai_limits(settings: Settings) -> Result<(), String> {
    settings.validate()?;
    let shared = ledger()?;
    let mut l = shared.lock().unwrap();
    if let Some(path) = &l.path {
        let parent = path.parent().ok_or("AI limits storage unavailable")?;
        std::fs::create_dir_all(parent).map_err(|_| "AI limits could not be saved")?;
        let mut file =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| "AI limits could not be saved")?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec(&settings).map_err(|_| "AI limits could not be saved")?)
            .map_err(|_| "AI limits could not be saved")?;
        file.as_file()
            .sync_all()
            .map_err(|_| "AI limits could not be saved")?;
        file.persist(path)
            .map_err(|_| "AI limits could not be saved")?;
    }
    // Disabling is checked again at every child attempt, even after admission.
    l.settings = settings;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{http::HttpProvider, provider::LlmProvider};
    use tokio_util::sync::CancellationToken;
    fn settings() -> Settings {
        Settings {
            automatic_enrichment: true,
            max_attempts: 1,
            workflow_units: 20_000,
            session_units: 20_000,
            output_tokens: 256,
            ..Settings::default()
        }
    }
    #[tokio::test]
    async fn retry_cannot_bypass_workflow_attempt_limit() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("POST", "/chat/completions")
            .with_status(400)
            .with_body("response_format unsupported")
            .expect(1)
            .create_async()
            .await;
        let provider = HttpProvider::new(
            server.url(),
            "fake".into(),
            None,
            false,
            Duration::from_secs(2),
        )
        .unwrap();
        let result = run_in(
            Arc::new(Mutex::new(Ledger::memory(settings()))),
            "r",
            None,
            async {
                provider
                    .extract("synthetic", &[], CancellationToken::new())
                    .await
                    .map_err(|e| e.to_string())
            },
        )
        .await;
        assert!(result.unwrap_err().contains("AI workflow limit reached"));
        first.assert_async().await;
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    fn memory() -> Arc<Mutex<Ledger>> {
        Arc::new(Mutex::new(Ledger::memory(Settings {
            automatic_enrichment: true,
            max_attempts: 3,
            workflow_units: 10_000,
            session_units: 10_000,
            output_tokens: 256,
            ..Settings::default()
        })))
    }
    #[test]
    fn simultaneous_threads_cannot_reserve_the_same_capacity() {
        let ledger = memory();
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let ledger = ledger.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    ledger.lock().unwrap().admit(&i.to_string(), None).is_ok()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .filter(|v| *v)
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn concurrency_is_limited_even_with_sufficient_capacity() {
        let l = memory();
        l.lock().unwrap().settings.session_units = 100_000;
        l.lock().unwrap().settings.max_concurrent = 1;
        run_in(l.clone(), "root", None, async {
            assert!(run_in(l.clone(), "another", None, async { Ok(()) })
                .await
                .unwrap_err()
                .contains("concurrency"));
            Ok(())
        })
        .await
        .unwrap();
        run_in(l, "another", None, async { Ok(()) }).await.unwrap();
    }
    #[tokio::test]
    async fn disabled_enrichment_releases_unused_capacity_on_completion() {
        let l = memory();
        l.lock().unwrap().settings.automatic_enrichment = false;
        run_in(l.clone(), "root", None, async { Ok(()) })
            .await
            .unwrap();
        assert!(l.lock().unwrap().runs.is_empty());
        run_in(l, "second", None, async { Ok(()) }).await.unwrap();
    }
    #[tokio::test]
    async fn concurrent_admission_reserves_whole_workflow_once() {
        let l = memory();
        run_in(l.clone(), "first", None, async {
            let other = run_in(l.clone(), "second", None, async {
                panic!("must not execute");
                #[allow(unreachable_code)]
                Ok(())
            })
            .await;
            assert!(other.unwrap_err().contains("capacity"));
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(l.lock().unwrap().runs.len(), 1);
    }
    #[tokio::test]
    async fn children_retries_unknown_and_reported_usage_share_allowance() {
        let l = memory();
        run_in(l.clone(), "root", None, async {
            let mut a = Attempt::start(1000).unwrap();
            a.reconcile(&Usage {
                input_tokens: Some(100),
                output_tokens: Some(20),
                provenance: Provenance::Actual,
            });
            let mut b = Attempt::start(1000).unwrap();
            b.reconcile(&Usage {
                input_tokens: Some(1),
                output_tokens: None,
                provenance: Provenance::Actual,
            });
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(l.lock().unwrap().spent, 120 + 1512);
        run_in(l.clone(), "child", Some("root"), async {
            let _cli = Attempt::start(100).unwrap(); // unknown, including CLI internal calls
            assert!(Attempt::start(1).is_err());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(l.lock().unwrap().spent, 120 + 1512 + 612);
        assert_eq!(l.lock().unwrap().runs.len(), 1);
    }
    #[tokio::test]
    async fn cancellation_keeps_dispatched_units_releases_unused_and_closes_children() {
        let l = memory();
        let _: Result<(), String> = run_in(l.clone(), "root", None, async {
            let _attempt = Attempt::start(100).unwrap();
            Err("cancelled".into())
        })
        .await;
        assert_eq!(l.lock().unwrap().spent, 612);
        assert_eq!(l.lock().unwrap().active, 0);
        assert!(l.lock().unwrap().runs.is_empty());
        assert!(run_in(l, "child", Some("root"), async { Ok(()) })
            .await
            .is_err());
    }
    #[tokio::test]
    async fn dropped_future_releases_concurrency_without_refunding_unknown_attempt() {
        let l = memory();
        let other = l.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            run_in(other, "root", None, async {
                let _a = Attempt::start(100).unwrap();
                tx.send(()).unwrap();
                std::future::pending::<Result<(), String>>().await
            })
            .await
        });
        rx.await.unwrap();
        task.abort();
        let _ = task.await;
        assert_eq!(l.lock().unwrap().active, 0);
        assert_eq!(l.lock().unwrap().spent, 612);
    }
    #[tokio::test]
    async fn preference_change_blocks_already_admitted_followup_dispatch() {
        let l = memory();
        run_in(l.clone(), "root", None, async { Ok(()) })
            .await
            .unwrap();
        run_in(l.clone(), "child", Some("root"), async {
            l.lock().unwrap().settings.automatic_enrichment = false;
            assert!(Attempt::start(100).is_err());
            Ok(())
        })
        .await
        .unwrap();
        assert!(run_in(l.clone(), "another", Some("root"), async { Ok(()) })
            .await
            .is_err());
        assert_eq!(l.lock().unwrap().spent, 0);
    }
    #[tokio::test]
    async fn expiry_releases_unused_reservation_but_never_spent_units() {
        let l = memory();
        run_in(l.clone(), "root", None, async {
            let _a = Attempt::start(100).unwrap();
            Ok(())
        })
        .await
        .unwrap();
        l.lock()
            .unwrap()
            .runs
            .get_mut(&key("root"))
            .unwrap()
            .expires = Instant::now() - Duration::from_secs(1);
        assert!(run_in(l.clone(), "child", Some("root"), async { Ok(()) })
            .await
            .is_err());
        assert_eq!(l.lock().unwrap().spent, 612);
        assert!(l.lock().unwrap().runs.is_empty());
    }
    #[tokio::test]
    async fn reported_overrun_stops_further_attempts_without_claiming_hard_cap() {
        let l = memory();
        run_in(l.clone(), "root", None, async {
            let mut a = Attempt::start(1).unwrap();
            a.reconcile(&Usage {
                input_tokens: Some(30_000),
                output_tokens: Some(400),
                provenance: Provenance::Actual,
            });
            assert!(Attempt::start(1).is_err());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(l.lock().unwrap().spent, 30_400);
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use crate::llm::{
        http::HttpProvider,
        provider::{ChatRole, ChatTurn, LlmProvider},
    };
    use tokio_util::sync::CancellationToken;
    #[tokio::test]
    async fn full_history_is_reserved_and_output_cap_is_sent_without_model_change() {
        let mut server = mockito::Server::new_async().await;
        let response = server.mock("POST", "/chat/completions")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({"model":"selected-model","max_completion_tokens":256})))
            .with_status(200).with_body(r#"{"usage":{"prompt_tokens":40,"completion_tokens":5},"choices":[{"message":{"content":"Synthetic answer"}}]}"#).expect(1).create_async().await;
        let p = HttpProvider::new(
            server.url(),
            "selected-model".into(),
            None,
            false,
            Duration::from_secs(2),
        )
        .unwrap();
        let ledger = Arc::new(Mutex::new(Ledger::memory(Settings {
            workflow_units: 5000,
            output_tokens: 256,
            output_parameter: OutputParameter::MaxCompletionTokens,
            ..Settings::default()
        })));
        run_in(ledger.clone(), "root", None, async {
            let huge_history = [
                ChatTurn {
                    role: ChatRole::Assistant,
                    content: "ก".repeat(3000),
                },
                ChatTurn {
                    role: ChatRole::User,
                    content: "continue".into(),
                },
            ];
            assert!(p
                .chat("system", &huge_history, CancellationToken::new())
                .await
                .unwrap_err()
                .to_string()
                .contains("workflow limit"));
            assert_eq!(
                p.chat("system", &[], CancellationToken::new())
                    .await
                    .unwrap(),
                "Synthetic answer"
            );
            Ok(())
        })
        .await
        .unwrap();
        response.assert_async().await;
        assert_eq!(ledger.lock().unwrap().spent, 45);
    }
    #[tokio::test]
    async fn deleting_receipts_does_not_reset_budget_or_restore_metadata() {
        use crate::llm::receipts;
        let ledger = Arc::new(Mutex::new(Ledger::memory(Settings {
            automatic_enrichment: true,
            max_attempts: 1,
            ..Settings::default()
        })));
        let store = Arc::new(Mutex::new(receipts::Store::open(None).unwrap()));
        run_in(
            ledger.clone(),
            "root",
            None,
            receipts::run_in(store.clone(), "root", None, "extract", async {
                let _a = Attempt::start(100).unwrap();
                store.lock().unwrap().delete(None).unwrap();
                Ok(())
            }),
        )
        .await
        .unwrap();
        run_in(
            ledger,
            "child",
            Some("root"),
            receipts::run_in(store.clone(), "child", Some("root"), "links", async {
                assert!(Attempt::start(1).is_err());
                Ok(())
            }),
        )
        .await
        .unwrap();
        assert!(store.lock().unwrap().list(None).is_empty());
    }
}
