//! Note provenance (M1): who wrote a note and whether a human reviewed it.
//! Spec: docs/superpowers/specs/2026-10-07-note-provenance-design.md.
//! Types and the tier rules live here; `db.rs` calls the SQL helpers below
//! inside its own transactions, so an event and its content commit together.

use serde::Serialize;

pub const COALESCE_MS: i64 = 600_000;

/// The writer of an event. `as_okf` is the OKF v0.2 actor string (SPEC §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    Human,
    Agent { client: String, version: String },
    Workflow { name: String, model: String },
    Process(&'static str),
}

/// One OKF actor-string segment. `:` is replaced too, so no agent or workflow
/// string can carry a `human:` / `process:` prefix (OKF consumers key off it).
fn segment(s: &str, fallback: &str) -> String {
    let t: String = s.trim().chars().map(|c| if c == '/' || c == ':' || c.is_whitespace() { '-' } else { c }).collect();
    if t.is_empty() { fallback.to_string() } else { t }
}

impl Actor {
    pub fn agent(client: &str, version: &str) -> Actor {
        Actor::Agent { client: segment(client, "agent"), version: segment(version, "unknown") }
    }
    /// A Jodd LLM workflow run with `account`'s effective provider.
    pub fn workflow(name: &str, account: &crate::accounts::Account) -> Actor {
        Actor::Workflow { name: segment(name, "workflow"), model: model_label(account) }
    }
    /// For tests only: a non-human actor, so fixtures never look reviewed.
    pub fn test() -> Actor { Actor::Process("test") }
    pub fn as_okf(&self) -> String {
        match self {
            Actor::Human => "human:owner".into(),
            Actor::Agent { client, version } => format!("{client}/{version}"),
            Actor::Workflow { name, model } => format!("jodd-{name}/{model}"),
            Actor::Process(name) => format!("process:jodd-{name}"),
        }
    }
    pub fn is_human(&self) -> bool { matches!(self, Actor::Human) }
}

/// The model part of a workflow actor. No wildcard arm (gotcha #18's discipline).
pub fn model_label(account: &crate::accounts::Account) -> String {
    use crate::accounts::LlmProviderKind;
    let Some(eff) = crate::llm::resolve::effective_for_account(account) else { return "unknown".into() };
    let raw = match eff.llm.provider {
        LlmProviderKind::Http => eff.llm.http_model.clone().unwrap_or_else(|| "http".into()),
        LlmProviderKind::AgentCli => eff.llm.agent_preset.clone().unwrap_or_else(|| "agent-cli".into()),
        LlmProviderKind::ClaudeCode => "claude-code-cli".into(),
        LlmProviderKind::None | LlmProviderKind::Disabled => "unknown".into(),
    };
    segment(&raw, "unknown")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind { Create, Edit, Remote, Move, Verify }

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Create => "create", EventKind::Edit => "edit", EventKind::Remote => "remote",
            EventKind::Move => "move", EventKind::Verify => "verify",
        }
    }
    pub fn is_content(self) -> bool { matches!(self, EventKind::Create | EventKind::Edit | EventKind::Remote) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event { pub id: i64, pub kind: String, pub actor: Option<String>, pub at: i64, pub local_version: Option<i64> }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier { NotRecorded, Unreviewed, HumanReviewed, HumanWritten }

/// Kind of the marker row R11 inserts when a write left no event. Not an
/// `EventKind`: nothing records it on purpose, and as an unknown kind it already
/// counts as non-human content (rule 2a), so the gap outlives later events.
pub const UNRECORDED_KIND: &str = "unrecorded";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Trust {
    pub tier: Tier,
    /// OKF `generated`: the latest content event, whoever wrote it.
    pub by: Option<String>,
    pub at: Option<String>,
    /// The latest NON-HUMAN content event after the last verify — the thing that
    /// is unreviewed, and what Mark reviewed certifies. `None` when nothing is.
    pub event_id: Option<i64>,
    /// That event's actor; `None` for a `remote` change or an unrecorded write.
    pub unreviewed_by: Option<String>,
    pub unreviewed_at: Option<String>,
    pub created_by: Option<String>,
    pub unrecorded_change: bool,
    pub note_local_version: i64,
}

fn is_content_kind(k: &str) -> bool { !matches!(k, "move" | "verify") } // unknown kinds count as content
/// Exact match, never a prefix test: an actor string that merely starts with
/// `human:` (an older build, a hostile client name) is not the human.
fn is_human_actor(a: &Option<String>) -> bool { a.as_deref() == Some(Actor::Human.as_okf().as_str()) }
/// Shared with `Db::verify_note` so "unseen agent content" and "unreviewed" can never drift apart.
pub(crate) fn non_human_content(e: &Event) -> bool {
    is_content_kind(&e.kind) && (e.kind == "remote" || !matches!(e.kind.as_str(), "create" | "edit") || !is_human_actor(&e.actor))
}

fn iso(ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Rule 2b: the note's version is past every version an event recorded.
pub(crate) fn unrecorded_gap(events: &[Event], note_local_version: i64) -> bool {
    events.iter().filter_map(|e| e.local_version).max().is_some_and(|m| note_local_version > m)
}

/// Events after the last human `verify` (all of them when there is none).
pub(crate) fn after_last_verify(events: &[Event]) -> &[Event] {
    let last_verify = events.iter().rposition(|e| e.kind == "verify" && is_human_actor(&e.actor));
    &events[last_verify.map_or(0, |i| i + 1)..]
}

/// The §3.6 rules, first match wins. `events` must be sorted by `id` (M9: the
/// wall clock can step back; ids cannot).
pub fn derive(events: &[Event], note_local_version: i64) -> Trust {
    let latest = events.iter().filter(|e| is_content_kind(&e.kind)).last();
    let gap = unrecorded_gap(events, note_local_version);
    let last_verify = events.iter().rposition(|e| e.kind == "verify" && is_human_actor(&e.actor));
    let after_verify = after_last_verify(events);
    let pending = after_verify.iter().filter(|e| non_human_content(e)).last();
    // Rule 2b, or the unreviewed thing is a gap an earlier write pinned (R11).
    let unrecorded_change = gap || pending.is_some_and(|e| e.kind == UNRECORDED_KIND);
    let tier = if events.is_empty() {
        Tier::NotRecorded
    } else if pending.is_some() || gap {
        Tier::Unreviewed
    } else if last_verify.is_some() {
        Tier::HumanReviewed
    } else if events.iter().any(|e| e.kind == "create")
        && events.iter().filter(|e| is_content_kind(&e.kind)).all(|e| !non_human_content(e))
    {
        Tier::HumanWritten
    } else {
        Tier::NotRecorded
    };
    // Under rule 2b the newest write is unknown: name no writer at all.
    // `event_id` still names the pending event: Mark reviewed must be able to
    // certify it together with the gap the chip showed.
    let hide_writer = gap;
    let shown = pending.filter(|_| !hide_writer);
    Trust {
        tier,
        by: if hide_writer { None } else { latest.and_then(|e| e.actor.clone()) },
        at: if hide_writer { None } else { latest.and_then(|e| iso(e.at)) },
        event_id: pending.map(|e| e.id),
        unreviewed_by: shown.filter(|e| e.kind != "remote" && e.kind != UNRECORDED_KIND).and_then(|e| e.actor.clone()),
        unreviewed_at: shown.and_then(|e| iso(e.at)),
        created_by: events.iter().find(|e| e.kind == "create").and_then(|e| e.actor.clone()),
        unrecorded_change,
        note_local_version,
    }
}

fn normalized_text(html: &str) -> String {
    crate::agent_memory::text::html_to_text(html, true).split_whitespace().collect::<Vec<_>>().join(" ")
}

// Test-only count of text comparisons on THIS thread. Thread-local rather than a
// process-wide atomic: tests run in parallel, and another test's `same_content`
// would otherwise bump a shared counter between a test's before/after reads.
#[cfg(test)]
thread_local! {
    pub static TEXT_COMPARISONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// §3.5: title plus normalized text. Callers check byte equality first.
pub fn same_content(old_title: &str, old_html: &str, new_title: &str, new_html: &str) -> bool {
    #[cfg(test)]
    TEXT_COMPARISONS.with(|c| c.set(c.get() + 1));
    old_title.trim() == new_title.trim() && normalized_text(old_html) == normalized_text(new_html)
}

pub const NOTE_EVENTS_DDL: &str = "CREATE TABLE note_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        uuid TEXT NOT NULL,
        account_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        actor TEXT,
        at INTEGER NOT NULL,
        local_version INTEGER
    );
    CREATE INDEX idx_note_events_note ON note_events (account_id, uuid, at);";

/// Append an event, or fold it into the note's latest one when both are
/// `create`/`edit` events by the same actor of the same kind inside
/// `COALESCE_MS` (a burst of autosaves is one edit). Coalescing refreshes `at`
/// AND `local_version`, so the event keeps pointing at the version it describes.
/// `remote`, `move` and `verify` never coalesce: a remote change never advances
/// `local_version`, so a folded remote row would hide a second remote change
/// from `verify_note`'s stale check.
///
/// `prior_local_version` is the note's `local_version` read BEFORE this write,
/// in the same transaction (`None` for a new note). If it is past every version
/// an event recorded, something wrote the note without an event; an
/// `unrecorded` marker pinned at that version is inserted first (R11), so the
/// gap survives this and every later event until a human `verify`.
pub fn record_event_conn(conn: &rusqlite::Connection, account_id: &str, uuid: &str, kind: EventKind,
    actor: Option<&Actor>, local_version: Option<i64>, prior_local_version: Option<i64>) -> rusqlite::Result<()> {
    record_event_at(conn, account_id, uuid, kind, actor, local_version, prior_local_version, crate::db::now_ms())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_event_at(conn: &rusqlite::Connection, account_id: &str, uuid: &str, kind: EventKind,
    actor: Option<&Actor>, local_version: Option<i64>, prior_local_version: Option<i64>, now: i64) -> rusqlite::Result<()> {
    use rusqlite::{params, OptionalExtension};
    let actor_s = actor.map(|a| a.as_okf());
    if kind != EventKind::Verify {
        if let Some(prior) = prior_local_version {
            let (n, max_lv): (i64, Option<i64>) = conn.query_row(
                "SELECT COUNT(*), MAX(local_version) FROM note_events WHERE account_id = ?1 AND uuid = ?2",
                params![account_id, uuid], |r| Ok((r.get(0)?, r.get(1)?)))?;
            // Same test as rule 2b (`unrecorded_gap`): no history, or only copied
            // rows (NULL versions), is not a gap.
            if n > 0 && max_lv.is_some_and(|m| prior > m) {
                append_event_conn(conn, account_id, uuid, UNRECORDED_KIND, None, Some(prior), now)?;
            }
        }
    }
    if matches!(kind, EventKind::Create | EventKind::Edit) {
        // Newest by id, never by `at`: the clock can step back (M9).
        let last: Option<(i64, String, Option<String>, i64)> = conn.query_row(
            "SELECT id, kind, actor, at FROM note_events WHERE account_id = ?1 AND uuid = ?2
             ORDER BY id DESC LIMIT 1",
            params![account_id, uuid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
        if let Some((id, k, a, at)) = last {
            if k == kind.as_str() && a == actor_s && (now - at).abs() < COALESCE_MS {
                conn.execute("UPDATE note_events SET at = ?1, local_version = ?2 WHERE id = ?3",
                    params![now, local_version, id])?;
                return Ok(());
            }
        }
    }
    append_event_conn(conn, account_id, uuid, kind.as_str(), actor_s.as_deref(), local_version, now)
}

/// A plain INSERT: no coalescing, no gap check. For the marker, and for a
/// duplicate's own first event (which must not fold into a copied row).
pub(crate) fn append_event_conn(conn: &rusqlite::Connection, account_id: &str, uuid: &str, kind: &str,
    actor: Option<&str>, local_version: Option<i64>, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO note_events (uuid, account_id, kind, actor, at, local_version) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![uuid, account_id, kind, actor, now, local_version])?;
    Ok(())
}

/// Events for many notes, one statement per 500 uuids (SQLite's parameter
/// limit), each note's list sorted by `id` as `derive` requires. Ids are the
/// true order: coalescing only touches the newest row and a copy inserts in
/// order, while `at` can go backwards with the clock (M9).
pub fn events_for_conn(conn: &rusqlite::Connection, account_id: &str, uuids: &[String])
    -> rusqlite::Result<std::collections::HashMap<String, Vec<Event>>> {
    let mut out: std::collections::HashMap<String, Vec<Event>> = Default::default();
    for chunk in uuids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT uuid, id, kind, actor, at, local_version FROM note_events
             WHERE account_id = ?1 AND uuid IN ({marks}) ORDER BY uuid, id");
        let mut stmt = conn.prepare(&sql)?;
        let mut args: Vec<&dyn rusqlite::ToSql> = vec![&account_id];
        for u in chunk { args.push(u); }
        let rows = stmt.query_map(args.as_slice(), |r| Ok((r.get::<_, String>(0)?, Event {
            id: r.get(1)?, kind: r.get(2)?, actor: r.get(3)?, at: r.get(4)?, local_version: r.get(5)?,
        })))?;
        for row in rows { let (u, e) = row?; out.entry(u).or_default().push(e); }
    }
    Ok(out)
}

/// Copy `from_uuid`'s history onto `to_uuid` (a conflict copy inherits it).
/// `local_version` is NULLed: the copy has its own version counter, and a
/// version pinned to the source's would read as an unrecorded change.
pub fn copy_events_conn(conn: &rusqlite::Connection, account_id: &str, from_uuid: &str, to_uuid: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO note_events (uuid, account_id, kind, actor, at, local_version)
         SELECT ?3, account_id, kind, actor, at, NULL FROM note_events
         WHERE account_id = ?1 AND uuid = ?2 ORDER BY id",
        rusqlite::params![account_id, from_uuid, to_uuid])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: i64, kind: &str, actor: Option<&str>, at: i64, lv: Option<i64>) -> Event {
        Event { id, kind: kind.into(), actor: actor.map(Into::into), at, local_version: lv }
    }
    const AGENT: Option<&str> = Some("claude-code/2.1.0");
    const HUMAN: Option<&str> = Some("human:owner");

    #[test]
    fn actor_strings_are_okf() {
        assert_eq!(Actor::Human.as_okf(), "human:owner");
        assert_eq!(Actor::agent("claude-code", "2.1.0").as_okf(), "claude-code/2.1.0");
        assert_eq!(Actor::agent(" ", "").as_okf(), "agent/unknown");
        assert_eq!(Actor::agent("my client/x", "1 0").as_okf(), "my-client-x/1-0");
        assert_eq!(Actor::agent("human:owner", "1").as_okf(), "human-owner/1");
        assert_eq!(Actor::Workflow { name: "extract".into(), model: "m".into() }.as_okf(), "jodd-extract/m");
        assert_eq!(Actor::Process("conflict-copy").as_okf(), "process:jodd-conflict-copy");
        assert!(Actor::Human.is_human());
        assert!(!Actor::test().is_human());
    }

    #[test]
    fn tier_rules_in_order() {
        // rule 1
        assert_eq!(derive(&[], 5).tier, Tier::NotRecorded);
        // rule 2a: agent create, never verified
        assert_eq!(derive(&[ev(1, "create", AGENT, 10, Some(1))], 1).tier, Tier::Unreviewed);
        // rule 2a: remote change after verify
        let t = derive(&[ev(1, "create", AGENT, 10, Some(1)), ev(2, "verify", HUMAN, 20, Some(1)), ev(3, "remote", None, 30, Some(1))], 1);
        assert_eq!(t.tier, Tier::Unreviewed);
        assert_eq!(t.by, None);
        // rule 2a: unknown kind counts as non-human content
        assert_eq!(derive(&[ev(1, "create", HUMAN, 10, Some(1)), ev(2, "teleport", HUMAN, 20, Some(1))], 1).tier, Tier::Unreviewed);
        // rule 2b: local_version advanced past every recorded version
        let t = derive(&[ev(1, "create", HUMAN, 10, Some(1))], 2);
        assert_eq!(t.tier, Tier::Unreviewed);
        assert!(t.unrecorded_change);
        assert_eq!(t.by, None);
        // rule 3: verified, human edit afterwards keeps it reviewed
        assert_eq!(derive(&[ev(1, "create", AGENT, 10, Some(1)), ev(2, "verify", HUMAN, 20, Some(1)), ev(3, "edit", HUMAN, 30, Some(2))], 2).tier, Tier::HumanReviewed);
        // rule 3: pre-existing note (no content events) the user verified
        assert_eq!(derive(&[ev(1, "verify", HUMAN, 20, Some(7))], 7).tier, Tier::HumanReviewed);
        // rule 4
        assert_eq!(derive(&[ev(1, "create", HUMAN, 10, Some(1)), ev(2, "edit", HUMAN, 20, Some(2))], 2).tier, Tier::HumanWritten);
        // rule 5: pre-existing note, only human edits recorded
        assert_eq!(derive(&[ev(1, "edit", HUMAN, 20, Some(9))], 9).tier, Tier::NotRecorded);
        // move is not content
        assert_eq!(derive(&[ev(1, "create", HUMAN, 10, Some(1)), ev(2, "move", AGENT, 20, Some(2))], 2).tier, Tier::HumanWritten);
    }

    #[test]
    fn only_the_exact_human_actor_is_human() {
        // an older build could have recorded an agent as "human:owner/1"
        let t = derive(&[ev(1, "create", Some("human:owner/1"), 10, Some(1))], 1);
        assert_eq!(t.tier, Tier::Unreviewed);
        // and such a string cannot certify a note either
        let t = derive(&[ev(1, "create", AGENT, 10, Some(1)), ev(2, "verify", Some("human:owner/1"), 20, Some(1))], 1);
        assert_eq!(t.tier, Tier::Unreviewed);
    }

    #[test]
    fn equal_at_is_ordered_by_id() {
        // verify (id 3) and agent edit (id 2) share `at`; id order says the edit came first.
        let t = derive(&[ev(1, "create", AGENT, 10, Some(1)), ev(2, "edit", AGENT, 20, Some(2)), ev(3, "verify", HUMAN, 20, Some(2))], 2);
        assert_eq!(t.tier, Tier::HumanReviewed);
    }

    #[test]
    fn display_fields() {
        let t = derive(&[ev(4, "create", AGENT, 1_759_800_000_000, Some(1)), ev(5, "edit", HUMAN, 1_759_800_100_000, Some(2))], 2);
        assert_eq!(t.by.as_deref(), Some("human:owner"));
        // R14: event_id is the unreviewed (agent) event, not the latest content event
        assert_eq!(t.event_id, Some(4));
        assert_eq!(t.unreviewed_by.as_deref(), Some("claude-code/2.1.0"));
        assert_eq!(t.created_by.as_deref(), Some("claude-code/2.1.0"));
        assert_eq!(t.at.as_deref(), Some("2025-10-07T01:21:40Z"));
        assert_eq!(t.note_local_version, 2);
    }

    #[test]
    fn copied_rows_do_not_count_for_rule_2b() {
        // a duplicate inherits rows with NULL local_version, then its own create
        let t = derive(&[ev(1, "create", AGENT, 10, None), ev(2, "create", HUMAN, 30, Some(1))], 1);
        assert_eq!(t.tier, Tier::Unreviewed); // inherited agent content, never verified
        assert!(!t.unrecorded_change);
    }

    #[test]
    fn same_content_ignores_markup_and_whitespace_only() {
        assert!(same_content("T", "<div>a  b</div><div>c</div>", "T", "<p>a b</p>\n<p>c</p>"));
        assert!(!same_content("T", "<div>a</div>", "T", "<div>b</div>"));
        assert!(!same_content("T", "<div>a</div>", "U", "<div>a</div>"));
        assert!(same_content("T", "<div><b>a</b></div>", "T", "<div>a</div>")); // formatting-only: accepted blind spot
    }

    fn mem() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(super::NOTE_EVENTS_DDL).unwrap();
        c
    }

    #[test]
    fn coalesces_same_actor_and_kind_within_the_window_only() {
        let c = mem();
        let a = Actor::agent("cc", "1");
        record_event_at(&c, "acct", "U", EventKind::Edit, Some(&a), Some(1), None, 1_000).unwrap();
        record_event_at(&c, "acct", "U", EventKind::Edit, Some(&a), Some(2), None, 1_000 + COALESCE_MS - 1).unwrap();
        let ev = &events_for_conn(&c, "acct", &["U".into()]).unwrap()["U"];
        assert_eq!(ev.len(), 1, "coalesced");
        assert_eq!(ev[0].local_version, Some(2), "coalescing must carry local_version too (rule 2b)");
        record_event_at(&c, "acct", "U", EventKind::Edit, Some(&a), Some(3), None, 1_000 + 2 * COALESCE_MS).unwrap();
        record_event_at(&c, "acct", "U", EventKind::Edit, Some(&Actor::Human), Some(4), None, 1_000 + 2 * COALESCE_MS + 1).unwrap();
        record_event_at(&c, "acct", "U", EventKind::Verify, Some(&Actor::Human), Some(4), None, 1_000 + 2 * COALESCE_MS + 2).unwrap();
        record_event_at(&c, "acct", "U", EventKind::Verify, Some(&Actor::Human), Some(4), None, 1_000 + 2 * COALESCE_MS + 3).unwrap();
        assert_eq!(events_for_conn(&c, "acct", &["U".into()]).unwrap()["U"].len(), 5, "window, actor and verify each break coalescing");
    }

    #[test]
    fn remote_events_never_coalesce() {
        let c = mem();
        record_event_at(&c, "acct", "U", EventKind::Remote, None, Some(1), None, 1_000).unwrap();
        record_event_at(&c, "acct", "U", EventKind::Remote, None, Some(1), None, 1_001).unwrap();
        let ev = &events_for_conn(&c, "acct", &["U".into()]).unwrap()["U"];
        assert_eq!(ev.len(), 2, "a second remote change must stay visible to verify's stale check");
    }

    #[test]
    fn copy_nulls_local_version() {
        let c = mem();
        record_event_at(&c, "acct", "SRC", EventKind::Create, Some(&Actor::test()), Some(3), None, 10).unwrap();
        copy_events_conn(&c, "acct", "SRC", "DST").unwrap();
        let ev = &events_for_conn(&c, "acct", &["DST".into()]).unwrap()["DST"];
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].local_version, None);
        assert_eq!(ev[0].actor.as_deref(), Some("process:jodd-test"));
    }

    // M9: wall-clock order can fail open when the clock steps back; id order cannot.
    #[test]
    fn clock_stepping_back_cannot_hide_an_agent_edit_behind_a_verify() {
        let c = mem();
        let a = Actor::agent("cc", "1");
        record_event_at(&c, "acct", "U", EventKind::Create, Some(&a), Some(1), None, 1_000).unwrap();
        record_event_at(&c, "acct", "U", EventKind::Verify, Some(&Actor::Human), Some(1), None, 5_000).unwrap();
        // the clock stepped back between the verify and this agent edit
        record_event_at(&c, "acct", "U", EventKind::Edit, Some(&a), Some(2), Some(1), 4_000).unwrap();
        let ev = &events_for_conn(&c, "acct", &["U".into()]).unwrap()["U"];
        assert_eq!(ev.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), vec!["create", "verify", "edit"]);
        assert_eq!(derive(ev, 2).tier, Tier::Unreviewed);
    }

    #[test]
    fn coalescing_looks_at_the_newest_row_by_id_not_by_at() {
        let c = mem();
        let a = Actor::agent("cc", "1");
        record_event_at(&c, "acct", "U", EventKind::Edit, Some(&a), Some(1), None, 5_000).unwrap();
        record_event_at(&c, "acct", "U", EventKind::Verify, Some(&Actor::Human), Some(1), None, 4_000).unwrap();
        // newest by `at` would be the agent edit (5_000) and this would fold into it, behind the verify
        record_event_at(&c, "acct", "U", EventKind::Edit, Some(&a), Some(2), Some(1), 4_001).unwrap();
        let ev = &events_for_conn(&c, "acct", &["U".into()]).unwrap()["U"];
        assert_eq!(ev.len(), 3);
        assert_eq!(derive(ev, 2).tier, Tier::Unreviewed);
    }

    // I1 / R11: a write that left no event is pinned by a marker before the next event.
    #[test]
    fn a_gap_before_any_recorded_event_leaves_an_unrecorded_marker() {
        let c = mem();
        record_event_at(&c, "acct", "U", EventKind::Create, Some(&Actor::Human), Some(1), None, 1_000).unwrap();
        // a stale binary wrote v2 without an event; the user then moves the note (v2 -> v3)
        record_event_at(&c, "acct", "U", EventKind::Move, Some(&Actor::Human), Some(3), Some(2), 2_000).unwrap();
        let ev = &events_for_conn(&c, "acct", &["U".into()]).unwrap()["U"];
        assert_eq!(ev.iter().map(|e| (e.kind.as_str(), e.actor.clone(), e.local_version)).collect::<Vec<_>>(),
            vec![("create", HUMAN.map(Into::into), Some(1)), ("unrecorded", None, Some(2)), ("move", HUMAN.map(Into::into), Some(3))]);
        let t = derive(ev, 3);
        assert_eq!(t.tier, Tier::Unreviewed, "the later move must not erase the gap");
        assert!(t.unrecorded_change, "the chip still says 'changed without a record'");
        assert_eq!(t.unreviewed_by, None);
        assert_eq!(t.event_id, Some(ev[1].id), "Mark reviewed certifies the marker");
    }

    #[test]
    fn no_marker_without_a_gap_without_history_or_for_a_verify() {
        let c = mem();
        // no history: a pre-existing note's first event
        record_event_at(&c, "acct", "U", EventKind::Edit, Some(&Actor::Human), Some(8), Some(7), 1_000).unwrap();
        // no gap
        record_event_at(&c, "acct", "U", EventKind::Move, Some(&Actor::Human), Some(9), Some(8), 2_000).unwrap();
        // a verify closes a gap itself (it records the version the user saw)
        record_event_at(&c, "acct", "U", EventKind::Verify, Some(&Actor::Human), Some(12), Some(12), 3_000).unwrap();
        let ev = &events_for_conn(&c, "acct", &["U".into()]).unwrap()["U"];
        assert!(ev.iter().all(|e| e.kind != "unrecorded"), "{ev:?}");
    }

    // I4 / R14: the chip names who wrote what is unreviewed, not the latest writer.
    #[test]
    fn unreviewed_by_names_the_workflow_not_the_users_later_typo_fix() {
        let wf = Some("jodd-extract/m");
        let t = derive(&[ev(1, "create", wf, 10, Some(1)), ev(2, "edit", HUMAN, 20, Some(2))], 2);
        assert_eq!(t.tier, Tier::Unreviewed);
        assert_eq!(t.unreviewed_by.as_deref(), Some("jodd-extract/m"));
        assert_eq!(t.unreviewed_at, iso(10));
        assert_eq!(t.event_id, Some(1), "event_id is what Mark reviewed certifies");
        assert_eq!(t.by.as_deref(), Some("human:owner"), "by/at stay OKF `generated`");
    }

    #[test]
    fn a_remote_change_then_a_human_edit_is_changed_elsewhere() {
        let t = derive(&[ev(1, "create", HUMAN, 10, Some(1)), ev(2, "remote", None, 20, Some(1)), ev(3, "edit", HUMAN, 30, Some(2))], 2);
        assert_eq!(t.tier, Tier::Unreviewed);
        assert_eq!(t.unreviewed_by, None);
        assert!(!t.unrecorded_change);
        assert_eq!(t.event_id, Some(2));
        assert_eq!(t.unreviewed_at, iso(20));
    }

    #[test]
    fn no_unreviewed_writer_when_nothing_is_unreviewed() {
        let t = derive(&[ev(1, "create", AGENT, 10, Some(1)), ev(2, "verify", HUMAN, 20, Some(1)), ev(3, "edit", HUMAN, 30, Some(2))], 2);
        assert_eq!(t.tier, Tier::HumanReviewed);
        assert_eq!((t.unreviewed_by, t.unreviewed_at, t.event_id), (None, None, None));
    }

    #[test]
    fn batch_read_handles_more_than_one_chunk() {
        let c = mem();
        let uuids: Vec<String> = (0..1200).map(|i| format!("U{i}")).collect();
        for (i, u) in uuids.iter().enumerate() {
            record_event_at(&c, "acct", u, EventKind::Create, Some(&Actor::Human), Some(1), None, i as i64).unwrap();
        }
        assert_eq!(events_for_conn(&c, "acct", &uuids).unwrap().len(), 1200);
    }
}

#[cfg(test)]
mod echo_tests {
    use super::same_content;
    const BODIES: &[&str] = &[
        "<div>one</div><div>two</div>", "<div><b>bold body</b></div>", "",
        "one plain line of body text", "<div>มี note ใน TEST TEST L2</div>",
        "<div>a &amp; b</div>", "<div>#tag</div><div>x</div>", "<p>a paragraph</p>",
    ];

    #[test]
    fn gmail_and_localfs_title_round_trip_is_not_a_change() {
        // mime822::round_trips (mime822.rs:705) is the model; LocalFs decodes the same MIME.
        for b in BODIES {
            let back = crate::mime822::strip_leading_title(&crate::mime822::inject_title_into_body(b, "Title"), "Title");
            assert!(same_content("Title", b, "Title", &back), "gmail echo changed {b:?} → {back:?}");
        }
    }

    #[test]
    fn localfs_eml_round_trip_is_not_a_change() {
        // model: backend/localfs/decode.rs build_then_decode_roundtrips_envelope
        for b in BODIES {
            let raw = crate::mime822::build_note_mime("Title", &format!("<html><body>{b}</body></html>"), "AAAA-BBBB",
                "Thu, 4 Jun 2026 01:19:50 +0700", "Mon, 1 Jan 2024 09:00:00 +0700", "u@x.com", &[]);
            let d = crate::backend::localfs::decode::decode_eml(raw.as_bytes(), "Notes").unwrap();
            assert!(same_content("Title", b, &d.title, &d.body_html), "localfs echo changed {b:?} → {:?}", d.body_html);
        }
    }

    #[test]
    fn microsoft_round_trip_is_not_a_change() {
        // model: backend/microsoft/wire.rs round_trips_through_the_stripper
        use crate::backend::microsoft::wire::{inject_title_into_body_ms, strip_leading_title_ms};
        for b in BODIES {
            let back = strip_leading_title_ms(&inject_title_into_body_ms("Title", b), "Title");
            assert!(same_content("Title", b, "Title", &back), "microsoft echo changed {b:?} → {back:?}");
        }
    }

    #[test]
    fn ssh_markdown_round_trip_is_not_a_change() {
        // model: backend/ssh/mdconv.rs every_editor_fixture_round_trips_through_markdown
        use crate::backend::ssh::mdconv::{html_to_md, md_to_html};
        let mut ran = 0;
        for b in BODIES {
            // A body Markdown cannot hold is written as .html verbatim (spec A3) — no echo to test.
            if let Some(md) = html_to_md(b) {
                let back = md_to_html(&md);
                assert!(same_content("Title", b, "Title", &back), "ssh echo changed {b:?} → {back:?}");
                ran += 1;
            }
        }
        // If html_to_md ever refused every fixture this test would pass having compared nothing.
        assert!(ran > 0, "no fixture took the Markdown path");
    }

    #[test]
    fn icloud_text_round_trip_is_not_a_change() {
        // model: backend/icloud/compose.rs (the plain_ok branch of layers_round_trip):
        // editor HTML → document text → HTML as pulled.
        use crate::backend::icloud::{compose::html_to_text, doc::text_to_html};
        for b in BODIES {
            let back = text_to_html(&html_to_text(b));
            assert!(same_content("Title", b, "Title", &back), "icloud echo changed {b:?} → {back:?}");
        }
    }
}
