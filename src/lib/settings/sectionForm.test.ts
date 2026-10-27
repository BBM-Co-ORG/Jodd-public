import { describe, it, expect } from 'vitest';
import { saveAll, type SectionForm } from './sectionForm';

function fake(dirty: boolean, fail?: string) {
  const calls: string[] = [];
  const form: SectionForm = {
    isDirty: () => dirty,
    save: async () => {
      calls.push('save');
      if (fail) throw new Error(fail);
    },
  };
  return { form, calls };
}

describe('saveAll', () => {
  it('saves only dirty forms, in order', async () => {
    const order: number[] = [];
    const forms: SectionForm[] = [0, 1, 2].map((i) => ({
      isDirty: () => i !== 1,
      save: async () => { order.push(i); },
    }));
    expect(await saveAll(forms)).toEqual({ ok: true });
    expect(order).toEqual([0, 2]);
  });

  it('stops at first failure and reports its index and message', async () => {
    const a = fake(true);
    const b = fake(true, 'Key rejected.');
    const c = fake(true);
    expect(await saveAll([a.form, b.form, c.form])).toEqual({ ok: false, index: 1, message: 'Key rejected.' });
    expect(a.calls).toEqual(['save']);
    expect(c.calls).toEqual([]);
  });

  it('reports a non-Error throw as text', async () => {
    const form: SectionForm = { isDirty: () => true, save: async () => { throw 'plain'; } };
    expect(await saveAll([form])).toEqual({ ok: false, index: 0, message: 'plain' });
  });

  it('is ok with nothing dirty', async () => {
    expect(await saveAll([fake(false).form])).toEqual({ ok: true });
  });
});
