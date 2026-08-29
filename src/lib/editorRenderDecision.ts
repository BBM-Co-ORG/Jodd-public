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

// The other half of the same policy. `shouldRenderExternalBody` returning
// false is not "nothing happened" — it is "something arrived and we refused to
// apply it, because applying it would throw away what the user is typing".
// That refusal is correct and was silent, so the user kept typing on content
// the server had already moved past and found out only when the reconciler
// minted a conflict copy behind their back.
//
// So: when the decision above says SAFE, the fresh content lands on its own,
// just sooner than the ten-minute poll used to manage. When it says UNSAFE,
// this says so on screen instead.
export interface ExternalChangeBannerArgs {
  // Whether the banner is currently up. The reactive block re-runs on every
  // keystroke, and `bodyChanged` goes false again the moment the next
  // keystroke syncs the store — so the raise condition is a momentary edge and
  // the banner has to be latched, not recomputed from scratch each pass.
  shown: boolean;
  uuidChanged: boolean;
  // The render this pass will actually perform (including a reload the user
  // asked for), not merely what `shouldRenderExternalBody` advises.
  willRender: boolean;
  bodyChanged: boolean;
  // The incoming body is one WE pushed, coming back around. Our own write is
  // a remote change like any other as far as the backend's change detector is
  // concerned, so without this the banner accuses the user of a conflict with
  // themselves.
  //
  // Known limit, accepted rather than papered over: this compares bytes, and a
  // backend that normalises what it stores (iCloud re-encodes through Apple's
  // own document model) can hand back a body that is ours in substance and not
  // in bytes. That only matters in the narrow window where a save has landed
  // AND the user is still typing — the banner is then wrong, and the cost of
  // it being wrong is one dismissible line offering to load content the user
  // already has.
  isEcho: boolean;
}

export function nextExternalChangeBanner(args: ExternalChangeBannerArgs): boolean {
  const { shown, uuidChanged, willRender, bodyChanged, isEcho } = args;
  // Both of these RESOLVE the situation the banner reports: a render puts the
  // remote content on screen, and switching notes makes it somebody else's
  // problem. Clearing structurally — at the two events that resolve it —
  // rather than by re-deriving the raise condition is what keeps the latch
  // from getting stuck up.
  if (uuidChanged || willRender) return false;
  if (bodyChanged && !isEcho) return true;
  return shown;
}
