import { mount } from 'svelte';
import Instructor from './Instructor.svelte';
import replay from './teaching-replay.json';
import { accounts, currentAccount, notes, selectedNote, selectedFolder, capabilitiesByAccount } from '../../src/lib/stores/notes';
import type { Note } from '../../src/lib/types';
import '../../src/styles/tokens.css';
import '@fontsource/ibm-plex-sans-thai/400.css';
// Browser-only entry point. Never import into src/main.ts or ship as an app route.
if ((window as any).__TAURI_INTERNALS__) throw Error('Teaching fixture cannot run in native Tauri');
const platform = new URLSearchParams(location.search).get('platform') === 'android' ? 'android' : 'macos';
const row:Note = {account_id:'synthetic',uuid:'demo',id:'demo',title:'ประชุม — ข้อสรุปและรายการติดตามสำหรับทีมที่มีชื่อยาวมาก',body_html:'<body><p>ประชุมวางแผน: มาลีจะส่งรายการตรวจสอบภายในวันศุกร์</p></body>',label:'Notes/Jodd-Demo',date:'2026-09-20',local_version:1};
let scenario = 'th-01'; let state = 'ready';
let pendingSearch: (()=>void)[] = [];
let draft = false;
const requestScenarios = new Map<string,string>();
const callbacks = new Map<number,Function>();let seq=0;
const calls:{command:string;args:any}[]=[];
function receiptRows(id:string) {
 if(id==='policy'||id==='budget') return replay[id].receipts;
 return replay.lessons.find(l=>l.id===id)?.receipts ?? [];
}
Object.defineProperty(window,'__TAURI_INTERNALS__',{value:{
 transformCallback:(fn:Function)=>{callbacks.set(++seq,fn);return seq;},unregisterCallback:(id:number)=>callbacks.delete(id),
 invoke:async(command:string,args:any={})=>{
  calls.push({command,args});
  switch(command) {
   case 'platform_name':return platform;
   case 'plugin:event|listen':return ++seq;
   case 'plugin:event|unlisten':return;
   case 'list_ai_receipts':return structuredClone(receiptRows(requestScenarios.get(args.requestId) ?? args.requestId));
   case 'get_ai_limits':return {automatic_enrichment:false};
   case 'list_note_tags':case 'check_duplicate_citations':case 'get_note_attachments':return [];
   case 'note_connections':return {outgoing:[],backlinks:[]};
   case 'note_citations':return Array.from({length:9},(_,i)=>`https://example.test/${encodeURIComponent('หลักฐานการประชุม')}/${i+1}`);
   case 'note_persistence':return {account_id:row.account_id,uuid:row.uuid,local_version:row.local_version,sync_state:'dirty',push_blocked_reason:null};
   case 'save_note': if(args.accountId!=='synthetic'||args.existingUuid!==row.uuid) throw Error('Unsupported save identity');
    Object.assign(row,{title:args.title,body_html:args.bodyHtml,local_version:row.local_version!+1});return {id:row.id,uuid:row.uuid,local_version:row.local_version};
   case 'search_notes':
    if(state==='hold') await new Promise<void>(r=>pendingSearch.push(r));
    if(state==='error') throw Error('Synthetic search unavailable');
    return (!args.accountId||args.accountId==='synthetic')&&(!args.label||args.label===row.label)&&`${row.title} ${row.body_html}`.includes(args.query)?[{...row}]:[];
   case 'analyze_ingest_sources':return {sources:[],context_text:'',mostly_urls:false};
   case 'preview_action_items': {
    draft=false;
    requestScenarios.set(args.requestId,scenario);
    if(scenario==='policy'||scenario==='budget') throw Error(replay[scenario].error);
    const lesson=replay.lessons.find(l=>l.id===scenario);
    if(!lesson || args.sourceText!==lesson.source || args.accountId!=='synthetic' || args.targetUuid || args.sourceUuid) throw Error('Unsupported fixture input. No network fallback. Paste the exact synthetic lesson.');
    draft=true;return {ai_result_id:'synthetic-replay-not-write-authority',title:'Meeting actions — synthetic replay',before_html:null,body_html:lesson.body_html};
   }
   case 'apply_action_items':throw Error(draft?'Replay is read-only. A recorded receipt is not a backend-held review draft; no note was written.':'No eligible review draft');
   case 'discard_action_items':case 'cancel_extraction':draft=false;return;
   default:throw Error(`Unsupported teaching IPC: ${command}; no network fallback`);
  }
 }
}});
function choose(){selectedFolder.set(row.label);selectedNote.set({...row});}
accounts.set([{id:'synthetic',email:'Synthetic classroom',added_at:'2026-09-20'}]);currentAccount.set('synthetic');
capabilitiesByAccount.set({synthetic:{has_trash:false,writes:{notes:true,folders:false,relocate:false,sidecars:false}}});
notes.set([{...row}]);choose();
Object.assign(window,{packageHFixture:{calls,providerCalls:0}});
mount(Instructor,{target:document.getElementById('app')!,props:{platform,choose,setScenario:(s:string)=>{scenario=s;draft=false;},searchState:(s:string)=>{state=s;if(s!=='hold'){for(const resolve of pendingSearch) resolve();pendingSearch=[];}}}});
