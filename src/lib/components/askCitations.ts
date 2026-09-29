export type CitedNote = {
  uuid: string;
  account_id: string;
  title: string;
  slug: string;
};

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/**
 * Render an Ask Jodd answer as HTML with [[slug]] citations replaced by
 * clickable chips.
 *
 * Everything is escaped first: the answer is derived from note bodies, which
 * are user content, and the backend already stripped citations it could not
 * resolve — so anything left that looks like a citation but isn't in `cited`
 * stays plain text rather than becoming a chip that goes nowhere.
 */
export function renderAnswer(markdown: string, cited: CitedNote[]): string {
  const bySlug = new Map(cited.map((c) => [c.slug, c]));
  const escaped = escapeHtml(markdown);

  return escaped.replace(/\[\[([^\[\]\n]+)\]\]/g, (whole, slug: string) => {
    const note = bySlug.get(slug.trim());
    if (!note) return slug;
    return (
      `<button class="cite-chip" data-uuid="${escapeHtml(note.uuid)}" ` +
      `data-account="${escapeHtml(note.account_id)}">${escapeHtml(note.title)}</button>`
    );
  });
}
