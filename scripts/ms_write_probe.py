#!/usr/bin/env python3
"""M2 write-path probe: what does Graph actually do when Jodd writes?

Milestone 1 proved the read path against a live Outlook.com mailbox. Six
behaviours the WRITE path depends on have never been run, and at least three
of them change M2's architecture rather than its implementation. This script
runs all six and reports what came back.

WHY A PROBE AND NOT A TEST. This branch has already paid for the alternative.
A request for `String 0x001A` comes back tagged `String 0x1a` — lower-cased,
leading zeros stripped — and every lookup compared against the sent form, so a
13-note mailbox read as zero notes. All 82 module tests passed throughout,
because every fixture was hand-written with the sent form. A fixture written by
whoever wrote the matcher can only prove the matcher agrees with itself. So
this talks to the real API, and `same_property_id` below mirrors the fix.

A CAUTION THIS PROJECT LEARNED THE HARD WAY (CLAUDE.md, 2026-08-14): Graph
answers a request it dislikes with **200 and an empty result** rather than an
error — an `$expand=...($filter=id eq 'A' or id eq 'B')` does exactly that. So
a 200 is not by itself success. Every probe below checks what came back.

WHY THE TOKEN COMES FROM THE ENVIRONMENT: so a live credential never has to be
pasted anywhere it might persist. Run the token helper, then hand the token to
this script through the environment in your own shell:

    python3 scripts/ms_get_token.py --print-token
    MS_TOKEN='<paste the token>' python3 scripts/ms_write_probe.py

The token is read from $MS_TOKEN and never printed back.

═══════════════════════════════════════════════════════════════════════════
THIS SCRIPT WRITES TO A LIVE APPLE NOTES ACCOUNT.
═══════════════════════════════════════════════════════════════════════════

Everything it does reaches Notes.app and the iPhone within ~60s. On this
backend a delete is a HARD delete: an Apple-side delete leaves nothing in
Deleted Items and nothing any Graph scan can see (measured 2026-08-14). There
is no undo path to fall back on.

So the script holds ONE invariant, and every destructive call is routed
through `destroy()` which enforces it:

    It only ever modifies or deletes items it created itself, in this run.

Pre-existing notes and folders are read to locate the sandbox and are never
written to. Everything created is recorded in MANIFEST and printed at the end,
so an aborted run can be cleaned up by hand.

SETUP REQUIRED, in Apple Notes, before running:

    Create two folders, JODD-PROBE-A and JODD-PROBE-B, and put one throwaway
    note in each. The note is not optional: a folder holding zero notes cannot
    be discovered through Graph at all (gotcha #12), so an empty folder is
    invisible to this script. Two folders rather than one so probe 4 has a
    move destination even if probe 1 shows folder-create is impossible.

    Delete both folders in Notes.app when the probing is done.

    python3 scripts/ms_write_probe.py --dry-run    # discovery only, no writes
"""
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

TOKEN = os.environ.get("MS_TOKEN", "").strip()
if not TOKEN:
    print("MS_TOKEN is not set.\n")
    print("  python3 scripts/ms_get_token.py --print-token")
    print("  MS_TOKEN='<paste>' python3 scripts/ms_write_probe.py")
    raise SystemExit(1)

DRY_RUN = "--dry-run" in sys.argv

BASE = "https://graph.microsoft.com/v1.0/"
CLASS_PROP = "String 0x001A"        # PR_MESSAGE_CLASS
PARENT_DISPLAY_PROP = "String 0x0E05"  # PR_PARENT_DISPLAY
CONTAINER_CLASS_PROP = "String 0x3613"  # PR_CONTAINER_CLASS
STICKY = "IPM.StickyNote"
STICKY_CONTAINER = "IPF.StickyNote"

SANDBOX_A = "JODD-PROBE-A"
SANDBOX_B = "JODD-PROBE-B"

# Every id this run brought into existence. Nothing outside it may be
# destroyed; see destroy().
MANIFEST = {"messages": [], "folders": []}

# Probe-created items deliberately NOT cleaned up, because confirming them
# means looking at Notes.app and Apple takes ~60s to sync — long after this
# script has exited. Moved out of MANIFEST so cleanup skips them, and listed
# at the end for manual removal.
KEEP = []


def keep_for_inspection(ident, why):
    """Spare one probe-created message from cleanup."""
    if ident in MANIFEST["messages"]:
        MANIFEST["messages"].remove(ident)
    KEEP.append((ident, why))


def forget(kind, ident):
    """Drop an id that no longer exists — deleted as a side effect of
    something else (a note inside a deleted folder, a message id replaced by
    a move). Without this, cleanup 404s on it and reports a phantom failure."""
    if ident and ident in MANIFEST[kind]:
        MANIFEST[kind].remove(ident)


# ─────────────────────────────────────────────────────────── transport

def graph(method, path, body=None, prefer_immutable=True):
    """Call a Graph path. Returns (status, parsed-body-or-{}).

    `Prefer: IdType="ImmutableId"` matches what the vertical sends on every
    request, so a difference in behaviour here is never an artifact of the
    probe asking differently. Probe 4 deliberately turns it off to measure the
    contrast, and that is the only place it should ever be false.
    """
    headers = {"Authorization": "Bearer " + TOKEN}
    if prefer_immutable:
        headers["Prefer"] = 'IdType="ImmutableId"'
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=45) as r:
            raw = r.read().decode()
            return r.status, (json.loads(raw) if raw.strip() else {})
    except urllib.error.HTTPError as e:
        raw = e.read().decode() or "{}"
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, {"raw": raw[:400]}
    except Exception as e:  # noqa: BLE001 - probe, report anything
        return 0, {"error": {"message": f"{type(e).__name__}: {e}"}}


def q(s):
    return urllib.parse.quote(s, safe="/?&=$'(),:")


def err(body):
    return (body.get("error") or {}).get("message", "")[:200]


# ──────────────────────────────────────────────── the property-id trap

def property_tag(pid):
    """('String 0x001A') -> ('string', 26). None if unparseable.

    Mirrors `wire::property_tag`. Graph does not echo the id you send: ask for
    `String 0x001A` and the response carries `String 0x1a`.
    """
    parts = pid.strip().split()
    if len(parts) != 2:
        return None
    kind, tag = parts[0].lower(), parts[1].lower()
    if not tag.startswith("0x"):
        return None
    try:
        return kind, int(tag[2:], 16)
    except ValueError:
        return None


def same_property_id(a, b):
    """Compare two property ids the way `wire::same_property_id` does."""
    pa, pb = property_tag(a), property_tag(b)
    if pa is None or pb is None:
        return a.strip().lower() == b.strip().lower()
    return pa == pb


def prop_value(item, wanted_id):
    """Read one singleValueExtendedProperty off an item, id-normalised."""
    for p in item.get("singleValueExtendedProperties") or []:
        if same_property_id(p.get("id", ""), wanted_id):
            return p.get("value")
    return None


# ────────────────────────────────────────────────── destructive guard

def destroy(kind, ident, what):
    """DELETE something, but only if this run created it.

    The guard is the point. A hard delete on this backend has no undo, so the
    check is not 'be careful' — it is the only thing standing between a probe
    bug and someone's notes.
    """
    if ident not in MANIFEST[kind]:
        print(f"    REFUSED to delete {what}: not created by this run ({ident[:24]}…)")
        return False
    path = "me/messages/" if kind == "messages" else "me/mailFolders/"
    st, body = graph("DELETE", q(path + ident))
    ok = st in (200, 204)
    print(f"    cleanup: DELETE {what} -> {st}" + ("" if ok else f"  {err(body)}"))
    if ok:
        MANIFEST[kind].remove(ident)
    return ok


def note_body(title, rest):
    """The shape CLAUDE.md records as verified: the title as a leading bare
    text node, then a <div> per line. Gotcha #11 — the title is the first
    LINE of the body, not a node shape."""
    return {"contentType": "HTML",
            "content": f"<html><body>{title}<div>{rest}</div></body></html>"}


def make_note(folder_id, title, rest, with_class=True):
    """POST a note. Returns (status, item). Records it in MANIFEST on success."""
    body = {"subject": title, "body": note_body(title, rest)}
    if with_class:
        body["singleValueExtendedProperties"] = [{"id": CLASS_PROP, "value": STICKY}]
    st, item = graph("POST", q(f"me/mailFolders/{folder_id}/messages"), body)
    if st in (200, 201) and item.get("id"):
        MANIFEST["messages"].append(item["id"])
    return st, item


# ───────────────────────────────────────────────────────────── discovery

print("=" * 72)
print("M2 write-path probe" + ("  [DRY RUN — no writes]" if DRY_RUN else ""))
print("=" * 72)

# Folders cannot be enumerated (gotcha #12): /me/mailFolders omits the Notes
# tree entirely and GET /mailFolders/{id} 404s. The ONLY route to a folder id
# is parentFolderId on a message, and the only route to its name is
# PR_PARENT_DISPLAY on that same message. Hence the sandbox needs a note in it.
print("\n[0] DISCOVERY — locate the sandbox folders through their notes")
sticky_filter = (f"singleValueExtendedProperties/any(ep: ep/id eq '{CLASS_PROP}' "
                 f"and ep/value eq '{STICKY}')")
path = ("me/messages?$top=200&$select=id,subject,parentFolderId,internetMessageId"
        f"&$filter={sticky_filter}"
        f"&$expand=singleValueExtendedProperties($filter=id eq '{PARENT_DISPLAY_PROP}')")
st, body = graph("GET", q(path))
if st != 200:
    print(f"    status {st}: {err(body)}")
    print("\n    Discovery failed — the token or scopes are wrong; every probe")
    print("    below would be uninterpretable. Stopping.")
    raise SystemExit(2)

folders = {}   # display name -> folder id
for m in body.get("value", []):
    name = prop_value(m, PARENT_DISPLAY_PROP)
    if name and m.get("parentFolderId"):
        folders.setdefault(name, m["parentFolderId"])
print(f"    {len(body.get('value', []))} notes, {len(folders)} folders visible")
print(f"    folders: {sorted(folders)}")

missing = [n for n in (SANDBOX_A, SANDBOX_B) if n not in folders]
if missing:
    print(f"\n    Sandbox folder(s) not found: {missing}")
    print("    Create them in Apple Notes and put ONE throwaway note in each —")
    print("    a folder holding zero notes cannot be discovered through Graph")
    print("    at all, so an empty one is invisible here. Then re-run.")
    raise SystemExit(3)

A_ID, B_ID = folders[SANDBOX_A], folders[SANDBOX_B]
print(f"    {SANDBOX_A}: {A_ID[:32]}…")
print(f"    {SANDBOX_B}: {B_ID[:32]}…")

if DRY_RUN:
    print("\n    --dry-run: sandbox located, stopping before any write.")
    raise SystemExit(0)


# ─────────────────────────────────────────── 1. can a folder be created?

print(f"\n[1] FOLDER CREATE — POST me/mailFolders/{{{SANDBOX_A}}}/childFolders")
print("    Decides whether folder-create exists in M2 at all. GET on a folder")
print("    id 404s (gotcha #12), so POST to one may 404 the same way.")

child_plain = None
st, item = graph("POST", q(f"me/mailFolders/{A_ID}/childFolders"),
                 {"displayName": "JODD-PROBE-CHILD"})
print(f"    plain displayName        -> {st}" + ("" if st == 201 else f"  {err(item)}"))
if st == 201 and item.get("id"):
    child_plain = item["id"]
    MANIFEST["folders"].append(child_plain)

child_classed = None
st, item = graph("POST", q(f"me/mailFolders/{A_ID}/childFolders"),
                 {"displayName": "JODD-PROBE-CHILD-CLASSED",
                  "singleValueExtendedProperties": [
                      {"id": CONTAINER_CLASS_PROP, "value": STICKY_CONTAINER}]})
print(f"    with PR_CONTAINER_CLASS  -> {st}" + ("" if st == 201 else f"  {err(item)}"))
if st == 201 and item.get("id"):
    child_classed = item["id"]
    MANIFEST["folders"].append(child_classed)

if not child_plain and not child_classed:
    print("    VERDICT: folder-create is IMPOSSIBLE on this backend. M2 must")
    print("             gate it off rather than implement it.")
elif child_plain and child_classed:
    print("    VERDICT: both forms accepted. Whether the class matters shows up")
    print("             in Notes.app — check which of the two appears there.")
else:
    print("    VERDICT: exactly one form worked — the container class is the")
    print("             deciding factor, mirroring PR_MESSAGE_CLASS on notes.")

probe_child = child_plain or child_classed
witness_id = None


# ───────────────────────────────────────────────────── 2. folder rename

print("\n[2] FOLDER RENAME — PATCH me/mailFolders/{child}")
if not probe_child:
    print("    SKIPPED: probe 1 created no folder to rename.")
else:
    st, item = graph("PATCH", q(f"me/mailFolders/{probe_child}"),
                     {"displayName": "JODD-PROBE-CHILD-RENAMED"})
    print(f"    status {st}" + ("" if st == 200 else f"  {err(item)}"))
    # The folder object is unreadable, so the rename cannot be confirmed by
    # reading the folder back. File a note into it and read the name off
    # PR_PARENT_DISPLAY — the same route the vertical uses.
    stn, note = make_note(probe_child, "probe-rename-witness", "confirms the new name")
    print(f"    witness note in the child -> {stn}")
    if stn in (200, 201):
        witness_id = note["id"]
        path = (f"me/messages/{note['id']}?$select=id,parentFolderId"
                f"&$expand=singleValueExtendedProperties($filter=id eq '{PARENT_DISPLAY_PROP}')")
        st2, back = graph("GET", q(path))
        seen = prop_value(back, PARENT_DISPLAY_PROP)
        print(f"    PR_PARENT_DISPLAY reads: {seen!r}")
        if seen == "JODD-PROBE-CHILD-RENAMED":
            print("    VERDICT: rename WORKS and is visible through the only route")
            print("             the vertical has to a folder name.")
        elif seen:
            print("    VERDICT: rename did NOT take — the name is still the old one.")
        else:
            print("    VERDICT: inconclusive — the property came back absent.")


# ──────────────────────────── 3. does a deleted folder id answer 404?

print("\n[3] FOLDER EXISTENCE — GET me/mailFolders/{id}/messages after a DELETE")
print("    THE architectural question. list_folders derives folders from notes,")
print("    so 'absent from the listing' conflates 'emptied' with 'deleted' and")
print("    prune_clean_folders cannot tell them apart — which is what makes a")
print("    Jodd-created folder vanish once its create push succeeds. If a")
print("    deleted id 404s here, a known id has a positive existence check and")
print("    that ambiguity disappears.")
if not probe_child:
    print("    SKIPPED: probe 1 created no folder to delete.")
else:
    st, body = graph("GET", q(f"me/mailFolders/{probe_child}/messages?$top=1"))
    print(f"    before delete: {st}, {len(body.get('value', []))} items")

    st_del = destroy("folders", probe_child, "the probe's child folder")
    if st_del:
        # The witness note lived inside that folder and went with it. Drop it
        # from the manifest or cleanup 404s on it and reports a phantom
        # failure. (It is probe-created either way, so nothing is at risk.)
        forget("messages", witness_id)

        st, body = graph("GET", q(f"me/mailFolders/{probe_child}/messages?$top=1"))
        print(f"    after delete:  {st}, {len(body.get('value', []))} items")
        if st == 404:
            print("    VERDICT: 404 — a known folder id HAS a positive existence")
            print("             check. list_folders can be 'derived from notes ∪")
            print("             known ids that still answer', which distinguishes")
            print("             emptied from deleted and fixes the vanishing folder.")
        elif st == 200:
            print("    VERDICT: 200-empty — the door is SHUT. A deleted folder is")
            print("             indistinguishable from an empty one, so M2 needs a")
            print("             different answer to the vanishing folder.")
        else:
            print(f"    VERDICT: unexpected {st} — {err(body)}")


# ──────────────────────────── 4. does a note id survive a folder move?

print("\n[4] MOVE + IMMUTABLE ID — POST me/messages/{id}/move")
print("    reconcile_one (lib.rs:1224) compares remote_version to the fetched")
print("    id. If the id changes on a move, every folder move made on the")
print("    iPhone manufactures a conflict copy.")

st, note = make_note(A_ID, "probe-move-immutable", "moved with Prefer: ImmutableId")
if st not in (200, 201):
    print(f"    SKIPPED: could not create the note ({st}) {err(note)}")
else:
    before_id, before_imid = note["id"], note.get("internetMessageId")
    st, moved = graph("POST", q(f"me/messages/{before_id}/move"), {"destinationId": B_ID})
    print(f"    move status {st}" + ("" if st in (200, 201) else f"  {err(moved)}"))
    if st in (200, 201):
        after_id = moved.get("id", "")
        if after_id and after_id != before_id:
            MANIFEST["messages"].append(after_id)
            forget("messages", before_id)  # the pre-move id no longer resolves
        print(f"    id before: {before_id[:40]}…")
        print(f"    id after:  {after_id[:40]}…")
        print(f"    id preserved:              {after_id == before_id}")
        print(f"    internetMessageId preserved: "
              f"{moved.get('internetMessageId') == before_imid}")
        if after_id == before_id:
            print("    VERDICT: the id SURVIVES the move under ImmutableId — no")
            print("             conflict copy. Verify the contrast below.")
        else:
            print("    VERDICT: the id CHANGED even with ImmutableId. reconcile_one")
            print("             must stop keying on the Graph id for this backend.")

# The contrast. This is the only call in the script that omits the header, and
# it exists so 'the id survived' above cannot be a coincidence of this mailbox.
st, note2 = make_note(A_ID, "probe-move-no-header", "moved WITHOUT the Prefer header")
if st in (200, 201):
    b2 = note2["id"]
    st, moved2 = graph("POST", q(f"me/messages/{b2}/move"), {"destinationId": B_ID},
                       prefer_immutable=False)
    if st in (200, 201):
        a2 = moved2.get("id", "")
        if a2 and a2 != b2:
            MANIFEST["messages"].append(a2)
            forget("messages", b2)
        print(f"    contrast, no Prefer header — id preserved: {a2 == b2}")
        print("    (if this is False while the ImmutableId case was True, the")
        print("     header is load-bearing and must never be dropped)")


# ─────────────────────────────────────────── 5. title change round-trip

print("\n[5] TITLE CHANGE — PATCH subject + body together")
print("    Apple duplicates the title into the body, so subject and the leading")
print("    body line must move together or the note shows two different titles.")
st, note = make_note(A_ID, "probe-title-before", "the body line under the title")
if st not in (200, 201):
    print(f"    SKIPPED: could not create the note ({st}) {err(note)}")
else:
    nid = note["id"]
    st, patched = graph("PATCH", q(f"me/messages/{nid}"),
                        {"subject": "probe-title-after",
                         "body": note_body("probe-title-after", "the body line under the title")})
    print(f"    patch status {st}" + ("" if st == 200 else f"  {err(patched)}"))
    if st == 200:
        st, back = graph("GET", q(f"me/messages/{nid}?$select=id,subject,body"))
        content = (back.get("body") or {}).get("content", "")
        print(f"    subject reads back: {back.get('subject')!r}")
        print(f"    id unchanged by PATCH: {back.get('id') == nid}")
        print(f"    body leads with the new title: {'probe-title-after' in content[:200]}")
        print("    MANUAL CHECK: within ~60s, Notes.app should show ONE note")
        print("                  titled 'probe-title-after' with no duplicate")
        print("                  title line and no second copy of the note.")
        # Apple needs ~60s to sync and this script exits in seconds, so
        # deleting it now would delete the thing being checked.
        keep_for_inspection(nid, "probe 5 — confirm the title change in Notes.app")


# ───────────────────────── 6. POST without the message class — silent?

print("\n[6] MISSING MESSAGE CLASS — POST with no PR_MESSAGE_CLASS")
print("    Only the positive case was ever tested. If omitting the class")
print("    silently succeeds as IPM.Note, the bug is not an error anyone sees:")
print("    it is an email quietly deposited in the mailbox on every note create.")
st, item = make_note(A_ID, "probe-no-class", "posted without PR_MESSAGE_CLASS",
                     with_class=False)
print(f"    status {st}" + ("" if st in (200, 201) else f"  {err(item)}"))
if st in (200, 201):
    path = (f"me/messages/{item['id']}?$select=id,subject,isDraft"
            f"&$expand=singleValueExtendedProperties($filter=id eq '{CLASS_PROP}')")
    st2, back = graph("GET", q(path))
    cls = prop_value(back, CLASS_PROP)
    print(f"    message class: {cls!r}   isDraft: {back.get('isDraft')}")
    if cls == STICKY:
        print("    VERDICT: Graph inferred IPM.StickyNote from the folder. Setting")
        print("             the class explicitly is belt-and-braces, not required.")
    elif cls:
        print(f"    VERDICT: landed as {cls} — an EMAIL in the Notes folder. The")
        print("             explicit class is REQUIRED, and omitting it fails")
        print("             silently. Check whether it shows in Notes.app at all.")
    else:
        print("    VERDICT: class came back absent — inconclusive, re-probe singly.")
else:
    print("    VERDICT: rejected outright — the class is required and the failure")
    print("             is loud, which is the good outcome.")


# ──────────────────────────────────────────────────────────── cleanup

print("\n[7] CLEANUP — removing only what this run created")
for mid in list(MANIFEST["messages"]):
    destroy("messages", mid, f"message {mid[:20]}…")
for fid in list(MANIFEST["folders"]):
    destroy("folders", fid, f"folder {fid[:20]}…")

print("\n" + "=" * 72)
leftover = MANIFEST["messages"] + MANIFEST["folders"]
if leftover:
    print(f"{len(leftover)} item(s) this run created could NOT be deleted:")
    for i in leftover:
        print(f"    {i}")
    print("Remove them in Notes.app.")
else:
    print("Every item this run created was removed.")
if KEEP:
    print(f"\n{len(KEEP)} item(s) left in place ON PURPOSE, for you to inspect:")
    for ident, why in KEEP:
        print(f"    {ident[:40]}…  {why}")
    print("Delete them in Notes.app once you have looked.")
print(f"Delete {SANDBOX_A} and {SANDBOX_B} in Notes.app when probing is done.")
print("=" * 72)
