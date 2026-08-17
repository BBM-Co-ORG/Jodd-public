import { describe, it, expect } from 'vitest';
import { canWrite, canWriteAccount } from './notes';

describe('per-area write gate (canWrite)', () => {
  const allWritable = { has_trash: true, writes: { notes: true, folders: true, sidecars: true } };
  const allRefused = {
    has_trash: false,
    writes: { notes: false, folders: false, sidecars: false },
  };
  // Live-verified Microsoft shape (2026-08-15 M2 closeout): notes writable,
  // folders and sidecars not. Folders looked done after Task 11 (notes AND
  // folders on) but live testing against a real account found Graph-created
  // folders never reach Apple Notes — a measured negative, not caution — so
  // this flipped back off. See `Capabilities::for_backend`'s Microsoft arm
  // (src-tauri/src/backend/mod.rs) for the mechanism.
  const notesOnly = {
    has_trash: false,
    writes: { notes: true, folders: false, sidecars: false },
  };

  it('hides write affordances for a kind the backend refuses', () => {
    expect(canWrite(allRefused, 'notes')).toBe(false);
    expect(canWrite(allRefused, 'folders')).toBe(false);
    expect(canWrite(allRefused, 'sidecars')).toBe(false);
  });

  it('allows writes for a kind the backend accepts', () => {
    expect(canWrite(allWritable, 'notes')).toBe(true);
    expect(canWrite(allWritable, 'folders')).toBe(true);
    expect(canWrite(allWritable, 'sidecars')).toBe(true);
  });

  it('lets each area diverge, the whole point of the split (the M2 shape)', () => {
    expect(canWrite(notesOnly, 'notes')).toBe(true);
    expect(canWrite(notesOnly, 'folders')).toBe(false);
    expect(canWrite(notesOnly, 'sidecars')).toBe(false);
  });

  // The actual post-M4 Microsoft shape (2026-08-16): notes and sidecars
  // (pin) writable, folders still not — a measured negative that stays
  // permanent (Graph-created folders never reach Apple Notes), unlike
  // sidecars, which were only ever "not built yet". A New Folder button
  // rendering and then failing server-side is exactly the frontend/backend
  // divergence this split exists to prevent.
  it('keeps folders hidden on Microsoft while notes and pin stay open', () => {
    const ms = { has_trash: false, writes: { notes: true, folders: false, sidecars: true } };
    expect(canWrite(ms, 'notes')).toBe(true);
    expect(canWrite(ms, 'folders')).toBe(false);
    expect(canWrite(ms, 'sidecars')).toBe(true);
  });

  // Optimistic while loading, for the same no-flicker reason as
  // shouldShowTrash — and safe only because refuse_write (lib.rs) refuses
  // the command independently of this.
  it('assumes writable while capabilities are still loading', () => {
    expect(canWrite(undefined, 'notes')).toBe(true);
    expect(canWrite(undefined, 'folders')).toBe(true);
    expect(canWrite(undefined, 'sidecars')).toBe(true);
  });

  // A capabilities object cached before `writes` existed (the old `can_write`
  // shape, or any object with no `writes` field) must not read as refused and
  // lock a Gmail user out of their own editor.
  it('treats a capabilities object with no writes field as writable, for every kind', () => {
    const legacy = { has_trash: true } as const;
    expect(canWrite(legacy, 'notes')).toBe(true);
    expect(canWrite(legacy, 'folders')).toBe(true);
    expect(canWrite(legacy, 'sidecars')).toBe(true);
  });
});

describe('canWriteAccount (whole-account gate for not-yet-split components)', () => {
  it('mirrors canWrite for notes, the area it proxies', () => {
    expect(canWriteAccount({ has_trash: false, writes: { notes: false, folders: true, sidecars: true } })).toBe(false);
    expect(canWriteAccount({ has_trash: true, writes: { notes: true, folders: false, sidecars: false } })).toBe(true);
  });

  it('assumes writable while capabilities are still loading', () => {
    expect(canWriteAccount(undefined)).toBe(true);
  });

  it('treats a capabilities object with no writes field as writable', () => {
    expect(canWriteAccount({ has_trash: true })).toBe(true);
  });
});
