#!/usr/bin/env python3
"""M4 Phase 1 live probe: does `$filter=internetMessageId eq '<uuid>'` resolve
a note's uuid back to a Graph message id, the way `wire::resolve_message_id_
by_uuid` (src-tauri/src/backend/microsoft/wire.rs) assumes?

The M4 design spec (docs/superpowers/specs/2026-08-16-microsoft-vertical-
m4-design.md) names this as the ONE open mechanism question left before the
Pin sidecar ships: `put_sidecar`/`remove_sidecar` only ever hold a note's
uuid, not its Graph message id (unlike `save_note_full`, which gets an
`existing_remote_id` from the caller), so this lookup is new. Everything else
about M4 (the named property itself) was already probed live in
`scripts/ms_named_property_probe.py`.

This script is deliberately independent of that decision: it creates a note,
reads back the message id AND the internetMessageId Graph actually stored,
then tries the resolve two ways — WITH the RFC 5322 angle brackets Jodd's own
uuid has had stripped, and WITHOUT — and reports which one (if either) finds
exactly one match. `wire.rs`'s `resolve_by_uuid_url` currently assumes WITH
brackets; if this probe finds otherwise, fix that function before trusting
`put_sidecar`/`remove_sidecar` against a real account.

Also exercises the two things Component C's doc comment flags as open
alongside the bracket question: whether `$filter` needs any escaping beyond
plain OData string quoting, and whether `$top=2` genuinely comes back with
exactly one row (not zero, not more) for an ordinary note.

    python3 scripts/ms_get_token.py --print-token
    MS_TOKEN='<paste>' python3 scripts/ms_pin_sidecar_live_probe.py

Creates ONE throwaway note, filed directly in the Notes root (same
prerequisite as ms_named_property_probe.py: at least one note must already
exist there so the root's id is discoverable — gotcha #12). Deletes it again
at the end; note delete is proven hard-but-clean (M2), so nothing is left
behind on success. On failure the note's id is printed for manual cleanup.
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
    print("  MS_TOKEN='<paste>' python3 scripts/ms_pin_sidecar_live_probe.py")
    raise SystemExit(1)

BASE = "https://graph.microsoft.com/v1.0/"
CLASS_PROP = "String 0x001A"
PARENT_DISPLAY_PROP = "String 0x0E05"
STICKY = "IPM.StickyNote"
ROOT_NAME = "Notes"

# Must match `wire::JODD_PIN_PROP` exactly, so this probe exercises the same
# property the shipped code writes — not a stand-in.
JODD_PIN_PROP = "String {98d27db2-72ad-4511-a3b5-b73c7c42694b} Name JoddPin"


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


def resolve_url(raw_filter_value):
    """Mirrors `wire::resolve_by_uuid_url` exactly, parameterized on what
    goes inside the quotes so both the bracketed and unbracketed forms can be
    tried without duplicating the query shape."""
    return (
        "me/messages?$top=2&$select=id"
        f"&$filter=internetMessageId%20eq%20'{urllib.parse.quote(raw_filter_value, safe='')}'"
    )


print("=" * 72)
print("M4 Phase 1 probe — resolving a note's uuid to a Graph message id")
print("=" * 72)

# ── [0] locate the Notes root (same prerequisite as ms_named_property_probe) ─
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
    props = m.get("singleValueExtendedProperties") or []
    name = next((p.get("value") for p in props if p.get("id") == PARENT_DISPLAY_PROP), None)
    if name and m.get("parentFolderId"):
        folders.setdefault(name, m["parentFolderId"])

if ROOT_NAME not in folders:
    print(f"    {ROOT_NAME!r} is NOT among {sorted(folders)}")
    print("    File one note directly in the top-level Notes folder and re-run.")
    raise SystemExit(3)

ROOT = folders[ROOT_NAME]
print(f"    {ROOT_NAME}: {ROOT[:40]}…")

# ── [1] create a throwaway note ───────────────────────────────────────────
print("\n[1] Creating a throwaway note")
payload = {
    "subject": "JODD-M4-RESOLVE-PROBE",
    "body": {"contentType": "HTML",
             "content": "<html><body>JODD-M4-RESOLVE-PROBE"
                        "<div>probing internetMessageId $filter resolution</div></body></html>"},
    "singleValueExtendedProperties": [{"id": CLASS_PROP, "value": STICKY}],
}
st, item = graph("POST", q(f"me/mailFolders/{ROOT}/messages"), payload)
print(f"    create -> {st}" + ("" if st == 201 else f"  {err(item)}"))
if st != 201 or not item.get("id"):
    print("    Creation failed outright — stopping.")
    raise SystemExit(4)
msg_id = item["id"]
raw_imid = item.get("internetMessageId", "")
print(f"    message id:        {msg_id[:50]}…")
print(f"    internetMessageId: {raw_imid!r}")

cleanup_cmd = (
    f"MS_TOKEN=... python3 -c \"import urllib.request; "
    f"req=urllib.request.Request('{BASE}me/messages/{msg_id}', "
    f"headers={{'Authorization':'Bearer '+__import__('os').environ['MS_TOKEN']}}, "
    f"method='DELETE'); urllib.request.urlopen(req)\""
)

if not raw_imid:
    print("\n    Graph returned no internetMessageId at all on creation — refetching.")
    st, item = graph("GET", q(f"me/messages/{msg_id}?$select=id,internetMessageId"))
    raw_imid = item.get("internetMessageId", "")
    print(f"    internetMessageId (refetched): {raw_imid!r}")
if not raw_imid:
    print("    Still empty. This note carries no internetMessageId at all — cannot")
    print(f"    proceed. Clean up manually:\n    {cleanup_cmd}")
    raise SystemExit(5)

# `strip_message_id_brackets` in wire.rs: exactly one layer of RFC 5322 angle
# brackets stripped from each end. This IS what wire.rs computes as the uuid.
uuid = raw_imid.strip()
if uuid.startswith("<"):
    uuid = uuid[1:]
if uuid.endswith(">"):
    uuid = uuid[:-1]
print(f"    Jodd's uuid (brackets stripped): {uuid!r}")

# ── [2] resolve WITH brackets — what wire::resolve_by_uuid_url sends today ──
print("\n[2] Resolving via $filter with the uuid WRAPPED BACK in angle brackets")
bracketed = f"<{uuid}>"
st, body = graph("GET", q(resolve_url(bracketed)))
ids_bracketed = [m.get("id") for m in body.get("value", [])] if st == 200 else []
print(f"    status {st}, {len(ids_bracketed)} match(es): {ids_bracketed}")
bracketed_works = st == 200 and ids_bracketed == [msg_id]
if bracketed_works:
    print("    MATCH, exactly one — this is what wire::resolve_by_uuid_url does today.")
elif st == 200 and len(ids_bracketed) > 1:
    print("    MORE THAN ONE MATCH — unexpected, the uuid==internetMessageId identity")
    print("    assumption for this backend does not hold here.")
elif st == 200:
    print("    ZERO matches with brackets.")
else:
    print(f"    request failed: {err(body)}")

# ── [3] resolve WITHOUT brackets, for contrast ────────────────────────────
print("\n[3] Resolving via $filter with the BARE uuid (no brackets), for contrast")
st, body = graph("GET", q(resolve_url(uuid)))
ids_bare = [m.get("id") for m in body.get("value", [])] if st == 200 else []
print(f"    status {st}, {len(ids_bare)} match(es): {ids_bare}")
bare_works = st == 200 and ids_bare == [msg_id]
if bare_works:
    print("    MATCH, exactly one — Graph matched the bare uuid.")
elif st == 200 and len(ids_bare) > 1:
    print("    MORE THAN ONE MATCH — unexpected.")
elif st == 200:
    print("    ZERO matches without brackets.")
else:
    print(f"    request failed: {err(body)}")

# ── [4] if resolution worked, exercise the actual pin write through it ────
resolved_id = msg_id if bracketed_works else (msg_id if bare_works else None)
if resolved_id:
    print("\n[4] Resolution works — writing the pin property through the resolved id")
    pin_true = json.dumps({"pinned": True})
    st, _ = graph("PATCH", q(f"me/messages/{q(resolved_id)}"), {
        "singleValueExtendedProperties": [{"id": JODD_PIN_PROP, "value": pin_true}],
    })
    print(f"    PATCH pin:true -> {st}")
    read_q = (f"me/messages/{q(resolved_id)}?$select=id"
              f"&$expand=singleValueExtendedProperties($filter=id eq '{JODD_PIN_PROP}')")
    st, item2 = graph("GET", q(read_q))
    props = item2.get("singleValueExtendedProperties") or []
    val = next((p.get("value") for p in props if p.get("id") == JODD_PIN_PROP), None)
    print(f"    read back: {val!r}")
    print("    MATCH — the pin write path works end to end." if val == pin_true
          else "    MISMATCH — investigate before trusting put_pin live.")
else:
    print("\n[4] SKIPPED — neither bracketed nor bare resolution found exactly one")
    print("    match, so there is no id to write the pin property through.")

# ── cleanup ─────────────────────────────────────────────────────────────
print("\n[5] Cleaning up the throwaway note")
st, _ = graph("DELETE", q(f"me/messages/{msg_id}"))
print(f"    delete -> {st}" + ("  (clean)" if st in (200, 204) else f"  — clean up manually:\n    {cleanup_cmd}"))

print("\n" + "=" * 72)
print("VERDICT for wire::resolve_by_uuid_url:")
if bracketed_works and not bare_works:
    print("  Bracketed form is correct — matches what the code sends today. No change needed.")
elif bare_works and not bracketed_works:
    print("  BARE form is correct, bracketed is NOT — wire::resolve_by_uuid_url must drop")
    print("  the '<' '>' wrapping before this ships against a real account.")
elif bracketed_works and bare_works:
    print("  BOTH forms matched — Graph's $filter is lenient here either way.")
else:
    print("  NEITHER form found exactly one match — do not trust put_pin/remove_pin")
    print("  against a real account until this is understood.")
print("=" * 72)
