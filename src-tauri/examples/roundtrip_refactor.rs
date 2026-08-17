// Live round-trip verification for the Vertical #0 refactor.
//
// Exercises the REFACTORED code path — constructs `GmailVertical` exactly as
// lib.rs now does and drives save/list/edit/delete THROUGH it (not the raw
// gmail:: functions) against the real signed-in Gmail account. Scoped to a
// throwaway note in the `Notes/play5` test subtree, which it cleans up.
//
// Run from src-tauri/ :
//   cargo run --example roundtrip_refactor            # account index 0
//   cargo run --example roundtrip_refactor -- 1       # account index 1
//
// PASS criteria printed at the end. This is a dev tool (examples/ per the
// macOS-bundle one-binary rule), not part of the test suite.

use std::collections::HashMap;

use jodd_lib::backend::gmail::GmailVertical;
use jodd_lib::backend::{NoteStore, SaveOp, Transport};
use jodd_lib::{accounts, auth};

#[tokio::main]
async fn main() -> Result<(), String> {
    // keyring-core has no lazy init — unlike keyring 2, an Entry::new() call
    // before this returns NoDefaultStore, which accounts::load_refresh_token's
    // .ok()? swallows into None, misreporting as "no refresh token for {id}".
    jodd_lib::secrets::init()?;

    let _ = dotenv::from_path("../.env");
    let _ = dotenv::dotenv();

    let all = accounts::load_accounts();
    if all.is_empty() {
        return Err("no signed-in accounts — run Jodd and sign in first".into());
    }
    let idx: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let account = all.get(idx).ok_or_else(|| format!("account {} out of range", idx))?;
    eprintln!("[rt] account [{}] {}", idx, account.email);

    let rt = accounts::load_refresh_token(&account.id)
        .ok_or_else(|| format!("no refresh token for {}", account.id))?;
    let token = auth::refresh_access_token(&rt).await?.access_token;
    let label_map = jodd_lib::backend::gmail::wire::get_label_map(&token).await?;
    eprintln!("[rt] {} labels loaded", label_map.len());

    // Construct the vertical exactly as lib.rs's vertical_for() helper does.
    let meta_label = account.meta_label.clone().unwrap_or_else(|| "Notes-Meta".to_string());
    let v = GmailVertical::new(token.clone(), label_map.clone(), account.email.clone(), meta_label.clone());

    let mut failures: Vec<String> = Vec::new();
    let check = |cond: bool, msg: &str, failures: &mut Vec<String>| {
        if cond { eprintln!("[rt]   PASS: {}", msg); }
        else { eprintln!("[rt]   FAIL: {}", msg); failures.push(msg.to_string()); }
    };

    // 1) ensure_folder (Transport) — Notes/play5 test subtree.
    eprintln!("[rt] ensure_folder Notes/play5 ...");
    let folder = v.ensure_folder("Notes/play5").await.map_err(|e| e.to_string())?;
    eprintln!("[rt]   folder id={} path={}", folder.id, folder.path);
    // Refresh label_map in case ensure_folder created the label, then rebuild v.
    let label_map = jodd_lib::backend::gmail::wire::get_label_map(&token).await?;
    let v = GmailVertical::new(token.clone(), label_map.clone(), account.email.clone(), meta_label.clone());

    // 2) save (insert) THROUGH the vertical — body carries a #tag and a [[link]];
    //    title is NOT pre-injected (editor-view), so build_note_mime must inject it.
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let title = format!("Jodd refactor round-trip {}", stamp);
    let body = "<div>line one with a #refactortag here</div>\
                <div>see [[Some Other Note-abcd1234]] for context</div>\
                <div>plain body line</div>";
    let op = SaveOp {
        title: &title, body_html: body,
        existing_remote_id: None, existing_uuid: None, existing_created_date: None,
        label: "Notes/play5",
    };
    eprintln!("[rt] save_note_full (insert) ...");
    let saved = v.save_note_full(&op, &[]).await.map_err(|e| e.to_string())?;
    eprintln!("[rt]   inserted id={} uuid={}", saved.id, saved.uuid);
    check(!saved.uuid.is_empty(), "insert returned a uuid", &mut failures);
    check(!saved.id.is_empty(), "insert returned a gmail id", &mut failures);

    // 3) list_notes THROUGH the vertical — find our note by uuid, verify round-trip.
    eprintln!("[rt] list_all_notes (pull) ...");
    let (notes, _dedup) = v.list_all_notes(&HashMap::new()).await.map_err(|e| e.to_string())?;
    let mine = notes.iter().find(|n| n.uuid == saved.uuid);
    check(mine.is_some(), "inserted note found in list_notes pull", &mut failures);
    if let Some(n) = mine {
        eprintln!("[rt]   pulled title={:?} label={:?}", n.title, n.label);
        check(n.title == title, "title round-trips exactly (Subject decode)", &mut failures);
        check(n.label == "Notes/play5", "label round-trips as Notes/play5", &mut failures);
        // Body is editor-view (title stripped). It must NOT start with a duplicate
        // title row, and must still contain the tag + link markers.
        let stripped = n.body_html.contains("#refactortag");
        check(stripped, "body retains the inline #tag", &mut failures);
        check(n.body_html.contains("[[Some Other Note-abcd1234]]"), "body retains the [[link]]", &mut failures);
        // strip_leading_title removed the injected title row → the editor-view body
        // should start with our first content line, not the title text.
        let no_dup_title = !n.body_html.contains(&format!("<div>{}</div>", title));
        check(no_dup_title, "no duplicate title row in editor-view body (strip works)", &mut failures);
    }

    // 4) edit (insert-then-trash) THROUGH the vertical — same uuid, new id, no dup.
    eprintln!("[rt] save_note_full (edit) ...");
    let body2 = "<div>line one with a #refactortag here</div>\
                 <div>EDITED: see [[Some Other Note-abcd1234]] for context</div>";
    let op2 = SaveOp {
        title: &title, body_html: body2,
        existing_remote_id: Some(&saved.id), existing_uuid: Some(&saved.uuid),
        existing_created_date: None, label: "Notes/play5",
    };
    let saved2 = v.save_note_full(&op2, &[]).await.map_err(|e| e.to_string())?;
    check(saved2.uuid == saved.uuid, "edit preserves the uuid (identity stable)", &mut failures);
    check(saved2.id != saved.id, "edit re-mints the gmail id (insert-then-trash)", &mut failures);

    // Give Gmail a moment, then confirm exactly ONE live copy of our uuid.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let (notes2, _d2) = v.list_all_notes(&HashMap::new()).await.map_err(|e| e.to_string())?;
    let count = notes2.iter().filter(|n| n.uuid == saved.uuid).count();
    eprintln!("[rt]   live copies of uuid after edit: {}", count);
    check(count == 1, "exactly one live copy after edit (no duplicate)", &mut failures);
    if let Some(n) = notes2.iter().find(|n| n.uuid == saved.uuid) {
        check(n.body_html.contains("EDITED:"), "edited body round-trips", &mut failures);
    }

    // 5) delete (Transport) THROUGH the vertical — clean up the throwaway.
    eprintln!("[rt] delete (cleanup) ...");
    v.delete(&saved2.id).await.map_err(|e| e.to_string())?;
    eprintln!("[rt]   trashed id={}", saved2.id);

    eprintln!();
    if failures.is_empty() {
        eprintln!("[rt] ✅ ALL CHECKS PASSED — refactored vertical round-trips against real Gmail.");
        eprintln!("[rt] (the throwaway note in Notes/play5 was trashed; Notes/play5 folder left in place)");
        Ok(())
    } else {
        eprintln!("[rt] ❌ {} CHECK(S) FAILED:", failures.len());
        for f in &failures { eprintln!("[rt]    - {}", f); }
        Err(format!("{} round-trip check(s) failed", failures.len()))
    }
}
