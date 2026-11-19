//! Spawns a throwaway sshd on a free localhost port, accepting public-key
//! auth as the current user with a dedicated authorized_keys file — no root
//! needed, so this runs in CI. Test-only; shared by `setup.rs`'s end-to-end
//! test and later tasks' Advanced-account tests.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Spawns a throwaway sshd on a random high port, listening only on
/// localhost, accepting public-key auth as the current user with a
/// dedicated authorized_keys file — no root needed, so this runs in CI.
pub(crate) struct TestSshd {
    child: std::process::Child,
    pub port: u16,
    pub authorized_keys: PathBuf,
    pub host_key_line: String,
}
impl Drop for TestSshd {
    fn drop(&mut self) { let _ = self.child.kill(); }
}

pub(crate) fn start_sshd(dir: &Path) -> Option<TestSshd> {
    if Command::new("sshd").arg("-V").output().is_err() { return None; } // not installed here
    // Apple's sshd refuses to daemonize (`-D`) unless invoked with an
    // absolute path in argv[0] — a check independent of the `-V` probe
    // above, which succeeds either way. Resolve one explicitly rather
    // than relying on `Command::new("sshd")`'s PATH-search-with-
    // relative-argv0 behavior, which trips that refusal on macOS. If
    // resolution fails, return None (the same clean skip as "not
    // installed") rather than falling back to a bare "sshd" — that
    // fallback would still spawn *something* on macOS, which then exits
    // immediately without daemonizing, turning a should-skip scenario
    // into a confusing downstream "connection refused" instead of the
    // informative skip message.
    let sshd_bin = Command::new("sh").args(["-c", "command -v sshd"]).output().ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()))
        .filter(|p| !p.as_os_str().is_empty())?;
    let host_key = dir.join("host_key");
    Command::new("ssh-keygen").args(["-t", "ed25519", "-N", "", "-f"]).arg(&host_key).output().ok()?;
    let authorized_keys = dir.join("authorized_keys");
    std::fs::write(&authorized_keys, "").ok()?;
    // The port: ask the OS for a free one, then let it go so sshd can bind
    // it. That hand-off is a race — on Linux a port freed this way is
    // handed straight back to another test's `bind(":0")` (measured
    // 2026-10-08: 58 of 200 trials within 2000 binds), and a sshd that
    // loses it exits. So spawn on a fresh port until one sticks.
    let (child, port) = (0..SSHD_PORT_ATTEMPTS)
        .find_map(|_| {
            let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
            match spawn_on(&sshd_bin, dir, &host_key, &authorized_keys, port) {
                Spawn::Listening(child) => Some((child, port)),
                Spawn::PortTaken => None,
            }
        })
        .unwrap_or_else(|| panic!("sshd lost the bind race on {SSHD_PORT_ATTEMPTS} fresh ports in a row"));
    let host_pub = std::fs::read_to_string(dir.join("host_key.pub")).ok()?;
    let host_key_line = format!("[127.0.0.1]:{port} {}", crate::backend::ssh::scripts::key_body(&host_pub));
    Some(TestSshd { child, port, authorized_keys, host_key_line })
}

const SSHD_PORT_ATTEMPTS: usize = 5;
/// How long a spawned sshd gets to report either outcome. Generous: a
/// cold CI runner can be slow to start it, and both outcomes end the
/// wait early.
const SSHD_STARTUP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

enum Spawn {
    Listening(std::process::Child),
    /// Something else owns the port; try another.
    PortTaken,
}

/// Spawns sshd on `port` and waits until it says which way the bind went.
///
/// sshd's own log is the readiness signal, not a probe connection: a
/// connect proves only that *something* listens on the port — when two
/// tests race for it, that is the other test's sshd, banner and all.
/// Only the line sshd writes after its own `listen()` names the process.
/// `LogLevel INFO` is set explicitly so the line does not depend on a
/// default. Any other early exit panics with the log, rather than
/// surfacing later as an unexplained "connection refused".
fn spawn_on(sshd_bin: &Path, dir: &Path, host_key: &Path, authorized_keys: &Path, port: u16) -> Spawn {
    let config = dir.join(format!("sshd_config.{port}"));
    let log = dir.join(format!("sshd.{port}.log"));
    std::fs::write(&config, format!(
        // `StrictModes no`: `tempfile::tempdir()` lands under `/tmp`,
        // world-writable (mode 1777) on Linux, and OpenSSH's default
        // `StrictModes yes` refuses `authorized_keys` under a directory
        // it considers insecurely permissioned — which a `/tmp`-descended
        // path can be, depending on ownership/mode up the chain. This
        // only ever passed locally because macOS's `$TMPDIR` is a
        // private per-user directory, not shared `/tmp` (I1).
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\nPasswordAuthentication no\nStrictModes no\nPidFile {}\nLogLevel INFO\n",
        host_key.display(), authorized_keys.display(), dir.join("sshd.pid").display(),
    )).expect("write sshd_config");
    // `-E`: log to a file instead of syslog, so the outcome below is readable.
    let mut child = Command::new(sshd_bin).args(["-D", "-f"]).arg(&config).arg("-E").arg(&log)
        .spawn().expect("spawn sshd");
    let deadline = std::time::Instant::now() + SSHD_STARTUP_DEADLINE;
    loop {
        let logged = std::fs::read_to_string(&log).unwrap_or_default();
        if logged.contains("Server listening on") { return Spawn::Listening(child); }
        if let Some(status) = child.try_wait().expect("poll sshd") {
            // Re-read: the line that explains the exit may have landed
            // after the read above.
            let logged = std::fs::read_to_string(&log).unwrap_or_default();
            if logged.contains("Address already in use") { return Spawn::PortTaken; }
            panic!("sshd exited ({status}) before listening on port {port}; its log:\n{logged}");
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("sshd neither listened nor exited within {SSHD_STARTUP_DEADLINE:?}; its log:\n{logged}");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retry trigger, pinned deterministically: a port another sshd
    /// already holds must come back as `PortTaken` — promptly, and not as
    /// `Listening`, which is what a connect-probe readiness check would say.
    #[test]
    fn a_port_lost_to_another_sshd_is_reported_as_taken() {
        let d = tempfile::tempdir().unwrap();
        let Some(first) = start_sshd(d.path()) else {
            eprintln!("sshd not available in this environment — skipping");
            return;
        };
        let sshd_bin = Command::new("sh").args(["-c", "command -v sshd"]).output().unwrap();
        let sshd_bin = PathBuf::from(String::from_utf8_lossy(&sshd_bin.stdout).trim().to_string());
        let second = tempfile::tempdir().unwrap();
        let host_key = second.path().join("host_key");
        Command::new("ssh-keygen").args(["-t", "ed25519", "-N", "", "-f"]).arg(&host_key).output().unwrap();
        let authorized_keys = second.path().join("authorized_keys");
        std::fs::write(&authorized_keys, "").unwrap();
        match spawn_on(&sshd_bin, second.path(), &host_key, &authorized_keys, first.port) {
            Spawn::PortTaken => {}
            Spawn::Listening(mut child) => {
                let _ = child.kill();
                panic!("a second sshd on port {} claimed to be listening", first.port);
            }
        }
    }
}
