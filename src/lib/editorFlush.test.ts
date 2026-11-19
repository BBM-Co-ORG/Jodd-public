import { describe, it, expect, vi } from 'vitest';
import { flushEditors, registerEditorFlush } from './editorFlush';

describe('editorFlush', () => {
  it('flushEditors runs every registered flush and rejects if one does', async () => {
    const a = vi.fn().mockResolvedValue(undefined);
    const b = vi.fn().mockRejectedValue(new Error('disk full'));
    const offA = registerEditorFlush(a);
    const offB = registerEditorFlush(b);
    await expect(flushEditors()).rejects.toThrow('disk full');
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);
    offA();
    offB();
  });
  it('unregistering removes it', async () => {
    const a = vi.fn().mockResolvedValue(undefined);
    registerEditorFlush(a)();
    await flushEditors();
    expect(a).not.toHaveBeenCalled();
  });
  it('nothing registered is nothing to wait for', async () => {
    await expect(flushEditors()).resolves.toBeUndefined();
  });
});
