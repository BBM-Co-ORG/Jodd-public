import { describe, it, expect, vi, beforeEach } from 'vitest';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { deleteTag, DELETE_TAG_CONFIRM } from './tagActions';

describe('deleteTag', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue([]);
  });

  // Final review 2026-10-20: a destructive confirm is never a primary "OK".
  it('asks with a danger-styled "Delete" button', async () => {
    const ask = vi.fn().mockResolvedValue(false);
    await deleteTag('gmail:a@b.com', 'work', ask);
    expect(DELETE_TAG_CONFIRM).toEqual({ destructive: true, confirmLabel: 'Delete' });
    expect(ask).toHaveBeenCalledWith('Delete tag?', expect.stringContaining('#work'), DELETE_TAG_CONFIRM);
    expect(invoke).not.toHaveBeenCalledWith('delete_tag', expect.anything());
  });
});
