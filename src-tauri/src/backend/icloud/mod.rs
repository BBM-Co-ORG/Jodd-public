//! The iCloud vertical — Apple Notes read straight out of CloudKit's private
//! database web service, the one backend that reaches an account with no
//! non-iCloud address attached to Notes.
//!
//! **M1 is read-only, and the reason is not caution about the wire.** The
//! content model is only partly decoded — `attribute_run` (bold, links,
//! checklists, embeds) is M2's — so a write would round-trip a note through a
//! representation that does not hold everything it arrived with. A backend
//! that cannot write can never propagate that loss back.
//!
//! Every write method therefore returns [`TransportError::Permanent`] naming
//! M2, and `Capabilities::for_backend(ICloud)` has all three `Writes` fields
//! `false` so the command layer and the UI refuse before a call ever reaches
//! one.
//!
//! Design:
//! `docs/superpowers/specs/2026-08-21-icloud-vertical-m1-design.md`.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::backend::{
    Attachment, Capabilities, ContentKind, DedupSummary, Derived, Deriver, Identity, MessageIndex,
    Note, NoteStore, RemoteFolder, SaveOp, SavedNote, TransportError, TrashedNote, Vertical,
};
use crate::icloud_auth::{cookie_header_for, CookieSource, IcloudSession};

pub mod census;
pub mod compose;
mod crdt;
pub mod doc;
pub mod format;
pub mod format_html;
pub mod format_reconcile;
mod gen;
pub mod transport;
pub mod wire;

/// A write this backend does not offer, and why.
///
/// **Not "not built yet".** M1's `milestone_2()` said that about everything,
/// and it was true then; M2 writes notes, so a caller reaching one of these
/// needs the actual reason rather than a milestone number that has passed.
/// Each call site names its own — folder writes and the pin are unmeasured
/// (M2 spec, Component O), restore has nowhere to restore *to*.
pub(crate) fn unsupported(what: &str) -> TransportError {
    TransportError::Permanent { source: anyhow::anyhow!("iCloud: {what}") }
}

/// A note this particular record cannot accept a write to.
///
/// Reaches the user through `push_one_dirty` → `notes.push_blocked_reason`
/// (gotcha #14): the row keeps its edit, stops retrying, and says why.
pub(crate) fn refused(u: compose::Unwritable) -> TransportError {
    TransportError::Permanent { source: anyhow::anyhow!("{u}") }
}

/// A hard stop on the zone walk.
///
/// `changes/zone` pages on `moreComing`, which is server-controlled: a server
/// that never clears the flag would page forever inside one UI action. 200
/// pages is far past any real account (776 notes arrived in a handful) and
/// still terminates.
const MAX_PAGES: usize = 200;

/// One full read of the Notes zone, decoded.
///
/// `Clone` so a write can be folded into the cached copy rather than dropping
/// it — see [`AccountCache::apply`].
#[derive(Clone)]
pub struct Scan {
    pub notes: Vec<Note>,
    pub folders: Vec<RemoteFolder>,
    /// The resume token this walk ended on. Held for the instance only — see
    /// [`ICloudVertical::scan`]; persisting it to `accounts.sync_cursor` is M2's.
    pub sync_token: Option<String>,
    pub tally: DecodeTally,
    /// Notes in Apple's Recently Deleted — recoverable, and excluded from
    /// `notes` on purpose.
    ///
    /// A trashed record's `Folder` names the Trash, so the decode has no real
    /// path to give it and uses the root as a placeholder. Putting these in
    /// `notes` would therefore show every deleted note in the root folder.
    pub trashed: Vec<Note>,
    /// Did this walk reach the end of the zone?
    ///
    /// **False makes an absent note "not read yet", never "not there".** A
    /// missing record is how `push_one_dirty` learns a note was deleted on
    /// another device, and its answer is to DROP the local row along with the
    /// edit it was holding (`not_found_means_deleted_remotely`). That is right
    /// when the zone was read to the end and the note genuinely is not in it;
    /// it is silent data loss when the walk stopped early at the page cap or on
    /// a `moreComing` it could not resume from.
    pub complete: bool,
    /// What a write needs from the record it would replace, by `recordName`.
    ///
    /// The zone is the unit of read on this backend and there is no per-record
    /// endpoint, so a push cannot go and fetch the note it is about to
    /// overwrite — it would cost a whole second walk per keystroke-batch. The
    /// walk keeps the few bytes a write needs instead, which is also what
    /// makes the write gate run against the **remote's own current document**
    /// rather than against the edit.
    pub bases: HashMap<String, WriteBase>,
    /// Every live `InlineAttachment` record, `recordName -> ref` — what a
    /// `U+FFFC` inline object (a hashtag, most importantly) resolves through
    /// on both the read and the write gate (M3 F5).
    pub inline_refs: HashMap<String, wire::InlineRef>,
}

/// How many times a single content push will re-read the record's current
/// `recordChangeTag` and retry after a self-conflict before giving up and
/// surfacing the `Conflict` for the poll's keep-both path.
///
/// One is almost always enough — Apple's post-create server bump moves the tag
/// exactly once — but the read-after-write window can carry more than one bump,
/// and each refresh is a single cheap `records/lookup`, so a small bound trades
/// a few point reads for not spinning on a stale lock for up to `POLL_MS`.
const MAX_CONFLICT_REFRESHES: u8 = 3;

/// Whether the note's CONTENT changed between the base a write was built from
/// and a freshly looked-up copy of the same record — the gate that tells a
/// stale optimistic lock (Apple bumped only the `recordChangeTag`) apart from
/// a genuine concurrent edit by another device.
///
/// Compares the DECODED text, not the raw bytes: Apple re-serves a record's
/// document in whatever container its last writer used (gzip *or* zlib, gotcha
/// #20) and may re-order or re-stamp CRDT identity without touching a
/// character, so a byte comparison would read Apple's own re-encoding as a
/// concurrent edit and refuse every safe retry. A document that fails to
/// decode on either side is treated as "differs" — the conservative answer,
/// which leaves the write a `Conflict` rather than overwriting something this
/// code could not read.
fn icloud_content_differs(base: &WriteBase, fresh: &WriteBase) -> bool {
    match (doc::decode_note_text(&base.document), doc::decode_note_text(&fresh.document)) {
        (Ok(a), Ok(b)) => a != b,
        _ => true,
    }
}

/// A string as a log line can carry it without lying: the first `n` chars
/// quoted (control characters escaped), the char count, and the code point
/// of each of those chars — so a stray combining mark, a `U+2028`, or a
/// `U+FFFC` is visible in the log rather than rendered away by the terminal.
/// Never the whole text: a note body is the user's, and one line is enough
/// to tell "sent the old head" from "sent the new head".
pub(crate) fn head_codepoints(s: &str, n: usize) -> String {
    let head: String = s.chars().take(n).collect();
    let points: Vec<String> = head.chars().map(|c| format!("{:04X}", c as u32)).collect();
    let more = if s.chars().count() > n { "…" } else { "" };
    format!(
        "{:?}{more} ({} chars) [{}]",
        head,
        s.chars().count(),
        points.join(" ")
    )
}

/// The record a write is replacing, reduced to what M2 needs of it.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteBase {
    /// `TextDataEncrypted`, base64-decoded: the compressed document itself, so
    /// the round-trip guard and the attribute-run splice both run against what
    /// the server actually holds.
    pub document: Vec<u8>,
    /// `recordChangeTag` — the optimistic lock the next write sends back.
    pub change_tag: String,
    /// `CreationDate` in epoch ms, carried so an edit never rewrites it.
    pub created_ms: Option<i64>,
    /// The `Folder` reference's `recordName`.
    pub folder_id: String,
    /// `TitleEncrypted` as it stands — the **verifier**, never the cut key
    /// (gotcha #21).
    pub title_field: String,
    /// A `PasswordProtectedNote`, not a `Note`.
    ///
    /// **The write refusal keys on this**, which is Component H3's amendment
    /// stated as a field: the remote record is a different record TYPE, and a
    /// guard that compared the cached body against `wire::LOCKED_BODY_HTML`
    /// would pass the moment the user edited the placeholder.
    pub locked: bool,
    /// Raw inner values of the fields an update echoes back verbatim
    /// (`wire::write_base` collects them; `wire::modify_note_body` sends
    /// them). Carrying them here is what lets the write conform to the
    /// captured client's field set without a per-push `records/lookup`.
    pub echo: serde_json::Map<String, serde_json::Value>,
}

/// What the decode made of the records it saw.
///
/// **Component H (the Advanced Data Protection gate) consumes this and derives
/// nothing of its own** — that is the whole point of keeping the counts apart
/// here. `unreadable` is the ADP shape: bytes that are not a compressed stream
/// at all. `malformed` decompressed and then failed to parse, which is a bug or
/// a schema change and must never count toward an ADP verdict; `incomplete` is
/// a structurally broken record and must not either. Collapsing any of the
/// three would let Jodd tell a user with a perfectly readable account that
/// their notes are end-to-end encrypted — an accusation they cannot act on
/// (gotcha #20).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DecodeTally {
    pub decoded: usize,
    pub unreadable: usize,
    pub malformed: usize,
    pub incomplete: usize,
    /// `Deleted == 1`, or filed in Trash. Not a failure of any kind.
    pub deleted: usize,
    /// Records sitting in Recently Deleted. Counted apart from `deleted`
    /// because one is recoverable and the other is a tombstone.
    pub trashed: usize,
    /// Notes the user locked with a password.
    ///
    /// A locked note is a **`PasswordProtectedNote` record**, a different
    /// record type from `Note`, and its body is encrypted with a key Jodd does
    /// not have and should not want. So it cannot be read — but it must not
    /// vanish either, which is what it did: a folder Apple showed with two
    /// notes showed one here, with nothing anywhere to explain the difference.
    ///
    /// **Counted AND shown** — with its real title, in its real folder, and a
    /// body that says why it is blank. Measured 2026-08-23: the record carries
    /// the same fields as a `Note` and its title is base64 of plain text, so
    /// only the body is genuinely out of reach.
    ///
    /// Skipping it was the original behaviour and it is what made this a bug:
    /// Apple showed a folder with two notes, Jodd showed one, and nothing
    /// anywhere explained the difference. The body is a sentence rather than
    /// empty because an empty body is gotcha #17's landmine — but note that
    /// the danger there is *silent* emptiness a later write pushes back, and
    /// this is the opposite of silent. See `wire::decode_locked_note` for the
    /// M2 obligation that comes with it.
    pub locked: usize,
    /// Notes whose `Folder` reference matched no folder record, so they were
    /// filed under the root.
    ///
    /// **Not an error, and not nothing.** Filing an orphan under the root is
    /// the right call — the note stays visible and the next sync repairs the
    /// path — but it is indistinguishable in the result from a note that
    /// genuinely lives in the root. Counting it is what turns "the root has
    /// four more notes than Apple shows" from a mystery into a number.
    pub orphaned: usize,
    /// Notes that reached the root carrying **no `Folder` reference at all**,
    /// as opposed to one naming a folder this walk never saw.
    ///
    /// **This is the gap `orphaned` alone left open, and it is the shape the
    /// open discrepancy has.** `decode_note` resolves placement with
    /// `unwrap_or(DEFAULT_FOLDER)`, so an absent field and an explicit "this
    /// note is in the root" produce the identical result. The orphan check
    /// only ever fired on a *present* reference, so a note with no field was
    /// counted as neither — invisible in every number the walk reports. A
    /// live account showing four more notes in the root than Apple does, with
    /// `orphaned = 0`, is exactly what that blind spot looks like from
    /// outside, which is why the count exists before the explanation does.
    ///
    /// Deliberately **not** an error and not a skip: filing it under the root
    /// keeps the note visible, which is the right call whatever the cause
    /// turns out to be. Only the silence was wrong.
    pub unfiled: usize,
}

/// Whether this account's notes can be read at all.
///
/// **Component H.** With Advanced Data Protection on, `TextDataEncrypted` is
/// genuinely end-to-end encrypted and there is no readable content at any
/// price. That is a "this account cannot work" state, not an error — and the
/// whole difficulty is telling it apart from the states that merely *look* the
/// same from outside.
///
/// **The verdict is evidence-shaped, not field-shaped.** Nobody has an ADP
/// account to test against, so it must not depend on a field nobody has seen.
/// It reads [`DecodeTally`] and re-derives nothing: `icloud/doc.rs` already
/// split its failures by what a caller should do about them, and this consumes
/// that split. The first draft of this check carried its own copy of "the
/// container is zlib" — and that copy was wrong in the direction that refuses
/// working accounts, on 774 of 776 real notes (gotcha #20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdpVerdict {
    /// At least one note decoded. Nothing is blocked.
    Readable,
    /// No note records at all — a brand-new or emptied account.
    ///
    /// **This is the trap the three-way split exists for.** An empty account
    /// decodes nothing either, and reporting that as ADP is a false accusation
    /// the user cannot act on.
    NoNotes,
    /// Records exist, none decoded, and the failures are the ADP shape.
    /// **The only verdict that blocks.**
    Unreadable { records: usize },
    /// Records exist and none decoded, but nothing failed the way ADP content
    /// fails — every failure was a `Malformed` document or a structurally
    /// broken record.
    ///
    /// **Not in the design spec's three-way table, and added deliberately.**
    /// That table said "n note records, 0 decoded → Unreadable", which counts
    /// a `Malformed` note toward the verdict — the exact thing H1's own prose
    /// forbids two paragraphs later. Following the table literally would block
    /// an account over a bug in Jodd's decoder or a schema change Apple
    /// shipped, which is the false accusation this component exists to
    /// prevent. So it proceeds, loudly.
    Inconclusive { malformed: usize, incomplete: usize },
}

impl AdpVerdict {
    /// Reads the tally. The one place the rule lives.
    pub fn of(t: &DecodeTally) -> AdpVerdict {
        if t.decoded > 0 {
            return AdpVerdict::Readable;
        }
        if t.unreadable > 0 {
            return AdpVerdict::Unreadable { records: t.unreadable };
        }
        if t.malformed > 0 || t.incomplete > 0 {
            return AdpVerdict::Inconclusive { malformed: t.malformed, incomplete: t.incomplete };
        }
        // Deleted records are tombstones, so an account whose only notes are
        // in the Trash genuinely has no notes to read.
        AdpVerdict::NoNotes
    }

    /// Whether an account may exist at all. **Exactly one verdict blocks.**
    ///
    /// A predicate rather than three call sites matching on the enum: the
    /// sign-in gate and the runtime banner must agree about what "blocked"
    /// means, and two independent matches are two chances to disagree.
    pub fn blocks_account(&self) -> bool {
        matches!(self, AdpVerdict::Unreadable { .. })
    }

    /// What to show the user. `None` when nothing is wrong.
    ///
    /// Names Advanced Data Protection explicitly and says what it means,
    /// because the user's only possible action lives in Apple's settings, not
    /// in Jodd. A generic "could not read this account" would leave them
    /// nowhere to go.
    pub fn blocked_reason(&self) -> Option<String> {
        match self {
            AdpVerdict::Unreadable { records } => Some(format!(
                "This Apple ID has Advanced Data Protection turned on, so its {records} note(s) \
                 are end-to-end encrypted and Jodd cannot read them. Only Apple's own apps can. \
                 Turning ADP off (Settings → Apple Account → iCloud → Advanced Data Protection) \
                 would let Jodd read this account."
            )),
            _ => None,
        }
    }
}

/// One account's shared session and zone read.
///
/// **CloudKit's unit of read is the whole zone — there is no per-folder
/// endpoint** — and a `Vertical` is constructed per operation. Without this,
/// the UI's 2500 ms folder sweep would walk the entire zone once *per folder*:
/// on a real account (102 folders, 776 notes) that is 102 whole-zone reads in
/// about four minutes, every session, against Apple's private API. Microsoft
/// escapes the same shape with a scoped `GET /mailFolders/{id}/messages`; this
/// backend has no such request to make, so the sharing has to happen here.
///
/// It is also why one walk is *enough*: a zone read returns every note in every
/// folder, so the first one hydrates the whole account and each later call just
/// filters it by label.
///
/// Explicit refresh calls [`AccountCache::invalidate`] first — that is the ⟳
/// button's whole job, and a cache no user action can clear is a bug waiting
/// for a support ticket. [`AccountCache::MAX_AGE`] is the backstop for the paths
/// that forget.
#[derive(Default)]
pub struct AccountCache {
    /// The `/validate` bootstrap.
    ///
    /// **Cached for the same reason the scan is, one layer up.** `vertical_for`
    /// builds a vertical per operation and each one has to establish a session
    /// before it can do anything, so the folder sweep would `POST /validate`
    /// every 2500 ms — about a hundred calls during one sweep. Nothing in it is
    /// a secret: the Apple ID, the dsid, the partition host and the client
    /// version strings, all of which Apple just told us.
    session: tokio::sync::Mutex<Option<(std::time::Instant, IcloudSession)>>,
    // A tokio mutex, not a std one: it is held across the zone walk, which is
    // an await point. A std guard is not Send and would not compile here —
    // which is the honest signal that this lock has async work under it.
    scan: tokio::sync::Mutex<Option<(std::time::Instant, std::sync::Arc<Scan>)>>,
}

impl AccountCache {
    /// How stale a shared scan may be before it is walked again.
    ///
    /// Long enough that one folder sweep (2500 ms × however many folders)
    /// reuses a single walk, short enough that a cache nobody invalidated
    /// still corrects itself. The local SQLite replica — not this — is what
    /// the UI actually reads, so staleness here costs a delayed refresh, never
    /// a wrong screen.
    pub const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(300);

    /// The account's session, bootstrapping one if there is none fresh enough.
    ///
    /// `/validate` stays the first call of any session — this caches its
    /// *result*, not the session itself, which still lives only in the
    /// webview's cookie jar (gotcha #19).
    pub async fn session(
        &self,
        cookies: &dyn CookieSource,
    ) -> Result<IcloudSession, TransportError> {
        let mut slot = self.session.lock().await;
        if let Some((at, s)) = slot.as_ref() {
            if at.elapsed() < Self::MAX_AGE {
                return Ok(s.clone());
            }
        }
        let fresh = crate::icloud_auth::establish(cookies).await?;
        *slot = Some((std::time::Instant::now(), fresh.clone()));
        Ok(fresh)
    }

    /// Drops everything cached for this account.
    ///
    /// Both halves together, always. A refresh that re-walked the zone against
    /// a stale partition host would fail in a way that looks like a dead
    /// session, and the session is the cheaper of the two to re-establish.
    ///
    /// **Never call this from inside a zone walk.** `ICloudVertical::scan`
    /// holds the scan lock across the walk on purpose, so that two verticals
    /// cannot both walk — and a tokio mutex is not reentrant. Taking it again
    /// from under the walk deadlocks the read with no error, no panic and no
    /// timeout; it simply never returns. Use [`AccountCache::invalidate_session`]
    /// there, which touches the other lock. (Found exactly this way: the 421
    /// test stopped finishing.)
    pub async fn invalidate(&self) {
        *self.session.lock().await = None;
        *self.scan.lock().await = None;
    }

    /// Drops only the `/validate` bootstrap.
    ///
    /// Exists for the one caller that runs *inside* a zone walk: a 421 means
    /// the session is over, so the cached bootstrap describes something that
    /// no longer exists — but the scan lock is already held by the walk, and
    /// [`AccountCache::invalidate`] would deadlock on it.
    pub async fn invalidate_session(&self) {
        *self.session.lock().await = None;
    }

    /// Folds a change Jodd itself just made into the cached zone read.
    ///
    /// **The alternative — dropping the cache after every write — is a
    /// performance bug with a correctness bug behind it.** A read on this
    /// backend filters one whole-zone walk, and `save_note_full` takes a scan
    /// before it writes: drop the cache on each success and the worker
    /// draining five dirty notes performs five whole-zone reads, back to back,
    /// against Apple's private API. Keeping a STALE cache is worse still — the
    /// 2500 ms folder sweep renders it, so the user watches their own edit
    /// revert.
    ///
    /// Folding is the third answer, and it is the one the write result already
    /// pays for: the server said what the record now is, so the cache can be
    /// told rather than asked.
    ///
    /// The timestamp is deliberately **not** refreshed. A write is not a read,
    /// and letting one extend `MAX_AGE` would let a busy editor hold a cache
    /// past the age at which it is meant to be re-walked.
    pub async fn apply(&self, f: impl FnOnce(&mut Scan)) {
        let mut slot = self.scan.lock().await;
        if let Some((_, scan)) = slot.as_mut() {
            f(std::sync::Arc::make_mut(scan));
        }
    }
}

pub struct ICloudVertical {
    pub(crate) session: IcloudSession,
    cookies: std::sync::Arc<dyn CookieSource>,
    account_id: String,
    capabilities: Capabilities,
    /// The account's shared zone read — see [`AccountCache`].
    ///
    /// Shared rather than per-instance because a vertical is constructed per
    /// operation and the zone is the unit of read: a per-instance cache would
    /// make the folder sweep walk the whole account once per folder.
    scans: std::sync::Arc<AccountCache>,
    /// What the last walk this instance observed decided, for the synchronous
    /// [`Vertical::blocked_reason`].
    ///
    /// A separate copy rather than a peek into `scans`: that lock is async, and
    /// `blocked_reason` is a getter on a path that must not block or fetch.
    seen_tally: std::sync::Mutex<Option<DecodeTally>>,
    /// This account's durable CRDT replica identity (M2.5), minted once by
    /// `ensure_icloud_replica_id` and carried in here so `save_note_full` has
    /// it without a second `AppState` lookup. Unused by any read-only
    /// operation — only `save_note_full`'s CRDT-writable branch reads it.
    replica_id: [u8; 16],
}

/// Counts one record's field NAMES into the census.
///
/// Names only. The whole value of this diagnostic is that it can run over a
/// real account without a character of note text reaching the log.
fn census_fields(
    r: &serde_json::Value,
    into: &mut HashMap<String, usize>,
    total: &mut usize,
) {
    *total += 1;
    if let Some(fields) = r["fields"].as_object() {
        for k in fields.keys() {
            *into.entry(k.clone()).or_default() += 1;
        }
    }
}

/// Describes a record's `Folders` (plural) field for the log, without ever
/// printing content.
///
/// `Folders` sits in `DESIRED_KEYS` because the web client asks for it, comes
/// back on real records, and is read by nothing. That makes it the first place
/// to look when a note has no singular `Folder` — but the question is only
/// "does it hold a reference, and to what record", so this yields record names
/// and shapes and never a value that could carry note text.
fn describe_folders_field(v: &serde_json::Value) -> String {
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
    // Present but a shape nothing here anticipated — say so rather than
    // guessing, and name the JSON kind only.
    format!("present, unrecognised shape ({})", json_kind(inner))
}

/// The JSON type name, for a diagnostic that must not print the value.
fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

impl ICloudVertical {
    pub fn new(
        session: IcloudSession,
        cookies: std::sync::Arc<dyn CookieSource>,
        account_id: String,
        scans: std::sync::Arc<AccountCache>,
        replica_id: [u8; 16],
    ) -> Self {
        Self {
            session,
            cookies,
            account_id,
            capabilities: Capabilities::for_backend(crate::accounts::BackendKind::ICloud),
            scans,
            seen_tally: std::sync::Mutex::new(None),
            replica_id,
        }
    }

    /// The `Cookie:` header for one CloudKit request, harvested fresh.
    ///
    /// **Harvested per burst and never stored** (gotcha #19): the live webview
    /// rotates its own cookies, and a captured copy measured dead inside 3.5
    /// hours. A harvest that fails means the session is gone, which is
    /// [`TransportError::Auth`] — the same thing a 421 means, and the same
    /// thing the revival path (B4) exists to answer. Calling it `Transient`
    /// would make the worker retry a webview that is not coming back.
    async fn cookie_header(&self, host: &str, path: &str) -> Result<String, TransportError> {
        let jar = self.cookies.harvest().await.map_err(|e| {
            crate::log!("icloud: cookie harvest failed: {e}");
            TransportError::Auth
        })?;
        let header = cookie_header_for(host, path, &jar);
        if header.is_empty() {
            // An empty header is not "no cookies to send" — it is a jar with
            // nothing that matches CloudKit's host, i.e. no session. Sending it
            // would produce a 421 whose cause looks like an expired session
            // rather than an empty harvest.
            crate::log!("icloud: harvested jar has nothing scoped to {host}");
            return Err(TransportError::Auth);
        }
        Ok(header)
    }

    fn ck_hostname(&self) -> String {
        // The partition host comes from `/validate` and is a full URL. Cookie
        // scoping is decided by HOST, so the host is what matters here; a
        // parse failure falls back to the bare domain, which every one of
        // Apple's session cookies is scoped to anyway.
        reqwest::Url::parse(&self.session.ck_host)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_else(|| "icloud.com".to_string())
    }

    /// The M2 write census over this account's own zone read.
    ///
    /// Deliberately NOT served from the cached scan: the census needs the raw
    /// records, and the scan keeps only what the decode chose to keep. One
    /// extra whole-zone read for an explicit diagnostic is the same price
    /// `fetch_note` pays, and this is not on any hot path.
    pub async fn write_census(&self) -> Result<String, TransportError> {
        let (records, _, complete) = self.fetch_all_records().await?;
        let mut out = census::report(&records);
        if !complete {
            out.push_str(
                "\n⚠ the zone walk did not reach the end, so every count above is a \
                 LOWER BOUND\n",
            );
        }
        Ok(out)
    }

    /// **The live write self-test** — the one thing no test here can confirm.
    ///
    /// `records/modify`'s request shape is the part of M2 that only Apple can
    /// judge: every fixture in this repo agrees with the code that wrote it.
    /// This runs the real sequence against the real account, on a scratch note
    /// it creates itself, using the session the app is already holding.
    ///
    /// # Containment, enforced rather than advised
    ///
    /// - The `recordName` is **minted here** and never taken from a caller, so
    ///   there is no argument that could point this at somebody's note.
    /// - Every write asserts the record it is about to touch is that one.
    /// - The destination folder must already exist and be named explicitly;
    ///   nothing is created, and there is no default that could land a note in
    ///   the user's root.
    /// - It cleans up after itself, and says so loudly when it cannot.
    ///
    /// # The step that matters
    ///
    /// Step 4 re-sends a **superseded `recordChangeTag`**. If the server
    /// refuses it, the optimistic lock is real and the keep-both conflict model
    /// works on this backend. If the server ACCEPTS it, `records/modify` is
    /// last-writer-wins, two devices editing one note silently lose an edit,
    /// and the conflict story needs redesigning — better learned from a scratch
    /// note than from an account.
    ///
    /// Each verification re-reads the whole zone, because this backend has no
    /// per-record endpoint. On a large account that is minutes, not seconds.
    pub async fn write_selftest(&self, folder_path: &str) -> Result<String, TransportError> {
        use std::fmt::Write as _;
        let mut out = String::new();
        let scan = self.scan().await?;
        let folder = self.folder_id_for(&scan, folder_path)?;
        drop(scan);

        // Minted, never accepted. This is the containment rule as code rather
        // than a comment: no caller can name the record this touches.
        let record = self.mint();
        let _ = writeln!(out, "● scratch recordName: {record}");
        let _ = writeln!(out, "● destination: {folder_path}");

        let title = format!("Jodd write self-test {}", &record[..8]);
        let text = compose::compose_new(&title, "first line\nsecond line");
        let now = chrono::Utc::now().timestamp_millis();

        // ── 1. create ───────────────────────────────────────────────────
        let created = self
            .modify(&wire::modify_note_body(&wire::NoteWrite {
                display_text: None,
                record_name: record.clone(),
                change_tag: None,
                text: text.clone(),
                document: compose::encode(&compose::NoteDocument::new_with_replica(
                    &text,
                    self.replica_id,
                )),
                folder: folder.clone(),
                created_ms: Some(now),
                modified_ms: now,
                echo: serde_json::Map::new(),
            }))
            .await?;
        let _ = writeln!(out, "✓ 1. created — changeTag {}", created.change_tag);

        // ── 2. read it back ─────────────────────────────────────────────
        let (base, read_title) = self.selftest_read(&record).await?;
        let parsed = compose::writability(&base.document, &read_title).map_err(|u| {
            TransportError::Permanent {
                source: anyhow::anyhow!("the note Jodd just wrote is not writable by Jodd: {u}"),
            }
        })?;
        if parsed.text() != text {
            return Err(TransportError::Permanent {
                source: anyhow::anyhow!(
                    "round trip lost content: sent {} char(s), read back {}",
                    text.chars().count(),
                    parsed.text().chars().count()
                ),
            });
        }
        let _ = writeln!(
            out,
            "✓ 2. read back — folder matches: {}, text survived exactly",
            base.folder_id == folder
        );

        // ── 3. edit, with the lock ──────────────────────────────────────
        let edited = compose::recompose(
            parsed.text(),
            &read_title,
            &read_title,
            "first line\nsecond line\nthird line",
        );
        let updated = self
            .modify(&wire::modify_note_body(&wire::NoteWrite {
                display_text: None,
                record_name: record.clone(),
                change_tag: Some(base.change_tag.clone()),
                text: edited.clone(),
                document: compose::encode(&parsed.with_text(&edited)),
                folder: folder.clone(),
                created_ms: base.created_ms,
                modified_ms: chrono::Utc::now().timestamp_millis(),
                echo: base.echo.clone(),
            }))
            .await?;
        let _ = writeln!(
            out,
            "✓ 3. edited — changeTag {} → {}",
            base.change_tag, updated.change_tag
        );
        let (after_edit, _) = self.selftest_read(&record).await?;
        let after = compose::parse(&after_edit.document)
            .map_err(|e| TransportError::Permanent { source: anyhow::anyhow!("{e}") })?;
        if after.text() != edited {
            return Err(TransportError::Permanent {
                source: anyhow::anyhow!("the edit did not land as sent"),
            });
        }
        let _ = writeln!(out, "   the edit landed exactly");

        // ── 4. the optimistic lock ──────────────────────────────────────
        let stale = self
            .modify(&wire::modify_note_body(&wire::NoteWrite {
                display_text: None,
                record_name: record.clone(),
                // Deliberately the tag step 3 superseded.
                change_tag: Some(base.change_tag.clone()),
                text: format!("{edited}\nmust not land"),
                document: compose::encode(&after.with_text(&format!("{edited}\nmust not land"))),
                folder: folder.clone(),
                created_ms: base.created_ms,
                modified_ms: chrono::Utc::now().timestamp_millis(),
                echo: base.echo.clone(),
            }))
            .await;
        match stale {
            Err(TransportError::Conflict { .. }) => {
                let _ = writeln!(
                    out,
                    "✓ 4. a stale changeTag was REFUSED — the optimistic lock is real"
                );
            }
            Err(e) => {
                let _ = writeln!(
                    out,
                    "△ 4. a stale changeTag was refused, but not as a conflict: {e}"
                );
            }
            Ok(_) => {
                let _ = writeln!(
                    out,
                    "✗ 4. a stale changeTag was ACCEPTED — the server does not re-check it,\n                          so records/modify is last-writer-wins and the conflict model needs\n                          redesigning"
                );
            }
        }

        // ── 5. delete, which is also the cleanup ────────────────────────
        let (latest, _) = self.selftest_read(&record).await?;
        match self
            .modify(&wire::delete_note_body(&record, Some(&latest.change_tag), chrono::Utc::now().timestamp_millis()))
            .await
        {
            Ok(_) => {
                let _ = writeln!(
                    out,
                    "✓ 5. filed in Recently Deleted — check Apple Notes; it should be there"
                );
            }
            Err(e) => {
                let _ = writeln!(
                    out,
                    "✗ 5. delete failed: {e}\n                          The scratch note {record} is still in {folder_path} — remove it by hand."
                );
            }
        }

        let _ = writeln!(
            out,
            "\nConfirm on the iPhone before believing any of it: a record CloudKit\n             accepted is not the same claim as a note Apple Notes displays."
        );
        Ok(out)
    }

    /// **The content self-test** — proves the CRDT engine (M2.5) against a
    /// REAL note, not a scratch one.
    ///
    /// `write_selftest` above proves the write plumbing (create/edit/lock/
    /// delete) but only ever exercises the OPAQUE path: a note it creates via
    /// `NoteDocument::new` never carries CRDT identity, since only a replica
    /// (Apple's own client) can mint the `substring`/`timestamp` fields that
    /// make a document CRDT-writable. Testing the CRDT engine therefore means
    /// testing against a note that already has that identity — there is no
    /// safe scratch equivalent — so this method targets an EXISTING record by
    /// name, appends a marker, verifies it landed byte-exact, then reverts to
    /// the original text and verifies that too. Net effect on the note: none.
    /// Two real writes happen along the way.
    ///
    /// Refuses outright if the note is not CRDT-writable (`writability()`
    /// returns `Err`, or returns `Ok` with `crdt: None` — an ordinary
    /// opaque-path note has nothing this test needs to prove that
    /// `write_selftest` doesn't already cover).
    pub async fn content_write_selftest(&self, record_name: &str) -> Result<String, TransportError> {
        use std::fmt::Write as _;
        let mut out = String::new();

        // ── 0. read and confirm this note is CRDT-writable ──────────────
        let (base, title) = self.selftest_read(record_name).await?;
        let parsed = compose::writability(&base.document, &title).map_err(|u| {
            TransportError::Permanent {
                source: anyhow::anyhow!("record {record_name} is not writable: {u}"),
            }
        })?;
        if parsed.crdt.is_none() {
            return Err(TransportError::Permanent {
                source: anyhow::anyhow!(
                    "record {record_name} carries no CRDT identity — nothing for this test to \
                     prove; use write_selftest for the opaque path instead"
                ),
            });
        }
        let original_text = parsed.text().to_string();
        let _ = writeln!(out, "● record: {record_name}");
        let _ = writeln!(out, "● original text: {original_text:?}");

        // ── 1. insert a marker MID-TEXT via the CRDT engine ─────────────
        // Before the last character, not appended: an append only ever
        // extends or adds a trailing run, while a mid-text insert lands
        // INSIDE an existing run — the shape that used to be a split and,
        // as of the 2026-08-26 no-split strategy, is a whole-run rewrite
        // (tombstone + reinsert; see `crdt::apply_text_edit`). This is the
        // exact edit shape whose split form made Apple's clients duplicate
        // the note, so it is the shape this selftest must keep exercising.
        let marker = " [Jodd CRDT test — reverting]";
        let cut = original_text
            .char_indices()
            .next_back()
            .map(|(byte, _)| byte)
            .unwrap_or(0);
        let edited_text =
            format!("{}{marker}{}", &original_text[..cut], &original_text[cut..]);
        let edited = parsed.with_text_crdt(&edited_text, self.replica_id).map_err(|e| {
            TransportError::Permanent { source: anyhow::anyhow!("with_text_crdt refused the marker edit: {e}") }
        })?;
        let pushed = self
            .modify(&wire::modify_note_body(&wire::NoteWrite {
                display_text: None,
                record_name: record_name.to_string(),
                change_tag: Some(base.change_tag.clone()),
                text: edited_text.clone(),
                document: compose::encode(&edited),
                folder: base.folder_id.clone(),
                created_ms: base.created_ms,
                modified_ms: chrono::Utc::now().timestamp_millis(),
                echo: base.echo.clone(),
            }))
            .await?;
        let _ = writeln!(out, "✓ 1. marker pushed — changeTag {} → {}", base.change_tag, pushed.change_tag);

        // ── 2. read back and verify it landed byte-exact ────────────────
        let (after_marker, _) = self.selftest_read(record_name).await?;
        let read_back = compose::parse(&after_marker.document)
            .map_err(|e| TransportError::Permanent { source: anyhow::anyhow!("{e}") })?;
        if read_back.text() != edited_text {
            return Err(TransportError::Permanent {
                source: anyhow::anyhow!(
                    "marker edit did not land as sent — read back {:?}, sent {:?}. The note may \
                     be left in the marked state; check it by hand.",
                    read_back.text(),
                    edited_text
                ),
            });
        }
        let _ = writeln!(out, "✓ 2. read back — marker landed exactly: {edited_text:?}");

        // ── 3. revert to the original text ──────────────────────────────
        // Re-run writability on the just-confirmed remote state, not the
        // in-memory `edited` — the same discipline as every other check
        // here: verify against what the server actually holds.
        let reverted_base = compose::writability(&after_marker.document, &title).map_err(|u| {
            TransportError::Permanent {
                source: anyhow::anyhow!(
                    "the marked note is no longer writable, refusing to attempt the revert: {u}. \
                     The note is left in the marked state; revert it by hand."
                ),
            }
        })?;
        let reverted = reverted_base.with_text_crdt(&original_text, self.replica_id).map_err(|e| {
            TransportError::Permanent {
                source: anyhow::anyhow!(
                    "with_text_crdt refused the revert: {e}. The note is left in the marked \
                     state; revert it by hand."
                ),
            }
        })?;
        let pushed_revert = self
            .modify(&wire::modify_note_body(&wire::NoteWrite {
                display_text: None,
                record_name: record_name.to_string(),
                change_tag: Some(after_marker.change_tag.clone()),
                text: original_text.clone(),
                document: compose::encode(&reverted),
                folder: after_marker.folder_id.clone(),
                created_ms: after_marker.created_ms,
                modified_ms: chrono::Utc::now().timestamp_millis(),
                echo: after_marker.echo.clone(),
            }))
            .await?;
        let _ = writeln!(
            out,
            "✓ 3. reverted — changeTag {} → {}",
            after_marker.change_tag, pushed_revert.change_tag
        );

        // ── 4. read back and verify the revert too ───────────────────────
        let (after_revert, _) = self.selftest_read(record_name).await?;
        let final_read = compose::parse(&after_revert.document)
            .map_err(|e| TransportError::Permanent { source: anyhow::anyhow!("{e}") })?;
        if final_read.text() != original_text {
            return Err(TransportError::Permanent {
                source: anyhow::anyhow!(
                    "revert did not land as sent — read back {:?}, expected {:?}. The note may \
                     not be back to its original text; check it by hand.",
                    final_read.text(),
                    original_text
                ),
            });
        }
        let _ = writeln!(out, "✓ 4. read back — original text restored exactly");
        let _ = writeln!(
            out,
            "\nConfirm on the iPhone/Mac before believing any of it: a record CloudKit\n             accepted is not the same claim as a note Apple Notes displays."
        );
        Ok(out)
    }

    /// **The relocation self-test** — everything a write can do here that does
    /// not touch the note document.
    ///
    /// The census answered the content question and answered it "no":
    /// `WRITABLE: 0/776`, because `substring` (Apple's per-character CRDT
    /// identity) is populated on every note and `compose` cannot mint the
    /// `CharID`s an insertion needs. **None of that reaches the operations
    /// below.** A move sends `Folder` and a `recordChangeTag` and nothing else
    /// ([`wire::move_note_body`]); a delete is that same move, to the Trash; a
    /// `Folder` record has no document to be refused over. So this is a
    /// genuinely separate question from the one the census closed, and it is
    /// open: `writes.folders` is false because nothing has measured it, not
    /// because something has.
    ///
    /// # The one that could destroy something
    ///
    /// CloudKit's `records/modify` distinguishes `update` from `replace`, and
    /// Jodd sends `update` — which should change only the fields in the
    /// request. **Should. Nothing here has measured it.** If `update` behaves
    /// as a replace, a move wipes `TextDataEncrypted` on a note whose content
    /// Jodd cannot rebuild. So step 0 captures the document's exact bytes
    /// BEFORE the first move, every verification compares against them, and a
    /// mismatch stops the run and puts the captured bytes back verbatim
    /// ([`wire::restore_document_body`], which composes nothing and is
    /// therefore not subject to the six refusals).
    ///
    /// # Containment, enforced rather than advised
    ///
    /// The subject note is **not** named by the caller. The caller names a
    /// folder, and the folder must hold **exactly one note** — so pointing
    /// this at real content takes deliberately emptying a folder down to one
    /// note first, which is not something a mistyped argument does. A locked
    /// note is refused outright. The scratch folder this creates is minted
    /// here, like `write_selftest`'s record name.
    ///
    /// Each verification re-reads the whole zone — there is no per-record
    /// endpoint — so on a large account expect minutes, not seconds.
    pub async fn relocation_selftest(&self, folder_path: &str) -> Result<String, TransportError> {
        use std::fmt::Write as _;
        let mut out = String::new();

        // ── 0. pick the subject, under the containment rule ─────────────
        let scan = self.scan().await?;
        let home = self.folder_id_for(&scan, folder_path)?;
        let subjects: Vec<String> = scan
            .notes
            .iter()
            .filter(|n| n.label == folder_path)
            .map(|n| n.uuid.clone())
            .collect();
        // **The refusal has to say what it SAW, not just that it refused.**
        // The first live attempt reported `holds 0 note(s)` for a folder Apple
        // Notes showed with one, and the message gave nothing to work from —
        // three readings of the code all said it should have matched. So the
        // count by PATH is reported beside the count by folder ID, which comes
        // from `bases` and does not go through the path map at all. If the two
        // disagree, the path map is the defect and the labels below name where
        // those notes were filed instead; if both are zero, the note is not in
        // the walk and the folder-id line says so without a second run.
        let by_id: Vec<&str> = scan
            .bases
            .iter()
            .filter(|(_, b)| b.folder_id == home)
            .map(|(k, _)| k.as_str())
            .collect();
        let their_labels: Vec<String> = by_id
            .iter()
            .map(|id| match scan.notes.iter().find(|n| n.uuid == *id) {
                Some(n) => format!("{id} → {:?}", n.label),
                None if scan.trashed.iter().any(|n| n.uuid == *id) => {
                    format!("{id} → in Recently Deleted")
                }
                None => format!("{id} → decoded out of the walk entirely"),
            })
            .collect();
        let sibling_paths: Vec<&str> = scan
            .folders
            .iter()
            .filter(|f| f.path.starts_with(folder_path) || folder_path.starts_with(&f.path))
            .map(|f| f.path.as_str())
            .collect();
        let diagnosis = format!(
            "\n  the folder's recordName is {home}\n  \
             notes whose LABEL is that path: {}\n  \
             notes whose Folder FIELD is that record: {} {}\n  \
             folder paths overlapping it: {:?}\n  \
             the walk holds {} note(s) and {} in Recently Deleted",
            subjects.len(),
            by_id.len(),
            if their_labels.is_empty() {
                String::new()
            } else {
                format!("— {}", their_labels.join(", "))
            },
            sibling_paths,
            scan.notes.len(),
            scan.trashed.len(),
        );
        drop(scan);
        if subjects.len() != 1 {
            return Err(unsupported(&format!(
                "{folder_path:?} holds {} note(s); this test needs a folder with EXACTLY one, \
                 and it must be one you are willing to lose.{diagnosis}",
                subjects.len()
            )));
        }
        let subject = subjects[0].clone();
        let _ = writeln!(out, "● subject note: {subject}");
        let _ = writeln!(out, "● home folder:  {folder_path}");

        // ONE zone read for both the note's write base and the home folder's
        // own record. The folder's raw `ParentFolder` is what the new folder
        // copies — see `wire::create_folder_body` for the level it landed at
        // when this was derived from the path instead.
        let (records, _) = self.selftest_records().await?;
        let find = |name: &str| {
            records.iter().find(|r| r["recordName"] == serde_json::json!(name)).cloned()
        };
        let Some(note_record) = find(&subject) else {
            return Err(unsupported("the note in that folder is not in the zone read"));
        };
        // **Two independent answers to the same question, reported against
        // each other.** The copy takes the field verbatim off a folder that is
        // already where the new one should go; the rule derives it from the
        // path, on the convention Apple's own client keeps (`parent_for_path`).
        // The copy is what gets sent — it needs no theory to be right — and a
        // disagreement is a finding, because M3 has to place folders at paths
        // where there is nothing to copy from.
        let parent = find(&home).as_ref().and_then(wire::parent_folder_of);
        let derived = {
            let scan = self.scan().await?;
            let d = wire::parent_for_path(folder_path, &scan.folders);
            drop(scan);
            d
        };
        let describe = |p: Option<&str>| match p {
            Some(wire::DEFAULT_FOLDER) => "the account root".to_string(),
            Some(_) => "another folder".to_string(),
            None => "absent — a sibling of Notes".to_string(),
        };
        let _ = writeln!(
            out,
            "● new folder's parent: copied from {folder_path:?} — {}",
            describe(parent.as_deref())
        );
        let _ = writeln!(
            out,
            "  the path rule would have said: {}{}",
            match &derived {
                Ok(d) => describe(d.as_deref()),
                Err(e) => format!("refused — {e}"),
            },
            match &derived {
                Ok(d) if *d == parent => "  (they agree)",
                Ok(_) => "  ✗ THEY DISAGREE — the rule is wrong for this account",
                Err(_) => "",
            }
        );
        let base0 = wire::write_base(&note_record, false);
        if base0.locked {
            return Err(unsupported(
                "the one note in that folder is password-protected; pick a folder with an \
                 ordinary note",
            ));
        }
        // The bytes every later step is checked against. Captured once, before
        // anything is written.
        let document = base0.document.clone();
        let _ = writeln!(
            out,
            "● captured document: {} byte(s) — every step below re-checks these",
            document.len()
        );
        let mut tag = base0.change_tag.clone();

        // ── 1. create a folder ──────────────────────────────────────────
        //
        // From here on NOTHING uses `?`. A run that dies in the middle takes
        // its report with it — which is exactly what happened on the first
        // live attempt: CloudKit answered 503 partway through, the error
        // propagated, and the user was left with one red line and no idea
        // which steps had run or what was left behind in their account. A
        // diagnostic that loses its findings on the interesting runs is worse
        // than none, so every failure below lands in `out` and the function
        // returns `Ok`.
        let scratch = self.mint();
        let scratch_title = format!("Jodd relocation test {}", &scratch[..8]);
        let mut scratch_tag: Option<String> = None;
        let mut destination = wire::DEFAULT_FOLDER.to_string();
        match self
            .modify_resilient(&wire::create_folder_body(&scratch, &scratch_title, parent.as_deref()))
            .await
        {
            Ok((saved, _)) => {
                let _ = writeln!(out, "✓ 1. CloudKit accepted a Folder create — {scratch_title}");
                scratch_tag = Some(saved.change_tag);
                destination = scratch.clone();
            }
            Err(e) => {
                let _ = writeln!(
                    out,
                    "✗ 1. Folder create refused: {e}\n     using the account root as the move destination instead, so the \
                     rest still runs"
                );
            }
        }

        // ── 2. does Apple's own zone show it? ───────────────────────────
        if scratch_tag.is_some() {
            match self.selftest_records().await {
                Ok((records, complete)) => {
                    match records.iter().find(|r| r["recordName"] == serde_json::json!(scratch)) {
                        Some(r) => {
                            let title = wire::decode_text_field(&r["fields"]["TitleEncrypted"]);
                            let par = wire::parent_folder_of(r);
                            let _ = writeln!(
                                out,
                                "✓ 2. it is in the zone — title matches: {}, parent matches the \
                                 one it copied: {}",
                                title.as_deref() == Some(scratch_title.as_str()),
                                par == parent
                            );
                        }
                        None if !complete => {
                            let _ = writeln!(
                                out,
                                "△ 2. not found, but the walk did not finish — this is 'not read yet', \
                                 not 'not there'"
                            );
                        }
                        None => {
                            let _ = writeln!(
                                out,
                                "✗ 2. CloudKit accepted the create and the folder is NOT in the zone"
                            );
                        }
                    }
                }
                Err(e) => {
                    let _ = writeln!(out, "△ 2. could not re-read the zone to confirm it: {e}");
                }
            }
            let _ = writeln!(
                out,
                "     Apple Notes is the real judge: a Folder record CloudKit stored is not\n     \
                 the same claim as a folder Notes.app shows."
            );
        }

        // ── 3–6. the moves, each checked against the captured bytes ─────
        let steps: [(&str, &str); 4] = [
            ("3. move into the new folder", destination.as_str()),
            ("4. move back home", home.as_str()),
            ("5. delete (a move to the Trash)", wire::TRASH_FOLDER),
            ("6. restore (a move back out)", home.as_str()),
        ];
        let mut at_home = true;
        for (label, target) in steps {
            let reply = match self
                .modify_resilient(&wire::move_note_body(&subject, Some(&tag), target, chrono::Utc::now().timestamp_millis()))
                .await
            {
                Ok((saved, record)) => {
                    tag = saved.change_tag;
                    at_home = target == home;
                    record
                }
                Err(e) => {
                    let _ = writeln!(out, "✗ {label} — refused: {e}");
                    break;
                }
            };
            // The reply first — it's free, this call already made it. The
            // alternatives cost a real request each: `records/lookup` one
            // round trip, a whole-zone read thirty pages, which is what
            // provoked the 503 that killed the first run. Which source
            // answered is printed, because "the reply carries the stored
            // record" is itself something nothing here has measured.
            let (record, source) = if wire::folder_and_document(&reply).is_some() {
                (reply, "the modify reply")
            } else {
                match self.selftest_records().await {
                    Ok((records, _)) => {
                        match records
                            .iter()
                            .find(|r| r["recordName"] == serde_json::json!(subject))
                        {
                            Some(r) => (r.clone(), "a whole-zone read"),
                            None => {
                                let _ = writeln!(
                                    out,
                                    "△ {label} — the write was accepted and the note is not in \
                                     the zone read that followed"
                                );
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = writeln!(
                            out,
                            "△ {label} — the write was accepted but could not be verified: {e}"
                        );
                        break;
                    }
                }
            };
            let Some((folder_now, doc_now)) = wire::folder_and_document(&record) else {
                let _ = writeln!(
                    out,
                    "△ {label} — the write was accepted and the record checked back carried \
                     neither field"
                );
                break;
            };
            if let Some(t) = record["recordChangeTag"].as_str() {
                tag = t.to_string();
            }
            // **The controlled version of the census's open question.** The
            // census found `Folders` (plural) naming a real folder on one
            // trashed note and only the Trash on another, and could not say
            // why: nobody knows where either was deleted from. Here the origin
            // is known — this test filed it — so what the field says after
            // Jodd's own delete is an answer rather than a candidate.
            if target == wire::TRASH_FOLDER {
                let plural = wire::folders_plural(&record["fields"]["Folders"]);
                let _ = writeln!(
                    out,
                    "     Folders (plural) after Jodd's own delete, from a folder we know: {}",
                    match plural {
                        None => "absent".to_string(),
                        Some(ids) if ids.iter().any(|i| i == &home) =>
                            "names the home folder — restore could stop asking".to_string(),
                        Some(ids) if ids.iter().all(|i| i == wire::TRASH_FOLDER) =>
                            "only the Trash — the origin is not in it".to_string(),
                        Some(ids) => format!("{} entry(s), none of them home", ids.len()),
                    }
                );
            }
            let landed = folder_now == target;
            let intact = doc_now == document;
            let _ = writeln!(
                out,
                "{} {label} — landed where sent: {landed}, document intact: {intact}  (checked via {source})",
                if landed && intact { "✓" } else { "✗" }
            );
            if !intact {
                let _ = writeln!(
                    out,
                    "\n✗✗ THE MOVE CHANGED THE DOCUMENT — {} byte(s) went in, {} came back.\n    \
                     CloudKit's `update` is behaving as a REPLACE, so no write on this backend\n    \
                     is safe, moves included. Putting the captured bytes back and stopping.",
                    document.len(),
                    doc_now.len()
                );
                match self
                    .modify_resilient(&wire::restore_document_body(&subject, &tag, &document))
                    .await
                {
                    Ok((_, r)) => {
                        let back = wire::folder_and_document(&r).map(|(_, d)| d == document);
                        let _ = writeln!(
                            out,
                            "    restore: the document matches the capture again: {}",
                            match back {
                                Some(v) => v.to_string(),
                                None => "accepted, but the reply did not echo it".to_string(),
                            }
                        );
                    }
                    Err(e) => {
                        let _ = writeln!(
                            out,
                            "    restore FAILED: {e}\n    Note {subject} needs recovering from \
                             Apple's own version history."
                        );
                    }
                }
                break;
            }
        }
        if !at_home {
            let _ = writeln!(
                out,
                "\n⚠ the run stopped with the note NOT back in {folder_path:?} — move it back in \
                 Apple Notes."
            );
        }

        // ── 7–8. rename the scratch folder, then remove it ──────────────
        if let Some(ft) = scratch_tag.clone() {
            let renamed = format!("{scratch_title} (renamed)");
            match self.modify_resilient(&wire::rename_folder_body(&scratch, &ft, &renamed)).await {
                Ok((saved, r)) => {
                    scratch_tag = Some(saved.change_tag);
                    let seen = wire::decode_text_field(&r["fields"]["TitleEncrypted"]);
                    let _ = writeln!(
                        out,
                        "{} 7. folder rename — accepted; the reply's title matches: {}",
                        if seen.as_deref() == Some(renamed.as_str()) { "✓" } else { "△" },
                        match seen {
                            Some(s) => (s == renamed).to_string(),
                            None => "the reply did not echo it".to_string(),
                        }
                    );
                }
                Err(e) => {
                    let _ = writeln!(out, "✗ 7. folder rename refused: {e}");
                }
            }
        }
        match scratch_tag {
            Some(ft) => match self.modify_resilient(&wire::delete_folder_body(&scratch, &ft)).await {
                Ok(_) => {
                    let _ = writeln!(out, "✓ 8. the scratch folder was deleted — nothing to clean up");
                }
                Err(e) => {
                    let _ = writeln!(
                        out,
                        "✗ 8. deleting the scratch folder failed: {e}\n     \
                         {scratch_title:?} is still in the account — remove it in Apple Notes."
                    );
                }
            },
            None => {
                let _ = writeln!(out, "  8. no scratch folder was created, so there is none to remove");
            }
        }

        let _ = writeln!(
            out,
            "\nNow check Apple Notes, and the iPhone: the note should be back in\n\
             {folder_path:?} with its text unchanged, and no folder named\n\
             {scratch_title:?} should remain.\n\
             A record CloudKit accepted is not the same claim as a note Apple displays."
        );
        Ok(out)
    }

    /// [`Self::modify_raw`] with a transient failure retried rather than
    /// surfaced.
    ///
    /// **Measured, not defensive**: the first live relocation run died on
    /// `HTTP 503` partway through — CloudKit is rate-limited, and this test
    /// walks the zone repeatedly right before writing. A transient is by
    /// definition worth retrying, and a diagnostic that abandons a half-written
    /// account because Apple was briefly busy leaves the user to clean up
    /// after it. Nothing else is retried: a `Conflict` or a `Permanent` means
    /// the same request will be refused again.
    async fn modify_resilient(
        &self,
        body: &serde_json::Value,
    ) -> Result<(wire::SavedRecord, serde_json::Value), TransportError> {
        let mut last = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt))).await;
            }
            match self.modify_raw(body).await {
                Err(e @ TransportError::Transient { .. }) => {
                    crate::log!("icloud: transient on a write, retrying ({e})");
                    last = Some(e);
                }
                other => return other,
            }
        }
        Err(last.unwrap_or(TransportError::Transient {
            source: anyhow::anyhow!("CloudKit stayed unavailable across three attempts"),
        }))
    }

    /// **Forensics for one disagreement, not a general diagnostic.** Apple's
    /// own Notes.app, after a confirmed forced re-sync — the account toggled
    /// off and on in System Settings, the app quit and reopened — still shows
    /// a note living in a folder that this backend's zone walk places in the
    /// Trash. That rules out staleness on Apple's end, so what is left to
    /// check is the raw records themselves, not another summary of them.
    ///
    /// Filters by a TITLE SUBSTRING because the caller has no `recordName` to
    /// hand. Every value printed is an identifier or a count — `recordName`,
    /// `ModificationDate`, `recordChangeTag`, folder placement, how many times
    /// a name arrived — never note text beyond the substring the caller
    /// already knows.
    pub async fn debug_note_history(&self, title_contains: &str) -> Result<String, TransportError> {
        use std::fmt::Write as _;
        let (records, _, complete) = self.fetch_all_records().await?;
        let mut out = String::new();
        if !complete {
            let _ = writeln!(
                out,
                "⚠ the zone walk did not reach the end — results are a LOWER BOUND"
            );
        }
        let folder_paths = wire::build_folder_paths(&wire::folder_records(&records));

        let mut by_name: std::collections::HashMap<&str, Vec<&serde_json::Value>> =
            std::collections::HashMap::new();
        for r in records.iter().filter(|r| {
            matches!(r["recordType"].as_str(), Some("Note") | Some("PasswordProtectedNote"))
        }) {
            let title = wire::decode_text_field(&r["fields"]["TitleEncrypted"]).unwrap_or_default();
            if !title.contains(title_contains) {
                continue;
            }
            let Some(name) = r["recordName"].as_str() else { continue };
            by_name.entry(name).or_default().push(r);
        }
        if by_name.is_empty() {
            let _ = writeln!(
                out,
                "no Note record with {title_contains:?} in its title arrived in this walk \
                 ({} record(s) total)",
                records.len()
            );
            return Ok(out);
        }
        // Content-bearing fields — never printed by value, only by presence
        // and size, so this diagnostic stays within "identifiers and counts
        // only, never note content" even when it dumps a field nothing here
        // has a name for yet.
        const CONTENT_FIELD_NAMES: &[&str] = &[
            "TitleEncrypted", "SnippetEncrypted", "TextDataEncrypted", "DisplayTextEncrypted",
            "StandardizedContentEncrypted", "TokenContentIdentifierEncrypted",
            "AltTextEncrypted", "MergeableDataEncrypted", "FirstAttachmentThumbnail",
        ];
        for (name, copies) in &by_name {
            let _ = writeln!(out, "\n● {name} — arrived {} time(s) in this walk", copies.len());
            for r in copies.iter() {
                let deleted = wire::is_deleted_record(r);
                let folder_id =
                    r["fields"]["Folder"]["value"]["recordName"].as_str().unwrap_or("<none>");
                let folder_path =
                    folder_paths.get(folder_id).cloned().unwrap_or_else(|| "<unresolved>".into());
                let trashed = folder_id == wire::TRASH_FOLDER;
                let ms = wire::modification_ms(r);
                let tag = r["recordChangeTag"].as_str().unwrap_or("<none>");
                let _ = writeln!(
                    out,
                    "    deleted={deleted}  trashed={trashed}  folder={folder_path:?} \
                     ({folder_id})  ModificationDate={ms:?}  changeTag={tag}"
                );
                // Every OTHER field this record carries, whether or not this
                // codebase has ever named it before now — `Folders` (plural,
                // distinct from `Folder`) is the one this run is actually
                // hunting: never wired into the delete/move path, and
                // gotcha #22 already found it present but mixed on other
                // notes without anything here printing what it says on THIS
                // one.
                if let Some(obj) = r["fields"].as_object() {
                    let mut keys: Vec<&String> = obj.keys().collect();
                    keys.sort();
                    for k in keys {
                        if k == "Folder" || k == "ModificationDate" {
                            continue; // already shown above
                        }
                        let v = &obj[k]["value"];
                        if CONTENT_FIELD_NAMES.contains(&k.as_str()) {
                            let len = v.as_str().map(|s| s.len()).unwrap_or(0);
                            let _ = writeln!(out, "      {k} = <redacted, {len} base64 char(s)>");
                        } else {
                            let _ = writeln!(out, "      {k} = {v}");
                        }
                    }
                }
                // The document itself, as STRUCTURE — which runs are live,
                // which are tombstoned, whose replica minted them — plus the
                // head of its first line and of `TitleEncrypted` as code
                // points. Added 2026-09-09 for the head-deletion report: the
                // counts-only dump above could not say whether the server
                // held Jodd's second push (one tombstoned run, one live run,
                // one replica) or something a client wrote over it, and
                // "23 base64 chars" cannot tell a stray combining mark from
                // any other three bytes. Twelve code points of a line the
                // caller already searched by, never the body.
                let title_field = wire::decode_text_field(&r["fields"]["TitleEncrypted"]).unwrap_or_default();
                let _ = writeln!(out, "      TitleEncrypted decoded: {}", head_codepoints(&title_field, 12));
                if let Some(b64) = r["fields"]["TextDataEncrypted"]["value"].as_str() {
                    use base64::Engine as _;
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(b64)
                        .map_err(|e| e.to_string())
                        .and_then(|bytes| compose::parse(&bytes).map_err(|e| e.to_string()));
                    match decoded {
                        Ok(d) => {
                            let first_line = d.text().split('\n').next().unwrap_or("");
                            let _ = writeln!(
                                out,
                                "      document: first line {}  total {} char(s), {} attribute run(s)",
                                head_codepoints(first_line, 12),
                                d.text().chars().count(),
                                d.string.attribute_run.len()
                            );
                            match crdt::parse_crdt_document(&d.string) {
                                Ok(c) => {
                                    for (i, rep) in c.replicas.iter().enumerate() {
                                        let id: String = rep.id.iter().map(|b| format!("{b:02x}")).collect();
                                        let _ = writeln!(
                                            out,
                                            "      replica {}: {id} counters={:?}{}",
                                            i + 1,
                                            rep.counters,
                                            if rep.id == self.replica_id { "  ← this Jodd" } else { "" }
                                        );
                                    }
                                    for (i, run) in c.runs.iter().enumerate() {
                                        let _ = writeln!(
                                            out,
                                            "      run[{i}] replica={} clock={} len={} tombstone={} \
                                             anchor=({},{}) child={:?}",
                                            run.coord.replica,
                                            run.coord.clock,
                                            run.length,
                                            run.tombstone,
                                            run.anchor.replica,
                                            run.anchor.clock,
                                            run.sequence
                                        );
                                    }
                                }
                                Err(e) => {
                                    let _ = writeln!(out, "      document carries no readable CRDT identity: {e}");
                                }
                            }
                        }
                        Err(e) => {
                            let _ = writeln!(out, "      document did not decode: {e}");
                        }
                    }
                }
            }
        }

        // The per-user record's OWN `Folder`/`Deleted` — never read by
        // anything else in this codebase before 2026-08-25.
        //
        // `collect_pins` already proved `*_UserSpecific` records exist per
        // note and reference it back via `fields.Note`, but only ever reads
        // `IsPinned` off them — the exact shape of gotcha #23 before it was
        // found: the pin looked like it lived on `Note` and didn't, and nothing
        // here has asked whether "is this note trashed, for the viewing user"
        // has the same split. The account census already showed `Folder`
        // present on 266 of 267 per-user records; this is the first place
        // anything reads what it actually says. If it disagrees with the base
        // `Note.Folder` above for a record this backend trashed, that is a
        // real candidate for why Apple's own client keeps treating a
        // Jodd-trashed note as ordinary and editable — Jodd's delete has only
        // ever written the base record.
        let mut has_user_record: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for r in records.iter().filter(|r| {
            matches!(
                r["recordType"].as_str(),
                Some("Note_UserSpecific") | Some("PasswordProtectedNote_UserSpecific")
            )
        }) {
            let Some(note_id) = r["fields"]["Note"]["value"]["recordName"].as_str() else { continue };
            if !by_name.contains_key(note_id) {
                continue;
            }
            has_user_record.insert(note_id);
            let deleted = wire::is_deleted_record(r);
            let folder_id = r["fields"]["Folder"]["value"]["recordName"].as_str().unwrap_or("<none>");
            let folder_path = folder_paths.get(folder_id).cloned().unwrap_or_else(|| "<unresolved>".into());
            let pinned = r["fields"]["IsPinned"]["value"].as_i64().unwrap_or(0) != 0
                || r["fields"]["IsPinned"]["value"].as_bool().unwrap_or(false);
            let ms = wire::modification_ms(r);
            let tag = r["recordChangeTag"].as_str().unwrap_or("<none>");
            let _ = writeln!(
                out,
                "  {}'s per-user record ({}): deleted={deleted}  folder={folder_path:?} \
                 ({folder_id})  pinned={pinned}  ModificationDate={ms:?}  changeTag={tag}",
                note_id,
                r["recordType"].as_str().unwrap_or("?"),
            );
        }
        // Silence here is ambiguous between "no per-user record exists for
        // this note" (true for most notes — one is only minted on pin or on
        // being opened through a native client) and "this build predates the
        // block above". Say which, rather than leaving a blank where a line
        // was expected.
        for name in by_name.keys() {
            if !has_user_record.contains(name) {
                let _ = writeln!(out, "  {name} has no per-user record in this walk (never pinned or viewed)");
            }
        }

        // What the SCAN — the same code path a real read or write uses —
        // concluded for each of these names, so a disagreement between the
        // raw records above and this section points straight at
        // decode_records rather than at the walk.
        let scan = self.scan().await?;
        for name in by_name.keys() {
            let filed = scan.notes.iter().find(|n| n.uuid == *name);
            let trashed = scan.trashed.iter().find(|n| n.uuid == *name);
            let _ = writeln!(
                out,
                "  scan concluded for {name}: {}",
                match (filed, trashed) {
                    (Some(n), None) => format!("filed at {:?}", n.label),
                    (None, Some(_)) => "in Recently Deleted".to_string(),
                    (None, None) => "absent from both — dropped somewhere before the scan output"
                        .to_string(),
                    (Some(_), Some(_)) => "in BOTH — that is its own bug".to_string(),
                }
            );
        }
        Ok(out)
    }

    /// One whole-zone read, raw, for the steps that need a record type the
    /// scan does not keep (a `Folder`, whose write path has no cached base).
    async fn selftest_records(&self) -> Result<(Vec<serde_json::Value>, bool), TransportError> {
        let (records, _, complete) = self.fetch_all_records().await?;
        Ok((records, complete))
    }

    /// Re-reads the zone and finds the self-test's own record.
    ///
    /// A whole-zone read per verification, and correct: there is no per-record
    /// endpoint on this backend, which is the same constraint the scan cache
    /// exists to manage.
    async fn selftest_read(
        &self,
        record: &str,
    ) -> Result<(WriteBase, String), TransportError> {
        let (records, _, _) = self.fetch_all_records().await?;
        let r = records
            .iter()
            .find(|r| r["recordName"] == serde_json::json!(record))
            .ok_or_else(|| TransportError::Permanent {
                source: anyhow::anyhow!("the record Jodd just wrote is not in the zone"),
            })?;
        let base = wire::write_base(r, false);
        let title = wire::decode_text_field(&r["fields"]["TitleEncrypted"]).unwrap_or_default();
        Ok((base, title))
    }

    /// The resume token the account's own zone read ended on.
    ///
    /// Served from the cached scan, so asking for it costs nothing when a read
    /// has already happened. This is what primes `accounts.sync_cursor`: a
    /// second pass through `changes_since` just to establish a cursor reads the
    /// whole zone again, and stopping that pass early stores a token pointing
    /// into the middle of it.
    pub async fn sync_token(&self) -> Option<String> {
        self.scan().await.ok().and_then(|s| s.sync_token.clone())
    }

    /// The Advanced Data Protection verdict for this account, from the one
    /// zone walk. Component H's entry point.
    pub async fn adp_verdict(&self) -> Result<AdpVerdict, TransportError> {
        Ok(AdpVerdict::of(&self.scan().await?.tally))
    }

    /// The `recordName` of the folder a note is being filed into.
    ///
    /// Resolved from the scan's own tree, never created: `writes.folders` is
    /// false on this backend (M2 spec, Component O), so a path with no folder
    /// behind it is a refusal rather than a `create_folder` call. It is a
    /// **permanent** one — the folder will not appear by retrying — and it
    /// names the path so the user can see which one Jodd could not find.
    fn folder_id_for(&self, scan: &Scan, label: &str) -> Result<String, TransportError> {
        scan.folders
            .iter()
            .find(|f| f.path == label)
            .map(|f| f.id.clone())
            // The root's record name is Apple's own fixed constant, and the
            // read path already falls back to it for a note whose folder this
            // walk never saw. Refusing to write to the root because its Folder
            // record happened not to arrive would be a guess in the losing
            // direction.
            .or_else(|| {
                (label == wire::ROOT_PATH).then(|| wire::DEFAULT_FOLDER.to_string())
            })
            .ok_or_else(|| {
                // **Named alternatives, because the path shape is not
                // guessable.** Jodd models `Notes` as the root of the whole
                // account, so every folder's path starts with it — including
                // the ones Apple Notes shows BESIDE `Notes` rather than inside
                // it. Someone reading their own sidebar types the leaf and
                // gets this refusal, which until now said only that the name
                // was wrong. A leaf that matches exactly one folder is worth
                // naming outright.
                let leaf = label.rsplit('/').next().unwrap_or(label);
                let matches: Vec<&str> = scan
                    .folders
                    .iter()
                    .filter(|f| f.path.rsplit('/').next() == Some(leaf))
                    .map(|f| f.path.as_str())
                    .collect();
                unsupported(&format!(
                    "no folder named {label:?} in this account, and Jodd cannot create one \
                     here — make it in Apple Notes first.{}",
                    match matches.as_slice() {
                        [] => String::new(),
                        [one] => format!(
                            " Jodd calls that folder {one:?} — every path starts with \
                             {ROOT:?}, even for folders Apple Notes shows beside it.",
                            ROOT = wire::ROOT_PATH
                        ),
                        many => format!(" Did you mean one of {many:?}?"),
                    }
                ))
            })
    }

    /// The path a folder `recordName` maps to, with the same fallback the read
    /// path uses for a folder this walk never saw (`decode_note` files those
    /// under the root).
    fn label_of<'a>(&self, scan: &'a Scan, folder_id: &str) -> &'a str {
        scan.folders
            .iter()
            .find(|f| f.id == folder_id)
            .map(|f| f.path.as_str())
            .unwrap_or(wire::ROOT_PATH)
    }

    /// Posts one `records/modify`.
    ///
    /// **The caller must fold the result into the cached scan** — see
    /// [`AccountCache::apply`] for why neither leaving the cache alone nor
    /// dropping it is acceptable. Folding is the caller's because only the
    /// caller knows what changed: a content write replaces a note, a delete
    /// removes one, a move relabels one.
    async fn modify(&self, body: &serde_json::Value) -> Result<wire::SavedRecord, TransportError> {
        self.modify_raw(body).await.map(|(s, _)| s)
    }

    /// [`Self::modify`] keeping the server's own copy of the saved record.
    ///
    /// See [`wire::post_modify_raw`]: `records/lookup` (see [`Self::lookup_records`])
    /// is a real per-record endpoint, but the reply this call already got is
    /// a free way to check a field you just wrote, with no second request at
    /// all — walking the whole zone is the expensive fallback, not the only
    /// one.
    async fn modify_raw(
        &self,
        body: &serde_json::Value,
    ) -> Result<(wire::SavedRecord, serde_json::Value), TransportError> {
        let http = reqwest::Client::new();
        let host = self.ck_hostname();
        let header = self.cookie_header(&host, "/database/1").await?;
        wire::post_modify_raw(
            &http,
            &self.session.ck_host,
            &self.session.dsid,
            &self.session.client,
            &header,
            body,
        )
        .await
    }

    /// Posts one `records/lookup` — a direct point read, not a page of the
    /// `changes/zone` feed everything else in this module reads through. See
    /// [`wire::lookup_url`] for why this exists: to test, rather than assume,
    /// whether the feed and a point lookup see the same server state.
    async fn lookup_records(&self, record_names: &[String]) -> Result<Vec<serde_json::Value>, TransportError> {
        let http = reqwest::Client::new();
        let host = self.ck_hostname();
        let header = self.cookie_header(&host, "/database/1").await?;
        wire::post_lookup(&http, &self.session.ck_host, &self.session.dsid, &self.session.client, &header, record_names)
            .await
    }

    /// One `records/lookup` for a single record, reduced to the [`WriteBase`]
    /// a write needs — the fresh `recordChangeTag` above all. `Ok(None)` means
    /// the record is genuinely absent (a point read, not the change feed, so
    /// this is authoritative), which the caller turns into `NotFound`.
    ///
    /// A point read, deliberately not a zone walk: the whole reason the
    /// conflict loop exists is to refresh one stale tag without the whole-zone
    /// read the scan cache would otherwise cost.
    async fn lookup_write_base(
        &self,
        id: &str,
        locked: bool,
    ) -> Result<Option<WriteBase>, TransportError> {
        let records = self.lookup_records(&[id.to_string()]).await?;
        let Some(r) = records.iter().find(|r| r["recordName"].as_str() == Some(id)) else {
            return Ok(None);
        };
        if let Some(code) = r["serverErrorCode"].as_str() {
            // `NOT_FOUND`/`UNKNOWN_ITEM` map to a real absence; anything else
            // from a lookup is not a shape this path can act on, so surface it.
            return match wire::classify_modify_error(code, r["reason"].as_str().unwrap_or("")) {
                TransportError::NotFound => Ok(None),
                other => Err(other),
            };
        }
        Ok(Some(wire::write_base(r, locked)))
    }

    /// Builds the `records/modify` payload for an in-place content update from
    /// a given base — factored out of [`Self::save_note_full`] so the
    /// conflict-refresh loop can rebuild the write against a freshly
    /// looked-up base (a new `recordChangeTag`, and whatever document Apple
    /// now serves) without duplicating the writability / recompose / CRDT /
    /// formatting pipeline.
    fn build_update_write(
        &self,
        scan: &Scan,
        id: &str,
        base: &WriteBase,
        op: &SaveOp<'_>,
        now: i64,
    ) -> Result<wire::NoteWrite, TransportError> {
        // **Resolved only when the label actually changed.** A note whose
        // `Folder` names a record the walk never saw is filed under the root
        // for display (`decode_note`, counted as `orphaned`), so re-resolving
        // its label on every save would write `DefaultFolder-CloudKit` onto it
        // and MOVE it — a silent reorganisation of the user's account performed
        // by an edit that changed a word. Measured `orphaned = 0` on the live
        // account, which is a reason to keep it that way, not a reason to skip
        // the guard.
        let folder = if self.label_of(scan, &base.folder_id) == op.label {
            base.folder_id.clone()
        } else {
            self.folder_id_for(scan, op.label)?
        };
        let doc = compose::writability_with_refs(&base.document, &base.title_field, &scan.inline_refs)
            .map_err(refused)?;
        // The title the body was CUT by, re-derived from the remote's own text
        // — not `TitleEncrypted`, which is a lossy derivation of it (gotcha
        // #21, and `doc::note_title`).
        let old_title = doc::note_title(doc.text(), &base.title_field);
        crate::log!(
            "icloud: update base for {id}: old_title={} base_tag={} crdt={} runs={}",
            head_codepoints(&old_title, 12),
            base.change_tag,
            doc.crdt.is_some(),
            doc.crdt.as_ref().map(|c| c.runs.len()).unwrap_or(0)
        );
        // One parse yields the plain text AND the formatting model, so the two
        // can never disagree (M3 F2).
        let parsed_body = format_html::parse_editor_html(op.body_html);
        let text = compose::recompose(doc.text(), &old_title, op.title, &parsed_body.text);
        // **The dispatch the CarriesCrdtIdentity relaxation (M2.5) needs and
        // `with_text` alone cannot give it.** `writability` now returns `Ok`
        // for a document `doc.crdt: Some(_)` too — `with_text` never learned
        // about that (it only re-lengths `attribute_run`, leaving
        // `substring`/`timestamp` exactly as they were), so calling it here on
        // such a document would silently produce a structurally invalid CRDT
        // write. `with_text_crdt` is the sibling that actually understands the
        // identity: same edit, but spliced through `crdt::apply_text_edit`.
        let mut edited = match &doc.crdt {
            Some(_) => doc.with_text_crdt(&text, self.replica_id).map_err(|e| {
                TransportError::Permanent {
                    source: anyhow::anyhow!(
                        "this note's CRDT engine refused the edit: {e} — open it in \
                         Apple Notes to edit"
                    ),
                }
            })?,
            None => doc.with_text(&text),
        };
        // Formatting rides the same save (M3): rewrite the changed paragraphs'
        // runs, downgrade to text-only on refusal.
        Self::reconcile_formatting(&mut edited, &parsed_body, self.replica_id);
        // Title/snippet derive from the text with objects rendered as their
        // display text — a raw U+FFFC in TitleEncrypted is a replacement glyph
        // in Apple's list view (gotcha #21).
        let display_text =
            doc::text_with_objects_rendered(&text, &edited.string.attribute_run, &scan.inline_refs);
        Ok(wire::NoteWrite {
            display_text: Some(display_text),
            record_name: id.to_string(),
            change_tag: Some(base.change_tag.clone()),
            text,
            document: compose::encode(&edited),
            folder,
            created_ms: base.created_ms,
            modified_ms: now,
            echo: base.echo.clone(),
        })
    }

    /// **A direct `records/lookup` for one record, formatted the same way
    /// [`Self::debug_note_history`] formats a `changes/zone` copy** — so the
    /// two can be read side by side for the same `recordName`.
    ///
    /// Exists to answer one question without guessing: when Jodd's own
    /// `changes/zone` walk disagrees with what Apple Notes.app and
    /// icloud.com show, does a point lookup agree with the feed (the whole
    /// endpoint is behind for this record) or with Apple's clients (the
    /// feed specifically is behind, and a point read is not)? Prints
    /// identifiers and counts only, never note text.
    ///
    /// **Deliberately does not call `self.scan()`.** An earlier version
    /// resolved `folder_id` to a path via the cached scan, which meant a
    /// diagnostic meant to test whether `changes/zone` is stale silently
    /// depended on `changes/zone` to render its own answer — a ~40s zone
    /// walk hiding behind what should be one fast HTTP round trip. The raw
    /// `folder_id` is enough to tell trashed from filed from moved; the
    /// path is a label a human can look up separately if they want it.
    pub async fn debug_record_lookup(&self, record_name: &str) -> Result<String, TransportError> {
        use std::fmt::Write as _;
        let mut out = String::new();
        let records = self.lookup_records(&[record_name.to_string()]).await?;
        let Some(r) = records.iter().find(|r| r["recordName"].as_str() == Some(record_name)) else {
            let _ = writeln!(
                out,
                "records/lookup returned nothing for {record_name:?} — CloudKit reports it as \
                 absent, not merely errored"
            );
            return Ok(out);
        };
        if let Some(code) = r["serverErrorCode"].as_str() {
            let _ = writeln!(
                out,
                "records/lookup: {record_name} → {code}: {}",
                r["reason"].as_str().unwrap_or("no reason given")
            );
            return Ok(out);
        }
        let deleted = wire::is_deleted_record(r);
        let folder_id = r["fields"]["Folder"]["value"]["recordName"].as_str().unwrap_or("<none>");
        let trashed = folder_id == wire::TRASH_FOLDER;
        let ms = wire::modification_ms(r);
        let tag = r["recordChangeTag"].as_str().unwrap_or("<none>");
        let _ = writeln!(
            out,
            "records/lookup (direct, not changes/zone) for {record_name}:\n\
             \x20   deleted={deleted}  trashed={trashed}  folder_id={folder_id}  \
             ModificationDate={ms:?}  changeTag={tag}"
        );
        Ok(out)
    }

    /// Replaces one note in the cached scan, or adds it.
    ///
    /// Also drops it from `trashed`: a write that lands anywhere but the Trash
    /// means the note is no longer in Recently Deleted, and leaving the old
    /// copy there would show a restored note in both places at once.
    async fn cache_note(&self, note: Note, base: WriteBase) {
        self.scans
            .apply(move |scan| {
                scan.bases.insert(note.uuid.clone(), base);
                scan.trashed.retain(|n| n.uuid != note.uuid);
                match scan.notes.iter_mut().find(|n| n.uuid == note.uuid) {
                    Some(slot) => *slot = note,
                    None => scan.notes.push(note),
                }
            })
            .await;
    }

    /// Moves one note from the cached listing into the cached Trash.
    ///
    /// **Moved, not dropped.** A delete on this backend files the note in
    /// Apple's Recently Deleted, so it leaves every listing AND appears in the
    /// trash view — and `has_trash` is true, which means there is a view for it
    /// to appear in. Dropping it would leave the trash empty until the next
    /// whole-zone walk, which reads as a delete that ate the note.
    async fn trash_note_in_cache(&self, record_name: &str) {
        let name = record_name.to_string();
        self.scans
            .apply(move |scan| {
                if let Some(i) = scan.notes.iter().position(|n| n.uuid == name) {
                    let mut n = scan.notes.remove(i);
                    // The placeholder `list_trashed` reports, so the cached row
                    // and a freshly-walked one describe the note the same way.
                    n.label = wire::ROOT_PATH.to_string();
                    scan.trashed.push(n);
                }
            })
            .await;
    }

    /// The account's zone read — from the shared cache when it is fresh
    /// enough, otherwise walked and stored.
    pub async fn scan(&self) -> Result<std::sync::Arc<Scan>, TransportError> {
        let mut slot = self.scans.scan.lock().await;
        if let Some((at, scan)) = slot.as_ref() {
            if at.elapsed() < AccountCache::MAX_AGE {
                *self.seen_tally.lock().unwrap() = Some(scan.tally);
                return Ok(scan.clone());
            }
        }
        // The lock is deliberately held across the walk. Two verticals racing
        // here would otherwise both walk the whole zone — which is precisely
        // the duplication this cache exists to remove, and the sweep produces
        // exactly that race by constructing a vertical per tick.
        let scan = std::sync::Arc::new(self.walk_zone().await?);
        *self.seen_tally.lock().unwrap() = Some(scan.tally);
        *slot = Some((std::time::Instant::now(), scan.clone()));
        Ok(scan)
    }

    /// Pages `changes/zone` to the end and decodes everything it returns.
    async fn walk_zone(&self) -> Result<Scan, TransportError> {
        let (records, token, complete) = self.fetch_all_records().await?;
        Ok(self.decode_records(records, token, complete))
    }

    /// The raw records of one whole-zone read: **every field the server sent**,
    /// before anything decides what to keep.
    ///
    /// Split out of `walk_zone` so the write census (`census::report`) can run
    /// against the same read the app already performs, using the session the
    /// app already holds. `examples/icloud_probe` borrows icloud-md's stored
    /// cookie jar — scaffolding from before this project had an iCloud session
    /// of its own — and that jar expires, so measuring an account Jodd is
    /// signed into required a HAR capture through a third-party tool. The
    /// session is here; the read that feeds a diagnostic should be too.
    ///
    /// Returns `(records, resume token, did the walk reach the end)`.
    async fn fetch_all_records(
        &self,
    ) -> Result<(Vec<serde_json::Value>, Option<String>, bool), TransportError> {
        let http = reqwest::Client::new();
        let host = self.ck_hostname();
        let mut token: Option<String> = None;
        let mut records: Vec<serde_json::Value> = Vec::new();
        // A from-scratch walk cannot itself be rejected for a stale token —
        // it sent none. Retrying forever on a server that rejects everything
        // is the failure this flag prevents.
        let mut restarted = false;
        let mut page_no = 1usize;

        for _ in 0..MAX_PAGES {
            let header = self.cookie_header(&host, "/database/1").await?;
            let reply = match wire::fetch_zone_page(
                &http,
                &self.session.ck_host,
                &self.session.dsid,
                &self.session.client,
                &header,
                token.as_deref(),
            )
            .await
            {
                Ok(r) => r,
                Err(TransportError::Auth) => {
                    // 421 means this session is over, so the cached
                    // `/validate` result is describing a session that no
                    // longer exists — including its partition host. Dropping
                    // it is what lets the next read re-establish instead of
                    // replaying a dead bootstrap for the full MAX_AGE.
                    //
                    // The SESSION only: this runs under the scan lock that
                    // `scan()` holds across the walk, and a tokio mutex is not
                    // reentrant — `invalidate()` would hang the read forever.
                    self.scans.invalidate_session().await;
                    return Err(TransportError::Auth);
                }
                Err(e) => return Err(e),
            };

            match reply {
                wire::ZoneReply::TokenRejected(reason) => {
                    if token.is_none() || restarted {
                        return Err(TransportError::Permanent {
                            source: anyhow::anyhow!(
                                "CloudKit refused a from-scratch zone read ({reason})"
                            ),
                        });
                    }
                    // Correctness, not optimization: a merely old token still
                    // syncs, so a rejection means the server's history no
                    // longer covers us. Say so and re-read the zone.
                    crate::log!("icloud: sync token rejected ({reason}) — refetching the zone from scratch");
                    restarted = true;
                    token = None;
                    records.clear();
                    continue;
                }
                wire::ZoneReply::Page(page) => {
                    // Logged per page, not just at the end. A zone walk over a
                    // real account is hundreds of notes through gunzip and
                    // protobuf — tens of seconds in a debug build — and it runs
                    // inside sign-in with the window still open. Without this
                    // the log goes silent between "session established" and the
                    // verdict, which is indistinguishable from a hang. It was:
                    // the first successful live sign-in looked stuck.
                    crate::log!(
                        "icloud: zone page {} — {} record(s), {} so far{}",
                        page_no,
                        page.records.len(),
                        records.len() + page.records.len(),
                        if page.more_coming { ", more coming" } else { ", last page" }
                    );
                    page_no += 1;
                    records.extend(page.records);
                    token = page.sync_token;
                    if !page.more_coming {
                        return Ok((records, token, true));
                    }
                    if token.is_none() {
                        // `moreComing` with no token to resume from cannot be
                        // paged. Returning what arrived beats looping on the
                        // same first page forever.
                        crate::log!("icloud: moreComing with no syncToken — stopping the walk here");
                        return Ok((records, None, false));
                    }
                }
            }
        }

        crate::log!("icloud: zone walk hit the {MAX_PAGES}-page cap — returning what arrived");
        Ok((records, token, false))
    }

    /// The formatting half of a save (M3 F6): brings the just-text-edited
    /// document's attribute runs to what the editor HTML expressed, via the
    /// clone-overlay reconcile. **Failure is downgrade, not blockage** — the
    /// text edit has already landed in `edited`, and blocking a whole save
    /// over a formatting keystroke is the worse trade (M2's L3, inverted:
    /// now the exception rather than the rule). Logged with the reason.
    fn reconcile_formatting(
        edited: &mut compose::NoteDocument,
        parsed: &format_html::ParsedBody,
        replica_id: [u8; 16],
    ) {
        if parsed.text.is_empty() {
            return; // a title-only note has no body formatting to reconcile
        }
        if !format::model_applies(edited.text()) {
            // `\r`/`U+2029`: the read path renders these plain, so a decode
            // "succeeding" here would reconcile against a segmentation no
            // other layer uses (the model splits on `\n` only).
            crate::log!(
                "icloud: formatting downgraded to text-only: the text carries separators the \
                 paragraph model does not"
            );
            return;
        }
        let current = match format::decode_note_format(edited.text(), &edited.string.attribute_run) {
            Ok(p) => p,
            Err(e) => {
                crate::log!("icloud: formatting downgraded to text-only: {e}");
                return;
            }
        };
        // The desired document: the title-side prefix paragraphs exactly as
        // they are (identity ⇒ the reconciler never touches them — the title
        // is edited in a plain input and keeps its style), then the editor's
        // body paragraphs with their offsets shifted past the prefix.
        let body_lines = parsed.text.split('\n').count();
        let Some(cut) = current.len().checked_sub(body_lines) else {
            crate::log!(
                "icloud: formatting downgraded to text-only: body has more lines than the document"
            );
            return;
        };
        // **The stale-cache guard.** An editor body that is ENTIRELY plain
        // over a body that currently carries formatting is far more likely a
        // pre-M3 cached rendering echoing back than a deliberate
        // strip-everything — and reconciling it would delete the remote's
        // formatting server-side, silently: gotcha #17's class of landmine.
        // Skipping preserves the formatting (M2's exact behavior); the
        // benign cost is that a user who truly removes ALL formatting in
        // Jodd sees it come back on the next pull. Partial formatting
        // reconciles fully, removals included.
        let projects_plain = |p: &format::Paragraph| {
            format::projected_kind(p.kind) == format::ParagraphKind::Body
                && p.block_quote_level == 0
                && format::normalize_spans(p).iter().all(|s| s.style == format::InlineStyle::default())
        };
        if parsed.paragraphs.iter().all(projects_plain) && !current[cut..].iter().all(projects_plain) {
            crate::log!(
                "icloud: formatting left untouched — plain editor body over a formatted note \
                 (stale-cache guard)"
            );
            return;
        }
        let base = current.get(cut).map(|p| p.start).unwrap_or(0);
        let mut desired: Vec<format::Paragraph> = current[..cut].to_vec();
        desired.extend(
            parsed.paragraphs.iter().map(|p| format::Paragraph { start: p.start + base, ..p.clone() }),
        );
        let mut mint = || *uuid::Uuid::new_v4().as_bytes();
        if let Err(e) = format_reconcile::reconcile_note_format(edited, &desired, replica_id, &mut mint) {
            crate::log!("icloud: formatting downgraded to text-only: {e}");
        }
    }

    /// Turns raw records into notes and folders. Pure, given the records.
    fn decode_records(
        &self,
        records: Vec<serde_json::Value>,
        sync_token: Option<String>,
        complete: bool,
    ) -> Scan {
        let folder_paths = wire::build_folder_paths(&wire::folder_records(&records));

        // **One list, not two.** Every decoded record lands here with the two
        // things the dedupe below needs — whether that copy was in the Trash,
        // and Apple's own `ModificationDate`. Splitting `notes` and `trashed`
        // before deduping is what let one record be in both at once.
        let mut candidates: Vec<(Note, bool, Option<i64>)> = Vec::new();
        let mut tally = DecodeTally::default();
        // Keyed by `recordName`, so a record that arrives twice in the change
        // feed leaves the LATER base in place — the same rule the note dedupe
        // below applies, and for the same reason (gotcha #22). A write against
        // the earlier copy's `recordChangeTag` would be refused as a conflict
        // by the server, which is safe but confusing.
        let mut bases: HashMap<String, WriteBase> = HashMap::new();

        // A census of which FIELD NAMES appear on the notes that land in the
        // root, so the remaining discrepancy can name itself.
        //
        // The account shows 588 notes in the root where Apple shows 584, with
        // `orphaned` and `unfiled` both zero — so all 588 carry an explicit
        // `Folder` naming the default folder, and the difference is in what
        // Apple chooses to DISPLAY there, not in what Jodd decoded. Guessing
        // which four (Quick Notes was the standing hypothesis, and it accounts
        // for two at most) is how the last three of these were got wrong.
        //
        // Field NAMES only, never values: if some small subset of root notes
        // carries a field the rest do not, that field is the answer and this
        // prints it without a single character of note text reaching the log.
        //
        // Seeded with every key the request ASKS for, at zero. Without that,
        // a field no note carries never enters the map and reads exactly like
        // one every note carries — both are simply absent from a report that
        // lists what is not universal. `IsPinned` is precisely that case, and
        // the whole question about it is which of those two it is.
        let mut root_fields: HashMap<String, usize> =
            wire::DESIRED_KEYS.iter().map(|k| ((*k).to_string(), 0)).collect();
        let mut root_total = 0usize;

        // Per-user state, which is where Apple keeps the pin — see
        // `wire::USER_SPECIFIC_TYPES`.
        let pins = wire::collect_pins(&records);
        // Inline text attachments — the hashtag texts (M3 F5).
        let inline_refs = wire::collect_inline_refs(&records);
        {
            // A census over the per-user records, for the same reason the root
            // one exists: if Apple's field names are not what `collect_pins`
            // expects, every pin fails to join and the result is
            // indistinguishable from an account with nothing pinned. Names
            // only, never values.
            let mut fields: HashMap<String, usize> = HashMap::new();
            let mut total = 0usize;
            for r in records.iter().filter(|r| {
                matches!(r["recordType"].as_str(), Some(t) if wire::USER_SPECIFIC_TYPES.contains(&t))
            }) {
                census_fields(r, &mut fields, &mut total);
            }
            let mut rows: Vec<(&str, usize)> =
                fields.iter().map(|(k, &c)| (k.as_str(), c)).collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            crate::log!(
                "icloud: {} per-user record(s), fields: {} — {} pinned, {} tombstoned, \
                 {} with no note reference",
                total,
                rows.iter().map(|(k, c)| format!("{k}={c}")).collect::<Vec<_>>().join("  "),
                pins.pinned.len(),
                pins.deleted,
                pins.unjoinable
            );
        }

        // Locked notes arrive as their own record type and are skipped by the
        // `Note` filter below. Counting them here is what stops the skip being
        // silent — see `DecodeTally::locked`.
        for r in records.iter().filter(|r| r["recordType"] == serde_json::json!("PasswordProtectedNote")) {
            let Some(mut n) = wire::decode_locked_note(r, &folder_paths) else { continue };
            tally.locked += 1;
            bases.insert(n.uuid.clone(), wire::write_base(r, true));
            let folder = n.label.clone();
            n.account_id = Some(self.account_id.clone());
            n.pinned = pins.pinned.contains(&n.uuid);
            if folder == wire::ROOT_PATH {
                census_fields(r, &mut root_fields, &mut root_total);
            }
            candidates.push((n, false, wire::modification_ms(r)));
            // The title, never its text. A locked note's title is the one
            // thing its owner put behind a password; the count and the folder
            // are what a log needs to be useful.
            crate::log!(
                "icloud: {} is password-protected — shown with its title, body unreadable \
                 (folder {})",
                r["recordName"].as_str().unwrap_or("<unnamed>"),
                folder
            );
        }
        for r in records.iter().filter(|r| r["recordType"] == serde_json::json!("Note")) {
            match wire::decode_note_with_refs(r, &folder_paths, &inline_refs) {
                wire::Decoded::Note(mut n) => {
                    // The wire layer is account-blind, like every other
                    // backend's; the vertical stamps ownership.
                    n.account_id = Some(self.account_id.clone());
                    bases.insert(n.uuid.clone(), wire::write_base(r, false));
                    // The pin arrived on a different record; join it here,
                    // where both halves are in hand.
                    n.pinned = pins.pinned.contains(&n.uuid);
                    tally.decoded += 1;
                    // A note is an orphan when its Folder reference names a
                    // record this walk never saw. `decode_note` files those
                    // under the root, which is right and invisible — the count
                    // is what makes it visible.
                    match r["fields"]["Folder"]["value"]["recordName"].as_str() {
                        Some(fid) => {
                            if fid != wire::DEFAULT_FOLDER && !folder_paths.contains_key(fid) {
                                tally.orphaned += 1;
                                crate::log!(
                                    "icloud: note {} references unknown folder {} — filed under {}",
                                    n.uuid,
                                    fid,
                                    wire::ROOT_PATH
                                );
                            }
                        }
                        // No reference at all. Same destination, different
                        // cause — and the one the orphan check could never
                        // see. `Folders` (plural) is logged alongside it
                        // because it is the leading candidate for where the
                        // membership actually lives: it is in `DESIRED_KEYS`,
                        // it comes back on real records, and nothing reads it.
                        // Record names only — those are UUIDs, never content.
                        None => {
                            tally.unfiled += 1;
                            crate::log!(
                                "icloud: note {} carries no Folder reference — filed under {}; \
                                 Folders: {}",
                                n.uuid,
                                wire::ROOT_PATH,
                                describe_folders_field(&r["fields"]["Folders"])
                            );
                        }
                    }
                    if n.label == wire::ROOT_PATH {
                        census_fields(r, &mut root_fields, &mut root_total);
                    }
                    candidates.push((*n, false, wire::modification_ms(r)));
                }
                // Recoverable, not gone: kept aside so `list_trashed` can show
                // it and `fetch_note` can preview it, and deliberately NOT in
                // `notes` — its label is the root placeholder, so a note in the
                // Trash would otherwise appear in the root folder.
                wire::Decoded::Trashed(mut n) => {
                    tally.trashed += 1;
                    n.account_id = Some(self.account_id.clone());
                    bases.insert(n.uuid.clone(), wire::write_base(r, false));
                    candidates.push((*n, true, wire::modification_ms(r)));
                }
                wire::Decoded::Skipped { record_name, reason } => match reason {
                    wire::SkipReason::Deleted => tally.deleted += 1,
                    wire::SkipReason::Incomplete(why) => {
                        tally.incomplete += 1;
                        crate::log!("icloud: skipped {record_name} — {why}");
                    }
                    wire::SkipReason::Undecodable(doc::DecodeError::Unreadable(why)) => {
                        tally.unreadable += 1;
                        crate::log!("icloud: {record_name} did not decompress — {why}");
                    }
                    wire::SkipReason::Undecodable(doc::DecodeError::Malformed(why)) => {
                        tally.malformed += 1;
                        crate::log!("icloud: {record_name} is not a note document — {why}");
                    }
                },
            }
        }

        // A `changes/zone` walk is a CHANGE FEED, not a listing, and the same
        // record can come back on more than one page — a record modified while
        // the walk is in flight is the ordinary way it happens. The cache never
        // showed it because `(uuid, account_id)` is the primary key, so SQLite
        // collapsed the repeats on its own; the walk's own count did not, and
        // that is the whole disagreement the sidebar was showing: 778 from the
        // index against 772 in the cache, where 772 is also what Apple says.
        //
        // **Two things about the first version of this were wrong, and the
        // second one only shows up once a copy CHANGES FOLDER.** It deduped
        // `notes` and `trashed` as separate vectors, so a record that arrived
        // once filed and once in the Trash landed in BOTH and neither pass
        // could see the other — Apple showed one note in a folder and one in
        // Recently Deleted where Jodd showed zero and two, off by one in both
        // directions at once. And it kept whichever copy came LAST in the
        // walk, which is a guess about CloudKit's paging; `ModificationDate`
        // is Apple's own statement about the record. Feed order stays as the
        // tiebreak, and every case where the two disagree is logged, so the
        // assumption that replaced it is measured rather than assumed in turn.
        let (notes, trashed) = {
            let before = candidates.len();
            let mut at: HashMap<String, usize> = HashMap::new();
            let mut kept: Vec<(Note, bool, Option<i64>)> = Vec::with_capacity(before);
            let (mut moved_between, mut order_disagreed) = (0usize, 0usize);
            for (n, is_trashed, ms) in candidates {
                match at.get(&n.uuid) {
                    Some(&i) => {
                        let (prev, prev_trashed, prev_ms) = &kept[i];
                        if *prev_trashed != is_trashed {
                            moved_between += 1;
                            crate::log!(
                                "icloud: {} arrived both filed and trashed in one walk — \
                                 {} ({:?}) vs {} ({:?})",
                                n.uuid,
                                if *prev_trashed { "trashed" } else { &prev.label },
                                prev_ms,
                                if is_trashed { "trashed" } else { &n.label },
                                ms
                            );
                        }
                        // The later copy wins on feed order; a strictly older
                        // timestamp overrides that.
                        let older = match (ms, *prev_ms) {
                            (Some(a), Some(b)) => a < b,
                            _ => false,
                        };
                        if older {
                            order_disagreed += 1;
                        } else {
                            kept[i] = (n, is_trashed, ms);
                        }
                    }
                    None => {
                        at.insert(n.uuid.clone(), kept.len());
                        kept.push((n, is_trashed, ms));
                    }
                }
            }
            if before != kept.len() {
                crate::log!(
                    "icloud: {} record(s) arrived more than once in the walk — {} distinct; \
                     {} changed between filed and trashed, {} where the later copy was the \
                     OLDER one and feed order would have picked wrong",
                    before - kept.len(),
                    kept.len(),
                    moved_between,
                    order_disagreed
                );
            }
            let mut live = Vec::new();
            let mut binned = Vec::new();
            for (n, is_trashed, _) in kept {
                if is_trashed {
                    binned.push(n);
                } else {
                    live.push(n);
                }
            }
            (live, binned)
        };

        // Where the notes actually landed, biggest first.
        //
        // Added because a live account came back with the right TOTAL and the
        // wrong distribution — 776 notes decoded, but the root folder showed
        // empty while Apple had 584 there. A count alone cannot tell "the
        // decode lost them" from "something downstream did", and this can.
        {
            let mut by_label: HashMap<&str, usize> = HashMap::new();
            for n in &notes {
                *by_label.entry(n.label.as_str()).or_default() += 1;
            }
            let mut rows: Vec<(&str, usize)> = by_label.into_iter().collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            let shown: Vec<String> =
                rows.iter().take(12).map(|(l, c)| format!("{l}={c}")).collect();
            crate::log!(
                "icloud: notes by folder (top {} of {}): {}",
                shown.len(),
                rows.len(),
                shown.join("  ")
            );
        }

        // What the zone actually contains, by record type.
        //
        // 1176 records came back where notes and folders together account for
        // roughly 880, and nothing said what the rest were. That matters twice
        // over: Apple keeps per-user state (the pin among the candidates) in
        // separate `*_UserSpecific` records, and an unexplained population is
        // exactly where an unexplained count difference would hide. Record
        // TYPE names only — never a field, never a value.
        {
            let mut by_type: HashMap<&str, usize> = HashMap::new();
            for r in &records {
                *by_type.entry(r["recordType"].as_str().unwrap_or("<none>")).or_default() += 1;
            }
            let mut rows: Vec<(&str, usize)> = by_type.into_iter().collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            crate::log!(
                "icloud: {} record(s) in the zone by type: {}",
                records.len(),
                rows.iter().map(|(t, c)| format!("{t}={c}")).collect::<Vec<_>>().join("  ")
            );
        }

        // Which fields are NOT universal among the root's notes. A field
        // carried by only a handful of them is the discrepancy naming itself.
        {
            let mut odd: Vec<(&str, usize)> = root_fields
                .iter()
                .filter(|(_, &c)| c != root_total)
                .map(|(k, &c)| (k.as_str(), c))
                .collect();
            odd.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(b.0)));
            crate::log!(
                "icloud: root has {} note(s); fields not carried by all of them: {}",
                root_total,
                if odd.is_empty() {
                    "none — every root note has the identical field set".to_string()
                } else {
                    odd.iter().map(|(k, c)| format!("{k}={c}")).collect::<Vec<_>>().join("  ")
                }
            );
        }

        crate::log!(
            "icloud: {} note(s) in the result — this is the number to compare with \
             Apple's \"All iCloud\". The per-record counts below are RECORDS, and a \
             change feed can carry one note on more than one page, so they run higher.",
            notes.len()
        );

        crate::log!(
            "icloud: decoded {} note(s) — {} unreadable, {} malformed, {} incomplete, {} deleted, \
             {} orphaned into the root, {} unfiled into the root, \
             {} password-protected; {} folder(s)",
            tally.decoded,
            tally.unreadable,
            tally.malformed,
            tally.incomplete,
            tally.deleted,
            tally.orphaned,
            tally.unfiled,
            tally.locked,
            folder_paths.len()
        );

        // Where the pins landed. Record names and folders only — never a
        // title. Apple showed 2 pinned notes against 17 read from the per-user
        // records, and one of the two (in the root) did not appear pinned in
        // Jodd at all: an over-count and a miss at the same time, which one
        // number cannot tell apart. This lists them.
        {
            let mut rows: Vec<String> = notes
                .iter()
                .filter(|n| n.pinned)
                .map(|n| format!("{} in {}", n.uuid, n.label))
                .collect();
            rows.sort();
            crate::log!("icloud: {} pinned note(s) reached the result: {}",
                        rows.len(), rows.join("  |  "));
        }

        let mut folders: Vec<RemoteFolder> = folder_paths
            .into_iter()
            .map(|(id, path)| RemoteFolder { id, path })
            .collect();
        // A HashMap iterates in an arbitrary order, and `list_folders` feeds
        // the sidebar. Sorting keeps the tree stable between two reads of an
        // unchanged account instead of reshuffling on every refresh.
        folders.sort_by(|a, b| a.path.cmp(&b.path));

        // Same change-feed rule as `notes`: a record can arrive on more than
        // one page, and the later copy is the newer state (gotcha #22).
        let trashed = {
            let mut at: HashMap<String, usize> = HashMap::new();
            let mut out: Vec<Note> = Vec::with_capacity(trashed.len());
            for n in trashed {
                match at.get(&n.uuid) {
                    Some(&i) => out[i] = n,
                    None => {
                        at.insert(n.uuid.clone(), out.len());
                        out.push(n);
                    }
                }
            }
            out
        };

        Scan { notes, folders, sync_token, tally, bases, complete, trashed, inline_refs }
    }
}

impl Vertical for ICloudVertical {
    fn backend_id(&self) -> &str {
        "apple-via-icloud"
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// The ADP verdict, but **only if this instance already walked the zone**.
    ///
    /// `OnceCell::get` never blocks and never fetches, which is what makes this
    /// safe to call from a synchronous path: an instance that has not read yet
    /// answers `None` — "nothing known against this account" — rather than
    /// triggering a network round trip from a getter.
    fn blocked_reason(&self) -> Option<String> {
        let tally = (*self.seen_tally.lock().unwrap())?;
        AdpVerdict::of(&tally).blocked_reason()
    }
}

impl Identity for ICloudVertical {
    /// A **lowercase** v4 UUID, matching CloudKit's `recordName` shape.
    ///
    /// M1 never creates a record, so nothing calls this in anger. It is
    /// lowercase anyway because `canonical_uuid_for(ICloud, …)` is pass-through
    /// (gotcha #18): the moment M2 mints an id, an uppercase one would be a
    /// `recordName` naming nothing, and the symptom is a lookup that 404s only
    /// after the user's first save.
    fn mint(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

impl Deriver for ICloudVertical {
    /// `doc.rs` turns the note document into the same Apple-flavoured HTML the
    /// other backends carry, so the shared deriver applies unchanged and FTS,
    /// `[[wikilinks]]` and citations span iCloud for free.
    ///
    /// **Tags are the exception, and it is a known M1 limit rather than a bug
    /// here.** An inline `#hashtag` is an inline object on this backend, so the
    /// body text carries `U+FFFC` where the tag is and this deriver finds
    /// nothing to extract. The fix needs `attribute_run`, which is M2 —
    /// Component K in the design spec carries the options.
    fn derive(&self, kind: ContentKind, blob: &[u8]) -> Derived {
        crate::backend::deriver_applehtml::AppleHtmlDeriver.derive(kind, blob)
    }
}

#[async_trait]
impl NoteStore for ICloudVertical {
    /// `cache_by_id` is unused: `changes/zone` already carries every body, so
    /// there is no per-note fetch to skip.
    ///
    /// `DedupSummary` is always empty. Gmail re-mints a message id on every
    /// content edit, which is what produces the transient duplicates it dedups;
    /// a CloudKit record keeps its `recordName` for life, so one note is
    /// never two records.
    async fn list_all_notes(
        &self,
        _cache_by_id: &HashMap<String, Note>,
    ) -> Result<(Vec<Note>, DedupSummary), TransportError> {
        Ok((self.scan().await?.notes.clone(), DedupSummary::default()))
    }

    /// Filters the instance scan — CloudKit has no per-folder endpoint.
    ///
    /// The match is **exact**, not a subtree: a note carries one folder path,
    /// and the sidebar shows the count for that folder alone (gotcha #1). An
    /// unknown folder yields an empty Vec rather than an error, which is what
    /// the trait's callers rely on for a folder that exists locally and has no
    /// remote representation.
    async fn list_notes_in_folder(
        &self,
        folder: &str,
        _cache_by_id: &HashMap<String, Note>,
    ) -> Result<Vec<Note>, TransportError> {
        Ok(self
            .scan()
            .await?
            .notes
            .iter()
            .filter(|n| n.label == folder)
            .cloned()
            .collect())
    }

    async fn list_index(&self) -> Result<Vec<MessageIndex>, TransportError> {
        Ok(self
            .scan()
            .await?
            .notes
            .iter()
            .map(|n| MessageIndex { id: n.id.clone(), label: n.label.clone() })
            .collect())
    }

    /// Costs a zone walk for one note, like Microsoft's `fetch_note` costs a
    /// mailbox scan, and for the same reason: the note's `label` must come from
    /// the same folder map every other read builds, or two reads disagree about
    /// where a note lives. Every caller is an explicit user action.
    ///
    /// **Searches the Trash too**, because one of those callers is
    /// `get_trashed_note_preview` — a note in Recently Deleted is exactly the
    /// one it is asked for, and looking only at `notes` would answer
    /// `NotFound` for every row the trash view can show.
    async fn fetch_note(&self, remote_id: &str) -> Result<Note, TransportError> {
        let scan = self.scan().await?;
        scan.notes
            .iter()
            .chain(scan.trashed.iter())
            .find(|n| n.id == remote_id)
            .cloned()
            .ok_or(TransportError::NotFound)
    }

    /// Writes one note — create or in-place update.
    ///
    /// `attachments` is ignored and cannot be otherwise: the iCloud **web**
    /// Notes editor cannot attach a file, so there is no client behaviour to
    /// mimic, and `decode_note` never populates them on the way in either.
    async fn save_note_full(
        &self,
        op: &SaveOp<'_>,
        _attachments: &[Attachment],
    ) -> Result<SavedNote, TransportError> {
        let scan = self.scan().await?;
        let now = chrono::Utc::now().timestamp_millis();

        let (write, saved, previously_pinned) = match op.existing_remote_id.filter(|id| !id.is_empty()) {
            // ── update ──────────────────────────────────────────────────
            Some(id) => {
                let Some(base) = scan.bases.get(id) else {
                    // An absent record means "deleted on another device" ONLY
                    // if the whole zone was read. `push_one_dirty` answers a
                    // `NotFound` on an update by dropping the local row and the
                    // unpushed edit with it, so on a partial walk that answer
                    // is silent data loss — `Transient` retries instead.
                    return Err(if scan.complete {
                        TransportError::NotFound
                    } else {
                        TransportError::Transient {
                            source: anyhow::anyhow!(
                                "note {id} was not in a zone read that did not finish — \
                                 not writing until the whole zone has been seen"
                            ),
                        }
                    });
                };
                // Record TYPE, not body contents — Component H3's amendment.
                // The cached body is `wire::LOCKED_BODY_HTML`, and a guard that
                // compared against it would pass the moment the user edited
                // the placeholder.
                if base.locked {
                    return Err(refused(compose::Unwritable::Locked));
                }
                // Apple's own pin lives on a `Note_UserSpecific` record this
                // write does not touch (gotcha #23). Carrying it forward keeps
                // the folded cache honest; rebuilding the note with `false`
                // would repeat exactly the defect that made the pin work in one
                // view and nowhere else. Independent of the base, so computed
                // once rather than inside the conflict loop.
                let pinned = scan.notes.iter().find(|n| n.uuid == id).is_some_and(|n| n.pinned);

                // **The conflict-refresh loop — the fix for the stale
                // create changeTag (measured live 2026-09-09).** Apple bumps
                // a freshly-created note's `recordChangeTag` server-side
                // moments after the create reply hands one back (measured:
                // `euo` → `eup` with no Jodd write in between), so the tag
                // `cache_note` folded in is stale by the time the user's very
                // next edit pushes. The optimistic lock then fires `CONFLICT`
                // on a write that carries no concurrent edit at all — and the
                // old code just returned that error, so the worker re-sent the
                // identical stale-tag write every ~5s until a full zone walk
                // happened to refresh the base (up to `POLL_MS`, ~10 min).
                // For all of that window Apple shows the pre-edit content — a
                // deleted character still in the title — and a poll landing
                // mid-spin can mint a spurious keep-both copy (the 2026-08-26
                // duplication shape).
                //
                // On a `CONFLICT`, re-read the record's CURRENT tag with one
                // cheap point read (`records/lookup`, not another whole-zone
                // walk) and retry against it — but ONLY when the remote's
                // content is unchanged from the base we were overwriting, i.e.
                // the tag is all that moved. A remote whose content actually
                // differs is a genuine concurrent edit and stays a `Conflict`
                // for the poll's keep-both path (`reconcile_one`), unchanged.
                let mut base = base.clone();
                let mut refreshes = 0u8;
                loop {
                    let write = self.build_update_write(&scan, id, &base, op, now)?;
                    let derived_title =
                        wire::derive_note_title(write.display_text.as_deref().unwrap_or(&write.text));
                    crate::log!(
                        "icloud: records/modify update {} lock={:?} title_in={} TitleEncrypted={} text_head={}",
                        write.record_name,
                        write.change_tag,
                        head_codepoints(op.title, 12),
                        head_codepoints(derived_title, 12),
                        head_codepoints(&write.text, 12),
                    );
                    match self.modify(&wire::modify_note_body(&write)).await {
                        Ok(saved) => {
                            crate::log!(
                                "icloud: records/modify accepted {} → tag={} ModificationDate={:?}",
                                saved.record_name, saved.change_tag, saved.modified_ms
                            );
                            break (write, saved, pinned);
                        }
                        Err(TransportError::Conflict { .. }) if refreshes < MAX_CONFLICT_REFRESHES => {
                            refreshes += 1;
                            let Some(fresh) = self.lookup_write_base(id, base.locked).await? else {
                                // The record is gone from a point read — a real
                                // NotFound, decided the same way the initial
                                // lookup is (`push_one_dirty` handles it).
                                return Err(TransportError::NotFound);
                            };
                            // A tag that did not move, or remote content that
                            // genuinely differs, is NOT a stale-lock self-
                            // conflict — leave it as a Conflict for keep-both.
                            if fresh.change_tag == base.change_tag
                                || icloud_content_differs(&base, &fresh)
                            {
                                crate::log!(
                                    "icloud: conflict on {id} is a real concurrent edit \
                                     (base_tag={} fresh_tag={}) — leaving it for keep-both",
                                    base.change_tag, fresh.change_tag
                                );
                                return Err(TransportError::Conflict { remote_etag: None });
                            }
                            crate::log!(
                                "icloud: self-conflict on {id}: base_tag={} was stale (Apple \
                                 bumped it to {}), content unchanged — retrying against the \
                                 fresh tag",
                                base.change_tag, fresh.change_tag
                            );
                            base = fresh;
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            // ── create ──────────────────────────────────────────────────
            None => {
                let folder = self.folder_id_for(&scan, op.label)?;
                // Hashtag spans flatten to their TEXT on a create: a new
                // record cannot reference another note's InlineAttachment,
                // and a bare U+FFFC with no attachment run is a dangling
                // object Apple renders as nothing (the conflict-copy path
                // hits this — its body HTML is another note's rendering).
                let parsed_body = format_html::parse_editor_html_objects_as_text(op.body_html);
                let text = compose::compose_new(op.title, &parsed_body.text);
                // **With CRDT identity, or Apple discards the note within
                // minutes** — measured live 2026-08-27, three creates, all
                // tombstoned by Apple's own client with no delete ever asked
                // for. See `NoteDocument::new_with_replica`.
                let mut created = compose::NoteDocument::new_with_replica(&text, self.replica_id);
                // A note created with formatting gets real runs from day one
                // (M3 F6) — same reconcile, same downgrade rule.
                Self::reconcile_formatting(&mut created, &parsed_body, self.replica_id);
                let document = compose::encode(&created);
                let write = wire::NoteWrite {
                    display_text: None,
                    // The uuid Jodd already minted IS the `recordName` —
                    // `backend::mint_uuid_for(ICloud)` is lowercase for
                    // exactly this moment (gotcha #18). A create with no
                    // uuid to reuse mints one here rather than letting the
                    // server choose, so the cache never has to be rekeyed
                    // afterwards (gotcha #16).
                    record_name: op
                        .existing_uuid
                        .filter(|u| !u.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| self.mint()),
                    change_tag: None,
                    text,
                    document,
                    folder,
                    created_ms: Some(now),
                    modified_ms: now,
                    echo: serde_json::Map::new(),
                };
                crate::log!(
                    "icloud: records/modify create {} lock=None title_in={} TitleEncrypted={} text_head={}",
                    write.record_name,
                    head_codepoints(op.title, 12),
                    head_codepoints(wire::derive_note_title(&write.text), 12),
                    head_codepoints(&write.text, 12),
                );
                let saved = self.modify(&wire::modify_note_body(&write)).await?;
                crate::log!(
                    "icloud: records/modify accepted {} → tag={} ModificationDate={:?}",
                    saved.record_name, saved.change_tag, saved.modified_ms
                );
                (write, saved, false)
            }
        };
        drop(scan);

        let date = saved
            .modified_ms
            .or(Some(write.modified_ms))
            .and_then(wire::apple_date_from_ms)
            .unwrap_or_default();

        // Tell the cached scan what the server just said, rather than dropping
        // it — see `AccountCache::apply`.
        self.cache_note(
            Note {
                id: saved.record_name.clone(),
                uuid: saved.record_name.clone(),
                title: op.title.to_string(),
                body_html: op.body_html.to_string(),
                date: date.clone(),
                version: saved.change_tag.clone(),
                label: op.label.to_string(),
                x_mail_created_date: write.created_ms.and_then(wire::apple_date_from_ms),
                account_id: Some(self.account_id.clone()),
                pinned: previously_pinned,
                local_version: 0,
                push_blocked_reason: None,
                attachments: Vec::new(),
            },
            WriteBase {
                document: write.document.clone(),
                change_tag: saved.change_tag.clone(),
                created_ms: write.created_ms,
                folder_id: write.folder.clone(),
                // What the server now holds: the DERIVED title the write sent
                // (76-unit word-boundary truncation), not the caller's line.
                title_field: wire::derive_note_title(&write.text).to_string(),
                locked: false,
                echo: wire::echo_after_write(&write),
            },
        )
        .await;

        Ok(SavedNote {
            id: saved.record_name.clone(),
            version: saved.change_tag,
            uuid: saved.record_name,
            date,
            // Editor-view HTML, exactly what was handed in — the same
            // asymmetry `SavedNote::body_html` documents for every backend: the
            // cache stores what a read would hand back, not what went on the
            // wire.
            body_html: op.body_html.to_string(),
            local_version: 0,
        })
    }

    /// One uuid is one record on this backend — a `recordName` is fixed for
    /// the record's life, so a note is never two of them.
    ///
    /// Gmail's duplicates come from re-minting a message id on every content
    /// edit; nothing here does that, which is also why `DedupSummary` is always
    /// empty above.
    async fn find_ids_for_uuid(&self, uuid: &str) -> Result<Vec<String>, TransportError> {
        let scan = self.scan().await?;
        Ok(if scan.bases.contains_key(uuid) { vec![uuid.to_string()] } else { Vec::new() })
    }

    /// Apple's Recently Deleted, read out of the same zone walk everything else
    /// filters — there is no separate endpoint, and the records were already
    /// arriving all along (M1 counted them and threw them away).
    ///
    /// **`label` is the root, and on this backend that is a placeholder rather
    /// than a fact.** A trashed record's `Folder` reference names the Trash;
    /// the folder it came from is not on any field this code reads. `Folder`'s
    /// sibling `Folders` (plural) is the standing candidate — it is in
    /// `DESIRED_KEYS`, it comes back on real records, and nothing reads it —
    /// which is a hypothesis to measure, not one to act on. Until it is
    /// measured, restore must ask the user where the note goes rather than
    /// quietly filing it in the root: see `TrashedNote::original_known`.
    async fn list_trashed(&self) -> Result<Vec<TrashedNote>, TransportError> {
        Ok(self
            .scan()
            .await?
            .trashed
            .iter()
            .map(|n| TrashedNote {
                id: n.id.clone(),
                uuid: n.uuid.clone(),
                title: n.title.clone(),
                date: n.date.clone(),
                label: wire::ROOT_PATH.to_string(),
                original_known: false,
            })
            .collect())
    }

    /// **Never called on this backend, and refused rather than guessing.**
    ///
    /// `untrash` means "put it back where it was", and a trashed CloudKit
    /// record's `Folder` reference has been replaced by the Trash's — where it
    /// came from is not on any field this code reads. Restoring to the root
    /// instead would silently reorganise the user's account.
    ///
    /// `restore_note` (lib.rs) therefore routes this backend through
    /// `RestoreKind::MoveOutOfTrash`, which relocates the record to a folder
    /// the USER named. Nothing is lost by refusing here: a restore is one write
    /// either way, and there is no separate un-delete step to perform first.
    async fn untrash(&self, _remote_id: &str) -> Result<(), TransportError> {
        Err(unsupported(
            "a deleted note has to be restored to a folder you choose — Jodd cannot tell \
             which one it came from. Pick one, or restore it in Apple Notes",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{ChangeKind, MetadataSidecar, SidecarKind, Transport};
    use crate::icloud_auth::{ClientConfig, HarvestedCookie, StaticJar};
    use base64::Engine;
    use prost::Message;
    use serde_json::json;
    use std::sync::Arc;

    // ── fixtures ────────────────────────────────────────────────────────

    /// A fixed replica id for every test vertical — no test here exercises
    /// `ensure_icloud_replica_id`'s own minting (that lives in `accounts.rs`
    /// and `lib.rs`), so a constant keeps every fixture's `save_note_full`
    /// call deterministic instead of each needing its own placeholder.
    const TEST_REPLICA_ID: [u8; 16] = [0xAB; 16];

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
    }

    /// The real byte shape of a note body: protobuf, gzipped, base64'd.
    fn note_body(text: &str) -> String {
        use super::gen::{topotext, versioned_document};
        use flate2::write::GzEncoder;
        use std::io::Write;

        // One attribute run covering the whole text. Real records carry runs,
        // and M2's write gate refuses a document whose runs do not cover its
        // text — a fixture with none is a fixture no write path can exercise.
        let inner = topotext::String {
            string: text.to_string(),
            attribute_run: vec![topotext::AttributeRun {
                length: text.chars().map(char::len_utf16).sum::<usize>() as u32,
                ..Default::default()
            }],
            ..Default::default()
        };
        let doc = versioned_document::Document {
            serialization_version: Some(1),
            version: vec![versioned_document::Version {
                serialization_version: Some(1),
                minimum_supported_version: Some(1),
                data: Some(inner.encode_to_vec()),
            }],
        };
        let mut e = GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&doc.encode_to_vec()).unwrap();
        base64::engine::general_purpose::STANDARD.encode(e.finish().unwrap())
    }

    /// A CRDT-carrying document (M2.5): one origin run, one content run
    /// authored by `AUTHOR_REPLICA` (a different replica than
    /// `TEST_REPLICA_ID`, so a test exercising this joins the document as a
    /// new replica — the realistic shape of Jodd editing a note Apple's own
    /// client wrote), one end sentinel. Mirrors `crdt.rs`'s own
    /// `simple_document` test fixture shape.
    const AUTHOR_REPLICA: [u8; 16] = [0xCD; 16];

    fn note_body_crdt(text: &str) -> String {
        use super::gen::{topotext, versioned_document};
        use flate2::write::GzEncoder;
        use std::io::Write;

        let len = text.chars().map(char::len_utf16).sum::<usize>() as u32;
        let inner = topotext::String {
            string: text.to_string(),
            substring: vec![
                topotext::Substring {
                    char_id: topotext::CharId { replica_id: 0, clock: 0 },
                    length: 0,
                    timestamp: topotext::CharId { replica_id: 0, clock: 0 },
                    tombstone: None,
                    child: vec![1],
                },
                topotext::Substring {
                    char_id: topotext::CharId { replica_id: 1, clock: 0 },
                    length: len,
                    timestamp: topotext::CharId { replica_id: 1, clock: 0 },
                    tombstone: None,
                    child: vec![2],
                },
                topotext::Substring {
                    char_id: topotext::CharId { replica_id: 0, clock: 0xFFFF_FFFF },
                    length: 0,
                    timestamp: topotext::CharId { replica_id: 0, clock: 0xFFFF_FFFF },
                    tombstone: None,
                    child: Vec::new(),
                },
            ],
            timestamp: Some(topotext::VectorTimestamp {
                clock: vec![topotext::vector_timestamp::Clock {
                    replica_uuid: AUTHOR_REPLICA.to_vec(),
                    replica_clock: vec![
                        topotext::vector_timestamp::clock::ReplicaClock { clock: len, subclock: None },
                        topotext::vector_timestamp::clock::ReplicaClock { clock: 1, subclock: None },
                    ],
                }],
            }),
            attribute_run: vec![topotext::AttributeRun { length: len, ..Default::default() }],
        };
        let doc = versioned_document::Document {
            serialization_version: Some(1),
            version: vec![versioned_document::Version {
                serialization_version: Some(1),
                minimum_supported_version: Some(1),
                data: Some(inner.encode_to_vec()),
            }],
        };
        let mut e = GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&doc.encode_to_vec()).unwrap();
        base64::engine::general_purpose::STANDARD.encode(e.finish().unwrap())
    }

    /// Same shape as `note_rec`, but the body carries real CRDT identity
    /// (`note_body_crdt`) instead of the opaque single-run document.
    fn note_rec_crdt(name: &str, title: &str, text: &str, folder: &str) -> serde_json::Value {
        let document = if title.is_empty() {
            text.to_string()
        } else if text.is_empty() {
            title.to_string()
        } else {
            format!("{title}\n{text}")
        };
        json!({
            "recordName": name,
            "recordType": "Note",
            "recordChangeTag": "tag1",
            "fields": {
                "TitleEncrypted": { "value": b64(title) },
                "TextDataEncrypted": { "value": note_body_crdt(&document) },
                "Folder": { "value": { "recordName": folder } },
                "CreationDate": { "value": 1_600_000_000_000i64 },
                "ModificationDate": { "value": 1_700_000_000_000i64 },
            }
        })
    }

    fn folder_rec(name: &str, title: &str, parent: Option<&str>) -> serde_json::Value {
        let mut f = json!({ "TitleEncrypted": { "value": b64(title) } });
        if let Some(p) = parent {
            f["ParentFolder"] = json!({ "value": { "recordName": p } });
        }
        json!({ "recordName": name, "recordType": "Folder", "fields": f })
    }

    /// `text` is the note's BODY. The document gets the title as its first
    /// line, which is what a real record carries — Apple keeps no separate
    /// title, and `doc::note_title` now reads the line rather than the record's
    /// lossy `TitleEncrypted`.
    fn note_rec(name: &str, title: &str, text: &str, folder: &str) -> serde_json::Value {
        let document = if title.is_empty() {
            text.to_string()
        } else if text.is_empty() {
            title.to_string()
        } else {
            format!("{title}\n{text}")
        };
        json!({
            "recordName": name,
            "recordType": "Note",
            "recordChangeTag": "tag1",
            "fields": {
                "TitleEncrypted": { "value": b64(title) },
                "TextDataEncrypted": { "value": note_body(&document) },
                "Folder": { "value": { "recordName": folder } },
                "CreationDate": { "value": 1_600_000_000_000i64 },
                "ModificationDate": { "value": 1_700_000_000_000i64 },
            }
        })
    }

    fn zone_body(records: Vec<serde_json::Value>, token: &str, more: bool) -> String {
        json!({ "zones": [{ "records": records, "syncToken": token, "moreComing": more }] })
            .to_string()
    }

    fn jar() -> Arc<StaticJar> {
        // Scoped to the loopback host mockito serves from, so the RFC 6265
        // matching in `cookie_header_for` (exhaustively tested next door)
        // produces a non-empty header here.
        Arc::new(StaticJar(vec![HarvestedCookie {
            name: "X-APPLE-WEBAUTH-TOKEN".into(),
            value: "session".into(),
            domain: "127.0.0.1".into(),
            path: "/".into(),
            host_only: false,
            secure: true,
        }]))
    }

    fn vertical_at(url: &str, cookies: Arc<dyn CookieSource>) -> ICloudVertical {
        ICloudVertical::new(
            IcloudSession {
                apple_id: "kaiwan@me.com".into(),
                dsid: "12345".into(),
                ck_host: url.to_string(),
                client: ClientConfig {
                    client_build_number: "2628Build44".into(),
                    client_mastering_number: "2628B36".into(),
                    client_id: "ABC".into(),
                },
            },
            cookies,
            "icloud:kaiwan@me.com".into(),
            // A fresh cache per test vertical, so a test that asserts on the
            // request count is measuring its own walk and not a neighbour's.
            std::sync::Arc::new(AccountCache::default()),
            TEST_REPLICA_ID,
        )
    }

    /// Two verticals over ONE cache — the shape `vertical_for` produces for
    /// two operations on the same account.
    fn pair_at(url: &str) -> (ICloudVertical, ICloudVertical) {
        let shared = std::sync::Arc::new(AccountCache::default());
        let build = || {
            ICloudVertical::new(
                IcloudSession {
                    apple_id: "kaiwan@me.com".into(),
                    dsid: "12345".into(),
                    ck_host: url.to_string(),
                    client: ClientConfig {
                        client_build_number: "2628Build44".into(),
                        client_mastering_number: "2628B36".into(),
                        client_id: "ABC".into(),
                    },
                },
                jar(),
                "icloud:kaiwan@me.com".into(),
                shared.clone(),
                TEST_REPLICA_ID,
            )
        };
        (build(), build())
    }

    /// A jar that always fails — what a closed or missing webview looks like.
    struct DeadJar;
    #[async_trait]
    impl CookieSource for DeadJar {
        async fn harvest(&self) -> Result<Vec<HarvestedCookie>, String> {
            Err("no iCloud webview to harvest a session from".into())
        }
    }

    // ── identity, capabilities, the read-only surface ───────────────────

    #[test]
    fn it_identifies_itself_and_declares_a_partial_write_surface() {
        let v = vertical_at("https://p149-ckdatabasews.icloud.com", jar());
        assert_eq!(v.backend_id(), "apple-via-icloud");
        let w = v.capabilities().writes;
        // Content editing is ON (2026-08-26 evening): the duplicated-note
        // merge was root-caused to the replica table's serialization order
        // and fixed in `crdt::ensure_replica` (editor first, runs
        // renumbered — the shape a live icloud.com capture pinned), then
        // confirmed by two fresh-note live passes that survived the
        // delayed-merge window. See `backend::mod.rs`'s ICloud doc comment.
        assert!(w.notes, "content editing is on — table-order fix confirmed live");
        assert!(!w.sidecars, "the pin is Apple's own; a Jodd sidecar write would be overwritten");
        // Relocation and folder create/rename/delete are the OPPOSITE case:
        // measured safe live, 2026-08-24, on three independent Apple surfaces
        // — none of them ever sends TextDataEncrypted, so none of them can
        // touch what the CRDT gate above refuses.
        assert!(w.relocate, "move/trash/restore never touch the document — measured live");
        assert!(w.folders, "create/rename(same parent)/delete — measured live");
        assert!(
            v.capabilities().has_trash,
            "a delete files the note in Apple's Recently Deleted, and M2 can show it"
        );
    }

    #[test]
    fn mint_is_lowercase_because_a_record_name_is() {
        // gotcha #18: `canonical_uuid_for(ICloud, …)` is pass-through, so an
        // uppercase mint would become a recordName naming nothing the first
        // time M2 creates a record.
        let v = vertical_at("https://x", jar());
        let id = v.mint();
        assert_eq!(id, id.to_lowercase(), "got {id}");
        assert!(uuid::Uuid::parse_str(&id).is_ok(), "got {id}");
        assert_ne!(v.mint(), v.mint());
    }

    #[test]
    fn the_deriver_is_the_shared_apple_html_one() {
        let v = vertical_at("https://x", jar());
        let d = v.derive(ContentKind::AppleHtml, b"<div>see [[Other Note-abcd1234]]</div>");
        assert_eq!(d.edges.len(), 1);
        assert_eq!(d.edges[0].rel, "mentions");
    }

    #[tokio::test]
    async fn the_sidecar_store_reports_uninitialized_never_empty() {
        // The difference is data loss: Some(vec![]) tells the core it may
        // prune local pins to nothing, and iCloud has no sidecar store to
        // enumerate in the first place.
        let v = vertical_at("https://x", jar());
        assert!(v.list_sidecars(SidecarKind::Pin).await.unwrap().is_none());
    }

    /// The trash view reads out of the same zone walk everything else filters —
    /// there is no separate endpoint, and these records were arriving all
    /// along; M1 counted them and threw them away.
    ///
    /// **`original_known` is false**, and that is the whole reason the restore
    /// UI asks rather than assumes: a trashed record's `Folder` reference has
    /// been replaced by the Trash's, so the root reported here is a
    /// placeholder, not the folder the note came from.
    #[tokio::test]
    async fn the_trash_view_lists_what_is_in_recently_deleted() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(zone_body(
                vec![
                    folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                    note_rec("n1", "Alive", "body", wire::DEFAULT_FOLDER),
                    note_rec("n2", "Deleted", "body", wire::TRASH_FOLDER),
                ],
                "T1",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let scan = v.scan().await.unwrap();
        assert_eq!(scan.notes.len(), 1, "a trashed note is not in any listing");
        assert_eq!(scan.tally.trashed, 1, "and it is counted apart from a tombstone");

        let trash = v.list_trashed().await.unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].title, "Deleted");
        assert!(
            !trash[0].original_known,
            "the folder it came from is not on any field this decode reads"
        );

        // The preview reads through `fetch_note`, so a trashed note has to be
        // findable there or every row in the view fails to open.
        assert_eq!(v.fetch_note("n2").await.unwrap().body_html, "<div>body</div>");
    }

    /// A delete must leave every listing AND appear in the trash view, without
    /// waiting for the next whole-zone walk — `has_trash` is true, so there is
    /// a view for it to be missing from.
    #[tokio::test]
    async fn a_deleted_note_moves_into_the_cached_trash_rather_than_vanishing() {
        let mut server = mockito::Server::new_async().await;
        let (zone, _m, _s) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                note_rec("n1", "T", "b", wire::DEFAULT_FOLDER),
            ],
            saved_reply("n1", "tag9"),
        );
        let zone = zone.expect(1);

        let v = vertical_at(&server.url(), jar());
        v.delete("n1").await.unwrap();
        let after = v.scan().await.unwrap();
        zone.assert();
        assert!(after.notes.is_empty(), "gone from the listing");
        assert_eq!(after.trashed.len(), 1, "and present in Recently Deleted");
        assert_eq!(v.list_trashed().await.unwrap()[0].id, "n1");
    }

    /// Restoring is a move OUT of the Trash, so the note must leave the cached
    /// trash as well as arrive in the listing — otherwise it shows in both.
    #[tokio::test]
    async fn restoring_moves_the_note_out_of_the_cached_trash() {
        let mut server = mockito::Server::new_async().await;
        let (zone, _m, _s) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                folder_rec("f1", "Work", Some(wire::DEFAULT_FOLDER)),
                note_rec("n1", "T", "b", wire::TRASH_FOLDER),
            ],
            saved_reply("n1", "tag9"),
        );
        let zone = zone.expect(1);

        let v = vertical_at(&server.url(), jar());
        assert_eq!(v.list_trashed().await.unwrap().len(), 1);
        v.move_note("n1", std::slice::from_ref(&"Notes/Work".to_string()), &[])
            .await
            .unwrap();
        let after = v.scan().await.unwrap();
        zone.assert();
        assert!(after.trashed.is_empty(), "no longer in Recently Deleted");
        assert_eq!(after.notes.len(), 1);
        assert_eq!(after.notes[0].label, "Notes/Work");
    }

    #[tokio::test]
    /// The writes M2 does NOT do, each refused permanently and each pointing
    /// at the one place the user can actually do it.
    ///
    /// **The refusal must not name a milestone.** M1's did, truthfully, and
    /// then M2 arrived: a message that says "not until Milestone 2" on a
    /// milestone that has shipped is the same defect as
    /// `SIDECARS_UNAVAILABLE_MSG` naming Microsoft and Milestone 4 after M4
    /// turned sidecars on there. These are refusals with reasons, not
    /// placeholders.
    async fn the_writes_this_backend_does_not_do_are_refused_with_a_way_out() {
        let v = vertical_at("https://x", jar());
        let errs: Vec<TransportError> = vec![
            v.ensure_folder("Notes/x").await.unwrap_err(),
            // `move_note`, `create_folder`, `rename_folder` and
            // `delete_folder` are deliberately absent: all four are real
            // writes on this backend now (M2 live pass, 2026-08-24). Their
            // own tests below cover what they do and don't accept.
            v.put_sidecar("u", SidecarKind::Pin, None, None).await.unwrap_err(),
            v.remove_sidecar("id").await.unwrap_err(),
            v.untrash("id").await.unwrap_err(),
        ];
        for e in errs {
            match e {
                // Permanent, not Transient: nothing about retrying makes a
                // folder writable, and a Transient would have the worker loop
                // forever (gotcha #14's 5,816 attempts).
                TransportError::Permanent { source } => {
                    let msg = source.to_string();
                    assert!(
                        !msg.contains("Milestone"),
                        "a refusal that names a milestone goes stale the day it ships: {msg}"
                    );
                    assert!(
                        msg.contains("Apple Notes"),
                        "a refusal must say where the user CAN do it: {msg}"
                    );
                }
                other => panic!("expected Permanent, got {other:?}"),
            }
        }
    }

    // ── M2: writes ──────────────────────────────────────────────────────

    /// A zone with one folder and one note, plus a `records/modify` endpoint
    /// that answers with `reply` and records what it was sent.
    ///
    /// Two mocks on one server, matched by PATH — the walk and the write go to
    /// the same host, and a `Matcher::Any` on both would let either answer
    /// either, which is how a write test silently asserts nothing.
    fn write_server(
        server: &mut mockito::ServerGuard,
        records: Vec<serde_json::Value>,
        reply: serde_json::Value,
    ) -> (mockito::Mock, mockito::Mock, Arc<std::sync::Mutex<Vec<u8>>>) {
        let zone = server
            .mock("POST", mockito::Matcher::Regex("changes/zone".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(zone_body(records, "T1", false))
            .create();
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = sent.clone();
        let modify = server
            .mock("POST", mockito::Matcher::Regex("records/modify".into()))
            .match_request(move |req| {
                if let Ok(b) = req.body() {
                    *capture.lock().unwrap() = b.clone();
                }
                true
            })
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(reply.to_string())
            .create();
        (zone, modify, sent)
    }

    fn saved_reply(name: &str, tag: &str) -> serde_json::Value {
        json!({
            "records": [{
                "recordName": name,
                "recordType": "Note",
                "recordChangeTag": tag,
                "fields": { "ModificationDate": { "value": 1_700_000_000_002i64 } }
            }]
        })
    }

    fn sent_json(sent: &Arc<std::sync::Mutex<Vec<u8>>>) -> serde_json::Value {
        serde_json::from_slice(&sent.lock().unwrap()).expect("the write must send JSON")
    }

    /// The document that actually went on the wire, decoded back.
    fn sent_document(sent: &Arc<std::sync::Mutex<Vec<u8>>>) -> compose::NoteDocument {
        let b = sent_json(sent);
        let b64 = b["operations"][0]["record"]["fields"]["TextDataEncrypted"]["value"]
            .as_str()
            .expect("a content write must carry a document");
        compose::parse(&base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap()
    }

    fn edit(id: Option<&str>, title: &str, body: &str, label: &str) -> SaveOp<'static> {
        // Leaked so the op can outlive the call in a test helper; a test
        // process is the only place this is acceptable and it is bounded.
        SaveOp {
            title: Box::leak(title.to_string().into_boxed_str()),
            body_html: Box::leak(body.to_string().into_boxed_str()),
            existing_remote_id: id.map(|s| &*Box::leak(s.to_string().into_boxed_str())),
            existing_uuid: None,
            existing_created_date: None,
            label: Box::leak(label.to_string().into_boxed_str()),
        }
    }

    #[tokio::test]
    async fn an_edit_goes_out_as_an_update_carrying_the_records_own_change_tag() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                note_rec("n1", "Groceries", "milk", wire::DEFAULT_FOLDER),
            ],
            saved_reply("n1", "tag2"),
        );

        let v = vertical_at(&server.url(), jar());
        let saved = v
            .save_note_full(&edit(Some("n1"), "Groceries", "<div>milk</div><div>eggs</div>", "Notes"), &[])
            .await
            .expect("an ordinary edit must go through");
        m.assert();

        let b = sent_json(&sent);
        assert_eq!(b["operations"][0]["operationType"], json!("update"));
        assert_eq!(
            b["operations"][0]["record"]["recordChangeTag"],
            json!("tag1"),
            "the tag the READ observed is the optimistic lock the write sends back"
        );
        assert_eq!(sent_document(&sent).text(), "Groceries\nmilk\neggs");
        assert_eq!(saved.version, "tag2", "the note's version is the NEW tag");
        assert_eq!(saved.id, "n1");
    }

    /// Reported live 2026-09-09 (`0b845432…`, `icloud:kaiwan@me.com`): the
    /// user mistyped a character at the head of a NEW note's title, autosave
    /// pushed the CREATE, they pressed backspace, the EDIT pushed 11 seconds
    /// later — and Apple Notes showed the deleted character still in front
    /// of the title while Jodd's own copy was clean. The CRDT engine was
    /// cleared first (`crdt::tests::deleting_the_very_first_character_…`),
    /// but that test edits a foreign replica's run; live, the SAME replica
    /// deletes the head of the run it minted one push earlier, and the base
    /// it edits is the one `cache_note` folded in after the create, not a
    /// zone read. This runs exactly that sequence through `save_note_full`
    /// — the shipped create path, the cache fold, the shipped update path —
    /// and reads both `records/modify` bodies back the way Apple would.
    ///
    /// ASCII first, because the mechanism is the offset and not the script;
    /// the Thai case reproduces the report verbatim.
    #[tokio::test]
    async fn a_character_deleted_from_the_head_of_the_title_between_two_pushes_is_gone_from_the_second() {
        for (typed, corrected, body) in [
            ("Xnote test", "note test", "body"),
            ("\u{0e37}note in F1 from Jodd", "note in F1 from Jodd", "สวัสดี"),
            ("ab", "b", ""),
        ] {
            const NAME: &str = "0b845432-72b3-48a6-9954-ac59feef9fbe";
            let mut server = mockito::Server::new_async().await;
            let _zone = server
                .mock("POST", mockito::Matcher::Regex("changes/zone".into()))
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(zone_body(vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None)], "T1", false))
                .create();
            // Every body the write endpoint receives, in order — one capture
            // slot would keep only the edit and lose the create it must be
            // read against.
            let bodies: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
            let capture = bodies.clone();
            let modify = server
                .mock("POST", mockito::Matcher::Regex("records/modify".into()))
                .match_request(move |req| {
                    if let Ok(b) = req.body() {
                        capture.lock().unwrap().push(b.clone());
                    }
                    true
                })
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(saved_reply(NAME, "tag-after-create").to_string())
                .expect(2)
                .create();

            let v = vertical_at(&server.url(), jar());
            let body_html = if body.is_empty() { String::new() } else { format!("<div>{body}</div>") };
            let mut create = edit(None, typed, &body_html, "Notes");
            create.existing_uuid = Some(NAME);
            v.save_note_full(&create, &[]).await.expect("the create must go through");
            v.save_note_full(&edit(Some(NAME), corrected, &body_html, "Notes"), &[])
                .await
                .expect("the edit must go through");
            modify.assert();

            let bodies = bodies.lock().unwrap().clone();
            assert_eq!(bodies.len(), 2, "one create, one edit");
            let first: serde_json::Value = serde_json::from_slice(&bodies[0]).unwrap();
            let second: serde_json::Value = serde_json::from_slice(&bodies[1]).unwrap();
            assert_eq!(first["operations"][0]["operationType"], json!("create"));
            assert_eq!(second["operations"][0]["operationType"], json!("update"));
            assert_eq!(
                second["operations"][0]["record"]["recordChangeTag"],
                json!("tag-after-create"),
                "the edit's optimistic lock is the tag the CREATE's reply carried"
            );

            let expected_text = if body.is_empty() { corrected.to_string() } else { format!("{corrected}\n{body}") };
            let fields = &second["operations"][0]["record"]["fields"];
            let title_sent = base64::engine::general_purpose::STANDARD
                .decode(fields["TitleEncrypted"]["value"].as_str().unwrap())
                .unwrap();
            assert_eq!(
                String::from_utf8(title_sent).unwrap(),
                corrected,
                "TitleEncrypted on the edit ({typed:?} → {corrected:?})"
            );
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(fields["TextDataEncrypted"]["value"].as_str().unwrap())
                .unwrap();
            // The read path's view of it — what a later Jodd pull would show.
            assert_eq!(doc::decode_note_text(&bytes).unwrap(), expected_text, "read-path text ({typed:?})");
            // And the CRDT view — what Apple's merge reads: the text the
            // visible runs cover, the minted run tombstoned, one live run
            // carrying the corrected text under the SAME replica.
            let sent = compose::parse(&bytes).unwrap();
            let crdt = crdt::parse_crdt_document(&sent.string).expect("a note Jodd created carries CRDT identity");
            let crdt = &crdt;
            assert_eq!(crdt.text, expected_text);
            crdt::validate_document_invariants(crdt).unwrap();
            let live: Vec<&crdt::TextRun> =
                crdt.runs.iter().filter(|r| !r.tombstone && r.length > 0).collect();
            let dead: Vec<&crdt::TextRun> = crdt.runs.iter().filter(|r| r.tombstone).collect();
            assert_eq!(live.len(), 1, "one live run after the edit: {:?}", crdt.runs);
            assert_eq!(dead.len(), 1, "the created run is tombstoned whole: {:?}", crdt.runs);
            assert_eq!(live[0].length as usize, expected_text.encode_utf16().count());
            assert_eq!(dead[0].length as usize, format!("{typed}{}", if body.is_empty() { String::new() } else { format!("\n{body}") }).encode_utf16().count());
            assert_eq!(live[0].coord.replica, dead[0].coord.replica, "same replica on both");
            assert_eq!(crdt.replicas.len(), 1, "one replica in the table: {:?}", crdt.replicas);
            assert_eq!(
                dead[0].coord.clock + dead[0].length,
                live[0].coord.clock,
                "the re-inserted run's clock continues where the tombstoned one ended"
            );
        }
    }

    /// **M2.5's dispatch, live in `save_note_full`.** A document with CRDT
    /// identity (`note_rec_crdt`) must go through `with_text_crdt`, not the
    /// opaque `with_text` — the sent document must still carry `substring`/
    /// `timestamp` afterward (an opaque re-length would leave those fields
    /// exactly as they arrived, never minting an insertion run).
    #[tokio::test]
    async fn an_edit_of_a_crdt_carrying_note_goes_through_the_crdt_engine() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                note_rec_crdt("n1", "Groceries", "milk", wire::DEFAULT_FOLDER),
            ],
            saved_reply("n1", "tag2"),
        );

        let v = vertical_at(&server.url(), jar());
        let saved = v
            .save_note_full(&edit(Some("n1"), "Groceries", "<div>milk and eggs</div>", "Notes"), &[])
            .await
            .expect("a CRDT-writable edit must go through");
        m.assert();

        let sent_doc = sent_document(&sent);
        assert_eq!(sent_doc.text(), "Groceries\nmilk and eggs");
        assert!(
            sent_doc.string.substring.len() >= 3,
            "the sent document must still carry CRDT identity — an opaque with_text would \
             have left it exactly as it arrived, never adding an insertion run"
        );
        assert!(sent_doc.string.timestamp.is_some(), "the replica clock table must survive the edit");
        // TEST_REPLICA_ID must have joined the document's own replica table —
        // proof the edit was authored under Jodd's real replica identity, not
        // silently dropped or invented.
        let joined = sent_doc
            .string
            .timestamp
            .as_ref()
            .unwrap()
            .clock
            .iter()
            .any(|c| c.replica_uuid == TEST_REPLICA_ID.to_vec());
        assert!(joined, "TEST_REPLICA_ID must appear in the replica clock table after the edit");
        assert_eq!(saved.version, "tag2");
    }

    /// The counterpart: a note with NO CRDT identity still takes the opaque
    /// path exactly as before M2.5 — this dispatch must not regress the
    /// backend's original (M2) write behavior for the common case.
    #[tokio::test]
    async fn an_edit_of_an_opaque_note_still_uses_with_text_unchanged() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                note_rec("n1", "Groceries", "milk", wire::DEFAULT_FOLDER),
            ],
            saved_reply("n1", "tag2"),
        );

        let v = vertical_at(&server.url(), jar());
        v.save_note_full(&edit(Some("n1"), "Groceries", "<div>milk and eggs</div>", "Notes"), &[])
            .await
            .expect("an ordinary edit must go through");
        m.assert();

        let sent_doc = sent_document(&sent);
        assert_eq!(sent_doc.text(), "Groceries\nmilk and eggs");
        assert!(sent_doc.string.substring.is_empty(), "an opaque document must stay opaque");
        assert!(sent_doc.string.timestamp.is_none());
    }

    // ── Transport::create_folder / rename_folder / delete_folder ────────
    //
    // The three writes gotcha's "ambiguity closes on a convention" note
    // depends on: create must send what `parent_for_path` derives, rename
    // must refuse to double as a reparent, delete must send the record's own
    // change tag. `write_server`'s zone mock answers as many times as asked
    // (`fetch_all_records` never uses the cached scan) — these calls hit it
    // more than once per test on purpose.

    #[tokio::test]
    async fn a_folder_create_sends_the_derived_parent_and_nothing_else() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None)],
            json!({ "records": [{
                "recordName": "ignored",
                "recordType": "Folder",
                "recordChangeTag": "ftag1",
                "fields": {},
            }]}),
        );
        let v = vertical_at(&server.url(), jar());

        // A sibling of Notes: parent_for_path derives `None` for this shape,
        // the same convention M2's live pass measured (root: 0 on a real
        // account).
        let created = v.create_folder("Notes/New Folder").await.expect("must create");
        m.assert();

        let b = sent_json(&sent);
        assert_eq!(b["operations"][0]["operationType"], json!("create"));
        assert_eq!(b["operations"][0]["record"]["recordType"], json!("Folder"));
        let f = &b["operations"][0]["record"]["fields"];
        assert!(f["ParentFolder"].is_null(), "a top-level folder carries no parent");
        assert_eq!(
            wire::decode_text_field(&f["TitleEncrypted"]).as_deref(),
            Some("New Folder"),
            "the title is the path's own leaf"
        );
        assert_eq!(created.path, "Notes/New Folder");
    }

    #[tokio::test]
    async fn a_folder_create_under_an_existing_folder_copies_its_id_as_parent() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                folder_rec("f-parent", "Parent", None),
            ],
            json!({ "records": [{
                "recordName": "ignored", "recordType": "Folder",
                "recordChangeTag": "ftag1", "fields": {},
            }]}),
        );
        let v = vertical_at(&server.url(), jar());

        v.create_folder("Notes/Parent/Child").await.expect("must create");
        m.assert();

        let f = &sent_json(&sent)["operations"][0]["record"]["fields"];
        assert_eq!(
            f["ParentFolder"]["value"]["recordName"],
            json!("f-parent"),
            "nested under a real folder — its id is the parent, copied not guessed"
        );
    }

    #[tokio::test]
    async fn a_folder_create_at_the_root_itself_is_refused_before_any_write() {
        let mut server = mockito::Server::new_async().await;
        let zone = server
            .mock("POST", mockito::Matcher::Regex("changes/zone".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(zone_body(vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None)], "T1", false))
            .create();
        // No records/modify mock at all — a write here is the test failing.
        let v = vertical_at(&server.url(), jar());

        let _ = zone; // the mock's mere presence is what stops a connection error
        let err = v.create_folder("Notes").await.unwrap_err();
        match err {
            TransportError::Permanent { source } => {
                assert!(source.to_string().contains("Notes.app"));
            }
            other => panic!("expected Permanent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_same_parent_rename_sends_only_the_title() {
        let mut server = mockito::Server::new_async().await;
        let mut old = folder_rec(wire::DEFAULT_FOLDER, "Notes", None);
        old["recordName"] = json!(wire::DEFAULT_FOLDER);
        let mut renaming = folder_rec("f1", "Old Name", None);
        renaming["recordChangeTag"] = json!("ftag1");
        let (_z, m, sent) = write_server(
            &mut server,
            vec![old, renaming],
            json!({ "records": [{
                "recordName": "f1", "recordType": "Folder",
                "recordChangeTag": "ftag2", "fields": {},
            }]}),
        );
        let v = vertical_at(&server.url(), jar());

        // Absent parent → absent parent: the SAME placement under
        // `parent_for_path`, just a new leaf.
        v.rename_folder("f1", "Notes/New Name").await.expect("a same-parent rename must go through");
        m.assert();

        let record = &sent_json(&sent)["operations"][0]["record"];
        assert_eq!(record["recordChangeTag"], json!("ftag1"));
        let f = &record["fields"];
        assert!(f["ParentFolder"].is_null(), "a rename must not restate the parent");
        assert_eq!(wire::decode_text_field(&f["TitleEncrypted"]).as_deref(), Some("New Name"));
    }

    #[tokio::test]
    async fn a_rename_that_would_also_reparent_is_refused_not_guessed() {
        let mut server = mockito::Server::new_async().await;
        let zone = server
            .mock("POST", mockito::Matcher::Regex("changes/zone".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(zone_body(
                vec![
                    folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                    folder_rec("f-target", "Target", None),
                    folder_rec("f1", "Movable", None),
                ],
                "T1",
                false,
            ))
            .create();
        // No records/modify mock — a reparent must never reach the wire.
        let v = vertical_at(&server.url(), jar());

        // "Movable" currently has no parent (top-level); asking to rename it
        // to a path nested under "Target" asks for a DIFFERENT parent too —
        // that is a move, not a rename, and this call must refuse it rather
        // than write a title and silently leave the placement wrong.
        let _ = zone; // the mock's mere presence is what stops a connection error
        let err = v.rename_folder("f1", "Notes/Target/Movable").await.unwrap_err();
        match err {
            TransportError::Permanent { source } => {
                assert!(source.to_string().contains("different parent"));
            }
            other => panic!("expected Permanent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_folder_delete_sends_the_records_own_change_tag() {
        let mut server = mockito::Server::new_async().await;
        let mut target = folder_rec("f1", "Gone Soon", None);
        target["recordChangeTag"] = json!("ftag9");
        let (_z, m, sent) = write_server(
            &mut server,
            vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), target],
            json!({ "records": [{ "recordName": "f1", "recordChangeTag": "ftag10", "fields": {} }]}),
        );
        let v = vertical_at(&server.url(), jar());

        v.delete_folder("f1").await.expect("must delete");
        m.assert();

        let op = &sent_json(&sent)["operations"][0];
        assert_eq!(op["operationType"], json!("delete"));
        assert_eq!(op["record"]["recordChangeTag"], json!("ftag9"));
    }

    #[tokio::test]
    async fn deleting_a_folder_thats_already_gone_succeeds_without_a_write() {
        let mut server = mockito::Server::new_async().await;
        let zone = server
            .mock("POST", mockito::Matcher::Regex("changes/zone".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(zone_body(vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None)], "T1", false))
            .create();
        // No records/modify mock — deleting an absent folder must not write.
        let v = vertical_at(&server.url(), jar());

        v.delete_folder("already-gone").await.expect("absent target is a success, not a failure");
        zone.assert();
    }

    /// One write, folder included — the whole of
    /// `SaveSemantics::InPlaceUpdateIncludingMove`. A second, explicit move
    /// after this one would be a write against a `recordChangeTag` this write
    /// just bumped, which the caller does not hold: a guaranteed CONFLICT, or
    /// a whole extra zone walk to avoid one.
    #[tokio::test]
    async fn an_edit_that_also_changes_folder_relocates_in_the_same_write() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                folder_rec("f1", "Work", Some(wire::DEFAULT_FOLDER)),
                note_rec("n1", "T", "b", wire::DEFAULT_FOLDER),
            ],
            saved_reply("n1", "tag2"),
        );
        let v = vertical_at(&server.url(), jar());
        v.save_note_full(&edit(Some("n1"), "T", "<div>b2</div>", "Notes/Work"), &[])
            .await
            .unwrap();
        m.assert();

        let f = &sent_json(&sent)["operations"][0]["record"]["fields"];
        assert_eq!(f["Folder"]["value"]["recordName"], json!("f1"), "moved");
        assert!(!f["TextDataEncrypted"].is_null(), "and rewritten, in the same request");
    }

    /// **The 491-retry storm's actual cause, as a test.** Deleting a
    /// password-protected note builds a `Note`-shaped update for a
    /// `PasswordProtectedNote` record, which CloudKit answers 400 — every
    /// time, forever. Measured live 2026-08-27: two locked notes, 491
    /// identical requests over 3.5 hours.
    ///
    /// The refusal keys on the record TYPE (Component H3's amendment again),
    /// is `Permanent` so the worker records it and stops, and **nothing
    /// reaches the wire** — the modify mock is asserted never to be called.
    #[tokio::test]
    async fn deleting_a_locked_note_is_refused_permanently_and_never_reaches_the_wire() {
        let mut server = mockito::Server::new_async().await;
        let locked = json!({
            "recordName": "L1",
            "recordType": "PasswordProtectedNote",
            "recordChangeTag": "tag1",
            "fields": {
                "TitleEncrypted": { "value": b64("Secret") },
                "TextDataEncrypted": { "value": b64("not a document at all") },
                "Folder": { "value": { "recordName": wire::DEFAULT_FOLDER } },
                "ModificationDate": { "value": 1_700_000_000_000i64 },
            }
        });
        let (_z, m, _s) = write_server(
            &mut server,
            vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), locked],
            saved_reply("L1", "tag2"),
        );
        let m = m.expect(0);

        let v = vertical_at(&server.url(), jar());
        let err = v.delete("L1").await.unwrap_err();
        match err {
            TransportError::Permanent { source } => assert!(
                source.to_string().contains("password-protected"),
                "the refusal must name the lock so the user knows where to go: {source}"
            ),
            other => panic!("a doomed delete must be Permanent, or the worker retries it forever: {other:?}"),
        }
        m.assert();
    }

    /// Component H3's amendment, as a test. A locked note's cached body is
    /// `wire::LOCKED_BODY_HTML`; a guard that compared against that string
    /// would pass the moment the user edited the placeholder, so the guard
    /// keys on the remote RECORD TYPE — which no edit can change.
    #[tokio::test]
    async fn a_locked_note_is_refused_by_record_type_even_once_its_placeholder_is_edited() {
        let mut server = mockito::Server::new_async().await;
        let locked = json!({
            "recordName": "L1",
            "recordType": "PasswordProtectedNote",
            "recordChangeTag": "tag1",
            "fields": {
                "TitleEncrypted": { "value": b64("Secret") },
                "TextDataEncrypted": { "value": b64("not a document at all") },
                "Folder": { "value": { "recordName": wire::DEFAULT_FOLDER } },
                "ModificationDate": { "value": 1_700_000_000_000i64 },
            }
        });
        let (_z, m, _s) = write_server(
            &mut server,
            vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), locked],
            saved_reply("L1", "tag2"),
        );

        let v = vertical_at(&server.url(), jar());
        // The user has typed over the placeholder, so the body no longer
        // resembles LOCKED_BODY_HTML in any way.
        let err = v
            .save_note_full(&edit(Some("L1"), "Secret", "<div>my own words now</div>", "Notes"), &[])
            .await
            .unwrap_err();
        match err {
            TransportError::Permanent { source } => assert!(
                source.to_string().contains("password-protected"),
                "the refusal must name the lock: {source}"
            ),
            other => panic!("expected Permanent, got {other:?}"),
        }
        assert_eq!(m.matched(), false, "nothing may reach records/modify for a locked note");
    }

    /// The uuid Jodd already minted IS the `recordName` (gotcha #18's
    /// lowercase mint, arriving at the moment it was written for), so a create
    /// needs no rekey afterwards — gotcha #16's whole failure mode never opens
    /// on this backend.
    #[tokio::test]
    async fn a_create_uses_the_uuid_jodd_already_minted_as_the_record_name() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None)],
            saved_reply("f8bf619a-1b84-40eb-932d-6318ee9aeeb4", "tag1"),
        );

        let v = vertical_at(&server.url(), jar());
        let mut op = edit(None, "Fresh", "<div>body</div>", "Notes");
        op.existing_uuid = Some("f8bf619a-1b84-40eb-932d-6318ee9aeeb4");
        let saved = v.save_note_full(&op, &[]).await.unwrap();
        m.assert();

        let b = sent_json(&sent);
        assert_eq!(b["operations"][0]["operationType"], json!("create"));
        assert_eq!(
            b["operations"][0]["record"]["recordName"],
            json!("f8bf619a-1b84-40eb-932d-6318ee9aeeb4")
        );
        assert_eq!(saved.uuid, "f8bf619a-1b84-40eb-932d-6318ee9aeeb4", "no rekey needed");
        assert_eq!(sent_document(&sent).text(), "Fresh\nbody");
    }

    /// **The 2026-08-27 incident, pinned at the layer that produced it.**
    /// Three notes created through the real UI were accepted by CloudKit,
    /// shown in Apple Notes, and tombstoned by Apple within four minutes —
    /// purged, not trashed, with no delete ever issued by Jodd. The document
    /// `NoteDocument::new` built carried no `substring` and no `timestamp`:
    /// no per-character identity, no replica clock table. Apple's clients
    /// merge notes through that structure and drop a document that has none.
    ///
    /// This asserts on the bytes that actually leave the process, not on the
    /// constructor — a create that stops carrying identity somewhere between
    /// `compose` and the wire is the same outage with a different cause.
    #[tokio::test]
    async fn a_created_note_reaches_the_wire_carrying_crdt_identity() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None)],
            saved_reply("new-1", "tag1"),
        );

        let v = vertical_at(&server.url(), jar());
        v.save_note_full(&edit(None, "Fresh", "<div>body line</div>", "Notes"), &[]).await.unwrap();
        m.assert();

        let doc = sent_document(&sent);
        assert_eq!(doc.text(), "Fresh\nbody line");
        assert!(
            !doc.string.substring.is_empty(),
            "the created note reached the wire with NO substring runs — this is the shape \
             Apple tombstones: {:?}",
            doc.string
        );
        let table = doc.string.timestamp.as_ref().expect(
            "the created note reached the wire with NO replica clock table — Apple discards it",
        );
        assert_eq!(
            table.clock.first().map(|c| c.replica_uuid.clone()),
            Some(TEST_REPLICA_ID.to_vec()),
            "and the editing replica must sit FIRST in the table (the table-order rule)"
        );
        // The document the wire carries must also be one this engine can
        // edit next time — a create that lands unwritable is a note the user
        // can never touch again.
        assert!(compose::writability(&compose::encode(&doc), "Fresh").is_ok());
    }

    /// The design's whole claim, end to end through the transport: an edit
    /// leaves formatting this code cannot read exactly where it was, re-lengthed.
    #[tokio::test]
    async fn an_edit_preserves_runs_this_code_cannot_interpret() {
        let mut server = mockito::Server::new_async().await;
        // A body whose two runs carry a value nothing here understands.
        let document = {
            use super::gen::{topotext, versioned_document};
            use flate2::write::GzEncoder;
            use std::io::Write;
            let inner = topotext::String {
                string: "Title\nbody".into(),
                attribute_run: vec![
                    topotext::AttributeRun { length: 6, font_hints: Some(1), ..Default::default() },
                    topotext::AttributeRun { length: 4, font_hints: Some(2), ..Default::default() },
                ],
                ..Default::default()
            };
            let doc = versioned_document::Document {
                serialization_version: Some(1),
                version: vec![versioned_document::Version {
                    serialization_version: Some(1),
                    minimum_supported_version: Some(1),
                    data: Some(inner.encode_to_vec()),
                }],
            };
            let mut e = GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&doc.encode_to_vec()).unwrap();
            base64::engine::general_purpose::STANDARD.encode(e.finish().unwrap())
        };
        let rec = json!({
            "recordName": "n1",
            "recordType": "Note",
            "recordChangeTag": "tag1",
            "fields": {
                "TitleEncrypted": { "value": b64("Title") },
                "TextDataEncrypted": { "value": document },
                "Folder": { "value": { "recordName": wire::DEFAULT_FOLDER } },
                "CreationDate": { "value": 1_600_000_000_000i64 },
                "ModificationDate": { "value": 1_700_000_000_000i64 },
            }
        });
        let (_z, m, sent) = write_server(
            &mut server,
            vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), rec],
            saved_reply("n1", "tag2"),
        );

        let v = vertical_at(&server.url(), jar());
        v.save_note_full(&edit(Some("n1"), "Title", "<div>body extended</div>", "Notes"), &[])
            .await
            .unwrap();
        m.assert();

        let out = sent_document(&sent);
        assert_eq!(out.text(), "Title\nbody extended");
        assert_eq!(
            out.string
                .attribute_run
                .iter()
                .map(|r| (r.length, r.font_hints))
                .collect::<Vec<_>>(),
            vec![(6, Some(1)), (13, Some(2))],
            "the values survive untouched; only the lengths move"
        );
        // And the creation date is carried, not rewritten — Apple sorts on it.
        assert_eq!(
            sent_json(&sent)["operations"][0]["record"]["fields"]["CreationDate"]["value"],
            json!(1_600_000_000_000i64)
        );
    }

    /// The optimistic lock firing. It arrives inside an HTTP 200, so a
    /// transport that only classifies statuses reports it as a success — and
    /// the reconciler never makes the keep-both conflict copy that is the
    /// user's only record of the other device's edit.
    #[tokio::test]
    async fn a_stale_change_tag_reaches_the_caller_as_a_conflict() {
        // A GENUINE concurrent edit: the conflict loop looks the record up,
        // finds the remote content actually CHANGED (not just the tag), and
        // leaves it a `Conflict` for the poll's keep-both path rather than
        // auto-overwriting the other device's edit.
        let mut server = mockito::Server::new_async().await;
        let _zone = server
            .mock("POST", mockito::Matcher::Regex("changes/zone".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(zone_body(
                vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), note_rec("n1", "T", "b", wire::DEFAULT_FOLDER)],
                "T1",
                false,
            ))
            .create();
        let _modify = server
            .mock("POST", mockito::Matcher::Regex("records/modify".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "records": [{ "recordName": "n1", "serverErrorCode": "CONFLICT",
                                             "reason": "change tag mismatch" }] }).to_string())
            .create();
        // The lookup shows a moved tag AND different body text — a real edit
        // by another device.
        let mut looked_up = note_rec("n1", "T", "b CHANGED ON ANOTHER DEVICE", wire::DEFAULT_FOLDER);
        looked_up["recordChangeTag"] = json!("tag2");
        let _lookup = server
            .mock("POST", mockito::Matcher::Regex("records/lookup".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "records": [looked_up] }).to_string())
            .create();
        let v = vertical_at(&server.url(), jar());
        match v.save_note_full(&edit(Some("n1"), "T", "<div>b2</div>", "Notes"), &[]).await {
            Err(TransportError::Conflict { .. }) => {}
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    /// **The fix for the stale create changeTag (measured live 2026-09-09).**
    /// Apple bumps a freshly-created note's `recordChangeTag` server-side just
    /// after the create reply hands one back, so the user's very next edit
    /// pushes against a tag that is already stale and gets `CONFLICT` — a
    /// self-conflict carrying no concurrent edit at all. The old code re-sent
    /// the identical stale-tag write every worker tick until a whole-zone walk
    /// happened to refresh the base (up to ~10 min), and for that whole window
    /// Apple showed the pre-edit content (a deleted character still in the
    /// title).
    ///
    /// The conflict loop must instead point-read the record's current tag and
    /// retry against it, because the content is unchanged (only the tag moved).
    #[tokio::test]
    async fn a_stale_create_change_tag_is_refreshed_and_the_write_retried() {
        let mut server = mockito::Server::new_async().await;
        let _zone = server
            .mock("POST", mockito::Matcher::Regex("changes/zone".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(zone_body(
                vec![
                    folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                    // A note Jodd created, carrying real CRDT identity, at tag1.
                    note_rec_crdt("n1", "Xnote test", "body", wire::DEFAULT_FOLDER),
                ],
                "T1",
                false,
            ))
            .create();
        // The first write carries the stale tag1 → CONFLICT.
        let conflict = server
            .mock("POST", mockito::Matcher::Regex("records/modify".into()))
            .match_body(mockito::Matcher::Regex(r#""recordChangeTag":"tag1""#.into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "records": [{ "recordName": "n1", "serverErrorCode": "CONFLICT",
                                             "reason": "change tag mismatch" }] }).to_string())
            .expect(1)
            .create();
        // The point read reveals the tag Apple bumped it to — same content.
        let mut looked_up = note_rec_crdt("n1", "Xnote test", "body", wire::DEFAULT_FOLDER);
        looked_up["recordChangeTag"] = json!("tag2");
        let lookup = server
            .mock("POST", mockito::Matcher::Regex("records/lookup".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "records": [looked_up] }).to_string())
            .expect(1)
            .create();
        // The retry carries the FRESH tag2 → accepted.
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = sent.clone();
        let success = server
            .mock("POST", mockito::Matcher::Regex("records/modify".into()))
            .match_body(mockito::Matcher::Regex(r#""recordChangeTag":"tag2""#.into()))
            .match_request(move |req| {
                if let Ok(b) = req.body() {
                    *capture.lock().unwrap() = b.clone();
                }
                true
            })
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(saved_reply("n1", "tag3").to_string())
            .expect(1)
            .create();

        let v = vertical_at(&server.url(), jar());
        let saved = v
            .save_note_full(&edit(Some("n1"), "note test", "<div>body</div>", "Notes"), &[])
            .await
            .expect("a stale-tag self-conflict must recover, not surface as an error");

        conflict.assert();
        lookup.assert();
        success.assert();
        assert_eq!(saved.version, "tag3", "the note ends on the tag the successful retry returned");

        // The retry sent the corrected title (the whole point) under the fresh
        // optimistic lock.
        let body = sent_json(&sent);
        assert_eq!(body["operations"][0]["record"]["recordChangeTag"], json!("tag2"));
        assert_eq!(sent_document(&sent).text(), "note test\nbody");
    }

    /// `writes.folders` is false, so a label with no folder behind it is a
    /// refusal — never an implicit `create_folder`, which would put a folder
    /// in the user's iCloud account that Jodd is not allowed to make.
    #[tokio::test]
    async fn a_note_filed_under_an_unknown_folder_is_refused_rather_than_creating_one() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, _s) = write_server(
            &mut server,
            vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None)],
            saved_reply("n1", "tag2"),
        );
        let v = vertical_at(&server.url(), jar());
        let err = v
            .save_note_full(&edit(None, "T", "<div>b</div>", "Notes/Nowhere"), &[])
            .await
            .unwrap_err();
        match err {
            TransportError::Permanent { source } => {
                assert!(source.to_string().contains("Notes/Nowhere"), "{source}");
            }
            other => panic!("expected Permanent, got {other:?}"),
        }
        assert!(!m.matched(), "nothing may be written when the destination is unknown");
    }

    /// A move writes the note's own record, so it must report the note's new
    /// version — `push_one_dirty` stamps it, and a `None` here would leave
    /// `remote_version` holding the pre-move tag and make the next poll read
    /// Jodd's own write as someone else's edit.
    #[tokio::test]
    async fn a_move_writes_only_the_folder_and_reports_the_notes_new_version() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                folder_rec("f1", "Work", Some(wire::DEFAULT_FOLDER)),
                note_rec("n1", "T", "b", wire::DEFAULT_FOLDER),
            ],
            saved_reply("n1", "tag9"),
        );
        let v = vertical_at(&server.url(), jar());
        let moved = v
            .move_note("n1", std::slice::from_ref(&"Notes/Work".to_string()), &[])
            .await
            .unwrap()
            .expect("a move on this backend touches the note's own record");
        m.assert();
        assert_eq!(moved.version, "tag9");

        let f = &sent_json(&sent)["operations"][0]["record"]["fields"];
        assert_eq!(f["Folder"]["value"]["recordName"], json!("f1"));
        assert!(f["TextDataEncrypted"].is_null(), "a move must not rewrite the body");
    }

    #[tokio::test]
    async fn a_delete_files_the_note_in_apples_own_trash() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                note_rec("n1", "T", "b", wire::DEFAULT_FOLDER),
            ],
            saved_reply("n1", "tag9"),
        );
        let v = vertical_at(&server.url(), jar());
        v.delete("n1").await.unwrap();
        m.assert();
        let f = &sent_json(&sent)["operations"][0]["record"]["fields"];
        assert_eq!(f["Folder"]["value"]["recordName"], json!(wire::TRASH_FOLDER));
        assert_eq!(
            sent_json(&sent)["operations"][0]["record"]["recordChangeTag"],
            json!("tag1"),
            "a delete is a write like any other and races an edit on the phone"
        );
    }

    /// Every read on this backend filters one cached zone walk. Leaving that
    /// walk untouched after a write means the 2500 ms folder sweep renders the
    /// pre-write state and the user watches their own edit revert; DROPPING it
    /// means the worker draining five dirty notes performs five whole-zone
    /// reads back to back, because `save_note_full` takes a scan before it
    /// writes. Folding is the third answer, and the write result already paid
    /// for it.
    #[tokio::test]
    async fn a_successful_write_is_folded_into_the_cached_read_rather_than_dropping_it() {
        let mut server = mockito::Server::new_async().await;
        let (zone, _m, _s) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                note_rec("n1", "T", "b", wire::DEFAULT_FOLDER),
            ],
            saved_reply("n1", "tag2"),
        );
        let zone = zone.expect(1);

        let v = vertical_at(&server.url(), jar());
        v.scan().await.unwrap();
        v.save_note_full(&edit(Some("n1"), "T2", "<div>b2</div>", "Notes"), &[]).await.unwrap();
        let after = v.scan().await.unwrap();
        zone.assert();

        let n = after.notes.iter().find(|n| n.uuid == "n1").expect("still cached");
        assert_eq!(n.title, "T2");
        assert_eq!(n.body_html, "<div>b2</div>");
        assert_eq!(n.version, "tag2", "the NEW change tag, or the next write CONFLICTs");
        assert_eq!(
            after.bases.get("n1").map(|b| b.change_tag.as_str()),
            Some("tag2"),
            "and the write base the next edit reads its lock from"
        );
    }

    /// A second edit in the same burst must send the tag the FIRST edit
    /// returned. This is what the fold buys that a dropped cache would have
    /// bought with a whole extra zone read, and what a stale cache gets wrong:
    /// the server refuses the second write as a conflict, and the user's own
    /// two keystrokes manufacture a conflict copy.
    #[tokio::test]
    async fn a_second_edit_in_the_same_burst_sends_the_tag_the_first_one_returned() {
        let mut server = mockito::Server::new_async().await;
        let (zone, _m, sent) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                note_rec("n1", "T", "b", wire::DEFAULT_FOLDER),
            ],
            saved_reply("n1", "tag2"),
        );
        let zone = zone.expect(1);

        let v = vertical_at(&server.url(), jar());
        v.save_note_full(&edit(Some("n1"), "T", "<div>b2</div>", "Notes"), &[]).await.unwrap();
        v.save_note_full(&edit(Some("n1"), "T", "<div>b3</div>", "Notes"), &[]).await.unwrap();
        zone.assert();
        assert_eq!(
            sent_json(&sent)["operations"][0]["record"]["recordChangeTag"],
            json!("tag2")
        );
    }

    /// A deleted note is in the Trash, which every listing excludes — so the
    /// cached scan must stop carrying it, or the folder sweep renders a note
    /// the user just deleted.
    #[tokio::test]
    async fn a_delete_is_folded_out_of_the_cached_read() {
        let mut server = mockito::Server::new_async().await;
        let (zone, _m, _s) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                note_rec("n1", "T", "b", wire::DEFAULT_FOLDER),
                note_rec("n2", "U", "c", wire::DEFAULT_FOLDER),
            ],
            saved_reply("n1", "tag9"),
        );
        let zone = zone.expect(1);

        let v = vertical_at(&server.url(), jar());
        v.delete("n1").await.unwrap();
        let after = v.scan().await.unwrap();
        zone.assert();
        assert!(after.notes.iter().all(|n| n.uuid != "n1"), "the deleted note is gone");
        assert!(after.notes.iter().any(|n| n.uuid == "n2"), "and nothing else moved");
    }

    /// **The difference between "deleted on another device" and "not read
    /// yet".** `push_one_dirty` answers a `NotFound` on an update by dropping
    /// the local row — correct when the zone was read to the end and the note
    /// genuinely is not in it, silent data loss when the walk stopped early.
    /// A partial walk must therefore report `Transient`, which retries.
    #[tokio::test]
    async fn a_note_missing_from_an_unfinished_walk_is_retried_not_declared_deleted() {
        let mut server = mockito::Server::new_async().await;
        // `moreComing` with no `syncToken` to resume from: the walk stops and
        // says so, and what it holds is not the whole zone.
        let _z = server
            .mock("POST", mockito::Matcher::Regex("changes/zone".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({ "zones": [{
                    "records": [folder_rec(wire::DEFAULT_FOLDER, "Notes", None)],
                    "moreComing": true
                }] })
                .to_string(),
            )
            .create();

        let v = vertical_at(&server.url(), jar());
        assert!(!v.scan().await.unwrap().complete, "the fixture must be a partial walk");
        match v.save_note_full(&edit(Some("n1"), "T", "<div>b</div>", "Notes"), &[]).await {
            Err(TransportError::Transient { .. }) => {}
            other => panic!(
                "a partial walk must not let an absent note look deleted, got {other:?}"
            ),
        }
    }

    /// A note whose `Folder` names a record the walk never saw is filed under
    /// the root for DISPLAY (`decode_note`, counted as `orphaned`). Re-resolving
    /// that label on every save would write `DefaultFolder-CloudKit` onto it and
    /// genuinely move it — a silent reorganisation of the user's account,
    /// performed by an edit that changed a word.
    #[tokio::test]
    async fn editing_an_orphaned_note_does_not_relocate_it_into_the_root() {
        let mut server = mockito::Server::new_async().await;
        let (_z, m, sent) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                // Its folder record is not in the zone.
                note_rec("n1", "T", "b", "a-folder-this-walk-never-saw"),
            ],
            saved_reply("n1", "tag2"),
        );
        let v = vertical_at(&server.url(), jar());
        v.save_note_full(&edit(Some("n1"), "T", "<div>b2</div>", "Notes"), &[]).await.unwrap();
        m.assert();
        assert_eq!(
            sent_json(&sent)["operations"][0]["record"]["fields"]["Folder"]["value"]["recordName"],
            json!("a-folder-this-walk-never-saw"),
            "the note keeps the folder it is actually in"
        );
    }

    /// A refused write must NOT drop the cache: the account's state is
    /// unchanged, and re-walking the zone on every failed push is how a
    /// permanently-refused note (gotcha #14) turns into a whole-zone read
    /// every five seconds.
    #[tokio::test]
    async fn a_refused_write_leaves_the_cached_read_alone() {
        let mut server = mockito::Server::new_async().await;
        let (zone, _m, _s) = write_server(
            &mut server,
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                note_rec("n1", "T", "b", wire::DEFAULT_FOLDER),
            ],
            json!({ "records": [{ "recordName": "n1", "serverErrorCode": "CONFLICT" }] }),
        );
        let zone = zone.expect(1);

        let v = vertical_at(&server.url(), jar());
        v.scan().await.unwrap();
        let _ = v.save_note_full(&edit(Some("n1"), "T", "<div>b2</div>", "Notes"), &[]).await;
        v.scan().await.unwrap();
        zone.assert();
    }

    // ── the zone walk ───────────────────────────────────────────────────

    #[tokio::test]
    async fn a_zone_walk_decodes_notes_folders_and_stamps_the_account() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(zone_body(
                vec![
                    folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                    folder_rec("f1", "Work", Some(wire::DEFAULT_FOLDER)),
                    note_rec("n1", "Meeting", "first point", "f1"),
                ],
                "T1",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let scan = v.scan().await.expect("the walk must complete");

        assert_eq!(scan.notes.len(), 1);
        assert_eq!(scan.notes[0].label, "Notes/Work");
        assert_eq!(scan.notes[0].body_html, "<div>first point</div>", "the title line is cut");
        assert_eq!(
            scan.notes[0].account_id.as_deref(),
            Some("icloud:kaiwan@me.com"),
            "the wire layer is account-blind; the vertical stamps ownership"
        );
        assert_eq!(scan.sync_token.as_deref(), Some("T1"));

        let paths: Vec<&str> = scan.folders.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["Notes", "Notes/Work"], "sorted, so the sidebar is stable");
    }

    #[tokio::test]
    async fn the_walk_pages_on_more_coming() {
        let mut server = mockito::Server::new_async().await;
        let _p1 = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), note_rec("n1", "A", "A", wire::DEFAULT_FOLDER)],
                "T1",
                true,
            ))
            .expect(1)
            .create_async()
            .await;
        let _p2 = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(vec![note_rec("n2", "B", "B", wire::DEFAULT_FOLDER)], "T2", false))
            .expect(1)
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let scan = v.scan().await.expect("two pages");
        assert_eq!(scan.notes.len(), 2, "both pages' notes must survive the walk");
        assert_eq!(scan.sync_token.as_deref(), Some("T2"), "the LAST page's token is the resume point");
    }

    #[tokio::test]
    async fn the_scan_runs_once_per_instance_however_many_reads_ask_for_it() {
        // list_notes_in_folder is on a 2500 ms UI sweep and CloudKit has no
        // per-folder endpoint, so a second walk per call would be a second
        // whole-zone read every time.
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), note_rec("n1", "A", "A", wire::DEFAULT_FOLDER)],
                "T1",
                false,
            ))
            .expect(1)
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        v.list_all_notes(&HashMap::new()).await.unwrap();
        v.list_notes_in_folder("Notes", &HashMap::new()).await.unwrap();
        v.list_index().await.unwrap();
        v.list_folders().await.unwrap();
        v.fetch_note("n1").await.unwrap();
        m.assert_async().await;
    }

    #[tokio::test]
    async fn a_rejected_token_restarts_the_walk_instead_of_failing_it() {
        // The rejection arrives inside an HTTP 200, and a merely old token
        // still syncs — so this is rare, and treating it as an error would
        // fail rarely and confusingly.
        let mut server = mockito::Server::new_async().await;
        let _bad = server
            .mock("POST", mockito::Matcher::Any)
            .match_body(mockito::Matcher::Regex("STALE".into()))
            .with_status(200)
            .with_body(json!({ "zones": [{ "serverErrorCode": "BAD_REQUEST", "reason": "bad token" }] }).to_string())
            .create_async()
            .await;
        let _good = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), note_rec("n1", "A", "A", wire::DEFAULT_FOLDER)],
                "FRESH",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let cs = v
            .changes_since(Some(&crate::backend::SyncCursor(b"STALE".to_vec())))
            .await
            .expect("a rejected token is a refetch, not an error");
        assert_eq!(cs.changes.len(), 1);
        assert_eq!(String::from_utf8(cs.next_cursor.0).unwrap(), "FRESH");
    }

    #[tokio::test]
    async fn a_from_scratch_read_that_is_also_refused_is_a_real_failure() {
        // Without this, a server refusing everything would loop forever
        // clearing and re-sending a token it never accepts.
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(json!({ "zones": [{ "serverErrorCode": "BAD_REQUEST", "reason": "no" }] }).to_string())
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        match v.scan().await {
            Err(TransportError::Permanent { source }) => {
                assert!(source.to_string().contains("from-scratch"), "got {source}")
            }
            other => panic!("expected Permanent, got {:?}", other.err()),
        }
    }

    // ── the decode split Component H consumes ───────────────────────────

    #[tokio::test]
    async fn the_tally_keeps_unreadable_malformed_and_incomplete_apart() {
        // Component H keys "this account is ADP-encrypted" on `unreadable`
        // alone. A malformed document is a bug or a schema change and a
        // structurally broken record is neither — counting either one would
        // accuse a readable account of being end-to-end encrypted (gotcha #20).
        let mut unreadable = note_rec("n-unreadable", "t", "t", wire::DEFAULT_FOLDER);
        unreadable["fields"]["TextDataEncrypted"] = json!({ "value": b64("not a compressed stream") });

        let mut malformed = note_rec("n-malformed", "t", "t", wire::DEFAULT_FOLDER);
        malformed["fields"]["TextDataEncrypted"] = json!({ "value": {
            // gzip of bytes that decompress fine and are not a note document.
            "value": ""
        }});
        // Build it properly: gzip some non-protobuf bytes.
        {
            use flate2::write::GzEncoder;
            use std::io::Write;
            let mut e = GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).unwrap();
            malformed["fields"]["TextDataEncrypted"] = json!({
                "value": base64::engine::general_purpose::STANDARD.encode(e.finish().unwrap())
            });
        }

        let mut incomplete = note_rec("n-incomplete", "t", "t", wire::DEFAULT_FOLDER);
        incomplete["fields"]["TextDataEncrypted"] = json!({ "value": "!!! not base64 !!!" });

        let mut deleted = note_rec("n-deleted", "t", "t", wire::DEFAULT_FOLDER);
        deleted["fields"]["Deleted"] = json!({ "value": 1 });

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![
                    folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                    note_rec("n-ok", "A", "A", wire::DEFAULT_FOLDER),
                    unreadable,
                    malformed,
                    incomplete,
                    deleted,
                ],
                "T1",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let t = v.scan().await.unwrap().tally;
        assert_eq!(
            t,
            DecodeTally { decoded: 1, unreadable: 1, malformed: 1, incomplete: 1, deleted: 1, trashed: 0, locked: 0, orphaned: 0, unfiled: 0 },
            "each cause must stay in its own bucket"
        );
    }

    #[tokio::test]
    async fn a_locked_note_is_counted_rather_than_silently_dropped() {
        // Found on a live account: Apple showed a folder with two notes, Jodd
        // showed one, and nothing anywhere said why. A locked note is a
        // PasswordProtectedNote record — a different record type — so the
        // `Note` filter skipped it without a trace.
        let locked = json!({
            "recordName": "locked-1",
            "recordType": "PasswordProtectedNote",
            "recordChangeTag": "lk1",
            "fields": {
                "TitleEncrypted": { "value": b64("the locked one") },
                "Folder": { "value": { "recordName": wire::DEFAULT_FOLDER } },
                "CreationDate": { "value": 1_600_000_000_000i64 },
                "ModificationDate": { "value": 1_700_000_000_000i64 },
            }
        });
        let mut locked_but_deleted = locked.clone();
        locked_but_deleted["recordName"] = json!("locked-2");
        locked_but_deleted["fields"]["Deleted"] = json!({ "value": 1 });

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![
                    folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                    note_rec("n1", "A", "A", wire::DEFAULT_FOLDER),
                    locked,
                    locked_but_deleted,
                ],
                "T1",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let scan = v.scan().await.unwrap();
        assert_eq!(scan.tally.locked, 1, "the live one only — a deleted locked note is not live");
        assert_eq!(scan.notes.len(), 2, "the locked note appears alongside the ordinary one");

        let n = scan.notes.iter().find(|n| n.id == "locked-1").expect("shown, not skipped");
        assert_eq!(n.title, "the locked one", "its title IS readable — measured on a live account");
        assert_eq!(n.label, wire::ROOT_PATH, "and it lands in its real folder");
        assert!(
            n.body_html.contains("Apple Notes"),
            "the body must SAY why it is blank and where to read it: {}",
            n.body_html
        );
        assert!(
            !n.body_html.is_empty(),
            "an empty body is gotcha #17's landmine; a visible sentence is not that shape"
        );
        assert!(n.version == "lk1", "a real change tag, so a re-lock/rename is seen as a change");

        assert_eq!(
            AdpVerdict::of(&scan.tally),
            AdpVerdict::Readable,
            "a locked note is not evidence of ADP — the user locked it on purpose"
        );
    }

    #[test]
    fn a_locked_note_in_the_trash_is_not_shown() {
        // has_trash is false, so a locked note the user deleted must not come
        // back as a live one — the same rule ordinary notes follow.
        let paths = HashMap::new();
        let trashed = json!({
            "recordName": "locked-trashed",
            "recordType": "PasswordProtectedNote",
            "fields": { "Folder": { "value": { "recordName": wire::TRASH_FOLDER } } }
        });
        assert!(wire::decode_locked_note(&trashed, &paths).is_none());
    }

    #[tokio::test]
    async fn a_note_whose_folder_is_unknown_is_counted_as_orphaned() {
        // Filing an orphan under the root is right — the note stays visible —
        // but it is indistinguishable in the result from a note that genuinely
        // lives there. Counting is what makes "the root has more notes than
        // Apple shows" answerable.
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![
                    folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                    note_rec("n1", "A", "A", wire::DEFAULT_FOLDER),
                    note_rec("n2", "B", "B", "a-folder-this-walk-never-saw"),
                ],
                "T1",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let scan = v.scan().await.unwrap();
        assert_eq!(scan.tally.orphaned, 1, "only the one with an unknown folder");
        assert_eq!(scan.tally.decoded, 2, "the orphan is still a real, readable note");
        assert!(scan.notes.iter().all(|n| n.label == wire::ROOT_PATH));
    }

    #[tokio::test]
    async fn a_note_carried_on_two_pages_is_one_note_with_the_later_state() {
        // `changes/zone` is a change feed, not a listing: a record modified
        // while the walk is in flight comes back again on a later page. The
        // cache never showed it because (uuid, account_id) is the primary key
        // and SQLite collapsed the repeats; the walk's own count did not, and
        // that was the entire 778-vs-772 disagreement between the sidebar and
        // the cache — with 772 also being what Apple says.
        let mut server = mockito::Server::new_async().await;
        let page1 = zone_body(
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                note_rec("n1", "A", "first state", wire::DEFAULT_FOLDER),
                note_rec("n2", "B", "B", wire::DEFAULT_FOLDER),
            ],
            "T1",
            true,
        );
        let page2 = zone_body(
            vec![note_rec("n1", "A revised", "second state", wire::DEFAULT_FOLDER)],
            "T2",
            false,
        );
        let _m1 = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(page1)
            .expect(1)
            .create_async()
            .await;
        let _m2 = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(page2)
            .expect(1)
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let scan = v.scan().await.unwrap();
        assert_eq!(scan.notes.len(), 2, "two distinct notes, not three");
        let n1 = scan.notes.iter().find(|n| n.uuid == "n1").unwrap();
        assert_eq!(
            n1.title, "A revised",
            "later wins — a second copy in a change feed is a newer state"
        );
    }

    #[tokio::test]
    async fn a_record_that_arrives_filed_and_trashed_is_one_note_on_the_newer_side() {
        // **Off by one in both directions at once, which is how this was
        // found.** Apple showed one note in a folder and one in Recently
        // Deleted; Jodd showed zero and two. The dedupe ran over `notes` and
        // `trashed` as separate vectors, so a record arriving once filed and
        // once in the Trash landed in both and neither pass could see the
        // other.
        //
        // And the tiebreak had to change with it: the older copy came LAST in
        // the feed here, so "later wins" picks the Trash and loses a note the
        // user can see. `ModificationDate` is Apple's own statement about the
        // record; page order is a guess about CloudKit's paging.
        let mut server = mockito::Server::new_async().await;
        let mut trashed_copy = note_rec("n1", "A", "body", wire::TRASH_FOLDER);
        trashed_copy["fields"]["ModificationDate"] = json!({ "value": 1_000i64 });
        let mut restored = note_rec("n1", "A", "body", "f-sub");
        restored["fields"]["ModificationDate"] = json!({ "value": 2_000i64 });

        let page1 = zone_body(
            vec![
                folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                folder_rec("f-sub", "Sub", None),
                // The NEWER state first, the older one later, so feed order
                // and the timestamps disagree.
                restored,
                trashed_copy,
            ],
            "T1",
            false,
        );
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(page1)
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let scan = v.scan().await.unwrap();
        assert_eq!(scan.notes.len(), 1, "one note, not one here and one in the bin");
        assert_eq!(scan.trashed.len(), 0, "the older Trash copy must not survive too");
        assert_eq!(scan.notes[0].label, "Notes/Sub");
    }

    #[tokio::test]
    async fn a_note_with_no_folder_reference_at_all_is_counted_separately() {
        // The blind spot `orphaned` could never see. `decode_note` resolves
        // placement with `unwrap_or(DEFAULT_FOLDER)`, so a missing field and
        // an explicit "in the root" are the same result — and the orphan
        // check only ever fired on a reference that was PRESENT. A note with
        // no field was therefore counted as neither, and a live account
        // showing more notes in the root than Apple does, with `orphaned = 0`,
        // has exactly this shape.
        let mut rootless = note_rec("n3", "C", "C", wire::DEFAULT_FOLDER);
        rootless["fields"].as_object_mut().unwrap().remove("Folder");

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![
                    folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                    note_rec("n1", "A", "A", wire::DEFAULT_FOLDER),
                    note_rec("n2", "B", "B", "a-folder-this-walk-never-saw"),
                    rootless,
                ],
                "T1",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let scan = v.scan().await.unwrap();
        assert_eq!(scan.tally.unfiled, 1, "the one carrying no Folder field");
        assert_eq!(
            scan.tally.orphaned, 1,
            "and it must not be double-counted as an orphan — different cause, different fix"
        );
        assert_eq!(scan.tally.decoded, 3, "all three are real, readable notes");
        // Still visible. Counting the cause never changes where it lands.
        assert!(scan.notes.iter().all(|n| n.label == wire::ROOT_PATH));
    }

    #[test]
    fn the_field_census_counts_names_and_never_values() {
        use serde_json::json;
        let mut into = HashMap::new();
        let mut total = 0usize;
        census_fields(
            &json!({ "fields": { "TitleEncrypted": { "value": "c2VjcmV0" }, "Folder": {} } }),
            &mut into,
            &mut total,
        );
        census_fields(&json!({ "fields": { "Folder": {} } }), &mut into, &mut total);
        // A record with no fields object still counts toward the total, or the
        // "carried by all of them" comparison silently shifts.
        census_fields(&json!({}), &mut into, &mut total);

        assert_eq!(total, 3);
        assert_eq!(into.get("Folder"), Some(&2));
        assert_eq!(into.get("TitleEncrypted"), Some(&1), "the field the report is looking for");
        // The point of the whole helper: nothing it stores can be a value.
        assert!(into.keys().all(|k| k != "c2VjcmV0"));
    }

    #[test]
    fn the_folders_field_is_described_without_ever_printing_content() {
        use serde_json::json;
        assert_eq!(describe_folders_field(&json!({})), "absent");
        assert_eq!(describe_folders_field(&json!({ "value": [] })), "empty list");
        assert_eq!(
            describe_folders_field(&json!({ "value": [{ "recordName": "f1" }] })),
            "[f1]"
        );
        assert_eq!(
            describe_folders_field(&json!({ "value": { "recordName": "f2" } })),
            "ref f2"
        );
        // A shape nothing anticipated names its JSON kind and stops. The
        // point of the whole helper is that an unexpected `Folders` cannot
        // leak note text into the log by being unexpected.
        assert_eq!(
            describe_folders_field(&json!({ "value": "some string" })),
            "present, unrecognised shape (string)"
        );
    }

    // ── scoping and lookup ──────────────────────────────────────────────

    #[tokio::test]
    async fn a_folder_read_is_exact_not_a_subtree_and_an_unknown_folder_is_empty() {
        // gotcha #1: a note carries one label, and the sidebar count for
        // Notes/Work must not include Notes/Work/Deep.
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![
                    folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                    folder_rec("f1", "Work", Some(wire::DEFAULT_FOLDER)),
                    folder_rec("f2", "Deep", Some("f1")),
                    note_rec("n1", "A", "A", "f1"),
                    note_rec("n2", "B", "B", "f2"),
                ],
                "T1",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let shallow = v.list_notes_in_folder("Notes/Work", &HashMap::new()).await.unwrap();
        assert_eq!(shallow.len(), 1, "a descendant's note must not be counted here");
        assert_eq!(shallow[0].id, "n1");

        assert!(
            v.list_notes_in_folder("Notes/Nope", &HashMap::new()).await.unwrap().is_empty(),
            "an unknown folder is empty, never NotFound"
        );
        assert!(matches!(v.fetch_note("nope").await, Err(TransportError::NotFound)));
    }

    #[tokio::test]
    async fn a_deleted_record_is_a_deletion_in_the_change_set_not_an_upsert() {
        // Tombstones and live notes arrive in the same array. Reading
        // everything as Upserted would resurrect deleted notes on every sync.
        let mut deleted = note_rec("n-gone", "t", "t", wire::DEFAULT_FOLDER);
        deleted["fields"]["Deleted"] = json!({ "value": 1 });

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![
                    folder_rec(wire::DEFAULT_FOLDER, "Notes", None),
                    note_rec("n-live", "A", "A", wire::DEFAULT_FOLDER),
                    deleted,
                ],
                "T9",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let cs = v.changes_since(None).await.unwrap();
        let live = cs.changes.iter().find(|c| c.remote_id == "n-live").unwrap();
        let gone = cs.changes.iter().find(|c| c.remote_id == "n-gone").unwrap();
        assert_eq!(live.kind, ChangeKind::Upserted);
        assert_eq!(live.folder_hint.as_deref(), Some("Notes"));
        assert_eq!(gone.kind, ChangeKind::Deleted);
        assert!(!cs.more);
    }

    #[tokio::test]
    async fn an_undecodable_record_is_an_upsert_not_a_deletion() {
        // It still exists in iCloud and it changed. Calling it Deleted would
        // remove a note that is sitting there readable in Apple Notes.
        let mut bad = note_rec("n-bad", "t", "t", wire::DEFAULT_FOLDER);
        bad["fields"]["TextDataEncrypted"] = json!({ "value": b64("not compressed") });

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), bad], "T1", false))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let cs = v.changes_since(None).await.unwrap();
        assert_eq!(cs.changes[0].kind, ChangeKind::Upserted);
    }

    // ── Component H — the ADP verdict ───────────────────────────────────

    #[test]
    fn one_decoded_note_is_enough_to_call_the_account_readable() {
        // 774 of 776 real notes decoded, and the two that did not were a
        // different compression container, not encryption. Any decode at all
        // rules ADP out.
        let t = DecodeTally { decoded: 1, unreadable: 99, ..Default::default() };
        assert_eq!(AdpVerdict::of(&t), AdpVerdict::Readable);
        assert!(!AdpVerdict::of(&t).blocks_account());
    }

    #[test]
    fn an_empty_account_is_not_accused_of_being_encrypted() {
        // THE trap. An account with zero notes decodes nothing either, and
        // "your notes are end-to-end encrypted" is a false accusation with no
        // action available to the person receiving it.
        let v = AdpVerdict::of(&DecodeTally::default());
        assert_eq!(v, AdpVerdict::NoNotes);
        assert!(!v.blocks_account());
        assert!(v.blocked_reason().is_none());
    }

    #[test]
    fn an_account_whose_only_notes_are_deleted_has_no_notes_not_no_readable_notes() {
        // Tombstones are not unreadable content — they are absence.
        let t = DecodeTally { deleted: 12, ..Default::default() };
        assert_eq!(AdpVerdict::of(&t), AdpVerdict::NoNotes);
    }

    #[test]
    fn records_that_are_not_a_compressed_stream_at_all_are_the_adp_shape() {
        let t = DecodeTally { unreadable: 7, ..Default::default() };
        let v = AdpVerdict::of(&t);
        assert_eq!(v, AdpVerdict::Unreadable { records: 7 });
        assert!(v.blocks_account(), "this is the only verdict that blocks");
        let msg = v.blocked_reason().expect("a blocked account must say why");
        assert!(msg.contains("Advanced Data Protection"), "name it: {msg}");
        assert!(msg.contains('7'), "say how many: {msg}");
    }

    #[test]
    fn a_malformed_document_never_counts_toward_an_adp_verdict() {
        // The design spec's own table said "n note records, 0 decoded →
        // Unreadable", which counts these — contradicting H1's prose two
        // paragraphs later. Following the table would block an account over a
        // bug in Jodd's decoder or a schema change Apple shipped.
        let t = DecodeTally { malformed: 3, incomplete: 1, ..Default::default() };
        let v = AdpVerdict::of(&t);
        assert_eq!(v, AdpVerdict::Inconclusive { malformed: 3, incomplete: 1 });
        assert!(!v.blocks_account(), "a decoder bug must not read as encryption");
        assert!(v.blocked_reason().is_none());
    }

    #[test]
    fn unreadable_wins_over_malformed_when_both_are_present() {
        // A single note that is genuinely not a compressed stream is evidence
        // of ADP; a malformed one alongside it is not evidence against.
        let t = DecodeTally { unreadable: 1, malformed: 5, ..Default::default() };
        assert_eq!(AdpVerdict::of(&t), AdpVerdict::Unreadable { records: 1 });
    }

    #[tokio::test]
    async fn blocked_reason_reports_nothing_before_a_read_and_never_fetches() {
        // `Vertical::blocked_reason` is synchronous and is called from paths
        // that must not touch the network. An instance that has not walked the
        // zone answers None — "nothing known against this account" — rather
        // than triggering a round trip from a getter. Pointed at a URL that
        // would fail if it were ever called.
        let v = vertical_at("http://127.0.0.1:1", jar());
        assert!(
            crate::backend::Vertical::blocked_reason(&v).is_none(),
            "a getter must not fetch, and must not guess"
        );
    }

    #[tokio::test]
    async fn blocked_reason_reports_the_verdict_once_the_read_has_happened() {
        let mut encrypted = note_rec("n1", "t", "t", wire::DEFAULT_FOLDER);
        encrypted["fields"]["TextDataEncrypted"] = json!({ "value": b64("not a compressed stream") });

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), encrypted], "T1", false))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        assert!(crate::backend::Vertical::blocked_reason(&v).is_none(), "no read yet");
        v.scan().await.unwrap();
        let reason = crate::backend::Vertical::blocked_reason(&v).expect("the read found ADP");
        assert!(reason.contains("Advanced Data Protection"), "got {reason}");
    }

    #[tokio::test]
    async fn a_readable_account_reports_no_block_after_a_read() {
        // The field must CLEAR itself, not only get set: an account that was
        // blocked and is now readable has to lose the banner on the next pass.
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), note_rec("n1", "A", "A", wire::DEFAULT_FOLDER)],
                "T1",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        v.scan().await.unwrap();
        assert!(crate::backend::Vertical::blocked_reason(&v).is_none());
    }

    #[tokio::test]
    async fn the_verdict_reads_the_same_walk_the_notes_came_from() {
        // Not a second read of the account: the gate and the listing must
        // agree, and a separate walk could see a different zone.
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), note_rec("n1", "A", "A", wire::DEFAULT_FOLDER)],
                "T1",
                false,
            ))
            .expect(1)
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        assert_eq!(v.adp_verdict().await.unwrap(), AdpVerdict::Readable);
        v.list_all_notes(&HashMap::new()).await.unwrap();
        m.assert_async().await;
    }

    #[tokio::test]
    async fn an_account_whose_every_note_is_encrypted_reads_as_blocked() {
        let mut encrypted = note_rec("n1", "t", "t", wire::DEFAULT_FOLDER);
        encrypted["fields"]["TextDataEncrypted"] =
            json!({ "value": b64("\u{0}\u{1}not a compressed stream at all") });
        let mut second = note_rec("n2", "t", "t", wire::DEFAULT_FOLDER);
        second["fields"]["TextDataEncrypted"] = json!({ "value": b64("also not compressed") });

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), encrypted, second],
                "T1",
                false,
            ))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let verdict = v.adp_verdict().await.unwrap();
        assert_eq!(verdict, AdpVerdict::Unreadable { records: 2 });
        assert!(verdict.blocks_account());
    }

    #[tokio::test]
    async fn an_unreadable_record_never_becomes_an_empty_bodied_note() {
        // H3, and it is gotcha #17's exact shape: a note cached with an empty
        // body is a landmine M2 detonates the moment the user edits it and the
        // worker pushes that emptiness over content that was fine on the
        // server. Skip it, count it, report the count.
        let mut bad = note_rec("n-bad", "t", "t", wire::DEFAULT_FOLDER);
        bad["fields"]["TextDataEncrypted"] = json!({ "value": b64("not compressed") });

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), bad], "T1", false))
            .create_async()
            .await;

        let v = vertical_at(&server.url(), jar());
        let scan = v.scan().await.unwrap();
        assert!(scan.notes.is_empty(), "an unreadable record must not reach the cache at all");
        assert_eq!(scan.tally.unreadable, 1, "but it must be counted");
    }

    // ── the shared zone read ────────────────────────────────────────────

    #[tokio::test]
    async fn two_verticals_over_one_account_share_a_single_zone_walk() {
        // THE reason AccountCache exists. A vertical is built per operation and
        // CloudKit has no per-folder endpoint, so without sharing the 2500 ms
        // folder sweep walks the whole account once per folder — 102 whole-zone
        // reads in four minutes on a real account.
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(
                vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), note_rec("n1", "A", "A", wire::DEFAULT_FOLDER)],
                "T1",
                false,
            ))
            .expect(1)
            .create_async()
            .await;

        let (a, b) = pair_at(&server.url());
        a.list_all_notes(&HashMap::new()).await.unwrap();
        let second = b.list_notes_in_folder("Notes", &HashMap::new()).await.unwrap();

        m.assert_async().await;
        assert_eq!(second.len(), 1, "the second vertical must SEE the shared walk, not just skip it");
    }

    #[tokio::test]
    async fn invalidating_the_cache_makes_the_next_read_walk_again() {
        // The ⟳ button's entire job. A cache no user action can clear would
        // answer "go and look again" out of memory.
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None)], "T1", false))
            .expect(2)
            .create_async()
            .await;

        let shared = std::sync::Arc::new(AccountCache::default());
        let v = ICloudVertical::new(
            IcloudSession {
                apple_id: "kaiwan@me.com".into(),
                dsid: "12345".into(),
                ck_host: server.url(),
                client: ClientConfig {
                    client_build_number: "b".into(),
                    client_mastering_number: "m".into(),
                    client_id: "c".into(),
                },
            },
            jar(),
            "icloud:kaiwan@me.com".into(),
            shared.clone(),
            TEST_REPLICA_ID,
        );

        v.scan().await.unwrap();
        v.scan().await.unwrap(); // still cached — one request so far
        shared.invalidate().await;
        v.scan().await.unwrap(); // now two
        m.assert_async().await;
    }

    #[tokio::test]
    async fn a_shared_scan_still_answers_blocked_reason_on_a_vertical_that_did_not_walk() {
        // `seen_tally` is stamped whether the scan was walked or reused, or a
        // second vertical would report an account healthy purely because
        // someone else did the reading.
        let mut encrypted = note_rec("n1", "t", "t", wire::DEFAULT_FOLDER);
        encrypted["fields"]["TextDataEncrypted"] = json!({ "value": b64("not a compressed stream") });

        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None), encrypted], "T1", false))
            .create_async()
            .await;

        let (a, b) = pair_at(&server.url());
        a.scan().await.unwrap();
        b.scan().await.unwrap(); // served from the shared cache
        assert!(
            crate::backend::Vertical::blocked_reason(&b).is_some(),
            "a vertical that reused someone else's walk still knows what it found"
        );
    }

    #[tokio::test]
    async fn the_session_bootstrap_is_cached_too_not_just_the_zone_read() {
        // Same problem one layer up: a vertical is built per operation and each
        // one must establish a session first, so the sweep would POST /validate
        // every 2500 ms — about a hundred calls in one pass.
        //
        // No HTTP server here on purpose. `establish` posts to the real setup
        // host, so a cache MISS would try to reach Apple and fail; a hit
        // returns without touching the network. That makes "did it hit the
        // cache?" observable as "did it succeed at all", which is a stronger
        // assertion than counting requests to a mock.
        let cache = AccountCache::default();
        let cookies = jar();
        let seeded = IcloudSession {
            apple_id: "kaiwan@me.com".into(),
            dsid: "12345".into(),
            ck_host: "https://p149-ckdatabasews.icloud.com".into(),
            client: ClientConfig {
                client_build_number: "b".into(),
                client_mastering_number: "m".into(),
                client_id: "c".into(),
            },
        };
        *cache.session.lock().await = Some((std::time::Instant::now(), seeded.clone()));

        let a = cache.session(cookies.as_ref()).await.expect("served from cache");
        let b = cache.session(cookies.as_ref()).await.expect("still served from cache");
        assert_eq!(a, seeded);
        assert_eq!(a, b);

        cache.invalidate().await;
        assert!(
            cache.session.lock().await.is_none(),
            "invalidate must clear the session as well as the scan — a refresh that \
             re-walked against a stale partition host fails like a dead session"
        );
    }

    // ── the session seam ────────────────────────────────────────────────

    #[tokio::test]
    async fn a_dead_webview_is_an_auth_failure_so_the_revival_path_can_run() {
        // Not Transient: the worker would retry a webview that is not coming
        // back. Auth is what a 421 means too, and both are B4's to answer.
        let v = vertical_at("https://p149-ckdatabasews.icloud.com", Arc::new(DeadJar));
        assert!(matches!(v.scan().await, Err(TransportError::Auth)));
    }

    #[tokio::test]
    async fn a_jar_with_nothing_scoped_to_cloudkit_is_auth_not_an_empty_request() {
        // Sending an empty Cookie header would earn a 421 whose cause reads as
        // "the session expired" when it is really "the harvest found nothing".
        let wrong_host = Arc::new(StaticJar(vec![HarvestedCookie {
            name: "X-APPLE-WEBAUTH-TOKEN".into(),
            value: "session".into(),
            domain: "example.com".into(),
            path: "/".into(),
            host_only: false,
            secure: true,
        }]));
        let v = vertical_at("https://p149-ckdatabasews.icloud.com", wrong_host);
        assert!(matches!(v.scan().await, Err(TransportError::Auth)));
    }

    #[tokio::test]
    async fn cookies_are_harvested_per_request_never_cached_across_the_walk() {
        // gotcha #19: the live webview rotates its own cookies, and a captured
        // jar measured dead inside 3.5 hours. A vertical that harvested once
        // and reused it would be holding exactly that stale copy.
        struct Counting(std::sync::atomic::AtomicUsize);
        #[async_trait]
        impl CookieSource for Counting {
            async fn harvest(&self) -> Result<Vec<HarvestedCookie>, String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(vec![HarvestedCookie {
                    name: "X-APPLE-WEBAUTH-TOKEN".into(),
                    value: "session".into(),
                    domain: "127.0.0.1".into(),
                    path: "/".into(),
                    host_only: false,
                    secure: true,
                }])
            }
        }

        let mut server = mockito::Server::new_async().await;
        let _p1 = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(vec![folder_rec(wire::DEFAULT_FOLDER, "Notes", None)], "T1", true))
            .expect(1)
            .create_async()
            .await;
        let _p2 = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_body(zone_body(vec![], "T2", false))
            .expect(1)
            .create_async()
            .await;

        let counter = Arc::new(Counting(std::sync::atomic::AtomicUsize::new(0)));
        let v = vertical_at(&server.url(), counter.clone());
        v.scan().await.unwrap();
        assert_eq!(
            counter.0.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one harvest per page, not one per walk"
        );
    }

    #[tokio::test]
    async fn a_dead_session_surfaces_as_auth_so_the_revival_path_can_run() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("POST", mockito::Matcher::Any).with_status(421).create_async().await;
        let v = vertical_at(&server.url(), jar());

        // Wrapped in a timeout because of how this test failed once: the 421
        // path invalidated the WHOLE cache from inside the walk, and `scan()`
        // holds the scan lock across the walk — a tokio mutex is not
        // reentrant, so the read never returned. A hanging test reports
        // nothing useful and takes the whole suite down with it; a timeout
        // turns the same defect into one named assertion.
        let got = tokio::time::timeout(std::time::Duration::from_secs(10), v.scan())
            .await
            .expect("scan() must RETURN on a dead session — a hang here means a lock was taken twice");
        assert!(matches!(got, Err(TransportError::Auth)));
    }

    // ── the formatting save path (M3 Task 10) ───────────────────────────

    mod formatting_save {
        use super::super::*;
        use super::super::gen::topotext;

        const REPLICA: [u8; 16] = [0xAB; 16];

        fn doc_of(text: &str, runs: Vec<topotext::AttributeRun>) -> compose::NoteDocument {
            let d = compose::NoteDocument {
                string: topotext::String {
                    string: text.to_string(),
                    substring: Vec::new(),
                    timestamp: None,
                    attribute_run: runs,
                },
                serialization_version: Some(0),
                version_serialization_version: Some(0),
                minimum_supported_version: Some(0),
                crdt: None,
            };
            compose::parse(&compose::encode(&d)).unwrap()
        }
        fn run_len(len: u32) -> topotext::AttributeRun {
            topotext::AttributeRun { length: len, ..Default::default() }
        }
        fn styled_run(len: u32, style: u32) -> topotext::AttributeRun {
            topotext::AttributeRun {
                length: len,
                paragraph_style: Some(topotext::ParagraphStyle {
                    style: Some(style),
                    ..Default::default()
                }),
                ..Default::default()
            }
        }

        /// The update branch's exact steps, minus the wire: parse the editor
        /// HTML once, recompose the text, splice, reconcile.
        fn run_update(base: &compose::NoteDocument, title_field: &str, new_title: &str, body_html: &str) -> compose::NoteDocument {
            let parsed_body = format_html::parse_editor_html(body_html);
            let old_title = doc::note_title(base.text(), title_field);
            let text = compose::recompose(base.text(), &old_title, new_title, &parsed_body.text);
            let mut edited = match &base.crdt {
                Some(_) => base.with_text_crdt(&text, REPLICA).unwrap(),
                None => base.with_text(&text),
            };
            ICloudVertical::reconcile_formatting(&mut edited, &parsed_body, REPLICA);
            edited
        }

        #[test]
        fn a_bolded_word_in_the_editor_reaches_the_document_runs() {
            let base = doc_of("Title\nbold rest", vec![run_len(15)]);
            let edited = run_update(&base, "Title", "Title", "<div><b>bold</b> rest</div>");
            assert_eq!(edited.text(), "Title\nbold rest");
            assert!(
                edited.string.attribute_run.iter().any(|r| r.font_hints == Some(1)),
                "no bold run landed: {:?}",
                edited.string.attribute_run
            );
            assert!(compose::runs_cover(edited.text(), &edited.string.attribute_run));
        }

        #[test]
        fn a_reconcile_refusal_downgrades_to_text_only_instead_of_blocking() {
            // Unknown style 77: the current format is undecodable, so the
            // formatting half refuses — the TEXT edit must still land, runs
            // preserved opaquely by the splice.
            let base = doc_of("Title\nold body", vec![styled_run(14, 77)]);
            let edited = run_update(&base, "Title", "Title", "<div>new text</div>");
            assert_eq!(edited.text(), "Title\nnew text");
            assert_eq!(
                edited.string.attribute_run[0].paragraph_style.as_ref().unwrap().style,
                Some(77),
                "the opaque splice must keep the style it cannot read"
            );
        }

        #[test]
        fn a_note_created_with_formatting_gets_real_runs_from_day_one() {
            let parsed_body = format_html::parse_editor_html("<div><b>hi</b></div>");
            let text = compose::compose_new("T", &parsed_body.text);
            let mut created = compose::NoteDocument::new(&text);
            ICloudVertical::reconcile_formatting(&mut created, &parsed_body, REPLICA);
            assert_eq!(created.text(), "T\nhi");
            assert!(
                created.string.attribute_run.iter().any(|r| r.font_hints == Some(1)),
                "no bold run landed: {:?}",
                created.string.attribute_run
            );
            // The title paragraph stays plain: only the body was formatted.
            assert_ne!(created.string.attribute_run[0].font_hints, Some(1));
            assert!(compose::runs_cover(created.text(), &created.string.attribute_run));
        }

        /// The stale-cache guard: an ENTIRELY plain editor body over a
        /// formatted note preserves the formatting (a pre-M3 cached
        /// rendering echoing back must not strip the server's runs — gotcha
        /// #17's landmine class), while a PARTIALLY formatted body
        /// reconciles fully, removals included.
        #[test]
        fn a_fully_plain_editor_body_over_a_formatted_note_preserves_the_formatting() {
            // Body run carries italic (font_hints 2); the editor sends plain.
            let base = doc_of(
                "Title\nbody",
                vec![run_len(6), topotext::AttributeRun { length: 4, font_hints: Some(2), ..Default::default() }],
            );
            let edited = run_update(&base, "Title", "Title", "<div>body extended</div>");
            assert_eq!(edited.text(), "Title\nbody extended");
            assert!(
                edited.string.attribute_run.iter().any(|r| r.font_hints == Some(2)),
                "the splice must keep the italic run: {:?}",
                edited.string.attribute_run
            );
            // But a PARTIALLY formatted body applies its removals: bold moves
            // from the first word to the second.
            let base = doc_of(
                "Title\nbold rest",
                vec![
                    run_len(6),
                    topotext::AttributeRun { length: 4, font_hints: Some(1), ..Default::default() },
                    run_len(5),
                ],
            );
            let edited = run_update(&base, "Title", "Title", "<div>bold <b>rest</b></div>");
            let hints: Vec<Option<u32>> = edited.string.attribute_run.iter().map(|r| r.font_hints).collect();
            assert!(
                !hints.contains(&Some(1)) || {
                    // the bold bit must now cover "rest", not "bold": find the
                    // run offsets
                    let mut off = 0usize;
                    let mut bold_covers_rest = false;
                    for r in &edited.string.attribute_run {
                        if r.font_hints == Some(1) && off >= 11 {
                            bold_covers_rest = true;
                        }
                        off += r.length as usize;
                    }
                    bold_covers_rest
                },
                "bold must have moved to the second word: {:?}",
                edited.string.attribute_run
            );
        }

        #[test]
        fn the_title_paragraph_is_never_rewritten_by_a_body_only_format_change() {
            // The title line carries style 0 (Title); the editor bolds only
            // the body. The title's run must pass through byte-identical.
            let base = doc_of("Title\nbody", vec![styled_run(6, 0), run_len(4)]);
            let edited = run_update(&base, "Title", "Title", "<div><b>body</b></div>");
            assert_eq!(edited.string.attribute_run[0], styled_run(6, 0));
            assert!(edited.string.attribute_run.iter().skip(1).any(|r| r.font_hints == Some(1)));
        }

        #[test]
        fn a_crdt_carrying_note_survives_the_full_edit_plus_format_pass() {
            // The compose fixture shape with real CRDT identity.
            let d = compose::NoteDocument {
                string: topotext::String {
                    string: "Title\nbody".into(),
                    substring: vec![
                        topotext::Substring {
                            char_id: topotext::CharId { replica_id: 0, clock: 0 },
                            length: 0,
                            timestamp: topotext::CharId { replica_id: 0, clock: 0 },
                            tombstone: None,
                            child: vec![1],
                        },
                        topotext::Substring {
                            char_id: topotext::CharId { replica_id: 1, clock: 0 },
                            length: 10,
                            timestamp: topotext::CharId { replica_id: 1, clock: 0 },
                            tombstone: None,
                            child: vec![2],
                        },
                        topotext::Substring {
                            char_id: topotext::CharId { replica_id: 0, clock: 0xFFFF_FFFF },
                            length: 0,
                            timestamp: topotext::CharId { replica_id: 0, clock: 0xFFFF_FFFF },
                            tombstone: None,
                            child: Vec::new(),
                        },
                    ],
                    timestamp: Some(topotext::VectorTimestamp {
                        clock: vec![topotext::vector_timestamp::Clock {
                            replica_uuid: vec![0xAA; 16],
                            replica_clock: vec![
                                topotext::vector_timestamp::clock::ReplicaClock { clock: 10, subclock: None },
                                topotext::vector_timestamp::clock::ReplicaClock { clock: 1, subclock: None },
                            ],
                        }],
                    }),
                    attribute_run: vec![run_len(10)],
                },
                serialization_version: Some(0),
                version_serialization_version: Some(0),
                minimum_supported_version: Some(0),
                crdt: None,
            };
            let base = compose::writability(&compose::encode(&d), "Title").unwrap();
            assert!(base.crdt.is_some());
            let edited = run_update(&base, "Title", "Title", "<div><b>body</b>!</div>");
            assert_eq!(edited.text(), "Title\nbody!");
            assert!(edited.string.attribute_run.iter().any(|r| r.font_hints == Some(1)));
            crdt::validate_document_invariants(edited.crdt.as_ref().unwrap()).unwrap();
            // And the result is itself writable — an edit must not make a
            // note read-only for the next one.
            assert!(compose::writability(&compose::encode(&edited), "Title").is_ok());
        }
    }
}
