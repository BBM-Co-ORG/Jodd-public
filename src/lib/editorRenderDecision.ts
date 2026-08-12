// Pure decision logic for NoteEditor's "should I replace editorEl.innerHTML
// with the store's body_html" reactive block. Extracted so the race that
// corrupts WKWebView's native undo stack can be covered by a plain unit
// test without mounting the component.
//
// Any full innerHTML reassignment on a contenteditable element resets the
// browser's undo history. That's correct when uuidChanged (switching notes)
// but destructive when it happens mid-edit: it silently discards whatever
// the user typed since the last confirmed push AND wipes Cmd+Z history, so
// a later undo operates on stale/unrelated browser-internal state.
export interface ShouldRenderExternalBodyArgs {
  uuidChanged: boolean;
  bodyChanged: boolean;
  isSaving: boolean;
  activeIsInEditor: boolean;
  // True when the live content (title and/or body) differs from the last
  // content confirmed pushed to the backend — i.e. there is unsaved work.
  // This is a broader, more reliable signal than DOM focus: focus can be on
  // the title input (a sibling of editorEl, not covered by activeIsInEditor)
  // or transiently elsewhere (window blur/refocus) while an edit is still
  // outstanding.
  hasPendingEdit: boolean;
}

export function shouldRenderExternalBody(args: ShouldRenderExternalBodyArgs): boolean {
  const { uuidChanged, bodyChanged, isSaving, activeIsInEditor, hasPendingEdit } = args;
  return uuidChanged || (bodyChanged && !isSaving && !activeIsInEditor && !hasPendingEdit);
}
