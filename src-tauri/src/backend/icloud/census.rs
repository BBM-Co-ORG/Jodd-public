//! The M2 write census — **what fraction of a real account can Jodd write, and
//! why not the rest.**
//!
//! The write path refuses a note it cannot prove safe (`compose::writability`),
//! and each refusal is defensible on its own. What none of them is, before a
//! run like this, is *priced*: a gate that refuses 95% of an account is a
//! feature that does not exist, and the only way to know is to count. M1's
//! method one milestone later — three hypotheses about the note count were
//! refused by measurement before the right one, and none would have been
//! refutable by a log line that said what the code believed.
//!
//! # Why it lives in the library and not in the probe
//!
//! It started in `examples/icloud_probe`, which borrows **icloud-md's** stored
//! cookie jar — scaffolding from before this project had an iCloud session of
//! its own. That jar expires (HTTP 421) and refreshing it means a HAR capture
//! through a third-party tool, to measure an account Jodd is already signed
//! into. The session is in the app; the census belongs where it can reach it.
//!
//! So these are pure functions over the raw `changes/zone` records, rendered
//! to a `String`. The probe prints it, the `icloud_census` command returns it,
//! and there is one implementation rather than two that drift.
//!
//! # The rule every line here follows
//!
//! **Names, counts and histograms of opaque values — never a value that could
//! be note text.** This has to be runnable against somebody's real Apple ID,
//! which is the only kind of account whose numbers mean anything.

use std::collections::HashMap;
use std::fmt::Write as _;

use base64::Engine;
use serde_json::{json, Value};

use super::{compose, wire};

/// The whole census, as a report.
pub fn report(records: &[Value]) -> String {
    let mut out = String::new();
    let live: Vec<&Value> = records
        .iter()
        .filter(|r| r["recordType"] == json!("Note"))
        .filter(|r| !wire::is_deleted_record(r))
        // The WRITABLE percentage describes notes the user could actually
        // edit, and a note in Recently Deleted is not one. Those get their own
        // section below.
        .filter(|r| {
            r["fields"]["Folder"]["value"]["recordName"] != json!(wire::TRASH_FOLDER)
        })
        .collect();
    // The InlineAttachment map — hashtag texts and UTIs — feeds both the
    // narrowed InlineObjects gate and the M3 formatting section.
    let inline_refs = wire::collect_inline_refs(records);
    write_readiness(&live, &inline_refs, &mut out);
    folder_parentage(records, &mut out);
    trash_provenance(records, &mut out);
    tag_records(records, &mut out);
    tag_anatomy(records, &mut out);
    out
}

/// **What does a tag actually look like on the wire?**
///
/// M3 reads hashtags and refuses to create them, and the refusal is a data
/// problem rather than a code one: a tag is not text but a record — in fact
/// probably two, since the zone carries both `InlineAttachment` (this tag,
/// in this note, at this position) and `Hashtag` (the tag as an entity of
/// the account, which is what Apple's own tag browser lists). icloud-md
/// only ever reads them, so there is no reference implementation of a
/// CREATE to conform to, and guessing the field set of a private API is the
/// mistake this backend's whole history warns about.
///
/// This does the half that costs nothing: report which FIELD NAMES each
/// record type carries on a real account, so whoever picks the milestone up
/// starts from the account's own shape instead of a blank page.
///
/// **What that "remaining half" turned out to be is smaller than this comment
/// used to claim.** It said a capture of icloud.com creating a tag was the
/// only way forward, on the reasoning that stored records say what Apple
/// STORES and never what a create must SEND. True in general, and it stopped
/// short: most of what a create must send is DERIVABLE from what is stored,
/// once you ask about relations instead of values. `tag_anatomy` below did
/// that and left only the `records/modify` envelope open — a shape this
/// backend already has a proven pattern for.
///
/// Names and counts only, never a value — an `AltTextEncrypted` is the
/// user's own tag text, which is content (the rule every diagnostic in this
/// module follows, asserted by `the_census_prints_no_note_text_...`).
/// What a field's decoded bytes LOOK like, never what they say.
///
/// The first anatomy run answered `0/6` to three independent questions at
/// once, which is the signature of a wrong assumption about the field rather
/// than of strange data — and the report could not tell "decoded to text that
/// disagrees" from "did not decode to text at all", because a `None` from
/// `decode_text_field` and a decoded-but-different string both just failed the
/// comparison. This says which.
///
/// Lengths are reported only as RELATIONS between two fields (below), never
/// absolutely, since the length of a short tag is closer to content than a
/// character class is. A non-UTF-8 length is safe and load-bearing: 16 bytes
/// is a raw UUID, which is a different encoding, not a different value.
fn field_shape(field: &Value) -> String {
    let Some(b64) = field["value"].as_str() else { return "absent".into() };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return "not-base64".into();
    };
    let s = match std::str::from_utf8(&bytes) {
        Err(_) => return format!("bytes({})", bytes.len()),
        Ok(s) => s,
    };
    let mut cls: Vec<&str> = Vec::new();
    if s.starts_with('#') {
        cls.push("leading#");
    }
    if s.chars().any(|c| c.is_ascii_uppercase()) {
        cls.push("upper");
    }
    if s.chars().any(|c| c.is_ascii_lowercase()) {
        cls.push("lower");
    }
    if s.chars().any(|c| c.is_ascii_digit()) {
        cls.push("digit");
    }
    if s.chars().any(|c| !c.is_ascii()) {
        cls.push("non-ascii");
    }
    if s.chars().any(|c| c.is_ascii_whitespace()) {
        cls.push("space");
    }
    if uuid_shaped(s) {
        cls.push("uuid-shaped");
    }
    if cls.is_empty() {
        cls.push("plain");
    }
    format!("utf8:{}", cls.join("+"))
}

/// UUID shape, ignoring case — the distinction gotcha #18 exists for. The
/// first run compared identifiers with `==` and answered 0/9; a
/// case-insensitive pass is the cheapest way to find out whether that was the
/// whole story.
fn uuid_shaped(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() }
        })
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.eq_ignore_ascii_case(b)
}

/// How a `Hashtag` record's two text fields relate to each other.
///
/// Returned as a shape rather than asserted, because a mismatch on a real
/// account is a finding — the rule this account happens not to follow — and
/// not an error to fail on.
///
/// **Why relations and not values.** This module's contract (see the header)
/// is names, counts and histograms, never a value that could be user text, and
/// a tag name is user text. A create does not need the text anyway: it needs
/// the RULE that derives `StandardizedContentEncrypted` from
/// `DisplayTextEncrypted`. Reporting the rule as a checked count is also
/// stronger evidence than a human comparing two printed strings.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct HashtagTextRelation {
    pub display_has_hash: bool,
    /// `Standardized` is `Display` with a single leading `#` removed.
    pub standardized_is_display_sans_hash: bool,
    /// …and additionally lowercased. Both can be true when the tag is already
    /// lowercase, which is why they are counted apart rather than as one
    /// "matches" flag — a tag typed `#Foo` is the only sample that can tell
    /// them apart, and an account may not contain one.
    pub standardized_is_lowercased: bool,
}

pub(super) fn hashtag_text_relation(display: &str, standardized: &str) -> HashtagTextRelation {
    let sans = display.strip_prefix('#').unwrap_or(display);
    HashtagTextRelation {
        display_has_hash: display.starts_with('#'),
        standardized_is_display_sans_hash: standardized == sans,
        standardized_is_lowercased: standardized == sans.to_lowercase(),
    }
}

/// A `recordName`'s SHAPE, so a create knows what to mint without this report
/// carrying an identifier.
pub(super) fn record_name_shape(name: &str) -> &'static str {
    let uuid_like = name.len() == 36
        && name.chars().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() }
        });
    if !uuid_like {
        return "not-uuid-shaped";
    }
    let has_upper = name.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = name.chars().any(|c| c.is_ascii_lowercase());
    match (has_upper, has_lower) {
        (true, false) => "UUID-uppercase",
        (false, true) => "uuid-lowercase",
        (true, true) => "uuid-mixed-case",
        // All digits and dashes — a real UUID, just no letters in this one.
        (false, false) => "uuid-lowercase",
    }
}

/// **What a tag is, as the set of invariants a create would have to
/// reproduce** — the half of the question that reading can answer.
///
/// `tag_records` above says which FIELDS exist. It cannot say what links a
/// hashtag to the note that carries it, and that linkage is most of the work:
/// three records and one protobuf field have to agree, and if a create gets
/// one of them wrong, CloudKit accepts the write and Apple's clients show
/// nothing — the same failure mode as the `Folders` omission (gotcha #22).
///
/// Every line is a CHECKED COUNT, never a value. If a count comes back short
/// of its denominator, the invariant is not one and the create cannot assume
/// it — which is the answer either way, and the reason this is worth running
/// before a capture rather than after.
fn tag_anatomy(records: &[Value], out: &mut String) {
    const HASHTAG_UTI: &str = "com.apple.notes.inlinetextattachment.hashtag";
    let _ = writeln!(out, "\n  ── the relations a create must reproduce (no values printed) ──");

    // Live records only. A tombstone keeps the fields it had when it died, so
    // counting one would price a rule against a record no client will read —
    // the same trap the pin census fell into (gotcha #23).
    let live = |r: &&Value| !wire::is_deleted_record(r) && r["deleted"] != json!(true);

    // Kept as RECORDS, not as decoded pairs. The first run's `filter_map`
    // dropped anything whose text did not decode, so "the field is not UTF-8"
    // and "the field decoded and disagrees" produced the same 0/n and could
    // not be told apart.
    let hashtag_recs: Vec<&Value> = records
        .iter()
        .filter(|r| r["recordType"] == json!("Hashtag"))
        .filter(live)
        .collect();
    let attach_recs: Vec<&Value> = records
        .iter()
        .filter(|r| r["recordType"] == json!("InlineAttachment"))
        .filter(live)
        .filter(|r| {
            wire::decode_text_field(&r["fields"]["UTIEncrypted"]).as_deref() == Some(HASHTAG_UTI)
        })
        .collect();

    // The decoded view, for the records where decoding worked at all.
    let hashtags: HashMap<&str, (String, String)> = hashtag_recs
        .iter()
        .filter_map(|r| {
            let name = r["recordName"].as_str()?;
            let display = wire::decode_text_field(&r["fields"]["DisplayTextEncrypted"])?;
            let std = wire::decode_text_field(&r["fields"]["StandardizedContentEncrypted"])?;
            Some((name, (display, std)))
        })
        .collect();

    let note_names: std::collections::HashSet<&str> = records
        .iter()
        .filter(|r| matches!(r["recordType"].as_str(), Some("Note") | Some("PasswordProtectedNote")))
        .filter_map(|r| r["recordName"].as_str())
        .collect();

    // recordName → (token, altText, owning note)
    let attachments: HashMap<&str, (Option<String>, Option<String>, Option<&str>)> = attach_recs
        .iter()
        .filter_map(|r| {
            let name = r["recordName"].as_str()?;
            Some((
                name,
                (
                    wire::decode_text_field(&r["fields"]["TokenContentIdentifierEncrypted"]),
                    wire::decode_text_field(&r["fields"]["AltTextEncrypted"]),
                    r["fields"]["Note"]["value"]["recordName"].as_str(),
                ),
            ))
        })
        .collect();

    if hashtag_recs.is_empty() && attach_recs.is_empty() {
        let _ = writeln!(
            out,
            "    no live Hashtag or hashtag InlineAttachment on this account — tag a note in \
             Apple Notes and re-run, or every line below is vacuous."
        );
        return;
    }

    // ── what these fields ARE, before asking what they mean ──────────────
    let shapes_of = |label: &str, recs: &[&Value], field: &str, out: &mut String| {
        let mut h: HashMap<String, usize> = HashMap::new();
        for r in recs {
            *h.entry(field_shape(&r["fields"][field])).or_default() += 1;
        }
        let _ = writeln!(out, "      {label:<32}{}", histogram(&h));
    };
    let _ = writeln!(out, "    field shapes (classes, never contents):");
    shapes_of("Hashtag.DisplayText", &hashtag_recs, "DisplayTextEncrypted", out);
    shapes_of("Hashtag.StandardizedContent", &hashtag_recs, "StandardizedContentEncrypted", out);
    shapes_of("InlineAttachment.AltText", &attach_recs, "AltTextEncrypted", out);
    shapes_of(
        "InlineAttachment.TokenContentId",
        &attach_recs,
        "TokenContentIdentifierEncrypted",
        out,
    );

    // ── Hashtag, on its own terms ────────────────────────────────────────
    let mut rel_hash = 0usize;
    let mut rel_sans = 0usize;
    let mut rel_lower = 0usize;
    let mut same_len = 0usize;
    let mut one_shorter = 0usize;
    let mut std_eq_display_ci = 0usize;
    let mut shapes: HashMap<&str, usize> = HashMap::new();
    for (name, (display, std)) in &hashtags {
        let rel = hashtag_text_relation(display, std);
        rel_hash += usize::from(rel.display_has_hash);
        rel_sans += usize::from(rel.standardized_is_display_sans_hash);
        rel_lower += usize::from(rel.standardized_is_lowercased);
        same_len += usize::from(std.chars().count() == display.chars().count());
        one_shorter += usize::from(std.chars().count() + 1 == display.chars().count());
        std_eq_display_ci += usize::from(eq_ci(std, display));
        *shapes.entry(record_name_shape(name)).or_default() += 1;
    }
    let h = hashtags.len();
    let _ = writeln!(out, "    Hashtag ({h} decoded of {} live):", hashtag_recs.len());
    let _ = writeln!(out, "      DisplayText starts with '#'                     {rel_hash}/{h}");
    let _ = writeln!(out, "      Standardized == DisplayText minus that '#'      {rel_sans}/{h}");
    let _ = writeln!(out, "      Standardized == the same, lowercased            {rel_lower}/{h}");
    let _ = writeln!(out, "      Standardized == DisplayText ignoring case       {std_eq_display_ci}/{h}");
    let _ = writeln!(out, "      same character count as DisplayText             {same_len}/{h}");
    let _ = writeln!(out, "      exactly one character shorter                   {one_shorter}/{h}");
    let _ = writeln!(out, "      recordName shape: {}", histogram(&shapes));
    let _ = writeln!(
        out,
        "      MinimumSupportedNotesVersion (live only): {}",
        version_histogram(&hashtag_recs)
    );

    // ── InlineAttachment → Hashtag, and → Note ───────────────────────────
    //
    // Every candidate linkage is tried side by side rather than one being
    // assumed. The first run tested ONE (token == recordName, case-sensitive)
    // and reported its failure as though the other candidates had also been
    // ruled out.
    let a = attach_recs.len();
    let mut token_decoded = 0usize;
    let mut tok_is_record = 0usize;
    let mut tok_is_record_ci = 0usize;
    let mut tok_is_standardized = 0usize;
    let mut tok_is_display = 0usize;
    let mut alt_decoded = 0usize;
    let mut alt_is_any_display = 0usize;
    let mut alt_is_any_standardized = 0usize;
    let mut note_resolves = 0usize;
    let mut a_shapes: HashMap<&str, usize> = HashMap::new();
    for (name, (token, alt, note)) in &attachments {
        *a_shapes.entry(record_name_shape(name)).or_default() += 1;
        if let Some(t) = token.as_deref() {
            token_decoded += 1;
            tok_is_record += usize::from(hashtags.contains_key(t));
            tok_is_record_ci += usize::from(hashtags.keys().any(|k| eq_ci(k, t)));
            tok_is_standardized += usize::from(hashtags.values().any(|(_, s)| eq_ci(s, t)));
            tok_is_display += usize::from(hashtags.values().any(|(d, _)| eq_ci(d, t)));
        }
        if let Some(x) = alt.as_deref() {
            alt_decoded += 1;
            alt_is_any_display += usize::from(hashtags.values().any(|(d, _)| d == x));
            alt_is_any_standardized += usize::from(hashtags.values().any(|(_, s)| s == x));
        }
        if note.is_some_and(|n| note_names.contains(n)) {
            note_resolves += 1;
        }
    }
    let _ = writeln!(out, "    InlineAttachment, hashtag UTI ({a} live):");
    let _ = writeln!(out, "      TokenContentId decoded as text                  {token_decoded}/{a}");
    let _ = writeln!(out, "      …IS a Hashtag recordName                        {tok_is_record}/{token_decoded}");
    let _ = writeln!(out, "      …IS one ignoring case (gotcha #18)              {tok_is_record_ci}/{token_decoded}");
    let _ = writeln!(out, "      …IS some Hashtag's StandardizedContent          {tok_is_standardized}/{token_decoded}");
    let _ = writeln!(out, "      …IS some Hashtag's DisplayText                  {tok_is_display}/{token_decoded}");
    let _ = writeln!(out, "      AltText decoded as text                         {alt_decoded}/{a}");
    let _ = writeln!(out, "      …equals SOME Hashtag's DisplayText              {alt_is_any_display}/{alt_decoded}");
    let _ = writeln!(out, "      …equals SOME Hashtag's StandardizedContent      {alt_is_any_standardized}/{alt_decoded}");
    let _ = writeln!(out, "      Note reference resolves in this zone            {note_resolves}/{a}");
    let _ = writeln!(out, "      recordName shape: {}", histogram(&a_shapes));
    let _ = writeln!(
        out,
        "      MinimumSupportedNotesVersion (live only): {}",
        version_histogram(&attach_recs)
    );
    // Reuse is a DESIGN fact, not a statistic: if one Hashtag serves several
    // notes, a create must look for an existing one before minting, or the
    // account grows a duplicate tag per note. Counted over whatever the token
    // is, since that is the grouping key whatever it turns out to mean.
    let distinct_tags: std::collections::HashSet<&str> =
        attachments.values().filter_map(|(t, _, _)| t.as_deref()).collect();
    let _ = writeln!(
        out,
        "      {a} attachment(s) carry {} distinct token(s) — reuse is {}",
        distinct_tags.len(),
        if distinct_tags.len() < a { "REAL: a create must find-or-mint" } else { "not evidenced here" }
    );

    // ── the note document → InlineAttachment ─────────────────────────────
    //
    // Only the notes an attachment actually names are parsed. The whole-zone
    // pass above already costs one decode per note; scoping this to the
    // handful that carry a tag keeps the section from doubling the census.
    let owning: std::collections::HashSet<&str> =
        attachments.values().filter_map(|(_, _, n)| *n).collect();
    let (mut runs, mut id_hits, mut owner_agrees, mut len_one, mut at_fffc) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut sys_class, mut sys_data) = (0usize, 0usize);
    for n in records.iter().filter(|r| {
        r["recordType"] == json!("Note")
            && r["recordName"].as_str().is_some_and(|x| owning.contains(x))
    }) {
        let Some(b64) = n["fields"]["TextDataEncrypted"]["value"].as_str() else { continue };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else { continue };
        let Ok(parsed) = compose::parse(&bytes) else { continue };
        let units: Vec<u16> = parsed.text().encode_utf16().collect();
        let mut offset = 0usize;
        for r in &parsed.string.attribute_run {
            let start = offset;
            offset += r.length as usize;
            let Some(info) = &r.attachment_info else { continue };
            if info.type_uti.as_deref() != Some(HASHTAG_UTI) {
                continue;
            }
            runs += 1;
            if info.system_attachment_class_name.is_some() {
                sys_class += 1;
            }
            if info.system_attachment_data.is_some() {
                sys_data += 1;
            }
            if r.length == 1 {
                len_one += 1;
            }
            if units.get(start) == Some(&0xFFFC) {
                at_fffc += 1;
            }
            let Some(id) = info.attachment_identifier.as_deref() else { continue };
            let Some((_, _, note_of_attachment)) = attachments.get(id) else { continue };
            id_hits += 1;
            if *note_of_attachment == n["recordName"].as_str() {
                owner_agrees += 1;
            }
        }
    }
    let _ = writeln!(out, "    note document -> InlineAttachment ({runs} hashtag run(s)):");
    let _ = writeln!(out, "      attachmentIdentifier IS an InlineAttachment     {id_hits}/{runs}");
    let _ = writeln!(out, "      that attachment's Note == the note it sits in   {owner_agrees}/{runs}");
    let _ = writeln!(out, "      run length == 1                                 {len_one}/{runs}");
    let _ = writeln!(out, "      the character it covers is U+FFFC               {at_fffc}/{runs}");
    let _ = writeln!(out, "      systemAttachmentClassName present               {sys_class}/{runs}");
    let _ = writeln!(out, "      systemAttachmentData present                    {sys_data}/{runs}");
    let _ = writeln!(
        out,
        "\n    A denominator not met is the finding: that invariant is not one, and a\n    \
         create may not assume it."
    );
}

/// A write must CARRY this number, never choose one — the same rule the
/// wrapper versions follow, and for the same reason: an invented value is how
/// a record becomes unreadable to an older client.
///
/// Takes the already-filtered records rather than re-selecting by type: the
/// first run counted tombstones here while every line beside it counted live
/// records, so `7=11` sat under a heading that said 9. Two denominators in one
/// block is a reading error waiting to happen.
fn version_histogram(records: &[&Value]) -> String {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for r in records {
        let v = &r["fields"]["MinimumSupportedNotesVersion"]["value"];
        if v.is_null() {
            continue;
        }
        *counts.entry(v.to_string()).or_default() += 1;
    }
    let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if rows.is_empty() {
        return "none carried".into();
    }
    rows.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join("  ")
}

fn tag_records(records: &[Value], out: &mut String) {
    let _ = writeln!(out, "\n── M3: what a tag is made of (for the deferred create) ──");

    for kind in ["Hashtag", "InlineAttachment"] {
        let of_kind: Vec<&Value> =
            records.iter().filter(|r| r["recordType"] == json!(kind)).collect();
        if of_kind.is_empty() {
            let _ = writeln!(
                out,
                "  {kind}: none on this account — tag a note in Apple Notes and re-run, \
                 or this stays unanswered."
            );
            continue;
        }
        let mut fields: HashMap<String, usize> = HashMap::new();
        let mut deleted = 0usize;
        for r in &of_kind {
            if wire::is_deleted_record(r) {
                deleted += 1;
            }
            if let Some(obj) = r["fields"].as_object() {
                for k in obj.keys() {
                    *fields.entry(k.clone()).or_default() += 1;
                }
            }
        }
        let mut rows: Vec<(String, usize)> = fields.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let _ = writeln!(
            out,
            "  {kind}: {} record(s), {deleted} tombstoned; fields: {}",
            of_kind.len(),
            rows.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join("  ")
        );
        // A field carried by SOME and not others is the interesting one —
        // it is either optional on create or set later, and both matter.
        let universal: Vec<&str> =
            rows.iter().filter(|(_, n)| *n == of_kind.len()).map(|(k, _)| k.as_str()).collect();
        let partial: Vec<&str> =
            rows.iter().filter(|(_, n)| *n != of_kind.len()).map(|(k, _)| k.as_str()).collect();
        let _ = writeln!(
            out,
            "    on every record: {}\n    on only some:    {}",
            if universal.is_empty() { "none".into() } else { universal.join(", ") },
            if partial.is_empty() { "none".into() } else { partial.join(", ") },
        );
    }
    let _ = writeln!(
        out,
        "\n  This says what Apple STORES; the section below turns it into what a \
         create must SEND. What neither can settle is the `records/modify` \
         envelope — whether the three records go in one operation, and what \
         reference `action` the Note takes — which is the same kind of question \
         the Folders field was, and has the same answer: conform to icloud-md's \
         captured shape rather than guess."
    );
}

/// **Where a folder actually hangs, as opposed to where its path says it does.**
///
/// M1's `path_for` gives a folder with NO `ParentFolder` and a folder whose
/// parent is the root the same path — `Notes/<title>` — which was right for a
/// milestone that only displayed the tree. The first folder Jodd created was
/// therefore placed by inference from a path, and Apple Notes showed it one
/// level below the sibling it was meant to sit beside (2026-08-24, live).
///
/// So the distinction is real on this account, and this line says how the
/// account is actually shaped: if top-level folders carry no parent, a writer
/// that always names the root is wrong for every one of them. Names and
/// counts, never a title.
fn folder_parentage(records: &[Value], out: &mut String) {
    use std::fmt::Write as _;
    let folders: Vec<&Value> = records
        .iter()
        .filter(|r| r["recordType"] == json!("Folder"))
        .filter(|r| !wire::is_deleted_record(r))
        .collect();
    let (mut absent, mut root, mut other) = (0usize, 0usize, 0usize);
    for f in &folders {
        match wire::parent_folder_of(f).as_deref() {
            None => absent += 1,
            Some(wire::DEFAULT_FOLDER) => root += 1,
            Some(_) => other += 1,
        }
    }
    let _ = writeln!(out, "\n── M2: where does a folder actually hang? ──");
    let _ = writeln!(
        out,
        "  {} folder(s) — ParentFolder absent: {absent}  the account root: {root}  \
         another folder: {other}",
        folders.len()
    );
    let _ = writeln!(
        out,
        "  Absent and root are the SAME path in Jodd's tree and different places in\n\
         \x20 Apple Notes, so a folder write must copy the field off a folder already\n\
         \x20 in the right place rather than derive it from a path."
    );

    // **Which fields a Folder record carries at all — because one of them may
    // mean "this is a smart folder".**
    //
    // Notes.app offers a smart folder at creation time AND can convert an
    // ordinary folder into one, so whatever marks it is a mutable field on a
    // record of this same type. Nothing in Jodd distinguishes them: a smart
    // folder is a saved QUERY, so it holds no notes of its own, and the read
    // path would render it as an ordinary empty folder. That is cosmetic while
    // this backend only reads. It stops being cosmetic the moment a write
    // targets one — filing a note into a query folder, or a rename disturbing
    // whatever field carries the predicate.
    //
    // So: names and counts of every field seen on a Folder, and nothing else.
    // The marker is whichever name appears on some folders and not others once
    // the account has one; on an account with none, this line is the baseline
    // that makes the difference visible.
    let mut fields: HashMap<String, usize> = HashMap::new();
    for f in &folders {
        if let Some(obj) = f["fields"].as_object() {
            for k in obj.keys() {
                *fields.entry(k.clone()).or_default() += 1;
            }
        }
    }
    let mut rows: Vec<(&String, &usize)> = fields.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let _ = writeln!(
        out,
        "  fields on a Folder record: {}",
        rows.iter().map(|(k, c)| format!("{k}={c}")).collect::<Vec<_>>().join("  ")
    );
    let _ = writeln!(
        out,
        "  A field carried by SOME folders and not others is the candidate for the\n\
         \x20 smart-folder marker — Notes.app can make one at creation and convert an\n\
         \x20 ordinary folder into one, so it is a mutable field on this same record\n\
         \x20 type. Jodd cannot tell them apart yet, which is cosmetic on a read-only\n\
         \x20 backend and is not once a write can target one."
    );
}

/// **Can Jodd write these notes?**
///
/// The two lines that decide the most:
///
/// - **byte-for-byte re-encode.** The first write guard is "reproduce the
///   remote's current form exactly from your own model, or refuse to edit it".
///   If prost does not reproduce Apple's encoder, that guard refuses
///   everything and M2's design needs rethinking rather than tuning.
/// - **run coverage, in three candidate units.** The attribute-run splice is
///   length arithmetic, and `AttributeRun.length` being UTF-16 code units is an
///   inference from `NSAttributedString`, not a measurement. Whichever column
///   is ~100% is the answer; a split result means the assumption is wrong and
///   the splice must not ship.
fn write_readiness(
    notes: &[&Value],
    inline_refs: &HashMap<String, wire::InlineRef>,
    out: &mut String,
) {
    let _ = writeln!(out, "\n── M2: can Jodd write these notes? ──");

    let (mut total, mut no_body, mut undecodable) = (0usize, 0usize, 0usize);
    let mut round_trips = 0usize;
    // **Counted apart, and the first census got this wrong.** `substring` is
    // per-character CRDT identity — `CharID` ranges a splice would have to mint
    // for a replica Jodd is not. `timestamp` is a document-level version
    // vector. The write gate ORs them into one refusal, so the first run
    // reported "776 carry CRDT identity" and could not say whether the write
    // path is dead or nearly fine. That is the same defect the refusals
    // themselves are kept apart to avoid, committed inside the instrument.
    let (mut with_substrings, mut with_timestamp) = (0usize, 0usize);
    let mut substring_max = 0usize;
    // Where a failed re-encode actually differs — the outer wrapper or the
    // inner string. One is a field this code omits; the other is the model
    // being incomplete.
    let (mut differs_in_wrapper, mut differs_in_string) = (0usize, 0usize);
    let (mut cover_utf16, mut cover_chars, mut cover_bytes, mut cover_none) =
        (0usize, 0usize, 0usize, 0usize);
    let (mut with_objects, mut layers_round_trip) = (0usize, 0usize);
    let mut writable = 0usize;
    // Why the rest are not. The DISTRIBUTION is the finding: it says which
    // refusal M3 should relax first, and a single "unsupported" count could not.
    let mut refusals: HashMap<String, usize> = HashMap::new();
    let (mut runs_min, mut runs_max, mut runs_total) = (usize::MAX, 0usize, 0usize);
    // Which run FIELDS occur at all — names only, so M3 knows what interpreting
    // them would cost before it starts.
    let mut run_fields: HashMap<&str, usize> = HashMap::new();
    // Histograms of opaque values. This module reports what is there, never
    // what it means (gotcha #7).
    let mut styles: HashMap<u32, usize> = HashMap::new();
    let mut hints: HashMap<u32, usize> = HashMap::new();
    // What `versioned_document` versions this account carries. A write must
    // CARRY these, never choose them — an invented number is how a note becomes
    // unreadable on an older iPhone.
    let mut wrapper_versions: HashMap<(Option<u32>, Option<u32>, Option<u32>), usize> =
        HashMap::new();
    // ── M3: the formatting census (spec F7) ──
    let mut format_decodes = 0usize;
    let mut format_refusals: HashMap<String, usize> = HashMap::new();
    let mut paragraph_kinds: HashMap<&'static str, usize> = HashMap::new();

    for n in notes {
        total += 1;
        let title = wire::decode_text_field(&n["fields"]["TitleEncrypted"]).unwrap_or_default();
        let Some(b64) = n["fields"]["TextDataEncrypted"]["value"].as_str() else {
            no_body += 1;
            continue;
        };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
            undecodable += 1;
            continue;
        };
        let Ok(parsed) = compose::parse(&bytes) else {
            undecodable += 1;
            continue;
        };
        if compose::round_trips(&bytes) {
            round_trips += 1;
        } else if let Ok(raw) = compose::decompress(&bytes) {
            if compose::inner_round_trips(&parsed, &raw) {
                differs_in_wrapper += 1;
            } else {
                differs_in_string += 1;
            }
        }
        if !parsed.string.substring.is_empty() {
            with_substrings += 1;
            substring_max = substring_max.max(parsed.string.substring.len());
        }
        if parsed.string.timestamp.is_some() {
            with_timestamp += 1;
        }

        let text = parsed.text();
        let run_total: u64 = parsed.string.attribute_run.iter().map(|r| r.length as u64).sum();
        // Three candidate units for one number. Exactly one of these columns
        // should come back at ~100%.
        if run_total == compose::utf16_len(text) as u64 {
            cover_utf16 += 1;
        } else if run_total == text.chars().count() as u64 {
            cover_chars += 1;
        } else if run_total == text.len() as u64 {
            cover_bytes += 1;
        } else {
            cover_none += 1;
        }

        let count = parsed.string.attribute_run.len();
        runs_min = runs_min.min(count);
        runs_max = runs_max.max(count);
        runs_total += count;
        for r in &parsed.string.attribute_run {
            if let Some(ps) = &r.paragraph_style {
                *run_fields.entry("paragraphStyle").or_default() += 1;
                if let Some(v) = ps.style {
                    *styles.entry(v).or_default() += 1;
                }
                if ps.todo.is_some() {
                    *run_fields.entry("paragraphStyle.todo").or_default() += 1;
                }
            }
            if let Some(v) = r.font_hints {
                *run_fields.entry("fontHints").or_default() += 1;
                *hints.entry(v).or_default() += 1;
            }
            if r.font.is_some() {
                *run_fields.entry("font").or_default() += 1;
            }
            if r.link.is_some() {
                *run_fields.entry("link").or_default() += 1;
            }
            if r.color.is_some() {
                *run_fields.entry("color").or_default() += 1;
            }
            if r.underline.is_some() {
                *run_fields.entry("underline").or_default() += 1;
            }
            if r.strikethrough.is_some() {
                *run_fields.entry("strikethrough").or_default() += 1;
            }
            if r.attachment_info.is_some() || r.system_attachment_info.is_some() {
                *run_fields.entry("attachmentInfo").or_default() += 1;
            }
        }

        if text.contains('\u{FFFC}') {
            with_objects += 1;
        }
        if compose::layers_round_trip(text, &title, &parsed.string.attribute_run, inline_refs) {
            layers_round_trip += 1;
        }
        // M3: does the formatting model cover this note, and what does the
        // account actually use? Names and counts only.
        match super::format::decode_note_format(text, &parsed.string.attribute_run) {
            Ok(paragraphs) => {
                format_decodes += 1;
                for p in &paragraphs {
                    let name = match p.kind {
                        super::format::ParagraphKind::Title => "Title",
                        super::format::ParagraphKind::Heading => "Heading",
                        super::format::ParagraphKind::Subheading => "Subheading",
                        super::format::ParagraphKind::Body => "Body",
                        super::format::ParagraphKind::Monospaced => "Monospaced",
                        super::format::ParagraphKind::BulletList => "BulletList",
                        super::format::ParagraphKind::DashList => "DashList",
                        super::format::ParagraphKind::NumberedList => "NumberedList",
                        super::format::ParagraphKind::TodoList => "TodoList",
                    };
                    *paragraph_kinds.entry(name).or_default() += 1;
                }
            }
            Err(e) => {
                *format_refusals.entry(format!("{e:?}")).or_default() += 1;
            }
        }
        *wrapper_versions
            .entry((
                parsed.serialization_version,
                parsed.version_serialization_version,
                parsed.minimum_supported_version,
            ))
            .or_default() += 1;

        // The SHIPPED gate, run over the account — so a re-run is a regression
        // check of the real function rather than a second implementation of the
        // rule drifting inside a diagnostic.
        match compose::writability_with_refs(&bytes, &title, inline_refs) {
            Ok(_) => writable += 1,
            Err(u) => {
                // The variant's NAME, not its message: the message carries the
                // note's own numbers, which would make every note its own row.
                let key = format!("{u:?}");
                let key = key.split(|c| c == ' ' || c == '{').next().unwrap_or("?").to_string();
                *refusals.entry(key).or_default() += 1;
            }
        }
    }

    let pct = |n: usize| if total == 0 { 0.0 } else { n as f64 * 100.0 / total as f64 };
    let _ = writeln!(
        out,
        "  {total} note(s) examined; {no_body} with no body field, {undecodable} undecodable"
    );
    let _ = writeln!(
        out,
        "  documents re-encode byte-for-byte: {round_trips}/{total} ({:.1}%)",
        pct(round_trips)
    );
    let _ = writeln!(
        out,
        "  substring (per-character CRDT identity): {with_substrings}, max {substring_max} per note"
    );
    let _ = writeln!(out, "  timestamp (document-level version vector): {with_timestamp}");
    let _ = writeln!(
        out,
        "  re-encode mismatch is in — the wrapper: {differs_in_wrapper}  the string: {differs_in_string}"
    );
    let _ = writeln!(
        out,
        "  run coverage — UTF-16: {cover_utf16}  chars: {cover_chars}  bytes: {cover_bytes}  \
         none of the three: {cover_none}"
    );
    if runs_min != usize::MAX {
        let _ = writeln!(
            out,
            "  attribute runs per note: min {runs_min}, max {runs_max}, mean {:.1}",
            runs_total as f64 / total.saturating_sub(no_body + undecodable).max(1) as f64
        );
    }
    let mut fields: Vec<(&str, usize)> = run_fields.into_iter().collect();
    fields.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let _ = writeln!(
        out,
        "  run fields present (runs carrying each): {}",
        if fields.is_empty() {
            "none".to_string()
        } else {
            fields.iter().map(|(k, c)| format!("{k}={c}")).collect::<Vec<_>>().join("  ")
        }
    );
    let _ = writeln!(out, "  paragraphStyle.style values: {}", histogram(&styles));
    let _ = writeln!(out, "  fontHints values: {}", histogram(&hints));
    let _ = writeln!(
        out,
        "  text contains U+FFFC (attachments, tables, inline tags): {with_objects}"
    );
    let _ = writeln!(
        out,
        "  title + HTML layers round-trip: {layers_round_trip}/{total} ({:.1}%)",
        pct(layers_round_trip)
    );
    let mut versions: Vec<((Option<u32>, Option<u32>, Option<u32>), usize)> =
        wrapper_versions.into_iter().collect();
    versions.sort_by(|a, b| b.1.cmp(&a.1));
    let _ = writeln!(
        out,
        "  wrapper versions (doc.ser, ver.ser, ver.min): {}",
        versions
            .iter()
            .map(|((a, b, c), n)| format!("({a:?},{b:?},{c:?})={n}"))
            .collect::<Vec<_>>()
            .join("  ")
    );

    // ── M3: the formatting census (spec F7) ──
    let _ = writeln!(out, "\n  ── M3: does the formatting model cover this account? ──");
    let _ = writeln!(
        out,
        "  FORMAT DECODES: {format_decodes}/{total} ({:.1}%)",
        pct(format_decodes)
    );
    let mut format_rows: Vec<(String, usize)> = format_refusals.into_iter().collect();
    format_rows.sort_by(|a, b| b.1.cmp(&a.1));
    for (why, n) in &format_rows {
        let _ = writeln!(out, "  format refused — {why}: {n}");
    }
    let mut kind_rows: Vec<(&str, usize)> = paragraph_kinds.into_iter().collect();
    kind_rows.sort_by(|a, b| b.1.cmp(&a.1));
    let _ = writeln!(
        out,
        "  paragraph kinds: {}",
        if kind_rows.is_empty() {
            "none".to_string()
        } else {
            kind_rows.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join("  ")
        }
    );
    // The UTI allow-list feed (spec F5): which inline text attachments this
    // account actually carries. UTIs are type names, never content.
    let mut uti_counts: HashMap<&str, usize> = HashMap::new();
    for r in inline_refs.values() {
        *uti_counts.entry(r.type_uti.as_str()).or_default() += 1;
    }
    let mut uti_rows: Vec<(&str, usize)> = uti_counts.into_iter().collect();
    uti_rows.sort_by(|a, b| b.1.cmp(&a.1));
    let _ = writeln!(
        out,
        "  inline attachment records by UTI: {}",
        if uti_rows.is_empty() {
            "none".to_string()
        } else {
            uti_rows.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join("  ")
        }
    );

    let _ = writeln!(out, "\n  ── the shipped gate, run over this account ──");
    let _ = writeln!(out, "  WRITABLE: {writable}/{total} ({:.1}%)", pct(writable));
    let mut rows: Vec<(String, usize)> = refusals.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    for (why, n) in &rows {
        let _ = writeln!(out, "  refused — {why}: {n} ({:.1}%)", pct(*n));
    }
    if rows.is_empty() && total > 0 {
        let _ = writeln!(out, "  (nothing refused)");
    }
    let _ = writeln!(
        out,
        "\n  These are the numbers M3 acts on. A refusal that dominates is the one to\n  \
         relax first; a WRITABLE well under 100% is the milestone's real ceiling, not\n  \
         a rounding error."
    );
}

/// **Where does a note in Recently Deleted come back to?**
///
/// M2 shows the Trash and can restore from it, but a trashed record's `Folder`
/// reference has been replaced by the Trash's — so Jodd cannot tell which
/// folder the note came from, and the restore UI asks the user instead of
/// filing everything in the root. That is the honest behaviour for an unknown;
/// it is not the right behaviour if the answer is sitting on the record.
///
/// **`Folders` (plural) is the standing candidate.** It is in `DESIRED_KEYS`
/// because the web client asks for it, it comes back on real records, and
/// nothing reads it. Apple's own client restores a note to the right place, so
/// the information exists somewhere; this prints what a trashed record actually
/// carries so the question stops being a hypothesis.
///
/// Field NAMES and reference record names only — a record name is a UUID, never
/// content.
fn trash_provenance(records: &[Value], out: &mut String) {
    let _ = writeln!(
        out,
        "\n── M2: what does a trashed note remember about where it lived? ──"
    );

    let trashed: Vec<&Value> = records
        .iter()
        .filter(|r| r["recordType"] == json!("Note"))
        .filter(|r| !wire::is_deleted_record(r))
        .filter(|r| {
            r["fields"]["Folder"]["value"]["recordName"] == json!(wire::TRASH_FOLDER)
        })
        .collect();

    if trashed.is_empty() {
        let _ = writeln!(
            out,
            "  nothing in Recently Deleted on this account — delete a scratch note in\n  \
             Apple Notes and re-run, or this question stays open."
        );
        return;
    }

    let _ = writeln!(out, "  {} note(s) in Recently Deleted", trashed.len());
    let mut fields: HashMap<String, usize> = HashMap::new();
    let mut folders_shape: HashMap<String, usize> = HashMap::new();
    for r in &trashed {
        if let Some(obj) = r["fields"].as_object() {
            for k in obj.keys() {
                *fields.entry(k.clone()).or_default() += 1;
            }
        }
        *folders_shape.entry(describe_reference_field(&r["fields"]["Folders"])).or_default() += 1;
    }

    let mut rows: Vec<(&String, &usize)> = fields.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let _ = writeln!(
        out,
        "  fields present: {}",
        rows.iter().map(|(k, c)| format!("{k}={c}")).collect::<Vec<_>>().join("  ")
    );

    let mut shapes: Vec<(&String, &usize)> = folders_shape.iter().collect();
    shapes.sort_by(|a, b| b.1.cmp(a.1));
    let _ = writeln!(out, "  Folders (plural), by shape:");
    for (shape, n) in shapes {
        let _ = writeln!(out, "    {n} × {shape}");
    }

    // Resolved to paths, because a bare `recordName` cannot be checked against
    // anything. The question this answers is "is that where you deleted it
    // from?", and only the person who deleted it can answer — so the id has to
    // arrive as a place they recognise. Folder paths are already what the
    // walk's own `notes by folder` line prints, so this crosses no line the
    // diagnostics have not already drawn.
    let paths = wire::build_folder_paths(&wire::folder_records(records));
    let named: Vec<String> = trashed
        .iter()
        .filter_map(|r| wire::folders_plural(&r["fields"]["Folders"]))
        .flatten()
        .filter(|id| id != wire::TRASH_FOLDER)
        .map(|id| match paths.get(&id) {
            Some(p) => format!("{p:?}"),
            None => format!("{id} (no live folder by that name)"),
        })
        .collect();
    let _ = writeln!(
        out,
        "  of those, {} name(s) a folder that is NOT the Trash{}",
        named.len(),
        if named.is_empty() { String::new() } else { format!(": {}", named.join(", ")) }
    );
    let _ = writeln!(
        out,
        "\n  `Folders` was the candidate for where a trashed note came from. It is\n  \
         CLOSED, 2026-08-28: the delete OVERWRITES it. `delete_note_body` goes\n  \
         through `folder_relocation_fields(TRASH_FOLDER)`, which sets Folder,\n  \
         Folders=[Trash] and FoldersModificationDate together — and icloud-md's\n  \
         HAR-matched encoder shows Apple's own client doing the same, so a delete\n  \
         that PRESERVED it would be the deviation. An earlier reading that found\n  \
         the origin intact predates the 2026-08-25 Folders fix, when this backend\n  \
         still sent Folder alone and the field survived by omission.\n  \
         A non-Trash entry above would therefore be news; expect none."
    );
}

/// A reference field described by shape and record name, never by value.
fn describe_reference_field(v: &Value) -> String {
    let inner = &v["value"];
    if inner.is_null() {
        return "absent".to_string();
    }
    if let Some(list) = inner.as_array() {
        if list.is_empty() {
            return "empty list".to_string();
        }
        let names: Vec<&str> = list
            .iter()
            .map(|e| e["recordName"].as_str().unwrap_or("<no recordName>"))
            .collect();
        return format!("[{}]", names.join(", "));
    }
    if let Some(name) = inner["recordName"].as_str() {
        return format!("ref {name}");
    }
    "present, unrecognised shape".to_string()
}

/// A `{value: count}` histogram of opaque numbers, biggest first.
///
/// Values, not meanings: this never claims `fontHints = 1` is bold. Reporting
/// the distribution is a measurement; naming it would be gotcha #7's
/// documentation-derived preset, one milestone early.
fn histogram<K: std::fmt::Display + Ord>(m: &HashMap<K, usize>) -> String {
    if m.is_empty() {
        return "none".to_string();
    }
    let mut rows: Vec<(&K, &usize)> = m.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    rows.iter().map(|(v, c)| format!("{v}={c}")).collect::<Vec<_>>().join("  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The M3 formatting section: one note whose format decodes and one whose
    /// style code is unknown, plus an InlineAttachment record feeding the UTI
    /// line — every count named, no note text anywhere.
    #[test]
    fn the_formatting_census_counts_decodes_refusals_kinds_and_utis() {
        use super::super::gen::topotext;
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        let doc_with_style = |text: &str, style: u32| {
            let d = compose::NoteDocument {
                string: topotext::String {
                    string: text.to_string(),
                    substring: Vec::new(),
                    timestamp: None,
                    attribute_run: vec![topotext::AttributeRun {
                        length: compose::utf16_len(text) as u32,
                        paragraph_style: Some(topotext::ParagraphStyle {
                            style: Some(style),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                },
                serialization_version: Some(0),
                version_serialization_version: Some(0),
                minimum_supported_version: Some(0),
                crdt: None,
            };
            base64::engine::general_purpose::STANDARD.encode(compose::encode(&d))
        };
        let records = vec![
            json!({
                "recordName": "n-ok", "recordType": "Note", "recordChangeTag": "t1",
                "fields": {
                    "TitleEncrypted": { "value": b64("T") },
                    "TextDataEncrypted": { "value": doc_with_style("T\nhead", 1) },
                    "Folder": { "value": { "recordName": wire::DEFAULT_FOLDER } },
                }
            }),
            json!({
                "recordName": "n-unknown", "recordType": "Note", "recordChangeTag": "t2",
                "fields": {
                    "TitleEncrypted": { "value": b64("T") },
                    "TextDataEncrypted": { "value": doc_with_style("T\nbody", 77) },
                    "Folder": { "value": { "recordName": wire::DEFAULT_FOLDER } },
                }
            }),
            json!({
                "recordName": "tag-1", "recordType": "InlineAttachment",
                "fields": {
                    "UTIEncrypted": { "value": b64("com.apple.notes.inlinetextattachment.hashtag") },
                    "AltTextEncrypted": { "value": b64("#work") },
                }
            }),
        ];
        let out = report(&records);
        assert!(out.contains("FORMAT DECODES: 1/2"), "{out}");
        assert!(out.contains("format refused — UnknownStyle(77): 1"), "{out}");
        // One whole-text run styled 1 covers both lines, so both paragraphs
        // are Heading — the run-anchored decode, not a per-line guess.
        assert!(out.contains("Heading=2"), "{out}");
        assert!(
            out.contains("inline attachment records by UTI: com.apple.notes.inlinetextattachment.hashtag=1"),
            "{out}"
        );
        assert!(!out.contains("#work"), "an alt text is CONTENT and must never print: {out}");
    }

    /// The deferred tag-create's data half: field NAMES per record type, and
    /// the universal/partial split that says which of them a create might be
    /// allowed to omit. Never a value — an `AltTextEncrypted` is the user's
    /// own tag text.
    #[test]
    fn the_tag_census_reports_field_names_and_never_the_tag_text() {
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        let records = vec![
            json!({ "recordName": "h1", "recordType": "Hashtag", "fields": {
                "NameEncrypted": { "value": b64("#secrettag") },
                "CreationDate": { "value": 1_700_000_000_000i64 },
            }}),
            json!({ "recordName": "h2", "recordType": "Hashtag", "fields": {
                "NameEncrypted": { "value": b64("#other") },
            }}),
            json!({ "recordName": "tag-1", "recordType": "InlineAttachment", "fields": {
                "UTIEncrypted": { "value": b64("com.apple.notes.inlinetextattachment.hashtag") },
                "AltTextEncrypted": { "value": b64("#secrettag") },
            }}),
        ];
        let out = report(&records);
        assert!(out.contains("Hashtag: 2 record(s)"), "{out}");
        assert!(out.contains("InlineAttachment: 1 record(s)"), "{out}");
        // The universal/partial split is the actionable part: CreationDate is
        // on one Hashtag of two, so a create may or may not have to send it.
        assert!(out.contains("on every record: NameEncrypted"), "{out}");
        assert!(out.contains("on only some:    CreationDate"), "{out}");
        assert!(
            !out.contains("secrettag"),
            "a tag's text is the user's content and must never print: {out}"
        );
    }

    /// A tagless account must say so rather than print an empty section that
    /// reads like an answer.
    #[test]
    fn the_tag_census_says_when_the_account_has_none() {
        let out = report(&[json!({ "recordName": "n1", "recordType": "Note", "fields": {} })]);
        assert!(out.contains("Hashtag: none on this account"), "{out}");
        assert!(out.contains("InlineAttachment: none on this account"), "{out}");
    }

    /// The rule every line in here follows, asserted rather than trusted: this
    /// runs against somebody's real Apple ID, and a census that could leak note
    /// text by meeting an unanticipated shape is not runnable at all.
    #[test]
    fn the_census_prints_no_note_text_even_when_the_records_are_full_of_it() {
        let secret = "SUPER-SECRET-NOTE-TEXT";
        let doc_bytes = compose::encode(&compose::NoteDocument::new(&format!(
            "{secret}\n{secret} in the body too"
        )));
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        let records = vec![
            json!({
                "recordName": "n1",
                "recordType": "Note",
                "recordChangeTag": "tag1",
                "fields": {
                    "TitleEncrypted": { "value": b64(secret) },
                    "TextDataEncrypted": {
                        "value": base64::engine::general_purpose::STANDARD.encode(&doc_bytes)
                    },
                    "Folder": { "value": { "recordName": wire::DEFAULT_FOLDER } },
                }
            }),
            json!({
                "recordName": "n2",
                "recordType": "Note",
                "fields": {
                    "TitleEncrypted": { "value": b64(secret) },
                    "TextDataEncrypted": {
                        "value": base64::engine::general_purpose::STANDARD.encode(&doc_bytes)
                    },
                    "Folder": { "value": { "recordName": wire::TRASH_FOLDER } },
                    "Folders": { "value": { "recordName": "f-original" } },
                }
            }),
        ];

        let out = report(&records);
        assert!(!out.contains(secret), "the census leaked note text:\n{out}");
        // And it still answered the questions it exists for.
        assert!(out.contains("WRITABLE: 1/1"), "{out}");
        assert!(out.contains("1 note(s) in Recently Deleted"), "{out}");
        assert!(out.contains("ref f-original"), "the Folders hypothesis must be reported:\n{out}");
    }

    /// A trashed note is not one the user can edit, so it must not move the
    /// number the write census exists to report.
    #[test]
    fn a_note_in_the_trash_is_not_counted_as_writable_or_unwritable() {
        let doc_bytes = compose::encode(&compose::NoteDocument::new("T\nbody"));
        let rec = |folder: &str| {
            json!({
                "recordName": "x",
                "recordType": "Note",
                "fields": {
                    "TitleEncrypted": {
                        "value": base64::engine::general_purpose::STANDARD.encode("T")
                    },
                    "TextDataEncrypted": {
                        "value": base64::engine::general_purpose::STANDARD.encode(&doc_bytes)
                    },
                    "Folder": { "value": { "recordName": folder } },
                }
            })
        };
        let out = report(&[rec(wire::DEFAULT_FOLDER), rec(wire::TRASH_FOLDER)]);
        assert!(out.contains("1 note(s) examined"), "{out}");
    }

    #[test]
    fn an_empty_trash_says_so_rather_than_reporting_nothing() {
        let out = report(&[]);
        assert!(out.contains("nothing in Recently Deleted"), "{out}");
    }

    /// The two flags are kept apart because a lowercase tag satisfies both and
    /// therefore proves neither. Only a tag with an uppercase letter can tell
    /// "strip the #" from "strip the # and lowercase it" — so an account with
    /// no such tag must report the ambiguity rather than resolve it, which is
    /// what counting them separately does.
    #[test]
    fn a_lowercase_tag_cannot_tell_the_two_derivations_apart() {
        let both = hashtag_text_relation("#thai", "thai");
        assert!(both.display_has_hash);
        assert!(both.standardized_is_display_sans_hash);
        assert!(both.standardized_is_lowercased);

        // The sample that discriminates: only the lowercasing rule survives.
        let discriminating = hashtag_text_relation("#TagByApple", "tagbyapple");
        assert!(discriminating.display_has_hash);
        assert!(!discriminating.standardized_is_display_sans_hash);
        assert!(discriminating.standardized_is_lowercased);
    }

    /// A tag with no `#` is not a malformed record to fail on — it is a shape
    /// this account carries, and the create has to know it exists.
    #[test]
    fn a_display_text_without_a_hash_is_reported_not_rejected() {
        let rel = hashtag_text_relation("bare", "bare");
        assert!(!rel.display_has_hash);
        assert!(rel.standardized_is_display_sans_hash);
    }

    #[test]
    fn record_name_shape_says_case_because_a_create_has_to_mint_one() {
        assert_eq!(record_name_shape("f8bf619a-1b84-40eb-932d-6318ee9aeeb4"), "uuid-lowercase");
        assert_eq!(record_name_shape("F8BF619A-1B84-40EB-932D-6318EE9AEEB4"), "UUID-uppercase");
        assert_eq!(record_name_shape("TrashFolder-CloudKit"), "not-uuid-shaped");
    }

    /// **The leak guard above does not cover the anatomy section, and that is
    /// why this exists.** Its fixture spells the field `NameEncrypted`, which
    /// no real account carries — the live shape is
    /// `DisplayTextEncrypted`/`StandardizedContentEncrypted` — so the anatomy
    /// pass drops those records and prints its "no live Hashtag" line instead.
    /// A guard that returns early on its own fixture is green without checking
    /// anything, which is the failure this repo already has a rule about.
    #[test]
    fn the_anatomy_section_reports_relations_and_never_the_tag_text() {
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        let secret = "SecretTag";
        let records = vec![
            json!({ "recordName": "f8bf619a-1b84-40eb-932d-6318ee9aeeb4",
                    "recordType": "Hashtag", "fields": {
                "DisplayTextEncrypted": { "value": b64(&format!("#{secret}")) },
                "StandardizedContentEncrypted": { "value": b64(&secret.to_lowercase()) },
                "MinimumSupportedNotesVersion": { "value": 0 },
            }}),
            json!({ "recordName": "aaaaaaaa-1b84-40eb-932d-6318ee9aeeb4",
                    "recordType": "InlineAttachment", "fields": {
                "UTIEncrypted": { "value": b64("com.apple.notes.inlinetextattachment.hashtag") },
                "AltTextEncrypted": { "value": b64(&format!("#{secret}")) },
                "TokenContentIdentifierEncrypted": {
                    "value": b64("f8bf619a-1b84-40eb-932d-6318ee9aeeb4") },
                "Note": { "value": { "recordName": "n1" } },
                "MinimumSupportedNotesVersion": { "value": 0 },
            }}),
            json!({ "recordName": "n1", "recordType": "Note", "fields": {
                "TitleEncrypted": { "value": b64("t") },
                "Folder": { "value": { "recordName": wire::DEFAULT_FOLDER } },
            }}),
        ];
        let out = report(&records);

        // The section actually ran on this fixture — not the early-out line.
        assert!(!out.contains("no live Hashtag or hashtag InlineAttachment"), "{out}");
        // The relations a create must reproduce, resolved rather than printed.
        assert!(out.contains("DisplayText starts with '#'                     1/1"), "{out}");
        assert!(out.contains("Standardized == the same, lowercased            1/1"), "{out}");
        assert!(out.contains("…IS a Hashtag recordName                        1/1"), "{out}");
        assert!(out.contains("…equals SOME Hashtag's DisplayText              1/1"), "{out}");
        assert!(out.contains("Note reference resolves in this zone            1/1"), "{out}");
        assert!(out.contains("uuid-lowercase=1"), "{out}");
        // The shape line describes the field without quoting it: this tag is
        // "#SecretTag", so leading#, upper and lower must all be reported.
        assert!(out.contains("Hashtag.DisplayText"), "{out}");
        assert!(out.contains("utf8:leading#+upper+lower"), "{out}");

        // …and the tag's own text never appears, in either case.
        assert!(!out.contains(secret), "a tag's text is the user's content: {out}");
        assert!(!out.contains(&secret.to_lowercase()), "{out}");
    }

    /// One Hashtag serving several notes is a DESIGN fact, not a statistic: a
    /// create has to find-or-mint, and minting unconditionally would grow a
    /// duplicate tag per note. So the report must name it when it sees it.
    #[test]
    fn reuse_of_one_hashtag_across_notes_is_called_out() {
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
        let attach = |name: &str, note: &str| {
            json!({ "recordName": name, "recordType": "InlineAttachment", "fields": {
                "UTIEncrypted": { "value": b64("com.apple.notes.inlinetextattachment.hashtag") },
                "AltTextEncrypted": { "value": b64("#t") },
                "TokenContentIdentifierEncrypted": { "value": b64("h1") },
                "Note": { "value": { "recordName": note } },
            }})
        };
        let records = vec![
            json!({ "recordName": "h1", "recordType": "Hashtag", "fields": {
                "DisplayTextEncrypted": { "value": b64("#t") },
                "StandardizedContentEncrypted": { "value": b64("t") },
            }}),
            attach("a1", "n1"),
            attach("a2", "n2"),
        ];
        let out = report(&records);
        assert!(out.contains("2 attachment(s) carry 1 distinct token(s)"), "{out}");
        assert!(out.contains("REAL: a create must find-or-mint"), "{out}");
    }

    /// Vacuous counts read as passing invariants. An account with no tag must
    /// say it has none, or "0/0" everywhere looks like agreement.
    #[test]
    fn an_account_with_no_tags_says_so_rather_than_printing_zeroes() {
        let out = report(&[]);
        assert!(out.contains("no live Hashtag or hashtag InlineAttachment"), "{out}");
    }
}
