//! Run an agent-CLI preset on the user's own server over SSH.
//! Spec: docs/superpowers/specs/2026-10-13-remote-agent-cli-design.md

use std::path::PathBuf;

use crate::accounts::AgentHost;

pub mod exec;
pub mod install;
pub mod wrapper;

#[cfg(all(test, unix))]
mod docker_tests;

#[cfg(all(test, unix))]
mod host_kit_tests;

/// The only presets a server may run. A Custom spec would make the wrapper
/// accept arbitrary argv, which un-restricts the key (spec §3).
pub const REMOTE_PRESETS: [&str; 2] = ["claude", "codex"];

pub fn remote_capable(preset_id: &str) -> bool {
    REMOTE_PRESETS.contains(&preset_id)
}

/// The id baked into the key comment (`jodd-ai-<id>`) and the forced command
/// (`--key <id>`): the first 8 hex digits of the key directory's uuid.
pub fn key_id_for(key_dir: &str) -> String {
    key_dir.chars().filter(|c| c.is_ascii_hexdigit()).take(8).collect::<String>().to_lowercase()
}

/// What the user sees: `vps.example`, or `vps.example:2222`.
pub fn host_label(h: &AgentHost) -> String {
    let host = h.target.rsplit_once('@').map(|(_, h)| h).unwrap_or(&h.target);
    match h.port {
        Some(p) if p != 22 => format!("{host}:{p}"),
        _ => host.to_string(),
    }
}

/// `<data>/jodd/ai-hosts/<key_dir>`. `key_dir` arrives over IPC and a later
/// command deletes this directory, so anything that is not a uuid — `..`, an
/// absolute path — yields `None` instead of a path outside `ai-hosts/`.
pub fn host_dir(h: &AgentHost) -> Option<PathBuf> {
    uuid::Uuid::parse_str(&h.key_dir).ok()?;
    crate::paths::data_base().map(|d| d.join("jodd").join("ai-hosts").join(&h.key_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::LlmConfig;

    #[test]
    fn only_claude_and_codex_run_remotely() {
        assert!(remote_capable("claude"));
        assert!(remote_capable("codex"));
        assert!(!remote_capable("custom"));
        assert!(!remote_capable("qwen"));
    }

    #[test]
    fn key_id_is_eight_lowercase_hex_digits() {
        assert_eq!(key_id_for("3F2A9B1C-0d4e-4f00-8a00-000000000000"), "3f2a9b1c");
    }

    #[test]
    fn host_label_hides_the_user_and_shows_a_non_default_port() {
        let h = AgentHost { target: "me@vps.example".into(), port: Some(2222), key_dir: "x".into() };
        assert_eq!(host_label(&h), "vps.example:2222");
        let h = AgentHost { port: Some(22), ..h };
        assert_eq!(host_label(&h), "vps.example");
    }

    #[test]
    fn host_dir_refuses_a_key_dir_that_is_not_a_uuid() {
        for bad in ["../..", "..", "/etc", "a/b", "", "x"] {
            let h = AgentHost { target: "me@vps".into(), port: None, key_dir: bad.into() };
            assert_eq!(host_dir(&h), None, "{bad:?}");
        }
        let ok = AgentHost { target: "me@vps".into(), port: None, key_dir: "3f2a9b1c-0d4e-4f00-8a00-000000000000".into() };
        if let Some(d) = host_dir(&ok) {
            assert!(d.ends_with("ai-hosts/3f2a9b1c-0d4e-4f00-8a00-000000000000"), "{d:?}");
        }
    }

    #[test]
    fn a_config_without_agent_host_still_parses() {
        let cfg: LlmConfig = serde_json::from_str(r#"{"provider":"agent_cli","agent_preset":"codex"}"#).unwrap();
        assert!(cfg.agent_host.is_none());
    }

    #[test]
    fn agent_host_round_trips() {
        let cfg: LlmConfig = serde_json::from_str(
            r#"{"provider":"agent_cli","agent_preset":"codex","agent_host":{"target":"me@vps","port":2222,"key_dir":"abc"}}"#,
        ).unwrap();
        let back: LlmConfig = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.agent_host, Some(AgentHost { target: "me@vps".into(), port: Some(2222), key_dir: "abc".into() }));
    }
}
