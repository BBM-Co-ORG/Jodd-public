// @vitest-environment jsdom
//
// The app-level error surface.
//
// Two defects this pins, both live on 2026-09-20 behind one screenshot
// ("Folder not found: __TRASH__" sitting under an unrelated folder):
//
//  1. PLACEMENT. The bar used to live inside NoteEditor.svelte, which
//     App.svelte swaps out for TrashPreview whenever '__TRASH__' is
//     selected. So an error raised while the user was in Recently Deleted
//     rendered nowhere at all, then appeared the moment they opened a note
//     in some other folder — looking like that folder had failed. A global
//     error store needs a surface that does not depend on which pane won.
//
//  2. LIFETIME. Nothing cleared it. The only `error.set(null)` outside
//     AuthScreen was NoteEditor's save path, so a one-off failure stayed on
//     screen until the user happened to save a note or sign in again.
//
// The fix is deliberately NOT "clear it on navigation": this same store
// carries `handleAuthLoss`'s "Signed out — Keychain credentials were
// removed. Please sign in again." (App.svelte), which is an account state
// the user must act on, not a transient failure. Auto-expiring the bar
// would hide it. Dismissal is the user's call instead.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import { get } from 'svelte/store';
import { error } from '../stores/notes';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import ErrorBar from './ErrorBar.svelte';

function render() {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const host = mount(ErrorBar, { target });
  flushSync();
  return { target, host };
}

describe('ErrorBar', () => {
  let host: ReturnType<typeof mount> | null = null;

  beforeEach(() => {
    document.body.innerHTML = '';
    error.set(null);
  });

  afterEach(() => {
    if (host) unmount(host);
    host = null;
    error.set(null);
  });

  it('shows nothing while there is no error', () => {
    const r = render();
    host = r.host;
    expect(r.target.querySelector('.error-bar')).toBeNull();
  });

  it('shows the message once an error is set', () => {
    const r = render();
    host = r.host;

    error.set('Folder not found: __TRASH__');
    flushSync();

    const bar = r.target.querySelector('.error-bar');
    expect(bar).not.toBeNull();
    expect(bar!.textContent).toContain('Folder not found: __TRASH__');
  });

  // The stuck-banner half. Before the dismiss existed the only way out was
  // saving a note or signing in again.
  it('clears the store and hides itself when dismissed', () => {
    const r = render();
    host = r.host;

    error.set('Failed to move note: nope');
    flushSync();

    const dismiss = r.target.querySelector<HTMLButtonElement>('button[aria-label="Dismiss error"]');
    expect(dismiss).not.toBeNull();

    dismiss!.click();
    flushSync();

    expect(get(error)).toBeNull();
    expect(r.target.querySelector('.error-bar')).toBeNull();
  });

  // Placement half: the bar must not depend on a note being open or on
  // which pane App.svelte chose. Mounting it standalone, with no editor and
  // no selected note anywhere, is exactly the Recently Deleted case.
  it('renders with no note open and no editor mounted', () => {
    const r = render();
    host = r.host;

    error.set('Folder not found: __TRASH__');
    flushSync();

    expect(r.target.querySelector('.error-bar')!.textContent).toContain('Folder not found');
  });
});
