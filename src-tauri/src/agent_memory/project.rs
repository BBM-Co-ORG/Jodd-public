//! Which project a coding agent is working on (spec §5.5): the repository,
//! not the model's guess. Reads `.git/config` directly — no `git` process,
//! so a session-start hook stays fast and works where git is not on PATH.

use std::path::Path;

/// `git@github.com:BBM-Co-ORG/Jodd.git`, `https://github.com/BBM-Co-ORG/Jodd`,
/// `ssh://git@host/x/Jodd.git/` → `Jodd`.
pub fn repo_name_from_remote(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    (!name.is_empty()).then(|| name.to_string())
}

fn origin_url(git_config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in git_config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == r#"[remote "origin"]"#;
        } else if in_origin {
            if let Some(v) = line.strip_prefix("url") {
                if let Some(v) = v.trim_start().strip_prefix('=') {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// Walk up from `dir` to the repository root. The origin's repository name
/// if there is one, else the root folder's name; `None` outside a repo.
pub fn project_from_dir(dir: &Path) -> Option<String> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let git = d.join(".git");
        if git.exists() {
            // A worktree's `.git` is a file pointing at the main repo's
            // gitdir; its config lives two levels up from that gitdir.
            let config = if git.is_dir() {
                std::fs::read_to_string(git.join("config")).ok()
            } else {
                std::fs::read_to_string(&git).ok().and_then(|s| {
                    let gitdir = s.trim().strip_prefix("gitdir:")?.trim().to_string();
                    let gitdir = d.join(gitdir);
                    std::fs::read_to_string(gitdir.parent()?.parent()?.join("config")).ok()
                })
            };
            return config
                .as_deref()
                .and_then(origin_url)
                .and_then(|u| repo_name_from_remote(&u))
                .or_else(|| d.file_name().map(|n| n.to_string_lossy().into_owned()));
        }
        cur = d.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_url_forms() {
        for (u, want) in [
            ("git@github.com:BBM-Co-ORG/Jodd.git", "Jodd"),
            ("https://github.com/BBM-Co-ORG/Jodd", "Jodd"),
            ("https://github.com/BBM-Co-ORG/Jodd.git/", "Jodd"),
            ("ssh://git@host:22/team/atlas2.git", "atlas2"),
        ] {
            assert_eq!(repo_name_from_remote(u).as_deref(), Some(want), "{u}");
        }
        assert_eq!(repo_name_from_remote(""), None);
    }

    #[test]
    fn the_origin_of_the_enclosing_repo_names_the_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = git@x:a/Other.git\n[remote \"origin\"]\n\turl = git@github.com:BBM-Co-ORG/Jodd.git\n",
        )
        .unwrap();
        let sub = dir.path().join("src/lib");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(project_from_dir(&sub).as_deref(), Some("Jodd"));
    }

    #[test]
    fn a_repo_without_origin_uses_its_folder_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("my-tool");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "[core]\n").unwrap();
        assert_eq!(project_from_dir(&root).as_deref(), Some("my-tool"));
    }

    #[test]
    fn outside_any_repo_there_is_no_project() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(project_from_dir(dir.path()), None);
    }
}
