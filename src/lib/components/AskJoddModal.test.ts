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
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import AskJoddModal from './AskJoddModal.svelte';
import { currentAccount } from '../stores/notes';

const invoke = vi.fn();
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
};

function render() {
  const target = document.createElement('div');
  document.body.appendChild(target);
  // `currentAccount` starts null, and the default 'account' scope is
  // account-anchored — askScope() returns null for it, so the Ask button is
  // (correctly) disabled and nothing can be sent. Every test below is about
  // what happens AFTER a question goes out, so each needs a selected account.
  currentAccount.set('a@x');
  const host = mount(AskJoddModal, { target, props: { open: true } });
  flushSync();
  return { target, host };
}

function askTurns(target: HTMLElement): HTMLElement {
  return target.querySelector('.ask-turns') as HTMLElement;
}

async function sendQuestion(target: HTMLElement, text: string) {
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
  // seven fields asserted in `ask_wire_shapes_are_stable`
  // (src-tauri/src/ask/mod.rs); the two lists are the contract, and a field
  // added on one side without the other shows up as a diff between them.
  it('fixture carries exactly the seven AskAnswer fields', () => {
    expect(Object.keys(ANSWER).sort()).toEqual([
      'cited',
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
      return Promise.resolve([]);
    });

    const { target, host } = render();
    await sendQuestion(target, 'first question');

    // Second call still rejects, but we only care what turns/wire it sent.
    await sendQuestion(target, 'second question');

    const calls = invoke.mock.calls.filter((c) => c[0] === 'ask_jodd');
    expect(calls.length).toBe(2);
    const secondCallTurns = calls[1][1].turns as { role: string; content: string }[];
    // Only the two user questions should be present — no assistant error
    // turn from the first round.
    expect(secondCallTurns.some((t) => t.role === 'assistant')).toBe(false);
    expect(secondCallTurns.filter((t) => t.role === 'user').length).toBe(2);

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
      if (cmd === 'get_app_llm_config') {
        return Promise.resolve({ cfg: { llm: { provider } }, has_api_key: false });
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

    expect(askTurns(target).textContent).toContain('needs an LLM provider');
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

    expect(askTurns(target).textContent).not.toContain('needs an LLM provider');
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(false);

    unmount(host);
  });

  it('fails open when the probe itself errors, rather than locking the user out', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_app_llm_config') return Promise.reject(new Error('config unreadable'));
      return Promise.resolve([]);
    });
    const { target, host } = render();
    await settle();

    expect(askTurns(target).textContent).not.toContain('needs an LLM provider');
    expect((target.querySelector('textarea.field') as HTMLTextAreaElement).disabled).toBe(false);

    unmount(host);
  });
});

describe('AskJoddModal — unsendable scope', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_app_llm_config') {
        return Promise.resolve({ cfg: { llm: { provider: 'agent_cli' } }, has_api_key: false });
      }
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
    expect(calls[0][1].scope).toEqual({ kind: 'all_accounts' });

    unmount(host);
  });
});
