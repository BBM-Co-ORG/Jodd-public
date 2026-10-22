// A note's wiki-link slug, mirroring Rust's `db::note_slug`: the title
// slugified (letters, digits, `_` and combining marks kept, every other run
// collapsed to one `-`), then `-` and the first 8 hex digits of the uuid.
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

export function noteSlug(title: string, uuid: string): string {
  const id = (uuid.match(/[0-9a-fA-F]/g) || []).join('').slice(0, 8).toLowerCase();
  const base = slugify(title);
  return base ? `${base}-${id}` : id;
}
