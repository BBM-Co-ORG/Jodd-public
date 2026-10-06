import { mount } from 'svelte';
import Modal from '../../src/lib/components/LessonExtractModal.svelte';
import { currentAccount, notes, selectedNote } from '../../src/lib/stores/notes';
import '../../src/styles/tokens.css';
const calls: { command: string; args: any }[] = [];
const callbacks = new Map<number, Function>();
let next = 1;
const target = { uuid:'target',account_id:'synthetic',id:'',title:'Original meeting',body_html:'<p>Keep original content</p>',label:'Notes',local_version:3 };
let rejectApply = false;
Object.defineProperty(window, '__TAURI_INTERNALS__', { value: {
  transformCallback: (fn:Function) => { const id=next++;callbacks.set(id,fn);return id; },
  unregisterCallback: (id:number) => callbacks.delete(id),
  invoke: async (command: string,args:any) => {
    calls.push({command,args});
    switch(command) {
      case 'plugin:event|listen': return 1;
      case 'plugin:event|unlisten': return;
      case 'list_ai_receipts': return [];
      case 'check_duplicate_citations': case 'list_note_tags': return [];
      case 'analyze_ingest_sources': return {sources:[],context_text:'',mostly_urls:false};
      case 'search_notes': return [target];
      case 'preview_action_items': return {ai_result_id:'fixture-eligibility',title:'Meeting actions — draft',before_html:args.targetUuid?target.body_html:null,
        body_html:'<p>Draft for review. Quote matching verifies location, not meaning.</p><h2>Decisions</h2><p>Not specified in the supplied source.</p><h2>Actions</h2><p>ส่ง QA checklist — Owner: Not specified; Due: Not specified <a href="#meeting-fixture-1">Evidence 1</a></p><h2>Unresolved / missing information</h2><p>ผู้รับผิดชอบและกำหนดส่งยังไม่ระบุ</p><h2>Evidence passages</h2><p id="meeting-fixture-1"><strong>Passage 1</strong>: ตกลงให้ส่ง QA checklist ยังไม่ได้กำหนดผู้รับผิดชอบหรือวันส่ง</p>'};
      case 'apply_action_items': if(rejectApply) throw new Error('Target changed since preview; generate a new draft before applying.'); return {uuid:'new',label:'Notes'};
      case 'discard_action_items': case 'cancel_extraction': return;
      case 'list_cached_notes_in_folder': return [{...target,uuid:'new'}];
      case 'get_ai_limits': return {automatic_enrichment:false};
      default: throw new Error(`Forbidden synthetic IPC: ${command}`);
    }
  }
}});
currentAccount.set('synthetic');notes.set([]);selectedNote.set(null);
Object.assign(window,{packageFFixture:{calls,refuseApply:()=>rejectApply=true}});
mount(Modal,{target:document.getElementById('app')!,props:{open:true}});
