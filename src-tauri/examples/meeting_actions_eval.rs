//! No-network deterministic meeting corpus replay. Never constructs a provider.
mod meeting {
    pub use jodd_lib::llm::meeting::*;
}
#[path = "../src/llm/meeting_eval.rs"]
mod meeting_eval;
fn main() {
    let responses = std::env::args().nth(1).map(|path| {
        let text = std::fs::read_to_string(path).expect("read local responses JSON");
        serde_json::from_str(&text).expect("responses must be a JSON object keyed by case ID")
    });
    let report = if let Some(responses) = &responses {
        meeting_eval::run_with_responses(Some(responses))
    } else {
        meeting_eval::run()
    };
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if !report["failures"].as_array().unwrap().is_empty() {
        std::process::exit(1);
    }
}
