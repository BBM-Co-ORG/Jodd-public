/** Latest refresh intent, serialized behind local saves and the current read. */
export function createRefreshQueue(blocked: () => boolean, onError: (error: unknown) => void) {
  let pending: (() => Promise<void>) | null = null;
  let running = false;
  let disposed = false;
  function drain() {
    if (disposed || blocked() || running || !pending) return;
    const next = pending;
    pending = null;
    running = true;
    void next().catch(onError).finally(() => { running = false; drain(); });
  }
  return {
    get running() { return running; },
    schedule(next: () => Promise<void>) { if (!disposed) { pending = next; drain(); } },
    drain,
    dispose() { disposed = true; pending = null; },
  };
}
