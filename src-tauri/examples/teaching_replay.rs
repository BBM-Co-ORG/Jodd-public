//! Synthetic instructor fixture generator. No app state, files, settings or provider.
use jodd_lib::{
    accounts::Account,
    llm::{budget, markdown, meeting, policy, receipts},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../tests/evals/meeting-actions-v1/cases.json"
    ))
    .unwrap();
    let store = Arc::new(Mutex::new(receipts::Store::open(None).unwrap()));
    let mut lessons = Vec::new();
    for id in ["th-01", "th-02", "th-10"] {
        let case = corpus["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap();
        let source = case["source"].as_str().unwrap();
        let output: Result<String, String> =
            receipts::run_in(store.clone(), id, None, "action_items", async {
                receipts::stage("validating_citations");
                receipts::check("synthetic_response_no_provider");
                meeting::request(source).map_err(|e| e.to_string())?;
                let parsed = meeting::parse(&case["synthetic_response"].to_string(), source)
                    .map_err(|e| e.to_string())?;
                let envelope = meeting::envelope(&parsed, source).map_err(|e| e.to_string())?;
                receipts::check("full_passage_match_not_semantic_proof");
                Ok(markdown::md_to_html(&envelope.lessons_markdown))
            })
            .await;
        lessons.push(json!({"id":id,"source":source,"body_html":output.unwrap(),"receipts":store.lock().unwrap().list(Some(id))}));
    }
    let account: Account = serde_json::from_value(json!({"id":"synthetic", "email":"synthetic@example.test", "added_at":"2026-09-20", "llm":{"provider":"disabled","data_allowed":false}})).unwrap();
    let denied: Result<(), String> =
        receipts::run_in(store.clone(), "policy", None, "action_items", async {
            receipts::check("permission_before_provider_construction");
            policy::build_account_provider(&[account], "synthetic", |_| {
                panic!("denied policy must never build a provider")
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .await;
    assert!(denied.is_err());
    let policy_error = denied.unwrap_err();
    let ledger = Arc::new(Mutex::new(budget::Ledger::memory(budget::Settings {
        workflow_units: 512,
        session_units: 512,
        output_tokens: 256,
        ..Default::default()
    })));
    let denied: Result<(), String> =
        receipts::run_in(store.clone(), "budget", None, "action_items", async {
            budget::run_in(ledger, "budget", None, async {
                budget::Attempt::start(100).map_err(|e| e.to_string())?;
                panic!("over-budget request must never dispatch");
            })
            .await
        })
        .await;
    assert!(denied.is_err());
    let policy_receipts = store.lock().unwrap().list(Some("policy"));
    let budget_receipts = store.lock().unwrap().list(Some("budget"));
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "mode":"recorded host execution on synthetic inputs; browser replays; no model called",
        "corpus_sha256":format!("{:x}",Sha256::digest(include_bytes!("../../tests/evals/meeting-actions-v1/cases.json"))),
        "meeting_contract_sha256":format!("{:x}",Sha256::digest(include_bytes!("../src/llm/meeting.rs"))),
        "policy_sha256":format!("{:x}",Sha256::digest(include_bytes!("../src/llm/policy.rs"))),
        "budget_sha256":format!("{:x}",Sha256::digest(include_bytes!("../src/llm/budget.rs"))),
            "lessons":lessons,
            "policy":{"error":policy_error,"receipts":policy_receipts},
            "budget":{"error":denied.unwrap_err(),"receipts":budget_receipts}
        }))
        .unwrap()
    );
}
