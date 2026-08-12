// Live round-trip for the LocalFS vertical on a throwaway temp directory.
//   cargo run --example roundtrip_localfs
// No network, no signed-in account — exercises the filesystem vertical directly.
use std::collections::HashMap;
use jodd_lib::backend::localfs::LocalFsVertical;
use jodd_lib::backend::{MetadataSidecar, NoteStore, SaveOp, SidecarKind, Transport};

#[tokio::main]
async fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("jodd_localfs_rt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let v = LocalFsVertical::new(dir.clone(), "local-test".into());
    let mut fails: Vec<String> = vec![];
    let chk = |c: bool, m: &str, f: &mut Vec<String>| { if c { eprintln!("[rt] PASS: {}", m) } else { eprintln!("[rt] FAIL: {}", m); f.push(m.into()) } };

    // 1) ensure_folder + save (insert)
    v.ensure_folder("Notes/play5").await.map_err(|e| e.to_string())?;
    let op = SaveOp { title: "LFS title", body_html: "<div>body #t [[L-abcd1234]]</div>",
        existing_remote_id: None, existing_uuid: None, existing_created_date: None, label: "Notes/play5" };
    let saved = v.save_note_full(&op, &[]).await.map_err(|e| e.to_string())?;
    chk(dir.join(&saved.id).exists(), "eml file written to disk", &mut fails);

    // 2) list + round-trip checks
    let (notes, _) = v.list_all_notes(&HashMap::new()).await.map_err(|e| e.to_string())?;
    let mine = notes.iter().find(|n| n.uuid == saved.uuid);
    chk(mine.is_some(), "note found via list_all_notes", &mut fails);
    if let Some(n) = mine {
        chk(n.title == "LFS title", "title round-trips", &mut fails);
        chk(n.label == "Notes/play5", "label = folder path", &mut fails);
        chk(n.body_html.contains("#t") && n.body_html.contains("[[L-abcd1234]]"), "tag+link retained", &mut fails);
        chk(!n.body_html.contains("<div>LFS title</div>"), "title row stripped", &mut fails);
    }

    // 3) edit (stable id, no dup)
    let op2 = SaveOp { title: "LFS title", body_html: "<div>EDITED #t</div>",
        existing_remote_id: Some(&saved.id), existing_uuid: Some(&saved.uuid), existing_created_date: None, label: "Notes/play5" };
    let saved2 = v.save_note_full(&op2, &[]).await.map_err(|e| e.to_string())?;
    chk(saved2.id == saved.id, "edit keeps stable remote_id (overwrite in place)", &mut fails);
    let (notes2, _) = v.list_all_notes(&HashMap::new()).await.map_err(|e| e.to_string())?;
    chk(notes2.iter().filter(|n| n.uuid == saved.uuid).count() == 1, "exactly one copy after edit", &mut fails);
    chk(notes2.iter().find(|n| n.uuid == saved.uuid).map(|n| n.body_html.contains("EDITED")).unwrap_or(false), "edited body round-trips", &mut fails);

    // 4) pin sidecar (note: list_sidecars returns Option<Vec> — None when .meta absent)
    v.put_sidecar(&saved.uuid, SidecarKind::Pin, None, None).await.map_err(|e| e.to_string())?;
    chk(dir.join(".meta").join(format!("{}.pin", saved.uuid)).exists(), "pin sidecar file created", &mut fails);
    let pins = v.list_sidecars(SidecarKind::Pin).await.map_err(|e| e.to_string())?;
    chk(pins.as_ref().map(|p| p.iter().any(|s| s.note_uuid == saved.uuid)).unwrap_or(false), "pin sidecar listed (Some)", &mut fails);

    // 5) move between folders
    v.move_note(&saved2.id, &["Notes".to_string()], &["Notes/play5".to_string()]).await.map_err(|e| e.to_string())?;
    let (notes3, _) = v.list_all_notes(&HashMap::new()).await.map_err(|e| e.to_string())?;
    chk(notes3.iter().find(|n| n.uuid == saved.uuid).map(|n| n.label == "Notes").unwrap_or(false), "move_note relabels to Notes", &mut fails);

    // 6) delete → .trash, then untrash
    let moved_id = notes3.iter().find(|n| n.uuid == saved.uuid).map(|n| n.id.clone()).ok_or("note vanished before delete")?;
    v.delete(&moved_id).await.map_err(|e| e.to_string())?;
    let trashed = v.list_trashed().await.map_err(|e| e.to_string())?;
    chk(trashed.iter().any(|t| t.uuid == saved.uuid), "delete moves to .trash", &mut fails);

    // 7) list_sidecars returns None on a FRESH dir (store-absence semantics)
    let fresh = std::env::temp_dir().join(format!("jodd_localfs_fresh_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&fresh);
    let v2 = LocalFsVertical::new(fresh.clone(), "fresh".into());
    chk(v2.list_sidecars(SidecarKind::Pin).await.map_err(|e| e.to_string())?.is_none(), "list_sidecars None when .meta absent (no-prune signal)", &mut fails);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&fresh);
    if fails.is_empty() { eprintln!("[rt] ✅ ALL PASSED"); Ok(()) }
    else { eprintln!("[rt] ❌ {} failed", fails.len()); Err(format!("{} checks failed", fails.len())) }
}
