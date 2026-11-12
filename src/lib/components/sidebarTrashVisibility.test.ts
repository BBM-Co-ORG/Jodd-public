import { describe, it, expect } from 'vitest';
import { shouldShowTrash } from '../stores/notes';

describe('Trash visibility', () => {
  it('hides Trash for a backend with no trash', () => {
    expect(shouldShowTrash({ has_trash: false })).toBe(false);
  });

  it('shows Trash for a backend that has one', () => {
    expect(shouldShowTrash({ has_trash: true })).toBe(true);
  });

  it('shows Trash while capabilities are still loading, rather than flickering it away', () => {
    expect(shouldShowTrash(undefined)).toBe(true);
  });
});
