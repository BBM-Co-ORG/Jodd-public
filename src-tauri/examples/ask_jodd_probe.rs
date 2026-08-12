//! Measure Ask Jodd's retrieval against a REAL vault, with no LLM call.
//!
//! Run:  cargo run --example ask_jodd_probe
//!       cargo run --example ask_jodd_probe -- "your question here"
//!
//! Why this exists: every automated test for this feature runs against
//! synthetic fixtures — a few dozen notes of a few dozen bytes. The design was
//! justified by measurements of a 6,655-note account holding 18 MB of body
//! HTML with a 1 MB outlier note (spec §2), and no fixture reproduces that
//! shape. This harness runs the real stage 1 (SQL pre-filter) and stage 2
//! (catalog) against the live SQLite cache and prints what they actually
//! produce, so the constants can be checked against reality instead of
//! against an estimate.
//!
//! It calls NO LLM and needs no provider: it invokes `ask::pool` and
//! `ask::catalog` directly, which is everything upstream of the model. So it
//! costs no tokens and no subscription quota, reads no credentials, touches
//! no config, and only reads the database. Answer QUALITY still needs a real
//! provider and a human — that is not what this measures.
//!
//! Deliberately does not reach into `jodd_lib::llm`, which is a private
//! module: a diagnostic is not a reason to widen production visibility.
//!
//! Lives in examples/ rather than src/bin/ — see CLAUDE.md edge #5: Tauri's
//! bundler copies every [[bin]] into Jodd.app/Contents/MacOS/, and macOS
//! Tahoe refuses to launch an ad-hoc bundle with more than one binary there.

use jodd_lib::ask::{catalog, pool, AskScope, CANDIDATE_POOL_MAX};
use jodd_lib::db::Db;

/// Crude on purpose: English runs ~4 chars/token, Thai far worse. The point is
/// the order of magnitude against the spec's ~12k-token catalog budget.
fn approx_tokens(chars: usize) -> usize {
    chars / 4
}

fn main() -> anyhow::Result<()> {
    let data_dir = dirs::data_dir()
        .map(|d| d.join("jodd"))
        .expect("no data dir on this OS");
    println!("vault: {}\n", data_dir.display());
    let db = Db::open(&data_dir)?;

    let questions: Vec<String> = {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.is_empty() {
            vec![
                // English, wording that deliberately does not match any title.
                "what did I decide about sync conflicts?".to_string(),
                // Thai — the case the FTS source is structurally weakest on.
                "ผมสรุปอะไรไว้เกี่ยวกับ ATLAS บ้าง".to_string(),
                // A term that should hit hard.
                "ATLAS".to_string(),
            ]
        } else {
            args
        }
    };

    // Read the account list with a second connection rather than adding a
    // production Db method that only a probe would ever call.
    let accounts: Vec<String> = {
        let conn = rusqlite::Connection::open(data_dir.join("jodd.sqlite3"))?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT account_id FROM notes \
             WHERE sync_state != 'deleted_pending' ORDER BY account_id",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    println!("accounts: {accounts:?}");
    println!("CANDIDATE_POOL_MAX = {CANDIDATE_POOL_MAX}\n");

    let mut scopes: Vec<(String, AskScope)> = vec![("all accounts".into(), AskScope::AllAccounts)];
    for a in &accounts {
        scopes.push((a.clone(), AskScope::Account { account_id: a.clone() }));
    }

    for question in &questions {
        println!("════ {question:?}");
        for (label, scope) in &scopes {
            // No exclusions: this probe runs against a real vault to measure
            // retrieval, and every account it enumerates is one the operator
            // asked about. Deactivated accounts are hidden in the app (see
            // AccountStatus), but hiding them here would silently change the
            // numbers this tool exists to report.
            let no_excluded: [String; 0] = [];
            let in_scope =
                db.count_notes_in_scope(scope.account_id(), scope.label(), &no_excluded)?;
            let candidates = pool::build_candidate_pool(&db, scope, question, &no_excluded)?;
            let fts_hits = candidates.iter().filter(|c| c.from_fts).count();
            let (catalog_text, index) = catalog::build_catalog(&candidates);

            println!(
                "  {label:<32} in_scope={in_scope:<6} considered={:<5} fts={fts_hits:<4} \
                 catalog={} chars (~{} tok){}",
                candidates.len(),
                catalog_text.len(),
                approx_tokens(catalog_text.len()),
                if candidates.len() != index.len() {
                    format!("  [{} uuid8 collision(s) dropped]", candidates.len() - index.len())
                } else {
                    String::new()
                }
            );
        }
        println!();
    }

    Ok(())
}
