//! Jodd's own executable, re-entered as ssh's `SSH_ASKPASS` helper, so a
//! password the user typed into the app can authenticate one `ssh` /
//! `ssh-keygen` invocation without ever touching disk or argv. See
//! docs/superpowers/specs/2026-09-27-ssh-easy-setup-design.md §3.

pub const ASKPASS_ENV_VAR: &str = "JODD_ASKPASS_SECRET";

/// ssh/ssh-keygen pass the prompt text as argv[1] — e.g.
/// "me@box's password:" or "Enter passphrase for key '...'". Anything else
/// (a yes/no host-key question, for example) must not get the secret.
pub fn looks_like_secret_prompt(prompt: &str) -> bool {
    let p = prompt.to_lowercase();
    p.contains("password") || p.contains("passphrase")
}

pub fn run_as_askpass(prompt: &str, secret: Option<&str>) -> Result<String, ()> {
    match secret {
        Some(s) if looks_like_secret_prompt(prompt) => Ok(s.to_string()),
        _ => Err(()),
    }
}

/// Called at the very top of `main()`, before Tauri starts. If this process
/// was launched as the askpass helper (`JODD_ASKPASS_SECRET` is set), print
/// the secret for a password/passphrase prompt (or nothing, for anything
/// else — ssh then fails the auth attempt, which is the safe outcome) and
/// return `true` so the caller exits immediately without starting the app.
pub fn maybe_run_as_askpass() -> bool {
    let Ok(secret) = std::env::var(ASKPASS_ENV_VAR) else { return false };
    let prompt = std::env::args().nth(1).unwrap_or_default();
    if let Ok(s) = run_as_askpass(&prompt, Some(&secret)) {
        println!("{s}");
    }
    true
}

/// Environment for the ONE child `ssh`/`ssh-keygen` process that should use
/// this executable as its askpass helper. The secret lives only in this
/// child's environment — never written to disk, never in argv.
///
/// `current_exe()` is the real, shipped `jodd` binary in every production
/// build — the one that checks `maybe_run_as_askpass()` at the top of its own
/// `main()` — so this is the only path outside `#[cfg(test)]`. Under
/// `cargo test`, `current_exe()` instead resolves to the test-harness binary
/// (`jodd_lib-<hash>`), which has no such check in libtest's own `main` and
/// so cannot answer a real askpass prompt. `JODD_TEST_ASKPASS_EXE_OVERRIDE`
/// lets a unit test substitute a disposable helper script for that one case.
/// `cfg!(test)` makes this branch compile-time-eliminated in every non-test
/// build (including release), so production behavior is unchanged.
pub fn spawn_env(secret: &str) -> Vec<(String, String)> {
    let exe = if cfg!(test) {
        std::env::var("JODD_TEST_ASKPASS_EXE_OVERRIDE").ok()
    } else {
        None
    }
    .unwrap_or_else(|| {
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "jodd".to_string())
    });
    vec![
        ("SSH_ASKPASS".to_string(), exe),
        ("SSH_ASKPASS_REQUIRE".to_string(), "force".to_string()),
        ("DISPLAY".to_string(), std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string())),
        (ASKPASS_ENV_VAR.to_string(), secret.to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_prompt_gets_the_secret() {
        assert_eq!(run_as_askpass("me@box's password:", Some("hunter2")), Ok("hunter2".to_string()));
    }

    #[test]
    fn a_passphrase_prompt_gets_the_secret() {
        assert_eq!(run_as_askpass("Enter passphrase for key '/tmp/id':", Some("hunter2")), Ok("hunter2".to_string()));
    }

    #[test]
    fn a_yesno_prompt_gets_nothing() {
        assert_eq!(run_as_askpass("Are you sure you want to continue connecting (yes/no)?", Some("hunter2")), Err(()));
    }

    #[test]
    fn no_secret_configured_gets_nothing() {
        assert_eq!(run_as_askpass("me@box's password:", None), Err(()));
    }

    #[test]
    fn a_password_containing_shell_metacharacters_round_trips_unmangled() {
        let weird = "p$w`o\"rd'\n2";
        assert_eq!(run_as_askpass("me@box's password:", Some(weird)), Ok(weird.to_string()));
    }
}
