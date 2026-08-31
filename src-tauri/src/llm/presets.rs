//! The shipped agent-CLI preset table.
//!
//! Kept separate from `agent_cli.rs` so that editing a row cannot disturb the
//! runner's tests. Every row carries an Empirical note recording whether it
//! was exercised against a real binary, and what was observed.

use serde::Serialize;

use crate::llm::agent_cli::{AgentCliSpec, OutputFidelity, OutputSource, PromptDelivery};

/// Whether this CLI could actually be driven end-to-end when the table was
/// last verified. `which` finding the binary is NOT the same thing: qwen and
/// aider are installed here and both fail at their own auth step, so showing
/// them as available would send the user into a dead end with an error that
/// has nothing to do with Jodd. (Gemini was installed here too when this row
/// was first written, 2026-07-28, and failed the same way — but as of
/// 2026-08-29 it is not present on this machine at all, so its own row below
/// no longer claims installation, only that Google ended the account tier it
/// needed.)
pub enum Availability {
    /// Exercised end-to-end against the real binary.
    Verified,
    /// Present on disk but unusable as configured. The text is shown to the
    /// user verbatim, so it must say what THEY can do about it.
    NotAvailable(&'static str),
}

/// One measured way a CLI fails, and what the user can do about it.
///
/// `needles` must ALL appear (case-insensitively) for the entry to match:
/// opencode's message is generic enough that a single needle would fire on
/// unrelated faults. Entries are tried in order, so the most specific first.
///
/// Every entry must quote text observed from a real run — the same rule
/// gotcha #7 applies to the flags in this table. An invented signature is a
/// dictionary that lies confidently.
pub struct FailureSignature {
    pub needles: &'static [&'static str],
    /// What went wrong, in the user's terms. One sentence.
    pub cause: &'static str,
    /// What THEY can do next. One sentence, imperative.
    pub action: &'static str,
}

pub struct AgentCliPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub availability: Availability,
    pub spec: AgentCliSpec,
    /// Measured failure modes for THIS CLI. Empty is honest for a row nobody
    /// has seen fail yet.
    pub failure_signatures: &'static [FailureSignature],
}

fn a(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

pub fn all_presets() -> Vec<AgentCliPreset> {
    vec![
        // Empirical note (verified 2026-06-13, claude 1.0.24): `claude -p
        // --output-format json` emits ONE JSON object on stdout whose
        // `result` field holds the model's text as a string.
        //
        // Isolation flags added 2026-08-29 against claude 2.1.246 and
        // VERIFIED LIVE on 2026-08-30 on an authenticated session, which is
        // what the earlier "argument parsing verified only" note was waiting
        // for. This exact argv answers `is_error: false` with the model's
        // text, so `--setting-sources ''` and `--tools ''` do not disturb
        // the run: Jodd feeds the whole prompt on stdin and needs no tools.
        //
        // `--json-schema` is verified live too, and — unlike codex, where the
        // equivalent flag rejected Jodd's first schema outright (see the
        // codex row) — claude accepted it and returned an inner payload
        // carrying every property the schema requires, which deserializes
        // into `ExtractEnvelope`.
        //
        // One shape worth pinning, because a code path depends on it:
        // **`result` came back a STRING, not a parsed object**, so
        // `dig_unwrap`'s `.as_str()` is satisfied on the happy path. The
        // stringify fallback there is insurance against a future change, not
        // a workaround for observed behaviour.
        //
        // Measured effect, two rounds of scripts/measure-headless.sh:
        // local startup (wall minus `duration_api_ms`) falls from ~5.3-5.5 s
        // as-shipped to ~2 s isolated. The isolated variant was faster in
        // 4 runs out of 4, but one isolated run drifted to ~4.6 s — the
        // harness's own control spread 2.4 s that round, so treat ~3 s saved
        // as typical rather than guaranteed.
        AgentCliPreset {
            id: "claude",
            label: "Claude Code",
            availability: Availability::Verified,
            spec: AgentCliSpec {
                binary: "claude".into(),
                args: a(&[
                    "-p",
                    "--output-format",
                    "json",
                    // Isolation: without these, `claude -p` loads the user's
                    // settings.json, hooks, skills, plugins and every
                    // registered MCP server before reading the prompt.
                    // Deliberately NOT `--bare`, which reaches the same
                    // ground but stops reading OAuth/keychain entirely and
                    // demands ANTHROPIC_API_KEY — see spec §4.1.
                    "--strict-mcp-config",
                    "--setting-sources",
                    "",
                    // Jodd hands over the whole prompt on stdin and runs the
                    // child in an empty scratch dir; there is nothing for a
                    // file tool to find, and a turn spent looking is a turn
                    // wasted. "" disables all built-in tools.
                    //
                    // `--tools <tools...>` is VARIADIC and MUST stay last in
                    // `args`. Anything appended after it — which today means
                    // everything in `schema_args`, below — is silently
                    // swallowed as another tool name unless it starts with
                    // `-`. This is why `schema_args` begins with
                    // `--json-schema` rather than a bare value: that leading
                    // `-` is load-bearing and invisible from either row read
                    // in isolation.
                    "--tools",
                    "",
                ]),
                // `--json-schema` added 2026-08-29 against claude 2.1.246.
                // HELP-TEXT/ARG-PARSING VERIFIED ONLY: the flag was accepted
                // by the real binary, but this machine's session could not
                // authenticate, so no authenticated live run has exercised
                // its EFFECT on the output.
                // NO SCHEMA ON THIS ROW, and that is a measurement, not an
                // omission. `--json-schema` was added 2026-08-29 on the
                // strength of a one-line probe and shipped in v0.25.1; run
                // against Jodd's REAL Extract prompt on 2026-08-30 it broke
                // Extract on the default preset, two different ways:
                //
                //   model sonnet   -> subtype `error_max_structured_output_retries`
                //                     (`terminal_reason: structured_output_retry_exhausted`,
                //                     6 turns burned, `result: null`)
                //   model opus-5[1m] -> the request tripped a safety
                //                     classifier: "safeguards flagged this
                //                     message ... Details: [reasoning_extraction]"
                //
                // Neither is caused by `--tools ""` — tested as a matrix, the
                // schema fails with tools enabled too, and the SAME prompt on
                // the SAME model succeeds the moment the schema is dropped,
                // returning an ExtractEnvelope that deserializes cleanly.
                // Jodd's own `parse_envelope_lenient` has handled this row for
                // months and needs no help.
                //
                // The Extract system prompt already commands "your response
                // MUST be a single JSON object"; layering claude's structured
                // output on top of that is what fights. codex's
                // `--output-schema` is a different mechanism and is verified
                // working — see that row.
                schema_args: Vec::new(),
                prompt_delivery: PromptDelivery::StdinAll,
                output: OutputSource::Stdout,
                unwrap: Some("result".into()),
                fidelity: OutputFidelity::Structured,
                timeout_secs: 120,
            },
            failure_signatures: &[
                FailureSignature {
                    needles: &["structured_output_retry_exhausted"],
                    cause: "The model could not produce the answer in the shape Jodd asked for.",
                    action: "Try a different model in your CLI, then test again.",
                },
                FailureSignature {
                    needles: &["safeguards flagged this message"],
                    cause: "The model declined this request's content.",
                    action: "Try a different model in your CLI, then test again.",
                },
                FailureSignature {
                    needles: &["Not logged in"],
                    cause: "Claude Code is not signed in.",
                    action: "Run `claude` once in a terminal and sign in, then test again.",
                },
            ],
        },
        // Empirical note (verified 2026-07-28, codex on macOS): `codex exec`
        // refuses to start outside a trusted directory unless
        // `--skip-git-repo-check` is passed — without it the run dies
        // immediately with "Not inside a trusted directory". stdout is full of
        // hook chatter and token counts, but `--output-last-message` writes
        // ONLY the final assistant message to the given path, with no wrapper
        // and no ANSI. That is why this row reads a file instead of stdout.
        //
        // Added 2026-08-29 against codex-cli 0.147.0 and VERIFIED LIVE the
        // same day, end to end, on an authenticated session: this exact argv
        // (`--ignore-user-config --ephemeral --sandbox read-only
        // --skip-git-repo-check --output-last-message <f> -`) exits 0 and
        // writes the model's answer to the file. `--ignore-user-config` skips
        // `$CODEX_HOME/config.toml` — where `mcp_servers` also lives — and
        // its help's promise that "auth still uses CODEX_HOME" holds in
        // practice: the run authenticated normally. `--ephemeral` stops a
        // one-shot Jodd call persisting a session file.
        //
        // `--output-schema` is verified live too, and it is STRICTER than a
        // plain JSON Schema: it enforces OpenAI's strict structured-output
        // rules, answering `400 invalid_json_schema` ("'additionalProperties'
        // is required to be supplied and to be false") and exiting 1 with an
        // EMPTY last-message file when they are not met. Jodd's constants in
        // `provider.rs` satisfy them; `assert_strict_structured_output`
        // (provider.rs) is the pin that keeps them satisfying them. Do not
        // relax those schemas without re-running codex against them.
        AgentCliPreset {
            id: "codex",
            label: "Codex CLI",
            availability: Availability::Verified,
            spec: AgentCliSpec {
                binary: "codex".into(),
                args: a(&[
                    "exec",
                    // Skip ~/.codex/config.toml — which is also where
                    // mcp_servers lives. Safe here in a way claude's --bare
                    // is not: codex's help states "auth still uses
                    // CODEX_HOME", so the user's sign-in survives.
                    "--ignore-user-config",
                    // A Jodd call is one-shot; persisting a session file per
                    // Extract just accumulates on the user's disk.
                    "--ephemeral",
                    "--sandbox",
                    "read-only",
                    "--skip-git-repo-check",
                    "--output-last-message",
                    "{out_file}",
                    "-",
                ]),
                // `--output-schema` added 2026-08-29 against codex-cli
                // 0.147.0, from `codex exec --help`. HELP-TEXT/ARG-PARSING
                // VERIFIED ONLY — no authenticated live run has exercised
                // its EFFECT on the output.
                schema_args: a(&["--output-schema", "{schema_file}"]),
                prompt_delivery: PromptDelivery::StdinAll,
                output: OutputSource::LastMessageFile,
                unwrap: None,
                fidelity: OutputFidelity::Structured,
                timeout_secs: 180,
            },
            failure_signatures: &[FailureSignature {
                needles: &["invalid_json_schema"],
                cause: "Codex rejected the output schema Jodd sent.",
                action: "Please report this — Jodd is sending a shape this CLI will not accept.",
            }],
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
                schema_args: Vec::new(),
                prompt_delivery: PromptDelivery::StdinPayloadSystemArg,
                output: OutputSource::Stdout,
                unwrap: Some("result".into()),
                fidelity: OutputFidelity::Structured,
                timeout_secs: 120,
            },
            failure_signatures: &[],
        },
        // Empirical note: UNVERIFIED. Gemini CLI was installed on the machine
        // where this row was written (2026-07-28); it is not present as of
        // 2026-08-29. Its account tier was revoked upstream on 2026-07-28
        // ("IneligibleTierError: This client is no longer supported for
        // Gemini Code Assist for individuals"), so no successful run could be
        // observed even while it was installed. `--help` confirms `-p`
        // ("Appended to input on stdin (if any)"), `-o json` and
        // `--approval-mode plan`. Qwen Code is a Gemini CLI fork and its
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
                schema_args: Vec::new(),
                prompt_delivery: PromptDelivery::StdinPayloadSystemArg,
                output: OutputSource::Stdout,
                unwrap: Some("response".into()),
                fidelity: OutputFidelity::Structured,
                timeout_secs: 120,
            },
            failure_signatures: &[],
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
        // have not authenticated, every run dies before Jodd is involved.
        //
        // **The symptom is NOT self-explanatory, which is why this note now
        // quotes the real one.** It used to say the run dies with "Model not
        // found: <model>". Measured 2026-08-30 against opencode on a machine
        // whose config named `anthropic/claude-sonnet-4-5` while `opencode
        // auth list` held only IO.NET and Zhipu credentials, the actual
        // output is opaque and names neither the model nor the provider:
        //
        //     Error: { "name": "UnknownError", "data": { "message":
        //     "Unexpected server error. Check server logs for details.",
        //     "ref": "err_3055ad82" } }
        //
        // The `ref` changes every run, which makes it look like a transient
        // server fault rather than a permanent local misconfiguration. Proven
        // both directions: the same command with `--model
        // zhipuai-coding-plan/glm-5.2` (a provider that IS authenticated)
        // answers normally. Diagnose with `opencode auth list` against the
        // config's `model` field — the server log line to look for is
        // `model=undefined` on the created session.
        // `--pure` ("run without external plugins") added 2026-08-29 against
        // opencode 1.3.13, from `opencode run --help`. HELP-TEXT VERIFIED
        // ONLY. Narrower than claude's or codex's isolation: it does not
        // touch this CLI's own config or MCP registrations.
        AgentCliPreset {
            id: "opencode",
            label: "opencode",
            availability: Availability::Verified,
            spec: AgentCliSpec {
                binary: "opencode".into(),
                args: a(&["run", "--pure", "{prompt}"]),
                schema_args: Vec::new(),
                prompt_delivery: PromptDelivery::Argv,
                output: OutputSource::Stdout,
                unwrap: None,
                fidelity: OutputFidelity::Heuristic,
                timeout_secs: 180,
            },
            failure_signatures: &[FailureSignature {
                needles: &["UnknownError", "Unexpected server error"],
                cause: "opencode is configured for a model it has no credential for.",
                action: "Compare `opencode auth list` with the `model` line in ~/.config/opencode/opencode.jsonc.",
            }],
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
                schema_args: Vec::new(),
                prompt_delivery: PromptDelivery::Argv,
                output: OutputSource::Stdout,
                unwrap: None,
                fidelity: OutputFidelity::Heuristic,
                timeout_secs: 180,
            },
            failure_signatures: &[],
        },
    ]
}

pub fn preset_by_id(id: &str) -> Option<AgentCliSpec> {
    all_presets().into_iter().find(|p| p.id == id).map(|p| p.spec)
}

/// The measured failure modes for one preset id, or an empty slice for an id
/// that is not a shipped preset (a Custom spec, or a typo).
pub fn failure_signatures_for(preset_id: &str) -> &'static [FailureSignature] {
    match all_presets().into_iter().find(|p| p.id == preset_id) {
        Some(p) => p.failure_signatures,
        None => &[],
    }
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
    ///
    /// Updated 2026-08-29: the 2026-08-29 isolation flags are folded into
    /// this exact-vector pin rather than left to
    /// `claude_preset_isolates_itself_from_user_scope` alone — that test
    /// only checks flag-name presence via `.any()`, which is order-blind and
    /// value-blind (it would not catch `--setting-sources` losing its paired
    /// `""`, a flag stealing its neighbour's value, the tail reordering, or
    /// a duplicate flag). This test's exact `assert_eq!` is what still
    /// catches all of those.
    #[test]
    fn claude_preset_matches_the_historical_hardcoded_argv() {
        let spec = preset_by_id("claude").expect("claude preset exists");
        assert_eq!(spec.binary, "claude");
        assert_eq!(
            spec.args,
            a(&[
                "-p",
                "--output-format",
                "json",
                "--strict-mcp-config",
                "--setting-sources",
                "",
                "--tools",
                "",
            ])
        );
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

    /// The claude row must isolate itself from the user's ambient config.
    /// Without these three the CLI loads user settings, hooks, skills,
    /// plugins and every registered MCP server before it looks at the
    /// prompt — measured at ~3s of pre-API startup (spec §1.1).
    #[test]
    fn claude_preset_isolates_itself_from_user_scope() {
        let spec = preset_by_id("claude").expect("claude preset exists");
        let joined = spec.args.join(" ");
        for flag in ["--strict-mcp-config", "--setting-sources", "--tools"] {
            assert!(
                spec.args.iter().any(|a| a == flag),
                "claude row must send {flag}; got: {joined}"
            );
        }
    }

    /// `--bare` is rejected permanently: it requires ANTHROPIC_API_KEY and
    /// never reads OAuth or the keychain, which would take every Max/Pro
    /// user off the subscription they signed into. Spec §4.1.
    ///
    /// `schema_args` is a second argv source alongside `spec.args` (see
    /// `AgentCliSpec::schema_args`), so a future row putting `--bare` there
    /// would pass a guard that only scanned `args` — chain both iterators.
    #[test]
    fn no_preset_sends_bare_or_otherwise_changes_auth() {
        for p in all_presets() {
            assert!(
                !p.spec
                    .args
                    .iter()
                    .chain(p.spec.schema_args.iter())
                    .any(|a| a == "--bare"),
                "{} must not send --bare",
                p.id
            );
        }
    }

    /// codex's own help states `--ignore-user-config` still authenticates
    /// through CODEX_HOME, so unlike claude's --bare this is safe to send.
    /// `--ephemeral` stops a one-shot Jodd call persisting a session file.
    #[test]
    fn codex_preset_ignores_user_config_and_stays_ephemeral() {
        let spec = preset_by_id("codex").expect("codex preset exists");
        let joined = spec.args.join(" ");
        for flag in ["--ignore-user-config", "--ephemeral"] {
            assert!(
                spec.args.iter().any(|a| a == flag),
                "codex row must send {flag}; got: {joined}"
            );
        }
        // Already correct before this plan — pinned so a future edit that
        // drops them fails here rather than in a live run.
        for flag in ["--sandbox", "--skip-git-repo-check"] {
            assert!(
                spec.args.iter().any(|a| a == flag),
                "codex row must keep {flag}; got: {joined}"
            );
        }
    }

    /// Pins the 2026-08-30 measurement: `--json-schema` on the claude row
    /// breaks Extract on Jodd's real prompt (structured-output retries
    /// exhausted on sonnet; a safety-classifier trip on opus-5[1m]), while
    /// the same prompt succeeds without it. Re-adding it needs a live run
    /// against the REAL Extract prompt, not a one-line probe — the one-line
    /// probe is exactly what passed before v0.25.1 shipped the regression.
    #[test]
    fn the_claude_row_sends_no_output_schema() {
        let spec = preset_by_id("claude").expect("claude preset exists");
        assert!(
            spec.schema_args.is_empty(),
            "claude must send no schema args; got: {:?}",
            spec.schema_args
        );
    }

    /// codex is the row where a schema IS verified working, so the guard
    /// above must not be read as "schemas are unsupported everywhere".
    #[test]
    fn the_codex_row_still_sends_its_output_schema() {
        let spec = preset_by_id("codex").expect("codex preset exists");
        assert!(
            spec.schema_args.iter().any(|a| a == "--output-schema"),
            "codex must keep --output-schema; got: {:?}",
            spec.schema_args
        );
    }

    /// opencode's isolation flag is narrower than the others — it drops
    /// external plugins only — but it is free and safe.
    #[test]
    fn opencode_preset_runs_without_external_plugins() {
        let spec = preset_by_id("opencode").expect("opencode preset exists");
        assert!(
            spec.args.iter().any(|a| a == "--pure"),
            "opencode row must send --pure; got: {}",
            spec.args.join(" ")
        );
    }

    /// An unavailable-reason string is shown to the user verbatim, so it may
    /// not assert facts about their machine that Jodd has not checked.
    /// The gemini row used to say the CLI "is installed here"; it was not.
    #[test]
    fn unavailable_reasons_do_not_assert_local_installation() {
        for p in all_presets() {
            if let Availability::NotAvailable(reason) = p.availability {
                let lowered = reason.to_lowercase();
                for claim in ["installed here", "on this machine"] {
                    assert!(
                        !lowered.contains(claim),
                        "{} claims {claim:?} about the user's machine: {reason}",
                        p.id
                    );
                }
            }
        }
    }
}
