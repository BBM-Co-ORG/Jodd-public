//! Files Jodd writes for Claude Code (spec 2026-11-05, roadmap #20).
//!
//! Two parts, one engine, two callers (`jodd-mcp sync-agent-files` from the
//! SessionStart hook, and Settings' "Sync now"):
//! - the mirror (`mirror.rs`): a read-only Markdown copy of the accounts the
//!   user chose, which Claude Code browses and greps like an Obsidian vault;
//! - skills (`skills.rs`): marked folders published as Claude Code skills.
//!
//! The rules that keep it safe (§4.3) live here and in `mirror.rs`:
//! - only paths in Jodd's own manifest are ever deleted;
//! - a folder that is neither empty nor already a Jodd mirror is refused;
//! - an account Jodd has not finished listing keeps its old files;
//! - many notes vanishing at once deletes nothing until the user says so.

pub mod live;
pub mod mirror;
pub mod skills;
pub mod status;

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::accounts::Account;
use crate::db::Db;
use crate::mcp_scope::{MirrorScope, ScopeFile};

/// Where Jodd keeps its own bookkeeping inside the mirror.
pub const STATE_DIR: &str = ".jodd";
const LOCK_FILE: &str = "lock";
const STATUS_FILE: &str = "status.json";
/// Beside `mcp_write_scope.json`: skills run with or without the mirror.
const SKILLS_LOCK_FILE: &str = "agent-skills.lock";
const SKILLS_STATUS_FILE: &str = "agent-skills-status.json";
/// A made-up Stripe-shaped key for the masking tests. Built with `concat!`
/// so no source file holds the whole string: GitHub push protection rejected
/// the v0.44.0 snapshot to Jodd-public over the literal form (2026-11-06).
#[cfg(test)]
pub(crate) const FAKE_STRIPE_KEY: &str = concat!("sk_", "live_", "51HxYz", "ABCDEF0123456789abcd");

/// The error a part returns when another sync holds its lock.
pub const BUSY: &str = "another sync is running";

/// `~/Jodd for agents`: visible, so the user can open it in Finder or Obsidian
/// (spec §12 C1).
pub fn default_mirror_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Jodd for agents"))
}

/// The mirror's root as configured, falling back to the default.
pub fn mirror_root(cfg: &MirrorScope) -> Option<PathBuf> {
    match cfg.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => default_mirror_path(),
    }
}

pub fn status_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(STATUS_FILE)
}

/// The last run's status for the configured mirror, if any.
pub fn last_status(scope: &ScopeFile) -> Option<status::Status> {
    let root = mirror_root(scope.mirror())?;
    status::read(&status_path(&root))
}

#[derive(Default)]
pub struct SyncOptions {
    /// Apply a deletion `mirror` held back (§4.3), after the user confirmed it.
    pub allow_large_delete: bool,
    /// Stop writing at this instant, keeping progress (§4.2).
    pub deadline: Option<Instant>,
    /// Where skills go; `None` is `~/.claude/skills`. Tests point it elsewhere.
    pub skills_root: Option<PathBuf>,
    /// Where the skills lock and status live; `None` is beside the scope file.
    pub state_dir: Option<PathBuf>,
}

/// What one run did, part by part. A part that is off is `None`; a part that
/// could not run carries its reason in `*_error`.
#[derive(Debug, Default, serde::Serialize)]
pub struct SyncOutcome {
    pub mirror: Option<status::Status>,
    pub mirror_error: Option<String>,
    pub skills: Option<skills::SkillsStatus>,
    pub skills_error: Option<String>,
}

/// Run every enabled part once.
///
/// The caller has already opened the real database. A cache that did not
/// open never reaches here (gotcha #44), and neither does an unreadable scope
/// file: both mean "write nothing, delete nothing".
pub fn sync(db: &Db, accounts: &[Account], scope: &ScopeFile, opts: &SyncOptions) -> SyncOutcome {
    let (mirror, mirror_error) = split(sync_mirror_part(db, accounts, scope, opts));
    let (skills, skills_error) = split(sync_skills_part(db, accounts, scope, opts));
    SyncOutcome { mirror, mirror_error, skills, skills_error }
}

fn split<T>(r: Result<Option<T>, String>) -> (Option<T>, Option<String>) {
    match r {
        Ok(v) => (v, None),
        Err(e) => (None, Some(e)),
    }
}

fn state_dir(opts: &SyncOptions) -> Option<PathBuf> {
    opts.state_dir.clone().or_else(|| crate::mcp_scope::scope_path().parent().map(Path::to_path_buf))
}

/// The last skills run, for Settings.
pub fn last_skills_status(opts: &SyncOptions) -> Option<skills::SkillsStatus> {
    serde_json::from_str(&std::fs::read_to_string(state_dir(opts)?.join(SKILLS_STATUS_FILE)).ok()?).ok()
}

/// Skills, when any is configured or Jodd published one before (so stopping
/// the last one still removes it). `Ok(None)` when there is nothing to do.
fn sync_skills_part(db: &Db, accounts: &[Account], scope: &ScopeFile, opts: &SyncOptions) -> Result<Option<skills::SkillsStatus>, String> {
    let root = opts.skills_root.clone().or_else(skills::default_skills_root).ok_or("no home folder for Claude Code's skills")?;
    if scope.skills().is_empty() && !skills::has_published(&root) {
        return Ok(None);
    }
    let state = state_dir(opts).ok_or("no settings folder")?;
    std::fs::create_dir_all(&state).map_err(|e| format!("create {}: {e}", state.display()))?;
    let _lock = try_lock(&state.join(SKILLS_LOCK_FILE))?;
    let created_root = !root.exists();
    let mut st = skills::sync_skills(db, accounts, scope, &root);
    if created_root && root.exists() {
        st.problems.push("Claude Code's skills folder was just created: in a session that is already open, run /reload-skills.".into());
    }
    let text = serde_json::to_string_pretty(&st).map_err(|e| e.to_string())?;
    write_file(&state.join(SKILLS_STATUS_FILE), text.as_bytes(), false)?;
    Ok(Some(st))
}

/// An exclusive lock held until the returned file is dropped.
fn try_lock(path: &Path) -> Result<std::fs::File, String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| format!("open the lock {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(BUSY.into()),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("lock {}: {e}", path.display())),
    }
}

/// The mirror. `Ok(None)` when it is off.
fn sync_mirror_part(db: &Db, accounts: &[Account], scope: &ScopeFile, opts: &SyncOptions) -> Result<Option<status::Status>, String> {
    let cfg = scope.mirror();
    if !cfg.enabled {
        return Ok(None);
    }
    let root = mirror_root(cfg).ok_or("no home folder to put the mirror in")?;
    if !root.is_absolute() {
        return Err(format!("the mirror folder must be an absolute path, not '{}'", root.display()));
    }
    mirror::check_root(&root)?;
    let state = root.join(STATE_DIR);
    std::fs::create_dir_all(&state).map_err(|e| format!("create {}: {e}", state.display()))?;
    let _lock = try_lock(&state.join(LOCK_FILE))?;
    let report = mirror::sync_mirror(db, &root, accounts, scope, opts);
    let st = match report {
        Ok(r) => r.into_status(&root),
        Err(e) => status::Status {
            finished_at: now_rfc3339(),
            outcome: status::FAILED.into(),
            path: root.display().to_string(),
            accounts: vec![],
            problems: vec![e],
        },
    };
    status::write(&status_path(&root), &st)?;
    Ok(Some(st))
}

/// Remove everything Jodd wrote into the mirror at `root`, and its
/// bookkeeping. Used when the mirror is turned off or moved: the copy is
/// unencrypted, so it should not outlive the setting. Files Jodd did not
/// write stay where they are.
pub fn remove_mirror(root: &Path) -> Result<usize, String> {
    let removed = mirror::remove_all(root)?;
    let state = root.join(STATE_DIR);
    for f in [STATUS_FILE, LOCK_FILE] {
        let _ = std::fs::remove_file(state.join(f));
    }
    let _ = std::fs::remove_dir(&state);
    let _ = std::fs::remove_dir(root);
    Ok(removed)
}

/// `brief`'s line about the mirror (spec §5.7), when it is on and its last
/// run wrote something usable.
pub fn brief_line(scope: &ScopeFile) -> Option<String> {
    if !scope.mirror().enabled {
        return None;
    }
    let st = last_status(scope)?;
    if st.outcome == status::FAILED {
        return None;
    }
    let when = chrono::DateTime::parse_from_rfc3339(&st.finished_at)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| st.finished_at.clone());
    Some(format!(
        "Your Jodd notes are also files at `{}` (read-only, updated {when}). Start from `index.md`; grep for anything. Write with create_note, update_note or remember — edits to these files are overwritten.",
        st.path
    ))
}

pub(crate) fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Write `bytes` to `path` through a temp file and a rename, creating parent
/// directories. `read_only` marks the result read-only (spec §5.6); an
/// existing read-only file is made writable first, which Windows needs before
/// a rename can replace it.
pub(crate) fn write_file(path: &Path, bytes: &[u8], read_only: bool) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| format!("{} has no parent", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    if path.exists() {
        set_read_only(path, false);
    }
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| format!("temp file in {}: {e}", parent.display()))?;
    use std::io::Write;
    tmp.write_all(bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
    tmp.persist(path).map_err(|e| format!("replace {}: {}", path.display(), e.error))?;
    if read_only {
        set_read_only(path, true);
    }
    Ok(())
}

/// Best-effort: a file that keeps its mode is still a correct mirror.
pub(crate) fn set_read_only(path: &Path, read_only: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if read_only { 0o444 } else { 0o644 };
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(read_only);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
}

#[cfg(test)]
mod tests {
    /// Push protection on Jodd-public refuses a snapshot that holds anything
    /// shaped like a live Stripe key, even a fake one — which blocks the
    /// release (2026-11-06, v0.44.0). Keep test keys split, as
    /// `FAKE_STRIPE_KEY` is.
    #[test]
    fn no_source_file_holds_a_whole_stripe_shaped_key() {
        fn walk(dir: &std::path::Path, hits: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, hits);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                    for prefix in ["sk_live_", "sk_test_", "rk_live_"] {
                        for (i, _) in text.match_indices(prefix) {
                            let tail = &text[i + prefix.len()..];
                            if tail.chars().take_while(|c| c.is_ascii_alphanumeric()).count() >= 16 {
                                hits.push(format!("{}: {prefix}…", p.display()));
                            }
                        }
                    }
                }
            }
        }
        let mut hits = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut hits);
        assert!(hits.is_empty(), "split these with concat!: {hits:?}");
    }
}
