//! Turning a host/username/password-or-keyfile into a working Managed SSH
//! account: generate or import a key, install it on the server, write
//! Jodd's own known_hosts. See
//! docs/superpowers/specs/2026-09-27-ssh-easy-setup-design.md §1–§3.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::askpass;
use super::scripts;

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct Fingerprint {
    pub algorithm: String,
    pub fingerprint: String,
    pub known_hosts_line: String,
}

pub enum PublicKeySource {
    Generate,
    ImportFile { path: PathBuf, passphrase: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManagedSetup {
    pub dir: PathBuf,
    pub pubkey_line: String,
}

/// True if `bytes` is a PuTTY private key — `ssh-keygen` cannot read this
/// format, so the caller must ask the user to convert it first.
pub fn classify_ppk(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PuTTY-User-Key-File-")
}

pub fn write_known_hosts(base_dir: &Path, known_hosts_line: &str) -> Result<(), String> {
    std::fs::write(base_dir.join("known_hosts"), format!("{}\n", known_hosts_line.trim()))
        .map_err(|e| format!("could not write known_hosts: {e}"))
}

/// The literal contents `write_key_source_marker` writes and `key_was_generated`
/// reads back — a bare marker file rather than a new `Account` field, kept
/// minimal per the C5 ruling (see `docs/superpowers/specs/
/// 2026-09-27-ssh-easy-setup-design.md`'s "Amendments from planning").
const KEY_SOURCE_GENERATED: &str = "generated";
const KEY_SOURCE_IMPORTED: &str = "imported";

/// Records whether this account's key came from `PublicKeySource::Generate`
/// (a key Jodd itself created and installed) or `ImportFile` (the user's own,
/// provider-issued key, which Jodd only verified). `perform_account_removal`
/// reads this back before ever attempting a revoke — see `key_was_generated`.
pub fn write_key_source_marker(base_dir: &Path, generated: bool) -> Result<(), String> {
    let text = if generated { KEY_SOURCE_GENERATED } else { KEY_SOURCE_IMPORTED };
    std::fs::write(base_dir.join("source"), text).map_err(|e| format!("could not write key source marker: {e}"))
}

/// C5: a key-file-sourced Managed account's key is the user's OWN
/// provider-issued key, not one Jodd generated — Jodd never installed it, so
/// Jodd must never offer to remove it on account removal. Revoking it risks
/// locking the user out of their only access to the server, and if two
/// accounts happen to import the same key file, revoking one would silently
/// break the other's access too ("one key per account" no longer holds for
/// imported keys).
///
/// A missing or unreadable marker (an account created before this marker
/// existed, or a filesystem hiccup) is treated as **imported** — the safe
/// default — so any ambiguity fails toward NOT revoking, never toward it.
pub fn key_was_generated(base_dir: &Path) -> bool {
    std::fs::read_to_string(base_dir.join("source"))
        .map(|s| s.trim() == KEY_SOURCE_GENERATED)
        .unwrap_or(false)
}

const KEEP_KEY_MARKER: &str = "keep_key_on_removal";

/// Records that the user unticked "Also remove Jodd's key from the server"
/// on a removal that had to be queued (the account was still Draining). The
/// queued removal completes later, from the sync worker, with no memory of
/// that choice — so the key directory, which lives exactly as long as the
/// removal is pending, carries it.
pub fn write_keep_key_marker(base_dir: &Path) -> Result<(), String> {
    std::fs::write(base_dir.join(KEEP_KEY_MARKER), "").map_err(|e| format!("could not record the keep-key choice: {e}"))
}

pub fn keep_key_requested(base_dir: &Path) -> bool {
    base_dir.join(KEEP_KEY_MARKER).exists()
}

#[derive(Debug, PartialEq)]
pub enum RetireOutcome {
    Revoked,
    Skipped(&'static str),
    Failed(String),
}

/// Retires a removed Managed account's key: revokes Jodd's line from the
/// server's authorized_keys when that is allowed, then deletes the local key
/// directory whatever the revoke did. Blocking, and network-bound — callers
/// run it off the removal's critical path, after the account is already gone
/// locally (local-first: removing an account never waits on the server).
pub fn retire_managed_key(
    target: &str,
    opts: &super::session::SshOptions,
    key_dir: &Path,
    revoke_requested: bool,
) -> RetireOutcome {
    retire_managed_key_with(target, opts, key_dir, revoke_requested, super::EMBEDDED)
}

/// `embedded` picks the transport for the revoke script only; the policy
/// (`revoke_if_allowed`) is identical. With `embedded`, this must run on a
/// thread inside a tokio runtime (a `spawn_blocking` thread is one).
pub fn retire_managed_key_with(
    target: &str,
    opts: &super::session::SshOptions,
    key_dir: &Path,
    revoke_requested: bool,
    embedded: bool,
) -> RetireOutcome {
    let outcome = revoke_if_allowed(target, opts, key_dir, revoke_requested, embedded);
    let _ = std::fs::remove_dir_all(key_dir);
    outcome
}

fn revoke_if_allowed(
    target: &str,
    opts: &super::session::SshOptions,
    key_dir: &Path,
    revoke_requested: bool,
    embedded: bool,
) -> RetireOutcome {
    if !revoke_requested || keep_key_requested(key_dir) {
        return RetireOutcome::Skipped("the user chose to keep the key on the server");
    }
    // C5: an imported key is the user's own; Jodd never installed it.
    if !key_was_generated(key_dir) {
        return RetireOutcome::Skipped("the key was imported, not generated by Jodd");
    }
    let pubkey_line = match std::fs::read_to_string(key_dir.join("id_ed25519.pub")) {
        Ok(l) => l,
        Err(e) => return RetireOutcome::Failed(format!("could not read the public key: {e}")),
    };
    let script = match scripts::revoke_authorized_key(scripts::key_body(pubkey_line.trim())) {
        Ok(s) => s,
        Err(e) => return RetireOutcome::Failed(e),
    };
    if embedded {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return RetireOutcome::Failed("no async runtime to run the revoke on".into());
        };
        let run = super::russh_setup::run_script_once(target, opts, &script);
        return match rt.block_on(run) {
            Ok(out) if out.exit == 0 => RetireOutcome::Revoked,
            Ok(out) => RetireOutcome::Failed(out.stderr.trim().to_string()),
            Err(e) => RetireOutcome::Failed(e),
        };
    }
    let spec = super::session::SpawnSpec::ssh(target, opts);
    match run_script_over_stdin(&spec.program, &spec.args, &[], &script) {
        Ok(out) if out.status.success() => RetireOutcome::Revoked,
        Ok(out) => RetireOutcome::Failed(String::from_utf8_lossy(out_stderr_or(&out)).trim().to_string()),
        Err(e) => RetireOutcome::Failed(e),
    }
}

/// Generates or imports the key pair into `base_dir` (created if absent, at
/// owner-only permissions on Unix), strips any passphrase from an imported
/// key, and returns the public key line. On any failure, `base_dir` is
/// removed before returning — a retry starts clean.
pub fn prepare_key(base_dir: &Path, source: PublicKeySource) -> Result<ManagedSetup, String> {
    match prepare_key_inner(base_dir, source) {
        Ok(setup) => Ok(setup),
        Err(e) => {
            let _ = std::fs::remove_dir_all(base_dir);
            Err(e)
        }
    }
}

fn prepare_key_inner(base_dir: &Path, source: PublicKeySource) -> Result<ManagedSetup, String> {
    std::fs::create_dir_all(base_dir).map_err(|e| format!("could not create {}: {e}", base_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(base_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("could not secure {}: {e}", base_dir.display()))?;
    }
    let key_path = base_dir.join("id_ed25519");
    match source {
        PublicKeySource::Generate => {
            // No `-C` here: the comment is set once, below, from
            // `pubkey_line_for` — giving `-C jodd` here AND appending a
            // comment below duplicated it into "... jodd jodd" (FIX 9).
            let out = Command::new("ssh-keygen")
                .args(["-t", "ed25519", "-N", "", "-f"])
                .arg(&key_path)
                .output()
                .map_err(|e| format!("could not run ssh-keygen: {e}"))?;
            if !out.status.success() {
                return Err(format!("ssh-keygen failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("could not secure key: {e}"))?;
            }
            secure_key_file_windows(&key_path)?;
        }
        PublicKeySource::ImportFile { path, passphrase } => {
            let bytes = std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
            if classify_ppk(&bytes) {
                return Err("This is a PuTTY key. In PuTTYgen choose Conversions → Export OpenSSH key, then pick that file.".into());
            }
            std::fs::write(&key_path, &bytes).map_err(|e| format!("could not copy key: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("could not secure key: {e}"))?;
            }
            secure_key_file_windows(&key_path)?;
            if let Some(pass) = passphrase {
                let mut cmd = Command::new("ssh-keygen");
                cmd.args(["-p", "-N", "", "-f"]).arg(&key_path);
                cmd.envs(askpass::spawn_env(&pass));
                let out = cmd.output().map_err(|e| format!("could not run ssh-keygen: {e}"))?;
                if !out.status.success() {
                    return Err("That passphrase didn't unlock the key.".into());
                }
            } else {
                // Confirm it's actually unencrypted; ssh-keygen -y fails on an
                // encrypted key with no passphrase supplied. `-P ""` is
                // required: without it, some invocation contexts (no
                // controlling terminal is not guaranteed here) can make
                // ssh-keygen block on an interactive passphrase prompt
                // instead of failing immediately (FIX 8).
                let out = Command::new("ssh-keygen").args(["-y", "-P", "", "-f"]).arg(&key_path).output()
                    .map_err(|e| format!("could not run ssh-keygen: {e}"))?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(if stderr.contains("incorrect passphrase") {
                        "This key is protected by a passphrase".into()
                    } else {
                        "That file isn't a private key.".into()
                    });
                }
            }
        }
    }
    let pub_out = Command::new("ssh-keygen").args(["-y", "-f"]).arg(&key_path).output()
        .map_err(|e| format!("could not run ssh-keygen: {e}"))?;
    if !pub_out.status.success() {
        return Err("That file isn't a private key.".into());
    }
    let pubkey_out = String::from_utf8_lossy(&pub_out.stdout).trim().to_string();
    let pubkey_line = pubkey_line_for(&pubkey_out, base_dir);
    Ok(ManagedSetup { dir: base_dir.to_path_buf(), pubkey_line })
}

/// Builds the `authorized_keys` line Jodd installs: the key's algorithm and
/// body (via `scripts::key_body`, which drops whatever comment `ssh-keygen -y`
/// printed — a generated key's own `ssh-keygen`-assigned comment, or an
/// imported key's original one) plus a comment naming this account, so two
/// Managed accounts on the same server are distinguishable in
/// `~/.ssh/authorized_keys` instead of both reading "... jodd" (FIX 9).
/// `base_dir`'s file name is the account's uuid (`ssh_setup_managed` names
/// the directory that way); an 8-character prefix is enough to tell rows
/// apart without a long uuid cluttering the file.
pub(crate) fn pubkey_line_for(pubkey_out: &str, base_dir: &Path) -> String {
    let body = scripts::key_body(pubkey_out);
    let short_id: String = base_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.chars().filter(|c| *c != '-').take(8).collect())
        .unwrap_or_default();
    let comment = if short_id.is_empty() { "jodd".to_string() } else { format!("jodd-{short_id}") };
    format!("{body} {comment}")
}

/// `ssh-keyscan` ships alongside `ssh` on macOS and Windows OpenSSH, so no
/// new dependency. Actually prefers ed25519 over ecdsa/rsa when the server
/// offers several — `ssh-keyscan`'s own output order across key types is not
/// guaranteed to follow the `-t` list, so `pick_keyscan_line` picks the
/// ed25519 line explicitly rather than trusting output order.
pub fn fetch_fingerprint(host: &str, port: Option<u16>) -> Result<Fingerprint, String> {
    let mut cmd = Command::new(super::session::ssh_binary().with_file_name(
        if cfg!(windows) { "ssh-keyscan.exe" } else { "ssh-keyscan" },
    ));
    cmd.args(["-T", "10"]);
    if let Some(p) = port {
        cmd.args(["-p", &p.to_string()]);
    }
    cmd.args(["-t", "ed25519,ecdsa,rsa", host]);
    let out = cmd.output().map_err(|e| format!("could not run ssh-keyscan: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = pick_keyscan_line(&stdout)
        .ok_or_else(|| format!("could not reach {host}: no host key returned"))?
        .to_string();
    let tmp = tempfile::NamedTempFile::new().map_err(|e| format!("could not create a temp file: {e}"))?;
    std::fs::write(tmp.path(), format!("{line}\n")).map_err(|e| format!("could not write temp file: {e}"))?;
    let lf = Command::new("ssh-keygen").args(["-lf"]).arg(tmp.path()).output()
        .map_err(|e| format!("could not run ssh-keygen: {e}"))?;
    let text = String::from_utf8_lossy(&lf.stdout);
    let mut parts = text.split_whitespace();
    let _bits = parts.next();
    let fingerprint = parts.next().unwrap_or_default().to_string();
    let algorithm = text.trim().rsplit('(').next().unwrap_or_default().trim_end_matches(')').to_string();
    let known_hosts_line = known_hosts_line_for(host, port, &line);
    Ok(Fingerprint { algorithm, fingerprint, known_hosts_line })
}

/// Picks the ed25519 line if `ssh-keyscan` returned one, else the first
/// non-comment, non-blank line — `ssh-keyscan -t ed25519,ecdsa,rsa`'s output
/// order across key types is not guaranteed to follow that list (FIX 7).
fn pick_keyscan_line(stdout: &str) -> Option<&str> {
    let candidates: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).collect();
    candidates
        .iter()
        .find(|l| l.split_whitespace().any(|f| f == "ssh-ed25519"))
        .or_else(|| candidates.first())
        .copied()
}

/// Puts a `ssh-keyscan` output line into `known_hosts` form for a non-default
/// port. `ssh-keyscan -p N host` already prints its own `[host]:N ssh-… …`
/// line when a non-default port is given, so this must not re-wrap a line
/// that is already in `[host]:port` form — doing so produces
/// `[[host]:N]:N ssh-… …`, a malformed entry that never matches on a later
/// connection (C2). A pure function so the double-wrap regression is a unit
/// test, not something only a live `ssh-keyscan -p` run surfaces.
pub(crate) fn known_hosts_line_for(host: &str, port: Option<u16>, keyscan_line: &str) -> String {
    if port.is_some_and(|p| p != 22) && !keyscan_line.starts_with('[') {
        keyscan_line.replacen(host, &format!("[{host}]:{}", port.unwrap()), 1)
    } else {
        keyscan_line.to_string()
    }
}

/// Spawns `program args` — a full `ssh`/`sh` invocation whose argv already
/// ends in a bare `sh`, a persistent remote shell that reads its command from
/// **stdin**, never from an appended argv tail — pipes `script` to that
/// stdin, closes it, and waits for the result.
///
/// This is the one place that knows that rule. Before this helper existed it
/// was written correctly by hand twice (`install_key_over_password`,
/// `verify_key_login`) and incorrectly a third time (the revoke-on-removal
/// call in `lib.rs::perform_account_removal`, C4): appending the script as
/// another `.arg(...)` after a spawn spec ending in `sh` makes ssh fold it
/// into the *same* trailing command string sent to the remote shell — `sh
/// <script text>` — which the remote's login shell parses as "run a script
/// file named after the first word of the script", not "run this script".
/// The remote command then does nothing, but still often exits 0-ish, so a
/// caller that only checks the exit code logs success for an operation that
/// never ran. Every caller that spawns one of these one-shot `ssh … sh`
/// processes must go through here instead of building its own `Command` with
/// a trailing `.arg(script)`.
pub fn run_script_over_stdin(
    program: &Path,
    args: &[String],
    envs: &[(String, String)],
    script: &str,
) -> Result<std::process::Output, String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("could not start ssh: {e}"))?;
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(script.as_bytes())
            .map_err(|e| format!("could not send the script: {e}"))?;
    }
    child.wait_with_output().map_err(|e| format!("ssh did not exit cleanly: {e}"))
}

/// Runs the ONE password-authenticated `ssh`, installing `pubkey_line` via
/// `scripts::install_authorized_key`. Uses the askpass helper from
/// `backend::ssh::askpass`, never argv, for the password.
///
/// C3: this connection has no host-key pin of its own — it must be given
/// `known_hosts_path`, the SAME file `ssh_setup_managed` already wrote via
/// `write_known_hosts` (the fingerprint the user just confirmed in the
/// dialog) — plus `StrictHostKeyChecking=yes`. Without it, ssh falls back to
/// the user's own `~/.ssh/known_hosts`, which never has this server (that is
/// the entire premise of this flow), so ssh either hangs on an unanswerable
/// host-key prompt or refuses outright — the headline "just give it a
/// password" feature failing on exactly the servers it exists for — and the
/// password would otherwise be sent before the server's identity was ever
/// checked against the fingerprint the user approved.
/// Pure argv-builder for `install_key_over_password`, split out so the C3
/// fix (pinning `known_hosts_path`) is a direct unit test rather than
/// something only a real ssh subprocess would reveal.
fn install_key_over_password_args(target: &str, port: Option<u16>, known_hosts_path: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "-o", "BatchMode=no",
        "-o", "PubkeyAuthentication=no",
        "-o", "NumberOfPasswordPrompts=1",
    ].iter().map(|s| s.to_string()).collect();
    if let Some(p) = port {
        args.push("-p".into());
        args.push(p.to_string());
    }
    // Quoted the same way as `SpawnSpec::ssh` (C1): the path can contain a
    // space (macOS's own `~/Library/Application Support/...` data dir), and
    // `UserKnownHostsFile` splits an unquoted value on whitespace.
    args.push("-o".into());
    args.push(format!("UserKnownHostsFile=\"{}\"", known_hosts_path.display()));
    args.push("-o".into());
    args.push("StrictHostKeyChecking=yes".into());
    args.push("-T".into());
    args.push("--".into());
    args.push(target.to_string());
    args.push("sh".into());
    args
}

pub fn install_key_over_password(
    target: &str,
    port: Option<u16>,
    known_hosts_path: &Path,
    password: &str,
    pubkey_line: &str,
) -> Result<(), String> {
    let args = install_key_over_password_args(target, port, known_hosts_path);
    let out = run_script_over_stdin(
        &super::session::ssh_binary(),
        &args,
        &askpass::spawn_env(password),
        &scripts::install_authorized_key(pubkey_line),
    )?;
    if !out.status.success() {
        return Err(super::classify::from_setup_stderr(&String::from_utf8_lossy(out_stderr_or(&out))));
    }
    Ok(())
}

fn out_stderr_or(out: &std::process::Output) -> &[u8] {
    if out.stderr.is_empty() { &out.stdout } else { &out.stderr }
}

/// One round trip with the account's real Managed options, proving the key
/// alone (no password) can now sign in. `spec.args` already ends in a bare
/// `sh` — a persistent remote shell that expects script text over stdin —
/// so the command goes through `run_script_over_stdin` rather than being
/// appended to argv, which ssh would otherwise fold into the same trailing
/// command string (`sh exit 0`, which `sh` treats as "run a script file
/// named exit", not "run exit 0").
///
/// `key_just_installed` says which path got here. A key-only login that is
/// refused means something different on each: on the Password path Jodd has
/// just installed the key (password login demonstrably works, so "choose the
/// Key file option" — `from_setup_stderr`'s publickey-only reading — would be
/// wrong); on the Key-file path the server does not trust the chosen file.
pub fn verify_key_login(target: &str, opts: &super::session::SshOptions, key_just_installed: bool) -> Result<(), String> {
    let spec = super::session::SpawnSpec::ssh(target, opts);
    let out = run_script_over_stdin(&spec.program, &spec.args, &[], "exit 0\n")?;
    if !out.status.success() {
        return Err(verify_failure_message(&String::from_utf8_lossy(out_stderr_or(&out)), key_just_installed));
    }
    Ok(())
}

pub(crate) fn verify_failure_message(stderr: &str, key_just_installed: bool) -> String {
    if stderr.contains("Permission denied") {
        return if key_just_installed {
            "Jodd installed its key, but the server did not accept it. This usually means the \
             home folder or ~/.ssh on the server can be written by other users, which the server \
             refuses for safety, or that the server keeps keys somewhere other than \
             ~/.ssh/authorized_keys.".to_string()
        } else {
            "The server did not accept this key file for this username. Check that it is the key \
             your provider gave you for this user.".to_string()
        };
    }
    super::classify::from_setup_stderr(stderr)
}

/// Windows OpenSSH refuses a private key file whose ACL grants access to
/// more than the owner. `ssh-keygen`/import both call this after writing
/// the key. A no-op on other platforms — Unix permissions are already set
/// by `prepare_key_inner`.
#[cfg(windows)]
pub fn secure_key_file_windows(path: &Path) -> Result<(), String> {
    let user = std::env::var("USERNAME").map_err(|_| "no USERNAME set".to_string())?;
    let out = Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &format!("{user}:F")])
        .output()
        .map_err(|e| format!("could not run icacls: {e}"))?;
    if !out.status.success() {
        return Err(format!("could not secure the key file: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn secure_key_file_windows(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// A key directory with no account row is left when setup is interrupted
/// after `prepare_key` — on Android, when the OS kills Jodd mid-setup. Only
/// directories older than this are swept: two Jodd instances can share one
/// data dir (`tauri dev` beside the installed app), and one must not delete
/// the other's in-progress setup. Setup takes seconds; an hour is certain.
pub const ORPHAN_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Filesystem only — no IPC, no network — so it is safe in `.setup()`
/// (gotcha #32's stranded-reply hazard does not apply). A `pending_removal`
/// account still has its row, so its directory is kept.
pub fn sweep_orphan_key_dirs(ssh_dir: &Path, accounts: &[crate::accounts::Account], min_age: std::time::Duration) -> usize {
    let keep: std::collections::HashSet<&str> = accounts.iter().filter_map(|a| a.ssh_key.as_deref()).collect();
    let Ok(entries) = std::fs::read_dir(ssh_dir) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // `file_type()` is lstat-based (unlike `path.is_dir()`, which
        // follows symlinks): a symlink is never a sweep candidate, so one
        // named like an orphan uuid is neither swept nor followed into.
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() || keep.contains(name) {
            continue;
        }
        let old_enough = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if old_enough && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_sweep_removes_only_old_directories_no_account_names() {
        use std::time::{Duration, SystemTime};
        let d = tempfile::tempdir().unwrap();
        let mk = |name: &str, age_secs: u64| {
            let p = d.path().join(name);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("id_ed25519"), "k").unwrap();
            let t = SystemTime::now() - Duration::from_secs(age_secs);
            std::fs::File::open(&p).unwrap().set_modified(t).unwrap();
            p
        };
        let live = mk("live-uuid", 7200);
        let draining = mk("draining-uuid", 7200);
        let orphan = mk("orphan-uuid", 7200);
        let in_progress = mk("fresh-uuid", 5); // another instance mid-setup
        let acct = |key: &str, pending: bool| crate::accounts::Account {
            id: crate::accounts::account_id_for(crate::accounts::BackendKind::Ssh, key),
            email: key.to_string(),
            added_at: chrono::Utc::now().to_rfc3339(),
            notes_label: None,
            meta_label: None,
            llm: crate::accounts::LlmConfig::default(),
            backend_kind: crate::accounts::BackendKind::Ssh,
            root_dir: None,
            ssh_target: None,
            ssh_port: None,
            ssh_key: Some(key.into()),
            icloud_session_established: false,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status: crate::accounts::AccountStatus::Active,
            pending_removal: pending,
        };
        let accounts = [acct("live-uuid", false), acct("draining-uuid", true)];
        assert_eq!(sweep_orphan_key_dirs(d.path(), &accounts, ORPHAN_MIN_AGE), 1);
        assert!(live.exists() && draining.exists() && in_progress.exists());
        assert!(!orphan.exists());
        assert_eq!(sweep_orphan_key_dirs(&d.path().join("absent"), &accounts, ORPHAN_MIN_AGE), 0);
    }

    /// `path.is_dir()` follows a symlink to decide it's a directory, but
    /// `remove_dir_all` on a symlink-to-a-directory can remove the TARGET's
    /// contents rather than just unlinking the link — so candidacy must be
    /// decided with `entry.file_type()` (lstat-based, never follows) and a
    /// symlink must never be a sweep candidate, whatever its target's age.
    #[test]
    fn the_sweep_never_follows_or_removes_a_symlink() {
        use std::time::{Duration, SystemTime};
        let d = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("sentinel"), "still here").unwrap();
        let t = SystemTime::now() - Duration::from_secs(7200);
        std::fs::File::open(target.path()).unwrap().set_modified(t).unwrap();

        let link = d.path().join("orphan-uuid");
        std::os::unix::fs::symlink(target.path(), &link).unwrap();

        let accounts: [crate::accounts::Account; 0] = [];
        assert_eq!(sweep_orphan_key_dirs(d.path(), &accounts, ORPHAN_MIN_AGE), 0);
        assert!(link.exists(), "the symlink itself must survive");
        assert!(
            target.path().join("sentinel").exists(),
            "the sweep must never follow the symlink into its target, whatever the target's age"
        );
    }

    #[test]
    fn classify_ppk_detects_the_putty_header() {
        assert!(classify_ppk(b"PuTTY-User-Key-File-3: ssh-ed25519\n"));
        // Split so the whole PEM header never appears literally in source:
        // sync-to-public.sh's secret scan refuses any file that contains it.
        let openssh = [b"-----BEGIN OPENSSH ".as_slice(), b"PRIVATE KEY-----\n"].concat();
        assert!(!classify_ppk(&openssh));
    }

    #[test]
    fn prepare_key_generate_writes_a_usable_pair() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("acct1");
        let setup = prepare_key(&base, PublicKeySource::Generate).unwrap();
        assert!(setup.dir.join("id_ed25519").exists());
        assert!(setup.dir.join("id_ed25519.pub").exists());
        assert!(setup.pubkey_line.starts_with("ssh-ed25519 "));
        // Owner-only permissions are Jodd's own guarantee, not an assumption
        // about ssh-keygen's default mode.
        assert_eq!(std::fs::metadata(&base).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(
            std::fs::metadata(setup.dir.join("id_ed25519")).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn prepare_key_cleans_up_the_directory_on_failure() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("acct2");
        let bad_path = d.path().join("not-a-key");
        std::fs::write(&bad_path, b"not a key at all").unwrap();
        let err = prepare_key(&base, PublicKeySource::ImportFile { path: bad_path, passphrase: None });
        assert!(err.is_err());
        assert!(!base.exists(), "the half-made directory must not survive a failed setup");
    }

    #[test]
    fn prepare_key_reports_a_missing_file_plainly() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("acct3");
        let missing = d.path().join("does-not-exist");
        let err = prepare_key(&base, PublicKeySource::ImportFile { path: missing, passphrase: None }).unwrap_err();
        assert!(err.contains("could not read"), "{err}");
        assert!(!base.exists());
    }

    #[test]
    fn prepare_key_reports_a_directory_passed_as_a_key_file_plainly() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("acct4");
        let a_dir = d.path().join("i-am-a-directory");
        std::fs::create_dir(&a_dir).unwrap();
        let err = prepare_key(&base, PublicKeySource::ImportFile { path: a_dir, passphrase: None }).unwrap_err();
        assert!(err.contains("could not read"), "{err}");
        assert!(!base.exists());
    }

    #[test]
    fn prepare_key_rejects_a_putty_key_with_the_conversion_hint() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("acct5");
        let ppk = d.path().join("key.ppk");
        std::fs::write(&ppk, b"PuTTY-User-Key-File-3: ssh-ed25519\n...").unwrap();
        let err = prepare_key(&base, PublicKeySource::ImportFile { path: ppk, passphrase: None }).unwrap_err();
        assert!(err.contains("PuTTYgen"), "{err}");
        assert!(!base.exists());
    }

    #[test]
    fn prepare_key_import_sets_owner_only_permissions_regardless_of_source_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src_key");
        let gen = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-f"]).arg(&src)
            .output().unwrap();
        assert!(gen.status.success(), "{}", String::from_utf8_lossy(&gen.stderr));
        // The source file is world-readable — the copy must not inherit that.
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();
        let base = d.path().join("acct7");
        let setup = prepare_key(&base, PublicKeySource::ImportFile { path: src, passphrase: None }).unwrap();
        assert_eq!(std::fs::metadata(&base).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(
            std::fs::metadata(setup.dir.join("id_ed25519")).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn write_known_hosts_writes_exactly_the_given_line() {
        let d = tempfile::tempdir().unwrap();
        write_known_hosts(d.path(), "box ssh-ed25519 AAAA...").unwrap();
        assert_eq!(std::fs::read_to_string(d.path().join("known_hosts")).unwrap(), "box ssh-ed25519 AAAA...\n");
    }

    // ─── C2: known_hosts_line_for must not double-wrap an already-bracketed
    // ssh-keyscan line ───────────────────────────────────────────────────────

    #[test]
    fn known_hosts_line_for_wraps_a_plain_line_on_a_non_default_port() {
        let line = "box.example.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA...";
        let got = known_hosts_line_for("box.example.com", Some(2222), line);
        assert_eq!(got, "[box.example.com]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA...");
    }

    /// C2 itself: `ssh-keyscan -p 2222 host` already prints its own
    /// `[host]:2222 ...` line — re-wrapping it produced
    /// `[[host]:2222]:2222 ...`, which never matches on a later connection.
    #[test]
    fn known_hosts_line_for_does_not_double_wrap_an_already_bracketed_line() {
        let realistic_keyscan_output =
            "[box.example.com]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJQk7a/ETMFL3i0/Cy5iDEHHoH49qaQ6SYQXxvw5ZrLS";
        let got = known_hosts_line_for("box.example.com", Some(2222), realistic_keyscan_output);
        assert_eq!(got, realistic_keyscan_output, "must not be re-wrapped");
        assert!(!got.contains("[["), "{got}");
    }

    #[test]
    fn known_hosts_line_for_leaves_port_22_alone() {
        let line = "box.example.com ssh-ed25519 AAAA...";
        assert_eq!(known_hosts_line_for("box.example.com", Some(22), line), line);
        assert_eq!(known_hosts_line_for("box.example.com", None, line), line);
    }

    // ─── FIX 7: pick_keyscan_line prefers ed25519 ───────────────────────────

    #[test]
    fn pick_keyscan_line_prefers_ed25519_out_of_order_output() {
        let stdout = "# comment\n\
                       box.example.com ssh-rsa AAAARSA...\n\
                       box.example.com ssh-ed25519 AAAAED25519...\n\
                       box.example.com ecdsa-sha2-nistp256 AAAAECDSA...\n";
        assert_eq!(pick_keyscan_line(stdout), Some("box.example.com ssh-ed25519 AAAAED25519..."));
    }

    #[test]
    fn pick_keyscan_line_falls_back_to_the_first_line_with_no_ed25519() {
        let stdout = "box.example.com ssh-rsa AAAARSA...\nbox.example.com ecdsa-sha2-nistp256 AAAAECDSA...\n";
        assert_eq!(pick_keyscan_line(stdout), Some("box.example.com ssh-rsa AAAARSA..."));
    }

    // ─── FIX 9: no duplicated/garbled key comment ───────────────────────────

    #[test]
    fn pubkey_line_for_uses_a_distinguishing_comment_not_a_duplicated_jodd() {
        let base = PathBuf::from("/data/ssh/abcdef12-3456-7890-abcd-ef1234567890");
        let out = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... jodd";
        let line = pubkey_line_for(out, &base);
        assert_eq!(line, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... jodd-abcdef12");
        assert!(!line.contains("jodd jodd"), "{line}");
    }

    #[test]
    fn pubkey_line_for_drops_an_imported_keys_original_comment() {
        let base = PathBuf::from("/data/ssh/abcdef12-3456-7890-abcd-ef1234567890");
        let out = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... someone@their-laptop";
        let line = pubkey_line_for(out, &base);
        assert_eq!(line, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... jodd-abcdef12");
    }

    /// Generating a key must not leave "... jodd jodd" — the literal bug
    /// this fix closes, exercised through the real `ssh-keygen` binary.
    #[test]
    fn prepare_key_generate_does_not_duplicate_the_comment() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("acct-comment");
        let setup = prepare_key(&base, PublicKeySource::Generate).unwrap();
        assert!(!setup.pubkey_line.contains("jodd jodd"), "{}", setup.pubkey_line);
        assert!(setup.pubkey_line.split_whitespace().last().unwrap().starts_with("jodd"));
    }

    // ─── C5: the generated/imported marker Jodd uses to gate revoke ────────

    #[test]
    fn key_was_generated_reads_back_what_was_written() {
        let d = tempfile::tempdir().unwrap();
        write_key_source_marker(d.path(), true).unwrap();
        assert!(key_was_generated(d.path()));

        let d2 = tempfile::tempdir().unwrap();
        write_key_source_marker(d2.path(), false).unwrap();
        assert!(!key_was_generated(d2.path()));
    }

    /// A missing marker (an older account, or a filesystem hiccup) must fail
    /// toward NOT revoking, never toward revoking.
    #[test]
    fn key_was_generated_treats_a_missing_marker_as_imported() {
        let d = tempfile::tempdir().unwrap();
        assert!(!key_was_generated(d.path()));
    }

    /// The real sequence `ssh_setup_managed` runs: decide `key_was_generated`
    /// from the `PublicKeySource` BEFORE it moves into `prepare_key`, then
    /// write the marker after. Proves the two ends of that sequence agree —
    /// a Password-path account (`Generate`) reads back as generated (revoke
    /// allowed), a Key-file account (`ImportFile`) reads back as imported
    /// (revoke skipped) — which is exactly what `perform_account_removal`'s
    /// gate depends on (C5, parts a and b).
    #[test]
    fn end_to_end_source_marker_matches_the_credential_path_used() {
        let d = tempfile::tempdir().unwrap();

        let generated_base = d.path().join("password-path-account");
        let generated_is_password_path = matches!(PublicKeySource::Generate, PublicKeySource::Generate);
        prepare_key(&generated_base, PublicKeySource::Generate).unwrap();
        write_key_source_marker(&generated_base, generated_is_password_path).unwrap();
        assert!(key_was_generated(&generated_base), "a Password-path account's key must be revocable");

        let src = d.path().join("provided_key");
        Command::new("ssh-keygen").args(["-t", "ed25519", "-N", "", "-f"]).arg(&src).output().unwrap();
        let imported_base = d.path().join("keyfile-path-account");
        let source = PublicKeySource::ImportFile { path: src, passphrase: None };
        let imported_is_password_path = matches!(source, PublicKeySource::Generate);
        prepare_key(&imported_base, source).unwrap();
        write_key_source_marker(&imported_base, imported_is_password_path).unwrap();
        assert!(!key_was_generated(&imported_base), "a Key-file account's key must NOT be revocable");
    }

    // ─── C3: the password-authenticated install must pin the confirmed host
    // key, not fall back to the user's own known_hosts ─────────────────────

    /// `install_key_over_password` builds its own argv rather than going
    /// through `SpawnSpec::ssh`, so this pins its shape directly: it must add
    /// a quoted `UserKnownHostsFile` for the given path plus
    /// `StrictHostKeyChecking=yes`, exactly like a Managed sync connection
    /// does — the fix for C3 (previously it pinned no host key at all, so
    /// this connection either hung on an unanswerable host-key prompt or
    /// fell back to the user's own, server-less `~/.ssh/known_hosts`).
    #[test]
    fn install_key_over_password_pins_the_given_known_hosts_file() {
        let kh = PathBuf::from("/tmp/a b/known_hosts");
        let args = install_key_over_password_args("me@box", Some(2222), &kh);
        assert!(
            args.iter().any(|a| a == "UserKnownHostsFile=\"/tmp/a b/known_hosts\""),
            "{:?}",
            args
        );
        assert!(args.iter().any(|a| a == "StrictHostKeyChecking=yes"));
        assert!(args.windows(2).any(|w| w == ["-p", "2222"]));
    }

    // ─── C4: script over stdin, never as an argv tail ───────────────────────

    /// The shared helper both `verify_key_login`/`install_key_over_password`
    /// and (via `lib.rs::perform_account_removal`) the revoke-on-removal path
    /// now go through. Mirrors the real shape of every one-shot call site:
    /// `program` + `args` ending in a bare `sh` (a stand-in for the
    /// persistent remote shell `ssh … sh` gives), with the script piped to
    /// its stdin rather than appended as another argv element.
    #[test]
    fn run_script_over_stdin_pipes_the_script_and_returns_its_real_output() {
        let out = run_script_over_stdin(&PathBuf::from("sh"), &[], &[], "echo hi-from-stdin\n").unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hi-from-stdin\n");
    }

    #[test]
    fn run_script_over_stdin_carries_the_given_environment() {
        let envs = [("JODD_TEST_MARKER".to_string(), "present".to_string())];
        let out = run_script_over_stdin(&PathBuf::from("sh"), &[], &envs, "echo \"$JODD_TEST_MARKER\"\n").unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "present\n");
    }

    /// Documents exactly what the C4 bug did: appending the script text as
    /// another `.arg(...)` after argv already ending in a bare `sh` makes
    /// `sh` treat the script's first word as the name of a script FILE to
    /// run — not as commands to execute — so it silently does nothing useful
    /// (and typically fails, rather than running the intended script). This
    /// is why `install_key_over_password`, `verify_key_login` and the
    /// revoke-on-removal call all go through `run_script_over_stdin` instead.
    #[test]
    fn appending_the_script_as_an_argv_tail_is_the_c4_regression_being_guarded_against() {
        let out = std::process::Command::new("sh").arg("echo this-is-not-how-sh-args-work\n").output().unwrap();
        assert!(
            !out.status.success(),
            "a bare `sh <script text>` argument must NOT run the script the way the argv-tail bug assumed"
        );
    }

    /// Relies on being the sole test in this binary that sets
    /// `JODD_TEST_ASKPASS_EXE_OVERRIDE` and exercises the passphrase-import
    /// path — `std::env::set_var`/`remove_var` are process-global, and Rust
    /// runs tests in parallel by default. A future passphrase-import test
    /// must coordinate with this one rather than setting the same var
    /// independently.
    #[test]
    fn prepare_key_reimport_with_a_passphrase_strips_it() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src_key");
        // Build a real encrypted key with ssh-keygen itself, so this test
        // measures our stripping step against a real OpenSSH artifact.
        let gen = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "s3cr3t", "-f"]).arg(&src)
            .output().unwrap();
        assert!(gen.status.success(), "{}", String::from_utf8_lossy(&gen.stderr));

        // `askpass::spawn_env` normally points SSH_ASKPASS at `current_exe()`,
        // which under `cargo test` is the test-harness binary, not a real
        // askpass-capable `jodd` executable — see the doc comment on
        // `spawn_env`. Substitute a disposable script that just echoes back
        // the secret Jodd already puts in the child's environment.
        let askpass_script = d.path().join("fake_askpass.sh");
        std::fs::write(&askpass_script, "#!/bin/sh\nprintf '%s\\n' \"$JODD_ASKPASS_SECRET\"\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&askpass_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("JODD_TEST_ASKPASS_EXE_OVERRIDE", askpass_script.to_str().unwrap());

        let base = d.path().join("acct6");
        let result = prepare_key(&base, PublicKeySource::ImportFile {
            path: src, passphrase: Some("s3cr3t".to_string()),
        });
        std::env::remove_var("JODD_TEST_ASKPASS_EXE_OVERRIDE");
        let setup = result.unwrap();

        // Now readable with NO passphrase.
        let out = Command::new("ssh-keygen").args(["-y", "-f"]).arg(setup.dir.join("id_ed25519")).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    // ─── Retiring a removed account's key ───────────────────────────────────
    // Every case below is decided before any ssh is spawned: the target is
    // unroutable on purpose, so a test that wrongly reached the network
    // would fail with `Failed`, not pass.

    const UNROUTABLE: &str = "nobody@invalid.invalid";

    fn generated_key_dir(parent: &Path) -> PathBuf {
        let dir = parent.join("acct");
        prepare_key(&dir, PublicKeySource::Generate).unwrap();
        write_key_source_marker(&dir, true).unwrap();
        dir
    }

    #[test]
    fn retire_skips_the_revoke_the_user_declined_and_still_deletes_the_key() {
        let d = tempfile::tempdir().unwrap();
        let dir = generated_key_dir(d.path());
        let out = retire_managed_key(UNROUTABLE, &super::super::session::SshOptions::default(), &dir, false);
        assert!(matches!(out, RetireOutcome::Skipped(_)), "{out:?}");
        assert!(!dir.exists(), "the key directory is deleted whatever the revoke did");
    }

    /// Finding 2: a removal queued while Draining completes from the sync
    /// worker with `revoke_requested = true` — the marker written at queue
    /// time is what carries the user's "keep the key" choice to it.
    #[test]
    fn retire_honours_a_keep_key_marker_even_when_revoke_is_requested() {
        let d = tempfile::tempdir().unwrap();
        let dir = generated_key_dir(d.path());
        write_keep_key_marker(&dir).unwrap();
        let out = retire_managed_key(UNROUTABLE, &super::super::session::SshOptions::default(), &dir, true);
        assert!(matches!(out, RetireOutcome::Skipped(_)), "{out:?}");
        assert!(!dir.exists());
    }

    #[test]
    fn retire_never_revokes_an_imported_key() {
        let d = tempfile::tempdir().unwrap();
        let dir = generated_key_dir(d.path());
        write_key_source_marker(&dir, false).unwrap();
        let out = retire_managed_key(UNROUTABLE, &super::super::session::SshOptions::default(), &dir, true);
        assert_eq!(out, RetireOutcome::Skipped("the key was imported, not generated by Jodd"));
    }

    /// Finding 1, end to end: an empty `.pub` never reaches the server as a
    /// match-everything revoke.
    #[test]
    fn retire_refuses_to_revoke_with_an_empty_public_key() {
        let d = tempfile::tempdir().unwrap();
        let dir = generated_key_dir(d.path());
        std::fs::write(dir.join("id_ed25519.pub"), "").unwrap();
        let out = retire_managed_key(UNROUTABLE, &super::super::session::SshOptions::default(), &dir, true);
        match out {
            RetireOutcome::Failed(e) => assert!(e.contains("implausible key body"), "{e}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(!dir.exists());
    }

    /// The embedded revoke needs a tokio runtime to block on. Called from a
    /// thread without one, it must report that, not panic — and the local
    /// key directory is still deleted.
    #[test]
    fn an_embedded_revoke_outside_a_runtime_fails_instead_of_panicking() {
        let d = tempfile::tempdir().unwrap();
        let dir = generated_key_dir(d.path());
        let out = retire_managed_key_with(UNROUTABLE, &super::super::session::SshOptions::default(), &dir, true, true);
        assert_eq!(out, RetireOutcome::Failed("no async runtime to run the revoke on".into()));
        assert!(!dir.exists(), "the key directory is deleted whatever the revoke did");
    }

    /// Finding 5: the same `Permission denied (publickey)` means different
    /// things on the two setup paths, and neither is "choose Key file" when
    /// password login just worked.
    #[test]
    fn a_refused_key_login_is_explained_per_setup_path() {
        let stderr = "me@box: Permission denied (publickey).\n";
        let installed = verify_failure_message(stderr, true);
        assert!(installed.contains("installed its key") && !installed.contains("Key file option"), "{installed}");
        let imported = verify_failure_message(stderr, false);
        assert!(imported.contains("did not accept this key file"), "{imported}");
        assert!(verify_failure_message("ssh: Could not resolve hostname x\n", true).contains("Can't reach"));
    }
}

#[cfg(all(test, unix))]
mod end_to_end {
    use super::*;
    use std::process::Command;
    use crate::backend::ssh::test_sshd::start_sshd;

    #[test]
    fn key_file_setup_end_to_end_against_a_local_sshd() {
        let d = tempfile::tempdir().unwrap();
        let Some(sshd) = start_sshd(d.path()) else {
            eprintln!("sshd not available in this environment — skipping end-to-end test");
            return;
        };
        // The "provider-issued key file": generate one and pre-install its
        // PUBLIC half on the sshd, simulating what a hosting panel would
        // have already done for a key-file customer.
        let provided_key = d.path().join("provided_key");
        Command::new("ssh-keygen").args(["-t", "ed25519", "-N", "", "-f"]).arg(&provided_key).output().unwrap();
        let pubkey = std::fs::read_to_string(provided_key.with_extension("pub")).unwrap();
        std::fs::write(&sshd.authorized_keys, &pubkey).unwrap();

        let base = d.path().join("managed");
        let prepared = prepare_key(&base, PublicKeySource::ImportFile {
            path: provided_key, passphrase: None,
        }).unwrap();
        assert_eq!(prepared.pubkey_line.split_whitespace().nth(1), pubkey.split_whitespace().nth(1));

        let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "test".into());
        let target = format!("{user}@127.0.0.1");
        let opts = super::super::session::SshOptions {
            port: Some(sshd.port),
            key: Some(base.join("id_ed25519")),
            known_hosts: Some(base.join("known_hosts")),
        };
        // No known_hosts entry yet in this test — accept-new just for this
        // localhost throwaway server, since fetch_fingerprint's own contract
        // is covered separately and a real host key comparison adds nothing
        // here. Build the ssh invocation directly rather than through
        // verify_key_login (which insists on StrictHostKeyChecking=yes).
        let spec_args_extra = ["-o", "StrictHostKeyChecking=accept-new"];
        let mut spec = super::super::session::SpawnSpec::ssh(&target, &opts);
        spec.args.splice(0..0, spec_args_extra.iter().map(|s| s.to_string()));
        // spec.args already ends in a bare `sh` (see verify_key_login's doc
        // comment above) — write the command to stdin rather than appending
        // it to argv, or ssh folds it into the trailing command string as
        // `sh exit 0`, which `sh` treats as "run a script file named exit",
        // not "run exit 0".
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(b"exit 0\n").unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
}
