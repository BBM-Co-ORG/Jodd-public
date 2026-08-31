//! Stage 2 — the candidate catalog (spec §5.2) and stage 3's response parser
//! (spec §5.3).

use std::collections::HashMap;

use crate::ask::Candidate;

/// `uuid8 → (account_id, uuid)`. uuid8 alone is the model's handle, so this
/// index is what turns a cited id back into a real note.
pub type CatalogIndex = HashMap<String, (String, String)>;

/// One line per candidate, plus the index the model's reply is resolved
/// against. Bodies are never included — that is the whole point of the
/// catalog.
///
/// A candidate whose uuid8 collides with one already in the index is OMITTED
/// rather than aliased: 8 hex chars is only 4 billion values and the pool
/// spans accounts, so a collision is possible, and silently pointing two
/// catalog lines at one note would make a citation resolve to the wrong note.
pub fn build_catalog(candidates: &[Candidate]) -> (String, CatalogIndex) {
    let mut index: CatalogIndex = HashMap::new();
    let mut text = String::with_capacity(candidates.len() * 96);

    for c in candidates {
        if index.contains_key(&c.uuid8) {
            continue;
        }
        index.insert(c.uuid8.clone(), (c.account_id.clone(), c.uuid.clone()));

        let tags = if c.tags.is_empty() {
            String::new()
        } else {
            format!(
                " · {}",
                c.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ")
            )
        };
        // Titles are user data and can contain newlines, which would forge a
        // second catalog line. Collapse all whitespace to single spaces.
        let title = collapse_ws(&c.title);
        let label = collapse_ws(&c.label);
        text.push_str(&format!(
            "{} · {} · {}{} · {}\n",
            c.uuid8,
            title,
            label,
            tags,
            ymd(c.date_ms)
        ));
    }
    (text, index)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Epoch ms → YYYY-MM-DD, via a plain civil-date computation (days since the
/// Unix epoch). No chrono dependency; the codebase has none.
fn ymd(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Pull note ids out of the model's reply.
///
/// Deliberately lenient rather than JSON-schema'd: providers differ wildly in
/// how they wrap output (prose, bullets, code fences, event arrays), and a
/// strict contract would fail on formatting rather than on substance. Scanning
/// for runs of exactly 8 hex characters and intersecting with the catalog also
/// doubles as the hallucination guard — an id that was never offered cannot
/// be selected.
///
/// Returns `(account_id, uuid)` pairs, de-duplicated, first-seen order.
pub fn parse_selected_uuid8s(response: &str, known: &CatalogIndex) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let chars: Vec<char> = response.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_ascii_hexdigit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_hexdigit() {
            i += 1;
        }
        // Exactly 8: a longer run is some other identifier, not one of ours,
        // and slicing it would invent an id the model never wrote.
        if i - start != 8 {
            continue;
        }
        let token: String = chars[start..i].iter().collect::<String>().to_lowercase();
        if let Some((account_id, uuid)) = known.get(&token) {
            if seen.insert(token) {
                out.push((account_id.clone(), uuid.clone()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask::Candidate;

    fn cand(uuid: &str, uuid8: &str, title: &str, label: &str) -> Candidate {
        Candidate {
            uuid: uuid.into(),
            account_id: "acct@x".into(),
            uuid8: uuid8.into(),
            title: title.into(),
            label: label.into(),
            tags: vec!["debugging".into()],
            date_ms: 1_700_000_000_000,
            from_fts: true,
        }
    }

    #[test]
    fn catalog_has_one_line_per_candidate_with_its_id() {
        let c = vec![
            cand("uuid-1", "aaaaaaaa", "Sync conflicts", "Notes/A"),
            cand("uuid-2", "bbbbbbbb", "Tahoe bundling", "Notes/B"),
        ];
        let (text, index) = build_catalog(&c);
        assert_eq!(text.lines().filter(|l| l.contains('·')).count(), 2);
        assert!(text.contains("aaaaaaaa"));
        assert!(text.contains("Sync conflicts"));
        assert!(text.contains("Notes/A"));
        assert!(text.contains("#debugging"));
        assert_eq!(index.get("aaaaaaaa"), Some(&("acct@x".to_string(), "uuid-1".to_string())));
    }

    #[test]
    fn catalog_line_renders_the_correct_date_for_date_ms() {
        // cand()'s date_ms is 1_700_000_000_000 — independently confirmed to
        // be 2023-11-14 (worked by hand via civil_from_days on
        // z = 19675 + 719468 = 739143, see the ymd_tests module below for
        // the same method applied to other fixed points). This is the only
        // place any test checks that the rendered catalog line's date
        // actually reflects date_ms end to end (build_catalog -> ymd ->
        // formatted line) rather than just the line's shape.
        let c = vec![cand("uuid-1", "aaaaaaaa", "Title", "Notes")];
        let (text, _) = build_catalog(&c);
        assert!(
            text.contains("2023-11-14"),
            "expected ymd(1_700_000_000_000) = 2023-11-14 in catalog line:\n{text}"
        );
    }

    #[test]
    fn duplicate_uuid8_keeps_the_first_and_drops_the_second() {
        // uuid8 is only 8 hex chars, so a collision across accounts is
        // possible. The index must stay unambiguous.
        let mut a = cand("uuid-1", "aaaaaaaa", "First", "Notes");
        let mut b = cand("uuid-2", "aaaaaaaa", "Second", "Notes");
        a.account_id = "a1".into();
        b.account_id = "a2".into();
        let (text, index) = build_catalog(&[a, b]);
        assert_eq!(index.len(), 1);
        assert_eq!(index.get("aaaaaaaa").unwrap().1, "uuid-1");
        assert!(!text.contains("Second"), "the colliding entry is omitted, not silently aliased");
    }

    #[test]
    fn newlines_in_a_title_cannot_forge_a_catalog_line() {
        let c = vec![cand("uuid-1", "aaaaaaaa", "Real\nffffffff · Fake title", "Notes")];
        let (text, _) = build_catalog(&c);
        assert_eq!(text.lines().filter(|l| l.contains('·')).count(), 1);
    }

    #[test]
    fn parses_ids_out_of_prose_bullets_and_code_fences() {
        let mut known = std::collections::HashMap::new();
        known.insert("aaaaaaaa".to_string(), ("acct@x".to_string(), "uuid-1".to_string()));
        known.insert("bbbbbbbb".to_string(), ("acct@x".to_string(), "uuid-2".to_string()));

        for resp in [
            "I'd open aaaaaaaa and bbbbbbbb.",
            "- aaaaaaaa\n- bbbbbbbb\n",
            "```\naaaaaaaa\nbbbbbbbb\n```",
            "[\"aaaaaaaa\", \"bbbbbbbb\"]",
        ] {
            let got = parse_selected_uuid8s(resp, &known);
            assert_eq!(got.len(), 2, "failed on {resp:?}");
        }
    }

    #[test]
    fn unknown_ids_are_discarded() {
        let mut known = std::collections::HashMap::new();
        known.insert("aaaaaaaa".to_string(), ("acct@x".to_string(), "uuid-1".to_string()));
        // deadbeef is well-formed but was never in the catalog.
        let got = parse_selected_uuid8s("aaaaaaaa deadbeef", &known);
        assert_eq!(got, vec![("acct@x".to_string(), "uuid-1".to_string())]);
    }

    #[test]
    fn repeated_ids_are_returned_once_in_first_seen_order() {
        let mut known = std::collections::HashMap::new();
        known.insert("aaaaaaaa".to_string(), ("acct@x".to_string(), "uuid-1".to_string()));
        known.insert("bbbbbbbb".to_string(), ("acct@x".to_string(), "uuid-2".to_string()));
        let got = parse_selected_uuid8s("bbbbbbbb aaaaaaaa bbbbbbbb", &known);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1, "uuid-2", "first-seen order");
    }

    #[test]
    fn longer_hex_runs_do_not_yield_a_spurious_match() {
        // A 12-hex-char token must not be sliced into an 8-char id.
        let mut known = std::collections::HashMap::new();
        known.insert("aaaaaaaa".to_string(), ("acct@x".to_string(), "uuid-1".to_string()));
        assert!(parse_selected_uuid8s("aaaaaaaabbbb", &known).is_empty());
    }

    #[test]
    fn empty_response_yields_nothing() {
        let known = std::collections::HashMap::new();
        assert!(parse_selected_uuid8s("", &known).is_empty());
    }
}

#[cfg(test)]
mod ymd_tests {
    use super::*;

    // Every expected string below was computed BY HAND from the ms value,
    // never by running `ymd` and copying its output — otherwise a bug in
    // `ymd` (e.g. the epoch offset 719_468, or the `+ 1` on `d`) would be
    // enshrined as correct instead of caught. See each test's comment for
    // the arithmetic.

    #[test]
    fn epoch_is_1970_01_01() {
        assert_eq!(ymd(0), "1970-01-01");
    }

    #[test]
    fn leap_day_2024_02_29() {
        // Days from 1970-01-01 to 2024-02-29, counted independently:
        //   2000-01-01 is a well-known reference point, Unix ts 946_684_800.
        //   2000..=2023 is 24 years; leap years in that span (2000, 04, 08,
        //   12, 16, 20 — 2000 qualifies because it's divisible by 400) = 6,
        //   so 2000-01-01 -> 2024-01-01 is 24*365 + 6 = 8766 days.
        //   2024-01-01 -> 2024-02-29 is 31 (all of Jan) + 28 (Feb 1..=29,
        //   i.e. day 29 is offset 28 from day 1) = 59 days.
        //   Total: 8766 + 59 = 8825 days since 2000-01-01, i.e.
        //   946_684_800 + 8825*86_400 = 946_684_800 + 762_480_000
        //   = 1_709_164_800 seconds = 1_709_164_800_000 ms.
        assert_eq!(ymd(1_709_164_800_000), "2024-02-29");
    }

    #[test]
    fn year_boundary_2023_12_31_to_2024_01_01() {
        // 2024-01-01 00:00:00 UTC = 1_704_067_200 s (946_684_800 +
        // 8766*86_400, using the 8766-day count derived in the leap-day
        // test above). One day (86_400_000 ms) earlier is 2023-12-31.
        assert_eq!(ymd(1_704_067_200_000 - 86_400_000), "2023-12-31");
        assert_eq!(ymd(1_704_067_200_000), "2024-01-01");
    }

    #[test]
    fn pre_epoch_negative_ms_exercises_div_euclid() {
        // ms = -1, one millisecond before the epoch. This must NOT be an
        // exact multiple of the day length (86_400_000): for an exact
        // multiple, truncating division and div_euclid agree (both give
        // -1 for -86_400_000 / 86_400_000), so that would not distinguish
        // them. -1 is the sharpest case: div_euclid(-1, 86_400_000) floors
        // toward negative infinity, giving days = -1 (1969-12-31, correct),
        // while truncating "/" rounds toward zero, giving days = 0
        // (1970-01-01, wrong — swapping div_euclid for a plain `/` in ymd
        // fails this test).
        assert_eq!(ymd(-1), "1969-12-31");
    }

    #[test]
    fn far_pre_epoch_ms_exercises_the_negative_z_branch() {
        // The `z < 0` branch of the era computation (z = days + 719_468)
        // needs days <= -719_469. Using days = -719_469 exactly:
        //   ms = -719_469 * 86_400_000 = -62_162_121_600_000.
        //
        // Independently walked the algorithm by hand for that ms value
        // (not by running `ymd`):
        //   days = ms.div_euclid(86_400_000) = -719_469 (exact multiple).
        //   z = days + 719_468 = -1.
        //   z < 0, so era = (z - 146_096) / 146_097 = (-1 - 146_096) / 146_097
        //        = -146_097 / 146_097 = -1 (exact).
        //   doe = z - era*146_097 = -1 - (-1*146_097) = -1 + 146_097 = 146_096.
        //   doe/1460 = 146_096/1460 = 100 (1460*100=146_000, remainder 96).
        //   doe/36524 = 146_096/36_524 = 4 (36_524*4=146_096 exactly).
        //   doe/146096 = 146_096/146_096 = 1 (exact).
        //   yoe = (146_096 - 100 + 4 - 1)/365 = 145_999/365 = 399
        //        (365*399=145_635, remainder 364).
        //   y = yoe + era*400 = 399 + (-1*400) = -1.
        //   365*yoe = 365*399 = 145_635; yoe/4 = 399/4 = 99; yoe/100 = 399/100 = 3.
        //   doy = doe - (145_635 + 99 - 3) = 146_096 - 145_731 = 365.
        //   mp = (5*365+2)/153 = 1827/153 = 11 (153*11=1683, remainder 144).
        //   d = doy - (153*mp+2)/5 + 1 = 365 - (1683+2)/5 + 1
        //      = 365 - 1685/5 + 1 = 365 - 337 + 1 = 29.
        //   m = mp>=10, so m = mp-9 = 11-9 = 2.
        //   m<=2, so final y = y+1 = -1+1 = 0.
        //   Result: y=0, m=2, d=29 -> "0000-02-29".
        assert_eq!(ymd(-62_162_121_600_000), "0000-02-29");
    }
}
