<script lang="ts">
  import NoteList from '../../src/lib/components/NoteList.svelte';
  import NoteEditor from '../../src/lib/components/NoteEditor.svelte';
  import { currentAccount, selectedNote, notes, searchQuery } from '../../src/lib/stores/notes';
  function choose(id: string) { currentAccount.set(id); searchQuery.set(''); selectedNote.set($notes.find(n => n.account_id === id) ?? null); }
</script>
<header><strong>Jodd · Browser walkthrough</strong><span>Synthetic data · no real AI or remote sync</span></header>
<nav><button onclick={() => choose('demo-a')}>Account A</button><button onclick={() => choose('demo-b')}>Account B</button><a href="./package-a.html">A · confirmation</a><a href="./package-b.html">B · Ask privacy</a><a href="./package-c.html">C · sync states</a></nav>
<main><NoteList width={300}/><section><NoteEditor/></section></main>
<style>
:global(body){margin:0;font-family:system-ui;color:var(--text);background:var(--surface-panel)}
header,nav{display:flex;gap:20px;align-items:center;padding:12px 20px;border-bottom:1px solid var(--border)}header span{font-size:13px}nav a{color:var(--text)}main{display:flex;height:calc(100vh - 112px)}section{flex:1;min-width:0}
</style>
