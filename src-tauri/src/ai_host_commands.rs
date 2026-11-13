//! AI host setup (spec 2026-10-13-remote-agent-cli §4.6; for MCP hosts spec
//! 2026-10-30-ai-host-subproject §8 and §9). The password is used once per
//! setup/update and never stored, logged or echoed.
//!
//! A host is one of two kinds, and every command finds out which by asking:
//! - **MCP**: the AI host kit's `jodd-ai-mcp` is the key's forced command.
//!   Status is its `tools/list`, Remove is its `forget_key`.
//! - **Legacy**: an older Jodd's `~/.jodd/ai-run-v2` wrapper. Status and
//!   Remove use the wrapper's verbs, and the status says how to upgrade.
//!
//! Setup installs MCP lines only (it needs the kit); Reinstall moves a legacy
//! line to MCP when the kit is there and reinstalls the wrapper when not.
//!
//! Without the password ("Run a command on the host", spec
//! 2026-11-12-ssh-key-authorization §5.2–5.3): Jodd makes the key and shows
//! the kit's `jodd-ai-host add-key '<line>'`, which writes the same line the
//! password path does; Finish then checks the key signs in and is MCP.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::accounts::AgentHost;
use crate::backend::ssh::{
    classify,
    russh_session::{self, Credential, RusshFailure},
    russh_setup, scripts, session, setup,
};
use crate::llm::remote::{
    self,
    channel::{self, Opened, RemoteMcp},
    exec::{RemoteExec, RusshExec},
    install,
    mcp::{self, HostInfo, KNOWN_TOOLS},
    wrapper,
};

/// Probe, upload and authorized_keys edit are a handful of round trips on one
/// connection; a server that accepts the connection and then goes quiet must
/// not leave the setup spinner running forever.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
/// The MCP status check: connect, handshake, `tools/list`.
const STATUS_DEADLINE: Duration = Duration::from_secs(30);
/// Remove is best effort on the server; it must not hold the UI.
const FORGET_DEADLINE: Duration = Duration::from_secs(15);

const KIT_URL: &str = "https://github.com/BBM-Co-ORG/jodd-ai-host#install";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AiHostPreset {
    pub id: String,
    pub path: Option<String>,
}

/// What the host is and what it offers. Serialised for the frontend with a
/// `protocol` tag: `"mcp"` or `"legacy"`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum AiHostStatus {
    Mcp {
        /// `serverInfo.version`, as the host sent it.
        version: String,
        /// `infer`'s `harness` enum as the host lists it: the enabled
        /// harnesses (empty when `infer` is not listed).
        harnesses: Vec<String>,
        notices: Vec<AiHostNotice>,
    },
    Legacy {
        version: u32,
        current: bool,
        presets: Vec<AiHostPreset>,
        notices: Vec<AiHostNotice>,
    },
}

/// Why something does not work on this host, and what to do (spec §8).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AiHostNotice {
    /// A harness id (`claude`, `codex`) when the notice is about one, else
    /// the tool's name (`infer`, `forget_key`), or `legacy`.
    pub feature: String,
    pub kind: NoticeKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    UpgradeHost,
    AskAdmin,
    UpdateJodd,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiHostSetupResult {
    pub host: AgentHost,
    pub status: AiHostStatus,
}

/// What the user runs on the host to authorize Jodd's key (kit 1.1.0's
/// `add-key`): on the host itself, or in the kit's docker compose directory.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AiHostCommands {
    pub sudo: String,
    pub docker: String,
}

/// "Run a command on the host", step one: the key exists locally, nothing
/// has been sent anywhere.
#[derive(Debug, Clone, Serialize)]
pub struct AiHostPrepared {
    pub host: AgentHost,
    pub commands: AiHostCommands,
}

/// The harnesses Jodd has a preset for, with the names the user knows.
const HARNESSES: [(&str, &str); 2] = [("claude", "Claude Code"), ("codex", "Codex")];

/// What a tool lets the user do, in words for "Upgrade the AI host to use …".
fn feature_name(tool: &str) -> &str {
    match tool {
        "infer" => "AI features",
        "forget_key" => "Remove AI host",
        other => other,
    }
}

/// `x.y.z` as numbers; anything else is `0.0.0` (older than every release).
fn parse_version(v: &str) -> (u64, u64, u64) {
    let parts: Vec<Option<u64>> = v.split('.').map(|p| p.parse().ok()).collect();
    match parts[..] {
        [Some(a), Some(b), Some(c)] => (a, b, c),
        _ => (0, 0, 0),
    }
}

fn notice(feature: &str, kind: NoticeKind, text: String) -> AiHostNotice {
    AiHostNotice { feature: feature.to_string(), kind, text }
}

fn upgrade_host(feature: &str, name: &str) -> AiHostNotice {
    notice(feature, NoticeKind::UpgradeHost, format!(
        "Upgrade the AI host to use {name}: on the host, re-run the installer, or run \
         `docker compose pull && docker compose up -d`."
    ))
}

/// What the host's own answers say Jodd cannot use, and why (spec §8): a
/// tool the host is too old for, a harness or tool its administrator turned
/// off, a tool newer than this Jodd. Pure.
pub(crate) fn notices_for(info: &HostInfo) -> Vec<AiHostNotice> {
    let host = parse_version(&info.version);
    let listed = |name: &str| info.tools.iter().find(|t| t.name == name);
    let mut out = Vec::new();
    // `infer` missing on a host too old for it: each harness row says upgrade.
    let mut infer_too_old = false;
    for &(tool, known, introduced) in KNOWN_TOOLS {
        let too_old = host < parse_version(introduced);
        match listed(tool) {
            Some(t) if t.tool_version > known => out.push(notice(tool, NoticeKind::UpdateJodd, format!(
                "Update Jodd to use {} on this host.", feature_name(tool)
            ))),
            Some(_) => {}
            None if tool == "infer" => infer_too_old = too_old,
            None if too_old => out.push(upgrade_host(tool, feature_name(tool))),
            // Listed by every host new enough to know it (PROTOCOL.md): the
            // host is broken, and its own check says how.
            None => out.push(notice(tool, NoticeKind::AskAdmin, format!(
                "{} is not available on this host. Ask its administrator to run `jodd-ai-host doctor`.",
                feature_name(tool)
            ))),
        }
    }
    let enabled = listed("infer").map(|t| t.harnesses.as_slice()).unwrap_or_default();
    for (id, name) in HARNESSES {
        if infer_too_old {
            out.push(upgrade_host(id, name));
        } else if !enabled.iter().any(|h| h == id) {
            out.push(notice(id, NoticeKind::AskAdmin, format!(
                "{name} is turned off on this host. Ask its administrator to run `sudo jodd-ai-host enable {id}`."
            )));
        }
    }
    out
}

/// A legacy host's banner (spec §8 row 1).
pub(crate) fn legacy_notice() -> AiHostNotice {
    notice("legacy", NoticeKind::UpgradeHost, format!(
        "This AI host uses the old helper. Upgrade it: install the AI host kit on the host ({KIT_URL}), \
         then run `sudo jodd-ai-host adopt-keys`. AI features keep working meanwhile."
    ))
}

fn mcp_status(info: HostInfo) -> AiHostStatus {
    let notices = notices_for(&info);
    let harnesses = info.tools.into_iter().find(|t| t.name == "infer").map(|t| t.harnesses).unwrap_or_default();
    AiHostStatus::Mcp { version: info.version, harnesses, notices }
}

/// The legacy wrapper's `status` output.
pub(crate) fn parse_status(stdout: &str) -> AiHostStatus {
    let version = stdout
        .lines()
        .find_map(|l| l.strip_prefix("version="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let presets = stdout
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("preset=")?;
            let (id, path) = rest.split_once(" path=")?;
            Some(AiHostPreset {
                id: id.to_string(),
                path: Some(path.trim().to_string()).filter(|p| p.starts_with('/')),
            })
        })
        .collect();
    AiHostStatus::Legacy {
        version,
        current: version == wrapper::WRAPPER_VERSION,
        presets,
        notices: vec![legacy_notice()],
    }
}

/// Over the restricted key, on a legacy host: the wrapper's own `status` verb
/// (also what a Jodd from before MCP asks; the contract tests check that an
/// MCP host still answers it).
pub(crate) async fn legacy_status(exec: &dyn RemoteExec) -> Result<AiHostStatus, String> {
    let out = exec
        .exec(
            &format!("jodd-ai-run {} status", wrapper::WRAPPER_VERSION),
            b"",
            Duration::from_secs(30),
            CancellationToken::new(),
        )
        .await
        .map_err(|e| e.to_string())?;
    match out.exit {
        Some(0) => Ok(parse_status(&out.stdout)),
        other => Err(format!(
            "The AI host did not answer the status check (exit {other:?}). {}",
            out.stderr.trim()
        )),
    }
}

/// Over the restricted key: the host's MCP `tools/list`, or `None` when it
/// has only the legacy wrapper. Bounded by [`STATUS_DEADLINE`].
async fn mcp_info(mcp: &dyn RemoteMcp) -> Result<Option<HostInfo>, String> {
    let cancel = CancellationToken::new();
    let work = async {
        let ch = mcp.open_mcp(cancel.clone()).await.map_err(|e| e.to_string())?;
        match channel::connect(ch).await.map_err(|e| e.to_string())? {
            Opened::Legacy => Ok(None),
            Opened::Mcp(client, version) => {
                let info = client.host_info(version).await.map_err(|e| e.to_string());
                client.close().await;
                info.map(Some)
            }
        }
    };
    let got = tokio::time::timeout(STATUS_DEADLINE, work).await;
    if got.is_err() {
        cancel.cancel();
    }
    got.unwrap_or_else(|_| {
        Err(format!("The AI host did not answer the status check within {}s.", STATUS_DEADLINE.as_secs()))
    })
}

/// Over the restricted key: MCP first, the legacy wrapper's status when the
/// host has only that.
pub(crate) async fn status_core(mcp: &dyn RemoteMcp, exec: &dyn RemoteExec) -> Result<AiHostStatus, String> {
    match mcp_info(mcp).await? {
        Some(info) => Ok(mcp_status(info)),
        None => legacy_status(exec).await,
    }
}

/// User-facing text for a connect failure. Never includes the password.
fn setup_message(f: &RusshFailure) -> String {
    match f {
        RusshFailure::HostKeyMismatch => {
            "The server's identity does not match the fingerprint you confirmed. Nothing was changed.".into()
        }
        RusshFailure::AuthRejected { password_offered: true, .. } => {
            "The server rejected that username or password.".into()
        }
        RusshFailure::AuthRejected { password_offered: false, .. } => {
            // Setup and Reinstall both reach this; each offers the command method.
            "This host doesn't accept password sign-in. Choose Run a command on the host \
             (needs AI host kit 1.1.0 or later), or run sudo jodd-ai-host unlock on the host, use the password, then run sudo jodd-ai-host lock."
                .into()
        }
        RusshFailure::Other(m) => format!("Could not connect: {m}"),
        other => classify::from_setup_stderr(&russh_session::stderr_for(other)),
    }
}

/// Why an install over the password session failed, and whether the server may
/// have been changed before it did: `touched` is set the moment the install
/// script is sent, so a timeout or a dropped connection after that point still
/// counts.
struct InstallFailure {
    msg: String,
    touched: bool,
}

impl InstallFailure {
    fn untouched(msg: String) -> Self {
        Self { msg, touched: false }
    }
}

/// What a host without the kit gets from Setup.
fn kit_missing() -> String {
    format!("This host needs the AI host kit — install it from {KIT_URL}, then set up again.")
}

/// Is the kit's `bin` there? Prints `absent`, or the binary's `--version`.
fn kit_probe(bin: &str) -> String {
    let b = session::sh_quote(bin);
    format!("[ -x {b} ] || {{ echo absent; exit 0; }}\n{b} --version\n")
}

/// When the kit is absent: fail (Setup), or reinstall the legacy wrapper
/// (Reinstall on a host that has not upgraded yet).
#[derive(Clone, Copy, PartialEq, Eq)]
enum WithoutKit {
    Refuse,
    Legacy,
}

/// Over the PASSWORD session: install this key's line. With the kit at `bin`
/// it forces `bin --key <id>`; without it, `without` decides.
#[allow(clippy::too_many_arguments)]
async fn install_over_password(
    target: &str,
    port: Option<u16>,
    known_hosts: &Path,
    password: &str,
    key_id: &str,
    pubkey_line: &str,
    bin: &str,
    without: WithoutKit,
) -> Result<(), InstallFailure> {
    // Before anything is sent: a path the forced command cannot hold must
    // not reach the server, nor count as a line left behind.
    if !install::valid_mcp_bin(bin) {
        return Err(InstallFailure::untouched(format!("internal: the AI host kit path {bin:?} cannot be used")));
    }
    // `connect` pins against the file's CONTENTS, not its path.
    let pinned = std::fs::read_to_string(known_hosts)
        .map_err(|e| InstallFailure::untouched(format!("could not read known_hosts: {e}")))?;
    let touched = AtomicBool::new(false);
    let work = async {
        let h = russh_session::connect(target, port, pinned, Credential::Password(password))
            .await
            .map_err(|f| setup_message(&f))?;
        let run = |script: String| {
            let h = &h;
            async move { russh_session::run_on(h, &script).await.map_err(|e| format!("Could not talk to the server: {e}")) }
        };
        let result = async {
            let probe = run(kit_probe(bin)).await?;
            let script = if probe.exit == 0 && probe.stdout.trim() == "absent" {
                if without == WithoutKit::Refuse {
                    return Err(kit_missing());
                }
                let flavor = scripts::parse_probe(&run(scripts::PROBE.to_string()).await?.stdout)?;
                let paths = install::parse_probe_paths(&run(install::PROBE_PATHS.to_string()).await?.stdout);
                install::install_script(flavor, &wrapper::generate(&paths)?, key_id, pubkey_line)
            } else if probe.exit != 0 {
                return Err(format!("The AI host kit on this server did not run: {}", probe.stderr.trim()));
            } else {
                crate::log!("ai_host_install: kit {}", probe.stdout.trim());
                install::mcp_line_script(bin, key_id, pubkey_line)
            };
            touched.store(true, Ordering::SeqCst);
            let out = run(script).await?;
            if out.exit != 0 || out.stdout.trim() != "installed" {
                return Err(format!("Installing on the server failed: {}", out.stderr.trim()));
            }
            Ok(())
        }
        .await;
        let _ = h.disconnect(russh::Disconnect::ByApplication, "", "").await;
        result
    };
    let result = tokio::time::timeout(INSTALL_TIMEOUT, work)
        .await
        .unwrap_or_else(|_| Err(russh_setup::SERVER_STOPPED_ANSWERING.to_string()));
    result.map_err(|msg| InstallFailure { msg, touched: touched.load(Ordering::SeqCst) })
}

/// What a failed setup tells the user it may have left on the server. The
/// local key is already gone, so the line cannot be used — but it is there.
fn left_behind_note(key_id: &str, label: &str) -> String {
    format!(
        "Jodd may have left an entry ending in jodd-ai-{key_id} in ~/.ssh/authorized_keys on {label}; \
         its key no longer exists on this computer, so you can remove that line."
    )
}

/// Set up an AI host with the kit at `bin` (production: [`mcp::MCP_BIN`]).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn setup_core(
    host: &str,
    port: Option<u16>,
    user: &str,
    password: &str,
    known_hosts_line: &str,
    base: &Path,
    key_dir: &str,
    bin: &str,
) -> Result<AiHostSetupResult, String> {
    setup_with_verifier(host, port, user, password, known_hosts_line, base, key_dir, bin, |e| Box::new(e)).await
}

/// `setup_core` with the final key-session check replaceable, so the
/// "installed, but the key does not work" path can be driven in a test.
#[allow(clippy::too_many_arguments)]
async fn setup_with_verifier(
    host: &str,
    port: Option<u16>,
    user: &str,
    password: &str,
    known_hosts_line: &str,
    base: &Path,
    key_dir: &str,
    bin: &str,
    verifier: impl FnOnce(RusshExec) -> Box<dyn RemoteMcp>,
) -> Result<AiHostSetupResult, String> {
    session::validate_target(host)?;
    session::validate_target(user)?;
    let target = format!("{user}@{host}");
    let key_id = remote::key_id_for(key_dir);
    if key_id.len() != 8 {
        return Err("internal: bad key id".into());
    }
    let agent_host = AgentHost { target: target.clone(), port, key_dir: key_dir.to_string() };
    let label = remote::host_label(&agent_host);
    let fail = |e: String| {
        let _ = std::fs::remove_dir_all(base);
        e
    };
    let made = russh_setup::generate_key(base)?;
    setup::write_known_hosts(base, known_hosts_line).map_err(fail)?;
    // Our own comment, so `forget_key` and re-install can find exactly this line.
    let pubkey_line = format!("{} jodd-ai-{key_id}", scripts::key_body(&made.pubkey_line));
    install_over_password(&target, port, &base.join("known_hosts"), password, &key_id, &pubkey_line, bin, WithoutKit::Refuse)
        .await
        .map_err(|f| if f.touched { fail(format!("{} {}", f.msg, left_behind_note(&key_id, &label))) } else { fail(f.msg) })?;
    let exec = RusshExec {
        target,
        port,
        key: base.join("id_ed25519"),
        known_hosts: base.join("known_hosts"),
        label: label.clone(),
    };
    // Reconnect WITH THE KEY: proves the restricted line is live and is MCP.
    let info = match mcp_info(verifier(exec).as_ref()).await {
        Ok(Some(info)) => info,
        Ok(None) => return Err(fail(format!(
            "The AI host answered the old way after the kit's line was installed. {}",
            left_behind_note(&key_id, &label)
        ))),
        Err(e) => return Err(fail(format!("{e} {}", left_behind_note(&key_id, &label)))),
    };
    Ok(AiHostSetupResult { host: agent_host, status: mcp_status(info) })
}

/// Reinstall this key's line with the password: on the kit at `bin` when the
/// host has it (which moves a legacy host's line to MCP), else the legacy
/// wrapper. The working key stays on disk whether or not this succeeds.
pub(crate) async fn update_core(h: &AgentHost, base: &Path, password: &str, bin: &str) -> Result<AiHostStatus, String> {
    let key_id = remote::key_id_for(&h.key_dir);
    let pubkey_line = existing_key_line(h, base)?;
    install_over_password(&h.target, h.port, &base.join("known_hosts"), password, &key_id, &pubkey_line, bin, WithoutKit::Legacy)
        .await
        .map_err(|f| f.msg)?;
    let exec = RusshExec {
        target: h.target.clone(),
        port: h.port,
        key: base.join("id_ed25519"),
        known_hosts: base.join("known_hosts"),
        label: remote::host_label(h),
    };
    status_core(&exec, &exec).await
}

/// The working key's line, `<type> <body> jodd-ai-<id>`: what Reinstall
/// installs, with the password or with a command on the host.
fn existing_key_line(h: &AgentHost, base: &Path) -> Result<String, String> {
    let key_id = remote::key_id_for(&h.key_dir);
    let pub_text = std::fs::read_to_string(base.join("id_ed25519.pub"))
        .map_err(|e| format!("Jodd's key is missing: {e}"))?;
    Ok(format!("{} jodd-ai-{key_id}", scripts::key_body(&pub_text)))
}

/// The commands that authorize `pubkey_line` (`<type> <body> jodd-ai-<id>`)
/// with the kit's `add-key`, which forces `jodd-ai-mcp --key <id>` on it —
/// the same line the password path writes. Refuses a line that single
/// quotes cannot carry as one pasteable argument.
pub(crate) fn add_key_commands(pubkey_line: &str) -> Result<AiHostCommands, String> {
    if pubkey_line.contains('\'') || pubkey_line.chars().any(char::is_control) {
        return Err("internal: Jodd's key line cannot be put in a command".into());
    }
    let line = session::sh_quote(pubkey_line);
    Ok(AiHostCommands {
        sudo: format!("sudo jodd-ai-host add-key {line}"),
        docker: format!("docker compose exec ai-host jodd-ai-host add-key {line}"),
    })
}

/// "Run a command on the host", step one: make the key and `known_hosts`
/// in `base` and the commands that authorize it. Nothing is sent; on
/// failure `base` is removed.
pub(crate) async fn prepare_command_core(
    host: &str,
    port: Option<u16>,
    user: &str,
    known_hosts_line: &str,
    base: &Path,
    key_dir: &str,
) -> Result<AiHostPrepared, String> {
    session::validate_target(host)?;
    session::validate_target(user)?;
    let key_id = remote::key_id_for(key_dir);
    if key_id.len() != 8 {
        return Err("internal: bad key id".into());
    }
    let agent_host = AgentHost { target: format!("{user}@{host}"), port, key_dir: key_dir.to_string() };
    let made = (|| {
        let made = russh_setup::generate_key(base)?;
        setup::write_known_hosts(base, known_hosts_line)?;
        add_key_commands(&format!("{} jodd-ai-{key_id}", scripts::key_body(&made.pubkey_line)))
    })();
    match made {
        Ok(commands) => Ok(AiHostPrepared { host: agent_host, commands }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(base);
            Err(e)
        }
    }
}

/// "Run a command on the host", step two: does the host take the key yet,
/// and is it the kit? Never removes `base`: the user can run the command
/// and try again, or Cancel.
pub(crate) async fn finish_command_core(
    h: &AgentHost,
    base: &Path,
    verifier: impl FnOnce(RusshExec) -> Box<dyn RemoteMcp>,
) -> Result<AiHostSetupResult, String> {
    if !base.is_dir() {
        return Err(crate::backend::ssh::setup::SETUP_EXPIRED.into());
    }
    let pinned = std::fs::read_to_string(base.join("known_hosts"))
        .map_err(|e| format!("could not read known_hosts: {e}"))?;
    let key = base.join("id_ed25519");
    // One sign-in with the key alone, so "not added yet" is told apart from
    // everything the MCP check could fail on.
    let probe = tokio::time::timeout(STATUS_DEADLINE, russh_session::connect(&h.target, h.port, pinned, Credential::Key(&key)))
        .await
        .map_err(|_| russh_setup::SERVER_STOPPED_ANSWERING.to_string())?;
    match probe {
        Ok(session) => {
            let _ = session.disconnect(russh::Disconnect::ByApplication, "", "").await;
        }
        Err(RusshFailure::AuthRejected { .. }) => {
            return Err("The host doesn't accept Jodd's key yet. Run the command on the host, then try again.".into())
        }
        Err(f) => return Err(setup_message(&f)),
    }
    let exec = RusshExec {
        target: h.target.clone(),
        port: h.port,
        key,
        known_hosts: base.join("known_hosts"),
        label: remote::host_label(h),
    };
    match mcp_info(verifier(exec).as_ref()).await? {
        Some(info) => Ok(AiHostSetupResult { host: h.clone(), status: mcp_status(info) }),
        None => Err("This host doesn't have the AI host kit, which \"Run a command\" needs. \
                     Install the kit, or set up with the password."
            .into()),
    }
}

/// Reinstall by a command on the host: the working key's line, as
/// [`update_core`] installs it with the password.
pub(crate) fn reinstall_commands_core(h: &AgentHost, base: &Path) -> Result<AiHostCommands, String> {
    add_key_commands(&existing_key_line(h, base)?)
}

/// Best effort on the server (it may be gone), certain locally: `forget_key`
/// on an MCP host, the wrapper's `forget` on a legacy one.
pub(crate) async fn forget_core(h: &AgentHost, base: &Path, mcp: &dyn RemoteMcp, exec: &dyn RemoteExec) {
    let label = remote::host_label(h);
    let cancel = CancellationToken::new();
    let work = async {
        let ch = mcp.open_mcp(cancel.clone()).await.map_err(|e| e.to_string())?;
        match channel::connect(ch).await.map_err(|e| e.to_string())? {
            Opened::Mcp(client, _) => {
                let r = client.forget_key().await.map(|removed| format!("forget_key removed={removed}"));
                client.close().await;
                r.map_err(|e| e.to_string())
            }
            Opened::Legacy => exec
                .exec(
                    &format!("jodd-ai-run {} forget", wrapper::WRAPPER_VERSION),
                    b"",
                    FORGET_DEADLINE,
                    cancel.clone(),
                )
                .await
                .map(|o| format!("legacy forget exit {:?}", o.exit))
                .map_err(|e| e.to_string()),
        }
    };
    let r = tokio::time::timeout(FORGET_DEADLINE, work).await;
    if r.is_err() {
        cancel.cancel();
    }
    crate::log!("ai_host_forget {label}: remote {r:?}");
    let _ = std::fs::remove_dir_all(base);
}

/// The only way a command turns an IPC `AgentHost` into a directory: through
/// `host_dir`, which refuses a `key_dir` that is not a uuid.
fn base_for(h: &AgentHost) -> Result<PathBuf, String> {
    remote::host_dir(h).ok_or_else(|| "this AI host has no usable key directory".to_string())
}

/// Keep the runner's legacy memo in step with what a command just saw, so a
/// host moved to MCP is tried on MCP at once (and a legacy one is not asked
/// again for ten minutes).
fn note_protocol(h: &AgentHost, status: &Result<AiHostStatus, String>) {
    match status {
        Ok(AiHostStatus::Mcp { .. }) => crate::llm::agent_cli::forget_legacy(&h.key_dir),
        Ok(AiHostStatus::Legacy { .. }) => crate::llm::agent_cli::remember_legacy(&h.key_dir),
        Err(_) => {}
    }
}

#[tauri::command]
pub(crate) async fn ai_host_setup(
    host: String,
    port: Option<u16>,
    user: String,
    password: String,
    known_hosts_line: String,
) -> Result<AiHostSetupResult, String> {
    let key_dir = uuid::Uuid::new_v4().to_string();
    let probe = AgentHost { target: format!("{user}@{host}"), port, key_dir: key_dir.clone() };
    let base = base_for(&probe).map_err(|_| "no app data directory available".to_string())?;
    setup_core(&host, port, &user, &password, &known_hosts_line, &base, &key_dir, mcp::MCP_BIN).await
}

#[tauri::command]
pub(crate) async fn ai_host_status(host: AgentHost) -> Result<AiHostStatus, String> {
    let exec = RusshExec::for_host(&host).map_err(|e| e.to_string())?;
    let status = status_core(&exec, &exec).await;
    note_protocol(&host, &status);
    status
}

#[tauri::command]
pub(crate) async fn ai_host_update(host: AgentHost, password: String) -> Result<AiHostStatus, String> {
    let status = update_core(&host, &base_for(&host)?, &password, mcp::MCP_BIN).await;
    note_protocol(&host, &status);
    status
}

#[tauri::command]
pub(crate) async fn ai_host_forget(host: AgentHost) -> Result<(), String> {
    let base = base_for(&host)?;
    let exec = RusshExec::for_host(&host).map_err(|e| e.to_string())?;
    forget_core(&host, &base, &exec, &exec).await;
    crate::llm::agent_cli::forget_legacy(&host.key_dir);
    Ok(())
}

#[tauri::command]
pub(crate) async fn ai_host_prepare_command(
    host: String,
    port: Option<u16>,
    user: String,
    known_hosts_line: String,
) -> Result<AiHostPrepared, String> {
    let key_dir = uuid::Uuid::new_v4().to_string();
    let probe = AgentHost { target: format!("{user}@{host}"), port, key_dir: key_dir.clone() };
    let base = base_for(&probe).map_err(|_| "no app data directory available".to_string())?;
    prepare_command_core(&host, port, &user, &known_hosts_line, &base, &key_dir).await
}

#[tauri::command]
pub(crate) async fn ai_host_finish_command(host: AgentHost) -> Result<AiHostSetupResult, String> {
    let r = finish_command_core(&host, &base_for(&host)?, |e| Box::new(e)).await;
    note_protocol(&host, &r.as_ref().map(|r| r.status.clone()).map_err(String::clone));
    r
}

/// Cancel is Remove: best effort on the host (which has usually never seen
/// the key), certain locally.
#[tauri::command]
pub(crate) async fn ai_host_cancel_command(host: AgentHost) -> Result<(), String> {
    ai_host_forget(host).await
}

#[tauri::command]
pub(crate) async fn ai_host_reinstall_commands(host: AgentHost) -> Result<AiHostCommands, String> {
    reinstall_commands_core(&host, &base_for(&host)?)
}

/// These run a real ssh server on loopback (`test_server`): Unix only. Its
/// exec handler serves the in-test MCP host (`fake_host`) for the kit's
/// forced command and runs everything else — the password session's `sh`,
/// the legacy wrapper — with the real `sh`, as sshd would.
#[cfg(all(test, unix))]
mod live_tests {
    use std::sync::{Arc, Mutex, OnceLock};

    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::backend::ssh::test_server::{start, BoxFuture, ExecCtx, ExecHandler, ServerOpts, TestServer};
    use crate::llm::remote::fake_host::{FakeHost, FakeTool, Recorder};
    use crate::llm::remote::mcp::MCP_REQUEST;
    use crate::llm::remote::wrapper::{generate, wrapper_file_name, CliPaths, WRAPPER_VERSION};

    const KEY_DIR: &str = "0123abcd-0000-4000-8000-000000000000";
    const KIT_URL: &str = "https://github.com/BBM-Co-ORG/jodd-ai-host#install";

    /// A test server, and where its fake `jodd-ai-mcp` lives.
    struct Kit {
        srv: TestServer,
        _bin_dir: tempfile::TempDir,
        /// What setup probes and puts in the forced command.
        bin: String,
        /// Every MCP session the host served, in order.
        hosts: Arc<Mutex<Vec<Recorder>>>,
    }

    impl Kit {
        fn keys_path(&self) -> std::path::PathBuf {
            self.srv.home.path().join(".ssh/authorized_keys")
        }
        fn keys(&self) -> String {
            std::fs::read_to_string(self.keys_path()).unwrap_or_default()
        }
        fn last_host(&self) -> Recorder {
            self.hosts.lock().unwrap().last().cloned().expect("no MCP session was served")
        }
        async fn setup(&self, dir: &std::path::Path, password: &str) -> Result<AiHostSetupResult, String> {
            setup_core("127.0.0.1", Some(self.srv.port), "jodd", password, &self.srv.host_key_line, dir, KEY_DIR, &self.bin)
                .await
        }
    }

    /// `sh -c command` in `home`, streamed both ways, as sshd runs it.
    fn sh(ctx: ExecCtx, home: std::path::PathBuf) -> BoxFuture<'static, u32> {
        Box::pin(async move {
            let ExecCtx { command, original, mut stdin, mut stdout, mut stderr } = ctx;
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(&command).current_dir(&home).env("HOME", &home);
            if let Some(o) = &original {
                c.env("SSH_ORIGINAL_COMMAND", o);
            }
            let piped = std::process::Stdio::piped;
            let mut child = c.stdin(piped()).stdout(piped()).stderr(piped()).kill_on_drop(true).spawn().unwrap();
            let (mut cin, mut cout, mut cerr) =
                (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
            // The legacy protocol never sends EOF, so stdin is not waited on.
            let feed = tokio::spawn(async move {
                let _ = tokio::io::copy(&mut stdin, &mut cin).await;
            });
            let _ = tokio::join!(tokio::io::copy(&mut cout, &mut stdout), tokio::io::copy(&mut cerr, &mut stderr));
            let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(255);
            feed.abort();
            code as u32
        })
    }

    /// A password server. `kit`: whether its `jodd-ai-mcp` is installed.
    /// The kit's forced command asked for `jodd-ai-mcp` gets a fresh
    /// `make()` host; asked for anything else it exits 64 like the real one
    /// would for an unknown request.
    async fn kit_server(kit: bool, make: impl Fn() -> FakeHost + Send + Sync + 'static) -> Kit {
        let bin_dir = tempfile::tempdir().unwrap();
        let bin = bin_dir.path().join("jodd-ai-mcp").to_string_lossy().into_owned();
        if kit {
            std::fs::write(&bin, "#!/bin/sh\necho \"jodd-ai-mcp 1.0.0\"\n").unwrap();
            std::process::Command::new("chmod").arg("755").arg(&bin).status().unwrap();
        }
        let hosts: Arc<Mutex<Vec<Recorder>>> = Default::default();
        let home: Arc<OnceLock<std::path::PathBuf>> = Default::default();
        let handler: ExecHandler = {
            let (hosts, home, forced) = (hosts.clone(), home.clone(), format!("{bin} --key "));
            Arc::new(move |mut ctx: ExecCtx| {
                if !ctx.command.starts_with(&forced) {
                    return sh(ctx, home.get().expect("home is set before any login").clone());
                }
                if ctx.original.as_deref() != Some(MCP_REQUEST) {
                    return Box::pin(async move {
                        let _ = ctx.stderr.write_all(b"unknown request\n").await;
                        64
                    });
                }
                hosts.lock().unwrap().push(make().serve(ctx.stdin, ctx.stdout));
                Box::pin(async { 0 })
            })
        };
        let srv = start(ServerOpts { password: Some("hunter2"), exec_handler: Some(handler), ..Default::default() }).await;
        home.set(srv.home.path().to_path_buf()).unwrap();
        Kit { srv, _bin_dir: bin_dir, bin, hosts }
    }

    /// This key on the legacy wrapper, exactly as an older Jodd's setup left
    /// it: the key, `known_hosts`, `~/.jodd/ai-run-v2` and its line.
    fn legacy_line(kit: &Kit, dir: &std::path::Path) -> AgentHost {
        let made = crate::backend::ssh::russh_setup::generate_key(dir).unwrap();
        std::fs::write(dir.join("known_hosts"), format!("{}\n", kit.srv.host_key_line)).unwrap();
        authorize_on_wrapper(kit, &made.pubkey_line);
        AgentHost { target: kit.srv.target(), port: Some(kit.srv.port), key_dir: KEY_DIR.into() }
    }

    /// `~/.jodd/ai-run-v2` on the host, and `pubkey_line`'s key forced onto
    /// it, as an older Jodd's setup wrote it.
    fn authorize_on_wrapper(kit: &Kit, pubkey_line: &str) {
        let home = kit.srv.home.path();
        let text = generate(&CliPaths { path_env: "/usr/bin:/bin".into(), claude: None, codex: None }).unwrap();
        std::fs::create_dir_all(home.join(".jodd")).unwrap();
        let w = home.join(".jodd").join(wrapper_file_name());
        std::fs::write(&w, text).unwrap();
        std::process::Command::new("chmod").arg("700").arg(&w).status().unwrap();
        let body = crate::backend::ssh::scripts::key_body(pubkey_line);
        kit.srv.authorize(&format!("restrict,command=\"{} --key 0123abcd\" {body} jodd-ai-0123abcd", w.display()));
    }

    fn exec_for(h: &AgentHost, dir: &std::path::Path) -> RusshExec {
        RusshExec { target: h.target.clone(), port: h.port, key: dir.join("id_ed25519"),
            known_hosts: dir.join("known_hosts"), label: "t".into() }
    }

    fn mcp_status(harnesses: &[&str], notices: Vec<AiHostNotice>) -> AiHostStatus {
        AiHostStatus::Mcp {
            version: "1.0.0".into(),
            harnesses: harnesses.iter().map(|h| h.to_string()).collect(),
            notices,
        }
    }

    #[tokio::test]
    async fn setup_installs_the_mcp_line_and_reports_status() {
        let kit = kit_server(true, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let r = kit.setup(&dir, "hunter2").await.unwrap();
        assert_eq!(r.host.target, "jodd@127.0.0.1");
        assert_eq!(r.status, mcp_status(&["claude", "codex"], vec![]));
        let body = crate::backend::ssh::scripts::key_body(&std::fs::read_to_string(dir.join("id_ed25519.pub")).unwrap()).to_string();
        assert_eq!(kit.keys(), format!("restrict,command=\"{} --key 0123abcd\" {body} jodd-ai-0123abcd\n", kit.bin));
        assert!(!kit.srv.home.path().join(".jodd").exists(), "no ~/.jodd/ai-run-v* on an MCP host");
        assert!(dir.join("id_ed25519").exists() && dir.join("known_hosts").exists());
        assert!(kit.last_host().calls().is_empty(), "status is tools/list only: no tool was run");
    }

    #[tokio::test]
    async fn setup_without_the_kit_touches_nothing() {
        let kit = kit_server(false, || FakeHost::new("1.0.0")).await;
        kit.srv.authorize("ssh-ed25519 AAAAother me@laptop");
        let before = std::fs::read(kit.keys_path()).unwrap();
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let e = kit.setup(&dir, "hunter2").await.unwrap_err();
        assert_eq!(e, format!("This host needs the AI host kit — install it from {KIT_URL}, then set up again."));
        assert_eq!(std::fs::read(kit.keys_path()).unwrap(), before, "authorized_keys byte for byte");
        assert!(!dir.exists(), "the local key dir is removed");
        assert!(!kit.srv.home.path().join(".jodd").exists());
    }

    #[tokio::test]
    async fn a_turned_off_harness_gets_an_ask_admin_notice() {
        let kit = kit_server(true, || {
            FakeHost::new("1.0.0").tools(vec![FakeTool::infer(&["claude"]), FakeTool::forget_key()])
        })
        .await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let r = kit.setup(&dir, "hunter2").await.unwrap();
        let exec = exec_for(&r.host, &dir);
        let s = status_core(&exec, &exec).await.unwrap();
        let codex_off = AiHostNotice {
            feature: "codex".into(),
            kind: NoticeKind::AskAdmin,
            text: "Codex is turned off on this host. Ask its administrator to run `sudo jodd-ai-host enable codex`.".into(),
        };
        assert_eq!(s, mcp_status(&["claude"], vec![codex_off]));
        assert_eq!(r.status, s, "setup reports the same status");
    }

    #[tokio::test]
    async fn status_of_a_legacy_host_says_upgrade() {
        let (_srv, _d, exec) = crate::llm::remote::exec::live_tests::host().await;
        match status_core(&exec, &exec).await.unwrap() {
            AiHostStatus::Legacy { version, current, presets, notices } => {
                assert_eq!(version, WRAPPER_VERSION);
                assert!(current);
                assert_eq!(presets.len(), 2);
                assert_eq!(notices, vec![legacy_notice()]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn reinstall_moves_a_legacy_line_to_mcp_with_the_same_key() {
        let kit = kit_server(true, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let h = legacy_line(&kit, &dir);
        let exec = exec_for(&h, &dir);
        assert!(matches!(status_core(&exec, &exec).await.unwrap(), AiHostStatus::Legacy { .. }));
        let key_before = std::fs::read(dir.join("id_ed25519")).unwrap();
        let body = crate::backend::ssh::scripts::key_body(&std::fs::read_to_string(dir.join("id_ed25519.pub")).unwrap()).to_string();

        let s = update_core(&h, &dir, "hunter2", &kit.bin).await.unwrap();
        assert_eq!(s, mcp_status(&["claude", "codex"], vec![]));
        assert_eq!(kit.keys(), format!("restrict,command=\"{} --key 0123abcd\" {body} jodd-ai-0123abcd\n", kit.bin));
        assert_eq!(std::fs::read(dir.join("id_ed25519")).unwrap(), key_before, "the same key");
        assert!(!kit.srv.home.path().join(".jodd").join(wrapper_file_name()).exists(), "no line uses the wrapper now");
        assert!(matches!(status_core(&exec, &exec).await.unwrap(), AiHostStatus::Mcp { .. }));
    }

    #[tokio::test]
    async fn reinstall_without_the_kit_reinstalls_the_legacy_helper() {
        let kit = kit_server(false, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let h = legacy_line(&kit, &dir);
        let before = kit.keys();
        match update_core(&h, &dir, "hunter2", &kit.bin).await.unwrap() {
            AiHostStatus::Legacy { current, notices, .. } => {
                assert!(current);
                assert_eq!(notices, vec![legacy_notice()]);
            }
            other => panic!("{other:?}"),
        }
        let after = kit.keys();
        assert_eq!(after.matches("jodd-ai-0123abcd").count(), 1, "{after}");
        assert_eq!(before.lines().count(), after.lines().count());
        assert!(after.contains(&format!("/.jodd/{} --key 0123abcd", wrapper_file_name())), "{after}");
        let e = update_core(&h, &dir, "wrong", &kit.bin).await.unwrap_err();
        assert!(!e.contains("wrong"), "{e}");
        assert!(dir.join("id_ed25519").exists(), "a failed update must not delete the working key");
    }

    #[tokio::test]
    async fn forget_uses_forget_key_then_falls_back_to_legacy() {
        // An MCP host: `forget_key`, over the key.
        let kit = kit_server(true, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let r = kit.setup(&dir, "hunter2").await.unwrap();
        let exec = exec_for(&r.host, &dir);
        forget_core(&r.host, &dir, &exec, &exec).await;
        let calls: Vec<String> = kit.last_host().calls().into_iter().map(|c| c.tool).collect();
        assert_eq!(calls, vec!["forget_key".to_string()]);
        assert!(!dir.exists());

        // A legacy host: the wrapper's `forget`, which removes the line.
        let kit = kit_server(false, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let h = legacy_line(&kit, &dir);
        kit.srv.authorize("ssh-ed25519 AAAAother me@laptop");
        forget_core(&h, &dir, &exec_for(&h, &dir), &exec_for(&h, &dir)).await;
        assert_eq!(kit.keys(), "ssh-ed25519 AAAAother me@laptop\n");
        assert!(!dir.exists());
        assert!(kit.hosts.lock().unwrap().is_empty(), "no MCP session on a legacy host");
    }

    #[tokio::test]
    async fn a_wrong_password_leaves_nothing_behind() {
        let kit = kit_server(true, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let e = kit.setup(&dir, "wrong").await.unwrap_err();
        assert!(!e.contains("wrong"), "never echo the password: {e}");
        assert!(!e.contains("jodd-ai-"), "nothing was installed, so no leftover is claimed: {e}");
        assert!(!dir.exists(), "the key dir is removed on failure");
        assert!(!kit.keys().contains("jodd-ai-"));
    }

    #[tokio::test]
    async fn setup_refuses_a_mismatched_host_key() {
        let kit = kit_server(true, || FakeHost::new("1.0.0")).await;
        let other = start(ServerOpts { password: Some("hunter2"), ..Default::default() }).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        // The user confirmed `other`'s fingerprint, but we are talking to `kit`.
        let e = setup_core("127.0.0.1", Some(kit.srv.port), "jodd", "hunter2", &other.host_key_line, &dir, KEY_DIR, &kit.bin)
            .await
            .unwrap_err();
        assert!(e.to_lowercase().contains("identity") || e.to_lowercase().contains("host key"), "{e}");
        assert!(!dir.exists());
        assert!(!kit.keys().contains("jodd-ai-"), "nothing installed");
    }

    #[tokio::test]
    async fn setup_reports_an_unreachable_server() {
        let other = start(ServerOpts::default()).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let e = setup_core("127.0.0.1", Some(1), "jodd", "hunter2", &other.host_key_line, &dir, KEY_DIR, "/usr/local/bin/jodd-ai-mcp")
            .await
            .unwrap_err();
        assert!(!e.is_empty() && !e.contains("hunter2"), "{e}");
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn a_kit_path_that_cannot_be_installed_is_refused_before_connecting() {
        let kit = kit_server(true, || FakeHost::new("1.0.0")).await;
        for bad in ["/opt/a b/jodd-ai-mcp", "/opt/x\"y", "relative/jodd-ai-mcp", ""] {
            let base = tempfile::tempdir().unwrap();
            let dir = base.path().join(KEY_DIR);
            let e = setup_core("127.0.0.1", Some(kit.srv.port), "jodd", "hunter2", &kit.srv.host_key_line, &dir, KEY_DIR, bad)
                .await
                .unwrap_err();
            assert!(!e.contains("jodd-ai-0123abcd"), "nothing was sent, so no leftover is claimed: {bad:?}: {e}");
            assert!(!dir.exists(), "{bad:?}");
            let h = legacy_free_key(&kit.srv, &dir);
            let e = update_core(&h, &dir, "hunter2", bad).await.unwrap_err();
            assert!(!e.contains("hunter2"), "{e}");
        }
        assert_eq!(kit.srv.connections.load(std::sync::atomic::Ordering::SeqCst), 0, "refused before any connection");
        assert!(!kit.keys().contains("jodd-ai-"));
    }

    /// A key and `known_hosts` for `srv` in `dir`, authorised nowhere.
    fn legacy_free_key(srv: &TestServer, dir: &std::path::Path) -> AgentHost {
        crate::backend::ssh::russh_setup::generate_key(dir).unwrap();
        std::fs::write(dir.join("known_hosts"), format!("{}\n", srv.host_key_line)).unwrap();
        AgentHost { target: srv.target(), port: Some(srv.port), key_dir: KEY_DIR.into() }
    }

    #[tokio::test]
    async fn a_locked_host_is_offered_the_command_or_unlock_for_setup_and_reinstall() {
        // `sudo jodd-ai-host lock`: the server offers no password sign-in.
        let srv = start(ServerOpts::default()).await;
        let want = "This host doesn't accept password sign-in. Choose Run a command on the host \
            (needs AI host kit 1.1.0 or later), or run sudo jodd-ai-host unlock on the host, use the password, then run sudo jodd-ai-host lock.";
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let e = setup_core("127.0.0.1", Some(srv.port), "jodd", "hunter2", &srv.host_key_line, &dir, KEY_DIR, mcp::MCP_BIN)
            .await
            .unwrap_err();
        assert_eq!(e, want);
        assert!(!dir.exists());
        let h = legacy_free_key(&srv, &dir);
        assert_eq!(update_core(&h, &dir, "hunter2", mcp::MCP_BIN).await.unwrap_err(), want);
        assert!(dir.join("id_ed25519").exists(), "a failed reinstall keeps the working key");
    }

    #[tokio::test]
    async fn forget_still_removes_the_local_key_when_the_server_is_gone() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("k");
        std::fs::create_dir_all(&dir).unwrap();
        let h = AgentHost { target: "me@127.0.0.1".into(), port: Some(1), key_dir: "k".into() };
        forget_core(&h, &dir, &exec_for(&h, &dir), &exec_for(&h, &dir)).await;
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn status_reports_a_server_that_does_not_answer() {
        let base = tempfile::tempdir().unwrap();
        let h = AgentHost { target: "me@127.0.0.1".into(), port: Some(1), key_dir: "k".into() };
        let exec = exec_for(&h, base.path());
        assert!(status_core(&exec, &exec).await.is_err());
    }

    #[derive(Debug)]
    struct BrokenKey;

    #[async_trait::async_trait]
    impl RemoteMcp for BrokenKey {
        async fn open_mcp(&self, _: CancellationToken)
            -> Result<crate::llm::remote::channel::McpChannel, crate::llm::provider::ExtractError> {
            Err(crate::llm::provider::ExtractError::Transport("the key was refused".into()))
        }
    }

    #[tokio::test]
    async fn installed_but_unverified_removes_the_key_and_names_what_remains() {
        let kit = kit_server(true, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let e = setup_with_verifier("127.0.0.1", Some(kit.srv.port), "jodd", "hunter2", &kit.srv.host_key_line, &dir, KEY_DIR,
            &kit.bin, |_| Box::new(BrokenKey)).await.unwrap_err();
        assert!(e.contains("jodd-ai-0123abcd") && e.contains("authorized_keys"), "{e}");
        assert!(!e.contains("ai-run-v"), "there is no wrapper file to name: {e}");
        assert!(!e.contains("hunter2"), "{e}");
        assert!(!dir.exists(), "the local key is removed");
        assert!(kit.keys().contains("jodd-ai-0123abcd"), "the note is honest: the line really is still there");
    }

    impl Kit {
        async fn prepare(&self, dir: &std::path::Path) -> AiHostPrepared {
            prepare_command_core("127.0.0.1", Some(self.srv.port), "jodd", &self.srv.host_key_line, dir, KEY_DIR)
                .await
                .unwrap()
        }
    }

    /// The `<line>` argument of the shown `sudo` command.
    fn shown_line(cmd: &str) -> &str {
        cmd.strip_prefix("sudo jodd-ai-host add-key '").and_then(|s| s.strip_suffix('\'')).expect(cmd)
    }

    /// What the password path's `install::mcp_line_script` writes to
    /// authorized_keys for this key — run for real in a scratch home, so the
    /// test compares against the script itself, not a copy of its format.
    fn password_path_line(bin: &str, key_id: &str, pubkey_line: &str) -> String {
        let home = tempfile::tempdir().unwrap();
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(install::mcp_line_script(bin, key_id, pubkey_line))
            .env("HOME", home.path())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "installed", "{}", String::from_utf8_lossy(&out.stderr));
        std::fs::read_to_string(home.path().join(".ssh/authorized_keys")).unwrap()
    }

    #[tokio::test]
    async fn command_setup_before_the_key_is_added_explains_and_keeps_the_key() {
        let kit = kit_server(true, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let p = kit.prepare(&dir).await;
        assert_eq!(p.host, AgentHost { target: "jodd@127.0.0.1".into(), port: Some(kit.srv.port), key_dir: KEY_DIR.into() });
        assert_eq!(kit.srv.connections.load(std::sync::atomic::Ordering::SeqCst), 0, "prepare never connects");
        assert!(dir.join("id_ed25519").exists() && dir.join("known_hosts").exists());

        let e = finish_command_core(&p.host, &dir, |e| Box::new(e)).await.unwrap_err();
        assert!(e.contains("doesn't accept Jodd's key yet"), "{e}");
        assert!(dir.join("id_ed25519").exists(), "a failed finish keeps the key for the next try");

        // The shown line is this key, with the comment forget_key looks for.
        let line = shown_line(&p.commands.sudo);
        let body = crate::backend::ssh::scripts::key_body(&std::fs::read_to_string(dir.join("id_ed25519.pub")).unwrap()).to_string();
        assert_eq!(line, format!("{body} jodd-ai-0123abcd"));
        // Kit 1.1.0's `add-key <line>` writes `restrict,command="<bin> --key
        // <id>" <line>`: byte-identical to the password path's line.
        let written = password_path_line(&kit.bin, "0123abcd", line);
        assert_eq!(written, format!("restrict,command=\"{} --key 0123abcd\" {line}\n", kit.bin));
        kit.srv.authorize(&written);

        let r = finish_command_core(&p.host, &dir, |e| Box::new(e)).await.unwrap();
        assert_eq!(r.host, p.host);
        assert_eq!(r.status, mcp_status(&["claude", "codex"], vec![]));
        assert!(kit.last_host().calls().is_empty(), "status is tools/list only: no tool was run");
    }

    #[tokio::test]
    async fn command_setup_on_a_legacy_host_names_the_kit() {
        let kit = kit_server(false, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let p = kit.prepare(&dir).await;
        authorize_on_wrapper(&kit, shown_line(&p.commands.sudo));
        let e = finish_command_core(&p.host, &dir, |e| Box::new(e)).await.unwrap_err();
        assert_eq!(
            e,
            "This host doesn't have the AI host kit, which \"Run a command\" needs. Install the kit, or set up with the password."
        );
        assert!(dir.join("id_ed25519").exists(), "a failed finish keeps the key");
        assert!(kit.hosts.lock().unwrap().is_empty(), "no MCP session on a legacy host");
    }

    #[tokio::test]
    async fn reinstall_commands_match_the_existing_key() {
        // A Command-path host: the same command as setup showed.
        let kit = kit_server(true, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let p = kit.prepare(&dir).await;
        let c = reinstall_commands_core(&p.host, &dir).unwrap();
        assert_eq!((c.sudo.as_str(), c.docker.as_str()), (p.commands.sudo.as_str(), p.commands.docker.as_str()));

        // A legacy host set up by an older Jodd: its own key, moved to the kit.
        let kit = kit_server(false, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let h = legacy_line(&kit, &dir);
        let key_before = std::fs::read(dir.join("id_ed25519")).unwrap();
        let body = crate::backend::ssh::scripts::key_body(&std::fs::read_to_string(dir.join("id_ed25519.pub")).unwrap()).to_string();
        let c = reinstall_commands_core(&h, &dir).unwrap();
        assert_eq!(c.sudo, format!("sudo jodd-ai-host add-key '{body} jodd-ai-0123abcd'"));
        assert_eq!(c.docker, format!("docker compose exec ai-host jodd-ai-host add-key '{body} jodd-ai-0123abcd'"));
        assert_eq!(std::fs::read(dir.join("id_ed25519")).unwrap(), key_before, "no new key");

        let gone = base.path().join("missing");
        assert!(reinstall_commands_core(&h, &gone).unwrap_err().contains("Jodd's key is missing"));
    }

    #[tokio::test]
    async fn cancel_before_the_key_is_added_removes_the_local_key() {
        // Cancel is `forget_core`: the host refuses the key it never got, and
        // the local directory still goes.
        let kit = kit_server(true, || FakeHost::new("1.0.0")).await;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(KEY_DIR);
        let p = kit.prepare(&dir).await;
        let started = std::time::Instant::now();
        forget_core(&p.host, &dir, &exec_for(&p.host, &dir), &exec_for(&p.host, &dir)).await;
        assert!(started.elapsed() < FORGET_DEADLINE, "a refused key fails at once, not at the deadline");
        assert!(!dir.exists());
        assert!(kit.hosts.lock().unwrap().is_empty());
        assert!(!kit.keys().contains("jodd-ai-"));
    }

    #[tokio::test]
    async fn the_command_path_refuses_a_key_dir_that_is_not_a_uuid() {
        for bad in ["../..", "..", "/etc", "a/b", ""] {
            let h = AgentHost { target: "me@127.0.0.1".into(), port: Some(1), key_dir: bad.into() };
            assert!(ai_host_finish_command(h.clone()).await.is_err(), "finish {bad:?}");
            assert!(ai_host_cancel_command(h.clone()).await.is_err(), "cancel {bad:?}");
            assert!(ai_host_reinstall_commands(h).await.is_err(), "reinstall {bad:?}");
        }
    }

    #[tokio::test]
    async fn forget_and_update_refuse_a_key_dir_that_is_not_a_uuid() {
        // Refusal happens in `base_for`, before `forget_core` could remove anything
        // or any connection is attempted.
        for bad in ["../..", "..", "/etc", "a/b", ""] {
            let h = AgentHost { target: "me@127.0.0.1".into(), port: Some(1), key_dir: bad.into() };
            assert!(ai_host_forget(h.clone()).await.is_err(), "forget {bad:?}");
            assert!(ai_host_update(h, "pw".into()).await.is_err(), "update {bad:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::remote::mcp::ToolInfo;

    /// A Command setup left open past `ORPHAN_MIN_AGE` can have its key
    /// directory swept by another Jodd; finishing it then says so.
    #[tokio::test]
    async fn command_finish_after_the_directory_was_swept_says_it_expired() {
        let base = tempfile::tempdir().unwrap();
        let h = AgentHost { target: "jodd@127.0.0.1".into(), port: Some(22), key_dir: "gone".into() };
        let e = finish_command_core(&h, &base.path().join("gone"), |e| Box::new(e)).await.unwrap_err();
        assert_eq!(e, "This setup expired — start again.");
    }

    #[test]
    fn add_key_commands_quote_the_line() {
        let line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabc jodd-ai-0123abcd";
        let c = add_key_commands(line).unwrap();
        assert_eq!(c.sudo, "sudo jodd-ai-host add-key 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabc jodd-ai-0123abcd'");
        assert_eq!(
            c.docker,
            "docker compose exec ai-host jodd-ai-host add-key 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabc jodd-ai-0123abcd'"
        );
        assert!(add_key_commands("ssh-ed25519 AAAA'; rm -rf ~; ' jodd-ai-0123abcd").is_err());
    }

    #[test]
    fn status_parsing() {
        let s = parse_status("version=1\npreset=claude path=-\npreset=codex path=/usr/bin/codex\n");
        assert_eq!(s, AiHostStatus::Legacy {
            version: 1,
            current: false,
            presets: vec![
                AiHostPreset { id: "claude".into(), path: None },
                AiHostPreset { id: "codex".into(), path: Some("/usr/bin/codex".into()) },
            ],
            notices: vec![legacy_notice()],
        });
    }

    fn infer(harnesses: &[&str]) -> ToolInfo {
        ToolInfo { name: "infer".into(), tool_version: 1, harnesses: harnesses.iter().map(|h| h.to_string()).collect() }
    }

    fn forget_key() -> ToolInfo {
        ToolInfo { name: "forget_key".into(), tool_version: 1, harnesses: vec![] }
    }

    fn info(version: &str, tools: Vec<ToolInfo>) -> HostInfo {
        HostInfo { version: version.into(), tools }
    }

    /// (feature, kind) of each notice, in order.
    fn kinds(n: &[AiHostNotice]) -> Vec<(&str, NoticeKind)> {
        n.iter().map(|n| (n.feature.as_str(), n.kind)).collect()
    }

    const UPGRADE: &str = ": on the host, re-run the installer, or run `docker compose pull && docker compose up -d`.";

    #[test]
    fn notices_follow_the_mismatch_table() {
        use NoticeKind::*;
        // Too old for a tool it does not list.
        let n = notices_for(&info("0.9.0", vec![infer(&["claude", "codex"])]));
        assert_eq!(kinds(&n), vec![("forget_key", UpgradeHost)]);
        assert_eq!(n[0].text, format!("Upgrade the AI host to use Remove AI host{UPGRADE}"));
        // New enough, but a harness is off.
        let n = notices_for(&info("1.0.0", vec![infer(&["claude"]), forget_key()]));
        assert_eq!(kinds(&n), vec![("codex", AskAdmin)]);
        assert_eq!(n[0].text, "Codex is turned off on this host. Ask its administrator to run `sudo jodd-ai-host enable codex`.");
        // A tool newer than this Jodd knows.
        let n = notices_for(&info("1.0.0", vec![ToolInfo { tool_version: 2, ..infer(&["claude", "codex"]) }, forget_key()]));
        assert_eq!(kinds(&n), vec![("infer", UpdateJodd)]);
        assert_eq!(n[0].text, "Update Jodd to use AI features on this host.");
        // A version that does not parse counts as 0.0.0: `infer` is unknown to it.
        let n = notices_for(&info("garbage", vec![forget_key()]));
        assert_eq!(kinds(&n), vec![("claude", UpgradeHost), ("codex", UpgradeHost)]);
        assert_eq!(n[0].text, format!("Upgrade the AI host to use Claude Code{UPGRADE}"));
        assert_eq!(n[1].text, format!("Upgrade the AI host to use Codex{UPGRADE}"));
        // Everything there.
        assert!(notices_for(&info("1.0.0", vec![infer(&["claude", "codex"]), forget_key()])).is_empty());
    }

    #[test]
    fn notices_for_a_new_enough_host_with_tools_missing() {
        use NoticeKind::*;
        // Every harness off: `infer` is unlisted, yet the host knows it.
        let n = notices_for(&info("1.0.0", vec![forget_key()]));
        assert_eq!(kinds(&n), vec![("claude", AskAdmin), ("codex", AskAdmin)]);
        assert_eq!(n[0].text, "Claude Code is turned off on this host. Ask its administrator to run `sudo jodd-ai-host enable claude`.");
        // A tool it should list and does not.
        let n = notices_for(&info("1.2.0", vec![infer(&["claude", "codex"])]));
        assert_eq!(kinds(&n), vec![("forget_key", AskAdmin)]);
        assert!(n[0].text.contains("jodd-ai-host doctor"), "{}", n[0].text);
        // A harness this Jodd has no preset for is not Jodd's business.
        assert!(notices_for(&info("1.0.0", vec![infer(&["claude", "codex", "gemini"]), forget_key()])).is_empty());
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(notices_for(&info("1.10.0", vec![infer(&["claude", "codex"])]))[0].kind == NoticeKind::AskAdmin);
        for old in ["0.10.99", "", "1.0", "v1.0.0", "1.0.0.0", "1.x.0"] {
            assert_eq!(notices_for(&info(old, vec![infer(&["claude", "codex"])]))[0].kind, NoticeKind::UpgradeHost, "{old:?}");
        }
    }

    #[test]
    fn the_legacy_notice_says_how_to_upgrade() {
        let n = legacy_notice();
        assert_eq!(n.kind, NoticeKind::UpgradeHost);
        assert_eq!(n.text, "This AI host uses the old helper. Upgrade it: install the AI host kit on the host \
            (https://github.com/BBM-Co-ORG/jodd-ai-host#install), then run `sudo jodd-ai-host adopt-keys`. \
            AI features keep working meanwhile.");
    }

    #[test]
    fn the_status_json_is_what_the_frontend_reads() {
        let s = AiHostStatus::Mcp {
            version: "1.0.0".into(),
            harnesses: vec!["claude".into()],
            notices: vec![AiHostNotice { feature: "codex".into(), kind: NoticeKind::AskAdmin, text: "t".into() }],
        };
        assert_eq!(serde_json::to_value(&s).unwrap(), serde_json::json!({
            "protocol": "mcp", "version": "1.0.0", "harnesses": ["claude"],
            "notices": [{ "feature": "codex", "kind": "ask_admin", "text": "t" }],
        }));
        let l = AiHostStatus::Legacy { version: 2, current: true, presets: vec![AiHostPreset { id: "codex".into(), path: None }], notices: vec![] };
        assert_eq!(serde_json::to_value(&l).unwrap(), serde_json::json!({
            "protocol": "legacy", "version": 2, "current": true,
            "presets": [{ "id": "codex", "path": null }], "notices": [],
        }));
        for (k, want) in [(NoticeKind::UpgradeHost, "upgrade_host"), (NoticeKind::AskAdmin, "ask_admin"), (NoticeKind::UpdateJodd, "update_jodd")] {
            assert_eq!(serde_json::to_value(k).unwrap(), want);
        }
    }
}
