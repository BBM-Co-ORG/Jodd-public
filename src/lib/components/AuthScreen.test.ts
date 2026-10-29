// @vitest-environment jsdom
//
// The sign-in wait gives up after a deadline and says "Sign-in did not
// complete". A sign-in that finishes AFTER that deadline still lands (the
// backend emits `oauth-success`, App.svelte flips isAuthenticated and
// unmounts this screen), so the message must not follow the user into the
// signed-in app. Measured on a Galaxy S23 FE, 2026-10-29: the browser opened
// at 00:20:06, consent came back at 00:22:33, and the 2-minute message stayed
// on the notes list after a successful sign-in.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { isAuthenticated, error } from '../stores/notes';
import { signInBlocked } from '../stores/ui';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(() => Promise.resolve(null)) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
  emit: () => Promise.resolve(),
}));
vi.mock('../stores/platform', () => ({
  isAndroid: { subscribe: (f: (v: boolean) => void) => { f(true); return () => {}; } },
  isMacos: { subscribe: (f: (v: boolean) => void) => { f(false); return () => {}; } },
  supportsIcloud: { subscribe: (f: (v: boolean) => void) => { f(true); return () => {}; } },
}));

import AuthScreen, { SIGN_IN_WAIT_MS } from './AuthScreen.svelte';

const TIMED_OUT = /Sign-in did not complete/;

let target: HTMLElement;
let component: ReturnType<typeof mount> | null = null;

async function settle() {
  for (let i = 0; i < 8; i++) { await tick(); await Promise.resolve(); }
  flushSync();
}

function googleButton() {
  return target.querySelector<HTMLButtonElement>('.google-btn')!;
}

/** Click Google and let the browser "open"; the poll is now running. */
async function startGoogleSignIn() {
  component = mount(AuthScreen, { target });
  flushSync();
  await settle();
  googleButton().click();
  await settle();
}

/** Run the 2-second poll past `ms`, letting each tick's IPC settle. */
async function advance(ms: number) {
  for (let t = 0; t < ms; t += 2000) {
    await vi.advanceTimersByTimeAsync(2000);
  }
  await settle();
}

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'get_oauth_config': return Promise.resolve({ credentials_available: true });
      case 'get_auth_url': return Promise.resolve('https://accounts.example/auth');
      case 'open_auth_url': return Promise.resolve();
      case 'is_authenticated': return Promise.resolve(false);
      default: return Promise.resolve([]);
    }
  });
  isAuthenticated.set(false);
  error.set(null);
  signInBlocked.set(null);
  target = document.createElement('div');
  document.body.append(target);
});

afterEach(() => {
  if (component) unmount(component);
  component = null;
  target.remove();
  vi.useRealTimers();
});

describe('AuthScreen sign-in wait', () => {
  it('still waits at 2 min 27 s, the sign-in measured on the S23', async () => {
    await startGoogleSignIn();
    await advance(147_000);
    expect(get(error)).toBeNull();
    expect(googleButton().textContent).toMatch(/Waiting for Google/);
  });

  it('gives up after the deadline and says so', async () => {
    await startGoogleSignIn();
    await advance(SIGN_IN_WAIT_MS + 2000);
    expect(get(error)).toMatch(TIMED_OUT);
    expect(googleButton().textContent).toMatch(/Sign in with Google/);
  });

  it('a sign-in that lands after the deadline clears the timeout message', async () => {
    await startGoogleSignIn();
    await advance(SIGN_IN_WAIT_MS + 2000);
    expect(get(error)).toMatch(TIMED_OUT);

    // What App.svelte does on `oauth-success`: flip the flag, unmount us.
    isAuthenticated.set(true);
    unmount(component!);
    component = null;

    expect(get(error)).toBeNull();
  });

  it('leaves any other error in place when the screen goes away', async () => {
    await startGoogleSignIn();
    await advance(SIGN_IN_WAIT_MS + 2000);
    // Something else reports a problem after the timeout message.
    error.set('Gmail refused the request.');

    isAuthenticated.set(true);
    unmount(component!);
    component = null;

    expect(get(error)).toBe('Gmail refused the request.');
  });

  it('keeps the timeout message if the screen goes away without a sign-in', async () => {
    await startGoogleSignIn();
    await advance(SIGN_IN_WAIT_MS + 2000);

    unmount(component!);
    component = null;

    expect(get(error)).toMatch(TIMED_OUT);
  });
});
