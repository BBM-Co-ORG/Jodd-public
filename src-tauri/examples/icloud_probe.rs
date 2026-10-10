//! Prove Jodd can read Apple Notes straight from iCloud — in Rust, with no
//! Node, no Playwright, and (deliberately) no protobuf.
//!
//! Background: Jodd's two shipped verticals both reach Apple Notes through an
//! *email* backend, which only exists for accounts the user attached to Notes
//! (Gmail, Exchange). An iCloud-native account has no such backend. The web
//! client talks to CloudKit's private database service instead, and the
//! 2026-08-18 probe (docs/PRIOR-ART.md) established that path works. This is
//! that path, reimplemented here, so the claim rests on our own code.
//!
//! **It also answers verify-first items 4 and 5** (see the M1 design spec):
//! whether the decoded body repeats the title as its first line, what
//! character separates that line from the next, and whether `ParentFolder`
//! yields real folder nesting. Both need a live account and nothing else —
//! any machine with an unexpired session will do, no Mac required.
//!
//! **Why (almost) no protobuf.** A Note record carries `TitleEncrypted` and
//! `SnippetEncrypted` as base64 of *plain text* — the name describes Apple's
//! server-side at-rest encryption, not anything we must undo. Only the note
//! BODY (`TextDataEncrypted`) is a zlib-compressed CRDT document needing a
//! schema. So titles, folder membership and timestamps are reachable with zero
//! schema work, which is what makes a read-only M1 much smaller than "port the
//! content model". The table below still stops at that line to demonstrate it;
//! the sections after it decode bodies through `backend::icloud::doc`, which
//! exists now and did not when this probe was first written.
//!
//! **The session is borrowed, and that has gone stale — read this before
//! running it.** It reads the cookie jar `icloud-md` stored under
//! `~/.config/icloud-md/`, which was scaffolding for the M1 verify-first phase,
//! when Jodd had no iCloud session of its own. **It has one now** —
//! `icloud_auth.rs`, shipped with M1 — so the M2 census runs in the app
//! instead, against the session it is already holding:
//!
//! ```js
//! await __TAURI__.core.invoke('icloud_census', { accountId: 'icloud:you@me.com' })
//! ```
//!
//! What is left here that the command does not carry: the two M1 reports below
//! (title repetition, folder nesting). `report_title_repetition` prints note
//! content as codepoints on purpose — a mismatch counted but not seen is the
//! question restated — which is exactly why it is NOT in a command that logs.
//! Those still need a live jar; the census does not.
//!
//! Lives in `examples/`, never `src/bin/` — gotcha #3: Tauri's bundler copies
//! every `[[bin]]` into `Contents/MacOS/`, and macOS Tahoe refuses to launch a
//! bundle with more than one binary there.
//!
//! Usage:
//!     cargo run --example icloud_probe
//!     cargo run --example icloud_probe -- /path/to/session.local.json
//!
//! The cookie jar is never printed.

use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use jodd_lib::backend::icloud::doc;
use serde_json::{json, Value};

/// Limit the scan to one folder, by its display name.
///
///     JODD_PROBE_FOLDER=FolderforJoddTesting cargo run --example icloud_probe
///
/// **This is the read-side of PRIOR-ART practice #1's containment idea.** That
/// rule was written for tests that DELETE, but the reasoning transfers: on a
/// real Apple ID with hundreds of personal notes, touching only the folder made
/// for testing is both less data handled and a far more readable report. The
/// aggregate statistics stay honest either way — they describe whatever was
/// scanned, and the header says which.
fn folder_filter() -> Option<String> {
    std::env::var("JODD_PROBE_FOLDER").ok().filter(|s| !s.is_empty())
}

/// How many per-note rows to print. Statistics always cover EVERYTHING
/// scanned; this bounds only the listing.
///
/// A real account has hundreds of notes, and 766 rows of personal titles in a
/// terminal is both useless and a disclosure. The counts are what answer the
/// verify-first questions; the rows are just for eyeballing.
fn row_limit() -> usize {
    std::env::var("JODD_PROBE_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
}

/// Whether to withhold note and folder text from the output.
///
/// Off by default: on a throwaway account the titles are what make the report
/// readable. On a **real** Apple ID they are personal data, and this probe's
/// output is meant to be pasted into a conversation — so there has to be a
/// mode where that is safe.
///
/// What survives redaction is everything the verify-first questions actually
/// need: counts, lengths, verdicts, boundary codepoints, compression
/// containers, folder depths. What goes is the text itself.
///
///     JODD_PROBE_REDACT=1 cargo run --example icloud_probe
fn redacting() -> bool {
    std::env::var_os("JODD_PROBE_REDACT").is_some()
}

/// A string as it may be shown: itself, or its shape.
fn shown(s: &str) -> String {
    if redacting() {
        format!("<{} ch>", s.chars().count())
    } else {
        s.to_string()
    }
}

/// Captured from a real web-client session, exactly as icloud-md documents:
/// there is no way to derive these, and they may need bumping when Apple ships
/// a new web build. Kept loud and isolated rather than buried at a call site.
const CKJS_BUILD_VERSION: &str = "2310ProjectDev27";
const CKJS_VERSION: &str = "2.6.4";
const SETUP_HOST: &str = "https://setup.icloud.com";

/// The web client asks for this whole set; sending an unfamiliar shape is the
/// kind of thing a private API notices, so we ask for what it asks for even
/// though this probe reads four of them.
const DESIRED_KEYS: &[&str] = &[
    "TitleEncrypted", "SnippetEncrypted", "FirstAttachmentUTIEncrypted",
    "FirstAttachmentThumbnail", "FirstAttachmentThumbnailOrientation",
    "CreationDate", "ModificationDate", "Deleted", "Folders", "Folder",
    "Attachments", "ParentFolder", "Note", "LastViewedModificationDate",
    "MinimumSupportedNotesVersion", "DisplayTextEncrypted",
    "StandardizedContentEncrypted", "TokenContentIdentifierEncrypted",
    "AltTextEncrypted", "UTIEncrypted", "MergeableDataEncrypted", "IsPinned",
    "TextDataEncrypted",
];

const DESIRED_RECORD_TYPES: &[&str] = &[
    "AccountData", "Note", "SearchIndexes", "Folder", "PasswordProtectedNote",
    "User", "Users", "Note_UserSpecific", "PasswordProtectedNote_UserSpecific",
    "Folder_UserSpecific", "cloudkit.share", "Hashtag", "InlineAttachment",
];

struct Session {
    cookie: String,
    client_id: String,
    client_build_number: String,
    client_mastering_number: String,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("\n✗ {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let session = load_session(std::env::args().nth(1))?;
    let http = reqwest::Client::new();

    // 1. Validate: confirms the session AND bootstraps the account — the dsid
    //    and which `p<N>-ckdatabasews` partition serves it. There is no way to
    //    guess the partition host; it must come from here.
    let (dsid, apple_id, ck_host) = validate(&http, &session).await?;
    if redacting() {
        println!("● JODD_PROBE_REDACT is on — note and folder text is withheld");
        println!("✓ signed in as <redacted Apple ID>");
    } else {
        println!("✓ signed in as {apple_id}  (dsid {dsid})");
    }
    println!("  CloudKit host: {ck_host}");

    // 2. Walk the private Notes zone.
    let records = fetch_zone(&http, &session, &ck_host, &dsid).await?;
    println!("✓ fetched {} record(s) from the Notes zone\n", records.len());

    // 3. Folder records first: a note points at its folder by record id, so the
    //    id → name map has to exist before notes can be printed readably.
    //    `parent` comes along for verify-first #5 (does ParentFolder nest?).
    let mut folders: HashMap<String, Folder> = HashMap::new();
    for r in &records {
        if r["recordType"] == "Folder" {
            let name = decoded_text(&r["fields"]["TitleEncrypted"])
                .unwrap_or_else(|| record_name(r).to_string());
            let parent = r["fields"]["ParentFolder"]["value"]["recordName"]
                .as_str()
                .map(String::from);
            folders.insert(record_name(r).to_string(), Folder { name, parent });
        }
    }

    // `PasswordProtectedNote` is deliberately NOT in here, and that matters
    // more than it looks: a locked note IS genuinely encrypted, so counting it
    // as an unreadable `Note` would push the ADP verdict toward "this account
    // cannot work" on an account that is merely using a per-note lock. A
    // separate record type is the whole reason that stays easy.
    let locked = records.iter().filter(|r| r["recordType"] == "PasswordProtectedNote").count();

    let mut notes: Vec<&Value> = records
        .iter()
        .filter(|r| r["recordType"] == "Note" && r["fields"]["Deleted"]["value"] != json!(1))
        .collect();

    let scope = folder_filter();
    if let Some(want) = &scope {
        let ids: Vec<&String> = folders
            .iter()
            .filter(|(_, f)| &f.name == want)
            .map(|(id, _)| id)
            .collect();
        if ids.is_empty() {
            return Err(anyhow!(
                "no folder named {want:?} in this account — check the name, or unset \
                 JODD_PROBE_FOLDER to scan everything"
            ));
        }
        notes.retain(|n| {
            let f = n["fields"]["Folder"]["value"]["recordName"].as_str().unwrap_or("");
            ids.iter().any(|id| id.as_str() == f)
        });
        println!("● scoped to folder {want:?} — {} note(s)", notes.len());
    }
    if locked > 0 {
        println!(
            "● {locked} password-protected note(s) present. Excluded: they are a per-note\n  \
             lock, NOT Advanced Data Protection, and must never count toward an ADP verdict."
        );
    }

    println!("{:<28} {:<20} {:<44} {}", "MODIFIED", "FOLDER", "TITLE", "BODY");
    println!("{}", "─".repeat(108));
    for n in notes.iter().take(row_limit()) {
        let title = decoded_text(&n["fields"]["TitleEncrypted"]).unwrap_or_default();
        let folder_id = n["fields"]["Folder"]["value"]["recordName"].as_str().unwrap_or("");
        let folder = folders.get(folder_id).map(|f| f.name.as_str()).unwrap_or(folder_id);
        let modified = n["fields"]["ModificationDate"]["value"].as_i64().map(ms_to_rfc3339).unwrap_or_default();
        // Deliberately NOT decoded — its size is the point: this is the only
        // field that needs the CRDT schema, and everything else above did not.
        let body = n["fields"]["TextDataEncrypted"]["value"]
            .as_str()
            .map(|b64| format!("{} B zlib+protobuf (not decoded)", b64.len() * 3 / 4))
            .unwrap_or_else(|| "—".into());
        println!(
            "{modified:<28} {:<20} {:<44} {body}",
            truncate(&shown(folder), 20),
            truncate(&shown(&title), 44)
        );
    }

    if notes.len() > row_limit() {
        println!(
            "… {} more not listed (JODD_PROBE_ROWS to change). Every count below still \ncovers all {} notes.",
            notes.len() - row_limit(),
            notes.len()
        );
    }

    println!("\n{} note(s), {} folder(s).", notes.len(), folders.len());
    println!("Every column above came from base64 and JSON alone — no protobuf schema involved.");

    report_title_repetition(&notes)?;
    // The M2 census lives in the library (`backend::icloud::census`), not here.
    // It has to run against an account Jodd is signed into, and THIS probe
    // borrows icloud-md's stored cookie jar — scaffolding from before Jodd had
    // an iCloud session of its own, which expires and needs a HAR capture
    // through a third-party tool to refresh. The `icloud_census` command runs
    // the same function against the app's own live session; this call keeps the
    // probe useful for anyone who already has a jar, without a second
    // implementation to drift.
    print!("{}", jodd_lib::backend::icloud::census::report(&records));
    report_folder_nesting(&folders);
    Ok(())
}

/// A `Folder` record reduced to the two things the tree needs.
struct Folder {
    name: String,
    parent: Option<String>,
}

/// Reads icloud-md's stored cookie jar. Defaults to the single account under
/// its accounts dir, which is the common case and saves passing a path.
fn load_session(explicit: Option<String>) -> Result<Session> {
    let path = match explicit {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let root = dirs::home_dir()
                .ok_or_else(|| anyhow!("no home directory"))?
                .join(".config/icloud-md/accounts");
            let mut dirs_found: Vec<_> = std::fs::read_dir(&root)
                .with_context(|| format!("no session store at {}", root.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect();
            dirs_found.sort();
            match dirs_found.len() {
                0 => return Err(anyhow!(
                    "no accounts under {} — import a session first", root.display()
                )),
                1 => dirs_found[0].join("session.local.json"),
                // Taking the first of several is a silent wrong answer: the
                // directories are named by dsid, so which one sorts first is
                // arbitrary and has nothing to do with which account you meant.
                // Reading the wrong Apple ID and reporting confidently about it
                // is worse than refusing — and it matters most exactly when a
                // throwaway account and a REAL one are both present.
                _ => {
                    let list = dirs_found
                        .iter()
                        .map(|d| format!("      {}", d.display()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Err(anyhow!(
                        "{} accounts are stored and this probe will not guess between them:\n{}\n\n\
                         Pass the one you mean:\n      \
                         cargo run --example icloud_probe -- <path>/session.local.json\n\n\
                         Or delete the account directory you are finished with.",
                        dirs_found.len(), list
                    ));
                }
            }
        }
    };
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading session {}", path.display()))?;
    let v: Value = serde_json::from_str(&raw)?;
    let get = |k: &str| -> Result<String> {
        v[k].as_str().map(String::from).ok_or_else(|| anyhow!("session file has no {k}"))
    };
    Ok(Session {
        cookie: get("cookie")?,
        client_id: get("clientId")?,
        client_build_number: get("clientBuildNumber")?,
        client_mastering_number: get("clientMasteringNumber")?,
    })
}

async fn validate(http: &reqwest::Client, s: &Session) -> Result<(String, String, String)> {
    let url = format!(
        "{SETUP_HOST}/setup/ws/1/validate?clientBuildNumber={}&clientMasteringNumber={}&clientId={}&requestId={}",
        s.client_build_number, s.client_mastering_number, s.client_id, uuid::Uuid::new_v4()
    );
    let resp = http
        .post(&url)
        .header("Cookie", &s.cookie)
        .header("Origin", "https://www.icloud.com")
        .header("Referer", "https://www.icloud.com/")
        .header("Accept", "application/json")
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        // 421 is specifically "session expired" — the failure this probe hits
        // most, and the one with an actionable fix.
        // The 421 this hits most is icloud-md's jar expiring, and the fix is
        // NOT to go and refresh a third-party tool's session: Jodd has held its
        // own since M1. Point at the command that uses it.
        let hint = if status.as_u16() == 421 {
            "\n      This probe reads icloud-md's cookie jar, which is scaffolding from              before\n      Jodd had an iCloud session of its own — and that jar has expired.             \n\n      The census does not need it. Run Jodd, and in devtools:\n                     await __TAURI__.core.invoke('icloud_census', { accountId: 'icloud:you@me.com' })             \n\n      Only the M1 sections below (title repetition, folder nesting) still              need\n      a jar — refresh it with icloud-md's import-har if you want those."
        } else {
            ""
        };
        return Err(anyhow!("validate failed: HTTP {status}{hint}"));
    }
    let body: Value = resp.json().await?;
    // A password-accepted-but-2FA-pending session also answers 200 here.
    if body["hsaChallengeRequired"] == json!(true) || body["dsInfo"]["hsaChallengeRequired"] == json!(true) {
        return Err(anyhow!("session is only partially authenticated (2FA still pending)"));
    }
    let dsid = body["dsInfo"]["dsid"].as_str().ok_or_else(|| anyhow!("no dsid in /validate"))?;
    let apple_id = body["dsInfo"]["appleId"].as_str().unwrap_or("<unknown>");
    let host = body["webservices"]["ckdatabasews"]["url"]
        .as_str()
        .ok_or_else(|| anyhow!("/validate carried no ckdatabasews host — is Notes enabled on this account?"))?;
    Ok((dsid.to_string(), apple_id.to_string(), host.to_string()))
}

/// Pages `changes/zone` until `moreComing` is false, the same incremental-sync
/// model the web client uses. The returned `syncToken` is what an eventual
/// `accounts.sync_cursor` would hold; this probe fetches from scratch and
/// prints it rather than persisting anything.
async fn fetch_zone(http: &reqwest::Client, s: &Session, ck_host: &str, dsid: &str) -> Result<Vec<Value>> {
    let url = format!(
        "{ck_host}/database/1/com.apple.notes/production/private/changes/zone\
         ?ckjsBuildVersion={CKJS_BUILD_VERSION}&ckjsVersion={CKJS_VERSION}&clientId={}\
         &clientBuildNumber={}&clientMasteringNumber={}&dsid={dsid}",
        s.client_id, s.client_build_number, s.client_mastering_number
    );

    let mut out = Vec::new();
    let mut sync_token: Option<String> = None;
    loop {
        let mut zone = json!({
            "zoneID": { "zoneName": "Notes" },
            "desiredKeys": DESIRED_KEYS,
            "desiredRecordTypes": DESIRED_RECORD_TYPES,
            "reverse": true,
        });
        if let Some(t) = &sync_token {
            zone["syncToken"] = json!(t);
        }
        let resp = http
            .post(&url)
            .header("Cookie", &s.cookie)
            .header("Content-Type", "application/json")
            .header("Origin", "https://www.icloud.com")
            .header("Referer", "https://www.icloud.com/")
            .header("Accept", "application/json")
            .json(&json!({ "zones": [zone] }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("changes/zone failed: HTTP {}", resp.status()));
        }
        let body: Value = resp.json().await?;
        let z = &body["zones"][0];
        if let Some(records) = z["records"].as_array() {
            out.extend(records.iter().cloned());
        }
        sync_token = z["syncToken"].as_str().map(String::from).or(sync_token);
        if z["moreComing"] != json!(true) {
            break;
        }
    }
    if let Some(t) = &sync_token {
        println!("  syncToken: {}… ({} chars) — this is what sync_cursor would hold", &t[..t.len().min(24)], t.len());
    }
    Ok(out)
}

/// **Verify-first #4 — does the body repeat the title, and what ends that line?**
///
/// The M1 design assumes it does: in Apple's native format a note's first line
/// *is* its title, and `TitleEncrypted` is derived from it. icloud-md's own
/// source agrees ("title + body"). Neither is a measurement against THIS
/// account, and `strip_leading_title` is the highest-risk function in
/// the milestone — the slot that produced gotchas #11 and #17.
///
/// The separator is the half that actually matters and the half nobody has
/// checked. Gotcha #17 was not about *finding* the title; it was about what
/// ends the title's line, and a wrong answer there costs the first real line of
/// every note. On this backend the text is plain (no markup), so the separator
/// should simply be `\n` — but "should" is what gotcha #11 says three samples
/// will happily confirm. This prints the actual codepoint, so the answer is
/// read rather than assumed.
///
/// Note bodies are never printed. Titles are (they are already in the table
/// above); the body contributes only its first line's length and the boundary
/// character.
fn report_title_repetition(notes: &[&Value]) -> Result<()> {
    println!("\n── verify-first #4: does the body repeat the title? ──");

    let (mut exact, mut prefix, mut differs, mut undecodable, mut empty) = (0, 0, 0, 0, 0);
    // Kept so the mismatches can be diagnosed at the end rather than merely
    // counted. "DIFFERENT" without knowing HOW is not an answer — it is the
    // question restated, and the whole point of this run is to write a rule.
    let mut mismatches: Vec<(String, String)> = Vec::new();
    // Only REAL boundaries land here. A note that is nothing but its title has
    // no second line, so it observes no separator at all — counting that as
    // evidence is how a run with one title-only note reads as "answered".
    let mut separators: HashMap<String, usize> = HashMap::new();
    let mut single_line = 0usize;
    let mut containers: HashMap<&str, usize> = HashMap::new();
    // The SHIPPED rule, run over the same notes. The counters above measure
    // the account; this one measures `doc::strip_leading_title` against it, so
    // a re-run on any account is a regression check of the real function
    // rather than a second, drifting implementation of it living in a probe.
    let mut classified: HashMap<String, usize> = HashMap::new();
    let mut unexplained: Vec<(String, String)> = Vec::new();

    for (idx, n) in notes.iter().enumerate() {
        let quiet = idx >= row_limit();
        let title = decoded_text(&n["fields"]["TitleEncrypted"]).unwrap_or_default();
        let Some(b64) = n["fields"]["TextDataEncrypted"]["value"].as_str() else {
            empty += 1;
            continue;
        };
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64)?;

        // Direct evidence for gotcha #20 on THIS account: the container is
        // whichever client last wrote the note, not a property of the endpoint.
        *containers
            .entry(match bytes.get(..2) {
                Some([0x1f, 0x8b]) => "gzip",
                // NOT an equality test on `78 9c`: the second byte varies with
                // compression level, and rejecting the others rejects real
                // notes (gotcha #20).
                Some([0x78, _]) => "zlib",
                _ => "neither",
            })
            .or_default() += 1;

        let text = match doc::decode_note_text(&bytes) {
            Ok(t) => t,
            Err(e) => {
                if !quiet {
                    println!("  {:<40} UNDECODABLE — {e}", truncate(&shown(&title), 40));
                }
                undecodable += 1;
                continue;
            }
        };

        let stripped = doc::strip_leading_title(&text, &title);
        *classified.entry(format!("{:?}", stripped.matched)).or_default() += 1;
        if stripped.matched == doc::TitleMatch::Unexplained {
            unexplained.push((title.clone(), text.lines().find(|l| !l.is_empty()).unwrap_or("").to_string()));
        }

        let first_line: String = text.chars().take_while(|c| *c != '\n').collect();
        // The character immediately after the title, by name rather than by
        // eye — U+2028 and U+000D both look like "a line break" in output.
        // `None` means the note IS just its title: nothing follows, so there
        // is no separator to observe.
        let boundary = text.chars().nth(first_line.chars().count());
        let boundary_label = boundary
            .map(|c| format!("U+{:04X}", c as u32))
            .unwrap_or_else(|| "<none: title-only note>".into());

        let mut record_boundary = || match boundary {
            Some(c) => *separators.entry(format!("U+{:04X}", c as u32)).or_default() += 1,
            None => single_line += 1,
        };

        let verdict = if first_line == title {
            exact += 1;
            record_boundary();
            "EXACT"
        } else if !title.is_empty() && first_line.starts_with(&title) {
            prefix += 1;
            record_boundary();
            "PREFIX — line 1 is longer than the title"
        } else {
            differs += 1;
            mismatches.push((title.clone(), first_line.clone()));
            "DIFFERENT — the body does NOT open with the title"
        };

        if !quiet {
            println!(
                "  {:<40} line1={:>4} ch  after-line1={:<14} {verdict}",
                truncate(&shown(&title), 40),
                first_line.chars().count(),
                boundary_label
            );
        }
    }

    println!(
        "\n  exact={exact}  prefix={prefix}  differs={differs}  \
         undecodable={undecodable}  no-body={empty}  title-only={single_line}"
    );
    println!("  compression containers seen: {containers:?}   (gotcha #20 — expect BOTH eventually)");
    println!("  boundaries actually observed: {separators:?}");

    // The two questions are answered separately, because one run can settle
    // the first and say nothing about the second — which is exactly what a
    // near-empty account does.
    if differs > 0 {
        println!("\n  TITLE REPETITION: NO — {differs} note(s) do not open with their title.");
        println!("  The rule needs a case for them. Write that case; do NOT widen the cut.");
    } else if exact + prefix == 0 {
        println!("\n  TITLE REPETITION: UNCONFIRMED — no note decoded.");
    } else {
        println!("\n  TITLE REPETITION: YES on {} note(s).", exact + prefix);
    }

    match separators.len() {
        // The half gotcha #17 is actually about, and the half a title-only
        // note cannot answer: a note with no second line has no separator.
        0 => println!(
            "  SEPARATOR: UNCONFIRMED — every note here is title-only, so nothing\n  \
             followed the title to observe. This is the question that matters:\n  \
             gotcha #17 was never about FINDING the title, it was about what ends\n  \
             the title's line. Add a note with a title AND at least two more lines,\n  \
             then re-run."
        ),
        1 => println!(
            "  SEPARATOR: one codepoint, {:?} — strip_leading_title can cut on\n  \
             the title field and that boundary, and nothing else.",
            separators.keys().next().unwrap()
        ),
        _ => println!(
            "  SEPARATOR: MORE THAN ONE — {separators:?}. Decide how each is handled\n  \
             BEFORE writing the stripper. This is gotcha #17 in a new costume."
        ),
    }

    println!("\n  doc::strip_leading_title on these notes: {classified:?}");
    match unexplained.len() {
        0 => println!(
            "  CLASSIFICATION: every note's first line is explained by a measured cause.\n               The cut is correct on all of them; nothing here asks for a new rule."
        ),
        n => {
            println!(
                "  CLASSIFICATION: {n} note(s) are Unexplained. The line is still CUT — Apple's\n                   model is that the first line IS the title — but our account of how\n                   TitleEncrypted is derived has a gap. Diagnose before M2 writes anything."
            );
            report_mismatches(&unexplained);
        }
    }

    if !mismatches.is_empty() {
        report_mismatches(&mismatches);
    }

    if notes.len() < 3 {
        println!(
            "\n  ⚠ {} note(s) in this account. That is too few to conclude anything:\n  \
             gotcha #11 records three samples producing a tidy-but-wrong rule, and\n  \
             the case that falsified it was the one nobody had made yet.",
            notes.len()
        );
    }
    Ok(())
}

/// Shows exactly WHERE a title and its body's first line diverge.
///
/// **This prints note content**, as codepoints — and it has to. A mismatch
/// counted but not seen is the question restated, and the difference is
/// routinely invisible: a leading space, a zero-width character and a
/// look-alike Unicode form all render identically to the eye. `U+0020` does
/// not.
///
/// Bounded to the first 32 codepoints of each, which is enough to find a
/// divergence that matters and short of dumping a note.
fn report_mismatches(mismatches: &[(String, String)]) {
    // Bounded hard. On a real account a systematic difference shows up in the
    // first few; hundreds of dumps would be a disclosure with no extra
    // information in it.
    const MAX_SHOWN: usize = 8;
    println!(
        "\n  ── mismatch diagnosis ({} of {} shown; prints note content as codepoints) ──",
        mismatches.len().min(MAX_SHOWN),
        mismatches.len()
    );
    for (title, line1) in mismatches.iter().take(MAX_SHOWN) {
        let cps = |s: &str| -> String {
            if redacting() {
                // The full dump is note content. Under redaction the counts
                // and the divergence below still answer "how do they differ",
                // which is the whole reason to look.
                return "<withheld — JODD_PROBE_REDACT>".to_string();
            }
            s.chars()
                .take(32)
                .map(|c| format!("U+{:04X}", c as u32))
                .collect::<Vec<_>>()
                .join(" ")
        };
        println!("\n  title  ({:>3} cp): {}", title.chars().count(), cps(title));
        println!("  line 1 ({:>3} cp): {}", line1.chars().count(), cps(line1));

        let diverge = title
            .chars()
            .zip(line1.chars())
            .position(|(a, b)| a != b);
        match (line1.chars().count(), diverge) {
            // The body opens with an empty line, yet Apple still derived a
            // title. That means `TitleEncrypted` is NOT simply "line one" —
            // it is the first line with something in it, and a stripper that
            // cuts line one would cut a blank and leave the title in place.
            (0, _) => println!(
                "  → the body's first line is EMPTY while the title is not:\n  \
                   Apple derived the title from a LATER line. TitleEncrypted is the\n  \
                   first NON-EMPTY line, not literally the first."
            ),
            // Two codepoints, even under redaction: they ARE the finding
            // (U+2028 was discovered exactly here), and one character each is
            // a far smaller disclosure than the run above.
            (_, Some(i)) => println!(
                "  → first divergence at codepoint {i}: title has {:?}, body has {:?}",
                title.chars().nth(i),
                line1.chars().nth(i)
            ),
            (_, None) => println!(
                "  → one is a prefix of the other by codepoint, yet the equality and\n  \
                   starts_with checks both failed. Compare the lengths above."
            ),
        }
    }
}

/// **Verify-first #5 — does `ParentFolder` give real nesting?**
///
/// If it does, `folders.path` is a genuine `Notes/A/B` tree and every existing
/// subtree query works untouched — unlike Microsoft, where nesting is
/// unrecoverable (gotcha #12) and the sidebar is flat by necessity.
///
/// Also exercises the two hazards the tree builder has to close, against real
/// data rather than a fixture: a `/` inside a folder title would forge a path
/// segment, and a cyclic parent chain would spin forever.
fn report_folder_nesting(folders: &HashMap<String, Folder>) {
    println!("\n── verify-first #5: does ParentFolder give real nesting? ──");

    let mut depths = Vec::new();
    let mut slash_in_name = Vec::new();
    let mut cyclic = Vec::new();

    let mut ids: Vec<&String> = folders.keys().collect();
    ids.sort();
    for id in ids {
        match folder_path(id, folders) {
            Ok(path) => {
                let depth = path.matches('/').count() + 1;
                depths.push(depth);
                println!("  {:<30} depth {depth}  {}", truncate(id, 30), shown(&path));
            }
            Err(e) => {
                cyclic.push(id.clone());
                println!("  {:<30} {e}", truncate(id, 30));
            }
        }
        if folders[id].name.contains('/') {
            slash_in_name.push(shown(&folders[id].name));
        }
    }

    let max = depths.iter().copied().max().unwrap_or(0);
    println!("\n  deepest path: {max} segment(s)");
    if max <= 1 {
        println!(
            "  READ THIS AS: every folder is a root here, so this account proves\n  \
             nothing either way — nesting is UNCONFIRMED. Make a subfolder in\n  \
             Notes.app, file one note into it, and re-run."
        );
    } else {
        println!("  READ THIS AS: nesting is real. Build the tree (Component G).");
    }
    if !slash_in_name.is_empty() {
        println!("  ⚠ folder titles containing '/': {slash_in_name:?} — the tree builder must escape these");
    }
    if !cyclic.is_empty() {
        println!("  ⚠ cyclic or over-deep parent chains: {cyclic:?} — the depth cap is load-bearing");
    }
}

/// Walks `ParentFolder` to the root, joining titles with `/`.
///
/// Depth-capped rather than trusting the data: a cycle in a chain Jodd does not
/// own would hang the app, and "the server would never do that" is not a
/// property this code can check.
fn folder_path(id: &str, folders: &HashMap<String, Folder>) -> Result<String> {
    const MAX_DEPTH: usize = 32;
    let mut parts = Vec::new();
    let mut cursor = Some(id.to_string());
    while let Some(current) = cursor {
        if parts.len() >= MAX_DEPTH {
            return Err(anyhow!("parent chain deeper than {MAX_DEPTH} — cycle?"));
        }
        let Some(f) = folders.get(&current) else {
            parts.push(format!("<unknown:{current}>"));
            break;
        };
        parts.push(f.name.clone());
        cursor = f.parent.clone();
    }
    parts.reverse();
    Ok(parts.join("/"))
}

/// `*Encrypted` is a misnomer for these fields: base64 of plain UTF-8 bytes.
fn decoded_text(field: &Value) -> Option<String> {
    let b64 = field["value"].as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    String::from_utf8(bytes).ok()
}

fn record_name(r: &Value) -> &str {
    r["recordName"].as_str().unwrap_or("<no recordName>")
}

fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|| ms.to_string())
}

/// Character-aware so a Thai title isn't cut mid-codepoint.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
    t.push('…');
    t
}
