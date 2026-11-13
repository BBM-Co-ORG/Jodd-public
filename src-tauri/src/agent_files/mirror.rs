//! The read-only Markdown mirror (spec 2026-11-05 §5).
//!
//! One run: plan every file the mirror should hold, write the ones that
//! changed, then delete the ones Jodd wrote before and no longer plans. The
//! manifest (`.jodd/manifest.json`) lists exactly the files Jodd wrote and
//! their hashes; nothing outside it is ever deleted.
//!
//! What a note may show is the MCP read rule, unchanged: hidden folders
//! (`ScopeFile::is_hidden`, gotcha #33) and inactive accounts never reach the
//! disk. Secrets are masked on top of that (§5.3), because this copy is not
//! encrypted.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::status::{AccountStatus, Status, OK, PARTIAL};
use super::{now_rfc3339, set_read_only, write_file, SyncOptions, STATE_DIR};
use crate::accounts::Account;
use crate::backup::naming::{account_slug, dir_for_folder, sanitize_segment, NameAllocator};
use crate::db::{BackupRead, CachedNote, Db};
use crate::mcp_scope::ScopeFile;

const MANIFEST: &str = "manifest.json";
const MANIFEST_VERSION: u32 = 1;
/// Attachments above this are left out and counted (§5.4).
pub(crate) const MAX_ATTACHMENT_BYTES: usize = 50 * 1024 * 1024;
const CHECKPOINT_EVERY: usize = 100;
/// §4.3: a run that would delete more than half of an account's notes, and
/// more than this many, deletes nothing.
const HOLD_BACK_MIN: usize = 20;
const ATTACHMENTS_DIR: &str = "_attachments";
const INDEX_LINE_CHARS: usize = 120;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    /// Root-relative, `/`-separated path → what Jodd wrote there.
    files: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Entry {
    /// The account the file belongs to; `None` for the root's own files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    /// Set on note files only — what the hold-back rule counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uuid: Option<String>,
    /// sha256 of the bytes for text files; `cid:<uuid>/<content id>` for an
    /// attachment, whose bytes never change under one content id.
    hash: String,
}

enum Content {
    Text(String),
    Attachment { uuid: String, content_id: String },
}

struct Planned {
    entry: Entry,
    content: Content,
}

#[derive(Debug, Default)]
pub struct MirrorReport {
    pub accounts: Vec<AccountStatus>,
    pub finished: bool,
    pub problems: Vec<String>,
}

impl MirrorReport {
    pub fn into_status(self, root: &Path) -> Status {
        Status {
            finished_at: now_rfc3339(),
            outcome: if self.finished { OK } else { PARTIAL }.into(),
            path: root.display().to_string(),
            accounts: self.accounts,
            problems: self.problems,
        }
    }
}

fn manifest_path(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(MANIFEST)
}

/// Refuse a folder that holds anything and is not already a Jodd mirror:
/// the mirror writes `README.md` and `index.md` at its root, and must never
/// overwrite a file the user put there.
pub fn check_root(root: &Path) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    if !root.is_dir() {
        return Err(format!("{} is a file, not a folder", root.display()));
    }
    if manifest_path(root).is_file() {
        return Ok(());
    }
    let foreign = std::fs::read_dir(root)
        .map_err(|e| format!("read {}: {e}", root.display()))?
        .flatten()
        .any(|e| e.file_name() != STATE_DIR && e.file_name() != ".DS_Store");
    if foreign {
        return Err(format!(
            "{} already holds other files and is not a Jodd mirror. Choose an empty folder.",
            root.display()
        ));
    }
    Ok(())
}

fn load_manifest(root: &Path) -> (Manifest, Option<String>) {
    match std::fs::read_to_string(manifest_path(root)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Manifest { version: MANIFEST_VERSION, ..Default::default() }, None),
        Err(e) => (Manifest::default(), Some(format!("could not read the mirror's manifest ({e}); nothing was deleted"))),
        Ok(raw) => match serde_json::from_str::<Manifest>(&raw) {
            Ok(m) if m.version <= MANIFEST_VERSION => (m, None),
            Ok(_) => (Manifest::default(), Some("the mirror was written by a newer Jodd; nothing was deleted".into())),
            Err(e) => (Manifest::default(), Some(format!("the mirror's manifest is damaged ({e}); nothing was deleted"))),
        },
    }
}

fn save_manifest(root: &Path, m: &Manifest) -> Result<(), String> {
    let text = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    write_file(&manifest_path(root), text.as_bytes(), false)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn on_disk(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, seg| p.join(seg))
}

/// A Markdown link target that survives spaces and parentheses.
fn link(path: &str) -> String {
    path.replace('%', "%25").replace(' ', "%20").replace('(', "%28").replace(')', "%29")
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// `text` without Markdown's backslash escapes, and for each byte of the
/// result the byte offset it came from in `text` (plus one entry for the end).
/// The encoder writes `api\_key`, which the detector would not recognise as
/// `api_key`, so detection runs on this and the spans are mapped back.
fn unescaped(text: &str) -> (String, Vec<usize>) {
    let mut plain = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len() + 1);
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            if let Some(&(j, next)) = chars.peek() {
                if next.is_ascii_punctuation() {
                    chars.next();
                    plain.push(next);
                    map.push(j);
                    continue;
                }
            }
        }
        for k in 0..c.len_utf8() {
            map.push(i + k);
        }
        plain.push(c);
    }
    map.push(text.len());
    (plain, map)
}

/// Replace every secret `privacy::detect` finds with a mask. Returns the text
/// and whether anything was masked.
pub(crate) fn mask_secrets(text: &str) -> (String, bool) {
    let (plain, map) = unescaped(text);
    let mut hits = crate::privacy::detect::secrets(&plain);
    if hits.is_empty() {
        return (text.to_string(), false);
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.span.start));
    let mut out = text.to_string();
    let mut floor = usize::MAX;
    let mut masked = false;
    for h in hits {
        let (Some(&s), Some(&e)) = (map.get(h.span.start), map.get(h.span.end)) else { continue };
        // Skip overlaps and anything that is not on a char boundary.
        if e > floor || s >= e || !out.is_char_boundary(s) || !out.is_char_boundary(e) {
            continue;
        }
        out.replace_range(s..e, &format!("[secret hidden by Jodd: {}]", h.mask_label));
        floor = s;
        masked = true;
    }
    (out, masked)
}

/// The line under the title, for `index.md`: the first non-empty line that is
/// not the title itself.
fn summary_line(title: &str, body: &str) -> String {
    let clean = |l: &str| l.trim().trim_start_matches('#').trim().trim_matches('*').trim().to_string();
    let t = clean(title);
    body.lines()
        .map(clean)
        .find(|l| !l.is_empty() && *l != t)
        .map(|l| cut(&one_line(&l), INDEX_LINE_CHARS))
        .unwrap_or_default()
}

#[derive(Serialize)]
struct SourceOut {
    id: String,
    resource: String,
}

#[derive(Serialize)]
struct JoddOut<'a> {
    uuid: &'a str,
    account: &'a str,
    folder: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
    trust: crate::provenance::Tier,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<String>,
}

/// The same OKF concept shape a backup writes (`type`, `title`, `tags`,
/// `sources`, then `jodd:`), with a slimmer `jodd:` block: the mirror is for
/// reading, not restoring (§5.2).
#[derive(Serialize)]
struct FrontmatterOut<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    title: &'a str,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    tags: &'a [String],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sources: Vec<SourceOut>,
    jodd: JoddOut<'a>,
}

fn modified_iso(note: &CachedNote) -> Option<String> {
    [Some(note.last_local_modified_at), note.last_remote_modified_at]
        .into_iter()
        .flatten()
        .find(|&ms| ms > 0)
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

struct RenderedNote {
    text: String,
    summary: String,
    masked: bool,
}

/// One note file. `attachments` are the links, relative to the note's own
/// directory.
fn render_note(account_id: &str, note: &CachedNote, read: &BackupRead, attachments: &[(String, String)]) -> Result<RenderedNote, String> {
    let (_, body) = crate::backup::body::encode_body(&note.body_html);
    let (mut body, body_masked) = mask_secrets(&body);
    let (title, title_masked) = mask_secrets(&note.title);
    if !attachments.is_empty() {
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("\n## Attachments\n\n");
        for (name, rel) in attachments {
            body.push_str(&format!("- [{}]({})\n", name.replace('[', "\\[").replace(']', "\\]"), link(rel)));
        }
    }
    let events = read.events.get(&note.uuid).map(Vec::as_slice).unwrap_or(&[]);
    let trust = crate::provenance::derive(events, note.local_version).tier;
    let tags = read.tags.get(&note.uuid).cloned().unwrap_or_default();
    let fm = FrontmatterOut {
        kind: "Note",
        title: &title,
        tags: &tags,
        sources: read
            .citations
            .get(&note.uuid)
            .map(|v| {
                v.iter()
                    .enumerate()
                    .map(|(i, r)| SourceOut { id: format!("s{}", i + 1), resource: r.clone() })
                    .collect()
            })
            .unwrap_or_default(),
        jodd: JoddOut {
            uuid: &note.uuid,
            account: account_id,
            folder: &note.label,
            modified: modified_iso(note),
            trust,
            attachments: attachments.iter().map(|(_, rel)| rel.clone()).collect(),
        },
    };
    let yaml = serde_yaml_ng::to_string(&fm).map_err(|e| format!("frontmatter: {e}"))?;
    let summary = summary_line(&title, &body);
    Ok(RenderedNote { text: format!("---\n{yaml}---\n{body}"), summary, masked: body_masked || title_masked })
}

/// Per-directory listing for `index.md`: subfolders and notes.
#[derive(Default)]
struct DirListing {
    subdirs: BTreeSet<String>,
    /// (file name, title, summary), in allocation order.
    notes: Vec<(String, String, String)>,
}

fn add_dir_chain(tree: &mut BTreeMap<String, DirListing>, dir: &str) {
    let mut cur = dir.to_string();
    tree.entry(cur.clone()).or_default();
    while let Some((parent, leaf)) = cur.rsplit_once('/') {
        tree.entry(parent.to_string()).or_default().subdirs.insert(leaf.to_string());
        cur = parent.to_string();
    }
}

fn render_dir_index(title: &str, listing: &DirListing) -> String {
    let mut entries: Vec<(String, String)> = listing
        .subdirs
        .iter()
        .map(|d| (format!("{d}/index.md"), format!("{d}/")))
        .collect();
    for (file, title, summary) in &listing.notes {
        let text = if summary.is_empty() { title.clone() } else { format!("{title} — {summary}") };
        entries.push((file.clone(), text));
    }
    crate::backup::okf::render_index(title, false, &entries)
}

const README: &str = "# Jodd for agents\n\n\
This folder is a read-only copy of your Jodd notes, written by Jodd for AI agents such as Claude Code.\n\
Start from `index.md`. Each note is one Markdown file; its `jodd:` block says which account and folder it is in, \
and `trust` says whether a person wrote or reviewed it.\n\n\
Do not edit these files: Jodd overwrites them at the next sync. To change a note, use Jodd itself, or Jodd's MCP \
tools (`create_note`, `update_note`, `remember`).\n\n\
Folders hidden from agents in Jodd are not here, and secrets are replaced by `[secret hidden by Jodd: …]`.\n";

/// What one account contributes: its planned files and its report.
struct AccountPlan {
    files: BTreeMap<String, Planned>,
    live: HashSet<String>,
    status: AccountStatus,
    root_entry: Option<(String, String)>,
}

fn plan_account(db: &Db, account: &Account, scope: &ScopeFile) -> Result<AccountPlan, String> {
    let read = db.backup_read(&account.id).map_err(|e| e.to_string())?;
    let slug = account_slug(&account.id);
    let mut files = BTreeMap::new();
    let mut status = AccountStatus { account_id: account.id.clone(), ..Default::default() };
    let live: HashSet<String> = read.notes.iter().map(|n| n.uuid.clone()).collect();

    let mut notes: Vec<&CachedNote> = read.notes.iter().filter(|n| !scope.is_hidden(&account.id, &n.label)).collect();
    // Sorted, so a run with no edits allocates the same names (no churn).
    notes.sort_by(|a, b| (&a.label, &a.title, &a.uuid).cmp(&(&b.label, &b.title, &b.uuid)));

    let mut alloc = NameAllocator::new();
    let mut tree: BTreeMap<String, DirListing> = BTreeMap::new();
    tree.entry(slug.clone()).or_default();
    for note in notes {
        let dir = format!("{slug}/{}", dir_for_folder(&note.label));
        let rel = alloc.file_for(&dir, &note.title);
        let file_name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        let stem = file_name.strip_suffix(".md").unwrap_or(&file_name).to_string();

        let mut links = Vec::new();
        let mut taken = HashSet::new();
        for a in read.attachments.get(&note.uuid).map(Vec::as_slice).unwrap_or(&[]) {
            let base = sanitize_segment(a.filename.as_deref().filter(|f| !f.is_empty()).unwrap_or(&a.content_id));
            let mut name = base.clone();
            let mut n = 1;
            while !taken.insert(name.to_lowercase()) {
                n += 1;
                name = format!("{n}-{base}");
            }
            let local = format!("{ATTACHMENTS_DIR}/{stem}/{name}");
            files.insert(
                format!("{dir}/{local}"),
                Planned {
                    entry: Entry {
                        account: Some(account.id.clone()),
                        uuid: None,
                        hash: format!("cid:{}/{}", note.uuid, a.content_id),
                    },
                    content: Content::Attachment { uuid: note.uuid.clone(), content_id: a.content_id.clone() },
                },
            );
            links.push((name, local));
        }

        let r = render_note(&account.id, note, &read, &links)?;
        if r.masked {
            status.masked_notes += 1;
        }
        add_dir_chain(&mut tree, &dir);
        tree.get_mut(&dir).expect("added above").notes.push((file_name, note.title.clone(), r.summary));
        files.insert(
            rel,
            Planned {
                entry: Entry { account: Some(account.id.clone()), uuid: Some(note.uuid.clone()), hash: sha256_hex(r.text.as_bytes()) },
                content: Content::Text(r.text),
            },
        );
        status.notes += 1;
    }

    for (dir, listing) in &tree {
        let title = if dir == &slug {
            account.email.clone()
        } else {
            dir.strip_prefix(&format!("{slug}/")).unwrap_or(dir).to_string()
        };
        let text = render_dir_index(&title, listing);
        files.insert(
            format!("{dir}/index.md"),
            Planned {
                entry: Entry { account: Some(account.id.clone()), uuid: None, hash: sha256_hex(text.as_bytes()) },
                content: Content::Text(text),
            },
        );
    }
    let root_entry = Some((format!("{slug}/index.md"), format!("{} — {} notes", account.email, status.notes)));
    Ok(AccountPlan { files, live, status, root_entry })
}

fn root_files(entries: &[(String, String)]) -> Vec<(String, Planned)> {
    let index = crate::backup::okf::render_index("Jodd notes", true, entries);
    [("README.md", README.to_string()), ("index.md", index)]
        .into_iter()
        .map(|(p, text)| {
            (
                p.to_string(),
                Planned { entry: Entry { account: None, uuid: None, hash: sha256_hex(text.as_bytes()) }, content: Content::Text(text) },
            )
        })
        .collect()
}

/// Bring the mirror at `root` up to date. The caller holds the lock.
pub fn sync_mirror(db: &Db, root: &Path, accounts: &[Account], scope: &ScopeFile, opts: &SyncOptions) -> Result<MirrorReport, String> {
    check_root(root)?;
    let (mut manifest, manifest_problem) = load_manifest(root);
    let may_delete = manifest_problem.is_none();
    let mut report = MirrorReport { problems: manifest_problem.into_iter().collect(), ..Default::default() };
    if may_delete && !manifest_path(root).exists() {
        // Written before any note, so a run cut short still leaves a folder
        // that `check_root` recognises as a mirror.
        save_manifest(root, &manifest)?;
    }

    let included: HashSet<&str> = scope.mirror().accounts.iter().map(String::as_str).collect();
    let mut planned: BTreeMap<String, Planned> = BTreeMap::new();
    // Accounts whose old files stay untouched this run.
    let mut kept: HashSet<String> = HashSet::new();
    // Accounts still mirrored: deletions there are checked by the hold-back rule.
    let mut live_by_account: HashMap<String, HashSet<String>> = HashMap::new();
    let mut statuses: Vec<AccountStatus> = Vec::new();
    let mut root_entries = Vec::new();

    for account in accounts.iter().filter(|a| a.is_active() && included.contains(a.id.as_str())) {
        let listed = db.full_listing_at(&account.id).map_err(|e| e.to_string())?;
        if listed.is_none() {
            kept.insert(account.id.clone());
            statuses.push(AccountStatus {
                account_id: account.id.clone(),
                skipped: Some("not synced yet — its previous files are kept".into()),
                ..Default::default()
            });
            continue;
        }
        match plan_account(db, account, scope) {
            Ok(p) => {
                planned.extend(p.files);
                live_by_account.insert(account.id.clone(), p.live);
                root_entries.extend(p.root_entry);
                statuses.push(p.status);
            }
            Err(e) => {
                kept.insert(account.id.clone());
                statuses.push(AccountStatus {
                    account_id: account.id.clone(),
                    skipped: Some(format!("could not read it ({e}) — its previous files are kept")),
                    ..Default::default()
                });
            }
        }
    }
    planned.extend(root_files(&root_entries));

    // Write what changed.
    let by_account = |statuses: &mut Vec<AccountStatus>, id: &Option<String>| -> Option<usize> {
        let id = id.as_deref()?;
        statuses.iter().position(|s| s.account_id == id)
    };
    let mut since_checkpoint = 0;
    report.finished = true;
    for (rel, p) in &planned {
        if opts.deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            report.finished = false;
            break;
        }
        let path = on_disk(root, rel);
        let old = manifest.files.get(rel);
        if old == Some(&p.entry) && path.is_file() {
            if let Content::Text(text) = &p.content {
                // Unchanged in Jodd. Overwrite only if someone edited the file.
                if std::fs::read(&path).map(|b| sha256_hex(&b) == p.entry.hash).unwrap_or(false) {
                    continue;
                }
                if let Some(i) = by_account(&mut statuses, &p.entry.account) {
                    statuses[i].overwritten_local_edits += 1;
                }
                write_file(&path, text.as_bytes(), true)?;
                continue;
            }
            continue;
        }
        let bytes: Vec<u8> = match &p.content {
            Content::Text(t) => t.clone().into_bytes(),
            Content::Attachment { uuid, content_id } => {
                let account = p.entry.account.as_deref().unwrap_or_default();
                match db.attachment_bytes(account, uuid, content_id).map_err(|e| e.to_string())? {
                    Some(b) if b.len() <= MAX_ATTACHMENT_BYTES => b,
                    _ => {
                        if let Some(i) = by_account(&mut statuses, &p.entry.account) {
                            statuses[i].skipped_attachments += 1;
                        }
                        continue;
                    }
                }
            }
        };
        write_file(&path, &bytes, true)?;
        manifest.files.insert(rel.clone(), p.entry.clone());
        if let Some(i) = by_account(&mut statuses, &p.entry.account) {
            statuses[i].written += 1;
        }
        since_checkpoint += 1;
        if since_checkpoint >= CHECKPOINT_EVERY {
            save_manifest(root, &manifest)?;
            since_checkpoint = 0;
        }
    }

    // Delete what Jodd wrote before and no longer plans. Never on a run the
    // deadline cut short (the plan was not carried out), never without a
    // manifest Jodd could read.
    if report.finished && may_delete {
        let gone: Vec<(String, Entry)> = manifest
            .files
            .iter()
            .filter(|(rel, e)| !planned.contains_key(*rel) && !e.account.as_ref().is_some_and(|a| kept.contains(a)))
            .map(|(r, e)| (r.clone(), e.clone()))
            .collect();
        let held = held_back_accounts(&manifest, &gone, &live_by_account, opts.allow_large_delete);
        for (rel, e) in gone {
            if e.account.as_ref().is_some_and(|a| held.contains(a)) {
                continue;
            }
            remove_file(root, &rel)?;
            manifest.files.remove(&rel);
            if let Some(i) = by_account(&mut statuses, &e.account) {
                if e.uuid.is_some() {
                    statuses[i].deleted += 1;
                }
            }
        }
        for s in statuses.iter_mut() {
            s.held_back = held.contains(&s.account_id);
        }
        if !held.is_empty() {
            report.problems.push("Many notes disappeared at once, so the mirror kept their files. Use Sync now › Apply anyway if that was intended.".into());
        }
    }
    save_manifest(root, &manifest)?;
    report.accounts = statuses;
    Ok(report)
}

/// §4.3, counting only notes that vanished from an account that is still
/// mirrored. Hiding a folder, renaming a note or turning an account off are
/// intended removals and never held back.
fn held_back_accounts(
    manifest: &Manifest,
    gone: &[(String, Entry)],
    live_by_account: &HashMap<String, HashSet<String>>,
    allow: bool,
) -> HashSet<String> {
    if allow {
        return HashSet::new();
    }
    let mut held = HashSet::new();
    for (account, live) in live_by_account {
        let before = manifest
            .files
            .values()
            .filter(|e| e.uuid.is_some() && e.account.as_ref() == Some(account))
            .count();
        let vanished = gone
            .iter()
            .filter(|(_, e)| e.account.as_ref() == Some(account))
            .filter(|(_, e)| e.uuid.as_ref().is_some_and(|u| !live.contains(u)))
            .count();
        if vanished > HOLD_BACK_MIN && vanished * 2 > before {
            held.insert(account.clone());
        }
    }
    held
}

/// Remove one file Jodd wrote, then any directory that leaves empty, up to
/// (not including) the root.
fn remove_file(root: &Path, rel: &str) -> Result<(), String> {
    let path = on_disk(root, rel);
    if path.exists() {
        set_read_only(&path, false);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("remove {}: {e}", path.display())),
        }
    }
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// Each mirrored note's file, keyed by `(account id, uuid)`, from the
/// manifest — so `start_project` can point an agent at the whole file
/// (spec §16.2). Empty when there is no readable mirror at `root`.
pub fn note_files(root: &Path) -> HashMap<(String, String), PathBuf> {
    let (m, problem) = load_manifest(root);
    if problem.is_some() {
        return HashMap::new();
    }
    m.files
        .iter()
        .filter_map(|(rel, e)| Some(((e.account.clone()?, e.uuid.clone()?), on_disk(root, rel))))
        .collect()
}

/// Remove every file in the manifest, then the manifest. `Ok(0)` for a folder
/// that is not a mirror.
pub fn remove_all(root: &Path) -> Result<usize, String> {
    if !manifest_path(root).is_file() {
        return Ok(0);
    }
    let (manifest, problem) = load_manifest(root);
    if let Some(p) = problem {
        return Err(p);
    }
    for rel in manifest.files.keys() {
        remove_file(root, rel)?;
    }
    let _ = std::fs::remove_file(manifest_path(root));
    Ok(manifest.files.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Account;
    use crate::db::RemotePin;
    use crate::mcp_scope::{AgentFilesScope, MirrorScope};

    const ACCT: &str = "gmail:a@x.com";

    fn account(id: &str) -> Account {
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","email":"{}","added_at":"2026-01-01T00:00:00Z","backend_kind":"gmail"}}"#,
            id.split(':').nth(1).unwrap()
        ))
        .unwrap()
    }

    fn scope(accounts: &[&str], hidden: &[&str]) -> ScopeFile {
        let mut s = ScopeFile {
            agent_files: AgentFilesScope {
                mirror: MirrorScope { enabled: true, path: None, accounts: accounts.iter().map(|a| a.to_string()).collect() },
                ..Default::default()
            },
            ..Default::default()
        };
        if !hidden.is_empty() {
            s.accounts.insert(
                ACCT.into(),
                serde_json::from_value(serde_json::json!({ "hidden_folders": hidden })).unwrap(),
            );
        }
        s
    }

    fn opts() -> SyncOptions {
        SyncOptions { allow_large_delete: false, deadline: None, ..Default::default() }
    }

    fn put(db: &Db, uuid: &str, label: &str, title: &str, body: &str) {
        let n = crate::test_support::note(ACCT, uuid).label(label).title(title).body(body).build();
        db.upsert_from_remote(&n, RemotePin::RemoteWins).unwrap();
    }

    fn seeded(n: usize) -> Db {
        let db = crate::test_support::temp_db();
        for i in 0..n {
            put(&db, &format!("u{i}"), "Notes/ERP", &format!("Note {i}"), &format!("line {i}"));
        }
        db.record_full_listing(ACCT).unwrap();
        db
    }

    fn files_under(root: &Path) -> BTreeSet<String> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeSet<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(base, &p, out);
                } else {
                    out.insert(p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let mut out = BTreeSet::new();
        walk(root, root, &mut out);
        out
    }

    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(on_disk(root, rel)).unwrap()
    }

    #[test]
    fn mirrors_notes_with_indexes_and_a_second_run_writes_nothing() {
        let db = seeded(2);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        let r = sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        assert!(r.finished);
        assert_eq!(r.accounts[0].notes, 2);
        let slug = account_slug(ACCT);
        let note = read(&root, &format!("{slug}/Notes/ERP/Note 0.md"));
        assert!(note.starts_with("---\ntype: Note\ntitle: Note 0\n"), "{note}");
        assert!(note.contains(&format!("account: {ACCT}")) && note.contains("folder: Notes/ERP"));
        assert!(note.contains("trust: "));
        let idx = read(&root, &format!("{slug}/Notes/ERP/index.md"));
        assert!(idx.contains("[Note 0 — line 0](Note%200.md)"), "{idx}");
        assert!(read(&root, &format!("{slug}/Notes/index.md")).contains("(ERP/index.md)"));
        assert!(read(&root, "index.md").contains(&format!("({slug}/index.md)")));
        assert!(read(&root, "README.md").contains("read-only"));

        let again = sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        assert_eq!(again.accounts[0].written, 0, "no edits → no writes");
        assert_eq!(again.accounts[0].deleted, 0);
    }

    #[test]
    fn hidden_folders_never_reach_disk_and_hiding_later_deletes_them() {
        let db = seeded(1);
        put(&db, "p1", "Notes/Personal", "Diary", "secret life");
        put(&db, "p2", "Notes/Personal/Deep", "Deeper", "deeper");
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &["Notes/Personal"]), &opts()).unwrap();
        let all = files_under(&root);
        assert!(!all.iter().any(|f| f.contains("Personal") || f.contains("Diary")), "{all:?}");

        let open = tempfile::tempdir().unwrap();
        let root2 = open.path().join("m");
        sync_mirror(&db, &root2, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        assert!(files_under(&root2).iter().any(|f| f.ends_with("Personal/Diary.md")));
        let r = sync_mirror(&db, &root2, &[account(ACCT)], &scope(&[ACCT], &["Notes/Personal"]), &opts()).unwrap();
        assert_eq!(r.accounts[0].deleted, 2);
        assert!(!files_under(&root2).iter().any(|f| f.contains("Personal")), "hiding removes the files and the folder");
    }

    #[test]
    fn inactive_and_not_included_accounts_are_left_out_and_removed() {
        let db = seeded(1);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        let r = sync_mirror(&db, &root, &[account(ACCT)], &scope(&[], &[]), &opts()).unwrap();
        assert!(r.accounts.is_empty(), "opt-in: no account chosen, nothing mirrored");

        sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        let mut inactive = account(ACCT);
        inactive.status = crate::accounts::AccountStatus::Inactive;
        sync_mirror(&db, &root, &[inactive], &scope(&[ACCT], &[]), &opts()).unwrap();
        assert_eq!(files_under(&root).into_iter().filter(|f| !f.starts_with(".jodd")).collect::<Vec<_>>(), ["README.md", "index.md"]);
    }

    #[test]
    fn an_account_never_fully_listed_keeps_its_previous_files() {
        let db = seeded(3);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        let before = files_under(&root);
        // A fresh cache for the same account: no listing mark, no notes.
        let empty = crate::test_support::temp_db();
        let r = sync_mirror(&empty, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        assert!(r.accounts[0].skipped.is_some());
        let after = files_under(&root);
        assert!(before.iter().filter(|f| f.ends_with(".md") && f.contains("ERP")).all(|f| after.contains(f)));
    }

    #[test]
    fn many_notes_vanishing_at_once_are_held_back_until_allowed() {
        let db = seeded(30);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        let fewer = crate::test_support::temp_db();
        put(&fewer, "u0", "Notes/ERP", "Note 0", "line 0");
        fewer.record_full_listing(ACCT).unwrap();
        let r = sync_mirror(&fewer, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        assert!(r.accounts[0].held_back);
        assert_eq!(r.accounts[0].deleted, 0);
        assert!(on_disk(&root, &format!("{}/Notes/ERP/Note 29.md", account_slug(ACCT))).exists());
        let ok = sync_mirror(&fewer, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &SyncOptions { allow_large_delete: true, deadline: None, ..Default::default() }).unwrap();
        assert!(!ok.accounts[0].held_back);
        assert_eq!(ok.accounts[0].deleted, 29);
    }

    #[test]
    fn a_folder_with_other_files_is_refused_and_foreign_files_survive() {
        let db = seeded(1);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mine.txt"), "keep").unwrap();
        assert!(sync_mirror(&db, dir.path(), &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).is_err());
        assert_eq!(std::fs::read_to_string(dir.path().join("mine.txt")).unwrap(), "keep");

        let root = dir.path().join("m");
        sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        let foreign = on_disk(&root, &format!("{}/Notes/ERP/mine.txt", account_slug(ACCT)));
        std::fs::write(&foreign, "keep").unwrap();
        sync_mirror(&db, &root, &[account(ACCT)], &scope(&[], &[]), &opts()).unwrap();
        assert!(foreign.exists(), "only manifest paths are deleted");
        assert!(remove_all(&root).unwrap() > 0);
        assert!(foreign.exists());
        assert!(!manifest_path(&root).exists());
    }

    #[test]
    fn secrets_are_masked_in_the_file() {
        let db = seeded(0);
        put(&db, "s1", "Notes/Ops", "Server", &format!("api_key = {}", crate::agent_files::FAKE_STRIPE_KEY));
        db.record_full_listing(ACCT).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        let r = sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        let text = read(&root, &format!("{}/Notes/Ops/Server.md", account_slug(ACCT)));
        assert!(!text.contains(crate::agent_files::FAKE_STRIPE_KEY), "{text}");
        assert!(text.contains("[secret hidden by Jodd:"), "{text}");
        assert_eq!(r.accounts[0].masked_notes, 1);
    }

    #[test]
    fn a_hand_edit_is_overwritten_and_counted() {
        let db = seeded(1);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        let p = on_disk(&root, &format!("{}/Notes/ERP/Note 0.md", account_slug(ACCT)));
        set_read_only(&p, false);
        std::fs::write(&p, "edited").unwrap();
        let r = sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        assert_eq!(r.accounts[0].overwritten_local_edits, 1);
        assert!(std::fs::read_to_string(&p).unwrap().contains("title: Note 0"));
        assert!(std::fs::metadata(&p).unwrap().permissions().readonly());
    }

    #[test]
    fn attachments_are_written_beside_the_note_and_oversized_ones_skipped() {
        let db = seeded(1);
        let att = |cid: &str, name: &str, data: Vec<u8>| crate::backend::gmail::wire::Attachment {
            content_id: cid.into(),
            mime_type: "image/png".into(),
            filename: Some(name.into()),
            x_apple_part_url: None,
            data,
        };
        db.upsert_attachment(ACCT, "u0", &att("c1", "logo.png", vec![1, 2, 3])).unwrap();
        db.upsert_attachment(ACCT, "u0", &att("c2", "huge.bin", vec![0; MAX_ATTACHMENT_BYTES + 1])).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        let r = sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        let slug = account_slug(ACCT);
        assert_eq!(std::fs::read(on_disk(&root, &format!("{slug}/Notes/ERP/_attachments/Note 0/logo.png"))).unwrap(), [1, 2, 3]);
        assert!(!on_disk(&root, &format!("{slug}/Notes/ERP/_attachments/Note 0/huge.bin")).exists());
        assert_eq!(r.accounts[0].skipped_attachments, 1);
        assert!(read(&root, &format!("{slug}/Notes/ERP/Note 0.md")).contains("(_attachments/Note%200/logo.png)"));
    }

    #[test]
    fn a_past_deadline_writes_nothing_and_deletes_nothing() {
        let db = seeded(2);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("m");
        let past = SyncOptions { allow_large_delete: false, deadline: Some(std::time::Instant::now()), ..Default::default() };
        let r = sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &past).unwrap();
        assert!(!r.finished);
        assert!(manifest_path(&root).exists(), "the folder is recognisable as a mirror");
        let r2 = sync_mirror(&db, &root, &[account(ACCT)], &scope(&[ACCT], &[]), &opts()).unwrap();
        assert!(r2.finished && r2.accounts[0].written > 0, "the next run continues");
    }

    #[test]
    fn masking_sees_through_markdown_escapes_and_keeps_offsets_right() {
        let (out, masked) = mask_secrets("ก่อน api\\_key = sk\\_live\\_51HxYzABCDEF0123456789abcd หลัง");
        assert!(masked);
        assert!(out.starts_with("ก่อน ") && out.ends_with(" หลัง"), "{out}");
        assert!(!out.contains("51HxYz"), "{out}");
    }

    #[test]
    fn mask_secrets_leaves_plain_text_alone() {
        assert_eq!(mask_secrets("FIFO costs the oldest stock first."), ("FIFO costs the oldest stock first.".into(), false));
    }
}
