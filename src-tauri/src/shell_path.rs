//! Recover the user's real PATH on macOS.
//!
//! A GUI app launched from Finder or the Dock is started by `launchd`, not by
//! a shell, so it inherits launchd's PATH — typically just
//! `/usr/bin:/bin:/usr/sbin:/sbin`. Every agent CLI Jodd shells out to
//! (`claude`, `codex`, …) installs somewhere else: `~/.local/bin`,
//! `/opt/homebrew/bin`, a Node version manager's shim directory. So
//! `which::which("claude")` fails in the shipped app while succeeding in
//! `npm run tauri dev`, which inherits the terminal's PATH.
//!
//! The standard remedy — the one VS Code and friends use — is to ask the
//! user's login shell what PATH it would set, then adopt it.

/// Extract the PATH a login shell printed, ignoring whatever else it said.
///
/// A login+interactive shell runs the user's full startup files, which is the
/// point (PATH is usually set in `.zshrc`, not `.zprofile`) but also means the
/// output can carry a motd, a version-manager banner, or a framework's
/// greeting. So the caller has the shell echo a sentinel immediately before
/// the PATH, and this reads only what follows it.
/// Replace the process PATH with the login shell's, if that is an improvement.
///
/// Call once, before anything resolves a binary — `which::which` in
/// `llm::agent_cli` is the only consumer today, but every subprocess Jodd
/// spawns inherits the result.
///
/// macOS only: this is a `launchd`-vs-shell discrepancy specific to how the
/// Mac starts GUI apps. Windows has no equivalent, and while a Linux desktop
/// launcher can differ from a terminal, that was not measured here — better to
/// leave a platform alone than to guess at it.
#[cfg(target_os = "macos")]
pub fn adopt_login_shell_path() {
    let inherited = std::env::var("PATH").unwrap_or_default();
    match login_shell_path_with_env(&inherited) {
        Some(p) if p != inherited => {
            std::env::set_var("PATH", &p);
            crate::log!("shell_path: adopted login shell PATH ({} chars)", p.len());
        }
        Some(_) => crate::log!("shell_path: login shell PATH matches the inherited one"),
        None => crate::log!(
            "shell_path: could not read a PATH from the login shell — keeping the inherited one"
        ),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn adopt_login_shell_path() {}

/// Ask the login shell for its PATH, starting it from `base_path`.
///
/// Split out from [`adopt_login_shell_path`] so the probe can be exercised
/// against a known-poor starting PATH instead of whatever the test runner
/// happened to inherit.
pub fn login_shell_path_with_env(base_path: &str) -> Option<String> {
    let shell = std::env::var("SHELL").ok()?;
    probe_shell_path(&shell, base_path)
}

/// The sentinel the probe script echoes immediately before the PATH. Long and
/// unpunctuated so no plausible motd or shell banner reproduces it by accident.
const SENTINEL: &str = "__JODD_LOGIN_PATH__";

fn probe_shell_path(shell: &str, base_path: &str) -> Option<String> {
    // -l so login files (.zprofile, .bash_profile) run; -i as well because
    // most people export PATH from .zshrc, which a non-interactive shell
    // skips. Both are needed — either alone misses a common setup.
    let script = format!("printf %s {SENTINEL}; printenv PATH");
    let out = std::process::Command::new(shell)
        .args(["-ilc", &script])
        // Start from the PATH we are trying to improve on, so the shell's
        // startup files compose against the same base the app really has.
        .env("PATH", base_path)
        // An interactive shell that decides to read from stdin would block
        // app startup forever otherwise.
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    parse_shell_path_output(&String::from_utf8_lossy(&out.stdout), SENTINEL)
}

pub fn parse_shell_path_output(raw: &str, sentinel: &str) -> Option<String> {
    let at = raw.find(sentinel)?;
    // Only the remainder of the sentinel's own line: the shell may keep
    // talking afterwards (exit traps, "you have mail").
    let line = raw[at + sentinel.len()..].lines().next().unwrap_or("");
    let path = line.trim();
    // An empty PATH is a failed probe, not a valid answer. Adopting it would
    // replace a working inherited PATH with nothing.
    (!path.is_empty()).then(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "__JODD_PATH__";

    /// The bug this module exists for, reproduced end to end.
    ///
    /// Runs the real login shell under the PATH `launchd` hands a
    /// Finder-launched app, and asserts we recover something better. Asserting
    /// only "longer than the launchd default and still contains /usr/bin"
    /// keeps it true on any mac and on CI, where no agent CLI is installed —
    /// a stricter assertion would pass here and fail there for the wrong
    /// reason.
    #[cfg(target_os = "macos")]
    #[test]
    fn recovers_a_richer_path_than_launchd_hands_a_gui_app() {
        const LAUNCHD: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
        let recovered = login_shell_path_with_env(LAUNCHD).expect("login shell should report a PATH");
        assert!(
            recovered.contains("/usr/bin"),
            "recovered PATH lost the system dirs: {recovered}"
        );
        assert!(
            recovered.len() > LAUNCHD.len(),
            "recovered PATH is no richer than launchd's, so the probe did nothing: {recovered}"
        );
    }

    #[test]
    fn extracts_the_path_that_follows_the_sentinel() {
        let raw = format!("{S}/Users/k/.local/bin:/usr/bin\n");
        assert_eq!(
            parse_shell_path_output(&raw, S).as_deref(),
            Some("/Users/k/.local/bin:/usr/bin")
        );
    }

    /// The reason the sentinel exists. Startup files print things — oh-my-zsh
    /// banners, nvm notices, "Last login:" — and some of that text can look
    /// like a PATH. Taking the first plausible line instead of the marked one
    /// would adopt the banner's text as the process PATH.
    #[test]
    fn ignores_everything_printed_before_the_sentinel() {
        let raw = format!(
            "Last login: Wed Jul 30 09:00\n\
             nvm: /decoy/bin:/another/decoy\n\
             {S}/Users/k/.local/bin:/usr/bin\n"
        );
        assert_eq!(
            parse_shell_path_output(&raw, S).as_deref(),
            Some("/Users/k/.local/bin:/usr/bin")
        );
    }

    /// A shell can also print *after* the PATH — an exit trap, a job-control
    /// notice. Only the sentinel's own line is the answer.
    #[test]
    fn stops_at_the_end_of_the_sentinel_line() {
        let raw = format!("{S}/usr/bin\nyou have mail\n");
        assert_eq!(parse_shell_path_output(&raw, S).as_deref(), Some("/usr/bin"));
    }

    /// The shell failed, was killed, or printed nothing recognisable. Falling
    /// back to the inherited PATH is correct; inventing one is not.
    #[test]
    fn returns_none_when_the_sentinel_never_appeared() {
        assert_eq!(parse_shell_path_output("command not found\n", S), None);
    }

    /// Sentinel present but PATH empty — treat as failure rather than
    /// overwriting a working PATH with nothing.
    #[test]
    fn returns_none_when_the_path_is_empty() {
        let raw = format!("{S}\n");
        assert_eq!(parse_shell_path_output(&raw, S), None);
    }
}
