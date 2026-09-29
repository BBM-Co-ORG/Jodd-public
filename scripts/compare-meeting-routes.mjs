// Offline only: this program can score local output files, never collect them.
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { resolve, dirname } from 'node:path';
const root = fileURLToPath(new URL('../', import.meta.url));
const digest = path => createHash('sha256').update(readFileSync(path)).digest('hex');
export function compare(manifest, score, corpus, provenance) {
  const ids = corpus.cases.filter(c => c.split === 'held_out').map(c => c.id);
  const routes = manifest.routes ?? [];
  if (routes.length && (manifest.corpus_sha256 !== provenance.corpus_sha256 || manifest.prompt_sha256 !== provenance.prompt_sha256)) throw new Error('Corpus/prompt digest mismatch');
  const seen = new Set();
  const results = routes.map(route => {
    if (!route.id || seen.has(route.id)) throw new Error('Missing or duplicate route ID');
    seen.add(route.id);
    for (const field of ['approval_reference', 'provider', 'model', 'configuration', 'collected_at', 'outputs']) {
      if (typeof route[field] !== 'string' || !route[field].trim()) throw new Error(`Missing route metadata: ${field}`);
    }
    if (!Number.isInteger(route.repeat) || route.repeat < 1) throw new Error('repeat must identify a collected run');
    const report = score(route);
    // The production evaluator scores ALL 36 cases; compare only the fixed 12.
    if (report.cases.length !== corpus.cases.length || new Set(report.cases.map(c => c.id)).size !== corpus.cases.length || corpus.cases.some(c => !report.cases.some(r => r.id === c.id))) throw new Error('Incomplete evaluator report');
    const held = ids.map(id => report.cases.find(c => c.id === id));
    return { ...route, outputs: undefined, evidence: 'supplied approval/configuration metadata; not independently verified',
      held_out_n: held.length, failures: held.filter(c => !c.pass).map(c => c.id),
      development_failures: report.cases.filter(c => c.split === 'development' && !c.pass).map(c => c.id),
      groups: report.groups.filter(g => g.split === 'held_out'), cases: held.map(({latency_ms, ...row}) => row),
      provider_latency: null, human_acceptance: {n:0, rate:null}, human_correction_time: null,
      total_cost_per_accepted_result: null, usage: 'unknown',
    };
  });
  return { mode:'offline comparison preparation; no provider invoked', ...provenance,
    corpus:corpus.version, corpus_n:corpus.cases.length, held_out_ids:ids,
    labels:'agent-authored synthetic; independent human review pending',
    status: results.length ? 'supplied outputs scored; human acceptance and live evidence pending' : 'pending approved routes and outputs',
    routes:results, selected_default:null, live_comparison:'pending', human_acceptance:{n:0,rate:null},
    cost_per_accepted_result:null, time_saved:null,
    limitation:'Quote matching does not prove intent or ownership. Reference equality is not human acceptance. Local scoring latency is not provider latency.' };
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const corpusPath = resolve(root,'tests/evals/meeting-actions-v1/cases.json');
  const corpus = JSON.parse(readFileSync(corpusPath,'utf8'));
  const provenance = {corpus_sha256:digest(corpusPath), prompt_sha256:digest(resolve(root,'src-tauri/src/llm/meeting.rs'))};
  const manifestPath = process.argv[2] ? resolve(process.argv[2]) : null;
  const manifest = manifestPath ? JSON.parse(readFileSync(manifestPath,'utf8')) : {routes:[]};
  const report = compare(manifest, route => {
    const path = resolve(dirname(manifestPath),route.outputs);
    const outputs = JSON.parse(readFileSync(path,'utf8'));
    if (corpus.cases.some(c => !Object.hasOwn(outputs,c.id))) throw new Error('Missing response IDs; no fixture fallback');
    const run = spawnSync('cargo',['run','--locked','-q','-p','jodd','--example','meeting_actions_eval',path], {cwd:root,encoding:'utf8',maxBuffer:10*1024*1024});
    if (run.error || ![0,1].includes(run.status)) throw new Error(run.error?.message ?? run.stderr);
    return JSON.parse(run.stdout);
  },corpus,provenance);
  console.log(JSON.stringify(report,null,2));
  if (report.routes.some(r => r.failures.length || r.development_failures.length)) process.exitCode=1;
}
