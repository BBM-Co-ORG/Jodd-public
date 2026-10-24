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
import { studioRequest, PLAN_DEBOUNCE_MS, type StudioPlan, type PlanNote } from '../studio';
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
  generate = async () => ({ uuid: 'new-1', label: LABEL });
  invoke.mockImplementation((cmd: string, args: Record<string, unknown>) => {
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
});
