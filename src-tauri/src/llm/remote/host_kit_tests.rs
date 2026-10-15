//! A host built by the published kit (installer or image), checked with
//! Jodd's real setup code. Local only: `scripts/ai-host-e2e/*.sh` set the env.
//!
//! Env contract (every test is `#[ignore]` and returns early without it):
//! - `JODD_AI_E2E=<host>:<port>`      the host's sshd. Needed by every test.
//! - `JODD_AI_E2E_USER`               the AI account (default `jodd-ai`).
//! - `JODD_AI_E2E_PASSWORD`           its password (the tests that run Setup).
//! - `JODD_AI_E2E_CONTAINER`          container name for `docker exec`. The
//!   tests that drive `jodd-ai-host` on the host return early without it, so
//!   the rest can run against a real machine that has no container.
//!
//! Run with `--test-threads=1`: `lock` turns password login off for the whole
//! account, so it would break every other test running beside it. The tests
//! leave `jodd-ai-abcdef0N` keys on the host; use a throwaway one.
//!
//! Each test uses its own key-dir uuid, so `revoke <8-hex>` names exactly one
//! authorized_keys line and tests never see each other's keys.

use crate::ai_host_commands::{setup_core, status_core, AiHostSetupResult};
use crate::llm::remote::exec::RusshExec;

/// `None` (a visible skip) when `JODD_AI_E2E` is unset; a set but malformed
/// value is a mistake in the driver and panics rather than skipping.
fn target() -> Option<(String, u16)> {
    let Ok(v) = std::env::var("JODD_AI_E2E") else {
        eprintln!("skipped: JODD_AI_E2E not set");
        return None;
    };
    let parsed = v.rsplit_once(':').and_then(|(h, p)| Some((h.to_string(), p.parse().ok()?)));
    Some(parsed.unwrap_or_else(|| panic!("JODD_AI_E2E must be <host>:<port>, got {v:?}")))
}

fn user() -> String {
    std::env::var("JODD_AI_E2E_USER").unwrap_or_else(|_| "jodd-ai".into())
}

fn password() -> String {
    std::env::var("JODD_AI_E2E_PASSWORD").expect("JODD_AI_E2E_PASSWORD")
}

/// `None` when there is no container to `docker exec` into (a real host).
fn container() -> Option<String> {
    let c = std::env::var("JODD_AI_E2E_CONTAINER").ok().filter(|c| !c.is_empty());
    if c.is_none() {
        eprintln!("skipped: JODD_AI_E2E_CONTAINER not set");
    }
    c
}

/// Run `jodd-ai-host <args>` inside the container (as root, which is what
/// `lock`/`unlock`/`revoke` need).
fn helper(container: &str, args: &str) -> std::process::Output {
    std::process::Command::new("docker")
        .args(["exec", container, "sh", "-c", &format!("jodd-ai-host {args}")])
        .output()
        .expect("docker exec")
}

fn helper_ok(container: &str, args: &str) -> String {
    let o = helper(container, args);
    assert!(
        o.status.success(),
        "jodd-ai-host {args} failed: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// Setup's own words, copied from `ai_host_commands::setup_message`.
const NO_PASSWORD_SIGN_IN: &str =
    "This server does not accept password sign-in, which setting up an AI host needs once.";
/// `llm::remote::exec::map_failure` for a key the server no longer lists.
const KEY_NO_LONGER_ACCEPTED: &str = "no longer accepts Jodd's key";

/// sshd's reload after `lock`/`unlock` can briefly refuse connections.
async fn fingerprint(h: &str, p: u16) -> crate::backend::ssh::setup::Fingerprint {
    let mut last = String::new();
    for _ in 0..5 {
        match crate::backend::ssh::russh_setup::fetch_fingerprint(h, Some(p)).await {
            Ok(fp) => return fp,
            Err(e) => last = e,
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    panic!("could not read the host key from {h}:{p}: {last}");
}

async fn setup(key_dir: &str, dir: &std::path::Path) -> Result<AiHostSetupResult, String> {
    let (h, p) = target().unwrap();
    let fp = fingerprint(&h, p).await;
    setup_core(&h, Some(p), &user(), &password(), &fp.known_hosts_line, dir, key_dir).await
}

/// Leaves the account unlocked even when an assertion fails mid-test.
struct Unlock(String);
impl Drop for Unlock {
    fn drop(&mut self) {
        // No `expect` here: a panic inside drop while unwinding aborts the process.
        match std::process::Command::new("docker")
            .args(["exec", &self.0, "sh", "-c", "jodd-ai-host unlock"])
            .output()
        {
            Ok(o) if o.status.success() => {}
            Ok(o) => eprintln!("unlock guard: jodd-ai-host unlock failed: {}", String::from_utf8_lossy(&o.stderr)),
            Err(e) => eprintln!("unlock guard: docker exec failed: {e}"),
        }
    }
}

#[tokio::test]
#[ignore]
async fn setup_finds_both_clis_through_the_login_shell() {
    if target().is_none() { return; }
    let key_dir = "abcdef01-0000-4000-8000-000000000001";
    let d = tempfile::tempdir().unwrap();
    let r = setup(key_dir, &d.path().join(key_dir)).await.expect("setup");
    assert!(r.status.current, "{:?}", r.status);
    assert_eq!(r.status.presets.len(), 2, "{:?}", r.status);
    for p in &r.status.presets {
        let path = p.path.as_deref().unwrap_or_else(|| panic!("{} not found: {:?}", p.id, r.status));
        assert!(path.starts_with('/'), "{path}");
    }
}

#[tokio::test]
#[ignore]
async fn the_fingerprint_matches_what_the_helper_prints() {
    if target().is_none() { return; }
    let Some(c) = container() else { return };
    let (h, p) = target().unwrap();
    let fp = fingerprint(&h, p).await;
    let out = helper_ok(&c, "fingerprint");
    assert_eq!(out.trim(), format!("{} {}", fp.algorithm, fp.fingerprint));
}

#[tokio::test]
#[ignore]
async fn lock_makes_setup_fail_with_a_password_message_and_unlock_restores_it() {
    if target().is_none() { return; }
    let Some(c) = container() else { return };
    let key_dir = "abcdef02-0000-4000-8000-000000000002";
    let _unlock = Unlock(c.clone());
    helper_ok(&c, "lock");
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join(key_dir);
    let e = setup(key_dir, &dir).await.unwrap_err();
    // The drop-in sets PasswordAuthentication no AND KbdInteractiveAuthentication
    // no, so only publickey remains: password_offered is false, and that is
    // the text the user guide must map to `jodd-ai-host unlock`.
    assert!(e.contains(NO_PASSWORD_SIGN_IN), "{e}");
    assert!(!e.contains(&password()), "never echo the password: {e}");
    assert!(!dir.exists(), "a failed setup leaves no key behind");
    helper_ok(&c, "unlock");
    let r = setup(key_dir, &dir).await.expect("setup after unlock");
    assert!(r.status.current, "{:?}", r.status);
}

#[tokio::test]
#[ignore]
async fn revoke_removes_exactly_the_named_key() {
    if target().is_none() { return; }
    let Some(c) = container() else { return };
    // Two keys installed; revoking one id must leave the other working.
    let (a, b) = ("abcdef03-0000-4000-8000-000000000003", "abcdef04-0000-4000-8000-000000000004");
    let d = tempfile::tempdir().unwrap();
    let (da, db) = (d.path().join(a), d.path().join(b));
    let ra = setup(a, &da).await.expect("setup a");
    let rb = setup(b, &db).await.expect("setup b");
    let before = helper_ok(&c, "keys");
    assert!(before.contains("jodd-ai-abcdef03") && before.contains("jodd-ai-abcdef04"), "{before}");
    helper_ok(&c, "revoke abcdef03");
    let after = helper_ok(&c, "keys");
    assert!(!after.contains("jodd-ai-abcdef03"), "{after}");
    assert!(after.contains("jodd-ai-abcdef04"), "{after}");
    let exec = |r: &AiHostSetupResult, dir: &std::path::Path| RusshExec {
        target: r.host.target.clone(),
        port: r.host.port,
        key: dir.join("id_ed25519"),
        known_hosts: dir.join("known_hosts"),
        label: "e2e".into(),
    };
    let e = status_core(&exec(&ra, &da)).await.unwrap_err();
    assert!(e.contains(KEY_NO_LONGER_ACCEPTED), "{e}");
    assert!(status_core(&exec(&rb, &db)).await.expect("the other key still works").current);
    // A key that is not there is an error from the helper, not silence.
    assert!(!helper(&c, "revoke abcdef03").status.success());
}
