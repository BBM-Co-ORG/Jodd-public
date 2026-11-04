//! "What's on my TV": choosing among the linked TVs' answers, and turning the
//! chosen one into a URL and a "Watched on ..." line.
//!
//! Pure functions only: no I/O, no logging. The screen ID never appears here;
//! a [`PerTv`] carries only the TV's id and its label.

use crate::ingest::tv_lounge::{Sighting, TvError};
use serde::Serialize;

/// One TV's answer to "what is playing?".
pub struct PerTv {
    pub tv_id: String,
    pub label: String,
    pub result: Result<Sighting, TvError>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Candidate {
    pub tv_id: String,
    pub label: String,
    pub url: String,
    pub watched_label: Option<String>,
    pub playing: bool,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Found {
        tv_id: String,
        label: String,
        url: String,
        watched_label: Option<String>,
        via_default: bool,
    },
    Choose { candidates: Vec<Candidate> },
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum NowError {
    NoTvs,
    NothingPlaying,
    NeedsRepair { label: String },
    Network,
    Cancelled,
}

/// A reported time of under one second ("currentTime below 1 s") is no time.
fn usable(seconds: Option<u64>) -> Option<u64> {
    seconds.filter(|s| *s >= 1)
}

/// `https://youtu.be/<id>`, with `?t=<s>` when the TV reported a time of at
/// least one second (a `currentTime` below 1 s is "no time", ruling R9).
pub fn video_url(video_id: &str, seconds: Option<u64>) -> String {
    match usable(seconds) {
        Some(s) => format!("https://youtu.be/{video_id}?t={s}"),
        None => format!("https://youtu.be/{video_id}"),
    }
}

/// 72 -> "1:12", 3725 -> "1:02:05".
pub fn clock(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "Watched on TV up to 1:12" for the default TV, "Watched on Bedroom up to
/// 2:27" for a named one; `None` without a usable time.
pub fn watched_label(seconds: Option<u64>, tv_label: &str, via_default: bool) -> Option<String> {
    let s = usable(seconds)?;
    let name = if via_default { "TV" } else { tv_label };
    Some(format!("Watched on {name} up to {}", clock(s)))
}

fn is_playing(s: &Sighting) -> bool {
    s.state.as_deref() == Some("1")
}

fn found(tv: PerTv, s: &Sighting, via_default: bool) -> Outcome {
    Outcome::Found {
        url: video_url(&s.video_id, s.seconds),
        watched_label: watched_label(s.seconds, &tv.label, via_default),
        tv_id: tv.tv_id,
        label: tv.label,
        via_default,
    }
}

/// Picks one answer (or asks the user to) from the default TV's result and the
/// other linked TVs'. `pick(None, vec![one_tv])` asks a single explicit TV.
///
/// 1. The default TV's own answer, when it has a video, always wins (even
///    paused).
/// 2. Otherwise the other TVs that answered: the playing ones if any, else
///    all of them. One is `Found`, several are `Choose` (input order).
/// 3. Nothing found: `Cancelled`, then `NeedsRepair` (default checked first),
///    then `NothingPlaying`, then `Network`.
pub fn pick(default: Option<PerTv>, others: Vec<PerTv>) -> Result<Outcome, NowError> {
    if default.is_none() && others.is_empty() {
        return Err(NowError::NoTvs);
    }

    let mut default = default;
    if let Some(d) = default.take() {
        match &d.result {
            Ok(s) => {
                let s = s.clone();
                return Ok(found(d, &s, true));
            }
            Err(_) => default = Some(d),
        }
    }

    let answered: Vec<&PerTv> = others.iter().filter(|t| t.result.is_ok()).collect();
    let playing: Vec<&PerTv> = answered
        .iter()
        .copied()
        .filter(|t| t.result.as_ref().is_ok_and(is_playing))
        .collect();
    let group = if playing.is_empty() { answered } else { playing };

    if group.len() == 1 {
        let id = group[0].tv_id.clone();
        let tv = others.into_iter().find(|t| t.tv_id == id).expect("in group");
        let s = tv.result.as_ref().expect("ok").clone();
        return Ok(found(tv, &s, false));
    }
    if group.len() > 1 {
        let ids: Vec<&str> = group.iter().map(|t| t.tv_id.as_str()).collect();
        let candidates = others
            .iter()
            .filter(|t| ids.contains(&t.tv_id.as_str()))
            .filter_map(|t| {
                let s = t.result.as_ref().ok()?;
                Some(Candidate {
                    tv_id: t.tv_id.clone(),
                    label: t.label.clone(),
                    url: video_url(&s.video_id, s.seconds),
                    watched_label: watched_label(s.seconds, &t.label, false),
                    playing: is_playing(s),
                })
            })
            .collect();
        return Ok(Outcome::Choose { candidates });
    }

    let all: Vec<&PerTv> = default.iter().chain(others.iter()).collect();
    let has = |f: fn(&TvError) -> bool| all.iter().any(|t| t.result.as_ref().err().is_some_and(f));
    if has(|e| matches!(e, TvError::Cancelled)) {
        return Err(NowError::Cancelled);
    }
    if let Some(t) = all.iter().find(|t| matches!(t.result, Err(TvError::Unlinked))) {
        return Err(NowError::NeedsRepair { label: t.label.clone() });
    }
    if has(|e| matches!(e, TvError::NothingPlaying)) {
        return Err(NowError::NothingPlaying);
    }
    Err(NowError::Network)
}

/// The `tv_id`s whose result is `Err(Unlinked)`.
pub fn unlinked_ids(results: &[&PerTv]) -> Vec<String> {
    results
        .iter()
        .filter(|t| matches!(t.result, Err(TvError::Unlinked)))
        .map(|t| t.tv_id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sight(id: &str, secs: Option<u64>, state: Option<&str>) -> Sighting {
        Sighting {
            video_id: id.into(),
            seconds: secs,
            duration: None,
            state: state.map(str::to_string),
        }
    }
    fn tv(id: &str, label: &str, result: Result<Sighting, TvError>) -> PerTv {
        PerTv { tv_id: id.into(), label: label.into(), result }
    }
    fn ok(id: &str, label: &str, vid: &str, secs: Option<u64>, state: Option<&str>) -> PerTv {
        tv(id, label, Ok(sight(vid, secs, state)))
    }
    fn err(id: &str, label: &str, e: TvError) -> PerTv {
        tv(id, label, Err(e))
    }

    #[test]
    fn video_url_with_and_without_time() {
        assert_eq!(video_url("abc", None), "https://youtu.be/abc");
        assert_eq!(video_url("abc", Some(87)), "https://youtu.be/abc?t=87");
    }

    #[test]
    fn zero_seconds_counts_as_no_time() {
        assert_eq!(video_url("abc", Some(0)), "https://youtu.be/abc");
        assert_eq!(watched_label(Some(0), "Bedroom", false), None);
        assert_eq!(watched_label(Some(0), "TV", true), None);
        assert_eq!(clock(0), "0:00");
    }

    #[test]
    fn clock_formats() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(72), "1:12");
        assert_eq!(clock(3725), "1:02:05");
    }

    #[test]
    fn watched_label_names_non_default_tv() {
        assert_eq!(watched_label(Some(72), "Bedroom", true).as_deref(), Some("Watched on TV up to 1:12"));
        assert_eq!(watched_label(Some(147), "Bedroom", false).as_deref(), Some("Watched on Bedroom up to 2:27"));
        assert_eq!(watched_label(None, "Bedroom", false), None);
    }

    #[test]
    fn default_ok_wins_whatever_the_others_hold() {
        let out = pick(
            Some(ok("d", "Lounge", "dvid", Some(72), Some("2"))),
            vec![ok("o", "Bedroom", "ovid", Some(5), Some("1"))],
        )
        .unwrap();
        assert_eq!(
            out,
            Outcome::Found {
                tv_id: "d".into(),
                label: "Lounge".into(),
                url: "https://youtu.be/dvid?t=72".into(),
                watched_label: Some("Watched on TV up to 1:12".into()),
                via_default: true,
            }
        );
    }

    #[test]
    fn default_nothing_playing_and_one_other_ok() {
        let out = pick(
            Some(err("d", "Lounge", TvError::NothingPlaying)),
            vec![ok("o", "Bedroom", "ovid", Some(147), Some("1"))],
        )
        .unwrap();
        assert_eq!(
            out,
            Outcome::Found {
                tv_id: "o".into(),
                label: "Bedroom".into(),
                url: "https://youtu.be/ovid?t=147".into(),
                watched_label: Some("Watched on Bedroom up to 2:27".into()),
                via_default: false,
            }
        );
    }

    #[test]
    fn playing_beats_paused_among_others() {
        let out = pick(
            Some(err("d", "Lounge", TvError::NothingPlaying)),
            vec![
                ok("a", "Kitchen", "avid", None, Some("2")),
                ok("b", "Bedroom", "bvid", None, Some("1")),
            ],
        )
        .unwrap();
        match out {
            Outcome::Found { tv_id, via_default, url, .. } => {
                assert_eq!(tv_id, "b");
                assert!(!via_default);
                assert_eq!(url, "https://youtu.be/bvid");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn two_playing_means_choose_in_input_order() {
        let out = pick(
            Some(err("d", "Lounge", TvError::NothingPlaying)),
            vec![
                ok("a", "Kitchen", "avid", Some(10), Some("1")),
                err("x", "Den", TvError::NothingPlaying),
                ok("b", "Bedroom", "bvid", None, Some("1")),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            Outcome::Choose {
                candidates: vec![
                    Candidate {
                        tv_id: "a".into(),
                        label: "Kitchen".into(),
                        url: "https://youtu.be/avid?t=10".into(),
                        watched_label: Some("Watched on Kitchen up to 0:10".into()),
                        playing: true,
                    },
                    Candidate {
                        tv_id: "b".into(),
                        label: "Bedroom".into(),
                        url: "https://youtu.be/bvid".into(),
                        watched_label: None,
                        playing: true,
                    },
                ]
            }
        );
    }

    #[test]
    fn two_paused_tvs_choose_with_both_not_playing() {
        let out = pick(
            None,
            vec![
                ok("a", "Kitchen", "avid", None, Some("2")),
                ok("b", "Bedroom", "bvid", None, None),
            ],
        )
        .unwrap();
        match out {
            Outcome::Choose { candidates } => {
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[0].tv_id, "a");
                assert_eq!(candidates[1].tv_id, "b");
                assert!(!candidates[0].playing && !candidates[1].playing);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn one_paused_other_is_found_not_choose() {
        let out = pick(None, vec![ok("a", "Kitchen", "avid", None, Some("2"))]).unwrap();
        assert!(matches!(out, Outcome::Found { via_default: false, .. }));
    }

    #[test]
    fn all_nothing_playing() {
        let out = pick(
            Some(err("d", "Lounge", TvError::NothingPlaying)),
            vec![err("o", "Bedroom", TvError::NothingPlaying)],
        );
        assert_eq!(out, Err(NowError::NothingPlaying));
    }

    #[test]
    fn default_unlinked_alone_needs_repair() {
        let out = pick(Some(err("d", "Lounge", TvError::Unlinked)), vec![]);
        assert_eq!(out, Err(NowError::NeedsRepair { label: "Lounge".into() }));
    }

    #[test]
    fn default_unlinked_other_ok_is_found_for_other() {
        let out = pick(
            Some(err("d", "Lounge", TvError::Unlinked)),
            vec![ok("o", "Bedroom", "ovid", None, Some("1"))],
        )
        .unwrap();
        assert!(matches!(out, Outcome::Found { ref tv_id, via_default: false, .. } if tv_id == "o"));
    }

    #[test]
    fn all_network_is_network() {
        let out = pick(
            Some(err("d", "Lounge", TvError::Network("x".into()))),
            vec![err("o", "Bedroom", TvError::Network("y".into()))],
        );
        assert_eq!(out, Err(NowError::Network));
    }

    #[test]
    fn nothing_at_all_is_no_tvs() {
        assert_eq!(pick(None, vec![]), Err(NowError::NoTvs));
    }

    #[test]
    fn cancelled_with_nothing_found() {
        let out = pick(
            Some(err("d", "Lounge", TvError::NothingPlaying)),
            vec![err("o", "Bedroom", TvError::Cancelled)],
        );
        assert_eq!(out, Err(NowError::Cancelled));
    }

    #[test]
    fn single_explicit_tv_r3() {
        let found = pick(None, vec![ok("o", "Bedroom", "ovid", Some(147), Some("1"))]).unwrap();
        assert_eq!(
            found,
            Outcome::Found {
                tv_id: "o".into(),
                label: "Bedroom".into(),
                url: "https://youtu.be/ovid?t=147".into(),
                watched_label: Some("Watched on Bedroom up to 2:27".into()),
                via_default: false,
            }
        );
        let unlinked = pick(None, vec![err("o", "Bedroom", TvError::Unlinked)]);
        assert_eq!(unlinked, Err(NowError::NeedsRepair { label: "Bedroom".into() }));
    }

    #[test]
    fn mixed_errors_nothing_playing_beats_network_r7() {
        let out = pick(
            Some(err("d", "Lounge", TvError::Network("x".into()))),
            vec![err("o", "Bedroom", TvError::NothingPlaying)],
        );
        assert_eq!(out, Err(NowError::NothingPlaying));
    }

    #[test]
    fn mixed_errors_unlinked_beats_nothing_playing_r7() {
        let out = pick(
            Some(err("d", "Lounge", TvError::NothingPlaying)),
            vec![err("o", "Bedroom", TvError::Unlinked)],
        );
        assert_eq!(out, Err(NowError::NeedsRepair { label: "Bedroom".into() }));
    }

    #[test]
    fn mixed_errors_cancelled_beats_unlinked_r7() {
        let out = pick(
            Some(err("d", "Lounge", TvError::Unlinked)),
            vec![err("o", "Bedroom", TvError::Cancelled)],
        );
        assert_eq!(out, Err(NowError::Cancelled));
    }

    #[test]
    fn needs_repair_checks_default_first_r7() {
        let out = pick(
            Some(err("d", "Lounge", TvError::Unlinked)),
            vec![err("o", "Bedroom", TvError::Unlinked)],
        );
        assert_eq!(out, Err(NowError::NeedsRepair { label: "Lounge".into() }));
    }

    #[test]
    fn default_paused_ok_is_still_found_via_default() {
        let out = pick(
            Some(ok("d", "Lounge", "dvid", None, Some("2"))),
            vec![ok("o", "Bedroom", "ovid", None, Some("1"))],
        )
        .unwrap();
        assert!(matches!(out, Outcome::Found { ref tv_id, via_default: true, .. } if tv_id == "d"));
    }

    #[test]
    fn unlinked_ids_lists_only_unlinked() {
        let a = err("a", "A", TvError::Unlinked);
        let b = ok("b", "B", "v", None, None);
        let c = err("c", "C", TvError::NothingPlaying);
        let d = err("d", "D", TvError::Unlinked);
        assert_eq!(unlinked_ids(&[&a, &b, &c, &d]), vec!["a".to_string(), "d".to_string()]);
    }

    #[test]
    fn json_shapes_the_frontend_relies_on() {
        use serde_json::json;
        let found = pick(None, vec![ok("o", "Bedroom", "ovid", Some(147), Some("1"))]).unwrap();
        assert_eq!(
            serde_json::to_value(found).unwrap(),
            json!({
                "kind": "found",
                "tv_id": "o",
                "label": "Bedroom",
                "url": "https://youtu.be/ovid?t=147",
                "watched_label": "Watched on Bedroom up to 2:27",
                "via_default": false,
            })
        );
        let choose = pick(
            None,
            vec![ok("a", "Kitchen", "avid", None, Some("1")), ok("b", "Bedroom", "bvid", Some(10), Some("1"))],
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(choose).unwrap(),
            json!({
                "kind": "choose",
                "candidates": [
                    {"tv_id": "a", "label": "Kitchen", "url": "https://youtu.be/avid", "watched_label": null, "playing": true},
                    {"tv_id": "b", "label": "Bedroom", "url": "https://youtu.be/bvid?t=10", "watched_label": "Watched on Bedroom up to 0:10", "playing": true},
                ]
            })
        );
        assert_eq!(
            serde_json::to_value(NowError::NeedsRepair { label: "Lounge".into() }).unwrap(),
            json!({"code": "needs_repair", "label": "Lounge"})
        );
        assert_eq!(serde_json::to_value(NowError::NoTvs).unwrap(), json!({"code": "no_tvs"}));
        assert_eq!(serde_json::to_value(NowError::NothingPlaying).unwrap(), json!({"code": "nothing_playing"}));
        assert_eq!(serde_json::to_value(NowError::Network).unwrap(), json!({"code": "network"}));
        assert_eq!(serde_json::to_value(NowError::Cancelled).unwrap(), json!({"code": "cancelled"}));
    }
}
