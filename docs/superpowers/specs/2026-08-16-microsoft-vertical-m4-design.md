# Microsoft/Outlook — Vertical #2, Milestone 4 (pin sidecar)

> Status: **design / approved** (2026-08-16). Turns on `MetadataSidecar` for
> the Microsoft vertical — pin only. M1 (read) and M2 (note writes) are
> merged. M3 closed folder writes as a permanent negative
> (`docs/superpowers/specs/2026-08-14-microsoft-vertical-m2-design.md`'s
> "Deferred to M3" section).
>
> Builds on:
> - `CLAUDE.md` → "How Apple Notes ↔ Outlook.com works", gotchas #4, #9, #11, #12
> - [2026-08-14-microsoft-vertical-m2-design.md](2026-08-14-microsoft-vertical-m2-design.md)'s
>   "M4 sidecars" section — the original sketch this design revises
>
> **Acceptance bar:** on a live `@live.com` account, pinning and unpinning a
> note in Jodd is reflected correctly after the next `sync_pin_state` pull —
> including from a note pinned by a second Jodd instance signed into the same
> account. `Capabilities::for_backend(Microsoft).writes.sidecars` is `true`.
> `cargo test --workspace` — not a narrower gate.

## What was measured before this design existed

Both live, `kaiwan.h@live.com`, `scripts/ms_named_property_probe.py`,
2026-08-15/16. **Nothing below is inferred from documentation.**

| Behaviour | Result |
| --- | --- |
| `POST /me/mailFolders/{notes_root}/messages` with a MAPI **named** property (`String {GUID} Name JoddMeta`) in `singleValueExtendedProperties`, alongside `PR_MESSAGE_CLASS` | **201**, and the value round-trips exactly on a scoped `$expand` read |
| `PATCH` the note's `subject`/`body` (an ordinary content edit) | **200**, and the named property **survives unchanged** — a content edit does not need to resend it |
| `PATCH` **only** the named property (no `subject`/`body` in the payload) | **200**, and the new value round-trips — content is untouched |
| The resulting note, read in Notes.app on Mac and iPhone and on `outlook.live.com/mail/notes`, after ActiveSync caught up | **Renders completely normally** — correct title, correct body, no corruption, no error. This is the M4 hypothesis ("Apple only reads fields it knows about") and it had never been measured before this session. |

Four consequences:

1. **The M2 spec's original plan — one JSON property holding both pin and
   tags — is no longer the right shape**, not because it doesn't work
   mechanically, but because the premise underneath it changed: see
   "Decisions locked in brainstorming" below.
2. **A named property is addressed differently from the numbered MAPI tags
   already in use** (`PROP_MESSAGE_CLASS`, `PROP_CONTAINER_CLASS`): the id
   syntax is `String {GUID} Name PropertyName`, not `String 0x...`.
   `same_property_id`'s existing numeric-tag normalization does not apply to
   named ids and must not be used to compare them — a plain string compare is
   correct here (Graph did not lowercase or reformat the named id in testing,
   unlike the numbered ones).
3. **No content-resend requirement.** Because the property survives a
   content `PATCH`, `save_note_full` does not need to touch it, and the pin
   push path does not need to touch note content. The two are fully
   independent operations against the same message.
4. **A GUID was minted for this session's probe and is proposed as the
   permanent constant**: `98d27db2-72ad-4511-a3b5-b73c7c42694b`. It was
   generated fresh (uuid4), is not a Microsoft-defined property set, and has
   no collision risk in practice. If a different GUID is preferred before
   implementation, regenerate it — nothing depends on this exact value except
   readability of the probe's own test note (already cleaned up).

## Decisions locked in brainstorming (2026-08-16)

- **Pin only. Tags are out of scope, deliberately, not by oversight.**
  Reading `lib.rs` end to end (not just the trait) found that
  `sync_tag_state` (the read/reconciliation half of the tags sidecar) is
  **already disabled** — tags moved to being derived from inline `#hashtag`
  text in the note body (`reconcile_tags_from_body_conn`), which already
  round-trips with Apple Notes identically on every backend, Microsoft
  included, since M1. The write half (`push_one_tag_set`) still runs and
  still calls `put_sidecar(..., Tags, ...)`, but nothing reads that value
  back on **any** backend anymore — it is already vestigial for Gmail, not
  something this milestone introduces for Microsoft. Implementing a real
  Tags mechanism on Microsoft would be solving a problem for a consumer that
  does not exist.
- **One property, not two.** Rejected: splitting pin and tags into two
  named properties to keep each write independent. Moot once tags are
  scoped out — there is only one kind of data to store, so there is nothing
  to split.
- **No `MetadataSidecar` trait changes.** The trait's `remove_sidecar(id)`
  has no `kind` parameter, which would have been a real gap if pin and tags
  shared one property (an id alone can't say which to clear). Scoping tags
  out removes the need: `remove_sidecar` on Microsoft only ever receives a
  pin-originated id, because `put_sidecar(..., Tags, ...)` returns an empty
  string, and every call site already filters on
  `.filter(|s| !s.is_empty())` before calling `remove_sidecar` — an idiom
  already established in `lib.rs`, not new dispatch logic invented for this
  milestone.
- **The "sidecar id" is the note's own uuid, not a separate object's id.**
  Gmail's sidecar is a real second message with its own id; Microsoft has no
  second object at all — the property lives on the note itself. `put_sidecar`
  returns the input `note_uuid` back as the "id", which the caller stores
  opaquely in `meta_msg_id` and later passes to `remove_sidecar` — round-trips
  correctly without the caller needing to know Microsoft has no separate
  object.
- **`internetMessageId` lookup, not a full-mailbox scan, for individual
  pushes.** Because a pushed Microsoft note's `uuid` **is** its Exchange
  `internetMessageId` (rekeyed on first push — see the M2 spec's Component C),
  `put_sidecar`/`remove_sidecar` resolve the target message with one
  targeted query, `GET /me/messages?$filter=internetMessageId eq '<uuid>'`,
  not the vertical's full cached scan. Only `list_sidecars(Pin)` — the
  reconciliation pass — needs a full paginated scan, and it already has
  passable justification for the cost: it runs once at cold start (`App.svelte`
  triggers `sync_pin_state` there today, unchanged by this milestone), not on
  every 5 s sync tick.

## Component A — capability flip

```rust
BackendKind::Microsoft => Capabilities {
    ...
    writes: Writes { notes: true, folders: false, sidecars: true },
},
```

No `Writes` struct changes. `set_pin`/`set_pin_batch` become live;
`add_tag`/`remove_tag`/`rename_tag`/`delete_tag` also become live (they share
the same `Write::Sidecars` gate), but their effect on Microsoft is now the
sanctioned no-op described in Component D, not a `milestone_2()`/`Permanent`
error — so a tag add/remove on a Microsoft note succeeds locally (the
`note_tags` table and body `#hashtag` are the source of truth either way) and
performs no network call for the tag-sidecar half.

## Component B — the property

```rust
pub const JODD_PIN_PROP: &str =
    "String {98d27db2-72ad-4511-a3b5-b73c7c42694b} Name JoddPin";
```

Value: `{"pinned": true}` / `{"pinned": false}` — a JSON object rather than a
bare boolean so the shape can grow without a migration if a future milestone
needs to add a field here (unlikely, given tags are scoped out, but cheap to
allow). **Do not** run this id through `same_property_id`/`property_tag` —
those exist specifically to normalize Graph's response-side reformatting of
**numbered** tags (`String 0x001A` → `String 0x1a`) and do not apply to named
ids; use a plain string comparison, confirmed correct by the probe (Graph
returned the named id back unchanged, byte-for-byte, in every read).

## Component C — `put_sidecar` / `remove_sidecar` (Pin)

```
put_sidecar(uuid, Pin, Some(body), _replace):
    id = resolve(uuid)                      // $filter=internetMessageId eq '<uuid>'
    PATCH /me/messages/{id}
        { singleValueExtendedProperties: [{ id: JODD_PIN_PROP, value: body }] }
    -> Ok(uuid.to_string())                 // self-referential; no second object

remove_sidecar(id):                         // id is always a note uuid here
    target = resolve(id)
    PATCH /me/messages/{target}
        { singleValueExtendedProperties: [{ id: JODD_PIN_PROP, value: r#"{"pinned":false}"# }] }
    -> Ok(())
```

`remove_sidecar` must actually write `pinned: false`, not merely no-op —
`list_sidecars(Pin)`'s reconciliation reads whatever value is stored, so
leaving a stale `pinned: true` behind would make an unpinned note re-pin
itself on the next cold start.

**Not yet live-verified: the `$filter=internetMessageId eq '<uuid>'` lookup
itself.** Today's probe read back the property using the message id Graph
returned at creation time — it never exercised resolving a uuid to a message
id via `$filter`. This is a plain, documented Graph filter (not the
Notes-tree enumeration gotcha #12 rules out), and low-risk, but it is an
assumption, not a measurement, until Task 1 of the implementation plan runs
it live. Two things worth checking specifically: whether Graph's stored
`internetMessageId` still carries the RFC 5322 angle brackets Jodd's own
uuid has had stripped (`raw_to_graph_message` strips exactly one layer — the
`$filter` value may need the brackets put back), and whether `$filter` on
this property needs any escaping beyond the standard OData string quoting
already used elsewhere in `wire.rs`.

## Component D — `put_sidecar` / `remove_sidecar` (Tags, no-op)

```rust
put_sidecar(_uuid, Tags, _, _) -> Ok(String::new())
```

No Graph call. `remove_sidecar` is never invoked for a Tags-originated call
on Microsoft: `push_one_tag_set` and `push_one_deletion` both gate on
`.filter(|s| !s.is_empty())` before calling `remove_sidecar`, and an empty
string never passes that filter — this is the existing "no sidecar" idiom in
`lib.rs`, not new logic. Tag round-tripping is unaffected either way: it goes
through the body-derived path (`AppleHtmlDeriver`, `reconcile_tags_from_body_conn`),
identical to every other backend and already proven since M1.

## Component E — `list_sidecars(Pin)` (reconciliation)

Full paginated scan (id + `internetMessageId` + `JODD_PIN_PROP` only, no
message bodies — a separate, lighter request shape than the vertical's
existing note-listing scan, per gotcha #12's "one property per `$expand`"
rule). For each message where the property decodes to `pinned: true`, emit
`SidecarRecord { id: uuid, note_uuid: uuid, kind: Pin, body: None }`. Returns
`Ok(Some(vec))` — never `Ok(None)` (that value means "the store doesn't exist
yet," which has no equivalent here: the property either exists on a note or
it doesn't, there is no separate store to be absent). An account with zero
pinned notes returns `Ok(Some(vec![]))`, which `sync_pin_state` already
handles correctly (it prunes local pins to exactly this set).

**Cost, accepted:** one full mailbox pagination per `sync_pin_state` call.
Today that is cold-start only (`App.svelte`), so this is not a per-tick cost.
If a future milestone changes when `sync_pin_state` fires, revisit this.

## What M4 does **not** implement

- **Tags sidecar on Microsoft** — scoped out; see "Decisions locked in
  brainstorming". `SidecarKind::Tags` reaches Microsoft's `MetadataSidecar`
  impl and returns a harmless success with no network effect.
- **Deleting the extended property outright** (as opposed to writing
  `pinned: false`) — untested, and `pinned: false` is simpler and already
  proven via the property-only `PATCH` in today's probe. Not worth the extra
  risk for a behavior-equivalent outcome.
- **Removing the now-fully-vestigial tags-sidecar write path from `lib.rs`**
  (`push_one_tag_set`'s `put_sidecar(..., Tags, ...)` call, live for Gmail
  too) — real cleanup, but out of scope for a Microsoft-specific milestone.
  Worth a follow-up task on its own.

## Verification

Unit-testable with no live account, matching this vertical's existing test
shape (`wire.rs`'s `write_path_tests`, `transport.rs`'s `transport_tests`):

| target | why |
| --- | --- |
| `JODD_PIN_PROP` id round-trip through encode/decode | the property id and JSON body shape are the whole mechanism |
| `put_sidecar(Pin)` dispatches the right `$filter` query then the right `PATCH`, via a local HTTP server recording the request (same pattern as `save_note_full_dispatches_create_and_patch_to_the_right_endpoint_and_method`) | proves the two-request sequence, not just that each half compiles |
| `put_sidecar(Tags)` sends **zero** requests | the no-op guarantee is exactly the kind of thing a passing test can silently stop guaranteeing if refactored carelessly |
| `remove_sidecar` writes `pinned: false`, not a delete or a no-op | the reconciliation-correctness argument in Component C, made executable |
| `list_sidecars(Pin)` — a fixture with mixed pinned/unpinned/malformed-property notes — only pinned ones come back | mirrors `list_folders_keeps_an_emptied_cached_folder_and_drops_a_deleted_one`'s shape: assert the filter, not just the decode |
| `Db::mark_pin_pushed` / `mark_tags_pushed` accept an empty-string id the same as `None` | Component D's design depends on this equivalence already holding; confirm rather than assume |

Needs a live account (manual, recorded in the milestone notes):

- [ ] Resolve a note's uuid to a message id via
  `$filter=internetMessageId eq '<uuid>'` and confirm it returns exactly one
  match — the one open mechanism question from Component C.
- [ ] Pin a note in Jodd, confirm the property lands via a direct Graph read
  (not just that the local `pinned` flag flipped).
- [ ] Unpin, confirm the property reads back `pinned: false` and
  `sync_pin_state` does not re-apply the pin.
- [ ] Two-Jodd-instance scenario: pin a note from a second signed-in
  instance (or simulate via a direct Graph write), confirm the first
  instance's `sync_pin_state` picks it up.
- [ ] Confirm a pinned note still renders correctly in Notes.app (expected
  to hold, given today's probe, but the M2 postmortem's lesson — an
  unverified expectation is a liability, not a fact — argues for checking
  once rather than assuming the earlier probe's note is representative of
  every note shape).

Gate — the commands CI runs, never anything narrower:

```bash
cargo test --workspace
cargo check --workspace --all-targets
npx vitest run
npx svelte-check --threshold error
npm run build
```

## Implementation order (phases)

1. **Live-verify the `$filter=internetMessageId` lookup** (the one unproven
   mechanism) before writing the rest, the same discipline M2 and M3 used —
   a design detail that turns out to be wrong is cheap to fix before code is
   built on top of it, expensive after.
2. **`JODD_PIN_PROP` + encode/decode helpers**, tests first.
3. **`put_sidecar`/`remove_sidecar` for Pin**, tests first (dispatch-level,
   per the Verification table).
4. **`put_sidecar` for Tags (no-op)**, tests first (the zero-requests
   guarantee).
5. **`list_sidecars(Pin)`**, tests first (the pinned-only filter).
6. **Flip `Writes { sidecars: true }`.**
7. **Live pass** on `kaiwan.h@live.com`, per the Verification checklist
   above, including the two-instance scenario.

Every phase before 6 is safe to ship half-done, because the capability still
refuses. Phase 6 is the only irreversible step in the sense that mattered for
M2 and M3 — flip it only after the mechanism it depends on is proven, not
assumed.
