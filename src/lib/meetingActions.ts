export interface ActionItemsPreview {
  ai_result_id: string;
  title: string;
  body_html: string;
  before_html: string | null;
}

/** Local passage links work inside a contenteditable note too. No body mutation. */
export function followMeetingEvidence(event: MouseEvent, container: HTMLElement): boolean {
  const anchor = (event.target as Element | null)?.closest('a');
  const href = anchor?.getAttribute('href');
  if (!href?.startsWith('#meeting-')) return false;
  event.preventDefault();
  const passage = Array.from(container.querySelectorAll('[id]')).find(el => el.id === href.slice(1));
  passage?.scrollIntoView({ block: 'nearest' });
  return true;
}
