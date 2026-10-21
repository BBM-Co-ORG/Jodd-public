#!/usr/bin/env python3
"""Task 8 probe: does Graph accept a server-side $filter over an extended property?

The Microsoft vertical currently identifies notes CLIENT-side: it asks for
PR_MESSAGE_CLASS (`String 0x001A`) via $expand and keeps only `IPM.StickyNote`.
That works and is proven, but it downloads every ordinary email in the mailbox —
including bodies — to throw most of them away. A server-side $filter would let
Graph do the discarding.

This probe answers whether that is possible. It is deliberately a probe and not
a change: the shipped code keeps the client-side filter until this says
otherwise.

WHY THIS SCRIPT EXISTS SEPARATELY FROM ms_get_token.py: so the access token
never has to be pasted anywhere it might persist. Run the token helper, then
hand the token to this script through the environment in your own shell:

    python3 scripts/ms_get_token.py --print-token
    MS_TOKEN='<paste the token>' python3 scripts/ms_probe_filter.py

The token is read from $MS_TOKEN and never printed back.

A CAUTION THIS PROJECT LEARNED THE HARD WAY (CLAUDE.md, 2026-08-14): an
`$expand=...($filter=id eq 'A' or id eq 'B')` returns **200 with no properties
at all** rather than an error, which reads exactly like "the property does not
exist". So a 200 here is not by itself success — every probe below checks what
came back, not just the status code.
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
    print("  MS_TOKEN='<paste>' python3 scripts/ms_probe_filter.py")
    raise SystemExit(1)

BASE = "https://graph.microsoft.com/v1.0/"
CLASS_PROP = "String 0x001A"
STICKY = "IPM.StickyNote"


def graph(path, prefer_immutable=True):
    """GET a Graph path. Returns (status, parsed-body-or-{})."""
    headers = {"Authorization": "Bearer " + TOKEN}
    if prefer_immutable:
        # The vertical sends this on every request, so the probe must too —
        # otherwise a difference here would be attributed to the $filter.
        headers["Prefer"] = 'IdType="ImmutableId"'
    req = urllib.request.Request(BASE + path, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=45) as r:
            return r.status, json.loads(r.read().decode())
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


def classes_of(body):
    """Message class of each returned item, via the $expand'ed property."""
    out = []
    for m in body.get("value", []):
        props = m.get("singleValueExtendedProperties") or []
        out.append(props[0]["value"] if props else "<absent>")
    return out


print("=" * 72)
print("Task 8 — server-side $filter over an extended property")
print("=" * 72)

# ---------------------------------------------------------------- baseline
# What the shipped code does today. Everything else is measured against this.
print("\n[0] BASELINE — client-side filter (what ships today)")
path = ("me/messages?$top=50&$select=id,subject"
        f"&$expand=singleValueExtendedProperties($filter=id eq '{CLASS_PROP}')")
st, body = graph(q(path))
base_items = body.get("value", []) if st == 200 else []
base_classes = classes_of(body) if st == 200 else []
base_notes = [c for c in base_classes if c == STICKY]
print(f"    status {st}, {len(base_items)} items returned")
if st != 200:
    print("    error:", err(body))
    print("\n    Baseline failed — the token or scopes are wrong; later probes")
    print("    would be uninterpretable. Stopping.")
    raise SystemExit(2)
print(f"    of those, {len(base_notes)} are {STICKY}")
if base_classes and all(c == "<absent>" for c in base_classes):
    print("    !! the property came back absent on every item — see the caution")
    print("       in this file's docstring; treat all results below as suspect")

# ------------------------------------------------------------------ probe 1
# The documented OData shape for filtering on a collection-valued property.
print("\n[1] $filter with any(ep: id eq ... and value eq ...)")
f1 = (f"singleValueExtendedProperties/any(ep: ep/id eq '{CLASS_PROP}' "
      f"and ep/value eq '{STICKY}')")
path = f"me/messages?$top=50&$select=id,subject&$filter={f1}"
st, body = graph(q(path))
n = len(body.get("value", []))
print(f"    status {st}, {n} items")
if st == 200:
    print(f"    baseline said {len(base_notes)} of the first 50 were notes")
    if n == 0 and base_notes:
        print("    VERDICT: accepted but returns NOTHING — a silent empty result,")
        print("             which is worse than an error. Do NOT adopt.")
    elif n > 0:
        print("    VERDICT: WORKS — this is the optimisation Task 8 was looking for.")
        print("             Confirm the count is plausible before adopting.")
    else:
        print("    VERDICT: inconclusive — no notes in the sample to distinguish")
        print("             'filtered correctly' from 'filtered everything away'.")
else:
    print("    error:", err(body))
    print("    VERDICT: rejected — keep the client-side filter.")

# ------------------------------------------------------------------ probe 2
# Same filter, but also expanding the property, to see whether the two can
# coexist — the vertical needs the value as well as the filtering.
print("\n[2] the same $filter combined with $expand of the same property")
path = (f"me/messages?$top=50&$select=id,subject&$filter={f1}"
        f"&$expand=singleValueExtendedProperties($filter=id eq '{CLASS_PROP}')")
st, body = graph(q(path))
n = len(body.get("value", []))
print(f"    status {st}, {n} items")
if st == 200:
    cls = classes_of(body)
    bad = [c for c in cls if c != STICKY]
    print(f"    classes returned: {sorted(set(cls))}")
    if not cls:
        print("    VERDICT: empty — cannot combine, or nothing matched.")
    elif bad:
        print("    VERDICT: the $filter did NOT actually restrict the results.")
    else:
        print("    VERDICT: WORKS and keeps the value — the ideal outcome.")
else:
    print("    error:", err(body))

# ------------------------------------------------------------------ probe 3
# Narrower form: filter on id only, to separate "cannot filter on this
# property at all" from "cannot filter on its value".
print("\n[3] $filter on the property id alone (no value comparison)")
f3 = f"singleValueExtendedProperties/any(ep: ep/id eq '{CLASS_PROP}')"
path = f"me/messages?$top=50&$select=id,subject&$filter={f3}"
st, body = graph(q(path))
print(f"    status {st}, {len(body.get('value', []))} items")
if st != 200:
    print("    error:", err(body))
    print("    (a failure here means the property cannot be filtered on at all,")
    print("     not merely that its value cannot be compared)")

# ------------------------------------------------------------------ probe 4
# Does $filter survive alongside the paging the vertical relies on?
print("\n[4] does a filtered request still page? ($top=2)")
path = f"me/messages?$top=2&$select=id&$filter={f1}"
st, body = graph(q(path))
print(f"    status {st}, {len(body.get('value', []))} items, "
      f"nextLink {'present' if body.get('@odata.nextLink') else 'absent'}")
if st != 200:
    print("    error:", err(body))

# --------------------------------------------------------------- probe 5
# Unrelated to the filter, but it needs a live token and the branch is
# waiting on it: does the Deleted Items exclusion have anything to match?
print("\n[5] BONUS — is a well-known folder id comparable to parentFolderId?")
print("    (the branch excludes Deleted Items by id; if the id forms differ,")
print("     that exclusion silently matches nothing)")
st, body = graph("me/mailFolders/deleteditems?$select=id,displayName")
if st == 200:
    del_id = body.get("id", "")
    print(f"    deleteditems resolved, id starts {del_id[:24]}…")
    st2, b2 = graph(q("me/messages?$top=5&$select=id,parentFolderId"))
    if st2 == 200 and b2.get("value"):
        pf = b2["value"][0].get("parentFolderId", "")
        print(f"    a message's parentFolderId starts {pf[:24]}…")
        same_shape = len(pf) == len(del_id) or pf[:8] == del_id[:8]
        print(f"    same id family? {'looks like it' if same_shape else 'NO — SUSPECT'}")
        print("    (definitive check: trash a note on the iPhone, then see whether")
        print("     its parentFolderId equals this deleteditems id exactly)")
else:
    print(f"    status {st}: {err(body)}")

print("\n" + "=" * 72)
print("Record the verdicts in the plan's Task 8 before changing any code.")
print("=" * 72)
