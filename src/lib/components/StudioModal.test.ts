// @vitest-environment jsdom
//
// Research Studio's dialog (spec 2026-10-23 §3), mounted: the plan it shows,
// the ticks it sends, the run with its progress, cancel and the consent retry.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';

vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false), supportsIcloud: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

const invoke = vi.fn();
const channels: { onmessage: (m: unknown) => void }[] = [];
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class {
    onmessage: (m: unknown) => void = () => {};
    constructor() {
      channels.push(this);
    }
  },
}));
const openCreatedNote = vi.fn(async (..._a: unknown[]) => undefined);
vi.mock('../openCreatedNote', () => ({ openCreatedNote: (...a: unknown[]) => openCreatedNote(...a) }));
vi.mock('../unreviewed', () => ({ refreshUnreviewed: vi.fn(async () => {}) }));

import StudioModal from './StudioModal.svelte';
import { studioRequest, PLAN_DEBOUNCE_MS, type StudioPlan, type PlanNote, type RelatedNote, type StudioRelated } from '../studio';
import { AI_CONSENT_NEEDED, consentRequest, answerConsent } from '../aiConsent';

const ACCT = 'gmail:a@b.com';
const LABEL = 'Notes/Research';

const NOTES: PlanNote[] = [
  { uuid: 'u1', title: 'First source', label: LABEL, date_ms: 3, chars: 100, is_studio: false },
  { uuid: 'u2', title: 'Second source', label: `${LABEL}/Papers`, date_ms: 2, chars: 200, is_studio: false },
  { uuid: 'u3', title: 'Briefing: Research', label: LABEL, date_ms: 1, chars: 300, is_studio: true },
];

function planFor(args: { selected: string[] | null }, over: Partial<StudioPlan> = {}): StudioPlan {
  return {
    notes: NOTES,
    not_shown: 0,
    selected: args.selected ?? ['u1', 'u2'],
    estimate: { calls: 1, bytes: 1000, map_reduce: false, refusal: null },
    ...over,
  };
}

let planOverride: Partial<StudioPlan> = {};
let related: (args: Record<string, unknown>) => Promise<StudioRelated> = async () => ({ notes: [], capped: false });
let generate: (args: Record<string, unknown>) => Promise<unknown> = async () => ({ uuid: 'new-1', label: LABEL });

function calls(cmd: string): Record<string, unknown>[] {
  return invoke.mock.calls.filter((c) => c[0] === cmd).map((c) => c[1] as Record<string, unknown>);
}

async function settle() {
  for (let i = 0; i < 20; i++) {
    await Promise.resolve();
    flushSync();
  }
  await tick();
  flushSync();
}

let host: HTMLElement;
let component: ReturnType<typeof mount>;

async function open() {
  host = document.createElement('div');
  document.body.appendChild(host);
  component = mount(StudioModal, { target: host });
  studioRequest.set({ accountId: ACCT, label: LABEL });
  flushSync();
  await settle();
}

function checkbox(uuid: string): HTMLInputElement {
  return document.querySelector(`input[type="checkbox"][value="${uuid}"]`) as HTMLInputElement;
}
function generateButton(): HTMLButtonElement {
  return Array.from(document.querySelectorAll('button')).find((b) => b.textContent?.trim().startsWith('Generat')) as HTMLButtonElement;
}
function estimate(): string {
  return document.querySelector('.studio-estimate')?.textContent?.trim() ?? '';
}

beforeEach(() => {
  invoke.mockReset();
  openCreatedNote.mockClear();
  channels.length = 0;
  planOverride = {};
  related = async () => ({ notes: [], capped: false });
  generate = async () => ({ uuid: 'new-1', label: LABEL });
  invoke.mockImplementation((cmd: string, args: Record<string, unknown>) => {
    if (cmd === 'studio_related') return related(args);
    if (cmd === 'studio_plan') return Promise.resolve(planFor(args as { selected: string[] | null }, planOverride));
    if (cmd === 'generate_studio_note') return generate(args);
    if (cmd === 'cancel_extraction') return Promise.resolve(true);
    if (cmd === 'allow_ai_for_account') return Promise.resolve();
    return Promise.resolve([]);
  });
});

afterEach(() => {
  answerConsent(false);
  studioRequest.set(null);
  flushSync();
  if (component) unmount(component);
  host?.remove();
});

describe('StudioModal', () => {
  it('loads the plan for the requested folder and shows the estimate', async () => {
    await open();
    expect(calls('studio_plan')[0]).toEqual({ accountId: ACCT, label: LABEL, output: 'briefing', focus: '', selected: null });
    expect(estimate()).toBe('About 1 AI call.');
    expect(document.body.textContent).toContain('2 of 3 notes');
    const briefing = document.querySelector('input[type="radio"][value="briefing"]') as HTMLInputElement;
    expect(briefing.checked).toBe(true);
    const focus = document.querySelector('input.studio-focus') as HTMLInputElement;
    expect(focus.placeholder).toBe('Optional — what to focus on');
    expect(focus.maxLength).toBe(200);
  });

  it('leaves earlier Studio notes unticked', async () => {
    await open();
    expect(checkbox('u1').checked).toBe(true);
    expect(checkbox('u2').checked).toBe(true);
    expect(checkbox('u3').checked).toBe(false);
    expect(checkbox('u3').closest('li')?.querySelector('.studio-chip')?.textContent).toBe('Studio');
  });

  it('says how many older notes are not shown', async () => {
    planOverride = { not_shown: 7 };
    await open();
    expect(document.body.textContent).toContain('7 older notes not shown.');
  });

  it('re-plans with the new selection when a note is unticked', async () => {
    await open();
    checkbox('u2').click();
    flushSync();
    expect(checkbox('u2').checked).toBe(false);
    await new Promise((r) => setTimeout(r, PLAN_DEBOUNCE_MS + 50));
    await settle();
    const plans = calls('studio_plan');
    expect(plans).toHaveLength(2);
    expect(plans[1]).toEqual({ accountId: ACCT, label: LABEL, output: 'briefing', focus: '', selected: ['u1'] });
    expect(document.body.textContent).toContain('1 of 3 notes');
  });

  it('an_empty_plan_disables_generate', async () => {
    planOverride = { notes: [], selected: [], estimate: { calls: 0, bytes: 0, map_reduce: false, refusal: 'Tick at least one note.' } };
    await open();
    expect(estimate()).toBe('No notes here that AI can read.');
    expect(generateButton().disabled).toBe(true);
  });

  it('a refusal disables generate and shows its text', async () => {
    const refusal = 'These notes need 12 AI calls; the limit is 8.';
    planOverride = { estimate: { calls: 12, bytes: 1000, map_reduce: true, refusal } };
    await open();
    expect(estimate()).toBe(refusal);
    expect(generateButton().disabled).toBe(true);
  });

  it('generate sends the ticked uuids and opens the created note', async () => {
    await open();
    generateButton().click();
    await settle();
    const [args] = calls('generate_studio_note');
    expect(args).toMatchObject({ accountId: ACCT, label: LABEL, output: 'briefing', focus: '', uuids: ['u1', 'u2'] });
    expect(typeof args.requestId).toBe('string');
    expect(args.onProgress).toBe(channels[0]);
    expect(openCreatedNote).toHaveBeenCalledWith(ACCT, { uuid: 'new-1', label: LABEL });
    // The dialog closed and the request was consumed.
    expect(get(studioRequest)).toBeNull();
    expect(document.querySelector('.studio-estimate')).toBeNull();
  });

  // Spec 2026-10-26 §4: two captioned groups, one radio group, one `output`.
  it('shows the outputs in two groups and sends the chosen deliverable', async () => {
    await open();
    const groups = Array.from(document.querySelectorAll('.studio-output-group'));
    expect(groups.map((g) => g.querySelector('.studio-output-group-label')?.textContent)).toEqual([
      'Understand the notes',
      'Draft a deliverable',
    ]);
    const values = (g: Element) => Array.from(g.querySelectorAll('input[type="radio"]')).map((r) => (r as HTMLInputElement).value);
    expect(values(groups[0])).toEqual(['briefing', 'faq', 'timeline', 'study_guide']);
    expect(values(groups[1])).toEqual(['slide_deck', 'course_module', 'podcast_script', 'video_script']);
    expect(new Set(Array.from(document.querySelectorAll('input[type="radio"]')).map((r) => (r as HTMLInputElement).name))).toEqual(
      new Set(['studio-output']),
    );

    (document.querySelector('input[type="radio"][value="podcast_script"]') as HTMLInputElement).click();
    flushSync();
    await new Promise((r) => setTimeout(r, PLAN_DEBOUNCE_MS + 20));
    await settle();
    expect(calls('studio_plan').at(-1)).toMatchObject({ output: 'podcast_script' });
    generateButton().click();
    await settle();
    expect(calls('generate_studio_note')[0]).toMatchObject({ output: 'podcast_script' });
  });

  it('shows progress sent through the Channel', async () => {
    generate = () => new Promise(() => {});
    await open();
    generateButton().click();
    await settle();
    channels[0].onmessage({ stage: 'reading', total: 2 });
    channels[0].onmessage({ stage: 'writing' });
    await settle();
    const items = Array.from(document.querySelectorAll('ol.ingest-progress li')).map((li) => li.textContent);
    expect(items).toEqual(['Reading 2 notes…', 'Writing…']);
    expect(document.querySelector('ol.ingest-progress')?.getAttribute('aria-live')).toBe('polite');
  });

  it('cancel during a run calls cancel_extraction with the same requestId', async () => {
    let reject: (e: unknown) => void = () => {};
    generate = () => new Promise((_, rej) => { reject = rej; });
    await open();
    generateButton().click();
    await settle();
    expect(generateButton().disabled).toBe(true);
    const cancel = Array.from(document.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Cancel') as HTMLButtonElement;
    cancel.click();
    await settle();
    const [gen] = calls('generate_studio_note');
    expect(calls('cancel_extraction')).toEqual([{ requestId: gen.requestId }]);
    reject('cancelled');
    await settle();
    // Cancelled shows nothing and keeps the dialog for another try.
    expect(document.querySelector('.error')).toBeNull();
    expect(generateButton().disabled).toBe(false);
    expect(openCreatedNote).not.toHaveBeenCalled();
  });

  it('shows any other error inline', async () => {
    generate = async () => { throw 'The AI provider returned an error.'; };
    await open();
    generateButton().click();
    await settle();
    expect(document.querySelector('.error')?.textContent).toBe('The AI provider returned an error.');
    expect(openCreatedNote).not.toHaveBeenCalled();
  });

  it('a consent refusal offers consent and retries once', async () => {
    let attempt = 0;
    generate = async () => {
      attempt++;
      if (attempt === 1) throw `provider not configured: ${AI_CONSENT_NEEDED}`;
      return { uuid: 'new-2', label: LABEL };
    };
    await open();
    generateButton().click();
    await settle();
    expect(get(consentRequest)?.accountId).toBe(ACCT);
    answerConsent(true);
    await settle();
    expect(calls('allow_ai_for_account')).toEqual([{ accountId: ACCT }]);
    expect(calls('generate_studio_note')).toHaveLength(2);
    expect(openCreatedNote).toHaveBeenCalledWith(ACCT, { uuid: 'new-2', label: LABEL });
  });

  // Stage 2a (spec 2026-10-25 §3): notes from other folders, suggested unticked.
  describe('From other folders', () => {
    const X1: RelatedNote = {
      uuid: 'x1', title: 'Cell chemistry', label: 'Notes/Chemistry', date_ms: 5, chars: 400,
      linked_with: 2, tags: ['ev', 'battery'], terms: ['battery'],
    };
    const X2: RelatedNote = {
      uuid: 'x2', title: 'Charging survey', label: 'Notes/Inbox', date_ms: 4, chars: 300,
      linked_with: 0, tags: [], terms: ['charging', 'survey', 'grid'],
    };
    const X3: RelatedNote = {
      uuid: 'x3', title: 'Grid storage', label: 'Notes/Energy', date_ms: 3, chars: 200,
      linked_with: 1, tags: [], terms: [],
    };

    function section(): HTMLElement | null {
      return document.querySelector('section[aria-label="From other folders"]');
    }
    function rowTitles(): string[] {
      return Array.from(section()?.querySelectorAll('.studio-note-title') ?? []).map((e) => e.textContent?.trim() ?? '');
    }
    async function debounce() {
      await new Promise((r) => setTimeout(r, PLAN_DEBOUNCE_MS + 50));
      await settle();
    }
    async function typeFocus(text: string) {
      const input = document.querySelector('input.studio-focus') as HTMLInputElement;
      input.value = text;
      input.dispatchEvent(new Event('input', { bubbles: true }));
      flushSync();
    }

    it('shows suggestions unticked under From other folders', async () => {
      let resolve: (r: StudioRelated) => void = () => {};
      related = () => new Promise((r) => { resolve = r; });
      await open();
      // Asked after the plan, for the same folder and Focus.
      expect(calls('studio_related')).toEqual([{ accountId: ACCT, label: LABEL, focus: '' }]);
      expect(section()?.textContent).toContain('From other folders');
      expect(section()?.textContent).toContain('Looking for related notes…');
      resolve({ notes: [X1, X2], capped: false });
      await settle();
      expect(section()?.textContent).not.toContain('Looking for related notes…');
      expect(section()?.querySelector('h3')?.textContent).toContain('From other folders');
      expect(rowTitles()).toEqual(['Cell chemistry', 'Charging survey']);
      expect(checkbox('x1').checked).toBe(false);
      expect(checkbox('x2').checked).toBe(false);
      const chips = (uuid: string) =>
        Array.from(checkbox(uuid).closest('li')?.querySelectorAll('.studio-chip') ?? []).map((c) => c.textContent);
      expect(chips('x1')).toEqual(['Linked with 2 notes', '#ev #battery', 'Mentions “battery”']);
      expect(chips('x2')).toEqual(['Mentions “charging”, “survey”']);
      expect(checkbox('x1').closest('li')?.textContent).toContain('Chemistry');
      // Folder counts are folder notes only.
      expect(document.body.textContent).toContain('2 of 3 notes');
      expect(document.body.textContent).not.toContain('Showing the 20 closest.');
    });

    it('is absent when nothing is related', async () => {
      await open();
      expect(calls('studio_related')).toHaveLength(1);
      expect(section()).toBeNull();
      expect(document.body.textContent).not.toContain('From other folders');
    });

    it('a ticked suggestion is sent to studio_plan and generate_studio_note', async () => {
      related = async () => ({ notes: [X1, X2], capped: false });
      await open();
      checkbox('x2').click();
      flushSync();
      expect(checkbox('x2').checked).toBe(true);
      // The folder default is kept: ticking a suggestion adds to it.
      expect(checkbox('u1').checked).toBe(true);
      expect(checkbox('u2').checked).toBe(true);
      expect(checkbox('u3').checked).toBe(false);
      await debounce();
      expect(calls('studio_plan').at(-1)).toEqual({ accountId: ACCT, label: LABEL, output: 'briefing', focus: '', selected: ['u1', 'u2', 'x2'] });
      // A tick does not re-ask for suggestions.
      expect(calls('studio_related')).toHaveLength(1);
      generateButton().click();
      await settle();
      expect(calls('generate_studio_note')[0]).toMatchObject({ uuids: ['u1', 'u2', 'x2'] });
    });

    it('toggling a folder note keeps a ticked suggestion', async () => {
      related = async () => ({ notes: [X1, X2], capped: false });
      await open();
      checkbox('x1').click();
      flushSync();
      checkbox('u2').click();
      flushSync();
      expect(checkbox('x1').checked).toBe(true);
      expect(checkbox('u2').checked).toBe(false);
      checkbox('u3').click();
      flushSync();
      await debounce();
      // Folder notes in plan order, then suggestions in suggestion order.
      expect(calls('studio_plan').at(-1)?.selected).toEqual(['u1', 'u3', 'x1']);
      checkbox('x2').click();
      flushSync();
      await debounce();
      expect(calls('studio_plan').at(-1)?.selected).toEqual(['u1', 'u3', 'x1', 'x2']);
    });

    it('Select all ticks folder notes only', async () => {
      related = async () => ({ notes: [X1, X2], capped: false });
      await open();
      checkbox('x2').click();
      flushSync();
      const toggleAll = () => (document.querySelector('.studio-toggle-all') as HTMLButtonElement).click();
      toggleAll();
      flushSync();
      expect(['u1', 'u2', 'u3'].map((u) => checkbox(u).checked)).toEqual([true, true, true]);
      expect(checkbox('x1').checked).toBe(false);
      expect(checkbox('x2').checked).toBe(true);
      expect(document.querySelector('.studio-toggle-all')?.textContent?.trim()).toBe('Select none');
      toggleAll();
      flushSync();
      expect(['u1', 'u2', 'u3'].map((u) => checkbox(u).checked)).toEqual([false, false, false]);
      expect(checkbox('x2').checked).toBe(true);
      await debounce();
      expect(calls('studio_plan').at(-1)?.selected).toEqual(['x2']);
    });

    it('a Focus change re-requests suggestions and keeps a ticked one shown', async () => {
      related = async (args) => (args.focus === '' ? { notes: [X1, X2], capped: false } : { notes: [X3, X1], capped: false });
      await open();
      checkbox('x2').click();
      flushSync();
      await typeFocus('grid storage');
      // Debounced: nothing asked yet.
      expect(calls('studio_related')).toHaveLength(1);
      await debounce();
      const asks = calls('studio_related');
      expect(asks).toHaveLength(2);
      expect(asks[1]).toEqual({ accountId: ACCT, label: LABEL, focus: 'grid storage' });
      // x2 dropped out of the answer but was ticked: still shown, still ticked, still sent.
      expect(rowTitles()).toEqual(['Grid storage', 'Cell chemistry', 'Charging survey']);
      expect(section()?.querySelector('h3')?.textContent?.trim()).toBe('From other folders (3)');
      expect(checkbox('x2').checked).toBe(true);
      expect(checkbox('x3').checked).toBe(false);
      expect(calls('studio_plan').at(-1)?.selected).toEqual(['u1', 'u2', 'x2']);
      generateButton().click();
      await settle();
      expect(calls('generate_studio_note')[0]).toMatchObject({ uuids: ['u1', 'u2', 'x2'] });
    });

    it('a Focus edited and reverted inside the debounce does not re-ask', async () => {
      related = async () => ({ notes: [X1], capped: false });
      await open();
      expect(calls('studio_related')).toHaveLength(1);
      await typeFocus('grid');
      await typeFocus('');
      await debounce();
      expect(calls('studio_related')).toHaveLength(1);
      expect(rowTitles()).toEqual(['Cell chemistry']);
    });

    it('a failing first request leaves the section out and the dialog working', async () => {
      const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
      related = async () => { throw new Error('boom'); };
      await open();
      expect(calls('studio_related')).toHaveLength(1);
      expect(section()).toBeNull();
      expect(document.body.textContent).not.toContain('Looking for related notes…');
      // The plan still works: untick, re-plan, generate.
      checkbox('u2').click();
      flushSync();
      await debounce();
      expect(calls('studio_plan').at(-1)?.selected).toEqual(['u1']);
      expect(generateButton().disabled).toBe(false);
      generateButton().click();
      await settle();
      expect(calls('generate_studio_note')[0]).toMatchObject({ uuids: ['u1'] });
      warn.mockRestore();
    });

    it('a failing Focus reload keeps the shown rows and the tick', async () => {
      const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
      related = async (args) => {
        if (args.focus === '') return { notes: [X1, X2], capped: false };
        throw new Error('boom');
      };
      await open();
      expect(section()?.querySelector('h3')?.textContent?.trim()).toBe('From other folders (2)');
      checkbox('x2').click();
      flushSync();
      await typeFocus('grid');
      await debounce();
      expect(calls('studio_related')).toHaveLength(2);
      expect(rowTitles()).toEqual(['Cell chemistry', 'Charging survey']);
      expect(section()?.querySelector('h3')?.textContent?.trim()).toBe('From other folders (2)');
      expect(section()?.textContent).not.toContain('Looking for related notes…');
      expect(checkbox('x2').checked).toBe(true);
      expect(calls('studio_plan').at(-1)?.selected).toEqual(['u1', 'u2', 'x2']);
      generateButton().click();
      await settle();
      expect(calls('generate_studio_note')[0]).toMatchObject({ uuids: ['u1', 'u2', 'x2'] });
      warn.mockRestore();
    });

    it('a dropped suggestion that was not ticked goes', async () => {
      related = async (args) => (args.focus === '' ? { notes: [X1, X2], capped: false } : { notes: [X3], capped: false });
      await open();
      await typeFocus('grid');
      await debounce();
      expect(rowTitles()).toEqual(['Grid storage']);
    });

    it('a late answer for an older request is dropped', async () => {
      const pending: ((r: StudioRelated) => void)[] = [];
      related = () => new Promise((r) => { pending.push(r); });
      await open();
      await typeFocus('grid');
      await debounce();
      expect(pending).toHaveLength(2);
      pending[1]({ notes: [X3], capped: false });
      await settle();
      pending[0]({ notes: [X1, X2], capped: false });
      await settle();
      expect(rowTitles()).toEqual(['Grid storage']);
      expect(checkbox('x1')).toBeNull();
    });

    it('a late answer after reopening is dropped', async () => {
      const pending: ((r: StudioRelated) => void)[] = [];
      related = () => new Promise((r) => { pending.push(r); });
      await open();
      studioRequest.set(null);
      flushSync();
      studioRequest.set({ accountId: ACCT, label: LABEL });
      flushSync();
      await settle();
      expect(pending).toHaveLength(2);
      pending[0]({ notes: [X1], capped: false });
      await settle();
      expect(section()?.textContent).toContain('Looking for related notes…');
      expect(checkbox('x1')).toBeNull();
      pending[1]({ notes: [X2], capped: false });
      await settle();
      expect(rowTitles()).toEqual(['Charging survey']);
    });

    it('capped shows Showing the 20 closest.', async () => {
      related = async () => ({ notes: [X1], capped: true });
      await open();
      expect(section()?.textContent).toContain('Showing the 20 closest.');
    });

    it('Generate needs a ticked folder note', async () => {
      related = async () => ({ notes: [X1], capped: false });
      await open();
      (document.querySelector('.studio-toggle-all') as HTMLButtonElement).click(); // Select all
      flushSync();
      (document.querySelector('.studio-toggle-all') as HTMLButtonElement).click(); // Select none
      flushSync();
      checkbox('x1').click();
      flushSync();
      await debounce();
      expect(checkbox('x1').checked).toBe(true);
      expect(generateButton().disabled).toBe(true);
      checkbox('u1').click();
      flushSync();
      expect(generateButton().disabled).toBe(false);
    });
  });
});
