//! Writing and reading a backup bundle: a zip of OKF concept files (one per
//! note), a root `index.md` and `log.md`, attachment bytes under
//! `references/attachments/`, and `.jodd/bundle.json` (the manifest).

use super::body::{content_sha256, encode_body};
use super::manifest::{Manifest, ManifestNote, FORMAT_VERSION};
use super::naming::{dir_for_folder, sanitize_segment, NameAllocator};
use super::okf::{parse_concept, render_concept, render_index, render_log};
use super::{AttachmentRef, BodyFormat, HistoryEvent, JoddMeta, NoteDoc, Trigger};
use crate::db::{AttachmentMeta, BackupRead, CachedNote};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const MANIFEST_ENTRY: &str = ".jodd/bundle.json";
const ATTACHMENT_ROOT: &str = "references/attachments";
/// Exact note HTML for `BodyFormat::HtmlFile` bodies. Not `.md`, so never read as a concept.
const BODY_ROOT: &str = "references/bodies";
const NEWER_FORMAT: &str = "This backup was made by a newer version of Jodd.";
/// Ceilings on what one zip entry may inflate to; a hostile or corrupt zip
/// can declare any size.
const MAX_CONCEPT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ATTACHMENT_BYTES: u64 = 512 * 1024 * 1024;

pub struct BundleSource<'a> {
    pub account_id: &'a str,
    pub backend: &'a str,
    pub root_label: &'a str,
    pub trigger: Trigger,
    pub created_ms: i64,
    pub app_version: &'a str,
    pub device: &'a str,
    pub read: &'a BackupRead,
    /// Write only these note uuids (the pre-restore snapshot of the notes a
    /// restore is about to overwrite). `None` writes every note.
    pub only: Option<&'a HashSet<String>>,
}

pub struct BundleNote {
    /// The concept file's path inside the zip.
    pub path: String,
    pub doc: NoteDoc,
}

pub struct OpenBundle {
    info: Manifest,
    notes: Vec<BundleNote>,
    zip: ZipArchive<File>,
}

fn iso(ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// When the note last changed: its last local edit, else (a note never
/// edited here) the remote's time. A 0 means "not known", never 1970.
fn modified_iso(note: &CachedNote) -> Option<String> {
    [Some(note.last_local_modified_at), note.last_remote_modified_at]
        .into_iter()
        .flatten()
        .find(|&ms| ms > 0)
        .and_then(iso)
}

fn sync_state_str(note: &CachedNote) -> String {
    match serde_json::to_value(note.sync_state) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// Where a note's exact HTML goes when its `.md` body is only readable text.
fn body_path(uuid: &str) -> String {
    format!("/{BODY_ROOT}/{}.html", sanitize_segment(uuid))
}

/// A bundle-relative attachment path, with the leading `/` the
/// `AttachmentRef.path` field carries. Unique within one note.
fn attachment_path(uuid: &str, a: &AttachmentMeta, taken: &mut HashSet<String>) -> String {
    let cid = sanitize_segment(&a.content_id);
    let stem = match a.filename.as_deref().filter(|f| !f.is_empty()) {
        Some(f) => format!("{cid}-{}", sanitize_segment(f)),
        None => cid,
    };
    let dir = format!("{ATTACHMENT_ROOT}/{}", sanitize_segment(uuid));
    let mut name = stem.clone();
    let mut n = 1;
    while !taken.insert(name.to_lowercase()) {
        n += 1;
        name = format!("{stem} ({n})");
    }
    format!("/{dir}/{name}")
}

fn note_doc(src: &BundleSource, note: &CachedNote, attachments: Vec<AttachmentRef>) -> NoteDoc {
    let read = src.read;
    let events = read.events.get(&note.uuid).map(Vec::as_slice).unwrap_or(&[]);
    let trust = crate::provenance::derive(events, note.local_version);
    // OKF `generated` needs a writer AND a time; when the trust rules name no
    // writer (an unrecorded gap) the field is left out. `jodd.history` still
    // carries every event.
    let generated = match (trust.by, trust.at) {
        (Some(by), Some(at)) => Some((by, at)),
        _ => None,
    };
    let verified = events
        .iter()
        .filter(|e| e.kind == "verify")
        .filter_map(|e| Some((e.actor.clone()?, iso(e.at)?)))
        .collect();
    let history = events
        .iter()
        .map(|e| HistoryEvent {
            kind: e.kind.clone(),
            by: e.actor.clone(),
            at: iso(e.at).unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
        })
        .collect();
    let (format, body) = encode_body(&note.body_html);
    let html = (format == BodyFormat::HtmlFile).then(|| body_path(&note.uuid));
    NoteDoc {
        title: note.title.clone(),
        tags: read.tags.get(&note.uuid).cloned().unwrap_or_default(),
        sources: read.citations.get(&note.uuid).cloned().unwrap_or_default(),
        generated,
        verified,
        jodd: Some(JoddMeta {
            uuid: note.uuid.clone(),
            account: src.account_id.to_string(),
            backend: src.backend.to_string(),
            folder: note.label.clone(),
            created: note.x_mail_created_date.clone(),
            modified: modified_iso(note),
            pinned: note.pinned,
            body: format,
            html,
            content_sha256: content_sha256(&note.body_html),
            sync_state: sync_state_str(note),
            attachments,
            history,
            partial: read.render_loss.get(&note.uuid).cloned(),
        }),
        body,
    }
}

fn zerr(what: &str, e: impl std::fmt::Display) -> String {
    format!("{what}: {e}")
}

/// Supplies one attachment's bytes by `(note uuid, content id)`. `Ok(None)`
/// means it no longer exists; an `Err` fails the whole bundle.
pub type FetchAttachment<'a> = dyn FnMut(&str, &str) -> Result<Option<Vec<u8>>, String> + 'a;

/// Stream a bundle into `out`. Returns the manifest without `zip_bytes` and
/// `zip_sha256`: only the caller can hash the finished file.
///
/// Attachment bytes come from `fetch`, one note at a time, so the snapshot
/// never holds every attachment. One that vanished since the snapshot is left
/// out of its note and reported in `skipped`; the note is still written.
pub fn write_bundle<W: Write + Seek>(
    src: &BundleSource,
    out: W,
    fetch: &mut FetchAttachment,
) -> Result<Manifest, String> {
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut zip = ZipWriter::new(out);
    let put = |zip: &mut ZipWriter<W>, name: &str, bytes: &[u8]| -> Result<(), String> {
        zip.start_file(name, opts).map_err(|e| zerr(name, e))?;
        zip.write_all(bytes).map_err(|e| zerr(name, e))
    };

    // A stable order makes "Title (2)" go to the same note every time.
    let mut notes: Vec<&CachedNote> = src
        .read
        .notes
        .iter()
        .filter(|n| src.only.is_none_or(|set| set.contains(&n.uuid)))
        .collect();
    notes.sort_by(|a, b| (&a.label, &a.title, &a.uuid).cmp(&(&b.label, &b.title, &b.uuid)));

    let mut names = NameAllocator::new();
    let mut manifest_notes = Vec::with_capacity(notes.len());
    let mut index_entries = Vec::with_capacity(notes.len());
    let mut skipped: Vec<(String, String)> = Vec::new();

    for note in notes {
        let mut taken = HashSet::new();
        let stored = src.read.attachments.get(&note.uuid).map(Vec::as_slice).unwrap_or(&[]);
        let mut refs = Vec::with_capacity(stored.len());
        let mut blobs: Vec<(String, Vec<u8>)> = Vec::with_capacity(stored.len());
        for a in stored {
            let Some(data) = fetch(&note.uuid, &a.content_id)? else {
                skipped.push((note.uuid.clone(), format!("attachment {} changed during backup", a.content_id)));
                continue;
            };
            let path = attachment_path(&note.uuid, a, &mut taken);
            refs.push(AttachmentRef {
                content_id: a.content_id.clone(),
                filename: a.filename.clone(),
                mime: a.mime_type.clone(),
                path: path.clone(),
                x_apple_part_url: a.x_apple_part_url.clone(),
            });
            blobs.push((path, data));
        }
        let refs_len = refs.len();
        let doc = note_doc(src, note, refs);
        let text = match render_concept(&doc) {
            Ok(t) => t,
            Err(e) => {
                skipped.push((note.uuid.clone(), e));
                continue;
            }
        };
        let file = names.file_for(&dir_for_folder(&note.label), &note.title);
        put(&mut zip, &file, text.as_bytes())?;
        if let Some(path) = doc.jodd.as_ref().and_then(|j| j.html.as_deref()) {
            put(&mut zip, path.trim_start_matches('/'), note.body_html.as_bytes())?;
        }
        for (path, data) in &blobs {
            put(&mut zip, path.trim_start_matches('/'), data)?;
        }
        let jodd = doc.jodd.as_ref().expect("note_doc always sets jodd");
        manifest_notes.push(ManifestNote {
            uuid: note.uuid.clone(),
            title: note.title.clone(),
            folder: note.label.clone(),
            modified: jodd.modified.clone(),
            content_sha256: jodd.content_sha256.clone(),
            attachments: refs_len,
            partial: jodd.partial.clone(),
        });
        let shown = if note.title.trim().is_empty() { "Untitled" } else { note.title.as_str() };
        index_entries.push((file, shown.to_string()));
    }

    let mut folders: Vec<String> = src
        .read
        .folders
        .iter()
        .map(|f| f.path.clone())
        .filter(|p| match src.only {
            None => true,
            Some(_) => manifest_notes.iter().any(|n| &n.folder == p),
        })
        .collect();
    folders.sort();
    folders.dedup();

    let manifest = Manifest {
        jodd_backup: FORMAT_VERSION,
        account_id: src.account_id.to_string(),
        backend: src.backend.to_string(),
        root_label: src.root_label.to_string(),
        created_ms: src.created_ms,
        app_version: src.app_version.to_string(),
        device: src.device.to_string(),
        trigger: src.trigger,
        keep: false,
        zip_bytes: 0,
        zip_sha256: String::new(),
        notes: manifest_notes,
        folders,
        skipped,
    };

    let created = iso(src.created_ms).unwrap_or_default();
    let index = render_index(&format!("Jodd backup of {}", src.account_id), true, &index_entries);
    let log = render_log(
        &created,
        &format!("Backup of {} ({} notes)", src.account_id, manifest.notes.len()),
    );
    put(&mut zip, "index.md", index.as_bytes())?;
    put(&mut zip, "log.md", log.as_bytes())?;
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| zerr(MANIFEST_ENTRY, e))?;
    put(&mut zip, MANIFEST_ENTRY, &json)?;

    let mut inner = zip.finish().map_err(|e| zerr("finish zip", e))?;
    inner.flush().map_err(|e| zerr("flush zip", e))?;
    Ok(manifest)
}

/// Read one entry, refusing one that inflates past `limit`.
fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str, limit: u64) -> Result<Vec<u8>, String> {
    let entry = zip.by_name(name).map_err(|e| zerr(name, e))?;
    let mut buf = Vec::new();
    entry
        .take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|e| zerr(name, e))?;
    if buf.len() as u64 > limit {
        return Err(format!("{name}: larger than a backup entry may be"));
    }
    Ok(buf)
}

/// Whether a zip entry is a note: a `.md` file that is not one of the
/// bundle's own `index.md` / `log.md`, not an attachment (the writer puts those
/// under `references/attachments/`) and not `.jodd/bundle.json`. Nothing wider
/// is skipped: a folder labelled `references` or `.jodd` holds real notes.
fn is_concept_path(name: &str) -> bool {
    if name.ends_with('/') || !name.to_lowercase().ends_with(".md") {
        return false;
    }
    if name.starts_with("references/attachments/") {
        return false;
    }
    let base = name.rsplit('/').next().unwrap_or(name).to_lowercase();
    base != "index.md" && base != "log.md"
}

impl OpenBundle {
    pub fn open(path: &Path) -> Result<OpenBundle, String> {
        let file = File::open(path).map_err(|e| zerr(&path.display().to_string(), e))?;
        let mut zip = ZipArchive::new(file).map_err(|e| zerr("not a readable backup", e))?;

        let raw = read_entry(&mut zip, MANIFEST_ENTRY, MAX_CONCEPT_BYTES)
            .map_err(|e| format!("not a Jodd backup ({e})"))?;
        let value: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|e| zerr("bundle.json", e))?;
        // Check the version before the shape: a newer format may not fit this one.
        match value.get("joddBackup").and_then(|v| v.as_u64()) {
            Some(v) if v == u64::from(FORMAT_VERSION) => {}
            Some(_) => return Err(NEWER_FORMAT.to_string()),
            None => return Err("not a Jodd backup (bundle.json has no joddBackup)".to_string()),
        }
        let info: Manifest = serde_json::from_value(value).map_err(|e| zerr("bundle.json", e))?;

        // A name the zip cannot decode as text is not one this writer made.
        let names: Vec<String> = zip
            .file_names()
            .filter_map(Result::ok)
            .map(|n| n.into_owned())
            .filter(|n| is_concept_path(n))
            .collect();
        let mut notes = Vec::with_capacity(names.len());
        for name in names {
            let bytes = read_entry(&mut zip, &name, MAX_CONCEPT_BYTES)?;
            let text = String::from_utf8(bytes).map_err(|e| zerr(&name, e))?;
            let mut doc = parse_concept(&text).map_err(|e| zerr(&name, e))?;
            // The readable body is for people; restore needs the exact HTML.
            if let Some(j) = doc.jodd.as_mut().filter(|j| j.body == BodyFormat::HtmlFile) {
                let path = j.html.clone().ok_or_else(|| zerr(&name, "html-file body without its html path"))?;
                let bytes = read_entry(&mut zip, path.trim_start_matches('/'), MAX_CONCEPT_BYTES)
                    .map_err(|e| format!("This backup is incomplete: the text of {name} could not be read ({e})."))?;
                doc.body = String::from_utf8(bytes).map_err(|e| zerr(&path, e))?;
                j.body = BodyFormat::Html;
            }
            notes.push(BundleNote { path: name, doc });
        }
        notes.sort_by(|a, b| a.path.cmp(&b.path));

        // The manifest lists every note the writer put in. One that did not
        // come back is a lost note, so refuse rather than restore a short list.
        let parsed: HashSet<&str> = notes
            .iter()
            .filter_map(|n| n.doc.jodd.as_ref().map(|j| j.uuid.as_str()))
            .collect();
        let missing = info.notes.iter().filter(|n| !parsed.contains(n.uuid.as_str())).count();
        if missing > 0 {
            return Err(format!(
                "This backup is incomplete: {missing} note(s) listed in its manifest could not be read."
            ));
        }
        Ok(OpenBundle { info, notes, zip })
    }

    pub fn info(&self) -> &Manifest {
        &self.info
    }

    pub fn notes(&self) -> &[BundleNote] {
        &self.notes
    }

    /// The bytes of an attachment, by the bundle-relative path a note's
    /// `jodd.attachments[].path` carries (leading `/` optional).
    pub fn attachment(&mut self, path: &str) -> Result<Vec<u8>, String> {
        read_entry(&mut self.zip, path.trim_start_matches('/'), MAX_ATTACHMENT_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::body::decode_body;
    use crate::db::{CachedFolder, FolderSyncState};
    use crate::provenance::Event;
    use std::collections::HashMap;
    use std::io::Cursor;

    const ACCT: &str = "gmail:a@x";

    fn folder(path: &str) -> CachedFolder {
        CachedFolder {
            account_id: ACCT.into(),
            path: path.into(),
            label_id: None,
            sync_state: FolderSyncState::Clean,
            last_local_modified_at: 0,
            last_synced_at: None,
            kind: "user".into(),
        }
    }

    fn empty_read() -> BackupRead {
        BackupRead {
            notes: vec![],
            folders: vec![],
            attachments: HashMap::new(),
            citations: HashMap::new(),
            tags: HashMap::new(),
            events: HashMap::new(),
            render_loss: HashMap::new(),
        }
    }

    fn source<'a>(read: &'a BackupRead, only: Option<&'a HashSet<String>>) -> BundleSource<'a> {
        BundleSource {
            account_id: ACCT,
            backend: "gmail",
            root_label: "Notes",
            trigger: Trigger::Manual,
            created_ms: 1_761_210_900_000,
            app_version: "0.0.0-test",
            device: "test",
            read,
            only,
        }
    }

    fn sample() -> BackupRead {
        let mut read = empty_read();
        let mut a = crate::test_support::note(ACCT, "u1").title("Alpha").label("Notes/Work").build();
        a.pinned = true;
        a.x_mail_created_date = Some("Thu, 4 Jun 2026 01:19:50 +0700".into());
        a.last_local_modified_at = 1_761_210_000_000;
        let b = crate::test_support::note(ACCT, "u2").title("Beta บันทึก").label("Notes").build();
        read.notes = vec![a, b];
        read.folders = vec![folder("Notes"), folder("Notes/Work"), folder("Notes/Empty")];
        read.attachments.insert(
            "u1".into(),
            vec![meta("cid1", "a.png")],
        );
        read.tags.insert("u1".into(), vec!["work".into()]);
        read.citations.insert("u1".into(), vec!["https://example.com".into()]);
        read.events.insert(
            "u1".into(),
            vec![
                Event { id: 1, kind: "create".into(), actor: Some("human:owner".into()), at: 1_761_200_000_000, local_version: Some(1) },
                Event { id: 2, kind: "verify".into(), actor: Some("human:owner".into()), at: 1_761_200_500_000, local_version: Some(1) },
            ],
        );
        read
    }

    fn meta(cid: &str, filename: &str) -> AttachmentMeta {
        AttachmentMeta {
            content_id: cid.into(),
            mime_type: "image/png".into(),
            filename: Some(filename.into()),
            x_apple_part_url: None,
        }
    }

    /// The bytes the tests' fake store holds: `u1`/`cid1` is `[1, 2, 3, 4]`.
    fn store(uuid: &str, cid: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(match (uuid, cid) {
            ("u1", "cid1") => Some(vec![1, 2, 3, 4]),
            _ => None,
        })
    }

    fn write_file(read: &BackupRead, only: Option<&HashSet<String>>) -> (tempfile::NamedTempFile, Manifest) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let file = tmp.reopen().unwrap();
        let m = write_bundle(&source(read, only), file, &mut store).unwrap();
        (tmp, m)
    }

    fn names(path: &Path) -> Vec<String> {
        let zip = ZipArchive::new(File::open(path).unwrap()).unwrap();
        zip.file_names().map(|n| n.unwrap().into_owned()).collect()
    }

    #[test]
    fn modified_falls_back_to_the_remote_time_for_a_never_edited_note() {
        let mut read = empty_read();
        let mut pulled = crate::test_support::note(ACCT, "u1").title("Pulled").build();
        pulled.last_local_modified_at = 0;
        pulled.last_remote_modified_at = Some(1_761_210_000_000);
        let mut unknown = crate::test_support::note(ACCT, "u2").title("Unknown").build();
        unknown.last_local_modified_at = 0;
        unknown.last_remote_modified_at = Some(0);
        let mut none = crate::test_support::note(ACCT, "u3").title("None").build();
        none.last_local_modified_at = 0;
        none.last_remote_modified_at = None;
        read.notes = vec![pulled, unknown, none];
        let (tmp, manifest) = write_file(&read, None);
        let open = OpenBundle::open(tmp.path()).unwrap();
        let modified = |title: &str| {
            open.notes().iter().find(|n| n.doc.title == title).unwrap().doc.jodd.as_ref().unwrap().modified.clone()
        };
        // Never 1970: a 0 is "not known", not the epoch.
        assert_eq!(modified("Pulled").as_deref(), Some("2025-10-23T09:00:00Z"));
        assert_eq!(modified("Unknown"), None);
        assert_eq!(modified("None"), None);
        let in_manifest = |uuid: &str| manifest.notes.iter().find(|n| n.uuid == uuid).unwrap().modified.clone();
        assert_eq!(in_manifest("u1").as_deref(), Some("2025-10-23T09:00:00Z"));
        assert_eq!((in_manifest("u2"), in_manifest("u3")), (None, None));
    }

    #[test]
    fn bundle_round_trips_notes_folders_attachments() {
        let read = sample();
        let (tmp, written) = write_file(&read, None);
        let mut open = OpenBundle::open(tmp.path()).unwrap();
        assert_eq!(open.notes().len(), 2);
        for src in &read.notes {
            let n = open
                .notes()
                .iter()
                .find(|n| n.doc.jodd.as_ref().unwrap().uuid == src.uuid)
                .unwrap_or_else(|| panic!("{} missing", src.uuid));
            let jodd = n.doc.jodd.as_ref().unwrap();
            assert_eq!(n.doc.title, src.title);
            assert_eq!(decode_body(jodd.body, &n.doc.body), src.body_html);
            assert_eq!(jodd.content_sha256, content_sha256(&src.body_html));
            assert_eq!(jodd.pinned, src.pinned);
            assert_eq!(jodd.folder, src.label);
            assert_eq!(jodd.created, src.x_mail_created_date);
            assert_eq!(jodd.account, ACCT);
            assert_eq!(jodd.sync_state, "clean");
        }
        let a = open.notes().iter().find(|n| n.doc.title == "Alpha").unwrap().doc.clone();
        let jodd = a.jodd.as_ref().unwrap();
        assert_eq!(a.tags, vec!["work".to_string()]);
        assert_eq!(a.sources, vec!["https://example.com".to_string()]);
        assert_eq!(jodd.modified.as_deref(), Some("2025-10-23T09:00:00Z"));
        assert_eq!(
            jodd.history,
            vec![
                HistoryEvent { kind: "create".into(), by: Some("human:owner".into()), at: "2025-10-23T06:13:20Z".into() },
                HistoryEvent { kind: "verify".into(), by: Some("human:owner".into()), at: "2025-10-23T06:21:40Z".into() },
            ]
        );
        assert_eq!(a.verified, vec![("human:owner".to_string(), "2025-10-23T06:21:40Z".to_string())]);
        assert_eq!(a.generated, Some(("human:owner".to_string(), "2025-10-23T06:13:20Z".to_string())));

        assert_eq!(jodd.attachments.len(), 1);
        assert!(jodd.attachments[0].path.starts_with("/references/attachments/u1/"));
        assert_eq!(open.attachment(&jodd.attachments[0].path).unwrap(), vec![1, 2, 3, 4]);

        assert_eq!(open.info().jodd_backup, 1);
        assert!(open.info().folders.contains(&"Notes/Empty".to_string()));
        assert_eq!(open.info(), &written);
        assert_eq!(written.notes.len(), 2);
        assert_eq!(written.notes.iter().find(|n| n.uuid == "u1").unwrap().attachments, 1);
    }

    #[test]
    fn bundle_layout_is_okf() {
        let read = sample();
        let (tmp, _) = write_file(&read, None);
        let mut zip = ZipArchive::new(File::open(tmp.path()).unwrap()).unwrap();
        let mut text = |name: &str| {
            let mut s = String::new();
            zip.by_name(name).unwrap().read_to_string(&mut s).unwrap();
            s
        };
        assert!(text("index.md").starts_with("---\nokf_version: \"0.2\"\n---\n"));
        assert!(text("log.md").starts_with("# Directory Update Log"));
        assert!(text(".jodd/bundle.json").contains("\"joddBackup\": 1"));

        let all = names(tmp.path());
        for n in &read.notes {
            let dir = dir_for_folder(&n.label);
            assert!(
                all.iter().any(|f| f.starts_with(&format!("{dir}/")) && f.ends_with(&format!("{}.md", n.title))),
                "{} not under {dir}: {all:?}",
                n.title
            );
        }
        for f in all.iter().filter(|f| is_concept_path(f)) {
            assert!(text(f).starts_with("---\ntype: Note"), "{f}");
        }
        assert_eq!(all.iter().filter(|f| is_concept_path(f)).count(), 2);
    }

    #[test]
    fn only_subset_writes_only_those_notes() {
        let read = sample();
        let only: HashSet<String> = ["u2".to_string()].into();
        let (tmp, m) = write_file(&read, Some(&only));
        let open = OpenBundle::open(tmp.path()).unwrap();
        assert_eq!(open.notes().len(), 1);
        assert_eq!(open.notes()[0].doc.jodd.as_ref().unwrap().uuid, "u2");
        assert_eq!(m.notes.len(), 1);
        assert_eq!(m.folders, vec!["Notes".to_string()]);
        assert!(!names(tmp.path()).iter().any(|n| n.starts_with("references/")));
    }

    #[test]
    fn partial_flag_is_written() {
        let mut read = sample();
        read.render_loss.insert("u1".into(), "inline_objects".into());
        let (tmp, m) = write_file(&read, None);
        let open = OpenBundle::open(tmp.path()).unwrap();
        let a = open.notes().iter().find(|n| n.doc.title == "Alpha").unwrap();
        assert_eq!(a.doc.jodd.as_ref().unwrap().partial.as_deref(), Some("inline_objects"));
        assert_eq!(m.notes.iter().find(|n| n.uuid == "u1").unwrap().partial.as_deref(), Some("inline_objects"));
        assert_eq!(m.notes.iter().find(|n| n.uuid == "u2").unwrap().partial, None);
    }

    #[test]
    fn newer_format_is_refused() {
        let read = sample();
        let (tmp, _) = write_file(&read, None);
        // Rebuild the zip with a version-2 manifest.
        let mut src = ZipArchive::new(File::open(tmp.path()).unwrap()).unwrap();
        let patched = tempfile::NamedTempFile::new().unwrap();
        let mut out = ZipWriter::new(patched.reopen().unwrap());
        let opts = SimpleFileOptions::default();
        for i in 0..src.len() {
            let mut e = src.by_index(i).unwrap();
            let name = e.name().unwrap().into_owned();
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            if name == MANIFEST_ENTRY {
                let s = String::from_utf8(buf).unwrap().replace("\"joddBackup\": 1", "\"joddBackup\": 2");
                assert!(s.contains("\"joddBackup\": 2"));
                buf = s.into_bytes();
            }
            out.start_file(name, opts).unwrap();
            out.write_all(&buf).unwrap();
        }
        out.finish().unwrap();
        let err = OpenBundle::open(patched.path()).err().expect("must refuse");
        assert!(err.contains("newer version"), "{err}");
    }

    #[test]
    fn corrupt_zip_is_refused() {
        let read = sample();
        let (tmp, _) = write_file(&read, None);
        let bytes = std::fs::read(tmp.path()).unwrap();
        let cut = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(cut.path(), &bytes[..bytes.len() / 2]).unwrap();
        assert!(OpenBundle::open(cut.path()).is_err());
        std::fs::write(cut.path(), b"not a zip at all").unwrap();
        assert!(OpenBundle::open(cut.path()).is_err());
        std::fs::write(cut.path(), b"").unwrap();
        assert!(OpenBundle::open(cut.path()).is_err());
    }

    #[test]
    fn notes_in_folders_named_like_reserved_dirs_round_trip() {
        let mut read = empty_read();
        for (u, label) in [("u1", "references"), ("u2", ".jodd"), ("u3", "references/deep")] {
            read.notes.push(crate::test_support::note(ACCT, u).title(&format!("N {u}")).label(label).build());
        }
        let (tmp, m) = write_file(&read, None);
        assert_eq!(m.notes.len(), 3);
        let open = OpenBundle::open(tmp.path()).unwrap();
        assert_eq!(open.notes().len(), 3);
    }

    #[test]
    fn bundle_missing_a_listed_concept_is_refused() {
        let read = sample();
        let (tmp, _) = write_file(&read, None);
        let mut src = ZipArchive::new(File::open(tmp.path()).unwrap()).unwrap();
        let cut = tempfile::NamedTempFile::new().unwrap();
        let mut out = ZipWriter::new(cut.reopen().unwrap());
        let mut dropped = false;
        for i in 0..src.len() {
            let mut e = src.by_index(i).unwrap();
            let name = e.name().unwrap().into_owned();
            if !dropped && is_concept_path(&name) {
                dropped = true;
                continue;
            }
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            out.start_file(name, SimpleFileOptions::default()).unwrap();
            out.write_all(&buf).unwrap();
        }
        out.finish().unwrap();
        assert!(dropped);
        let err = OpenBundle::open(cut.path()).err().expect("must refuse");
        assert!(err.contains("incomplete") && err.contains("1 note(s)"), "{err}");
    }

    const STYLED: &str = "<html><head></head><body style=\"overflow-wrap: break-word;\"><div><u>Under</u> text</div><div>line two</div></body></html>";

    fn styled_read() -> BackupRead {
        let mut read = empty_read();
        let mut n = crate::test_support::note(ACCT, "u1").title("Styled").label("Notes").build();
        n.body_html = STYLED.to_string();
        read.notes = vec![n];
        read.folders = vec![folder("Notes")];
        read
    }

    /// Live pass 2026-10-24: three notes in four carried raw HTML in their `.md`.
    #[test]
    fn a_body_without_exact_markdown_is_readable_and_kept_exactly() {
        let (tmp, _) = write_file(&styled_read(), None);
        let all = names(tmp.path());
        let concept = all.iter().find(|n| is_concept_path(n)).unwrap().clone();
        let html_file = all.iter().find(|n| n.starts_with("references/bodies/")).expect("exact HTML kept").clone();
        assert!(html_file.ends_with(".html"));

        let mut zip = ZipArchive::new(File::open(tmp.path()).unwrap()).unwrap();
        let mut text = String::new();
        zip.by_name(&concept).unwrap().read_to_string(&mut text).unwrap();
        let body = text.splitn(3, "---\n").nth(2).unwrap();
        assert!(body.contains("Under") && body.contains("line two"), "{body}");
        assert!(!body.contains('<'), "no tags in the readable body: {body}");
        assert!(text.contains("body: html-file"));

        let open = OpenBundle::open(tmp.path()).unwrap();
        let doc = &open.notes()[0].doc;
        assert_eq!(doc.body, STYLED, "restore reads the exact HTML");
        assert_eq!(doc.jodd.as_ref().unwrap().body, BodyFormat::Html);
    }

    /// The first M1 builds wrote the HTML itself as the `.md` body.
    #[test]
    fn a_bundle_with_html_in_the_body_still_opens() {
        let (tmp, _) = write_file(&styled_read(), None);
        let mut src = ZipArchive::new(File::open(tmp.path()).unwrap()).unwrap();
        let old = tempfile::NamedTempFile::new().unwrap();
        let mut out = ZipWriter::new(old.reopen().unwrap());
        for i in 0..src.len() {
            let mut e = src.by_index(i).unwrap();
            let name = e.name().unwrap().into_owned();
            if name.starts_with("references/bodies/") {
                continue;
            }
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            if is_concept_path(&name) {
                let mut doc = parse_concept(std::str::from_utf8(&buf).unwrap()).unwrap();
                let j = doc.jodd.as_mut().unwrap();
                j.body = BodyFormat::Html;
                j.html = None;
                doc.body = STYLED.to_string();
                buf = render_concept(&doc).unwrap().into_bytes();
            }
            out.start_file(name, SimpleFileOptions::default()).unwrap();
            out.write_all(&buf).unwrap();
        }
        out.finish().unwrap();
        let open = OpenBundle::open(old.path()).unwrap();
        assert_eq!(open.notes()[0].doc.body, STYLED);
    }

    #[test]
    fn colliding_titles_and_attachment_names_stay_distinct() {
        let mut read = empty_read();
        for u in ["u1", "u2"] {
            read.notes.push(crate::test_support::note(ACCT, u).title("Same").build());
        }
        // Two attachments with the same content id and name on one note; the
        // store hands back 1 then 2.
        read.attachments.insert("u1".into(), vec![meta("cid", "a.png"), meta("cid", "a.png")]);
        let mut next = 0u8;
        let mut fetch = |_: &str, _: &str| -> Result<Option<Vec<u8>>, String> {
            next += 1;
            Ok(Some(vec![next]))
        };
        let mut buf = Cursor::new(Vec::new());
        write_bundle(&source(&read, None), &mut buf, &mut fetch).unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), buf.into_inner()).unwrap();
        let mut open = OpenBundle::open(tmp.path()).unwrap();
        assert_eq!(open.notes().len(), 2);
        let u1 = open.notes().iter().find(|n| n.doc.jodd.as_ref().unwrap().uuid == "u1").unwrap().doc.clone();
        let refs = &u1.jodd.as_ref().unwrap().attachments;
        assert_ne!(refs[0].path, refs[1].path);
        let got: Vec<Vec<u8>> = refs.iter().map(|r| open.attachment(&r.path).unwrap()).collect();
        assert_eq!(got, vec![vec![1], vec![2]]);
    }

    #[test]
    fn a_vanished_attachment_is_left_out_and_reported() {
        let mut read = sample();
        // u1 has cid1 (present in the store) and cid2 (gone since the snapshot).
        read.attachments.get_mut("u1").unwrap().push(meta("cid2", "b.png"));
        let (tmp, m) = write_file(&read, None);

        assert_eq!(m.skipped, vec![("u1".to_string(), "attachment cid2 changed during backup".to_string())]);
        let written = m.notes.iter().find(|n| n.uuid == "u1").unwrap();
        assert_eq!(written.attachments, 1);

        let mut open = OpenBundle::open(tmp.path()).unwrap();
        assert_eq!(open.info(), &m);
        let a = open.notes().iter().find(|n| n.doc.title == "Alpha").unwrap().doc.clone();
        let refs = &a.jodd.as_ref().unwrap().attachments;
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].content_id, "cid1");
        assert_eq!(open.attachment(&refs[0].path).unwrap(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_fetch_error_fails_the_bundle() {
        let read = sample();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut fetch = |_: &str, _: &str| -> Result<Option<Vec<u8>>, String> { Err("db locked".into()) };
        let r = write_bundle(&source(&read, None), tmp.reopen().unwrap(), &mut fetch);
        assert_eq!(r.unwrap_err(), "db locked");
    }

    #[test]
    fn empty_body_note_still_gets_a_file() {
        let mut read = empty_read();
        let mut n = crate::test_support::note(ACCT, "u1").title("Blank").build();
        n.body_html = String::new();
        read.notes.push(n);
        let (tmp, _) = write_file(&read, None);
        let open = OpenBundle::open(tmp.path()).unwrap();
        assert_eq!(open.notes().len(), 1);
        let jodd = open.notes()[0].doc.jodd.as_ref().unwrap();
        assert_eq!(decode_body(jodd.body, &open.notes()[0].doc.body), "");
    }

    #[test]
    fn two_thousand_notes_stream_to_file() {
        let mut read = empty_read();
        let filler = "x".repeat(2000);
        for i in 0..2000 {
            let mut n = crate::test_support::note(ACCT, &format!("u{i}")).title(&format!("Note {i}")).build();
            n.body_html = format!("<div>Note {i}</div><div>{filler}</div>");
            read.notes.push(n);
        }
        let (tmp, m) = write_file(&read, None);
        assert_eq!(m.notes.len(), 2000);
        let open = OpenBundle::open(tmp.path()).unwrap();
        assert_eq!(open.notes().len(), 2000);
    }
}
