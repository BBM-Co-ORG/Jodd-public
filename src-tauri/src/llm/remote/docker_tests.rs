//! Real OpenSSH (Alpine, in Docker) — what the in-process server cannot
//! prove: `restrict`, pty refusal, and that Stop reaches the CLI through
//! sshd closing the forced command's stdin. Every test is `#[ignore]`
//! and returns early without `JODD_AI_DOCKER`; run them with
//! `sh scripts/ai-host-docker/run-tests.sh`.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::ai_host_commands::{forget_core, setup_core, status_core, AiHostSetupResult};
use crate::llm::provider::ExtractError;
use crate::llm::remote::exec::{RemoteExec, RusshExec};
use crate::llm::remote::wrapper::WRAPPER_VERSION;

const KEY_DIR: &str = "feedbeef-0000-4000-8000-000000000000";
const CONTAINER: &str = "jodd-ai-host-test";

/// `JODD_AI_DOCKER=127.0.0.1:2222` → 2222. `None` skips the test.
fn port() -> Option<u16> {
    std::env::var("JODD_AI_DOCKER").ok()?.rsplit_once(':')?.1.parse().ok()
}

/// A full setup over the container's password login, into `dir`.
async fn setup(dir: &std::path::Path) -> (AiHostSetupResult, RusshExec) {
    let port = port().unwrap();
    let fp = crate::backend::ssh::russh_setup::fetch_fingerprint("127.0.0.1", Some(port)).await.unwrap();
    let r = setup_core("127.0.0.1", Some(port), "jodd", "jodd-test-pw", &fp.known_hosts_line, dir, KEY_DIR)
        .await
        .unwrap();
    let exec = RusshExec {
        target: r.host.target.clone(),
        port: r.host.port,
        key: dir.join("id_ed25519"),
        known_hosts: dir.join("known_hosts"),
        label: "docker".into(),
    };
    (r, exec)
}

fn run_req(preset: &str) -> String {
    format!("jodd-ai-run {WRAPPER_VERSION} run {preset}")
}

/// What `run_remote` sends: protocol v2 framing, no schema.
fn v2(prompt: &str) -> Vec<u8> {
    crate::llm::agent_cli::remote_stdin(None, prompt)
}

/// How many times the fake codex has started (it appends a line per start).
fn fake_starts() -> usize {
    in_container("cat /tmp/fake.starts 2>/dev/null | wc -l").parse().unwrap()
}

/// The fake's `sleep` is alive (a zombie, state Z, counts as gone).
fn fake_alive(pid: &str) -> bool {
    in_container(&format!(
        "s=$(awk '/^State:/{{print $2}}' /proc/{pid}/status 2>/dev/null); \
         if [ -n \"$s\" ] && [ \"$s\" != Z ]; then echo alive; else echo gone; fi"
    )) == "alive"
}

fn scratch_dirs() -> usize {
    in_container("ls -d /tmp/jodd-ai.* 2>/dev/null | wc -l").parse().unwrap()
}

fn in_container(script: &str) -> String {
    let o = std::process::Command::new("docker").args(["exec", CONTAINER, "sh", "-c", script]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[tokio::test]
#[ignore]
async fn setup_finds_the_clis_through_the_login_shell() {
    if port().is_none() { return; }
    let d = tempfile::tempdir().unwrap();
    let (r, _) = setup(&d.path().join("k")).await;
    assert!(r.status.current, "{:?}", r.status);
    assert_eq!(r.status.presets.len(), 2, "{:?}", r.status);
    // ~/.local/bin is on PATH only via the login shell's .profile.
    for p in &r.status.presets {
        let path = p.path.as_deref().unwrap_or("");
        assert!(path.contains("/.local/bin/"), "{p:?}");
    }
}

#[tokio::test]
#[ignore]
async fn the_restricted_key_refuses_everything_but_jodd_requests() {
    if port().is_none() { return; }
    let d = tempfile::tempdir().unwrap();
    let (_, exec) = setup(&d.path().join("k")).await;
    let custom = format!("jodd-ai-run {WRAPPER_VERSION} run custom");
    for cmd in ["id", "sh -c id", "cat /etc/passwd", &custom] {
        let out = exec.exec(cmd, b"", Duration::from_secs(10), CancellationToken::new()).await.unwrap();
        assert_eq!(out.exit, Some(64), "{cmd}: {out:?}");
        assert!(!out.stdout.contains("uid=") && !out.stdout.contains("root:"), "{cmd}: {out:?}");
    }
}

#[tokio::test]
#[ignore]
async fn a_pty_request_is_refused() {
    if port().is_none() { return; }
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("k");
    let (r, _) = setup(&dir).await;
    let h = crate::backend::ssh::russh_session::connect(
        &r.host.target,
        r.host.port,
        std::fs::read_to_string(dir.join("known_hosts")).unwrap(),
        crate::backend::ssh::russh_session::Credential::Key(&dir.join("id_ed25519")),
    )
    .await
    .unwrap();
    let mut ch = h.channel_open_session().await.unwrap();
    // `restrict` implies no-pty: sshd answers the pty request with failure.
    ch.request_pty(true, "xterm", 80, 24, 0, 0, &[]).await.unwrap();
    let first = ch.wait().await;
    assert!(matches!(first, Some(russh::ChannelMsg::Failure)), "{first:?}");
}

#[tokio::test]
#[ignore]
async fn a_run_goes_through_the_forced_command() {
    if port().is_none() { return; }
    let d = tempfile::tempdir().unwrap();
    let (_, exec) = setup(&d.path().join("k")).await;
    let out = exec
        .exec(&run_req("codex"), &v2("สวัสดี"), Duration::from_secs(30), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.exit, Some(0), "{out:?}");
    assert_eq!(out.stdout, "สวัสดี", "{out:?}");
    assert!(out.stderr.contains("codex stderr line"), "{out:?}");

    let out = exec
        .exec(&run_req("claude"), &v2("hi"), Duration::from_secs(30), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.exit, Some(0), "{out:?}");
    assert!(out.stdout.contains(r#""result":"hi""#), "{out:?}");
}

/// Stop must stop the CLI on the server. sshd refuses `signal` requests to a
/// forced command (measured 2026-10-13), but closes its stdin as soon as the
/// client goes away (measured 2026-10-14): wrapper v2 turns that EOF into a
/// kill of the CLI's process group.
/// Starts a run of the fake codex with `prompt`, Stops it once the fake is
/// asleep, and returns whether the fake's `sleep` was still alive `within`
/// after Stop, and how long it took to go.
async fn stop_and_watch(prompt: &str, within: Duration) -> (bool, Duration) {
    let d = tempfile::tempdir().unwrap();
    let (_, exec) = setup(&d.path().join("k")).await;
    // Emptied as root, so hand it back to the user the fake runs as.
    in_container(": > /tmp/fake.pid; chown jodd:jodd /tmp/fake.pid");
    let cancel = CancellationToken::new();
    let run = tokio::spawn({
        let (exec, cancel, stdin) = (exec.clone(), cancel.clone(), v2(prompt));
        async move { exec.exec(&run_req("codex"), &stdin, Duration::from_secs(120), cancel).await }
    });
    let mut pid = String::new();
    for _ in 0..100 {
        pid = in_container("cat /tmp/fake.pid");
        if !pid.is_empty() { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!pid.is_empty(), "the fake CLI never started");
    assert!(fake_alive(&pid), "the fake CLI must be running before Stop");
    cancel.cancel();
    let r = run.await.unwrap();
    assert!(matches!(r, Err(ExtractError::Cancelled)), "{r:?}");
    let stopped = std::time::Instant::now();
    while fake_alive(&pid) && stopped.elapsed() < within {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let alive = fake_alive(&pid);
    println!("{prompt}: fake CLI {} {:?} after Stop; scratch dirs left = {}",
        if alive { "STILL ALIVE" } else { "gone" }, stopped.elapsed(), scratch_dirs());
    (alive, stopped.elapsed())
}

#[tokio::test]
#[ignore]
async fn cancel_stops_the_cli_on_the_server() {
    if port().is_none() { return; }
    let (alive, _) = stop_and_watch("sleep", Duration::from_secs(2)).await;
    assert!(!alive, "the CLI outlived Stop by 2s");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(scratch_dirs(), 0, "the prompt must not outlive the call");
}

/// A CLI that ignores TERM: the wrapper exits at once (nobody is listening),
/// and its detached `reap` KILLs the group 5 s later.
#[tokio::test]
#[ignore]
async fn cancel_kills_a_cli_that_ignores_term() {
    if port().is_none() { return; }
    let (alive, _) = stop_and_watch("stubborn", Duration::from_secs(8)).await;
    assert!(!alive, "a CLI that ignores TERM outlived Stop by 8s");
    assert_eq!(scratch_dirs(), 0, "the prompt must not outlive the call");
}

/// Stop while the prompt is still uploading: the wrapper has part of it and
/// stdin then ends. It must not start the CLI on a truncated prompt.
#[tokio::test]
#[ignore]
async fn cancel_during_the_upload_never_starts_the_cli() {
    if port().is_none() { return; }
    let d = tempfile::tempdir().unwrap();
    let (_, exec) = setup(&d.path().join("k")).await;
    let before = fake_starts();
    // Promise a megabyte, send five bytes: the server sits mid-upload.
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        c2.cancel();
    });
    let r = exec.exec(&run_req("codex"), b"-\n1000000\nsleep", Duration::from_secs(60), cancel).await;
    assert!(matches!(r, Err(ExtractError::Cancelled)), "{r:?}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(fake_starts(), before, "the CLI started on a truncated prompt");
    assert_eq!(scratch_dirs(), 0, "the partial prompt must not outlive the call");
}

#[tokio::test]
#[ignore]
async fn forget_revokes_the_key() {
    if port().is_none() { return; }
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("k");
    let (r, exec) = setup(&dir).await;
    // keep a copy of the key to try after forget
    let keep = d.path().join("kept");
    std::fs::create_dir(&keep).unwrap();
    for f in ["id_ed25519", "known_hosts"] {
        std::fs::copy(dir.join(f), keep.join(f)).unwrap();
    }
    forget_core(&r.host, &dir, &exec).await;
    assert!(!dir.exists());
    let again = RusshExec { key: keep.join("id_ed25519"), known_hosts: keep.join("known_hosts"), ..exec.clone() };
    let e = status_core(&again).await.unwrap_err();
    assert!(e.contains("no longer accepts"), "{e}");
}
