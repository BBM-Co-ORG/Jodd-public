// @vitest-environment jsdom
//
// Regression test for a CRITICAL finding in Task 8 review: the error-turn
// branch used to fall through `{@html t.html ?? t.content}`. An error turn
// never sets `t.html`, so that `??` spliced the raw backend error string
// straight into {@html} with none of askCitations.ts's escaping. That
// string is attacker-reachable — an upstream HTTP error page
// (src-tauri/src/llm/http.rs UpstreamError embeds the raw response body of
// whatever endpoint the user configured) or raw agent-CLI subprocess stderr
// (llm/agent_cli.rs UpstreamError) can both contain arbitrary HTML/script.
//
// Fixed by branching on `t.html` presence: the html branch always goes
// through renderAnswer() (escaped), the fallback branch is a plain `{t.content}`
// text interpolation (Svelte auto-escapes text interpolation — never {@html}).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';

// The phone gate is `$isAndroid && $androidLayoutMode === 'phone'`; both are
// plain writables here (desktop by default) that the phone tests flip.
vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

import AskJoddModal from './AskJoddModal.svelte';
import type { Writable } from 'svelte/store';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import {
  handlePopState,
  initPhoneNavHistory,
  depthFromHistoryState,
  overlayFromHistoryState,
} from '../stores/phoneNav';
import { get } from 'svelte/store';
import { accounts, currentAccount, selectedFolder, error } from '../stores/notes';
import type { Account } from '../types';
import { AI_CONSENT_NEEDED, DATA_ALLOWED_LABEL } from '../aiConsent';
import { openAskWith, pendingAskScope } from '../stores/ui';
import { noteScope } from './askScope';

const invoke = vi.fn();
const policyEvents = vi.hoisted(() => ({ changed: () => {} }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async (_: string, callback: () => void) => { policyEvents.changed = callback; return () => {}; }) }));
const SESSION = { session_id: 'session', destination: 'HTTP https://example.test/v1 · test', scope: 'Allowed accounts' };
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

// The AskAnswer the mocked `invoke` resolves with. Hand-written here, so it is
// only as good as its agreement with the Rust struct — see the shape test at
// the bottom of this file and its counterpart, `ask_wire_shapes_are_stable` in
// src-tauri/src/ask/mod.rs. Both must be updated together with the struct.
const ANSWER: Record<string, unknown> = {
  markdown: 'We chose keep-both. [[sync-conflicts-aabbccdd]]',
  cited: [
    {
      uuid: 'aabbccdd-0000-0000-0000-000000000000',
      account_id: 'a@x',
      title: 'Sync conflicts',
      slug: 'sync-conflicts-aabbccdd',
    },
  ],
  notes_in_scope: 10,
  notes_considered: 10,
  notes_used: 3,
  trimmed: false,
  dropped_citations: 0,
  conversation_id: null,
};

function render() {
  const target = document.createElement('div');
  document.body.appendChild(target);
  // `currentAccount` starts null, and the default 'account' scope is
  // account-anchored — askScope() returns null for it, so the Ask button is
  // (correctly) disabled and nothing can be sent. Every test below is about
  // what happens AFTER a question goes out, so each needs a selected account.
  currentAccount.set('a@x');
  selectedFolder.set('');
  const host = mount(AskJoddModal, { target, props: { open: true } });
  flushSync();
  return { target, host };
}

function askTurns(target: HTMLElement): HTMLElement {
  return target.querySelector('.ask-turns') as HTMLElement;
}

async function sendQuestion(target: HTMLElement, text: string) {
  for (let i = 0; i < 15; i++) await Promise.resolve();
  flushSync();
  const textarea = target.querySelector('textarea.field') as HTMLTextAreaElement;
  textarea.value = text;
  textarea.dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
  const sendBtn = Array.from(target.querySelectorAll('button')).find(
    (b) => b.textContent?.trim() === 'Ask',
  ) as HTMLButtonElement;
  sendBtn.click();
  flushSync();
  await tick();
  for (let i = 0; i < 10; i++) await Promise.resolve();
  flushSync();
}

describe('AskJoddModal — error-turn {@html} safety', () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  it('renders a backend error containing HTML as inert text, not markup', async () => {
    // Mirrors llm/http.rs UpstreamError: the raw response body of a
    // misbehaving/compromised upstream endpoint, embedded verbatim.
    const hostileError = 'HTTP 502: <img src=x onerror="window.__pwned = true">';
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'ask_jodd') return Promise.reject(new Error(hostileError));
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      return Promise.resolve([]);
    });

    const { target, host } = render();
    await sendQuestion(target, 'What did we decide about sync conflicts?');

    const turns = askTurns(target);
    // The failure text must be visible to the user...
    expect(turns.textContent).toContain('onerror');
    expect(turns.textContent).toContain('<img');
    // ...but never parsed as markup: no real <img> element in the DOM, and
    // the payload never executed.
    expect(turns.querySelector('img')).toBeNull();
    expect((globalThis as any).__pwned).toBeUndefined();
    // Belt and suspenders: the serialized markup must show the escaped
    // entity form, not a raw '<img'.
    expect(turns.innerHTML).toContain('&lt;img');
    expect(turns.innerHTML).not.toContain('<img');

    unmount(host);
  });

  it('still renders a real answer as a clickable chip via {@html} (positive control)', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'ask_jodd') return Promise.resolve(ANSWER);
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      return Promise.resolve([]);
    });

    const { target, host } = render();
    await sendQuestion(target, 'How do we handle conflicts?');

    const turns = askTurns(target);
    expect(turns.querySelector('.cite-chip')).toBeTruthy();
    expect(turns.textContent).toContain('Sync conflicts');

    unmount(host);
  });
});

describe('AskJoddModal — AskAnswer wire shape', () => {
  // The suite mocks `invoke`, so a drifted backend shape can never fail the
  // behavioural tests above — they'd keep passing against a fixture that no
  // longer resembles what Rust sends. This pins the fixture's key set to the
  // eight fields asserted in `ask_wire_shapes_are_stable`
  // (src-tauri/src/ask/mod.rs); the two lists are the contract, and a field
  // added on one side without the other shows up as a diff between them.
  it('fixture carries exactly the eight AskAnswer fields', () => {
    expect(Object.keys(ANSWER).sort()).toEqual([
      'cited',
      'conversation_id',
      'dropped_citations',
      'markdown',
      'notes_considered',
      'notes_in_scope',
      'notes_used',
      'trimmed',
    ]);
    expect(Object.keys((ANSWER.cited as object[])[0]).sort()).toEqual([
      'account_id',
      'slug',
      'title',
      'uuid',
    ]);
  });
});

describe('AskJoddModal — error turns excluded from replayed context', () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  it('does not send a prior error turn back to the model as assistant context', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'ask_jodd') return Promise.reject(new Error('provider not configured'));
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      return Promise.resolve([]);
    });

    const { target, host } = render();
    await sendQuestion(target, 'first question');

    // Second call still rejects, but we only care what turns/wire it sent.
    await sendQuestion(target, 'second question');

    const calls = invoke.mock.calls.filter((c) => c[0] === 'ask_jodd');
    expect(calls.length).toBe(2);
    expect(calls[1][1]).toMatchObject({ question: 'second question', sessionId: 'session' });
    expect(calls[1][1]).not.toHaveProperty('turns');

    unmount(host);
  });
});

describe('AskJoddModal — no app-level provider empty state', () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  // Ask Jodd runs only on the app-level provider. Before this, an unconfigured
  // provider was discoverable exactly one way: type a question, wait, read a
  // red error turn. Design spec §8 asked for an empty state instead.
  function mockAppProvider(provider: string) {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') {
        return provider === 'none'
          ? Promise.reject('Ask Jodd needs an LLM provider. Open App Settings.')
          : Promise.resolve(SESSION);
      }
      return Promise.resolve([]);
    });
  }

  async function settle() {
    for (let i = 0; i < 10; i++) await Promise.resolve();
    flushSync();
  }

  it('offers a route into App Settings instead of the ask prompt', async () => {
    mockAppProvider('none');
    const { target, host } = render();
    await settle();

    expect(askTurns(target).textContent).toContain('needs an AI provider');
    const btn = Array.from(target.querySelectorAll('button')).find(
      (b) => b.textContent?.trim() === 'Open App Settings',
    );
    expect(btn).toBeTruthy();

    unmount(host);
  });

  it('blocks asking, so the red-error-turn path is unreachable', async () => {
    mockAppProvider('none');
    const { target, host } = render();
    await settle();

    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(true);
    await sendQuestion(target, 'anything');
    expect(invoke.mock.calls.some((c) => c[0] === 'ask_jodd')).toBe(false);

    unmount(host);
  });

  it('stays out of the way once a provider is configured', async () => {
    mockAppProvider('agent_cli');
    const { target, host } = render();
    await settle();

    expect(askTurns(target).textContent).not.toContain('needs an AI provider');
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(false);

    unmount(host);
  });

  it('fails closed when permission preflight cannot be read', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.reject(new Error('config unreadable'));
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      return Promise.resolve([]);
    });
    const { target, host } = render();
    await settle();

    expect(target.textContent).toContain('config unreadable');
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(true);

    unmount(host);
  });
});

describe('AskJoddModal — unsendable scope', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_app_llm_provider') {
        return Promise.resolve({ provider: 'agent_cli' });
      }
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      return Promise.resolve([]);
    });
  });

  // `askScope` returns null for an account-anchored scope with no account,
  // because the backend deserializes account_id into a String and would reject
  // null. The UI has to honour that instead of sending anyway.
  function renderWithoutAccount() {
    const target = document.createElement('div');
    document.body.appendChild(target);
    currentAccount.set(null);
    const host = mount(AskJoddModal, { target, props: { open: true } });
    flushSync();
    return { target, host };
  }

  async function settle() {
    for (let i = 0; i < 10; i++) await Promise.resolve();
    flushSync();
  }

  it('does not send, and says why, when no account is selected', async () => {
    const { target, host } = renderWithoutAccount();
    await settle();

    expect(askTurns(target).textContent).toContain('No account is selected');
    await sendQuestion(target, 'what did I decide?');
    expect(invoke.mock.calls.some((c) => c[0] === 'ask_jodd')).toBe(false);

    unmount(host);
  });

  it('keeps the question in the box rather than eating it', async () => {
    const { target, host } = renderWithoutAccount();
    await settle();

    await sendQuestion(target, 'what did I decide?');
    // send() bails before `input = ''`, so the text the user typed survives for
    // them to retry after selecting an account.
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).value).toBe(
      'what did I decide?',
    );

    unmount(host);
  });

  it('becomes sendable via All accounts, which needs no account', async () => {
    const { target, host } = renderWithoutAccount();
    await settle();

    const select = target.querySelector('select.field') as HTMLSelectElement;
    select.value = 'all';
    select.dispatchEvent(new Event('change', { bubbles: true }));
    flushSync();

    await sendQuestion(target, 'what did I decide?');
    const calls = invoke.mock.calls.filter((c) => c[0] === 'ask_jodd');
    expect(calls.length).toBe(1);
    expect(invoke.mock.calls.filter((c) => c[0] === 'begin_ask').at(-1)?.[1].scope).toEqual({ kind: 'all_accounts' });

    unmount(host);
  });
});

describe('Ask permission boundary', () => {
  it('discards an old answer immediately when the scope changes', async () => {
    let finish!: (value: unknown) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'ask_jodd') return new Promise((resolve) => { finish = resolve; });
      if (cmd === 'get_app_llm_provider') return Promise.resolve({ provider: 'http' });
      if (cmd === 'begin_ask') return Promise.resolve({ session_id: 'session', destination: 'HTTP https://example.test/v1 · test', scope: 'Allowed accounts' });
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      return Promise.resolve([]);
    });
    const { target, host } = render();
    await sendQuestion(target, 'old question');
    const scope = target.querySelector('select')!;
    scope.value = 'all';
    scope.dispatchEvent(new Event('change', { bubbles: true }));
    flushSync();
    finish({ ...ANSWER, markdown: 'FORBIDDEN_OLD_ANSWER' });
    for (let i = 0; i < 15; i++) await Promise.resolve();
    flushSync();
    expect(askTurns(target).textContent).not.toContain('FORBIDDEN_OLD_ANSWER');
    expect(askTurns(target).textContent).not.toContain('old question');
    await unmount(host);
  });
});

describe('Ask backend-owned conversation eligibility', () => {
  beforeEach(() => { invoke.mockReset(); });
  it('discards a pending answer and resets history on a policy event', async () => {
    let finish!: (value: unknown) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      if (cmd === 'ask_jodd') return new Promise((resolve) => { finish = resolve; });
      return Promise.resolve(null);
    });
    const { target, host } = render();
    await sendQuestion(target, 'old context');
    policyEvents.changed();
    flushSync();
    finish({ ...ANSWER, markdown: 'REVOKED_ANSWER' });
    for (let i = 0; i < 20; i++) await Promise.resolve();
    flushSync();
    expect(askTurns(target).textContent).not.toContain('REVOKED_ANSWER');
    expect(askTurns(target).textContent).not.toContain('old context');
    expect(invoke.mock.calls.some(([name]) => name === 'end_ask')).toBe(true);
    expect(invoke.mock.calls.some(([name]) => name === 'cancel_ask')).toBe(true);
    await unmount(host);
  });

  it('ignores a stale permission preflight and closes its backend session', async () => {
    let finish!: (value: unknown) => void;
    let starts = 0;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return ++starts === 1
        ? new Promise((resolve) => { finish = resolve; })
        : Promise.resolve({ ...SESSION, session_id: 'new', destination: 'NEW_DESTINATION' });
      if (cmd === 'ask_jodd') return Promise.resolve(ANSWER);
      return Promise.resolve(null);
    });
    const { target, host } = render();
    const select = target.querySelector('select')!;
    select.value = 'all'; select.dispatchEvent(new Event('change', { bubbles: true }));
    flushSync();
    finish({ ...SESSION, session_id: 'old', destination: 'OLD_DESTINATION' });
    await sendQuestion(target, 'new question');
    expect(target.textContent).toContain('NEW_DESTINATION');
    expect(target.textContent).not.toContain('OLD_DESTINATION');
    expect(invoke).toHaveBeenCalledWith('end_ask', { sessionId: 'old' });
    const ask = invoke.mock.calls.find(([cmd]) => cmd === 'ask_jodd')![1];
    expect(ask).toMatchObject({ sessionId: 'new', question: 'new question' });
    expect(ask).not.toHaveProperty('turns');
    expect(ask).not.toHaveProperty('scope');
    await unmount(host);
  });

  it('opens on the current real folder and discloses the actual preflight destination', async () => {
    invoke.mockImplementation((cmd: string) => cmd === 'begin_ask'
      ? Promise.resolve({ ...SESSION, destination: 'Agent CLI fixture · may use cloud services', scope: 'Notes/Demo and subfolders' })
      : Promise.resolve(null));
    currentAccount.set('a@x'); selectedFolder.set('Notes/Demo');
    const target = document.createElement('div'); document.body.appendChild(target);
    const host = mount(AskJoddModal, { target, props: { open: true } });
    flushSync();
    for (let i = 0; i < 15; i++) await Promise.resolve();
    flushSync();
    expect(invoke).toHaveBeenCalledWith('begin_ask', { scope: { kind: 'folder', account_id: 'a@x', label: 'Notes/Demo' } });
    expect(target.textContent).toContain('may use cloud services');
    expect(target.textContent).toContain('Data already sent cannot be recalled');
    expect(target.textContent).toContain('External MCP clients');
    await unmount(host); selectedFolder.set('');
  });

  it('unmount cancels a request and never renders its late rejection', async () => {
    let reject!: (value: unknown) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      if (cmd === 'ask_jodd') return new Promise((_, r) => { reject = r; });
      return Promise.resolve(null);
    });
    const { target, host } = render();
    await sendQuestion(target, 'question');
    await unmount(host);
    reject('LATE_ERROR');
    for (let i = 0; i < 15; i++) await Promise.resolve();
    expect(target.textContent).not.toContain('LATE_ERROR');
    expect(invoke.mock.calls.some(([cmd]) => cmd === 'end_ask')).toBe(true);
  });
});

describe('AskJoddModal — consent refusal', () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  it('offers Allow AI for the scope account, and the policy event re-opens the session', async () => {
    let allowed = false;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') {
        return allowed ? Promise.resolve(SESSION) : Promise.reject(`provider not configured: ${AI_CONSENT_NEEDED}`);
      }
      if (cmd === 'allow_ai_for_account') { allowed = true; return Promise.resolve(); }
      return Promise.resolve([]);
    });
    const { target, host } = render();
    for (let i = 0; i < 15; i++) await Promise.resolve();
    flushSync();
    const allow = [...target.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Allow AI for a@x');
    expect(allow).toBeDefined();
    allow!.click();
    for (let i = 0; i < 10; i++) await Promise.resolve();
    flushSync();
    expect(invoke).toHaveBeenCalledWith('allow_ai_for_account', { accountId: 'a@x' });
    // allow_ai_for_account → invalidate_ai → emit("ai-policy-changed") (gotcha #6)
    policyEvents.changed();
    for (let i = 0; i < 15; i++) await Promise.resolve();
    flushSync();
    expect(target.textContent).toContain(`Destination: ${SESSION.destination}`);
    unmount(host);
  });

  // A refused Allow must never vanish. `allow_ai_for_account` emits
  // ai-policy-changed BEFORE it saves, so a failed save still bumps the
  // generation underneath allowAi and the re-check returns the same refusal.
  function refusedThenAllowRejects() {
    let rejectAllow!: (e: unknown) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.reject(`provider not configured: ${AI_CONSENT_NEEDED}`);
      if (cmd === 'allow_ai_for_account') return new Promise((_, r) => { rejectAllow = r; });
      return Promise.resolve([]);
    });
    return { rejectAllow: (e: unknown) => rejectAllow(e) };
  }
  const settleTurns = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); flushSync(); };
  const findAllow = (t: HTMLElement) =>
    [...t.querySelectorAll('button')].find((b) => b.textContent?.trim().startsWith('Allow AI for'));

  function refusedForever() {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.reject(`provider not configured: ${AI_CONSENT_NEEDED}`);
      return Promise.resolve([]);
    });
  }

  // Live 2026-10-14, Galaxy Tab S7: the refusal arrived as raw text
  // ("provider not configured: AI data access is not allowed…") with a
  // link-styled button under it, above a question box that looked usable.
  // The user could not tell the button had to be pressed.
  it('explains the consent state as a titled callout with a primary, account-named button', async () => {
    accounts.set([{ id: 'a@x', email: 'kai@example.test', backend_kind: 'gmail' } as unknown as Account]);
    refusedForever();
    const { target, host } = render();
    await settleTurns();

    const callout = askTurns(target).querySelector('.needs-consent') as HTMLElement;
    expect(callout).toBeTruthy();
    expect(callout.getAttribute('role')).toBe('alert');
    const text = callout.textContent!.replace(/\s+/g, ' ');
    expect(text).toContain('Ask Jodd needs permission to read this account’s notes');
    // What is sent, and where — mirrors the Destination/Scope text a session shows.
    expect(text).toContain('your questions');
    expect(text).toContain('titles/tags/folders');
    expect(text).toContain('selected note bodies');
    expect(text).toContain('AI provider set in App Settings');
    // Names the same Account Settings checkbox the click ticks.
    expect(text).toContain(DATA_ALLOWED_LABEL);

    const allow = findAllow(target)!;
    expect(allow.textContent?.trim()).toBe('Allow AI for kai@example.test');
    expect(allow.classList.contains('primary')).toBe(true);
    expect(callout.contains(allow)).toBe(true);

    // The raw wire string is not what the user reads.
    expect(target.textContent).not.toContain('provider not configured');
    expect(target.textContent).not.toContain('Retry permission check');
    // Nothing in the conversation area invites a question that cannot be asked.
    expect(askTurns(target).textContent).not.toContain('Ask a question about your notes');
    const textarea = target.querySelector('textarea.field') as HTMLTextAreaElement;
    expect(textarea.disabled).toBe(true);
    expect(textarea.placeholder).toContain('Allow AI for this account above');
    const ask = [...target.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Ask') as HTMLButtonElement;
    expect(ask.disabled).toBe(true);

    // One explicit click, never automatic.
    expect(invoke.mock.calls.some(([cmd]) => cmd === 'allow_ai_for_account')).toBe(false);
    unmount(host);
    accounts.set([]);
  });

  it('keeps other preflight refusals as plain text with a retry', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.reject('AI data access is disabled or this account is unavailable. Review its AI data permission in Account Settings.');
      return Promise.resolve([]);
    });
    const { target, host } = render();
    await settleTurns();
    expect(target.querySelector('.needs-consent')).toBeNull();
    expect(findAllow(target)).toBeUndefined();
    expect(target.textContent).toContain('Retry permission check');
    unmount(host);
  });

  it('restores the refusal inline when the allow fails and nothing else changed', async () => {
    error.set(null);
    const ctl = refusedThenAllowRejects();
    const { target, host } = render();
    await settleTurns();
    findAllow(target)!.click();
    flushSync();
    ctl.rejectAllow('disk full');
    await settleTurns();
    const failure = target.querySelector('.needs-consent .allow-error');
    expect(failure?.textContent).toBe('Could not allow AI for this account: disk full');
    expect(findAllow(target)).toBeDefined();
    expect(get(error)).toBeNull();
    unmount(host);
  });

  it('reports a failed allow through the error bar when the policy event already re-ran the check', async () => {
    error.set(null);
    const ctl = refusedThenAllowRejects();
    const { target, host } = render();
    await settleTurns();
    findAllow(target)!.click();
    flushSync();
    // The backend emits before it saves: the listener re-prepares, and only then does the save fail.
    policyEvents.changed();
    await settleTurns();
    ctl.rejectAllow('disk full');
    await settleTurns();
    expect(get(error)).toBe('Could not allow AI for this account: disk full');
    unmount(host);
  });
});

// Live 2026-10-14: after one answer, Cancel on a second question went through
// invalidate() — it ended the backend session and wiped the first answer and
// its citations. Cancel now stops only the request in flight.
describe('AskJoddModal — Cancel stops one question, not the conversation', () => {
  beforeEach(() => { invoke.mockReset(); });

  type Pending = { resolve: (v: unknown) => void; reject: (e: unknown) => void };
  function mockAsks() {
    const asks: Pending[] = [];
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      if (cmd === 'ask_jodd') {
        return asks.length === 0
          ? (asks.push({ resolve: () => {}, reject: () => {} }), Promise.resolve(ANSWER))
          : new Promise((resolve, reject) => { asks.push({ resolve, reject }); });
      }
      return Promise.resolve(null);
    });
    return asks;
  }
  const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); flushSync(); };
  const calls = (cmd: string) => invoke.mock.calls.filter(([c]) => c === cmd);

  it('keeps the earlier answer, its citations and the session', async () => {
    const asks = mockAsks();
    const { target, host } = render();
    await sendQuestion(target, 'first question');
    await sendQuestion(target, 'second question');
    const secondRequest = calls('ask_jodd')[1][1].requestId;

    (target.querySelector('.ask-turn.busy button') as HTMLButtonElement).click();
    flushSync();

    expect(invoke).toHaveBeenCalledWith('cancel_ask', { requestId: secondRequest });
    expect(calls('end_ask')).toHaveLength(0);
    expect(calls('begin_ask')).toHaveLength(1);
    const turns = askTurns(target);
    expect(turns.textContent).toContain('first question');
    expect(turns.querySelector('.cite-chip')).toBeTruthy();
    expect(turns.textContent).toContain('second question');
    expect(turns.querySelector('.cancelled-note')).toBeTruthy();
    expect(turns.querySelector('.ask-turn.busy')).toBeNull();
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(false);

    // The cancelled request's late outcome is ignored, whichever way it lands.
    asks[1].resolve({ ...ANSWER, markdown: 'LATE_CANCELLED_ANSWER' });
    await settle();
    expect(askTurns(target).textContent).not.toContain('LATE_CANCELLED_ANSWER');
    expect(askTurns(target).querySelectorAll('.ask-turn.error')).toHaveLength(0);
    unmount(host);
  });

  it('ignores a late rejection from the cancelled request', async () => {
    const asks = mockAsks();
    const { target, host } = render();
    await sendQuestion(target, 'first question');
    await sendQuestion(target, 'second question');
    (target.querySelector('.ask-turn.busy button') as HTMLButtonElement).click();
    flushSync();
    asks[1].reject('Question cancelled.');
    await settle();
    expect(askTurns(target).textContent).not.toContain('Question cancelled.');
    expect(askTurns(target).querySelector('.cite-chip')).toBeTruthy();
    unmount(host);
  });

  it('asks the next question in the same session once the cancelled one has unwound', async () => {
    const asks = mockAsks();
    const { target, host } = render();
    await sendQuestion(target, 'first question');
    await sendQuestion(target, 'second question');
    (target.querySelector('.ask-turn.busy button') as HTMLButtonElement).click();
    flushSync();

    // The backend holds the session busy until the cancelled call returns, so
    // the third question must not reach ask_jodd before then.
    await sendQuestion(target, 'third question');
    expect(calls('ask_jodd')).toHaveLength(2);
    asks[1].reject('Question cancelled.');
    await settle();
    expect(calls('ask_jodd')).toHaveLength(3);
    expect(calls('ask_jodd')[2][1]).toMatchObject({ sessionId: 'session', question: 'third question' });

    asks[2].resolve({ ...ANSWER, markdown: 'THIRD_ANSWER [[sync-conflicts-aabbccdd]]' });
    await settle();
    expect(askTurns(target).textContent).toContain('THIRD_ANSWER');
    expect(askTurns(target).querySelectorAll('.cite-chip').length).toBe(2);
    unmount(host);
  });

  // The footer's Close/Cancel button is gone (R10): Modal's header close
  // button is the close control, and while busy it cancels first, as the old
  // footer button did.
  it('a Close click while busy cancels the question and keeps the modal and conversation', async () => {
    mockAsks();
    const { target, host } = render();
    await sendQuestion(target, 'first question');
    await sendQuestion(target, 'second question');
    const close = () => target.querySelector('dialog button[aria-label="Close"]') as HTMLButtonElement;
    close().click();
    flushSync();

    expect(calls('cancel_ask')).toHaveLength(1);
    expect(calls('end_ask')).toHaveLength(0);
    expect(target.querySelector('.ask-modal')).toBeTruthy();
    expect(askTurns(target).querySelector('.cite-chip')).toBeTruthy();

    // Not busy any more, so the same button now closes — and that ends the session.
    close().click();
    flushSync();
    expect(target.querySelector('.ask-modal')).toBeNull();
    expect(calls('end_ask')).toHaveLength(1);
    unmount(host);
  });
});

describe('AskJoddModal — history', () => {
  const STORED = {
    id: 'conv-1', title: 'Why keep-both?', scope: { kind: 'all_accounts' }, destination: SESSION.destination,
    created_at: 1, updated_at: 2, archived: false,
    turns: [{
      seq: 1, question: 'Why keep-both?', answer_markdown: 'Because. [[sync-conflicts-aabbccdd]]',
      cited: ANSWER.cited, read: [], created_at: 1,
      stats: { notes_in_scope: 10, notes_considered: 10, notes_used: 3, trimmed: false, dropped_citations: 0 },
    }],
  };
  const LIST = [{ id: 'conv-1', title: 'Why keep-both?', scope: { kind: 'all_accounts' }, updated_at: Date.now(), archived: false, turn_count: 1 }];
  let resume: (cmd: string, args: any) => unknown;

  async function settle() { for (let i = 0; i < 15; i++) await Promise.resolve(); flushSync(); }
  const calls = (cmd: string) => invoke.mock.calls.filter((c) => c[0] === cmd);
  const btn = (t: HTMLElement, text: string) =>
    Array.from(t.querySelectorAll('button')).find((b) => b.textContent?.trim() === text) as HTMLButtonElement;

  beforeEach(() => {
    invoke.mockReset();
    resume = () => Promise.resolve({ kind: 'ready', session: { ...SESSION, session_id: 'resumed' } });
    invoke.mockImplementation((cmd: string, args: any) => {
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      if (cmd === 'list_ask_conversations') return Promise.resolve(args.archived ? [] : LIST);
      if (cmd === 'load_ask_conversation') return Promise.resolve(STORED);
      if (cmd === 'resume_ask') return resume(cmd, args);
      if (cmd === 'ask_jodd') return Promise.resolve({ ...ANSWER, conversation_id: 'conv-new' });
      return Promise.resolve(null);
    });
  });

  async function openConv1(target: HTMLElement) {
    await settle();
    (target.querySelector('.ask-row[data-id="conv-1"] .ask-row-title') as HTMLElement).click();
    await settle();
  }

  it('opens a saved conversation, shows its turns and resumes it', async () => {
    const { target, host } = render();
    await openConv1(target);
    expect(askTurns(target).textContent).toContain('Why keep-both?');
    expect(askTurns(target).querySelector('.cite-chip')).toBeTruthy();
    expect(askTurns(target).textContent).toContain('10 in scope');
    expect(calls('resume_ask').at(-1)?.[1]).toEqual({ conversationId: 'conv-1', acceptDestination: undefined });
    expect(target.querySelector('.scope-locked')?.textContent).toContain('All accounts');
    await sendQuestion(target, 'follow-up');
    expect(calls('ask_jodd').at(-1)?.[1]).toMatchObject({ sessionId: 'resumed', question: 'follow-up' });
    unmount(host);
  });

  it('asks before sending history to a new destination', async () => {
    resume = (_c, args) => Promise.resolve(args.acceptDestination === 'NEW'
      ? { kind: 'ready', session: { ...SESSION, session_id: 'resumed' } }
      : { kind: 'needs_confirm', previous: 'OLD', current: 'NEW' });
    const { target, host } = render();
    await openConv1(target);
    expect(target.querySelector('.resume-confirm')?.textContent).toContain('NEW');
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(true);
    btn(target, 'Continue').click();
    await settle();
    expect(calls('resume_ask').at(-1)?.[1]).toEqual({ conversationId: 'conv-1', acceptDestination: 'NEW' });
    expect(target.querySelector('.resume-confirm')).toBeNull();
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(false);
    unmount(host);
  });

  it('shows a read-only conversation with its reason and offers a fresh start', async () => {
    resume = () => Promise.resolve({ kind: 'read_only', reason: 'A note this conversation used is now in a hidden folder.' });
    const { target, host } = render();
    await openConv1(target);
    expect(target.querySelector('.read-only')?.textContent).toContain('hidden folder');
    expect(target.querySelector('textarea.field')).toBeNull();
    expect(askTurns(target).textContent).toContain('Why keep-both?');
    btn(target, 'Start new conversation with this scope').click();
    await settle();
    expect(askTurns(target).textContent).not.toContain('Because.');
    expect(calls('begin_ask').at(-1)?.[1]).toEqual({ scope: { kind: 'all_accounts' } });
    unmount(host);
  });

  it('binds a new conversation to the id the first answer returns and refreshes the rail', async () => {
    const { target, host } = render();
    await settle();
    const before = calls('list_ask_conversations').length;
    await sendQuestion(target, 'fresh question');
    await settle();
    expect(calls('list_ask_conversations').length).toBeGreaterThan(before);
    unmount(host);
  });

  it('+ New leaves a loaded conversation for a fresh session', async () => {
    const { target, host } = render();
    await openConv1(target);
    btn(target, '+ New').click();
    await settle();
    expect(askTurns(target).textContent).not.toContain('Because.');
    expect(target.querySelector('select.field')).toBeTruthy();
    expect(calls('end_ask').some((c) => c[1].sessionId === 'resumed')).toBe(true);
    unmount(host);
  });

  it('keeps the stored turns and shows the refusal when resume_ask rejects with an R1 message', async () => {
    resume = () => Promise.reject(`provider not configured: ${AI_CONSENT_NEEDED}`);
    const { target, host } = render();
    await openConv1(target);
    expect(askTurns(target).textContent).toContain('Because.');
    expect(target.querySelector('.hint [role="alert"]')?.textContent).toContain(AI_CONSENT_NEEDED);
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(true);
    unmount(host);
  });

  // The merge with #191: a consent refusal on resume names the account the
  // SAVED scope is for, so it reaches the same callout begin_ask's does.
  it('an account-scoped conversation refused for consent on resume shows the consent callout', async () => {
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args: any) =>
      cmd === 'load_ask_conversation'
        ? Promise.resolve({ ...STORED, scope: { kind: 'account', account_id: 'a@x' } })
        : base(cmd, args));
    resume = () => Promise.reject(`provider not configured: ${AI_CONSENT_NEEDED}`);
    const { target, host } = render();
    await openConv1(target);
    expect(askTurns(target).textContent).toContain('Because.');
    expect(askTurns(target).querySelector('.needs-consent')).toBeTruthy();
    expect(btn(target, 'Allow AI for a@x')).toBeDefined();
    expect(target.textContent).not.toContain('provider not configured');
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(true);
    unmount(host);
  });

  it('opens an archived conversation read-only and Unarchive resumes it', async () => {
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args: any) =>
      cmd === 'load_ask_conversation' ? Promise.resolve({ ...STORED, archived: true }) : base(cmd, args));
    const { target, host } = render();
    await openConv1(target);
    expect(target.querySelector('.read-only')?.textContent).toContain('archived');
    expect(calls('resume_ask')).toHaveLength(0);
    const listsBefore = calls('list_ask_conversations').length;
    btn(target, 'Unarchive').click();
    await settle();
    expect(calls('set_ask_conversation_archived').at(-1)?.[1]).toEqual({ conversationId: 'conv-1', archived: false });
    expect(calls('resume_ask').at(-1)?.[1]).toEqual({ conversationId: 'conv-1', acceptDestination: undefined });
    expect(calls('list_ask_conversations').length).toBeGreaterThan(listsBefore);
    expect(target.querySelector('.read-only')).toBeNull();
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(false);
    unmount(host);
  });

  it('a failed Unarchive rolls back and reports', async () => {
    error.set(null);
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args: any) => {
      if (cmd === 'load_ask_conversation') return Promise.resolve({ ...STORED, archived: true });
      if (cmd === 'set_ask_conversation_archived') return Promise.reject('disk full');
      return base(cmd, args);
    });
    const { target, host } = render();
    await openConv1(target);
    btn(target, 'Unarchive').click();
    await settle();
    expect(get(error)).toBe('Could not unarchive the conversation: disk full');
    expect(btn(target, 'Unarchive')).toBeTruthy();
    expect(calls('resume_ask')).toHaveLength(0);
    error.set(null);
    unmount(host);
  });

  it('re-reads the history setting each time the modal opens', async () => {
    const { writable, fromStore } = await import('svelte/store');
    let enabled = false;
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args: any) =>
      cmd === 'get_ask_history_enabled' ? Promise.resolve(enabled) : base(cmd, args));
    const openStore = writable(true);
    const openBox = fromStore(openStore);
    const target = document.createElement('div');
    document.body.appendChild(target);
    currentAccount.set('a@x'); selectedFolder.set('');
    const host = mount(AskJoddModal, {
      target,
      props: { get open() { return openBox.current; }, set open(v: boolean) { openStore.set(v); } },
    });
    flushSync();
    await settle();
    expect(askTurns(target).textContent).toContain('History is off');
    (target.querySelector('dialog button[aria-label="Close"]') as HTMLButtonElement).click();
    flushSync();
    expect(target.querySelector('.ask-modal')).toBeNull();
    enabled = true;
    openStore.set(true);
    flushSync();
    await settle();
    expect(askTurns(target).textContent).toContain('kept on this device under History');
    expect(askTurns(target).textContent).not.toContain('History is off');
    unmount(host);
  });

  it('locks the scope display after "Start new conversation with this scope"', async () => {
    resume = () => Promise.resolve({ kind: 'read_only', reason: 'A note this conversation used is now in a hidden folder.' });
    const { target, host } = render();
    await openConv1(target);
    btn(target, 'Start new conversation with this scope').click();
    await settle();
    expect(target.querySelector('.scope-locked')?.textContent).toContain('All accounts');
    expect(target.querySelector('select.field')).toBeNull();
    btn(target, 'New conversation').click();
    await settle();
    expect(target.querySelector('.scope-locked')).toBeNull();
    expect(target.querySelector('select.field')).toBeTruthy();
    unmount(host);
  });

  it('archiving the open conversation from the rail re-checks it into read-only', async () => {
    let archived = false;
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args: any) => {
      if (cmd === 'set_ask_conversation_archived') { archived = args.archived; return Promise.resolve(null); }
      if (cmd === 'load_ask_conversation') return Promise.resolve({ ...STORED, archived });
      return base(cmd, args);
    });
    const { target, host } = render();
    await openConv1(target);
    expect(target.querySelector('.read-only')).toBeNull();
    (target.querySelector('.ask-row[data-id="conv-1"] .ask-row-menu') as HTMLButtonElement).click(); flushSync();
    btn(target, 'Archive').click();
    await settle();
    expect(target.querySelector('.read-only')?.textContent).toContain('archived');
    expect(calls('end_ask').some((c) => c[1].sessionId === 'resumed')).toBe(true);
    unmount(host);
  });

  it('re-clicking the open, live conversation does nothing', async () => {
    const { target, host } = render();
    await openConv1(target);
    const ends = calls('end_ask').length;
    const resumes = calls('resume_ask').length;
    const loads = calls('load_ask_conversation').length;
    (target.querySelector('.ask-row[data-id="conv-1"] .ask-row-title') as HTMLElement).click();
    await settle();
    expect(calls('end_ask')).toHaveLength(ends);
    expect(calls('resume_ask')).toHaveLength(resumes);
    expect(calls('load_ask_conversation')).toHaveLength(loads);
    expect(askTurns(target).textContent).toContain('Because.');
    unmount(host);
  });

  // Not in the brief: replacing `loaded` with a renamed copy changes the
  // derived scope object, which re-runs the effect and re-resumes.
  it('renaming the open conversation does not re-resume it', async () => {
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args: any) =>
      cmd === 'rename_ask_conversation' ? Promise.resolve('Renamed') : base(cmd, args));
    const { target, host } = render();
    await openConv1(target);
    const resumes = calls('resume_ask').length;
    (target.querySelector('.ask-row[data-id="conv-1"] .ask-row-menu') as HTMLButtonElement).click(); flushSync();
    btn(target, 'Rename').click(); flushSync();
    const field = target.querySelector('.ask-row[data-id="conv-1"] input') as HTMLInputElement;
    field.value = 'Renamed';
    field.dispatchEvent(new Event('input', { bubbles: true }));
    field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await settle();
    expect(calls('rename_ask_conversation')).toHaveLength(1);
    expect(calls('resume_ask')).toHaveLength(resumes);
    expect(calls('end_ask').some((c) => c[1].sessionId === 'resumed')).toBe(false);
    expect(askTurns(target).textContent).toContain('Because.');
    unmount(host);
  });

  it('a later click wins over a slow load of an earlier one', async () => {
    let finishSlow!: (v: unknown) => void;
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args: any) => {
      if (cmd === 'load_ask_conversation' && args.conversationId === 'conv-1') return new Promise((r) => { finishSlow = r; });
      return base(cmd, args);
    });
    const { target, host } = render();
    await settle();
    (target.querySelector('.ask-row[data-id="conv-1"] .ask-row-title') as HTMLElement).click();
    btn(target, '+ New').click();
    await settle();
    finishSlow(STORED);
    await settle();
    expect(askTurns(target).textContent).not.toContain('Because.');
    expect(calls('resume_ask')).toHaveLength(0);
    expect(target.querySelector('select.field')).toBeTruthy();
    unmount(host);
  });

  it('deleting the open conversation starts a new one', async () => {
    const { target, host } = render();
    await openConv1(target);
    (target.querySelector('.ask-row[data-id="conv-1"] .ask-row-menu') as HTMLButtonElement).click(); flushSync();
    btn(target, 'Delete').click(); flushSync();
    btn(target, 'Delete conversation').click();
    await settle();
    expect(askTurns(target).textContent).not.toContain('Because.');
    unmount(host);
  });
});

// Live 2026-10-14 (macOS): after a long first answer, a follow-up cleared the
// input but its question and answer landed below the fold, so it looked as if
// nothing had happened. The newest question now scrolls to the top of
// `.ask-turns` — the top, not the bottom, so a long answer is read from its start.
describe('AskJoddModal — a new turn scrolls its question into view', () => {
  const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
  const saved = { scrollTo: proto.scrollTo, rect: proto.getBoundingClientRect };
  let scrollTo: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    invoke.mockReset();
    scrollTo = vi.fn();
    proto.scrollTo = scrollTo;
    // jsdom does no layout: the container sits at 100, the n-th question 600px below the last.
    proto.getBoundingClientRect = function (this: HTMLElement) {
      if (this.classList.contains('ask-turns')) return { top: 100 } as DOMRect;
      const questions = [...document.querySelectorAll('.ask-turn.user')];
      const i = questions.indexOf(this);
      return { top: i < 0 ? 0 : 100 + 600 * i } as DOMRect;
    };
  });
  afterEach(() => {
    proto.scrollTo = saved.scrollTo;
    proto.getBoundingClientRect = saved.rect;
  });

  function mockAsks() {
    const pending: Array<(v: unknown) => void> = [];
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      if (cmd === 'ask_jodd') return new Promise((resolve) => { pending.push(resolve); });
      return Promise.resolve(null);
    });
    return pending;
  }
  const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); flushSync(); };

  it('scrolls a follow-up question to the top on send, and again when its answer lands', async () => {
    const answers = mockAsks();
    const { target, host } = render();
    await sendQuestion(target, 'first question');
    answers[0](ANSWER);
    await settle();

    scrollTo.mockClear();
    await sendQuestion(target, 'follow-up question');
    // Before any answer: the follow-up is already in view.
    expect(scrollTo).toHaveBeenCalledTimes(1);
    expect(scrollTo.mock.contexts[0]).toBe(askTurns(target));
    expect(scrollTo).toHaveBeenLastCalledWith({ top: 600, behavior: 'smooth' });

    answers[1]({ ...ANSWER, markdown: 'A long answer.' });
    await settle();
    expect(askTurns(target).textContent).toContain('A long answer.');
    expect(scrollTo).toHaveBeenCalledTimes(2);
    expect(scrollTo).toHaveBeenLastCalledWith({ top: 600, behavior: 'smooth' });
    unmount(host);
  });

  it('scrolls when the answer is an error turn too', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      if (cmd === 'ask_jodd') return Promise.reject('upstream down');
      return Promise.resolve(null);
    });
    const { target, host } = render();
    await sendQuestion(target, 'first question');
    await sendQuestion(target, 'second question');
    await settle();
    expect(askTurns(target).querySelectorAll('.ask-turn.error')).toHaveLength(2);
    expect(scrollTo).toHaveBeenLastCalledWith({ top: 600, behavior: 'smooth' });
    unmount(host);
  });

  it('a Cancel inside the render tick stops the pending scroll', async () => {
    mockAsks();
    const { target, host } = render();
    for (let i = 0; i < 15; i++) await Promise.resolve();
    flushSync();
    const textarea = target.querySelector('textarea.field') as HTMLTextAreaElement;
    textarea.value = 'question';
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    [...target.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Ask')!.click();
    flushSync();
    // Synchronously, before send()'s tick() has resolved.
    (target.querySelector('.ask-turn.busy button') as HTMLButtonElement).click();
    await settle();
    expect(askTurns(target).querySelector('.cancelled-note')).toBeTruthy();
    expect(scrollTo).not.toHaveBeenCalled();
    unmount(host);
  });

  it('a cancelled question never scrolls when its late answer arrives', async () => {
    const answers = mockAsks();
    const { target, host } = render();
    await sendQuestion(target, 'question');
    (target.querySelector('.ask-turn.busy button') as HTMLButtonElement).click();
    flushSync();
    scrollTo.mockClear();
    answers[0]({ ...ANSWER, markdown: 'LATE' });
    await settle();
    expect(scrollTo).not.toHaveBeenCalled();
    unmount(host);
  });
});

// Task 2.6: Ask Jodd on Modal (size xl). A citation closes the modal INTO the
// note pane before App hears jodd:open-note, so Android Back from the note
// lands beneath the modal; on a phone the History rail is a sheet.
describe('AskJoddModal — on Modal', () => {
  const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
  const androidLayoutMode = layoutReadonly as unknown as Writable<'phone' | 'tablet' | 'tabletWide'>;
  const LIST = [{ id: 'conv-1', title: 'Why keep-both?', scope: { kind: 'all_accounts' }, updated_at: Date.now(), archived: false, turn_count: 1 }];
  const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); flushSync(); };
  const calls = (cmd: string) => invoke.mock.calls.filter(([c]) => c === cmd);
  const dialogs = (t: HTMLElement) => [...t.querySelectorAll('dialog')];
  const esc = () => new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
  let opened: CustomEvent[];
  const onOpen = (e: Event) => opened.push(e as CustomEvent);

  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string, args: any) => {
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      if (cmd === 'ask_jodd') return Promise.resolve(ANSWER);
      if (cmd === 'list_ask_conversations') return Promise.resolve(args.archived ? [] : LIST);
      if (cmd === 'load_ask_conversation') return new Promise(() => {});
      return Promise.resolve(null);
    });
    opened = [];
    window.addEventListener('jodd:open-note', onOpen);
    isAndroid.set(false);
    androidLayoutMode.set('phone');
  });
  afterEach(() => {
    window.removeEventListener('jodd:open-note', onOpen);
    isAndroid.set(false);
  });

  it('is an xl Modal; desktop keeps the rail beside the chat and has no History toggle', async () => {
    const { target, host } = render();
    await settle();
    const dialog = dialogs(target)[0];
    expect(dialog.dataset.size).toBe('xl');
    expect(dialog.dataset.presentation).toBe('card');
    expect(dialog.querySelector('.ask-body nav.ask-rail')).toBeTruthy();
    expect([...dialog.querySelectorAll('button')].some((b) => b.textContent?.trim() === 'History')).toBe(false);
    // Ask is the Modal's primary; there is no footer Close beside the header close.
    expect(dialog.querySelector('button.primary')?.textContent?.trim()).toBe('Ask');
    expect([...dialog.querySelectorAll('button')].some((b) => b.textContent?.trim() === 'Close')).toBe(false);
    unmount(host);
  });

  it('Esc while a question is in flight cancels it and keeps the modal; the next Esc closes', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      if (cmd === 'ask_jodd') return new Promise(() => {});
      return Promise.resolve(null);
    });
    const { target, host } = render();
    await sendQuestion(target, 'pending question');
    dialogs(target)[0].dispatchEvent(esc());
    flushSync();
    expect(calls('cancel_ask')).toHaveLength(1);
    expect(calls('end_ask')).toHaveLength(0);
    expect(target.querySelector('.ask-modal')).toBeTruthy();
    dialogs(target)[0].dispatchEvent(esc());
    flushSync();
    expect(target.querySelector('.ask-modal')).toBeNull();
    expect(calls('end_ask')).toHaveLength(1);
    unmount(host);
  });

  it('a citation closes the modal and opens the note', async () => {
    const { target, host } = render();
    await sendQuestion(target, 'How do we handle conflicts?');
    (askTurns(target).querySelector('.cite-chip') as HTMLElement).click();
    flushSync();
    expect(target.querySelector('.ask-modal')).toBeNull();
    expect(opened.map((e) => e.detail)).toEqual([{ uuid: 'aabbccdd-0000-0000-0000-000000000000', accountId: 'a@x' }]);
    unmount(host);
  });

  describe('Android phone', () => {
    beforeEach(() => {
      isAndroid.set(true);
      androidLayoutMode.set('phone');
      window.addEventListener('popstate', handlePopState);
      history.replaceState(null, '', location.href); // a fresh page: no stack for init to unwind
      initPhoneNavHistory();
    });
    afterEach(async () => {
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
      window.removeEventListener('popstate', handlePopState);
    });

    it('phone citation replaces overlay entry', async () => {
      const { target, host } = render();
      expect(dialogs(target)[0].dataset.presentation).toBe('page');
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeTruthy());
      const depth = depthFromHistoryState(history.state);
      const length = history.length;
      await sendQuestion(target, 'How do we handle conflicts?');
      let stateAtEvent: unknown = 'not dispatched';
      const seeState = () => { stateAtEvent = history.state; };
      window.addEventListener('jodd:open-note', seeState, { once: true });
      (askTurns(target).querySelector('.cite-chip') as HTMLElement).click();
      flushSync();
      // closeInto ran before App heard the event: the entry is already the note's.
      expect(stateAtEvent).toEqual({ pane: 'note', depth });
      expect((history.state as { overlay?: string }).overlay).toBeUndefined();
      expect((history.state as { pane: string }).pane).toBe('note');
      expect(depthFromHistoryState(history.state)).toBe(depth);
      expect(history.length).toBe(length);
      expect(target.querySelector('.ask-modal')).toBeNull();
      expect(opened).toHaveLength(1);
      unmount(host);
    });

    it('phone history opens as sheet', async () => {
      const { target, host } = render();
      await settle();
      const page = dialogs(target)[0];
      // No rail beside the chat on a phone.
      expect(page.querySelector('nav.ask-rail')).toBeNull();
      const toggle = [...page.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'History')!;
      expect(toggle.closest('header')).toBeTruthy();
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeTruthy());
      const pageEntry = overlayFromHistoryState(history.state);
      toggle.click();
      await settle();
      const sheet = dialogs(target).find((d) => d.dataset.presentation === 'sheet')!;
      expect(sheet).toBeTruthy();
      expect(sheet.querySelector('h2')?.textContent).toBe('History');
      expect(sheet.querySelector('nav.ask-rail .ask-row[data-id="conv-1"]')).toBeTruthy();
      // The sheet owns its own history entry above the page's.
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).not.toBe(pageEntry));
      // Picking a conversation closes the sheet and leaves the page open.
      (sheet.querySelector('.ask-row[data-id="conv-1"] .ask-row-title') as HTMLElement).click();
      await settle();
      expect(dialogs(target).some((d) => d.dataset.presentation === 'sheet')).toBe(false);
      expect(target.querySelector('.ask-modal')).toBeTruthy();
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBe(pageEntry));
      // Back closes the page too.
      history.back();
      await vi.waitFor(() => expect(target.querySelector('.ask-modal')).toBeNull());
      unmount(host);
    });

    it('Back closes the History sheet first, then the page', async () => {
      const { target, host } = render();
      await settle();
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeTruthy());
      [...dialogs(target)[0].querySelectorAll('button')].find((b) => b.textContent?.trim() === 'History')!.click();
      await settle();
      expect(dialogs(target).some((d) => d.dataset.presentation === 'sheet')).toBe(true);
      history.back();
      await vi.waitFor(() => expect(dialogs(target).some((d) => d.dataset.presentation === 'sheet')).toBe(false));
      expect(target.querySelector('.ask-modal')).toBeTruthy();
      history.back();
      await vi.waitFor(() => expect(target.querySelector('.ask-modal')).toBeNull());
      unmount(host);
    });
  });
});

describe('AskJoddModal — opened from a context menu with a scope', () => {
  const NOTE = { account_id: 'a@x', uuid: 'u1', title: 'Plan' };
  async function settle() { for (let i = 0; i < 15; i++) await Promise.resolve(); flushSync(); }
  const calls = (cmd: string) => invoke.mock.calls.filter((c) => c[0] === cmd);
  const box = (t: HTMLElement, label: string) =>
    Array.from(t.querySelectorAll('label')).find((l) => l.textContent?.includes(label))?.querySelector('input') as HTMLInputElement | undefined;

  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.resolve({ ...SESSION, scope: 'a@x · Plan' });
      if (cmd === 'list_ask_conversations') return Promise.resolve([]);
      return Promise.resolve(null);
    });
  });
  afterEach(() => { pendingAskScope.set(null); });

  it('starts a conversation locked to the note, and consumes the pending scope', async () => {
    const { target, host } = render();
    await settle();
    openAskWith(noteScope(NOTE));
    await settle();
    expect(calls('begin_ask').at(-1)?.[1]).toEqual({ scope: { kind: 'note', ...NOTE, links: false, tags: false, depth: 1 } });
    expect(target.querySelector('.scope-locked')?.textContent).toContain('a@x · Plan');
    expect(target.querySelector('select[aria-label="Scope"]')).toBeNull();
    expect(get(pendingAskScope)).toBeNull();
    unmount(host);
  });

  it('shows the related-notes controls, Depth disabled until a box is ticked', async () => {
    const { target, host } = render();
    openAskWith(noteScope(NOTE));
    await settle();
    expect(box(target, 'Include linked notes')).toBeTruthy();
    expect(box(target, 'Include notes with the same tag')).toBeTruthy();
    const depth = target.querySelector('select[aria-label="Depth"]') as HTMLSelectElement;
    expect(depth.disabled).toBe(true);
    box(target, 'Include linked notes')!.click();
    await settle();
    expect(calls('begin_ask').at(-1)?.[1].scope).toMatchObject({ kind: 'note', links: true, tags: false, depth: 1 });
    expect((target.querySelector('select[aria-label="Depth"]') as HTMLSelectElement).disabled).toBe(false);
    unmount(host);
  });

  it('says exactly what a note scope sends', async () => {
    const { target, host } = render();
    openAskWith(noteScope(NOTE));
    await settle();
    const hint = target.querySelector('.hint')?.textContent?.replace(/\s+/g, ' ') ?? '';
    expect(hint).toContain("Sends your questions, eligible conversation history and this note's full text (up to 120,000 characters) on every question.");
    expect(hint).not.toContain('Also sends titles');
    box(target, 'Include notes with the same tag')!.click();
    await settle();
    const after = target.querySelector('.hint')?.textContent?.replace(/\s+/g, ' ') ?? '';
    expect(after).toContain('Also sends titles, tags and folders of the related notes, then the bodies of those the AI selects.');
    unmount(host);
  });

  it('replaces a loaded conversation with a fresh one in the menu scope', async () => {
    const STORED = {
      id: 'conv-1', title: 'Old', scope: { kind: 'all_accounts' }, destination: SESSION.destination,
      created_at: 1, updated_at: 2, archived: false,
      turns: [{ seq: 1, question: 'Old q', answer_markdown: 'OLDANSWER', cited: [], read: [], created_at: 1,
        stats: { notes_in_scope: 1, notes_considered: 1, notes_used: 1, trimmed: false, dropped_citations: 0 } }],
    };
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'begin_ask') return Promise.resolve(SESSION);
      if (cmd === 'list_ask_conversations') return Promise.resolve([{ id: 'conv-1', title: 'Old', scope: { kind: 'all_accounts' }, updated_at: Date.now(), archived: false, turn_count: 1 }]);
      if (cmd === 'load_ask_conversation') return Promise.resolve(STORED);
      if (cmd === 'resume_ask') return Promise.resolve({ kind: 'ready', session: { ...SESSION, session_id: 'resumed' } });
      return Promise.resolve(null);
    });
    const { target, host } = render();
    await settle();
    (target.querySelector('.ask-row[data-id="conv-1"] .ask-row-title') as HTMLElement).click();
    await settle();
    expect(askTurns(target).textContent).toContain('OLDANSWER');
    openAskWith({ kind: 'folder', account_id: 'a@x', label: 'Notes/Work' });
    await settle();
    expect(askTurns(target).textContent).not.toContain('OLDANSWER');
    expect(calls('begin_ask').at(-1)?.[1]).toEqual({ scope: { kind: 'folder', account_id: 'a@x', label: 'Notes/Work' } });
    unmount(host);
  });
});
