<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount, onDestroy, tick } from 'svelte';
  import { get } from 'svelte/store';
  import { selectedNote, notes, isSaving, error, currentAccount, markRecentlySaved, indexUpsertOnSave, indexRemoveOnDelete, tagsByAccount, setNoteTags, selectedFolder, searchQuery, accounts, accountDisplay } from '../stores/notes';
  import type { Note } from '../types';
  import { tryExitSpecialBlock, caretToEnd } from '../editor/exitSpecialBlock';
  import { tryFixFirstBlockEnter } from '../editor/splitFirstBlock';
  import { tryBackspaceMergeEmptyPrevious } from '../editor/backspaceMergeEmpty';
  import { shouldRenderExternalBody } from '../editorRenderDecision';

  let title = '';

  // ─── Tags (Jodd-local) ──────────────────────────────────────────────────
  // Tags are independent of the note body and the autosave flow — they're a
  // inline #hashtags in the body. The chip row reads from bodyTags; add/remove
  // optimistically patch the store (synchronous UI) then invoke, rolling back
  // on failure. The client normalization here mirrors the backend's
  // normalize_tag exactly, so the optimistic value equals what's stored.
  let tagInput = '';
  let tagInputFocused = false;
  let suggestIndex = -1; // -1 = nothing highlighted; Enter commits typed text
  // Tags now derive from the note BODY (inline #hashtags) — the round-trips-to-
  // Apple source of truth. bodyTags is re-parsed from the editor on note load
  // and on every input; the chip row reflects it. Adding a chip injects a
  // #hashtag into the body; removing strips it.
  let bodyTags: string[] = [];
  $: currentTags = $selectedNote ? bodyTags : [];

  // Autocomplete: existing tags for this account that match what's typed and
  // aren't already on the note. Empty input → show all available (focus to
  // browse). Recomputed every keystroke; the highlight resets so it never
  // points at a stale row.
  $: tagSuggestions = (() => {
    if (!$selectedNote) return [] as string[];
    const accountId = $selectedNote.account_id ?? $currentAccount;
    if (!accountId) return [] as string[];
    const own = new Set(bodyTags);
    const q = normalizeTagClient(tagInput);
    const all = ($tagsByAccount.get(accountId) ?? []).map((t) => t.tag).filter((t) => !own.has(t));
    const matches = q === '' ? all : all.filter((t) => t.includes(q));
    return matches.slice(0, 8);
  })();
  $: { void tagSuggestions; suggestIndex = -1; }

  // Mirrors the backend normalize_tag exactly: trim, lowercase, then drop
  // whitespace, control chars, and every '#'. Unicode-friendly (Thai/CJK/etc.
  // survive) — \p{White_Space} and \p{Cc} match Rust's is_whitespace()/
  // is_control().
  function normalizeTagClient(raw: string): string {
    return raw.trim().toLowerCase().replace(/[#\p{White_Space}\p{Cc}]/gu, '');
  }

  // Mirror of the Rust db::extract_hashtags: # at a word boundary, then
  // letters/digits/_/HYPHEN/combining-marks (\p{M} → Thai etc.), requiring a
  // letter (so #2024 isn't a tag). Lowercased, de-duplicated, first-seen order.
  //
  // Hyphen support added 2026-06-13 to match the Rust side (db.rs is_tag_word_char).
  // Compound kebab-case tags like #database-migrations and #local-first must parse
  // as one tag, not truncate at the dash. Word-boundary on the LEFT also excludes
  // hyphen so `foo-#bar` still recognizes #bar (hyphen before # is a boundary).
  function extractHashtags(text: string): string[] {
    const out: string[] = [];
    const seen = new Set<string>();
    const re = /(?:^|[^\p{L}\p{N}_-])#([\p{L}\p{N}_\-\p{M}]+)/gu;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) {
      const raw = m[1];
      if (!/\p{L}/u.test(raw)) continue;
      const norm = raw.toLowerCase();
      if (!seen.has(norm)) { seen.add(norm); out.push(norm); }
    }
    return out;
  }
  function refreshBodyTags() {
    const next = editorEl ? extractHashtags(editorEl.innerText || '') : [];
    const changed =
      next.length !== bodyTags.length || next.some((t, i) => t !== bodyTags[i]);
    bodyTags = next;
    // Keep the shared tag store in sync with the body LIVE — that store derives
    // tagsByAccount, which drives the sidebar Tags filter + note-list filtering.
    // Without this the sidebar only picked up new tags on the next app load.
    if (changed) {
      const note = get(selectedNote);
      if (note && !note.uuid.startsWith('tmp:')) {
        const accountId = note.account_id ?? get(currentAccount);
        if (accountId) setNoteTags(accountId, note.uuid, next);
      }
    }
  }
  function escapeRegex(s: string): string {
    return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  }

  // Shared add path used by both typing-then-Enter and picking a suggestion.
  // Injects the hashtag into the BODY (source of truth) and lets the normal
  // content save derive + round-trip it; no separate sidecar write.
  function addNormalizedTag(tag: string) {
    const note = $selectedNote;
    if (!note || !tag || !editorEl) return;
    if (note.uuid.startsWith('tmp:')) return; // hidden for tmp: notes anyway
    if (bodyTags.includes(tag)) return;
    const txt = editorEl.innerText || '';
    const sep = txt.length && !/\s$/.test(txt) ? ' ' : '';
    editorEl.appendChild(document.createTextNode(`${sep}#${tag}`));
    refreshBodyTags();
    onInput(); // optimistic store update + schedule the content save
  }

  function commitTag() {
    const tag = normalizeTagClient(tagInput);
    tagInput = '';
    suggestIndex = -1;
    if (tag) addNormalizedTag(tag);
  }

  function pickSuggestion(tag: string) {
    tagInput = '';
    suggestIndex = -1;
    addNormalizedTag(tag);
  }

  // Strip every occurrence of the hashtag from the body (inline + the trailing
  // tag line), then save. The lookahead keeps #work from also matching #working.
  function removeTag(tag: string) {
    const note = $selectedNote;
    if (!note || !editorEl) return;
    const re = new RegExp(
      `(^|[^\\p{L}\\p{N}_])#${escapeRegex(tag)}(?=[^\\p{L}\\p{N}_\\p{M}]|$)`,
      'giu',
    );
    for (const tn of gatherTextNodes(editorEl)) {
      const next = tn.data.replace(re, '$1');
      if (next !== tn.data) tn.data = next;
    }
    refreshBodyTags();
    onInput();
  }

  function onTagKeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (tagSuggestions.length) suggestIndex = (suggestIndex + 1) % tagSuggestions.length;
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (tagSuggestions.length) suggestIndex = (suggestIndex - 1 + tagSuggestions.length) % tagSuggestions.length;
    } else if (e.key === 'Enter') {
      e.preventDefault();
      // A highlighted suggestion wins; otherwise commit exactly what was typed.
      if (suggestIndex >= 0 && suggestIndex < tagSuggestions.length) pickSuggestion(tagSuggestions[suggestIndex]);
      else commitTag();
    } else if (e.key === 'Escape') {
      suggestIndex = -1;
      tagInputFocused = false;
    }
  }
  // Backlinks / outgoing links ([[wikilink]] graph) for the open note. Fetched
  // from the edges table on load + after each save; rendered in a footer panel.
  let connections: { outgoing: Note[]; backlinks: Note[] } = { outgoing: [], backlinks: [] };
  let connSeq = 0;
  async function refreshConnections() {
    const note = get(selectedNote);
    if (!note || note.uuid.startsWith('tmp:')) {
      connections = { outgoing: [], backlinks: [] };
      return;
    }
    const accountId = note.account_id ?? get(currentAccount);
    if (!accountId) return;
    const seq = ++connSeq;
    try {
      const res = await invoke<{ outgoing: Note[]; backlinks: Note[] }>('note_connections', {
        accountId,
        uuid: note.uuid,
      });
      if (seq === connSeq) connections = res;
    } catch (e) {
      if (seq === connSeq) connections = { outgoing: [], backlinks: [] };
    }
  }
  function openConnection(n: Note) {
    if (n.account_id && n.account_id !== get(currentAccount)) currentAccount.set(n.account_id);
    selectedNote.set(n);
  }

  // `[[` link picker. Typing `[[query` opens an autocomplete of notes; picking
  // one inserts `[[<slug>]]` (slug = title + stable uuid id) so the link is
  // unambiguous even across duplicate titles.
  type LinkCandidate = { uuid: string; title: string; label: string; slug: string };
  let linkPickerOpen = false;
  let linkQuery = '';
  let linkResults: LinkCandidate[] = [];
  let linkIndex = 0;
  let linkX = 0;
  let linkY = 0;
  let linkAnchor: { node: Text; start: number; end: number } | null = null;
  let linkSeq = 0;

  function closeLinkPicker() {
    linkPickerOpen = false;
    linkAnchor = null;
    linkResults = [];
  }

  // Detect `[[query` immediately before the caret (no closing ]] yet).
  function detectLinkPicker() {
    const sel = typeof window !== 'undefined' ? window.getSelection() : null;
    if (!sel || sel.rangeCount === 0 || !editorEl) return closeLinkPicker();
    const range = sel.getRangeAt(0);
    const node = range.startContainer;
    if (!range.collapsed || node.nodeType !== 3 || !editorEl.contains(node)) return closeLinkPicker();
    const before = (node as Text).data.slice(0, range.startOffset);
    const m = before.match(/\[\[([^\[\]\n]*)$/);
    if (!m) return closeLinkPicker();
    // Don't trigger when the caret is INSIDE an already-completed [[link]] (a
    // `]]` follows before any newline) — otherwise arrowing through an existing
    // link re-opens the picker and swallows the arrow keys.
    const after = (node as Text).data.slice(range.startOffset);
    if (/^[^[\n]*\]\]/.test(after)) return closeLinkPicker();
    linkQuery = m[1];
    linkAnchor = { node: node as Text, start: range.startOffset - m[0].length, end: range.startOffset };
    const rect = range.getBoundingClientRect();
    linkX = rect.left || 80;
    linkY = (rect.bottom || 120) + 2;
    linkPickerOpen = true;
    fetchLinkResults();
  }

  async function fetchLinkResults() {
    const note = get(selectedNote);
    const accountId = note?.account_id ?? get(currentAccount);
    if (!accountId) { linkResults = []; return; }
    const seq = ++linkSeq;
    try {
      const res = await invoke<LinkCandidate[]>('search_note_links', { accountId, query: linkQuery });
      if (seq === linkSeq) {
        linkResults = res.filter((r) => r.uuid !== note?.uuid); // can't link to self
        linkIndex = 0;
      }
    } catch (e) {
      if (seq === linkSeq) linkResults = [];
    }
  }

  function pickLink(c: LinkCandidate) {
    if (!linkAnchor) return;
    const { node, start, end } = linkAnchor;
    const insert = `[[${c.slug}]]`;
    node.data = node.data.slice(0, start) + insert + node.data.slice(end);
    const sel = window.getSelection();
    if (sel) {
      const range = document.createRange();
      range.setStart(node, start + insert.length);
      range.collapse(true);
      sel.removeAllRanges();
      sel.addRange(range);
    }
    closeLinkPicker();
    editorEl?.focus();
    onInput(); // save + derive the edge
  }

  // Re-evaluate the [[ picker after caret-moving keys so it closes once the
  // caret leaves the [[query context — otherwise it stays open and swallows the
  // arrow keys (you couldn't move the cursor).
  function onEditorKeyup(e: KeyboardEvent) {
    if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown'].includes(e.key)) {
      detectLinkPicker();
    }
  }

  function onEditorKeydown(e: KeyboardEvent) {
    // Backspace immediately after a markdown trigger reverts it. We consume
    // the Backspace (preventDefault) so the user gets the trigger text back
    // instead of also deleting a character.
    if (lastTriggerUndo && e.key === 'Backspace' && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      const undo = lastTriggerUndo;
      lastTriggerUndo = null;
      undo();
      return;
    }
    // Any other keystroke invalidates the pending revert — the trigger has
    // been "accepted" by continued typing.
    if (lastTriggerUndo && e.key !== 'Shift' && e.key !== 'Meta' && e.key !== 'Control' && e.key !== 'Alt') {
      lastTriggerUndo = null;
    }

    // WebKit's native backspace-merge loses the current block's tag when the
    // previous line is empty (a heading merging backward into a blank line
    // comes out demoted to plain text) — see backspaceMergeEmpty.ts.
    if (e.key === 'Backspace' && !e.metaKey && !e.ctrlKey && !e.altKey && tryBackspaceMergeEmptyPrevious(editorEl)) {
      e.preventDefault();
      onInput();
      return;
    }

    // Keyboard shortcuts for first-class formatting. All listed in the ⓘ
    // cheatsheet so they are not undocumented behaviour. We use e.code (the
    // physical key, e.g. 'Digit8') rather than e.key (the produced character)
    // because Shift+8 produces '*' on US layouts, Alt+1 produces '¡' on macOS,
    // and AZERTY shifts the entire digit row — all of which would silently
    // break e.key-based matching. e.code is layout- and modifier-independent.
    const mod = e.metaKey || e.ctrlKey;
    if (mod && !e.altKey) {
      // Cmd/Ctrl+B/I/U: WebKit normally handles these via its built-in editing
      // key bindings, but in Tauri 2 on macOS Tahoe the Cocoa menu doesn't have
      // matching key equivalents (no Format menu), and the chord never reaches
      // WebKit's editing pipeline — Cmd+B silently no-ops. Intercepting in JS
      // and calling execCommand directly makes it work regardless of the menu.
      if (!e.shiftKey && e.code === 'KeyB') { e.preventDefault(); formatBold(); onInput(); return; }
      if (!e.shiftKey && e.code === 'KeyI') { e.preventDefault(); formatItalic(); onInput(); return; }
      if (!e.shiftKey && e.code === 'KeyU') { e.preventDefault(); formatUnderline(); onInput(); return; }
      if (e.shiftKey && e.code === 'KeyX') { e.preventDefault(); formatStrike(); onInput(); return; }
      if (!e.shiftKey && e.code === 'KeyK') { e.preventDefault(); formatLink(); return; }
      if (e.shiftKey && e.code === 'Digit7') { e.preventDefault(); formatOrderedList(); onInput(); return; }
      if (e.shiftKey && e.code === 'Digit8') { e.preventDefault(); formatList(); onInput(); return; }
      if (e.shiftKey && e.code === 'Digit9') { e.preventDefault(); formatTask(); return; }
      if (e.shiftKey && e.code === 'Period') { e.preventDefault(); formatBlockquote(); return; }
      if (!e.shiftKey && e.code === 'KeyE') { e.preventDefault(); formatInlineCode(); return; }
      if (e.shiftKey && e.code === 'KeyC') { e.preventDefault(); formatCodeBlock(); return; }
    }
    if (mod && e.altKey && !e.shiftKey) {
      if (e.code === 'Digit1') { e.preventDefault(); formatHeadingLevel(1); return; }
      if (e.code === 'Digit2') { e.preventDefault(); formatHeadingLevel(2); return; }
      if (e.code === 'Digit3') { e.preventDefault(); formatHeadingLevel(3); return; }
    }

    // Link picker takes the navigation keys first (Tab = accept, like Enter).
    if (linkPickerOpen && linkResults.length > 0) {
      if (e.key === 'ArrowDown') { e.preventDefault(); linkIndex = (linkIndex + 1) % linkResults.length; return; }
      if (e.key === 'ArrowUp') { e.preventDefault(); linkIndex = (linkIndex - 1 + linkResults.length) % linkResults.length; return; }
      if (e.key === 'Enter' || e.key === 'Tab') { e.preventDefault(); pickLink(linkResults[linkIndex]); return; }
      if (e.key === 'Escape') { e.preventDefault(); closeLinkPicker(); return; }
    }
    // Enter on a checklist line → continue the checklist with a new task (or
    // exit to a plain line when the task is empty), carrying the indent level.
    if (e.key === 'Enter' && !e.shiftKey) {
      const tb = taskBlock();
      if (tb) {
        e.preventDefault();
        const text = (tb.textContent || '').replace(/ /g, '').trim();
        if (text === '') {
          // Empty task → exit the checklist (plain empty line).
          const div = document.createElement('div');
          div.appendChild(document.createElement('br'));
          tb.replaceWith(div);
          caretToEnd(div);
        } else {
          const div = document.createElement('div');
          div.innerHTML = '<input type="checkbox" contenteditable="false">&nbsp;';
          if (tb.style.marginLeft) div.style.marginLeft = tb.style.marginLeft; // same level
          tb.after(div);
          caretToEnd(div);
        }
        onInput();
        return;
      }
      // Enter on an empty line inside a blockquote / heading → exit the block
      // to a plain paragraph below. Same UX pattern as Notion / Bear: one Enter
      // adds a new line within the special block; a second Enter (now on an
      // empty line) exits it.
      if (!e.metaKey && !e.ctrlKey && !e.altKey && tryExitSpecialBlock(editorEl)) {
        e.preventDefault();
        onInput();
        return;
      }
      // WebKit's native Enter-split misfires at the caret's absolute start
      // of the editor's very first block (content stays put instead of
      // moving into a new block below) — see splitFirstBlock.ts.
      if (!e.metaKey && !e.ctrlKey && !e.altKey && tryFixFirstBlockEnter(editorEl)) {
        e.preventDefault();
        onInput();
        return;
      }
    }
    // Tab / Shift-Tab → outline indent / outdent.
    if (e.key === 'Tab') {
      e.preventDefault();
      indentSelection(e.shiftKey);
    }
  }

  // The caret's block if it's a checklist row (a line whose first child is a
  // checkbox), else null.
  function taskBlock(): HTMLElement | null {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0 || !editorEl) return null;
    const node = sel.getRangeAt(0).startContainer;
    const el = (node.nodeType === 3 ? node.parentElement : node) as HTMLElement | null;
    const block = topBlock(el);
    return block && block.querySelector(':scope > input[type=checkbox]') ? block : null;
  }


  function setCheckboxState(cb: HTMLInputElement, val: boolean) {
    cb.checked = val;
    if (val) cb.setAttribute('checked', '');
    else cb.removeAttribute('checked');
  }

  // Nested-checklist propagation. Indent level (margin / 28) defines the tree:
  // a task's "subtree" is the consecutive deeper rows below it. Toggling a
  // parent sets its whole subtree; then, bottom-up, every parent becomes done
  // iff all of its subtree is done (so finishing the last subtask ticks the
  // parent, unticking one unticks it).
  function propagateChecklist(toggled: HTMLInputElement) {
    if (!editorEl) return;
    // Document order, any nesting depth; level from the task line's indent.
    const rows = (Array.from(editorEl.querySelectorAll('input[type=checkbox]')) as HTMLInputElement[]).map((cb) => {
      const line = cb.closest('div, li') as HTMLElement | null;
      const m = line ? parseInt(line.style.marginLeft || '0', 10) || 0 : 0;
      return { cb, level: Math.round(m / 28) };
    });
    const subtree = (i: number) => {
      const out: typeof rows = [];
      for (let j = i + 1; j < rows.length; j++) {
        if (rows[j].level > rows[i].level) out.push(rows[j]);
        else break;
      }
      return out;
    };
    const idx = rows.findIndex((r) => r.cb === toggled);
    if (idx >= 0) {
      const sub = subtree(idx);
      for (const r of sub) setCheckboxState(r.cb, toggled.checked); // parent → children
    }
    for (let i = rows.length - 1; i >= 0; i--) {
      const sub = subtree(i);
      if (sub.length) setCheckboxState(rows[i].cb, sub.every((r) => r.cb.checked)); // children → parent
    }
  }

  // The NEAREST block-level ancestor of `el` — the caret's line (a <div>,
  // checklist row, <li>, …). Nearest (not top-level) so a body wrapped in an
  // outer <div> indents the actual line, never the whole wrapper.
  function topBlock(el: HTMLElement | null): HTMLElement | null {
    let cur: HTMLElement | null = el;
    while (cur && cur !== editorEl) {
      if (['DIV', 'LI', 'P', 'H1', 'H2', 'H3', 'H4', 'BLOCKQUOTE'].includes(cur.tagName)) return cur;
      cur = cur.parentElement;
    }
    return null;
  }

  // Outline indent. List items nest via execCommand (→ nested <ul>/<ol>, which
  // round-trips to Apple). Other lines (plain paragraphs, checklists) indent via
  // margin-left in fixed steps.
  function indentSelection(outdent: boolean) {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0 || !editorEl) return;
    const node = sel.getRangeAt(0).startContainer;
    const el = (node.nodeType === 3 ? node.parentElement : node) as HTMLElement | null;
    const li = el?.closest('li');
    if (li && editorEl.contains(li)) {
      document.execCommand(outdent ? 'outdent' : 'indent');
    } else {
      const block = topBlock(el);
      // Only indent a SINGLE line. If the block contains other block-level
      // children it's a container holding many lines — indenting it would push
      // the whole note in (the bug just reported), so skip it.
      if (block && !block.querySelector('div, ul, ol, li, p, blockquote, table')) {
        const cur = parseInt(block.style.marginLeft || '0', 10) || 0;
        const next = Math.max(0, Math.min(168, cur + (outdent ? -28 : 28)));
        block.style.marginLeft = next === 0 ? '' : `${next}px`;
      } else {
        return; // nothing safe to indent → don't fire a save
      }
    }
    onInput();
  }

  // Note slug (mirror Rust db::note_slug) — shown on the editor + copyable so
  // the user knows how to link to THIS note ([[slug]]).
  function slugifyClient(s: string): string {
    let out = '';
    let pendingDash = false;
    for (const c of s.trim()) {
      if (/[\p{L}\p{N}_\p{M}]/u.test(c)) {
        if (pendingDash && out) out += '-';
        pendingDash = false;
        out += c.toLowerCase();
      } else {
        pendingDash = true;
      }
    }
    return out;
  }
  function noteSlugClient(t: string, uuid: string): string {
    const id = (uuid.match(/[0-9a-fA-F]/g) || []).join('').slice(0, 8).toLowerCase();
    const base = slugifyClient(t);
    return base ? `${base}-${id}` : id;
  }
  $: noteSlug =
    $selectedNote && !$selectedNote.uuid.startsWith('tmp:')
      ? noteSlugClient(title, $selectedNote.uuid)
      : '';
  let slugCopied = false;
  async function copySlug() {
    if (!noteSlug) return;
    try {
      await navigator.clipboard.writeText(`[[${noteSlug}]]`);
      slugCopied = true;
      setTimeout(() => (slugCopied = false), 1200);
    } catch (e) {}
  }

  // Local "connections graph" for the open note — its folder (child_of), tags
  // (tagged), outgoing + backlinks (mentions) laid out radially. Reads the data
  // the editor already has; the same relationships live in the fact-schema
  // `edges` table.
  let showGraph = false;
  function gtrunc(s: string): string {
    return s.length > 16 ? s.slice(0, 15) + '…' : s;
  }
  $: graphNodes = (() => {
    if (!$selectedNote) return [] as any[];
    const items: any[] = [];
    if ($selectedNote.label) items.push({ kind: 'folder', label: $selectedNote.label });
    for (const t of bodyTags) items.push({ kind: 'tag', label: '#' + t });
    for (const n of connections.outgoing) items.push({ kind: 'link', label: n.title || 'Untitled', note: n });
    for (const n of connections.backlinks) items.push({ kind: 'backlink', label: n.title || 'Untitled', note: n });
    const N = items.length;
    const cx = 300, cy = 200, r = N <= 6 ? 125 : 160;
    return items.map((it, i) => {
      const a = (i / Math.max(1, N)) * 2 * Math.PI - Math.PI / 2;
      return { ...it, x: cx + r * Math.cos(a), y: cy + r * Math.sin(a) };
    });
  })();
  function openGraphNode(gn: any) {
    if (gn.note) {
      showGraph = false;
      openConnection(gn.note);
    }
  }

  let editorEl: HTMLDivElement;
  let saveTimer: ReturnType<typeof setTimeout>;
  // True only after a REAL user edit (set in onInput, which fires on genuine
  // input events — NOT on programmatic innerHTML or the browser's re-serialize/
  // normalize of the loaded body). Gates all saves so merely viewing a note and
  // switching away can't spuriously insert-new + trash-old (which was sending
  // untouched notes' old revisions to Gmail Trash). Reset when a note loads.
  let userEdited = false;

  // uuid of the note currently rendered into editorEl. We repopulate the
  // contenteditable DOM when (a) the user switches to a different note, or
  // (b) the SAME note's body changed externally — e.g. polling picked up
  // Apple Notes' edit. Tracked separately from selectedNote.body_html so we
  // can tell "this is fresh server content" from "this is the body we wrote".
  let editorUuid: string | undefined;
  let lastRenderedBody: string | undefined;
  // Last (body, title) confirmed as pushed to Gmail. Distinct from
  // lastRenderedBody because lastRenderedBody is updated on every onInput
  // keystroke (for optimistic store sync), so it equals the live editor —
  // useless for "has anything actually changed vs server" comparisons.
  // These two ONLY move forward when a save succeeds (or on initial render
  // from cached/pulled content, which is the server's truth at that moment).
  let lastPushedBody: string | undefined;
  let lastPushedTitle: string | undefined;

  // The Note object the editor is currently bound to. Distinct from
  // $selectedNote because $selectedNote may have already changed to a NEW
  // note inside the reactive block — we need the OLD note's identity to
  // flush a pending save before the swap.
  let editorNote: Note | null = null;

  // Re-render decisions:
  //   - UUID changed → user selected a different note → always re-render,
  //     but flush any pending edit for the OLD note first (or it's lost forever)
  //   - Same UUID but body changed externally (poll picked up Apple Notes' edit)
  //     → re-render, BUT only when safe (not saving, not typing)
  //   - Same UUID, body matches what we last wrote out → no re-render
  $: if ($selectedNote) {
    const uuidChanged = $selectedNote.uuid !== editorUuid;
    const bodyChanged = $selectedNote.body_html !== lastRenderedBody;
    // Treat ANY element inside the editor as "user is interacting" — not
    // just the editor div itself. Clicking a checkbox shifts focus to the
    // <input>; the original check (activeElement === editorEl) missed
    // that and let polls re-render the body, wiping the user's toggle.
    const activeIsInEditor =
      typeof document !== 'undefined' &&
      !!editorEl &&
      document.activeElement instanceof Node &&
      (document.activeElement === editorEl ||
        editorEl.contains(document.activeElement));
    // Broader than activeIsInEditor: catches unsaved work even when DOM
    // focus is elsewhere in the note-editing surface (e.g. the title
    // <input>, a sibling of editorEl) or transiently lost (window
    // blur/refocus). Without this, editing the title right after editing
    // the body — focus now outside editorEl, autosave still debouncing —
    // let a background poll/settle/sweep overwrite editorEl.innerHTML with
    // the stale pre-edit body, silently discarding the edit and resetting
    // WKWebView's native undo stack.
    const hasPendingEdit =
      lastRenderedBody !== lastPushedBody ||
      (typeof title === 'string' && title !== lastPushedTitle);
    const shouldRender = shouldRenderExternalBody({
      uuidChanged,
      bodyChanged,
      isSaving: $isSaving,
      activeIsInEditor,
      hasPendingEdit,
    });

    if (shouldRender) {
      // FLUSH then clear — never just clear. If we're switching away from a
      // note that has a pending unsaved edit, the user's typing only exists
      // in DOM/title-variable; canceling the autosave timer without firing
      // a save would silently discard it. Capture the previous note's
      // identity + current editor contents and fire a save in the background.
      if (uuidChanged && editorNote && editorEl) {
        const prevBody = wrapBody(editorEl.innerHTML);
        flushPendingEdit(editorNote, title, prevBody);
        // If the previous note was an untouched tmp: blank (default title,
        // no real body content), drop it from the notes array. Otherwise
        // every `+` click without typing leaves a permanent ghost row in
        // the sidebar, which is how 8 real notes became 15 visible rows.
        if (
          editorNote.uuid?.startsWith('tmp:') &&
          title === 'New Note'
        ) {
          const inner = prevBody.match(/<body[^>]*>([\s\S]*)<\/body>/i)?.[1] ?? '';
          const stripped = inner.replace(/<[^>]*>/g, '').replace(/\s|&nbsp;/g, '');
          if (stripped === '') {
            const dropUuid = editorNote.uuid;
            notes.update((ns) => ns.filter((n) => n.uuid !== dropUuid));
          }
        }
      }
      clearTimeout(saveTimer);
      editorNote = $selectedNote;
      editorUuid = $selectedNote.uuid;
      title = typeof $selectedNote.title === 'string' ? $selectedNote.title : '';
      // Defer innerHTML until after Svelte's DOM update (bind:this timing).
      const targetUuid = $selectedNote.uuid;
      const targetBody = $selectedNote.body_html;
      const targetAccount = $selectedNote.account_id ?? $currentAccount;
      const targetId = $selectedNote.id;
      tick().then(() => {
        if (editorEl && editorUuid === targetUuid) {
          editorEl.innerHTML = extractBody(targetBody);
          lastRenderedBody = targetBody;
          // Initial render = the server's confirmed state for this note.
          // Anchor the "did anything change since push?" comparison here.
          lastPushedBody = targetBody;
          lastPushedTitle = title;
          // Fresh note loaded — no pending user edit yet. Critical: prevents a
          // view-then-switch from being treated as an edit (see flushPendingEdit).
          userEdited = false;
          refreshBodyTags(); // chips reflect the freshly-loaded note's #hashtags
          refreshConnections(); // backlinks / outgoing links for this note
          // Render inline images + run the stale-body safety net. Called
          // unconditionally (not gated on cid in body) so a stale body with no
          // refs but stored attachments is detected and healed.
          if (targetAccount) {
            hydrateAttachments(targetUuid, targetAccount, targetId);
          }
        }
      });
    }
  } else {
    // No note selected → flush any pending edit before clearing state
    if (editorNote && editorEl) {
      flushPendingEdit(editorNote, title, wrapBody(editorEl.innerHTML));
    }
    clearTimeout(saveTimer);
    editorNote = null;
    editorUuid = undefined;
    lastRenderedBody = undefined;
  }

  // Fire-and-forget save with an explicit note identity (not $selectedNote).
  // Called when the user switches notes before the debounced autosave fires —
  // the captured `note` parameter carries the previous note's id/uuid/label
  // so the save targets the right Gmail message, not whichever note is
  // currently selected after the swap.
  function flushPendingEdit(note: Note, noteTitle: string, bodyHtml: string) {
    // No genuine user edit on this note → never save. Without this, the
    // browser's innerHTML re-serialization (attribute order, whitespace,
    // entities) makes a merely-VIEWED note's body differ from lastPushedBody,
    // which the string compare below reads as a change → spurious insert-new +
    // trash-old → the untouched note's old revision lands in Gmail Trash.
    if (!userEdited) return;
    // Skip if nothing has changed since the last confirmed push (same logic
    // as autoSave). Compare against lastPushedBody/Title, NOT lastRenderedBody
    // — lastRenderedBody moves on every keystroke (see autoSave for the full
    // reasoning).
    if (bodyHtml === lastPushedBody && noteTitle === lastPushedTitle) return;
    // Skip if this is an untouched brand-new note (tmp: UUID, default title,
    // no real body content). The wrap-on-flush adds a style attribute that
    // makes the string compare above false-positive — without this guard,
    // clicking + and then clicking another note saves a blank "New Note"
    // to Gmail. We use the tmp: prefix as proof the note has never been
    // persisted; for real notes we always honor the user's edits (even
    // emptying them deliberately).
    if (note.uuid?.startsWith('tmp:') && noteTitle === 'New Note') {
      const inner = bodyHtml.match(/<body[^>]*>([\s\S]*)<\/body>/i)?.[1] ?? '';
      const stripped = inner.replace(/<[^>]*>/g, '').replace(/\s|&nbsp;/g, '');
      if (stripped === '') return;
    }
    // Don't block the swap. Save runs concurrently with the new note rendering.
    saveNote(note, noteTitle, bodyHtml).catch((e) => {
      error.set(`flush save failed: ${e}`);
    });
  }

  function extractBody(html: string): string {
    const match = html.match(/<body[^>]*>([\s\S]*)<\/body>/i);
    return match ? match[1] : html;
  }

  // Convert rendered inline images back to Apple's <object cid:…> form before
  // storing/saving, so the persisted body stays Apple-canonical and round-trips
  // (and matches lastPushedBody, avoiding spurious re-saves).
  function imgsToObjects(inner: string): string {
    return inner.replace(
      /<img[^>]*\bdata-jodd-cid="([^"]+)"[^>]*>/gi,
      (_m, cid) =>
        `<object type="application/x-apple-msg-attachment" data="cid:${cid}"></object>`
    );
  }

  function wrapBody(inner: string): string {
    return `<html><head></head><body style="overflow-wrap: break-word; -webkit-nbsp-mode: space; line-break: after-white-space;">${imgsToObjects(inner)}</body></html>`;
  }

  // Notes we've already tried to heal this session — prevents a refetch loop on
  // a note whose remote body is ALSO empty (already-damaged: edited+pushed while
  // its cache body was stale).
  const healedAttachmentNotes = new Set<string>();

  // After a note renders: render image attachments inline (swap <object cid:…>
  // for <img>), AND act as the data-loss safety net — if the note has stored
  // attachments but the body references NONE, the body is stale/over-stripped
  // (parsed before the title+image fix); editing it would push an
  // attachment-less message and lose them. Force one re-parse from Gmail to heal
  // before any edit. Guards against the selected note changing mid-fetch.
  async function hydrateAttachments(uuid: string, accountId: string, noteId: string) {
    type Att = { content_id: string; mime_type: string; data_uri: string };
    let atts: Att[] = [];
    try {
      atts = await invoke<Att[]>('get_note_attachments', { accountId, uuid });
    } catch (e) {
      return;
    }
    if (!editorEl || editorUuid !== uuid || !atts.length) return;

    const objects = editorEl.querySelectorAll('object[data^="cid:"]');
    if (objects.length === 0) {
      // Stale body — heal once via refetch (re-parses with the fixed strip).
      if (noteId && !healedAttachmentNotes.has(uuid)) {
        healedAttachmentNotes.add(uuid);
        try {
          const fresh = await invoke<Note>('refetch_note', { accountId, id: noteId });
          notes.update((ns) => {
            const i = ns.findIndex((n) => n.uuid === fresh.uuid);
            if (i >= 0) ns[i] = fresh;
            return ns;
          });
          // Re-renders + re-hydrates via the reactive load block (now with refs).
          if (get(selectedNote)?.uuid === fresh.uuid) selectedNote.set(fresh);
        } catch (e) {
          // Offline, or remote also empty (already-damaged) — leave as-is.
        }
      }
      return;
    }

    // Render images inline; non-image attachments keep their <object>.
    const byCid = new Map(atts.filter((a) => a.data_uri).map((a) => [a.content_id, a]));
    objects.forEach((obj) => {
      const cid = (obj.getAttribute('data') || '').slice(4); // strip "cid:"
      const a = byCid.get(cid);
      if (!a) return;
      const img = document.createElement('img');
      img.src = a.data_uri;
      img.setAttribute('data-jodd-cid', cid);
      img.style.maxWidth = '100%';
      img.style.height = 'auto';
      obj.replaceWith(img);
    });
  }

  function onInput(e?: Event) {
    userEdited = true;
    // Defer markdown-trigger detection by a frame: under accessibility-API
    // text input (e.g. computer-use 'type'), the input event fires BEFORE
    // WebKit finishes updating the DOM, so reading textContent/cursor offset
    // immediately would see stale state. requestAnimationFrame guarantees the
    // browser has painted before we read.
    const inputEvt = e as InputEvent | undefined;
    requestAnimationFrame(() => detectMarkdownTrigger(inputEvt));
    refreshBodyTags(); // keep chip row in sync as #hashtags are typed/edited
    detectLinkPicker(); // open the [[ note-link autocomplete when relevant
    clearTimeout(saveTimer);
    saveTimer = setTimeout(autoSave, 1500);

    // Optimistic store update: reflect the live edit in the note list immediately
    // (title chip, body preview) without waiting for the 1.5s autosave debounce.
    // The actual Gmail save still goes through autoSave; this just keeps the
    // user-visible store in sync with what they're typing.
    //
    // We update lastRenderedBody to the same value we're putting in the store,
    // so the editor's re-render reactive block doesn't see this as an "external
    // change" and try to re-render (which would destroy the cursor).
    if ($selectedNote && editorEl) {
      const liveBody = wrapBody(editorEl.innerHTML);
      const liveTitle = typeof title === 'string' ? title : '';
      // Skip if nothing actually changed since last render.
      if (liveBody === lastRenderedBody && liveTitle === $selectedNote.title) return;
      const updated: Note = {
        ...$selectedNote,
        title: liveTitle,
        body_html: liveBody,
      };
      lastRenderedBody = liveBody;
      notes.update((ns) => {
        const idx = ns.findIndex((n) => n.uuid === updated.uuid);
        if (idx >= 0) ns[idx] = updated;
        return ns;
      });
      selectedNote.set(updated);
    }
  }

  function onTitleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      editorEl?.focus();
    }
    // DO NOT call onInput() here. The title input already has `oninput={onInput}`
    // which fires only when the *value* actually changes. Calling onInput from
    // keydown would schedule autosave on every key — including navigation keys
    // (Tab, arrows, Esc) and modifier shortcuts (Cmd+C etc) that don't change
    // the value but would still trigger a save 1.5s later. That was the silent
    // path that allowed an empty-editor autosave to fire without any real edit.
  }

  async function autoSave() {
    if (!$selectedNote) return;
    const bodyHtml = wrapBody(editorEl?.innerHTML || '');

    // Skip if nothing has changed since the LAST CONFIRMED PUSH to Gmail.
    // The 1.5s debounce reschedules on every keystroke, so type+delete leaves
    // a save pending with byte-identical content; without this guard Jodd
    // would insert a new Gmail message + trash the prior one for zero net
    // change. Compare against lastPushedBody/lastPushedTitle (server truth),
    // NOT lastRenderedBody — that one tracks the live editor and would
    // always equal `bodyHtml`, falsely skipping legitimate edits.
    const titleStr = typeof title === 'string' ? title : '';
    if (bodyHtml === lastPushedBody && titleStr === lastPushedTitle) return;

    // ─── Data-loss guard ────────────────────────────────────────────────
    const stored = $selectedNote.body_html || '';
    const storedText = stored.replace(/<[^>]*>/g, ' ').replace(/&nbsp;/g, ' ').replace(/\s+/g, ' ').trim();
    const newText = (editorEl?.innerText || '').trim();
    if ($selectedNote.id && storedText.length >= 20) {
      const ratio = newText.length / storedText.length;
      if (ratio < 0.25) {
        error.set(
          `Autosave blocked: would shrink note from ${storedText.length} to ${newText.length} chars. ` +
            `If you intended to clear the note, delete it instead.`
        );
        return;
      }
    }
    // ────────────────────────────────────────────────────────────────────

    await saveNote($selectedNote, title, bodyHtml);
    userEdited = false;
    refreshConnections(); // [[links]] just saved → refresh the graph panel
  }

  // saveNote takes an explicit Note parameter rather than reading
  // $selectedNote inline. This matters for two paths:
  //   1) autoSave passes $selectedNote (current note being edited)
  //   2) flushPendingEdit passes the PREVIOUS note (already off-screen because
  //      the user just clicked a different note) — we must save against THAT
  //      identity, not the currently-selected one
  // Serialize all saves through a single Promise chain so multiple in-flight
  // autosaves can't race. Each save sees the previous one's resolved Gmail id
  // (via the updated store) before deciding what to trash — eliminating the
  // "every save tracks the same stale OLD id and orphans the previous NEW id"
  // failure mode that was causing dozens of duplicate messages to accumulate.
  let savePromiseChain: Promise<unknown> = Promise.resolve();

  function saveNote(note: Note, noteTitle: string, bodyHtml: string): Promise<void> {
    const next = savePromiseChain
      .catch(() => {}) // previous failure doesn't block this one
      .then(() => doSaveNote(note, noteTitle, bodyHtml));
    savePromiseChain = next.catch(() => {});
    return next;
  }

  async function doSaveNote(note: Note, noteTitle: string, bodyHtml: string) {
    // Defensive: if any of these are the wrong type, Tauri's serde rejects
    // the invoke with a cryptic "invalid type: map, expected a string" that
    // tells us nothing about which value was wrong. Catch it here with a
    // useful message so we can diagnose.
    if (typeof noteTitle !== 'string') {
      const sample = (() => { try { return JSON.stringify(noteTitle).slice(0, 200); } catch { return String(noteTitle); } })();
      error.set(`saveNote refused: noteTitle is ${typeof noteTitle}, expected string. Value: ${sample}`);
      console.error('saveNote: noteTitle bad type', noteTitle);
      return;
    }
    if (typeof bodyHtml !== 'string') {
      const sample = (() => { try { return JSON.stringify(bodyHtml).slice(0, 200); } catch { return String(bodyHtml); } })();
      error.set(`saveNote refused: bodyHtml is ${typeof bodyHtml}, expected string. Value: ${sample}`);
      console.error('saveNote: bodyHtml bad type', bodyHtml);
      return;
    }
    if (!note || typeof note !== 'object') {
      error.set(`saveNote refused: note is ${typeof note}, expected object`);
      console.error('saveNote: note bad type', note);
      return;
    }

    // Re-read the latest note from the store. If a previous queued save
    // updated this note's Gmail id, we want to use the freshest id — not
    // the one captured when this save was queued. Without this re-read,
    // the queue serialization helps but the second save still tracks the
    // stale id that the first save just replaced.
    const cur = get(selectedNote);
    if (cur && cur.uuid === note.uuid) {
      note = { ...note, id: cur.id, x_mail_created_date: cur.x_mail_created_date ?? note.x_mail_created_date };
    } else {
      const fromArr = get(notes).find((n) => n.uuid === note.uuid);
      if (fromArr) {
        note = { ...note, id: fromArr.id, x_mail_created_date: fromArr.x_mail_created_date ?? note.x_mail_created_date };
      }
    }

    isSaving.set(true);
    error.set(null);
    try {
      const existingGmailId = note.id || null;
      // A `tmp:` prefix marks a client-side temporary uuid for a brand-new
      // note that has never been saved. Strip it before sending — Rust
      // generates a real Apple-format UUID when existing_uuid is empty.
      const existingUuid = note.uuid && !note.uuid.startsWith('tmp:') ? note.uuid : null;
      const tmpUuidToReplace = note.uuid && note.uuid.startsWith('tmp:') ? note.uuid : null;
      const existingXMailCreatedDate = note.x_mail_created_date || null;
      const label = note.label || 'Notes';

      const noteAcct = note.account_id ?? null;
      const curAcct = get(currentAccount);
      const accountId = noteAcct || curAcct;
      if (typeof accountId !== 'string' || accountId.length === 0) {
        error.set(
          `No account context to save into (note.account_id=${JSON.stringify(noteAcct)}, currentAccount=${JSON.stringify(curAcct)}).`,
        );
        return;
      }

      const saved = await invoke<{ id: string; uuid: string }>('save_note', {
        accountId,
        title: noteTitle,
        bodyHtml,
        existingGmailId,
        existingUuid,
        existingXMailCreatedDate,
        label,
      });

      const updatedNote: Note = {
        id: saved.id,
        uuid: saved.uuid,
        title: noteTitle,
        body_html: bodyHtml,
        date: new Date().toISOString(),
        label,
        x_mail_created_date: existingXMailCreatedDate,
        account_id: accountId,
      };

      notes.update(ns => {
        // For a first-time save of a new note, the array contains a tmp:
        // entry that we need to replace with the resolved real UUID — find
        // by either real UUID (subsequent saves) OR tmp UUID (first save).
        const idx = ns.findIndex(
          n => n.uuid === saved.uuid || (tmpUuidToReplace !== null && n.uuid === tmpUuidToReplace),
        );
        if (idx >= 0) {
          ns[idx] = updatedNote;
        } else {
          ns = [updatedNote, ...ns];
        }
        return ns;
      });

      // Protect this note from being overwritten by a background poll that
      // fires before Gmail's index has propagated our insert. App.svelte's
      // loadNotes merges recently-saved notes back in if the fetch missed them.
      // Scoped per-account so Account A's save doesn't suppress Account B's
      // remote change for the same uuid (cross-mailbox notes can collide).
      markRecentlySaved(accountId, saved.uuid);

      // Keep the sidebar folder counts honest. The index is a sign-in
      // snapshot; without this patch a fresh note never bumps its folder.
      indexUpsertOnSave(accountId, existingGmailId, saved.id, label);

      // CRITICAL guard: only mutate selectedNote / lastRenderedBody if the
      // user is STILL viewing the note we just saved. Otherwise a flush-on-
      // switch would yank the user back to the previous note after they've
      // already navigated away — terrible UX.
      const cur = get(selectedNote);
      if (cur?.uuid === note.uuid) {
        // ALSO sync editorUuid/editorNote BEFORE selectedNote.set fires the
        // reactive block. Otherwise: when we save a brand-new note (whose
        // captured uuid was ''), the resulting selectedNote.set(updatedNote)
        // changes $selectedNote.uuid to a real value, the reactive block
        // sees uuidChanged=true and fires flushPendingEdit(blank_note, ...),
        // which invokes save_note AGAIN with empty existing_uuid → Rust
        // generates ANOTHER fresh UUID → a 2× duplicate of the new note.
        // Keeping editorUuid/editorNote in lockstep with selectedNote means
        // the reactive block sees "no change, no flush needed."
        lastRenderedBody = updatedNote.body_html;
        // Anchor the "did anything change since push?" check to what we just
        // confirmed on Gmail. Without this, the next autoSave would compare
        // against lastRenderedBody (which moves on every keystroke) and skip
        // a legitimate edit because "live editor == lastRenderedBody" by
        // construction.
        lastPushedBody = updatedNote.body_html;
        lastPushedTitle = updatedNote.title;
        editorUuid = updatedNote.uuid;
        editorNote = updatedNote;
        selectedNote.set(updatedNote);
      }
    } catch (e) {
      error.set(String(e));
    } finally {
      isSaving.set(false);
    }
  }

  async function deleteNote() {
    const target = $selectedNote;
    if (!target) return;
    // No native confirm() — unreliable in WKWebView. The trash icon click
    // is already an explicit action; deleted notes go to Gmail's Trash
    // folder (30-day recovery), not permanent delete.
    try {
      const accountId = target.account_id || $currentAccount;
      if (!accountId) {
        error.set('No account context — cannot delete.');
        return;
      }
      await invoke('delete_note', {
        accountId,
        id: target.id,
        uuid: target.uuid,
      });
      // Filter by uuid — id can be empty for local-only notes that have
      // never synced, in which case id-based filtering would drop nothing
      // (or every other unsynced note).
      notes.update(ns => ns.filter(n => n.uuid !== target.uuid));
      indexRemoveOnDelete(accountId, target.id);
      selectedNote.set(null);
    } catch (e) {
      error.set(String(e));
    }
  }

  // formatting commands
  function formatBold() { document.execCommand('bold'); }
  function formatItalic() { document.execCommand('italic'); }
  function formatUnderline() { document.execCommand('underline'); }
  function formatStrike() { document.execCommand('strikeThrough'); }
  function formatList() { document.execCommand('insertUnorderedList'); }
  function formatOrderedList() { document.execCommand('insertOrderedList'); }
  // Heading via formatBlock. <h2> round-trips as plain HTML Apple preserves in
  // the body; toggle off by reverting an existing h2 line back to a div.
  function formatHeading() {
    const block = (document.queryCommandValue('formatBlock') || '').toLowerCase();
    document.execCommand('formatBlock', false, block === 'h2' ? '<div>' : '<h2>');
  }

  // Insert a task line at the cursor: a single <div> with a checkbox + space.
  // We use a <div> wrapper rather than a bare <input> so pressing Enter after
  // the task creates a new line cleanly (contenteditable promotes the new line
  // to a sibling div). The trailing &nbsp; gives the cursor a place to land
  // when the user starts typing — without it, the caret falls behind the input.
  //
  // `contenteditable="false"` on the input itself is critical: inputs inside
  // a contenteditable region are otherwise treated as text-flow content and
  // some browsers swallow clicks for caret placement instead of toggling the
  // form control. Marking it non-editable restores normal click behavior.
  function formatTask() {
    document.execCommand(
      'insertHTML',
      false,
      '<div><input type="checkbox" contenteditable="false">&nbsp;</div>',
    );
    onInput();
  }

  // New first-class formatting: link, heading levels, blockquote, code.
  // Link prompt — Tauri 2's WKWebView silently suppresses window.prompt(), so
  // we render a small in-editor popover instead. The selection is captured
  // BEFORE the popover steals focus (moving focus to the popover's input
  // collapses the contenteditable selection), then restored on submit.
  let linkPromptOpen = false;
  let linkPromptUrl = '';
  let linkPromptText = '';
  let linkPromptHasSelection = false;
  let linkPromptSavedRange: Range | null = null;
  let linkPromptUrlEl: HTMLInputElement | null = null;
  function formatLink() {
    const sel = window.getSelection();
    if (sel && sel.rangeCount > 0) {
      linkPromptSavedRange = sel.getRangeAt(0).cloneRange();
      linkPromptHasSelection = sel.toString().length > 0;
      linkPromptText = linkPromptHasSelection ? sel.toString() : '';
    } else {
      linkPromptSavedRange = null;
      linkPromptHasSelection = false;
      linkPromptText = '';
    }
    linkPromptUrl = 'https://';
    linkPromptOpen = true;
    // setTimeout 0 lets Svelte mount the input before we focus it.
    setTimeout(() => {
      linkPromptUrlEl?.focus();
      linkPromptUrlEl?.select();
    }, 0);
  }
  function applyLinkPrompt() {
    const url = linkPromptUrl.trim();
    if (!url || url === 'https://') { closeLinkPrompt(); return; }
    // Restore the saved selection so createLink / insertHTML targets the
    // original caret position instead of the popover input.
    if (linkPromptSavedRange) {
      const sel = window.getSelection();
      if (sel) { sel.removeAllRanges(); sel.addRange(linkPromptSavedRange); }
    }
    if (linkPromptHasSelection) {
      document.execCommand('createLink', false, url);
    } else {
      const label = (linkPromptText || url).trim();
      document.execCommand(
        'insertHTML',
        false,
        `<a href="${escapeAttr(url)}">${escapeText(label)}</a>`,
      );
    }
    closeLinkPrompt();
    onInput();
  }
  function closeLinkPrompt() {
    linkPromptOpen = false;
    linkPromptSavedRange = null;
  }
  function onLinkPromptKey(e: KeyboardEvent) {
    if (e.key === 'Enter') { e.preventDefault(); applyLinkPrompt(); }
    else if (e.key === 'Escape') { e.preventDefault(); closeLinkPrompt(); }
  }
  function formatHeadingLevel(n: 1 | 2 | 3) {
    const cur = (document.queryCommandValue('formatBlock') || '').toLowerCase();
    const tag = `h${n}`;
    document.execCommand('formatBlock', false, cur === tag ? '<div>' : `<${tag}>`);
    onInput();
  }
  function formatBlockquote() {
    const cur = (document.queryCommandValue('formatBlock') || '').toLowerCase();
    document.execCommand('formatBlock', false, cur === 'blockquote' ? '<div>' : '<blockquote>');
    onInput();
  }
  function formatInlineCode() {
    const sel = window.getSelection();
    const text = sel?.toString() || '';
    // Multi-line selection: inline <code> would replace the line breaks
    // (execCommand insertHTML deletes the entire selected range as a unit),
    // collapsing N lines into 1. That's never what the user wants. Promote to
    // a block-level <pre><code>…</code></pre> which preserves the newlines.
    if (text.includes('\n')) {
      document.execCommand(
        'insertHTML',
        false,
        `<pre><code>${escapeText(text)}</code></pre>`,
      );
      onInput();
      return;
    }
    if (text) {
      document.execCommand('insertHTML', false, `<code>${escapeText(text)}</code>`);
    } else {
      // Zero-width space gives the caret a place to land inside the empty <code>.
      document.execCommand('insertHTML', false, '<code>​</code>');
    }
    onInput();
  }
  function formatCodeBlock() {
    const cur = (document.queryCommandValue('formatBlock') || '').toLowerCase();
    document.execCommand('formatBlock', false, cur === 'pre' ? '<div>' : '<pre>');
    onInput();
  }
  function escapeText(s: string): string {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }
  function escapeAttr(s: string): string {
    return escapeText(s).replace(/"/g, '&quot;');
  }

  // Caret-in-code: <pre> / <code> contexts where markdown triggers must NOT fire
  // (they would mangle the user's literal text) and where paste must always be
  // text/plain (so a pasted snippet doesn't get its angle-brackets interpreted).
  function caretInCodeContext(): boolean {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0) return false;
    const node = sel.getRangeAt(0).startContainer;
    const el = (node.nodeType === 3 ? node.parentElement : node) as HTMLElement | null;
    return !!el?.closest('pre, code');
  }

  // Markdown-style triggers. They are typing affordances, not a storage format:
  // the trigger string is deleted, the line is wrapped in real HTML, and only
  // HTML is ever persisted to body_html.
  //
  // DESIGN: triggers fire ONLY on a real space keystroke (inputType ===
  // 'insertText', data === ' '). They do NOT fire on paste — pasted text has
  // no "intent pause" so a `>` at the start of a quoted email line would be a
  // false positive (see chat discussion). Also suppressed inside <pre>/<code>.
  //
  // Each trigger stages a `lastTriggerUndo` revert closure. If Backspace is
  // the very next keystroke (onEditorKeydown), the revert fires and restores
  // the literal trigger text — this makes the feature feel forgiving.
  let lastTriggerUndo: (() => void) | null = null;
  // Swap a block's tag — used by heading + blockquote triggers instead of
  // document.execCommand('formatBlock', ...). On macOS Tahoe (WebKit 17.x in
  // Tauri 2), formatBlock silently no-ops when the target block is empty,
  // which is exactly the state after we deleteContents the trigger marker on
  // a fresh new-line block. Directly replacing the element bypasses that quirk.
  // execCommand insertUnorderedList / insertOrderedList wraps the current
  // block in <ul>/<ol> + <li>. In Tauri 2 WebKit the cursor lands in the
  // OLD block position (now empty), so typed text appears on a new line
  // outside the bullet. Re-anchor the cursor inside the new <li>.
  function applyListTrigger(b: Element, cmd: 'insertUnorderedList' | 'insertOrderedList') {
    document.execCommand(cmd);
    const root = (b.parentNode || editorEl) as Element | null;
    const li = root?.querySelector?.('li') as HTMLElement | null;
    if (li) {
      const r = document.createRange();
      r.selectNodeContents(li);
      r.collapse(true);
      const sel = window.getSelection();
      if (sel) { sel.removeAllRanges(); sel.addRange(r); }
    }
  }
  function swapBlockTag(b: Element, newTag: string): Element {
    const fresh = document.createElement(newTag);
    fresh.innerHTML = b.innerHTML || '<br>';
    b.parentNode!.replaceChild(fresh, b);
    const r = document.createRange();
    r.selectNodeContents(fresh);
    r.collapse(true);
    const sel = window.getSelection();
    if (sel) { sel.removeAllRanges(); sel.addRange(r); }
    return fresh;
  }
  type Trigger = { match: RegExp; apply: (block: Element) => void };
  const MARKDOWN_TRIGGERS: Trigger[] = [
    { match: /^#[  ]$/,    apply: (b) => { swapBlockTag(b, 'h1'); } },
    { match: /^##[  ]$/,   apply: (b) => { swapBlockTag(b, 'h2'); } },
    { match: /^###[  ]$/,  apply: (b) => { swapBlockTag(b, 'h3'); } },
    { match: /^>[  ]$/,    apply: (b) => { swapBlockTag(b, 'blockquote'); } },
    { match: /^[-*][  ]$/, apply: (b) => applyListTrigger(b, 'insertUnorderedList') },
    { match: /^1\.[  ]$/, apply: (b) => applyListTrigger(b, 'insertOrderedList') },
  ];
  function detectMarkdownTrigger(_e: InputEvent | undefined) {
    if (caretInCodeContext()) { return; }
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0) { return; }
    const range = sel.getRangeAt(0);
    if (!range.collapsed) { return; }
    // The caret's startContainer may be either a Text node (cursor amid text)
    // or an Element with startOffset pointing at the Nth child (cursor at a
    // boundary, e.g. just after a freshly-typed text node in an empty editor).
    // Resolve the element case to the immediately-preceding text node, which
    // is what holds the trigger text.
    let node: Node = range.startContainer;
    let offset = range.startOffset;
    if (node.nodeType !== 3) {
      const el = node as Element;
      const prev = offset > 0 ? el.childNodes[offset - 1] : null;
      if (!prev || prev.nodeType !== 3) { return; }
      node = prev;
      offset = (node as Text).length;
    }
    // Build the "current line" text by walking back from the cursor's text
    // node through inline siblings, stopping at a <br> or block boundary. We
    // can't just read block.textContent because Enter may put new lines into
    // the SAME block (separated by <br>), so the whole block's text is the
    // concatenation of every line — the regex would never match.
    const INLINE = new Set(['SPAN','B','STRONG','I','EM','U','S','A','CODE','FONT']);
    function lineBeforeCursor(t: Text, off: number): string {
      let acc = (t.textContent || '').slice(0, off);
      let cur: Node | null = t.previousSibling;
      while (cur) {
        if (cur.nodeType === 3) { acc = (cur.textContent || '') + acc; }
        else if (cur.nodeType === 1) {
          const tag = (cur as Element).tagName;
          if (tag === 'BR') break;
          if (!INLINE.has(tag)) break;
          acc = (cur.textContent || '') + acc;
        } else break;
        cur = cur.previousSibling;
      }
      return acc;
    }
    let block = topBlock(node.parentElement as HTMLElement | null);
    const lineText = lineBeforeCursor(node as Text, offset);
    const matchingTrig = MARKDOWN_TRIGGERS.find((t) => t.match.test(lineText));
    if (!matchingTrig) { return; }
    // Confirmed match. Now wrap top-level text in a div so formatBlock has a
    // block to act on.
    if (!block && editorEl && node.parentNode === editorEl) {
      const wrap = document.createElement('div');
      editorEl.insertBefore(wrap, node);
      wrap.appendChild(node);
      block = wrap;
    }
    if (!block) { return; }
    for (const trig of MARKDOWN_TRIGGERS) {
      if (trig !== matchingTrig) continue;
      const blockParent = block.parentNode;
      const blockBefore = block.previousSibling;
      const snapshotHTML = block.outerHTML;
      // Strip ONLY the current line's trigger characters (not the whole
      // block — if the block contains earlier lines separated by <br>, those
      // must survive). Find the line-start position by walking back the same
      // way lineBeforeCursor does, then delete from there to the cursor.
      let startNode: Node = node;
      let startOffset = 0;
      let walker: Node | null = node.previousSibling;
      while (walker) {
        if (walker.nodeType === 3) {
          startNode = walker; startOffset = 0;
        } else if (walker.nodeType === 1) {
          const tag = (walker as Element).tagName;
          if (tag === 'BR') {
            const parent = walker.parentNode!;
            const idx = Array.from(parent.childNodes).indexOf(walker as ChildNode);
            startNode = parent; startOffset = idx + 1;
            break;
          }
          if (!INLINE.has(tag)) {
            const parent = walker.parentNode!;
            const idx = Array.from(parent.childNodes).indexOf(walker as ChildNode);
            startNode = parent; startOffset = idx + 1;
            break;
          }
          startNode = walker; startOffset = 0;
        } else break;
        walker = walker.previousSibling;
      }
      const delRange = document.createRange();
      delRange.setStart(startNode, startOffset);
      delRange.setEnd(node, offset);
      delRange.deleteContents();
      // Place the cursor at the start of the block, not at the deleted text
      // node — deleteContents may have removed it entirely, and execCommand
      // formatBlock needs the selection to point at a live position INSIDE the
      // block we want to convert.
      const r = document.createRange();
      r.selectNodeContents(block);
      r.collapse(true);
      sel.removeAllRanges();
      sel.addRange(r);
      trig.apply(block);
      // Stage the revert for the next keystroke.
      lastTriggerUndo = () => {
        if (!blockParent) return;
        // After formatBlock / insertList, the original block is gone; find the
        // block currently at the caret and replace whatever wraps it back with
        // the pre-trigger snapshot. Using the saved previousSibling avoids
        // depending on whatever DOM the list/heading wrapper produced.
        const tmp = document.createElement('template');
        tmp.innerHTML = snapshotHTML;
        const restored = tmp.content.firstChild as HTMLElement | null;
        if (!restored) return;
        const newBlock = topBlock(
          (window.getSelection()?.focusNode as Node | null)?.parentElement as HTMLElement | null,
        );
        // Wrapper might be a UL/OL/BLOCKQUOTE that holds only the converted
        // line; remove the whole wrapper, not just the inner LI/DIV.
        const wrapper = newBlock?.closest('ul, ol, blockquote, h1, h2, h3') || newBlock;
        if (wrapper && wrapper.parentNode) {
          wrapper.parentNode.insertBefore(restored, wrapper);
          wrapper.parentNode.removeChild(wrapper);
          caretToEnd(restored);
          onInput();
        }
      };
      return;
    }
  }
  function firstTextNode(el: Node): Node | null {
    if (el.nodeType === 3) return el;
    for (const c of Array.from(el.childNodes)) {
      const r = firstTextNode(c);
      if (r) return r;
    }
    return null;
  }

  // Cheatsheet modal — surfaces every shortcut + the markdown triggers so they
  // are not hidden behaviour. See the toolbar ⓘ button.
  let showCheatsheet = false;
  // Diagnostic — visible next to the "Saved" indicator in the toolbar.
  // Remove once markdown triggers are confirmed working.

  // Sanitize pasted rich HTML so foreign font sizes / colors / fonts do not leak
  // into the note. Spotlight, Quick Look, and many web pages put inline
  // style="font-size:..." on the clipboard HTML, which the default contenteditable
  // paste would preserve — pasted text then appears much bigger than the
  // surrounding line. We keep structural tags (links, lists, headings, basic
  // emphasis) so pastes from articles still carry meaning; we drop presentation.
  function onEditorPaste(e: ClipboardEvent) {
    const cd = e.clipboardData;
    if (!cd) return;
    e.preventDefault();
    const html = cd.getData('text/html');
    const text = cd.getData('text/plain');
    // Inside <pre>/<code>, ALWAYS paste as plain text. Code blocks are for
    // literal source — sanitizing a syntax-highlighted clipboard would let
    // DOMParser interpret pasted <b>foo</b> as an actual bold tag.
    if (!html || caretInCodeContext()) {
      document.execCommand('insertText', false, text);
      onInput();
      return;
    }
    const cleaned = sanitizePastedHtml(html);
    document.execCommand('insertHTML', false, cleaned);
    onInput();
  }

  // Whitelist of structural tags worth preserving across a paste. Anything else
  // is unwrapped (children kept, tag dropped). <font> and <span> are dropped
  // because they almost always carry presentational styling we don't want.
  const PASTE_TAG_WHITELIST = new Set([
    'A', 'B', 'STRONG', 'I', 'EM', 'U', 'S', 'STRIKE', 'CODE', 'PRE',
    'BR', 'P', 'DIV', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6',
    'UL', 'OL', 'LI', 'BLOCKQUOTE',
  ]);

  function sanitizePastedHtml(html: string): string {
    const doc = new DOMParser().parseFromString(html, 'text/html');
    // Process body's children — never the body itself, or it would be unwrapped
    // out of the document and doc.body would be null on read-back.
    const walk = (node: Node) => {
      const kids = Array.from(node.childNodes);
      for (const child of kids) walk(child);
      if (node.nodeType !== Node.ELEMENT_NODE) return;
      const el = node as Element;
      if (el.tagName === 'SCRIPT' || el.tagName === 'STYLE' || el.tagName === 'META') {
        el.remove();
        return;
      }
      if (!PASTE_TAG_WHITELIST.has(el.tagName)) {
        const parent = el.parentNode;
        if (!parent) return;
        while (el.firstChild) parent.insertBefore(el.firstChild, el);
        parent.removeChild(el);
        return;
      }
      for (const attr of Array.from(el.attributes)) {
        if (el.tagName === 'A' && attr.name === 'href') continue;
        el.removeAttribute(attr.name);
      }
    };
    for (const child of Array.from(doc.body.childNodes)) walk(child);
    return doc.body.innerHTML;
  }

  // Click handler for checkboxes inside the editor body.
  //
  // Critical: DO NOT call e.preventDefault(). In WKWebView's contenteditable,
  // preventDefault on a checkbox click suppresses the native visual repaint —
  // the .checked property updates but the box stays painted in its previous
  // state. The earlier version did preventDefault to avoid double-toggling,
  // but the cost was invisible UI updates.
  //
  // Instead: let the native click toggle the property (visual repaints
  // immediately), then sync the `checked` attribute in a microtask so
  // innerHTML serialization captures the new state (innerHTML only serializes
  // the attribute, never the property).
  function onEditorClick(e: MouseEvent) {
    detectLinkPicker(); // clicking elsewhere closes a stale [[ picker
    const t = e.target as HTMLElement;
    if (t.tagName === 'INPUT' && (t as HTMLInputElement).type === 'checkbox') {
      // Defer to a microtask so the native click has already flipped
      // .checked. Reading the property at that point tells us the new state.
      queueMicrotask(() => {
        const cb = t as HTMLInputElement;
        setCheckboxState(cb, cb.checked);
        propagateChecklist(cb); // finishing all subtasks ticks the parent (& vice-versa)
        onInput();
      });
    }
  }

  // Updated-date display at top of note (mirrors Apple Notes).
  // Forces *local* timezone (getDate/getMonth/getHours etc are local-tz).
  // For Thailand timezone (+07:00, no DST) we emit the BE calendar form Apple
  // Notes uses: "4 June BE 2569, 02:25". Elsewhere we fall back to the
  // system locale's natural date+time.
  const EN_MONTHS = ['January','February','March','April','May','June',
                     'July','August','September','October','November','December'];

  function formatNoteDate(s: string): string {
    if (!s) return '';
    const d = new Date(s);
    if (isNaN(d.getTime())) return s;

    // getTimezoneOffset returns minutes WEST of UTC, so +07:00 = -420.
    const isThaiTZ = d.getTimezoneOffset() === -420;
    if (isThaiTZ) {
      const day = d.getDate();
      const month = EN_MONTHS[d.getMonth()];
      const beYear = d.getFullYear() + 543;
      const hh = String(d.getHours()).padStart(2, '0');
      const mm = String(d.getMinutes()).padStart(2, '0');
      return `${day} ${month} BE ${beYear}, ${hh}:${mm}`;
    }

    // Other regions: use system locale (24-hour clock).
    const date = d.toLocaleDateString(undefined, { year: 'numeric', month: 'long', day: 'numeric' });
    const time = d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', hour12: false });
    return `${date}, ${time}`;
  }

  // ─── Find / Replace ───────────────────────────────────────────────
  // Cmd+F toggles the find bar. Search/replace operate by walking text
  // nodes in the contenteditable — preserves HTML structure, no innerHTML
  // mass-replace that would kill the cursor.
  let showFind = false;
  let findQuery = '';
  let replaceQuery = '';
  let showReplace = false;
  let matchCount = 0;
  let currentMatchIdx = 0;
  let findInputEl: HTMLInputElement | undefined;

  function gatherTextNodes(root: Node): Text[] {
    const out: Text[] = [];
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node) {
      if ((node as Text).data.length > 0) out.push(node as Text);
      node = walker.nextNode();
    }
    return out;
  }

  // A match can live either in the title input or in a body text node.
  // The find/replace UI treats them as a single ordered sequence (title
  // matches first, then body matches in document order) so that nextMatch
  // can navigate between them naturally.
  //
  // Apple Notes folds title-and-body into one editable surface (their
  // first body line IS the title), so find/replace there is body-only.
  // Jodd splits them — extending find/replace to both keeps the user's
  // mental model intact.
  type Match =
    | { kind: 'title'; start: number; end: number }
    | { kind: 'body'; range: Range };

  function findAllMatches(query: string): Match[] {
    if (!query) return [];
    const q = query.toLowerCase();
    const out: Match[] = [];

    // Title matches come first in the ordered sequence.
    if (title) {
      const t = title.toLowerCase();
      let i = t.indexOf(q);
      while (i !== -1) {
        out.push({ kind: 'title', start: i, end: i + q.length });
        i = t.indexOf(q, i + q.length);
      }
    }

    // Then body matches in DOM order.
    if (editorEl) {
      for (const tn of gatherTextNodes(editorEl)) {
        const txt = tn.data.toLowerCase();
        let i = txt.indexOf(q);
        while (i !== -1) {
          const r = document.createRange();
          r.setStart(tn, i);
          r.setEnd(tn, i + q.length);
          out.push({ kind: 'body', range: r });
          i = txt.indexOf(q, i + q.length);
        }
      }
    }

    return out;
  }

  // `focusTarget=true` actually moves caret/focus to the match (used by
  // Enter / Next / Prev). When the user is still typing the query, we only
  // refresh the count so the find bar keeps focus and the editor caret
  // doesn't jump on every keystroke.
  function selectMatch(idx: number, focusTarget: boolean = true) {
    const matches = findAllMatches(findQuery);
    matchCount = matches.length;
    if (!matchCount) return;
    currentMatchIdx = ((idx % matchCount) + matchCount) % matchCount;
    if (!focusTarget) return;
    const m = matches[currentMatchIdx];
    if (m.kind === 'title') {
      // Title is a real <input> — use the input selection API.
      const input = document.querySelector('.title-input') as HTMLInputElement | null;
      if (input) {
        input.focus();
        input.setSelectionRange(m.start, m.end);
      }
    } else {
      const sel = window.getSelection();
      if (!sel) return;
      sel.removeAllRanges();
      sel.addRange(m.range);
      const rect = m.range.getBoundingClientRect();
      const editorRect = editorEl.getBoundingClientRect();
      if (rect.bottom > editorRect.bottom || rect.top < editorRect.top) {
        (m.range.startContainer.parentElement || editorEl).scrollIntoView({ block: 'center' });
      }
    }
  }

  // Debounced so typing a multi-char query doesn't jump the caret / steal
  // focus to the title input on every keystroke. The match COUNT updates
  // live (so "no match" / "N matches" reflects what's typed), but the
  // actual selection only moves once the user pauses.
  let findInputDebounce: ReturnType<typeof setTimeout> | undefined;
  function onFindInput() {
    currentMatchIdx = 0;
    // Live count refresh — no focus change.
    matchCount = findAllMatches(findQuery).length;
    clearTimeout(findInputDebounce);
    findInputDebounce = setTimeout(() => selectMatch(0), 350);
  }

  function nextMatch() { selectMatch(currentMatchIdx + 1); }
  function prevMatch() { selectMatch(currentMatchIdx - 1); }

  function replaceCurrent() {
    if (!findQuery) return;
    const matches = findAllMatches(findQuery);
    if (!matches.length) return;
    const idx = Math.min(currentMatchIdx, matches.length - 1);
    const m = matches[idx];
    if (m.kind === 'title') {
      title = title.slice(0, m.start) + replaceQuery + title.slice(m.end);
    } else {
      m.range.deleteContents();
      if (replaceQuery) m.range.insertNode(document.createTextNode(replaceQuery));
    }
    onInput();
    setTimeout(() => selectMatch(idx), 0);
  }

  function replaceAll() {
    if (!findQuery) return;
    const q = findQuery.toLowerCase();
    let count = 0;

    // Replace in title first.
    if (title) {
      const orig = title;
      const lower = orig.toLowerCase();
      if (lower.includes(q)) {
        let out = '';
        let i = 0;
        while (i < orig.length) {
          const j = lower.indexOf(q, i);
          if (j === -1) { out += orig.slice(i); break; }
          out += orig.slice(i, j) + replaceQuery;
          i = j + q.length;
          count++;
        }
        title = out;
      }
    }

    // Then walk the body text nodes.
    if (editorEl) {
      for (const tn of gatherTextNodes(editorEl)) {
        const orig = tn.data;
        const lower = orig.toLowerCase();
        if (!lower.includes(q)) continue;
        let out = '';
        let i = 0;
        while (i < orig.length) {
          const j = lower.indexOf(q, i);
          if (j === -1) { out += orig.slice(i); break; }
          out += orig.slice(i, j) + replaceQuery;
          i = j + q.length;
          count++;
        }
        tn.data = out;
      }
    }

    if (count > 0) onInput();
    matchCount = 0;
  }

  function openFind() {
    showFind = true;
    setTimeout(() => findInputEl?.focus(), 0);
  }
  function closeFind() {
    showFind = false;
    showReplace = false;
    findQuery = '';
    replaceQuery = '';
    matchCount = 0;
    clearTimeout(findInputDebounce);
    window.getSelection()?.removeAllRanges();
  }

  function onFindKey(e: KeyboardEvent) {
    if (e.key === 'Escape') { closeFind(); return; }
    if (e.key === 'Enter') {
      e.preventDefault();
      // User committed — cancel any pending debounced selectMatch(0) so it
      // doesn't snap them back to the first match after they pressed Enter.
      clearTimeout(findInputDebounce);
      if (e.shiftKey) prevMatch(); else nextMatch();
    }
  }

  // Cmd+F / Ctrl+F to open find bar — only when the editor is in scope.
  function onWindowKey(e: KeyboardEvent) {
    const cmd = e.metaKey || e.ctrlKey;
    if (cmd && e.key.toLowerCase() === 'f' && $selectedNote) {
      e.preventDefault();
      openFind();
    }
  }

  onMount(() => window.addEventListener('keydown', onWindowKey));
  onDestroy(() => window.removeEventListener('keydown', onWindowKey));
</script>

{#if $selectedNote}
  <div class="editor-pane">
    <div class="editor-toolbar">
      <div class="toolbar-left">
        {#if $isSaving}
          <span class="save-status saving">Saving...</span>
        {:else if $selectedNote?.uuid?.startsWith('tmp:')}
          <!-- tmp: UUID means the note exists only client-side; no save has
               reached Gmail yet. Distinguish from "Saved" so the user knows
               typing is required to persist. -->
          <span class="save-status draft">Draft — start typing to save</span>
        {:else}
          <span class="save-status saved">Saved</span>
        {/if}
      </div>
      <div class="toolbar-actions">
        <!-- onmousedown preventDefault keeps focus + selection IN the editor so a
             format button click applies to the selected text instead of blurring. -->
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatBold} title="Bold"><b>B</b></button>
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatItalic} title="Italic"><i>I</i></button>
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatUnderline} title="Underline"><u>U</u></button>
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatStrike} title="Strikethrough (Cmd/Ctrl+Shift+X)"><s>S</s></button>
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatLink} title="Link (Cmd/Ctrl+K)">🔗</button>
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatHeading} title="Heading 2 (Cmd/Ctrl+Alt+2)">H</button>
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatBlockquote} title="Blockquote (Cmd/Ctrl+Shift+.)">❝</button>
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatInlineCode} title="Inline code (Cmd/Ctrl+E)">&lt;/&gt;</button>
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatList} title="Bullet List (Cmd/Ctrl+Shift+8)">• —</button>
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatOrderedList} title="Numbered List (Cmd/Ctrl+Shift+7)">1.</button>
        <button class="fmt-btn" onmousedown={(e) => e.preventDefault()} onclick={formatTask} title="Task / Checklist (Cmd/Ctrl+Shift+9)">☐</button>
        <div class="divider"></div>
        <button class="fmt-btn" onclick={() => (showCheatsheet = true)} title="Editor shortcuts">ⓘ</button>
        <button class="fmt-btn" onclick={() => (showGraph = true)} title="Connections graph">🕸</button>
        <button class="delete-btn" onclick={deleteNote} title="Delete note">
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M2 3.5h10M5.5 3.5V2.5h3v1M3 3.5l.7 8h6.6l.7-8" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
        </button>
      </div>
    </div>

    {#if linkPromptOpen}
      <div class="link-prompt">
        {#if !linkPromptHasSelection}
          <input
            type="text"
            bind:value={linkPromptText}
            placeholder="Link text"
            class="link-prompt-input link-prompt-text"
            onkeydown={onLinkPromptKey}
          />
        {/if}
        <input
          type="url"
          bind:value={linkPromptUrl}
          bind:this={linkPromptUrlEl}
          placeholder="https://example.com"
          class="link-prompt-input link-prompt-url"
          onkeydown={onLinkPromptKey}
        />
        <button type="button" class="link-prompt-btn" onclick={applyLinkPrompt}>OK</button>
        <button type="button" class="link-prompt-btn link-prompt-cancel" onclick={closeLinkPrompt}>Cancel</button>
      </div>
    {/if}

    {#if showFind}
      <div class="find-bar">
        <div class="find-row">
          <svg width="12" height="12" viewBox="0 0 16 16" fill="none" style="color: #888">
            <circle cx="7" cy="7" r="5" stroke="currentColor" stroke-width="1.5"/>
            <path d="M11 11l3 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
          <input
            bind:this={findInputEl}
            bind:value={findQuery}
            oninput={onFindInput}
            onkeydown={onFindKey}
            placeholder="Find in note"
            class="find-input"
          />
          <span class="find-count">
            {#if findQuery && matchCount > 0}{currentMatchIdx + 1} / {matchCount}{:else if findQuery}no match{/if}
          </span>
          <button class="find-btn" onclick={prevMatch} disabled={!matchCount} title="Previous (Shift+Enter)">‹</button>
          <button class="find-btn" onclick={nextMatch} disabled={!matchCount} title="Next (Enter)">›</button>
          <label class="find-toggle" title="Show replace">
            <input type="checkbox" bind:checked={showReplace} /> Replace
          </label>
          <button class="find-close" onclick={closeFind} title="Close (Esc)">✕</button>
        </div>
        {#if showReplace}
          <div class="find-row">
            <span style="width: 12px"></span>
            <input
              bind:value={replaceQuery}
              placeholder="Replace with"
              onkeydown={onFindKey}
              class="find-input"
            />
            <button class="find-btn-text" onclick={replaceCurrent} disabled={!matchCount}>Replace</button>
            <button class="find-btn-text" onclick={replaceAll} disabled={!matchCount}>All</button>
          </div>
        {/if}
      </div>
    {/if}

    <div class="note-context">
      <span class="ctx-account" title="Account: {$selectedNote.account_id}">{accountDisplay($accounts.find((a) => a.id === $selectedNote.account_id), $selectedNote.account_id ?? '')}</span>
      <span class="ctx-sep">›</span>
      <button
        type="button"
        class="ctx-folder"
        title="Reveal folder in sidebar: {$selectedNote.label}"
        onclick={() => {
          const lbl = $selectedNote?.label || 'Notes';
          const acct = $selectedNote?.account_id;
          if (acct && acct !== $currentAccount) currentAccount.set(acct);
          if ($searchQuery) searchQuery.set('');
          if ($selectedFolder !== lbl) selectedFolder.set(lbl);
        }}
      >{$selectedNote.label || 'Notes'}</button>
      {#if noteSlug}
        <button
          class="ctx-slug"
          type="button"
          title={`Copy link  [[${noteSlug}]]`}
          onclick={copySlug}
        >🔗 {slugCopied ? 'copied!' : noteSlug}</button>
      {/if}
    </div>

    <div class="note-date" title="Last updated">{formatNoteDate($selectedNote.date)}</div>

    <input
      class="title-input"
      bind:value={title}
      onkeydown={onTitleKeydown}
      oninput={onInput}
      placeholder="Title"
    />

    {#if !$selectedNote.uuid?.startsWith('tmp:')}
      <div class="tag-row">
        {#each currentTags as tag (tag)}
          <span class="tag-chip">
            #{tag}
            <button
              type="button"
              class="tag-remove"
              title="Remove tag"
              aria-label="Remove tag {tag}"
              onclick={() => removeTag(tag)}
            >×</button>
          </span>
        {/each}
        <div class="tag-input-wrap">
          <input
            class="tag-input"
            bind:value={tagInput}
            onkeydown={onTagKeydown}
            onfocus={() => (tagInputFocused = true)}
            onblur={() => (tagInputFocused = false)}
            placeholder="add tag…"
            autocomplete="off"
          />
          {#if tagInputFocused && tagSuggestions.length > 0}
            <ul class="tag-suggest">
              {#each tagSuggestions as s, i (s)}
                <li
                  class="tag-suggest-item"
                  class:highlight={i === suggestIndex}
                  onmousedown={(e) => { e.preventDefault(); pickSuggestion(s); }}
                >#{s}</li>
              {/each}
            </ul>
          {/if}
        </div>
      </div>
    {/if}

    <div
      class="editor-body"
      contenteditable="true"
      bind:this={editorEl}
      oninput={onInput}
      onclick={onEditorClick}
      onkeydown={onEditorKeydown}
      onkeyup={onEditorKeyup}
      onpaste={onEditorPaste}
      data-placeholder="Start writing… (type # > - or 1. for formatting · click ⓘ for shortcuts)"
    ></div>
    {#if linkPickerOpen && linkResults.length}
      <div class="link-picker" style="left:{linkX}px; top:{linkY}px">
        {#each linkResults as c, i (c.uuid)}
          <button
            class="link-opt"
            class:active={i === linkIndex}
            type="button"
            onmousedown={(e) => { e.preventDefault(); pickLink(c); }}
          >
            <span class="link-opt-title">{c.title || 'Untitled'}</span>
            <span class="link-opt-label">{c.label}</span>
          </button>
        {/each}
      </div>
    {/if}

    {#if connections.outgoing.length || connections.backlinks.length}
      <div class="connections">
        {#if connections.outgoing.length}
          <div class="conn-group">
            <span class="conn-label">→ Links to</span>
            {#each connections.outgoing as n (n.uuid)}
              <button class="conn-link" type="button" onclick={() => openConnection(n)}
                >{n.title || 'Untitled'}</button>
            {/each}
          </div>
        {/if}
        {#if connections.backlinks.length}
          <div class="conn-group">
            <span class="conn-label">← Linked from</span>
            {#each connections.backlinks as n (n.uuid)}
              <button class="conn-link" type="button" onclick={() => openConnection(n)}
                >{n.title || 'Untitled'}</button>
            {/each}
          </div>
        {/if}
      </div>
    {/if}

    {#if $error}
      <div class="error-bar">{$error}</div>
    {/if}

    {#if showCheatsheet}
      <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
      <div class="graph-overlay" onclick={() => (showCheatsheet = false)}>
        <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
        <div class="graph-modal cheat-modal" onclick={(e) => e.stopPropagation()}>
          <div class="graph-head">
            <span>ⓘ Editor shortcuts</span>
            <button class="graph-close" onclick={() => (showCheatsheet = false)} aria-label="Close">✕</button>
          </div>
          <table class="cheat-table">
            <tbody>
              <tr><th colspan="2">Formatting</th></tr>
              <tr><td>Bold</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>B</kbd></td></tr>
              <tr><td>Italic</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>I</kbd></td></tr>
              <tr><td>Underline</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>U</kbd></td></tr>
              <tr><td>Strikethrough</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>X</kbd></td></tr>
              <tr><td>Link</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>K</kbd></td></tr>
              <tr><td>Heading 1 / 2 / 3</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>1</kbd>/<kbd>2</kbd>/<kbd>3</kbd></td></tr>
              <tr><td>Bullet list</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>8</kbd></td></tr>
              <tr><td>Numbered list</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>7</kbd></td></tr>
              <tr><td>Checklist</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>9</kbd></td></tr>
              <tr><td>Blockquote</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>.</kbd></td></tr>
              <tr><td>Inline code</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>E</kbd></td></tr>
              <tr><td>Code block</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd></td></tr>
              <tr><th colspan="2">Typing shortcuts (live keystrokes only)</th></tr>
              <tr><td>Heading 1 / 2 / 3</td><td><kbd>#</kbd> / <kbd>##</kbd> / <kbd>###</kbd> + space</td></tr>
              <tr><td>Blockquote</td><td><kbd>&gt;</kbd> + space</td></tr>
              <tr><td>Bullet list</td><td><kbd>-</kbd> or <kbd>*</kbd> + space</td></tr>
              <tr><td>Numbered list</td><td><kbd>1.</kbd> + space</td></tr>
              <tr><th colspan="2">Editor</th></tr>
              <tr><td>Indent / outdent line</td><td><kbd>Tab</kbd> / <kbd>Shift</kbd>+<kbd>Tab</kbd></td></tr>
              <tr><td>Link to another note</td><td>type <kbd>[[</kbd></td></tr>
              <tr><td>Find / replace in note</td><td><kbd>Cmd/Ctrl</kbd>+<kbd>F</kbd></td></tr>
              <tr><td>Continue / exit checklist</td><td><kbd>Enter</kbd> on a checklist line</td></tr>
            </tbody>
          </table>
          <p class="cheat-note">
            Typing shortcuts fire only as you type — never on paste — so quoted email lines
            (<kbd>&gt;</kbd>) and pasted code (<kbd>#</kbd>) stay literal.
            Press <kbd>Backspace</kbd> right after a trigger to revert it.
            Inside a code block, paste always preserves the literal text.
          </p>
        </div>
      </div>
    {/if}

    {#if showGraph && $selectedNote}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="graph-overlay" onclick={() => (showGraph = false)}>
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="graph-modal" onclick={(e) => e.stopPropagation()}>
          <div class="graph-head">
            <span>🕸 Connections — {$selectedNote.title || 'Untitled'}</span>
            <button class="graph-close" onclick={() => (showGraph = false)} aria-label="Close">✕</button>
          </div>
          {#if graphNodes.length === 0}
            <div class="graph-empty">โน้ตนี้ยังไม่มี connection (โฟลเดอร์ / แท็ก / ลิงก์)</div>
          {:else}
            <svg viewBox="0 0 600 400" class="graph-svg">
              {#each graphNodes as gn (gn.label)}
                <line x1="300" y1="200" x2={gn.x} y2={gn.y} class="g-edge g-{gn.kind}" />
              {/each}
              {#each graphNodes as gn (gn.label)}
                <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
                <g class="g-node g-{gn.kind}" class:clickable={!!gn.note} onclick={() => openGraphNode(gn)}>
                  <rect x={gn.x - 56} y={gn.y - 14} width="112" height="28" rx="9" />
                  <text x={gn.x} y={gn.y}>{gtrunc(gn.label)}</text>
                </g>
              {/each}
              <g class="g-node g-center">
                <rect x="238" y="184" width="124" height="32" rx="9" />
                <text x="300" y="200">{gtrunc($selectedNote.title || 'Untitled')}</text>
              </g>
            </svg>
            <div class="graph-legend">
              <span class="lg lg-folder">folder</span>
              <span class="lg lg-tag">tag</span>
              <span class="lg lg-link">→ links to</span>
              <span class="lg lg-backlink">← linked from</span>
            </div>
          {/if}
        </div>
      </div>
    {/if}
  </div>
{:else}
  <div class="empty-editor">
    <p>Select a note or create a new one</p>
  </div>
{/if}

<style>
  .editor-pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    background: #fffef9;
    height: 100vh;
    overflow: hidden;
  }

  .editor-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 20px;
    border-bottom: 1px solid #eee;
    background: #fffef9;
  }

  .save-status {
    font-size: 11px;
  }

  .save-status.saving { color: #aaa; }
  .save-status.saved { color: #bbb; }
  .save-status.draft { color: #c97c1f; font-style: italic; }

  .toolbar-actions {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .fmt-btn {
    padding: 4px 8px;
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: 13px;
    color: #666;
    transition: background 0.15s;
  }

  .fmt-btn:hover {
    background: rgba(0,0,0,0.07);
  }

  .divider {
    width: 1px;
    height: 16px;
    background: #ddd;
    margin: 0 4px;
  }

  .delete-btn {
    padding: 4px 6px;
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    color: #bbb;
    display: flex;
    align-items: center;
    transition: color 0.15s, background 0.15s;
  }

  .delete-btn:hover {
    color: #e55;
    background: rgba(220,50,50,0.07);
  }

  .find-bar {
    background: #fafafa;
    border-bottom: 1px solid #e8e2d4;
    padding: 6px 12px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .find-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .find-input {
    flex: 1;
    background: white;
    border: 1px solid #ddd;
    border-radius: 4px;
    padding: 4px 8px;
    font-size: 12px;
    font-family: inherit;
    outline: none;
  }
  .find-input:focus { border-color: #999; }
  .find-count {
    font-size: 11px;
    color: #888;
    min-width: 56px;
    text-align: right;
  }
  .find-btn {
    width: 22px;
    height: 22px;
    background: white;
    border: 1px solid #ddd;
    border-radius: 4px;
    color: #555;
    cursor: pointer;
    font-size: 13px;
    line-height: 1;
  }
  .find-btn:hover:not(:disabled) { background: #f0f0f0; }
  .find-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .find-btn-text {
    padding: 3px 10px;
    background: white;
    border: 1px solid #ddd;
    border-radius: 4px;
    font-size: 11px;
    color: #333;
    cursor: pointer;
    font-family: inherit;
  }
  .find-btn-text:hover:not(:disabled) { background: #f0f0f0; }
  .find-btn-text:disabled { opacity: 0.4; cursor: not-allowed; }
  .find-toggle {
    font-size: 11px;
    color: #555;
    display: flex;
    align-items: center;
    gap: 3px;
    cursor: pointer;
    user-select: none;
  }
  .find-close {
    background: none;
    border: none;
    cursor: pointer;
    color: #888;
    font-size: 12px;
    padding: 0 4px;
  }
  .find-close:hover { color: #333; }

  .note-context {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    padding: 10px 28px 0;
    font-size: 11px;
    color: #b0a99a;
  }
  .ctx-account {
    color: #8a8270;
    font-weight: 600;
    max-width: 240px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ctx-folder {
    color: #b08a4a;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 240px;
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    cursor: pointer;
  }
  .ctx-folder:hover {
    text-decoration: underline;
    color: #946d2a;
  }
  .ctx-sep {
    opacity: 0.6;
  }
  .ctx-slug {
    margin-left: auto;
    border: none;
    background: rgba(0, 0, 0, 0.05);
    color: #4a6b8a;
    font-size: 10px;
    font-family: inherit;
    padding: 1px 8px;
    border-radius: 9px;
    cursor: pointer;
    max-width: 260px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ctx-slug:hover {
    background: rgba(74, 107, 138, 0.14);
  }

  .note-date {
    font-size: 12px;
    color: #999;
    text-align: center;
    padding: 6px 28px 0;
    user-select: none;
  }

  .title-input {
    font-size: 22px;
    font-weight: 700;
    color: #1a1a1a;
    border: none;
    outline: none;
    padding: 8px 28px 8px;
    background: transparent;
    width: 100%;
    font-family: inherit;
  }

  .title-input::placeholder {
    color: #ccc;
  }

  .tag-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 0 28px 6px;
  }

  .tag-chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 12px;
    color: #6b6150;
    background: rgba(0, 0, 0, 0.06);
    padding: 2px 4px 2px 9px;
    border-radius: 11px;
  }

  .tag-remove {
    border: none;
    background: transparent;
    color: #a89e8a;
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
    padding: 0 2px;
    border-radius: 50%;
  }

  .tag-remove:hover {
    color: #c0392b;
  }

  .tag-input-wrap {
    position: relative;
    display: inline-block;
  }

  .tag-input {
    font-size: 12px;
    border: none;
    outline: none;
    background: transparent;
    padding: 2px 4px;
    min-width: 80px;
    font-family: inherit;
    color: #6b6150;
  }

  .tag-input::placeholder {
    color: #cfc8b8;
  }

  .tag-suggest {
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 20;
    margin: 2px 0 0;
    padding: 4px;
    list-style: none;
    min-width: 140px;
    max-height: 220px;
    overflow-y: auto;
    background: #fff;
    border: 1px solid #e0d9c8;
    border-radius: 8px;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.12);
  }

  .tag-suggest-item {
    padding: 4px 8px;
    font-size: 12px;
    color: #6b6150;
    border-radius: 5px;
    cursor: pointer;
  }

  .tag-suggest-item:hover,
  .tag-suggest-item.highlight {
    background: #f3e4c0;
    color: #3a2f12;
  }

  .editor-body {
    flex: 1;
    padding: 8px 28px 28px;
    font-size: 14px;
    line-height: 1.7;
    color: #333;
    outline: none;
    overflow-y: auto;
    font-family: inherit;
  }

  /* Blockquote: visually distinct from regular text — left bar + indent +
     muted color so a `> ` line reads as a quote, not a paragraph. */
  .editor-body :global(blockquote) {
    margin: 6px 0;
    padding: 2px 0 2px 14px;
    border-left: 3px solid #d8c79a;
    color: #5a5040;
    font-style: italic;
  }
  /* Outline lists: indent + a distinct bullet per nesting level so Tab-nested
     items read as a hierarchy. :global — list markup is injected via innerHTML. */
  .editor-body :global(ul),
  .editor-body :global(ol) {
    padding-left: 26px;
    margin: 0;
  }
  .editor-body :global(ul) { list-style: disc; }
  .editor-body :global(ul ul) { list-style: circle; }
  .editor-body :global(ul ul ul) { list-style: square; }

  .editor-body:empty::before {
    content: attr(data-placeholder);
    color: #ccc;
    pointer-events: none;
  }

  .empty-editor {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #ccc;
    font-size: 14px;
    background: #fffef9;
  }

  .link-picker {
    position: fixed;
    z-index: 1000;
    min-width: 220px;
    max-width: 340px;
    max-height: 240px;
    overflow-y: auto;
    background: #fff;
    border: 1px solid #e0d9c8;
    border-radius: 8px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.15);
    padding: 4px;
  }
  .link-opt {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    border: none;
    background: none;
    text-align: left;
    padding: 6px 9px;
    border-radius: 6px;
    cursor: pointer;
    font-family: inherit;
  }
  .link-opt:hover,
  .link-opt.active {
    background: #f3e4c0;
  }
  .link-opt-title {
    font-size: 13px;
    color: #333;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 1;
  }
  .link-opt-label {
    font-size: 10px;
    color: #b0a99a;
    white-space: nowrap;
  }

  .graph-overlay {
    position: fixed;
    inset: 0;
    z-index: 1100;
    background: rgba(20, 16, 8, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .graph-modal {
    background: #fffef9;
    border-radius: 12px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.3);
    width: min(680px, 92vw);
    max-height: 90vh;
    overflow: auto;
    padding: 14px 16px 16px;
  }
  .graph-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 14px;
    font-weight: 600;
    color: #3a3a3a;
    margin-bottom: 6px;
  }
  .graph-close {
    border: none;
    background: none;
    cursor: pointer;
    color: #999;
    font-size: 14px;
  }
  .graph-empty {
    color: #aaa;
    font-size: 13px;
    text-align: center;
    padding: 40px 0;
  }
  .link-prompt {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 8px 12px;
    background: #faf7ef;
    border-bottom: 1px solid #ece6d2;
  }
  .link-prompt-input {
    flex: 1;
    padding: 5px 8px;
    border: 1px solid #d8d2c4;
    border-radius: 4px;
    font-size: 13px;
    background: #fff;
    color: #3a3a3a;
    font-family: inherit;
  }
  .link-prompt-text {
    flex: 0 0 30%;
  }
  .link-prompt-input:focus {
    outline: none;
    border-color: #b39959;
    box-shadow: 0 0 0 2px rgba(179, 153, 89, 0.15);
  }
  .link-prompt-btn {
    padding: 5px 12px;
    border: 1px solid #d8d2c4;
    border-radius: 4px;
    background: #fff;
    color: #3a3a3a;
    font-size: 12px;
    cursor: pointer;
  }
  .link-prompt-btn:hover {
    background: #f0e9d4;
  }
  .link-prompt-cancel {
    color: #888;
  }
  .cheat-modal {
    width: min(560px, 92vw);
  }
  .cheat-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
    color: #3a3a3a;
  }
  .cheat-table th {
    text-align: left;
    padding: 12px 0 4px;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #888;
    border-bottom: 1px solid #eee;
  }
  .cheat-table td {
    padding: 4px 0;
    vertical-align: top;
  }
  .cheat-table td:last-child {
    text-align: right;
    white-space: nowrap;
    color: #555;
  }
  .cheat-table kbd {
    display: inline-block;
    padding: 1px 6px;
    border: 1px solid #d8d2c4;
    border-bottom-width: 2px;
    border-radius: 4px;
    background: #faf7ef;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 11px;
    color: #3a3a3a;
  }
  .cheat-note {
    margin: 14px 0 0;
    padding: 10px 12px;
    background: #faf7ef;
    border-left: 3px solid #d8c79a;
    border-radius: 4px;
    font-size: 12px;
    line-height: 1.5;
    color: #6a5f48;
  }
  .cheat-note kbd {
    padding: 0 4px;
    border: 1px solid #d8d2c4;
    border-radius: 3px;
    background: #fff;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 11px;
  }
  .graph-svg {
    width: 100%;
    height: auto;
    display: block;
  }
  .g-edge {
    stroke-width: 1.5;
  }
  .g-edge.g-folder { stroke: #d8c79a; }
  .g-edge.g-tag { stroke: #e6d3a0; }
  .g-edge.g-link { stroke: #9fc0dd; }
  .g-edge.g-backlink { stroke: #a8d0b0; }
  .g-node rect { stroke-width: 1.2; }
  .g-node text {
    font-size: 11px;
    text-anchor: middle;
    dominant-baseline: middle;
    fill: #333;
  }
  .g-node.clickable { cursor: pointer; }
  .g-node.clickable:hover rect { filter: brightness(0.95); }
  .g-center rect { fill: #3a2f12; stroke: #3a2f12; }
  .g-center text { fill: #fff; font-weight: 600; }
  .g-folder rect { fill: #f5ecd6; stroke: #d8c79a; }
  .g-tag rect { fill: #f9f0d6; stroke: #e6d3a0; }
  .g-link rect { fill: #eaf2f9; stroke: #9fc0dd; }
  .g-backlink rect { fill: #eaf5ed; stroke: #a8d0b0; }
  .graph-legend {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
    margin-top: 8px;
    font-size: 11px;
    color: #888;
  }
  .lg::before { content: '●'; margin-right: 4px; }
  .lg-folder::before { color: #d8c79a; }
  .lg-tag::before { color: #e6d3a0; }
  .lg-link::before { color: #9fc0dd; }
  .lg-backlink::before { color: #a8d0b0; }

  .connections {
    border-top: 1px solid #efe9dd;
    background: #faf8f3;
    padding: 8px 28px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex-shrink: 0;
  }
  .conn-group {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .conn-label {
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: #b0a99a;
  }
  .conn-link {
    border: none;
    background: rgba(0, 0, 0, 0.05);
    color: #4a6b8a;
    font-size: 12px;
    font-family: inherit;
    padding: 2px 9px;
    border-radius: 11px;
    cursor: pointer;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .conn-link:hover {
    background: rgba(74, 107, 138, 0.16);
    color: #2f5066;
  }

  .error-bar {
    background: #fee;
    color: #c33;
    padding: 8px 20px;
    font-size: 12px;
    border-top: 1px solid #fcc;
  }
</style>
