// @vitest-environment jsdom
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import { currentAccount, notes, selectedNote, selectedFolder } from '../stores/notes';
const invoke = vi.fn();
let policyChanged = () => {};
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a), Channel: class {} }));
vi.mock('@tauri-apps/api/event', () => ({ listen: async (_: string, callback: () => void) => { policyChanged = callback; return () => {}; } }));
import Modal from './LessonExtractModal.svelte';
let host: HTMLElement;
let app: ReturnType<typeof mount>;
const draft = { ai_result_id: 'eligibility-not-receipt', title: 'Meeting draft', body_html: '<p>Owner: Not specified; Due: Not specified</p><a href="#meeting-x-1">Evidence 1</a><p id="meeting-x-1">ส่ง draft</p>', before_html: null as string | null };
const target = { uuid: 'target', account_id: 'a', id: '', title: 'Target', body_html: '<p>Keep original</p>', label: 'Notes', local_version: 3 };
const saved = { ...target, uuid: 'new', title: 'Draft' };
async function settle() { for (let i=0;i<15;i++) { await tick(); await Promise.resolve(); } flushSync(); }
function button(name: string) { return [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === name)!; }
function input(el: HTMLInputElement | HTMLTextAreaElement, value: string) { el.value=value; el.dispatchEvent(new Event('input',{bubbles:true})); flushSync(); }
function defer<T>() { let resolve!: (v:T)=>void; const promise=new Promise<T>(r=>resolve=r); return {promise,resolve}; }
const commands = () => invoke.mock.calls.map(c=>c[0]);
beforeEach(async () => {
  currentAccount.set('a'); notes.set([]); selectedNote.set(null); selectedFolder.set('Notes');
  invoke.mockReset();
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd==='preview_action_items') return structuredClone(draft);
    if (cmd==='apply_action_items') return {uuid:'new',label:'Notes'};
    if (cmd==='list_cached_notes_in_folder') return [saved];
    if (cmd==='get_ai_limits') return {automatic_enrichment:false};
    if (cmd==='analyze_ingest_sources') return {sources:[],context_text:'',mostly_urls:false};
    if (cmd==='search_notes') return [target];
    return [];
  });
  host=document.createElement('div'); document.body.append(host);
  app=mount(Modal,{target:host,props:{open:true}}); await settle();
  button('Action items').click(); await settle();
  input(host.querySelector('textarea')!,'ส่ง draft');
});
afterEach(async () => { await unmount(app); host.remove(); vi.useRealTimers(); });
it('previews a separate draft with evidence and requires a second explicit save',async()=>{
  button('Preview meeting actions').click(); await settle();
  expect(commands()).toContain('preview_action_items');
  expect(commands()).not.toContain('apply_action_items');
  expect(commands()).not.toContain('run_llm_workflow');
  expect(host.textContent).toContain('Nothing has been saved');
  expect(host.querySelector('a')?.getAttribute('href')).toBe('#meeting-x-1');
  button('Save separate draft').click(); await settle();
  expect(invoke.mock.calls.find(c=>c[0]==='apply_action_items')?.[1]).toEqual({accountId:'a',aiResultId:'eligibility-not-receipt'});
  expect(commands()).not.toContain('suggest_wiki_links');
});
it('shows append-only diff and passes target identity for the backend snapshot',async()=>{
  button('Append to existing note').click(); await settle();
  input(host.querySelector('input[placeholder="Search notes by title or content..."]')!,'Target');
  await new Promise(r=>setTimeout(r,180)); await settle();
  [...host.querySelectorAll('button')].find(b=>b.textContent?.includes('Target') && b.textContent?.includes('Notes'))!.click();
  await settle();
  const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((cmd,...args)=>cmd==='preview_action_items'?Promise.resolve({...draft,before_html:'<p>Keep original</p>'}):original(cmd,...args));
  button('Preview meeting actions').click(); await settle();
  expect(invoke.mock.calls.find(c=>c[0]==='preview_action_items')?.[1].targetUuid).toBe('target');
  expect(host.textContent).toContain('Append-only diff');
  expect(host.textContent).toContain('Keep original');
  expect(commands()).not.toContain('append_llm_workflow_note');
  button('Confirm append').click(); await settle();
  expect(commands()).toContain('apply_action_items');
});
it('discard returns to intact source without saving',async()=>{
  button('Preview meeting actions').click(); await settle();
  button('Back to source').click(); await settle();
  expect(host.querySelector('textarea')?.value).toBe('ส่ง draft');
  expect(commands()).toContain('discard_action_items');
  expect(commands()).not.toContain('apply_action_items');
});
it('permission change discards a pending result and a late reply',async()=>{
  const pending=defer<typeof draft>();const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((cmd,...args)=>cmd==='preview_action_items'?pending.promise:original(cmd,...args));
  button('Preview meeting actions').click(); await settle();
  policyChanged(); await settle(); pending.resolve(draft); await settle();
  expect(host.textContent).not.toContain('Save separate draft');
  expect(commands()).toContain('discard_action_items');
  expect(commands()).not.toContain('apply_action_items');
});
it('account switch cannot repopulate the previous account preview',async()=>{
  button('Preview meeting actions').click(); await settle();
  currentAccount.set('b'); await settle();
  expect(host.textContent).not.toContain('Save separate draft');
  expect(commands()).toContain('discard_action_items');
});
it('version refusal keeps source and asks for a fresh preview',async()=>{
  const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((cmd,...args)=>cmd==='apply_action_items'?Promise.reject('Target changed since preview'):original(cmd,...args));
  button('Preview meeting actions').click(); await settle();button('Save separate draft').click();await settle();
  expect(host.textContent).toContain('Target changed since preview');
  expect(host.querySelector('textarea')?.value).toBe('ส่ง draft');
});
it('cancellation during advisory checks prevents dispatch',async()=>{
  const pending=defer<unknown[]>();const original=invoke.getMockImplementation()!;
  invoke.mockImplementation((cmd,...args)=>cmd==='check_duplicate_citations'?pending.promise:original(cmd,...args));
  button('Preview meeting actions').click();await settle();button('Cancel extraction').click();await settle();
  pending.resolve([]);await settle();
  expect(commands()).not.toContain('preview_action_items');
});
