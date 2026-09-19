//! Generic headless-agent-CLI provider. One runner, many CLIs — the
//! per-CLI variation lives entirely in `AgentCliSpec` (see presets.rs).

use serde::{Deserialize, Serialize};

/// How the prompt reaches the CLI.
///
/// Three variants rather than one implicit rule, because the choice is
/// forced by real constraints: `gemini` and `qwen` refuse to run
/// non-interactively without `-p`, so their system prompt must sit in argv —
/// but Windows caps a command line at ~32,767 characters and a pasted source
/// can be far longer. Keeping `Argv` (the only variant that puts unbounded
/// text on the command line) as its own named case makes that risk visible
/// in the type instead of hidden in a substitution rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDelivery {
    /// system + payload → stdin. Args carry no prompt placeholder.
    StdinAll,
    /// system → `{system}` in args; payload → stdin.
    StdinPayloadSystemArg,
    /// system + payload → `{prompt}` in args; nothing on stdin.
    Argv,
}

/// Where the model's text ends up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSource {
    Stdout,
    /// Jodd creates a temp path, substitutes it into `{out_file}`, and reads
    /// the file after the process exits (`codex exec --output-last-message`).
    LastMessageFile,
}

/// How reliably the model's text can be located in the CLI's output.
/// Drives the single retry in `run_json` — see Task 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFidelity {
    /// The CLI has a real JSON output mode: the text's location is known.
    Structured,
    /// Raw stdout: the JSON must be dug out of prose and ANSI noise.
    Heuristic,
}

/// The complete description of one headless CLI. Preset rows and
/// user-authored Custom configs are the same type; `Serialize`/`Deserialize`
/// because a Custom value is persisted inside `accounts.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCliSpec {
    /// Bare name resolved through PATH (`"gemini"`) or an absolute path.
    pub binary: String,
    /// Argv template. Placeholders: `{system}`, `{prompt}`, `{out_file}`.
    pub args: Vec<String>,
    /// Argv fragment appended ONLY when the caller supplies an output schema.
    /// Kept separate from `args` because whether a schema exists is a
    /// property of the WORKFLOW (extract and suggest_links have one, chat
    /// does not), while `args` is a property of the CLI.
    /// `serde(default)`: a Custom spec persisted before this field existed
    /// must still deserialize out of accounts.json.
    #[serde(default)]
    pub schema_args: Vec<String>,
    pub prompt_delivery: PromptDelivery,
    pub output: OutputSource,
    /// JSON field holding the model text (`"result"`, `"response"`).
    /// `None` means the output *is* the text.
    pub unwrap: Option<String>,
    pub fidelity: OutputFidelity,
    pub timeout_secs: u64,
}

/// The two shapes a CLI can take an output schema in. claude wants the schema
/// inline (`--json-schema '<json>'`); codex wants a path
/// (`--output-schema <FILE>`). Jodd always writes the temp file, then fills
/// whichever placeholder the row asks for.
pub struct SchemaSubstitution {
    pub json: String,
    pub path: String,
}

/// Remove ANSI escape sequences (CSI and OSC) from CLI output.
///
/// Hand-rolled rather than pulling a crate, matching the precedent set by
/// `db::extract_urls`. Iterates over `char`s, not bytes: Jodd's content is
/// Thai-first and a byte-wise scanner would split multi-byte sequences.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            // CSI: ESC [ ... final byte in 0x40..=0x7E
            Some('[') => {
                chars.next();
                for c2 in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c2) {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... terminated by BEL or by ESC \
            Some(']') => {
                chars.next();
                loop {
                    match chars.peek().copied() {
                        None => break,
                        Some('\u{07}') => {
                            chars.next();
                            break;
                        }
                        // Leave the ESC unconsumed: the outer loop strips it,
                        // whether it starts the ST (ESC \) or a new sequence.
                        Some('\u{1b}') => break,
                        Some(_) => {
                            chars.next();
                        }
                    }
                }
            }
            // Two-character escapes (ESC c, ESC M, ESC =) are always ASCII.
            // A non-ASCII char after ESC is real content — keep it.
            Some(c2) if c2.is_ascii() => {
                chars.next();
            }
            _ => {}
        }
    }
    out
}

/// Expand `{system}`, `{prompt}`, `{out_file}`, `{schema_json}` and
/// `{schema_file}` in an argv template.
///
/// Performs a single left-to-right pass, never rescanning substituted text,
/// so payload containing literal `{system}`, `{prompt}`, `{out_file}`,
/// `{schema_json}` or `{schema_file}` is not corrupted. `{out_file}` is left
/// verbatim when no temp path exists, so a Custom spec that asks for a file
/// while configured for stdout fails visibly at the CLI rather than silently
/// passing an empty argument — `{schema_json}`/`{schema_file}` follow the
/// same precedent when no schema is in play.
pub fn substitute(
    args: &[String],
    system: &str,
    full_prompt: &str,
    out_file: Option<&str>,
    schema: Option<&SchemaSubstitution>,
) -> Vec<String> {
    const PLACEHOLDERS: [&str; 5] = [
        "{system}",
        "{prompt}",
        "{out_file}",
        "{schema_json}",
        "{schema_file}",
    ];
    args.iter()
        .map(|a| {
            let mut out = String::with_capacity(a.len());
            let mut rest = a.as_str();
            loop {
                let hit = PLACEHOLDERS
                    .iter()
                    .filter_map(|p| rest.find(p).map(|i| (i, *p)))
                    .min_by_key(|(i, _)| *i);
                match hit {
                    None => {
                        out.push_str(rest);
                        break;
                    }
                    Some((i, p)) => {
                        out.push_str(&rest[..i]);
                        match p {
                            "{system}" => out.push_str(system),
                            "{prompt}" => out.push_str(full_prompt),
                            // No temp path in play: emit the placeholder
                            // verbatim so a misconfigured spec fails at the
                            // CLI instead of receiving an empty argument.
                            "{out_file}" => match out_file {
                                Some(f) => out.push_str(f),
                                None => out.push_str(p),
                            },
                            // Same precedent: no schema in play, emit
                            // verbatim rather than an empty argument.
                            "{schema_json}" => match schema {
                                Some(s) => out.push_str(&s.json),
                                None => out.push_str(p),
                            },
                            "{schema_file}" => match schema {
                                Some(s) => out.push_str(&s.path),
                                None => out.push_str(p),
                            },
                            _ => unreachable!(),
                        }
                        rest = &rest[i + p.len()..];
                    }
                }
            }
            out
        })
        .collect()
}

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::llm::prompt::{
    FOLDER_SUGGESTION_SYSTEM_PROMPT, LINK_SUGGESTION_SYSTEM_PROMPT, SYNTHESIS_SYSTEM_PROMPT,
};
use crate::llm::provider::{
    folder_suggestion_request_json, parse_envelope_lenient, synthesis_request_json, CandidateSummary,
    ChatTurn, ExtractEnvelope, ExtractError, FolderSuggestionEnvelope, LinkSuggestionsEnvelope, LlmProvider,
    SourceDigest,
};

/// Agent CLIs log heavily; a full stderr dump would swamp the error toast.
const STDERR_TAIL: usize = 2000;

/// Appended to the system prompt on the single retry a Heuristic preset gets.
const JSON_ONLY_NUDGE: &str =
    "IMPORTANT: reply with the raw JSON object only. No prose before or after \
     it, no markdown code fence, no explanation.";

/// Build the message shown when a CLI exits non-zero.
///
/// **stderr is not where every CLI puts its error.** `claude -p
/// --output-format json` reports "Not logged in · Please run /login" inside
/// the JSON object on STDOUT and exits 1, leaving stderr completely empty —
/// so reporting stderr alone rendered as `claude exit exit status: 1:` with
/// nothing after the colon, in the app, to the user. Measured 2026-08-30
/// against a real failing run; the failure was undiagnosable from Jodd's own
/// error text, which is the defect this fixes.
///
/// Order: a non-empty stderr wins, because a CLI that bothered to write there
/// is being explicit. Otherwise fall back to stdout, and when the row declares
/// an `unwrap` field, dig the human sentence out of the envelope rather than
/// showing the user a multi-kilobyte JSON blob.
pub fn failure_detail(stderr: &str, stdout: &str, unwrap: Option<&str>) -> String {
    if !stderr.trim().is_empty() {
        return tail_on_char_boundary(stderr).to_string();
    }
    let out = stdout.trim();
    if out.is_empty() {
        return String::new();
    }
    if let Some(field) = unwrap {
        if let Some(msg) = dig_unwrap(out, field).ok().filter(|m| !m.trim().is_empty()) {
            return tail_on_char_boundary(&msg).to_string();
        }
    }
    tail_on_char_boundary(out).to_string()
}

/// A named cause and one thing the user can do about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnosis {
    pub cause: String,
    pub action: String,
}

/// Turn a raw CLI failure into something actionable, or admit we do not know.
///
/// Deliberately has NO fallback arm. Three failures measured on 2026-08-30
/// each named neither the model nor the provider in their own text, so a
/// guess here would send the user somewhere wrong with Jodd's authority
/// behind it. `None` routes to the raw evidence instead, which is always
/// true even when it is not helpful.
pub fn diagnose(preset_id: &str, raw: &str) -> Option<Diagnosis> {
    let hay = raw.to_lowercase();
    crate::llm::presets::failure_signatures_for(preset_id)
        .iter()
        .find(|sig| {
            sig.needles
                .iter()
                .all(|n| hay.contains(&n.to_lowercase()))
        })
        .map(|sig| Diagnosis {
            cause: sig.cause.to_string(),
            action: sig.action.to_string(),
        })
}

/// Last `STDERR_TAIL` bytes, cut on a char boundary. `&s[n..]` panics if `n`
/// splits a multi-byte character, and a CLI printing Thai or emoji is
/// entirely realistic.
fn tail_on_char_boundary(s: &str) -> &str {
    let want = s.len().saturating_sub(STDERR_TAIL);
    let start = (want..=s.len())
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(s.len());
    &s[start..]
}

#[derive(Debug)]
pub struct AgentCliProvider {
    spec: AgentCliSpec,
    binary: PathBuf,
}

impl AgentCliProvider {
    /// Resolves `spec.binary` through PATH. A missing binary is
    /// `NotConfigured`, which the UI already maps to "open Account Settings".
    pub fn new(spec: AgentCliSpec) -> Result<Self, ExtractError> {
        let binary = which::which(&spec.binary).map_err(|_| {
            ExtractError::NotConfigured(format!("{} not found in PATH", spec.binary))
        })?;
        Ok(Self { spec, binary })
    }

    async fn run_json<T: serde::de::DeserializeOwned + std::fmt::Debug>(
        &self,
        system: &str,
        payload: &str,
        schema: Option<&str>,
        cancel: CancellationToken,
    ) -> Result<T, ExtractError> {
        let first = self.run_once(system, payload, schema, cancel.clone()).await;

        // For Heuristic presets, retry on any MalformedEnvelope from run_once
        // (e.g., empty output or dig_unwrap failure), not just parse failures.
        if let Err(ExtractError::MalformedEnvelope { .. }) = &first {
            if self.spec.fidelity == OutputFidelity::Heuristic {
                super::receipts::check("retry_malformed_envelope");
                let nudged = format!("{system}\n\n{JSON_ONLY_NUDGE}");
                let retry_raw = self.run_once(&nudged, payload, schema, cancel).await?;
                return parse_envelope_lenient::<T>(&retry_raw).map_err(|reason| ExtractError::MalformedEnvelope {
                    reason,
                    raw: retry_raw,
                });
            }
        }

        let first_raw = first?;

        let first_parsed = parse_envelope_lenient::<T>(&first_raw).map_err(|reason| ExtractError::MalformedEnvelope {
            reason,
            raw: first_raw.clone(),
        });

        // Also retry on parse failure if Heuristic. For a Structured preset
        // a parse failure means something is genuinely wrong, and silently
        // doubling the latency would hide it. Retrying a transport or
        // upstream error would just repeat it.
        match first_parsed {
            Err(ExtractError::MalformedEnvelope { .. })
                if self.spec.fidelity == OutputFidelity::Heuristic =>
            {
                super::receipts::check("retry_malformed_envelope");
                let nudged = format!("{system}\n\n{JSON_ONLY_NUDGE}");
                let retry_raw = self.run_once(&nudged, payload, schema, cancel).await?;
                parse_envelope_lenient::<T>(&retry_raw).map_err(|reason| ExtractError::MalformedEnvelope {
                    reason,
                    raw: retry_raw,
                })
            }
            other => other,
        }
    }

    async fn run_once(
        &self,
        system: &str,
        payload: &str,
        schema: Option<&str>,
        cancel: CancellationToken,
    ) -> Result<String, ExtractError> {
        if cancel.is_cancelled() { return Err(ExtractError::Cancelled); }
        let _budget = super::budget::Attempt::start(system.len().saturating_add(payload.len()).saturating_add(schema.map_or(0, str::len)))?;
        let mut attempt = super::receipts::Attempt::start("agent_cli", None);
        let result = self.run_once_untracked(system, payload, schema, cancel).await
            .map(|value| super::receipts::AiResult { value, usage: super::receipts::Usage::default() });
        attempt.finish(&result);
        if let Ok(result) = &result { attempt.record_usage(result.usage.clone()); }
        result.map(|result| result.value)
    }

    async fn run_once_untracked(
        &self,
        system: &str,
        payload: &str,
        schema: Option<&str>,
        cancel: CancellationToken,
    ) -> Result<String, ExtractError> {
        let full = format!("{system}\n\n---\n\n{payload}");

        // Held for the duration: dropping it deletes the file.
        let out_file = match self.spec.output {
            OutputSource::LastMessageFile => Some(
                tempfile::NamedTempFile::new()
                    .map_err(|e| ExtractError::Transport(format!("temp file: {e}")))?,
            ),
            OutputSource::Stdout => None,
        };
        let out_path = out_file.as_ref().map(|f| f.path().to_string_lossy().into_owned());

        // Held for the duration: dropping it deletes the file, same as
        // `out_file` above.
        let schema_file = match schema {
            Some(text) => {
                let mut f = tempfile::NamedTempFile::new()
                    .map_err(|e| ExtractError::Transport(format!("schema temp file: {e}")))?;
                std::io::Write::write_all(&mut f, text.as_bytes())
                    .map_err(|e| ExtractError::Transport(format!("schema write: {e}")))?;
                Some(f)
            }
            None => None,
        };
        let schema_sub = schema.map(|json| SchemaSubstitution {
            json: json.to_string(),
            path: schema_file
                .as_ref()
                .map(|f| f.path().to_string_lossy().into_owned())
                .unwrap_or_default(),
        });

        let mut template = self.spec.args.clone();
        if schema_sub.is_some() {
            template.extend(self.spec.schema_args.iter().cloned());
        }
        let args = substitute(&template, system, &full, out_path.as_deref(), schema_sub.as_ref());

        let stdin_payload = match self.spec.prompt_delivery {
            PromptDelivery::StdinAll => Some(full.clone()),
            PromptDelivery::StdinPayloadSystemArg => Some(payload.to_string()),
            PromptDelivery::Argv => None,
        };

        // Give the CLI an empty scratch directory to run in. Without this it
        // inherits Jodd's cwd, which for an app launched from Finder is `/` —
        // and an agent CLI started at the filesystem root goes looking for
        // context. On macOS that walks into TCC-protected folders and the
        // prompt ("Jodd would like to access Apple Music…") is attributed to
        // Jodd, because the OS holds the parent process responsible for what
        // its children touch. It also wasted tokens: a connection test once
        // came back describing this very repo.
        let scratch = tempfile::tempdir()
            .map_err(|e| ExtractError::Transport(format!("scratch dir: {e}")))?;

        let mut child = Command::new(&self.binary)
            .kill_on_drop(true)
            .current_dir(scratch.path())
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ExtractError::Transport(format!("spawn {}: {e}", self.spec.binary)))?;

        if let Some(mut stdin) = child.stdin.take() {
            if let Some(p) = &stdin_payload {
                if let Err(e) = stdin.write_all(p.as_bytes()).await {
                    // A broken pipe means the child already decided — it exited
                    // before reading its prompt (bad flag, not authenticated,
                    // `codex exec` refusing an untrusted directory…). Returning
                    // here would replace the CLI's own stderr message with an
                    // opaque "stdin write: Broken pipe", so fall through and let
                    // the exit-status path below report what actually went wrong.
                    // Any other write error is a real transport fault.
                    if e.kind() != std::io::ErrorKind::BrokenPipe {
                        return Err(ExtractError::Transport(format!("stdin write: {e}")));
                    }
                    eprintln!(
                        "[llm] {} closed stdin before reading the prompt; \
                         reporting its exit status instead",
                        self.spec.binary
                    );
                }
            }
            // Closing stdin is what makes these CLIs start work.
            drop(stdin);
        }

        // Handles taken up front so the cancel branch can still start_kill —
        // wait_with_output would consume `child`. `mut` because the timeout
        // branch takes stdout out of it before the normal drain below.
        let mut stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let waited = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(ExtractError::Cancelled);
            }
            r = tokio::time::timeout(Duration::from_secs(self.spec.timeout_secs), child.wait()) => r,
        };

        let exit_status = match waited {
            Ok(Ok(st)) => st,
            Ok(Err(e)) => return Err(ExtractError::Transport(format!("wait: {e}"))),
            Err(_elapsed) => {
                // Kill on timeout. Letting the timeout future drop leaves the
                // child running: `tokio::time::timeout` abandons the wait, it
                // does not terminate the process. Without this the CLI keeps
                // burning the user's subscription quota after Jodd gave up.
                let _ = child.start_kill();
                let _ = child.wait().await;

                let mut partial = Vec::new();
                if let Some(mut s) = stdout_handle.take() {
                    let _ = s.read_to_end(&mut partial).await;
                }
                // Silence is the signature of a CLI sitting at an interactive
                // prompt — the dominant failure mode for a Custom spec.
                let hint = if partial.is_empty() {
                    " — no output at all; the CLI may have been waiting for interactive input, check its headless flags"
                } else {
                    ""
                };
                return Err(ExtractError::Transport(format!(
                    "{} timed out after {}s{hint}",
                    self.spec.binary, self.spec.timeout_secs
                )));
            }
        };

        let mut stdout_bytes = Vec::new();
        if let Some(mut s) = stdout_handle {
            let _ = s.read_to_end(&mut stdout_bytes).await;
        }
        let mut stderr_bytes = Vec::new();
        if let Some(mut s) = stderr_handle {
            let _ = s.read_to_end(&mut stderr_bytes).await;
        }

        if !exit_status.success() {
            let stderr = String::from_utf8_lossy(&stderr_bytes);
            let stdout = String::from_utf8_lossy(&stdout_bytes);
            let detail = failure_detail(&stderr, &stdout, self.spec.unwrap.as_deref());
            return Err(ExtractError::UpstreamError(format!(
                "{} exit {exit_status}: {detail}",
                self.spec.binary
            )));
        }

        let raw = match (&self.spec.output, &out_file) {
            (OutputSource::LastMessageFile, Some(f)) => std::fs::read_to_string(f.path())
                .map_err(|e| ExtractError::Transport(format!("read output file: {e}")))?,
            _ => String::from_utf8_lossy(&stdout_bytes).to_string(),
        };

        let text = strip_ansi(&raw);

        if text.trim().is_empty() {
            return Err(ExtractError::MalformedEnvelope {
                reason: "CLI produced no output".into(),
                raw: text,
            });
        }

        match &self.spec.unwrap {
            Some(field) => dig_unwrap(text.trim(), field).map_err(|reason| {
                ExtractError::MalformedEnvelope {
                    reason,
                    raw: text.clone(),
                }
            }),
            None => Ok(text),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for AgentCliProvider {
    async fn extract(
        &self,
        source: &str,
        existing_tags: &[String],
        cancel: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let system = crate::llm::prompt::extract_system_prompt(existing_tags);
        self.run_json(&system, source, Some(ExtractEnvelope::JSON_SCHEMA), cancel)
            .await
    }

    async fn run_workflow(
        &self,
        workflow: crate::llm::provider::WorkflowKind,
        source: &str,
        existing_tags: &[String],
        cancel: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let system = crate::llm::prompt::workflow_system_prompt(workflow, existing_tags);
        if workflow == crate::llm::provider::WorkflowKind::ActionItems {
            let request = super::meeting::request(source)?;
            let result: super::meeting::Meeting = self.run_json(&system, &request, Some(super::meeting::SCHEMA), cancel).await?;
            super::meeting::validate(&result, source)?;
            super::receipts::check("meeting_quotes_validated_not_entailment");
            return super::meeting::envelope(&result, source);
        }
        self.run_json(&system, source, Some(ExtractEnvelope::JSON_SCHEMA), cancel)
            .await
    }

    async fn suggest_links(
        &self,
        source: &str,
        candidates: &[CandidateSummary],
        cancel: CancellationToken,
    ) -> Result<LinkSuggestionsEnvelope, ExtractError> {
        let request_json = serde_json::json!({
            "new_text": source,
            "candidates": candidates,
        })
        .to_string();
        self.run_json(
            LINK_SUGGESTION_SYSTEM_PROMPT,
            &request_json,
            Some(LinkSuggestionsEnvelope::JSON_SCHEMA),
            cancel,
        )
        .await
    }

    async fn suggest_folder(
        &self,
        note_text: &str,
        folders: &[String],
        cancel: CancellationToken,
    ) -> Result<FolderSuggestionEnvelope, ExtractError> {
        let request_json = folder_suggestion_request_json(note_text, folders);
        self.run_json(
            FOLDER_SUGGESTION_SYSTEM_PROMPT,
            &request_json,
            Some(FolderSuggestionEnvelope::JSON_SCHEMA),
            cancel,
        )
        .await
    }

    async fn synthesize(
        &self,
        digests: &[SourceDigest],
        context: &str,
        cancel: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let request_json = synthesis_request_json(digests, context);
        self.run_json(SYNTHESIS_SYSTEM_PROMPT, &request_json, Some(ExtractEnvelope::JSON_SCHEMA), cancel)
            .await
    }

    async fn chat(
        &self,
        system: &str,
        turns: &[ChatTurn],
        cancel: CancellationToken,
    ) -> Result<String, ExtractError> {
        // For chat, pass system directly for argv substitution (e.g., StdinPayloadSystemArg presets),
        // and flatten_turns("", turns) as payload so it contains only the conversation.
        // This ensures system goes to args where {system} placeholders expect it, and the payload
        // contains only the turns (no system prefix).
        let payload = crate::llm::provider::flatten_turns("", turns);
        self.run_once(system, &payload, None, cancel).await
    }
}

/// Pull the model's text out of a CLI's JSON wrapper.
///
/// Two shapes are in the wild and both must work:
///
/// * a single object — `claude -p --output-format json` emits
///   `{"type":"result","result":"<model text>",…}`
/// * an **array of events** — `qwen -o json` emits
///   `[{"type":"result","result":…,"is_error":…}]` (observed 2026-07-28).
///   Gemini CLI, which Qwen Code forks, is assumed to do the same.
///
/// For an array the LAST element carrying the field wins: these are event
/// logs, and the final result event is the one that holds the answer.
fn dig_unwrap(text: &str, field: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("wrapper json: {e}"))?;

    let found = match &v {
        serde_json::Value::Array(items) => items.iter().rev().find_map(|item| item.get(field)),
        other => other.get(field),
    };

    match found {
        None => Err(format!(
            "wrapper has no string field '{field}' ({})",
            match &v {
                serde_json::Value::Array(i) => format!("searched {} array elements", i.len()),
                _ => "top level is an object".to_string(),
            }
        )),
        Some(serde_json::Value::String(s)) => Ok(s.clone()),
        // The field exists but is not a string. MEASURED 2026-08-30 on an
        // authenticated session: `claude -p --output-format json
        // --json-schema <s>` returns `result` as a STRING, so the happy path
        // does not come through here and this arm is insurance against a
        // future change rather than a workaround for observed behaviour.
        // Kept because `claude` is Jodd's DEFAULT preset and is
        // `OutputFidelity::Structured`, which gets no lenient-nudge retry, so
        // erroring here would make every Extract and auto-link call fail
        // permanently the instant that shape appears. Re-stringifying is the
        // only direction that fails safe: a JSON string containing JSON is
        // exactly what `parse_envelope_lenient` already knows how to unwrap,
        // so this degrades gracefully instead of breaking outright.
        Some(other) => {
            serde_json::to_string(other).map_err(|e| format!("re-stringifying field '{field}': {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::provider::ExtractEnvelope;
    use crate::llm::provider::LlmProvider;
    use std::path::Path;
    use tokio_util::sync::CancellationToken;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn substitutes_system_placeholder() {
        let out = substitute(&args(&["-p", "{system}"]), "SYS", "SYS\n\nPAY", None, None);
        assert_eq!(out, vec!["-p".to_string(), "SYS".to_string()]);
    }

    #[test]
    fn substitutes_prompt_placeholder_with_the_full_prompt() {
        let out = substitute(&args(&["run", "{prompt}"]), "SYS", "SYS\n\nPAY", None, None);
        assert_eq!(out[1], "SYS\n\nPAY");
    }

    #[test]
    fn substitutes_out_file_placeholder() {
        let out = substitute(&args(&["--out", "{out_file}"]), "SYS", "P", Some("/tmp/x.txt"), None);
        assert_eq!(out[1], "/tmp/x.txt");
    }

    #[test]
    fn leaves_out_file_placeholder_alone_when_no_file_is_in_play() {
        // A Stdout-mode spec never gets a temp path; the literal must survive
        // untouched rather than becoming an empty string, so a misconfigured
        // Custom spec fails loudly at the CLI instead of silently passing "".
        let out = substitute(&args(&["--out", "{out_file}"]), "SYS", "P", None, None);
        assert_eq!(out[1], "{out_file}");
    }

    #[test]
    fn leaves_args_without_placeholders_untouched() {
        let out = substitute(&args(&["-p", "--output-format", "json"]), "SYS", "P", None, None);
        assert_eq!(out, args(&["-p", "--output-format", "json"]));
    }

    #[test]
    fn payload_containing_out_file_placeholder_is_not_rewritten() {
        let out = substitute(
            &args(&["-p", "{prompt}", "--out", "{out_file}"]),
            "SYS",
            "What does {out_file} mean?",
            Some("/tmp/real.txt"),
            None,
        );
        // The {prompt} arg should contain the literal {out_file} from the payload
        assert!(out[1].contains("{out_file}"));
        // It should NOT contain the real temp path
        assert!(!out[1].contains("/tmp/real.txt"));
        // But the --out arg SHOULD contain the real temp path
        assert_eq!(out[3], "/tmp/real.txt");
    }

    #[test]
    fn payload_containing_system_placeholder_is_not_rewritten() {
        let out = substitute(
            &args(&["-p", "{prompt}", "--out", "{out_file}"]),
            "SYS",
            "Explain {system} variables",
            Some("/tmp/real.txt"),
            None,
        );
        // The {prompt} arg should contain the literal {system} from the payload
        assert!(out[1].contains("{system}"));
        // It should NOT be replaced with "SYS"
        assert!(!out[1].contains("Explain SYS variables"));
    }

    #[test]
    fn payload_containing_schema_placeholders_is_not_rewritten() {
        // Same property as payload_containing_out_file_placeholder_is_not_
        // rewritten / payload_containing_system_placeholder_is_not_rewritten,
        // pinned for the two placeholders `substitute()` gained alongside
        // `{out_file}`. This is the highest-risk regression surface on this
        // branch — user note content flows through `{prompt}` — and it was
        // untested for `{schema_json}`/`{schema_file}` before this fix.
        let sub = SchemaSubstitution {
            json: r#"{"type":"object"}"#.to_string(),
            path: "/tmp/s.json".to_string(),
        };
        let out = substitute(
            &args(&[
                "-p",
                "{prompt}",
                "--json-schema",
                "{schema_json}",
                "--output-schema",
                "{schema_file}",
            ]),
            "SYS",
            "What do {schema_json} and {schema_file} mean?",
            None,
            Some(&sub),
        );
        // The {prompt} arg should contain the literal placeholders from the payload
        assert!(out[1].contains("{schema_json}"));
        assert!(out[1].contains("{schema_file}"));
        // It should NOT contain the real schema JSON or path
        assert!(!out[1].contains(r#"{"type":"object"}"#));
        assert!(!out[1].contains("/tmp/s.json"));
        // But the dedicated schema args SHOULD contain the real values
        assert_eq!(out[3], r#"{"type":"object"}"#);
        assert_eq!(out[5], "/tmp/s.json");
    }

    #[test]
    fn adjacent_placeholders_are_each_substituted_once() {
        let out = substitute(
            &args(&["{system}{prompt}"]),
            "SYS",
            "PAYLOAD",
            None,
            None,
        );
        // The result should be exactly system + prompt
        assert_eq!(out[0], "SYSPAYLOAD");
    }

    #[test]
    fn strips_sgr_colour_codes() {
        assert_eq!(strip_ansi("\u{1b}[32mgreen\u{1b}[0m"), "green");
    }

    #[test]
    fn strips_cursor_movement_and_erase_sequences() {
        assert_eq!(strip_ansi("a\u{1b}[2K\u{1b}[1Gb"), "ab");
    }

    #[test]
    fn strips_osc_title_sequences_terminated_by_bel() {
        assert_eq!(strip_ansi("\u{1b}]0;title\u{07}text"), "text");
    }

    #[test]
    fn strips_osc_sequences_terminated_by_string_terminator() {
        assert_eq!(strip_ansi("\u{1b}]8;;http://x\u{1b}\\link"), "link");
    }

    #[test]
    fn leaves_clean_text_untouched() {
        assert_eq!(strip_ansi("{\"lessons_markdown\":\"x\"}"), "{\"lessons_markdown\":\"x\"}");
    }

    #[test]
    fn preserves_multibyte_text() {
        // Jodd is Thai-first; a byte-wise stripper would corrupt this.
        assert_eq!(strip_ansi("\u{1b}[1mบทเรียน\u{1b}[0m"), "บทเรียน");
    }

    #[test]
    fn bare_escape_does_not_consume_multibyte_content() {
        // A stray ESC must not eat a Thai character. Real two-char escapes
        // (ESC c, ESC M, ESC =) are always ASCII.
        assert_eq!(strip_ansi("\u{1b}บทเรียน"), "บทเรียน");
    }

    #[test]
    fn osc_aborted_by_a_new_escape_still_strips_that_sequence() {
        // An OSC with no BEL/ST, immediately followed by an SGR sequence.
        // The CSI must be stripped, not leaked as literal text.
        assert_eq!(strip_ansi("\u{1b}]0;title\u{1b}[31mred\u{1b}[0m"), "red");
    }

    /// Writes an executable fake CLI into `dir` and returns its absolute path.
    /// Callers supply both platform bodies; `$1`/`%1` style args work as usual.
    /// Test payloads are passed via files, never via shell-quoted literals —
    /// quoting JSON inside `cmd.exe` is a losing game and would make these tests
    /// test the fixture instead of the runner.
    fn fake_cli(dir: &Path, name: &str, unix_body: &str, windows_body: &str) -> String {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join(format!("{name}.sh"));
            std::fs::write(&path, format!("#!/bin/sh\n{unix_body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _ = windows_body;
            path.to_string_lossy().into_owned()
        }
        #[cfg(windows)]
        {
            let path = dir.join(format!("{name}.cmd"));
            std::fs::write(&path, format!("@echo off\r\n{windows_body}\r\n")).unwrap();
            let _ = unix_body;
            path.to_string_lossy().into_owned()
        }
    }

    fn spec_for(binary: String) -> AgentCliSpec {
        AgentCliSpec {
            binary,
            args: vec![],
            schema_args: Vec::new(),
            prompt_delivery: PromptDelivery::StdinAll,
            output: OutputSource::Stdout,
            unwrap: None,
            fidelity: OutputFidelity::Structured,
            timeout_secs: 10,
        }
    }

    #[tokio::test]
    async fn budget_caps_cli_subprocesses_and_preserves_selected_args() {
        use crate::llm::{budget, receipts};
        use std::sync::{Arc, Mutex};
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_cli(dir.path(), "budget", "cat >/dev/null\nprintf '%s' \"$1\"", "more >nul\r\necho %1");
        let mut spec = spec_for(binary);
        spec.args = vec!["selected-model".into()];
        let p = AgentCliProvider::new(spec).unwrap();
        let ledger = Arc::new(Mutex::new(budget::Ledger::memory(budget::Settings { max_attempts: 1, ..Default::default() })));
        let store = Arc::new(Mutex::new(receipts::Store::open(None).unwrap()));
        budget::run_in(ledger,"cli",None,receipts::run_in(store.clone(),"cli",None,"ask",async {
            assert_eq!(p.chat("synthetic", &[], CancellationToken::new()).await.unwrap().trim(), "selected-model");
            assert!(p.chat("synthetic", &[], CancellationToken::new()).await.unwrap_err().to_string().contains("workflow limit"));
            Ok(())
        })).await.unwrap();
        let row = store.lock().unwrap().list(None).remove(0);
        assert_eq!(row.calls.len(), 1);
        assert_eq!(row.calls[0].usage, receipts::Usage::default());
        assert!(row.calls[0].model.is_none());
    }

    // 3 hashes: the JSON value itself starts with `"##`, which would
    // collide with a 1- or 2-hash raw-string terminator (`"#` / `"##`).
    const ENVELOPE: &str = r###"{"lessons_markdown":"## L1\nbody"}"###;

    /// The exact failing shape measured from the running app on 2026-08-30:
    /// claude exits 1, writes NOTHING to stderr, and puts the real sentence
    /// in `result` on stdout. Before this, the user saw
    /// "claude exit exit status: 1:" — a blank — and had no way to learn why.
    #[test]
    fn a_cli_that_reports_its_error_on_stdout_is_not_reported_as_a_blank() {
        let stdout = "{\"is_error\":true,\"result\":\"Not logged in \u{b7} Please run /login\",\"type\":\"result\"}";
        assert_eq!(
            failure_detail("", stdout, Some("result")),
            "Not logged in \u{b7} Please run /login"
        );
    }

    /// A CLI that did write to stderr is being explicit; stderr wins.
    #[test]
    fn stderr_wins_when_the_cli_wrote_there() {
        let got = failure_detail("boom: bad flag\n", "{\"result\":\"ignored\"}", Some("result"));
        assert_eq!(got.trim(), "boom: bad flag");
    }

    /// No unwrap field, or stdout that is not JSON: show it raw rather than
    /// showing the user nothing.
    #[test]
    fn non_json_stdout_is_still_surfaced() {
        assert_eq!(failure_detail("", "plain failure text", None), "plain failure text");
        assert_eq!(failure_detail("", "not json at all", Some("result")), "not json at all");
    }

    /// Both streams empty is the one case where a blank detail is honest.
    #[test]
    fn both_streams_empty_yields_an_empty_detail() {
        assert_eq!(failure_detail("", "   ", Some("result")), "");
    }

    /// Jodd is Thai-first: the tail cut must not split a multi-byte char.
    #[test]
    fn a_long_multibyte_stdout_does_not_panic() {
        let long = "\u{e01}".repeat(4000);
        let got = failure_detail("", &long, None);
        assert!(got.len() <= STDERR_TAIL + 3, "tail should be bounded, got {}", got.len());
        assert!(got.chars().all(|c| c == '\u{e01}'));
    }


    #[tokio::test]
    async fn runs_a_structured_cli_and_parses_the_wrapped_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("out.json");
        std::fs::write(&payload, format!(r#"{{"result":{}}}"#, serde_json::to_string(ENVELOPE).unwrap())).unwrap();
        let bin = fake_cli(
            dir.path(), "ok",
            &format!("cat '{}'", payload.display()),
            &format!("type \"{}\"", payload.display()),
        );

        let mut spec = spec_for(bin);
        spec.unwrap = Some("result".into());
        let p = AgentCliProvider::new(spec).unwrap();
        let env = p.extract("source text", &[], CancellationToken::new()).await.unwrap();
        assert_eq!(env.lessons_markdown, "## L1\nbody");
    }

    #[tokio::test]
    async fn runs_a_heuristic_cli_and_digs_the_envelope_out_of_prose_and_ansi() {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("out.txt");
        std::fs::write(&payload, format!("\u{1b}[32mHere you go:\u{1b}[0m\n{ENVELOPE}\nDone!")).unwrap();
        let bin = fake_cli(
            dir.path(), "prose",
            &format!("cat '{}'", payload.display()),
            &format!("type \"{}\"", payload.display()),
        );

        let p = AgentCliProvider::new(spec_for(bin)).unwrap();
        let env = p.extract("source", &[], CancellationToken::new()).await.unwrap();
        assert_eq!(env.lessons_markdown, "## L1\nbody");
    }

    /// `run_workflow`'s own version of `runs_a_structured_cli_and_parses_the_
    /// wrapped_envelope` — same plumbing (`run_json`), different entry point.
    #[tokio::test]
    async fn run_workflow_runs_a_structured_cli_and_parses_the_wrapped_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("out.json");
        std::fs::write(&payload, format!(r#"{{"result":{}}}"#, serde_json::to_string(ENVELOPE).unwrap())).unwrap();
        let bin = fake_cli(
            dir.path(), "ok",
            &format!("cat '{}'", payload.display()),
            &format!("type \"{}\"", payload.display()),
        );

        let mut spec = spec_for(bin);
        spec.unwrap = Some("result".into());
        let p = AgentCliProvider::new(spec).unwrap();
        let env = p
            .run_workflow(
                crate::llm::provider::WorkflowKind::Summarize,
                "source text",
                &[],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(env.lessons_markdown, "## L1\nbody");
    }

    /// Distinctive substrings of the three workflow prompts in `prompt.rs`,
    /// mirroring `SYSTEM_PROMPT_MARKER` above — real prompt text, not the
    /// Rust identifier, so this test can actually fail if `run_workflow`
    /// dispatches to the wrong prompt (or to `extract`'s).
    #[cfg(unix)]
    #[tokio::test]
    async fn run_workflow_sends_the_matching_prompt_for_each_kind() {
        use crate::llm::provider::WorkflowKind;

        let cases = [
            (WorkflowKind::Summarize, "short, faithful overview"),
            (WorkflowKind::ActionItems, "concrete, actionable"),
            (WorkflowKind::ExpandBullets, "expand a short, terse"),
        ];

        for (kind, marker) in cases {
            let dir = tempfile::tempdir().unwrap();
            let stdin_file = dir.path().join("stdin.txt");
            let bin = fake_cli(
                dir.path(),
                "capture_stdin",
                &format!("cat > '{}'; echo '{}'", stdin_file.display(), if kind == WorkflowKind::ActionItems { r#"{"items":[],"incomplete":false}"# } else { r#"{"lessons_markdown":"ok"}"# }),
                "",
            );

            let p = AgentCliProvider::new(spec_for(bin)).unwrap();
            p.run_workflow(kind, "payload text", &[], CancellationToken::new())
                .await
                .unwrap_or_else(|e| panic!("{kind:?} should succeed: {e}"));

            let stdin_content = std::fs::read_to_string(&stdin_file).unwrap();
            assert!(
                stdin_content.contains(marker),
                "{kind:?} must send its own prompt: '{stdin_content}'"
            );
            assert!(
                stdin_content.contains("payload text"),
                "payload must still reach stdin: '{stdin_content}'"
            );
        }
    }

    #[tokio::test]
    async fn non_zero_exit_surfaces_stderr_as_upstream_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_cli(dir.path(), "boom", "echo 'kaboom' >&2; exit 3", "echo kaboom 1>&2& exit /b 3");

        let p = AgentCliProvider::new(spec_for(bin)).unwrap();
        let err = p.extract("s", &[], CancellationToken::new()).await.unwrap_err();
        match err {
            ExtractError::UpstreamError(m) => assert!(m.contains("kaboom"), "stderr missing from: {m}"),
            other => panic!("expected UpstreamError, got {other:?}"),
        }
    }

    /// The deterministic sibling of the test above. That one races: a CLI that
    /// exits without reading stdin may or may not have torn down its read end
    /// before Jodd's write lands, so on a fast machine the prompt disappears
    /// into the pipe buffer and the write succeeds. Here the payload is larger
    /// than any pipe buffer (Linux 64 KiB, Windows 4 KiB), so `write_all` MUST
    /// keep writing after the child is gone and MUST see EPIPE — every run, on
    /// every machine. A broken stdin pipe is not the failure; it is the
    /// symptom of one the child already reported on stderr, so the error the
    /// user sees has to be the child's, not the pipe's.
    #[tokio::test]
    async fn broken_stdin_pipe_still_surfaces_the_cli_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_cli(dir.path(), "epipe", "echo 'kaboom' >&2; exit 3", "echo kaboom 1>&2& exit /b 3");

        let p = AgentCliProvider::new(spec_for(bin)).unwrap();
        let huge = "x".repeat(1024 * 1024);
        let err = p.extract(&huge, &[], CancellationToken::new()).await.unwrap_err();
        match err {
            ExtractError::UpstreamError(m) => assert!(m.contains("kaboom"), "stderr missing from: {m}"),
            other => panic!("expected UpstreamError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn slow_cli_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_cli(dir.path(), "slow", "sleep 30", "timeout /t 30 /nobreak >nul");

        let mut spec = spec_for(bin);
        spec.timeout_secs = 1;
        let p = AgentCliProvider::new(spec).unwrap();
        let err = p.extract("s", &[], CancellationToken::new()).await.unwrap_err();
        assert!(matches!(err, ExtractError::Transport(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn timeout_on_a_silent_cli_hints_at_interactive_input() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_cli(dir.path(), "mute", "sleep 30", "timeout /t 30 /nobreak >nul");

        let mut spec = spec_for(bin);
        spec.timeout_secs = 1;
        let p = AgentCliProvider::new(spec).unwrap();
        let err = p.extract("s", &[], CancellationToken::new()).await.unwrap_err();
        match err {
            ExtractError::Transport(m) => assert!(
                m.contains("interactive input"),
                "silent timeout should hint at the interactive-prompt case, got: {m}"
            ),
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancellation_returns_cancelled_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_cli(dir.path(), "slow2", "sleep 30", "timeout /t 30 /nobreak >nul");

        let mut spec = spec_for(bin);
        spec.timeout_secs = 300; // must not be what ends this call
        let p = AgentCliProvider::new(spec).unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            c2.cancel();
        });

        let started = std::time::Instant::now();
        let store = std::sync::Arc::new(std::sync::Mutex::new(crate::llm::receipts::Store::open(None).unwrap()));
        let err = crate::llm::receipts::run_in(store.clone(), "cli-cancel", None, "extract", async {
            p.extract("s", &[], cancel).await.map_err(|e| e.to_string())
        }).await.unwrap_err();
        assert_eq!(err, "cancelled");
        let row = store.lock().unwrap().list(None).remove(0);
        assert_eq!(row.steps[0].outcome, crate::llm::receipts::Outcome::Cancelled);
        assert_eq!(row.calls.len(), 1);
        assert_eq!(row.calls[0].outcome, crate::llm::receipts::Outcome::Cancelled);
        assert_eq!(row.calls[0].usage, crate::llm::receipts::Usage::default());
        assert!(started.elapsed().as_secs() < 5, "cancel did not short-circuit the wait");
    }

    #[tokio::test]
    async fn last_message_file_output_is_read_from_the_substituted_path() {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("src.json");
        std::fs::write(&payload, ENVELOPE).unwrap();
        // args are ["--out", "{out_file}"], so the destination is $2 / %2
        let bin = fake_cli(
            dir.path(), "tofile",
            &format!("cat '{}' > \"$2\"", payload.display()),
            &format!("type \"{}\" > %2", payload.display()),
        );

        let mut spec = spec_for(bin);
        spec.args = vec!["--out".into(), "{out_file}".into()];
        spec.output = OutputSource::LastMessageFile;
        let p = AgentCliProvider::new(spec).unwrap();
        let env = p.extract("s", &[], CancellationToken::new()).await.unwrap();
        assert_eq!(env.lessons_markdown, "## L1\nbody");
    }

    #[tokio::test]
    async fn probe_temp_file_cleaned_up_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_cli(dir.path(), "slowfile", "sleep 30", "timeout /t 30 /nobreak >nul");
        let mut spec = spec_for(bin);
        spec.args = vec!["--out".into(), "{out_file}".into()];
        spec.output = OutputSource::LastMessageFile;
        spec.timeout_secs = 1;
        let p = AgentCliProvider::new(spec).unwrap();

        // Snapshot tempdir contents before, so we can find the created temp file.
        let sys_tmp = std::env::temp_dir();
        let before: std::collections::HashSet<_> = std::fs::read_dir(&sys_tmp)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();

        let started = std::time::Instant::now();
        let result = p.extract("s", &[], CancellationToken::new()).await;
        eprintln!("PROBE extract() took {:?}, result={:?}", started.elapsed(), result.err());

        let after: Vec<_> = std::fs::read_dir(&sys_tmp)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| !before.contains(p))
            .collect();
        eprintln!("PROBE new files left behind in system tempdir: {after:?}");
    }

    #[tokio::test]
    async fn heuristic_preset_retries_once_and_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("n");
        let good = dir.path().join("good.json");
        std::fs::write(&good, ENVELOPE).unwrap();
        let bin = fake_cli(
            dir.path(),
            "flaky",
            &format!(
                "if [ -f '{c}' ]; then cat '{g}'; else touch '{c}'; echo 'I cannot do that'; fi",
                c = counter.display(),
                g = good.display()
            ),
            &format!(
                "if exist \"{c}\" (type \"{g}\") else (type nul > \"{c}\" & echo I cannot do that)",
                c = counter.display(),
                g = good.display()
            ),
        );

        let mut spec = spec_for(bin);
        spec.fidelity = OutputFidelity::Heuristic;
        let p = AgentCliProvider::new(spec).unwrap();
        let env = p.extract("s", &[], CancellationToken::new()).await.unwrap();
        assert_eq!(env.lessons_markdown, "## L1\nbody");
    }

    #[tokio::test]
    async fn chat_cancellation_returns_promptly() {
        // (a) chat cancellation test for agent CLI
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_cli(
            dir.path(),
            "slow_chat",
            "sleep 30",
            "timeout /t 30 /nobreak >nul",
        );

        let mut spec = spec_for(bin);
        spec.timeout_secs = 300; // must not be what ends this call
        let p = AgentCliProvider::new(spec).unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            c2.cancel();
        });

        let started = std::time::Instant::now();
        let turns = vec![crate::llm::provider::ChatTurn {
            role: crate::llm::provider::ChatRole::User,
            content: "hello".into(),
        }];
        let err = p
            .chat("system", &turns, cancel)
            .await
            .expect_err("expected cancellation");
        assert!(matches!(err, ExtractError::Cancelled), "got {err:?}");
        assert!(
            started.elapsed().as_secs() < 5,
            "cancel did not short-circuit the wait"
        );
    }

    #[tokio::test]
    async fn heuristic_cli_with_empty_output_retries() {
        // (OPEN 4) Test that empty CLI output triggers a retry for Heuristic presets.
        // This verifies that MalformedEnvelope from run_once (not just parse failures) gets retried.
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("attempt");
        let good = dir.path().join("good.json");
        std::fs::write(&good, ENVELOPE).unwrap();

        let bin = fake_cli(
            dir.path(),
            "empty_then_good",
            &format!(
                "if [ -f '{c}' ]; then cat '{g}'; else touch '{c}'; fi",
                c = counter.display(),
                g = good.display()
            ),
            &format!(
                "if exist \"{c}\" (type \"{g}\") else (type nul > \"{c}\")",
                c = counter.display(),
                g = good.display()
            ),
        );

        let mut spec = spec_for(bin);
        spec.fidelity = OutputFidelity::Heuristic;
        let p = AgentCliProvider::new(spec).unwrap();
        let store = std::sync::Arc::new(std::sync::Mutex::new(crate::llm::receipts::Store::open(None).unwrap()));
        let env = crate::llm::receipts::run_in(store.clone(), "cli-retry", None, "extract", async {
            p.extract("s", &[], CancellationToken::new()).await.map_err(|e| e.to_string())
        }).await.unwrap();
        assert_eq!(env.lessons_markdown, "## L1\nbody");
        let row = store.lock().unwrap().list(None).remove(0);
        assert_eq!(row.calls.len(), 2);
        assert!(row.calls.iter().all(|c| c.model.is_none() && c.usage == crate::llm::receipts::Usage::default()));
        assert_eq!(row.calls[0].outcome, crate::llm::receipts::Outcome::Failed);
        assert_eq!(row.calls[1].outcome, crate::llm::receipts::Outcome::Succeeded);
        assert!(row.steps[0].checks.contains(&"retry_malformed_envelope".into()));
    }

    /// A distinctive substring of `crate::llm::prompt::SYSTEM_PROMPT`. Asserting
    /// on real prompt text (rather than on the Rust *identifier* `SYSTEM_PROMPT`,
    /// which never appears in the prompt's own bytes) is what makes the two
    /// capture tests below able to fail.
    #[cfg(unix)]
    const SYSTEM_PROMPT_MARKER: &str = "single JSON object";

    /// A distinctive substring of `JSON_ONLY_NUDGE`.
    #[cfg(unix)]
    const NUDGE_MARKER: &str = "raw JSON object only";

    // The next two tests need a fake CLI that captures its own stdin to a file.
    // cmd.exe has no dependable one-liner for that (`type` with no argument does
    // not read stdin — it errors, and the `&&` chain then aborts before any
    // output is produced), so rather than ship a Windows body that silently
    // fails on the project's primary platform, these are Unix-only. The
    // behaviour they cover — argv/stdin split and retry-prompt content — is
    // platform-independent: it is decided in `run_once`/`run_json`, not by the
    // child process.
    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_payload_system_arg_sends_system_in_args_only() {
        // StdinPayloadSystemArg must put the system prompt in argv and ONLY the
        // payload on stdin. Sending `full` (system + payload) on stdin — the bug
        // this guards — must fail the middle assertion.
        let dir = tempfile::tempdir().unwrap();
        let argv_file = dir.path().join("argv.txt");
        let stdin_file = dir.path().join("stdin.txt");

        let bin = fake_cli(
            dir.path(),
            "capture_both",
            &format!(
                "printf '%s' \"$1\" > '{a}'; cat > '{s}'; echo '{{\"lessons_markdown\": \"ok\"}}'",
                a = argv_file.display(),
                s = stdin_file.display()
            ),
            "",
        );

        let mut spec = spec_for(bin);
        spec.prompt_delivery = PromptDelivery::StdinPayloadSystemArg;
        spec.args = vec!["{system}".into()];
        spec.fidelity = OutputFidelity::Structured;
        spec.unwrap = None;

        let p = AgentCliProvider::new(spec).unwrap();
        p.extract("payload text", &[], CancellationToken::new())
            .await
            .expect("extraction should succeed");

        let argv_content = std::fs::read_to_string(&argv_file).unwrap();
        let stdin_content = std::fs::read_to_string(&stdin_file).unwrap();

        assert!(
            argv_content.contains(SYSTEM_PROMPT_MARKER),
            "system must reach argv via {{system}}: '{argv_content}'"
        );
        assert!(
            !stdin_content.contains(SYSTEM_PROMPT_MARKER),
            "system must NOT be duplicated onto stdin: '{stdin_content}'"
        );
        assert!(
            stdin_content.contains("payload text"),
            "payload must reach stdin: '{stdin_content}'"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn heuristic_retry_prompt_carries_the_json_only_nudge() {
        // Pins the CONTENT of the retry, not merely that a retry happened:
        // emptying JSON_ONLY_NUDGE must fail the last assertion.
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("attempt");
        let first = dir.path().join("prompt1.txt");
        let second = dir.path().join("prompt2.txt");
        let good = dir.path().join("good.json");
        std::fs::write(&good, ENVELOPE).unwrap();

        let bin = fake_cli(
            dir.path(),
            "capture_prompts",
            &format!(
                "p=$(cat); \
                 if [ -f '{c}' ]; then printf '%s' \"$p\" > '{p2}'; cat '{g}'; \
                 else touch '{c}'; printf '%s' \"$p\" > '{p1}'; echo 'I cannot do that'; fi",
                c = counter.display(),
                p1 = first.display(),
                p2 = second.display(),
                g = good.display()
            ),
            "",
        );

        let mut spec = spec_for(bin);
        spec.fidelity = OutputFidelity::Heuristic;
        let p = AgentCliProvider::new(spec).unwrap();
        let env = p.extract("s", &[], CancellationToken::new()).await.unwrap();
        assert_eq!(env.lessons_markdown, "## L1\nbody");

        let first_prompt = std::fs::read_to_string(&first).expect("first attempt ran");
        let second_prompt = std::fs::read_to_string(&second).expect("retry ran");

        assert!(
            !first_prompt.contains(NUDGE_MARKER),
            "the first attempt must not be nudged: '{first_prompt}'"
        );
        assert!(
            second_prompt.contains(NUDGE_MARKER),
            "the retry must append JSON_ONLY_NUDGE to the system prompt: '{second_prompt}'"
        );
    }

    #[tokio::test]
    async fn structured_preset_does_not_retry() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("n2");
        let good = dir.path().join("good2.json");
        std::fs::write(&good, ENVELOPE).unwrap();
        let bin = fake_cli(
            dir.path(),
            "flaky2",
            &format!(
                "if [ -f '{c}' ]; then cat '{g}'; else touch '{c}'; echo 'I cannot do that'; fi",
                c = counter.display(),
                g = good.display()
            ),
            &format!(
                "if exist \"{c}\" (type \"{g}\") else (type nul > \"{c}\" & echo I cannot do that)",
                c = counter.display(),
                g = good.display()
            ),
        );

        let mut spec = spec_for(bin);
        spec.fidelity = OutputFidelity::Structured;
        let p = AgentCliProvider::new(spec).unwrap();
        let err = p.extract("s", &[], CancellationToken::new()).await.unwrap_err();
        assert!(
            matches!(err, ExtractError::MalformedEnvelope { .. }),
            "structured must surface the first failure, got {err:?}"
        );
    }

    #[tokio::test]
    async fn stderr_tail_does_not_panic_on_multibyte() {
        let dir = tempfile::tempdir().unwrap();
        // stderr must exceed STDERR_TAIL (2000 bytes) or the slice starts at
        // 0 and is trivially safe — the test would then pass with or without
        // the fix. Thai is 3 bytes per char, so 900 repetitions of a 10-char
        // word is ~27kB and the cut at len-2000 lands mid-character.
        let bin = fake_cli(
            dir.path(),
            "thai_err",
            "for i in $(seq 1 900); do printf 'เกิดข้อผิดพลาด' >&2; done; exit 2",
            "echo error 1>&2& exit /b 2",
        );
        let p = AgentCliProvider::new(spec_for(bin)).unwrap();
        let err = p.extract("s", &[], CancellationToken::new()).await.unwrap_err();
        assert!(matches!(err, ExtractError::UpstreamError(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn cli_runs_in_an_empty_scratch_dir_not_the_apps_cwd() {
        // Regression: the CLI used to inherit Jodd's cwd (`/` when launched
        // from Finder), so agent CLIs wandered into the user's home and
        // triggered macOS privacy prompts attributed to Jodd.
        let dir = tempfile::tempdir().unwrap();
        // The fake CLI reports its own working directory as the model text.
        let bin = fake_cli(
            dir.path(),
            "pwd_probe",
            "printf '{\"lessons_markdown\":\"%s\"}' \"$(pwd)\"",
            "echo {\"lessons_markdown\":\"%CD%\"}",
        );
        let p = AgentCliProvider::new(spec_for(bin)).unwrap();
        let env = p.extract("s", &[], CancellationToken::new()).await.unwrap();

        let cwd = env.lessons_markdown;
        assert_ne!(cwd, "/", "CLI ran at the filesystem root");
        let listing = std::fs::read_dir(&cwd).map(|d| d.count()).unwrap_or(0);
        assert_eq!(listing, 0, "scratch dir should be empty, {cwd} had {listing} entries");
    }

    #[test]
    fn dig_unwrap_reads_a_single_object() {
        let got = dig_unwrap(r#"{"type":"result","result":"hello"}"#, "result").unwrap();
        assert_eq!(got, "hello");
    }

    #[test]
    fn dig_unwrap_reads_an_event_array() {
        // qwen -o json shape, observed 2026-07-28.
        let raw = r#"[{"type":"init"},{"type":"result","result":"hello","is_error":false}]"#;
        assert_eq!(dig_unwrap(raw, "result").unwrap(), "hello");
    }

    #[test]
    fn dig_unwrap_takes_the_last_matching_event() {
        let raw = r#"[{"result":"first"},{"result":"final"}]"#;
        assert_eq!(dig_unwrap(raw, "result").unwrap(), "final");
    }

    #[test]
    fn dig_unwrap_reports_a_missing_field_rather_than_guessing() {
        let err = dig_unwrap(r#"[{"type":"init"}]"#, "result").unwrap_err();
        assert!(err.contains("no string field 'result'"), "got: {err}");
    }

    /// Unmeasured: whether `claude --json-schema` returns `result` as a JSON
    /// string or a parsed object/array. `claude` is Jodd's DEFAULT preset and
    /// is `OutputFidelity::Structured`, so it gets no lenient-nudge retry —
    /// an object-valued `result` must be unwrapped, not rejected, or every
    /// Extract and auto-link call on the default preset breaks permanently
    /// the moment that shape shows up.
    #[test]
    fn dig_unwrap_stringifies_a_non_string_result_field() {
        let got = dig_unwrap(r#"{"type":"result","result":{"a":1,"b":2}}"#, "result").unwrap();
        assert_eq!(got, r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn missing_binary_is_not_configured() {
        let err = AgentCliProvider::new(spec_for("definitely-not-a-real-binary-xyz".into())).unwrap_err();
        match err {
            ExtractError::NotConfigured(m) => assert!(m.contains("definitely-not-a-real-binary-xyz")),
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn schema_placeholders_fill_inline_json_and_a_path() {
        let args = vec![
            "--json-schema".to_string(),
            "{schema_json}".to_string(),
            "--output-schema".to_string(),
            "{schema_file}".to_string(),
        ];
        let sub = SchemaSubstitution {
            json: r#"{"type":"object"}"#.to_string(),
            path: "/tmp/s.json".to_string(),
        };
        let got = substitute(&args, "sys", "full", None, Some(&sub));
        assert_eq!(got[1], r#"{"type":"object"}"#);
        assert_eq!(got[3], "/tmp/s.json");
    }

    /// With no schema in play the placeholders are emitted verbatim, exactly
    /// as `{out_file}` already does — a misconfigured Custom spec must fail
    /// visibly at the CLI, not pass an empty argument that looks valid.
    #[test]
    fn schema_placeholders_survive_verbatim_when_no_schema_is_supplied() {
        let args = vec!["--json-schema".to_string(), "{schema_json}".to_string()];
        let got = substitute(&args, "sys", "full", None, None);
        assert_eq!(got[1], "{schema_json}");
    }

    /// chat() has no schema (Ask Jodd returns prose), so the schema argv
    /// fragment must be absent entirely rather than present-and-empty.
    #[tokio::test]
    async fn schema_args_are_omitted_when_the_caller_supplies_no_schema() {
        let dir = tempfile::tempdir().unwrap();
        // echoes its own argv so the test can assert on what was passed
        let bin = fake_cli(
            dir.path(),
            "argv_echo",
            r#"echo "{\"result\":\"$*\"}""#,
            r#"echo {"result":"%*"}"#,
        );
        let mut spec = spec_for(bin);
        spec.schema_args = vec!["--json-schema".into(), "{schema_json}".into()];
        let p = AgentCliProvider::new(spec).unwrap();
        let out = p
            .run_once("sys", "payload", None, CancellationToken::new())
            .await
            .unwrap();
        assert!(
            !out.contains("--json-schema"),
            "schema args must be omitted with no schema; got: {out}"
        );
    }

    /// The positive case `schema_args_are_omitted_when_the_caller_supplies_no_schema`
    /// doesn't cover: a spec with a non-empty `schema_args` and a real schema
    /// must actually reach the child process's argv, placeholder filled in.
    #[tokio::test]
    async fn schema_args_reach_the_child_process_when_a_schema_is_supplied() {
        let dir = tempfile::tempdir().unwrap();
        // echoes its own argv so the test can assert on what was passed
        let bin = fake_cli(
            dir.path(),
            "argv_echo_schema",
            r#"echo "{\"result\":\"$*\"}""#,
            r#"echo {"result":"%*"}"#,
        );
        let mut spec = spec_for(bin);
        spec.schema_args = vec!["--json-schema".into(), "{schema_json}".into()];
        let p = AgentCliProvider::new(spec).unwrap();
        let schema = r#"{"type":"object"}"#;
        let out = p
            .run_once("sys", "payload", Some(schema), CancellationToken::new())
            .await
            .unwrap();
        assert!(
            out.contains("--json-schema"),
            "schema flag must reach the child process; got: {out}"
        );
        assert!(
            out.contains(schema),
            "the filled schema JSON must reach the child process; got: {out}"
        );
    }

    /// Every string below was CAPTURED from a real failing run on
    /// 2026-08-30, verbatim. Paraphrasing them is how a dictionary passes its
    /// own tests and still fails the user.
    #[test]
    fn diagnose_names_the_measured_claude_failures() {
        let retries = diagnose(
            "claude",
            "claude exit exit status: 1: terminal_reason structured_output_retry_exhausted",
        )
        .expect("retry exhaustion is in the dictionary");
        assert!(retries.cause.to_lowercase().contains("shape"));
        assert!(!retries.action.is_empty());

        let guard = diagnose(
            "claude",
            "API Error: Opus 5 (1M context)'s safeguards flagged this message (https://www.anthropic.com/legal/aup).",
        )
        .expect("a safeguard trip is in the dictionary");
        assert!(guard.action.to_lowercase().contains("model"));

        let login = diagnose("claude", "Not logged in \u{b7} Please run /login")
            .expect("a signed-out CLI is in the dictionary");
        assert!(login.action.to_lowercase().contains("sign in"));
    }

    #[test]
    fn diagnose_names_the_measured_opencode_failure() {
        let raw = "opencode exit exit status: 1: Error: { \"name\": \"UnknownError\", \"data\": { \"message\": \"Unexpected server error. Check server logs for details.\", \"ref\": \"err_5558b681\" } }";
        let d = diagnose("opencode", raw).expect("opencode misconfiguration is in the dictionary");
        assert!(d.action.contains("opencode auth list"));
    }

    /// Both needles must be present. opencode's message is generic enough
    /// that matching on "UnknownError" alone would fire on unrelated faults.
    #[test]
    fn diagnose_requires_every_needle_of_an_entry() {
        assert!(diagnose("opencode", "Error: { \"name\": \"UnknownError\" }").is_none());
    }

    /// A signature belongs to the preset that produced it: claude's wording
    /// must not be used to explain an opencode failure.
    #[test]
    fn diagnose_does_not_borrow_another_presets_signatures() {
        assert!(diagnose("codex", "Not logged in \u{b7} Please run /login").is_none());
    }

    /// The no-wildcard rule. An unknown failure gets no invented explanation.
    #[test]
    fn an_unrecognised_failure_is_not_guessed_at() {
        assert!(diagnose("claude", "some error nobody has ever seen").is_none());
        assert!(diagnose("nosuchpreset", "structured_output_retry_exhausted").is_none());
        // A Custom agent-CLI spec's preset id is the literal "custom", which
        // is in no failure-signature table — named explicitly so this isn't
        // an accident of "nosuchpreset" happening to also miss.
        assert!(diagnose("custom", "structured_output_retry_exhausted").is_none());
    }
}
