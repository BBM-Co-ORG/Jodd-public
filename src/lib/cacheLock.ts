// The frontend half of src-tauri/src/cache_lock.rs.
//
// When the local cache cannot be opened (2026-10-26: a Keychain prompt nobody
// answered), the Rust side locks the process — no accounts, no sync, every
// command refused but `retry_cache_unlock` — and tells the page so through an
// initialization script that runs before this bundle does. main.ts reads it
// synchronously and mounts the unlock screen INSTEAD of App, so no component
// mounts, and nothing invokes, beside a throwaway cache.

export type CacheLockKind = 'keychain' | 'open_failed';

export interface CacheLock {
  kind: CacheLockKind;
  detail: string;
}

type LockWindow = { __JODD_CACHE_LOCK__?: unknown };

/** The lock the backend announced, or null when the cache opened normally. */
export function readCacheLock(w: LockWindow = window as unknown as LockWindow): CacheLock | null {
  const raw = w.__JODD_CACHE_LOCK__;
  if (raw === undefined || raw === null) return null;
  // Fail closed: anything announced but unreadable is still a lock. Mounting
  // App on a malformed value would put the real UI back beside a temp cache.
  const r = raw as Partial<CacheLock>;
  const kind: CacheLockKind = r.kind === 'keychain' ? 'keychain' : 'open_failed';
  const detail = typeof r.detail === 'string' ? r.detail : '';
  return { kind, detail };
}

export interface CacheLockCopy {
  title: string;
  body: string[];
  action: string;
}

/** What the unlock screen says. One place, so the test reads the same words. */
export function cacheLockCopy(kind: CacheLockKind): CacheLockCopy {
  if (kind === 'keychain') {
    return {
      title: "Couldn't unlock your notes",
      body: [
        "Jodd keeps its copy of your notes encrypted, and the key is in your system keychain. " +
          "Jodd asked for it but didn't get it. The keychain prompt may have timed out or been denied.",
        'Your notes and accounts are safe. Nothing has been changed or deleted, and Jodd will not ' +
          'sync or change any account until it can open your notes.',
        'Click Try again. Jodd restarts and asks for the key again. When the keychain prompt ' +
          'appears, allow it.',
      ],
      action: 'Try again',
    };
  }
  return {
    title: "Couldn't open your notes",
    body: [
      "Jodd couldn't open its local copy of your notes.",
      'Your notes and accounts are safe. Nothing has been changed or deleted, and Jodd will not ' +
        'sync or change any account until it can open your notes.',
      'Click Try again to restart Jodd. If this keeps happening, the details below say what failed.',
    ],
    action: 'Try again',
  };
}
