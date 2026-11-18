//! Search, note links, citations and the virtual folders (orphaned, stale,
//! extract) — the read queries that rank or filter across notes.
//! Split out of `db.rs` 2026-11-16.

use super::*;

impl Db {
    /// Full-text search over title, body and folder name. Returns full notes
    /// (so the UI can show results that aren't loaded into memory). Excludes
    /// deleted_pending. The trigram tokenizer needs >=3 chars; shorter queries
    /// fall back to a LIKE scan so 1-2 char searches still work.
    /// `account_id`/`label` are optional filters (None = don't filter), so the
    /// caller picks the scope: a folder (both set), one account (account set),
    /// or everything across all accounts (both None). Implemented with
    /// `?n IS NULL OR col = ?n` so the SQL stays static. `label` is an EXACT
    /// filter (gotcha #1) — matching a folder's NAME is a different thing,
    /// done by the `folder` column below.
    ///
    /// Folder-name hits rank strictly after title/body hits: every note that
    /// matches in its text comes first, in the same order as before folder
    /// names were searchable, then notes that match only by folder, newest
    /// first (spec 2026-10-09-search-folder-names-design.md, D4).
    pub fn search_notes(
        &self,
        account_id: Option<&str>,
        label: Option<&str>,
        query: &str,
        exclude_accounts: &[String],
    ) -> SqlResult<Vec<CachedNote>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }

        if q.chars().count() < 3 {
            let conn = self.conn.lock().unwrap();
            let cols = Self::NOTE_COLS;
            // trigram can't index <3 chars — LIKE fallback over title + body
            // + folder text, text hits first.
            let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
            // The LIMIT makes the ordering load-bearing: it decides WHICH 200
            // notes the user ever sees, and the frontend's own date sort can
            // only reorder what already came back.
            let order = Self::NEWEST_FIRST;
            let folder = folder_text_sql("n.label");
            let text_hit = "(n.title LIKE ?1 ESCAPE '\\' OR n.body_html LIKE ?1 ESCAPE '\\')";
            let excl = exclude_clause(exclude_accounts, 4);
            let sql = format!(
                "SELECT {cols} FROM notes n
                 WHERE n.sync_state != 'deleted_pending'
                   AND ({text_hit} OR {folder} LIKE ?1 ESCAPE '\\')
                   AND (?2 IS NULL OR n.account_id = ?2)
                   AND (?3 IS NULL OR n.label = ?3){excl}
                 ORDER BY {text_hit} DESC, {order} LIMIT 200"
            );
            let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
                Box::new(like),
                Box::new(account_id.map(str::to_string)),
                Box::new(label.map(str::to_string)),
            ];
            for a in exclude_accounts {
                binds.push(Box::new(a.clone()));
            }
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), row_to_note)?;
            return rows.collect();
        }

        // FTS5 phrase query (quoted → substring match; double quotes escaped).
        let hits = self.search_fts_tiers(account_id, label, &[fts_phrase(q)], exclude_accounts)?;

        // Additive fallback: the phrase match above requires the ENTIRE query
        // to appear verbatim as one substring. A caller combining several
        // search terms in one call (e.g. an MCP client passing "credential
        // password api key token secret auth") gets a silent, correct-but-
        // useless `[]`, because that six-word sequence never appears
        // anywhere. Only retry when the strict match found nothing AND there
        // is more than one token — a single-word query is already a phrase
        // of one token, so retrying it would be redundant, and a query that
        // already hit must never be widened by this branch.
        let tokens: Vec<&str> = q.split_whitespace().collect();
        if hits.is_empty() && tokens.len() > 1 {
            // FTS5's MATCH must be the sole predicate against the virtual
            // table per query — `notes_fts MATCH ?1 OR notes_fts MATCH ?2`
            // is not valid, so instead of one OR'd MATCH expression we run
            // one query per token and union the results in Rust.
            let phrases: Vec<String> = tokens.iter().map(|t| fts_phrase(t)).collect();
            return self.search_fts_tiers(account_id, label, &phrases, exclude_accounts);
        }
        Ok(hits)
    }

    /// The FTS half of `search_notes`, for one phrase or (the OR-of-terms
    /// fallback) several, unioned in Rust because a `MATCH` must be the sole
    /// predicate against the virtual table.
    ///
    /// Tier 1 is every phrase against `{title body}`, `ORDER BY rank` —
    /// exactly what this search was before folder names were indexed. Tier 2,
    /// only while fewer than 200 rows are in hand, is every phrase against
    /// `folder`, newest first. Tiers rather than bm25 column weights because
    /// weights cannot guarantee the order: bm25 normalises by column length,
    /// and a folder column is a few trigrams long.
    ///
    /// Dedupes on the notes table's REAL primary key, `(uuid, account_id)` —
    /// not uuid alone. The same UUID legitimately exists in two accounts (a
    /// note moved or duplicated across them; see CLAUDE.md roadmap #6), and a
    /// cross-account search — `account_id = None`, the MCP server's default —
    /// would silently drop one of the two.
    pub(crate) fn search_fts_tiers(
        &self,
        account_id: Option<&str>,
        label: Option<&str>,
        phrases: &[String],
        exclude_accounts: &[String],
    ) -> SqlResult<Vec<CachedNote>> {
        const CAP: usize = 200;
        let conn = self.conn.lock().unwrap();
        let cols = Self::NOTE_COLS;
        let excl = exclude_clause(exclude_accounts, 4);
        let tier_sql = |order: &str| {
            format!(
                "SELECT {cols} FROM notes n
                 JOIN notes_fts f ON f.uuid = n.uuid AND f.account_id = n.account_id
                 WHERE notes_fts MATCH ?1
                   AND n.sync_state != 'deleted_pending'
                   AND (?2 IS NULL OR n.account_id = ?2)
                   AND (?3 IS NULL OR n.label = ?3){excl}
                 ORDER BY {order} LIMIT {CAP}"
            )
        };
        let tiers = [
            ("{title body} : ", tier_sql("rank")),
            ("folder : ", tier_sql(Self::NEWEST_FIRST)),
        ];
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (columns, sql) in &tiers {
            if out.len() >= CAP {
                break;
            }
            let mut stmt = conn.prepare(sql)?;
            for phrase in phrases {
                let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
                    Box::new(format!("{columns}{phrase}")),
                    Box::new(account_id.map(str::to_string)),
                    Box::new(label.map(str::to_string)),
                ];
                for a in exclude_accounts {
                    binds.push(Box::new(a.clone()));
                }
                let rows: Vec<CachedNote> = stmt
                    .query_map(rusqlite::params_from_iter(binds.iter()), row_to_note)?
                    .collect::<SqlResult<Vec<_>>>()?;
                for n in rows {
                    if seen.insert((n.uuid.clone(), n.account_id.clone())) {
                        out.push(n);
                    }
                }
            }
        }
        out.truncate(CAP);
        Ok(out)
    }
}

impl Db {
    /// Every clean or dirty note, in any account, whose title starts with
    /// `prefix` (a literal match, never `LIKE`). Ask Jodd's title repair only.
    pub fn notes_with_title_prefix(&self, prefix: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE substr(n.title, 1, length(?1)) = ?1 COLLATE BINARY
               AND n.sync_state IN ('clean', 'dirty')",
            Self::NOTE_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![prefix], row_to_note)?;
        rows.collect()
    }

    /// Every note in `label` and below. Jodd's folder tree is derived by
    /// splitting the label string on '/', with no containment relation in the
    /// data, so "descendant" has to be reconstructed by prefix matching.
    /// Literal prefix comparison matches folder_scope::Mode::Subtree, including
    /// case and wildcard characters; shared conformance tests pin the SQL form.
    ///
    /// Read-only. Excludes deleted_pending. Ask Jodd only — `search_notes`
    /// keeps its exact-label semantics for the search box (spec §5.6).
    pub fn list_notes_in_subtree(
        &self,
        account_id: &str,
        label: &str,
    ) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1
               AND n.sync_state != 'deleted_pending'
               AND (n.label = ?2 OR substr(n.label, 1, length(?2) + 1) = (?2 || '/') COLLATE BINARY)
             ORDER BY {}",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, label], row_to_note)?;
        rows.collect()
    }

    /// Notes of `account_id` OUTSIDE the `subtree` folder (itself and every
    /// descendant) whose title or text contains `term` — Research Studio's
    /// words signal (spec 2026-10-25 §4.2). Best BM25 match first, at most
    /// `cap`. Excludes deleted_pending.
    ///
    /// Two differences from `search_notes`, both why this is its own query:
    /// the subtree is excluded IN SQL, before the cap — the research folder's
    /// own notes, which by construction contain the folder's title words, would
    /// otherwise fill the cap and hide every outside match; and only the
    /// `{title body}` columns are searched — a note whose folder NAME is the
    /// only match is not a mention. The subtree test is the literal prefix
    /// `list_notes_in_subtree` uses, negated; never `LIKE` (gotcha #33).
    ///
    /// `term` must be at least 3 characters: the trigram index cannot match
    /// less, and Studio's terms come from `extract_query_terms`, which drops
    /// anything shorter. A shorter term returns nothing (no LIKE fallback).
    pub fn search_text_outside_subtree(
        &self,
        account_id: &str,
        subtree: &str,
        term: &str,
        cap: usize,
    ) -> SqlResult<Vec<CachedNote>> {
        let term = term.trim();
        debug_assert!(term.chars().count() >= 3, "trigram FTS needs >= 3 chars: {term:?}");
        if term.chars().count() < 3 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             JOIN notes_fts f ON f.uuid = n.uuid AND f.account_id = n.account_id
             WHERE notes_fts MATCH ?1
               AND n.account_id = ?2
               AND n.sync_state != 'deleted_pending'
               AND NOT (n.label = ?3 OR substr(n.label, 1, length(?3) + 1) = (?3 || '/') COLLATE BINARY)
             ORDER BY rank LIMIT ?4",
            Self::NOTE_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![format!("{{title body}} : {}", fts_phrase(term)), account_id, subtree, cap as i64],
            row_to_note,
        )?;
        rows.collect()
    }

    /// Notes whose body contains at least one checklist row. Feeds
    /// `list_tasks` (jodd-mcp spec §4.6(B)): the SQL `LIKE` pre-filter is
    /// mandatory, not an optimization — the measured production vault has
    /// 6,655 notes and 18MB of body_html in a single account, so parsing
    /// every body to find checkboxes is not viable. `label`, when given, is
    /// the recursive-subtree filter (see `list_notes_in_subtree` / gotcha
    /// #1): `Notes/A` matches itself and every descendant, never the
    /// lexical-prefix sibling `Notes/AB`.
    ///
    /// Read-only. Excludes deleted_pending.
    pub fn list_notes_with_checkboxes(
        &self,
        account_id: &str,
        label: Option<&str>,
    ) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1
               AND n.sync_state != 'deleted_pending'
               AND n.body_html LIKE '%type=\"checkbox\"%'
               AND (?2 IS NULL OR n.label = ?2 OR substr(n.label, 1, length(?2) + 1) = (?2 || '/') COLLATE BINARY)
             ORDER BY {}",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, label], row_to_note)?;
        rows.collect()
    }

    /// The `limit` most recently modified notes, newest first.
    ///
    /// Ordered by `last_remote_modified_at` (epoch ms), NEVER by `date` —
    /// `date` is an RFC822 string and sorts by weekday name, putting 2013
    /// above 2020 (spec F4). Falls back to `last_local_modified_at` for rows
    /// whose remote timestamp is NULL (local-only notes never yet pushed).
    pub fn list_recent_notes(
        &self,
        account_id: Option<&str>,
        limit: usize,
        exclude_accounts: &[String],
    ) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let excl = exclude_clause(exclude_accounts, 3);
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.sync_state != 'deleted_pending'
               AND (?1 IS NULL OR n.account_id = ?1){excl}
             ORDER BY {}
             LIMIT ?2",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(account_id.map(str::to_string)),
            Box::new(limit as i64),
        ];
        for a in exclude_accounts {
            binds.push(Box::new(a.clone()));
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), row_to_note)?;
        rows.collect()
    }

    /// How many notes a scope contains — the denominator Ask Jodd shows the
    /// user ("6,655 in scope → 400 considered → 9 read"). `label_subtree` is
    /// recursive, matching list_notes_in_subtree.
    pub fn count_notes_in_scope(
        &self,
        account_id: Option<&str>,
        label_subtree: Option<&str>,
        exclude_accounts: &[String],
    ) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        let excl = exclude_clause(exclude_accounts, 3);
        let sql = format!(
            "SELECT COUNT(*) FROM notes n
             WHERE n.sync_state != 'deleted_pending'
               AND (?1 IS NULL OR n.account_id = ?1)
               AND (?2 IS NULL OR n.label = ?2 OR substr(n.label, 1, length(?2) + 1) = (?2 || '/') COLLATE BINARY){excl}"
        );
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(account_id.map(str::to_string)),
            Box::new(label_subtree.map(str::to_string)),
        ];
        for a in exclude_accounts {
            binds.push(Box::new(a.clone()));
        }
        let mut stmt = conn.prepare(&sql)?;
        let n: i64 = stmt.query_row(rusqlite::params_from_iter(binds.iter()), |r| r.get(0))?;
        Ok(n as usize)
    }

    /// One note by identity, or None if it is absent or locally deleted.
    ///
    /// Returns None rather than erroring for deleted_pending: Ask Jodd may
    /// select a note that a sync tick removes moments later, and dropping one
    /// source beats failing the whole turn.
    pub fn note_by_uuid(&self, account_id: &str, uuid: &str) -> SqlResult<Option<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1 AND n.uuid = ?2
               AND n.sync_state != 'deleted_pending'",
            Self::NOTE_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_row(params![account_id, uuid], row_to_note).optional()
    }

    /// Notes that link TO this note via a `[[title]]` wikilink (backlinks).
    /// Matched by this note's normalized title against edges.dst_title.
    pub fn backlinks(&self, account_id: &str, uuid: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let title: Option<String> = conn
            .query_row(
                "SELECT title FROM notes WHERE account_id = ?1 AND uuid = ?2",
                params![account_id, uuid],
                |r| r.get(0),
            )
            .optional()?;
        // Match links that target THIS note by its id (slug links) — its own
        // or that of any uuid it was rekeyed from (`ALIAS_SHORT_ID`) — OR its
        // title (plain links).
        let id8 = uuid_short(uuid);
        let norm_title = normalize_title(&title.unwrap_or_default());
        let sql = format!(
            "SELECT DISTINCT {} FROM edges e
             JOIN notes n ON n.account_id = e.account_id AND n.uuid = e.src_uuid
             WHERE e.account_id = ?1 AND e.rel = 'mentions'
               AND ( (e.dst_id <> '' AND (e.dst_id = ?2 OR EXISTS (
                        SELECT 1 FROM note_uuid_aliases a
                        WHERE a.account_id = ?1 AND a.new_uuid = ?4
                          AND jodd_uuid_short(a.old_uuid) = e.dst_id)))
                  OR (e.dst_title <> '' AND e.dst_title = ?3) )
               AND n.uuid != ?4 AND n.sync_state != 'deleted_pending'
             ORDER BY {}",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, id8, norm_title, uuid], row_to_note)?;
        rows.collect()
    }

    /// Notes this note links TO — its `[[..]]` targets resolved to real notes by
    /// id (slug links) or title (plain links). Unresolved links don't appear.
    pub fn outgoing_links(&self, account_id: &str, uuid: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT DISTINCT {} FROM edges e
             JOIN notes n ON n.account_id = e.account_id AND (
                    (e.dst_id <> '' AND (jodd_uuid_short(n.uuid) = e.dst_id
                                         OR {}))
                 OR (e.dst_title <> '' AND lower(trim(n.title)) = e.dst_title) )
             WHERE e.account_id = ?1 AND e.rel = 'mentions' AND e.src_uuid = ?2
               AND n.uuid != ?2 AND n.sync_state != 'deleted_pending'
             ORDER BY {}",
            Self::NOTE_COLS,
            alias_short_id_matches("e.dst_id"),
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, uuid], row_to_note)?;
        rows.collect()
    }

    /// The live note a `[[wikilink]]` key points at, from SQLite only. The key is
    /// the text between the brackets: `…-<8 hex>` resolves by id, anything else
    /// by trimmed, lower-cased title — the same match `outgoing_links` uses.
    /// Several title matches resolve to the most recently modified one (ties
    /// broken by uuid so the answer is stable). `deleted_pending` rows are gone.
    pub fn resolve_note_link(&self, account_id: &str, key: &str) -> SqlResult<Option<CachedNote>> {
        let (dst_id, dst_title) = parse_link_key(key);
        if dst_id.is_empty() && dst_title.is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1 AND n.sync_state != 'deleted_pending'
               AND ( (?2 <> '' AND (jodd_uuid_short(n.uuid) = ?2 OR {}))
                  OR (?3 <> '' AND lower(trim(n.title)) = ?3) )
             ORDER BY max(coalesce(n.last_local_modified_at, 0),
                          coalesce(n.last_remote_modified_at, 0)) DESC, n.uuid
             LIMIT 1",
            Self::NOTE_COLS,
            alias_short_id_matches("?2")
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![account_id, dst_id, dst_title], row_to_note)?;
        rows.next().transpose()
    }

    /// URLs cited by this note (`rel='cites'` edges), sorted for stable
    /// display order.
    pub fn citations(&self, account_id: &str, uuid: &str) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT dst_id FROM edges WHERE account_id = ?1 AND src_uuid = ?2 AND rel = 'cites' ORDER BY dst_id",
        )?;
        let rows = stmt.query_map(params![account_id, uuid], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    /// The first note (if any) that already cites `url`, excluding
    /// `exclude_uuid` (used when checking an append target against its own
    /// prior citations so re-appending into a note that already cites the
    /// same URL doesn't self-flag). Backs Extract's duplicate-source warning.
    pub fn find_citation_owner(
        &self,
        account_id: &str,
        url: &str,
        exclude_uuid: Option<&str>,
    ) -> SqlResult<Option<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT DISTINCT {} FROM edges e
             JOIN notes n ON n.account_id = e.account_id AND n.uuid = e.src_uuid
             WHERE e.account_id = ?1 AND e.rel = 'cites' AND e.dst_id = ?2
               AND n.sync_state != 'deleted_pending'
               AND (?3 IS NULL OR n.uuid != ?3)
             LIMIT 1",
            Self::NOTE_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_row(params![account_id, url, exclude_uuid], row_to_note)
            .optional()
    }

    /// `find_citation_owner` with any number of exclusions. URL ingest must
    /// exclude both the note it reads links from and an append target.
    /// Fetches at most `exclude.len() + 1` owners, so the first survivor is
    /// found without a dynamic `NOT IN` list.
    pub fn find_citation_owner_excluding(
        &self,
        account_id: &str,
        url: &str,
        exclude: &[String],
    ) -> SqlResult<Option<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT DISTINCT {} FROM edges e
             JOIN notes n ON n.account_id = e.account_id AND n.uuid = e.src_uuid
             WHERE e.account_id = ?1 AND e.rel = 'cites' AND e.dst_id = ?2
               AND n.sync_state != 'deleted_pending'
             ORDER BY n.uuid
             LIMIT ?3",
            Self::NOTE_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, url, (exclude.len() + 1) as i64], row_to_note)?;
        for row in rows {
            let note = row?;
            if !exclude.iter().any(|x| x == &note.uuid) {
                return Ok(Some(note));
            }
        }
        Ok(None)
    }

    /// Notes with zero incoming `mentions` edges — nothing else references
    /// them. Backs the "Orphaned" Smart Folder (fully virtual — no `folders`
    /// table row; see design spec decision 4).
    pub fn list_orphaned_notes(&self, account_id: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1 AND n.sync_state != 'deleted_pending'
               AND NOT EXISTS (
                 SELECT 1 FROM edges e
                 WHERE e.account_id = n.account_id AND e.rel = 'mentions'
                   AND ( (e.dst_id <> '' AND (e.dst_id = jodd_uuid_short(n.uuid) OR {}))
                      OR (e.dst_title <> '' AND e.dst_title = lower(trim(n.title))) )
               )
             ORDER BY {}",
            Self::NOTE_COLS,
            alias_short_id_matches("e.dst_id"),
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id], row_to_note)?;
        rows.collect()
    }

    /// Notes whose `last_local_modified_at` is older than 30 days — a
    /// hardcoded threshold, no settings UI (design spec decision 3). Backs
    /// the "Stale" Smart Folder.
    pub fn list_stale_notes(&self, account_id: &str) -> SqlResult<Vec<CachedNote>> {
        const STALE_THRESHOLD_MS: i64 = 30 * 24 * 60 * 60 * 1000;
        let conn = self.conn.lock().unwrap();
        let cutoff = now_ms() - STALE_THRESHOLD_MS;
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1 AND n.sync_state != 'deleted_pending'
               AND n.last_local_modified_at < ?2
             ORDER BY n.last_local_modified_at ASC",
            Self::NOTE_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, cutoff], row_to_note)?;
        rows.collect()
    }

    /// Notes carrying Extract's Source block. Backs the "Extracts" view —
    /// fully virtual like `list_orphaned_notes`: provenance is derived from
    /// the body, never from the folder a note happens to be filed in.
    /// See docs/superpowers/specs/2026-09-15-extract-filing-design.md.
    pub fn list_extract_notes(&self, account_id: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM notes n
             WHERE n.account_id = ?1 AND n.sync_state != 'deleted_pending'
               AND instr(n.body_html, ?2) > 0
             ORDER BY {}",
            Self::NOTE_COLS,
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![account_id, crate::llm::markdown::SOURCE_MARKER],
            row_to_note,
        )?;
        rows.collect()
    }

    /// Autocomplete source for the `[[` link picker: notes whose title matches
    /// `query` (substring, case-insensitive), newest first. Returns (uuid,
    /// title, label) — the frontend builds the slug via note_slug.
    pub fn search_titles(
        &self,
        account_id: &str,
        query: &str,
        limit: i64,
    ) -> SqlResult<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let like = format!("%{}%", query.trim().replace('%', "\\%").replace('_', "\\_"));
        // Aliased `n` so NEWEST_FIRST (which qualifies its columns) applies.
        let sql = format!(
            "SELECT n.uuid, n.title, n.label FROM notes n
             WHERE n.account_id = ?1 AND n.sync_state != 'deleted_pending'
               AND n.title LIKE ?2 ESCAPE '\\'
             ORDER BY {} LIMIT ?3",
            Self::NEWEST_FIRST
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, like, limit], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests_citations {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Db {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        Db::open_unencrypted(&path).expect("open temp db")
    }

    fn mk_note(account_id: &str, uuid: &str, title: &str, body_html: &str) -> CachedNote {
        CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: title.to_string(),
            body_html: body_html.to_string(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 0,
            remote_version: None,
            sync_state: SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: 0,
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
        push_blocked_reason: None,
        push_blocked_by_remote: false,
        }
    }

    #[test]
    fn insert_local_new_derives_cites_edges() {
        let db = temp_db();
        let acct = "test@example.com";
        let uuid = "AAAAAAAA-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(
            acct,
            uuid,
            "Note A",
            "<div>see https://example.com/source and https://example.com/source again</div>",
        ), &crate::provenance::Actor::test())
        .unwrap();

        let conn = db.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE account_id=?1 AND src_uuid=?2 AND rel='cites'",
                params![acct, uuid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "dedup within one body must produce one edge row");
    }

    #[test]
    fn apply_local_edit_full_replaces_cites_edges() {
        let db = temp_db();
        let acct = "test@example.com";
        let uuid = "BBBBBBBB-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(acct, uuid, "Note B", "<div>https://old.example</div>"), &crate::provenance::Actor::test())
            .unwrap();
        db.apply_local_edit(uuid, acct, "Note B", "<div>https://new.example</div>", "Notes", &crate::provenance::Actor::test())
            .unwrap();

        let conn = db.conn.lock().unwrap();
        let urls: Vec<String> = conn
            .prepare("SELECT dst_id FROM edges WHERE account_id=?1 AND src_uuid=?2 AND rel='cites'")
            .unwrap()
            .query_map(params![acct, uuid], |r| r.get(0))
            .unwrap()
            .collect::<SqlResult<Vec<String>>>()
            .unwrap();
        assert_eq!(urls, vec!["https://new.example".to_string()], "old URL must be dropped, not accumulated");
    }

    /// Migration 32: edges derived under the old link rules (an unterminated
    /// `[[` swallowing the next line, a URL cut at its balanced `)`) are
    /// cleared on open and re-derived under the new ones.
    #[test]
    fn migration_32_rebuilds_edges_with_new_rules() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let acct = "test@example.com";
        let uuid = "CCCCCCCC-1111-2222-3333-444444444444";
        {
            let db = Db::open_unencrypted(&path).unwrap();
            db.insert_local_new(
                &mk_note(
                    acct,
                    uuid,
                    "Note C",
                    "<div>[[draft</div><div>[[Real]]</div><div>https://a.co/Foo_(bar)</div>",
                ),
                &crate::provenance::Actor::test(),
            )
            .unwrap();
            let conn = db.conn.lock().unwrap();
            // What the old rules recorded for this body, planted by hand.
            conn.execute_batch(
                "DELETE FROM edges WHERE rel IN ('mentions', 'cites');
                 DELETE FROM migrations WHERE version = 32;",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO edges (account_id, src_uuid, dst_id, dst_title, rel) VALUES
                 (?1, ?2, '', 'draft … [[real', 'mentions'),
                 (?1, ?2, 'https://a.co/Foo_(bar', '', 'cites')",
                params![acct, uuid],
            )
            .unwrap();
        }
        let db = Db::open_unencrypted(&path).unwrap();
        let conn = db.conn.lock().unwrap();
        let rows = |rel: &str| -> Vec<(String, String)> {
            conn.prepare(
                "SELECT dst_id, dst_title FROM edges
                 WHERE account_id=?1 AND src_uuid=?2 AND rel=?3 ORDER BY dst_id, dst_title",
            )
            .unwrap()
            .query_map(params![acct, uuid, rel], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<SqlResult<Vec<_>>>()
            .unwrap()
        };
        assert_eq!(rows("mentions"), vec![("".to_string(), "real".to_string())]);
        assert_eq!(
            rows("cites"),
            vec![("https://a.co/Foo_(bar)".to_string(), "".to_string())]
        );
    }

    #[test]
    fn citations_returns_sorted_urls() {
        let db = temp_db();
        let acct = "test@example.com";
        let uuid = "CCCCCCCC-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(
            acct,
            uuid,
            "Note C",
            "<div>https://z.example and https://a.example</div>",
        ), &crate::provenance::Actor::test())
        .unwrap();
        let urls = db.citations(acct, uuid).unwrap();
        assert_eq!(urls, vec!["https://a.example".to_string(), "https://z.example".to_string()]);
    }

    #[test]
    fn find_citation_owner_locates_and_excludes() {
        let db = temp_db();
        let acct = "test@example.com";
        let owner_uuid = "DDDDDDDD-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(acct, owner_uuid, "Owner", "<div>https://shared.example</div>"), &crate::provenance::Actor::test())
            .unwrap();

        let found = db.find_citation_owner(acct, "https://shared.example", None).unwrap();
        assert_eq!(found.map(|n| n.uuid), Some(owner_uuid.to_string()));

        let excluded = db
            .find_citation_owner(acct, "https://shared.example", Some(owner_uuid))
            .unwrap();
        assert!(excluded.is_none(), "excluding the owner's own uuid must suppress the match");

        let none = db.find_citation_owner(acct, "https://not-cited.example", None).unwrap();
        assert!(none.is_none());
    }

    /// URL ingest excludes the source note AND an append target: a
    /// link-collection note always "cites" its own links.
    #[test]
    fn find_citation_owner_excluding_skips_every_excluded_uuid() {
        let db = crate::test_support::temp_db();
        let acct = "gmail:x@example.com";
        let (a, b) = ("AAAAAAAA-0000-0000-0000-00000000000A", "BBBBBBBB-0000-0000-0000-00000000000B");
        crate::test_support::note(acct, a).body("see https://shared.example/v").insert(&db);
        crate::test_support::note(acct, b).body("also https://shared.example/v").insert(&db);
        let url = "https://shared.example/v";
        assert!(db.find_citation_owner_excluding(acct, url, &[]).unwrap().is_some());
        assert_eq!(db.find_citation_owner_excluding(acct, url, &[a.to_string()]).unwrap().map(|n| n.uuid).as_deref(), Some(b));
        assert_eq!(db.find_citation_owner_excluding(acct, url, &[a.to_string(), b.to_string()]).unwrap(), None);
    }
}

#[cfg(test)]
mod tests_resolve_note_link {
    use crate::test_support::{note, temp_db};

    const ACCT: &str = "gmail:a@example.com";

    #[test]
    fn resolve_note_link_by_id() {
        let db = temp_db();
        note(ACCT, "A1B2C3D4-1111-2222-3333-444444444444").title("Plan").insert(&db);
        note(ACCT, "FFFFFFFF-1111-2222-3333-444444444444").title("Other").insert(&db);
        let hit = db.resolve_note_link(ACCT, "plan-a1b2c3d4").unwrap().unwrap();
        assert_eq!(hit.uuid, "A1B2C3D4-1111-2222-3333-444444444444");
        // The slug's title part is decoration: the id decides.
        let hit = db.resolve_note_link(ACCT, "whatever-A1B2C3D4").unwrap().unwrap();
        assert_eq!(hit.uuid, "A1B2C3D4-1111-2222-3333-444444444444");
    }

    #[test]
    fn resolve_note_link_by_title() {
        let db = temp_db();
        note(ACCT, "A1B2C3D4-1111-2222-3333-444444444444").title(" plan ").insert(&db);
        let hit = db.resolve_note_link(ACCT, "Plan").unwrap().unwrap();
        assert_eq!(hit.uuid, "A1B2C3D4-1111-2222-3333-444444444444");
        assert!(db.resolve_note_link(ACCT, "no such note").unwrap().is_none());
    }

    #[test]
    fn resolve_note_link_picks_newest_title_match() {
        let db = temp_db();
        note(ACCT, "11111111-1111-2222-3333-444444444444").title("Plan").modified_ms(1_000).insert(&db);
        note(ACCT, "33333333-1111-2222-3333-444444444444").title("Plan").modified_ms(3_000).insert(&db);
        note(ACCT, "22222222-1111-2222-3333-444444444444").title("Plan").modified_ms(2_000).insert(&db);
        let hit = db.resolve_note_link(ACCT, "plan").unwrap().unwrap();
        assert_eq!(hit.uuid, "33333333-1111-2222-3333-444444444444");
    }

    #[test]
    fn resolve_note_link_skips_deleted_pending() {
        let db = temp_db();
        let uuid = "A1B2C3D4-1111-2222-3333-444444444444";
        note(ACCT, uuid).title("Plan").insert(&db);
        db.mark_deleted(uuid, ACCT).unwrap();
        assert!(db.resolve_note_link(ACCT, "plan-a1b2c3d4").unwrap().is_none());
        assert!(db.resolve_note_link(ACCT, "Plan").unwrap().is_none());
    }

    #[test]
    fn resolve_note_link_empty_key_is_none() {
        let db = temp_db();
        note(ACCT, "A1B2C3D4-1111-2222-3333-444444444444").title("").insert(&db);
        assert!(db.resolve_note_link(ACCT, "").unwrap().is_none());
        assert!(db.resolve_note_link(ACCT, "   ").unwrap().is_none());
    }

    #[test]
    fn resolve_note_link_hex_suffix_resolves_by_id_only() {
        let db = temp_db();
        // The title equals the whole key, but its uuid does not carry the id:
        // a `-<8 hex>` key is an id, never a title.
        note(ACCT, "11111111-1111-2222-3333-444444444444").title("plan-deadbeef").insert(&db);
        assert!(db.resolve_note_link(ACCT, "plan-deadbeef").unwrap().is_none());
        note(ACCT, "DEADBEEF-1111-2222-3333-444444444444").title("Other").insert(&db);
        let hit = db.resolve_note_link(ACCT, "plan-deadbeef").unwrap().unwrap();
        assert_eq!(hit.uuid, "DEADBEEF-1111-2222-3333-444444444444");
    }

    #[test]
    fn resolve_note_link_other_account_is_none() {
        let db = temp_db();
        note("gmail:b@example.com", "A1B2C3D4-1111-2222-3333-444444444444")
            .title("Plan")
            .insert(&db);
        assert!(db.resolve_note_link(ACCT, "plan-a1b2c3d4").unwrap().is_none());
        assert!(db.resolve_note_link(ACCT, "Plan").unwrap().is_none());
    }
}

/// `[[slug-<8hex>]]` must resolve the same way in Rust (`uuid_short`) and in
/// SQL (`jodd_uuid_short`). A Microsoft note's uuid is an internetMessageId
/// (`<AM0PR…@…>`), where the old `substr(lower(replace(uuid,'-','')),1,8)`
/// never matched `uuid_short`. Spec 2026-10-23-research-studio §5.8.
#[cfg(test)]
mod short_id_sql_tests {
    use super::*;
    use crate::test_support::{note, temp_db};

    const ACCT: &str = "microsoft:me@live.com";
    const MS: &str = "<AM0PR04MB6433D2F1C0E9A1B2@AM0PR04MB6433.eurprd04.prod.outlook.com>";
    const PLAIN: &str = "1a2b3c4d-0000-4000-8000-000000000001";
    const GMAIL: &str = "A1B2C3D4-E5F6-4A7B-8C9D-0E1F2A3B4C5D";
    // Two notes on ONE Exchange server share the `JH0PR04MB7174` prefix.
    const MS_OLD: &str = "<JH0PR04MB7174E99@JH0PR04MB7174.apcprd04.prod.outlook.com>";
    const MS_NEW: &str = "<JH0PR04MB7174A12@JH0PR04MB7174.apcprd04.prod.outlook.com>";

    fn uuids(v: &[CachedNote]) -> Vec<&str> {
        v.iter().map(|n| n.uuid.as_str()).collect()
    }

    /// A target note plus a source note whose body links to `slug`.
    fn linked_pair(db: &Db, target_uuid: &str, slug: &str) {
        note(ACCT, target_uuid).title("Target").insert(db);
        note(ACCT, "source").title("Source").body(&format!("<p>see [[{slug}]]</p>")).insert(db);
    }

    #[test]
    fn the_sql_short_id_agrees_with_uuid_short() {
        assert_eq!(uuid_short(MS), "f3ceba56");
        let db = temp_db();
        let conn = db.conn.lock().unwrap();
        for input in [MS, PLAIN, "", "ABCDEF12-3456-7890-ABCD-EF1234567890", "zzz", "จดโน้ต-ab12"] {
            let sql: String = conn
                .query_row("SELECT jodd_uuid_short(?1)", [input], |r| r.get(0))
                .unwrap();
            assert_eq!(sql, uuid_short(input), "input {input:?}");
        }
        let null: String = conn.query_row("SELECT jodd_uuid_short(NULL)", [], |r| r.get(0)).unwrap();
        assert_eq!(null, "");
    }

    #[test]
    fn a_message_id_uuid_resolves_by_its_short_id() {
        let db = temp_db();
        linked_pair(&db, MS, "target-f3ceba56");
        let hit = db.resolve_note_link(ACCT, "target-f3ceba56").unwrap();
        assert_eq!(hit.map(|n| n.uuid), Some(MS.to_string()));
    }

    #[test]
    fn outgoing_links_finds_a_message_id_target() {
        let db = temp_db();
        linked_pair(&db, MS, "target-f3ceba56");
        let out = db.outgoing_links(ACCT, "source").unwrap();
        assert!(uuids(&out).contains(&MS), "got {:?}", uuids(&out));
    }

    #[test]
    fn a_linked_message_id_note_is_not_orphaned() {
        let db = temp_db();
        linked_pair(&db, MS, "target-f3ceba56");
        let orphans = db.list_orphaned_notes(ACCT).unwrap();
        assert!(!uuids(&orphans).contains(&MS), "got {:?}", uuids(&orphans));
        assert!(uuids(&orphans).contains(&"source"), "the unlinked source is still an orphan");
    }

    #[test]
    fn a_plain_uuid_resolves_exactly_as_before() {
        let db = temp_db();
        linked_pair(&db, PLAIN, "target-1a2b3c4d");
        let hit = db.resolve_note_link(ACCT, "target-1a2b3c4d").unwrap();
        assert_eq!(hit.map(|n| n.uuid), Some(PLAIN.to_string()));
        assert_eq!(uuids(&db.outgoing_links(ACCT, "source").unwrap()), vec![PLAIN]);
        assert!(!uuids(&db.list_orphaned_notes(ACCT).unwrap()).contains(&PLAIN));
    }

    #[test]
    fn an_uppercase_hyphenated_uuid_resolves_exactly_as_before() {
        let db = temp_db();
        linked_pair(&db, GMAIL, "target-a1b2c3d4");
        let hit = db.resolve_note_link(ACCT, "target-a1b2c3d4").unwrap();
        assert_eq!(hit.map(|n| n.uuid), Some(GMAIL.to_string()));
        assert_eq!(uuids(&db.outgoing_links(ACCT, "source").unwrap()), vec![GMAIL]);
        assert!(!uuids(&db.list_orphaned_notes(ACCT).unwrap()).contains(&GMAIL));
    }

    #[test]
    fn fnv1a32_matches_the_published_vectors() {
        assert_eq!(format!("{:08x}", fnv1a32(b"")), "811c9dc5");
        assert_eq!(format!("{:08x}", fnv1a32(b"a")), "e40c292c");
        assert_eq!(format!("{:08x}", fnv1a32(b"foobar")), "bf9cf968");
    }

    #[test]
    fn a_plain_uuid_keeps_its_first_eight_hex_digits() {
        assert_eq!(uuid_short(GMAIL), "a1b2c3d4");
        assert_eq!(uuid_short(PLAIN), "1a2b3c4d");
        // 32 hex digits without hyphens is still a plain uuid.
        assert_eq!(uuid_short("A1B2C3D4E5F64A7B8C9D0E1F2A3B4C5D"), "a1b2c3d4");
    }

    #[test]
    fn anything_that_is_not_a_plain_uuid_hashes_the_whole_id() {
        assert_eq!(uuid_short(""), "811c9dc5");
        assert_eq!(uuid_short(MS_OLD), "9e66d121");
        assert_eq!(uuid_short(MS_NEW), "f6a58e22");
        // 31 and 33 hex digits are not a uuid.
        assert_eq!(uuid_short("a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5"), format!("{:08x}", fnv1a32(b"a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5")));
        assert_eq!(uuid_short("a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5d0"), format!("{:08x}", fnv1a32(b"a1b2c3d4e5f64a7b8c9d0e1f2a3b4c5d0")));
    }

    /// The Rust side of the parity pin in `src/lib/noteSlug.test.ts`; the
    /// values there are these.
    #[test]
    fn the_short_ids_the_frontend_test_hard_codes() {
        assert_eq!(uuid_short(GMAIL), "a1b2c3d4");
        assert_eq!(uuid_short(PLAIN), "1a2b3c4d");
        assert_eq!(uuid_short(MS), "f3ceba56");
    }

    #[test]
    fn two_notes_on_one_server_get_different_short_ids() {
        assert_ne!(uuid_short(MS_OLD), uuid_short(MS_NEW));
    }

    #[test]
    fn a_link_to_the_older_note_on_a_server_opens_the_older_note() {
        let db = temp_db();
        note(ACCT, MS_OLD).title("Older").modified_ms(1_000).insert(&db);
        note(ACCT, MS_NEW).title("Newer").modified_ms(9_000).insert(&db);
        let old_slug = format!("older-{}", uuid_short(MS_OLD));
        let hit = db.resolve_note_link(ACCT, &old_slug).unwrap();
        assert_eq!(hit.map(|n| n.uuid), Some(MS_OLD.to_string()), "not the newest note on the server");
    }

    /// A Microsoft note is created under a plain placeholder uuid (short id =
    /// its first 8 hex digits) and rekeyed on push to its internetMessageId
    /// (short id = the FNV hash, gotcha #16). A link written before the push
    /// carries the placeholder's id; the alias table must keep it working.
    #[test]
    fn a_link_made_before_the_first_push_still_resolves_after_the_rekey() {
        let db = temp_db();
        linked_pair(&db, PLAIN, "target-1a2b3c4d");
        db.rekey_note_uuid(PLAIN, MS, ACCT).unwrap();
        assert_ne!(uuid_short(MS), uuid_short(PLAIN));

        let hit = db.resolve_note_link(ACCT, "target-1a2b3c4d").unwrap();
        assert_eq!(hit.map(|n| n.uuid), Some(MS.to_string()), "resolve_note_link follows the alias");
        assert_eq!(uuids(&db.outgoing_links(ACCT, "source").unwrap()), vec![MS], "outgoing_links follows the alias");
        assert_eq!(uuids(&db.backlinks(ACCT, MS).unwrap()), vec!["source"], "backlinks follow the alias");
        let orphans = db.list_orphaned_notes(ACCT).unwrap();
        assert!(!uuids(&orphans).contains(&MS), "not Orphaned: {:?}", uuids(&orphans));

        // A link made after the push uses the new id and resolves too.
        assert_eq!(db.resolve_note_link(ACCT, "target-f3ceba56").unwrap().map(|n| n.uuid), Some(MS.to_string()));
    }

    #[test]
    fn an_alias_only_answers_for_the_note_it_forwards_to() {
        let db = temp_db();
        note(ACCT, MS_OLD).title("Rekeyed").insert(&db);
        note(ACCT, MS_NEW).title("Other").insert(&db);
        note(ACCT, "source").title("Source").body("<p>see [[x-1a2b3c4d]]</p>").insert(&db);
        // No alias yet: the placeholder's id names nothing.
        assert!(db.resolve_note_link(ACCT, "x-1a2b3c4d").unwrap().is_none());
        assert!(db.outgoing_links(ACCT, "source").unwrap().is_empty());

        // An alias on another account changes nothing here.
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO note_uuid_aliases (account_id, old_uuid, new_uuid, created_at) VALUES ('microsoft:else@live.com', ?1, ?2, 0)",
                params![PLAIN, MS_OLD],
            )
            .unwrap();
        }
        assert!(db.resolve_note_link(ACCT, "x-1a2b3c4d").unwrap().is_none());
        assert!(db.backlinks(ACCT, MS_OLD).unwrap().is_empty());

        // The placeholder forwards to MS_OLD on this account: only MS_OLD answers for it.
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO note_uuid_aliases (account_id, old_uuid, new_uuid, created_at) VALUES (?1, ?2, ?3, 0)",
                params![ACCT, PLAIN, MS_OLD],
            )
            .unwrap();
        }
        assert_eq!(db.resolve_note_link(ACCT, "x-1a2b3c4d").unwrap().map(|n| n.uuid), Some(MS_OLD.to_string()));
        assert_eq!(uuids(&db.outgoing_links(ACCT, "source").unwrap()), vec![MS_OLD]);
        assert!(db.backlinks(ACCT, MS_NEW).unwrap().is_empty(), "the other note gains no backlink");
        assert!(uuids(&db.list_orphaned_notes(ACCT).unwrap()).contains(&MS_NEW));
    }

    #[test]
    fn renaming_a_rekeyed_note_rewrites_links_made_before_its_push() {
        let db = temp_db();
        note(ACCT, PLAIN).title("Target").insert(&db);
        note(ACCT, "source").title("Source").body("<p>see [[target-1a2b3c4d]]</p>").insert(&db);
        db.rekey_note_uuid(PLAIN, MS, ACCT).unwrap();

        db.apply_local_edit(MS, ACCT, "Renamed", "<p>body</p>", "Notes", &crate::provenance::Actor::test()).unwrap();

        let body = db.get("source", ACCT).unwrap().unwrap().body_html;
        let new_slug = note_slug("Renamed", MS);
        assert!(body.contains(&format!("[[{new_slug}]]")), "{body}");
        assert!(!body.contains("1a2b3c4d"), "{body}");
        assert_eq!(uuids(&db.backlinks(ACCT, MS).unwrap()), vec!["source"]);
    }
}

#[cfg(test)]
mod tests_ask_queries {
    use crate::test_support::{note, temp_db};

    #[test]
    fn subtree_includes_descendants_but_not_prefix_siblings() {
        let db = temp_db();
        let a = "acct@x";
        for label in ["Notes/A", "Notes/A/B", "Notes/A/B/C", "Notes/AB", "Notes/Other"] {
            // uuid doubles as a readable id; "Notes/AB" is the prefix sibling
            // that a bare LIKE 'Notes/A%' would wrongly match.
            note(a, label).label(label).insert(&db);
        }

        let got = db.list_notes_in_subtree(a, "Notes/A").unwrap();
        let mut labels: Vec<&str> = got.iter().map(|n| n.label.as_str()).collect();
        labels.sort();
        assert_eq!(
            labels,
            vec!["Notes/A", "Notes/A/B", "Notes/A/B/C"],
            "Notes/AB is a prefix sibling, not a descendant"
        );
    }

    #[test]
    fn recency_orders_by_epoch_not_by_the_rfc822_date_string() {
        let db = temp_db();
        let a = "acct@x";
        // The trap: lexically "Wed, ..." sorts above "Mon, ...", but Mon is the
        // NEWER note. A query that orders by `date` returns these backwards.
        note(a, "old").modified_ms(1_000).date("Wed, 9 Oct 2013 07:49:21 +0700").insert(&db);
        note(a, "new").modified_ms(9_000).date("Mon, 3 Aug 2020 11:39:07 +0700").insert(&db);

        let got = db.list_recent_notes(Some(a), 10, &[]).unwrap();
        assert_eq!(got[0].uuid, "new", "newest first, by last_remote_modified_at");
        assert_eq!(got[1].uuid, "old");
    }

    #[test]
    fn recency_respects_the_limit_and_spans_accounts_when_unscoped() {
        let db = temp_db();
        note("a1", "x1").modified_ms(5_000).insert(&db);
        note("a2", "x2").modified_ms(6_000).insert(&db);

        assert_eq!(db.list_recent_notes(None, 10, &[]).unwrap().len(), 2);
        assert_eq!(db.list_recent_notes(None, 1, &[]).unwrap().len(), 1);
        assert_eq!(db.list_recent_notes(Some("a1"), 10, &[]).unwrap().len(), 1);
    }

    #[test]
    fn deleted_pending_notes_are_excluded_everywhere() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "keep").label("Notes/A").insert(&db);
        note(a, "gone").label("Notes/A").insert(&db);
        // NOTE the argument order: mark_deleted(uuid, account_id), not the
        // (account_id, uuid) order used by almost every other Db method
        // (db.rs:1736). Verified against the source - do not "fix" it.
        db.mark_deleted("gone", a).expect("mark deleted");

        assert!(db.list_notes_in_subtree(a, "Notes/A").unwrap().iter().all(|n| n.uuid != "gone"));
        assert!(db.list_recent_notes(Some(a), 10, &[]).unwrap().iter().all(|n| n.uuid != "gone"));
        assert_eq!(db.count_notes_in_scope(Some(a), Some("Notes/A"), &[]).unwrap(), 1);
    }

    #[test]
    fn count_in_scope_counts_the_subtree_and_the_account() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "u1").label("Notes/A").insert(&db);
        note(a, "u2").label("Notes/A/B").insert(&db);
        note(a, "u3").label("Notes/Other").insert(&db);

        assert_eq!(db.count_notes_in_scope(Some(a), Some("Notes/A"), &[]).unwrap(), 2);
        assert_eq!(db.count_notes_in_scope(Some(a), None, &[]).unwrap(), 3);
        assert_eq!(db.count_notes_in_scope(None, None, &[]).unwrap(), 3);
    }

    #[test]
    fn search_text_outside_subtree_skips_the_subtree_folder_names_and_deleted() {
        let db = temp_db();
        let a = "acct@x";
        let n = |uuid: &str, label: &str, body: &str| note(a, uuid).title(uuid).body(body).label(label).insert(&db);
        n("in", "Notes/A", "battery");
        n("sub", "Notes/A/B", "battery");
        // A prefix sibling is outside the subtree.
        n("sibling", "Notes/AB", "battery");
        n("out", "Notes/Other", "a battery note");
        // Matches by folder name only: not a mention.
        n("folder-only", "Notes/Battery", "nothing here");
        n("gone", "Notes/Other", "battery");
        db.mark_deleted("gone", a).expect("mark deleted");
        note("other@x", "foreign").body("battery").label("Notes/Other").insert(&db);
        note(a, "titled").title("Battery plan").body("plain").label("Notes/Other").insert(&db);

        let mut got: Vec<String> =
            db.search_text_outside_subtree(a, "Notes/A", "battery", 200).unwrap().into_iter().map(|n| n.uuid).collect();
        got.sort();
        assert_eq!(got, vec!["out", "sibling", "titled"]);
        // The cap applies after the subtree is excluded.
        assert_eq!(db.search_text_outside_subtree(a, "Notes/A", "battery", 1).unwrap().len(), 1);
        // `search_notes` does reach the folder-name-only note: the difference is the point.
        assert!(db.search_notes(Some(a), None, "battery", &[]).unwrap().iter().any(|n| n.uuid == "folder-only"));
    }

    #[test]
    fn note_by_uuid_returns_the_note_and_none_for_absent_or_deleted() {
        let db = temp_db();
        let a = "acct@x";
        note(a, "here").title("Found me").insert(&db);
        note(a, "gone").insert(&db);
        db.mark_deleted("gone", a).expect("mark deleted");

        assert_eq!(db.note_by_uuid(a, "here").unwrap().unwrap().title, "Found me");
        assert!(db.note_by_uuid(a, "gone").unwrap().is_none(), "deleted_pending reads as absent");
        assert!(db.note_by_uuid(a, "never-existed").unwrap().is_none());
    }

    /// list_tasks (spec 4.6(B)) must not scan every note's body_html to find
    /// checkboxes — the pre-filter is mandatory on a 6,655-note/18MB vault —
    /// and the label scope must be the recursive-subtree filter (gotcha #1),
    /// not a bare LIKE prefix that also matches the sibling "Notes/AB".
    #[test]
    fn list_notes_with_checkboxes_prefilters_and_scopes() {
        let db = temp_db();
        let a = "acc";
        db.create_folder_local_new(a, "Notes/A").unwrap();
        // one note with a task, one without, one in a sibling folder
        note(a, "u1").label("Notes/A").body(r#"<div><input type="checkbox">a</div>"#).insert(&db);
        note(a, "u2").label("Notes/A").body("<p>no tasks here</p>").insert(&db);
        note(a, "u3").label("Notes/AB").body(r#"<div><input type="checkbox">c</div>"#).insert(&db);

        let all = db.list_notes_with_checkboxes(a, None).unwrap();
        assert_eq!(all.len(), 2, "only checkbox-bearing notes");

        let scoped = db.list_notes_with_checkboxes(a, Some("Notes/A")).unwrap();
        assert_eq!(scoped.len(), 1, "Notes/AB is a sibling, not a descendant");
        assert_eq!(scoped[0].uuid, "u1");
    }
}

#[cfg(test)]
mod tests_search_or_fallback {
    use crate::test_support::{note, temp_db};

    fn insert_note(db: &crate::db::Db, uuid: &str, account_id: &str, title: &str, body_html: &str, label: &str) {
        note(account_id, uuid).title(title).body(body_html).label(label).insert(db);
    }

    #[test]
    fn multi_term_query_falls_back_to_or_when_phrase_match_is_empty() {
        let db = temp_db();
        insert_note(&db, "u1", "acc", "Password note", "<p>my password is hunter2</p>", "Notes/A");
        insert_note(&db, "u2", "acc", "API note", "<p>the api key is xyz</p>", "Notes/A");
        insert_note(&db, "u3", "acc", "Unrelated", "<p>nothing relevant here</p>", "Notes/A");

        // The exact failure shape observed live: terms combined in one query,
        // never appearing as that literal six-word phrase anywhere.
        let hits = db
            .search_notes(None, None, "password api key token secret auth", &[])
            .unwrap();
        let uuids: Vec<&str> = hits.iter().map(|n| n.uuid.as_str()).collect();
        assert!(uuids.contains(&"u1"), "password hit missing: {uuids:?}");
        assert!(uuids.contains(&"u2"), "api key hit missing: {uuids:?}");
        assert!(!uuids.contains(&"u3"), "unrelated note should not match: {uuids:?}");
    }

    #[test]
    fn phrase_match_that_already_hits_is_not_widened() {
        // A query that DOES match as a literal phrase must return exactly
        // that match, not the (broader, noisier) OR-of-terms result — the
        // fallback must only fire when the strict match is empty.
        let db = temp_db();
        insert_note(&db, "u1", "acc", "Exact", "<p>the quick brown fox</p>", "Notes/A");
        insert_note(&db, "u2", "acc", "Partial", "<p>quick something fox somewhere</p>", "Notes/A");
        let hits = db.search_notes(None, None, "quick brown fox", &[]).unwrap();
        assert_eq!(hits.len(), 1, "phrase match must stay exact when it already hits: {hits:?}");
        assert_eq!(hits[0].uuid, "u1");
    }

    #[test]
    fn single_term_query_is_unaffected() {
        // Single-word queries are already a phrase of one token — the
        // fallback path must not change their (already-correct) behavior.
        let db = temp_db();
        insert_note(&db, "u1", "acc", "T", "<p>credential exposure</p>", "Notes/A");
        let hits = db.search_notes(None, None, "credential", &[]).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn same_uuid_in_two_accounts_both_survive_the_or_fallback() {
        // The notes PK is (uuid, account_id): the SAME uuid legitimately
        // exists in two accounts (CLAUDE.md roadmap #6 — a cross-account
        // move keeps the uuid). A cross-account search (account_id omitted,
        // the MCP server's default) must return BOTH rows.
        //
        // Deduping on `uuid` alone drops the second one silently. Note the
        // two rows differ in body as well as account, so a uuid-keyed
        // HashSet returns exactly one row here while a
        // (uuid, account_id)-keyed one returns two — that is the whole
        // discriminating power of this test, and why both must match the
        // query rather than one being a decoy.
        let db = temp_db();
        let shared = "SHARED-UUID-1234";
        insert_note(&db, shared, "work@x.com", "Work copy", "<p>my password lives here</p>", "Notes/A");
        insert_note(&db, shared, "home@x.com", "Home copy", "<p>the api key lives here</p>", "Notes/A");
        // A third, single-account note guards against the fallback silently
        // not firing at all (which would also return "2 rows" from a plain
        // phrase match if the phrase ever did hit).
        insert_note(&db, "other", "work@x.com", "Other", "<p>nothing relevant here</p>", "Notes/A");

        // Multi-token, matches nothing as a literal phrase → OR fallback.
        let hits = db
            .search_notes(None, None, "password api key token secret auth", &[])
            .unwrap();
        let ids: Vec<(&str, &str)> = hits
            .iter()
            .map(|n| (n.uuid.as_str(), n.account_id.as_str()))
            .collect();
        assert!(
            ids.contains(&(shared, "work@x.com")),
            "work account's copy dropped: {ids:?}"
        );
        assert!(
            ids.contains(&(shared, "home@x.com")),
            "home account's copy dropped — dedup keyed on uuid alone: {ids:?}"
        );
        assert!(!ids.contains(&("other", "work@x.com")), "unrelated note matched: {ids:?}");
    }

    #[test]
    fn or_fallback_still_dedupes_a_row_matched_by_two_tokens() {
        // The counterpart to the test above: widening the dedup key must not
        // stop it deduping. One note containing BOTH tokens is returned by
        // both per-token queries and must still appear exactly once.
        let db = temp_db();
        insert_note(&db, "u1", "acc", "Both", "<p>password and api key together</p>", "Notes/A");
        let hits = db
            .search_notes(None, None, "password api key token secret auth", &[])
            .unwrap();
        assert_eq!(hits.len(), 1, "row matched by several tokens must not repeat: {hits:?}");
    }
}

/// `notes.date` is the raw RFC822 `Date` header string ("Wed, 9 Sep 2020 …"),
/// so `ORDER BY date DESC` sorts lexically by weekday name — "Wed" beats "Thu"
/// and a 2020 note outranks a 2026 one. Every list query must instead order by
/// the epoch-ms `last_remote_modified_at` (falling back to
/// `last_local_modified_at`, which is NOT NULL, for rows created locally and
/// not yet pushed).
#[cfg(test)]
mod tests_chronological_order {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Db {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        Db::open_unencrypted(&path).expect("open temp db")
    }

    // Deliberately adversarial pair: lexically "Wed…2020" > "Thu…2026", so any
    // string sort puts the OLD note first while the epochs say the opposite.
    const OLD_DATE: &str = "Wed, 9 Sep 2020 14:23:15 +0700";
    const OLD_MS: i64 = 1_599_636_195_000;
    const NEW_DATE: &str = "Thu, 4 Jun 2026 01:19:50 +0700";
    const NEW_MS: i64 = 1_780_251_590_000;

    const OLD_UUID: &str = "AAAAAAAA-1111-2222-3333-444444444444";
    const NEW_UUID: &str = "BBBBBBBB-1111-2222-3333-444444444444";

    fn mk_note(
        account_id: &str,
        uuid: &str,
        title: &str,
        body_html: &str,
        date: &str,
        remote_ms: i64,
    ) -> CachedNote {
        CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: title.to_string(),
            body_html: body_html.to_string(),
            date: date.to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 0,
            remote_version: None,
            sync_state: SyncState::Clean,
            last_synced_at: None,
            // Deliberately the INVERSE of the remote order, so a query that
            // accidentally falls back to the local clock still fails the test.
            last_local_modified_at: if remote_ms == OLD_MS { 2 } else { 1 },
            last_remote_modified_at: Some(remote_ms),
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
        push_blocked_reason: None,
        push_blocked_by_remote: false,
        }
    }

    /// Seeds one old + one new note carrying the `zq` marker and a wikilink to
    /// "Target", plus the "Target" note itself.
    fn seed(db: &Db, acct: &str) {
        db.insert_local_new(&mk_note(
            acct,
            OLD_UUID,
            "zq carrier old",
            "<div>zq body [[Target]]</div>",
            OLD_DATE,
            OLD_MS,
        ), &crate::provenance::Actor::test())
        .unwrap();
        db.insert_local_new(&mk_note(
            acct,
            NEW_UUID,
            "zq carrier new",
            "<div>zq body [[Target]]</div>",
            NEW_DATE,
            NEW_MS,
        ), &crate::provenance::Actor::test())
        .unwrap();
    }

    fn uuids(notes: &[CachedNote]) -> Vec<&str> {
        notes.iter().map(|n| n.uuid.as_str()).collect()
    }

    #[test]
    fn search_notes_like_fallback_is_newest_first() {
        let db = temp_db();
        let acct = "test@example.com";
        seed(&db, acct);
        // <3 chars → the LIKE fallback branch, which is the one that sorts by date.
        let hits = db.search_notes(Some(acct), None, "zq", &[]).unwrap();
        assert_eq!(uuids(&hits), vec![NEW_UUID, OLD_UUID]);
    }

    #[test]
    fn search_titles_is_newest_first() {
        let db = temp_db();
        let acct = "test@example.com";
        seed(&db, acct);
        let hits = db.search_titles(acct, "carrier", 10).unwrap();
        let ids: Vec<&str> = hits.iter().map(|(u, _, _)| u.as_str()).collect();
        assert_eq!(ids, vec![NEW_UUID, OLD_UUID]);
    }

    #[test]
    fn list_orphaned_notes_is_newest_first() {
        let db = temp_db();
        let acct = "test@example.com";
        seed(&db, acct);
        // Neither carrier has an incoming mention, so both are orphaned.
        let orphans = db.list_orphaned_notes(acct).unwrap();
        assert_eq!(uuids(&orphans), vec![NEW_UUID, OLD_UUID]);
    }

    #[test]
    fn backlinks_are_newest_first() {
        let db = temp_db();
        let acct = "test@example.com";
        let target_uuid = "CCCCCCCC-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(
            acct,
            target_uuid,
            "Target",
            "<div>target body</div>",
            OLD_DATE,
            OLD_MS,
        ), &crate::provenance::Actor::test())
        .unwrap();
        seed(&db, acct);
        let back = db.backlinks(acct, target_uuid).unwrap();
        assert_eq!(uuids(&back), vec![NEW_UUID, OLD_UUID]);
    }

    #[test]
    fn outgoing_links_are_newest_first() {
        let db = temp_db();
        let acct = "test@example.com";
        // A source note linking to both carriers by plain title.
        let src_uuid = "DDDDDDDD-1111-2222-3333-444444444444";
        seed(&db, acct);
        db.insert_local_new(&mk_note(
            acct,
            src_uuid,
            "Source",
            "<div>[[zq carrier old]] and [[zq carrier new]]</div>",
            NEW_DATE,
            NEW_MS,
        ), &crate::provenance::Actor::test())
        .unwrap();
        let out = db.outgoing_links(acct, src_uuid).unwrap();
        assert_eq!(uuids(&out), vec![NEW_UUID, OLD_UUID]);
    }
}

#[cfg(test)]
mod tests_smart_folders {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Db {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        Db::open_unencrypted(&path).expect("open temp db")
    }

    fn mk_note(account_id: &str, uuid: &str, title: &str, body_html: &str) -> CachedNote {
        CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: title.to_string(),
            body_html: body_html.to_string(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 0,
            remote_version: None,
            sync_state: SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: now_ms(),
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
        push_blocked_reason: None,
        push_blocked_by_remote: false,
        }
    }

    #[test]
    fn orphaned_excludes_linked_notes() {
        let db = temp_db();
        let acct = "test@example.com";
        let linked_uuid = "EEEEEEEE-1111-2222-3333-444444444444"; // uuid8 = "eeeeeeee"
        let orphan_uuid = "FFFFFFFF-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(acct, linked_uuid, "Linked Note", "<div>no links</div>"), &crate::provenance::Actor::test())
            .unwrap();
        db.insert_local_new(&mk_note(acct, orphan_uuid, "Orphan Note", "<div>no links</div>"), &crate::provenance::Actor::test())
            .unwrap();
        // A third note links to "Linked Note" by title.
        db.insert_local_new(&mk_note(
            acct,
            "AAAAAAAA-9999-8888-7777-666666666666",
            "Linker",
            "<div>[[Linked Note]]</div>",
        ), &crate::provenance::Actor::test())
        .unwrap();

        let orphans = db.list_orphaned_notes(acct).unwrap();
        let orphan_uuids: Vec<&str> = orphans.iter().map(|n| n.uuid.as_str()).collect();
        assert!(orphan_uuids.contains(&orphan_uuid), "unlinked note must appear as orphaned");
        assert!(!orphan_uuids.contains(&linked_uuid), "linked note must NOT appear as orphaned");
    }

    #[test]
    fn stale_respects_30_day_threshold() {
        let db = temp_db();
        let acct = "test@example.com";
        let fresh_uuid = "11111111-1111-2222-3333-444444444444";
        let stale_uuid = "22222222-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note(acct, fresh_uuid, "Fresh", "<div>x</div>"), &crate::provenance::Actor::test()).unwrap();
        db.insert_local_new(&mk_note(acct, stale_uuid, "Stale", "<div>x</div>"), &crate::provenance::Actor::test()).unwrap();

        // Backdate the "Stale" note's last_local_modified_at to 31 days ago.
        let thirty_one_days_ago_ms = now_ms() - 31 * 24 * 60 * 60 * 1000;
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "UPDATE notes SET last_local_modified_at = ?1 WHERE account_id = ?2 AND uuid = ?3",
                params![thirty_one_days_ago_ms, acct, stale_uuid],
            )
            .unwrap();
        }

        let stale = db.list_stale_notes(acct).unwrap();
        let stale_uuids: Vec<&str> = stale.iter().map(|n| n.uuid.as_str()).collect();
        assert!(stale_uuids.contains(&stale_uuid), "backdated note must appear as stale");
        assert!(!stale_uuids.contains(&fresh_uuid), "freshly-created note must NOT appear as stale");
    }

    /// The Extracts view is fully virtual, like Orphaned and Stale: it is
    /// derived from the Source block Extract writes, not from a folder, so an
    /// extract filed into any folder still shows up here.
    #[test]
    fn list_extract_notes_follows_the_source_marker_not_the_folder() {
        let db = temp_db();
        let acct = "test@example.com";
        let marker = crate::llm::markdown::SOURCE_MARKER;
        let filed = "11111111-0000-0000-0000-000000000001";
        let plain = "11111111-0000-0000-0000-000000000002";
        let deleted = "11111111-0000-0000-0000-000000000003";

        let mut filed_note = mk_note(
            acct,
            filed,
            "Filed extract",
            &format!("<h2>Point</h2><details>{marker}<pre>src</pre></details>"),
        );
        filed_note.label = "Notes/Trading".to_string();
        db.insert_local_new(&filed_note, &crate::provenance::Actor::test()).unwrap();
        db.insert_local_new(&mk_note(acct, plain, "Plain note", "<div>no source</div>"), &crate::provenance::Actor::test()).unwrap();
        db.insert_local_new(&mk_note(
            acct,
            deleted,
            "Deleted extract",
            &format!("<details>{marker}<pre>src</pre></details>"),
        ), &crate::provenance::Actor::test())
        .unwrap();
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "UPDATE notes SET sync_state = 'deleted_pending' WHERE uuid = ?1",
                params![deleted],
            )
            .unwrap();
        }

        let uuids: Vec<String> = db
            .list_extract_notes(acct)
            .unwrap()
            .into_iter()
            .map(|n| n.uuid)
            .collect();
        assert_eq!(uuids, vec![filed.to_string()]);
    }
}
