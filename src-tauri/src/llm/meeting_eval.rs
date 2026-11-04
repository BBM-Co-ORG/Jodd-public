//! Offline replay only. Compiled by the test suite and developer example.
use super::meeting;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Instant;

#[derive(Deserialize)]
struct Corpus {
    version: String,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    split: String,
    language: String,
    category: String,
    critical: bool,
    source: String,
    synthetic_response: Value,
    reference: meeting::Meeting,
    expect_rejection: bool,
}

pub fn run() -> Value {
    run_with_responses(None)
}

pub fn run_with_responses(responses: Option<&Value>) -> Value {
    let corpus: Corpus = serde_json::from_str(include_str!(
        "../../../tests/evals/meeting-actions-v1/cases.json"
    ))
    .unwrap();
    let mut rows = vec![];
    let mut times = vec![];
    for c in corpus.cases {
        let started = Instant::now();
        let response = responses
            .and_then(|r| r.get(&c.id))
            .unwrap_or(if responses.is_some() {
                &Value::Null
            } else {
                &c.synthetic_response
            });
        let raw = response
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| response.to_string());
        let request = meeting::request(&c.source);
        let required: std::collections::BTreeSet<_> =
            c.reference.items.iter().map(|i| i.passage).collect();
        let payload: Value = request
            .as_ref()
            .ok()
            .and_then(|r| serde_json::from_str(r).ok())
            .unwrap_or(Value::Null);
        let source_parts: Vec<_> = c
            .source
            .lines()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        let retrieved = required
            .iter()
            .filter(|&&id| {
                payload["passages"].as_array().is_some_and(|ps| {
                    ps.iter().any(|p| {
                        p["id"] == id && p["text"].as_str() == source_parts.get(id - 1).copied()
                    })
                })
            })
            .count();
        let result = request.and_then(|_| meeting::parse(&raw, &c.source));
        let rejected = result.is_err();
        let reference_rows = c.reference.items.len();
        let matched = result
            .as_ref()
            .map(|m| {
                let mut remaining = c.reference.items.clone();
                m.items
                    .iter()
                    .filter(|item| {
                        if let Some(index) = remaining.iter().position(|r| r == *item) {
                            remaining.remove(index);
                            true
                        } else {
                            false
                        }
                    })
                    .count()
            })
            .unwrap_or(0);
        let accepted_rows = result.as_ref().map(|m| m.items.len()).unwrap_or(0);
        let semantic_match = result
            .as_ref()
            .map(|m| {
                m.incomplete == c.reference.incomplete
                    && matched == reference_rows
                    && accepted_rows == reference_rows
            })
            .unwrap_or(false);
        let passage_count = meeting::passages(&c.source).map(|p| p.len()).unwrap_or(0);
        let rendering_ok = result
            .as_ref()
            .map(|m| meeting::envelope(m, &c.source).is_ok())
            .unwrap_or(false);
        let pass = (responses.is_none() || responses.is_some_and(|r| r.get(&c.id).is_some()))
            && if c.expect_rejection {
                rejected
            } else {
                semantic_match && rendering_ok
            };
        let elapsed = started.elapsed().as_secs_f64() * 1000.;
        times.push(elapsed);
        rows.push(json!({"id":c.id,"split":c.split,"language":c.language,"category":c.category,"critical":c.critical,
            "pass":pass,"expected_rejection":c.expect_rejection,"rejected":rejected,
            "reference_rows":reference_rows,"accepted_rows":accepted_rows,"matching_reference_rows":matched,
            "admitted_passages":passage_count,"required_passages":required.len(),"retrieved_required_passages":retrieved,"reference_abstention":c.reference.items.iter().all(|i|i.kind!=meeting::Kind::Action && i.kind!=meeting::Kind::Decision),
            "latency_ms":elapsed}));
    }
    times.sort_by(f64::total_cmp);
    let failures: Vec<_> = rows
        .iter()
        .filter(|r| r["pass"] != true)
        .map(|r| r["id"].clone())
        .collect();
    let mut groups = vec![];
    for split in ["development", "held_out"] {
        for language in ["en", "th", "mixed"] {
            let group: Vec<_> = rows
                .iter()
                .filter(|r| r["split"] == split && r["language"] == language)
                .collect();
            let sum = |key: &str| {
                group
                    .iter()
                    .filter(|r| r["expected_rejection"] == false)
                    .map(|r| r[key].as_u64().unwrap_or(0))
                    .sum::<u64>()
            };
            let eligible: Vec<_> = group
                .iter()
                .filter(|r| r["expected_rejection"] == false)
                .collect();
            let abstentions: Vec<_> = eligible
                .iter()
                .filter(|r| r["reference_abstention"] == true)
                .collect();
            groups.push(json!({"split":split,"language":language,"cases":group.len(),"failures":group.iter().filter(|r|r["pass"]!=true).count(),
            "reference_grounding":{"matched":sum("matching_reference_rows"),"emitted":sum("accepted_rows"),"required":sum("reference_rows")},
            "input_passage_recall":{"retrieved":sum("retrieved_required_passages"),"required":sum("required_passages"),"scope":"one complete explicitly supplied source; no search"},
            "abstention":{"correct":abstentions.iter().filter(|r|r["pass"]==true).count(),"cases":abstentions.len()}}));
        }
    }
    json!({"corpus":corpus.version,"mode":if responses.is_some() {"supplied responses scored offline against synthetic reference labels; no live provider"} else {"synthetic offline validator replay; NOT provider quality"}, "sample_size":rows.len(),
        "failures":failures,"groups":groups,"cases":rows,"harness_latency_ms":{"median":times[times.len()/2],"p95":times[((times.len() as f64*0.95).ceil() as usize)-1]},
        "provider_latency":null,"human_acceptance":{"n":0,"rate":null},"human_edit_effort":null,
        "total_cost_per_human_accepted_result":null,"usage":"unknown/not measured; no provider called"})
}
#[cfg(test)]
mod tests {
    #[test]
    fn reference_labels_detect_wrong_semantics_even_with_real_quote() {
        let corpus: super::Corpus = serde_json::from_str(include_str!(
            "../../../tests/evals/meeting-actions-v1/cases.json"
        ))
        .unwrap();
        let mut responses = serde_json::Map::new();
        for case in corpus.cases {
            responses.insert(case.id, case.synthetic_response);
        }
        responses.get_mut("en-06").unwrap()["items"][0]["kind"] = "action".into();
        let report = super::run_with_responses(Some(&serde_json::Value::Object(responses)));
        assert_eq!(report["failures"], serde_json::json!(["en-06"]));
    }
    #[test]
    fn scoring_ignores_order_but_never_double_counts_duplicate_claims() {
        let corpus: super::Corpus = serde_json::from_str(include_str!(
            "../../../tests/evals/meeting-actions-v1/cases.json"
        ))
        .unwrap();
        let mut responses = serde_json::Map::new();
        for case in corpus.cases {
            responses.insert(case.id, case.synthetic_response);
        }
        responses.get_mut("en-01").unwrap()["items"]
            .as_array_mut()
            .unwrap()
            .reverse();
        let value = serde_json::Value::Object(responses.clone());
        assert_eq!(
            super::run_with_responses(Some(&value))["failures"],
            serde_json::json!([])
        );
        let rows = responses.get_mut("en-01").unwrap()["items"]
            .as_array_mut()
            .unwrap();
        rows.push(rows[0].clone());
        let report = super::run_with_responses(Some(&serde_json::Value::Object(responses)));
        let case = report["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "en-01")
            .unwrap();
        assert_eq!(case["matching_reference_rows"], 2);
        assert_eq!(case["accepted_rows"], 3);
        assert_eq!(report["failures"], serde_json::json!(["en-01"]));
    }
    #[test]
    fn synthetic_corpus_matches_reference_and_rejections() {
        let report = super::run();
        assert_eq!(report["sample_size"], 36);
        assert_eq!(report["failures"], serde_json::json!([]), "{report}");
    }
}
