//! Task 12: drives Jodd's **shipped** Microsoft write path — the real
//! `MicrosoftVertical`, through its real `NoteStore`/`Transport` trait
//! methods — against a live Outlook.com/Exchange mailbox, and prints a
//! report a human can read next to Notes.app.
//!
//! `scripts/ms_write_probe.py` already answered what the Graph *API* does.
//! This answers a different question: does Jodd's own code do it? That is a
//! check 82 green module tests cannot perform — this branch once read a
//! 13-note mailbox as zero notes while every unit test stayed green, because
//! every fixture was hand-written to agree with the matcher it was testing.
//! So this calls `save_note_full`, `Transport::save`, `delete`,
//! `create_folder`, `rename_folder`, `delete_folder` and `move_note`
//! directly on a constructed `MicrosoftVertical` — it does not rebuild any
//! Graph request itself.
//!
//! Lives in `examples/`, never `src/bin/` — gotcha #3 in CLAUDE.md: Tauri's
//! bundler copies every `[[bin]]` into `Contents/MacOS/`, and macOS Tahoe
//! refuses to launch a bundle with more than one binary there.
//!
//! Usage:
//!     python3 scripts/ms_get_token.py --print-token
//!     MS_TOKEN='<paste>' cargo run --example ms_write_probe -- kaiwan.h@live.com
//!
//! **The token is read from `$MS_TOKEN` and never printed, and MUST NEVER be
//! passed as a CLI argument** — a process argument list is visible to every
//! other process on the machine (`ps`) and often ends up in shell history;
//! `$MS_TOKEN` in the parent shell does not. Mirrors the convention
//! `scripts/ms_probe_filter.py` and `scripts/ms_write_probe.py` document and
//! follow for the exact same reason.
//!
//! ═══════════════════════════════════════════════════════════════════════
//! THIS EXAMPLE WRITES TO A LIVE APPLE NOTES ACCOUNT.
//! ═══════════════════════════════════════════════════════════════════════
//!
//! Everything it does reaches Notes.app and the iPhone within roughly a
//! minute. On this backend a delete is a HARD delete: measured 2026-08-14,
//! an Apple-side delete left Deleted Items at zero items and the note absent
//! from every scan — there is no undo path to fall back on
//! (`Capabilities::has_trash` is `false` on exactly this evidence).
//!
//! So every destructive call here is routed through `Manifest::destroy_note`
//! / `destroy_folder`, which mirror `scripts/ms_write_probe.py`'s
//! `destroy()`: **refuse to touch any id this run did not itself create.**
//! The guard is enforced in code (a `HashSet` membership check before the
//! request goes out), not merely by which variable name the caller
//! happened to use — see that pair of methods below for the exact check.
//!
//! Two items this run creates are deliberately left behind rather than
//! cleaned up, printed at the end under "LEFT FOR YOU TO INSPECT": the
//! full-write-path note (so you can look at its title/body/folder placement
//! in Notes.app — the auto-deletes below would otherwise race Apple's own
//! ~60s sync and delete the evidence before it ever appeared) and the
//! empty-title note Task 3's guard exists for (never previously observed in
//! Notes.app). Delete both by hand once you've checked them.
//!
//! **What this probe cannot show you directly:** whether a write response's
//! `internetMessageId` was present (vs. Task 5's one-shot refetch fallback
//! firing) is not visible through `SavedNote` — `NoteStore::save_note_full`
//! normalizes that away by construction (the fallback GET, if it fires,
//! backfills the same field `SavedNote.uuid` reads either way). So
//! `wire::decode_or_refetch` logs the ANSWER directly via `crate::log!` on
//! every write this probe makes, and `wire::graph_send` logs the write's
//! full raw response body alongside it. BOTH are gated behind the same
//! opt-in toggle (`wire::LOG_WRITE_BODIES`) — even the id-only line is off
//! by default, because once shipped it would fire on every real user's
//! create/patch/move forever, for a measurement that only this probe needs.
//! This file turns the toggle on for itself via
//! `wire::enable_write_body_logging()` right after reading `MS_TOKEN`, so
//! ONLY this probe's own run gets either log line. Watch the interleaved
//! `[jodd HH:MM:SS.mmm] microsoft …` lines while this runs (they also land
//! in the persistent app log — see its doc comment in `lib.rs`), or `grep
//! 'internetMessageId present\|no usable internetMessageId\|graph_send:'`
//! in that file afterwards.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use jodd_lib::backend::microsoft::MicrosoftVertical;
use jodd_lib::backend::{Identity, NoteStore, SaveOp, SavedNote, Transport};

/// Every id this run created. `destroy_note`/`destroy_folder` are the ONLY
/// way anything gets deleted in this file, and both refuse an id that is
/// not in here — see their doc comments.
#[derive(Default)]
struct Manifest {
    notes: HashSet<String>,
    folders: HashSet<String>,
}

impl Manifest {
    fn created_note(&mut self, id: &str) {
        self.notes.insert(id.to_string());
    }
    fn created_folder(&mut self, id: &str) {
        self.folders.insert(id.to_string());
    }

    /// Delete a note, but ONLY if `id` is one this run itself created.
    /// Mirrors `scripts/ms_write_probe.py`'s `destroy()` — the check runs
    /// against the manifest, not against the caller's intent, so a bug
    /// upstream that hands this the wrong id gets refused rather than
    /// silently deleting a stranger's note. A hard delete on this backend
    /// has no undo, so this is the one invariant the whole file leans on.
    async fn destroy_note(&mut self, v: &MicrosoftVertical, id: &str, what: &str) {
        if !self.notes.contains(id) {
            println!("    REFUSED to delete {what}: id {id:?} was not created by this run — not touching it");
            return;
        }
        match Transport::delete(v, id).await {
            Ok(()) => {
                println!("    cleanup OK: deleted {what} ({id})");
                self.notes.remove(id);
            }
            Err(e) => println!("    cleanup FAILED for {what} ({id}): {e}"),
        }
    }

    /// Folder counterpart of `destroy_note` — same refusal discipline.
    async fn destroy_folder(&mut self, v: &MicrosoftVertical, id: &str, what: &str) {
        if !self.folders.contains(id) {
            println!("    REFUSED to delete {what}: id {id:?} was not created by this run — not touching it");
            return;
        }
        match Transport::delete_folder(v, id).await {
            Ok(()) => {
                println!("    cleanup OK: deleted {what} ({id})");
                self.folders.remove(id);
            }
            Err(e) => println!("    cleanup FAILED for {what} ({id}): {e}"),
        }
    }
}

/// Millisecond-resolution tag so titles/folder names from repeated runs
/// never collide and are trivially greppable in Notes.app.
fn run_tag() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// A fresh `MicrosoftVertical` instance for `folder_ids`.
///
/// Deliberately NOT a single long-lived instance: `MicrosoftVertical::scan`
/// caches the mailbox scan in a `OnceCell` for the instance's own lifetime
/// (see its doc comment — "at most ONE mailbox scan per vertical
/// instance"), exactly like `vertical_for` (lib.rs) constructing a new
/// vertical per operation. A step that needs to OBSERVE the effect of a
/// prior write (the folder rename, the delete) needs a vertical that has
/// never scanned before, or it would see its own stale cache.
fn fresh(token: &str, account_id: &str, folder_ids: &HashMap<String, String>) -> MicrosoftVertical {
    MicrosoftVertical::new(token.to_string(), account_id.to_string(), folder_ids.clone())
}

fn print_saved(what: &str, saved: &SavedNote) {
    println!("    {what} ->");
    println!("      id (Exchange, used for PATCH/DELETE/move): {}", saved.id);
    println!("      uuid (internetMessageId, Jodd's identity):  {}", saved.uuid);
    println!("      version (lastModifiedDateTime):             {}", saved.version);
    println!("      date (RFC 2822, written to the cache):      {}", saved.date);
    println!("      body_html ({} chars, editor-view):          {:?}",
        saved.body_html.len(),
        saved.body_html.chars().take(80).collect::<String>());
    println!("      (full raw Graph response body for this write is in the");
    println!("       [jodd …] log line printed just above/below by wire::graph_send)");
}

#[tokio::main]
async fn main() {
    println!("{}", "=".repeat(78));
    println!("ms_write_probe — Jodd's shipped Microsoft write path, live");
    println!("{}", "=".repeat(78));

    // ── [0] Setup ───────────────────────────────────────────────────────
    let account_id = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: MS_TOKEN='<token>' cargo run --example ms_write_probe -- <account-email>");
        std::process::exit(2);
    });
    println!("\n[0] SETUP");
    println!("    account: {account_id}");

    // Never accept the token as an argument — see this file's module doc
    // for why, and `scripts/ms_probe_filter.py`'s docstring for the
    // convention this mirrors.
    let token = std::env::var("MS_TOKEN").unwrap_or_default().trim().to_string();
    if token.is_empty() {
        println!("    FAIL: MS_TOKEN is not set.\n");
        println!("    python3 scripts/ms_get_token.py --print-token");
        println!("    MS_TOKEN='<paste>' cargo run --example ms_write_probe -- {account_id}");
        std::process::exit(1);
    }
    println!("    token: present ({} chars, never printed)", token.len());

    // Off by default in the shipped app and the test suite (see
    // `LOG_WRITE_BODIES`'s doc comment in wire.rs): a write response body
    // carries the note's actual content, and `crate::log!` persists to a
    // file on disk. This probe is the one caller that turns it on, so the
    // one person running it against their own account gets the full raw
    // response body in the interleaved [jodd …] log lines — see the module
    // doc comment above for how to read them.
    jodd_lib::backend::microsoft::wire::enable_write_body_logging();

    let mut manifest = Manifest::default();

    // ── [1] Discovery ───────────────────────────────────────────────────
    // `list_folders` is real shipped `Transport` code — the only route to a
    // folder list this backend offers (gotcha #12: folders are derived from
    // messages, never enumerated directly).
    println!("\n[1] DISCOVERY — Transport::list_folders on a fresh scan");
    let v0 = fresh(&token, &account_id, &HashMap::new());
    let discovered = match v0.list_folders().await {
        Ok(f) => f,
        Err(e) => {
            println!("    FAIL: list_folders: {e}");
            std::process::exit(1);
        }
    };
    println!("    {} folder(s) visible:", discovered.len());
    for f in &discovered {
        println!("      {:?} -> {}", f.path, f.id);
    }
    let mut folder_ids: HashMap<String, String> =
        discovered.into_iter().map(|f| (f.path, f.id)).collect();

    let Some(notes_root_id) = folder_ids.get("Notes").cloned() else {
        println!("\n    FAIL: no note is filed directly in the 'Notes' root, so its Exchange");
        println!("    id is unreachable (gotcha #12) and this probe cannot create anything.");
        println!("    File one note directly in Notes in Notes.app, wait ~60s, and re-run.");
        std::process::exit(3);
    };
    println!("    Notes root id: {notes_root_id}");

    // ── [2] CREATE — NoteStore::save_note_full, no existing_remote_id ─────
    println!("\n[2] CREATE — save_note_full(existing_remote_id: None) — Note A");
    let v = fresh(&token, &account_id, &folder_ids);
    let tag = run_tag();
    let a_title = format!("jodd-ms-write-probe-A-{tag}");
    let a_uuid = v.mint();
    let create_op = SaveOp {
        title: &a_title,
        body_html: "<div>created by ms_write_probe — step [2] CREATE</div>",
        existing_remote_id: None,
        existing_uuid: Some(&a_uuid),
        existing_created_date: None,
        label: "Notes",
    };
    let note_a = match v.save_note_full(&create_op, &[]).await {
        Ok(saved) => saved,
        Err(e) => {
            println!("    FAIL: save_note_full (create): {e}");
            std::process::exit(1);
        }
    };
    print_saved("CREATE result", &note_a);
    manifest.created_note(&note_a.id);
    let note_a_id = note_a.id.clone();

    // ── [3] PATCH — save_note_full(existing_remote_id: Some), body only ───
    println!("\n[3] PATCH — save_note_full(existing_remote_id: Some) — body change, same title");
    let patch_op = SaveOp {
        title: &a_title,
        body_html: "<div>created by ms_write_probe — step [3] PATCHED the body</div>",
        existing_remote_id: Some(&note_a_id),
        existing_uuid: Some(&note_a.uuid),
        existing_created_date: None,
        label: "Notes",
    };
    match v.save_note_full(&patch_op, &[]).await {
        Ok(saved) => {
            print_saved("PATCH result", &saved);
            report_id_stability(&note_a_id, &saved.id, "PATCH");
        }
        Err(e) => println!("    FAIL: save_note_full (patch): {e}"),
    }

    // ── [4] RETITLE — save_note_full(existing_remote_id: Some), new title ─
    println!("\n[4] RETITLE — save_note_full(existing_remote_id: Some) — title change");
    let a_title_2 = format!("jodd-ms-write-probe-A-RETITLED-{tag}");
    let retitle_op = SaveOp {
        title: &a_title_2,
        body_html: "<div>created by ms_write_probe — step [4] RETITLED</div>",
        existing_remote_id: Some(&note_a_id),
        existing_uuid: Some(&note_a.uuid),
        existing_created_date: None,
        label: "Notes",
    };
    match v.save_note_full(&retitle_op, &[]).await {
        Ok(saved) => {
            print_saved("RETITLE result", &saved);
            report_id_stability(&note_a_id, &saved.id, "RETITLE");
        }
        Err(e) => println!("    FAIL: save_note_full (retitle): {e}"),
    }

    // ── [5] CREATE FOLDER — Transport::create_folder ───────────────────────
    println!("\n[5] CREATE FOLDER — Transport::create_folder — Folder P");
    let folder_path = format!("Notes/JODD-PROBE-{tag}");
    let folder_p = match v.create_folder(&folder_path).await {
        Ok(f) => f,
        Err(e) => {
            println!("    FAIL: create_folder: {e}");
            print_summary_and_exit(&mut manifest, &v, &note_a_id, &a_title, &a_title_2, 1).await;
            return;
        }
    };
    println!("    CREATE FOLDER result -> id {:?} path {:?}", folder_p.id, folder_p.path);
    manifest.created_folder(&folder_p.id);
    folder_ids.insert(folder_p.path.clone(), folder_p.id.clone());

    // ── [6] MOVE — Transport::move_note — into Folder P ────────────────────
    // Destination is `folder_p.path` (what `create_folder` actually
    // RETURNED and what `folder_ids` above was keyed by) rather than the
    // pre-call `folder_path` literal — those two happen to be identical
    // today (`create_child_folder`'s doc comment: `path` on the returned
    // `RemoteFolder` is `name` as given, unchanged), but reading off the
    // return value here removes the assumption instead of relying on it.
    println!("\n[6] MOVE — Transport::move_note — Note A: Notes -> {}", folder_p.path);
    let v = fresh(&token, &account_id, &folder_ids);
    match v.move_note(&note_a_id, std::slice::from_ref(&folder_p.path), &["Notes".to_string()]).await {
        Ok(version) => println!("    MOVE result -> Ok, new version: {version:?}"),
        Err(e) => println!("    FAIL: move_note (into Folder P): {e}"),
    }
    // Confirm on a FRESH scan (this instance's own scan predates the move
    // it just made) — fetch_note is real NoteStore code, not a re-derived
    // check, so this proves the move from the read side too.
    let v_confirm = fresh(&token, &account_id, &folder_ids);
    match v_confirm.fetch_note(&note_a_id).await {
        Ok(n) => println!(
            "    confirm: fetch_note reads Note A's label as {:?} (this backend reports \
             the folder's LEAF name once a note is filed there, not the full Jodd path \
             used to create it — see create_folder's doc comment)",
            n.label
        ),
        Err(e) => println!("    confirm FAILED: fetch_note after move: {e}"),
    }

    // ── [7] RENAME FOLDER — Transport::rename_folder ────────────────────────
    println!("\n[7] RENAME FOLDER — Transport::rename_folder — Folder P");
    let renamed_name = format!("JODD-PROBE-RENAMED-{tag}");
    match v.rename_folder(&folder_p.id, &renamed_name).await {
        Ok(()) => println!("    RENAME result -> Ok, new displayName {renamed_name:?}"),
        Err(e) => println!("    FAIL: rename_folder: {e}"),
    }
    // The folder object itself is unreadable (`GET /mailFolders/{id}` 404s —
    // gotcha #12), so the ONLY route to confirming the rename is the same
    // one the vertical uses: a note filed in it, read back on a fresh scan.
    let v_confirm2 = fresh(&token, &account_id, &folder_ids);
    match v_confirm2.fetch_note(&note_a_id).await {
        Ok(n) => println!("    confirm: fetch_note reads Note A's label as {:?} after the rename", n.label),
        Err(e) => println!("    confirm FAILED: fetch_note after rename: {e}"),
    }

    // ── [8] MOVE BACK — Transport::move_note — Folder P -> Notes ───────────
    // Empties Folder P without deleting Note A, so the folder-delete below
    // exercises a clean delete and — as a side effect — demonstrates the
    // Task 9 distinction this backend needs: an EMPTIED folder must still
    // be listed (only a DELETED one drops out), see `list_folders`'s doc
    // comment in transport.rs.
    println!("\n[8] MOVE BACK — Transport::move_note — Note A: {} -> Notes", folder_p.path);
    let move_back_ok = match v.move_note(&note_a_id, &["Notes".to_string()], std::slice::from_ref(&folder_p.path)).await {
        Ok(version) => {
            println!("    MOVE BACK result -> Ok, new version: {version:?}");
            true
        }
        Err(e) => {
            println!("    FAIL: move_note (back to Notes): {e}");
            false
        }
    };
    let v_confirm3 = fresh(&token, &account_id, &folder_ids);
    match v_confirm3.list_folders().await {
        Ok(fs) => {
            let still_there = fs.iter().any(|f| f.id == folder_p.id);
            println!(
                "    confirm: Folder P (now emptied, not deleted) still appears in \
                 list_folders: {still_there} (expected true — this is the emptied-vs-deleted \
                 distinction Task 9 exists for)"
            );
        }
        Err(e) => println!("    confirm FAILED: list_folders after emptying Folder P: {e}"),
    }

    // ── [9] DELETE FOLDER — Transport::delete_folder ────────────────────────
    println!("\n[9] DELETE FOLDER — Transport::delete_folder — Folder P");
    if !move_back_ok {
        // Deleting a folder that still contains a note deletes the note WITH
        // it (measured by `scripts/ms_write_probe.py`'s probe [3]: "the
        // witness note lived inside that folder and went with it"). Note A
        // is meant to survive this whole run for human inspection, so if
        // step [8] could not confirm it left Folder P, deleting Folder P
        // here would be exactly the code path this file's self-review must
        // rule out — skip it and leave Folder P for manual cleanup instead.
        println!(
            "    SKIPPED: step [8]'s move-back did not confirm success, and Folder P may \
             still contain Note A — deleting it now could delete Note A along with it. \
             Leaving Folder P in place; check Notes.app and clean up by hand."
        );
    } else {
        manifest.destroy_folder(&v, &folder_p.id, "Folder P").await;
        let v_confirm4 = fresh(&token, &account_id, &folder_ids);
        match v_confirm4.list_folders().await {
            Ok(fs) => {
                let still_there = fs.iter().any(|f| f.id == folder_p.id);
                println!(
                    "    confirm: Folder P appears in list_folders after delete: {still_there} \
                     (expected false)"
                );
            }
            Err(e) => println!("    confirm FAILED: list_folders after deleting Folder P: {e}"),
        }
    }

    // ── [10] Transport::save + Transport::delete — Note B, full round trip ─
    println!("\n[10] CREATE via Transport::save, then DELETE via Transport::delete — Note B");
    let b_title = format!("jodd-ms-write-probe-B-delete-roundtrip-{tag}");
    let b_uuid = v.mint();
    let create_b_op = SaveOp {
        title: &b_title,
        body_html: "<div>created by ms_write_probe — step [10], exercises Transport::save \
                     and Transport::delete directly rather than save_note_full</div>",
        existing_remote_id: None,
        existing_uuid: Some(&b_uuid),
        existing_created_date: None,
        label: "Notes",
    };
    match v.save(create_b_op).await {
        Ok(outcome) => {
            println!("    Transport::save result -> remote_id {}", outcome.remote_id);
            manifest.created_note(&outcome.remote_id);
            let v_confirm5 = fresh(&token, &account_id, &folder_ids);
            match v_confirm5.fetch_note(&outcome.remote_id).await {
                Ok(n) => println!("    confirm: fetch_note finds Note B, title {:?}", n.title),
                Err(e) => println!("    confirm FAILED: fetch_note for Note B before delete: {e}"),
            }
            manifest.destroy_note(&v, &outcome.remote_id, "Note B (delete round-trip)").await;
            let v_confirm6 = fresh(&token, &account_id, &folder_ids);
            match v_confirm6.fetch_note(&outcome.remote_id).await {
                Ok(n) => println!(
                    "    confirm UNEXPECTED: fetch_note still finds Note B after delete \
                     (title {:?}) — the delete may not have taken",
                    n.title
                ),
                Err(e) => println!("    confirm: fetch_note after delete correctly fails: {e} (expected NotFound)"),
            }
        }
        Err(e) => println!("    FAIL: Transport::save (create Note B): {e}"),
    }

    // ── [11] EMPTY TITLE — save_note_full with an empty title ──────────────
    // Task 3's guard: an empty/whitespace-only title emits an explicit empty
    // title line, because without one the injected body is bit-identical to
    // "no title at all" and the stripper eats the body's own first <div> —
    // destroying content in a loop. The guard holds by construction against
    // the stripper (covered by unit tests); whether Apple RENDERS it
    // acceptably has never been run before this probe. Left behind — see
    // "LEFT FOR YOU TO INSPECT" in the summary.
    println!("\n[11] EMPTY TITLE — save_note_full with title: \"\" — Note C");
    let c_uuid = v.mint();
    let empty_title_op = SaveOp {
        title: "",
        body_html: "<div>created by ms_write_probe — step [11], empty title (Task 3's guard)</div>",
        existing_remote_id: None,
        existing_uuid: Some(&c_uuid),
        existing_created_date: None,
        label: "Notes",
    };
    let note_c = v.save_note_full(&empty_title_op, &[]).await;
    let note_c_id = match note_c {
        Ok(saved) => {
            print_saved("EMPTY TITLE result", &saved);
            manifest.created_note(&saved.id);
            Some(saved.id)
        }
        Err(e) => {
            println!("    FAIL: save_note_full (empty title): {e}");
            None
        }
    };

    print_summary_and_exit(
        &mut manifest,
        &v,
        &note_a_id,
        &a_title,
        &a_title_2,
        if note_c_id.is_some() { 0 } else { 1 },
    )
    .await;
}

/// `PATCH` and `move_note` are both documented as real in-place operations
/// on this backend (unlike Gmail's insert-new + trash-old) — this is the
/// live check for that claim, printed rather than panicked so one surprise
/// doesn't abort a report that's otherwise informative.
fn report_id_stability(before: &str, after: &str, what: &str) {
    if before == after {
        println!("      id unchanged by {what}: true (expected — this backend PATCHes in place)");
    } else {
        println!(
            "      id unchanged by {what}: FALSE — was {before}, now {after}. This backend was \
             measured to PATCH in place; if this actually differs live, `SavedNote.version` \
             (not `id`) is still what `mark_pushed` must stamp — see its doc comment."
        );
    }
}

/// Final report: what got cleaned up automatically, what's left behind on
/// purpose for a human to look at in Notes.app, and the checklist to run
/// through. Always runs, even on a mid-probe failure, so a partial run still
/// tells the operator what state the mailbox is in.
async fn print_summary_and_exit(
    manifest: &mut Manifest,
    v: &MicrosoftVertical,
    note_a_id: &str,
    note_a_title_before_retitle: &str,
    note_a_title: &str,
    exit_code: i32,
) {
    println!("\n{}", "=".repeat(78));
    println!("SUMMARY");
    println!("{}", "=".repeat(78));

    // Note A is deliberately NOT auto-deleted — see the module doc comment.
    // Remove it from the manifest so nothing downstream could accidentally
    // sweep it up; destroy_note's own guard would already refuse it once
    // it's out, but this makes the intent explicit at the call site too.
    manifest.notes.remove(note_a_id);

    // Final sanity read on Note A, through the SAME instance the rest of
    // this run used for its writes. `v`'s own mailbox scan (the
    // `OnceCell` `MicrosoftVertical::scan` doc comment describes) was never
    // triggered by any call this file made on `v` itself — every
    // confirmation above deliberately used a SEPARATE `fresh(...)`
    // instance instead (see `fresh`'s doc comment for why) — so this
    // `fetch_note` performs `v`'s first and only scan, live at this exact
    // moment. Confirms Note A is still there (nothing above accidentally
    // deleted the one thing meant to survive) and shows exactly the
    // title/label a human will see in Notes.app.
    match v.fetch_note(note_a_id).await {
        Ok(n) => println!(
            "\nNote A final read-back: title {:?}, label {:?} — still present, as expected",
            n.title, n.label
        ),
        Err(e) => println!(
            "\nNote A final read-back FAILED ({e}) — it should still exist; investigate before \
             trusting the checklist below"
        ),
    }

    if !manifest.notes.is_empty() || !manifest.folders.is_empty() {
        println!(
            "\n{} note(s) and {} folder(s) created by this run were not cleaned up \
             (a step above may have failed before reaching its own cleanup):",
            manifest.notes.len(),
            manifest.folders.len()
        );
        for id in &manifest.notes {
            println!("    note   {id}");
        }
        for id in &manifest.folders {
            println!("    folder {id}");
        }
        println!("Remove these in Notes.app, or re-run — destroy_note/destroy_folder refuse");
        println!("anything not in THIS run's own manifest, so a stale id here is inert.");
    } else {
        println!("\nEverything auto-cleaned by this run was removed successfully.");
    }

    println!("\nLEFT FOR YOU TO INSPECT (not auto-deleted — delete by hand once checked):");
    println!("    Note A — id {note_a_id}");
    println!("      title: {note_a_title:?}");
    println!("      Went through: create -> patch -> retitle -> move -> move back.");
    println!("      Should be sitting in the Notes root with exactly this title, one");
    println!("      copy, no duplicate title line in the body.");
    println!("    Note C — the empty-title note from step [11], if it was created");
    println!("      (see above for its id — search Notes.app for a blank-titled note");
    println!("      containing \"step [11], empty title\").");

    println!("\nCHECKLIST — verify by eye in Notes.app on the Mac AND on the iPhone:");
    println!("    [ ] Note A shows title {note_a_title:?}, sitting in the Notes root");
    println!("        (not in the now-deleted probe folder), with no duplicate title");
    println!("        line inside the body and no second copy anywhere.");
    println!("    [ ] Note C exists with an EMPTY title and renders acceptably — this");
    println!("        is the shape that has never been observed in Notes.app before.");
    println!("    [ ] No folder named \"JODD-PROBE-<digits>\" or");
    println!("        \"JODD-PROBE-RENAMED-<digits>\" remains anywhere.");
    println!("    [ ] No note titled \"jodd-ms-write-probe-B-delete-roundtrip-<digits>\"");
    println!("        remains anywhere (Note B was created and deleted by this run).");
    println!("    [ ] No note titled {note_a_title_before_retitle:?} (the PRE-retitle name)");
    println!("        remains — only the RETITLED name above should be visible.");
    println!("    [ ] Scroll back through this run's terminal output for any \"FAIL\"");
    println!("        or \"confirm FAILED\"/\"confirm UNEXPECTED\" line — none should");
    println!("        appear in a clean run.");
    println!(
        "    [ ] In the interleaved [jodd …] log lines: at least one \"internetMessageId \
         present\" or \"no usable internetMessageId\" line per write, confirming whether \
         Task 5's refetch fallback ever actually fires live (deferred measurement 1)."
    );

    std::process::exit(exit_code);
}
