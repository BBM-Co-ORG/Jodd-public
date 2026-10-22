//! Question-shaped keyword extraction.
//!
//! Deliberately NOT autolink::extract_keywords. That one keeps a token only
//! if it is capitalized somewhere or repeats twice — correct for long,
//! repetitive note bodies, and useless for a one-sentence question, which
//! repeats nothing (spec F3). Thai fails both of its branches structurally.
//! Both extractors are correct for their own input distribution.

/// English + Thai stopwords. Intentionally short: the ≥3-char rule and the
/// FTS index do most of the filtering. Mirrors autolink's list plus the
/// interrogatives and auxiliaries that only appear in questions.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "of", "to", "in", "on", "at", "for",
    "with", "is", "are", "was", "were", "be", "been", "this", "that", "it",
    "as", "by", "from", "not", "we", "you", "he", "she", "they", "my", "our",
    "what", "which", "who", "whom", "whose", "when", "where", "why", "how",
    "did", "do", "does", "done", "have", "has", "had", "can", "could", "would",
    "should", "about", "any", "anything", "some", "there", "then", "than",
    "all", "also", "into", "over", "out", "up", "down", "me", "him", "her",
    "และ", "หรือ", "ใน", "ที่", "เป็น", "จะ", "ได้", "มี", "ไม่", "ให้",
    "อะไร", "ไหน", "ทำไม", "อย่างไร", "บ้าง", "ผม", "ฉัน", "เรา",
];

/// Content words from a question, lowercased, de-duplicated, first-seen order
/// preserved (so the result is deterministic), capped at `max_terms`.
///
/// Splits on `db::is_tag_word_char` (letter/digit/underscore/hyphen, plus the
/// combining tone/vowel-mark ranges for Thai, Lao, Devanagari, Arabic, Hebrew,
/// Cyrillic, and Latin diacritics), NOT on `char::is_alphanumeric`. Thai (and
/// several other scripts) glue tone/vowel marks onto base letters, and those
/// marks are Unicode category Mn — `is_alphanumeric() == false` — so a plain
/// alphanumeric split shreds a Thai word at every mark (e.g. "เกี่ยวกับ" ->
/// "เกี" + "ยวกับ"). Reusing db.rs's tag word class (the same fix `slugify`
/// already relies on) keeps hyphenated tags (`agent-cli`) as one token too.
/// Thai still has no inter-word spaces, so a Thai phrase still arrives as one
/// long token spanning multiple actual words; this fix stops mid-word
/// shredding, it does NOT segment Thai into words. The FTS index is
/// trigram-tokenized and matches substrings regardless, which is why this
/// stays one of three pool sources rather than the only one (spec §5.5).
pub fn extract_query_terms(question: &str, max_terms: usize) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();

    for raw in question.split(|c: char| !crate::db::is_tag_word_char(c)) {
        let word = raw.trim().trim_matches('-');
        if word.chars().count() < 3 {
            continue;
        }
        let lower = word.to_lowercase();
        if STOPWORDS.contains(&lower.as_str()) {
            continue;
        }
        if seen.insert(lower.clone()) {
            out.push(lower);
            if out.len() >= max_terms {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact strings from spec F3 that autolink::extract_keywords fails.
    #[test]
    fn english_question_yields_content_words() {
        let t = extract_query_terms("what did I decide about sync conflicts?", 8);
        assert!(t.contains(&"decide".to_string()), "got {t:?}");
        assert!(t.contains(&"sync".to_string()), "got {t:?}");
        assert!(t.contains(&"conflicts".to_string()), "got {t:?}");
        assert!(!t.contains(&"what".to_string()), "'what' is a stopword");
        assert!(!t.contains(&"did".to_string()), "'did' is a stopword");
    }

    #[test]
    fn does_not_require_capitalization_or_repetition() {
        // The exact failure mode of autolink::extract_keywords (spec F3):
        // every token appears once and only "CLI" is capitalized.
        let t = extract_query_terms("summarize what I learned about agent CLI providers", 8);
        assert!(t.contains(&"agent".to_string()), "got {t:?}");
        assert!(t.contains(&"providers".to_string()), "got {t:?}");
        assert!(t.contains(&"summarize".to_string()), "got {t:?}");
    }

    #[test]
    fn keeps_latin_tokens_out_of_thai_text() {
        let t = extract_query_terms("ผมสรุปอะไรไว้เกี่ยวกับ ATLAS บ้าง", 8);
        assert!(t.contains(&"atlas".to_string()), "got {t:?}");
    }

    // Tone/vowel marks (Unicode Mn) are NOT is_alphanumeric, so splitting on
    // "not alphanumeric and not '-'" shreds Thai words at every mark. This
    // must not split the Thai portion into meaningless fragments like
    // "ผมสรุปอะไรไว", "เกี", "ยวกับ" (measured against the real bug).
    #[test]
    fn does_not_shred_thai_words_at_tone_marks() {
        let t = extract_query_terms("ผมสรุปอะไรไว้เกี่ยวกับ ATLAS บ้าง", 8);
        assert!(t.contains(&"atlas".to_string()), "got {t:?}");
        assert!(
            !t.iter().any(|x| x == "ผมสรุปอะไรไว"),
            "Thai word was shredded at a tone mark: got {t:?}"
        );
        assert!(
            !t.iter().any(|x| x == "เกี"),
            "Thai word was shredded at a tone mark: got {t:?}"
        );
        assert!(
            !t.iter().any(|x| x == "ยวกับ"),
            "Thai word was shredded at a tone mark: got {t:?}"
        );
    }

    #[test]
    fn drops_tokens_under_three_chars() {
        let t = extract_query_terms("is my k8s ok", 8);
        assert!(!t.iter().any(|x| x.chars().count() < 3), "got {t:?}");
        assert!(t.contains(&"k8s".to_string()), "got {t:?}");
    }

    #[test]
    fn caps_at_max_terms_and_is_deterministic() {
        let q = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
        let a = extract_query_terms(q, 4);
        let b = extract_query_terms(q, 4);
        assert_eq!(a.len(), 4);
        assert_eq!(a, b, "same input must give the same order");
    }

    #[test]
    fn deduplicates_repeated_words() {
        let t = extract_query_terms("sync sync sync conflicts", 8);
        assert_eq!(t.iter().filter(|x| *x == "sync").count(), 1, "got {t:?}");
    }

    #[test]
    fn empty_question_yields_no_terms() {
        assert!(extract_query_terms("   ", 8).is_empty());
    }
}
