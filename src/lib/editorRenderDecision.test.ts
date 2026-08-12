import { describe, it, expect } from 'vitest';
import { shouldRenderExternalBody } from './editorRenderDecision';

// The formula NoteEditor.svelte used before this fix (no hasPendingEdit
// term). Kept here only to document the exact regression this test guards:
// with the old formula, editing the title (which moves DOM focus outside
// editorEl) while a body edit is still unsaved allowed a background
// poll/settle/sweep tick to clobber editorEl.innerHTML mid-edit, discarding
// the unsaved edit and resetting WKWebView's native undo stack.
function oldShouldRenderExternalBody(args: {
  uuidChanged: boolean;
  bodyChanged: boolean;
  isSaving: boolean;
  activeIsInEditor: boolean;
}): boolean {
  const { uuidChanged, bodyChanged, isSaving, activeIsInEditor } = args;
  return uuidChanged || (bodyChanged && !isSaving && !activeIsInEditor);
}

describe('shouldRenderExternalBody', () => {
  it('title focused with an unsaved body edit: old formula wrongly re-rendered, new formula must not', () => {
    // User typed body text (unsaved, autosave still debouncing), then clicked
    // into the title field to rename the note. A background refresh
    // (focus-settle / folder-settle / sweep / poll) lands here with the
    // server's stale (pre-edit) body_html.
    const args = {
      uuidChanged: false,
      bodyChanged: true, // server body_html differs from lastRenderedBody
      isSaving: false, // no save currently in flight
      activeIsInEditor: false, // focus is on the title <input>, not editorEl
      hasPendingEdit: true, // lastRenderedBody !== lastPushedBody
    };

    expect(oldShouldRenderExternalBody(args)).toBe(true); // the bug
    expect(shouldRenderExternalBody(args)).toBe(false); // the fix
  });

  it('legitimate remote edit while unfocused and nothing pending still re-renders', () => {
    expect(
      shouldRenderExternalBody({
        uuidChanged: false,
        bodyChanged: true,
        isSaving: false,
        activeIsInEditor: false,
        hasPendingEdit: false,
      }),
    ).toBe(true);
  });

  it('never re-renders while the body editor itself is focused, pending or not', () => {
    expect(
      shouldRenderExternalBody({
        uuidChanged: false,
        bodyChanged: true,
        isSaving: false,
        activeIsInEditor: true,
        hasPendingEdit: false,
      }),
    ).toBe(false);
  });

  it('always re-renders on note switch regardless of other flags', () => {
    expect(
      shouldRenderExternalBody({
        uuidChanged: true,
        bodyChanged: false,
        isSaving: true,
        activeIsInEditor: true,
        hasPendingEdit: true,
      }),
    ).toBe(true);
  });

  it('never re-renders mid-save', () => {
    expect(
      shouldRenderExternalBody({
        uuidChanged: false,
        bodyChanged: true,
        isSaving: true,
        activeIsInEditor: false,
        hasPendingEdit: false,
      }),
    ).toBe(false);
  });
});
