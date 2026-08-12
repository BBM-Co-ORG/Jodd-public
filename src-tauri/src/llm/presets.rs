//! The shipped agent-CLI preset table.
//!
//! Kept separate from `agent_cli.rs` so that editing a row cannot disturb the
//! runner's tests. Every row carries an Empirical note recording whether it
//! was exercised against a real binary, and what was observed.

use serde::Serialize;

use crate::llm::agent_cli::{AgentCliSpec, OutputFidelity, OutputSource, PromptDelivery};

/// Whether this CLI could actually be driven end-to-end when the table was
/// last verified. `which` finding the binary is NOT the same thing: gemini,
/// qwen and aider are all installed here and all fail at their own auth step,
/// so showing them as available would send the user into a dead end with an
/// error that has nothing to do with Jodd.
pub enum Availability {
    /// Exercised end-to-end against the real binary.
    Verified,
    /// Present on disk but unusable as configured. The text is shown to the
    /// user verbatim, so it must say what THEY can do about it.
    NotAvailable(&'static str),
}

pub struct AgentCliPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub availability: Availability,
    pub spec: AgentCliSpec,
}

fn a(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

pub fn all_presets() -> Vec<AgentCliPreset> {
    vec![
        // Empirical note (verified 2026-06-13, claude 1.0.24): `claude -p
        // --output-format json` emits ONE JSON object on stdout whose
        // `result` field holds the model's text as a string.
        AgentCliPreset {
            id: "claude",
            label: "Claude Code",
            availability: Availability::Verified,
            spec: AgentCliSpec {
                binary: "claude".into(),
                args: a(&["-p", "--output-format", "json"]),
                prompt_delivery: PromptDelivery::StdinAll,
                output: OutputSource::Stdout,
                unwrap: Some("result".into()),
                fidelity: OutputFidelity::Structured,
                timeout_secs: 120,
            },
        },
        // Empirical note (verified 2026-07-28, codex on macOS): `codex exec`
        // refuses to start outside a trusted directory unless
        // `--skip-git-repo-check` is passed — without it the run dies
        // immediately with "Not inside a trusted directory". stdout is full of
        // hook chatter and token counts, but `--output-last-message` writes
        // ONLY the final assistant message to the given path, with no wrapper
        // and no ANSI. That is why this row reads a file instead of stdout.
        AgentCliPreset {
            id: "codex",
            label: "Codex CLI",
            availability: Availability::Verified,
            spec: AgentCliSpec {
                binary: "codex".into(),
                args: a(&[
                    "exec",
                    "--sandbox",
                    "read-only",
                    "--skip-git-repo-check",
                    "--output-last-message",
                    "{out_file}",
                    "-",
                ]),
                prompt_delivery: PromptDelivery::StdinAll,
                output: OutputSource::LastMessageFile,
                unwrap: None,
                fidelity: OutputFidelity::Structured,
                timeout_secs: 180,
            },
        },
        // Empirical note (partially verified 2026-07-28, qwen): `-o json`
        // emits a JSON **array** of event objects, not a single object —
        // observed shape `[{"type":"result","result":...,"is_error":...}]`.
        // The unwrap therefore has to search an array; see `dig_unwrap` in
        // agent_cli.rs. The success path could NOT be exercised because this
        // machine has no qwen auth configured ("No auth type is selected"),
        // so the `result` field name is inferred from the error envelope.
        AgentCliPreset {
            id: "qwen",
            label: "Qwen Code",
            availability: Availability::NotAvailable(
                "No auth type is configured. Run `qwen` once and pick a sign-in method, then reopen this dialog.",
            ),
            spec: AgentCliSpec {
                binary: "qwen".into(),
                args: a(&["-o", "json", "--approval-mode", "plan", "-p", "{system}"]),
                prompt_delivery: PromptDelivery::StdinPayloadSystemArg,
                output: OutputSource::Stdout,
                unwrap: Some("result".into()),
                fidelity: OutputFidelity::Structured,
                timeout_secs: 120,
            },
        },
        // Empirical note: UNVERIFIED. Gemini CLI is installed here but its
        // account tier was revoked upstream on 2026-07-28 ("IneligibleTierError:
        // This client is no longer supported for Gemini Code Assist for
        // individuals"), so no successful run could be observed. `--help`
        // confirms `-p` ("Appended to input on stdin (if any)"), `-o json`
        // and `--approval-mode plan`. Qwen Code is a Gemini CLI fork and its
        // `-o json` returns an event array, so this row assumes the same.
        AgentCliPreset {
            id: "gemini",
            label: "Gemini CLI",
            availability: Availability::NotAvailable(
                "Google ended Gemini Code Assist for individual accounts, so signing in with a personal Google account no longer works. Set GEMINI_API_KEY (aistudio.google.com/apikey) to use it.",
            ),
            spec: AgentCliSpec {
                binary: "gemini".into(),
                args: a(&["-o", "json", "--approval-mode", "plan", "-p", "{system}"]),
                prompt_delivery: PromptDelivery::StdinPayloadSystemArg,
                output: OutputSource::Stdout,
                unwrap: Some("response".into()),
                fidelity: OutputFidelity::Structured,
                timeout_secs: 120,
            },
        },
        // Empirical note (verified 2026-07-28, opencode 1.3.13): `opencode run
        // '<prompt>'` works, and it checks whether stdout is a TTY. Piped —
        // which is how Jodd always invokes it — the output is the model's
        // text ALONE: no ANSI, no "> build · <model>" header, nothing to
        // strip. Interactively it decorates. `run --format json` emits a raw
        // event stream rather than a single wrapper, so this row stays on the
        // default format and lets parse_envelope_lenient take the JSON.
        // Kept `Heuristic` deliberately: opencode publishes no output
        // contract, so the one retry is cheap insurance, and it costs nothing
        // when the first parse succeeds.
        //
        // Note for support: opencode resolves the model from the USER's
        // ~/.config/opencode/opencode.jsonc. If that names a provider they
        // have not authenticated, every run dies with "Model not found:
        // <model>" before Jodd is involved.
        AgentCliPreset {
            id: "opencode",
            label: "opencode",
            availability: Availability::Verified,
            spec: AgentCliSpec {
                binary: "opencode".into(),
                args: a(&["run", "{prompt}"]),
                prompt_delivery: PromptDelivery::Argv,
                output: OutputSource::Stdout,
                unwrap: None,
                fidelity: OutputFidelity::Heuristic,
                timeout_secs: 180,
            },
        },
        // Empirical note: UNVERIFIED — not exercised on this machine.
        AgentCliPreset {
            id: "aider",
            label: "Aider",
            availability: Availability::NotAvailable(
                "Its API key was rejected (OpenRouter returned 401 \"User not found\"). Configure a working key for aider first.",
            ),
            spec: AgentCliSpec {
                binary: "aider".into(),
                args: a(&["--no-auto-commits", "--yes", "--message", "{prompt}"]),
                prompt_delivery: PromptDelivery::Argv,
                output: OutputSource::Stdout,
                unwrap: None,
                fidelity: OutputFidelity::Heuristic,
                timeout_secs: 180,
            },
        },
    ]
}

pub fn preset_by_id(id: &str) -> Option<AgentCliSpec> {
    all_presets().into_iter().find(|p| p.id == id).map(|p| p.spec)
}

#[derive(Serialize, Debug, Clone)]
pub struct AgentCliPresetInfo {
    pub id: String,
    pub label: String,
    /// "structured" | "heuristic"
    pub fidelity: String,
    pub installed: bool,
    pub resolved_path: Option<String>,
    /// False when the CLI is present but known to be unusable as configured.
    pub available: bool,
    /// User-facing explanation, present iff `available` is false.
    pub unavailable_reason: Option<String>,
}

/// Presence is resolved live on every call rather than cached: a user may
/// install a CLI while Jodd is running, and `which` is cheap.
pub fn preset_infos() -> Vec<AgentCliPresetInfo> {
    all_presets()
        .into_iter()
        .map(|p| {
            let resolved = which::which(&p.spec.binary)
                .ok()
                .map(|x| x.to_string_lossy().into_owned());
            AgentCliPresetInfo {
                id: p.id.to_string(),
                label: p.label.to_string(),
                fidelity: match p.spec.fidelity {
                    OutputFidelity::Structured => "structured".into(),
                    OutputFidelity::Heuristic => "heuristic".into(),
                },
                installed: resolved.is_some(),
                resolved_path: resolved,
                available: matches!(p.availability, Availability::Verified),
                unavailable_reason: match p.availability {
                    Availability::Verified => None,
                    Availability::NotAvailable(why) => Some(why.to_string()),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claude preset must reproduce the argv that shipped in v0.16.1,
    /// because existing accounts.json files say "claude_code" and are never
    /// rewritten. If this fails, existing users' extraction changed.
    #[test]
    fn claude_preset_matches_the_historical_hardcoded_argv() {
        let spec = preset_by_id("claude").expect("claude preset exists");
        assert_eq!(spec.binary, "claude");
        assert_eq!(spec.args, vec!["-p", "--output-format", "json"]);
        assert_eq!(spec.prompt_delivery, PromptDelivery::StdinAll);
        assert_eq!(spec.output, OutputSource::Stdout);
        assert_eq!(spec.unwrap.as_deref(), Some("result"));
        assert_eq!(spec.timeout_secs, 120);
    }

    /// codex refuses to run outside a trusted directory without this flag.
    /// Observed 2026-07-28: omitting it fails instantly with
    /// "Not inside a trusted directory and --skip-git-repo-check was not
    /// specified."
    #[test]
    fn codex_preset_passes_the_git_repo_check_escape() {
        let spec = preset_by_id("codex").expect("codex preset exists");
        assert!(
            spec.args.iter().any(|a| a == "--skip-git-repo-check"),
            "codex will not start without --skip-git-repo-check"
        );
        assert_eq!(spec.output, OutputSource::LastMessageFile);
    }

    #[test]
    fn preset_ids_are_unique() {
        let mut ids: Vec<&str> = all_presets().iter().map(|p| p.id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "duplicate preset id");
    }

    /// These invariants exist so that editing one table row wrongly fails
    /// here rather than at a user's runtime.
    #[test]
    fn every_preset_declares_the_placeholders_its_modes_require() {
        for p in all_presets() {
            let joined = p.spec.args.join(" ");
            match p.spec.prompt_delivery {
                PromptDelivery::Argv => assert!(
                    joined.contains("{prompt}"),
                    "preset '{}' delivers via argv but has no {{prompt}}",
                    p.id
                ),
                PromptDelivery::StdinPayloadSystemArg => assert!(
                    joined.contains("{system}"),
                    "preset '{}' puts the system prompt in argv but has no {{system}}",
                    p.id
                ),
                PromptDelivery::StdinAll => assert!(
                    !joined.contains("{prompt}") && !joined.contains("{system}"),
                    "preset '{}' sends everything on stdin but also has a prompt placeholder",
                    p.id
                ),
            }
            if p.spec.output == OutputSource::LastMessageFile {
                assert!(
                    joined.contains("{out_file}"),
                    "preset '{}' reads an output file but never passes {{out_file}}",
                    p.id
                );
            }
        }
    }

    /// Every row that is not Verified must explain itself, and the text has
    /// to tell the user what to do — a bare "unavailable" sends them to us.
    #[test]
    fn unavailable_presets_carry_an_actionable_reason() {
        for i in preset_infos() {
            if i.available {
                assert!(i.unavailable_reason.is_none(), "{} is available but has a reason", i.id);
            } else {
                let why = i.unavailable_reason.unwrap_or_default();
                assert!(why.len() > 30, "{}: reason too vague: {why:?}", i.id);
            }
        }
    }

    #[test]
    fn unknown_preset_id_returns_none() {
        assert!(preset_by_id("no-such-cli").is_none());
    }

    #[test]
    fn preset_info_reports_installation_status_consistently() {
        let infos = preset_infos();
        assert!(!infos.is_empty());
        for i in &infos {
            assert_eq!(i.installed, i.resolved_path.is_some());
        }
    }
}
