//! The view engine (spec 2026-11-19-properties-tasks-views §8.4): one place
//! runs a parsed [`ViewQuery`] against `notes`, `note_props`, `task_items`,
//! `note_tags` and the FTS index. The Tauri commands and the MCP tools call
//! [`Db::run_view`]; neither re-implements filtering.

use super::query::{
    parse_view, Anchor, Cmp, DateSpec, Show, Sort, ValueTest, ViewError, ViewQuery,
};
use crate::db::{Db, IS_A_VIEW_SQL, NOT_A_VIEW_SQL};
use crate::folder_scope;
use chrono::{Datelike, Days, NaiveDate, NaiveTime, TimeZone};
use rusqlite::types::Value;
use rusqlite::{params, Connection, Result as SqlResult};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ViewDef {
    pub uuid: String,
    pub name: String,
    /// The folder the view note sits in, so a scoped reader (MCP) can leave
    /// out views in hidden folders.
    pub label: String,
    /// The property block, one `Key: value` line each.
    pub lines: Vec<String>,
    /// Why the view does not run; empty when it does.
    pub errors: Vec<ViewError>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NoteRow {
    pub uuid: String,
    pub title: String,
    pub label: String,
    pub modified_ms: i64,
    /// Open (unticked) task rows in the note.
    pub task_count: u32,
    /// Earliest due date among the open task rows.
    pub earliest_due: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskRow {
    pub uuid: String,
    pub title: String,
    pub label: String,
    /// The `list_tasks` / `set_task_state` index.
    pub idx: usize,
    pub text: String,
    pub done: bool,
    pub due: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ViewRows {
    Notes(Vec<NoteRow>),
    Tasks(Vec<TaskRow>),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ViewResult {
    pub rows: ViewRows,
    /// Things the owner should know about an empty or short result.
    pub notices: Vec<String>,
}

/// A note's last modification, in epoch ms: the remote's time, or the
/// local one for a note never pushed — the note list's own sort key
/// (`Db::NEWEST_FIRST`), so a view orders and filters as the list shows.
const MODIFIED_SQL: &str = "COALESCE(n.last_remote_modified_at, n.last_local_modified_at)";

/// The account's live notes (`n`, bound as `?1`). `deleted_pending` is the
/// only local trash; a note trashed remotely is gone from `notes`. Pair it
/// with `NOT_A_VIEW_SQL` wherever rows are results.
const LIVE_SQL: &str = "n.account_id = ?1 AND n.sync_state != 'deleted_pending'";

/// Inclusive day bounds; `None` leaves that side open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DayRange {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
}

/// `spec` as days, against `today`; `None` for [`DateSpec::None`] (no date).
/// A range that runs off the calendar is left open on that side.
fn day_range(spec: DateSpec, today: NaiveDate) -> Option<DayRange> {
    let range = |from, to| Some(DayRange { from, to });
    match spec {
        DateSpec::None => None,
        DateSpec::Today => range(Some(today), Some(today)),
        DateSpec::ThisWeek => {
            let monday = today - Days::new(u64::from(today.weekday().num_days_from_monday()));
            range(Some(monday), monday.checked_add_days(Days::new(6)))
        }
        DateSpec::NextDays(n) => range(Some(today), today.checked_add_days(Days::new(n.into()))),
        DateSpec::LastDays(n) => range(today.checked_sub_days(Days::new(n.into())), Some(today)),
        DateSpec::On(day) => range(Some(day), Some(day)),
        DateSpec::Cmp(cmp, anchor) => {
            let at = match anchor {
                Anchor::Today => today,
                Anchor::Date(day) => day,
            };
            // `pred_opt`/`succ_opt` fail only at chrono's calendar ends,
            // which no `YYYY-MM-DD` date or device clock reaches.
            match cmp {
                Cmp::Lt => range(None, Some(at.pred_opt().unwrap_or(at))),
                Cmp::Le => range(None, Some(at)),
                Cmp::Gt => range(Some(at.succ_opt().unwrap_or(at)), None),
                Cmp::Ge => range(Some(at), None),
            }
        }
    }
}

/// Epoch ms of `day`'s first instant on this device's clock. A day whose
/// midnight a DST change skips starts at its first hour that exists.
fn day_start_ms(day: NaiveDate) -> i64 {
    (0..24)
        .find_map(|h| {
            let at = day.and_time(NaiveTime::from_hms_opt(h, 0, 0)?);
            chrono::Local.from_local_datetime(&at).earliest()
        })
        .map_or_else(
            || day.and_time(NaiveTime::MIN).and_utc().timestamp_millis(),
            |t| t.timestamp_millis(),
        )
}

/// A `WHERE` clause built from fixed fragments; every value the view supplies
/// goes in through [`Where::bind`] as a `?n` placeholder, never into the text.
struct Where {
    sql: String,
    binds: Vec<Value>,
}

impl Where {
    /// `?1` is the account.
    fn new(account_id: &str) -> Self {
        Where {
            sql: format!("{LIVE_SQL} AND {NOT_A_VIEW_SQL}"),
            binds: vec![Value::Text(account_id.to_string())],
        }
    }

    fn bind(&mut self, v: impl Into<Value>) -> String {
        self.binds.push(v.into());
        format!("?{}", self.binds.len())
    }

    fn and(&mut self, fragment: String) {
        self.sql.push_str("\n AND ");
        self.sql.push_str(&fragment);
    }

    /// `col` (ISO `YYYY-MM-DD` text) inside `r`; `"1"` when both sides are open.
    fn iso_in(&mut self, col: &str, r: DayRange) -> String {
        let mut parts = Vec::new();
        if let Some(from) = r.from {
            parts.push(format!("{col} >= {}", self.bind(from.to_string())));
        }
        if let Some(to) = r.to {
            parts.push(format!("{col} <= {}", self.bind(to.to_string())));
        }
        if parts.is_empty() {
            "1".into()
        } else {
            parts.join(" AND ")
        }
    }

    /// `col` (epoch ms) inside `r`'s local days.
    fn ms_in(&mut self, col: &str, r: DayRange) -> String {
        let mut parts = Vec::new();
        if let Some(from) = r.from {
            parts.push(format!("{col} >= {}", self.bind(day_start_ms(from))));
        }
        if let Some(after) = r.to.and_then(|to| to.succ_opt()) {
            parts.push(format!("{col} < {}", self.bind(day_start_ms(after))));
        }
        if parts.is_empty() {
            "1".into()
        } else {
            parts.join(" AND ")
        }
    }

    /// Any of the alternatives; each alternative is already negated as asked.
    /// None at all matches nothing (the parser never sends an empty line).
    fn any_of(alternatives: Vec<String>) -> String {
        if alternatives.is_empty() {
            return "0".into();
        }
        format!("({})", alternatives.join(" OR "))
    }

    /// The conditions on the note itself, common to both `Show`s.
    fn note_conditions(&mut self, q: &ViewQuery, today: NaiveDate) {
        for &pinned in &q.pinned {
            let b = self.bind(i64::from(pinned));
            self.and(format!("n.pinned = {b}"));
        }
        for &spec in &q.modified {
            // The parser refuses `Modified: none`; were one to arrive, it
            // matches nothing rather than everything.
            let cond = match day_range(spec, today) {
                Some(r) => self.ms_in(MODIFIED_SQL, r),
                None => "0".into(),
            };
            self.and(cond);
        }
        for group in &q.tags {
            let alts = group
                .iter()
                .map(|(negated, tag)| {
                    // The stored form (`db::normalize_tag`); a tag with nothing
                    // left after normalizing matches no note.
                    let tag = crate::db::normalize_tag(tag).unwrap_or_default();
                    let exists = format!(
                        "EXISTS (SELECT 1 FROM note_tags t WHERE t.account_id = n.account_id
                                 AND t.uuid = n.uuid AND t.tag = {})",
                        self.bind(tag)
                    );
                    negate(*negated, exists)
                })
                .collect();
            let cond = Self::any_of(alts);
            self.and(cond);
        }
        for text in &q.text {
            let cond = if text.chars().count() < 3 {
                // Trigrams cannot match fewer than 3 characters, so a LIKE
                // scan — over the index's own text, not `body_html`, whose
                // markup (`checkbox`, `div`) would match short words.
                let like = format!(
                    "%{}%",
                    text.replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                );
                let b = self.bind(like);
                format!(
                    "(n.uuid, n.account_id) IN (SELECT uuid, account_id FROM notes_fts
                       WHERE title LIKE {b} ESCAPE '\\' OR body LIKE {b} ESCAPE '\\')"
                )
            } else {
                // `Db::search_notes`'s phrase match over title and body. Its
                // any-word fallback widens a search that found nothing, which
                // a filter must never do, so it is not used here.
                let phrase = format!("{{title body}} : {}", crate::db::fts_phrase(text));
                format!(
                    "(n.uuid, n.account_id) IN (SELECT uuid, account_id FROM notes_fts WHERE notes_fts MATCH {})",
                    self.bind(phrase)
                )
            };
            self.and(cond);
        }
        for clause in &q.props {
            let alts = clause
                .any_of
                .iter()
                .map(|(negated, test)| {
                    let key = self.bind(clause.key.clone());
                    let has = |cond: &str| {
                        format!(
                            "EXISTS (SELECT 1 FROM note_props p WHERE p.account_id = n.account_id
                                     AND p.uuid = n.uuid AND p.key = {key} AND {cond})"
                        )
                    };
                    match test {
                        // ASCII case folding only: SQLite's NOCASE.
                        ValueTest::Exact(v) => {
                            let v = self.bind(v.clone());
                            negate(*negated, has(&format!("p.value = {v} COLLATE NOCASE")))
                        }
                        ValueTest::Date(spec) => match day_range(*spec, today) {
                            // `none`: no dated value for the key.
                            None => negate(!*negated, has("p.value_date IS NOT NULL")),
                            // A value that is not a date matches neither the
                            // test nor its negation.
                            Some(r) => {
                                let inside = self.iso_in("p.value_date", r);
                                let cond = if *negated {
                                    format!("p.value_date IS NOT NULL AND NOT ({inside})")
                                } else {
                                    format!("p.value_date IS NOT NULL AND {inside}")
                                };
                                has(&cond)
                            }
                        },
                    }
                })
                .collect();
            let cond = Self::any_of(alts);
            self.and(cond);
        }
    }

    /// `Done` and `Due` on a task row (`Show: tasks` only — under
    /// `Show: notes` the parser files them as properties).
    fn task_conditions(&mut self, q: &ViewQuery, today: NaiveDate) {
        for &done in &q.done {
            let b = self.bind(i64::from(done));
            self.and(format!("t.done = {b}"));
        }
        for group in &q.due {
            let mut alts = Vec::new();
            for &spec in group {
                alts.push(match day_range(spec, today) {
                    None => "t.due IS NULL".to_string(),
                    Some(r) => {
                        let inside = self.iso_in("t.due", r);
                        format!("(t.due IS NOT NULL AND {inside})")
                    }
                });
            }
            self.and(format!("({})", alts.join(" OR ")));
        }
    }
}

fn negate(negated: bool, cond: String) -> String {
    if negated {
        format!("NOT {cond}")
    } else {
        cond
    }
}

/// Every `Folder:` line holds (lines AND, alternatives OR, `not` negates one).
fn folders_match(label: &str, folders: &[Vec<(bool, String, folder_scope::Mode)>]) -> bool {
    folders.iter().all(|group| {
        group
            .iter()
            .any(|(negated, path, mode)| folder_scope::matches(label, path, *mode) != *negated)
    })
}

fn date_col(s: Option<String>) -> Option<NaiveDate> {
    s.as_deref().and_then(crate::props::parse_iso_date)
}

/// What the owner should know about the property lines: a key no visible
/// note carries, and visible notes whose value for a date-tested key is not
/// a date. View notes never count (spec §8.2).
fn property_notices(
    conn: &Connection,
    account_id: &str,
    q: &ViewQuery,
    hidden: &dyn Fn(&str) -> bool,
) -> SqlResult<Vec<String>> {
    let mut notices = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut labels = conn.prepare_cached(&format!(
        "SELECT n.label, max(p.value_date IS NULL) FROM note_props p
         JOIN notes n ON n.account_id = p.account_id AND n.uuid = p.uuid
         WHERE {LIVE_SQL} AND {NOT_A_VIEW_SQL} AND p.key = ?2
         GROUP BY n.uuid"
    ))?;
    for clause in &q.props {
        if seen.contains(&clause.key.as_str()) {
            continue;
        }
        seen.push(&clause.key);
        let carriers: Vec<bool> = labels
            .query_map(params![account_id, clause.key], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
            })?
            .filter_map(|row| match row {
                Ok((label, _)) if hidden(&label) => None,
                Ok((_, undated)) => Some(Ok(undated)),
                Err(e) => Some(Err(e)),
            })
            .collect::<SqlResult<_>>()?;
        if carriers.is_empty() {
            notices.push(format!("no note has the property `{}`", clause.key));
            continue;
        }
        let date_tested = clause
            .any_of
            .iter()
            .any(|(_, t)| matches!(t, ValueTest::Date(_)));
        let undated = carriers.iter().filter(|u| **u).count();
        if date_tested && undated > 0 {
            let (n, verb) = if undated == 1 {
                ("note", "has")
            } else {
                ("notes", "have")
            };
            notices.push(format!(
                "{undated} {n} {verb} `{}:` values that are not dates",
                clause.key
            ));
        }
    }
    Ok(notices)
}

impl Db {
    /// The account's views: live notes whose folder's last segment is
    /// `__Views__`, by name. A view that does not parse is listed with its
    /// errors, never dropped.
    pub fn list_views(&self, account_id: &str) -> SqlResult<Vec<ViewDef>> {
        let notes: Vec<(String, String, String)> = self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT n.uuid, n.title, n.label FROM notes n
                 WHERE {LIVE_SQL} AND {IS_A_VIEW_SQL}
                 ORDER BY n.title COLLATE NOCASE, n.uuid"
            ))?;
            let rows = stmt.query_map(params![account_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            rows.collect::<SqlResult<Vec<_>>>()
        })?;
        let mut out = Vec::with_capacity(notes.len());
        for (uuid, name, label) in notes {
            let props = self.props_for_note(account_id, &uuid)?;
            let lines = props
                .iter()
                .map(|p| format!("{}: {}", p.key_display, p.value))
                .collect();
            let errors = parse_view(&props).err().unwrap_or_default();
            out.push(ViewDef {
                uuid,
                name,
                label,
                lines,
                errors,
            });
        }
        Ok(out)
    }

    /// Run `q` over `account_id`'s notes as of `today` (the device's local
    /// date). `hidden` names folders the reader may not see — `|_| false`
    /// for the app, the agent's scope for MCP — and their notes are left out
    /// of rows and notices alike. At most `q.limit` rows come back.
    pub fn run_view(
        &self,
        account_id: &str,
        q: &ViewQuery,
        hidden: &dyn Fn(&str) -> bool,
        today: NaiveDate,
    ) -> SqlResult<ViewResult> {
        let limit = q.limit as usize;
        let visible = |label: &str| !hidden(label) && folders_match(label, &q.folders);
        let mut w = Where::new(account_id);
        w.note_conditions(q, today);
        self.with_conn(|conn| {
            let rows = match q.show {
                Show::Notes => {
                    let own_due = "(SELECT min(p.value_date) FROM note_props p
                         WHERE p.account_id = n.account_id AND p.uuid = n.uuid AND p.key = 'due')";
                    let order = match q.sort {
                        Sort::Modified => format!("{MODIFIED_SQL} DESC, n.uuid"),
                        Sort::Title => "n.title COLLATE NOCASE, n.uuid".to_string(),
                        Sort::Due => format!("{own_due} IS NULL, {own_due}, {MODIFIED_SQL} DESC, n.uuid"),
                    };
                    let sql = format!(
                        "SELECT n.uuid, n.title, n.label, {MODIFIED_SQL},
                                (SELECT count(*) FROM task_items t
                                  WHERE t.account_id = n.account_id AND t.uuid = n.uuid AND t.done = 0),
                                (SELECT min(t.due) FROM task_items t
                                  WHERE t.account_id = n.account_id AND t.uuid = n.uuid AND t.done = 0)
                         FROM notes n
                         WHERE {}
                         ORDER BY {order}",
                        w.sql
                    );
                    let mut stmt = conn.prepare(&sql)?;
                    let mut out = Vec::new();
                    let mut rows = stmt.query(rusqlite::params_from_iter(w.binds.iter()))?;
                    while let Some(r) = rows.next()? {
                        let label: String = r.get(2)?;
                        if !visible(&label) {
                            continue;
                        }
                        out.push(NoteRow {
                            uuid: r.get(0)?,
                            title: r.get(1)?,
                            label,
                            modified_ms: r.get(3)?,
                            task_count: r.get(4)?,
                            earliest_due: date_col(r.get(5)?),
                        });
                        if out.len() == limit {
                            break;
                        }
                    }
                    ViewRows::Notes(out)
                }
                Show::Tasks => {
                    w.task_conditions(q, today);
                    let order = match q.sort {
                        Sort::Due => "t.due IS NULL, t.due, n.title COLLATE NOCASE, n.uuid, t.idx".to_string(),
                        Sort::Modified => format!("{MODIFIED_SQL} DESC, n.uuid, t.idx"),
                        Sort::Title => "n.title COLLATE NOCASE, n.uuid, t.idx".to_string(),
                    };
                    // The JOIN is what keeps derived rows whose note is gone
                    // (`prune_clean_in_label` leaves them until a sweep) out.
                    let sql = format!(
                        "SELECT n.uuid, n.title, n.label, t.idx, t.text, t.done, t.due
                         FROM task_items t
                         JOIN notes n ON n.account_id = t.account_id AND n.uuid = t.uuid
                         WHERE {}
                         ORDER BY {order}",
                        w.sql
                    );
                    let mut stmt = conn.prepare(&sql)?;
                    let mut out = Vec::new();
                    let mut rows = stmt.query(rusqlite::params_from_iter(w.binds.iter()))?;
                    while let Some(r) = rows.next()? {
                        let label: String = r.get(2)?;
                        if !visible(&label) {
                            continue;
                        }
                        out.push(TaskRow {
                            uuid: r.get(0)?,
                            title: r.get(1)?,
                            label,
                            idx: r.get::<_, i64>(3)? as usize,
                            text: r.get(4)?,
                            done: r.get(5)?,
                            due: date_col(r.get(6)?),
                        });
                        if out.len() == limit {
                            break;
                        }
                    }
                    ViewRows::Tasks(out)
                }
            };
            let notices = property_notices(conn, account_id, q, hidden)?;
            Ok(ViewResult { rows, notices })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::props::parse_property_block;
    use crate::test_support::{note, temp_db};
    use rusqlite::params;

    const A: &str = "a";

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn today() -> NaiveDate {
        d("2026-11-19")
    }

    fn task(text: &str, done: bool) -> String {
        let checked = if done { " checked" } else { "" };
        format!("<div><input type=\"checkbox\"{checked}>&nbsp;{text}</div>")
    }

    /// A cached note: the body never carries the title (Task 4).
    fn put(db: &Db, account: &str, uuid: &str, label: &str, title: &str, body: &str) {
        let mut n = note(account, uuid).label(label).build();
        n.title = title.to_string();
        n.body_html = body.to_string();
        db.insert_local_new(&n, &crate::provenance::Actor::test())
            .unwrap();
    }

    fn sql(db: &Db, s: &str) {
        db.with_conn(|c| c.execute_batch(s)).unwrap();
    }

    /// Eight notes in account `a` across Projects/A, Projects/B, Inbox,
    /// __Views__ and Secret, one trashed, one in another account, and
    /// derived rows whose note is gone.
    fn seeded() -> Db {
        let db = temp_db();
        put(
            &db,
            A,
            "alpha",
            "Notes/Projects/A",
            "Alpha",
            &format!(
                "<div>Status: active</div><div>Due: 2026-11-01</div><div><br></div>{}{}{}",
                task("Call Bob 📅 2026-11-18", false),
                task("Old done 📅 2026-11-10", true),
                task("Undated alpha", false),
            ),
        );
        put(&db, A, "beta", "Notes/Projects/B", "Beta", &format!(
            "<div>Status: waiting</div><div>Next contact: soon</div><div><br></div><div>#someday</div>{}{}",
            task("Due today 📅 2026-11-19", false),
            task("Future 📅 2026-12-01", false),
        ));
        put(&db, A, "gamma", "Notes/Projects/A", "Gamma", &format!(
            "<div>Status: Active</div><div>Next contact: 2026-11-15</div><div><br></div><div>#meeting</div>{}",
            task("Overdue 📅 2026-11-02", false),
        ));
        put(
            &db,
            A,
            "inbox",
            "Notes/Inbox",
            "Inbox note",
            &format!(
                "<div>Status: done</div><div><br></div>{}",
                task("Inbox undated", false),
            ),
        );
        put(
            &db,
            A,
            "deadline",
            "Notes/Inbox",
            "Deadline only",
            "<div>Due: 2026-11-25</div><div><br></div><div>no tasks here</div>",
        );
        put(&db, A, "view1", "Notes/__Views__", "My view", &format!(
            "<div>Show: tasks</div><div>Due: &lt;= today</div><div>Done: no</div><div>Viewonly: x</div><div><br></div>{}",
            task("View task 📅 2026-11-01", false),
        ));
        put(
            &db,
            A,
            "secret",
            "Notes/Secret",
            "Secret",
            &format!(
                "<div>Status: active</div><div><br></div>{}",
                task("Secret task 📅 2026-11-17", false),
            ),
        );
        put(
            &db,
            A,
            "trashed",
            "Notes/Projects/A",
            "Trashed",
            &format!(
                "<div>Status: active</div><div><br></div>{}",
                task("Trashed task 📅 2026-11-01", false),
            ),
        );
        sql(
            &db,
            "UPDATE notes SET sync_state = 'deleted_pending' WHERE uuid = 'trashed'",
        );
        put(
            &db,
            "b",
            "other",
            "Notes/Projects/A",
            "Other account",
            &format!(
                "<div>Status: active</div><div><br></div>{}",
                task("Other task 📅 2026-11-01", false),
            ),
        );
        // What `prune_clean_in_label` can leave until the next sweep.
        sql(
            &db,
            "INSERT INTO note_props (account_id, uuid, pos, key, key_display, value, value_date)
                  VALUES ('a', 'ghost', 0, 'status', 'Status', 'active', NULL);
                  INSERT INTO task_items (account_id, uuid, idx, text, done, due, depth)
                  VALUES ('a', 'ghost', 0, 'Ghost task', 0, '2026-11-01', 0);
                  INSERT INTO note_tags (account_id, uuid, tag) VALUES ('a', 'ghost', 'meeting');",
        );
        db
    }

    fn query(lines: &[&str]) -> ViewQuery {
        parse_view(&parse_property_block(lines)).unwrap()
    }

    fn none_hidden(_: &str) -> bool {
        false
    }

    fn run(db: &Db, lines: &[&str]) -> ViewResult {
        db.run_view(A, &query(lines), &none_hidden, today())
            .unwrap()
    }

    fn notes(r: &ViewResult) -> Vec<&str> {
        match &r.rows {
            ViewRows::Notes(rows) => rows.iter().map(|n| n.uuid.as_str()).collect(),
            ViewRows::Tasks(_) => panic!("expected note rows"),
        }
    }

    fn tasks(r: &ViewResult) -> Vec<(&str, usize)> {
        match &r.rows {
            ViewRows::Tasks(rows) => rows.iter().map(|t| (t.uuid.as_str(), t.idx)).collect(),
            ViewRows::Notes(_) => panic!("expected task rows"),
        }
    }

    #[test]
    fn tasks_due_today_or_earlier_not_done() {
        let db = seeded();
        let r = run(&db, &["Show: tasks", "Due: <= today", "Done: no"]);
        assert_eq!(
            tasks(&r),
            vec![("gamma", 0), ("secret", 0), ("alpha", 0), ("beta", 0)],
            "sorted by due ascending"
        );
        let ViewRows::Tasks(rows) = &r.rows else {
            unreachable!()
        };
        assert_eq!(rows[0].due, Some(d("2026-11-02")));
        assert_eq!(rows[0].text, "Overdue 📅 2026-11-02");
        assert_eq!(
            (rows[0].title.as_str(), rows[0].label.as_str()),
            ("Gamma", "Notes/Projects/A")
        );
        assert!(!rows[0].done);
        assert!(r.notices.is_empty(), "{:?}", r.notices);
    }

    #[test]
    fn notes_by_property_or_and_not() {
        let db = seeded();
        let r = run(
            &db,
            &["Status: active, waiting", "Tag: not someday", "Sort: title"],
        );
        // `Active` matches `active`; beta is waiting but tagged someday;
        // inbox is done; the trashed note and the orphan rows never show.
        assert_eq!(notes(&r), vec!["alpha", "gamma", "secret"]);
        let r = run(&db, &["Status: not active", "Sort: title"]);
        assert_eq!(notes(&r), vec!["beta", "deadline", "inbox"]);
    }

    #[test]
    fn note_rows_count_open_tasks() {
        let db = seeded();
        let r = run(&db, &["Status: active", "Folder: Projects/A"]);
        let ViewRows::Notes(rows) = &r.rows else {
            panic!()
        };
        let alpha = rows.iter().find(|n| n.uuid == "alpha").unwrap();
        assert_eq!(alpha.task_count, 2, "the ticked row is not counted");
        assert_eq!(alpha.earliest_due, Some(d("2026-11-18")));
        assert_eq!(alpha.title, "Alpha");
        assert_eq!(alpha.label, "Notes/Projects/A");
        assert!(alpha.modified_ms > 0);
    }

    #[test]
    fn notes_due_property_compares_dates() {
        let db = seeded();
        // gamma only has a dated task; deadline's own Due is in the future.
        assert_eq!(
            notes(&run(&db, &["Show: notes", "Due: < today"])),
            vec!["alpha"]
        );
        assert_eq!(notes(&run(&db, &["Due: >= today"])), vec!["deadline"]);
        assert_eq!(
            notes(&run(&db, &["Due: 2026-11-01"])),
            vec!["alpha"],
            "a bare date is exact text"
        );
    }

    #[test]
    fn note_due_never_in_task_view() {
        let db = seeded();
        for lines in [
            &["Show: tasks", "Due: >= 2026-11-01"][..],
            &["Show: tasks", "Due: none"][..],
            &["Show: tasks", "Done: no"][..],
        ] {
            let r = run(&db, lines);
            assert!(
                tasks(&r).iter().all(|(u, _)| *u != "deadline"),
                "{lines:?}: {:?}",
                tasks(&r)
            );
        }
    }

    #[test]
    fn views_and_trash_excluded() {
        let db = seeded();
        let all_tasks = run(&db, &["Show: tasks", "Done: no"]);
        let all_notes = run(&db, &["Status: active"]);
        for u in ["view1", "trashed", "ghost", "other"] {
            assert!(
                tasks(&all_tasks).iter().all(|(x, _)| *x != u),
                "{u} task row"
            );
            assert!(!notes(&all_notes).contains(&u), "{u} note row");
        }
        assert!(
            notes(&run(&db, &["Tag: meeting"])) == vec!["gamma"],
            "ghost's tag row never surfaces"
        );
        let r = run(&db, &["Viewonly: x"]);
        assert!(notes(&r).is_empty());
    }

    #[test]
    fn hidden_excluded() {
        let db = seeded();
        let q = query(&["Show: tasks", "Done: no"]);
        let hide = |l: &str| l.starts_with("Notes/Secret");
        let r = db.run_view(A, &q, &hide, today()).unwrap();
        assert!(tasks(&r).iter().all(|(u, _)| *u != "secret"));
        let r = db.run_view(A, &q, &none_hidden, today()).unwrap();
        assert!(tasks(&r).iter().any(|(u, _)| *u == "secret"));
        let q = query(&["Status: active"]);
        assert!(!notes(&db.run_view(A, &q, &hide, today()).unwrap()).contains(&"secret"));
    }

    #[test]
    fn unknown_key_notice() {
        let db = seeded();
        let r = run(&db, &["Colour: red"]);
        assert!(notes(&r).is_empty());
        assert_eq!(
            r.notices,
            vec!["no note has the property `colour`".to_string()]
        );
        // Only the view note itself carries `Viewonly:`.
        let r = run(&db, &["Viewonly: x"]);
        assert_eq!(
            r.notices,
            vec!["no note has the property `viewonly`".to_string()]
        );
        // A key only hidden notes carry is unknown to the scoped reader.
        put(
            &db,
            A,
            "hush",
            "Notes/Secret",
            "Hush",
            "<div>Codename: x</div>",
        );
        let hide = |l: &str| l.starts_with("Notes/Secret");
        let r = db
            .run_view(A, &query(&["Codename: x"]), &hide, today())
            .unwrap();
        assert!(notes(&r).is_empty());
        assert_eq!(
            r.notices,
            vec!["no note has the property `codename`".to_string()]
        );
    }

    #[test]
    fn non_date_values_notice() {
        let db = seeded();
        let r = run(&db, &["Next contact: <= today"]);
        assert_eq!(notes(&r), vec!["gamma"], "beta's `soon` is not a date");
        assert_eq!(
            r.notices,
            vec!["1 note has `next contact:` values that are not dates".to_string()]
        );
        // The view note's own `Due: <= today` is not counted.
        let r = run(&db, &["Due: < today"]);
        assert!(r.notices.is_empty(), "{:?}", r.notices);
    }

    #[test]
    fn due_none_matches_undated_rows() {
        let db = seeded();
        let r = run(&db, &["Show: tasks", "Due: none", "Done: no"]);
        assert_eq!(tasks(&r), vec![("alpha", 2), ("inbox", 0)]);
    }

    #[test]
    fn bound_params_only() {
        let db = seeded();
        let r = run(&db, &["Status: x' OR 1=1 --"]);
        assert!(notes(&r).is_empty());
        let r = run(&db, &["Text: x\" OR 1=1 --"]);
        assert!(notes(&r).is_empty());
        let r = run(&db, &["Tag: x' OR '1'='1"]);
        assert!(notes(&r).is_empty());
    }

    #[test]
    fn this_week_is_monday_to_sunday() {
        let db = seeded();
        // 2026-11-19 is a Thursday: the week is 2026-11-16 .. 2026-11-22.
        let r = run(&db, &["Show: tasks", "Due: this week"]);
        assert_eq!(tasks(&r), vec![("secret", 0), ("alpha", 0), ("beta", 0)]);
        let r = db
            .run_view(
                A,
                &query(&["Show: tasks", "Due: this week"]),
                &none_hidden,
                d("2026-11-16"),
            )
            .unwrap();
        assert_eq!(
            tasks(&r),
            vec![("secret", 0), ("alpha", 0), ("beta", 0)],
            "Monday starts its own week"
        );
        let r = db
            .run_view(
                A,
                &query(&["Show: tasks", "Due: this week"]),
                &none_hidden,
                d("2026-11-22"),
            )
            .unwrap();
        assert_eq!(
            tasks(&r),
            vec![("secret", 0), ("alpha", 0), ("beta", 0)],
            "Sunday ends it"
        );
        let r = db
            .run_view(
                A,
                &query(&["Show: tasks", "Due: this week"]),
                &none_hidden,
                d("2026-11-23"),
            )
            .unwrap();
        assert!(tasks(&r).is_empty());
    }

    #[test]
    fn relative_ranges() {
        let db = seeded();
        assert_eq!(
            tasks(&run(&db, &["Show: tasks", "Due: today"])),
            vec![("beta", 0)]
        );
        assert_eq!(
            tasks(&run(&db, &["Show: tasks", "Due: next 12 days"])),
            vec![("beta", 0), ("beta", 1)]
        );
        assert_eq!(
            tasks(&run(&db, &["Show: tasks", "Due: last 2 days"])),
            vec![("secret", 0), ("alpha", 0), ("beta", 0)]
        );
        assert_eq!(
            tasks(&run(&db, &["Show: tasks", "Due: > today"])),
            vec![("beta", 1)]
        );
        assert_eq!(
            tasks(&run(
                &db,
                &["Show: tasks", "Due: >= 2026-11-17", "Due: < 2026-11-19"]
            )),
            vec![("secret", 0), ("alpha", 0)],
            "two Due lines AND"
        );
        // Ticked rows are rows too unless `Done: no` says otherwise.
        assert_eq!(
            tasks(&run(
                &db,
                &["Show: tasks", "Due: = 2026-11-10", "Done: yes"]
            )),
            vec![("alpha", 1)]
        );
    }

    #[test]
    fn folders_and_together_or_within() {
        let db = seeded();
        let r = run(
            &db,
            &[
                "Folder: Projects/*",
                "Folder: not Projects/B",
                "Sort: title",
            ],
        );
        assert_eq!(notes(&r), vec!["alpha", "gamma"]);
        let r = run(&db, &["Folder: Inbox, Secret", "Sort: title"]);
        assert_eq!(notes(&r), vec!["deadline", "inbox", "secret"]);
        let r = run(&db, &["Folder: Projects", "Sort: title"]);
        assert!(notes(&r).is_empty(), "no `/*`: exact folder only");
    }

    #[test]
    fn tags_and_together() {
        let db = seeded();
        assert_eq!(
            notes(&run(&db, &["Tag: meeting, someday", "Sort: title"])),
            vec!["beta", "gamma"]
        );
        assert_eq!(
            notes(&run(&db, &["Tag: meeting", "Tag: someday"])),
            Vec::<&str>::new()
        );
        assert_eq!(notes(&run(&db, &["Tag: #Meeting"])), vec!["gamma"]);
    }

    #[test]
    fn text_uses_the_search_index() {
        let db = seeded();
        assert_eq!(notes(&run(&db, &["Text: call bob"])), vec!["alpha"]);
        assert_eq!(
            notes(&run(&db, &["Text: LL"])),
            vec!["alpha"],
            "short words fall back to LIKE"
        );
        assert!(
            notes(&run(&db, &["Text: di"])).is_empty(),
            "markup (`<div>`) is not text"
        );
        assert_eq!(
            notes(&run(&db, &["Text: Call", "Text: nothing like this"])),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn pinned_sort_and_limit() {
        let db = seeded();
        sql(
            &db,
            "UPDATE notes SET pinned = 1 WHERE uuid IN ('beta', 'secret')",
        );
        assert_eq!(
            notes(&run(&db, &["Pinned: yes", "Sort: title"])),
            vec!["beta", "secret"]
        );
        assert_eq!(
            notes(&run(&db, &["Pinned: yes", "Sort: title", "Limit: 1"])),
            vec!["beta"]
        );
        let r = run(&db, &["Show: tasks", "Done: no", "Limit: 2"]);
        assert_eq!(tasks(&r).len(), 2);
        // Sort: due under notes sorts by the note's own `Due:`, undated last.
        let r = run(&db, &["Folder: Projects/*, Inbox", "Sort: due"]);
        assert_eq!(&notes(&r)[..2], &["alpha", "deadline"]);
        // Sort: modified — newest first.
        sql(
            &db,
            "UPDATE notes SET last_remote_modified_at = 10, last_local_modified_at = 10;
                  UPDATE notes SET last_remote_modified_at = 30 WHERE uuid = 'gamma';
                  UPDATE notes SET last_remote_modified_at = 20 WHERE uuid = 'alpha';",
        );
        assert_eq!(
            notes(&run(&db, &["Folder: Projects/A"])),
            vec!["gamma", "alpha"]
        );
    }

    #[test]
    fn modified_uses_local_day_bounds() {
        let db = temp_db();
        put(&db, A, "in", "Notes", "In", "<div>x</div>");
        put(&db, A, "out", "Notes", "Out", "<div>x</div>");
        put(&db, A, "local", "Notes", "Local only", "<div>x</div>");
        let start = day_start_ms(today());
        db.with_conn(|c| {
            c.execute("UPDATE notes SET last_remote_modified_at = ?1 WHERE uuid = 'in'", params![start])?;
            c.execute("UPDATE notes SET last_remote_modified_at = ?1 WHERE uuid = 'out'", params![start - 1])?;
            // Never pushed: the local time is the note's modification.
            c.execute(
                "UPDATE notes SET last_remote_modified_at = NULL, last_local_modified_at = ?1 WHERE uuid = 'local'",
                params![start + 3_600_000],
            )
        })
        .unwrap();
        let r = run(&db, &["Modified: today", "Sort: title"]);
        assert_eq!(notes(&r), vec!["in", "local"]);
        let r = run(&db, &["Modified: < today"]);
        assert_eq!(notes(&r), vec!["out"]);
        let r = run(&db, &["Modified: last 1 days", "Sort: title"]);
        assert_eq!(notes(&r), vec!["in", "local", "out"]);
    }

    /// Ruling R10 through the engine: a bare date under `Modified:` is that
    /// local day, and under a date-valued property it matches that date.
    #[test]
    fn bare_date_under_modified_and_a_date_property_means_that_day() {
        let db = seeded();
        assert_eq!(notes(&run(&db, &["Next contact: 2026-11-15"])), vec!["gamma"]);
        assert!(notes(&run(&db, &["Next contact: 2026-11-16"])).is_empty());

        let db = temp_db();
        put(&db, A, "on", "Notes", "On", "<div>x</div>");
        put(&db, A, "before", "Notes", "Before", "<div>x</div>");
        put(&db, A, "after", "Notes", "After", "<div>x</div>");
        let start = day_start_ms(d("2026-11-10"));
        db.with_conn(|c| {
            c.execute("UPDATE notes SET last_remote_modified_at = ?1 WHERE uuid = 'on'", params![start + 3_600_000])?;
            c.execute("UPDATE notes SET last_remote_modified_at = ?1 WHERE uuid = 'before'", params![start - 1])?;
            c.execute("UPDATE notes SET last_remote_modified_at = ?1 WHERE uuid = 'after'", params![start + 86_400_000])
        })
        .unwrap();
        assert_eq!(notes(&run(&db, &["Modified: 2026-11-10"])), vec!["on"]);
    }

    #[test]
    fn list_views_reads_view_notes() {
        let db = seeded();
        put(
            &db,
            A,
            "bad",
            "Notes/__Views__",
            "Broken",
            "<div>Show: tasks</div><div>Due: soon</div>",
        );
        put(
            &db,
            A,
            "gone",
            "Notes/__Views__",
            "Gone",
            "<div>Pinned: yes</div>",
        );
        sql(
            &db,
            "UPDATE notes SET sync_state = 'deleted_pending' WHERE uuid = 'gone'",
        );
        put(
            &db,
            A,
            "nested",
            "Notes/Work/__Views__",
            "Nested",
            "<div>Pinned: yes</div>",
        );
        put(
            &db,
            A,
            "lookalike",
            "Notes/__Views__x",
            "Not a view",
            "<div>Pinned: yes</div>",
        );
        let views = db.list_views(A).unwrap();
        let names: Vec<_> = views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["Broken", "My view", "Nested"]);
        let mine = &views[1];
        assert_eq!(mine.uuid, "view1");
        assert_eq!(mine.label, "Notes/__Views__");
        assert_eq!(
            mine.lines,
            vec!["Show: tasks", "Due: <= today", "Done: no", "Viewonly: x"]
        );
        assert!(mine.errors.is_empty());
        assert_eq!(views[0].errors.len(), 1);
        assert_eq!(views[0].errors[0].key, "due");
        assert!(db.list_views("b").unwrap().is_empty());
    }
}
