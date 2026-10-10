// Curate (M2) — types mirroring src-tauri/src/curate/mod.rs, and the pure
// rules the review screen follows, tested without mounting it.

export type CurateKind = 'duplicate' | 'misfiled' | 'secret';

export type CurateAction =
  | { type: 'keep'; keep: string }
  | { type: 'append'; into: string }
  | { type: 'move'; to: string }
  | { type: 'hide' };

export interface NoteRef {
  uuid: string;
  title: string;
  label: string;
  local_version: number;
  date: string;
  chars: number;
}

export interface Proposal {
  id: number;
  kind: CurateKind;
  status: 'pending' | 'stale' | 'failed' | 'applied' | 'dismissed';
  created_at: number;
  error: string | null;
  payload: { notes: NoteRef[]; action: CurateAction; reason: string; evidence: string[] };
}

export interface ScanSummary {
  duplicates: number;
  misfiled: number;
  secrets: number;
  skipped: number;
  notes: string[];
  ai?: 'consent_needed' | null;
}

export interface NoteText {
  uuid: string;
  title: string;
  label: string;
  text: string;
}

export interface Choice {
  label: string;
  action: CurateAction;
}

export function folderName(label: string): string {
  return label === 'Notes' ? 'Notes' : label.replace(/^Notes\//, '');
}

/** Every action the user may pick for a proposal; the recommended one first. */
export function choices(p: Proposal): Choice[] {
  const name = (uuid: string) => p.payload.notes.find(n => n.uuid === uuid)?.title ?? uuid;
  switch (p.kind) {
    case 'duplicate': {
      const all: Choice[] = [];
      for (const n of p.payload.notes) {
        all.push({ label: `Keep “${n.title}”, move the others to the trash`, action: { type: 'keep', keep: n.uuid } });
        all.push({ label: `Append the others into “${n.title}”`, action: { type: 'append', into: n.uuid } });
      }
      const rec = p.payload.action;
      const recLabel = rec.type === 'keep' ? `Keep “${name(rec.keep)}”, move the others to the trash`
        : rec.type === 'append' ? `Append the others into “${name(rec.into)}”` : '';
      return [...all.filter(c => c.label === recLabel), ...all.filter(c => c.label !== recLabel)];
    }
    case 'misfiled': {
      const a = p.payload.action;
      return a.type === 'move' ? [{ label: `Move to ${folderName(a.to)}`, action: a }] : [];
    }
    case 'secret':
      return [{ label: 'Move to a folder hidden from agents', action: { type: 'hide' } }];
  }
}

export function sameAction(a: CurateAction, b: CurateAction): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** What the kept note will read like after an append — the preview the
 * user approves, built the way apply builds it. */
export function appendPreview(texts: NoteText[], into: string): string {
  const kept = texts.find(t => t.uuid === into);
  if (!kept) return '';
  return [kept.text, ...texts.filter(t => t.uuid !== into).map(t => `———\nMerged from: ${t.title}\n${t.text}`)].join('\n\n');
}

export function summaryLine(s: ScanSummary): string {
  const parts = [
    s.duplicates && `${s.duplicates} duplicate group${s.duplicates === 1 ? '' : 's'}`,
    s.misfiled && `${s.misfiled} misfiled note${s.misfiled === 1 ? '' : 's'}`,
    s.secrets && `${s.secrets} note${s.secrets === 1 ? '' : 's'} with secrets`,
  ].filter(Boolean);
  const found = parts.length ? `Found ${parts.join(', ')}.` : 'Nothing new to fix.';
  const skipped = s.skipped ? ` ${s.skipped} could not be checked; try again later.` : '';
  return [found + skipped, ...s.notes].join(' ');
}
