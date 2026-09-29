//! Read-only probe for the ONE question gotcha #12 leaves open in a way that
//! blocks writes: **can Jodd learn the `Notes` folder id in a mailbox that
//! holds zero notes?**
//!
//! `MicrosoftVertical::save_note_full` refuses a create with `no Exchange
//! folder id for '<path>'` when `folder_ids` has no entry for the destination,
//! and the only documented route to a folder id is `parentFolderId` on a
//! message (gotcha #12). A mailbox never used with Apple Notes has no message
//! to read one off, so the first note can never be filed — a bootstrap
//! deadlock, measured live 2026-08-17 on `TheArchitect@renny.co.th`.
//!
//! Read-only by default: every request is a GET unless `--write` is passed,
//! which adds one `POST` that creates a real note (see [`write_avenue`]).
//!
//! Lives in `examples/`, never `src/bin/` — gotcha #3.
//!
//! Usage:
//!     cargo run --example ms_folder_probe -- <account-email>
//!     cargo run --example ms_folder_probe -- <account-email> --write
//!
//! If the account was signed in through the packaged app, the keychain item
//! is ACL'd to that binary and this one cannot read it. Pass the token in:
//!
//!     JODD_MS_REFRESH_TOKEN="$(security find-generic-password \
//!         -s jodd -a 'rt::<account-email>' -w)" \
//!       cargo run --example ms_folder_probe -- <account-email>

const GRAPH_V1: &str = "https://graph.microsoft.com/v1.0";
const GRAPH_BETA: &str = "https://graph.microsoft.com/beta";

/// `PR_CONTAINER_CLASS` — the property that distinguishes an `IPF.StickyNote`
/// container (what Apple's Notes sync writes into) from an ordinary
/// `IPF.Note` mail folder. Graph never surfaces it as a first-class field.
const PROP_CONTAINER_CLASS: &str = "String%200x3613";

async fn probe(client: &reqwest::Client, token: &str, label: &str, url: &str) {
    println!("\n### {label}\n    GET {url}");
    match client.get(url).bearer_auth(token).send().await {
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            let shown: String = body.chars().take(1600).collect();
            println!("    -> {status}\n{shown}");
        }
        Err(e) => println!("    -> transport error: {e}"),
    }
}

#[tokio::main]
async fn main() {
    let account_id = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: cargo run --example ms_folder_probe -- <account-email>");
        std::process::exit(2);
    });

    // `run()` loads this for the app; an example never calls `run()`, and
    // `auth_ms::client_id()` reads `MS_CLIENT_ID` straight out of the
    // environment — without this the token refresh fails with an empty
    // client id rather than anything that names the cause.
    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".env");
    dotenv::from_path(&env_path).ok();

    let _ = jodd_lib::secrets::init();
    println!("account: {account_id}");

    // The keychain item is ACL'd to whichever binary created it — the signed
    // `Jodd.app`, not a `cargo run --example` build — so `get_password` fails
    // here for an account signed in through the app. `JODD_MS_REFRESH_TOKEN`
    // is the escape hatch: pass the value in from `security
    // find-generic-password -s jodd -a rt::<account> -w` without ever
    // printing it.
    let rt = match std::env::var("JODD_MS_REFRESH_TOKEN") {
        Ok(v) if !v.is_empty() => {
            println!("refresh token: from JODD_MS_REFRESH_TOKEN ({} chars)", v.len());
            v
        }
        _ => match jodd_lib::accounts::load_refresh_token(&account_id) {
            Some(rt) => {
                println!("refresh token: from keychain ({} chars)", rt.len());
                rt
            }
            None => {
                println!(
                    "FAIL: no refresh token — keychain read failed (ACL?) and \
                     JODD_MS_REFRESH_TOKEN is unset"
                );
                std::process::exit(1);
            }
        },
    };

    let token = match jodd_lib::auth_ms::refresh_access_token(&rt).await {
        Ok(t) => t.access_token,
        Err(e) => {
            println!("FAIL at refresh_access_token:\n  {e}");
            std::process::exit(1);
        }
    };
    println!("access token: obtained ({} chars)", token.len());

    let c = reqwest::Client::new();

    // Is the mailbox actually empty of notes? Everything below only matters if
    // it is — that is the deadlock's precondition.
    probe(&c, &token, "whole-mailbox sticky notes", &format!(
        "{GRAPH_V1}/me/messages?$top=5&$select=id,subject,parentFolderId\
         &$filter=singleValueExtendedProperties/any(ep:ep/id eq 'String 0x001A' and ep/value eq 'IPM.StickyNote')"
    )).await;

    // gotcha #12's baseline, re-measured on THIS account: the Notes tree is
    // absent from `mailFolders`. Measured previously only on a personal
    // live.com account; this one is work/school (a different tenant class),
    // so the result is worth re-establishing rather than assuming.
    probe(&c, &token, "mailFolders root listing", &format!(
        "{GRAPH_V1}/me/mailFolders?$top=100&$select=id,displayName,childFolderCount"
    )).await;
    probe(&c, &token, "mailFolders + hidden", &format!(
        "{GRAPH_V1}/me/mailFolders?$top=100&includeHiddenFolders=true&$select=id,displayName"
    )).await;
    probe(&c, &token, "msgfolderroot children + hidden + container class", &format!(
        "{GRAPH_V1}/me/mailFolders/msgfolderroot/childFolders?$top=100&includeHiddenFolders=true\
         &$select=id,displayName&$expand=singleValueExtendedProperties($filter=id%20eq%20'{PROP_CONTAINER_CLASS}')"
    )).await;

    // The avenue gotcha #12 records as "absent from wellKnownFolderName" —
    // absent from the DOCUMENTED enum, which is not the same as rejected by
    // the implementation. EWS's DistinguishedFolderIdNameType does have
    // `notes`, and Graph's enum descends from it.
    for base in [GRAPH_V1, GRAPH_BETA] {
        let tag = if base == GRAPH_V1 { "v1.0" } else { "beta" };
        probe(&c, &token, &format!("wellKnownFolderName 'notes' ({tag})"),
            &format!("{base}/me/mailFolders/notes?$select=id,displayName")).await;
        probe(&c, &token, &format!("'notes' messages ({tag})"),
            &format!("{base}/me/mailFolders/notes/messages?$top=1&$select=id,subject")).await;
        probe(&c, &token, &format!("'notes' childFolders ({tag})"),
            &format!("{base}/me/mailFolders/notes/childFolders?$top=50&$select=id,displayName")).await;
    }

    // Long shots, cheap to run while we hold a token.
    probe(&c, &token, "wellKnownFolderName 'stickynotes'", &format!(
        "{GRAPH_V1}/me/mailFolders/stickynotes?$select=id,displayName"
    )).await;
    probe(&c, &token, "mailFolders/delta (first page)", &format!(
        "{GRAPH_V1}/me/mailFolders/delta?$select=id,displayName"
    )).await;
    probe(&c, &token, "mailFolders filtered by displayName eq 'Notes'", &format!(
        "{GRAPH_V1}/me/mailFolders?$filter=displayName%20eq%20'Notes'&$select=id,displayName"
    )).await;

    if std::env::args().any(|a| a == "--write") {
        write_avenue(&c, &token).await;
    } else {
        println!("\n(read-only run — pass --write to create one note by well-known name)");
    }
}

/// The avenue gotcha #12's log never tried: reach the Notes folder by NAME
/// rather than by the id discovery cannot produce.
///
/// **This one WRITES.** It creates a real note in the account's Notes folder
/// and deliberately leaves it there — a note in that folder is exactly what
/// the deadlock needs, and deleting it would put the mailbox straight back
/// into the state being diagnosed. Delete it from Jodd, Outlook or the iPhone
/// once the folder id has been learned.
async fn write_avenue(c: &reqwest::Client, token: &str) {
    println!("\n══ WRITE AVENUE ══");

    // Avenue 0, found by the read pass above and cheaper than everything
    // that follows: address the folder by NAME instead of by id.
    // `GET /me/mailFolders/notes` 404s (the folder object is unreadable —
    // gotcha #12's signature for a genuine Notes-tree member), but
    // `/me/mailFolders/notes/messages` answers 200, and a name Graph does
    // NOT recognise answers 400 `ErrorInvalidIdMalformed` instead
    // (measured: `stickynotes`). So the segment resolves. If a POST to that
    // same collection lands an `IPM.StickyNote` in the real Notes
    // container, the whole deadlock dissolves: no folder id is needed to
    // file the first note, and the response's `parentFolderId` hands Jodd
    // the id discovery could never reach.
    println!("\n### create DIRECTLY in mailFolders('notes')\n    POST {GRAPH_V1}/me/mailFolders/notes/messages");
    let direct = c
        .post(format!("{GRAPH_V1}/me/mailFolders/notes/messages"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "subject": "jodd bootstrap probe A — safe to delete",
            "body": { "contentType": "HTML", "content": "<div>jodd bootstrap probe A</div>" },
            "singleValueExtendedProperties": [
                { "id": "String 0x001A", "value": "IPM.StickyNote" }
            ]
        }))
        .send()
        .await;
    match direct {
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let id = v["id"].as_str().unwrap_or_default().to_string();
            let parent = v["parentFolderId"].as_str().unwrap_or_default().to_string();
            println!("    -> {status}");
            if status.is_success() && !id.is_empty() {
                println!("    *** CREATED IN NOTES BY NAME ***");
                println!("    parentFolderId = {parent}");
                // Confirm it is really in the Notes tree the way a scan would
                // see it, then confirm the id is usable the way Jodd uses one.
                probe(c, token, "  re-read notes/messages", &format!(
                    "{GRAPH_V1}/me/mailFolders/notes/messages?$top=5&$select=id,subject,parentFolderId"
                )).await;
                probe(c, token, "  scan sees it as a sticky note", &format!(
                    "{GRAPH_V1}/me/messages?$top=5&$select=id,subject,parentFolderId\
                     &$filter=singleValueExtendedProperties/any(ep:ep/id eq 'String 0x001A' and ep/value eq 'IPM.StickyNote')"
                )).await;
                probe(c, token, "  messages under the discovered id", &format!(
                    "{GRAPH_V1}/me/mailFolders/{}/messages?$top=5&$select=id,subject",
                    urlencoding::encode(&parent)
                )).await;
                println!("\n    leaving probe A in place — a note in the Notes folder is\n\
                          \x20   exactly what the deadlock needs, and the user can delete it\n\
                          \x20   from Jodd/Outlook/iPhone. id={id}");
                return;
            }
            println!("{}", &body[..900.min(body.len())]);
        }
        Err(e) => println!("    -> transport error: {e}"),
    }

    // The other candidate avenue, deliberately not implemented: `POST
    // /me/messages` (which always lands in Drafts) followed by `POST
    // /me/messages/{{id}}/move` with `destinationId: "notes"` — the move docs
    // do say `destinationId` accepts a well-known folder name. It is the next
    // thing to try if the create above ever regresses, but it is strictly
    // worse while that one works: a sticky note parked in Drafts is not in
    // the container Apple syncs, and its `parentFolderId` would teach Jodd
    // the DRAFTS id.
    println!("\n    the name-addressed create did not succeed — see the body above");
}
