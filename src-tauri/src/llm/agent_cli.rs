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
    pub prompt_delivery: PromptDelivery,
    pub output: OutputSource,
    /// JSON field holding the model text (`"result"`, `"response"`).
    /// `None` means the output *is* the text.
    pub unwrap: Option<String>,
    pub fidelity: OutputFidelity,
    pub timeout_secs: u64,
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

/// Expand `{system}`, `{prompt}` and `{out_file}` in an argv template.
///
/// Performs a single left-to-right pass, never rescanning substituted text,
/// so payload containing literal `{system}`, `{prompt}`, or `{out_file}` is
/// not corrupted. `{out_file}` is left verbatim when no temp path exists, so
/// a Custom spec that asks for a file while configured for stdout fails
/// visibly at the CLI rather than silently passing an empty argument.
pub fn substitute(
    args: &[String],
    system: &str,
    full_prompt: &str,
    out_file: Option<&str>,
) -> Vec<String> {
    const PLACEHOLDERS: [&str; 3] = ["{system}", "{prompt}", "{out_file}"];
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

/// Try several strategies to extract a valid `T` from a string that should
/// contain JSON but may have extra text around it. Generic over any
/// DeserializeOwned type — used for both ExtractEnvelope (extract) and
/// LinkSuggestionsEnvelope (suggest_links), same lenient-parsing contract.
///
/// Order of attempts:
///   1. Direct parse — the well-behaved case
///   2. Strip a leading/trailing markdown code fence (```json ... ``` or ``` ... ```)
///   3. Slice from the first `{` to its matching `}` via brace-counting
///
/// Returns the original parse error message if every strategy fails.
pub(crate) fn parse_envelope_lenient<T: serde::de::DeserializeOwned + std::fmt::Debug>(s: &str) -> Result<T, String> {
    // Strategy 1: direct parse on the raw string (also handles surrounding whitespace)
    let trimmed = s.trim();
    if let Ok(env) = serde_json::from_str::<T>(trimmed) {
        return Ok(env);
    }

    // Strategy 2: strip a markdown code fence if present
    if let Some(unfenced) = strip_code_fence(trimmed) {
        if let Ok(env) = serde_json::from_str::<T>(unfenced.trim()) {
            return Ok(env);
        }
    }

    // Strategy 3: brace-balanced slice from first `{` to its matching `}`
    if let Some(sliced) = find_first_balanced_json_object(trimmed) {
        if let Ok(env) = serde_json::from_str::<T>(sliced) {
            return Ok(env);
        }
    }

    // All strategies failed. Re-run the direct parse to get a useful error message.
    Err(format!(
        "inner json (after lenient parse): {}",
        serde_json::from_str::<T>(trimmed).unwrap_err()
    ))
}

/// Strips matching ```lang ... ``` or ``` ... ``` fences. Returns the inner
/// content. Returns None if the input doesn't start with a fence.
fn strip_code_fence(s: &str) -> Option<&str> {
    let s = s.trim();
    let after_open = s.strip_prefix("```")?;
    // Skip optional language tag on the first line (e.g. "json\n...")
    let body_start = after_open.find('\n').map(|i| i + 1).unwrap_or(0);
    let body = &after_open[body_start..];
    // Trim trailing ``` (and any whitespace after it)
    let body = body.trim_end();
    let body = body.strip_suffix("```").unwrap_or(body);
    Some(body)
}

/// Finds the first `{` in `s` and returns a slice ending at the matching `}`.
/// Performs naive brace counting; ignores braces inside JSON string literals
/// (handles escaped quotes correctly). Returns None if no balanced object found.
fn find_first_balanced_json_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for i in start..bytes.len() {
        let b = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::llm::prompt::{LINK_SUGGESTION_SYSTEM_PROMPT, SYSTEM_PROMPT};
use crate::llm::provider::{
    CandidateSummary, ChatTurn, ExtractEnvelope, ExtractError, LinkSuggestionsEnvelope, LlmProvider,
};

/// Agent CLIs log heavily; a full stderr dump would swamp the error toast.
const STDERR_TAIL: usize = 2000;

/// Appended to the system prompt on the single retry a Heuristic preset gets.
const JSON_ONLY_NUDGE: &str =
    "IMPORTANT: reply with the raw JSON object only. No prose before or after \
     it, no markdown code fence, no explanation.";

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
        cancel: CancellationToken,
    ) -> Result<T, ExtractError> {
        let first = self.run_once(system, payload, cancel.clone()).await;

        // For Heuristic presets, retry on any MalformedEnvelope from run_once
        // (e.g., empty output or dig_unwrap failure), not just parse failures.
        if let Err(ExtractError::MalformedEnvelope { .. }) = &first {
            if self.spec.fidelity == OutputFidelity::Heuristic {
                let nudged = format!("{system}\n\n{JSON_ONLY_NUDGE}");
                let retry_raw = self.run_once(&nudged, payload, cancel).await?;
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
                let nudged = format!("{system}\n\n{JSON_ONLY_NUDGE}");
                let retry_raw = self.run_once(&nudged, payload, cancel).await?;
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

        let args = substitute(&self.spec.args, system, &full, out_path.as_deref());

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
            // Slice on a char boundary: `&s[n..]` panics if n splits a
            // multi-byte character, and a CLI printing Thai or emoji to
            // stderr is entirely realistic.
            let want = stderr.len().saturating_sub(STDERR_TAIL);
            let start = (want..=stderr.len())
                .find(|&i| stderr.is_char_boundary(i))
                .unwrap_or(stderr.len());
            let tail = &stderr[start..];
            return Err(ExtractError::UpstreamError(format!(
                "{} exit {exit_status}: {tail}",
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
        cancel: CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        self.run_json(SYSTEM_PROMPT, source, cancel).await
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
        self.run_json(LINK_SUGGESTION_SYSTEM_PROMPT, &request_json, cancel)
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
        self.run_once(system, &payload, cancel).await
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
        serde_json::Value::Array(items) => items
            .iter()
            .rev()
            .find_map(|item| item.get(field).and_then(|x| x.as_str())),
        other => other.get(field).and_then(|x| x.as_str()),
    };

    found.map(str::to_string).ok_or_else(|| {
        format!(
            "wrapper has no string field '{field}' ({})",
            match &v {
                serde_json::Value::Array(i) => format!("searched {} array elements", i.len()),
                _ => "top level is an object".to_string(),
            }
        )
    })
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
        let out = substitute(&args(&["-p", "{system}"]), "SYS", "SYS\n\nPAY", None);
        assert_eq!(out, vec!["-p".to_string(), "SYS".to_string()]);
    }

    #[test]
    fn substitutes_prompt_placeholder_with_the_full_prompt() {
        let out = substitute(&args(&["run", "{prompt}"]), "SYS", "SYS\n\nPAY", None);
        assert_eq!(out[1], "SYS\n\nPAY");
    }

    #[test]
    fn substitutes_out_file_placeholder() {
        let out = substitute(&args(&["--out", "{out_file}"]), "SYS", "P", Some("/tmp/x.txt"));
        assert_eq!(out[1], "/tmp/x.txt");
    }

    #[test]
    fn leaves_out_file_placeholder_alone_when_no_file_is_in_play() {
        // A Stdout-mode spec never gets a temp path; the literal must survive
        // untouched rather than becoming an empty string, so a misconfigured
        // Custom spec fails loudly at the CLI instead of silently passing "".
        let out = substitute(&args(&["--out", "{out_file}"]), "SYS", "P", None);
        assert_eq!(out[1], "{out_file}");
    }

    #[test]
    fn leaves_args_without_placeholders_untouched() {
        let out = substitute(&args(&["-p", "--output-format", "json"]), "SYS", "P", None);
        assert_eq!(out, args(&["-p", "--output-format", "json"]));
    }

    #[test]
    fn payload_containing_out_file_placeholder_is_not_rewritten() {
        let out = substitute(
            &args(&["-p", "{prompt}", "--out", "{out_file}"]),
            "SYS",
            "What does {out_file} mean?",
            Some("/tmp/real.txt"),
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
        );
        // The {prompt} arg should contain the literal {system} from the payload
        assert!(out[1].contains("{system}"));
        // It should NOT be replaced with "SYS"
        assert!(!out[1].contains("Explain SYS variables"));
    }

    #[test]
    fn adjacent_placeholders_are_each_substituted_once() {
        let out = substitute(
            &args(&["{system}{prompt}"]),
            "SYS",
            "PAYLOAD",
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

    const MINIMAL_JSON: &str = "{\"lessons_markdown\": \"## L1\\nbody\"}";

    #[test]
    fn lenient_parses_clean_json() {
        let env = parse_envelope_lenient::<ExtractEnvelope>(MINIMAL_JSON).expect("ok");
        assert_eq!(env.lessons_markdown, "## L1\nbody");
    }

    #[test]
    fn lenient_parses_json_with_surrounding_whitespace() {
        let s = format!("\n\n  {}  \n", MINIMAL_JSON);
        parse_envelope_lenient::<ExtractEnvelope>(&s).expect("whitespace ok");
    }

    #[test]
    fn lenient_parses_json_in_markdown_fence_with_lang() {
        let s = format!("```json\n{}\n```", MINIMAL_JSON);
        parse_envelope_lenient::<ExtractEnvelope>(&s).expect("fenced ok");
    }

    #[test]
    fn lenient_parses_json_in_markdown_fence_no_lang() {
        let s = format!("```\n{}\n```", MINIMAL_JSON);
        parse_envelope_lenient::<ExtractEnvelope>(&s).expect("plain fence ok");
    }

    #[test]
    fn lenient_parses_json_after_prose_preamble() {
        let s = format!("Here is the JSON object you asked for:\n\n{}", MINIMAL_JSON);
        parse_envelope_lenient::<ExtractEnvelope>(&s).expect("prose preamble ok");
    }

    #[test]
    fn lenient_parses_json_with_trailing_prose() {
        let s = format!("{}\n\nHope that helps!", MINIMAL_JSON);
        parse_envelope_lenient::<ExtractEnvelope>(&s).expect("trailing prose ok");
    }

    #[test]
    fn lenient_handles_braces_inside_strings() {
        // The lessons_markdown body contains a `{` literal, which a naive
        // brace counter would miscount. The string-tracking logic must
        // ignore braces inside JSON string literals.
        let json = "{\"lessons_markdown\": \"## L1\\n```\\nfn x() { y(); }\\n```\\n\"}";
        let s = format!("preamble {} trailing", json);
        let env = parse_envelope_lenient::<ExtractEnvelope>(&s).expect("braces-in-string ok");
        assert!(env.lessons_markdown.contains("fn x() { y(); }"));
    }

    #[test]
    fn lenient_handles_escaped_quotes_inside_strings() {
        // Escaped quote inside the JSON string MUST NOT toggle the in-string flag,
        // otherwise the brace counter sees fake structure.
        let json = "{\"lessons_markdown\": \"He said \\\"hi {then} bye\\\".\"}";
        let s = format!("noise {} more", json);
        parse_envelope_lenient::<ExtractEnvelope>(&s).expect("escaped quotes ok");
    }

    #[test]
    fn lenient_fails_on_completely_invalid_input() {
        let err = parse_envelope_lenient::<ExtractEnvelope>("this is just prose with no json at all").expect_err("fail");
        assert!(err.contains("inner json"));
    }

    #[test]
    fn lenient_fails_on_unbalanced_braces() {
        let err = parse_envelope_lenient::<ExtractEnvelope>("{ \"lessons_markdown\": \"x\" ").expect_err("fail");
        assert!(err.contains("inner json"));
    }

    #[test]
    fn lenient_parses_link_suggestions_envelope_generically() {
        // Confirms parse_envelope_lenient was generalized to work for ANY
        // DeserializeOwned type, not just ExtractEnvelope.
        let json = r#"{"suggestions":[{"uuid":"A","related":true,"should_append":false,"addition_text":null}]}"#;
        let env: crate::llm::provider::LinkSuggestionsEnvelope =
            parse_envelope_lenient(json).expect("ok");
        assert_eq!(env.suggestions.len(), 1);
        assert!(env.suggestions[0].related);
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
            prompt_delivery: PromptDelivery::StdinAll,
            output: OutputSource::Stdout,
            unwrap: None,
            fidelity: OutputFidelity::Structured,
            timeout_secs: 10,
        }
    }

    // 3 hashes: the JSON value itself starts with `"##`, which would
    // collide with a 1- or 2-hash raw-string terminator (`"#` / `"##`).
    const ENVELOPE: &str = r###"{"lessons_markdown":"## L1\nbody"}"###;

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
        let env = p.extract("source text", CancellationToken::new()).await.unwrap();
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
        let env = p.extract("source", CancellationToken::new()).await.unwrap();
        assert_eq!(env.lessons_markdown, "## L1\nbody");
    }

    #[tokio::test]
    async fn non_zero_exit_surfaces_stderr_as_upstream_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_cli(dir.path(), "boom", "echo 'kaboom' >&2; exit 3", "echo kaboom 1>&2& exit /b 3");

        let p = AgentCliProvider::new(spec_for(bin)).unwrap();
        let err = p.extract("s", CancellationToken::new()).await.unwrap_err();
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
        let err = p.extract(&huge, CancellationToken::new()).await.unwrap_err();
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
        let err = p.extract("s", CancellationToken::new()).await.unwrap_err();
        assert!(matches!(err, ExtractError::Transport(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn timeout_on_a_silent_cli_hints_at_interactive_input() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_cli(dir.path(), "mute", "sleep 30", "timeout /t 30 /nobreak >nul");

        let mut spec = spec_for(bin);
        spec.timeout_secs = 1;
        let p = AgentCliProvider::new(spec).unwrap();
        let err = p.extract("s", CancellationToken::new()).await.unwrap_err();
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
        let err = p.extract("s", cancel).await.unwrap_err();
        assert!(matches!(err, ExtractError::Cancelled), "got {err:?}");
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
        let env = p.extract("s", CancellationToken::new()).await.unwrap();
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
        let result = p.extract("s", CancellationToken::new()).await;
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
        let env = p.extract("s", CancellationToken::new()).await.unwrap();
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
        let env = p.extract("s", CancellationToken::new()).await.unwrap();
        assert_eq!(env.lessons_markdown, "## L1\nbody");
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
        p.extract("payload text", CancellationToken::new())
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
        let env = p.extract("s", CancellationToken::new()).await.unwrap();
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
        let err = p.extract("s", CancellationToken::new()).await.unwrap_err();
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
        let err = p.extract("s", CancellationToken::new()).await.unwrap_err();
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
        let env = p.extract("s", CancellationToken::new()).await.unwrap();

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

    #[test]
    fn missing_binary_is_not_configured() {
        let err = AgentCliProvider::new(spec_for("definitely-not-a-real-binary-xyz".into())).unwrap_err();
        match err {
            ExtractError::NotConfigured(m) => assert!(m.contains("definitely-not-a-real-binary-xyz")),
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }
}
