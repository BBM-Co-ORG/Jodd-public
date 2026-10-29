//! Settings › Links, the part that is not a secret (spec
//! 2026-10-28-follow-links-and-discussions-design.md D2): which accounts
//! follow the best links automatically. Off for every account until the user
//! turns it on.
//!
//! A file beside the app config (`app_links.json`), never the keychain — the
//! Extract modal reads it every time it opens (gotcha #15). Kept apart from
//! `accounts.json` so a UI preference cannot race the writers that file has.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Followed automatically per source, at most (D2).
pub const AUTO_FOLLOW_MAX: usize = 3;
/// A candidate below this score is never followed automatically — higher
/// than `PRETICK_MIN_SCORE`, because nobody looks at the list first.
pub const AUTO_FOLLOW_MIN_SCORE: i32 = 40;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkSettings {
    /// Account ids that follow links automatically.
    #[serde(default)]
    pub auto_follow: Vec<String>,
}

impl LinkSettings {
    /// Turn auto-follow on or off for `account_id`; the list stays sorted and
    /// free of repeats, so the file diffs cleanly.
    pub fn set_auto_follow(&mut self, account_id: &str, on: bool) {
        self.auto_follow.retain(|a| a != account_id);
        if on {
            self.auto_follow.push(account_id.to_string());
            self.auto_follow.sort();
        }
    }

    /// Accounts that no longer exist leave the list.
    pub fn keep_only(&mut self, account_ids: &[String]) {
        self.auto_follow.retain(|a| account_ids.contains(a));
    }
}

fn path() -> Result<PathBuf, String> {
    let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
    let dir = base.join("jodd");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    Ok(dir.join("app_links.json"))
}

/// Missing or unreadable reads as "nothing turned on".
pub fn load() -> LinkSettings {
    path().ok().and_then(|p| fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(settings: &LinkSettings) -> Result<(), String> {
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(path()?, json).map_err(|e| format!("Could not save the link settings: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_keeps_one_sorted_entry_per_account() {
        let mut s = LinkSettings::default();
        s.set_auto_follow("gmail:b@x", true);
        s.set_auto_follow("gmail:a@x", true);
        s.set_auto_follow("gmail:a@x", true);
        assert_eq!(s.auto_follow, vec!["gmail:a@x", "gmail:b@x"]);
        s.set_auto_follow("gmail:b@x", false);
        assert_eq!(s.auto_follow, vec!["gmail:a@x"]);
        s.keep_only(&["gmail:z@x".to_string()]);
        assert!(s.auto_follow.is_empty());
    }

    #[test]
    fn an_old_or_empty_file_reads_as_nothing_on() {
        assert_eq!(serde_json::from_str::<LinkSettings>("{}").unwrap(), LinkSettings::default());
        assert!(AUTO_FOLLOW_MIN_SCORE > crate::ingest::links::PRETICK_MIN_SCORE);
    }
}
