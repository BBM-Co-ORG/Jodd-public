// The live strip on a view note (spec 2026-11-19-properties-tasks-views §8.5):
// while the user edits a view's property block, `preview_view` runs the draft
// body (never writing) and the strip says what it would match.
//
// Two rules, both from the local-first doctrine: a debounce so a burst of
// typing is one query, and **latest request wins** — an answer that arrives
// after a newer request was made, or after the user left the note, is dropped
// instead of painted over the strip.
import { invoke } from '@tauri-apps/api/core';
import { viewErrorsFrom, type ViewError, type ViewResult } from './viewsStore';

export const PREVIEW_DELAY_MS = 300;

/** What the strip shows. `idle` = nothing yet (no strip). */
export type PreviewState =
  | { kind: 'idle' }
  | { kind: 'ok'; summary: string; notices: string[] }
  | { kind: 'error'; errors: string[] };

/** Detected the way `Db::list_views` does: the last segment of the label. */
export function isViewsLabel(label: string | null | undefined): boolean {
  if (!label) return false;
  const leaf = label.split('/').filter(Boolean).pop();
  return leaf === '__Views__';
}

/** `ตรง 12 โน้ต` for a notes view, `ตรง 12 งาน` for a tasks view. */
export function previewSummary(result: ViewResult): string {
  return 'Notes' in result.rows
    ? `ตรง ${result.rows.Notes.length} โน้ต`
    : `ตรง ${result.rows.Tasks.length} งาน`;
}

/** One error as a line of text; a lined error says which line (1-based). */
export function errorLine(e: ViewError): string {
  return e.line >= 0 ? `${e.line + 1}: ${e.message}` : e.message;
}

export interface PreviewRequest {
  accountId: string;
  label: string | null | undefined;
  title: string;
  /** Read when the timer fires, so it is the body as of then. */
  body: () => string;
}

export interface ViewPreview {
  /** Ask for a preview after the debounce. Not a view note: no call, strip cleared. */
  schedule(req: PreviewRequest): void;
  /** Drop any pending or in-flight request (note switch, unmount). */
  cancel(): void;
}

export function createViewPreview(
  onState: (s: PreviewState) => void,
  delayMs: number = PREVIEW_DELAY_MS,
): ViewPreview {
  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const clearTimer = () => {
    if (timer !== null) { clearTimeout(timer); timer = null; }
  };

  async function run(token: number, req: PreviewRequest) {
    let state: PreviewState;
    try {
      const result = await invoke<ViewResult>('preview_view', {
        accountId: req.accountId, title: req.title, bodyHtml: req.body(),
      });
      state = { kind: 'ok', summary: previewSummary(result), notices: result.notices ?? [] };
    } catch (e) {
      state = { kind: 'error', errors: viewErrorsFrom(e).map(errorLine) };
    }
    if (token === seq) onState(state);
  }

  return {
    schedule(req) {
      clearTimer();
      const token = ++seq;
      if (!isViewsLabel(req.label)) { onState({ kind: 'idle' }); return; }
      timer = setTimeout(() => { timer = null; void run(token, req); }, delayMs);
    },
    cancel() {
      clearTimer();
      seq++;
    },
  };
}
