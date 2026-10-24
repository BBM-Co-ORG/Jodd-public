//! A live URL ingest through the SHIPPED `ingest::run::ingest_to_note`, with
//! the account's real LLM provider and the real AI limits — but into a
//! throwaway, unencrypted SQLite in a temp dir, so nothing reaches the
//! user's `jodd.sqlite3` and nothing syncs. No receipts are written.
//!
//! ```text
//! cargo run --example ingest_live_probe -- <account_id> <key_points|summarize|transcript> "<url>" [out.html]
//! ```
//!
//! Prints every progress event with elapsed time, the call counts, the
//! title, and writes the note body to `out.html` (default: next to the DB).

use jodd_lib::accounts::load_accounts;
use jodd_lib::db::Db;
use jodd_lib::ingest::run::{ingest_to_note, map_input_cap, IngestDestination, IngestInput, IngestMode, IngestRequest};
use jodd_lib::llm::{budget, policy, provider::WorkflowKind, resolve, transcript::CleanLimits};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [account_id, mode, url, rest @ ..] = args.as_slice() else {
        eprintln!("usage: ingest_live_probe <account_id> <key_points|summarize|transcript> <url> [out.html]");
        std::process::exit(2);
    };
    let mode = match mode.as_str() {
        "key_points" => IngestMode::KeyPoints,
        "summarize" => IngestMode::Workflow { workflow: WorkflowKind::Summarize },
        "transcript" => IngestMode::Transcript { clean: false },
        other => panic!("unknown mode {other}"),
    };
    let accounts = load_accounts();
    let account = accounts.iter().find(|a| &a.id == account_id).expect("account not found").clone();
    let provider = policy::build_account_provider(&accounts, account_id, resolve::resolve_provider_for_account).expect("provider");
    let limits = budget::get_ai_limits().expect("AI limits");
    println!("limits: max_attempts {} · output_tokens {} · workflow_units {}", limits.max_attempts, limits.output_tokens, limits.workflow_units);

    let dir = tempfile::tempdir().expect("tempdir").keep();
    let db = Db::open_unencrypted(&dir).expect("temp db");
    let req = IngestRequest {
        account_id: account_id.clone(),
        backend_kind: account.backend_kind,
        can_create_folders: true,
        context: String::new(),
        title_override: None,
        destination: IngestDestination::Resolve,
        map_cap: map_input_cap(resolve::prompt_delivery_for_account(&account), std::env::consts::OS),
        mode,
        clean_limits: CleanLimits::from_settings(&limits),
        actor: jodd_lib::provenance::Actor::workflow("ingest", &account),
    };
    println!("map_cap: {} chars · temp db: {}", req.map_cap, dir.display());

    let started = std::time::Instant::now();
    let progress = move |p: jodd_lib::ingest::run::IngestProgress| {
        println!("[{:>6.1}s] {:?} {} of {} {}", started.elapsed().as_secs_f64(), p.stage, p.index, p.total, p.url_host.unwrap_or_default());
    };
    let result = budget::run("ingest_live_probe", None, async {
        ingest_to_note(&db, provider.as_ref(), &jodd_lib::ingest::HttpFetcher::default(), IngestInput::Urls(vec![url.clone()]), &req, CancellationToken::new(), &progress)
            .await
            .map_err(|e| e.to_string())
    })
    .await;
    let note = match result {
        Ok(n) => n,
        Err(e) => {
            println!("ingest failed after {:.1}s: {e}", started.elapsed().as_secs_f64());
            std::process::exit(1);
        }
    };
    let row = db.get(&note.uuid, account_id).unwrap().unwrap();
    let out = rest.first().map(std::path::PathBuf::from).unwrap_or_else(|| dir.join("note.html"));
    std::fs::write(&out, format!("<meta charset=utf-8><h1>{}</h1>\n{}", row.title, row.body_html)).unwrap();
    println!("done in {:.1}s", started.elapsed().as_secs_f64());
    println!("title: {}", row.title);
    println!("body: {} chars → {}", row.body_html.chars().count(), out.display());
}
