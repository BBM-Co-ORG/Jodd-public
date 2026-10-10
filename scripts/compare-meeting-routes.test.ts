import { expect, it } from 'vitest';
// @ts-expect-error local offline CLI module
import { compare } from './compare-meeting-routes.mjs';
import corpus from '../tests/evals/meeting-actions-v1/cases.json';
import report from '../tests/evals/meeting-actions-v1/report.json';
const provenance = {corpus_sha256:'corpus',prompt_sha256:'prompt'};
const route = {id:'synthetic',approval_reference:'fixture only',provider:'fixture',model:'none',configuration:'offline',collected_at:'2026-09-20',outputs:'synthetic.json',repeat:1};
it('leaves unapproved comparison and human measurements pending', () => {
 const result=compare({routes:[]},()=>{throw Error('must not collect');},corpus,provenance);
 expect(result.routes).toEqual([]); expect(result.held_out_ids).toHaveLength(12);
 expect(result.cost_per_accepted_result).toBeNull(); expect(result.selected_default).toBeNull();
});
it('uses identical held-out IDs and retains every failure without reporting scoring latency as live', () => {
 const failed=structuredClone(report);const held=failed.cases.find(c=>c.split==='held_out')!;held.pass=false;
 const result=compare({...provenance,routes:[route,{...route,id:'other'}]},()=>failed,corpus,provenance);
 expect(result.routes[0].failures).toEqual([held.id]);expect(result.routes[0].held_out_n).toBe(12);
 expect(result.routes[0].cases).toEqual(result.routes[1].cases);expect(result.routes[0].cases[0]).not.toHaveProperty('latency_ms');
});
it('refuses mismatched corpus, incomplete reports and absent approval provenance', () => {
 expect(()=>compare({routes:[route]},()=>report,corpus,provenance)).toThrow('digest mismatch');
 expect(()=>compare({...provenance,routes:[route]},()=>({...report,cases:[]}),corpus,provenance)).toThrow('Incomplete');
 expect(()=>compare({...provenance,routes:[{...route,approval_reference:''}]},()=>report,corpus,provenance)).toThrow('approval_reference');
});

it('keeps the recorded teaching artifact tied to its production contracts and no provider attempts', async () => {
 const {readFileSync}=await import('node:fs');const {createHash}=await import('node:crypto');
 const replay=JSON.parse(readFileSync('tests/browser/teaching-replay.json','utf8'));
 for(const [field,path] of Object.entries({corpus_sha256:'tests/evals/meeting-actions-v1/cases.json',meeting_contract_sha256:'src-tauri/src/llm/meeting.rs',policy_sha256:'src-tauri/src/llm/policy.rs',budget_sha256:'src-tauri/src/llm/budget.rs'})) {
  expect(replay[field]).toBe(createHash('sha256').update(readFileSync(path)).digest('hex'));
 }
 for(const row of [...replay.lessons,replay.policy,replay.budget]) for(const receipt of row.receipts) expect(receipt.calls).toEqual([]);
 expect(replay.policy.error).toContain('disabled');expect(replay.budget.error).toContain('limit reached');
});
