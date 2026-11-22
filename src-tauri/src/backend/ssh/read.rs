//! The SSH vertical's read path, plus `cas_write`, which adoption needs.

use std::collections::{HashMap, HashSet};

use super::scripts::{self, FileEntry, Header, Listing};
use super::{apple_date_from_unix, canon_uuid, file, iso_from_unix, iso_to_apple, label_of, parent_dir, SshVertical};
use crate::backend::{DedupSummary, Note, TransportError, TrashedNote};

const READ_BATCH: usize = 50;

impl SshVertical {
    pub(crate) async fn listing(&self, folder: Option<&str>) -> Result<Listing, TransportError> {
        let f = self.flavor().await?;
        let l = scripts::parse_listing(&self.exec(&scripts::list(&self.root, f, folder)).await?);
        if l.skipped > 0 {
            crate::log!("ssh: {} listing line(s) skipped under {} (newline or backslash in a file name)", l.skipped, self.root);
        }
        if l.incomplete {
            crate::log!("ssh: find could not read part of {} — listing incomplete, no prune this round", self.root);
        }
        Ok(l)
    }

    pub(crate) async fn read_files(&self, paths: &[String]) -> Result<HashMap<String, Option<Vec<u8>>>, TransportError> {
        let mut out = HashMap::new();
        for chunk in paths.chunks(READ_BATCH) {
            out.extend(scripts::parse_read_batch(&self.exec(&scripts::read_batch(&self.root, chunk)).await?));
        }
        Ok(out)
    }

    pub(crate) async fn headers(&self) -> Result<Vec<Header>, TransportError> {
        Ok(scripts::parse_headers(&self.exec(&scripts::header_scan(&self.root)).await?))
    }

    /// Returns the new sha. `Conflict` if the file is no longer `expected`.
    pub(crate) async fn cas_write(&self, path: &str, expected: &str, content: &[u8]) -> Result<String, TransportError> {
        let f = self.flavor().await?;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let new_sha = scripts::sha256_hex(content);
        Ok(self.exec(&scripts::cas_write(&self.root, f, path, expected, content, &new_sha, &nonce)).await?.trim().to_string())
    }

    fn build_note(path: &str, sha: &str, mtime: i64, d: &file::Decoded, uuid: &str) -> Note {
        Note {
            id: path.to_string(),
            uuid: uuid.to_string(),
            title: d.title.clone(),
            body_html: d.body_html.clone(),
            date: apple_date_from_unix(mtime),
            version: sha.to_string(),
            label: label_of(path),
            x_mail_created_date: d.fm.as_ref().and_then(|f| f.created.as_deref()).and_then(iso_to_apple),
            account_id: None,
            pinned: d.fm.as_ref().is_some_and(|f| f.pinned),
            local_version: 0,
            push_blocked_reason: None,
            push_blocked_by_remote: false,
            render_loss: None,
            attachments: Vec::new(),
        }
    }

    /// Give a file with no uuid one, in place: frontmatter prepended or
    /// completed, body bytes untouched, file name kept (spec Q7-A). `None`
    /// when the file changed underneath — the next round tries again.
    async fn adopt(&self, path: &str, entry: &FileEntry, bytes: &[u8], d: &file::Decoded) -> Result<Option<(String, String)>, TransportError> {
        let uuid = uuid::Uuid::new_v4().to_string();
        let title = d.title.clone();
        let created = iso_from_unix(entry.mtime);
        let content = file::with_frontmatter(bytes, |fm| {
            fm.uuid = Some(uuid.clone());
            fm.title.get_or_insert(title);
            fm.created.get_or_insert(created);
        });
        match self.cas_write(path, &entry.sha, &content).await {
            Ok(sha) => Ok(Some((uuid, sha))),
            Err(TransportError::Conflict { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Notes for every file in `listing`, reusing `cache` (keyed by remote
    /// id) wherever the sha is unchanged. One uuid in several files keeps
    /// the newest (ties: the smaller path) and is reported as a dup.
    ///
    /// Every listed file it cannot turn into a note (gone before its read,
    /// not decodable, not adoptable) marks the summary `incomplete`, so the
    /// caller does not prune that note's row on a round that simply missed
    /// it (gotcha #43). The price: while one file stays unadoptable (a
    /// directory Jodd cannot write), listings that include it never prune.
    pub(crate) async fn notes_from(&self, listing: &Listing, cache: &HashMap<String, Note>) -> Result<(Vec<Note>, DedupSummary), TransportError> {
        let mut found: Vec<(i64, Note)> = Vec::new();
        let mut fetch: Vec<String> = Vec::new();
        for (path, entry) in &listing.files {
            match cache.get(path) {
                Some(c) if c.version == entry.sha => {
                    let mut n = c.clone();
                    n.label = label_of(path);
                    found.push((entry.mtime, n));
                }
                _ => fetch.push(path.clone()),
            }
        }
        let mut incomplete = listing.incomplete;
        let bodies = self.read_files(&fetch).await?;
        for path in &fetch {
            let (Some(Some(bytes)), Some(entry)) = (bodies.get(path), listing.files.get(path)) else {
                crate::log!("ssh: {path} was listed but gone before its read — listing incomplete");
                incomplete = true;
                continue;
            };
            let Some(d) = file::decode(path, bytes) else {
                incomplete = true;
                continue;
            };
            let known = d.fm.as_ref().and_then(|f| f.uuid.as_deref()).and_then(canon_uuid);
            let (uuid, sha) = match known {
                Some(u) => (u, entry.sha.clone()),
                // Any failure — not only a lost race — skips this one file
                // for the round: a file in a directory Jodd cannot write
                // must not hide every other note in the vault.
                None => match self.adopt(path, entry, bytes, &d).await {
                    Ok(Some(adopted)) => adopted,
                    Ok(None) => {
                        incomplete = true;
                        continue;
                    }
                    Err(e) => {
                        crate::log!("ssh: could not adopt {path}, skipping it this round: {e}");
                        incomplete = true;
                        continue;
                    }
                },
            };
            found.push((entry.mtime, Self::build_note(path, &sha, entry.mtime, &d, &uuid)));
        }

        let mut best: HashMap<String, (i64, Note)> = HashMap::new();
        let mut dup_uuids: HashSet<String> = HashSet::new();
        let mut collapsed = 0;
        for (mtime, note) in found {
            match best.get(&note.uuid) {
                Some((m, kept)) => {
                    collapsed += 1;
                    dup_uuids.insert(note.uuid.clone());
                    if mtime > *m || (mtime == *m && note.id < kept.id) {
                        best.insert(note.uuid.clone(), (mtime, note));
                    }
                }
                None => {
                    best.insert(note.uuid.clone(), (mtime, note));
                }
            }
        }
        let mut notes: Vec<Note> = best.into_values().map(|(_, n)| n).collect();
        notes.sort_by(|a, b| a.id.cmp(&b.id));
        Ok((notes, DedupSummary { collapsed, uuids_affected: dup_uuids.len(), incomplete, ..Default::default() }))
    }

    pub(crate) async fn fetch_one(&self, remote_id: &str) -> Result<Note, TransportError> {
        let listing = self.listing(Some(&parent_dir(remote_id))).await?;
        let Some(entry) = listing.files.get(remote_id).cloned() else { return Err(TransportError::NotFound) };
        let one = Listing { files: [(remote_id.to_string(), entry)].into_iter().collect(), ..Listing::default() };
        let (notes, _) = self.notes_from(&one, &HashMap::new()).await?;
        notes.into_iter().next().ok_or(TransportError::NotFound)
    }

    pub(crate) async fn ids_for_uuid(&self, uuid: &str) -> Result<Vec<String>, TransportError> {
        let Some(want) = canon_uuid(uuid) else { return Ok(Vec::new()) };
        Ok(self
            .headers()
            .await?
            .into_iter()
            .filter(|h| h.uuid.as_deref().and_then(canon_uuid).as_deref() == Some(want.as_str()))
            .map(|h| h.path)
            .collect())
    }

    pub(crate) async fn trashed(&self) -> Result<Vec<TrashedNote>, TransportError> {
        let listed = scripts::parse_read_batch(&self.exec(&scripts::list_trash(&self.root)).await?);
        let mut out = Vec::new();
        for (name, bytes) in listed {
            let Some(bytes) = bytes else { continue };
            let original = super::trash_original(&name);
            let Some(d) = file::decode(&original, &bytes) else { continue };
            out.push(TrashedNote {
                id: format!(".jodd/trash/{name}"),
                uuid: d.fm.as_ref().and_then(|f| f.uuid.as_deref()).and_then(canon_uuid).unwrap_or_default(),
                title: d.title,
                date: String::new(),
                label: label_of(&original),
                original_known: true,
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    pub(crate) fn vault() -> (tempfile::TempDir, SshVertical) {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap().to_str().unwrap().to_string();
        let s = session::ProcessSession::new(session::SpawnSpec::local_sh(d.path().to_path_buf()), Duration::from_secs(20));
        let v = SshVertical::new(Arc::new(s), "local".into(), root, "ssh:test".into());
        (d, v)
    }

    /// chmod 555 on a directory, restored to 755 on drop — also when the
    /// test panics. `None` when the chmod does not actually stop writes (the
    /// test runs as root), so the caller skips rather than asserting a
    /// failure that cannot happen.
    pub(crate) struct ReadOnlyDir(std::path::PathBuf);

    impl ReadOnlyDir {
        pub(crate) fn new(dir: &std::path::Path) -> Option<ReadOnlyDir> {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            let guard = ReadOnlyDir(dir.to_path_buf());
            let probe = dir.join(".jodd-write-probe");
            if std::fs::write(&probe, "").is_ok() {
                let _ = std::fs::remove_file(&probe);
                eprintln!("skipping: chmod 555 does not stop writes here (running as root?)");
                return None;
            }
            Some(guard)
        }
    }

    impl Drop for ReadOnlyDir {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    pub(crate) fn write(d: &tempfile::TempDir, rel: &str, text: &str) {
        let p = d.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    const JODD_FILE: &str = "---\nuuid: 11111111-2222-4333-8444-555555555555\ntitle: \"Plan\"\ncreated: 2026-09-01T09:00:00+07:00\npinned: true\n---\n# Plan\n\n- [ ] one\n";

    #[test]
    fn path_helpers() {
        assert_eq!(label_of("Notes/Work/a.md"), "Notes/Work");
        assert_eq!(label_of("Notes/a.md"), "Notes");
        assert_eq!(dir_of_label("Notes/Work"), "Notes/Work");
        assert_eq!(dir_of_label("Notes"), "Notes");
        assert_eq!(dir_of_label("Work"), "Notes/Work");
        assert_eq!(parent_dir("Notes/Work/a.md"), "Notes/Work");
        assert_eq!(slugify("Meeting notes: Q3!"), "meeting-notes-q3");
        assert_eq!(slugify("ประชุม ทีม"), "ประชุม-ทีม");
        assert_eq!(slugify("..."), "untitled");
        assert_eq!(slugify(".hidden"), "hidden");
        // A byte budget (a 60-character Thai slug's worth), cut at a word
        // boundary: an English title no longer loses its end mid-number
        // while a Thai one of the same byte length kept it whole.
        assert_eq!(
            slugify("my first note (with title updated) (conflict from Mac 2026-09-26 14:54)"),
            "my-first-note-with-title-updated-conflict-from-mac-2026-09-26-14-54"
        );
        let long = slugify(&"word ".repeat(60));
        assert!(long.len() <= 180 && long.ends_with("word") && !long.ends_with('-'), "{long}");
        assert_eq!(slugify(&"x".repeat(200)).len(), 180); // no boundary: hard cut
        let thai = slugify(&"ก".repeat(100));
        assert_eq!(thai.chars().count(), 60); // Thai keeps exactly its old room
        let wide = slugify(&"𝐀".repeat(100)); // 4-byte alphanumerics
        assert!(wide.len() <= 180 && wide.is_char_boundary(wide.len()), "{}", wide.len());
        assert_eq!(canon_uuid("11111111-2222-4333-8444-555555555555".to_uppercase().as_str()).as_deref(), Some("11111111-2222-4333-8444-555555555555"));
        assert_eq!(canon_uuid("nope"), None);
        let t = trash_name("Notes/a%b/x.md");
        assert_eq!((t.len(), &t[32..33]), (33 + "Notes%2Fa%25b%2Fx.md".len(), "-"));
        assert_eq!(trash_original(&t), "Notes/a%b/x.md");
        assert_eq!(trash_original("Notes%2Fhand-moved.md"), "Notes/hand-moved.md");
        assert_eq!(apple_to_iso(&iso_to_apple("2026-09-01T09:00:00+07:00").unwrap()).unwrap(), "2026-09-01T09:00:00+07:00");
    }

    #[tokio::test]
    async fn a_jodd_file_reads_back_as_a_note() {
        let (d, v) = vault();
        write(&d, "Notes/Work/plan.md", JODD_FILE);
        let (notes, dedup) = v.notes_from(&v.listing(None).await.unwrap(), &HashMap::new()).await.unwrap();
        assert_eq!(dedup.collapsed, 0);
        let n = &notes[0];
        assert_eq!((n.id.as_str(), n.label.as_str(), n.title.as_str()), ("Notes/Work/plan.md", "Notes/Work", "Plan"));
        assert_eq!(n.uuid, "11111111-2222-4333-8444-555555555555");
        assert!(n.pinned);
        assert_eq!(n.version, scripts::sha256_hex(JODD_FILE.as_bytes()));
        assert!(n.body_html.contains("type=\"checkbox\""), "{}", n.body_html);
        assert!(n.x_mail_created_date.as_deref().unwrap().contains("2026"));
    }

    /// Spec Q7-A: a file a program dropped gets a uuid written into it, and
    /// keeps its name.
    #[tokio::test]
    async fn a_file_without_frontmatter_is_adopted_in_place() {
        let (d, v) = vault();
        write(&d, "Notes/Inbox/summary.md", "# Summary\n\n- [ ] do it\n");
        let (notes, _) = v.notes_from(&v.listing(None).await.unwrap(), &HashMap::new()).await.unwrap();
        let n = &notes[0];
        assert_eq!(n.id, "Notes/Inbox/summary.md");
        let on_disk = std::fs::read_to_string(d.path().join("Notes/Inbox/summary.md")).unwrap();
        assert!(on_disk.starts_with(&format!("---\nuuid: {}\n", n.uuid)), "{on_disk}");
        assert!(on_disk.ends_with("# Summary\n\n- [ ] do it\n"), "body bytes kept: {on_disk}");
        assert_eq!(n.version, scripts::sha256_hex(on_disk.as_bytes()));
    }

    /// A cached note whose sha still matches is reused without a read. The
    /// sentinel title proves the cache was the source.
    #[tokio::test]
    async fn an_unchanged_file_reuses_the_cached_note() {
        let (d, v) = vault();
        write(&d, "Notes/plan.md", JODD_FILE);
        let (mut notes, _) = v.notes_from(&v.listing(None).await.unwrap(), &HashMap::new()).await.unwrap();
        let mut cached = notes.remove(0);
        cached.title = "FROM CACHE".into();
        let cache = HashMap::from([(cached.id.clone(), cached)]);
        let (again, _) = v.notes_from(&v.listing(None).await.unwrap(), &cache).await.unwrap();
        assert_eq!(again[0].title, "FROM CACHE");
    }

    #[tokio::test]
    async fn two_files_with_one_uuid_collapse_to_the_newest() {
        let (d, v) = vault();
        write(&d, "Notes/a.md", JODD_FILE);
        write(&d, "Notes/b.md", &JODD_FILE.replace("- [ ] one", "- [ ] newer"));
        let later = std::time::SystemTime::now() + Duration::from_secs(60);
        std::fs::File::options().write(true).open(d.path().join("Notes/b.md")).unwrap().set_modified(later).unwrap();
        let (notes, dedup) = v.notes_from(&v.listing(None).await.unwrap(), &HashMap::new()).await.unwrap();
        assert_eq!((notes.len(), dedup.collapsed, dedup.uuids_affected), (1, 1, 1));
        assert_eq!(notes[0].id, "Notes/b.md");
    }

    #[tokio::test]
    async fn fetch_and_lookup_by_uuid() {
        let (d, v) = vault();
        write(&d, "Notes/Work/plan.md", JODD_FILE);
        assert_eq!(v.fetch_one("Notes/Work/plan.md").await.unwrap().title, "Plan");
        assert!(matches!(v.fetch_one("Notes/Work/gone.md").await, Err(TransportError::NotFound)));
        let ids = v.ids_for_uuid("11111111-2222-4333-8444-555555555555".to_uppercase().as_str()).await.unwrap();
        assert_eq!(ids, vec!["Notes/Work/plan.md"]);
    }

    /// I-5: one file Jodd cannot adopt must not hide every other note.
    #[tokio::test]
    async fn an_unadoptable_file_is_skipped_and_the_listing_continues() {
        let (d, v) = vault();
        write(&d, "Notes/plan.md", JODD_FILE);
        write(&d, "Notes/Locked/raw.md", "# Raw\n");
        let Some(_ro) = ReadOnlyDir::new(&d.path().join("Notes/Locked")) else { return };
        let (notes, dedup) = v.notes_from(&v.listing(None).await.unwrap(), &HashMap::new()).await.unwrap();
        assert_eq!(notes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec!["Notes/plan.md"]);
        // Gotcha #43: a listing that skipped a file is not a picture of the
        // vault, so the caller must not prune on it.
        assert!(dedup.incomplete, "a skipped file must mark the listing incomplete");
    }

    /// The control for every `incomplete` assertion here: a listing that read
    /// everything says so — and a file that is not a note (`.txt`) is ignored
    /// on purpose, not skipped.
    #[tokio::test]
    async fn a_listing_that_read_everything_is_complete() {
        let (d, v) = vault();
        write(&d, "Notes/plan.md", JODD_FILE);
        write(&d, "Notes/raw.md", "# Raw\n");
        write(&d, "Notes/keep.txt", "not a note");
        let listing = v.listing(None).await.unwrap();
        assert!(!listing.incomplete);
        let (notes, dedup) = v.notes_from(&listing, &HashMap::new()).await.unwrap();
        assert_eq!(notes.len(), 2);
        assert!(!dedup.incomplete);
    }

    /// Another device trashed or moved the file between the listing and the
    /// read. Its row is not gone from the vault, only from this round.
    #[tokio::test]
    async fn a_file_gone_before_its_read_marks_the_listing_incomplete() {
        let (d, v) = vault();
        write(&d, "Notes/plan.md", JODD_FILE);
        write(&d, "Notes/other.md", &JODD_FILE.replace("11111111", "22222222"));
        let listing = v.listing(None).await.unwrap();
        std::fs::rename(d.path().join("Notes/plan.md"), d.path().join("Notes/plan.html")).unwrap();
        let (notes, dedup) = v.notes_from(&listing, &HashMap::new()).await.unwrap();
        assert_eq!(notes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec!["Notes/other.md"]);
        assert!(dedup.incomplete);
    }

    /// `find` that cannot read a directory prints what it could, complains on
    /// stderr and exits 1 — but the script's status was the last `sed`'s, 0,
    /// so the missing subtree read as deleted.
    #[tokio::test]
    async fn an_unreadable_directory_marks_the_listing_incomplete() {
        let (d, v) = vault();
        write(&d, "Notes/plan.md", JODD_FILE);
        write(&d, "Notes/Locked/hidden.md", &JODD_FILE.replace("11111111", "33333333"));
        let Some(_nr) = UnreadableDir::new(&d.path().join("Notes/Locked")) else { return };
        let listing = v.listing(None).await.unwrap();
        assert!(listing.files.contains_key("Notes/plan.md"), "the readable part is still listed");
        assert!(!listing.files.contains_key("Notes/Locked/hidden.md"));
        assert!(listing.incomplete, "the unreadable subtree must not read as deleted");
        let (_, dedup) = v.notes_from(&listing, &HashMap::new()).await.unwrap();
        assert!(dedup.incomplete);
    }

    /// The folder open prunes too (gotcha #35), so its listing must carry
    /// the same flag — `list_notes_in_folder` threw the summary away.
    #[tokio::test]
    async fn a_folder_listing_reports_a_skipped_file() {
        let (d, v) = vault();
        write(&d, "Notes/Locked/raw.md", "# Raw\n");
        write(&d, "Notes/Locked/plan.md", JODD_FILE);
        let Some(_ro) = ReadOnlyDir::new(&d.path().join("Notes/Locked")) else { return };
        let (notes, dedup) = crate::backend::NoteStore::list_folder_with_divergent(&v, "Notes/Locked", &HashMap::new()).await.unwrap();
        assert_eq!(notes.len(), 1);
        assert!(dedup.incomplete);
    }

    /// chmod 000 on a directory, restored on drop. `None` when it does not
    /// stop reads (root).
    pub(crate) struct UnreadableDir(std::path::PathBuf);

    impl UnreadableDir {
        pub(crate) fn new(dir: &std::path::Path) -> Option<UnreadableDir> {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
            let guard = UnreadableDir(dir.to_path_buf());
            if std::fs::read_dir(dir).is_ok() {
                eprintln!("skipping: chmod 000 does not stop reads here (running as root?)");
                return None;
            }
            Some(guard)
        }
    }

    impl Drop for UnreadableDir {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    #[tokio::test]
    async fn a_trashed_uuid_is_canonical() {
        let (d, v) = vault();
        write(&d, "Notes/plan.md", &JODD_FILE.replace("11111111-2222-4333-8444-555555555555", "AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE"));
        v.trash_note("Notes/plan.md", None).await.unwrap();
        assert_eq!(v.trashed().await.unwrap()[0].uuid, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
    }

    #[tokio::test]
    async fn hostile_file_names_are_listed_and_execute_nothing() {
        let (d, v) = vault();
        write(&d, "Notes/it's $(touch pwned).md", "# Q\n");
        let (notes, _) = v.notes_from(&v.listing(None).await.unwrap(), &HashMap::new()).await.unwrap();
        assert_eq!(notes[0].id, "Notes/it's $(touch pwned).md");
        assert!(!d.path().join("pwned").exists() && !d.path().join("Notes/pwned").exists());
    }
}
