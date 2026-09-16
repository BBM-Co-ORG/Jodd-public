import { describe, it, expect } from 'vitest';
import { shouldRenderExternalBody, nextExternalChangeBanner, pushedBodyAfterConfirmation } from './editorRenderDecision';

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

describe('nextExternalChangeBanner', () => {
  // The exact scenario measured live on 2026-08-27 11:22: an edit was made in
  // Apple Notes while the same note was open and being typed in here, and
  // `reconcile_one` logged `CONFLICT on uuid=4de5ef24 — saved local content as
  // duplicate uuid=0fd58145`. `shouldRenderExternalBody` had correctly refused
  // to clobber the typing; nothing had told the user why that mattered.
  it('raises when a remote body arrives that we refused to render', () => {
    expect(
      nextExternalChangeBanner({
        shown: false,
        uuidChanged: false,
        willRender: false, // shouldRenderExternalBody said unsafe — user typing
        bodyChanged: true,
        isEcho: false,
      }),
    ).toBe(true);
  });

  // The reactive block re-runs on every keystroke, and the next keystroke
  // syncs the store to the DOM — so `bodyChanged` falls back to false while
  // the situation it reported is still entirely unresolved.
  it('stays up once raised, even after bodyChanged settles back to false', () => {
    expect(
      nextExternalChangeBanner({
        shown: true,
        uuidChanged: false,
        willRender: false,
        bodyChanged: false,
        isEcho: false,
      }),
    ).toBe(true);
  });

  it('clears when the content is finally rendered — including a reload the user asked for', () => {
    expect(
      nextExternalChangeBanner({
        shown: true,
        uuidChanged: false,
        willRender: true,
        bodyChanged: true,
        isEcho: false,
      }),
    ).toBe(false);
  });

  it('clears on switching to another note', () => {
    expect(
      nextExternalChangeBanner({
        shown: true,
        uuidChanged: true,
        willRender: true,
        bodyChanged: true,
        isEcho: false,
      }),
    ).toBe(false);
  });

  // Our own push is a remote change as far as the backend's change detector is
  // concerned. Without the echo term the banner accuses the user of conflicting
  // with themselves every time a save comes back around while they keep typing.
  it('does not raise on our own write echoing back', () => {
    expect(
      nextExternalChangeBanner({
        shown: false,
        uuidChanged: false,
        willRender: false,
        bodyChanged: true,
        isEcho: true,
      }),
    ).toBe(false);
  });

  it('stays down when nothing arrived', () => {
    expect(
      nextExternalChangeBanner({
        shown: false,
        uuidChanged: false,
        willRender: false,
        bodyChanged: false,
        isEcho: false,
      }),
    ).toBe(false);
  });
});

// ─── Push confirmation ──────────────────────────────────────────────────
// `lastPushedBody` is what `isEcho` compares an incoming body against, so it
// must mean "a body the REMOTE is known to hold". It used to be set from
// `save_note`'s return — but `save_note` is local-first (it writes SQLite and
// returns; the worker pushes up to 5s later), so between a local save and its
// push the value named a body the remote had never seen. Any refresh landing
// in that window delivered the PREVIOUS body, which is then neither the
// rendered one nor the "pushed" one — a false "Edited on another device".
describe('pushedBodyAfterConfirmation', () => {
  const base = {
    editorUuid: 'note-1',
    editorAccountId: 'gmail:a@b.com',
    currentPushedBody: '<p>B</p>',
  };

  it('advances to the confirmed body when the worker pushed the open note', () => {
    expect(
      pushedBodyAfterConfirmation({
        ...base,
        confirmation: { accountId: 'gmail:a@b.com', uuid: 'note-1', bodyHtml: '<p>A</p>' },
      }),
    ).toBe('<p>A</p>');
  });

  it('leaves it alone when no confirmation has arrived', () => {
    expect(pushedBodyAfterConfirmation({ ...base, confirmation: null })).toBe('<p>B</p>');
  });

  it('leaves it alone for a different note', () => {
    expect(
      pushedBodyAfterConfirmation({
        ...base,
        confirmation: { accountId: 'gmail:a@b.com', uuid: 'other-note', bodyHtml: '<p>X</p>' },
      }),
    ).toBe('<p>B</p>');
  });

  // uuid is only unique WITHIN an account — the notes PK is (uuid, account_id)
  // and `markRecentlySaved` is scoped per-account for the same reason.
  it('leaves it alone for the same uuid on a different account', () => {
    expect(
      pushedBodyAfterConfirmation({
        ...base,
        confirmation: { accountId: 'gmail:other@b.com', uuid: 'note-1', bodyHtml: '<p>X</p>' },
      }),
    ).toBe('<p>B</p>');
  });
});

// The end-to-end shape of the bug, at the level where it was decided.
//
// Measured on gmail:kaiwan@bbmedia.co.th, 2026-08-30: one note, two Gmail
// messages 87s apart (insert-new + trash-old), editor showing "Saved" AND
// "Edited on another device" with nothing but this device ever touching it.
describe('a refresh landing between a local save and its push', () => {
  it('is our own sync lag, not another device', () => {
    // Note opened: the backend held A, and that is what rendered.
    const bodyOnBackend: string = '<p>A</p>';
    // The user then edited. `save_note` committed B to SQLite and returned —
    // the worker has NOT pushed it yet, so the backend still holds A.
    const bodySavedLocally: string = '<p>B</p>';
    // App.svelte's 10s focus settle fires a scoped read of the backend, which
    // truthfully answers A. The editor is focused, so the render is refused.
    const incoming = bodyOnBackend;

    const common = {
      shown: false,
      uuidChanged: false,
      willRender: false, // cursor in the editor → render refused
      bodyChanged: incoming !== bodySavedLocally,
    };

    // Before: `isEcho` compared against the LOCAL save, which the backend had
    // never seen — so its own content read back as somebody else's edit.
    expect(nextExternalChangeBanner({ ...common, isEcho: incoming === bodySavedLocally })).toBe(true);

    // After: the anchor is what the worker confirmed the backend holds.
    expect(nextExternalChangeBanner({ ...common, isEcho: incoming === bodyOnBackend })).toBe(false);
  });

  it('still reports a genuine edit made somewhere else', () => {
    const bodyOnBackend: string = '<p>A</p>';
    const bodyFromTheIphone: string = '<p>typed on the iPhone</p>';
    expect(
      nextExternalChangeBanner({
        shown: false,
        uuidChanged: false,
        willRender: false,
        bodyChanged: true,
        isEcho: bodyFromTheIphone === bodyOnBackend,
      }),
    ).toBe(true);
  });
});
