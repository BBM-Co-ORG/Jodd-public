#!/usr/bin/env python3
"""Follow-up probe: does a Graph-created folder appear in APPLE NOTES?

ms_write_probe.py established that `POST /me/mailFolders/{id}/childFolders`
returns 201 both with and without `PR_CONTAINER_CLASS`. It could not establish
anything more, because cleanup removed both folders within seconds — long
before Apple's ~60s sync — so no human ever saw them.

That gap matters because the same run proved this API says 201 to something
Apple will not accept. A note POSTed without `PR_MESSAGE_CLASS` came back
`201`, and the item was an `IPM.Note`: an EMAIL, silently filed in the Notes
folder. A status code from Graph is not evidence that Apple agrees.

So this probe asks the question the other one couldn't:

    1. Does a Graph-created folder show up in Notes.app at all?
    2. Does PR_CONTAINER_CLASS = IPF.StickyNote decide whether it does?
    3. Does creating under the `Notes` ROOT work — which is what Jodd's
       create_folder will actually do, rather than under a subfolder?

It creates two folders under the Notes root, files one note into each (an
empty folder may not sync, and cannot be re-found through Graph either —
gotcha #12), and then DELIBERATELY LEAVES THEM so you can look. Nothing is
cleaned up: this probe's whole output is what you see in Notes.app.

    python3 scripts/ms_get_token.py --print-token
    MS_TOKEN='<paste>' python3 scripts/ms_folder_probe.py

Everything it creates is listed at the end for manual removal. It creates
only; it never deletes, so it cannot touch anything pre-existing at all.
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
    print("  MS_TOKEN='<paste>' python3 scripts/ms_folder_probe.py")
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
print("Folder visibility probe — does Apple Notes see a Graph-created folder?")
print("=" * 72)

# ── find the Notes root ──────────────────────────────────────────────────
# Jodd's create_folder will target the root, not a subfolder. The root's id is
# reachable only if some note sits DIRECTLY in it, because a folder id comes
# only from parentFolderId on a message (gotcha #12). If no note lives in the
# root, Jodd cannot create a folder at all — which is itself a finding M2 has
# to design around, so report it plainly rather than falling back to a
# subfolder and hiding it.
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
    print("    FINDING: on such an account Jodd could not create a folder at all.")
    print("    Put one note directly in the top-level Notes folder and re-run.")
    raise SystemExit(3)

ROOT = folders[ROOT_NAME]
print(f"    {ROOT_NAME}: {ROOT[:40]}…")

# ── create the two folders under the root ────────────────────────────────
print("\n[1] Creating two folders under the root — one classed, one not")
targets = [
    ("JODD-VIS-PLAIN", None),
    ("JODD-VIS-CLASSED", STICKY_CONTAINER),
]
made = []
for name, container_class in targets:
    payload = {"displayName": name}
    if container_class:
        payload["singleValueExtendedProperties"] = [
            {"id": CONTAINER_CLASS_PROP, "value": container_class}]
    st, item = graph("POST", q(f"me/mailFolders/{ROOT}/childFolders"), payload)
    label = "with PR_CONTAINER_CLASS" if container_class else "plain displayName  "
    print(f"    {name:18} {label} -> {st}" + ("" if st == 201 else f"  {err(item)}"))
    if st == 201 and item.get("id"):
        made.append((name, item["id"]))
        created.append(("folder", name, item["id"]))

if not made:
    print("    Neither folder was created — nothing to look at. Stopping.")
    raise SystemExit(4)

# ── file a note into each, so Apple has something to sync ────────────────
# An empty folder is invisible to Graph and may not sync to Apple at all, so a
# note is the only way to make the folder observable from either side.
print("\n[2] Filing one note into each, so the folder is observable")
for name, fid in made:
    st, item = graph("POST", q(f"me/mailFolders/{fid}/messages"), {
        "subject": f"witness for {name}",
        "body": {"contentType": "HTML",
                 "content": f"<html><body>witness for {name}"
                            f"<div>created by ms_folder_probe</div></body></html>"},
        "singleValueExtendedProperties": [{"id": CLASS_PROP, "value": STICKY}],
    })
    print(f"    note in {name:18} -> {st}" + ("" if st == 201 else f"  {err(item)}"))
    if st == 201 and item.get("id"):
        created.append(("note", f"witness for {name}", item["id"]))

# ── what to look for ─────────────────────────────────────────────────────
print("\n" + "=" * 72)
print("NOTHING WAS DELETED. Wait ~60s, then look in Notes.app and answer:")
print()
print("  a) Do BOTH 'JODD-VIS-PLAIN' and 'JODD-VIS-CLASSED' appear as folders?")
print("  b) If only one appears, PR_CONTAINER_CLASS is required on create —")
print("     the same silent-wrong shape as PR_MESSAGE_CLASS on notes.")
print("  c) If NEITHER appears, a Graph-created folder is invisible to Apple")
print("     and M2 must not offer folder creation on this backend.")
print("  d) Does each contain its witness note, and is the title right?")
print()
print("Created (delete these in Notes.app when done):")
for kind, name, ident in created:
    print(f"    {kind:6} {name:28} {ident[:36]}…")
print("=" * 72)
