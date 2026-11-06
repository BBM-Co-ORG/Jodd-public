//! Contract tests: this Jodd against the pinned AI host image (spec
//! 2026-10-30-ai-host-subproject §6), `ghcr.io/bbm-co-org/jodd-ai-host-test`
//! — the real `jodd-ai-mcp` behind real OpenSSH, with fake `claude` and
//! `codex` that echo what they are given (the host repo's
//! `test/docker/fake-*`).
//!
//! CI's `ai-host-contract` job starts the image and runs
//! `cargo test -p jodd --lib llm::remote::contract_tests -- --ignored --test-threads=1`
//! with:
//! - `JODD_AI_CONTRACT=127.0.0.1:<port>` — the container's published sshd;
//! - `JODD_AI_CONTRACT_PASSWORD` — the AI account's password (`jodd-ai`);
//! - `JODD_AI_CONTRACT_CONTAINER` — its name, for `docker exec` as root.
//!
//! Each test sets up its own key (its own key directory and id). One thread:
//! two tests change the host's policy for codex and put it back.
//!
//! Every provider here is built with `AgentCliProvider::with_remote`, which is
//! `AgentCliProvider::remote` minus `RusshExec::for_host`: that resolves the
//! key under the real app data directory, and a test must never touch it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::accounts::AgentHost;
use crate::ai_host_commands::{
    forget_core, legacy_notice, legacy_status, setup_core, status_core, AiHostNotice, AiHostPreset, AiHostStatus,
    NoticeKind,
};
use crate::llm::agent_cli::{forget_legacy, remember_legacy, AgentCliProvider};
use crate::llm::provider::{flatten_turns, ChatRole, ChatTurn, ExtractError, LlmProvider};
use crate::llm::remote::channel::RemoteMcp;
use crate::llm::remote::exec::{RemoteExec, RusshExec};
use crate::llm::remote::mcp::MCP_BIN;

/// The host's `serverInfo.version`: the image tag CI pins (keep in step with
/// `JODD_AI_HOST_TEST_IMAGE` in ci.yml).
const PINNED_VERSION: &str = "1.0.0";
/// The AI account the image creates.
const USER: &str = "jodd-ai";
const SYSTEM: &str = "contract system text";

struct Contract {
    host: String,
    port: u16,
    password: String,
    container: String,
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set: these tests run against the pinned AI host image (the ai-host-contract job in ci.yml)")
    })
}

fn contract() -> Contract {
    let addr = env("JODD_AI_CONTRACT");
    let (host, port) = addr.rsplit_once(':').unwrap_or_else(|| panic!("JODD_AI_CONTRACT={addr:?} is not host:port"));
    Contract {
        host: host.to_string(),
        port: port.parse().unwrap_or_else(|_| panic!("JODD_AI_CONTRACT={addr:?}: bad port")),
        password: env("JODD_AI_CONTRACT_PASSWORD"),
        container: env("JODD_AI_CONTRACT_CONTAINER"),
    }
}

impl Contract {
    /// `sh -c script` in the container, as root: (exit code, stdout, stderr).
    fn docker(&self, script: &str) -> (i32, String, String) {
        let out = Command::new("docker")
            .args(["exec", &self.container, "sh", "-c", script])
            .output()
            .expect("docker exec");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// [`Self::docker`] that must succeed; its stdout.
    fn root(&self, script: &str) -> String {
        let (code, out, err) = self.docker(script);
        assert_eq!(code, 0, "in the container: {script}\nstdout: {out}\nstderr: {err}");
        out
    }

    /// The line the user would have confirmed: the container's host key,
    /// read from inside it (the host repo's e2e pins it the same way).
    fn known_hosts_line(&self) -> String {
        let key = self.root(r#". /etc/jodd-ai-host.conf && cut -d" " -f1-2 "$JODD_AI_HOSTKEY""#);
        format!("[{}]:{} {}", self.host, self.port, key.trim())
    }

    fn authorized_keys(&self) -> String {
        self.root("cat /home/jodd-ai/.ssh/authorized_keys")
    }

    /// The host's one metadata line per call, for this key.
    fn calls(&self, key_id: &str) -> Vec<String> {
        // The file appears with the host's first logged call.
        self.root("cat /var/log/jodd-ai-host/calls.log 2>/dev/null; true")
            .lines()
            .filter(|l| l.contains(&format!("key={key_id} ")))
            .map(str::to_string)
            .collect()
    }
}

/// One key, set up on the host with the password, as the Settings screen does.
struct Host {
    _base: tempfile::TempDir,
    dir: PathBuf,
    agent: AgentHost,
    key_id: String,
    status: AiHostStatus,
}

impl Host {
    async fn set_up(c: &Contract) -> Host {
        let key_dir = uuid::Uuid::new_v4().to_string();
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join(&key_dir);
        let r = setup_core(&c.host, Some(c.port), USER, &c.password, &c.known_hosts_line(), &dir, &key_dir, MCP_BIN)
            .await
            .unwrap_or_else(|e| panic!("setup: {e}"));
        Host { key_id: crate::llm::remote::key_id_for(&key_dir), agent: r.host, status: r.status, dir, _base: base }
    }

    fn exec(&self) -> RusshExec {
        exec_in(&self.agent, &self.dir)
    }

    /// This key's one line in the AI account's `authorized_keys`.
    fn authorized_keys_line(&self, c: &Contract) -> String {
        let all = c.authorized_keys();
        let mine: Vec<&str> = all.lines().filter(|l| l.ends_with(&format!(" jodd-ai-{}", self.key_id))).collect();
        assert_eq!(mine.len(), 1, "{all}");
        mine[0].to_string()
    }

    fn provider(&self, preset: &str) -> AgentCliProvider {
        let spec = crate::llm::presets::preset_by_id(preset).unwrap();
        let exec = Arc::new(self.exec());
        let label = crate::llm::remote::host_label(&self.agent);
        AgentCliProvider::with_remote(spec, preset, label, self.agent.key_dir.clone(), exec.clone(), exec)
    }
}

fn exec_in(h: &AgentHost, dir: &Path) -> RusshExec {
    RusshExec {
        target: h.target.clone(),
        port: h.port,
        key: dir.join("id_ed25519"),
        known_hosts: dir.join("known_hosts"),
        label: crate::llm::remote::host_label(h),
    }
}

fn turns(text: &str) -> Vec<ChatTurn> {
    vec![ChatTurn { role: ChatRole::User, content: text.to_string() }]
}

/// What the host feeds the CLI for `chat(SYSTEM, turns(text))`: the legacy
/// wrapper's join, byte for byte.
fn joined(text: &str) -> String {
    format!("{SYSTEM}\n\n---\n\n{}", flatten_turns("", &turns(text)))
}

/// Puts codex back as the image enabled it (`/usr/local/bin/codex`) when a
/// test that changed the policy ends, passing or not.
struct RestoreCodex<'a>(&'a Contract);

impl Drop for RestoreCodex<'_> {
    fn drop(&mut self) {
        let (code, out, err) = self.0.docker("jodd-ai-host enable codex");
        if code != 0 {
            eprintln!("could not re-enable codex on the contract host: {out}{err}");
        }
    }
}

fn mcp_status(harnesses: &[&str], notices: Vec<AiHostNotice>) -> AiHostStatus {
    AiHostStatus::Mcp {
        version: PINNED_VERSION.into(),
        harnesses: harnesses.iter().map(|h| h.to_string()).collect(),
        notices,
    }
}

#[tokio::test]
#[ignore = "needs the pinned AI host image (the ai-host-contract job in ci.yml)"]
async fn contract_setup_over_password_then_mcp_status() {
    let c = contract();
    let h = Host::set_up(&c).await;
    match &h.status {
        AiHostStatus::Mcp { version, harnesses, notices } => {
            assert_eq!(version, PINNED_VERSION);
            for want in ["claude", "codex"] {
                assert!(harnesses.iter().any(|x| x == want), "{harnesses:?}");
            }
            assert_eq!(notices, &vec![], "a fresh host with both CLIs needs nothing");
        }
        other => panic!("{other:?}"),
    }
    // The line forces the kit's binary, under this key's own comment.
    let id = &h.key_id;
    let line = h.authorized_keys_line(&c);
    assert!(line.starts_with(&format!("restrict,command=\"{MCP_BIN} --key {id}\" ssh-ed25519 ")), "{line}");
    // And the key alone, afterwards, gets the same status.
    let exec = h.exec();
    assert_eq!(status_core(&exec, &exec).await.unwrap(), h.status);
    assert!(c.calls(id).iter().all(|l| !l.contains("tool=infer")), "status runs no CLI: {:?}", c.calls(id));
}

#[tokio::test]
#[ignore = "needs the pinned AI host image (the ai-host-contract job in ci.yml)"]
async fn contract_infer_claude_and_codex_through_the_fakes() {
    let c = contract();
    let h = Host::set_up(&c).await;

    // codex: the fake writes its stdin to --output-last-message, which is the
    // answer. So the answer IS what reached the CLI: the system join included.
    let got = h.provider("codex").chat(SYSTEM, &turns("codex marker"), CancellationToken::new()).await.unwrap();
    assert_eq!(got, joined("codex marker"));

    // claude: the fake prints `{"result":"<stdin>",…}` without escaping, and
    // Jodd's stdin always holds the join's newlines, so its JSON never parses
    // — whatever Jodd sends. The host's "answer could not be read" still
    // carries the CLI's stdout, which shows the join reached claude intact.
    match h.provider("claude").chat(SYSTEM, &turns("claude marker"), CancellationToken::new()).await {
        Err(ExtractError::MalformedEnvelope { reason, raw }) => {
            assert!(reason.contains("could not be read"), "{reason}");
            assert!(raw.starts_with(&format!("{{\"result\":\"{}\"", joined("claude marker"))), "{raw}");
        }
        other => panic!("{other:?}"),
    }

    // Both went over MCP (`infer`), never the legacy shim (`jodd-ai-run`).
    let calls = c.calls(&h.key_id);
    assert!(calls.iter().any(|l| l.contains("tool=infer harness=codex ") && l.ends_with("outcome=ok")), "{calls:?}");
    assert!(calls.iter().any(|l| l.contains("tool=infer harness=claude ")), "{calls:?}");
    assert!(calls.iter().all(|l| !l.contains("tool=jodd-ai-run")), "{calls:?}");
}

/// A CLI that takes a minute: it reads its prompt, then records the pid of
/// the `sleep` it waits on, as the image's fake codex does for the prompt
/// `sleep`. Jodd cannot send that prompt bare (the system join always comes
/// first), so this test plants its own slow CLI and points codex at it.
const SLOW_CLI: &str = "/usr/local/bin/jodd-contract-slow";
const SLOW_PID: &str = "/tmp/jodd-contract-slow.pid";

#[tokio::test]
#[ignore = "needs the pinned AI host image (the ai-host-contract job in ci.yml)"]
async fn contract_cancel_stops_the_cli_on_the_host() {
    let c = contract();
    let h = Host::set_up(&c).await;
    let _restore = RestoreCodex(&c);
    c.root(&format!(
        "printf '%s\\n' '#!/bin/sh' 'cat > /dev/null' 'sleep 60 &' 'echo $! > {SLOW_PID}' 'wait $!' > {SLOW_CLI} \
         && chmod 755 {SLOW_CLI} && rm -f {SLOW_PID} \
         && jodd-ai-mcp admin enable codex --path {SLOW_CLI} --path-env /usr/local/bin:/usr/bin:/bin"
    ));

    let cancel = CancellationToken::new();
    let p = h.provider("codex");
    let run = {
        let cancel = cancel.clone();
        tokio::spawn(async move { p.chat(SYSTEM, &turns("cancel me"), cancel).await })
    };
    // Until the CLI is running on the host.
    let started = Instant::now();
    let pid = loop {
        let (_, out, _) = c.docker(&format!("cat {SLOW_PID} 2>/dev/null"));
        if !out.trim().is_empty() {
            break out.trim().to_string();
        }
        assert!(started.elapsed() < Duration::from_secs(30), "the slow CLI never started on the host");
        if run.is_finished() {
            panic!("the run ended before the CLI started: {:?}", run.await);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let alive = format!(
        "st=$(ps -o stat= -p {pid} | tr -d ' '); case $st in ''|Z*) ;; *) echo alive ;; esac; \
         ps -eo stat=,args= | grep -v '^ *Z' | grep -q '[j]odd-contract-slow' && echo alive; true"
    );
    assert!(c.root(&alive).contains("alive"), "the CLI is not running before the cancel");

    let at = Instant::now();
    cancel.cancel();
    let got = tokio::time::timeout(Duration::from_secs(10), run).await.expect("the cancelled run did not return").unwrap();
    assert!(matches!(got, Err(ExtractError::Cancelled)), "{got:?}");
    loop {
        if !c.root(&alive).contains("alive") {
            break;
        }
        assert!(at.elapsed() < Duration::from_secs(7), "the CLI outlived the cancel by 7 s (pid {pid})");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    eprintln!("the CLI was gone {} ms after the cancel", at.elapsed().as_millis());
}

#[tokio::test]
#[ignore = "needs the pinned AI host image (the ai-host-contract job in ci.yml)"]
async fn contract_disabled_harness_is_not_enabled_and_noticed() {
    let c = contract();
    let h = Host::set_up(&c).await;
    let restore = RestoreCodex(&c);
    c.root("jodd-ai-host disable codex");

    let exec = h.exec();
    let codex_off = AiHostNotice {
        feature: "codex".into(),
        kind: NoticeKind::AskAdmin,
        text: "Codex is turned off on this host. Ask its administrator to run `sudo jodd-ai-host enable codex`.".into(),
    };
    assert_eq!(status_core(&exec, &exec).await.unwrap(), mcp_status(&["claude"], vec![codex_off]));

    match h.provider("codex").chat(SYSTEM, &turns("x"), CancellationToken::new()).await {
        Err(ExtractError::NotConfigured(m)) => assert!(m.contains("sudo jodd-ai-host enable codex"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(c.calls(&h.key_id).iter().any(|l| l.contains("tool=infer harness=codex ")), "{:?}", c.calls(&h.key_id));

    drop(restore);
    assert_eq!(status_core(&exec, &exec).await.unwrap(), mcp_status(&["claude", "codex"], vec![]), "re-enabled");
}

#[tokio::test]
#[ignore = "needs the pinned AI host image (the ai-host-contract job in ci.yml)"]
async fn contract_legacy_client_still_works_against_the_new_host() {
    // Spec §10 item 1: a Jodd that predates MCP, on a key whose line already
    // points at jodd-ai-mcp (Setup installed it here; on a real host
    // `adopt-keys` re-points an old line the same way).
    let c = contract();
    let h = Host::set_up(&c).await;
    assert!(h.authorized_keys_line(&c).starts_with(&format!("restrict,command=\"{MCP_BIN} --key ")));

    // The old client's status: the wrapper's `jodd-ai-run 2 status` verb.
    let exec = h.exec();
    assert_eq!(legacy_status(&exec).await.unwrap(), AiHostStatus::Legacy {
        version: crate::llm::remote::wrapper::WRAPPER_VERSION,
        current: true,
        presets: vec![
            AiHostPreset { id: "claude".into(), path: Some("/usr/local/bin/claude".into()) },
            AiHostPreset { id: "codex".into(), path: Some("/usr/local/bin/codex".into()) },
        ],
        notices: vec![legacy_notice()],
    });

    // The old client's run: `jodd-ai-run 2 run codex`, framed as wrapper v2.
    remember_legacy(&h.agent.key_dir);
    let got = h.provider("codex").chat(SYSTEM, &turns("legacy marker"), CancellationToken::new()).await;
    forget_legacy(&h.agent.key_dir);
    assert_eq!(got.unwrap(), joined("legacy marker"));

    let calls = c.calls(&h.key_id);
    assert!(calls.iter().any(|l| l.contains("tool=jodd-ai-run harness=codex ") && l.ends_with("outcome=ok")), "{calls:?}");
    assert!(calls.iter().all(|l| !l.contains("tool=infer")), "the legacy path never spoke MCP: {calls:?}");
}

#[tokio::test]
#[ignore = "needs the pinned AI host image (the ai-host-contract job in ci.yml)"]
async fn contract_forget_key_revokes() {
    let c = contract();
    let h = Host::set_up(&c).await;
    // Remove deletes the key directory; keep a copy to knock with afterwards.
    let kept = tempfile::tempdir().unwrap();
    for f in ["id_ed25519", "id_ed25519.pub", "known_hosts"] {
        std::fs::copy(h.dir.join(f), kept.path().join(f)).unwrap();
    }
    let exec = h.exec();
    assert!(matches!(status_core(&exec, &exec).await, Ok(AiHostStatus::Mcp { .. })), "the key works before Remove");

    forget_core(&h.agent, &h.dir, &exec, &exec).await;
    assert!(!h.dir.exists(), "the local key is gone");
    let id = &h.key_id;
    assert!(!c.authorized_keys().contains(&format!(" jodd-ai-{id}")), "the host dropped the line");
    let calls = c.calls(id);
    assert!(calls.iter().any(|l| l.contains("tool=forget_key ") && l.ends_with("outcome=removed")), "{calls:?}");

    // The same key, again: the server no longer accepts it, on either request.
    let old = exec_in(&h.agent, kept.path());
    match old.exec("jodd-ai-run 2 status", b"", Duration::from_secs(30), CancellationToken::new()).await {
        Err(ExtractError::NotConfigured(m)) => assert!(m.contains("no longer accepts Jodd's key"), "{m}"),
        other => panic!("{other:?}"),
    }
    match old.open_mcp(CancellationToken::new()).await {
        Err(ExtractError::NotConfigured(m)) => assert!(m.contains("no longer accepts Jodd's key"), "{m}"),
        Err(e) => panic!("{e:?}"),
        Ok(_) => panic!("a forgotten key opened an MCP session"),
    }
}
