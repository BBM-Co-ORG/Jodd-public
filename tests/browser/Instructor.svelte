<script lang="ts">
  import NoteList from '../../src/lib/components/NoteList.svelte';
  import NoteEditor from '../../src/lib/components/NoteEditor.svelte';
  import LessonExtractModal from '../../src/lib/components/LessonExtractModal.svelte';
  import AiReceipts from '../../src/lib/components/AiReceipts.svelte';
  import {selectedFolder, selectedNote, searchQuery} from '../../src/lib/stores/notes';
  import comparison from '../evals/meeting-actions-v1/comparison.json';
  import replay from './teaching-replay.json';
  let {setScenario, choose, searchState, platform}: {setScenario:(s:string)=>void;choose:()=>void;searchState:(s:string)=>void;platform:string} = $props();
  let scenario = $state('th-01');
  let open = $state(false);
  let section = $state('editor');
  let listWidth = $state(300);
  const names:Record<string,string> = {'th-01':'Grounded meeting synthesis','th-02':'Missing owner and date','th-10':'Incomplete source — withhold commitments',policy:'Policy denial',budget:'Budget denial'};
  const lesson = $derived(replay.lessons.find(l=>l.id===scenario));
  function select(value:string) {scenario=value;setScenario(value);}
</script>
<svelte:head><title>Jodd · Instructor replay</title></svelte:head>
<header><p class="eyebrow">JODD / TEACHING LAB</p><h1>Sources before certainty</h1>
<p>Replay · synthetic responses · no model performance claim</p>
<p>Recorded: local Rust validators, permission and budget checks. Live provider: not run. Usage, cost, human acceptance and time saved: unknown.</p>
<p>{platform === 'android' ? 'Mobile platform mode simulated in Chrome; not an Android device test.' : 'Desktop platform mode; viewport resizing is not Android.'}</p>
</header>
<main>
<section class="lesson">
<h2>1 · Find without AI</h2><p>Production note list with synthetic local IPC responses. Search “ประชุม”; exact folder scope remains separate from recursive Ask. This browser fixture does not execute SQLite FTS.</p>
<div class="controls"><button onclick={()=>{section='search';selectedFolder.set('Notes/Jodd-Demo');searchQuery.set('ประชุม');}}>Try exact search</button><button onclick={()=>{section='search';searchState('error');searchQuery.set('unavailable');}}>Search error</button><button onclick={()=>{section='search';searchState('hold');searchQuery.set('waiting');}}>Hold search</button><button onclick={()=>searchState('ready')}>Release / restore search</button><button onclick={()=>{section='search';searchQuery.set('no-matches');}}>No matches</button></div>
<h2>2 · Inspect before accepting</h2>
<label>Lesson <select value={scenario} onchange={e=>select(e.currentTarget.value)}>{#each Object.entries(names) as [id,name]}<option value={id}>{name}</option>{/each}</select></label>
{#if lesson}<pre>{lesson.source}</pre>{:else}<p>{scenario==='policy' ? 'AI data permission disabled: refuse before constructing a provider.' : 'The complete request exceeds the in-memory workflow allowance: no attempt dispatched.'}</p>{/if}
<button onclick={()=>open=true}>Open production Action Items</button>
<p>In the dialog select Action items and paste the source above. Only an exact fixture match is supported. Apply is deliberately unavailable in this replay: a recorded receipt grants no write eligibility.</p>
<p>Quote validation locates a passage. It does not prove intent, ownership or that a proposal became a commitment. Review/correction remains necessary.</p>
{#key scenario}<AiReceipts requestId={scenario}/>{/key}
<h2>3 · The regression behind the change</h2>
<p><code>packageH.test.ts</code>: four intended-behavior failures across two red runs, passing after their fixes. The mounted test keeps pending Thai edits when browsing an empty folder, explicitly closes and saves against the old account, then prevents a late reply from replacing the new editor. Another regression keeps the newest closed draft when an older save rekeys and its queued successor fails.</p>
<p><code>policy::tests::direct_command_admission_denies_before_provider_construction</code> and <code>budget::tests::concurrent_admission_reserves_whole_workflow_once</code> exercise the Rust boundary. Browser replay is not proof of native IPC enforcement.</p>
<details><summary>Comparison and measurement protocol</summary><p>{comparison.status}. Same 12 held-out IDs out of 36 synthetic cases; labels are agent-authored, human review pending. No route/default selected.</p><p>Before measuring savings: independent human labels; approved route and budget; fixed corpus/prompt; manual/non-AI baseline; repeated runs; record failures, review and correction time, retries/enrichment, usage provenance and cost per human-accepted result. Unknown is not zero.</p><p>See docs/TEACHING-DEMO.md and tests/evals/meeting-actions-v1/comparison.json.</p></details>
</section>
<section class="workspace" aria-label="Production components">
<div class="controls"><button onclick={()=>section='editor'}>Editor</button><button onclick={()=>section='search'}>Note list</button><button onclick={()=>{selectedFolder.set('Notes/ว่าง');searchQuery.set('');section='editor';}}>Browse empty folder</button><button onclick={()=>{choose();section='editor';}}>Reopen demo note</button></div>
<div hidden={section!=='search'} bind:clientWidth={listWidth}><NoteList width={listWidth}/></div>
<div hidden={section!=='editor'}><NoteEditor/></div>
</section>
</main>
<LessonExtractModal bind:open/>
<style>
:global(body){margin:0;background:var(--surface-editor);color:var(--text);font-family:'IBM Plex Sans Thai',system-ui,sans-serif}
:global(*){box-sizing:border-box}:global(button,input,select,textarea){color:inherit}:global(:focus-visible){outline:2px solid var(--focus);outline-offset:2px}header{padding:24px clamp(16px,4vw,48px);background:var(--surface-panel);border-bottom:1px solid var(--border)}h1{font-size:clamp(26px,4vw,42px);margin:4px 0}header p{max-width:900px}.eyebrow{font-size:12px;letter-spacing:0.16em;color:var(--text-muted)}main{display:grid;grid-template-columns:minmax(0,1fr);gap:16px;padding:16px}section{min-width:0}.lesson{padding:4px 12px;line-height:1.65}.workspace{border:1px solid var(--border);background:var(--surface-panel)}.controls{display:flex;flex-wrap:wrap;gap:8px;padding:8px}button,select{font:inherit;padding:8px 12px;max-width:100%;border:1px solid var(--border);border-radius:6px;background:var(--surface-panel);color:var(--text);cursor:pointer}pre{white-space:pre-wrap;overflow-wrap:anywhere;padding:12px;border-left:3px solid var(--text-muted);background:var(--surface-panel);font:inherit}p,code{overflow-wrap:anywhere}h2{font-size:20px;margin-top:24px}details{padding:12px 0}summary{cursor:pointer}button:focus-visible,select:focus-visible,summary:focus-visible{outline:2px solid var(--text);outline-offset:2px}:global(.workspace .editor-pane){height:760px}:global(.workspace .note-list){width:100%;height:640px}:global(.workspace [hidden]){display:none!important}
@media(min-width:1100px){main{grid-template-columns:minmax(0,0.85fr) minmax(0,1.15fr)}}
</style>
