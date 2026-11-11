// A note's wiki-link slug, mirroring Rust's `db::note_slug`: the title
// slugified (letters, digits, `_` and combining marks kept, every other run
// collapsed to one `-`), then `-` and the first 8 hex digits of the uuid.
// The id is `uuidShort`, the same rule as Rust's `db::uuid_short`: a plain
// uuid gives its first 8 hex digits; any other identity (a Microsoft
// internetMessageId, whose hex digits come from the shared server prefix)
// gives FNV-1a 32 of the whole id.
// `[[slug]]` is how the user links to THIS note. Moved verbatim from
// NoteEditor.svelte when "Copy wiki link" moved into the ⋯ menu.

function slugify(s: string): string {
  let out = '';
  let pendingDash = false;
  for (const c of s.trim()) {
    if (/[\p{L}\p{N}_\p{M}]/u.test(c)) {
      if (pendingDash && out) out += '-';
      pendingDash = false;
      out += c.toLowerCase();
    } else {
      pendingDash = true;
    }
  }
  return out;
}

const HEX = /[0-9a-fA-F]/g;

function fnv1a32(bytes: Uint8Array): number {
  let h = 0x811c9dc5;
  for (const b of bytes) {
    h ^= b;
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** Mirrors Rust `db::uuid_short` exactly; `noteSlug.test.ts` pins the parity. */
export function uuidShort(uuid: string): string {
  const hex = uuid.match(HEX) || [];
  if (/^[0-9a-fA-F-]*$/.test(uuid) && hex.length === 32) {
    return hex.join('').slice(0, 8).toLowerCase();
  }
  return fnv1a32(new TextEncoder().encode(uuid)).toString(16).padStart(8, '0');
}

export function noteSlug(title: string, uuid: string): string {
  const id = uuidShort(uuid);
  const base = slugify(title);
  return base ? `${base}-${id}` : id;
}
