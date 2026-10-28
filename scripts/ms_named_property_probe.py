#!/usr/bin/env python3
"""M4 probe: does a MAPI named property under a Jodd-owned GUID actually work?

The M2 spec's "M4 sidecars" section proposes storing pin/tags as a single
JSON-valued MAPI **named property** set directly on the note's own message —
not a sidecar message (which M1 already flagged as a problem: it shows up in
Notes.app as a stray note). The reasoning was "extended properties on
messages are proven to work — that's how PR_MESSAGE_CLASS gets set" — but
every extended property Jodd has used so far is a **numbered** MAPI tag
(`String 0x001A`, `String 0x3613`). A **named** property under a custom GUID
uses different id syntax entirely (`String {GUID} Name PropertyName`), and
`wire.rs`'s own doc comment on `same_property_id` says named properties are
"not used today" — genuinely untested territory, not an extrapolation from
what already works.

Four questions this probe answers, live, that the M4 plan currently assumes
rather than measures:

    1. Does Graph accept + round-trip a GUID-named property at all?
    2. Does it SURVIVE a normal content PATCH (title/body edit) — or does
       every future save need to resend it?
    3. Does the "one property per $expand" rule (gotcha #12, proven for
       numbered tags) also hold for a named one?
    4. THE ACTUAL M4 HYPOTHESIS: does the note still look completely normal
       in Notes.app — title, body, no corruption — while carrying a property
       Apple doesn't recognize? Never measured until now.

Creates ONE throwaway note. Notes are proven cleanly hard-deletable (M2), so
this is much lower-stakes than the folder probes — nothing is left behind
except this one note, printed at the end for manual cleanup once you've
looked at it in Notes.app.

    python3 scripts/ms_get_token.py --print-token
    MS_TOKEN='<paste>' python3 scripts/ms_named_property_probe.py
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
    print("  MS_TOKEN='<paste>' python3 scripts/ms_named_property_probe.py")
    raise SystemExit(1)

BASE = "https://graph.microsoft.com/v1.0/"
CLASS_PROP = "String 0x001A"
PARENT_DISPLAY_PROP = "String 0x0E05"
STICKY = "IPM.StickyNote"
ROOT_NAME = "Notes"

# Jodd's own property-set GUID — minted fresh for this probe (uuid4, not tied
# to any existing Microsoft-defined property set). If this mechanism works,
# this becomes a permanent constant in wire.rs.
JODD_GUID = "98d27db2-72ad-4511-a3b5-b73c7c42694b"
JODD_PROP_NAME = "JoddMeta"
JODD_PROP_ID = f"String {{{JODD_GUID}}} Name {JODD_PROP_NAME}"


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
    return urllib.parse.quote(s, safe="/?&=$'(),:{}")


def err(body):
    return (body.get("error") or {}).get("message", "")[:250]


def property_tag(pid):
    """`"String 0x001A"` -> `("string", 0x1a)`; `"String 0x1a"` -> the same.
    Named-property ids (`String {guid} Name X`) don't match this shape and
    fall through to a plain string compare in same_property_id below."""
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
    """Graph returns `String 0x1a` for the `String 0x001A` you sent — this
    bit the codebase before (gotcha in CLAUDE.md / wire.rs's same_property_id)
    and would have silently broken this probe's own readback checks too."""
    pa, pb = property_tag(a), property_tag(b)
    if pa is not None and pb is not None:
        return pa == pb
    return a.strip().lower() == b.strip().lower()


def prop_value(item, wanted_id):
    for p in item.get("singleValueExtendedProperties") or []:
        if same_property_id(p.get("id", ""), wanted_id):
            return p.get("value")
    return None


print("=" * 72)
print("Named-property probe — GUID-named MAPI property on a note message")
print(f"Property id: {JODD_PROP_ID}")
print("=" * 72)

# ── find the Notes root ──────────────────────────────────────────────────
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
    print("    File one note directly in the top-level Notes folder and re-run.")
    raise SystemExit(3)

ROOT = folders[ROOT_NAME]
print(f"    {ROOT_NAME}: {ROOT[:40]}…")

# ── [1] create a note carrying the named property at creation time ───────
print("\n[1] Creating a note with PR_MESSAGE_CLASS + the named property")
sidecar_value_v1 = json.dumps({"pinned": True, "tags": ["m4test"]})
payload = {
    "subject": "JODD-M4-NAMEDPROP",
    "body": {"contentType": "HTML",
             "content": "<html><body>JODD-M4-NAMEDPROP"
                        "<div>probing a GUID-named MAPI property</div></body></html>"},
    "singleValueExtendedProperties": [
        {"id": CLASS_PROP, "value": STICKY},
        {"id": JODD_PROP_ID, "value": sidecar_value_v1},
    ],
}
st, item = graph("POST", q(f"me/mailFolders/{ROOT}/messages"), payload)
print(f"    create -> {st}" + ("" if st == 201 else f"  {err(item)}"))
if st != 201 or not item.get("id"):
    print("    Creation failed outright — stopping.")
    raise SystemExit(4)
msg_id = item["id"]
print(f"    message id: {msg_id[:40]}…")

# ── [2] read it back via its OWN scoped $expand ───────────────────────────
print("\n[2] Reading the property back via a request scoped to JUST this id")
read_q = (f"me/messages/{q(msg_id)}?$select=id,subject"
          f"&$expand=singleValueExtendedProperties($filter=id eq '{JODD_PROP_ID}')")
st, item = graph("GET", q(read_q))
readback_v1 = prop_value(item, JODD_PROP_ID) if st == 200 else None
print(f"    status {st}, value read back: {readback_v1!r}")
if readback_v1 == sidecar_value_v1:
    print("    MATCH — Graph round-trips a GUID-named property on creation.")
elif st == 200 and not (item.get("singleValueExtendedProperties")):
    print("    200 but NO properties at all — this is gotcha #12's silent-")
    print("    rejection shape (an $expand with more than the filter it can")
    print("    parse answers 200-and-nothing). The named-property $expand")
    print("    syntax itself may need adjustment.")
else:
    print("    MISMATCH or missing — Graph is not preserving this property")
    print("    as sent. Treat the named-property approach as unconfirmed.")

# ── [3] does a normal content PATCH wipe it? ──────────────────────────────
print("\n[3] PATCHing subject/body (simulating a normal Jodd edit), then re-reading")
st, _ = graph("PATCH", q(f"me/messages/{q(msg_id)}"), {
    "subject": "JODD-M4-NAMEDPROP-EDITED",
    "body": {"contentType": "HTML",
             "content": "<html><body>JODD-M4-NAMEDPROP-EDITED"
                        "<div>edited after the named property was set</div></body></html>"},
})
print(f"    PATCH content -> {st}")
st, item = graph("GET", q(read_q))
readback_v2 = prop_value(item, JODD_PROP_ID) if st == 200 else None
print(f"    status {st}, value after content edit: {readback_v2!r}")
if readback_v2 == sidecar_value_v1:
    print("    SURVIVED — a content PATCH does not disturb the named property.")
    print("    Jodd's save path would NOT need to resend it on every edit.")
elif readback_v2 is None:
    print("    WIPED — a content-only PATCH cleared the named property.")
    print("    Jodd's save path would need to resend it on every content edit.")
else:
    print("    CHANGED to something else — unexpected, investigate.")

# ── [4] can we independently update JUST the named property afterward? ───
print("\n[4] Updating ONLY the named property via its own PATCH (no content touched)")
sidecar_value_v2 = json.dumps({"pinned": False, "tags": ["m4test", "second-write"]})
st, _ = graph("PATCH", q(f"me/messages/{q(msg_id)}"), {
    "singleValueExtendedProperties": [{"id": JODD_PROP_ID, "value": sidecar_value_v2}],
})
print(f"    PATCH property-only -> {st}")
st, item = graph("GET", q(read_q))
readback_v3 = prop_value(item, JODD_PROP_ID) if st == 200 else None
print(f"    status {st}, value after property-only update: {readback_v3!r}")
if readback_v3 == sidecar_value_v2:
    print("    MATCH — a property-only PATCH updates it without touching content.")
else:
    print("    MISMATCH — property-only PATCH did not take as expected.")

# ── what to look for in Notes.app ─────────────────────────────────────────
print("\n" + "=" * 72)
print("NOTHING WAS DELETED. Wait for ActiveSync, then check Notes.app AND")
print("outlook.live.com/mail/notes for the note titled")
print("'JODD-M4-NAMEDPROP-EDITED' (or 'JODD-M4-NAMEDPROP' if the edit hasn't")
print("landed yet) in the Notes root, and answer:")
print()
print("  a) Does the note appear at all, with the right title and body?")
print("  b) Does it render normally — no corruption, no error, nothing that")
print("     suggests Apple choked on the unrecognized property? THIS is the")
print("     core M4 hypothesis: 'Apple only reads fields it knows about.'")
print()
print("Cleanup when done (safe — note delete is hard but clean, per M2):")
print(f"    message id: {msg_id}")
print(f"    MS_TOKEN=... python3 -c \"import urllib.request; "
      f"req=urllib.request.Request('{BASE}me/messages/{msg_id}', "
      f"headers={{'Authorization':'Bearer '+__import__('os').environ['MS_TOKEN']}}, "
      f"method='DELETE'); urllib.request.urlopen(req)\"")
print("=" * 72)
