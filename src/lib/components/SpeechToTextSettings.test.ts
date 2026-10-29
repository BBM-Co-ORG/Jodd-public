// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, tick, unmount } from 'svelte';
import SpeechToTextSettings from './SpeechToTextSettings.svelte';

const CONFIRM_COPY = 'Audio from video links will be sent to api.groq.com to turn speech into text.';

type Cfg = { preset: string; base_url: string; model: string; language: string | null; max_upload_mb: number; confirmed_host: string | null };
const EMPTY: Cfg = { preset: 'custom', base_url: '', model: '', language: null, max_upload_mb: 25, confirmed_host: null };

let stored: { cfg: Cfg; has_api_key: boolean | null };
let testReply: () => Promise<string>;
const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
  switch (cmd) {
    case 'get_stt_config': return stored;
    case 'set_stt_config': {
      const a = args as { cfg: Cfg; apiKey: string | null; confirm: string | null };
      // What the backend does: confirmed_host comes only from `confirm`, and
      // only when it is the origin of the base URL being saved.
      const origin = (u: string) => new URL(u).protocol + '//' + new URL(u).host;
      if (a.confirm !== null && a.confirm !== origin(a.cfg.base_url)) throw 'the address changed — save again to confirm it';
      stored = {
        cfg: { ...a.cfg, confirmed_host: a.confirm !== null ? a.confirm : stored.cfg.confirmed_host },
        has_api_key: a.apiKey === '' ? false : a.apiKey ? true : stored.has_api_key,
      };
      return null;
    }
    case 'test_stt': return testReply();
    default: throw new Error(`Unexpected command ${cmd}`);
  }
});
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: [string, Record<string, unknown>?]) => invoke(...a) }));

let component: ReturnType<typeof mount> | undefined;
let target: HTMLElement;
async function settle() { for (let i = 0; i < 12; i++) await tick(); flushSync(); }
async function render() {
  target = document.createElement('div');
  document.body.append(target);
  component = mount(SpeechToTextSettings, { target });
  await settle();
}
const btn = (label: string) => [...target.querySelectorAll('button')].find((b) => b.textContent?.trim() === label) as HTMLButtonElement | undefined;
async function click(label: string) { btn(label)!.click(); await settle(); }
// The Settings page's one Save (the section has none of its own): the
// section-form save(). Its failure is shown once, by the page above the
// section, so the tests read it from the rejection (`lastSave`).
let lastSave: Promise<void> = Promise.resolve();
async function save() {
  lastSave = (component as unknown as { save(): Promise<void> }).save();
  lastSave.catch(() => {});
  await settle();
}
function field(label: string): HTMLInputElement {
  return [...target.querySelectorAll('label')].find((l) => l.textContent?.trim().startsWith(label))!.querySelector('input')!;
}
async function type(label: string, value: string) {
  const el = field(label);
  el.value = value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  await settle();
}
async function pick(value: string) {
  const sel = target.querySelector('select')!;
  sel.value = value;
  sel.dispatchEvent(new Event('change', { bubbles: true }));
  await settle();
}
const setCalls = () => invoke.mock.calls.filter((c) => c[0] === 'set_stt_config');

beforeEach(() => {
  stored = { cfg: { ...EMPTY }, has_api_key: null };
  testReply = () => Promise.resolve('สวัสดีครับ this is a test');
});
afterEach(async () => {
  if (component) await unmount(component);
  component = undefined;
  document.body.innerHTML = '';
  invoke.mockClear();
});

describe('SpeechToTextSettings', () => {
  it('saving_shows_the_host_and_requires_confirmation', async () => {
    await render();
    await pick('groq');
    expect(field('Base URL').value).toBe('https://api.groq.com/openai/v1');
    expect(field('Model').value).toBe('whisper-large-v3');

    await save();
    // Asked, not saved: the exact copy, the host, and nothing written yet.
    expect(target.textContent).toContain(CONFIRM_COPY);
    expect(setCalls()).toHaveLength(0);

    await click('Confirm');
    expect(setCalls()).toHaveLength(1);
    expect(setCalls()[0][1]).toMatchObject({ confirm: 'https://api.groq.com', cfg: { preset: 'groq', base_url: 'https://api.groq.com/openai/v1' } });
    expect(target.textContent).not.toContain(CONFIRM_COPY);
    expect(target.textContent).toContain('Audio from video links is sent to api.groq.com.');
  });

  it('cancelling the confirmation saves nothing', async () => {
    await render();
    await pick('groq');
    await save();
    await click('Cancel');
    expect(setCalls()).toHaveLength(0);
    expect(target.textContent).not.toContain(CONFIRM_COPY);
    expect(target.textContent).toContain('Not set up');
  });

  it('the openai and groq rows say they are not yet tested with Jodd', async () => {
    await render();
    const options = [...target.querySelectorAll('option')].map((o) => o.textContent);
    expect(options.filter((t) => t?.includes('not yet tested with Jodd'))).toHaveLength(2);
    await pick('openai');
    expect(field('Base URL').value).toBe('https://api.openai.com/v1');
    expect(field('Model').value).toBe('whisper-1');
    expect(target.querySelector('.untested')?.textContent).toContain('not yet tested with Jodd');
    await pick('custom');
    expect(target.querySelector('.untested')).toBeNull();
  });

  it('a changed host asks again; a changed model on the same host does not', async () => {
    stored = { cfg: { ...EMPTY, preset: 'groq', base_url: 'https://api.groq.com/openai/v1', model: 'whisper-large-v3', confirmed_host: 'https://api.groq.com' }, has_api_key: true };
    await render();
    await type('Model', 'whisper-large-v3-turbo');
    await save();
    expect(setCalls()).toHaveLength(1);
    expect(setCalls()[0][1]).toMatchObject({ confirm: null });

    await type('Base URL', 'https://elsewhere.example/v1');
    await save();
    expect(setCalls()).toHaveLength(1);
    expect(target.textContent).toContain('Audio from video links will be sent to elsewhere.example to turn speech into text.');
  });

  it('a changed port or scheme asks again, and the copy shows the port and an unencrypted scheme', async () => {
    stored = { cfg: { ...EMPTY, base_url: 'http://192.168.1.5:8000/v1', model: 'm', confirmed_host: 'http://192.168.1.5:8000' }, has_api_key: false };
    await render();
    expect(target.textContent).toContain('Audio from video links is sent to 192.168.1.5:8000 (not encrypted).');
    expect(target.textContent).toContain('Test sends a 3-second Thai and English clip bundled with Jodd to 192.168.1.5:8000 (not encrypted).');
    await type('Base URL', 'http://192.168.1.5:9000/v1');
    await save();
    expect(setCalls()).toHaveLength(0);
    expect(target.textContent).toContain('Audio from video links will be sent to 192.168.1.5:9000 (not encrypted) to turn speech into text.');
    await click('Cancel');
    await type('Base URL', 'https://192.168.1.5:8000/v1');
    await save();
    expect(setCalls()).toHaveLength(0);
    expect(target.textContent).toContain('sent to 192.168.1.5:8000 to turn speech into text.');
  });

  it('editing the address while the confirmation is open cannot confirm an address that was not shown', async () => {
    await render();
    await pick('groq');
    await save();
    expect(target.textContent).toContain(CONFIRM_COPY);
    // Every input is locked while the panel is open...
    for (const label of ['Base URL', 'Model', 'Language', 'New API key']) expect(field(label).disabled).toBe(true);
    expect(target.querySelector('select')!.disabled).toBe(true);
    // ...and even an edit that gets through (bypassing the disabled attribute)
    // is not what Confirm sends: the config and origin are the ones shown.
    const url = field('Base URL');
    url.value = 'https://other.example/v1';
    url.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    await click('Confirm');
    expect(setCalls()).toHaveLength(1);
    expect(setCalls()[0][1]).toMatchObject({ confirm: 'https://api.groq.com', cfg: { base_url: 'https://api.groq.com/openai/v1' } });
    expect(stored.cfg.confirmed_host).toBe('https://api.groq.com');
  });

  it('shows the backend refusal when the address no longer matches, and confirms nothing', async () => {
    await render();
    await pick('groq');
    await save();
    invoke.mockImplementationOnce(async () => { throw 'the address changed — save again to confirm it'; });
    await click('Confirm');
    await expect(lastSave).rejects.toThrow('save: the address changed — save again to confirm it');
    // One surface per failure: the page's error line, not this section too.
    expect(target.textContent).not.toContain('the address changed');
    expect(stored.cfg.confirmed_host).toBeNull();
    expect(target.textContent).not.toContain(CONFIRM_COPY);
  });

  it('refuses a base URL that is not http(s) without asking', async () => {
    await render();
    await type('Base URL', 'ftp://files.example');
    await save();
    await expect(lastSave).rejects.toThrow('The base URL must start with http:// or https://');
    expect(target.textContent).not.toContain(CONFIRM_COPY);
    expect(setCalls()).toHaveLength(0);
  });

  it('Test is enabled only for the saved, confirmed settings, and shows the transcript', async () => {
    await render();
    expect(btn('Test')!.disabled).toBe(true);
    await pick('groq');
    await save();
    await click('Confirm');
    expect(btn('Test')!.disabled).toBe(false);

    await type('Model', 'something-else');
    expect(btn('Test')!.disabled).toBe(true);
    expect(target.textContent).toContain('Unsaved changes');
    await type('Model', 'whisper-large-v3');
    expect(btn('Test')!.disabled).toBe(false);

    await click('Test');
    expect(invoke.mock.calls.map((c) => c[0])).toContain('test_stt');
    expect(target.textContent).toContain('สวัสดีครับ this is a test');
  });

  it('shows the failure a Test came back with', async () => {
    stored = { cfg: { ...EMPTY, preset: 'groq', base_url: 'https://api.groq.com/openai/v1', model: 'm', confirmed_host: 'https://api.groq.com' }, has_api_key: true };
    testReply = () => Promise.reject('speech-to-text rejected the key');
    await render();
    await click('Test');
    expect(target.querySelector('.test-result.bad')?.textContent).toContain('speech-to-text rejected the key');
  });

  it('sends a typed key once and never keeps it', async () => {
    await render();
    await pick('groq');
    await type('New API key', 'sekret');
    await save();
    await click('Confirm');
    expect(setCalls()[0][1]).toMatchObject({ apiKey: 'sekret' });
    expect(field('New API key').value).toBe('');
    expect(target.textContent).toContain('Previously saved');
  });

  it('loads without touching anything that reads the credential store', async () => {
    await render();
    expect(invoke.mock.calls.map((c) => c[0])).toEqual(['get_stt_config']);
  });
});

describe('SpeechToTextSettings — section-form contract (isDirty / save)', () => {
  it('isDirty() follows the edits', async () => {
    await render();
    const form = component as unknown as { isDirty(): boolean };
    expect(form.isDirty()).toBe(false);
    await type('Model', 'whisper-1');
    expect(form.isDirty()).toBe(true);
  });

  it('save() resolves only after the host is confirmed', async () => {
    await render();
    await pick('groq');
    const form = component as unknown as { save(): Promise<void> };
    let done = false;
    const p = form.save().then(() => { done = true; });
    await settle();
    expect(done).toBe(false);
    expect(target.textContent).toContain(CONFIRM_COPY);
    expect(setCalls()).toHaveLength(0);
    await click('Confirm');
    await p;
    expect(done).toBe(true);
    expect(setCalls()).toHaveLength(1);
  });

  it('save() throws "Speech-to-text host not confirmed." when the user cancels', async () => {
    await render();
    await pick('groq');
    const form = component as unknown as { save(): Promise<void> };
    const p = form.save();
    const settled = p.then(() => 'resolved', (e: Error) => e.message);
    await settle();
    await click('Cancel');
    expect(await settled).toBe('Speech-to-text host not confirmed.');
    expect(setCalls()).toHaveLength(0);
    expect(target.textContent).not.toContain(CONFIRM_COPY);
  });

  it('save() throws the shown message when the write fails, and for a bad URL', async () => {
    await render();
    await type('Base URL', 'ftp://files.example');
    const form = component as unknown as { save(): Promise<void> };
    await expect(form.save()).rejects.toThrow('The base URL must start with http:// or https://');

    await pick('groq');
    const p = form.save();
    const settled = p.then(() => 'resolved', (e: Error) => e.message);
    await settle();
    invoke.mockImplementationOnce(async () => { throw 'the address changed — save again to confirm it'; });
    await click('Confirm');
    expect(await settled).toBe('save: the address changed — save again to confirm it');
  });
});

describe('SpeechToTextSettings — re-entrant save()', () => {
  const outcome = (p: Promise<void>) => p.then(() => 'resolved', (e: Error) => e.message);

  it('a second save() while the confirm is open shares the first: one Confirm resolves both, one write', async () => {
    await render();
    await pick('groq');
    const form = component as unknown as { save(): Promise<void> };
    const p1 = form.save();
    const p2 = form.save();
    expect(p2).toBe(p1);
    const o1 = outcome(p1); const o2 = outcome(p2);
    await settle();
    expect(target.textContent).toContain(CONFIRM_COPY); // panel survives the second call
    await click('Confirm');
    expect(await o1).toBe('resolved');
    expect(await o2).toBe('resolved');
    expect(setCalls()).toHaveLength(1);
  });

  it('twice then Cancel: both reject with the same message and the panel closes', async () => {
    await render();
    await pick('groq');
    const form = component as unknown as { save(): Promise<void> };
    const o1 = outcome(form.save());
    const o2 = outcome(form.save());
    await settle();
    await click('Cancel');
    expect(await o1).toBe('Speech-to-text host not confirmed.');
    expect(await o2).toBe('Speech-to-text host not confirmed.');
    expect(setCalls()).toHaveLength(0);
    expect(target.textContent).not.toContain(CONFIRM_COPY);
    // and a later save() starts fresh
    const o3 = outcome(form.save());
    await settle();
    expect(target.textContent).toContain(CONFIRM_COPY);
    await click('Confirm');
    expect(await o3).toBe('resolved');
  });
});
