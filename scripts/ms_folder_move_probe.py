#!/usr/bin/env python3
"""M3 probe: the one folder-write avenue M2 left untried.

M2 established, live, that the ONLY call able to nest a folder under `Notes`
(`POST /me/mailFolders/{id}/childFolders`) silently drops `PR_CONTAINER_CLASS`
on creation, and that trying to set the class AFTER creation on such a folder
answers `500 ErrorObjectTypeChanged` — the container class is the object type,
and it is immutable post-creation. Neither classed nor unclassed childFolders
output ever reached Notes.app; both were confirmed absent 21 hours later. See
CLAUDE.md's "How Apple Notes ↔ Outlook.com works" and the M2 spec's "Deferred
to M3" section for the full mechanism.

The one avenue that mechanism does NOT rule out: `POST /me/mailFolders` (the
OTHER documented creation path) creates at the mailbox ROOT, outside
childFolders entirely, so the immutability finding above may not apply to it.
If it sets the class correctly at creation — untested until now — a subsequent
`POST /me/mailFolders/{id}/move` under the Notes root might carry a folder
Apple accepts. This has never been run. `GET /mailFolders/{id}` 404s for every
folder either way (gotcha #12), so this is NOT verifiable by reading — the
only way to know is to create, move, wait for ActiveSync, and look in
Notes.app directly.

Two folders, so the classed/unclassed question this backend has asked before
(PR_MESSAGE_CLASS mattered for notes; PR_CONTAINER_CLASS did not seem to for
childFolders) gets asked again on this untested path rather than assumed by
analogy from the childFolders result:

    JODD-MOVE-PLAIN    root-create (no class)      -> move under Notes
    JODD-MOVE-CLASSED  root-create + PR_CONTAINER_CLASS -> move under Notes

Nothing is deleted. Everything created is listed at the end for manual
removal in Notes.app once you've looked.

    python3 scripts/ms_get_token.py --print-token
    MS_TOKEN='<paste>' python3 scripts/ms_folder_move_probe.py
"""
import json
import os
import urllib.error
import urllib.parse
import urllib.request

TOKEN = os.environ.get("MS_TOKEN", "").strip()
if not TOKEN:
    print("MS_TOKEN is not set.\n")
    print("  python3 scripts/ms_get_token.py --print-token")
    print("  MS_TOKEN='<paste>' python3 scripts/ms_folder_move_probe.py")
    raise SystemExit(1)

BASE = "https://graph.microsoft.com/v1.0/"
CLASS_PROP = "String 0x001A"
PARENT_DISPLAY_PROP = "String 0x0E05"
CONTAINER_CLASS_PROP = "String 0x3613"
STICKY = "IPM.StickyNote"
STICKY_CONTAINER = "IPF.StickyNote"
ROOT_NAME = "Notes"


def graph(method, path, body=None):
    headers = {"Authorization": "Bearer " + TOKEN, "Prefer": 'IdType="ImmutableId"'}
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


def property_tag(pid):
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
    """Graph returns `String 0x1a` for the `String 0x001A` you sent."""
    pa, pb = property_tag(a), property_tag(b)
    if pa is None or pb is None:
        return a.strip().lower() == b.strip().lower()
    return pa == pb


def prop_value(item, wanted_id):
    for p in item.get("singleValueExtendedProperties") or []:
        if same_property_id(p.get("id", ""), wanted_id):
            return p.get("value")
    return None


created = []

print("=" * 72)
print("Folder move probe — create at root, move under Notes, then look")
print("=" * 72)

# ── find the Notes root ──────────────────────────────────────────────────
# Same discovery as ms_folder_probe.py: the root's id is reachable only
# through a note filed directly in it (gotcha #12).
print(f"\n[0] Locating the {ROOT_NAME!r} root through a note filed directly in it")
sticky_filter = (f"singleValueExtendedProperties/any(ep: ep/id eq '{CLASS_PROP}' "
                 f"and ep/value eq '{STICKY}')")
path = ("me/messages?$top=200&$select=id,subject,parentFolderId"
        f"&$filter={sticky_filter}"
        f"&$expand=singleValueExtendedProperties($filter=id eq '{PARENT_DISPLAY_PROP}')")
st, body = graph("GET", q(path))
if st != 200:
    print(f"    status {st}: {err(body)}")
    raise SystemExit(2)

folders = {}
for m in body.get("value", []):
    name = prop_value(m, PARENT_DISPLAY_PROP)
    if name and m.get("parentFolderId"):
        folders.setdefault(name, m["parentFolderId"])

if ROOT_NAME not in folders:
    print(f"    {ROOT_NAME!r} is NOT among {sorted(folders)}")
    print("    No note sits directly in the root, so its id is unreachable.")
    print("    File one note directly in the top-level Notes folder and re-run.")
    raise SystemExit(3)

ROOT = folders[ROOT_NAME]
print(f"    {ROOT_NAME}: {ROOT[:40]}…")

# ── create two folders at the mailbox ROOT (not childFolders) ────────────
print("\n[1] Creating two folders at the mailbox root via POST /me/mailFolders")
targets = [
    ("JODD-MOVE-PLAIN", None),
    ("JODD-MOVE-CLASSED", STICKY_CONTAINER),
]
made = []
for name, container_class in targets:
    payload = {"displayName": name}
    if container_class:
        payload["singleValueExtendedProperties"] = [
            {"id": CONTAINER_CLASS_PROP, "value": container_class}]
    st, item = graph("POST", "me/mailFolders", payload)
    label = "with PR_CONTAINER_CLASS" if container_class else "plain displayName  "
    print(f"    {name:18} {label} -> {st}" + ("" if st == 201 else f"  {err(item)}"))
    if st == 201 and item.get("id"):
        made.append((name, item["id"]))
        created.append(("folder (root, pre-move)", name, item["id"]))

if not made:
    print("    Neither folder was created — nothing to move. Stopping.")
    raise SystemExit(4)

# ── move each under the Notes root ────────────────────────────────────────
print("\n[2] Moving each folder under the Notes root via POST .../move")
moved = []
for name, fid in made:
    st, item = graph("POST", q(f"me/mailFolders/{fid}/move"), {"destinationId": ROOT})
    new_id = item.get("id", fid) if st in (200, 201) else fid
    print(f"    {name:18} {fid[:24]}… -> {st}" + ("" if st in (200, 201) else f"  {err(item)}"))
    if st in (200, 201):
        moved.append((name, new_id))
        if new_id != fid:
            print(f"        id changed by the move: {fid[:24]}… -> {new_id[:24]}…")
            created.append(("folder (post-move id)", name, new_id))

if not moved:
    print("    Nothing moved successfully — nothing to look at. Stopping.")
    raise SystemExit(5)

# ── file a witness note into each, so the folder is observable ───────────
print("\n[3] Filing one witness note into each moved folder")
for name, fid in moved:
    st, item = graph("POST", q(f"me/mailFolders/{fid}/messages"), {
        "subject": f"witness for {name}",
        "body": {"contentType": "HTML",
                 "content": f"<html><body>witness for {name}"
                            f"<div>created by ms_folder_move_probe</div></body></html>"},
        "singleValueExtendedProperties": [{"id": CLASS_PROP, "value": STICKY}],
    })
    print(f"    note in {name:18} -> {st}" + ("" if st == 201 else f"  {err(item)}"))
    if st == 201 and item.get("id"):
        created.append(("note", f"witness for {name}", item["id"]))

# ── what to look for ─────────────────────────────────────────────────────
print("\n" + "=" * 72)
print("NOTHING WAS DELETED. Apple's ActiveSync has taken up to ~30 min during")
print("earlier M2 testing — do not conclude absence early. Check Notes.app AND")
print("outlook.live.com/mail/notes (a third observer that separates 'we wrote")
print("it wrong' from 'Apple has not caught up yet'), then answer:")
print()
print("  a) Do 'JODD-MOVE-PLAIN' and/or 'JODD-MOVE-CLASSED' appear as folders")
print("     under Notes in Notes.app?")
print("  b) If only CLASSED appears, the class must be set at root-create time")
print("     and survives the move — this avenue works, with that requirement.")
print("  c) If NEITHER appears, create-then-move does not produce a folder")
print("     Apple accepts either — the avenue is dead, and folder create should")
print("     be documented as a permanent limitation of this backend.")
print("  d) Does each surviving folder contain its witness note?")
print()
print("Created (delete these in Notes.app when done):")
for kind, name, ident in created:
    print(f"    {kind:24} {name:28} {ident[:36]}…")
print("=" * 72)
