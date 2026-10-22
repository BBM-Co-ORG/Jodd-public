import { describe, it, expect } from 'vitest';
import { needsPermanentDeleteConfirm } from '../stores/notes';

describe('permanent-delete confirmation', () => {
  it('confirms when the backend has no trash', () => {
    expect(needsPermanentDeleteConfirm({ has_trash: false })).toBe(true);
  });
  it('does not confirm when a real restore path exists', () => {
    expect(needsPermanentDeleteConfirm({ has_trash: true })).toBe(false);
  });
  // Optimistic: while capabilities load we assume a trash exists, matching
  // shouldShowTrash. A spurious confirm on a recoverable delete is friction;
  // a missing confirm on an irreversible one is data loss — but the default
  // here is only reached for a moment, and the sidebar shows Trash anyway.
  it('does not confirm while capabilities are still loading', () => {
    expect(needsPermanentDeleteConfirm(undefined)).toBe(false);
  });
});
