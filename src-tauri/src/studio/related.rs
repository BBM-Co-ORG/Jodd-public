//! Related notes for a Studio run (stage 2a): notes from OTHER folders of the
//! same account that link with, share tags with, or mention the Focus or the
//! title words of the research folder's notes. Local SQL only — no AI call, nothing sent anywhere.
//!
//! Rules the tests below pin:
//! - the seeds are the folder's notes the AI may read, minus earlier Studio
//!   notes (their citations would make every old source look "linked");
//! - a hidden note is never suggested, and nothing is reached through one;
//! - a tag carried by a large share of the account is noise, not a reason;
//! - a word from the folder's titles counts only in another note's title; a
//!   Focus word may be in its own text too (never a saved source transcript,
//!   never the folder name);
//! - a spaced-script word counts only as a whole word, and a word on a large
//!   share of the account is noise, like a common tag.

use std::collections::{HashMap, HashSet};

use super::plan::{allowed_subtree, date_ms, is_studio_note, note_text, MAX_LISTED_NOTES};
use crate::ask::terms::extract_query_terms;
use crate::agent_memory::text::html_to_text;
use crate::db::{CachedNote, Db};
use crate::privacy::ai_scope::AiScope;

/// At most this many search terms are used for the words signal.
pub const MAX_TERMS: usize = 8;
/// At most this many suggestions are returned.
pub const RELATED_MAX: usize = 20;
/// A tag on more notes than `max(NOISE_TAG_FLOOR, account_notes / 5)` is noise.
pub const NOISE_TAG_FLOOR: usize = 10;
/// A word on more notes outside the folder than
/// `max(NOISE_WORD_FLOOR, account_notes / 20)` is noise. Stricter than the
/// tag rule (1/5): words are far commoner than tags, and an ingested note's
/// saved transcript mentions nearly every everyday word (live pass 2026-10-26).
pub const NOISE_WORD_FLOOR: usize = 10;
/// At most this many outside notes are read per search term (best match first).
const WORDS_CAP: usize = 200;

#[derive(Debug, Clone, serde::Serialize)]
pub struct RelatedNote {
    pub uuid: String,
    pub title: String,
    pub label: String,
    pub date_ms: i64,
    pub chars: usize,
    /// Distinct folder notes this one is linked with, either direction.
    pub linked_with: usize,
    /// Shared, non-noise tags, sorted.
    pub tags: Vec<String>,
    /// Matched search terms, in term order.
    pub terms: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct StudioRelated {
    pub notes: Vec<RelatedNote>,
    /// True when more than `RELATED_MAX` qualified.
    pub capped: bool,
}

/// What the finder knows about one candidate while it accumulates signals.
struct Hit {
    note: CachedNote,
    /// Distinct seeds this note is linked with (either direction).
    seeds: HashSet<String>,
    tags: Vec<String>,
    /// Search terms whose hits include this note, in term order.
    terms: Vec<String>,
}

/// Who may become a candidate (spec §4.3): another account, the research
/// subtree and a hidden folder never get an entry.
struct Gate<'a> {
    account_id: &'a str,
    ai: &'a AiScope,
    in_subtree: HashSet<&'a str>,
}

impl Gate<'_> {
    /// The note's entry, created on first sight, or None when it may not be one.
    fn admit<'h>(&self, hits: &'h mut HashMap<String, Hit>, n: CachedNote) -> Option<&'h mut Hit> {
        if n.account_id != self.account_id || self.in_subtree.contains(n.uuid.as_str()) || !self.ai.allows(self.account_id, &n.label) {
            return None;
        }
        Some(hits.entry(n.uuid.clone()).or_insert_with(|| Hit { note: n, seeds: HashSet::new(), tags: Vec::new(), terms: Vec::new() }))
    }
}

/// Suggest notes from other folders of `account_id` for a Studio run over
/// `label` (spec §4). `focus` and the seeds' titles feed the words signal.
pub fn find_related(db: &Db, ai: &AiScope, account_id: &str, label: &str, focus: &str) -> Result<StudioRelated, String> {
    // The whole readable subtree is excluded from the candidates; only the
    // non-Studio, newest MAX_LISTED_NOTES are seeds.
    let subtree = allowed_subtree(db, ai, account_id, label)?;
    let subtree_uuids: Vec<String> = subtree.iter().map(|n| n.uuid.clone()).collect();
    let creators = db.creators_for(account_id, &subtree_uuids).map_err(|e| format!("read provenance: {e}"))?;
    let seeds: Vec<&CachedNote> = subtree.iter().filter(|n| !is_studio_note(n, creators.get(&n.uuid))).take(MAX_LISTED_NOTES).collect();
    if seeds.is_empty() {
        return Ok(StudioRelated::default());
    }

    let mut hits: HashMap<String, Hit> = HashMap::new();
    // Candidates are filtered as they arrive.
    let gate = Gate { account_id, ai, in_subtree: subtree.iter().map(|n| n.uuid.as_str()).collect() };

    // Links, both directions, one hop.
    for seed in &seeds {
        let linked = db
            .outgoing_links(account_id, &seed.uuid)
            .and_then(|mut out| db.backlinks(account_id, &seed.uuid).map(|back| { out.extend(back); out }))
            .map_err(|e| format!("read links: {e}"))?;
        for n in linked {
            if let Some(h) = gate.admit(&mut hits, n) {
                h.seeds.insert(seed.uuid.clone());
            }
        }
    }

    // Tags the seeds carry, minus the ones so common they say nothing.
    let account_notes = db.count_notes_in_scope(Some(account_id), None, &[]).map_err(|e| format!("count notes: {e}"))?;
    let noise_above = NOISE_TAG_FLOOR.max(account_notes / 5);
    let tag_counts: HashMap<String, i64> = db.list_all_tags(account_id).map_err(|e| format!("read tags: {e}"))?.into_iter().collect();
    let mut seed_tags: Vec<String> = Vec::new();
    for seed in &seeds {
        seed_tags.extend(db.list_tags_for(account_id, &seed.uuid).map_err(|e| format!("read tags: {e}"))?);
    }
    seed_tags.sort();
    seed_tags.dedup();
    seed_tags.retain(|t| tag_counts.get(t).copied().unwrap_or(0) as usize <= noise_above);
    for tag in &seed_tags {
        for n in db.notes_sharing_tags(account_id, std::slice::from_ref(tag), "").map_err(|e| format!("read tagged notes: {e}"))? {
            if let Some(h) = gate.admit(&mut hits, n) {
                h.tags.push(tag.clone());
            }
        }
    }

    // Words: the Focus line's terms, then the words the folder's titles share.
    // The query searches title and text only (a folder-name match is not a
    // mention) and leaves the research subtree out before its cap — its own
    // notes contain the title words by construction and would fill it.
    // The trigram index matches inside words, so a spaced-script term is
    // re-checked as a whole word ("move" is not in "remove"); Thai keeps the
    // substring match it needs. A word on too many notes is dropped, as tags are.
    //
    // A word from the folder's titles names a topic, so it must be in the
    // other note's TITLE; a Focus word is the owner's own and may be in the
    // text too. Bodies are where long transcripts live (live pass 2026-10-26).
    let titles: Vec<&str> = seeds.iter().map(|n| n.title.as_str()).collect();
    let focus_terms: HashSet<String> = pick_terms(focus, &[]).into_iter().collect();
    let word_noise_above = NOISE_WORD_FLOOR.max(account_notes / 20);
    // Lowercased title + the note's OWN text, read once however many terms hit
    // it. An ingest's verbatim source block is left out: a saved video
    // transcript mentions nearly every everyday word (live pass 2026-10-26).
    let mut texts: HashMap<String, String> = HashMap::new();
    for term in pick_terms(focus, &titles) {
        let whole = needs_whole_word(&term);
        let in_text = focus_terms.contains(&term);
        let found: Vec<CachedNote> = db
            .search_text_outside_subtree(account_id, label, &term, WORDS_CAP)
            .map_err(|e| format!("search notes: {e}"))?
            .into_iter()
            .filter(|n| {
                if !in_text {
                    return mentions(&n.title.to_lowercase(), &term, whole);
                }
                let text = texts
                    .entry(n.uuid.clone())
                    .or_insert_with(|| format!("{}\n{}", n.title.to_lowercase(), html_to_text(&n.body_html, false).to_lowercase()));
                mentions(text, &term, whole)
            })
            .collect();
        if found.len() > word_noise_above {
            continue;
        }
        for n in found {
            if let Some(h) = gate.admit(&mut hits, n) {
                h.terms.push(term.clone());
            }
        }
    }

    // An earlier Studio note elsewhere is never a suggestion.
    let candidate_uuids: Vec<String> = hits.keys().cloned().collect();
    let candidate_creators = db.creators_for(account_id, &candidate_uuids).map_err(|e| format!("read provenance: {e}"))?;
    // `chars` is filled in after the cap: it reads the whole text.
    let mut ranked: Vec<(RelatedNote, CachedNote)> = hits
        .into_values()
        .filter(|h| !is_studio_note(&h.note, candidate_creators.get(&h.note.uuid)))
        .map(|mut h| {
            h.tags.sort();
            let r = RelatedNote {
                uuid: h.note.uuid.clone(),
                title: h.note.title.clone(),
                label: h.note.label.clone(),
                date_ms: date_ms(&h.note),
                chars: 0,
                linked_with: h.seeds.len(),
                tags: h.tags,
                terms: h.terms,
            };
            (r, h.note)
        })
        .collect();
    ranked.sort_by(|(a, _), (b, _)| {
        signals(b)
            .cmp(&signals(a))
            .then_with(|| b.linked_with.cmp(&a.linked_with))
            .then_with(|| b.tags.len().cmp(&a.tags.len()))
            .then_with(|| b.terms.len().cmp(&a.terms.len()))
            .then_with(|| b.date_ms.cmp(&a.date_ms))
            .then_with(|| a.uuid.cmp(&b.uuid))
    });
    let capped = ranked.len() > RELATED_MAX;
    ranked.truncate(RELATED_MAX);
    let notes = ranked
        .into_iter()
        .map(|(mut r, n)| {
            r.chars = note_text(&n).chars().count();
            r
        })
        .collect();
    Ok(StudioRelated { notes, capped })
}

/// The search terms for the words signal (spec §4.2 item 3), at most
/// `MAX_TERMS`: the Focus line's terms in order, then the seed titles' terms
/// by how many titles contain them (most first), then alphabetically. A term
/// found in one title only is used just while there is room. A term with no
/// letter (`2026`, `2026-10-25`) is never used: in a folder of dated notes it
/// would suggest every note that mentions the year (spec §2, noise).
fn pick_terms(focus: &str, seed_titles: &[&str]) -> Vec<String> {
    let has_letter = |t: &String| t.chars().any(char::is_alphabetic);
    let mut terms: Vec<String> = extract_query_terms(focus, usize::MAX).into_iter().filter(has_letter).collect();
    let mut title_counts: HashMap<String, usize> = HashMap::new();
    for title in seed_titles {
        // Each term counts once per title.
        for term in extract_query_terms(title, usize::MAX).into_iter().filter(has_letter) {
            *title_counts.entry(term).or_insert(0) += 1;
        }
    }
    let mut from_titles: Vec<(String, usize)> = title_counts.into_iter().filter(|(t, _)| !terms.contains(t)).collect();
    from_titles.sort_by(|(a, na), (b, nb)| nb.cmp(na).then_with(|| a.cmp(b)));
    terms.extend(from_titles.into_iter().map(|(t, _)| t));
    terms.truncate(MAX_TERMS);
    terms
}

/// Whether a term must match as a whole word: true unless it holds a letter
/// of a script written without spaces between words (Thai, Lao, Myanmar,
/// Khmer, Japanese kana, CJK ideographs), where a word boundary cannot be seen.
fn needs_whole_word(term: &str) -> bool {
    !term.chars().any(|c| {
        matches!(c as u32,
            0x0E00..=0x0EFF            // Thai, Lao
            | 0x1000..=0x109F          // Myanmar
            | 0x1780..=0x17FF          // Khmer
            | 0x3040..=0x30FF          // Hiragana, Katakana
            | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF) // CJK ideographs
    })
}

/// Whether lowercased `text` mentions lowercased `term`: anywhere, or only
/// with no letter or digit right before and after it when `whole`.
fn mentions(text: &str, term: &str, whole: bool) -> bool {
    if !whole {
        return text.contains(term);
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(term).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + term.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// How many of the three signals (links, tags, words) a suggestion has.
fn signals(n: &RelatedNote) -> usize {
    usize::from(n.linked_with > 0) + usize::from(!n.tags.is_empty()) + usize::from(!n.terms.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privacy::ai_scope::HIDDEN_FOLDER;
    use crate::studio::STUDIO_MARKER;
    use crate::test_support::{note, temp_db};

    const A: &str = "gmail:a@x.com";
    const R: &str = "Notes/R";

    fn open() -> AiScope {
        AiScope::hiding(A, &[])
    }

    fn find(db: &Db, ai: &AiScope) -> StudioRelated {
        find_related(db, ai, A, R, "").unwrap()
    }

    fn find_focus(db: &Db, focus: &str) -> StudioRelated {
        find_related(db, &open(), A, R, focus).unwrap()
    }

    fn got(r: &StudioRelated) -> Vec<String> {
        r.notes.iter().map(|n| n.uuid.clone()).collect()
    }

    fn sorted(r: &StudioRelated) -> Vec<String> {
        let mut v = got(r);
        v.sort();
        v
    }

    fn by<'a>(r: &'a StudioRelated, uuid: &str) -> &'a RelatedNote {
        r.notes.iter().find(|n| n.uuid == uuid).unwrap_or_else(|| panic!("{uuid} not suggested: {:?}", got(r)))
    }

    #[test]
    fn links_count_both_directions_and_distinct_seeds() {
        let db = temp_db();
        note(A, "r1").title("R1").body("see [[X1]]").label(R).insert(&db);
        note(A, "r2").title("R2").body("plain").label(R).insert(&db);
        note(A, "r3").title("R3").body("plain").label(R).insert(&db);
        note(A, "x1").title("X1").body("plain").label("Notes/X").insert(&db);
        // x2 links to r2 (backlink of r2) and r3 is linked from x2 too (forward from x2 = backlink of r3).
        note(A, "x2").title("X2").body("about [[R2]] and [[R3]]").label("Notes/X").insert(&db);
        let r = find(&db, &open());
        assert_eq!(by(&r, "x1").linked_with, 1);
        assert_eq!(by(&r, "x2").linked_with, 2);
        assert_eq!(got(&r), vec!["x2", "x1"]);
        assert!(!r.capped);
        assert!(by(&r, "x1").terms.is_empty());
    }

    #[test]
    fn a_hidden_note_is_never_suggested_or_walked() {
        let db = temp_db();
        note(A, "r1").title("R1").body("see [[X1]] and [[X3]]").label(R).insert(&db);
        note(A, "x1").title("X1").body("plain").label("Notes/P").insert(&db);
        note(A, "x2").title("X2").body("only [[X1]]").label("Notes/X").insert(&db);
        note(A, "x3").title("X3").body("fine").label("Notes/X").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/P"]);
        assert_eq!(sorted(&find(&db, &ai)), vec!["x3"]);
    }

    #[test]
    fn noise_tags_are_ignored_at_the_threshold() {
        let db = temp_db();
        // 12 notes: r1 + x1..x10 + x3 carries #rare too (it is one of the ten) + one plain.
        note(A, "r1").title("R1").body("a #common #rare").label(R).insert(&db);
        for i in 1..=10 {
            let body = if i == 3 { "b #common #rare" } else { "b #common" };
            note(A, &format!("x{i}")).title(&format!("X{i}")).body(body).label("Notes/X").insert(&db);
        }
        note(A, "z").title("Z").body("plain").label("Notes/X").insert(&db);
        // #common is on 11 of 12 notes: > max(10, 12/5) -> noise.
        let r = find(&db, &open());
        assert_eq!(sorted(&r), vec!["x3"]);
        assert_eq!(by(&r, "x3").tags, vec!["rare".to_string()]);
    }

    #[test]
    fn a_tag_on_exactly_ten_notes_is_not_noise() {
        let db = temp_db();
        note(A, "r1").title("R1").body("a #shared").label(R).insert(&db);
        for i in 1..=9 {
            note(A, &format!("x{i}")).title(&format!("X{i}")).body("b #shared").label("Notes/X").insert(&db);
        }
        note(A, "z1").title("Z1").body("plain").label("Notes/X").insert(&db);
        note(A, "z2").title("Z2").body("plain").label("Notes/X").insert(&db);
        let r = find(&db, &open());
        assert_eq!(r.notes.len(), 9);
        assert_eq!(by(&r, "x1").tags, vec!["shared".to_string()]);
    }

    #[test]
    fn seeds_subtree_studio_other_account_and_deleted_are_never_suggested() {
        let db = temp_db();
        let studio = format!("{STUDIO_MARKER} from 2 notes");
        note(A, "r1")
            .title("R1")
            .body(&format!("[[Sub]] [[R2]] [[{studio}]] [[Foreign]] [[Gone]] [[Kept]]"))
            .label(R)
            .insert(&db);
        note(A, "r2").title("R2").body("plain").label(R).insert(&db);
        note(A, "sub").title("Sub").body("plain").label("Notes/R/Sub").insert(&db);
        note(A, "st").title(&studio).body("plain").label("Notes/X").insert(&db);
        note("other@x", "foreign").title("Foreign").body("plain").label("Notes/X").insert(&db);
        note(A, "gone").title("Gone").body("plain").label("Notes/X").insert(&db);
        db.mark_deleted("gone", A).unwrap();
        note(A, "kept").title("Kept").body("plain").label("Notes/X").insert(&db);
        assert_eq!(sorted(&find(&db, &open())), vec!["kept"]);
    }

    #[test]
    fn an_earlier_studio_note_is_not_a_seed() {
        let db = temp_db();
        let studio = format!("{STUDIO_MARKER} from 1 note");
        note(A, "st").title(&studio).body("cites [[X1]]").label(R).insert(&db);
        note(A, "x1").title("X1").body("plain").label("Notes/X").insert(&db);
        assert!(find(&db, &open()).notes.is_empty());
    }

    #[test]
    fn only_studio_notes_means_no_seeds_and_no_error() {
        let db = temp_db();
        let studio = format!("{STUDIO_MARKER} from 1 note");
        note(A, "st").title(&studio).body("cites [[X1]] #t").label(R).insert(&db);
        note(A, "x1").title("X1").body("plain #t").label("Notes/X").insert(&db);
        let r = find_related(&db, &open(), A, R, "").unwrap();
        assert!(r.notes.is_empty());
        assert!(!r.capped);
        // An empty folder is the same.
        assert!(find_related(&temp_db(), &open(), A, R, "").unwrap().notes.is_empty());
    }

    #[test]
    fn ranking_and_cap() {
        let db = temp_db();
        let links: String = (1..=21).map(|i| format!("[[X{i:02}]] ")).collect();
        note(A, "r1").title("R1").body(&format!("{links} #t #u")).label(R).insert(&db);
        note(A, "r2").title("R2").body("see [[X05]]").label(R).insert(&db);
        for i in 1..=21 {
            let body = match i {
                3 => "b #t",
                7 => "b #t #u",
                _ => "b",
            };
            // The fixture clock is real time; pin every date so only x20 is newer.
            let ms = if i == 20 { 2_000_000_000_000 } else { 1_000_000_000_000 };
            note(A, &format!("x{i:02}")).title(&format!("X{i:02}")).body(body).label("Notes/X").modified_ms(ms).insert(&db);
        }
        let r = find(&db, &open());
        assert_eq!(r.notes.len(), RELATED_MAX);
        assert!(r.capped);
        let order = got(&r);
        // Two signals first: x07 has two tags, x03 one. Then x05 (linked twice). Then the
        // rest: newest first (x20), then uuid. x21 is the one the cap drops.
        assert_eq!(&order[..4], &["x07", "x03", "x05", "x20"]);
        assert_eq!(order[4], "x01");
        assert_eq!(order[5], "x02");
        assert!(!order.contains(&"x21".to_string()), "{order:?}");
        // Every survivor carries its size, measured after the cap.
        let x07 = db.note_by_uuid(A, "x07").unwrap().unwrap();
        assert_eq!(by(&r, "x07").chars, note_text(&x07).chars().count());
        assert!(r.notes.iter().all(|n| n.chars > 0));
    }

    /// Spec §4.5: the finder is local SQL, up to 2x200 link queries. A
    /// generous bound (the debug build on CI) so a regression shows.
    #[test]
    fn two_hundred_seeds_stay_fast() {
        let db = temp_db();
        for i in 0..MAX_LISTED_NOTES {
            note(A, &format!("r{i:03}"))
                .title(&format!("R{i:03}"))
                .body(&format!("see [[X{i:03}]] #t{}", i % 7))
                .label(R)
                .insert(&db);
            note(A, &format!("x{i:03}")).title(&format!("X{i:03}")).body("b").label("Notes/X").insert(&db);
        }
        let start = std::time::Instant::now();
        let r = find(&db, &open());
        let took = start.elapsed();
        assert!(r.capped && r.notes.len() == RELATED_MAX, "{}", r.notes.len());
        assert!(took < std::time::Duration::from_secs(5), "took {took:?}");
    }

    #[test]
    fn focus_terms_come_first_then_shared_title_words() {
        let t = pick_terms("battery pricing", &["EV battery cost", "Battery recycling", "Solar"]);
        // "battery" once (focus), "pricing" next; the title words are single-title,
        // so alphabetical. "ev" is too short.
        assert_eq!(t, vec!["battery", "pricing", "cost", "recycling", "solar"]);
        // A word in more titles outranks an earlier-sorting word in one.
        let t = pick_terms("", &["alpha shared", "beta shared", "zulu"]);
        assert_eq!(t, vec!["shared", "alpha", "beta", "zulu"]);
        // Never more than MAX_TERMS.
        let titles: Vec<String> = (0..20).map(|i| format!("word{i:02}")).collect();
        let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
        assert_eq!(pick_terms("focus one two", &refs).len(), MAX_TERMS);
        assert_eq!(pick_terms("focus one two", &refs)[..2], ["focus", "one"]);
    }

    #[test]
    fn a_term_without_a_letter_is_never_used() {
        assert_eq!(pick_terms("", &["Weekly 2026-10-25", "Weekly 2026-10-18"]), vec!["weekly"]);
        assert_eq!(pick_terms("2026 battery", &[]), vec!["battery"]);
        // Thai letters are letters.
        assert_eq!(pick_terms("", &["บันทึก 2026", "บันทึก 2025"]), vec!["บันทึก"]);
        // A mixed run keeps its digits.
        assert_eq!(pick_terms("", &["Q3-2026 plan"]), vec!["plan", "q3-2026"]);
    }

    #[test]
    fn the_research_folder_does_not_crowd_out_an_outside_word_match() {
        let db = temp_db();
        // More research notes than the per-term cap, every one dense in the shared
        // title word, so they all outrank the outside note.
        for i in 0..(WORDS_CAP + 50) {
            note(A, &format!("r{i:03}")).title(&format!("Battery log {i:03}")).body("battery battery").label(R).insert(&db);
        }
        let filler = "unrelated words ".repeat(200);
        note(A, "x1")
            .title("X1 battery")
            .body(&filler)
            .label("Notes/X")
            .modified_ms(1_000_000_000_000)
            .insert(&db);
        // The scenario is real: an account-wide search hits the cap before x1.
        let wide = db.search_notes(Some(A), None, "battery", &[]).unwrap();
        assert_eq!(wide.len(), 200);
        assert!(wide.iter().all(|n| n.uuid != "x1"));
        let r = find(&db, &open());
        assert_eq!(got(&r), vec!["x1"]);
        assert_eq!(by(&r, "x1").terms, vec!["battery".to_string()]);
    }

    #[test]
    fn a_thai_focus_term_is_kept_whole() {
        let t = pick_terms("เกี่ยวกับแบตเตอรี่", &[]);
        assert!(t.contains(&"เกี่ยวกับแบตเตอรี่".to_string()), "{t:?}");
    }

    #[test]
    fn short_or_empty_focus_adds_nothing() {
        for focus in ["", "ev", "!!"] {
            assert_eq!(pick_terms(focus, &["Solar panels"]), vec!["panels", "solar"], "focus {focus:?}");
        }
        let db = temp_db();
        note(A, "r1").title("R1").body("plain").label(R).insert(&db);
        for focus in ["", "ev", "!!"] {
            assert!(find_focus(&db, focus).notes.is_empty());
        }
    }

    #[test]
    fn a_word_in_title_or_text_suggests_with_its_term() {
        let db = temp_db();
        note(A, "r1").title("R1").body("plain").label(R).insert(&db);
        note(A, "x1").title("X1").body("<div>Battery costs fell</div>").label("Notes/X").insert(&db);
        note(A, "x2").title("Battery notes").body("nothing").label("Notes/X").insert(&db);
        note(A, "x3").title("X3").body("unrelated").label("Notes/X").insert(&db);
        let r = find_focus(&db, "battery");
        assert_eq!(sorted(&r), vec!["x1", "x2"]);
        assert_eq!(by(&r, "x1").terms, vec!["battery".to_string()]);
        assert_eq!(by(&r, "x2").terms, vec!["battery".to_string()]);
    }

    #[test]
    fn a_title_word_matches_other_titles_only() {
        // Live pass 2026-10-26: a folder's everyday title words matched long
        // transcript notes through their bodies. A title word is a topic; it
        // must be in the other note's title. Focus words may match the text.
        let db = temp_db();
        note(A, "r1").title("Solar panels").body("plain").label(R).insert(&db);
        note(A, "x1").title("Solar costs 2026").body("nothing").label("Notes/X").insert(&db);
        note(A, "x2").title("X2").body("a long talk that mentions solar once").label("Notes/X").insert(&db);
        let r = find_focus(&db, "");
        assert_eq!(got(&r), vec!["x1"]);
        assert_eq!(by(&r, "x1").terms, vec!["solar".to_string()]);
        // Typed as Focus, the same word may match the text.
        assert_eq!(sorted(&find_focus(&db, "solar")), vec!["x1", "x2"]);
    }

    #[test]
    fn a_folder_name_match_alone_does_not_suggest() {
        let db = temp_db();
        note(A, "r1").title("R1").body("plain").label(R).insert(&db);
        note(A, "x1").title("X1").body("nothing here").label("Notes/Battery").insert(&db);
        note(A, "x2").title("X2").body("a battery note").label("Notes/Battery").insert(&db);
        assert_eq!(sorted(&find_focus(&db, "battery")), vec!["x2"]);
    }

    #[test]
    fn words_never_reach_a_hidden_other_account_or_research_note() {
        let db = temp_db();
        note(A, "r1").title("R1").body("battery inside").label(R).insert(&db);
        note(A, "x1").title("X1").body("battery").label("Notes/P").insert(&db);
        note("other@x", "x2").title("X2").body("battery").label("Notes/X").insert(&db);
        note(A, "x3").title("X3").body("battery").label("Notes/X").insert(&db);
        let ai = AiScope::hiding(A, &["Notes/P"]);
        assert_eq!(sorted(&find_related(&db, &ai, A, R, "battery").unwrap()), vec!["x3"]);
    }

    #[test]
    fn words_count_as_one_signal_in_ranking() {
        let db = temp_db();
        note(A, "r1").title("R1").body("see [[X2]]").label(R).insert(&db);
        // x1 is newer, with words only; x2 has a link and words.
        note(A, "x1").title("X1").body("battery").label("Notes/X").modified_ms(2_000_000_000_000).insert(&db);
        note(A, "x2").title("X2").body("battery").label("Notes/X").modified_ms(1_000_000_000_000).insert(&db);
        let r = find_focus(&db, "battery");
        assert_eq!(got(&r), vec!["x2", "x1"]);
        assert_eq!(by(&r, "x2").linked_with, 1);
        assert_eq!(by(&r, "x1").linked_with, 0);
    }

    #[test]
    fn more_terms_alone_do_not_beat_a_second_signal() {
        let db = temp_db();
        note(A, "r1").title("R1").body("see [[X2]]").label(R).insert(&db);
        note(A, "x1").title("X1").body("alpha beta gamma").label("Notes/X").insert(&db);
        note(A, "x2").title("X2").body("alpha").label("Notes/X").insert(&db);
        let r = find_focus(&db, "alpha beta gamma");
        assert_eq!(got(&r), vec!["x2", "x1"]);
        assert_eq!(by(&r, "x1").terms, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn a_spaced_word_counts_only_as_a_whole_word() {
        let db = temp_db();
        note(A, "r1").title("R1").body("plain").label(R).insert(&db);
        // Live pass 2026-10-26: "move" suggested notes that only said "remove".
        note(A, "x1").title("X1").body("remove the ego and movement drills").label("Notes/X").insert(&db);
        note(A, "x2").title("X2").body("the move is planned").label("Notes/X").insert(&db);
        note(A, "x3").title("X3").body("Our move-in date. Move.").label("Notes/X").insert(&db);
        let r = find_focus(&db, "move");
        assert_eq!(sorted(&r), vec!["x2", "x3"]);
        assert_eq!(by(&r, "x2").terms, vec!["move".to_string()]);
    }

    #[test]
    fn a_word_only_in_a_saved_source_transcript_does_not_count() {
        let db = temp_db();
        note(A, "r1").title("R1").body("plain").label(R).insert(&db);
        // Live pass 2026-10-26: ingested video notes matched everyday words
        // only through the verbatim transcript saved below their summary.
        let ingested = format!(
            "<p>Summary about pricing</p><hr>\n<details>\n{}\n<pre>we talked about the internet and a plan</pre>\n</details>\n",
            crate::llm::markdown::SOURCE_MARKER
        );
        note(A, "x1").title("X1").body(&ingested).label("Notes/X").insert(&db);
        note(A, "x2").title("X2").body("<p>the internet quotes</p>").label("Notes/X").insert(&db);
        let r = find_focus(&db, "internet");
        assert_eq!(got(&r), vec!["x2"]);
        assert_eq!(by(&r, "x2").terms, vec!["internet".to_string()]);
    }

    #[test]
    fn a_thai_word_still_matches_inside_longer_text() {
        let db = temp_db();
        note(A, "r1").title("แบตเตอรี่").body("plain").label(R).insert(&db);
        // Thai has no spaces between words: whole-word matching would find nothing.
        note(A, "x1").title("ราคาแบตเตอรี่รถไฟฟ้า").body("ลดลง").label("Notes/X").insert(&db);
        note(A, "x2").title("X2").body("ราคาแบตเตอรี่รถไฟฟ้าลดลง").label("Notes/X").insert(&db);
        assert_eq!(got(&find_focus(&db, "")), vec!["x1"], "a title word, inside a longer Thai title");
        assert_eq!(sorted(&find_focus(&db, "แบตเตอรี่")), vec!["x1", "x2"], "a Focus word, inside longer Thai text");
    }

    #[test]
    fn whole_word_matching_applies_only_to_spaced_scripts() {
        assert!(needs_whole_word("move"));
        assert!(needs_whole_word("q3-2026"));
        assert!(needs_whole_word("café"));
        assert!(!needs_whole_word("แบตเตอรี่"));
        assert!(!needs_whole_word("ev-แบต"));
        assert!(!needs_whole_word("会議"));
        assert!(mentions("the move is on", "move", true));
        assert!(mentions("move", "move", true));
        assert!(mentions("(move)", "move", true));
        assert!(!mentions("remove", "move", true));
        assert!(!mentions("moves", "move", true));
        assert!(mentions("remove", "move", false));
    }

    #[test]
    fn a_word_on_too_many_notes_is_noise() {
        // 1 seed + 11 outside notes: max(10, 12 / 20) = 10, and "office" is on 11.
        let db = temp_db();
        note(A, "r1").title("R1").body("plain").label(R).insert(&db);
        for i in 0..11 {
            note(A, &format!("x{i:02}")).title(&format!("X{i:02}")).body("the office").label("Notes/X").insert(&db);
        }
        assert!(find_focus(&db, "office").notes.is_empty(), "a word on 11 notes says nothing");
        // On exactly 10 notes it is still a reason.
        db.mark_deleted("x10", A).unwrap();
        assert_eq!(find_focus(&db, "office").notes.len(), 10);
    }

    #[test]
    fn a_common_word_is_dropped_but_a_rare_one_still_suggests() {
        let db = temp_db();
        note(A, "r1").title("Meeting about Sathorn").body("plain").label(R).insert(&db);
        for i in 0..12 {
            note(A, &format!("m{i:02}")).title(&format!("Weekly meeting {i:02}")).body("notes").label("Notes/X").insert(&db);
        }
        note(A, "s1").title("Sathorn Square visit").body("floor 18").label("Notes/Y").insert(&db);
        let r = find_focus(&db, "");
        assert_eq!(got(&r), vec!["s1"]);
        assert_eq!(by(&r, "s1").terms, vec!["sathorn".to_string()]);
    }

    #[test]
    fn a_hidden_research_folder_is_refused() {
        let db = temp_db();
        note(A, "r1").title("R1").body("plain").label(R).insert(&db);
        let ai = AiScope::hiding(A, &[R]);
        assert_eq!(find_related(&db, &ai, A, R, "").unwrap_err(), HIDDEN_FOLDER);
    }
}
