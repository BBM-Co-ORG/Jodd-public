//! Scripts run once, over the password session, to set a server up (spec
//! 2026-10-13-remote-agent-cli §4.4 and §4.6). They run through the normal
//! framed `sh` (`russh_session::run_on`), never through the restricted key.

use crate::backend::ssh::scripts::{self, Flavor};
use crate::backend::ssh::session::sh_quote;
use crate::llm::remote::wrapper::{wrapper_file_name, CliPaths};

/// Asks the login shell (then an interactive one, for nvm-style setups that
/// only `.bashrc` knows) where each CLI is and what PATH it runs with. Output
/// lines are prefixed so a chatty profile cannot be mistaken for an answer.
pub const PROBE_PATHS: &str = r#"s=${SHELL:-/bin/sh}
for mode in lc lic; do
  "$s" -$mode 'printf "\n@@JODD %s PATH=%s\n" "$0" "$PATH"; for c in claude codex; do printf "\n@@JODD %s %s=%s\n" "$0" "$c" "$(command -v "$c" 2>/dev/null)"; done' "$mode" </dev/null 2>/dev/null | grep '^@@JODD '
done
exit 0
"#;

pub fn parse_probe_paths(stdout: &str) -> CliPaths {
    let line = |mode: &str, key: &str| -> Option<String> {
        let prefix = format!("@@JODD {mode} {key}=");
        stdout.lines().find_map(|l| l.strip_prefix(&prefix)).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    };
    // A CLI path must be absolute (`command -v` prints `alias …` or a bare name
    // for builtins/aliases); a PATH value is taken as it comes.
    let cli = |mode: &str, key: &str| line(mode, key).filter(|v| v.starts_with('/'));
    let pick = |c: &str| cli("lc", c).or_else(|| cli("lic", c));
    let claude = pick("claude");
    let codex = pick("codex");
    let only_interactive = ["claude", "codex"].iter().any(|c| cli("lc", c).is_none() && cli("lic", c).is_some());
    let path_env = if only_interactive { line("lic", "PATH").or_else(|| line("lc", "PATH")) } else { line("lc", "PATH").or_else(|| line("lic", "PATH")) }
        .unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".to_string());
    CliPaths { path_env, claude, codex }
}

pub fn authorized_line_marker(key_id: &str) -> String {
    format!(" jodd-ai-{key_id}")
}

/// Installs `~/.jodd/ai-run-v<n>` and this key's restricted line, replacing an
/// older line for the same key id and never touching any other line. Deletes
/// `ai-run-v*` files that no line references any more. Prints `installed`.
pub fn install_script(f: Flavor, wrapper_text: &str, key_id: &str, pubkey_line: &str) -> String {
    let name = wrapper_file_name();
    let body = scripts::key_body(pubkey_line);
    format!(
        "umask 077\n\
         case $HOME in *\\\"*|*\\ *|'') echo \"unsupported home directory\" >&2; exit 1 ;; esac\n\
         mkdir -p \"$HOME/.jodd\" \"$HOME/.ssh\" || exit 1\n\
         chmod 700 \"$HOME/.jodd\" \"$HOME/.ssh\" || exit 1\n\
         t=\"$HOME/.jodd/.ai-run.tmp.$$\"\n\
         {decode}\
         chmod 700 \"$t\" && mv \"$t\" \"$HOME/.jodd/{name}\" || {{ rm -f \"$t\"; exit 1; }}\n\
         k=\"$HOME/.ssh/authorized_keys\"; touch \"$k\" && chmod 600 \"$k\" || exit 1\n\
         n=\"$HOME/.ssh/.jodd-ai.tmp.$$\"\n\
         grep -a -v {marker} \"$k\" > \"$n\"; s=$?\n\
         [ \"$s\" -le 1 ] || {{ rm -f \"$n\"; exit 1; }}\n\
         printf 'restrict,command=\"%s/.jodd/{name} --key %s\" %s jodd-ai-%s\\n' \"$HOME\" {id} {body} {id} >> \"$n\" || {{ rm -f \"$n\"; exit 1; }}\n\
         chmod 600 \"$n\" && mv \"$n\" \"$k\" || {{ rm -f \"$n\"; exit 1; }}\n\
         for w in \"$HOME\"/.jodd/ai-run-v*; do\n\
           [ -e \"$w\" ] || continue\n\
           grep -qF \"/.jodd/${{w##*/}} \" \"$k\" || rm -f \"$w\"\n\
         done\n\
         echo installed\n",
        decode = scripts::decode_to_tmp(f, wrapper_text.as_bytes(), &scripts::sha256_hex(wrapper_text.as_bytes())),
        marker = sh_quote(&format!("{}$", authorized_line_marker(key_id))),
        id = sh_quote(key_id),
        body = sh_quote(body),
    )
}

/// These run the generated scripts with `sh`: Unix only.
#[cfg(all(test, unix))]
mod shell_tests {
    use super::*;
    use crate::llm::remote::wrapper::{generate, CliPaths, wrapper_file_name};
    use std::process::Command;

    fn sh_in_home(home: &std::path::Path, script: &str) -> (i32, String) {
        let o = Command::new("sh").arg("-c").arg(script).env("HOME", home).output().unwrap();
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into())
    }

    fn flavor(home: &std::path::Path) -> crate::backend::ssh::scripts::Flavor {
        let (_, out) = sh_in_home(home, crate::backend::ssh::scripts::PROBE);
        crate::backend::ssh::scripts::parse_probe(&out).unwrap()
    }

    const PUB: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDummyDummyDummyDummyDummyDummyDummyDu jodd-ai-abcd1234";

    fn wrapper() -> String {
        generate(&CliPaths { path_env: "/usr/bin:/bin".into(), claude: None, codex: None }).unwrap()
    }

    #[test]
    fn the_probe_runs_and_reports_the_real_login_shell_path() {
        let home = tempfile::tempdir().unwrap();
        let (code, out) = sh_in_home(home.path(), PROBE_PATHS);
        assert_eq!(code, 0);
        assert!(out.contains("@@JODD lc PATH=/"), "{out}");
    }

    #[test]
    fn install_writes_the_wrapper_and_one_restricted_line() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        std::fs::write(home.path().join(".ssh/authorized_keys"), "ssh-ed25519 AAAAother me@laptop\n").unwrap();
        let s = install_script(flavor(home.path()), &wrapper(), "abcd1234", PUB);
        let (code, out) = sh_in_home(home.path(), &s);
        assert_eq!((code, out.trim()), (0, "installed"));
        let installed = std::fs::read_to_string(home.path().join(".jodd").join(wrapper_file_name())).unwrap();
        assert_eq!(installed, wrapper());
        let keys = std::fs::read_to_string(home.path().join(".ssh/authorized_keys")).unwrap();
        let want = format!(
            "restrict,command=\"{}/.jodd/{} --key abcd1234\" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDummyDummyDummyDummyDummyDummyDummyDu jodd-ai-abcd1234",
            home.path().display(), wrapper_file_name());
        assert_eq!(keys, format!("ssh-ed25519 AAAAother me@laptop\n{want}\n"));
    }

    #[test]
    fn install_twice_leaves_one_line() {
        let home = tempfile::tempdir().unwrap();
        let s = install_script(flavor(home.path()), &wrapper(), "abcd1234", PUB);
        assert_eq!(sh_in_home(home.path(), &s).0, 0);
        assert_eq!(sh_in_home(home.path(), &s).0, 0);
        let keys = std::fs::read_to_string(home.path().join(".ssh/authorized_keys")).unwrap();
        assert_eq!(keys.matches("jodd-ai-abcd1234").count(), 1, "{keys}");
    }

    #[test]
    fn install_keeps_a_wrapper_another_key_still_uses() {
        let home = tempfile::tempdir().unwrap();
        let jodd = home.path().join(".jodd");
        std::fs::create_dir_all(&jodd).unwrap();
        std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
        // A phone on an older Jodd installed v0 for key 11111111; an orphan v7 has no line.
        std::fs::write(jodd.join("ai-run-v0"), "old").unwrap();
        std::fs::write(jodd.join("ai-run-v7"), "orphan").unwrap();
        std::fs::write(home.path().join(".ssh/authorized_keys"), format!(
            "restrict,command=\"{}/.jodd/ai-run-v0 --key 11111111\" ssh-ed25519 AAAAphone jodd-ai-11111111\n", home.path().display())).unwrap();
        let s = install_script(flavor(home.path()), &wrapper(), "abcd1234", PUB);
        assert_eq!(sh_in_home(home.path(), &s).0, 0);
        assert!(jodd.join("ai-run-v0").exists(), "still referenced by the phone's line");
        assert!(!jodd.join("ai-run-v7").exists(), "referenced by nothing");
        let keys = std::fs::read_to_string(home.path().join(".ssh/authorized_keys")).unwrap();
        assert!(keys.contains("jodd-ai-11111111") && keys.contains("jodd-ai-abcd1234"), "{keys}");
    }

    #[test]
    fn install_refuses_a_home_that_cannot_be_quoted_in_a_forced_command() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("we\"ird");
        std::fs::create_dir(&home).unwrap();
        let s = install_script(flavor(&home), &wrapper(), "abcd1234", PUB);
        let (code, _) = sh_in_home(&home, &s);
        assert_ne!(code, 0);
        assert!(!home.join(".ssh/authorized_keys").exists() || !std::fs::read_to_string(home.join(".ssh/authorized_keys")).unwrap().contains("jodd-ai"));
    }

    #[test]
    fn forget_removes_only_its_own_line() {
        let home = tempfile::tempdir().unwrap();
        let s = install_script(flavor(home.path()), &wrapper(), "abcd1234", PUB);
        assert_eq!(sh_in_home(home.path(), &s).0, 0);
        let keys = home.path().join(".ssh/authorized_keys");
        let before = std::fs::read_to_string(&keys).unwrap();
        std::fs::write(&keys, format!("ssh-ed25519 AAAAother me@laptop\n{before}")).unwrap();
        let w = home.path().join(".jodd").join(wrapper_file_name());
        let o = Command::new("sh").arg(&w).args(["--key", "abcd1234", "jodd-ai-run", "1", "forget"]).env("HOME", home.path()).output().unwrap();
        assert_eq!(o.status.code(), Some(0));
        assert_eq!(std::fs::read_to_string(&keys).unwrap(), "ssh-ed25519 AAAAother me@laptop\n");
    }

    fn sh_env(home: &std::path::Path, script: &str, envs: &[(&str, &std::ffi::OsStr)]) -> (i32, String) {
        let mut c = Command::new("sh");
        c.arg("-c").arg(script).env("HOME", home);
        for (k, v) in envs { c.env(k, v); }
        let o = c.output().unwrap();
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into())
    }

    const LATIN1_KEY: &[u8] = b"ssh-ed25519 AAAAlatin caf\xe9@laptop\n";

    #[test]
    fn a_profile_that_prints_without_a_newline_does_not_eat_the_path_line() {
        let home = tempfile::tempdir().unwrap();
        let fake = home.path().join("fakesh");
        std::fs::write(&fake, "#!/bin/sh\nprintf nonl\nexec /bin/sh \"$@\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (code, out) = sh_env(home.path(), PROBE_PATHS, &[("SHELL", fake.as_os_str())]);
        assert_eq!(code, 0);
        assert!(parse_probe_paths(&out).path_env != "/usr/local/bin:/usr/bin:/bin", "default PATH was baked: {out}");
        assert!(out.lines().any(|l| l.starts_with("@@JODD lc PATH=/")), "{out}");
    }

    #[test]
    fn install_keeps_a_foreign_key_with_a_non_utf8_byte() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        let keys = home.path().join(".ssh/authorized_keys");
        let mut seed = b"ssh-ed25519 AAAAone a@b\n".to_vec();
        seed.extend_from_slice(LATIN1_KEY);
        seed.extend_from_slice(b"ssh-ed25519 AAAAtwo c@d\n");
        std::fs::write(&keys, &seed).unwrap();
        let s = install_script(flavor(home.path()), &wrapper(), "abcd1234", PUB);
        let (code, out) = sh_env(home.path(), &s, &[("LC_ALL", "C.UTF-8".as_ref())]);
        assert_eq!((code, out.trim()), (0, "installed"));
        let after = std::fs::read(&keys).unwrap();
        assert!(after.starts_with(&seed), "foreign lines must survive byte for byte");
    }

    #[test]
    fn forget_keeps_a_foreign_key_with_a_non_utf8_byte() {
        let home = tempfile::tempdir().unwrap();
        let s = install_script(flavor(home.path()), &wrapper(), "abcd1234", PUB);
        assert_eq!(sh_in_home(home.path(), &s).0, 0);
        let keys = home.path().join(".ssh/authorized_keys");
        let mut seed = b"ssh-ed25519 AAAAone a@b\n".to_vec();
        seed.extend_from_slice(LATIN1_KEY);
        seed.extend_from_slice(b"ssh-ed25519 AAAAtwo c@d\n");
        let mut all = seed.clone();
        all.extend_from_slice(&std::fs::read(&keys).unwrap());
        std::fs::write(&keys, all).unwrap();
        let w = home.path().join(".jodd").join(wrapper_file_name());
        let o = Command::new("sh").arg(&w).args(["--key", "abcd1234", "jodd-ai-run", "1", "forget"])
            .env("HOME", home.path()).env("LC_ALL", "C.UTF-8").output().unwrap();
        assert_eq!(o.status.code(), Some(0));
        assert_eq!(std::fs::read(&keys).unwrap(), seed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_prefers_the_login_shell_and_falls_back_to_interactive() {
        let out = "motd noise\n@@JODD lc PATH=/usr/bin:/bin\n@@JODD lc claude=/home/u/.local/bin/claude\n@@JODD lc codex=\n\
                   @@JODD lic PATH=/home/u/.nvm/bin:/usr/bin:/bin\n@@JODD lic claude=/home/u/.local/bin/claude\n@@JODD lic codex=/home/u/.nvm/bin/codex\n";
        let p = parse_probe_paths(out);
        assert_eq!(p.claude.as_deref(), Some("/home/u/.local/bin/claude"));
        assert_eq!(p.codex.as_deref(), Some("/home/u/.nvm/bin/codex"));
        // codex was only found interactively, so the interactive PATH (which has node) wins.
        assert_eq!(p.path_env, "/home/u/.nvm/bin:/usr/bin:/bin");
    }

    #[test]
    fn parse_ignores_relative_or_empty_paths() {
        let p = parse_probe_paths("@@JODD lc PATH=/bin\n@@JODD lc claude=claude: aliased\n@@JODD lc codex=\n");
        assert_eq!(p.claude, None);
        assert_eq!(p.codex, None);
        assert_eq!(p.path_env, "/bin");
    }

    #[test]
    fn parse_with_nothing_found_uses_a_sane_default_path() {
        assert_eq!(parse_probe_paths("").path_env, "/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn parse_falls_back_to_the_login_path_when_the_interactive_one_is_missing() {
        let p = parse_probe_paths("@@JODD lc PATH=/usr/bin:/bin\n@@JODD lc claude=\n@@JODD lic claude=/home/u/bin/claude\n");
        assert_eq!(p.claude.as_deref(), Some("/home/u/bin/claude"));
        assert_eq!(p.path_env, "/usr/bin:/bin");
    }

    #[test]
    fn parse_keeps_a_path_that_starts_with_a_colon_or_has_relative_entries() {
        let p = parse_probe_paths("@@JODD lc PATH=:/usr/bin:bin:/bin\n");
        assert_eq!(p.path_env, ":/usr/bin:bin:/bin");
    }
}
