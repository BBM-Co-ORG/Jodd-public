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

// A push the sync worker confirmed against the backend. Emitted by
// `sync_worker_tick` (Rust) once `mark_pushed` has cleaned the row, so the
// body it carries is one the REMOTE now holds — not one this device merely
// wrote to SQLite.
export interface PushConfirmation {
  accountId: string;
  uuid: string;
  bodyHtml: string;
}

export interface PushedBodyArgs {
  // The newest confirmation seen, or null if none has arrived yet.
  confirmation: PushConfirmation | null;
  // Identity of the note the editor is currently bound to. Both halves are
  // checked: a note's PRIMARY KEY is (uuid, account_id), so the same uuid can
  // legitimately name a different note on another account.
  editorUuid: string | undefined;
  // Nullable because the editor derives it as `note.account_id ||
  // $currentAccount`, and both halves can be null before an account loads.
  editorAccountId: string | null | undefined;
  currentPushedBody: string | undefined;
}

// What `lastPushedBody` should become given a push confirmation.
//
// Why this is not simply `saved.body_html` from the `save_note` response:
// `save_note` is local-first — it commits to SQLite and returns, and the
// worker pushes on a later tick. Setting `lastPushedBody` there named a body
// the backend had never seen, so `isEcho` compared incoming remote content
// against a value the remote could not possibly match yet. A refresh landing
// in that window (App.svelte's 10s focus/folder settle reads the backend)
// delivered the PREVIOUS body — not the rendered one, not the "pushed" one —
// and the banner blamed another device for this device's own sync lag.
export function pushedBodyAfterConfirmation(args: PushedBodyArgs): string | undefined {
  const { confirmation, editorUuid, editorAccountId, currentPushedBody } = args;
  if (!confirmation) return currentPushedBody;
  if (confirmation.uuid !== editorUuid) return currentPushedBody;
  if (confirmation.accountId !== editorAccountId) return currentPushedBody;
  return confirmation.bodyHtml;
}
