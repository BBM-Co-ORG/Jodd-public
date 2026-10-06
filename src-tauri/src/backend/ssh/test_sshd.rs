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
    // The port: the pid-derived port collides when one test binary starts
    // two sshds. Ask the OS for a free one.
    let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
    let config = dir.join("sshd_config");
    std::fs::write(&config, format!(
        // `StrictModes no`: `tempfile::tempdir()` lands under `/tmp`,
        // world-writable (mode 1777) on Linux, and OpenSSH's default
        // `StrictModes yes` refuses `authorized_keys` under a directory
        // it considers insecurely permissioned — which a `/tmp`-descended
        // path can be, depending on ownership/mode up the chain. This
        // only ever passed locally because macOS's `$TMPDIR` is a
        // private per-user directory, not shared `/tmp` (I1).
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\nPasswordAuthentication no\nStrictModes no\nPidFile {}\n",
        host_key.display(), authorized_keys.display(), dir.join("sshd.pid").display(),
    )).ok()?;
    let child = Command::new(&sshd_bin).args(["-D", "-f"]).arg(&config).spawn().ok()?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    let host_pub = std::fs::read_to_string(dir.join("host_key.pub")).ok()?;
    let host_key_line = format!("[127.0.0.1]:{port} {}", crate::backend::ssh::scripts::key_body(&host_pub));
    Some(TestSshd { child, port, authorized_keys, host_key_line })
}
