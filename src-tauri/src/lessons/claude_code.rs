//! Claude Code subprocess provider — `claude -p` with stdin/stdout.
//!
//! Empirical note (verified 2026-06-13, claude 1.0.24): `claude -p --output-format json`
//! emits a single JSON object on stdout with fields:
//!   {"type":"result","subtype":"success","is_error":false,"duration_ms":...,
//!    "result":"<model text>","session_id":"...","total_cost_usd":...,"usage":{...}}
//! The model's actual output is in the `result` field as a string (re-parse for JSON).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::lessons::prompt::SYSTEM_PROMPT;
use crate::lessons::provider::{ExtractEnvelope, ExtractError, LessonProvider};

pub struct ClaudeCodeProvider {
    pub binary_path: PathBuf,
    pub timeout: Duration,
}

#[derive(Deserialize)]
struct ClaudeOutput {
    result: String,
}

impl ClaudeCodeProvider {
    /// Tries to resolve the `claude` binary via PATH. Returns None if not found.
    pub fn detect() -> Option<Self> {
        which::which("claude").ok().map(|p| Self {
            binary_path: p,
            timeout: Duration::from_secs(120),
        })
    }
}

#[async_trait::async_trait]
impl LessonProvider for ClaudeCodeProvider {
    async fn extract(
        &self,
        source: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ExtractEnvelope, ExtractError> {
        let prompt = format!("{SYSTEM_PROMPT}\n\n---\n\n{source}");

        let mut child = Command::new(&self.binary_path)
            .arg("-p")
            .arg("--output-format")
            .arg("json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // start_kill via Child::start_kill works only if we kept a handle
            // to the Child rather than consuming it via wait_with_output below
            // — see the select! block.
            .spawn()
            .map_err(|e| ExtractError::Transport(format!("spawn claude: {e}")))?;

        // Write prompt to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| ExtractError::Transport(format!("stdin write: {e}")))?;
            // Closing stdin signals end-of-input
            drop(stdin);
        }

        // Take stdout/stderr handles up front so we can read them after the
        // wait without needing wait_with_output (which consumes child and
        // would prevent the cancel branch from calling start_kill).
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        // Race the subprocess wait against timeout AND cancellation. On
        // cancel: kill the child cleanly (start_kill is async-friendly and
        // doesn't block) so the orphaned process doesn't keep burning the
        // user's Claude Code subscription budget.
        let exit_status = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(ExtractError::Cancelled);
            }
            r = tokio::time::timeout(self.timeout, child.wait()) => {
                r.map_err(|_| ExtractError::Transport("claude -p timed out".into()))?
                 .map_err(|e| ExtractError::Transport(format!("wait: {e}")))?
            }
        };

        // Process exited; drain stdout and stderr.
        use tokio::io::AsyncReadExt;
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
            return Err(ExtractError::UpstreamError(format!(
                "claude -p exit {exit_status}: {stderr}"
            )));
        }

        let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();

        // Step 1: parse the Claude Code envelope (the `result` field holds the model output)
        let wrapper: ClaudeOutput = serde_json::from_str(&stdout).map_err(|e| {
            ExtractError::MalformedEnvelope {
                reason: format!("claude wrapper: {e}"),
                raw: stdout.clone(),
            }
        })?;

        // Step 2: parse the model's output as the lesson envelope.
        //
        // The model SHOULD return raw JSON (the system prompt is emphatic about
        // this), but Claude Code's `--output-format json` only constrains the
        // outer wrapper — the inner `result` string is whatever the model
        // produced. Empirically, the model occasionally wraps the JSON in a
        // ```json fence or prepends a sentence of prose. parse_envelope_lenient
        // handles those cases without giving up; only if all extraction
        // strategies fail do we return MalformedEnvelope.
        parse_envelope_lenient(&wrapper.result).map_err(|reason| {
            ExtractError::MalformedEnvelope {
                reason,
                raw: wrapper.result,
            }
        })
    }
}

/// Try several strategies to extract a valid `ExtractEnvelope` from a string
/// that should contain JSON but may have extra text around it.
///
/// Order of attempts:
///   1. Direct parse — the well-behaved case
///   2. Strip a leading/trailing markdown code fence (```json ... ``` or ``` ... ```)
///   3. Slice from the first `{` to its matching `}` via brace-counting
///
/// Returns the original parse error message if every strategy fails.
fn parse_envelope_lenient(s: &str) -> Result<ExtractEnvelope, String> {
    // Strategy 1: direct parse on the raw string (also handles surrounding whitespace)
    let trimmed = s.trim();
    if let Ok(env) = serde_json::from_str::<ExtractEnvelope>(trimmed) {
        return Ok(env);
    }

    // Strategy 2: strip a markdown code fence if present
    if let Some(unfenced) = strip_code_fence(trimmed) {
        if let Ok(env) = serde_json::from_str::<ExtractEnvelope>(unfenced.trim()) {
            return Ok(env);
        }
    }

    // Strategy 3: brace-balanced slice from first `{` to its matching `}`
    if let Some(sliced) = find_first_balanced_json_object(trimmed) {
        if let Ok(env) = serde_json::from_str::<ExtractEnvelope>(sliced) {
            return Ok(env);
        }
    }

    // All strategies failed. Re-run the direct parse to get a useful error message.
    Err(format!(
        "inner json (after lenient parse): {}",
        serde_json::from_str::<ExtractEnvelope>(trimmed).unwrap_err()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_none_when_binary_absent() {
        // Override PATH to a directory that doesn't contain `claude`
        let dir = tempfile::tempdir().unwrap();
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        let result = ClaudeCodeProvider::detect();
        if let Some(orig) = original {
            std::env::set_var("PATH", orig);
        }
        assert!(result.is_none());
    }

    const MINIMAL_JSON: &str = "{\"lessons_markdown\": \"## L1\\nbody\"}";

    #[test]
    fn lenient_parses_clean_json() {
        let env = parse_envelope_lenient(MINIMAL_JSON).expect("ok");
        assert_eq!(env.lessons_markdown, "## L1\nbody");
    }

    #[test]
    fn lenient_parses_json_with_surrounding_whitespace() {
        let s = format!("\n\n  {}  \n", MINIMAL_JSON);
        parse_envelope_lenient(&s).expect("whitespace ok");
    }

    #[test]
    fn lenient_parses_json_in_markdown_fence_with_lang() {
        let s = format!("```json\n{}\n```", MINIMAL_JSON);
        parse_envelope_lenient(&s).expect("fenced ok");
    }

    #[test]
    fn lenient_parses_json_in_markdown_fence_no_lang() {
        let s = format!("```\n{}\n```", MINIMAL_JSON);
        parse_envelope_lenient(&s).expect("plain fence ok");
    }

    #[test]
    fn lenient_parses_json_after_prose_preamble() {
        let s = format!("Here is the JSON object you asked for:\n\n{}", MINIMAL_JSON);
        parse_envelope_lenient(&s).expect("prose preamble ok");
    }

    #[test]
    fn lenient_parses_json_with_trailing_prose() {
        let s = format!("{}\n\nHope that helps!", MINIMAL_JSON);
        parse_envelope_lenient(&s).expect("trailing prose ok");
    }

    #[test]
    fn lenient_handles_braces_inside_strings() {
        // The lessons_markdown body contains a `{` literal, which a naive
        // brace counter would miscount. The string-tracking logic must
        // ignore braces inside JSON string literals.
        let json = "{\"lessons_markdown\": \"## L1\\n```\\nfn x() { y(); }\\n```\\n\"}";
        let s = format!("preamble {} trailing", json);
        let env = parse_envelope_lenient(&s).expect("braces-in-string ok");
        assert!(env.lessons_markdown.contains("fn x() { y(); }"));
    }

    #[test]
    fn lenient_handles_escaped_quotes_inside_strings() {
        // Escaped quote inside the JSON string MUST NOT toggle the in-string flag,
        // otherwise the brace counter sees fake structure.
        let json = "{\"lessons_markdown\": \"He said \\\"hi {then} bye\\\".\"}";
        let s = format!("noise {} more", json);
        parse_envelope_lenient(&s).expect("escaped quotes ok");
    }

    #[test]
    fn lenient_fails_on_completely_invalid_input() {
        let err = parse_envelope_lenient("this is just prose with no json at all").expect_err("fail");
        assert!(err.contains("inner json"));
    }

    #[test]
    fn lenient_fails_on_unbalanced_braces() {
        let err = parse_envelope_lenient("{ \"lessons_markdown\": \"x\" ").expect_err("fail");
        assert!(err.contains("inner json"));
    }
}
